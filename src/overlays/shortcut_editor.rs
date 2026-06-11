//! Shortcut-Editor: durchsuchbare Tabelle aller Commands gruppiert nach
//! Kategorie, Preset-Wahl, Aufnahme-Modal, Konfliktanzeige, Zurücksetzen.

use crate::core::commands::CommandRegistry;
use crate::core::keyboard::{
    chord_from_keypress, serialize_chords, KeyChord, KeymapStore,
};
use crate::overlays::command_palette::shortcut_chip;
use crate::state::AppState;
use crate::stores::DialogId;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::drop_shadow;
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::select::select;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::widgets::{IconButton, TextButton, TextButtonStyle};
use crate::ui::{FontKind, Ui};
use raylib::consts::{KeyboardKey, MouseCursor};

struct RecorderTarget {
    command_id: String,
    command_title: String,
    /// None = neues Binding anhängen, sonst Index des zu ersetzenden Bindings.
    replace_index: Option<usize>,
    chords: Vec<KeyChord>,
}

pub struct ShortcutEditor {
    query: TextInputState,
    scroll: ScrollState,
    recorder: Option<RecorderTarget>,
    was_open: bool,
}

impl Default for ShortcutEditor {
    fn default() -> Self {
        ShortcutEditor {
            query: TextInputState::default(),
            scroll: ScrollState::default(),
            recorder: None,
            was_open: false,
        }
    }
}

impl ShortcutEditor {
    pub fn render(&mut self, ui: &mut Ui, state: &mut AppState, registry: &CommandRegistry) {
        if state.app.open_dialog != Some(DialogId::Shortcuts) {
            self.was_open = false;
            return;
        }
        if !self.was_open {
            self.was_open = true;
            self.query.set_text("");
            self.scroll.offset_y = 0.0;
            self.recorder = None;
        }

        // Escape schließt — außer der Recorder ist offen (der fängt selbst).
        if self.recorder.is_none()
            && ui.input.keys.iter().any(|k| k.key == KeyboardKey::KEY_ESCAPE)
        {
            state.app.open_dialog = None;
            return;
        }

        ui.fill(ui.screen, theme::with_alpha(theme::BLACK, 102));
        let w = 760.0_f32.min(ui.screen.w - 48.0);
        let h = (ui.screen.h - 120.0).clamp(320.0, 640.0);
        let rect = Rect::new(
            ui.screen.x + (ui.screen.w - w) / 2.0,
            ui.screen.y + (ui.screen.h - h) / 2.0,
            w,
            h,
        );
        drop_shadow(ui, rect, theme::RADIUS_LG);
        ui.fill_rounded(rect, theme::RADIUS_LG, theme::SURFACE_1);
        ui.stroke_rounded(rect, theme::RADIUS_LG, 1.0, theme::LINE_STRONG);

        let mut area = rect;

        // ---- Kopfzeile: Icon + Titel | Preset | Alle zurücksetzen | X ----
        let head = area.cut_top(48.0);
        ui.hline(head.x, head.bottom() - 1.0, head.w, theme::LINE);
        let mut hi = head.inset_xy(16.0, 0.0);
        let icon_cell = hi.cut_left(18.0);
        ui.icon("keyboard", icon_cell, 18.0, theme::TEXT_2);
        hi.cut_left(8.0);
        ui.text_left("Tastaturbefehle", hi, theme::TEXT_1, FontKind::Sans16Semibold);

        let close = Rect::new(head.right() - 16.0 - 28.0, head.y + 10.0, 28.0, 28.0);
        if IconButton::new("x")
            .tooltip("Schließen")
            .show(ui, "shortcuts.close", close)
            .clicked
        {
            state.app.open_dialog = None;
            return;
        }
        let reset_btn = TextButton::new("Alle zurücksetzen").style(TextButtonStyle::Outline);
        let rw = reset_btn.measure(ui);
        let reset_rect = Rect::new(close.x - 8.0 - rw, head.y + 12.0, rw, 24.0);
        if reset_btn.show(ui, "shortcuts.resetAll", reset_rect).clicked {
            state.keymap.reset_all();
        }
        // Preset-Auswahl
        let preset_names: Vec<&str> = state.keymap.presets.iter().map(|p| p.name).collect();
        let active_idx = state
            .keymap
            .presets
            .iter()
            .position(|p| p.id == state.keymap.active_preset_id)
            .unwrap_or(0);
        let select_rect = Rect::new(reset_rect.x - 8.0 - 190.0, head.y + 12.0, 190.0, 24.0);
        if let Some(idx) = select(ui, "shortcuts.preset", select_rect, &preset_names, active_idx) {
            let id = state.keymap.presets[idx].id;
            state.keymap.set_active_preset(id);
        }

        // ---- Suchfeld ----
        let search_row = area.cut_top(44.0);
        ui.hline(search_row.x, search_row.bottom() - 1.0, search_row.w, theme::LINE);
        let mut sr = search_row.inset_xy(16.0, 0.0);
        let icon_cell = sr.cut_left(14.0);
        ui.icon("search", icon_cell, 14.0, theme::TEXT_3);
        sr.cut_left(8.0);
        let field = Rect::new(sr.x, search_row.y + 10.0, sr.w, 24.0);
        self.query.show(ui, "shortcuts.search", field, "Befehle durchsuchen …");

        // ---- Tabelle ----
        let q = self.query.text.trim().to_lowercase();
        struct Row {
            cmd_idx: usize,
            category_header: Option<String>,
        }
        let mut rows: Vec<Row> = Vec::new();
        let mut last_cat = "";
        for (i, cmd) in registry.all().iter().enumerate() {
            if !q.is_empty()
                && !format!("{} {} {}", cmd.category, cmd.title, cmd.id)
                    .to_lowercase()
                    .contains(&q)
            {
                continue;
            }
            let header = if cmd.category != last_cat {
                last_cat = cmd.category;
                Some(cmd.category.to_string())
            } else {
                None
            };
            rows.push(Row { cmd_idx: i, category_header: header });
        }

        let row_h = 32.0;
        let header_h = 28.0;
        let content_h: f32 = rows
            .iter()
            .map(|r| row_h + if r.category_header.is_some() { header_h } else { 0.0 })
            .sum::<f32>()
            + 8.0;

        let list_rect = area;
        let view = self.scroll.begin(ui, list_rect, 0.0, content_h);
        let mut y = view.origin_y + 4.0;
        let vw = view.viewport.w;

        let mut record_request: Option<(String, String, Option<usize>)> = None;
        let mut reset_request: Option<String> = None;

        for row in &rows {
            let cmd = &registry.all()[row.cmd_idx];
            if let Some(cat) = &row.category_header {
                let hr = Rect::new(view.viewport.x + 16.0, y, vw - 32.0, header_h);
                if hr.bottom() >= view.viewport.y && hr.y <= view.viewport.bottom() {
                    ui.text_left(cat, hr, theme::TEXT_3, FontKind::Sans12Medium);
                    ui.hline(hr.x, hr.bottom() - 4.0, hr.w, theme::LINE);
                }
                y += header_h;
            }
            let r = Rect::new(view.viewport.x + 16.0, y, vw - 32.0, row_h);
            y += row_h;
            if r.bottom() < view.viewport.y || r.y > view.viewport.bottom() {
                continue;
            }
            if ui.mouse_in(r) {
                ui.fill(r, theme::SURFACE_2);
            }
            let mut inner = r;
            // Plus + Reset rechts
            let plus = Rect::new(inner.right() - 24.0, r.y + 5.0, 22.0, 22.0);
            inner.cut_right(28.0);
            if IconButton::new("plus")
                .size(14.0)
                .tooltip("Tastenkombination hinzufügen")
                .show(ui, ("shortcuts.add", cmd.id.as_str()), plus)
                .clicked
            {
                record_request = Some((cmd.id.clone(), cmd.title.clone(), None));
            }
            if state.keymap.is_customized(&cmd.id) {
                let undo = Rect::new(inner.right() - 24.0, r.y + 5.0, 22.0, 22.0);
                inner.cut_right(28.0);
                if IconButton::new("rotate-ccw")
                    .size(14.0)
                    .tooltip("Auf Preset zurücksetzen")
                    .show(ui, ("shortcuts.reset", cmd.id.as_str()), undo)
                    .clicked
                {
                    reset_request = Some(cmd.id.clone());
                }
            }

            // Chips (rechtsbündig vor den Buttons; Klick = ersetzen)
            let mut chip_x = inner.right();
            let bindings = state.keymap.bindings_for_command(&cmd.id);
            for (bi, binding) in bindings.iter().enumerate().rev() {
                let conflicts = state.keymap.conflicts_for(&binding.keys, Some(&cmd.id));
                let used = shortcut_chip(ui, chip_x, r.center().y, &binding.keys, !conflicts.is_empty());
                let chip_rect = Rect::new(chip_x - used, r.center().y - 9.0, used, 18.0);
                let chip_id = ui.id(("shortcuts.chip", cmd.id.as_str(), bi));
                if ui.mouse_in(chip_rect) {
                    ui.set_hot(chip_id);
                    ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                    if !conflicts.is_empty() {
                        let names: Vec<String> = conflicts
                            .iter()
                            .map(|c| {
                                registry
                                    .get(&c.command)
                                    .map(|x| x.title.clone())
                                    .unwrap_or_else(|| c.command.clone())
                            })
                            .collect();
                        ui.tooltip(chip_id, chip_rect, &format!("Konflikt mit: {}", names.join(", ")));
                    } else {
                        ui.tooltip(chip_id, chip_rect, "Klick: Kombination ändern");
                    }
                    if ui.input.left_pressed {
                        record_request = Some((cmd.id.clone(), cmd.title.clone(), Some(bi)));
                    }
                }
                chip_x -= used + 4.0;
            }

            inner.w = (chip_x - inner.x - 8.0).max(0.0);
            let title = ui.font(FontKind::Sans12).ellipsize(&cmd.title, inner.w);
            ui.text_left(&title, inner, theme::TEXT_1, FontKind::Sans12);
        }
        self.scroll.end(ui, list_rect, 0.0, content_h);

        if let Some((command_id, command_title, replace_index)) = record_request {
            self.recorder = Some(RecorderTarget {
                command_id,
                command_title,
                replace_index,
                chords: Vec::new(),
            });
        }
        if let Some(id) = reset_request {
            state.keymap.reset_command(&id);
        }

        // ---- Recorder-Modal ----
        self.render_recorder(ui, state);
    }

    fn render_recorder(&mut self, ui: &mut Ui, state: &mut AppState) {
        let Some(recorder) = &mut self.recorder else { return };

        // Tasten aufnehmen (Escape = Abbrechen, Enter = Übernehmen)
        let mut cancel = false;
        let mut submit = false;
        let presses: Vec<crate::ui::input::KeyPress> = ui.input.keys.clone();
        for press in &presses {
            match press.key {
                KeyboardKey::KEY_ESCAPE => cancel = true,
                KeyboardKey::KEY_ENTER | KeyboardKey::KEY_KP_ENTER
                    if !recorder.chords.is_empty() =>
                {
                    submit = true
                }
                KeyboardKey::KEY_BACKSPACE if !recorder.chords.is_empty() => {
                    recorder.chords.pop();
                }
                _ => {
                    if let Some(chord) = chord_from_keypress(press) {
                        if recorder.chords.len() < 2 {
                            recorder.chords.push(chord);
                        }
                    }
                }
            }
        }

        ui.fill(ui.screen, theme::with_alpha(theme::BLACK, 102));
        let w = 420.0_f32;
        let h = 190.0_f32;
        let rect = ui.screen.center_box(w, h);
        drop_shadow(ui, rect, theme::RADIUS_LG);
        ui.fill_rounded(rect, theme::RADIUS_LG, theme::SURFACE_2);
        ui.stroke_rounded(rect, theme::RADIUS_LG, 1.0, theme::LINE_STRONG);

        let mut area = rect.inset(16.0);
        let title_row = area.cut_top(22.0);
        ui.text_left(
            &format!("Neue Kombination: {}", recorder.command_title),
            title_row,
            theme::TEXT_1,
            FontKind::Sans14Semibold,
        );
        area.cut_top(6.0);
        let hint_row = area.cut_top(18.0);
        ui.text_left(
            "Tasten drücken (max. 2 Chords) — Rücktaste löscht, Enter übernimmt.",
            hint_row,
            theme::TEXT_3,
            FontKind::Sans12,
        );
        area.cut_top(10.0);

        // Aufnahme-Anzeige
        let display_rect = area.cut_top(40.0);
        ui.fill_rounded(display_rect, theme::RADIUS_SM, theme::SURFACE_1);
        ui.stroke_rounded(display_rect, theme::RADIUS_SM, 1.0, theme::ACCENT);
        if recorder.chords.is_empty() {
            ui.text_centered("…", display_rect, theme::TEXT_3, FontKind::Sans14);
        } else {
            let keys = serialize_chords(&recorder.chords);
            ui.text_centered(
                &crate::core::keyboard::format_keys(&keys),
                display_rect,
                theme::TEXT_1,
                FontKind::Mono12,
            );
        }
        // Konflikt-Hinweis
        area.cut_top(8.0);
        let conflict_row = area.cut_top(16.0);
        if !recorder.chords.is_empty() {
            let keys = serialize_chords(&recorder.chords);
            let conflicts = state.keymap.conflicts_for(&keys, Some(&recorder.command_id));
            if !conflicts.is_empty() {
                ui.text_left(
                    &format!("⚠ Konflikt mit {} weiteren Belegungen", conflicts.len()),
                    conflict_row,
                    theme::WARNING,
                    FontKind::Sans12,
                );
            }
        }

        // Buttons
        area.cut_top(8.0);
        let btn_row = area.cut_top(28.0);
        let apply = TextButton::new("Übernehmen");
        let aw = apply.measure(ui);
        let cancel_btn = TextButton::new("Abbrechen").style(TextButtonStyle::Outline);
        let cw = cancel_btn.measure(ui);
        let apply_rect = Rect::new(btn_row.right() - aw, btn_row.y, aw, 26.0);
        let cancel_rect = Rect::new(apply_rect.x - 8.0 - cw, btn_row.y, cw, 26.0);
        if cancel_btn.show(ui, "recorder.cancel", cancel_rect).clicked {
            cancel = true;
        }
        if !recorder.chords.is_empty() && apply.show(ui, "recorder.apply", apply_rect).clicked {
            submit = true;
        }

        if submit && !recorder.chords.is_empty() {
            let keys = serialize_chords(&recorder.chords);
            let mut bindings = state.keymap.bindings_for_command(&recorder.command_id);
            let new_binding = KeymapStore::make_binding(&recorder.command_id, &keys);
            match recorder.replace_index {
                Some(i) if i < bindings.len() => bindings[i] = new_binding,
                _ => bindings.push(new_binding),
            }
            let command_id = recorder.command_id.clone();
            state.keymap.set_user_bindings(&command_id, bindings);
            self.recorder = None;
        } else if cancel {
            self.recorder = None;
        }
    }
}
