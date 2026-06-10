import type { ReactNode } from "react";
import {
  PREVIEW_SCALES,
  useMonitorStore,
  type PreviewScale,
} from "@/stores/monitorStore";
import type { MonitorId } from "@/stores/playbackStore";

/** Quadratischer Icon-Knopf der Monitor-Transportleisten. */
export function TransportButton({
  title,
  onClick,
  disabled,
  active = false,
  children,
}: {
  title: string;
  onClick: () => void;
  disabled?: boolean;
  active?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      title={title}
      onClick={onClick}
      disabled={disabled}
      className={`flex size-7 items-center justify-center rounded disabled:pointer-events-none disabled:opacity-40 ${
        active
          ? "bg-surface-3 text-accent"
          : "text-text-2 hover:bg-surface-3 hover:text-text-1"
      }`}
    >
      {children}
    </button>
  );
}

/**
 * Wiedergabeauflösung des Monitors: Voll, 1/2, 1/4, 1/8. Reduzierte
 * Stufen rendern intern kleiner und sparen bei aufwendigem Material
 * Rendering-Last; die Anzeige wird per CSS wieder eingepasst.
 */
export function PreviewScaleSelect({ monitor }: { monitor: MonitorId }) {
  const scale = useMonitorStore((s) => s.scale[monitor]);
  return (
    <select
      title="Wiedergabeauflösung"
      aria-label="Wiedergabeauflösung"
      value={String(scale)}
      onChange={(e) =>
        useMonitorStore
          .getState()
          .setScale(monitor, Number(e.target.value) as PreviewScale)
      }
      className="h-6 shrink-0 rounded border border-line bg-surface-2 px-1 text-xs text-text-2 focus:border-accent focus:outline-none"
    >
      {PREVIEW_SCALES.map((o) => (
        <option key={o.value} value={String(o.value)}>
          {o.label}
        </option>
      ))}
    </select>
  );
}
