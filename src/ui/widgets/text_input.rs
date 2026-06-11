//! Einzeiliges Texteingabefeld: Fokus, Cursor (Blink), Auswahl, Clipboard.
//! Optik: h-6, rounded, border-line, bg-surface-2, px-2, text-xs,
//! placeholder text-text-3, focus:border-accent.

use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::{FontKind, Ui};
use raylib::consts::{KeyboardKey, MouseCursor};
use raylib::ffi;
use std::ffi::CString;

#[derive(Default)]
pub struct TextInputState {
    pub text: String,
    pub cursor: usize,    // Byte-Index (immer an char-Grenze)
    pub sel_start: usize, // == cursor: keine Auswahl
    /// Zusätzliches linkes Innenpolster (z. B. für ein Lupen-Icon, pl-6).
    pub pad_left: f32,
}

pub struct TextInputResult {
    pub changed: bool,
    pub submitted: bool,
    pub focused: bool,
}

impl TextInputState {
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.sel_start = self.cursor;
    }

    fn selection(&self) -> (usize, usize) {
        (
            self.cursor.min(self.sel_start),
            self.cursor.max(self.sel_start),
        )
    }

    fn delete_selection(&mut self) {
        let (a, b) = self.selection();
        if a != b {
            self.text.replace_range(a..b, "");
            self.cursor = a;
            self.sel_start = a;
        }
    }

    fn insert(&mut self, s: &str) {
        self.delete_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.sel_start = self.cursor;
    }

    fn prev_boundary(&self, from: usize) -> usize {
        if from == 0 {
            return 0;
        }
        let mut i = from - 1;
        while i > 0 && !self.text.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    fn next_boundary(&self, from: usize) -> usize {
        if from >= self.text.len() {
            return self.text.len();
        }
        let mut i = from + 1;
        while i < self.text.len() && !self.text.is_char_boundary(i) {
            i += 1;
        }
        i
    }

    fn prev_word(&self, from: usize) -> usize {
        let mut i = self.prev_boundary(from);
        while i > 0 && self.text[..i].ends_with(char::is_whitespace) {
            i = self.prev_boundary(i);
        }
        while i > 0 && !self.text[..i].ends_with(char::is_whitespace) {
            i = self.prev_boundary(i);
        }
        i
    }

    fn next_word(&self, from: usize) -> usize {
        let mut i = self.next_boundary(from);
        while i < self.text.len() && !self.text[i..].starts_with(char::is_whitespace) {
            i = self.next_boundary(i);
        }
        while i < self.text.len() && self.text[i..].starts_with(char::is_whitespace) {
            i = self.next_boundary(i);
        }
        i
    }

    /// Zeichnet das Feld und verarbeitet Eingaben, wenn fokussiert.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        id_src: impl std::hash::Hash,
        rect: Rect,
        placeholder: &str,
    ) -> TextInputResult {
        let id = ui.id(id_src);
        let it = ui.interact(id, rect);
        if it.clicked || (it.hovered && ui.input.left_pressed) {
            ui.persist.keyboard_focus = id;
        } else if ui.input.left_pressed && !it.hovered && ui.persist.keyboard_focus == id {
            ui.persist.keyboard_focus = 0;
        }
        let focused = ui.persist.keyboard_focus == id;

        let mut changed = false;
        let mut submitted = false;

        if focused {
            // Texteingabe
            let chars: Vec<char> = ui.input.chars.clone();
            for c in chars {
                if !c.is_control() {
                    self.insert(&c.to_string());
                    changed = true;
                }
            }
            // Steuer-Tasten
            let keys = ui.input.keys.clone();
            for k in keys {
                use KeyboardKey::*;
                let sel = k.shift;
                match k.key {
                    KEY_BACKSPACE => {
                        let (a, b) = self.selection();
                        if a != b {
                            self.delete_selection();
                        } else if self.cursor > 0 {
                            let p = if k.ctrl {
                                self.prev_word(self.cursor)
                            } else {
                                self.prev_boundary(self.cursor)
                            };
                            self.text.replace_range(p..self.cursor, "");
                            self.cursor = p;
                            self.sel_start = p;
                        }
                        changed = true;
                    }
                    KEY_DELETE => {
                        let (a, b) = self.selection();
                        if a != b {
                            self.delete_selection();
                        } else if self.cursor < self.text.len() {
                            let p = if k.ctrl {
                                self.next_word(self.cursor)
                            } else {
                                self.next_boundary(self.cursor)
                            };
                            self.text.replace_range(self.cursor..p, "");
                        }
                        changed = true;
                    }
                    KEY_LEFT => {
                        self.cursor = if k.ctrl {
                            self.prev_word(self.cursor)
                        } else if self.cursor != self.sel_start && !sel {
                            self.selection().0
                        } else {
                            self.prev_boundary(self.cursor)
                        };
                        if !sel {
                            self.sel_start = self.cursor;
                        }
                    }
                    KEY_RIGHT => {
                        self.cursor = if k.ctrl {
                            self.next_word(self.cursor)
                        } else if self.cursor != self.sel_start && !sel {
                            self.selection().1
                        } else {
                            self.next_boundary(self.cursor)
                        };
                        if !sel {
                            self.sel_start = self.cursor;
                        }
                    }
                    KEY_HOME => {
                        self.cursor = 0;
                        if !sel {
                            self.sel_start = 0;
                        }
                    }
                    KEY_END => {
                        self.cursor = self.text.len();
                        if !sel {
                            self.sel_start = self.cursor;
                        }
                    }
                    KEY_ENTER | KEY_KP_ENTER => submitted = true,
                    KEY_ESCAPE => ui.persist.keyboard_focus = 0,
                    KEY_A if k.ctrl => {
                        self.sel_start = 0;
                        self.cursor = self.text.len();
                    }
                    KEY_C if k.ctrl => {
                        let (a, b) = self.selection();
                        if a != b {
                            set_clipboard(&self.text[a..b]);
                        }
                    }
                    KEY_X if k.ctrl => {
                        let (a, b) = self.selection();
                        if a != b {
                            set_clipboard(&self.text[a..b]);
                            self.delete_selection();
                            changed = true;
                        }
                    }
                    KEY_V if k.ctrl => {
                        if let Some(paste) = get_clipboard() {
                            let clean: String =
                                paste.chars().filter(|c| !c.is_control()).collect();
                            if !clean.is_empty() {
                                self.insert(&clean);
                                changed = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // ---- Rendering ----
        ui.fill_rounded(rect, theme::RADIUS_SM, theme::SURFACE_2);
        let border = if focused { theme::ACCENT } else { theme::LINE };
        ui.stroke_rounded(rect, theme::RADIUS_SM, 1.0, border);

        let mut inner = rect.inset_xy(8.0, 0.0);
        inner.cut_left(self.pad_left);
        ui.push_clip(inner);
        let font = FontKind::Sans12;
        if self.text.is_empty() {
            ui.text_left(placeholder, inner, theme::TEXT_3, font);
        } else {
            // Auswahl-Hintergrund
            let (a, b) = self.selection();
            if focused && a != b {
                let pre = ui.font(font).width(&self.text[..a]);
                let sel_w = ui.font(font).width(&self.text[a..b]);
                ui.fill(
                    Rect::new(inner.x + pre, rect.y + 4.0, sel_w, rect.h - 8.0),
                    theme::ACCENT_SOFT,
                );
            }
            ui.text_left(&self.text, inner, theme::TEXT_1, font);
        }
        if focused && (ui.time * 2.0) as i64 % 2 == 0 {
            let cursor_x = inner.x + ui.font(font).width(&self.text[..self.cursor]);
            ui.fill(
                Rect::new(cursor_x, rect.y + 5.0, 1.0, rect.h - 10.0),
                theme::TEXT_1,
            );
        }
        ui.pop_clip();

        if it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_IBEAM);
        }

        TextInputResult {
            changed,
            submitted,
            focused,
        }
    }
}

fn set_clipboard(text: &str) {
    if let Ok(c) = CString::new(text) {
        unsafe { ffi::SetClipboardText(c.as_ptr()) };
    }
}

fn get_clipboard() -> Option<String> {
    unsafe {
        let ptr = ffi::GetClipboardText();
        if ptr.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned())
    }
}
