//! Gesamtzustand der App (alle Stores) + Workspace-Wechsel-Logik.

use crate::core::dock::DockManager;
use crate::core::keyboard::KeymapStore;
use crate::core::project::ProjectStore;
use crate::core::timeline::TimelineStore;
use crate::overlays::context_menu::ContextMenuState;
use crate::stores::{AppStore, AudioStore, MediaStore, MonitorStore, PlaybackStore};

pub struct AppState {
    pub app: AppStore,
    pub media: MediaStore,
    pub timeline: TimelineStore,
    pub playback: PlaybackStore,
    pub audio: AudioStore,
    pub monitor: MonitorStore,
    pub keymap: KeymapStore,
    pub dock: DockManager,
    pub context_menu: ContextMenuState,
    pub project: ProjectStore,
}

impl Default for AppState {
    fn default() -> Self {
        let mut state = AppState {
            app: AppStore::default(),
            media: MediaStore::default(),
            timeline: TimelineStore::default(),
            playback: PlaybackStore::default(),
            audio: AudioStore::default(),
            monitor: MonitorStore::default(),
            keymap: KeymapStore::load(),
            dock: DockManager::default(),
            context_menu: ContextMenuState::default(),
            project: ProjectStore::default(),
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
