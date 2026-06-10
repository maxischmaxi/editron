import { useTimelineStore } from "@/core/timeline/timelineStore";
import {
  SEQUENCE_FPS,
  sequenceEnd,
  type TimelineClip,
  type TimelineTrack,
} from "@/core/timeline/types";
import { mediaSrc } from "@/lib/ipc";
import { useMediaStore } from "@/stores/mediaStore";
import { useMonitorStore } from "@/stores/monitorStore";
import {
  usePlaybackStore,
  type PlaybackController,
} from "@/stores/playbackStore";

const EPS = 1e-6;
const MAX_SHUTTLE_RATE = 8;
/** Clips, die innerhalb dieses Fensters starten, werden vorgeladen. */
const PRELOAD_AHEAD_SEC = 2;
/** Maximal gleichzeitig gehaltene Medienelemente. */
const MAX_POOL = 16;
/** Drift-Toleranz während der Wiedergabe bzw. beim pausierten Positionieren. */
const DRIFT_PLAYING = 0.12;
const DRIFT_PAUSED = 0.005;
/** Intervall und Schrittfaktor des Rückwärts-Shuttles. */
const REVERSE_TICK_MS = 250;

interface PoolEntry {
  clipId: string;
  assetId: string;
  el: HTMLVideoElement | HTMLAudioElement | HTMLImageElement;
}

function isMediaEl(
  el: PoolEntry["el"],
): el is HTMLVideoElement | HTMLAudioElement {
  return el instanceof HTMLMediaElement;
}

/**
 * Sichtbarkeits-/Hörbarkeitsfilter einer Spur: Solo gewinnt innerhalb
 * der Spurart, sonst zählt Mute. Gilt für Video (Sichtbarkeit) und
 * Audio (Hörbarkeit) gleichermaßen.
 */
function trackActive(track: TimelineTrack, tracks: TimelineTrack[]): boolean {
  const anySolo = tracks.some((t) => t.kind === track.kind && t.solo);
  if (anySolo) return track.solo;
  return !track.muted;
}

function clipAt(clip: TimelineClip, t: number): boolean {
  return t >= clip.start - EPS && t < clip.start + clip.duration - EPS;
}

/**
 * Wiedergabe-Engine des Programmmonitors: spielt die Timeline-Sequenz ab.
 *
 * Hält einen Pool unsichtbarer HTMLMedia-Elemente (eines je Clip in
 * Playhead-Nähe), synchronisiert sie über eine performance.now()-Master-
 * Clock auf die Sequenzzeit und schreibt den Playhead in den timelineStore
 * zurück. Das sichtbare Videobild wird in ein vom Panel angedocktes
 * Canvas gezeichnet — dessen Backing-Auflösung skaliert mit der
 * Wiedergabeauflösung (Voll, 1/2, 1/4, 1/8).
 */
class SequencePlayer implements PlaybackController {
  private pool = new Map<string, PoolEntry>();
  private canvas: HTMLCanvasElement | null = null;

  private playing = false;
  private rate = 1;
  private renderScale = 1;

  private raf = 0;
  private reverseTimer: number | null = null;
  private drawQueued = false;

  /** Master-Clock: Sequenzzeit + performance.now()-Anker beim Start. */
  private clockSeq = 0;
  private clockPerf = 0;
  private lastTickT = 0;

  /** Eigene Playhead-Writes nicht als externe Seeks interpretieren. */
  private writingPlayhead = false;

  private visibleEl: HTMLVideoElement | HTMLImageElement | null = null;

  constructor() {
    useTimelineStore.subscribe((s, prev) => {
      if (s.playheadSec !== prev.playheadSec && !this.writingPlayhead) {
        this.onExternalSeek(s.playheadSec);
      }
      if (s.clips !== prev.clips || s.tracks !== prev.tracks) {
        this.pruneStaleEntries(s.clips);
        this.syncElements();
        this.requestDraw();
      }
    });
  }

  // ------------------------------------------------------------ Anbindung

  /** Vom Programmmonitor-Panel beim Mount/Unmount aufgerufen. */
  attachCanvas(canvas: HTMLCanvasElement | null): void {
    if (canvas === null && this.canvas === null) return;
    this.canvas = canvas;
    this.requestDraw();
  }

  /** Wiedergabeauflösung (1, 1/2, 1/4, 1/8) — vom Panel gemeldet. */
  setRenderScale(scale: number): void {
    if (scale === this.renderScale) return;
    this.renderScale = scale;
    this.requestDraw();
  }

  // ----------------------------------------------------------- Controller

  play(): void {
    this.stopReverse();
    this.rate = 1;
    usePlaybackStore.getState().setRate(1);
    const end = this.seqEnd();
    if (end <= 0) return;
    // Am Sequenzende (bzw. am Loop-Out) startet Space wieder von vorn.
    const t = this.playhead();
    if (t >= end - EPS) {
      this.writePlayhead(this.loopRange()?.in ?? 0);
    }
    this.startClock();
  }

  pause(): void {
    this.stopReverse();
    this.rate = 1;
    usePlaybackStore.getState().setRate(1);
    this.stopClock();
  }

  toggle(): void {
    if (this.playing) this.pause();
    else this.play();
  }

  seekTo(seconds: number): void {
    const t = Math.min(Math.max(seconds, 0), this.seqEnd());
    this.writePlayhead(t);
    // Beim Rück-Shuttle läuft das Intervall ab der neuen Position weiter.
    if (this.playing && this.reverseTimer === null) this.startClock();
    this.syncElements();
    this.requestDraw();
  }

  seekBy(seconds: number): void {
    this.seekTo(this.playhead() + seconds);
  }

  stepFrames(frames: number): void {
    this.pause();
    this.seekTo(this.playhead() + frames / SEQUENCE_FPS);
  }

  setRate(rate: number): void {
    this.stopReverse();
    this.rate = rate;
    usePlaybackStore.getState().setRate(rate === 0 ? 1 : rate);
    if (rate > 0) {
      if (this.seqEnd() <= 0) return;
      this.startClock();
    } else if (rate === 0) {
      this.stopClock();
    } else {
      // Rückwärts: HTMLMediaElement kann nicht nativ rückwärts spielen —
      // der Shuttle springt alle 250 ms um |rate| * 0,25 s zurück.
      this.stopClock();
      this.playing = true;
      usePlaybackStore.getState().setIsPlaying(true);
      this.reverseTimer = window.setInterval(() => {
        const next = this.playhead() - Math.abs(rate) * (REVERSE_TICK_MS / 1000);
        this.writePlayhead(Math.max(0, next));
        this.syncElements();
        this.requestDraw();
        if (next <= 0) this.pause();
      }, REVERSE_TICK_MS);
    }
  }

  shuttle(direction: 1 | -1): void {
    const next =
      direction === 1
        ? !this.playing || this.rate <= 0
          ? 1
          : Math.min(this.rate * 2, MAX_SHUTTLE_RATE)
        : this.rate >= 0 || !this.playing
          ? -1
          : Math.max(this.rate * 2, -MAX_SHUTTLE_RATE);
    this.setRate(next);
  }

  goToStart(): void {
    this.seekTo(0);
  }

  goToEnd(): void {
    this.seekTo(this.seqEnd());
  }

  markIn(): void {
    useTimelineStore.getState().setInPoint(this.playhead());
  }

  markOut(): void {
    useTimelineStore.getState().setOutPoint(this.playhead());
  }

  clearMarks(): void {
    useTimelineStore.getState().clearInOut();
  }

  // ------------------------------------------------------------- Interna

  private playhead(): number {
    return useTimelineStore.getState().playheadSec;
  }

  private seqEnd(): number {
    return sequenceEnd(useTimelineStore.getState().clips);
  }

  private loopRange(): { in: number; out: number } | null {
    const { inPoint, outPoint } = useTimelineStore.getState();
    if (inPoint === null || outPoint === null) return null;
    if (outPoint <= inPoint + EPS) return null;
    return { in: inPoint, out: outPoint };
  }

  private writePlayhead(t: number): void {
    this.writingPlayhead = true;
    useTimelineStore.getState().setPlayhead(t);
    this.writingPlayhead = false;
  }

  private onExternalSeek(t: number): void {
    if (this.playing && this.reverseTimer === null) {
      // Clock auf die neue Position verankern, Wiedergabe läuft weiter.
      this.clockSeq = t;
      this.clockPerf = performance.now();
      this.lastTickT = t;
    }
    this.syncElements();
    this.requestDraw();
  }

  private startClock(): void {
    this.clockSeq = this.playhead();
    this.clockPerf = performance.now();
    this.lastTickT = this.clockSeq;
    if (!this.playing) {
      this.playing = true;
      usePlaybackStore.getState().setIsPlaying(true);
    }
    cancelAnimationFrame(this.raf);
    this.raf = requestAnimationFrame(this.tick);
    this.syncElements();
  }

  private stopClock(): void {
    cancelAnimationFrame(this.raf);
    if (this.playing) {
      this.playing = false;
      usePlaybackStore.getState().setIsPlaying(false);
    }
    this.syncElements();
    this.requestDraw();
  }

  private stopReverse(): void {
    if (this.reverseTimer !== null) {
      window.clearInterval(this.reverseTimer);
      this.reverseTimer = null;
    }
  }

  private tick = (): void => {
    if (!this.playing || this.reverseTimer !== null) return;

    let t = this.clockSeq + ((performance.now() - this.clockPerf) / 1000) * this.rate;
    const end = this.seqEnd();
    const loop = this.loopRange();

    // Loop: beim Überqueren des Out-Punkts (von links) zurück zum In-Punkt.
    if (loop && this.lastTickT < loop.out - EPS && t >= loop.out - EPS) {
      t = loop.in;
      this.clockSeq = t;
      this.clockPerf = performance.now();
    } else if (t >= end) {
      // Liegt der Out-Punkt hinter dem Sequenzende (z. B. nach dem
      // Löschen von Clips), loopt der Bereich am Sequenzende.
      if (loop && loop.in < end - EPS) {
        t = loop.in;
        this.clockSeq = t;
        this.clockPerf = performance.now();
      } else {
        this.writePlayhead(end);
        this.lastTickT = end;
        this.stopClock();
        return;
      }
    }

    this.lastTickT = t;
    this.writePlayhead(t);
    this.syncElements();
    this.draw();
    this.raf = requestAnimationFrame(this.tick);
  };

  // ------------------------------------------------------- Element-Pool

  /**
   * Bestimmt aktive (und bald aktive) Clips, hält für sie Medienelemente
   * bereit und synchronisiert deren Position/Abspielstatus auf die
   * Sequenzzeit.
   */
  private syncElements(): void {
    const t = this.playhead();
    const { clips, tracks } = useTimelineStore.getState();
    const assets = useMediaStore.getState().assets;

    const videoTracks = tracks.filter((tr) => tr.kind === "video");
    const trackById = new Map(tracks.map((tr) => [tr.id, tr] as const));

    // Sichtbarer Videoclip: oberste aktive Videospur mit Clip am Playhead.
    let visibleClip: TimelineClip | null = null;
    for (const tr of videoTracks) {
      if (!trackActive(tr, tracks)) continue;
      const clip = clips.find(
        (c) => c.trackId === tr.id && c.enabled && clipAt(c, t),
      );
      if (clip) {
        visibleClip = clip;
        break;
      }
    }

    // Benötigte Clips: alle am Playhead aktiven + die im Preload-Fenster.
    const needed = clips.filter((c) => {
      if (!c.enabled) return false;
      const track = trackById.get(c.trackId);
      if (!track || !trackActive(track, tracks)) return false;
      return clipAt(c, t) || (c.start > t && c.start <= t + PRELOAD_AHEAD_SEC);
    });

    const neededIds = new Set(needed.map((c) => c.id));
    let visibleEl: HTMLVideoElement | HTMLImageElement | null = null;

    for (const clip of needed) {
      const asset = assets.find((a) => a.id === clip.assetId);
      if (!asset) continue;
      const entry = this.ensureEntry(clip, asset.kind, asset.path);
      if (!entry) continue;

      const active = clipAt(clip, t);
      if (isMediaEl(entry.el)) {
        const el = entry.el;
        const target = clip.srcIn + Math.max(0, t - clip.start);
        const shouldRun = active && this.playing && this.reverseTimer === null && this.rate > 0;
        const drift = Math.abs(el.currentTime - target);
        if (drift > (shouldRun ? DRIFT_PLAYING : DRIFT_PAUSED)) {
          el.currentTime = target;
        }
        if (shouldRun) {
          const wantedRate = Math.min(this.rate, MAX_SHUTTLE_RATE);
          if (el.playbackRate !== wantedRate) el.playbackRate = wantedRate;
          if (el.paused) {
            el.play().catch(() => {
              /* z. B. unterbrochen durch sofortiges pause() */
            });
          }
        } else if (!el.paused) {
          el.pause();
        }
      }
      if (visibleClip && clip.id === visibleClip.id) {
        visibleEl = entry.el as HTMLVideoElement | HTMLImageElement;
      }
    }

    // Nicht mehr benötigte Elemente pausieren; Pool begrenzen.
    for (const [clipId, entry] of this.pool) {
      if (neededIds.has(clipId)) continue;
      if (isMediaEl(entry.el) && !entry.el.paused) entry.el.pause();
    }
    if (this.pool.size > MAX_POOL) {
      for (const [clipId] of this.pool) {
        if (this.pool.size <= MAX_POOL) break;
        if (!neededIds.has(clipId)) this.disposeEntry(clipId);
      }
    }

    if (visibleEl !== this.visibleEl) {
      this.visibleEl = visibleEl;
      useMonitorStore
        .getState()
        .setVideoEl(visibleEl instanceof HTMLVideoElement ? visibleEl : null);
      this.requestDraw();
    }
  }

  private ensureEntry(
    clip: TimelineClip,
    assetKind: "video" | "audio" | "image",
    path: string,
  ): PoolEntry | null {
    const existing = this.pool.get(clip.id);
    if (existing && existing.assetId === clip.assetId) return existing;
    if (existing) this.disposeEntry(clip.id);

    let el: PoolEntry["el"];
    if (assetKind === "image") {
      const img = new Image();
      img.src = mediaSrc(path);
      img.decode().then(() => this.requestDraw()).catch(() => {});
      el = img;
    } else if (clip.kind === "video") {
      const video = document.createElement("video");
      video.src = mediaSrc(path);
      video.preload = "auto";
      video.muted = true;
      video.playsInline = true;
      // Pausiert gemalte Frames (Scrubbing, Schnittwechsel) und die
      // Seek-Sprünge des Rück-Shuttles nachzeichnen.
      const drawIfIdle = () => {
        if (!this.playing || this.reverseTimer !== null) this.requestDraw();
      };
      video.addEventListener("seeked", drawIfIdle);
      video.addEventListener("loadeddata", drawIfIdle);
      el = video;
    } else {
      // Audio-Clip — auch wenn die Quelle eine Videodatei ist (verknüpftes
      // Paar), genügt ein Audio-Element auf dieselbe Datei.
      const audio = new Audio(mediaSrc(path));
      audio.preload = "auto";
      el = audio;
    }

    const entry: PoolEntry = { clipId: clip.id, assetId: clip.assetId, el };
    this.pool.set(clip.id, entry);
    return entry;
  }

  private disposeEntry(clipId: string): void {
    const entry = this.pool.get(clipId);
    if (!entry) return;
    this.pool.delete(clipId);
    if (isMediaEl(entry.el)) {
      entry.el.pause();
      entry.el.removeAttribute("src");
      entry.el.load();
    }
    if (entry.el === this.visibleEl) {
      this.visibleEl = null;
      useMonitorStore.getState().setVideoEl(null);
    }
  }

  private pruneStaleEntries(clips: TimelineClip[]): void {
    const alive = new Map(clips.map((c) => [c.id, c] as const));
    for (const [clipId, entry] of this.pool) {
      const clip = alive.get(clipId);
      if (!clip || clip.assetId !== entry.assetId) this.disposeEntry(clipId);
    }
  }

  // ------------------------------------------------------------- Zeichnen

  /** Bündelt pausierte Draw-Anlässe auf maximal einen pro Frame. */
  private requestDraw(): void {
    if (this.drawQueued) return;
    this.drawQueued = true;
    requestAnimationFrame(() => {
      this.drawQueued = false;
      this.draw();
    });
  }

  private draw(): void {
    const canvas = this.canvas;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const el = this.visibleEl;
    let srcW = 0;
    let srcH = 0;
    if (el instanceof HTMLVideoElement) {
      srcW = el.videoWidth;
      srcH = el.videoHeight;
    } else if (el instanceof HTMLImageElement) {
      srcW = el.naturalWidth;
      srcH = el.naturalHeight;
    }

    if (!el || srcW === 0 || srcH === 0) {
      if (canvas.width === 0 || canvas.height === 0) {
        canvas.width = 640;
        canvas.height = 360;
      }
      ctx.fillStyle = "#000";
      ctx.fillRect(0, 0, canvas.width, canvas.height);
      return;
    }

    const w = Math.max(2, Math.round(srcW * this.renderScale));
    const h = Math.max(2, Math.round(srcH * this.renderScale));
    if (canvas.width !== w || canvas.height !== h) {
      canvas.width = w;
      canvas.height = h;
    }
    ctx.drawImage(el, 0, 0, w, h);
  }
}

let player: SequencePlayer | null = null;

/** Singleton-Zugriff auf die Engine (erzeugt sie beim ersten Aufruf). */
export function sequencePlayer(): SequencePlayer {
  if (!player) player = new SequencePlayer();
  return player;
}

/**
 * Initialisiert die Engine beim App-Start und registriert sie als
 * Programm-Controller — die Timeline ist damit auch dann abspielbar,
 * wenn das Programmmonitor-Panel (noch) nicht gemountet ist.
 */
export function initSequencePlayer(): void {
  usePlaybackStore.getState().registerController("program", sequencePlayer());
}
