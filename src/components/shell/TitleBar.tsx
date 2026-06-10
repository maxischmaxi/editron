import type { ComponentType } from "react";
import { FileOutput, Import, Keyboard, SquareTerminal } from "lucide-react";
import { commandRegistry } from "@/core/commands/registry";
import { formatKeys } from "@/core/keyboard/keys";
import { useKeymapStore } from "@/core/keyboard/keymapStore";
import type { Keybinding } from "@/core/keyboard/types";
import { WORKSPACES } from "@/core/workspace/workspaces";
import type { WorkspaceId } from "@/core/workspace/types";
import { useAppStore } from "@/stores/appStore";

/** Erwartete (defensive) Form des Keymap-Stores anderer Module. */
interface KeymapStoreShape {
  bindingsForCommand?: (commandId: string) => Keybinding[] | undefined;
}

/**
 * Erste Tastenkombination eines Commands, hübsch formatiert — oder null.
 * Defensiv, damit die TitleBar auch ohne fertiges Keymap-System rendert.
 */
function useShortcut(commandId: string): string | null {
  const keys = useKeymapStore((s: unknown): string | null => {
    try {
      const store = s as KeymapStoreShape;
      return store.bindingsForCommand?.(commandId)?.[0]?.keys ?? null;
    } catch {
      return null;
    }
  });
  if (!keys) return null;
  try {
    return formatKeys(keys);
  } catch {
    return null;
  }
}

function switchWorkspace(id: WorkspaceId): void {
  void commandRegistry.execute(`workspace.switch.${id}`).then((executed) => {
    // Fallback, falls die Builtin-Commands (noch) nicht registriert sind
    if (!executed) useAppStore.getState().setActiveWorkspace(id);
  });
}

function WorkspaceTabs() {
  const activeWorkspace = useAppStore((s) => s.activeWorkspace);
  const workspaces = [...WORKSPACES].sort((a, b) => a.order - b.order);

  return (
    <nav aria-label="Arbeitsbereiche" className="flex h-full items-stretch">
      {workspaces.map((ws) => {
        const active = ws.id === activeWorkspace;
        return (
          <button
            key={ws.id}
            type="button"
            aria-current={active ? "page" : undefined}
            onClick={() => switchWorkspace(ws.id)}
            className={
              active
                ? "relative px-3 text-xs font-medium text-text-1 after:absolute after:inset-x-2 after:bottom-0 after:h-0.5 after:rounded-full after:bg-accent"
                : "px-3 text-xs text-text-3 transition-colors hover:text-text-2"
            }
          >
            {ws.name}
          </button>
        );
      })}
    </nav>
  );
}

interface ActionButtonProps {
  commandId: string;
  label: string;
  icon: ComponentType<{ className?: string }>;
}

function ActionButton({ commandId, label, icon: Icon }: ActionButtonProps) {
  const shortcut = useShortcut(commandId);
  return (
    <button
      type="button"
      title={shortcut ? `${label} (${shortcut})` : label}
      aria-label={label}
      onClick={() => void commandRegistry.execute(commandId)}
      className="flex size-7 items-center justify-center rounded-sm text-text-2 transition-colors hover:bg-surface-2 hover:text-text-1"
    >
      <Icon className="size-4" />
    </button>
  );
}

function FfmpegStatus() {
  const ffmpeg = useAppStore((s) => s.ffmpeg);

  let dotClass = "bg-surface-4";
  let tooltip = "FFmpeg wird gesucht …";
  if (ffmpeg !== null) {
    if (ffmpeg.available) {
      dotClass = "bg-success";
      tooltip = `FFmpeg ${ffmpeg.version ?? "(Version unbekannt)"}${
        ffmpeg.ffmpegPath ? ` — ${ffmpeg.ffmpegPath}` : ""
      }`;
    } else {
      dotClass = "bg-danger";
      tooltip = "FFmpeg wurde nicht gefunden — Medienfunktionen sind deaktiviert.";
    }
  }

  return (
    <div className="flex cursor-default items-center gap-1.5 px-2" title={tooltip}>
      <span aria-hidden className={`size-2 rounded-full ${dotClass}`} />
      <span className="text-xs text-text-3">FFmpeg</span>
    </div>
  );
}

/**
 * Kopfleiste der App: Logo links, Workspace-Leiste in der Mitte,
 * globale Aktionen + FFmpeg-Status rechts.
 */
export function TitleBar() {
  return (
    <header className="flex h-10 shrink-0 items-stretch border-b border-line bg-surface-0">
      <div className="flex flex-1 items-center gap-2 pl-3">
        <div
          aria-hidden
          className="flex size-5 items-center justify-center rounded-sm bg-accent text-xs font-bold text-white"
        >
          E
        </div>
        <span className="text-sm font-semibold text-text-1">Editron</span>
      </div>

      <WorkspaceTabs />

      <div className="flex flex-1 items-center justify-end gap-1 pr-3">
        <ActionButton commandId="media.import" label="Medien importieren" icon={Import} />
        <ActionButton commandId="app.export" label="Exportieren" icon={FileOutput} />
        <ActionButton commandId="app.shortcutEditor" label="Tastaturbefehle" icon={Keyboard} />
        <ActionButton commandId="app.commandPalette" label="Befehlspalette" icon={SquareTerminal} />
        <div aria-hidden className="mx-1 h-4 w-px bg-line" />
        <FfmpegStatus />
      </div>
    </header>
  );
}
