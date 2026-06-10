import type { MouseEvent } from "react";
import { Headphones, Lock, Volume2 } from "lucide-react";
import type { TimelineTrack } from "@/core/timeline/types";
import { useTimelineStore } from "@/core/timeline/timelineStore";

/**
 * Spurkopf: Name (V1/A1 …) plus Stumm/Solo/Sperren.
 * Sperren blockiert sämtliche Bearbeitung der Spur; Stumm/Solo werden
 * gespeichert und greifen, sobald die Sequenz-Wiedergabe existiert.
 */
export function TrackHeaderCell({
  track,
  name,
  onContextMenu,
}: {
  track: TimelineTrack;
  name: string;
  onContextMenu: (e: MouseEvent, track: TimelineTrack) => void;
}) {
  const toggleTrackFlag = useTimelineStore((s) => s.toggleTrackFlag);

  return (
    <div
      onContextMenu={(e) => onContextMenu(e, track)}
      className={`sticky left-0 z-2 flex w-24 shrink-0 items-center gap-1 border-r border-line px-2 ${
        track.locked ? "bg-surface-3" : "bg-surface-2"
      }`}
    >
      <span className="min-w-0 flex-1 truncate text-xs font-medium text-text-2">
        {name}
      </span>
      <button
        type="button"
        title="Stummschalten — greift bei der künftigen Sequenz-Wiedergabe"
        aria-pressed={track.muted}
        onClick={() => toggleTrackFlag(track.id, "muted")}
        className={
          track.muted
            ? "rounded-sm bg-warning/20 text-warning"
            : "rounded-sm text-text-3 hover:text-text-1"
        }
      >
        <Volume2 className="size-3.5" />
      </button>
      <button
        type="button"
        title="Solo — greift bei der künftigen Sequenz-Wiedergabe"
        aria-pressed={track.solo}
        onClick={() => toggleTrackFlag(track.id, "solo")}
        className={
          track.solo
            ? "rounded-sm bg-success/20 text-success"
            : "rounded-sm text-text-3 hover:text-text-1"
        }
      >
        <Headphones className="size-3.5" />
      </button>
      <button
        type="button"
        title="Sperren — Spur ist von der Bearbeitung ausgenommen"
        aria-pressed={track.locked}
        onClick={() => toggleTrackFlag(track.id, "locked")}
        className={
          track.locked
            ? "rounded-sm bg-accent-soft text-accent"
            : "rounded-sm text-text-3 hover:text-text-1"
        }
      >
        <Lock className="size-3.5" />
      </button>
    </div>
  );
}
