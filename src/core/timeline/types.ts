/**
 * Datenmodell der Schnitt-Sequenz: Spuren und Clips.
 * Alle Zeiten in Sekunden, Positionen relativ zum Sequenzanfang.
 */

export type TrackKind = "video" | "audio";

export interface TimelineTrack {
  id: string;
  kind: TrackKind;
  muted: boolean;
  solo: boolean;
  /** Gesperrte Spuren sind von allen Editier-Operationen ausgenommen. */
  locked: boolean;
}

export interface TimelineClip {
  id: string;
  trackId: string;
  assetId: string;
  /** Anzeigename (i. d. R. Dateiname, Audio-Teil mit Suffix). */
  name: string;
  kind: TrackKind;
  /** Startzeit in der Sequenz. */
  start: number;
  duration: number;
  /** In-Punkt innerhalb der Quelldatei. */
  srcIn: number;
  /** Gesamtdauer der Quelle; Infinity bei Standbildern (frei dehnbar). */
  srcDuration: number;
  /**
   * Verknüpfungsgruppe: Video- und Audio-Clip desselben Assets teilen
   * sich eine linkId und werden gemeinsam ausgewählt/bewegt/geschnitten.
   */
  linkId: string | null;
  /** Deaktivierte Clips bleiben in der Sequenz, sind aber ausgegraut. */
  enabled: boolean;
}

/** Framerate der Sequenz (für Playhead-Stepping und Timecode-Anzeige). */
export const SEQUENCE_FPS = 25;

/** Dauer, mit der Standbilder in die Timeline eingefügt werden. */
export const IMAGE_DEFAULT_DURATION = 5;

/** Kürzestmögliche Clipdauer (1 Frame). */
export const MIN_CLIP_DURATION = 1 / SEQUENCE_FPS;

/** Ende des letzten Clips — die effektive Sequenzdauer. */
export function sequenceEnd(clips: TimelineClip[]): number {
  let end = 0;
  for (const clip of clips) {
    end = Math.max(end, clip.start + clip.duration);
  }
  return end;
}

/** Anzeigename einer Spur (V1 unten im Video-Block, A1 oben im Audio-Block). */
export function trackName(
  track: TimelineTrack,
  tracks: TimelineTrack[],
): string {
  if (track.kind === "video") {
    const videos = tracks.filter((t) => t.kind === "video");
    const fromBottom = videos.length - videos.indexOf(track);
    return `V${fromBottom}`;
  }
  const audios = tracks.filter((t) => t.kind === "audio");
  return `A${audios.indexOf(track) + 1}`;
}
