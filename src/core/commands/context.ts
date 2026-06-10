import { create } from "zustand";

export type ContextValue = string | number | boolean;

interface ContextState {
  values: Record<string, ContextValue>;
  set: (key: string, value: ContextValue) => void;
  remove: (key: string) => void;
}

/**
 * Globaler UI-Kontext für when-Klauseln — analog zu VS Codes "when contexts".
 * Beispiele: panel (fokussiertes Panel), dialogOpen, mediaSelected.
 */
export const useContextStore = create<ContextState>((set) => ({
  values: {
    panel: "",
    dialogOpen: false,
  },
  set: (key, value) =>
    set((s) =>
      s.values[key] === value
        ? s
        : { values: { ...s.values, [key]: value } },
    ),
  remove: (key) =>
    set((s) => {
      if (!(key in s.values)) return s;
      const values = { ...s.values };
      delete values[key];
      return { values };
    }),
}));

export function setContext(key: string, value: ContextValue): void {
  useContextStore.getState().set(key, value);
}

export function getContextValues(): Record<string, ContextValue> {
  return useContextStore.getState().values;
}

/**
 * Wertet eine when-Klausel gegen die Kontextwerte aus.
 *
 * Unterstützte Syntax (bewusst klein gehalten, keine Klammern):
 *   key             – truthy-Check
 *   !key            – negierter truthy-Check
 *   key == wert     – Vergleich, wert optional in '...'
 *   key != wert
 *   a && b          – UND (bindet stärker als ODER)
 *   a || b          – ODER
 */
export function evaluateWhen(
  expr: string | undefined,
  values: Record<string, ContextValue> = getContextValues(),
): boolean {
  if (!expr || expr.trim() === "") return true;
  return expr
    .split("||")
    .some((orPart) => orPart.split("&&").every((atom) => evalAtom(atom.trim(), values)));
}

function evalAtom(atom: string, values: Record<string, ContextValue>): boolean {
  if (atom === "") return true;

  const cmp = atom.match(/^([\w.]+)\s*(==|!=)\s*'?([^']*?)'?$/);
  if (cmp) {
    const [, key, op, raw] = cmp;
    const actual = values[key];
    const equal = String(actual ?? "") === raw;
    return op === "==" ? equal : !equal;
  }

  const truthy = atom.match(/^(!?)([\w.]+)$/);
  if (truthy) {
    const [, neg, key] = truthy;
    const val = Boolean(values[key]);
    return neg ? !val : val;
  }

  console.warn(`[commands] Ungültige when-Klausel: "${atom}"`);
  return false;
}
