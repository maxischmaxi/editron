//! Marker-Bearbeiten-Dialog (modal): Name, Notiz, Farbe (8 Premiere-
//! Standardfarben), Timecode-Position und optionale Dauer (Bereichsmarker).
//! Wirkt auf Sequenz-, Clip- oder Asset-Marker (Ziel in
//! `AppStore::marker_editor`). Änderungen werden live übernommen; die ganze
//! Bearbeitung ist EIN Undo-Schritt (begin_marker_edit + …_live).

use crate::core::marker::{Marker, MarkerColor, MarkerScope};
use crate::core::timecode::{format_sequence_timecode, parse_sequence_timecode};
use crate::overlays::sequence_dialog::{labeled_row, primary_button, section};
use crate::state::AppState;
use crate::stores::{DialogId, MarkerEditTarget};
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::drop_shadow;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::widgets::{IconButton, TextButton, TextButtonStyle};
use crate::ui::{FontKind, Ui};
use raylib::consts::KeyboardKey;
use raylib::prelude::Color;

/// raylib-Farbe einer Markerfarbe.
pub fn marker_color(c: MarkerColor) -> Color {
    let (r, g, b) = c.rgb();
    Color::new(r, g, b, 255)
}

#[derive(Default)]
pub struct MarkerDialog {
    name_input: TextInputState,
    note_input: TextInputState,
    tc_input: TextInputState,
    dur_input: TextInputState,
    color: MarkerColor,
    /// Geladenes Ziel (Erkennung von Zielwechsel/Neu-Öffnen).
    loaded: Option<MarkerEditTarget>,
    /// Wurde für diese Bearbeitung schon ein Undo-Snapshot gesetzt?
    began: bool,
}

impl MarkerDialog {
    pub fn render(&mut self, ui: &mut Ui, state: &mut AppState) {
        if state.app.open_dialog != Some(DialogId::Marker) {
            self.loaded = None;
            return;
        }
        let Some(target) = state.app.marker_editor.clone() else {
            state.app.open_dialog = None;
            return;
        };
        let Some(marker) = read_marker(state, &target) else {
            // Marker verschwand (gelöscht/Undo) → Dialog schließen.
            state.app.open_dialog = None;
            state.app.marker_editor = None;
            return;
        };
        // Felder beim (Neu-)Öffnen aus dem Marker laden.
        if self.loaded.as_ref() != Some(&target) {
            self.loaded = Some(target.clone());
            self.began = false;
            self.color = marker.color;
            self.name_input.set_text(marker.name.clone());
            self.note_input.set_text(marker.note.clone());
            self.tc_input
                .set_text(format_sequence_timecode(marker.time, &state.timeline.settings));
            self.dur_input
                .set_text(format_sequence_timecode(marker.duration, &state.timeline.settings));
        }

        let is_sequence = matches!(target.scope, MarkerScope::Sequence);

        // ESC schließt — außer ein Textfeld verarbeitet die Taste selbst.
        let esc = ui.input.keys.iter().any(|k| k.key == KeyboardKey::KEY_ESCAPE);
        if esc && ui.persist.keyboard_focus == 0 {
            self.close(state);
            return;
        }

        ui.fill(ui.screen, theme::with_alpha(theme::BLACK, 130));
        let w = 420f32.min(ui.screen.w - 32.0);
        let h = (if is_sequence { 396.0_f32 } else { 332.0_f32 }).min(ui.screen.h - 32.0);
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
        // Farbiger Punkt als „Icon".
        let dot = Rect::new(icon_cell.x, icon_cell.y + (icon_cell.h - 12.0) / 2.0, 12.0, 12.0);
        ui.fill_rounded(dot, 6.0, marker_color(self.color));
        hi.cut_left(8.0);
        let title = match &target.scope {
            MarkerScope::Sequence => "Sequenz-Marker",
            MarkerScope::Clip(_) => "Clip-Marker",
            MarkerScope::Asset(_) => "Quell-Marker",
        };
        ui.text_left(title, hi, theme::TEXT_1, FontKind::Sans16Semibold);
        let close = Rect::new(head.right() - 16.0 - 28.0, head.y + 10.0, 28.0, 28.0);
        if IconButton::new("x")
            .tooltip("Schließen (Esc)")
            .show(ui, "marker.close", close)
            .clicked
        {
            self.close(state);
            return;
        }

        let footer = area.cut_bottom(52.0);
        let mut body = area.inset_xy(16.0, 12.0);

        // ---- Name ----
        let mut r = labeled_row(ui, &mut body, "Name");
        r.cut_left(0.0);
        let name_res = self.name_input.show(ui, "marker.name", r, "z. B. Schnitt-Idee");
        if name_res.changed {
            let txt = self.name_input.text.clone();
            self.ensure_begin(state, &target);
            apply(state, &target, |m| m.name = txt);
        }

        // ---- Notiz ----
        let mut r = labeled_row(ui, &mut body, "Notiz");
        r.cut_left(0.0);
        let note_res = self.note_input.show(ui, "marker.note", r, "Kommentar (optional)");
        if note_res.changed {
            let txt = self.note_input.text.clone();
            self.ensure_begin(state, &target);
            apply(state, &target, |m| m.note = txt);
        }

        body.cut_top(2.0);
        section(ui, &mut body, "Farbe");
        // ---- Farb-Swatches ----
        let row = body.cut_top(28.0);
        let gap = 8.0;
        let count = MarkerColor::ALL.len() as f32;
        let sw = ((row.w - gap * (count - 1.0)) / count).min(30.0);
        for (i, c) in MarkerColor::ALL.into_iter().enumerate() {
            let x = row.x + i as f32 * (sw + gap);
            let cell = Rect::new(x, row.y, sw, sw.min(row.h));
            let id = ui.id(("marker.swatch", i));
            let it = ui.interact(id, cell);
            ui.fill_rounded(cell, theme::RADIUS_SM, marker_color(c));
            if self.color == c {
                ui.stroke_rounded(cell, theme::RADIUS_SM, 2.0, theme::WHITE);
            } else if it.hovered {
                ui.stroke_rounded(cell, theme::RADIUS_SM, 1.0, theme::with_alpha(theme::WHITE, 160));
                ui.want_cursor(raylib::consts::MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            }
            if it.clicked {
                self.color = c;
                self.ensure_begin(state, &target);
                apply(state, &target, |m| m.color = c);
            }
        }
        body.cut_top(8.0);

        // ---- Position + Dauer (nur Sequenz-Marker editierbar) ----
        if is_sequence {
            section(ui, &mut body, "Position");
            let mut r = labeled_row(ui, &mut body, "Timecode");
            let field = r.cut_left(150.0);
            let tc_res = self.tc_input.show(ui, "marker.tc", field, "HH:MM:SS:FF");
            if tc_res.changed || tc_res.submitted {
                if let Some(t) = parse_sequence_timecode(&self.tc_input.text, &state.timeline.settings) {
                    let t = state.timeline.snap_to_frame(t);
                    self.ensure_begin(state, &target);
                    apply(state, &target, |m| m.time = t);
                }
            }
            let mut r = labeled_row(ui, &mut body, "Dauer");
            let field = r.cut_left(150.0);
            r.cut_left(8.0);
            ui.text_left("0 = Punkt", r, theme::TEXT_3, FontKind::Sans12);
            let dur_res = self.dur_input.show(ui, "marker.dur", field, "HH:MM:SS:FF");
            if dur_res.changed || dur_res.submitted {
                if let Some(d) = parse_sequence_timecode(&self.dur_input.text, &state.timeline.settings) {
                    let d = state.timeline.snap_to_frame(d.max(0.0));
                    self.ensure_begin(state, &target);
                    apply(state, &target, |m| m.duration = d);
                }
            }
        } else {
            // Clip-/Asset-Marker: Medienzeit nur anzeigen.
            let r = labeled_row(ui, &mut body, "Quellzeit");
            let label = format_sequence_timecode(marker.time, &state.timeline.settings);
            ui.text_left(&label, r, theme::TEXT_2, FontKind::Mono12);
        }

        // ---- Fußzeile: Löschen | Abbrechen | Fertig ----
        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        let f = footer.inset_xy(16.0, 0.0);
        let done_rect = Rect::new(f.right() - 110.0, f.y + 12.0, 110.0, 28.0);
        let submit = name_res.submitted;
        if primary_button(ui, "marker.done", done_rect, "Fertig", true).clicked || submit {
            self.close(state);
            return;
        }
        // Löschen ganz links (rot).
        let del = TextButton::new("Löschen").icon("trash-2").style(TextButtonStyle::Outline);
        let dw = del.measure(ui);
        if del
            .show(ui, "marker.delete", Rect::new(f.x, f.y + 12.0, dw, 28.0))
            .clicked
        {
            delete(state, &target);
            self.close(state);
            return;
        }
    }

    /// Einmaliger Undo-Snapshot für die ganze Bearbeitung (nur
    /// Sequenz-/Clip-Marker; Asset-Marker liegen außerhalb der History).
    fn ensure_begin(&mut self, state: &mut AppState, target: &MarkerEditTarget) {
        if self.began {
            return;
        }
        self.began = true;
        if !matches!(target.scope, MarkerScope::Asset(_)) {
            state.timeline.begin_marker_edit();
        }
    }

    fn close(&mut self, state: &mut AppState) {
        state.app.open_dialog = None;
        state.app.marker_editor = None;
        self.loaded = None;
    }
}

fn read_marker(state: &AppState, target: &MarkerEditTarget) -> Option<Marker> {
    match &target.scope {
        MarkerScope::Sequence => state
            .timeline
            .markers
            .iter()
            .find(|m| m.id == target.marker_id)
            .cloned(),
        MarkerScope::Clip(cid) => state
            .timeline
            .clips
            .iter()
            .find(|c| c.id == *cid)
            .and_then(|c| c.markers.iter().find(|m| m.id == target.marker_id))
            .cloned(),
        MarkerScope::Asset(aid) => state
            .media
            .asset(aid)
            .and_then(|a| a.markers.iter().find(|m| m.id == target.marker_id))
            .cloned(),
    }
}

/// Marker live ändern (ohne neuen Snapshot — begin ist bereits passiert).
fn apply(state: &mut AppState, target: &MarkerEditTarget, f: impl FnOnce(&mut Marker)) {
    match &target.scope {
        MarkerScope::Sequence => state.timeline.marker_update_live(&target.marker_id, f),
        MarkerScope::Clip(cid) => state
            .timeline
            .clip_marker_update_live(cid, &target.marker_id, f),
        MarkerScope::Asset(aid) => state.media.asset_marker_update(aid, &target.marker_id, f),
    }
}

fn delete(state: &mut AppState, target: &MarkerEditTarget) {
    match &target.scope {
        MarkerScope::Sequence => state.timeline.remove_marker(&target.marker_id),
        MarkerScope::Clip(cid) => state.timeline.remove_clip_marker(cid, &target.marker_id),
        MarkerScope::Asset(aid) => state.media.remove_asset_marker(aid, &target.marker_id),
    }
}
