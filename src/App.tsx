import { useEffect } from "react";
import { TitleBar } from "@/components/shell/TitleBar";
import { StatusBar } from "@/components/shell/StatusBar";
import { WorkspaceHost } from "@/components/shell/WorkspaceHost";
import { CommandPalette } from "@/components/keyboard/CommandPalette";
import { ShortcutEditorDialog } from "@/components/keyboard/ShortcutEditorDialog";
import { ExportDialog } from "@/components/dialogs/ExportDialog";
import { ContextMenuHost } from "@/components/ui/ContextMenu";
import { useKeyboardManager } from "@/core/keyboard/useKeyboardManager";
import { useAppStore } from "@/stores/appStore";
import { ipc } from "@/lib/ipc";

export default function App() {
  useKeyboardManager();

  const setFfmpeg = useAppStore((s) => s.setFfmpeg);
  const setStatusMessage = useAppStore((s) => s.setStatusMessage);

  useEffect(() => {
    ipc
      .ffmpegInfo()
      .then((info) => {
        setFfmpeg(info);
        if (!info.available) {
          setStatusMessage(
            "FFmpeg wurde nicht gefunden — Medienfunktionen sind deaktiviert.",
          );
        }
      })
      .catch((err) => console.error("[app] ffmpeg_info fehlgeschlagen:", err));
  }, [setFfmpeg, setStatusMessage]);

  return (
    <div className="flex h-full select-none flex-col bg-surface-0 text-text-1">
      <TitleBar />
      <div className="min-h-0 flex-1">
        <WorkspaceHost />
      </div>
      <StatusBar />

      <CommandPalette />
      <ShortcutEditorDialog />
      <ExportDialog />
      <ContextMenuHost />
    </div>
  );
}
