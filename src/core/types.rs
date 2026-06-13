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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Video,
    Audio,
    Image,
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
    /// Quelldatei aktuell nicht auffindbar (wird beim Projektladen geprüft).
    #[serde(default)]
    pub offline: bool,
    /// Asset-/Quell-Marker in Quell-Sekunden (Quellmonitor). Werden beim
    /// Einfügen in die Timeline in Clip-Marker übernommen (Formatversion 6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<crate::core::marker::Marker>,
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
