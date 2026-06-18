//! „Projekt konsolidieren …“: alle benutzten (oder alle importierten) Medien in
//! einen Zielordner `<ziel>/media` einsammeln und das Projekt portabel (relative
//! Pfade) als `<ziel>/<name>.etron` ablegen. Optional auf die benutzten Bereiche
//! gekürzt (neu kodiert, mit Reserve). Während der Arbeit zeigt der Dialog einen
//! Fortschrittsbalken; danach eine Zusammenfassung.

use crate::core::consolidate::{
    build_plan, AssetScope, ConsolidateOptions, ConsolidatePlan, ConsolidateResult,
};
use crate::services::Services;
use crate::state::AppState;
use crate::stores::DialogId;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::drop_shadow;
use crate::ui::widgets::select::select;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::widgets::{IconButton, TextButton, TextButtonStyle};
use crate::ui::{FontKind, Ui};
use raylib::consts::KeyboardKey;
use std::path::PathBuf;

use super::sequence_dialog::{checkbox, primary_button, section};

const LABEL_W: f32 = 130.0;

pub struct ConsolidateDialog {
    /// 0 = nur benutzte Medien, 1 = alle importierten.
    scope_choice: usize,
    trim: bool,
    handle_input: TextInputState,
    name_input: TextInputState,
    target_dir: Option<PathBuf>,
    was_open: bool,

    // ---- Laufzeit ----
    running: bool,
    done: usize,
    total: usize,
    pct: f64,
    current: String,
    /// In Arbeit befindlicher Plan (für die Übernahme beim Abschluss).
    active_plan: Option<ConsolidatePlan>,
    /// Transienter Validierungsfehler vor dem Start.
    error: Option<String>,
    /// Zusammenfassung nach dem Abschluss.
    result: Option<String>,
}

impl Default for ConsolidateDialog {
    fn default() -> Self {
        let mut handle_input = TextInputState::default();
        handle_input.set_text("1");
        ConsolidateDialog {
            scope_choice: 0,
            trim: false,
            handle_input,
            name_input: TextInputState::default(),
            target_dir: None,
            was_open: false,
            running: false,
            done: 0,
            total: 0,
            pct: 0.0,
            current: String::new(),
            active_plan: None,
            error: None,
            result: None,
        }
    }
}

impl ConsolidateDialog {
    /// Zielordner-Auswahl aus dem Verzeichnis-Dialog übernehmen.
    pub fn on_folder_picked(&mut self, path: Option<PathBuf>) {
        if let Some(p) = path {
            self.target_dir = Some(p);
            self.error = None;
        }
    }

    /// Fortschritt eines laufenden Konsolidierungs-Workers.
    pub fn on_progress(&mut self, done: usize, total: usize, pct: f64, current: String) {
        if self.running {
            self.done = done;
            self.total = total;
            self.pct = pct;
            self.current = current;
        }
    }

    /// Worker fertig: Ergebnis übernehmen (Pfade umbiegen + portabel speichern)
    /// und die Zusammenfassung anzeigen.
    pub fn on_done(&mut self, state: &mut AppState, results: Vec<ConsolidateResult>, now: f64) {
        self.running = false;
        let Some(plan) = self.active_plan.take() else { return };
        let outcome = crate::core::consolidate::finish(state, &plan, results);

        let mut parts = vec![format!("{} Medien kopiert", outcome.copied)];
        if !outcome.failed.is_empty() {
            parts.push(format!("{} fehlgeschlagen", outcome.failed.len()));
        }
        if outcome.skipped > 0 {
            parts.push(format!("{} offline übersprungen", outcome.skipped));
        }
        let summary = parts.join(", ");
        self.result = Some(summary.clone());

        let status = if let Some(e) = &outcome.save_error {
            format!("Konsolidierung: Speichern fehlgeschlagen — {e}")
        } else {
            format!("Projekt konsolidiert nach {} ({summary})", plan.etron_path.display())
        };
        state.app.set_status_message(Some(status), now);
    }

    fn close(&mut self, state: &mut AppState) {
        state.app.open_dialog = None;
        self.was_open = false;
        self.target_dir = None;
        self.active_plan = None;
        self.error = None;
        self.result = None;
    }

    pub fn render(&mut self, ui: &mut Ui, state: &mut AppState, services: &Services) {
        if state.app.open_dialog != Some(DialogId::Consolidate) {
            self.was_open = false;
            return;
        }
        if !self.was_open {
            self.was_open = true;
            self.error = None;
            self.result = None;
            if self.name_input.text.trim().is_empty() {
                self.name_input.set_text(&state.project.display_name());
            }
        }

        // ESC schließt — außer während der Arbeit oder wenn ein Popup/Textfeld
        // die Taste selbst verarbeitet.
        let esc = ui.input.keys.iter().any(|k| k.key == KeyboardKey::KEY_ESCAPE);
        if esc && !self.running && ui.persist.select.popup.is_none() && ui.persist.keyboard_focus == 0 {
            self.close(state);
            return;
        }

        ui.fill(ui.screen, theme::with_alpha(theme::BLACK, 130));
        let w = 540f32.min(ui.screen.w - 32.0);
        let mut h = 300f32;
        if self.trim && !self.running && self.result.is_none() {
            h += 32.0; // Reserve-Zeile
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
        ui.icon("folder-open", icon_cell, 18.0, theme::TEXT_2);
        hi.cut_left(8.0);
        ui.text_left("Projekt konsolidieren", hi, theme::TEXT_1, FontKind::Sans16Semibold);
        if !self.running {
            let close = Rect::new(head.right() - 16.0 - 28.0, head.y + 10.0, 28.0, 28.0);
            if IconButton::new("x")
                .tooltip("Schließen (Esc)")
                .show(ui, "consol.close", close)
                .clicked
            {
                self.close(state);
                return;
            }
        }

        let footer = area.cut_bottom(52.0);
        let body = area.inset_xy(16.0, 12.0);

        if self.running {
            self.render_progress(ui, body);
            // Fußzeile bleibt während der Arbeit leer (kein Abbruch in v1).
            ui.hline(footer.x, footer.y, footer.w, theme::LINE);
            return;
        }

        if let Some(result) = self.result.clone() {
            self.render_result(ui, state, &result, footer, body);
            return;
        }

        self.render_form(ui, state, services, footer, body);
    }

    fn render_form(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        services: &Services,
        footer: Rect,
        body: Rect,
    ) {
        let mut body = body;

        // ===== Medien =====
        section(ui, &mut body, "Medien");
        let r = labeled_row(ui, &mut body, "Umfang");
        let used_n = referenced_count(state);
        let all_n = state.media.assets.len();
        let labels = [
            format!("Nur benutzte Medien ({used_n})"),
            format!("Alle importierten Medien ({all_n})"),
        ];
        let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
        if let Some(i) = select(ui, "consol.scope", r, &refs, self.scope_choice) {
            self.scope_choice = i;
        }

        let trim_row = body.cut_top(24.0);
        let it = checkbox(
            ui,
            "consol.trim",
            trim_row,
            "Auf benutzte Bereiche kürzen (neu kodiert, mit Reserve)",
            self.trim,
            true,
        );
        if it.clicked {
            self.trim = !self.trim;
        }
        body.cut_top(8.0);
        if self.trim {
            let mut r = labeled_row(ui, &mut body, "Reserve");
            let field = r.cut_left(72.0);
            r.cut_left(8.0);
            ui.text_left("Sekunden vor/nach", r, theme::TEXT_3, FontKind::Sans12);
            self.handle_input.show(ui, "consol.handle", field, "1");
        }
        body.cut_top(4.0);

        // ===== Ziel =====
        section(ui, &mut body, "Ziel");
        let r = labeled_row(ui, &mut body, "Projektname");
        self.name_input.show(ui, "consol.name", r, "Projekt");

        let mut r = labeled_row(ui, &mut body, "Zielordner");
        let pick = TextButton::new("Ordner wählen …").style(TextButtonStyle::Outline);
        let pw = pick.measure(ui).max(130.0);
        let pick_rect = r.cut_left(pw);
        r.cut_left(8.0);
        if pick.show(ui, "consol.pick", pick_rect).clicked {
            services.pick_consolidate_folder();
        }
        let dir_text = match &self.target_dir {
            Some(p) => p.to_string_lossy().into_owned(),
            None => "— noch nicht gewählt".to_string(),
        };
        let fg = if self.target_dir.is_some() { theme::TEXT_1 } else { theme::TEXT_3 };
        let shown = ui.font(FontKind::Sans12).ellipsize(&dir_text, r.w);
        ui.text_left(&shown, r, fg, FontKind::Sans12);

        // ---- Fußzeile ----
        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        let mut f = footer.inset_xy(16.0, 0.0);
        let btn_row = f.cut_top(52.0);
        let can_run = self.target_dir.is_some() && !self.name_input.text.trim().is_empty();
        let run_rect = Rect::new(btn_row.right() - 150.0, btn_row.y + 12.0, 150.0, 28.0);
        if primary_button(ui, "consol.run", run_rect, "Konsolidieren", can_run).clicked && can_run {
            self.start(state, services);
        }
        let cancel = TextButton::new("Abbrechen").style(TextButtonStyle::Outline);
        let cw = cancel.measure(ui);
        if cancel
            .show(ui, "consol.cancel", Rect::new(run_rect.x - 8.0 - cw, btn_row.y + 12.0, cw, 28.0))
            .clicked
        {
            self.close(state);
            return;
        }
        if let Some(err) = &self.error {
            let err_rect = Rect::new(btn_row.x, btn_row.y + 12.0, run_rect.x - 8.0 - cw - 16.0 - btn_row.x, 28.0);
            let msg = ui.font(FontKind::Sans12).ellipsize(err, err_rect.w);
            ui.text_left(&msg, err_rect, theme::DANGER, FontKind::Sans12);
        }
    }

    fn start(&mut self, state: &mut AppState, services: &Services) {
        let scope = if self.scope_choice == 1 { AssetScope::All } else { AssetScope::UsedOnly };
        let handle = self.handle_input.text.trim().replace(',', ".").parse::<f64>().unwrap_or(1.0).max(0.0);
        let opts = ConsolidateOptions {
            scope,
            trim: self.trim,
            handle_sec: handle,
            project_name: self.name_input.text.trim().to_string(),
        };
        let Some(target) = self.target_dir.clone() else { return };
        match build_plan(state, &target, &opts) {
            Ok(plan) => {
                self.total = plan.items.len();
                self.done = 0;
                self.pct = 0.0;
                self.current = String::new();
                self.error = None;
                self.result = None;
                self.running = true;
                services.start_consolidate(plan.items.clone());
                self.active_plan = Some(plan);
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn render_progress(&self, ui: &mut Ui, body: Rect) {
        let mut body = body;
        body.cut_top(16.0);
        let line = body.cut_top(20.0);
        ui.text_left("Medien werden eingesammelt …", line, theme::TEXT_1, FontKind::Sans12Medium);
        body.cut_top(16.0);

        // Fortschrittsbalken: (abgeschlossene Items + aktueller Bruchteil)/total.
        let overall = if self.total > 0 {
            ((self.done as f64 + self.pct) / self.total as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let track = body.cut_top(10.0);
        ui.fill_rounded(track, 5.0, theme::SURFACE_3);
        if overall > 0.0 {
            let fill = Rect::new(track.x, track.y, (track.w * overall as f32).max(2.0), track.h);
            ui.fill_rounded(fill, 5.0, theme::ACCENT);
        }
        body.cut_top(12.0);

        let info = body.cut_top(18.0);
        let count = format!("{} von {}", self.done.min(self.total), self.total);
        ui.text_left(&count, info, theme::TEXT_2, FontKind::Sans12);
        if !self.current.is_empty() {
            let cur = body.cut_top(18.0);
            let shown = ui.font(FontKind::Mono12).ellipsize(&self.current, cur.w);
            ui.text_left(&shown, cur, theme::TEXT_3, FontKind::Mono12);
        }
    }

    fn render_result(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        result: &str,
        footer: Rect,
        body: Rect,
    ) {
        let mut body = body;
        body.cut_top(16.0);
        let icon_line = body.cut_top(24.0);
        let mut il = icon_line;
        let ic = il.cut_left(20.0);
        ui.icon("circle-check", ic, 18.0, theme::ACCENT);
        il.cut_left(8.0);
        ui.text_left("Konsolidierung abgeschlossen", il, theme::TEXT_1, FontKind::Sans16Semibold);
        body.cut_top(12.0);
        let r = body.cut_top(20.0);
        ui.text_left(result, r, theme::TEXT_2, FontKind::Sans12);
        if let Some(plan) = &self.active_plan {
            let r = body.cut_top(18.0);
            let shown = ui.font(FontKind::Mono12).ellipsize(&plan.etron_path.to_string_lossy(), r.w);
            ui.text_left(&shown, r, theme::TEXT_3, FontKind::Mono12);
        }

        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        let f = footer.inset_xy(16.0, 0.0);
        let btn_row = f;
        let done_rect = Rect::new(btn_row.right() - 110.0, btn_row.y + 12.0, 110.0, 28.0);
        if primary_button(ui, "consol.done", done_rect, "Schließen", true).clicked {
            self.close(state);
        }
    }
}

/// Anzahl in irgendeiner Sequenz benutzter Assets (für die Umfangsanzeige).
fn referenced_count(state: &AppState) -> usize {
    let mut ids = std::collections::HashSet::new();
    for seq in state.timeline.iter() {
        if let Some(mc) = &seq.timeline.multicam {
            for a in &mc.angles {
                if !a.asset_id.is_empty() {
                    ids.insert(a.asset_id.clone());
                }
            }
        }
        for clip in &seq.timeline.clips {
            if !clip.asset_id.is_empty() {
                ids.insert(clip.asset_id.clone());
            }
        }
    }
    ids.iter().filter(|id| state.media.asset(id).is_some()).count()
}

/// Beschriftete Formularzeile (wie im Sequenz-Dialog, eigene LABEL_W).
fn labeled_row(ui: &mut Ui, body: &mut Rect, label: &str) -> Rect {
    let mut row = body.cut_top(24.0);
    let label_cell = row.cut_left(LABEL_W);
    if !label.is_empty() {
        ui.text_left(label, label_cell, theme::TEXT_2, FontKind::Sans12);
    }
    body.cut_top(8.0);
    row
}
