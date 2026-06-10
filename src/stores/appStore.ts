import { create } from "zustand";
import type { FfmpegInfo } from "@/core/types";
import type { WorkspaceId } from "@/core/workspace/types";
import { setContext } from "@/core/commands/context";

export type DialogId = "shortcuts" | "export" | "settings";

export type TimelineTool =
  | "select"
  | "razor"
  | "ripple"
  | "rolling"
  | "slip"
  | "slide"
  | "hand"
  | "zoom";

interface AppState {
  activeWorkspace: WorkspaceId;
  setActiveWorkspace: (id: WorkspaceId) => void;

  openDialog: DialogId | null;
  setOpenDialog: (dialog: DialogId | null) => void;

  commandPaletteOpen: boolean;
  setCommandPaletteOpen: (open: boolean) => void;

  activeTool: TimelineTool;
  setActiveTool: (tool: TimelineTool) => void;

  statusMessage: string | null;
  setStatusMessage: (message: string | null) => void;

  ffmpeg: FfmpegInfo | null;
  setFfmpeg: (info: FfmpegInfo | null) => void;
}

/** Anzeigedauer transienter Statusmeldungen, bevor die StatusBar wieder „Bereit“ zeigt. */
const STATUS_MESSAGE_TIMEOUT_MS = 6000;

let statusMessageTimer: ReturnType<typeof setTimeout> | null = null;

export const useAppStore = create<AppState>((set) => ({
  activeWorkspace: "edit",
  setActiveWorkspace: (id) => {
    setContext("workspace", id);
    set({ activeWorkspace: id });
  },

  openDialog: null,
  setOpenDialog: (dialog) => {
    setContext("dialogOpen", dialog !== null);
    set({ openDialog: dialog });
  },

  commandPaletteOpen: false,
  setCommandPaletteOpen: (open) => {
    setContext("commandPaletteOpen", open);
    set({ commandPaletteOpen: open });
  },

  activeTool: "select",
  setActiveTool: (tool) => {
    setContext("tool", tool);
    set({ activeTool: tool });
  },

  statusMessage: null,
  setStatusMessage: (message) => {
    if (statusMessageTimer !== null) {
      clearTimeout(statusMessageTimer);
      statusMessageTimer = null;
    }
    if (message !== null) {
      statusMessageTimer = setTimeout(() => {
        statusMessageTimer = null;
        set({ statusMessage: null });
      }, STATUS_MESSAGE_TIMEOUT_MS);
    }
    set({ statusMessage: message });
  },

  ffmpeg: null,
  setFfmpeg: (info) => set({ ffmpeg: info }),
}));
