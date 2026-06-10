import { Clock, Import, Scissors, Sparkles, type LucideIcon } from "lucide-react";

const EXAMPLES: { name: string; icon: LucideIcon }[] = [
  { name: "Import: clip_0042.mp4", icon: Import },
  { name: "Schnitt bei 00:00:12:08", icon: Scissors },
  { name: "Effekt: Gaußscher Weichzeichner", icon: Sparkles },
];

export function HistoryPanel() {
  return (
    <div className="flex h-full flex-col bg-surface-1">
      <div className="flex flex-col items-center gap-2 px-4 pt-8 pb-4 text-center">
        <Clock size={20} className="text-text-3" />
        <p className="text-xs text-text-3">
          Der Verlauf zeichnet künftig jeden Bearbeitungsschritt auf.
        </p>
      </div>

      <div className="px-2">
        {EXAMPLES.map((entry) => {
          const Icon = entry.icon;
          return (
            <div
              key={entry.name}
              className="flex h-7 items-center gap-2 px-2 text-xs text-text-3 opacity-60"
            >
              <Icon size={14} className="shrink-0" />
              <span className="min-w-0 flex-1 truncate">{entry.name}</span>
              <span className="shrink-0 rounded border border-line px-1 font-mono text-xs leading-4">
                Vorschau
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
