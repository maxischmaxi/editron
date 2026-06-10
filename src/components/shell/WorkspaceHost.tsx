import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { FunctionComponent } from "react";
import {
  DockviewReact,
  type DockviewReadyEvent,
  type DockviewTheme,
  type IDockviewPanelHeaderProps,
  type IDockviewPanelProps,
} from "dockview";
import { PanelsTopLeft, X } from "lucide-react";
import { setContext } from "@/core/commands/context";
import { getPanel, usePanels } from "@/core/workspace/panelRegistry";
import type { PanelDefinition, WorkspaceId } from "@/core/workspace/types";
import {
  loadWorkspaceLayout,
  saveCurrentLayout,
  saveLayoutFor,
  setDockApi,
} from "@/core/workspace/layoutManager";
import { useAppStore } from "@/stores/appStore";

/**
 * Eigenes dockview-Theme: nur Name + Klassenname, die eigentlichen
 * --dv-*-Variablen liegen in src/styles/dockview-theme.css.
 * (Ohne explizites theme-Objekt würde dockview 6 sein Abyss-Default anwenden.)
 */
const EDITRON_THEME: DockviewTheme = {
  name: "editron",
  className: "dockview-theme-editron",
  colorScheme: "dark",
  gap: 2,
  dndTabIndicator: "fill",
};

/**
 * Wrapper um jede Panel-Komponente: meldet bei Fokus/Maus-Interaktion das
 * Panel als Kontext ("panel") für when-Klauseln und sorgt für volles
 * Höhen-/Scrollverhalten. Pro PanelDefinition gecacht, damit dockview
 * stabile Komponenten-Referenzen sieht.
 */
const panelWrappers = new WeakMap<PanelDefinition, FunctionComponent<IDockviewPanelProps>>();

function wrapPanel(def: PanelDefinition): FunctionComponent<IDockviewPanelProps> {
  let wrapper = panelWrappers.get(def);
  if (!wrapper) {
    const Content = def.component;
    const PanelContent: FunctionComponent<IDockviewPanelProps> = (_props) => (
      <div
        className="h-full overflow-auto"
        onFocusCapture={() => setContext("panel", def.id)}
        onMouseDownCapture={() => setContext("panel", def.id)}
      >
        <Content />
      </div>
    );
    PanelContent.displayName = `Panel(${def.id})`;
    wrapper = PanelContent;
    panelWrappers.set(def, wrapper);
  }
  return wrapper;
}

/** Kompakter Tab: Icon (14 px) + Titel, Schließen-Knopf bei Hover. */
function PanelTab({ api }: IDockviewPanelHeaderProps) {
  const [title, setTitle] = useState<string | undefined>(api.title);

  useEffect(() => {
    const disposable = api.onDidTitleChange((event) => setTitle(event.title));
    // Titel kann zwischen Render und Effekt bereits geändert worden sein
    setTitle(api.title);
    return () => disposable.dispose();
  }, [api]);

  const def = getPanel(api.id);
  const Icon = def?.icon;

  return (
    <div className="group flex h-full select-none items-center gap-1.5 pl-2 pr-1 text-xs">
      {Icon ? <Icon className="size-3.5 shrink-0 opacity-80" /> : null}
      <span className="max-w-40 truncate">{title ?? def?.title ?? api.id}</span>
      <button
        type="button"
        title="Schließen"
        aria-label="Panel schließen"
        className="flex size-4 shrink-0 items-center justify-center rounded-xs text-text-3 opacity-0 transition-opacity hover:bg-surface-3 hover:text-text-1 focus-visible:opacity-100 group-hover:opacity-100"
        onPointerDown={(event) => event.preventDefault()}
        onClick={(event) => {
          event.preventDefault();
          api.close();
        }}
      >
        <X className="size-3" />
      </button>
    </div>
  );
}

/** Dezenter Empty-State, wenn keine Panels geöffnet sind. */
function Watermark() {
  return (
    <div className="flex h-full w-full flex-col items-center justify-center gap-2 bg-surface-0">
      <PanelsTopLeft className="size-6 text-text-3 opacity-60" />
      <p className="text-xs text-text-3">Panel über Fenster-Befehle öffnen</p>
    </div>
  );
}

/**
 * Die Docking-Fläche der App: dockview-Host, der Workspaces als
 * umschaltbare Layouts lädt und Nutzeränderungen pro Workspace
 * in localStorage persistiert.
 */
export function WorkspaceHost() {
  const activeWorkspace = useAppStore((s) => s.activeWorkspace);
  const panels = usePanels();

  /** Welcher Workspace aktuell im Dock geladen ist (nicht zwingend == Store). */
  const loadedWorkspaceRef = useRef<WorkspaceId | null>(null);
  const disposablesRef = useRef<{ dispose(): void }[]>([]);

  const components = useMemo(() => {
    const map: Record<string, FunctionComponent<IDockviewPanelProps>> = {};
    for (const def of panels) {
      map[def.id] = wrapPanel(def);
    }
    return map;
  }, [panels]);

  const onReady = useCallback((event: DockviewReadyEvent) => {
    setDockApi(event.api);

    disposablesRef.current.push(
      event.api.onDidActivePanelChange((panel) => {
        setContext("panel", panel?.id ?? "");
      }),
    );

    // StrictMode-sicher: läuft bei Doppelmount erneut mit frischer api,
    // loadWorkspaceLayout baut den Zustand idempotent auf.
    const workspaceId = useAppStore.getState().activeWorkspace;
    loadedWorkspaceRef.current = workspaceId;
    loadWorkspaceLayout(workspaceId);
  }, []);

  // Workspace-Wechsel: altes Layout sichern, Ziel-Layout laden.
  useEffect(() => {
    const previous = loadedWorkspaceRef.current;
    if (previous === null || previous === activeWorkspace) return;
    saveLayoutFor(previous);
    loadedWorkspaceRef.current = activeWorkspace;
    loadWorkspaceLayout(activeWorkspace);
  }, [activeWorkspace]);

  // App-Ende bzw. Fenster verborgen: aktuelles Layout best effort sichern.
  useEffect(() => {
    const onVisibilityChange = () => {
      if (document.visibilityState === "hidden") saveCurrentLayout();
    };
    const onPageHide = () => saveCurrentLayout();

    document.addEventListener("visibilitychange", onVisibilityChange);
    window.addEventListener("pagehide", onPageHide);
    return () => {
      document.removeEventListener("visibilitychange", onVisibilityChange);
      window.removeEventListener("pagehide", onPageHide);
      disposablesRef.current.forEach((d) => d.dispose());
      disposablesRef.current = [];
    };
  }, []);

  return (
    <DockviewReact
      theme={EDITRON_THEME}
      components={components}
      defaultTabComponent={PanelTab}
      watermarkComponent={Watermark}
      onReady={onReady}
    />
  );
}
