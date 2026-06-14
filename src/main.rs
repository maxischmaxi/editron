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
    settings_dialog: overlays::settings_dialog::SettingsDialog,
    autosave_dialog: overlays::autosave_dialog::AutosaveDialog,
    speed_dialog: overlays::speed_dialog::SpeedDialog,
    marker_dialog: overlays::marker_dialog::MarkerDialog,
    media_dialogs: overlays::media_dialogs::MediaDialogs,
    interop_report: overlays::interop_report::InteropReportDialog,
    player: crate::core::player::PlayerEngine,
    titles: crate::core::title_engine::TitleEngine,
    /// Letzte Proxy-Revalidierung (Sekunden) — gedrosselte Prüfung, ob ein
    /// Proxy zwischenzeitlich gelöscht/veraltet ist (automatischer Fallback).
    last_proxy_check: f64,
    /// Zeitpunkt (Sekunden) des letzten zeitgesteuerten Autosaves bzw. der
    /// Baseline, solange das Projekt sauber ist (siehe [`App::maybe_autosave`]).
    last_autosave: f64,
}

impl App {
    fn new() -> App {
        let mut state = AppState::default();
        // ffmpeg-/ffprobe-Pfad-Override aus den Einstellungen aktiv schalten,
        // BEVOR die Binary-Discovery beim Services-Start läuft.
        crate::services::set_ffmpeg_override(
            state.settings.ffmpeg_path.clone(),
            state.settings.ffprobe_path.clone(),
        );
        // Wiedergabe-Auflösungs-Default auf die Monitore anwenden.
        state.monitor.program_scale = state.settings.default_preview_scale;
        state.monitor.source_scale = state.settings.default_preview_scale;
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
            settings_dialog: Default::default(),
            autosave_dialog: Default::default(),
            speed_dialog: Default::default(),
            marker_dialog: Default::default(),
            media_dialogs: Default::default(),
            interop_report: Default::default(),
            player: crate::core::player::PlayerEngine::new(),
            titles: Default::default(),
            last_proxy_check: 0.0,
            last_autosave: 0.0,
        }
    }

    /// Zeitgesteuertes Autosave: alle N Minuten bei „dirty" Projekt eine
    /// Versionskopie schreiben (atomar, neben der Projektdatei). Läuft im
    /// Mainloop — kein eigener Thread. Solange nichts zu sichern ist, wandert
    /// die Baseline mit, damit das Intervall ab dem Moment des ersten
    /// ungespeicherten Edits zählt.
    fn maybe_autosave(&mut self, now: f64) {
        let cfg = self.state.settings.autosave.clamped();
        if !cfg.enabled || !self.state.project.dirty {
            self.last_autosave = now;
            return;
        }
        if now - self.last_autosave < cfg.interval_secs() {
            return;
        }
        self.last_autosave = now;
        match crate::core::autosave::write_timed_autosave(
            &self.state,
            cfg.max_versions as usize,
            std::time::SystemTime::now(),
        ) {
            Ok(path) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.state
                    .app
                    .set_status_message(Some(format!("Autosave-Version gesichert: {name}")), now);
            }
            Err(e) => {
                self.state
                    .app
                    .set_status_message(Some(format!("Autosave fehlgeschlagen: {e}")), now);
            }
        }
    }

    /// Eine Autosave-Version als UNGESPEICHERTE Kopie öffnen (Original bleibt
    /// unberührt; Pfad wird gelöscht ⇒ „Unbenannt", dirty zum Schutz).
    fn open_autosave_version(&mut self, path: &std::path::Path, now: f64) {
        if let Some(msg) = project::safeguard_unsaved(&mut self.state) {
            self.state.app.set_status_message(Some(msg), now);
        }
        match project::open_into(&mut self.state, path) {
            Ok(_) => {
                self.state.project.path = None;
                let entry = path.to_string_lossy().into_owned();
                self.state.project.remove_recent(&entry);
                // Als ungespeicherte Kopie führen: dirty, damit der Inhalt
                // beim Beenden nicht verloren geht (Sitzungs-Autosave greift).
                self.state.project.dirty = true;
                self.last_autosave = now;
                self.state.app.set_status_message(
                    Some("Autosave-Version geöffnet (ungespeicherte Kopie)".into()),
                    now,
                );
            }
            Err(err) => {
                self.state
                    .app
                    .set_status_message(Some(format!("Öffnen fehlgeschlagen: {err}")), now);
            }
        }
    }

    /// Gedrosselt prüfen, ob noch alle Proxys vorhanden sind, solange der
    /// Proxy-Modus aktiv ist. Verschwindet ein Proxy (Datei gelöscht/veraltet),
    /// fällt die Vorschau automatisch aufs Original zurück + Hinweis.
    fn maybe_revalidate_proxies(&mut self, now: f64) {
        if !self.state.media.use_proxies || now - self.last_proxy_check < 2.0 {
            return;
        }
        self.last_proxy_check = now;
        if self.state.media.revalidate_proxies() {
            let missing = self
                .state
                .media
                .assets
                .iter()
                .filter(|a| a.proxy_path.is_some() && a.proxy_offline)
                .count();
            if missing > 0 {
                self.state.app.set_status_message(
                    Some(format!(
                        "{missing} Proxy-Datei(en) fehlen — Vorschau nutzt das Original."
                    )),
                    now,
                );
            }
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

    /// Eine Austauschformat-Datei (OTIO/EDL) als neue Sequenz importieren:
    /// Spuren/Clips bauen, fehlende Medien als Offline-Assets anlegen, online
    /// auffindbare nachträglich per ffprobe verifizieren, Relink anbieten.
    fn import_interop(
        &mut self,
        format: crate::core::interop::InteropFormat,
        path: &std::path::Path,
        now: f64,
    ) {
        use crate::core::interop;
        let ext = format.extension().to_uppercase();
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                self.state
                    .app
                    .set_status_message(Some(format!("{ext} konnte nicht gelesen werden: {e}")), now);
                return;
            }
        };
        let build = match interop::import_text(format, &text, &self.state.media.assets) {
            Ok(b) => b,
            Err(e) => {
                self.state
                    .app
                    .set_status_message(Some(format!("{ext}-Import fehlgeschlagen: {e}")), now);
                return;
            }
        };

        let offline = build.summary.media_offline;
        let clip_count = build.summary.clips;

        // Neue Assets übernehmen (sie tragen bereits ihr Bin = Wurzel).
        for a in build.new_assets {
            if !self.state.media.assets.iter().any(|x| x.id == a.id) {
                self.state.media.assets.push(a);
            }
        }
        self.state.media.revision += 1;

        // Sequenz aufbauen + aktivieren.
        let mut tl = crate::core::timeline::TimelineStore::default();
        tl.load_document(
            Some(build.settings),
            build.tracks,
            build.clips,
            build.transitions,
            build.markers,
            0.0,
            None,
            None,
            40.0,
            true,
            Vec::new(),
            0.0,
            None,
        );
        let seq = crate::core::sequences::Sequence::new(
            build.sequence_name.clone(),
            crate::core::bin::ROOT_BIN_ID,
            tl,
        );
        self.state.timeline.add_sequence(seq);
        self.state.dock.open_panel("timeline");

        // Online auffindbare Medien per ffprobe verifizieren (füllt Metadaten,
        // korrigiert Spurarten); nutzt den vorhandenen Relink-Pfad.
        for (asset_id, p) in &build.probe {
            self.services.resolve_relink(asset_id, std::path::PathBuf::from(p));
        }

        // Ergebnis-Bericht öffnen (Kennzahlen + Auslassungen, ggf. Relink).
        self.state.app.interop_report = Some(interop::InteropReport::from_import(&build.summary));
        self.state.app.open_dialog = Some(crate::stores::DialogId::InteropReport);
        let msg = if offline > 0 {
            format!("{ext} importiert — {clip_count} Clips, {offline} Medien fehlen")
        } else {
            format!("{ext} importiert — {clip_count} Clips")
        };
        self.state.app.set_status_message(Some(msg), now);
    }

    /// Die aktive Sequenz in ein Austauschformat schreiben. Auslassungen werden
    /// nie still verschluckt: bei Warnungen öffnet der Ergebnis-Bericht.
    fn export_interop(
        &mut self,
        format: crate::core::interop::InteropFormat,
        path: std::path::PathBuf,
        now: f64,
    ) {
        use crate::core::interop;
        let path = match path.extension() {
            Some(ext) if ext.eq_ignore_ascii_case(format.extension()) => path,
            _ => path.with_extension(format.extension()),
        };
        let ext = format.extension().to_uppercase();
        let name = self.state.timeline.active_name().to_string();
        let (text, warnings) =
            interop::export_text(format, &self.state.timeline, &self.state.media.assets, &name);
        match std::fs::write(&path, text) {
            Ok(()) => {
                let msg = if warnings.is_empty() {
                    format!("{ext} exportiert: {}", path.display())
                } else {
                    format!(
                        "{ext} exportiert — {} Hinweis(e): {}",
                        warnings.len(),
                        path.display()
                    )
                };
                self.state.app.set_status_message(Some(msg), now);
                if !warnings.is_empty() {
                    let report = interop::InteropReport::from_export(
                        format,
                        &path.to_string_lossy(),
                        warnings,
                    );
                    self.state.app.interop_report = Some(report);
                    self.state.app.open_dialog = Some(crate::stores::DialogId::InteropReport);
                }
            }
            Err(e) => {
                self.state
                    .app
                    .set_status_message(Some(format!("{ext}-Export fehlgeschlagen: {e}")), now);
            }
        }
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
                ServiceEvent::FfmpegBinaryPicked { which, path } => {
                    if let Some(path) = path {
                        let p = path.to_string_lossy().into_owned();
                        if which == "ffmpeg" {
                            self.state.settings.ffmpeg_path = Some(p);
                        } else {
                            self.state.settings.ffprobe_path = Some(p);
                        }
                        self.state.settings.save();
                        crate::services::set_ffmpeg_override(
                            self.state.settings.ffmpeg_path.clone(),
                            self.state.settings.ffprobe_path.clone(),
                        );
                        // Verfügbarkeit/Version mit dem neuen Pfad neu erfragen.
                        self.services.refresh_ffmpeg_info();
                        self.state
                            .app
                            .set_status_message(Some(format!("{which}-Pfad gesetzt")), now);
                    }
                }
                ServiceEvent::ProxyProgress { asset_id, pct } => {
                    self.state.media.set_proxy_building(&asset_id, pct as f32);
                }
                ServiceEvent::ProxyDone {
                    asset_id,
                    proxy_path,
                    src_mtime,
                } => {
                    self.state
                        .media
                        .apply_proxy_result(&asset_id, proxy_path, src_mtime);
                    // Statusmeldung erst, wenn keine Proxys mehr in Arbeit sind.
                    let remaining = self.state.media.proxy_jobs.len();
                    let msg = if remaining == 0 {
                        "Proxies erstellt".to_string()
                    } else {
                        format!("Proxy erstellt — {remaining} verbleiben")
                    };
                    self.state.app.set_status_message(Some(msg), now);
                }
                ServiceEvent::ProxyFolderPicked(path) => {
                    if let Some(path) = path {
                        self.state.media.proxy_settings.folder =
                            Some(path.to_string_lossy().into_owned());
                        self.state.media.revision += 1;
                        self.state.app.set_status_message(
                            Some(format!("Proxy-Ordner: {}", path.display())),
                            now,
                        );
                    }
                }
                ServiceEvent::ProxyFailed { asset_id, error } => {
                    let name = self
                        .state
                        .media
                        .asset(&asset_id)
                        .map(|a| a.name.clone())
                        .unwrap_or_else(|| asset_id.clone());
                    self.state.media.set_proxy_failed(&asset_id, error.clone());
                    self.state.app.set_status_message(
                        Some(format!("Proxy fehlgeschlagen ({name}): {error}")),
                        now,
                    );
                }
                ServiceEvent::AssetImported(asset) => {
                    // Duplikate (gleicher Pfad) überspringen.
                    if !self.state.media.assets.iter().any(|a| a.path == asset.path) {
                        let asset_id = asset.id.clone();
                        self.state.media.add_asset(asset);
                        // Testmodus: Demo-Etiketten round-robin (EDITRON_TEST_BINS).
                        if std::env::var("EDITRON_TEST_BINS").is_ok() {
                            let n = self.state.media.assets.len();
                            let label = crate::core::bin::MediaLabel::ALL[n % 8];
                            if n % 2 == 0 {
                                self.state.media.set_label(&[asset_id.clone()], Some(label));
                            }
                            self.state.media.clear_history();
                        }
                        // Testmodus: Proxy-Zustand simulieren (EDITRON_TEST_PROXY=
                        // ready|building|failed) — schaltet den Proxy-Modus an
                        // und zeigt Badges/Toggle im Smoke-Test.
                        if let Ok(mode) = std::env::var("EDITRON_TEST_PROXY") {
                            self.state.media.use_proxies = true;
                            match mode.as_str() {
                                "building" => {
                                    self.state.media.set_proxy_building(&asset_id, 0.42)
                                }
                                "failed" => self
                                    .state
                                    .media
                                    .set_proxy_failed(&asset_id, "Test-Fehler".into()),
                                // „ready“ (Standard): Quelle als Pseudo-Proxy
                                // eintragen (existiert ⇒ gültiger Proxy-Badge).
                                _ => {
                                    if let Some(a) = self
                                        .state
                                        .media
                                        .assets
                                        .iter()
                                        .find(|a| a.id == asset_id)
                                        .map(|a| a.path.clone())
                                    {
                                        self.state.media.apply_proxy_result(
                                            &asset_id,
                                            a,
                                            Some(0.0),
                                        );
                                    }
                                }
                            }
                        }
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
                                self.state.playback.program_rate = test_play_rate();
                            }
                            // Performance-Overlay für die visuelle Verifikation.
                            if std::env::var("EDITRON_TEST_PERF").is_ok() {
                                self.state.monitor.show_perf_overlay = true;
                            }
                            // Interop-Export End-to-End durch den echten App-Pfad:
                            // EDITRON_TEST_INTEROP="otio:/tmp/out.otio" (Schlüssel
                            // otio|edl|fcpxml). Schreibt die Test-Sequenz in das
                            // Austauschformat (Frame-Genauigkeit gegen Resolve prüfen).
                            if let Ok(spec) = std::env::var("EDITRON_TEST_INTEROP") {
                                if let Some((fmt, out)) = spec.split_once(':') {
                                    if let Some(format) =
                                        crate::core::interop::InteropFormat::from_key(fmt.trim())
                                    {
                                        self.export_interop(
                                            format,
                                            std::path::PathBuf::from(out.trim()),
                                            now,
                                        );
                                    }
                                }
                            }
                            // Multicam-Smoke-Test: EDITRON_TEST_MULTICAM=N (Winkel-
                            // zahl 2–9, Default 4). Baut aus den importierten Video-
                            // Assets eine Multicam-Quelle (gemeinsamer Start; ein
                            // Asset wird ggf. zu mehreren Winkeln dupliziert), legt
                            // einen Multicam-Clip an und schaltet in den Multicam-
                            // Monitor (Raster-Verifikation; Zifferntasten schneiden).
                            if let Ok(spec) = std::env::var("EDITRON_TEST_MULTICAM") {
                                let already =
                                    self.state.timeline.clips.iter().any(|c| c.is_multicam());
                                if !already {
                                    let want: usize =
                                        spec.trim().parse().unwrap_or(4).clamp(2, 9);
                                    let vids: Vec<crate::core::types::MediaAsset> = self
                                        .state
                                        .media
                                        .assets
                                        .iter()
                                        .filter(|a| {
                                            a.kind == crate::core::types::MediaKind::Video
                                                && !a.info.video.is_empty()
                                        })
                                        .cloned()
                                        .collect();
                                    if let Some(first) = vids.first().cloned() {
                                        let mut chosen = vids.clone();
                                        while chosen.len() < want {
                                            chosen.push(first.clone());
                                        }
                                        chosen.truncate(want);
                                        let refs: Vec<&crate::core::types::MediaAsset> =
                                            chosen.iter().collect();
                                        let pos = crate::core::multicam::positions_from_start(
                                            refs.len(),
                                        );
                                        let source = crate::core::multicam::build_source(
                                            &refs,
                                            &pos,
                                            None,
                                            crate::core::multicam::MulticamSync::Start,
                                        );
                                        let inner =
                                            crate::core::multicam::build_inner_timeline(&source);
                                        let mut seq = crate::core::sequences::Sequence::new(
                                            "Multicam – Test",
                                            crate::core::bin::ROOT_BIN_ID,
                                            inner,
                                        );
                                        seq.timeline.multicam = Some(source);
                                        let src_id = self.state.timeline.add_background(seq);
                                        let (dur, has_audio) = self
                                            .state
                                            .timeline
                                            .multicam_source(&src_id)
                                            .map(|s| {
                                                (
                                                    s.duration,
                                                    s.angles.iter().any(|a| a.has_audio),
                                                )
                                            })
                                            .unwrap_or((5.0, false));
                                        self.state.timeline.clips.clear();
                                        self.state.timeline.insert_multicam_clip(
                                            &src_id,
                                            "Multicam – Test",
                                            dur,
                                            has_audio,
                                            0.0,
                                            None,
                                        );
                                        self.state.monitor.view =
                                            stores::MonitorView::Multicam;
                                        self.state.dock.open_panel("program");
                                        self.state
                                            .timeline
                                            .set_playhead((dur / 2.0).max(0.0));
                                    }
                                }
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
                ServiceEvent::ScrubThumbReady { asset_id, bucket, path } => {
                    if !path.is_empty() {
                        self.state
                            .media
                            .scrub_thumbs
                            .entry(asset_id)
                            .or_default()
                            .insert(bucket, path);
                    }
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
                    self.state.render_queue.on_progress(
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
                    self.state
                        .render_queue
                        .on_done(&job_id, ok, cancelled, error.clone(), now);
                    let name = std::path::Path::new(&output)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| output.clone());
                    let msg = if ok {
                        format!("Export abgeschlossen: {name}")
                    } else if cancelled {
                        format!("Export abgebrochen: {name}")
                    } else {
                        format!("Export fehlgeschlagen ({name}): {}", error.unwrap_or_default())
                    };
                    self.state.app.set_status_message(Some(msg), now);
                }
                ServiceEvent::RenderCacheProgress {
                    job_id,
                    start_frame,
                    end_frame,
                    pct,
                } => {
                    self.state.render_cache.rendering =
                        Some(crate::core::render_cache::RenderProgress {
                            start_frame,
                            end_frame,
                            pct,
                            job_id: job_id
                                .rsplit('-')
                                .next()
                                .and_then(|n| n.parse().ok())
                                .unwrap_or(0),
                        });
                }
                ServiceEvent::RenderCacheDone {
                    job_id: _,
                    start_frame,
                    end_frame,
                    file,
                    content_hash,
                    ok,
                    error,
                } => {
                    self.state.render_cache.rendering = None;
                    if ok {
                        // Segment registrieren; ersetzte (überlappende) Dateien
                        // löschen — aber NICHT die gerade geschriebene (gleicher
                        // Bereich + Hash ⇒ gleicher Dateiname beim Re-Render).
                        let new_file = file.clone();
                        let removed =
                            self.state
                                .render_cache
                                .add_segment(crate::core::render_cache::CacheSegment {
                                    start_frame,
                                    end_frame,
                                    file,
                                    content_hash,
                                    codec: self.state.settings.render_cache_codec,
                                });
                        for f in removed {
                            if f != new_file {
                                let _ = std::fs::remove_file(f);
                            }
                        }
                        self.state.app.set_status_message(
                            Some("Render-Cache aktualisiert".into()),
                            now,
                        );
                    } else {
                        self.state.app.set_status_message(
                            Some(format!(
                                "Render-Cache fehlgeschlagen: {}",
                                error.unwrap_or_default()
                            )),
                            now,
                        );
                    }
                }
                ServiceEvent::ExportTargetPicked(path) => {
                    self.export_dialog.on_target_picked(path);
                }
                ServiceEvent::FrameExportTargetPicked(path) => {
                    if let Some(path) = path {
                        self.export_program_frame(path, now);
                    }
                }
                ServiceEvent::FrameExportDone { path, ok, error } => {
                    let name = std::path::Path::new(&path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or(path);
                    let msg = if ok {
                        format!("Frame exportiert: {name}")
                    } else {
                        format!("Frame-Export fehlgeschlagen: {}", error.unwrap_or_default())
                    };
                    self.state.app.set_status_message(Some(msg), now);
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
                ServiceEvent::InteropImportPicked { format, path } => {
                    if let Some(path) = path {
                        self.import_interop(format, &path, now);
                    }
                }
                ServiceEvent::InteropExportTargetPicked { format, path } => {
                    if let Some(path) = path {
                        self.export_interop(format, path, now);
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
        self.pump_render_queue(now);
    }

    /// Sequentielle Abarbeitung der Render-Warteschlange: startet den nächsten
    /// wartenden Job, sobald keiner mehr läuft. Plan + Einstellungen werden in
    /// den Worker geklont — der Snapshot in der Queue bleibt für einen
    /// möglichen Neustart erhalten und entkoppelt den Job von späteren Edits.
    fn pump_render_queue(&mut self, now: f64) {
        let Some(id) = self.state.render_queue.next_to_start() else {
            return;
        };
        let Some(job) = self.state.render_queue.job(id) else {
            return;
        };
        let (plan, settings) = (job.plan.clone(), job.settings.clone());
        match self.services.start_sequence_export(plan, settings) {
            Ok(service_id) => self.state.render_queue.mark_started(id, service_id, now),
            Err(e) => self.state.render_queue.mark_start_failed(id, e, now),
        }
    }

    /// Einzel-Frame-Export am Programmmonitor: den komponierten Frame am
    /// Playhead (Sequenz-Auflösung) in die gewählte Bilddatei rendern.
    fn export_program_frame(&mut self, path: std::path::PathBuf, now: f64) {
        let seq = self.state.timeline.settings;
        let (w, h) = (seq.width, seq.height);
        let fps = seq.rate.fps();
        if w < 16 || h < 16 || !(fps > 0.0) {
            self.state
                .app
                .set_status_message(Some("Ungültige Sequenz-Auflösung für den Frame-Export.".into()), now);
            return;
        }
        let frame = (self.state.timeline.playhead_sec * fps).round().max(0.0) as u64;
        let plan = crate::core::export::build_cache_plan(
            &self.state.timeline,
            &self.state.media,
            w,
            h,
            fps,
            frame,
            frame + 1,
        );
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_else(|| "png".into());
        let args = crate::core::export::frame_export_args(&ext);
        self.services.export_frame(plan, args, path);
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

/// Testmodus-Wiedergaberate (EDITRON_TEST_RATE, z. B. `2`, `-1`, `0.5`) für
/// die manuelle Shuttle-/Rückwärts-Audio-Verifikation mit EDITRON_TEST_PLAY;
/// Standard 1,0.
fn test_play_rate() -> f64 {
    std::env::var("EDITRON_TEST_RATE")
        .ok()
        .and_then(|s| s.trim().replace(',', ".").parse::<f64>().ok())
        .filter(|r| r.is_finite() && *r != 0.0)
        .unwrap_or(1.0)
}

/// Vom Fenstersystem gemeldeter DPI-Faktor des aktuellen Monitors (Basis für
/// die automatische HiDPI-Erkennung; aktualisiert sich beim Verschieben auf
/// einen Monitor mit anderer DPI). Ungültige Werte fallen auf 1,0 zurück.
fn detected_dpi_scale(rl: &RaylibHandle) -> f32 {
    let s = rl.get_window_scale_dpi().x;
    if s.is_finite() && s > 0.0 {
        s
    } else {
        1.0
    }
}

/// GPU↔CPU-Paritätstest des Grades (Stufe-3-DoD): ein nicht-trivialer Grade
/// wird auf der GPU (Fragment-Shader, Dither AUS) in eine RenderTexture und auf
/// der CPU (`grade::grade_buffer`, derselbe `grade_pixel`) gerechnet; die
/// maximale/mittlere Kanal-Differenz wird gemeldet. Quelle = horizontaler
/// Verlauf (jede Zeile gleich ⇒ unempfindlich gegen die RT-Vertikalspiegelung).
fn run_grade_parity(
    rl: &mut raylib::RaylibHandle,
    thread: &raylib::RaylibThread,
    grade_shader: &mut Option<ui::grade_shader::GradeShader>,
) {
    use raylib::prelude::*;
    let Some(gs) = grade_shader.as_mut() else {
        println!("PARITY SKIP: Grade-Shader nicht ladbar");
        return;
    };
    let n = 256usize;
    // Quelle: horizontaler Farbverlauf, der alle Kanäle bewegt.
    let mut src = vec![0u8; n * n * 4];
    for y in 0..n {
        for x in 0..n {
            let i = (y * n + x) * 4;
            src[i] = x as u8;
            src[i + 1] = ((x * 7 / 3) % 256) as u8;
            src[i + 2] = (255 - x as i32) as u8;
            src[i + 3] = 255;
        }
    }
    let img = Image::gen_image_color(n as i32, n as i32, Color::BLACK);
    let mut tex = rl.load_texture_from_image(thread, &img).expect("Quelltextur");
    tex.set_texture_filter(thread, TextureFilter::TEXTURE_FILTER_POINT);
    tex.update_texture(&src).expect("Upload");

    // Nicht-trivialer Grade (ohne Vignette — reine Geometrie, separat getestet).
    let mut g = crate::core::grade::ColorGrade::default();
    g.temperature = 30.0;
    g.contrast = 40.0;
    g.exposure = 0.4;
    g.saturation = 140.0;
    g.shadows = 30.0;
    g.highlights = -20.0;
    g.gamma.luma = 0.2;
    g.gain = crate::core::grade::WheelValue { x: 0.3, y: -0.2, luma: 0.0 };
    let params = crate::core::grade::precompute(&g);

    // GPU-Pfad: in eine RenderTexture rendern, Dither AUS.
    gs.apply(&params);
    gs.set_dither(false);
    let mut rt = rl.load_render_texture(thread, n as u32, n as u32).expect("RT");
    {
        let mut tm = rl.begin_texture_mode(thread, &mut rt);
        tm.clear_background(Color::BLACK);
        let mut sm = tm.begin_shader_mode(&mut gs.shader);
        sm.draw_texture_pro(
            &tex,
            Rectangle::new(0.0, 0.0, n as f32, n as f32),
            Rectangle::new(0.0, 0.0, n as f32, n as f32),
            Vector2::zero(),
            0.0,
            Color::WHITE,
        );
    }
    let rt_raw: &raylib::ffi::RenderTexture2D =
        AsRef::<raylib::ffi::RenderTexture2D>::as_ref(&rt);
    let tex_id = rt_raw.texture.id;
    let fmt = raylib::ffi::PixelFormat::PIXELFORMAT_UNCOMPRESSED_R8G8B8A8 as i32;
    let gpu: Vec<u8> = unsafe {
        let ptr = raylib::ffi::rlReadTexturePixels(tex_id, n as i32, n as i32, fmt);
        let v = std::slice::from_raw_parts(ptr as *const u8, n * n * 4).to_vec();
        raylib::ffi::MemFree(ptr);
        v
    };

    // CPU-Pfad: gleiche Quelle f32 → grade_buffer → u8 (kein Dither).
    let mut cpu = crate::core::pixbuf::rgba8_to_f32(&src);
    crate::core::grade::grade_buffer(&mut cpu, n, n, (0, 0, n, n), &params, 4);
    let cpu_u8 = crate::core::pixbuf::f32_to_rgba8(&cpu);

    let mut max_d = 0i32;
    let mut sum = 0i64;
    let mut cnt = 0i64;
    for y in 0..n {
        for x in 0..n {
            let i = (y * n + x) * 4;
            for k in 0..3 {
                let d = (gpu[i + k] as i32 - cpu_u8[i + k] as i32).abs();
                max_d = max_d.max(d);
                sum += d as i64;
                cnt += 1;
            }
        }
    }
    let mean = sum as f64 / cnt as f64;
    let tol = 4;
    println!(
        "PARITY grade GPU vs CPU ({n}x{n}): max_diff={max_d} LSB, mean={mean:.3} LSB — {}",
        if max_d <= tol { "OK" } else { "FAIL" }
    );
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

    let mut app = App::new();
    // HiDPI: effektiver UI-Scale (Override aus Einstellungen/Env > Monitor-DPI).
    // Die Font-Atlanten werden in physikalischer Auflösung für diesen Faktor
    // gerastert; bei Laufzeit-Wechsel (Monitorwechsel) baut die Schleife sie neu.
    let mut ui_scale = app.state.settings.resolve_ui_scale(detected_dpi_scale(&rl));
    app.state.app.ui_scale = ui_scale;
    let mut fonts = ui::text::Fonts::load(&mut rl, &thread, ui_scale);
    let icons = ui::icons::IconSet::load();
    let mut grade_shader = ui::grade_shader::GradeShader::load(&mut rl, &thread);

    // GPU↔CPU-Paritätstest (Definition of Done Stufe 3): denselben Grade auf
    // GPU (Shader) und CPU (grade_buffer) rechnen und die Differenz melden.
    if std::env::var("EDITRON_TEST_PARITY").is_ok() {
        run_grade_parity(&mut rl, &thread, &mut grade_shader);
        return;
    }
    let mut fx_renderer = ui::fx_shader::EffectChainRenderer::load(&mut rl, &thread);
    // Effekt-Jobs des letzten Frames — werden vor dem nächsten verarbeitet.
    let mut pending_fx_jobs: Vec<ui::fx_shader::EffectJob> = Vec::new();
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
                app.state.playback.program_rate = test_play_rate();
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
    // Testmodus: Demo-Bins für den Medien-Browser anlegen (EDITRON_TEST_BINS=1).
    if std::env::var("EDITRON_TEST_BINS").is_ok() {
        use crate::core::bin::ROOT_BIN_ID;
        let footage = app.state.media.create_bin(ROOT_BIN_ID, "Footage");
        app.state.media.create_bin(&footage, "B-Roll");
        app.state.media.create_bin(ROOT_BIN_ID, "Musik");
        app.state.media.clear_history();
    }
    // Testmodus: Ansichtsmodus des Browsers wählen (EDITRON_TEST_MEDIA_VIEW=list).
    if let Ok(v) = std::env::var("EDITRON_TEST_MEDIA_VIEW") {
        app.state.media.view.mode = if v == "list" {
            crate::core::bin::ViewMode::List
        } else {
            crate::core::bin::ViewMode::Grid
        };
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
            "proxy" => Some(stores::DialogId::ProxySettings),
            "settings" => Some(stores::DialogId::Settings),
            "autosave" => Some(stores::DialogId::AutosaveVersions),
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

    // Absturz-Wiederherstellung: ist eine Autosave-Version neuer als die
    // Projektdatei (CLI-geöffnet oder zuletzt verwendet), wurde Editron
    // vermutlich unerwartet beendet — den Versionen-Dialog mit Hinweis
    // anbieten. Nicht im Screenshot-/Testmodus und nur, wenn kein Dialog
    // bereits offen ist (z. B. via EDITRON_TEST_DIALOG).
    if shot_path.is_none() && app.state.app.open_dialog.is_none() {
        let project_path = app
            .state
            .project
            .path
            .clone()
            .or_else(|| app.state.project.recent.first().map(std::path::PathBuf::from));
        if let Some(p) = project_path {
            if let Some(version) = core::autosave::find_crash_recovery(&p) {
                app.state.app.autosave_recover_hint = Some(version);
                app.state.app.open_dialog = Some(stores::DialogId::AutosaveVersions);
            }
        }
    }

    // Screenshot-Vergleich über mehrere Scales: das Fenster auf die
    // physikalische Größe (logisches 1440×900 × Scale) bringen, damit die
    // Aufnahmen bei 1.0/1.5/2.0 dasselbe Layout zeigen — nur schärfer. Im
    // Normalbetrieb bleibt das Fenster unangetastet (logisch = render/scale).
    if shot_path.is_some() && (ui_scale - 1.0).abs() > 0.01 {
        rl.set_window_size((1440.0 * ui_scale) as i32, (900.0 * ui_scale) as i32);
    }

    loop {
        // Schließen-Anfrage (X / ESC). `window_should_close()` setzt das Flag
        // jeden Aufruf zurück — daher abfangbar: laufen noch Render-Jobs, wird
        // statt zu beenden ein Bestätigungsdialog geöffnet (das eigentliche
        // Beenden setzt `quit_requested`).
        if rl.window_should_close() {
            if app.state.render_queue.has_active() {
                app.state.app.open_dialog = Some(stores::DialogId::ConfirmQuitRender);
            } else {
                break;
            }
        }
        if app.state.app.quit_requested {
            break;
        }
        let now = rl.get_time();
        let dt = rl.get_frame_time() as f64;

        // Test: Laufzeit-Scale-Wechsel simulieren (Monitorwechsel/Override-
        // Änderung). EDITRON_TEST_SCALE_TO=2.0 stellt nach 60 Frames um und
        // verifiziert das Neu-Rastern der Atlanten ohne Neustart.
        if frame_count == 60 {
            if let Some(f) = std::env::var("EDITRON_TEST_SCALE_TO")
                .ok()
                .and_then(|v| v.trim().replace(',', ".").parse::<f32>().ok())
            {
                app.state.settings.ui_scale = Some(f);
            }
        }

        // HiDPI: effektiven Scale neu bestimmen (Override > Monitor-DPI). Bei
        // Wechsel — etwa nach dem Ziehen auf einen Monitor mit anderer DPI oder
        // einer Override-Änderung — die Font-Atlanten in der neuen physikalischen
        // Auflösung neu rastern. Kein Neustart nötig.
        let new_scale = app
            .state
            .settings
            .resolve_ui_scale(detected_dpi_scale(&rl));
        if (new_scale - ui_scale).abs() > 0.005 {
            ui_scale = new_scale;
            fonts = ui::text::Fonts::load(&mut rl, &thread, ui_scale);
        }
        app.state.app.ui_scale = ui_scale;

        let mut input = InputState::collect(&mut rl, &mut app.persist.clock);
        if let Some(pos) = test_mouse {
            input.mouse = pos;
        }
        // Maus/Delta kommen in Framebuffer-Pixeln aus raylib — in den logischen
        // UI-Raum übersetzen, in dem das Layout rechnet.
        input.mouse.x /= ui_scale;
        input.mouse.y /= ui_scale;
        input.mouse_delta.x /= ui_scale;
        input.mouse_delta.y /= ui_scale;

        // Logische Zeichenfläche = Framebuffer-Pixel ÷ Scale. Auf macOS/Retina
        // ist `render` bereits 2× `screen`; auf X11 ist beides gleich und der
        // Scale kommt aus der Monitor-DPI — beide Fälle ergeben hier konsistent
        // die logische Größe.
        let screen = Rect::new(
            0.0,
            0.0,
            rl.get_render_width() as f32 / ui_scale,
            rl.get_render_height() as f32 / ui_scale,
        );

        // HiDPI-Diagnose (v. a. für die macOS-Retina-Verifikation, siehe
        // docs/HIDPI_MACOS.md): einmal pro Sekunde die rohen raylib-Maße + die
        // abgeleiteten logischen Werte + die rohe vs. logische Maus loggen.
        // Auf X11 gilt screen == render; auf macOS+HIGHDPI render == screen×scale.
        if frame_count % 60 == 0 && std::env::var("EDITRON_DPI_DEBUG").is_ok() {
            let raw_mouse = rl.get_mouse_position();
            eprintln!(
                "[dpi] dpi={:.3} ui_scale={:.3} screen={}x{} render={}x{} logical={:.0}x{:.0} mouse_raw=({:.0},{:.0}) mouse_logical=({:.0},{:.0})",
                detected_dpi_scale(&rl),
                ui_scale,
                rl.get_screen_width(),
                rl.get_screen_height(),
                rl.get_render_width(),
                rl.get_render_height(),
                screen.w,
                screen.h,
                raw_mouse.x,
                raw_mouse.y,
                input.mouse.x,
                input.mouse.y,
            );
        }

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
        app.maybe_revalidate_proxies(now);
        app.state.app.tick_status(now);
        // Zeitgesteuertes Autosave mit Versionen (nicht im Screenshot-/
        // Testmodus — sonst würden Testläufe echte Projektordner zumüllen).
        if shot_path.is_none() {
            app.maybe_autosave(now);
        }
        // „Sequenz an Medien anpassen?“ — ausstehenden Vorschlag (erster
        // Clip-Drop in eine leere Timeline) als modalen Prompt öffnen,
        // sobald kein anderer Dialog im Weg ist.
        if app.state.timeline.pending_media_match.is_some()
            && app.state.app.open_dialog.is_none()
        {
            app.state.app.open_dialog = Some(stores::DialogId::MatchMedia);
        }
        let decode_t0 = std::time::Instant::now();
        app.player
            .tick(&mut rl, &thread, &mut app.state, &mut app.textures, now);
        // Decode-/Frame-Zeit (EMA-geglättet) für das Performance-Overlay.
        {
            let perf = &mut app.state.monitor.perf;
            let decode_ms = decode_t0.elapsed().as_secs_f32() * 1000.0;
            perf.decode_ms += (decode_ms - perf.decode_ms) * 0.15;
            let frame_ms = (dt as f32) * 1000.0;
            perf.frame_ms += (frame_ms - perf.frame_ms) * 0.15;
            perf.fps = rl.get_fps() as f32;
        }
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
        // Audio-Scrubbing-Flag zurücksetzen: die Panels (Lineal/Scrubber)
        // setzen es im folgenden UI-Frame neu, solange aktiv gezogen wird.
        app.state.playback.scrub_active = false;

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
            ui_scale,
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
        app.settings_dialog
            .render(&mut ui, &mut app.state, &app.services);
        app.autosave_dialog.render(&mut ui, &mut app.state);
        app.speed_dialog.render(&mut ui, &mut app.state);
        app.marker_dialog.render(&mut ui, &mut app.state);
        app.media_dialogs.render(&mut ui, &mut app.state);
        app.interop_report.render(&mut ui, &mut app.state);
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
        let scrub_requests = std::mem::take(&mut ui.scrub_requests);
        pending_fx_jobs = std::mem::take(&mut ui.effect_requests);
        drop(ui);
        d.set_mouse_cursor(cursor);
        drop(d);

        // ---- Nach dem Frame: Commands + Texture-Uploads ----
        for item in dispatch {
            app.run_command(&item.command, item.arg.as_ref(), now);
        }
        // Autosave-Version öffnen (vom Versionen-Dialog angefordert).
        if let Some(path) = app.state.app.autosave_open_request.take() {
            app.open_autosave_version(&path, now);
        }
        for req in scrub_requests {
            app.services
                .request_scrub_thumb(&req.asset_id, &req.path, req.time, req.bucket);
        }
        let upload_t0 = std::time::Instant::now();
        app.textures
            .process_requests(&mut rl, &thread, texture_requests);
        {
            let perf = &mut app.state.monitor.perf;
            let upload_ms = upload_t0.elapsed().as_secs_f32() * 1000.0;
            perf.upload_ms += (upload_ms - perf.upload_ms) * 0.15;
        }
        // Keymap-Änderungen aus dem Shortcut-Editor in den Resolver spiegeln.
        if app.state.app.open_dialog == Some(stores::DialogId::Shortcuts) {
            app.resolver
                .set_bindings(app.state.keymap.effective_bindings());
        }

        // ---- Projekt: Änderungen erkennen + Fenstertitel pflegen ----
        // Revision über ALLE Sequenzen aggregieren (Änderung in irgendeiner
        // Sequenz markiert das Projekt als ungespeichert).
        let (t_rev, m_rev) = (app.state.timeline.aggregate_revision(), app.state.media.revision);
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

    // Laufende Exporte + Proxy-Transcodes + Render-Cache-Jobs hart beenden —
    // sonst laufen ffmpeg-Waisen weiter.
    app.services.cancel_all_jobs();
    app.services.cancel_all_proxies();
    // Render-Cache-Dateien sind sitzungsgebunden (der Store wird nicht
    // persistiert) — verwaiste Dateien beim Beenden aufräumen.
    for f in app.state.render_cache.clear() {
        let _ = std::fs::remove_file(f);
    }
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
        DragPayload::Sequences(ids) => {
            if ids.len() == 1 {
                "1 Sequenz".to_string()
            } else {
                format!("{} Sequenzen", ids.len())
            }
        }
        DragPayload::Bins(ids) => {
            if ids.len() == 1 {
                "1 Ordner".to_string()
            } else {
                format!("{} Ordner", ids.len())
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
