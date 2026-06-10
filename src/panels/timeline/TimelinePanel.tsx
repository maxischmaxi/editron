import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ComponentType,
  type DragEvent,
  type MouseEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import {
  ArrowLeftRight,
  ArrowRightFromLine,
  ArrowRightToLine,
  ChevronsLeftRight,
  ClipboardPaste,
  Copy,
  Eye,
  Film,
  FolderOpen,
  Hand,
  Link2,
  Magnet,
  Maximize2,
  MousePointer,
  MoveHorizontal,
  Music,
  Plus,
  Scissors,
  StretchHorizontal,
  Trash2,
  X,
  ZoomIn,
  ZoomOut,
} from "lucide-react";
import { commandRegistry } from "@/core/commands/registry";
import { TOOL_LABELS } from "@/core/tools";
import {
  ASSET_DRAG_MIME,
  clearDraggedAssets,
  getDraggedAssets,
} from "@/core/dnd";
import {
  expandLinks,
  planAssetPlacements,
  trimRange,
  useTimelineStore,
  type PlannedPlacement,
  type TrimEdge,
} from "@/core/timeline/timelineStore";
import {
  MIN_CLIP_DURATION,
  SEQUENCE_FPS,
  sequenceEnd,
  trackName,
  type TimelineClip,
  type TimelineTrack,
} from "@/core/timeline/types";
import {
  commandItem,
  openContextMenu,
  type MenuEntry,
} from "@/components/ui/ContextMenu";
import { openPanel } from "@/core/workspace/layoutManager";
import { formatDuration, formatTimecode } from "@/lib/timecode";
import { useAppStore, type TimelineTool } from "@/stores/appStore";
import { useMediaStore } from "@/stores/mediaStore";
import { ClipView } from "./ClipView";
import { TrackHeaderCell } from "./TrackHeaderCell";

/** Breite der Track-Header-Spalte (w-24 = 6rem). */
const TRACK_HEADER_W = 96;
const RULER_H = 28;
const VIDEO_H = 48;
const AUDIO_H = 40;
/** Einrast-Radius in Pixeln (zoomunabhängig). */
const SNAP_PX = 8;
/** Kantenzone der Clips für Trim-Erkennung in Pixeln. */
const EDGE_PX = 7;
const MAJOR_TICK_MIN_PX = 80;
const TICK_STEPS = [1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 1800, 3600];
const MINOR_PER_MAJOR = 5;
const EPS = 1e-6;

/** Tooltips kommen aus der zentralen Label-Quelle TOOL_LABELS (core/tools.ts). */
const TOOLS: {
  id: TimelineTool;
  icon: ComponentType<{ className?: string }>;
}[] = [
  { id: "select", icon: MousePointer },
  { id: "razor", icon: Scissors },
  { id: "ripple", icon: ChevronsLeftRight },
  { id: "rolling", icon: ArrowLeftRight },
  { id: "slip", icon: MoveHorizontal },
  { id: "slide", icon: StretchHorizontal },
  { id: "hand", icon: Hand },
  { id: "zoom", icon: ZoomIn },
];

type DragState =
  | {
      type: "move";
      clipIds: string[];
      grabId: string;
      originX: number;
      deltaSec: number;
      laneOffset: number;
      snapTime: number | null;
    }
  | {
      type: "trim";
      clipId: string;
      edge: TrimEdge;
      ripple: boolean;
      originX: number;
      deltaSec: number;
      snapTime: number | null;
    }
  | { type: "roll"; leftId: string; rightId: string; originX: number; deltaSec: number }
  | { type: "slip"; clipId: string; originX: number; deltaSec: number }
  | { type: "slide"; clipId: string; originX: number; deltaSec: number }
  | {
      type: "marquee";
      originT: number;
      originY: number;
      t: number;
      y: number;
      additive: boolean;
      base: string[];
      moved: boolean;
    }
  | { type: "pan"; originX: number; originY: number; startLeft: number; startTop: number };

interface DropPreview {
  placements: PlannedPlacement[];
  snapTime: number | null;
}

function trackHeight(track: TimelineTrack): number {
  return track.kind === "video" ? VIDEO_H : AUDIO_H;
}

function applyTrimPreview(
  clip: TimelineClip,
  edge: TrimEdge,
  delta: number,
): TimelineClip {
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

/**
 * Die Timeline: Werkzeugleiste, Lineal, editierbare Spuren mit Clips,
 * Playhead, Snapping, Drag&Drop aus dem Medien-Browser und Kontextmenüs.
 * Alle Editier-Operationen laufen über den timelineStore (mit Undo).
 */
export function TimelinePanel() {
  const zoom = useTimelineStore((s) => s.zoomPxPerSec);
  const tracks = useTimelineStore((s) => s.tracks);
  const clips = useTimelineStore((s) => s.clips);
  const selectedClipIds = useTimelineStore((s) => s.selectedClipIds);
  const snapping = useTimelineStore((s) => s.snapping);
  const inPoint = useTimelineStore((s) => s.inPoint);
  const outPoint = useTimelineStore((s) => s.outPoint);
  const activeTool = useAppStore((s) => s.activeTool);
  const assets = useMediaStore((s) => s.assets);

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const playheadRef = useRef<HTMLDivElement | null>(null);
  const timecodeRef = useRef<HTMLSpanElement | null>(null);
  const snapTargetsRef = useRef<number[]>([]);
  const zoomAnchorRef = useRef<{ time: number; clientX: number } | null>(null);
  const prevZoomRef = useRef(zoom);
  /** Aktive Lineal-Geste: Scrubbing, Loop-Bereich aufziehen oder Kante ziehen. */
  const rulerDragRef = useRef<
    | { mode: "scrub" }
    | { mode: "range"; originT: number }
    | { mode: "edge"; edge: "in" | "out" }
    | null
  >(null);

  const [scrollLeft, setScrollLeft] = useState(0);
  const [viewportW, setViewportW] = useState(0);
  const [drag, setDrag] = useState<DragState | null>(null);
  const [dropPreview, setDropPreview] = useState<DropPreview | null>(null);

  const dragRef = useRef<DragState | null>(null);
  dragRef.current = drag;

  const seqEnd = sequenceEnd(clips);
  const contentDur = Math.max(seqEnd + 60, 120);
  const contentWidth = Math.ceil(contentDur * zoom);
  const lanesHeight = tracks.reduce((sum, t) => sum + trackHeight(t), 0);

  const assetById = useMemo(() => {
    const map = new Map(assets.map((a) => [a.id, a] as const));
    return map;
  }, [assets]);

  // ----------------------------------------------------------- Geometrie
  const pointerTime = useCallback((clientX: number): number => {
    const el = scrollRef.current;
    if (!el) return 0;
    const rect = el.getBoundingClientRect();
    const z = useTimelineStore.getState().zoomPxPerSec;
    return (clientX - rect.left - TRACK_HEADER_W + el.scrollLeft) / z;
  }, []);

  /** Y-Position relativ zum Spurbereich (unterhalb des Lineals). */
  const laneY = useCallback((clientY: number): number => {
    const el = scrollRef.current;
    if (!el) return 0;
    const rect = el.getBoundingClientRect();
    return clientY - rect.top + el.scrollTop - RULER_H;
  }, []);

  const laneAtY = useCallback(
    (clientY: number): TimelineTrack | null => {
      let y = laneY(clientY);
      if (y < 0) return null;
      for (const t of useTimelineStore.getState().tracks) {
        const h = trackHeight(t);
        if (y < h) return t;
        y -= h;
      }
      return null;
    },
    [laneY],
  );

  // ------------------------------------------------------------ Snapping
  const collectSnapTargets = useCallback((excludeIds: ReadonlySet<string>) => {
    const s = useTimelineStore.getState();
    const targets = [0, s.playheadSec];
    for (const c of s.clips) {
      if (excludeIds.has(c.id)) continue;
      targets.push(c.start, c.start + c.duration);
    }
    snapTargetsRef.current = targets;
  }, []);

  /** Verschiebt delta so, dass eine der Kanten auf ein Snap-Ziel fällt. */
  const snapAdjust = useCallback(
    (edges: number[], delta: number): { delta: number; snapTime: number | null } => {
      const s = useTimelineStore.getState();
      if (!s.snapping) return { delta, snapTime: null };
      const threshold = SNAP_PX / s.zoomPxPerSec;
      let best: { dist: number; delta: number; time: number } | null = null;
      for (const edge of edges) {
        const moved = edge + delta;
        for (const t of snapTargetsRef.current) {
          const dist = Math.abs(moved - t);
          if (dist <= threshold && (best === null || dist < best.dist)) {
            best = { dist, delta: delta + (t - moved), time: t };
          }
        }
      }
      return best ? { delta: best.delta, snapTime: best.time } : { delta, snapTime: null };
    },
    [],
  );

  // ---------------------------------------------------- Playhead-Anzeige
  useEffect(() => {
    const apply = (t: number) => {
      if (playheadRef.current) {
        playheadRef.current.style.transform = `translateX(${
          TRACK_HEADER_W + t * zoom
        }px)`;
      }
      if (timecodeRef.current) {
        timecodeRef.current.textContent = formatTimecode(t, SEQUENCE_FPS);
      }
    };
    apply(useTimelineStore.getState().playheadSec);
    return useTimelineStore.subscribe((s, prev) => {
      if (s.playheadSec !== prev.playheadSec) apply(s.playheadSec);
    });
  }, [zoom]);

  // ------------------------------------------------- Viewport beobachten
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const report = () => {
      setViewportW(el.clientWidth);
      useTimelineStore
        .getState()
        .setViewportWidth(Math.max(0, el.clientWidth - TRACK_HEADER_W));
    };
    const ro = new ResizeObserver(report);
    ro.observe(el);
    report();
    return () => ro.disconnect();
  }, []);

  // ------------------------------------- Mausrad: Zoomen + Scrollen
  // Shift/Mod+Rad zoomt um den Cursor, Alt+Rad scrollt horizontal,
  // ohne Modifier scrollt der Container nativ.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      const s = useTimelineStore.getState();
      if (e.shiftKey || e.ctrlKey || e.metaKey) {
        e.preventDefault();
        // Mit Shift liefern manche Plattformen das Delta auf der X-Achse.
        const raw = e.deltaY !== 0 ? e.deltaY : e.deltaX;
        if (raw === 0) return;
        const rect = el.getBoundingClientRect();
        const time =
          (e.clientX - rect.left - TRACK_HEADER_W + el.scrollLeft) /
          s.zoomPxPerSec;
        zoomAnchorRef.current = { time, clientX: e.clientX };
        s.setZoom(s.zoomPxPerSec * Math.exp(-raw * 0.0018));
      } else if (e.altKey) {
        e.preventDefault();
        el.scrollLeft += e.deltaY !== 0 ? e.deltaY : e.deltaX;
      }
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);

  // Beim Zoomen den Anker (Cursor bzw. Sichtmitte) stabil halten.
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el || prevZoomRef.current === zoom) return;
    const prevZoom = prevZoomRef.current;
    prevZoomRef.current = zoom;
    const anchor = zoomAnchorRef.current;
    zoomAnchorRef.current = null;
    if (anchor) {
      const rect = el.getBoundingClientRect();
      el.scrollLeft = Math.max(
        0,
        anchor.time * zoom - (anchor.clientX - rect.left - TRACK_HEADER_W),
      );
    } else {
      const laneViewW = el.clientWidth - TRACK_HEADER_W;
      const centerTime = (el.scrollLeft + laneViewW / 2) / prevZoom;
      el.scrollLeft = Math.max(0, centerTime * zoom - laneViewW / 2);
    }
  }, [zoom]);

  // ----------------------------------------------------- Clip-Interaktion
  const onClipPointerDown = (
    e: ReactPointerEvent<HTMLDivElement>,
    clip: TimelineClip,
  ) => {
    // Mittelmaus-Pan, Hand und Zoom übernimmt der Spurbereich (bubbling).
    if (e.button === 1 || activeTool === "hand" || activeTool === "zoom") return;
    if (e.button !== 0) return;
    e.stopPropagation();

    const store = useTimelineStore.getState();
    const locked =
      store.tracks.find((t) => t.id === clip.trackId)?.locked ?? false;

    if (activeTool === "razor") {
      if (!locked) store.splitAt(pointerTime(e.clientX), [clip.id]);
      return;
    }

    // Auswahl: Shift/Mod toggelt, Alt wählt ohne verknüpften Partner.
    if (e.shiftKey || e.metaKey || e.ctrlKey) {
      store.selectClips([clip.id], "toggle");
      return;
    }
    if (e.altKey) {
      store.selectClips([clip.id], "replace", { links: false });
    } else if (!store.selectedClipIds.includes(clip.id)) {
      store.selectClips([clip.id], "replace");
    }
    if (locked) return;

    const rect = e.currentTarget.getBoundingClientRect();
    const offsetX = e.clientX - rect.left;
    const edge: TrimEdge | null =
      rect.width > EDGE_PX * 3
        ? offsetX <= EDGE_PX
          ? "start"
          : offsetX >= rect.width - EDGE_PX
            ? "end"
            : null
        : null;

    if ((activeTool === "select" || activeTool === "ripple") && edge) {
      collectSnapTargets(new Set(expandLinks(store.clips, [clip.id])));
      setDrag({
        type: "trim",
        clipId: clip.id,
        edge,
        ripple: activeTool === "ripple",
        originX: e.clientX,
        deltaSec: 0,
        snapTime: null,
      });
      return;
    }

    if (activeTool === "rolling" && edge) {
      const clipEnd = clip.start + clip.duration;
      const neighbor =
        edge === "end"
          ? store.clips.find(
              (c) =>
                c.trackId === clip.trackId &&
                Math.abs(c.start - clipEnd) < EPS,
            )
          : store.clips.find(
              (c) =>
                c.trackId === clip.trackId &&
                Math.abs(c.start + c.duration - clip.start) < EPS,
            );
      if (neighbor) {
        setDrag({
          type: "roll",
          leftId: edge === "end" ? clip.id : neighbor.id,
          rightId: edge === "end" ? neighbor.id : clip.id,
          originX: e.clientX,
          deltaSec: 0,
        });
      }
      return;
    }

    if (activeTool === "slip") {
      setDrag({ type: "slip", clipId: clip.id, originX: e.clientX, deltaSec: 0 });
      return;
    }
    if (activeTool === "slide") {
      setDrag({ type: "slide", clipId: clip.id, originX: e.clientX, deltaSec: 0 });
      return;
    }

    if (activeTool === "select") {
      // Alt-Drag bewegt gezielt eine Hälfte eines verknüpften Paares;
      // sonst gilt die (bereits link-expandierte) aktuelle Auswahl.
      const ids = e.altKey
        ? [clip.id]
        : expandLinks(
            store.clips,
            useTimelineStore.getState().selectedClipIds,
          );
      collectSnapTargets(new Set(ids));
      setDrag({
        type: "move",
        clipIds: ids,
        grabId: clip.id,
        originX: e.clientX,
        deltaSec: 0,
        laneOffset: 0,
        snapTime: null,
      });
    }
  };

  const updateClipDrag = useCallback((e: { clientX: number; clientY: number }) => {
    const d = dragRef.current;
    if (!d) return;
    const s = useTimelineStore.getState();
    const z = s.zoomPxPerSec;

    if (d.type === "move") {
      const moving = s.clips.filter((c) => d.clipIds.includes(c.id));
      if (moving.length === 0) return;
      const grab = s.clips.find((c) => c.id === d.grabId);

      // Spur-Versatz innerhalb derselben Spurart, geklemmt auf die Grenzen
      // aller bewegten Clips.
      let laneOffset = d.laneOffset;
      const lane = laneAtY(e.clientY);
      if (grab && lane && lane.kind === grab.kind) {
        const lanes = s.tracks.filter((t) => t.kind === grab.kind);
        laneOffset =
          lanes.findIndex((t) => t.id === lane.id) -
          lanes.findIndex((t) => t.id === grab.trackId);
      }
      let lo = Number.NEGATIVE_INFINITY;
      let hi = Number.POSITIVE_INFINITY;
      const vLanes = s.tracks.filter((t) => t.kind === "video");
      const aLanes = s.tracks.filter((t) => t.kind === "audio");
      for (const c of moving) {
        const lanes = c.kind === "video" ? vLanes : aLanes;
        const idx = lanes.findIndex((t) => t.id === c.trackId);
        if (idx < 0) continue;
        lo = Math.max(lo, -idx);
        hi = Math.min(hi, lanes.length - 1 - idx);
      }
      if (!Number.isFinite(lo) || lo > hi) laneOffset = 0;
      else laneOffset = Math.min(Math.max(laneOffset, lo), hi);

      const minStart = Math.min(...moving.map((c) => c.start));
      const raw = Math.max((e.clientX - d.originX) / z, -minStart);
      const edges = moving.flatMap((c) => [c.start, c.start + c.duration]);
      const snapped = snapAdjust(edges, raw);
      const delta = Math.max(snapped.delta, -minStart);
      setDrag({ ...d, deltaSec: delta, laneOffset, snapTime: snapped.snapTime });
      return;
    }

    if (d.type === "trim") {
      const targets = s.clips.filter((c) =>
        expandLinks(s.clips, [d.clipId]).includes(c.id),
      );
      if (targets.length === 0) return;
      let delta = (e.clientX - d.originX) / z;
      for (const c of targets) {
        const [lo, hi] = trimRange(c, d.edge, s.clips, !d.ripple);
        delta = Math.min(Math.max(delta, lo), hi);
      }
      const anchor = targets.find((c) => c.id === d.clipId) ?? targets[0];
      const edgeTime =
        d.edge === "start" ? anchor.start : anchor.start + anchor.duration;
      const snapped = snapAdjust([edgeTime], delta);
      for (const c of targets) {
        const [lo, hi] = trimRange(c, d.edge, s.clips, !d.ripple);
        snapped.delta = Math.min(Math.max(snapped.delta, lo), hi);
      }
      setDrag({ ...d, deltaSec: snapped.delta, snapTime: snapped.snapTime });
      return;
    }

    if (d.type === "roll") {
      const left = s.clips.find((c) => c.id === d.leftId);
      const right = s.clips.find((c) => c.id === d.rightId);
      if (!left || !right) return;
      const [loL, hiL] = trimRange(left, "end", s.clips, false);
      const [loR, hiR] = trimRange(right, "start", s.clips, false);
      const delta = Math.min(
        Math.max((e.clientX - d.originX) / z, Math.max(loL, loR)),
        Math.min(hiL, hiR),
      );
      setDrag({ ...d, deltaSec: delta });
      return;
    }

    if (d.type === "slip") {
      const targets = s.clips.filter(
        (c) =>
          expandLinks(s.clips, [d.clipId]).includes(c.id) &&
          Number.isFinite(c.srcDuration),
      );
      let delta = (e.clientX - d.originX) / z;
      for (const c of targets) {
        delta = Math.min(
          Math.max(delta, -c.srcIn),
          c.srcDuration - c.srcIn - c.duration,
        );
      }
      setDrag({ ...d, deltaSec: targets.length > 0 ? delta : 0 });
      return;
    }

    if (d.type === "slide") {
      const targets = s.clips.filter((c) =>
        expandLinks(s.clips, [d.clipId]).includes(c.id),
      );
      if (targets.length === 0) return;
      const minStart = Math.min(...targets.map((c) => c.start));
      const delta = Math.max((e.clientX - d.originX) / z, -minStart);
      setDrag({ ...d, deltaSec: delta });
    }
  }, [laneAtY, snapAdjust]);

  const finishClipDrag = useCallback(() => {
    const d = dragRef.current;
    if (!d) return;
    const s = useTimelineStore.getState();
    switch (d.type) {
      case "move":
        if (Math.abs(d.deltaSec) > EPS || d.laneOffset !== 0) {
          s.moveClips(d.clipIds, d.deltaSec, d.laneOffset);
        }
        break;
      case "trim":
        if (Math.abs(d.deltaSec) > EPS) {
          if (d.ripple) s.rippleTrimClip(d.clipId, d.edge, d.deltaSec);
          else s.trimClip(d.clipId, d.edge, d.deltaSec);
        }
        break;
      case "roll":
        if (Math.abs(d.deltaSec) > EPS) s.rollEdit(d.leftId, d.rightId, d.deltaSec);
        break;
      case "slip":
        if (Math.abs(d.deltaSec) > EPS) s.slipClip(d.clipId, d.deltaSec);
        break;
      case "slide":
        if (Math.abs(d.deltaSec) > EPS) s.slideClip(d.clipId, d.deltaSec);
        break;
      default:
        return;
    }
    setDrag(null);
  }, []);

  // Clip-Drags laufen über window-Listener: beim Spurwechsel wird der
  // Clip in eine andere Zeile re-mounted, Pointer-Capture auf dem alten
  // Element würde den Drag abreißen lassen.
  const clipDragActive =
    drag !== null && drag.type !== "marquee" && drag.type !== "pan";
  useEffect(() => {
    if (!clipDragActive) return;
    const onMove = (e: PointerEvent) => updateClipDrag(e);
    const onUp = () => finishClipDrag();
    const onCancel = () => setDrag(null);
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onCancel);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onCancel);
    };
  }, [clipDragActive, updateClipDrag, finishClipDrag]);

  // ------------------------------------------------ Spurbereich (leer)
  const onLanePointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    const store = useTimelineStore.getState();

    if (e.button === 1 || (e.button === 0 && activeTool === "hand")) {
      const el = scrollRef.current;
      if (!el) return;
      e.preventDefault();
      e.currentTarget.setPointerCapture(e.pointerId);
      setDrag({
        type: "pan",
        originX: e.clientX,
        originY: e.clientY,
        startLeft: el.scrollLeft,
        startTop: el.scrollTop,
      });
      return;
    }
    if (e.button !== 0) return;

    if (activeTool === "zoom") {
      const time = pointerTime(e.clientX);
      zoomAnchorRef.current = { time, clientX: e.clientX };
      if (e.altKey) store.zoomOut();
      else store.zoomIn();
      return;
    }

    if (activeTool === "select") {
      e.currentTarget.setPointerCapture(e.pointerId);
      const additive = e.shiftKey || e.metaKey || e.ctrlKey;
      setDrag({
        type: "marquee",
        originT: pointerTime(e.clientX),
        originY: Math.max(0, laneY(e.clientY)),
        t: pointerTime(e.clientX),
        y: Math.max(0, laneY(e.clientY)),
        additive,
        base: additive ? store.selectedClipIds : [],
        moved: false,
      });
    }
  };

  const applyMarquee = useCallback((d: Extract<DragState, { type: "marquee" }>) => {
    const s = useTimelineStore.getState();
    const t0 = Math.min(d.originT, d.t);
    const t1 = Math.max(d.originT, d.t);
    const y0 = Math.min(d.originY, d.y);
    const y1 = Math.max(d.originY, d.y);
    const hits: string[] = [];
    let top = 0;
    for (const tr of s.tracks) {
      const h = trackHeight(tr);
      if (y0 < top + h && y1 > top) {
        for (const c of s.clips) {
          if (
            c.trackId === tr.id &&
            c.start < t1 &&
            c.start + c.duration > t0
          ) {
            hits.push(c.id);
          }
        }
      }
      top += h;
    }
    s.selectClips(
      d.additive ? [...new Set([...d.base, ...hits])] : hits,
      "replace",
    );
  }, []);

  const onLanePointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const d = dragRef.current;
    if (!d) return;
    if (d.type === "pan") {
      const el = scrollRef.current;
      if (!el) return;
      el.scrollLeft = d.startLeft - (e.clientX - d.originX);
      el.scrollTop = d.startTop - (e.clientY - d.originY);
      return;
    }
    if (d.type === "marquee") {
      const next = {
        ...d,
        t: pointerTime(e.clientX),
        y: Math.max(0, laneY(e.clientY)),
        moved: true,
      };
      setDrag(next);
      applyMarquee(next);
    }
  };

  const onLanePointerUp = () => {
    const d = dragRef.current;
    if (!d) return;
    if (d.type === "marquee" && !d.moved && !d.additive) {
      useTimelineStore.getState().clearSelection();
    }
    if (d.type === "marquee" || d.type === "pan") setDrag(null);
  };

  // -------------------------------------------------------- Kontextmenüs
  const onClipContextMenu = (e: MouseEvent, clip: TimelineClip) => {
    const store = useTimelineStore.getState();
    if (!store.selectedClipIds.includes(clip.id)) {
      store.selectClips([clip.id], "replace");
    }
    const t = pointerTime(e.clientX);
    const selected = store.clips.filter((c) =>
      useTimelineStore.getState().selectedClipIds.includes(c.id),
    );
    const allEnabled = selected.every((c) => c.enabled);
    const asset = assetById.get(clip.assetId);
    const items: MenuEntry[] = [
      {
        label: "Hier schneiden",
        icon: Scissors,
        onSelect: () => useTimelineStore.getState().splitAt(t, [clip.id]),
      },
      commandItem("timeline.splitAtPlayhead"),
      { type: "separator" },
      commandItem("timeline.copy", { icon: Copy }),
      commandItem("timeline.cut"),
      commandItem("timeline.paste", { icon: ClipboardPaste }),
      { type: "separator" },
      commandItem("timeline.toggleClipEnabled", {
        icon: Eye,
        checked: allEnabled,
      }),
      commandItem("timeline.toggleLink", {
        icon: Link2,
        checked: selected.some((c) => c.linkId !== null),
      }),
      { type: "separator" },
      {
        label: "Im Medien-Browser anzeigen",
        icon: FolderOpen,
        disabled: asset === undefined,
        onSelect: () => {
          useMediaStore.getState().select([clip.assetId]);
          openPanel("media");
        },
      },
      {
        label: "In Quellmonitor laden",
        icon: Film,
        disabled: asset === undefined,
        onSelect: () => {
          useMediaStore.getState().select([clip.assetId]);
          void commandRegistry.execute("media.openInSource", {
            assetId: clip.assetId,
          });
        },
      },
      { type: "separator" },
      commandItem("timeline.deleteSelected", { icon: Trash2, danger: true }),
      commandItem("timeline.rippleDelete", { danger: true }),
    ];
    openContextMenu(e, items);
  };

  const onLaneContextMenu = (e: MouseEvent, track: TimelineTrack | null) => {
    const store = useTimelineStore.getState();
    const t = pointerTime(e.clientX);
    const items: MenuEntry[] = [
      {
        label: "Hier einfügen",
        icon: ClipboardPaste,
        disabled: store.clipboard.length === 0,
        onSelect: () => useTimelineStore.getState().paste(Math.max(0, t)),
      },
      commandItem("timeline.selectAll"),
      commandItem("timeline.deselectAll"),
      { type: "separator" },
      commandItem("timeline.toggleSnapping", {
        icon: Magnet,
        checked: store.snapping,
      }),
      commandItem("timeline.zoomFit", { icon: Maximize2 }),
      { type: "separator" },
      commandItem("timeline.addVideoTrack", { icon: Plus }),
      commandItem("timeline.addAudioTrack", { icon: Plus }),
      ...(track
        ? [
            {
              label: `Spur ${trackName(track, store.tracks)} entfernen`,
              icon: Trash2,
              danger: true,
              onSelect: () => useTimelineStore.getState().removeTrack(track.id),
            } satisfies MenuEntry,
          ]
        : []),
    ];
    openContextMenu(e, items);
  };

  const onTrackHeaderContextMenu = (e: MouseEvent, track: TimelineTrack) => {
    const store = useTimelineStore.getState();
    const name = trackName(track, store.tracks);
    openContextMenu(e, [
      {
        label: "Stummschalten",
        checked: track.muted,
        onSelect: () =>
          useTimelineStore.getState().toggleTrackFlag(track.id, "muted"),
      },
      {
        label: "Solo",
        checked: track.solo,
        onSelect: () =>
          useTimelineStore.getState().toggleTrackFlag(track.id, "solo"),
      },
      {
        label: "Sperren",
        checked: track.locked,
        onSelect: () =>
          useTimelineStore.getState().toggleTrackFlag(track.id, "locked"),
      },
      { type: "separator" },
      commandItem("timeline.addVideoTrack", { icon: Plus }),
      commandItem("timeline.addAudioTrack", { icon: Plus }),
      {
        label: `Spur ${name} entfernen`,
        icon: Trash2,
        danger: true,
        onSelect: () => useTimelineStore.getState().removeTrack(track.id),
      },
    ]);
  };

  // -------------------------------------------- Lineal: Playhead + Loop
  const onRulerPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    const s = useTimelineStore.getState();
    const t = Math.max(0, pointerTime(e.clientX));
    const edgeSec = EDGE_PX / s.zoomPxPerSec;

    if (e.altKey) {
      // Alt+Ziehen definiert den Loop-Bereich (In/Out) der Sequenz.
      rulerDragRef.current = { mode: "range", originT: t };
      return;
    }
    if (s.inPoint !== null && Math.abs(t - s.inPoint) <= edgeSec) {
      rulerDragRef.current = { mode: "edge", edge: "in" };
      return;
    }
    if (s.outPoint !== null && Math.abs(t - s.outPoint) <= edgeSec) {
      rulerDragRef.current = { mode: "edge", edge: "out" };
      return;
    }
    rulerDragRef.current = { mode: "scrub" };
    s.setPlayhead(t);
  };

  const onRulerPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (
      (e.buttons & 1) === 0 ||
      !e.currentTarget.hasPointerCapture(e.pointerId)
    ) {
      return;
    }
    const d = rulerDragRef.current;
    if (!d) return;
    const s = useTimelineStore.getState();
    const t = Math.max(0, pointerTime(e.clientX));
    if (d.mode === "scrub") {
      s.setPlayhead(t);
    } else if (d.mode === "range") {
      s.setInOutRange(d.originT, t);
    } else if (d.edge === "in") {
      s.setInPoint(
        Math.max(
          0,
          Math.min(
            t,
            (s.outPoint ?? Number.POSITIVE_INFINITY) - MIN_CLIP_DURATION,
          ),
        ),
      );
    } else {
      s.setOutPoint(Math.max(t, (s.inPoint ?? 0) + MIN_CLIP_DURATION));
    }
  };

  const onRulerContextMenu = (e: MouseEvent) => {
    e.stopPropagation();
    const t = Math.max(0, pointerTime(e.clientX));
    const store = useTimelineStore.getState();
    openContextMenu(e, [
      {
        label: "In-Punkt hier setzen",
        icon: ArrowRightFromLine,
        onSelect: () => useTimelineStore.getState().setInPoint(t),
      },
      {
        label: "Out-Punkt hier setzen",
        icon: ArrowRightToLine,
        onSelect: () => useTimelineStore.getState().setOutPoint(t),
      },
      { type: "separator" },
      {
        label: "Loop-Bereich entfernen",
        icon: X,
        disabled: store.inPoint === null && store.outPoint === null,
        onSelect: () => useTimelineStore.getState().clearInOut(),
      },
    ]);
  };

  // ----------------------------------------------------------- Drag&Drop
  const dropTarget = useCallback(
    (e: DragEvent): { ids: string[]; t: number; trackId: string | null; snapTime: number | null } | null => {
      const ids = getDraggedAssets();
      if (ids.length === 0) return null;
      collectSnapTargets(new Set());
      const raw = Math.max(0, pointerTime(e.clientX));
      const snapped = snapAdjust([raw], 0);
      const t = Math.max(0, raw + snapped.delta);
      const lane = laneAtY(e.clientY);
      return { ids, t, trackId: lane?.id ?? null, snapTime: snapped.snapTime };
    },
    [collectSnapTargets, laneAtY, pointerTime, snapAdjust],
  );

  const onDragOver = (e: DragEvent<HTMLDivElement>) => {
    if (!e.dataTransfer.types.includes(ASSET_DRAG_MIME)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
    const target = dropTarget(e);
    if (!target) return;
    setDropPreview({
      placements: planAssetPlacements(target.ids, target.t, target.trackId),
      snapTime: target.snapTime,
    });
  };

  const onDragLeave = (e: DragEvent<HTMLDivElement>) => {
    if (!e.currentTarget.contains(e.relatedTarget as Node | null)) {
      setDropPreview(null);
    }
  };

  const onDrop = (e: DragEvent<HTMLDivElement>) => {
    if (!e.dataTransfer.types.includes(ASSET_DRAG_MIME)) return;
    e.preventDefault();
    let target = dropTarget(e);
    if (!target) {
      // Fallback: IDs aus dem dataTransfer (z. B. wenn das Modul leer ist).
      try {
        const ids = JSON.parse(
          e.dataTransfer.getData(ASSET_DRAG_MIME) || "[]",
        ) as string[];
        if (ids.length > 0) {
          target = {
            ids,
            t: Math.max(0, pointerTime(e.clientX)),
            trackId: laneAtY(e.clientY)?.id ?? null,
            snapTime: null,
          };
        }
      } catch {
        target = null;
      }
    }
    setDropPreview(null);
    clearDraggedAssets();
    if (!target) return;
    useTimelineStore
      .getState()
      .insertAssets(target.ids, { at: target.t, trackId: target.trackId });
  };

  // ------------------------------------------------------------- Vorschau
  const previewClips = useMemo(() => {
    if (!drag) return clips;
    if (drag.type === "move") {
      const ids = new Set(drag.clipIds);
      const vLanes = tracks.filter((t) => t.kind === "video");
      const aLanes = tracks.filter((t) => t.kind === "audio");
      return clips.map((c) => {
        if (!ids.has(c.id)) return c;
        let trackId = c.trackId;
        if (drag.laneOffset !== 0) {
          const lanes = c.kind === "video" ? vLanes : aLanes;
          const idx = lanes.findIndex((t) => t.id === c.trackId);
          if (idx >= 0) {
            trackId =
              lanes[
                Math.min(Math.max(idx + drag.laneOffset, 0), lanes.length - 1)
              ].id;
          }
        }
        return { ...c, start: c.start + drag.deltaSec, trackId };
      });
    }
    if (drag.type === "trim") {
      const ids = new Set(expandLinks(clips, [drag.clipId]));
      return clips.map((c) =>
        ids.has(c.id) ? applyTrimPreview(c, drag.edge, drag.deltaSec) : c,
      );
    }
    if (drag.type === "roll") {
      return clips.map((c) => {
        if (c.id === drag.leftId) return applyTrimPreview(c, "end", drag.deltaSec);
        if (c.id === drag.rightId)
          return applyTrimPreview(c, "start", drag.deltaSec);
        return c;
      });
    }
    if (drag.type === "slip") {
      const ids = new Set(expandLinks(clips, [drag.clipId]));
      return clips.map((c) =>
        ids.has(c.id) && Number.isFinite(c.srcDuration)
          ? { ...c, srcIn: c.srcIn + drag.deltaSec }
          : c,
      );
    }
    if (drag.type === "slide") {
      const ids = new Set(expandLinks(clips, [drag.clipId]));
      return clips.map((c) =>
        ids.has(c.id) ? { ...c, start: c.start + drag.deltaSec } : c,
      );
    }
    return clips;
  }, [clips, drag, tracks]);

  // Tick-Raster abhängig vom Zoom: Haupt-Ticks mit >= 80 px Abstand.
  const step =
    TICK_STEPS.find((s) => s * zoom >= MAJOR_TICK_MIN_PX) ??
    TICK_STEPS[TICK_STEPS.length - 1];
  const minorStep = step / MINOR_PER_MAJOR;
  const visibleEndPx = scrollLeft + Math.max(viewportW, 800);
  const firstIdx = Math.max(
    0,
    Math.floor(Math.max(0, scrollLeft - TRACK_HEADER_W) / zoom / minorStep),
  );
  const lastIdx = Math.ceil(
    Math.min(visibleEndPx / zoom, contentWidth / zoom) / minorStep,
  );
  const ticks: { idx: number; t: number; major: boolean }[] = [];
  for (let i = firstIdx; i <= lastIdx; i++) {
    ticks.push({ idx: i, t: i * minorStep, major: i % MINOR_PER_MAJOR === 0 });
  }

  const snapLineTime =
    (drag && "snapTime" in drag ? drag.snapTime : null) ??
    dropPreview?.snapTime ??
    null;

  const marqueeRect =
    drag?.type === "marquee" && drag.moved
      ? {
          left: TRACK_HEADER_W + Math.min(drag.originT, drag.t) * zoom,
          top: RULER_H + Math.min(drag.originY, drag.y),
          width: Math.abs(drag.t - drag.originT) * zoom,
          height: Math.abs(drag.y - drag.originY),
        }
      : null;

  const newTrackPlacements = {
    video: dropPreview?.placements.filter((p) => p.trackId === "newVideo") ?? [],
    audio: dropPreview?.placements.filter((p) => p.trackId === "newAudio") ?? [],
  };

  const laneCursor =
    activeTool === "razor"
      ? "cursor-crosshair"
      : activeTool === "hand"
        ? drag?.type === "pan"
          ? "cursor-grabbing"
          : "cursor-grab"
        : activeTool === "zoom"
          ? "cursor-zoom-in"
          : "";

  return (
    <div className="flex h-full min-w-0 bg-surface-1 text-text-1">
      {/* Werkzeugleiste */}
      <div className="flex w-9 shrink-0 flex-col items-center gap-1 border-r border-line py-2">
        {TOOLS.map(({ id, icon: Icon }) => (
          <button
            key={id}
            type="button"
            title={TOOL_LABELS[id]}
            onClick={() => void commandRegistry.execute(`tools.${id}`)}
            className={`flex size-7 items-center justify-center rounded ${
              activeTool === id
                ? "bg-surface-3 text-accent"
                : "text-text-2 hover:bg-surface-2 hover:text-text-1"
            }`}
          >
            <Icon className="size-4" />
          </button>
        ))}
      </div>

      <div className="flex min-w-0 flex-1 flex-col">
        {/* Kopfzeile */}
        <div className="flex h-9 shrink-0 items-center justify-between border-b border-line px-3">
          <span ref={timecodeRef} className="font-mono text-base text-accent">
            {formatTimecode(0, SEQUENCE_FPS)}
          </span>
          <div className="flex items-center gap-1">
            <button
              type="button"
              title="Einrasten umschalten (Snapping)"
              aria-pressed={snapping}
              onClick={() =>
                void commandRegistry.execute("timeline.toggleSnapping")
              }
              className={`flex size-7 items-center justify-center rounded ${
                snapping
                  ? "bg-surface-3 text-accent"
                  : "text-text-2 hover:bg-surface-2 hover:text-text-1"
              }`}
            >
              <Magnet className="size-4" />
            </button>
            <div className="mx-1 h-4 w-px bg-line" />
            <button
              type="button"
              title="Timeline verkleinern"
              onClick={() => void commandRegistry.execute("timeline.zoomOut")}
              className="flex size-7 items-center justify-center rounded text-text-2 hover:bg-surface-2 hover:text-text-1"
            >
              <ZoomOut className="size-4" />
            </button>
            <button
              type="button"
              title="Timeline vergrößern"
              onClick={() => void commandRegistry.execute("timeline.zoomIn")}
              className="flex size-7 items-center justify-center rounded text-text-2 hover:bg-surface-2 hover:text-text-1"
            >
              <ZoomIn className="size-4" />
            </button>
            <button
              type="button"
              title="An Sequenz anpassen"
              onClick={() => void commandRegistry.execute("timeline.zoomFit")}
              className="flex size-7 items-center justify-center rounded text-text-2 hover:bg-surface-2 hover:text-text-1"
            >
              <Maximize2 className="size-4" />
            </button>
          </div>
        </div>

        {/* Lineal + Spuren in einem gemeinsamen Scroll-Container */}
        <div
          ref={scrollRef}
          onScroll={(e) => setScrollLeft(e.currentTarget.scrollLeft)}
          onDragOver={onDragOver}
          onDragLeave={onDragLeave}
          onDrop={onDrop}
          onContextMenu={(e) => onLaneContextMenu(e, null)}
          className="min-h-0 flex-1 overflow-auto bg-surface-0"
        >
          <div
            className="relative min-w-full"
            style={{ width: TRACK_HEADER_W + contentWidth }}
          >
            {/* Lineal */}
            <div className="flex h-7 border-b border-line bg-surface-1">
              <div className="sticky left-0 z-2 w-24 shrink-0 border-r border-line bg-surface-1" />
              <div
                className="relative h-full shrink-0 cursor-ew-resize"
                style={{ width: contentWidth }}
                title="Ziehen: Playhead • Alt+Ziehen: Loop-Bereich"
                onPointerDown={onRulerPointerDown}
                onPointerMove={onRulerPointerMove}
                onPointerUp={() => {
                  rulerDragRef.current = null;
                }}
                onPointerCancel={() => {
                  rulerDragRef.current = null;
                }}
                onContextMenu={onRulerContextMenu}
              >
                {/* Loop-Bereich (In/Out) der Sequenz */}
                {inPoint !== null && outPoint !== null ? (
                  <div
                    className="pointer-events-none absolute inset-y-0 border-x border-accent bg-accent/20"
                    style={{
                      left: Math.round(inPoint * zoom),
                      width: Math.max(
                        Math.round((outPoint - inPoint) * zoom),
                        2,
                      ),
                    }}
                  />
                ) : (
                  [inPoint, outPoint].map(
                    (mark, i) =>
                      mark !== null && (
                        <div
                          key={i}
                          className="pointer-events-none absolute inset-y-0 w-0.5 bg-accent/80"
                          style={{ left: Math.round(mark * zoom) }}
                        />
                      ),
                  )
                )}
                {ticks.map(({ idx, t, major }) => (
                  <div
                    key={idx}
                    className="absolute top-0 h-full"
                    style={{ left: Math.round(t * zoom) }}
                  >
                    <div
                      className={`absolute bottom-0 w-px ${
                        major ? "h-2.5 bg-line-strong" : "h-1.5 bg-line"
                      }`}
                    />
                    {major && (
                      <span className="absolute left-1 top-0.5 font-mono text-xs text-text-3">
                        {formatDuration(t)}
                      </span>
                    )}
                  </div>
                ))}
              </div>
            </div>

            {/* Spuren */}
            {tracks.map((track) => (
              <div
                key={track.id}
                className={`flex border-b border-line ${
                  track.kind === "video" ? "h-12" : "h-10"
                }`}
              >
                <TrackHeaderCell
                  track={track}
                  name={trackName(track, tracks)}
                  onContextMenu={onTrackHeaderContextMenu}
                />
                <div
                  className={`relative h-full shrink-0 ${laneCursor} ${
                    track.locked ? "bg-surface-2/40" : ""
                  }`}
                  style={{ width: contentWidth }}
                  onPointerDown={onLanePointerDown}
                  onPointerMove={onLanePointerMove}
                  onPointerUp={onLanePointerUp}
                  onPointerCancel={() => setDrag(null)}
                  onContextMenu={(e) => onLaneContextMenu(e, track)}
                >
                  {previewClips
                    .filter((c) => c.trackId === track.id)
                    .map((clip) => (
                      <ClipView
                        key={clip.id}
                        clip={clip}
                        asset={assetById.get(clip.assetId)}
                        zoom={zoom}
                        selected={selectedClipIds.includes(clip.id)}
                        locked={track.locked}
                        razor={activeTool === "razor"}
                        onPointerDown={onClipPointerDown}
                        onContextMenu={onClipContextMenu}
                        onDoubleClick={(c) => {
                          if (assetById.has(c.assetId)) {
                            useMediaStore.getState().select([c.assetId]);
                            void commandRegistry.execute("media.openInSource", {
                              assetId: c.assetId,
                            });
                          }
                        }}
                      />
                    ))}
                  {dropPreview?.placements
                    .filter((p) => p.trackId === track.id)
                    .map((p, i) => (
                      <div
                        key={i}
                        className="pointer-events-none absolute inset-y-1 rounded border border-dashed border-accent bg-accent/10"
                        style={{
                          left: Math.round(p.start * zoom),
                          width: Math.max(Math.round(p.duration * zoom), 3),
                        }}
                      />
                    ))}
                </div>
              </div>
            ))}

            {/* Vorschau für automatisch anzulegende Spuren beim Drop */}
            {(["video", "audio"] as const).map((kind) => {
              const placements = newTrackPlacements[kind];
              if (placements.length === 0) return null;
              return (
                <div
                  key={kind}
                  className={`flex border-b border-dashed border-accent/60 ${
                    kind === "video" ? "h-12" : "h-10"
                  }`}
                >
                  <div className="sticky left-0 z-2 flex w-24 shrink-0 items-center gap-1 border-r border-line bg-surface-2 px-2 text-xs text-accent">
                    {kind === "video" ? (
                      <Film className="size-3.5" />
                    ) : (
                      <Music className="size-3.5" />
                    )}
                    Neue Spur
                  </div>
                  <div
                    className="relative h-full shrink-0"
                    style={{ width: contentWidth }}
                  >
                    {placements.map((p, i) => (
                      <div
                        key={i}
                        className="pointer-events-none absolute inset-y-1 rounded border border-dashed border-accent bg-accent/10"
                        style={{
                          left: Math.round(p.start * zoom),
                          width: Math.max(Math.round(p.duration * zoom), 3),
                        }}
                      />
                    ))}
                  </div>
                </div>
              );
            })}

            {/* Hinweis bei leerer Sequenz */}
            {clips.length === 0 && !dropPreview && (
              <div
                className="pointer-events-none absolute flex items-center justify-center"
                style={{
                  left: TRACK_HEADER_W,
                  top: RULER_H,
                  width: Math.max(viewportW - TRACK_HEADER_W, 0),
                  height: lanesHeight,
                }}
              >
                <p className="text-xs text-text-3">
                  Medien aus dem Medien-Browser hierher ziehen
                </p>
              </div>
            )}

            {/* Snap-Hilfslinie */}
            {snapLineTime !== null && (
              <div
                className="pointer-events-none absolute inset-y-0 z-1 w-px bg-warning"
                style={{ left: TRACK_HEADER_W + snapLineTime * zoom }}
              />
            )}

            {/* Auswahlrechteck */}
            {marqueeRect && (
              <div
                className="pointer-events-none absolute z-1 border border-accent bg-accent/10"
                style={marqueeRect}
              />
            )}

            {/* Playhead über Lineal + Spuren (unter den Sticky-Spurköpfen) */}
            <div
              ref={playheadRef}
              className="pointer-events-none absolute inset-y-0 left-0 z-1 w-px bg-accent"
            >
              <div className="absolute -left-1 top-0 size-0 border-x-4 border-t-6 border-x-transparent border-t-accent" />
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
