import { ipc } from "@/lib/ipc";

/**
 * Prozessweiter Cache für Waveform-Peaks (0..1) pro Mediendatei.
 * Fehlschläge werden als null gecacht, damit nicht wiederholt
 * erfolglos ffmpeg gestartet wird.
 */

const SAMPLES = 1200;

const cache = new Map<string, Promise<number[] | null>>();

export function getWaveform(path: string): Promise<number[] | null> {
  let entry = cache.get(path);
  if (!entry) {
    entry = ipc.extractWaveform(path, SAMPLES).catch((err) => {
      console.warn(`[waveform] Extraktion fehlgeschlagen: ${path}`, err);
      return null;
    });
    cache.set(path, entry);
  }
  return entry;
}
