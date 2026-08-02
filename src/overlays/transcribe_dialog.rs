//! „Auto-Transkription“-Dialog (modal): Whisper-Klasse-Transkription des
//! ausgewählten Clip-Audios zu getimten Untertiteln. Wählt die Sprache, zeigt
//! den Modell-/Binary-Status und startet den asynchronen Job (Fortschritt/
//! Abbruch laufen wie beim Proxy-Workflow, sichtbar im Untertitel-Panel).
//!
//! Die externe whisper.cpp-CLI und das Modell sind optional/konfigurierbar
//! (Einstellungen → Medien). Ohne konfiguriertes Modell ist „Transkribieren“
//! deaktiviert und der Dialog verweist auf die Einstellungen.

use crate::core::commands::{transcribe_reference, TranscribeSource};
use crate::core::timecode::format_sequence_timecode;
use crate::core::transcribe::{language_index, LANGUAGES};
use crate::overlays::sequence_dialog::primary_button;
use crate::services::Services;
use crate::state::AppState;
use crate::stores::DialogId;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::select::select;
use crate::ui::widgets::{drop_shadow, IconButton, TextButton, TextButtonStyle};
use crate::ui::{FontKind, Ui};
use raylib::consts::KeyboardKey;

#[derive(Default)]
pub struct TranscribeDialog {
    /// Gewählter Sprachcode (`auto`/`de`/…) — beim Öffnen aus den Einstellungen.
    language: String,
    was_open: bool,
}

impl TranscribeDialog {
    pub fn render(&mut self, ui: &mut Ui, state: &mut AppState, services: &Services) {
        if state.app.open_dialog != Some(DialogId::Transcribe) {
            self.was_open = false;
            return;
        }
        // Quelle bei jedem Frame frisch auflösen (die Auswahl kann wandern).
        let Some(src) = transcribe_reference(state) else {
            state.app.open_dialog = None;
            self.was_open = false;
            return;
        };
        if !self.was_open {
            self.was_open = true;
            self.language = state.settings.whisper_language.clone();
        }

        // ESC schließt — außer ein offenes Dropdown/Textfeld verarbeitet die
        // Taste selbst (sonst würde ESC im Sprach-Dropdown den Dialog schließen).
        if ui.input.keys.iter().any(|k| k.key == KeyboardKey::KEY_ESCAPE)
            && ui.persist.select.popup.is_none()
            && ui.persist.keyboard_focus == 0
        {
            state.app.open_dialog = None;
            return;
        }

        ui.fill(ui.screen, theme::with_alpha(theme::BLACK, 130));
        let w = 520f32.min(ui.screen.w - 32.0);
        let h = 364f32.min(ui.screen.h - 32.0);
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
        ui.icon("sparkles", icon_cell, 18.0, theme::ACCENT);
        hi.cut_left(8.0);
        ui.text_left("Auto-Transkription", hi, theme::TEXT_1, FontKind::Sans16Semibold);
        let close = Rect::new(head.right() - 16.0 - 28.0, head.y + 10.0, 28.0, 28.0);
        if IconButton::new("x")
            .tooltip("Schließen (Esc)")
            .show(ui, "transcribe.close", close)
            .clicked
        {
            state.app.open_dialog = None;
            return;
        }

        let footer = area.cut_bottom(52.0);
        let mut body = area.inset_xy(16.0, 12.0);

        // ---- Quelle ----
        let mut row = body.cut_top(20.0);
        let cell = row.cut_left(120.0);
        ui.text_left("Quelle", cell, theme::TEXT_2, FontKind::Sans12);
        let name = ui.font(FontKind::Sans12).ellipsize(&src.clip_name, row.w);
        ui.text_left(&name, row, theme::TEXT_1, FontKind::Sans12);
        body.cut_top(4.0);
        let mut row = body.cut_top(20.0);
        let cell = row.cut_left(120.0);
        ui.text_left("Länge", cell, theme::TEXT_2, FontKind::Sans12);
        let dur = format_sequence_timecode(src.clip_dur, &state.timeline.settings);
        ui.text_left(&dur, row, theme::TEXT_1, FontKind::Mono12);
        body.cut_top(10.0);

        // ---- Sprache ----
        let mut row = body.cut_top(28.0);
        let cell = row.cut_left(120.0);
        ui.text_left("Sprache", Rect::new(cell.x, cell.y + 5.0, cell.w, 24.0), theme::TEXT_2, FontKind::Sans12);
        let labels: Vec<&str> = LANGUAGES.iter().map(|(_, l)| *l).collect();
        let cur = language_index(&self.language);
        let sel = Rect::new(row.x, row.y + 1.0, row.w.min(300.0), 26.0);
        if let Some(i) = select(ui, "transcribe.lang", sel, &labels, cur) {
            self.language = LANGUAGES[i].0.to_string();
        }
        body.cut_top(12.0);

        // ---- Modell-/Binary-Status ----
        let ready = state.settings.transcription_ready();
        let ff_ok = state.app.ffmpeg.as_ref().is_some_and(|i| i.available);
        let model_row = body.cut_top(18.0);
        let (txt, col, icon) = if ready {
            let name = state
                .settings
                .whisper_model
                .as_deref()
                .and_then(|p| std::path::Path::new(p).file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (format!("Modell: {name}"), theme::SUCCESS, "circle-check")
        } else {
            (
                "Kein Whisper-Modell konfiguriert.".to_string(),
                theme::WARNING,
                "triangle-alert",
            )
        };
        ui.icon(icon, Rect::new(model_row.x, model_row.y, 14.0, 14.0), 14.0, col);
        let disp = ui.font(FontKind::Sans12).ellipsize(&txt, model_row.w - 20.0);
        ui.text_left(&disp, Rect::new(model_row.x + 20.0, model_row.y, model_row.w - 20.0, 18.0), col, FontKind::Sans12);
        body.cut_top(6.0);

        if !ready {
            let hint = body.cut_top(34.0);
            ui.text_left(
                "Lege die whisper.cpp-CLI und ein Modell (ggml-*.bin) unter\nEinstellungen → Medien fest. Modelle einmalig herunterladen.",
                hint,
                theme::TEXT_3,
                FontKind::Sans12,
            );
            let open = TextButton::new("Einstellungen öffnen…").style(TextButtonStyle::Outline);
            let ow = open.measure(ui).max(160.0);
            if open
                .show(ui, "transcribe.settings", Rect::new(body.x, body.cut_top(30.0).y, ow, 26.0))
                .clicked
            {
                ui.run_command("app.settings");
            }
        } else if !ff_ok {
            let hint = body.cut_top(18.0);
            ui.text_left(
                "FFmpeg nicht gefunden — Audio-Extraktion nicht möglich.",
                hint,
                theme::DANGER,
                FontKind::Sans12,
            );
        } else {
            let hint = body.cut_top(34.0);
            ui.text_left(
                "Das Clip-Audio wird extrahiert und im Hintergrund transkribiert.\nDie Cues landen auf einer neuen Untertitel-Spur.",
                hint,
                theme::TEXT_3,
                FontKind::Sans12,
            );
        }

        // ---- Fußzeile ----
        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        let f = footer.inset_xy(16.0, 0.0);
        let can_run = ready && ff_ok;
        let rw = 140.0f32;
        let run_rect = Rect::new(f.right() - rw, f.y + 12.0, rw, 28.0);
        if primary_button(ui, "transcribe.run", run_rect, "Transkribieren", can_run).clicked
            && can_run
        {
            if let Some(task) = crate::core::commands::build_transcribe_task(state, &self.language) {
                services.start_transcribe_job(task);
                state.app.set_transcribe_running(&src.clip_id, 0.0);
                // Sprachwahl als neuen Standard merken.
                if state.settings.whisper_language != self.language {
                    state.settings.whisper_language = self.language.clone();
                    state.settings.save();
                }
                let msg = format!("Transkribiere „{}“ …", clip_short(&src));
                state.app.set_status_message(Some(msg), ui.time);
            }
            state.app.open_dialog = None;
            return;
        }

        let cancel = TextButton::new("Abbrechen").style(TextButtonStyle::Outline);
        let cw = cancel.measure(ui).max(96.0);
        if cancel
            .show(ui, "transcribe.cancel", Rect::new(run_rect.x - 8.0 - cw, f.y + 12.0, cw, 28.0))
            .clicked
        {
            state.app.open_dialog = None;
        }
    }
}

/// Kurzname des Quell-Clips (für die Statusmeldung).
fn clip_short(src: &TranscribeSource) -> String {
    src.clip_name.chars().take(32).collect()
}
