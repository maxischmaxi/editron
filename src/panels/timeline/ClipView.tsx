import { memo, useEffect, useRef } from "react";
import type { PointerEvent as ReactPointerEvent, MouseEvent } from "react";
import { Link2 } from "lucide-react";
import type { MediaAsset } from "@/core/types";
import type { TimelineClip } from "@/core/timeline/types";
import { getWaveform } from "@/core/timeline/waveforms";
import { mediaSrc } from "@/lib/ipc";
import { formatDuration } from "@/lib/timecode";

/** Canvas-Breite deckeln — bei hohem Zoom wird die Wellenform gestreckt. */
const MAX_CANVAS_W = 4096;
const WAVE_COLOR = "rgba(62, 224, 143, 0.5)";

function WaveformCanvas({
  path,
  srcIn,
  srcDuration,
  duration,
  widthPx,
}: {
  path: string;
  srcIn: number;
  srcDuration: number;
  duration: number;
  widthPx: number;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    let alive = true;
    void getWaveform(path).then((peaks) => {
      const canvas = canvasRef.current;
      if (!alive || !canvas || !peaks || peaks.length === 0) return;
      const w = Math.max(1, Math.min(Math.round(widthPx), MAX_CANVAS_W));
      const h = canvas.clientHeight || 36;
      canvas.width = w;
      canvas.height = h;
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.clearRect(0, 0, w, h);
      ctx.fillStyle = WAVE_COLOR;
      // Sichtbaren Quellausschnitt [srcIn, srcIn+duration] auf die Peaks mappen.
      const total =
        Number.isFinite(srcDuration) && srcDuration > 0 ? srcDuration : duration;
      const from = (srcIn / total) * peaks.length;
      const span = Math.max(1, (duration / total) * peaks.length);
      for (let x = 0; x < w; x++) {
        const idx = Math.min(
          peaks.length - 1,
          Math.max(0, Math.floor(from + (x / w) * span)),
        );
        const v = peaks[idx] ?? 0;
        const barH = Math.max(1, v * (h - 2));
        ctx.fillRect(x, (h - barH) / 2, 1, barH);
      }
    });
    return () => {
      alive = false;
    };
  }, [path, srcIn, srcDuration, duration, widthPx]);

  return (
    <canvas
      ref={canvasRef}
      className="pointer-events-none absolute inset-0 h-full w-full"
    />
  );
}

export interface ClipViewProps {
  clip: TimelineClip;
  asset: MediaAsset | undefined;
  zoom: number;
  selected: boolean;
  locked: boolean;
  razor: boolean;
  onPointerDown: (e: ReactPointerEvent<HTMLDivElement>, clip: TimelineClip) => void;
  onContextMenu: (e: MouseEvent, clip: TimelineClip) => void;
  onDoubleClick: (clip: TimelineClip) => void;
}

/**
 * Ein Clip in der Timeline: Video blau (Thumbnail links), Audio grün
 * (Wellenform), Auswahl-Ring, Trim-Griffe an den Kanten, Link- und
 * Deaktiviert-Zustand.
 */
export const ClipView = memo(function ClipView({
  clip,
  asset,
  zoom,
  selected,
  locked,
  razor,
  onPointerDown,
  onContextMenu,
  onDoubleClick,
}: ClipViewProps) {
  const left = Math.round(clip.start * zoom);
  const width = Math.max(Math.round(clip.duration * zoom), 3);
  const isAudio = clip.kind === "audio";

  return (
    <div
      role="button"
      tabIndex={-1}
      onPointerDown={(e) => onPointerDown(e, clip)}
      onContextMenu={(e) => onContextMenu(e, clip)}
      onDoubleClick={() => onDoubleClick(clip)}
      title={clip.name}
      className={`group absolute inset-y-0.5 touch-none select-none overflow-hidden rounded border ${
        isAudio
          ? "border-success/60 bg-success/15"
          : "border-accent bg-accent-soft/80"
      } ${selected ? "ring-2 ring-text-1" : ""} ${
        clip.enabled ? "" : "opacity-40 saturate-0"
      } ${locked ? "cursor-not-allowed" : razor ? "cursor-crosshair" : "cursor-default"}`}
      style={{ left, width }}
    >
      {isAudio ? (
        asset && (
          <WaveformCanvas
            path={asset.path}
            srcIn={clip.srcIn}
            srcDuration={clip.srcDuration}
            duration={clip.duration}
            widthPx={width}
          />
        )
      ) : (
        asset?.thumbnailPath &&
        width > 48 && (
          <img
            src={mediaSrc(asset.thumbnailPath)}
            alt=""
            draggable={false}
            className="pointer-events-none absolute left-0 top-0 h-full w-auto opacity-50"
          />
        )
      )}

      <div className="pointer-events-none relative flex h-full min-w-0 items-start gap-1 px-1.5 py-0.5">
        {clip.linkId !== null && (
          <Link2 className="mt-0.5 size-3 shrink-0 text-text-2" />
        )}
        <span className="min-w-0 flex-1 truncate text-xs text-text-1">
          {clip.name}
        </span>
        {width > 88 && (
          <span className="shrink-0 font-mono text-xs text-text-2">
            {formatDuration(clip.duration)}
          </span>
        )}
      </div>

      {/* Trim-Griffe (visuell; die Kanten-Erkennung läuft über offsetX) */}
      {!locked && width > 24 && (
        <>
          <div className="absolute inset-y-0 left-0 w-1.5 cursor-ew-resize bg-text-1/40 opacity-0 group-hover:opacity-100" />
          <div className="absolute inset-y-0 right-0 w-1.5 cursor-ew-resize bg-text-1/40 opacity-0 group-hover:opacity-100" />
        </>
      )}
    </div>
  );
});
