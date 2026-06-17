//! Komponentenbasiertes Immediate-Mode-UI-Framework auf raylib.
//!
//! Pro Frame entsteht ein [`Ui`]-Kontext (Zeichen-Handle + Input + Fonts +
//! persistenter Interaktions-State). Komponenten sind Structs mit eigenem
//! State, die `update(ui, app, rect)` implementieren — Rendering und
//! Interaktion in einem Pass.

pub mod fx_shader;
pub mod blend_shader;
pub mod geom;
pub mod grade_shader;
pub mod icons;
pub mod lut_gpu;
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

/// In-App-Drag&Drop-Payload (Assets aus dem Medien-Browser, Dock-Tabs,
/// Effekte aus dem Effekte-Panel).
#[derive(Clone, Debug, PartialEq)]
pub enum DragPayload {
    /// Asset-IDs aus dem Medien-Browser (MIME "editron/assets").
    Assets(Vec<String>),
    /// Sequenz-IDs aus dem Medien-Browser (Drag in eine andere Timeline =
    /// Nesting).
    Sequences(Vec<String>),
    /// Bin-IDs aus dem Medien-Browser (Ordner zwischen Bins verschieben).
    Bins(Vec<String>),
    /// Dock-Tab wird gezogen.
    Tab { panel: String },
    /// Effekt aus dem Effekte-Panel (Ziel: Timeline-Clip oder
    /// Effekteinstellungen).
    Effect(crate::core::effects::EffectKind),
    /// Übergang aus dem Effekte-Panel (Ziel: Schnittkante in der Timeline).
    Transition(crate::core::transitions::TransitionKind),
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
    /// Kontinuierlicher (ungerasterter) Wert des gerade gezogenen Sliders.
    /// Nur ein Widget kann „active“ sein, daher genügt ein Slot. Wird beim
    /// Drücken neu gesetzt und während des Ziehens rein aus der Mausbewegung
    /// fortgeschrieben (siehe [`widgets::slider`]) — so koppelt ein Slider, der
    /// das Layout selbst skaliert (HiDPI-Faktor), nicht über die Position zurück.
    pub slider_grab: f64,
}

/// Befehl, der nach dem UI-Pass über die Registry ausgeführt wird.
#[derive(Clone, Debug)]
pub struct Dispatch {
    pub command: String,
    pub arg: Option<serde_json::Value>,
}

/// Anforderung eines Hover-Scrub-Vorschaubilds (Medien-Browser): erzeugt im
/// Hintergrund ein Standbild des Assets zur Zeit `time` und legt es unter
/// `bucket` ab. `bucket` quantisiert die Scrub-Position für den Cache.
#[derive(Clone, Debug)]
pub struct ScrubRequest {
    pub asset_id: String,
    pub path: String,
    pub time: f64,
    pub bucket: u32,
}

/// Input-/Look-LUT-Referenzen (Pfad + Stärke 0…1) eines gegradeten Quads.
/// `draw_texture_quad_graded` schlägt die GPU-Texturen über den Pfad im
/// `lut_textures`-Cache nach. Leer ⇒ kein LUT-Slot.
#[derive(Clone, Copy, Default)]
pub struct GradeLutRefs<'a> {
    pub input: Option<(&'a str, f32)>,
    pub look: Option<(&'a str, f32)>,
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
    /// Logische Zeichenfläche (Framebuffer-Pixel ÷ [`Ui::scale`]). Das gesamte
    /// Layout rechnet in diesem logischen Raum.
    pub screen: Rect,
    /// HiDPI-Faktor: bildet Layout-Logikpixel auf Framebuffer-Pixel ab. Alle
    /// Zeichen-/Hit-Test-Helfer übersetzen zentral mit diesem Faktor — Panels
    /// rechnen ausschließlich logisch und werden NICHT angefasst.
    pub scale: f32,
    /// Farbkorrektur-Shader für den Programmmonitor (None ⇒ ungegradete
    /// Vorschau, z. B. wenn die Kompilierung fehlschlug). Wird in main()
    /// nach `Ui::new` gesetzt.
    pub grade_shader: Option<&'f mut grade_shader::GradeShader>,
    /// Hochgeladene 3D-LUT-Texturen (pfad-indiziert); wird in main() nach
    /// `Ui::new` gesetzt. Der Programmmonitor bindet sie an den Grade-Shader.
    pub lut_textures: Option<&'f lut_gpu::LutGpuCache>,
    /// Effekt-Renderer (Lesezugriff auf die `fx://`-Ergebnis-Texturen);
    /// wird in main() nach `Ui::new` gesetzt.
    pub fx_outputs: Option<&'f fx_shader::EffectChainRenderer>,
    /// Raylib-Thread-Handle (für `begin_texture_mode` im Blend-Compositor);
    /// wird in main() nach `Ui::new` gesetzt.
    pub thread: Option<&'f raylib::RaylibThread>,
    /// Blend-Compositor (Lesezugriff auf das Ergebnis; Mainloop setzt es).
    pub blend_compositor: Option<&'f blend_shader::BlendCompositor>,
    /// Effekt-Jobs für den nächsten Frame (Pendant zu `texture_requests`;
    /// der Mainloop verarbeitet sie zwischen den Frames).
    pub effect_requests: Vec<fx_shader::EffectJob>,
    /// Hover-Scrub-Anforderungen des Medien-Browsers (Pendant zu
    /// `texture_requests`); der Mainloop löst sie nach dem Frame asynchron auf.
    pub scrub_requests: Vec<ScrubRequest>,
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
        scale: f32,
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
            scale,
            grade_shader: None,
            lut_textures: None,
            fx_outputs: None,
            thread: None,
            blend_compositor: None,
            effect_requests: Vec::new(),
            scrub_requests: Vec::new(),
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

    /// Hover-Scrub-Vorschaubild anfordern (vom Medien-Browser).
    pub fn request_scrub(&mut self, asset_id: &str, path: &str, time: f64, bucket: u32) {
        self.scrub_requests.push(ScrubRequest {
            asset_id: asset_id.to_string(),
            path: path.to_string(),
            time,
            bucket,
        });
    }

    // ----- Clipping ----------------------------------------------------------

    pub fn push_clip(&mut self, rect: Rect) {
        // Clip-Stack hält LOGISCHE Rechtecke (Hit-Test gegen logische Maus);
        // die Scissor-Box wird beim Setzen in Framebuffer-Pixel übersetzt.
        let clipped = match self.clip_stack.last() {
            Some(top) => top.intersect(rect),
            None => rect,
        };
        self.clip_stack.push(clipped);
        unsafe {
            ffi::EndScissorMode();
        }
        self.begin_scissor(clipped);
    }

    pub fn pop_clip(&mut self) {
        self.clip_stack.pop();
        unsafe {
            ffi::EndScissorMode();
        }
        if let Some(top) = self.clip_stack.last().copied() {
            self.begin_scissor(top);
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

    // ----- HiDPI-Übersetzung (logisch → Framebuffer-Pixel) -------------------

    /// Skalar logisch → physikalisch.
    #[inline]
    fn sx(&self, v: f32) -> f32 {
        v * self.scale
    }

    /// Punkt logisch → physikalisch (ohne Snapping; für glatt animierte
    /// Geometrie wie Transform-Gizmo/Kurven).
    #[inline]
    fn pv(&self, p: Vector2) -> Vector2 {
        v2(p.x * self.scale, p.y * self.scale)
    }

    /// Rechteck logisch → physikalisch, auf ganze Framebuffer-Pixel gesnappt.
    /// Beide Kanten werden gerundet (statt Position+Breite separat), damit
    /// aneinandergrenzende Flächen nahtlos bleiben und 1-px-Linien gestochen
    /// scharf auf dem Pixelraster sitzen — kein 1,5-px-Verschmieren.
    #[inline]
    fn rp(&self, r: Rect) -> Rect {
        let s = self.scale;
        let x0 = (r.x * s).round();
        let y0 = (r.y * s).round();
        let x1 = (r.right() * s).round();
        let y1 = (r.bottom() * s).round();
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Rechteck logisch → physikalisch ohne Snapping (Texturen: glatte
    /// Sub-Pixel-Bewegung, kein Zittern bei animierten Layern).
    #[inline]
    fn rpf(&self, r: Rect) -> Rect {
        let s = self.scale;
        Rect::new(r.x * s, r.y * s, r.w * s, r.h * s)
    }

    /// Scissor (Framebuffer-Pixel) für ein logisches Clip-Rechteck setzen.
    /// Außenkanten großzügig (floor/ceil), damit kein Inhalt am Rand
    /// angeschnitten wird.
    fn begin_scissor(&self, r: Rect) {
        let s = self.scale;
        let x0 = (r.x * s).floor() as i32;
        let y0 = (r.y * s).floor() as i32;
        let x1 = (r.right() * s).ceil() as i32;
        let y1 = (r.bottom() * s).ceil() as i32;
        unsafe {
            ffi::BeginScissorMode(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0));
        }
    }

    // ----- Zeichen-Helfer ----------------------------------------------------

    pub fn fill(&mut self, rect: Rect, color: Color) {
        let r = self.rp(rect);
        self.d.draw_rectangle_rec(r, color);
    }

    pub fn fill_rounded(&mut self, rect: Rect, radius: f32, color: Color) {
        if radius <= 0.0 {
            self.fill(rect, color);
            return;
        }
        // Rundungsanteil aus den LOGISCHEN Maßen — auf das physikalische
        // Rechteck angewandt skaliert der Radius automatisch korrekt mit.
        let roundness = (radius / (rect.w.min(rect.h) / 2.0)).min(1.0);
        let pr = self.rp(rect);
        self.d.draw_rectangle_rounded(pr, roundness, 6, color);
    }

    pub fn stroke_rounded(&mut self, rect: Rect, radius: f32, thickness: f32, color: Color) {
        if radius <= 0.0 {
            self.stroke(rect, thickness, color);
            return;
        }
        let roundness = (radius / (rect.w.min(rect.h) / 2.0)).min(1.0);
        let pr = self.rp(rect);
        let t = self.sx(thickness);
        self.d
            .draw_rectangle_rounded_lines_ex(pr, roundness, 6, t, color);
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
        let s = self.scale;
        // Physikalische Schriftgröße zur AKTUELLEN Skalierung (logisch × scale) —
        // bewusst NICHT die beim Rastern gespeicherte Atlas-Auflösung. So bleibt
        // Text exakt dimensioniert und platziert, auch wenn der Atlas (nach einem
        // Scale-Wechsel) noch in einer anderen Auflösung vorliegt und erst
        // verzögert neu gerastert wird — raylib skaliert die Glyphen dann
        // bilinear (OVERSAMPLE federt das ab). Grundlinie aufs Pixelraster runden.
        let render_size = font.size * s;
        let p = v2((pos.x * s).round(), (pos.y * s).round());
        self.d
            .draw_text_ex(font.raw(), text, p, render_size, 0.0, color);
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
        let scale = self.scale;
        self.icons.draw(self.d, name, rect, size, color, scale);
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
                let dest = self.rpf(rect);
                self.d.draw_texture_pro(
                    tex,
                    src,
                    dest,
                    v2(0.0, 0.0),
                    0.0,
                    Color::WHITE,
                );
            }
            None => self.texture_requests.push(path.to_string()),
        }
    }

    /// Gecachte Texture (Schlüssel) in ein explizites LOGISCHES Ziel-Rechteck
    /// zeichnen (Teil-`src` in Texturpixeln, frei wählbarer Tint). Für Panels,
    /// die object-fit-frei platzieren (z. B. Timeline-Thumbnails). Skaliert das
    /// Ziel zentral auf Framebuffer-Pixel.
    pub fn draw_texture_in(&mut self, key: &str, src: Rect, dest: Rect, tint: Color) {
        let raw = match self.textures.get(key) {
            Some(tex) => *tex.as_ref(),
            None => {
                self.texture_requests.push(key.to_string());
                return;
            }
        };
        struct RawTex(ffi::Texture2D);
        impl AsRef<ffi::Texture2D> for RawTex {
            fn as_ref(&self) -> &ffi::Texture2D {
                &self.0
            }
        }
        let d = self.rpf(dest);
        self.d
            .draw_texture_pro(RawTex(raw), src, d, v2(0.0, 0.0), 0.0, tint);
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
                let dst = self.rpf(Rect::new(cx, cy, w, h));
                let origin = self.pv(v2(w / 2.0, h / 2.0));
                self.d.draw_texture_pro(
                    tex,
                    Rect::new(0.0, 0.0, tex.width as f32, tex.height as f32),
                    dst,
                    origin,
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

    /// Wie [`Ui::draw_texture_quad`], aber mit Farbkorrektur über den
    /// Grade-Shader. `grade` = Identität (oder Shader nicht verfügbar)
    /// zeichnet ungegradet. `fx://`-Schlüssel werden aus dem Effekt-Renderer
    /// aufgelöst (vertikal kompensiert — RenderTexture-Inhalte sind
    /// gespiegelt gespeichert).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_texture_quad_graded(
        &mut self,
        key: &str,
        cx: f32,
        cy: f32,
        w: f32,
        h: f32,
        rot_deg: f32,
        alpha: u8,
        grade: &crate::core::grade::GradeParams,
        luts: GradeLutRefs<'_>,
    ) -> bool {
        use raylib::prelude::RaylibShaderModeExt;
        struct RawTex(ffi::Texture2D);
        impl AsRef<ffi::Texture2D> for RawTex {
            fn as_ref(&self) -> &ffi::Texture2D {
                &self.0
            }
        }
        let (tex, src) = if key.starts_with("fx://") {
            let Some(out) = self.fx_outputs.and_then(|r| r.output(key)) else {
                return false;
            };
            let (tw, th) = (out.tex.width as f32, out.tex.height as f32);
            let src_h = if out.flipped { -th } else { th };
            (RawTex(out.tex), Rect::new(0.0, 0.0, tw, src_h))
        } else if key == blend_shader::blend_output_key() {
            // Ergebnis des Blend-Compositors (Ping-Pong-RenderTexture, vertikal
            // gespiegelt gespeichert wie alle RenderTextures).
            let Some(t) = self.blend_compositor.and_then(|bc| bc.output_texture()) else {
                return false;
            };
            let (tw, th) = (t.width as f32, t.height as f32);
            (RawTex(t), Rect::new(0.0, 0.0, tw, -th))
        } else {
            match self.textures.get(key) {
                Some(tex) => {
                    let src = Rect::new(0.0, 0.0, tex.width as f32, tex.height as f32);
                    (RawTex(*tex.as_ref()), src)
                }
                None => {
                    self.texture_requests.push(key.to_string());
                    return false;
                }
            }
        };
        let dst = self.rpf(Rect::new(cx, cy, w, h));
        let origin = self.pv(v2(w / 2.0, h / 2.0));
        let tint = Color::new(255, 255, 255, alpha);

        // LUT-Texturen auflösen (kopiert die Option<&Cache>, hält keinen
        // self-Borrow ⇒ verträgt sich mit dem späteren &mut grade_shader).
        use crate::ui::grade_shader::LutUniform;
        let cache = self.lut_textures;
        let input = luts
            .input
            .and_then(|(p, s)| cache.and_then(|c| c.get(p)).map(|lt| (lt, s)));
        let look = luts
            .look
            .and_then(|(p, s)| cache.and_then(|c| c.get(p)).map(|lt| (lt, s)));
        let uni = |slot: Option<(&lut_gpu::LutTexture, f32)>| match slot {
            Some((lt, s)) => LutUniform {
                mode: lt.mode,
                size: lt.size,
                dmin: lt.dmin,
                dmax: lt.dmax,
                strength: s,
            },
            None => LutUniform::OFF,
        };
        let input_uni = uni(input);
        let look_uni = uni(look);
        let any_lut = input_uni.is_active() || look_uni.is_active();

        match self
            .grade_shader
            .as_mut()
            .filter(|_| !grade.is_identity() || any_lut)
        {
            Some(gs) => {
                gs.apply(grade);
                gs.apply_luts(input_uni, look_uni);
                // Roh-Handles VOR dem Shader-Modus ziehen (danach ist
                // gs.shader mutabel ausgeliehen).
                let (raw_shader, loc_in, loc_look) = gs.raw_and_lut_locs();
                let in_tex = input.filter(|_| input_uni.is_active()).map(|(lt, _)| *lt.tex.as_ref());
                let look_tex = look.filter(|_| look_uni.is_active()).map(|(lt, _)| *lt.tex.as_ref());
                let mut mode = self.d.begin_shader_mode(&mut gs.shader);
                // Zusatztexturen binden (raylib bindet sie beim Draw) — wie fx_shader.
                unsafe {
                    if let Some(t) = in_tex {
                        if loc_in >= 0 {
                            ffi::SetShaderValueTexture(raw_shader, loc_in, t);
                        }
                    }
                    if let Some(t) = look_tex {
                        if loc_look >= 0 {
                            ffi::SetShaderValueTexture(raw_shader, loc_look, t);
                        }
                    }
                }
                mode.draw_texture_pro(tex, src, dst, origin, rot_deg, tint);
            }
            None => {
                self.d.draw_texture_pro(tex, src, dst, origin, rot_deg, tint);
            }
        }
        true
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

    /// Größe eines Effekt-Ergebnisses (`fx://…`); None ⇒ noch nicht
    /// gerendert (Aufrufer fällt auf die Roh-Texture zurück).
    pub fn fx_output_size(&self, key: &str) -> Option<(f32, f32)> {
        let out = self.fx_outputs?.output(key)?;
        Some((out.tex.width as f32, out.tex.height as f32))
    }

    /// Hat der Blend-Compositor ein Ergebnis? (Größe in physischen Pixeln.)
    pub fn blend_output_size(&self) -> Option<(f32, f32)> {
        let bc = self.blend_compositor?;
        let tex = bc.output_texture()?;
        Some((tex.width as f32, tex.height as f32))
    }

    /// Bild als object-contain (Monitore): eingepasst, Seitenverhältnis bleibt.
    pub fn draw_texture_contain(&mut self, path: &str, rect: Rect) {
        match self.textures.get(path) {
            Some(tex) => {
                let target = self.rpf(rect.fit_contain(tex.width as f32, tex.height as f32));
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

    // ----- Skalierte Vektor-Primitive (für Panels: Gizmo, Scopes, Kurven) -----
    // Alle Methoden nehmen LOGISCHE Koordinaten und übersetzen zentral auf
    // Framebuffer-Pixel — Panels rufen ausschließlich diese statt `ui.d.*`.

    /// Linie mit Strichstärke (logisch); Endpunkte + Dicke werden skaliert.
    pub fn line(&mut self, a: Vector2, b: Vector2, thickness: f32, color: Color) {
        let (pa, pb, t) = (self.pv(a), self.pv(b), self.sx(thickness));
        self.d.draw_line_ex(pa, pb, t, color);
    }

    /// Dünne 1-px-Linie (Graticule/Fadenkreuz) — Endpunkte skaliert, raylib
    /// rastert exakt 1 Framebuffer-Pixel (scharf).
    pub fn line_thin(&mut self, a: Vector2, b: Vector2, color: Color) {
        let (pa, pb) = (self.pv(a), self.pv(b));
        self.d.draw_line_v(pa, pb, color);
    }

    /// Gefüllter Kreis (Zentrum + Radius logisch).
    pub fn circle(&mut self, center: Vector2, radius: f32, color: Color) {
        let (c, r) = (self.pv(center), self.sx(radius));
        self.d.draw_circle_v(c, r, color);
    }

    /// Kreis-Kontur (1-px-Linie, Zentrum + Radius logisch).
    pub fn circle_outline(&mut self, center: Vector2, radius: f32, color: Color) {
        let c = self.pv(center);
        self.d
            .draw_circle_lines(c.x as i32, c.y as i32, self.sx(radius), color);
    }

    /// Kreissektor (Farbring im Color-Panel).
    pub fn circle_sector(
        &mut self,
        center: Vector2,
        radius: f32,
        start_deg: f32,
        end_deg: f32,
        segments: i32,
        color: Color,
    ) {
        let (c, r) = (self.pv(center), self.sx(radius));
        self.d
            .draw_circle_sector(c, r, start_deg, end_deg, segments, color);
    }

    /// Gefülltes Dreieck (Playhead-Griff, Marker-Kerben).
    pub fn triangle(&mut self, a: Vector2, b: Vector2, c: Vector2, color: Color) {
        let (pa, pb, pc) = (self.pv(a), self.pv(b), self.pv(c));
        self.d.draw_triangle(pa, pb, pc, color);
    }

    /// Reguläres Polygon gefüllt (Keyframe-Raute = 4 Seiten, 0°).
    pub fn poly(&mut self, center: Vector2, sides: i32, radius: f32, rot: f32, color: Color) {
        let (c, r) = (self.pv(center), self.sx(radius));
        self.d.draw_poly(c, sides, r, rot, color);
    }

    /// Reguläres Polygon als Kontur.
    pub fn poly_lines(&mut self, center: Vector2, sides: i32, radius: f32, rot: f32, color: Color) {
        let (c, r) = (self.pv(center), self.sx(radius));
        self.d.draw_poly_lines(c, sides, r, rot, color);
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
