//! Effekte-Panel: durchsuchbare, einklappbare Kategorien (Video-/Audio-
//! Effekte und -Übergänge).

use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::{FontKind, Ui};
use raylib::consts::MouseCursor;
use std::collections::HashSet;

const CATEGORIES: [(&str, &str, &[(&str, &str)]); 4] = [
    (
        "video-effects",
        "Video-Effekte",
        &[
            ("Gaußscher Weichzeichner", "droplets"),
            ("Schärfen", "focus"),
            ("Farbkorrektur", "palette"),
            ("Transformieren", "move"),
            ("Zuschneiden", "crop"),
            ("Geschwindigkeit", "gauge"),
        ],
    ),
    (
        "video-transitions",
        "Video-Übergänge",
        &[
            ("Überblendung", "blend"),
            ("Wischen", "arrow-left-right"),
            ("Zoom", "zoom-in"),
        ],
    ),
    (
        "audio-effects",
        "Audio-Effekte",
        &[
            ("Equalizer", "sliders-horizontal"),
            ("Kompressor", "activity"),
            ("Hall", "audio-waveform"),
            ("Rauschminderung", "mic"),
        ],
    ),
    (
        "audio-transitions",
        "Audio-Übergänge",
        &[("Konstante Leistung", "volume-2")],
    ),
];

#[derive(Default)]
pub struct EffectsPanel {
    search: TextInputState,
    collapsed: HashSet<&'static str>,
    scroll: ScrollState,
}

impl Panel for EffectsPanel {
    fn update(&mut self, ui: &mut Ui, _app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);
        let mut area = rect;

        // Suchleiste (h-9, border-b): Lupe + transparentes Eingabefeld
        let bar = area.cut_top(36.0);
        ui.hline(bar.x, bar.bottom() - 1.0, bar.w, theme::LINE);
        let mut bar_inner = bar.inset_xy(8.0, 0.0);
        let icon_cell = bar_inner.cut_left(14.0);
        ui.icon("search", icon_cell, 14.0, theme::TEXT_3);
        bar_inner.cut_left(8.0);
        let field = Rect::new(bar_inner.x, bar.y + 6.0, bar_inner.w, 24.0);
        self.search
            .show(ui, "effects.search", field, "Effekte durchsuchen …");

        // Fußzeile
        let footer = area.cut_bottom(33.0);
        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        ui.text_left(
            "Effekte anwenden folgt — die FFmpeg-Filtergraph-Basis steht.",
            footer.inset_xy(12.0, 0.0),
            theme::TEXT_3,
            FontKind::Sans12,
        );

        // Inhalt
        let query = self.search.text.trim().to_lowercase();
        let searching = !query.is_empty();
        let filtered: Vec<(&str, &str, Vec<(&str, &str)>)> = CATEGORIES
            .iter()
            .map(|(id, name, entries)| {
                let entries: Vec<(&str, &str)> = entries
                    .iter()
                    .filter(|(n, _)| !searching || n.to_lowercase().contains(&query))
                    .copied()
                    .collect();
                (*id, *name, entries)
            })
            .filter(|(_, _, entries)| !searching || !entries.is_empty())
            .collect();

        let mut content_h = 8.0;
        for (id, _, entries) in &filtered {
            content_h += 28.0;
            if searching || !self.collapsed.contains(id) {
                content_h += entries.len() as f32 * 28.0;
            }
        }
        if filtered.is_empty() {
            ui.text_centered(
                "Keine Effekte gefunden.",
                area,
                theme::TEXT_3,
                FontKind::Sans12,
            );
            return;
        }

        let view = self.scroll.begin(ui, area, 0.0, content_h);
        let mut y = view.origin_y + 4.0;
        let w = view.viewport.w;
        for (id, name, entries) in &filtered {
            let header = Rect::new(view.viewport.x, y, w, 28.0);
            let is_collapsed = !searching && self.collapsed.contains(id);
            let hid = ui.id(("effects.cat", *id));
            let it = ui.interact(hid, header);
            if it.hovered {
                ui.fill(header, theme::SURFACE_2);
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            }
            let mut hi = header.inset_xy(8.0, 0.0);
            let chev = hi.cut_left(14.0);
            ui.icon(
                if is_collapsed { "chevron-right" } else { "chevron-down" },
                chev,
                14.0,
                theme::TEXT_3,
            );
            hi.cut_left(6.0);
            let count = entries.len().to_string();
            let count_w = ui.font(FontKind::Mono12).width(&count);
            let count_cell = hi.cut_right(count_w);
            ui.text_left(&count, count_cell, theme::TEXT_3, FontKind::Mono12);
            ui.text_left(
                name,
                hi,
                if it.hovered { theme::TEXT_1 } else { theme::TEXT_2 },
                FontKind::Sans12Medium,
            );
            if it.clicked && !searching {
                if is_collapsed {
                    self.collapsed.remove(id);
                } else {
                    self.collapsed.insert(id);
                }
            }
            y += 28.0;
            if is_collapsed {
                continue;
            }
            for (entry_name, icon) in entries {
                let row = Rect::new(view.viewport.x, y, w, 28.0);
                if ui.mouse_in(row) {
                    ui.fill(row, theme::SURFACE_2);
                }
                let mut ri = row.inset_xy(8.0, 0.0);
                ri.cut_left(20.0); // pl-7 gesamt
                let ic = ri.cut_left(14.0);
                ui.icon(icon, ic, 14.0, theme::TEXT_2);
                ri.cut_left(8.0);
                // Badge "ffmpeg" rechts
                let badge_w = ui.font(FontKind::Mono12).width("ffmpeg") + 8.0;
                let badge = ri.cut_right(badge_w);
                let badge = Rect::new(badge.x, row.y + 6.0, badge_w, 17.0);
                ui.stroke_rounded(badge, theme::RADIUS_SM, 1.0, theme::LINE);
                ui.text_centered("ffmpeg", badge, theme::TEXT_3, FontKind::Mono12);
                ri.cut_right(8.0);
                let display = ui.font(FontKind::Sans12).ellipsize(entry_name, ri.w);
                ui.text_left(&display, ri, theme::TEXT_1, FontKind::Sans12);
                y += 28.0;
            }
        }
        self.scroll.end(ui, area, 0.0, content_h);
    }
}
