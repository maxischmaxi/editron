//! Maschinen-/nutzergebundene Performance-Einstellungen: Hardware-Decode,
//! RAM-Budget des Scrub-Frame-Caches und Codec/Ablage des Sequenz-Render-
//! Caches. Persistiert nach `~/.config/editron/settings.json` — bewusst NICHT
//! in der `.etron`-Projektdatei, da geräteabhängig (ein Projekt wandert
//! zwischen Rechnern mit unterschiedlicher Hardware/RAM). Env-Variablen
//! überschreiben die geladenen Werte (`EDITRON_HWACCEL`,
//! `EDITRON_FRAME_CACHE_MB`, `EDITRON_RENDER_CACHE_DIR`,
//! `EDITRON_RENDER_CACHE_CODEC`), gespeichert wird aber nur, was der Nutzer
//! aktiv umschaltet.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::PathBuf;

/// Codec des Sequenz-Render-Caches. Intra-Frame-Codecs (jeder Frame einzeln
/// dekodierbar) sind Pflicht, damit der Cache bei Wiedergabe sofort an jeder
/// Stelle seekbar ist — wie bei Premiere/Resolve-Previews.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum RenderCacheCodec {
    /// Apple ProRes 422 Proxy (`prores_ks -profile:v 0`) — schnell, intra,
    /// breit unterstützt. Standard.
    #[default]
    ProresProxy,
    /// Avid DNxHR LB (`dnxhd -profile:v dnxhr_lb`).
    DnxhrLb,
    /// H.264 ultraschnell, reine Keyframes (`libx264 -preset ultrafast -g 1`)
    /// — kleinste Dateien, aber qualitativ unter ProRes.
    H264Fast,
}

impl RenderCacheCodec {
    /// Containerendung der Cache-Dateien.
    pub fn ext(self) -> &'static str {
        match self {
            RenderCacheCodec::ProresProxy | RenderCacheCodec::DnxhrLb => "mov",
            RenderCacheCodec::H264Fast => "mp4",
        }
    }

    /// ffmpeg-Encoder-Argumente für den Cache (rohes RGBA rein → Datei raus).
    /// Kein Audio — der Render-Cache deckt nur das Bild ab.
    pub fn encode_args(self) -> Vec<&'static str> {
        match self {
            RenderCacheCodec::ProresProxy => {
                vec!["-c:v", "prores_ks", "-profile:v", "0", "-pix_fmt", "yuv422p10le"]
            }
            RenderCacheCodec::DnxhrLb => {
                vec!["-c:v", "dnxhd", "-profile:v", "dnxhr_lb", "-pix_fmt", "yuv422p"]
            }
            RenderCacheCodec::H264Fast => vec![
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "1",
                "-crf",
                "18",
                "-pix_fmt",
                "yuv420p",
            ],
        }
    }

    fn from_key(s: &str) -> Option<RenderCacheCodec> {
        match s.trim().to_ascii_lowercase().as_str() {
            "prores" | "proresproxy" | "proxy" => Some(RenderCacheCodec::ProresProxy),
            "dnxhr" | "dnxhrlb" | "dnxhr_lb" => Some(RenderCacheCodec::DnxhrLb),
            "h264" | "h264fast" | "x264" => Some(RenderCacheCodec::H264Fast),
            _ => None,
        }
    }
}

/// Standard-RAM-Budget des Scrub-Frame-Caches (Megabyte).
pub const DEFAULT_FRAME_CACHE_MB: u64 = 2048;

// --------------------------------------------------------------- Autosave
// Zeitgesteuertes Autosave mit Versionshistorie (Premiere/Resolve-Niveau:
// verlorene Arbeit ist inakzeptabel). Geräteunabhängige Defaults; die
// eigentlichen Versionskopien landen pro Projekt im `.etron-autosave`-Ordner
// neben der Projektdatei (siehe `core::autosave`).

/// Erlaubter Bereich des Autosave-Intervalls in Minuten.
pub const AUTOSAVE_INTERVAL_MIN: u32 = 1;
pub const AUTOSAVE_INTERVAL_MAX: u32 = 120;
/// Erlaubter Bereich der aufbewahrten Versionen.
pub const AUTOSAVE_VERSIONS_MIN: u32 = 1;
pub const AUTOSAVE_VERSIONS_MAX: u32 = 200;
/// Standard-Intervall (Minuten) und -Versionsanzahl.
pub const DEFAULT_AUTOSAVE_INTERVAL_MIN: u32 = 5;
pub const DEFAULT_AUTOSAVE_VERSIONS: u32 = 20;

fn default_autosave_enabled() -> bool {
    true
}
fn default_autosave_interval() -> u32 {
    DEFAULT_AUTOSAVE_INTERVAL_MIN
}
fn default_autosave_versions() -> u32 {
    DEFAULT_AUTOSAVE_VERSIONS
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutosaveSettings {
    /// Zeitgesteuertes Autosave aktiv.
    #[serde(default = "default_autosave_enabled")]
    pub enabled: bool,
    /// Intervall zwischen zwei Versionen, in Minuten.
    #[serde(default = "default_autosave_interval")]
    pub interval_min: u32,
    /// Maximale Anzahl aufbewahrter Versionen je Projekt (älteste werden
    /// rotiert).
    #[serde(default = "default_autosave_versions")]
    pub max_versions: u32,
}

impl Default for AutosaveSettings {
    fn default() -> Self {
        AutosaveSettings {
            enabled: default_autosave_enabled(),
            interval_min: default_autosave_interval(),
            max_versions: default_autosave_versions(),
        }
    }
}

impl AutosaveSettings {
    /// Werte auf gültige Bereiche klemmen (gegen manipulierte JSON-Dateien).
    pub fn clamped(&self) -> AutosaveSettings {
        AutosaveSettings {
            enabled: self.enabled,
            interval_min: self.interval_min.clamp(AUTOSAVE_INTERVAL_MIN, AUTOSAVE_INTERVAL_MAX),
            max_versions: self.max_versions.clamp(AUTOSAVE_VERSIONS_MIN, AUTOSAVE_VERSIONS_MAX),
        }
    }

    /// Intervall in Sekunden (für den Mainloop-Timer).
    pub fn interval_secs(&self) -> f64 {
        (self.interval_min.clamp(AUTOSAVE_INTERVAL_MIN, AUTOSAVE_INTERVAL_MAX) as f64) * 60.0
    }
}

/// Standard-UI-/Menü-Sprache. Aktuell nur Deutsch ausgeliefert; das Feld ist
/// für künftige Lokalisierung vorbereitet.
pub const DEFAULT_LANGUAGE: &str = "de";

fn default_language() -> String {
    DEFAULT_LANGUAGE.to_string()
}

/// Standard-Vorschau-Auflösung (Wiedergabe-Skalierung). 1,0 = volle Auflösung.
fn default_preview_scale() -> f64 {
    1.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    /// Hardware-Decode aktiv (mit automatischem Software-Fallback bei Fehlern).
    #[serde(default)]
    pub hwaccel: bool,
    /// Erzwungene hwaccel-Methode (`vaapi`/`cuda`/`videotoolbox`/…). `None` =
    /// beste automatisch erkannte. Wird i. d. R. nur per Env gesetzt.
    #[serde(default)]
    pub hwaccel_method: Option<String>,
    /// RAM-Budget des Scrub-Frame-Caches in Megabyte.
    #[serde(default = "default_cache_mb")]
    pub frame_cache_budget_mb: u64,
    /// Verzeichnis für Render-Cache-Dateien (`None` = App-Cache-Verzeichnis).
    #[serde(default)]
    pub render_cache_dir: Option<String>,
    /// Codec des Sequenz-Render-Caches.
    #[serde(default)]
    pub render_cache_codec: RenderCacheCodec,
    /// Manueller HiDPI-Faktor (UI-Skalierung). `None` = automatisch aus der
    /// Monitor-DPI (`get_window_scale_dpi`). Ein gesetzter Wert übersteuert die
    /// Erkennung (z. B. für WMs ohne korrekte DPI-Meldung). Maschinengebunden —
    /// gehört in die `settings.json`, nicht in die `.etron`.
    #[serde(default)]
    pub ui_scale: Option<f32>,
    /// UI-/Menü-Sprache (ISO-639-Kürzel; aktuell nur `de`). Vorbereitet für
    /// künftige Lokalisierung — der Dialog bietet derzeit nur Deutsch an.
    #[serde(default = "default_language")]
    pub language: String,
    /// Standard-Wiedergabe-Auflösung (Vorschau-Skalierung) für Programm- und
    /// Quellmonitor. 1,0 = voll, 0,5 = halb, 0,25 = viertel, 0,125 = achtel.
    /// Wird beim Start auf die Monitore angewandt (Performance auf schwacher
    /// Hardware). Geräteabhängig ⇒ gehört in die settings.json.
    #[serde(default = "default_preview_scale")]
    pub default_preview_scale: f64,
    /// Manueller ffmpeg-Pfad (`None` = im PATH suchen). Übersteuert die
    /// automatische Suche, falls ffmpeg nicht im PATH liegt — wie der
    /// „FFmpeg-Speicherort“ in Resolve.
    #[serde(default)]
    pub ffmpeg_path: Option<String>,
    /// Manueller ffprobe-Pfad (`None` = im PATH suchen).
    #[serde(default)]
    pub ffprobe_path: Option<String>,
    /// Zeitgesteuertes Autosave mit Versionshistorie.
    #[serde(default)]
    pub autosave: AutosaveSettings,
    /// Felder einer NEUEREN Editron-Version, die diese Version noch nicht
    /// kennt. Sie werden beim Speichern unverändert wieder herausgeschrieben,
    /// damit ein älterer Build die settings.json eines neueren nicht
    /// stillschweigend kaputtschreibt (Vorwärtskompatibilität).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Grenzen des UI-Scale (unter 1.0 würde die UI unscharf/winzig, über 4.0 ist
/// jenseits realer Displays).
pub const UI_SCALE_MIN: f32 = 0.75;
pub const UI_SCALE_MAX: f32 = 4.0;

fn default_cache_mb() -> u64 {
    DEFAULT_FRAME_CACHE_MB
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            hwaccel: false,
            hwaccel_method: None,
            frame_cache_budget_mb: DEFAULT_FRAME_CACHE_MB,
            render_cache_dir: None,
            render_cache_codec: RenderCacheCodec::default(),
            ui_scale: None,
            language: default_language(),
            default_preview_scale: default_preview_scale(),
            ffmpeg_path: None,
            ffprobe_path: None,
            autosave: AutosaveSettings::default(),
            extra: Map::new(),
        }
    }
}

fn settings_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("editron")
        .join("settings.json")
}

impl AppSettings {
    /// Aus der Konfigdatei laden, dann Env-Overrides anwenden.
    pub fn load() -> AppSettings {
        let mut s = std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|raw| serde_json::from_str::<AppSettings>(&raw).ok())
            .unwrap_or_default();
        s.apply_env_overrides();
        s.sanitize();
        s
    }

    /// Werte auf gültige Bereiche klemmen (gegen manipulierte/veraltete JSON).
    fn sanitize(&mut self) {
        self.frame_cache_budget_mb = self.frame_cache_budget_mb.clamp(64, 65536);
        self.autosave = self.autosave.clamped();
        if !(self.default_preview_scale.is_finite()
            && self.default_preview_scale > 0.0
            && self.default_preview_scale <= 1.0)
        {
            self.default_preview_scale = 1.0;
        }
        if self.language.trim().is_empty() {
            self.language = default_language();
        }
        // Leere Pfad-Strings wie „nicht gesetzt“ behandeln.
        if self.ffmpeg_path.as_deref().is_some_and(|p| p.trim().is_empty()) {
            self.ffmpeg_path = None;
        }
        if self.ffprobe_path.as_deref().is_some_and(|p| p.trim().is_empty()) {
            self.ffprobe_path = None;
        }
    }

    /// Persistieren. Nur aufrufen, wenn der Nutzer eine Einstellung aktiv
    /// geändert hat (sonst würden Env-Overrides in die Datei einsickern).
    pub fn save(&self) {
        let path = settings_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Frame-Cache-Budget in Bytes.
    pub fn frame_cache_budget_bytes(&self) -> usize {
        (self.frame_cache_budget_mb as usize).saturating_mul(1024 * 1024)
    }

    /// Effektiver UI-Scale: manueller Override falls gesetzt, sonst der vom
    /// Fenster gemeldete DPI-Faktor. Immer auf einen sinnvollen Bereich
    /// geklemmt.
    pub fn resolve_ui_scale(&self, detected: f32) -> f32 {
        let s = self.ui_scale.filter(|v| v.is_finite() && *v > 0.0).unwrap_or(detected);
        let s = if s.is_finite() && s > 0.0 { s } else { 1.0 };
        s.clamp(UI_SCALE_MIN, UI_SCALE_MAX)
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("EDITRON_HWACCEL") {
            let key = v.trim().to_ascii_lowercase();
            match key.as_str() {
                "" => {}
                "0" | "off" | "false" | "no" | "none" => {
                    self.hwaccel = false;
                    self.hwaccel_method = None;
                }
                "1" | "on" | "true" | "yes" | "auto" => {
                    self.hwaccel = true;
                    self.hwaccel_method = None;
                }
                // Konkrete Methode erzwingen (vaapi/cuda/nvdec/videotoolbox/qsv/dxva2/d3d11va …).
                method => {
                    self.hwaccel = true;
                    self.hwaccel_method = Some(method.to_string());
                }
            }
        }
        if let Some(mb) = std::env::var("EDITRON_FRAME_CACHE_MB")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
        {
            self.frame_cache_budget_mb = mb;
        }
        if let Ok(dir) = std::env::var("EDITRON_RENDER_CACHE_DIR") {
            if !dir.trim().is_empty() {
                self.render_cache_dir = Some(dir);
            }
        }
        if let Some(codec) = std::env::var("EDITRON_RENDER_CACHE_CODEC")
            .ok()
            .and_then(|s| RenderCacheCodec::from_key(&s))
        {
            self.render_cache_codec = codec;
        }
        // EDITRON_UI_SCALE: fester HiDPI-Faktor (z. B. `1.5`, `2`), `auto`/leer
        // schaltet auf automatische DPI-Erkennung zurück.
        if let Ok(v) = std::env::var("EDITRON_UI_SCALE") {
            let key = v.trim().to_ascii_lowercase();
            match key.as_str() {
                "" | "auto" => self.ui_scale = None,
                _ => {
                    if let Ok(f) = key.replace(',', ".").parse::<f32>() {
                        if f.is_finite() && f > 0.0 {
                            self.ui_scale = Some(f.clamp(UI_SCALE_MIN, UI_SCALE_MAX));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_budget_is_two_gb() {
        let s = AppSettings::default();
        assert_eq!(s.frame_cache_budget_mb, 2048);
        assert_eq!(s.frame_cache_budget_bytes(), 2048 * 1024 * 1024);
        assert!(!s.hwaccel);
    }

    #[test]
    fn autosave_defaults_are_professional() {
        let a = AutosaveSettings::default();
        assert!(a.enabled);
        assert_eq!(a.interval_min, 5);
        assert_eq!(a.max_versions, 20);
        assert_eq!(a.interval_secs(), 300.0);
    }

    #[test]
    fn autosave_clamps_out_of_range() {
        let a = AutosaveSettings { enabled: true, interval_min: 0, max_versions: 9999 }.clamped();
        assert_eq!(a.interval_min, AUTOSAVE_INTERVAL_MIN);
        assert_eq!(a.max_versions, AUTOSAVE_VERSIONS_MAX);
    }

    #[test]
    fn codec_round_trips_via_key() {
        assert_eq!(
            RenderCacheCodec::from_key("ProRes"),
            Some(RenderCacheCodec::ProresProxy)
        );
        assert_eq!(
            RenderCacheCodec::from_key("dnxhr_lb"),
            Some(RenderCacheCodec::DnxhrLb)
        );
        assert_eq!(
            RenderCacheCodec::from_key("h264"),
            Some(RenderCacheCodec::H264Fast)
        );
        assert_eq!(RenderCacheCodec::from_key("nonsense"), None);
    }

    #[test]
    fn json_round_trip_with_missing_fields_uses_defaults() {
        // Alte/teilweise Konfig: fehlende Felder fallen auf Defaults zurück.
        let s: AppSettings = serde_json::from_str("{\"hwaccel\":true}").unwrap();
        assert!(s.hwaccel);
        assert_eq!(s.frame_cache_budget_mb, 2048);
        assert_eq!(s.render_cache_codec, RenderCacheCodec::ProresProxy);
        // Neue Felder erhalten ihre Defaults.
        assert_eq!(s.language, "de");
        assert_eq!(s.default_preview_scale, 1.0);
        assert!(s.autosave.enabled);
        assert_eq!(s.autosave.interval_min, 5);
        assert!(s.ffmpeg_path.is_none());
    }

    #[test]
    fn unknown_fields_survive_round_trip() {
        // Vorwärtskompatibilität: eine settings.json aus einer NEUEREN Version
        // (mit unbekannten Feldern) muss laden UND beim Wiederspeichern die
        // unbekannten Felder verlustfrei behalten.
        let raw = r#"{
            "hwaccel": true,
            "autosave": { "intervalMin": 7 },
            "futureFeature": { "nested": [1, 2, 3] },
            "experimentalFlag": true
        }"#;
        let s: AppSettings = serde_json::from_str(raw).unwrap();
        // Bekannte Felder gelesen, fehlende mit Default.
        assert!(s.hwaccel);
        assert_eq!(s.autosave.interval_min, 7);
        assert_eq!(s.autosave.max_versions, 20);
        // Unbekannte Felder im `extra`-Bucket aufbewahrt.
        assert!(s.extra.contains_key("futureFeature"));
        assert!(s.extra.contains_key("experimentalFlag"));

        // Re-Serialisierung gibt die unbekannten Felder wieder her.
        let json = serde_json::to_string(&s).unwrap();
        let reparsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(reparsed["futureFeature"]["nested"][2], 3);
        assert_eq!(reparsed["experimentalFlag"], true);
        // Und ein zweiter Durchlauf bleibt stabil.
        let s2: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(s, s2);
    }

    #[test]
    fn sanitize_clamps_preview_scale_and_autosave() {
        let raw = r#"{ "defaultPreviewScale": 5.0, "autosave": { "maxVersions": 100000 } }"#;
        let mut s: AppSettings = serde_json::from_str(raw).unwrap();
        s.sanitize();
        assert_eq!(s.default_preview_scale, 1.0);
        assert_eq!(s.autosave.max_versions, AUTOSAVE_VERSIONS_MAX);
    }
}
