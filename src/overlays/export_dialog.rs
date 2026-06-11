//! Export-Dialog: Preset (H.264/H.265/VP9/Nur Audio), Auflösung, Qualität
//! (CRF), Zieldatei, Start/Abbruch und Fortschritt über die
//! Transcode-Events.

use crate::services::{Services, TranscodeOptions};
use crate::state::AppState;
use crate::stores::DialogId;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::drop_shadow;
use crate::ui::widgets::select::select;
use crate::ui::widgets::{slider, IconButton, TextButton, TextButtonStyle};
use crate::ui::{FontKind, Ui};
use raylib::consts::KeyboardKey;

struct Preset {
    label: &'static str,
    ext: &'static str,
    video_codec: Option<&'static str>,
    audio_codec: &'static str,
    crf: Option<(u32, u32, u32)>, // min, max, default
    ffmpeg_preset: Option<&'static str>,
}

const PRESETS: [Preset; 4] = [
    Preset {
        label: "H.264 / MP4",
        ext: "mp4",
        video_codec: Some("libx264"),
        audio_codec: "aac",
        crf: Some((14, 32, 21)),
        ffmpeg_preset: Some("medium"),
    },
    Preset {
        label: "H.265 / MP4",
        ext: "mp4",
        video_codec: Some("libx265"),
        audio_codec: "aac",
        crf: Some((18, 36, 26)),
        ffmpeg_preset: Some("medium"),
    },
    Preset {
        label: "WebM / VP9",
        ext: "webm",
        video_codec: Some("libvpx-vp9"),
        audio_codec: "libopus",
        crf: Some((18, 44, 32)),
        ffmpeg_preset: None,
    },
    Preset {
        label: "Nur Audio / M4A",
        ext: "m4a",
        video_codec: None,
        audio_codec: "aac",
        crf: None,
        ffmpeg_preset: None,
    },
];

const RESOLUTIONS: [(&str, Option<u32>); 3] =
    [("Original", None), ("1080p", Some(1080)), ("720p", Some(720))];

#[derive(Clone, Copy, PartialEq)]
enum JobState {
    Idle,
    Running,
    Done,
    Failed,
}

pub struct ExportDialog {
    preset: usize,
    resolution: usize,
    crf: f64,
    output: Option<String>,
    job_id: Option<String>,
    job_state: JobState,
    progress_pct: f64,
    speed: Option<f64>,
    error: Option<String>,
    was_open: bool,
}

impl Default for ExportDialog {
    fn default() -> Self {
        ExportDialog {
            preset: 0,
            resolution: 0,
            crf: 21.0,
            output: None,
            job_id: None,
            job_state: JobState::Idle,
            progress_pct: 0.0,
            speed: None,
            error: None,
            was_open: false,
        }
    }
}

impl ExportDialog {
    /// Transcode-Events aus dem Service-Kanal übernehmen.
    pub fn on_progress(&mut self, job_id: &str, pct: Option<f64>, speed: Option<f64>) {
        if self.job_id.as_deref() == Some(job_id) {
            if let Some(p) = pct {
                self.progress_pct = p;
            }
            self.speed = speed;
        }
    }

    pub fn on_done(&mut self, job_id: &str, ok: bool, error: Option<String>) {
        if self.job_id.as_deref() == Some(job_id) {
            self.job_state = if ok { JobState::Done } else { JobState::Failed };
            self.error = error;
            if ok {
                self.progress_pct = 100.0;
            }
        }
    }

    pub fn on_target_picked(&mut self, path: Option<std::path::PathBuf>) {
        if let Some(p) = path {
            self.output = Some(p.to_string_lossy().into_owned());
        }
    }

    pub fn render(&mut self, ui: &mut Ui, state: &mut AppState, services: &Services) {
        if state.app.open_dialog != Some(DialogId::Export) {
            self.was_open = false;
            return;
        }
        // Quelle: erstes ausgewähltes Asset, sonst Quellmonitor-Asset.
        let asset = state
            .media
            .selected_asset_ids
            .first()
            .and_then(|id| state.media.asset(id))
            .or_else(|| {
                state
                    .playback
                    .source_asset_id
                    .as_ref()
                    .and_then(|id| state.media.asset(id))
            })
            .cloned();

        if !self.was_open {
            self.was_open = true;
            if self.job_state != JobState::Running {
                *self = ExportDialog {
                    was_open: true,
                    ..Default::default()
                };
                if let Some(a) = &asset {
                    let preset = &PRESETS[self.preset];
                    self.output = Some(replace_extension(&a.path, &format!("export.{}", preset.ext)));
                    self.crf = preset.crf.map(|c| c.2 as f64).unwrap_or(21.0);
                }
            }
        }

        if ui.input.keys.iter().any(|k| k.key == KeyboardKey::KEY_ESCAPE)
            && self.job_state != JobState::Running
        {
            state.app.open_dialog = None;
            return;
        }

        ui.fill(ui.screen, theme::with_alpha(theme::BLACK, 102));
        let w = 480.0_f32;
        let h = 360.0_f32;
        let rect = ui.screen.center_box(w, h);
        drop_shadow(ui, rect, theme::RADIUS_LG);
        ui.fill_rounded(rect, theme::RADIUS_LG, theme::SURFACE_1);
        ui.stroke_rounded(rect, theme::RADIUS_LG, 1.0, theme::LINE_STRONG);

        let mut area = rect;
        let head = area.cut_top(48.0);
        ui.hline(head.x, head.bottom() - 1.0, head.w, theme::LINE);
        let mut hi = head.inset_xy(16.0, 0.0);
        let icon_cell = hi.cut_left(18.0);
        ui.icon("file-output", icon_cell, 18.0, theme::TEXT_2);
        hi.cut_left(8.0);
        ui.text_left("Exportieren", hi, theme::TEXT_1, FontKind::Sans16Semibold);
        let close = Rect::new(head.right() - 16.0 - 28.0, head.y + 10.0, 28.0, 28.0);
        if IconButton::new("x")
            .tooltip("Schließen")
            .disabled(self.job_state == JobState::Running)
            .show(ui, "export.close", close)
            .clicked
        {
            state.app.open_dialog = None;
            return;
        }

        let mut body = area.inset(16.0);

        let Some(asset) = asset else {
            ui.text_centered(
                "Kein Medium ausgewählt — Clip im Medien-Browser wählen.",
                body,
                theme::TEXT_3,
                FontKind::Sans12,
            );
            return;
        };

        // Quelle
        let row = body.cut_top(24.0);
        let mut r = row;
        let label = r.cut_left(96.0);
        ui.text_left("Quelle", label, theme::TEXT_2, FontKind::Sans12);
        let name = ui.font(FontKind::Sans12).ellipsize(&asset.name, r.w);
        ui.text_left(&name, r, theme::TEXT_1, FontKind::Sans12);
        body.cut_top(8.0);

        // Preset
        let row = body.cut_top(24.0);
        let mut r = row;
        let label = r.cut_left(96.0);
        ui.text_left("Format", label, theme::TEXT_2, FontKind::Sans12);
        let labels: Vec<&str> = PRESETS.iter().map(|p| p.label).collect();
        if let Some(p) = select(ui, "export.preset", r, &labels, self.preset) {
            self.preset = p;
            let preset = &PRESETS[p];
            self.crf = preset.crf.map(|c| c.2 as f64).unwrap_or(21.0);
            if let Some(out) = &self.output {
                self.output = Some(replace_extension(out, preset.ext));
            }
        }
        body.cut_top(8.0);

        let preset = &PRESETS[self.preset];

        // Auflösung (nur mit Video)
        if preset.video_codec.is_some() {
            let row = body.cut_top(24.0);
            let mut r = row;
            let label = r.cut_left(96.0);
            ui.text_left("Auflösung", label, theme::TEXT_2, FontKind::Sans12);
            let labels: Vec<&str> = RESOLUTIONS.iter().map(|(l, _)| *l).collect();
            if let Some(idx) = select(ui, "export.resolution", r, &labels, self.resolution) {
                self.resolution = idx;
            }
            body.cut_top(8.0);
        }

        // Qualität (CRF)
        if let Some((min, max, _)) = preset.crf {
            let row = body.cut_top(24.0);
            let mut r = row;
            let label = r.cut_left(96.0);
            ui.text_left("Qualität (CRF)", label, theme::TEXT_2, FontKind::Sans12);
            let value_cell = r.cut_right(36.0);
            r.cut_right(8.0);
            slider(ui, "export.crf", r, &mut self.crf, min as f64, max as f64, theme::ACCENT);
            self.crf = self.crf.round();
            ui.text_right(
                &format!("{}", self.crf as i64),
                value_cell,
                theme::TEXT_1,
                FontKind::Mono12,
            );
            body.cut_top(8.0);
        }

        // Ziel
        let row = body.cut_top(24.0);
        let mut r = row;
        let label = r.cut_left(96.0);
        ui.text_left("Ziel", label, theme::TEXT_2, FontKind::Sans12);
        let browse = TextButton::new("Durchsuchen…").style(TextButtonStyle::Outline);
        let bw = browse.measure(ui);
        let browse_rect = r.cut_right(bw);
        r.cut_right(8.0);
        let target = self.output.clone().unwrap_or_else(|| "—".into());
        let target_display = ui.font(FontKind::Mono12).ellipsize(&target, r.w);
        ui.text_left(&target_display, r, theme::TEXT_1, FontKind::Mono12);
        if browse
            .show(ui, "export.browse", Rect::new(browse_rect.x, row.y, bw, 24.0))
            .clicked
        {
            let default_name = std::path::Path::new(&target)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("export.{}", preset.ext));
            services.pick_export_target(&default_name, preset.ext);
        }
        body.cut_top(16.0);

        // Fortschritt / Status
        match self.job_state {
            JobState::Running => {
                let row = body.cut_top(20.0);
                let track = Rect::new(row.x, row.y + 7.0, row.w, 6.0);
                ui.fill_rounded(track, 3.0, theme::SURFACE_3);
                ui.fill_rounded(
                    Rect::new(track.x, track.y, track.w * (self.progress_pct as f32 / 100.0), 6.0),
                    3.0,
                    theme::ACCENT,
                );
                let info = body.cut_top(18.0);
                let speed = self
                    .speed
                    .map(|s| format!(" — {s:.1}×"))
                    .unwrap_or_default();
                ui.text_left(
                    &format!("Exportiere … {:.0} %{}", self.progress_pct, speed),
                    info,
                    theme::TEXT_2,
                    FontKind::Sans12,
                );
            }
            JobState::Done => {
                let info = body.cut_top(20.0);
                let mut r = info;
                let ic = r.cut_left(16.0);
                ui.icon("circle-check", ic, 16.0, theme::SUCCESS);
                r.cut_left(6.0);
                ui.text_left("Export abgeschlossen.", r, theme::SUCCESS, FontKind::Sans12);
            }
            JobState::Failed => {
                let info = body.cut_top(20.0);
                let mut r = info;
                let ic = r.cut_left(16.0);
                ui.icon("circle-alert", ic, 16.0, theme::DANGER);
                r.cut_left(6.0);
                let msg = self
                    .error
                    .clone()
                    .unwrap_or_else(|| "Export fehlgeschlagen".into());
                let msg = ui.font(FontKind::Sans12).ellipsize(&msg, r.w);
                ui.text_left(&msg, r, theme::DANGER, FontKind::Sans12);
            }
            JobState::Idle => {}
        }

        // Buttons unten
        let btn_row = Rect::new(rect.x + 16.0, rect.bottom() - 44.0, rect.w - 32.0, 28.0);
        if self.job_state == JobState::Running {
            let cancel = TextButton::new("Abbrechen").icon("ban").style(TextButtonStyle::Outline);
            let cw = cancel.measure(ui);
            if cancel
                .show(ui, "export.cancel", Rect::new(btn_row.right() - cw, btn_row.y, cw, 28.0))
                .clicked
            {
                if let Some(id) = &self.job_id {
                    services.cancel_job(id);
                }
            }
        } else {
            let start = TextButton::new("Export starten");
            let sw = start.measure(ui);
            let can_start = self.output.is_some();
            if start
                .disabled(!can_start)
                .show(ui, "export.start", Rect::new(btn_row.right() - sw, btn_row.y, sw, 28.0))
                .clicked
            {
                let (width, height) = (None, RESOLUTIONS[self.resolution].1);
                let options = TranscodeOptions {
                    input: asset.path.clone(),
                    output: self.output.clone().unwrap(),
                    video_codec: preset.video_codec.map(String::from),
                    audio_codec: Some(preset.audio_codec.to_string()),
                    crf: preset.crf.map(|_| self.crf as u32),
                    preset: preset.ffmpeg_preset.map(String::from),
                    width,
                    height: if preset.video_codec.is_some() { height } else { None },
                };
                match services.start_transcode(options) {
                    Ok(job_id) => {
                        self.job_id = Some(job_id);
                        self.job_state = JobState::Running;
                        self.progress_pct = 0.0;
                        self.error = None;
                    }
                    Err(err) => {
                        self.job_state = JobState::Failed;
                        self.error = Some(err);
                    }
                }
            }
        }
    }
}

fn replace_extension(path: &str, ext: &str) -> String {
    match path.rfind('.') {
        Some(idx) if idx > path.rfind('/').unwrap_or(0) => format!("{}.{ext}", &path[..idx]),
        _ => format!("{path}.{ext}"),
    }
}
