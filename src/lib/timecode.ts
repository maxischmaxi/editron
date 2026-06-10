/**
 * Timecode-Formatierung für Monitore, Timeline und Medien-Browser.
 */

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/**
 * Formatiert Sekunden als SMPTE-artigen Timecode "HH:MM:SS:FF".
 * Nicht-ganzzahlige Frameraten (23.976, 29.97 …) werden non-drop behandelt.
 */
export function formatTimecode(seconds: number, fps = 25): string {
  const safeFps = Number.isFinite(fps) && fps > 0 ? fps : 25;
  const total = Number.isFinite(seconds) && seconds > 0 ? seconds : 0;

  const whole = Math.floor(total);
  const maxFrame = Math.max(1, Math.round(safeFps)) - 1;
  const frames = Math.min(
    maxFrame,
    Math.floor((total - whole) * safeFps + 1e-6),
  );

  const h = Math.floor(whole / 3600);
  const m = Math.floor((whole % 3600) / 60);
  const s = whole % 60;
  return `${pad2(h)}:${pad2(m)}:${pad2(s)}:${pad2(frames)}`;
}

/**
 * Kompakte Dauer-Anzeige: "M:SS", ab einer Stunde "H:MM:SS".
 */
export function formatDuration(seconds: number): string {
  const total =
    Number.isFinite(seconds) && seconds > 0 ? Math.round(seconds) : 0;
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return h > 0 ? `${h}:${pad2(m)}:${pad2(s)}` : `${m}:${pad2(s)}`;
}
