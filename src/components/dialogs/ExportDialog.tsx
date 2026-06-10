import { useEffect, useRef, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import {
  Ban,
  CircleAlert,
  CircleCheck,
  FolderOpen,
  LoaderCircle,
  X,
} from "lucide-react";
import type { TranscodeOptions } from "@/core/types";
import { useAppStore } from "@/stores/appStore";
import { useExportStore } from "@/stores/exportStore";
import { useMediaStore } from "@/stores/mediaStore";
import { usePlaybackStore } from "@/stores/playbackStore";
import { formatTimecode } from "@/lib/timecode";

/* ------------------------------------------------------------------ */
/* Presets                                                             */
/* ------------------------------------------------------------------ */

interface ExportPreset {
  id: string;
  label: string;
  ext: string;
  videoCodec?: string;
  audioCodec: string;
  crf?: { min: number; max: number; default: number };
  ffmpegPreset?: string;
  audioOnly?: boolean;
}

const PRESETS: ExportPreset[] = [
  {
    id: "h264",
    label: "H.264 / MP4",
    ext: "mp4",
    videoCodec: "libx264",
    audioCodec: "aac",
    crf: { min: 14, max: 32, default: 21 },
    ffmpegPreset: "medium",
  },
  {
    id: "h265",
    label: "H.265 / MP4",
    ext: "mp4",
    videoCodec: "libx265",
    audioCodec: "aac",
    crf: { min: 18, max: 36, default: 26 },
    ffmpegPreset: "medium",
  },
  {
    id: "vp9",
    label: "WebM / VP9",
    ext: "webm",
    videoCodec: "libvpx-vp9",
    audioCodec: "libopus",
    crf: { min: 18, max: 44, default: 32 },
  },
  {
    id: "audio",
    label: "Nur Audio / M4A",
    ext: "m4a",
    audioCodec: "aac",
    audioOnly: true,
  },
];

type ResolutionId = "original" | "1080" | "720";

const RESOLUTIONS: { id: ResolutionId; label: string }[] = [
  { id: "original", label: "Original" },
  { id: "1080", label: "1080p" },
  { id: "720", label: "720p" },
];

function stripExtension(path: string): string {
  return path.replace(/\.[^./\\]+$/, "");
}

function replaceExtension(path: string, ext: string): string {
  return `${stripExtension(path)}.${ext}`;
}

/** Alle per Tab erreichbaren Bedienelemente innerhalb des Dialogs. */
function focusableIn(root: HTMLElement): HTMLElement[] {
  return Array.from(
    root.querySelectorAll<HTMLElement>(
      "button:not(:disabled), select:not(:disabled), input:not(:disabled)",
    ),
  ).filter((el) => el.tabIndex !== -1);
}

/* ------------------------------------------------------------------ */
/* Dialog                                                              */
/* ------------------------------------------------------------------ */

export function ExportDialog() {
  const open = useAppStore((s) => s.openDialog === "export");
  if (!open) return null;
  return <ExportDialogContent />;
}

const LABEL_CLASS = "w-24 shrink-0 pt-1.5 text-xs text-text-2";
const FIELD_CLASS =
  "h-7 w-full rounded border border-line bg-surface-3 px-2 text-xs text-text-1 focus:border-accent focus:outline-none";
/** Zieldatei ist reine Ausgabe — Interaktion läuft über „Durchsuchen …“. */
const OUTPUT_CLASS =
  "h-7 w-full cursor-default truncate rounded border border-line bg-surface-1 px-2 text-xs text-text-2 placeholder:text-text-3 focus:outline-none";

function ExportDialogContent() {
  const assets = useMediaStore((s) => s.assets);
  const sourceAssetId = usePlaybackStore((s) => s.sourceAssetId);

  /* Job-Zustand lebt im exportStore und überlebt das Schließen des Dialogs */
  const phase = useExportStore((s) => s.phase);
  const outputPath = useExportStore((s) => s.outputPath);

  const [sourceId, setSourceId] = useState<string>(() => {
    const active = assets.find((a) => a.id === sourceAssetId);
    return active?.id ?? assets[0]?.id ?? "";
  });
  const [output, setOutput] = useState("");
  const [presetId, setPresetId] = useState("h264");
  const [crf, setCrf] = useState(21);
  const [resolution, setResolution] = useState<ResolutionId>("original");

  const panelRef = useRef<HTMLDivElement>(null);
  const sourceSelectRef = useRef<HTMLSelectElement>(null);

  const sourceAsset = assets.find((a) => a.id === sourceId) ?? null;
  const preset = PRESETS.find((p) => p.id === presetId) ?? PRESETS[0];
  const sourceVideo = sourceAsset?.info.video[0] ?? null;
  const running = phase.kind === "running";

  const close = () => useAppStore.getState().setOpenDialog(null);

  /* Initial-Fokus: erstes sinnvolles Bedienelement statt Hintergrund */
  useEffect(() => {
    const panel = panelRef.current;
    if (!panel) return;
    const select = sourceSelectRef.current;
    if (select && !select.disabled) {
      select.focus();
      return;
    }
    /* Fallback (z. B. laufender Job): erstes Element nach dem Schließen-Knopf */
    const items = focusableIn(panel);
    (items[1] ?? items[0])?.focus();
  }, []);

  /* Escape schließt den Dialog (laufender Job läuft weiter),
     Tab bleibt zyklisch innerhalb des Dialogs */
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        useAppStore.getState().setOpenDialog(null);
        return;
      }
      if (e.key === "Tab") {
        const panel = panelRef.current;
        if (!panel) return;
        const items = focusableIn(panel);
        if (items.length === 0) return;
        const first = items[0];
        const last = items[items.length - 1];
        const active = document.activeElement;
        if (e.shiftKey) {
          if (active === first || !panel.contains(active)) {
            e.preventDefault();
            last.focus();
          }
        } else if (active === last || !panel.contains(active)) {
          e.preventDefault();
          first.focus();
        }
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const selectPreset = (id: string) => {
    const next = PRESETS.find((p) => p.id === id);
    if (!next) return;
    setPresetId(id);
    if (next.crf) setCrf(next.crf.default);
    setOutput((cur) => (cur === "" ? cur : replaceExtension(cur, next.ext)));
  };

  const browse = async () => {
    const defaultPath = sourceAsset
      ? `${stripExtension(sourceAsset.path)}_export.${preset.ext}`
      : undefined;
    const picked = await save({
      title: "Zieldatei wählen",
      defaultPath,
      filters: [{ name: preset.label, extensions: [preset.ext] }],
    });
    if (!picked) return;
    /* Extension nur anhängen, wenn sie fehlt — der finale Pfad steht damit
       sichtbar im Zieldatei-Feld, BEVOR der Export startet. */
    const withExt = picked.toLowerCase().endsWith(`.${preset.ext}`)
      ? picked
      : `${picked}.${preset.ext}`;
    setOutput(withExt);
  };

  /** Beide Dimensionen proportional aus der Quellauflösung (gerade Werte). */
  const targetDimensions = (): { width?: number; height?: number } => {
    if (preset.audioOnly || resolution === "original") return {};
    if (!sourceVideo || sourceVideo.height <= 0) return {};
    const height = resolution === "1080" ? 1080 : 720;
    const width = Math.max(
      2,
      Math.round((sourceVideo.width * height) / sourceVideo.height / 2) * 2,
    );
    return { width, height };
  };

  const start = () => {
    if (!sourceAsset || output === "" || running) return;
    const options: TranscodeOptions = {
      input: sourceAsset.path,
      output,
      audioCodec: preset.audioCodec,
      ...(preset.audioOnly
        ? {}
        : {
            videoCodec: preset.videoCodec,
            crf,
            ...(preset.ffmpegPreset ? { preset: preset.ffmpegPreset } : {}),
            ...targetDimensions(),
          }),
    };
    void useExportStore.getState().startExport(options);
  };

  const cancel = () => {
    void useExportStore.getState().cancelExport();
  };

  const progress = phase.kind === "running" ? phase.progress : null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
      onMouseDown={(e) => {
        /* Backdrop-Klick schließt — aber nicht, während ein Job läuft */
        if (e.target === e.currentTarget && !running) close();
      }}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="export-dialog-title"
        className="w-full max-w-lg rounded-lg border border-line-strong bg-surface-2 shadow-xl"
      >
        {/* Header */}
        <div className="flex h-10 items-center justify-between border-b border-line px-4">
          <h2 id="export-dialog-title" className="text-sm font-medium text-text-1">
            Exportieren
          </h2>
          <button
            type="button"
            onClick={close}
            title="Schließen"
            className="rounded p-1 text-text-3 hover:bg-surface-3 hover:text-text-1"
          >
            <X size={16} />
          </button>
        </div>

        <div className="space-y-3 p-4">
          {/* Quelle */}
          <div className="flex gap-3">
            <span className={LABEL_CLASS}>Quelle</span>
            <select
              ref={sourceSelectRef}
              value={sourceId}
              onChange={(e) => setSourceId(e.target.value)}
              disabled={running || assets.length === 0}
              className={FIELD_CLASS}
            >
              {assets.length === 0 && (
                <option value="">Keine Medien importiert</option>
              )}
              {assets.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.name}
                </option>
              ))}
            </select>
          </div>

          {/* Zieldatei */}
          <div className="flex gap-3">
            <span className={LABEL_CLASS}>Zieldatei</span>
            <div className="flex min-w-0 flex-1 gap-2">
              <input
                type="text"
                readOnly
                tabIndex={-1}
                value={output}
                placeholder="Noch kein Ziel gewählt"
                title={output === "" ? undefined : output}
                className={OUTPUT_CLASS}
              />
              <button
                type="button"
                onClick={browse}
                disabled={running}
                className="flex h-7 shrink-0 items-center gap-1.5 rounded border border-line px-2 text-xs text-text-1 hover:border-line-strong hover:bg-surface-3 disabled:cursor-not-allowed disabled:opacity-50"
              >
                <FolderOpen size={14} />
                Durchsuchen …
              </button>
            </div>
          </div>

          {/* Format */}
          <div className="flex gap-3">
            <span className={LABEL_CLASS}>Format</span>
            <select
              value={presetId}
              onChange={(e) => selectPreset(e.target.value)}
              disabled={running}
              className={FIELD_CLASS}
            >
              {PRESETS.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.label}
                </option>
              ))}
            </select>
          </div>

          {/* Qualität (CRF) */}
          {preset.crf && (
            <div className="flex gap-3">
              <span className={LABEL_CLASS}>Qualität (CRF)</span>
              <div className="flex min-w-0 flex-1 items-center gap-2 pt-1">
                <input
                  type="range"
                  min={preset.crf.min}
                  max={preset.crf.max}
                  step={1}
                  value={crf}
                  onChange={(e) => setCrf(Number(e.target.value))}
                  disabled={running}
                  className="h-1 min-w-0 flex-1 cursor-pointer accent-accent"
                />
                <span className="w-6 shrink-0 text-right font-mono text-xs text-text-1">
                  {crf}
                </span>
              </div>
            </div>
          )}

          {/* Auflösung */}
          {!preset.audioOnly && (
            <div className="flex gap-3">
              <span className={LABEL_CLASS}>Auflösung</span>
              <select
                value={resolution}
                onChange={(e) => setResolution(e.target.value as ResolutionId)}
                disabled={running}
                className={FIELD_CLASS}
              >
                {RESOLUTIONS.map((r) => (
                  <option key={r.id} value={r.id}>
                    {r.label}
                    {r.id === "original" && sourceVideo
                      ? ` (${sourceVideo.width}×${sourceVideo.height})`
                      : ""}
                  </option>
                ))}
              </select>
            </div>
          )}

          {/* Status */}
          {phase.kind === "running" && (
            <div className="space-y-2 rounded border border-line bg-surface-1 p-3">
              <div className="flex items-center gap-2 text-xs text-text-1">
                <LoaderCircle size={14} className="animate-spin text-accent" />
                Export läuft …
              </div>
              {outputPath && (
                <p
                  className="truncate font-mono text-xs text-text-2"
                  title={outputPath}
                >
                  {outputPath}
                </p>
              )}
              <div className="h-1.5 overflow-hidden rounded-full bg-surface-4">
                {progress?.progressPct != null ? (
                  <div
                    className="h-full rounded-full bg-accent transition-all"
                    style={{
                      width: `${Math.min(100, Math.max(0, progress.progressPct))}%`,
                    }}
                  />
                ) : (
                  <div className="h-full w-1/3 animate-pulse rounded-full bg-accent" />
                )}
              </div>
              <div className="flex justify-between font-mono text-xs text-text-2">
                <span>{formatTimecode(progress?.outTimeSec ?? 0)}</span>
                <span>
                  {progress?.speed != null
                    ? `${progress.speed.toFixed(1)}x`
                    : "—"}
                </span>
                <span>
                  {progress?.fps != null
                    ? `${Math.round(progress.fps)} fps`
                    : "—"}
                </span>
              </div>
              <p className="text-xs text-text-3">
                Dialog schließen bricht den Export nicht ab — der Job läuft im
                Hintergrund weiter.
              </p>
            </div>
          )}

          {phase.kind === "done" && (
            <div className="flex items-start gap-2 rounded border border-line bg-surface-1 p-3">
              <CircleCheck size={16} className="mt-0.5 shrink-0 text-success" />
              <div className="min-w-0">
                <p className="text-xs font-medium text-text-1">
                  Export abgeschlossen
                </p>
                <p
                  className="truncate font-mono text-xs text-text-2"
                  title={outputPath ?? undefined}
                >
                  {outputPath}
                </p>
              </div>
            </div>
          )}

          {phase.kind === "error" && (
            <div className="flex items-start gap-2 rounded border border-line bg-surface-1 p-3">
              <CircleAlert size={16} className="mt-0.5 shrink-0 text-danger" />
              <div className="min-w-0">
                <p className="text-xs font-medium text-danger">
                  Export fehlgeschlagen
                </p>
                <p className="text-xs break-words text-text-2">
                  {phase.message}
                </p>
              </div>
            </div>
          )}

          {phase.kind === "cancelled" && (
            <div className="flex items-center gap-2 rounded border border-line bg-surface-1 p-3">
              <Ban size={16} className="shrink-0 text-warning" />
              <p className="text-xs font-medium text-text-1">
                Export abgebrochen
              </p>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex h-12 items-center justify-end gap-2 border-t border-line px-4">
          {phase.kind === "running" ? (
            <>
              <button
                type="button"
                onClick={cancel}
                className="h-7 rounded border border-line px-3 text-xs text-danger hover:border-danger/50 hover:bg-danger/10"
              >
                Abbrechen
              </button>
              <button
                type="button"
                disabled
                title="Es läuft bereits ein Export — abwarten oder abbrechen."
                className="h-7 rounded bg-accent px-3 text-xs font-medium text-white disabled:cursor-not-allowed disabled:opacity-50"
              >
                Export starten
              </button>
            </>
          ) : (
            <>
              {(phase.kind === "done" ||
                phase.kind === "error" ||
                phase.kind === "cancelled") && (
                <button
                  type="button"
                  onClick={() => useExportStore.getState().reset()}
                  className="h-7 rounded border border-line px-3 text-xs text-text-2 hover:border-line-strong hover:bg-surface-3 hover:text-text-1"
                >
                  Neuer Export
                </button>
              )}
              <button
                type="button"
                onClick={close}
                className="h-7 rounded border border-line px-3 text-xs text-text-2 hover:border-line-strong hover:bg-surface-3 hover:text-text-1"
              >
                Schließen
              </button>
              {phase.kind === "idle" && (
                <button
                  type="button"
                  onClick={start}
                  disabled={output === "" || !sourceAsset}
                  className="h-7 rounded bg-accent px-3 text-xs font-medium text-white hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
                >
                  Export starten
                </button>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
