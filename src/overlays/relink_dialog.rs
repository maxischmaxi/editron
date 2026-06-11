//! Relink-Wizard: fehlende Medien anzeigen, automatisch in einem Ordner
//! suchen (Worker-Scan mit Fortschritt/Abbruch) oder einzeln manuell
//! neu zuweisen. Relink-Ergebnisse treffen als ServiceEvents ein.

use crate::services::Services;
use crate::state::AppState;
use crate::stores::DialogId;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::drop_shadow;
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::{IconButton, TextButton, TextButtonStyle};
use crate::ui::{FontKind, Ui};
use raylib::consts::KeyboardKey;
use std::collections::HashMap;

#[derive(Default)]
pub struct RelinkDialog {
    pub scanning: bool,
    scanned_dirs: u64,
    /// Ergebnis des letzten Scans (abgebrochen, nicht gefunden).
    finished: Option<(bool, usize)>,
    /// Letzter Fehler je Asset (Probe fehlgeschlagen).
    errors: HashMap<String, String>,
    scroll: ScrollState,
}

impl RelinkDialog {
    pub fn on_scan_started(&mut self) {
        self.scanning = true;
        self.scanned_dirs = 0;
        self.finished = None;
        self.errors.clear();
    }

    pub fn on_progress(&mut self, scanned_dirs: u64) {
        self.scanned_dirs = scanned_dirs;
    }

    pub fn on_resolved(&mut self, asset_id: &str) {
        self.errors.remove(asset_id);
    }

    pub fn on_failed(&mut self, asset_id: &str, error: String) {
        self.errors.insert(asset_id.to_string(), error);
    }

    pub fn on_finished(&mut self, cancelled: bool, unresolved: usize) {
        self.scanning = false;
        self.finished = Some((cancelled, unresolved));
    }

    pub fn render(&mut self, ui: &mut Ui, state: &mut AppState, services: &Services) {
        if state.app.open_dialog != Some(DialogId::Relink) {
            return;
        }
        if ui.input.keys.iter().any(|k| k.key == KeyboardKey::KEY_ESCAPE) {
            state.app.open_dialog = None;
            return;
        }

        ui.fill(ui.screen, theme::with_alpha(theme::BLACK, 102));
        let w = 560.0_f32;
        let h = 420.0_f32;
        let rect = ui.screen.center_box(w, h);
        drop_shadow(ui, rect, theme::RADIUS_LG);
        ui.fill_rounded(rect, theme::RADIUS_LG, theme::SURFACE_1);
        ui.stroke_rounded(rect, theme::RADIUS_LG, 1.0, theme::LINE_STRONG);

        let mut area = rect;
        let head = area.cut_top(48.0);
        ui.hline(head.x, head.bottom() - 1.0, head.w, theme::LINE);
        let mut hi = head.inset_xy(16.0, 0.0);
        let icon_cell = hi.cut_left(18.0);
        ui.icon("link-2", icon_cell, 18.0, theme::TEXT_2);
        hi.cut_left(8.0);
        ui.text_left(
            "Medien wieder verknüpfen",
            hi,
            theme::TEXT_1,
            FontKind::Sans16Semibold,
        );
        let close = Rect::new(head.right() - 16.0 - 28.0, head.y + 10.0, 28.0, 28.0);
        if IconButton::new("x")
            .tooltip("Schließen")
            .show(ui, "relink.close", close)
            .clicked
        {
            state.app.open_dialog = None;
            return;
        }

        // Fußzeile zuerst reservieren (Buttons + Statuszeile).
        let footer = area.cut_bottom(56.0);
        let mut body = area.inset(16.0);

        let missing: Vec<(String, String, String)> = state
            .media
            .assets
            .iter()
            .filter(|a| a.offline)
            .map(|a| (a.id.clone(), a.name.clone(), a.path.clone()))
            .collect();

        if missing.is_empty() {
            let mut row = body.cut_top(20.0);
            let ic = row.cut_left(16.0);
            ui.icon("circle-check", ic, 16.0, theme::SUCCESS);
            row.cut_left(6.0);
            ui.text_left(
                "Alle Medien sind verbunden.",
                row,
                theme::SUCCESS,
                FontKind::Sans12,
            );
        } else {
            let intro = body.cut_top(18.0);
            ui.text_left(
                &format!(
                    "{} Medien wurden am gespeicherten Ort nicht gefunden:",
                    missing.len()
                ),
                intro,
                theme::TEXT_2,
                FontKind::Sans12,
            );
            body.cut_top(8.0);

            // ---- Liste fehlender Medien ----
            let row_h = 48.0;
            let content_h = missing.len() as f32 * row_h;
            let view = self.scroll.begin(ui, body, 0.0, content_h);
            for (i, (asset_id, name, path)) in missing.iter().enumerate() {
                let row = Rect::new(
                    view.viewport.x,
                    view.origin_y + i as f32 * row_h,
                    view.viewport.w,
                    row_h,
                );
                if row.bottom() < view.viewport.y || row.y > view.viewport.bottom() {
                    continue;
                }
                if i > 0 {
                    ui.hline(row.x, row.y, row.w, theme::LINE);
                }
                let mut r = row.inset_xy(4.0, 6.0);
                let ic = r.cut_left(18.0);
                ui.icon(
                    "triangle-alert",
                    Rect::new(ic.x, row.y + 8.0, 16.0, 16.0),
                    16.0,
                    theme::DANGER,
                );
                r.cut_left(8.0);

                // Button rechts
                let browse = TextButton::new("Durchsuchen…").style(TextButtonStyle::Outline);
                let bw = browse.measure(ui);
                let btn_cell = r.cut_right(bw);
                if browse
                    .show(
                        ui,
                        ("relink.browse", asset_id.as_str()),
                        Rect::new(btn_cell.x, row.y + (row_h - 24.0) / 2.0, bw, 24.0),
                    )
                    .clicked
                {
                    services.pick_relink_file(asset_id);
                }
                r.cut_right(8.0);

                let name_row = Rect::new(r.x, row.y + 6.0, r.w, 18.0);
                let display = ui.font(FontKind::Sans12).ellipsize(name, name_row.w);
                ui.text_left(&display, name_row, theme::TEXT_1, FontKind::Sans12);
                let detail_row = Rect::new(r.x, row.y + 24.0, r.w, 16.0);
                match self.errors.get(asset_id) {
                    Some(err) => {
                        let msg = ui.font(FontKind::Sans12).ellipsize(err, detail_row.w);
                        ui.text_left(&msg, detail_row, theme::DANGER, FontKind::Sans12);
                    }
                    None => {
                        let p = ui.font(FontKind::Mono12).ellipsize(path, detail_row.w);
                        ui.text_left(&p, detail_row, theme::TEXT_3, FontKind::Mono12);
                    }
                }
            }
            self.scroll.end(ui, body, 0.0, content_h);
        }

        // ---- Fußzeile: Status links, Aktionen rechts ----
        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        let mut f = footer.inset_xy(16.0, 0.0);
        let btn_y = footer.y + (footer.h - 28.0) / 2.0;

        let close_btn = TextButton::new("Schließen").style(TextButtonStyle::Outline);
        let cw = close_btn.measure(ui);
        let close_rect = f.cut_right(cw);
        if close_btn
            .show(ui, "relink.closeBtn", Rect::new(close_rect.x, btn_y, cw, 28.0))
            .clicked
        {
            state.app.open_dialog = None;
            return;
        }
        f.cut_right(8.0);

        if self.scanning {
            let cancel = TextButton::new("Suche abbrechen").icon("ban").style(TextButtonStyle::Outline);
            let bw = cancel.measure(ui);
            let cell = f.cut_right(bw);
            if cancel
                .show(ui, "relink.cancelScan", Rect::new(cell.x, btn_y, bw, 28.0))
                .clicked
            {
                services.cancel_relink_scan();
            }
        } else if !missing.is_empty() {
            let scan = TextButton::new("In Ordner suchen…").icon("folder-search");
            let bw = scan.measure(ui);
            let cell = f.cut_right(bw);
            if scan
                .show(ui, "relink.scan", Rect::new(cell.x, btn_y, bw, 28.0))
                .clicked
            {
                services.pick_relink_folder();
            }
        }
        f.cut_right(12.0);

        // Statuszeile
        let status = if self.scanning {
            format!(
                "Suche läuft … {} Ordner durchsucht",
                self.scanned_dirs.max(1)
            )
        } else {
            match self.finished {
                Some((true, _)) => "Suche abgebrochen".to_string(),
                Some((false, 0)) => "Suche abgeschlossen — alle Medien gefunden".to_string(),
                Some((false, n)) => format!("Suche abgeschlossen — {n} nicht gefunden"),
                None => String::new(),
            }
        };
        if !status.is_empty() {
            ui.text_left(&status, f, theme::TEXT_3, FontKind::Sans12);
        }
    }
}
