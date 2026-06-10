import { Info } from "lucide-react";
import { useMediaStore } from "@/stores/mediaStore";
import { usePlaybackStore } from "@/stores/playbackStore";
import { formatTimecode } from "@/lib/timecode";

function formatMegabytes(bytes: number): string {
  return `${(bytes / 1e6).toFixed(1)} MB`;
}

function formatVideoBitrate(bitrate: number | null): string {
  if (bitrate == null || bitrate <= 0) return "—";
  return `${(bitrate / 1e6).toFixed(1)} Mbit/s`;
}

function formatAudioBitrate(bitrate: number | null): string {
  if (bitrate == null || bitrate <= 0) return "—";
  return `${Math.round(bitrate / 1e3)} kbit/s`;
}

function formatFps(fps: number): string {
  return fps % 1 === 0 ? fps.toFixed(0) : fps.toFixed(2);
}

function formatSampleRate(sampleRate: number): string {
  const khz = sampleRate / 1000;
  return `${khz % 1 === 0 ? khz.toFixed(0) : khz.toFixed(1)} kHz`;
}

const TH_CLASS = "px-2 py-1 text-left font-normal text-text-3";
const TD_CLASS = "px-2 py-1 font-mono text-text-1";

export function InfoPanel() {
  const assets = useMediaStore((s) => s.assets);
  const selectedAssetIds = useMediaStore((s) => s.selectedAssetIds);
  const sourceAssetId = usePlaybackStore((s) => s.sourceAssetId);

  const asset =
    assets.find((a) => a.id === selectedAssetIds[0]) ??
    assets.find((a) => a.id === sourceAssetId) ??
    null;

  if (!asset) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-2 bg-surface-1">
        <Info size={20} className="text-text-3" />
        <p className="text-xs text-text-3">Clip auswählen</p>
      </div>
    );
  }

  const { info } = asset;
  const fps = info.video[0]?.fps;

  return (
    <div className="h-full overflow-y-auto bg-surface-1 p-3">
      <h2 className="text-sm font-medium text-text-1">{asset.name}</h2>
      <p className="mt-0.5 truncate text-xs text-text-3" title={asset.path}>
        {asset.path}
      </p>

      <div className="mt-3 grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
        <span className="text-text-2">Container</span>
        <span className="font-mono text-text-1">{info.container}</span>
        <span className="text-text-2">Dauer</span>
        <span className="font-mono text-text-1">
          {formatTimecode(info.durationSec, fps)}
        </span>
        <span className="text-text-2">Größe</span>
        <span className="font-mono text-text-1">
          {formatMegabytes(info.sizeBytes)}
        </span>
        <span className="text-text-2">Importiert</span>
        <span className="font-mono text-text-1">
          {new Date(asset.importedAt).toLocaleString("de-DE", {
            dateStyle: "medium",
            timeStyle: "short",
          })}
        </span>
      </div>

      {info.video.length > 0 && (
        <div className="mt-4">
          <h3 className="mb-1 text-xs font-medium text-text-2">
            Video-Streams
          </h3>
          <table className="w-full border-collapse text-xs">
            <thead>
              <tr className="border-b border-line">
                <th className={TH_CLASS}>Codec</th>
                <th className={TH_CLASS}>Auflösung</th>
                <th className={TH_CLASS}>fps</th>
                <th className={TH_CLASS}>Pixelformat</th>
                <th className={TH_CLASS}>Bitrate</th>
              </tr>
            </thead>
            <tbody>
              {info.video.map((v) => (
                <tr key={v.index} className="border-b border-line">
                  <td className={TD_CLASS}>{v.codec}</td>
                  <td className={TD_CLASS}>
                    {v.width}×{v.height}
                  </td>
                  <td className={TD_CLASS}>{formatFps(v.fps)}</td>
                  <td className={TD_CLASS}>{v.pixFmt ?? "—"}</td>
                  <td className={TD_CLASS}>{formatVideoBitrate(v.bitrate)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {info.audio.length > 0 && (
        <div className="mt-4">
          <h3 className="mb-1 text-xs font-medium text-text-2">
            Audio-Streams
          </h3>
          <table className="w-full border-collapse text-xs">
            <thead>
              <tr className="border-b border-line">
                <th className={TH_CLASS}>Codec</th>
                <th className={TH_CLASS}>Kanäle</th>
                <th className={TH_CLASS}>Samplerate</th>
                <th className={TH_CLASS}>Bitrate</th>
              </tr>
            </thead>
            <tbody>
              {info.audio.map((a) => (
                <tr key={a.index} className="border-b border-line">
                  <td className={TD_CLASS}>{a.codec}</td>
                  <td className={TD_CLASS}>{a.channels}</td>
                  <td className={TD_CLASS}>{formatSampleRate(a.sampleRate)}</td>
                  <td className={TD_CLASS}>{formatAudioBitrate(a.bitrate)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
