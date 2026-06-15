//! Befehlspalette: oben zentriertes Overlay mit Suchfeld und gefilterter
//! Command-Liste; deaktivierte Commands (when nicht erfüllt) ausgegraut am
//! Ende.

use crate::core::commands::{evaluate_when, Command, CommandRegistry};
use crate::core::keyboard::format_keys;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::drop_shadow;
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::{FontKind, Ui};
use raylib::consts::{KeyboardKey, MouseCursor};

/// Substring- (gewichtet) und Fuzzy-Match über Titel, Kategorie und ID.
fn match_score(query: &str, cmd: &Command) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let title = cmd.title.to_lowercase();
    let haystack = format!("{} {} {}", cmd.category, cmd.title, cmd.id).to_lowercase();
    if let Some(idx) = title.find(query) {
        return Some(1000 - idx as i32);
    }
    if let Some(idx) = haystack.find(query) {
        return Some(500 - idx as i32);
    }
    // Fuzzy: alle Zeichen der Query in Reihenfolge enthalten
    let mut chars = query.chars().peekable();
    for ch in haystack.chars() {
        if chars.peek() == Some(&ch) {
            chars.next();
        }
        if chars.peek().is_none() {
            return Some(1);
        }
    }
    None
}

/// Kompakter Shortcut-Chip im kbd-Stil.
pub fn shortcut_chip(ui: &mut Ui, x: f32, center_y: f32, keys: &str, conflict: bool) -> f32 {
    let label = format_keys(keys);
    let w = ui.font(FontKind::Mono12).width(&label) + 12.0;
    let rect = Rect::new(x - w, center_y - 9.0, w, 18.0);
    let (border, bg, fg) = if conflict {
        (
            theme::with_alpha(theme::DANGER, 153),
            theme::with_alpha(theme::DANGER, 26),
            theme::DANGER,
        )
    } else {
        (theme::LINE_STRONG, theme::SURFACE_3, theme::TEXT_2)
    };
    ui.fill_rounded(rect, theme::RADIUS_SM, bg);
    ui.stroke_rounded(rect, theme::RADIUS_SM, 1.0, border);
    ui.text_centered(&label, rect, fg, FontKind::Mono12);
    w
}

#[derive(Default)]
pub struct CommandPalette {
    query: TextInputState,
    selected: usize,
    scroll: ScrollState,
    was_open: bool,
}


impl CommandPalette {
    pub fn render(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        registry: &CommandRegistry,
    ) {
        if !state.app.command_palette_open {
            self.was_open = false;
            return;
        }
        let input_id = ui.id("palette.input");
        if !self.was_open {
            self.was_open = true;
            self.query.set_text("");
            self.selected = 0;
            self.scroll.offset_y = 0.0;
            ui.persist.keyboard_focus = input_id;
        }

        // Escape schließt immer
        if ui.input.keys.iter().any(|k| k.key == KeyboardKey::KEY_ESCAPE) {
            state.app.command_palette_open = false;
            return;
        }

        // Abdunkelung + Box (max-w-xl, pt-24)
        ui.fill(ui.screen, theme::with_alpha(theme::BLACK, 102));
        let w = 576.0_f32.min(ui.screen.w - 32.0);
        let list_max = 384.0_f32;
        let x = ui.screen.x + (ui.screen.w - w) / 2.0;
        let y = ui.screen.y + 96.0;

        // Einträge filtern + sortieren
        let q = self.query.text.trim().to_lowercase();
        let mut entries: Vec<(usize, bool, i32)> = Vec::new();
        for (i, cmd) in registry.all().iter().enumerate() {
            if let Some(score) = match_score(&q, cmd) {
                entries.push((i, evaluate_when(cmd.when, state), score));
            }
        }
        entries.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));
        if self.selected >= entries.len() {
            self.selected = entries.len().saturating_sub(1);
        }

        // Tastatur-Navigation
        for k in &ui.input.keys {
            match k.key {
                KeyboardKey::KEY_DOWN => {
                    self.selected = (self.selected + 1).min(entries.len().saturating_sub(1));
                }
                KeyboardKey::KEY_UP => self.selected = self.selected.saturating_sub(1),
                _ => {}
            }
        }

        let row_h = 32.0;
        let list_h = (entries.len() as f32 * row_h + 8.0).clamp(48.0, list_max);
        let box_rect = Rect::new(x, y, w, 44.0 + list_h);
        drop_shadow(ui, box_rect, theme::RADIUS_LG);
        ui.fill_rounded(box_rect, theme::RADIUS_LG, theme::SURFACE_2);
        ui.stroke_rounded(box_rect, theme::RADIUS_LG, 1.0, theme::LINE_STRONG);

        // Suchfeld-Zeile (h-10 + border-b)
        let search_row = Rect::new(box_rect.x, box_rect.y, box_rect.w, 43.0);
        ui.hline(search_row.x, search_row.bottom(), search_row.w, theme::LINE);
        let mut sr = search_row.inset_xy(12.0, 0.0);
        let icon_cell = sr.cut_left(16.0);
        ui.icon("search", icon_cell, 16.0, theme::TEXT_3);
        sr.cut_left(8.0);
        let field = Rect::new(sr.x, search_row.y + 8.0, sr.w, 28.0);
        // Transparentes Eingabefeld: eigener Stil — Text direkt zeichnen
        let result = self.query.show(ui, "palette.input", field, "Befehl suchen …");
        if result.changed {
            self.selected = 0;
        }
        ui.persist.keyboard_focus = input_id;

        // Liste
        let list_rect = Rect::new(box_rect.x, search_row.bottom() + 1.0, box_rect.w, list_h);
        let content_h = entries.len() as f32 * row_h + 8.0;
        let view = self.scroll.begin(ui, list_rect, 0.0, content_h);

        // Auswahl sichtbar halten
        let sel_top = 4.0 + self.selected as f32 * row_h;
        if sel_top < self.scroll.offset_y {
            self.scroll.offset_y = sel_top;
        } else if sel_top + row_h > self.scroll.offset_y + list_rect.h {
            self.scroll.offset_y = sel_top + row_h - list_rect.h;
        }

        let mut run: Option<usize> = None;
        if entries.is_empty() {
            ui.text_centered(
                "Keine Befehle gefunden",
                list_rect,
                theme::TEXT_3,
                FontKind::Sans14,
            );
        }
        for (vis_idx, (cmd_idx, enabled, _)) in entries.iter().enumerate() {
            let row = Rect::new(
                view.viewport.x + 4.0,
                view.origin_y + 4.0 + vis_idx as f32 * row_h,
                view.viewport.w - 8.0,
                row_h,
            );
            if row.bottom() < view.viewport.y || row.y > view.viewport.bottom() {
                continue;
            }
            let cmd = &registry.all()[*cmd_idx];
            let hovered = ui.mouse_in(row);
            if hovered && *enabled {
                self.selected = vis_idx;
            }
            let selected = vis_idx == self.selected;
            if selected {
                ui.fill_rounded(
                    row,
                    theme::RADIUS_SM,
                    if *enabled { theme::ACCENT_SOFT } else { theme::SURFACE_3 },
                );
            }
            let fg = if *enabled { theme::TEXT_1 } else { theme::TEXT_3 };
            let mut inner = row.inset_xy(8.0, 0.0);
            let cat_cell = inner.cut_left(96.0);
            let cat = ui.font(FontKind::Sans12).ellipsize(cmd.category, cat_cell.w);
            ui.text_left(&cat, cat_cell, theme::TEXT_3, FontKind::Sans12);
            inner.cut_left(8.0);

            // Shortcut-Chips rechtsbündig
            let mut chip_x = inner.right();
            for binding in state.keymap.bindings_for_command(&cmd.id) {
                let used = shortcut_chip(ui, chip_x, row.center().y, &binding.keys, false);
                chip_x -= used + 4.0;
            }
            inner.w = (chip_x - inner.x - 8.0).max(0.0);
            let title = ui.font(FontKind::Sans14).ellipsize(&cmd.title, inner.w);
            ui.text_left(&title, inner, fg, FontKind::Sans14);

            if hovered {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            }
            if hovered && ui.input.left_pressed {
                run = Some(vis_idx);
            }
        }
        self.scroll.end(ui, list_rect, 0.0, content_h);

        // Enter führt aus
        if ui.input.keys.iter().any(|k| {
            k.key == KeyboardKey::KEY_ENTER || k.key == KeyboardKey::KEY_KP_ENTER
        }) {
            run = Some(self.selected);
        }

        if let Some(vis_idx) = run {
            if let Some((cmd_idx, enabled, _)) = entries.get(vis_idx) {
                if *enabled {
                    let id = registry.all()[*cmd_idx].id.clone();
                    state.app.command_palette_open = false;
                    ui.persist.keyboard_focus = 0;
                    ui.run_command(id);
                } else {
                    let now = ui.time;
                    state
                        .app
                        .set_status_message(Some("Im aktuellen Kontext nicht verfügbar".into()), now);
                }
            }
        }

        // Klick außerhalb schließt
        if ui.input.left_pressed && !box_rect.contains(ui.input.mouse) {
            state.app.command_palette_open = false;
            ui.persist.keyboard_focus = 0;
        }
    }
}
