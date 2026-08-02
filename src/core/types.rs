//! Medien-Typen (ffprobe-Vertrag) und ID-Erzeugung.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FfmpegInfo {
    pub available: bool,
    pub version: Option<String>,
    pub ffmpeg_path: Option<String>,
    pub ffprobe_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoStreamInfo {
    pub index: u32,
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub pix_fmt: Option<String>,
    pub bitrate: Option<u64>,
    /// Bittiefe pro Farbkanal (aus `pix_fmt`/`bits_per_raw_sample` abgeleitet);
    /// 8, wenn unbekannt. >8 ⇒ Decode in 16 Bit (rgba64le) statt 8 (rgba), damit
    /// Log-/HDR-Material ohne Banding durch die f32-Pipeline läuft.
    #[serde(default = "default_bit_depth")]
    pub bit_depth: u32,
    /// Farb-Transfercharakteristik (ffprobe `color_transfer`, z. B. `bt709`,
    /// `smpte2084` = PQ, `arib-std-b67` = HLG). `None` = unbekannt/unspezifiziert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_transfer: Option<String>,
    /// Farb-Primärvalenzen (ffprobe `color_primaries`, z. B. `bt709`, `bt2020`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_primaries: Option<String>,
    /// Matrixkoeffizienten/Farbraum (ffprobe `color_space`, z. B. `bt709`,
    /// `bt2020nc`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_space: Option<String>,
    /// Signalbereich (ffprobe `color_range`: `tv` = limited, `pc` = full).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_range: Option<String>,
}

fn default_bit_depth() -> u32 {
    8
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioStreamInfo {
    pub index: u32,
    pub codec: String,
    pub channels: u32,
    pub sample_rate: u32,
    pub bitrate: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    pub path: String,
    pub file_name: String,
    pub container: String,
    pub duration_sec: f64,
    pub size_bytes: u64,
    pub video: Vec<VideoStreamInfo>,
    pub audio: Vec<AudioStreamInfo>,
    /// Aufnahmedatum als Unix-Sekunden, aus `format.tags.creation_time` der
    /// ffprobe-Ausgabe (falls vorhanden). `default` hält Altprojekte lesbar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Video,
    Audio,
    Image,
}

/// Eine nummerierte Bildsequenz (VFX-Render: `render_0001.png`, `render_0002.png`,
/// …) als EIN Asset. Das Asset trägt `kind = Video` (endliche Dauer, normales
/// Seeking/Trimmen), `path` zeigt auf den ERSTEN realen Frame (Offline-/Relink-/
/// Thumbnail-tauglich). Wiedergabe und Export dekodieren über das printf-Muster
/// via ffmpeg-image2-Demuxer (`-framerate`/`-start_number`). Die Bildrate liegt
/// in `info.video[0].fps`, die Dauer in `info.duration_sec` (= `count / fps`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSequence {
    /// printf-Muster mit `%0Nd`, z. B. `/renders/shot_%04d.exr` (absolut).
    pub pattern: String,
    /// Nummer des ersten Frames (ffmpeg `-start_number`).
    pub start: u64,
    /// Anzahl (zusammenhängender) Frames der Folge.
    pub count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaAsset {
    pub id: String,
    pub path: String,
    pub name: String,
    pub kind: MediaKind,
    pub info: MediaInfo,
    /// Pfad zu einem generierten Vorschaubild im App-Cache, falls vorhanden.
    pub thumbnail_path: Option<String>,
    pub imported_at: f64,
    /// Bin (Ordner), in dem das Asset liegt. `default` ⇒ Wurzel; Altprojekte
    /// ohne Feld landen damit automatisch im Root-Bin (Formatversion 9).
    #[serde(default = "default_bin_id")]
    pub bin_id: String,
    /// Farbetikett (Premiere-Label) — rein organisatorisch, optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<crate::core::bin::MediaLabel>,
    /// Quelldatei aktuell nicht auffindbar (wird beim Projektladen geprüft).
    #[serde(default)]
    pub offline: bool,
    /// Asset-/Quell-Marker in Quell-Sekunden (Quellmonitor). Werden beim
    /// Einfügen in die Timeline in Clip-Marker übernommen (Formatversion 6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<crate::core::marker::Marker>,
    /// Pfad zu einer erzeugten Proxy-Datei (leichtgewichtiger Transcode für die
    /// Vorschau). None = kein Proxy. Persistiert ab Formatversion 10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_path: Option<String>,
    /// mtime der ORIGINALdatei zum Zeitpunkt der Proxy-Erzeugung (Unix-Sekunden).
    /// Ist die Quelle später neuer, gilt der Proxy als veraltet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_src_mtime: Option<f64>,
    /// Laufzeit-Flag: Proxy-Pfad gesetzt, aber Datei fehlt/ist veraltet (beim
    /// Laden + bei Proxy-Aktionen geprüft). Nicht persistiert.
    #[serde(skip)]
    pub proxy_offline: bool,
    /// Nummerierte Bildsequenz (VFX-Render) — `path` zeigt auf den ersten Frame,
    /// dekodiert wird über `pattern`. None = normales Einzelmedium. Persistiert
    /// ab Formatversion 21.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_seq: Option<ImageSequence>,
    /// Felder einer NEUEREN Editron-Version, die dieser Build noch nicht kennt.
    /// Werden beim Speichern unverändert wieder herausgeschrieben (Vorwärts-
    /// kompatibilität, siehe `core::project::ProjectFile::extra`).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl MediaAsset {
    /// Gültiger Proxy vorhanden? (Pfad gesetzt und Datei aktuell nutzbar.)
    pub fn has_valid_proxy(&self) -> bool {
        self.proxy_path.is_some() && !self.proxy_offline
    }

    /// Pfad, aus dem die VORSCHAU dekodiert werden soll: bei aktivem
    /// Proxy-Modus und gültigem Proxy die Proxy-Datei, sonst das Original.
    /// NIE im Export verwenden — der Export nimmt immer `path`.
    pub fn decode_path(&self, use_proxy: bool) -> &str {
        if use_proxy {
            if let Some(p) = &self.proxy_path {
                if !self.proxy_offline {
                    return p;
                }
            }
        }
        &self.path
    }

    /// Lässt sich das Asset in der Vorschau abspielen? Original online ODER —
    /// bei aktivem Proxy-Modus — ein gültiger Proxy vorhanden (Premiere zeigt
    /// dann den Proxy, obwohl das Original fehlt).
    pub fn preview_playable(&self, use_proxy: bool) -> bool {
        !self.offline || (use_proxy && self.has_valid_proxy())
    }
}

fn default_bin_id() -> String {
    crate::core::bin::ROOT_BIN_ID.to_string()
}

/// Einfacher eindeutiger ID-Generator (crypto.randomUUID()-Ersatz).
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:016x}-{n:08x}")
}
