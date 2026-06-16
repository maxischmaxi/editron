//! Die Timeline: vertikale Werkzeugleiste, Timecode-Kopfzeile, Lineal mit
//! Loop-Bereich, editierbare Spuren mit Clips, Playhead, Snapping mit
//! Hilfslinie, Marquee, Pan/Zoom, Drag&Drop aus dem Medien-Browser und
//! Kontextmenüs. Alle Editier-Operationen laufen über den TimelineStore
//! (mit Undo); während einer Drag-Geste rendert das Panel eine Vorschau.

use crate::core::animation::{Keyframe, KF_TIME_EPS};
use crate::core::timecode::{format_duration, format_sequence_timecode};
use crate::core::timeline::{
    apply_trim, expand_links, plan_asset_placements, sequence_end, track_name, trim_range,
    PlannedPlacement, TimelineClip, TimelineTrack, TrackAutoParam, TrackFlag, TrackKind, TrimEdge,
    MAX_TRACK_HEIGHT, MIN_CLIP_DURATION, MIN_TRACK_HEIGHT, SelectMode,
};
use std::collections::HashSet;
use crate::core::transitions::{
    self, Transition, TransitionAlignment, TransitionDirection, TransitionKind,
    DEFAULT_TRANSITION_DURATION,
};
use crate::ui::widgets::text_input::TextInputState;
use crate::overlays::context_menu::{CustomAction, MenuEntry, MenuItem};
use crate::overlays::marker_dialog::marker_color;
use crate::core::marker::MarkerScope;
use crate::stores::{DialogId, MarkerEditTarget};
use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::{v2, Rect};
use crate::ui::widgets::scroll::scrollbar;
use crate::ui::widgets::IconButton;
use crate::ui::{DragPayload, FontKind, Ui};
use raylib::consts::MouseCursor;
use raylib::math::Vector2;

const TRACK_HEADER_W: f32 = 144.0; // Platz für Patch-Chip + Target + Toggles
const TOOLBAR_W: f32 = 36.0; // w-9
const TABS_H: f32 = 28.0; // h-7 — Sequenz-Tab-Leiste (über der Kopfzeile)
const HEADER_H: f32 = 36.0; // h-9
const RULER_H: f32 = 28.0; // h-7
const VIDEO_H: f32 = 48.0; // h-12
const AUDIO_H: f32 = 40.0; // h-10
const SUBTITLE_H: f32 = 28.0; // h-7 — kompakte Untertitel-Spuren
/// Greifzone (Höhe) des Sash am Spurkopf-Unterrand zum Verstellen der Spurhöhe.
const TRACK_SASH_H: f32 = 5.0;
/// Höhe des Marker-Bandes am unteren Linealrand.
const MARKER_BAND_H: f32 = 11.0;
/// Einrast-Radius in Pixeln (zoomunabhängig).
const SNAP_PX: f64 = 8.0;
/// Kantenzone der Clips für Trim-Erkennung in Pixeln.
const EDGE_PX: f32 = 7.0;
const MAJOR_TICK_MIN_PX: f64 = 80.0;
const TICK_STEPS: [f64; 12] = [
    1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0,
];
const MINOR_PER_MAJOR: usize = 5;
const EPS: f64 = 1e-6;

/// Werkzeuge mit Lucide-Icons.
const TOOLS: [(&str, &str); 8] = [
    ("select", "mouse-pointer"),
    ("razor", "scissors"),
    ("ripple", "chevrons-left-right"),
    ("rolling", "arrow-left-right"),
    ("slip", "move-horizontal"),
    ("slide", "stretch-horizontal"),
    ("hand", "hand"),
    ("zoom", "zoom-in"),
];

enum TlDrag {
    Move {
        clip_ids: Vec<String>,
        grab_id: String,
        origin_x: f32,
        delta_sec: f64,
        lane_offset: i32,
        snap_time: Option<f64>,
        /// Alt+Drag: Originale bleiben liegen, an der Zielposition entstehen
        /// Kopien (live togglebar — der Alt-Zustand beim Loslassen zählt).
        duplicate: bool,
    },
    Trim {
        clip_id: String,
        edge: TrimEdge,
        ripple: bool,
        origin_x: f32,
        delta_sec: f64,
        snap_time: Option<f64>,
    },
    /// Übergangs-Dauer per Kanten-Drag trimmen (Vorschau, Commit beim Drop).
    TransTrim {
        id: String,
        edge: TrimEdge,
        preview: f64,
    },
    Roll {
        left_id: String,
        right_id: String,
        origin_x: f32,
        delta_sec: f64,
    },
    Slip {
        clip_id: String,
        origin_x: f32,
        delta_sec: f64,
    },
    Slide {
        clip_id: String,
        origin_x: f32,
        delta_sec: f64,
    },
    Marquee {
        origin_t: f64,
        origin_y: f32,
        t: f64,
        y: f32,
        additive: bool,
        base: Vec<String>,
        moved: bool,
    },
    Pan {
        origin_x: f32,
        origin_y: f32,
        start_left: f32,
        start_top: f32,
    },
}

enum RulerDrag {
    Scrub,
    Range { origin_t: f64 },
    Edge { is_in: bool },
    /// Sequenz-Marker verschieben (Differenz Mausklick ↔ Markerzeit).
    Marker { id: String, grab_dt: f64, began: bool },
}

pub struct TimelinePanel {
    scroll_x: f32,
    scroll_y: f32,
    drag: Option<TlDrag>,
    ruler_drag: Option<RulerDrag>,
    snap_targets: Vec<f64>,
    zoom_anchor: Option<(f64, f32)>,
    prev_zoom: f64,
    sb_drag_v: Option<f32>,
    sb_drag_h: Option<f32>,
    /// Drop-Vorschau im aktuellen Frame (Platzierungen + Snap-Linie).
    drop_preview: Option<(Vec<PlannedPlacement>, Option<f64>)>,
    /// Offene Dauer-Eingabe eines Übergangs (Doppelklick/Kontextmenü).
    trans_editor: Option<(String, TextInputState)>,
    /// Laufendes Verschieben eines Automations-Punkts (Spur-Gummiband).
    auto_drag: Option<AutoDrag>,
    /// Audio-Spuren, deren Gummiband Pan statt Lautstärke bearbeitet (Vol/Pan-
    /// Umschalter im Lane-Eck).
    auto_pan: HashSet<String>,
    /// Inline-Umbenennen eines Sequenz-Tabs (Eingabezustand; das Ziel steht in
    /// `app.app.rename_sequence`).
    tab_rename: Option<TextInputState>,
    /// Laufendes Verschieben eines Sequenz-Tabs (Reihenfolge per Drag).
    tab_drag: Option<TabDrag>,
    /// Laufendes Verstellen einer Spurhöhe (Sash-Drag am Spurkopf-Unterrand).
    track_resize: Option<TrackResizeDrag>,
}

/// Laufendes Ziehen eines Sequenz-Tabs (Reihenfolge umsortieren).
struct TabDrag {
    id: String,
    origin_x: f32,
    /// Erst nach kleiner Bewegung „echter“ Drag (sonst bleibt es ein Klick).
    moved: bool,
}

/// Laufendes Verstellen einer Spurhöhe per Sash-Drag am unteren Spurkopf-Rand.
struct TrackResizeDrag {
    track_id: String,
    /// Mausposition (y) und Spurhöhe bei Gestenbeginn — die Höhe folgt dann
    /// `start_h + (maus_y − start_mouse_y)`.
    start_mouse_y: f32,
    start_h: f32,
}

/// Laufendes Ziehen eines Automations-Punkts auf einer Audiospur.
struct AutoDrag {
    track_id: String,
    param: TrackAutoParam,
    /// Ursprüngliche Zeit des gezogenen Punkts (Identität in `orig_keys`).
    orig_t: f64,
    /// Kurve bei Gestenbeginn (alle anderen Punkte bleiben stehen).
    orig_keys: Vec<Keyframe>,
    /// Undo-Snapshot bereits angelegt? (Add legt ihn vorab an.)
    pushed: bool,
}

impl Default for TimelinePanel {
    fn default() -> Self {
        TimelinePanel {
            scroll_x: 0.0,
            scroll_y: 0.0,
            drag: None,
            ruler_drag: None,
            snap_targets: Vec::new(),
            zoom_anchor: None,
            prev_zoom: 40.0,
            sb_drag_v: None,
            sb_drag_h: None,
            drop_preview: None,
            trans_editor: None,
            auto_drag: None,
            auto_pan: HashSet::new(),
            tab_rename: None,
            tab_drag: None,
            track_resize: None,
        }
    }
}

/// Wertebereich der Automations-Kurve je Parameter (für die y-Abbildung):
/// Lautstärke ±18 dB Offset, Pan ±1.
fn auto_range(param: TrackAutoParam) -> f64 {
    match param {
        TrackAutoParam::Volume => 18.0,
        TrackAutoParam::Pan => 1.0,
    }
}

/// Kompakte Standardhöhe einer Spurart (gilt, solange keine manuelle Höhe
/// per Sash-Drag gesetzt ist).
fn default_track_height(kind: TrackKind) -> f32 {
    match kind {
        TrackKind::Video => VIDEO_H,
        TrackKind::Audio => AUDIO_H,
        TrackKind::Subtitle => SUBTITLE_H,
    }
}

fn track_height(track: &TimelineTrack) -> f32 {
    track
        .height
        .map(|h| h.clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT))
        .unwrap_or_else(|| default_track_height(track.kind))
}

/// Tooltip-Text mit Shortcut der aktiven Keymap, z. B. "Rasierklinge (C)".
fn command_tooltip(app: &AppState, label: &str, command: &str) -> String {
    match app.keymap.shortcut_for(command) {
        Some(keys) => format!("{label} ({keys})"),
        None => label.to_string(),
    }
}

/// Clip unter dem Mauszeiger samt Spur und Zeilen-Rect; sehr schmale Clips
/// bekommen eine 3-px-Mindesttrefferbreite.
fn clip_under_mouse(
    preview: &[TimelineClip],
    lane_rects: &[(TimelineTrack, Rect)],
    mouse_y: f32,
    t: f64,
    zoom: f64,
) -> Option<(TimelineClip, TimelineTrack, Rect)> {
    lane_rects.iter().find_map(|(track, lane)| {
        if mouse_y < lane.y || mouse_y >= lane.bottom() {
            return None;
        }
        preview
            .iter()
            .find(|c| {
                c.track_id == track.id && t >= c.start && t <= c.end().max(c.start + 3.0 / zoom)
            })
            .map(|c| (c.clone(), track.clone(), *lane))
    })
}

impl TimelinePanel {
    fn collect_snap_targets(&mut self, app: &AppState, exclude: &[String]) {
        let mut targets = vec![0.0, app.timeline.playhead_sec];
        for c in &app.timeline.clips {
            if exclude.contains(&c.id) {
                continue;
            }
            targets.push(c.start);
            targets.push(c.end());
        }
        // Sequenz-Marker als Snap-Ziele (Beat-genaues Schneiden zu Musik);
        // Bereichsmarker liefern zusätzlich ihre Endkante. Aufs Frame-Raster
        // gerastert, weil der frame-genaue Edit-Pfad die Frame-Rundung bei
        // aktivem Snap überspringt (er vertraut darauf, dass Snap-Ziele selbst
        // frame-aligned sind) — Marker können aber sub-frame gesetzt sein.
        for m in &app.timeline.markers {
            targets.push(app.timeline.snap_to_frame(m.time));
            if m.duration > 0.0 {
                targets.push(app.timeline.snap_to_frame(m.end()));
            }
        }
        self.snap_targets = targets;
    }

    /// Verschiebt delta so, dass eine der Kanten auf ein Snap-Ziel fällt.
    fn snap_adjust(&self, app: &AppState, edges: &[f64], delta: f64) -> (f64, Option<f64>) {
        if !app.timeline.snapping {
            return (delta, None);
        }
        let threshold = SNAP_PX / app.timeline.zoom_px_per_sec;
        let mut best: Option<(f64, f64, f64)> = None; // (dist, delta, time)
        for &edge in edges {
            let moved = edge + delta;
            for &t in &self.snap_targets {
                let dist = (moved - t).abs();
                if dist <= threshold && best.is_none_or(|(d, _, _)| dist < d) {
                    best = Some((dist, delta + (t - moved), t));
                }
            }
        }
        match best {
            Some((_, d, t)) => (d, Some(t)),
            None => (delta, None),
        }
    }

    /// Vorschau-Clips während eines Drags (Store bleibt bis zum Drop unverändert).
    fn preview_clips(&self, app: &AppState) -> Vec<TimelineClip> {
        let clips = &app.timeline.clips;
        let Some(drag) = &self.drag else {
            return clips.clone();
        };
        match drag {
            TlDrag::Move {
                clip_ids,
                delta_sec,
                lane_offset,
                duplicate,
                ..
            } => {
                let v_lanes: Vec<&TimelineTrack> = app
                    .timeline
                    .tracks
                    .iter()
                    .filter(|t| t.kind == TrackKind::Video)
                    .collect();
                let a_lanes: Vec<&TimelineTrack> = app
                    .timeline
                    .tracks
                    .iter()
                    .filter(|t| t.kind == TrackKind::Audio)
                    .collect();
                let s_lanes: Vec<&TimelineTrack> = app
                    .timeline
                    .tracks
                    .iter()
                    .filter(|t| t.kind == TrackKind::Subtitle)
                    .collect();
                let mut out: Vec<TimelineClip> = Vec::with_capacity(clips.len() + 4);
                let mut moved: Vec<TimelineClip> = Vec::new();
                for c in clips {
                    if !clip_ids.contains(&c.id) {
                        out.push(c.clone());
                        continue;
                    }
                    let mut m = c.clone();
                    if *lane_offset != 0 {
                        let lanes = match c.kind {
                            TrackKind::Video => &v_lanes,
                            TrackKind::Audio => &a_lanes,
                            TrackKind::Subtitle => &s_lanes,
                        };
                        if let Some(idx) = lanes.iter().position(|t| t.id == c.track_id) {
                            let ni = (idx as i32 + lane_offset)
                                .clamp(0, lanes.len() as i32 - 1)
                                as usize;
                            m.track_id = lanes[ni].id.clone();
                        }
                    }
                    m.start = c.start + delta_sec;
                    if *duplicate {
                        // Original bleibt liegen; eigene Vorschau-ID, damit nur
                        // die Kopie das Auswahl-Outline trägt.
                        let mut orig = c.clone();
                        orig.id.push_str("~src");
                        out.push(orig);
                    }
                    moved.push(m);
                }
                out.extend(moved);
                out
            }
            TlDrag::Trim {
                clip_id,
                edge,
                delta_sec,
                ..
            } => {
                let ids = expand_links(clips, &[clip_id.clone()]);
                clips
                    .iter()
                    .map(|c| {
                        if ids.contains(&c.id) {
                            apply_trim(c, *edge, *delta_sec)
                        } else {
                            c.clone()
                        }
                    })
                    .collect()
            }
            TlDrag::Roll {
                left_id,
                right_id,
                delta_sec,
                ..
            } => clips
                .iter()
                .map(|c| {
                    if c.id == *left_id {
                        apply_trim(c, TrimEdge::End, *delta_sec)
                    } else if c.id == *right_id {
                        apply_trim(c, TrimEdge::Start, *delta_sec)
                    } else {
                        c.clone()
                    }
                })
                .collect(),
            TlDrag::Slip {
                clip_id, delta_sec, ..
            } => {
                let ids = expand_links(clips, &[clip_id.clone()]);
                clips
                    .iter()
                    .map(|c| {
                        if ids.contains(&c.id) && c.src_duration.is_finite() {
                            let mut m = c.clone();
                            m.src_in = c.src_in + delta_sec * c.eff_speed();
                            m
                        } else {
                            c.clone()
                        }
                    })
                    .collect()
            }
            TlDrag::Slide {
                clip_id, delta_sec, ..
            } => {
                let ids = expand_links(clips, &[clip_id.clone()]);
                clips
                    .iter()
                    .map(|c| {
                        if ids.contains(&c.id) {
                            let mut m = c.clone();
                            m.start = c.start + delta_sec;
                            m
                        } else {
                            c.clone()
                        }
                    })
                    .collect()
            }
            _ => clips.clone(),
        }
    }
}

impl Panel for TimelinePanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);
        let mut area = rect;

        // Dauer-Eingabe aus dem Kontextmenü übernehmen.
        if let Some(id) = app.app.edit_transition_duration.take() {
            self.open_trans_editor(ui, app, &id);
        }

        // ---------------- Werkzeugleiste (vertikal, w-9, border-r) ----------
        let toolbar = area.cut_left(TOOLBAR_W);
        ui.vline(toolbar.right() - 1.0, toolbar.y, toolbar.h, theme::LINE);
        let mut ty = toolbar.y + 8.0; // py-2
        for (tool, icon) in TOOLS {
            let btn = Rect::new(toolbar.x + (TOOLBAR_W - 28.0) / 2.0, ty, 28.0, 28.0);
            let label = crate::stores::tool_label(tool);
            let tip = command_tooltip(app, label, &format!("tools.{tool}"));
            if IconButton::new(icon)
                .active(app.app.active_tool == tool)
                .tooltip(&tip)
                .show(ui, ("tl.tool", tool), btn)
                .clicked
            {
                ui.run_command(format!("tools.{tool}"));
            }
            ty += 28.0 + 4.0; // gap-1
        }

        // ---------------- Sequenz-Tabs (über der Kopfzeile, wie Premiere) ----
        let tabbar = area.cut_top(TABS_H);
        self.render_sequence_tabs(ui, app, tabbar);

        // ---------------- Kopfzeile (h-9: Timecode + Snapping/Zoom) ---------
        let header = area.cut_top(HEADER_H);
        ui.hline(header.x, header.bottom() - 1.0, header.w, theme::LINE);
        let header_inner = header.inset_xy(12.0, 0.0);
        let tc = format_sequence_timecode(app.timeline.playhead_sec, &app.timeline.settings);
        ui.text_left(&tc, header_inner, theme::ACCENT, FontKind::Sans16); // font-mono text-base
        // rechts: Magnet | Divider | ZoomOut ZoomIn Fit
        let mut hx = header_inner.right();
        let buttons: [(&str, &str, &str, bool); 3] = [
            ("timeline.zoomFit", "maximize-2", "An Sequenz anpassen", false),
            ("timeline.zoomIn", "zoom-in", "Timeline vergrößern", false),
            ("timeline.zoomOut", "zoom-out", "Timeline verkleinern", false),
        ];
        for (cmd, icon, tip, _) in buttons {
            hx -= 28.0;
            let btn = Rect::new(hx, header.y + (HEADER_H - 28.0) / 2.0, 28.0, 28.0);
            let tip = command_tooltip(app, tip, cmd);
            if IconButton::new(icon)
                .tooltip(&tip)
                .show(ui, ("tl.h", cmd), btn)
                .clicked
            {
                if cmd == "timeline.zoomIn" || cmd == "timeline.zoomOut" {
                    // Zoom-Buttons: Mitte stabil halten (kein Anker)
                }
                ui.run_command(cmd);
            }
            hx -= 4.0;
        }
        hx -= 4.0 + 1.0;
        ui.fill(
            Rect::new(hx + 1.0, header.y + (HEADER_H - 16.0) / 2.0, 1.0, 16.0),
            theme::LINE,
        );
        hx -= 4.0 + 28.0;
        let snap_btn = Rect::new(hx, header.y + (HEADER_H - 28.0) / 2.0, 28.0, 28.0);
        let snap_tip = command_tooltip(app, "Einrasten (Snapping)", "timeline.toggleSnapping");
        if IconButton::new("magnet")
            .active(app.timeline.snapping)
            .tooltip(&snap_tip)
            .show(ui, "tl.snap", snap_btn)
            .clicked
        {
            ui.run_command("timeline.toggleSnapping");
        }

        // ---------------- Scroll-Bereich -------------------------------------
        let outer = area;
        ui.fill(outer, theme::SURFACE_0);

        let tracks = app.timeline.tracks.clone();
        let zoom = app.timeline.zoom_px_per_sec;
        let seq_end = sequence_end(&app.timeline.clips);
        let content_dur = (seq_end + 60.0).max(120.0);
        let content_w_px = (content_dur * zoom).ceil() as f32;
        let lanes_h: f32 = tracks.iter().map(track_height).sum();

        // Drop-Vorschau: Zeilen für automatisch anzulegende Spuren
        let new_video_rows = self
            .drop_preview
            .as_ref()
            .map(|(p, _)| p.iter().any(|p| p.track_id.is_none() && p.kind == TrackKind::Video))
            .unwrap_or(false);
        let new_audio_rows = self
            .drop_preview
            .as_ref()
            .map(|(p, _)| p.iter().any(|p| p.track_id.is_none() && p.kind == TrackKind::Audio))
            .unwrap_or(false);
        let extra_h = if new_video_rows { VIDEO_H } else { 0.0 }
            + if new_audio_rows { AUDIO_H } else { 0.0 };

        let content_w = TRACK_HEADER_W + content_w_px;
        let content_h = RULER_H + lanes_h + extra_h;

        let need_v = content_h > outer.h;
        let mut viewport = outer;
        if need_v {
            viewport.w -= theme::SCROLLBAR_W;
        }
        let need_h = content_w > viewport.w;
        if need_h {
            viewport.h -= theme::SCROLLBAR_W;
        }

        app.timeline.viewport_w = (viewport.w - TRACK_HEADER_W).max(0.0) as f64;

        // Mausrad: Shift/Mod = Zoom um Cursor, Alt = horizontal, sonst nativ.
        if ui.mouse_in(outer) && self.drag.is_none() {
            let wheel = ui.input.wheel;
            if wheel.y != 0.0 || wheel.x != 0.0 {
                if ui.input.shift || ui.input.ctrl || ui.input.meta {
                    let raw = if wheel.y != 0.0 { wheel.y } else { wheel.x } as f64;
                    let time = ((ui.input.mouse.x - viewport.x - TRACK_HEADER_W + self.scroll_x)
                        / zoom as f32) as f64;
                    self.zoom_anchor = Some((time, ui.input.mouse.x));
                    // Browser-Wheel ≈ 120 px/Tick → exp(-raw*0.0018) auf Pixel
                    app.timeline.set_zoom(zoom * (raw * 120.0 * 0.0018).exp());
                } else if ui.input.alt {
                    self.scroll_x += -wheel.y * 48.0;
                } else {
                    self.scroll_y += -wheel.y * 48.0;
                    self.scroll_x += -wheel.x * 48.0;
                }
            }
        }

        // Zoom-Anker stabil halten (auch bei Zoom über Commands/Buttons).
        let zoom_now = app.timeline.zoom_px_per_sec;
        if (zoom_now - self.prev_zoom).abs() > f64::EPSILON {
            let prev = self.prev_zoom;
            self.prev_zoom = zoom_now;
            if let Some((time, screen_x)) = self.zoom_anchor.take() {
                self.scroll_x =
                    (time * zoom_now) as f32 - (screen_x - viewport.x - TRACK_HEADER_W);
            } else {
                let lane_view_w = viewport.w - TRACK_HEADER_W;
                let center_time = (self.scroll_x + lane_view_w / 2.0) / prev as f32;
                self.scroll_x = center_time * zoom_now as f32 - lane_view_w / 2.0;
            }
        }
        self.scroll_x = self.scroll_x.clamp(0.0, (content_w - viewport.w).max(0.0));
        self.scroll_y = self.scroll_y.clamp(0.0, (content_h - viewport.h).max(0.0));

        let zoom = app.timeline.zoom_px_per_sec;
        let zoom_f = zoom as f32;

        // Geometrie-Helfer
        let lane_x0 = viewport.x + TRACK_HEADER_W; // linke Kante des Zeitbereichs
        let scroll_x = self.scroll_x;
        let scroll_y = self.scroll_y;
        let pointer_time = move |x: f32| -> f64 { ((x - lane_x0 + scroll_x) / zoom_f) as f64 };
        let time_x = move |t: f64| -> f32 { lane_x0 + (t * zoom) as f32 - scroll_x };
        let lanes_top = viewport.y + RULER_H - scroll_y;
        let lane_y = move |y: f32| -> f32 { y - lanes_top };
        let track_at_y = {
            let tracks = tracks.clone();
            move |y: f32| -> Option<TimelineTrack> {
                let mut rel = y - lanes_top;
                if rel < 0.0 {
                    return None;
                }
                for t in &tracks {
                    let h = track_height(t);
                    if rel < h {
                        return Some(t.clone());
                    }
                    rel -= h;
                }
                None
            }
        };

        ui.push_clip(viewport);

        // ---------------- Aktiven Drag fortschreiben ------------------------
        self.update_drag(ui, app, pointer_time, lane_y, &track_at_y);

        let preview = self.preview_clips(app);
        let selected = app.timeline.selected_clip_ids.clone();
        let assets = app.media.assets.clone();
        let mouse = ui.input.mouse;
        let tool: &str = app.app.active_tool;

        // Cursor je Werkzeug
        if ui.mouse_in(viewport) {
            match tool {
                "razor" => ui.want_cursor(MouseCursor::MOUSE_CURSOR_CROSSHAIR),
                "hand" => ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND),
                _ => {}
            }
        }

        // ---------------- Spuren + Clips -------------------------------------
        let mut row_y = viewport.y + RULER_H - scroll_y;
        let mut lane_rects: Vec<(TimelineTrack, Rect)> = Vec::new();
        // Automations-Geste hat in diesem Frame den Klick verbraucht?
        let mut auto_input = self.auto_drag.is_some();
        for track in &tracks {
            let h = track_height(track);
            let row = Rect::new(viewport.x, row_y, viewport.w.max(content_w), h);
            let lane = Rect::new(lane_x0 - scroll_x, row_y, content_w_px, h);
            if track.locked {
                ui.fill(
                    Rect::new(viewport.x, row_y, viewport.w, h),
                    theme::with_alpha(theme::SURFACE_2, 102),
                );
            }
            ui.hline(viewport.x, row.bottom() - 1.0, viewport.w, theme::LINE);

            // Clips dieser Spur
            for clip in preview.iter().filter(|c| c.track_id == track.id) {
                let asset = assets.iter().find(|a| a.id == clip.asset_id);
                self.draw_clip(
                    ui,
                    app,
                    services,
                    clip,
                    asset,
                    Rect::new(
                        time_x(clip.start),
                        row_y + 2.0,
                        ((clip.duration * zoom) as f32).max(3.0),
                        h - 4.0,
                    ),
                    selected.contains(&clip.id),
                    track.locked,
                );
            }
            // Spur-Automation (Lautstärke/Pan) als Premiere-artiges Gummiband
            // direkt auf der Audiospur: Mod+Klick setzt Punkte, Ziehen
            // verschiebt sie, Rechts-/Alt-Klick löscht.
            if track.kind == TrackKind::Audio {
                let lane_clip =
                    Rect::new(lane_x0, row_y, (viewport.right() - lane_x0).max(0.0), h);
                if self.handle_track_automation(ui, app, track, lane_clip, zoom, lane_x0, scroll_x)
                {
                    auto_input = true;
                }
            }
            lane_rects.push((track.clone(), Rect::new(lane.x, row_y, lane.w, h)));
            row_y += h;
        }

        // ---------------- Übergangs-Bänder über den Clips --------------------
        // (id, Band-Rect, gesperrt) — auch Treffer-Liste für die Interaktion.
        let mut trans_rects: Vec<(String, Rect, bool)> = Vec::new();
        {
            let selected_trs = app.timeline.selected_transition_ids.clone();
            let trs: Vec<Transition> = app.timeline.transitions.clone();
            for tr in &trs {
                let Some(track_id) = app.timeline.transition_track_id(tr) else {
                    continue;
                };
                let Some((track, lane)) = lane_rects.iter().find(|(t, _)| t.id == track_id) else {
                    continue;
                };
                let Some((mut w0, mut w1)) = app.timeline.transition_window(tr) else {
                    continue;
                };
                // Laufende Trim-Geste: Band mit Vorschau-Dauer zeichnen.
                if let Some(TlDrag::TransTrim { id, preview, .. }) = &self.drag {
                    if id == &tr.id {
                        let (from, to) = transitions::resolve_clips(&app.timeline.clips, tr);
                        if let Some(w) = transitions::window(from, to, tr.alignment, *preview) {
                            (w0, w1) = w;
                        }
                    }
                }
                let band = Rect::new(
                    time_x(w0),
                    lane.y + 2.0,
                    (((w1 - w0) * zoom) as f32).max(6.0),
                    lane.h - 4.0,
                );
                draw_transition_band(
                    ui,
                    tr,
                    band,
                    selected_trs.contains(&tr.id),
                    track.locked,
                );
                trans_rects.push((tr.id.clone(), band, track.locked));
            }
        }

        // Drop-Vorschau in bestehenden Spuren + neue Spurzeilen
        if let Some((placements, _)) = &self.drop_preview {
            for p in placements {
                let rect_for = |track_id: &str| -> Option<Rect> {
                    lane_rects
                        .iter()
                        .find(|(t, _)| t.id == track_id)
                        .map(|(_, r)| *r)
                };
                if let Some(track_id) = &p.track_id {
                    if let Some(r) = rect_for(track_id) {
                        let prev = Rect::new(
                            time_x(p.start),
                            r.y + 4.0,
                            ((p.duration * zoom) as f32).max(3.0),
                            r.h - 8.0,
                        );
                        ui.fill_rounded(prev, theme::RADIUS_SM, theme::with_alpha(theme::ACCENT, 26));
                        ui.stroke_rounded(prev, theme::RADIUS_SM, 1.0, theme::ACCENT);
                    }
                }
            }
            // Neue Spuren (Video oben konzeptionell, hier unten angefügt wie Vorschau)
            for (is_video, label_icon) in [(true, "film"), (false, "music")] {
                let any = placements
                    .iter()
                    .any(|p| p.track_id.is_none() && (p.kind == TrackKind::Video) == is_video);
                if !any {
                    continue;
                }
                let h = if is_video { VIDEO_H } else { AUDIO_H };
                let row = Rect::new(viewport.x, row_y, viewport.w, h);
                ui.hline(row.x, row.bottom() - 1.0, row.w, theme::with_alpha(theme::ACCENT, 153));
                // Header-Zelle "Neue Spur"
                let head = Rect::new(viewport.x, row_y, TRACK_HEADER_W, h);
                ui.fill(head, theme::SURFACE_2);
                ui.vline(head.right() - 1.0, head.y, head.h, theme::LINE);
                let mut hi = head.inset_xy(8.0, 0.0);
                let ic = hi.cut_left(14.0);
                ui.icon(label_icon, ic, 14.0, theme::ACCENT);
                hi.cut_left(4.0);
                ui.text_left("Neue Spur", hi, theme::ACCENT, FontKind::Sans12);
                for p in placements
                    .iter()
                    .filter(|p| p.track_id.is_none() && (p.kind == TrackKind::Video) == is_video)
                {
                    let prev = Rect::new(
                        time_x(p.start),
                        row_y + 4.0,
                        ((p.duration * zoom) as f32).max(3.0),
                        h - 8.0,
                    );
                    ui.fill_rounded(prev, theme::RADIUS_SM, theme::with_alpha(theme::ACCENT, 26));
                    ui.stroke_rounded(prev, theme::RADIUS_SM, 1.0, theme::ACCENT);
                }
                row_y += h;
            }
        }

        // Hinweis bei leerer Sequenz
        if app.timeline.clips.is_empty() && self.drop_preview.is_none() {
            let hint = Rect::new(
                viewport.x + TRACK_HEADER_W,
                viewport.y + RULER_H,
                viewport.w - TRACK_HEADER_W,
                lanes_h,
            );
            ui.text_centered(
                "Medien aus dem Medien-Browser hierher ziehen",
                hint,
                theme::TEXT_3,
                FontKind::Sans12,
            );
        }

        // ---------------- Interaktion: Klicks auf Clips/Spuren ---------------
        if self.drag.is_none()
            && self.auto_drag.is_none()
            && !auto_input
            && self.ruler_drag.is_none()
            && self.trans_editor.is_none()
            && self.track_resize.is_none()
            && ui.mouse_in(viewport)
            && mouse.y >= viewport.y + RULER_H - scroll_y
        {
            self.handle_lane_input(
                ui,
                app,
                &preview,
                &lane_rects,
                &trans_rects,
                pointer_time,
                lane_y,
                &track_at_y,
                viewport,
            );
        }

        // ---------------- Lineal ---------------------------------------------
        let ruler = Rect::new(viewport.x, viewport.y - scroll_y, viewport.w, RULER_H);
        self.draw_ruler(ui, app, ruler, time_x, pointer_time, viewport);

        // ---------------- Track-Header (sticky left) --------------------------
        let mut head_y = viewport.y + RULER_H - scroll_y;
        for track in &tracks {
            let h = track_height(track);
            let head = Rect::new(viewport.x, head_y, TRACK_HEADER_W, h);
            self.draw_track_header(ui, app, track, head);
            // Sash am Unterrand: Spurhöhe per Drag verstellen (nach dem Header,
            // damit der Griff über dem Trennstrich liegt).
            self.handle_track_resize(ui, app, track, head);
            head_y += h;
        }
        // Sicherheitsnetz: Wurde die gezogene Spur während der Geste entfernt
        // (z. B. removeTrack-Shortcut bei gehaltenem Sash), besucht die Schleife
        // sie nicht mehr und der Release-Zweig läuft nie — `track_resize` bliebe
        // stale und das Lane-Input-Gate würde Klicks dauerhaft blockieren. Beim
        // Loslassen daher hart aufräumen (der normale Commit lief oben bereits).
        if self.track_resize.is_some() && !ui.input.left_down {
            self.track_resize = None;
        }

        // ---------------- Snap-Hilfslinie, Marquee, Playhead ------------------
        let snap_line = match (&self.drag, &self.drop_preview) {
            (Some(TlDrag::Move { snap_time, .. }), _) => *snap_time,
            (Some(TlDrag::Trim { snap_time, .. }), _) => *snap_time,
            (None, Some((_, snap))) => *snap,
            _ => None,
        };
        if let Some(t) = snap_line {
            let x = time_x(t);
            ui.fill(Rect::new(x, viewport.y, 1.0, viewport.h), theme::WARNING);
        }

        if let Some(TlDrag::Marquee {
            origin_t,
            origin_y,
            t,
            y,
            moved: true,
            ..
        }) = &self.drag
        {
            let x0 = time_x(origin_t.min(*t));
            let x1 = time_x(origin_t.max(*t));
            let top = viewport.y + RULER_H + origin_y.min(*y) - scroll_y;
            let height = (y - origin_y).abs();
            let rect = Rect::new(x0, top, x1 - x0, height);
            ui.fill(rect, theme::with_alpha(theme::ACCENT, 26));
            ui.stroke(rect, 1.0, theme::ACCENT);
        }

        // Rasierklinge: rote Linie zeigt, wo der Schnitt landen würde —
        // über dem Clip unter der Maus und seinen verknüpften Partnern,
        // nur dort, wo split_at tatsächlich schneiden kann.
        if tool == "razor"
            && self.drag.is_none()
            && self.ruler_drag.is_none()
            && ui.mouse_in(viewport)
            && mouse.x >= viewport.x + TRACK_HEADER_W
        {
            let t = pointer_time(mouse.x);
            if let Some((clip, track, _)) =
                clip_under_mouse(&preview, &lane_rects, mouse.y, t, zoom)
            {
                if !track.locked {
                    let ids = expand_links(&preview, &[clip.id.clone()]);
                    let x = time_x(t).round();
                    for (tr, lane) in &lane_rects {
                        if tr.locked {
                            continue;
                        }
                        let cuttable = preview.iter().any(|c| {
                            c.track_id == tr.id
                                && ids.contains(&c.id)
                                && t > c.start + MIN_CLIP_DURATION - EPS
                                && t < c.end() - MIN_CLIP_DURATION + EPS
                        });
                        if cuttable {
                            ui.fill(
                                Rect::new(x, lane.y + 2.0, 1.0, lane.h - 4.0),
                                theme::DANGER,
                            );
                        }
                    }
                }
            }
        }

        // ---------------- Effekt-Drop aus dem Effekte-Panel -------------------
        // Zielclip unter der Maus highlighten; Drop wendet den Effekt an
        // (Audio-Effekte landen über das A/V-Routing ggf. beim Partner).
        let effect_drag = match ui.drag_over(viewport) {
            Some(crate::ui::DragPayload::Effect(kind)) => Some(*kind),
            _ => None,
        };
        if let Some(kind) = effect_drag {
            if mouse.x >= viewport.x + TRACK_HEADER_W {
                let t = pointer_time(mouse.x);
                if let Some((clip, track, _)) =
                    clip_under_mouse(&preview, &lane_rects, mouse.y, t, zoom)
                {
                    if !track.locked && app.timeline.effect_target_clip(&clip.id, kind).is_some()
                    {
                        if let Some((_, lane)) =
                            lane_rects.iter().find(|(tr, _)| tr.id == clip.track_id)
                        {
                            let x0 = time_x(clip.start);
                            let x1 = time_x(clip.end());
                            let r = Rect::new(x0, lane.y + 1.0, (x1 - x0).max(3.0), lane.h - 2.0);
                            ui.fill(r, theme::with_alpha(theme::ACCENT, 30));
                            ui.stroke(r, 2.0, theme::ACCENT);
                        }
                    }
                }
            }
        }
        // WICHTIG: accept_drop konsumiert JEDEN Drag über dem Viewport, egal
        // welche Payload-Variante. Nur konsumieren, wenn wirklich ein Effekt
        // schwebt — sonst verschluckt dieser Zweig den Asset-Drop aus dem
        // Medien-Browser (handle_asset_drop bekäme dann None).
        if matches!(ui.drag_over(viewport), Some(crate::ui::DragPayload::Effect(_))) {
            if let Some(crate::ui::DragPayload::Effect(kind)) = ui.accept_drop(viewport) {
            let t = pointer_time(mouse.x);
            if let Some((clip, track, _)) =
                clip_under_mouse(&preview, &lane_rects, mouse.y, t, zoom)
            {
                if !track.locked {
                    if let Some(target) = app.timeline.effects_add(&clip.id, kind) {
                        // Zielclip auswählen — Effekteinstellungen zeigen ihn an.
                        app.timeline.selected_clip_ids = vec![target];
                    }
                }
            }
            }
        }

        // ---------------- Übergangs-Drop aus dem Effekte-Panel ----------------
        // Ziel ist die der Maus nächste Schnittkante des Clips darunter; die
        // Vorschau zeigt das Übergangsfenster (oder rot, wenn kein Material).
        let trans_drag = match ui.drag_over(viewport) {
            Some(crate::ui::DragPayload::Transition(kind)) => Some(*kind),
            _ => None,
        };
        if let Some(kind) = trans_drag {
            if mouse.x >= viewport.x + TRACK_HEADER_W {
                let t = pointer_time(mouse.x);
                if let Some((clip, track, edge)) = transition_drop_target(
                    &app.timeline.clips,
                    &lane_rects,
                    mouse.y,
                    t,
                    zoom,
                    kind,
                ) {
                    if let Some((_, lane)) = lane_rects.iter().find(|(tr, _)| tr.id == track.id) {
                        draw_transition_drop_preview(
                            ui, app, &clip, edge, kind, *lane, time_x,
                        );
                    }
                }
            }
        }
        // Nur konsumieren, wenn wirklich ein Übergang schwebt (siehe Effekt-Drop oben).
        if matches!(ui.drag_over(viewport), Some(crate::ui::DragPayload::Transition(_))) {
            if let Some(crate::ui::DragPayload::Transition(kind)) = ui.accept_drop(viewport) {
            let t = pointer_time(mouse.x);
            if let Some((clip, _, edge)) =
                transition_drop_target(&app.timeline.clips, &lane_rects, mouse.y, t, zoom, kind)
            {
                if let Err(err) =
                    app.timeline
                        .add_transition(kind, &clip.id, edge, DEFAULT_TRANSITION_DURATION)
                {
                    let now = ui.time;
                    app.app.set_status_message(Some(err), now);
                }
            }
            }
        }

        // Playhead (w-px accent + Dreieck) über Lineal + Spuren
        let ph_x = time_x(app.timeline.playhead_sec);
        if ph_x >= viewport.x + TRACK_HEADER_W - 4.0 {
            ui.fill(
                Rect::new(ph_x, viewport.y, 1.0, viewport.h),
                theme::ACCENT,
            );
            let tri_y = viewport.y - scroll_y;
            ui.triangle(
                v2(ph_x - 3.5, tri_y),
                v2(ph_x + 4.5, tri_y),
                v2(ph_x + 0.5, tri_y + 6.0),
                theme::ACCENT,
            );
        }

        // ---------------- Dauer-Eingabe eines Übergangs -----------------------
        self.render_trans_editor(ui, app, &trans_rects, viewport);

        ui.pop_clip();

        // ---------------- Asset-Drop aus dem Medien-Browser -------------------
        self.handle_asset_drop(ui, app, pointer_time, &track_at_y, viewport);

        // ---------------- Scrollbars ------------------------------------------
        if need_v {
            let track = Rect::new(viewport.right(), viewport.y, theme::SCROLLBAR_W, viewport.h);
            self.scroll_y = scrollbar(
                ui,
                ui.id(("tl.scroll.v", rect.x.to_bits())),
                track,
                self.scroll_y,
                viewport.h,
                content_h,
                false,
                &mut self.sb_drag_v,
            );
        }
        if need_h {
            let track = Rect::new(viewport.x, viewport.bottom(), viewport.w, theme::SCROLLBAR_W);
            self.scroll_x = scrollbar(
                ui,
                ui.id(("tl.scroll.h", rect.x.to_bits())),
                track,
                self.scroll_x,
                viewport.w,
                content_w,
                true,
                &mut self.sb_drag_h,
            );
        }
    }
}

impl TimelinePanel {
    #[allow(clippy::too_many_arguments)]
    fn draw_clip(
        &self,
        ui: &mut Ui,
        app: &AppState,
        services: &Services,
        clip: &TimelineClip,
        asset: Option<&crate::core::types::MediaAsset>,
        rect: Rect,
        selected: bool,
        locked: bool,
    ) {
        let is_audio = clip.kind == TrackKind::Audio;
        let offline = asset.is_some_and(|a| a.offline);
        let (border, bg) = if offline {
            // Quelldatei fehlt: Clip bleibt erhalten, wird aber rot markiert.
            (
                theme::with_alpha(theme::DANGER, 204),
                theme::with_alpha(theme::DANGER, 38),
            )
        } else if clip.is_title() {
            // Titel/Grafik: violett (Premiere-Konvention).
            (
                theme::with_alpha(theme::GRAPHIC, 178),
                theme::with_alpha(theme::GRAPHIC, 33),
            )
        } else if clip.is_subtitle() {
            // Untertitel: gelb (Premiere-Konvention für Captions).
            (
                theme::with_alpha(theme::WARNING, 178),
                theme::with_alpha(theme::WARNING, 33),
            )
        } else if is_audio {
            (
                theme::with_alpha(theme::SUCCESS, 153),
                theme::with_alpha(theme::SUCCESS, 38),
            )
        } else {
            (theme::ACCENT, theme::with_alpha(theme::ACCENT_SOFT, 204))
        };
        let alpha = if clip.enabled { 255 } else { 102 };

        ui.fill_rounded(rect, theme::RADIUS_SM, theme::with_alpha(bg, bg.a.min(alpha)));
        ui.push_clip(rect);

        if is_audio {
            // Wellenform (grün, halbtransparent)
            if let Some(asset) = asset {
                match app.media.waveforms.get(&asset.id) {
                    Some(Some(peaks)) if !peaks.is_empty() => {
                        let total = if clip.src_duration.is_finite() && clip.src_duration > 0.0 {
                            clip.src_duration
                        } else {
                            clip.media_span().max(clip.duration)
                        };
                        // Sichtbare Medienspanne = duration × speed; rückwärts
                        // läuft die Wellenform gespiegelt vom Medien-Out.
                        let span = ((clip.media_span() / total) * peaks.len() as f64).max(1.0);
                        let from = if clip.reverse {
                            (clip.media_out() / total) * peaks.len() as f64
                        } else {
                            (clip.src_in / total) * peaks.len() as f64
                        };
                        let h = rect.h;
                        let w = rect.w as i32;
                        let dir: f32 = if clip.reverse { -1.0 } else { 1.0 };
                        let step = (1.0 / rect.w.max(1.0) * span as f32).max(0.0) * dir;
                        let mut idx_f = from as f32;
                        let color = theme::with_alpha(theme::SUCCESS, 128_u8.min(alpha));
                        for x in 0..w {
                            let idx = (idx_f.max(0.0) as usize).min(peaks.len() - 1);
                            let v = peaks[idx];
                            let bar_h = (v * (h - 2.0)).max(1.0);
                            ui.fill(
                                Rect::new(rect.x + x as f32, rect.y + (h - bar_h) / 2.0, 1.0, bar_h),
                                color,
                            );
                            idx_f += step;
                        }
                    }
                    Some(_) => {}
                    None => {
                        // Im Proxy-Modus aus der Proxy-Datei extrahieren (Audio
                        // durchgereicht; spart das Seeken im großen Original).
                        let src = asset.decode_path(app.media.use_proxies);
                        services.request_waveform(&asset.id, src, 1200);
                    }
                }
            }
        } else if let Some(asset) = asset {
            // Thumbnail links (h-full w-auto, opacity-50)
            if rect.w > 48.0 {
                if let Some(thumb) = &asset.thumbnail_path {
                    // Erst die Texturmaße (fordert die Texture bei Bedarf an),
                    // dann zentral skaliert über `draw_texture_in` zeichnen.
                    if let Some((tw_px, th_px)) = ui.texture_size(thumb) {
                        let tw = tw_px / th_px * rect.h;
                        let dest = Rect::new(rect.x, rect.y, tw, rect.h);
                        let src = Rect::new(0.0, 0.0, tw_px, th_px);
                        ui.draw_texture_in(
                            thumb,
                            src,
                            dest,
                            raylib::color::Color::new(255, 255, 255, 128_u8.min(alpha)),
                        );
                    }
                }
            }
        }

        // Titelzeile: Offline-/Link-Icon + Name + Dauer (oben ausgerichtet;
        // bei den kompakten Untertitel-Spuren vertikal zentriert)
        let row_y = if clip.is_subtitle() {
            rect.y + (rect.h - 16.0) / 2.0
        } else {
            rect.y + 2.0
        };
        let mut inner = Rect::new(rect.x + 6.0, row_y, rect.w - 12.0, 16.0);
        if clip.is_title() {
            let ic = inner.cut_left(12.0);
            ui.icon(
                "type",
                Rect::new(ic.x, ic.y + 2.0, 12.0, 12.0),
                12.0,
                theme::with_alpha(theme::GRAPHIC, alpha),
            );
            inner.cut_left(4.0);
        }
        if clip.is_subtitle() {
            let ic = inner.cut_left(12.0);
            ui.icon(
                "captions",
                Rect::new(ic.x, ic.y + 2.0, 12.0, 12.0),
                12.0,
                theme::with_alpha(theme::WARNING, alpha),
            );
            inner.cut_left(4.0);
        }
        if offline {
            let ic = inner.cut_left(12.0);
            ui.icon(
                "triangle-alert",
                Rect::new(ic.x, ic.y + 2.0, 12.0, 12.0),
                12.0,
                theme::with_alpha(theme::DANGER, alpha),
            );
            inner.cut_left(4.0);
        }
        if clip.link_id.is_some() {
            let ic = inner.cut_left(12.0);
            ui.icon(
                "link-2",
                Rect::new(ic.x, ic.y + 2.0, 12.0, 12.0),
                12.0,
                theme::with_alpha(theme::TEXT_2, alpha),
            );
            inner.cut_left(4.0);
        }
        // Keyframe-Badge: Clip trägt animierte Parameter (Raute = 0°-Poly).
        if clip.fx.any_animated() || clip.effects.iter().any(|e| e.any_animated()) {
            let ic = inner.cut_left(10.0);
            ui.poly(
                crate::ui::geom::v2(ic.x + 5.0, ic.y + 8.0),
                4,
                4.0,
                0.0,
                theme::with_alpha(theme::ACCENT, alpha),
            );
            inner.cut_left(4.0);
        }
        // Effekt-Badge: Clip trägt aktive Effekte (Blitz).
        if clip.effects.iter().any(|e| e.enabled) {
            let ic = inner.cut_left(12.0);
            ui.icon(
                "zap",
                Rect::new(ic.x, ic.y + 2.0, 12.0, 12.0),
                12.0,
                theme::with_alpha(theme::ACCENT, alpha),
            );
            inner.cut_left(4.0);
        }
        // Geschwindigkeits-Badge („50 %“, „−100 %“, „Standbild“).
        if let Some(speed) = clip.speed_label() {
            let bw = ui.font(FontKind::Mono11).width(&speed) + 8.0;
            if inner.w > bw + 48.0 {
                let cell = inner.cut_left(bw);
                let chip = Rect::new(cell.x, cell.y + 1.0, bw, 14.0);
                ui.fill_rounded(chip, 3.0, theme::with_alpha(theme::SURFACE_0, 150_u8.min(alpha)));
                ui.text_centered(
                    &speed,
                    chip,
                    theme::with_alpha(theme::TEXT_1, alpha),
                    FontKind::Mono11,
                );
                inner.cut_left(4.0);
            }
        }
        let dur_label = format_duration(clip.duration);
        if rect.w > 88.0 && !clip.is_subtitle() {
            let dw = ui.font(FontKind::Mono12).width(&dur_label);
            let cell = inner.cut_right(dw);
            ui.text_left(
                &dur_label,
                cell,
                theme::with_alpha(theme::TEXT_2, alpha),
                FontKind::Mono12,
            );
            inner.cut_right(4.0);
        }
        let name = ui.font(FontKind::Sans12).ellipsize(&clip.name, inner.w);
        ui.text_left(
            &name,
            inner,
            theme::with_alpha(theme::TEXT_1, alpha),
            FontKind::Sans12,
        );

        // Clip-Marker als kleine Kerben am unteren Clip-Rand (Medienzeit →
        // Sequenzposition über die Clip-Abbildung).
        if !clip.markers.is_empty() && clip.duration > EPS {
            let notch_y = rect.bottom() - 4.0;
            for (st, m) in clip.visible_markers() {
                let frac = ((st - clip.start) / clip.duration).clamp(0.0, 1.0) as f32;
                let x = rect.x + frac * rect.w;
                let col = theme::with_alpha(marker_color(m.color), alpha);
                ui.fill(Rect::new(x - 0.5, rect.y, 1.0, rect.h), theme::with_alpha(col, 70_u8.min(alpha)));
                ui.triangle(
                    v2(x - 3.5, rect.bottom()),
                    v2(x, notch_y),
                    v2(x + 3.5, rect.bottom()),
                    col,
                );
            }
        }

        // Trim-Griffe bei Hover (visuell)
        let hovered = ui.mouse_in(rect);
        if hovered && !locked && rect.w > 24.0 {
            let grip = theme::with_alpha(theme::TEXT_1, 102);
            ui.fill(Rect::new(rect.x, rect.y, 6.0, rect.h), grip);
            ui.fill(Rect::new(rect.right() - 6.0, rect.y, 6.0, rect.h), grip);
        }
        ui.pop_clip();

        ui.stroke_rounded(rect, theme::RADIUS_SM, 1.0, theme::with_alpha(border, alpha));
        if selected {
            ui.stroke_rounded(rect.inset(-1.0), theme::RADIUS_SM, 2.0, theme::TEXT_1);
        }

        // Cursor an den Kanten
        if hovered && !locked {
            let local_x = ui.input.mouse.x - rect.x;
            let on_edge = rect.w > EDGE_PX * 3.0
                && (local_x <= EDGE_PX || local_x >= rect.w - EDGE_PX);
            let tool: &str = app.app.active_tool;
            if on_edge && (tool == "select" || tool == "ripple" || tool == "rolling") {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
            }
        }
    }

    fn draw_track_header(&self, ui: &mut Ui, app: &mut AppState, track: &TimelineTrack, head: Rect) {
        let bg = if track.locked {
            theme::SURFACE_3
        } else {
            theme::SURFACE_2
        };
        ui.fill(head, bg);
        ui.vline(head.right() - 1.0, head.y, head.h, theme::LINE);
        ui.hline(head.x, head.bottom() - 1.0, head.w, theme::LINE);

        let name = track_name(track, &app.timeline.tracks);
        let mut inner = head.inset_xy(8.0, 0.0);

        // Buttons rechts (size-3.5-Icons). Untertitel-Spuren haben statt
        // Mute/Solo/Sync nur einen Sichtbarkeits-Schalter (Auge); `muted`
        // dient dort als „ausgeblendet“.
        let buttons: Vec<(&str, bool, TrackFlag, raylib::color::Color, raylib::color::Color)> =
            if track.kind == TrackKind::Subtitle {
                vec![
                    (
                        if track.muted { "eye-off" } else { "eye" },
                        track.muted,
                        TrackFlag::Muted,
                        theme::WARNING,
                        theme::with_alpha(theme::WARNING, 51),
                    ),
                    ("lock", track.locked, TrackFlag::Locked, theme::ACCENT, theme::ACCENT_SOFT),
                ]
            } else {
                vec![
                    // Sync-Lock (rippelt bei Insert/Extract mit) — eigene Farbe
                    // (Lila), klar getrennt von der Spur-Sperre (Blau).
                    (
                        "arrow-left-right",
                        track.sync_lock,
                        TrackFlag::SyncLock,
                        theme::GRAPHIC,
                        theme::with_alpha(theme::GRAPHIC, 51),
                    ),
                    (
                        "volume-2",
                        track.muted,
                        TrackFlag::Muted,
                        theme::WARNING,
                        theme::with_alpha(theme::WARNING, 51),
                    ),
                    (
                        "headphones",
                        track.solo,
                        TrackFlag::Solo,
                        theme::SUCCESS,
                        theme::with_alpha(theme::SUCCESS, 51),
                    ),
                    ("lock", track.locked, TrackFlag::Locked, theme::ACCENT, theme::ACCENT_SOFT),
                ]
            };
        let mut bx = inner.right();
        for (i, (icon, active, flag, fg, bg)) in buttons.iter().enumerate().rev() {
            bx -= 16.0;
            let btn = Rect::new(bx, head.y + (head.h - 16.0) / 2.0, 16.0, 16.0);
            let id = ui.id(("tl.track.flag", track.id.as_str(), i));
            let it = ui.interact(id, btn);
            if *active {
                ui.fill_rounded(btn, theme::RADIUS_SM, *bg);
            }
            let color = if *active {
                *fg
            } else if it.hovered {
                theme::TEXT_1
            } else {
                theme::TEXT_3
            };
            ui.icon(icon, btn, 14.0, color);
            if it.hovered {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                ui.tooltip(id, btn, track_flag_tip(*flag, track.kind));
            }
            if it.clicked {
                app.timeline.toggle_track_flag(&track.id, *flag);
            }
            bx -= 4.0;
        }

        if track.kind == TrackKind::Subtitle {
            // Untertitel: schlichtes Namens-Label links wie zuvor.
            inner.w = (bx - inner.x).max(0.0);
            let display = ui.font(FontKind::Sans12Medium).ellipsize(&name, inner.w);
            ui.text_left(&display, inner, theme::TEXT_2, FontKind::Sans12Medium);
        } else {
            // Video/Audio: Patch-Chip (zeigt den Spurnamen + Source-Patch-Ziel)
            // und Target-Toggle links — die Source-/Ziel-Zone wie in Premiere.
            let chip = Rect::new(inner.x, head.y + (head.h - 18.0) / 2.0, 30.0, 18.0);
            let chip_id = ui.id(("tl.track.patch", track.id.as_str()));
            let chip_it = ui.interact(chip_id, chip);
            let patched = track.source_patched;
            let chip_bg = if patched {
                theme::ACCENT_SOFT
            } else if chip_it.hovered {
                theme::SURFACE_4
            } else {
                theme::SURFACE_1
            };
            ui.fill_rounded(chip, theme::RADIUS_SM, chip_bg);
            let chip_fg = if patched { theme::ACCENT } else { theme::TEXT_2 };
            let display = ui.font(FontKind::Sans12Medium).ellipsize(&name, chip.w - 6.0);
            ui.text_centered(&display, chip, chip_fg, FontKind::Sans12Medium);
            if chip_it.hovered {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                ui.tooltip(chip_id, chip, "Source-Patch: Ziel für Insert/Überschreiben");
            }
            if chip_it.clicked {
                app.timeline.toggle_source_patch(&track.id);
            }

            let tgt = Rect::new(chip.right() + 4.0, head.y + (head.h - 16.0) / 2.0, 16.0, 16.0);
            let tgt_id = ui.id(("tl.track.target", track.id.as_str()));
            let tgt_it = ui.interact(tgt_id, tgt);
            if track.targeted {
                ui.fill_rounded(tgt, theme::RADIUS_SM, theme::ACCENT_SOFT);
            }
            let tgt_col = if track.targeted {
                theme::ACCENT
            } else if tgt_it.hovered {
                theme::TEXT_1
            } else {
                theme::TEXT_3
            };
            ui.icon("focus", tgt, 14.0, tgt_col);
            if tgt_it.hovered {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                ui.tooltip(tgt_id, tgt, "Spur anvisieren (Lift/Extract/Match Frame)");
            }
            if tgt_it.clicked {
                app.timeline.toggle_track_flag(&track.id, TrackFlag::Targeted);
            }
        }

        // Rechtsklick: Spur-Menü
        if ui.mouse_in(head) && ui.input.right_pressed {
            let items = track_header_menu(track, &name);
            app.context_menu
                .show(ui.input.mouse.x, ui.input.mouse.y, items);
        }
    }

    /// Sash am Spurkopf-Unterrand: Spurhöhe per Drag verstellen (für Waveforms/
    /// Keyframes). Setzt den Resize-Cursor beim Überfahren, startet die Geste
    /// beim Drücken und schreibt die geklemmte Höhe live nach `app.timeline`
    /// (persistiert in der Sequenz). Liegt in der Header-Spalte, die der Lane-
    /// Input ohnehin ausnimmt — daher kein Konflikt mit Clip-/Marquee-Klicks.
    fn handle_track_resize(&mut self, ui: &mut Ui, app: &mut AppState, track: &TimelineTrack, head: Rect) {
        let sash = Rect::new(head.x, head.bottom() - TRACK_SASH_H, head.w, TRACK_SASH_H);
        let active = self
            .track_resize
            .as_ref()
            .is_some_and(|d| d.track_id == track.id);
        let hover = ui.mouse_in(sash);
        if active || hover {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_NS);
            // Akzent-Griff am Unterrand als visuelle Rückmeldung.
            ui.fill(Rect::new(sash.x, head.bottom() - 2.0, sash.w, 2.0), theme::ACCENT);
        }

        if active {
            if ui.input.left_down {
                if let Some(d) = self.track_resize.as_ref() {
                    let new_h = (d.start_h + (ui.input.mouse.y - d.start_mouse_y))
                        .clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
                    // Erst ab spürbarem Versatz schreiben — ein reines Klicken
                    // (ohne Ziehen) lässt die Höhe unangetastet (kein None→Some).
                    if (new_h - d.start_h).abs() > 0.5 {
                        app.timeline.set_track_height_live(&track.id, new_h);
                    }
                }
            } else {
                // Geste beendet: einmalig als geändert verbuchen (Dirty/Autosave),
                // wenn sich die Höhe gegenüber dem Gestenbeginn verändert hat. So
                // bleibt die Revision während des Ziehens stabil (kein Per-Frame-
                // Dirty / keine Render-Cache-Revalidierung pro Frame).
                if let Some(d) = self.track_resize.take() {
                    if (track_height(track) - d.start_h).abs() > 0.5 {
                        app.timeline.mark_track_resized();
                    }
                }
            }
            return;
        }

        // Neue Geste: Druck auf die Greifzone (nur wenn nichts anderes zieht).
        if hover
            && ui.input.left_pressed
            && self.drag.is_none()
            && self.ruler_drag.is_none()
        {
            self.track_resize = Some(TrackResizeDrag {
                track_id: track.id.clone(),
                start_mouse_y: ui.input.mouse.y,
                start_h: track_height(track),
            });
        }
    }

    /// Lautstärke-/Pan-Gummiband einer Audiospur zeichnen und bearbeiten
    /// (Mod+Klick = Punkt setzen, Ziehen = verschieben, Rechts-/Alt-Klick =
    /// löschen). Liefert true, wenn der Klick verbraucht wurde (Clip-Input
    /// dann überspringen). Automation in SEQUENZZEIT; `track` ist eine Kopie
    /// (Stand Frame-Beginn), Edits laufen über `app.timeline` mit Undo.
    #[allow(clippy::too_many_arguments)]
    fn handle_track_automation(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        track: &TimelineTrack,
        lane_clip: Rect,
        zoom: f64,
        lane_x0: f32,
        scroll_x: f32,
    ) -> bool {
        if lane_clip.w <= 4.0 || lane_clip.h < 18.0 {
            return false;
        }
        let param = if self.auto_pan.contains(&track.id) {
            TrackAutoParam::Pan
        } else {
            TrackAutoParam::Volume
        };
        let range = auto_range(param);
        let pad = 5.0f32;
        let mid = lane_clip.y + lane_clip.h * 0.5;
        let half = (lane_clip.h * 0.5 - pad).max(1.0);
        let y_of = |v: f64| mid - (v.clamp(-range, range) / range) as f32 * half;
        let v_of = |y: f32| (-((y - mid) / half) as f64 * range).clamp(-range, range);
        let time_x = |t: f64| lane_x0 + (t * zoom) as f32 - scroll_x;
        let pointer_time = |x: f32| ((x - lane_x0 + scroll_x) as f64 / zoom).max(0.0);

        // ---- Zeichnen ----
        ui.push_clip(lane_clip);
        ui.hline(lane_clip.x, mid, lane_clip.w, theme::with_alpha(theme::LINE, 90));
        let ap = track.auto_param(param);
        let animated = ap.is_animated();
        let line_col = if animated {
            theme::with_alpha(theme::ACCENT, 220)
        } else {
            theme::with_alpha(theme::ACCENT, 80)
        };
        let mut prev: Option<Vector2> = None;
        let mut xx = lane_clip.x;
        while xx <= lane_clip.right() {
            let v = ap.eval(pointer_time(xx));
            let p = v2(xx, y_of(v));
            if let Some(pp) = prev {
                ui.line(pp, p, 1.5, line_col);
            }
            prev = Some(p);
            xx += 3.0;
        }
        let mut hit_point: Option<f64> = None;
        for k in &ap.keyframes {
            let px = time_x(k.t);
            if px < lane_clip.x - 6.0 || px > lane_clip.right() + 6.0 {
                continue;
            }
            let py = y_of(k.value);
            let hov = ui.mouse_in(Rect::new(px - 5.0, py - 5.0, 10.0, 10.0));
            ui.circle(v2(px, py), 4.0, if hov { theme::TEXT_1 } else { theme::ACCENT });
            ui.circle_outline(v2(px, py), 4.0, theme::SURFACE_0);
            if hov {
                hit_point = Some(k.t);
            }
        }
        ui.pop_clip();

        // ---- Vol/Pan-Umschalter (Lane-Eck oben links) ----
        let chip = Rect::new(lane_clip.x + 2.0, lane_clip.y + 2.0, 30.0, 13.0);
        let chip_id = ui.id(("tl.auto.chip", &track.id));
        let cit = ui.interact(chip_id, chip);
        ui.fill_rounded(chip, theme::RADIUS_XS, theme::with_alpha(theme::SURFACE_0, 200));
        ui.text_centered(
            if param == TrackAutoParam::Pan { "Pan" } else { "Vol" },
            chip,
            if cit.hovered { theme::TEXT_1 } else { theme::TEXT_3 },
            FontKind::Mono11,
        );
        if cit.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
        }
        if cit.clicked {
            if !self.auto_pan.insert(track.id.clone()) {
                self.auto_pan.remove(&track.id);
            }
            return true;
        }

        // ---- Laufende Geste fortführen ----
        let mouse = ui.input.mouse;
        if self
            .auto_drag
            .as_ref()
            .is_some_and(|d| d.track_id == track.id)
        {
            if ui.input.left_down {
                let nt = pointer_time(mouse.x);
                let nv = v_of(mouse.y);
                let d = self.auto_drag.as_mut().expect("auto_drag");
                if !d.pushed {
                    app.timeline.begin_mix_edit();
                    d.pushed = true;
                }
                let mut keys = d.orig_keys.clone();
                if let Some(kf) = keys.iter_mut().find(|k| (k.t - d.orig_t).abs() < KF_TIME_EPS) {
                    kf.t = nt;
                    kf.value = nv;
                }
                app.timeline.track_auto_replace_live(&track.id, d.param, keys);
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_ALL);
            } else {
                self.auto_drag = None;
            }
            return true;
        }

        // ---- Neue Geste starten ----
        if !ui.mouse_in(lane_clip) || cit.hovered {
            return false;
        }
        if let Some(t) = hit_point {
            // Löschen: Rechts- oder Alt-Klick auf einen Punkt.
            if ui.input.right_pressed || (ui.input.left_pressed && ui.input.alt) {
                app.timeline.track_auto_remove_point(&track.id, param, t);
                return true;
            }
            // Vorhandenen Punkt ziehen.
            if ui.input.left_pressed {
                self.auto_drag = Some(AutoDrag {
                    track_id: track.id.clone(),
                    param,
                    orig_t: t,
                    orig_keys: ap.keyframes.clone(),
                    pushed: false,
                });
                return true;
            }
        } else if ui.input.left_pressed && (ui.input.ctrl || ui.input.meta) {
            // Mod+Klick: neuen Punkt setzen und sofort ziehen.
            let t = pointer_time(mouse.x);
            let v = v_of(mouse.y);
            app.timeline.track_auto_add_point(&track.id, param, t, v);
            let keys = app
                .timeline
                .tracks
                .iter()
                .find(|tr| tr.id == track.id)
                .map(|tr| tr.auto_param(param).keyframes.clone())
                .unwrap_or_default();
            self.auto_drag = Some(AutoDrag {
                track_id: track.id.clone(),
                param,
                orig_t: t,
                orig_keys: keys,
                pushed: true, // add_point hat bereits einen Snapshot gelegt
            });
            return true;
        }
        false
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_lane_input(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        preview: &[TimelineClip],
        lane_rects: &[(TimelineTrack, Rect)],
        trans_rects: &[(String, Rect, bool)],
        pointer_time: impl Fn(f32) -> f64,
        lane_y: impl Fn(f32) -> f32,
        track_at_y: &impl Fn(f32) -> Option<TimelineTrack>,
        viewport: Rect,
    ) {
        let mouse = ui.input.mouse;
        // Header-Spalte ausnehmen (sticky links)
        if mouse.x < viewport.x + TRACK_HEADER_W {
            return;
        }
        let tool: &str = app.app.active_tool;

        // Clip unter der Maus finden
        let zoom = app.timeline.zoom_px_per_sec;
        let hit_clip =
            clip_under_mouse(preview, lane_rects, mouse.y, pointer_time(mouse.x), zoom);

        // ---------------- Übergangs-Band unter der Maus ----------------
        // Bänder liegen ÜBER den Clips und fangen Auswahl-Interaktionen ab.
        let hit_trans = trans_rects
            .iter()
            .find(|(_, r, _)| ui.mouse_in(*r))
            .cloned();
        if let Some((trans_id, band, locked)) = &hit_trans {
            if !*locked && tool == "select" {
                let local_x = mouse.x - band.x;
                if band.w > EDGE_PX * 3.0 && (local_x <= EDGE_PX || local_x >= band.w - EDGE_PX) {
                    ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
                } else {
                    ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                }
            }
            if ui.input.right_pressed {
                app.timeline.select_transition(trans_id);
                if let Some(tr) = app.timeline.transition(trans_id).cloned() {
                    let items = transition_context_menu(app, &tr);
                    app.context_menu.show(mouse.x, mouse.y, items);
                }
                return;
            }
            if ui.input.left_pressed && tool == "select" {
                app.app.focused_panel = "timeline".into();
                if *locked {
                    return;
                }
                if ui.input.double_click {
                    self.open_trans_editor(ui, app, trans_id);
                    return;
                }
                // Kantenzonen: Dauer trimmen; sonst auswählen.
                let local_x = mouse.x - band.x;
                let edge = if band.w > EDGE_PX * 3.0 {
                    if local_x <= EDGE_PX {
                        Some(TrimEdge::Start)
                    } else if local_x >= band.w - EDGE_PX {
                        Some(TrimEdge::End)
                    } else {
                        None
                    }
                } else {
                    None
                };
                app.timeline.select_transition(trans_id);
                if let Some(edge) = edge {
                    let preview = app
                        .timeline
                        .transition(trans_id)
                        .map(|t| t.duration)
                        .unwrap_or(DEFAULT_TRANSITION_DURATION);
                    self.drag = Some(TlDrag::TransTrim {
                        id: trans_id.clone(),
                        edge,
                        preview,
                    });
                }
                return;
            }
            if ui.input.left_pressed && tool == "razor" {
                // Kein Schnitt durch einen Übergang (Premiere-Verhalten).
                return;
            }
        }

        // ---------------- Rechtsklick-Menüs ----------------
        if ui.input.right_pressed && ui.mouse_in(viewport) {
            let t = pointer_time(mouse.x);
            if let Some((clip, _, _)) = &hit_clip {
                if !app.timeline.selected_clip_ids.contains(&clip.id) {
                    app.timeline
                        .select_clips(&[clip.id.clone()], SelectMode::Replace, true);
                }
                let items = clip_context_menu(app, clip, t);
                app.context_menu.show(mouse.x, mouse.y, items);
            } else {
                let track = track_at_y(mouse.y);
                let items = lane_context_menu(app, track.as_ref(), t);
                app.context_menu.show(mouse.x, mouse.y, items);
            }
            return;
        }

        if !ui.input.left_pressed || !ui.mouse_in(viewport) {
            return;
        }
        app.app.focused_panel = "timeline".into();

        // ---------------- Mittelmaus/Hand: Pan -------------
        if ui.input.middle_pressed || tool == "hand" {
            self.drag = Some(TlDrag::Pan {
                origin_x: mouse.x,
                origin_y: mouse.y,
                start_left: self.scroll_x,
                start_top: self.scroll_y,
            });
            return;
        }

        // ---------------- Zoom-Werkzeug --------------------
        if tool == "zoom" {
            let time = pointer_time(mouse.x);
            self.zoom_anchor = Some((time, mouse.x));
            if ui.input.alt {
                app.timeline.zoom_out();
            } else {
                app.timeline.zoom_in();
            }
            return;
        }

        if let Some((clip, track, _lane)) = hit_clip {
            // ---------------- Klick auf Clip ----------------
            let locked = track.locked;
            if tool == "razor" {
                if !locked {
                    let t = pointer_time(mouse.x);
                    app.timeline.split_at(t, Some(&[clip.id.clone()]));
                }
                return;
            }
            // Auswahl: Shift/Mod toggelt, Alt wählt ohne verknüpften Partner
            // (bereits selektierte Clips bleiben — Alt+Drag dupliziert dann
            // die ganze Auswahl).
            if ui.input.shift || ui.input.ctrl || ui.input.meta {
                app.timeline
                    .select_clips(&[clip.id.clone()], SelectMode::Toggle, true);
                return;
            }
            if !app.timeline.selected_clip_ids.contains(&clip.id) {
                app.timeline.select_clips(
                    &[clip.id.clone()],
                    SelectMode::Replace,
                    !ui.input.alt,
                );
            }
            if locked {
                return;
            }

            // Doppelklick: Nest-Clip öffnet die innere Sequenz im Tab,
            // sonst lädt der Clip in den Quellmonitor.
            if ui.input.double_click {
                if let Some(nested) = clip.nest_seq.clone() {
                    ui.run_command_with(
                        "sequence.open",
                        serde_json::json!({ "sequenceId": nested }),
                    );
                } else if app.media.asset(&clip.asset_id).is_some() {
                    app.media.select(vec![clip.asset_id.clone()]);
                    ui.run_command_with(
                        "media.openInSource",
                        serde_json::json!({ "assetId": clip.asset_id }),
                    );
                }
                return;
            }

            let clip_x = {
                let t0 = clip.start;
                (pointer_time(mouse.x) - t0) * zoom
            } as f32;
            let clip_w = (clip.duration * zoom) as f32;
            let edge: Option<TrimEdge> = if clip_w > EDGE_PX * 3.0 {
                if clip_x <= EDGE_PX {
                    Some(TrimEdge::Start)
                } else if clip_x >= clip_w - EDGE_PX {
                    Some(TrimEdge::End)
                } else {
                    None
                }
            } else {
                None
            };

            if (tool == "select" || tool == "ripple") && edge.is_some() {
                let exclude = expand_links(&app.timeline.clips, &[clip.id.clone()]);
                self.collect_snap_targets(app, &exclude);
                self.drag = Some(TlDrag::Trim {
                    clip_id: clip.id.clone(),
                    edge: edge.unwrap(),
                    ripple: tool == "ripple",
                    origin_x: mouse.x,
                    delta_sec: 0.0,
                    snap_time: None,
                });
                return;
            }

            if tool == "rolling" {
                if let Some(edge) = edge {
                    let clip_end = clip.end();
                    let neighbor = match edge {
                        TrimEdge::End => app.timeline.clips.iter().find(|c| {
                            c.track_id == clip.track_id && (c.start - clip_end).abs() < EPS
                        }),
                        TrimEdge::Start => app.timeline.clips.iter().find(|c| {
                            c.track_id == clip.track_id && (c.end() - clip.start).abs() < EPS
                        }),
                    };
                    if let Some(neighbor) = neighbor {
                        let (left_id, right_id) = match edge {
                            TrimEdge::End => (clip.id.clone(), neighbor.id.clone()),
                            TrimEdge::Start => (neighbor.id.clone(), clip.id.clone()),
                        };
                        self.drag = Some(TlDrag::Roll {
                            left_id,
                            right_id,
                            origin_x: mouse.x,
                            delta_sec: 0.0,
                        });
                    }
                }
                return;
            }

            if tool == "slip" {
                self.drag = Some(TlDrag::Slip {
                    clip_id: clip.id.clone(),
                    origin_x: mouse.x,
                    delta_sec: 0.0,
                });
                return;
            }
            if tool == "slide" {
                self.drag = Some(TlDrag::Slide {
                    clip_id: clip.id.clone(),
                    origin_x: mouse.x,
                    delta_sec: 0.0,
                });
                return;
            }

            if tool == "select" {
                let ids = if ui.input.alt {
                    // Alt: exakt die (ggf. teil-)selektierten Clips ziehen.
                    app.timeline.selected_clip_ids.clone()
                } else {
                    expand_links(&app.timeline.clips, &app.timeline.selected_clip_ids)
                };
                self.collect_snap_targets(app, &ids);
                self.drag = Some(TlDrag::Move {
                    clip_ids: ids,
                    grab_id: clip.id.clone(),
                    origin_x: mouse.x,
                    delta_sec: 0.0,
                    lane_offset: 0,
                    snap_time: None,
                    duplicate: ui.input.alt,
                });
            }
        } else if tool == "select" {
            // ---------------- Marquee auf leerer Fläche ----------------
            let additive = ui.input.shift || ui.input.ctrl || ui.input.meta;
            self.drag = Some(TlDrag::Marquee {
                origin_t: pointer_time(mouse.x),
                origin_y: lane_y(mouse.y).max(0.0),
                t: pointer_time(mouse.x),
                y: lane_y(mouse.y).max(0.0),
                additive,
                base: if additive {
                    app.timeline.selected_clip_ids.clone()
                } else {
                    Vec::new()
                },
                moved: false,
            });
        }
    }

    fn update_drag(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        pointer_time: impl Fn(f32) -> f64,
        lane_y: impl Fn(f32) -> f32,
        track_at_y: &impl Fn(f32) -> Option<TimelineTrack>,
    ) {
        let Some(mut drag) = self.drag.take() else { return };
        let mouse = ui.input.mouse;
        let zoom = app.timeline.zoom_px_per_sec;
        let moved_this_frame =
            ui.input.mouse_delta.x.abs() > 0.0 || ui.input.mouse_delta.y.abs() > 0.0;

        // Pan separat (mutiert scroll, läuft auch mit Mittelmaus)
        if let TlDrag::Pan {
            origin_x,
            origin_y,
            start_left,
            start_top,
        } = &drag
        {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_ALL);
            self.scroll_x = *start_left - (mouse.x - *origin_x);
            self.scroll_y = *start_top - (mouse.y - *origin_y);
            if ui.input.left_down || ui.input.middle_down {
                self.drag = Some(drag);
            }
            return;
        }

        let finished = ui.input.left_released || !ui.input.left_down;

        match &mut drag {
            TlDrag::Move {
                clip_ids,
                grab_id,
                origin_x,
                delta_sec,
                lane_offset,
                snap_time,
                duplicate,
            } => {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_ALL);
                // Alt während der Geste entscheidet: Verschieben oder Duplizieren.
                *duplicate = ui.input.alt;
                let moving: Vec<TimelineClip> = app
                    .timeline
                    .clips
                    .iter()
                    .filter(|c| clip_ids.contains(&c.id))
                    .cloned()
                    .collect();
                if !moving.is_empty() {
                    let grab = app.timeline.clips.iter().find(|c| c.id == *grab_id).cloned();
                    let mut offset = *lane_offset;
                    if let (Some(grab), Some(lane)) = (&grab, track_at_y(mouse.y)) {
                        if lane.kind == grab.kind {
                            let lanes: Vec<&TimelineTrack> = app
                                .timeline
                                .tracks
                                .iter()
                                .filter(|t| t.kind == grab.kind)
                                .collect();
                            let li = lanes.iter().position(|t| t.id == lane.id);
                            let gi = lanes.iter().position(|t| t.id == grab.track_id);
                            if let (Some(li), Some(gi)) = (li, gi) {
                                offset = li as i32 - gi as i32;
                            }
                        }
                    }
                    // Offset auf die Grenzen aller bewegten Clips klemmen
                    let mut lo = i32::MIN;
                    let mut hi = i32::MAX;
                    let v_lanes: Vec<&TimelineTrack> = app
                        .timeline
                        .tracks
                        .iter()
                        .filter(|t| t.kind == TrackKind::Video)
                        .collect();
                    let a_lanes: Vec<&TimelineTrack> = app
                        .timeline
                        .tracks
                        .iter()
                        .filter(|t| t.kind == TrackKind::Audio)
                        .collect();
                    let s_lanes: Vec<&TimelineTrack> = app
                        .timeline
                        .tracks
                        .iter()
                        .filter(|t| t.kind == TrackKind::Subtitle)
                        .collect();
                    for c in &moving {
                        let lanes = match c.kind {
                            TrackKind::Video => &v_lanes,
                            TrackKind::Audio => &a_lanes,
                            TrackKind::Subtitle => &s_lanes,
                        };
                        if let Some(idx) = lanes.iter().position(|t| t.id == c.track_id) {
                            lo = lo.max(-(idx as i32));
                            hi = hi.min(lanes.len() as i32 - 1 - idx as i32);
                        }
                    }
                    let offset = if lo > hi { 0 } else { offset.clamp(lo, hi) };

                    let min_start = moving.iter().map(|c| c.start).fold(f64::INFINITY, f64::min);
                    let raw = (((mouse.x - *origin_x) / zoom as f32) as f64).max(-min_start);
                    let edges: Vec<f64> = moving.iter().flat_map(|c| [c.start, c.end()]).collect();
                    let (snapped, st) = self.snap_adjust(app, &edges, raw);
                    // Frame-Genauigkeit: ohne aktiven Kanten-/Playhead-Snap die
                    // Zielposition aufs Frame-Raster runden (ein NLE rastet jeden
                    // Edit auf ganze Frames; Kanten/Playhead sind selbst frame-aligned).
                    let snapped = if st.is_none() {
                        app.timeline.snap_to_frame(min_start + snapped) - min_start
                    } else {
                        snapped
                    };
                    *delta_sec = snapped.max(-min_start);
                    *lane_offset = offset;
                    *snap_time = st;
                }
            }
            TlDrag::Trim {
                clip_id,
                edge,
                ripple,
                origin_x,
                delta_sec,
                snap_time,
            } => {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
                let expanded = expand_links(&app.timeline.clips, &[clip_id.clone()]);
                let targets: Vec<TimelineClip> = app
                    .timeline
                    .clips
                    .iter()
                    .filter(|c| expanded.contains(&c.id))
                    .cloned()
                    .collect();
                if !targets.is_empty() {
                    let mut delta = ((mouse.x - *origin_x) / zoom as f32) as f64;
                    for c in &targets {
                        let (lo, hi) = trim_range(c, *edge, &app.timeline.clips, !*ripple);
                        delta = delta.clamp(lo, hi);
                    }
                    let anchor = targets
                        .iter()
                        .find(|c| c.id == *clip_id)
                        .unwrap_or(&targets[0]);
                    let edge_time = match edge {
                        TrimEdge::Start => anchor.start,
                        TrimEdge::End => anchor.end(),
                    };
                    let (mut snapped, st) = self.snap_adjust(app, &[edge_time], delta);
                    // Frame-Genauigkeit: ohne aktiven Snap die getrimmte Kante aufs
                    // Frame-Raster runden, dann auf den legalen Bereich klemmen.
                    if st.is_none() {
                        snapped = app.timeline.snap_to_frame(edge_time + snapped) - edge_time;
                    }
                    for c in &targets {
                        let (lo, hi) = trim_range(c, *edge, &app.timeline.clips, !*ripple);
                        snapped = snapped.clamp(lo, hi);
                    }
                    *delta_sec = snapped;
                    *snap_time = st;
                }
            }
            TlDrag::TransTrim { id, edge, preview } => {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
                if let Some(tr) = app.timeline.transition(id).cloned() {
                    let (from, to) = transitions::resolve_clips(&app.timeline.clips, &tr);
                    let t = pointer_time(mouse.x);
                    // Gewünschte Dauer aus der gezogenen Kante ableiten —
                    // welche Kante beweglich ist, hängt von Seite/Ausrichtung ab.
                    let desired: Option<f64> = match (from, to) {
                        (Some(f), Some(_)) => {
                            let cut = f.end();
                            match (tr.alignment, *edge) {
                                (TransitionAlignment::Center, _) => Some(2.0 * (t - cut).abs()),
                                (TransitionAlignment::StartAtCut, TrimEdge::End) => Some(t - cut),
                                (TransitionAlignment::EndAtCut, TrimEdge::Start) => Some(cut - t),
                                _ => None, // Kante liegt fest am Schnitt
                            }
                        }
                        // Ausblenden: Fenster endet am Clipende — linke Kante zieht.
                        (Some(f), None) => match edge {
                            TrimEdge::Start => Some(f.end() - t),
                            TrimEdge::End => None,
                        },
                        // Einblenden: Fenster beginnt am Clipanfang — rechte Kante.
                        (None, Some(c)) => match edge {
                            TrimEdge::End => Some(t - c.start),
                            TrimEdge::Start => None,
                        },
                        (None, None) => None,
                    };
                    if let Some(desired) = desired {
                        let max = app.timeline.transition_max_duration(&tr);
                        *preview = desired.clamp(MIN_CLIP_DURATION, max.max(MIN_CLIP_DURATION));
                    }
                }
            }
            TlDrag::Roll {
                left_id,
                right_id,
                origin_x,
                delta_sec,
            } => {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
                let left = app.timeline.clips.iter().find(|c| c.id == *left_id);
                let right = app.timeline.clips.iter().find(|c| c.id == *right_id);
                if let (Some(left), Some(right)) = (left, right) {
                    let (lo_l, hi_l) = trim_range(left, TrimEdge::End, &app.timeline.clips, false);
                    let (lo_r, hi_r) =
                        trim_range(right, TrimEdge::Start, &app.timeline.clips, false);
                    // Frame-Genauigkeit: die gemeinsame Schnittkante aufs Raster runden.
                    let cut = left.end();
                    let raw = ((mouse.x - *origin_x) / zoom as f32) as f64;
                    let quantized = app.timeline.snap_to_frame(cut + raw) - cut;
                    *delta_sec = quantized.clamp(lo_l.max(lo_r), hi_l.min(hi_r));
                }
            }
            TlDrag::Slip {
                clip_id,
                origin_x,
                delta_sec,
            } => {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
                let expanded = expand_links(&app.timeline.clips, &[clip_id.clone()]);
                let targets: Vec<&TimelineClip> = app
                    .timeline
                    .clips
                    .iter()
                    .filter(|c| expanded.contains(&c.id) && c.src_duration.is_finite())
                    .collect();
                let mut delta = ((mouse.x - *origin_x) / zoom as f32) as f64;
                for c in &targets {
                    let s = c.eff_speed();
                    delta = delta.clamp(
                        -c.src_in / s,
                        (c.src_duration - c.media_out()).max(0.0) / s,
                    );
                }
                *delta_sec = if targets.is_empty() { 0.0 } else { delta };
            }
            TlDrag::Slide {
                clip_id,
                origin_x,
                delta_sec,
            } => {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
                let expanded = expand_links(&app.timeline.clips, &[clip_id.clone()]);
                let targets: Vec<&TimelineClip> = app
                    .timeline
                    .clips
                    .iter()
                    .filter(|c| expanded.contains(&c.id))
                    .collect();
                if !targets.is_empty() {
                    let min_start = targets.iter().map(|c| c.start).fold(f64::INFINITY, f64::min);
                    // Frame-Genauigkeit: die verschobene Position aufs Raster runden.
                    let raw = ((mouse.x - *origin_x) / zoom as f32) as f64;
                    *delta_sec = (app.timeline.snap_to_frame(min_start + raw) - min_start).max(-min_start);
                }
            }
            TlDrag::Marquee {
                origin_t,
                origin_y,
                t,
                y,
                additive,
                base,
                moved,
            } => {
                *t = pointer_time(mouse.x);
                *y = lane_y(mouse.y).max(0.0);
                if moved_this_frame {
                    *moved = true;
                }
                // Auswahl live anwenden
                let t0 = origin_t.min(*t);
                let t1 = origin_t.max(*t);
                let y0 = origin_y.min(*y);
                let y1 = origin_y.max(*y);
                let mut hits: Vec<String> = Vec::new();
                let mut top = 0.0f32;
                for tr in &app.timeline.tracks {
                    let h = track_height(tr);
                    if y0 < top + h && y1 > top {
                        for c in &app.timeline.clips {
                            if c.track_id == tr.id && c.start < t1 && c.end() > t0 {
                                hits.push(c.id.clone());
                            }
                        }
                    }
                    top += h;
                }
                let mut sel = if *additive { base.clone() } else { Vec::new() };
                for h in hits {
                    if !sel.contains(&h) {
                        sel.push(h);
                    }
                }
                if *moved {
                    app.timeline.select_clips(&sel, SelectMode::Replace, true);
                }
            }
            TlDrag::Pan { .. } => unreachable!(),
        }

        if !finished {
            self.drag = Some(drag);
            return;
        }

        // ---------------- Drag anwenden (Store mit Undo) ----------------
        match drag {
            TlDrag::Move {
                clip_ids,
                delta_sec,
                lane_offset,
                duplicate,
                ..
            } => {
                if delta_sec.abs() > EPS || lane_offset != 0 {
                    if duplicate {
                        app.timeline.duplicate_clips(&clip_ids, delta_sec, lane_offset);
                    } else {
                        app.timeline.move_clips(&clip_ids, delta_sec, lane_offset);
                    }
                }
            }
            TlDrag::Trim {
                clip_id,
                edge,
                ripple,
                delta_sec,
                ..
            } => {
                if delta_sec.abs() > EPS {
                    if ripple {
                        app.timeline.ripple_trim_clip(&clip_id, edge, delta_sec);
                    } else {
                        app.timeline.trim_clip(&clip_id, edge, delta_sec);
                    }
                }
            }
            TlDrag::TransTrim { id, preview, .. } => {
                app.timeline.set_transition_duration(&id, preview);
            }
            TlDrag::Roll {
                left_id,
                right_id,
                delta_sec,
                ..
            } => {
                if delta_sec.abs() > EPS {
                    app.timeline.roll_edit(&left_id, &right_id, delta_sec);
                }
            }
            TlDrag::Slip {
                clip_id, delta_sec, ..
            } => {
                if delta_sec.abs() > EPS {
                    app.timeline.slip_clip(&clip_id, delta_sec);
                }
            }
            TlDrag::Slide {
                clip_id, delta_sec, ..
            } => {
                if delta_sec.abs() > EPS {
                    app.timeline.slide_clip(&clip_id, delta_sec);
                }
            }
            TlDrag::Marquee {
                moved, additive, ..
            } => {
                if !moved && !additive {
                    app.timeline.clear_selection();
                }
            }
            TlDrag::Pan { .. } => {}
        }
    }

    fn draw_ruler(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        ruler: Rect,
        time_x: impl Fn(f64) -> f32,
        pointer_time: impl Fn(f32) -> f64,
        viewport: Rect,
    ) {
        let zoom = app.timeline.zoom_px_per_sec;
        // Header-Ecke + Zeitbereich
        ui.fill(ruler, theme::SURFACE_1);
        ui.hline(ruler.x, ruler.bottom() - 1.0, ruler.w, theme::LINE);

        let time_area = Rect::new(
            ruler.x + TRACK_HEADER_W,
            ruler.y,
            ruler.w - TRACK_HEADER_W,
            ruler.h,
        );
        ui.push_clip(time_area);

        // Loop-Bereich (In/Out)
        match (app.timeline.in_point, app.timeline.out_point) {
            (Some(i), Some(o)) => {
                let x0 = time_x(i);
                let x1 = time_x(o);
                let r = Rect::new(x0, ruler.y, (x1 - x0).max(2.0), ruler.h);
                ui.fill(r, theme::with_alpha(theme::ACCENT, 51));
                ui.vline(r.x, r.y, r.h, theme::ACCENT);
                ui.vline(r.right() - 1.0, r.y, r.h, theme::ACCENT);
            }
            (i, o) => {
                for mark in [i, o].into_iter().flatten() {
                    ui.fill(
                        Rect::new(time_x(mark), ruler.y, 2.0, ruler.h),
                        theme::with_alpha(theme::ACCENT, 204),
                    );
                }
            }
        }

        // ---- Sequenz-Render-Cache-Leiste (Premiere-Pendant) ----
        // rot = vorrender-relevant aber nicht gecacht, grün = gültig gecacht,
        // gelb = wird gerade gerendert. Dünner Streifen über dem Marker-Band.
        {
            app.render_cache.refresh(&app.timeline, &app.media);
            let fps = app.timeline.settings.rate.fps().max(1.0);
            let bar_h = 3.0;
            let bar_y = ruler.bottom() - MARKER_BAND_H - bar_h - 1.0;
            for (a, b) in crate::core::render_cache::complex_spans(&app.timeline) {
                let x0 = time_x(a);
                let w = (time_x(b) - x0).max(1.0);
                ui.fill(
                    Rect::new(x0, bar_y, w, bar_h),
                    theme::with_alpha(theme::DANGER, 200),
                );
            }
            for (sf, ef) in app.render_cache.cached_spans() {
                let x0 = time_x(sf as f64 / fps);
                let w = (time_x(ef as f64 / fps) - x0).max(1.0);
                ui.fill(Rect::new(x0, bar_y, w, bar_h), theme::SUCCESS);
            }
            if let Some(r) = &app.render_cache.rendering {
                let x0 = time_x(r.start_frame as f64 / fps);
                let full = (time_x(r.end_frame as f64 / fps) - x0).max(1.0);
                ui.fill(
                    Rect::new(x0, bar_y, full, bar_h),
                    theme::with_alpha(theme::WARNING, 90),
                );
                ui.fill(
                    Rect::new(x0, bar_y, full * r.pct.clamp(0.0, 1.0), bar_h),
                    theme::WARNING,
                );
            }
        }

        // Ticks
        let step = TICK_STEPS
            .iter()
            .copied()
            .find(|s| s * zoom >= MAJOR_TICK_MIN_PX)
            .unwrap_or(TICK_STEPS[TICK_STEPS.len() - 1]);
        let minor_step = step / MINOR_PER_MAJOR as f64;
        let first_t = (self.scroll_x as f64 / zoom / minor_step).floor().max(0.0) as i64;
        let last_t = (((self.scroll_x + time_area.w) as f64 / zoom) / minor_step).ceil() as i64;
        for i in first_t..=last_t {
            let t = i as f64 * minor_step;
            let x = time_x(t);
            let major = i % MINOR_PER_MAJOR as i64 == 0;
            if major {
                ui.fill(
                    Rect::new(x, ruler.bottom() - 10.0, 1.0, 10.0),
                    theme::LINE_STRONG,
                );
                ui.text(
                    &format_duration(t),
                    v2(x + 4.0, ruler.y + 2.0),
                    theme::TEXT_3,
                    FontKind::Mono11,
                );
            } else {
                ui.fill(Rect::new(x, ruler.bottom() - 6.0, 1.0, 6.0), theme::LINE);
            }
        }

        // ---- Sequenz-Marker (farbige Symbole + Bereichsbalken) ----
        // Hit-Daten (Symbol-Rechteck in Bildschirmkoordinaten) für die
        // Interaktion einsammeln. Band am unteren Linealrand.
        let band_top = ruler.bottom() - MARKER_BAND_H;
        let mut marker_hits: Vec<(String, Rect, f64, String)> = Vec::new();
        for m in &app.timeline.markers {
            let x = time_x(m.time);
            let col = marker_color(m.color);
            if m.duration > 0.0 {
                let x2 = time_x(m.end());
                ui.fill(
                    Rect::new(x, band_top + 1.0, (x2 - x).max(1.0), MARKER_BAND_H - 2.0),
                    theme::with_alpha(col, 70),
                );
                ui.fill(Rect::new(x2 - 1.0, band_top, 1.0, MARKER_BAND_H), theme::with_alpha(col, 200));
            }
            draw_marker_symbol(ui, x, band_top, col);
            let label = marker_tooltip(&m.name, &m.note, &format_sequence_timecode(m.time, &app.timeline.settings));
            marker_hits.push((m.id.clone(), Rect::new(x - 6.0, band_top, 12.0, MARKER_BAND_H), m.time, label));
        }
        ui.pop_clip();

        // Interaktion: Marker > Scrub / Alt+Range / Kanten / Kontextmenü
        let edge_sec = EDGE_PX as f64 / zoom;
        let mouse = ui.input.mouse;
        let over_marker = marker_hits
            .iter()
            .rev()
            .find(|(_, r, _, _)| r.contains(mouse))
            .cloned();
        if ui.mouse_in(time_area) && self.ruler_drag.is_none() {
            if let Some((id, _, _, tip)) = &over_marker {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                let mid = ui.id(("tl.marker", id.as_str()));
                ui.set_hot(mid);
                ui.tooltip(mid, Rect::new(mouse.x, band_top, 1.0, MARKER_BAND_H), tip);
                if ui.input.right_pressed {
                    app.app.focused_panel = "timeline".into();
                    let menu = crate::panels::markers::marker_menu(id);
                    app.context_menu.show(mouse.x, mouse.y, menu);
                    return;
                }
                if ui.input.left_pressed {
                    app.app.focused_panel = "timeline".into();
                    if ui.input.double_click {
                        open_marker_dialog(app, id.clone());
                    } else {
                        let t = pointer_time(mouse.x).max(0.0);
                        let grab_dt = t - app.timeline.markers.iter().find(|m| &m.id == id).map(|m| m.time).unwrap_or(t);
                        self.ruler_drag = Some(RulerDrag::Marker { id: id.clone(), grab_dt, began: false });
                    }
                    return;
                }
            } else {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
                let ruler_id = ui.id("tl.ruler");
                ui.set_hot(ruler_id);
                ui.tooltip(
                    ruler_id,
                    Rect::new(mouse.x, time_area.y, 1.0, time_area.h),
                    "Ziehen: Playhead • Alt+Ziehen: Loop-Bereich • M: Marker",
                );

                if ui.input.right_pressed {
                    let t = pointer_time(mouse.x).max(0.0);
                    let has_loop =
                        app.timeline.in_point.is_some() || app.timeline.out_point.is_some();
                    app.context_menu.show(mouse.x, mouse.y, ruler_context_menu(t, has_loop));
                    return;
                }
                if ui.input.left_pressed {
                    app.app.focused_panel = "timeline".into();
                    let t = pointer_time(mouse.x).max(0.0);
                    if ui.input.alt {
                        self.ruler_drag = Some(RulerDrag::Range { origin_t: t });
                    } else if app
                        .timeline
                        .in_point
                        .is_some_and(|i| (t - i).abs() <= edge_sec)
                    {
                        self.ruler_drag = Some(RulerDrag::Edge { is_in: true });
                    } else if app
                        .timeline
                        .out_point
                        .is_some_and(|o| (t - o).abs() <= edge_sec)
                    {
                        self.ruler_drag = Some(RulerDrag::Edge { is_in: false });
                    } else {
                        self.ruler_drag = Some(RulerDrag::Scrub);
                        // Scrubbing rastet auf ganze Frames (wie in Premiere/Resolve);
                        // die kontinuierliche Playback-Position bleibt unberührt.
                        let snapped = app.timeline.snap_to_frame(t);
                        app.timeline.set_playhead(snapped);
                    }
                }
            }
        }

        if let Some(rd) = &mut self.ruler_drag {
            let t = pointer_time(mouse.x).max(0.0);
            match rd {
                RulerDrag::Scrub => {
                    let snapped = app.timeline.snap_to_frame(t);
                    app.timeline.set_playhead(snapped);
                    app.playback.scrub_active = true; // Audio-Scrubbing auslösen
                }
                RulerDrag::Range { origin_t } => {
                    app.timeline.set_in_out_range(*origin_t, t);
                }
                RulerDrag::Edge { is_in: true } => {
                    let limit = app.timeline.out_point.unwrap_or(f64::INFINITY) - MIN_CLIP_DURATION;
                    app.timeline.set_in_point(Some(t.min(limit).max(0.0)));
                }
                RulerDrag::Edge { is_in: false } => {
                    let limit = app.timeline.in_point.unwrap_or(0.0) + MIN_CLIP_DURATION;
                    app.timeline.set_out_point(Some(t.max(limit)));
                }
                RulerDrag::Marker { id, grab_dt, began } => {
                    ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                    let target = app.timeline.snap_to_frame((t - *grab_dt).max(0.0));
                    if !*began {
                        app.timeline.begin_marker_edit();
                        *began = true;
                    }
                    let id = id.clone();
                    app.timeline.marker_update_live(&id, |m| m.time = target);
                }
            }
            if !matches!(self.ruler_drag, Some(RulerDrag::Marker { .. })) {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
            }
            if ui.input.left_released || !ui.input.left_down {
                self.ruler_drag = None;
            }
        }
        let _ = viewport;
    }

    fn handle_asset_drop(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        pointer_time: impl Fn(f32) -> f64,
        track_at_y: &impl Fn(f32) -> Option<TimelineTrack>,
        viewport: Rect,
    ) {
        self.drop_preview = None;
        // Sequenz-Drop = Nesting (eigener Pfad, kein Asset-Insert).
        if matches!(ui.drag_over(viewport), Some(DragPayload::Sequences(_))) {
            if let Some(DragPayload::Sequences(ids)) = ui.accept_drop(viewport) {
                self.insert_dropped_sequences(ui, app, &ids, &pointer_time, track_at_y);
            }
            return;
        }
        let ids: Vec<String> = match ui.drag_over(viewport) {
            Some(DragPayload::Assets(ids)) => ids.clone(),
            _ => {
                // Drop trotzdem prüfen (accept_drop räumt den Drag auf)
                if let Some(DragPayload::Assets(ids)) = ui.accept_drop(viewport) {
                    self.insert_dropped(ui, app, ids, pointer_time, track_at_y);
                }
                return;
            }
        };

        // Vorschau: Platzierungen + Snap
        self.collect_snap_targets(app, &[]);
        let raw = pointer_time(ui.input.mouse.x).max(0.0);
        let (delta, snap) = self.snap_adjust(app, &[raw], 0.0);
        let t = (raw + delta).max(0.0);
        let track = track_at_y(ui.input.mouse.y);
        let placements = plan_asset_placements(
            &app.timeline,
            &app.media.assets,
            &ids,
            t,
            track.as_ref().map(|t| t.id.as_str()),
        );
        self.drop_preview = Some((placements, snap));

        if let Some(DragPayload::Assets(ids)) = ui.accept_drop(viewport) {
            self.insert_dropped(ui, app, ids, pointer_time, track_at_y);
        }
    }

    fn insert_dropped(
        &mut self,
        ui: &Ui,
        app: &mut AppState,
        ids: Vec<String>,
        pointer_time: impl Fn(f32) -> f64,
        track_at_y: &impl Fn(f32) -> Option<TimelineTrack>,
    ) {
        self.collect_snap_targets(app, &[]);
        let raw = pointer_time(ui.input.mouse.x).max(0.0);
        let (delta, _) = self.snap_adjust(app, &[raw], 0.0);
        let t = (raw + delta).max(0.0);
        let track = track_at_y(ui.input.mouse.y);
        let assets = app.media.assets.clone();
        app.timeline
            .insert_assets(&assets, &ids, t, track.as_ref().map(|t| t.id.as_str()));
        self.drop_preview = None;
    }

    /// Eine oder mehrere Sequenzen als Nest-Clips einsetzen (Drop). Der
    /// Rekursionsschutz lehnt Sequenzen ab, die sich (transitiv) selbst
    /// enthalten würden.
    fn insert_dropped_sequences(
        &mut self,
        ui: &Ui,
        app: &mut AppState,
        ids: &[String],
        pointer_time: &impl Fn(f32) -> f64,
        track_at_y: &impl Fn(f32) -> Option<TimelineTrack>,
    ) {
        self.collect_snap_targets(app, &[]);
        let raw = pointer_time(ui.input.mouse.x).max(0.0);
        let (delta, _) = self.snap_adjust(app, &[raw], 0.0);
        let t = (raw + delta).max(0.0);
        let track = track_at_y(ui.input.mouse.y);
        let track_id = track.as_ref().map(|t| t.id.clone());
        // Multicam-Quellen werden zu Multicam-Clips, normale Sequenzen zu Nests.
        let mut cursor = t;
        let mut nest_ids: Vec<String> = Vec::new();
        for id in ids {
            let mc = app
                .timeline
                .multicam_source(id)
                .map(|src| (src.duration, src.angles.iter().any(|a| a.has_audio)));
            if let Some((dur, has_audio)) = mc {
                let dur = dur.max(crate::core::timeline::MIN_CLIP_DURATION);
                let name = app.timeline.name_of(id).unwrap_or("Multicam").to_string();
                app.timeline
                    .insert_multicam_clip(id, &name, dur, has_audio, cursor, track_id.as_deref());
                cursor += dur;
            } else {
                nest_ids.push(id.clone());
            }
        }
        if !nest_ids.is_empty() {
            let (inserted, rejected) =
                app.timeline.insert_nests(&nest_ids, cursor, track_id.as_deref());
            if rejected > 0 {
                let msg = if inserted == 0 {
                    "Verschachtelung abgelehnt: Eine Sequenz darf sich nicht selbst enthalten."
                } else {
                    "Einige Sequenzen abgelehnt (Selbst-Verschachtelung)."
                };
                app.app.set_status_message(Some(msg.to_string()), ui.time);
            }
        }
        self.drop_preview = None;
    }

    // ----------------------------------------------------- Sequenz-Tabs

    /// Sequenz-Tab-Leiste (Premiere-artig): offene Sequenzen als Tabs, Klick
    /// wechselt, Mittelklick/× schließt (Sequenz bleibt im Projekt), Doppelklick
    /// benennt inline um, Drag sortiert um, Rechtsklick öffnet das Menü, „+“
    /// legt eine neue Sequenz an.
    fn render_sequence_tabs(&mut self, ui: &mut Ui, app: &mut AppState, bar: Rect) {
        ui.fill(bar, theme::SURFACE_1);
        ui.hline(bar.x, bar.bottom() - 1.0, bar.w, theme::LINE);

        // Inline-Rename initialisieren/aufräumen anhand des angeforderten Ziels.
        match app.app.rename_sequence.clone() {
            Some(target) if self.tab_rename.is_none() => {
                let mut input = TextInputState::default();
                input.set_text(app.timeline.name_of(&target).unwrap_or("").to_string());
                input.sel_start = 0;
                self.tab_rename = Some(input);
                ui.persist.keyboard_focus = ui.id(("tl.tabrename", target.as_str()));
            }
            None => self.tab_rename = None,
            _ => {}
        }

        let tabs = app.timeline.open_tabs().to_vec();
        let renaming = app.app.rename_sequence.clone();
        let active_id = app.timeline.active_id().to_string();
        let font = FontKind::Sans12;
        let pad = 10.0f32;
        let close_w = 14.0f32;
        let mouse = ui.input.mouse;
        let mut x = bar.x + 6.0;
        let mut tab_rects: Vec<(String, Rect)> = Vec::new();

        ui.push_clip(bar);
        for id in &tabs {
            let name = app.timeline.name_of(id).unwrap_or("Sequenz").to_string();
            let active = active_id == *id;
            let is_rename = renaming.as_deref() == Some(id.as_str());
            let text_w = ui.font(font).measure(&name).x.clamp(36.0, 150.0);
            let tab_w = pad + text_w + close_w + 8.0;
            let tab = Rect::new(x, bar.y + 3.0, tab_w, bar.h - 5.0);
            tab_rects.push((id.clone(), tab));

            let bg = if active { theme::SURFACE_0 } else { theme::SURFACE_2 };
            ui.fill_rounded(tab, theme::RADIUS_SM, bg);
            if active {
                ui.fill(Rect::new(tab.x, tab.bottom() - 2.0, tab.w, 2.0), theme::ACCENT);
            }

            if is_rename {
                let field = Rect::new(tab.x + 4.0, tab.y + 3.0, tab.w - 8.0, tab.h - 6.0);
                if let Some(mut input) = self.tab_rename.take() {
                    let r = input.show(ui, ("tl.tabrename", id.as_str()), field, "Name");
                    let escaped = ui
                        .input
                        .keys
                        .iter()
                        .any(|k| k.key == raylib::consts::KeyboardKey::KEY_ESCAPE);
                    let outside = ui.input.left_pressed && !field.contains(mouse);
                    if r.submitted {
                        let t = input.text.trim().to_string();
                        if !t.is_empty() {
                            app.timeline.rename(id, &t);
                        }
                    }
                    if r.submitted || escaped || outside {
                        app.app.rename_sequence = None;
                        self.tab_rename = None;
                    } else {
                        self.tab_rename = Some(input);
                    }
                }
            } else {
                let ellip = ui.font(font).ellipsize(&name, text_w);
                let label = Rect::new(tab.x + pad, tab.y, text_w + 2.0, tab.h);
                ui.text_left(&ellip, label, if active { theme::TEXT_1 } else { theme::TEXT_2 }, font);

                let close = Rect::new(
                    tab.right() - close_w - 4.0,
                    tab.y + (tab.h - close_w) / 2.0,
                    close_w,
                    close_w,
                );
                let body = Rect::new(tab.x, tab.y, tab.w - close_w - 2.0, tab.h);
                let body_id = ui.id(("tl.tab", id.as_str()));
                let close_id = ui.id(("tl.tabx", id.as_str()));
                let body_it = ui.interact(body_id, body);
                let close_it = ui.interact(close_id, close);
                if close_it.hovered {
                    ui.fill_rounded(close, theme::RADIUS_XS, theme::SURFACE_1);
                }
                ui.icon("x", close, 10.0, if close_it.hovered { theme::TEXT_1 } else { theme::TEXT_3 });
                if body_it.hovered {
                    ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                }
                // Klick aktiviert (nur, wenn es kein Reorder-Drag war).
                let was_drag = self.tab_drag.as_ref().is_some_and(|d| d.moved);
                if body_it.clicked && !was_drag {
                    app.timeline.set_active(id);
                }
                if body_it.double_clicked {
                    app.app.rename_sequence = Some(id.clone());
                }
                if close_it.clicked || (ui.input.middle_pressed && tab.contains(mouse)) {
                    app.timeline.close_tab(id);
                }
                if body_it.right_clicked {
                    app.app.focused_panel = "timeline".into();
                    let last = app.timeline.len() <= 1;
                    app.context_menu.show(mouse.x, mouse.y, sequence_tab_menu(id, last));
                }
                if body_it.hovered && ui.input.left_pressed {
                    self.tab_drag = Some(TabDrag { id: id.clone(), origin_x: mouse.x, moved: false });
                }
            }
            x = tab.right() + 4.0;
        }

        // „+“ — neue Sequenz.
        let plus = Rect::new(x + 2.0, bar.y + (bar.h - 20.0) / 2.0, 20.0, 20.0);
        let plus_id = ui.id("tl.tabnew");
        let plus_it = ui.interact(plus_id, plus);
        if plus_it.hovered {
            ui.fill_rounded(plus, theme::RADIUS_XS, theme::SURFACE_2);
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            ui.tooltip(plus_id, plus, "Neue Sequenz");
        }
        ui.icon("plus", plus, 13.0, if plus_it.hovered { theme::TEXT_1 } else { theme::TEXT_3 });
        if plus_it.clicked {
            ui.run_command("sequence.new");
        }
        ui.pop_clip();

        // Reorder: Bewegung erkennen; beim Loslassen einmalig umsortieren.
        if let Some(drag) = self.tab_drag.as_mut() {
            if (mouse.x - drag.origin_x).abs() > 4.0 {
                drag.moved = true;
            }
        }
        if !ui.input.left_down {
            if let Some(drag) = self.tab_drag.take() {
                if drag.moved {
                    let mut target = 0usize;
                    for (tid, r) in tab_rects.iter() {
                        if tid == &drag.id {
                            continue;
                        }
                        if mouse.x >= r.x + r.w / 2.0 {
                            target += 1;
                        }
                    }
                    app.timeline.reorder_tab(&drag.id, target);
                }
            }
        }
    }

    // ------------------------------------------- Übergangs-Dauer-Eingabe

    /// Dauer-Eingabe für einen Übergang öffnen (Doppelklick/Kontextmenü).
    fn open_trans_editor(&mut self, ui: &mut Ui, app: &AppState, id: &str) {
        let Some(tr) = app.timeline.transition(id) else { return };
        let mut input = TextInputState::default();
        input.set_text(format!("{:.2}", tr.duration).replace('.', ","));
        // Auswahl komplett, damit Tippen den Wert ersetzt.
        input.sel_start = 0;
        self.trans_editor = Some((id.to_string(), input));
        // Tastaturfokus direkt auf das Feld (gleiche ID wie in render).
        ui.persist.keyboard_focus = ui.id(("tl.transdur", id));
    }

    /// Kleine Eingabebox nahe dem Übergangs-Band zeichnen und auswerten.
    fn render_trans_editor(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        trans_rects: &[(String, Rect, bool)],
        viewport: Rect,
    ) {
        let Some((id, mut input)) = self.trans_editor.take() else { return };
        if app.timeline.transition(&id).is_none() {
            return; // Übergang weg (Undo etc.) → Editor schließen
        }
        // Esc bricht ab.
        if ui
            .input
            .keys
            .iter()
            .any(|k| k.key == raylib::consts::KeyboardKey::KEY_ESCAPE)
        {
            return;
        }
        let anchor = trans_rects
            .iter()
            .find(|(tid, _, _)| tid == &id)
            .map(|(_, r, _)| *r)
            .unwrap_or_else(|| viewport.center_box(0.0, 0.0));
        let (w, h) = (216.0, 64.0);
        let x = (anchor.x + anchor.w / 2.0 - w / 2.0)
            .clamp(viewport.x + 4.0, viewport.right() - w - 4.0);
        let y = (anchor.bottom() + 6.0).min(viewport.bottom() - h - 4.0);
        let rect = Rect::new(x, y, w, h);
        ui.fill_rounded(rect, theme::RADIUS_MD, theme::SURFACE_2);
        ui.stroke_rounded(rect, theme::RADIUS_MD, 1.0, theme::LINE_STRONG);
        let inner = rect.inset_xy(10.0, 0.0);
        let label_row = Rect::new(inner.x, rect.y + 6.0, inner.w, 16.0);
        ui.text_left(
            "Übergangsdauer (Sekunden)",
            label_row,
            theme::TEXT_2,
            FontKind::Sans12,
        );
        let field = Rect::new(inner.x, rect.y + 28.0, inner.w, 24.0);
        let result = input.show(ui, ("tl.transdur", id.as_str()), field, "z. B. 1,0");
        if result.submitted {
            if let Ok(v) = input.text.trim().replace(',', ".").parse::<f64>() {
                app.timeline.set_transition_duration(&id, v);
            }
            return; // Editor schließen
        }
        // Klick außerhalb der Box schließt ohne Übernahme.
        if ui.input.left_pressed && !rect.contains(ui.input.mouse) {
            return;
        }
        self.trans_editor = Some((id, input));
    }
}

// ------------------------------------------------------ Übergangs-Helfer

/// Übergangs-Band über dem Schnitt zeichnen (Premiere-Optik: Fläche mit
/// Diagonale + Label).
fn draw_transition_band(ui: &mut Ui, tr: &Transition, band: Rect, selected: bool, locked: bool) {
    let alpha = if locked { 128 } else { 255 };
    ui.push_clip(band);
    // Abdunkeln + Akzentfläche, damit das Band über Clips/Wellenform lesbar ist.
    ui.fill_rounded(band, theme::RADIUS_SM, theme::with_alpha(theme::SURFACE_0, 150));
    ui.fill_rounded(band, theme::RADIUS_SM, theme::with_alpha(theme::ACCENT, 46));
    // Diagonale von unten-links nach oben-rechts (Blendverlauf-Symbol).
    ui.line(
        v2(band.x, band.bottom() - 1.0),
        v2(band.right(), band.y + 1.0),
        1.0,
        theme::with_alpha(theme::ACCENT, 140_u8.min(alpha)),
    );
    if band.w > 56.0 {
        let label = ui
            .font(FontKind::Sans12)
            .ellipsize(tr.kind.label(), band.w - 12.0);
        ui.text_centered(&label, band, theme::with_alpha(theme::TEXT_1, alpha), FontKind::Sans12);
    }
    ui.pop_clip();
    ui.stroke_rounded(band, theme::RADIUS_SM, 1.0, theme::with_alpha(theme::ACCENT, alpha));
    if selected {
        ui.stroke_rounded(band.inset(-1.0), theme::RADIUS_SM, 2.0, theme::TEXT_1);
    }
}

/// Ziel-Kante eines Übergangs-Drops: Clip unter der Maus + die der Maus
/// nächste Schnittkante; None bei gesperrter Spur oder falscher Spurart.
fn transition_drop_target(
    clips: &[TimelineClip],
    lane_rects: &[(TimelineTrack, Rect)],
    mouse_y: f32,
    t: f64,
    zoom: f64,
    kind: TransitionKind,
) -> Option<(TimelineClip, TimelineTrack, TrimEdge)> {
    let (clip, track, _) = clip_under_mouse(clips, lane_rects, mouse_y, t, zoom)?;
    if track.locked
        || track.kind == TrackKind::Subtitle
        || kind.is_audio() != (track.kind == TrackKind::Audio)
    {
        return None;
    }
    let edge = if (t - clip.start) <= (clip.end() - t) {
        TrimEdge::Start
    } else {
        TrimEdge::End
    };
    Some((clip, track, edge))
}

/// Drop-Vorschau: Übergangsfenster an der Zielkante (rot, wenn die Kante
/// kein Material für einen Übergang hergibt).
fn draw_transition_drop_preview(
    ui: &mut Ui,
    app: &AppState,
    clip: &TimelineClip,
    edge: TrimEdge,
    _kind: TransitionKind,
    lane: Rect,
    time_x: impl Fn(f64) -> f32,
) {
    let clips = &app.timeline.clips;
    let neighbor = match edge {
        TrimEdge::Start => clips
            .iter()
            .find(|c| c.track_id == clip.track_id && (c.end() - clip.start).abs() < EPS),
        TrimEdge::End => clips
            .iter()
            .find(|c| c.track_id == clip.track_id && (c.start - clip.end()).abs() < EPS),
    };
    let (from, to) = match edge {
        TrimEdge::Start => (neighbor, Some(clip)),
        TrimEdge::End => (Some(clip), neighbor),
    };
    let cut = match edge {
        TrimEdge::Start => clip.start,
        TrimEdge::End => clip.end(),
    };
    let max = transitions::max_duration(from, to, TransitionAlignment::Center);
    if max < MIN_CLIP_DURATION {
        // Kein Material an dieser Kante: rote Markierung.
        let x = time_x(cut);
        ui.fill(Rect::new(x - 1.0, lane.y + 2.0, 3.0, lane.h - 4.0), theme::DANGER);
        return;
    }
    let dur = DEFAULT_TRANSITION_DURATION.min(max).max(MIN_CLIP_DURATION);
    let Some((w0, w1)) = transitions::window(from, to, TransitionAlignment::Center, dur) else {
        return;
    };
    let x0 = time_x(w0);
    let x1 = time_x(w1);
    let rect = Rect::new(x0, lane.y + 2.0, (x1 - x0).max(6.0), lane.h - 4.0);
    ui.fill_rounded(rect, theme::RADIUS_SM, theme::with_alpha(theme::ACCENT, 64));
    ui.stroke_rounded(rect, theme::RADIUS_SM, 1.0, theme::ACCENT);
    // Schnittkante hervorheben.
    ui.fill(Rect::new(time_x(cut), lane.y + 2.0, 1.0, lane.h - 4.0), theme::ACCENT);
}

/// Kontextmenü eines Sequenz-Tabs: Umbenennen, Duplizieren, Einstellungen,
/// Löschen (nur wenn mehr als eine Sequenz existiert).
fn sequence_tab_menu(seq_id: &str, is_last: bool) -> Vec<MenuEntry> {
    let arg = serde_json::json!({ "sequenceId": seq_id });
    let mut items = vec![
        MenuEntry::Item(
            MenuItem::command("sequence.rename")
                .with_icon("type")
                .with_args(arg.clone()),
        ),
        MenuEntry::Item(
            MenuItem::command("sequence.duplicate")
                .with_icon("copy")
                .with_args(arg.clone()),
        ),
        MenuEntry::Item(MenuItem::command("sequence.settings").with_icon("sliders-horizontal")),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem::command("sequence.new").with_icon("plus")),
    ];
    if !is_last {
        items.push(MenuEntry::Separator);
        items.push(MenuEntry::Item(
            MenuItem::command("sequence.delete")
                .with_icon("trash-2")
                .with_args(arg),
        ));
    }
    items
}

/// Kontextmenü eines Übergangs: Dauer, Ausrichtung, Richtung, Ersetzen,
/// Entfernen.
fn transition_context_menu(_app: &AppState, tr: &Transition) -> Vec<MenuEntry> {
    let dur_label = format!("Dauer ändern… ({:.2} s)", tr.duration).replace('.', ",");
    let mut items: Vec<MenuEntry> = vec![MenuEntry::Item(
        MenuItem::custom(
            &dur_label,
            CustomAction::TransitionEditDuration { id: tr.id.clone() },
        )
        .with_icon("timer"),
    )];
    if tr.is_two_sided() {
        items.push(MenuEntry::Submenu {
            label: "Ausrichtung".into(),
            icon: Some("arrow-left-right"),
            items: TransitionAlignment::ALL
                .iter()
                .map(|a| {
                    MenuEntry::Item(
                        MenuItem::custom(
                            a.label(),
                            CustomAction::TransitionAlign {
                                id: tr.id.clone(),
                                alignment: *a,
                            },
                        )
                        .with_checked(tr.alignment == *a),
                    )
                })
                .collect(),
        });
    }
    if tr.kind.directional() {
        items.push(MenuEntry::Submenu {
            label: "Richtung".into(),
            icon: Some("move"),
            items: TransitionDirection::ALL
                .iter()
                .map(|d| {
                    MenuEntry::Item(
                        MenuItem::custom(
                            d.label(),
                            CustomAction::TransitionDirection {
                                id: tr.id.clone(),
                                direction: *d,
                            },
                        )
                        .with_checked(tr.direction == *d),
                    )
                })
                .collect(),
        });
    }
    let replacements: Vec<MenuEntry> = TransitionKind::ALL
        .iter()
        .filter(|k| k.is_audio() == tr.kind.is_audio() && **k != tr.kind)
        .map(|k| {
            MenuEntry::Item(
                MenuItem::custom(
                    k.label(),
                    CustomAction::TransitionReplace {
                        id: tr.id.clone(),
                        kind: *k,
                    },
                )
                .with_icon(k.icon()),
            )
        })
        .collect();
    items.push(MenuEntry::Submenu {
        label: "Ersetzen durch".into(),
        icon: Some("repeat"),
        items: replacements,
    });
    items.push(MenuEntry::Separator);
    items.push(MenuEntry::Item(
        MenuItem::custom(
            "Übergang entfernen",
            CustomAction::TransitionRemove { id: tr.id.clone() },
        )
        .with_icon("trash-2")
        .with_danger(),
    ));
    items
}

// ---------------------------------------------------------- Kontextmenüs

fn clip_context_menu(app: &AppState, clip: &TimelineClip, t: f64) -> Vec<MenuEntry> {
    let selected: Vec<&TimelineClip> = app
        .timeline
        .clips
        .iter()
        .filter(|c| app.timeline.selected_clip_ids.contains(&c.id))
        .collect();
    let all_enabled = selected.iter().all(|c| c.enabled);
    let any_linked = selected.iter().any(|c| c.link_id.is_some());
    let has_asset = app.media.asset(&clip.asset_id).is_some();
    let audio_selected = selected.iter().any(|c| c.kind == TrackKind::Audio);
    let mut entries = vec![
        MenuEntry::Item(
            MenuItem::custom(
                "Hier schneiden",
                CustomAction::TimelineSplitAt {
                    t,
                    clip_id: clip.id.clone(),
                },
            )
            .with_icon("scissors"),
        ),
        MenuEntry::Item(MenuItem::command("timeline.splitAtPlayhead")),
        MenuEntry::Separator,
        MenuEntry::Item(
            MenuItem::custom(
                "Clip-Marker hier hinzufügen",
                CustomAction::MarkerAddClipAt {
                    clip_id: clip.id.clone(),
                    t,
                },
            )
            .with_icon("bookmark"),
        ),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem::command("clip.speedDuration").with_icon("gauge")),
        MenuEntry::Item(MenuItem::command("clip.freezeFrame").with_icon("pause")),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem::command("timeline.copy").with_icon("copy")),
        MenuEntry::Item(MenuItem::command("timeline.cut")),
        MenuEntry::Item(MenuItem::command("timeline.paste").with_icon("clipboard-paste")),
        MenuEntry::Separator,
        MenuEntry::Item(
            MenuItem::command("timeline.toggleClipEnabled")
                .with_icon("eye")
                .with_checked(all_enabled),
        ),
        MenuEntry::Item(
            MenuItem::command("timeline.toggleLink")
                .with_icon("link-2")
                .with_checked(any_linked),
        ),
        MenuEntry::Separator,
        MenuEntry::Item(
            MenuItem::command("window.openPanel.effectControls")
                .with_label("Effekteinstellungen öffnen")
                .with_icon("sliders-horizontal"),
        ),
        MenuEntry::Item(MenuItem::command("clip.resetMotion").with_icon("rotate-ccw")),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem::command("clip.copyAttributes").with_icon("copy")),
        MenuEntry::Item(MenuItem::command("clip.pasteAttributes").with_icon("clipboard-paste")),
        MenuEntry::Item(MenuItem::command("color.copyGrade").with_icon("palette")),
        MenuEntry::Item(
            MenuItem::command("color.pasteGrade")
                .with_icon("clipboard-paste")
                .with_disabled(app.grade_clipboard.is_none()),
        ),
        MenuEntry::Item(MenuItem::command("clip.toggleEffects").with_icon("zap")),
        MenuEntry::Item(
            MenuItem::command("clip.removeAllEffects")
                .with_icon("trash-2")
                .with_disabled(!selected.iter().any(|c| !c.effects.is_empty())),
        ),
        MenuEntry::Separator,
        MenuEntry::Item(
            MenuItem::custom(
                "Im Medien-Browser anzeigen",
                CustomAction::MediaShowInBrowser {
                    asset_id: clip.asset_id.clone(),
                },
            )
            .with_icon("folder-open")
            .with_disabled(!has_asset),
        ),
        MenuEntry::Item(
            MenuItem::command("media.openInSource")
                .with_label("In Quellmonitor laden")
                .with_icon("film")
                .with_args(serde_json::json!({ "assetId": clip.asset_id }))
                .with_disabled(!has_asset),
        ),
        MenuEntry::Separator,
        MenuEntry::Item(
            MenuItem::command("timeline.deleteSelected")
                .with_icon("trash-2")
                .with_danger(),
        ),
        MenuEntry::Item(MenuItem::command("timeline.rippleDelete").with_danger()),
    ];
    if audio_selected {
        // Audio-Sektion vor dem Lösch-Block einsortieren.
        let danger_at = entries.len() - 2;
        let gain = if clip.kind == TrackKind::Audio {
            clip.gain_db
        } else {
            selected
                .iter()
                .find(|c| c.kind == TrackKind::Audio)
                .map(|c| c.gain_db)
                .unwrap_or(0.0)
        };
        entries.splice(
            danger_at..danger_at,
            [
                MenuEntry::Item(MenuItem::command("timeline.clipGainUp").with_icon("plus")),
                MenuEntry::Item(MenuItem::command("timeline.clipGainDown").with_icon("minus")),
                MenuEntry::Item(
                    MenuItem::command("timeline.clipGainReset")
                        .with_label(&format!(
                            "Clip-Verstärkung zurücksetzen ({:+.1} dB)",
                            gain
                        ))
                        .with_icon("rotate-ccw")
                        .with_disabled(gain == 0.0),
                ),
                MenuEntry::Separator,
            ],
        );
    }
    entries
}

fn lane_context_menu(app: &AppState, track: Option<&TimelineTrack>, t: f64) -> Vec<MenuEntry> {
    let mut items = vec![
        MenuEntry::Item(
            MenuItem::custom("Hier einfügen", CustomAction::TimelinePasteAt { t })
                .with_icon("clipboard-paste")
                .with_disabled(app.timeline.clipboard.is_empty()),
        ),
        MenuEntry::Item(MenuItem::command("timeline.selectAll")),
        MenuEntry::Item(MenuItem::command("timeline.deselectAll")),
        MenuEntry::Separator,
        MenuEntry::Item(
            MenuItem::command("timeline.toggleSnapping")
                .with_icon("magnet")
                .with_checked(app.timeline.snapping),
        ),
        MenuEntry::Item(MenuItem::command("timeline.zoomFit").with_icon("maximize-2")),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem::command("timeline.addVideoTrack").with_icon("plus")),
        MenuEntry::Item(MenuItem::command("timeline.addAudioTrack").with_icon("plus")),
    ];
    if let Some(track) = track {
        items.push(MenuEntry::Item(
            MenuItem::command("timeline.removeTrack")
                .with_label(&format!(
                    "Spur {} entfernen",
                    track_name(track, &app.timeline.tracks)
                ))
                .with_args(serde_json::json!({ "trackId": track.id }))
                .with_icon("trash-2")
                .with_danger(),
        ));
    }
    items
}

/// Tooltip-Text der Spur-Header-Toggles.
fn track_flag_tip(flag: TrackFlag, kind: TrackKind) -> &'static str {
    match flag {
        TrackFlag::Muted => {
            if kind == TrackKind::Subtitle {
                "Spur ausblenden"
            } else {
                "Stummschalten"
            }
        }
        TrackFlag::Solo => "Solo",
        TrackFlag::Locked => "Spur sperren",
        TrackFlag::SyncLock => "Sync-Lock — rippelt bei Insert/Extract mit",
        TrackFlag::Targeted => "Spur anvisieren (Lift/Extract/Match Frame)",
    }
}

fn track_header_menu(track: &TimelineTrack, name: &str) -> Vec<MenuEntry> {
    let mut items: Vec<MenuEntry> = Vec::new();
    if track.kind == TrackKind::Subtitle {
        items.push(MenuEntry::Item(
            MenuItem::custom(
                "Ausblenden",
                CustomAction::TimelineToggleTrackFlag {
                    track_id: track.id.clone(),
                    flag: TrackFlag::Muted,
                },
            )
            .with_checked(track.muted),
        ));
    } else {
        items.push(MenuEntry::Item(
            MenuItem::custom(
                "Stummschalten",
                CustomAction::TimelineToggleTrackFlag {
                    track_id: track.id.clone(),
                    flag: TrackFlag::Muted,
                },
            )
            .with_checked(track.muted),
        ));
        items.push(MenuEntry::Item(
            MenuItem::custom(
                "Solo",
                CustomAction::TimelineToggleTrackFlag {
                    track_id: track.id.clone(),
                    flag: TrackFlag::Solo,
                },
            )
            .with_checked(track.solo),
        ));
        items.push(MenuEntry::Separator);
        items.push(MenuEntry::Item(
            MenuItem::custom(
                "Source-Patch (Insert/Überschreiben-Ziel)",
                CustomAction::TimelineToggleSourcePatch {
                    track_id: track.id.clone(),
                },
            )
            .with_checked(track.source_patched),
        ));
        items.push(MenuEntry::Item(
            MenuItem::custom(
                "Anvisieren (Lift/Extract/Match)",
                CustomAction::TimelineToggleTrackFlag {
                    track_id: track.id.clone(),
                    flag: TrackFlag::Targeted,
                },
            )
            .with_checked(track.targeted),
        ));
        items.push(MenuEntry::Item(
            MenuItem::custom(
                "Sync-Lock",
                CustomAction::TimelineToggleTrackFlag {
                    track_id: track.id.clone(),
                    flag: TrackFlag::SyncLock,
                },
            )
            .with_checked(track.sync_lock),
        ));
        items.push(MenuEntry::Separator);
    }
    items.push(MenuEntry::Item(
        MenuItem::custom(
            "Sperren",
            CustomAction::TimelineToggleTrackFlag {
                track_id: track.id.clone(),
                flag: TrackFlag::Locked,
            },
        )
        .with_checked(track.locked),
    ));
    items.push(MenuEntry::Separator);
    items.push(MenuEntry::Item(
        MenuItem::command("timeline.addVideoTrack").with_icon("plus"),
    ));
    items.push(MenuEntry::Item(
        MenuItem::command("timeline.addAudioTrack").with_icon("plus"),
    ));
    items.push(MenuEntry::Item(
        MenuItem::command("subtitle.addTrack").with_icon("plus"),
    ));
    items.push(MenuEntry::Item(
        MenuItem::command("timeline.removeTrack")
            .with_label(&format!("Spur {name} entfernen"))
            .with_args(serde_json::json!({ "trackId": track.id }))
            .with_icon("trash-2")
            .with_danger(),
    ));
    items
}

fn ruler_context_menu(t: f64, has_loop: bool) -> Vec<MenuEntry> {
    vec![
        MenuEntry::Item(
            MenuItem::custom("Marker hier hinzufügen", CustomAction::MarkerAddAt { t })
                .with_icon("bookmark"),
        ),
        MenuEntry::Item(MenuItem::command("marker.add").with_icon("bookmark-plus")),
        MenuEntry::Separator,
        MenuEntry::Item(
            MenuItem::custom("In-Punkt hier setzen", CustomAction::TimelineSetInAt { t })
                .with_icon("arrow-right-from-line"),
        ),
        MenuEntry::Item(
            MenuItem::custom("Out-Punkt hier setzen", CustomAction::TimelineSetOutAt { t })
                .with_icon("arrow-right-to-line"),
        ),
        MenuEntry::Separator,
        MenuEntry::Item(
            MenuItem::custom("Loop-Bereich entfernen", CustomAction::TimelineClearInOut)
                .with_icon("x")
                .with_disabled(!has_loop),
        ),
    ]
}

/// Marker-Symbol (Pentagon) zeichnen: oben flach, unten spitz — wie
/// in Premiere; `band_top` ist die Oberkante des Marker-Bandes.
fn draw_marker_symbol(ui: &mut Ui, x: f32, band_top: f32, col: raylib::prelude::Color) {
    let hw = 4.0;
    let body_h = 6.0;
    ui.fill(Rect::new(x - hw, band_top, hw * 2.0, body_h), col);
    // Spitze nach unten.
    ui.triangle(
        v2(x - hw, band_top + body_h),
        v2(x, band_top + body_h + 4.5),
        v2(x + hw, band_top + body_h),
        col,
    );
    // Dünner dunkler Rahmen oben für Kontrast auf hellen Farben.
    ui.fill(Rect::new(x - hw, band_top, hw * 2.0, 1.0), theme::with_alpha(theme::BLACK, 60));
}

/// Tooltip-Text eines Markers: Name (oder Timecode) + optionale Notiz.
fn marker_tooltip(name: &str, note: &str, tc: &str) -> String {
    let head = if name.trim().is_empty() {
        format!("Marker • {tc}")
    } else {
        format!("{} • {tc}", name.trim())
    };
    if note.trim().is_empty() {
        head
    } else {
        format!("{head}\n{}", note.trim())
    }
}

/// Marker-Bearbeiten-Dialog für einen Sequenz-Marker öffnen.
fn open_marker_dialog(app: &mut AppState, marker_id: String) {
    app.app.marker_editor = Some(MarkerEditTarget {
        scope: MarkerScope::Sequence,
        marker_id,
    });
    app.app.open_dialog = Some(DialogId::Marker);
}

#[cfg(test)]
mod snap_tests {
    use super::*;
    use crate::core::marker::Marker;

    #[test]
    fn snap_targets_include_sequence_markers_and_range_edges() {
        let mut app = AppState::default();
        // Punktmarker bei 5 s und Bereichsmarker 10–14 s.
        app.timeline.markers.push(Marker::new(5.0));
        let mut range = Marker::new(10.0);
        range.duration = 4.0;
        app.timeline.markers.push(range);

        let mut panel = TimelinePanel::default();
        panel.collect_snap_targets(&app, &[]);

        let has = |t: f64| panel.snap_targets.iter().any(|&s| (s - t).abs() < 1e-9);
        assert!(has(5.0), "Punktmarkerzeit fehlt");
        assert!(has(10.0), "Bereichsmarker-Start fehlt");
        assert!(has(14.0), "Bereichsmarker-Ende fehlt");
    }

    #[test]
    fn sub_frame_marker_target_is_frame_aligned() {
        // Default = 25 fps ⇒ 0,04 s/Frame. Ein sub-frame gesetzter Marker
        // (z. B. via roher EDITRON_TEST_MARKER-Dauer) darf kein Snap-Ziel
        // zwischen zwei Frames erzeugen, sonst landet die Clip-Kante off-grid.
        let mut app = AppState::default();
        let mut m = Marker::new(5.03); // → Frame 126 = 5,04 s
        m.duration = 2.01; // end 7,04 → Frame 176 = 7,04 s
        app.timeline.markers.push(m);

        let mut panel = TimelinePanel::default();
        panel.collect_snap_targets(&app, &[]);

        // Jedes Marker-Ziel sitzt exakt auf einem Frame (idempotent).
        for &s in &panel.snap_targets {
            assert!(
                (s - app.timeline.snap_to_frame(s)).abs() < 1e-9,
                "Snap-Ziel {s} liegt nicht auf dem Frame-Raster",
            );
        }
        let has = |t: f64| panel.snap_targets.iter().any(|&s| (s - t).abs() < 1e-9);
        assert!(has(5.04), "Markerzeit nicht frame-gerastet");
        assert!(!has(5.03), "rohe Sub-Frame-Markerzeit darf kein Ziel sein");
    }

    #[test]
    fn marker_snap_respects_snapping_toggle() {
        let mut app = AppState::default();
        app.timeline.markers.push(Marker::new(5.0));
        let mut panel = TimelinePanel::default();
        panel.collect_snap_targets(&app, &[]);

        // Kante knapp neben dem Marker (innerhalb der Schwelle bei 40 px/s).
        let edge = 5.05;

        // Snapping an: Kante rastet exakt auf den Marker.
        app.timeline.snapping = true;
        let (delta, target) = panel.snap_adjust(&app, &[edge], 0.0);
        assert_eq!(target, Some(5.0));
        assert!((edge + delta - 5.0).abs() < 1e-9);

        // Snapping aus (Taste S): keine Anpassung.
        app.timeline.snapping = false;
        let (delta_off, target_off) = panel.snap_adjust(&app, &[edge], 0.0);
        assert_eq!(target_off, None);
        assert_eq!(delta_off, 0.0);
    }
}
