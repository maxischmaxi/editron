//! Ergebnis-Dialog für Interop-Import/-Export: Kennzahlen + ALLE Auslassungen.
//!
//! Interop darf nichts still verschlucken — was Editron in ein Format (oder aus
//! einem Format) nicht abbilden kann, landet als Warnung in dieser Liste. Beim
//! Import mit fehlenden Medien führt ein Button direkt in den Relink-Wizard.

use crate::state::AppState;
use crate::stores::DialogId;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::drop_shadow;
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::{IconButton, TextButton, TextButtonStyle};
use crate::ui::{FontKind, Ui};
use raylib::consts::KeyboardKey;

#[derive(Default)]
pub struct InteropReportDialog {
    scroll: ScrollState,
}

impl InteropReportDialog {
    pub fn render(&mut self, ui: &mut Ui, state: &mut AppState) {
        if state.app.open_dialog != Some(DialogId::InteropReport) {
            return;
        }
        let Some(report) = state.app.interop_report.clone() else {
            state.app.open_dialog = None;
            return;
        };
        if ui.input.keys.iter().any(|k| k.key == KeyboardKey::KEY_ESCAPE) {
            self.close(state);
            return;
        }

        ui.fill(ui.screen, theme::with_alpha(theme::BLACK, 102));
        let rect = ui.screen.center_box(600.0, 460.0);
        drop_shadow(ui, rect, theme::RADIUS_LG);
        ui.fill_rounded(rect, theme::RADIUS_LG, theme::SURFACE_1);
        ui.stroke_rounded(rect, theme::RADIUS_LG, 1.0, theme::LINE_STRONG);

        let mut area = rect;

        // ---- Kopfzeile ----
        let head = area.cut_top(48.0);
        ui.hline(head.x, head.bottom() - 1.0, head.w, theme::LINE);
        let mut hi = head.inset_xy(16.0, 0.0);
        let icon_cell = hi.cut_left(18.0);
        let icon = if report.is_import { "import" } else { "file-output" };
        ui.icon(icon, icon_cell, 18.0, theme::TEXT_2);
        hi.cut_left(8.0);
        ui.text_left(&report.title, hi, theme::TEXT_1, FontKind::Sans16Semibold);
        let close = Rect::new(head.right() - 16.0 - 28.0, head.y + 10.0, 28.0, 28.0);
        if IconButton::new("x")
            .tooltip("Schließen")
            .show(ui, "interop.close", close)
            .clicked
        {
            self.close(state);
            return;
        }

        let footer = area.cut_bottom(56.0);
        let mut body = area.inset(16.0);

        // ---- Kennzahlen ----
        for (label, value) in &report.summary {
            let mut row = body.cut_top(22.0);
            let label_cell = row.cut_left(96.0);
            ui.text_left(label, label_cell, theme::TEXT_3, FontKind::Sans12);
            let display = ui.font(FontKind::Sans12Medium).ellipsize(value, row.w);
            ui.text_left(&display, row, theme::TEXT_1, FontKind::Sans12Medium);
        }

        body.cut_top(8.0);
        ui.hline(body.x, body.y, body.w, theme::LINE);
        body.cut_top(8.0);

        // ---- Hinweise / Auslassungen ----
        if report.warnings.is_empty() {
            let mut row = body.cut_top(20.0);
            let ic = row.cut_left(16.0);
            ui.icon("circle-check", ic, 16.0, theme::SUCCESS);
            row.cut_left(6.0);
            ui.text_left(
                "Keine Auslassungen — alles wurde übertragen.",
                row,
                theme::SUCCESS,
                FontKind::Sans12,
            );
        } else {
            let heading = body.cut_top(18.0);
            ui.text_left(
                &format!("Hinweise ({})", report.warnings.len()),
                heading,
                theme::WARNING,
                FontKind::Sans12Medium,
            );
            body.cut_top(4.0);

            let row_h = 34.0;
            let content_h = report.warnings.len() as f32 * row_h;
            let view = self.scroll.begin(ui, body, 0.0, content_h);
            for (i, w) in report.warnings.iter().enumerate() {
                let row = Rect::new(
                    view.viewport.x,
                    view.origin_y + i as f32 * row_h,
                    view.viewport.w,
                    row_h,
                );
                if row.bottom() < view.viewport.y || row.y > view.viewport.bottom() {
                    continue;
                }
                let mut r = row.inset_xy(2.0, 4.0);
                let ic = r.cut_left(16.0);
                ui.icon(
                    "triangle-alert",
                    Rect::new(ic.x, row.y + 6.0, 14.0, 14.0),
                    14.0,
                    theme::WARNING,
                );
                r.cut_left(6.0);
                // Lange Hinweise auf zwei Zeilen umbrechen (keine stille Kürzung
                // wesentlicher Aussagen).
                let (l1, l2) = split_two_lines(ui, w, r.w);
                ui.text_left(
                    &l1,
                    Rect::new(r.x, row.y + 3.0, r.w, 15.0),
                    theme::TEXT_2,
                    FontKind::Sans12,
                );
                if !l2.is_empty() {
                    ui.text_left(
                        &l2,
                        Rect::new(r.x, row.y + 18.0, r.w, 15.0),
                        theme::TEXT_3,
                        FontKind::Sans12,
                    );
                }
            }
            self.scroll.end(ui, body, 0.0, content_h);
        }

        // ---- Fußzeile ----
        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        let mut f = footer.inset_xy(16.0, 0.0);
        let btn_y = footer.y + (footer.h - 28.0) / 2.0;

        let close_btn = TextButton::new("Schließen").style(TextButtonStyle::Outline);
        let cw = close_btn.measure(ui);
        let close_rect = f.cut_right(cw);
        if close_btn
            .show(ui, "interop.closeBtn", Rect::new(close_rect.x, btn_y, cw, 28.0))
            .clicked
        {
            self.close(state);
            return;
        }
        f.cut_right(8.0);

        // Bei fehlenden Medien direkt in den Relink-Wizard.
        if report.is_import && report.offline > 0 {
            let relink_label = format!("{} Medien verknüpfen…", report.offline);
            let relink = TextButton::new(&relink_label).icon("link-2");
            let bw = relink.measure(ui);
            let cell = f.cut_right(bw);
            if relink
                .show(ui, "interop.relink", Rect::new(cell.x, btn_y, bw, 28.0))
                .clicked
            {
                state.app.interop_report = None;
                state.app.open_dialog = Some(DialogId::Relink);
                return;
            }
        }
    }

    fn close(&mut self, state: &mut AppState) {
        state.app.open_dialog = None;
        state.app.interop_report = None;
        self.scroll = ScrollState::default();
    }
}

/// Einen langen Hinweis an einer Wortgrenze auf höchstens zwei Zeilen verteilen
/// (die zweite Zeile wird bei Bedarf ellipsiert).
fn split_two_lines(ui: &Ui, text: &str, width: f32) -> (String, String) {
    let font = ui.font(FontKind::Sans12);
    if font.width(text) <= width {
        return (text.to_string(), String::new());
    }
    let mut line1 = String::new();
    let mut rest = String::new();
    for word in text.split(' ') {
        if rest.is_empty() {
            let candidate = if line1.is_empty() {
                word.to_string()
            } else {
                format!("{line1} {word}")
            };
            if font.width(&candidate) <= width {
                line1 = candidate;
            } else if line1.is_empty() {
                line1 = word.to_string();
            } else {
                rest = word.to_string();
            }
        } else {
            rest.push(' ');
            rest.push_str(word);
        }
    }
    let line2 = font.ellipsize(&rest, width);
    (line1, line2)
}
