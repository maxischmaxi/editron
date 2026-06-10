import { create } from "zustand";
import { getContextValues } from "@/core/commands/context";

export type MonitorId = "source" | "program";

/**
 * Imperatives Interface eines Monitors. Der Quellmonitor registriert es
 * für sein Medienelement, der Programmmonitor wird vom SequencePlayer
 * (Timeline-Wiedergabe) bedient. Commands (Space, JKL, I/O, Pfeiltasten …)
 * steuern darüber den jeweils aktiven Monitor.
 */
export interface PlaybackController {
  play(): void;
  pause(): void;
  toggle(): void;
  seekTo(seconds: number): void;
  seekBy(seconds: number): void;
  stepFrames(frames: number): void;
  /** JKL-Shuttle: negative Werte = rückwärts */
  setRate(rate: number): void;
  /** Ein JKL-Druck: startet bei ±1, verdoppelt bis ±8, Richtungswechsel resettet. */
  shuttle(direction: 1 | -1): void;
  goToStart(): void;
  goToEnd(): void;
  /** In-/Out-Marke an der aktuellen Position setzen bzw. löschen. */
  markIn(): void;
  markOut(): void;
  clearMarks(): void;
}

interface PlaybackState {
  /** Asset, das aktuell im Quellmonitor geladen ist. */
  sourceAssetId: string | null;
  setSourceAsset: (id: string | null) => void;

  /** Wiedergabestatus des Programmmonitors (Timeline), vom SequencePlayer gemeldet. */
  isPlaying: boolean;
  setIsPlaying: (playing: boolean) => void;
  rate: number;
  setRate: (r: number) => void;

  controllers: Record<MonitorId, PlaybackController | null>;
  registerController: (monitor: MonitorId, c: PlaybackController | null) => void;
}

export const usePlaybackStore = create<PlaybackState>((set) => ({
  sourceAssetId: null,
  setSourceAsset: (id) => set({ sourceAssetId: id }),

  isPlaying: false,
  setIsPlaying: (playing) => set({ isPlaying: playing }),

  rate: 1,
  setRate: (r) => set({ rate: r }),

  controllers: { source: null, program: null },
  registerController: (monitor, c) =>
    set((s) => ({ controllers: { ...s.controllers, [monitor]: c } })),
}));

/**
 * Controller des aktiven Monitors für Commands außerhalb von React:
 * Fokus auf dem Quellmonitor-Panel steuert die Quelle, alles andere
 * (Timeline, Programm, Medien-Browser …) den Programmmonitor.
 */
export function activePlayback(): PlaybackController | null {
  const { controllers } = usePlaybackStore.getState();
  if (getContextValues().panel === "source" && controllers.source) {
    return controllers.source;
  }
  return controllers.program;
}
