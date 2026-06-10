//! Transcode-Job-Manager: spawnt ffmpeg, parst `-progress`-Ausgabe und
//! emittet `transcode://progress` / `transcode://done`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tauri::Emitter;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::Mutex;

use super::error::{Error, Result};
use super::types::{TranscodeDone, TranscodeOptions, TranscodeProgress};
use super::{locate, probe, stderr_tail};

/// Pro Job ein geteilter Handle: Die Map dient nur dem Kill-Zugriff,
/// das exklusive Wait-Recht nimmt sich der Monitor-Task per `take()`.
type JobHandle = Arc<Mutex<Option<Child>>>;

pub struct JobManager {
    next_id: AtomicU64,
    jobs: Arc<Mutex<HashMap<String, JobHandle>>>,
}

impl Default for JobManager {
    fn default() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            jobs: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl JobManager {
    pub async fn start_transcode(
        &self,
        app: tauri::AppHandle,
        options: TranscodeOptions,
    ) -> Result<String> {
        // Quelldauer für progressPct (best effort)
        let duration = probe::probe_media(&options.input)
            .await
            .ok()
            .map(|m| m.duration_sec)
            .filter(|d| *d > 0.0);

        let mut cmd = locate::command(&locate::ffmpeg_bin());
        cmd.args(["-y", "-i", &options.input]);
        if let Some(codec) = &options.video_codec {
            cmd.args(["-c:v", codec]);
        }
        if let Some(crf) = options.crf {
            cmd.args(["-crf", &crf.to_string()]);
        }
        if let Some(preset) = &options.preset {
            cmd.args(["-preset", preset]);
        }
        if let Some(filter) = scale_filter(options.width, options.height) {
            cmd.args(["-vf", &filter]);
        }
        if let Some(codec) = &options.audio_codec {
            cmd.args(["-c:a", codec]);
        }
        // Audio-only-Export: ohne videoCodec (aber mit audioCodec) keine Videospur
        // mappen, sonst re-encodiert ffmpeg sie mit dem Muxer-Default.
        if options.video_codec.is_none() && options.audio_codec.is_some() {
            cmd.arg("-vn");
        }
        cmd.args(["-progress", "pipe:1", "-nostats", "-loglevel", "error"]);
        cmd.arg(&options.output);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Other("ffmpeg-stdout nicht verfügbar".to_string()))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Other("ffmpeg-stderr nicht verfügbar".to_string()))?;

        let job_id = format!("job-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let handle: JobHandle = Arc::new(Mutex::new(Some(child)));
        self.jobs.lock().await.insert(job_id.clone(), Arc::clone(&handle));

        let jobs = Arc::clone(&self.jobs);
        let id = job_id.clone();
        let output = options.output.clone();
        tauri::async_runtime::spawn(async move {
            let stderr_task = tauri::async_runtime::spawn(async move {
                let mut buf = Vec::new();
                let _ = stderr.read_to_end(&mut buf).await;
                buf
            });

            let mut lines = BufReader::new(stdout).lines();
            let mut out_time_sec = 0.0_f64;
            let mut fps: Option<f64> = None;
            let mut speed: Option<f64> = None;
            while let Ok(Some(line)) = lines.next_line().await {
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                let value = value.trim();
                match key.trim() {
                    // out_time_ms liefert historisch ebenfalls Mikrosekunden
                    "out_time_us" | "out_time_ms" => {
                        if let Ok(us) = value.parse::<i64>() {
                            out_time_sec = us.max(0) as f64 / 1_000_000.0;
                        }
                    }
                    "fps" => fps = value.parse().ok().filter(|v: &f64| *v > 0.0),
                    "speed" => speed = value.trim_end_matches('x').trim().parse().ok(),
                    // Ein -progress-Block endet immer mit "progress=..."
                    "progress" => {
                        let progress_pct =
                            duration.map(|d| (out_time_sec / d * 100.0).clamp(0.0, 100.0));
                        let _ = app.emit(
                            "transcode://progress",
                            TranscodeProgress {
                                job_id: id.clone(),
                                out_time_sec,
                                fps,
                                speed,
                                progress_pct,
                            },
                        );
                    }
                    _ => {}
                }
            }

            // stdout-EOF: Prozess ist beendet (oder wurde gekillt)
            jobs.lock().await.remove(&id);
            // Child per take() übernehmen — der Guard fällt am Semikolon,
            // wait() läuft also ohne gehaltenen Lock.
            let child = handle.lock().await.take();
            let status = match child {
                Some(mut child) => child.wait().await.ok(),
                None => None,
            };
            let stderr_buf = stderr_task.await.unwrap_or_default();
            let ok = status.map(|s| s.success()).unwrap_or(false);
            if !ok {
                // Unvollständige Zieldatei nach Abbruch/Fehlschlag entfernen (best effort)
                let _ = std::fs::remove_file(&output);
            }
            let error = if ok {
                None
            } else {
                let tail = stderr_tail(&stderr_buf);
                Some(if tail.is_empty() {
                    "Job abgebrochen oder ohne Fehlermeldung beendet".to_string()
                } else {
                    tail
                })
            };
            let _ = app.emit("transcode://done", TranscodeDone { job_id: id, ok, error });
        });

        Ok(job_id)
    }

    /// Killt den Child-Prozess; unbekannte IDs (z. B. bereits fertig) sind ok.
    pub async fn cancel_job(&self, job_id: &str) -> Result<()> {
        let handle = self.jobs.lock().await.get(job_id).cloned();
        if let Some(handle) = handle {
            // Lock nur kurz halten: start_kill ist synchron und blockiert nicht
            if let Some(child) = handle.lock().await.as_mut() {
                child.start_kill()?;
            }
        }
        Ok(())
    }
}

fn scale_filter(width: Option<u32>, height: Option<u32>) -> Option<String> {
    match (width, height) {
        (Some(w), Some(h)) => Some(format!("scale={w}:{h}")),
        (Some(w), None) => Some(format!("scale={w}:-2")),
        (None, Some(h)) => Some(format!("scale=-2:{h}")),
        (None, None) => None,
    }
}
