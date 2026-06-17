//! Masken-Editor im Programmmonitor: zeigt die gerade bearbeitete Effekt-Maske
//! (`AppStore::active_mask`) als editierbare Form über dem Clip — Umriss,
//! Feather-Andeutung und Handles. Ziehen verändert Mittelpunkt, Radien,
//! Rotation, Feather (Ellipse/Rechteck) bzw. die Stützpunkte (Polygon). Alle
//! Änderungen laufen über die `mask_*`-API der Timeline; ein Undo-Snapshot
//! entsteht beim ersten echten Versatz der Geste.
//!
//! Maskenkoordinaten sind normierte Inhalts-UVs (0..1); das Layer-Quad bildet
//! sie auf Bildschirmkoordinaten ab (inkl. Clip-Transform/Rotation), genau wie
//! der Effekt-Shader die Maske auf den Clip-Inhalt abbildet.

use crate::core::compose::LayerQuad;
use crate::core::mask::{Mask, MaskShape};
use crate::panels::monitor::ResolvedLayer;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::{v2, Rect};
use crate::ui::Ui;
use raylib::consts::MouseCursor;
use raylib::math::Vector2;

const HANDLE: f32 = 8.0;
const HANDLE_HIT: f32 = 8.0;
/// Pixel-Schwelle, ab der eine Geste „echt“ wird (ein Undo-Snapshot).
const DRAG_THRESHOLD: f32 = 2.0;
/// Abstand des Rotationsgriffs über der Oberkante (in UV).
const ROT_OFFSET_UV: f32 = 0.06;

#[derive(Clone, Copy, PartialEq)]
enum Handle {
    /// Ganze Maske verschieben.
    Move,
    /// Radius-Handle: 0=rechts, 1=unten, 2=links, 3=oben (Masken-lokal).
    Radius(usize),
    Rotate,
    Feather,
    /// Polygon-Stützpunkt.
    Vertex(usize),
}

struct Drag {
    handle: Handle,
    start_mouse: Vector2,
    /// Maus-UV bei Gestenbeginn.
    start_uv: (f32, f32),
    /// Maske bei Gestenbeginn (Bezug für relative Änderungen).
    start_mask: Mask,
    history_pushed: bool,
}

#[derive(Default)]
pub struct MaskGizmo {
    drag: Option<Drag>,
}

// ----------------------------------------------------------- UV ↔ Bildschirm

/// UV-Punkt (0..1) über das Layer-Quad nach Bildschirm.
fn uv_to_screen(q: &LayerQuad, u: f32, v: f32) -> Vector2 {
    let lx = (u as f64 - 0.5) * q.w;
    let ly = (v as f64 - 0.5) * q.h;
    let (s, c) = q.rot_deg.to_radians().sin_cos();
    v2(
        (q.cx + lx * c - ly * s) as f32,
        (q.cy + lx * s + ly * c) as f32,
    )
}

/// Bildschirmpunkt zurück in UV (inverse Layer-Transform).
fn screen_to_uv(q: &LayerQuad, p: Vector2) -> (f32, f32) {
    let (s, c) = (-q.rot_deg.to_radians()).sin_cos();
    let dx = p.x as f64 - q.cx;
    let dy = p.y as f64 - q.cy;
    let lx = dx * c - dy * s;
    let ly = dx * s + dy * c;
    ((lx / q.w.max(1e-6) + 0.5) as f32, (ly / q.h.max(1e-6) + 0.5) as f32)
}

/// Masken-lokale Koordinaten (vor Maskenrotation) nach UV.
fn mask_local_to_uv(m: &Mask, lx: f32, ly: f32) -> (f32, f32) {
    let a = m.rotation.to_radians();
    let (s, c) = (a.sin(), a.cos());
    (m.center[0] + lx * c - ly * s, m.center[1] + lx * s + ly * c)
}

/// UV nach masken-lokal (entrotiert ums Maskenzentrum).
fn uv_to_mask_local(m: &Mask, u: f32, v: f32) -> (f32, f32) {
    let dx = u - m.center[0];
    let dy = v - m.center[1];
    let a = -m.rotation.to_radians();
    let (s, c) = (a.sin(), a.cos());
    (dx * c - dy * s, dx * s + dy * c)
}

/// Maximaler Abstand der Polygon-Stützpunkte vom Schwerpunkt (für die
/// Feather-Handle-Platzierung beim Polygon).
fn poly_bound_radius(m: &Mask) -> f32 {
    let c = m.centroid();
    m.points
        .iter()
        .map(|p| ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt())
        .fold(0.0f32, f32::max)
}

/// Handle-Positionen (UV) der aktuellen Maske: Radien/Rotation/Feather für
/// Ellipse+Rechteck, Stützpunkte + Feather fürs Polygon. (Handle, UV).
fn handle_positions(m: &Mask) -> Vec<(Handle, (f32, f32))> {
    let mut out = Vec::new();
    match m.shape {
        MaskShape::Polygon => {
            for (i, _) in m.points.iter().enumerate() {
                out.push((Handle::Vertex(i), (m.points[i][0], m.points[i][1])));
            }
            // Feather-Handle rechts vom Schwerpunkt (außerhalb des Polygons).
            let c = m.centroid();
            let r = poly_bound_radius(m);
            out.push((Handle::Feather, (c[0] + r + m.feather.max(0.03), c[1])));
        }
        _ => {
            let (rx, ry) = (m.radius[0], m.radius[1]);
            out.push((Handle::Radius(0), mask_local_to_uv(m, rx, 0.0)));
            out.push((Handle::Radius(1), mask_local_to_uv(m, 0.0, ry)));
            out.push((Handle::Radius(2), mask_local_to_uv(m, -rx, 0.0)));
            out.push((Handle::Radius(3), mask_local_to_uv(m, 0.0, -ry)));
            out.push((Handle::Rotate, mask_local_to_uv(m, 0.0, -(ry + ROT_OFFSET_UV))));
            out.push((
                Handle::Feather,
                mask_local_to_uv(m, rx + m.feather.max(0.03), 0.0),
            ));
        }
    }
    out
}

impl MaskGizmo {
    /// Liefert (clip_id, fx_id, mask_id, geklonte Maske, Layer-Quad) der aktiven
    /// Maske, falls ihr Clip am Playhead sichtbar ist.
    fn active<'a>(
        app: &AppState,
        layers: &'a [ResolvedLayer],
    ) -> Option<(String, String, String, Mask, &'a LayerQuad)> {
        let sel = app.app.active_mask.as_ref()?;
        let clip = app.timeline.clip(&sel.clip_id)?;
        let fx = clip.effects.iter().find(|e| e.id == sel.fx_id)?;
        let mask = fx.masks.iter().find(|m| m.id == sel.mask_id)?.clone();
        let layer = layers.iter().find(|l| l.clip_id == sel.clip_id)?;
        Some((
            sel.clip_id.clone(),
            sel.fx_id.clone(),
            sel.mask_id.clone(),
            mask,
            &layer.quad,
        ))
    }

    /// Bearbeitet eine sichtbare Maske: zeichnet das Gizmo und verarbeitet die
    /// Interaktion. Gibt `true` zurück, wenn eine Maske aktiv bearbeitet wird
    /// (der Monitor unterdrückt dann das Transform-Gizmo). `false`, wenn keine
    /// Maske aktiv oder die aktive Maske im aktuellen Kontext nicht auflösbar
    /// ist (anderer Sequenz-Tab, Clip nicht am Playhead) — dann übernimmt das
    /// Transform-Gizmo.
    pub fn update(&mut self, ui: &mut Ui, app: &mut AppState, stage: Rect, layers: &[ResolvedLayer]) -> bool {
        let Some((clip_id, fx_id, mask_id, mask, quad)) = Self::active(app, layers) else {
            // Maske (noch) nicht sichtbar/aufgelöst — Geste abbrechen, Transform-
            // Gizmo übernehmen lassen.
            self.drag = None;
            return false;
        };
        let quad = *quad;

        // Laufende Geste fortführen/beenden.
        if self.drag.is_some() {
            if !ui.input.left_down {
                self.drag = None;
            } else {
                self.continue_drag(ui, app, &quad, &clip_id, &fx_id, &mask_id);
            }
        }

        // Interaktion (nur ohne laufende Geste).
        let mouse_in_stage = ui.mouse_in(stage);
        if self.drag.is_none() && mouse_in_stage && ui.nothing_active() {
            let mouse = ui.input.mouse;
            let mut hover: Option<Handle> = None;
            // Handles zuerst (liegen über der Fläche).
            for (h, uv) in handle_positions(&mask) {
                let p = uv_to_screen(&quad, uv.0, uv.1);
                if (mouse.x - p.x).abs() <= HANDLE_HIT && (mouse.y - p.y).abs() <= HANDLE_HIT {
                    hover = Some(h);
                    break;
                }
            }
            // Sonst: Klick in die Fläche (oder knapp daneben) verschiebt die
            // Maske — geometrisch, unabhängig von Invertierung.
            if hover.is_none() {
                let (u, v) = screen_to_uv(&quad, mouse);
                if mask.signed_distance(u, v) < 0.04 {
                    hover = Some(Handle::Move);
                }
            }

            if let Some(h) = hover {
                ui.want_cursor(match h {
                    Handle::Rotate => MouseCursor::MOUSE_CURSOR_CROSSHAIR,
                    Handle::Move => MouseCursor::MOUSE_CURSOR_RESIZE_ALL,
                    _ => MouseCursor::MOUSE_CURSOR_POINTING_HAND,
                });
                if ui.input.left_pressed {
                    let (u, v) = screen_to_uv(&quad, mouse);
                    self.drag = Some(Drag {
                        handle: h,
                        start_mouse: mouse,
                        start_uv: (u, v),
                        start_mask: mask.clone(),
                        history_pushed: false,
                    });
                }
            }
        }

        // Zeichnen.
        ui.push_clip(stage);
        draw_mask(ui, &quad, &mask, self.drag.is_some());
        ui.pop_clip();
        true
    }

    fn continue_drag(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        quad: &LayerQuad,
        clip_id: &str,
        fx_id: &str,
        mask_id: &str,
    ) {
        let Some(drag) = self.drag.as_mut() else { return };
        let mouse = ui.input.mouse;
        let dx = mouse.x - drag.start_mouse.x;
        let dy = mouse.y - drag.start_mouse.y;
        if !drag.history_pushed {
            if (dx * dx + dy * dy).sqrt() < DRAG_THRESHOLD {
                return;
            }
            app.timeline.begin_fx_edit();
            drag.history_pushed = true;
        }

        let (cu, cv) = screen_to_uv(quad, mouse);
        let (du, dv) = (cu - drag.start_uv.0, cv - drag.start_uv.1);
        let start = drag.start_mask.clone();
        let handle = drag.handle;
        let shift = ui.input.shift;

        app.timeline
            .mask_update_live(clip_id, fx_id, mask_id, |m| match handle {
                Handle::Move => {
                    m.center = [start.center[0] + du, start.center[1] + dv];
                    if m.shape == MaskShape::Polygon {
                        for (i, p) in m.points.iter_mut().enumerate() {
                            if let Some(sp) = start.points.get(i) {
                                *p = [sp[0] + du, sp[1] + dv];
                            }
                        }
                    }
                }
                Handle::Radius(idx) => {
                    let (lx, ly) = uv_to_mask_local(&start, cu, cv);
                    if idx == 0 || idx == 2 {
                        m.radius[0] = lx.abs().max(0.01);
                    } else {
                        m.radius[1] = ly.abs().max(0.01);
                    }
                }
                Handle::Rotate => {
                    let a = (cv - start.center[1]).atan2(cu - start.center[0]);
                    let mut rot = a.to_degrees() + 90.0;
                    if shift {
                        rot = (rot / 15.0).round() * 15.0;
                    }
                    m.rotation = rot;
                }
                Handle::Feather => {
                    if start.shape == MaskShape::Polygon {
                        // Abstand vom Schwerpunkt minus Bounding-Radius.
                        let c = start.centroid();
                        let d = ((cu - c[0]).powi(2) + (cv - c[1]).powi(2)).sqrt();
                        m.feather = (d - poly_bound_radius(&start)).clamp(0.0, 1.0);
                    } else {
                        let (lx, _) = uv_to_mask_local(&start, cu, cv);
                        m.feather = (lx.abs() - start.radius[0]).clamp(0.0, 1.0);
                    }
                }
                Handle::Vertex(i) => {
                    if let Some(p) = m.points.get_mut(i) {
                        if let Some(sp) = start.points.get(i) {
                            *p = [sp[0] + du, sp[1] + dv];
                        }
                    }
                }
            });

        match handle {
            Handle::Rotate => ui.want_cursor(MouseCursor::MOUSE_CURSOR_CROSSHAIR),
            Handle::Move => ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_ALL),
            _ => ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND),
        }
    }
}

/// Umriss + Handles der Maske zeichnen. Der Umriss wird in UV abgetastet und
/// pro Punkt über das Quad projiziert (folgt jeder Clip-Transform/Rotation).
fn draw_mask(ui: &mut Ui, quad: &LayerQuad, m: &Mask, dragging: bool) {
    let color = if dragging {
        theme::ACCENT_HOVER
    } else {
        theme::ACCENT
    };
    let feather_color = theme::with_alpha(color, 110);

    // Boundary-Punkte (UV) der Hauptkante und – falls Feather – der äußeren
    // Feather-Grenze (Kante + halber Feather) sammeln.
    let main = boundary_uv(m, 0.0);
    draw_uv_loop(ui, quad, &main, 1.5, color);
    if m.feather > 1e-4 && m.shape != MaskShape::Polygon {
        let outer = boundary_uv(m, m.feather * 0.5);
        draw_uv_loop(ui, quad, &outer, 1.0, feather_color);
        let inner = boundary_uv(m, -m.feather * 0.5);
        draw_uv_loop(ui, quad, &inner, 1.0, feather_color);
    }

    // Rotationsgriff: Linie + Kreis.
    if m.shape != MaskShape::Polygon {
        let top = mask_local_to_uv(m, 0.0, -m.radius[1]);
        let rot = mask_local_to_uv(m, 0.0, -(m.radius[1] + ROT_OFFSET_UV));
        let tp = uv_to_screen(quad, top.0, top.1);
        let rp = uv_to_screen(quad, rot.0, rot.1);
        ui.line(tp, rp, 1.5, color);
        ui.circle(rp, HANDLE / 2.0 + 1.0, theme::SURFACE_0);
        ui.circle_outline(rp, HANDLE / 2.0 + 1.0, color);
    }

    // Handles.
    for (h, uv) in handle_positions(m) {
        if h == Handle::Rotate {
            continue; // schon gezeichnet
        }
        let p = uv_to_screen(quad, uv.0, uv.1);
        if h == Handle::Feather {
            // Feather-Handle als kleine Raute.
            let d = HANDLE / 2.0 + 1.0;
            ui.line(v2(p.x, p.y - d), v2(p.x + d, p.y), 1.5, feather_color);
            ui.line(v2(p.x + d, p.y), v2(p.x, p.y + d), 1.5, feather_color);
            ui.line(v2(p.x, p.y + d), v2(p.x - d, p.y), 1.5, feather_color);
            ui.line(v2(p.x - d, p.y), v2(p.x, p.y - d), 1.5, feather_color);
        } else {
            let r = Rect::new(p.x - HANDLE / 2.0, p.y - HANDLE / 2.0, HANDLE, HANDLE);
            ui.fill(r, theme::SURFACE_0);
            ui.stroke(r, 1.5, color);
        }
    }

    // Mittelpunkt-Markierung.
    let center = m.centroid();
    let cp = uv_to_screen(quad, center[0], center[1]);
    ui.circle(cp, 2.5, color);
}



/// Linienzug eines UV-Punkt-Loops (geschlossen) projiziert zeichnen.
fn draw_uv_loop(ui: &mut Ui, quad: &LayerQuad, uv: &[(f32, f32)], th: f32, color: raylib::color::Color) {
    if uv.len() < 2 {
        return;
    }
    let pts: Vec<Vector2> = uv.iter().map(|p| uv_to_screen(quad, p.0, p.1)).collect();
    for i in 0..pts.len() {
        ui.line(pts[i], pts[(i + 1) % pts.len()], th, color);
    }
}

/// Umriss-Punkte (UV) einer Maske; `grow` weitet die Form (für Feather-Ringe).
fn boundary_uv(m: &Mask, grow: f32) -> Vec<(f32, f32)> {
    match m.shape {
        MaskShape::Ellipse => {
            let (rx, ry) = (m.radius[0] + grow, m.radius[1] + grow);
            (0..64)
                .map(|i| {
                    let t = i as f32 / 64.0 * std::f32::consts::TAU;
                    mask_local_to_uv(m, rx * t.cos(), ry * t.sin())
                })
                .collect()
        }
        MaskShape::Rectangle => {
            let (rx, ry) = (m.radius[0] + grow, m.radius[1] + grow);
            [(-rx, -ry), (rx, -ry), (rx, ry), (-rx, ry)]
                .into_iter()
                .map(|(lx, ly)| mask_local_to_uv(m, lx, ly))
                .collect()
        }
        MaskShape::Polygon => m.points.iter().map(|p| (p[0], p[1])).collect(),
    }
}
