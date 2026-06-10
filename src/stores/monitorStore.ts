import { create } from "zustand";
import type { MonitorId } from "@/stores/playbackStore";

/**
 * Wiedergabeauflösung der Monitore: 1 = Originalgröße, darunter wird
 * intern auf einen entsprechend kleineren Canvas gerendert, um bei
 * aufwendigem Material Rendering-Last zu sparen.
 */
export type PreviewScale = 1 | 0.5 | 0.25 | 0.125;

export const PREVIEW_SCALES: { value: PreviewScale; label: string }[] = [
  { value: 1, label: "Voll" },
  { value: 0.5, label: "1/2" },
  { value: 0.25, label: "1/4" },
  { value: 0.125, label: "1/8" },
];

/**
 * Verbindet die Monitore mit Farb- und Scopes-Panel und hält deren
 * Anzeige-Einstellungen:
 * - Das Farb-Panel schreibt `filterCss` (CSS-Filterkette), der Programmmonitor
 *   wendet sie auf seine Bildfläche an.
 * - `videoEl` ist das Videoelement des gerade sichtbaren Programm-Clips,
 *   damit Scopes daraus Frames abgreifen können.
 * - `scale` ist die Wiedergabeauflösung je Monitor.
 */
interface MonitorState {
  videoEl: HTMLVideoElement | null;
  setVideoEl: (el: HTMLVideoElement | null) => void;

  filterCss: string;
  setFilterCss: (css: string) => void;

  scale: Record<MonitorId, PreviewScale>;
  setScale: (monitor: MonitorId, scale: PreviewScale) => void;
}

export const useMonitorStore = create<MonitorState>((set) => ({
  videoEl: null,
  setVideoEl: (el) => set({ videoEl: el }),

  filterCss: "",
  setFilterCss: (css) => set({ filterCss: css }),

  scale: { source: 1, program: 1 },
  setScale: (monitor, scale) =>
    set((s) => ({ scale: { ...s.scale, [monitor]: scale } })),
}));
