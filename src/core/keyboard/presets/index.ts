import type { KeymapPreset } from "../types";
import { editronPreset } from "./editron";
import { premierePreset } from "./premiere";
import { resolvePreset } from "./resolve";

export { editronPreset, premierePreset, resolvePreset };

export const DEFAULT_PRESET_ID = editronPreset.id;

/** Alle mitgelieferten Keymap-Presets in Anzeige-Reihenfolge. */
export const allPresets: KeymapPreset[] = [
  editronPreset,
  premierePreset,
  resolvePreset,
];
