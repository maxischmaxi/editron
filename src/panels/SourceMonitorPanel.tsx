import { useMediaStore } from "@/stores/mediaStore";
import { usePlaybackStore } from "@/stores/playbackStore";
import { MonitorView } from "./MonitorView";

/**
 * Quellmonitor: zeigt das explizit geladene Asset (Doppelklick im
 * Medien-Browser, „In Quellmonitor laden“, Doppelklick auf einen
 * Timeline-Clip) — unabhängig vom Programmmonitor, der die Sequenz spielt.
 */
export function SourceMonitorPanel() {
  const sourceAssetId = usePlaybackStore((s) => s.sourceAssetId);
  const asset = useMediaStore((s) =>
    sourceAssetId
      ? (s.assets.find((a) => a.id === sourceAssetId) ?? null)
      : null,
  );

  return (
    <MonitorView
      asset={asset}
      emptyHint="Doppelklick im Medien-Browser lädt einen Clip in den Quellmonitor."
    />
  );
}
