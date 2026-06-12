//! Textbearbeitung direkt im Programmmonitor (Premiere-Stil): Doppelklick
//! auf einen Titel-Layer öffnet die Eingabe an Ort und Stelle — Klick
//! positioniert den Caret, Pfeile/Home/End navigieren, Enter bricht die
//! Zeile um, Esc oder Klick außerhalb beendet. Alle Änderungen laufen als
//! EINE Undo-Geste über `begin_title_edit` + `title_update_live`; der
//! Monitor zeigt das Ergebnis live (Titel-Engine rastert bei jeder
//! Änderung neu).

use crate::core::compose::LayerQuad;
use crate::core::text_raster;
use crate::core::timeline::SelectMode;
use crate::panels::monitor::ResolvedLayer;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::{v2, Rect};
use crate::ui::Ui;
use raylib::consts::{KeyboardKey, MouseCursor};
use raylib::prelude::RaylibDraw;

struct EditState {
    clip_id: String,
    /// Caret als Byte-Index in `spec.text` (immer an einer char-Grenze).
    caret: usize,
    /// Undo-Snapshot der Tipp-Sitzung wurde bereits angelegt.
    dirty: bool,
}

#[derive(Default)]
pub struct TitleTextEditor {
    state: Option<EditState>,
}

/// Frame-Koordinaten (0..canvas) ↔ Bildschirm, durch das Layer-Quad
/// (inkl. Rotation/Skalierung/Erweiterung) abgebildet.
struct QuadMap {
    cx: f64,
    cy: f64,
    sx: f64,
    sy: f64,
    rot: f64,
    fw: f64,
    fh: f64,
}

impl QuadMap {
    fn new(quad: &LayerQuad, extend_k: f64, canvas: Rect) -> QuadMap {
        QuadMap {
            cx: quad.cx,
            cy: quad.cy,
            sx: quad.w / canvas.w.max(1.0) as f64,
            sy: (quad.h / extend_k.max(1.0)) / canvas.h.max(1.0) as f64,
            rot: quad.rot_deg,
            fw: canvas.w as f64,
            fh: canvas.h as f64,
        }
    }

    fn to_screen(&self, p: (f64, f64)) -> (f32, f32) {
        let lx = (p.0 - self.fw / 2.0) * self.sx;
        let ly = (p.1 - self.fh / 2.0) * self.sy;
        let (s, c) = self.rot.to_radians().sin_cos();
        (
            (self.cx + lx * c - ly * s) as f32,
            (self.cy + lx * s + ly * c) as f32,
        )
    }

    fn to_frame(&self, mx: f64, my: f64) -> (f64, f64) {
        let dx = mx - self.cx;
        let dy = my - self.cy;
        let (s, c) = (-self.rot.to_radians()).sin_cos();
        let lx = dx * c - dy * s;
        let ly = dx * s + dy * c;
        (
            lx / self.sx.max(1e-9) + self.fw / 2.0,
            ly / self.sy.max(1e-9) + self.fh / 2.0,
        )
    }
}

fn prev_boundary(text: &str, from: usize) -> usize {
    if from == 0 {
        return 0;
    }
    let mut i = from - 1;
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn next_boundary(text: &str, from: usize) -> usize {
    if from >= text.len() {
        return text.len();
    }
    let mut i = from + 1;
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

impl TitleTextEditor {
    pub fn stop(&mut self, ui: &mut Ui) {
        if self.state.take().is_some() {
            ui.persist.keyboard_focus = 0;
        }
    }

    /// Pro Frame nach dem Zeichnen der Bühne aufrufen. Liefert true,
    /// solange die Bearbeitung aktiv ist (das Transform-Gizmo pausiert).
    pub fn update(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        stage: Rect,
        canvas: Rect,
        layers: &[ResolvedLayer],
    ) -> bool {
        let (cw, ch) = (canvas.w.round().max(1.0) as u32, canvas.h.round().max(1.0) as u32);

        // ---- Bearbeitung per Doppelklick starten ----
        if self.state.is_none() {
            if !(ui.input.double_click && ui.mouse_in(stage)) {
                return false;
            }
            // Treffer gegen den TEXTBLOCK (nicht das Vollframe-Quad) —
            // Doppelklick neben dem Titel soll nichts öffnen.
            let hit = layers.iter().rev().filter(|l| l.is_title).find_map(|l| {
                let spec = app.timeline.clip(&l.clip_id).and_then(|c| c.title.clone())?;
                let lay = text_raster::layout_title(&spec, cw, ch)?;
                let map = QuadMap::new(&l.quad, l.extend_k, canvas);
                let (fx, fy) = map.to_frame(ui.input.mouse.x as f64, ui.input.mouse.y as f64);
                let (bx, by, bw, bh) = lay.box_rect.unwrap_or(lay.block);
                let m = 14.0;
                let inside =
                    fx >= bx - m && fx <= bx + bw + m && fy >= by - m && fy <= by + bh + m;
                inside.then(|| (l.clip_id.clone(), lay.caret_at(fx, fy).min(spec.text.len())))
            });
            let Some((clip_id, caret)) = hit else { return false };
            app.timeline
                .select_clips(&[clip_id.clone()], SelectMode::Replace, false);
            self.state = Some(EditState {
                clip_id,
                caret,
                dirty: false,
            });
        }

        // ---- Laufende Bearbeitung ----
        let st = self.state.as_mut().expect("Bearbeitung aktiv");
        let clip_id = st.clip_id.clone();
        let Some(layer) = layers.iter().find(|l| l.clip_id == clip_id) else {
            // Playhead hat den Clip verlassen / Clip gelöscht.
            self.stop(ui);
            return false;
        };
        let Some(mut text) = app
            .timeline
            .clip(&clip_id)
            .and_then(|c| c.title.as_ref())
            .map(|s| s.text.clone())
        else {
            self.stop(ui);
            return false;
        };

        // Globale Shortcuts pausieren, solange getippt wird.
        let focus_id = ui.id("monitor.titleEdit");
        ui.persist.keyboard_focus = focus_id;
        ui.set_hot(focus_id);

        let map = QuadMap::new(&layer.quad, layer.extend_k, canvas);
        let spec_now = app
            .timeline
            .clip(&clip_id)
            .and_then(|c| c.title.clone())
            .expect("Titel-Spec vorhanden");
        let layout = text_raster::layout_title(&spec_now, cw, ch);

        // ---- Maus: Caret setzen / Bearbeitung beenden ----
        if ui.input.left_pressed && !ui.input.double_click {
            let inside_stage = stage.contains(ui.input.mouse);
            if inside_stage {
                let (fx, fy) = map.to_frame(ui.input.mouse.x as f64, ui.input.mouse.y as f64);
                let near_block = layout.as_ref().is_some_and(|l| {
                    let (bx, by, bw, bh) = l.box_rect.unwrap_or(l.block);
                    let m = 24.0; // großzügiger Rand um den Block
                    fx >= bx - m && fx <= bx + bw + m && fy >= by - m && fy <= by + bh + m
                });
                if near_block {
                    if let Some(l) = &layout {
                        st.caret = l.caret_at(fx, fy).min(text.len());
                    }
                } else {
                    self.stop(ui);
                    return false;
                }
            } else {
                self.stop(ui);
                return false;
            }
        }
        if ui.mouse_in(stage) {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_IBEAM);
        }

        // ---- Tastatur ----
        let mut caret = st.caret.min(text.len());
        while caret > 0 && !text.is_char_boundary(caret) {
            caret -= 1;
        }
        let mut changed = false;
        let chars: Vec<char> = ui.input.chars.clone();
        for c in chars {
            if c.is_control() {
                continue;
            }
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            text.insert_str(caret, s);
            caret += s.len();
            changed = true;
        }
        let keys = ui.input.keys.clone();
        let mut stop_after = false;
        for k in keys {
            match k.key {
                KeyboardKey::KEY_ENTER | KeyboardKey::KEY_KP_ENTER => {
                    text.insert(caret, '\n');
                    caret += 1;
                    changed = true;
                }
                KeyboardKey::KEY_BACKSPACE => {
                    if caret > 0 {
                        let p = prev_boundary(&text, caret);
                        text.replace_range(p..caret, "");
                        caret = p;
                        changed = true;
                    }
                }
                KeyboardKey::KEY_DELETE => {
                    if caret < text.len() {
                        let n = next_boundary(&text, caret);
                        text.replace_range(caret..n, "");
                        changed = true;
                    }
                }
                KeyboardKey::KEY_LEFT => caret = prev_boundary(&text, caret),
                KeyboardKey::KEY_RIGHT => caret = next_boundary(&text, caret),
                KeyboardKey::KEY_UP | KeyboardKey::KEY_DOWN => {
                    if let Some(l) = &layout {
                        if let Some((x, y, h)) = l.caret_pos(caret.min(text.len())) {
                            let dir = if k.key == KeyboardKey::KEY_UP { -1.0 } else { 1.0 };
                            caret = l
                                .caret_at(x, y + h / 2.0 + dir * l.line_height)
                                .min(text.len());
                        }
                    }
                }
                KeyboardKey::KEY_HOME | KeyboardKey::KEY_END => {
                    if let Some(l) = &layout {
                        if let Some(line) = l
                            .lines
                            .iter()
                            .find(|ln| caret >= ln.byte_range.0 && caret <= ln.byte_range.1)
                        {
                            caret = if k.key == KeyboardKey::KEY_HOME {
                                line.byte_range.0
                            } else {
                                line.byte_range.1
                            };
                        }
                    }
                }
                KeyboardKey::KEY_ESCAPE => stop_after = true,
                _ => {}
            }
        }

        if changed {
            if !st.dirty {
                app.timeline.begin_title_edit();
                st.dirty = true;
            }
            let new_text = text.clone();
            app.timeline
                .title_update_live(&clip_id, move |s| s.text = new_text);
        }
        st.caret = caret;
        if stop_after {
            // Esc beendet die Bearbeitung (Änderungen sind bereits übernommen).
            self.stop(ui);
            return false;
        }

        // ---- Overlay: Blockrahmen + Caret (durch das Quad transformiert) ----
        ui.push_clip(stage);
        let spec_draw = app
            .timeline
            .clip(&clip_id)
            .and_then(|c| c.title.clone())
            .unwrap_or(spec_now);
        if let Some(l) = text_raster::layout_title(&spec_draw, cw, ch) {
            let (bx, by, bw, bh) = l.box_rect.unwrap_or(l.block);
            let pad = 8.0;
            // Das Quad ist bereits in Bühnenkoordinaten verankert
            // (canvas-Offset steckt in quad.cx/cy).
            let corners = [
                (bx - pad, by - pad),
                (bx + bw + pad, by - pad),
                (bx + bw + pad, by + bh + pad),
                (bx - pad, by + bh + pad),
            ]
            .map(|p| {
                let (x, y) = map.to_screen(p);
                v2(x, y)
            });
            for i in 0..4 {
                ui.d.draw_line_ex(
                    corners[i],
                    corners[(i + 1) % 4],
                    1.5,
                    theme::with_alpha(theme::GRAPHIC, 230),
                );
            }
            // Caret (blinkend)
            if (ui.time * 2.5) as i64 % 2 == 0 {
                if let Some((x, y, h)) = l.caret_pos(st.caret.min(spec_draw.text.len()))
                {
                    let a = map.to_screen((x, y));
                    let b = map.to_screen((x, y + h));
                    ui.d.draw_line_ex(
                        v2(a.0, a.1),
                        v2(b.0, b.1),
                        2.0,
                        theme::TEXT_1,
                    );
                }
            }
        }
        ui.pop_clip();
        true
    }
}
