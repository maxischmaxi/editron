import { useEffect, useRef, useState, type Ref } from "react";
import { usePlaybackStore } from "@/stores/playbackStore";

/* ------------------------------------------------------------------ */
/* Pegelmeter — Vorschau-Animation (noch nicht signalbasiert)          */
/* ------------------------------------------------------------------ */

function MeterBar({ fillRef }: { fillRef: Ref<HTMLDivElement> }) {
  return (
    <div
      className="relative h-40 w-1 overflow-hidden rounded-full"
      style={{
        background:
          "linear-gradient(to top, var(--color-success) 0%, var(--color-success) 60%, var(--color-warning) 80%, var(--color-danger) 100%)",
      }}
    >
      {/* Abdeckung von oben: Höhe = ungefüllter Anteil */}
      <div
        ref={fillRef}
        className="absolute inset-x-0 top-0 bg-surface-3"
        style={{ height: "100%" }}
      />
    </div>
  );
}

/* ------------------------------------------------------------------ */
/* Kanalzug                                                            */
/* ------------------------------------------------------------------ */

function MixerStrip({
  label,
  isMaster = false,
}: {
  label: string;
  isMaster?: boolean;
}) {
  const [gain, setGain] = useState(0);
  const [pan, setPan] = useState(0);
  const [muted, setMuted] = useState(false);
  const [solo, setSolo] = useState(false);
  const isPlaying = usePlaybackStore((s) => s.isPlaying);

  const coverL = useRef<HTMLDivElement>(null);
  const coverR = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let raf = 0;
    let lvlL = 0;
    let lvlR = 0;
    let tgtL = 0;
    let tgtR = 0;
    let lastTarget = 0;
    const active = isPlaying && !muted;

    const apply = () => {
      if (coverL.current)
        coverL.current.style.height = `${(1 - lvlL) * 100}%`;
      if (coverR.current)
        coverR.current.style.height = `${(1 - lvlR) * 100}%`;
    };

    const tick = (t: number) => {
      if (active && t - lastTarget > 130) {
        lastTarget = t;
        tgtL = 0.3 + Math.random() * 0.6;
        tgtR = 0.3 + Math.random() * 0.6;
      }
      if (!active) {
        tgtL = 0;
        tgtR = 0;
      }
      /* schnelle Attack, langsamer gedämpfter Release */
      lvlL += (tgtL - lvlL) * (tgtL > lvlL ? 0.45 : 0.12);
      lvlR += (tgtR - lvlR) * (tgtR > lvlR ? 0.45 : 0.12);
      apply();
      /* Loop beenden, sobald alles abgeklungen ist */
      if (!active && lvlL < 0.005 && lvlR < 0.005) {
        lvlL = 0;
        lvlR = 0;
        apply();
        return;
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [isPlaying, muted]);

  const panLabel = pan === 0 ? "C" : pan < 0 ? `L${-pan}` : `R${pan}`;
  const gainLabel = gain <= -60 ? "-∞" : gain.toFixed(1);

  const faderClasses = [
    "absolute top-1/2 left-1/2 h-6 w-40 -translate-x-1/2 -translate-y-1/2 -rotate-90 cursor-pointer appearance-none bg-transparent",
    "[&::-webkit-slider-runnable-track]:h-1 [&::-webkit-slider-runnable-track]:rounded-full [&::-webkit-slider-runnable-track]:bg-surface-4",
    "[&::-webkit-slider-thumb]:-mt-2 [&::-webkit-slider-thumb]:h-5 [&::-webkit-slider-thumb]:w-2.5 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-xs [&::-webkit-slider-thumb]:border [&::-webkit-slider-thumb]:border-line-strong",
    isMaster
      ? "[&::-webkit-slider-thumb]:bg-accent hover:[&::-webkit-slider-thumb]:bg-accent-hover"
      : "[&::-webkit-slider-thumb]:bg-text-2 hover:[&::-webkit-slider-thumb]:bg-text-1",
  ].join(" ");

  return (
    <div className="flex w-16 shrink-0 flex-col items-center gap-1.5 rounded border border-line bg-surface-2 py-2">
      <span
        className={
          isMaster
            ? "text-xs font-medium text-text-1"
            : "text-xs font-medium text-text-2"
        }
      >
        {label}
      </span>

      {/* Pan */}
      <input
        type="range"
        min={-100}
        max={100}
        step={1}
        value={pan}
        onChange={(e) => setPan(Number(e.target.value))}
        onDoubleClick={() => setPan(0)}
        title="Panorama (Doppelklick: Mitte)"
        className="h-1 w-12 cursor-pointer accent-accent"
      />
      <span className="font-mono text-xs leading-3 text-text-3">
        {panLabel}
      </span>

      {/* Fader + Meter */}
      <div className="flex items-stretch gap-1.5">
        <div className="relative h-40 w-6">
          <input
            type="range"
            min={-60}
            max={6}
            step={0.5}
            value={gain}
            onChange={(e) => setGain(Number(e.target.value))}
            onDoubleClick={() => setGain(0)}
            title="Pegel (Doppelklick: 0 dB)"
            className={faderClasses}
          />
        </div>
        <div
          className="flex items-stretch gap-0.5"
          title="Pegelanzeige: Vorschau-Animation, noch nicht signalbasiert"
        >
          <MeterBar fillRef={coverL} />
          <MeterBar fillRef={coverR} />
        </div>
      </div>

      <span className="font-mono text-xs text-text-2">{gainLabel} dB</span>

      {/* Mute / Solo */}
      <div className="flex gap-1">
        <button
          type="button"
          onClick={() => setMuted((m) => !m)}
          title="Stumm schalten"
          className={
            muted
              ? "h-5 w-5 rounded border border-warning/40 bg-warning/20 text-xs font-medium text-warning"
              : "h-5 w-5 rounded border border-line text-xs font-medium text-text-3 hover:border-line-strong hover:text-text-1"
          }
        >
          M
        </button>
        <button
          type="button"
          onClick={() => setSolo((s) => !s)}
          title="Solo"
          className={
            solo
              ? "h-5 w-5 rounded border border-success/40 bg-success/20 text-xs font-medium text-success"
              : "h-5 w-5 rounded border border-line text-xs font-medium text-text-3 hover:border-line-strong hover:text-text-1"
          }
        >
          S
        </button>
      </div>
    </div>
  );
}

/* ------------------------------------------------------------------ */
/* Panel                                                               */
/* ------------------------------------------------------------------ */

export function AudioMixerPanel() {
  return (
    <div className="flex h-full flex-col bg-surface-1">
      <p className="shrink-0 border-b border-line px-3 py-1.5 text-xs text-text-3">
        Vorschau — Regler wirken noch nicht auf die Wiedergabe
      </p>
      <div className="min-h-0 flex-1 overflow-auto p-3">
        <div className="flex min-w-fit items-start justify-center gap-2">
          <MixerStrip label="A1" />
          <MixerStrip label="A2" />
          <MixerStrip label="A3" />
          <MixerStrip label="A4" />
          <div className="mx-1 w-px self-stretch bg-line" />
          <MixerStrip label="Master" isMaster />
        </div>
      </div>
    </div>
  );
}
