/**
 * Vertrag des Keybinding-Systems.
 *
 * Serialisierungsformat für Tastenkombinationen ("keys"):
 *   - Einzelner Chord:  "Mod+Shift+K"
 *   - Sequenz:          "Mod+K Mod+S"   (durch Leerzeichen getrennt)
 *   - "Mod" steht für Cmd auf macOS, Ctrl auf Windows/Linux.
 *   - Weitere Modifier: "Ctrl", "Alt", "Shift", "Meta" (explizit).
 *   - Tasten: Buchstaben/Ziffern ("K", "1"), benannte Tasten
 *     ("Space", "Enter", "Escape", "ArrowLeft", "Home", "F1", "+", ",").
 */

export interface KeyChord {
  /** Normalisierte Taste, z. B. "k", "space", "arrowleft", "+" */
  key: string;
  ctrl: boolean;
  shift: boolean;
  alt: boolean;
  meta: boolean;
}

export interface Keybinding {
  /** Command-ID aus der Registry */
  command: string;
  /** Serialisierte Tastenkombination, siehe oben */
  keys: string;
  /** Optionale when-Klausel; überschreibt NICHT die des Commands, beide müssen passen */
  when?: string;
  /** Optionale Argumente für commandRegistry.execute */
  args?: unknown;
}

export interface KeymapPreset {
  /** "editron" | "premiere" | "resolve" | eigene */
  id: string;
  name: string;
  description?: string;
  bindings: Keybinding[];
}
