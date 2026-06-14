//! Medien-Browser: professionelle Medienverwaltung im Stil des Premiere-
//! Projekt-Panels / Resolve-Media-Pools. Bins (Ordner) mit Breadcrumb-
//! Navigation, Raster- und Listenansicht mit sortierbaren, anpassbaren
//! Metadaten-Spalten, Hover-Scrub, Farbetiketten, Verwendungs-Tracking,
//! Inline-Umbenennen, Mehrfachauswahl (Modifikatoren + Marquee), Drag&Drop
//! zwischen Bins und in die Timeline. Aktionen laufen über Commands.

use crate::core::bin::{MediaLabel, SortKey, ViewMode, COLUMNS, ROOT_BIN_ID};
use crate::core::types::{MediaAsset, MediaKind};
use crate::overlays::context_menu::{MenuEntry, MenuItem};
use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::stores::{MediaStore, RenameTarget};
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::widgets::{select::select, IconButton, TextButton};
use crate::ui::{DragPayload, FontKind, Ui};
use raylib::color::Color;
use raylib::consts::MouseCursor;
use raylib::math::Vector2;

/// Buckets für den Hover-Scrub (quantisierte Scrub-Position → Cache-Schlüssel).
const SCRUB_BUCKETS: u32 = 16;

/// Inline-Umbenennung eines Bins oder Assets.
struct RenameEdit {
    target: RenameTarget,
    input: TextInputState,
    /// false bis zum ersten Render (dann Fokus + Alles-auswählen).
    started: bool,
}

/// Aktives Aufziehen einer Auswahl-Lasso (Bildschirmkoordinaten).
struct Marquee {
    origin: Vector2,
    additive: bool,
}

pub struct MediaBrowserPanel {
    search: TextInputState,
    scroll: ScrollState,
    edit: Option<RenameEdit>,
    /// Anker der Shift-Bereichsauswahl (Asset-ID).
    anchor: Option<String>,
    /// Auswahl-Lasso.
    marquee: Option<Marquee>,
    /// Laufende Hover-Scrub-Anforderungen (asset_id, bucket) — Drosselung.
    scrub_pending: std::collections::HashSet<(String, u32)>,
    /// Spalte, deren Breite gerade gezogen wird (Index in COLUMNS).
    col_drag: Option<usize>,
}

impl Default for MediaBrowserPanel {
    fn default() -> Self {
        MediaBrowserPanel {
            search: TextInputState {
                pad_left: 16.0,
                ..Default::default()
            },
            scroll: ScrollState::default(),
            edit: None,
            anchor: None,
            marquee: None,
            scrub_pending: std::collections::HashSet::new(),
            col_drag: None,
        }
    }
}

// ----------------------------------------------------------------- Formatierung

fn format_duration(sec: f64) -> String {
    let total = sec.round().max(0.0) as i64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn kind_icon(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Video => "film",
        MediaKind::Audio => "music",
        MediaKind::Image => "image",
    }
}

fn label_color(label: MediaLabel) -> Color {
    let (r, g, b) = label.rgb();
    Color::new(r, g, b, 255)
}

/// Proxy-Badge eines Assets: Kurztext + Farbe (None = kein Proxy/kein Job).
/// „P %“ = wird erstellt (gelb), „P!“ = Fehler (rot), „P“ = vorhanden (grün),
/// „P?“ = Proxy-Pfad gesetzt, aber Datei fehlt/veraltet (orange).
fn proxy_badge(media: &MediaStore, asset: &MediaAsset) -> Option<(String, Color)> {
    use crate::stores::ProxyJobStatus;
    match media.proxy_status(&asset.id) {
        Some(ProxyJobStatus::Building(p)) => {
            Some((format!("P {}%", (p * 100.0).round() as u32), theme::WARNING))
        }
        Some(ProxyJobStatus::Failed(_)) => Some(("P!".into(), theme::DANGER)),
        None => {
            asset.proxy_path.as_ref()?;
            if asset.proxy_offline {
                Some(("P?".into(), Color::new(0xf0, 0x97, 0x33, 255)))
            } else {
                Some(("P".into(), theme::SUCCESS))
            }
        }
    }
}

fn fps_label(a: &MediaAsset) -> String {
    match a.info.video.first().map(|v| v.fps) {
        Some(f) if f > 0.0 && f.fract() == 0.0 => format!("{f:.0}"),
        Some(f) if f > 0.0 => format!("{f:.2}"),
        _ => "—".into(),
    }
}

fn resolution_label(a: &MediaAsset) -> String {
    a.info
        .video
        .first()
        .map(|v| format!("{}×{}", v.width, v.height))
        .unwrap_or_else(|| "—".into())
}

fn codec_label(a: &MediaAsset) -> String {
    a.info
        .video
        .first()
        .map(|v| v.codec.clone())
        .or_else(|| a.info.audio.first().map(|a| a.codec.clone()))
        .unwrap_or_else(|| "—".into())
}

fn audio_label(a: &MediaAsset) -> String {
    match a.info.audio.first().map(|s| s.channels) {
        None => "—".into(),
        Some(0) => "—".into(),
        Some(1) => "Mono".into(),
        Some(2) => "Stereo".into(),
        Some(n) => format!("{n} Kanäle"),
    }
}

fn size_label(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{bytes} B")
    }
}

/// Unix-Sekunden → „TT.MM.JJJJ HH:MM“ (UTC, ohne Chrono; civil-from-days).
pub fn format_unix_date(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "—".into();
    }
    let z = (secs / 86400.0).floor() as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    let secs_of_day = (secs as i64).rem_euclid(86400);
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day % 3600) / 60;
    format!("{d:02}.{m:02}.{year:04} {hh:02}:{mm:02}")
}

/// Metadaten-Zelle eines Assets als Text (Listenansicht + Suchindex).
fn metadata_cell(a: &MediaAsset, key: SortKey) -> String {
    match key {
        SortKey::Name => a.name.clone(),
        SortKey::Duration => format_duration(a.info.duration_sec),
        SortKey::Fps => fps_label(a),
        SortKey::Resolution => resolution_label(a),
        SortKey::Codec => codec_label(a),
        SortKey::Audio => audio_label(a),
        SortKey::Size => size_label(a.info.size_bytes),
        SortKey::Path => a.path.clone(),
        SortKey::Date => a.info.recorded_at.map(format_unix_date).unwrap_or_else(|| "—".into()),
    }
}

/// Sortierschlüssel als Zahl (für numerische Sortierungen).
fn sort_number(a: &MediaAsset, key: SortKey) -> f64 {
    match key {
        SortKey::Duration => a.info.duration_sec,
        SortKey::Fps => a.info.video.first().map(|v| v.fps).unwrap_or(0.0),
        SortKey::Resolution => a
            .info
            .video
            .first()
            .map(|v| (v.width as f64) * (v.height as f64))
            .unwrap_or(0.0),
        SortKey::Audio => a.info.audio.first().map(|s| s.channels as f64).unwrap_or(0.0),
        SortKey::Size => a.info.size_bytes as f64,
        SortKey::Date => a.info.recorded_at.unwrap_or(0.0),
        _ => 0.0,
    }
}

fn is_numeric_sort(key: SortKey) -> bool {
    matches!(
        key,
        SortKey::Duration | SortKey::Fps | SortKey::Resolution | SortKey::Audio | SortKey::Size | SortKey::Date
    )
}

/// Assets nach dem aktuellen Sortierschlüssel ordnen (Name als Tie-Breaker).
fn sort_assets(media: &MediaStore, ids: &mut [String], sort: SortKey, desc: bool) {
    ids.sort_by(|a, b| {
        let (Some(aa), Some(ab)) = (media.asset(a), media.asset(b)) else {
            return std::cmp::Ordering::Equal;
        };
        let ord = if is_numeric_sort(sort) {
            sort_number(aa, sort)
                .partial_cmp(&sort_number(ab, sort))
                .unwrap_or(std::cmp::Ordering::Equal)
        } else {
            metadata_cell(aa, sort)
                .to_lowercase()
                .cmp(&metadata_cell(ab, sort).to_lowercase())
        };
        let ord = ord.then_with(|| aa.name.to_lowercase().cmp(&ab.name.to_lowercase()));
        if desc {
            ord.reverse()
        } else {
            ord
        }
    });
}

fn rects_overlap(a: Rect, b: Rect) -> bool {
    a.x < b.right() && a.right() > b.x && a.y < b.bottom() && a.bottom() > b.y
}

// --------------------------------------------------------------- Kontextmenüs

fn asset_context_menu(media: &MediaStore, asset_id: &str) -> Vec<MenuEntry> {
    let arg = serde_json::json!({ "assetId": asset_id });
    let mut entries = vec![
        MenuEntry::Item(
            MenuItem::command("media.openInSource")
                .with_icon("monitor-play")
                .with_args(arg.clone()),
        ),
        MenuEntry::Item(MenuItem::command("media.addSelectionToTimeline").with_icon("list-video")),
        MenuEntry::Item(
            MenuItem::command("media.showInTimeline")
                .with_icon("focus")
                .with_args(arg.clone()),
        ),
        MenuEntry::Separator,
        MenuEntry::Item(
            MenuItem::command("media.renameAsset")
                .with_icon("type")
                .with_args(arg.clone()),
        ),
        MenuEntry::Submenu {
            label: "Farbetikett".into(),
            icon: Some("palette"),
            items: label_menu_items(),
        },
        MenuEntry::Submenu {
            label: "In Ordner verschieben".into(),
            icon: Some("move"),
            items: move_to_menu_items(media),
        },
        MenuEntry::Separator,
        MenuEntry::Item(
            MenuItem::command("media.revealInFileManager")
                .with_icon("folder-search")
                .with_args(arg.clone()),
        ),
        MenuEntry::Item(
            MenuItem::command("media.relinkAsset")
                .with_icon("link-2")
                .with_args(arg),
        ),
        MenuEntry::Separator,
        // Proxys wirken auf die gesamte Auswahl (kein assetId-Argument).
        MenuEntry::Item(MenuItem::command("media.createProxies").with_icon("gauge")),
        MenuEntry::Item(MenuItem::command("media.deleteProxies").with_icon("image-minus")),
        MenuEntry::Item(
            MenuItem::command("window.openPanel.info")
                .with_label("Eigenschaften")
                .with_icon("info"),
        ),
        MenuEntry::Separator,
        MenuEntry::Item(
            MenuItem::command("media.removeSelected")
                .with_icon("trash-2")
                .with_danger(),
        ),
    ];
    // Multicam-Quelle (nur bei Mehrfachauswahl ≥ 2 Assets) — direkt unter
    // „Auswahl in Timeline einfügen".
    if media.selected_asset_ids.len() >= 2 {
        entries.insert(
            2,
            MenuEntry::Submenu {
                label: "Multicam-Quelle erstellen".into(),
                icon: Some("layers"),
                items: multicam_menu_items(),
            },
        );
    }
    entries.shrink_to_fit();
    entries
}

/// Untermenü „Multicam-Quelle erstellen": ein Eintrag je Sync-Verfahren.
fn multicam_menu_items() -> Vec<MenuEntry> {
    vec![
        MenuEntry::Item(
            MenuItem::command("media.createMulticamSource")
                .with_label("Per Audio-Analyse (empfohlen)")
                .with_icon("audio-waveform")
                .with_args(serde_json::json!({ "method": "audio" })),
        ),
        MenuEntry::Item(
            MenuItem::command("media.createMulticamSource")
                .with_label("Per Timecode")
                .with_icon("clock")
                .with_args(serde_json::json!({ "method": "timecode" })),
        ),
        MenuEntry::Item(
            MenuItem::command("media.createMulticamSource")
                .with_label("Per gemeinsamem Startpunkt")
                .with_icon("flag")
                .with_args(serde_json::json!({ "method": "start" })),
        ),
    ]
}

fn label_menu_items() -> Vec<MenuEntry> {
    let mut items: Vec<MenuEntry> = MediaLabel::ALL
        .into_iter()
        .map(|c| MenuEntry::Item(MenuItem::command(&format!("media.setLabel.{}", c.key()))))
        .collect();
    items.push(MenuEntry::Separator);
    items.push(MenuEntry::Item(
        MenuItem::command("media.clearLabel").with_icon("ban"),
    ));
    items
}

fn move_to_menu_items(media: &MediaStore) -> Vec<MenuEntry> {
    let mut items = vec![MenuEntry::Item(
        MenuItem::command("media.moveToBin")
            .with_label("Projekt (Wurzel)")
            .with_icon("folder-open")
            .with_args(serde_json::json!({ "binId": ROOT_BIN_ID })),
    )];
    // Alle Bins nach Pfad sortiert anbieten.
    let mut bins: Vec<&crate::core::bin::Bin> = media.bins.iter().collect();
    bins.sort_by(|a, b| {
        media
            .bin_path_label(&a.id)
            .to_lowercase()
            .cmp(&media.bin_path_label(&b.id).to_lowercase())
    });
    for b in bins {
        // Wurzel-Präfix „Projekt / “ aus dem Pfad weglassen.
        let path = media.bin_path_label(&b.id);
        let label = path.strip_prefix("Projekt / ").unwrap_or(&path).to_string();
        items.push(MenuEntry::Item(
            MenuItem::command("media.moveToBin")
                .with_label(&label)
                .with_icon("folder-open")
                .with_args(serde_json::json!({ "binId": b.id })),
        ));
    }
    items
}

fn bin_context_menu(bin_id: &str) -> Vec<MenuEntry> {
    let arg = serde_json::json!({ "binId": bin_id });
    vec![
        MenuEntry::Item(
            MenuItem::command("media.openBin")
                .with_label("Öffnen")
                .with_icon("folder-open")
                .with_args(arg.clone()),
        ),
        MenuEntry::Item(
            MenuItem::command("media.createBin")
                .with_label("Neuer Unterordner")
                .with_icon("plus")
                .with_args(arg.clone()),
        ),
        MenuEntry::Item(
            MenuItem::command("media.renameBin")
                .with_icon("type")
                .with_args(arg.clone()),
        ),
        MenuEntry::Separator,
        MenuEntry::Item(
            MenuItem::command("media.deleteBin")
                .with_icon("trash-2")
                .with_danger()
                .with_args(arg),
        ),
    ]
}

/// Kontextmenü einer Sequenz im Medien-Browser: Öffnen, Umbenennen,
/// Duplizieren, Einstellungen, Löschen (nur wenn mehr als eine Sequenz).
fn sequence_browser_menu(seq_id: &str, is_last: bool) -> Vec<MenuEntry> {
    let arg = serde_json::json!({ "sequenceId": seq_id });
    let mut items = vec![
        MenuEntry::Item(
            MenuItem::command("sequence.open")
                .with_label("Öffnen")
                .with_icon("clapperboard")
                .with_args(arg.clone()),
        ),
        MenuEntry::Item(
            MenuItem::command("sequence.rename")
                .with_icon("type")
                .with_args(arg.clone()),
        ),
        MenuEntry::Item(
            MenuItem::command("sequence.duplicate")
                .with_icon("copy")
                .with_args(arg.clone()),
        ),
        MenuEntry::Item(MenuItem::command("sequence.settings").with_icon("sliders-horizontal")),
    ];
    if !is_last {
        items.push(MenuEntry::Separator);
        items.push(MenuEntry::Item(
            MenuItem::command("sequence.delete")
                .with_icon("trash-2")
                .with_danger()
                .with_args(arg),
        ));
    }
    items
}

fn background_context_menu(current_bin: &str) -> Vec<MenuEntry> {
    vec![
        MenuEntry::Item(
            MenuItem::command("media.createBin")
                .with_icon("plus")
                .with_args(serde_json::json!({ "binId": current_bin })),
        ),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem::command("media.import").with_icon("import")),
        MenuEntry::Item(MenuItem::command("media.createProxiesAll").with_icon("gauge")),
        MenuEntry::Item(MenuItem::command("proxy.settings").with_icon("sliders-horizontal")),
    ]
}

// ------------------------------------------------------------------ Panel-Impl

impl Panel for MediaBrowserPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);
        // Anstehende Umbenennung vom Command übernehmen.
        self.take_rename_request(app);

        let mut area = rect;
        let toolbar = area.cut_top(36.0);
        self.render_toolbar(ui, app, toolbar);

        let query = self.search.text.trim().to_lowercase();
        let searching = !query.is_empty();

        if !searching {
            let crumbs = area.cut_top(28.0);
            self.render_breadcrumb(ui, app, crumbs);
        }

        // Inhalt zusammenstellen.
        let current = app.media.current_bin().to_string();
        let (folders, assets) = if searching {
            (Vec::new(), self.search_results(app, &query))
        } else {
            let folders: Vec<String> = app
                .media
                .bin_children(&current)
                .iter()
                .map(|b| b.id.clone())
                .collect();
            let mut assets: Vec<String> =
                app.media.assets_in_bin(&current).iter().map(|a| a.id.clone()).collect();
            sort_assets(&app.media, &mut assets, app.media.view.sort, app.media.view.sort_desc);
            (folders, assets)
        };
        // Sequenzen erscheinen als eigene Browser-Einträge (eigenes Icon,
        // Doppelklick öffnet, Drag = Nesting).
        let mut sequences: Vec<String> = if searching {
            app.timeline
                .iter()
                .filter(|s| s.name.to_lowercase().contains(&query))
                .map(|s| s.id.clone())
                .collect()
        } else {
            app.timeline
                .iter()
                .filter(|s| s.bin_id == current)
                .map(|s| s.id.clone())
                .collect()
        };
        sequences.sort_by(|a, b| {
            let na = app.timeline.name_of(a).unwrap_or("").to_lowercase();
            let nb = app.timeline.name_of(b).unwrap_or("").to_lowercase();
            na.cmp(&nb)
        });

        if app.media.assets.is_empty() && app.media.bins.is_empty() && sequences.is_empty() && !searching {
            let msg = if app.media.importing {
                "Import läuft …"
            } else {
                "Noch keine Medien importiert"
            };
            ui.text_centered(msg, area, theme::TEXT_3, FontKind::Sans12);
            return;
        }
        if folders.is_empty() && assets.is_empty() && sequences.is_empty() {
            let msg = if searching {
                "Keine Treffer"
            } else {
                "Dieser Ordner ist leer"
            };
            ui.text_centered(msg, area, theme::TEXT_3, FontKind::Sans12);
            // Rechtsklick auf leere Fläche bietet trotzdem das Hintergrundmenü.
            self.background_interactions(ui, app, area, searching);
            return;
        }

        match app.media.view.mode {
            ViewMode::Grid => self.grid_view(ui, app, area, &folders, &sequences, &assets, searching),
            ViewMode::List => self.list_view(ui, app, area, &folders, &sequences, &assets, searching),
        }

        self.background_interactions(ui, app, area, searching);
    }
}

impl MediaBrowserPanel {
    // ------------------------------------------------------------- Toolbar

    fn render_toolbar(&mut self, ui: &mut Ui, app: &mut AppState, toolbar: Rect) {
        ui.hline(toolbar.x, toolbar.bottom() - 1.0, toolbar.w, theme::LINE);
        let mut tb = toolbar.inset_xy(8.0, 0.0);

        let import_btn = TextButton::new("Importieren").icon("import");
        let btn_w = import_btn.measure(ui);
        let import_rect = Rect::new(tb.cut_left(btn_w).x, tb.y + 6.0, btn_w, 24.0);
        if import_btn.show(ui, "media.import.btn", import_rect).clicked {
            ui.run_command("media.import");
        }
        tb.cut_left(6.0);

        // Neuer Ordner.
        let folder_rect = Rect::new(tb.cut_left(24.0).x, tb.y + 6.0, 24.0, 24.0);
        if IconButton::new("plus")
            .size(15.0)
            .tooltip("Neuen Ordner anlegen")
            .show(ui, "media.newbin.btn", folder_rect)
            .clicked
        {
            ui.run_command("media.createBin");
        }
        tb.cut_left(8.0);

        // Ansicht-Umschalter rechts.
        let list_rect = Rect::new(tb.right() - 24.0, tb.y + 6.0, 24.0, 24.0);
        let grid_rect = Rect::new(tb.right() - 48.0, tb.y + 6.0, 24.0, 24.0);
        tb.cut_right(48.0);
        if IconButton::new("layout-grid")
            .size(14.0)
            .active(app.media.view.mode == ViewMode::Grid)
            .tooltip("Rasteransicht")
            .show(ui, "media.view.grid", grid_rect)
            .clicked
        {
            app.media.view.mode = ViewMode::Grid;
        }
        if IconButton::new("list")
            .size(14.0)
            .active(app.media.view.mode == ViewMode::List)
            .tooltip("Listenansicht")
            .show(ui, "media.view.list", list_rect)
            .clicked
        {
            app.media.view.mode = ViewMode::List;
        }
        tb.cut_right(8.0);

        // Sortierung (nur Rasteransicht — in der Liste sortieren die Köpfe).
        if app.media.view.mode == ViewMode::Grid {
            let labels = [
                "Name", "Dauer", "Framerate", "Auflösung", "Codec", "Audio", "Größe", "Pfad",
                "Aufnahme",
            ];
            let keys = [
                SortKey::Name,
                SortKey::Duration,
                SortKey::Fps,
                SortKey::Resolution,
                SortKey::Codec,
                SortKey::Audio,
                SortKey::Size,
                SortKey::Path,
                SortKey::Date,
            ];
            let cur = keys.iter().position(|k| *k == app.media.view.sort).unwrap_or(0);
            let sel_w = 116.0;
            let sel_rect = Rect::new(tb.right() - sel_w, tb.y + 6.0, sel_w, 24.0);
            tb.cut_right(sel_w + 6.0);
            if let Some(i) = select(ui, "media.sort", sel_rect, &labels, cur) {
                app.media.view.sort = keys[i];
            }
            // Sortierrichtung.
            let dir_rect = Rect::new(tb.right() - 24.0, tb.y + 6.0, 24.0, 24.0);
            tb.cut_right(28.0);
            let icon = if app.media.view.sort_desc { "chevron-down" } else { "chevron-up" };
            if IconButton::new(icon)
                .size(14.0)
                .tooltip("Sortierrichtung")
                .show(ui, "media.sort.dir", dir_rect)
                .clicked
            {
                app.media.view.sort_desc = !app.media.view.sort_desc;
            }
        }

        // Suchfeld füllt den Rest.
        let search_rect = Rect::new(tb.x, tb.y + 6.0, (tb.w - 8.0).max(80.0), 24.0);
        self.search.show(ui, "media.search", search_rect, "Suchen (alle Ordner)");
        ui.icon(
            "search",
            Rect::new(search_rect.x + 4.0, search_rect.y + 5.0, 14.0, 14.0),
            14.0,
            theme::TEXT_3,
        );
    }

    // ----------------------------------------------------------- Breadcrumb

    fn render_breadcrumb(&mut self, ui: &mut Ui, app: &mut AppState, crumbs: Rect) {
        ui.fill(crumbs, theme::SURFACE_1);
        ui.hline(crumbs.x, crumbs.bottom() - 1.0, crumbs.w, theme::LINE);
        let path = app.media.bin_path(app.media.current_bin());
        let mut x = crumbs.x + 8.0;
        let y = crumbs.y;
        let last = path.len().saturating_sub(1);
        for (i, (id, name)) in path.iter().enumerate() {
            if i > 0 {
                let sep = Rect::new(x, y, 14.0, crumbs.h);
                ui.icon("chevron-right", sep, 12.0, theme::TEXT_3);
                x += 14.0;
            }
            let is_last = i == last;
            let w = ui.font(FontKind::Sans12).width(name).min(crumbs.right() - x - 8.0).max(8.0);
            let seg = Rect::new(x, y, w + 8.0, crumbs.h);
            let id_w = ui.id(("media.crumb", id));
            let it = ui.interact(id_w, seg);
            // Drop-Ziel: Assets/Bins auf einen Pfad-Vorfahren ziehen.
            self.handle_bin_drop(ui, app, seg, id);
            let hovered = it.hovered && !is_last;
            let color = if is_last {
                theme::TEXT_1
            } else if hovered {
                theme::ACCENT
            } else {
                theme::TEXT_2
            };
            ui.text_left(name, Rect::new(seg.x + 4.0, y, w + 4.0, crumbs.h), color, FontKind::Sans12);
            if hovered {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                if it.clicked {
                    app.media.set_current_bin(id);
                }
            }
            x = seg.right();
            if x > crumbs.right() - 8.0 {
                break;
            }
        }
    }

    // ----------------------------------------------------------- Rasteransicht

    #[allow(clippy::too_many_arguments)]
    fn grid_view(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        area: Rect,
        folders: &[String],
        sequences: &[String],
        assets: &[String],
        searching: bool,
    ) {
        let pad = 8.0;
        let gap = 8.0;
        let min_w = app.media.view.tile_w.max(96.0);
        let inner_w = area.w - pad * 2.0 - theme::SCROLLBAR_W;
        let cols = ((inner_w + gap) / (min_w + gap)).floor().max(1.0) as usize;
        let tile_w = (inner_w - gap * (cols as f32 - 1.0)) / cols as f32;
        let folder_h = 34.0;
        let thumb_h = tile_w * 9.0 / 16.0;
        let tile_h = thumb_h + (if searching { 40.0 } else { 25.0 });

        // Ordner-Reihen (jede volle Breite gerastert) + Sequenz-Reihen +
        // Asset-Reihen.
        let folder_rows = folders.len().div_ceil(cols);
        let seq_rows = sequences.len().div_ceil(cols);
        let asset_rows = assets.len().div_ceil(cols);
        let mut content_h = pad;
        if !folders.is_empty() {
            content_h += folder_rows as f32 * (folder_h + gap);
        }
        if !sequences.is_empty() {
            content_h += seq_rows as f32 * (folder_h + gap);
        }
        content_h += asset_rows as f32 * (tile_h + gap) + pad;

        let view = self.scroll.begin(ui, area, 0.0, content_h);
        let mut y = view.origin_y + pad;
        let mut placed: Vec<(String, Rect)> = Vec::new();

        // Ordner.
        for (i, bin_id) in folders.iter().enumerate() {
            let col = i % cols;
            if col == 0 && i > 0 {
                y += folder_h + gap;
            }
            let tile = Rect::new(view.viewport.x + pad + col as f32 * (tile_w + gap), y, tile_w, folder_h);
            if tile.bottom() >= view.viewport.y && tile.y <= view.viewport.bottom() {
                self.folder_tile(ui, app, tile, bin_id);
            }
        }
        if !folders.is_empty() {
            y += folder_h + gap;
        }

        // Sequenzen (kompakte Reihen wie Ordner, eigenes Icon).
        for (i, seq_id) in sequences.iter().enumerate() {
            let col = i % cols;
            if col == 0 && i > 0 {
                y += folder_h + gap;
            }
            let tile = Rect::new(view.viewport.x + pad + col as f32 * (tile_w + gap), y, tile_w, folder_h);
            if tile.bottom() >= view.viewport.y && tile.y <= view.viewport.bottom() {
                self.sequence_tile(ui, app, tile, seq_id);
            }
        }
        if !sequences.is_empty() {
            y += folder_h + gap;
        }

        // Assets.
        for (i, asset_id) in assets.iter().enumerate() {
            let col = i % cols;
            if col == 0 && i > 0 {
                y += tile_h + gap;
            }
            let tile = Rect::new(view.viewport.x + pad + col as f32 * (tile_w + gap), y, tile_w, tile_h);
            placed.push((asset_id.clone(), tile));
            if tile.bottom() >= view.viewport.y && tile.y <= view.viewport.bottom() {
                self.asset_tile(ui, app, ui_tile_parts(tile, thumb_h, searching), asset_id, assets);
            }
        }

        self.run_marquee(ui, app, area, &placed);
        self.scroll.end(ui, area, 0.0, content_h);
    }

    fn folder_tile(&mut self, ui: &mut Ui, app: &mut AppState, tile: Rect, bin_id: &str) {
        let selected = app.media.current_bin() == bin_id;
        let count = app.media.assets_in_bin(bin_id).len() + app.media.bin_children(bin_id).len();
        let name = app.media.bin_name(bin_id);

        let id = ui.id(("media.folder", bin_id));
        let it = ui.interact(id, tile);
        ui.fill_rounded(tile, theme::RADIUS_SM, theme::SURFACE_2);
        let border = if it.hovered { theme::LINE_STRONG } else { theme::LINE };
        ui.stroke_rounded(tile, theme::RADIUS_SM, 1.0, border);
        if selected {
            ui.stroke_rounded(tile, theme::RADIUS_SM, 2.0, theme::ACCENT);
        }

        let mut inner = tile.inset_xy(8.0, 0.0);
        let icon_cell = inner.cut_left(18.0);
        ui.icon("folder-open", icon_cell, 16.0, theme::ACCENT);
        inner.cut_left(6.0);
        let badge_w = ui.font(FontKind::Mono12).width(&count.to_string()) + 4.0;
        let badge = inner.cut_right(badge_w);
        ui.text_left(&count.to_string(), badge, theme::TEXT_3, FontKind::Mono12);

        // Inline-Umbenennen oder Name.
        if self.is_renaming_bin(bin_id) {
            self.render_rename_input(ui, app, inner);
        } else {
            let display = ui.font(FontKind::Sans12).ellipsize(&name, inner.w);
            ui.text_left(&display, inner, theme::TEXT_1, FontKind::Sans12);
        }

        // Interaktionen.
        self.handle_bin_drop(ui, app, tile, bin_id);
        if it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
        }
        if it.hovered && ui.input.left_pressed && !self.is_renaming_bin(bin_id) {
            app.app.focused_panel = "media".into();
            app.media.selected_asset_ids.clear();
            app.media.set_current_bin(bin_id);
            // Ordner als Drag-Quelle (zwischen Bins verschieben).
            ui.start_drag(DragPayload::Bins(vec![bin_id.to_string()]));
        }
        if it.double_clicked {
            app.media.set_current_bin(bin_id);
        }
        if it.right_clicked {
            app.app.focused_panel = "media".into();
            app.context_menu
                .show(ui.input.mouse.x, ui.input.mouse.y, bin_context_menu(bin_id));
        }
    }

    /// Kompakte Sequenz-Kachel (Rasteransicht): eigenes Icon, Doppelklick
    /// öffnet die Sequenz im Tab, Drag = Nesting, Rechtsklick = Menü.
    fn sequence_tile(&mut self, ui: &mut Ui, app: &mut AppState, tile: Rect, seq_id: &str) {
        let Some(name) = app.timeline.name_of(seq_id).map(|s| s.to_string()) else {
            return;
        };
        let active = app.timeline.active_id() == seq_id;
        let id = ui.id(("media.seq", seq_id));
        let it = ui.interact(id, tile);
        ui.fill_rounded(tile, theme::RADIUS_SM, theme::SURFACE_2);
        let border = if it.hovered { theme::LINE_STRONG } else { theme::LINE };
        ui.stroke_rounded(tile, theme::RADIUS_SM, 1.0, border);
        if active {
            ui.stroke_rounded(tile, theme::RADIUS_SM, 2.0, theme::ACCENT);
        }
        let mut inner = tile.inset_xy(8.0, 0.0);
        let icon_cell = inner.cut_left(18.0);
        // Multicam-Quellen tragen ein Raster-Icon statt der Klappe.
        let icon = if app.timeline.is_multicam_source(seq_id) {
            "layout-grid"
        } else {
            "clapperboard"
        };
        ui.icon(icon, icon_cell, 16.0, theme::ACCENT);
        inner.cut_left(6.0);
        let display = ui.font(FontKind::Sans12).ellipsize(&name, inner.w);
        ui.text_left(&display, inner, theme::TEXT_1, FontKind::Sans12);

        if it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
        }
        if it.hovered && ui.input.left_pressed {
            app.app.focused_panel = "media".into();
            // Sequenz als Drag-Quelle (Nesting in eine andere Timeline).
            ui.start_drag(DragPayload::Sequences(vec![seq_id.to_string()]));
        }
        if it.double_clicked {
            ui.run_command_with("sequence.open", serde_json::json!({ "sequenceId": seq_id }));
        }
        if it.right_clicked {
            app.app.focused_panel = "media".into();
            let last = app.timeline.len() <= 1;
            app.context_menu
                .show(ui.input.mouse.x, ui.input.mouse.y, sequence_browser_menu(seq_id, last));
        }
    }

    fn asset_tile(&mut self, ui: &mut Ui, app: &mut AppState, parts: TileParts, asset_id: &str, ordered: &[String]) {
        let Some(asset) = app.media.asset(asset_id) else { return };
        let selected = app.media.selected_asset_ids.iter().any(|id| id == asset_id);
        let name = asset.name.clone();
        let kind = asset.kind;
        let duration = asset.info.duration_sec;
        let thumb = asset.thumbnail_path.clone();
        // Hover-Scrub im Proxy-Modus aus der Proxy-Datei (schnelles Seeken).
        let path = asset.decode_path(app.media.use_proxies).to_string();
        let offline = asset.offline;
        let label = asset.label;
        let proxy = proxy_badge(&app.media, asset);
        let usage = app.timeline.asset_usage_count(asset_id);
        let bin_label = if parts.show_path {
            Some(app.media.bin_path_label(app.media.effective_bin(asset)))
        } else {
            None
        };

        let TileParts { tile, thumb_rect, .. } = parts;
        let id = ui.id(("media.tile", asset_id));
        let it = ui.interact(id, tile);

        ui.fill_rounded(tile, theme::RADIUS_SM, theme::SURFACE_2);
        // Thumbnail bzw. Hover-Scrub.
        let hovered_scrub = it.hovered && kind == MediaKind::Video && duration > 0.05 && thumb.is_some();
        let scrub_path = if hovered_scrub {
            self.scrub_frame(ui, app, asset_id, &path, duration, thumb_rect)
        } else {
            None
        };
        if let Some(p) = scrub_path.as_ref().or(thumb.as_ref()) {
            ui.push_clip(thumb_rect);
            ui.draw_texture_cover(p, thumb_rect);
            ui.pop_clip();
        } else if kind == MediaKind::Audio {
            ui.icon("music", thumb_rect, 24.0, theme::TEXT_3);
        }

        // Auswahlring/Rahmen.
        if selected {
            ui.stroke_rounded(tile, theme::RADIUS_SM, 2.0, theme::ACCENT);
        } else {
            let border = if it.hovered { theme::LINE_STRONG } else { theme::LINE };
            ui.stroke_rounded(tile, theme::RADIUS_SM, 1.0, border);
        }

        // Art-Icon oben links.
        let badge = Rect::new(thumb_rect.x + 4.0, thumb_rect.y + 4.0, 20.0, 20.0);
        ui.fill_rounded(badge, theme::RADIUS_XS, theme::with_alpha(theme::BLACK, 153));
        ui.icon(kind_icon(kind), badge, 12.0, theme::TEXT_1);

        // Farbetikett (Punkt oben links neben dem Art-Icon).
        if let Some(label) = label {
            let dot = Rect::new(thumb_rect.x + 28.0, thumb_rect.y + 6.0, 14.0, 14.0);
            ui.fill_rounded(dot, 7.0, label_color(label));
            ui.stroke_rounded(dot, 7.0, 1.0, theme::with_alpha(theme::BLACK, 120));
        }

        // Verwendungs-Badge (oben rechts), sofern verwendet.
        if usage > 0 && !offline {
            let txt = format!("{usage}×");
            let w = ui.font(FontKind::Mono12).width(&txt) + 8.0;
            let used = Rect::new(thumb_rect.right() - w - 4.0, thumb_rect.y + 4.0, w, 16.0);
            ui.fill_rounded(used, theme::RADIUS_XS, theme::with_alpha(theme::ACCENT, 200));
            ui.text_centered(&txt, used, theme::WHITE, FontKind::Mono12);
        }
        // Offline-Badge.
        if offline {
            let warn = Rect::new(thumb_rect.right() - 24.0, thumb_rect.y + 4.0, 20.0, 20.0);
            ui.fill_rounded(warn, theme::RADIUS_XS, theme::with_alpha(theme::BLACK, 153));
            ui.icon("triangle-alert", warn, 12.0, theme::DANGER);
            let warn_id = ui.id(("media.offline", asset_id));
            if ui.mouse_in(warn) {
                ui.set_hot(warn_id);
            }
            ui.tooltip(warn_id, warn, "Medium offline — Quelldatei nicht gefunden");
        }

        // Dauer-Badge.
        if kind != MediaKind::Image {
            let lbl = format_duration(duration);
            let w = ui.font(FontKind::Mono12).width(&lbl) + 8.0;
            let dur = Rect::new(thumb_rect.right() - w - 4.0, thumb_rect.bottom() - 20.0, w, 16.0);
            ui.fill_rounded(dur, theme::RADIUS_XS, theme::with_alpha(theme::BLACK, 179));
            ui.text_centered(&lbl, dur, theme::TEXT_1, FontKind::Mono12);
        }
        // Proxy-Badge unten links (Premiere-Pendant: „PR“-Markierung).
        if let Some((lbl, color)) = &proxy {
            let w = ui.font(FontKind::Mono12).width(lbl) + 8.0;
            let chip = Rect::new(thumb_rect.x + 4.0, thumb_rect.bottom() - 20.0, w, 16.0);
            ui.fill_rounded(chip, theme::RADIUS_XS, theme::with_alpha(theme::BLACK, 179));
            ui.text_centered(lbl, chip, *color, FontKind::Mono12);
        }

        // Namenszeile (oder Inline-Umbenennen).
        let name_rect = Rect::new(tile.x + 6.0, thumb_rect.bottom() + 1.0, tile.w - 12.0, 22.0);
        if self.is_renaming_asset(asset_id) {
            self.render_rename_input(ui, app, name_rect);
        } else {
            let display = ui.font(FontKind::Sans12).ellipsize(&name, name_rect.w);
            ui.text_left(&display, name_rect, theme::TEXT_1, FontKind::Sans12);
        }
        if let Some(bin_label) = bin_label {
            let p = Rect::new(tile.x + 6.0, name_rect.bottom(), tile.w - 12.0, 16.0);
            let disp = ui.font(FontKind::Sans12).ellipsize(&bin_label, p.w);
            ui.text_left(&disp, p, theme::TEXT_3, FontKind::Sans12);
        }

        self.asset_interactions(ui, app, it, asset_id, ordered);
    }

    // ------------------------------------------------------------- Listenansicht

    #[allow(clippy::too_many_arguments)]
    fn list_view(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        area: Rect,
        folders: &[String],
        sequences: &[String],
        assets: &[String],
        searching: bool,
    ) {
        let row_h = 28.0;
        let header_h = 28.0;
        let name_w = 220.0f32;
        let meta_w: f32 = app.media.view.col_widths.iter().sum();
        let content_w = name_w + meta_w + 24.0;
        let content_h =
            header_h + (folders.len() + sequences.len() + assets.len()) as f32 * row_h;

        let view = self.scroll.begin(ui, area, content_w, content_h);
        let x0 = view.origin_x;
        let mut placed: Vec<(String, Rect)> = Vec::new();
        let mut y = view.origin_y + header_h;

        // Ordner-Reihen.
        for bin_id in folders {
            let row = Rect::new(view.viewport.x, y, view.viewport.w, row_h);
            if row.bottom() >= view.viewport.y && row.y <= view.viewport.bottom() {
                self.folder_row(ui, app, row, x0, name_w, bin_id);
            }
            y += row_h;
        }
        // Sequenz-Reihen.
        for seq_id in sequences {
            let row = Rect::new(view.viewport.x, y, view.viewport.w, row_h);
            if row.bottom() >= view.viewport.y && row.y <= view.viewport.bottom() {
                self.sequence_row(ui, app, row, x0, seq_id);
            }
            y += row_h;
        }
        // Asset-Reihen.
        for asset_id in assets {
            let row = Rect::new(view.viewport.x, y, view.viewport.w, row_h);
            placed.push((asset_id.clone(), row));
            if row.bottom() >= view.viewport.y && row.y <= view.viewport.bottom() {
                self.asset_row(ui, app, row, x0, name_w, asset_id, assets, searching);
            }
            y += row_h;
        }

        // Marquee-Start nur unterhalb des (zuletzt gezeichneten) Headers.
        let mq_area = Rect::new(area.x, area.y + header_h, area.w, (area.h - header_h).max(0.0));
        self.run_marquee(ui, app, mq_area, &placed);

        // Sticky Header (zuletzt, vertikal fixiert, horizontal mitlaufend).
        let header = Rect::new(view.viewport.x, view.viewport.y, view.viewport.w, header_h);
        self.list_header(ui, app, header, x0, name_w);

        self.scroll.end(ui, area, content_w, content_h);
    }

    fn list_header(&mut self, ui: &mut Ui, app: &mut AppState, header: Rect, x0: f32, name_w: f32) {
        ui.fill(header, theme::SURFACE_1);
        ui.hline(header.x, header.bottom() - 1.0, header.w, theme::LINE);

        // Name-Spalte (klickbar, fix breit).
        let name_rect = Rect::new(x0 + 28.0, header.y, name_w - 28.0, header.h);
        self.header_cell(ui, app, name_rect, "Name", SortKey::Name);

        let mut x = x0 + name_w;
        let widths = app.media.view.col_widths.clone();
        for (i, def) in COLUMNS.iter().enumerate() {
            let w = widths.get(i).copied().unwrap_or(def.default_w);
            let cell = Rect::new(x, header.y, w, header.h);
            self.header_cell(ui, app, cell, def.label, def.key);
            // Resize-Handle an der rechten Kante.
            let handle = Rect::new(x + w - 3.0, header.y, 6.0, header.h);
            let hid = ui.id(("media.colresize", i));
            let hit = ui.interact(hid, handle);
            if hit.hovered || self.col_drag == Some(i) {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
                ui.fill(Rect::new(x + w - 1.0, header.y + 4.0, 1.0, header.h - 8.0), theme::LINE_STRONG);
            }
            if hit.hovered && ui.input.left_pressed {
                self.col_drag = Some(i);
            }
            x += w;
        }
        // Spalten-Resize fortschreiben.
        if let Some(i) = self.col_drag {
            if ui.input.left_down {
                // Linke Kante der Spalte i berechnen.
                let mut left = x0 + name_w;
                for w in widths.iter().take(i) {
                    left += *w;
                }
                let new_w = (ui.input.mouse.x - left).clamp(48.0, 640.0);
                if let Some(slot) = app.media.view.col_widths.get_mut(i) {
                    *slot = new_w;
                }
            } else {
                self.col_drag = None;
            }
        }
    }

    fn header_cell(&mut self, ui: &mut Ui, app: &mut AppState, cell: Rect, label: &str, key: SortKey) {
        let active = app.media.view.sort == key;
        let id = ui.id(("media.header", label));
        let it = ui.interact(id, cell);
        if it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
        }
        if it.clicked && self.col_drag.is_none() {
            if active {
                app.media.view.sort_desc = !app.media.view.sort_desc;
            } else {
                app.media.view.sort = key;
                app.media.view.sort_desc = false;
            }
        }
        let color = if active { theme::TEXT_1 } else { theme::TEXT_3 };
        ui.text_left(label, Rect::new(cell.x + 2.0, cell.y, cell.w - 16.0, cell.h), color, FontKind::Sans12);
        if active {
            let arrow = if app.media.view.sort_desc { "chevron-down" } else { "chevron-up" };
            ui.icon(arrow, Rect::new(cell.right() - 14.0, cell.y + 7.0, 12.0, 12.0), 12.0, theme::ACCENT);
        }
    }

    fn folder_row(&mut self, ui: &mut Ui, app: &mut AppState, row: Rect, x0: f32, name_w: f32, bin_id: &str) {
        let selected = app.media.current_bin() == bin_id;
        let name = app.media.bin_name(bin_id);
        let count = app.media.assets_in_bin(bin_id).len() + app.media.bin_children(bin_id).len();

        let id = ui.id(("media.frow", bin_id));
        let it = ui.interact(id, row);
        if selected {
            ui.fill(row, theme::ACCENT_SOFT);
        } else if it.hovered {
            ui.fill(row, theme::SURFACE_2);
        }
        let icon_rect = Rect::new(x0 + 8.0, row.y + 7.0, 14.0, 14.0);
        ui.icon("folder-open", icon_rect, 14.0, theme::ACCENT);
        let name_rect = Rect::new(x0 + 28.0, row.y, name_w - 60.0, row.h);
        if self.is_renaming_bin(bin_id) {
            self.render_rename_input(ui, app, name_rect.inset_xy(0.0, 3.0));
        } else {
            let disp = ui.font(FontKind::Sans12).ellipsize(&name, name_rect.w);
            ui.text_left(&disp, name_rect, theme::TEXT_1, FontKind::Sans12);
        }
        ui.text_left(
            &format!("{count} Elemente"),
            Rect::new(x0 + name_w, row.y, 120.0, row.h),
            theme::TEXT_3,
            FontKind::Sans12,
        );

        self.handle_bin_drop(ui, app, row, bin_id);
        if it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
        }
        if it.hovered && ui.input.left_pressed && !self.is_renaming_bin(bin_id) {
            app.app.focused_panel = "media".into();
            app.media.selected_asset_ids.clear();
            app.media.set_current_bin(bin_id);
            ui.start_drag(DragPayload::Bins(vec![bin_id.to_string()]));
        }
        if it.double_clicked {
            app.media.set_current_bin(bin_id);
        }
        if it.right_clicked {
            app.app.focused_panel = "media".into();
            app.context_menu
                .show(ui.input.mouse.x, ui.input.mouse.y, bin_context_menu(bin_id));
        }
    }

    /// Sequenz-Zeile (Listenansicht): Icon, Name, Doppelklick öffnet, Drag =
    /// Nesting, Rechtsklick = Menü.
    fn sequence_row(&mut self, ui: &mut Ui, app: &mut AppState, row: Rect, x0: f32, seq_id: &str) {
        let Some(name) = app.timeline.name_of(seq_id).map(|s| s.to_string()) else {
            return;
        };
        let active = app.timeline.active_id() == seq_id;
        let id = ui.id(("media.srow", seq_id));
        let it = ui.interact(id, row);
        if active {
            ui.fill(row, theme::ACCENT_SOFT);
        } else if it.hovered {
            ui.fill(row, theme::SURFACE_2);
        }
        let icon_rect = Rect::new(x0 + 8.0, row.y + 7.0, 14.0, 14.0);
        let row_icon = if app.timeline.is_multicam_source(seq_id) {
            "layout-grid"
        } else {
            "clapperboard"
        };
        ui.icon(row_icon, icon_rect, 14.0, theme::ACCENT);
        let name_rect = Rect::new(x0 + 28.0, row.y, row.w - 36.0, row.h);
        let disp = ui.font(FontKind::Sans12).ellipsize(&name, name_rect.w);
        ui.text_left(&disp, name_rect, theme::TEXT_1, FontKind::Sans12);

        if it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
        }
        if it.hovered && ui.input.left_pressed {
            app.app.focused_panel = "media".into();
            ui.start_drag(DragPayload::Sequences(vec![seq_id.to_string()]));
        }
        if it.double_clicked {
            ui.run_command_with("sequence.open", serde_json::json!({ "sequenceId": seq_id }));
        }
        if it.right_clicked {
            app.app.focused_panel = "media".into();
            let last = app.timeline.len() <= 1;
            app.context_menu
                .show(ui.input.mouse.x, ui.input.mouse.y, sequence_browser_menu(seq_id, last));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn asset_row(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        row: Rect,
        x0: f32,
        name_w: f32,
        asset_id: &str,
        ordered: &[String],
        searching: bool,
    ) {
        let Some(asset) = app.media.asset(asset_id) else { return };
        let selected = app.media.selected_asset_ids.iter().any(|id| id == asset_id);
        let name = asset.name.clone();
        let kind = asset.kind;
        let offline = asset.offline;
        let label = asset.label;
        let proxy = proxy_badge(&app.media, asset);
        let cells: Vec<String> = COLUMNS.iter().map(|c| metadata_cell(asset, c.key)).collect();
        let usage = app.timeline.asset_usage_count(asset_id);
        let bin_label = if searching {
            Some(app.media.bin_path_label(app.media.effective_bin(asset)))
        } else {
            None
        };

        let id = ui.id(("media.row", asset_id));
        let it = ui.interact(id, row);
        if selected {
            ui.fill(row, theme::ACCENT_SOFT);
        } else if it.hovered {
            ui.fill(row, theme::SURFACE_2);
        }
        let fg = if selected { theme::TEXT_1 } else { theme::TEXT_2 };

        // Label-Streifen ganz links.
        if let Some(label) = label {
            ui.fill(Rect::new(row.x, row.y + 3.0, 3.0, row.h - 6.0), label_color(label));
        }
        // Art-Icon + Name (fix breit).
        let icon_rect = Rect::new(x0 + 8.0, row.y + 7.0, 14.0, 14.0);
        if offline {
            ui.icon("triangle-alert", icon_rect, 14.0, theme::DANGER);
        } else {
            ui.icon(kind_icon(kind), icon_rect, 14.0, theme::TEXT_3);
        }
        let mut name_rect = Rect::new(x0 + 28.0, row.y, name_w - 36.0, row.h);
        // Verwendungs-Punkt rechts in der Name-Spalte.
        if usage > 0 {
            let badge = name_rect.cut_right(28.0);
            let txt = format!("{usage}×");
            ui.text_left(&txt, Rect::new(badge.x + 4.0, badge.y, 24.0, badge.h), theme::ACCENT, FontKind::Mono12);
        }
        // Proxy-Indikator (kleiner Buchstabe) rechts in der Name-Spalte.
        if let Some((lbl, color)) = &proxy {
            let chip = name_rect.cut_right(22.0);
            ui.text_left(lbl, Rect::new(chip.x + 2.0, chip.y, 20.0, chip.h), *color, FontKind::Mono12);
        }
        if self.is_renaming_asset(asset_id) {
            self.render_rename_input(ui, app, name_rect.inset_xy(0.0, 3.0));
        } else {
            let disp = ui.font(FontKind::Sans12).ellipsize(&name, name_rect.w);
            ui.text_left(&disp, name_rect, fg, FontKind::Sans12);
        }

        // Metadaten-Spalten.
        let widths = &app.media.view.col_widths;
        let mut x = x0 + name_w;
        for (i, def) in COLUMNS.iter().enumerate() {
            let w = widths.get(i).copied().unwrap_or(def.default_w);
            let font = if def.key == SortKey::Path || def.key == SortKey::Codec {
                FontKind::Sans12
            } else {
                FontKind::Mono12
            };
            let text = if def.key == SortKey::Path {
                if let Some(bl) = &bin_label {
                    // Im Suchmodus zeigt die Pfad-Spalte den Bin-Pfad.
                    bl.clone()
                } else {
                    cells[i].clone()
                }
            } else {
                cells[i].clone()
            };
            let disp = ui.font(font).ellipsize(&text, w - 8.0);
            ui.text_left(&disp, Rect::new(x + 2.0, row.y, w - 4.0, row.h), fg, font);
            x += w;
        }

        self.asset_interactions(ui, app, it, asset_id, ordered);
    }

    // ------------------------------------------------ gemeinsame Interaktionen

    fn asset_interactions(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        it: crate::ui::Interaction,
        asset_id: &str,
        ordered: &[String],
    ) {
        if self.is_renaming_asset(asset_id) {
            return;
        }
        if it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
        }
        if it.hovered && ui.input.left_pressed {
            app.app.focused_panel = "media".into();
            let already = app.media.selected_asset_ids.iter().any(|id| id == asset_id);
            if !already || ui.input.ctrl || ui.input.meta || ui.input.shift {
                self.select_asset(app, ui, asset_id, ordered);
            }
            let ids = if app.media.selected_asset_ids.is_empty() {
                vec![asset_id.to_string()]
            } else {
                app.media.selected_asset_ids.clone()
            };
            ui.start_drag(DragPayload::Assets(ids));
        }
        if it.double_clicked {
            ui.run_command_with("media.openInSource", serde_json::json!({ "assetId": asset_id }));
        }
        if it.right_clicked {
            app.app.focused_panel = "media".into();
            if !app.media.selected_asset_ids.iter().any(|id| id == asset_id) {
                app.media.select(vec![asset_id.to_string()]);
                self.anchor = Some(asset_id.to_string());
            }
            let menu = asset_context_menu(&app.media, asset_id);
            app.context_menu.show(ui.input.mouse.x, ui.input.mouse.y, menu);
        }
    }

    fn select_asset(&mut self, app: &mut AppState, ui: &Ui, asset_id: &str, ordered: &[String]) {
        let multi = ui.input.ctrl || ui.input.meta;
        let range = ui.input.shift;
        if range {
            let anchor = self
                .anchor
                .clone()
                .or_else(|| app.media.selected_asset_ids.first().cloned())
                .unwrap_or_else(|| asset_id.to_string());
            let ia = ordered.iter().position(|id| *id == anchor);
            let ib = ordered.iter().position(|id| id == asset_id);
            if let (Some(a), Some(b)) = (ia, ib) {
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                let mut sel: Vec<String> = ordered[lo..=hi].to_vec();
                if multi {
                    // Bestehende Auswahl ergänzen (ohne Duplikate).
                    for id in &app.media.selected_asset_ids {
                        if !sel.contains(id) {
                            sel.push(id.clone());
                        }
                    }
                }
                app.media.select(sel);
            }
        } else if multi {
            let mut sel = app.media.selected_asset_ids.clone();
            if let Some(pos) = sel.iter().position(|id| id == asset_id) {
                sel.remove(pos);
            } else {
                sel.push(asset_id.to_string());
            }
            app.media.select(sel);
            self.anchor = Some(asset_id.to_string());
        } else {
            app.media.select(vec![asset_id.to_string()]);
            self.anchor = Some(asset_id.to_string());
        }
    }

    /// Drop-Ziel eines Bins (Ordner-Kachel/-Reihe oder Breadcrumb): Assets oder
    /// einen anderen Bin hineinziehen.
    fn handle_bin_drop(&mut self, ui: &mut Ui, app: &mut AppState, rect: Rect, bin_id: &str) {
        // Hover-Hervorhebung während eines Asset-/Bin-Drags.
        let highlight = matches!(
            ui.drag_over(rect),
            Some(DragPayload::Assets(_) | DragPayload::Bins(_))
        );
        if highlight {
            ui.stroke_rounded(rect, theme::RADIUS_SM, 2.0, theme::ACCENT);
        }
        match ui.accept_drop(rect) {
            Some(DragPayload::Assets(ids)) => {
                app.media.move_assets_to_bin(&ids, bin_id);
            }
            Some(DragPayload::Bins(ids)) => {
                for id in ids {
                    app.media.move_bin(&id, bin_id);
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------- Hover-Scrub

    /// Liefert den Cache-Pfad des Scrub-Frames an der Mausposition (oder None,
    /// solange er noch erzeugt wird) und fordert ihn bei Bedarf an.
    fn scrub_frame(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        asset_id: &str,
        path: &str,
        duration: f64,
        thumb: Rect,
    ) -> Option<String> {
        let frac = ((ui.input.mouse.x - thumb.x) / thumb.w.max(1.0)).clamp(0.0, 1.0);
        let bucket = (frac * (SCRUB_BUCKETS - 1) as f32).round() as u32;
        if let Some(p) = app.media.scrub_thumbs.get(asset_id).and_then(|m| m.get(&bucket)) {
            self.scrub_pending.remove(&(asset_id.to_string(), bucket));
            return Some(p.clone());
        }
        let key = (asset_id.to_string(), bucket);
        if !self.scrub_pending.contains(&key) {
            let t = (bucket as f64 / (SCRUB_BUCKETS - 1) as f64) * duration;
            ui.request_scrub(asset_id, path, t, bucket);
            self.scrub_pending.insert(key);
        }
        None
    }

    // --------------------------------------------------------------- Marquee

    fn run_marquee(&mut self, ui: &mut Ui, app: &mut AppState, area: Rect, placed: &[(String, Rect)]) {
        // Start: Druck auf leere Fläche (kein Item aktiv), in der Ansicht.
        if ui.input.left_pressed
            && area.contains(ui.input.mouse)
            && ui.nothing_active()
            && self.edit.is_none()
            && ui.active_drag().is_none()
        {
            self.marquee = Some(Marquee {
                origin: ui.input.mouse,
                additive: ui.input.ctrl || ui.input.meta || ui.input.shift,
            });
            if !(ui.input.ctrl || ui.input.meta || ui.input.shift) {
                app.media.select(Vec::new());
            }
        }
        let Some(m) = &self.marquee else { return };
        let o = m.origin;
        let additive = m.additive;
        let p = ui.input.mouse;
        let sel_rect = Rect::new(o.x.min(p.x), o.y.min(p.y), (o.x - p.x).abs(), (o.y - p.y).abs());
        // Visualisierung.
        ui.fill(sel_rect, theme::with_alpha(theme::ACCENT, 40));
        ui.stroke_rounded(sel_rect, 0.0, 1.0, theme::ACCENT);
        // Treffer einsammeln.
        let mut hits: Vec<String> = Vec::new();
        for (id, r) in placed {
            if rects_overlap(sel_rect, *r) {
                hits.push(id.clone());
            }
        }
        if additive {
            for id in &app.media.selected_asset_ids {
                if !hits.contains(id) {
                    hits.push(id.clone());
                }
            }
        }
        app.media.select(hits);
        if ui.input.left_released {
            self.marquee = None;
        }
    }

    fn background_interactions(&mut self, ui: &mut Ui, app: &mut AppState, area: Rect, _searching: bool) {
        // Rechtsklick auf leere Fläche → Hintergrundmenü.
        if ui.mouse_in(area) && ui.input.right_pressed && ui.nothing_active() && !app.context_menu.open {
            let current = app.media.current_bin().to_string();
            app.app.focused_panel = "media".into();
            app.context_menu
                .show(ui.input.mouse.x, ui.input.mouse.y, background_context_menu(&current));
        }
    }

    // ------------------------------------------------------ Inline-Umbenennen

    fn take_rename_request(&mut self, app: &mut AppState) {
        if let Some(req) = app.media.rename_request.take() {
            let initial = match &req {
                RenameTarget::Asset(id) => app.media.asset(id).map(|a| a.name.clone()),
                RenameTarget::Bin(id) => Some(app.media.bin_name(id)),
            };
            let Some(initial) = initial else { return };
            let mut input = TextInputState::default();
            input.set_text(initial);
            self.edit = Some(RenameEdit { target: req, input, started: false });
        }
    }

    fn is_renaming_asset(&self, asset_id: &str) -> bool {
        matches!(&self.edit, Some(e) if e.target == RenameTarget::Asset(asset_id.to_string()))
    }

    fn is_renaming_bin(&self, bin_id: &str) -> bool {
        matches!(&self.edit, Some(e) if e.target == RenameTarget::Bin(bin_id.to_string()))
    }

    fn render_rename_input(&mut self, ui: &mut Ui, app: &mut AppState, rect: Rect) {
        let Some(edit) = self.edit.as_mut() else { return };
        let key = match &edit.target {
            RenameTarget::Asset(id) => format!("media.rename.asset.{id}"),
            RenameTarget::Bin(id) => format!("media.rename.bin.{id}"),
        };
        // Beim ersten Render fokussieren + alles markieren.
        if !edit.started {
            let wid = ui.id(&key);
            ui.persist.keyboard_focus = wid;
            edit.input.sel_start = 0;
            edit.input.cursor = edit.input.text.len();
            edit.started = true;
        }
        let res = edit.input.show(ui, &key, rect, "Name");
        // Übernehmen bei Enter oder Fokusverlust (Klick woanders). Leerer/
        // unveränderter Name wird vom Store ohnehin ignoriert.
        if res.submitted || !res.focused {
            let text = edit.input.text.trim().to_string();
            let target = edit.target.clone();
            if !text.is_empty() {
                match target {
                    RenameTarget::Asset(id) => app.media.rename_asset(&id, &text),
                    RenameTarget::Bin(id) => app.media.rename_bin(&id, &text),
                }
            }
            self.edit = None;
        }
    }

    // --------------------------------------------------------------- Suche

    fn search_results(&self, app: &AppState, query: &str) -> Vec<String> {
        let media = &app.media;
        let mut ids: Vec<String> = media
            .assets
            .iter()
            .filter(|a| {
                if a.name.to_lowercase().contains(query) || a.path.to_lowercase().contains(query) {
                    return true;
                }
                if media.bin_path_label(media.effective_bin(a)).to_lowercase().contains(query) {
                    return true;
                }
                // Metadaten-Treffer (Codec, Auflösung, Datum …).
                [
                    SortKey::Resolution,
                    SortKey::Codec,
                    SortKey::Audio,
                    SortKey::Date,
                ]
                .iter()
                .any(|k| metadata_cell(a, *k).to_lowercase().contains(query))
            })
            .map(|a| a.id.clone())
            .collect();
        sort_assets(media, &mut ids, media.view.sort, media.view.sort_desc);
        ids
    }
}

/// Geometrie einer Asset-Kachel (Rahmen + Thumbnail-Bereich + Suchmodus-Flag).
struct TileParts {
    tile: Rect,
    thumb_rect: Rect,
    show_path: bool,
}

fn ui_tile_parts(tile: Rect, thumb_h: f32, show_path: bool) -> TileParts {
    TileParts {
        tile,
        thumb_rect: Rect::new(tile.x, tile.y, tile.w, thumb_h),
        show_path,
    }
}
