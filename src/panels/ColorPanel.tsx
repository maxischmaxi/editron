import { useState, type ReactNode } from "react";
import { ChevronDown, ChevronRight, RotateCcw } from "lucide-react";
import {
  useColorStore,
  type BasicCorrection,
  type LookId,
} from "@/panels/colorStore";

const LOOKS: { id: LookId; label: string }[] = [
  { id: "neutral", label: "Neutral" },
  { id: "warm", label: "Film warm" },
  { id: "cold", label: "Film kalt" },
  { id: "bw", label: "S/W" },
];

/* ------------------------------------------------------------------ */
/* Bausteine                                                           */
/* ------------------------------------------------------------------ */

function Section({
  title,
  children,
  defaultOpen = true,
}: {
  title: string;
  children: ReactNode;
  defaultOpen?: boolean;
}) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <div className="border-b border-line">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className="flex h-8 w-full items-center gap-1.5 px-2 text-xs font-medium text-text-1 hover:bg-surface-2"
      >
        {open ? (
          <ChevronDown size={14} className="shrink-0 text-text-3" />
        ) : (
          <ChevronRight size={14} className="shrink-0 text-text-3" />
        )}
        <span>{title}</span>
      </button>
      {open && <div className="space-y-1.5 px-2 pb-2.5">{children}</div>}
    </div>
  );
}

function SliderRow({
  label,
  value,
  min,
  max,
  defaultValue,
  onChange,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  defaultValue: number;
  onChange: (v: number) => void;
}) {
  return (
    <div className="flex items-center gap-2">
      <button
        type="button"
        onDoubleClick={() => onChange(defaultValue)}
        title="Doppelklick: zurücksetzen"
        className="w-22 shrink-0 cursor-default truncate text-left text-xs text-text-2"
      >
        {label}
      </button>
      <input
        type="range"
        min={min}
        max={max}
        step={1}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        className="h-1 min-w-0 flex-1 cursor-pointer accent-accent"
      />
      <span className="w-9 shrink-0 text-right font-mono text-xs text-text-1">
        {value}
      </span>
    </div>
  );
}

function ColorWheel({ label }: { label: string }) {
  return (
    <div
      className="flex flex-col items-center gap-1"
      title="Farbräder folgen"
    >
      <div
        className="relative h-16 w-16 rounded-full border border-line opacity-80"
        style={{
          background:
            "radial-gradient(circle, var(--color-surface-3) 0%, transparent 70%), conic-gradient(from 90deg, #f33, #ff3, #3f6, #3ff, #36f, #f3f, #f33)",
        }}
      >
        <div className="absolute top-1/2 left-1/2 h-1.5 w-1.5 -translate-x-1/2 -translate-y-1/2 rounded-full border border-surface-0 bg-text-1" />
      </div>
      <span className="text-xs text-text-3">{label}</span>
    </div>
  );
}

/* ------------------------------------------------------------------ */
/* Panel                                                               */
/* ------------------------------------------------------------------ */

export function ColorPanel() {
  /* Korrekturwerte leben im colorStore (überleben Workspace-Wechsel);
     der Store schreibt die Live-Vorschau selbst in den monitorStore. */
  const basic = useColorStore((s) => s.basic);
  const look = useColorStore((s) => s.look);
  const intensity = useColorStore((s) => s.intensity);
  const setBasicValue = useColorStore((s) => s.setBasicValue);
  const setLook = useColorStore((s) => s.setLook);
  const setIntensity = useColorStore((s) => s.setIntensity);
  const resetAll = useColorStore((s) => s.resetAll);

  const setValue = (key: keyof BasicCorrection) => (v: number) =>
    setBasicValue(key, v);

  return (
    <div className="flex h-full flex-col bg-surface-1">
      <div className="min-h-0 flex-1 overflow-y-auto">
        <Section title="Basiskorrektur">
          <SliderRow
            label="Temperatur"
            value={basic.temperature}
            min={-100}
            max={100}
            defaultValue={0}
            onChange={setValue("temperature")}
          />
          <SliderRow
            label="Farbton"
            value={basic.tint}
            min={-100}
            max={100}
            defaultValue={0}
            onChange={setValue("tint")}
          />
          <SliderRow
            label="Belichtung"
            value={basic.exposure}
            min={-100}
            max={100}
            defaultValue={0}
            onChange={setValue("exposure")}
          />
          <SliderRow
            label="Kontrast"
            value={basic.contrast}
            min={-100}
            max={100}
            defaultValue={0}
            onChange={setValue("contrast")}
          />
          <SliderRow
            label="Sättigung"
            value={basic.saturation}
            min={0}
            max={200}
            defaultValue={100}
            onChange={setValue("saturation")}
          />
          <div className="flex justify-end pt-1">
            <button
              type="button"
              onClick={resetAll}
              className="flex h-6 items-center gap-1.5 rounded border border-line px-2 text-xs text-text-2 hover:border-line-strong hover:bg-surface-3 hover:text-text-1"
            >
              <RotateCcw size={12} />
              Zurücksetzen
            </button>
          </div>
        </Section>

        <Section title="Kreativ">
          <div className="flex items-center gap-2">
            <span className="w-22 shrink-0 text-xs text-text-2">Look</span>
            <select
              value={look}
              onChange={(e) => setLook(e.target.value as LookId)}
              className="h-6 min-w-0 flex-1 rounded border border-line bg-surface-3 px-1 text-xs text-text-1 focus:border-accent focus:outline-none"
            >
              {LOOKS.map((l) => (
                <option key={l.id} value={l.id}>
                  {l.label}
                </option>
              ))}
            </select>
          </div>
          <SliderRow
            label="Intensität"
            value={intensity}
            min={0}
            max={100}
            defaultValue={100}
            onChange={setIntensity}
          />
        </Section>

        <Section title="Farbräder">
          <div className="flex items-center justify-around py-1">
            <ColorWheel label="Schatten" />
            <ColorWheel label="Mitteltöne" />
            <ColorWheel label="Lichter" />
          </div>
        </Section>
      </div>

      <div className="shrink-0 border-t border-line px-3 py-1.5 text-xs text-text-3">
        CSS-Filter-Vorschau auf dem Programmmonitor — FFmpeg-Render folgt.
      </div>
    </div>
  );
}
