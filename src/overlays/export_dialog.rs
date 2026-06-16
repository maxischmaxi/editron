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
    self, build_render_plan, loudness_preset_index, validate, AudioSettings, EncoderQuality,
    ExportSettings, LoudnessNorm, QualityKind, Severity, VideoQuality, VideoSettings, CONTAINERS,
    FRAMERATES, LOUDNESS_PRESETS, PRESETS, RESOLUTIONS, SAMPLE_RATES,
};
use crate::core::export_preset::{PresetData, UserPresets};
use crate::core::render_queue::JobState;
use crate::core::timecode::format_duration;
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
enum Tab {
    /// Einstellungen + Presets — Jobs in die Warteschlange legen.
    Settings,
    /// Render-Warteschlange — laufende/wartende/fertige Jobs.
    Queue,
}

pub struct ExportDialog {
    settings: Option<ExportSettings>,
    /// Aktives eingebautes Render-Preset; None = Benutzerdefiniert/Nutzer-Preset.
    preset_idx: Option<usize>,
    /// Aktives Nutzer-Preset (Name), falls eines gewählt ist.
    user_preset: Option<String>,
    /// 0 = „Wie Quelle“, 1..=RESOLUTIONS = Vorgabe, danach Benutzerdefiniert.
    resolution_choice: usize,
    /// 0 = „Wie Sequenz“, 1..=FRAMERATES = Vorgabe, danach Benutzerdefiniert.
    fps_choice: usize,
    width_input: TextInputState,
    height_input: TextInputState,
    fps_input: TextInputState,
    bitrate_input: TextInputState,
    start_num_input: TextInputState,
    /// Eingabefeld für den Namen eines neuen/zu überschreibenden Presets.
    preset_name_input: TextInputState,
    crf: f64,

    issues: Vec<export::ValidationIssue>,
    plan: export::RenderPlan,
    dirty: bool,
    encoders_seen: usize,
    /// Timeline-/Medien-Revision der letzten Validierung (Async-Import).
    revs_seen: (u64, u64),

    /// Geladene Nutzer-Presets (XDG-Config).
    user_presets: UserPresets,

    tab: Tab,

    form_scroll: ScrollState,
    form_content_h: f32,
    preset_scroll: ScrollState,
    queue_scroll: ScrollState,
    was_open: bool,
    /// Testmodus: Export automatisch starten (EDITRON_TEST_EXPORT=<ziel>).
    test_autostart: bool,
}

impl Default for ExportDialog {
    fn default() -> Self {
        ExportDialog {
            settings: None,
            preset_idx: None,
            user_preset: None,
            resolution_choice: 0,
            fps_choice: 0,
            width_input: TextInputState::default(),
            height_input: TextInputState::default(),
            fps_input: TextInputState::default(),
            bitrate_input: TextInputState::default(),
            start_num_input: TextInputState::default(),
            preset_name_input: TextInputState::default(),
            crf: 20.0,
            issues: Vec::new(),
            plan: export::RenderPlan::default(),
            dirty: true,
            encoders_seen: usize::MAX,
            revs_seen: (u64::MAX, u64::MAX),
            user_presets: UserPresets::load(),
            tab: Tab::Settings,
            form_scroll: ScrollState::default(),
            form_content_h: 0.0,
            preset_scroll: ScrollState::default(),
            queue_scroll: ScrollState::default(),
            was_open: false,
            test_autostart: std::env::var("EDITRON_TEST_EXPORT").is_ok(),
        }
    }
}

impl ExportDialog {
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
        if self.settings.is_none() {
            self.apply_preset(3, state); // H.264 Master als Allzweck-Default
        }
        if let Some(s) = &mut self.settings {
            if s.output.is_empty() {
                s.output = default_output_path(state, s.container.ext);
            }
            // Testmodus: Lautheits-Normalisierung vorbelegen
            // (EDITRON_TEST_LOUDNESS=ebu|-14|-16,-1,11 …) — für Screenshots des
            // Dialogabschnitts und die Delivery-Verifikation per
            // EDITRON_TEST_EXPORT.
            if s.audio.is_some() {
                if let Ok(spec) = std::env::var("EDITRON_TEST_LOUDNESS") {
                    s.loudness = parse_test_loudness(&spec);
                }
                // Stems-Export vorbelegen (EDITRON_TEST_STEMS=1) — für die
                // Delivery-Verifikation per EDITRON_TEST_EXPORT (je Audiospur
                // ein eigener Stream). Nur bei Containern mit mehreren Streams.
                if s.container.multi_audio() && std::env::var("EDITRON_TEST_STEMS").is_ok() {
                    s.audio_stems = true;
                    s.loudness = None;
                }
            }
        }
        self.dirty = true;
    }

    fn apply_preset(&mut self, idx: usize, state: &AppState) {
        let Some(preset) = PRESETS.get(idx) else { return };
        let seq = state.timeline.settings;
        let mut settings = (preset.build)((seq.width, seq.height), seq.rate.fps());
        // Zielpfad behalten, nur die Endung dem Container anpassen.
        settings.output = match self.settings.as_ref() {
            Some(old) if !old.output.is_empty() => {
                replace_extension(&old.output, settings.container.ext)
            }
            _ => default_output_path_raw(settings.container.ext),
        };
        settings.use_in_out = self.settings.as_ref().map(|s| s.use_in_out).unwrap_or(false);
        // Untertitel-Wahl über Preset-Wechsel hinweg behalten.
        settings.subtitles = self
            .settings
            .as_ref()
            .map(|s| s.subtitles)
            .unwrap_or_default();
        self.preset_idx = Some(idx);
        self.user_preset = None;
        self.settings = Some(settings);
        self.sync_inputs_from_settings(state);
        self.dirty = true;
    }

    /// Ein gespeichertes Nutzer-Preset anwenden (konkrete Werte).
    fn apply_user_preset(&mut self, name: &str, state: &AppState) {
        let Some(data) = self.user_presets.get(name).cloned() else { return };
        // Zielpfad/Endung wie bei den eingebauten Presets behandeln.
        let ext = export::container(&data.container).ext;
        let output = match self.settings.as_ref() {
            Some(old) if !old.output.is_empty() => replace_extension(&old.output, ext),
            _ => default_output_path_raw(ext),
        };
        let mut settings = data.to_settings(output);
        settings.use_in_out = self.settings.as_ref().map(|s| s.use_in_out).unwrap_or(false);
        self.preset_idx = None;
        self.user_preset = Some(name.to_string());
        self.settings = Some(settings);
        self.sync_inputs_from_settings(state);
        self.dirty = true;
    }

    /// Wahl-Indizes + Textfelder aus den Settings ableiten (nach Preset).
    fn sync_inputs_from_settings(&mut self, state: &AppState) {
        let Some(s) = &self.settings else { return };
        if let Some(v) = &s.video {
            let seq = state.timeline.settings;
            self.resolution_choice = if (v.width, v.height) == (seq.width, seq.height) {
                0
            } else if let Some(i) = RESOLUTIONS.iter().position(|(_, w, h)| (*w, *h) == (v.width, v.height)) {
                1 + i
            } else {
                1 + RESOLUTIONS.len()
            };
            self.fps_choice = if (v.fps - seq.rate.fps()).abs() < 0.001 {
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
        self.start_num_input.set_text(s.image_start.to_string());
    }

    fn mark_custom(&mut self) {
        self.preset_idx = None;
        self.user_preset = None;
        self.dirty = true;
    }

    fn revalidate(&mut self, state: &AppState) {
        let Some(s) = &self.settings else { return };
        self.plan = build_render_plan(&state.timeline, &state.media, s, &state.timeline);
        self.issues = validate(
            &state.timeline,
            &state.media,
            state.app.ffmpeg.as_ref().map(|f| f.available),
            state.app.encoders.as_ref(),
            s,
            &state.timeline,
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
            // „Wie Sequenz“ folgt den Sequenz-Einstellungen — wichtig, wenn
            // sich die Sequenz ändert (Settings-Dialog/Media-Match), während
            // der Export-Dialog bereits Settings hält.
            if revs != self.revs_seen && self.resolution_choice == 0 {
                let seq = state.timeline.settings;
                if let Some(v) = self.settings.as_mut().and_then(|s| s.video.as_mut()) {
                    (v.width, v.height) = (seq.width, seq.height);
                }
                self.width_input.set_text(seq.width.to_string());
                self.height_input.set_text(seq.height.to_string());
            }
            if revs != self.revs_seen && self.fps_choice == 0 {
                let fps = state.timeline.settings.rate.fps();
                if let Some(v) = self.settings.as_mut().and_then(|s| s.video.as_mut()) {
                    v.fps = fps;
                }
                self.fps_input.set_text(format_fps(fps));
            }
            self.encoders_seen = enc_len;
            self.revs_seen = revs;
            self.revalidate(state);
        }

        // Über die Statusleiste/„Warteschlange öffnen" angefordert: Queue-Tab.
        if state.app.export_open_queue {
            state.app.export_open_queue = false;
            self.tab = Tab::Queue;
        }

        // Testmodus: sobald die Validierung durchgeht, automatisch in die
        // Warteschlange legen + Queue-Tab zeigen (der asynchrone Test-Import
        // braucht ein paar Frames). Die Running-/Done-Screenshots zeigen dann
        // den Job in der Warteschlange.
        if self.test_autostart {
            if let Ok(out) = std::env::var("EDITRON_TEST_EXPORT") {
                if self.settings.as_ref().is_some_and(|s| s.output != out) {
                    if let Some(s) = &mut self.settings {
                        s.output = out;
                    }
                    self.revalidate(state);
                }
                if !self.issues.iter().any(|i| i.severity == Severity::Error) {
                    self.test_autostart = false;
                    self.enqueue(state);
                    self.tab = Tab::Queue;
                }
            } else {
                self.test_autostart = false;
            }
        }

        // ESC schließt — außer wenn Popup/Textfeld den Tastendruck selbst
        // verarbeitet. Der Hintergrund-Export läuft beim Schließen weiter.
        let esc = ui
            .input
            .keys
            .iter()
            .any(|k| k.key == KeyboardKey::KEY_ESCAPE);
        if esc && ui.persist.select.popup.is_none() && ui.persist.keyboard_focus == 0 {
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
            .tooltip("Schließen (Esc) — Export läuft im Hintergrund weiter")
            .show(ui, "export.close", close)
            .clicked
        {
            state.app.open_dialog = None;
            return;
        }

        // ---- Tab-Leiste ----
        let active = state.render_queue.active_count();
        let tab_row = area.cut_top(36.0);
        ui.hline(tab_row.x, tab_row.bottom() - 1.0, tab_row.w, theme::LINE);
        let mut tx = tab_row.inset_xy(16.0, 0.0);
        if self.tab_button(ui, tx.cut_left(140.0), "Einstellungen", self.tab == Tab::Settings) {
            self.tab = Tab::Settings;
        }
        tx.cut_left(4.0);
        let queue_label = if active > 0 {
            format!("Warteschlange ({active})")
        } else {
            "Warteschlange".to_string()
        };
        if self.tab_button(ui, tx.cut_left(170.0), &queue_label, self.tab == Tab::Queue) {
            self.tab = Tab::Queue;
        }

        match self.tab {
            Tab::Settings => self.render_settings(ui, state, services, area),
            Tab::Queue => self.render_queue(ui, state, services, area),
        }
    }

    /// Tab-Schaltfläche mit Unterstreichung des aktiven Tabs.
    fn tab_button(&self, ui: &mut Ui, rect: Rect, label: &str, active: bool) -> bool {
        let id = ui.id(("export.tab", label));
        let it = ui.interact(id, rect);
        if it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
        }
        let fg = if active {
            theme::TEXT_1
        } else if it.hovered {
            theme::TEXT_2
        } else {
            theme::TEXT_3
        };
        ui.text_left(label, rect.inset_xy(8.0, 0.0), fg, FontKind::Sans12Medium);
        if active {
            ui.fill(Rect::new(rect.x, rect.bottom() - 2.0, rect.w, 2.0), theme::ACCENT);
        }
        it.clicked
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
        self.render_footer(ui, state, footer);

        // ---- Linke Spalte: Render-Presets ----
        let mut presets_col = area.cut_left(PRESET_COL_W);
        ui.vline(presets_col.right(), presets_col.y, presets_col.h, theme::LINE);
        presets_col = presets_col.inset_xy(8.0, 8.0);
        // Festen Speicher-Bereich unten abschneiden (Name + Speichern/Löschen).
        let save_area = presets_col.cut_bottom(64.0);
        self.render_preset_save(ui, save_area);

        let head = presets_col.cut_top(20.0);
        ui.text_left("PRESETS", head.offset(4.0, 0.0), theme::TEXT_3, FontKind::Sans12Medium);
        let row_h = 26.0;
        let user_n = self.user_presets.presets.len();
        // Inhalt: eingebaute Presets + (Trenner + „EIGENE" + Nutzer-Presets).
        let extra = if user_n > 0 { 24.0 + user_n as f32 * row_h } else { 0.0 };
        let content_h = PRESETS.len() as f32 * row_h + 8.0 + extra;
        let view = self.preset_scroll.begin(ui, presets_col, presets_col.w, content_h);
        let mut y = view.origin_y;
        let mut clicked_preset: Option<usize> = None;
        let mut clicked_user: Option<String> = None;
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
        if user_n > 0 {
            y += 6.0;
            let hr = Rect::new(view.viewport.x + 4.0, y, view.viewport.w - 8.0, 16.0);
            ui.text_left("EIGENE", hr, theme::TEXT_3, FontKind::Sans12Medium);
            y += 18.0;
            let names: Vec<String> = self.user_presets.presets.iter().map(|p| p.name.clone()).collect();
            for name in names {
                let row = Rect::new(view.viewport.x, y, view.viewport.w, row_h - 2.0);
                let id = ui.id(("export.userPreset", &name));
                let it = ui.interact(id, row);
                let active = self.user_preset.as_deref() == Some(name.as_str());
                if active {
                    ui.fill_rounded(row, theme::RADIUS_SM, theme::ACCENT_SOFT);
                } else if it.hovered {
                    ui.fill_rounded(row, theme::RADIUS_SM, theme::SURFACE_2);
                    ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                }
                let fg = if active { theme::TEXT_1 } else { theme::TEXT_2 };
                let label = ui.font(FontKind::Sans12).ellipsize(&name, row.w - 16.0);
                ui.text_left(&label, row.inset_xy(8.0, 0.0), fg, FontKind::Sans12);
                if it.clicked {
                    clicked_user = Some(name.clone());
                }
                y += row_h;
            }
        }
        self.preset_scroll.end(ui, presets_col, presets_col.w, content_h);
        if let Some(i) = clicked_preset {
            self.apply_preset(i, state);
        }
        if let Some(name) = clicked_user {
            self.preset_name_input.set_text(name.clone());
            self.apply_user_preset(&name, state);
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
            // Container ohne Mehrstrom-Audio kann keine Stems tragen — Flag
            // zurücksetzen, damit es nicht unsichtbar „hängen“ bleibt.
            if !c.multi_audio() {
                s.audio_stems = false;
            }
            // Codecs auf erlaubte Werte des Containers clampen.
            if c.video {
                let seq = state.timeline.settings;
                let v_fps = s.video.as_ref().map(|v| v.fps).unwrap_or_else(|| seq.rate.fps());
                let (vw, vh) = s
                    .video
                    .as_ref()
                    .map(|v| (v.width, v.height))
                    .unwrap_or((seq.width, seq.height));
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
            // Bild-Sequenzen kennen kein Audio.
            if c.audio_codecs.is_empty() {
                s.audio = None;
            } else {
                let audio_ok = s
                    .audio
                    .as_ref()
                    .is_some_and(|a| c.audio_codecs.contains(&a.codec.id));
                if !audio_ok {
                    s.audio = Some(make_audio(c.audio_codecs[0]));
                }
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
                    let seq = state.timeline.settings;
                    s.video = Some(make_video(
                        s.container.video_codecs[0],
                        seq.width,
                        seq.height,
                        seq.rate.fps(),
                    ));
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

            // ---- Encoder (Software/Hardware) ----
            // Nur anbieten, wenn die Codec-Familie mehrere Backends hat
            // (H.264/HEVC). Nicht verfügbare Hardware-Encoder werden — sofern
            // die Encoder-Liste vorliegt — ausgeblendet.
            let backends = v.codec.encoders;
            if backends.len() > 1 {
                let shown = export::available_video_encoders(v.codec.id, encoders.as_ref());
                if shown.len() > 1 {
                    let r = labeled_row(ui, body, "Encoder");
                    let labels: Vec<&str> = shown.iter().map(|e| e.label).collect();
                    let current = shown.iter().position(|e| e.id == v.encoder.id).unwrap_or(0);
                    if let Some(i) = select(ui, "export.encoder", r, &labels, current) {
                        let enc = shown[i];
                        if let Some(video) = &mut s.video {
                            video.encoder = enc;
                            // VideoToolbox kennt kein CRF → auf Bitrate zwingen.
                            if matches!(enc.quality, EncoderQuality::BitrateOnly) {
                                if let VideoQuality::Crf(_) = video.quality {
                                    video.quality = VideoQuality::Bitrate(parse_mbits(&self.bitrate_input.text).max(1));
                                }
                            }
                        }
                        changed = true;
                        layout_changed = true;
                    }
                }
            }

            // ---- Auflösung ----
            let r = labeled_row(ui, body, "Auflösung");
            let seq = state.timeline.settings;
            let source = (seq.width, seq.height);
            let source_label = format!("Wie Sequenz — {}×{}", source.0, source.1);
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
            let seq_fps = state.timeline.settings.rate.fps();
            let seq_label = format!("Wie Sequenz — {} fps", state.timeline.settings.rate.label());
            let mut fps_labels: Vec<String> = vec![seq_label];
            fps_labels.extend(FRAMERATES.iter().map(|(l, _)| format!("{l} fps")));
            fps_labels.push("Benutzerdefiniert …".into());
            let refs: Vec<&str> = fps_labels.iter().map(|s| s.as_str()).collect();
            if let Some(i) = select(ui, "export.fps", r, &refs, self.fps_choice) {
                self.fps_choice = i;
                if let Some(v) = &mut s.video {
                    if i == 0 {
                        v.fps = seq_fps;
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

            // ---- Qualität / Bild-Sequenz ----
            if s.container.image_sequence {
                // PNG/TIFF verlustfrei; JPEG mit Qualitätsregler (q:v 2..31).
                if v.codec.id == "mjpeg" {
                    let r = labeled_row(ui, body, "JPEG-Qualität");
                    let mut rr = r;
                    let value_cell = rr.cut_right(120.0);
                    rr.cut_right(8.0);
                    self.crf = self.crf.clamp(2.0, 31.0);
                    let before = self.crf;
                    slider(ui, "export.jpegq", rr, &mut self.crf, 2.0, 31.0, theme::ACCENT);
                    self.crf = self.crf.round();
                    ui.text_right(
                        &format!("q {} (2 = beste)", self.crf as i64),
                        value_cell,
                        theme::TEXT_2,
                        FontKind::Sans12,
                    );
                    if (self.crf - before).abs() > 0.01 {
                        if let Some(v) = &mut s.video {
                            v.quality = VideoQuality::Crf(self.crf as u32);
                        }
                        changed = true;
                    }
                } else {
                    let r = labeled_row(ui, body, "Qualität");
                    ui.text_left("Verlustfrei (RGB)", r, theme::TEXT_3, FontKind::Sans12);
                }
                // Startnummer der Sequenz.
                let mut r = labeled_row(ui, body, "Startnummer");
                let field = r.cut_left(90.0);
                let res = self.start_num_input.show(ui, "export.startNum", field, "1");
                r.cut_left(8.0);
                let pat = export::image_sequence_pattern(&s.output);
                let pat_name = std::path::Path::new(&pat)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or(pat);
                ui.text_left(&pat_name, r, theme::TEXT_3, FontKind::Mono12);
                if res.changed {
                    s.image_start = self.start_num_input.text.trim().parse().unwrap_or(1);
                    changed = true;
                    keeps_preset = true;
                }
            } else {
                match v.codec.quality {
                    QualityKind::CrfOrBitrate { crf } => {
                        let (min, max, _) = v.encoder.quality_range(crf);
                        let supports_cq = v.encoder.supports_constant_quality();
                        let q_label = v.encoder.quality_label();
                        let r = labeled_row(ui, body, "Qualität");
                        let mut rr = r;
                        // Modus-Auswahl nur, wenn der Encoder konstante Qualität kann.
                        if supports_cq {
                            let mode = match v.quality {
                                VideoQuality::Crf(_) => 0,
                                VideoQuality::Bitrate(_) => 1,
                            };
                            let mode_cell = rr.cut_left(190.0);
                            let cq = format!("Konstante Qualität ({q_label})");
                            if let Some(i) = select(
                                ui,
                                "export.qmode",
                                mode_cell,
                                &[cq.as_str(), "Ziel-Bitrate (VBR)"],
                                mode,
                            ) {
                                if let Some(v) = &mut s.video {
                                    v.quality = if i == 0 {
                                        VideoQuality::Crf(self.crf.round() as u32)
                                    } else {
                                        // .max(1): ein leeres/0-Feld darf kein
                                        // Bitrate(0) erzeugen (ungültig für ffmpeg).
                                        VideoQuality::Bitrate(parse_mbits(&self.bitrate_input.text).max(1))
                                    };
                                }
                                changed = true;
                                layout_changed = true;
                            }
                            rr.cut_left(12.0);
                        }
                        let show_cq = supports_cq && matches!(v.quality, VideoQuality::Crf(_));
                        if show_cq {
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
                        } else {
                            let field = rr.cut_left(72.0);
                            rr.cut_left(8.0);
                            ui.text_left("Mbit/s", rr, theme::TEXT_3, FontKind::Sans12);
                            let res = self.bitrate_input.show(ui, "export.bitrate", field, "12");
                            // Bei reinen Bitrate-Encodern sicherstellen, dass die
                            // Qualität auch wirklich auf Bitrate steht.
                            if res.changed || matches!(v.quality, VideoQuality::Crf(_)) {
                                if let Some(v) = &mut s.video {
                                    v.quality = VideoQuality::Bitrate(parse_mbits(&self.bitrate_input.text).max(1));
                                }
                                changed = true;
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

            // ---- 10-Bit-Ausgabe (höhere Farbtiefe, weniger Banding) ----
            // Nur CRF/Bitrate-Codecs mit 10-Bit-Pfad (HEVC main10, AV1, VP9,
            // H.264 High 10). ProRes/DNxHR steuern die Bittiefe über das Profil.
            if export::codec_supports_tenbit(v.codec.id) {
                let row = body.cut_top(24.0);
                if checkbox(ui, "export.tenbit", row, "10-Bit-Ausgabe (höhere Farbtiefe)", v.tenbit)
                    .clicked
                {
                    if let Some(v) = &mut s.video {
                        v.tenbit = !v.tenbit;
                    }
                    changed = true;
                }
            }
        }

        // ================= Audio =================
        // Bild-Sequenzen haben keine Audiospur (audio_codecs leer).
        if !s.container.audio_codecs.is_empty() {
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

            // ---- Stems: Audiospuren getrennt ausgeben ----
            // Nur bei Containern mit mehreren Audio-Streams (MP4/MOV/MKV/WebM/
            // M4A); WAV/MP3/FLAC tragen nur einen Stream.
            if s.container.multi_audio() {
                body.cut_top(8.0);
                let row = body.cut_top(24.0);
                let on = s.audio_stems;
                if checkbox(
                    ui,
                    "export.audioStems",
                    row,
                    "Audiospuren getrennt exportieren (Stems)",
                    on,
                )
                .clicked
                {
                    s.audio_stems = !on;
                    // Stems und Lautheits-Normalisierung schließen sich aus —
                    // loudnorm ist eine Master-Bus-Operation und würde die
                    // relativen Stem-Pegel verfälschen.
                    if s.audio_stems {
                        s.loudness = None;
                    }
                    changed = true;
                    layout_changed = true;
                }
                if on {
                    let hint = body.cut_top(16.0);
                    ui.text_left(
                        "Je Audiospur ein eigener Stream (Spurname als Titel) — Bus-FX, Automation und Master bleiben angewandt.",
                        hint,
                        theme::TEXT_3,
                        FontKind::Sans12,
                    );
                }
            }
        }

        // ================= Lautheit =================
        // Nur bei vorhandener Tonspur (Normalisierung wirkt auf den Mixdown);
        // bei aktiven Stems ausgeblendet (loudnorm ist eine Master-Bus-
        // Operation und mit getrennten Stems unvereinbar).
        if s.audio.is_some() && !export::stems_enabled(&s) {
            section(ui, body, "Lautheit");
            let row = body.cut_top(24.0);
            let on = s.loudness.is_some();
            if checkbox(ui, "export.loudOn", row, "Lautheit normalisieren", on).clicked {
                s.loudness = if on { None } else { Some(LoudnessNorm::EBU_R128) };
                changed = true;
                layout_changed = true;
            }
            body.cut_top(8.0);

            if let Some(mut norm) = s.loudness {
                // ---- Vorgabe (Preset) bzw. Benutzerdefiniert ----
                let r = labeled_row(ui, body, "Vorgabe");
                let mut labels: Vec<&str> = LOUDNESS_PRESETS.iter().map(|p| p.label).collect();
                labels.push("Benutzerdefiniert");
                let custom_idx = LOUDNESS_PRESETS.len();
                let current = loudness_preset_index(&norm).unwrap_or(custom_idx);
                if let Some(i) = select(ui, "export.loudPreset", r, &labels, current) {
                    if i < LOUDNESS_PRESETS.len() {
                        norm = LOUDNESS_PRESETS[i].norm;
                        s.loudness = Some(norm);
                        changed = true;
                    }
                }

                // ---- Frei einstellbare Zielwerte (Slider mit Wert-Anzeige) ----
                let mut field_changed = false;
                field_changed |= loudness_slider(
                    ui, body, "Ziel-Lautheit", "export.loudI",
                    &mut norm.target_i, -36.0, -9.0, 0.5, "LUFS", 1,
                );
                field_changed |= loudness_slider(
                    ui, body, "True-Peak", "export.loudTp",
                    &mut norm.true_peak, -9.0, 0.0, 0.1, "dBTP", 1,
                );
                field_changed |= loudness_slider(
                    ui, body, "Lautheitsumfang", "export.loudLra",
                    &mut norm.lra, 1.0, 30.0, 1.0, "LU", 0,
                );
                if field_changed {
                    s.loudness = Some(norm);
                    changed = true;
                }

                let hint = body.cut_top(16.0);
                ui.text_left(
                    "2-Pass-Messung über ffmpeg loudnorm (integriertes LUFS-Ziel + True-Peak-Limit).",
                    hint,
                    theme::TEXT_3,
                    FontKind::Sans12,
                );
                body.cut_top(8.0);
            }
        }

        // ================= Untertitel =================
        // Nur anbieten, wenn die Sequenz Untertitel-Spuren hat — die
        // Optionen hängen am Container (Einbetten braucht Subtitle-Streams).
        let has_subtitle_tracks = state
            .timeline
            .tracks
            .iter()
            .any(|t| t.kind == crate::core::timeline::TrackKind::Subtitle);
        if has_subtitle_tracks {
            section(ui, body, "Untertitel");
            let r = labeled_row(ui, body, "Untertitel");
            let labels: Vec<String> = export::SubtitleMode::ALL
                .iter()
                .map(|m| {
                    if *m == export::SubtitleMode::Embed && s.container.subtitle_codec.is_none() {
                        format!("{} — Container ungeeignet", m.label())
                    } else {
                        m.label().to_string()
                    }
                })
                .collect();
            let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
            let current = export::SubtitleMode::ALL
                .iter()
                .position(|m| *m == s.subtitles)
                .unwrap_or(0);
            if let Some(i) = select(ui, "export.subtitles", r, &refs, current) {
                s.subtitles = export::SubtitleMode::ALL[i];
                changed = true;
                keeps_preset = true;
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

    /// Speicher-Bereich der Preset-Spalte: Namensfeld + Speichern/Überschreiben
    /// und Löschen des gewählten Nutzer-Presets.
    fn render_preset_save(&mut self, ui: &mut Ui, area: Rect) {
        let mut a = area;
        ui.hline(a.x, a.y, a.w, theme::LINE);
        a.cut_top(8.0);
        let name_row = a.cut_top(24.0);
        let _ = self
            .preset_name_input
            .show(ui, "export.presetName", name_row, "Preset-Name …");
        a.cut_top(6.0);
        let btn_row = a.cut_top(24.0);

        let name = self.preset_name_input.text.trim().to_string();
        let exists = self.user_presets.contains(&name);
        let can_save = !name.is_empty() && self.settings.is_some();
        let save = TextButton::new(if exists { "Überschreiben" } else { "Speichern" })
            .style(TextButtonStyle::Outline);
        let sw = save.measure(ui);
        if save
            .disabled(!can_save)
            .show(ui, "export.savePreset", Rect::new(btn_row.x, btn_row.y, sw, 24.0))
            .clicked
            && can_save
        {
            // Daten vor dem mutablen Borrow auf user_presets ableiten.
            let data = self.settings.as_ref().map(PresetData::from_settings);
            if let Some(data) = data {
                self.user_presets.upsert(&name, data);
                self.user_preset = Some(name.clone());
                self.preset_idx = None;
            }
        }

        // Löschen nur, wenn ein gespeichertes Nutzer-Preset gewählt ist.
        if let Some(sel) = self.user_preset.clone() {
            if self.user_presets.contains(&sel) {
                let del = TextButton::new("Löschen").style(TextButtonStyle::Outline);
                let dw = del.measure(ui);
                if del
                    .show(ui, "export.delPreset", Rect::new(btn_row.right() - dw, btn_row.y, dw, 24.0))
                    .clicked
                {
                    self.user_presets.remove(&sel);
                    self.user_preset = None;
                }
            }
        }
    }

    fn render_footer(&mut self, ui: &mut Ui, state: &mut AppState, footer: Rect) {
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
                    "{} {}{}",
                    a.codec.label,
                    if a.channels == 1 { "Mono" } else { "Stereo" },
                    if export::stems_enabled(s) { " · Stems" } else { "" }
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

        // Buttons: „Exportieren" (in Queue + Queue-Tab) · „Hinzufügen"
        // (in Queue, bleibt für weitere Jobs) · „Schließen".
        let btn_row = f.cut_top(40.0);
        let has_error = self.issues.iter().any(|i| i.severity == Severity::Error);
        let start_rect = Rect::new(btn_row.right() - 130.0, btn_row.y + 6.0, 130.0, 28.0);
        if primary_button(ui, "export.start", start_rect, "Exportieren", !has_error).clicked
            && !has_error
            && self.enqueue(state)
        {
            self.tab = Tab::Queue;
        }
        let add = TextButton::new("Hinzufügen").icon("plus").style(TextButtonStyle::Outline);
        let aw = add.measure(ui);
        let add_rect = Rect::new(start_rect.x - 8.0 - aw, btn_row.y + 6.0, aw, 28.0);
        if !has_error && add.show(ui, "export.enqueue", add_rect).clicked {
            self.enqueue(state);
        }
        let cancel = TextButton::new("Schließen").style(TextButtonStyle::Outline);
        let cw = cancel.measure(ui);
        if cancel
            .show(
                ui,
                "export.cancelDialog",
                Rect::new(add_rect.x - 8.0 - cw, btn_row.y + 6.0, cw, 28.0),
            )
            .clicked
        {
            state.app.open_dialog = None;
        }
    }

    /// Aktuelle Einstellungen als neuen Job in die Warteschlange legen. Der
    /// Renderplan ist bereits ein entkoppelter Snapshot — der Job läuft im
    /// Hintergrund weiter, auch wenn die Timeline danach editiert wird.
    fn enqueue(&mut self, state: &mut AppState) -> bool {
        self.revalidate(state);
        if self.issues.iter().any(|i| i.severity == Severity::Error) {
            return false;
        }
        let Some(settings) = self.settings.clone() else { return false };
        let name = std::path::Path::new(&settings.output)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| settings.output.clone());
        let summary = settings_summary(&settings, &self.plan);
        state.render_queue.enqueue(
            name,
            summary,
            settings.output.clone(),
            self.plan.clone(),
            settings,
        );
        true
    }

    // ------------------------------------------------------- Warteschlange-Tab

    fn render_queue(&mut self, ui: &mut Ui, state: &mut AppState, services: &Services, area: Rect) {
        let mut area = area.inset_xy(16.0, 12.0);

        // ---- Kopf: Zusammenfassung + Steuerung ----
        let head = area.cut_top(28.0);
        let mut h = head;
        let total = state.render_queue.jobs.len();
        let active = state.render_queue.active_count();
        let summary = if total == 0 {
            "Keine Jobs — über „Einstellungen“ einen Export hinzufügen.".to_string()
        } else {
            format!("{total} Job(s) · {active} aktiv")
        };
        ui.text_left(&summary, h.cut_left(360.0), theme::TEXT_2, FontKind::Sans12);

        // „Fertige entfernen“ rechts.
        let clear = TextButton::new("Fertige entfernen").style(TextButtonStyle::Outline);
        let cw = clear.measure(ui);
        let has_finished = state.render_queue.jobs.iter().any(|j| j.state.is_finished());
        if has_finished
            && clear
                .show(ui, "queue.clearDone", Rect::new(h.right() - cw, h.y, cw, 24.0))
                .clicked
        {
            state.render_queue.clear_finished();
        }
        // Pause/Fortsetzen links daneben.
        let paused = state.render_queue.paused;
        let pause = TextButton::new(if paused { "Fortsetzen" } else { "Pausieren" })
            .icon(if paused { "play" } else { "pause" })
            .style(TextButtonStyle::Outline);
        let pw = pause.measure(ui);
        let pause_x = if has_finished { h.right() - cw - 8.0 - pw } else { h.right() - pw };
        if !state.render_queue.jobs.is_empty()
            && pause
                .show(ui, "queue.pause", Rect::new(pause_x, h.y, pw, 24.0))
                .clicked
        {
            state.render_queue.paused = !paused;
        }
        area.cut_top(8.0);

        // ---- Job-Liste (scrollbar) ----
        let row_h = 86.0;
        let count = state.render_queue.jobs.len();
        let content_h = (count as f32 * row_h).max(area.h);
        let view = self.queue_scroll.begin(ui, area, 0.0, content_h);
        let mut y = view.origin_y;
        let now = ui.time;

        // Aktionen sammeln und nach der Iteration anwenden (Borrow).
        let mut action: Option<QueueRowAction> = None;
        let ids: Vec<u64> = state.render_queue.jobs.iter().map(|j| j.id).collect();
        for id in ids {
            let row = Rect::new(view.viewport.x, y, view.viewport.w, row_h - 8.0);
            y += row_h;
            if let Some(a) = self.render_queue_row(ui, state, id, row, now) {
                action = Some(a);
            }
        }
        self.queue_scroll.end(ui, area, 0.0, content_h);

        if let Some(a) = action {
            match a {
                QueueRowAction::Cancel(id) => {
                    if let Some(svc) = state.render_queue.cancel(id, now) {
                        services.cancel_job(&svc);
                    }
                }
                QueueRowAction::Restart(id) => state.render_queue.restart(id),
                QueueRowAction::Remove(id) => state.render_queue.remove(id),
                QueueRowAction::Up(id) => state.render_queue.move_up(id),
                QueueRowAction::Down(id) => state.render_queue.move_down(id),
                QueueRowAction::Reveal(path) => {
                    let _ = services.reveal_in_file_manager(&path);
                }
            }
        }
    }

    /// Eine Job-Zeile (Name, Status, Fortschritt, Aktionen). Liefert die
    /// angeforderte Aktion (außerhalb der Borrow-Schleife angewandt).
    fn render_queue_row(
        &self,
        ui: &mut Ui,
        state: &AppState,
        id: u64,
        row: Rect,
        now: f64,
    ) -> Option<QueueRowAction> {
        let job = state.render_queue.job(id)?;
        ui.fill_rounded(row, theme::RADIUS_SM, theme::SURFACE_0);
        ui.stroke_rounded(row, theme::RADIUS_SM, 1.0, theme::LINE);
        let mut p = row.inset_xy(12.0, 8.0);

        // Kopfzeile: Statuspunkt + Name + Statuslabel rechts.
        let mut head = p.cut_top(20.0);
        let (dot, color) = match job.state {
            JobState::Waiting => ("clock", theme::TEXT_3),
            JobState::Running => ("loader-circle", theme::ACCENT),
            JobState::Done => ("circle-check", theme::SUCCESS),
            JobState::Failed => ("circle-alert", theme::DANGER),
            JobState::Cancelled => ("ban", theme::TEXT_3),
        };
        let icon_cell = head.cut_left(16.0);
        ui.icon(dot, icon_cell, 14.0, color);
        head.cut_left(6.0);
        let status_cell = head.cut_right(110.0);
        ui.text_right(job.state.label(), status_cell, color, FontKind::Sans12Medium);
        let name = ui.font(FontKind::Sans12Medium).ellipsize(&job.name, head.w);
        ui.text_left(&name, head, theme::TEXT_1, FontKind::Sans12Medium);

        // Zusammenfassung.
        let sum_row = p.cut_top(16.0);
        let sum = ui.font(FontKind::Sans12).ellipsize(&job.summary, sum_row.w);
        ui.text_left(&sum, sum_row, theme::TEXT_3, FontKind::Sans12);

        // Fortschritt / Detailzeile.
        let bar_row = p.cut_top(16.0);
        if job.state == JobState::Running {
            let mut bar = bar_row;
            let pct_cell = bar.cut_right(44.0);
            bar.cut_right(8.0);
            // Phase links neben dem Balken (Audio mischen / Video rendern / …).
            let phase_cell = bar.cut_left(150.0);
            bar.cut_left(8.0);
            ui.text_left(job.phase.label(), phase_cell, theme::TEXT_3, FontKind::Sans12);
            let track = Rect::new(bar.x, bar.y + 5.0, bar.w, 6.0);
            ui.fill_rounded(track, 3.0, theme::SURFACE_3);
            let frac = (job.progress_pct / 100.0).clamp(0.0, 1.0) as f32;
            if frac > 0.0 {
                ui.fill_rounded(Rect::new(track.x, track.y, (track.w * frac).max(6.0), 6.0), 3.0, theme::ACCENT);
            }
            ui.text_right(&format!("{:.0} %", job.progress_pct), pct_cell, theme::TEXT_1, FontKind::Sans12Medium);
        } else if job.state == JobState::Failed {
            let mut msg = job.error.clone().unwrap_or_else(|| "Unbekannter Fehler".into());
            // Hardware-Encoder-Fehler: auf den Software-Fallback hinweisen.
            if job.settings.video.as_ref().is_some_and(|v| v.encoder.is_hardware()) {
                msg = format!("{msg}  · Tipp: Software-Encoder wählen");
            }
            let msg = ui.font(FontKind::Mono12).ellipsize(&msg, bar_row.w);
            ui.text_left(&msg, bar_row, theme::DANGER, FontKind::Mono12);
        } else {
            let detail = match job.state {
                JobState::Done => {
                    let mut t = format!("Fertig in {}", format_duration(job.elapsed(now)));
                    if let Ok(meta) = std::fs::metadata(&job.output) {
                        t.push_str(&format!("  ·  {}", export::format_bytes(meta.len())));
                    }
                    t
                }
                JobState::Waiting => "Wartet auf einen freien Render-Slot …".to_string(),
                _ => String::new(),
            };
            ui.text_left(&detail, bar_row, theme::TEXT_3, FontKind::Sans12);
        }

        // Aktionszeile (rechtsbündig).
        let btn_row = p.cut_top(20.0);
        let mut bx = btn_row.right();
        let mut act: Option<QueueRowAction> = None;
        let icon_btn = |ui: &mut Ui, key: &'static str, icon: &str, tip: &str, bx: &mut f32| -> bool {
            *bx -= 26.0;
            let r = Rect::new(*bx, btn_row.y - 2.0, 24.0, 24.0);
            IconButton::new(icon)
                .tooltip(tip)
                .show(ui, ("queue", id, key), r)
                .clicked
        };
        match job.state {
            JobState::Running => {
                if icon_btn(ui, "cancel", "ban", "Abbrechen", &mut bx) {
                    act = Some(QueueRowAction::Cancel(id));
                }
            }
            JobState::Waiting => {
                if icon_btn(ui, "cancel", "x", "Aus Warteschlange entfernen", &mut bx) {
                    act = Some(QueueRowAction::Cancel(id));
                }
                if icon_btn(ui, "down", "chevron-down", "Nach unten", &mut bx) {
                    act = Some(QueueRowAction::Down(id));
                }
                if icon_btn(ui, "up", "chevron-up", "Nach oben", &mut bx) {
                    act = Some(QueueRowAction::Up(id));
                }
            }
            JobState::Done => {
                if icon_btn(ui, "remove", "trash-2", "Aus Liste entfernen", &mut bx) {
                    act = Some(QueueRowAction::Remove(id));
                }
                if icon_btn(ui, "reveal", "folder-open", "Im Dateimanager zeigen", &mut bx) {
                    act = Some(QueueRowAction::Reveal(job.output.clone()));
                }
                if icon_btn(ui, "restart", "rotate-ccw", "Erneut rendern", &mut bx) {
                    act = Some(QueueRowAction::Restart(id));
                }
            }
            JobState::Failed | JobState::Cancelled => {
                if icon_btn(ui, "remove", "trash-2", "Aus Liste entfernen", &mut bx) {
                    act = Some(QueueRowAction::Remove(id));
                }
                if icon_btn(ui, "restart", "rotate-ccw", "Erneut versuchen", &mut bx) {
                    act = Some(QueueRowAction::Restart(id));
                }
            }
        }
        act
    }
}

/// Aktion einer Warteschlangen-Zeile (außerhalb der Borrow-Schleife angewandt).
enum QueueRowAction {
    Cancel(u64),
    Restart(u64),
    Remove(u64),
    Up(u64),
    Down(u64),
    Reveal(String),
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

/// Labeled-Row mit Slider + rechtsbündiger Wertanzeige (Einheit). Rastet auf
/// `step` und meldet, ob sich der Wert geändert hat. Für die frei
/// einstellbaren Lautheits-Zielwerte (LUFS/dBTP/LU).
#[allow(clippy::too_many_arguments)]
fn loudness_slider(
    ui: &mut Ui,
    body: &mut Rect,
    label: &str,
    id: &str,
    value: &mut f64,
    min: f64,
    max: f64,
    step: f64,
    unit: &str,
    decimals: usize,
) -> bool {
    let mut rr = labeled_row(ui, body, label);
    let value_cell = rr.cut_right(76.0);
    rr.cut_right(8.0);
    let before = ((*value / step).round() * step).clamp(min, max);
    *value = before;
    slider(ui, id, rr, value, min, max, theme::ACCENT);
    *value = ((*value / step).round() * step).clamp(min, max);
    let text = format!("{:.*} {}", decimals, *value, unit).replace('.', ",");
    ui.text_right(&text, value_cell, theme::TEXT_1, FontKind::Mono12);
    (*value - before).abs() > step / 2.0
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
        encoder: &codec.encoders[0],
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
        tenbit: false,
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

/// Test-Hook `EDITRON_TEST_LOUDNESS`: `off`/leer = aus; Preset-Schlüssel
/// (`ebu`/`r128`, `-16`, `-14`, `atsc`/`-24`); `I[,TP[,LRA]]` = frei (Komma
/// oder Doppelpunkt getrennt). Liefert `None` für „aus“.
fn parse_test_loudness(spec: &str) -> Option<LoudnessNorm> {
    let s = spec.trim().to_ascii_lowercase();
    if s.is_empty() || s == "off" || s == "aus" || s == "0" {
        return None;
    }
    match s.as_str() {
        "ebu" | "r128" | "-23" => return Some(LOUDNESS_PRESETS[0].norm),
        "-16" | "podcast" => return Some(LOUDNESS_PRESETS[1].norm),
        "-14" | "streaming" => return Some(LOUDNESS_PRESETS[2].norm),
        "atsc" | "a85" | "-24" => return Some(LOUDNESS_PRESETS[3].norm),
        _ => {}
    }
    let nums: Vec<f64> = s
        .split([',', ':'])
        .filter_map(|p| p.trim().parse::<f64>().ok())
        .collect();
    let i = *nums.first()?;
    Some(
        LoudnessNorm {
            target_i: i,
            true_peak: nums.get(1).copied().unwrap_or(-1.0),
            lra: nums.get(2).copied().unwrap_or(11.0),
        }
        .clamped(),
    )
}

/// Kompakte Job-Beschreibung für die Warteschlange (Container · Codec ·
/// Auflösung · Dauer · Bereich).
fn settings_summary(s: &ExportSettings, plan: &export::RenderPlan) -> String {
    let mut parts: Vec<String> = vec![s.container.label.to_string()];
    if let Some(v) = &s.video {
        if s.container.image_sequence {
            parts.push(format!("{}×{}", v.width, v.height));
        } else {
            parts.push(format!("{} · {}×{} @ {} fps", v.encoder.label, v.width, v.height, format_fps(v.fps)));
        }
    }
    if let Some(a) = &s.audio {
        parts.push(a.codec.label.to_string());
    }
    if export::stems_enabled(s) {
        parts.push("Stems".to_string());
    }
    parts.push(format!(
        "{} ({} Frames)",
        format_duration(plan.duration),
        group_thousands(plan.total_frames)
    ));
    if s.use_in_out {
        parts.push("In/Out".to_string());
    }
    parts.join("  ·  ")
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
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(c);
    }
    out
}
