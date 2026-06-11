//! Medien-Engine: ffmpeg/ffprobe-Discovery, Import-Pipeline (Datei-Dialog →
//! probe → Thumbnail), Waveform-Extraktion — alles in Worker-Threads,
//! Ergebnisse als Events zurück in den UI-Thread.

use crate::core::types::{new_id, FfmpegInfo, MediaAsset, MediaInfo, MediaKind};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

pub const VIDEO_EXT: [&str; 8] = ["mp4", "mov", "mkv", "webm", "avi", "m4v", "mts", "mxf"];
pub const AUDIO_EXT: [&str; 7] = ["wav", "mp3", "flac", "aac", "m4a", "ogg", "opus"];
pub const IMAGE_EXT: [&str; 8] = ["png", "jpg", "jpeg", "webp", "tif", "tiff", "bmp", "gif"];

pub enum ServiceEvent {
    FfmpegInfo(FfmpegInfo),
    AssetImported(MediaAsset),
    ImportFinished { errors: Vec<String> },
    ImportCancelled,
    WaveformReady { asset_id: String, peaks: Vec<f32> },
    WaveformFailed { asset_id: String },
    /// Verfügbare ffmpeg-Encoder (für die Validierung im Export-Dialog).
    EncoderListReady(HashSet<String>),
    /// Fortschritt eines laufenden Sequenz-Exports.
    SequenceExportProgress {
        job_id: String,
        pct: f64,
        phase: crate::core::export::ExportPhase,
        frames_done: u64,
        frames_total: u64,
        render_fps: f64,
        eta_sec: Option<f64>,
    },
    /// Sequenz-Export beendet (Erfolg, Abbruch oder Fehler).
    SequenceExportDone {
        job_id: String,
        ok: bool,
        cancelled: bool,
        error: Option<String>,
        output: String,
    },
    /// Ziel im Speichern-Dialog gewählt (Export).
    ExportTargetPicked(Option<PathBuf>),
    /// Projektdatei im Öffnen-Dialog gewählt.
    ProjectOpenPicked(Option<PathBuf>),
    /// Ziel im Projekt-speichern-Dialog gewählt.
    ProjectSaveTargetPicked(Option<PathBuf>),
    /// Suchordner für den Relink-Wizard gewählt.
    RelinkFolderPicked(Option<PathBuf>),
    /// Ersatzdatei für ein einzelnes Medium manuell gewählt.
    RelinkManualPicked {
        asset_id: String,
        path: Option<PathBuf>,
    },
    /// Fortschritt der Ordnersuche (Drosselung im Worker).
    RelinkScanProgress { scanned_dirs: u64 },
    /// Medium erfolgreich neu verknüpft (inkl. frischer Metadaten).
    RelinkResolved {
        asset_id: String,
        path: String,
        info: MediaInfo,
        thumbnail_path: Option<String>,
    },
    /// Kandidat gefunden/gewählt, aber Probe fehlgeschlagen.
    RelinkFailed { asset_id: String, error: String },
    /// Ordnersuche beendet.
    RelinkScanFinished { cancelled: bool, unresolved: usize },
}

/// Fehlendes Medium als Suchauftrag für den Relink-Scan.
#[derive(Clone)]
pub struct RelinkTarget {
    pub asset_id: String,
    pub file_name: String,
    pub size_bytes: u64,
}

/// Laufender Sequenz-Export: Abbruch-Flag + Kindprozesse für hartes Beenden.
struct ExportJobHandle {
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    children: std::sync::Arc<Mutex<Vec<(u64, std::process::Child)>>>,
}

pub struct Services {
    tx: Sender<ServiceEvent>,
    rx: Receiver<ServiceEvent>,
    waveform_pending: Mutex<HashSet<String>>,
    next_job_id: std::sync::atomic::AtomicU64,
    jobs: Mutex<std::collections::HashMap<String, ExportJobHandle>>,
    /// Abbruch-Flag des laufenden Relink-Scans (neuer Scan ersetzt es).
    relink_cancel: Mutex<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
}

impl Services {
    pub fn new() -> Services {
        let (tx, rx) = channel();
        let s = Services {
            tx,
            rx,
            waveform_pending: Mutex::new(HashSet::new()),
            next_job_id: std::sync::atomic::AtomicU64::new(1),
            jobs: Mutex::new(std::collections::HashMap::new()),
            relink_cancel: Mutex::new(None),
        };
        // Binary-Discovery beim Start (Version) für die Statusanzeige.
        let tx = s.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(ServiceEvent::FfmpegInfo(ffmpeg_info()));
        });
        s
    }

    /// Pro Frame: alle eingetroffenen Worker-Ergebnisse abholen.
    pub fn poll(&self) -> Vec<ServiceEvent> {
        let mut events = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            if let ServiceEvent::WaveformReady { asset_id, .. }
            | ServiceEvent::WaveformFailed { asset_id } = &ev
            {
                self.waveform_pending.lock().unwrap().remove(asset_id);
            }
            if let ServiceEvent::SequenceExportDone { job_id, .. } = &ev {
                self.jobs.lock().unwrap().remove(job_id);
            }
            events.push(ev);
        }
        events
    }

    /// Import-Dialog in eigenem Thread (rfd blockiert sonst den Mainloop).
    pub fn open_import_dialog(&self) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let all_media: Vec<&str> = VIDEO_EXT
                .iter()
                .chain(AUDIO_EXT.iter())
                .chain(IMAGE_EXT.iter())
                .copied()
                .collect();
            let picked = rfd::FileDialog::new()
                .set_title("Medien importieren")
                .add_filter("Medien", &all_media)
                .add_filter("Alle Dateien", &["*"])
                .pick_files();
            match picked {
                Some(paths) if !paths.is_empty() => import_files(&tx, paths),
                _ => {
                    let _ = tx.send(ServiceEvent::ImportCancelled);
                }
            }
        });
    }

    /// Dateien direkt importieren (natives Drag&Drop ins Fenster).
    pub fn import_paths(&self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || import_files(&tx, paths));
    }

    /// Waveform-Peaks für ein Asset anfordern (dedupliziert).
    pub fn request_waveform(&self, asset_id: &str, path: &str, samples: u32) {
        let mut pending = self.waveform_pending.lock().unwrap();
        if !pending.insert(asset_id.to_string()) {
            return;
        }
        drop(pending);
        let tx = self.tx.clone();
        let asset_id = asset_id.to_string();
        let path = path.to_string();
        std::thread::spawn(move || {
            let ev = match extract_waveform(&path, samples) {
                Ok(peaks) => ServiceEvent::WaveformReady { asset_id, peaks },
                Err(err) => {
                    eprintln!("[waveform] {path}: {err}");
                    ServiceEvent::WaveformFailed { asset_id }
                }
            };
            let _ = tx.send(ev);
        });
    }

    pub fn reveal_in_file_manager(&self, path: &str) -> Result<(), String> {
        reveal_in_file_manager(path)
    }

    /// Speichern-Dialog für das Exportziel (eigener Thread).
    pub fn pick_export_target(&self, default_name: &str, ext: &str) {
        let tx = self.tx.clone();
        let default_name = default_name.to_string();
        let ext = ext.to_string();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title("Exportziel wählen")
                .set_file_name(&default_name)
                .add_filter(ext.to_uppercase(), &[ext.as_str()])
                .save_file();
            let _ = tx.send(ServiceEvent::ExportTargetPicked(picked));
        });
    }

    /// Sequenz-Export starten: Render-Worker-Thread mit Abbruch-Flag und
    /// Kindprozess-Registry (für hartes Beenden über `cancel_job`).
    pub fn start_sequence_export(
        &self,
        plan: crate::core::export::RenderPlan,
        settings: crate::core::export::ExportSettings,
    ) -> Result<String, String> {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        if settings.video.is_none() && settings.audio.is_none() {
            return Err("Weder Video noch Audio ausgewählt".to_string());
        }
        let job_id = format!("export-{}", self.next_job_id.fetch_add(1, Ordering::Relaxed));
        let cancel = Arc::new(AtomicBool::new(false));
        let children: Arc<Mutex<Vec<(u64, std::process::Child)>>> = Arc::new(Mutex::new(Vec::new()));
        self.jobs.lock().unwrap().insert(
            job_id.clone(),
            ExportJobHandle {
                cancel: Arc::clone(&cancel),
                children: Arc::clone(&children),
            },
        );

        let tx = self.tx.clone();
        let id = job_id.clone();
        std::thread::spawn(move || {
            crate::core::export::run_export_worker(id, plan, settings, tx, cancel, children);
        });
        Ok(job_id)
    }

    /// Bricht einen Export ab: Flag setzen und Kindprozesse killen, damit
    /// blockierende Pipe-Reads sofort enden. Unbekannte IDs sind ok.
    pub fn cancel_job(&self, job_id: &str) {
        let jobs = self.jobs.lock().unwrap();
        if let Some(handle) = jobs.get(job_id) {
            handle.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            let mut children = handle.children.lock().unwrap_or_else(|p| p.into_inner());
            for (_, child) in children.iter_mut() {
                let _ = child.kill();
            }
        }
    }

    /// Beim App-Ende: alle laufenden Exporte hart beenden (keine Waisen).
    pub fn cancel_all_jobs(&self) {
        let ids: Vec<String> = self.jobs.lock().unwrap().keys().cloned().collect();
        for id in ids {
            self.cancel_job(&id);
        }
    }

    /// Verfügbare Encoder einmalig erfragen (`ffmpeg -encoders`).
    pub fn request_encoder_list(&self) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let set = list_encoders();
            let _ = tx.send(ServiceEvent::EncoderListReady(set));
        });
    }

    // ------------------------------------------------------------- Projekt

    /// Öffnen-Dialog für Projektdateien (eigener Thread).
    pub fn pick_project_open(&self) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title("Projekt öffnen")
                .add_filter("Editron-Projekt", &[crate::core::project::PROJECT_EXT])
                .pick_file();
            let _ = tx.send(ServiceEvent::ProjectOpenPicked(picked));
        });
    }

    /// Speichern-Dialog für Projektdateien (eigener Thread).
    pub fn pick_project_save_target(&self, default_name: &str) {
        let tx = self.tx.clone();
        let default_name = default_name.to_string();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title("Projekt speichern")
                .set_file_name(&default_name)
                .add_filter("Editron-Projekt", &[crate::core::project::PROJECT_EXT])
                .save_file();
            let _ = tx.send(ServiceEvent::ProjectSaveTargetPicked(picked));
        });
    }

    // -------------------------------------------------------------- Relink

    /// Ordner-Dialog für die automatische Mediensuche.
    pub fn pick_relink_folder(&self) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title("Ordner durchsuchen")
                .pick_folder();
            let _ = tx.send(ServiceEvent::RelinkFolderPicked(picked));
        });
    }

    /// Datei-Dialog, um ein einzelnes Medium manuell neu zuzuweisen.
    pub fn pick_relink_file(&self, asset_id: &str) {
        let tx = self.tx.clone();
        let asset_id = asset_id.to_string();
        std::thread::spawn(move || {
            let all_media: Vec<&str> = VIDEO_EXT
                .iter()
                .chain(AUDIO_EXT.iter())
                .chain(IMAGE_EXT.iter())
                .copied()
                .collect();
            let picked = rfd::FileDialog::new()
                .set_title("Medium suchen")
                .add_filter("Medien", &all_media)
                .add_filter("Alle Dateien", &["*"])
                .pick_file();
            let _ = tx.send(ServiceEvent::RelinkManualPicked { asset_id, path: picked });
        });
    }

    /// Gewählte Ersatzdatei proben und als Relink-Ergebnis melden.
    pub fn resolve_relink(&self, asset_id: &str, path: PathBuf) {
        let tx = self.tx.clone();
        let asset_id = asset_id.to_string();
        std::thread::spawn(move || {
            send_relink_result(&tx, asset_id, &path);
        });
    }

    /// Rekursive Ordnersuche nach fehlenden Medien in einem Worker-Thread:
    /// Match per Dateiname (case-insensitive), Gleichstand entscheidet die
    /// Dateigröße. Gefundene Kandidaten werden direkt geprobt und einzeln
    /// als `RelinkResolved` gemeldet, damit die UI live aktualisiert.
    pub fn start_relink_scan(&self, targets: Vec<RelinkTarget>, root: PathBuf) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        // Laufenden Scan abbrechen, bevor der neue startet.
        self.cancel_relink_scan();
        let cancel = Arc::new(AtomicBool::new(false));
        *self.relink_cancel.lock().unwrap() = Some(Arc::clone(&cancel));

        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let total = targets.len();
            let progress_tx = tx.clone();
            let (matches, cancelled) =
                match_relink_candidates(&root, &targets, &cancel, |scanned_dirs| {
                    let _ = progress_tx.send(ServiceEvent::RelinkScanProgress { scanned_dirs });
                });

            // Jeden Treffer proben und einzeln melden (UI aktualisiert live).
            let mut resolved = 0usize;
            for (asset_id, path) in matches {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                if send_relink_result(&tx, asset_id, &path) {
                    resolved += 1;
                }
            }

            let _ = tx.send(ServiceEvent::RelinkScanFinished {
                cancelled,
                unresolved: total.saturating_sub(resolved),
            });
        });
    }

    pub fn cancel_relink_scan(&self) {
        if let Some(cancel) = self.relink_cancel.lock().unwrap().take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Reiner Scan + Zuordnung (ohne ffprobe, dadurch testbar): durchsucht den
/// Baum unter `root` per Tiefensuche (Symlinks werden übersprungen) nach den
/// Dateinamen der Suchaufträge. Zuordnung: exakter Größen-Match gewinnt,
/// sonst der erste Fund; jeder Kandidat wird höchstens einmal vergeben
/// (gleichnamige Medien aus verschiedenen Ordnern).
fn match_relink_candidates(
    root: &Path,
    targets: &[RelinkTarget],
    cancel: &std::sync::atomic::AtomicBool,
    mut on_progress: impl FnMut(u64),
) -> (Vec<(String, PathBuf)>, bool) {
    use std::sync::atomic::Ordering;

    let mut wanted: std::collections::HashMap<String, Vec<&RelinkTarget>> =
        std::collections::HashMap::new();
    for t in targets {
        wanted.entry(t.file_name.to_lowercase()).or_default().push(t);
    }

    let mut candidates: std::collections::HashMap<String, Vec<(PathBuf, u64)>> =
        std::collections::HashMap::new();
    let mut stack = vec![root.to_path_buf()];
    let mut scanned_dirs: u64 = 0;
    let mut cancelled = false;
    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        scanned_dirs += 1;
        if scanned_dirs % 64 == 0 {
            on_progress(scanned_dirs);
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else { continue };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(entry.path());
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if wanted.contains_key(&name) {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                candidates.entry(name).or_default().push((entry.path(), size));
            }
        }
    }

    let mut matches = Vec::new();
    if !cancelled {
        for (name, targets) in &wanted {
            let mut found = candidates.remove(name).unwrap_or_default();
            for target in targets {
                let pick = found
                    .iter()
                    .position(|(_, size)| *size == target.size_bytes)
                    .or(if found.is_empty() { None } else { Some(0) });
                let Some(idx) = pick else { continue };
                let (path, _) = found.remove(idx);
                matches.push((target.asset_id.clone(), path));
            }
        }
    }
    (matches, cancelled)
}

/// Ersatzdatei proben (+ Thumbnail) und Ergebnis-Event senden.
/// Liefert true bei Erfolg.
fn send_relink_result(tx: &Sender<ServiceEvent>, asset_id: String, path: &Path) -> bool {
    let path_str = path.to_string_lossy().into_owned();
    match probe_media(&path_str) {
        Ok(info) => {
            let kind = detect_kind(&path_str, &info);
            let thumbnail_path = if kind != MediaKind::Audio {
                let t = if kind == MediaKind::Image {
                    0.0
                } else {
                    (info.duration_sec * 0.25).min(1.0)
                };
                generate_thumbnail(&path_str, t, 320).ok()
            } else {
                None
            };
            let _ = tx.send(ServiceEvent::RelinkResolved {
                asset_id,
                path: path_str,
                info,
                thumbnail_path,
            });
            true
        }
        Err(error) => {
            let _ = tx.send(ServiceEvent::RelinkFailed { asset_id, error });
            false
        }
    }
}

// --------------------------------------------------------------- Discovery

/// Pfad zum ffmpeg-Binary: Env-Variable zuerst, sonst PATH.
pub fn ffmpeg_bin() -> String {
    std::env::var("EDITRON_FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".to_string())
}

pub fn ffprobe_bin() -> String {
    std::env::var("EDITRON_FFPROBE_PATH").unwrap_or_else(|_| "ffprobe".to_string())
}

/// Encoder-Namen aus `ffmpeg -encoders` (Zeilenformat ` V..... name  Beschreibung`).
fn list_encoders() -> HashSet<String> {
    let mut set = HashSet::new();
    let Ok(out) = Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-encoders"])
        .output()
    else {
        return set;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines().skip_while(|l| !l.contains("------")).skip(1) {
        let mut parts = line.split_whitespace();
        let (Some(_flags), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        set.insert(name.to_string());
    }
    set
}

fn ffmpeg_info() -> FfmpegInfo {
    let ffmpeg = ffmpeg_bin();
    let ffprobe = ffprobe_bin();
    match Command::new(&ffmpeg).arg("-version").output() {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let version = stdout.lines().next().map(|first| {
                match first.trim().strip_prefix("ffmpeg version ") {
                    Some(rest) => rest.split_whitespace().next().unwrap_or(rest).to_string(),
                    None => first.trim().to_string(),
                }
            });
            FfmpegInfo {
                available: true,
                version,
                ffmpeg_path: Some(ffmpeg),
                ffprobe_path: Some(ffprobe),
            }
        }
        _ => FfmpegInfo::default(),
    }
}

// ------------------------------------------------------------------ Import

fn detect_kind(path: &str, info: &MediaInfo) -> MediaKind {
    let ext = Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if IMAGE_EXT.contains(&ext.as_str()) {
        return MediaKind::Image;
    }
    if !info.video.is_empty() {
        return MediaKind::Video;
    }
    if !info.audio.is_empty() {
        return MediaKind::Audio;
    }
    MediaKind::Video
}

fn import_files(tx: &Sender<ServiceEvent>, paths: Vec<PathBuf>) {
    let mut errors: Vec<String> = Vec::new();
    for path_buf in paths {
        let path = path_buf.to_string_lossy().into_owned();
        match import_one(&path) {
            Ok(asset) => {
                let _ = tx.send(ServiceEvent::AssetImported(asset));
            }
            Err(err) => {
                eprintln!("[media] Import fehlgeschlagen: {path}: {err}");
                errors.push(
                    Path::new(&path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or(path),
                );
            }
        }
    }
    let _ = tx.send(ServiceEvent::ImportFinished { errors });
}

fn import_one(path: &str) -> Result<MediaAsset, String> {
    let info = probe_media(path)?;
    let kind = detect_kind(path, &info);
    let thumbnail_path = if kind != MediaKind::Audio {
        let t = if kind == MediaKind::Image {
            0.0
        } else {
            (info.duration_sec * 0.25).min(1.0)
        };
        generate_thumbnail(path, t, 320).ok()
    } else {
        None
    };
    Ok(MediaAsset {
        id: new_id(),
        path: path.to_string(),
        name: info.file_name.clone(),
        kind,
        info,
        thumbnail_path,
        imported_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0),
        offline: false,
    })
}

// ------------------------------------------------------------------- Probe

fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines
        .iter()
        .rev()
        .take(4)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Strukturierte Medienanalyse über `ffprobe -print_format json`.
pub fn probe_media(path: &str) -> Result<MediaInfo, String> {
    let output = Command::new(ffprobe_bin())
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe konnte nicht gestartet werden: {e}"))?;
    if !output.status.success() {
        return Err(format!("ffprobe: {}", stderr_tail(&output.stderr)));
    }

    let probe: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("ffprobe-JSON: {e}"))?;
    let streams = probe["streams"].as_array().cloned().unwrap_or_default();
    let format = &probe["format"];

    let parse_rate = |s: &str| -> Option<f64> {
        let v = match s.trim().split_once('/') {
            Some((num, den)) => {
                let num: f64 = num.trim().parse().ok()?;
                let den: f64 = den.trim().parse().ok()?;
                if den == 0.0 {
                    return None;
                }
                num / den
            }
            None => s.trim().parse().ok()?,
        };
        (v > 0.0).then_some(v)
    };
    let parse_u64 = |v: &serde_json::Value| -> Option<u64> {
        v.as_str().and_then(|s| s.trim().parse().ok()).or(v.as_u64())
    };

    // Cover-Art (attached_pic) zählt nicht als Video-Stream.
    let video_streams: Vec<&serde_json::Value> = streams
        .iter()
        .filter(|s| {
            s["codec_type"].as_str() == Some("video")
                && s["disposition"]["attached_pic"].as_i64().unwrap_or(0) != 1
        })
        .collect();
    let audio_streams: Vec<&serde_json::Value> = streams
        .iter()
        .filter(|s| s["codec_type"].as_str() == Some("audio"))
        .collect();

    let duration_sec = format["duration"]
        .as_str()
        .and_then(|d| d.trim().parse::<f64>().ok())
        .or_else(|| {
            video_streams
                .first()
                .and_then(|s| s["duration"].as_str())
                .and_then(|d| d.trim().parse::<f64>().ok())
        })
        .unwrap_or(0.0);

    let size_bytes = format["size"]
        .as_str()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or_else(|| std::fs::metadata(path).map(|m| m.len()).unwrap_or(0));

    Ok(MediaInfo {
        path: path.to_string(),
        file_name: Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string()),
        container: format["format_name"].as_str().unwrap_or_default().to_string(),
        duration_sec,
        size_bytes,
        video: video_streams
            .iter()
            .map(|s| crate::core::types::VideoStreamInfo {
                index: s["index"].as_u64().unwrap_or(0) as u32,
                codec: s["codec_name"].as_str().unwrap_or_default().to_string(),
                width: s["width"].as_u64().unwrap_or(0) as u32,
                height: s["height"].as_u64().unwrap_or(0) as u32,
                fps: s["avg_frame_rate"]
                    .as_str()
                    .and_then(parse_rate)
                    .or_else(|| s["r_frame_rate"].as_str().and_then(parse_rate))
                    .unwrap_or(0.0),
                pix_fmt: s["pix_fmt"].as_str().map(String::from),
                bitrate: parse_u64(&s["bit_rate"]),
            })
            .collect(),
        audio: audio_streams
            .iter()
            .map(|s| crate::core::types::AudioStreamInfo {
                index: s["index"].as_u64().unwrap_or(0) as u32,
                codec: s["codec_name"].as_str().unwrap_or_default().to_string(),
                channels: s["channels"].as_u64().unwrap_or(0) as u32,
                sample_rate: parse_u64(&s["sample_rate"]).unwrap_or(0) as u32,
                bitrate: parse_u64(&s["bit_rate"]),
            })
            .collect(),
    })
}

// --------------------------------------------------------------- Thumbnails

fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("editron")
}

/// Thumbnail in den App-Cache rendern (Hash-basierter Datei-Cache).
pub fn generate_thumbnail(path: &str, time_sec: f64, max_width: u32) -> Result<String, String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_NONCE: AtomicU64 = AtomicU64::new(0);

    let time_sec = time_sec.max(0.0);
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    if let Ok(mtime) = std::fs::metadata(path).and_then(|m| m.modified()) {
        mtime.hash(&mut hasher);
    }
    time_sec.to_bits().hash(&mut hasher);
    max_width.hash(&mut hasher);
    let hash = hasher.finish();

    let dir = cache_dir().join("thumbnails");
    let out = dir.join(format!("{hash:016x}.jpg"));
    if out.exists() {
        return Ok(out.to_string_lossy().into_owned());
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // Erst in eine Temp-Datei rendern, bei Erfolg atomar umbenennen.
    let nonce = TMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!("{hash:016x}.tmp-{}-{nonce}.jpg", std::process::id()));
    // Komma im Filterausdruck escapen, sonst trennt es Filter im Graphen.
    let filter = format!("scale=w=min({max_width}\\,iw):h=-2");
    let output = Command::new(ffmpeg_bin())
        .args(["-y", "-v", "error", "-ss", &format!("{time_sec:.3}")])
        .args(["-i", path])
        .args(["-frames:v", "1", "-vf", &filter])
        .arg(&tmp)
        .output()
        .map_err(|e| format!("ffmpeg konnte nicht gestartet werden: {e}"))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("ffmpeg: {}", stderr_tail(&output.stderr)));
    }
    if let Err(err) = std::fs::rename(&tmp, &out) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err.to_string());
    }
    Ok(out.to_string_lossy().into_owned())
}

// ----------------------------------------------------------------- Waveform

const WAVEFORM_SAMPLE_RATE: u32 = 8000;

/// Waveform-Peaks (0..1): PCM mono s16le streamen, in Buckets falten.
pub fn extract_waveform(path: &str, samples: u32) -> Result<Vec<f32>, String> {
    if samples == 0 {
        return Ok(Vec::new());
    }
    // Dauer vorab schätzen, um beim Streamen direkt in Buckets zu falten.
    let expected_total = probe_media(path)
        .ok()
        .map(|m| m.duration_sec)
        .filter(|d| *d > 0.0)
        .map(|d| (d * f64::from(WAVEFORM_SAMPLE_RATE)).round() as u64)
        .filter(|n| *n > 0);

    let mut child = Command::new(ffmpeg_bin())
        .args(["-v", "error", "-i", path])
        .args(["-map", "a:0", "-ac", "1", "-ar", "8000"])
        .args(["-f", "s16le", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg konnte nicht gestartet werden: {e}"))?;

    let mut stdout = child.stdout.take().ok_or("ffmpeg-stdout nicht verfügbar")?;
    let mut stderr = child.stderr.take().ok_or("ffmpeg-stderr nicht verfügbar")?;
    // stderr nebenläufig leeren, sonst kann ffmpeg an einer vollen Pipe hängen.
    let stderr_task = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let buckets = samples as usize;
    let mut peaks = vec![0i32; buckets];
    let mut chunk = vec![0u8; 64 * 1024];
    let mut leftover: Option<u8> = None;
    let mut index: u64 = 0;
    let mut fallback: Vec<u8> = Vec::new();
    loop {
        let n = stdout.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        match expected_total {
            Some(total) => {
                let mut bytes = &chunk[..n];
                let mut fold = |lo: u8, hi: u8| {
                    let v = i16::from_le_bytes([lo, hi]) as i32;
                    let b = ((index * buckets as u64) / total).min(buckets as u64 - 1) as usize;
                    peaks[b] = peaks[b].max(v.abs());
                    index += 1;
                };
                if let Some(lo) = leftover.take() {
                    fold(lo, bytes[0]);
                    bytes = &bytes[1..];
                }
                for pair in bytes.chunks_exact(2) {
                    fold(pair[0], pair[1]);
                }
                if bytes.len() % 2 == 1 {
                    leftover = Some(bytes[bytes.len() - 1]);
                }
            }
            None => {
                if fallback.len() + n > 256 * 1024 * 1024 {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Audio zu lang für Waveform-Analyse".to_string());
                }
                fallback.extend_from_slice(&chunk[..n]);
            }
        }
    }

    if expected_total.is_none() {
        let total = fallback.len() / 2;
        index = total as u64;
        for (i, peak) in peaks.iter_mut().enumerate() {
            let start = i * total / buckets;
            let end = (i + 1) * total / buckets;
            for j in start..end {
                let v = i16::from_le_bytes([fallback[2 * j], fallback[2 * j + 1]]) as i32;
                *peak = (*peak).max(v.abs());
            }
        }
    }

    let status = child.wait().map_err(|e| e.to_string())?;
    let stderr_buf = stderr_task.join().unwrap_or_default();
    if !status.success() {
        let tail = stderr_tail(&stderr_buf);
        if tail.contains("matches no streams") {
            return Err(format!("Kein Audio-Stream: {path}"));
        }
        return Err(format!("ffmpeg: {tail}"));
    }
    if index == 0 {
        return Err(format!("Kein Audio-Stream: {path}"));
    }
    Ok(peaks.into_iter().map(|p| p as f32 / 32768.0).collect())
}

// ------------------------------------------------------------------- Reveal

/// Prozent-Encoding für file://-URLs (alles außer unreserved + "/").
#[cfg(all(unix, not(target_os = "macos")))]
fn encode_file_url(path: &str) -> String {
    let mut url = String::from("file://");
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                url.push(byte as char)
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

/// Zeigt `path` im Dateimanager an (Finder/Explorer/FileManager1-D-Bus/xdg-open).
pub fn reveal_in_file_manager(path: &str) -> Result<(), String> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(format!("Pfad existiert nicht: {path}"));
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .args(["-R", path])
            .spawn()
            .map_err(|e| format!("Finder konnte nicht gestartet werden: {e}"))?;
        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("explorer")
            .arg(format!("/select,{path}"))
            .spawn()
            .map_err(|e| format!("Explorer konnte nicht gestartet werden: {e}"))?;
        Ok(())
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let url = encode_file_url(path);
        let dbus = Command::new("dbus-send")
            .args([
                "--session",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
                &format!("array:string:{url}"),
                "string:",
            ])
            .status();
        if matches!(dbus, Ok(status) if status.success()) {
            return Ok(());
        }
        let dir = p.parent().unwrap_or_else(|| Path::new("/"));
        Command::new("xdg-open")
            .arg(dir)
            .spawn()
            .map_err(|e| format!("Dateimanager konnte nicht gestartet werden: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn write_file(path: &Path, len: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![0u8; len]).unwrap();
    }

    #[test]
    fn relink_scan_matches_by_name_and_prefers_size() {
        let root = std::env::temp_dir().join(format!("editron-relink-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // Zwei gleichnamige Kandidaten in verschiedenen Tiefen — die Größe
        // entscheidet; ein zweiter Suchauftrag bekommt den Rest-Kandidaten.
        write_file(&root.join("a/musik.wav"), 100);
        write_file(&root.join("b/tief/verschachtelt/musik.wav"), 200);
        write_file(&root.join("CLIP.MP4"), 50); // Case-insensitive-Match
        write_file(&root.join("unrelated.txt"), 10);

        let targets = vec![
            RelinkTarget {
                asset_id: "wav-200".into(),
                file_name: "musik.wav".into(),
                size_bytes: 200,
            },
            RelinkTarget {
                asset_id: "wav-other".into(),
                file_name: "musik.wav".into(),
                size_bytes: 999, // kein Größen-Match → erster freier Kandidat
            },
            RelinkTarget {
                asset_id: "video".into(),
                file_name: "clip.mp4".into(),
                size_bytes: 50,
            },
            RelinkTarget {
                asset_id: "fehlt".into(),
                file_name: "nirgends.mov".into(),
                size_bytes: 1,
            },
        ];
        let cancel = AtomicBool::new(false);
        let (matches, cancelled) = match_relink_candidates(&root, &targets, &cancel, |_| {});
        assert!(!cancelled);

        let by_id: std::collections::HashMap<&str, &PathBuf> = matches
            .iter()
            .map(|(id, p)| (id.as_str(), p))
            .collect();
        assert_eq!(matches.len(), 3);
        assert!(by_id["wav-200"].ends_with("verschachtelt/musik.wav"));
        assert!(by_id["wav-other"].ends_with("a/musik.wav"));
        assert!(by_id["video"].ends_with("CLIP.MP4"));
        assert!(!by_id.contains_key("fehlt"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn relink_scan_honors_cancel() {
        let root = std::env::temp_dir().join(format!("editron-relink-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_file(&root.join("x/m.wav"), 10);
        let targets = vec![RelinkTarget {
            asset_id: "a".into(),
            file_name: "m.wav".into(),
            size_bytes: 10,
        }];
        let cancel = AtomicBool::new(true);
        let (matches, cancelled) = match_relink_candidates(&root, &targets, &cancel, |_| {});
        assert!(cancelled);
        assert!(matches.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
