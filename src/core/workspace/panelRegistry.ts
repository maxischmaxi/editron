import { useSyncExternalStore } from "react";
import type { PanelDefinition } from "./types";

const panels = new Map<string, PanelDefinition>();
const listeners = new Set<() => void>();
let snapshot: PanelDefinition[] = [];

function bump() {
  snapshot = [...panels.values()];
  listeners.forEach((fn) => fn());
}

export function registerPanel(def: PanelDefinition): void {
  if (panels.has(def.id)) {
    console.warn(`[panels] Panel "${def.id}" wird überschrieben`);
  }
  panels.set(def.id, def);
  bump();
}

export function getPanel(id: string): PanelDefinition | undefined {
  return panels.get(id);
}

export function allPanels(): PanelDefinition[] {
  return snapshot;
}

function subscribe(fn: () => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** React-Hook: alle registrierten Panels (reaktiv). */
export function usePanels(): PanelDefinition[] {
  return useSyncExternalStore(subscribe, allPanels);
}
