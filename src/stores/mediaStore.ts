import { create } from "zustand";
import { open } from "@tauri-apps/plugin-dialog";
import type { MediaAsset, MediaKind, MediaInfo } from "@/core/types";
import { ipc } from "@/lib/ipc";
import { setContext } from "@/core/commands/context";
import { useAppStore } from "./appStore";

const VIDEO_EXT = ["mp4", "mov", "mkv", "webm", "avi", "m4v", "mts", "mxf"];
const AUDIO_EXT = ["wav", "mp3", "flac", "aac", "m4a", "ogg", "opus"];
const IMAGE_EXT = ["png", "jpg", "jpeg", "webp", "tif", "tiff", "bmp", "gif"];

function detectKind(path: string, info: MediaInfo): MediaKind {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  if (IMAGE_EXT.includes(ext)) return "image";
  if (info.video.length > 0) return "video";
  if (info.audio.length > 0) return "audio";
  return "video";
}

interface MediaState {
  assets: MediaAsset[];
  selectedAssetIds: string[];
  importing: boolean;

  importViaDialog: () => Promise<void>;
  importFiles: (paths: string[]) => Promise<void>;
  removeAssets: (ids: string[]) => void;
  select: (ids: string[]) => void;
}

export const useMediaStore = create<MediaState>((set, get) => ({
  assets: [],
  selectedAssetIds: [],
  importing: false,

  importViaDialog: async () => {
    const picked = await open({
      multiple: true,
      title: "Medien importieren",
      filters: [
        {
          name: "Medien",
          extensions: [...VIDEO_EXT, ...AUDIO_EXT, ...IMAGE_EXT],
        },
        { name: "Alle Dateien", extensions: ["*"] },
      ],
    });
    if (!picked) return;
    const paths = Array.isArray(picked) ? picked : [picked];
    await get().importFiles(paths);
  },

  importFiles: async (paths) => {
    if (paths.length === 0) return;
    const setStatus = useAppStore.getState().setStatusMessage;
    set({ importing: true });
    const errors: string[] = [];

    for (const path of paths) {
      if (get().assets.some((a) => a.path === path)) continue;
      try {
        const info = await ipc.probeMedia(path);
        const kind = detectKind(path, info);

        let thumbnailPath: string | null = null;
        if (kind !== "audio") {
          const t =
            kind === "image" ? 0 : Math.min(1, info.durationSec * 0.25);
          thumbnailPath = await ipc
            .generateThumbnail(path, t, 320)
            .catch(() => null);
        }

        const asset: MediaAsset = {
          id: crypto.randomUUID(),
          path,
          name: info.fileName,
          kind,
          info,
          thumbnailPath,
          importedAt: Date.now(),
        };
        set((s) => ({ assets: [...s.assets, asset] }));
      } catch (err) {
        console.error(`[media] Import fehlgeschlagen: ${path}`, err);
        errors.push(path.split("/").pop() ?? path);
      }
    }

    set({ importing: false });
    setStatus(
      errors.length > 0
        ? `Import: ${errors.length} Datei(en) fehlgeschlagen (${errors.join(", ")})`
        : null,
    );
  },

  removeAssets: (ids) =>
    set((s) => {
      const assets = s.assets.filter((a) => !ids.includes(a.id));
      const selectedAssetIds = s.selectedAssetIds.filter(
        (id) => !ids.includes(id),
      );
      setContext("mediaSelected", selectedAssetIds.length > 0);
      return { assets, selectedAssetIds };
    }),

  select: (ids) => {
    setContext("mediaSelected", ids.length > 0);
    set({ selectedAssetIds: ids });
  },
}));
