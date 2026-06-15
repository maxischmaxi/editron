//! impl-TimelineStore-Methoden (aus timeline.rs zerlegt).
use super::*;

impl TimelineStore {
    // --------------------------------------------------------- Bearbeitung

    pub fn insert_assets(
        &mut self,
        assets: &[MediaAsset],
        asset_ids: &[String],
        at: f64,
        drop_track_id: Option<&str>,
    ) {
        let placements = plan_asset_placements(self, assets, asset_ids, at, drop_track_id);
        if placements.is_empty() {
            return;
        }
        let was_empty = self.clips.is_empty();
        self.push_history();

        // Fehlende Spuren anlegen (höchstens eine je Art).
        let mut new_video: Option<String> = None;
        let mut new_audio: Option<String> = None;
        for p in &placements {
            if p.track_id.is_some() {
                continue;
            }
            match p.kind {
                TrackKind::Video => {
                    if new_video.is_none() {
                        let track = make_track(TrackKind::Video);
                        new_video = Some(track.id.clone());
                        let at = self.video_block_start();
                        self.tracks.insert(at, track);
                    }
                }
                TrackKind::Audio => {
                    if new_audio.is_none() {
                        let track = make_track(TrackKind::Audio);
                        new_audio = Some(track.id.clone());
                        self.tracks.push(track);
                    }
                }
                // Medien-Placements zielen nie auf Untertitel-Spuren.
                TrackKind::Subtitle => {}
            }
        }

        let mut clips = std::mem::take(&mut self.clips);
        let mut inserted: Vec<TimelineClip> = Vec::new();
        // Video- und Audio-Teil desselben Assets teilen sich eine link_id.
        let mut link_ids: std::collections::HashMap<String, String> = Default::default();
        for p in &placements {
            let track_id = match &p.track_id {
                Some(id) => id.clone(),
                None => match p.kind {
                    TrackKind::Video => new_video.clone().unwrap(),
                    TrackKind::Audio => new_audio.clone().unwrap(),
                    // Medien-Placements zielen nie auf Untertitel-Spuren.
                    TrackKind::Subtitle => continue,
                },
            };
            let link_id = if p.linked {
                let key = format!("{}@{}", p.asset_id, p.start);
                Some(
                    link_ids
                        .entry(key)
                        .or_insert_with(new_id)
                        .clone(),
                )
            } else {
                None
            };
            clips = overwrite_range(clips, &track_id, p.start, p.start + p.duration);
            // Asset-Marker (Quellmonitor) in Clip-Marker übernehmen, sofern
            // sie in den belegten Quellausschnitt [0, duration] fallen.
            let markers: Vec<Marker> = assets
                .iter()
                .find(|a| a.id == p.asset_id)
                .map(|a| {
                    a.markers
                        .iter()
                        .filter(|m| m.time >= -EPS && m.time <= p.duration + EPS)
                        .map(|m| {
                            let mut nm = m.clone();
                            nm.id = new_id();
                            nm
                        })
                        .collect()
                })
                .unwrap_or_default();
            inserted.push(TimelineClip {
                id: new_id(),
                track_id,
                asset_id: p.asset_id.clone(),
                name: p.name.clone(),
                kind: p.kind,
                start: p.start,
                duration: p.duration,
                src_in: 0.0,
                src_duration: p.src_duration,
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
                markers,
                nest_seq: None,
                multicam: None,
            });
        }

        self.selected_clip_ids = inserted.iter().map(|c| c.id.clone()).collect();
        clips.extend(inserted);
        self.clips = clips;
        self.reconcile_transitions();

        // Erster Drop in eine leere Timeline: abweichende Medien-Settings
        // als „An Medien anpassen?“-Vorschlag anbieten (wie Premiere).
        if was_empty {
            if let Some((suggested, asset_name)) =
                sequence::suggest_from_assets(&self.settings, assets, asset_ids)
            {
                if suggested != self.settings {
                    self.pending_media_match = Some(MediaMatchSuggestion {
                        settings: suggested,
                        asset_name,
                    });
                }
            }
        }
    }

    // -------------------------------------------- Insert / Three-Point-Edit

    /// Insert-Ripple-Primitive: öffnet auf den `affected`-Spuren bei `at` eine
    /// Lücke der Breite `amount`. Clips, die `at` durchschneiden, werden geteilt
    /// (Marker und End-Übergänge wandern auf die rechte Hälfte); alles ab `at`
    /// rückt nach hinten. KEIN History-Snapshot (der Aufrufer macht einen).
    fn open_gap(&mut self, affected: &std::collections::HashSet<String>, at: f64, amount: f64) {
        use std::collections::HashMap;
        if amount <= EPS {
            return;
        }
        let mut new_link_ids: HashMap<String, String> = HashMap::new();
        // Original-Clip → rechte Hälfte: End-Übergänge müssen mitwandern.
        let mut right_ids: HashMap<String, String> = HashMap::new();
        let mut out: Vec<TimelineClip> = Vec::with_capacity(self.clips.len() + 4);
        for c in std::mem::take(&mut self.clips) {
            let on_track = affected.contains(&c.track_id);
            let straddles = on_track
                && at > c.start + MIN_CLIP_DURATION - EPS
                && at < c.end() - MIN_CLIP_DURATION + EPS;
            if straddles {
                let left_len = at - c.start;
                let (left_src_in, right_src_in) = c.split_src_ins(left_len);
                let right_link = c.link_id.as_ref().map(|link| {
                    new_link_ids.entry(link.clone()).or_insert_with(new_id).clone()
                });
                let cut_media = c.media_time_at(at);
                let on_right = |m: &Marker| -> bool {
                    if c.freeze {
                        false
                    } else if c.reverse {
                        m.time <= cut_media
                    } else {
                        m.time >= cut_media
                    }
                };
                let mut left = c.clone();
                left.src_in = left_src_in;
                left.duration = left_len;
                left.markers = c.markers.iter().filter(|m| !on_right(m)).cloned().collect();
                let mut right = c.clone();
                right.id = new_id();
                right_ids.insert(c.id.clone(), right.id.clone());
                right.start = at + amount;
                right.src_in = right_src_in;
                right.duration = c.end() - at;
                right.link_id = right_link;
                right.markers = c.markers.iter().filter(|m| on_right(m)).cloned().collect();
                out.push(left);
                out.push(right);
            } else {
                let mut c = c;
                if on_track && c.start >= at - EPS {
                    c.start += amount;
                }
                out.push(c);
            }
        }
        self.clips = out;
        for tr in &mut self.transitions {
            if let Some(right) = tr.from_clip_id.as_ref().and_then(|id| right_ids.get(id)) {
                tr.from_clip_id = Some(right.clone());
            }
        }
    }

    /// Fertige Quell-Clips als Insert- (Ripple) oder Overwrite-Edit setzen.
    /// `gap_at`/`gap_len` = der von den Clips belegte Sequenzbereich:
    /// beim Insert die zu öffnende Lücke (auf Ziel- UND Sync-Lock-Spuren),
    /// beim Overwrite der zu leerende Bereich (nur Zielspuren). Verknüpfung,
    /// src_in und Marker stecken bereits in `new_clips`. Ein History-Eintrag.
    pub fn commit_edit(
        &mut self,
        new_clips: Vec<TimelineClip>,
        gap_at: f64,
        gap_len: f64,
        ripple: bool,
    ) {
        use std::collections::HashSet;
        if new_clips.is_empty() || gap_len <= EPS {
            return;
        }
        self.push_history();
        let targets: HashSet<String> = new_clips.iter().map(|c| c.track_id.clone()).collect();
        if ripple {
            let mut affected = targets.clone();
            for t in &self.tracks {
                if t.sync_lock && !t.locked {
                    affected.insert(t.id.clone());
                }
            }
            self.open_gap(&affected, gap_at, gap_len);
        } else {
            let mut clips = std::mem::take(&mut self.clips);
            for tid in &targets {
                clips = overwrite_range(clips, tid, gap_at, gap_at + gap_len);
            }
            self.clips = clips;
        }
        self.selected_clip_ids = new_clips.iter().map(|c| c.id.clone()).collect();
        self.clips.extend(new_clips);
        self.reconcile_transitions();
    }

    /// Lift (Premiere): den Sequenz-In/Out-Bereich auf allen anvisierten Spuren
    /// leeren — die Lücke bleibt stehen (kein Ripple). Liefert false, wenn
    /// nichts zu tun ist. Ein History-Eintrag.
    pub fn lift_range(&mut self) -> bool {
        let (Some(a), Some(b)) = (self.in_point, self.out_point) else {
            return false;
        };
        let (a, b) = (a.min(b), a.max(b));
        if b - a < MIN_CLIP_DURATION {
            return false;
        }
        let targets = self.targeted_track_ids();
        if targets.is_empty() {
            return false;
        }
        let has_work = self.clips.iter().any(|c| {
            targets.contains(&c.track_id) && c.start < b - EPS && c.end() > a + EPS
        });
        if !has_work {
            return false;
        }
        self.push_history();
        let mut clips = std::mem::take(&mut self.clips);
        for tid in &targets {
            clips = overwrite_range(clips, tid, a, b);
        }
        self.clips = clips;
        self.set_playhead(a);
        self.prune_selection();
        self.reconcile_transitions();
        true
    }

    /// Extract (Premiere): den Sequenz-In/Out-Bereich auf den anvisierten Spuren
    /// entfernen UND die Lücke schließen. Sync-Lock-Spuren rippeln mit (ihr
    /// Material im Bereich wird ebenfalls entfernt, damit der Sync erhalten
    /// bleibt). Liefert false, wenn nichts zu tun ist. Ein History-Eintrag.
    pub fn extract_range(&mut self) -> bool {
        use std::collections::HashSet;
        let (Some(a), Some(b)) = (self.in_point, self.out_point) else {
            return false;
        };
        let (a, b) = (a.min(b), a.max(b));
        let d = b - a;
        if d < MIN_CLIP_DURATION {
            return false;
        }
        let targets = self.targeted_track_ids();
        if targets.is_empty() {
            return false;
        }
        let mut affected: HashSet<String> = targets;
        for t in &self.tracks {
            if t.sync_lock && !t.locked {
                affected.insert(t.id.clone());
            }
        }
        let has_work = self.clips.iter().any(|c| {
            affected.contains(&c.track_id)
                && ((c.start < b - EPS && c.end() > a + EPS) || c.start >= b - EPS)
        });
        if !has_work {
            return false;
        }
        self.push_history();
        let mut clips = std::mem::take(&mut self.clips);
        for tid in &affected {
            clips = overwrite_range(clips, tid, a, b);
        }
        for c in &mut clips {
            if affected.contains(&c.track_id) && c.start >= b - EPS {
                c.start = (c.start - d).max(0.0);
            }
        }
        self.clips = clips;
        self.set_playhead(a);
        self.prune_selection();
        self.reconcile_transitions();
        true
    }

    /// Erstes echtes Material-Clip (kein Generator) unter Sequenzzeit `t` auf
    /// einer Spur der gewünschten Art; optional auf anvisierte Spuren beschränkt.
    /// Reihenfolge folgt dem Track-Stack (oben zuerst).
    fn first_clip_at(
        &self,
        t: f64,
        kind: TrackKind,
        targeted_only: bool,
    ) -> Option<&TimelineClip> {
        for track in self.tracks.iter().filter(|tr| tr.kind == kind) {
            if targeted_only && !track.targeted {
                continue;
            }
            if let Some(c) = self.clips.iter().find(|c| {
                c.track_id == track.id
                    && !c.is_generator()
                    && !c.asset_id.is_empty()
                    && c.start <= t + EPS
                    && c.end() > t - EPS
            }) {
                return Some(c);
            }
        }
        None
    }

    /// Match Frame: Quelle (asset_id) und exakte Medienzeit unter dem Playhead.
    /// Bevorzugt anvisierte Video-, dann anvisierte Audio-, dann beliebige
    /// Video-/Audiospuren (oberste Ebene mit Material).
    pub fn match_frame_source(&self, t: f64) -> Option<(String, f64)> {
        let clip = self
            .first_clip_at(t, TrackKind::Video, true)
            .or_else(|| self.first_clip_at(t, TrackKind::Audio, true))
            .or_else(|| self.first_clip_at(t, TrackKind::Video, false))
            .or_else(|| self.first_clip_at(t, TrackKind::Audio, false))?;
        Some((clip.asset_id.clone(), clip.media_time_at(t)))
    }

    /// Bewusst keine Link-Expansion: die Auswahl ist bereits expandiert,
    /// und Alt-Drag soll gezielt eine Hälfte eines Paares bewegen können.
    pub fn move_clips(&mut self, ids: &[String], delta_sec: f64, lane_offset: i32) {
        use std::collections::HashSet;
        let locked = locked_track_ids(&self.tracks);
        let id_set: HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
        let moving: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| id_set.contains(c.id.as_str()) && !locked.contains(&c.track_id))
            .cloned()
            .collect();
        if moving.is_empty() {
            return;
        }

        let min_start = moving.iter().map(|c| c.start).fold(f64::INFINITY, f64::min);
        let d = delta_sec.max(-min_start);
        if d.abs() < EPS && lane_offset == 0 {
            return;
        }

        let lanes_of = |kind: TrackKind| -> Vec<&TimelineTrack> {
            self.tracks.iter().filter(|t| t.kind == kind).collect()
        };
        let video_tracks = lanes_of(TrackKind::Video);
        let audio_tracks = lanes_of(TrackKind::Audio);
        let subtitle_tracks = lanes_of(TrackKind::Subtitle);
        let remap = |clip: &TimelineClip| -> String {
            if lane_offset == 0 {
                return clip.track_id.clone();
            }
            let lanes = match clip.kind {
                TrackKind::Video => &video_tracks,
                TrackKind::Audio => &audio_tracks,
                TrackKind::Subtitle => &subtitle_tracks,
            };
            let Some(idx) = lanes.iter().position(|t| t.id == clip.track_id) else {
                return clip.track_id.clone();
            };
            let new_idx = (idx as i32 + lane_offset).clamp(0, lanes.len() as i32 - 1) as usize;
            lanes[new_idx].id.clone()
        };

        let placed: Vec<TimelineClip> = moving
            .iter()
            .map(|c| {
                let mut p = c.clone();
                p.start = c.start + d;
                p.track_id = remap(c);
                p
            })
            .collect();
        if placed.iter().any(|c| locked.contains(&c.track_id)) {
            return;
        }

        self.push_history();
        let mut rest: Vec<TimelineClip> = std::mem::take(&mut self.clips)
            .into_iter()
            .filter(|c| !id_set.contains(c.id.as_str()) || locked.contains(&c.track_id))
            .collect();
        for p in &placed {
            rest = overwrite_range(rest, &p.track_id, p.start, p.start + p.duration);
        }
        rest.extend(placed);
        self.clips = rest;
        self.reconcile_transitions();
    }

    /// Wie `move_clips`, aber die Originale bleiben liegen: die Auswahl wird
    /// an der Zielposition als Kopie eingefügt (Alt+Drag-Duplizieren).
    pub fn duplicate_clips(&mut self, ids: &[String], delta_sec: f64, lane_offset: i32) {
        use std::collections::{HashMap, HashSet};
        let locked = locked_track_ids(&self.tracks);
        let id_set: HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
        let sources: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| id_set.contains(c.id.as_str()) && !locked.contains(&c.track_id))
            .cloned()
            .collect();
        if sources.is_empty() {
            return;
        }

        let min_start = sources.iter().map(|c| c.start).fold(f64::INFINITY, f64::min);
        let d = delta_sec.max(-min_start);
        if d.abs() < EPS && lane_offset == 0 {
            return;
        }

        let lanes_of = |kind: TrackKind| -> Vec<&TimelineTrack> {
            self.tracks.iter().filter(|t| t.kind == kind).collect()
        };
        let video_tracks = lanes_of(TrackKind::Video);
        let audio_tracks = lanes_of(TrackKind::Audio);
        let subtitle_tracks = lanes_of(TrackKind::Subtitle);
        let remap = |clip: &TimelineClip| -> String {
            if lane_offset == 0 {
                return clip.track_id.clone();
            }
            let lanes = match clip.kind {
                TrackKind::Video => &video_tracks,
                TrackKind::Audio => &audio_tracks,
                TrackKind::Subtitle => &subtitle_tracks,
            };
            let Some(idx) = lanes.iter().position(|t| t.id == clip.track_id) else {
                return clip.track_id.clone();
            };
            let new_idx = (idx as i32 + lane_offset).clamp(0, lanes.len() as i32 - 1) as usize;
            lanes[new_idx].id.clone()
        };

        // Kopien verknüpfter Paare teilen sich eine frische link_id.
        let mut new_link_ids: HashMap<String, String> = HashMap::new();
        let mut id_map: HashMap<String, String> = HashMap::new();
        let placed: Vec<TimelineClip> = sources
            .iter()
            .map(|c| {
                let mut p = c.clone();
                p.id = new_id();
                id_map.insert(c.id.clone(), p.id.clone());
                p.start = c.start + d;
                p.track_id = remap(c);
                p.link_id = c.link_id.as_ref().map(|link| {
                    new_link_ids
                        .entry(link.clone())
                        .or_insert_with(new_id)
                        .clone()
                });
                p
            })
            .collect();
        if placed.iter().any(|c| locked.contains(&c.track_id)) {
            return;
        }

        self.push_history();
        let mut rest = std::mem::take(&mut self.clips);
        for p in &placed {
            rest = overwrite_range(rest, &p.track_id, p.start, p.start + p.duration);
        }
        self.selected_clip_ids = placed.iter().map(|c| c.id.clone()).collect();
        rest.extend(placed);
        self.clips = rest;
        // Übergänge mitkopieren, deren Kanten vollständig kopiert wurden.
        let copies: Vec<Transition> = self
            .transitions
            .iter()
            .filter_map(|t| remap_transition(t, &id_map))
            .collect();
        self.transitions.extend(copies);
        self.reconcile_transitions();
    }

    pub fn trim_clip(&mut self, id: &str, edge: TrimEdge, delta: f64) {
        let locked = locked_track_ids(&self.tracks);
        let expanded = expand_links(&self.clips, &[id.to_string()]);
        let targets: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| expanded.contains(&c.id) && !locked.contains(&c.track_id))
            .cloned()
            .collect();
        if targets.is_empty() {
            return;
        }
        let mut d = delta;
        for clip in &targets {
            let (lo, hi) = trim_range(clip, edge, &self.clips, true);
            d = d.clamp(lo, hi);
        }
        if d.abs() < EPS {
            return;
        }
        self.push_history();
        let target_ids: std::collections::HashSet<&str> =
            targets.iter().map(|c| c.id.as_str()).collect();
        self.clips = self
            .clips
            .iter()
            .map(|c| {
                if target_ids.contains(c.id.as_str()) {
                    apply_trim(c, edge, d)
                } else {
                    c.clone()
                }
            })
            .collect();
        self.reconcile_transitions();
    }

    pub fn ripple_trim_clip(&mut self, id: &str, edge: TrimEdge, delta: f64) {
        let locked = locked_track_ids(&self.tracks);
        let expanded = expand_links(&self.clips, &[id.to_string()]);
        let targets: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| expanded.contains(&c.id) && !locked.contains(&c.track_id))
            .cloned()
            .collect();
        if targets.is_empty() {
            return;
        }
        let mut d = delta;
        for clip in &targets {
            let (lo, hi) = trim_range(clip, edge, &self.clips, false);
            d = d.clamp(lo, hi);
        }
        if d.abs() < EPS {
            return;
        }

        self.push_history();
        let target_ids: std::collections::HashSet<&str> =
            targets.iter().map(|c| c.id.as_str()).collect();
        let anchor = &targets[0];
        // Alles hinter dem Schnittpunkt rückt nach: End-Trim verschiebt um +d,
        // Start-Trim (Kopf kürzen) um -d.
        let boundary = match edge {
            TrimEdge::End => anchor.end(),
            TrimEdge::Start => anchor.start,
        };
        let shift = match edge {
            TrimEdge::End => d,
            TrimEdge::Start => -d,
        };
        self.clips = self
            .clips
            .iter()
            .map(|c| {
                if target_ids.contains(c.id.as_str()) {
                    let trimmed = apply_trim(c, edge, d);
                    // Beim Start-Trim bleibt die Schnittkante stehen: Clip rückt mit.
                    if edge == TrimEdge::Start {
                        let mut t = trimmed;
                        t.start = c.start;
                        t
                    } else {
                        trimmed
                    }
                } else if !locked.contains(&c.track_id) && c.start >= boundary - EPS {
                    let mut m = c.clone();
                    m.start = (c.start + shift).max(0.0);
                    m
                } else {
                    c.clone()
                }
            })
            .collect();
        self.reconcile_transitions();
    }

    pub fn roll_edit(&mut self, left_id: &str, right_id: &str, delta: f64) {
        let locked = locked_track_ids(&self.tracks);
        let Some(left) = self.clips.iter().find(|c| c.id == left_id).cloned() else {
            return;
        };
        let Some(right) = self.clips.iter().find(|c| c.id == right_id).cloned() else {
            return;
        };
        if locked.contains(&left.track_id) || locked.contains(&right.track_id) {
            return;
        }
        let (lo_l, hi_l) = trim_range(&left, TrimEdge::End, &self.clips, false);
        let (lo_r, hi_r) = trim_range(&right, TrimEdge::Start, &self.clips, false);
        let d = delta.clamp(lo_l.max(lo_r), hi_l.min(hi_r));
        if d.abs() < EPS {
            return;
        }
        self.push_history();
        self.clips = self
            .clips
            .iter()
            .map(|c| {
                if c.id == left_id {
                    apply_trim(c, TrimEdge::End, d)
                } else if c.id == right_id {
                    apply_trim(c, TrimEdge::Start, d)
                } else {
                    c.clone()
                }
            })
            .collect();
        self.reconcile_transitions();
    }

    pub fn slide_clip(&mut self, id: &str, delta: f64) {
        use std::collections::HashSet;
        let locked = locked_track_ids(&self.tracks);
        let ids: HashSet<String> = expand_links(&self.clips, &[id.to_string()])
            .into_iter()
            .collect();
        let sliding: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| ids.contains(&c.id) && !locked.contains(&c.track_id))
            .cloned()
            .collect();
        if sliding.is_empty() {
            return;
        }

        // Direkt angrenzende Nachbarn rollen mit; Lücken absorbieren die Bewegung.
        let mut left_ids: HashSet<String> = HashSet::new();
        let mut right_ids: HashSet<String> = HashSet::new();
        let mut d = delta;
        for clip in &sliding {
            let clip_end = clip.end();
            let prev = self.clips.iter().find(|c| {
                c.track_id == clip.track_id
                    && !ids.contains(&c.id)
                    && (c.end() - clip.start).abs() < EPS
            });
            let next = self.clips.iter().find(|c| {
                c.track_id == clip.track_id && !ids.contains(&c.id) && (c.start - clip_end).abs() < EPS
            });
            if let Some(prev) = prev {
                let (lo, hi) = trim_range(prev, TrimEdge::End, &self.clips, false);
                d = d.clamp(lo, hi);
                left_ids.insert(prev.id.clone());
            }
            if let Some(next) = next {
                let (lo, hi) = trim_range(next, TrimEdge::Start, &self.clips, false);
                d = d.clamp(lo, hi);
                right_ids.insert(next.id.clone());
            }
            d = d.max(-clip.start);
        }
        if d.abs() < EPS {
            return;
        }

        self.push_history();
        self.clips = self
            .clips
            .iter()
            .map(|c| {
                if ids.contains(&c.id) && !locked.contains(&c.track_id) {
                    let mut m = c.clone();
                    m.start = c.start + d;
                    m
                } else if left_ids.contains(&c.id) {
                    apply_trim(c, TrimEdge::End, d)
                } else if right_ids.contains(&c.id) {
                    apply_trim(c, TrimEdge::Start, d)
                } else {
                    c.clone()
                }
            })
            .collect();
        self.reconcile_transitions();
    }

    pub fn slip_clip(&mut self, id: &str, delta: f64) {
        let locked = locked_track_ids(&self.tracks);
        let expanded = expand_links(&self.clips, &[id.to_string()]);
        let targets: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| {
                expanded.contains(&c.id)
                    && !locked.contains(&c.track_id)
                    && c.src_duration.is_finite()
            })
            .cloned()
            .collect();
        if targets.is_empty() {
            return;
        }
        // Delta in Timeline-Sekunden; die Medienverschiebung skaliert mit
        // der Clip-Geschwindigkeit und bleibt innerhalb der Quelle.
        let mut d = delta;
        for clip in &targets {
            let s = clip.eff_speed();
            d = d.clamp(
                -clip.src_in / s,
                (clip.src_duration - clip.media_out()).max(0.0) / s,
            );
        }
        if d.abs() < EPS {
            return;
        }
        self.push_history();
        let target_ids: std::collections::HashSet<&str> =
            targets.iter().map(|c| c.id.as_str()).collect();
        for c in &mut self.clips {
            if target_ids.contains(c.id.as_str()) {
                c.src_in += d * c.eff_speed();
            }
        }
        self.reconcile_transitions();
    }

    pub fn split_at(&mut self, time: f64, clip_ids: Option<&[String]>) {
        use std::collections::{HashMap, HashSet};
        let locked = locked_track_ids(&self.tracks);
        let candidates: Option<HashSet<String>> = match clip_ids {
            Some(ids) if !ids.is_empty() => {
                Some(expand_links(&self.clips, ids).into_iter().collect())
            }
            _ => None,
        };
        let splittable = |c: &TimelineClip| -> bool {
            !locked.contains(&c.track_id)
                && candidates.as_ref().is_none_or(|set| set.contains(&c.id))
                && time > c.start + MIN_CLIP_DURATION - EPS
                && time < c.end() - MIN_CLIP_DURATION + EPS
        };
        if !self.clips.iter().any(&splittable) {
            return;
        }

        self.push_history();
        // Rechte Hälften verknüpfter Clips bekommen eine gemeinsame neue link_id.
        let mut new_link_ids: HashMap<String, String> = HashMap::new();
        // Map Original → rechte Hälfte: Übergänge am CLIPENDE wandern mit.
        let mut right_ids: HashMap<String, String> = HashMap::new();
        let mut clips: Vec<TimelineClip> = Vec::with_capacity(self.clips.len() + 4);
        let mut new_selection: Vec<String> = Vec::new();
        for c in &self.clips {
            if !splittable(c) {
                clips.push(c.clone());
                continue;
            }
            let left_len = time - c.start;
            let (left_src_in, right_src_in) = c.split_src_ins(left_len);
            let right_link = c.link_id.as_ref().map(|link| {
                new_link_ids
                    .entry(link.clone())
                    .or_insert_with(new_id)
                    .clone()
            });
            let mut right = c.clone();
            right.id = new_id();
            right_ids.insert(c.id.clone(), right.id.clone());
            right.start = time;
            right.src_in = right_src_in;
            right.duration = c.duration - left_len;
            right.link_id = right_link;
            let mut left = c.clone();
            left.src_in = left_src_in;
            left.duration = left_len;
            // Clip-Marker an der Schnittkante (Medienzeit) auf die Hälften
            // aufteilen — jede Hälfte behält die Marker ihres Quellausschnitts.
            let cut_media = c.media_time_at(time);
            let on_right = |m: &Marker| -> bool {
                if c.freeze {
                    false
                } else if c.reverse {
                    m.time <= cut_media
                } else {
                    m.time >= cut_media
                }
            };
            right.markers = c.markers.iter().filter(|m| on_right(m)).cloned().collect();
            left.markers = c.markers.iter().filter(|m| !on_right(m)).cloned().collect();
            if self.selected_clip_ids.contains(&c.id) {
                new_selection.push(left.id.clone());
                new_selection.push(right.id.clone());
            }
            clips.push(left);
            clips.push(right);
        }
        self.clips = clips;
        // Übergänge, die am Ende des geteilten Clips hingen, referenzieren
        // jetzt die rechte Hälfte (sie endet an der ursprünglichen Kante).
        for tr in &mut self.transitions {
            if let Some(right) = tr.from_clip_id.as_ref().and_then(|id| right_ids.get(id)) {
                tr.from_clip_id = Some(right.clone());
            }
        }
        self.reconcile_transitions();
        if !new_selection.is_empty() {
            self.selected_clip_ids = new_selection;
        } else {
            self.prune_selection();
        }
    }

}
