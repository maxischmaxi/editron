//! Farbe-Panel: Basiskorrektur-Slider, Kreativ-Looks, Farbräder (Platzhalter).
//! Die Werte überleben Workspace-Wechsel im Panel-State der PanelHost-Instanz.

use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::{v2, Rect};
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::select::select;
use crate::ui::widgets::{slider, TextButton, TextButtonStyle};
use crate::ui::{FontKind, Ui};
use raylib::consts::MouseCursor;
use raylib::prelude::RaylibDraw;

const LOOKS: [&str; 4] = ["Neutral", "Film warm", "Film kalt", "S/W"];

pub struct ColorPanel {
    temperature: f64,
    tint: f64,
    exposure: f64,
    contrast: f64,
    saturation: f64,
    look: usize,
    intensity: f64,
    open_basic: bool,
    open_creative: bool,
    open_wheels: bool,
    scroll: ScrollState,
}

impl Default for ColorPanel {
    fn default() -> Self {
        ColorPanel {
            temperature: 0.0,
            tint: 0.0,
            exposure: 0.0,
            contrast: 0.0,
            saturation: 100.0,
            look: 0,
            intensity: 100.0,
            open_basic: true,
            open_creative: true,
            open_wheels: true,
            scroll: ScrollState::default(),
        }
    }
}

/// Einklappbarer Abschnitt-Header (h-8); liefert true, wenn offen.
pub fn section_header(
    ui: &mut Ui,
    id_src: impl std::hash::Hash,
    rect: Rect,
    title: &str,
    open: &mut bool,
) {
    let id = ui.id(id_src);
    let it = ui.interact(id, rect);
    if it.hovered {
        ui.fill(rect, theme::SURFACE_2);
        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
    }
    let mut inner = rect.inset_xy(8.0, 0.0);
    let chev = inner.cut_left(14.0);
    ui.icon(
        if *open { "chevron-down" } else { "chevron-right" },
        chev,
        14.0,
        theme::TEXT_3,
    );
    inner.cut_left(6.0);
    ui.text_left(title, inner, theme::TEXT_1, FontKind::Sans12Medium);
    if it.clicked {
        *open = !*open;
    }
}

/// Slider-Zeile: Label (w-22, Doppelklick = Reset) + Range + Wert (mono).
pub fn slider_row(
    ui: &mut Ui,
    id_src: impl std::hash::Hash,
    rect: Rect,
    label: &str,
    value: &mut f64,
    min: f64,
    max: f64,
    default_value: f64,
) {
    let mut inner = rect;
    let label_cell = inner.cut_left(88.0);
    let label_id = ui.id((&id_src, "label"));
    let label_it = ui.interact(label_id, label_cell);
    if label_it.double_clicked {
        *value = default_value;
    }
    let display = ui.font(FontKind::Sans12).ellipsize(label, label_cell.w);
    ui.text_left(&display, label_cell, theme::TEXT_2, FontKind::Sans12);
    inner.cut_left(8.0);
    let value_cell = inner.cut_right(36.0);
    inner.cut_right(8.0);
    slider(ui, (&id_src, "slider"), inner, value, min, max, theme::ACCENT);
    *value = value.round();
    ui.text_right(
        &format!("{}", *value as i64),
        value_cell,
        theme::TEXT_1,
        FontKind::Mono12,
    );
}

impl Panel for ColorPanel {
    fn update(&mut self, ui: &mut Ui, _app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);
        let mut area = rect;

        // Fußzeile
        let footer = area.cut_bottom(29.0);
        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        let note = ui.font(FontKind::Sans12).ellipsize(
            "CSS-Filter-Vorschau auf dem Programmmonitor — FFmpeg-Render folgt.",
            footer.w - 24.0,
        );
        ui.text_left(&note, footer.inset_xy(12.0, 0.0), theme::TEXT_3, FontKind::Sans12);

        let basic_h = if self.open_basic { 6.0 * 26.0 + 30.0 } else { 0.0 };
        let creative_h = if self.open_creative { 2.0 * 26.0 + 10.0 } else { 0.0 };
        let wheels_h = if self.open_wheels { 100.0 } else { 0.0 };
        let content_h = 32.0 * 3.0 + basic_h + creative_h + wheels_h + 8.0;

        let view = self.scroll.begin(ui, area, 0.0, content_h);
        let w = view.viewport.w;
        let x = view.viewport.x;
        let mut y = view.origin_y;

        // ---- Basiskorrektur ----
        section_header(
            ui,
            "color.sec.basic",
            Rect::new(x, y, w, 32.0),
            "Basiskorrektur",
            &mut self.open_basic,
        );
        y += 32.0;
        if self.open_basic {
            let row = |_ui: &mut Ui, y: &mut f32| {
                let r = Rect::new(x + 8.0, *y, w - 16.0, 24.0);
                *y += 26.0;
                r
            };
            let r = row(ui, &mut y);
            slider_row(ui, ("color.s", "temp"), r, "Temperatur", &mut self.temperature, -100.0, 100.0, 0.0);
            let r = row(ui, &mut y);
            slider_row(ui, ("color.s", "tint"), r, "Farbton", &mut self.tint, -100.0, 100.0, 0.0);
            let r = row(ui, &mut y);
            slider_row(ui, ("color.s", "expo"), r, "Belichtung", &mut self.exposure, -100.0, 100.0, 0.0);
            let r = row(ui, &mut y);
            slider_row(ui, ("color.s", "contrast"), r, "Kontrast", &mut self.contrast, -100.0, 100.0, 0.0);
            let r = row(ui, &mut y);
            slider_row(ui, ("color.s", "sat"), r, "Sättigung", &mut self.saturation, 0.0, 200.0, 100.0);
            // Zurücksetzen-Button rechtsbündig
            let btn = TextButton::new("Zurücksetzen")
                .icon("rotate-ccw")
                .style(TextButtonStyle::Outline);
            let bw = btn.measure(ui);
            let brect = Rect::new(x + w - 8.0 - bw, y + 4.0, bw, 24.0);
            if btn.show(ui, "color.reset", brect).clicked {
                self.temperature = 0.0;
                self.tint = 0.0;
                self.exposure = 0.0;
                self.contrast = 0.0;
                self.saturation = 100.0;
                self.look = 0;
                self.intensity = 100.0;
            }
            y += 30.0;
        }
        ui.hline(x, y - 1.0, w, theme::LINE);

        // ---- Kreativ ----
        section_header(
            ui,
            "color.sec.creative",
            Rect::new(x, y, w, 32.0),
            "Kreativ",
            &mut self.open_creative,
        );
        y += 32.0;
        if self.open_creative {
            let row = Rect::new(x + 8.0, y, w - 16.0, 24.0);
            let mut inner = row;
            let label_cell = inner.cut_left(88.0);
            ui.text_left("Look", label_cell, theme::TEXT_2, FontKind::Sans12);
            inner.cut_left(8.0);
            if let Some(idx) = select(ui, "color.look", inner, &LOOKS, self.look) {
                self.look = idx;
            }
            y += 26.0;
            let row = Rect::new(x + 8.0, y, w - 16.0, 24.0);
            slider_row(
                ui,
                "color.intensity",
                row,
                "Intensität",
                &mut self.intensity,
                0.0,
                100.0,
                100.0,
            );
            y += 26.0 + 10.0;
        }
        ui.hline(x, y - 1.0, w, theme::LINE);

        // ---- Farbräder ----
        section_header(
            ui,
            "color.sec.wheels",
            Rect::new(x, y, w, 32.0),
            "Farbräder",
            &mut self.open_wheels,
        );
        y += 32.0;
        if self.open_wheels {
            let labels = ["Schatten", "Mitteltöne", "Lichter"];
            let cell_w = w / 3.0;
            for (i, label) in labels.iter().enumerate() {
                let cx = x + cell_w * (i as f32 + 0.5);
                let cy = y + 38.0;
                // Vereinfachtes Farbrad: Farbring + dunkler Kern + Marker
                let segments = 24;
                for s in 0..segments {
                    let a0 = s as f32 / segments as f32 * std::f32::consts::TAU;
                    let hue = s as f32 / segments as f32 * 360.0;
                    let color = raylib::color::Color::color_from_hsv(hue, 0.8, 0.9);
                    ui.d.draw_circle_sector(
                        v2(cx, cy),
                        32.0,
                        a0.to_degrees(),
                        (a0 + std::f32::consts::TAU / segments as f32).to_degrees(),
                        4,
                        raylib::color::Color::new(color.r, color.g, color.b, 204),
                    );
                }
                ui.d.draw_circle_v(v2(cx, cy), 22.0, theme::SURFACE_3);
                ui.d.draw_circle_v(v2(cx, cy), 3.0, theme::TEXT_1);
                ui.text_centered(
                    label,
                    Rect::new(cx - cell_w / 2.0, y + 74.0, cell_w, 16.0),
                    theme::TEXT_3,
                    FontKind::Sans12,
                );
            }
        }

        self.scroll.end(ui, area, 0.0, content_h);
    }
}
