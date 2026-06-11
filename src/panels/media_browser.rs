//! Medien-Browser: Toolbar (Import, Suche, Ansicht-Toggle), Raster-/Listen-
//! ansicht, Mehrfachauswahl, Kontextmenü, Drag-Quelle für die Timeline.

use crate::core::types::MediaKind;
use crate::overlays::context_menu::{MenuEntry, MenuItem};
use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::widgets::{IconButton, TextButton};
use crate::ui::{DragPayload, FontKind, Ui};
use raylib::consts::MouseCursor;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Grid,
    List,
}

pub struct MediaBrowserPanel {
    view: ViewMode,
    search: TextInputState,
    scroll: ScrollState,
}

impl Default for MediaBrowserPanel {
    fn default() -> Self {
        MediaBrowserPanel {
            view: ViewMode::Grid,
            search: TextInputState {
                pad_left: 16.0, // Platz für die Lupe (pl-6)
                ..Default::default()
            },
            scroll: ScrollState::default(),
        }
    }
}

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

fn asset_context_menu(asset_id: &str) -> Vec<MenuEntry> {
    let arg = serde_json::json!({ "assetId": asset_id });
    vec![
        MenuEntry::Item(
            MenuItem::command("media.openInSource")
                .with_icon("monitor-play")
                .with_args(arg.clone()),
        ),
        MenuEntry::Item(
            MenuItem::command("media.addSelectionToTimeline").with_icon("list-video"),
        ),
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
    ]
}

impl MediaBrowserPanel {
    fn select_with_modifiers(&self, app: &mut AppState, ui: &Ui, asset_id: &str) {
        let multi = ui.input.ctrl || ui.input.meta;
        if multi {
            let mut sel = app.media.selected_asset_ids.clone();
            if let Some(pos) = sel.iter().position(|id| id == asset_id) {
                sel.remove(pos);
            } else {
                sel.push(asset_id.to_string());
            }
            app.media.select(sel);
        } else {
            app.media.select(vec![asset_id.to_string()]);
        }
    }
}

impl Panel for MediaBrowserPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);
        let mut area = rect;

        // ---- Toolbar (h-9, border-b, px-2, gap-2) ----
        let toolbar = area.cut_top(36.0);
        ui.hline(toolbar.x, toolbar.bottom() - 1.0, toolbar.w, theme::LINE);
        let mut tb = toolbar.inset_xy(8.0, 0.0);

        let import_btn = TextButton::new("Importieren").icon("import");
        let btn_w = import_btn.measure(ui);
        let import_rect = tb.cut_left(btn_w);
        let import_rect = Rect::new(import_rect.x, tb.y + 6.0, btn_w, 24.0);
        if import_btn.show(ui, "media.import.btn", import_rect).clicked {
            ui.run_command("media.import");
        }
        tb.cut_left(8.0);

        // Suchfeld (w-44, h-6) mit Lupe
        let search_rect = Rect::new(tb.x, tb.y + 6.0, 176.0, 24.0);
        tb.cut_left(176.0 + 8.0);
        self.search.show(ui, "media.search", search_rect, "Suchen");
        ui.icon(
            "search",
            Rect::new(search_rect.x + 4.0, search_rect.y + 5.0, 14.0, 14.0),
            14.0,
            theme::TEXT_3,
        );

        // Ansicht-Umschalter rechts (Grid/Liste)
        let mut right = tb;
        let list_rect = Rect::new(right.right() - 24.0, right.y + 6.0, 24.0, 24.0);
        let grid_rect = Rect::new(right.right() - 48.0, right.y + 6.0, 24.0, 24.0);
        right.cut_right(48.0);
        if IconButton::new("layout-grid")
            .size(14.0)
            .active(self.view == ViewMode::Grid)
            .tooltip("Rasteransicht")
            .show(ui, "media.view.grid", grid_rect)
            .clicked
        {
            self.view = ViewMode::Grid;
        }
        if IconButton::new("list")
            .size(14.0)
            .active(self.view == ViewMode::List)
            .tooltip("Listenansicht")
            .show(ui, "media.view.list", list_rect)
            .clicked
        {
            self.view = ViewMode::List;
        }

        // ---- Inhalt ----
        let query = self.search.text.to_lowercase();
        let assets: Vec<(usize, String)> = app
            .media
            .assets
            .iter()
            .enumerate()
            .filter(|(_, a)| query.is_empty() || a.name.to_lowercase().contains(&query))
            .map(|(i, a)| (i, a.id.clone()))
            .collect();

        if app.media.assets.is_empty() {
            let msg = if app.media.importing {
                "Import läuft …"
            } else {
                "Noch keine Medien importiert"
            };
            ui.text_centered(msg, area, theme::TEXT_3, FontKind::Sans12);
            return;
        }

        match self.view {
            ViewMode::Grid => self.grid_view(ui, app, area, &assets),
            ViewMode::List => self.list_view(ui, app, area, &assets),
        }

        // Klick auf leere Fläche: Auswahl aufheben
        if ui.mouse_in(area) && ui.input.left_pressed && ui.nothing_active() {
            app.media.select(Vec::new());
        }
    }
}

impl MediaBrowserPanel {
    fn grid_view(&mut self, ui: &mut Ui, app: &mut AppState, area: Rect, assets: &[(usize, String)]) {
        // grid-cols-[repeat(auto-fill,minmax(8.5rem,1fr))] gap-2 p-2
        let pad = 8.0;
        let gap = 8.0;
        let min_w = 136.0;
        let inner_w = area.w - pad * 2.0 - theme::SCROLLBAR_W;
        let cols = ((inner_w + gap) / (min_w + gap)).floor().max(1.0) as usize;
        let tile_w = (inner_w - gap * (cols as f32 - 1.0)) / cols as f32;
        let thumb_h = tile_w * 9.0 / 16.0; // aspect-video
        let tile_h = thumb_h + 25.0; // + Namenszeile
        let rows = assets.len().div_ceil(cols);
        let content_h = pad * 2.0 + rows as f32 * (tile_h + gap) - gap;

        let view = self.scroll.begin(ui, area, 0.0, content_h);
        for (i, (_, asset_id)) in assets.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let tile = Rect::new(
                view.viewport.x + pad + col as f32 * (tile_w + gap),
                view.origin_y + pad + row as f32 * (tile_h + gap),
                tile_w,
                tile_h,
            );
            if tile.bottom() < view.viewport.y || tile.y > view.viewport.bottom() {
                continue;
            }
            self.asset_tile(ui, app, tile, thumb_h, asset_id);
        }
        self.scroll.end(ui, area, 0.0, content_h);
    }

    fn asset_tile(&self, ui: &mut Ui, app: &mut AppState, tile: Rect, thumb_h: f32, asset_id: &str) {
        let Some(asset) = app.media.asset(asset_id) else { return };
        let selected = app.media.selected_asset_ids.contains(&asset.id);
        let name = asset.name.clone();
        let kind = asset.kind;
        let duration = asset.info.duration_sec;
        let thumb = asset.thumbnail_path.clone();
        let offline = asset.offline;

        let id = ui.id(("media.tile", asset_id));
        let it = ui.interact(id, tile);

        // Kachel: rounded, border, bg-surface-2; ausgewählt: Ring accent
        let thumb_rect = Rect::new(tile.x, tile.y, tile.w, thumb_h);
        ui.fill_rounded(tile, theme::RADIUS_SM, theme::SURFACE_2);
        if let Some(thumb_path) = &thumb {
            ui.push_clip(thumb_rect);
            ui.draw_texture_cover(thumb_path, thumb_rect);
            ui.pop_clip();
        } else if kind == MediaKind::Audio {
            ui.icon("music", thumb_rect, 24.0, theme::TEXT_3);
        }
        if selected {
            ui.stroke_rounded(tile, theme::RADIUS_SM, 2.0, theme::ACCENT);
        } else {
            let border = if it.hovered {
                theme::LINE_STRONG
            } else {
                theme::LINE
            };
            ui.stroke_rounded(tile, theme::RADIUS_SM, 1.0, border);
        }

        // Art-Icon oben links (size-5, bg-black/60)
        let badge = Rect::new(thumb_rect.x + 4.0, thumb_rect.y + 4.0, 20.0, 20.0);
        ui.fill_rounded(badge, theme::RADIUS_XS, theme::with_alpha(theme::BLACK, 153));
        ui.icon(kind_icon(kind), badge, 12.0, theme::TEXT_1);

        // Offline-Badge oben rechts: Quelldatei fehlt.
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

        // Dauer-Badge unten rechts (font-mono, bg-black/70)
        if kind != MediaKind::Image {
            let label = format_duration(duration);
            let w = ui.font(FontKind::Mono12).width(&label) + 8.0;
            let dur = Rect::new(
                thumb_rect.right() - w - 4.0,
                thumb_rect.bottom() - 20.0,
                w,
                16.0,
            );
            ui.fill_rounded(dur, theme::RADIUS_XS, theme::with_alpha(theme::BLACK, 179));
            ui.text_centered(&label, dur, theme::TEXT_1, FontKind::Mono12);
        }

        // Namenszeile (truncate px-1.5 py-1 text-xs)
        let name_rect = Rect::new(tile.x + 6.0, thumb_rect.bottom(), tile.w - 12.0, 24.0);
        let display = ui.font(FontKind::Sans12).ellipsize(&name, name_rect.w);
        ui.text_left(&display, name_rect, theme::TEXT_1, FontKind::Sans12);

        self.asset_interactions(ui, app, it, asset_id);
    }

    fn list_view(&mut self, ui: &mut Ui, app: &mut AppState, area: Rect, assets: &[(usize, String)]) {
        let row_h = 28.0; // h-7
        let header_h = 28.0;
        let content_h = header_h + assets.len() as f32 * row_h;

        let view = self.scroll.begin(ui, area, 0.0, content_h);

        // Spalten: Name | Dauer | Auflösung | Codec
        let name_w = (view.viewport.w - 90.0 - 110.0 - 90.0).max(120.0);
        let cols = [name_w, 90.0, 110.0, 90.0];

        for (i, (_, asset_id)) in assets.iter().enumerate() {
            let row = Rect::new(
                view.viewport.x,
                view.origin_y + header_h + i as f32 * row_h,
                view.viewport.w,
                row_h,
            );
            if row.bottom() < view.viewport.y || row.y > view.viewport.bottom() {
                continue;
            }
            let Some(asset) = app.media.asset(asset_id) else { continue };
            let selected = app.media.selected_asset_ids.contains(&asset.id);
            let name = asset.name.clone();
            let duration = format_duration(asset.info.duration_sec);
            let resolution = asset
                .info
                .video
                .first()
                .map(|v| format!("{}×{}", v.width, v.height))
                .unwrap_or_else(|| "—".into());
            let codec = asset
                .info
                .video
                .first()
                .map(|v| v.codec.clone())
                .or_else(|| asset.info.audio.first().map(|a| a.codec.clone()))
                .unwrap_or_else(|| "—".into());
            let kind = asset.kind;
            let offline = asset.offline;

            let id = ui.id(("media.row", asset_id));
            let it = ui.interact(id, row);
            if selected {
                ui.fill(row, theme::ACCENT_SOFT);
            } else if it.hovered {
                ui.fill(row, theme::SURFACE_2);
            }
            let fg = if selected { theme::TEXT_1 } else { theme::TEXT_2 };
            let mut x = row.x + 8.0;
            let icon_rect = Rect::new(x, row.y + 7.0, 14.0, 14.0);
            if offline {
                ui.icon("triangle-alert", icon_rect, 14.0, theme::DANGER);
            } else {
                ui.icon(kind_icon(kind), icon_rect, 14.0, theme::TEXT_3);
            }
            x += 20.0;
            let name_rect = Rect::new(x, row.y, cols[0] - 28.0, row_h);
            let display = ui.font(FontKind::Sans12).ellipsize(&name, name_rect.w);
            ui.text_left(&display, name_rect, fg, FontKind::Sans12);
            x = row.x + cols[0];
            ui.text_left(
                &duration,
                Rect::new(x, row.y, cols[1], row_h),
                fg,
                FontKind::Mono12,
            );
            x += cols[1];
            ui.text_left(
                &resolution,
                Rect::new(x, row.y, cols[2], row_h),
                fg,
                FontKind::Mono12,
            );
            x += cols[2];
            ui.text_left(&codec, Rect::new(x, row.y, cols[3], row_h), fg, FontKind::Sans12);

            self.asset_interactions(ui, app, it, asset_id);
        }

        // Sticky Header zuletzt (überdeckt gescrollte Zeilen)
        let header = Rect::new(view.viewport.x, view.viewport.y, view.viewport.w, header_h);
        ui.fill(header, theme::SURFACE_1);
        ui.hline(header.x, header.bottom() - 1.0, header.w, theme::LINE);
        let mut x = header.x + 28.0;
        for (label, w) in [
            ("Name", cols[0] - 28.0),
            ("Dauer", cols[1]),
            ("Auflösung", cols[2]),
            ("Codec", cols[3]),
        ] {
            ui.text_left(
                label,
                Rect::new(x, header.y, w, header_h),
                theme::TEXT_3,
                FontKind::Sans12,
            );
            x += w;
        }

        self.scroll.end(ui, area, 0.0, content_h);
    }

    /// Gemeinsame Interaktionen: Auswahl, Doppelklick, Rechtsklick, Drag.
    fn asset_interactions(
        &self,
        ui: &mut Ui,
        app: &mut AppState,
        it: crate::ui::Interaction,
        asset_id: &str,
    ) {
        if it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
        }
        if it.hovered && ui.input.left_pressed {
            app.app.focused_panel = "media".into();
            if !app.media.selected_asset_ids.contains(&asset_id.to_string()) || ui.input.ctrl {
                self.select_with_modifiers(app, ui, asset_id);
            }
            // Drag-Kandidat: alle ausgewählten Assets (MIME editron/assets)
            let ids = if app.media.selected_asset_ids.is_empty() {
                vec![asset_id.to_string()]
            } else {
                app.media.selected_asset_ids.clone()
            };
            ui.start_drag(DragPayload::Assets(ids));
        }
        if it.double_clicked {
            ui.run_command_with(
                "media.openInSource",
                serde_json::json!({ "assetId": asset_id }),
            );
        }
        if it.right_clicked {
            app.app.focused_panel = "media".into();
            if !app.media.selected_asset_ids.contains(&asset_id.to_string()) {
                app.media.select(vec![asset_id.to_string()]);
            }
            app.context_menu.show(
                ui.input.mouse.x,
                ui.input.mouse.y,
                asset_context_menu(asset_id),
            );
        }
    }
}
