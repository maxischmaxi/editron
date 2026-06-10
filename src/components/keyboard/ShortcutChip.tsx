import { formatKeys } from "@/core/keyboard/keys";

/**
 * Kompakter Shortcut-Chip im kbd-Stil, optional rot bei Konflikt.
 */
export function ShortcutChip({
  keys,
  conflict = false,
  title,
}: {
  /** Serialisierter Keybinding-String, z. B. "Mod+Shift+K" */
  keys: string;
  conflict?: boolean;
  title?: string;
}) {
  return (
    <kbd
      title={title}
      className={`inline-flex items-center whitespace-nowrap rounded border px-1.5 py-0.5 font-mono text-xs leading-none ${
        conflict
          ? "border-danger/60 bg-danger/10 text-danger"
          : "border-line-strong bg-surface-3 text-text-2"
      }`}
    >
      {formatKeys(keys)}
    </kbd>
  );
}
