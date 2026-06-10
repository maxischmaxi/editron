import { useEffect, useMemo, useRef, useState } from "react";
import { TriangleAlert } from "lucide-react";
import type { KeyChord } from "@/core/keyboard/types";
import {
  chordFromEvent,
  formatKeys,
  serializeChords,
} from "@/core/keyboard/keys";
import { useKeymapStore } from "@/core/keyboard/keymapStore";
import { commandRegistry } from "@/core/commands/registry";

/** Weitere Chords innerhalb dieses Fensters bilden eine Sequenz. */
const SEQUENCE_WINDOW_MS = 1000;

/**
 * Aufnahme-Modal: fängt Tastendrücke ab und baut daraus einen Chord
 * bzw. eine Sequenz. Escape bricht ab, Enter oder Klick übernimmt.
 */
export function KeyRecorderModal({
  commandId,
  commandTitle,
  onCancel,
  onSubmit,
}: {
  commandId: string;
  commandTitle: string;
  onCancel: () => void;
  onSubmit: (keys: string) => void;
}) {
  const [chords, setChords] = useState<KeyChord[]>([]);
  const lastChordAt = useRef(0);

  const serialized = serializeChords(chords);
  const conflictsFor = useKeymapStore((s) => s.conflictsFor);

  const conflictNames = useMemo(() => {
    if (chords.length === 0) return [];
    const conflicts = conflictsFor(serialized, commandId);
    return [
      ...new Set(
        conflicts.map(
          (b) => commandRegistry.get(b.command)?.title ?? b.command,
        ),
      ),
    ];
  }, [chords.length, serialized, commandId, conflictsFor]);

  useEffect(() => {
    const onKeydown = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopImmediatePropagation();

      // Auto-Repeat einer gehaltenen Taste darf keine Sequenz aufbauen.
      if (e.repeat) return;

      if (e.key === "Escape") {
        onCancel();
        return;
      }
      if (e.key === "Enter") {
        if (chords.length > 0) onSubmit(serializeChords(chords));
        return;
      }

      const chord = chordFromEvent(e);
      if (!chord) return; // reiner Modifier-Druck

      const now = Date.now();
      const withinWindow =
        chords.length > 0 && now - lastChordAt.current <= SEQUENCE_WINDOW_MS;
      lastChordAt.current = now;
      setChords(withinWindow ? [...chords, chord] : [chord]);
    };

    window.addEventListener("keydown", onKeydown, true);
    return () => window.removeEventListener("keydown", onKeydown, true);
  }, [chords, onCancel, onSubmit]);

  return (
    <div className="fixed inset-0 z-60 flex items-center justify-center bg-black/50">
      <div className="w-full max-w-md rounded-lg border border-line-strong bg-surface-2 p-4 shadow-2xl">
        <h3 className="text-sm font-semibold text-text-1">
          Tastenkombination für „{commandTitle}“
        </h3>
        <p className="mt-1 text-xs text-text-3">
          Gewünschte Kombination drücken. Weitere Chords innerhalb von 1
          Sekunde ergeben eine Sequenz. Enter übernimmt, Escape bricht ab.
        </p>

        <div className="mt-3 flex min-h-12 items-center justify-center rounded border border-line bg-surface-1 px-3 py-2">
          {chords.length === 0 ? (
            <span className="text-sm text-text-3">Warte auf Eingabe …</span>
          ) : (
            <span className="font-mono text-base text-text-1">
              {formatKeys(serialized)}
            </span>
          )}
        </div>

        {conflictNames.length > 0 && (
          <div className="mt-2 flex items-start gap-1.5 text-xs text-danger">
            <TriangleAlert className="mt-0.5 size-3.5 shrink-0" />
            <span>Konflikt mit: {conflictNames.join(", ")}</span>
          </div>
        )}

        <div className="mt-4 flex justify-end gap-2">
          <button
            type="button"
            onClick={onCancel}
            className="h-8 rounded border border-line-strong bg-surface-3 px-3 text-xs text-text-2 hover:bg-surface-4 hover:text-text-1"
          >
            Abbrechen
          </button>
          <button
            type="button"
            disabled={chords.length === 0}
            onClick={() => onSubmit(serialized)}
            className="h-8 rounded bg-accent px-3 text-xs font-medium text-white hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
          >
            Übernehmen
          </button>
        </div>
      </div>
    </div>
  );
}
