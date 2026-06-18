//! impl-TimelineStore-Methoden (aus timeline.rs zerlegt).
use super::*;

impl TimelineStore {
    // ------------------------------------------------ Geschwindigkeit/Dauer

    /// „Geschwindigkeit/Dauer“ (Premiere Mod+R) auf die Clips anwenden:
    /// die belegte Medienspanne bleibt erhalten, die Dauer folgt aus
    /// duration = span / speed. `ripple` verschiebt nachfolgende Clips
    /// (aller ungesperrten Spuren) um die Längenänderung; ohne Ripple wird
    /// die neue Dauer am nächsten Clip der Spur gekappt (Premiere-Verhalten).
    /// Verknüpfte A/V-Partner ändern sich gemeinsam (ein Ripple je Paar).
    pub fn set_clip_speed(
        &mut self,
        ids: &[String],
        speed: f64,
        reverse: bool,
        freeze: bool,
        ripple: bool,
    ) {
        use std::collections::HashSet;
        let locked = locked_track_ids(&self.tracks);
        let speed = if speed.is_finite() {
            speed.clamp(MIN_CLIP_SPEED, MAX_CLIP_SPEED)
        } else {
            1.0
        };
        let expanded: HashSet<String> = expand_links(&self.clips, ids).into_iter().collect();
        let targets: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| {
                expanded.contains(&c.id) && !locked.contains(&c.track_id) && !c.is_generator()
            })
            .cloned()
            .collect();
        if targets.is_empty() {
            return;
        }
        let unchanged = targets.iter().all(|c| {
            // Animierte Clips immer flachklopfen (eine Time-Remap-Kurve mit dem
            // mittleren Tempo == eingestelltem Wert gilt nicht als unverändert).
            !c.speed.is_animated()
                && (c.eff_speed() - speed).abs() < EPS
                && c.reverse == reverse
                && c.freeze == freeze
        });
        if unchanged {
            return;
        }
        self.push_history();

        // Neue Dauer je Clip: Medienspanne erhalten (duration = span/speed).
        // Standbild: Dauer frei; Auftauen behält die Dauer, kappt die
        // Spanne aber an der Quelle.
        let new_duration = |c: &TimelineClip| -> f64 {
            if freeze {
                return c.duration;
            }
            let span = if c.freeze {
                c.duration * speed
            } else {
                c.media_span()
            };
            let span = if c.src_duration.is_finite() {
                span.min((c.src_duration - c.src_in).max(0.0))
            } else {
                span
            };
            (span / speed).max(MIN_CLIP_DURATION)
        };

        // Längenänderung je Verknüpfungsgruppe einsammeln (Ripple zählt
        // pro Schnittpunkt einmal, nicht je A/V-Hälfte).
        struct EditShift {
            old_end: f64,
            delta: f64,
            group: Vec<String>,
        }
        let mut shifts: Vec<EditShift> = Vec::new();
        let mut grouped: HashSet<&str> = HashSet::new();
        for c in &targets {
            if grouped.contains(c.id.as_str()) {
                continue;
            }
            let group: Vec<&TimelineClip> = match &c.link_id {
                Some(link) => targets
                    .iter()
                    .filter(|t| t.link_id.as_deref() == Some(link))
                    .collect(),
                None => vec![c],
            };
            for g in &group {
                grouped.insert(g.id.as_str());
            }
            let old_end = group.iter().map(|g| g.end()).fold(0.0, f64::max);
            let delta = group
                .iter()
                .map(|g| new_duration(g) - g.duration)
                .fold(f64::NEG_INFINITY, f64::max);
            shifts.push(EditShift {
                old_end,
                delta,
                group: group.iter().map(|g| g.id.clone()).collect(),
            });
        }

        let target_ids: HashSet<&str> = targets.iter().map(|c| c.id.as_str()).collect();
        // Ohne Ripple: Dauer am nächsten Clip derselben Spur kappen.
        let next_start = |c: &TimelineClip| -> f64 {
            self.clips
                .iter()
                .filter(|o| {
                    o.track_id == c.track_id
                        && !target_ids.contains(o.id.as_str())
                        && o.start >= c.end() - EPS
                })
                .map(|o| o.start)
                .fold(f64::INFINITY, f64::min)
        };
        let caps: std::collections::HashMap<String, f64> = if ripple {
            Default::default()
        } else {
            targets
                .iter()
                .map(|c| (c.id.clone(), (next_start(c) - c.start).max(MIN_CLIP_DURATION)))
                .collect()
        };

        for c in &mut self.clips {
            if !target_ids.contains(c.id.as_str()) {
                continue;
            }
            let mut dur = new_duration(c);
            if let Some(cap) = caps.get(&c.id) {
                dur = dur.min(*cap);
            }
            c.duration = dur;
            // Konstantes Tempo aus dem Dialog ersetzt eine etwaige Time-Remap-
            // Kurve durch einen festen Wert.
            c.speed = AnimatedParam::fixed(speed);
            c.reverse = reverse && !freeze;
            c.freeze = freeze;
        }

        if ripple {
            // Verschiebungen kumulativ anwenden: jeder Clip rückt um die
            // Summe der Deltas aller Schnittpunkte vor seinem Start.
            shifts.sort_by(|a, b| a.old_end.total_cmp(&b.old_end));
            let moves: Vec<(String, f64)> = self
                .clips
                .iter()
                .filter(|c| !locked.contains(&c.track_id))
                .filter_map(|c| {
                    let shift: f64 = shifts
                        .iter()
                        .filter(|s| {
                            s.old_end <= c.start + EPS && !s.group.contains(&c.id)
                        })
                        .map(|s| s.delta)
                        .sum();
                    (shift.abs() > EPS).then(|| (c.id.clone(), shift))
                })
                .collect();
            for (id, shift) in moves {
                if let Some(c) = self.clips.iter_mut().find(|c| c.id == id) {
                    c.start = (c.start + shift).max(0.0);
                }
            }
        }
        self.reconcile_transitions();
    }

    /// „Frame einfrieren“: Standbild aus der Medienzeit am Playhead (liegt
    /// der Playhead außerhalb des Clips, friert der In-Punkt ein).
    pub fn freeze_frame_at_playhead(&mut self, ids: &[String]) {
        use std::collections::HashSet;
        let locked = locked_track_ids(&self.tracks);
        let t = self.playhead_sec;
        let expanded: HashSet<String> = expand_links(&self.clips, ids).into_iter().collect();
        let affected: Vec<String> = self
            .clips
            .iter()
            .filter(|c| {
                expanded.contains(&c.id)
                    && !locked.contains(&c.track_id)
                    && !c.is_generator()
                    && !c.freeze
            })
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
            if t >= c.start - EPS && t < c.end() + EPS {
                let m = c.media_time_at(t.clamp(c.start, c.end()));
                c.src_in = m.clamp(0.0, c.src_duration);
            }
            c.freeze = true;
            c.reverse = false;
        }
    }

    // ----------------------------------------------------------------- Titel

    /// Titel-Clip bei `at` anlegen: auf der nächsten freien Videospur ÜBER
    /// dem obersten belegten Material (Premiere-Konvention); ist keine frei,
    /// entsteht eine neue Spur oben. Liefert die Clip-ID.
    pub fn add_title_clip(&mut self, spec: TitleSpec, at: f64, duration: f64) -> String {
        let at = at.max(0.0);
        let duration = duration.max(MIN_CLIP_DURATION);
        let end = at + duration;
        self.push_history();

        let locked = locked_track_ids(&self.tracks);
        let occupied = |track_id: &str| -> bool {
            self.clips
                .iter()
                .any(|c| c.track_id == track_id && c.start < end - EPS && c.end() > at + EPS)
        };
        // Videospuren in Zeichenreihenfolge oben → unten (Index 0 = oberste).
        let video: Vec<(usize, String)> = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == TrackKind::Video)
            .map(|(i, t)| (i, t.id.clone()))
            .collect();
        // Oberste belegte Spur; Kandidaten sind freie Spuren darüber —
        // die dichteste gewinnt. Ohne Belegung: unterste freie Spur.
        let top_occupied = video.iter().position(|(_, id)| occupied(id));
        let candidates: Vec<&(usize, String)> = match top_occupied {
            Some(limit) => video.iter().take(limit).rev().collect(),
            None => video.iter().rev().collect(),
        };
        let track_id = candidates
            .into_iter()
            .find(|(_, id)| !locked.contains(id) && !occupied(id))
            .map(|(_, id)| id.clone())
            .unwrap_or_else(|| {
                let track = make_track(TrackKind::Video);
                let id = track.id.clone();
                let at = self.video_block_start();
                self.tracks.insert(at, track);
                id
            });

        let clip = TimelineClip {
            extra: Default::default(),
            id: new_id(),
            track_id,
            asset_id: String::new(),
            name: spec.display_name(),
            kind: TrackKind::Video,
            start: at,
            duration,
            src_in: 0.0,
            src_duration: f64::INFINITY,
            link_id: None,
            enabled: true,
            gain_db: 0.0,
            fx: ClipFx::default(),
            grade: ColorGrade::default(),
            effects: Vec::new(),
            title: Some(spec),
            subtitle: None,
            adjustment: None,
            speed: crate::core::animation::AnimatedParam::fixed(1.0),
            reverse: false,
            freeze: false,
            markers: Vec::new(),
            nest_seq: None,
            multicam: None,
            blend_mode: crate::core::compose::BlendMode::default(),
        };
        let id = clip.id.clone();
        self.selected_clip_ids = vec![id.clone()];
        self.selected_transition_ids.clear();
        self.clips.push(clip);
        id
    }

    // -------------------------------------------------- Adjustment Layer

    /// Adjustment Layer (Einstellungsebene) bei `at` über der Dauer `duration`
    /// anlegen — auf der nächsten freien Videospur ÜBER dem obersten belegten
    /// Material (wie Titel), damit sein Korrektur-Pass auf alles Darunterliegende
    /// wirkt. Trägt anfangs ein neutrales Grade/keine Effekte (der Nutzer
    /// gradet ihn anschließend im Farbe-/Effekte-Panel). Liefert die Clip-ID.
    pub fn add_adjustment_clip(&mut self, at: f64, duration: f64) -> String {
        let at = at.max(0.0);
        let duration = duration.max(MIN_CLIP_DURATION);
        let end = at + duration;
        self.push_history();

        let locked = locked_track_ids(&self.tracks);
        let occupied = |track_id: &str| -> bool {
            self.clips
                .iter()
                .any(|c| c.track_id == track_id && c.start < end - EPS && c.end() > at + EPS)
        };
        let video: Vec<(usize, String)> = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == TrackKind::Video)
            .map(|(i, t)| (i, t.id.clone()))
            .collect();
        let top_occupied = video.iter().position(|(_, id)| occupied(id));
        let candidates: Vec<&(usize, String)> = match top_occupied {
            Some(limit) => video.iter().take(limit).rev().collect(),
            None => video.iter().rev().collect(),
        };
        let track_id = candidates
            .into_iter()
            .find(|(_, id)| !locked.contains(id) && !occupied(id))
            .map(|(_, id)| id.clone())
            .unwrap_or_else(|| {
                let track = make_track(TrackKind::Video);
                let id = track.id.clone();
                let at = self.video_block_start();
                self.tracks.insert(at, track);
                id
            });

        let clip = TimelineClip {
            extra: Default::default(),
            id: new_id(),
            track_id,
            asset_id: String::new(),
            name: crate::core::adjustment::AdjustmentSpec::display_name(),
            kind: TrackKind::Video,
            start: at,
            duration,
            src_in: 0.0,
            src_duration: f64::INFINITY,
            link_id: None,
            enabled: true,
            gain_db: 0.0,
            fx: ClipFx::default(),
            grade: ColorGrade::default(),
            effects: Vec::new(),
            title: None,
            subtitle: None,
            adjustment: Some(AdjustmentSpec::new()),
            speed: crate::core::animation::AnimatedParam::fixed(1.0),
            reverse: false,
            freeze: false,
            markers: Vec::new(),
            nest_seq: None,
            multicam: None,
            blend_mode: crate::core::compose::BlendMode::default(),
        };
        let id = clip.id.clone();
        self.selected_clip_ids = vec![id.clone()];
        self.selected_transition_ids.clear();
        self.clips.push(clip);
        id
    }

    /// Beginn einer Titel-Bearbeitungsgeste (Slider-Drag, Tipp-Sitzung):
    /// legt einmalig einen Undo-Snapshot an (Muster wie `begin_fx_edit`).
    pub fn begin_title_edit(&mut self) {
        self.push_history();
    }

    /// Titel-Spec ändern (mit Undo-Snapshot) — für Einzelklicks.
    pub fn title_update(&mut self, id: &str, f: impl FnOnce(&mut TitleSpec)) {
        if self
            .fx_clip_mut(id)
            .map(|c| c.title.is_none())
            .unwrap_or(true)
        {
            return;
        }
        self.push_history();
        self.title_update_live(id, f);
    }

    /// Titel-Spec ändern OHNE Snapshot (laufende Geste nach
    /// `begin_title_edit`). Hält den Clipnamen mit dem Text synchron.
    pub fn title_update_live(&mut self, id: &str, f: impl FnOnce(&mut TitleSpec)) {
        let Some(clip) = self.fx_clip_mut(id) else { return };
        let Some(spec) = clip.title.as_mut() else { return };
        f(spec);
        clip.name = spec.display_name();
        self.revision += 1;
    }

    // ------------------------------------------------------------ Untertitel

    /// Untertitel-Spuren in Zeichenreihenfolge der Timeline (oben → unten).
    pub fn subtitle_tracks(&self) -> Vec<&TimelineTrack> {
        self.tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Subtitle)
            .collect()
    }

    /// Aktive Untertitel-Spur: explizit gewählt, sonst U1 (unterste).
    pub fn active_subtitle_track(&self) -> Option<&TimelineTrack> {
        self.active_subtitle_track_id
            .as_ref()
            .and_then(|id| {
                self.tracks
                    .iter()
                    .find(|t| t.id == *id && t.kind == TrackKind::Subtitle)
            })
            .or_else(|| {
                self.tracks
                    .iter().rfind(|t| t.kind == TrackKind::Subtitle)
            })
    }

    pub fn set_active_subtitle_track(&mut self, track_id: &str) {
        if self
            .tracks
            .iter()
            .any(|t| t.id == track_id && t.kind == TrackKind::Subtitle)
        {
            self.active_subtitle_track_id = Some(track_id.to_string());
        }
    }

    /// Stil einer Untertitel-Spur (Standardstil, solange keiner gesetzt ist).
    pub fn subtitle_style(&self, track_id: &str) -> SubtitleStyle {
        self.tracks
            .iter()
            .find(|t| t.id == track_id)
            .and_then(|t| t.subtitle_style.clone())
            .unwrap_or_default()
    }

    /// Beginn einer Untertitel-Bearbeitungsgeste (Slider-Drag, Tipp-
    /// Sitzung): legt einmalig einen Undo-Snapshot an (wie `begin_title_edit`).
    pub fn begin_subtitle_edit(&mut self) {
        self.push_history();
    }

    /// Spurstil ändern (mit Undo-Snapshot) — für Einzelklicks.
    pub fn subtitle_style_update(&mut self, track_id: &str, f: impl FnOnce(&mut SubtitleStyle)) {
        if !self
            .tracks
            .iter()
            .any(|t| t.id == track_id && t.kind == TrackKind::Subtitle)
        {
            return;
        }
        self.push_history();
        self.subtitle_style_update_live(track_id, f);
    }

    /// Spurstil ändern OHNE Snapshot (laufende Geste nach `begin_subtitle_edit`).
    pub fn subtitle_style_update_live(
        &mut self,
        track_id: &str,
        f: impl FnOnce(&mut SubtitleStyle),
    ) {
        let Some(track) = self
            .tracks
            .iter_mut()
            .find(|t| t.id == track_id && t.kind == TrackKind::Subtitle)
        else {
            return;
        };
        let mut style = track.subtitle_style.clone().unwrap_or_default();
        f(&mut style);
        track.subtitle_style = Some(style);
        self.revision += 1;
    }

    /// Segment-Text ändern OHNE Snapshot (Tipp-Sitzung nach
    /// `begin_subtitle_edit` — Muster wie `title_update_live`). Hält den
    /// Clipnamen synchron.
    pub fn subtitle_update_live(&mut self, id: &str, f: impl FnOnce(&mut SubtitleSpec)) {
        let Some(clip) = self.fx_clip_mut(id) else { return };
        let Some(spec) = clip.subtitle.as_mut() else { return };
        f(spec);
        clip.name = spec.display_name();
        self.revision += 1;
    }

    fn make_subtitle_clip(track_id: &str, text: &str, start: f64, duration: f64) -> TimelineClip {
        let spec = SubtitleSpec::new(text);
        TimelineClip {
            extra: Default::default(),
            id: new_id(),
            track_id: track_id.to_string(),
            asset_id: String::new(),
            name: spec.display_name(),
            kind: TrackKind::Subtitle,
            start,
            duration,
            src_in: 0.0,
            src_duration: f64::INFINITY,
            link_id: None,
            enabled: true,
            gain_db: 0.0,
            fx: ClipFx::default(),
            grade: ColorGrade::default(),
            effects: Vec::new(),
            title: None,
            subtitle: Some(spec),
            adjustment: None,
            speed: crate::core::animation::AnimatedParam::fixed(1.0),
            reverse: false,
            freeze: false,
            markers: Vec::new(),
            nest_seq: None,
            multicam: None,
            blend_mode: crate::core::compose::BlendMode::default(),
        }
    }

    /// Untertitel-Segment am Playhead anlegen: auf der aktiven Spur (ohne
    /// Spur entsteht eine), frame-genau eingerastet, Dauer bis maximal zum
    /// nächsten Segment. Liefert die Clip-ID oder eine Fehlermeldung.
    pub fn add_subtitle_clip(&mut self, text: &str, at: f64) -> Result<String, String> {
        let rate = self.settings.rate;
        let start = rate.time_of_frame(rate.frame_round(at.max(0.0)).max(0) as f64);

        let locked = locked_track_ids(&self.tracks);
        let existing = self
            .active_subtitle_track()
            .map(|t| t.id.clone())
            .filter(|id| !locked.contains(id));
        if let Some(track_id) = &existing {
            let occupied = self.clips.iter().any(|c| {
                c.track_id == *track_id
                    && c.start < start + MIN_CLIP_DURATION - EPS
                    && c.end() > start + EPS
            });
            if occupied {
                return Err("An dieser Position liegt bereits ein Untertitel".to_string());
            }
        }

        self.push_history();
        let track_id = existing.unwrap_or_else(|| {
            let track = make_track(TrackKind::Subtitle);
            let id = track.id.clone();
            self.tracks.insert(0, track);
            id
        });
        self.active_subtitle_track_id = Some(track_id.clone());

        // Dauer: Standard, aber nie ins nächste Segment hinein.
        let next_start = self
            .clips
            .iter()
            .filter(|c| c.track_id == track_id && c.start > start + EPS)
            .map(|c| c.start)
            .fold(f64::INFINITY, f64::min);
        let duration = (next_start - start)
            .min(crate::core::subtitle::DEFAULT_CUE_DURATION)
            .max(MIN_CLIP_DURATION);

        let clip = Self::make_subtitle_clip(&track_id, text, start, duration);
        let id = clip.id.clone();
        self.selected_clip_ids = vec![id.clone()];
        self.selected_transition_ids.clear();
        self.clips.push(clip);
        Ok(id)
    }

    /// SRT-Cues auf eine NEUE Untertitel-Spur importieren (frame-genau aufs
    /// Sequenzraster gerundet, Überlappungen aufgelöst). Liefert die Spur-ID
    /// und die Anzahl eingefügter Segmente — ein Undo-Schritt.
    pub fn import_subtitle_cues(&mut self, cues: &[SrtCue]) -> (String, usize) {
        self.push_history();
        let track = make_track(TrackKind::Subtitle);
        let track_id = track.id.clone();
        self.tracks.insert(0, track);
        self.active_subtitle_track_id = Some(track_id.clone());

        let rate = self.settings.rate;
        let snap = |t: f64| rate.time_of_frame(rate.frame_round(t.max(0.0)).max(0) as f64);
        let mut inserted = 0usize;
        let mut prev_end = 0.0f64;
        let mut sorted: Vec<&SrtCue> = cues.iter().collect();
        sorted.sort_by(|a, b| a.start.total_cmp(&b.start));
        for cue in sorted {
            if cue.text.trim().is_empty() {
                continue;
            }
            let mut start = snap(cue.start).max(prev_end);
            let mut end = snap(cue.end);
            // Mindestens ein Frame, sonst verschluckt die Rundung den Cue.
            if end - start < MIN_CLIP_DURATION - EPS {
                end = rate.time_of_frame((rate.frame_round(start) + 1) as f64);
                start = snap(start);
            }
            if end - start < MIN_CLIP_DURATION - EPS {
                continue;
            }
            self.clips
                .push(Self::make_subtitle_clip(&track_id, &cue.text, start, end - start));
            prev_end = end;
            inserted += 1;
        }
        (track_id, inserted)
    }

    /// Segmente einer Untertitel-Spur als SRT-Cues (frame-genau aufs
    /// Sequenzraster gerundet, nach Startzeit sortiert).
    pub fn subtitle_cues(&self, track_id: &str) -> Vec<SrtCue> {
        let rate = self.settings.rate;
        let snap = |t: f64| rate.time_of_frame(rate.frame_round(t.max(0.0)).max(0) as f64);
        let mut cues: Vec<SrtCue> = self
            .clips
            .iter()
            .filter(|c| c.track_id == track_id && c.enabled)
            .filter_map(|c| {
                let text = c.subtitle.as_ref()?.text.trim().to_string();
                if text.is_empty() {
                    return None;
                }
                let start = snap(c.start);
                let end = snap(c.end());
                (end > start + EPS).then_some(SrtCue { start, end, text })
            })
            .collect();
        cues.sort_by(|a, b| a.start.total_cmp(&b.start));
        cues
    }

    /// Text eines Segments an einer Wort-/Zeilengrenze möglichst nah am
    /// Anteil `frac` (0–1) zweiteilen — verteilt die Lesezeit gleichmäßig,
    /// ohne ein Wort zu zerschneiden. Beide Hälften werden getrimmt; kein
    /// Wort geht verloren (Whitespace an der Grenze wird konsumiert).
    fn split_subtitle_text(text: &str, frac: f64) -> (String, String) {
        let chars: Vec<char> = text.chars().collect();
        let n = chars.len();
        if n == 0 {
            return (String::new(), String::new());
        }
        let target = ((frac.clamp(0.0, 1.0) * n as f64).round() as usize).min(n);
        // Whitespace-Position mit dem kleinsten Abstand zum Zielindex; ohne
        // jeglichen Whitespace (Einzelwort) am Zielindex hart trennen.
        let split = chars
            .iter()
            .enumerate()
            .filter(|(_, c)| c.is_whitespace())
            .min_by_key(|(i, _)| target.abs_diff(*i))
            .map(|(i, _)| i)
            .unwrap_or(target);
        let left: String = chars[..split].iter().collect();
        let right: String = chars[split..].iter().collect();
        (left.trim().to_string(), right.trim().to_string())
    }

    /// Untertitel-Segment unter dem Playhead (auf der aktiven Spur) am
    /// Playhead in zwei teilen: das Timing wird frame-genau an der Kante
    /// geteilt, der Text proportional zur Lesezeit auf beide Hälften
    /// verteilt. Ein Undo-Schritt; liefert die ID der rechten Hälfte.
    pub fn subtitle_split_at_playhead(&mut self) -> Result<String, String> {
        let track_id = self
            .active_subtitle_track()
            .map(|t| t.id.clone())
            .ok_or_else(|| "Keine Untertitel-Spur vorhanden".to_string())?;
        let locked = locked_track_ids(&self.tracks);
        if locked.contains(&track_id) {
            return Err("Die Untertitel-Spur ist gesperrt".to_string());
        }
        let rate = self.settings.rate;
        let at = rate.time_of_frame(rate.frame_round(self.playhead_sec.max(0.0)).max(0) as f64);
        // Segment, das beidseitig Platz für mindestens eine Frame-Hälfte lässt.
        let Some(idx) = self.clips.iter().position(|c| {
            c.track_id == track_id
                && c.subtitle.is_some()
                && at > c.start + MIN_CLIP_DURATION - EPS
                && at < c.end() - MIN_CLIP_DURATION + EPS
        }) else {
            return Err("Kein teilbares Untertitel-Segment am Playhead".to_string());
        };

        self.push_history();
        let original = self.clips[idx].clone();
        let left_len = at - original.start;
        let frac = (left_len / original.duration).clamp(0.0, 1.0);
        let text = original
            .subtitle
            .as_ref()
            .map(|s| s.text.clone())
            .unwrap_or_default();
        let (left_text, right_text) = Self::split_subtitle_text(&text, frac);

        let mut left = original.clone();
        left.duration = left_len;
        if let Some(spec) = left.subtitle.as_mut() {
            spec.text = left_text;
            left.name = spec.display_name();
        }

        let mut right = original.clone();
        right.id = new_id();
        right.start = at;
        right.duration = original.end() - at;
        if let Some(spec) = right.subtitle.as_mut() {
            spec.text = right_text;
            right.name = spec.display_name();
        }

        let right_id = right.id.clone();
        self.clips[idx] = left;
        self.clips.insert(idx + 1, right);
        self.selected_clip_ids = vec![right_id.clone()];
        self.selected_transition_ids.clear();
        Ok(right_id)
    }

    /// Das ausgewählte (sonst das am Playhead liegende) Untertitel-Segment
    /// mit seinem Nachbarn auf derselben Spur verschmelzen — bevorzugt mit
    /// dem nächsten, sonst dem vorherigen. Das Ergebnis spannt von der
    /// frühen Start- bis zur späten Endkante, die Texte werden zeilenweise
    /// verbunden. Ein Undo-Schritt; liefert die ID des Ergebnis-Segments.
    pub fn subtitle_merge(&mut self) -> Result<String, String> {
        // Anker: erstes ausgewähltes Untertitel-Segment, sonst das Segment
        // unter dem Playhead auf der aktiven Spur.
        let anchor = self
            .selected_clip_ids
            .iter()
            .find_map(|id| self.clips.iter().find(|c| &c.id == id && c.subtitle.is_some()))
            .or_else(|| {
                let track_id = self.active_subtitle_track().map(|t| t.id.clone())?;
                let at = self.playhead_sec;
                self.clips.iter().find(|c| {
                    c.track_id == track_id
                        && c.subtitle.is_some()
                        && at >= c.start - EPS
                        && at < c.end() + EPS
                })
            })
            .map(|c| (c.track_id.clone(), c.id.clone()));
        let Some((track_id, anchor_id)) = anchor else {
            return Err("Kein Untertitel-Segment ausgewählt".to_string());
        };
        if locked_track_ids(&self.tracks).contains(&track_id) {
            return Err("Die Untertitel-Spur ist gesperrt".to_string());
        }

        // Segmente der Spur nach Start sortiert; Nachbar des Ankers bestimmen.
        let mut seg: Vec<(String, f64)> = self
            .clips
            .iter()
            .filter(|c| c.track_id == track_id && c.subtitle.is_some())
            .map(|c| (c.id.clone(), c.start))
            .collect();
        seg.sort_by(|a, b| a.1.total_cmp(&b.1));
        let pos = seg.iter().position(|(id, _)| *id == anchor_id).unwrap();
        let neighbor = if pos + 1 < seg.len() {
            pos + 1
        } else if pos > 0 {
            pos - 1
        } else {
            return Err("Kein Nachbarsegment zum Zusammenführen".to_string());
        };
        let first_id = seg[pos.min(neighbor)].0.clone();
        let second_id = seg[pos.max(neighbor)].0.clone();

        self.push_history();
        let first = self.clips.iter().find(|c| c.id == first_id).cloned().unwrap();
        let second = self.clips.iter().find(|c| c.id == second_id).cloned().unwrap();
        let new_start = first.start;
        let new_end = second.end();
        let trim = |c: &TimelineClip| {
            c.subtitle
                .as_ref()
                .map(|s| s.text.trim().to_string())
                .unwrap_or_default()
        };
        let (a, b) = (trim(&first), trim(&second));
        let merged_text = match (a.is_empty(), b.is_empty()) {
            (true, _) => b,
            (_, true) => a,
            _ => format!("{a}\n{b}"),
        };

        self.clips.retain(|c| c.id != second_id);
        if let Some(c) = self.clips.iter_mut().find(|c| c.id == first_id) {
            c.start = new_start;
            c.duration = new_end - new_start;
            if let Some(spec) = c.subtitle.as_mut() {
                spec.text = merged_text;
                c.name = spec.display_name();
            }
        }
        self.selected_clip_ids = vec![first_id.clone()];
        self.selected_transition_ids.clear();
        // Falls je ein Übergang an dem entfernten Segment hing, bereinigen
        // (Untertitel tragen normalerweise keine — defensiv wie beim Löschen).
        self.reconcile_transitions();
        Ok(first_id)
    }

    pub fn remove_clips_for_assets(&mut self, asset_ids: &[String]) {
        let asset_set: std::collections::HashSet<&str> =
            asset_ids.iter().map(|s| s.as_str()).collect();
        if !self.clips.iter().any(|c| asset_set.contains(c.asset_id.as_str())) {
            return;
        }
        self.push_history();
        self.clips.retain(|c| !asset_set.contains(c.asset_id.as_str()));
        self.prune_selection();
        self.reconcile_transitions();
    }

}
