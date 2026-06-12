//! Komponentenbasiertes Immediate-Mode-UI-Framework auf raylib.
//!
//! Pro Frame entsteht ein [`Ui`]-Kontext (Zeichen-Handle + Input + Fonts +
//! persistenter Interaktions-State). Komponenten sind Structs mit eigenem
//! State, die `update(ui, app, rect)` implementieren — Rendering und
//! Interaktion in einem Pass.

pub mod geom;
pub mod icons;
pub mod icons_data;
pub mod input;
pub mod text;
pub mod textures;
pub mod widgets;

use geom::{v2, Rect};
use icons::IconSet;
use input::InputState;
use raylib::color::Color;
use raylib::consts::MouseCursor;
use raylib::core::drawing::RaylibDrawHandle;
use raylib::ffi;
use raylib::math::Vector2;
use raylib::prelude::RaylibDraw;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use text::{FontHandle, Fonts};
use textures::TextureCache;

pub type WidgetId = u64;

/// In-App-Drag&Drop-Payload (Assets aus dem Medien-Browser, Dock-Tabs).
#[derive(Clone, Debug, PartialEq)]
pub enum DragPayload {
    /// Asset-IDs aus dem Medien-Browser (MIME "editron/assets").
    Assets(Vec<String>),
    /// Dock-Tab wird gezogen.
    Tab { panel: String },
}

pub struct DragState {
    pub payload: DragPayload,
    pub origin: Vector2,
    /// Erst nach 4 px Bewegung „echter“ Drag (Klick bleibt Klick).
    pub started: bool,
    /// Drop wurde in diesem Frame angenommen.
    pub consumed: bool,
}

struct TooltipRequest {
    text: String,
    anchor: Rect,
}

/// Interaktions-State, der Frames überlebt.
#[derive(Default)]
pub struct UiPersist {
    pub hot: WidgetId,
    pub active: WidgetId,
    pub keyboard_focus: WidgetId,
    hot_this_frame: WidgetId,
    tooltip_widget: WidgetId,
    tooltip_since: f64,
    pub drag: Option<DragState>,
    pub clock: input::InputClock,
    /// Offenes Select-Dropdown (zentral, wird im Overlay-Pass gerendert).
    pub select: widgets::select::SelectHost,
}

/// Befehl, der nach dem UI-Pass über die Registry ausgeführt wird.
#[derive(Clone, Debug)]
pub struct Dispatch {
    pub command: String,
    pub arg: Option<serde_json::Value>,
}

pub struct Ui<'f, 'rl> {
    pub d: &'f mut RaylibDrawHandle<'rl>,
    pub input: InputState,
    pub fonts: &'f Fonts,
    pub icons: &'f IconSet,
    pub textures: &'f TextureCache,
    pub persist: &'f mut UiPersist,
    pub dispatch: Vec<Dispatch>,
    /// Pfade, deren Texturen vor dem nächsten Frame geladen werden sollen.
    pub texture_requests: Vec<String>,
    pub time: f64,
    pub frame_time: f32,
    pub screen: Rect,
    clip_stack: Vec<Rect>,
    cursor: MouseCursor,
    tooltip: Option<TooltipRequest>,
    /// true, solange die Hauptschicht gezeichnet wird und ein Overlay offen ist.
    overlay_blocks: bool,
}

impl<'f, 'rl> Ui<'f, 'rl> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        d: &'f mut RaylibDrawHandle<'rl>,
        input: InputState,
        fonts: &'f Fonts,
        icons: &'f IconSet,
        textures: &'f TextureCache,
        persist: &'f mut UiPersist,
        time: f64,
        frame_time: f32,
        screen: Rect,
    ) -> Self {
        persist.hot = persist.hot_this_frame;
        persist.hot_this_frame = 0;
        Ui {
            d,
            input,
            fonts,
            icons,
            textures,
            persist,
            dispatch: Vec::new(),
            texture_requests: Vec::new(),
            time,
            frame_time,
            screen,
            clip_stack: Vec::new(),
            cursor: MouseCursor::MOUSE_CURSOR_DEFAULT,
            tooltip: None,
            overlay_blocks: false,
        }
    }

    // ----- Widget-IDs ------------------------------------------------------

    pub fn id(&self, source: impl Hash) -> WidgetId {
        let mut h = DefaultHasher::new();
        source.hash(&mut h);
        let id = h.finish();
        if id == 0 {
            1
        } else {
            id
        }
    }

    // ----- Schichten / Input-Routing ----------------------------------------

    /// Hauptschicht beginnt; `overlay_open` = irgendein Overlay (Menü, Dialog,
    /// Palette, Drag) hat Vorrang und blockiert die Maus für diese Schicht.
    pub fn begin_main_layer(&mut self, overlay_open: bool) {
        self.overlay_blocks = overlay_open;
    }

    /// Overlay-Schicht beginnt (nach dem Haupt-Pass aufrufen).
    pub fn begin_overlay_layer(&mut self) {
        self.overlay_blocks = false;
    }

    /// Mausposition, falls diese Schicht Input bekommt.
    pub fn mouse(&self) -> Option<Vector2> {
        if self.overlay_blocks || self.input.mouse_blocked {
            None
        } else {
            Some(self.input.mouse)
        }
    }

    /// Hit-Test gegen Rechteck UND aktuellen Clip (gescrollte Inhalte!).
    pub fn mouse_in(&self, rect: Rect) -> bool {
        let Some(m) = self.mouse() else { return false };
        if !rect.contains(m) {
            return false;
        }
        match self.clip_stack.last() {
            Some(clip) => clip.contains(m),
            None => true,
        }
    }

    pub fn set_hot(&mut self, id: WidgetId) {
        self.persist.hot_this_frame = id;
    }

    pub fn is_hot(&self, id: WidgetId) -> bool {
        self.persist.hot == id
    }

    pub fn set_active(&mut self, id: WidgetId) {
        self.persist.active = id;
    }

    pub fn is_active(&self, id: WidgetId) -> bool {
        self.persist.active == id
    }

    pub fn clear_active(&mut self) {
        self.persist.active = 0;
    }

    /// Kein anderes Widget hält die Maus gedrückt.
    pub fn nothing_active(&self) -> bool {
        self.persist.active == 0
    }

    /// Standard-Interaktion: Hover/Press/Click für ein Widget in `rect`.
    pub fn interact(&mut self, id: WidgetId, rect: Rect) -> Interaction {
        let hovered = self.mouse_in(rect) && (self.nothing_active() || self.is_active(id));
        if hovered {
            self.set_hot(id);
            if self.input.left_pressed {
                self.set_active(id);
            }
        }
        let held = self.is_active(id) && self.input.left_down;
        let clicked = self.is_active(id) && self.input.left_released && hovered;
        let right_clicked = hovered && self.input.right_pressed && self.nothing_active();
        if self.is_active(id) && self.input.left_released {
            self.clear_active();
        }
        Interaction {
            hovered,
            held,
            clicked,
            right_clicked,
            double_clicked: hovered && self.input.double_click,
        }
    }

    pub fn want_cursor(&mut self, cursor: MouseCursor) {
        self.cursor = cursor;
    }

    pub fn take_cursor(&mut self) -> MouseCursor {
        std::mem::replace(&mut self.cursor, MouseCursor::MOUSE_CURSOR_DEFAULT)
    }

    // ----- Drag & Drop -------------------------------------------------------

    /// Beginnt einen Drag-Kandidaten (bei mouse-down auf einer Drag-Quelle).
    pub fn start_drag(&mut self, payload: DragPayload) {
        self.persist.drag = Some(DragState {
            payload,
            origin: self.input.mouse,
            started: false,
            consumed: false,
        });
    }

    /// Aktiver (gestarteter) Drag, falls vorhanden.
    pub fn active_drag(&self) -> Option<&DragPayload> {
        match &self.persist.drag {
            Some(d) if d.started => Some(&d.payload),
            _ => None,
        }
    }

    /// true, wenn ein gestarteter Drag über `rect` schwebt (Drop-Target-Check).
    /// Drag-Hover ignoriert die Overlay-Blockade, denn der Drag IST das Overlay.
    pub fn drag_over(&self, rect: Rect) -> Option<&DragPayload> {
        let drag = self.persist.drag.as_ref().filter(|d| d.started)?;
        if rect.contains(self.input.mouse) {
            Some(&drag.payload)
        } else {
            None
        }
    }

    /// true, wenn in diesem Frame ein Drop über `rect` stattfindet; markiert
    /// den Drag als konsumiert.
    pub fn accept_drop(&mut self, rect: Rect) -> Option<DragPayload> {
        let started = matches!(&self.persist.drag, Some(d) if d.started && !d.consumed);
        if !started || !self.input.left_released || !rect.contains(self.input.mouse) {
            return None;
        }
        let drag = self.persist.drag.as_mut().unwrap();
        drag.consumed = true;
        Some(drag.payload.clone())
    }

    /// Frame-Ende: Drag-Zustand fortschreiben (Start ab 4 px, Ende bei Release).
    pub fn finish_drag_frame(&mut self) {
        if let Some(drag) = &mut self.persist.drag {
            if !drag.started {
                let d = self.input.mouse - drag.origin;
                if (d.x * d.x + d.y * d.y).sqrt() > 4.0 {
                    drag.started = true;
                }
            }
            if self.input.left_released || !self.input.left_down {
                self.persist.drag = None;
            }
        }
    }

    // ----- Commands ----------------------------------------------------------

    pub fn run_command(&mut self, command: impl Into<String>) {
        self.dispatch.push(Dispatch {
            command: command.into(),
            arg: None,
        });
    }

    pub fn run_command_with(&mut self, command: impl Into<String>, arg: serde_json::Value) {
        self.dispatch.push(Dispatch {
            command: command.into(),
            arg: Some(arg),
        });
    }

    // ----- Clipping ----------------------------------------------------------

    pub fn push_clip(&mut self, rect: Rect) {
        let clipped = match self.clip_stack.last() {
            Some(top) => top.intersect(rect),
            None => rect,
        };
        self.clip_stack.push(clipped);
        unsafe {
            ffi::EndScissorMode();
            ffi::BeginScissorMode(
                clipped.x as i32,
                clipped.y as i32,
                clipped.w.ceil() as i32,
                clipped.h.ceil() as i32,
            );
        }
    }

    pub fn pop_clip(&mut self) {
        self.clip_stack.pop();
        unsafe {
            ffi::EndScissorMode();
            if let Some(top) = self.clip_stack.last() {
                ffi::BeginScissorMode(
                    top.x as i32,
                    top.y as i32,
                    top.w.ceil() as i32,
                    top.h.ceil() as i32,
                );
            }
        }
    }

    pub fn current_clip(&self) -> Option<Rect> {
        self.clip_stack.last().copied()
    }

    // ----- Tooltip -----------------------------------------------------------

    /// Tooltip nach Hover-Delay unterhalb des Ankers.
    pub fn tooltip(&mut self, id: WidgetId, anchor: Rect, text: &str) {
        if !self.is_hot(id) {
            return;
        }
        if self.persist.tooltip_widget != id {
            self.persist.tooltip_widget = id;
            self.persist.tooltip_since = self.time;
        }
        if self.time - self.persist.tooltip_since > 0.6 {
            self.tooltip = Some(TooltipRequest {
                text: text.to_string(),
                anchor,
            });
        }
    }

    /// Am Frame-Ende: Tooltip über allem zeichnen.
    pub fn draw_tooltip_overlay(&mut self) {
        if self.persist.hot == 0 || self.persist.hot != self.persist.tooltip_widget {
            self.persist.tooltip_widget = 0;
        }
        let Some(req) = self.tooltip.take() else { return };
        let font = &self.fonts.sans_12;
        let size = font.measure(&req.text);
        let pad_x = 8.0;
        let pad_y = 4.0;
        let w = size.x + pad_x * 2.0;
        let h = size.y + pad_y * 2.0;
        let mut x = req.anchor.x;
        let mut y = req.anchor.bottom() + 6.0;
        if x + w > self.screen.right() - 4.0 {
            x = self.screen.right() - 4.0 - w;
        }
        if y + h > self.screen.bottom() - 4.0 {
            y = req.anchor.y - 6.0 - h;
        }
        let rect = Rect::new(x.max(4.0), y, w, h);
        self.fill_rounded(rect, crate::theme::RADIUS_SM, crate::theme::SURFACE_3);
        self.stroke_rounded(rect, crate::theme::RADIUS_SM, 1.0, crate::theme::LINE_STRONG);
        self.text(
            &req.text,
            v2(rect.x + pad_x, rect.y + pad_y),
            crate::theme::TEXT_1,
            FontKind::Sans12,
        );
    }

    // ----- Zeichen-Helfer ----------------------------------------------------

    pub fn fill(&mut self, rect: Rect, color: Color) {
        self.d.draw_rectangle_rec(rect, color);
    }

    pub fn fill_rounded(&mut self, rect: Rect, radius: f32, color: Color) {
        if radius <= 0.0 {
            self.fill(rect, color);
            return;
        }
        let roundness = (radius / (rect.w.min(rect.h) / 2.0)).min(1.0);
        self.d.draw_rectangle_rounded(rect, roundness, 6, color);
    }

    pub fn stroke_rounded(&mut self, rect: Rect, radius: f32, thickness: f32, color: Color) {
        if radius <= 0.0 {
            self.stroke(rect, thickness, color);
            return;
        }
        let roundness = (radius / (rect.w.min(rect.h) / 2.0)).min(1.0);
        self.d
            .draw_rectangle_rounded_lines_ex(rect, roundness, 6, thickness, color);
    }

    /// 1px-scharfe Rechteck-Kontur (nicht gerundet).
    pub fn stroke(&mut self, rect: Rect, thickness: f32, color: Color) {
        let t = thickness;
        self.fill(Rect::new(rect.x, rect.y, rect.w, t), color);
        self.fill(Rect::new(rect.x, rect.bottom() - t, rect.w, t), color);
        self.fill(Rect::new(rect.x, rect.y + t, t, rect.h - 2.0 * t), color);
        self.fill(
            Rect::new(rect.right() - t, rect.y + t, t, rect.h - 2.0 * t),
            color,
        );
    }

    pub fn hline(&mut self, x: f32, y: f32, w: f32, color: Color) {
        self.fill(Rect::new(x, y, w, 1.0), color);
    }

    pub fn vline(&mut self, x: f32, y: f32, h: f32, color: Color) {
        self.fill(Rect::new(x, y, 1.0, h), color);
    }

    pub fn font(&self, kind: FontKind) -> &'f FontHandle {
        let fonts = self.fonts;
        match kind {
            FontKind::Sans12 => &fonts.sans_12,
            FontKind::Sans12Medium => &fonts.sans_12_medium,
            FontKind::Sans12Bold => &fonts.sans_12_bold,
            FontKind::Sans14 => &fonts.sans_14,
            FontKind::Sans14Semibold => &fonts.sans_14_semibold,
            FontKind::Sans16 => &fonts.sans_16,
            FontKind::Sans16Semibold => &fonts.sans_16_semibold,
            FontKind::Mono12 => &fonts.mono_12,
            FontKind::Mono11 => &fonts.mono_11,
        }
    }

    pub fn text(&mut self, text: &str, pos: Vector2, color: Color, kind: FontKind) {
        let font = self.font(kind);
        self.d.draw_text_ex(
            font.raw(),
            text,
            v2(pos.x.round(), pos.y.round()),
            font.size,
            0.0,
            color,
        );
    }

    /// Text vertikal zentriert in `rect`, linksbündig ab `rect.x`.
    pub fn text_left(&mut self, text: &str, rect: Rect, color: Color, kind: FontKind) {
        let font = self.font(kind);
        let h = font.measure(text).y;
        let y = rect.y + (rect.h - h) / 2.0;
        self.text(text, v2(rect.x, y), color, kind);
    }

    /// Text horizontal + vertikal zentriert in `rect`.
    pub fn text_centered(&mut self, text: &str, rect: Rect, color: Color, kind: FontKind) {
        let font = self.font(kind);
        let size = font.measure(text);
        self.text(
            text,
            v2(rect.x + (rect.w - size.x) / 2.0, rect.y + (rect.h - size.y) / 2.0),
            color,
            kind,
        );
    }

    /// Text rechtsbündig (rect.right()), vertikal zentriert.
    pub fn text_right(&mut self, text: &str, rect: Rect, color: Color, kind: FontKind) {
        let font = self.font(kind);
        let size = font.measure(text);
        self.text(
            text,
            v2(rect.right() - size.x, rect.y + (rect.h - size.y) / 2.0),
            color,
            kind,
        );
    }

    pub fn icon(&mut self, name: &str, rect: Rect, size: f32, color: Color) {
        self.icons.draw(self.d, name, rect, size, color);
    }

    /// Bild (z. B. Thumbnail) als object-cover in `rect`; lädt lazy über den
    /// TextureCache (1 Frame Latenz beim ersten Mal).
    pub fn draw_texture_cover(&mut self, path: &str, rect: Rect) {
        match self.textures.get(path) {
            Some(tex) => {
                let (tw, th) = (tex.width as f32, tex.height as f32);
                if tw <= 0.0 || th <= 0.0 {
                    return;
                }
                let scale = (rect.w / tw).max(rect.h / th);
                let (sw, sh) = (rect.w / scale, rect.h / scale);
                let src = Rect::new((tw - sw) / 2.0, (th - sh) / 2.0, sw, sh);
                self.d.draw_texture_pro(
                    tex,
                    src,
                    rect,
                    v2(0.0, 0.0),
                    0.0,
                    Color::WHITE,
                );
            }
            None => self.texture_requests.push(path.to_string()),
        }
    }

    /// Texture als transformierten Layer zeichnen: zentriert auf (cx, cy),
    /// Zielgröße (w, h), Rotation in Grad um den Mittelpunkt, Alpha 0–255.
    /// Liefert false (und fordert die Texture an), wenn sie noch fehlt.
    pub fn draw_texture_quad(
        &mut self,
        key: &str,
        cx: f32,
        cy: f32,
        w: f32,
        h: f32,
        rot_deg: f32,
        alpha: u8,
    ) -> bool {
        match self.textures.get(key) {
            Some(tex) => {
                self.d.draw_texture_pro(
                    tex,
                    Rect::new(0.0, 0.0, tex.width as f32, tex.height as f32),
                    Rect::new(cx, cy, w, h),
                    v2(w / 2.0, h / 2.0),
                    rot_deg,
                    Color::new(255, 255, 255, alpha),
                );
                true
            }
            None => {
                self.texture_requests.push(key.to_string());
                false
            }
        }
    }

    /// Natürliche Größe einer gecachten Texture (None ⇒ angefordert).
    pub fn texture_size(&mut self, key: &str) -> Option<(f32, f32)> {
        match self.textures.get(key) {
            Some(tex) => Some((tex.width as f32, tex.height as f32)),
            None => {
                self.texture_requests.push(key.to_string());
                None
            }
        }
    }

    /// Bild als object-contain (Monitore): eingepasst, Seitenverhältnis bleibt.
    pub fn draw_texture_contain(&mut self, path: &str, rect: Rect) {
        match self.textures.get(path) {
            Some(tex) => {
                let target = rect.fit_contain(tex.width as f32, tex.height as f32);
                self.d.draw_texture_pro(
                    tex,
                    Rect::new(0.0, 0.0, tex.width as f32, tex.height as f32),
                    target,
                    v2(0.0, 0.0),
                    0.0,
                    Color::WHITE,
                );
            }
            None => self.texture_requests.push(path.to_string()),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FontKind {
    Sans12,
    Sans12Medium,
    Sans12Bold,
    Sans14,
    Sans14Semibold,
    Sans16,
    Sans16Semibold,
    Mono12,
    Mono11,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Interaction {
    pub hovered: bool,
    pub held: bool,
    pub clicked: bool,
    pub right_clicked: bool,
    pub double_clicked: bool,
}
