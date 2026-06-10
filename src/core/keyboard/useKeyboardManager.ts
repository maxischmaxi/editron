import { useEffect } from "react";
import { KeybindingResolver } from "./resolver";
import { useKeymapStore } from "./keymapStore";
import { getContextValues } from "@/core/commands/context";

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

/**
 * Globaler Keyboard-Manager: lauscht auf window-keydown im Capture-Modus
 * und füttert den Resolver mit den effektiven Bindings.
 *
 * Übersprungen wird, wenn
 * - ein Dialog oder die Befehlspalette offen ist (die handeln ihre
 *   Tasten selbst), oder
 * - das Ziel editierbar ist (Input/Textarea/contenteditable) und der
 *   Chord weder Ctrl noch Meta noch Alt enthält — Tippen geht vor.
 */
export function useKeyboardManager(): void {
  useEffect(() => {
    const resolver = new KeybindingResolver();

    const syncBindings = () => {
      resolver.setBindings(useKeymapStore.getState().effectiveBindings());
    };
    syncBindings();
    const unsubscribe = useKeymapStore.subscribe(syncBindings);

    const onKeydown = (e: KeyboardEvent) => {
      const ctx = getContextValues();
      if (ctx.dialogOpen || ctx.commandPaletteOpen) return;
      if (
        isEditableTarget(e.target) &&
        !e.ctrlKey &&
        !e.metaKey &&
        !e.altKey
      ) {
        return;
      }
      resolver.handleKeydown(e);
    };

    window.addEventListener("keydown", onKeydown, true);
    return () => {
      window.removeEventListener("keydown", onKeydown, true);
      unsubscribe();
      resolver.dispose();
    };
  }, []);
}
