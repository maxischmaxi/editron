import type { KeymapPreset } from "../types";

/**
 * Editron-Standard-Keymap.
 *
 * Orientiert sich an branchenüblichen NLE-Konventionen (JKL-Shuttle,
 * I/O-Punkte, einbuchstabige Werkzeuge), ergänzt um Editron-eigene
 * Belegungen wie "Mod+K Mod+S" (Sequenz!) für den Shortcut-Editor.
 */
export const editronPreset: KeymapPreset = {
  id: "editron",
  name: "Editron (Standard)",
  description: "Die Standard-Tastaturbelegung von Editron.",
  bindings: [
    // Wiedergabe
    { command: "playback.togglePlay", keys: "Space" },
    { command: "playback.shuttleReverse", keys: "J" },
    { command: "playback.shuttleStop", keys: "K" },
    { command: "playback.shuttleForward", keys: "L" },
    { command: "playback.stepBackward", keys: "ArrowLeft" },
    { command: "playback.stepForward", keys: "ArrowRight" },
    { command: "playback.stepBackward5", keys: "Shift+ArrowLeft" },
    { command: "playback.stepForward5", keys: "Shift+ArrowRight" },
    { command: "playback.goToStart", keys: "Home" },
    { command: "playback.goToEnd", keys: "End" },
    { command: "playback.setInPoint", keys: "I" },
    { command: "playback.setOutPoint", keys: "O" },
    { command: "playback.clearInOut", keys: "Mod+Shift+X" },

    // Werkzeuge
    { command: "tools.select", keys: "V" },
    { command: "tools.razor", keys: "C" },
    { command: "tools.ripple", keys: "B" },
    { command: "tools.rolling", keys: "N" },
    { command: "tools.slip", keys: "Y" },
    { command: "tools.slide", keys: "U" },
    { command: "tools.hand", keys: "H" },
    { command: "tools.zoom", keys: "Z" },

    // Timeline — "=" zusätzlich zu "+", weil "+" auf US-Layouts
    // nur über Shift+"=" erreichbar ist.
    { command: "timeline.zoomIn", keys: "+" },
    { command: "timeline.zoomIn", keys: "=" },
    { command: "timeline.zoomOut", keys: "-" },
    { command: "timeline.zoomFit", keys: "Shift+Z" },
    { command: "timeline.addMarker", keys: "M" },
    { command: "timeline.toggleSnapping", keys: "S" },

    // Timeline-Bearbeitung — Schneiden bewusst auf Mod+B (nicht Mod+K):
    // Mod+K ist Präfix der Sequenz "Mod+K Mod+S" und würde erst nach dem
    // Sequenz-Timeout ausgeführt.
    { command: "edit.undo", keys: "Mod+Z" },
    { command: "edit.redo", keys: "Mod+Shift+Z" },
    { command: "edit.redo", keys: "Mod+Y" },
    { command: "timeline.splitAtPlayhead", keys: "Mod+B" },
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

    // Timeline-Playhead — gewinnt bei Fokus auf der Timeline gegen
    // die gleichlautenden Wiedergabe-Bindings (späterer Treffer zählt).
    { command: "timeline.goToStart", keys: "Home", when: "panel == 'timeline'" },
    { command: "timeline.goToEnd", keys: "End", when: "panel == 'timeline'" },
    { command: "timeline.stepBackward", keys: "ArrowLeft", when: "panel == 'timeline'" },
    { command: "timeline.stepForward", keys: "ArrowRight", when: "panel == 'timeline'" },
    { command: "timeline.stepBackward5", keys: "Shift+ArrowLeft", when: "panel == 'timeline'" },
    { command: "timeline.stepForward5", keys: "Shift+ArrowRight", when: "panel == 'timeline'" },
    { command: "timeline.prevEdit", keys: "ArrowUp", when: "panel == 'timeline'" },
    { command: "timeline.nextEdit", keys: "ArrowDown", when: "panel == 'timeline'" },

    // Medien — Entfernen nur bei Fokus auf dem Medien-Browser, damit
    // Delete in der Timeline keine Assets löscht.
    { command: "media.import", keys: "Mod+I" },
    { command: "media.addSelectionToTimeline", keys: "." },
    { command: "media.removeSelected", keys: "Delete", when: "panel == 'media'" },

    // Anwendung
    { command: "app.export", keys: "Mod+E" },
    { command: "app.commandPalette", keys: "Mod+Shift+P" },
    { command: "app.shortcutEditor", keys: "Mod+K Mod+S" },

    // Workspaces
    { command: "workspace.switch.media", keys: "Alt+1" },
    { command: "workspace.switch.edit", keys: "Alt+2" },
    { command: "workspace.switch.color", keys: "Alt+3" },
    { command: "workspace.switch.effects", keys: "Alt+4" },
    { command: "workspace.switch.audio", keys: "Alt+5" },
    { command: "workspace.switch.graphics", keys: "Alt+6" },
    { command: "workspace.next", keys: "Mod+Alt+ArrowRight" },
    { command: "workspace.previous", keys: "Mod+Alt+ArrowLeft" },
    { command: "workspace.resetLayout", keys: "Alt+Shift+0" },
  ],
};
