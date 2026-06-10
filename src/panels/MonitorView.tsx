import { useEffect, useRef, useState } from "react";
import {
  ArrowRightFromLine,
  ArrowRightToLine,
  Music,
  Pause,
  Play,
  Repeat,
  SkipBack,
  SkipForward,
  StepBack,
  StepForward,
  X,
} from "lucide-react";
import type { MediaAsset } from "@/core/types";
import { mediaSrc } from "@/lib/ipc";
import { formatTimecode } from "@/lib/timecode";
import { useMonitorStore } from "@/stores/monitorStore";
import {
  usePlaybackStore,
  type PlaybackController,
} from "@/stores/playbackStore";
import { PreviewScaleSelect, TransportButton } from "./MonitorControls";

interface MonitorViewProps {
  asset: MediaAsset | null;
  /** Hinweistext, wenn kein Medium geladen ist. */
  emptyHint?: string;
}

const MAX_SHUTTLE_RATE = 8;
/** Mindestabstand zwischen In- und Out-Marke in Sekunden. */
const MIN_MARK_GAP = 0.02;

/**
 * Der Quellmonitor-Player: Medienfläche, Scrubber und Transportleiste
 * für ein einzelnes Asset. Registriert sich als "source"-Controller —
 * bei Fokus auf dem Quellmonitor steuern Space/JKL/I/O dieses Element.
 *
 * In-/Out-Marken sind lokal; mit aktiviertem Loop wiederholt die
 * Wiedergabe den markierten Bereich (ohne Marken den ganzen Clip).
 * Bei reduzierter Wiedergabeauflösung wird das Video unsichtbar weiter
 * dekodiert und in ein entsprechend kleineres Canvas gezeichnet.
 */
export function MonitorView({ asset, emptyHint }: MonitorViewProps) {
  const mediaRef = useRef<HTMLVideoElement | HTMLAudioElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const controllerRef = useRef<PlaybackController | null>(null);
  const drawRef = useRef<() => void>(() => {});

  const [time, setTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [inPoint, setInPoint] = useState<number | null>(null);
  const [outPoint, setOutPoint] = useState<number | null>(null);
  const [loop, setLoop] = useState(false);

  const scale = useMonitorStore((s) => s.scale.source);

  // Werte, die der (an die Asset-ID gebundene) Player-Effekt laufend
  // braucht, ohne bei jeder Änderung neu zu mounten.
  const inRef = useRef(inPoint);
  inRef.current = inPoint;
  const outRef = useRef(outPoint);
  outRef.current = outPoint;
  const loopRef = useRef(loop);
  loopRef.current = loop;
  const scaleRef = useRef(scale);
  scaleRef.current = scale;

  const fps = asset?.info.video[0]?.fps || 25;
  const hasMedia = asset !== null && asset.kind !== "image";
  const assetId = asset?.id ?? null;
  const useCanvas = asset?.kind === "video" && scale < 1;

  // Lokalen Zustand beim Medienwechsel zurücksetzen (Dauer aus ffprobe als Startwert).
  useEffect(() => {
    setTime(0);
    setPlaying(false);
    setDuration(asset?.info.durationSec ?? 0);
    setInPoint(null);
    setOutPoint(null);
    // Bewusst nur an die Asset-ID gebunden: Reset ausschließlich beim Medienwechsel.
  }, [assetId]);

  useEffect(() => {
    const el = mediaRef.current;
    if (!el) return;

    let raf = 0;
    let shuttle: number | null = null;
    // JKL-Zustand des Quellmonitors (der Programmmonitor führt eigenen).
    let rate = 1;

    const drawFrame = () => {
      const canvas = canvasRef.current;
      if (!canvas || !(el instanceof HTMLVideoElement)) return;
      if (el.videoWidth === 0 || el.videoHeight === 0) return;
      const w = Math.max(2, Math.round(el.videoWidth * scaleRef.current));
      const h = Math.max(2, Math.round(el.videoHeight * scaleRef.current));
      if (canvas.width !== w || canvas.height !== h) {
        canvas.width = w;
        canvas.height = h;
      }
      canvas.getContext("2d")?.drawImage(el, 0, 0, w, h);
    };
    drawRef.current = drawFrame;

    const syncTime = () => setTime(el.currentTime);

    const stopShuttle = () => {
      if (shuttle !== null) {
        window.clearInterval(shuttle);
        shuttle = null;
      }
    };
    const clampTime = (t: number) =>
      Math.min(Math.max(t, 0), Number.isFinite(el.duration) ? el.duration : t);
    const safePlay = () => {
      el.play().catch(() => {
        /* z. B. unterbrochen durch sofortiges pause() */
      });
    };
    /** Läuft gerade Wiedergabe in irgendeine Richtung (inkl. Rück-Shuttle)? */
    const isRunning = () => !el.paused || shuttle !== null;

    const playLoop = () => {
      // Loop: am Out-Punkt zurück zum In ("ended" deckt das Clipende ab).
      if (loopRef.current && outRef.current !== null) {
        const start = inRef.current ?? 0;
        if (outRef.current > start + MIN_MARK_GAP && el.currentTime >= outRef.current) {
          el.currentTime = start;
        }
      }
      syncTime();
      drawFrame();
      raf = requestAnimationFrame(playLoop);
    };

    const onPlay = () => {
      setPlaying(true);
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(playLoop);
    };
    const onPause = () => {
      // Während des Rück-Shuttles ist das Element pausiert, die Wiedergabe
      // läuft aber logisch weiter — der Zustand gehört dann dem Shuttle.
      if (shuttle !== null) return;
      setPlaying(false);
      cancelAnimationFrame(raf);
      syncTime();
    };
    const onEnded = () => {
      if (loopRef.current) {
        el.currentTime = inRef.current ?? 0;
        safePlay();
        return;
      }
      onPause();
    };
    const onTimeUpdate = () => {
      // Während der Wiedergabe übernimmt der rAF-Loop.
      if (el.paused) syncTime();
    };
    const onLoadedMetadata = () => {
      setDuration(Number.isFinite(el.duration) ? el.duration : 0);
      syncTime();
    };
    // Pausiert gemalte Frames (Scrubbing, Frame-Stepping) nachzeichnen.
    const drawIfIdle = () => {
      if (el.paused) drawFrame();
    };

    el.addEventListener("play", onPlay);
    el.addEventListener("pause", onPause);
    el.addEventListener("ended", onEnded);
    el.addEventListener("timeupdate", onTimeUpdate);
    el.addEventListener("loadedmetadata", onLoadedMetadata);
    el.addEventListener("seeked", drawIfIdle);
    el.addEventListener("loadeddata", drawIfIdle);
    if (el.readyState >= HTMLMediaElement.HAVE_METADATA) onLoadedMetadata();

    const controller: PlaybackController = {
      play: () => {
        stopShuttle();
        rate = 1;
        el.playbackRate = 1;
        safePlay();
      },
      pause: () => {
        stopShuttle();
        rate = 1;
        el.pause();
        setPlaying(false);
      },
      toggle: () => {
        if (isRunning()) controller.pause();
        else controller.play();
      },
      seekTo: (seconds) => {
        el.currentTime = clampTime(seconds);
        syncTime();
      },
      seekBy: (seconds) => {
        el.currentTime = clampTime(el.currentTime + seconds);
        syncTime();
      },
      stepFrames: (frames) => {
        stopShuttle();
        rate = 1;
        el.pause();
        // Nach Rück-Shuttle ist das Element schon pausiert und feuert
        // kein "pause"-Event mehr — Zustand explizit zurücksetzen.
        setPlaying(false);
        el.currentTime = clampTime(el.currentTime + frames / fps);
        syncTime();
      },
      setRate: (next) => {
        stopShuttle();
        rate = next === 0 ? 1 : next;
        if (next > 0) {
          el.playbackRate = Math.min(next, MAX_SHUTTLE_RATE);
          safePlay();
        } else if (next === 0) {
          el.pause();
          setPlaying(false);
        } else {
          // HTMLMediaElement kann nicht nativ rückwärts spielen:
          // Shuttle springt alle 250 ms um |rate| * 0,25 s zurück.
          el.pause();
          cancelAnimationFrame(raf);
          setPlaying(true);
          shuttle = window.setInterval(() => {
            const step = Math.abs(next) * 0.25;
            if (el.currentTime - step <= 0) {
              el.currentTime = 0;
              stopShuttle();
              setPlaying(false);
            } else {
              el.currentTime -= step;
            }
            syncTime();
            drawFrame();
          }, 250);
        }
      },
      shuttle: (direction) => {
        const next =
          direction === 1
            ? !isRunning() || rate <= 0
              ? 1
              : Math.min(rate * 2, MAX_SHUTTLE_RATE)
            : rate >= 0 || !isRunning()
              ? -1
              : Math.max(rate * 2, -MAX_SHUTTLE_RATE);
        controller.setRate(next);
      },
      goToStart: () => {
        el.currentTime = 0;
        syncTime();
      },
      goToEnd: () => {
        if (Number.isFinite(el.duration)) el.currentTime = el.duration;
        syncTime();
      },
      markIn: () => {
        const t = el.currentTime;
        setInPoint(t);
        setOutPoint((out) =>
          out !== null && out <= t + MIN_MARK_GAP ? null : out,
        );
      },
      markOut: () => {
        const t = el.currentTime;
        setOutPoint(t);
        setInPoint((p) => (p !== null && p >= t - MIN_MARK_GAP ? null : p));
      },
      clearMarks: () => {
        setInPoint(null);
        setOutPoint(null);
      },
    };

    controllerRef.current = controller;
    usePlaybackStore.getState().registerController("source", controller);

    return () => {
      cancelAnimationFrame(raf);
      stopShuttle();
      el.removeEventListener("play", onPlay);
      el.removeEventListener("pause", onPause);
      el.removeEventListener("ended", onEnded);
      el.removeEventListener("timeupdate", onTimeUpdate);
      el.removeEventListener("loadedmetadata", onLoadedMetadata);
      el.removeEventListener("seeked", drawIfIdle);
      el.removeEventListener("loadeddata", drawIfIdle);
      if (controllerRef.current === controller) controllerRef.current = null;
      const pb = usePlaybackStore.getState();
      if (pb.controllers.source === controller) {
        pb.registerController("source", null);
      }
      drawRef.current = () => {};
    };
  }, [assetId, fps]);

  // Wechsel der Wiedergabeauflösung bzw. des Render-Pfads: das aktuelle
  // Frame sofort in den (ggf. frisch gemounteten) Canvas nachzeichnen.
  useEffect(() => {
    if (useCanvas) drawRef.current();
  }, [useCanvas, scale, assetId]);

  const seekFromPointer = (e: React.PointerEvent<HTMLDivElement>) => {
    if (duration <= 0) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const frac = Math.min(Math.max((e.clientX - rect.left) / rect.width, 0), 1);
    controllerRef.current?.seekTo(frac * duration);
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
    duration > 0 &&
    (inPoint !== null || outPoint !== null) &&
    outPct > inPct;
  const hasMarks = inPoint !== null || outPoint !== null;

  return (
    <div className="flex h-full flex-col bg-surface-0">
      {/* Medienfläche */}
      <div className="relative flex min-h-0 flex-1 items-center justify-center bg-black">
        {!asset && (
          <p className="max-w-72 px-4 text-center text-xs leading-5 text-text-3">
            {emptyHint ?? "Kein Medium geladen."}
          </p>
        )}
        {asset?.kind === "video" && (
          <>
            <video
              key={asset.id}
              ref={(el) => {
                mediaRef.current = el;
              }}
              src={mediaSrc(asset.path)}
              className={
                useCanvas
                  ? "invisible absolute inset-0 h-full w-full"
                  : "h-full w-full object-contain"
              }
              playsInline
              // Der Canvas-Pfad zeichnet erst ab "loadeddata" — dafür
              // muss mehr als nur Metadaten geladen werden.
              preload={useCanvas ? "auto" : "metadata"}
            />
            {useCanvas && (
              <canvas
                ref={canvasRef}
                className="h-full w-full object-contain"
              />
            )}
          </>
        )}
        {asset?.kind === "audio" && (
          <>
            <Music className="size-16 text-text-3" />
            <audio
              key={asset.id}
              ref={(el) => {
                mediaRef.current = el;
              }}
              src={mediaSrc(asset.path)}
              className="hidden"
              preload="metadata"
            />
          </>
        )}
        {asset?.kind === "image" && (
          <img
            key={asset.id}
            src={mediaSrc(asset.path)}
            alt={asset.name}
            className="h-full w-full object-contain"
            draggable={false}
          />
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
            className="pointer-events-none absolute inset-y-0 w-px bg-accent"
            style={{ left: `${(time / duration) * 100}%` }}
          />
        )}
      </div>

      {/* Transportleiste */}
      <div className="flex h-12 shrink-0 items-center gap-2 border-t border-line bg-surface-1 px-3">
        <span className="w-24 shrink-0 font-mono text-xs text-text-1">
          {formatTimecode(time, fps)}
        </span>
        <div className="flex min-w-0 flex-1 items-center justify-center gap-1">
          <TransportButton
            title="Zum Anfang"
            disabled={!hasMedia}
            onClick={() => controllerRef.current?.goToStart()}
          >
            <SkipBack className="size-4" />
          </TransportButton>
          <TransportButton
            title="1 Frame zurück"
            disabled={!hasMedia}
            onClick={() => controllerRef.current?.stepFrames(-1)}
          >
            <StepBack className="size-4" />
          </TransportButton>
          <TransportButton
            title={playing ? "Pause" : "Wiedergabe"}
            disabled={!hasMedia}
            onClick={() => controllerRef.current?.toggle()}
          >
            {playing ? (
              <Pause className="size-4" />
            ) : (
              <Play className="size-4" />
            )}
          </TransportButton>
          <TransportButton
            title="1 Frame vor"
            disabled={!hasMedia}
            onClick={() => controllerRef.current?.stepFrames(1)}
          >
            <StepForward className="size-4" />
          </TransportButton>
          <TransportButton
            title="Zum Ende"
            disabled={!hasMedia}
            onClick={() => controllerRef.current?.goToEnd()}
          >
            <SkipForward className="size-4" />
          </TransportButton>
          <div className="mx-1 h-4 w-px bg-line" />
          <TransportButton
            title="In-Punkt setzen"
            disabled={!hasMedia}
            onClick={() => controllerRef.current?.markIn()}
          >
            <ArrowRightFromLine className="size-4" />
          </TransportButton>
          <TransportButton
            title="Out-Punkt setzen"
            disabled={!hasMedia}
            onClick={() => controllerRef.current?.markOut()}
          >
            <ArrowRightToLine className="size-4" />
          </TransportButton>
          <TransportButton
            title="In/Out entfernen"
            disabled={!hasMarks}
            onClick={() => controllerRef.current?.clearMarks()}
          >
            <X className="size-4" />
          </TransportButton>
          <TransportButton
            title="Loop-Wiedergabe (zwischen In/Out)"
            disabled={!hasMedia}
            active={loop}
            onClick={() => setLoop((v) => !v)}
          >
            <Repeat className="size-4" />
          </TransportButton>
        </div>
        <div className="flex w-40 shrink-0 items-center justify-end gap-2">
          <PreviewScaleSelect monitor="source" />
          <span className="font-mono text-xs text-text-3">
            {formatTimecode(duration, fps)}
          </span>
        </div>
      </div>
    </div>
  );
}
