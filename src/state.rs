//! Gesamtzustand der App (alle Stores) + Workspace-Wechsel-Logik.

use crate::core::dock::DockManager;
use crate::core::keyboard::KeymapStore;
use crate::core::project::ProjectStore;
use crate::core::render_cache::RenderCacheStore;
use crate::core::render_queue::RenderQueue;
use crate::core::sequences::SequenceStore;
use crate::core::settings::AppSettings;
use crate::overlays::context_menu::ContextMenuState;
use crate::stores::{AppStore, AudioStore, MediaStore, MonitorStore, PlaybackStore};

pub struct AppState {
    pub app: AppStore,
    pub media: MediaStore,
    /// Alle Sequenzen des Projekts; dereferenziert transparent auf die aktive
    /// (`state.timeline.clips` etc. wirken auf die aktive Sequenz).
    pub timeline: SequenceStore,
    pub playback: PlaybackStore,
    pub audio: AudioStore,
    pub monitor: MonitorStore,
    pub keymap: KeymapStore,
    pub dock: DockManager,
    pub context_menu: ContextMenuState,
    pub project: ProjectStore,
    /// Maschinen-/nutzergebundene App-Einstellungen (Hardware-Decode,
    /// Cache-Budget, Render-Cache-Codec, Autosave, ffmpeg-Pfad, UI-Scale …).
    /// Persistiert separat von Projekten in der `settings.json`.
    pub settings: AppSettings,
    /// Sequenz-Render-Cache: gerenderte Bereiche + Gültigkeit (Laufzeit).
    pub render_cache: RenderCacheStore,
    /// Render-Warteschlange: Export-Jobs, die im Hintergrund sequentiell
    /// abgearbeitet werden (Laufzeit; nicht persistiert).
    pub render_queue: RenderQueue,
}

impl Default for AppState {
    fn default() -> Self {
        let mut state = AppState {
            app: AppStore::default(),
            media: MediaStore::default(),
            timeline: SequenceStore::default(),
            playback: PlaybackStore::default(),
            audio: AudioStore::default(),
            monitor: MonitorStore::default(),
            keymap: KeymapStore::load(),
            dock: DockManager::default(),
            context_menu: ContextMenuState::default(),
            project: ProjectStore::default(),
            settings: AppSettings::load(),
            render_cache: RenderCacheStore::default(),
            render_queue: RenderQueue::default(),
        };
        let ws = state.app.active_workspace.clone();
        state.dock.load_workspace_layout(&ws);
        state
    }
}

/// Workspace wechseln: altes Layout sichern, Ziel-Layout laden.
pub fn set_active_workspace(state: &mut AppState, id: &str) {
    if state.app.active_workspace == id {
        return;
    }
    let previous = state.app.active_workspace.clone();
    state.dock.save_layout_for(&previous);
    state.app.active_workspace = id.to_string();
    state.dock.load_workspace_layout(id);
}
