//! Transform-Gizmo im Programmmonitor: Auswahl-Rahmen um den selektierten
//! Video-Clip mit 8 Skalierungs-Handles (Ecken = proportional, Kanten =
//! je Achse) und Rotations-Griff über der Oberkante. Ziehen im Rahmen
//! verschiebt den Clip (Shift: Achse einrasten), Shift an Ecken verzerrt,
//! Shift beim Rotieren rastet auf 15°. Alle Änderungen laufen über die
//! fx-API der Timeline — bei aktiver Stopwatch entstehen Keyframes am
//! Playhead. Klick auf einen anderen Layer wählt dessen Clip aus.

use crate::core::animation::ParamId;
use crate::core::compose::{self, LayerQuad};
use crate::core::timeline::{SelectMode, TrackKind};
use crate::panels::monitor::ResolvedLayer;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::{v2, Rect};
use crate::ui::Ui;
use raylib::consts::MouseCursor;
use raylib::math::Vector2;

const HANDLE: f32 = 8.0;
const HANDLE_HIT: f32 = 7.0;
/// Abstand des Rotations-Griffs über der Oberkante (lokale Koordinaten).
const ROT_OFFSET: f64 = 24.0;
/// Erst ab dieser Bewegung wird die Geste „echt“ (ein Undo-Snapshot).
const DRAG_THRESHOLD: f32 = 2.0;

#[derive(Clone, Copy, PartialEq)]
enum DragKind {
    Move,
    /// 0–3 Ecken (TL, TR, BR, BL), 4–7 Kanten (T, R, B, L).
    Scale(usize),
    Rotate,
}

struct Drag {
    kind: DragKind,
    clip_id: String,
    start_mouse: Vector2,
    /// Ausgewertete Parameter bei Gestenbeginn.
    start_pos: (f64, f64),
    start_scale: (f64, f64),
    start_rot: f64,
    center: (f64, f64),
    /// Undo-Snapshot wird erst beim ersten echten Versatz angelegt.
    history_pushed: bool,
}

#[derive(Default)]
pub struct TransformGizmo {
    drag: Option<Drag>,
}

/// Mauspunkt in lokale (entrotierte) Koordinaten relativ zum Quad-Zentrum.
fn to_local(p: Vector2, center: (f64, f64), rot_deg: f64) -> (f64, f64) {
    let (s, c) = (-rot_deg.to_radians()).sin_cos();
    let dx = p.x as f64 - center.0;
    let dy = p.y as f64 - center.1;
    (dx * c - dy * s, dx * s + dy * c)
}

/// Lokalen Punkt zurück in Bildschirmkoordinaten.
fn to_screen(l: (f64, f64), center: (f64, f64), rot_deg: f64) -> Vector2 {
    let (s, c) = rot_deg.to_radians().sin_cos();
    v2(
        (center.0 + l.0 * c - l.1 * s) as f32,
        (center.1 + l.0 * s + l.1 * c) as f32,
    )
}

fn quad_contains(quad: &LayerQuad, p: Vector2) -> bool {
    let (lx, ly) = to_local(p, (quad.cx, quad.cy), quad.rot_deg);
    lx.abs() <= quad.w / 2.0 && ly.abs() <= quad.h / 2.0
}

/// Lokale Positionen der 8 Handles (Ecken, dann Kantenmitten).
fn handle_locals(quad: &LayerQuad) -> [(f64, f64); 8] {
    let (hw, hh) = (quad.w / 2.0, quad.h / 2.0);
    [
        (-hw, -hh),
        (hw, -hh),
        (hw, hh),
        (-hw, hh),
        (0.0, -hh),
        (hw, 0.0),
        (0.0, hh),
        (-hw, 0.0),
    ]
}

fn rotation_handle_local(quad: &LayerQuad) -> (f64, f64) {
    (0.0, -quad.h / 2.0 - ROT_OFFSET)
}

impl TransformGizmo {
    /// Nach dem Zeichnen der Bühne aufrufen; `stage` clippt die Darstellung,
    /// `canvas` ist das Programm-Frame (Bezugssystem der Prozent-Werte).
    pub fn update(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        stage: Rect,
        canvas: Rect,
        layers: &[ResolvedLayer],
    ) {
        let t = app.timeline.playhead_sec;

        // Laufende Geste fortführen/beenden.
        if let Some(drag) = &self.drag {
            if !ui.input.left_down {
                self.drag = None;
            } else {
                let drag_clip = drag.clip_id.clone();
                if app.timeline.clip(&drag_clip).is_some() {
                    self.continue_drag(ui, app, canvas);
                } else {
                    self.drag = None;
                }
            }
        }

        // Zielclip: erster ausgewählter Video-Clip unter dem Playhead.
        let target = app
            .timeline
            .selected_clip_ids
            .iter()
            .filter_map(|id| app.timeline.clip(id))
            .find(|c| c.kind == TrackKind::Video && c.enabled && t >= c.start && t < c.end())
            .map(|c| c.id.clone());

        let target_layer = target
            .as_deref()
            .and_then(|id| layers.iter().find(|l| l.clip_id == id));

        // ---- Interaktion (nur wenn keine Geste läuft) ----
        let mouse_in_stage = ui.mouse_in(stage);
        if self.drag.is_none() && mouse_in_stage && ui.nothing_active() {
            let mouse = ui.input.mouse;
            let mut hover_cursor: Option<MouseCursor> = None;
            let mut press_action: Option<DragKind> = None;

            if let Some(layer) = target_layer {
                let quad = &layer.quad;
                let center = (quad.cx, quad.cy);
                let rot = quad.rot_deg;
                // Handles zuerst (liegen über der Fläche).
                let mut hit_handle: Option<usize> = None;
                for (i, l) in handle_locals(quad).iter().enumerate() {
                    let p = to_screen(*l, center, rot);
                    if (mouse.x - p.x).abs() <= HANDLE_HIT && (mouse.y - p.y).abs() <= HANDLE_HIT {
                        hit_handle = Some(i);
                        break;
                    }
                }
                let rot_p = to_screen(rotation_handle_local(quad), center, rot);
                let on_rotate = hit_handle.is_none()
                    && ((mouse.x - rot_p.x).powi(2) + (mouse.y - rot_p.y).powi(2)).sqrt()
                        <= HANDLE_HIT;

                if let Some(i) = hit_handle {
                    hover_cursor = Some(handle_cursor(i, rot));
                    press_action = Some(DragKind::Scale(i));
                } else if on_rotate {
                    hover_cursor = Some(MouseCursor::MOUSE_CURSOR_CROSSHAIR);
                    press_action = Some(DragKind::Rotate);
                } else if quad_contains(quad, mouse) {
                    hover_cursor = Some(MouseCursor::MOUSE_CURSOR_RESIZE_ALL);
                    press_action = Some(DragKind::Move);
                }
            }

            if let Some(cursor) = hover_cursor {
                ui.want_cursor(cursor);
            }

            if ui.input.left_pressed {
                if let (Some(kind), Some(layer)) = (press_action, target_layer) {
                    let clip_id = layer.clip_id.clone();
                    self.start_drag(app, kind, &clip_id, layers, ui.input.mouse, t);
                } else {
                    // Klick auf einen anderen Layer wählt dessen Clip
                    // (oberster zuerst); Klick ins Leere lässt die Auswahl
                    // unangetastet (Fokus-Klicks sollen nichts zerstören).
                    if let Some(hit) = layers
                        .iter()
                        .rev()
                        .find(|l| quad_contains(&l.quad, ui.input.mouse))
                    {
                        let ids = vec![hit.clip_id.clone()];
                        app.timeline.select_clips(&ids, SelectMode::Replace, true);
                    }
                }
            }
        }

        // ---- Zeichnen ----
        if let Some(layer) = target_layer {
            ui.push_clip(stage);
            draw_gizmo(ui, &layer.quad, self.drag.is_some());
            ui.pop_clip();
        }
    }

    fn start_drag(
        &mut self,
        app: &mut AppState,
        kind: DragKind,
        clip_id: &str,
        layers: &[ResolvedLayer],
        mouse: Vector2,
        t: f64,
    ) {
        let Some(clip) = app.timeline.clip(clip_id) else { return };
        let Some(layer) = layers.iter().find(|l| l.clip_id == clip_id) else {
            return;
        };
        let media_t = compose::clip_media_time(clip, t);
        let fx = compose::eval_fx(&clip.fx, media_t);
        let quad = &layer.quad;
        self.drag = Some(Drag {
            kind,
            clip_id: clip_id.to_string(),
            start_mouse: mouse,
            start_pos: (fx.pos_x, fx.pos_y),
            start_scale: (fx.scale_x, fx.scale_y),
            start_rot: fx.rotation,
            center: (quad.cx, quad.cy),
            history_pushed: false,
        });
    }

    fn continue_drag(&mut self, ui: &mut Ui, app: &mut AppState, canvas: Rect) {
        let Some(drag) = self.drag.as_mut() else { return };
        let mouse = ui.input.mouse;
        let dx = mouse.x - drag.start_mouse.x;
        let dy = mouse.y - drag.start_mouse.y;
        if !drag.history_pushed {
            if (dx * dx + dy * dy).sqrt() < DRAG_THRESHOLD {
                return;
            }
            // Ein Undo-Snapshot pro Geste; Kanten-Drag entkoppelt die
            // Achsen (fx_set_uniform_scale legt dabei selbst den Snapshot an).
            let uniform = app
                .timeline
                .clip(&drag.clip_id)
                .map(|c| c.fx.uniform_scale)
                .unwrap_or(true);
            if matches!(drag.kind, DragKind::Scale(4..=7)) && uniform {
                app.timeline.fx_set_uniform_scale(&drag.clip_id, false);
            } else {
                app.timeline.begin_fx_edit();
            }
            drag.history_pushed = true;
        }

        let Some(clip) = app.timeline.clip(&drag.clip_id) else { return };
        let media_t = compose::clip_media_time(clip, app.timeline.playhead_sec);
        let uniform = clip.fx.uniform_scale;
        let shift = ui.input.shift;
        let clip_id = drag.clip_id.clone();

        match drag.kind {
            DragKind::Move => {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_ALL);
                let (mut ddx, mut ddy) = (dx as f64, dy as f64);
                if shift {
                    // Dominante Achse einrasten.
                    if ddx.abs() >= ddy.abs() {
                        ddy = 0.0;
                    } else {
                        ddx = 0.0;
                    }
                }
                let px = drag.start_pos.0 + ddx / canvas.w.max(1.0) as f64 * 100.0;
                let py = drag.start_pos.1 + ddy / canvas.h.max(1.0) as f64 * 100.0;
                app.timeline.fx_set_value_live(&clip_id, ParamId::PosX, media_t, px);
                app.timeline.fx_set_value_live(&clip_id, ParamId::PosY, media_t, py);
            }
            DragKind::Scale(handle) => {
                ui.want_cursor(handle_cursor(handle, drag.start_rot));
                let l0 = to_local(drag.start_mouse, drag.center, drag.start_rot);
                let l1 = to_local(mouse, drag.center, drag.start_rot);
                let min_scale = 1.0;
                match handle {
                    0..=3 => {
                        if uniform && !shift {
                            // Proportional: Verhältnis der Abstände zum Zentrum.
                            let d0 = (l0.0 * l0.0 + l0.1 * l0.1).sqrt().max(1e-6);
                            let d1 = (l1.0 * l1.0 + l1.1 * l1.1).sqrt();
                            let f = d1 / d0;
                            let s = (drag.start_scale.0 * f).max(min_scale);
                            app.timeline.fx_set_value_live(&clip_id, ParamId::ScaleX, media_t, s);
                            if !uniform {
                                app.timeline.fx_set_value_live(&clip_id, ParamId::ScaleY, media_t, s);
                            }
                        } else {
                            // Verzerren: Achsen unabhängig (Shift oder
                            // entkoppelte Skalierung).
                            if uniform {
                                app.timeline.fx_set_uniform_scale(&clip_id, false);
                            }
                            let fx0 = l0.0.abs().max(1e-6);
                            let fy0 = l0.1.abs().max(1e-6);
                            let sx = (drag.start_scale.0 * l1.0.abs() / fx0).max(min_scale);
                            let sy = (drag.start_scale.1 * l1.1.abs() / fy0).max(min_scale);
                            app.timeline.fx_set_value_live(&clip_id, ParamId::ScaleX, media_t, sx);
                            app.timeline.fx_set_value_live(&clip_id, ParamId::ScaleY, media_t, sy);
                        }
                    }
                    4 | 6 => {
                        let f0 = l0.1.abs().max(1e-6);
                        let sy = (drag.start_scale.1 * l1.1.abs() / f0).max(min_scale);
                        app.timeline.fx_set_value_live(&clip_id, ParamId::ScaleY, media_t, sy);
                    }
                    _ => {
                        let f0 = l0.0.abs().max(1e-6);
                        let sx = (drag.start_scale.0 * l1.0.abs() / f0).max(min_scale);
                        app.timeline.fx_set_value_live(&clip_id, ParamId::ScaleX, media_t, sx);
                    }
                }
            }
            DragKind::Rotate => {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_CROSSHAIR);
                let a0 = (drag.start_mouse.y as f64 - drag.center.1)
                    .atan2(drag.start_mouse.x as f64 - drag.center.0);
                let a1 = (mouse.y as f64 - drag.center.1).atan2(mouse.x as f64 - drag.center.0);
                let mut rot = drag.start_rot + (a1 - a0).to_degrees();
                if shift {
                    rot = (rot / 15.0).round() * 15.0;
                }
                app.timeline.fx_set_value_live(&clip_id, ParamId::Rotation, media_t, rot);
            }
        }
    }
}

/// Resize-Cursor passend zur (gerundeten) Rotation des Rahmens.
fn handle_cursor(handle: usize, rot_deg: f64) -> MouseCursor {
    // Bei starker Rotation stimmen die Achsen-Cursor nicht mehr — dann
    // generisch. Bis ±22,5° bleiben die gewohnten Richtungen.
    let tilted = ((rot_deg % 90.0) + 90.0) % 90.0;
    let tilted = tilted.min(90.0 - tilted) > 22.5;
    if tilted {
        return MouseCursor::MOUSE_CURSOR_RESIZE_ALL;
    }
    match handle {
        0 | 2 => MouseCursor::MOUSE_CURSOR_RESIZE_NWSE,
        1 | 3 => MouseCursor::MOUSE_CURSOR_RESIZE_NESW,
        4 | 6 => MouseCursor::MOUSE_CURSOR_RESIZE_NS,
        _ => MouseCursor::MOUSE_CURSOR_RESIZE_EW,
    }
}

fn draw_gizmo(ui: &mut Ui, quad: &LayerQuad, dragging: bool) {
    let center = (quad.cx, quad.cy);
    let rot = quad.rot_deg;
    let color = if dragging { theme::ACCENT_HOVER } else { theme::ACCENT };

    // Rahmen
    let corners = quad.corners().map(|(x, y)| v2(x as f32, y as f32));
    for i in 0..4 {
        ui.line(corners[i], corners[(i + 1) % 4], 1.5, color);
    }

    // Rotations-Griff: Linie von der Oberkante + Kreis.
    let top_mid = to_screen((0.0, -quad.h / 2.0), center, rot);
    let rot_p = to_screen(rotation_handle_local(quad), center, rot);
    ui.line(top_mid, rot_p, 1.5, color);
    ui.circle(rot_p, HANDLE / 2.0 + 1.0, theme::SURFACE_0);
    ui.circle_outline(rot_p, HANDLE / 2.0 + 1.0, color);

    // Handles (achsparallel, wie in den großen NLEs).
    for l in handle_locals(quad) {
        let p = to_screen(l, center, rot);
        let r = Rect::new(p.x - HANDLE / 2.0, p.y - HANDLE / 2.0, HANDLE, HANDLE);
        ui.fill(r, theme::SURFACE_0);
        ui.stroke(r, 1.5, color);
    }
}
