import { useSyncExternalStore } from "react";
import type { Command } from "./types";
import { evaluateWhen } from "./context";

type Listener = () => void;

class CommandRegistry {
  private map = new Map<string, Command>();
  private listeners = new Set<Listener>();
  private snapshot: Command[] = [];

  register = (cmd: Command): (() => void) => {
    if (this.map.has(cmd.id)) {
      console.warn(`[commands] Command "${cmd.id}" wird überschrieben`);
    }
    this.map.set(cmd.id, cmd);
    this.bump();
    return () => {
      if (this.map.get(cmd.id) === cmd) {
        this.map.delete(cmd.id);
        this.bump();
      }
    };
  };

  registerAll = (cmds: Command[]): (() => void) => {
    const disposers = cmds.map((c) => this.register(c));
    return () => disposers.forEach((d) => d());
  };

  get = (id: string): Command | undefined => this.map.get(id);

  /** Stabiler Snapshot, sortiert nach Kategorie und Titel. */
  all = (): Command[] => this.snapshot;

  /** Prüft die when-Klausel des Commands gegen den aktuellen Kontext. */
  isEnabled = (id: string): boolean => {
    const cmd = this.map.get(id);
    return cmd ? evaluateWhen(cmd.when) : false;
  };

  /**
   * Führt einen Command aus, sofern er existiert und sein Kontext passt.
   * Liefert true, wenn er tatsächlich ausgeführt wurde.
   */
  execute = async (id: string, args?: unknown): Promise<boolean> => {
    const cmd = this.map.get(id);
    if (!cmd) {
      console.warn(`[commands] Unbekannter Command: "${id}"`);
      return false;
    }
    if (!evaluateWhen(cmd.when)) return false;
    try {
      await cmd.run(args);
      return true;
    } catch (err) {
      console.error(`[commands] Fehler in "${id}":`, err);
      return false;
    }
  };

  subscribe = (fn: Listener): (() => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };

  private bump() {
    this.snapshot = [...this.map.values()].sort(
      (a, b) =>
        a.category.localeCompare(b.category, "de") ||
        a.title.localeCompare(b.title, "de"),
    );
    this.listeners.forEach((fn) => fn());
  }
}

export const commandRegistry = new CommandRegistry();

/** React-Hook: alle registrierten Commands (reaktiv). */
export function useCommands(): Command[] {
  return useSyncExternalStore(commandRegistry.subscribe, commandRegistry.all);
}
