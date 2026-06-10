import { useState, type ReactNode } from "react";
import { ChevronDown, ChevronRight, RotateCcw, Timer } from "lucide-react";
import type { MediaAsset } from "@/core/types";
import { useMediaStore } from "@/stores/mediaStore";
import { usePlaybackStore } from "@/stores/playbackStore";

/* ------------------------------------------------------------------ */
/* Bausteine                                                           */
/* ------------------------------------------------------------------ */

function Section({
  title,
  onReset,
  children,
}: {
  title: string;
  onReset: () => void;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(true);
  return (
    <div className="border-b border-line">
      <div className="flex h-8 items-center gap-1.5 px-2">
        <button
          type="button"
          onClick={() => setOpen((o) => !o)}
          className="flex min-w-0 flex-1 items-center gap-1.5 text-xs font-medium text-text-1 hover:text-accent-hover"
        >
          {open ? (
            <ChevronDown size={14} className="shrink-0 text-text-3" />
          ) : (
            <ChevronRight size={14} className="shrink-0 text-text-3" />
          )}
          <span className="truncate">{title}</span>
        </button>
        <button
          type="button"
          onClick={onReset}
          title="Abschnitt zurücksetzen"
          className="rounded p-1 text-text-3 hover:bg-surface-3 hover:text-text-1"
        >
          <RotateCcw size={14} />
        </button>
      </div>
      {open && <div className="space-y-1.5 px-2 pb-2.5">{children}</div>}
    </div>
  );
}

function ParamRow({
  label,
  value,
  min,
  max,
  step = 1,
  unit,
  onChange,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step?: number;
  unit?: string;
  onChange: (v: number) => void;
}) {
  return (
    <div className="flex items-center gap-2">
      <button
        type="button"
        disabled
        title="Keyframes folgen"
        className="shrink-0 cursor-not-allowed rounded p-0.5 text-text-3 opacity-50"
      >
        <Timer size={14} />
      </button>
      <span className="w-24 shrink-0 truncate text-xs text-text-2">
        {label}
      </span>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        className="h-1 min-w-0 flex-1 cursor-pointer accent-accent"
      />
      <span className="flex shrink-0 items-center gap-1">
        <input
          type="number"
          min={min}
          max={max}
          step={step}
          value={value}
          onChange={(e) => {
            const v = Number(e.target.value);
            if (!Number.isNaN(v)) onChange(Math.min(max, Math.max(min, v)));
          }}
          className="h-5 w-14 rounded border border-line bg-surface-3 px-1 text-right font-mono text-xs text-text-1 focus:border-accent focus:outline-none [&::-webkit-inner-spin-button]:appearance-none [&::-webkit-outer-spin-button]:appearance-none"
        />
        {unit && <span className="w-3 text-xs text-text-3">{unit}</span>}
      </span>
    </div>
  );
}

/* ------------------------------------------------------------------ */
/* Panel                                                               */
/* ------------------------------------------------------------------ */

const MOTION_DEFAULTS = { posX: 0, posY: 0, scale: 100, rotation: 0 };
const OPACITY_DEFAULTS = { opacity: 100, blendMode: "normal" };
const TIME_DEFAULTS = { speed: 100 };

const BLEND_MODES: { id: string; label: string }[] = [
  { id: "normal", label: "Normal" },
  { id: "darken", label: "Abdunkeln" },
  { id: "multiply", label: "Multiplizieren" },
  { id: "lighten", label: "Aufhellen" },
  { id: "screen", label: "Negativ multiplizieren" },
  { id: "overlay", label: "Ineinanderkopieren" },
];

function EffectControls({ asset }: { asset: MediaAsset }) {
  const [motion, setMotion] = useState(MOTION_DEFAULTS);
  const [opacity, setOpacity] = useState(OPACITY_DEFAULTS);
  const [time, setTime] = useState(TIME_DEFAULTS);

  return (
    <div className="flex h-full flex-col">
      <div className="flex h-9 shrink-0 items-center border-b border-line px-3">
        <span
          className="truncate text-xs font-medium text-text-1"
          title={asset.path}
        >
          {asset.name}
        </span>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto">
        <Section title="Bewegung" onReset={() => setMotion(MOTION_DEFAULTS)}>
          <ParamRow
            label="Position X"
            value={motion.posX}
            min={-1000}
            max={1000}
            onChange={(v) => setMotion((m) => ({ ...m, posX: v }))}
          />
          <ParamRow
            label="Position Y"
            value={motion.posY}
            min={-1000}
            max={1000}
            onChange={(v) => setMotion((m) => ({ ...m, posY: v }))}
          />
          <ParamRow
            label="Skalierung"
            value={motion.scale}
            min={0}
            max={400}
            unit="%"
            onChange={(v) => setMotion((m) => ({ ...m, scale: v }))}
          />
          <ParamRow
            label="Rotation"
            value={motion.rotation}
            min={-180}
            max={180}
            unit="°"
            onChange={(v) => setMotion((m) => ({ ...m, rotation: v }))}
          />
        </Section>

        <Section title="Deckkraft" onReset={() => setOpacity(OPACITY_DEFAULTS)}>
          <ParamRow
            label="Deckkraft"
            value={opacity.opacity}
            min={0}
            max={100}
            unit="%"
            onChange={(v) => setOpacity((o) => ({ ...o, opacity: v }))}
          />
          <div className="flex items-center gap-2 pl-6">
            <span className="w-24 shrink-0 text-xs text-text-2">
              Mischmodus
            </span>
            <select
              value={opacity.blendMode}
              onChange={(e) =>
                setOpacity((o) => ({ ...o, blendMode: e.target.value }))
              }
              className="h-6 min-w-0 flex-1 rounded border border-line bg-surface-3 px-1 text-xs text-text-1 focus:border-accent focus:outline-none"
            >
              {BLEND_MODES.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.label}
                </option>
              ))}
            </select>
          </div>
        </Section>

        <Section
          title="Zeitneuzuordnung"
          onReset={() => setTime(TIME_DEFAULTS)}
        >
          <ParamRow
            label="Geschwindigkeit"
            value={time.speed}
            min={10}
            max={400}
            unit="%"
            onChange={(v) => setTime({ speed: v })}
          />
        </Section>
      </div>

      <div className="shrink-0 border-t border-line bg-surface-1 px-3 py-1.5 text-xs text-text-3">
        Werte sind eine Vorschau — die Render-Verknüpfung folgt.
      </div>
    </div>
  );
}

export function EffectControlsPanel() {
  const assets = useMediaStore((s) => s.assets);
  const selectedAssetIds = useMediaStore((s) => s.selectedAssetIds);
  const sourceAssetId = usePlaybackStore((s) => s.sourceAssetId);

  const asset =
    assets.find((a) => a.id === selectedAssetIds[0]) ??
    assets.find((a) => a.id === sourceAssetId) ??
    null;

  if (!asset) {
    return (
      <div className="flex h-full items-center justify-center bg-surface-1">
        <p className="text-xs text-text-3">Clip auswählen</p>
      </div>
    );
  }

  /* key erzwingt frischen lokalen State pro Clip */
  return (
    <div className="h-full bg-surface-1">
      <EffectControls key={asset.id} asset={asset} />
    </div>
  );
}
