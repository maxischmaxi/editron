//! FFmpeg-Backend: Discovery, Probing, Thumbnails, Waveforms, Transcode-Jobs.

pub mod error;
pub mod jobs;
pub mod locate;
pub mod probe;
pub mod thumbs;
pub mod types;
pub mod waveform;

pub use error::Error;

/// Letzte Zeilen einer stderr-Ausgabe als kompakte Fehlermeldung.
pub(crate) fn stderr_tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(20);
    lines[start..].join("\n").trim().to_string()
}
