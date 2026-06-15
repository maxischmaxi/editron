//! Scroll-Container mit Webkit-Scrollbar-Optik (10 px Spur, Thumb surface-4
//! mit 2 px content-box-Rand, hover line-strong). Die Scrollbar nimmt wie im
//! Browser Layout-Platz vom Inhalt.

use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::Ui;

/// Pixel pro Mausrad-Tick.
const WHEEL_STEP: f32 = 48.0;

#[derive(Default)]
pub struct ScrollState {
    pub offset_y: f32,
    pub offset_x: f32,
    drag_v: Option<f32>,
    drag_h: Option<f32>,
}

pub struct ScrollView {
    /// Sichtbarer Inhaltsbereich (Scrollbars bereits abgezogen).
    pub viewport: Rect,
    /// viewport.y - offset_y: y-Koordinate, an der Inhalt bei 0 beginnt.
    pub origin_y: f32,
    pub origin_x: f32,
}

impl ScrollState {
    /// Beginnt den Scroll-Bereich: verarbeitet Mausrad, zeichnet nichts.
    /// Inhalte ab `view.origin_y` zeichnen; danach `end()` für Scrollbars +
    /// Clip-Aufhebung aufrufen.
    pub fn begin(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        content_w: f32,
        content_h: f32,
    ) -> ScrollView {
        let need_v = content_h > rect.h;
        let mut viewport = rect;
        if need_v {
            viewport.w -= theme::SCROLLBAR_W;
        }
        let need_h = content_w > viewport.w;
        if need_h {
            viewport.h -= theme::SCROLLBAR_W;
        }

        if ui.mouse_in(rect) && ui.nothing_active() {
            if ui.input.shift {
                self.offset_x -= ui.input.wheel.y * WHEEL_STEP;
            } else {
                self.offset_y -= ui.input.wheel.y * WHEEL_STEP;
                self.offset_x -= ui.input.wheel.x * WHEEL_STEP;
            }
        }
        self.clamp(viewport, content_w, content_h);

        ui.push_clip(viewport);
        ScrollView {
            viewport,
            origin_y: viewport.y - self.offset_y,
            origin_x: viewport.x - self.offset_x,
        }
    }

    fn clamp(&mut self, viewport: Rect, content_w: f32, content_h: f32) {
        self.offset_y = self.offset_y.clamp(0.0, (content_h - viewport.h).max(0.0));
        self.offset_x = self.offset_x.clamp(0.0, (content_w - viewport.w).max(0.0));
    }

    pub fn end(&mut self, ui: &mut Ui, rect: Rect, content_w: f32, content_h: f32) {
        ui.pop_clip();

        let need_v = content_h > rect.h;
        let mut viewport = rect;
        if need_v {
            viewport.w -= theme::SCROLLBAR_W;
        }
        let need_h = content_w > viewport.w;
        if need_h {
            viewport.h -= theme::SCROLLBAR_W;
        }

        if need_v {
            let track = Rect::new(viewport.right(), viewport.y, theme::SCROLLBAR_W, viewport.h);
            self.offset_y = scrollbar(
                ui,
                ui.id((rect.x.to_bits(), rect.y.to_bits(), "vscroll")),
                track,
                self.offset_y,
                viewport.h,
                content_h,
                false,
                &mut self.drag_v,
            );
        }
        if need_h {
            let track = Rect::new(viewport.x, viewport.bottom(), viewport.w, theme::SCROLLBAR_W);
            self.offset_x = scrollbar(
                ui,
                ui.id((rect.x.to_bits(), rect.y.to_bits(), "hscroll")),
                track,
                self.offset_x,
                viewport.w,
                content_w,
                true,
                &mut self.drag_h,
            );
        }
        self.clamp(viewport, content_w, content_h);
    }
}

/// Zeichnet eine Scrollbar und liefert den neuen Offset.
#[allow(clippy::too_many_arguments)]
pub fn scrollbar(
    ui: &mut Ui,
    id: u64,
    track: Rect,
    offset: f32,
    viewport_len: f32,
    content_len: f32,
    horizontal: bool,
    drag: &mut Option<f32>,
) -> f32 {
    let mut offset = offset;
    let track_len = if horizontal { track.w } else { track.h };
    let thumb_len = (viewport_len / content_len * track_len).max(24.0);
    let max_offset = (content_len - viewport_len).max(1.0);
    let thumb_pos = (offset / max_offset) * (track_len - thumb_len);

    let thumb = if horizontal {
        Rect::new(track.x + thumb_pos, track.y + 2.0, thumb_len, track.h - 4.0)
    } else {
        Rect::new(track.x + 2.0, track.y + thumb_pos, track.w - 4.0, thumb_len)
    };

    let it = ui.interact(id, thumb);
    if (it.hovered || it.held)
        && ui.input.left_pressed && it.hovered {
            *drag = Some(if horizontal {
                ui.input.mouse.x - thumb.x
            } else {
                ui.input.mouse.y - thumb.y
            });
        }
    if it.held {
        if let Some(grab) = *drag {
            let mouse = if horizontal {
                ui.input.mouse.x
            } else {
                ui.input.mouse.y
            };
            let track_start = if horizontal { track.x } else { track.y };
            let new_thumb = (mouse - grab - track_start).clamp(0.0, track_len - thumb_len);
            offset = new_thumb / (track_len - thumb_len).max(1.0) * max_offset;
        }
    } else {
        *drag = None;
    }

    let color = if it.hovered || it.held {
        theme::LINE_STRONG
    } else {
        theme::SURFACE_4
    };
    ui.fill_rounded(thumb, 3.0, color);
    offset
}
