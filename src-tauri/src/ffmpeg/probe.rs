//! Strukturierte Medienanalyse über `ffprobe -print_format json`.

use std::path::Path;

use serde::Deserialize;

use super::error::{Error, Result};
use super::types::{AudioStreamInfo, MediaInfo, VideoStreamInfo};
use super::{locate, stderr_tail};

#[derive(Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Deserialize)]
struct ProbeStream {
    index: u32,
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    pix_fmt: Option<String>,
    bit_rate: Option<String>,
    channels: Option<u32>,
    sample_rate: Option<String>,
    duration: Option<String>,
    #[serde(default)]
    disposition: Disposition,
}

#[derive(Deserialize, Default)]
struct Disposition {
    #[serde(default)]
    attached_pic: i32,
}

#[derive(Deserialize)]
struct ProbeFormat {
    format_name: Option<String>,
    duration: Option<String>,
    size: Option<String>,
}

pub async fn probe_media(path: &str) -> Result<MediaInfo> {
    let output = locate::command(&locate::ffprobe_bin())
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
        .await?;

    if !output.status.success() {
        return Err(Error::FfprobeFailed(stderr_tail(&output.stderr)));
    }

    let probe: ProbeOutput = serde_json::from_slice(&output.stdout)?;

    // Cover-Art (attached_pic) zählt nicht als Video-Stream.
    let video_streams: Vec<&ProbeStream> = probe
        .streams
        .iter()
        .filter(|s| s.codec_type.as_deref() == Some("video") && s.disposition.attached_pic != 1)
        .collect();
    let audio_streams: Vec<&ProbeStream> = probe
        .streams
        .iter()
        .filter(|s| s.codec_type.as_deref() == Some("audio"))
        .collect();

    let duration_sec = probe
        .format
        .as_ref()
        .and_then(|f| f.duration.as_deref())
        .and_then(|d| d.trim().parse::<f64>().ok())
        .or_else(|| {
            video_streams
                .first()
                .and_then(|s| s.duration.as_deref())
                .and_then(|d| d.trim().parse::<f64>().ok())
        })
        .unwrap_or(0.0);

    let size_bytes = probe
        .format
        .as_ref()
        .and_then(|f| f.size.as_deref())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or_else(|| std::fs::metadata(path).map(|m| m.len()).unwrap_or(0));

    let video = video_streams
        .iter()
        .map(|s| VideoStreamInfo {
            index: s.index,
            codec: s.codec_name.clone().unwrap_or_default(),
            width: s.width.unwrap_or(0),
            height: s.height.unwrap_or(0),
            fps: s
                .avg_frame_rate
                .as_deref()
                .and_then(parse_rate)
                .or_else(|| s.r_frame_rate.as_deref().and_then(parse_rate))
                .unwrap_or(0.0),
            pix_fmt: s.pix_fmt.clone(),
            bitrate: parse_u64(s.bit_rate.as_deref()),
        })
        .collect();

    let audio = audio_streams
        .iter()
        .map(|s| AudioStreamInfo {
            index: s.index,
            codec: s.codec_name.clone().unwrap_or_default(),
            channels: s.channels.unwrap_or(0),
            sample_rate: parse_u64(s.sample_rate.as_deref()).unwrap_or(0) as u32,
            bitrate: parse_u64(s.bit_rate.as_deref()),
        })
        .collect();

    Ok(MediaInfo {
        path: path.to_string(),
        file_name: Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string()),
        container: probe
            .format
            .and_then(|f| f.format_name)
            .unwrap_or_default(),
        duration_sec,
        size_bytes,
        video,
        audio,
    })
}

/// Parst Frameraten-Brüche wie "30000/1001"; "0/0" und 0 ergeben `None`.
fn parse_rate(s: &str) -> Option<f64> {
    let value = match s.trim().split_once('/') {
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
    (value > 0.0).then_some(value)
}

fn parse_u64(s: Option<&str>) -> Option<u64> {
    s.and_then(|v| v.trim().parse().ok())
}
