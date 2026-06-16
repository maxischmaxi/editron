//! Untertitel-Panel: Transkript-Workflow wie in Premiere — alle Segmente
//! der aktiven Untertitel-Spur untereinander (Timecode + Text), Klick
//! springt den Playhead, Doppelklick editiert inline, Enter bestätigt und
//! springt zum nächsten Segment. Kopfzeile: Spurauswahl, Segment/Spur
//! anlegen, SRT-Import/-Export; darunter der Spurstil (Schriftgröße,
//! Farbe, Position, Box/Kontur) — Änderungen sind undo-fähig
//! (Gesten-Snapshots wie im Grafik-Panel).

use crate::core::subtitle::{readability, SUBTITLE_POS_PRESETS};
use crate::core::text_raster::list_font_families;
use crate::core::timecode::format_sequence_timecode;
use crate::core::timeline::SelectMode;
use crate::core::title::{RgbaColor, TitleWeight};
use crate::panels::color::slider_row;
use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::{v2, Rect};
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::select::select;
use crate::ui::widgets::text_area::{TextArea, TEXTAREA_LINE_H};
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::widgets::IconButton;
use crate::ui::{FontKind, Ui};
use raylib::consts::MouseCursor;

const ROW_H: f32 = 26.0;
const GAP: f32 = 4.0;
const SEGMENT_ROW_H: f32 = 40.0;
const TOOLBAR_H: f32 = 36.0;

/// Anzeige-Daten eines Segments (vorab eingesammelt — Borrow-Hygiene).
struct SegmentRow {
    clip_id: String,
    start: f64,
    end: f64,
    text: String,
}

#[derive(Default)]
pub struct SubtitlesPanel {
    scroll: ScrollState,
    /// Inline-Editor der Segmentliste: Enter bestätigt (nächstes Segment),
    /// Umschalt+Enter bricht um (`TextArea`-Standardmodus).
    editor: TextArea,
    /// Clip-ID des Segments, dessen Text gerade inline editiert wird.
    editing: Option<String>,
    /// Fokus für den Inline-Editor im nächsten Frame anfordern.
    request_focus: bool,
    /// Spurstil-Sektion ein-/ausklappen.
    style_open: bool,
    hex_input: TextInputState,
    /// Laufende Slider-Geste (ein Undo-Snapshot pro Drag).
    slider_gesture: bool,
    /// Tipp-Sitzung (Inline-Editor/Hex-Feld): ein Snapshot je Fokus-Phase.
    text_gesture: bool,
    last_focus: u64,
}

impl SubtitlesPanel {
    /// Slider-Änderung am Spurstil: ein Undo-Snapshot pro Drag-Geste.
    fn style_live(
        &mut self,
        app: &mut AppState,
        track_id: &str,
        f: impl FnOnce(&mut crate::core::subtitle::SubtitleStyle),
    ) {
        if !self.slider_gesture {
            app.timeline.begin_subtitle_edit();
            self.slider_gesture = true;
        }
        app.timeline.subtitle_style_update_live(track_id, f);
    }

    /// Text-/Hex-Änderung: ein Snapshot pro Tipp-Sitzung.
    fn typed_live(&mut self, app: &mut AppState) {
        if !self.text_gesture {
            app.timeline.begin_subtitle_edit();
            self.text_gesture = true;
        }
    }

    /// Inline-Bearbeitung eines Segments starten (Caret ans Textende).
    fn start_edit(&mut self, app: &AppState, clip_id: &str) {
        let len = app
            .timeline
            .clip(clip_id)
            .and_then(|c| c.subtitle.as_ref())
            .map(|s| s.text.len())
            .unwrap_or(0);
        self.editing = Some(clip_id.to_string());
        self.editor.caret = len;
        self.request_focus = true;
        self.text_gesture = false;
    }
}

fn section(ui: &mut Ui, x: f32, y: f32, w: f32, label: &str) {
    ui.text(label, v2(x, y), theme::TEXT_3, FontKind::Sans12Medium);
    let lw = ui.font(FontKind::Sans12Medium).width(label);
    ui.hline(x + lw + 8.0, y + 8.0, (w - lw - 8.0).max(0.0), theme::LINE);
}

fn toggle_row(ui: &mut Ui, id_src: impl std::hash::Hash, rect: Rect, label: &str, on: bool) -> bool {
    let id = ui.id(id_src);
    let it = ui.interact(id, rect);
    let boxr = Rect::new(rect.x, rect.y + (rect.h - 16.0) / 2.0, 16.0, 16.0);
    ui.fill_rounded(
        boxr,
        theme::RADIUS_XS,
        if on { theme::ACCENT } else { theme::SURFACE_2 },
    );
    ui.stroke_rounded(boxr, theme::RADIUS_XS, 1.0, if on { theme::ACCENT } else { theme::LINE_STRONG });
    if on {
        ui.icon("check", boxr, 12.0, theme::WHITE);
    }
    let mut text_cell = rect;
    text_cell.cut_left(24.0);
    ui.text_left(label, text_cell, theme::TEXT_1, FontKind::Sans12);
    if it.hovered {
        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
    }
    it.clicked
}

/// Mehrzeiligen Text auf eine Anzeigezeile bringen (`⏎` als Trenner).
fn one_line(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ⏎ ")
}

impl Panel for SubtitlesPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);

        // Gesten beenden, sobald die Maus losgelassen bzw. der Fokus wandert.
        if !ui.input.left_down {
            self.slider_gesture = false;
        }
        if ui.persist.keyboard_focus != self.last_focus {
            self.last_focus = ui.persist.keyboard_focus;
            self.text_gesture = false;
        }

        let mut area = rect;

        // ------------------------------------------------------ Kopfzeile
        let toolbar = area.cut_top(TOOLBAR_H);
        ui.hline(toolbar.x, toolbar.bottom() - 1.0, toolbar.w, theme::LINE);
        let mut bar = toolbar.inset_xy(8.0, 0.0);

        let tracks: Vec<(String, String)> = app
            .timeline
            .subtitle_tracks()
            .iter()
            .map(|t| {
                (
                    t.id.clone(),
                    crate::core::timeline::track_name(t, &app.timeline.tracks),
                )
            })
            .collect();
        let active_track_id = app.timeline.active_subtitle_track().map(|t| t.id.clone());

        // Spurauswahl links.
        if !tracks.is_empty() {
            let labels: Vec<&str> = tracks.iter().map(|(_, n)| n.as_str()).collect();
            let current = tracks
                .iter()
                .position(|(id, _)| Some(id) == active_track_id.as_ref())
                .unwrap_or(0);
            let cell = Rect::new(bar.x, bar.y + (bar.h - 24.0) / 2.0, 72.0, 24.0);
            bar.cut_left(72.0 + 8.0);
            if let Some(idx) = select(ui, "subtitles.track", cell, &labels, current) {
                let id = tracks[idx].0.clone();
                app.timeline.set_active_subtitle_track(&id);
                self.editing = None;
            }
        }

        // Buttons rechts: Stil, Export, Import, Spur+, Segment+.
        struct BarBtn {
            icon: &'static str,
            tip: &'static str,
            command: &'static str,
            active: bool,
        }
        let buttons = [
            BarBtn {
                icon: "sliders-horizontal",
                tip: "Spurstil anzeigen",
                command: "",
                active: self.style_open,
            },
            BarBtn {
                icon: "file-output",
                tip: "Untertitel exportieren (SRT)…",
                command: "subtitle.exportSrt",
                active: false,
            },
            BarBtn {
                icon: "import",
                tip: "Untertitel importieren (SRT)…",
                command: "subtitle.importSrt",
                active: false,
            },
            BarBtn {
                icon: "list-video",
                tip: "Untertitelspur hinzufügen",
                command: "subtitle.addTrack",
                active: false,
            },
            BarBtn {
                icon: "plus",
                tip: "Untertitel am Playhead hinzufügen",
                command: "subtitle.addAtPlayhead",
                active: false,
            },
            BarBtn {
                icon: "scissors",
                tip: "Untertitel am Playhead teilen",
                command: "subtitle.split",
                active: false,
            },
            BarBtn {
                icon: "link-2",
                tip: "Untertitel mit Nachbarn zusammenführen",
                command: "subtitle.merge",
                active: false,
            },
        ];
        let mut bx = bar.right();
        for (i, b) in buttons.iter().enumerate() {
            bx -= 28.0;
            let btn = Rect::new(bx, bar.y + (bar.h - 28.0) / 2.0, 28.0, 28.0);
            let clicked = IconButton::new(b.icon)
                .active(b.active)
                .tooltip(b.tip)
                .show(ui, ("subtitles.bar", i), btn)
                .clicked;
            if clicked {
                if b.command.is_empty() {
                    self.style_open = !self.style_open;
                } else {
                    ui.run_command(b.command);
                }
            }
            bx -= 4.0;
        }

        // ------------------------------------------------------ Leerzustand
        let Some(track_id) = active_track_id else {
            let hint = area.center_box(280.0, 60.0);
            let mut h = hint;
            let top = h.cut_top(20.0);
            ui.text_centered(
                "Keine Untertitel-Spur vorhanden.",
                top,
                theme::TEXT_3,
                FontKind::Sans12,
            );
            ui.text_centered(
                "SRT importieren oder Spur über „+“ anlegen.",
                h,
                theme::TEXT_3,
                FontKind::Sans12,
            );
            return;
        };

        // ------------------------------------------------------- Spurstil
        if self.style_open {
            let style = app.timeline.subtitle_style(&track_id);
            let pad = 12.0;
            let style_h = 18.0 + (ROW_H + GAP) * 7.0 + 12.0;
            let panel = area.cut_top(style_h);
            let x = panel.x + pad;
            let w = panel.w - 2.0 * pad;
            let mut y = panel.y + 6.0;
            section(ui, x, y, w, "Spurstil");
            y += 18.0;

            // Schriftfamilie (fontconfig-Liste; „Standard" = leeres Feld).
            let families = list_font_families();
            let mut row = Rect::new(x, y, w, ROW_H);
            let label_cell = row.cut_left(88.0);
            ui.text_left("Schriftfamilie", label_cell, theme::TEXT_2, FontKind::Sans12);
            row.cut_left(8.0);
            let mut options: Vec<&str> = vec!["Standard (Sans)"];
            options.extend(families.iter().map(|f| f.as_str()));
            let family_idx = if style.font_family.is_empty() {
                0
            } else {
                families
                    .iter()
                    .position(|f| *f == style.font_family)
                    .map(|i| i + 1)
                    .unwrap_or(0)
            };
            let family_rect = Rect::new(row.x, y + 1.0, row.w, ROW_H - 2.0);
            if let Some(idx) = select(ui, "subtitles.family", family_rect, &options, family_idx) {
                let family = if idx == 0 {
                    String::new()
                } else {
                    families[idx - 1].clone()
                };
                app.timeline.subtitle_style_update(&track_id, move |s| s.font_family = family);
            }
            y += ROW_H + GAP;

            // Schriftschnitt (Gewicht).
            let mut row = Rect::new(x, y, w, ROW_H);
            let label_cell = row.cut_left(88.0);
            ui.text_left("Schriftschnitt", label_cell, theme::TEXT_2, FontKind::Sans12);
            row.cut_left(8.0);
            let weights: Vec<&str> = TitleWeight::ALL.iter().map(|wt| wt.label()).collect();
            let weight_idx = TitleWeight::ALL
                .iter()
                .position(|v| *v == style.weight)
                .unwrap_or(0);
            let weight_rect = Rect::new(row.x, y + 1.0, row.w, ROW_H - 2.0);
            if let Some(idx) = select(ui, "subtitles.weight", weight_rect, &weights, weight_idx) {
                let value = TitleWeight::ALL[idx];
                app.timeline.subtitle_style_update(&track_id, move |s| s.weight = value);
            }
            y += ROW_H + GAP;

            let mut size = style.size;
            slider_row(ui, "subtitles.size", Rect::new(x, y, w, ROW_H), "Schriftgröße", &mut size, 16.0, 120.0, 48.0);
            if size != style.size {
                self.style_live(app, &track_id, move |s| s.size = size);
            }
            y += ROW_H + GAP;

            // Position (Unten/Oben) + Feinjustage.
            let mut row = Rect::new(x, y, w, ROW_H);
            let label_cell = row.cut_left(88.0);
            ui.text_left("Position", label_cell, theme::TEXT_2, FontKind::Sans12);
            row.cut_left(8.0);
            let pos_labels: Vec<&str> = SUBTITLE_POS_PRESETS.iter().map(|(l, _)| *l).collect();
            let pos_idx = SUBTITLE_POS_PRESETS
                .iter()
                .position(|(_, v)| (v - style.pos_y).abs() < 1e-6)
                .unwrap_or(if style.pos_y >= 0.0 { 0 } else { 1 });
            let pos_rect = Rect::new(row.x, y + 1.0, row.w, ROW_H - 2.0);
            if let Some(idx) = select(ui, "subtitles.pos", pos_rect, &pos_labels, pos_idx) {
                let v = SUBTITLE_POS_PRESETS[idx].1;
                app.timeline.subtitle_style_update(&track_id, move |s| s.pos_y = v);
            }
            y += ROW_H + GAP;

            // Box (Hintergrund) + Deckkraft.
            let mut row = Rect::new(x, y, w, ROW_H);
            let toggle_cell = row.cut_left(96.0);
            if toggle_row(ui, "subtitles.bg", toggle_cell, "Box", style.bg_enabled) {
                let v = !style.bg_enabled;
                app.timeline.subtitle_style_update(&track_id, move |s| s.bg_enabled = v);
            }
            row.cut_left(8.0);
            let mut bg_op = style.bg_opacity;
            slider_row(ui, "subtitles.bgOp", row, "Deckkraft", &mut bg_op, 0.0, 100.0, 70.0);
            if bg_op != style.bg_opacity {
                self.style_live(app, &track_id, move |s| s.bg_opacity = bg_op);
            }
            y += ROW_H + GAP;

            let mut stroke = style.stroke_width;
            slider_row(ui, "subtitles.stroke", Rect::new(x, y, w, ROW_H), "Kontur", &mut stroke, 0.0, 12.0, 0.0);
            if stroke != style.stroke_width {
                self.style_live(app, &track_id, move |s| s.stroke_width = stroke);
            }
            y += ROW_H + GAP;

            // Farbe: Muster + Hex-Feld.
            let mut row = Rect::new(x, y, w, ROW_H);
            let label_cell = row.cut_left(88.0);
            ui.text_left("Schriftfarbe", label_cell, theme::TEXT_2, FontKind::Sans12);
            row.cut_left(8.0);
            let swatch = Rect::new(row.x, y + 3.0, 34.0, ROW_H - 6.0);
            ui.fill_rounded(
                swatch,
                theme::RADIUS_SM,
                raylib::color::Color::new(style.color.r, style.color.g, style.color.b, 255),
            );
            ui.stroke_rounded(swatch, theme::RADIUS_SM, 1.0, theme::LINE_STRONG);
            row.cut_left(34.0 + 8.0);
            let field = Rect::new(row.x, y + 1.0, row.w.min(96.0), ROW_H - 2.0);
            let hex_id = ui.id(("subtitles.hex", track_id.as_str()));
            if ui.persist.keyboard_focus != hex_id && self.hex_input.text != style.color.to_hex() {
                self.hex_input.set_text(style.color.to_hex());
            }
            let result = self
                .hex_input
                .show(ui, ("subtitles.hex", track_id.as_str()), field, "#rrggbb");
            if result.changed {
                if let Some(c) = RgbaColor::from_hex(&self.hex_input.text) {
                    self.typed_live(app);
                    app.timeline.subtitle_style_update_live(&track_id, move |s| s.color = c);
                }
            }
            ui.hline(panel.x, panel.bottom() - 1.0, panel.w, theme::LINE);
        }

        // ----------------------------------------------------- Segmentliste
        let seq = app.timeline.settings;
        let playhead = app.timeline.playhead_sec;
        let mut rows: Vec<SegmentRow> = app
            .timeline
            .clips
            .iter()
            .filter(|c| c.track_id == track_id)
            .filter_map(|c| {
                Some(SegmentRow {
                    clip_id: c.id.clone(),
                    start: c.start,
                    end: c.end(),
                    text: c.subtitle.as_ref()?.text.clone(),
                })
            })
            .collect();
        rows.sort_by(|a, b| a.start.total_cmp(&b.start));

        if rows.is_empty() {
            let hint = area.center_box(300.0, 40.0);
            ui.text_centered(
                "Keine Segmente — „+“ legt einen Untertitel am Playhead an.",
                hint,
                theme::TEXT_3,
                FontKind::Sans12,
            );
            return;
        }

        // Höhe je Zeile (der Editor braucht Platz für die Textzeilen).
        let editing_id = self.editing.clone();
        let editor_lines = |text: &str| text.split('\n').count().max(1) as f32;
        let row_height = move |row: &SegmentRow| -> f32 {
            if editing_id.as_deref() == Some(row.clip_id.as_str()) {
                18.0 + editor_lines(&row.text) * TEXTAREA_LINE_H + 16.0
            } else {
                SEGMENT_ROW_H
            }
        };
        let content_h: f32 = rows.iter().map(|r| row_height(r) + 2.0).sum::<f32>() + 8.0;
        let view = self.scroll.begin(ui, area, 0.0, content_h);
        let mut y = view.origin_y + 4.0;
        let x = view.viewport.x + 8.0;
        let w = view.viewport.w - 16.0;

        let selected_ids = app.timeline.selected_clip_ids.clone();
        let mut jump_to: Option<(String, f64)> = None;
        let mut begin_edit: Option<String> = None;
        let mut commit_text: Option<(String, String)> = None;
        let mut submit_from: Option<String> = None;

        for (idx, row) in rows.iter().enumerate() {
            let h = row_height(row);
            let r = Rect::new(x, y, w, h);
            y += h + 2.0;
            if r.bottom() < view.viewport.y || r.y > view.viewport.bottom() {
                continue;
            }
            let editing = self.editing.as_deref() == Some(row.clip_id.as_str());
            let selected = selected_ids.contains(&row.clip_id);
            let at_playhead = playhead >= row.start && playhead < row.end;

            let id = ui.id(("subtitles.row", row.clip_id.as_str()));
            let it = ui.interact(id, r);
            let bg = if selected {
                theme::SURFACE_3
            } else if it.hovered {
                theme::SURFACE_2
            } else {
                theme::SURFACE_1
            };
            ui.fill_rounded(r, theme::RADIUS_SM, bg);
            if at_playhead {
                // Segment unter dem Playhead: Akzentleiste links.
                ui.fill_rounded(Rect::new(r.x, r.y, 3.0, r.h), 1.5, theme::ACCENT);
            }

            // Kopfzeile: Index + In/Out-Timecode (links), Lesbarkeit (rechts).
            let mut head = Rect::new(r.x + 10.0, r.y + 3.0, r.w - 20.0, 14.0);
            // Lesbarkeit: Zeichen-pro-Sekunde + längste Zeile — rot über der
            // Profi-Norm (>17 CPS bzw. >42 Zeichen je Zeile).
            let dur = (row.end - row.start).max(0.0);
            let (cps, max_line, over) = readability(&row.text, dur);
            let badge = format!("{cps:.0} CPS · {max_line} Z.");
            let badge_cell = head.cut_right(116.0);
            ui.text_right(
                &badge,
                badge_cell,
                if over { theme::DANGER } else { theme::TEXT_3 },
                FontKind::Mono12,
            );
            let tc = format!(
                "{:>3}  {} → {}",
                idx + 1,
                format_sequence_timecode(row.start, &seq),
                format_sequence_timecode(row.end, &seq),
            );
            ui.text_left(&tc, head, theme::TEXT_3, FontKind::Mono12);

            if editing {
                let edit_rect = Rect::new(
                    r.x + 8.0,
                    r.y + 18.0,
                    r.w - 16.0,
                    editor_lines(&row.text) * TEXTAREA_LINE_H + 12.0,
                );
                let mut text = row.text.clone();
                if self.request_focus {
                    self.editor
                        .focus(ui, ("subtitles.editor", row.clip_id.as_str()), text.len());
                    self.request_focus = false;
                }
                let result = self.editor.show(
                    ui,
                    ("subtitles.editor", row.clip_id.as_str()),
                    edit_rect,
                    &mut text,
                );
                if result.changed {
                    commit_text = Some((row.clip_id.clone(), text.clone()));
                }
                if result.submitted {
                    submit_from = Some(row.clip_id.clone());
                } else if !result.focused && !self.request_focus {
                    // Fokus verloren (Esc/Klick daneben): Bearbeitung beenden.
                    self.editing = None;
                }
            } else {
                let body = Rect::new(r.x + 10.0, r.y + 18.0, r.w - 20.0, 18.0);
                let line = one_line(&row.text);
                let display = ui.font(FontKind::Sans12).ellipsize(&line, body.w);
                ui.text_left(&display, body, theme::TEXT_1, FontKind::Sans12);

                if it.double_clicked {
                    begin_edit = Some(row.clip_id.clone());
                } else if it.clicked {
                    jump_to = Some((row.clip_id.clone(), row.start));
                }
                if it.hovered {
                    ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                }
            }
        }

        self.scroll.end(ui, area, 0.0, content_h);

        // ---- Aktionen nach der Zeichenschleife (Borrow-Hygiene) ----
        if let Some((clip_id, start)) = jump_to {
            app.timeline.set_playhead(start);
            app.timeline
                .select_clips(&[clip_id], SelectMode::Replace, false);
        }
        if let Some(clip_id) = begin_edit {
            app.timeline
                .select_clips(std::slice::from_ref(&clip_id), SelectMode::Replace, false);
            self.start_edit(app, &clip_id);
        }
        if let Some((clip_id, text)) = commit_text {
            self.typed_live(app);
            app.timeline
                .subtitle_update_live(&clip_id, move |s| s.text = text);
        }
        if let Some(clip_id) = submit_from {
            // Enter: zum nächsten Segment springen und direkt weitertippen
            // (flüssiger Transkript-Workflow).
            let next = rows
                .iter()
                .position(|r| r.clip_id == clip_id)
                .and_then(|i| rows.get(i + 1));
            match next {
                Some(next) => {
                    app.timeline.set_playhead(next.start);
                    app.timeline
                        .select_clips(std::slice::from_ref(&next.clip_id), SelectMode::Replace, false);
                    let next_id = next.clip_id.clone();
                    self.start_edit(app, &next_id);
                }
                None => {
                    self.editing = None;
                    ui.persist.keyboard_focus = 0;
                }
            }
        }

        if ui.mouse_in(rect) && (ui.input.left_pressed || ui.input.right_pressed) {
            app.app.focused_panel = "subtitles".into();
        }
    }
}
