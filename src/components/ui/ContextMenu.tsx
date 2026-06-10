import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ComponentType,
} from "react";
import { create } from "zustand";
import { Check, ChevronRight } from "lucide-react";
import { commandRegistry } from "@/core/commands/registry";
import { useKeymapStore } from "@/core/keyboard/keymapStore";
import { formatKeys } from "@/core/keyboard/keys";

/**
 * App-eigenes Kontextmenü. Items können direkt einen Command referenzieren:
 * Label (Fallback), Shortcut-Anzeige und Enabled-Zustand kommen dann aus
 * Registry + Keymap — Menü, Palette und Tastatur bleiben so konsistent.
 */

export interface MenuItem {
  type?: "item";
  label: string;
  icon?: ComponentType<{ className?: string }>;
  commandId?: string;
  args?: unknown;
  onSelect?: () => void;
  disabled?: boolean;
  danger?: boolean;
  checked?: boolean;
}

export interface MenuSeparator {
  type: "separator";
}

export interface MenuSubmenu {
  type: "submenu";
  label: string;
  icon?: ComponentType<{ className?: string }>;
  items: MenuEntry[];
}

export type MenuEntry = MenuItem | MenuSeparator | MenuSubmenu;

/** Menü-Item aus einem registrierten Command (Titel aus der Registry). */
export function commandItem(
  commandId: string,
  overrides?: Partial<MenuItem>,
): MenuItem {
  return {
    label: commandRegistry.get(commandId)?.title ?? commandId,
    commandId,
    ...overrides,
  };
}

interface ContextMenuState {
  open: boolean;
  x: number;
  y: number;
  items: MenuEntry[];
  show: (x: number, y: number, items: MenuEntry[]) => void;
  close: () => void;
}

const useContextMenuStore = create<ContextMenuState>((set) => ({
  open: false,
  x: 0,
  y: 0,
  items: [],
  show: (x, y, items) => set({ open: true, x, y, items }),
  close: () => set({ open: false }),
}));

/**
 * Öffnet das Kontextmenü an der Mausposition des Events und unterdrückt
 * das native Menü des Webviews.
 */
export function openContextMenu(
  e: { clientX: number; clientY: number; preventDefault(): void; stopPropagation(): void },
  items: MenuEntry[],
): void {
  e.preventDefault();
  e.stopPropagation();
  if (items.length === 0) return;
  useContextMenuStore.getState().show(e.clientX, e.clientY, items);
}

export function closeContextMenu(): void {
  useContextMenuStore.getState().close();
}

function isItem(entry: MenuEntry): entry is MenuItem {
  return entry.type === undefined || entry.type === "item";
}

function itemDisabled(item: MenuItem): boolean {
  if (item.disabled) return true;
  if (item.commandId && !item.onSelect) {
    return !commandRegistry.isEnabled(item.commandId);
  }
  return false;
}

function shortcutFor(item: MenuItem): string | null {
  if (!item.commandId) return null;
  const binding = useKeymapStore.getState().bindingsForCommand(item.commandId)[0];
  return binding ? formatKeys(binding.keys) : null;
}

function MenuList({
  items,
  onClose,
  autoFocus,
}: {
  items: MenuEntry[];
  onClose: () => void;
  autoFocus?: boolean;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [activeIdx, setActiveIdx] = useState(-1);
  const [openSub, setOpenSub] = useState(-1);

  useEffect(() => {
    if (autoFocus) ref.current?.focus();
  }, [autoFocus]);

  const runItem = (item: MenuItem) => {
    if (itemDisabled(item)) return;
    onClose();
    if (item.onSelect) {
      item.onSelect();
    } else if (item.commandId) {
      void commandRegistry.execute(item.commandId, item.args);
    }
  };

  const selectable = items
    .map((entry, idx) => ({ entry, idx }))
    .filter(
      ({ entry }) =>
        entry.type === "submenu" || (isItem(entry) && !itemDisabled(entry)),
    )
    .map(({ idx }) => idx);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
      return;
    }
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (selectable.length === 0) return;
      const dir = e.key === "ArrowDown" ? 1 : -1;
      const pos = selectable.indexOf(activeIdx);
      const next =
        pos === -1
          ? dir === 1
            ? selectable[0]
            : selectable[selectable.length - 1]
          : selectable[(pos + dir + selectable.length) % selectable.length];
      setActiveIdx(next);
      setOpenSub(-1);
      return;
    }
    if (e.key === "Enter" || e.key === "ArrowRight") {
      const entry = items[activeIdx];
      if (!entry) return;
      e.preventDefault();
      if (entry.type === "submenu") {
        setOpenSub(activeIdx);
      } else if (isItem(entry) && e.key === "Enter") {
        runItem(entry);
      }
    }
  };

  return (
    <div
      ref={ref}
      role="menu"
      tabIndex={-1}
      onKeyDown={onKeyDown}
      className="min-w-52 rounded-md border border-line bg-surface-2 py-1 shadow-xl outline-none"
    >
      {items.map((entry, idx) => {
        if (entry.type === "separator") {
          return <div key={idx} className="mx-2 my-1 h-px bg-line" />;
        }
        if (entry.type === "submenu") {
          const Icon = entry.icon;
          return (
            <div
              key={idx}
              role="menuitem"
              className="relative"
              onMouseEnter={() => {
                setActiveIdx(idx);
                setOpenSub(idx);
              }}
              onMouseLeave={() => setOpenSub((cur) => (cur === idx ? -1 : cur))}
            >
              <div
                className={`flex h-7 cursor-default items-center gap-2 px-2.5 text-xs ${
                  activeIdx === idx
                    ? "bg-surface-3 text-text-1"
                    : "text-text-1"
                }`}
              >
                <span className="flex w-4 shrink-0 items-center justify-center">
                  {Icon ? <Icon className="size-3.5 text-text-2" /> : null}
                </span>
                <span className="min-w-0 flex-1 truncate">{entry.label}</span>
                <ChevronRight className="size-3.5 shrink-0 text-text-3" />
              </div>
              {openSub === idx && (
                <div className="absolute left-full top-0 -ml-1 -mt-1.5 pl-1">
                  <MenuList items={entry.items} onClose={onClose} />
                </div>
              )}
            </div>
          );
        }

        const item = entry;
        const Icon = item.icon;
        const disabled = itemDisabled(item);
        const shortcut = shortcutFor(item);
        return (
          <button
            key={idx}
            type="button"
            role="menuitem"
            disabled={disabled}
            onMouseEnter={() => {
              setActiveIdx(idx);
              setOpenSub(-1);
            }}
            onClick={() => runItem(item)}
            className={`flex h-7 w-full items-center gap-2 px-2.5 text-left text-xs ${
              disabled
                ? "cursor-default text-text-3"
                : activeIdx === idx
                  ? `bg-surface-3 ${item.danger ? "text-danger" : "text-text-1"}`
                  : item.danger
                    ? "text-danger"
                    : "text-text-1"
            }`}
          >
            <span className="flex w-4 shrink-0 items-center justify-center">
              {item.checked ? (
                <Check className="size-3.5" />
              ) : Icon ? (
                <Icon className={`size-3.5 ${disabled ? "" : "text-text-2"}`} />
              ) : null}
            </span>
            <span className="min-w-0 flex-1 truncate">{item.label}</span>
            {shortcut && (
              <span className="shrink-0 pl-4 font-mono text-xs text-text-3">
                {shortcut}
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}

/** Singleton-Host — einmal in App einhängen. */
export function ContextMenuHost() {
  const { open, x, y, items, close } = useContextMenuStore();
  const panelRef = useRef<HTMLDivElement | null>(null);
  const [pos, setPos] = useState({ left: 0, top: 0 });

  // An den Viewport anpassen, damit das Menü nie aus dem Fenster ragt.
  useLayoutEffect(() => {
    if (!open) return;
    const el = panelRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const left = Math.max(4, Math.min(x, window.innerWidth - rect.width - 4));
    const top = Math.max(4, Math.min(y, window.innerHeight - rect.height - 4));
    setPos({ left, top });
  }, [open, x, y, items]);

  useEffect(() => {
    if (!open) return;
    const onBlur = () => close();
    window.addEventListener("blur", onBlur);
    window.addEventListener("resize", onBlur);
    return () => {
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("resize", onBlur);
    };
  }, [open, close]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50"
      onPointerDown={(e) => {
        if (e.target === e.currentTarget) close();
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        close();
      }}
    >
      <div
        ref={panelRef}
        className="absolute"
        style={{ left: pos.left, top: pos.top }}
      >
        <MenuList items={items} onClose={close} autoFocus />
      </div>
    </div>
  );
}
