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
    player: crate::core::player::PlayerEngine,
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
            player: crate::core::player::PlayerEngine::new(),
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
                        // Testmodus: importierte Medien ans Sequenzende einfügen.
                        if std::env::var("EDITRON_TEST_TIMELINE").is_ok() {
                            let at = crate::core::timeline::sequence_end(&self.state.timeline.clips);
                            let assets = self.state.media.assets.clone();
                            self.state
                                .timeline
                                .insert_assets(&assets, &[asset_id], at, None);
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
    // Testmodus: Dialog beim Start öffnen (EDITRON_TEST_DIALOG=export|shortcuts|relink)
    if let Ok(dialog) = std::env::var("EDITRON_TEST_DIALOG") {
        app.state.app.open_dialog = match dialog.as_str() {
            "export" => Some(stores::DialogId::Export),
            "shortcuts" => Some(stores::DialogId::Shortcuts),
            "relink" => Some(stores::DialogId::Relink),
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
            // Natives File-Drop: Projektdateien öffnen, Medien importieren.
            let (projects, media): (Vec<_>, Vec<_>) = input
                .dropped_files
                .clone()
                .into_iter()
                .partition(|p| {
                    p.extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case(project::PROJECT_EXT))
                });
            if let Some(path) = projects.first() {
                app.open_project(path, now);
            }
            if !media.is_empty() {
                app.state.media.importing = true;
                app.services.import_paths(media);
            }
        }
        app.handle_keyboard(&input, now);
        playback::tick(&mut app.state, dt);
        app.state.app.tick_status(now);
        app.player
            .tick(&mut rl, &thread, &mut app.state, &mut app.textures, now);

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
        TimelineSetInAt { t } => state.timeline.set_in_point(Some(t)),
        TimelineSetOutAt { t } => state.timeline.set_out_point(Some(t)),
        TimelineClearInOut => state.timeline.clear_in_out(),
        MediaShowInBrowser { asset_id } => {
            state.media.select(vec![asset_id]);
            state.dock.open_panel("media");
        }
        FxSetInterp { keys, interp } => {
            for_each_clip_keys(keys, |clip_id, keys| {
                state.timeline.fx_set_interp(clip_id, keys, interp)
            });
        }
        FxRemoveKeyframes { keys } => {
            for_each_clip_keys(keys, |clip_id, keys| {
                state.timeline.fx_remove_keyframes(clip_id, keys)
            });
        }
    }
}

/// Keyframe-Listen aus Menüaktionen nach Clip gruppieren.
fn for_each_clip_keys(
    keys: Vec<(String, crate::core::animation::ParamId, f64)>,
    mut f: impl FnMut(&str, &[(crate::core::animation::ParamId, f64)]),
) {
    let mut by_clip: std::collections::HashMap<String, Vec<(crate::core::animation::ParamId, f64)>> =
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
    };
    let w = ui.font(FontKind::Sans12).width(&label) + 24.0;
    let rect = Rect::new(ui.input.mouse.x + 12.0, ui.input.mouse.y + 14.0, w, 24.0);
    ui.fill_rounded(rect, theme::RADIUS_SM, theme::with_alpha(theme::SURFACE_3, 230));
    ui.stroke_rounded(rect, theme::RADIUS_SM, 1.0, theme::ACCENT);
    let inner = rect.inset_xy(12.0, 0.0);
    ui.text_left(&label, inner, theme::TEXT_1, FontKind::Sans12);
}
