import { useKeymapStore } from "@/core/keyboard/keymapStore";
import type { KeymapPreset } from "@/core/keyboard/types";
import { TOOL_LABELS } from "@/core/tools";
import { WORKSPACES } from "@/core/workspace/workspaces";
import { useAppStore } from "@/stores/appStore";
import { useMediaStore } from "@/stores/mediaStore";

/** Erwartete (defensive) Form des Keymap-Stores anderer Module. */
interface KeymapStoreShape {
  activePresetId?: string;
  presets?: KeymapPreset[] | Record<string, KeymapPreset>;
}

/** Name des aktiven Keymap-Presets — defensiv, falls der Store noch fehlt. */
function useActivePresetName(): string | null {
  return useKeymapStore((s: unknown): string | null => {
    try {
      const store = s as KeymapStoreShape;
      const presets = store.presets;
      const list: KeymapPreset[] = Array.isArray(presets)
        ? presets
        : presets && typeof presets === "object"
          ? Object.values(presets)
          : [];
      const active = list.find((p) => p?.id === store.activePresetId);
      return active?.name ?? store.activePresetId ?? null;
    } catch {
      return null;
    }
  });
}

function Divider() {
  return <span aria-hidden className="h-3 w-px shrink-0 bg-line-strong" />;
}

/**
 * Fußleiste: Status/Werkzeug/Workspace links, Medienanzahl,
 * Keymap-Preset und FFmpeg-Version rechts.
 */
export function StatusBar() {
  const statusMessage = useAppStore((s) => s.statusMessage);
  const activeTool = useAppStore((s) => s.activeTool);
  const activeWorkspace = useAppStore((s) => s.activeWorkspace);
  const ffmpeg = useAppStore((s) => s.ffmpeg);
  const assetCount = useMediaStore((s) => s.assets.length);
  const presetName = useActivePresetName();

  const workspaceName = WORKSPACES.find((ws) => ws.id === activeWorkspace)?.name ?? activeWorkspace;
  const toolLabel = TOOL_LABELS[activeTool] ?? activeTool;

  return (
    <footer className="flex h-6 shrink-0 items-center justify-between gap-4 border-t border-line bg-surface-0 px-3 text-xs text-text-3">
      <div className="flex min-w-0 items-center gap-2">
        <span className={statusMessage ? "truncate text-text-2" : "truncate"}>
          {statusMessage ?? "Bereit"}
        </span>
        <Divider />
        <span className="shrink-0">Werkzeug: {toolLabel}</span>
        <Divider />
        <span className="shrink-0">{workspaceName}</span>
      </div>

      <div className="flex shrink-0 items-center gap-2">
        <span>{assetCount === 1 ? "1 Medium" : `${assetCount} Medien`}</span>
        {presetName ? (
          <>
            <Divider />
            <span>Tastatur: {presetName}</span>
          </>
        ) : null}
        <Divider />
        {ffmpeg === null ? (
          <span>FFmpeg …</span>
        ) : ffmpeg.available ? (
          <span className="font-mono">FFmpeg {ffmpeg.version ?? "—"}</span>
        ) : (
          <span className="text-danger">FFmpeg fehlt</span>
        )}
      </div>
    </footer>
  );
}
