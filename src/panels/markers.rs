//! Marker-Panel: Liste aller Sequenz-Marker (Farbe, Timecode, Name, Notiz).
//! Klick auf eine Zeile springt den Playhead, Doppelklick auf den Namen
//! editiert inline (Enter bestätigt), Rechtsklick öffnet das Kontextmenü
//! (Farbe, Bearbeiten…, Löschen). Die Kopfzeile bietet „Marker hinzufügen",
//! „Alle löschen" und einen Farbfilter.

use crate::core::marker::{MarkerColor, MarkerScope};
use crate::core::timecode::format_sequence_timecode;
use crate::overlays::context_menu::{CustomAction, MenuEntry, MenuItem};
use crate::overlays::marker_dialog::marker_color;
use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::widgets::IconButton;
use crate::ui::{FontKind, Ui};
use raylib::consts::MouseCursor;

const TOOLBAR_H: f32 = 36.0;
const FILTER_H: f32 = 26.0;
const ROW_H: f32 = 44.0;

/// Anzeige-Daten eines Markers (vorab eingesammelt — Borrow-Hygiene).
struct Row {
    id: String,
    time: f64,
    duration: f64,
    name: String,
    note: String,
    color: MarkerColor,
}

#[derive(Default)]
pub struct MarkersPanel {
    scroll: ScrollState,
    editor: TextInputState,
    /// Marker-ID, deren Name gerade inline editiert wird.
    editing: Option<String>,
    request_focus: bool,
    /// Undo-Snapshot für die laufende Umbenennung (einmalig je Sitzung).
    began: bool,
    /// Farbfilter (None = alle Farben).
    filter: Option<MarkerColor>,
}

impl MarkersPanel {
    fn stop_edit(&mut self) {
        self.editing = None;
        self.began = false;
    }
}

impl Panel for MarkersPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);

        if ui.mouse_in(rect) && (ui.input.left_pressed || ui.input.right_pressed) {
            app.app.focused_panel = "markers".into();
        }

        let mut area = rect;

        // ---------------------------------------------------- Kopfzeile
        let toolbar = area.cut_top(TOOLBAR_H);
        ui.fill(toolbar, theme::SURFACE_2);
        ui.hline(toolbar.x, toolbar.bottom() - 1.0, toolbar.w, theme::LINE);
        let mut tb = toolbar.inset_xy(8.0, 0.0);
        let total = app.timeline.markers.len();
        let title = if total == 0 {
            "Marker".to_string()
        } else {
            format!("Marker ({total})")
        };
        let tw = ui.font(FontKind::Sans12Medium).width(&title);
        ui.text_left(
            &title,
            Rect::new(tb.x, tb.y, tw + 4.0, tb.h),
            theme::TEXT_1,
            FontKind::Sans12Medium,
        );
        tb.cut_left(tw + 12.0);
        // Aktionen rechts.
        let btn = |ui: &mut Ui, tb: &mut Rect, icon: &str, id: &str, tip: &str, danger: bool| -> bool {
            let cell = tb.cut_right(30.0);
            let r = Rect::new(cell.x + 2.0, cell.y + (cell.h - 26.0) / 2.0, 26.0, 26.0);
            IconButton::new(icon)
                .tooltip(tip)
                .danger_hover(danger)
                .show(ui, id, r)
                .clicked
        };
        if btn(ui, &mut tb, "trash-2", "markers.clear", "Alle Marker löschen", true) {
            ui.run_command("marker.clearAll");
        }
        if btn(ui, &mut tb, "bookmark-plus", "markers.add", "Marker am Playhead (M)", false) {
            ui.run_command("marker.add");
        }

        // ---------------------------------------------------- Farbfilter
        let filter_bar = area.cut_top(FILTER_H);
        ui.fill(filter_bar, theme::SURFACE_1);
        ui.hline(filter_bar.x, filter_bar.bottom() - 1.0, filter_bar.w, theme::LINE);
        let mut fb = filter_bar.inset_xy(8.0, 0.0);
        let ic = fb.cut_left(16.0);
        ui.icon("filter", Rect::new(ic.x, ic.y + (ic.h - 13.0) / 2.0, 13.0, 13.0), 13.0, theme::TEXT_3);
        fb.cut_left(6.0);
        // „Alle"-Chip.
        {
            let cell = fb.cut_left(16.0);
            let dot = Rect::new(cell.x, cell.y + (cell.h - 14.0) / 2.0, 14.0, 14.0);
            let id = ui.id("markers.filter.all");
            let it = ui.interact(id, dot);
            ui.fill_rounded(dot, 3.0, theme::SURFACE_3);
            ui.icon("x", dot, 11.0, if self.filter.is_none() { theme::TEXT_1 } else { theme::TEXT_3 });
            if self.filter.is_none() {
                ui.stroke_rounded(dot, 3.0, 1.5, theme::ACCENT);
            } else if it.hovered {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            }
            if it.clicked {
                self.filter = None;
            }
            fb.cut_left(6.0);
        }
        for (i, c) in MarkerColor::ALL.into_iter().enumerate() {
            let cell = fb.cut_left(20.0);
            let dot = Rect::new(cell.x, cell.y + (cell.h - 14.0) / 2.0, 14.0, 14.0);
            let id = ui.id(("markers.filter", i));
            let it = ui.interact(id, dot);
            ui.fill_rounded(dot, 3.0, marker_color(c));
            if self.filter == Some(c) {
                ui.stroke_rounded(dot, 3.0, 2.0, theme::WHITE);
            } else if it.hovered {
                ui.stroke_rounded(dot, 3.0, 1.0, theme::with_alpha(theme::WHITE, 150));
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            }
            if it.clicked {
                self.filter = if self.filter == Some(c) { None } else { Some(c) };
            }
        }

        // ---------------------------------------------------- Liste
        let rows: Vec<Row> = app
            .timeline
            .markers
            .iter()
            .filter(|m| self.filter.is_none_or(|f| m.color == f))
            .map(|m| Row {
                id: m.id.clone(),
                time: m.time,
                duration: m.duration,
                name: m.name.clone(),
                note: m.note.clone(),
                color: m.color,
            })
            .collect();

        if rows.is_empty() {
            let center = area.center_box(260.0, 52.0);
            let mut c = center;
            let ic = c.cut_top(22.0);
            ui.icon("bookmark", Rect::new(ic.x + (ic.w - 22.0) / 2.0, ic.y, 22.0, 22.0), 22.0, theme::with_alpha(theme::TEXT_3, 150));
            c.cut_top(8.0);
            let hint = if total == 0 {
                "Noch keine Marker — M setzt einen am Playhead."
            } else {
                "Keine Marker dieser Farbe."
            };
            ui.text_centered(hint, c, theme::TEXT_3, FontKind::Sans12);
            return;
        }

        let content_h = rows.len() as f32 * ROW_H;
        let view = self.scroll.begin(ui, area, area.w, content_h);
        let playhead = app.timeline.playhead_sec;
        let frame_tol = 0.5 / app.timeline.settings.rate.fps().max(1.0);
        let mut seek_to: Option<f64> = None;
        let mut menu: Option<(f32, f32, String)> = None;
        let mut name_commit: Option<(String, String)> = None;
        for (i, row) in rows.iter().enumerate() {
            let y = view.origin_y + i as f32 * ROW_H;
            let row_rect = Rect::new(view.viewport.x, y, view.viewport.w, ROW_H);
            if row_rect.bottom() < area.y || row_rect.y > area.bottom() {
                continue;
            }
            let near_playhead = (row.time - playhead).abs() <= frame_tol
                || (row.duration > 0.0 && playhead >= row.time && playhead <= row.time + row.duration);
            let id = ui.id(("markers.row", &row.id));
            let it = ui.interact(id, row_rect);
            let bg = if near_playhead {
                theme::with_alpha(theme::ACCENT, 36)
            } else if it.hovered {
                theme::SURFACE_2
            } else {
                theme::SURFACE_1
            };
            ui.fill(row_rect, bg);
            ui.hline(row_rect.x, row_rect.bottom() - 1.0, row_rect.w, theme::with_alpha(theme::LINE, 120));

            // Farbband links + Punkt.
            ui.fill(Rect::new(row_rect.x, row_rect.y, 3.0, row_rect.h), marker_color(row.color));
            let mut inner = row_rect;
            inner.cut_left(10.0);
            let top = Rect::new(inner.x, inner.y + 5.0, inner.w - 8.0, 16.0);
            let bottom = Rect::new(inner.x, inner.y + 23.0, inner.w - 8.0, 14.0);

            // Timecode.
            let tc = format_sequence_timecode(row.time, &app.timeline.settings);
            let tcw = 92.0_f32;
            ui.text_left(&tc, Rect::new(top.x, top.y, tcw, top.h), theme::TEXT_2, FontKind::Mono11);
            if row.duration > 0.0 {
                ui.icon(
                    "arrow-right-to-line",
                    Rect::new(top.x + tcw, top.y + 1.0, 13.0, 13.0),
                    13.0,
                    theme::TEXT_3,
                );
            }

            // Lösch-Button rechts (bei Hover).
            let mut name_rect = bottom;
            if it.hovered {
                let del = Rect::new(top.right() - 22.0, top.y - 2.0, 22.0, 22.0);
                if IconButton::new("trash-2")
                    .size(14.0)
                    .danger_hover(true)
                    .tooltip("Marker löschen")
                    .show(ui, ("markers.del", &row.id), del)
                    .clicked
                {
                    app.timeline.remove_marker(&row.id);
                    self.stop_edit();
                    self.scroll.end(ui, area, area.w, content_h);
                    return;
                }
            }

            // Name: inline editierbar.
            if self.editing.as_deref() == Some(row.id.as_str()) {
                if self.request_focus {
                    ui.persist.keyboard_focus = ui.id(("markers.edit", &row.id));
                    self.request_focus = false;
                }
                let res = self.editor.show(ui, ("markers.edit", &row.id), name_rect, "Markername");
                if res.changed {
                    name_commit = Some((row.id.clone(), self.editor.text.clone()));
                }
                if res.submitted {
                    name_commit = Some((row.id.clone(), self.editor.text.clone()));
                    self.stop_edit();
                }
            } else {
                let label = if row.name.trim().is_empty() {
                    "—".to_string()
                } else {
                    row.name.clone()
                };
                let fg = if row.name.trim().is_empty() { theme::TEXT_3 } else { theme::TEXT_1 };
                name_rect.w -= 2.0;
                let label = ui.font(FontKind::Sans12).ellipsize(&label, name_rect.w);
                ui.text_left(&label, name_rect, fg, FontKind::Sans12);
            }
            // Notiz als dezenter Zusatz (rechtsbündig in der Namenszeile),
            // nur wenn nicht editiert und vorhanden.
            if self.editing.as_deref() != Some(row.id.as_str()) && !row.note.trim().is_empty() {
                let note_w = (inner.w * 0.45).min(160.0);
                let nr = Rect::new(bottom.right() - note_w, bottom.y, note_w, bottom.h);
                let txt = ui.font(FontKind::Sans12).ellipsize(row.note.trim(), note_w);
                ui.text_left(&txt, nr, theme::TEXT_3, FontKind::Sans12);
            }

            // Interaktion: Klick = springen, Doppelklick = umbenennen.
            if it.double_clicked {
                self.editing = Some(row.id.clone());
                self.editor.set_text(row.name.clone());
                self.request_focus = true;
                self.began = false;
            } else if it.clicked && self.editing.as_deref() != Some(row.id.as_str()) {
                seek_to = Some(row.time);
            }
            if it.right_clicked {
                menu = Some((ui.input.mouse.x, ui.input.mouse.y, row.id.clone()));
            }
            if it.hovered {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            }
        }
        self.scroll.end(ui, area, area.w, content_h);

        // Inline-Namensänderung übernehmen (ein Undo-Snapshot je Sitzung).
        if let Some((id, text)) = name_commit {
            if !self.began {
                app.timeline.begin_marker_edit();
                self.began = true;
            }
            app.timeline.marker_update_live(&id, |m| m.name = text);
        }
        if let Some(t) = seek_to {
            app.timeline.set_playhead(t);
        }
        if let Some((mx, my, id)) = menu {
            app.context_menu.show(mx, my, marker_menu(&id));
        }

        // Klick außerhalb der Editierzeile beendet die Inline-Bearbeitung.
        if self.editing.is_some()
            && ui.input.left_pressed
            && ui.persist.keyboard_focus
                != ui.id(("markers.edit", self.editing.as_ref().unwrap()))
        {
            self.stop_edit();
        }
    }
}

/// Kontextmenü eines Sequenz-Markers (Farbe-Untermenü, Bearbeiten, Löschen)
/// — vom Panel und vom Timeline-Lineal genutzt.
pub fn marker_menu(marker_id: &str) -> Vec<MenuEntry> {
    let scope = MarkerScope::Sequence;
    let color_items: Vec<MenuEntry> = MarkerColor::ALL
        .into_iter()
        .map(|c| {
            MenuEntry::Item(MenuItem::custom(
                c.label(),
                CustomAction::MarkerSetColor {
                    scope: scope.clone(),
                    marker_id: marker_id.to_string(),
                    color: c,
                },
            ))
        })
        .collect();
    vec![
        MenuEntry::Item(
            MenuItem::custom(
                "Bearbeiten…",
                CustomAction::MarkerEdit {
                    scope: scope.clone(),
                    marker_id: marker_id.to_string(),
                },
            )
            .with_icon("sliders-horizontal"),
        ),
        MenuEntry::Submenu {
            label: "Farbe".into(),
            icon: Some("palette"),
            items: color_items,
        },
        MenuEntry::Separator,
        MenuEntry::Item(
            MenuItem::custom(
                "Löschen",
                CustomAction::MarkerDelete {
                    scope,
                    marker_id: marker_id.to_string(),
                },
            )
            .with_icon("trash-2")
            .with_danger(),
        ),
    ]
}
