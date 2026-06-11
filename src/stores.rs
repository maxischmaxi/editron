//! Zentraler App-Zustand: App-, Medien-, Wiedergabe- und Monitor-Store
//! plus Kontextwerte für when-Klauseln.

use crate::core::types::{FfmpegInfo, MediaAsset};

pub const WORKSPACE_IDS: [&str; 6] = ["media", "edit", "color", "effects", "audio", "graphics"];

pub fn workspace_name(id: &str) -> &'static str {
    match id {
        "media" => "Medien",
        "edit" => "Schnitt",
        "color" => "Farbe",
        "effects" => "Effekte",
        "audio" => "Audio",
        "graphics" => "Grafik",
        _ => "?",
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DialogId {
    Shortcuts,
    Export,
    Settings,
    /// Wizard zum Wiederverknüpfen fehlender Medien.
    Relink,
}

pub type TimelineTool = &'static str;

pub const TOOLS: [TimelineTool; 8] = [
    "select", "razor", "ripple", "rolling", "slip", "slide", "hand", "zoom",
];

/// Kanonische deutsche Werkzeug-Bezeichnungen (StatusBar, Tooltips).
pub fn tool_label(tool: &str) -> &'static str {
    match tool {
        "select" => "Auswahl",
        "razor" => "Rasierklinge",
        "ripple" => "Ripple-Trimmen",
        "rolling" => "Rollen-Trimmen",
        "slip" => "Slip",
        "slide" => "Slide",
        "hand" => "Hand",
        "zoom" => "Zoom",
        _ => tool_command_title(tool),
    }
}

/// Command-Titel der Werkzeuge für Registry und Befehlspalette.
pub fn tool_command_title(tool: &str) -> &'static str {
    match tool {
        "select" => "Auswahlwerkzeug",
        "razor" => "Rasierklingen-Werkzeug",
        "ripple" => "Ripple-Trimmen-Werkzeug",
        "rolling" => "Rollen-Trimmen-Werkzeug",
        "slip" => "Slip-Werkzeug",
        "slide" => "Slide-Werkzeug",
        "hand" => "Hand-Werkzeug",
        "zoom" => "Zoom-Werkzeug",
        _ => "?",
    }
}

/// Anzeigedauer transienter Statusmeldungen.
const STATUS_MESSAGE_TIMEOUT: f64 = 6.0;

pub struct AppStore {
    pub active_workspace: String,
    pub open_dialog: Option<DialogId>,
    pub command_palette_open: bool,
    pub active_tool: TimelineTool,
    pub status_message: Option<String>,
    status_deadline: f64,
    pub ffmpeg: Option<FfmpegInfo>,
    /// Verfügbare ffmpeg-Encoder (None = noch nicht erfragt).
    pub encoders: Option<std::collections::HashSet<String>>,
    /// Fokussiertes Panel (Kontext-Key `panel`) — zuletzt angeklicktes Panel.
    pub focused_panel: String,
}

impl Default for AppStore {
    fn default() -> Self {
        AppStore {
            active_workspace: "edit".into(),
            open_dialog: None,
            command_palette_open: false,
            active_tool: "select",
            status_message: None,
            status_deadline: 0.0,
            ffmpeg: None,
            encoders: None,
            focused_panel: String::new(),
        }
    }
}

impl AppStore {
    pub fn set_status_message(&mut self, message: Option<String>, now: f64) {
        self.status_deadline = now + STATUS_MESSAGE_TIMEOUT;
        self.status_message = message;
    }

    /// Pro Frame: transiente Meldung nach 6 s wieder auf „Bereit“.
    pub fn tick_status(&mut self, now: f64) {
        if self.status_message.is_some() && now >= self.status_deadline {
            self.status_message = None;
        }
    }
}

#[derive(Default)]
pub struct MediaStore {
    pub assets: Vec<MediaAsset>,
    pub selected_asset_ids: Vec<String>,
    pub importing: bool,
    /// Waveform-Peaks je Asset (None = Extraktion fehlgeschlagen).
    pub waveforms: std::collections::HashMap<String, Option<Vec<f32>>>,
    /// Zählt Bestandsänderungen (Import/Entfernen/Relink) fürs Dirty-Tracking.
    pub revision: u64,
}

impl MediaStore {
    pub fn asset(&self, id: &str) -> Option<&MediaAsset> {
        self.assets.iter().find(|a| a.id == id)
    }

    pub fn select(&mut self, ids: Vec<String>) {
        self.selected_asset_ids = ids;
    }

    pub fn add_asset(&mut self, asset: MediaAsset) {
        self.assets.push(asset);
        self.revision += 1;
    }

    pub fn remove_assets(&mut self, ids: &[String]) {
        self.assets.retain(|a| !ids.contains(&a.id));
        self.selected_asset_ids.retain(|id| !ids.contains(id));
        self.revision += 1;
    }

    /// Anzahl der Assets, deren Quelldatei fehlt.
    pub fn offline_count(&self) -> usize {
        self.assets.iter().filter(|a| a.offline).count()
    }

    /// Neue Quelldatei für ein Asset übernehmen (Relink): Pfad/Metadaten
    /// ersetzen, Anzeigename bleibt erhalten, Waveform wird neu extrahiert.
    pub fn relink_asset(
        &mut self,
        asset_id: &str,
        path: String,
        info: crate::core::types::MediaInfo,
        thumbnail_path: Option<String>,
    ) -> bool {
        let Some(asset) = self.assets.iter_mut().find(|a| a.id == asset_id) else {
            return false;
        };
        asset.path = path;
        asset.info = info;
        if thumbnail_path.is_some() {
            asset.thumbnail_path = thumbnail_path;
        }
        asset.offline = false;
        self.waveforms.remove(asset_id);
        self.revision += 1;
        true
    }
}

/// Wiedergabe-Zustand eines Monitors (Quellmonitor; Programm nutzt die Timeline).
#[derive(Default)]
pub struct SourcePlayerState {
    pub position: f64,
    pub playing: bool,
    pub rate: f64,
    pub in_mark: Option<f64>,
    pub out_mark: Option<f64>,
    pub looping: bool,
}

pub struct PlaybackStore {
    /// Asset, das aktuell im Quellmonitor geladen ist.
    pub source_asset_id: Option<String>,
    pub source: SourcePlayerState,
    /// Programm (Timeline): Position = timeline.playhead_sec.
    pub program_playing: bool,
    pub program_rate: f64,
}

impl Default for PlaybackStore {
    fn default() -> Self {
        PlaybackStore {
            source_asset_id: None,
            source: SourcePlayerState {
                rate: 1.0,
                ..Default::default()
            },
            program_playing: false,
            program_rate: 1.0,
        }
    }
}

/// Laufzeit-Pegel der Audio-Engine (nicht persistiert): Spitzenwerte des
/// zuletzt gemischten Blocks, linear (1.0 = 0 dBFS), vor dem Hard-Clip
/// gemessen — Werte > 1 zeigen Übersteuerung an.
#[derive(Default)]
pub struct AudioStore {
    /// Peak L/R pro Audio-Spur (track_id), nach Spur-Gain/Pan.
    pub track_levels: std::collections::HashMap<String, [f32; 2]>,
    /// Peak L/R der Summe nach Master-Gain.
    pub master_level: [f32; 2],
}

/// Wiedergabeauflösung: 1 = voll, darunter kleinerer Offscreen-Render.
pub const PREVIEW_SCALES: [(f64, &str); 4] =
    [(1.0, "Voll"), (0.5, "1/2"), (0.25, "1/4"), (0.125, "1/8")];

pub struct MonitorStore {
    pub source_scale: f64,
    pub program_scale: f64,
}

impl Default for MonitorStore {
    fn default() -> Self {
        MonitorStore {
            source_scale: 1.0,
            program_scale: 1.0,
        }
    }
}
