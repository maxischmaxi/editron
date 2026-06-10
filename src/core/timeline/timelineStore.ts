import { create } from "zustand";
import { setContext } from "@/core/commands/context";
import { useMediaStore } from "@/stores/mediaStore";
import {
  IMAGE_DEFAULT_DURATION,
  MIN_CLIP_DURATION,
  SEQUENCE_FPS,
  sequenceEnd,
  type TimelineClip,
  type TimelineTrack,
  type TrackKind,
} from "./types";

const MIN_ZOOM = 4;
const MAX_ZOOM = 1000;
const ZOOM_FACTOR = 1.5;
const HISTORY_LIMIT = 100;
const EPS = 1e-6;

function clampZoom(v: number): number {
  if (!Number.isFinite(v)) return MIN_ZOOM;
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, v));
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

function makeTrack(kind: TrackKind): TimelineTrack {
  return { id: crypto.randomUUID(), kind, muted: false, solo: false, locked: false };
}

interface HistoryEntry {
  tracks: TimelineTrack[];
  clips: TimelineClip[];
}

export type TrimEdge = "start" | "end";

/**
 * Entfernt aus `clips` alles, was auf `trackId` den Bereich [start, end)
 * belegt (Overwrite-Semantik wie beim Premiere-Drop): innenliegende Clips
 * fliegen raus, überlappende werden getrimmt, umspannende geteilt.
 */
function overwriteRange(
  clips: TimelineClip[],
  trackId: string,
  start: number,
  end: number,
  ignore: ReadonlySet<string>,
): TimelineClip[] {
  if (end - start <= EPS) return clips;
  const out: TimelineClip[] = [];
  for (const clip of clips) {
    const clipEnd = clip.start + clip.duration;
    if (
      clip.trackId !== trackId ||
      ignore.has(clip.id) ||
      clipEnd <= start + EPS ||
      clip.start >= end - EPS
    ) {
      out.push(clip);
      continue;
    }
    const leftLen = start - clip.start;
    const rightLen = clipEnd - end;
    const keepLeft = leftLen >= MIN_CLIP_DURATION - EPS;
    if (keepLeft) out.push({ ...clip, duration: leftLen });
    if (rightLen >= MIN_CLIP_DURATION - EPS) {
      out.push({
        ...clip,
        id: keepLeft ? crypto.randomUUID() : clip.id,
        start: end,
        srcIn: clip.srcIn + (end - clip.start),
        duration: rightLen,
      });
    }
  }
  return out;
}

function lockedTrackIds(tracks: TimelineTrack[]): Set<string> {
  return new Set(tracks.filter((t) => t.locked).map((t) => t.id));
}

/** Erweitert eine Clip-Auswahl um alle verknüpften Partner. */
export function expandLinks(
  clips: TimelineClip[],
  ids: readonly string[],
): string[] {
  const idSet = new Set(ids);
  const linkIds = new Set<string>();
  for (const clip of clips) {
    if (idSet.has(clip.id) && clip.linkId !== null) linkIds.add(clip.linkId);
  }
  for (const clip of clips) {
    if (clip.linkId !== null && linkIds.has(clip.linkId)) idSet.add(clip.id);
  }
  return [...idSet];
}

/** Maximal erlaubtes Delta beim Trimmen einer Kante (Quelle + Nachbarn). */
export function trimRange(
  clip: TimelineClip,
  edge: TrimEdge,
  clips: TimelineClip[],
  respectNeighbors: boolean,
): [number, number] {
  let lo: number;
  let hi: number;
  if (edge === "start") {
    // Negativ = Kopf verlängern: nicht vor Quelle/Sequenzanfang.
    lo = Math.max(-clip.srcIn, -clip.start);
    hi = clip.duration - MIN_CLIP_DURATION;
    if (respectNeighbors) {
      let prevEnd = 0;
      for (const c of clips) {
        if (c.trackId !== clip.trackId || c.id === clip.id) continue;
        const cEnd = c.start + c.duration;
        if (cEnd <= clip.start + EPS) prevEnd = Math.max(prevEnd, cEnd);
      }
      lo = Math.max(lo, prevEnd - clip.start);
    }
  } else {
    const clipEnd = clip.start + clip.duration;
    lo = -(clip.duration - MIN_CLIP_DURATION);
    hi = Number.isFinite(clip.srcDuration)
      ? clip.srcDuration - clip.srcIn - clip.duration
      : Number.POSITIVE_INFINITY;
    if (respectNeighbors) {
      let nextStart = Number.POSITIVE_INFINITY;
      for (const c of clips) {
        if (c.trackId !== clip.trackId || c.id === clip.id) continue;
        if (c.start >= clipEnd - EPS) nextStart = Math.min(nextStart, c.start);
      }
      hi = Math.min(hi, nextStart - clipEnd);
    }
  }
  return [lo, hi];
}

function applyTrim(clip: TimelineClip, edge: TrimEdge, delta: number): TimelineClip {
  if (edge === "start") {
    return {
      ...clip,
      start: clip.start + delta,
      srcIn: clip.srcIn + delta,
      duration: clip.duration - delta,
    };
  }
  return { ...clip, duration: clip.duration + delta };
}

interface TimelineState {
  tracks: TimelineTrack[];
  clips: TimelineClip[];
  selectedClipIds: string[];
  clipboard: TimelineClip[];
  playheadSec: number;
  /**
   * In-/Out-Punkt der Sequenz: definiert den Bereich, den der
   * Programmmonitor in Schleife abspielt. Wie Playhead/Auswahl
   * bewusst nicht Teil der Undo-History.
   */
  inPoint: number | null;
  outPoint: number | null;
  snapping: boolean;
  zoomPxPerSec: number;
  /** Sichtbare Breite des Spurbereichs (vom Panel gemeldet, für zoomToFit). */
  viewportW: number;

  past: HistoryEntry[];
  future: HistoryEntry[];

  // Ansicht
  zoomIn: () => void;
  zoomOut: () => void;
  setZoom: (v: number) => void;
  zoomToFit: () => void;
  setViewportWidth: (w: number) => void;
  toggleSnapping: () => void;

  // Playhead
  setPlayhead: (t: number) => void;
  stepPlayheadFrames: (frames: number) => void;
  goToStart: () => void;
  goToEnd: () => void;
  goToPrevEdit: () => void;
  goToNextEdit: () => void;

  // In/Out (Loop-Bereich)
  setInPoint: (t: number | null) => void;
  setOutPoint: (t: number | null) => void;
  setInOutRange: (a: number, b: number) => void;
  clearInOut: () => void;

  // Auswahl
  selectClips: (
    ids: string[],
    mode?: "replace" | "add" | "toggle",
    opts?: { links?: boolean },
  ) => void;
  selectAll: () => void;
  clearSelection: () => void;

  // Spuren
  addTrack: (kind: TrackKind) => string;
  removeTrack: (trackId: string) => void;
  toggleTrackFlag: (trackId: string, flag: "muted" | "solo" | "locked") => void;

  // Bearbeitung
  insertAssets: (
    assetIds: string[],
    opts: { at: number; trackId?: string | null },
  ) => void;
  moveClips: (ids: string[], deltaSec: number, laneOffset?: number) => void;
  trimClip: (id: string, edge: TrimEdge, delta: number) => void;
  rippleTrimClip: (id: string, edge: TrimEdge, delta: number) => void;
  rollEdit: (leftId: string, rightId: string, delta: number) => void;
  slipClip: (id: string, delta: number) => void;
  slideClip: (id: string, delta: number) => void;
  splitAt: (time: number, clipIds?: string[]) => void;
  deleteClips: (ids: string[], opts?: { ripple?: boolean }) => void;
  setClipsEnabled: (ids: string[], enabled: boolean) => void;
  toggleLinkSelected: () => void;
  removeClipsForAssets: (assetIds: string[]) => void;

  // Zwischenablage
  copySelection: () => void;
  cutSelection: () => void;
  paste: (at?: number) => void;

  // Verlauf
  undo: () => void;
  redo: () => void;
}

/**
 * Geplante Platzierung eines Assets beim Einfügen/Droppen.
 * trackId "newVideo"/"newAudio" bedeutet: passende Spur fehlt und
 * wird beim tatsächlichen Einfügen automatisch angelegt.
 */
export interface PlannedPlacement {
  assetId: string;
  kind: TrackKind;
  trackId: string | "newVideo" | "newAudio";
  start: number;
  duration: number;
  name: string;
  srcDuration: number;
  /** true, wenn Video- und Audio-Teil desselben Assets verknüpft werden. */
  linked: boolean;
}

/**
 * Plant, wo Assets bei einem Drop/Einfügen landen: Video auf die
 * Ziel- bzw. unterste Videospur, Audio auf die Ziel- bzw. erste
 * Audiospur, mehrere Assets hintereinander. Wird von der Drop-Vorschau
 * der Timeline und von insertAssets() gleichermaßen benutzt.
 */
export function planAssetPlacements(
  assetIds: string[],
  at: number,
  dropTrackId?: string | null,
): PlannedPlacement[] {
  const { tracks } = useTimelineStore.getState();
  const assets = useMediaStore.getState().assets;
  const dropTrack = dropTrackId
    ? tracks.find((t) => t.id === dropTrackId) ?? null
    : null;
  if (dropTrack?.locked) return [];

  const laneFor = (kind: TrackKind): string => {
    if (dropTrack && dropTrack.kind === kind) return dropTrack.id;
    const ofKind = tracks.filter((t) => t.kind === kind && !t.locked);
    const lane = kind === "video" ? ofKind[ofKind.length - 1] : ofKind[0];
    return lane?.id ?? (kind === "video" ? "newVideo" : "newAudio");
  };

  let cursor = Math.max(0, at);
  const placements: PlannedPlacement[] = [];
  for (const assetId of assetIds) {
    const asset = assets.find((a) => a.id === assetId);
    if (!asset) continue;
    const isImage = asset.kind === "image";
    const duration = isImage
      ? IMAGE_DEFAULT_DURATION
      : Math.max(asset.info.durationSec, MIN_CLIP_DURATION);
    const srcDuration = isImage
      ? Number.POSITIVE_INFINITY
      : asset.info.durationSec;
    const hasVideo = asset.kind !== "audio";
    const hasAudio = !isImage && asset.info.audio.length > 0;
    const linked = hasVideo && hasAudio;
    if (hasVideo) {
      placements.push({
        assetId,
        kind: "video",
        trackId: laneFor("video"),
        start: cursor,
        duration,
        name: asset.name,
        srcDuration,
        linked,
      });
    }
    if (hasAudio) {
      placements.push({
        assetId,
        kind: "audio",
        trackId: laneFor("audio"),
        start: cursor,
        duration,
        name: linked ? `${asset.name} (Audio)` : asset.name,
        srcDuration,
        linked,
      });
    }
    if (hasVideo || hasAudio) cursor += duration;
  }
  return placements;
}

function pushHistory(s: TimelineState): Pick<TimelineState, "past" | "future"> {
  const past = [...s.past, { tracks: s.tracks, clips: s.clips }];
  if (past.length > HISTORY_LIMIT) past.shift();
  return { past, future: [] };
}

/** Auswahl auf noch existierende Clips reduzieren. */
function pruneSelection(ids: string[], clips: TimelineClip[]): string[] {
  const existing = new Set(clips.map((c) => c.id));
  return ids.filter((id) => existing.has(id));
}

export const useTimelineStore = create<TimelineState>((set, get) => ({
  // Startbelegung wie ein frisches Premiere-Projekt: 2 Video-, 2 Audiospuren.
  tracks: [makeTrack("video"), makeTrack("video"), makeTrack("audio"), makeTrack("audio")],
  clips: [],
  selectedClipIds: [],
  clipboard: [],
  playheadSec: 0,
  inPoint: null,
  outPoint: null,
  snapping: true,
  zoomPxPerSec: 40,
  viewportW: 0,
  past: [],
  future: [],

  // ------------------------------------------------------------- Ansicht
  zoomIn: () => set((s) => ({ zoomPxPerSec: clampZoom(s.zoomPxPerSec * ZOOM_FACTOR) })),
  zoomOut: () => set((s) => ({ zoomPxPerSec: clampZoom(s.zoomPxPerSec / ZOOM_FACTOR) })),
  setZoom: (v) => set({ zoomPxPerSec: clampZoom(v) }),
  zoomToFit: () =>
    set((s) => {
      const end = sequenceEnd(s.clips);
      if (end <= 0 || s.viewportW <= 0) return s;
      return { zoomPxPerSec: clampZoom((s.viewportW * 0.97) / end) };
    }),
  setViewportWidth: (w) => set({ viewportW: w }),
  toggleSnapping: () => set((s) => ({ snapping: !s.snapping })),

  // ------------------------------------------------------------ Playhead
  setPlayhead: (t) => set({ playheadSec: Math.max(0, t) }),
  stepPlayheadFrames: (frames) =>
    set((s) => ({ playheadSec: Math.max(0, s.playheadSec + frames / SEQUENCE_FPS) })),
  goToStart: () => set({ playheadSec: 0 }),
  goToEnd: () => set((s) => ({ playheadSec: sequenceEnd(s.clips) })),
  goToPrevEdit: () =>
    set((s) => {
      const edges = editPoints(s.clips);
      const prev = [...edges].reverse().find((e) => e < s.playheadSec - EPS);
      return { playheadSec: prev ?? 0 };
    }),
  goToNextEdit: () =>
    set((s) => {
      const edges = editPoints(s.clips);
      const next = edges.find((e) => e > s.playheadSec + EPS);
      return next === undefined ? s : { playheadSec: next };
    }),

  // ------------------------------------------------- In/Out (Loop-Bereich)
  // Halbgesetzte Zustände (nur In oder nur Out) sind erlaubt; ein Punkt,
  // der den anderen kreuzen würde, löscht ihn (Premiere-Konvention).
  setInPoint: (t) =>
    set((s) => {
      if (t === null) return { inPoint: null };
      const v = Math.max(0, t);
      return {
        inPoint: v,
        outPoint:
          s.outPoint !== null && s.outPoint <= v + MIN_CLIP_DURATION - EPS
            ? null
            : s.outPoint,
      };
    }),
  setOutPoint: (t) =>
    set((s) => {
      if (t === null) return { outPoint: null };
      const v = Math.max(0, t);
      return {
        outPoint: v,
        inPoint:
          s.inPoint !== null && s.inPoint >= v - MIN_CLIP_DURATION + EPS
            ? null
            : s.inPoint,
      };
    }),
  setInOutRange: (a, b) => {
    const lo = Math.max(0, Math.min(a, b));
    const hi = Math.max(0, Math.max(a, b));
    if (hi - lo < MIN_CLIP_DURATION) return;
    set({ inPoint: lo, outPoint: hi });
  },
  clearInOut: () => set({ inPoint: null, outPoint: null }),

  // ------------------------------------------------------------- Auswahl
  selectClips: (ids, mode = "replace", opts) =>
    set((s) => {
      const expanded =
        opts?.links === false ? [...new Set(ids)] : expandLinks(s.clips, ids);
      if (mode === "replace") return { selectedClipIds: expanded };
      const current = new Set(s.selectedClipIds);
      if (mode === "add") {
        expanded.forEach((id) => current.add(id));
      } else {
        const allIn = expanded.every((id) => current.has(id));
        expanded.forEach((id) => (allIn ? current.delete(id) : current.add(id)));
      }
      return { selectedClipIds: [...current] };
    }),
  selectAll: () => set((s) => ({ selectedClipIds: s.clips.map((c) => c.id) })),
  clearSelection: () => set({ selectedClipIds: [] }),

  // -------------------------------------------------------------- Spuren
  addTrack: (kind) => {
    const track = makeTrack(kind);
    set((s) => ({
      ...pushHistory(s),
      // Neue Videospur oben auf den Video-Block, neue Audiospur unten.
      tracks: kind === "video" ? [track, ...s.tracks] : [...s.tracks, track],
    }));
    return track.id;
  },
  removeTrack: (trackId) =>
    set((s) => {
      const track = s.tracks.find((t) => t.id === trackId);
      if (!track) return s;
      const clips = s.clips.filter((c) => c.trackId !== trackId);
      return {
        ...pushHistory(s),
        tracks: s.tracks.filter((t) => t.id !== trackId),
        clips,
        selectedClipIds: pruneSelection(s.selectedClipIds, clips),
      };
    }),
  toggleTrackFlag: (trackId, flag) =>
    set((s) => ({
      tracks: s.tracks.map((t) =>
        t.id === trackId ? { ...t, [flag]: !t[flag] } : t,
      ),
    })),

  // --------------------------------------------------------- Bearbeitung
  insertAssets: (assetIds, opts) => {
    const placements = planAssetPlacements(assetIds, opts.at, opts.trackId);
    if (placements.length === 0) return;
    set((s) => {
      // Fehlende Spuren anlegen (höchstens eine je Art).
      let tracks = s.tracks;
      const created: Partial<Record<"newVideo" | "newAudio", TimelineTrack>> = {};
      for (const p of placements) {
        if (p.trackId !== "newVideo" && p.trackId !== "newAudio") continue;
        if (!created[p.trackId]) {
          const track = makeTrack(p.kind);
          created[p.trackId] = track;
          tracks = p.kind === "video" ? [track, ...tracks] : [...tracks, track];
        }
      }

      let clips = s.clips;
      const inserted: TimelineClip[] = [];
      // Video- und Audio-Teil desselben Assets teilen sich eine linkId;
      // Platzierungen sind nach Asset gruppiert und starten gleich.
      const linkIds = new Map<string, string>();
      for (const p of placements) {
        const trackId =
          p.trackId === "newVideo" || p.trackId === "newAudio"
            ? created[p.trackId]!.id
            : p.trackId;
        let linkId: string | null = null;
        if (p.linked) {
          const key = `${p.assetId}@${p.start}`;
          linkId = linkIds.get(key) ?? crypto.randomUUID();
          linkIds.set(key, linkId);
        }
        clips = overwriteRange(clips, trackId, p.start, p.start + p.duration, new Set());
        inserted.push({
          id: crypto.randomUUID(),
          trackId,
          assetId: p.assetId,
          name: p.name,
          kind: p.kind,
          start: p.start,
          duration: p.duration,
          srcIn: 0,
          srcDuration: p.srcDuration,
          linkId,
          enabled: true,
        });
      }

      return {
        ...pushHistory(s),
        tracks,
        clips: [...clips, ...inserted],
        selectedClipIds: inserted.map((c) => c.id),
      };
    });
  },

  // Bewusst keine Link-Expansion: die Auswahl ist bereits expandiert,
  // und Alt-Drag soll gezielt eine Hälfte eines Paares bewegen können.
  moveClips: (ids, deltaSec, laneOffset = 0) =>
    set((s) => {
      const locked = lockedTrackIds(s.tracks);
      const idSet = new Set(ids);
      const moving = s.clips.filter((c) => idSet.has(c.id) && !locked.has(c.trackId));
      if (moving.length === 0) return s;

      const minStart = Math.min(...moving.map((c) => c.start));
      const d = Math.max(deltaSec, -minStart);
      if (Math.abs(d) < EPS && laneOffset === 0) return s;

      const videoTracks = s.tracks.filter((t) => t.kind === "video");
      const audioTracks = s.tracks.filter((t) => t.kind === "audio");
      const remap = (clip: TimelineClip): string => {
        if (laneOffset === 0) return clip.trackId;
        const lanes = clip.kind === "video" ? videoTracks : audioTracks;
        const idx = lanes.findIndex((t) => t.id === clip.trackId);
        if (idx < 0) return clip.trackId;
        return lanes[clamp(idx + laneOffset, 0, lanes.length - 1)].id;
      };

      const placed = moving.map((c) => ({ ...c, start: c.start + d, trackId: remap(c) }));
      if (placed.some((c) => locked.has(c.trackId))) return s;

      let rest = s.clips.filter((c) => !idSet.has(c.id) || locked.has(c.trackId));
      for (const p of placed) {
        rest = overwriteRange(rest, p.trackId, p.start, p.start + p.duration, new Set());
      }
      return { ...pushHistory(s), clips: [...rest, ...placed] };
    }),

  trimClip: (id, edge, delta) =>
    set((s) => {
      const locked = lockedTrackIds(s.tracks);
      const targets = s.clips.filter(
        (c) => expandLinks(s.clips, [id]).includes(c.id) && !locked.has(c.trackId),
      );
      if (targets.length === 0) return s;
      let d = delta;
      for (const clip of targets) {
        const [lo, hi] = trimRange(clip, edge, s.clips, true);
        d = clamp(d, lo, hi);
      }
      if (Math.abs(d) < EPS) return s;
      const targetIds = new Set(targets.map((c) => c.id));
      return {
        ...pushHistory(s),
        clips: s.clips.map((c) => (targetIds.has(c.id) ? applyTrim(c, edge, d) : c)),
      };
    }),

  rippleTrimClip: (id, edge, delta) =>
    set((s) => {
      const locked = lockedTrackIds(s.tracks);
      const targets = s.clips.filter(
        (c) => expandLinks(s.clips, [id]).includes(c.id) && !locked.has(c.trackId),
      );
      if (targets.length === 0) return s;
      let d = delta;
      for (const clip of targets) {
        const [lo, hi] = trimRange(clip, edge, s.clips, false);
        d = clamp(d, lo, hi);
      }
      if (Math.abs(d) < EPS) return s;

      const targetIds = new Set(targets.map((c) => c.id));
      const anchor = targets[0];
      // Alles hinter dem Schnittpunkt rückt nach, damit keine Lücke entsteht:
      // End-Trim verschiebt um +d, Start-Trim (Kopf kürzen) um -d.
      const boundary =
        edge === "end" ? anchor.start + anchor.duration : anchor.start;
      const shift = edge === "end" ? d : -d;
      const clips = s.clips.map((c) => {
        if (targetIds.has(c.id)) {
          const trimmed = applyTrim(c, edge, d);
          // Beim Start-Trim bleibt die Schnittkante stehen: Clip rückt mit.
          return edge === "start" ? { ...trimmed, start: c.start } : trimmed;
        }
        if (locked.has(c.trackId)) return c;
        if (c.start >= boundary - EPS && !targetIds.has(c.id)) {
          return { ...c, start: Math.max(0, c.start + shift) };
        }
        return c;
      });
      return { ...pushHistory(s), clips };
    }),

  rollEdit: (leftId, rightId, delta) =>
    set((s) => {
      const locked = lockedTrackIds(s.tracks);
      const left = s.clips.find((c) => c.id === leftId);
      const right = s.clips.find((c) => c.id === rightId);
      if (!left || !right || locked.has(left.trackId) || locked.has(right.trackId))
        return s;
      const [loL, hiL] = trimRange(left, "end", s.clips, false);
      const [loR, hiR] = trimRange(right, "start", s.clips, false);
      const d = clamp(delta, Math.max(loL, loR), Math.min(hiL, hiR));
      if (Math.abs(d) < EPS) return s;
      return {
        ...pushHistory(s),
        clips: s.clips.map((c) => {
          if (c.id === leftId) return applyTrim(c, "end", d);
          if (c.id === rightId) return applyTrim(c, "start", d);
          return c;
        }),
      };
    }),

  slideClip: (id, delta) =>
    set((s) => {
      const locked = lockedTrackIds(s.tracks);
      const ids = new Set(expandLinks(s.clips, [id]));
      const sliding = s.clips.filter(
        (c) => ids.has(c.id) && !locked.has(c.trackId),
      );
      if (sliding.length === 0) return s;

      // Direkt angrenzende Nachbarn rollen mit: der linke verlängert/
      // verkürzt sein Ende, der rechte seinen Anfang. Lücken absorbieren
      // die Bewegung einfach.
      const leftNeighbors: TimelineClip[] = [];
      const rightNeighbors: TimelineClip[] = [];
      let d = delta;
      for (const clip of sliding) {
        const clipEnd = clip.start + clip.duration;
        const prev = s.clips.find(
          (c) =>
            c.trackId === clip.trackId &&
            !ids.has(c.id) &&
            Math.abs(c.start + c.duration - clip.start) < EPS,
        );
        const next = s.clips.find(
          (c) =>
            c.trackId === clip.trackId &&
            !ids.has(c.id) &&
            Math.abs(c.start - clipEnd) < EPS,
        );
        if (prev) {
          const [lo, hi] = trimRange(prev, "end", s.clips, false);
          d = clamp(d, lo, hi);
          leftNeighbors.push(prev);
        }
        if (next) {
          const [lo, hi] = trimRange(next, "start", s.clips, false);
          d = clamp(d, lo, hi);
          rightNeighbors.push(next);
        }
        d = Math.max(d, -clip.start);
      }
      if (Math.abs(d) < EPS) return s;

      const leftIds = new Set(leftNeighbors.map((c) => c.id));
      const rightIds = new Set(rightNeighbors.map((c) => c.id));
      return {
        ...pushHistory(s),
        clips: s.clips.map((c) => {
          if (ids.has(c.id) && !locked.has(c.trackId)) {
            return { ...c, start: c.start + d };
          }
          if (leftIds.has(c.id)) return applyTrim(c, "end", d);
          if (rightIds.has(c.id)) return applyTrim(c, "start", d);
          return c;
        }),
      };
    }),

  slipClip: (id, delta) =>
    set((s) => {
      const locked = lockedTrackIds(s.tracks);
      const targets = s.clips.filter(
        (c) =>
          expandLinks(s.clips, [id]).includes(c.id) &&
          !locked.has(c.trackId) &&
          Number.isFinite(c.srcDuration),
      );
      if (targets.length === 0) return s;
      let d = delta;
      for (const clip of targets) {
        d = clamp(d, -clip.srcIn, clip.srcDuration - clip.srcIn - clip.duration);
      }
      if (Math.abs(d) < EPS) return s;
      const targetIds = new Set(targets.map((c) => c.id));
      return {
        ...pushHistory(s),
        clips: s.clips.map((c) =>
          targetIds.has(c.id) ? { ...c, srcIn: c.srcIn + d } : c,
        ),
      };
    }),

  splitAt: (time, clipIds) =>
    set((s) => {
      const locked = lockedTrackIds(s.tracks);
      const candidates =
        clipIds && clipIds.length > 0
          ? new Set(expandLinks(s.clips, clipIds))
          : null;
      const splittable = (c: TimelineClip) =>
        !locked.has(c.trackId) &&
        (candidates === null || candidates.has(c.id)) &&
        time > c.start + MIN_CLIP_DURATION - EPS &&
        time < c.start + c.duration - MIN_CLIP_DURATION + EPS;

      if (!s.clips.some(splittable)) return s;

      // Rechte Hälften verknüpfter Clips bekommen eine gemeinsame neue linkId.
      const newLinkIds = new Map<string, string>();
      const rightLink = (linkId: string | null): string | null => {
        if (linkId === null) return null;
        let mapped = newLinkIds.get(linkId);
        if (!mapped) {
          mapped = crypto.randomUUID();
          newLinkIds.set(linkId, mapped);
        }
        return mapped;
      };

      const clips: TimelineClip[] = [];
      const newSelection: string[] = [];
      for (const c of s.clips) {
        if (!splittable(c)) {
          clips.push(c);
          continue;
        }
        const leftLen = time - c.start;
        const right: TimelineClip = {
          ...c,
          id: crypto.randomUUID(),
          start: time,
          srcIn: c.srcIn + leftLen,
          duration: c.duration - leftLen,
          linkId: rightLink(c.linkId),
        };
        clips.push({ ...c, duration: leftLen }, right);
        if (s.selectedClipIds.includes(c.id)) newSelection.push(c.id, right.id);
      }
      return {
        ...pushHistory(s),
        clips,
        selectedClipIds:
          newSelection.length > 0
            ? newSelection
            : pruneSelection(s.selectedClipIds, clips),
      };
    }),

  // Keine Link-Expansion — die Auswahl ist bereits expandiert (s. moveClips).
  deleteClips: (ids, opts) =>
    set((s) => {
      const locked = lockedTrackIds(s.tracks);
      const idSet = new Set(
        ids.filter((id) => {
          const clip = s.clips.find((c) => c.id === id);
          return clip !== undefined && !locked.has(clip.trackId);
        }),
      );
      if (idSet.size === 0) return s;

      const removed = s.clips.filter((c) => idSet.has(c.id));
      let clips = s.clips.filter((c) => !idSet.has(c.id));

      if (opts?.ripple) {
        // Lücke der gesamten Auswahl schließen (über alle ungesperrten Spuren).
        const gapStart = Math.min(...removed.map((c) => c.start));
        const gapEnd = Math.max(...removed.map((c) => c.start + c.duration));
        const gap = gapEnd - gapStart;
        clips = clips.map((c) =>
          !locked.has(c.trackId) && c.start >= gapEnd - EPS
            ? { ...c, start: Math.max(0, c.start - gap) }
            : c,
        );
      }
      return {
        ...pushHistory(s),
        clips,
        selectedClipIds: pruneSelection(s.selectedClipIds, clips),
      };
    }),

  setClipsEnabled: (ids, enabled) =>
    set((s) => {
      const idSet = new Set(expandLinks(s.clips, ids));
      return {
        ...pushHistory(s),
        clips: s.clips.map((c) => (idSet.has(c.id) ? { ...c, enabled } : c)),
      };
    }),

  toggleLinkSelected: () =>
    set((s) => {
      const selected = s.clips.filter((c) => s.selectedClipIds.includes(c.id));
      if (selected.length === 0) return s;
      const anyLinked = selected.some((c) => c.linkId !== null);
      const idSet = new Set(selected.map((c) => c.id));
      if (anyLinked) {
        return {
          ...pushHistory(s),
          clips: s.clips.map((c) =>
            idSet.has(c.id) ? { ...c, linkId: null } : c,
          ),
        };
      }
      // Neu verknüpfen: genau dann sinnvoll, wenn Video- und Audio-Clips
      // gemeinsam ausgewählt sind — alle Ausgewählten in eine Gruppe.
      const hasVideo = selected.some((c) => c.kind === "video");
      const hasAudio = selected.some((c) => c.kind === "audio");
      if (!hasVideo || !hasAudio) return s;
      const linkId = crypto.randomUUID();
      return {
        ...pushHistory(s),
        clips: s.clips.map((c) => (idSet.has(c.id) ? { ...c, linkId } : c)),
      };
    }),

  removeClipsForAssets: (assetIds) =>
    set((s) => {
      const assetSet = new Set(assetIds);
      if (!s.clips.some((c) => assetSet.has(c.assetId))) return s;
      const clips = s.clips.filter((c) => !assetSet.has(c.assetId));
      return {
        ...pushHistory(s),
        clips,
        selectedClipIds: pruneSelection(s.selectedClipIds, clips),
      };
    }),

  // ------------------------------------------------------ Zwischenablage
  copySelection: () =>
    set((s) => {
      const selected = s.clips.filter((c) => s.selectedClipIds.includes(c.id));
      if (selected.length === 0) return s;
      const base = Math.min(...selected.map((c) => c.start));
      return { clipboard: selected.map((c) => ({ ...c, start: c.start - base })) };
    }),

  cutSelection: () => {
    get().copySelection();
    get().deleteClips(get().selectedClipIds);
  },

  paste: (at) =>
    set((s) => {
      if (s.clipboard.length === 0) return s;
      const t = Math.max(0, at ?? s.playheadSec);
      const locked = lockedTrackIds(s.tracks);
      const fallback = (kind: TrackKind): string | null => {
        const lanes = s.tracks.filter((tr) => tr.kind === kind && !tr.locked);
        const lane = kind === "video" ? lanes[lanes.length - 1] : lanes[0];
        return lane?.id ?? null;
      };
      const newLinkIds = new Map<string, string>();
      const pasted: TimelineClip[] = [];
      let clips = s.clips;
      for (const c of s.clipboard) {
        const trackId =
          s.tracks.some((tr) => tr.id === c.trackId && !tr.locked)
            ? c.trackId
            : fallback(c.kind);
        if (trackId === null || locked.has(trackId)) continue;
        let linkId: string | null = null;
        if (c.linkId !== null) {
          linkId = newLinkIds.get(c.linkId) ?? crypto.randomUUID();
          newLinkIds.set(c.linkId, linkId);
        }
        const start = t + c.start;
        clips = overwriteRange(clips, trackId, start, start + c.duration, new Set());
        pasted.push({ ...c, id: crypto.randomUUID(), trackId, start, linkId });
      }
      if (pasted.length === 0) return s;
      return {
        ...pushHistory(s),
        clips: [...clips, ...pasted],
        selectedClipIds: pasted.map((c) => c.id),
      };
    }),

  // ------------------------------------------------------------- Verlauf
  undo: () =>
    set((s) => {
      const prev = s.past[s.past.length - 1];
      if (!prev) return s;
      return {
        past: s.past.slice(0, -1),
        future: [{ tracks: s.tracks, clips: s.clips }, ...s.future],
        tracks: prev.tracks,
        clips: prev.clips,
        selectedClipIds: pruneSelection(s.selectedClipIds, prev.clips),
      };
    }),
  redo: () =>
    set((s) => {
      const next = s.future[0];
      if (!next) return s;
      return {
        past: [...s.past, { tracks: s.tracks, clips: s.clips }],
        future: s.future.slice(1),
        tracks: next.tracks,
        clips: next.clips,
        selectedClipIds: pruneSelection(s.selectedClipIds, next.clips),
      };
    }),
}));

/** Sortierte, eindeutige Schnittpunkte (Clipgrenzen) der Sequenz. */
function editPoints(clips: TimelineClip[]): number[] {
  const set = new Set<number>([0]);
  for (const c of clips) {
    set.add(Number(c.start.toFixed(6)));
    set.add(Number((c.start + c.duration).toFixed(6)));
  }
  return [...set].sort((a, b) => a - b);
}

// Kontextwerte für when-Klauseln synchron zum Store halten.
setContext("timelineClipSelected", false);
setContext("timelineHasClips", false);
setContext("timelineClipboard", false);
setContext("timelineCanUndo", false);
setContext("timelineCanRedo", false);
setContext("timelineInOutSet", false);
useTimelineStore.subscribe((s, prev) => {
  if (s.selectedClipIds !== prev.selectedClipIds) {
    setContext("timelineClipSelected", s.selectedClipIds.length > 0);
  }
  if (s.clips !== prev.clips) {
    setContext("timelineHasClips", s.clips.length > 0);
  }
  if (s.clipboard !== prev.clipboard) {
    setContext("timelineClipboard", s.clipboard.length > 0);
  }
  if (s.past !== prev.past) setContext("timelineCanUndo", s.past.length > 0);
  if (s.future !== prev.future) setContext("timelineCanRedo", s.future.length > 0);
  if (s.inPoint !== prev.inPoint || s.outPoint !== prev.outPoint) {
    setContext("timelineInOutSet", s.inPoint !== null || s.outPoint !== null);
  }
});
