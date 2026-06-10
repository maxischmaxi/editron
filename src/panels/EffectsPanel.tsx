import { useMemo, useState } from "react";
import {
  Activity,
  ArrowLeftRight,
  Blend,
  ChevronDown,
  ChevronRight,
  Crop,
  Droplets,
  Focus,
  Gauge,
  Mic,
  Move,
  Palette,
  Search,
  SlidersHorizontal,
  Volume2,
  AudioWaveform,
  ZoomIn,
  type LucideIcon,
} from "lucide-react";

interface EffectEntry {
  name: string;
  icon: LucideIcon;
}

interface EffectCategory {
  id: string;
  name: string;
  entries: EffectEntry[];
}

const CATEGORIES: EffectCategory[] = [
  {
    id: "video-effects",
    name: "Video-Effekte",
    entries: [
      { name: "Gaußscher Weichzeichner", icon: Droplets },
      { name: "Schärfen", icon: Focus },
      { name: "Farbkorrektur", icon: Palette },
      { name: "Transformieren", icon: Move },
      { name: "Zuschneiden", icon: Crop },
      { name: "Geschwindigkeit", icon: Gauge },
    ],
  },
  {
    id: "video-transitions",
    name: "Video-Übergänge",
    entries: [
      { name: "Überblendung", icon: Blend },
      { name: "Wischen", icon: ArrowLeftRight },
      { name: "Zoom", icon: ZoomIn },
    ],
  },
  {
    id: "audio-effects",
    name: "Audio-Effekte",
    entries: [
      { name: "Equalizer", icon: SlidersHorizontal },
      { name: "Kompressor", icon: Activity },
      { name: "Hall", icon: AudioWaveform },
      { name: "Rauschminderung", icon: Mic },
    ],
  },
  {
    id: "audio-transitions",
    name: "Audio-Übergänge",
    entries: [{ name: "Konstante Leistung", icon: Volume2 }],
  },
];

export function EffectsPanel() {
  const [query, setQuery] = useState("");
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (q === "") return CATEGORIES;
    return CATEGORIES.map((cat) => ({
      ...cat,
      entries: cat.entries.filter((e) => e.name.toLowerCase().includes(q)),
    })).filter((cat) => cat.entries.length > 0);
  }, [query]);

  const searching = query.trim() !== "";

  return (
    <div className="flex h-full flex-col bg-surface-1">
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-line px-2">
        <Search size={14} className="shrink-0 text-text-3" />
        <input
          type="text"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Effekte durchsuchen …"
          className="h-6 w-full bg-transparent text-xs text-text-1 placeholder:text-text-3 focus:outline-none"
        />
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto py-1">
        {filtered.length === 0 && (
          <p className="px-3 py-4 text-center text-xs text-text-3">
            Keine Effekte gefunden.
          </p>
        )}
        {filtered.map((cat) => {
          const isCollapsed = !searching && collapsed[cat.id] === true;
          return (
            <div key={cat.id}>
              <button
                type="button"
                onClick={() =>
                  setCollapsed((c) => ({ ...c, [cat.id]: !c[cat.id] }))
                }
                className="flex h-7 w-full items-center gap-1.5 px-2 text-xs font-medium text-text-2 hover:bg-surface-2 hover:text-text-1"
              >
                {isCollapsed ? (
                  <ChevronRight size={14} className="shrink-0 text-text-3" />
                ) : (
                  <ChevronDown size={14} className="shrink-0 text-text-3" />
                )}
                <span>{cat.name}</span>
                <span className="ml-auto font-mono text-text-3">
                  {cat.entries.length}
                </span>
              </button>
              {!isCollapsed &&
                cat.entries.map((entry) => {
                  const Icon = entry.icon;
                  return (
                    <div
                      key={entry.name}
                      className="flex h-7 cursor-default items-center gap-2 pr-2 pl-7 text-xs text-text-1 hover:bg-surface-2"
                    >
                      <Icon size={14} className="shrink-0 text-text-2" />
                      <span className="truncate">{entry.name}</span>
                      <span className="ml-auto shrink-0 rounded border border-line px-1 font-mono text-xs leading-4 text-text-3">
                        ffmpeg
                      </span>
                    </div>
                  );
                })}
            </div>
          );
        })}
      </div>

      <div className="shrink-0 border-t border-line px-3 py-2 text-xs text-text-3">
        Effekte anwenden folgt — die FFmpeg-Filtergraph-Basis steht.
      </div>
    </div>
  );
}
