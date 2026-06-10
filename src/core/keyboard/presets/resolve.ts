import type { KeymapPreset } from "../types";

/**
 * Keymap nach DaVinci Resolve (Default-Belegung).
 *
 * Abweichungen/Annäherungen gegenüber dem Resolve-Original:
 * - Seiten → Workspaces (Shift+2..7): Media→Medien, Edit→Schnitt,
 *   Fusion→Effekte, Color→Farbe, Fairlight→Audio. Die Cut-Seite (Shift+3)
 *   existiert in Editron nicht — der Slot ist mit dem Grafik-Workspace
 *   belegt. Shift+8 (Deliver) öffnet den Export-Dialog.
 * - Werkzeuge: Resolve kennt Bearbeitungs-Modi statt Werkzeugleiste:
 *   A = Auswahl, B = Blade, T = Trim (≈ Ripple). Rolling/Slip/Slide
 *   laufen in Resolve innerhalb des Trim-Modus und Hand/Zoom haben kein
 *   Standard-Kürzel — diese bleiben hier unbelegt.
 * - Shift+Pfeil: in Resolve 1 Sekunde Sprung, hier 5 Frames (Annäherung).
 * - "Mod+Shift+P" (Befehlspalette) ist Editron-eigen, Resolve hat keine.
 */
export const resolvePreset: KeymapPreset = {
  id: "resolve",
  name: "DaVinci Resolve",
  description:
    "Tastaturbelegung nach den Standard-Shortcuts von DaVinci Resolve.",
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
    // Resolve: "Clear In and Out" = Alt+X
    { command: "playback.clearInOut", keys: "Alt+X" },

    // Werkzeuge / Bearbeitungs-Modi
    { command: "tools.select", keys: "A" },
    { command: "tools.razor", keys: "B" },
    { command: "tools.ripple", keys: "T" },

    // Timeline — Resolve: Zoom In/Out = Ctrl/Cmd + "=" bzw. "-",
    // "Zoom to Fit" = Shift+Z, Snapping = N
    { command: "timeline.zoomIn", keys: "Mod+=" },
    { command: "timeline.zoomIn", keys: "Mod++" },
    { command: "timeline.zoomOut", keys: "Mod+-" },
    { command: "timeline.zoomFit", keys: "Shift+Z" },
    // Resolve: Marker hinzufügen = M
    { command: "timeline.addMarker", keys: "M" },
    { command: "timeline.toggleSnapping", keys: "N" },

    // Timeline-Bearbeitung — Resolve: Split Clip = Mod+B bzw. Mod+\,
    // Link Clips = Mod+Alt+L, Clip aktivieren = D,
    // Ripple Delete = Shift+Delete bzw. Shift+Backspace
    { command: "edit.undo", keys: "Mod+Z" },
    { command: "edit.redo", keys: "Mod+Shift+Z" },
    { command: "timeline.splitAtPlayhead", keys: "Mod+B" },
    { command: "timeline.splitAtPlayhead", keys: "Mod+\\" },
    { command: "timeline.toggleLink", keys: "Mod+Alt+L" },
    { command: "timeline.toggleClipEnabled", keys: "D" },
    { command: "timeline.deleteSelected", keys: "Delete", when: "panel == 'timeline'" },
    { command: "timeline.deleteSelected", keys: "Backspace", when: "panel == 'timeline'" },
    { command: "timeline.rippleDelete", keys: "Shift+Delete", when: "panel == 'timeline'" },
    { command: "timeline.rippleDelete", keys: "Shift+Backspace", when: "panel == 'timeline'" },
    { command: "timeline.selectAll", keys: "Mod+A", when: "panel == 'timeline'" },
    { command: "timeline.deselectAll", keys: "Mod+Shift+A", when: "panel == 'timeline'" },
    { command: "timeline.copy", keys: "Mod+C", when: "panel == 'timeline'" },
    { command: "timeline.cut", keys: "Mod+X", when: "panel == 'timeline'" },
    { command: "timeline.paste", keys: "Mod+V", when: "panel == 'timeline'" },

    // Timeline-Playhead bei Fokus auf der Timeline (Resolve: Up/Down
    // springen zwischen Schnittpunkten)
    { command: "timeline.goToStart", keys: "Home", when: "panel == 'timeline'" },
    { command: "timeline.goToEnd", keys: "End", when: "panel == 'timeline'" },
    { command: "timeline.stepBackward", keys: "ArrowLeft", when: "panel == 'timeline'" },
    { command: "timeline.stepForward", keys: "ArrowRight", when: "panel == 'timeline'" },
    { command: "timeline.stepBackward5", keys: "Shift+ArrowLeft", when: "panel == 'timeline'" },
    { command: "timeline.stepForward5", keys: "Shift+ArrowRight", when: "panel == 'timeline'" },
    { command: "timeline.prevEdit", keys: "ArrowUp", when: "panel == 'timeline'" },
    { command: "timeline.nextEdit", keys: "ArrowDown", when: "panel == 'timeline'" },

    // Medien — Resolve: "Import Media" = Ctrl/Cmd+I
    { command: "media.import", keys: "Mod+I" },
    { command: "media.removeSelected", keys: "Delete", when: "panel == 'media'" },
    { command: "media.removeSelected", keys: "Backspace", when: "panel == 'media'" },

    // Anwendung — Resolve: Tastatur-Anpassung = Ctrl/Cmd+Alt+K.
    // Export: Shift+8 (Deliver-Seite) und Mod+Shift+E als Annäherung
    // an Resolves Quick-Export-Workflows.
    { command: "app.shortcutEditor", keys: "Mod+Alt+K" },
    { command: "app.export", keys: "Shift+8" },
    { command: "app.export", keys: "Mod+Shift+E" },
    { command: "app.commandPalette", keys: "Mod+Shift+P" },

    // Workspaces — Resolve-Seiten Shift+2..7 (siehe Kopfkommentar)
    { command: "workspace.switch.media", keys: "Shift+2" },
    { command: "workspace.switch.graphics", keys: "Shift+3" },
    { command: "workspace.switch.edit", keys: "Shift+4" },
    { command: "workspace.switch.effects", keys: "Shift+5" },
    { command: "workspace.switch.color", keys: "Shift+6" },
    { command: "workspace.switch.audio", keys: "Shift+7" },
    { command: "workspace.next", keys: "Mod+Alt+ArrowRight" },
    { command: "workspace.previous", keys: "Mod+Alt+ArrowLeft" },
    { command: "workspace.resetLayout", keys: "Alt+Shift+0" },
  ],
};
