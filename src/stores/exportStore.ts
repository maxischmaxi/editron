import { create } from "zustand";
import type { TranscodeOptions, TranscodeProgress } from "@/core/types";
import { ipc } from "@/lib/ipc";
import { useAppStore } from "@/stores/appStore";

/** Lebenszyklus eines Export-Jobs — überlebt das Schließen des Dialogs. */
export type ExportPhase =
  | { kind: "idle" }
  | { kind: "running"; progress: TranscodeProgress | null }
  | { kind: "done" }
  | { kind: "error"; message: string }
  | { kind: "cancelled" };

interface ExportState {
  phase: ExportPhase;
  /** Backend-Job-Id des laufenden Exports */
  jobId: string | null;
  /** Zielpfad des laufenden bzw. zuletzt beendeten Exports */
  outputPath: string | null;

  startExport: (options: TranscodeOptions) => Promise<void>;
  cancelExport: () => Promise<void>;
  /** Setzt einen beendeten Job auf idle zurück (nicht während "running"). */
  reset: () => void;
}

/** Hat der Nutzer den Abbruch angefordert? (kein Render-Zustand) */
let cancelRequested = false;

/**
 * Die transcode-Events werden EINMAL modulglobal registriert (lazy beim
 * ersten Export) und schreiben in den Store — der Job-Zustand geht damit
 * beim Schließen des Dialogs nicht verloren.
 */
let listenersRegistered = false;

function ensureListeners(): void {
  if (listenersRegistered) return;
  listenersRegistered = true;

  void ipc.onTranscodeProgress((p) => {
    const { jobId, phase } = useExportStore.getState();
    if (p.jobId !== jobId || phase.kind !== "running") return;
    useExportStore.setState({ phase: { kind: "running", progress: p } });
  });

  void ipc.onTranscodeDone((d) => {
    const { jobId, outputPath } = useExportStore.getState();
    if (d.jobId !== jobId) return;
    if (d.ok) {
      useExportStore.setState({ jobId: null, phase: { kind: "done" } });
      useAppStore
        .getState()
        .setStatusMessage(
          outputPath
            ? `Export abgeschlossen: ${outputPath}`
            : "Export abgeschlossen",
        );
    } else if (cancelRequested) {
      useExportStore.setState({ jobId: null, phase: { kind: "cancelled" } });
    } else {
      useExportStore.setState({
        jobId: null,
        phase: {
          kind: "error",
          message: d.error ?? "Unbekannter Fehler beim Export.",
        },
      });
      useAppStore.getState().setStatusMessage("Export fehlgeschlagen");
    }
  });
}

export const useExportStore = create<ExportState>((set, get) => ({
  phase: { kind: "idle" },
  jobId: null,
  outputPath: null,

  startExport: async (options) => {
    if (get().phase.kind === "running") return;
    ensureListeners();
    cancelRequested = false;
    set({
      jobId: null,
      outputPath: options.output,
      phase: { kind: "running", progress: null },
    });
    try {
      const jobId = await ipc.startTranscode(options);
      set({ jobId });
    } catch (err) {
      set({ jobId: null, phase: { kind: "error", message: String(err) } });
    }
  },

  cancelExport: async () => {
    const { jobId } = get();
    if (!jobId) return;
    cancelRequested = true;
    try {
      await ipc.cancelJob(jobId);
    } catch (err) {
      console.error("[export] Abbrechen fehlgeschlagen", err);
    }
  },

  reset: () => {
    if (get().phase.kind === "running") return;
    set({ phase: { kind: "idle" }, jobId: null, outputPath: null });
  },
}));
