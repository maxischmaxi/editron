//! Farbe-Panel (Lumetri-Pendant): Basiskorrektur, Kreativ-Look, Lift/Gamma/
//! Gain-Farbräder und Vignette — wirkt auf die `ColorGrade` des Ziel-Clips
//! (erster ausgewählter Video-Clip, sonst oberster Video-Clip am Playhead).
//! Alle Änderungen laufen über den TimelineStore (undo-fähig: Drag-Gesten
//! legen EINEN Snapshot an, Einzelaktionen je einen), wirken live auf den
//! Programmmonitor (Grade-Shader), den Export und die Scopes und werden mit
//! dem Projekt gespeichert.

use crate::core::compose;
use crate::core::grade::{ColorGrade, GradeLook, WheelValue};
use crate::core::timeline::{TimelineClip, TrackKind};
use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::{v2, Rect};
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::select::select;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::widgets::{slider, IconButton};
use crate::ui::{FontKind, Ui};
use raylib::consts::MouseCursor;

const ROW_H: f32 = 26.0;
const SECTION_H: f32 = 32.0;

// --------------------------------------------------------- Generische Helfer
// (werden auch von anderen Panels genutzt — Signaturen stabil halten)

/// Einklappbarer Abschnitt-Header (h-8); liefert true, wenn offen.
pub fn section_header(
    ui: &mut Ui,
    id_src: impl std::hash::Hash,
    rect: Rect,
    title: &str,
    open: &mut bool,
) {
    let id = ui.id(id_src);
    let it = ui.interact(id, rect);
    if it.hovered {
        ui.fill(rect, theme::SURFACE_2);
        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
    }
    let mut inner = rect.inset_xy(8.0, 0.0);
    let chev = inner.cut_left(14.0);
    ui.icon(
        if *open { "chevron-down" } else { "chevron-right" },
        chev,
        14.0,
        theme::TEXT_3,
    );
    inner.cut_left(6.0);
    ui.text_left(title, inner, theme::TEXT_1, FontKind::Sans12Medium);
    if it.clicked {
        *open = !*open;
    }
}

/// Slider-Zeile: Label (w-22, Doppelklick = Reset) + Range + Wert (mono).
/// Generischer Helfer (Grafik-Panel); das Farbe-Panel nutzt `grade_slider_row`.
pub fn slider_row(
    ui: &mut Ui,
    id_src: impl std::hash::Hash,
    rect: Rect,
    label: &str,
    value: &mut f64,
    min: f64,
    max: f64,
    default_value: f64,
) {
    let mut inner = rect;
    let label_cell = inner.cut_left(88.0);
    let label_id = ui.id((&id_src, "label"));
    let label_it = ui.interact(label_id, label_cell);
    if label_it.double_clicked {
        *value = default_value;
    }
    let display = ui.font(FontKind::Sans12).ellipsize(label, label_cell.w);
    ui.text_left(&display, label_cell, theme::TEXT_2, FontKind::Sans12);
    inner.cut_left(8.0);
    let value_cell = inner.cut_right(36.0);
    inner.cut_right(8.0);
    slider(ui, (&id_src, "slider"), inner, value, min, max, theme::ACCENT);
    *value = value.round();
    ui.text_right(
        &format!("{}", *value as i64),
        value_cell,
        theme::TEXT_1,
        FontKind::Mono12,
    );
}

// ------------------------------------------------------------ Slider-Katalog

struct SliderDef {
    key: &'static str,
    label: &'static str,
    min: f64,
    max: f64,
    default: f64,
    decimals: usize,
    get: fn(&ColorGrade) -> f64,
    set: fn(&mut ColorGrade, f64),
}

static BASIC_SLIDERS: [SliderDef; 10] = [
    SliderDef { key: "temp", label: "Temperatur", min: -100.0, max: 100.0, default: 0.0, decimals: 0, get: |g| g.temperature, set: |g, v| g.temperature = v },
    SliderDef { key: "tint", label: "Farbton", min: -100.0, max: 100.0, default: 0.0, decimals: 0, get: |g| g.tint, set: |g, v| g.tint = v },
    SliderDef { key: "expo", label: "Belichtung", min: -5.0, max: 5.0, default: 0.0, decimals: 1, get: |g| g.exposure, set: |g, v| g.exposure = v },
    SliderDef { key: "contrast", label: "Kontrast", min: -100.0, max: 100.0, default: 0.0, decimals: 0, get: |g| g.contrast, set: |g, v| g.contrast = v },
    SliderDef { key: "highlights", label: "Lichter", min: -100.0, max: 100.0, default: 0.0, decimals: 0, get: |g| g.highlights, set: |g, v| g.highlights = v },
    SliderDef { key: "shadows", label: "Schatten", min: -100.0, max: 100.0, default: 0.0, decimals: 0, get: |g| g.shadows, set: |g, v| g.shadows = v },
    SliderDef { key: "whites", label: "Weiß", min: -100.0, max: 100.0, default: 0.0, decimals: 0, get: |g| g.whites, set: |g, v| g.whites = v },
    SliderDef { key: "blacks", label: "Schwarz", min: -100.0, max: 100.0, default: 0.0, decimals: 0, get: |g| g.blacks, set: |g, v| g.blacks = v },
    SliderDef { key: "sat", label: "Sättigung", min: 0.0, max: 200.0, default: 100.0, decimals: 0, get: |g| g.saturation, set: |g, v| g.saturation = v },
    SliderDef { key: "vibrance", label: "Dynamik", min: -100.0, max: 100.0, default: 0.0, decimals: 0, get: |g| g.vibrance, set: |g, v| g.vibrance = v },
];

static CREATIVE_SLIDERS: [SliderDef; 2] = [
    SliderDef { key: "intensity", label: "Intensität", min: 0.0, max: 100.0, default: 100.0, decimals: 0, get: |g| g.look_intensity, set: |g, v| g.look_intensity = v },
    SliderDef { key: "faded", label: "Verblasster Film", min: 0.0, max: 100.0, default: 0.0, decimals: 0, get: |g| g.faded_film, set: |g, v| g.faded_film = v },
];

static VIGNETTE_SLIDERS: [SliderDef; 4] = [
    SliderDef { key: "vigAmount", label: "Stärke", min: -100.0, max: 100.0, default: 0.0, decimals: 0, get: |g| g.vignette_amount, set: |g, v| g.vignette_amount = v },
    SliderDef { key: "vigMid", label: "Mittelpunkt", min: 0.0, max: 100.0, default: 50.0, decimals: 0, get: |g| g.vignette_midpoint, set: |g, v| g.vignette_midpoint = v },
    SliderDef { key: "vigRound", label: "Rundheit", min: -100.0, max: 100.0, default: 0.0, decimals: 0, get: |g| g.vignette_roundness, set: |g, v| g.vignette_roundness = v },
    SliderDef { key: "vigFeather", label: "Weiche Kante", min: 0.0, max: 100.0, default: 50.0, decimals: 0, get: |g| g.vignette_feather, set: |g, v| g.vignette_feather = v },
];

fn slider_def(key: &str) -> Option<&'static SliderDef> {
    BASIC_SLIDERS
        .iter()
        .chain(CREATIVE_SLIDERS.iter())
        .chain(VIGNETTE_SLIDERS.iter())
        .find(|d| d.key == key)
}

/// Zahl im deutschen Format (Komma) mit fester Nachkommastelle.
fn fmt_value(v: f64, decimals: usize) -> String {
    format!("{v:.decimals$}").replace('.', ",")
}

fn parse_value(s: &str) -> Option<f64> {
    s.trim().replace(',', ".").parse().ok()
}

// ----------------------------------------------------------------- Aktionen

enum Act {
    /// Laufende Geste (Snapshot einmalig pro Geste).
    LiveSet(&'static str, f64),
    /// Einzelaktion mit eigenem Snapshot (Reset, Eingabe bestätigt).
    Set(&'static str, f64),
    SetLook(GradeLook),
    /// Rad-Index (0 = Lift, 1 = Gamma, 2 = Gain) → neuer Wert (Geste).
    WheelLive(usize, WheelValue),
    /// Wie WheelLive, aber Einzelaktion mit eigenem Snapshot (Luma-Reset).
    WheelSet(usize, WheelValue),
    WheelReset(usize),
    ResetSection(u8),
    ResetAll,
    ToggleBypass,
    OpenEdit(&'static str, f64, usize),
    CommitEdit(&'static str, f64),
}

// ------------------------------------------------------------------- Panel

pub struct ColorPanel {
    /// Aktueller Ziel-Clip (Inline-Edit wird bei Wechsel verworfen).
    clip_id: Option<String>,
    open_basic: bool,
    open_creative: bool,
    open_wheels: bool,
    open_vignette: bool,
    scroll: ScrollState,
    /// Undo-Snapshot der laufenden Drag-Geste bereits angelegt.
    gesture_pushed: bool,
    /// Inline-Eingabe eines Werts: (Slider-Key, Feld).
    edit: Option<(&'static str, TextInputState)>,
}

impl Default for ColorPanel {
    fn default() -> Self {
        ColorPanel {
            clip_id: None,
            open_basic: true,
            open_creative: true,
            open_wheels: true,
            open_vignette: false,
            scroll: ScrollState::default(),
            gesture_pushed: false,
            edit: None,
        }
    }
}

/// Ziel-Clip der Farbkorrektur: erster ausgewählter Video-Clip (auch über
/// den verknüpften A/V-Partner), sonst oberster sichtbarer Video-Clip am
/// Playhead (Resolve-Verhalten). Liefert (Clip, kam-aus-Auswahl).
fn target_clip(app: &AppState) -> Option<(TimelineClip, bool)> {
    for id in &app.timeline.selected_clip_ids {
        let Some(clip) = app.timeline.clip(id) else { continue };
        if clip.kind == TrackKind::Video {
            return Some((clip.clone(), true));
        }
        // Audio-Teil ausgewählt → verknüpften Video-Partner graden.
        if let Some(link) = &clip.link_id {
            if let Some(video) = app.timeline.clips.iter().find(|c| {
                c.kind == TrackKind::Video && c.link_id.as_deref() == Some(link)
            }) {
                return Some((video.clone(), true));
            }
        }
    }
    compose::visible_video_clips(&app.timeline, app.timeline.playhead_sec)
        .last()
        .map(|c| ((*c).clone(), false))
}

impl ColorPanel {
    /// Slider-Zeile mit Undo-Gesten, Doppelklick-Reset (Label) und
    /// Doppelklick-Eingabe (Wert).
    fn grade_slider_row(
        &mut self,
        ui: &mut Ui,
        def: &'static SliderDef,
        rect: Rect,
        grade: &ColorGrade,
        acts: &mut Vec<Act>,
    ) {
        let mut inner = rect;
        let label_cell = inner.cut_left(96.0);
        let label_id = ui.id(("color.label", def.key));
        let label_it = ui.interact(label_id, label_cell);
        if label_it.hovered {
            ui.tooltip(label_id, label_cell, "Doppelklick setzt zurück");
        }
        let display = ui.font(FontKind::Sans12).ellipsize(def.label, label_cell.w);
        ui.text_left(&display, label_cell, theme::TEXT_2, FontKind::Sans12);
        if label_it.double_clicked {
            acts.push(Act::Set(def.key, def.default));
        }
        inner.cut_left(8.0);

        let value_cell = inner.cut_right(48.0);
        inner.cut_right(8.0);

        // ---- Slider (Geste) ----
        let old = (def.get)(grade);
        let mut v = old;
        let it = slider(ui, ("color.slider", def.key), inner, &mut v, def.min, def.max, theme::ACCENT);
        if def.decimals == 0 {
            v = v.round();
        } else {
            let f = 10f64.powi(def.decimals as i32);
            v = (v * f).round() / f;
        }
        if it.held && v != old {
            acts.push(Act::LiveSet(def.key, v));
        }

        // ---- Wert (Anzeige / Inline-Eingabe) ----
        let editing_here = matches!(&self.edit, Some((key, _)) if *key == def.key);
        if editing_here {
            let mut taken = self.edit.take().expect("edit state");
            let res = taken.1.show(ui, ("color.edit", def.key), value_cell, "");
            if res.submitted || !res.focused {
                if let Some(parsed) = parse_value(&taken.1.text) {
                    acts.push(Act::CommitEdit(def.key, parsed));
                }
                if res.submitted {
                    ui.persist.keyboard_focus = 0;
                }
            } else {
                self.edit = Some(taken);
            }
        } else {
            let vid = ui.id(("color.value", def.key));
            let vit = ui.interact(vid, value_cell);
            let non_default = old != def.default;
            ui.text_right(
                &fmt_value(old, def.decimals),
                value_cell,
                if non_default { theme::TEXT_1 } else { theme::TEXT_3 },
                FontKind::Mono12,
            );
            if vit.hovered {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_IBEAM);
                ui.tooltip(vid, value_cell, "Doppelklick zum Eingeben");
            }
            if vit.double_clicked {
                acts.push(Act::OpenEdit(def.key, old, def.decimals));
            }
        }
    }

    /// Farbrad mit Luma-Regler: Ziehen im Rad = Farb-Offset, Doppelklick =
    /// Reset; vertikaler Regler links daneben = Luma.
    #[allow(clippy::too_many_arguments)]
    fn wheel(
        &mut self,
        ui: &mut Ui,
        idx: usize,
        label: &str,
        cell: Rect,
        value: WheelValue,
        acts: &mut Vec<Act>,
    ) {
        let radius = (cell.w / 2.0 - 16.0).clamp(20.0, 34.0);
        let cx = cell.x + cell.w / 2.0 + 7.0; // Platz für den Luma-Regler links
        let cy = cell.y + radius + 6.0;

        // ---- Farbring ----
        let segments = 36;
        for s in 0..segments {
            let a0 = s as f32 / segments as f32 * 360.0;
            let a1 = (s + 1) as f32 / segments as f32 * 360.0;
            let color = raylib::color::Color::color_from_hsv(a0, 0.85, 0.9);
            ui.circle_sector(
                v2(cx, cy),
                radius,
                a0,
                a1,
                4,
                raylib::color::Color::new(color.r, color.g, color.b, 230),
            );
        }
        let inner_r = radius - 5.0;
        ui.circle(v2(cx, cy), inner_r, theme::SURFACE_2);

        // ---- Interaktion im Rad ----
        let hit = Rect::new(cx - radius, cy - radius, radius * 2.0, radius * 2.0);
        let id = ui.id(("color.wheel", idx));
        let it = ui.interact(id, hit);
        if it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_CROSSHAIR);
            ui.tooltip(id, hit, "Ziehen färbt — Doppelklick setzt zurück");
        }
        if it.double_clicked {
            acts.push(Act::WheelReset(idx));
        } else if it.held {
            let dx = ((ui.input.mouse.x - cx) / inner_r) as f64;
            let dy = ((ui.input.mouse.y - cy) / inner_r) as f64;
            let len = (dx * dx + dy * dy).sqrt();
            let scale = if len > 1.0 { 1.0 / len } else { 1.0 };
            let next = WheelValue {
                x: dx * scale,
                y: dy * scale,
                luma: value.luma,
            };
            if next != value {
                acts.push(Act::WheelLive(idx, next));
            }
        }

        // ---- Marker (Fadenkreuz + Punkt an der Offset-Position) ----
        let mx = cx + (value.x as f32) * inner_r;
        let my = cy + (value.y as f32) * inner_r;
        ui.line_thin(v2(cx - inner_r, cy), v2(cx + inner_r, cy), theme::with_alpha(theme::LINE_STRONG, 120));
        ui.line_thin(v2(cx, cy - inner_r), v2(cx, cy + inner_r), theme::with_alpha(theme::LINE_STRONG, 120));
        let active = value.x != 0.0 || value.y != 0.0;
        ui.circle(
            v2(mx, my),
            3.5,
            if active { theme::WHITE } else { theme::TEXT_2 },
        );
        ui.circle_outline(v2(mx, my), 4.5, theme::SURFACE_0);

        // ---- Luma-Regler (vertikal, links vom Rad) ----
        let track_h = radius * 2.0 - 4.0;
        let track = Rect::new(cx - radius - 14.0, cy - track_h / 2.0, 8.0, track_h);
        let lid = ui.id(("color.wheel.luma", idx));
        let lit = ui.interact(lid, track);
        ui.fill_rounded(track, 3.0, theme::SURFACE_4);
        if lit.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_NS);
            ui.tooltip(lid, track, "Luma — Doppelklick setzt zurück");
        }
        if lit.double_clicked {
            if value.luma != 0.0 {
                acts.push(Act::WheelSet(idx, WheelValue { luma: 0.0, ..value }));
            }
        } else if lit.held {
            let t = ((track.bottom() - ui.input.mouse.y) / track.h).clamp(0.0, 1.0) as f64;
            let luma = (t * 2.0 - 1.0).clamp(-1.0, 1.0);
            let next = WheelValue { luma, ..value };
            if next != value {
                acts.push(Act::WheelLive(idx, next));
            }
        }
        // Thumb
        let t = ((value.luma + 1.0) / 2.0) as f32;
        let ty = track.bottom() - t * track.h;
        let thumb = Rect::new(track.x - 2.0, ty - 2.5, track.w + 4.0, 5.0);
        ui.fill_rounded(
            thumb,
            2.0,
            if value.luma != 0.0 { theme::ACCENT } else { theme::TEXT_2 },
        );

        // ---- Label ----
        ui.text_centered(
            label,
            Rect::new(cell.x, cy + radius + 4.0, cell.w, 16.0),
            theme::TEXT_3,
            FontKind::Sans12,
        );
    }
}

impl Panel for ColorPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);
        if ui.mouse_in(rect) && (ui.input.left_pressed || ui.input.right_pressed) {
            app.app.focused_panel = "color".into();
        }
        // Gesten-Snapshot-Tracking: Maus losgelassen ⇒ nächste Geste pusht neu.
        if !ui.input.left_down {
            self.gesture_pushed = false;
        }

        // ---- Ziel-Clip ----
        let Some((clip, from_selection)) = target_clip(app) else {
            *self = ColorPanel {
                scroll: std::mem::take(&mut self.scroll),
                ..Default::default()
            };
            let hint = rect.center_box(280.0, 60.0);
            let mut c = hint;
            let ic = c.cut_top(22.0);
            ui.icon("palette", ic, 20.0, theme::TEXT_3);
            c.cut_top(6.0);
            ui.text_centered(
                "Clip auswählen oder Playhead über einen Clip bewegen.",
                c,
                theme::TEXT_3,
                FontKind::Sans12,
            );
            return;
        };
        if self.clip_id.as_deref() != Some(clip.id.as_str()) {
            self.clip_id = Some(clip.id.clone());
            self.edit = None;
        }
        let grade = clip.grade.clone();

        let mut area = rect;

        // ---- Kopfzeile: Clipname + Bypass + Alles zurücksetzen ----
        let head = area.cut_top(36.0);
        ui.hline(head.x, head.bottom() - 1.0, head.w, theme::LINE);
        let mut head_inner = head.inset_xy(12.0, 0.0);
        let reset_rect = Rect::new(head_inner.right() - 26.0, head.y + 5.0, 26.0, 26.0);
        let bypass_rect = Rect::new(head_inner.right() - 56.0, head.y + 5.0, 26.0, 26.0);
        head_inner.cut_right(64.0);
        let name = ui
            .font(FontKind::Sans12Medium)
            .ellipsize(&clip.name, head_inner.w);
        ui.text_left(&name, head_inner, theme::TEXT_1, FontKind::Sans12Medium);

        let mut acts: Vec<Act> = Vec::new();
        if IconButton::new(if grade.enabled { "eye" } else { "eye-off" })
            .size(15.0)
            .active(!grade.enabled)
            .tooltip(if grade.enabled {
                "Farbkorrektur umgehen (Bypass)"
            } else {
                "Farbkorrektur wieder aktivieren"
            })
            .show(ui, "color.bypass", bypass_rect)
            .clicked
        {
            acts.push(Act::ToggleBypass);
        }
        if IconButton::new("rotate-ccw")
            .size(15.0)
            .disabled(grade.is_default())
            .tooltip("Alle Korrekturen zurücksetzen")
            .show(ui, "color.resetAll", reset_rect)
            .clicked
        {
            acts.push(Act::ResetAll);
        }

        // ---- Fußzeile: Ziel-Hinweis ----
        let footer = area.cut_bottom(25.0);
        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        let src = if from_selection { "Auswahl" } else { "Playhead" };
        let note = ui.font(FontKind::Sans12).ellipsize(
            &format!("Wirkt auf: {} ({src})", clip.name),
            footer.w - 24.0,
        );
        ui.text_left(&note, footer.inset_xy(12.0, 0.0), theme::TEXT_3, FontKind::Sans12);

        // ---- Inhalt (Scroll) ----
        let wheel_h = 110.0;
        let basic_h = if self.open_basic { BASIC_SLIDERS.len() as f32 * ROW_H + 8.0 } else { 0.0 };
        let creative_h = if self.open_creative { ROW_H * 3.0 + 8.0 } else { 0.0 };
        let wheels_h = if self.open_wheels { wheel_h + 8.0 } else { 0.0 };
        let vignette_h = if self.open_vignette { VIGNETTE_SLIDERS.len() as f32 * ROW_H + 8.0 } else { 0.0 };
        let content_h = SECTION_H * 4.0 + basic_h + creative_h + wheels_h + vignette_h + 8.0;

        let view = self.scroll.begin(ui, area, 0.0, content_h);
        let w = view.viewport.w;
        let x = view.viewport.x;
        let mut y = view.origin_y;

        let section_reset = |ui: &mut Ui, key: &'static str, y: f32, enabled: bool| -> bool {
            let r = Rect::new(x + w - 36.0, y + 5.0, 22.0, 22.0);
            IconButton::new("rotate-ccw")
                .size(13.0)
                .disabled(!enabled)
                .tooltip("Abschnitt zurücksetzen")
                .show(ui, (key, "reset"), r)
                .clicked
        };
        let d = ColorGrade::default();

        // ================= Basiskorrektur =================
        section_header(ui, "color.sec.basic", Rect::new(x, y, w, SECTION_H), "Basiskorrektur", &mut self.open_basic);
        let basic_dirty = BASIC_SLIDERS.iter().any(|s| (s.get)(&grade) != s.default);
        if section_reset(ui, "color.sec.basic", y, basic_dirty) {
            acts.push(Act::ResetSection(0));
        }
        y += SECTION_H;
        if self.open_basic {
            for def in BASIC_SLIDERS.iter() {
                let r = Rect::new(x + 8.0, y, w - 16.0, ROW_H - 2.0);
                self.grade_slider_row(ui, def, r, &grade, &mut acts);
                y += ROW_H;
            }
            y += 8.0;
        }
        ui.hline(x, y - 1.0, w, theme::LINE);

        // ================= Kreativ =================
        section_header(ui, "color.sec.creative", Rect::new(x, y, w, SECTION_H), "Kreativ", &mut self.open_creative);
        let creative_dirty = grade.look != d.look
            || grade.look_intensity != d.look_intensity
            || grade.faded_film != d.faded_film;
        if section_reset(ui, "color.sec.creative", y, creative_dirty) {
            acts.push(Act::ResetSection(1));
        }
        y += SECTION_H;
        if self.open_creative {
            // Look-Auswahl
            let row = Rect::new(x + 8.0, y, w - 16.0, ROW_H - 2.0);
            let mut inner = row;
            let label_cell = inner.cut_left(96.0);
            ui.text_left("Look", label_cell, theme::TEXT_2, FontKind::Sans12);
            inner.cut_left(8.0);
            let labels: Vec<&str> = GradeLook::ALL.iter().map(|l| l.label()).collect();
            let current = GradeLook::ALL.iter().position(|l| *l == grade.look).unwrap_or(0);
            if let Some(idx) = select(ui, "color.look", inner, &labels, current) {
                acts.push(Act::SetLook(GradeLook::ALL[idx]));
            }
            y += ROW_H;
            for def in CREATIVE_SLIDERS.iter() {
                let r = Rect::new(x + 8.0, y, w - 16.0, ROW_H - 2.0);
                self.grade_slider_row(ui, def, r, &grade, &mut acts);
                y += ROW_H;
            }
            y += 8.0;
        }
        ui.hline(x, y - 1.0, w, theme::LINE);

        // ================= Farbräder =================
        section_header(ui, "color.sec.wheels", Rect::new(x, y, w, SECTION_H), "Farbräder", &mut self.open_wheels);
        let wheels_dirty = !grade.lift.is_zero() || !grade.gamma.is_zero() || !grade.gain.is_zero();
        if section_reset(ui, "color.sec.wheels", y, wheels_dirty) {
            acts.push(Act::ResetSection(2));
        }
        y += SECTION_H;
        if self.open_wheels {
            let cell_w = w / 3.0;
            let wheels = [
                ("Schatten", grade.lift),
                ("Mitteltöne", grade.gamma),
                ("Lichter", grade.gain),
            ];
            for (i, (label, value)) in wheels.iter().enumerate() {
                let cell = Rect::new(x + cell_w * i as f32, y, cell_w, wheel_h);
                self.wheel(ui, i, label, cell, *value, &mut acts);
            }
            y += wheel_h + 8.0;
        }
        ui.hline(x, y - 1.0, w, theme::LINE);

        // ================= Vignette =================
        section_header(ui, "color.sec.vignette", Rect::new(x, y, w, SECTION_H), "Vignette", &mut self.open_vignette);
        let vignette_dirty = VIGNETTE_SLIDERS.iter().any(|s| (s.get)(&grade) != s.default);
        if section_reset(ui, "color.sec.vignette", y, vignette_dirty) {
            acts.push(Act::ResetSection(3));
        }
        y += SECTION_H;
        if self.open_vignette {
            for def in VIGNETTE_SLIDERS.iter() {
                let r = Rect::new(x + 8.0, y, w - 16.0, ROW_H - 2.0);
                self.grade_slider_row(ui, def, r, &grade, &mut acts);
                y += ROW_H;
            }
        }

        self.scroll.end(ui, area, 0.0, content_h);

        // ---- Aktionen anwenden ----
        let id = clip.id.clone();
        for act in acts {
            match act {
                Act::LiveSet(key, v) => {
                    let Some(def) = slider_def(key) else { continue };
                    if !self.gesture_pushed {
                        app.timeline.begin_fx_edit();
                        self.gesture_pushed = true;
                    }
                    let v = v.clamp(def.min, def.max);
                    app.timeline.grade_update_live(&id, |g| (def.set)(g, v));
                }
                Act::Set(key, v) => {
                    let Some(def) = slider_def(key) else { continue };
                    let v = v.clamp(def.min, def.max);
                    if (def.get)(&grade) != v {
                        app.timeline.grade_update(&id, |g| (def.set)(g, v));
                    }
                }
                Act::CommitEdit(key, v) => {
                    self.edit = None;
                    let Some(def) = slider_def(key) else { continue };
                    let v = v.clamp(def.min, def.max);
                    if (def.get)(&grade) != v {
                        app.timeline.grade_update(&id, |g| (def.set)(g, v));
                    }
                }
                Act::OpenEdit(key, value, decimals) => {
                    let mut state = TextInputState::default();
                    state.set_text(fmt_value(value, decimals));
                    let edit_id = ui.id(("color.edit", key));
                    ui.persist.keyboard_focus = edit_id;
                    self.edit = Some((key, state));
                }
                Act::SetLook(look) => {
                    if grade.look != look {
                        app.timeline.grade_update(&id, |g| g.look = look);
                    }
                }
                Act::WheelLive(idx, value) => {
                    if !self.gesture_pushed {
                        app.timeline.begin_fx_edit();
                        self.gesture_pushed = true;
                    }
                    app.timeline.grade_update_live(&id, |g| match idx {
                        0 => g.lift = value,
                        1 => g.gamma = value,
                        _ => g.gain = value,
                    });
                }
                Act::WheelSet(idx, value) => {
                    app.timeline.grade_update(&id, |g| match idx {
                        0 => g.lift = value,
                        1 => g.gamma = value,
                        _ => g.gain = value,
                    });
                }
                Act::WheelReset(idx) => {
                    let current = match idx {
                        0 => grade.lift,
                        1 => grade.gamma,
                        _ => grade.gain,
                    };
                    if current != WheelValue::default() {
                        app.timeline.grade_update(&id, |g| match idx {
                            0 => g.lift = WheelValue::default(),
                            1 => g.gamma = WheelValue::default(),
                            _ => g.gain = WheelValue::default(),
                        });
                    }
                }
                Act::ResetSection(section) => {
                    app.timeline.grade_update(&id, |g| match section {
                        0 => {
                            g.temperature = 0.0;
                            g.tint = 0.0;
                            g.exposure = 0.0;
                            g.contrast = 0.0;
                            g.highlights = 0.0;
                            g.shadows = 0.0;
                            g.whites = 0.0;
                            g.blacks = 0.0;
                            g.saturation = 100.0;
                            g.vibrance = 0.0;
                        }
                        1 => {
                            g.look = GradeLook::Neutral;
                            g.look_intensity = 100.0;
                            g.faded_film = 0.0;
                        }
                        2 => {
                            g.lift = WheelValue::default();
                            g.gamma = WheelValue::default();
                            g.gain = WheelValue::default();
                        }
                        _ => {
                            g.vignette_amount = 0.0;
                            g.vignette_midpoint = 50.0;
                            g.vignette_roundness = 0.0;
                            g.vignette_feather = 50.0;
                        }
                    });
                }
                Act::ResetAll => {
                    app.timeline.grade_reset(std::slice::from_ref(&id));
                }
                Act::ToggleBypass => {
                    app.timeline.grade_toggle_enabled(&id);
                }
            }
        }
    }
}
