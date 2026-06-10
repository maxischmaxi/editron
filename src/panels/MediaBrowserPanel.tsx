import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type ComponentType,
  type DragEvent,
  type MouseEvent,
} from "react";
import {
  Clapperboard,
  Film,
  FolderSearch,
  ImageIcon,
  Import,
  Info,
  LayoutGrid,
  List,
  ListVideo,
  LoaderCircle,
  MonitorPlay,
  Music,
  Search,
  Trash2,
} from "lucide-react";
import type { MediaAsset, MediaKind } from "@/core/types";
import { commandRegistry } from "@/core/commands/registry";
import { ASSET_DRAG_MIME, clearDraggedAssets, setDraggedAssets } from "@/core/dnd";
import { openPanel } from "@/core/workspace/layoutManager";
import {
  commandItem,
  openContextMenu,
  type MenuEntry,
} from "@/components/ui/ContextMenu";
import { mediaSrc } from "@/lib/ipc";
import { formatDuration, formatTimecode } from "@/lib/timecode";
import { useMediaStore } from "@/stores/mediaStore";

type ViewMode = "grid" | "list";

const KIND_ICONS: Record<MediaKind, ComponentType<{ className?: string }>> = {
  video: Film,
  audio: Music,
  image: ImageIcon,
};

function resolutionLabel(asset: MediaAsset): string {
  const v = asset.info.video[0];
  return v ? `${v.width}×${v.height}` : "—";
}

function codecLabel(asset: MediaAsset): string {
  return asset.info.video[0]?.codec ?? asset.info.audio[0]?.codec ?? "—";
}

/**
 * Hover-Vorschau: solange die Maus über der Kachel liegt, läuft das Video
 * stumm in Schleife, damit man grob erkennt, was im Clip passiert.
 * Bis zum ersten gemalten Frame bleibt das Thumbnail darunter sichtbar
 * (WebKitGTK malt pausierte Seek-Frames unzuverlässig — echtes Abspielen
 * umgeht das). Timecode/Fortschritt aktualisieren bewusst nur grob über
 * timeupdate (~4 Hz).
 */
function HoverVideoPreview({ asset }: { asset: MediaAsset }) {
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const [time, setTime] = useState(0);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    const el = videoRef.current;
    if (!el) return;
    const onTimeUpdate = () => setTime(el.currentTime);
    const onPlaying = () => setReady(true);
    const onError = () => {
      const err = el.error;
      console.warn(
        `[media] Hover-Vorschau-Fehler (Code ${err?.code ?? "?"}): ${
          err?.message || "unbekannt"
        } — fehlen GStreamer-Codecs (gst-libav) oder ist der Pfad außerhalb des Asset-Scopes?`,
      );
    };
    el.addEventListener("timeupdate", onTimeUpdate);
    el.addEventListener("playing", onPlaying);
    el.addEventListener("error", onError);
    // muted vor play() auch imperativ setzen — Autoplay-Policies erlauben
    // nur stumme Wiedergabe ohne Nutzergeste.
    el.muted = true;
    el.play().catch((err) => {
      console.warn("[media] Hover-Vorschau konnte nicht starten:", err);
    });
    return () => {
      el.removeEventListener("timeupdate", onTimeUpdate);
      el.removeEventListener("playing", onPlaying);
      el.removeEventListener("error", onError);
      el.pause();
    };
  }, [asset.id]);

  const duration = asset.info.durationSec;
  const fps = asset.info.video[0]?.fps || 25;

  return (
    <div className="pointer-events-none absolute inset-0">
      <video
        ref={videoRef}
        src={mediaSrc(asset.path)}
        muted
        loop
        autoPlay
        playsInline
        preload="auto"
        className={`h-full w-full bg-black object-cover ${ready ? "" : "opacity-0"}`}
      />
      {ready && (
        <>
          <span className="absolute bottom-1 right-1 rounded bg-black/70 px-1 font-mono text-xs text-text-1">
            {formatTimecode(time, fps)}
          </span>
          <div className="absolute inset-x-0 bottom-0 h-0.5 bg-black/50">
            <div
              className="h-full bg-accent"
              style={{
                width: `${duration > 0 ? Math.min(time / duration, 1) * 100 : 0}%`,
              }}
            />
          </div>
        </>
      )}
    </div>
  );
}

/**
 * Medien-Browser: Import, Suche, Grid-/Listenansicht, Mehrfachauswahl,
 * Hover-Vorschau, Drag-Quelle für die Timeline und Kontextmenü.
 * Doppelklick lädt das Asset in den Quellmonitor.
 */
export function MediaBrowserPanel() {
  const assets = useMediaStore((s) => s.assets);
  const selectedAssetIds = useMediaStore((s) => s.selectedAssetIds);
  const select = useMediaStore((s) => s.select);
  const importing = useMediaStore((s) => s.importing);

  const openInSource = (assetId: string) => {
    void commandRegistry.execute("media.openInSource", { assetId });
  };

  const [query, setQuery] = useState("");
  const [view, setView] = useState<ViewMode>("grid");
  const [previewAssetId, setPreviewAssetId] = useState<string | null>(null);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return assets;
    return assets.filter((a) => a.name.toLowerCase().includes(q));
  }, [assets, query]);

  const runImport = () => {
    void commandRegistry.execute("media.import");
  };

  const handleSelect = (e: MouseEvent, id: string) => {
    if (e.metaKey || e.ctrlKey) {
      select(
        selectedAssetIds.includes(id)
          ? selectedAssetIds.filter((x) => x !== id)
          : [...selectedAssetIds, id],
      );
    } else {
      select([id]);
    }
  };

  /** Rechtsklick: Asset (falls nötig) selektieren und Menü öffnen. */
  const openAssetMenu = (e: MouseEvent, asset: MediaAsset) => {
    if (!selectedAssetIds.includes(asset.id)) select([asset.id]);
    const items: MenuEntry[] = [
      commandItem("media.openInSource", {
        icon: MonitorPlay,
        args: { assetId: asset.id },
      }),
      { type: "separator" },
      commandItem("media.addSelectionToTimeline", { icon: ListVideo }),
      { type: "separator" },
      commandItem("media.revealInFileManager", {
        icon: FolderSearch,
        args: { assetId: asset.id },
      }),
      {
        label: "Eigenschaften",
        icon: Info,
        onSelect: () => openPanel("info"),
      },
      { type: "separator" },
      commandItem("media.removeSelected", { icon: Trash2, danger: true }),
    ];
    openContextMenu(e, items);
  };

  const openBackgroundMenu = (e: MouseEvent) => {
    openContextMenu(e, [
      commandItem("media.import", { icon: Import }),
      { type: "separator" },
      {
        label: view === "grid" ? "Listenansicht" : "Rasteransicht",
        icon: view === "grid" ? List : LayoutGrid,
        onSelect: () => setView(view === "grid" ? "list" : "grid"),
      },
    ]);
  };

  const onAssetDragStart = (e: DragEvent, asset: MediaAsset) => {
    const ids = selectedAssetIds.includes(asset.id)
      ? selectedAssetIds
      : [asset.id];
    if (!selectedAssetIds.includes(asset.id)) select([asset.id]);
    e.dataTransfer.setData(ASSET_DRAG_MIME, JSON.stringify(ids));
    e.dataTransfer.effectAllowed = "copy";
    setDraggedAssets(ids);
    setPreviewAssetId(null);
  };

  return (
    <div className="flex h-full flex-col bg-surface-1">
      {/* Toolbar */}
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-line px-2">
        <button
          type="button"
          title="Medien importieren"
          onClick={runImport}
          className="flex h-6 shrink-0 items-center gap-1.5 rounded bg-surface-3 px-2 text-xs text-text-1 hover:bg-surface-4"
        >
          <Import className="size-3.5" />
          Importieren
        </button>
        {importing && (
          <LoaderCircle
            className="size-4 shrink-0 animate-spin text-accent"
            aria-label="Import läuft"
          />
        )}
        <div className="relative ml-auto w-44 min-w-0">
          <Search className="pointer-events-none absolute left-1.5 top-1/2 size-3.5 -translate-y-1/2 text-text-3" />
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Suchen …"
            className="h-6 w-full rounded border border-line bg-surface-2 pl-6 pr-2 text-xs text-text-1 placeholder:text-text-3"
          />
        </div>
        <div className="flex shrink-0 overflow-hidden rounded border border-line">
          <button
            type="button"
            title="Rasteransicht"
            onClick={() => setView("grid")}
            className={`flex size-6 items-center justify-center ${
              view === "grid"
                ? "bg-surface-3 text-accent"
                : "text-text-3 hover:bg-surface-2 hover:text-text-1"
            }`}
          >
            <LayoutGrid className="size-3.5" />
          </button>
          <button
            type="button"
            title="Listenansicht"
            onClick={() => setView("list")}
            className={`flex size-6 items-center justify-center border-l border-line ${
              view === "list"
                ? "bg-surface-3 text-accent"
                : "text-text-3 hover:bg-surface-2 hover:text-text-1"
            }`}
          >
            <List className="size-3.5" />
          </button>
        </div>
      </div>

      {/* Inhalt */}
      {assets.length === 0 ? (
        <div
          className="flex flex-1 flex-col items-center justify-center gap-3 p-4"
          onContextMenu={openBackgroundMenu}
        >
          <Clapperboard className="size-10 text-text-3" />
          <p className="text-sm text-text-2">Noch keine Medien importiert</p>
          <button
            type="button"
            onClick={runImport}
            className="flex h-9 items-center gap-2 rounded bg-accent px-4 text-sm font-medium text-white hover:bg-accent-hover"
          >
            <Import className="size-4" />
            Medien importieren
          </button>
        </div>
      ) : filtered.length === 0 ? (
        <div
          className="flex flex-1 items-center justify-center p-4"
          onContextMenu={openBackgroundMenu}
        >
          <p className="text-xs text-text-3">
            Keine Treffer für „{query.trim()}“
          </p>
        </div>
      ) : view === "grid" ? (
        <div
          className="grid flex-1 content-start gap-2 overflow-y-auto p-2 grid-cols-[repeat(auto-fill,minmax(8.5rem,1fr))]"
          onContextMenu={openBackgroundMenu}
        >
          {filtered.map((asset) => {
            const Kind = KIND_ICONS[asset.kind];
            const selected = selectedAssetIds.includes(asset.id);
            const previewing = previewAssetId === asset.id;
            return (
              <button
                key={asset.id}
                type="button"
                title={asset.name}
                draggable
                onDragStart={(e) => onAssetDragStart(e, asset)}
                onDragEnd={clearDraggedAssets}
                onClick={(e) => handleSelect(e, asset.id)}
                onDoubleClick={() => openInSource(asset.id)}
                onContextMenu={(e) => openAssetMenu(e, asset)}
                className={`group overflow-hidden rounded border bg-surface-2 text-left ${
                  selected
                    ? "border-transparent ring-2 ring-accent"
                    : "border-line hover:border-line-strong"
                }`}
              >
                <div
                  className="relative aspect-video w-full bg-surface-2"
                  onMouseEnter={
                    asset.kind === "video"
                      ? () => setPreviewAssetId(asset.id)
                      : undefined
                  }
                  onMouseLeave={() =>
                    setPreviewAssetId((id) => (id === asset.id ? null : id))
                  }
                >
                  {asset.thumbnailPath ? (
                    <img
                      src={mediaSrc(asset.thumbnailPath)}
                      alt=""
                      className="h-full w-full object-cover"
                      draggable={false}
                    />
                  ) : (
                    <div className="flex h-full w-full items-center justify-center">
                      <Kind className="size-8 text-text-3" />
                    </div>
                  )}
                  {previewing && <HoverVideoPreview asset={asset} />}
                  <span className="absolute left-1 top-1 flex size-5 items-center justify-center rounded bg-black/60 text-text-2">
                    <Kind className="size-3" />
                  </span>
                  {asset.kind !== "image" && !previewing && (
                    <span className="absolute bottom-1 right-1 rounded bg-black/70 px-1 font-mono text-xs text-text-1">
                      {formatDuration(asset.info.durationSec)}
                    </span>
                  )}
                </div>
                <p className="truncate px-1.5 py-1 text-xs text-text-1">
                  {asset.name}
                </p>
              </button>
            );
          })}
        </div>
      ) : (
        <div className="flex-1 overflow-y-auto" onContextMenu={openBackgroundMenu}>
          <div className="sticky top-0 z-1 flex h-7 items-center gap-2 border-b border-line bg-surface-1 px-2 text-xs text-text-3">
            <span className="min-w-0 flex-1">Name</span>
            <span className="w-14 shrink-0 text-right">Dauer</span>
            <span className="w-24 shrink-0 text-right">Auflösung</span>
            <span className="w-20 shrink-0 text-right">Codec</span>
          </div>
          {filtered.map((asset) => {
            const Kind = KIND_ICONS[asset.kind];
            const selected = selectedAssetIds.includes(asset.id);
            return (
              <button
                key={asset.id}
                type="button"
                draggable
                onDragStart={(e) => onAssetDragStart(e, asset)}
                onDragEnd={clearDraggedAssets}
                onClick={(e) => handleSelect(e, asset.id)}
                onDoubleClick={() => openInSource(asset.id)}
                onContextMenu={(e) => openAssetMenu(e, asset)}
                className={`flex h-7 w-full items-center gap-2 px-2 text-left text-xs ${
                  selected
                    ? "bg-accent-soft text-text-1"
                    : "text-text-2 hover:bg-surface-2"
                }`}
              >
                <span className="flex min-w-0 flex-1 items-center gap-2">
                  <Kind className="size-3.5 shrink-0 text-text-3" />
                  <span className="truncate text-text-1" title={asset.name}>
                    {asset.name}
                  </span>
                </span>
                <span className="w-14 shrink-0 text-right font-mono">
                  {asset.kind === "image"
                    ? "—"
                    : formatDuration(asset.info.durationSec)}
                </span>
                <span className="w-24 shrink-0 text-right font-mono">
                  {resolutionLabel(asset)}
                </span>
                <span className="w-20 shrink-0 truncate text-right font-mono">
                  {codecLabel(asset)}
                </span>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
