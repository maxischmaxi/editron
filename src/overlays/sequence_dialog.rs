//! Sequenzeinstellungen-Dialog (modal wie der Export-Dialog): Framerate
//! (Presets inkl. exakter NTSC-Raten + benutzerdefiniert), Drop-Frame-
//! Timecode, Auflösung (Presets HD–DCI 4K + frei). Zusätzlich der
//! „Sequenz an Medien anpassen?“-Prompt nach dem ersten Clip-Drop in eine
//! leere Timeline (Premiere-Verhalten).

use crate::core::sequence::{
    FrameRate, SequenceSettings, MAX_DIMENSION, MIN_DIMENSION, RESOLUTION_PRESETS,
};
use crate::state::AppState;
use crate::stores::DialogId;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::drop_shadow;
use crate::ui::widgets::select::select;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::widgets::{IconButton, TextButton, TextButtonStyle};
use crate::ui::{FontKind, Interaction, Ui};
use raylib::consts::{KeyboardKey, MouseCursor};

const LABEL_W: f32 = 140.0;

#[derive(Default)]
pub struct SequenceDialog {
    /// Index in FrameRate::PRESETS; PRESETS.len() = benutzerdefiniert.
    rate_choice: usize,
    fps_input: TextInputState,
    /// Index in RESOLUTION_PRESETS; len = benutzerdefiniert.
    res_choice: usize,
    width_input: TextInputState,
    height_input: TextInputState,
    drop_frame: bool,
    was_open: bool,
    /// Settings-Stand des letzten Formular-Syncs: ändern sich die Sequenz-
    /// Einstellungen extern (z. B. asynchroner Test-Import), zieht das
    /// Formular nach, statt veraltete Werte anzuzeigen.
    synced: Option<SequenceSettings>,
}

impl SequenceDialog {
    pub fn render(&mut self, ui: &mut Ui, state: &mut AppState) {
        match state.app.open_dialog {
            Some(DialogId::SequenceSettings) => {}
            Some(DialogId::MatchMedia) => {
                self.was_open = false;
                render_match_prompt(ui, state);
                return;
            }
            _ => {
                self.was_open = false;
                return;
            }
        }
        if !self.was_open || self.synced != Some(state.timeline.settings) {
            self.was_open = true;
            self.synced = Some(state.timeline.settings);
            self.sync_from_settings(&state.timeline.settings);
        }

        // ESC schließt — außer Popup/Textfeld verarbeiten die Taste selbst.
        let esc = ui.input.keys.iter().any(|k| k.key == KeyboardKey::KEY_ESCAPE);
        if esc && ui.persist.select.popup.is_none() && ui.persist.keyboard_focus == 0 {
            state.app.open_dialog = None;
            return;
        }

        ui.fill(ui.screen, theme::with_alpha(theme::BLACK, 130));
        let w = 520f32.min(ui.screen.w - 32.0);
        // Höhe wächst mit den Benutzerdefiniert-Eingabezeilen.
        let mut h = 396f32;
        if self.rate_choice >= FrameRate::PRESETS.len() {
            h += 32.0;
        }
        if self.res_choice >= RESOLUTION_PRESETS.len() {
            h += 32.0;
        }
        let h = h.min(ui.screen.h - 32.0);
        let rect = ui.screen.center_box(w, h);
        drop_shadow(ui, rect, theme::RADIUS_LG);
        ui.fill_rounded(rect, theme::RADIUS_LG, theme::SURFACE_1);
        ui.stroke_rounded(rect, theme::RADIUS_LG, 1.0, theme::LINE_STRONG);

        // ---- Kopfzeile ----
        let mut area = rect;
        let head = area.cut_top(48.0);
        ui.hline(head.x, head.bottom() - 1.0, head.w, theme::LINE);
        let mut hi = head.inset_xy(16.0, 0.0);
        let icon_cell = hi.cut_left(18.0);
        ui.icon("sliders-horizontal", icon_cell, 18.0, theme::TEXT_2);
        hi.cut_left(8.0);
        ui.text_left("Sequenzeinstellungen", hi, theme::TEXT_1, FontKind::Sans16Semibold);
        let close = Rect::new(head.right() - 16.0 - 28.0, head.y + 10.0, 28.0, 28.0);
        if IconButton::new("x")
            .tooltip("Schließen (Esc)")
            .show(ui, "seq.close", close)
            .clicked
        {
            state.app.open_dialog = None;
            return;
        }

        // ---- Fußzeile (Buttons + Validierung) ----
        let footer = area.cut_bottom(52.0);
        let mut body = area.inset_xy(16.0, 12.0);

        // ================= Zeitbasis =================
        section(ui, &mut body, "Zeitbasis");
        let r = labeled_row(ui, &mut body, "Framerate");
        let mut rate_labels: Vec<String> = FrameRate::PRESETS
            .iter()
            .map(|p| format!("{} fps", p.label()))
            .collect();
        rate_labels.push("Benutzerdefiniert …".into());
        let refs: Vec<&str> = rate_labels.iter().map(|s| s.as_str()).collect();
        if let Some(i) = select(ui, "seq.rate", r, &refs, self.rate_choice) {
            self.rate_choice = i;
            if let Some(rate) = FrameRate::PRESETS.get(i) {
                self.fps_input.set_text(rate.label());
                // NTSC-Broadcast-Konvention: Drop-Frame bei 29,97/59,94 an.
                self.drop_frame = rate.supports_drop_frame();
            }
        }
        if self.rate_choice >= FrameRate::PRESETS.len() {
            let mut r = labeled_row(ui, &mut body, "");
            let field = r.cut_left(88.0);
            r.cut_left(8.0);
            ui.text_left("fps", r, theme::TEXT_3, FontKind::Sans12);
            self.fps_input.show(ui, "seq.fpsCustom", field, "z. B. 29,97");
        }

        let rate = self.current_rate();
        let df_row = body.cut_top(24.0);
        let df_supported = rate.is_some_and(|r| r.supports_drop_frame());
        let it = checkbox(
            ui,
            "seq.dropFrame",
            df_row,
            "Drop-Frame-Timecode (Semikolon-Notation)",
            self.drop_frame && df_supported,
            df_supported,
        );
        if it.clicked && df_supported {
            self.drop_frame = !self.drop_frame;
        }
        if !df_supported {
            let hint = Rect::new(df_row.x + 22.0, df_row.bottom(), df_row.w - 22.0, 16.0);
            ui.text_left("— nur bei 29,97 und 59,94 fps", hint, theme::TEXT_3, FontKind::Sans12);
            body.cut_top(16.0);
        }
        body.cut_top(10.0);

        // ================= Raster =================
        section(ui, &mut body, "Raster");
        let r = labeled_row(ui, &mut body, "Auflösung");
        let mut res_labels: Vec<String> =
            RESOLUTION_PRESETS.iter().map(|(l, _, _)| l.to_string()).collect();
        res_labels.push("Benutzerdefiniert …".into());
        let refs: Vec<&str> = res_labels.iter().map(|s| s.as_str()).collect();
        if let Some(i) = select(ui, "seq.resolution", r, &refs, self.res_choice) {
            self.res_choice = i;
            if let Some((_, w, h)) = RESOLUTION_PRESETS.get(i) {
                self.width_input.set_text(w.to_string());
                self.height_input.set_text(h.to_string());
            }
        }
        if self.res_choice >= RESOLUTION_PRESETS.len() {
            let mut r = labeled_row(ui, &mut body, "");
            let wf = r.cut_left(72.0);
            r.cut_left(8.0);
            let xc = r.cut_left(10.0);
            ui.text_centered("×", xc, theme::TEXT_3, FontKind::Sans12);
            r.cut_left(8.0);
            let hf = r.cut_left(72.0);
            self.width_input.show(ui, "seq.width", wf, "Breite");
            self.height_input.show(ui, "seq.height", hf, "Höhe");
        }
        let r = labeled_row(ui, &mut body, "Pixel-Seitenverhältnis");
        ui.text_left("Quadratische Pixel (1,0)", r, theme::TEXT_3, FontKind::Sans12);

        // Hinweis wie Premiere: Clips werden nicht umgerechnet.
        body.cut_top(6.0);
        let note = body.cut_top(32.0);
        ui.text_left(
            "Bestehende Clips behalten ihre Zeiten; abweichende Quell-Frameraten",
            Rect::new(note.x, note.y, note.w, 16.0),
            theme::TEXT_3,
            FontKind::Sans12,
        );
        ui.text_left(
            "werden bei Wiedergabe und Export auf die Sequenzrate abgebildet.",
            Rect::new(note.x, note.y + 16.0, note.w, 16.0),
            theme::TEXT_3,
            FontKind::Sans12,
        );

        // ---- Fußzeile ----
        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        let mut f = footer.inset_xy(16.0, 0.0);
        let btn_row = f.cut_top(52.0);
        let result = self.build_settings();
        let apply_rect = Rect::new(btn_row.right() - 130.0, btn_row.y + 12.0, 130.0, 28.0);
        if primary_button(ui, "seq.apply", apply_rect, "Übernehmen", result.is_ok()).clicked {
            if let Ok(settings) = result {
                state.timeline.set_sequence_settings(settings);
                let msg = format!("Sequenzeinstellungen: {}", settings.describe());
                state.app.set_status_message(Some(msg), ui.time);
                state.app.open_dialog = None;
                return;
            }
        }
        let cancel = TextButton::new("Abbrechen").style(TextButtonStyle::Outline);
        let cw = cancel.measure(ui);
        if cancel
            .show(ui, "seq.cancel", Rect::new(apply_rect.x - 8.0 - cw, btn_row.y + 12.0, cw, 28.0))
            .clicked
        {
            state.app.open_dialog = None;
            return;
        }
        if let Err(msg) = &self.build_settings() {
            let err_rect = Rect::new(btn_row.x, btn_row.y + 12.0, apply_rect.x - 8.0 - cw - 16.0 - btn_row.x, 28.0);
            let msg = ui.font(FontKind::Sans12).ellipsize(msg, err_rect.w);
            ui.text_left(&msg, err_rect, theme::DANGER, FontKind::Sans12);
        }
    }

    /// Auswahl-Indizes und Textfelder aus den aktuellen Settings ableiten.
    fn sync_from_settings(&mut self, s: &SequenceSettings) {
        self.rate_choice = FrameRate::PRESETS
            .iter()
            .position(|p| *p == s.rate)
            .unwrap_or(FrameRate::PRESETS.len());
        self.fps_input.set_text(s.rate.label());
        self.res_choice = RESOLUTION_PRESETS
            .iter()
            .position(|(_, w, h)| (*w, *h) == (s.width, s.height))
            .unwrap_or(RESOLUTION_PRESETS.len());
        self.width_input.set_text(s.width.to_string());
        self.height_input.set_text(s.height.to_string());
        self.drop_frame = s.drop_frame;
    }

    fn current_rate(&self) -> Option<FrameRate> {
        if let Some(rate) = FrameRate::PRESETS.get(self.rate_choice) {
            return Some(*rate);
        }
        let fps: f64 = self.fps_input.text.trim().replace(',', ".").parse().ok()?;
        FrameRate::from_fps(fps)
    }

    fn build_settings(&self) -> Result<SequenceSettings, String> {
        let rate = self
            .current_rate()
            .ok_or_else(|| "Ungültige Framerate (z. B. 25 oder 29,97)".to_string())?;
        let width: u32 = self
            .width_input
            .text
            .trim()
            .parse()
            .map_err(|_| "Ungültige Breite".to_string())?;
        let height: u32 = self
            .height_input
            .text
            .trim()
            .parse()
            .map_err(|_| "Ungültige Höhe".to_string())?;
        if !(MIN_DIMENSION..=MAX_DIMENSION).contains(&width)
            || !(MIN_DIMENSION..=MAX_DIMENSION).contains(&height)
        {
            return Err(format!("Auflösung muss zwischen {MIN_DIMENSION} und {MAX_DIMENSION} liegen"));
        }
        Ok(SequenceSettings {
            rate,
            width,
            height,
            drop_frame: self.drop_frame && rate.supports_drop_frame(),
        })
    }
}

/// „Sequenz an Medien anpassen?“ — modaler Prompt nach dem ersten Clip-Drop
/// in eine leere Timeline, wenn das Material von den Einstellungen abweicht.
fn render_match_prompt(ui: &mut Ui, state: &mut AppState) {
    let Some(suggestion) = state.timeline.pending_media_match.clone() else {
        state.app.open_dialog = None;
        return;
    };

    // ESC = Einstellungen beibehalten.
    if ui.input.keys.iter().any(|k| k.key == KeyboardKey::KEY_ESCAPE) {
        state.timeline.pending_media_match = None;
        state.app.open_dialog = None;
        return;
    }

    ui.fill(ui.screen, theme::with_alpha(theme::BLACK, 130));
    let w = 520f32.min(ui.screen.w - 32.0);
    let h = 248f32.min(ui.screen.h - 32.0);
    let rect = ui.screen.center_box(w, h);
    drop_shadow(ui, rect, theme::RADIUS_LG);
    ui.fill_rounded(rect, theme::RADIUS_LG, theme::SURFACE_1);
    ui.stroke_rounded(rect, theme::RADIUS_LG, 1.0, theme::LINE_STRONG);

    let mut area = rect;
    let head = area.cut_top(48.0);
    ui.hline(head.x, head.bottom() - 1.0, head.w, theme::LINE);
    let mut hi = head.inset_xy(16.0, 0.0);
    let icon_cell = hi.cut_left(18.0);
    ui.icon("film", icon_cell, 18.0, theme::TEXT_2);
    hi.cut_left(8.0);
    ui.text_left("Sequenz an Medien anpassen?", hi, theme::TEXT_1, FontKind::Sans16Semibold);

    let footer = area.cut_bottom(52.0);
    let mut body = area.inset_xy(16.0, 12.0);

    let name = ui
        .font(FontKind::Sans12)
        .ellipsize(&suggestion.asset_name, body.w - 220.0);
    let intro = body.cut_top(18.0);
    ui.text_left(
        &format!("Der Clip „{name}“ weicht von den Sequenzeinstellungen ab."),
        intro,
        theme::TEXT_1,
        FontKind::Sans12,
    );
    body.cut_top(10.0);
    for (label, settings) in [
        ("Sequenz", state.timeline.settings),
        ("Clip", suggestion.settings),
    ] {
        let mut row = body.cut_top(20.0);
        let cell = row.cut_left(80.0);
        ui.text_left(label, cell, theme::TEXT_3, FontKind::Sans12);
        ui.text_left(&settings.describe(), row, theme::TEXT_1, FontKind::Mono12);
    }
    body.cut_top(10.0);
    let note = body.cut_top(18.0);
    ui.text_left(
        "Beim Anpassen behalten bestehende Clips ihre Zeiten.",
        note,
        theme::TEXT_3,
        FontKind::Sans12,
    );

    // ---- Buttons ----
    ui.hline(footer.x, footer.y, footer.w, theme::LINE);
    let f = footer.inset_xy(16.0, 0.0);
    let apply_rect = Rect::new(f.right() - 160.0, f.y + 12.0, 160.0, 28.0);
    if primary_button(ui, "seq.match.apply", apply_rect, "Sequenz anpassen", true).clicked {
        state.timeline.set_sequence_settings(suggestion.settings);
        state.timeline.pending_media_match = None;
        let msg = format!("Sequenz an Medien angepasst: {}", suggestion.settings.describe());
        state.app.set_status_message(Some(msg), ui.time);
        state.app.open_dialog = None;
        return;
    }
    let keep = TextButton::new("Einstellungen beibehalten").style(TextButtonStyle::Outline);
    let kw = keep.measure(ui);
    if keep
        .show(ui, "seq.match.keep", Rect::new(apply_rect.x - 8.0 - kw, f.y + 12.0, kw, 28.0))
        .clicked
    {
        state.timeline.pending_media_match = None;
        state.app.open_dialog = None;
    }
}

// ------------------------------------------------------------ UI-Bausteine

/// Beschriftete Formularzeile: Label links (LABEL_W), Feld rechts.
pub(crate) fn labeled_row(ui: &mut Ui, body: &mut Rect, label: &str) -> Rect {
    let mut row = body.cut_top(24.0);
    let label_cell = row.cut_left(LABEL_W);
    if !label.is_empty() {
        ui.text_left(label, label_cell, theme::TEXT_2, FontKind::Sans12);
    }
    body.cut_top(8.0);
    row
}

pub(crate) fn section(ui: &mut Ui, body: &mut Rect, title: &str) {
    body.cut_top(4.0);
    let row = body.cut_top(18.0);
    ui.text_left(title, row, theme::TEXT_3, FontKind::Sans12Medium);
    let line_y = row.bottom() + 2.0;
    ui.hline(row.x, line_y, row.w, theme::LINE);
    body.cut_top(10.0);
}

/// Checkbox mit Disabled-Zustand (14 px Kästchen + Label).
pub(crate) fn checkbox(
    ui: &mut Ui,
    id_src: impl std::hash::Hash,
    row: Rect,
    label: &str,
    checked: bool,
    enabled: bool,
) -> Interaction {
    let id = ui.id(id_src);
    let label_w = ui.font(FontKind::Sans12).width(label);
    let hit = Rect::new(row.x, row.y, 14.0 + 8.0 + label_w, row.h);
    let it = if enabled { ui.interact(id, hit) } else { Interaction::default() };
    let box_rect = Rect::new(row.x, row.y + (row.h - 14.0) / 2.0, 14.0, 14.0);
    if checked {
        ui.fill_rounded(box_rect, 3.0, theme::ACCENT);
        ui.icon("check", box_rect, 11.0, theme::WHITE);
    } else {
        ui.fill_rounded(box_rect, 3.0, theme::SURFACE_2);
        ui.stroke_rounded(
            box_rect,
            3.0,
            1.0,
            if it.hovered { theme::LINE_STRONG } else { theme::LINE },
        );
    }
    let text_rect = Rect::new(box_rect.right() + 8.0, row.y, label_w + 4.0, row.h);
    let fg = if !enabled {
        theme::TEXT_3
    } else if it.hovered {
        theme::TEXT_1
    } else {
        theme::TEXT_2
    };
    ui.text_left(label, text_rect, fg, FontKind::Sans12);
    if it.hovered {
        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
    }
    it
}

/// Akzentfarbener Primär-Button.
pub(crate) fn primary_button(
    ui: &mut Ui,
    id_src: impl std::hash::Hash,
    rect: Rect,
    label: &str,
    enabled: bool,
) -> Interaction {
    let id = ui.id(id_src);
    let it = if enabled { ui.interact(id, rect) } else { Interaction::default() };
    let bg = if !enabled {
        theme::with_alpha(theme::ACCENT, 90)
    } else if it.hovered || it.held {
        theme::ACCENT_HOVER
    } else {
        theme::ACCENT
    };
    ui.fill_rounded(rect, theme::RADIUS_SM, bg);
    let fg = if enabled { theme::WHITE } else { theme::with_alpha(theme::WHITE, 160) };
    ui.text_centered(label, rect, fg, FontKind::Sans12Medium);
    if it.hovered && enabled {
        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
    }
    it
}
