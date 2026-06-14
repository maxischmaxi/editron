//! Autosave-Versionen-Dialog: listet die Versionskopien des aktuellen Projekts
//! (Zeitstempel + Größe) und öffnet eine gewählte Version als UNGESPEICHERTE
//! Kopie (das Original bleibt unberührt). Dient auch der Absturz-
//! Wiederherstellung: ist `AppStore::autosave_recover_hint` gesetzt (eine
//! Version ist neuer als die Projektdatei), zeigt der Dialog beim Start einen
//! Hinweis und schlägt die jüngste Version vor.

use crate::core::autosave::{self, Version};
use crate::state::AppState;
use crate::stores::DialogId;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::{drop_shadow, IconButton, TextButton, TextButtonStyle};
use crate::ui::{FontKind, Ui};
use raylib::consts::{KeyboardKey, MouseCursor};
use std::path::PathBuf;

#[derive(Default)]
pub struct AutosaveDialog {
    was_open: bool,
    scroll: ScrollState,
    /// Versionen + Anzeigename des Projekts, beim Öffnen einmalig ermittelt.
    versions: Vec<Version>,
    project_name: String,
    dir_label: String,
}

impl AutosaveDialog {
    pub fn render(&mut self, ui: &mut Ui, state: &mut AppState) {
        if state.app.open_dialog != Some(DialogId::AutosaveVersions) {
            self.was_open = false;
            return;
        }
        if !self.was_open {
            self.was_open = true;
            self.refresh(state);
        }

        let esc = ui.input.keys.iter().any(|k| k.key == KeyboardKey::KEY_ESCAPE);
        if esc && ui.persist.keyboard_focus == 0 {
            self.close(state);
            return;
        }

        ui.fill(ui.screen, theme::with_alpha(theme::BLACK, 130));
        let w = 640f32.min(ui.screen.w - 32.0);
        let h = 500f32.min(ui.screen.h - 32.0);
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
        ui.icon("history", icon_cell, 18.0, theme::TEXT_2);
        hi.cut_left(8.0);
        ui.text_left("Autosave-Versionen", hi, theme::TEXT_1, FontKind::Sans16Semibold);
        let close = Rect::new(head.right() - 16.0 - 28.0, head.y + 10.0, 28.0, 28.0);
        if IconButton::new("x").tooltip("Schließen (Esc)").show(ui, "autosave.close", close).clicked {
            self.close(state);
            return;
        }

        // ---- Fußzeile ----
        let footer = area.cut_bottom(52.0);
        let mut body = area.inset_xy(16.0, 12.0);

        // ---- Absturz-Hinweis ----
        if let Some(hint) = state.app.autosave_recover_hint.clone() {
            let banner = body.cut_top(58.0);
            body.cut_top(10.0);
            ui.fill_rounded(banner, theme::RADIUS_SM, theme::with_alpha(theme::WARNING, 28));
            ui.stroke_rounded(banner, theme::RADIUS_SM, 1.0, theme::WARNING);
            let mut b = banner.inset_xy(12.0, 8.0);
            let top = b.cut_top(18.0);
            ui.icon("triangle-alert", Rect::new(top.x, top.y, 14.0, 16.0), 14.0, theme::WARNING);
            ui.text_left(
                "Editron wurde womöglich unerwartet beendet.",
                Rect::new(top.x + 20.0, top.y, top.w - 20.0, 18.0),
                theme::TEXT_1,
                FontKind::Sans12Medium,
            );
            let line2 = b.cut_top(16.0);
            ui.text_left(
                "Eine Autosave-Version ist neuer als die Projektdatei — jüngste Version wiederherstellen?",
                line2,
                theme::TEXT_2,
                FontKind::Sans12,
            );
            // Prominenter Wiederherstellen-Button rechts.
            let restore = TextButton::new("Wiederherstellen").style(TextButtonStyle::Solid);
            let rw = restore.measure(ui).max(132.0);
            if restore
                .show(ui, "autosave.restore", Rect::new(banner.right() - rw - 12.0, banner.y + banner.h - 34.0, rw, 26.0))
                .clicked
            {
                self.request_open(state, hint);
                return;
            }
        }

        // ---- Projekt-/Ordner-Zeile ----
        let info = body.cut_top(18.0);
        ui.text_left(
            &format!("Projekt: {}", self.project_name),
            info,
            theme::TEXT_1,
            FontKind::Sans12Medium,
        );
        let dir = body.cut_top(16.0);
        let dir_text = ui.font(FontKind::Mono12).ellipsize(&self.dir_label, dir.w);
        ui.text_left(&dir_text, dir, theme::TEXT_3, FontKind::Mono12);
        body.cut_top(8.0);
        ui.hline(body.x, body.y, body.w, theme::LINE);
        body.cut_top(8.0);

        // ---- Versionsliste ----
        if self.versions.is_empty() {
            let empty = body.cut_top(40.0);
            ui.text_centered(
                "Keine Autosave-Versionen vorhanden.",
                empty,
                theme::TEXT_3,
                FontKind::Sans12,
            );
        } else {
            let row_h = 34.0;
            let content_h = self.versions.len() as f32 * row_h;
            let mut open_path: Option<PathBuf> = None;
            let view = self.scroll.begin(ui, body, body.w, content_h);
            let mut y = view.origin_y;
            for (i, v) in self.versions.iter().enumerate() {
                let row = Rect::new(view.viewport.x, y, view.viewport.w, row_h);
                y += row_h;
                if row.bottom() < view.viewport.y || row.y > view.viewport.bottom() {
                    continue; // außerhalb des Sichtfensters
                }
                if i % 2 == 1 {
                    ui.fill_rounded(row, theme::RADIUS_SM, theme::SURFACE_0);
                }
                // Zeitstempel + Größe.
                let mut r = row.inset_xy(8.0, 0.0);
                let open_cell = r.cut_right(72.0);
                r.cut_right(8.0);
                let size_cell = r.cut_right(84.0);
                ui.icon("clock", Rect::new(r.x, r.y, 14.0, r.h), 13.0, theme::TEXT_3);
                ui.text_left(
                    &v.label,
                    Rect::new(r.x + 20.0, r.y, r.w - 20.0, r.h),
                    theme::TEXT_1,
                    FontKind::Sans12,
                );
                ui.text_left(&human_size(v.size), size_cell, theme::TEXT_3, FontKind::Mono12);
                let open = TextButton::new("Öffnen").style(TextButtonStyle::Outline);
                if open
                    .show(ui, ("autosave.open", i), Rect::new(open_cell.x, open_cell.y + 4.0, open_cell.w, 26.0))
                    .clicked
                {
                    open_path = Some(v.path.clone());
                }
            }
            self.scroll.end(ui, body, body.w, content_h);
            if let Some(p) = open_path {
                self.request_open(state, p);
                return;
            }
        }

        // ---- Fußzeile ----
        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        let mut f = footer.inset_xy(16.0, 0.0);
        let btn_row = f.cut_top(52.0);
        ui.text_left(
            "Öffnet als ungespeicherte Kopie — das Original bleibt unberührt.",
            Rect::new(btn_row.x, btn_row.y, btn_row.w - 130.0, btn_row.h),
            theme::TEXT_3,
            FontKind::Sans12,
        );
        let done = TextButton::new("Schließen").style(TextButtonStyle::Outline);
        let dw = done.measure(ui).max(100.0);
        if done
            .show(ui, "autosave.done", Rect::new(btn_row.right() - dw, btn_row.y + 12.0, dw, 28.0))
            .clicked
        {
            self.close(state);
        }

        if ui.mouse_in(rect) {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_DEFAULT);
        }
    }

    /// Versionen + Beschriftung des aktuellen Projekts (oder der Recovery-
    /// Quelle) ermitteln.
    fn refresh(&mut self, state: &AppState) {
        // Quelle: bei Absturz-Hinweis aus dem Versionspfad ableiten (Projekt
        // evtl. noch nicht geladen), sonst aus dem aktuellen Projekt.
        let (dir, stem) = match state
            .app
            .autosave_recover_hint
            .as_ref()
            .and_then(|p| version_dir_stem(p))
        {
            Some(ds) => ds,
            None => autosave::target_for(state),
        };
        self.versions = autosave::list_versions(&dir, &stem);
        self.project_name = if stem.is_empty() { "Unbenannt".into() } else { stem };
        self.dir_label = dir.to_string_lossy().into_owned();
    }

    fn request_open(&mut self, state: &mut AppState, path: PathBuf) {
        state.app.autosave_open_request = Some(path);
        self.close(state);
    }

    fn close(&mut self, state: &mut AppState) {
        state.app.open_dialog = None;
        state.app.autosave_recover_hint = None;
    }
}

/// Ordner + Projekt-Stamm aus einem Versionspfad ableiten.
fn version_dir_stem(path: &std::path::Path) -> Option<(PathBuf, String)> {
    let dir = path.parent()?.to_path_buf();
    let name = path.file_name()?.to_string_lossy();
    let stem = autosave::stem_of_version(&name)?;
    Some((dir, stem))
}

/// Dateigröße menschenlesbar (KB/MB).
fn human_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
