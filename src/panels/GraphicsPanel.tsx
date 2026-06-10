import { Eye, EyeOff, Plus, Trash2, Type } from "lucide-react";
import {
  useGraphicsStore,
  type TitleLayer,
  type TitlePosition,
} from "@/panels/graphicsStore";

const POSITIONS: { id: TitlePosition; label: string }[] = [
  { id: "lower", label: "Unteres Drittel" },
  { id: "center", label: "Mitte" },
  { id: "upper", label: "Oberes Drittel" },
];

export function GraphicsPanel() {
  /* Ebenen leben im graphicsStore (überleben Workspace-Wechsel) */
  const layers = useGraphicsStore((s) => s.layers);
  const selectedId = useGraphicsStore((s) => s.selectedId);
  const addLayer = useGraphicsStore((s) => s.addLayer);
  const selectLayer = useGraphicsStore((s) => s.selectLayer);
  const updateLayer = useGraphicsStore((s) => s.updateLayer);
  const removeLayer = useGraphicsStore((s) => s.removeLayer);

  const selected = layers.find((l) => l.id === selectedId) ?? null;

  const updateSelected = (patch: Partial<TitleLayer>) => {
    if (!selectedId) return;
    updateLayer(selectedId, patch);
  };

  return (
    <div className="flex h-full flex-col bg-surface-1">
      <div className="flex h-9 shrink-0 items-center border-b border-line px-2">
        <button
          type="button"
          onClick={addLayer}
          className="flex h-6 items-center gap-1.5 rounded border border-line px-2 text-xs text-text-1 hover:border-line-strong hover:bg-surface-3"
        >
          <Plus size={14} />
          Text hinzufügen
        </button>
      </div>

      {/* Ebenen-Liste */}
      <div className="max-h-48 shrink-0 overflow-y-auto border-b border-line py-1">
        {layers.length === 0 && (
          <p className="px-3 py-3 text-center text-xs text-text-3">
            Noch keine Ebenen.
          </p>
        )}
        {layers.map((layer) => (
          <div
            key={layer.id}
            onClick={() => selectLayer(layer.id)}
            className={
              layer.id === selectedId
                ? "flex h-7 cursor-pointer items-center gap-2 bg-accent-soft px-2 text-xs text-text-1"
                : "flex h-7 cursor-pointer items-center gap-2 px-2 text-xs text-text-1 hover:bg-surface-2"
            }
          >
            <Type size={14} className="shrink-0 text-text-2" />
            <span className="min-w-0 flex-1 truncate">{layer.name}</span>
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                updateLayer(layer.id, { visible: !layer.visible });
              }}
              title={layer.visible ? "Ausblenden" : "Einblenden"}
              className="shrink-0 rounded p-1 text-text-3 hover:bg-surface-3 hover:text-text-1"
            >
              {layer.visible ? <Eye size={14} /> : <EyeOff size={14} />}
            </button>
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                removeLayer(layer.id);
              }}
              title="Ebene löschen"
              className="shrink-0 rounded p-1 text-text-3 hover:bg-surface-3 hover:text-danger"
            >
              <Trash2 size={14} />
            </button>
          </div>
        ))}
      </div>

      {/* Eigenschaften */}
      <div className="min-h-0 flex-1 overflow-y-auto p-2">
        {selected ? (
          <div className="space-y-2.5">
            <div className="space-y-1">
              <label className="block text-xs text-text-2">Text</label>
              <textarea
                value={selected.text}
                onChange={(e) => updateSelected({ text: e.target.value })}
                rows={2}
                className="w-full resize-none rounded border border-line bg-surface-3 px-2 py-1 text-xs text-text-1 focus:border-accent focus:outline-none"
              />
            </div>

            <div className="flex items-center gap-2">
              <span className="w-24 shrink-0 text-xs text-text-2">
                Schriftgröße
              </span>
              <input
                type="range"
                min={12}
                max={200}
                step={1}
                value={selected.fontSize}
                onChange={(e) =>
                  updateSelected({ fontSize: Number(e.target.value) })
                }
                className="h-1 min-w-0 flex-1 cursor-pointer accent-accent"
              />
              <span className="w-9 shrink-0 text-right font-mono text-xs text-text-1">
                {selected.fontSize}
              </span>
            </div>

            <div className="flex items-center gap-2">
              <span className="w-24 shrink-0 text-xs text-text-2">Farbe</span>
              <input
                type="color"
                value={selected.color}
                onChange={(e) => updateSelected({ color: e.target.value })}
                className="h-6 w-10 cursor-pointer rounded border border-line bg-surface-3 p-0.5"
              />
              <span className="font-mono text-xs text-text-3">
                {selected.color}
              </span>
            </div>

            <div className="flex items-center gap-2">
              <span className="w-24 shrink-0 text-xs text-text-2">
                Position
              </span>
              <select
                value={selected.position}
                onChange={(e) =>
                  updateSelected({ position: e.target.value as TitlePosition })
                }
                className="h-6 min-w-0 flex-1 rounded border border-line bg-surface-3 px-1 text-xs text-text-1 focus:border-accent focus:outline-none"
              >
                {POSITIONS.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.label}
                  </option>
                ))}
              </select>
            </div>
          </div>
        ) : (
          <p className="py-4 text-center text-xs text-text-3">
            Ebene auswählen oder „Text hinzufügen“.
          </p>
        )}
      </div>

      <div className="shrink-0 border-t border-line px-3 py-1.5 text-xs text-text-3">
        Titel-Rendering auf dem Programmmonitor folgt.
      </div>
    </div>
  );
}
