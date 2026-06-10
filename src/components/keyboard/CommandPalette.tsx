import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { Search } from "lucide-react";
import type { Command } from "@/core/commands/types";
import { commandRegistry, useCommands } from "@/core/commands/registry";
import { evaluateWhen, useContextStore } from "@/core/commands/context";
import { useKeymapStore } from "@/core/keyboard/keymapStore";
import { useAppStore } from "@/stores/appStore";
import { ShortcutChip } from "./ShortcutChip";

interface PaletteEntry {
  cmd: Command;
  enabled: boolean;
  score: number;
}

/** Substring- (gewichtet) und Fuzzy-Match über Titel, Kategorie und ID. */
function matchScore(query: string, cmd: Command): number | null {
  if (query === "") return 0;
  const title = cmd.title.toLowerCase();
  const haystack =
    `${cmd.category} ${cmd.title} ${cmd.id}`.toLowerCase();

  const titleIndex = title.indexOf(query);
  if (titleIndex >= 0) return 1000 - titleIndex;
  const haystackIndex = haystack.indexOf(query);
  if (haystackIndex >= 0) return 500 - haystackIndex;

  // Fuzzy: alle Zeichen der Query in Reihenfolge enthalten
  let i = 0;
  for (const ch of haystack) {
    if (ch === query[i]) i++;
    if (i === query.length) return 1;
  }
  return null;
}

/**
 * Befehlspalette: oben zentriertes Overlay mit Suchfeld und gefilterter
 * Command-Liste. Deaktivierte Commands (when nicht erfüllt) erscheinen
 * ausgegraut am Ende.
 */
export function CommandPalette() {
  const open = useAppStore((s) => s.commandPaletteOpen);
  if (!open) return null;
  return <PaletteContent />;
}

function PaletteContent() {
  const setOpen = useAppStore((s) => s.setCommandPaletteOpen);
  const commands = useCommands();
  const contextValues = useContextStore((s) => s.values);
  const bindingsForCommand = useKeymapStore((s) => s.bindingsForCommand);

  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // Escape schließt die Palette auch dann, wenn der Fokus sie verlassen
  // hat (z. B. per Tab hinter das Overlay) — daher window-Listener
  // in der Capture-Phase statt nur onKeyDown des Containers.
  useEffect(() => {
    const onWindowKeydown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      setOpen(false);
    };
    window.addEventListener("keydown", onWindowKeydown, true);
    return () => window.removeEventListener("keydown", onWindowKeydown, true);
  }, [setOpen]);

  const entries = useMemo<PaletteEntry[]>(() => {
    const q = query.trim().toLowerCase();
    const matched: PaletteEntry[] = [];
    for (const cmd of commands) {
      const score = matchScore(q, cmd);
      if (score === null) continue;
      matched.push({
        cmd,
        enabled: evaluateWhen(cmd.when, contextValues),
        score,
      });
    }
    // Aktivierte zuerst, dann nach Score; sort() ist stabil,
    // die Registry-Reihenfolge (Kategorie/Titel) bleibt als Tiebreaker.
    matched.sort((a, b) => {
      if (a.enabled !== b.enabled) return a.enabled ? -1 : 1;
      return b.score - a.score;
    });
    return matched;
  }, [commands, contextValues, query]);

  useEffect(() => {
    const el = listRef.current?.querySelector(`[data-index="${selected}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [selected]);

  const close = () => setOpen(false);

  const runEntry = (entry: PaletteEntry | undefined) => {
    if (!entry) return;
    if (!entry.enabled) {
      useAppStore
        .getState()
        .setStatusMessage("Im aktuellen Kontext nicht verfügbar");
      return;
    }
    close();
    void commandRegistry.execute(entry.cmd.id);
  };

  const onKeyDown = (e: ReactKeyboardEvent) => {
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        setSelected((s) => Math.min(s + 1, entries.length - 1));
        break;
      case "ArrowUp":
        e.preventDefault();
        setSelected((s) => Math.max(s - 1, 0));
        break;
      case "Enter":
        e.preventDefault();
        runEntry(entries[selected]);
        break;
      // Escape behandelt der window-Listener oben.
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex flex-col items-center bg-black/40 pt-24"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div
        className="w-full max-w-xl overflow-hidden rounded-lg border border-line-strong bg-surface-2 shadow-2xl"
        onKeyDown={onKeyDown}
      >
        <div className="flex items-center gap-2 border-b border-line px-3">
          <Search className="size-4 shrink-0 text-text-3" />
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setSelected(0);
            }}
            placeholder="Befehl suchen …"
            spellCheck={false}
            className="h-10 min-w-0 flex-1 bg-transparent text-sm text-text-1 outline-none placeholder:text-text-3"
          />
        </div>

        <div ref={listRef} className="max-h-96 overflow-y-auto p-1">
          {entries.length === 0 ? (
            <div className="px-3 py-6 text-center text-sm text-text-3">
              Keine Befehle gefunden
            </div>
          ) : (
            entries.map((entry, i) => (
              <button
                key={entry.cmd.id}
                type="button"
                data-index={i}
                aria-disabled={entry.enabled ? undefined : true}
                onMouseEnter={entry.enabled ? () => setSelected(i) : undefined}
                onClick={() => runEntry(entry)}
                className={`flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-sm ${
                  entry.enabled
                    ? i === selected
                      ? "bg-accent-soft text-text-1"
                      : "text-text-1"
                    : i === selected
                      ? "cursor-default bg-surface-3 text-text-3"
                      : "cursor-default text-text-3"
                }`}
              >
                <span
                  className={`w-24 shrink-0 truncate text-xs ${
                    entry.enabled ? "text-text-3" : "text-text-3/70"
                  }`}
                >
                  {entry.cmd.category}
                </span>
                <span className="min-w-0 flex-1 truncate">
                  {entry.cmd.title}
                </span>
                <span className="flex shrink-0 gap-1">
                  {bindingsForCommand(entry.cmd.id).map((binding, j) => (
                    <ShortcutChip key={j} keys={binding.keys} />
                  ))}
                </span>
              </button>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
