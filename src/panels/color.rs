//! Farbe-Panel (Lumetri-Pendant): Basiskorrektur, Kreativ-Look, Lift/Gamma/
//! Gain-Farbräder und Vignette — wirkt auf die `ColorGrade` des Ziel-Clips
//! (erster ausgewählter Video-Clip, sonst oberster Video-Clip am Playhead).
//! Alle Änderungen laufen über den TimelineStore (undo-fähig: Drag-Gesten
//! legen EINEN Snapshot an, Einzelaktionen je einen), wirken live auf den
//! Programmmonitor (Grade-Shader), den Export und die Scopes und werden mit
//! dem Projekt gespeichert.

use crate::core::compose;
use crate::core::grade::{ColorGrade, Curve, CurvePoint, GradeCurves, GradeLook, LutSlot, WheelValue};
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
/// Höhe (= Breite, quadratisch) der Kurven-Editor-Fläche.
const CURVE_EDITOR_H: f32 = 150.0;

/// Kanalfarbe für die Kurven-UI (0 = Luma/neutral, 1 = R, 2 = G, 3 = B).
fn channel_color(ch: usize) -> raylib::color::Color {
    match ch {
        1 => raylib::color::Color::new(232, 96, 96, 255),
        2 => raylib::color::Color::new(96, 210, 120, 255),
        3 => raylib::color::Color::new(110, 150, 255, 255),
        _ => theme::TEXT_1,
    }
}

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
    /// Laufende Kurven-Geste (Punkt ziehen): Kanal-Index + neue Kurve.
    CurveLive(usize, Curve),
    /// Einzelaktion (Punkt hinzufügen/löschen): Kanal-Index + neue Kurve.
    CurveSet(usize, Curve),
    /// Kanal-Kurve zurücksetzen (Identität).
    CurveReset(usize),
    /// LUT-Datei für einen Slot wählen (true = Input, false = Look).
    PickLut(bool),
    /// LUT-Slot leeren.
    RemoveLut(bool),
    /// LUT-Stärke ziehen (laufende Geste).
    LutStrengthLive(bool, f64),
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
    open_curves: bool,
    open_luts: bool,
    open_wheels: bool,
    open_vignette: bool,
    /// Aktiver Kurven-Kanal (0 = Luma, 1 = Rot, 2 = Grün, 3 = Blau).
    curve_channel: usize,
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
            open_curves: true,
            open_luts: true,
            open_wheels: true,
            open_vignette: false,
            curve_channel: 0,
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

    /// Segmentierte Kanal-Auswahl (Luma/R/G/B). Nicht-neutrale Kanäle werden
    /// in ihrer Kanalfarbe hervorgehoben.
    fn curve_channel_selector(&mut self, ui: &mut Ui, row: Rect, curves: &GradeCurves) {
        const LABELS: [&str; 4] = ["Luma", "Rot", "Grün", "Blau"];
        let cw = row.w / 4.0;
        for i in 0..4 {
            let cell = Rect::new(row.x + cw * i as f32, row.y, cw - 3.0, row.h);
            let id = ui.id(("color.curve.chan", i));
            let it = ui.interact(id, cell);
            let selected = self.curve_channel == i;
            let edited = !curves.channel(i).is_identity();
            let bg = if selected {
                theme::SURFACE_3
            } else if it.hovered {
                theme::SURFACE_2
            } else {
                theme::SURFACE_0
            };
            ui.fill_rounded(cell, 3.0, bg);
            let col = if selected || edited {
                channel_color(i)
            } else {
                theme::TEXT_2
            };
            ui.text_centered(LABELS[i], cell, col, FontKind::Sans12);
            if it.hovered {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            }
            if it.clicked {
                self.curve_channel = i;
            }
        }
    }

    /// Kurven-Editor des aktiven Kanals: Raster + Diagonale, Spline-Vorschau,
    /// ziehbare Stützpunkte. Klick auf freie Fläche fügt einen Punkt hinzu,
    /// Doppelklick auf einen inneren Punkt löscht ihn; Endpunkte bleiben in x
    /// verankert (nur y verstellbar).
    fn curve_editor(&mut self, ui: &mut Ui, ed: Rect, grade: &ColorGrade, acts: &mut Vec<Act>) {
        let ch = self.curve_channel;
        let curve = grade.curves.channel(ch).clone();
        let line_col = channel_color(ch);

        ui.fill_rounded(ed, 4.0, theme::SURFACE_0);
        // Viertel-Raster.
        for i in 1..4 {
            let gx = (ed.x + ed.w * i as f32 / 4.0).round();
            ui.vline(gx, ed.y, ed.h, theme::with_alpha(theme::LINE, 50));
            let gy = (ed.y + ed.h * i as f32 / 4.0).round();
            ui.hline(ed.x, gy, ed.w, theme::with_alpha(theme::LINE, 50));
        }
        ui.fill(Rect::new(ed.x, ed.y, ed.w, 1.0), theme::with_alpha(theme::LINE, 90));
        // Rahmen + Identitäts-Diagonale.
        ui.line_thin(
            v2(ed.x, ed.bottom()),
            v2(ed.right(), ed.y),
            theme::with_alpha(theme::LINE_STRONG, 70),
        );

        let to_screen = |x: f64, y: f64| {
            v2(
                ed.x + (x as f32) * ed.w,
                ed.bottom() - (y as f32) * ed.h,
            )
        };

        // Spline-Linienzug (pixelweise abtasten).
        let steps = (ed.w as usize).max(2);
        let mut prev = to_screen(0.0, curve.eval(0.0));
        for s in 1..=steps {
            let x = s as f64 / steps as f64;
            let p = to_screen(x, curve.eval(x));
            ui.line(prev, p, 1.5, line_col);
            prev = p;
        }

        // ---- Stützpunkte: ziehen / löschen ----
        let n = curve.points.len();
        let hit_r = 8.0;
        let mut hovered_any = false;
        for i in 0..n {
            let pt = curve.points[i];
            let is_endpoint = i == 0 || i + 1 == n;
            let sp = to_screen(pt.x, pt.y);
            let hit = Rect::new(sp.x - hit_r, sp.y - hit_r, hit_r * 2.0, hit_r * 2.0);
            let id = ui.id(("color.curve.pt", ch, i));
            let it = ui.interact(id, hit);
            if it.hovered {
                hovered_any = true;
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_CROSSHAIR);
            }
            if it.double_clicked && !is_endpoint {
                let mut pts = curve.points.clone();
                pts.remove(i);
                acts.push(Act::CurveSet(ch, Curve { points: pts }));
            } else if it.held {
                let mut nx = ((ui.input.mouse.x - ed.x) / ed.w).clamp(0.0, 1.0) as f64;
                let ny = ((ed.bottom() - ui.input.mouse.y) / ed.h).clamp(0.0, 1.0) as f64;
                if is_endpoint {
                    nx = pt.x; // Endpunkte in x verankert
                } else {
                    // Zwischen die Nachbarn klemmen. Liegen diese enger als der
                    // doppelte Mindestabstand (z. B. dicht gepackte Punkte aus
                    // einer Datei), wäre lo > hi — `f64::clamp` würde paniken,
                    // daher in dem Fall mittig zwischen die Nachbarn setzen.
                    let lo = curve.points[i - 1].x + 1e-3;
                    let hi = curve.points[i + 1].x - 1e-3;
                    nx = if lo <= hi {
                        nx.clamp(lo, hi)
                    } else {
                        (curve.points[i - 1].x + curve.points[i + 1].x) * 0.5
                    };
                }
                let mut pts = curve.points.clone();
                pts[i] = CurvePoint { x: nx, y: ny };
                let cand = Curve { points: pts };
                if cand != curve {
                    acts.push(Act::CurveLive(ch, cand));
                }
            }
            // Marker.
            let big = it.hovered || it.held;
            ui.circle(sp, if big { 5.0 } else { 3.5 }, if big { theme::WHITE } else { line_col });
            ui.circle_outline(sp, if big { 6.0 } else { 4.5 }, theme::SURFACE_0);
        }

        // ---- Freie Fläche: Klick fügt einen Punkt hinzu ----
        let bg_id = ui.id(("color.curve.bg", ch));
        let bg = ui.interact(bg_id, ed);
        if bg.hovered && !hovered_any {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_CROSSHAIR);
        }
        if bg.clicked && !hovered_any {
            let nx = ((ui.input.mouse.x - ed.x) / ed.w).clamp(0.0, 1.0) as f64;
            let ny = ((ed.bottom() - ui.input.mouse.y) / ed.h).clamp(0.0, 1.0) as f64;
            let mut pts = curve.points.clone();
            let idx = pts.iter().position(|p| p.x > nx).unwrap_or(pts.len());
            // Nicht zu nah an einem Nachbarn einfügen (strikt steigendes x).
            let too_close = (idx > 0 && (nx - pts[idx - 1].x).abs() < 5e-3)
                || (idx < pts.len() && (pts[idx].x - nx).abs() < 5e-3);
            if !too_close {
                pts.insert(idx, CurvePoint { x: nx, y: ny });
                acts.push(Act::CurveSet(ch, Curve { points: pts }));
            }
        }
    }

    /// Ein LUT-Slot (Input oder Look): Statuszeile (Dateiname / „Keine" /
    /// Offline-Warnung) + Wählen/Entfernen, darunter ein Stärke-Regler (nur
    /// bei gesetzter LUT). `offline` = referenzierte Datei fehlt/ungültig.
    #[allow(clippy::too_many_arguments)]
    fn lut_slot_ui(
        &mut self,
        ui: &mut Ui,
        input: bool,
        label: &str,
        slot: &Option<LutSlot>,
        offline: bool,
        rect: Rect,
        acts: &mut Vec<Act>,
    ) {
        let mut r = rect;
        let mut row = r.cut_top(ROW_H - 2.0);
        let label_cell = row.cut_left(60.0);
        ui.text_left(label, label_cell, theme::TEXT_2, FontKind::Sans12);
        row.cut_left(6.0);
        // Rechts: Wählen-Button, davor (falls gesetzt) Entfernen.
        let choose = Rect::new(row.right() - 24.0, row.y, 24.0, 22.0);
        row.cut_right(28.0);
        let remove = if slot.is_some() {
            let rr = Rect::new(row.right() - 24.0, row.y, 24.0, 22.0);
            row.cut_right(28.0);
            Some(rr)
        } else {
            None
        };
        // Statuszeile.
        let (text, col): (String, _) = match slot {
            Some(s) if offline => (
                format!("fehlt: {}", slot_display(s)),
                theme::WARNING,
            ),
            Some(s) => (slot_display(s), theme::TEXT_1),
            None => ("Keine".to_string(), theme::TEXT_3),
        };
        if matches!(slot, Some(_) if offline) {
            let ic = row.cut_left(16.0);
            ui.icon("triangle-alert", ic.inset_xy(0.0, 4.0), 13.0, theme::WARNING);
            row.cut_left(2.0);
        }
        let disp = ui.font(FontKind::Sans12).ellipsize(&text, row.w);
        ui.text_left(&disp, row, col, FontKind::Sans12);

        if IconButton::new("folder-open")
            .size(14.0)
            .tooltip(if offline {
                "Andere LUT-Datei suchen (.cube)"
            } else {
                "LUT-Datei wählen (.cube)"
            })
            .show(ui, ("color.lut.pick", input), choose)
            .clicked
        {
            acts.push(Act::PickLut(input));
        }
        if let Some(rr) = remove {
            if IconButton::new("trash-2")
                .size(14.0)
                .tooltip("LUT entfernen")
                .show(ui, ("color.lut.remove", input), rr)
                .clicked
            {
                acts.push(Act::RemoveLut(input));
            }
        }
        // Stärke-Regler (nur bei gesetzter LUT).
        if let Some(s) = slot {
            let mut sr = r.cut_top(ROW_H - 4.0);
            let lc = sr.cut_left(60.0);
            ui.text_left("Stärke", lc, theme::TEXT_3, FontKind::Sans12);
            sr.cut_left(6.0);
            let vc = sr.cut_right(40.0);
            sr.cut_right(8.0);
            let mut v = s.strength;
            let it = slider(ui, ("color.lut.str", input), sr, &mut v, 0.0, 100.0, theme::ACCENT);
            v = v.round();
            if it.held && v != s.strength {
                acts.push(Act::LutStrengthLive(input, v));
            }
            ui.text_right(
                &format!("{}", v as i64),
                vc,
                theme::TEXT_1,
                FontKind::Mono12,
            );
        }
    }
}

/// Anzeigename eines LUT-Slots (Name, sonst Datei-Endknoten des Pfads).
fn slot_display(s: &LutSlot) -> String {
    if !s.name.is_empty() {
        s.name.clone()
    } else {
        std::path::Path::new(&s.path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| s.path.clone())
    }
}

impl Panel for ColorPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, services: &Services, rect: Rect) {
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
        let paste_rect = Rect::new(head_inner.right() - 86.0, head.y + 5.0, 26.0, 26.0);
        let copy_rect = Rect::new(head_inner.right() - 116.0, head.y + 5.0, 26.0, 26.0);
        head_inner.cut_right(124.0);
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
        // Grade kopieren/einfügen (sequenzübergreifend via AppState-Klemmbrett).
        // Beide wirken über die registrierten Commands auf die Auswahl; nur
        // sinnvoll, wenn der gezeigte Clip wirklich ausgewählt ist (nicht bloß
        // unter dem Playhead).
        if IconButton::new("copy")
            .size(15.0)
            .disabled(!from_selection)
            .tooltip(if from_selection {
                "Farbkorrektur kopieren"
            } else {
                "Zum Kopieren einen Clip auswählen"
            })
            .show(ui, "color.copyGrade", copy_rect)
            .clicked
        {
            ui.run_command("color.copyGrade");
        }
        let can_paste = from_selection && app.grade_clipboard.is_some();
        if IconButton::new("clipboard-paste")
            .size(15.0)
            .disabled(!can_paste)
            .tooltip(if app.grade_clipboard.is_none() {
                "Kein kopierter Grade im Klemmbrett"
            } else if from_selection {
                "Farbkorrektur einfügen"
            } else {
                "Zum Einfügen einen Clip auswählen"
            })
            .show(ui, "color.pasteGrade", paste_rect)
            .clicked
        {
            ui.run_command("color.pasteGrade");
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

        // Offline-Status der LUT-Slots (referenzierte Datei fehlt/ungültig) —
        // einmal auflösen (lädt bei Bedarf in den Cache).
        let input_offline = grade
            .input_lut
            .as_ref()
            .is_some_and(|s| !s.path.is_empty() && app.luts.get_or_load(&s.path).is_err());
        let look_offline = grade
            .look_lut
            .as_ref()
            .is_some_and(|s| !s.path.is_empty() && app.luts.get_or_load(&s.path).is_err());

        // ---- Inhalt (Scroll) ----
        let wheel_h = 110.0;
        let basic_h = if self.open_basic { BASIC_SLIDERS.len() as f32 * ROW_H + 8.0 } else { 0.0 };
        let creative_h = if self.open_creative { ROW_H * 3.0 + 8.0 } else { 0.0 };
        let curves_h = if self.open_curves { ROW_H * 2.0 + CURVE_EDITOR_H + 14.0 } else { 0.0 };
        let lut_rows = |slot: &Option<LutSlot>| if slot.is_some() { 2.0 } else { 1.0 };
        let luts_h = if self.open_luts {
            (lut_rows(&grade.input_lut) + lut_rows(&grade.look_lut)) * ROW_H + 14.0
        } else {
            0.0
        };
        let wheels_h = if self.open_wheels { wheel_h + 8.0 } else { 0.0 };
        let vignette_h = if self.open_vignette { VIGNETTE_SLIDERS.len() as f32 * ROW_H + 8.0 } else { 0.0 };
        let content_h = SECTION_H * 6.0
            + basic_h
            + creative_h
            + curves_h
            + luts_h
            + wheels_h
            + vignette_h
            + 8.0;

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

        // ================= Kurven =================
        section_header(ui, "color.sec.curves", Rect::new(x, y, w, SECTION_H), "Kurven", &mut self.open_curves);
        let curves_dirty = !grade.curves.is_identity();
        if section_reset(ui, "color.sec.curves", y, curves_dirty) {
            acts.push(Act::ResetSection(4));
        }
        y += SECTION_H;
        if self.open_curves {
            let inner_x = x + 8.0;
            let inner_w = w - 16.0;
            // Kanal-Auswahl.
            let chan_row = Rect::new(inner_x, y, inner_w, ROW_H - 4.0);
            self.curve_channel_selector(ui, chan_row, &grade.curves);
            y += ROW_H;
            // Editor (quadratisch zentriert).
            let sq = CURVE_EDITOR_H.min(inner_w);
            let ed = Rect::new(inner_x + (inner_w - sq) / 2.0, y, sq, sq);
            self.curve_editor(ui, ed, &grade, &mut acts);
            y += sq + 6.0;
            // Hinweis + Kanal-Reset.
            let hint_row = Rect::new(inner_x, y, inner_w, ROW_H - 4.0);
            let ch = self.curve_channel;
            let reset_r = Rect::new(hint_row.right() - 22.0, hint_row.y, 22.0, 22.0);
            if IconButton::new("rotate-ccw")
                .size(13.0)
                .disabled(grade.curves.channel(ch).is_identity())
                .tooltip("Kanal-Kurve zurücksetzen")
                .show(ui, ("color.curve.reset", ch), reset_r)
                .clicked
            {
                acts.push(Act::CurveReset(ch));
            }
            let mut hint = hint_row;
            hint.cut_right(26.0);
            let label = ui
                .font(FontKind::Sans12)
                .ellipsize("Klick fügt Punkt · ziehen · Doppelklick löscht", hint.w);
            ui.text_left(&label, hint, theme::TEXT_3, FontKind::Sans12);
            y += ROW_H + 8.0;
        }
        ui.hline(x, y - 1.0, w, theme::LINE);

        // ================= 3D-LUTs =================
        section_header(ui, "color.sec.luts", Rect::new(x, y, w, SECTION_H), "3D-LUTs (.cube)", &mut self.open_luts);
        let luts_dirty = grade.input_lut.is_some() || grade.look_lut.is_some();
        if section_reset(ui, "color.sec.luts", y, luts_dirty) {
            acts.push(Act::ResetSection(5));
        }
        y += SECTION_H;
        if self.open_luts {
            let inner_x = x + 8.0;
            let inner_w = w - 16.0;
            // Input-LUT (Pipeline-Anfang) — eigene Kopie, da self.lut_slot_ui
            // self mutabel braucht und `grade` darüber hinaus gelesen wird.
            let input_slot = grade.input_lut.clone();
            let input_h = if input_slot.is_some() { ROW_H * 2.0 } else { ROW_H };
            self.lut_slot_ui(ui, true, "Input", &input_slot, input_offline, Rect::new(inner_x, y, inner_w, input_h), &mut acts);
            y += input_h + 2.0;
            let look_slot = grade.look_lut.clone();
            let look_h = if look_slot.is_some() { ROW_H * 2.0 } else { ROW_H };
            self.lut_slot_ui(ui, false, "Look", &look_slot, look_offline, Rect::new(inner_x, y, inner_w, look_h), &mut acts);
            y += look_h + 10.0;
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
                Act::CurveLive(ch, curve) => {
                    if !self.gesture_pushed {
                        app.timeline.begin_fx_edit();
                        self.gesture_pushed = true;
                    }
                    app.timeline
                        .grade_update_live(&id, |g| *g.curves.channel_mut(ch) = curve);
                }
                Act::CurveSet(ch, curve) => {
                    app.timeline
                        .grade_update(&id, |g| *g.curves.channel_mut(ch) = curve);
                }
                Act::CurveReset(ch) => {
                    if !grade.curves.channel(ch).is_identity() {
                        app.timeline
                            .grade_update(&id, |g| *g.curves.channel_mut(ch) = Curve::identity());
                    }
                }
                Act::PickLut(input) => {
                    // Datei-Dialog im Worker; das Ergebnis (LutPicked) setzt den
                    // Slot in main.rs (undo-fähig) für genau diesen Clip.
                    services.pick_lut_file(&id, input);
                }
                Act::RemoveLut(input) => {
                    app.timeline.grade_update(&id, |g| {
                        if input {
                            g.input_lut = None;
                        } else {
                            g.look_lut = None;
                        }
                    });
                }
                Act::LutStrengthLive(input, v) => {
                    if !self.gesture_pushed {
                        app.timeline.begin_fx_edit();
                        self.gesture_pushed = true;
                    }
                    let v = v.clamp(0.0, 100.0);
                    app.timeline.grade_update_live(&id, |g| {
                        let slot = if input { &mut g.input_lut } else { &mut g.look_lut };
                        if let Some(s) = slot {
                            s.strength = v;
                        }
                    });
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
                        3 => {
                            g.vignette_amount = 0.0;
                            g.vignette_midpoint = 50.0;
                            g.vignette_roundness = 0.0;
                            g.vignette_feather = 50.0;
                        }
                        4 => g.curves = GradeCurves::default(),
                        5 => {
                            g.input_lut = None;
                            g.look_lut = None;
                        }
                        _ => {}
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
