//! impl-TimelineStore-Methoden (aus timeline.rs zerlegt).
use super::*;

impl TimelineStore {
    // ------------------------------------------------ Verwendungs-Tracking

    /// Wie oft ein Asset in der Sequenz verwendet wird (Anzahl Clips). A/V-
    /// Paare zählen als zwei Clips — wie Premieres Verwendungszähler.
    pub fn asset_usage_count(&self, asset_id: &str) -> usize {
        self.clips.iter().filter(|c| c.asset_id == asset_id).count()
    }

    /// Frühester Clip, der `asset_id` verwendet: (clip_id, start_sec). Ziel von
    /// „In Timeline anzeigen“.
    pub fn first_use_of_asset(&self, asset_id: &str) -> Option<(String, f64)> {
        self.clips
            .iter()
            .filter(|c| c.asset_id == asset_id)
            .min_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal))
            .map(|c| (c.id.clone(), c.start))
    }

    /// Springt zur ersten Verwendung eines Assets: Playhead auf den Clip-Start,
    /// Clip auswählen. Liefert false, wenn das Asset nicht verwendet wird.
    pub fn reveal_asset_usage(&mut self, asset_id: &str) -> bool {
        let Some((clip_id, start)) = self.first_use_of_asset(asset_id) else {
            return false;
        };
        self.set_playhead(start);
        self.selected_transition_ids.clear();
        // Verknüpften Partner (A/V) mitselektieren, falls vorhanden.
        let mut ids = vec![clip_id.clone()];
        if let Some(link) = self.clip(&clip_id).and_then(|c| c.link_id.clone()) {
            for c in &self.clips {
                if c.link_id.as_deref() == Some(&link) && c.id != clip_id {
                    ids.push(c.id.clone());
                }
            }
        }
        self.selected_clip_ids = ids;
        true
    }

    // ------------------------------------------------------ Zwischenablage

    pub fn copy_selection(&mut self) {
        let selected: Vec<&TimelineClip> = self
            .clips
            .iter()
            .filter(|c| self.selected_clip_ids.contains(&c.id))
            .collect();
        if selected.is_empty() {
            return;
        }
        let base = selected.iter().map(|c| c.start).fold(f64::INFINITY, f64::min);
        let selected_ids: std::collections::HashSet<&str> =
            selected.iter().map(|c| c.id.as_str()).collect();
        self.clipboard = selected
            .into_iter()
            .map(|c| {
                let mut copy = c.clone();
                copy.start -= base;
                copy
            })
            .collect();
        // Übergänge mitnehmen, deren Kanten vollständig in der Auswahl liegen
        // (Referenzen zeigen auf die Clipboard-Clip-IDs).
        self.clipboard_transitions = self
            .transitions
            .iter()
            .filter(|t| {
                let covered = |id: &Option<String>| {
                    id.as_deref().is_none_or(|id| selected_ids.contains(id))
                };
                covered(&t.from_clip_id) && covered(&t.to_clip_id)
            })
            .cloned()
            .collect();
    }

    pub fn cut_selection(&mut self) {
        self.copy_selection();
        let ids = self.selected_clip_ids.clone();
        self.delete_clips(&ids, false);
    }

    /// Fügt die Zwischenablage bei `at` (sonst Playhead) ein — nicht-destruktiv:
    /// Statt belegte Bereiche zu überschreiben, weicht jeder Clip auf die
    /// nächste freie Spur aus (Video nach oben, Audio nach unten); existiert
    /// keine, wird eine neue Spur angelegt.
    pub fn paste(&mut self, at: Option<f64>) {
        use std::collections::HashMap;
        if self.clipboard.is_empty() {
            return;
        }
        let t = at.unwrap_or(self.playhead_sec).max(0.0);

        let occupied = |existing: &[TimelineClip],
                        pending: &[TimelineClip],
                        track_id: &str,
                        start: f64,
                        end: f64|
         -> bool {
            existing
                .iter()
                .chain(pending.iter())
                .any(|c| c.track_id == track_id && c.start < end - EPS && c.end() > start + EPS)
        };

        // tracks-Kopie wächst um neu angelegte Spuren; erst beim Commit übernehmen.
        let mut tracks = self.tracks.clone();
        let mut new_link_ids: HashMap<String, String> = HashMap::new();
        let mut id_map: HashMap<String, String> = HashMap::new();
        let mut pasted: Vec<TimelineClip> = Vec::new();
        for c in &self.clipboard {
            let start = t + c.start;
            let end = start + c.duration;

            // Kandidaten gleicher Art (ohne gesperrte), beginnend bei der
            // Originalspur — fehlt sie, bei V1 bzw. A1.
            let lanes: Vec<usize> = tracks
                .iter()
                .enumerate()
                .filter(|(_, tr)| tr.kind == c.kind && !tr.locked)
                .map(|(i, _)| i)
                .collect();
            let candidates: Vec<usize> = if lanes.is_empty() {
                Vec::new()
            } else {
                let desired = lanes
                    .iter()
                    .position(|&i| tracks[i].id == c.track_id)
                    .unwrap_or(match c.kind {
                        TrackKind::Video | TrackKind::Subtitle => lanes.len() - 1,
                        TrackKind::Audio => 0,
                    });
                match c.kind {
                    // Array-Index 0 = oberste Spur → „nach oben“ = rückwärts.
                    TrackKind::Video | TrackKind::Subtitle => {
                        lanes[..=desired].iter().rev().copied().collect()
                    }
                    TrackKind::Audio => lanes[desired..].to_vec(),
                }
            };
            let target = candidates.into_iter().find_map(|idx| {
                let id = tracks[idx].id.as_str();
                (!occupied(&self.clips, &pasted, id, start, end)).then(|| id.to_string())
            });
            let track_id = target.unwrap_or_else(|| {
                let track = make_track(c.kind);
                let id = track.id.clone();
                match c.kind {
                    // Neue Videospur über den Video-Block (hinter die
                    // Untertitel-Spuren), Untertitelspur ganz nach oben.
                    TrackKind::Video => {
                        let at = tracks
                            .iter()
                            .position(|t| t.kind != TrackKind::Subtitle)
                            .unwrap_or(tracks.len());
                        tracks.insert(at, track);
                    }
                    TrackKind::Audio => tracks.push(track),
                    TrackKind::Subtitle => tracks.insert(0, track),
                }
                id
            });

            let link_id = c.link_id.as_ref().map(|link| {
                new_link_ids
                    .entry(link.clone())
                    .or_insert_with(new_id)
                    .clone()
            });
            let mut p = c.clone();
            p.id = new_id();
            id_map.insert(c.id.clone(), p.id.clone());
            p.track_id = track_id;
            p.start = start;
            p.link_id = link_id;
            pasted.push(p);
        }
        if pasted.is_empty() {
            return;
        }
        self.push_history();
        self.tracks = tracks;
        self.selected_clip_ids = pasted.iter().map(|c| c.id.clone()).collect();
        self.clips.extend(pasted);
        // Übergänge der Zwischenablage auf die frischen Clip-IDs übertragen.
        let copies: Vec<Transition> = self
            .clipboard_transitions
            .iter()
            .filter_map(|t| remap_transition(t, &id_map))
            .collect();
        self.transitions.extend(copies);
        self.reconcile_transitions();
    }

    // ------------------------------------------------- Effekte / Keyframes
    // Alle fx_*-Methoden mit Undo-Snapshot; *_live-Varianten schreiben ohne
    // Snapshot (für Drag-Gesten — der Aufrufer legt zu Gestenbeginn einmal
    // `begin_fx_edit()` an, wie beim Mixer).

    pub fn clip(&self, id: &str) -> Option<&TimelineClip> {
        self.clips.iter().find(|c| c.id == id)
    }

    pub(crate) fn fx_clip_mut(&mut self, id: &str) -> Option<&mut TimelineClip> {
        let locked = locked_track_ids(&self.tracks);
        self.clips
            .iter_mut()
            .find(|c| c.id == id && !locked.contains(&c.track_id))
    }

    /// Beginn einer fx-Geste (Wert-Scrubbing, Monitor-Drag, Keyframe-Drag):
    /// legt einmalig einen Undo-Snapshot an.
    pub fn begin_fx_edit(&mut self) {
        self.push_history();
    }

    /// Wert anwenden (ohne Snapshot): animierter Parameter ⇒ Keyframe an
    /// der Medienzeit, sonst statischer Wert.
    pub fn fx_set_value_live(&mut self, id: &str, param: ParamId, media_t: f64, value: f64) {
        if let Some(clip) = self.fx_clip_mut(id) {
            let (lo, hi) = param.range();
            clip.fx.param_mut(param).set_at(media_t, value.clamp(lo, hi));
        }
    }

    pub fn fx_set_uniform_scale(&mut self, id: &str, uniform: bool) {
        let Some(clip) = self.clip(id) else { return };
        if clip.fx.uniform_scale == uniform {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(id).expect("clip nach Snapshot");
        if !uniform {
            // Y übernimmt beim Entkoppeln die aktuelle X-Kurve.
            clip.fx.scale_y = clip.fx.scale_x.clone();
        }
        clip.fx.uniform_scale = uniform;
    }

    /// Bewegung (Position/Skalierung/Rotation) der Clips zurücksetzen.
    pub fn fx_reset_motion(&mut self, ids: &[String]) {
        let locked = locked_track_ids(&self.tracks);
        let affected: Vec<String> = self
            .clips
            .iter()
            .filter(|c| {
                ids.contains(&c.id) && !locked.contains(&c.track_id) && {
                    let d = ClipFx::default();
                    c.fx.pos_x != d.pos_x
                        || c.fx.pos_y != d.pos_y
                        || c.fx.scale_x != d.scale_x
                        || c.fx.scale_y != d.scale_y
                        || c.fx.rotation != d.rotation
                        || !c.fx.uniform_scale
                }
            })
            .map(|c| c.id.clone())
            .collect();
        if affected.is_empty() {
            return;
        }
        self.push_history();
        for c in &mut self.clips {
            if affected.contains(&c.id) {
                let d = ClipFx::default();
                c.fx.pos_x = d.pos_x;
                c.fx.pos_y = d.pos_y;
                c.fx.scale_x = d.scale_x;
                c.fx.scale_y = d.scale_y;
                c.fx.rotation = d.rotation;
                c.fx.uniform_scale = true;
            }
        }
    }

    // ------------------------------------------------------- Farbkorrektur
    // Gleiches Gesten-Muster wie fx_*: `grade_update` legt einen
    // Undo-Snapshot an (Einzelklicks), `grade_update_live` schreibt ohne
    // (Drag-Gesten — der Aufrufer ruft zu Gestenbeginn `begin_fx_edit()`).

    /// Farbkorrektur ändern (mit Undo-Snapshot).
    pub fn grade_update(&mut self, id: &str, f: impl FnOnce(&mut ColorGrade)) {
        if self.fx_clip_mut(id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(id).expect("clip nach Snapshot");
        f(&mut clip.grade);
    }

    /// Farbkorrektur ändern OHNE Snapshot (laufende Geste).
    pub fn grade_update_live(&mut self, id: &str, f: impl FnOnce(&mut ColorGrade)) {
        if let Some(clip) = self.fx_clip_mut(id) {
            f(&mut clip.grade);
        }
    }

    /// Farbkorrektur der Clips vollständig zurücksetzen.
    pub fn grade_reset(&mut self, ids: &[String]) {
        let locked = locked_track_ids(&self.tracks);
        let affected: Vec<String> = self
            .clips
            .iter()
            .filter(|c| {
                ids.contains(&c.id) && !locked.contains(&c.track_id) && !c.grade.is_default()
            })
            .map(|c| c.id.clone())
            .collect();
        if affected.is_empty() {
            return;
        }
        self.push_history();
        for c in &mut self.clips {
            if affected.contains(&c.id) {
                c.grade = ColorGrade::default();
            }
        }
    }

    /// Bypass der Farbkorrektur umschalten (Werte bleiben erhalten).
    pub fn grade_toggle_enabled(&mut self, id: &str) {
        let Some(clip) = self.clip(id) else { return };
        let next = !clip.grade.enabled;
        self.grade_update(id, |g| g.enabled = next);
    }

    /// Einzelnen Parameter auf den Standardwert zurücksetzen.
    pub fn fx_reset_param(&mut self, id: &str, param: ParamId) {
        let Some(clip) = self.clip(id) else { return };
        let p = clip.fx.param(param);
        if !p.is_animated() && p.value == param.default_value() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(id).expect("clip nach Snapshot");
        *clip.fx.param_mut(param) = crate::core::animation::AnimatedParam::fixed(param.default_value());
    }

    // ------------------------------------------- Generische Keyframe-Ops
    // `ParamRef` adressiert eingebaute Parameter (Bewegung/Deckkraft/
    // Lautstärke) UND Effekt-Parameter einheitlich; das Panel
    // Effekteinstellungen läuft komplett über diese kf_*-Methoden.

    /// (min, max, Default) eines Parameters — Effekt-Parameter aus der Spec.
    pub fn param_bounds(clip: &TimelineClip, pref: &ParamRef) -> (f64, f64, f64) {
        match pref {
            ParamRef::Builtin(id) => {
                let (lo, hi) = id.range();
                (lo, hi, id.default_value())
            }
            ParamRef::Effect { fx_id, index } => clip
                .effects
                .iter()
                .find(|e| &e.id == fx_id)
                .and_then(|e| e.kind.specs().get(*index))
                .map(|s| (s.min, s.max, s.default))
                .unwrap_or((f64::NEG_INFINITY, f64::INFINITY, 0.0)),
        }
    }

    /// Parameter eines Clips auflösen (lesend).
    pub fn clip_param<'a>(clip: &'a TimelineClip, pref: &ParamRef) -> Option<&'a AnimatedParam> {
        match pref {
            ParamRef::Builtin(id) => Some(clip.fx.param(*id)),
            ParamRef::Effect { fx_id, index } => clip
                .effects
                .iter()
                .find(|e| &e.id == fx_id)
                .and_then(|e| e.params.get(*index)),
        }
    }

    fn clip_param_mut<'a>(
        clip: &'a mut TimelineClip,
        pref: &ParamRef,
    ) -> Option<&'a mut AnimatedParam> {
        match pref {
            ParamRef::Builtin(id) => Some(clip.fx.param_mut(*id)),
            ParamRef::Effect { fx_id, index } => clip
                .effects
                .iter_mut()
                .find(|e| &e.id == fx_id)
                .and_then(|e| e.params.get_mut(*index)),
        }
    }

    /// Wert anwenden (ohne Snapshot — laufende Geste nach `begin_fx_edit`).
    pub fn kf_set_value_live(&mut self, id: &str, pref: &ParamRef, media_t: f64, value: f64) {
        let Some(clip) = self.fx_clip_mut(id) else { return };
        let (lo, hi, _) = Self::param_bounds(clip, pref);
        if let Some(p) = Self::clip_param_mut(clip, pref) {
            p.set_at(media_t, value.clamp(lo, hi));
        }
    }

    /// Stopwatch umschalten: an ⇒ erster Keyframe am Playhead; aus ⇒ Kurve
    /// verwerfen, aktuellen Wert einfrieren.
    pub fn kf_toggle_animated(&mut self, id: &str, pref: &ParamRef, media_t: f64) {
        if self
            .fx_clip_mut(id)
            .and_then(|c| Self::clip_param_mut(c, pref))
            .is_none()
        {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(id).expect("clip nach Snapshot");
        let p = Self::clip_param_mut(clip, pref).expect("param nach Snapshot");
        if p.is_animated() {
            p.clear_animation(media_t);
        } else {
            p.enable_animation(media_t);
        }
    }

    /// Keyframe am Playhead setzen bzw. entfernen (Raute-Button).
    pub fn kf_toggle_keyframe(&mut self, id: &str, pref: &ParamRef, media_t: f64) {
        let Some(p) = self.clip(id).and_then(|c| Self::clip_param(c, pref)) else {
            return;
        };
        let value = p.eval(media_t);
        let exists = p.key_index_at(media_t).is_some();
        if self.fx_clip_mut(id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(id).expect("clip nach Snapshot");
        let Some(p) = Self::clip_param_mut(clip, pref) else { return };
        if exists {
            p.remove_key_at(media_t);
        } else {
            p.upsert_key(media_t, value);
        }
    }

    /// Keyframes zu gegebenen Medienzeiten entfernen (Keyframe-Editor).
    pub fn kf_remove_keyframes(&mut self, id: &str, keys: &[(ParamRef, f64)]) {
        if keys.is_empty() || self.fx_clip_mut(id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(id).expect("clip nach Snapshot");
        for (pref, t) in keys {
            if let Some(p) = Self::clip_param_mut(clip, pref) {
                p.remove_key_at(*t);
            }
        }
    }

    /// Kurve eines Parameters ersetzen (ohne Snapshot — Keyframe-Drag).
    pub fn kf_replace_keys_live(&mut self, id: &str, pref: &ParamRef, keys: Vec<Keyframe>) {
        if let Some(clip) = self.fx_clip_mut(id) {
            if let Some(p) = Self::clip_param_mut(clip, pref) {
                p.replace_keys(keys);
            }
        }
    }

    /// Interpolation der Keyframes (Parameter, Medienzeit) setzen.
    pub fn kf_set_interp(&mut self, id: &str, keys: &[(ParamRef, f64)], interp: Interp) {
        if keys.is_empty() || self.fx_clip_mut(id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(id).expect("clip nach Snapshot");
        for (pref, t) in keys {
            if let Some(p) = Self::clip_param_mut(clip, pref) {
                if let Some(i) = p.key_index_at(*t) {
                    p.keyframes[i].interp = interp;
                }
            }
        }
    }

    /// Einzelnen Parameter auf den Spec-/Standardwert zurücksetzen.
    pub fn kf_reset_param(&mut self, id: &str, pref: &ParamRef) {
        let Some(clip) = self.clip(id) else { return };
        let (_, _, default) = Self::param_bounds(clip, pref);
        let Some(p) = Self::clip_param(clip, pref) else { return };
        if !p.is_animated() && p.value == default {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(id).expect("clip nach Snapshot");
        if let Some(p) = Self::clip_param_mut(clip, pref) {
            *p = AnimatedParam::fixed(default);
        }
    }

    // ------------------------------------------------------- Effekt-Stapel

    /// Effekt ans Ende des Stapels hängen. Audio-Effekte landen auf dem
    /// Audio-Clip (bei verknüpften Paaren ggf. dem Partner), Video-Effekte
    /// auf dem Video-Clip. Liefert die Ziel-Clip-ID bei Erfolg.
    pub fn effects_add(&mut self, clip_id: &str, kind: EffectKind) -> Option<String> {
        let target = self.effect_target_clip(clip_id, kind)?;
        self.push_history();
        let clip = self.fx_clip_mut(&target).expect("clip nach Snapshot");
        clip.effects.push(EffectInstance::new(kind));
        Some(target)
    }

    /// Passenden Clip für die Effektart finden: der Clip selbst oder sein
    /// verknüpfter A/V-Partner; None, wenn die Art nicht passt (z. B.
    /// Video-Effekt auf reinem Audio-Clip) oder die Spur gesperrt ist.
    pub fn effect_target_clip(&self, clip_id: &str, kind: EffectKind) -> Option<String> {
        let locked = locked_track_ids(&self.tracks);
        let clip = self.clip(clip_id)?;
        let want = if kind.is_audio() {
            TrackKind::Audio
        } else {
            TrackKind::Video
        };
        if clip.kind == want {
            return (!locked.contains(&clip.track_id)).then(|| clip.id.clone());
        }
        let link = clip.link_id.as_ref()?;
        self.clips
            .iter()
            .find(|c| {
                c.id != clip.id
                    && c.link_id.as_deref() == Some(link)
                    && c.kind == want
                    && !locked.contains(&c.track_id)
            })
            .map(|c| c.id.clone())
    }

    pub fn effects_remove(&mut self, clip_id: &str, fx_id: &str) {
        let exists = self
            .clip(clip_id)
            .is_some_and(|c| c.effects.iter().any(|e| e.id == fx_id));
        if !exists || self.fx_clip_mut(clip_id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(clip_id).expect("clip nach Snapshot");
        clip.effects.retain(|e| e.id != fx_id);
    }

    /// Effekt im Stapel verschieben (delta −1 = nach oben, +1 = nach unten).
    pub fn effects_move(&mut self, clip_id: &str, fx_id: &str, delta: i32) {
        let Some(clip) = self.clip(clip_id) else { return };
        let Some(idx) = clip.effects.iter().position(|e| e.id == fx_id) else {
            return;
        };
        let new_idx = idx as i32 + delta;
        if new_idx < 0 || new_idx >= clip.effects.len() as i32 {
            return;
        }
        if self.fx_clip_mut(clip_id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(clip_id).expect("clip nach Snapshot");
        clip.effects.swap(idx, new_idx as usize);
    }

    /// Effekt an eine Zielposition im Stapel ziehen (Drag-to-Reorder): `dest`
    /// = gewünschter Index NACH dem Entfernen (geklemmt). No-op ohne Bewegung.
    pub fn effects_reorder(&mut self, clip_id: &str, fx_id: &str, dest: usize) {
        let Some(clip) = self.clip(clip_id) else { return };
        let Some(cur) = clip.effects.iter().position(|e| e.id == fx_id) else {
            return;
        };
        let dest = dest.min(clip.effects.len().saturating_sub(1));
        if dest == cur || self.fx_clip_mut(clip_id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(clip_id).expect("clip nach Snapshot");
        let inst = clip.effects.remove(cur);
        clip.effects.insert(dest.min(clip.effects.len()), inst);
    }

    /// Bypass eines Effekts umschalten (Werte bleiben erhalten).
    pub fn effects_toggle_enabled(&mut self, clip_id: &str, fx_id: &str) {
        let exists = self
            .clip(clip_id)
            .is_some_and(|c| c.effects.iter().any(|e| e.id == fx_id));
        if !exists || self.fx_clip_mut(clip_id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(clip_id).expect("clip nach Snapshot");
        if let Some(e) = clip.effects.iter_mut().find(|e| e.id == fx_id) {
            e.enabled = !e.enabled;
        }
    }

    /// Alle Parameter eines Effekts auf Defaults zurücksetzen.
    pub fn effects_reset(&mut self, clip_id: &str, fx_id: &str) {
        let exists = self
            .clip(clip_id)
            .is_some_and(|c| c.effects.iter().any(|e| e.id == fx_id));
        if !exists || self.fx_clip_mut(clip_id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(clip_id).expect("clip nach Snapshot");
        if let Some(e) = clip.effects.iter_mut().find(|e| e.id == fx_id) {
            e.reset();
        }
    }

    // -------------------------------------------------- Effekt-Masken

    /// Findet eine Effekt-Instanz eines (entsperrten) Clips mutabel.
    fn effect_mut<'a>(
        clips: &'a mut [TimelineClip],
        locked: &std::collections::HashSet<String>,
        clip_id: &str,
        fx_id: &str,
    ) -> Option<&'a mut crate::core::effects::EffectInstance> {
        clips
            .iter_mut()
            .find(|c| c.id == clip_id && !locked.contains(&c.track_id))?
            .effects
            .iter_mut()
            .find(|e| e.id == fx_id)
    }

    /// Neue Maske an einen Effekt hängen; gibt die neue Masken-ID zurück.
    pub fn mask_add(
        &mut self,
        clip_id: &str,
        fx_id: &str,
        shape: crate::core::mask::MaskShape,
    ) -> Option<String> {
        let exists = self
            .clip(clip_id)
            .is_some_and(|c| c.effects.iter().any(|e| e.id == fx_id));
        if !exists {
            return None;
        }
        let locked = locked_track_ids(&self.tracks);
        // Auf MAX_MASKS deckeln (GPU-Uniform-Array-Grenze ⇒ Vorschau == Export).
        let at_cap = Self::effect_mut(&mut self.clips, &locked, clip_id, fx_id)
            .map(|e| e.masks.len() >= crate::core::mask::MAX_MASKS)
            .unwrap_or(true);
        if at_cap {
            return None;
        }
        self.push_history();
        let locked = locked_track_ids(&self.tracks);
        let m = crate::core::mask::Mask::new(shape);
        let id = m.id.clone();
        Self::effect_mut(&mut self.clips, &locked, clip_id, fx_id)?
            .masks
            .push(m);
        Some(id)
    }

    /// Maske von einem Effekt entfernen.
    pub fn mask_remove(&mut self, clip_id: &str, fx_id: &str, mask_id: &str) {
        let locked = locked_track_ids(&self.tracks);
        let present = Self::effect_mut(&mut self.clips, &locked, clip_id, fx_id)
            .is_some_and(|e| e.masks.iter().any(|m| m.id == mask_id));
        if !present {
            return;
        }
        self.push_history();
        let locked = locked_track_ids(&self.tracks);
        if let Some(e) = Self::effect_mut(&mut self.clips, &locked, clip_id, fx_id) {
            e.masks.retain(|m| m.id != mask_id);
        }
    }

    /// Maske mit Undo-Snapshot bearbeiten (Panel-Schalter/Slider).
    pub fn mask_update(
        &mut self,
        clip_id: &str,
        fx_id: &str,
        mask_id: &str,
        f: impl FnOnce(&mut crate::core::mask::Mask),
    ) {
        let locked = locked_track_ids(&self.tracks);
        let present = Self::effect_mut(&mut self.clips, &locked, clip_id, fx_id)
            .is_some_and(|e| e.masks.iter().any(|m| m.id == mask_id));
        if !present {
            return;
        }
        self.push_history();
        self.mask_update_live(clip_id, fx_id, mask_id, f);
    }

    /// Maske OHNE Snapshot bearbeiten (laufende Drag-Geste; der Aufrufer ruft
    /// zu Gestenbeginn `begin_fx_edit()`).
    pub fn mask_update_live(
        &mut self,
        clip_id: &str,
        fx_id: &str,
        mask_id: &str,
        f: impl FnOnce(&mut crate::core::mask::Mask),
    ) {
        let locked = locked_track_ids(&self.tracks);
        if let Some(e) = Self::effect_mut(&mut self.clips, &locked, clip_id, fx_id) {
            if let Some(m) = e.masks.iter_mut().find(|m| m.id == mask_id) {
                f(m);
            }
        }
    }

    /// Invertierung einer Maske umschalten (mit Snapshot).
    pub fn mask_toggle_invert(&mut self, clip_id: &str, fx_id: &str, mask_id: &str) {
        self.mask_update(clip_id, fx_id, mask_id, |m| m.inverted = !m.inverted);
    }

    /// Maske bypassen/aktivieren (mit Snapshot).
    pub fn mask_toggle_enabled(&mut self, clip_id: &str, fx_id: &str, mask_id: &str) {
        self.mask_update(clip_id, fx_id, mask_id, |m| m.enabled = !m.enabled);
    }

    // -------------------------------------------------- Spur-Effekte (Bus)

    /// Index einer editierbaren (entsperrten) Audio-Spur.
    fn track_audio_idx(&self, track_id: &str) -> Option<usize> {
        self.tracks
            .iter()
            .position(|t| t.id == track_id && t.kind == TrackKind::Audio && !t.locked)
    }

    fn track_idx(&self, track_id: &str) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == track_id)
    }

    /// Audio-Effekt an die Bus-Kette der Spur anhängen.
    pub fn track_effects_add(&mut self, track_id: &str, kind: EffectKind) -> bool {
        if !kind.is_audio() || self.track_audio_idx(track_id).is_none() {
            return false;
        }
        self.push_history();
        let i = self.track_audio_idx(track_id).expect("Spur nach Snapshot");
        self.tracks[i].effects.push(EffectInstance::new(kind));
        true
    }

    pub fn track_effects_remove(&mut self, track_id: &str, fx_id: &str) {
        let Some(i) = self.track_audio_idx(track_id) else {
            return;
        };
        if !self.tracks[i].effects.iter().any(|e| e.id == fx_id) {
            return;
        }
        self.push_history();
        self.tracks[i].effects.retain(|e| e.id != fx_id);
    }

    pub fn track_effects_move(&mut self, track_id: &str, fx_id: &str, delta: i32) {
        let Some(i) = self.track_audio_idx(track_id) else {
            return;
        };
        let Some(idx) = self.tracks[i].effects.iter().position(|e| e.id == fx_id) else {
            return;
        };
        let new_idx = idx as i32 + delta;
        if new_idx < 0 || new_idx >= self.tracks[i].effects.len() as i32 {
            return;
        }
        self.push_history();
        self.tracks[i].effects.swap(idx, new_idx as usize);
    }

    pub fn track_effects_toggle_enabled(&mut self, track_id: &str, fx_id: &str) {
        let Some(i) = self.track_audio_idx(track_id) else {
            return;
        };
        if !self.tracks[i].effects.iter().any(|e| e.id == fx_id) {
            return;
        }
        self.push_history();
        if let Some(e) = self.tracks[i].effects.iter_mut().find(|e| e.id == fx_id) {
            e.enabled = !e.enabled;
        }
    }

    pub fn track_effects_reset(&mut self, track_id: &str, fx_id: &str) {
        let Some(i) = self.track_audio_idx(track_id) else {
            return;
        };
        if !self.tracks[i].effects.iter().any(|e| e.id == fx_id) {
            return;
        }
        self.push_history();
        if let Some(e) = self.tracks[i].effects.iter_mut().find(|e| e.id == fx_id) {
            e.reset();
        }
    }

    // --------------------------------------------------- Spur-Automation
    // Lautstärke/Pan als Keyframe-Kurven über die SEQUENZZEIT (anders als
    // Clip-Keyframes, die in Medienzeit kleben). Diskrete Punkt-Operationen
    // legen einen Undo-Snapshot an; Drag-Gesten nutzen `begin_mix_edit` +
    // die `*_live`-Varianten (ein Snapshot pro Geste).

    pub fn track_auto_add_point(
        &mut self,
        track_id: &str,
        param: TrackAutoParam,
        t: f64,
        value: f64,
    ) {
        let Some(i) = self.track_idx(track_id) else {
            return;
        };
        self.push_history();
        self.tracks[i].auto_param_mut(param).upsert_key(t.max(0.0), value);
    }

    pub fn track_auto_remove_point(&mut self, track_id: &str, param: TrackAutoParam, t: f64) {
        let Some(i) = self.track_idx(track_id) else {
            return;
        };
        if self.tracks[i].auto_param(param).key_index_at(t).is_none() {
            return;
        }
        self.push_history();
        self.tracks[i].auto_param_mut(param).remove_key_at(t);
    }

    pub fn track_auto_clear(&mut self, track_id: &str, param: TrackAutoParam) {
        let Some(i) = self.track_idx(track_id) else {
            return;
        };
        if !self.tracks[i].auto_param(param).is_animated() {
            return;
        }
        self.push_history();
        self.tracks[i].auto_param_mut(param).keyframes.clear();
    }

    /// Lautstärke- und Pan-Automation der anvisierten Audio-Spuren löschen
    /// (ohne Targeting: aller Audio-Spuren) — ein Undo-Schritt. Command-Ziel.
    pub fn clear_track_automation_targeted(&mut self) {
        let any_targeted = self
            .tracks
            .iter()
            .any(|t| t.kind == TrackKind::Audio && t.targeted);
        let idxs: Vec<usize> = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.kind == TrackKind::Audio
                    && (!any_targeted || t.targeted)
                    && t.has_automation()
            })
            .map(|(i, _)| i)
            .collect();
        if idxs.is_empty() {
            return;
        }
        self.push_history();
        for i in idxs {
            self.tracks[i].volume_auto.keyframes.clear();
            self.tracks[i].pan_auto.keyframes.clear();
        }
    }

    /// Ganze Kurve ohne History ersetzen (Punkt-Drag in Zeit + Wert).
    pub fn track_auto_replace_live(
        &mut self,
        track_id: &str,
        param: TrackAutoParam,
        keys: Vec<Keyframe>,
    ) {
        let Some(i) = self.track_idx(track_id) else {
            return;
        };
        self.tracks[i].auto_param_mut(param).replace_keys(keys);
    }

    /// Alle Effekte der Clips entfernen.
    pub fn effects_clear(&mut self, ids: &[String]) {
        let locked = locked_track_ids(&self.tracks);
        let affected: Vec<String> = self
            .clips
            .iter()
            .filter(|c| {
                ids.contains(&c.id) && !locked.contains(&c.track_id) && !c.effects.is_empty()
            })
            .map(|c| c.id.clone())
            .collect();
        if affected.is_empty() {
            return;
        }
        self.push_history();
        for c in &mut self.clips {
            if affected.contains(&c.id) {
                c.effects.clear();
            }
        }
    }

    /// Bypass ALLER Effekte der Clips umschalten: ist irgendeiner aktiv,
    /// werden alle deaktiviert, sonst alle aktiviert.
    pub fn effects_toggle_bypass(&mut self, ids: &[String]) {
        let locked = locked_track_ids(&self.tracks);
        let affected: Vec<String> = self
            .clips
            .iter()
            .filter(|c| {
                ids.contains(&c.id) && !locked.contains(&c.track_id) && !c.effects.is_empty()
            })
            .map(|c| c.id.clone())
            .collect();
        if affected.is_empty() {
            return;
        }
        let any_enabled = self
            .clips
            .iter()
            .filter(|c| affected.contains(&c.id))
            .any(|c| c.effects.iter().any(|e| e.enabled));
        self.push_history();
        for c in &mut self.clips {
            if affected.contains(&c.id) {
                for e in &mut c.effects {
                    e.enabled = !any_enabled;
                }
            }
        }
    }

}
