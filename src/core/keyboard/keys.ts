import type { KeyChord } from "./types";

/**
 * Parsen, Normalisieren, Serialisieren und Formatieren von
 * Tastenkombinationen. Serialisierungsformat siehe types.ts:
 * "Mod+Shift+K", Sequenzen "Mod+K Mod+S".
 */

/** "Mod" = Cmd auf macOS, Ctrl auf Windows/Linux. */
export const isMacOS: boolean =
  typeof navigator !== "undefined" &&
  (/mac/i.test(navigator.platform) || /Mac OS X/i.test(navigator.userAgent));

/** e.key-Werte, die selbst Modifier sind — ergeben nie einen eigenen Chord. */
const MODIFIER_EVENT_KEYS = new Set([
  "control",
  "shift",
  "alt",
  "meta",
  "altgraph",
  "os",
  "capslock",
  "numlock",
  "scrolllock",
  "fn",
  "fnlock",
  "hyper",
  "super",
  "symbol",
  "symbollock",
]);

/** Aliasse beim Parsen von Keybinding-Strings (alle lowercase). */
const KEY_ALIASES: Record<string, string> = {
  esc: "escape",
  del: "delete",
  entf: "delete",
  return: "enter",
  spacebar: "space",
  leertaste: "space",
  plus: "+",
  minus: "-",
  left: "arrowleft",
  right: "arrowright",
  up: "arrowup",
  down: "arrowdown",
  pos1: "home",
  pgup: "pageup",
  pgdn: "pagedown",
};

/** Kanonische Schreibweise benannter Tasten für die Serialisierung. */
const CANONICAL_NAMES: Record<string, string> = {
  arrowleft: "ArrowLeft",
  arrowright: "ArrowRight",
  arrowup: "ArrowUp",
  arrowdown: "ArrowDown",
  pageup: "PageUp",
  pagedown: "PageDown",
  contextmenu: "ContextMenu",
  printscreen: "PrintScreen",
};

/** Deutsche/symbolische Anzeige-Namen für benannte Tasten. */
const DISPLAY_NAMES: Record<string, string> = {
  space: "Leertaste",
  enter: "Eingabe",
  escape: "Esc",
  backspace: "Rücktaste",
  delete: "Entf",
  insert: "Einfg",
  tab: "Tab",
  home: "Pos1",
  end: "Ende",
  pageup: "Bild↑",
  pagedown: "Bild↓",
  arrowleft: "←",
  arrowright: "→",
  arrowup: "↑",
  arrowdown: "↓",
};

/**
 * Normalisiert einen Tastennamen (aus KeyboardEvent.key oder einem
 * Keybinding-Token): Buchstaben lowercase, " " → "space",
 * benannte Tasten lowercase ("ArrowLeft" → "arrowleft").
 */
export function normalizeKey(key: string): string {
  if (key === " ") return "space";
  const lower = key.toLowerCase();
  return KEY_ALIASES[lower] ?? lower;
}

/**
 * Layout-robuste Taste aus einem KeyboardEvent: Mit Shift/Alt gedrückte
 * Ziffern-/Buchstabentasten liefern in e.key Sonderzeichen ("@", "∂") —
 * für Bindings wie "Shift+2" oder "Alt+Shift+0" greifen wir auf e.code zurück.
 */
function keyFromEvent(e: KeyboardEvent): string {
  // Numpad nur auf die Ziffer mappen, wenn die Taste wirklich eine Ziffer
  // geliefert hat — bei NumLock aus liefert e.key "ArrowLeft"/"Home"/…,
  // das übernimmt dann normalizeKey(e.key).
  const numpad = /^Numpad([0-9])$/.exec(e.code);
  if (numpad && /^[0-9]$/.test(e.key)) return numpad[1];
  if (e.shiftKey || e.altKey) {
    const digit = /^Digit([0-9])$/.exec(e.code);
    if (digit) return digit[1];
  }
  if (e.altKey) {
    const letter = /^Key([A-Z])$/.exec(e.code);
    if (letter) return letter[1].toLowerCase();
  }
  return normalizeKey(e.key);
}

/**
 * Erzeugt einen KeyChord aus einem KeyboardEvent.
 * Reine Modifier-Drücke (Shift allein usw.) liefern null.
 */
export function chordFromEvent(e: KeyboardEvent): KeyChord | null {
  const lower = e.key.toLowerCase();
  if (MODIFIER_EVENT_KEYS.has(lower)) return null;
  if (e.key === "Dead" || e.key === "Unidentified") return null;
  return {
    key: keyFromEvent(e),
    ctrl: e.ctrlKey,
    shift: e.shiftKey,
    alt: e.altKey,
    meta: e.metaKey,
  };
}

/** Parst einen einzelnen Chord wie "Mod+Shift+K" oder "Mod++". */
function parseChord(raw: string): KeyChord | null {
  const tokens = raw.split("+");
  // Endet der Ausdruck auf "+", ist die Taste das Pluszeichen selbst:
  // "Mod++" → ["Mod", "", ""], "+" → ["", ""]
  let keyToken = tokens.pop() ?? "";
  if (keyToken === "") {
    keyToken = "+";
    if (tokens[tokens.length - 1] === "") tokens.pop();
  }

  const chord: KeyChord = {
    key: "",
    ctrl: false,
    shift: false,
    alt: false,
    meta: false,
  };

  for (const token of tokens) {
    switch (token.trim().toLowerCase()) {
      case "mod":
        if (isMacOS) chord.meta = true;
        else chord.ctrl = true;
        break;
      case "ctrl":
      case "control":
      case "strg":
        chord.ctrl = true;
        break;
      case "alt":
      case "option":
      case "opt":
        chord.alt = true;
        break;
      case "shift":
      case "umschalt":
        chord.shift = true;
        break;
      case "meta":
      case "cmd":
      case "command":
      case "super":
      case "win":
        chord.meta = true;
        break;
      default:
        console.warn(`[keyboard] Unbekannter Modifier "${token}" in "${raw}"`);
        return null;
    }
  }

  chord.key = normalizeKey(keyToken.trim());
  if (chord.key === "") return null;
  return chord;
}

/**
 * Parst einen serialisierten Keybinding-String in eine Chord-Sequenz.
 * Ungültige Chords werden (mit Warnung) verworfen.
 */
export function parseKeys(keys: string): KeyChord[] {
  return keys
    .trim()
    .split(/\s+/)
    .filter((part) => part !== "")
    .map(parseChord)
    .filter((c): c is KeyChord => c !== null);
}

export function chordsEqual(a: KeyChord, b: KeyChord): boolean {
  return (
    a.key === b.key &&
    a.ctrl === b.ctrl &&
    a.shift === b.shift &&
    a.alt === b.alt &&
    a.meta === b.meta
  );
}

/** true, wenn `sequence` mit allen Chords aus `prefix` beginnt. */
export function sequenceStartsWith(
  sequence: KeyChord[],
  prefix: KeyChord[],
): boolean {
  if (prefix.length > sequence.length) return false;
  return prefix.every((chord, i) => chordsEqual(chord, sequence[i]));
}

function canonicalKeyName(key: string): string {
  if (key.length === 1) return key.toUpperCase();
  const canonical = CANONICAL_NAMES[key];
  if (canonical) return canonical;
  if (/^f\d{1,2}$/.test(key)) return key.toUpperCase();
  return key.charAt(0).toUpperCase() + key.slice(1);
}

/**
 * Serialisiert einen Chord zurück ins Keybinding-Format ("Mod+Shift+K").
 * Der plattformprimäre Modifier (Cmd auf macOS, Ctrl sonst) wird als
 * "Mod" geschrieben, damit Bindings portabel bleiben.
 */
export function serializeChord(chord: KeyChord): string {
  const parts: string[] = [];
  if (isMacOS) {
    if (chord.ctrl) parts.push("Ctrl");
    if (chord.alt) parts.push("Alt");
    if (chord.shift) parts.push("Shift");
    if (chord.meta) parts.push("Mod");
  } else {
    if (chord.ctrl) parts.push("Mod");
    if (chord.alt) parts.push("Alt");
    if (chord.shift) parts.push("Shift");
    if (chord.meta) parts.push("Meta");
  }
  parts.push(canonicalKeyName(chord.key));
  return parts.join("+");
}

/** Serialisiert eine Chord-Sequenz ("Mod+K Mod+S"). */
export function serializeChords(chords: KeyChord[]): string {
  return chords.map(serializeChord).join(" ");
}

function displayKeyName(key: string): string {
  if (key.length === 1) return key.toUpperCase();
  const display = DISPLAY_NAMES[key];
  if (display) return display;
  if (/^f\d{1,2}$/.test(key)) return key.toUpperCase();
  return key.charAt(0).toUpperCase() + key.slice(1);
}

/** Formatiert einen einzelnen Chord für die Anzeige. */
export function formatChord(chord: KeyChord): string {
  const keyLabel = displayKeyName(chord.key);
  if (isMacOS) {
    // macOS-Konvention: ⌃⌥⇧⌘ ohne Trennzeichen
    let out = "";
    if (chord.ctrl) out += "⌃";
    if (chord.alt) out += "⌥";
    if (chord.shift) out += "⇧";
    if (chord.meta) out += "⌘";
    return out + keyLabel;
  }
  const parts: string[] = [];
  if (chord.ctrl) parts.push("Strg");
  if (chord.alt) parts.push("Alt");
  if (chord.shift) parts.push("Umschalt");
  if (chord.meta) parts.push("Meta");
  parts.push(keyLabel);
  return parts.join("+");
}

/**
 * Formatiert einen serialisierten Keybinding-String für die Anzeige,
 * z. B. "Mod+Shift+K" → "Strg+Umschalt+K" bzw. "⇧⌘K" auf macOS.
 * Sequenzen werden mit Leerzeichen verbunden: "Strg+K Strg+S".
 */
export function formatKeys(keys: string): string {
  return parseKeys(keys).map(formatChord).join(" ");
}
