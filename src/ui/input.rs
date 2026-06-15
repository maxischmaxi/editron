//! Input-Sammlung pro Frame: Maus (inkl. Doppelklick), Tastatur (Queue mit
//! Auto-Repeat), Texteingabe, Mausrad, gedroppte Dateien.

use raylib::consts::{KeyboardKey, MouseButton};
use raylib::math::Vector2;
use raylib::RaylibHandle;
use std::path::PathBuf;

const DOUBLE_CLICK_SECS: f64 = 0.4;
const DOUBLE_CLICK_DIST: f32 = 4.0;

/// Ein im Frame gedrückter Key (inkl. Auto-Repeat) mit Modifier-Zustand.
#[derive(Clone, Copy, Debug)]
pub struct KeyPress {
    pub key: KeyboardKey,
    pub repeat: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

#[derive(Default)]
pub struct InputState {
    pub mouse: Vector2,
    pub mouse_delta: Vector2,

    pub left_pressed: bool,
    pub left_down: bool,
    pub left_released: bool,
    pub right_pressed: bool,
    pub right_released: bool,
    pub middle_pressed: bool,
    pub middle_down: bool,
    pub middle_released: bool,
    pub double_click: bool,

    pub wheel: Vector2,

    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,

    pub keys: Vec<KeyPress>,
    pub chars: Vec<char>,

    pub dropped_files: Vec<PathBuf>,

    /// Schichtensteuerung: true ⇒ die Hauptschicht sieht keine Maus
    /// (Kontextmenü, Dialog, Palette oder Drag-Overlay ist aktiv).
    pub mouse_blocked: bool,
}

/// Persistenter Anteil der Input-Erkennung (Doppelklick-Timer).
#[derive(Default)]
pub struct InputClock {
    last_click_at: f64,
    last_click_pos: Vector2,
}

/// Alle KeyboardKey-Varianten, für die Auto-Repeat geprüft wird.
const REPEATABLE_KEYS: &[KeyboardKey] = &[
    KeyboardKey::KEY_LEFT,
    KeyboardKey::KEY_RIGHT,
    KeyboardKey::KEY_UP,
    KeyboardKey::KEY_DOWN,
    KeyboardKey::KEY_BACKSPACE,
    KeyboardKey::KEY_DELETE,
    KeyboardKey::KEY_ENTER,
    KeyboardKey::KEY_TAB,
    KeyboardKey::KEY_PAGE_UP,
    KeyboardKey::KEY_PAGE_DOWN,
    KeyboardKey::KEY_KP_ADD,
    KeyboardKey::KEY_KP_SUBTRACT,
    KeyboardKey::KEY_EQUAL,
    KeyboardKey::KEY_MINUS,
];

impl InputState {
    pub fn collect(rl: &mut RaylibHandle, clock: &mut InputClock) -> InputState {
        let mouse = rl.get_mouse_position();
        let mouse_delta = rl.get_mouse_delta();
        let left_pressed = rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT);
        let now = rl.get_time();

        let mut double_click = false;
        if left_pressed {
            let dist = ((mouse.x - clock.last_click_pos.x).powi(2)
                + (mouse.y - clock.last_click_pos.y).powi(2))
            .sqrt();
            if now - clock.last_click_at < DOUBLE_CLICK_SECS && dist < DOUBLE_CLICK_DIST {
                double_click = true;
                clock.last_click_at = 0.0; // Dreifachklick nicht als weiteren Doppelklick werten
            } else {
                clock.last_click_at = now;
            }
            clock.last_click_pos = mouse;
        }

        let ctrl = rl.is_key_down(KeyboardKey::KEY_LEFT_CONTROL)
            || rl.is_key_down(KeyboardKey::KEY_RIGHT_CONTROL);
        let shift = rl.is_key_down(KeyboardKey::KEY_LEFT_SHIFT)
            || rl.is_key_down(KeyboardKey::KEY_RIGHT_SHIFT);
        let alt = rl.is_key_down(KeyboardKey::KEY_LEFT_ALT)
            || rl.is_key_down(KeyboardKey::KEY_RIGHT_ALT);
        let meta = rl.is_key_down(KeyboardKey::KEY_LEFT_SUPER)
            || rl.is_key_down(KeyboardKey::KEY_RIGHT_SUPER);

        let mut keys = Vec::new();
        while let Some(key) = rl.get_key_pressed() {
            keys.push(KeyPress {
                key,
                repeat: false,
                ctrl,
                shift,
                alt,
                meta,
            });
        }
        for &key in REPEATABLE_KEYS {
            if rl.is_key_pressed_repeat(key) {
                keys.push(KeyPress {
                    key,
                    repeat: true,
                    ctrl,
                    shift,
                    alt,
                    meta,
                });
            }
        }

        let mut chars = Vec::new();
        while let Some(c) = rl.get_char_pressed() {
            chars.push(c);
        }

        let mut dropped_files = Vec::new();
        if rl.is_file_dropped() {
            dropped_files = rl
                .load_dropped_files()
                .paths()
                .iter()
                .map(PathBuf::from)
                .collect();
        }

        InputState {
            mouse,
            mouse_delta,
            left_pressed,
            left_down: rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT),
            left_released: rl.is_mouse_button_released(MouseButton::MOUSE_BUTTON_LEFT),
            right_pressed: rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_RIGHT),
            right_released: rl.is_mouse_button_released(MouseButton::MOUSE_BUTTON_RIGHT),
            middle_pressed: rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_MIDDLE),
            middle_down: rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_MIDDLE),
            middle_released: rl.is_mouse_button_released(MouseButton::MOUSE_BUTTON_MIDDLE),
            double_click,
            wheel: rl.get_mouse_wheel_move_v().into(),
            ctrl,
            shift,
            alt,
            meta,
            keys,
            chars,
            dropped_files,
            mouse_blocked: false,
        }
    }
}

/// Kanonischer Tastenname (lowercase, `KeyboardEvent.key`-Stil) für das
/// Shortcut-System.
pub fn key_name(key: KeyboardKey, shift: bool) -> Option<&'static str> {
    use KeyboardKey::*;
    Some(match key {
        KEY_A => "a", KEY_B => "b", KEY_C => "c", KEY_D => "d", KEY_E => "e",
        KEY_F => "f", KEY_G => "g", KEY_H => "h", KEY_I => "i", KEY_J => "j",
        KEY_K => "k", KEY_L => "l", KEY_M => "m", KEY_N => "n", KEY_O => "o",
        KEY_P => "p", KEY_Q => "q", KEY_R => "r", KEY_S => "s", KEY_T => "t",
        KEY_U => "u", KEY_V => "v", KEY_W => "w", KEY_X => "x", KEY_Y => "y",
        KEY_Z => "z",
        // KEY_ZERO liefert IMMER "0" (wie KEY_ONE..KEY_NINE), damit shift-Ziffern-
        // Chords wie "Shift+0" (multicam.toggleMonitor) und "Alt+Shift+0"
        // (workspace.resetLayout) überhaupt erzeugbar sind. "=" kommt über KEY_EQUAL.
        KEY_ZERO => "0",
        KEY_ONE => "1", KEY_TWO => "2", KEY_THREE => "3", KEY_FOUR => "4",
        KEY_FIVE => "5", KEY_SIX => "6", KEY_SEVEN => "7", KEY_EIGHT => "8",
        KEY_NINE => "9",
        KEY_SPACE => "space",
        KEY_ENTER => "enter",
        KEY_KP_ENTER => "enter",
        KEY_ESCAPE => "escape",
        KEY_BACKSPACE => "backspace",
        KEY_DELETE => "delete",
        KEY_TAB => "tab",
        KEY_LEFT => "arrowleft",
        KEY_RIGHT => "arrowright",
        KEY_UP => "arrowup",
        KEY_DOWN => "arrowdown",
        KEY_HOME => "home",
        KEY_END => "end",
        KEY_PAGE_UP => "pageup",
        KEY_PAGE_DOWN => "pagedown",
        KEY_INSERT => "insert",
        // deutsches Layout: Plus/Minus über = und - (US-Scancodes via GLFW)
        KEY_EQUAL => if shift { "+" } else { "=" },
        KEY_MINUS => if shift { "_" } else { "-" },
        KEY_KP_ADD => "+",
        KEY_KP_SUBTRACT => "-",
        KEY_COMMA => if shift { "<" } else { "," },
        KEY_PERIOD => if shift { ">" } else { "." },
        KEY_SLASH => if shift { "?" } else { "/" },
        KEY_SEMICOLON => if shift { ":" } else { ";" },
        KEY_APOSTROPHE => "'",
        KEY_BACKSLASH => "\\",
        KEY_LEFT_BRACKET => "[",
        KEY_RIGHT_BRACKET => "]",
        KEY_GRAVE => "`",
        KEY_F1 => "f1", KEY_F2 => "f2", KEY_F3 => "f3", KEY_F4 => "f4",
        KEY_F5 => "f5", KEY_F6 => "f6", KEY_F7 => "f7", KEY_F8 => "f8",
        KEY_F9 => "f9", KEY_F10 => "f10", KEY_F11 => "f11", KEY_F12 => "f12",
        _ => return None,
    })
}
