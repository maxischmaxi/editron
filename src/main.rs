mod core;
mod overlays;
mod panels;
mod platform;
mod services;
mod shell;
mod state;
mod stores;
mod theme;
mod ui;

use crate::core::commands::{build_registry, CommandCtx, CommandRegistry};
use crate::core::keyboard::KeybindingResolver;
use crate::core::{playback, project};
use crate::services::{ServiceEvent, Services};
use crate::shell::dock_host::DockHost;
use crate::state::AppState;
use panels::PanelHost;
use raylib::prelude::*;
use ui::geom::Rect;
use ui::input::InputState;
use ui::textures::TextureCache;
use ui::widgets::select::render_select_popup;
use ui::{DragPayload, Dispatch, FontKind, Ui, UiPersist};

struct App {
    state: AppState,
    registry: CommandRegistry,
    services: Services,
    resolver: KeybindingResolver,
    panel_host: PanelHost,
    dock_host: DockHost,
    textures: TextureCache,
    persist: UiPersist,
    palette: overlays::command_palette::CommandPalette,
    shortcut_editor: overlays::shortcut_editor::ShortcutEditor,
    export_dialog: overlays::export_dialog::ExportDialog,
    relink_dialog: overlays::relink_dialog::RelinkDialog,
    sequence_dialog: overlays::sequence_dialog::SequenceDialog,
    speed_dialog: overlays::speed_dialog::SpeedDialog,
    marker_dialog: overlays::marker_dialog::MarkerDialog,
    player: crate::core::player::PlayerEngine,
    titles: crate::core::title_engine::TitleEngine,
}

impl App {
    fn new() -> App {
        let state = AppState::default();
        let mut resolver = KeybindingResolver::default();
        resolver.set_bindings(state.keymap.effective_bindings());
        App {
            state,
            registry: build_registry(),
            services: Services::new(),
            resolver,
            panel_host: PanelHost::new(),
            dock_host: DockHost::default(),
            textures: TextureCache::default(),
            persist: UiPersist::default(),
            palette: Default::default(),
            shortcut_editor: Default::default(),
            export_dialog: Default::default(),
            relink_dialog: Default::default(),
            sequence_dialog: Default::default(),
            speed_dialog: Default::default(),
            marker_dialog: Default::default(),
            player: crate::core::player::PlayerEngine::new(),
            titles: Default::default(),
        }
    }

    /// Projekt öffnen (CLI, Doppelklick, Dialog, Recent) + Statusmeldung.
    fn open_project(&mut self, path: &std::path::Path, now: f64) {
        if let Some(msg) = project::safeguard_unsaved(&mut self.state) {
            self.state.app.set_status_message(Some(msg), now);
        }
        let msg = match project::open_into(&mut self.state, path) {
            Ok(0) => format!("Projekt geöffnet: {}", self.state.project.display_name()),
            Ok(n) => format!("Projekt geöffnet — {n} Medien fehlen"),
            Err(err) => format!("Öffnen fehlgeschlagen: {err}"),
        };
        self.state.app.set_status_message(Some(msg), now);
    }

    /// SRT-Datei in eine neue Untertitel-Spur importieren + Statusmeldung.
    fn import_srt(&mut self, path: &std::path::Path, now: f64) {
        let msg = match std::fs::read(path) {
            Err(e) => format!("SRT konnte nicht gelesen werden: {e}"),
            Ok(bytes) => {
                let raw = crate::core::subtitle::decode_subtitle_bytes(&bytes);
                match crate::core::subtitle::parse_srt(&raw) {
                    Err(err) => format!("SRT-Import fehlgeschlagen: {err}"),
                    Ok(cues) => {
                        let total = cues.len();
                        let (track_id, n) = self.state.timeline.import_subtitle_cues(&cues);
                        let name = self
                            .state
                            .timeline
                            .tracks
                            .iter()
                            .find(|t| t.id == track_id)
                            .map(|t| {
                                crate::core::timeline::track_name(t, &self.state.timeline.tracks)
                            })
                            .unwrap_or_default();
                        self.state.dock.open_panel("subtitles");
                        if n < total {
                            format!("{n} von {total} Untertiteln importiert (Spur {name})")
                        } else {
                            format!("{n} Untertitel importiert (Spur {name})")
                        }
                    }
                }
            }
        };
        self.state.app.set_status_message(Some(msg), now);
    }

    /// Aktive Untertitel-Spur als SRT-Datei schreiben + Statusmeldung.
    fn export_srt(&mut self, path: std::path::PathBuf, now: f64) {
        let path = match path.extension() {
            Some(ext) if ext.eq_ignore_ascii_case("srt") => path,
            _ => path.with_extension("srt"),
        };
        let msg = match self.state.timeline.active_subtitle_track() {
            None => "Keine Untertitel-Spur vorhanden".to_string(),
            Some(track) => {
                let cues = self.state.timeline.subtitle_cues(&track.id.clone());
                if cues.is_empty() {
                    "Die aktive Untertitel-Spur enthält keine Segmente".to_string()
                } else {
                    match std::fs::write(&path, crate::core::subtitle::format_srt(&cues)) {
                        Ok(()) => {
                            format!("{} Untertitel exportiert: {}", cues.len(), path.display())
                        }
                        Err(e) => format!("SRT-Export fehlgeschlagen: {e}"),
                    }
                }
            }
        };
        self.state.app.set_status_message(Some(msg), now);
    }

    fn run_command(&mut self, command: &str, args: Option<&serde_json::Value>, now: f64) {
        let mut ctx = CommandCtx {
            state: &mut self.state,
            services: &self.services,
            now,
        };
        self.registry.execute(command, args, &mut ctx);
    }

    /// Worker-Ergebnisse in den Zustand übernehmen.
    fn apply_service_events(&mut self, now: f64) {
        for event in self.services.poll() {
            match event {
                ServiceEvent::FfmpegInfo(info) => {
                    if !info.available {
                        self.state.app.set_status_message(
                            Some(
                                "FFmpeg wurde nicht gefunden — Medienfunktionen sind deaktiviert."
                                    .into(),
                            ),
                            now,
                        );
                    } else {
                        // Encoder-Liste für die Export-Validierung nachladen.
                        self.services.request_encoder_list();
                    }
                    self.state.app.ffmpeg = Some(info);
                }
                ServiceEvent::EncoderListReady(set) => {
                    self.state.app.encoders = Some(set);
                }
                ServiceEvent::AssetImported(asset) => {
                    // Duplikate (gleicher Pfad) überspringen.
                    if !self.state.media.assets.iter().any(|a| a.path == asset.path) {
                        let asset_id = asset.id.clone();
                        self.state.media.add_asset(asset);
                        // Testmodus: erstes Asset in den Quellmonitor laden
                        // (EDITRON_TEST_SOURCE=1 oder "in,out" in Sekunden) —
                        // zeigt die Insert/Overwrite-Buttons des Quellmonitors.
                        if let Ok(spec) = std::env::var("EDITRON_TEST_SOURCE") {
                            if self.state.playback.source_asset_id.is_none() {
                                self.state.playback.source_asset_id = Some(asset_id.clone());
                                self.state.playback.source = Default::default();
                                self.state.playback.source.rate = 1.0;
                                if let Some((a, b)) = spec.split_once(',') {
                                    if let (Ok(a), Ok(b)) = (a.trim().parse(), b.trim().parse()) {
                                        self.state.playback.source.in_mark = Some(a);
                                        self.state.playback.source.out_mark = Some(b);
                                        self.state.playback.source.position = a;
                                    }
                                }
                                self.state.app.focused_panel = "source".into();
                                self.state.dock.open_panel("source");
                            }
                        }
                        // Testmodus: importierte Medien ans Sequenzende einfügen.
                        if std::env::var("EDITRON_TEST_TIMELINE").is_ok() {
                            let at = crate::core::timeline::sequence_end(&self.state.timeline.clips);
                            let assets = self.state.media.assets.clone();
                            self.state
                                .timeline
                                .insert_assets(&assets, &[asset_id], at, None);
                            // Headless: Media-Match-Vorschlag automatisch
                            // übernehmen (kein modaler Prompt im Smoke-Test);
                            // EDITRON_TEST_DIALOG=match lässt ihn stehen
                            // (Screenshot des Prompts), EDITRON_TEST_SEQUENCE=
                            // "29.97df@1280x720" setzt die Sequenz explizit.
                            let keep_prompt = std::env::var("EDITRON_TEST_DIALOG")
                                .is_ok_and(|d| d == "match");
                            if !keep_prompt {
                                if let Some(m) = self.state.timeline.pending_media_match.take() {
                                    self.state.timeline.set_sequence_settings(m.settings);
                                }
                            }
                            if let Ok(spec) = std::env::var("EDITRON_TEST_SEQUENCE") {
                                if let Some(s) = crate::core::sequence::parse_test_sequence(&spec) {
                                    self.state.timeline.set_sequence_settings(s);
                                }
                            }
                            // Farbkorrektur für Smoke-Tests, z. B.
                            // EDITRON_TEST_GRADE="saturation=0,vignette=80"
                            if let Ok(spec) = std::env::var("EDITRON_TEST_GRADE") {
                                let grade = crate::core::grade::parse_test_grade(&spec);
                                for clip in &mut self.state.timeline.clips {
                                    clip.grade = grade.clone();
                                }
                            }
                            // Effekte für Smoke-Tests, z. B.
                            // EDITRON_TEST_EFFECT="gaussianBlur:strength=40;invert"
                            if let Ok(spec) = std::env::var("EDITRON_TEST_EFFECT") {
                                let fx = crate::core::effects::parse_test_effects(&spec);
                                for clip in &mut self.state.timeline.clips {
                                    clip.effects = fx
                                        .iter()
                                        .filter(|e| {
                                            e.kind.is_audio()
                                                == (clip.kind
                                                    == crate::core::timeline::TrackKind::Audio)
                                        })
                                        .cloned()
                                        .collect();
                                }
                            }
                            // Geschwindigkeit für Smoke-Tests, z. B.
                            // EDITRON_TEST_SPEED="0.5" (Faktor; negativ =
                            // rückwärts, "freeze" = Standbild) auf die
                            // eingefügten Test-Clips (mit Ripple).
                            if let Ok(spec) = std::env::var("EDITRON_TEST_SPEED") {
                                let sel = self.state.timeline.selected_clip_ids.clone();
                                if spec.trim().eq_ignore_ascii_case("freeze") {
                                    self.state
                                        .timeline
                                        .set_clip_speed(&sel, 1.0, false, true, true);
                                } else if let Ok(v) = spec.trim().replace(',', ".").parse::<f64>()
                                {
                                    self.state.timeline.set_clip_speed(
                                        &sel,
                                        v.abs(),
                                        v < 0.0,
                                        false,
                                        true,
                                    );
                                }
                            }
                            // Übergang für Smoke-Tests, z. B.
                            // EDITRON_TEST_TRANSITION="wipe" oder "crossDissolve:0.8":
                            // teilt das eingefügte Material in der Mitte, setzt den
                            // Übergang an die Kante und parkt den Playhead mittig.
                            if let Ok(spec) = std::env::var("EDITRON_TEST_TRANSITION") {
                                let (key, dur) = spec
                                    .split_once(':')
                                    .map(|(k, d)| (k, d.parse().unwrap_or(1.0)))
                                    .unwrap_or((spec.as_str(), 1.0));
                                let kind = crate::core::transitions::TransitionKind::ALL
                                    .iter()
                                    .find(|k| k.key() == key)
                                    .copied();
                                if let Some(kind) = kind {
                                    let sel = self.state.timeline.selected_clip_ids.clone();
                                    let (mut a, mut b) = (f64::INFINITY, 0.0f64);
                                    for c in self
                                        .state
                                        .timeline
                                        .clips
                                        .iter()
                                        .filter(|c| sel.contains(&c.id))
                                    {
                                        a = a.min(c.start);
                                        b = b.max(c.end());
                                    }
                                    if b > a {
                                        let mid = (a + b) / 2.0;
                                        self.state.timeline.split_at(mid, None);
                                        let enders: Vec<String> = self
                                            .state
                                            .timeline
                                            .clips
                                            .iter()
                                            .filter(|c| {
                                                (c.end() - mid).abs() < 1e-6
                                                    && kind.is_audio()
                                                        == (c.kind
                                                            == crate::core::timeline::TrackKind::Audio)
                                            })
                                            .map(|c| c.id.clone())
                                            .collect();
                                        for id in enders {
                                            if let Err(err) = self.state.timeline.add_transition(
                                                kind,
                                                &id,
                                                crate::core::timeline::TrimEdge::End,
                                                dur,
                                            ) {
                                                eprintln!("[test] Übergang: {err}");
                                            }
                                        }
                                        self.state.timeline.set_playhead(mid);
                                    }
                                }
                            }
                            // Titel für Smoke-Tests, z. B.
                            // EDITRON_TEST_TITLE="lowerThird:Maria Muster\nReporterin":
                            // legt einen Titel-Clip über die gesamte Test-
                            // Timeline und parkt den Playhead mittig.
                            if let Ok(spec) = std::env::var("EDITRON_TEST_TITLE") {
                                let (template, spec) =
                                    crate::core::title::parse_test_title(&spec);
                                let end = crate::core::timeline::sequence_end(
                                    &self.state.timeline.clips,
                                );
                                let duration = if end > 0.0 {
                                    end
                                } else {
                                    template.default_duration()
                                };
                                let id = self
                                    .state
                                    .timeline
                                    .add_title_clip(spec, 0.0, duration);
                                if template.scrolls() {
                                    if let Some(clip) = self
                                        .state
                                        .timeline
                                        .clips
                                        .iter_mut()
                                        .find(|c| c.id == id)
                                    {
                                        clip.fx.pos_y.upsert_key(0.0, 110.0);
                                        clip.fx.pos_y.upsert_key(duration, -110.0);
                                    }
                                }
                                self.state.timeline.set_playhead(duration / 2.0);
                            }
                            // Untertitel für Smoke-Tests, z. B.
                            // EDITRON_TEST_SUBTITLE="Hallo Welt\nZweite Zeile":
                            // legt eine Untertitelspur mit einem Segment über
                            // die gesamte Test-Timeline und parkt den Playhead
                            // mittig (Verifikation in Monitor/Scopes/Export).
                            if let Ok(spec) = std::env::var("EDITRON_TEST_SUBTITLE") {
                                let text = spec.replace("\\n", "\n");
                                let end = crate::core::timeline::sequence_end(
                                    &self.state.timeline.clips,
                                );
                                let dur = if end > 0.0 { end } else { 3.0 };
                                let cues = vec![crate::core::subtitle::SrtCue {
                                    start: 0.0,
                                    end: dur,
                                    text,
                                }];
                                self.state.timeline.import_subtitle_cues(&cues);
                                self.state.timeline.set_playhead(dur / 2.0);
                                self.state.dock.open_panel("subtitles");
                            }
                            // Marker für Smoke-Tests, z. B.
                            // EDITRON_TEST_MARKER="2:Intro:red;8-10:Highlight:cyan":
                            // setzt Sequenz-Marker (Zeit[:Name[:Farbe]];
                            // Zeit als a-b ⇒ Bereichsmarker), parkt den
                            // Playhead auf dem ersten und öffnet das Panel.
                            if let Ok(spec) = std::env::var("EDITRON_TEST_MARKER") {
                                let mut first: Option<f64> = None;
                                for entry in spec.split(';').filter(|s| !s.trim().is_empty()) {
                                    let mut parts = entry.splitn(3, ':');
                                    let time_tok = parts.next().unwrap_or("").trim();
                                    let name = parts.next().unwrap_or("").trim().to_string();
                                    let color = parts.next().and_then(|c| {
                                        crate::core::marker::MarkerColor::from_key(c.trim())
                                    });
                                    let (start, dur) = match time_tok.split_once('-') {
                                        Some((a, b)) => {
                                            let a = a.trim().parse::<f64>().unwrap_or(0.0);
                                            let b = b.trim().parse::<f64>().unwrap_or(a);
                                            (a, (b - a).max(0.0))
                                        }
                                        None => (time_tok.parse::<f64>().unwrap_or(0.0), 0.0),
                                    };
                                    let id = self.state.timeline.add_marker_at(start);
                                    self.state.timeline.marker_update(&id, |m| {
                                        if !name.is_empty() {
                                            m.name = name.clone();
                                        }
                                        if let Some(c) = color {
                                            m.color = c;
                                        }
                                        if dur > 0.0 {
                                            m.duration = dur;
                                        }
                                    });
                                    first.get_or_insert(start);
                                }
                                if let Some(t) = first {
                                    self.state.timeline.set_playhead(t);
                                }
                                self.state.dock.open_panel("markers");
                                // EDITRON_TEST_DIALOG=marker öffnet den
                                // Bearbeiten-Dialog auf dem ersten Marker.
                                if std::env::var("EDITRON_TEST_DIALOG")
                                    .is_ok_and(|d| d == "marker")
                                {
                                    if let Some(m) = self.state.timeline.markers.first() {
                                        self.state.app.marker_editor =
                                            Some(stores::MarkerEditTarget {
                                                scope: core::marker::MarkerScope::Sequence,
                                                marker_id: m.id.clone(),
                                            });
                                        self.state.app.open_dialog =
                                            Some(stores::DialogId::Marker);
                                    }
                                }
                            }
                            // Geschwindigkeit/Dauer-Dialog erst nach dem
                            // (asynchronen) Import öffnen — er braucht den
                            // selektierten Clip als Referenz.
                            if std::env::var("EDITRON_TEST_DIALOG").is_ok_and(|d| d == "speed") {
                                self.state.app.open_dialog = Some(stores::DialogId::ClipSpeed);
                            }
                            if std::env::var("EDITRON_TEST_PLAY").is_ok() {
                                self.state.playback.program_playing = true;
                                self.state.playback.program_rate = 1.0;
                            }
                        }
                    }
                }
                ServiceEvent::ImportFinished { errors } => {
                    self.state.media.importing = false;
                    if !errors.is_empty() {
                        self.state.app.set_status_message(
                            Some(format!(
                                "Import: {} Datei(en) fehlgeschlagen ({})",
                                errors.len(),
                                errors.join(", ")
                            )),
                            now,
                        );
                    }
                }
                ServiceEvent::ImportCancelled => {
                    self.state.media.importing = false;
                }
                ServiceEvent::WaveformReady { asset_id, peaks } => {
                    self.state.media.waveforms.insert(asset_id, Some(peaks));
                }
                ServiceEvent::WaveformFailed { asset_id } => {
                    self.state.media.waveforms.insert(asset_id, None);
                }
                ServiceEvent::SequenceExportProgress {
                    job_id,
                    pct,
                    phase,
                    frames_done,
                    frames_total,
                    render_fps,
                    eta_sec,
                } => {
                    self.export_dialog.on_progress(
                        &job_id,
                        pct,
                        phase,
                        frames_done,
                        frames_total,
                        render_fps,
                        eta_sec,
                    );
                }
                ServiceEvent::SequenceExportDone {
                    job_id,
                    ok,
                    cancelled,
                    error,
                    output,
                } => {
                    self.export_dialog
                        .on_done(&job_id, ok, cancelled, error.clone(), &output);
                    let msg = if ok {
                        format!("Export abgeschlossen: {output}")
                    } else if cancelled {
                        "Export abgebrochen".to_string()
                    } else {
                        format!("Export fehlgeschlagen: {}", error.unwrap_or_default())
                    };
                    self.state.app.set_status_message(Some(msg), now);
                }
                ServiceEvent::ExportTargetPicked(path) => {
                    self.export_dialog.on_target_picked(path);
                }
                ServiceEvent::ProjectOpenPicked(path) => {
                    if let Some(path) = path {
                        self.open_project(&path, now);
                    }
                }
                ServiceEvent::ProjectSaveTargetPicked(path) => {
                    if let Some(path) = path {
                        let path = project::ensure_extension(path);
                        let msg = match project::save_to(&mut self.state, &path) {
                            Ok(()) => format!("Projekt gespeichert: {}", path.display()),
                            Err(err) => format!("Speichern fehlgeschlagen: {err}"),
                        };
                        self.state.app.set_status_message(Some(msg), now);
                    }
                }
                ServiceEvent::RelinkFolderPicked(root) => {
                    if let Some(root) = root {
                        let targets: Vec<crate::services::RelinkTarget> = self
                            .state
                            .media
                            .assets
                            .iter()
                            .filter(|a| a.offline)
                            .map(|a| crate::services::RelinkTarget {
                                asset_id: a.id.clone(),
                                file_name: a.info.file_name.clone(),
                                size_bytes: a.info.size_bytes,
                            })
                            .collect();
                        if !targets.is_empty() {
                            self.relink_dialog.on_scan_started();
                            self.services.start_relink_scan(targets, root);
                        }
                    }
                }
                ServiceEvent::RelinkManualPicked { asset_id, path } => {
                    if let Some(path) = path {
                        self.services.resolve_relink(&asset_id, path);
                    }
                }
                ServiceEvent::RelinkScanProgress { scanned_dirs } => {
                    self.relink_dialog.on_progress(scanned_dirs);
                }
                ServiceEvent::RelinkResolved {
                    asset_id,
                    path,
                    info,
                    thumbnail_path,
                } => {
                    if self
                        .state
                        .media
                        .relink_asset(&asset_id, path, info, thumbnail_path)
                    {
                        self.relink_dialog.on_resolved(&asset_id);
                    }
                }
                ServiceEvent::RelinkFailed { asset_id, error } => {
                    self.relink_dialog.on_failed(&asset_id, error);
                }
                ServiceEvent::SubtitleImportPicked(path) => {
                    if let Some(path) = path {
                        self.import_srt(&path, now);
                    }
                }
                ServiceEvent::SubtitleExportTargetPicked(path) => {
                    if let Some(path) = path {
                        self.export_srt(path, now);
                    }
                }
                ServiceEvent::RelinkScanFinished { cancelled, unresolved } => {
                    self.relink_dialog.on_finished(cancelled, unresolved);
                    let msg = if cancelled {
                        "Mediensuche abgebrochen".to_string()
                    } else if unresolved == 0 {
                        "Alle fehlenden Medien wurden wieder verknüpft".to_string()
                    } else {
                        format!("Mediensuche abgeschlossen — {unresolved} Medien nicht gefunden")
                    };
                    self.state.app.set_status_message(Some(msg), now);
                }
            }
        }
    }

    /// Shortcuts auflösen (Pendant zu useKeyboardManager: Dialog/Palette
    /// handhaben ihre Tasten selbst; Texteingabe geht vor, außer mit Modifier).
    fn handle_keyboard(&mut self, input: &InputState, now: f64) {
        if let Some(resolved) = self.resolver.tick(now) {
            self.run_command(&resolved.command, resolved.args.as_ref(), now);
        }
        if self.state.app.open_dialog.is_some()
            || self.state.app.command_palette_open
            || self.state.context_menu.open
        {
            return;
        }
        let editing = self.persist.keyboard_focus != 0;
        let presses: Vec<crate::ui::input::KeyPress> = input.keys.clone();
        for press in presses {
            if editing && !press.ctrl && !press.meta && !press.alt {
                continue;
            }
            if let Some(resolved) =
                self.resolver
                    .handle_keypress(&press, &self.registry, &self.state, now)
            {
                self.run_command(&resolved.command, resolved.args.as_ref(), now);
            }
        }
    }
}

/// Erstes Nicht-Flag-Argument: Projektdatei (oder Medien zum Import).
fn cli_open_path() -> Option<std::path::PathBuf> {
    std::env::args_os()
        .skip(1)
        .map(std::path::PathBuf::from)
        .find(|p| !p.to_string_lossy().starts_with('-'))
}

/// Medienendungen werden importiert, alles andere als Projekt geöffnet.
fn is_media_file(path: &std::path::Path) -> bool {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    services::VIDEO_EXT.contains(&ext.as_str())
        || services::AUDIO_EXT.contains(&ext.as_str())
        || services::IMAGE_EXT.contains(&ext.as_str())
}

fn main() {
    let (mut rl, thread) = raylib::init()
        .size(1440, 900)
        .title("Editron")
        .resizable()
        .msaa_4x()
        .build();
    rl.set_target_fps(60);
    rl.set_exit_key(None);
    // macOS: „Öffnen mit“ kommt als Apple Event statt argv.
    platform::install_open_files_handler();

    let fonts = ui::text::Fonts::load(&mut rl, &thread);
    let icons = ui::icons::IconSet::load();
    let mut grade_shader = ui::grade_shader::GradeShader::load(&mut rl, &thread);
    let mut fx_renderer = ui::fx_shader::EffectChainRenderer::load(&mut rl, &thread);
    // Effekt-Jobs des letzten Frames — werden vor dem nächsten verarbeitet.
    let mut pending_fx_jobs: Vec<ui::fx_shader::EffectJob> = Vec::new();
    let mut app = App::new();
    let mut window_title = String::new();

    // Screenshot-Modus für visuelle Verifikation: EDITRON_SHOT=/pfad/zu.png
    let shot_path = std::env::var("EDITRON_SHOT").ok();
    let mut frame_count: u64 = 0;

    // CLI/Doppelklick: editron <projekt.etron> bzw. editron <medien…>
    if let Some(path) = cli_open_path() {
        if is_media_file(&path) {
            let media: Vec<std::path::PathBuf> = std::env::args_os()
                .skip(1)
                .map(std::path::PathBuf::from)
                .filter(|p| !p.to_string_lossy().starts_with('-') && is_media_file(p))
                .collect();
            app.state.media.importing = true;
            app.services.import_paths(media);
        } else {
            app.open_project(&path, 0.0);
            // Testmodus: geladenes Projekt sofort abspielen.
            if std::env::var("EDITRON_TEST_PLAY").is_ok() {
                app.state.playback.program_playing = true;
                app.state.playback.program_rate = 1.0;
            }
        }
    }

    // Testmodus: Dateien beim Start importieren (EDITRON_TEST_IMPORT=a.mp4:b.wav)
    if let Ok(paths) = std::env::var("EDITRON_TEST_IMPORT") {
        let paths: Vec<std::path::PathBuf> =
            paths.split(':').map(std::path::PathBuf::from).collect();
        app.state.media.importing = true;
        app.services.import_paths(paths);
    }
    if let Ok(ws) = std::env::var("EDITRON_TEST_WORKSPACE") {
        state::set_active_workspace(&mut app.state, &ws);
    }
    // Testmodus: Dialog beim Start öffnen
    // (EDITRON_TEST_DIALOG=export|shortcuts|relink|sequence)
    if let Ok(dialog) = std::env::var("EDITRON_TEST_DIALOG") {
        app.state.app.open_dialog = match dialog.as_str() {
            "export" => Some(stores::DialogId::Export),
            "shortcuts" => Some(stores::DialogId::Shortcuts),
            "relink" => Some(stores::DialogId::Relink),
            "sequence" => Some(stores::DialogId::SequenceSettings),
            "speed" => Some(stores::DialogId::ClipSpeed),
            _ => None,
        };
    }
    // Testmodus: Werkzeug vorwählen (EDITRON_TEST_TOOL=razor) und Maus
    // synthetisch positionieren (EDITRON_TEST_MOUSE=x,y) — für Screenshots
    // von Hover-Zuständen (z. B. Razor-Vorschau, Tooltips).
    if let Ok(tool) = std::env::var("EDITRON_TEST_TOOL") {
        if let Some(t) = stores::TOOLS.iter().find(|t| **t == tool.as_str()) {
            app.state.app.active_tool = t;
        }
    }
    let test_mouse: Option<Vector2> = std::env::var("EDITRON_TEST_MOUSE")
        .ok()
        .and_then(|s| {
            let (x, y) = s.split_once(',')?;
            Some(ui::geom::v2(x.trim().parse().ok()?, y.trim().parse().ok()?))
        });

    while !rl.window_should_close() {
        let now = rl.get_time();
        let dt = rl.get_frame_time() as f64;
        let mut input = InputState::collect(&mut rl, &mut app.persist.clock);
        if let Some(pos) = test_mouse {
            input.mouse = pos;
        }
        let screen = Rect::new(
            0.0,
            0.0,
            rl.get_screen_width() as f32,
            rl.get_screen_height() as f32,
        );

        // ---- Nicht-UI-Arbeit vor dem Zeichnen ----
        app.apply_service_events(now);
        // macOS-Doppelklick während der Laufzeit (Apple Event).
        for path in platform::take_opened_files() {
            if is_media_file(&path) {
                app.state.media.importing = true;
                app.services.import_paths(vec![path]);
            } else {
                app.open_project(&path, now);
            }
        }
        if !input.dropped_files.is_empty() {
            // Natives File-Drop: Projektdateien öffnen, SRT-Untertitel auf
            // eine neue Untertitel-Spur importieren, Medien importieren.
            let (projects, rest): (Vec<_>, Vec<_>) = input
                .dropped_files
                .clone()
                .into_iter()
                .partition(|p| {
                    p.extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case(project::PROJECT_EXT))
                });
            let (srt, media): (Vec<_>, Vec<_>) = rest
                .into_iter()
                .partition(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("srt")));
            if let Some(path) = projects.first() {
                app.open_project(path, now);
            }
            for path in &srt {
                app.import_srt(path, now);
            }
            if !media.is_empty() {
                app.state.media.importing = true;
                app.services.import_paths(media);
            }
        }
        app.handle_keyboard(&input, now);
        playback::tick(&mut app.state, dt);
        app.state.app.tick_status(now);
        // „Sequenz an Medien anpassen?“ — ausstehenden Vorschlag (erster
        // Clip-Drop in eine leere Timeline) als modalen Prompt öffnen,
        // sobald kein anderer Dialog im Weg ist.
        if app.state.timeline.pending_media_match.is_some()
            && app.state.app.open_dialog.is_none()
        {
            app.state.app.open_dialog = Some(stores::DialogId::MatchMedia);
        }
        app.player
            .tick(&mut rl, &thread, &mut app.state, &mut app.textures, now);
        // Sichtbare Titel-Clips rastern + als Texturen hochladen (CPU-
        // Rasterizer — identisch zum Export-Pfad).
        app.titles
            .tick(&mut rl, &thread, &mut app.state, &mut app.textures);
        // Effekt-Ketten auf den frischen Decode-Frames rendern (GPU,
        // zwischen den Frames wie Texture-Uploads).
        fx_renderer.process(
            &mut rl,
            &thread,
            &app.textures,
            std::mem::take(&mut pending_fx_jobs),
        );

        // ---- UI-Frame ----
        let overlay_open = app.state.context_menu.open
            || app.state.app.command_palette_open
            || app.state.app.open_dialog.is_some()
            || app.persist.select.popup.is_some();

        let mut d = rl.begin_drawing(&thread);
        d.clear_background(theme::SURFACE_0);
        let mut ui = Ui::new(
            &mut d,
            input,
            &fonts,
            &icons,
            &app.textures,
            &mut app.persist,
            now,
            dt as f32,
            screen,
        );
        ui.grade_shader = grade_shader.as_mut();
        ui.fx_outputs = Some(&fx_renderer);

        ui.begin_main_layer(overlay_open);
        let mut area = screen;
        let title_rect = area.cut_top(theme::TITLEBAR_H);
        let status_rect = area.cut_bottom(theme::STATUSBAR_H);
        shell::title_bar::render(&mut ui, &mut app.state, title_rect);
        app.dock_host.render(
            &mut ui,
            &mut app.state,
            &app.services,
            &mut app.panel_host,
            area,
        );
        shell::status_bar::render(&mut ui, &app.state, status_rect);

        // ---- Overlays ----
        ui.begin_overlay_layer();
        let mut menu = std::mem::take(&mut app.state.context_menu);
        let menu_action = overlays::context_menu::render(&mut ui, &mut menu, &app.registry, &app.state);
        app.state.context_menu = menu;
        match menu_action {
            Some(overlays::context_menu::MenuAction::Command { command, args }) => {
                ui.dispatch.push(Dispatch { command, arg: args });
            }
            Some(overlays::context_menu::MenuAction::Custom(action)) => {
                apply_custom_action(&mut app.state, action);
            }
            None => {}
        }
        app.palette.render(&mut ui, &mut app.state, &app.registry);
        app.shortcut_editor
            .render(&mut ui, &mut app.state, &app.registry);
        app.export_dialog
            .render(&mut ui, &mut app.state, &app.services);
        app.relink_dialog
            .render(&mut ui, &mut app.state, &app.services);
        app.sequence_dialog.render(&mut ui, &mut app.state);
        app.speed_dialog.render(&mut ui, &mut app.state);
        app.marker_dialog.render(&mut ui, &mut app.state);
        render_select_popup(&mut ui);

        // Drag-Ghost (z. B. Assets aus dem Medien-Browser)
        if let Some(payload) = ui.active_drag().cloned() {
            draw_drag_ghost(&mut ui, &payload);
        }

        ui.draw_tooltip_overlay();
        ui.finish_drag_frame();
        let cursor = ui.take_cursor();
        let dispatch = std::mem::take(&mut ui.dispatch);
        let texture_requests = std::mem::take(&mut ui.texture_requests);
        pending_fx_jobs = std::mem::take(&mut ui.effect_requests);
        drop(ui);
        d.set_mouse_cursor(cursor);
        drop(d);

        // ---- Nach dem Frame: Commands + Texture-Uploads ----
        for item in dispatch {
            app.run_command(&item.command, item.arg.as_ref(), now);
        }
        app.textures
            .process_requests(&mut rl, &thread, texture_requests);
        // Keymap-Änderungen aus dem Shortcut-Editor in den Resolver spiegeln.
        if app.state.app.open_dialog == Some(stores::DialogId::Shortcuts) {
            app.resolver
                .set_bindings(app.state.keymap.effective_bindings());
        }

        // ---- Projekt: Änderungen erkennen + Fenstertitel pflegen ----
        let (t_rev, m_rev) = (app.state.timeline.revision, app.state.media.revision);
        app.state.project.track_changes(t_rev, m_rev);
        let title = format!(
            "{}{} — Editron",
            app.state.project.display_name(),
            if app.state.project.dirty { " •" } else { "" }
        );
        if title != window_title {
            rl.set_window_title(&thread, &title);
            window_title = title;
        }

        frame_count += 1;
        if let Some(path) = &shot_path {
            let shot_frame = std::env::var("EDITRON_SHOT_FRAME")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30);
            if frame_count == shot_frame {
                rl.take_screenshot(&thread, path);
                break;
            }
        }
    }

    // Laufende Exporte hart beenden — sonst rendern ffmpeg-Waisen weiter.
    app.services.cancel_all_jobs();
    // Layout des aktiven Workspace sichern (pagehide-Pendant).
    let ws = app.state.app.active_workspace.clone();
    app.state.dock.save_layout_for(&ws);
    app.state.keymap.save();
    // Ungespeicherte Projektänderungen sichern: mit Pfad direkt speichern,
    // ohne Pfad als Sitzungs-Autosave (Datei-Menü → „Letzte Sitzung …“).
    // Nicht im Screenshot-/Testmodus — Testläufe würden sonst den
    // Sitzungs-Autosave des Nutzers mit Test-Timelines überschreiben.
    if shot_path.is_none() {
        if let Some(msg) = project::safeguard_unsaved(&mut app.state) {
            eprintln!("[project] {msg}");
        }
    }
}

/// Panel-lokale Menüaktionen mit gebundenen Argumenten ausführen.
fn apply_custom_action(state: &mut AppState, action: overlays::context_menu::CustomAction) {
    use overlays::context_menu::CustomAction::*;
    match action {
        TimelineSplitAt { t, clip_id } => state.timeline.split_at(t, Some(&[clip_id])),
        TimelinePasteAt { t } => state.timeline.paste(Some(t.max(0.0))),
        TimelineRemoveTrack { track_id } => state.timeline.remove_track(&track_id),
        TimelineToggleTrackFlag { track_id, flag } => {
            state.timeline.toggle_track_flag(&track_id, flag)
        }
        TimelineToggleSourcePatch { track_id } => {
            state.timeline.toggle_source_patch(&track_id)
        }
        TimelineSetInAt { t } => state.timeline.set_in_point(Some(t)),
        TimelineSetOutAt { t } => state.timeline.set_out_point(Some(t)),
        TimelineClearInOut => state.timeline.clear_in_out(),
        MediaShowInBrowser { asset_id } => {
            state.media.select(vec![asset_id]);
            state.dock.open_panel("media");
        }
        FxSetInterp { keys, interp } => {
            for_each_clip_keys(keys, |clip_id, keys| {
                state.timeline.kf_set_interp(clip_id, keys, interp)
            });
        }
        FxRemoveKeyframes { keys } => {
            for_each_clip_keys(keys, |clip_id, keys| {
                state.timeline.kf_remove_keyframes(clip_id, keys)
            });
        }
        FxResetParam { clip_id, pref } => state.timeline.kf_reset_param(&clip_id, &pref),
        EffectsApplyToClips { kind, clip_ids } => {
            // Doppelte Ziele vermeiden (A/V-Paare zeigen auf denselben Clip).
            let mut applied: Vec<String> = Vec::new();
            for id in clip_ids {
                if let Some(target) = state.timeline.effect_target_clip(&id, kind) {
                    if !applied.contains(&target) {
                        state.timeline.effects_add(&id, kind);
                        applied.push(target);
                    }
                }
            }
        }
        EffectsMove { clip_id, fx_id, delta } => {
            state.timeline.effects_move(&clip_id, &fx_id, delta)
        }
        EffectsRemove { clip_id, fx_id } => state.timeline.effects_remove(&clip_id, &fx_id),
        EffectsToggle { clip_id, fx_id } => {
            state.timeline.effects_toggle_enabled(&clip_id, &fx_id)
        }
        EffectsReset { clip_id, fx_id } => state.timeline.effects_reset(&clip_id, &fx_id),
        TrackEffectsAdd { track_id, kind } => {
            state.timeline.track_effects_add(&track_id, kind);
        }
        TrackEffectsRemove { track_id, fx_id } => {
            state.timeline.track_effects_remove(&track_id, &fx_id)
        }
        TrackEffectsToggle { track_id, fx_id } => {
            state.timeline.track_effects_toggle_enabled(&track_id, &fx_id)
        }
        TrackEffectsReset { track_id, fx_id } => {
            state.timeline.track_effects_reset(&track_id, &fx_id)
        }
        TrackEffectsMove { track_id, fx_id, delta } => {
            state.timeline.track_effects_move(&track_id, &fx_id, delta)
        }
        TrackAutoClear { track_id, param } => state.timeline.track_auto_clear(&track_id, param),
        TransitionRemove { id } => state.timeline.remove_transitions(&[id]),
        TransitionReplace { id, kind } => state.timeline.set_transition_kind(&id, kind),
        TransitionAlign { id, alignment } => {
            state.timeline.set_transition_alignment(&id, alignment)
        }
        TransitionDirection { id, direction } => {
            state.timeline.set_transition_direction(&id, direction)
        }
        TransitionEditDuration { id } => state.app.edit_transition_duration = Some(id),
        MarkerEdit { scope, marker_id } => {
            state.app.marker_editor = Some(stores::MarkerEditTarget { scope, marker_id });
            state.app.open_dialog = Some(stores::DialogId::Marker);
        }
        MarkerDelete { scope, marker_id } => match scope {
            core::marker::MarkerScope::Sequence => state.timeline.remove_marker(&marker_id),
            core::marker::MarkerScope::Clip(cid) => {
                state.timeline.remove_clip_marker(&cid, &marker_id)
            }
            core::marker::MarkerScope::Asset(aid) => {
                state.media.remove_asset_marker(&aid, &marker_id)
            }
        },
        MarkerSetColor { scope, marker_id, color } => match scope {
            core::marker::MarkerScope::Sequence => {
                state.timeline.marker_update(&marker_id, |m| m.color = color)
            }
            core::marker::MarkerScope::Clip(cid) => {
                state.timeline.clip_marker_update(&cid, &marker_id, |m| m.color = color)
            }
            core::marker::MarkerScope::Asset(aid) => {
                state.media.asset_marker_update(&aid, &marker_id, |m| m.color = color)
            }
        },
        MarkerAddAt { t } => {
            state.timeline.add_marker_at(t);
        }
        MarkerAddClipAt { clip_id, t } => {
            // Hinzufügen + sofort den Bearbeiten-Dialog öffnen (Premiere).
            if let Some(mid) = state.timeline.add_clip_marker_at_seq(&clip_id, t) {
                state.app.marker_editor = Some(stores::MarkerEditTarget {
                    scope: core::marker::MarkerScope::Clip(clip_id),
                    marker_id: mid,
                });
                state.app.open_dialog = Some(stores::DialogId::Marker);
            }
        }
    }
}

/// Keyframe-Listen aus Menüaktionen nach Clip gruppieren.
fn for_each_clip_keys(
    keys: Vec<(String, crate::core::animation::ParamRef, f64)>,
    mut f: impl FnMut(&str, &[(crate::core::animation::ParamRef, f64)]),
) {
    let mut by_clip: std::collections::HashMap<String, Vec<(crate::core::animation::ParamRef, f64)>> =
        Default::default();
    for (clip_id, param, t) in keys {
        by_clip.entry(clip_id).or_default().push((param, t));
    }
    for (clip_id, keys) in by_clip {
        f(&clip_id, &keys);
    }
}

/// Ghost am Mauszeiger während eines In-App-Drags.
fn draw_drag_ghost(ui: &mut Ui, payload: &DragPayload) {
    let label = match payload {
        DragPayload::Assets(ids) => {
            if ids.len() == 1 {
                "1 Medium".to_string()
            } else {
                format!("{} Medien", ids.len())
            }
        }
        DragPayload::Tab { panel } => crate::panels::panel_title(panel).to_string(),
        DragPayload::Effect(kind) => kind.label().to_string(),
        DragPayload::Transition(kind) => kind.label().to_string(),
    };
    let w = ui.font(FontKind::Sans12).width(&label) + 24.0;
    let rect = Rect::new(ui.input.mouse.x + 12.0, ui.input.mouse.y + 14.0, w, 24.0);
    ui.fill_rounded(rect, theme::RADIUS_SM, theme::with_alpha(theme::SURFACE_3, 230));
    ui.stroke_rounded(rect, theme::RADIUS_SM, 1.0, theme::ACCENT);
    let inner = rect.inset_xy(12.0, 0.0);
    ui.text_left(&label, inner, theme::TEXT_1, FontKind::Sans12);
}
