//! Waveform-Peaks: PCM chunkweise streamen, in Buckets falten, Maxima normalisieren.

use std::process::Stdio;

use tokio::io::AsyncReadExt;
use tokio::process::ChildStdout;

use super::error::{Error, Result};
use super::{locate, probe, stderr_tail};

/// Abtastrate des dekodierten Mono-PCM-Streams (s16le).
const SAMPLE_RATE: u32 = 8000;
/// Lesepuffer fürs Streamen.
const CHUNK_SIZE: usize = 64 * 1024;
/// Obergrenze für den Fallback ohne bekannte Dauer (~4,5 h Audio als PCM).
const MAX_FALLBACK_BYTES: usize = 256 * 1024 * 1024;

pub async fn extract_waveform(path: &str, samples: u32) -> Result<Vec<f32>> {
    if samples == 0 {
        return Ok(Vec::new());
    }

    // Dauer vorab schätzen (best effort), um beim Streamen direkt in Buckets zu falten.
    let expected_total = probe::probe_media(path)
        .await
        .ok()
        .map(|m| m.duration_sec)
        .filter(|d| *d > 0.0)
        .map(|d| (d * f64::from(SAMPLE_RATE)).round() as u64)
        .filter(|n| *n > 0);

    let mut child = locate::command(&locate::ffmpeg_bin())
        .args(["-v", "error", "-i", path])
        .args(["-map", "a:0", "-ac", "1", "-ar", "8000"])
        .args(["-f", "s16le", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Other("ffmpeg-stdout nicht verfügbar".to_string()))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::Other("ffmpeg-stderr nicht verfügbar".to_string()))?;
    // stderr nebenläufig leeren, sonst kann ffmpeg an einer vollen Pipe hängen.
    let stderr_task = tauri::async_runtime::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        buf
    });

    let buckets = samples as usize;
    let folded = match expected_total {
        Some(total) => stream_peaks(&mut stdout, total, buckets).await,
        None => buffered_peaks(&mut stdout, buckets).await,
    };
    let (peaks, sample_count) = match folded {
        Ok(result) => result,
        Err(err) => {
            // z. B. Größenlimit überschritten: ffmpeg nicht weiterlaufen lassen
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(err);
        }
    };

    let status = child.wait().await?;
    let stderr_buf = stderr_task.await.unwrap_or_default();
    if !status.success() {
        let tail = stderr_tail(&stderr_buf);
        if tail.contains("matches no streams") {
            return Err(Error::NoAudioStream(path.to_string()));
        }
        return Err(Error::FfmpegFailed(tail));
    }
    if sample_count == 0 {
        return Err(Error::NoAudioStream(path.to_string()));
    }
    Ok(peaks.into_iter().map(|p| p as f32 / 32768.0).collect())
}

/// Faltet den PCM-Stream chunkweise in `buckets` Maxima. `total` ist die aus der
/// Dauer geschätzte Gesamt-Samplezahl; der Bucket-Index wird geclampt, falls die
/// reale Samplezahl davon abweicht.
async fn stream_peaks(
    stdout: &mut ChildStdout,
    total: u64,
    buckets: usize,
) -> Result<(Vec<i32>, u64)> {
    let mut peaks = vec![0i32; buckets];
    let mut chunk = vec![0u8; CHUNK_SIZE];
    let mut leftover: Option<u8> = None;
    let mut index: u64 = 0;
    let fold = |lo: u8, hi: u8, peaks: &mut Vec<i32>, index: &mut u64| {
        let v = i16::from_le_bytes([lo, hi]) as i32;
        let b = ((*index * buckets as u64) / total).min(buckets as u64 - 1) as usize;
        peaks[b] = peaks[b].max(v.abs());
        *index += 1;
    };
    loop {
        let n = stdout.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        let mut bytes = &chunk[..n];
        // Halbes Sample vom vorherigen Chunk-Rand vervollständigen
        if let Some(lo) = leftover.take() {
            fold(lo, bytes[0], &mut peaks, &mut index);
            bytes = &bytes[1..];
        }
        for pair in bytes.chunks_exact(2) {
            fold(pair[0], pair[1], &mut peaks, &mut index);
        }
        if bytes.len() % 2 == 1 {
            leftover = Some(bytes[bytes.len() - 1]);
        }
    }
    Ok((peaks, index))
}

/// Fallback bei unbekannter Dauer: sammelt den Stream (mit Größenlimit) und
/// teilt anschließend wie zuvor in Buckets.
async fn buffered_peaks(stdout: &mut ChildStdout, buckets: usize) -> Result<(Vec<i32>, u64)> {
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; CHUNK_SIZE];
    loop {
        let n = stdout.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        if bytes.len() + n > MAX_FALLBACK_BYTES {
            return Err(Error::Other(
                "Audio zu lang für Waveform-Analyse (Dauer unbekannt, PCM > 256 MB)".to_string(),
            ));
        }
        bytes.extend_from_slice(&chunk[..n]);
    }

    let total = bytes.len() / 2;
    let mut peaks = vec![0i32; buckets];
    for (i, peak) in peaks.iter_mut().enumerate() {
        let start = i * total / buckets;
        let end = (i + 1) * total / buckets;
        for j in start..end {
            let v = i16::from_le_bytes([bytes[2 * j], bytes[2 * j + 1]]) as i32;
            *peak = (*peak).max(v.abs());
        }
    }
    Ok((peaks, total as u64))
}
