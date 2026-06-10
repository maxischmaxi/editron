/// Fehlertyp des FFmpeg-Backends. Serialisiert als reiner Fehlertext,
/// damit `Result<T, Error>` direkt als Tauri-Command funktioniert.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("E/A-Fehler: {0}")]
    Io(#[from] std::io::Error),

    #[error("Ungültige ffprobe-Ausgabe: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Tauri-Fehler: {0}")]
    Tauri(#[from] tauri::Error),

    #[error("ffmpeg ist fehlgeschlagen: {0}")]
    FfmpegFailed(String),

    #[error("ffprobe ist fehlgeschlagen: {0}")]
    FfprobeFailed(String),

    #[error("Datei enthält keine Audiospur: {0}")]
    NoAudioStream(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl serde::Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}
