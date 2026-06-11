//! Sequenz-Modell + Store: Tracks/Clips mit verknüpften A/V-Paaren,
//! Snapshot-History (Undo/Redo) und allen Editier-Operationen.

use crate::core::types::{new_id, MediaAsset, MediaKind};
use serde::{Deserialize, Serialize};

pub const SEQUENCE_FPS: f64 = 25.0;
pub const IMAGE_DEFAULT_DURATION: f64 = 5.0;
pub const MIN_CLIP_DURATION: f64 = 1.0 / SEQUENCE_FPS;

const MIN_ZOOM: f64 = 4.0;
const MAX_ZOOM: f64 = 1000.0;
const ZOOM_FACTOR: f64 = 1.5;
const HISTORY_LIMIT: usize = 100;
const EPS: f64 = 1e-6;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackFlag {
    Muted,
    Solo,
    Locked,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineTrack {
    pub id: String,
    pub kind: TrackKind,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub solo: bool,
    /// Gesperrte Spuren sind von allen Editier-Operationen ausgenommen.
    #[serde(default)]
    pub locked: bool,
    /// Spur-Verstärkung in dB (Mixer-Fader); ≤ −60 gilt als −∞.
    #[serde(default)]
    pub gain_db: f64,
    /// Stereo-Balance −1 (links) .. +1 (rechts); nur für Audio-Spuren relevant.
    #[serde(default)]
    pub pan: f64,
}

fn make_track(kind: TrackKind) -> TimelineTrack {
    TimelineTrack {
        id: new_id(),
        kind,
        muted: false,
        solo: false,
        locked: false,
        gain_db: 0.0,
        pan: 0.0,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineClip {
    pub id: String,
    pub track_id: String,
    pub asset_id: String,
    /// Anzeigename (i. d. R. Dateiname, Audio-Teil mit Suffix).
    pub name: String,
    pub kind: TrackKind,
    /// Startzeit in der Sequenz (Sekunden).
    pub start: f64,
    pub duration: f64,
    /// In-Punkt innerhalb der Quelldatei.
    pub src_in: f64,
    /// Gesamtdauer der Quelle; INFINITY bei Standbildern (frei dehnbar).
    /// JSON kennt kein Infinity — wird als null gespeichert.
    #[serde(with = "infinite_duration")]
    pub src_duration: f64,
    /// Verknüpfungsgruppe Video↔Audio desselben Assets.
    pub link_id: Option<String>,
    /// Deaktivierte Clips bleiben in der Sequenz, sind aber ausgegraut.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Clip-Verstärkung in dB (zusätzlich zum Spur-Fader); ≤ −60 gilt als −∞.
    #[serde(default)]
    pub gain_db: f64,
}

fn default_enabled() -> bool {
    true
}

/// INFINITY (Standbilder) ↔ null: serde_json würde Infinity sonst stillschweigend
/// zu null serialisieren und beim Laden an f64 scheitern.
mod infinite_duration {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        if v.is_finite() {
            s.serialize_some(v)
        } else {
            s.serialize_none()
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        Ok(Option::<f64>::deserialize(d)?.unwrap_or(f64::INFINITY))
    }
}

impl TimelineClip {
    pub fn end(&self) -> f64 {
        self.start + self.duration
    }
}

/// Ende des letzten Clips — die effektive Sequenzdauer.
pub fn sequence_end(clips: &[TimelineClip]) -> f64 {
    clips.iter().map(|c| c.end()).fold(0.0, f64::max)
}

/// Anzeigename einer Spur (V1 unten im Video-Block, A1 oben im Audio-Block).
pub fn track_name(track: &TimelineTrack, tracks: &[TimelineTrack]) -> String {
    match track.kind {
        TrackKind::Video => {
            let videos: Vec<&TimelineTrack> =
                tracks.iter().filter(|t| t.kind == TrackKind::Video).collect();
            let idx = videos.iter().position(|t| t.id == track.id).unwrap_or(0);
            format!("V{}", videos.len() - idx)
        }
        TrackKind::Audio => {
            let audios: Vec<&TimelineTrack> =
                tracks.iter().filter(|t| t.kind == TrackKind::Audio).collect();
            let idx = audios.iter().position(|t| t.id == track.id).unwrap_or(0);
            format!("A{}", idx + 1)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrimEdge {
    Start,
    End,
}

#[derive(Clone)]
struct Snapshot {
    tracks: Vec<TimelineTrack>,
    clips: Vec<TimelineClip>,
    master_gain_db: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectMode {
    Replace,
    Add,
    Toggle,
}

/// Geplante Platzierung eines Assets beim Einfügen/Droppen.
#[derive(Clone, Debug)]
pub struct PlannedPlacement {
    pub asset_id: String,
    pub kind: TrackKind,
    /// None = passende Spur fehlt und wird automatisch angelegt.
    pub track_id: Option<String>,
    pub start: f64,
    pub duration: f64,
    pub name: String,
    pub src_duration: f64,
    pub linked: bool,
}

pub struct TimelineStore {
    pub tracks: Vec<TimelineTrack>,
    pub clips: Vec<TimelineClip>,
    pub selected_clip_ids: Vec<String>,
    pub clipboard: Vec<TimelineClip>,
    pub playhead_sec: f64,
    /// In-/Out-Punkt der Sequenz: Loop-Bereich des Programmmonitors.
    /// Wie Playhead/Auswahl bewusst nicht Teil der Undo-History.
    pub in_point: Option<f64>,
    pub out_point: Option<f64>,
    pub snapping: bool,
    pub zoom_px_per_sec: f64,
    /// Sichtbare Breite des Spurbereichs (vom Panel gemeldet, für zoom_to_fit).
    pub viewport_w: f64,
    /// Zählt strukturelle Änderungen (Edits/Undo/Redo) — Basis des
    /// Dirty-Trackings im Projekt; Playhead/Zoom/Auswahl zählen nicht.
    pub revision: u64,
    /// Summen-Fader des Mixers in dB; ≤ −60 gilt als −∞.
    pub master_gain_db: f64,
    past: Vec<Snapshot>,
    future: Vec<Snapshot>,
}

impl Default for TimelineStore {
    fn default() -> Self {
        // Startbelegung wie ein frisches Premiere-Projekt: 2 Video-, 2 Audiospuren.
        TimelineStore {
            tracks: vec![
                make_track(TrackKind::Video),
                make_track(TrackKind::Video),
                make_track(TrackKind::Audio),
                make_track(TrackKind::Audio),
            ],
            clips: Vec::new(),
            selected_clip_ids: Vec::new(),
            clipboard: Vec::new(),
            playhead_sec: 0.0,
            in_point: None,
            out_point: None,
            snapping: true,
            zoom_px_per_sec: 40.0,
            viewport_w: 0.0,
            revision: 0,
            master_gain_db: 0.0,
            past: Vec::new(),
            future: Vec::new(),
        }
    }
}

fn clamp_zoom(v: f64) -> f64 {
    if !v.is_finite() {
        return MIN_ZOOM;
    }
    v.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// Entfernt aus `clips` alles, was auf `track_id` den Bereich [start, end)
/// belegt (Overwrite-Semantik wie beim Premiere-Drop).
fn overwrite_range(clips: Vec<TimelineClip>, track_id: &str, start: f64, end: f64) -> Vec<TimelineClip> {
    if end - start <= EPS {
        return clips;
    }
    let mut out = Vec::with_capacity(clips.len());
    for clip in clips {
        let clip_end = clip.end();
        if clip.track_id != track_id || clip_end <= start + EPS || clip.start >= end - EPS {
            out.push(clip);
            continue;
        }
        let left_len = start - clip.start;
        let right_len = clip_end - end;
        let keep_left = left_len >= MIN_CLIP_DURATION - EPS;
        if keep_left {
            let mut left = clip.clone();
            left.duration = left_len;
            out.push(left);
        }
        if right_len >= MIN_CLIP_DURATION - EPS {
            let mut right = clip.clone();
            if keep_left {
                right.id = new_id();
            }
            right.src_in = clip.src_in + (end - clip.start);
            right.start = end;
            right.duration = right_len;
            out.push(right);
        }
    }
    out
}

fn locked_track_ids(tracks: &[TimelineTrack]) -> std::collections::HashSet<String> {
    tracks
        .iter()
        .filter(|t| t.locked)
        .map(|t| t.id.clone())
        .collect()
}

/// Erweitert eine Clip-Auswahl um alle verknüpften Partner.
pub fn expand_links(clips: &[TimelineClip], ids: &[String]) -> Vec<String> {
    use std::collections::HashSet;
    let mut id_set: HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
    let link_ids: HashSet<&str> = clips
        .iter()
        .filter(|c| id_set.contains(c.id.as_str()))
        .filter_map(|c| c.link_id.as_deref())
        .collect();
    for clip in clips {
        if let Some(link) = clip.link_id.as_deref() {
            if link_ids.contains(link) {
                id_set.insert(clip.id.as_str());
            }
        }
    }
    id_set.into_iter().map(String::from).collect()
}

/// Maximal erlaubtes Delta beim Trimmen einer Kante (Quelle + Nachbarn).
pub fn trim_range(
    clip: &TimelineClip,
    edge: TrimEdge,
    clips: &[TimelineClip],
    respect_neighbors: bool,
) -> (f64, f64) {
    match edge {
        TrimEdge::Start => {
            // Negativ = Kopf verlängern: nicht vor Quelle/Sequenzanfang.
            let mut lo = (-clip.src_in).max(-clip.start);
            let hi = clip.duration - MIN_CLIP_DURATION;
            if respect_neighbors {
                let mut prev_end: f64 = 0.0;
                for c in clips {
                    if c.track_id != clip.track_id || c.id == clip.id {
                        continue;
                    }
                    let c_end = c.end();
                    if c_end <= clip.start + EPS {
                        prev_end = prev_end.max(c_end);
                    }
                }
                lo = lo.max(prev_end - clip.start);
            }
            (lo, hi)
        }
        TrimEdge::End => {
            let clip_end = clip.end();
            let lo = -(clip.duration - MIN_CLIP_DURATION);
            let mut hi = if clip.src_duration.is_finite() {
                clip.src_duration - clip.src_in - clip.duration
            } else {
                f64::INFINITY
            };
            if respect_neighbors {
                let mut next_start = f64::INFINITY;
                for c in clips {
                    if c.track_id != clip.track_id || c.id == clip.id {
                        continue;
                    }
                    if c.start >= clip_end - EPS {
                        next_start = next_start.min(c.start);
                    }
                }
                hi = hi.min(next_start - clip_end);
            }
            (lo, hi)
        }
    }
}

fn apply_trim(clip: &TimelineClip, edge: TrimEdge, delta: f64) -> TimelineClip {
    let mut c = clip.clone();
    match edge {
        TrimEdge::Start => {
            c.start += delta;
            c.src_in += delta;
            c.duration -= delta;
        }
        TrimEdge::End => c.duration += delta,
    }
    c
}

/// Plant, wo Assets bei einem Drop/Einfügen landen — von Drop-Vorschau und
/// insert_assets gleichermaßen benutzt.
pub fn plan_asset_placements(
    timeline: &TimelineStore,
    assets: &[MediaAsset],
    asset_ids: &[String],
    at: f64,
    drop_track_id: Option<&str>,
) -> Vec<PlannedPlacement> {
    let drop_track = drop_track_id.and_then(|id| timeline.tracks.iter().find(|t| t.id == id));
    if drop_track.is_some_and(|t| t.locked) {
        return Vec::new();
    }

    let lane_for = |kind: TrackKind| -> Option<String> {
        if let Some(dt) = drop_track {
            if dt.kind == kind {
                return Some(dt.id.clone());
            }
        }
        let of_kind: Vec<&TimelineTrack> = timeline
            .tracks
            .iter()
            .filter(|t| t.kind == kind && !t.locked)
            .collect();
        let lane = match kind {
            TrackKind::Video => of_kind.last(),
            TrackKind::Audio => of_kind.first(),
        };
        lane.map(|t| t.id.clone())
    };

    let mut cursor = at.max(0.0);
    let mut placements = Vec::new();
    for asset_id in asset_ids {
        let Some(asset) = assets.iter().find(|a| &a.id == asset_id) else {
            continue;
        };
        let is_image = asset.kind == MediaKind::Image;
        let duration = if is_image {
            IMAGE_DEFAULT_DURATION
        } else {
            asset.info.duration_sec.max(MIN_CLIP_DURATION)
        };
        let src_duration = if is_image {
            f64::INFINITY
        } else {
            asset.info.duration_sec
        };
        let has_video = asset.kind != MediaKind::Audio;
        let has_audio = !is_image && !asset.info.audio.is_empty();
        let linked = has_video && has_audio;
        if has_video {
            placements.push(PlannedPlacement {
                asset_id: asset_id.clone(),
                kind: TrackKind::Video,
                track_id: lane_for(TrackKind::Video),
                start: cursor,
                duration,
                name: asset.name.clone(),
                src_duration,
                linked,
            });
        }
        if has_audio {
            placements.push(PlannedPlacement {
                asset_id: asset_id.clone(),
                kind: TrackKind::Audio,
                track_id: lane_for(TrackKind::Audio),
                start: cursor,
                duration,
                name: if linked {
                    format!("{} (Audio)", asset.name)
                } else {
                    asset.name.clone()
                },
                src_duration,
                linked,
            });
        }
        if has_video || has_audio {
            cursor += duration;
        }
    }
    placements
}

/// Sortierte, eindeutige Schnittpunkte (Clipgrenzen) der Sequenz.
pub fn edit_points(clips: &[TimelineClip]) -> Vec<f64> {
    let mut points: Vec<f64> = vec![0.0];
    for c in clips {
        points.push(c.start);
        points.push(c.end());
    }
    points.sort_by(|a, b| a.partial_cmp(b).unwrap());
    points.dedup_by(|a, b| (*a - *b).abs() < EPS);
    points
}

impl TimelineStore {
    fn push_history(&mut self) {
        self.past.push(Snapshot {
            tracks: self.tracks.clone(),
            clips: self.clips.clone(),
            master_gain_db: self.master_gain_db,
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
        tracks: Vec<TimelineTrack>,
        clips: Vec<TimelineClip>,
        playhead_sec: f64,
        in_point: Option<f64>,
        out_point: Option<f64>,
        zoom_px_per_sec: f64,
        snapping: bool,
        selected_clip_ids: Vec<String>,
        master_gain_db: f64,
    ) {
        let defaults = TimelineStore::default();
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
        self.clipboard.clear();
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
        self.past.clear();
        self.future.clear();
        self.revision += 1;
    }

    fn prune_selection(&mut self) {
        let existing: std::collections::HashSet<&str> =
            self.clips.iter().map(|c| c.id.as_str()).collect();
        self.selected_clip_ids.retain(|id| existing.contains(id.as_str()));
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

    pub fn step_playhead_frames(&mut self, frames: f64) {
        self.playhead_sec = (self.playhead_sec + frames / SEQUENCE_FPS).max(0.0);
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
    }

    pub fn clear_selection(&mut self) {
        self.selected_clip_ids.clear();
    }

    // -------------------------------------------------------------- Spuren

    pub fn add_track(&mut self, kind: TrackKind) -> String {
        let track = make_track(kind);
        let id = track.id.clone();
        self.push_history();
        // Neue Videospur oben auf den Video-Block, neue Audiospur unten.
        match kind {
            TrackKind::Video => self.tracks.insert(0, track),
            TrackKind::Audio => self.tracks.push(track),
        }
        id
    }

    pub fn remove_track(&mut self, track_id: &str) {
        if !self.tracks.iter().any(|t| t.id == track_id) {
            return;
        }
        self.push_history();
        self.tracks.retain(|t| t.id != track_id);
        self.clips.retain(|c| c.track_id != track_id);
        self.prune_selection();
    }

    pub fn toggle_track_flag(&mut self, track_id: &str, flag: TrackFlag) {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            match flag {
                TrackFlag::Muted => t.muted = !t.muted,
                TrackFlag::Solo => t.solo = !t.solo,
                TrackFlag::Locked => t.locked = !t.locked,
            }
            // Flags werden mitgespeichert → Projekt muss dirty werden
            // (bewusst ohne History-Snapshot, wie In-/Out-Punkte).
            self.revision += 1;
        }
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
                        self.tracks.insert(0, track);
                    }
                }
                TrackKind::Audio => {
                    if new_audio.is_none() {
                        let track = make_track(TrackKind::Audio);
                        new_audio = Some(track.id.clone());
                        self.tracks.push(track);
                    }
                }
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
            });
        }

        self.selected_clip_ids = inserted.iter().map(|c| c.id.clone()).collect();
        clips.extend(inserted);
        self.clips = clips;
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

        let video_tracks: Vec<&TimelineTrack> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .collect();
        let audio_tracks: Vec<&TimelineTrack> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .collect();
        let remap = |clip: &TimelineClip| -> String {
            if lane_offset == 0 {
                return clip.track_id.clone();
            }
            let lanes = match clip.kind {
                TrackKind::Video => &video_tracks,
                TrackKind::Audio => &audio_tracks,
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

        let video_tracks: Vec<&TimelineTrack> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .collect();
        let audio_tracks: Vec<&TimelineTrack> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .collect();
        let remap = |clip: &TimelineClip| -> String {
            if lane_offset == 0 {
                return clip.track_id.clone();
            }
            let lanes = match clip.kind {
                TrackKind::Video => &video_tracks,
                TrackKind::Audio => &audio_tracks,
            };
            let Some(idx) = lanes.iter().position(|t| t.id == clip.track_id) else {
                return clip.track_id.clone();
            };
            let new_idx = (idx as i32 + lane_offset).clamp(0, lanes.len() as i32 - 1) as usize;
            lanes[new_idx].id.clone()
        };

        // Kopien verknüpfter Paare teilen sich eine frische link_id.
        let mut new_link_ids: HashMap<String, String> = HashMap::new();
        let placed: Vec<TimelineClip> = sources
            .iter()
            .map(|c| {
                let mut p = c.clone();
                p.id = new_id();
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
        let mut d = delta;
        for clip in &targets {
            d = d.clamp(-clip.src_in, clip.src_duration - clip.src_in - clip.duration);
        }
        if d.abs() < EPS {
            return;
        }
        self.push_history();
        let target_ids: std::collections::HashSet<&str> =
            targets.iter().map(|c| c.id.as_str()).collect();
        for c in &mut self.clips {
            if target_ids.contains(c.id.as_str()) {
                c.src_in += d;
            }
        }
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
        if !self.clips.iter().any(|c| splittable(c)) {
            return;
        }

        self.push_history();
        // Rechte Hälften verknüpfter Clips bekommen eine gemeinsame neue link_id.
        let mut new_link_ids: HashMap<String, String> = HashMap::new();
        let mut clips: Vec<TimelineClip> = Vec::with_capacity(self.clips.len() + 4);
        let mut new_selection: Vec<String> = Vec::new();
        for c in &self.clips {
            if !splittable(c) {
                clips.push(c.clone());
                continue;
            }
            let left_len = time - c.start;
            let right_link = c.link_id.as_ref().map(|link| {
                new_link_ids
                    .entry(link.clone())
                    .or_insert_with(new_id)
                    .clone()
            });
            let mut right = c.clone();
            right.id = new_id();
            right.start = time;
            right.src_in = c.src_in + left_len;
            right.duration = c.duration - left_len;
            right.link_id = right_link;
            let mut left = c.clone();
            left.duration = left_len;
            if self.selected_clip_ids.contains(&c.id) {
                new_selection.push(left.id.clone());
                new_selection.push(right.id.clone());
            }
            clips.push(left);
            clips.push(right);
        }
        self.clips = clips;
        if !new_selection.is_empty() {
            self.selected_clip_ids = new_selection;
        } else {
            self.prune_selection();
        }
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
            // Lücke der gesamten Auswahl schließen (über alle ungesperrten Spuren).
            let gap_start = removed.iter().map(|c| c.start).fold(f64::INFINITY, f64::min);
            let gap_end = removed.iter().map(|c| c.end()).fold(0.0, f64::max);
            let gap = gap_end - gap_start;
            for c in &mut self.clips {
                if !locked.contains(&c.track_id) && c.start >= gap_end - EPS {
                    c.start = (c.start - gap).max(0.0);
                }
            }
        }
        self.prune_selection();
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

    pub fn remove_clips_for_assets(&mut self, asset_ids: &[String]) {
        let asset_set: std::collections::HashSet<&str> =
            asset_ids.iter().map(|s| s.as_str()).collect();
        if !self.clips.iter().any(|c| asset_set.contains(c.asset_id.as_str())) {
            return;
        }
        self.push_history();
        self.clips.retain(|c| !asset_set.contains(c.asset_id.as_str()));
        self.prune_selection();
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
        self.clipboard = selected
            .into_iter()
            .map(|c| {
                let mut copy = c.clone();
                copy.start -= base;
                copy
            })
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
                        TrackKind::Video => lanes.len() - 1,
                        TrackKind::Audio => 0,
                    });
                match c.kind {
                    // Array-Index 0 = oberste Videospur → „nach oben“ = rückwärts.
                    TrackKind::Video => lanes[..=desired].iter().rev().copied().collect(),
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
                    TrackKind::Video => tracks.insert(0, track),
                    TrackKind::Audio => tracks.push(track),
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
    }

    // ------------------------------------------------------------- Verlauf

    pub fn undo(&mut self) {
        let Some(prev) = self.past.pop() else { return };
        self.future.insert(
            0,
            Snapshot {
                tracks: std::mem::replace(&mut self.tracks, prev.tracks),
                clips: std::mem::replace(&mut self.clips, prev.clips),
                master_gain_db: std::mem::replace(&mut self.master_gain_db, prev.master_gain_db),
            },
        );
        self.prune_selection();
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
            master_gain_db: std::mem::replace(&mut self.master_gain_db, next.master_gain_db),
        });
        self.prune_selection();
        self.revision += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_clip(track_id: &str, kind: TrackKind, start: f64, duration: f64) -> TimelineClip {
        TimelineClip {
            id: new_id(),
            track_id: track_id.into(),
            asset_id: "asset".into(),
            name: "Clip".into(),
            kind,
            start,
            duration,
            src_in: 0.0,
            src_duration: f64::INFINITY,
            link_id: None,
            enabled: true,
            gain_db: 0.0,
        }
    }

    fn track_ids(store: &TimelineStore, kind: TrackKind) -> Vec<String> {
        store
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .map(|t| t.id.clone())
            .collect()
    }

    #[test]
    fn paste_keeps_original_track_when_free() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        store.clipboard = vec![test_clip(&v1, TrackKind::Video, 0.0, 4.0)];
        store.playhead_sec = 10.0;
        store.paste(None);
        assert_eq!(store.clips.len(), 1);
        assert_eq!(store.clips[0].track_id, v1);
        assert_eq!(store.clips[0].start, 10.0);
        assert_eq!(store.tracks.len(), 4);
    }

    #[test]
    fn paste_evades_to_track_above_instead_of_overwriting() {
        let mut store = TimelineStore::default();
        let videos = track_ids(&store, TrackKind::Video);
        let existing = test_clip(&videos[1], TrackKind::Video, 0.0, 8.0);
        let existing_id = existing.id.clone();
        store.clips.push(existing);
        store.clipboard = vec![test_clip(&videos[1], TrackKind::Video, 0.0, 4.0)];
        store.playhead_sec = 2.0;
        store.paste(None);
        // Bestehender Clip unangetastet, Kopie auf der Spur darüber.
        assert_eq!(store.clips.len(), 2);
        let original = store.clips.iter().find(|c| c.id == existing_id).unwrap();
        assert_eq!(original.duration, 8.0);
        let pasted = store.clips.iter().find(|c| c.id != existing_id).unwrap();
        assert_eq!(pasted.track_id, videos[0]);
        assert_eq!(pasted.start, 2.0);
        assert_eq!(store.tracks.len(), 4);
    }

    #[test]
    fn paste_creates_track_when_all_lanes_occupied() {
        let mut store = TimelineStore::default();
        let videos = track_ids(&store, TrackKind::Video);
        for id in &videos {
            store.clips.push(test_clip(id, TrackKind::Video, 0.0, 8.0));
        }
        store.clipboard = vec![test_clip(&videos[1], TrackKind::Video, 0.0, 4.0)];
        store.playhead_sec = 2.0;
        store.paste(None);
        assert_eq!(store.clips.len(), 3);
        // Neue Videospur entsteht oben (Index 0) und trägt die Kopie.
        assert_eq!(store.tracks.len(), 5);
        assert_eq!(store.tracks[0].kind, TrackKind::Video);
        let pasted = store.clips.last().unwrap();
        assert_eq!(pasted.track_id, store.tracks[0].id);
    }

    #[test]
    fn paste_audio_evades_downwards() {
        let mut store = TimelineStore::default();
        let audios = track_ids(&store, TrackKind::Audio);
        store.clips.push(test_clip(&audios[0], TrackKind::Audio, 0.0, 8.0));
        store.clipboard = vec![test_clip(&audios[0], TrackKind::Audio, 0.0, 4.0)];
        store.playhead_sec = 0.0;
        store.paste(None);
        assert_eq!(store.clips.len(), 2);
        assert_eq!(store.clips.last().unwrap().track_id, audios[1]);
    }

    #[test]
    fn duplicate_clips_keeps_originals_and_remaps_links() {
        let mut store = TimelineStore::default();
        let videos = track_ids(&store, TrackKind::Video);
        let audios = track_ids(&store, TrackKind::Audio);
        let link = new_id();
        let mut v = test_clip(&videos[1], TrackKind::Video, 0.0, 4.0);
        v.link_id = Some(link.clone());
        let mut a = test_clip(&audios[0], TrackKind::Audio, 0.0, 4.0);
        a.link_id = Some(link.clone());
        let ids = vec![v.id.clone(), a.id.clone()];
        store.clips.push(v);
        store.clips.push(a);

        store.duplicate_clips(&ids, 6.0, 0);

        assert_eq!(store.clips.len(), 4);
        // Originale unverändert bei 0 mit alter link_id.
        for id in &ids {
            let c = store.clips.iter().find(|c| &c.id == id).unwrap();
            assert_eq!(c.start, 0.0);
            assert_eq!(c.link_id.as_deref(), Some(link.as_str()));
        }
        // Kopien bei 6, selektiert, mit gemeinsamer neuer link_id.
        let copies: Vec<&TimelineClip> =
            store.clips.iter().filter(|c| !ids.contains(&c.id)).collect();
        assert_eq!(copies.len(), 2);
        for c in &copies {
            assert_eq!(c.start, 6.0);
            assert!(store.selected_clip_ids.contains(&c.id));
        }
        assert_eq!(copies[0].link_id, copies[1].link_id);
        assert_ne!(copies[0].link_id.as_deref(), Some(link.as_str()));
    }

    #[test]
    fn duplicate_without_movement_is_a_noop() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let clip = test_clip(&v1, TrackKind::Video, 0.0, 4.0);
        let id = clip.id.clone();
        store.clips.push(clip);
        store.duplicate_clips(&[id], 0.0, 0);
        assert_eq!(store.clips.len(), 1);
        assert!(!store.can_undo());
    }
}
