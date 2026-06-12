//! Mehrzeiliges Texteingabefeld (explizite Umbrüche, Caret-Navigation,
//! Klick-Positionierung) — für Titeltexte (Grafik-Panel) und Untertitel-
//! Segmente (Untertitel-Panel).

use crate::theme;
use crate::ui::geom::{v2, Rect};
use crate::ui::{FontKind, Ui};
use raylib::consts::{KeyboardKey, MouseCursor};

pub const TEXTAREA_LINE_H: f32 = 16.0;

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

/// Ergebnis eines TextArea-Frames.
#[derive(Default)]
pub struct TextAreaResult {
    pub changed: bool,
    /// Enter OHNE Umbruch-Modus (`enter_inserts_newline = false`) bzw.
    /// Strg/Cmd+Enter — „Eingabe bestätigt“.
    pub submitted: bool,
    pub focused: bool,
}

#[derive(Default)]
pub struct TextArea {
    pub caret: usize,
    /// true (Standard): Enter fügt einen Zeilenumbruch ein; Bestätigen über
    /// Strg/Cmd+Enter. false: Enter bestätigt, Umbruch über Umschalt+Enter.
    pub enter_inserts_newline: bool,
}

impl TextArea {
    pub fn multiline() -> TextArea {
        TextArea {
            caret: 0,
            enter_inserts_newline: true,
        }
    }

    /// Zeilengrenzen (Byte-Bereiche ohne '\n').
    fn lines(text: &str) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut start = 0usize;
        for line in text.split('\n') {
            out.push((start, start + line.len()));
            start += line.len() + 1;
        }
        out
    }

    /// Fokus erzwingen (z. B. nach Doppelklick auf eine Listenzeile).
    pub fn focus(&mut self, ui: &mut Ui, id_src: impl std::hash::Hash, caret: usize) {
        let id = ui.id(id_src);
        ui.persist.keyboard_focus = id;
        self.caret = caret;
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        id_src: impl std::hash::Hash,
        rect: Rect,
        text: &mut String,
    ) -> TextAreaResult {
        let id = ui.id(id_src);
        let it = ui.interact(id, rect);
        let inner = rect.inset_xy(8.0, 6.0);
        let mut result = TextAreaResult::default();

        if it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_IBEAM);
        }
        if it.clicked || (it.hovered && ui.input.left_pressed) {
            ui.persist.keyboard_focus = id;
            // Caret aus der Klickposition (Zeile über y, Spalte über Breiten).
            let lines = Self::lines(text);
            let row = (((ui.input.mouse.y - inner.y) / TEXTAREA_LINE_H).floor() as i64)
                .clamp(0, lines.len() as i64 - 1) as usize;
            let (start, end) = lines[row];
            let font = ui.font(FontKind::Sans12);
            let mut best = start;
            let mut best_d = f32::INFINITY;
            let mut i = start;
            loop {
                let w = font.width(&text[start..i]);
                let d = (ui.input.mouse.x - (inner.x + w)).abs();
                if d < best_d {
                    best_d = d;
                    best = i;
                }
                if i >= end {
                    break;
                }
                i = next_boundary(text, i);
            }
            self.caret = best;
        } else if ui.input.left_pressed && !it.hovered && ui.persist.keyboard_focus == id {
            ui.persist.keyboard_focus = 0;
        }
        let focused = ui.persist.keyboard_focus == id;
        result.focused = focused;

        if focused {
            let mut caret = self.caret.min(text.len());
            while caret > 0 && !text.is_char_boundary(caret) {
                caret -= 1;
            }
            let chars: Vec<char> = ui.input.chars.clone();
            for c in chars {
                if c.is_control() {
                    continue;
                }
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                text.insert_str(caret, s);
                caret += s.len();
                result.changed = true;
            }
            let keys = ui.input.keys.clone();
            for k in keys {
                match k.key {
                    KeyboardKey::KEY_ENTER | KeyboardKey::KEY_KP_ENTER => {
                        let newline = if self.enter_inserts_newline {
                            // Strg/Cmd+Enter bestätigt, Enter bricht um.
                            !(k.ctrl || k.meta)
                        } else {
                            // Enter bestätigt, Umschalt+Enter bricht um.
                            k.shift
                        };
                        if newline {
                            text.insert(caret, '\n');
                            caret += 1;
                            result.changed = true;
                        } else {
                            result.submitted = true;
                        }
                    }
                    KeyboardKey::KEY_BACKSPACE => {
                        if caret > 0 {
                            let p = prev_boundary(text, caret);
                            text.replace_range(p..caret, "");
                            caret = p;
                            result.changed = true;
                        }
                    }
                    KeyboardKey::KEY_DELETE => {
                        if caret < text.len() {
                            let n = next_boundary(text, caret);
                            text.replace_range(caret..n, "");
                            result.changed = true;
                        }
                    }
                    KeyboardKey::KEY_LEFT => caret = prev_boundary(text, caret),
                    KeyboardKey::KEY_RIGHT => caret = next_boundary(text, caret),
                    KeyboardKey::KEY_UP | KeyboardKey::KEY_DOWN => {
                        let lines = Self::lines(text);
                        if let Some(row) = lines
                            .iter()
                            .position(|(s, e)| caret >= *s && caret <= *e)
                        {
                            let col = caret - lines[row].0;
                            let target = if k.key == KeyboardKey::KEY_UP {
                                row.checked_sub(1)
                            } else {
                                (row + 1 < lines.len()).then_some(row + 1)
                            };
                            if let Some(t) = target {
                                let (s, e) = lines[t];
                                let mut c = s + col.min(e - s);
                                while c > s && !text.is_char_boundary(c) {
                                    c -= 1;
                                }
                                caret = c;
                            }
                        }
                    }
                    KeyboardKey::KEY_HOME | KeyboardKey::KEY_END => {
                        let lines = Self::lines(text);
                        if let Some(&(s, e)) = lines
                            .iter()
                            .find(|(s, e)| caret >= *s && caret <= *e)
                        {
                            caret = if k.key == KeyboardKey::KEY_HOME { s } else { e };
                        }
                    }
                    KeyboardKey::KEY_ESCAPE => ui.persist.keyboard_focus = 0,
                    _ => {}
                }
            }
            self.caret = caret;
        }

        // ---- Rendering ----
        ui.fill_rounded(rect, theme::RADIUS_SM, theme::SURFACE_2);
        let border = if focused { theme::ACCENT } else { theme::LINE };
        ui.stroke_rounded(rect, theme::RADIUS_SM, 1.0, border);
        ui.push_clip(inner);
        let mut y = inner.y;
        for (s, e) in Self::lines(text) {
            ui.text(&text[s..e], v2(inner.x, y), theme::TEXT_1, FontKind::Sans12);
            if focused && (ui.time * 2.0) as i64 % 2 == 0 && self.caret >= s && self.caret <= e {
                let cx = inner.x + ui.font(FontKind::Sans12).width(&text[s..self.caret]);
                ui.fill(Rect::new(cx, y + 1.0, 1.0, TEXTAREA_LINE_H - 2.0), theme::TEXT_1);
            }
            y += TEXTAREA_LINE_H;
        }
        ui.pop_clip();
        result
    }
}
