//! Design-Tokens der App.
//! Maß-Skala: 1 Einheit = 4 px; Schriftgrößen xs = 12, sm = 14, base = 16 px.

use raylib::color::Color;

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color { r, g, b, a: 255 }
}

pub const fn with_alpha(c: Color, a: u8) -> Color {
    Color { a, ..c }
}

// Flächen — von tiefster Ebene (App-Hintergrund) nach oben
pub const SURFACE_0: Color = rgb(0x12, 0x13, 0x16);
pub const SURFACE_1: Color = rgb(0x1a, 0x1c, 0x20);
pub const SURFACE_2: Color = rgb(0x22, 0x24, 0x28);
pub const SURFACE_3: Color = rgb(0x2a, 0x2d, 0x33);
pub const SURFACE_4: Color = rgb(0x34, 0x38, 0x3f);

// Linien & Trenner
pub const LINE: Color = rgb(0x2e, 0x31, 0x37);
pub const LINE_STRONG: Color = rgb(0x41, 0x45, 0x4d);

// Text
pub const TEXT_1: Color = rgb(0xe6, 0xe8, 0xee);
pub const TEXT_2: Color = rgb(0xa7, 0xad, 0xbb);
pub const TEXT_3: Color = rgb(0x6e, 0x74, 0x80);

// Akzente
pub const ACCENT: Color = rgb(0x4f, 0x8d, 0xff);
pub const ACCENT_HOVER: Color = rgb(0x6a, 0xa1, 0xff);
pub const ACCENT_SOFT: Color = rgb(0x24, 0x40, 0x6e);
pub const DANGER: Color = rgb(0xff, 0x5c, 0x5c);
pub const WARNING: Color = rgb(0xff, 0xb5, 0x47);
pub const SUCCESS: Color = rgb(0x3e, 0xe0, 0x8f);

pub const WHITE: Color = rgb(0xff, 0xff, 0xff);
pub const BLACK: Color = rgb(0x00, 0x00, 0x00);

// Eckenradien
pub const RADIUS_XS: f32 = 2.0;
pub const RADIUS_SM: f32 = 4.0;
pub const RADIUS_MD: f32 = 6.0;
pub const RADIUS_LG: f32 = 8.0;

// Shell-Maße (CSS-px)
pub const TITLEBAR_H: f32 = 40.0;
pub const STATUSBAR_H: f32 = 24.0;
pub const DOCK_TAB_H: f32 = 32.0; // Tab-Leiste der Dock-Gruppen
pub const DOCK_GAP: f32 = 2.0; // Fuge zwischen Dock-Gruppen
pub const SCROLLBAR_W: f32 = 10.0;
