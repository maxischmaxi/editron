mod ffmpeg;
mod reveal;

use ffmpeg::jobs::JobManager;
use ffmpeg::types::{FfmpegInfo, MediaInfo, TranscodeOptions};

#[tauri::command]
async fn ffmpeg_info() -> FfmpegInfo {
    ffmpeg::locate::ffmpeg_info().await
}

#[tauri::command]
async fn probe_media(path: String) -> Result<MediaInfo, ffmpeg::Error> {
    ffmpeg::probe::probe_media(&path).await
}

#[tauri::command]
async fn generate_thumbnail(
    app: tauri::AppHandle,
    path: String,
    time_sec: f64,
    max_width: u32,
) -> Result<String, ffmpeg::Error> {
    ffmpeg::thumbs::generate_thumbnail(&app, &path, time_sec, max_width).await
}

#[tauri::command]
async fn extract_waveform(path: String, samples: u32) -> Result<Vec<f32>, ffmpeg::Error> {
    ffmpeg::waveform::extract_waveform(&path, samples).await
}

#[tauri::command]
async fn start_transcode(
    app: tauri::AppHandle,
    state: tauri::State<'_, JobManager>,
    options: TranscodeOptions,
) -> Result<String, ffmpeg::Error> {
    state.start_transcode(app, options).await
}

#[tauri::command]
async fn reveal_in_file_manager(path: String) -> Result<(), String> {
    reveal::reveal_in_file_manager(&path)
}

#[tauri::command]
async fn cancel_job(
    state: tauri::State<'_, JobManager>,
    job_id: String,
) -> Result<(), ffmpeg::Error> {
    state.cancel_job(&job_id).await
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .manage(JobManager::default())
        .invoke_handler(tauri::generate_handler![
            ffmpeg_info,
            probe_media,
            generate_thumbnail,
            extract_waveform,
            start_transcode,
            cancel_job,
            reveal_in_file_manager
        ])
        .run(tauri::generate_context!())
        .expect("Fehler beim Starten von Editron");
}
