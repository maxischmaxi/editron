//! impl-TimelineStore-Methoden (aus timeline.rs zerlegt).
use super::*;

impl TimelineStore {
    // ------------------------------------------------------------- Auswahl

    pub fn select_clips(&mut self, ids: &[String], mode: SelectMode, links: bool) {
        let expanded: Vec<String> = if links {
            expand_links(&self.clips, ids)
        } else {
            let mut v = ids.to_vec();
            v.sort();
            v.dedup();
            v
        };
        // Clip- und Übergangsauswahl schließen sich aus (Premiere-Verhalten).
        self.selected_transition_ids.clear();
        match mode {
            SelectMode::Replace => self.selected_clip_ids = expanded,
            SelectMode::Add => {
                for id in expanded {
                    if !self.selected_clip_ids.contains(&id) {
                        self.selected_clip_ids.push(id);
                    }
                }
            }
            SelectMode::Toggle => {
                let all_in = expanded.iter().all(|id| self.selected_clip_ids.contains(id));
                if all_in {
                    self.selected_clip_ids.retain(|id| !expanded.contains(id));
                } else {
                    for id in expanded {
                        if !self.selected_clip_ids.contains(&id) {
                            self.selected_clip_ids.push(id);
                        }
                    }
                }
            }
        }
    }

    pub fn select_all(&mut self) {
        self.selected_clip_ids = self.clips.iter().map(|c| c.id.clone()).collect();
        self.selected_transition_ids.clear();
    }

    pub fn clear_selection(&mut self) {
        self.selected_clip_ids.clear();
        self.selected_transition_ids.clear();
    }

    // -------------------------------------------------------------- Spuren

    pub fn add_track(&mut self, kind: TrackKind) -> String {
        let track = make_track(kind);
        let id = track.id.clone();
        self.push_history();
        // Neue Videospur oben auf den Video-Block, neue Audiospur unten,
        // neue Untertitelspur ganz oben (über allen Untertitel-Spuren).
        match kind {
            TrackKind::Video => {
                let at = self.video_block_start();
                self.tracks.insert(at, track);
            }
            TrackKind::Audio => self.tracks.push(track),
            TrackKind::Subtitle => self.tracks.insert(0, track),
        }
        if kind == TrackKind::Subtitle {
            self.active_subtitle_track_id = Some(id.clone());
        }
        id
    }

    /// Index der obersten Videospur (Untertitel-Spuren liegen davor).
    pub(crate) fn video_block_start(&self) -> usize {
        self.tracks
            .iter()
            .position(|t| t.kind != TrackKind::Subtitle)
            .unwrap_or(self.tracks.len())
    }

    /// Verschachtelte Sequenzen als Nest-Clips einsetzen (Drop aus dem
    /// Medien-Browser). `planned`: (sequenz_id, anzeigename, länge_sekunden,
    /// hat_audio). Je Nest entsteht ein Video-Clip und — falls die innere
    /// Sequenz Audio enthält — ein verknüpfter Audio-Clip; der Zielbereich
    /// wird überschrieben (Overwrite wie beim Asset-Drop). `nest_seq` markiert
    /// die Clips; `asset_id` bleibt leer. Liefert die Anzahl eingefügter Nests.
    /// Der Rekursionsschutz liegt beim Aufrufer (siehe
    /// [`SequenceStore::insert_nests`](crate::core::sequences::SequenceStore::insert_nests)).
    pub fn insert_nest_clips(
        &mut self,
        planned: &[(String, String, f64, bool)],
        at: f64,
        drop_track_id: Option<&str>,
    ) -> usize {
        if planned.is_empty() {
            return 0;
        }
        self.push_history();
        let drop_track = drop_track_id
            .and_then(|id| self.tracks.iter().find(|t| t.id == id))
            .cloned();

        // Video-Zielspur: Drop-Spur, sonst oberste freie, sonst neu anlegen.
        let video_track = drop_track
            .as_ref()
            .filter(|t| t.kind == TrackKind::Video && !t.locked)
            .map(|t| t.id.clone())
            .or_else(|| {
                self.tracks
                    .iter().rfind(|t| t.kind == TrackKind::Video && !t.locked)
                    .map(|t| t.id.clone())
            })
            .unwrap_or_else(|| {
                let track = make_track(TrackKind::Video);
                let id = track.id.clone();
                let idx = self.video_block_start();
                self.tracks.insert(idx, track);
                id
            });
        // Audio-Zielspur lazy bestimmen (erst beim ersten Audio-Nest anlegen).
        let mut audio_track: Option<String> = self
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Audio && !t.locked)
            .map(|t| t.id.clone());

        let mut clips = std::mem::take(&mut self.clips);
        let mut inserted_ids: Vec<String> = Vec::new();
        let mut cursor = at.max(0.0);
        let mut count = 0usize;
        for (seq_id, name, length, has_audio) in planned {
            let dur = length.max(MIN_CLIP_DURATION);
            let link_id = if *has_audio { Some(new_id()) } else { None };
            clips = overwrite_range(clips, &video_track, cursor, cursor + dur);
            let vid = TimelineClip {
                id: new_id(),
                track_id: video_track.clone(),
                asset_id: String::new(),
                name: name.clone(),
                kind: TrackKind::Video,
                start: cursor,
                duration: dur,
                src_in: 0.0,
                src_duration: dur,
                link_id: link_id.clone(),
                enabled: true,
                gain_db: 0.0,
                fx: ClipFx::default(),
                grade: ColorGrade::default(),
                effects: Vec::new(),
                title: None,
                subtitle: None,
                speed: 1.0,
                reverse: false,
                freeze: false,
                markers: Vec::new(),
                nest_seq: Some(seq_id.clone()),
                multicam: None,
            };
            inserted_ids.push(vid.id.clone());
            clips.push(vid);
            if *has_audio {
                let atr = match &audio_track {
                    Some(id) => id.clone(),
                    None => {
                        let track = make_track(TrackKind::Audio);
                        let id = track.id.clone();
                        self.tracks.push(track);
                        audio_track = Some(id.clone());
                        id
                    }
                };
                clips = overwrite_range(clips, &atr, cursor, cursor + dur);
                let aud = TimelineClip {
                    id: new_id(),
                    track_id: atr,
                    asset_id: String::new(),
                    name: format!("{name} (Audio)"),
                    kind: TrackKind::Audio,
                    start: cursor,
                    duration: dur,
                    src_in: 0.0,
                    src_duration: dur,
                    link_id,
                    enabled: true,
                    gain_db: 0.0,
                    fx: ClipFx::default(),
                    grade: ColorGrade::default(),
                    effects: Vec::new(),
                    title: None,
                    subtitle: None,
                    speed: 1.0,
                    reverse: false,
                    freeze: false,
                    markers: Vec::new(),
                    nest_seq: Some(seq_id.clone()),
                    multicam: None,
                };
                inserted_ids.push(aud.id.clone());
                clips.push(aud);
            }
            cursor += dur;
            count += 1;
        }
        self.clips = clips;
        self.selected_clip_ids = inserted_ids;
        self.selected_transition_ids.clear();
        self.reconcile_transitions();
        count
    }

    /// Einen Multicam-Clip (Video + verknüpftes Audio) aus einer Multicam-Quelle
    /// einsetzen — wie der Asset-Drop ein Overwrite am Zielbereich. `source_id`
    /// ist die Quell-Sequenz; der aktive Winkel startet bei 0. Liefert die
    /// ID des Video-Clips.
    pub fn insert_multicam_clip(
        &mut self,
        source_id: &str,
        name: &str,
        duration: f64,
        has_audio: bool,
        at: f64,
        drop_track_id: Option<&str>,
    ) -> String {
        let dur = duration.max(MIN_CLIP_DURATION);
        self.push_history();
        let drop_track = drop_track_id
            .and_then(|id| self.tracks.iter().find(|t| t.id == id))
            .cloned();
        let video_track = drop_track
            .as_ref()
            .filter(|t| t.kind == TrackKind::Video && !t.locked)
            .map(|t| t.id.clone())
            .or_else(|| {
                self.tracks
                    .iter().rfind(|t| t.kind == TrackKind::Video && !t.locked)
                    .map(|t| t.id.clone())
            })
            .unwrap_or_else(|| {
                let track = make_track(TrackKind::Video);
                let id = track.id.clone();
                let idx = self.video_block_start();
                self.tracks.insert(idx, track);
                id
            });
        let link_id = if has_audio { Some(new_id()) } else { None };
        let cursor = at.max(0.0);
        let mut clips = std::mem::take(&mut self.clips);
        clips = overwrite_range(clips, &video_track, cursor, cursor + dur);
        let mut vid =
            new_multicam_clip(&video_track, source_id, 0, name, TrackKind::Video, cursor, dur, 0.0, dur);
        vid.link_id = link_id.clone();
        let vid_id = vid.id.clone();
        clips.push(vid);
        if has_audio {
            let atr = self
                .tracks
                .iter()
                .find(|t| t.kind == TrackKind::Audio && !t.locked)
                .map(|t| t.id.clone())
                .unwrap_or_else(|| {
                    let track = make_track(TrackKind::Audio);
                    let id = track.id.clone();
                    self.tracks.push(track);
                    id
                });
            clips = overwrite_range(clips, &atr, cursor, cursor + dur);
            let mut aud = new_multicam_clip(
                &atr,
                source_id,
                0,
                format!("{name} (Audio)"),
                TrackKind::Audio,
                cursor,
                dur,
                0.0,
                dur,
            );
            aud.link_id = link_id;
            clips.push(aud);
        }
        self.clips = clips;
        self.selected_clip_ids = vec![vid_id.clone()];
        self.selected_transition_ids.clear();
        self.reconcile_transitions();
        vid_id
    }

    pub fn remove_track(&mut self, track_id: &str) {
        if !self.tracks.iter().any(|t| t.id == track_id) {
            return;
        }
        self.push_history();
        self.tracks.retain(|t| t.id != track_id);
        self.clips.retain(|c| c.track_id != track_id);
        if self.active_subtitle_track_id.as_deref() == Some(track_id) {
            self.active_subtitle_track_id = None;
        }
        self.prune_selection();
        self.reconcile_transitions();
    }

    pub fn toggle_track_flag(&mut self, track_id: &str, flag: TrackFlag) {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            match flag {
                TrackFlag::Muted => t.muted = !t.muted,
                TrackFlag::Solo => t.solo = !t.solo,
                TrackFlag::Locked => t.locked = !t.locked,
                TrackFlag::SyncLock => t.sync_lock = !t.sync_lock,
                TrackFlag::Targeted => t.targeted = !t.targeted,
            }
            // Flags werden mitgespeichert → Projekt muss dirty werden
            // (bewusst ohne History-Snapshot, wie In-/Out-Punkte).
            self.revision += 1;
        }
    }

    /// Spur als Source-Patch-Ziel ihrer Art setzen (Radio: höchstens eine
    /// Video- und eine Audiospur). Erneutes Klicken hebt den Patch auf, sodass
    /// das entsprechende Quell-Material beim Edit übersprungen wird (Premiere:
    /// Patch deaktivieren). Untertitel-Spuren sind kein Patch-Ziel.
    pub fn toggle_source_patch(&mut self, track_id: &str) {
        let Some(kind) = self.tracks.iter().find(|t| t.id == track_id).map(|t| t.kind) else {
            return;
        };
        if kind == TrackKind::Subtitle {
            return;
        }
        let was = self
            .tracks
            .iter()
            .find(|t| t.id == track_id)
            .map(|t| t.source_patched)
            .unwrap_or(false);
        for t in &mut self.tracks {
            if t.kind == kind {
                t.source_patched = false;
            }
        }
        if !was {
            if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
                t.source_patched = true;
            }
        }
        self.revision += 1;
    }

    /// Aktuelles Source-Patch-Ziel einer Art (ungesperrt), falls vorhanden.
    pub fn source_patch_track(&self, kind: TrackKind) -> Option<&str> {
        self.tracks
            .iter()
            .find(|t| t.kind == kind && t.source_patched && !t.locked)
            .map(|t| t.id.as_str())
    }

    /// Anvisierte, ungesperrte Spur-IDs (Lift/Extract/Match-Frame-Ziele).
    pub fn targeted_track_ids(&self) -> std::collections::HashSet<String> {
        self.tracks
            .iter()
            .filter(|t| t.targeted && !t.locked && t.kind != TrackKind::Subtitle)
            .map(|t| t.id.clone())
            .collect()
    }

    /// Stellt für Video und Audio je ein Patch- und Targeting-Ziel sicher,
    /// falls keines gesetzt ist (Migration von Altprojekten vor Formatv7).
    /// Standard wie ein frisches Projekt: V1 (unterste Video-) und A1 (oberste
    /// Audiospur).
    pub fn ensure_patch_target_defaults(&mut self) {
        for kind in [TrackKind::Video, TrackKind::Audio] {
            let default_id = match kind {
                TrackKind::Video => self
                    .tracks
                    .iter().rfind(|t| t.kind == kind)
                    .map(|t| t.id.clone()),
                _ => self.tracks.iter().find(|t| t.kind == kind).map(|t| t.id.clone()),
            };
            let Some(default_id) = default_id else { continue };
            if !self.tracks.iter().any(|t| t.kind == kind && t.source_patched) {
                if let Some(t) = self.tracks.iter_mut().find(|t| t.id == default_id) {
                    t.source_patched = true;
                }
            }
            if !self.tracks.iter().any(|t| t.kind == kind && t.targeted) {
                if let Some(t) = self.tracks.iter_mut().find(|t| t.id == default_id) {
                    t.targeted = true;
                }
            }
        }
        self.revision += 1;
    }

    // ------------------------------------------------------------- Mixer

    /// Beginn einer Mixer-Geste (Fader-/Pan-Drag, Reset): legt einmalig
    /// einen Undo-Snapshot an; die laufenden Wertänderungen der Geste
    /// schreiben danach direkt über die Setter.
    pub fn begin_mix_edit(&mut self) {
        self.push_history();
    }

    pub fn set_track_gain_db(&mut self, track_id: &str, gain_db: f64) {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            t.gain_db = gain_db.clamp(-60.0, 6.0);
        }
    }

    pub fn set_track_pan(&mut self, track_id: &str, pan: f64) {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            t.pan = pan.clamp(-1.0, 1.0);
        }
    }

    pub fn set_master_gain_db(&mut self, gain_db: f64) {
        self.master_gain_db = gain_db.clamp(-60.0, 6.0);
    }

    /// Clip-Verstärkung der ausgewählten Audio-Clips um `delta_db` ändern
    /// (Premiere-Pendant „Audio-Verstärkung anpassen“).
    pub fn nudge_selected_clip_gain(&mut self, delta_db: f64) {
        let sel = self.selected_clip_ids.clone();
        if !self
            .clips
            .iter()
            .any(|c| sel.contains(&c.id) && c.kind == TrackKind::Audio)
        {
            return;
        }
        self.push_history();
        for clip in self
            .clips
            .iter_mut()
            .filter(|c| sel.contains(&c.id) && c.kind == TrackKind::Audio)
        {
            clip.gain_db = (clip.gain_db + delta_db).clamp(-60.0, 24.0);
        }
    }

    /// Clip-Verstärkung der ausgewählten Audio-Clips auf 0 dB zurücksetzen.
    pub fn reset_selected_clip_gain(&mut self) {
        let sel = self.selected_clip_ids.clone();
        if !self
            .clips
            .iter()
            .any(|c| sel.contains(&c.id) && c.kind == TrackKind::Audio && c.gain_db != 0.0)
        {
            return;
        }
        self.push_history();
        for clip in self
            .clips
            .iter_mut()
            .filter(|c| sel.contains(&c.id) && c.kind == TrackKind::Audio)
        {
            clip.gain_db = 0.0;
        }
    }

}
