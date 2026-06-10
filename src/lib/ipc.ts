import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  FfmpegInfo,
  MediaInfo,
  TranscodeDone,
  TranscodeOptions,
  TranscodeProgress,
} from "@/core/types";

/**
 * Typisierte Wrapper um die Tauri-Commands des Rust-Backends.
 * Command-Namen und Payload-Formen sind der Vertrag mit src-tauri.
 */
export const ipc = {
  ffmpegInfo: () => invoke<FfmpegInfo>("ffmpeg_info"),

  probeMedia: (path: string) => invoke<MediaInfo>("probe_media", { path }),

  /** Erzeugt ein Thumbnail im App-Cache und liefert dessen Pfad. */
  generateThumbnail: (path: string, timeSec: number, maxWidth: number) =>
    invoke<string>("generate_thumbnail", { path, timeSec, maxWidth }),

  /** Liefert normalisierte Waveform-Peaks (0..1). */
  extractWaveform: (path: string, samples: number) =>
    invoke<number[]>("extract_waveform", { path, samples }),

  startTranscode: (options: TranscodeOptions) =>
    invoke<string>("start_transcode", { options }),

  cancelJob: (jobId: string) => invoke<void>("cancel_job", { jobId }),

  /** Zeigt die Datei im Dateimanager des Systems an. */
  revealInFileManager: (path: string) =>
    invoke<void>("reveal_in_file_manager", { path }),

  onTranscodeProgress: (
    cb: (p: TranscodeProgress) => void,
  ): Promise<UnlistenFn> =>
    listen<TranscodeProgress>("transcode://progress", (e) => cb(e.payload)),

  onTranscodeDone: (cb: (d: TranscodeDone) => void): Promise<UnlistenFn> =>
    listen<TranscodeDone>("transcode://done", (e) => cb(e.payload)),
};

/** Wandelt einen lokalen Dateipfad in eine im Webview ladbare asset:-URL um. */
export function mediaSrc(path: string): string {
  return convertFileSrc(path);
}
