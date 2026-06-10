import { create } from "zustand";

export type TitlePosition = "lower" | "center" | "upper";

export interface TitleLayer {
  id: string;
  name: string;
  text: string;
  fontSize: number;
  color: string;
  position: TitlePosition;
  visible: boolean;
}

/**
 * Titel-Ebenen des Grafik-Panels. Lebt modul-global, damit Workspace-Wechsel
 * (dockview unmountet die Panels) weder die Ebenen samt eingegebenem Text
 * noch die laufende Nummerierung („Titel N“) verlieren.
 */
interface GraphicsState {
  layers: TitleLayer[];
  selectedId: string | null;
  counter: number;
  addLayer: () => void;
  selectLayer: (id: string | null) => void;
  updateLayer: (id: string, patch: Partial<TitleLayer>) => void;
  removeLayer: (id: string) => void;
}

export const useGraphicsStore = create<GraphicsState>((set) => ({
  layers: [],
  selectedId: null,
  counter: 1,
  addLayer: () =>
    set((s) => {
      const layer: TitleLayer = {
        id: crypto.randomUUID(),
        name: `Titel ${s.counter}`,
        text: "Neuer Titel",
        fontSize: 48,
        color: "#ffffff",
        position: "lower",
        visible: true,
      };
      return {
        layers: [...s.layers, layer],
        selectedId: layer.id,
        counter: s.counter + 1,
      };
    }),
  selectLayer: (id) => set({ selectedId: id }),
  updateLayer: (id, patch) =>
    set((s) => ({
      layers: s.layers.map((l) => (l.id === id ? { ...l, ...patch } : l)),
    })),
  removeLayer: (id) =>
    set((s) => ({
      layers: s.layers.filter((l) => l.id !== id),
      selectedId: s.selectedId === id ? null : s.selectedId,
    })),
}));
