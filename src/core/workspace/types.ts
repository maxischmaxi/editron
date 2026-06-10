import type { ComponentType } from "react";

/**
 * Ein Panel ist ein andockbares Modul (Timeline, Monitor, Medien-Browser …),
 * das in beliebigen Workspaces als Tab platziert werden kann.
 */
export interface PanelDefinition {
  /** Eindeutige ID, z. B. "timeline" */
  id: string;
  /** Tab-Titel (deutsch), z. B. "Timeline" */
  title: string;
  icon?: ComponentType<{ className?: string }>;
  component: ComponentType;
}

export type WorkspaceId =
  | "media"
  | "edit"
  | "color"
  | "effects"
  | "audio"
  | "graphics";

export interface WorkspaceDefinition {
  id: WorkspaceId;
  /** Nutzersichtbarer Name, z. B. "Schnitt" */
  name: string;
  /** Reihenfolge in der Workspace-Leiste */
  order: number;
}
