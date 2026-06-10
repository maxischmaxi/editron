import {
  Activity,
  AudioLines,
  Film,
  FolderOpen,
  History,
  Info,
  ListVideo,
  Palette,
  Play,
  SlidersHorizontal,
  Sparkles,
  Type,
} from "lucide-react";
import { registerPanel } from "@/core/workspace/panelRegistry";
import type { PanelDefinition } from "@/core/workspace/types";
import { MediaBrowserPanel } from "./MediaBrowserPanel";
import { SourceMonitorPanel } from "./SourceMonitorPanel";
import { ProgramMonitorPanel } from "./ProgramMonitorPanel";
import { TimelinePanel } from "./timeline/TimelinePanel";
import { EffectsPanel } from "./EffectsPanel";
import { EffectControlsPanel } from "./EffectControlsPanel";
import { AudioMixerPanel } from "./AudioMixerPanel";
import { ColorPanel } from "./ColorPanel";
import { ScopesPanel } from "./ScopesPanel";
import { GraphicsPanel } from "./GraphicsPanel";
import { InfoPanel } from "./InfoPanel";
import { HistoryPanel } from "./HistoryPanel";

const PANELS: PanelDefinition[] = [
  { id: "media", title: "Medien", icon: FolderOpen, component: MediaBrowserPanel },
  { id: "source", title: "Quelle", icon: Film, component: SourceMonitorPanel },
  { id: "program", title: "Programm", icon: Play, component: ProgramMonitorPanel },
  { id: "timeline", title: "Timeline", icon: ListVideo, component: TimelinePanel },
  { id: "effects", title: "Effekte", icon: Sparkles, component: EffectsPanel },
  {
    id: "effectControls",
    title: "Effekteinstellungen",
    icon: SlidersHorizontal,
    component: EffectControlsPanel,
  },
  {
    id: "audioMixer",
    title: "Audio-Mixer",
    icon: AudioLines,
    component: AudioMixerPanel,
  },
  { id: "color", title: "Farbe", icon: Palette, component: ColorPanel },
  { id: "scopes", title: "Scopes", icon: Activity, component: ScopesPanel },
  { id: "graphics", title: "Grafiken", icon: Type, component: GraphicsPanel },
  { id: "info", title: "Info", icon: Info, component: InfoPanel },
  { id: "history", title: "Verlauf", icon: History, component: HistoryPanel },
];

/** Registriert alle Panels in der panelRegistry (idempotent aufrufbar). */
export function registerAllPanels(): void {
  PANELS.forEach(registerPanel);
}
