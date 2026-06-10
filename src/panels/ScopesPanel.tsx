import { useEffect, useRef, useState } from "react";
import { MonitorOff } from "lucide-react";
import { useMonitorStore } from "@/stores/monitorStore";

/* Sampling-Auflösung: bewusst klein, damit getImageData billig bleibt */
const SAMPLE_W = 160;
const SAMPLE_H = 90;
/* ~10 Aktualisierungen pro Sekunde */
const SAMPLE_INTERVAL_MS = 100;

type ScopeMode = "waveform" | "histogram";

/* ------------------------------------------------------------------ */
/* Zeichenroutinen                                                     */
/* ------------------------------------------------------------------ */

function drawGraticule(ctx: CanvasRenderingContext2D, w: number, h: number) {
  ctx.strokeStyle = "rgba(255, 255, 255, 0.08)";
  ctx.lineWidth = 1;
  for (let i = 0; i <= 4; i++) {
    const y = Math.round((h * i) / 4) + 0.5;
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(w, y);
    ctx.stroke();
  }
}

function drawWaveform(
  ctx: CanvasRenderingContext2D,
  img: ImageData,
  w: number,
  h: number,
) {
  drawGraticule(ctx, w, h);
  const { data, width: sw, height: sh } = img;
  const colW = w / sw;
  ctx.globalCompositeOperation = "lighter";
  ctx.fillStyle = "rgba(70, 240, 130, 0.10)";
  for (let x = 0; x < sw; x++) {
    for (let y = 0; y < sh; y++) {
      const i = (y * sw + x) * 4;
      const luma =
        0.2126 * data[i] + 0.7152 * data[i + 1] + 0.0722 * data[i + 2];
      const py = h - 1 - (luma / 255) * (h - 2);
      ctx.fillRect(x * colW, py, Math.max(1, colW), 1);
    }
  }
  ctx.globalCompositeOperation = "source-over";
}

function drawHistogram(
  ctx: CanvasRenderingContext2D,
  img: ImageData,
  w: number,
  h: number,
) {
  drawGraticule(ctx, w, h);
  const { data } = img;
  const bins = [
    new Float32Array(256),
    new Float32Array(256),
    new Float32Array(256),
  ] as const;
  for (let i = 0; i < data.length; i += 4) {
    bins[0][data[i]]++;
    bins[1][data[i + 1]]++;
    bins[2][data[i + 2]]++;
  }
  let max = 1;
  for (const channel of bins) {
    for (let b = 0; b < 256; b++) if (channel[b] > max) max = channel[b];
  }
  const colors = [
    "rgba(255, 90, 90, 0.85)",
    "rgba(90, 240, 130, 0.85)",
    "rgba(110, 150, 255, 0.85)",
  ];
  ctx.globalCompositeOperation = "lighter";
  for (let c = 0; c < 3; c++) {
    ctx.beginPath();
    ctx.moveTo(0, h);
    for (let b = 0; b < 256; b++) {
      const x = (b / 255) * (w - 1);
      const y = h - 1 - (bins[c][b] / max) * (h - 4);
      ctx.lineTo(x, y);
    }
    ctx.lineTo(w, h);
    ctx.strokeStyle = colors[c];
    ctx.lineWidth = 1;
    ctx.stroke();
  }
  ctx.globalCompositeOperation = "source-over";
}

/* ------------------------------------------------------------------ */
/* Panel                                                               */
/* ------------------------------------------------------------------ */

export function ScopesPanel() {
  const [mode, setMode] = useState<ScopeMode>("waveform");
  const videoEl = useMonitorStore((s) => s.videoEl);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !videoEl) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    /* kleines Offscreen-Canvas für günstiges Pixel-Sampling */
    const sample = document.createElement("canvas");
    sample.width = SAMPLE_W;
    sample.height = SAMPLE_H;
    const sctx = sample.getContext("2d", { willReadFrequently: true });
    if (!sctx) return;

    let raf = 0;
    let last = 0;

    const tick = (t: number) => {
      raf = requestAnimationFrame(tick);
      /* Zeit-Gate: nur ~10x pro Sekunde tatsächlich samplen */
      if (t - last < SAMPLE_INTERVAL_MS) return;
      last = t;

      const w = canvas.clientWidth;
      const h = canvas.clientHeight;
      if (w === 0 || h === 0) return;
      if (canvas.width !== w || canvas.height !== h) {
        canvas.width = w;
        canvas.height = h;
      }

      ctx.fillStyle = "#000";
      ctx.fillRect(0, 0, w, h);

      if (videoEl.readyState < 2 || videoEl.videoWidth === 0) return;

      let img: ImageData;
      try {
        sctx.drawImage(videoEl, 0, 0, SAMPLE_W, SAMPLE_H);
        img = sctx.getImageData(0, 0, SAMPLE_W, SAMPLE_H);
      } catch {
        /* z. B. SecurityError bei nicht lesbarer Quelle */
        return;
      }

      if (mode === "waveform") drawWaveform(ctx, img, w, h);
      else drawHistogram(ctx, img, w, h);
    };

    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [videoEl, mode]);

  return (
    <div className="flex h-full flex-col bg-surface-1">
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-line px-2">
        <select
          value={mode}
          onChange={(e) => setMode(e.target.value as ScopeMode)}
          className="h-6 rounded border border-line bg-surface-3 px-1 text-xs text-text-1 focus:border-accent focus:outline-none"
        >
          <option value="waveform">Waveform</option>
          <option value="histogram">Histogramm</option>
        </select>
        <span className="ml-auto text-xs text-text-3">
          {mode === "waveform" ? "Luma" : "RGB"}
        </span>
      </div>

      <div className="relative min-h-0 flex-1 bg-black">
        {videoEl ? (
          <canvas ref={canvasRef} className="h-full w-full" />
        ) : (
          <div className="flex h-full flex-col items-center justify-center gap-2 px-4 text-center">
            <MonitorOff size={20} className="text-text-3" />
            <p className="text-xs text-text-3">
              Kein Bildsignal — Clip in den Programmmonitor laden.
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
