//! Thumbnail-Erzeugung in den App-Cache (mit Hash-basiertem Datei-Cache).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use tauri::Manager;

use super::error::{Error, Result};
use super::{locate, stderr_tail};

/// Eindeutigkeit für parallele Temp-Dateien innerhalb des Prozesses.
static TMP_NONCE: AtomicU64 = AtomicU64::new(0);

pub async fn generate_thumbnail(
    app: &tauri::AppHandle,
    path: &str,
    time_sec: f64,
    max_width: u32,
) -> Result<String> {
    let time_sec = time_sec.max(0.0);

    // Cache-Key über Pfad + mtime + Zeit + Breite
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    if let Ok(mtime) = std::fs::metadata(path).and_then(|m| m.modified()) {
        mtime.hash(&mut hasher);
    }
    time_sec.to_bits().hash(&mut hasher);
    max_width.hash(&mut hasher);
    let hash = hasher.finish();

    let dir = app.path().app_cache_dir()?.join("thumbnails");
    let out = dir.join(format!("{hash:016x}.jpg"));
    if out.exists() {
        return Ok(out.to_string_lossy().into_owned());
    }
    std::fs::create_dir_all(&dir)?;

    // Erst in eine Temp-Datei rendern, bei Erfolg atomar umbenennen — so wird
    // ein abgebrochener Schreibvorgang nie zum Cache-Treffer. Die .jpg-Endung
    // bleibt nötig, damit ffmpeg den Muxer aus dem Dateinamen ableiten kann.
    let nonce = TMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!("{hash:016x}.tmp-{}-{nonce}.jpg", std::process::id()));

    // Komma im Filterausdruck escapen, sonst trennt es Filter im Graphen.
    let filter = format!("scale=w=min({max_width}\\,iw):h=-2");
    let output = locate::command(&locate::ffmpeg_bin())
        .args(["-y", "-v", "error", "-ss", &format!("{time_sec:.3}")])
        .args(["-i", path])
        .args(["-frames:v", "1", "-vf", &filter])
        .arg(&tmp)
        .output()
        .await?;

    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::FfmpegFailed(stderr_tail(&output.stderr)));
    }
    if let Err(err) = std::fs::rename(&tmp, &out) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err.into());
    }
    Ok(out.to_string_lossy().into_owned())
}
