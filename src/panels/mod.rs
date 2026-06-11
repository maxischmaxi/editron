//! Panel-Registry: jedes Panel ist eine Komponente mit eigenem UI-State,
//! registriert unter seiner ID.

pub mod audio_mixer;
pub mod color;
pub mod effect_controls;
pub mod effects;
pub mod graphics;
pub mod info;
pub mod media_browser;
pub mod monitor;
pub mod scopes;
pub mod timeline;

use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::{FontKind, Ui};
use std::collections::HashMap;

/// (id, deutscher Titel, Lucide-Icon).
pub const PANEL_DEFS: [(&str, &str, &str); 12] = [
    ("media", "Medien", "folder-open"),
    ("source", "Quelle", "film"),
    ("program", "Programm", "play"),
    ("timeline", "Timeline", "list-video"),
    ("effects", "Effekte", "sparkles"),
    ("effectControls", "Effekteinstellungen", "sliders-horizontal"),
    ("audioMixer", "Audio-Mixer", "audio-lines"),
    ("color", "Farbe", "palette"),
    ("scopes", "Scopes", "activity"),
    ("graphics", "Grafiken", "type"),
    ("info", "Info", "info"),
    ("history", "Verlauf", "history"),
];

pub fn panel_title(id: &str) -> &'static str {
    PANEL_DEFS
        .iter()
        .find(|(pid, _, _)| *pid == id)
        .map(|(_, title, _)| *title)
        .unwrap_or("?")
}

pub fn panel_icon(id: &str) -> &'static str {
    PANEL_DEFS
        .iter()
        .find(|(pid, _, _)| *pid == id)
        .map(|(_, _, icon)| *icon)
        .unwrap_or("panels-top-left")
}

pub trait Panel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, services: &Services, rect: Rect);
}

/// Platzhalter, bis das jeweilige Panel portiert ist.
struct StubPanel {
    title: &'static str,
    icon: &'static str,
}

impl Panel for StubPanel {
    fn update(&mut self, ui: &mut Ui, _app: &mut AppState, _services: &Services, rect: Rect) {
        let center = rect.center_box(200.0, 56.0);
        let mut area = center;
        let icon_rect = area.cut_top(24.0);
        ui.icon(self.icon, icon_rect, 24.0, theme::with_alpha(theme::TEXT_3, 153));
        area.cut_top(8.0);
        ui.text_centered(self.title, area, theme::TEXT_3, FontKind::Sans12);
    }
}

pub struct PanelHost {
    instances: HashMap<String, Box<dyn Panel>>,
}

impl PanelHost {
    pub fn new() -> PanelHost {
        let mut instances: HashMap<String, Box<dyn Panel>> = HashMap::new();
        instances.insert(
            "media".into(),
            Box::new(media_browser::MediaBrowserPanel::default()),
        );
        instances.insert(
            "timeline".into(),
            Box::new(timeline::TimelinePanel::default()),
        );
        instances.insert(
            "source".into(),
            Box::new(monitor::SourceMonitorPanel::default()),
        );
        instances.insert(
            "program".into(),
            Box::new(monitor::ProgramMonitorPanel::default()),
        );
        instances.insert("effects".into(), Box::new(effects::EffectsPanel::default()));
        instances.insert("info".into(), Box::new(info::InfoPanel::default()));
        instances.insert("history".into(), Box::new(info::HistoryPanel));
        instances.insert("color".into(), Box::new(color::ColorPanel::default()));
        instances.insert("scopes".into(), Box::new(scopes::ScopesPanel::default()));
        instances.insert(
            "audioMixer".into(),
            Box::new(audio_mixer::AudioMixerPanel::default()),
        );
        instances.insert(
            "graphics".into(),
            Box::new(graphics::GraphicsPanel::default()),
        );
        instances.insert(
            "effectControls".into(),
            Box::new(effect_controls::EffectControlsPanel::default()),
        );
        for (id, title, icon) in PANEL_DEFS {
            instances
                .entry(id.to_string())
                .or_insert_with(|| Box::new(StubPanel { title, icon }));
        }
        PanelHost { instances }
    }

    pub fn update(
        &mut self,
        id: &str,
        ui: &mut Ui,
        app: &mut AppState,
        services: &Services,
        rect: Rect,
    ) {
        if let Some(panel) = self.instances.get_mut(id) {
            panel.update(ui, app, services, rect);
        }
    }
}
