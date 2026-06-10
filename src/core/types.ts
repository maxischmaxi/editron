/**
 * Gemeinsame Medien- und IPC-Typen.
 * Müssen 1:1 zu den Rust-Strukturen in src-tauri passen
 * (serde rename_all = "camelCase").
 */

export interface FfmpegInfo {
  available: boolean;
  version: string | null;
  ffmpegPath: string | null;
  ffprobePath: string | null;
}

export interface VideoStreamInfo {
  index: number;
  codec: string;
  width: number;
  height: number;
  fps: number;
  pixFmt: string | null;
  bitrate: number | null;
}

export interface AudioStreamInfo {
  index: number;
  codec: string;
  channels: number;
  sampleRate: number;
  bitrate: number | null;
}

export interface MediaInfo {
  path: string;
  fileName: string;
  container: string;
  durationSec: number;
  sizeBytes: number;
  video: VideoStreamInfo[];
  audio: AudioStreamInfo[];
}

export type MediaKind = "video" | "audio" | "image";

export interface MediaAsset {
  id: string;
  path: string;
  name: string;
  kind: MediaKind;
  info: MediaInfo;
  /** Pfad zu einem generierten Vorschaubild im App-Cache, falls vorhanden */
  thumbnailPath: string | null;
  importedAt: number;
}

export interface TranscodeOptions {
  input: string;
  output: string;
  videoCodec?: string;
  audioCodec?: string;
  crf?: number;
  preset?: string;
  width?: number;
  height?: number;
}

export interface TranscodeProgress {
  jobId: string;
  outTimeSec: number;
  fps: number | null;
  speed: number | null;
  /** 0..100, null wenn die Quelldauer unbekannt ist */
  progressPct: number | null;
}

export interface TranscodeDone {
  jobId: string;
  ok: boolean;
  error: string | null;
}
