//! Wiederverwendbare Widgets in der Editron-Optik (Theme-Tokens).

pub mod scroll;
pub mod select;
pub mod text_area;
pub mod text_input;

use super::geom::Rect;
use super::{FontKind, Interaction, Ui};
use crate::theme;
use raylib::color::Color;
use raylib::consts::MouseCursor;

/// Icon-Button (size-7 Standard): rounded-sm, hover:bg-surface-2,
/// text-text-2 → hover:text-text-1; `active` = bg-surface-3 text-accent.
pub struct IconButton<'a> {
    pub icon: &'a str,
    pub icon_size: f32,
    pub radius: f32,
    pub active: bool,
    pub disabled: bool,
    pub tooltip: Option<&'a str>,
    pub danger_hover: bool,
}

impl<'a> Default for IconButton<'a> {
    fn default() -> Self {
        IconButton {
            icon: "",
            icon_size: 16.0,
            radius: theme::RADIUS_SM,
            active: false,
            disabled: false,
            tooltip: None,
            danger_hover: false,
        }
    }
}

impl<'a> IconButton<'a> {
    pub fn new(icon: &'a str) -> Self {
        IconButton {
            icon,
            ..Default::default()
        }
    }

    pub fn size(mut self, s: f32) -> Self {
        self.icon_size = s;
        self
    }

    pub fn active(mut self, a: bool) -> Self {
        self.active = a;
        self
    }

    pub fn disabled(mut self, d: bool) -> Self {
        self.disabled = d;
        self
    }

    pub fn tooltip(mut self, t: &'a str) -> Self {
        self.tooltip = Some(t);
        self
    }

    pub fn show(self, ui: &mut Ui, id_src: impl std::hash::Hash, rect: Rect) -> Interaction {
        let id = ui.id(id_src);
        let it = if self.disabled {
            Interaction::default()
        } else {
            ui.interact(id, rect)
        };

        let alpha = if self.disabled { 102 } else { 255 }; // opacity-40
        if self.active {
            ui.fill_rounded(rect, self.radius, theme::SURFACE_3);
        } else if it.hovered || it.held {
            ui.fill_rounded(rect, self.radius, theme::SURFACE_2);
        }
        let color = if self.active {
            theme::ACCENT
        } else if it.hovered && self.danger_hover {
            theme::DANGER
        } else if it.hovered {
            theme::TEXT_1
        } else {
            theme::TEXT_2
        };
        ui.icon(
            self.icon,
            rect,
            self.icon_size,
            theme::with_alpha(color, alpha),
        );
        if it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
        }
        if let Some(tip) = self.tooltip {
            ui.tooltip(id, rect, tip);
        }
        it
    }
}

/// Text-Button-Stile.
pub enum TextButtonStyle {
    /// h-6 px-2 bg-surface-3 hover:bg-surface-4 (Import-Button im Browser)
    Solid,
    /// h-6 px-2 border-line hover:border-line-strong (Outline)
    Outline,
    /// nur Text, hover:text-text-1
    Ghost,
}

pub struct TextButton<'a> {
    pub label: &'a str,
    pub icon: Option<&'a str>,
    pub style: TextButtonStyle,
    pub disabled: bool,
    pub font: FontKind,
}

impl<'a> TextButton<'a> {
    pub fn new(label: &'a str) -> Self {
        TextButton {
            label,
            icon: None,
            style: TextButtonStyle::Solid,
            disabled: false,
            font: FontKind::Sans12,
        }
    }

    pub fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn style(mut self, style: TextButtonStyle) -> Self {
        self.style = style;
        self
    }

    pub fn disabled(mut self, d: bool) -> Self {
        self.disabled = d;
        self
    }

    /// Bevorzugte Breite (Inhalt + Padding px-2, gap-1.5 bei Icon).
    pub fn measure(&self, ui: &Ui) -> f32 {
        let text_w = ui.font(self.font).width(self.label);
        let icon_w = if self.icon.is_some() { 14.0 + 6.0 } else { 0.0 };
        text_w + icon_w + 16.0
    }

    pub fn show(self, ui: &mut Ui, id_src: impl std::hash::Hash, rect: Rect) -> Interaction {
        let id = ui.id(id_src);
        let it = if self.disabled {
            Interaction::default()
        } else {
            ui.interact(id, rect)
        };
        let alpha = if self.disabled { 102 } else { 255 };
        match self.style {
            TextButtonStyle::Solid => {
                let bg = if it.hovered || it.held {
                    theme::SURFACE_4
                } else {
                    theme::SURFACE_3
                };
                ui.fill_rounded(rect, theme::RADIUS_SM, theme::with_alpha(bg, alpha));
            }
            TextButtonStyle::Outline => {
                let bc = if it.hovered {
                    theme::LINE_STRONG
                } else {
                    theme::LINE
                };
                ui.stroke_rounded(rect, theme::RADIUS_SM, 1.0, theme::with_alpha(bc, alpha));
            }
            TextButtonStyle::Ghost => {}
        }
        let fg = if it.hovered {
            theme::TEXT_1
        } else {
            theme::TEXT_2
        };
        let fg = theme::with_alpha(fg, alpha);
        let mut inner = rect.inset_xy(8.0, 0.0);
        if let Some(icon) = self.icon {
            let icon_rect = inner.cut_left(14.0);
            ui.icon(icon, icon_rect, 14.0, fg);
            inner.cut_left(6.0); // gap-1.5
        }
        ui.text_left(self.label, inner, fg, self.font);
        if it.hovered && !self.disabled {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
        }
        it
    }
}

/// Weicher Schatten (shadow-xl-Ersatz) um ein Overlay-Rechteck.
pub fn drop_shadow(ui: &mut Ui, rect: Rect, radius: f32) {
    for (grow, alpha) in [(12.0, 14), (6.0, 22), (2.0, 36)] {
        let r = Rect::new(
            rect.x - grow,
            rect.y - grow + grow * 0.4,
            rect.w + grow * 2.0,
            rect.h + grow * 2.0,
        );
        ui.fill_rounded(r, radius + grow, Color::new(0, 0, 0, alpha));
    }
}

/// Checkbox-artiger Toggle mit Icon + Label (z. B. Snapping in der Timeline).
pub fn toggle_chip(
    ui: &mut Ui,
    id_src: impl std::hash::Hash,
    rect: Rect,
    icon: &str,
    label: &str,
    active: bool,
) -> Interaction {
    let id = ui.id(id_src);
    let it = ui.interact(id, rect);
    if active {
        ui.fill_rounded(rect, theme::RADIUS_SM, theme::SURFACE_3);
    } else if it.hovered {
        ui.fill_rounded(rect, theme::RADIUS_SM, theme::SURFACE_2);
    }
    let fg = if active {
        theme::ACCENT
    } else if it.hovered {
        theme::TEXT_1
    } else {
        theme::TEXT_2
    };
    let mut inner = rect.inset_xy(6.0, 0.0);
    let icon_rect = inner.cut_left(14.0);
    ui.icon(icon, icon_rect, 14.0, fg);
    inner.cut_left(4.0);
    ui.text_left(label, inner, fg, FontKind::Sans12);
    if it.hovered {
        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
    }
    it
}

/// input[type=range]-Nachbau: Track h-1 rounded-full bg-surface-4,
/// Thumb 10×20 rounded-xs mit border-line-strong.
pub fn slider(
    ui: &mut Ui,
    id_src: impl std::hash::Hash,
    rect: Rect,
    value: &mut f64,
    min: f64,
    max: f64,
    thumb_color: Color,
) -> Interaction {
    let id = ui.id(id_src);
    let it = ui.interact(id, rect);
    let track = Rect::new(rect.x + 5.0, rect.y + rect.h / 2.0 - 2.0, rect.w - 10.0, 4.0);
    ui.fill_rounded(track, 2.0, theme::SURFACE_4);

    if it.held {
        let t = ((ui.input.mouse.x - track.x) / track.w).clamp(0.0, 1.0) as f64;
        *value = min + t * (max - min);
    }

    let t = if max > min {
        ((*value - min) / (max - min)).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let cx = track.x + t * track.w;
    let thumb = Rect::new(cx - 5.0, rect.y + rect.h / 2.0 - 10.0, 10.0, 20.0);
    ui.fill_rounded(thumb, theme::RADIUS_XS, thumb_color);
    ui.stroke_rounded(thumb, theme::RADIUS_XS, 1.0, theme::LINE_STRONG);
    if it.hovered || it.held {
        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
    }
    it
}
