//! Grafik-Panel: echter Titel-Inspektor. Bearbeitet den ausgewählten
//! Titel-Clip der Timeline (Text, Schrift, Füllung, Kontur, Schatten,
//! Hintergrund-Box, Ausrichtung/Position) — alle Änderungen sind
//! projektpersistiert und undo-fähig (Gesten-Snapshots wie im Farbe-Panel).
//! Kopfzeile: „Titel hinzufügen“ + Vorlagen (Bauchbinde, zentrierter
//! Titel, Abspann-Rolle); darunter die Liste aller Titel der Sequenz.

use crate::core::text_raster::list_font_families;
use crate::core::timeline::SelectMode;
use crate::core::title::{RgbaColor, TitleAlign, TitleSpec, TitleTemplate, TitleWeight};
use crate::panels::color::slider_row;
use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::{v2, Rect};
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::select::select;
use crate::ui::widgets::text_area::TextArea;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::widgets::{TextButton, TextButtonStyle};
use crate::ui::{FontKind, Ui};
use raylib::consts::MouseCursor;
use std::collections::HashMap;

const ROW_H: f32 = 26.0;
const GAP: f32 = 4.0;
const SECTION_GAP: f32 = 14.0;
const TEXTAREA_LINE_H: f32 = 16.0;

/// Positions-Presets: None = „Frei“ (X/Y-Slider), sonst pos_y-Zielwert.
const POS_PRESETS: [(&str, Option<f64>); 4] = [
    ("Frei", None),
    ("Oberes Drittel", Some(-33.0)),
    ("Mitte", Some(0.0)),
    ("Unteres Drittel", Some(33.0)),
];

// ------------------------------------------------------------------- Panel

pub struct GraphicsPanel {
    scroll: ScrollState,
    text_area: TextArea,
    hex_inputs: HashMap<&'static str, TextInputState>,
    /// Laufende Slider-Geste (ein Undo-Snapshot pro Drag).
    slider_gesture: bool,
    /// Tipp-Sitzung (Textfeld/Hex-Feld): ein Snapshot, solange der Fokus
    /// auf demselben Widget bleibt.
    text_gesture: bool,
    last_focus: u64,
    /// Clip, dessen Text gerade im Textfeld bearbeitet wird.
    editing_clip: Option<String>,
}

impl Default for GraphicsPanel {
    fn default() -> Self {
        GraphicsPanel {
            scroll: ScrollState::default(),
            text_area: TextArea::multiline(),
            hex_inputs: HashMap::new(),
            slider_gesture: false,
            text_gesture: false,
            last_focus: 0,
            editing_clip: None,
        }
    }
}

/// Erster ausgewählter Titel-Clip (ID + Spec-Kopie).
fn selected_title(app: &AppState) -> Option<(String, TitleSpec)> {
    app.timeline
        .selected_clip_ids
        .iter()
        .filter_map(|id| app.timeline.clip(id))
        .find_map(|c| c.title.clone().map(|t| (c.id.clone(), t)))
}

impl GraphicsPanel {
    /// Slider-Änderung übernehmen: ein Undo-Snapshot pro Drag-Geste.
    fn commit_live(&mut self, app: &mut AppState, clip_id: &str, f: impl FnOnce(&mut TitleSpec)) {
        if !self.slider_gesture {
            app.timeline.begin_title_edit();
            self.slider_gesture = true;
        }
        app.timeline.title_update_live(clip_id, f);
    }

    /// Text-/Hex-Änderung übernehmen: ein Snapshot pro Tipp-Sitzung.
    fn commit_typed(&mut self, app: &mut AppState, clip_id: &str, f: impl FnOnce(&mut TitleSpec)) {
        if !self.text_gesture {
            app.timeline.begin_title_edit();
            self.text_gesture = true;
        }
        app.timeline.title_update_live(clip_id, f);
    }

    /// Farbzeile: Muster + Hex-Feld; liefert die neue Farbe bei Änderung.
    #[allow(clippy::too_many_arguments)]
    fn color_row(
        &mut self,
        ui: &mut Ui,
        key: &'static str,
        rect: Rect,
        label: &str,
        color: RgbaColor,
    ) -> Option<RgbaColor> {
        let mut row = rect;
        let label_cell = row.cut_left(88.0);
        ui.text_left(label, label_cell, theme::TEXT_2, FontKind::Sans12);
        row.cut_left(8.0);
        let swatch = row.cut_left(34.0);
        let swatch = Rect::new(swatch.x, rect.y + 3.0, 34.0, rect.h - 6.0);
        ui.fill_rounded(
            swatch,
            theme::RADIUS_SM,
            raylib::color::Color::new(color.r, color.g, color.b, 255),
        );
        ui.stroke_rounded(swatch, theme::RADIUS_SM, 1.0, theme::LINE_STRONG);
        row.cut_left(8.0);

        let input = self.hex_inputs.entry(key).or_default();
        let id = ui.id(("graphics.hex", key));
        // Hex-Feld mit der Farbe synchronisieren, solange nicht getippt wird.
        if ui.persist.keyboard_focus != id && input.text != color.to_hex() {
            input.set_text(color.to_hex());
        }
        let field = Rect::new(row.x, rect.y + 1.0, row.w.min(96.0), rect.h - 2.0);
        let result = input.show(ui, ("graphics.hex", key), field, "#rrggbb");
        if result.changed {
            if let Some(c) = RgbaColor::from_hex(&input.text) {
                return Some(c);
            }
        }
        None
    }
}

/// Abschnittsüberschrift.
fn section(ui: &mut Ui, x: f32, y: f32, w: f32, label: &str) {
    ui.text(label, v2(x, y), theme::TEXT_3, FontKind::Sans12Medium);
    let lw = ui.font(FontKind::Sans12Medium).width(label);
    ui.hline(x + lw + 8.0, y + 8.0, (w - lw - 8.0).max(0.0), theme::LINE);
}

/// Schalter-Zeile (Checkbox-Ersatz): liefert true bei Klick.
fn toggle_row(ui: &mut Ui, id_src: impl std::hash::Hash, rect: Rect, label: &str, on: bool) -> bool {
    let id = ui.id(id_src);
    let it = ui.interact(id, rect);
    let boxr = Rect::new(rect.x, rect.y + (rect.h - 16.0) / 2.0, 16.0, 16.0);
    ui.fill_rounded(
        boxr,
        theme::RADIUS_XS,
        if on { theme::ACCENT } else { theme::SURFACE_2 },
    );
    ui.stroke_rounded(boxr, theme::RADIUS_XS, 1.0, if on { theme::ACCENT } else { theme::LINE_STRONG });
    if on {
        ui.icon("check", boxr, 12.0, theme::WHITE);
    }
    let mut text_cell = rect;
    text_cell.cut_left(24.0);
    ui.text_left(label, text_cell, theme::TEXT_1, FontKind::Sans12);
    if it.hovered {
        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
    }
    it.clicked
}

impl Panel for GraphicsPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);
        let mut area = rect;

        // Gesten-Lebenszyklus: Slider enden mit der Maustaste, Tipp-
        // Sitzungen mit dem Fokuswechsel.
        if !ui.input.left_down {
            self.slider_gesture = false;
        }
        if ui.persist.keyboard_focus != self.last_focus {
            self.last_focus = ui.persist.keyboard_focus;
            self.text_gesture = false;
        }

        // ---- Kopfzeile: Titel hinzufügen + Vorlagen ----
        let bar = area.cut_top(36.0);
        ui.hline(bar.x, bar.bottom() - 1.0, bar.w, theme::LINE);
        let mut bx = bar.x + 8.0;
        let add = TextButton::new("Titel hinzufügen")
            .icon("plus")
            .style(TextButtonStyle::Solid);
        let aw = add.measure(ui);
        if add
            .show(ui, "graphics.add", Rect::new(bx, bar.y + 6.0, aw, 24.0))
            .clicked
        {
            ui.run_command("title.add");
        }
        bx += aw + 8.0;
        for template in [
            TitleTemplate::LowerThird,
            TitleTemplate::Centered,
            TitleTemplate::CreditRoll,
        ] {
            let btn = TextButton::new(template.label()).style(TextButtonStyle::Outline);
            let bw = btn.measure(ui);
            if bx + bw > bar.right() - 8.0 {
                break;
            }
            if btn
                .show(
                    ui,
                    ("graphics.template", template.key()),
                    Rect::new(bx, bar.y + 6.0, bw, 24.0),
                )
                .clicked
            {
                ui.run_command_with(
                    "title.add",
                    serde_json::json!({ "template": template.key() }),
                );
            }
            bx += bw + 6.0;
        }

        // ---- Titel-Liste der Sequenz ----
        let titles: Vec<(String, String, f64, f64)> = app
            .timeline
            .clips
            .iter()
            .filter(|c| c.is_title())
            .map(|c| (c.id.clone(), c.name.clone(), c.start, c.duration))
            .collect();
        if !titles.is_empty() {
            let list_h = (titles.len() as f32 * 24.0 + 8.0).min(128.0);
            let list = area.cut_top(list_h);
            ui.hline(list.x, list.bottom() - 1.0, list.w, theme::LINE);
            let mut y = list.y + 4.0;
            for (id, name, start, duration) in &titles {
                if y + 24.0 > list.bottom() {
                    break;
                }
                let row = Rect::new(list.x, y, list.w, 24.0);
                let selected = app.timeline.selected_clip_ids.contains(id);
                let row_id = ui.id(("graphics.title", id.as_str()));
                let it = ui.interact(row_id, row);
                if selected {
                    ui.fill(row, theme::ACCENT_SOFT);
                } else if it.hovered {
                    ui.fill(row, theme::SURFACE_2);
                }
                if it.hovered {
                    ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                }
                let mut inner = row.inset_xy(10.0, 0.0);
                let ic = inner.cut_left(14.0);
                ui.icon("type", ic, 13.0, theme::GRAPHIC);
                inner.cut_left(8.0);
                let tc = crate::core::timecode::format_sequence_timecode(
                    *start,
                    &app.timeline.settings,
                );
                let tw = ui.font(FontKind::Mono12).width(&tc);
                let tc_cell = inner.cut_right(tw);
                ui.text_left(&tc, tc_cell, theme::TEXT_3, FontKind::Mono12);
                inner.cut_right(8.0);
                let label = ui.font(FontKind::Sans12).ellipsize(name, inner.w);
                ui.text_left(&label, inner, theme::TEXT_1, FontKind::Sans12);
                if it.clicked {
                    app.timeline
                        .select_clips(&[id.clone()], SelectMode::Replace, false);
                    // Playhead in den Clip, damit Monitor + Inspektor ihn zeigen.
                    let t = app.timeline.playhead_sec;
                    if t < *start || t >= start + duration {
                        app.timeline.set_playhead(start + duration * 0.5);
                    }
                }
                y += 24.0;
            }
        }

        // ---- Inspektor ----
        let Some((clip_id, spec)) = selected_title(app) else {
            self.editing_clip = None;
            ui.text_centered(
                "Titel-Clip auswählen oder „Titel hinzufügen“ (Mod+T).\nDoppelklick auf den Titel im Programmmonitor\nbearbeitet den Text direkt im Bild.",
                area,
                theme::TEXT_3,
                FontKind::Sans12,
            );
            return;
        };
        if self.editing_clip.as_deref() != Some(clip_id.as_str()) {
            self.editing_clip = Some(clip_id.clone());
            self.text_area.caret = spec.text.len();
            self.text_gesture = false;
        }

        let pad = 10.0;
        let text_lines = spec.text.split('\n').count().clamp(2, 8) as f32;
        let text_h = text_lines * TEXTAREA_LINE_H + 12.0;
        // Inhaltshöhe: Text + Schrift (4) + Layout (4) + Füllung/Kontur (3)
        // + Schatten (6) + Box (6) + Überschriften.
        let content_h = 18.0
            + text_h
            + 8.0
            + (SECTION_GAP + 18.0) * 5.0
            + (ROW_H + GAP) * 23.0
            + 24.0;
        let view = self.scroll.begin(ui, area, 0.0, content_h);
        let x = view.viewport.x + pad;
        let w = view.viewport.w - 2.0 * pad;
        let mut y = view.origin_y + 8.0;

        // ---------- Text ----------
        section(ui, x, y, w, "Text");
        y += 18.0;
        let text_rect = Rect::new(x, y, w, text_h);
        let mut text = spec.text.clone();
        if self.text_area.show(ui, "graphics.text", text_rect, &mut text).changed {
            let new_text = text.clone();
            self.commit_typed(app, &clip_id, move |s| s.text = new_text);
        }
        y += text_h + 8.0;

        // Aktuellen Spec nachladen (Text kann sich geändert haben).
        let spec = app
            .timeline
            .clip(&clip_id)
            .and_then(|c| c.title.clone())
            .unwrap_or(spec);

        // ---------- Schrift ----------
        y += SECTION_GAP;
        section(ui, x, y, w, "Schrift");
        y += 18.0;

        // Familie
        let families = list_font_families();
        let mut row = Rect::new(x, y, w, ROW_H);
        let label_cell = row.cut_left(88.0);
        ui.text_left("Schriftfamilie", label_cell, theme::TEXT_2, FontKind::Sans12);
        row.cut_left(8.0);
        let mut options: Vec<&str> = vec!["Standard (Sans)"];
        options.extend(families.iter().map(|f| f.as_str()));
        let sel_idx = if spec.font_family.is_empty() {
            0
        } else {
            families
                .iter()
                .position(|f| *f == spec.font_family)
                .map(|i| i + 1)
                .unwrap_or(0)
        };
        let family_rect = Rect::new(row.x, y + 1.0, row.w, ROW_H - 2.0);
        if let Some(idx) = select(ui, "graphics.family", family_rect, &options, sel_idx) {
            let family = if idx == 0 {
                String::new()
            } else {
                families[idx - 1].clone()
            };
            app.timeline.title_update(&clip_id, move |s| s.font_family = family);
        }
        y += ROW_H + GAP;

        // Schnitt + Kursiv
        let mut row = Rect::new(x, y, w, ROW_H);
        let label_cell = row.cut_left(88.0);
        ui.text_left("Schriftschnitt", label_cell, theme::TEXT_2, FontKind::Sans12);
        row.cut_left(8.0);
        let weights: Vec<&str> = TitleWeight::ALL.iter().map(|w| w.label()).collect();
        let weight_idx = TitleWeight::ALL
            .iter()
            .position(|v| *v == spec.weight)
            .unwrap_or(0);
        let weight_rect = Rect::new(row.x, y + 1.0, (row.w - 86.0).max(60.0), ROW_H - 2.0);
        if let Some(idx) = select(ui, "graphics.weight", weight_rect, &weights, weight_idx) {
            let value = TitleWeight::ALL[idx];
            app.timeline.title_update(&clip_id, move |s| s.weight = value);
        }
        let italic_rect = Rect::new(weight_rect.right() + 8.0, y, 78.0, ROW_H);
        if toggle_row(ui, "graphics.italic", italic_rect, "Kursiv", spec.italic) {
            let value = !spec.italic;
            app.timeline.title_update(&clip_id, move |s| s.italic = value);
        }
        y += ROW_H + GAP;

        // Größe / Zeilenabstand / Laufweite
        let mut size = spec.size;
        slider_row(ui, "graphics.size", Rect::new(x, y, w, ROW_H), "Schriftgröße", &mut size, 8.0, 300.0, 64.0);
        if size != spec.size {
            self.commit_live(app, &clip_id, move |s| s.size = size);
        }
        y += ROW_H + GAP;
        let mut line_pct = (spec.line_spacing * 100.0).round();
        slider_row(ui, "graphics.lineSpacing", Rect::new(x, y, w, ROW_H), "Zeilenabstand", &mut line_pct, 80.0, 250.0, 115.0);
        if (line_pct / 100.0 - spec.line_spacing).abs() > 1e-9 {
            self.commit_live(app, &clip_id, move |s| s.line_spacing = line_pct / 100.0);
        }
        y += ROW_H + GAP;
        let mut tracking = spec.letter_spacing;
        slider_row(ui, "graphics.tracking", Rect::new(x, y, w, ROW_H), "Laufweite", &mut tracking, -5.0, 30.0, 0.0);
        if tracking != spec.letter_spacing {
            self.commit_live(app, &clip_id, move |s| s.letter_spacing = tracking);
        }
        y += ROW_H + GAP;

        // ---------- Layout ----------
        y += SECTION_GAP;
        section(ui, x, y, w, "Ausrichtung & Position");
        y += 18.0;

        let mut row = Rect::new(x, y, w, ROW_H);
        let label_cell = row.cut_left(88.0);
        ui.text_left("Ausrichtung", label_cell, theme::TEXT_2, FontKind::Sans12);
        row.cut_left(8.0);
        let aligns: Vec<&str> = TitleAlign::ALL.iter().map(|a| a.label()).collect();
        let align_idx = TitleAlign::ALL
            .iter()
            .position(|a| *a == spec.align)
            .unwrap_or(1);
        if let Some(idx) = select(ui, "graphics.align", Rect::new(row.x, y + 1.0, row.w, ROW_H - 2.0), &aligns, align_idx) {
            let value = TitleAlign::ALL[idx];
            app.timeline.title_update(&clip_id, move |s| s.align = value);
        }
        y += ROW_H + GAP;

        let mut row = Rect::new(x, y, w, ROW_H);
        let label_cell = row.cut_left(88.0);
        ui.text_left("Position", label_cell, theme::TEXT_2, FontKind::Sans12);
        row.cut_left(8.0);
        let pos_labels: Vec<&str> = POS_PRESETS.iter().map(|(l, _)| *l).collect();
        let pos_idx = POS_PRESETS
            .iter()
            .position(|(_, v)| v.is_some_and(|v| (v - spec.pos_y).abs() < 0.5))
            .unwrap_or(0);
        if let Some(idx) = select(ui, "graphics.posPreset", Rect::new(row.x, y + 1.0, row.w, ROW_H - 2.0), &pos_labels, pos_idx) {
            if let Some(target) = POS_PRESETS[idx].1 {
                app.timeline.title_update(&clip_id, move |s| s.pos_y = target);
            }
        }
        y += ROW_H + GAP;

        let mut pos_x = spec.pos_x;
        slider_row(ui, "graphics.posX", Rect::new(x, y, w, ROW_H), "Position X", &mut pos_x, -50.0, 50.0, 0.0);
        if pos_x != spec.pos_x {
            self.commit_live(app, &clip_id, move |s| s.pos_x = pos_x);
        }
        y += ROW_H + GAP;
        let mut pos_y = spec.pos_y;
        slider_row(ui, "graphics.posY", Rect::new(x, y, w, ROW_H), "Position Y", &mut pos_y, -50.0, 50.0, 0.0);
        if pos_y != spec.pos_y {
            self.commit_live(app, &clip_id, move |s| s.pos_y = pos_y);
        }
        y += ROW_H + GAP;

        // ---------- Füllung & Kontur ----------
        y += SECTION_GAP;
        section(ui, x, y, w, "Füllung & Kontur");
        y += 18.0;

        if let Some(c) = self.color_row(ui, "fill", Rect::new(x, y, w, ROW_H), "Füllfarbe", spec.fill) {
            self.commit_typed(app, &clip_id, move |s| s.fill = c);
        }
        y += ROW_H + GAP;
        let mut stroke_w = spec.stroke_width;
        slider_row(ui, "graphics.strokeW", Rect::new(x, y, w, ROW_H), "Konturbreite", &mut stroke_w, 0.0, 40.0, 0.0);
        if stroke_w != spec.stroke_width {
            self.commit_live(app, &clip_id, move |s| s.stroke_width = stroke_w);
        }
        y += ROW_H + GAP;
        if let Some(c) = self.color_row(ui, "stroke", Rect::new(x, y, w, ROW_H), "Konturfarbe", spec.stroke_color) {
            self.commit_typed(app, &clip_id, move |s| s.stroke_color = c);
        }
        y += ROW_H + GAP;

        // ---------- Schatten ----------
        y += SECTION_GAP;
        section(ui, x, y, w, "Schlagschatten");
        y += 18.0;
        if toggle_row(ui, "graphics.shadowOn", Rect::new(x, y, w, ROW_H), "Schatten aktiv", spec.shadow.enabled) {
            let value = !spec.shadow.enabled;
            app.timeline.title_update(&clip_id, move |s| s.shadow.enabled = value);
        }
        y += ROW_H + GAP;
        let mut sdx = spec.shadow.dx;
        slider_row(ui, "graphics.shadowDx", Rect::new(x, y, w, ROW_H), "Versatz X", &mut sdx, -50.0, 50.0, 0.0);
        if sdx != spec.shadow.dx {
            self.commit_live(app, &clip_id, move |s| s.shadow.dx = sdx);
        }
        y += ROW_H + GAP;
        let mut sdy = spec.shadow.dy;
        slider_row(ui, "graphics.shadowDy", Rect::new(x, y, w, ROW_H), "Versatz Y", &mut sdy, -50.0, 50.0, 3.0);
        if sdy != spec.shadow.dy {
            self.commit_live(app, &clip_id, move |s| s.shadow.dy = sdy);
        }
        y += ROW_H + GAP;
        let mut sblur = spec.shadow.blur;
        slider_row(ui, "graphics.shadowBlur", Rect::new(x, y, w, ROW_H), "Weichheit", &mut sblur, 0.0, 60.0, 10.0);
        if sblur != spec.shadow.blur {
            self.commit_live(app, &clip_id, move |s| s.shadow.blur = sblur);
        }
        y += ROW_H + GAP;
        let mut sop = spec.shadow.opacity;
        slider_row(ui, "graphics.shadowOpacity", Rect::new(x, y, w, ROW_H), "Deckkraft", &mut sop, 0.0, 100.0, 70.0);
        if sop != spec.shadow.opacity {
            self.commit_live(app, &clip_id, move |s| s.shadow.opacity = sop);
        }
        y += ROW_H + GAP;
        if let Some(c) = self.color_row(ui, "shadow", Rect::new(x, y, w, ROW_H), "Schattenfarbe", spec.shadow.color) {
            self.commit_typed(app, &clip_id, move |s| s.shadow.color = c);
        }
        y += ROW_H + GAP;

        // ---------- Hintergrund-Box ----------
        y += SECTION_GAP;
        section(ui, x, y, w, "Hintergrund-Box");
        y += 18.0;
        if toggle_row(ui, "graphics.boxOn", Rect::new(x, y, w, ROW_H), "Box aktiv", spec.bg.enabled) {
            let value = !spec.bg.enabled;
            app.timeline.title_update(&clip_id, move |s| s.bg.enabled = value);
        }
        y += ROW_H + GAP;
        if let Some(c) = self.color_row(ui, "box", Rect::new(x, y, w, ROW_H), "Boxfarbe", spec.bg.color) {
            self.commit_typed(app, &clip_id, move |s| s.bg.color = c);
        }
        y += ROW_H + GAP;
        let mut bop = spec.bg.opacity;
        slider_row(ui, "graphics.boxOpacity", Rect::new(x, y, w, ROW_H), "Deckkraft", &mut bop, 0.0, 100.0, 70.0);
        if bop != spec.bg.opacity {
            self.commit_live(app, &clip_id, move |s| s.bg.opacity = bop);
        }
        y += ROW_H + GAP;
        let mut bpx = spec.bg.pad_x;
        slider_row(ui, "graphics.boxPadX", Rect::new(x, y, w, ROW_H), "Polster X", &mut bpx, 0.0, 120.0, 22.0);
        if bpx != spec.bg.pad_x {
            self.commit_live(app, &clip_id, move |s| s.bg.pad_x = bpx);
        }
        y += ROW_H + GAP;
        let mut bpy = spec.bg.pad_y;
        slider_row(ui, "graphics.boxPadY", Rect::new(x, y, w, ROW_H), "Polster Y", &mut bpy, 0.0, 120.0, 12.0);
        if bpy != spec.bg.pad_y {
            self.commit_live(app, &clip_id, move |s| s.bg.pad_y = bpy);
        }
        y += ROW_H + GAP;
        let mut brad = spec.bg.radius;
        slider_row(ui, "graphics.boxRadius", Rect::new(x, y, w, ROW_H), "Rundung", &mut brad, 0.0, 60.0, 6.0);
        if brad != spec.bg.radius {
            self.commit_live(app, &clip_id, move |s| s.bg.radius = brad);
        }

        self.scroll.end(ui, area, 0.0, content_h);
    }
}
