//! Effekteinstellungen: Premiere-artiger Keyframe-Editor für den ersten
//! ausgewählten Timeline-Clip. Links Parameter-Zeilen (Stopwatch, Wert-
//! Scrubbing mit Doppelklick-Eingabe, Keyframe-Navigation ◀ ◆ ▶), rechts
//! je Parameter eine Keyframe-Spur über die Clipdauer mit Playhead-Lineal.
//! Neben den eingebauten Abschnitten (Bewegung/Deckkraft/Audio) erscheint
//! der EFFEKT-STAPEL des Clips: je Instanz ein zusammenklappbarer Abschnitt
//! mit Bypass-Toggle (Blitz), Reorder (▲▼), Reset und Löschen; alle
//! Effekt-Parameter sind über [`ParamRef`] genauso animierbar wie die
//! eingebauten. Effekte aus dem Effekte-Panel können direkt auf dieses
//! Panel gezogen werden. Bei verknüpften A/V-Paaren erscheinen zusätzlich
//! Lautstärke + Audio-Effekte des Partners.

use crate::core::animation::{AnimatedParam, Keyframe, ParamId, ParamRef, KF_TIME_EPS};
use crate::core::audio_fx;
use crate::core::compose;
use crate::core::effects::{EffectKind, ParamUi};
use crate::core::timeline::{TimelineClip, TimelineStore, TrackKind};
use crate::overlays::context_menu::{CustomAction, MenuEntry, MenuItem};
use crate::panels::color::section_header;
use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::{v2, Rect};
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::widgets::IconButton;
use crate::ui::{DragPayload, FontKind, Ui};
use raylib::consts::{KeyboardKey, MouseCursor};
use raylib::math::Vector2;
use std::collections::HashSet;

const ROW_H: f32 = 26.0;
const SECTION_H: f32 = 32.0;
const EFFECT_H: f32 = 30.0;
const RULER_H: f32 = 22.0;
/// Höhe der Effekt-Visualisierung (EQ-Kurve, Kompressor-Kennlinie).
const VIZ_H: f32 = 132.0;
/// Audio-Mixdown-Rate (für die EQ-Kurve, identisch zur Engine).
const VIZ_RATE: u32 = 48000;
/// Breite der linken Parameter-Spalte; rechts beginnen die Keyframe-Spuren.
const LEFT_W: f32 = 300.0;
const KEY_R: f32 = 5.0;
const KEY_HIT: f32 = 7.0;
const DRAG_THRESHOLD: f32 = 2.0;

/// Ausgewählter Keyframe (Medienzeit als Identität).
#[derive(Clone, Debug)]
struct SelKey {
    clip_id: String,
    pref: ParamRef,
    t: f64,
}

impl SelKey {
    fn matches(&self, clip_id: &str, pref: &ParamRef, t: f64) -> bool {
        self.clip_id == clip_id && &self.pref == pref && (self.t - t).abs() < KF_TIME_EPS
    }
}

/// Laufende Keyframe-Verschiebung: Originalkurven + Auswahl bei Gestenbeginn.
struct KeyDrag {
    start_mouse: Vector2,
    curves: Vec<(String, ParamRef, Vec<Keyframe>)>,
    orig_sel: Vec<SelKey>,
    history_pushed: bool,
}

/// Laufendes Wert-Scrubbing einer Parameter-Zeile.
struct ValueDrag {
    clip_id: String,
    pref: ParamRef,
    start_value: f64,
    start_x: f32,
    step: f64,
    history_pushed: bool,
}

/// Laufendes Umsortieren einer Effekt-Instanz im Stapel (Label ziehen).
struct FxReorder {
    /// Index in `clips` (Video/Audio-Partner werden getrennt sortiert).
    clip: usize,
    fx_id: String,
    start_y: f32,
    moved: bool,
}

/// Anzeige-Metadaten eines Parameters (eingebaut oder Effekt-Spec).
struct ParamMeta {
    label: String,
    unit: &'static str,
    step: f64,
    decimals: usize,
    animatable: bool,
}

/// Metadaten + Kurve eines Parameters auflösen.
fn param_meta<'a>(clip: &'a TimelineClip, pref: &ParamRef) -> Option<(ParamMeta, &'a AnimatedParam)> {
    match pref {
        ParamRef::Builtin(id) => {
            let label = if *id == ParamId::ScaleX && !clip.fx.uniform_scale {
                "Skalierung X".to_string()
            } else {
                id.label().to_string()
            };
            Some((
                ParamMeta {
                    label,
                    unit: id.unit(),
                    step: id.drag_step(),
                    decimals: id.decimals(),
                    animatable: true,
                },
                clip.fx.param(*id),
            ))
        }
        ParamRef::Effect { fx_id, index } => {
            let inst = clip.effects.iter().find(|e| &e.id == fx_id)?;
            let spec = inst.kind.specs().get(*index)?;
            Some((
                ParamMeta {
                    label: spec.label.to_string(),
                    unit: spec.unit,
                    step: spec.step,
                    decimals: spec.decimals,
                    animatable: spec.animatable,
                },
                inst.params.get(*index)?,
            ))
        }
    }
}

/// Zeile im Panel (vorab gesammelt, dann gerendert — Borrow-Trennung).
enum Row {
    Section {
        key: &'static str,
        title: &'static str,
        reset: ResetKind,
    },
    Param {
        clip: usize, // Index in `clips`
        pref: ParamRef,
    },
    UniformToggle {
        clip: usize,
    },
    /// Kopfzeile einer Effekt-Instanz (Bypass/Reorder/Reset/Löschen).
    EffectHeader {
        clip: usize,
        fx_idx: usize,
    },
    /// Bool-Parameter als Checkbox.
    ToggleParam {
        clip: usize,
        pref: ParamRef,
    },
    /// Farb-Parameter: Swatch + R/G/B-Zellen (drei Spec-Slots ab `p_idx`).
    ColorParam {
        clip: usize,
        fx_idx: usize,
        p_idx: usize,
    },
    /// Visualisierung eines Audio-Effekts (EQ-Frequenzgang bzw. Kompressor-
    /// Kennlinie + GR-Meter) — oberhalb der Parameter der Instanz.
    FxViz {
        clip: usize,
        fx_idx: usize,
    },
    /// „Masken“-Leiste eines Video-Effekts: Hinzufügen-Buttons (Ellipse/
    /// Rechteck/Polygon).
    MaskBar {
        clip: usize,
        fx_idx: usize,
    },
    /// Eine Maske eines Effekts (Bearbeiten/Invertieren/Bypass/Löschen).
    MaskItem {
        clip: usize,
        fx_idx: usize,
        mask_idx: usize,
    },
}

#[derive(Clone, Copy, PartialEq)]
enum ResetKind {
    Motion,
    Opacity,
    Audio,
}

pub struct EffectControlsPanel {
    /// Primärer Ziel-Clip (frischer State bei Wechsel).
    clip_id: Option<String>,
    open_motion: bool,
    open_opacity: bool,
    open_audio: bool,
    /// Zusammengeklappte Effekt-Instanzen (fx-IDs).
    collapsed_fx: HashSet<String>,
    scroll: ScrollState,
    selected_keys: Vec<SelKey>,
    key_drag: Option<KeyDrag>,
    value_drag: Option<ValueDrag>,
    /// Laufendes Drag-to-Reorder einer Effekt-Instanz.
    fx_reorder: Option<FxReorder>,
    /// Inline-Eingabe eines Werts: (Clip, Parameter, Feld).
    edit: Option<(String, ParamRef, TextInputState)>,
    /// Box-Auswahl: Startpunkt in Bildschirmkoordinaten.
    box_select: Option<Vector2>,
    ruler_drag: bool,
}

impl Default for EffectControlsPanel {
    fn default() -> Self {
        EffectControlsPanel {
            clip_id: None,
            open_motion: true,
            open_opacity: true,
            open_audio: true,
            collapsed_fx: HashSet::new(),
            scroll: ScrollState::default(),
            selected_keys: Vec::new(),
            key_drag: None,
            value_drag: None,
            fx_reorder: None,
            edit: None,
            box_select: None,
            ruler_drag: false,
        }
    }
}

/// Zahl im deutschen Format (Komma) mit fester Nachkommastelle.
fn fmt_value(v: f64, decimals: usize) -> String {
    format!("{v:.decimals$}").replace('.', ",")
}

fn parse_value(s: &str) -> Option<f64> {
    s.trim().replace(',', ".").parse().ok()
}

/// Keyframe-Raute. raylib-Falle: `draw_poly` setzt die Vertices AB dem
/// Rotationswinkel — 0° ergibt die Raute (Spitzen auf den Achsen), 45° wäre
/// das achsparallele Quadrat.
fn draw_diamond(ui: &mut Ui, cx: f32, cy: f32, r: f32, fill: raylib::color::Color, line: raylib::color::Color) {
    ui.poly(v2(cx, cy), 4, r, 0.0, fill);
    ui.poly_lines(v2(cx, cy), 4, r, 0.0, line);
}

/// EQ-Frequenzgang (20 Hz–20 kHz, ±18 dB) zeichnen. Nutzt dieselben Filter-
/// Koeffizienten wie der DSP-Pfad (`audio_fx::eq_response_db`) — die Kurve
/// zeigt also exakt, was zu hören ist. `values` = 12 EQ-Parameter.
fn draw_eq_curve(ui: &mut Ui, area: Rect, values: &[f64]) {
    const FMIN: f64 = 20.0;
    const FMAX: f64 = 20000.0;
    const DB: f64 = 18.0;
    let (lf0, lf1) = (FMIN.log10(), FMAX.log10());
    let x_of = |f: f64| area.x + ((f.log10() - lf0) / (lf1 - lf0)) as f32 * area.w;
    let y_of = |db: f64| area.y + area.h * 0.5 - (db / DB) as f32 * (area.h * 0.5);
    for db in [-12.0, -6.0, 0.0, 6.0, 12.0] {
        let yy = y_of(db);
        let col = if db.abs() < 0.01 {
            theme::LINE_STRONG
        } else {
            theme::with_alpha(theme::LINE, 90)
        };
        ui.hline(area.x, yy, area.w, col);
    }
    for (f, lbl) in [(100.0, "100"), (1000.0, "1k"), (10000.0, "10k")] {
        let xx = x_of(f);
        ui.vline(xx, area.y, area.h, theme::with_alpha(theme::LINE, 70));
        ui.text_left(
            lbl,
            Rect::new(xx + 2.0, area.bottom() - 13.0, 26.0, 12.0),
            theme::TEXT_3,
            FontKind::Mono11,
        );
    }
    let n = (area.w as usize).clamp(24, 480);
    let mut prev: Option<Vector2> = None;
    for i in 0..=n {
        let frac = i as f64 / n as f64;
        let f = 10f64.powf(lf0 + frac * (lf1 - lf0));
        let db = audio_fx::eq_response_db(values, VIZ_RATE, f).clamp(-DB, DB);
        let p = v2(area.x + frac as f32 * area.w, y_of(db));
        if let Some(pp) = prev {
            ui.line(pp, p, 1.6, theme::ACCENT);
        }
        prev = Some(p);
    }
    // Band-Mittelpunkte als Punkte (Frequenz/Gain je Band).
    for b in 0..4 {
        let f = values.get(b * 3).copied().unwrap_or(0.0);
        let g = values.get(b * 3 + 1).copied().unwrap_or(0.0).clamp(-DB, DB);
        if f <= 0.0 {
            continue;
        }
        let p = v2(x_of(f), y_of(g));
        ui.circle(p, 3.5, theme::ACCENT_HOVER);
    }
}

/// Kompressor-Kennlinie (Eingang→Ausgang, dB) + Live-Gain-Reduktions-Meter.
/// `values` = [Threshold, Ratio, Attack, Release, Makeup].
fn draw_comp_curve(ui: &mut Ui, area: Rect, values: &[f64], gr_db: f32) {
    const LO: f64 = -54.0;
    const HI: f64 = 0.0;
    let thr = values.first().copied().unwrap_or(-18.0);
    let ratio = values.get(1).copied().unwrap_or(4.0).max(1.0);
    let makeup = values.get(4).copied().unwrap_or(0.0);
    let meter_w = 26.0;
    let plot = Rect::new(area.x, area.y, (area.w - meter_w - 6.0).max(20.0), area.h);
    let x_of = |din: f64| plot.x + ((din - LO) / (HI - LO)) as f32 * plot.w;
    let y_of = |dout: f64| plot.bottom() - ((dout.clamp(LO, HI) - LO) / (HI - LO)) as f32 * plot.h;
    // 1:1-Referenz.
    ui.line(
        v2(x_of(LO), y_of(LO)),
        v2(x_of(HI), y_of(HI)),
        1.0,
        theme::with_alpha(theme::LINE, 120),
    );
    // Threshold-Markierung.
    let tx = x_of(thr.clamp(LO, HI));
    ui.vline(tx, plot.y, plot.h, theme::with_alpha(theme::WARNING, 150));
    // Kennlinie (Knie hart, Makeup eingerechnet).
    let n = (plot.w as usize).clamp(16, 320);
    let mut prev: Option<Vector2> = None;
    for i in 0..=n {
        let din = LO + (i as f64 / n as f64) * (HI - LO);
        let dout = if din <= thr {
            din
        } else {
            thr + (din - thr) / ratio
        } + makeup;
        let p = v2(x_of(din), y_of(dout));
        if let Some(pp) = prev {
            ui.line(pp, p, 1.6, theme::ACCENT);
        }
        prev = Some(p);
    }
    // Live-GR-Meter (0..24 dB, von oben nach unten).
    let meter = Rect::new(area.right() - meter_w, area.y + 2.0, meter_w - 4.0, area.h - 16.0);
    ui.fill(meter, theme::SURFACE_2);
    let gr = (gr_db / 24.0).clamp(0.0, 1.0);
    ui.fill(Rect::new(meter.x, meter.y, meter.w, gr * meter.h), theme::WARNING);
    ui.text_centered(
        "GR",
        Rect::new(meter.x, area.bottom() - 12.0, meter.w, 11.0),
        theme::TEXT_3,
        FontKind::Mono11,
    );
}

/// Medienzeit ↔ x-Position in der Keyframe-Spur (Clip-lokal): läuft über
/// die zentrale Zeit-Abbildung — Keyframes liegen damit auch bei
/// Geschwindigkeit ≠ 1 und rückwärts exakt unter dem Playhead.
fn t_to_x(lane: Rect, clip: &TimelineClip, media_t: f64) -> f32 {
    let local =
        ((clip.seq_time_of_media(media_t) - clip.start) / clip.duration.max(1e-9)) as f32;
    lane.x + local * lane.w
}

fn x_to_media_t(lane: Rect, clip: &TimelineClip, x: f32) -> f64 {
    let local = ((x - lane.x) / lane.w.max(1.0)) as f64;
    clip.media_time_at(clip.start + local * clip.duration)
}

impl EffectControlsPanel {
    /// Medienzeit des Playheads im Clip (für Werte/Keyframes geklemmt).
    fn playhead_media_t(clip: &TimelineClip, playhead: f64) -> f64 {
        compose::clip_media_time(clip, playhead)
            .clamp(clip.media_in(), clip.media_out().max(clip.media_in()))
    }

    fn is_selected(&self, clip_id: &str, pref: &ParamRef, t: f64) -> bool {
        self.selected_keys
            .iter()
            .any(|k| k.matches(clip_id, pref, t))
    }

    /// Auswahl bereinigen: nur Keys behalten, die noch existieren.
    fn prune_selection(&mut self, clips: &[TimelineClip]) {
        self.selected_keys.retain(|sel| {
            clips.iter().any(|c| {
                c.id == sel.clip_id
                    && TimelineStore::clip_param(c, &sel.pref)
                        .is_some_and(|p| p.key_index_at(sel.t).is_some())
            })
        });
    }

    /// Effekt-Zeilen einer Instanz anhängen (Header + Parameter).
    fn push_effect_rows(&self, rows: &mut Vec<Row>, clip_idx: usize, clip: &TimelineClip) {
        let want_audio = clip.kind == TrackKind::Audio;
        for (fx_idx, inst) in clip.effects.iter().enumerate() {
            if inst.kind.is_audio() != want_audio {
                continue;
            }
            rows.push(Row::EffectHeader { clip: clip_idx, fx_idx });
            if self.collapsed_fx.contains(&inst.id) {
                continue;
            }
            // Visualisierung über den Reglern: EQ-Kurve bzw. Kompressor-Kennlinie.
            if matches!(inst.kind, EffectKind::Equalizer | EffectKind::Compressor) {
                rows.push(Row::FxViz { clip: clip_idx, fx_idx });
            }
            let specs = inst.kind.specs();
            let mut i = 0;
            while i < specs.len() {
                match specs[i].ui {
                    ParamUi::ColorRgb => {
                        rows.push(Row::ColorParam { clip: clip_idx, fx_idx, p_idx: i });
                        // Drei Kanäle (R, G, B) in einer Zeile.
                        i += 3;
                    }
                    ParamUi::Toggle => {
                        rows.push(Row::ToggleParam {
                            clip: clip_idx,
                            pref: ParamRef::Effect { fx_id: inst.id.clone(), index: i },
                        });
                        i += 1;
                    }
                    ParamUi::Slider => {
                        rows.push(Row::Param {
                            clip: clip_idx,
                            pref: ParamRef::Effect { fx_id: inst.id.clone(), index: i },
                        });
                        i += 1;
                    }
                }
            }
            // Masken (nur Video-Effekte): Hinzufügen-Leiste + je Maske eine Zeile.
            if !inst.kind.is_audio() {
                rows.push(Row::MaskBar { clip: clip_idx, fx_idx });
                for mask_idx in 0..inst.masks.len() {
                    rows.push(Row::MaskItem { clip: clip_idx, fx_idx, mask_idx });
                }
            }
        }
    }
}

impl Panel for EffectControlsPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);
        if ui.mouse_in(rect) && (ui.input.left_pressed || ui.input.right_pressed) {
            app.app.focused_panel = "effectControls".into();
        }

        // ---- Ziel-Clips bestimmen: primärer + ggf. verknüpfter Partner ----
        let primary = app
            .timeline
            .selected_clip_ids
            .first()
            .and_then(|id| app.timeline.clip(id))
            .cloned();
        let Some(primary) = primary else {
            *self = EffectControlsPanel {
                scroll: std::mem::take(&mut self.scroll),
                ..Default::default()
            };
            ui.text_centered(
                "Clip in der Timeline auswählen",
                rect,
                theme::TEXT_3,
                FontKind::Sans12,
            );
            return;
        };
        if self.clip_id.as_deref() != Some(primary.id.as_str()) {
            *self = EffectControlsPanel {
                clip_id: Some(primary.id.clone()),
                ..Default::default()
            };
        }
        let linked = primary.link_id.as_ref().and_then(|link| {
            app.timeline
                .clips
                .iter()
                .find(|c| c.id != primary.id && c.link_id.as_deref() == Some(link))
                .cloned()
        });
        // clips[0] = Video-Teil (Bewegung/Deckkraft), clips[..] = Audio-Teil.
        let mut clips: Vec<TimelineClip> = Vec::new();
        let mut video_idx: Option<usize> = None;
        let mut audio_idx: Option<usize> = None;
        for c in [Some(primary.clone()), linked].into_iter().flatten() {
            match c.kind {
                // Untertitel-Segmente sind transformierbar wie Video-Layer.
                TrackKind::Video | TrackKind::Subtitle if video_idx.is_none() => {
                    video_idx = Some(clips.len());
                    clips.push(c);
                }
                TrackKind::Audio if audio_idx.is_none() => {
                    audio_idx = Some(clips.len());
                    clips.push(c);
                }
                _ => {}
            }
        }
        self.prune_selection(&clips);

        let playhead = app.timeline.playhead_sec;

        // ---- Zeilen zusammenstellen ----
        let mut rows: Vec<Row> = Vec::new();
        if let Some(vi) = video_idx {
            rows.push(Row::Section {
                key: "fx.sec.motion",
                title: "Bewegung",
                reset: ResetKind::Motion,
            });
            if self.open_motion {
                let uniform = clips[vi].fx.uniform_scale;
                rows.push(Row::Param { clip: vi, pref: ParamId::PosX.into() });
                rows.push(Row::Param { clip: vi, pref: ParamId::PosY.into() });
                rows.push(Row::Param { clip: vi, pref: ParamId::ScaleX.into() });
                if !uniform {
                    rows.push(Row::Param { clip: vi, pref: ParamId::ScaleY.into() });
                }
                rows.push(Row::UniformToggle { clip: vi });
                rows.push(Row::Param { clip: vi, pref: ParamId::Rotation.into() });
            }
            rows.push(Row::Section {
                key: "fx.sec.opacity",
                title: "Deckkraft",
                reset: ResetKind::Opacity,
            });
            if self.open_opacity {
                rows.push(Row::Param { clip: vi, pref: ParamId::Opacity.into() });
            }
            // Video-Effekt-Stapel.
            self.push_effect_rows(&mut rows, vi, &clips[vi]);
        }
        if let Some(ai) = audio_idx {
            rows.push(Row::Section {
                key: "fx.sec.audio",
                title: "Audio",
                reset: ResetKind::Audio,
            });
            if self.open_audio {
                rows.push(Row::Param { clip: ai, pref: ParamId::VolumeDb.into() });
            }
            // Audio-Effekt-Stapel.
            self.push_effect_rows(&mut rows, ai, &clips[ai]);
        }

        // ---- Kopf + Geometrie ----
        let mut area = rect;
        let head = area.cut_top(36.0);
        ui.hline(head.x, head.bottom() - 1.0, head.w, theme::LINE);
        let name = ui
            .font(FontKind::Sans12Medium)
            .ellipsize(&primary.name, head.w * 0.6);
        ui.text_left(&name, head.inset_xy(12.0, 0.0), theme::TEXT_1, FontKind::Sans12Medium);
        let tc = format!(
            "{} – {}",
            crate::core::timecode::format_sequence_timecode(primary.start, &app.timeline.settings),
            crate::core::timecode::format_sequence_timecode(primary.end(), &app.timeline.settings)
        );
        ui.text_right(&tc, head.inset_xy(12.0, 0.0), theme::TEXT_3, FontKind::Mono11);

        // Adaptive Spaltenbreite: Keyframe-Spuren nur, wenn rechts genug
        // Platz bleibt; sonst nimmt die Parameter-Spalte das ganze Panel.
        let left_w = if rect.w >= LEFT_W + 140.0 {
            LEFT_W
        } else {
            (rect.w - 8.0).max(160.0)
        };
        let lane_x = rect.x + left_w;
        let lane_w = (rect.right() - lane_x - 12.0).max(0.0);
        let has_lanes = lane_w > 100.0;

        // ---- Lineal (fix, nicht gescrollt) ----
        let ruler = Rect::new(lane_x, area.y, lane_w, RULER_H);
        area.cut_top(RULER_H);
        let lanes_full = Rect::new(lane_x, area.y, lane_w, area.h);

        let content_h = rows
            .iter()
            .map(|r| match r {
                Row::Section { .. } => SECTION_H,
                Row::EffectHeader { .. } => EFFECT_H,
                Row::FxViz { .. } => VIZ_H,
                _ => ROW_H,
            })
            .sum::<f32>()
            + 8.0;
        let view = self.scroll.begin(ui, area, 0.0, content_h);
        let x = view.viewport.x;
        let mut y = view.origin_y;

        // Gesammelte Aktionen (nach dem Zeichnen ausgeführt — Borrow-Trennung).
        enum Act {
            ToggleAnimated(String, ParamRef, f64),
            ToggleKeyframe(String, ParamRef, f64),
            SeekTo(f64),
            BeginValueDrag(ValueDrag),
            OpenEdit(String, ParamRef, f64, usize),
            CommitEdit(String, ParamRef, f64),
            Reset(ResetKind, usize),
            SetUniform(String, bool),
            /// Einzelwert mit Undo-Schritt setzen (Bool-Toggles).
            SetValue(String, ParamRef, f64),
            SelectKey { key: SelKey, additive: bool, toggle: bool },
            StartKeyDrag,
            AddKeyframe(String, ParamRef, f64),
            OpenKeyMenu { key: SelKey },
            EffectToggle(String, String),
            EffectMove(String, String, i32),
            EffectReorder(String, String, usize),
            EffectRemove(String, String),
            EffectReset(String, String),
            EffectCollapse(String),
            OpenEffectMenu { clip_id: String, fx_id: String, fx_idx: usize, count: usize },
            MaskAdd(String, String, crate::core::mask::MaskShape),
            MaskEdit(String, String, String),
            MaskRemove(String, String, String),
            MaskToggleInvert(String, String, String),
            MaskToggleEnabled(String, String, String),
        }
        let mut acts: Vec<Act> = Vec::new();
        let mut hover_any_key = false;
        // Effekt-Header-Positionen (Clip-Index, fx_idx, fx_id, y-oben) für
        // Drag-to-Reorder.
        let mut fx_spans: Vec<(usize, usize, String, f32)> = Vec::new();
        // Sichtbare Keys für die Box-Auswahl: (SelKey, Position).
        let mut key_positions: Vec<(SelKey, Vector2)> = Vec::new();

        for row in &rows {
            match row {
                Row::Section { key, title, reset } => {
                    let header = Rect::new(x, y, left_w - 8.0, SECTION_H);
                    let open = match reset {
                        ResetKind::Motion => &mut self.open_motion,
                        ResetKind::Opacity => &mut self.open_opacity,
                        ResetKind::Audio => &mut self.open_audio,
                    };
                    section_header(ui, key, header, title, open);
                    let reset_rect = Rect::new(x + left_w - 38.0, y + 5.0, 22.0, 22.0);
                    if IconButton::new("rotate-ccw")
                        .size(14.0)
                        .tooltip("Abschnitt zurücksetzen")
                        .show(ui, (key, "reset"), reset_rect)
                        .clicked
                    {
                        let clip_idx = match reset {
                            ResetKind::Audio => audio_idx.unwrap_or(0),
                            _ => video_idx.unwrap_or(0),
                        };
                        acts.push(Act::Reset(*reset, clip_idx));
                    }
                    ui.hline(x, y + SECTION_H - 1.0, rect.w - 12.0, theme::LINE);
                    y += SECTION_H;
                }
                Row::EffectHeader { clip, fx_idx } => {
                    let clip_ref = &clips[*clip];
                    let Some(inst) = clip_ref.effects.get(*fx_idx) else {
                        y += EFFECT_H;
                        continue;
                    };
                    let count = clip_ref
                        .effects
                        .iter()
                        .filter(|e| e.kind.is_audio() == (clip_ref.kind == TrackKind::Audio))
                        .count();
                    let header = Rect::new(x, y, left_w - 8.0, EFFECT_H);
                    let hid = ui.id(("fx.effect.header", &inst.id));
                    let it = ui.interact(hid, header);
                    if it.hovered {
                        ui.fill(Rect::new(x, y, rect.w - 12.0, EFFECT_H), theme::with_alpha(theme::SURFACE_2, 120));
                        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                    }
                    let collapsed = self.collapsed_fx.contains(&inst.id);
                    let mut hi = header.inset_xy(8.0, 0.0);
                    let chev = hi.cut_left(14.0);
                    ui.icon(
                        if collapsed { "chevron-right" } else { "chevron-down" },
                        chev,
                        13.0,
                        theme::TEXT_3,
                    );
                    hi.cut_left(4.0);
                    // Bypass-Toggle (Blitz): an = Akzent, aus = grau.
                    let zap_rect = Rect::new(hi.x, y + (EFFECT_H - 18.0) / 2.0, 18.0, 18.0);
                    hi.cut_left(22.0);
                    let zit = IconButton::new("zap")
                        .size(13.0)
                        .active(inst.enabled)
                        .tooltip(if inst.enabled { "Effekt umgehen (Bypass)" } else { "Effekt aktivieren" })
                        .show(ui, ("fx.effect.zap", &inst.id), zap_rect);
                    if zit.clicked {
                        acts.push(Act::EffectToggle(clip_ref.id.clone(), inst.id.clone()));
                    }
                    // Aktions-Buttons rechtsbündig: ▲ ▼ Reset Löschen.
                    let mut bx = x + left_w - 8.0;
                    let mut button = |ui: &mut Ui, icon: &'static str, tip: &'static str, disabled: bool, idkey: &str| -> bool {
                        bx -= 20.0;
                        let b = Rect::new(bx, y + (EFFECT_H - 18.0) / 2.0, 18.0, 18.0);
                        IconButton::new(icon)
                            .size(12.0)
                            .disabled(disabled)
                            .tooltip(tip)
                            .show(ui, ("fx.effect.btn", &inst.id, idkey), b)
                            .clicked
                    };
                    if button(ui, "trash-2", "Effekt entfernen", false, "del") {
                        acts.push(Act::EffectRemove(clip_ref.id.clone(), inst.id.clone()));
                    }
                    if button(ui, "rotate-ccw", "Effekt zurücksetzen", false, "reset") {
                        acts.push(Act::EffectReset(clip_ref.id.clone(), inst.id.clone()));
                    }
                    if button(ui, "chevron-down", "Im Stapel nach unten", *fx_idx + 1 >= count, "down") {
                        acts.push(Act::EffectMove(clip_ref.id.clone(), inst.id.clone(), 1));
                    }
                    if button(ui, "chevron-up", "Im Stapel nach oben", *fx_idx == 0, "up") {
                        acts.push(Act::EffectMove(clip_ref.id.clone(), inst.id.clone(), -1));
                    }
                    // Label (+ Hinweis bei Bypass).
                    let label_cell = Rect::new(hi.x, y, (bx - hi.x - 6.0).max(20.0), EFFECT_H);
                    let label = if inst.enabled {
                        inst.kind.label().to_string()
                    } else {
                        format!("{} (aus)", inst.kind.label())
                    };
                    let display = ui.font(FontKind::Sans12Medium).ellipsize(&label, label_cell.w);
                    ui.text_left(
                        &display,
                        label_cell,
                        if inst.enabled { theme::TEXT_1 } else { theme::TEXT_3 },
                        FontKind::Sans12Medium,
                    );
                    // Drag-to-Reorder: Label-Zelle ist die Greifzone (Buttons
                    // ausgespart). Beginn der Geste hier, Auswertung nach der
                    // Schleife (Borrow-Trennung).
                    fx_spans.push((*clip, *fx_idx, inst.id.clone(), y));
                    if ui.input.left_pressed
                        && ui.mouse_in(label_cell)
                        && self.fx_reorder.is_none()
                        && self.value_drag.is_none()
                        && self.key_drag.is_none()
                    {
                        self.fx_reorder = Some(FxReorder {
                            clip: *clip,
                            fx_id: inst.id.clone(),
                            start_y: ui.input.mouse.y,
                            moved: false,
                        });
                    }
                    let reordering_this = self
                        .fx_reorder
                        .as_ref()
                        .is_some_and(|r| r.fx_id == inst.id && r.moved);
                    if it.clicked && !reordering_this {
                        acts.push(Act::EffectCollapse(inst.id.clone()));
                    }
                    if it.right_clicked {
                        acts.push(Act::OpenEffectMenu {
                            clip_id: clip_ref.id.clone(),
                            fx_id: inst.id.clone(),
                            fx_idx: *fx_idx,
                            count,
                        });
                    }
                    ui.hline(x, y + EFFECT_H - 1.0, rect.w - 12.0, theme::with_alpha(theme::LINE, 140));
                    y += EFFECT_H;
                }
                Row::FxViz { clip, fx_idx } => {
                    let clip_ref = &clips[*clip];
                    let Some(inst) = clip_ref.effects.get(*fx_idx) else {
                        y += VIZ_H;
                        continue;
                    };
                    let media_t = Self::playhead_media_t(clip_ref, playhead);
                    let values = inst.eval(media_t).values;
                    let area_box = Rect::new(x + 10.0, y + 6.0, (rect.w - 28.0).max(80.0), VIZ_H - 16.0);
                    ui.fill_rounded(area_box, theme::RADIUS_SM, theme::SURFACE_0);
                    ui.stroke_rounded(area_box, theme::RADIUS_SM, 1.0, theme::LINE);
                    match inst.kind {
                        EffectKind::Equalizer => draw_eq_curve(ui, area_box, &values),
                        EffectKind::Compressor => {
                            let gr = app
                                .audio
                                .fx_gain_reduction
                                .get(&inst.id)
                                .copied()
                                .unwrap_or(0.0);
                            draw_comp_curve(ui, area_box, &values, gr);
                        }
                        _ => {}
                    }
                    y += VIZ_H;
                }
                Row::MaskBar { clip, fx_idx } => {
                    let clip_ref = &clips[*clip];
                    let Some(inst) = clip_ref.effects.get(*fx_idx) else {
                        y += ROW_H;
                        continue;
                    };
                    let label_cell = Rect::new(x + 30.0, y, 90.0, ROW_H);
                    ui.text_left("Masken", label_cell, theme::TEXT_2, FontKind::Sans12);
                    // Hinzufügen-Buttons rechtsbündig (Polygon, Rechteck, Ellipse).
                    let mut bx = x + left_w - 8.0;
                    for shape in [
                        crate::core::mask::MaskShape::Polygon,
                        crate::core::mask::MaskShape::Rectangle,
                        crate::core::mask::MaskShape::Ellipse,
                    ] {
                        bx -= 22.0;
                        let b = Rect::new(bx, y + (ROW_H - 18.0) / 2.0, 18.0, 18.0);
                        if IconButton::new(shape.icon())
                            .size(12.0)
                            .tooltip(shape.label())
                            .show(ui, ("fx.mask.add", &inst.id, shape.key()), b)
                            .clicked
                        {
                            acts.push(Act::MaskAdd(clip_ref.id.clone(), inst.id.clone(), shape));
                        }
                    }
                    let plus = Rect::new(bx - 14.0, y, 12.0, ROW_H);
                    ui.text_right("+", plus, theme::TEXT_3, FontKind::Sans12Medium);
                    y += ROW_H;
                }
                Row::MaskItem { clip, fx_idx, mask_idx } => {
                    let clip_ref = &clips[*clip];
                    let Some(inst) = clip_ref.effects.get(*fx_idx) else {
                        y += ROW_H;
                        continue;
                    };
                    let Some(mask) = inst.masks.get(*mask_idx) else {
                        y += ROW_H;
                        continue;
                    };
                    let editing = app
                        .app
                        .active_mask
                        .as_ref()
                        .is_some_and(|s| s.mask_id == mask.id);
                    if editing {
                        ui.fill(
                            Rect::new(x, y, rect.w - 12.0, ROW_H),
                            theme::with_alpha(theme::ACCENT, 36),
                        );
                    }
                    // Form-Icon + Name (eingerückt).
                    let mut hi = Rect::new(x + 44.0, y, left_w - 52.0, ROW_H);
                    let ic = hi.cut_left(16.0);
                    let dim = if mask.enabled { theme::TEXT_2 } else { theme::TEXT_3 };
                    ui.icon(mask.shape.icon(), ic, 12.0, dim);
                    hi.cut_left(4.0);
                    // Aktions-Buttons rechtsbündig: Löschen, Bypass, Invertieren, Bearbeiten.
                    let mut bx = x + left_w - 8.0;
                    let mut button = |ui: &mut Ui,
                                      icon: &'static str,
                                      tip: &'static str,
                                      active: bool,
                                      idkey: &str|
                     -> bool {
                        bx -= 20.0;
                        let b = Rect::new(bx, y + (ROW_H - 18.0) / 2.0, 18.0, 18.0);
                        IconButton::new(icon)
                            .size(12.0)
                            .active(active)
                            .tooltip(tip)
                            .show(ui, ("fx.mask.btn", &mask.id, idkey), b)
                            .clicked
                    };
                    if button(ui, "trash-2", "Maske löschen", false, "del") {
                        acts.push(Act::MaskRemove(
                            clip_ref.id.clone(),
                            inst.id.clone(),
                            mask.id.clone(),
                        ));
                    }
                    if button(
                        ui,
                        if mask.enabled { "eye" } else { "eye-off" },
                        "Maske umgehen",
                        mask.enabled,
                        "eye",
                    ) {
                        acts.push(Act::MaskToggleEnabled(
                            clip_ref.id.clone(),
                            inst.id.clone(),
                            mask.id.clone(),
                        ));
                    }
                    if button(
                        ui,
                        "flip-horizontal-2",
                        "Maske invertieren",
                        mask.inverted,
                        "inv",
                    ) {
                        acts.push(Act::MaskToggleInvert(
                            clip_ref.id.clone(),
                            inst.id.clone(),
                            mask.id.clone(),
                        ));
                    }
                    if button(ui, "move", "Maske im Monitor bearbeiten", editing, "edit") {
                        acts.push(Act::MaskEdit(
                            clip_ref.id.clone(),
                            inst.id.clone(),
                            mask.id.clone(),
                        ));
                    }
                    // Name zwischen Icon und Buttons.
                    let name = format!("{} {}", mask.shape.label(), mask_idx + 1);
                    let name_cell = Rect::new(hi.x, y, (bx - hi.x - 6.0).max(20.0), ROW_H);
                    let display = ui.font(FontKind::Sans12).ellipsize(&name, name_cell.w);
                    ui.text_left(&display, name_cell, dim, FontKind::Sans12);
                    y += ROW_H;
                }
                Row::UniformToggle { clip } => {
                    let clip_ref = &clips[*clip];
                    let row_rect = Rect::new(x + 30.0, y, left_w - 38.0, ROW_H);
                    let id = ui.id(("fx.uniform", &clip_ref.id));
                    let it = ui.interact(id, row_rect);
                    let box_rect = Rect::new(row_rect.x, y + (ROW_H - 14.0) / 2.0, 14.0, 14.0);
                    ui.fill_rounded(box_rect, theme::RADIUS_XS, theme::SURFACE_3);
                    ui.stroke_rounded(box_rect, theme::RADIUS_XS, 1.0, theme::LINE_STRONG);
                    if clip_ref.fx.uniform_scale {
                        ui.icon("check", box_rect, 12.0, theme::ACCENT);
                    }
                    let label = Rect::new(box_rect.right() + 8.0, y, row_rect.w - 22.0, ROW_H);
                    ui.text_left(
                        "Seitenverhältnis beibehalten",
                        label,
                        if it.hovered { theme::TEXT_1 } else { theme::TEXT_2 },
                        FontKind::Sans12,
                    );
                    if it.hovered {
                        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                    }
                    if it.clicked {
                        acts.push(Act::SetUniform(
                            clip_ref.id.clone(),
                            !clip_ref.fx.uniform_scale,
                        ));
                    }
                    y += ROW_H;
                }
                Row::ToggleParam { clip, pref } => {
                    let clip_ref = &clips[*clip];
                    let Some((meta, p)) = param_meta(clip_ref, pref) else {
                        y += ROW_H;
                        continue;
                    };
                    let media_t = Self::playhead_media_t(clip_ref, playhead);
                    let on = p.eval(media_t) >= 0.5;
                    let row_rect = Rect::new(x + 30.0, y, left_w - 38.0, ROW_H);
                    let id = ui.id(("fx.toggle", &clip_ref.id, pref));
                    let it = ui.interact(id, row_rect);
                    let box_rect = Rect::new(row_rect.x, y + (ROW_H - 14.0) / 2.0, 14.0, 14.0);
                    ui.fill_rounded(box_rect, theme::RADIUS_XS, theme::SURFACE_3);
                    ui.stroke_rounded(box_rect, theme::RADIUS_XS, 1.0, theme::LINE_STRONG);
                    if on {
                        ui.icon("check", box_rect, 12.0, theme::ACCENT);
                    }
                    let label = Rect::new(box_rect.right() + 8.0, y, row_rect.w - 22.0, ROW_H);
                    ui.text_left(
                        &meta.label,
                        label,
                        if it.hovered { theme::TEXT_1 } else { theme::TEXT_2 },
                        FontKind::Sans12,
                    );
                    if it.hovered {
                        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                    }
                    if it.clicked {
                        acts.push(Act::SetValue(
                            clip_ref.id.clone(),
                            pref.clone(),
                            if on { 0.0 } else { 1.0 },
                        ));
                    }
                    y += ROW_H;
                }
                Row::ColorParam { clip, fx_idx, p_idx } => {
                    let clip_ref = &clips[*clip];
                    let Some(inst) = clip_ref.effects.get(*fx_idx) else {
                        y += ROW_H;
                        continue;
                    };
                    let media_t = Self::playhead_media_t(clip_ref, playhead);
                    let mut inner = Rect::new(x + 8.0, y, left_w - 16.0, ROW_H);
                    inner.cut_left(24.0); // Einzug (keine Stopwatch)
                    let label_cell = inner.cut_left(48.0);
                    ui.text_left("Farbe", label_cell, theme::TEXT_2, FontKind::Sans12);
                    inner.cut_left(4.0);
                    // Aktuelle Farbe als Swatch.
                    let rgb: Vec<u8> = (0..3)
                        .map(|ch| {
                            inst.params
                                .get(p_idx + ch)
                                .map(|p| p.eval(media_t))
                                .unwrap_or(0.0)
                                .clamp(0.0, 255.0) as u8
                        })
                        .collect();
                    let swatch = Rect::new(inner.x, y + 5.0, 22.0, ROW_H - 10.0);
                    inner.cut_left(26.0);
                    ui.fill_rounded(swatch, theme::RADIUS_XS, raylib::color::Color::new(rgb[0], rgb[1], rgb[2], 255));
                    ui.stroke_rounded(swatch, theme::RADIUS_XS, 1.0, theme::LINE_STRONG);
                    // Pipette: nächster Klick in den Programmmonitor nimmt
                    // die Quellfarbe des Clips auf.
                    let pick_active = app.app.color_pick.as_ref().is_some_and(|r| {
                        r.clip_id == clip_ref.id && r.fx_id == inst.id && r.p_idx == *p_idx
                    });
                    let pipette = Rect::new(inner.x, y + (ROW_H - 18.0) / 2.0, 18.0, 18.0);
                    inner.cut_left(22.0);
                    let pit = IconButton::new("pipette")
                        .size(12.0)
                        .active(pick_active)
                        .tooltip("Farbe im Programmmonitor aufnehmen")
                        .show(ui, ("fx.pipette", &inst.id, p_idx), pipette);
                    if pit.clicked {
                        app.app.color_pick = if pick_active {
                            None
                        } else {
                            Some(crate::stores::ColorPickRequest {
                                clip_id: clip_ref.id.clone(),
                                fx_id: inst.id.clone(),
                                p_idx: *p_idx,
                            })
                        };
                    }
                    // R/G/B-Zellen (Scrubbing + Doppelklick-Eingabe).
                    for (ch, ch_label) in ["R", "G", "B"].iter().enumerate() {
                        let pref = ParamRef::Effect { fx_id: inst.id.clone(), index: p_idx + ch };
                        let cell = Rect::new(inner.x, y + 3.0, 44.0, ROW_H - 6.0);
                        inner.cut_left(48.0);
                        let value = rgb[ch] as f64;
                        let editing_here = matches!(&self.edit, Some((cid, pid, _)) if cid == &clip_ref.id && *pid == pref);
                        if editing_here {
                            let mut taken = self.edit.take().expect("edit state");
                            let res = taken.2.show(ui, ("fx.edit", &clip_ref.id, &pref), cell, "");
                            let lost_focus = !res.focused;
                            if res.submitted || lost_focus {
                                if let Some(v) = parse_value(&taken.2.text) {
                                    acts.push(Act::CommitEdit(clip_ref.id.clone(), pref.clone(), v));
                                }
                                if res.submitted {
                                    ui.persist.keyboard_focus = 0;
                                }
                            } else {
                                self.edit = Some(taken);
                            }
                        } else {
                            let vid = ui.id(("fx.color", &clip_ref.id, &pref));
                            let vit = ui.interact(vid, cell);
                            ui.fill_rounded(cell, theme::RADIUS_SM, theme::SURFACE_3);
                            ui.stroke_rounded(
                                cell,
                                theme::RADIUS_SM,
                                1.0,
                                if vit.hovered { theme::LINE_STRONG } else { theme::LINE },
                            );
                            ui.text_left(ch_label, Rect::new(cell.x + 5.0, cell.y, 12.0, cell.h), theme::TEXT_3, FontKind::Mono11);
                            ui.text_right(
                                &format!("{}", rgb[ch]),
                                cell.inset_xy(5.0, 0.0),
                                theme::TEXT_1,
                                FontKind::Mono12,
                            );
                            if vit.hovered {
                                ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
                            }
                            if vit.double_clicked {
                                acts.push(Act::OpenEdit(clip_ref.id.clone(), pref.clone(), value, 0));
                            } else if ui.input.left_pressed && vit.hovered && self.value_drag.is_none() {
                                acts.push(Act::BeginValueDrag(ValueDrag {
                                    clip_id: clip_ref.id.clone(),
                                    pref: pref.clone(),
                                    start_value: value,
                                    start_x: ui.input.mouse.x,
                                    step: 1.0,
                                    history_pushed: false,
                                }));
                            }
                        }
                    }
                    y += ROW_H;
                }
                Row::Param { clip, pref } => {
                    let clip_ref = &clips[*clip];
                    let Some((meta, p)) = param_meta(clip_ref, pref) else {
                        y += ROW_H;
                        continue;
                    };
                    let media_t = Self::playhead_media_t(clip_ref, playhead);
                    let value = p.eval(media_t);
                    let animated = p.is_animated();
                    let is_effect = matches!(pref, ParamRef::Effect { .. });
                    let mut inner = Rect::new(x + 8.0, y, left_w - 16.0, ROW_H);
                    if is_effect {
                        // Effekt-Parameter leicht einrücken (unter dem Header).
                        inner.cut_left(8.0);
                    }

                    // -- Stopwatch --
                    let sw = inner.cut_left(20.0);
                    if meta.animatable {
                        let sw_rect = Rect::new(sw.x, y + (ROW_H - 18.0) / 2.0, 18.0, 18.0);
                        let sw_id = ui.id(("fx.sw", &clip_ref.id, pref));
                        let sw_it = ui.interact(sw_id, sw_rect);
                        let sw_color = if animated {
                            theme::ACCENT
                        } else if sw_it.hovered {
                            theme::TEXT_1
                        } else {
                            theme::with_alpha(theme::TEXT_3, 180)
                        };
                        ui.icon("timer", sw_rect, 14.0, sw_color);
                        if sw_it.hovered {
                            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                            ui.tooltip(sw_id, sw_rect, "Animation umschalten (Keyframes)");
                        }
                        if sw_it.clicked {
                            acts.push(Act::ToggleAnimated(clip_ref.id.clone(), pref.clone(), media_t));
                        }
                    }
                    inner.cut_left(4.0);

                    // -- Label --
                    let label_cell = inner.cut_left(if is_effect { 104.0 } else { 92.0 });
                    let display = ui.font(FontKind::Sans12).ellipsize(&meta.label, label_cell.w);
                    ui.text_left(&display, label_cell, theme::TEXT_2, FontKind::Sans12);
                    if ui.mouse_in(label_cell) && meta.label.len() > 14 {
                        let lid = ui.id(("fx.label", &clip_ref.id, pref));
                        ui.tooltip(lid, label_cell, &meta.label);
                    }
                    inner.cut_left(6.0);

                    // -- Keyframe-Navigation (rechtsbündig) --
                    if animated {
                        let nav = inner.cut_right(58.0);
                        let mut nx = nav.x;
                        let prev_t = p.prev_key_time(media_t);
                        let next_t = p.next_key_time(media_t);
                        let on_key = p.key_index_at(media_t).is_some();
                        // ◀
                        let b = Rect::new(nx, y + (ROW_H - 18.0) / 2.0, 18.0, 18.0);
                        let it = IconButton::new("chevron-left")
                            .size(13.0)
                            .disabled(prev_t.is_none())
                            .tooltip("Zum vorherigen Keyframe")
                            .show(ui, ("fx.prev", &clip_ref.id, pref), b);
                        if it.clicked {
                            if let Some(t) = prev_t {
                                acts.push(Act::SeekTo(clip_ref.start + (t - clip_ref.src_in)));
                            }
                        }
                        nx += 20.0;
                        // ◆ (setzen/entfernen)
                        let b = Rect::new(nx, y + (ROW_H - 18.0) / 2.0, 18.0, 18.0);
                        let kid = ui.id(("fx.key", &clip_ref.id, pref));
                        let kit = ui.interact(kid, b);
                        let (fill, line) = if on_key {
                            (theme::ACCENT, theme::ACCENT)
                        } else if kit.hovered {
                            (theme::SURFACE_1, theme::TEXT_1)
                        } else {
                            (theme::SURFACE_1, theme::TEXT_3)
                        };
                        draw_diamond(ui, b.x + 9.0, b.y + 9.0, 4.5, fill, line);
                        if kit.hovered {
                            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                            ui.tooltip(kid, b, "Keyframe setzen/entfernen");
                        }
                        if kit.clicked {
                            acts.push(Act::ToggleKeyframe(clip_ref.id.clone(), pref.clone(), media_t));
                        }
                        nx += 20.0;
                        // ▶
                        let b = Rect::new(nx, y + (ROW_H - 18.0) / 2.0, 18.0, 18.0);
                        let it = IconButton::new("chevron-right")
                            .size(13.0)
                            .disabled(next_t.is_none())
                            .tooltip("Zum nächsten Keyframe")
                            .show(ui, ("fx.next", &clip_ref.id, pref), b);
                        if it.clicked {
                            if let Some(t) = next_t {
                                acts.push(Act::SeekTo(clip_ref.start + (t - clip_ref.src_in)));
                            }
                        }
                        inner.cut_right(6.0);
                    }

                    // -- Wert (Scrubbing / Inline-Eingabe) --
                    let unit = meta.unit;
                    let unit_w = if unit.is_empty() { 0.0 } else { ui.font(FontKind::Sans12).width(unit) + 4.0 };
                    let unit_cell = inner.cut_right(unit_w);
                    let value_cell = Rect::new(inner.x, y + 3.0, inner.w.min(72.0), ROW_H - 6.0);
                    if !unit.is_empty() {
                        ui.text_left(unit, unit_cell, theme::TEXT_3, FontKind::Sans12);
                    }

                    let editing_here = matches!(&self.edit, Some((cid, pid, _)) if cid == &clip_ref.id && pid == pref);
                    if editing_here {
                        let mut taken = self.edit.take().expect("edit state");
                        let res = taken.2.show(ui, ("fx.edit", &clip_ref.id, pref), value_cell, "");
                        let lost_focus = !res.focused;
                        if res.submitted || lost_focus {
                            if let Some(v) = parse_value(&taken.2.text) {
                                acts.push(Act::CommitEdit(clip_ref.id.clone(), pref.clone(), v));
                            }
                            if res.submitted {
                                ui.persist.keyboard_focus = 0;
                            }
                        } else {
                            self.edit = Some(taken);
                        }
                    } else {
                        let vid = ui.id(("fx.value", &clip_ref.id, pref));
                        let vit = ui.interact(vid, value_cell);
                        ui.fill_rounded(value_cell, theme::RADIUS_SM, theme::SURFACE_3);
                        ui.stroke_rounded(
                            value_cell,
                            theme::RADIUS_SM,
                            1.0,
                            if vit.hovered { theme::LINE_STRONG } else { theme::LINE },
                        );
                        let col = if animated { theme::ACCENT_HOVER } else { theme::TEXT_1 };
                        ui.text_right(
                            &fmt_value(value, meta.decimals),
                            value_cell.inset_xy(6.0, 0.0),
                            col,
                            FontKind::Mono12,
                        );
                        if vit.hovered {
                            ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
                            ui.tooltip(vid, value_cell, "Ziehen ändert den Wert — Doppelklick zum Eingeben");
                        }
                        if vit.double_clicked {
                            acts.push(Act::OpenEdit(clip_ref.id.clone(), pref.clone(), value, meta.decimals));
                        } else if ui.input.left_pressed && vit.hovered && self.value_drag.is_none()
                        {
                            acts.push(Act::BeginValueDrag(ValueDrag {
                                clip_id: clip_ref.id.clone(),
                                pref: pref.clone(),
                                start_value: value,
                                start_x: ui.input.mouse.x,
                                step: meta.step,
                                history_pushed: false,
                            }));
                        }
                        if vit.right_clicked {
                            app.context_menu.show(
                                ui.input.mouse.x,
                                ui.input.mouse.y,
                                vec![MenuEntry::Item(
                                    MenuItem::custom(
                                        "Parameter zurücksetzen",
                                        CustomAction::FxResetParam {
                                            clip_id: clip_ref.id.clone(),
                                            pref: pref.clone(),
                                        },
                                    )
                                    .with_icon("rotate-ccw"),
                                )],
                            );
                        }
                    }

                    // -- Keyframe-Spur --
                    if has_lanes && meta.animatable {
                        let lane = Rect::new(lane_x, y, lane_w, ROW_H);
                        ui.fill(lane, theme::with_alpha(theme::SURFACE_0, 120));
                        ui.hline(lane.x, lane.bottom() - 1.0, lane.w, theme::LINE);
                        let cy = y + ROW_H / 2.0;
                        for k in &p.keyframes {
                            let kx = t_to_x(lane, clip_ref, k.t);
                            if kx < lane.x - KEY_R || kx > lane.right() + KEY_R {
                                continue;
                            }
                            let sel = self.is_selected(&clip_ref.id, pref, k.t);
                            let hit = Rect::new(kx - KEY_HIT, cy - KEY_HIT, KEY_HIT * 2.0, KEY_HIT * 2.0);
                            let hovered = ui.mouse_in(hit) && self.key_drag.is_none();
                            if hovered {
                                hover_any_key = true;
                                ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                            }
                            let (fill, line) = if sel {
                                (theme::ACCENT, theme::WHITE)
                            } else if hovered {
                                (theme::TEXT_1, theme::TEXT_1)
                            } else {
                                (theme::TEXT_2, theme::SURFACE_0)
                            };
                            draw_diamond(ui, kx, cy, KEY_R, fill, line);
                            let sel_key = SelKey {
                                clip_id: clip_ref.id.clone(),
                                pref: pref.clone(),
                                t: k.t,
                            };
                            key_positions.push((sel_key.clone(), v2(kx, cy)));
                            if hovered && ui.input.left_pressed && ui.nothing_active() {
                                acts.push(Act::SelectKey {
                                    key: sel_key.clone(),
                                    additive: ui.input.shift,
                                    toggle: ui.input.ctrl || ui.input.meta,
                                });
                                if !(ui.input.ctrl || ui.input.meta) {
                                    acts.push(Act::StartKeyDrag);
                                }
                            }
                            if hovered && ui.input.right_pressed {
                                acts.push(Act::OpenKeyMenu { key: sel_key });
                            }
                        }
                        // Doppelklick auf leere Spur: Keyframe anlegen.
                        if ui.mouse_in(lane) && ui.input.double_click && !hover_any_key {
                            let t = x_to_media_t(lane, clip_ref, ui.input.mouse.x);
                            acts.push(Act::AddKeyframe(clip_ref.id.clone(), pref.clone(), t));
                        }
                    }

                    y += ROW_H;
                }
            }
        }

        self.scroll.end(ui, area, 0.0, content_h);

        // ---- Lineal + Playhead über den Spuren ----
        if has_lanes {
            ui.fill(ruler, theme::SURFACE_2);
            ui.hline(ruler.x, ruler.bottom() - 1.0, ruler.w, theme::LINE);
            // Sekundenmarken (clip-lokal).
            let dur = primary.duration.max(1e-9);
            let step = if dur > 60.0 { 10.0 } else if dur > 20.0 { 5.0 } else { 1.0 };
            let mut m = 0.0;
            while m <= dur + 1e-9 {
                let mx = ruler.x + (m / dur) as f32 * ruler.w;
                ui.vline(mx, ruler.bottom() - 6.0, 5.0, theme::LINE_STRONG);
                m += step;
            }
            let rid = ui.id("fx.ruler");
            let rit = ui.interact(rid, ruler);
            if rit.hovered || self.ruler_drag {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
            }
            if rit.hovered && ui.input.left_pressed {
                self.ruler_drag = true;
            }
            if self.ruler_drag {
                if ui.input.left_down {
                    let frac = ((ui.input.mouse.x - ruler.x) / ruler.w).clamp(0.0, 1.0) as f64;
                    acts.push(Act::SeekTo(primary.start + frac * primary.duration));
                } else {
                    self.ruler_drag = false;
                }
            }
            // Playhead-Linie über Lineal + Spuren.
            let local = ((playhead - primary.start) / dur).clamp(0.0, 1.0) as f32;
            let px = ruler.x + local * ruler.w;
            ui.fill(Rect::new(px - 0.5, ruler.y, 1.5, ruler.h + lanes_full.h), theme::ACCENT);
            // Griff im Lineal
            ui.triangle(
                v2(px - 5.0, ruler.y),
                v2(px + 5.0, ruler.y),
                v2(px, ruler.y + 7.0),
                theme::ACCENT,
            );
        }

        // ---- Box-Auswahl über den Spuren ----
        if has_lanes && self.key_drag.is_none() {
            if ui.mouse_in(lanes_full)
                && ui.input.left_pressed
                && !hover_any_key
                && ui.nothing_active()
                && !ui.input.double_click
                && self.value_drag.is_none()
                && !self.ruler_drag
            {
                self.box_select = Some(ui.input.mouse);
            }
            if let Some(origin) = self.box_select {
                if ui.input.left_down {
                    let m = ui.input.mouse;
                    let sel_rect = Rect::new(
                        origin.x.min(m.x),
                        origin.y.min(m.y),
                        (origin.x - m.x).abs(),
                        (origin.y - m.y).abs(),
                    );
                    ui.fill(sel_rect, theme::with_alpha(theme::ACCENT, 30));
                    ui.stroke(sel_rect, 1.0, theme::ACCENT);
                } else {
                    // Abschluss: Keys im Rechteck auswählen.
                    let m = ui.input.mouse;
                    let sel_rect = Rect::new(
                        origin.x.min(m.x),
                        origin.y.min(m.y),
                        (origin.x - m.x).abs(),
                        (origin.y - m.y).abs(),
                    );
                    let additive = ui.input.shift || ui.input.ctrl || ui.input.meta;
                    if !additive {
                        self.selected_keys.clear();
                    }
                    for (key, pos) in &key_positions {
                        if sel_rect.contains(*pos)
                            && !self.is_selected(&key.clip_id, &key.pref, key.t)
                        {
                            self.selected_keys.push(key.clone());
                        }
                    }
                    self.box_select = None;
                }
            }
        }

        // ---- Drag-to-Reorder des Effekt-Stapels ----
        if let Some(mut re) = self.fx_reorder.take() {
            let my = ui.input.mouse.y;
            if (my - re.start_y).abs() > DRAG_THRESHOLD {
                re.moved = true;
            }
            // Zielindex = Anzahl der ANDEREN Header derselben Spur oberhalb.
            let dest = fx_spans
                .iter()
                .filter(|(c, _, fid, yy)| {
                    *c == re.clip && *fid != re.fx_id && *yy + EFFECT_H / 2.0 < my
                })
                .count();
            if ui.input.left_down {
                if re.moved {
                    ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_NS);
                    // Einfügelinie an der Lücke zwischen den anderen Headern.
                    let mut others: Vec<f32> = fx_spans
                        .iter()
                        .filter(|(c, _, fid, _)| *c == re.clip && *fid != re.fx_id)
                        .map(|(_, _, _, yy)| *yy)
                        .collect();
                    others.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    let line_y = if dest < others.len() {
                        others[dest]
                    } else {
                        others.last().map(|t| t + EFFECT_H).unwrap_or(my)
                    };
                    ui.fill(Rect::new(x, line_y - 1.0, left_w - 8.0, 2.0), theme::ACCENT);
                }
                self.fx_reorder = Some(re);
            } else {
                // Loslassen: bei Bewegung umsortieren (ein Undo-Schritt).
                if re.moved {
                    if let Some(clip) = clips.get(re.clip) {
                        acts.push(Act::EffectReorder(clip.id.clone(), re.fx_id.clone(), dest));
                    }
                }
            }
        }

        // ---- Drop-Ziel: Effekt aus dem Effekte-Panel ----
        if let Some(DragPayload::Effect(_)) = ui.drag_over(rect) {
            ui.stroke(rect.inset_xy(1.0, 1.0), 2.0, theme::ACCENT);
        }
        if let Some(DragPayload::Effect(kind)) = ui.accept_drop(rect) {
            app.timeline.effects_add(&primary.id, kind);
        }

        // ---- Aktionen anwenden ----
        for act in acts {
            match act {
                Act::ToggleAnimated(id, pref, t) => {
                    app.timeline.kf_toggle_animated(&id, &pref, t)
                }
                Act::ToggleKeyframe(id, pref, t) => {
                    app.timeline.kf_toggle_keyframe(&id, &pref, t)
                }
                Act::SeekTo(t) => app.timeline.set_playhead(t),
                Act::BeginValueDrag(d) => self.value_drag = Some(d),
                Act::OpenEdit(id, pref, value, decimals) => {
                    let mut state = TextInputState::default();
                    state.set_text(fmt_value(value, decimals));
                    let edit_id = ui.id(("fx.edit", &id, &pref));
                    ui.persist.keyboard_focus = edit_id;
                    self.edit = Some((id, pref, state));
                }
                Act::CommitEdit(id, pref, v) => {
                    let clip = clips.iter().find(|c| c.id == id);
                    if let Some(clip) = clip {
                        let t = Self::playhead_media_t(clip, playhead);
                        app.timeline.begin_fx_edit();
                        app.timeline.kf_set_value_live(&id, &pref, t, v);
                    }
                    self.edit = None;
                }
                Act::SetValue(id, pref, v) => {
                    let clip = clips.iter().find(|c| c.id == id);
                    if let Some(clip) = clip {
                        let t = Self::playhead_media_t(clip, playhead);
                        app.timeline.begin_fx_edit();
                        app.timeline.kf_set_value_live(&id, &pref, t, v);
                    }
                }
                Act::Reset(kind, clip_idx) => {
                    let id = clips[clip_idx].id.clone();
                    match kind {
                        ResetKind::Motion => app.timeline.fx_reset_motion(&[id]),
                        ResetKind::Opacity => app.timeline.fx_reset_param(&id, ParamId::Opacity),
                        ResetKind::Audio => app.timeline.fx_reset_param(&id, ParamId::VolumeDb),
                    }
                    self.selected_keys.clear();
                }
                Act::SetUniform(id, uniform) => {
                    app.timeline.fx_set_uniform_scale(&id, uniform)
                }
                Act::SelectKey { key, additive, toggle } => {
                    let already = self.is_selected(&key.clip_id, &key.pref, key.t);
                    if toggle {
                        if already {
                            self.selected_keys
                                .retain(|k| !k.matches(&key.clip_id, &key.pref, key.t));
                        } else {
                            self.selected_keys.push(key);
                        }
                    } else if additive {
                        if !already {
                            self.selected_keys.push(key);
                        }
                    } else if !already {
                        self.selected_keys = vec![key];
                    }
                }
                Act::StartKeyDrag => {
                    // Originalkurven aller betroffenen Parameter sichern.
                    let mut curves: Vec<(String, ParamRef, Vec<Keyframe>)> = Vec::new();
                    for sel in &self.selected_keys {
                        if !curves
                            .iter()
                            .any(|(c, p, _)| c == &sel.clip_id && p == &sel.pref)
                        {
                            if let Some(clip) = clips.iter().find(|c| c.id == sel.clip_id) {
                                if let Some(p) = TimelineStore::clip_param(clip, &sel.pref) {
                                    curves.push((
                                        sel.clip_id.clone(),
                                        sel.pref.clone(),
                                        p.keyframes.clone(),
                                    ));
                                }
                            }
                        }
                    }
                    self.key_drag = Some(KeyDrag {
                        start_mouse: ui.input.mouse,
                        curves,
                        orig_sel: self.selected_keys.clone(),
                        history_pushed: false,
                    });
                }
                Act::AddKeyframe(id, pref, t) => {
                    app.timeline.kf_toggle_keyframe(&id, &pref, t);
                }
                Act::EffectToggle(clip_id, fx_id) => {
                    app.timeline.effects_toggle_enabled(&clip_id, &fx_id);
                }
                Act::EffectMove(clip_id, fx_id, delta) => {
                    app.timeline.effects_move(&clip_id, &fx_id, delta);
                }
                Act::EffectReorder(clip_id, fx_id, dest) => {
                    app.timeline.effects_reorder(&clip_id, &fx_id, dest);
                }
                Act::EffectRemove(clip_id, fx_id) => {
                    // Bearbeitete Maske dieses Effekts ggf. abwählen.
                    if app
                        .app
                        .active_mask
                        .as_ref()
                        .is_some_and(|s| s.clip_id == clip_id && s.fx_id == fx_id)
                    {
                        app.app.active_mask = None;
                    }
                    app.timeline.effects_remove(&clip_id, &fx_id);
                    self.selected_keys.clear();
                }
                Act::EffectReset(clip_id, fx_id) => {
                    app.timeline.effects_reset(&clip_id, &fx_id);
                    self.selected_keys.clear();
                }
                Act::EffectCollapse(fx_id) => {
                    if !self.collapsed_fx.remove(&fx_id) {
                        self.collapsed_fx.insert(fx_id);
                    }
                }
                Act::OpenEffectMenu { clip_id, fx_id, fx_idx, count } => {
                    app.context_menu.show(
                        ui.input.mouse.x,
                        ui.input.mouse.y,
                        vec![
                            MenuEntry::Item(
                                MenuItem::custom(
                                    "Effekt umgehen (Bypass)",
                                    CustomAction::EffectsToggle {
                                        clip_id: clip_id.clone(),
                                        fx_id: fx_id.clone(),
                                    },
                                )
                                .with_icon("zap"),
                            ),
                            MenuEntry::Separator,
                            MenuEntry::Item(
                                MenuItem::custom(
                                    "Im Stapel nach oben",
                                    CustomAction::EffectsMove {
                                        clip_id: clip_id.clone(),
                                        fx_id: fx_id.clone(),
                                        delta: -1,
                                    },
                                )
                                .with_icon("chevron-up")
                                .with_disabled(fx_idx == 0),
                            ),
                            MenuEntry::Item(
                                MenuItem::custom(
                                    "Im Stapel nach unten",
                                    CustomAction::EffectsMove {
                                        clip_id: clip_id.clone(),
                                        fx_id: fx_id.clone(),
                                        delta: 1,
                                    },
                                )
                                .with_icon("chevron-down")
                                .with_disabled(fx_idx + 1 >= count),
                            ),
                            MenuEntry::Separator,
                            MenuEntry::Item(
                                MenuItem::custom(
                                    "Effekt zurücksetzen",
                                    CustomAction::EffectsReset {
                                        clip_id: clip_id.clone(),
                                        fx_id: fx_id.clone(),
                                    },
                                )
                                .with_icon("rotate-ccw"),
                            ),
                            MenuEntry::Item(
                                MenuItem::custom(
                                    "Effekt entfernen",
                                    CustomAction::EffectsRemove { clip_id, fx_id },
                                )
                                .with_icon("trash-2")
                                .with_danger(),
                            ),
                        ],
                    );
                }
                Act::OpenKeyMenu { key } => {
                    if !self.is_selected(&key.clip_id, &key.pref, key.t) {
                        self.selected_keys = vec![key.clone()];
                    }
                    let keys: Vec<(String, ParamRef, f64)> = self
                        .selected_keys
                        .iter()
                        .map(|k| (k.clip_id.clone(), k.pref.clone(), k.t))
                        .collect();
                    // Aktuelle Interpolation des angeklickten Keys (Häkchen).
                    let current = clips
                        .iter()
                        .find(|c| c.id == key.clip_id)
                        .and_then(|c| {
                            let p = TimelineStore::clip_param(c, &key.pref)?;
                            p.key_index_at(key.t).map(|i| p.keyframes[i].interp)
                        });
                    let interp_items: Vec<MenuEntry> = crate::core::animation::Interp::ALL
                        .iter()
                        .map(|i| {
                            MenuEntry::Item(
                                MenuItem::custom(
                                    i.label(),
                                    CustomAction::FxSetInterp {
                                        keys: keys.clone(),
                                        interp: *i,
                                    },
                                )
                                .with_checked(current == Some(*i)),
                            )
                        })
                        .collect();
                    let n = self.selected_keys.len();
                    let del_label = if n > 1 {
                        format!("{n} Keyframes löschen")
                    } else {
                        "Keyframe löschen".to_string()
                    };
                    app.context_menu.show(
                        ui.input.mouse.x,
                        ui.input.mouse.y,
                        vec![
                            MenuEntry::Submenu {
                                label: "Interpolation".into(),
                                icon: Some("audio-waveform"),
                                items: interp_items,
                            },
                            MenuEntry::Separator,
                            MenuEntry::Item(
                                MenuItem::custom(
                                    &del_label,
                                    CustomAction::FxRemoveKeyframes { keys },
                                )
                                .with_icon("trash-2")
                                .with_danger(),
                            ),
                        ],
                    );
                }
                Act::MaskAdd(clip_id, fx_id, shape) => {
                    if let Some(mask_id) = app.timeline.mask_add(&clip_id, &fx_id, shape) {
                        app.app.active_mask = Some(crate::stores::MaskSelection {
                            clip_id,
                            fx_id,
                            mask_id,
                        });
                    }
                }
                Act::MaskEdit(clip_id, fx_id, mask_id) => {
                    // Bearbeiten umschalten: zweiter Klick auf dieselbe Maske beendet.
                    let same = app
                        .app
                        .active_mask
                        .as_ref()
                        .is_some_and(|s| s.mask_id == mask_id);
                    app.app.active_mask = if same {
                        None
                    } else {
                        Some(crate::stores::MaskSelection { clip_id, fx_id, mask_id })
                    };
                }
                Act::MaskRemove(clip_id, fx_id, mask_id) => {
                    app.timeline.mask_remove(&clip_id, &fx_id, &mask_id);
                    if app
                        .app
                        .active_mask
                        .as_ref()
                        .is_some_and(|s| s.mask_id == mask_id)
                    {
                        app.app.active_mask = None;
                    }
                }
                Act::MaskToggleInvert(clip_id, fx_id, mask_id) => {
                    app.timeline.mask_toggle_invert(&clip_id, &fx_id, &mask_id);
                }
                Act::MaskToggleEnabled(clip_id, fx_id, mask_id) => {
                    app.timeline.mask_toggle_enabled(&clip_id, &fx_id, &mask_id);
                }
            }
        }

        // ---- Wert-Scrubbing fortführen ----
        if let Some(drag) = &mut self.value_drag {
            if ui.input.left_down {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
                let dx = ui.input.mouse.x - drag.start_x;
                if dx.abs() >= 1.0 || drag.history_pushed {
                    if !drag.history_pushed {
                        app.timeline.begin_fx_edit();
                        drag.history_pushed = true;
                    }
                    let mut step = drag.step;
                    if ui.input.shift {
                        step *= 10.0;
                    } else if ui.input.ctrl || ui.input.meta {
                        step *= 0.1;
                    }
                    let v = drag.start_value + dx as f64 * step;
                    if let Some(clip) = clips.iter().find(|c| c.id == drag.clip_id) {
                        let t = Self::playhead_media_t(clip, playhead);
                        app.timeline.kf_set_value_live(&drag.clip_id, &drag.pref, t, v);
                    }
                }
            } else {
                self.value_drag = None;
            }
        }

        // ---- Keyframe-Drag fortführen ----
        if let Some(drag) = &mut self.key_drag {
            if ui.input.left_down {
                let dx = ui.input.mouse.x - drag.start_mouse.x;
                if dx.abs() >= DRAG_THRESHOLD || drag.history_pushed {
                    if !drag.history_pushed {
                        app.timeline.begin_fx_edit();
                        drag.history_pushed = true;
                    }
                    for (clip_id, pref, orig) in &drag.curves {
                        let Some(clip) = clips.iter().find(|c| c.id == *clip_id) else {
                            continue;
                        };
                        let dt = dx as f64 / lane_w.max(1.0) as f64 * clip.duration;
                        let moved: Vec<Keyframe> = orig
                            .iter()
                            .map(|k| {
                                let selected = drag
                                    .orig_sel
                                    .iter()
                                    .any(|s| s.matches(clip_id, pref, k.t));
                                if selected {
                                    Keyframe {
                                        t: (k.t + dt).max(0.0),
                                        ..*k
                                    }
                                } else {
                                    *k
                                }
                            })
                            .collect();
                        app.timeline.kf_replace_keys_live(clip_id, pref, moved);
                    }
                    // Auswahl folgt den verschobenen Keys.
                    self.selected_keys = drag
                        .orig_sel
                        .iter()
                        .map(|s| {
                            let dt = clips
                                .iter()
                                .find(|c| c.id == s.clip_id)
                                .map(|c| dx as f64 / lane_w.max(1.0) as f64 * c.duration)
                                .unwrap_or(0.0);
                            SelKey {
                                clip_id: s.clip_id.clone(),
                                pref: s.pref.clone(),
                                t: (s.t + dt).max(0.0),
                            }
                        })
                        .collect();
                }
            } else {
                self.key_drag = None;
            }
        }

        // ---- Entf/Backspace: ausgewählte Keyframes löschen ----
        if app.app.focused_panel == "effectControls"
            && self.edit.is_none()
            && !self.selected_keys.is_empty()
        {
            let delete = ui.input.keys.iter().any(|k| {
                matches!(k.key, KeyboardKey::KEY_DELETE | KeyboardKey::KEY_BACKSPACE)
            });
            if delete {
                let mut by_clip: std::collections::HashMap<String, Vec<(ParamRef, f64)>> =
                    Default::default();
                for k in &self.selected_keys {
                    by_clip
                        .entry(k.clip_id.clone())
                        .or_default()
                        .push((k.pref.clone(), k.t));
                }
                for (clip_id, keys) in by_clip {
                    app.timeline.kf_remove_keyframes(&clip_id, &keys);
                }
                self.selected_keys.clear();
            }
        }
    }
}
