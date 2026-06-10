import type { AddPanelPositionOptions, DockviewApi, IDockviewPanel } from "dockview";
import { getPanel } from "./panelRegistry";
import type { WorkspaceDefinition, WorkspaceId } from "./types";

/**
 * Alle Workspaces in der Reihenfolge der Workspace-Leiste —
 * analog zu Premieres Arbeitsbereichen.
 */
export const WORKSPACES: WorkspaceDefinition[] = [
  { id: "media", name: "Medien", order: 0 },
  { id: "edit", name: "Schnitt", order: 1 },
  { id: "color", name: "Farbe", order: 2 },
  { id: "effects", name: "Effekte", order: 3 },
  { id: "audio", name: "Audio", order: 4 },
  { id: "graphics", name: "Grafik", order: 5 },
];

export function getWorkspace(id: WorkspaceId): WorkspaceDefinition | undefined {
  return WORKSPACES.find((ws) => ws.id === id);
}

interface AddOptions {
  position?: AddPanelPositionOptions;
  initialWidth?: number;
  initialHeight?: number;
  inactive?: boolean;
}

/**
 * Fügt ein Panel aus der panelRegistry hinzu (Komponente = Panel-ID,
 * Titel = deutscher Titel aus der Registry). Defensiv: nicht registrierte
 * Panels werden übersprungen, damit ein halb fertiges Panel-Set das
 * Layout nicht komplett zerlegt.
 */
function add(api: DockviewApi, panelId: string, options: AddOptions = {}): IDockviewPanel | null {
  const def = getPanel(panelId);
  if (!def) {
    console.warn(`[workspace] Panel "${panelId}" ist nicht registriert — wird übersprungen`);
    return null;
  }
  try {
    return api.addPanel({
      id: panelId,
      component: panelId,
      title: def.title,
      ...options,
    });
  } catch (err) {
    console.error(`[workspace] Panel "${panelId}" konnte nicht angelegt werden:`, err);
    return null;
  }
}

function activate(api: DockviewApi, panelId: string): void {
  api.getPanel(panelId)?.api.setActive();
}

/**
 * Baut das Premiere-artige Default-Layout eines Workspace per `api.addPanel`.
 * Erwartet einen leeren Dock (Aufrufer ruft vorher `api.clear()`).
 *
 * Größen werden als Pixel aus den aktuellen Dock-Maßen abgeleitet
 * (dockview erwartet `initialWidth`/`initialHeight` in px).
 */
export function buildDefaultLayout(api: DockviewApi, workspaceId: WorkspaceId): void {
  const width = api.width > 0 ? api.width : 1600;
  const height = api.height > 0 ? api.height : 900;

  switch (workspaceId) {
    case "media": {
      // Links groß der Medien-Browser, rechts oben Quelle,
      // rechts unten Info + Verlauf als Tab-Stapel.
      add(api, "media");
      add(api, "source", {
        position: { direction: "right", referencePanel: "media" },
        initialWidth: Math.round(width * 0.38),
      });
      add(api, "info", {
        position: { direction: "below", referencePanel: "source" },
        initialHeight: Math.round(height * 0.45),
      });
      add(api, "history", {
        position: { direction: "within", referencePanel: "info" },
        inactive: true,
      });
      activate(api, "media");
      break;
    }

    case "edit": {
      // Oben das Vorschau-Fenster mit Programm + Quelle als Tabs
      // (Programm aktiv), unten links Medien + Effekte als Tabs,
      // unten rechts die Timeline.
      add(api, "program");
      add(api, "source", {
        position: { direction: "within", referencePanel: "program" },
        inactive: true,
      });
      add(api, "timeline", {
        position: { direction: "below", referencePanel: "program" },
        initialHeight: Math.round(height * 0.45),
      });
      add(api, "media", {
        position: { direction: "left", referencePanel: "timeline" },
        initialWidth: Math.round(width * 0.3),
      });
      add(api, "effects", {
        position: { direction: "within", referencePanel: "media" },
        inactive: true,
      });
      activate(api, "timeline");
      break;
    }

    case "color": {
      add(api, "program");
      add(api, "scopes", {
        position: { direction: "left", referencePanel: "program" },
        initialWidth: Math.round(width * 0.26),
      });
      add(api, "color", {
        position: { direction: "right", referencePanel: "program" },
        initialWidth: Math.round(width * 0.28),
      });
      add(api, "timeline", {
        position: { direction: "below" },
        initialHeight: Math.round(height * 0.3),
      });
      activate(api, "program");
      break;
    }

    case "effects": {
      add(api, "program");
      add(api, "effects", {
        position: { direction: "left", referencePanel: "program" },
        initialWidth: Math.round(width * 0.22),
      });
      add(api, "effectControls", {
        position: { direction: "right", referencePanel: "program" },
        initialWidth: Math.round(width * 0.28),
      });
      add(api, "timeline", {
        position: { direction: "below" },
        initialHeight: Math.round(height * 0.3),
      });
      activate(api, "program");
      break;
    }

    case "audio": {
      add(api, "program");
      add(api, "audioMixer", {
        position: { direction: "left", referencePanel: "program" },
        initialWidth: Math.round(width * 0.24),
      });
      add(api, "info", {
        position: { direction: "right", referencePanel: "program" },
        initialWidth: Math.round(width * 0.22),
      });
      add(api, "timeline", {
        position: { direction: "below" },
        initialHeight: Math.round(height * 0.3),
      });
      activate(api, "program");
      break;
    }

    case "graphics": {
      add(api, "program");
      add(api, "media", {
        position: { direction: "left", referencePanel: "program" },
        initialWidth: Math.round(width * 0.2),
      });
      add(api, "graphics", {
        position: { direction: "right", referencePanel: "program" },
        initialWidth: Math.round(width * 0.28),
      });
      add(api, "timeline", {
        position: { direction: "below" },
        initialHeight: Math.round(height * 0.3),
      });
      activate(api, "program");
      break;
    }
  }
}
