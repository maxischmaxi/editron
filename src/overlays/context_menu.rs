//! App-eigenes Kontextmenü: Items referenzieren Commands — Label (Fallback), Shortcut-Anzeige und
//! Enabled-Zustand kommen aus Registry + Keymap.

use crate::core::commands::CommandRegistry;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::drop_shadow;
use crate::ui::{FontKind, Ui};
use raylib::consts::{KeyboardKey, MouseCursor};

/// Panel-lokale Menüaktionen (mit gebundenen Argumenten, ohne Command-Registrierung).
#[derive(Clone, Debug)]
pub enum CustomAction {
    TimelineSplitAt { t: f64, clip_id: String },
    TimelinePasteAt { t: f64 },
    TimelineToggleTrackFlag { track_id: String, flag: crate::core::timeline::TrackFlag },
    /// Source-Patch-Ziel der Spurart umschalten (Radio).
    TimelineToggleSourcePatch { track_id: String },
    TimelineSetInAt { t: f64 },
    TimelineSetOutAt { t: f64 },
    TimelineClearInOut,
    MediaShowInBrowser { asset_id: String },
    /// Interpolation ausgewählter Keyframes setzen: (Clip, Parameter, Medienzeit).
    FxSetInterp {
        keys: Vec<(String, crate::core::animation::ParamRef, f64)>,
        interp: crate::core::animation::Interp,
    },
    FxRemoveKeyframes {
        keys: Vec<(String, crate::core::animation::ParamRef, f64)>,
    },
    /// Einzelnen Parameter auf den Standardwert zurücksetzen.
    FxResetParam {
        clip_id: String,
        pref: crate::core::animation::ParamRef,
    },
    /// Effekt aus dem Katalog auf Clips anwenden.
    EffectsApplyToClips {
        kind: crate::core::effects::EffectKind,
        clip_ids: Vec<String>,
    },
    /// Effekt-Instanz im Stapel verschieben (−1 = nach oben, +1 = nach unten).
    EffectsMove { clip_id: String, fx_id: String, delta: i32 },
    /// Effekt-Instanz vom Clip entfernen.
    EffectsRemove { clip_id: String, fx_id: String },
    /// Bypass einer Effekt-Instanz umschalten.
    EffectsToggle { clip_id: String, fx_id: String },
    /// Alle Parameter einer Effekt-Instanz zurücksetzen.
    EffectsReset { clip_id: String, fx_id: String },
    /// Audio-Effekt an die Bus-Kette einer Spur anhängen.
    TrackEffectsAdd {
        track_id: String,
        kind: crate::core::effects::EffectKind,
    },
    /// Bus-Effekt einer Spur entfernen.
    TrackEffectsRemove { track_id: String, fx_id: String },
    /// Bypass eines Bus-Effekts umschalten.
    TrackEffectsToggle { track_id: String, fx_id: String },
    /// Bus-Effekt einer Spur zurücksetzen.
    TrackEffectsReset { track_id: String, fx_id: String },
    /// Bus-Effekt im Stapel verschieben (−1 = hoch, +1 = runter).
    TrackEffectsMove { track_id: String, fx_id: String, delta: i32 },
    /// Spur-Automation eines Parameters löschen (Fader gilt wieder).
    TrackAutoClear {
        track_id: String,
        param: crate::core::timeline::TrackAutoParam,
    },
    /// Übergang entfernen.
    TransitionRemove { id: String },
    /// Übergangsart ersetzen.
    TransitionReplace { id: String, kind: crate::core::transitions::TransitionKind },
    /// Ausrichtung relativ zur Schnittkante setzen.
    TransitionAlign {
        id: String,
        alignment: crate::core::transitions::TransitionAlignment,
    },
    /// Richtung (Wischen/Schieben) setzen.
    TransitionDirection {
        id: String,
        direction: crate::core::transitions::TransitionDirection,
    },
    /// Dauer-Eingabe in der Timeline öffnen.
    TransitionEditDuration { id: String },
    /// Marker-Bearbeiten-Dialog für ein Ziel öffnen.
    MarkerEdit {
        scope: crate::core::marker::MarkerScope,
        marker_id: String,
    },
    /// Marker entfernen.
    MarkerDelete {
        scope: crate::core::marker::MarkerScope,
        marker_id: String,
    },
    /// Markerfarbe setzen.
    MarkerSetColor {
        scope: crate::core::marker::MarkerScope,
        marker_id: String,
        color: crate::core::marker::MarkerColor,
    },
    /// Sequenz-Marker an einer Lineal-Zeit hinzufügen.
    MarkerAddAt { t: f64 },
    /// Clip-Marker an einer Sequenzzeit auf einem bestimmten Clip hinzufügen.
    MarkerAddClipAt { clip_id: String, t: f64 },
}

#[derive(Clone)]
pub struct MenuItem {
    /// None ⇒ Titel aus der Command-Registry.
    pub label: Option<String>,
    pub icon: Option<&'static str>,
    pub command: Option<String>,
    pub custom: Option<CustomAction>,
    pub args: Option<serde_json::Value>,
    pub disabled: bool,
    pub danger: bool,
    pub checked: bool,
}

impl MenuItem {
    pub fn command(command: &str) -> MenuItem {
        MenuItem {
            label: None,
            icon: None,
            command: Some(command.to_string()),
            custom: None,
            args: None,
            disabled: false,
            danger: false,
            checked: false,
        }
    }

    pub fn custom(label: &str, action: CustomAction) -> MenuItem {
        MenuItem {
            label: Some(label.to_string()),
            icon: None,
            command: None,
            custom: Some(action),
            args: None,
            disabled: false,
            danger: false,
            checked: false,
        }
    }

    pub fn with_disabled(mut self, disabled: bool) -> MenuItem {
        self.disabled = disabled;
        self
    }

    pub fn with_label(mut self, label: &str) -> MenuItem {
        self.label = Some(label.to_string());
        self
    }

    pub fn with_icon(mut self, icon: &'static str) -> MenuItem {
        self.icon = Some(icon);
        self
    }

    pub fn with_args(mut self, args: serde_json::Value) -> MenuItem {
        self.args = Some(args);
        self
    }

    pub fn with_checked(mut self, checked: bool) -> MenuItem {
        self.checked = checked;
        self
    }

    pub fn with_danger(mut self) -> MenuItem {
        self.danger = true;
        self
    }
}

#[derive(Clone)]
pub enum MenuEntry {
    Item(MenuItem),
    Separator,
    Submenu {
        label: String,
        icon: Option<&'static str>,
        items: Vec<MenuEntry>,
    },
}

#[derive(Default)]
pub struct ContextMenuState {
    pub open: bool,
    pub x: f32,
    pub y: f32,
    pub items: Vec<MenuEntry>,
    /// Aktiver (gehoverter) Index je Menü-Ebene; Länge = Tiefe.
    active_path: Vec<i32>,
    /// Offenes Submenü je Ebene.
    open_sub: Vec<i32>,
    just_opened: bool,
}

impl ContextMenuState {
    pub fn show(&mut self, x: f32, y: f32, items: Vec<MenuEntry>) {
        if items.is_empty() {
            return;
        }
        self.open = true;
        self.x = x;
        self.y = y;
        self.items = items;
        self.active_path = vec![-1];
        self.open_sub = vec![-1];
        self.just_opened = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.items.clear();
        self.active_path.clear();
        self.open_sub.clear();
    }
}

const ROW_H: f32 = 28.0; // h-7
const PAD_X: f32 = 10.0; // px-2.5
const MIN_W: f32 = 208.0; // min-w-52

struct MenuMetrics {
    w: f32,
    h: f32,
}

fn item_disabled(item: &MenuItem, registry: &CommandRegistry, state: &AppState) -> bool {
    if item.disabled {
        return true;
    }
    if item.custom.is_some() {
        return false;
    }
    if let Some(cmd) = &item.command {
        return !registry.is_enabled(cmd, state);
    }
    false
}

fn item_label<'a>(item: &'a MenuItem, registry: &'a CommandRegistry) -> &'a str {
    if let Some(label) = &item.label {
        return label;
    }
    item.command
        .as_deref()
        .and_then(|id| registry.get(id).map(|c| c.title.as_str()))
        .unwrap_or("?")
}

fn shortcut_for(item: &MenuItem, state: &AppState) -> Option<String> {
    item.command
        .as_deref()
        .and_then(|id| state.keymap.shortcut_for(id))
}

fn measure_menu(
    ui: &Ui,
    items: &[MenuEntry],
    registry: &CommandRegistry,
    state: &AppState,
) -> MenuMetrics {
    let font = ui.font(FontKind::Sans12);
    let mono = ui.font(FontKind::Mono12);
    let mut w: f32 = MIN_W;
    let mut h: f32 = 8.0; // py-1
    for entry in items {
        match entry {
            MenuEntry::Separator => h += 9.0, // my-1 + h-px
            MenuEntry::Submenu { label, .. } => {
                h += ROW_H;
                let lw = PAD_X * 2.0 + 16.0 + 8.0 + font.width(label) + 8.0 + 14.0;
                w = w.max(lw);
            }
            MenuEntry::Item(item) => {
                h += ROW_H;
                let label = item_label(item, registry);
                let mut lw = PAD_X * 2.0 + 16.0 + 8.0 + font.width(label);
                if let Some(sc) = shortcut_for(item, state) {
                    lw += 16.0 + mono.width(&sc);
                }
                w = w.max(lw);
            }
        }
    }
    MenuMetrics { w, h }
}

/// Ausgeführter Menübefehl als Ergebnis des Overlay-Passes.
pub enum MenuAction {
    Command {
        command: String,
        args: Option<serde_json::Value>,
    },
    Custom(CustomAction),
}

fn item_action(item: &MenuItem) -> Option<MenuAction> {
    if let Some(custom) = &item.custom {
        return Some(MenuAction::Custom(custom.clone()));
    }
    item.command.as_ref().map(|cmd| MenuAction::Command {
        command: cmd.clone(),
        args: item.args.clone(),
    })
}

/// Zeichnet das offene Kontextmenü (Overlay-Pass). `menu` ist per
/// `std::mem::take` aus dem AppState entnommen (Borrow-Trennung).
pub fn render(
    ui: &mut Ui,
    menu: &mut ContextMenuState,
    registry: &CommandRegistry,
    state: &AppState,
) -> Option<MenuAction> {
    if !menu.open {
        return None;
    }

    // Tastatur: Escape schließt, Pfeile navigieren, Enter führt aus.
    let keys: Vec<KeyboardKey> = ui.input.keys.iter().map(|k| k.key).collect();
    if keys.contains(&KeyboardKey::KEY_ESCAPE) {
        menu.close();
        return None;
    }

    let metrics = measure_menu(ui, &menu.items, registry, state);
    let left = menu
        .x
        .min(ui.screen.right() - metrics.w - 4.0)
        .max(4.0);
    let top = menu
        .y
        .min(ui.screen.bottom() - metrics.h - 4.0)
        .max(4.0);

    let mut action: Option<MenuAction> = None;
    let mut close_requested = false;
    let mut mouse_in_any = false;

    render_list(
        ui,
        menu,
        0,
        Rect::new(left, top, metrics.w, metrics.h),
        registry,
        state,
        &keys,
        &mut action,
        &mut close_requested,
        &mut mouse_in_any,
    );

    // Klick außerhalb aller Menü-Ebenen schließt (auch Rechtsklick).
    if !menu.just_opened
        && (ui.input.left_pressed || ui.input.right_pressed)
        && !mouse_in_any
    {
        close_requested = true;
    }
    menu.just_opened = false;

    if action.is_some() || close_requested {
        menu.close();
    }
    action
}

/// Eine Menü-Ebene zeichnen; Submenüs rekursiv.
#[allow(clippy::too_many_arguments)]
fn render_list(
    ui: &mut Ui,
    menu: &mut ContextMenuState,
    depth: usize,
    rect: Rect,
    registry: &CommandRegistry,
    state: &AppState,
    keys: &[KeyboardKey],
    action: &mut Option<MenuAction>,
    close_requested: &mut bool,
    mouse_in_any: &mut bool,
) {
    // Items dieser Ebene ermitteln (Pfad durch die open_sub-Kette).
    let items: Vec<MenuEntry> = {
        let mut current: &[MenuEntry] = &menu.items;
        for level in 0..depth {
            let idx = menu.open_sub.get(level).copied().unwrap_or(-1);
            if idx < 0 {
                return;
            }
            match current.get(idx as usize) {
                Some(MenuEntry::Submenu { items, .. }) => current = items,
                _ => return,
            }
        }
        current.to_vec()
    };

    while menu.active_path.len() <= depth {
        menu.active_path.push(-1);
    }
    while menu.open_sub.len() <= depth {
        menu.open_sub.push(-1);
    }

    if ui.mouse_in(rect) || rect.contains(ui.input.mouse) {
        *mouse_in_any = true;
    }

    drop_shadow(ui, rect, theme::RADIUS_MD);
    ui.fill_rounded(rect, theme::RADIUS_MD, theme::SURFACE_2);
    ui.stroke_rounded(rect, theme::RADIUS_MD, 1.0, theme::LINE);

    // Tastatur-Navigation wirkt auf die tiefste offene Ebene.
    let deepest = menu.open_sub.iter().take_while(|i| **i >= 0).count();
    let keyboard_here = depth == deepest;
    if keyboard_here {
        let selectable: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, e)| match e {
                MenuEntry::Submenu { .. } => true,
                MenuEntry::Item(item) => !item_disabled(item, registry, state),
                MenuEntry::Separator => false,
            })
            .map(|(i, _)| i)
            .collect();
        let active = menu.active_path[depth];
        if (keys.contains(&KeyboardKey::KEY_DOWN) || keys.contains(&KeyboardKey::KEY_UP))
            && !selectable.is_empty() {
                let dir: i32 = if keys.contains(&KeyboardKey::KEY_DOWN) { 1 } else { -1 };
                let pos = selectable.iter().position(|i| *i as i32 == active);
                let next = match pos {
                    None => {
                        if dir == 1 {
                            selectable[0]
                        } else {
                            selectable[selectable.len() - 1]
                        }
                    }
                    Some(p) => {
                        let len = selectable.len() as i32;
                        selectable[((p as i32 + dir + len) % len) as usize]
                    }
                };
                menu.active_path[depth] = next as i32;
                menu.open_sub[depth] = -1;
            }
        if (keys.contains(&KeyboardKey::KEY_ENTER) || keys.contains(&KeyboardKey::KEY_RIGHT))
            && active >= 0 {
                match items.get(active as usize) {
                    Some(MenuEntry::Submenu { .. }) => {
                        menu.open_sub[depth] = active;
                        while menu.active_path.len() <= depth + 1 {
                            menu.active_path.push(-1);
                        }
                        menu.active_path[depth + 1] = -1;
                    }
                    Some(MenuEntry::Item(item)) if keys.contains(&KeyboardKey::KEY_ENTER) => {
                        if !item_disabled(item, registry, state) {
                            match item_action(item) {
                                Some(a) => *action = Some(a),
                                None => *close_requested = true,
                            }
                        }
                    }
                    _ => {}
                }
            }
    }

    let mut y = rect.y + 4.0; // py-1
    let mut submenu_anchor: Option<(usize, Rect)> = None;

    for (idx, entry) in items.iter().enumerate() {
        match entry {
            MenuEntry::Separator => {
                // mx-2 my-1 h-px bg-line
                ui.fill(
                    Rect::new(rect.x + 8.0, y + 4.0, rect.w - 16.0, 1.0),
                    theme::LINE,
                );
                y += 9.0;
            }
            MenuEntry::Submenu { label, icon, .. } => {
                let row = Rect::new(rect.x, y, rect.w, ROW_H);
                let hovered = ui.mouse_in(row) || row.contains(ui.input.mouse);
                if hovered {
                    menu.active_path[depth] = idx as i32;
                    menu.open_sub[depth] = idx as i32;
                    // Tiefere Ebenen zurücksetzen
                    for level in depth + 1..menu.open_sub.len() {
                        menu.open_sub[level] = -1;
                        menu.active_path[level] = -1;
                    }
                }
                let active = menu.active_path[depth] == idx as i32;
                if active {
                    ui.fill(row, theme::SURFACE_3);
                }
                let mut inner = row.inset_xy(PAD_X, 0.0);
                let icon_cell = inner.cut_left(16.0);
                if let Some(icon) = icon {
                    ui.icon(icon, icon_cell, 14.0, theme::TEXT_2);
                }
                inner.cut_left(8.0);
                let chevron = inner.cut_right(14.0);
                ui.text_left(label, inner, theme::TEXT_1, FontKind::Sans12);
                ui.icon("chevron-right", chevron, 14.0, theme::TEXT_3);

                if menu.open_sub[depth] == idx as i32 {
                    submenu_anchor = Some((idx, row));
                }
                y += ROW_H;
            }
            MenuEntry::Item(item) => {
                let row = Rect::new(rect.x, y, rect.w, ROW_H);
                let disabled = item_disabled(item, registry, state);
                let hovered = (ui.mouse_in(row) || row.contains(ui.input.mouse)) && !disabled;
                if hovered {
                    menu.active_path[depth] = idx as i32;
                    menu.open_sub[depth] = -1;
                    ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                    if ui.input.left_pressed {
                        match item_action(item) {
                            Some(a) => *action = Some(a),
                            None => *close_requested = true,
                        }
                    }
                }
                let active = menu.active_path[depth] == idx as i32 && !disabled;
                if active {
                    ui.fill(row, theme::SURFACE_3);
                }
                let fg = if disabled {
                    theme::TEXT_3
                } else if item.danger {
                    theme::DANGER
                } else {
                    theme::TEXT_1
                };
                let mut inner = row.inset_xy(PAD_X, 0.0);
                let icon_cell = inner.cut_left(16.0);
                if item.checked {
                    ui.icon("check", icon_cell, 14.0, fg);
                } else if let Some(icon) = item.icon {
                    ui.icon(
                        icon,
                        icon_cell,
                        14.0,
                        if disabled { fg } else { theme::TEXT_2 },
                    );
                }
                inner.cut_left(8.0);
                if let Some(sc) = shortcut_for(item, state) {
                    let mono = ui.font(FontKind::Mono12);
                    let sc_w = mono.width(&sc);
                    let sc_cell = inner.cut_right(sc_w);
                    ui.text_left(&sc, sc_cell, theme::TEXT_3, FontKind::Mono12);
                    inner.cut_right(16.0); // pl-4
                }
                let label = item_label(item, registry);
                let text = ui.font(FontKind::Sans12).ellipsize(label, inner.w);
                ui.text_left(&text, inner, fg, FontKind::Sans12);
                y += ROW_H;
            }
        }
    }

    // Offenes Submenü dieser Ebene zeichnen (left-full -ml-1 -mt-1.5).
    if let Some((idx, anchor_row)) = submenu_anchor {
        if let Some(MenuEntry::Submenu { items: sub_items, .. }) = items.get(idx) {
            let m = measure_menu(ui, sub_items, registry, state);
            let mut sx = rect.right() - 4.0;
            let mut sy = anchor_row.y - 6.0;
            if sx + m.w > ui.screen.right() - 4.0 {
                sx = (rect.x - m.w + 4.0).max(4.0);
            }
            sy = sy.min(ui.screen.bottom() - m.h - 4.0).max(4.0);
            render_list(
                ui,
                menu,
                depth + 1,
                Rect::new(sx, sy, m.w, m.h),
                registry,
                state,
                keys,
                action,
                close_requested,
                mouse_in_any,
            );
        }
    }
}
