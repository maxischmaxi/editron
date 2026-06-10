import type { KeymapPreset } from "../types";

/**
 * Keymap nach Adobe Premiere Pro (Default-Belegung).
 *
 * Abweichungen/Annäherungen gegenüber dem Premiere-Original:
 * - "Mod+Shift+P" (Befehlspalette): Premiere hat keine Command Palette,
 *   die Editron-Belegung bleibt erhalten.
 * - "Alt+Shift+0" (Layout zurücksetzen): Premiere nutzt das für
 *   "Auf gespeichertes Layout zurücksetzen" — passt sinngemäß.
 * - Premiere kennt "Zoom In" als "=" und "Zoom Out" als "-";
 *   "+" ist zusätzlich belegt, weil es auf DE-Layouts die direkte Taste ist.
 */
export const premierePreset: KeymapPreset = {
  id: "premiere",
  name: "Adobe Premiere Pro",
  description:
    "Tastaturbelegung nach den Standard-Shortcuts von Adobe Premiere Pro.",
  bindings: [
    // Wiedergabe — identisch zu Premiere (Space, JKL, I/O, Pfeile)
    { command: "playback.togglePlay", keys: "Space" },
    { command: "playback.shuttleReverse", keys: "J" },
    { command: "playback.shuttleStop", keys: "K" },
    { command: "playback.shuttleForward", keys: "L" },
    { command: "playback.stepBackward", keys: "ArrowLeft" },
    { command: "playback.stepForward", keys: "ArrowRight" },
    // Premiere: Shift+Pfeil = 5 Frames
    { command: "playback.stepBackward5", keys: "Shift+ArrowLeft" },
    { command: "playback.stepForward5", keys: "Shift+ArrowRight" },
    { command: "playback.goToStart", keys: "Home" },
    { command: "playback.goToEnd", keys: "End" },
    { command: "playback.setInPoint", keys: "I" },
    { command: "playback.setOutPoint", keys: "O" },
    // Premiere: "In- und Out-Punkt löschen" = Ctrl/Cmd+Shift+X
    { command: "playback.clearInOut", keys: "Mod+Shift+X" },

    // Werkzeuge — Premiere-Originaltasten
    { command: "tools.select", keys: "V" },
    { command: "tools.razor", keys: "C" },
    { command: "tools.ripple", keys: "B" },
    { command: "tools.rolling", keys: "N" },
    { command: "tools.slip", keys: "Y" },
    { command: "tools.slide", keys: "U" },
    { command: "tools.hand", keys: "H" },
    { command: "tools.zoom", keys: "Z" },

    // Timeline
    { command: "timeline.zoomIn", keys: "=" },
    { command: "timeline.zoomIn", keys: "+" },
    { command: "timeline.zoomOut", keys: "-" },
    // Premiere: "Zoom to Sequence" = \ — Shift+Z zusätzlich (Editron-Konvention)
    { command: "timeline.zoomFit", keys: "\\" },
    { command: "timeline.zoomFit", keys: "Shift+Z" },
    { command: "timeline.addMarker", keys: "M" },
    // Premiere: Snap = S
    { command: "timeline.toggleSnapping", keys: "S" },

    // Timeline-Bearbeitung — Premiere: Add Edit = Mod+K, Link/Unlink = Mod+L,
    // Enable/Disable Clip = Shift+E, Ripple Delete = Shift+Delete
    { command: "edit.undo", keys: "Mod+Z" },
    { command: "edit.redo", keys: "Mod+Shift+Z" },
    { command: "timeline.splitAtPlayhead", keys: "Mod+K" },
    { command: "timeline.toggleLink", keys: "Mod+L" },
    { command: "timeline.toggleClipEnabled", keys: "Shift+E" },
    { command: "timeline.deleteSelected", keys: "Delete", when: "panel == 'timeline'" },
    { command: "timeline.deleteSelected", keys: "Backspace", when: "panel == 'timeline'" },
    { command: "timeline.rippleDelete", keys: "Shift+Delete", when: "panel == 'timeline'" },
    { command: "timeline.rippleDelete", keys: "Shift+Backspace", when: "panel == 'timeline'" },
    { command: "timeline.selectAll", keys: "Mod+A", when: "panel == 'timeline'" },
    { command: "timeline.deselectAll", keys: "Mod+Shift+A", when: "panel == 'timeline'" },
    { command: "timeline.copy", keys: "Mod+C", when: "panel == 'timeline'" },
    { command: "timeline.cut", keys: "Mod+X", when: "panel == 'timeline'" },
    { command: "timeline.paste", keys: "Mod+V", when: "panel == 'timeline'" },

    // Timeline-Playhead bei Fokus auf der Timeline (Premiere: Up/Down
    // springen zwischen Schnittpunkten)
    { command: "timeline.goToStart", keys: "Home", when: "panel == 'timeline'" },
    { command: "timeline.goToEnd", keys: "End", when: "panel == 'timeline'" },
    { command: "timeline.stepBackward", keys: "ArrowLeft", when: "panel == 'timeline'" },
    { command: "timeline.stepForward", keys: "ArrowRight", when: "panel == 'timeline'" },
    { command: "timeline.stepBackward5", keys: "Shift+ArrowLeft", when: "panel == 'timeline'" },
    { command: "timeline.stepForward5", keys: "Shift+ArrowRight", when: "panel == 'timeline'" },
    { command: "timeline.prevEdit", keys: "ArrowUp", when: "panel == 'timeline'" },
    { command: "timeline.nextEdit", keys: "ArrowDown", when: "panel == 'timeline'" },

    // Medien — Premiere: Importieren = Ctrl/Cmd+I, Overwrite = "."
    { command: "media.import", keys: "Mod+I" },
    { command: "media.addSelectionToTimeline", keys: "." },
    { command: "media.removeSelected", keys: "Delete", when: "panel == 'media'" },
    { command: "media.removeSelected", keys: "Backspace", when: "panel == 'media'" },

    // Anwendung — Premiere: Medien exportieren = Ctrl/Cmd+M,
    // Tastaturbefehle = Ctrl+Alt+K (Cmd+Opt+K auf macOS)
    { command: "app.export", keys: "Mod+M" },
    { command: "app.shortcutEditor", keys: "Mod+Alt+K" },
    { command: "app.commandPalette", keys: "Mod+Shift+P" },

    // Workspaces — Premiere: Alt+Shift+1..9 für Arbeitsbereiche
    { command: "workspace.switch.media", keys: "Alt+Shift+1" },
    { command: "workspace.switch.edit", keys: "Alt+Shift+2" },
    { command: "workspace.switch.color", keys: "Alt+Shift+3" },
    { command: "workspace.switch.effects", keys: "Alt+Shift+4" },
    { command: "workspace.switch.audio", keys: "Alt+Shift+5" },
    { command: "workspace.switch.graphics", keys: "Alt+Shift+6" },
    { command: "workspace.next", keys: "Mod+Alt+ArrowRight" },
    { command: "workspace.previous", keys: "Mod+Alt+ArrowLeft" },
    { command: "workspace.resetLayout", keys: "Alt+Shift+0" },
  ],
};
