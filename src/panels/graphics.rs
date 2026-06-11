//! Grafiken-Panel: Titel-Ebenen (Liste mit Sichtbarkeit/Löschen) und
//! Eigenschaften (Text, Schriftgröße, Farbe, Position).

use crate::core::types::new_id;
use crate::panels::color::slider_row;
use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::select::select;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::widgets::{TextButton, TextButtonStyle};
use crate::ui::{FontKind, Ui};
use raylib::consts::MouseCursor;

const POSITIONS: [&str; 3] = ["Unteres Drittel", "Mitte", "Oberes Drittel"];

struct TitleLayer {
    id: String,
    name: String,
    text: String,
    font_size: f64,
    color: String,
    position: usize,
    visible: bool,
}

pub struct GraphicsPanel {
    layers: Vec<TitleLayer>,
    selected_id: Option<String>,
    counter: usize,
    text_input: TextInputState,
    /// Layer-ID, deren Text gerade im Eingabefeld steht.
    editing_layer: Option<String>,
    list_scroll: ScrollState,
}

impl Default for GraphicsPanel {
    fn default() -> Self {
        GraphicsPanel {
            layers: Vec::new(),
            selected_id: None,
            counter: 0,
            text_input: TextInputState::default(),
            editing_layer: None,
            list_scroll: ScrollState::default(),
        }
    }
}

impl Panel for GraphicsPanel {
    fn update(&mut self, ui: &mut Ui, _app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);
        let mut area = rect;

        // Kopfzeile: „Text hinzufügen“
        let bar = area.cut_top(36.0);
        ui.hline(bar.x, bar.bottom() - 1.0, bar.w, theme::LINE);
        let btn = TextButton::new("Text hinzufügen")
            .icon("plus")
            .style(TextButtonStyle::Outline);
        let bw = btn.measure(ui);
        if btn
            .show(ui, "graphics.add", Rect::new(bar.x + 8.0, bar.y + 6.0, bw, 24.0))
            .clicked
        {
            self.counter += 1;
            let layer = TitleLayer {
                id: new_id(),
                name: format!("Titel {}", self.counter),
                text: "Neuer Titel".into(),
                font_size: 48.0,
                color: "#ffffff".into(),
                position: 0,
                visible: true,
            };
            self.selected_id = Some(layer.id.clone());
            self.layers.push(layer);
        }

        // Fußzeile
        let footer = area.cut_bottom(29.0);
        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        ui.text_left(
            "Titel-Rendering auf dem Programmmonitor folgt.",
            footer.inset_xy(12.0, 0.0),
            theme::TEXT_3,
            FontKind::Sans12,
        );

        // Ebenen-Liste (max-h-48 = 192px, border-b)
        let list_h = (self.layers.len() as f32 * 28.0 + 8.0).clamp(40.0, 192.0);
        let list = area.cut_top(list_h);
        ui.hline(list.x, list.bottom() - 1.0, list.w, theme::LINE);

        if self.layers.is_empty() {
            ui.text_centered(
                "Noch keine Ebenen.",
                list,
                theme::TEXT_3,
                FontKind::Sans12,
            );
        } else {
            let content_h = self.layers.len() as f32 * 28.0 + 8.0;
            let view = self.list_scroll.begin(ui, list, 0.0, content_h);
            let mut remove: Option<String> = None;
            let mut y = view.origin_y + 4.0;
            for layer in &mut self.layers {
                let row = Rect::new(view.viewport.x, y, view.viewport.w, 28.0);
                let selected = self.selected_id.as_deref() == Some(layer.id.as_str());
                let id = ui.id(("graphics.layer", layer.id.as_str()));
                let it = ui.interact(id, row);
                if selected {
                    ui.fill(row, theme::ACCENT_SOFT);
                } else if it.hovered {
                    ui.fill(row, theme::SURFACE_2);
                }
                if it.hovered {
                    ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                }
                let mut inner = row.inset_xy(8.0, 0.0);
                let ic = inner.cut_left(14.0);
                ui.icon("type", ic, 14.0, theme::TEXT_2);
                inner.cut_left(8.0);

                // Buttons rechts: Auge + Papierkorb
                let trash = inner.cut_right(22.0);
                let trash = Rect::new(trash.x, row.y + 3.0, 22.0, 22.0);
                let eye = inner.cut_right(22.0);
                let eye = Rect::new(eye.x, row.y + 3.0, 22.0, 22.0);

                let eye_id = ui.id(("graphics.eye", layer.id.as_str()));
                let eye_it = ui.interact(eye_id, eye);
                if eye_it.hovered {
                    ui.fill_rounded(eye, theme::RADIUS_SM, theme::SURFACE_3);
                }
                ui.icon(
                    if layer.visible { "eye" } else { "eye-off" },
                    eye,
                    14.0,
                    if eye_it.hovered { theme::TEXT_1 } else { theme::TEXT_3 },
                );
                if eye_it.clicked {
                    layer.visible = !layer.visible;
                }

                let trash_id = ui.id(("graphics.trash", layer.id.as_str()));
                let trash_it = ui.interact(trash_id, trash);
                if trash_it.hovered {
                    ui.fill_rounded(trash, theme::RADIUS_SM, theme::SURFACE_3);
                }
                ui.icon(
                    "trash-2",
                    trash,
                    14.0,
                    if trash_it.hovered { theme::DANGER } else { theme::TEXT_3 },
                );
                if trash_it.clicked {
                    remove = Some(layer.id.clone());
                }

                let name = ui.font(FontKind::Sans12).ellipsize(&layer.name, inner.w);
                ui.text_left(&name, inner, theme::TEXT_1, FontKind::Sans12);

                if it.clicked && !eye_it.clicked && !trash_it.clicked {
                    self.selected_id = Some(layer.id.clone());
                }
                y += 28.0;
            }
            self.list_scroll.end(ui, list, 0.0, content_h);
            if let Some(id) = remove {
                self.layers.retain(|l| l.id != id);
                if self.selected_id.as_deref() == Some(id.as_str()) {
                    self.selected_id = None;
                }
                if self.editing_layer.as_deref() == Some(id.as_str()) {
                    self.editing_layer = None;
                }
            }
        }

        // Eigenschaften
        let props = area.inset(8.0);
        let Some(selected_id) = self.selected_id.clone() else {
            ui.text_centered(
                "Ebene auswählen oder „Text hinzufügen“.",
                area,
                theme::TEXT_3,
                FontKind::Sans12,
            );
            return;
        };
        // Eingabefeld mit Layer-Text synchronisieren
        if self.editing_layer.as_deref() != Some(selected_id.as_str()) {
            let text = self
                .layers
                .iter()
                .find(|l| l.id == selected_id)
                .map(|l| l.text.clone())
                .unwrap_or_default();
            self.text_input.set_text(text);
            self.editing_layer = Some(selected_id.clone());
        }
        let Some(idx) = self.layers.iter().position(|l| l.id == selected_id) else {
            return;
        };

        let mut y = props.y;
        ui.text(
            "Text",
            crate::ui::geom::v2(props.x, y),
            theme::TEXT_2,
            FontKind::Sans12,
        );
        y += 18.0;
        let field = Rect::new(props.x, y, props.w, 24.0);
        let result = self.text_input.show(ui, "graphics.text", field, "");
        if result.changed {
            let text = self.text_input.text.clone();
            let layer = &mut self.layers[idx];
            layer.text = text.clone();
            layer.name = if text.trim().is_empty() {
                format!("Titel {}", self.counter)
            } else {
                text.lines().next().unwrap_or("").chars().take(24).collect()
            };
        }
        y += 32.0;

        let layer = &mut self.layers[idx];
        let row = Rect::new(props.x, y, props.w, 24.0);
        slider_row(
            ui,
            "graphics.fontsize",
            row,
            "Schriftgröße",
            &mut layer.font_size,
            12.0,
            200.0,
            48.0,
        );
        y += 30.0;

        // Farbe (Feld + Hex)
        let mut row = Rect::new(props.x, y, props.w, 24.0);
        let label_cell = row.cut_left(96.0);
        ui.text_left("Farbe", label_cell, theme::TEXT_2, FontKind::Sans12);
        let swatch = row.cut_left(40.0);
        let color = parse_hex(&layer.color);
        ui.fill_rounded(swatch, theme::RADIUS_SM, color);
        ui.stroke_rounded(swatch, theme::RADIUS_SM, 1.0, theme::LINE);
        row.cut_left(8.0);
        ui.text_left(&layer.color, row, theme::TEXT_3, FontKind::Mono12);
        let swatch_id = ui.id("graphics.color");
        let sw_it = ui.interact(swatch_id, swatch);
        if sw_it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            ui.tooltip(swatch_id, swatch, "Klick: Voreinstellungen durchschalten");
        }
        if sw_it.clicked {
            const PRESETS: [&str; 6] =
                ["#ffffff", "#e6e8ee", "#4f8dff", "#3ee08f", "#ffb547", "#ff5c5c"];
            let cur = PRESETS
                .iter()
                .position(|p| *p == layer.color)
                .map(|i| (i + 1) % PRESETS.len())
                .unwrap_or(0);
            layer.color = PRESETS[cur].to_string();
        }
        y += 30.0;

        // Position
        let mut row = Rect::new(props.x, y, props.w, 24.0);
        let label_cell = row.cut_left(96.0);
        ui.text_left("Position", label_cell, theme::TEXT_2, FontKind::Sans12);
        if let Some(p) = select(ui, "graphics.position", row, &POSITIONS, layer.position) {
            layer.position = p;
        }
    }
}

fn parse_hex(hex: &str) -> raylib::color::Color {
    let h = hex.trim_start_matches('#');
    if h.len() == 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&h[0..2], 16),
            u8::from_str_radix(&h[2..4], 16),
            u8::from_str_radix(&h[4..6], 16),
        ) {
            return raylib::color::Color::new(r, g, b, 255);
        }
    }
    theme::TEXT_1
}
