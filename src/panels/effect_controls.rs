//! Effekteinstellungen: Abschnitte (Bewegung, Deckkraft, Zeitneuzuordnung)
//! mit Parameter-Zeilen (Keyframe-Platzhalter, Slider, Wert).

use crate::panels::color::section_header;
use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::select::select;
use crate::ui::widgets::{slider, IconButton};
use crate::ui::{FontKind, Ui};

const BLEND_MODES: [&str; 6] = [
    "Normal",
    "Abdunkeln",
    "Multiplizieren",
    "Aufhellen",
    "Negativ multiplizieren",
    "Ineinanderkopieren",
];

pub struct EffectControlsPanel {
    /// Asset, zu dem die aktuellen Werte gehören (frischer State pro Clip).
    asset_id: Option<String>,
    pos_x: f64,
    pos_y: f64,
    scale: f64,
    rotation: f64,
    opacity: f64,
    blend_mode: usize,
    speed: f64,
    open_motion: bool,
    open_opacity: bool,
    open_time: bool,
    scroll: ScrollState,
}

impl Default for EffectControlsPanel {
    fn default() -> Self {
        EffectControlsPanel {
            asset_id: None,
            pos_x: 0.0,
            pos_y: 0.0,
            scale: 100.0,
            rotation: 0.0,
            opacity: 100.0,
            blend_mode: 0,
            speed: 100.0,
            open_motion: true,
            open_opacity: true,
            open_time: true,
            scroll: ScrollState::default(),
        }
    }
}

impl EffectControlsPanel {
    fn reset_motion(&mut self) {
        self.pos_x = 0.0;
        self.pos_y = 0.0;
        self.scale = 100.0;
        self.rotation = 0.0;
    }

    fn reset_opacity(&mut self) {
        self.opacity = 100.0;
        self.blend_mode = 0;
    }

    fn reset_time(&mut self) {
        self.speed = 100.0;
    }
}

/// Parameter-Zeile: Keyframe-Platzhalter + Label + Slider + Wert (+ Einheit).
#[allow(clippy::too_many_arguments)]
fn param_row(
    ui: &mut Ui,
    id_src: impl std::hash::Hash,
    rect: Rect,
    label: &str,
    value: &mut f64,
    min: f64,
    max: f64,
    unit: &str,
) {
    let mut inner = rect;
    let kf = inner.cut_left(18.0);
    ui.icon("timer", kf, 14.0, theme::with_alpha(theme::TEXT_3, 128));
    inner.cut_left(4.0);
    let label_cell = inner.cut_left(96.0);
    let display = ui.font(FontKind::Sans12).ellipsize(label, label_cell.w);
    ui.text_left(&display, label_cell, theme::TEXT_2, FontKind::Sans12);
    inner.cut_left(8.0);
    let unit_w = if unit.is_empty() { 0.0 } else { 14.0 };
    let unit_cell = inner.cut_right(unit_w);
    let value_cell = inner.cut_right(56.0);
    inner.cut_right(6.0);
    slider(ui, (&id_src, "slider"), inner, value, min, max, theme::ACCENT);
    *value = value.round();
    // Wertefeld (Optik des number-Inputs)
    ui.fill_rounded(
        Rect::new(value_cell.x, rect.y + 2.0, value_cell.w, 20.0),
        theme::RADIUS_SM,
        theme::SURFACE_3,
    );
    ui.stroke_rounded(
        Rect::new(value_cell.x, rect.y + 2.0, value_cell.w, 20.0),
        theme::RADIUS_SM,
        1.0,
        theme::LINE,
    );
    ui.text_right(
        &format!("{}", *value as i64),
        value_cell.inset_xy(4.0, 0.0),
        theme::TEXT_1,
        FontKind::Mono12,
    );
    if !unit.is_empty() {
        ui.text_right(unit, unit_cell, theme::TEXT_3, FontKind::Sans12);
    }
}

impl Panel for EffectControlsPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);

        let asset = app
            .media
            .selected_asset_ids
            .first()
            .and_then(|id| app.media.asset(id))
            .or_else(|| {
                app.playback
                    .source_asset_id
                    .as_ref()
                    .and_then(|id| app.media.asset(id))
            })
            .cloned();

        let Some(asset) = asset else {
            ui.text_centered("Clip auswählen", rect, theme::TEXT_3, FontKind::Sans12);
            return;
        };
        // Frischer State pro Clip (key={asset.id}-Pendant)
        if self.asset_id.as_deref() != Some(asset.id.as_str()) {
            *self = EffectControlsPanel {
                asset_id: Some(asset.id.clone()),
                ..Default::default()
            };
        }

        let mut area = rect;
        // Kopf: Clipname
        let head = area.cut_top(36.0);
        ui.hline(head.x, head.bottom() - 1.0, head.w, theme::LINE);
        let name = ui
            .font(FontKind::Sans12Medium)
            .ellipsize(&asset.name, head.w - 24.0);
        ui.text_left(&name, head.inset_xy(12.0, 0.0), theme::TEXT_1, FontKind::Sans12Medium);

        // Fuß
        let footer = area.cut_bottom(29.0);
        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        let note = ui.font(FontKind::Sans12).ellipsize(
            "Werte sind eine Vorschau — die Render-Verknüpfung folgt.",
            footer.w - 24.0,
        );
        ui.text_left(&note, footer.inset_xy(12.0, 0.0), theme::TEXT_3, FontKind::Sans12);

        let motion_h = if self.open_motion { 4.0 * 26.0 + 10.0 } else { 0.0 };
        let opacity_h = if self.open_opacity { 2.0 * 26.0 + 10.0 } else { 0.0 };
        let time_h = if self.open_time { 26.0 + 10.0 } else { 0.0 };
        let content_h = 3.0 * 32.0 + motion_h + opacity_h + time_h;

        let view = self.scroll.begin(ui, area, 0.0, content_h);
        let x = view.viewport.x;
        let w = view.viewport.w;
        let mut y = view.origin_y;

        // ---- Bewegung ----
        let header = Rect::new(x, y, w, 32.0);
        section_header(ui, "fx.sec.motion", header, "Bewegung", &mut self.open_motion);
        let reset = Rect::new(x + w - 30.0, y + 5.0, 22.0, 22.0);
        if IconButton::new("rotate-ccw")
            .size(14.0)
            .tooltip("Abschnitt zurücksetzen")
            .show(ui, "fx.reset.motion", reset)
            .clicked
        {
            self.reset_motion();
        }
        y += 32.0;
        if self.open_motion {
            let r = Rect::new(x + 8.0, y, w - 16.0, 24.0);
            param_row(ui, "fx.posx", r, "Position X", &mut self.pos_x, -1000.0, 1000.0, "");
            y += 26.0;
            let r = Rect::new(x + 8.0, y, w - 16.0, 24.0);
            param_row(ui, "fx.posy", r, "Position Y", &mut self.pos_y, -1000.0, 1000.0, "");
            y += 26.0;
            let r = Rect::new(x + 8.0, y, w - 16.0, 24.0);
            param_row(ui, "fx.scale", r, "Skalierung", &mut self.scale, 0.0, 400.0, "%");
            y += 26.0;
            let r = Rect::new(x + 8.0, y, w - 16.0, 24.0);
            param_row(ui, "fx.rot", r, "Rotation", &mut self.rotation, -180.0, 180.0, "°");
            y += 26.0 + 10.0;
        }
        ui.hline(x, y - 1.0, w, theme::LINE);

        // ---- Deckkraft ----
        let header = Rect::new(x, y, w, 32.0);
        section_header(ui, "fx.sec.opacity", header, "Deckkraft", &mut self.open_opacity);
        let reset = Rect::new(x + w - 30.0, y + 5.0, 22.0, 22.0);
        if IconButton::new("rotate-ccw")
            .size(14.0)
            .tooltip("Abschnitt zurücksetzen")
            .show(ui, "fx.reset.opacity", reset)
            .clicked
        {
            self.reset_opacity();
        }
        y += 32.0;
        if self.open_opacity {
            let r = Rect::new(x + 8.0, y, w - 16.0, 24.0);
            param_row(ui, "fx.opacity", r, "Deckkraft", &mut self.opacity, 0.0, 100.0, "%");
            y += 26.0;
            // Mischmodus (pl-6)
            let mut r = Rect::new(x + 8.0 + 22.0, y, w - 16.0 - 22.0, 24.0);
            let label_cell = r.cut_left(96.0);
            ui.text_left("Mischmodus", label_cell, theme::TEXT_2, FontKind::Sans12);
            r.cut_left(8.0);
            if let Some(m) = select(ui, "fx.blend", r, &BLEND_MODES, self.blend_mode) {
                self.blend_mode = m;
            }
            y += 26.0 + 10.0;
        }
        ui.hline(x, y - 1.0, w, theme::LINE);

        // ---- Zeitneuzuordnung ----
        let header = Rect::new(x, y, w, 32.0);
        section_header(ui, "fx.sec.time", header, "Zeitneuzuordnung", &mut self.open_time);
        let reset = Rect::new(x + w - 30.0, y + 5.0, 22.0, 22.0);
        if IconButton::new("rotate-ccw")
            .size(14.0)
            .tooltip("Abschnitt zurücksetzen")
            .show(ui, "fx.reset.time", reset)
            .clicked
        {
            self.reset_time();
        }
        y += 32.0;
        if self.open_time {
            let r = Rect::new(x + 8.0, y, w - 16.0, 24.0);
            param_row(ui, "fx.speed", r, "Geschwindigkeit", &mut self.speed, 10.0, 400.0, "%");
        }

        self.scroll.end(ui, area, 0.0, content_h);
    }
}
