//! impl-TimelineStore-Methoden (aus timeline.rs zerlegt).
use super::*;

impl TimelineStore {
    // ------------------------------------------------------------- Multicam

    /// Topmost sichtbarer Multicam-Video-Clip, der die Sequenzzeit `t` enthält.
    pub fn topmost_multicam_video_at(&self, t: f64) -> Option<&TimelineClip> {
        let order = |track_id: &str| {
            self.tracks
                .iter()
                .position(|tr| tr.id == track_id)
                .unwrap_or(usize::MAX)
        };
        self.clips
            .iter()
            .filter(|c| {
                c.is_multicam()
                    && c.kind == TrackKind::Video
                    && c.enabled
                    && t >= c.start - EPS
                    && t < c.end() - EPS
            })
            .min_by_key(|c| order(&c.track_id))
    }

    /// Den aktiven Winkel eines Multicam-Clips (samt verknüpfter Link-Gruppe,
    /// Video + Audio) setzen. Liefert true, wenn sich etwas geändert hat.
    pub fn set_multicam_angle(&mut self, clip_id: &str, angle: u32) -> bool {
        let group: std::collections::HashSet<String> =
            expand_links(&self.clips, &[clip_id.to_string()])
                .into_iter()
                .collect();
        let mut changed = false;
        for c in self.clips.iter_mut().filter(|c| group.contains(&c.id)) {
            if let Some(mc) = c.multicam.as_mut() {
                if mc.angle != angle {
                    mc.angle = angle;
                    changed = true;
                }
            }
        }
        if changed {
            self.revision += 1;
        }
        changed
    }

    /// Wie [`set_multicam_angle`](Self::set_multicam_angle), aber mit History-
    /// Snapshot (Winkelwechsel ohne Wiedergabe ist eine reguläre, rückgängig-
    /// machbare Bearbeitung). Kein Snapshot, wenn sich nichts ändert.
    pub fn set_multicam_angle_undoable(&mut self, clip_id: &str, angle: u32) -> bool {
        let group: std::collections::HashSet<String> =
            expand_links(&self.clips, &[clip_id.to_string()])
                .into_iter()
                .collect();
        let changes = self.clips.iter().any(|c| {
            group.contains(&c.id) && c.multicam.as_ref().is_some_and(|mc| mc.angle != angle)
        });
        if !changes {
            return false;
        }
        self.push_history();
        self.set_multicam_angle(clip_id, angle)
    }

    /// Multicam-Live-Schnitt am Playhead `t`: den Multicam-Clip (samt Link-
    /// Gruppe) teilen und der RECHTEN Hälfte den Winkel `angle` geben (Premiere-
    /// Verhalten während der Wiedergabe). Liegt der Playhead an einer Kante
    /// (kein Schnitt möglich), wird nur der Winkel des bestehenden Clips
    /// gesetzt. Liefert true, wenn ein Multicam-Clip betroffen war.
    pub fn multicam_live_cut(&mut self, t: f64, angle: u32) -> bool {
        let Some(clip_id) = self.topmost_multicam_video_at(t).map(|c| c.id.clone()) else {
            return false;
        };
        let before: std::collections::HashSet<String> =
            self.clips.iter().map(|c| c.id.clone()).collect();
        self.split_at(t, Some(&[clip_id.clone()]));
        let new_right: Vec<String> = self
            .clips
            .iter()
            .filter(|c| !before.contains(&c.id) && c.is_multicam())
            .map(|c| c.id.clone())
            .collect();
        if new_right.is_empty() {
            // Kein Schnitt (Playhead an der Kante): nur Winkel setzen.
            self.set_multicam_angle(&clip_id, angle);
        } else {
            for c in self
                .clips
                .iter_mut()
                .filter(|c| new_right.contains(&c.id))
            {
                if let Some(mc) = c.multicam.as_mut() {
                    mc.angle = angle;
                }
            }
            self.revision += 1;
        }
        true
    }

    /// Keine Link-Expansion — die Auswahl ist bereits expandiert.
    pub fn delete_clips(&mut self, ids: &[String], ripple: bool) {
        use std::collections::HashSet;
        let locked = locked_track_ids(&self.tracks);
        let id_set: HashSet<&str> = ids
            .iter()
            .filter(|id| {
                self.clips
                    .iter()
                    .find(|c| &c.id == *id)
                    .is_some_and(|c| !locked.contains(&c.track_id))
            })
            .map(|s| s.as_str())
            .collect();
        if id_set.is_empty() {
            return;
        }

        self.push_history();
        let removed: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| id_set.contains(c.id.as_str()))
            .cloned()
            .collect();
        self.clips.retain(|c| !id_set.contains(c.id.as_str()));

        if ripple {
            // Pro Spur die TATSÄCHLICH entfernten Bereiche schließen — nicht eine
            // globale Sammelspanne über alle Spuren (das verschob früher auch
            // unberührte Spuren um die volle Auswahlbreite ⇒ A/V-Sync kaputt, und
            // ließ nicht-zusammenhängende Auswahlen absurd weit springen).
            // Betroffen: Spuren mit Entfernung + sync-gelockte (ungesperrte) Spuren
            // (analog zu extract_range). Sie teilen die gemergte Intervall-Achse,
            // verschieben sich also positionsgleich und bleiben in Sync.
            let mut affected: HashSet<String> =
                removed.iter().map(|c| c.track_id.clone()).collect();
            for t in &self.tracks {
                if t.sync_lock && !t.locked {
                    affected.insert(t.id.clone());
                }
            }
            // Entfernte Intervalle der betroffenen Spuren zu disjunkten Bereichen mergen.
            let mut intervals: Vec<(f64, f64)> = removed
                .iter()
                .filter(|c| affected.contains(&c.track_id))
                .map(|c| (c.start, c.end()))
                .collect();
            intervals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            let mut merged: Vec<(f64, f64)> = Vec::new();
            for (s, e) in intervals {
                match merged.last_mut() {
                    Some(last) if s <= last.1 + EPS => last.1 = last.1.max(e),
                    _ => merged.push((s, e)),
                }
            }
            // Jeder Clip rückt um die Gesamtlänge der entfernten Bereiche nach
            // links, die VOR seinem Start enden (überlappende Bereiche aus
            // Sync-Spuren werden bewusst nicht teilverschoben).
            for c in &mut self.clips {
                if !affected.contains(&c.track_id) {
                    continue;
                }
                let shift: f64 = merged
                    .iter()
                    .filter(|(_, e)| *e <= c.start + EPS)
                    .map(|(s, e)| e - s)
                    .sum();
                if shift > 0.0 {
                    c.start = (c.start - shift).max(0.0);
                }
            }
        }
        self.prune_selection();
        self.reconcile_transitions();
    }

    pub fn set_clips_enabled(&mut self, ids: &[String], enabled: bool) {
        let id_set: std::collections::HashSet<String> =
            expand_links(&self.clips, ids).into_iter().collect();
        self.push_history();
        for c in &mut self.clips {
            if id_set.contains(&c.id) {
                c.enabled = enabled;
            }
        }
    }

    pub fn toggle_link_selected(&mut self) {
        let selected: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| self.selected_clip_ids.contains(&c.id))
            .cloned()
            .collect();
        if selected.is_empty() {
            return;
        }
        let any_linked = selected.iter().any(|c| c.link_id.is_some());
        let id_set: std::collections::HashSet<&str> =
            selected.iter().map(|c| c.id.as_str()).collect();
        if any_linked {
            self.push_history();
            for c in &mut self.clips {
                if id_set.contains(c.id.as_str()) {
                    c.link_id = None;
                }
            }
            return;
        }
        // Neu verknüpfen: nur sinnvoll, wenn Video + Audio gemeinsam gewählt sind.
        let has_video = selected.iter().any(|c| c.kind == TrackKind::Video);
        let has_audio = selected.iter().any(|c| c.kind == TrackKind::Audio);
        if !has_video || !has_audio {
            return;
        }
        self.push_history();
        let link_id = new_id();
        for c in &mut self.clips {
            if id_set.contains(c.id.as_str()) {
                c.link_id = Some(link_id.clone());
            }
        }
    }

}
