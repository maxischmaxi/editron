//! Die Timeline: vertikale Werkzeugleiste, Timecode-Kopfzeile, Lineal mit
//! Loop-Bereich, editierbare Spuren mit Clips, Playhead, Snapping mit
//! Hilfslinie, Marquee, Pan/Zoom, Drag&Drop aus dem Medien-Browser und
//! Kontextmenüs. Alle Editier-Operationen laufen über den TimelineStore
//! (mit Undo); während einer Drag-Geste rendert das Panel eine Vorschau.

use crate::core::timecode::{format_duration, format_timecode};
use crate::core::timeline::{
    expand_links, plan_asset_placements, sequence_end, track_name, trim_range, PlannedPlacement,
    TimelineClip, TimelineTrack, TrackFlag, TrackKind, TrimEdge, MIN_CLIP_DURATION, SEQUENCE_FPS,
    SelectMode,
};
use crate::overlays::context_menu::{CustomAction, MenuEntry, MenuItem};
use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::{v2, Rect};
use crate::ui::widgets::scroll::scrollbar;
use crate::ui::widgets::IconButton;
use crate::ui::{DragPayload, FontKind, Ui};
use raylib::consts::MouseCursor;
use raylib::prelude::RaylibDraw;

const TRACK_HEADER_W: f32 = 96.0; // w-24
const TOOLBAR_W: f32 = 36.0; // w-9
const HEADER_H: f32 = 36.0; // h-9
const RULER_H: f32 = 28.0; // h-7
const VIDEO_H: f32 = 48.0; // h-12
const AUDIO_H: f32 = 40.0; // h-10
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
        }
    }
}

fn track_height(track: &TimelineTrack) -> f32 {
    match track.kind {
        TrackKind::Video => VIDEO_H,
        TrackKind::Audio => AUDIO_H,
    }
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

fn apply_trim_preview(clip: &TimelineClip, edge: TrimEdge, delta: f64) -> TimelineClip {
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
                if dist <= threshold && best.map_or(true, |(d, _, _)| dist < d) {
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
                            apply_trim_preview(c, *edge, *delta_sec)
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
                        apply_trim_preview(c, TrimEdge::End, *delta_sec)
                    } else if c.id == *right_id {
                        apply_trim_preview(c, TrimEdge::Start, *delta_sec)
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
                            m.src_in = c.src_in + delta_sec;
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

        // ---------------- Kopfzeile (h-9: Timecode + Snapping/Zoom) ---------
        let header = area.cut_top(HEADER_H);
        ui.hline(header.x, header.bottom() - 1.0, header.w, theme::LINE);
        let header_inner = header.inset_xy(12.0, 0.0);
        let tc = format_timecode(app.timeline.playhead_sec, SEQUENCE_FPS);
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
            lane_rects.push((track.clone(), Rect::new(lane.x, row_y, lane.w, h)));
            row_y += h;
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
            && self.ruler_drag.is_none()
            && ui.mouse_in(viewport)
            && mouse.y >= viewport.y + RULER_H - scroll_y
        {
            self.handle_lane_input(
                ui,
                app,
                &preview,
                &lane_rects,
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
            head_y += h;
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

        // Playhead (w-px accent + Dreieck) über Lineal + Spuren
        let ph_x = time_x(app.timeline.playhead_sec);
        if ph_x >= viewport.x + TRACK_HEADER_W - 4.0 {
            ui.fill(
                Rect::new(ph_x, viewport.y, 1.0, viewport.h),
                theme::ACCENT,
            );
            let tri_y = viewport.y - scroll_y;
            ui.d.draw_triangle(
                v2(ph_x - 3.5, tri_y),
                v2(ph_x + 4.5, tri_y),
                v2(ph_x + 0.5, tri_y + 6.0),
                theme::ACCENT,
            );
        }

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
                            clip.duration
                        };
                        let from = (clip.src_in / total) * peaks.len() as f64;
                        let span = ((clip.duration / total) * peaks.len() as f64).max(1.0);
                        let h = rect.h;
                        let w = rect.w as i32;
                        let step = (1.0 / rect.w.max(1.0) * span as f32).max(0.0);
                        let mut idx_f = from as f32;
                        let color = theme::with_alpha(theme::SUCCESS, 128_u8.min(alpha));
                        for x in 0..w {
                            let idx = (idx_f as usize).min(peaks.len() - 1);
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
                        services.request_waveform(&asset.id, &asset.path, 1200);
                    }
                }
            }
        } else if let Some(asset) = asset {
            // Thumbnail links (h-full w-auto, opacity-50)
            if rect.w > 48.0 {
                if let Some(thumb) = &asset.thumbnail_path {
                    if let Some(tex) = ui.textures.get(thumb) {
                        let tw = tex.width as f32 / tex.height as f32 * rect.h;
                        let dest = Rect::new(rect.x, rect.y, tw, rect.h);
                        ui.d.draw_texture_pro(
                            tex,
                            Rect::new(0.0, 0.0, tex.width as f32, tex.height as f32),
                            dest,
                            v2(0.0, 0.0),
                            0.0,
                            raylib::color::Color::new(255, 255, 255, 128_u8.min(alpha)),
                        );
                    } else {
                        ui.texture_requests.push(thumb.clone());
                    }
                }
            }
        }

        // Titelzeile: Offline-/Link-Icon + Name + Dauer (oben ausgerichtet)
        let mut inner = Rect::new(rect.x + 6.0, rect.y + 2.0, rect.w - 12.0, 16.0);
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
        let dur_label = format_duration(clip.duration);
        if rect.w > 88.0 {
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

        // Buttons rechts (size-3.5-Icons): Mute, Solo, Lock
        let buttons: [(&str, bool, raylib::color::Color, raylib::color::Color); 3] = [
            ("volume-2", track.muted, theme::WARNING, theme::with_alpha(theme::WARNING, 51)),
            ("headphones", track.solo, theme::SUCCESS, theme::with_alpha(theme::SUCCESS, 51)),
            ("lock", track.locked, theme::ACCENT, theme::ACCENT_SOFT),
        ];
        let flags = [TrackFlag::Muted, TrackFlag::Solo, TrackFlag::Locked];
        let mut bx = inner.right();
        for (i, (icon, active, fg, bg)) in buttons.iter().enumerate().rev() {
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
            }
            if it.clicked {
                app.timeline.toggle_track_flag(&track.id, flags[i]);
            }
            bx -= 4.0;
        }

        inner.w = (bx - inner.x).max(0.0);
        let display = ui.font(FontKind::Sans12Medium).ellipsize(&name, inner.w);
        ui.text_left(&display, inner, theme::TEXT_2, FontKind::Sans12Medium);

        // Rechtsklick: Spur-Menü
        if ui.mouse_in(head) && ui.input.right_pressed {
            let items = track_header_menu(track, &name);
            app.context_menu
                .show(ui.input.mouse.x, ui.input.mouse.y, items);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_lane_input(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        preview: &[TimelineClip],
        lane_rects: &[(TimelineTrack, Rect)],
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

            // Doppelklick: in den Quellmonitor laden
            if ui.input.double_click {
                if app.media.asset(&clip.asset_id).is_some() {
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
                    for c in &moving {
                        let lanes = match c.kind {
                            TrackKind::Video => &v_lanes,
                            TrackKind::Audio => &a_lanes,
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
                    for c in &targets {
                        let (lo, hi) = trim_range(c, *edge, &app.timeline.clips, !*ripple);
                        snapped = snapped.clamp(lo, hi);
                    }
                    *delta_sec = snapped;
                    *snap_time = st;
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
                    *delta_sec = (((mouse.x - *origin_x) / zoom as f32) as f64)
                        .clamp(lo_l.max(lo_r), hi_l.min(hi_r));
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
                    delta = delta.clamp(-c.src_in, c.src_duration - c.src_in - c.duration);
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
                    *delta_sec = (((mouse.x - *origin_x) / zoom as f32) as f64).max(-min_start);
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
        ui.pop_clip();

        // Interaktion: Scrub / Alt+Range / Kanten ziehen / Kontextmenü
        let edge_sec = EDGE_PX as f64 / zoom;
        if ui.mouse_in(time_area) {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
            let ruler_id = ui.id("tl.ruler");
            ui.set_hot(ruler_id);
            ui.tooltip(
                ruler_id,
                Rect::new(ui.input.mouse.x, time_area.y, 1.0, time_area.h),
                "Ziehen: Playhead • Alt+Ziehen: Loop-Bereich",
            );

            if ui.input.right_pressed {
                let t = pointer_time(ui.input.mouse.x).max(0.0);
                let has_loop =
                    app.timeline.in_point.is_some() || app.timeline.out_point.is_some();
                app.context_menu.show(
                    ui.input.mouse.x,
                    ui.input.mouse.y,
                    ruler_context_menu(t, has_loop),
                );
                return;
            }
            if ui.input.left_pressed {
                app.app.focused_panel = "timeline".into();
                let t = pointer_time(ui.input.mouse.x).max(0.0);
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
                    app.timeline.set_playhead(t);
                }
            }
        }

        if let Some(rd) = &self.ruler_drag {
            let t = pointer_time(ui.input.mouse.x).max(0.0);
            match rd {
                RulerDrag::Scrub => app.timeline.set_playhead(t),
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
            }
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
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
            MenuItem::custom(
                &format!("Spur {} entfernen", track_name(track, &app.timeline.tracks)),
                CustomAction::TimelineRemoveTrack {
                    track_id: track.id.clone(),
                },
            )
            .with_icon("trash-2")
            .with_danger(),
        ));
    }
    items
}

fn track_header_menu(track: &TimelineTrack, name: &str) -> Vec<MenuEntry> {
    vec![
        MenuEntry::Item(
            MenuItem::custom(
                "Stummschalten",
                CustomAction::TimelineToggleTrackFlag {
                    track_id: track.id.clone(),
                    flag: TrackFlag::Muted,
                },
            )
            .with_checked(track.muted),
        ),
        MenuEntry::Item(
            MenuItem::custom(
                "Solo",
                CustomAction::TimelineToggleTrackFlag {
                    track_id: track.id.clone(),
                    flag: TrackFlag::Solo,
                },
            )
            .with_checked(track.solo),
        ),
        MenuEntry::Item(
            MenuItem::custom(
                "Sperren",
                CustomAction::TimelineToggleTrackFlag {
                    track_id: track.id.clone(),
                    flag: TrackFlag::Locked,
                },
            )
            .with_checked(track.locked),
        ),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem::command("timeline.addVideoTrack").with_icon("plus")),
        MenuEntry::Item(MenuItem::command("timeline.addAudioTrack").with_icon("plus")),
        MenuEntry::Item(
            MenuItem::custom(
                &format!("Spur {name} entfernen"),
                CustomAction::TimelineRemoveTrack {
                    track_id: track.id.clone(),
                },
            )
            .with_icon("trash-2")
            .with_danger(),
        ),
    ]
}

fn ruler_context_menu(t: f64, has_loop: bool) -> Vec<MenuEntry> {
    vec![
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
