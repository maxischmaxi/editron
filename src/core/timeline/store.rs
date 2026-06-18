//! impl-TimelineStore-Methoden (aus timeline.rs zerlegt).
use super::*;

impl TimelineStore {
    pub(crate) fn push_history(&mut self) {
        self.past.push(Snapshot {
            tracks: self.tracks.clone(),
            clips: self.clips.clone(),
            transitions: self.transitions.clone(),
            markers: self.markers.clone(),
            master_gain_db: self.master_gain_db,
            seq: crate::core::next_op_seq(),
        });
        if self.past.len() > HISTORY_LIMIT {
            self.past.remove(0);
        }
        self.future.clear();
        self.revision += 1;
    }

    /// Sequenz aus einer Projektdatei übernehmen: ersetzt Inhalt und
    /// verwirft History/Auswahl/Zwischenablage.
    #[allow(clippy::too_many_arguments)]
    pub fn load_document(
        &mut self,
        settings: Option<SequenceSettings>,
        tracks: Vec<TimelineTrack>,
        clips: Vec<TimelineClip>,
        transitions: Vec<Transition>,
        markers: Vec<Marker>,
        playhead_sec: f64,
        in_point: Option<f64>,
        out_point: Option<f64>,
        zoom_px_per_sec: f64,
        snapping: bool,
        selected_clip_ids: Vec<String>,
        master_gain_db: f64,
        active_subtitle_track_id: Option<String>,
    ) {
        let defaults = TimelineStore::default();
        // Altprojekte ohne Sequenz-Einstellungen laden mit 25 fps weiter;
        // der Aufrufer (project::apply) rät die Auflösung aus den Medien.
        self.settings = settings.map(SequenceSettings::sanitized).unwrap_or_default();
        self.pending_media_match = None;
        self.tracks = if tracks.is_empty() { defaults.tracks } else { tracks };
        self.clips = clips;
        let track_ids: std::collections::HashSet<&str> =
            self.tracks.iter().map(|t| t.id.as_str()).collect();
        // Defensive Validierung: Clips ohne gültige Spur oder mit kaputten
        // Zeiten fliegen raus, statt später Layout/Player zu zerlegen.
        self.clips.retain(|c| {
            track_ids.contains(c.track_id.as_str())
                && c.start.is_finite()
                && c.duration.is_finite()
                && c.src_in.is_finite()
                && c.duration >= MIN_CLIP_DURATION - EPS
        });
        // Effekt-Parameterlisten an die aktuelle Spec angleichen (ältere/
        // neuere Projektdateien).
        for c in &mut self.clips {
            for e in &mut c.effects {
                e.normalize();
            }
            for m in &mut c.markers {
                m.sanitize();
            }
            sort_markers(&mut c.markers);
            // Negative Zeiten aus fremden/korrupten Dateien klemmen (is_finite
            // allein lässt z. B. start=-5 oder src_in=-1 durch).
            c.start = c.start.max(0.0);
            c.src_in = c.src_in.max(0.0);
            // Geschwindigkeit defensiv klemmen (fremde/kaputte Dateien) — Basis
            // wie auch jeder Keyframe der Time-Remap-Kurve.
            c.speed.value = crate::core::timeline::clamp_speed(c.speed.value);
            for k in &mut c.speed.keyframes {
                k.value = crate::core::timeline::clamp_speed(k.value);
            }
            if c.freeze {
                c.reverse = false;
            }
        }
        // Übergänge defensiv validieren (verwaiste/kaputte fliegen raus).
        self.transitions = transitions
            .into_iter()
            .filter(|t| t.duration.is_finite() && t.duration > 0.0)
            .collect();
        self.selected_transition_ids.clear();
        self.reconcile_transitions();
        // Sequenz-Marker defensiv bereinigen + sortieren.
        self.markers = markers;
        for m in &mut self.markers {
            m.sanitize();
        }
        sort_markers(&mut self.markers);
        self.clipboard.clear();
        self.clipboard_transitions.clear();
        self.selected_clip_ids = selected_clip_ids;
        self.prune_selection();
        self.playhead_sec = if playhead_sec.is_finite() { playhead_sec.max(0.0) } else { 0.0 };
        self.in_point = in_point.filter(|v| v.is_finite());
        self.out_point = out_point.filter(|v| v.is_finite());
        self.zoom_px_per_sec = clamp_zoom(zoom_px_per_sec);
        self.snapping = snapping;
        self.master_gain_db = if master_gain_db.is_finite() {
            master_gain_db.clamp(-60.0, 6.0)
        } else {
            0.0
        };
        self.active_subtitle_track_id = active_subtitle_track_id.filter(|id| {
            self.tracks
                .iter()
                .any(|t| t.id == *id && t.kind == TrackKind::Subtitle)
        });
        self.past.clear();
        self.future.clear();
        self.revision += 1;
    }

    pub(crate) fn prune_selection(&mut self) {
        let existing: std::collections::HashSet<&str> =
            self.clips.iter().map(|c| c.id.as_str()).collect();
        self.selected_clip_ids.retain(|id| existing.contains(id.as_str()));
    }

    /// Inhaltskopie für „Sequenz duplizieren": gleiche Spuren/Clips/Übergänge/
    /// Marker/Einstellungen, aber frische (leere) Undo-History und ohne
    /// Auswahl/Zwischenablage. Clip-/Spur-IDs bleiben (nur innerhalb der
    /// eigenen Timeline referenziert).
    pub fn duplicate_content(&self) -> TimelineStore {
        let mut t = TimelineStore::default();
        t.settings = self.settings;
        t.tracks = self.tracks.clone();
        t.clips = self.clips.clone();
        t.transitions = self.transitions.clone();
        t.markers = self.markers.clone();
        t.master_gain_db = self.master_gain_db;
        t.active_subtitle_track_id = self.active_subtitle_track_id.clone();
        t.playhead_sec = self.playhead_sec;
        t.in_point = self.in_point;
        t.out_point = self.out_point;
        t.zoom_px_per_sec = self.zoom_px_per_sec;
        t.snapping = self.snapping;
        t.multicam = self.multicam.clone();
        t.revision = 1;
        t
    }

    /// Alle Nest-Clips entfernen, die eine bestimmte (gelöschte) Sequenz
    /// referenzieren — samt anhängender Übergänge. Bumpt `revision`, falls
    /// etwas entfernt wurde.
    pub fn remove_nest_clips_of(&mut self, nested_seq_id: &str) {
        let gone: std::collections::HashSet<String> = self
            .clips
            .iter()
            .filter(|c| c.nest_seq.as_deref() == Some(nested_seq_id))
            .map(|c| c.id.clone())
            .collect();
        if gone.is_empty() {
            return;
        }
        self.clips.retain(|c| !gone.contains(&c.id));
        self.transitions.retain(|t| {
            let hit = |id: &Option<String>| id.as_ref().is_some_and(|id| gone.contains(id));
            !hit(&t.from_clip_id) && !hit(&t.to_clip_id)
        });
        self.selected_clip_ids.retain(|id| !gone.contains(id));
        self.revision += 1;
    }

    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    // ------------------------------------------------------------- Ansicht

    pub fn zoom_in(&mut self) {
        self.zoom_px_per_sec = clamp_zoom(self.zoom_px_per_sec * ZOOM_FACTOR);
    }

    pub fn zoom_out(&mut self) {
        self.zoom_px_per_sec = clamp_zoom(self.zoom_px_per_sec / ZOOM_FACTOR);
    }

    pub fn set_zoom(&mut self, v: f64) {
        self.zoom_px_per_sec = clamp_zoom(v);
    }

    pub fn zoom_to_fit(&mut self) {
        let end = sequence_end(&self.clips);
        if end <= 0.0 || self.viewport_w <= 0.0 {
            return;
        }
        self.zoom_px_per_sec = clamp_zoom(self.viewport_w * 0.97 / end);
    }

    pub fn toggle_snapping(&mut self) {
        self.snapping = !self.snapping;
    }

    // ------------------------------------------------------------ Playhead

    pub fn set_playhead(&mut self, t: f64) {
        self.playhead_sec = t.max(0.0);
    }

    /// Frame-genaues Stepping gegen die Sequenzrate: rastet den Playhead
    /// auf das Frame-Raster ein (rationale Arithmetik, NTSC-driftfrei).
    pub fn step_playhead_frames(&mut self, frames: f64) {
        let rate = self.settings.rate;
        let current = rate.frame_round(self.playhead_sec) as f64;
        self.playhead_sec = rate.time_of_frame((current + frames).max(0.0));
    }

    // ------------------------------------------------ Sequenz-Einstellungen

    /// Sequenz-Einstellungen übernehmen (Dialog/Media-Match). Kein Undo-
    /// Snapshot (Premiere-Konvention), aber Dirty-Tracking über `revision`.
    pub fn set_sequence_settings(&mut self, settings: SequenceSettings) {
        let settings = settings.sanitized();
        if settings == self.settings {
            return;
        }
        self.settings = settings;
        self.revision += 1;
    }

    pub fn go_to_start(&mut self) {
        self.playhead_sec = 0.0;
    }

    pub fn go_to_end(&mut self) {
        self.playhead_sec = sequence_end(&self.clips);
    }

    pub fn go_to_prev_edit(&mut self) {
        let edges = edit_points(&self.clips);
        let prev = edges.iter().rev().find(|e| **e < self.playhead_sec - EPS);
        self.playhead_sec = prev.copied().unwrap_or(0.0);
    }

    pub fn go_to_next_edit(&mut self) {
        let edges = edit_points(&self.clips);
        if let Some(next) = edges.iter().find(|e| **e > self.playhead_sec + EPS) {
            self.playhead_sec = *next;
        }
    }

    // ------------------------------------------------------------ Marker
    // Sequenz-Marker liegen in Sequenz-Sekunden und sind Teil der
    // Undo-History (über push_history). Sie werden stets frame-genau gegen
    // die Sequenzrate gerastert und nach Zeit sortiert gehalten.

    /// Rastert eine Sequenzzeit frame-genau (rationale NTSC-Arithmetik).
    pub fn snap_to_frame(&self, t: f64) -> f64 {
        let rate = self.settings.rate;
        if rate.num == 0 || rate.den == 0 || !t.is_finite() {
            return t.max(0.0);
        }
        rate.time_of_frame(rate.frame_round(t.max(0.0)) as f64).max(0.0)
    }

    /// Sequenz-Marker exakt (innerhalb eines halben Frames) bei `t`.
    fn marker_index_at(&self, t: f64) -> Option<usize> {
        let tol = (0.5 / self.settings.rate.fps()).max(EPS);
        self.markers.iter().position(|m| (m.time - t).abs() <= tol)
    }

    /// Sequenz-Marker am Playhead setzen (M) — latenzfrei, idempotent:
    /// existiert am selben Frame schon einer, wird dessen ID zurückgegeben.
    /// Liefert die ID des (neuen oder bestehenden) Markers.
    pub fn add_marker_at(&mut self, t: f64) -> String {
        let t = self.snap_to_frame(t);
        if let Some(idx) = self.marker_index_at(t) {
            return self.markers[idx].id.clone();
        }
        self.push_history();
        let marker = Marker::new(t);
        let id = marker.id.clone();
        self.markers.push(marker);
        sort_markers(&mut self.markers);
        id
    }

    /// Beginn einer Marker-Geste (Drag/Dialog) — ein Snapshot.
    pub fn begin_marker_edit(&mut self) {
        self.push_history();
    }

    /// Sequenz-Marker ändern OHNE neuen Snapshot (laufende Geste nach
    /// `begin_marker_edit`). Hält die Sortierung aufrecht.
    pub fn marker_update_live(&mut self, id: &str, f: impl FnOnce(&mut Marker)) {
        if let Some(m) = self.markers.iter_mut().find(|m| m.id == id) {
            f(m);
            m.sanitize();
        }
        sort_markers(&mut self.markers);
        self.revision += 1;
    }

    /// Sequenz-Marker ändern (mit Undo-Snapshot) — für Einzelaktionen.
    pub fn marker_update(&mut self, id: &str, f: impl FnOnce(&mut Marker)) {
        if !self.markers.iter().any(|m| m.id == id) {
            return;
        }
        self.push_history();
        self.marker_update_live(id, f);
    }

    /// Einen Sequenz-Marker entfernen.
    pub fn remove_marker(&mut self, id: &str) {
        if !self.markers.iter().any(|m| m.id == id) {
            return;
        }
        self.push_history();
        self.markers.retain(|m| m.id != id);
    }

    /// Alle Sequenz-Marker entfernen.
    pub fn clear_markers(&mut self) {
        if self.markers.is_empty() {
            return;
        }
        self.push_history();
        self.markers.clear();
    }

    /// Sequenz-Marker, der den Playhead überdeckt (Punkt: exakt am Frame;
    /// Bereich: Playhead in [time, end]) — für „Marker löschen" / Dialog.
    pub fn marker_at_playhead(&self) -> Option<&Marker> {
        let t = self.snap_to_frame(self.playhead_sec);
        let tol = (0.5 / self.settings.rate.fps()).max(EPS);
        // Exakter Punkttreffer hat Vorrang vor Bereichsüberdeckung.
        self.markers
            .iter()
            .find(|m| (m.time - t).abs() <= tol)
            .or_else(|| {
                self.markers
                    .iter()
                    .find(|m| m.duration > 0.0 && t >= m.time - tol && t <= m.end() + tol)
            })
    }

    /// Den nächstgelegenen Marker zum Playhead löschen (Premiere: „Marker
    /// löschen" wirkt am aktuellen/überdeckten Marker).
    pub fn remove_marker_at_playhead(&mut self) -> bool {
        let Some(id) = self.marker_at_playhead().map(|m| m.id.clone()) else {
            return false;
        };
        self.remove_marker(&id);
        true
    }

    /// Playhead auf den nächsten Sequenz-Marker (echt rechts) setzen.
    pub fn go_to_next_marker(&mut self) -> bool {
        let t = self.playhead_sec;
        if let Some(next) = self
            .markers
            .iter()
            .map(|m| m.time)
            .filter(|mt| *mt > t + EPS)
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        {
            self.playhead_sec = next;
            true
        } else {
            false
        }
    }

    /// Playhead auf den vorherigen Sequenz-Marker (echt links) setzen.
    pub fn go_to_prev_marker(&mut self) -> bool {
        let t = self.playhead_sec;
        if let Some(prev) = self
            .markers
            .iter()
            .map(|m| m.time)
            .filter(|mt| *mt < t - EPS)
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        {
            self.playhead_sec = prev;
            true
        } else {
            false
        }
    }

    // -------- Clip-Marker (Medienzeit; wandern mit dem Material) ----------

    /// Clip-Marker an der zur Sequenzzeit `t_seq` gehörenden Medienzeit
    /// setzen (z. B. Playhead über dem Clip). Idempotent pro Quell-Frame.
    /// Liefert die Marker-ID, falls der Clip existiert und `t_seq` ihn trifft.
    pub fn add_clip_marker_at_seq(&mut self, clip_id: &str, t_seq: f64) -> Option<String> {
        let Some(clip) = self.clips.iter().find(|c| c.id == clip_id) else {
            return None;
        };
        if t_seq < clip.start - EPS || t_seq > clip.end() + EPS {
            return None;
        }
        let media_t = clip.media_time_at(t_seq).max(0.0);
        self.add_clip_marker(clip_id, media_t)
    }

    /// Clip-Marker an einer absoluten Medienzeit setzen (idempotent pro
    /// Quell-Frame). Liefert die ID des (neuen oder bestehenden) Markers.
    pub fn add_clip_marker(&mut self, clip_id: &str, media_t: f64) -> Option<String> {
        let tol = (0.5 / self.settings.rate.fps()).max(EPS);
        let Some(clip) = self.clips.iter().find(|c| c.id == clip_id) else {
            return None;
        };
        if let Some(existing) = clip
            .markers
            .iter()
            .find(|m| (m.time - media_t).abs() <= tol)
            .map(|m| m.id.clone())
        {
            return Some(existing);
        }
        self.push_history();
        let clip = self.clips.iter_mut().find(|c| c.id == clip_id)?;
        let marker = Marker::new(media_t.max(0.0));
        let id = marker.id.clone();
        clip.markers.push(marker);
        sort_markers(&mut clip.markers);
        Some(id)
    }

    /// Clip-Marker ändern OHNE Snapshot (laufende Geste).
    pub fn clip_marker_update_live(
        &mut self,
        clip_id: &str,
        marker_id: &str,
        f: impl FnOnce(&mut Marker),
    ) {
        if let Some(clip) = self.clips.iter_mut().find(|c| c.id == clip_id) {
            if let Some(m) = clip.markers.iter_mut().find(|m| m.id == marker_id) {
                f(m);
                m.sanitize();
            }
            sort_markers(&mut clip.markers);
        }
        self.revision += 1;
    }

    /// Clip-Marker ändern (mit Undo-Snapshot).
    pub fn clip_marker_update(
        &mut self,
        clip_id: &str,
        marker_id: &str,
        f: impl FnOnce(&mut Marker),
    ) {
        let exists = self
            .clips
            .iter()
            .find(|c| c.id == clip_id)
            .is_some_and(|c| c.markers.iter().any(|m| m.id == marker_id));
        if !exists {
            return;
        }
        self.push_history();
        self.clip_marker_update_live(clip_id, marker_id, f);
    }

    /// Einen Clip-Marker entfernen.
    pub fn remove_clip_marker(&mut self, clip_id: &str, marker_id: &str) {
        let exists = self
            .clips
            .iter()
            .find(|c| c.id == clip_id)
            .is_some_and(|c| c.markers.iter().any(|m| m.id == marker_id));
        if !exists {
            return;
        }
        self.push_history();
        if let Some(clip) = self.clips.iter_mut().find(|c| c.id == clip_id) {
            clip.markers.retain(|m| m.id != marker_id);
        }
    }

    // ------------------------------------------------- In/Out (Loop-Bereich)
    // Halbgesetzte Zustände sind erlaubt; ein Punkt, der den anderen kreuzen
    // würde, löscht ihn (Premiere-Konvention).

    pub fn set_in_point(&mut self, t: Option<f64>) {
        match t {
            None => self.in_point = None,
            Some(t) => {
                let v = t.max(0.0);
                self.in_point = Some(v);
                if let Some(out) = self.out_point {
                    if out <= v + MIN_CLIP_DURATION - EPS {
                        self.out_point = None;
                    }
                }
            }
        }
    }

    pub fn set_out_point(&mut self, t: Option<f64>) {
        match t {
            None => self.out_point = None,
            Some(t) => {
                let v = t.max(0.0);
                self.out_point = Some(v);
                if let Some(inp) = self.in_point {
                    if inp >= v - MIN_CLIP_DURATION + EPS {
                        self.in_point = None;
                    }
                }
            }
        }
    }

    pub fn set_in_out_range(&mut self, a: f64, b: f64) {
        let lo = a.min(b).max(0.0);
        let hi = a.max(b).max(0.0);
        if hi - lo < MIN_CLIP_DURATION {
            return;
        }
        self.in_point = Some(lo);
        self.out_point = Some(hi);
    }

    pub fn clear_in_out(&mut self) {
        self.in_point = None;
        self.out_point = None;
    }

}
