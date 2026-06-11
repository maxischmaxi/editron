//! Export-Dialog: modaler Sequenz-Export mit Render-Presets (links),
//! vollständigen Einstellungen (Container, Video-/Audio-Codec, Auflösung,
//! Framerate, CRF/Bitrate bzw. Profil, Samplerate, Kanäle, Bereich, Ziel),
//! Live-Validierung mit Warnungen/Fehlern und Render-Ansicht mit
//! Fortschritt, Frame-Zähler, Geschwindigkeit, Restzeit und Abbruch.
//!
//! Modalität: Der Dialog läuft im Overlay-Pass; `begin_main_layer(true)`
//! blockiert Maus und Shortcuts der Hauptschicht auf allen Plattformen
//! (macOS/Windows/Linux identisch). Während des Renderns ist auch das
//! Schließen gesperrt — nur „Abbrechen“ beendet den Job.

use crate::core::export::{
    self, build_render_plan, validate, AudioSettings, ExportPhase, ExportSettings, QualityKind,
    Severity, VideoQuality, VideoSettings, CONTAINERS, FRAMERATES, PRESETS, RESOLUTIONS,
    SAMPLE_RATES,
};
use crate::core::timecode::format_duration;
use crate::core::timeline::SEQUENCE_FPS;
use crate::services::Services;
use crate::state::AppState;
use crate::stores::DialogId;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::drop_shadow;
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::select::select;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::widgets::{slider, IconButton, TextButton, TextButtonStyle};
use crate::ui::{FontKind, Interaction, Ui};
use raylib::consts::{KeyboardKey, MouseCursor};

const LABEL_W: f32 = 120.0;
const PRESET_COL_W: f32 = 184.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Settings,
    Running,
    Done,
    Failed,
}

pub struct ExportDialog {
    settings: Option<ExportSettings>,
    /// Aktives Render-Preset; None = Benutzerdefiniert.
    preset_idx: Option<usize>,
    /// 0 = „Wie Quelle“, 1..=RESOLUTIONS = Vorgabe, danach Benutzerdefiniert.
    resolution_choice: usize,
    /// 0 = „Wie Sequenz“, 1..=FRAMERATES = Vorgabe, danach Benutzerdefiniert.
    fps_choice: usize,
    width_input: TextInputState,
    height_input: TextInputState,
    fps_input: TextInputState,
    bitrate_input: TextInputState,
    crf: f64,

    issues: Vec<export::ValidationIssue>,
    plan: export::RenderPlan,
    dirty: bool,
    encoders_seen: usize,
    /// Timeline-/Medien-Revision der letzten Validierung (Async-Import).
    revs_seen: (u64, u64),

    stage: Stage,
    job_id: Option<String>,
    progress_pct: f64,
    phase: ExportPhase,
    frames_done: u64,
    frames_total: u64,
    render_fps: f64,
    eta_sec: Option<f64>,
    started_at: f64,
    finished_output: String,
    error: Option<String>,

    form_scroll: ScrollState,
    form_content_h: f32,
    preset_scroll: ScrollState,
    was_open: bool,
    /// Testmodus: Export automatisch starten (EDITRON_TEST_EXPORT=<ziel>).
    test_autostart: bool,
}

impl Default for ExportDialog {
    fn default() -> Self {
        ExportDialog {
            settings: None,
            preset_idx: None,
            resolution_choice: 0,
            fps_choice: 0,
            width_input: TextInputState::default(),
            height_input: TextInputState::default(),
            fps_input: TextInputState::default(),
            bitrate_input: TextInputState::default(),
            crf: 20.0,
            issues: Vec::new(),
            plan: export::RenderPlan::default(),
            dirty: true,
            encoders_seen: usize::MAX,
            revs_seen: (u64::MAX, u64::MAX),
            stage: Stage::Settings,
            job_id: None,
            progress_pct: 0.0,
            phase: ExportPhase::MixAudio,
            frames_done: 0,
            frames_total: 0,
            render_fps: 0.0,
            eta_sec: None,
            started_at: 0.0,
            finished_output: String::new(),
            error: None,
            form_scroll: ScrollState::default(),
            form_content_h: 0.0,
            preset_scroll: ScrollState::default(),
            was_open: false,
            test_autostart: std::env::var("EDITRON_TEST_EXPORT").is_ok(),
        }
    }
}

impl ExportDialog {
    #[allow(clippy::too_many_arguments)]
    pub fn on_progress(
        &mut self,
        job_id: &str,
        pct: f64,
        phase: ExportPhase,
        frames_done: u64,
        frames_total: u64,
        render_fps: f64,
        eta_sec: Option<f64>,
    ) {
        if self.job_id.as_deref() != Some(job_id) {
            return;
        }
        self.progress_pct = pct;
        self.phase = phase;
        self.frames_done = frames_done;
        self.frames_total = frames_total;
        self.render_fps = render_fps;
        self.eta_sec = eta_sec;
    }

    pub fn on_done(
        &mut self,
        job_id: &str,
        ok: bool,
        cancelled: bool,
        error: Option<String>,
        output: &str,
    ) {
        if self.job_id.as_deref() != Some(job_id) {
            return;
        }
        self.job_id = None;
        if ok {
            self.stage = Stage::Done;
            self.progress_pct = 100.0;
            self.finished_output = output.to_string();
        } else if cancelled {
            self.stage = Stage::Settings;
            self.dirty = true;
        } else {
            self.stage = Stage::Failed;
            self.error = error;
        }
    }

    pub fn on_target_picked(&mut self, path: Option<std::path::PathBuf>) {
        let Some(p) = path else { return };
        if let Some(s) = &mut self.settings {
            let mut out = p.to_string_lossy().into_owned();
            let expected = format!(".{}", s.container.ext);
            if !out.to_lowercase().ends_with(&expected) {
                out.push_str(&expected);
            }
            // Bewusst kein mark_custom: der Zielpfad gehört nicht zum Preset.
            s.output = out;
            self.dirty = true;
        }
    }

    // ------------------------------------------------------------ Lifecycle

    fn open_fresh(&mut self, state: &mut AppState) {
        // Wiedergabe anhalten — Export braucht Decode-Ressourcen.
        state.playback.program_playing = false;
        state.playback.source.playing = false;
        if self.stage != Stage::Running {
            self.stage = Stage::Settings;
        }
        if self.settings.is_none() {
            self.apply_preset(3, state); // H.264 Master als Allzweck-Default
        }
        if let Some(s) = &mut self.settings {
            if s.output.is_empty() {
                s.output = default_output_path(state, s.container.ext);
            }
        }
        self.dirty = true;
    }

    fn apply_preset(&mut self, idx: usize, state: &AppState) {
        let Some(preset) = PRESETS.get(idx) else { return };
        let source = export::suggested_resolution(&state.timeline, &state.media);
        let mut settings = (preset.build)(source, SEQUENCE_FPS);
        // Zielpfad behalten, nur die Endung dem Container anpassen.
        settings.output = match self.settings.as_ref() {
            Some(old) if !old.output.is_empty() => {
                replace_extension(&old.output, settings.container.ext)
            }
            _ => default_output_path_raw(settings.container.ext),
        };
        settings.use_in_out = self.settings.as_ref().map(|s| s.use_in_out).unwrap_or(false);
        self.preset_idx = Some(idx);
        self.settings = Some(settings);
        self.sync_inputs_from_settings(state);
        self.dirty = true;
    }

    /// Wahl-Indizes + Textfelder aus den Settings ableiten (nach Preset).
    fn sync_inputs_from_settings(&mut self, state: &AppState) {
        let Some(s) = &self.settings else { return };
        if let Some(v) = &s.video {
            let source = export::suggested_resolution(&state.timeline, &state.media);
            self.resolution_choice = if (v.width, v.height) == source {
                0
            } else if let Some(i) = RESOLUTIONS.iter().position(|(_, w, h)| (*w, *h) == (v.width, v.height)) {
                1 + i
            } else {
                1 + RESOLUTIONS.len()
            };
            self.fps_choice = if (v.fps - SEQUENCE_FPS).abs() < 0.001 {
                0
            } else if let Some(i) = FRAMERATES.iter().position(|(_, f)| (f - v.fps).abs() < 0.001) {
                1 + i
            } else {
                1 + FRAMERATES.len()
            };
            self.width_input.set_text(v.width.to_string());
            self.height_input.set_text(v.height.to_string());
            self.fps_input.set_text(format_fps(v.fps));
            match v.quality {
                VideoQuality::Crf(c) => self.crf = c as f64,
                VideoQuality::Bitrate(kbps) => {
                    self.bitrate_input
                        .set_text(format!("{}", kbps as f64 / 1000.0).replace('.', ","));
                }
            }
            if self.bitrate_input.text.is_empty() {
                self.bitrate_input.set_text("12");
            }
        }
    }

    fn mark_custom(&mut self) {
        self.preset_idx = None;
        self.dirty = true;
    }

    fn revalidate(&mut self, state: &AppState) {
        let Some(s) = &self.settings else { return };
        self.plan = build_render_plan(&state.timeline, &state.media, s);
        self.issues = validate(
            &state.timeline,
            &state.media,
            state.app.ffmpeg.as_ref().map(|f| f.available),
            state.app.encoders.as_ref(),
            s,
        );
        self.dirty = false;
    }

    // --------------------------------------------------------------- Render

    pub fn render(&mut self, ui: &mut Ui, state: &mut AppState, services: &Services) {
        if state.app.open_dialog != Some(DialogId::Export) {
            self.was_open = false;
            return;
        }
        if !self.was_open {
            self.was_open = true;
            self.open_fresh(state);
        }

        // Validierung aktuell halten: Settings-Änderungen, eintreffende
        // Encoder-Liste und asynchrone Store-Änderungen (z. B. Import-Worker).
        let enc_len = state.app.encoders.as_ref().map(|e| e.len()).unwrap_or(0);
        let revs = (state.timeline.revision, state.media.revision);
        if self.dirty || enc_len != self.encoders_seen || revs != self.revs_seen {
            // „Wie Quelle“ folgt der Timeline — wichtig, wenn ein asynchroner
            // Import die erste Quellauflösung erst nach dem Öffnen liefert.
            if revs != self.revs_seen && self.resolution_choice == 0 {
                let source = export::suggested_resolution(&state.timeline, &state.media);
                if let Some(v) = self.settings.as_mut().and_then(|s| s.video.as_mut()) {
                    (v.width, v.height) = source;
                }
                self.width_input.set_text(source.0.to_string());
                self.height_input.set_text(source.1.to_string());
            }
            self.encoders_seen = enc_len;
            self.revs_seen = revs;
            self.revalidate(state);
        }

        // Testmodus: sobald die Validierung durchgeht, automatisch starten
        // (der asynchrone Test-Import braucht ein paar Frames).
        if self.test_autostart && self.stage == Stage::Settings {
            if let Ok(out) = std::env::var("EDITRON_TEST_EXPORT") {
                if self.settings.as_ref().is_some_and(|s| s.output != out) {
                    if let Some(s) = &mut self.settings {
                        s.output = out;
                    }
                    self.revalidate(state);
                }
                if !self.issues.iter().any(|i| i.severity == Severity::Error) {
                    self.test_autostart = false;
                    self.start_export(state, services);
                }
            } else {
                self.test_autostart = false;
            }
        }

        // ESC schließt — außer beim Rendern oder wenn Popup/Textfeld den
        // Tastendruck selbst verarbeitet.
        let esc = ui
            .input
            .keys
            .iter()
            .any(|k| k.key == KeyboardKey::KEY_ESCAPE);
        if esc
            && self.stage != Stage::Running
            && ui.persist.select.popup.is_none()
            && ui.persist.keyboard_focus == 0
        {
            state.app.open_dialog = None;
            return;
        }

        // Abdunkeln + Dialogfläche
        ui.fill(ui.screen, theme::with_alpha(theme::BLACK, 130));
        let w = 780f32.min(ui.screen.w - 32.0);
        let h = 580f32.min(ui.screen.h - 32.0);
        let rect = ui.screen.center_box(w, h);
        drop_shadow(ui, rect, theme::RADIUS_LG);
        ui.fill_rounded(rect, theme::RADIUS_LG, theme::SURFACE_1);
        ui.stroke_rounded(rect, theme::RADIUS_LG, 1.0, theme::LINE_STRONG);

        // ---- Kopfzeile ----
        let mut area = rect;
        let head = area.cut_top(48.0);
        ui.hline(head.x, head.bottom() - 1.0, head.w, theme::LINE);
        let mut hi = head.inset_xy(16.0, 0.0);
        let icon_cell = hi.cut_left(18.0);
        ui.icon("file-output", icon_cell, 18.0, theme::TEXT_2);
        hi.cut_left(8.0);
        let title = format!("Exportieren — {}", state.project.display_name());
        ui.text_left(&title, hi, theme::TEXT_1, FontKind::Sans16Semibold);
        let close = Rect::new(head.right() - 16.0 - 28.0, head.y + 10.0, 28.0, 28.0);
        if IconButton::new("x")
            .tooltip(if self.stage == Stage::Running {
                "Während des Exports gesperrt"
            } else {
                "Schließen (Esc)"
            })
            .disabled(self.stage == Stage::Running)
            .show(ui, "export.close", close)
            .clicked
        {
            state.app.open_dialog = None;
            return;
        }

        match self.stage {
            Stage::Settings => self.render_settings(ui, state, services, area),
            Stage::Running => self.render_running(ui, services, area),
            Stage::Done => self.render_done(ui, state, services, area),
            Stage::Failed => self.render_failed(ui, area),
        }
    }

    // ------------------------------------------------------- Settings-Stage

    fn render_settings(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        services: &Services,
        mut area: Rect,
    ) {
        // ---- Fußbereich: Validierung + Zusammenfassung + Buttons ----
        let shown_issues = self.issues.len().min(3);
        let footer_h = 52.0 + 22.0 + shown_issues as f32 * 18.0 + if shown_issues > 0 { 8.0 } else { 0.0 };
        let footer = area.cut_bottom(footer_h);
        self.render_footer(ui, state, services, footer);

        // ---- Linke Spalte: Render-Presets ----
        let mut presets_col = area.cut_left(PRESET_COL_W);
        ui.vline(presets_col.right(), presets_col.y, presets_col.h, theme::LINE);
        presets_col = presets_col.inset_xy(8.0, 8.0);
        let head = presets_col.cut_top(20.0);
        ui.text_left("PRESETS", head.offset(4.0, 0.0), theme::TEXT_3, FontKind::Sans12Medium);
        let row_h = 26.0;
        let content_h = PRESETS.len() as f32 * row_h + 8.0;
        let view = self.preset_scroll.begin(ui, presets_col, presets_col.w, content_h);
        let mut y = view.origin_y;
        let mut clicked_preset: Option<usize> = None;
        for (i, preset) in PRESETS.iter().enumerate() {
            let row = Rect::new(view.viewport.x, y, view.viewport.w, row_h - 2.0);
            let id = ui.id(("export.preset", i));
            let it = ui.interact(id, row);
            let active = self.preset_idx == Some(i);
            if active {
                ui.fill_rounded(row, theme::RADIUS_SM, theme::ACCENT_SOFT);
            } else if it.hovered {
                ui.fill_rounded(row, theme::RADIUS_SM, theme::SURFACE_2);
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            }
            let fg = if active { theme::TEXT_1 } else { theme::TEXT_2 };
            ui.text_left(preset.label, row.inset_xy(8.0, 0.0), fg, FontKind::Sans12);
            if it.clicked {
                clicked_preset = Some(i);
            }
            y += row_h;
        }
        self.preset_scroll.end(ui, presets_col, presets_col.w, content_h);
        if let Some(i) = clicked_preset {
            self.apply_preset(i, state);
        }

        // ---- Rechte Spalte: Einstellungen (scrollbar; Inhalt passt sich der
        // Breite an, deshalb content_w 0 → nie horizontal scrollen) ----
        let form_area = area.inset_xy(16.0, 12.0);
        let content_h = self.form_content_h.max(form_area.h);
        let view = self.form_scroll.begin(ui, form_area, 0.0, content_h);
        let mut body = Rect::new(view.viewport.x, view.origin_y, view.viewport.w, 4000.0);
        let top_y = body.y;
        self.render_form(ui, state, services, &mut body);
        self.form_content_h = body.y - top_y + 8.0;
        self.form_scroll.end(ui, form_area, 0.0, content_h);
    }

    fn render_form(&mut self, ui: &mut Ui, state: &mut AppState, services: &Services, body: &mut Rect) {
        let Some(mut s) = self.settings.clone() else { return };
        let mut changed = false;
        let mut layout_changed = false;
        // Container-/Video-Wechsel können neue Settings erzeugen — danach
        // alle Auswahl-Indizes und Textfelder neu ableiten.
        let mut needs_full_sync = false;
        // Bereich und Ziel gehören nicht zum Preset (kein „Benutzerdefiniert“).
        let mut keeps_preset = false;
        let encoders = state.app.encoders.clone();

        // ================= Format =================
        section(ui, body, "Format");
        let r = labeled_row(ui, body, "Container");
        let labels: Vec<&str> = CONTAINERS.iter().map(|c| c.label).collect();
        let current = CONTAINERS.iter().position(|c| c.id == s.container.id).unwrap_or(0);
        if let Some(i) = select(ui, "export.container", r, &labels, current) {
            let c = &CONTAINERS[i];
            s.container = c;
            // Codecs auf erlaubte Werte des Containers clampen.
            if c.video {
                let v_fps = s.video.as_ref().map(|v| v.fps).unwrap_or(SEQUENCE_FPS);
                let (vw, vh) = s
                    .video
                    .as_ref()
                    .map(|v| (v.width, v.height))
                    .unwrap_or_else(|| export::suggested_resolution(&state.timeline, &state.media));
                let keep = s
                    .video
                    .as_ref()
                    .filter(|v| c.video_codecs.contains(&v.codec.id));
                if keep.is_none() {
                    s.video = Some(make_video(c.video_codecs[0], vw, vh, v_fps));
                }
            } else {
                s.video = None;
            }
            let audio_ok = s
                .audio
                .as_ref()
                .is_some_and(|a| c.audio_codecs.contains(&a.codec.id));
            if !audio_ok {
                s.audio = Some(make_audio(c.audio_codecs[0]));
            }
            s.output = replace_extension(&s.output, c.ext);
            changed = true;
            layout_changed = true;
            needs_full_sync = true;
        }

        // ================= Ziel =================
        let mut r = labeled_row(ui, body, "Zieldatei");
        let browse = TextButton::new("Durchsuchen…").style(TextButtonStyle::Outline);
        let bw = browse.measure(ui);
        let browse_rect = r.cut_right(bw);
        r.cut_right(8.0);
        let display = if s.output.is_empty() { "—".to_string() } else { s.output.clone() };
        let display = ui.font(FontKind::Mono12).ellipsize(&display, r.w);
        ui.text_left(&display, r, theme::TEXT_1, FontKind::Mono12);
        if browse
            .show(ui, "export.browse", Rect::new(browse_rect.x, browse_rect.y, bw, 24.0))
            .clicked
        {
            let default_name = std::path::Path::new(&s.output)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{}.{}", state.project.display_name(), s.container.ext));
            services.pick_export_target(&default_name, s.container.ext);
        }

        // ================= Video =================
        if s.container.video {
            section(ui, body, "Video");
            let row = body.cut_top(24.0);
            if checkbox(ui, "export.videoOn", row, "Video exportieren", s.video.is_some()).clicked {
                if s.video.is_some() {
                    s.video = None;
                } else {
                    let (w, h) = export::suggested_resolution(&state.timeline, &state.media);
                    s.video = Some(make_video(s.container.video_codecs[0], w, h, SEQUENCE_FPS));
                    needs_full_sync = true;
                }
                changed = true;
                layout_changed = true;
            }
            body.cut_top(8.0);
        }

        if let Some(v) = s.video.clone() {
            // ---- Codec ----
            let r = labeled_row(ui, body, "Codec");
            let codec_labels: Vec<String> = s
                .container
                .video_codecs
                .iter()
                .map(|id| {
                    let c = export::video_codec(id);
                    match &encoders {
                        Some(set) if !set.contains(c.encoder) => {
                            format!("{} — nicht verfügbar", c.label)
                        }
                        _ => c.label.to_string(),
                    }
                })
                .collect();
            let refs: Vec<&str> = codec_labels.iter().map(|s| s.as_str()).collect();
            let current = s
                .container
                .video_codecs
                .iter()
                .position(|id| *id == v.codec.id)
                .unwrap_or(0);
            if let Some(i) = select(ui, "export.vcodec", r, &refs, current) {
                let mut nv = make_video(s.container.video_codecs[i], v.width, v.height, v.fps);
                nv.quality = match (&nv.codec.quality, v.quality) {
                    (QualityKind::CrfOrBitrate { crf }, VideoQuality::Bitrate(k)) => {
                        let _ = crf;
                        VideoQuality::Bitrate(k)
                    }
                    (QualityKind::CrfOrBitrate { crf }, _) => VideoQuality::Crf(crf.2),
                    _ => nv.quality,
                };
                s.video = Some(nv);
                self.sync_quality_inputs(&s);
                changed = true;
                layout_changed = true;
            }

            // ---- Auflösung ----
            let r = labeled_row(ui, body, "Auflösung");
            let source = export::suggested_resolution(&state.timeline, &state.media);
            let source_label = format!("Wie Quelle — {}×{}", source.0, source.1);
            let mut res_labels: Vec<String> = vec![source_label];
            res_labels.extend(RESOLUTIONS.iter().map(|(l, _, _)| l.to_string()));
            res_labels.push("Benutzerdefiniert …".into());
            let refs: Vec<&str> = res_labels.iter().map(|s| s.as_str()).collect();
            if let Some(i) = select(ui, "export.resolution", r, &refs, self.resolution_choice) {
                self.resolution_choice = i;
                if let Some(v) = &mut s.video {
                    if i == 0 {
                        (v.width, v.height) = source;
                    } else if i <= RESOLUTIONS.len() {
                        let (_, w, h) = RESOLUTIONS[i - 1];
                        (v.width, v.height) = (w, h);
                    }
                    self.width_input.set_text(v.width.to_string());
                    self.height_input.set_text(v.height.to_string());
                }
                changed = true;
                layout_changed = true;
            }
            if self.resolution_choice == 1 + RESOLUTIONS.len() {
                let mut r = labeled_row(ui, body, "");
                let wf = r.cut_left(72.0);
                r.cut_left(8.0);
                let xc = r.cut_left(10.0);
                ui.text_centered("×", xc, theme::TEXT_3, FontKind::Sans12);
                r.cut_left(8.0);
                let hf = r.cut_left(72.0);
                let wres = self.width_input.show(ui, "export.width", wf, "Breite");
                let hres = self.height_input.show(ui, "export.height", hf, "Höhe");
                if wres.changed || hres.changed {
                    if let Some(v) = &mut s.video {
                        v.width = self.width_input.text.trim().parse().unwrap_or(0);
                        v.height = self.height_input.text.trim().parse().unwrap_or(0);
                    }
                    changed = true;
                }
            }

            // ---- Framerate ----
            let r = labeled_row(ui, body, "Framerate");
            let seq_label = format!("Wie Sequenz — {} fps", format_fps(SEQUENCE_FPS));
            let mut fps_labels: Vec<String> = vec![seq_label];
            fps_labels.extend(FRAMERATES.iter().map(|(l, _)| format!("{l} fps")));
            fps_labels.push("Benutzerdefiniert …".into());
            let refs: Vec<&str> = fps_labels.iter().map(|s| s.as_str()).collect();
            if let Some(i) = select(ui, "export.fps", r, &refs, self.fps_choice) {
                self.fps_choice = i;
                if let Some(v) = &mut s.video {
                    if i == 0 {
                        v.fps = SEQUENCE_FPS;
                    } else if i <= FRAMERATES.len() {
                        v.fps = FRAMERATES[i - 1].1;
                    }
                    self.fps_input.set_text(format_fps(v.fps));
                }
                changed = true;
                layout_changed = true;
            }
            if self.fps_choice == 1 + FRAMERATES.len() {
                let mut r = labeled_row(ui, body, "");
                let ff = r.cut_left(72.0);
                let fres = self.fps_input.show(ui, "export.fpsCustom", ff, "fps");
                if fres.changed {
                    if let Some(v) = &mut s.video {
                        v.fps = self
                            .fps_input
                            .text
                            .trim()
                            .replace(',', ".")
                            .parse()
                            .unwrap_or(0.0);
                    }
                    changed = true;
                }
            }

            // ---- Qualität ----
            match v.codec.quality {
                QualityKind::CrfOrBitrate { crf: (min, max, _) } => {
                    let r = labeled_row(ui, body, "Qualität");
                    let mode = match v.quality {
                        VideoQuality::Crf(_) => 0,
                        VideoQuality::Bitrate(_) => 1,
                    };
                    let mut rr = r;
                    let mode_cell = rr.cut_left(190.0);
                    if let Some(i) = select(
                        ui,
                        "export.qmode",
                        mode_cell,
                        &["Konstante Qualität (CRF)", "Ziel-Bitrate (VBR)"],
                        mode,
                    ) {
                        if let Some(v) = &mut s.video {
                            v.quality = if i == 0 {
                                VideoQuality::Crf(self.crf.round() as u32)
                            } else {
                                VideoQuality::Bitrate(parse_mbits(&self.bitrate_input.text))
                            };
                        }
                        changed = true;
                        layout_changed = true;
                    }
                    rr.cut_left(12.0);
                    match v.quality {
                        VideoQuality::Crf(_) => {
                            let value_cell = rr.cut_right(34.0);
                            rr.cut_right(8.0);
                            self.crf = self.crf.clamp(min as f64, max as f64);
                            let before = self.crf;
                            slider(ui, "export.crf", rr, &mut self.crf, min as f64, max as f64, theme::ACCENT);
                            self.crf = self.crf.round();
                            ui.text_right(
                                &format!("{}", self.crf as i64),
                                value_cell,
                                theme::TEXT_1,
                                FontKind::Mono12,
                            );
                            if (self.crf - before).abs() > 0.01 {
                                if let Some(v) = &mut s.video {
                                    v.quality = VideoQuality::Crf(self.crf as u32);
                                }
                                changed = true;
                            }
                        }
                        VideoQuality::Bitrate(_) => {
                            let field = rr.cut_left(72.0);
                            rr.cut_left(8.0);
                            ui.text_left("Mbit/s", rr, theme::TEXT_3, FontKind::Sans12);
                            let res = self.bitrate_input.show(ui, "export.bitrate", field, "12");
                            if res.changed {
                                if let Some(v) = &mut s.video {
                                    v.quality = VideoQuality::Bitrate(parse_mbits(&self.bitrate_input.text));
                                }
                                changed = true;
                            }
                        }
                    }
                }
                QualityKind::Profiles(profiles) => {
                    let r = labeled_row(ui, body, "Profil");
                    let labels: Vec<&str> = profiles.iter().map(|(_, l, _)| *l).collect();
                    if let Some(i) = select(ui, "export.profile", r, &labels, v.profile.min(labels.len() - 1)) {
                        if let Some(v) = &mut s.video {
                            v.profile = i;
                        }
                        changed = true;
                    }
                }
            }

            // ---- Encoder-Tempo ----
            if !v.codec.speed_presets.is_empty() {
                let label = if v.codec.id == "av1" { "SVT-Preset" } else { "Encoder-Tempo" };
                let r = labeled_row(ui, body, label);
                if let Some(i) = select(
                    ui,
                    "export.speed",
                    r,
                    v.codec.speed_presets,
                    v.speed.min(v.codec.speed_presets.len() - 1),
                ) {
                    if let Some(v) = &mut s.video {
                        v.speed = i;
                    }
                    changed = true;
                }
            }
        }

        // ================= Audio =================
        section(ui, body, "Audio");
        if s.container.video {
            let row = body.cut_top(24.0);
            if checkbox(ui, "export.audioOn", row, "Audio exportieren", s.audio.is_some()).clicked {
                s.audio = if s.audio.is_some() {
                    None
                } else {
                    Some(make_audio(s.container.audio_codecs[0]))
                };
                changed = true;
                layout_changed = true;
            }
            body.cut_top(8.0);
        }

        if let Some(a) = s.audio.clone() {
            let r = labeled_row(ui, body, "Codec");
            let labels: Vec<String> = s
                .container
                .audio_codecs
                .iter()
                .map(|id| {
                    let c = export::audio_codec(id);
                    match &encoders {
                        Some(set) if !set.contains(c.encoder) => {
                            format!("{} — nicht verfügbar", c.label)
                        }
                        _ => c.label.to_string(),
                    }
                })
                .collect();
            let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
            let current = s
                .container
                .audio_codecs
                .iter()
                .position(|id| *id == a.codec.id)
                .unwrap_or(0);
            if let Some(i) = select(ui, "export.acodec", r, &refs, current) {
                s.audio = Some(make_audio(s.container.audio_codecs[i]));
                changed = true;
                layout_changed = true;
            }

            if !a.codec.bitrates.is_empty() {
                let r = labeled_row(ui, body, "Bitrate");
                let labels: Vec<String> =
                    a.codec.bitrates.iter().map(|b| format!("{b} kbit/s")).collect();
                let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
                let current = a
                    .codec
                    .bitrates
                    .iter()
                    .position(|b| *b == a.bitrate_kbps)
                    .unwrap_or(0);
                if let Some(i) = select(ui, "export.abitrate", r, &refs, current) {
                    if let Some(audio) = &mut s.audio {
                        audio.bitrate_kbps = a.codec.bitrates[i];
                    }
                    changed = true;
                }
            }

            let mut r = labeled_row(ui, body, "Samplerate");
            let rate_cell = r.cut_left(120.0);
            if a.codec.forced_rate.is_some() {
                ui.text_left("48.000 Hz (Opus)", rate_cell, theme::TEXT_3, FontKind::Sans12);
            } else {
                let labels: Vec<String> = SAMPLE_RATES
                    .iter()
                    .map(|r| format!("{} Hz", group_thousands(*r as u64)))
                    .collect();
                let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
                let current = SAMPLE_RATES.iter().position(|r| *r == a.sample_rate).unwrap_or(1);
                if let Some(i) = select(ui, "export.arate", rate_cell, &refs, current) {
                    if let Some(audio) = &mut s.audio {
                        audio.sample_rate = SAMPLE_RATES[i];
                    }
                    changed = true;
                }
            }
            r.cut_left(16.0);
            let ch_label = r.cut_left(46.0);
            ui.text_left("Kanäle", ch_label, theme::TEXT_2, FontKind::Sans12);
            let ch_cell = r.cut_left(92.0);
            let current = if a.channels == 1 { 0 } else { 1 };
            if let Some(i) = select(ui, "export.channels", ch_cell, &["Mono", "Stereo"], current) {
                if let Some(audio) = &mut s.audio {
                    audio.channels = if i == 0 { 1 } else { 2 };
                }
                changed = true;
            }
        }

        // ================= Bereich =================
        section(ui, body, "Bereich");
        let r = labeled_row(ui, body, "Exportieren");
        let in_out_ready = state.timeline.in_point.is_some() && state.timeline.out_point.is_some();
        let range_labels = if in_out_ready {
            ["Gesamte Sequenz", "Zwischen In- und Out-Punkt"]
        } else {
            ["Gesamte Sequenz", "Zwischen In/Out — nicht gesetzt"]
        };
        let current = if s.use_in_out { 1 } else { 0 };
        if let Some(i) = select(ui, "export.range", r, &range_labels, current) {
            s.use_in_out = i == 1 && in_out_ready;
            changed = true;
            keeps_preset = true;
        }

        if changed {
            self.settings = Some(s);
            if needs_full_sync {
                self.sync_inputs_from_settings(state);
            } else if layout_changed {
                self.sync_quality_inputs_from_self();
            }
            if keeps_preset {
                self.dirty = true;
            } else {
                self.mark_custom();
            }
        }
    }

    /// CRF-/Bitrate-Felder nach Codec-Wechsel an die neuen Settings angleichen.
    fn sync_quality_inputs(&mut self, s: &ExportSettings) {
        if let Some(v) = &s.video {
            match v.quality {
                VideoQuality::Crf(c) => self.crf = c as f64,
                VideoQuality::Bitrate(kbps) => self
                    .bitrate_input
                    .set_text(format!("{}", kbps as f64 / 1000.0).replace('.', ",")),
            }
        }
    }

    fn sync_quality_inputs_from_self(&mut self) {
        if let Some(s) = self.settings.clone() {
            self.sync_quality_inputs(&s);
        }
    }

    // ----------------------------------------------------------- Fußbereich

    fn render_footer(&mut self, ui: &mut Ui, state: &mut AppState, services: &Services, footer: Rect) {
        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        let mut f = footer.inset_xy(16.0, 0.0);
        f.cut_top(8.0);

        // Validierung (max. 3 Zeilen, Fehler zuerst).
        let mut sorted: Vec<&export::ValidationIssue> = self.issues.iter().collect();
        sorted.sort_by_key(|i| (i.severity != Severity::Error) as u8);
        let extra = sorted.len().saturating_sub(3);
        for issue in sorted.iter().take(3) {
            let mut row = f.cut_top(18.0);
            let icon_cell = row.cut_left(14.0);
            row.cut_left(6.0);
            let (icon, color) = match issue.severity {
                Severity::Error => ("circle-alert", theme::DANGER),
                Severity::Warning => ("triangle-alert", theme::WARNING),
            };
            ui.icon(icon, icon_cell, 13.0, color);
            let mut msg = issue.message.clone();
            if extra > 0 && std::ptr::eq(*issue, sorted[2]) {
                msg = format!("{msg}  (+{extra} weitere)");
            }
            let msg = ui.font(FontKind::Sans12).ellipsize(&msg, row.w);
            ui.text_left(&msg, row, color, FontKind::Sans12);
        }
        if !sorted.is_empty() {
            f.cut_top(8.0);
        }

        // Zusammenfassung
        let summary_row = f.cut_top(20.0);
        if let Some(s) = &self.settings {
            let mut parts: Vec<String> = Vec::new();
            parts.push(format!(
                "Dauer {} ({} Frames)",
                format_duration(self.plan.duration),
                group_thousands(self.plan.total_frames)
            ));
            if let Some(v) = &s.video {
                parts.push(format!("{}×{} @ {} fps", v.width, v.height, format_fps(v.fps)));
            }
            if let Some(a) = &s.audio {
                parts.push(format!(
                    "{} {}",
                    a.codec.label,
                    if a.channels == 1 { "Mono" } else { "Stereo" }
                ));
            }
            let size = match export::estimate_size(s, self.plan.duration) {
                Some(b) => format!("≈ {}", export::format_bytes(b)),
                None => "Größe inhaltsabhängig".to_string(),
            };
            parts.push(size);
            let text = parts.join("  ·  ");
            let text = ui.font(FontKind::Sans12).ellipsize(&text, summary_row.w);
            ui.text_left(&text, summary_row, theme::TEXT_3, FontKind::Sans12);
        }

        // Buttons
        let btn_row = f.cut_top(40.0);
        let has_error = self.issues.iter().any(|i| i.severity == Severity::Error);
        let start_rect = Rect::new(btn_row.right() - 130.0, btn_row.y + 6.0, 130.0, 28.0);
        if primary_button(ui, "export.start", start_rect, "Exportieren", !has_error).clicked
            && !has_error
        {
            self.start_export(state, services);
        }
        let cancel = TextButton::new("Schließen").style(TextButtonStyle::Outline);
        let cw = cancel.measure(ui);
        if cancel
            .show(
                ui,
                "export.cancelDialog",
                Rect::new(start_rect.x - 8.0 - cw, btn_row.y + 6.0, cw, 28.0),
            )
            .clicked
        {
            state.app.open_dialog = None;
        }
    }

    fn start_export(&mut self, state: &mut AppState, services: &Services) {
        // Letzte Prüfung direkt vor dem Start (Pfad kann sich geändert haben).
        self.revalidate(state);
        if self.issues.iter().any(|i| i.severity == Severity::Error) {
            return;
        }
        let Some(settings) = self.settings.clone() else { return };
        // Wiedergabe stoppen — der Export soll alle Decode-Ressourcen haben.
        state.playback.program_playing = false;
        state.playback.source.playing = false;
        match services.start_sequence_export(self.plan.clone(), settings) {
            Ok(job_id) => {
                self.job_id = Some(job_id);
                self.stage = Stage::Running;
                self.progress_pct = 0.0;
                self.frames_done = 0;
                self.frames_total = self.plan.total_frames;
                self.render_fps = 0.0;
                self.eta_sec = None;
                self.error = None;
                self.started_at = -1.0; // im Render-Pass mit ui.time initialisieren
            }
            Err(err) => {
                self.stage = Stage::Failed;
                self.error = Some(err);
            }
        }
    }

    // ------------------------------------------------------- Running-Stage

    fn render_running(&mut self, ui: &mut Ui, services: &Services, area: Rect) {
        if self.started_at < 0.0 {
            self.started_at = ui.time;
        }
        let Some(s) = &self.settings else { return };
        let panel = area.center_box(area.w - 120.0, 190.0);
        let mut p = panel;

        let name = std::path::Path::new(&s.output)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| s.output.clone());
        let head = p.cut_top(24.0);
        ui.text_left(
            &format!("Exportiere „{name}“"),
            head,
            theme::TEXT_1,
            FontKind::Sans14Semibold,
        );
        let phase_row = p.cut_top(20.0);
        ui.text_left(
            &format!("{} …", self.phase.label()),
            phase_row,
            theme::TEXT_2,
            FontKind::Sans12,
        );
        p.cut_top(10.0);

        // Fortschrittsbalken + Prozent
        let bar_row = p.cut_top(22.0);
        let mut bar = bar_row;
        let pct_cell = bar.cut_right(52.0);
        bar.cut_right(10.0);
        let track = Rect::new(bar.x, bar.y + 7.0, bar.w, 8.0);
        ui.fill_rounded(track, 4.0, theme::SURFACE_3);
        let frac = (self.progress_pct / 100.0).clamp(0.0, 1.0) as f32;
        if frac > 0.0 {
            ui.fill_rounded(Rect::new(track.x, track.y, (track.w * frac).max(8.0), 8.0), 4.0, theme::ACCENT);
        }
        ui.text_right(
            &format!("{:.0} %", self.progress_pct),
            pct_cell,
            theme::TEXT_1,
            FontKind::Sans14Semibold,
        );
        p.cut_top(8.0);

        // Frames + Geschwindigkeit
        let detail = p.cut_top(18.0);
        let mut text = String::new();
        if self.frames_total > 0 {
            text.push_str(&format!(
                "Frame {} / {}",
                group_thousands(self.frames_done),
                group_thousands(self.frames_total)
            ));
            if self.render_fps > 0.0 {
                let target_fps = s.video.as_ref().map(|v| v.fps).unwrap_or(SEQUENCE_FPS);
                text.push_str(&format!(
                    "   ·   {:.0} fps ({}× Echtzeit)",
                    self.render_fps,
                    format!("{:.1}", self.render_fps / target_fps).replace('.', ",")
                ));
            }
        }
        ui.text_left(&text, detail, theme::TEXT_2, FontKind::Mono12);

        // Zeiten
        let times = p.cut_top(18.0);
        let elapsed = (ui.time - self.started_at).max(0.0);
        let eta = match self.eta_sec {
            Some(e) if e.is_finite() => format!("Verbleibend ca. {}", format_duration(e)),
            _ => "Verbleibend wird berechnet …".to_string(),
        };
        ui.text_left(
            &format!("Verstrichen {}   ·   {}", format_duration(elapsed), eta),
            times,
            theme::TEXT_2,
            FontKind::Mono12,
        );
        p.cut_top(16.0);

        // Abbrechen
        let btn_row = p.cut_top(28.0);
        let cancel = TextButton::new("Abbrechen").icon("ban").style(TextButtonStyle::Outline);
        let cw = cancel.measure(ui);
        if cancel
            .show(ui, "export.cancelJob", Rect::new(btn_row.x, btn_row.y, cw, 28.0))
            .clicked
        {
            if let Some(id) = &self.job_id {
                services.cancel_job(id);
            }
        }
    }

    // ---------------------------------------------------------- Done/Failed

    fn render_done(&mut self, ui: &mut Ui, state: &mut AppState, services: &Services, area: Rect) {
        let panel = area.center_box(area.w - 120.0, 150.0);
        let mut p = panel;
        let mut head = p.cut_top(26.0);
        let icon_cell = head.cut_left(22.0);
        ui.icon("circle-check", icon_cell, 20.0, theme::SUCCESS);
        head.cut_left(8.0);
        ui.text_left("Export abgeschlossen", head, theme::SUCCESS, FontKind::Sans16Semibold);
        p.cut_top(8.0);

        let path_row = p.cut_top(20.0);
        let mut line = self.finished_output.clone();
        if let Ok(meta) = std::fs::metadata(&self.finished_output) {
            line.push_str(&format!("  ({})", export::format_bytes(meta.len())));
        }
        let line = ui.font(FontKind::Mono12).ellipsize(&line, path_row.w);
        ui.text_left(&line, path_row, theme::TEXT_2, FontKind::Mono12);
        p.cut_top(16.0);

        let btn_row = p.cut_top(28.0);
        let reveal = TextButton::new("Im Dateimanager zeigen")
            .icon("folder-open")
            .style(TextButtonStyle::Outline);
        let rw = reveal.measure(ui);
        if reveal
            .show(ui, "export.reveal", Rect::new(btn_row.x, btn_row.y, rw, 28.0))
            .clicked
        {
            let _ = services.reveal_in_file_manager(&self.finished_output);
        }
        let close_rect = Rect::new(btn_row.x + rw + 8.0, btn_row.y, 110.0, 28.0);
        if primary_button(ui, "export.doneClose", close_rect, "Schließen", true).clicked {
            self.stage = Stage::Settings;
            self.dirty = true;
            state.app.open_dialog = None;
        }
    }

    fn render_failed(&mut self, ui: &mut Ui, area: Rect) {
        let panel = area.center_box(area.w - 120.0, 220.0);
        let mut p = panel;
        let mut head = p.cut_top(26.0);
        let icon_cell = head.cut_left(22.0);
        ui.icon("circle-alert", icon_cell, 20.0, theme::DANGER);
        head.cut_left(8.0);
        ui.text_left("Export fehlgeschlagen", head, theme::DANGER, FontKind::Sans16Semibold);
        p.cut_top(8.0);

        let msg = self
            .error
            .clone()
            .unwrap_or_else(|| "Unbekannter Fehler".to_string());
        let box_rect = p.cut_top(96.0);
        ui.fill_rounded(box_rect, theme::RADIUS_SM, theme::SURFACE_0);
        ui.stroke_rounded(box_rect, theme::RADIUS_SM, 1.0, theme::with_alpha(theme::DANGER, 120));
        let mut text_area = box_rect.inset_xy(10.0, 6.0);
        for line in msg.lines().take(5) {
            let row = text_area.cut_top(17.0);
            let line = ui.font(FontKind::Mono12).ellipsize(line, row.w);
            ui.text_left(&line, row, theme::TEXT_1, FontKind::Mono12);
        }
        p.cut_top(14.0);

        let btn_row = p.cut_top(28.0);
        let back_rect = Rect::new(btn_row.x, btn_row.y, 220.0, 28.0);
        if primary_button(ui, "export.backToSettings", back_rect, "Zurück zu den Einstellungen", true)
            .clicked
        {
            self.stage = Stage::Settings;
            self.dirty = true;
        }
    }
}

// ------------------------------------------------------------ UI-Bausteine

/// Beschriftete Formularzeile: Label links (LABEL_W), Feld rechts.
fn labeled_row(ui: &mut Ui, body: &mut Rect, label: &str) -> Rect {
    let mut row = body.cut_top(24.0);
    let label_cell = row.cut_left(LABEL_W);
    if !label.is_empty() {
        ui.text_left(label, label_cell, theme::TEXT_2, FontKind::Sans12);
    }
    body.cut_top(8.0);
    row
}

fn section(ui: &mut Ui, body: &mut Rect, title: &str) {
    body.cut_top(4.0);
    let row = body.cut_top(18.0);
    ui.text_left(title, row, theme::TEXT_3, FontKind::Sans12Medium);
    let line_y = row.bottom() + 2.0;
    ui.hline(row.x, line_y, row.w, theme::LINE);
    body.cut_top(10.0);
}

/// Einfache Checkbox (14 px Kästchen + Label).
fn checkbox(ui: &mut Ui, id_src: impl std::hash::Hash, row: Rect, label: &str, checked: bool) -> Interaction {
    let id = ui.id(id_src);
    let label_w = ui.font(FontKind::Sans12).width(label);
    let hit = Rect::new(row.x, row.y, 14.0 + 8.0 + label_w, row.h);
    let it = ui.interact(id, hit);
    let box_rect = Rect::new(row.x, row.y + (row.h - 14.0) / 2.0, 14.0, 14.0);
    if checked {
        ui.fill_rounded(box_rect, 3.0, theme::ACCENT);
        ui.icon("check", box_rect, 11.0, theme::WHITE);
    } else {
        ui.fill_rounded(box_rect, 3.0, theme::SURFACE_2);
        ui.stroke_rounded(
            box_rect,
            3.0,
            1.0,
            if it.hovered { theme::LINE_STRONG } else { theme::LINE },
        );
    }
    let text_rect = Rect::new(box_rect.right() + 8.0, row.y, label_w + 4.0, row.h);
    ui.text_left(
        label,
        text_rect,
        if it.hovered { theme::TEXT_1 } else { theme::TEXT_2 },
        FontKind::Sans12,
    );
    if it.hovered {
        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
    }
    it
}

/// Akzentfarbener Primär-Button.
fn primary_button(ui: &mut Ui, id_src: impl std::hash::Hash, rect: Rect, label: &str, enabled: bool) -> Interaction {
    let id = ui.id(id_src);
    let it = if enabled { ui.interact(id, rect) } else { Interaction::default() };
    let bg = if !enabled {
        theme::with_alpha(theme::ACCENT, 90)
    } else if it.hovered || it.held {
        theme::ACCENT_HOVER
    } else {
        theme::ACCENT
    };
    ui.fill_rounded(rect, theme::RADIUS_SM, bg);
    let fg = if enabled { theme::WHITE } else { theme::with_alpha(theme::WHITE, 160) };
    ui.text_centered(label, rect, fg, FontKind::Sans12Medium);
    if it.hovered && enabled {
        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
    }
    it
}

// ---------------------------------------------------------------- Helfer

fn make_video(codec_id: &str, width: u32, height: u32, fps: f64) -> VideoSettings {
    let codec = export::video_codec(codec_id);
    let quality = match codec.quality {
        QualityKind::CrfOrBitrate { crf } => VideoQuality::Crf(crf.2),
        QualityKind::Profiles(_) => VideoQuality::Crf(0),
    };
    VideoSettings {
        codec,
        width,
        height,
        fps,
        quality,
        speed: codec.default_speed,
        profile: match codec.id {
            "prores" => 3, // 422 HQ
            "dnxhr" => 2,  // HQ
            _ => 0,
        },
    }
}

fn make_audio(codec_id: &str) -> AudioSettings {
    let codec = export::audio_codec(codec_id);
    AudioSettings {
        codec,
        bitrate_kbps: codec.default_bitrate,
        sample_rate: codec.forced_rate.unwrap_or(48000),
        channels: 2,
    }
}

fn default_output_path(state: &AppState, ext: &str) -> String {
    let name = state.project.display_name();
    let dir = dirs::video_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    dir.join(format!("{name}.{ext}")).to_string_lossy().into_owned()
}

fn default_output_path_raw(ext: &str) -> String {
    let dir = dirs::video_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    dir.join(format!("Export.{ext}")).to_string_lossy().into_owned()
}

fn replace_extension(path: &str, ext: &str) -> String {
    let p = std::path::Path::new(path);
    match (p.parent(), p.file_stem()) {
        (Some(dir), Some(stem)) if !stem.to_string_lossy().is_empty() => dir
            .join(format!("{}.{ext}", stem.to_string_lossy()))
            .to_string_lossy()
            .into_owned(),
        _ => format!("{path}.{ext}"),
    }
}

fn parse_mbits(text: &str) -> u32 {
    let v: f64 = text.trim().replace(',', ".").parse().unwrap_or(0.0);
    (v * 1000.0).round().max(0.0) as u32
}

fn format_fps(fps: f64) -> String {
    if (fps - fps.round()).abs() < 1e-9 {
        format!("{}", fps.round() as u64)
    } else {
        format!("{fps:.3}").trim_end_matches('0').trim_end_matches('.').replace('.', ",")
    }
}

/// 12345 → "12.345" (deutsche Tausendergruppierung).
fn group_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('.');
        }
        out.push(c);
    }
    out
}
