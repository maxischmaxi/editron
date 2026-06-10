import type { DockviewApi, SerializedDockview } from "dockview";
import { useAppStore } from "@/stores/appStore";
import { getPanel } from "./panelRegistry";
import type { WorkspaceId } from "./types";
import { buildDefaultLayout } from "./workspaces";

/**
 * Hält die DockviewApi-Referenz und kapselt Layout-Persistenz
 * (localStorage, ein Eintrag pro Workspace) sowie Panel-/Layout-Befehle,
 * die das Shortcut-System aufruft.
 */

// v2: Schnitt-Workspace mit Vorschau-Tabs (Programm + Quelle in einer
// Gruppe) — alte v1-Layouts werden bewusst nicht mehr geladen.
const STORAGE_PREFIX = "editron.layout.v2.";

let dockApi: DockviewApi | null = null;

export function setDockApi(api: DockviewApi): void {
  dockApi = api;
}

export function getDockApi(): DockviewApi | null {
  return dockApi;
}

function storageKey(workspaceId: string): string {
  return `${STORAGE_PREFIX}${workspaceId}`;
}

/** Layout eines Workspace nach localStorage sichern (best effort). */
export function saveLayoutFor(workspaceId: string): void {
  if (!dockApi) return;
  try {
    localStorage.setItem(storageKey(workspaceId), JSON.stringify(dockApi.toJSON()));
  } catch (err) {
    console.warn(`[layout] Layout für "${workspaceId}" konnte nicht gespeichert werden:`, err);
  }
}

/** Layout des aktiven Workspace sichern (best effort). */
export function saveCurrentLayout(): void {
  saveLayoutFor(useAppStore.getState().activeWorkspace);
}

/**
 * Layout eines Workspace laden: gespeichertes JSON via `fromJSON`,
 * bei Fehler/Versionssalat den Eintrag verwerfen und das Default bauen.
 */
export function loadWorkspaceLayout(workspaceId: WorkspaceId): void {
  if (!dockApi) return;

  const key = storageKey(workspaceId);
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(key);
  } catch {
    // localStorage nicht verfügbar — einfach das Default bauen
  }

  if (raw !== null) {
    try {
      dockApi.fromJSON(JSON.parse(raw) as SerializedDockview);
      return;
    } catch (err) {
      console.warn(
        `[layout] Gespeichertes Layout für "${workspaceId}" ist ungültig und wird verworfen:`,
        err,
      );
      try {
        localStorage.removeItem(key);
      } catch {
        // best effort
      }
    }
  }

  try {
    dockApi.clear();
  } catch {
    // Dock war ggf. schon leer / mitten im Teardown
  }
  buildDefaultLayout(dockApi, workspaceId);
}

/**
 * "Layout zurücksetzen": gespeichertes Layout des aktiven Workspace
 * löschen, Dock leeren und das Default-Layout neu bauen.
 */
export function resetCurrentWorkspaceLayout(): void {
  if (!dockApi) return;
  const workspaceId = useAppStore.getState().activeWorkspace;
  try {
    localStorage.removeItem(storageKey(workspaceId));
  } catch {
    // best effort
  }
  try {
    dockApi.clear();
  } catch {
    // best effort
  }
  buildDefaultLayout(dockApi, workspaceId);
}

/**
 * Panel öffnen bzw. fokussieren: existiert es bereits, wird es aktiviert,
 * sonst in der aktiven Gruppe als Tab hinzugefügt (Titel/Komponente aus
 * der panelRegistry).
 */
export function openPanel(panelId: string): void {
  if (!dockApi) return;

  const existing = dockApi.getPanel(panelId);
  if (existing) {
    existing.api.setActive();
    return;
  }

  const def = getPanel(panelId);
  if (!def) {
    console.warn(`[layout] Unbekanntes Panel: "${panelId}"`);
    return;
  }

  try {
    dockApi.addPanel({
      id: panelId,
      component: panelId,
      title: def.title,
    });
  } catch (err) {
    console.error(`[layout] Panel "${panelId}" konnte nicht geöffnet werden:`, err);
  }
}
