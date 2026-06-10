import { useEffect, useRef } from "react";
import {
  ArrowRightFromLine,
  ArrowRightToLine,
  Pause,
  Play,
  SkipBack,
  SkipForward,
  StepBack,
  StepForward,
  X,
} from "lucide-react";
import { sequencePlayer } from "@/core/playback/sequencePlayer";
import { useTimelineStore } from "@/core/timeline/timelineStore";
import { SEQUENCE_FPS, sequenceEnd } from "@/core/timeline/types";
import { formatTimecode } from "@/lib/timecode";
import { useMonitorStore } from "@/stores/monitorStore";
import { usePlaybackStore } from "@/stores/playbackStore";
import { PreviewScaleSelect, TransportButton } from "./MonitorControls";

/**
 * Programmmonitor: spielt die Timeline-Sequenz ab (SequencePlayer).
 * Die Engine zeichnet in das Canvas; Scrubber und Timecode folgen dem
 * Timeline-Playhead. Der In/Out-Bereich der Sequenz (Loop) wird im
 * Scrubber markiert.
 */
export function ProgramMonitorPanel() {
  const duration = useTimelineStore((s) => sequenceEnd(s.clips));
  const hasClips = useTimelineStore((s) => s.clips.length > 0);
  const inPoint = useTimelineStore((s) => s.inPoint);
  const outPoint = useTimelineStore((s) => s.outPoint);
  const isPlaying = usePlaybackStore((s) => s.isPlaying);
  const filterCss = useMonitorStore((s) => s.filterCss);
  const scale = useMonitorStore((s) => s.scale.program);

  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const playheadRef = useRef<HTMLDivElement | null>(null);
  const timecodeRef = useRef<HTMLSpanElement | null>(null);

  const engine = sequencePlayer();

  useEffect(() => {
    engine.attachCanvas(canvasRef.current);
    return () => engine.attachCanvas(null);
  }, [engine]);

  useEffect(() => {
    engine.setRenderScale(scale);
  }, [engine, scale]);

  // Playhead-Linie und Timecode direkt am DOM nachführen — der Store
  // ändert sich während der Wiedergabe mit Frame-Rate, ein React-Re-Render
  // pro Frame wäre unnötig teuer.
  useEffect(() => {
    const apply = (t: number) => {
      if (timecodeRef.current) {
        timecodeRef.current.textContent = formatTimecode(t, SEQUENCE_FPS);
      }
      if (playheadRef.current) {
        const pct = duration > 0 ? Math.min(t / duration, 1) * 100 : 0;
        playheadRef.current.style.left = `${pct}%`;
      }
    };
    apply(useTimelineStore.getState().playheadSec);
    return useTimelineStore.subscribe((s, prev) => {
      if (s.playheadSec !== prev.playheadSec) apply(s.playheadSec);
    });
  }, [duration]);

  const seekFromPointer = (e: React.PointerEvent<HTMLDivElement>) => {
    if (duration <= 0) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const frac = Math.min(Math.max((e.clientX - rect.left) / rect.width, 0), 1);
    engine.seekTo(frac * duration);
  };

  const inPct =
    duration > 0
      ? (Math.min(Math.max(inPoint ?? 0, 0), duration) / duration) * 100
      : 0;
  const outPct =
    duration > 0
      ? (Math.min(Math.max(outPoint ?? duration, 0), duration) / duration) *
        100
      : 0;
  const showInOut =
    duration > 0 && (inPoint !== null || outPoint !== null) && outPct > inPct;
  const hasMarks = inPoint !== null || outPoint !== null;

  return (
    <div className="flex h-full flex-col bg-surface-0">
      {/* Bildfläche */}
      <div className="relative flex min-h-0 flex-1 items-center justify-center bg-black">
        <canvas
          ref={canvasRef}
          className={`h-full w-full object-contain ${hasClips ? "" : "hidden"}`}
          style={filterCss ? { filter: filterCss } : undefined}
        />
        {!hasClips && (
          <p className="max-w-72 px-4 text-center text-xs leading-5 text-text-3">
            Keine Clips in der Sequenz — Medien aus dem Medien-Browser in die
            Timeline ziehen.
          </p>
        )}
      </div>

      {/* Scrubber */}
      <div
        className="relative h-2 shrink-0 cursor-ew-resize bg-surface-2"
        onPointerDown={(e) => {
          if (duration <= 0) return;
          e.currentTarget.setPointerCapture(e.pointerId);
          seekFromPointer(e);
        }}
        onPointerMove={(e) => {
          if (
            (e.buttons & 1) !== 0 &&
            e.currentTarget.hasPointerCapture(e.pointerId)
          ) {
            seekFromPointer(e);
          }
        }}
      >
        {showInOut && (
          <div
            className="pointer-events-none absolute inset-y-0 bg-accent/30"
            style={{ left: `${inPct}%`, width: `${outPct - inPct}%` }}
          />
        )}
        {duration > 0 && (
          <div
            ref={playheadRef}
            className="pointer-events-none absolute inset-y-0 w-px bg-accent"
          />
        )}
      </div>

      {/* Transportleiste */}
      <div className="flex h-12 shrink-0 items-center gap-2 border-t border-line bg-surface-1 px-3">
        <span
          ref={timecodeRef}
          className="w-24 shrink-0 font-mono text-xs text-text-1"
        >
          {formatTimecode(0, SEQUENCE_FPS)}
        </span>
        <div className="flex min-w-0 flex-1 items-center justify-center gap-1">
          <TransportButton
            title="Zum Anfang"
            disabled={!hasClips}
            onClick={() => engine.goToStart()}
          >
            <SkipBack className="size-4" />
          </TransportButton>
          <TransportButton
            title="1 Frame zurück"
            disabled={!hasClips}
            onClick={() => engine.stepFrames(-1)}
          >
            <StepBack className="size-4" />
          </TransportButton>
          <TransportButton
            title={isPlaying ? "Pause" : "Wiedergabe"}
            disabled={!hasClips}
            onClick={() => engine.toggle()}
          >
            {isPlaying ? (
              <Pause className="size-4" />
            ) : (
              <Play className="size-4" />
            )}
          </TransportButton>
          <TransportButton
            title="1 Frame vor"
            disabled={!hasClips}
            onClick={() => engine.stepFrames(1)}
          >
            <StepForward className="size-4" />
          </TransportButton>
          <TransportButton
            title="Zum Ende"
            disabled={!hasClips}
            onClick={() => engine.goToEnd()}
          >
            <SkipForward className="size-4" />
          </TransportButton>
          <div className="mx-1 h-4 w-px bg-line" />
          <TransportButton
            title="In-Punkt setzen (Loop-Anfang)"
            disabled={!hasClips}
            onClick={() => engine.markIn()}
          >
            <ArrowRightFromLine className="size-4" />
          </TransportButton>
          <TransportButton
            title="Out-Punkt setzen (Loop-Ende)"
            disabled={!hasClips}
            onClick={() => engine.markOut()}
          >
            <ArrowRightToLine className="size-4" />
          </TransportButton>
          <TransportButton
            title="Loop-Bereich entfernen"
            disabled={!hasMarks}
            onClick={() => engine.clearMarks()}
          >
            <X className="size-4" />
          </TransportButton>
        </div>
        <div className="flex w-40 shrink-0 items-center justify-end gap-2">
          <PreviewScaleSelect monitor="program" />
          <span className="font-mono text-xs text-text-3">
            {formatTimecode(duration, SEQUENCE_FPS)}
          </span>
        </div>
      </div>
    </div>
  );
}
