import type { KeyChord, Keybinding } from "./types";
import { chordFromEvent, parseKeys, sequenceStartsWith } from "./keys";
import { evaluateWhen } from "@/core/commands/context";
import { commandRegistry } from "@/core/commands/registry";

export type ResolveResult = "none" | "pending" | "executed";

interface CompiledBinding {
  binding: Keybinding;
  chords: KeyChord[];
}

/** Wartezeit auf den nächsten Chord einer Sequenz. */
const SEQUENCE_TIMEOUT_MS = 1200;

/**
 * Löst Keydown-Events gegen die effektiven Keybindings auf.
 *
 * - Unterstützt Mehrfach-Chords ("Mod+K Mod+S") über einen Sequenz-Puffer.
 * - Ein Binding matcht nur, wenn Binding-`when` UND Command-`when` im
 *   aktuellen Kontext wahr sind.
 * - Bei mehreren exakten Treffern gewinnt der späteste in der Liste —
 *   effectiveBindings() hängt Nutzer-Overrides hinten an, daher schlagen
 *   Overrides die Preset-Bindings.
 * - Ist die gedrückte Sequenz Präfix eines längeren Bindings, wird
 *   gewartet ("pending"); ein gleichzeitig existierender exakter Treffer
 *   wird gemerkt und beim Timeout ausgeführt (wie ein einzelnes "Mod+K",
 *   wenn auch "Mod+K Mod+S" existiert).
 */
export class KeybindingResolver {
  private compiled: CompiledBinding[] = [];
  private buffer: KeyChord[] = [];
  private pendingExact: CompiledBinding | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private readonly timeoutMs: number;

  constructor(timeoutMs: number = SEQUENCE_TIMEOUT_MS) {
    this.timeoutMs = timeoutMs;
  }

  /** Ersetzt alle Bindings (z. B. nach Preset-Wechsel) und leert den Puffer. */
  setBindings(bindings: Keybinding[]): void {
    this.compiled = bindings
      .map((binding) => ({ binding, chords: parseKeys(binding.keys) }))
      .filter((compiled) => compiled.chords.length > 0);
    this.reset();
  }

  /** Verwirft den Sequenz-Puffer und einen laufenden Timeout. */
  reset(): void {
    this.buffer = [];
    this.pendingExact = null;
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }

  dispose(): void {
    this.reset();
  }

  handleKeydown(e: KeyboardEvent): ResolveResult {
    const chord = chordFromEvent(e);
    if (!chord) return "none";

    const sequence = [...this.buffer, chord];
    let exact: CompiledBinding | null = null;
    let hasLonger = false;

    for (const compiled of this.compiled) {
      if (!sequenceStartsWith(compiled.chords, sequence)) continue;
      if (!evaluateWhen(compiled.binding.when)) continue;
      const cmd = commandRegistry.get(compiled.binding.command);
      if (!cmd || !evaluateWhen(cmd.when)) continue;

      if (compiled.chords.length === sequence.length) {
        exact = compiled; // späterer Treffer gewinnt
      } else {
        hasLonger = true;
      }
    }

    if (hasLonger) {
      // Auf weitere Chords warten; ein exakter Treffer wird beim Timeout
      // nachgeholt, falls kein weiterer Chord kommt.
      this.buffer = sequence;
      this.pendingExact = exact;
      this.armTimer();
      e.preventDefault();
      e.stopPropagation();
      return "pending";
    }

    const hadBuffer = this.buffer.length > 0;
    this.reset();

    if (exact) {
      e.preventDefault();
      e.stopPropagation();
      // Key-Auto-Repeat nur ausführen, wenn der Command das erlaubt
      // (allowRepeat) — das Event wird trotzdem verschluckt, damit
      // Browser-Defaults (Scrollen, Button-Aktivierung) nicht greifen.
      if (e.repeat) {
        const cmd = commandRegistry.get(exact.binding.command);
        if (cmd?.allowRepeat !== true) return "none";
      }
      void commandRegistry.execute(exact.binding.command, exact.binding.args);
      return "executed";
    }

    // Sequenz abgebrochen: der Chord, der sie gebrochen hat, wird
    // verworfen (wie in VS Code) und nicht als neuer Sequenz-Anfang gewertet.
    if (hadBuffer) {
      e.preventDefault();
      e.stopPropagation();
    }
    return "none";
  }

  private armTimer(): void {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      const deferred = this.pendingExact;
      this.reset();
      if (deferred) {
        void commandRegistry.execute(
          deferred.binding.command,
          deferred.binding.args,
        );
      }
    }, this.timeoutMs);
  }
}
