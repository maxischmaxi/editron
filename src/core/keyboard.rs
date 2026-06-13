//! Shortcut-System: Keybinding-Parser ("Mod+Shift+K", Sequenzen
//! "Mod+K Mod+S"), Resolver mit Sequenz-Puffer, Presets
//! editron/premiere/resolve, Nutzer-Overrides mit JSON-Persistenz.

use crate::core::commands::{evaluate_when, CommandRegistry};
use crate::state::AppState;
use crate::ui::input::{key_name, KeyPress};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

pub const IS_MACOS: bool = cfg!(target_os = "macos");

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct KeyChord {
    /// Normalisierte Taste, z. B. "k", "space", "arrowleft", "+"
    pub key: String,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Keybinding {
    pub command: String,
    pub keys: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}

impl Keybinding {
    fn new(command: &str, keys: &str) -> Keybinding {
        Keybinding {
            command: command.into(),
            keys: keys.into(),
            when: None,
            args: None,
        }
    }

    fn when(command: &str, keys: &str, when: &str) -> Keybinding {
        Keybinding {
            command: command.into(),
            keys: keys.into(),
            when: Some(when.into()),
            args: None,
        }
    }
}

pub struct KeymapPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub bindings: Vec<Keybinding>,
}

// ------------------------------------------------------------------ Parsen

fn key_alias(lower: &str) -> &str {
    match lower {
        "esc" => "escape",
        "del" | "entf" => "delete",
        "return" => "enter",
        "spacebar" | "leertaste" => "space",
        "plus" => "+",
        "minus" => "-",
        "left" => "arrowleft",
        "right" => "arrowright",
        "up" => "arrowup",
        "down" => "arrowdown",
        "pos1" => "home",
        "pgup" => "pageup",
        "pgdn" => "pagedown",
        other => other,
    }
}

pub fn normalize_key(key: &str) -> String {
    if key == " " {
        return "space".into();
    }
    key_alias(&key.to_lowercase()).to_string()
}

/// Parst einen einzelnen Chord wie "Mod+Shift+K" oder "Mod++".
fn parse_chord(raw: &str) -> Option<KeyChord> {
    let mut tokens: Vec<&str> = raw.split('+').collect();
    let mut key_token = tokens.pop().unwrap_or("");
    if key_token.is_empty() {
        // "Mod++" → ["Mod", "", ""], "+" → ["", ""]
        key_token = "+";
        if tokens.last() == Some(&"") {
            tokens.pop();
        }
    }
    let mut chord = KeyChord {
        key: String::new(),
        ctrl: false,
        shift: false,
        alt: false,
        meta: false,
    };
    for token in tokens {
        match token.trim().to_lowercase().as_str() {
            "mod" => {
                if IS_MACOS {
                    chord.meta = true;
                } else {
                    chord.ctrl = true;
                }
            }
            "ctrl" | "control" | "strg" => chord.ctrl = true,
            "alt" | "option" | "opt" => chord.alt = true,
            "shift" | "umschalt" => chord.shift = true,
            "meta" | "cmd" | "command" | "super" | "win" => chord.meta = true,
            _ => {
                eprintln!("[keyboard] Unbekannter Modifier \"{token}\" in \"{raw}\"");
                return None;
            }
        }
    }
    chord.key = normalize_key(key_token.trim());
    if chord.key.is_empty() {
        return None;
    }
    Some(chord)
}

/// Parst einen serialisierten Keybinding-String in eine Chord-Sequenz.
pub fn parse_keys(keys: &str) -> Vec<KeyChord> {
    keys.split_whitespace().filter_map(parse_chord).collect()
}

/// Chord aus einem Tastendruck.
pub fn chord_from_keypress(press: &KeyPress) -> Option<KeyChord> {
    let key = key_name(press.key, press.shift)?;
    Some(KeyChord {
        key: key.to_string(),
        ctrl: press.ctrl,
        shift: press.shift,
        alt: press.alt,
        meta: press.meta,
    })
}

pub fn sequence_starts_with(sequence: &[KeyChord], prefix: &[KeyChord]) -> bool {
    prefix.len() <= sequence.len() && prefix.iter().zip(sequence).all(|(a, b)| a == b)
}

// ------------------------------------------------------------ Formatierung

fn canonical_key_name(key: &str) -> String {
    if key.chars().count() == 1 {
        return key.to_uppercase();
    }
    match key {
        "arrowleft" => "ArrowLeft".into(),
        "arrowright" => "ArrowRight".into(),
        "arrowup" => "ArrowUp".into(),
        "arrowdown" => "ArrowDown".into(),
        "pageup" => "PageUp".into(),
        "pagedown" => "PageDown".into(),
        _ => {
            if key.len() <= 3 && key.starts_with('f') && key[1..].parse::<u8>().is_ok() {
                return key.to_uppercase();
            }
            let mut c = key.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        }
    }
}

/// Serialisiert einen Chord zurück ins Keybinding-Format ("Mod+Shift+K").
pub fn serialize_chord(chord: &KeyChord) -> String {
    let mut parts: Vec<String> = Vec::new();
    if IS_MACOS {
        if chord.ctrl {
            parts.push("Ctrl".into());
        }
        if chord.alt {
            parts.push("Alt".into());
        }
        if chord.shift {
            parts.push("Shift".into());
        }
        if chord.meta {
            parts.push("Mod".into());
        }
    } else {
        if chord.ctrl {
            parts.push("Mod".into());
        }
        if chord.alt {
            parts.push("Alt".into());
        }
        if chord.shift {
            parts.push("Shift".into());
        }
        if chord.meta {
            parts.push("Meta".into());
        }
    }
    parts.push(canonical_key_name(&chord.key));
    parts.join("+")
}

pub fn serialize_chords(chords: &[KeyChord]) -> String {
    chords
        .iter()
        .map(serialize_chord)
        .collect::<Vec<_>>()
        .join(" ")
}

fn display_key_name(key: &str) -> String {
    if key.chars().count() == 1 {
        return key.to_uppercase();
    }
    match key {
        "space" => "Leertaste".into(),
        "enter" => "Eingabe".into(),
        "escape" => "Esc".into(),
        "backspace" => "Rücktaste".into(),
        "delete" => "Entf".into(),
        "insert" => "Einfg".into(),
        "tab" => "Tab".into(),
        "home" => "Pos1".into(),
        "end" => "Ende".into(),
        "pageup" => "Bild↑".into(),
        "pagedown" => "Bild↓".into(),
        "arrowleft" => "←".into(),
        "arrowright" => "→".into(),
        "arrowup" => "↑".into(),
        "arrowdown" => "↓".into(),
        _ => canonical_key_name(key),
    }
}

/// Formatiert einen Chord für die Anzeige ("Strg+Umschalt+K" bzw. "⇧⌘K").
pub fn format_chord(chord: &KeyChord) -> String {
    let key_label = display_key_name(&chord.key);
    if IS_MACOS {
        let mut out = String::new();
        if chord.ctrl {
            out.push('⌃');
        }
        if chord.alt {
            out.push('⌥');
        }
        if chord.shift {
            out.push('⇧');
        }
        if chord.meta {
            out.push('⌘');
        }
        return out + &key_label;
    }
    let mut parts: Vec<String> = Vec::new();
    if chord.ctrl {
        parts.push("Strg".into());
    }
    if chord.alt {
        parts.push("Alt".into());
    }
    if chord.shift {
        parts.push("Umschalt".into());
    }
    if chord.meta {
        parts.push("Meta".into());
    }
    parts.push(key_label);
    parts.join("+")
}

pub fn format_keys(keys: &str) -> String {
    parse_keys(keys)
        .iter()
        .map(format_chord)
        .collect::<Vec<_>>()
        .join(" ")
}

// ------------------------------------------------------------------ Presets

pub fn presets() -> Vec<KeymapPreset> {
    vec![editron_preset(), premiere_preset(), resolve_preset()]
}

fn shared_timeline_bindings() -> Vec<Keybinding> {
    vec![
        // Standardübergänge wie Premiere: Mod+D Video, Mod+Shift+D Audio.
        Keybinding::new("clip.applyDefaultVideoTransition", "Mod+D"),
        Keybinding::new("clip.applyDefaultAudioTransition", "Mod+Shift+D"),
        // Geschwindigkeit/Dauer wie Premiere: Mod+R.
        Keybinding::new("clip.speedDuration", "Mod+R"),
        // Titel am Playhead (Premiere: Strg+T = neue Textebene).
        Keybinding::new("title.add", "Mod+T"),
        // Untertitel am Playhead (Untertitel-Workflow, alle Presets gleich).
        Keybinding::new("subtitle.addAtPlayhead", "Mod+U"),
        Keybinding::new("subtitle.importSrt", "Mod+Alt+U"),
        Keybinding::new("subtitle.exportSrt", "Mod+Shift+U"),
        Keybinding::when("timeline.deleteSelected", "Delete", "panel == 'timeline'"),
        Keybinding::when("timeline.deleteSelected", "Backspace", "panel == 'timeline'"),
        Keybinding::when("timeline.rippleDelete", "Shift+Delete", "panel == 'timeline'"),
        Keybinding::when("timeline.rippleDelete", "Shift+Backspace", "panel == 'timeline'"),
        Keybinding::when("timeline.selectAll", "Mod+A", "panel == 'timeline'"),
        Keybinding::when("timeline.deselectAll", "Mod+Shift+A", "panel == 'timeline'"),
        // Three-Point-Editing wie Premiere (alle Presets): Einfügen = Komma,
        // Überschreiben = Punkt, Lift = Semikolon, Extract = Apostroph,
        // Match Frame = F. Bewusst global (tastaturgetrieben, panel-unabhängig).
        Keybinding::new("timeline.insert", ","),
        Keybinding::new("timeline.overwrite", "."),
        Keybinding::new("timeline.liftRange", ";"),
        Keybinding::new("timeline.extractRange", "'"),
        Keybinding::new("timeline.matchFrame", "F"),
        Keybinding::when("timeline.copy", "Mod+C", "panel == 'timeline'"),
        Keybinding::when("timeline.cut", "Mod+X", "panel == 'timeline'"),
        Keybinding::when("timeline.paste", "Mod+V", "panel == 'timeline'"),
        Keybinding::when("timeline.goToStart", "Home", "panel == 'timeline'"),
        Keybinding::when("timeline.goToEnd", "End", "panel == 'timeline'"),
        Keybinding::when("timeline.stepBackward", "ArrowLeft", "panel == 'timeline'"),
        Keybinding::when("timeline.stepForward", "ArrowRight", "panel == 'timeline'"),
        Keybinding::when("timeline.stepBackward5", "Shift+ArrowLeft", "panel == 'timeline'"),
        Keybinding::when("timeline.stepForward5", "Shift+ArrowRight", "panel == 'timeline'"),
        Keybinding::when("timeline.prevEdit", "ArrowUp", "panel == 'timeline'"),
        Keybinding::when("timeline.nextEdit", "ArrowDown", "panel == 'timeline'"),
        Keybinding::when("clip.prevKeyframe", "Shift+ArrowUp", "panel == 'timeline'"),
        Keybinding::when("clip.nextKeyframe", "Shift+ArrowDown", "panel == 'timeline'"),
        // Marker — in allen Presets gleich: M setzt latenzfrei am Playhead
        // (Logging-Workflow), Shift+M öffnet den Bearbeiten-Dialog,
        // Shift+Bild↓/↑ springt zum nächsten/vorherigen Marker.
        Keybinding::new("marker.add", "M"),
        Keybinding::new("marker.addDialog", "Shift+M"),
        Keybinding::new("marker.next", "Shift+PageDown"),
        Keybinding::new("marker.prev", "Shift+PageUp"),
        Keybinding::when("marker.deleteAtPlayhead", "Mod+Alt+M", "panel == 'timeline'"),
        Keybinding::new("markers.openPanel", "Mod+Shift+M"),
    ]
}

/// Projektdatei-Shortcuts — in allen Presets identisch (NLE-Konvention).
fn project_bindings() -> Vec<Keybinding> {
    vec![
        Keybinding::new("project.new", "Mod+N"),
        Keybinding::new("project.open", "Mod+O"),
        Keybinding::new("project.save", "Mod+S"),
        Keybinding::new("project.saveAs", "Mod+Shift+S"),
    ]
}

fn playback_bindings(clear_in_out: &str) -> Vec<Keybinding> {
    vec![
        Keybinding::new("playback.togglePlay", "Space"),
        Keybinding::new("playback.shuttleReverse", "J"),
        Keybinding::new("playback.shuttleStop", "K"),
        Keybinding::new("playback.shuttleForward", "L"),
        Keybinding::new("playback.stepBackward", "ArrowLeft"),
        Keybinding::new("playback.stepForward", "ArrowRight"),
        Keybinding::new("playback.stepBackward5", "Shift+ArrowLeft"),
        Keybinding::new("playback.stepForward5", "Shift+ArrowRight"),
        Keybinding::new("playback.goToStart", "Home"),
        Keybinding::new("playback.goToEnd", "End"),
        Keybinding::new("playback.setInPoint", "I"),
        Keybinding::new("playback.setOutPoint", "O"),
        Keybinding::new("playback.clearInOut", clear_in_out),
    ]
}

/// Editron-Standard-Keymap (NLE-Konventionen + Editron-Eigenheiten).
fn editron_preset() -> KeymapPreset {
    let mut bindings = playback_bindings("Mod+Shift+X");
    bindings.extend([
        // Werkzeuge
        Keybinding::new("tools.select", "V"),
        Keybinding::new("tools.razor", "C"),
        Keybinding::new("tools.ripple", "B"),
        Keybinding::new("tools.rolling", "N"),
        Keybinding::new("tools.slip", "Y"),
        Keybinding::new("tools.slide", "U"),
        Keybinding::new("tools.hand", "H"),
        Keybinding::new("tools.zoom", "Z"),
        // Timeline — "=" zusätzlich zu "+", weil "+" auf US-Layouts
        // nur über Shift+"=" erreichbar ist.
        Keybinding::new("timeline.zoomIn", "+"),
        Keybinding::new("timeline.zoomIn", "="),
        Keybinding::new("timeline.zoomOut", "-"),
        Keybinding::new("timeline.zoomFit", "Shift+Z"),
        Keybinding::new("timeline.toggleSnapping", "S"),
        // Bearbeitung — Schneiden bewusst auf Mod+B (nicht Mod+K):
        // Mod+K ist Präfix der Sequenz "Mod+K Mod+S".
        Keybinding::new("edit.undo", "Mod+Z"),
        Keybinding::new("edit.redo", "Mod+Shift+Z"),
        Keybinding::new("edit.redo", "Mod+Y"),
        Keybinding::new("timeline.splitAtPlayhead", "Mod+B"),
        Keybinding::new("timeline.toggleLink", "Mod+L"),
        Keybinding::new("timeline.toggleClipEnabled", "Shift+E"),
        // Farbe — Shift+D wie Resolves „Bypass Color Grades“.
        Keybinding::new("clip.toggleGrade", "Shift+D"),
        Keybinding::new("clip.resetGrade", "Mod+Shift+R"),
        // Effekte + Attribute (Premiere: Strg+Alt+V = Attribute einfügen).
        Keybinding::new("clip.toggleEffects", "Mod+Shift+E"),
        Keybinding::new("clip.copyAttributes", "Mod+Alt+C"),
        Keybinding::new("clip.pasteAttributes", "Mod+Alt+V"),
        // Audio — "=" zusätzlich zu "+" (siehe Zoom-Kommentar oben).
        Keybinding::new("timeline.clipGainUp", "Alt++"),
        Keybinding::new("timeline.clipGainUp", "Alt+="),
        Keybinding::new("timeline.clipGainDown", "Alt+-"),
    ]);
    bindings.extend(shared_timeline_bindings());
    bindings.extend(project_bindings());
    bindings.extend([
        // Medien
        Keybinding::new("media.import", "Mod+I"),
        Keybinding::when("media.removeSelected", "Delete", "panel == 'media'"),
        // Anwendung
        Keybinding::new("app.export", "Mod+E"),
        Keybinding::new("sequence.settings", "Mod+Alt+S"),
        Keybinding::new("app.commandPalette", "Mod+Shift+P"),
        Keybinding::new("app.shortcutEditor", "Mod+K Mod+S"),
        // Workspaces
        Keybinding::new("workspace.switch.media", "Alt+1"),
        Keybinding::new("workspace.switch.edit", "Alt+2"),
        Keybinding::new("workspace.switch.color", "Alt+3"),
        Keybinding::new("workspace.switch.effects", "Alt+4"),
        Keybinding::new("workspace.switch.audio", "Alt+5"),
        Keybinding::new("workspace.switch.graphics", "Alt+6"),
        Keybinding::new("workspace.next", "Mod+Alt+ArrowRight"),
        Keybinding::new("workspace.previous", "Mod+Alt+ArrowLeft"),
        Keybinding::new("workspace.resetLayout", "Alt+Shift+0"),
    ]);
    KeymapPreset {
        id: "editron",
        name: "Editron (Standard)",
        description: "Die Standard-Tastaturbelegung von Editron.",
        bindings,
    }
}

/// Keymap nach Adobe Premiere Pro (Default-Belegung).
fn premiere_preset() -> KeymapPreset {
    let mut bindings = playback_bindings("Mod+Shift+X");
    bindings.extend([
        Keybinding::new("tools.select", "V"),
        Keybinding::new("tools.razor", "C"),
        Keybinding::new("tools.ripple", "B"),
        Keybinding::new("tools.rolling", "N"),
        Keybinding::new("tools.slip", "Y"),
        Keybinding::new("tools.slide", "U"),
        Keybinding::new("tools.hand", "H"),
        Keybinding::new("tools.zoom", "Z"),
        Keybinding::new("timeline.zoomIn", "="),
        Keybinding::new("timeline.zoomIn", "+"),
        Keybinding::new("timeline.zoomOut", "-"),
        // Premiere: "Zoom to Sequence" = \ — Shift+Z zusätzlich.
        Keybinding::new("timeline.zoomFit", "\\"),
        Keybinding::new("timeline.zoomFit", "Shift+Z"),
        Keybinding::new("timeline.toggleSnapping", "S"),
        Keybinding::new("edit.undo", "Mod+Z"),
        Keybinding::new("edit.redo", "Mod+Shift+Z"),
        Keybinding::new("timeline.splitAtPlayhead", "Mod+K"),
        Keybinding::new("timeline.toggleLink", "Mod+L"),
        Keybinding::new("timeline.toggleClipEnabled", "Shift+E"),
        // Premiere: Clip-Lautstärke = [ / ]
        Keybinding::new("timeline.clipGainUp", "]"),
        Keybinding::new("timeline.clipGainDown", "["),
    ]);
    bindings.extend(shared_timeline_bindings());
    bindings.extend(project_bindings());
    bindings.extend([
        Keybinding::new("media.import", "Mod+I"),
        Keybinding::when("media.removeSelected", "Delete", "panel == 'media'"),
        Keybinding::when("media.removeSelected", "Backspace", "panel == 'media'"),
        // Premiere: Medien exportieren = Ctrl/Cmd+M, Tastaturbefehle = Ctrl+Alt+K
        Keybinding::new("app.export", "Mod+M"),
        Keybinding::new("sequence.settings", "Mod+Alt+S"),
        Keybinding::new("app.shortcutEditor", "Mod+Alt+K"),
        Keybinding::new("app.commandPalette", "Mod+Shift+P"),
        // Premiere: Alt+Shift+1..9 für Arbeitsbereiche
        Keybinding::new("workspace.switch.media", "Alt+Shift+1"),
        Keybinding::new("workspace.switch.edit", "Alt+Shift+2"),
        Keybinding::new("workspace.switch.color", "Alt+Shift+3"),
        Keybinding::new("workspace.switch.effects", "Alt+Shift+4"),
        Keybinding::new("workspace.switch.audio", "Alt+Shift+5"),
        Keybinding::new("workspace.switch.graphics", "Alt+Shift+6"),
        Keybinding::new("workspace.next", "Mod+Alt+ArrowRight"),
        Keybinding::new("workspace.previous", "Mod+Alt+ArrowLeft"),
        Keybinding::new("workspace.resetLayout", "Alt+Shift+0"),
    ]);
    KeymapPreset {
        id: "premiere",
        name: "Adobe Premiere Pro",
        description: "Tastaturbelegung nach den Standard-Shortcuts von Adobe Premiere Pro.",
        bindings,
    }
}

/// Keymap nach DaVinci Resolve (Default-Belegung).
fn resolve_preset() -> KeymapPreset {
    let mut bindings = playback_bindings("Alt+X");
    bindings.extend([
        // Werkzeuge / Bearbeitungs-Modi: A = Auswahl, B = Blade, T = Trim
        Keybinding::new("tools.select", "A"),
        Keybinding::new("tools.razor", "B"),
        Keybinding::new("tools.ripple", "T"),
        Keybinding::new("timeline.zoomIn", "Mod+="),
        Keybinding::new("timeline.zoomIn", "Mod++"),
        Keybinding::new("timeline.zoomOut", "Mod+-"),
        Keybinding::new("timeline.zoomFit", "Shift+Z"),
        Keybinding::new("timeline.toggleSnapping", "N"),
        Keybinding::new("edit.undo", "Mod+Z"),
        Keybinding::new("edit.redo", "Mod+Shift+Z"),
        Keybinding::new("timeline.splitAtPlayhead", "Mod+B"),
        Keybinding::new("timeline.splitAtPlayhead", "Mod+\\"),
        Keybinding::new("timeline.toggleLink", "Mod+Alt+L"),
        Keybinding::new("timeline.toggleClipEnabled", "D"),
    ]);
    bindings.extend(shared_timeline_bindings());
    bindings.extend(project_bindings());
    bindings.extend([
        Keybinding::new("media.import", "Mod+I"),
        Keybinding::when("media.removeSelected", "Delete", "panel == 'media'"),
        Keybinding::when("media.removeSelected", "Backspace", "panel == 'media'"),
        Keybinding::new("app.shortcutEditor", "Mod+Alt+K"),
        Keybinding::new("app.export", "Shift+8"),
        Keybinding::new("app.export", "Mod+Shift+E"),
        // Resolve: Projekteinstellungen = Shift+9 — hier Sequenzeinstellungen.
        Keybinding::new("sequence.settings", "Shift+9"),
        Keybinding::new("app.commandPalette", "Mod+Shift+P"),
        // Resolve-Seiten Shift+2..7
        Keybinding::new("workspace.switch.media", "Shift+2"),
        Keybinding::new("workspace.switch.graphics", "Shift+3"),
        Keybinding::new("workspace.switch.edit", "Shift+4"),
        Keybinding::new("workspace.switch.effects", "Shift+5"),
        Keybinding::new("workspace.switch.color", "Shift+6"),
        Keybinding::new("workspace.switch.audio", "Shift+7"),
        Keybinding::new("workspace.next", "Mod+Alt+ArrowRight"),
        Keybinding::new("workspace.previous", "Mod+Alt+ArrowLeft"),
        Keybinding::new("workspace.resetLayout", "Alt+Shift+0"),
    ]);
    KeymapPreset {
        id: "resolve",
        name: "DaVinci Resolve",
        description: "Tastaturbelegung nach den Standard-Shortcuts von DaVinci Resolve.",
        bindings,
    }
}

// -------------------------------------------------------------- KeymapStore

#[derive(Serialize, Deserialize, Default)]
struct SavedKeymap {
    active_preset_id: String,
    #[serde(default)]
    user_bindings: std::collections::HashMap<String, Vec<Keybinding>>,
}

pub struct KeymapStore {
    pub presets: Vec<KeymapPreset>,
    pub active_preset_id: String,
    /// Overrides pro Command: ersetzen die Preset-Bindings dieses Commands
    /// vollständig. Ein leeres Array bedeutet „Command bewusst unbelegt“.
    pub user_bindings: std::collections::HashMap<String, Vec<Keybinding>>,
}

fn keymap_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("editron")
        .join("keymap.json")
}

impl KeymapStore {
    pub fn load() -> KeymapStore {
        let mut store = KeymapStore {
            presets: presets(),
            active_preset_id: "editron".into(),
            user_bindings: Default::default(),
        };
        if let Ok(raw) = std::fs::read_to_string(keymap_path()) {
            if let Ok(saved) = serde_json::from_str::<SavedKeymap>(&raw) {
                if store.presets.iter().any(|p| p.id == saved.active_preset_id) {
                    store.active_preset_id = saved.active_preset_id;
                }
                store.user_bindings = saved.user_bindings;
            }
        }
        store
    }

    pub fn save(&self) {
        let saved = SavedKeymap {
            active_preset_id: self.active_preset_id.clone(),
            user_bindings: self.user_bindings.clone(),
        };
        let path = keymap_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(&saved) {
            let _ = std::fs::write(path, json);
        }
    }

    pub fn active_preset(&self) -> &KeymapPreset {
        self.presets
            .iter()
            .find(|p| p.id == self.active_preset_id)
            .unwrap_or(&self.presets[0])
    }

    /// Preset-Bindings (ohne überschriebene Commands) gefolgt von allen
    /// Nutzer-Overrides — Overrides hinten, weil der Resolver den
    /// spätesten Treffer gewinnen lässt.
    pub fn effective_bindings(&self) -> Vec<Keybinding> {
        let mut bindings: Vec<Keybinding> = self
            .active_preset()
            .bindings
            .iter()
            .filter(|b| !self.user_bindings.contains_key(&b.command))
            .cloned()
            .collect();
        for user in self.user_bindings.values() {
            bindings.extend(user.iter().cloned());
        }
        bindings
    }

    pub fn bindings_for_command(&self, command_id: &str) -> Vec<Keybinding> {
        if let Some(over) = self.user_bindings.get(command_id) {
            return over.clone();
        }
        self.active_preset()
            .bindings
            .iter()
            .filter(|b| b.command == command_id)
            .cloned()
            .collect()
    }

    pub fn is_customized(&self, command_id: &str) -> bool {
        self.user_bindings.contains_key(command_id)
    }

    pub fn set_active_preset(&mut self, id: &str) {
        if self.presets.iter().any(|p| p.id == id) {
            self.active_preset_id = id.to_string();
            self.save();
        }
    }

    pub fn set_user_bindings(&mut self, command_id: &str, bindings: Vec<Keybinding>) {
        self.user_bindings.insert(command_id.to_string(), bindings);
        self.save();
    }

    pub fn reset_command(&mut self, command_id: &str) {
        self.user_bindings.remove(command_id);
        self.save();
    }

    pub fn reset_all(&mut self) {
        self.user_bindings.clear();
        self.save();
    }

    /// Effektive Bindings (außer denen von `exclude_command`), deren
    /// Chord-Sequenz sich mit `keys` überschneidet (gleich oder Präfix).
    pub fn conflicts_for(&self, keys: &str, exclude_command: Option<&str>) -> Vec<Keybinding> {
        let chords = parse_keys(keys);
        if chords.is_empty() {
            return Vec::new();
        }
        self.effective_bindings()
            .into_iter()
            .filter(|binding| {
                if exclude_command == Some(binding.command.as_str()) {
                    return false;
                }
                let other = parse_keys(&binding.keys);
                if other.is_empty() {
                    return false;
                }
                sequence_starts_with(&other, &chords) || sequence_starts_with(&chords, &other)
            })
            .collect()
    }

    /// Erste Tastenkombination eines Commands (für Menüs/Tooltips), formatiert.
    pub fn shortcut_for(&self, command: &str) -> Option<String> {
        self.bindings_for_command(command)
            .first()
            .map(|b| format_keys(&b.keys))
    }

    /// Neues Binding für die Aufnahme erzeugen.
    pub fn make_binding(command: &str, keys: &str) -> Keybinding {
        Keybinding {
            command: command.to_string(),
            keys: keys.to_string(),
            when: None,
            args: None,
        }
    }
}

// ----------------------------------------------------------------- Resolver

/// Wartezeit auf den nächsten Chord einer Sequenz.
const SEQUENCE_TIMEOUT: f64 = 1.2;

struct CompiledBinding {
    binding: Keybinding,
    chords: Vec<KeyChord>,
}

/// Auszuführender Befehl als Resolver-Ergebnis.
pub struct ResolvedCommand {
    pub command: String,
    pub args: Option<Value>,
}

#[derive(Default)]
pub struct KeybindingResolver {
    compiled: Vec<CompiledBinding>,
    buffer: Vec<KeyChord>,
    pending_exact: Option<usize>,
    deadline: Option<f64>,
}

impl KeybindingResolver {
    pub fn set_bindings(&mut self, bindings: Vec<Keybinding>) {
        self.compiled = bindings
            .into_iter()
            .map(|binding| CompiledBinding {
                chords: parse_keys(&binding.keys),
                binding,
            })
            .filter(|c| !c.chords.is_empty())
            .collect();
        self.reset();
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
        self.pending_exact = None;
        self.deadline = None;
    }

    /// Löst einen Tastendruck auf; liefert ggf. den auszuführenden Command.
    pub fn handle_keypress(
        &mut self,
        press: &KeyPress,
        registry: &CommandRegistry,
        state: &AppState,
        now: f64,
    ) -> Option<ResolvedCommand> {
        let chord = chord_from_keypress(press)?;
        let mut sequence = self.buffer.clone();
        sequence.push(chord);

        let mut exact: Option<usize> = None;
        let mut has_longer = false;
        for (i, compiled) in self.compiled.iter().enumerate() {
            if !sequence_starts_with(&compiled.chords, &sequence) {
                continue;
            }
            if !evaluate_when(compiled.binding.when.as_deref(), state) {
                continue;
            }
            let Some(cmd) = registry.get(&compiled.binding.command) else {
                continue;
            };
            if !evaluate_when(cmd.when, state) {
                continue;
            }
            if compiled.chords.len() == sequence.len() {
                exact = Some(i); // späterer Treffer gewinnt (Overrides)
            } else {
                has_longer = true;
            }
        }

        if has_longer {
            // Auf weitere Chords warten; exakter Treffer wird beim Timeout
            // nachgeholt, falls kein weiterer Chord kommt.
            self.buffer = sequence;
            self.pending_exact = exact;
            self.deadline = Some(now + SEQUENCE_TIMEOUT);
            return None;
        }

        self.reset();
        let i = exact?;
        let compiled = &self.compiled[i];
        // Key-Auto-Repeat nur ausführen, wenn der Command das erlaubt.
        if press.repeat {
            let allow = registry
                .get(&compiled.binding.command)
                .map(|c| c.allow_repeat)
                .unwrap_or(false);
            if !allow {
                return None;
            }
        }
        Some(ResolvedCommand {
            command: compiled.binding.command.clone(),
            args: compiled.binding.args.clone(),
        })
    }

    /// Pro Frame: Sequenz-Timeout prüfen (setTimeout-Pendant).
    pub fn tick(&mut self, now: f64) -> Option<ResolvedCommand> {
        let deadline = self.deadline?;
        if now < deadline {
            return None;
        }
        let pending = self.pending_exact.take();
        self.reset();
        let i = pending?;
        let compiled = &self.compiled[i];
        Some(ResolvedCommand {
            command: compiled.binding.command.clone(),
            args: compiled.binding.args.clone(),
        })
    }
}
