import { create } from "zustand";
import { LazyStore } from "@tauri-apps/plugin-store";
import type { Keybinding, KeymapPreset } from "./types";
import { parseKeys, sequenceStartsWith } from "./keys";
import { allPresets, DEFAULT_PRESET_ID } from "./presets";

/**
 * Keymap-Zustand: aktives Preset + Nutzer-Overrides.
 *
 * Persistenz über @tauri-apps/plugin-store ("keymap.json"). Außerhalb
 * von Tauri (reines Vite) degradiert der Store defensiv: alles bleibt
 * nur im Speicher.
 */

const STORE_FILE = "keymap.json";
const SAVE_DEBOUNCE_MS = 400;

interface KeymapState {
  /** true, sobald der Persistenz-Stand geladen (oder verworfen) wurde */
  hydrated: boolean;
  activePresetId: string;
  presets: KeymapPreset[];
  /**
   * Overrides pro Command: ersetzen die Preset-Bindings dieses Commands
   * vollständig. Ein leeres Array bedeutet "Command bewusst unbelegt".
   */
  userBindings: Record<string, Keybinding[]>;

  /**
   * Preset-Bindings (ohne überschriebene Commands) gefolgt von allen
   * Nutzer-Overrides. Die Overrides stehen hinten, weil der Resolver
   * den spätesten Treffer gewinnen lässt.
   */
  effectiveBindings: () => Keybinding[];
  bindingsForCommand: (commandId: string) => Keybinding[];
  /** true, wenn für den Command ein Nutzer-Override existiert */
  isCustomized: (commandId: string) => boolean;
  setActivePreset: (id: string) => void;
  setUserBindings: (commandId: string, bindings: Keybinding[]) => void;
  resetCommand: (commandId: string) => void;
  resetAll: () => void;
  /**
   * Effektive Bindings (außer denen von excludeCommand), deren
   * Chord-Sequenz sich mit `keys` überschneidet (gleich oder Präfix).
   */
  conflictsFor: (keys: string, excludeCommand?: string) => Keybinding[];
}

export const useKeymapStore = create<KeymapState>((set, get) => ({
  hydrated: false,
  activePresetId: DEFAULT_PRESET_ID,
  presets: allPresets,
  userBindings: {},

  effectiveBindings: () => {
    const { activePresetId, presets, userBindings } = get();
    const preset =
      presets.find((p) => p.id === activePresetId) ?? presets[0];
    const fromPreset = preset
      ? preset.bindings.filter((b) => !(b.command in userBindings))
      : [];
    const fromUser = Object.values(userBindings).flat();
    return [...fromPreset, ...fromUser];
  },

  bindingsForCommand: (commandId) => {
    const { activePresetId, presets, userBindings } = get();
    const override = userBindings[commandId];
    if (override) return override;
    const preset =
      presets.find((p) => p.id === activePresetId) ?? presets[0];
    return preset
      ? preset.bindings.filter((b) => b.command === commandId)
      : [];
  },

  isCustomized: (commandId) => commandId in get().userBindings,

  setActivePreset: (id) => {
    if (!get().presets.some((p) => p.id === id)) {
      console.warn(`[keymap] Unbekanntes Preset: "${id}"`);
      return;
    }
    set({ activePresetId: id });
    schedulePersist();
  },

  setUserBindings: (commandId, bindings) => {
    set((s) => ({
      userBindings: { ...s.userBindings, [commandId]: bindings },
    }));
    schedulePersist();
  },

  resetCommand: (commandId) => {
    set((s) => {
      if (!(commandId in s.userBindings)) return s;
      const userBindings = { ...s.userBindings };
      delete userBindings[commandId];
      return { userBindings };
    });
    schedulePersist();
  },

  resetAll: () => {
    set({ userBindings: {} });
    schedulePersist();
  },

  conflictsFor: (keys, excludeCommand) => {
    const chords = parseKeys(keys);
    if (chords.length === 0) return [];
    return get()
      .effectiveBindings()
      .filter((binding) => {
        if (excludeCommand !== undefined && binding.command === excludeCommand)
          return false;
        const other = parseKeys(binding.keys);
        if (other.length === 0) return false;
        return (
          sequenceStartsWith(other, chords) ||
          sequenceStartsWith(chords, other)
        );
      });
  },
}));

// ---------------------------------------------------------------------------
// Persistenz (Tauri plugin-store) — außerhalb von Tauri rein im Speicher
// ---------------------------------------------------------------------------

let diskStore: LazyStore | null = null;

function getDiskStore(): LazyStore | null {
  if (diskStore) return diskStore;
  try {
    if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
      diskStore = new LazyStore(STORE_FILE);
    }
  } catch (err) {
    console.warn("[keymap] Persistenter Store nicht verfügbar:", err);
  }
  return diskStore;
}

let saveTimer: ReturnType<typeof setTimeout> | null = null;

function schedulePersist(): void {
  if (saveTimer !== null) clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    saveTimer = null;
    void persistNow();
  }, SAVE_DEBOUNCE_MS);
}

/** Schreibt eine ausstehende Debounce-Speicherung sofort (z. B. beim App-Schluss). */
export function flushKeymap(): void {
  if (saveTimer === null) return;
  clearTimeout(saveTimer);
  saveTimer = null;
  void persistNow();
}

if (typeof window !== "undefined") {
  window.addEventListener("pagehide", flushKeymap);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") flushKeymap();
  });
}

async function persistNow(): Promise<void> {
  const store = getDiskStore();
  if (!store) return;
  try {
    const { activePresetId, userBindings } = useKeymapStore.getState();
    await store.set("activePresetId", activePresetId);
    await store.set("userBindings", userBindings);
    await store.save();
  } catch (err) {
    console.warn("[keymap] Speichern fehlgeschlagen:", err);
  }
}

let hydrationStarted = false;

/**
 * Validiert geladene userBindings feldweise (keymap.json kann handeditiert
 * oder korrupt sein): null/Arrays/Primitive werden komplett verworfen,
 * Nicht-Array-Werte pro Command ebenso, Einträge ohne string-`command`/
 * `keys` werden herausgefiltert. Liefert null, wenn nichts Brauchbares da ist.
 */
function sanitizeUserBindings(
  raw: unknown,
): Record<string, Keybinding[]> | null {
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) {
    return null;
  }
  const result: Record<string, Keybinding[]> = {};
  for (const [command, value] of Object.entries(raw)) {
    if (!Array.isArray(value)) continue;
    result[command] = value.filter(
      (b): b is Keybinding =>
        typeof b === "object" &&
        b !== null &&
        typeof (b as Keybinding).command === "string" &&
        typeof (b as Keybinding).keys === "string" &&
        ((b as Keybinding).when === undefined ||
          typeof (b as Keybinding).when === "string"),
    );
  }
  return result;
}

/** Lädt den persistierten Stand. Idempotent — mehrfacher Aufruf ist ein No-op. */
export async function hydrateKeymap(): Promise<void> {
  if (hydrationStarted) return;
  hydrationStarted = true;

  const store = getDiskStore();
  if (!store) {
    useKeymapStore.setState({ hydrated: true });
    return;
  }

  try {
    const presetId = await store.get<unknown>("activePresetId");
    const userBindings = sanitizeUserBindings(
      await store.get<unknown>("userBindings"),
    );
    useKeymapStore.setState((s) => ({
      hydrated: true,
      activePresetId:
        typeof presetId === "string" &&
        s.presets.some((p) => p.id === presetId)
          ? presetId
          : s.activePresetId,
      userBindings: userBindings ?? s.userBindings,
    }));
  } catch (err) {
    console.warn("[keymap] Laden fehlgeschlagen:", err);
    useKeymapStore.setState({ hydrated: true });
  }
}

// Beim Modul-Load asynchron hydrieren (Module werden nur einmal evaluiert).
void hydrateKeymap();
