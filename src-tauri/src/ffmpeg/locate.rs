//! Binary-Discovery für ffmpeg/ffprobe inkl. prozessweitem Versions-Cache.

use tokio::sync::OnceCell;

use super::types::FfmpegInfo;

static INFO: OnceCell<FfmpegInfo> = OnceCell::const_new();

/// Pfad zum ffmpeg-Binary: Env-Variable zuerst, sonst PATH.
pub fn ffmpeg_bin() -> String {
    std::env::var("EDITRON_FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".to_string())
}

/// Pfad zum ffprobe-Binary: Env-Variable zuerst, sonst PATH.
pub fn ffprobe_bin() -> String {
    std::env::var("EDITRON_FFPROBE_PATH").unwrap_or_else(|_| "ffprobe".to_string())
}

/// Erzeugt ein Command, auf Windows ohne aufpoppendes Konsolenfenster.
pub fn command(program: &str) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

/// Ermittelt Verfügbarkeit + Version; nur der Erfolgsfall wird gecached,
/// damit eine nachträgliche ffmpeg-Installation beim nächsten Aufruf greift.
pub async fn ffmpeg_info() -> FfmpegInfo {
    INFO.get_or_try_init(|| async {
        let ffmpeg = ffmpeg_bin();
        let ffprobe = ffprobe_bin();
        match command(&ffmpeg).arg("-version").output().await {
            Ok(out) if out.status.success() => Ok(FfmpegInfo {
                available: true,
                version: parse_version(&String::from_utf8_lossy(&out.stdout)),
                ffmpeg_path: Some(ffmpeg),
                ffprobe_path: Some(ffprobe),
            }),
            _ => Err(()),
        }
    })
    .await
    .cloned()
    .unwrap_or(FfmpegInfo {
        available: false,
        version: None,
        ffmpeg_path: None,
        ffprobe_path: None,
    })
}

/// Extrahiert die Version aus der ersten Zeile von `ffmpeg -version`.
fn parse_version(stdout: &str) -> Option<String> {
    let first = stdout.lines().next()?.trim();
    match first.strip_prefix("ffmpeg version ") {
        Some(rest) => rest.split_whitespace().next().map(str::to_string),
        None => Some(first.to_string()),
    }
}
