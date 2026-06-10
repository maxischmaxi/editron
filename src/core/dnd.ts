/**
 * Übergabe von Drag-Daten innerhalb der App.
 *
 * HTML5-DnD erlaubt während `dragover` keinen Lesezugriff auf die
 * dataTransfer-Daten — nur auf die Typen. Da Quelle und Ziel im selben
 * Fenster leben, werden die gezogenen Asset-IDs zusätzlich hier abgelegt,
 * damit das Timeline-Ziel schon während des Überfahrens eine korrekte
 * Vorschau (Cliplängen, Spurzuordnung) rendern kann.
 */

export const ASSET_DRAG_MIME = "application/x-editron-assets";

let draggedAssetIds: string[] = [];

export function setDraggedAssets(ids: string[]): void {
  draggedAssetIds = ids;
}

export function getDraggedAssets(): string[] {
  return draggedAssetIds;
}

export function clearDraggedAssets(): void {
  draggedAssetIds = [];
}
