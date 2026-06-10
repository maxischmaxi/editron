import { create } from "zustand";
import { useMonitorStore } from "@/stores/monitorStore";

/* ------------------------------------------------------------------ */
/* Werte & Filter-Mapping                                              */
/* ------------------------------------------------------------------ */

export interface BasicCorrection {
  temperature: number; // -100..100
  tint: number; // -100..100
  exposure: number; // -100..100
  contrast: number; // -100..100
  saturation: number; // 0..200
}

export const BASIC_DEFAULTS: BasicCorrection = {
  temperature: 0,
  tint: 0,
  exposure: 0,
  contrast: 0,
  saturation: 100,
};

export type LookId = "neutral" | "warm" | "cold" | "bw";

/**
 * Annäherung der Farbkorrektur über CSS-Filter (Live-Vorschau auf dem
 * Programmmonitor — die FFmpeg-Filtergraph-Anbindung folgt):
 *
 * - Belichtung   → brightness(1 + v/100)
 * - Kontrast     → contrast(1 + v/100)
 * - Sättigung    → saturate(v/100)
 * - Temperatur   → warm (v > 0): sepia-Anteil + leichte Sättigungsanhebung;
 *                  kalt (v < 0): hue-rotate Richtung Blau
 * - Farbton      → hue-rotate (Grün ↔ Magenta nur näherungsweise)
 * - Looks        → zusätzliche sepia/hue-rotate/grayscale-Anteile,
 *                  skaliert über die Intensität
 */
function buildFilterCss(
  basic: BasicCorrection,
  look: LookId,
  intensity: number,
): string {
  const parts: string[] = [];
  let saturate = basic.saturation / 100;

  if (basic.exposure !== 0)
    parts.push(`brightness(${(1 + basic.exposure / 100).toFixed(3)})`);
  if (basic.contrast !== 0)
    parts.push(`contrast(${(1 + basic.contrast / 100).toFixed(3)})`);

  if (basic.temperature > 0) {
    parts.push(`sepia(${((basic.temperature / 100) * 0.35).toFixed(3)})`);
    saturate *= 1 + (basic.temperature / 100) * 0.15;
  } else if (basic.temperature < 0) {
    parts.push(`hue-rotate(${((basic.temperature / 100) * 30).toFixed(1)}deg)`);
  }

  if (basic.tint !== 0)
    parts.push(`hue-rotate(${((basic.tint / 100) * 25).toFixed(1)}deg)`);

  const k = intensity / 100;
  if (k > 0) {
    switch (look) {
      case "warm":
        parts.push(`sepia(${(0.3 * k).toFixed(3)})`);
        parts.push(`contrast(${(1 + 0.06 * k).toFixed(3)})`);
        break;
      case "cold":
        parts.push(`hue-rotate(${(-18 * k).toFixed(1)}deg)`);
        saturate *= 1 - 0.2 * k;
        break;
      case "bw":
        parts.push(`grayscale(${k.toFixed(3)})`);
        break;
      case "neutral":
        break;
    }
  }

  if (saturate !== 1) parts.push(`saturate(${saturate.toFixed(3)})`);
  return parts.join(" ");
}

/* ------------------------------------------------------------------ */
/* Store                                                               */
/* ------------------------------------------------------------------ */

/**
 * Farbkorrektur-Zustand des Farb-Panels. Lebt modul-global, damit Workspace-
 * Wechsel (dockview unmountet die Panels) weder die Reglerwerte noch die
 * laufende Vorschau verlieren. Jede Änderung schreibt den abgeleiteten
 * CSS-Filter-String direkt in den monitorStore.
 */
interface ColorState {
  basic: BasicCorrection;
  look: LookId;
  intensity: number;
  setBasicValue: (key: keyof BasicCorrection, value: number) => void;
  setLook: (look: LookId) => void;
  setIntensity: (intensity: number) => void;
  resetAll: () => void;
}

function applyPreview(
  basic: BasicCorrection,
  look: LookId,
  intensity: number,
) {
  useMonitorStore
    .getState()
    .setFilterCss(buildFilterCss(basic, look, intensity));
}

export const useColorStore = create<ColorState>((set, get) => ({
  basic: BASIC_DEFAULTS,
  look: "neutral",
  intensity: 100,
  setBasicValue: (key, value) => {
    const basic = { ...get().basic, [key]: value };
    set({ basic });
    const { look, intensity } = get();
    applyPreview(basic, look, intensity);
  },
  setLook: (look) => {
    set({ look });
    const { basic, intensity } = get();
    applyPreview(basic, look, intensity);
  },
  setIntensity: (intensity) => {
    set({ intensity });
    const { basic, look } = get();
    applyPreview(basic, look, intensity);
  },
  resetAll: () => {
    set({ basic: BASIC_DEFAULTS, look: "neutral", intensity: 100 });
    useMonitorStore.getState().setFilterCss("");
  },
}));
