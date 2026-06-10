import type { TimelineTool } from "@/stores/appStore";

/**
 * Kanonische deutsche Werkzeug-Bezeichnungen.
 * Einzige Quelle für Tooltips, StatusBar und Befehlspalette —
 * niemals abweichende Namen hartkodieren.
 */
export const TOOL_LABELS: Record<TimelineTool, string> = {
  select: "Auswahl",
  razor: "Rasierklinge",
  ripple: "Ripple-Trimmen",
  rolling: "Rollen-Trimmen",
  slip: "Slip",
  slide: "Slide",
  hand: "Hand",
  zoom: "Zoom",
};

/** Command-Titel der Werkzeuge für Registry und Befehlspalette. */
export const TOOL_COMMAND_TITLES: Record<TimelineTool, string> = {
  select: "Auswahlwerkzeug",
  razor: "Rasierklingen-Werkzeug",
  ripple: "Ripple-Trimmen-Werkzeug",
  rolling: "Rollen-Trimmen-Werkzeug",
  slip: "Slip-Werkzeug",
  slide: "Slide-Werkzeug",
  hand: "Hand-Werkzeug",
  zoom: "Zoom-Werkzeug",
};
