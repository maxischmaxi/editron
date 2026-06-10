import { commandRegistry } from "./registry";
import type { Command } from "./types";
import { useAppStore, type TimelineTool } from "@/stores/appStore";
import { TOOL_COMMAND_TITLES } from "@/core/tools";
import { useMediaStore } from "@/stores/mediaStore";
import { activePlayback, usePlaybackStore } from "@/stores/playbackStore";
import { allPanels } from "@/core/workspace/panelRegistry";
import type { WorkspaceId } from "@/core/workspace/types";
import {
  resetCurrentWorkspaceLayout,
  openPanel,
} from "@/core/workspace/layoutManager";
import { useTimelineStore } from "@/core/timeline/timelineStore";
import { ipc } from "@/lib/ipc";

/**
 * Registriert alle Basis-Commands von Editron. Jede Funktion läuft als
 * Command durch die Registry und ist damit über Shortcuts, die
 * Befehlspalette, Kontextmenüs und künftige Menüs erreichbar.
 *
 * Alle Handler greifen über getState() auf die Stores zu (außerhalb React).
 */

const WORKSPACES: { id: WorkspaceId; name: string }[] = [
  { id: "media", name: "Medien" },
  { id: "edit", name: "Schnitt" },
  { id: "color", name: "Farbe" },
  { id: "effects", name: "Effekte" },
  { id: "audio", name: "Audio" },
  { id: "graphics", name: "Grafik" },
];

/** Werkzeug-IDs in Anzeige-Reihenfolge; Titel kommen aus TOOL_COMMAND_TITLES. */
const TOOLS = Object.keys(TOOL_COMMAND_TITLES) as TimelineTool[];

/**
 * Wiedergabe-Commands wirken auf den aktiven Monitor: Fokus auf dem
 * Quellmonitor steuert die Quelle, sonst den Programmmonitor (Timeline).
 * Die JKL-Verdopplungslogik lebt in den Controllern selbst.
 */
function stepFrames(frames: number): void {
  activePlayback()?.stepFrames(frames);
}

function cycleWorkspace(offset: number): void {
  const { activeWorkspace, setActiveWorkspace } = useAppStore.getState();
  const index = WORKSPACES.findIndex((w) => w.id === activeWorkspace);
  const next =
    WORKSPACES[(index + offset + WORKSPACES.length) % WORKSPACES.length];
  setActiveWorkspace(next.id);
}

/** Erstes ausgewähltes Asset im Medien-Browser (oder per args übergeben). */
function targetAssetId(args?: unknown): string | null {
  if (
    typeof args === "object" &&
    args !== null &&
    typeof (args as { assetId?: unknown }).assetId === "string"
  ) {
    return (args as { assetId: string }).assetId;
  }
  return useMediaStore.getState().selectedAssetIds[0] ?? null;
}

function buildCommands(): Command[] {
  const commands: Command[] = [
    // ----------------------------------------------------------- Anwendung
    {
      id: "app.commandPalette",
      title: "Befehlspalette öffnen",
      category: "Anwendung",
      run: () => useAppStore.getState().setCommandPaletteOpen(true),
    },
    {
      id: "app.shortcutEditor",
      title: "Tastaturbefehle…",
      category: "Anwendung",
      run: () => useAppStore.getState().setOpenDialog("shortcuts"),
    },
    {
      id: "app.export",
      title: "Exportieren…",
      category: "Anwendung",
      run: () => useAppStore.getState().setOpenDialog("export"),
    },
    {
      id: "app.settings",
      title: "Einstellungen…",
      category: "Anwendung",
      run: () => useAppStore.getState().setStatusMessage("Einstellungen folgen"),
    },

    // ----------------------------------------------------------- Bearbeiten
    {
      id: "edit.undo",
      title: "Rückgängig",
      category: "Bearbeiten",
      when: "timelineCanUndo",
      allowRepeat: true,
      run: () => useTimelineStore.getState().undo(),
    },
    {
      id: "edit.redo",
      title: "Wiederholen",
      category: "Bearbeiten",
      when: "timelineCanRedo",
      allowRepeat: true,
      run: () => useTimelineStore.getState().redo(),
    },

    // -------------------------------------------------------------- Medien
    {
      id: "media.import",
      title: "Medien importieren…",
      category: "Medien",
      run: () => useMediaStore.getState().importViaDialog(),
    },
    {
      id: "media.removeSelected",
      title: "Ausgewählte Medien entfernen",
      category: "Medien",
      when: "mediaSelected",
      run: () => {
        const { selectedAssetIds, removeAssets } = useMediaStore.getState();
        if (selectedAssetIds.length === 0) return;
        // Clips in der Timeline, die auf die Assets zeigen, mit entfernen.
        useTimelineStore.getState().removeClipsForAssets(selectedAssetIds);
        const pb = usePlaybackStore.getState();
        if (pb.sourceAssetId && selectedAssetIds.includes(pb.sourceAssetId)) {
          pb.setSourceAsset(null);
        }
        removeAssets(selectedAssetIds);
      },
    },
    {
      id: "media.addSelectionToTimeline",
      title: "Auswahl an Playhead in Timeline einfügen",
      category: "Medien",
      when: "mediaSelected",
      run: () => {
        const { selectedAssetIds } = useMediaStore.getState();
        if (selectedAssetIds.length === 0) return;
        const timeline = useTimelineStore.getState();
        timeline.insertAssets(selectedAssetIds, { at: timeline.playheadSec });
        // Playhead ans Ende des Eingefügten (Overwrite-Konvention).
        const after = useTimelineStore.getState();
        const inserted = after.clips.filter((c) =>
          after.selectedClipIds.includes(c.id),
        );
        if (inserted.length > 0) {
          after.setPlayhead(
            Math.max(...inserted.map((c) => c.start + c.duration)),
          );
        }
        openPanel("timeline");
      },
    },
    {
      id: "media.openInSource",
      title: "In Quellmonitor laden",
      category: "Medien",
      when: "mediaSelected",
      run: (args) => {
        const assetId = targetAssetId(args);
        if (!assetId) return;
        usePlaybackStore.getState().setSourceAsset(assetId);
        openPanel("source");
      },
    },
    {
      id: "media.revealInFileManager",
      title: "Im Dateimanager anzeigen",
      category: "Medien",
      when: "mediaSelected",
      run: async (args) => {
        const assetId = targetAssetId(args);
        const asset = useMediaStore
          .getState()
          .assets.find((a) => a.id === assetId);
        if (!asset) return;
        try {
          await ipc.revealInFileManager(asset.path);
        } catch (err) {
          console.error("[media] Dateimanager öffnen fehlgeschlagen:", err);
          useAppStore
            .getState()
            .setStatusMessage("Dateimanager konnte nicht geöffnet werden");
        }
      },
    },

    // ----------------------------------------------------------- Workspace
    ...WORKSPACES.map(
      (workspace): Command => ({
        id: `workspace.switch.${workspace.id}`,
        title: `Workspace: ${workspace.name}`,
        category: "Workspace",
        run: () => useAppStore.getState().setActiveWorkspace(workspace.id),
      }),
    ),
    {
      id: "workspace.next",
      title: "Nächster Workspace",
      category: "Workspace",
      run: () => cycleWorkspace(1),
    },
    {
      id: "workspace.previous",
      title: "Vorheriger Workspace",
      category: "Workspace",
      run: () => cycleWorkspace(-1),
    },
    {
      id: "workspace.resetLayout",
      title: "Layout zurücksetzen",
      category: "Workspace",
      run: () => resetCurrentWorkspaceLayout(),
    },

    // ---------------------------------------------------------- Wiedergabe
    {
      id: "playback.togglePlay",
      title: "Wiedergabe starten/pausieren",
      category: "Wiedergabe",
      run: () => activePlayback()?.toggle(),
    },
    {
      id: "playback.shuttleForward",
      title: "Shuttle vorwärts",
      category: "Wiedergabe",
      description: "Je Druck doppelte Geschwindigkeit, maximal 8×.",
      run: () => activePlayback()?.shuttle(1),
    },
    {
      id: "playback.shuttleReverse",
      title: "Shuttle rückwärts",
      category: "Wiedergabe",
      description: "Je Druck doppelte Geschwindigkeit rückwärts, maximal 8×.",
      run: () => activePlayback()?.shuttle(-1),
    },
    {
      id: "playback.shuttleStop",
      title: "Shuttle stoppen",
      category: "Wiedergabe",
      run: () => activePlayback()?.pause(),
    },
    {
      id: "playback.stepForward",
      title: "Ein Frame vorwärts",
      category: "Wiedergabe",
      allowRepeat: true,
      run: () => stepFrames(1),
    },
    {
      id: "playback.stepBackward",
      title: "Ein Frame zurück",
      category: "Wiedergabe",
      allowRepeat: true,
      run: () => stepFrames(-1),
    },
    {
      id: "playback.stepForward5",
      title: "Fünf Frames vorwärts",
      category: "Wiedergabe",
      allowRepeat: true,
      run: () => stepFrames(5),
    },
    {
      id: "playback.stepBackward5",
      title: "Fünf Frames zurück",
      category: "Wiedergabe",
      allowRepeat: true,
      run: () => stepFrames(-5),
    },
    {
      id: "playback.goToStart",
      title: "Zum Anfang",
      category: "Wiedergabe",
      run: () => activePlayback()?.goToStart(),
    },
    {
      id: "playback.goToEnd",
      title: "Zum Ende",
      category: "Wiedergabe",
      run: () => activePlayback()?.goToEnd(),
    },
    {
      id: "playback.setInPoint",
      title: "In-Punkt setzen",
      category: "Wiedergabe",
      description:
        "Im Programmmonitor: Anfang des Loop-Bereichs der Sequenz.",
      run: () => activePlayback()?.markIn(),
    },
    {
      id: "playback.setOutPoint",
      title: "Out-Punkt setzen",
      category: "Wiedergabe",
      description: "Im Programmmonitor: Ende des Loop-Bereichs der Sequenz.",
      run: () => activePlayback()?.markOut(),
    },
    {
      id: "playback.clearInOut",
      title: "In- und Out-Punkt löschen",
      category: "Wiedergabe",
      run: () => activePlayback()?.clearMarks(),
    },

    // ----------------------------------------------------------- Werkzeuge
    ...TOOLS.map(
      (tool): Command => ({
        id: `tools.${tool}`,
        title: TOOL_COMMAND_TITLES[tool],
        category: "Werkzeuge",
        run: () => useAppStore.getState().setActiveTool(tool),
      }),
    ),

    // ------------------------------------------------- Timeline: Ansicht
    {
      id: "timeline.zoomIn",
      title: "Timeline vergrößern",
      category: "Timeline",
      allowRepeat: true,
      run: () => useTimelineStore.getState().zoomIn(),
    },
    {
      id: "timeline.zoomOut",
      title: "Timeline verkleinern",
      category: "Timeline",
      allowRepeat: true,
      run: () => useTimelineStore.getState().zoomOut(),
    },
    {
      id: "timeline.zoomFit",
      title: "Timeline an Sequenz anpassen",
      category: "Timeline",
      when: "timelineHasClips",
      run: () => useTimelineStore.getState().zoomToFit(),
    },
    {
      id: "timeline.toggleSnapping",
      title: "Einrasten umschalten",
      category: "Timeline",
      run: () => {
        const s = useTimelineStore.getState();
        s.toggleSnapping();
        useAppStore
          .getState()
          .setStatusMessage(
            useTimelineStore.getState().snapping
              ? "Einrasten aktiviert"
              : "Einrasten deaktiviert",
          );
      },
    },
    {
      id: "timeline.addMarker",
      title: "Marker hinzufügen",
      category: "Timeline",
      run: () =>
        useAppStore
          .getState()
          .setStatusMessage("Marker folgen in einer späteren Version"),
    },

    // -------------------------------------------- Timeline: Bearbeitung
    {
      id: "timeline.splitAtPlayhead",
      title: "Am Playhead schneiden",
      category: "Timeline",
      description:
        "Schneidet ausgewählte Clips am Playhead; ohne Auswahl alle Clips unter dem Playhead.",
      when: "timelineHasClips",
      run: () => {
        const s = useTimelineStore.getState();
        s.splitAt(
          s.playheadSec,
          s.selectedClipIds.length > 0 ? s.selectedClipIds : undefined,
        );
      },
    },
    {
      id: "timeline.deleteSelected",
      title: "Ausgewählte Clips löschen",
      category: "Timeline",
      when: "timelineClipSelected",
      run: () => {
        const s = useTimelineStore.getState();
        s.deleteClips(s.selectedClipIds);
      },
    },
    {
      id: "timeline.rippleDelete",
      title: "Clips löschen und Lücke schließen",
      category: "Timeline",
      when: "timelineClipSelected",
      run: () => {
        const s = useTimelineStore.getState();
        s.deleteClips(s.selectedClipIds, { ripple: true });
      },
    },
    {
      id: "timeline.copy",
      title: "Clips kopieren",
      category: "Timeline",
      when: "timelineClipSelected",
      run: () => useTimelineStore.getState().copySelection(),
    },
    {
      id: "timeline.cut",
      title: "Clips ausschneiden",
      category: "Timeline",
      when: "timelineClipSelected",
      run: () => useTimelineStore.getState().cutSelection(),
    },
    {
      id: "timeline.paste",
      title: "Clips am Playhead einfügen",
      category: "Timeline",
      when: "timelineClipboard",
      run: () => useTimelineStore.getState().paste(),
    },
    {
      id: "timeline.selectAll",
      title: "Alle Clips auswählen",
      category: "Timeline",
      when: "timelineHasClips",
      run: () => useTimelineStore.getState().selectAll(),
    },
    {
      id: "timeline.deselectAll",
      title: "Clip-Auswahl aufheben",
      category: "Timeline",
      when: "timelineClipSelected",
      run: () => useTimelineStore.getState().clearSelection(),
    },
    {
      id: "timeline.toggleLink",
      title: "Video/Audio verknüpfen bzw. lösen",
      category: "Timeline",
      when: "timelineClipSelected",
      run: () => useTimelineStore.getState().toggleLinkSelected(),
    },
    {
      id: "timeline.toggleClipEnabled",
      title: "Clip aktivieren/deaktivieren",
      category: "Timeline",
      when: "timelineClipSelected",
      run: () => {
        const s = useTimelineStore.getState();
        const selected = s.clips.filter((c) =>
          s.selectedClipIds.includes(c.id),
        );
        const allEnabled = selected.every((c) => c.enabled);
        s.setClipsEnabled(s.selectedClipIds, !allEnabled);
      },
    },
    {
      id: "timeline.addVideoTrack",
      title: "Videospur hinzufügen",
      category: "Timeline",
      run: () => useTimelineStore.getState().addTrack("video"),
    },
    {
      id: "timeline.addAudioTrack",
      title: "Audiospur hinzufügen",
      category: "Timeline",
      run: () => useTimelineStore.getState().addTrack("audio"),
    },

    // ---------------------------------------------- Timeline: Playhead
    {
      id: "timeline.goToStart",
      title: "Playhead zum Sequenzanfang",
      category: "Timeline",
      run: () => useTimelineStore.getState().goToStart(),
    },
    {
      id: "timeline.goToEnd",
      title: "Playhead zum Sequenzende",
      category: "Timeline",
      run: () => useTimelineStore.getState().goToEnd(),
    },
    {
      id: "timeline.stepBackward",
      title: "Playhead: ein Frame zurück",
      category: "Timeline",
      allowRepeat: true,
      run: () => useTimelineStore.getState().stepPlayheadFrames(-1),
    },
    {
      id: "timeline.stepForward",
      title: "Playhead: ein Frame vorwärts",
      category: "Timeline",
      allowRepeat: true,
      run: () => useTimelineStore.getState().stepPlayheadFrames(1),
    },
    {
      id: "timeline.stepBackward5",
      title: "Playhead: fünf Frames zurück",
      category: "Timeline",
      allowRepeat: true,
      run: () => useTimelineStore.getState().stepPlayheadFrames(-5),
    },
    {
      id: "timeline.stepForward5",
      title: "Playhead: fünf Frames vorwärts",
      category: "Timeline",
      allowRepeat: true,
      run: () => useTimelineStore.getState().stepPlayheadFrames(5),
    },
    {
      id: "timeline.prevEdit",
      title: "Zum vorherigen Schnittpunkt",
      category: "Timeline",
      allowRepeat: true,
      run: () => useTimelineStore.getState().goToPrevEdit(),
    },
    {
      id: "timeline.nextEdit",
      title: "Zum nächsten Schnittpunkt",
      category: "Timeline",
      allowRepeat: true,
      run: () => useTimelineStore.getState().goToNextEdit(),
    },

    // ------------------------------------------------------------- Fenster
    // Je registriertem Panel ein Befehl zum Öffnen/Fokussieren.
    ...allPanels().map(
      (panel): Command => ({
        id: `window.openPanel.${panel.id}`,
        title: `Panel: ${panel.title}`,
        category: "Fenster",
        run: () => openPanel(panel.id),
      }),
    ),
  ];

  return commands;
}

/**
 * Registriert alle Builtin-Commands. Aufruf einmalig beim App-Start
 * (nach registerAllPanels(), damit die Panel-Commands vollständig sind).
 * Liefert einen Disposer, der alle Registrierungen wieder entfernt.
 */
export function registerBuiltinCommands(): () => void {
  return commandRegistry.registerAll(buildCommands());
}
