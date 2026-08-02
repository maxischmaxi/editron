//! App-Einstellungen-Dialog (modal wie der Export-/Sequenz-Dialog): Kategorien
//! links, Inhalte rechts. Alle Änderungen wirken sofort und werden direkt in
//! die `settings.json` persistiert (`state.settings` ist die einzige Quelle —
//! UI und Subsysteme lesen denselben Wert). Es werden nur Kategorien für
//! tatsächlich existierende Subsysteme gebaut.

use crate::core::settings::{
    AUTOSAVE_INTERVAL_MAX, AUTOSAVE_INTERVAL_MIN, AUTOSAVE_VERSIONS_MAX, AUTOSAVE_VERSIONS_MIN,
    UI_SCALE_MAX, UI_SCALE_MIN,
};
use crate::overlays::sequence_dialog::{checkbox, labeled_row, section};
use crate::services::Services;
use crate::state::AppState;
use crate::stores::{DialogId, PREVIEW_SCALES};
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::select::select;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::widgets::{drop_shadow, slider, IconButton, TextButton, TextButtonStyle};
use crate::ui::{FontKind, Interaction, Ui};
use raylib::consts::{KeyboardKey, MouseCursor};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Category {
    #[default]
    General,
    Autosave,
    Playback,
    Media,
    Appearance,
}

const CATEGORIES: [(Category, &str, &str); 5] = [
    (Category::General, "Allgemein", "sliders-horizontal"),
    (Category::Autosave, "Autosave", "history"),
    (Category::Playback, "Wiedergabe", "gauge"),
    (Category::Media, "Medien", "film"),
    (Category::Appearance, "Erscheinungsbild", "palette"),
];

const RAIL_W: f32 = 176.0;

#[derive(Default)]
pub struct SettingsDialog {
    cat: Category,
    was_open: bool,
    cache_input: TextInputState,
    synced_cache: String,
    ffmpeg_input: TextInputState,
    synced_ffmpeg: String,
    ffprobe_input: TextInputState,
    synced_ffprobe: String,
    whisper_input: TextInputState,
    synced_whisper: String,
    whisper_model_input: TextInputState,
    synced_whisper_model: String,
    /// Letztes Validierungs-Ergebnis der ffmpeg-Pfade `(ok, Text)`.
    ffmpeg_status: Option<(bool, String)>,
    /// UI-Scale wurde live geändert, aber noch nicht auf die Platte geschrieben
    /// (Persistenz erst beim Loslassen — kein fsync je Frame während des Ziehens).
    ui_scale_dirty: bool,
}

impl SettingsDialog {
    pub fn render(&mut self, ui: &mut Ui, state: &mut AppState, services: &Services) {
        if state.app.open_dialog != Some(DialogId::Settings) {
            self.was_open = false;
            return;
        }
        if !self.was_open {
            self.was_open = true;
            self.ffmpeg_status = None;
        }
        self.sync_inputs(state);

        // ESC schließt — außer ein Popup/Textfeld verarbeitet die Taste selbst.
        let esc = ui.input.keys.iter().any(|k| k.key == KeyboardKey::KEY_ESCAPE);
        if esc && ui.persist.select.popup.is_none() && ui.persist.keyboard_focus == 0 {
            state.app.open_dialog = None;
            return;
        }

        ui.fill(ui.screen, theme::with_alpha(theme::BLACK, 130));
        let w = 700f32.min(ui.screen.w - 32.0);
        let h = 480f32.min(ui.screen.h - 32.0);
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
        ui.icon("sliders-horizontal", icon_cell, 18.0, theme::TEXT_2);
        hi.cut_left(8.0);
        ui.text_left("Einstellungen", hi, theme::TEXT_1, FontKind::Sans16Semibold);
        let close = Rect::new(head.right() - 16.0 - 28.0, head.y + 10.0, 28.0, 28.0);
        if IconButton::new("x")
            .tooltip("Schließen (Esc)")
            .show(ui, "settings.close", close)
            .clicked
        {
            state.app.open_dialog = None;
            return;
        }

        // ---- Fußzeile ----
        let footer = area.cut_bottom(52.0);

        // ---- Kategorien-Leiste (links) ----
        let rail = area.cut_left(RAIL_W);
        ui.vline(rail.right() - 1.0, rail.y, rail.h, theme::LINE);
        let mut r = rail.inset_xy(8.0, 8.0);
        for (cat, label, icon) in CATEGORIES {
            let row = r.cut_top(34.0);
            r.cut_top(2.0);
            if cat_row(ui, ("settings.cat", label), row, icon, label, cat == self.cat).clicked {
                self.cat = cat;
            }
        }

        // ---- Inhalt (rechts) ----
        let body = area.inset_xy(18.0, 12.0);
        match self.cat {
            Category::General => self.content_general(ui, state, body),
            Category::Autosave => self.content_autosave(ui, state, body),
            Category::Playback => self.content_playback(ui, state, body),
            Category::Media => self.content_media(ui, state, services, body),
            Category::Appearance => self.content_appearance(ui, state, body),
        }

        // ---- Fußzeile: Schließen ----
        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        let mut f = footer.inset_xy(16.0, 0.0);
        let btn_row = f.cut_top(52.0);
        let note = Rect::new(btn_row.x, btn_row.y, btn_row.w - 140.0, btn_row.h);
        ui.text_left(
            "Änderungen wirken sofort und werden gespeichert.",
            note,
            theme::TEXT_3,
            FontKind::Sans12,
        );
        let done = TextButton::new("Schließen").style(TextButtonStyle::Solid);
        let dw = done.measure(ui).max(110.0);
        if done
            .show(ui, "settings.done", Rect::new(btn_row.right() - dw, btn_row.y + 12.0, dw, 28.0))
            .clicked
        {
            state.app.open_dialog = None;
        }
    }

    // ---------------------------------------------------- Inhaltsbereiche

    fn content_general(&mut self, ui: &mut Ui, state: &mut AppState, mut body: Rect) {
        section(ui, &mut body, "Sprache");
        let r = labeled_row(ui, &mut body, "Programmsprache");
        // Vorbereitet für Lokalisierung; aktuell nur Deutsch.
        let _ = select(ui, "settings.lang", r, &["Deutsch"], 0);
        let hint = body.cut_top(16.0);
        ui.text_left(
            "Weitere Sprachen folgen.",
            hint,
            theme::TEXT_3,
            FontKind::Sans12,
        );
        body.cut_top(8.0);

        section(ui, &mut body, "Wiedergabe");
        let r = labeled_row(ui, &mut body, "Vorschau-Auflösung");
        let labels: Vec<&str> = PREVIEW_SCALES.iter().map(|(_, l)| *l).collect();
        let cur = nearest_scale_idx(state.settings.default_preview_scale);
        if let Some(i) = select(ui, "settings.previewScale", r, &labels, cur) {
            let v = PREVIEW_SCALES[i].0;
            state.settings.default_preview_scale = v;
            // Sofort auf die Monitore anwenden (live wirksam).
            state.monitor.program_scale = v;
            state.monitor.source_scale = v;
            state.settings.save();
        }
        let hint = body.cut_top(16.0);
        ui.text_left(
            "Standard für Programm- und Quellmonitor (niedriger = flüssiger auf schwacher Hardware).",
            hint,
            theme::TEXT_3,
            FontKind::Sans12,
        );
    }

    fn content_autosave(&mut self, ui: &mut Ui, state: &mut AppState, mut body: Rect) {
        section(ui, &mut body, "Automatisches Speichern");

        let row = body.cut_top(24.0);
        body.cut_top(6.0);
        let enabled = state.settings.autosave.enabled;
        if checkbox(ui, "settings.autosaveOn", row, "Autosave aktivieren", enabled, true).clicked {
            state.settings.autosave.enabled = !enabled;
            state.settings.save();
        }

        let on = state.settings.autosave.enabled;
        // Intervall
        let r = labeled_row(ui, &mut body, "Intervall");
        let mut v = state.settings.autosave.interval_min as f64;
        let (track, label_cell) = split_value(r);
        if on {
            slider(ui, "settings.interval", track, &mut v, AUTOSAVE_INTERVAL_MIN as f64, AUTOSAVE_INTERVAL_MAX as f64, theme::ACCENT);
            let nv = (v.round() as u32).clamp(AUTOSAVE_INTERVAL_MIN, AUTOSAVE_INTERVAL_MAX);
            if nv != state.settings.autosave.interval_min {
                state.settings.autosave.interval_min = nv;
                state.settings.save();
            }
        }
        let mins = state.settings.autosave.interval_min;
        ui.text_left(
            &format!("{mins} Min"),
            label_cell,
            if on { theme::TEXT_1 } else { theme::TEXT_3 },
            FontKind::Mono12,
        );

        // Versionen
        let r = labeled_row(ui, &mut body, "Versionen behalten");
        let mut v = state.settings.autosave.max_versions as f64;
        let (track, label_cell) = split_value(r);
        if on {
            slider(ui, "settings.versions", track, &mut v, AUTOSAVE_VERSIONS_MIN as f64, AUTOSAVE_VERSIONS_MAX as f64, theme::ACCENT);
            let nv = (v.round() as u32).clamp(AUTOSAVE_VERSIONS_MIN, AUTOSAVE_VERSIONS_MAX);
            if nv != state.settings.autosave.max_versions {
                state.settings.autosave.max_versions = nv;
                state.settings.save();
            }
        }
        let n = state.settings.autosave.max_versions;
        ui.text_left(
            &format!("{n}"),
            label_cell,
            if on { theme::TEXT_1 } else { theme::TEXT_3 },
            FontKind::Mono12,
        );

        body.cut_top(10.0);
        for line in [
            "Versionen liegen in „.etron-autosave“ neben der Projektdatei und",
            "lassen die Originaldatei unberührt. Datei-Menü → „Autosave-Versionen…“.",
        ] {
            let row = body.cut_top(16.0);
            ui.text_left(line, row, theme::TEXT_3, FontKind::Sans12);
        }
    }

    fn content_playback(&mut self, ui: &mut Ui, state: &mut AppState, mut body: Rect) {
        section(ui, &mut body, "Dekodierung");

        let row = body.cut_top(24.0);
        body.cut_top(6.0);
        let hw = state.settings.hwaccel;
        if checkbox(ui, "settings.hwaccel", row, "Hardware-Decode (mit Software-Fallback)", hw, true).clicked {
            state.settings.hwaccel = !hw;
            state.settings.save();
        }
        let hint = body.cut_top(16.0);
        ui.text_left(
            "Nutzt die GPU zum Dekodieren; fällt bei Fehlern automatisch auf Software zurück.",
            hint,
            theme::TEXT_3,
            FontKind::Sans12,
        );
        body.cut_top(10.0);

        section(ui, &mut body, "Zwischenspeicher");
        // Frame-Cache-Budget (RAM, Scrubbing).
        let r = labeled_row(ui, &mut body, "Frame-Cache (RAM)");
        let mut field = r;
        let unit = field.cut_right(34.0);
        let res = self.cache_input.show(ui, "settings.cacheMb", field, "z. B. 2048");
        ui.text_left("MB", Rect::new(unit.x + 6.0, unit.y, unit.w, unit.h), theme::TEXT_3, FontKind::Sans12);
        if res.changed || res.submitted {
            if let Ok(mb) = self.cache_input.text.trim().parse::<u64>() {
                let mb = mb.clamp(64, 65536);
                if mb != state.settings.frame_cache_budget_mb {
                    state.settings.frame_cache_budget_mb = mb;
                    state.settings.save();
                }
            }
        }

        // Render-Cache-Codec.
        let r = labeled_row(ui, &mut body, "Render-Cache-Codec");
        let codecs = [
            crate::core::settings::RenderCacheCodec::ProresProxy,
            crate::core::settings::RenderCacheCodec::DnxhrLb,
            crate::core::settings::RenderCacheCodec::H264Fast,
        ];
        let labels = ["ProRes 422 Proxy", "DNxHR LB", "H.264 (schnell)"];
        let cur = codecs.iter().position(|c| *c == state.settings.render_cache_codec).unwrap_or(0);
        if let Some(i) = select(ui, "settings.rcCodec", r, &labels, cur) {
            state.settings.render_cache_codec = codecs[i];
            state.settings.save();
        }
        let hint = body.cut_top(16.0);
        ui.text_left(
            "Codec für „Render In to Out“. Intra-Frame ⇒ überall sofort seekbar.",
            hint,
            theme::TEXT_3,
            FontKind::Sans12,
        );
    }

    fn content_media(&mut self, ui: &mut Ui, state: &mut AppState, services: &Services, mut body: Rect) {
        section(ui, &mut body, "FFmpeg");

        // Status-Zeile: erkannte Version + Pfad.
        let row = body.cut_top(20.0);
        let available = state.app.ffmpeg.as_ref().is_some_and(|i| i.available);
        let (txt, col, icon) = if available {
            let ver = state
                .app
                .ffmpeg
                .as_ref()
                .and_then(|i| i.version.clone())
                .unwrap_or_else(|| "?".into());
            (format!("Erkannt: ffmpeg {ver}"), theme::SUCCESS, "circle-check")
        } else {
            (
                "FFmpeg nicht gefunden — Medienfunktionen deaktiviert.".to_string(),
                theme::DANGER,
                "triangle-alert",
            )
        };
        ui.icon(icon, Rect::new(row.x, row.y, 14.0, row.h), 14.0, col);
        ui.text_left(&txt, Rect::new(row.x + 20.0, row.y, row.w - 20.0, row.h), col, FontKind::Sans12);
        body.cut_top(6.0);

        // ffmpeg-Pfad-Override.
        self.binary_row(ui, state, services, &mut body, "ffmpeg-Pfad", "ffmpeg");
        // ffprobe-Pfad-Override.
        self.binary_row(ui, state, services, &mut body, "ffprobe-Pfad", "ffprobe");

        if let Some((ok, msg)) = &self.ffmpeg_status {
            let row = body.cut_top(18.0);
            let col = if *ok { theme::SUCCESS } else { theme::DANGER };
            ui.text_left(msg, row, col, FontKind::Sans12);
        }
        body.cut_top(8.0);
        let hint = body.cut_top(16.0);
        ui.text_left(
            "Leer lassen = im PATH suchen. Übersteuert die automatische Suche.",
            hint,
            theme::TEXT_3,
            FontKind::Sans12,
        );

        // ---- Auto-Transkription (Whisper) ----
        body.cut_top(10.0);
        section(ui, &mut body, "Auto-Transkription (Whisper)");

        // Status: Modell vorhanden?
        let row = body.cut_top(20.0);
        let ready = state.settings.transcription_ready();
        let (txt, col, icon) = if ready {
            ("Einsatzbereit — Modell konfiguriert.".to_string(), theme::SUCCESS, "circle-check")
        } else {
            ("Kein Modell — Auto-Transkription deaktiviert.".to_string(), theme::TEXT_3, "info")
        };
        ui.icon(icon, Rect::new(row.x, row.y, 14.0, 14.0), 14.0, col);
        ui.text_left(&txt, Rect::new(row.x + 20.0, row.y, row.w - 20.0, row.h), col, FontKind::Sans12);
        body.cut_top(4.0);

        self.whisper_row(ui, state, services, &mut body, "whisper.cpp", false);
        self.whisper_row(ui, state, services, &mut body, "Modell (ggml)", true);

        // Standard-Sprache.
        let r = labeled_row(ui, &mut body, "Sprache");
        let labels: Vec<&str> = crate::core::transcribe::LANGUAGES.iter().map(|(_, l)| *l).collect();
        let cur = crate::core::transcribe::language_index(&state.settings.whisper_language);
        let sel = Rect::new(r.x, r.y + 1.0, r.w.min(260.0), 24.0);
        if let Some(i) = select(ui, "settings.whisperLang", sel, &labels, cur) {
            state.settings.whisper_language = crate::core::transcribe::LANGUAGES[i].0.to_string();
            state.settings.save();
        }

        body.cut_top(6.0);
        let hint = body.cut_top(16.0);
        ui.text_left(
            "whisper.cpp-CLI (Standard whisper-cli) + Modell (ggml-*.bin) — beide einmalig festlegen.",
            hint,
            theme::TEXT_3,
            FontKind::Sans12,
        );
    }

    /// Eine Pfad-Zeile für die Whisper-CLI bzw. das Whisper-Modell: Textfeld +
    /// Durchsuchen + Übernehmen. `model = true` ⇒ Modellpfad, sonst Binärpfad.
    fn whisper_row(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        services: &Services,
        body: &mut Rect,
        label: &str,
        model: bool,
    ) {
        let mut r = labeled_row(ui, body, label);
        let apply_w = 92.0;
        let browse_w = 34.0;
        let apply_cell = r.cut_right(apply_w);
        r.cut_right(6.0);
        let browse_cell = r.cut_right(browse_w);
        r.cut_right(6.0);
        let field = r;
        let res = if model {
            self.whisper_model_input.show(ui, "settings.whisperModel", field, "Pfad zum Modell")
        } else {
            self.whisper_input.show(ui, "settings.whisperBin", field, "whisper-cli (im PATH)")
        };
        if IconButton::new("folder-open")
            .tooltip("Durchsuchen…")
            .show(ui, ("settings.whisperBrowse", model), browse_cell)
            .clicked
        {
            if model {
                services.pick_whisper_model();
            } else {
                services.pick_whisper_binary();
            }
        }
        let apply = TextButton::new("Übernehmen").style(TextButtonStyle::Outline);
        if apply.show(ui, ("settings.whisperApply", model), apply_cell).clicked || res.submitted {
            let raw = if model {
                self.whisper_model_input.text.trim().to_string()
            } else {
                self.whisper_input.text.trim().to_string()
            };
            let path = if raw.is_empty() { None } else { Some(raw.clone()) };
            if model {
                state.settings.whisper_model = path;
                self.synced_whisper_model = raw;
            } else {
                state.settings.whisper_path = path;
                self.synced_whisper = raw;
            }
            state.settings.save();
        }
    }

    fn content_appearance(&mut self, ui: &mut Ui, state: &mut AppState, mut body: Rect) {
        section(ui, &mut body, "UI-Skalierung (HiDPI)");

        let row = body.cut_top(24.0);
        body.cut_top(6.0);
        let auto = state.settings.ui_scale.is_none();
        if checkbox(ui, "settings.uiAuto", row, "Automatisch (Monitor-DPI)", auto, true).clicked {
            if auto {
                // Auf manuell umschalten: aktuellen effektiven Scale (auf ganze
                // Prozent gerundet) übernehmen — kein Sprung beim Umschalten.
                let cur = ((state.app.ui_scale * 100.0).round() / 100.0)
                    .clamp(UI_SCALE_MIN, UI_SCALE_MAX);
                state.settings.ui_scale = Some(cur);
            } else {
                state.settings.ui_scale = None;
            }
            state.settings.save();
        }

        if let Some(scale) = state.settings.ui_scale {
            let r = labeled_row(ui, &mut body, "Faktor");
            let mut v = scale as f64;
            let (track, label_cell) = split_value(r);
            let it = slider(ui, "settings.uiScale", track, &mut v, UI_SCALE_MIN as f64, UI_SCALE_MAX as f64, theme::ACCENT);
            // Auf ganze Prozent (1%-Schritte) einrasten und LIVE anwenden.
            let snapped = ((v * 100.0).round() / 100.0)
                .clamp(UI_SCALE_MIN as f64, UI_SCALE_MAX as f64) as f32;
            if (snapped - scale).abs() > 0.0005 {
                state.settings.ui_scale = Some(snapped);
                // Während des Ziehens nur im RAM anwenden (treibt den Scale); die
                // Datei erst beim Loslassen schreiben (sonst fsync je Frame).
                self.ui_scale_dirty = true;
            }
            if self.ui_scale_dirty && !it.held {
                state.settings.save();
                self.ui_scale_dirty = false;
            }
            ui.text_left(
                &format!("{:.0}%", snapped * 100.0),
                label_cell,
                theme::TEXT_1,
                FontKind::Mono12,
            );
        }
        let hint = body.cut_top(16.0);
        ui.text_left(
            &format!("Aktiv: {:.0}%. Änderung wirkt sofort (Atlanten werden neu gerastert).", state.app.ui_scale * 100.0),
            hint,
            theme::TEXT_3,
            FontKind::Sans12,
        );
    }

    // ----------------------------------------------------------- Helfer

    /// Eine Pfad-Zeile für ein Binary (`ffmpeg`/`ffprobe`): Textfeld +
    /// Durchsuchen + Übernehmen.
    fn binary_row(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        services: &Services,
        body: &mut Rect,
        label: &str,
        which: &str,
    ) {
        let mut r = labeled_row(ui, body, label);
        let apply_w = 92.0;
        let browse_w = 34.0;
        let apply_cell = r.cut_right(apply_w);
        r.cut_right(6.0);
        let browse_cell = r.cut_right(browse_w);
        r.cut_right(6.0);
        let field = r;
        let res = if which == "ffmpeg" {
            self.ffmpeg_input.show(ui, "settings.ffmpegPath", field, "im PATH")
        } else {
            self.ffprobe_input.show(ui, "settings.ffprobePath", field, "im PATH")
        };
        if IconButton::new("folder-open").tooltip("Durchsuchen…").show(ui, ("settings.browse", which), browse_cell).clicked {
            services.pick_ffmpeg_binary(which);
        }
        let apply = TextButton::new("Übernehmen").style(TextButtonStyle::Outline);
        if apply.show(ui, ("settings.applyBin", which), apply_cell).clicked || res.submitted {
            self.apply_binary(state, services, which);
        }
    }

    /// Pfad-Override eines Binaries validieren + anwenden.
    fn apply_binary(&mut self, state: &mut AppState, services: &Services, which: &str) {
        let raw = if which == "ffmpeg" {
            self.ffmpeg_input.text.trim().to_string()
        } else {
            self.ffprobe_input.text.trim().to_string()
        };
        let path = if raw.is_empty() { None } else { Some(raw.clone()) };
        if let Some(p) = &path {
            match crate::services::probe_binary_version(p) {
                Some(ver) => self.ffmpeg_status = Some((true, format!("{which}: {ver} — übernommen"))),
                None => {
                    self.ffmpeg_status =
                        Some((false, format!("{which}: konnte nicht gestartet werden — Pfad nicht übernommen")));
                    return;
                }
            }
        } else {
            self.ffmpeg_status = Some((true, format!("{which}: im PATH suchen")));
        }
        if which == "ffmpeg" {
            state.settings.ffmpeg_path = path;
            self.synced_ffmpeg = raw;
        } else {
            state.settings.ffprobe_path = path;
            self.synced_ffprobe = raw;
        }
        state.settings.save();
        crate::services::set_ffmpeg_override(
            state.settings.ffmpeg_path.clone(),
            state.settings.ffprobe_path.clone(),
        );
        services.refresh_ffmpeg_info();
    }

    /// Textfelder mit den Settings abgleichen (nach Öffnen + externer Änderung
    /// durch den Datei-Picker). Tippt der Nutzer, bleibt `settings` unverändert
    /// ⇒ kein Überschreiben des Entwurfs.
    fn sync_inputs(&mut self, state: &AppState) {
        let cache = state.settings.frame_cache_budget_mb.to_string();
        if cache != self.synced_cache {
            self.cache_input.set_text(&cache);
            self.synced_cache = cache;
        }
        let ff = state.settings.ffmpeg_path.clone().unwrap_or_default();
        if ff != self.synced_ffmpeg {
            self.ffmpeg_input.set_text(&ff);
            self.synced_ffmpeg = ff;
        }
        let fp = state.settings.ffprobe_path.clone().unwrap_or_default();
        if fp != self.synced_ffprobe {
            self.ffprobe_input.set_text(&fp);
            self.synced_ffprobe = fp;
        }
        let wp = state.settings.whisper_path.clone().unwrap_or_default();
        if wp != self.synced_whisper {
            self.whisper_input.set_text(&wp);
            self.synced_whisper = wp;
        }
        let wm = state.settings.whisper_model.clone().unwrap_or_default();
        if wm != self.synced_whisper_model {
            self.whisper_model_input.set_text(&wm);
            self.synced_whisper_model = wm;
        }
    }
}

/// Eintrag in der Kategorien-Leiste (Icon + Label, aktiv = Akzent-Hintergrund).
fn cat_row(ui: &mut Ui, id_src: impl std::hash::Hash, row: Rect, icon: &str, label: &str, active: bool) -> Interaction {
    let id = ui.id(id_src);
    let it = ui.interact(id, row);
    if active {
        ui.fill_rounded(row, theme::RADIUS_SM, theme::ACCENT_SOFT);
    } else if it.hovered {
        ui.fill_rounded(row, theme::RADIUS_SM, theme::SURFACE_2);
    }
    let icon_cell = Rect::new(row.x + 8.0, row.y, 16.0, row.h);
    let fg = if active { theme::TEXT_1 } else if it.hovered { theme::TEXT_1 } else { theme::TEXT_2 };
    ui.icon(icon, icon_cell, 16.0, fg);
    let text = Rect::new(row.x + 32.0, row.y, row.w - 36.0, row.h);
    ui.text_left(label, text, fg, FontKind::Sans12Medium);
    if it.hovered {
        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
    }
    it
}

/// Eine Feldzeile in Slider + rechtsbündige Wertanzeige (52 px) teilen.
fn split_value(mut field: Rect) -> (Rect, Rect) {
    let value = field.cut_right(52.0);
    (field, Rect::new(value.x + 8.0, value.y, value.w - 8.0, value.h))
}

/// Index der zur gespeicherten Skalierung nächstliegenden Vorschau-Stufe.
fn nearest_scale_idx(scale: f64) -> usize {
    PREVIEW_SCALES
        .iter()
        .enumerate()
        .min_by(|(_, (a, _)), (_, (b, _))| {
            (a - scale).abs().partial_cmp(&(b - scale).abs()).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}
