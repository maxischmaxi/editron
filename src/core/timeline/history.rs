//! impl-TimelineStore-Methoden (aus timeline.rs zerlegt).
use super::*;

impl TimelineStore {
    // -------------------------------------------------- Attribute-Klemmbrett

    /// Attribute des Clips (inkl. A/V-Partner) ins Klemmbrett kopieren —
    /// Premiere-Workflow „Attribute einfügen“.
    pub fn copy_attributes(&mut self, id: &str) -> bool {
        let Some(clip) = self.clip(id) else { return false };
        let mut attrs = ClipAttributes {
            fx: clip.fx.clone(),
            grade: clip.grade.clone(),
            effects: clip.effects.clone(),
            gain_db: clip.gain_db,
            from_kind: clip.kind,
            linked: None,
        };
        // Verknüpfter Partner liefert die jeweils andere Hälfte mit.
        if let Some(link) = clip.link_id.as_ref() {
            if let Some(partner) = self
                .clips
                .iter()
                .find(|c| c.id != clip.id && c.link_id.as_deref() == Some(link))
            {
                attrs.linked = Some(Box::new(ClipAttributes {
                    fx: partner.fx.clone(),
                    grade: partner.grade.clone(),
                    effects: partner.effects.clone(),
                    gain_db: partner.gain_db,
                    from_kind: partner.kind,
                    linked: None,
                }));
            }
        }
        self.attr_clipboard = Some(attrs);
        true
    }

    /// Attribute aus dem Klemmbrett auf die Clips anwenden (artgerecht:
    /// Video-Clips erhalten Video-Attribute, Audio-Clips Audio-Attribute).
    /// Effekt-Instanzen bekommen frische IDs.
    pub fn paste_attributes(&mut self, ids: &[String]) {
        let Some(attrs) = self.attr_clipboard.clone() else { return };
        let locked = locked_track_ids(&self.tracks);
        let affected: Vec<String> = self
            .clips
            .iter()
            .filter(|c| ids.contains(&c.id) && !locked.contains(&c.track_id))
            .map(|c| c.id.clone())
            .collect();
        if affected.is_empty() {
            return;
        }
        self.push_history();
        for c in &mut self.clips {
            if !affected.contains(&c.id) {
                continue;
            }
            // Quelle wählen: passende Art aus Haupt- oder Partner-Attributen.
            let src = if attrs.from_kind == c.kind {
                Some(&attrs)
            } else {
                attrs
                    .linked
                    .as_deref()
                    .filter(|l| l.from_kind == c.kind)
            };
            let Some(src) = src else { continue };
            c.grade = src.grade.clone();
            c.gain_db = src.gain_db;
            match c.kind {
                // Untertitel-Segmente sind transformierbar wie Video-Layer.
                TrackKind::Video | TrackKind::Subtitle => {
                    c.fx = src.fx.clone();
                }
                TrackKind::Audio => {
                    // Nur die Lautstärke-Kurve übernehmen (Video-Transform
                    // hat auf Audio-Clips keine Bedeutung).
                    c.fx.volume_db = src.fx.volume_db.clone();
                }
            }
            c.effects = src
                .effects
                .iter()
                .filter(|e| e.kind.is_audio() == (c.kind == TrackKind::Audio))
                .map(|e| {
                    let mut e = e.clone();
                    e.id = new_id();
                    // Masken bekommen ebenfalls frische IDs (global eindeutig).
                    for m in &mut e.masks {
                        m.id = new_id();
                    }
                    e
                })
                .collect();
        }
    }

    pub fn has_attr_clipboard(&self) -> bool {
        self.attr_clipboard.is_some()
    }

    /// Eine kopierte Farbkorrektur auf alle angegebenen Clips übertragen
    /// (Premiere-/Resolve-Workflow „Grade einfügen“). Wirkt nur auf sichtbare
    /// Clips (Video/Untertitel) auf entsperrten Spuren — Audio-Clips tragen
    /// keine sichtbare Farbkorrektur und werden übersprungen. Ein einziger
    /// Undo-Schnappschuss für die gesamte Mehrfachauswahl. Liefert die Anzahl
    /// der tatsächlich geänderten Clips.
    pub fn paste_grade(&mut self, grade: &ColorGrade, ids: &[String]) -> usize {
        let locked = locked_track_ids(&self.tracks);
        // Nur Clips, die sich tatsächlich ändern: Trägt ein Ziel den Grade
        // schon, bleibt es unangetastet — so entsteht kein leerer Undo-Schritt
        // (z. B. beim Einfügen auf den Quell-Clip selbst).
        let affected: Vec<String> = self
            .clips
            .iter()
            .filter(|c| {
                ids.contains(&c.id)
                    && c.kind != TrackKind::Audio
                    && !locked.contains(&c.track_id)
                    && c.grade != *grade
            })
            .map(|c| c.id.clone())
            .collect();
        if affected.is_empty() {
            return 0;
        }
        self.push_history();
        for c in &mut self.clips {
            if affected.contains(&c.id) {
                c.grade = grade.clone();
            }
        }
        affected.len()
    }

    // ------------------------------------------------------------- Verlauf

    /// Sequenz der zuletzt rückgängig machbaren Timeline-Operation (für die
    /// `edit.undo`-Koordination; höchste Sequenz = jüngste Operation).
    pub fn undo_seq(&self) -> Option<u64> {
        self.past.last().map(|s| s.seq)
    }

    /// Sequenz der als Nächstes wiederherstellbaren Timeline-Operation.
    pub fn redo_seq(&self) -> Option<u64> {
        self.future.first().map(|s| s.seq)
    }

    pub fn undo(&mut self) {
        let Some(prev) = self.past.pop() else { return };
        self.future.insert(
            0,
            Snapshot {
                tracks: std::mem::replace(&mut self.tracks, prev.tracks),
                clips: std::mem::replace(&mut self.clips, prev.clips),
                transitions: std::mem::replace(&mut self.transitions, prev.transitions),
                markers: std::mem::replace(&mut self.markers, prev.markers),
                master_gain_db: std::mem::replace(&mut self.master_gain_db, prev.master_gain_db),
                seq: prev.seq,
            },
        );
        self.prune_selection();
        self.prune_transition_selection();
        self.revision += 1;
    }

    pub fn redo(&mut self) {
        if self.future.is_empty() {
            return;
        }
        let next = self.future.remove(0);
        self.past.push(Snapshot {
            tracks: std::mem::replace(&mut self.tracks, next.tracks),
            clips: std::mem::replace(&mut self.clips, next.clips),
            transitions: std::mem::replace(&mut self.transitions, next.transitions),
            markers: std::mem::replace(&mut self.markers, next.markers),
            master_gain_db: std::mem::replace(&mut self.master_gain_db, next.master_gain_db),
            seq: next.seq,
        });
        self.prune_selection();
        self.prune_transition_selection();
        self.revision += 1;
    }

    // ----------------------------------------------------------- Übergänge

    pub fn transition(&self, id: &str) -> Option<&Transition> {
        self.transitions.iter().find(|t| t.id == id)
    }

    /// Spur eines Übergangs (über die referenzierten Clips).
    pub fn transition_track_id(&self, tr: &Transition) -> Option<String> {
        let (from, to) = transitions::resolve_clips(&self.clips, tr);
        from.or(to).map(|c| c.track_id.clone())
    }

    /// Zeitfenster `[w0, w1)` eines Übergangs in Sequenzzeit.
    pub fn transition_window(&self, tr: &Transition) -> Option<(f64, f64)> {
        let (from, to) = transitions::resolve_clips(&self.clips, tr);
        transitions::window(from, to, tr.alignment, tr.duration)
    }

    /// Schnittkante eines Übergangs in Sequenzzeit.
    pub fn transition_cut(&self, tr: &Transition) -> Option<f64> {
        let (from, to) = transitions::resolve_clips(&self.clips, tr);
        transitions::cut_time(from, to)
    }

    /// Maximal erlaubte Dauer eines Übergangs (Handles + Cliplängen).
    pub fn transition_max_duration(&self, tr: &Transition) -> f64 {
        let (from, to) = transitions::resolve_clips(&self.clips, tr);
        transitions::max_duration(from, to, tr.alignment)
    }

    /// Audio-Crossfade-Fenster eines Clips: (w0, w1, fade_in, equal_power).
    /// `fade_in` = Clip ist die eingehende Seite. Höchstens zwei Einträge
    /// (Clipanfang + Clipende). Player-Mixdown und Export-Planer nutzen
    /// dieselben Fenster — identische Hüllkurven.
    pub fn audio_fades(&self, clip: &TimelineClip) -> Vec<(f64, f64, bool, bool)> {
        let mut fades = Vec::new();
        for tr in &self.transitions {
            if !tr.kind.is_audio() {
                continue;
            }
            let fade_in = match (tr.from_clip_id.as_deref(), tr.to_clip_id.as_deref()) {
                (_, Some(to)) if to == clip.id => true,
                (Some(from), _) if from == clip.id => false,
                _ => continue,
            };
            let Some((w0, w1)) = self.transition_window(tr) else {
                continue;
            };
            if w1 > w0 {
                let equal_power = tr.kind == TransitionKind::ConstantPower;
                fades.push((w0, w1, fade_in, equal_power));
            }
        }
        fades
    }

    /// Hörbarer Bereich eines Audio-Clips inkl. Übergangs-Verlängerung.
    pub fn audio_extent(&self, clip: &TimelineClip, fades: &[(f64, f64, bool, bool)]) -> (f64, f64) {
        let mut a0 = clip.start;
        let mut a1 = clip.end();
        for (w0, w1, _, _) in fades {
            a0 = a0.min(*w0);
            a1 = a1.max(*w1);
        }
        (a0, a1)
    }

    fn prune_transition_selection(&mut self) {
        let existing: std::collections::HashSet<&str> =
            self.transitions.iter().map(|t| t.id.as_str()).collect();
        self.selected_transition_ids
            .retain(|id| existing.contains(id.as_str()));
    }

    /// Übergang auswählen (ersetzt Clip- und Übergangsauswahl).
    pub fn select_transition(&mut self, id: &str) {
        if self.transition(id).is_some() {
            self.selected_clip_ids.clear();
            self.selected_transition_ids = vec![id.to_string()];
        }
    }

    /// Konsistenz nach strukturellen Edits herstellen: verwaiste, nicht
    /// mehr benachbarte oder zu lange Übergänge entfernen bzw. kürzen;
    /// überlappende Fenster derselben Spur auflösen (frühere Kante gewinnt).
    pub(crate) fn reconcile_transitions(&mut self) {
        let clips = std::mem::take(&mut self.clips);
        let mut kept: Vec<Transition> = Vec::new();
        for mut tr in std::mem::take(&mut self.transitions) {
            let (from, to) = transitions::resolve_clips(&clips, &tr);
            // Referenzierte, aber verschwundene Clips ⇒ Übergang entfällt.
            if tr.from_clip_id.is_some() && from.is_none() {
                continue;
            }
            if tr.to_clip_id.is_some() && to.is_none() {
                continue;
            }
            let Some(anchor) = from.or(to) else { continue };
            // Art muss zur Spurart passen.
            if tr.kind.is_audio() != (anchor.kind == TrackKind::Audio) {
                continue;
            }
            // Zweiseitig: gleiche Spur + direkte Nachbarschaft.
            if let (Some(f), Some(t)) = (from, to) {
                if f.track_id != t.track_id || (f.end() - t.start).abs() > EPS {
                    continue;
                }
            }
            // Dauer an Handles/Cliplängen klemmen.
            let max = transitions::max_duration(from, to, tr.alignment);
            if max < MIN_CLIP_DURATION - EPS {
                continue;
            }
            tr.duration = tr.duration.min(max).max(MIN_CLIP_DURATION);
            kept.push(tr);
        }
        // Überlappungen je Spur entfernen: nach Fensterbeginn sortiert,
        // jede Kante, die in ein bereits akzeptiertes Fenster ragt, fliegt.
        let mut order: Vec<(String, f64, f64, usize)> = kept
            .iter()
            .enumerate()
            .filter_map(|(i, tr)| {
                let (from, to) = transitions::resolve_clips(&clips, tr);
                let track = from.or(to)?.track_id.clone();
                let (w0, w1) = transitions::window(from, to, tr.alignment, tr.duration)?;
                Some((track, w0, w1, i))
            })
            .collect();
        order.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        let mut drop: std::collections::HashSet<usize> = Default::default();
        let mut last: Option<(String, f64)> = None; // (Spur, bisheriges Fensterende)
        for (track, w0, w1, i) in order {
            match &last {
                Some((lt, lw1)) if *lt == track && w0 < lw1 - EPS => {
                    drop.insert(i);
                }
                _ => last = Some((track, w1)),
            }
        }
        if !drop.is_empty() {
            kept = kept
                .into_iter()
                .enumerate()
                .filter(|(i, _)| !drop.contains(i))
                .map(|(_, t)| t)
                .collect();
        }
        self.transitions = kept;
        self.clips = clips;
        self.prune_transition_selection();
    }

    /// Übergang an der Kante eines Clips anwenden: `edge` Start = Kante am
    /// Clipanfang, End = am Clipende. Existiert dort bereits ein Übergang,
    /// wird er ersetzt. Liefert die neue ID oder eine Fehlermeldung.
    pub fn add_transition(
        &mut self,
        kind: TransitionKind,
        clip_id: &str,
        edge: TrimEdge,
        duration: f64,
    ) -> Result<String, String> {
        let locked = locked_track_ids(&self.tracks);
        let Some(clip) = self.clip(clip_id).cloned() else {
            return Err("Clip nicht gefunden".into());
        };
        if locked.contains(&clip.track_id) {
            return Err("Spur ist gesperrt".into());
        }
        if clip.kind == TrackKind::Subtitle || kind.is_audio() != (clip.kind == TrackKind::Audio) {
            return Err(format!("„{}“ passt nicht zur Spurart", kind.label()));
        }
        // Adjustment Layer sind Vollbild-Korrektur-Pässe — Übergänge (Wipe/Push/
        // Dissolve) sind dafür weder sinnvoll noch im Compositor abgebildet; zum
        // Ein-/Ausblenden der Wirkung dienen Deckkraft-Keyframes.
        if clip.is_adjustment() {
            return Err("Einstellungsebenen nehmen keine Übergänge (Deckkraft-Keyframes nutzen)".into());
        }
        // Nachbar an der Kante (direkt angrenzend auf derselben Spur).
        let neighbor = match edge {
            TrimEdge::Start => self
                .clips
                .iter()
                .find(|c| c.track_id == clip.track_id && (c.end() - clip.start).abs() < EPS),
            TrimEdge::End => self
                .clips
                .iter()
                .find(|c| c.track_id == clip.track_id && (c.start - clip.end()).abs() < EPS),
        }
        .cloned();
        let (from_id, to_id) = match edge {
            TrimEdge::Start => (neighbor.as_ref().map(|c| c.id.clone()), Some(clip.id.clone())),
            TrimEdge::End => (Some(clip.id.clone()), neighbor.as_ref().map(|c| c.id.clone())),
        };
        let (from, to) = match edge {
            TrimEdge::Start => (neighbor.as_ref(), Some(&clip)),
            TrimEdge::End => (Some(&clip), neighbor.as_ref()),
        };
        let alignment = TransitionAlignment::Center;
        let max = transitions::max_duration(from, to, alignment);
        if max < MIN_CLIP_DURATION - EPS {
            return Err("Nicht genug Material (Handles) für einen Übergang an dieser Kante".into());
        }
        let clamped = duration.min(max).max(MIN_CLIP_DURATION);

        self.push_history();
        // Bestehenden Übergang an derselben Schnittkante ersetzen (auch
        // wenn er einseitig war oder andersherum verankert ist).
        let cut = match edge {
            TrimEdge::Start => clip.start,
            TrimEdge::End => clip.end(),
        };
        let replaced: Vec<String> = self
            .transitions
            .iter()
            .filter(|t| {
                self.transition_track_id(t).as_deref() == Some(clip.track_id.as_str())
                    && self
                        .transition_cut(t)
                        .is_some_and(|c| (c - cut).abs() < EPS)
            })
            .map(|t| t.id.clone())
            .collect();
        self.transitions.retain(|t| !replaced.contains(&t.id));
        let mut tr = Transition::new(kind, from_id, to_id, clamped);
        tr.alignment = alignment;
        let id = tr.id.clone();
        self.transitions.push(tr);
        self.reconcile_transitions();
        if self.transition(&id).is_some() {
            self.selected_clip_ids.clear();
            self.selected_transition_ids = vec![id.clone()];
            Ok(id)
        } else {
            // Reconcile hat ihn verworfen (z. B. Überlappung mit Nachbar-Übergang).
            Err("Übergang kollidiert mit einem bestehenden Übergang".into())
        }
    }

    /// Standard-/Wunschübergang auf alle passenden Kanten der Auswahl
    /// anwenden (Premiere: Mod+D / Mod+Shift+D). Bereits belegte Kanten
    /// werden übersprungen. Liefert die Anzahl neu gesetzter Übergänge.
    pub fn apply_transition_to_selection(&mut self, kind: TransitionKind) -> usize {
        let locked = locked_track_ids(&self.tracks);
        let want_audio = kind.is_audio();
        let selection: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| {
                self.selected_clip_ids.contains(&c.id)
                    && c.kind != TrackKind::Subtitle
                    // Einstellungsebenen nehmen keine Übergänge (Vollbild-Pass).
                    && !c.is_adjustment()
                    && (c.kind == TrackKind::Audio) == want_audio
                    && !locked.contains(&c.track_id)
            })
            .cloned()
            .collect();
        if selection.is_empty() {
            return 0;
        }

        // Belegte Kanten: Cut-Zeiten bestehender Übergänge je Spur.
        let occupied: Vec<(String, f64)> = self
            .transitions
            .iter()
            .filter_map(|t| Some((self.transition_track_id(t)?, self.transition_cut(t)?)))
            .collect();
        let is_free = |track: &str, cut: f64| {
            !occupied
                .iter()
                .any(|(tr, c)| tr == track && (c - cut).abs() < EPS)
        };

        let mut planned: Vec<Transition> = Vec::new();
        let mut planned_cuts: Vec<(String, f64)> = Vec::new();
        for clip in &selection {
            for edge in [TrimEdge::Start, TrimEdge::End] {
                let cut = match edge {
                    TrimEdge::Start => clip.start,
                    TrimEdge::End => clip.end(),
                };
                if !is_free(&clip.track_id, cut)
                    || planned_cuts
                        .iter()
                        .any(|(t, c)| t == &clip.track_id && (c - cut).abs() < EPS)
                {
                    continue;
                }
                let neighbor = self.clips.iter().find(|c| {
                    c.id != clip.id
                        && c.track_id == clip.track_id
                        && match edge {
                            TrimEdge::Start => (c.end() - clip.start).abs() < EPS,
                            TrimEdge::End => (c.start - clip.end()).abs() < EPS,
                        }
                });
                let (from, to) = match edge {
                    TrimEdge::Start => (neighbor, Some(clip)),
                    TrimEdge::End => (Some(clip), neighbor),
                };
                let max = transitions::max_duration(from, to, TransitionAlignment::Center);
                if max < MIN_CLIP_DURATION - EPS {
                    continue;
                }
                let duration = DEFAULT_TRANSITION_DURATION.min(max).max(MIN_CLIP_DURATION);
                planned.push(Transition::new(
                    kind,
                    from.map(|c| c.id.clone()),
                    to.map(|c| c.id.clone()),
                    duration,
                ));
                planned_cuts.push((clip.track_id.clone(), cut));
            }
        }
        if planned.is_empty() {
            return 0;
        }
        self.push_history();
        let before = self.transitions.len();
        self.transitions.extend(planned);
        self.reconcile_transitions();
        self.transitions.len().saturating_sub(before)
    }

    pub fn remove_transitions(&mut self, ids: &[String]) {
        if !self.transitions.iter().any(|t| ids.contains(&t.id)) {
            return;
        }
        self.push_history();
        self.transitions.retain(|t| !ids.contains(&t.id));
        self.prune_transition_selection();
    }

    /// Dauer setzen (geklemmt an Handles/Cliplängen) — mit Undo-Snapshot.
    pub fn set_transition_duration(&mut self, id: &str, duration: f64) {
        let Some(tr) = self.transition(id).cloned() else { return };
        let max = self.transition_max_duration(&tr);
        let clamped = duration.clamp(MIN_CLIP_DURATION, max.max(MIN_CLIP_DURATION));
        if (clamped - tr.duration).abs() < EPS {
            return;
        }
        self.push_history();
        if let Some(t) = self.transitions.iter_mut().find(|t| t.id == id) {
            t.duration = clamped;
        }
        self.reconcile_transitions();
    }

    pub fn set_transition_alignment(&mut self, id: &str, alignment: TransitionAlignment) {
        let Some(tr) = self.transition(id).cloned() else { return };
        if !tr.is_two_sided() || tr.alignment == alignment {
            return;
        }
        self.push_history();
        if let Some(t) = self.transitions.iter_mut().find(|t| t.id == id) {
            t.alignment = alignment;
        }
        self.reconcile_transitions();
    }

    /// Art ersetzen (Kontextmenü „Ersetzen durch …“) — Spurart muss passen.
    pub fn set_transition_kind(&mut self, id: &str, kind: TransitionKind) {
        let Some(tr) = self.transition(id).cloned() else { return };
        if tr.kind == kind || tr.kind.is_audio() != kind.is_audio() {
            return;
        }
        self.push_history();
        if let Some(t) = self.transitions.iter_mut().find(|t| t.id == id) {
            t.kind = kind;
            if !kind.directional() {
                t.direction = kind.default_direction();
            }
        }
    }

    pub fn set_transition_direction(
        &mut self,
        id: &str,
        direction: crate::core::transitions::TransitionDirection,
    ) {
        let Some(tr) = self.transition(id) else { return };
        if tr.direction == direction || !tr.kind.directional() {
            return;
        }
        self.push_history();
        if let Some(t) = self.transitions.iter_mut().find(|t| t.id == id) {
            t.direction = direction;
        }
    }
}
