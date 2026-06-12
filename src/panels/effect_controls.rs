//! Effekteinstellungen: Premiere-artiger Keyframe-Editor für den ersten
//! ausgewählten Timeline-Clip. Links Parameter-Zeilen (Stopwatch, Wert-
//! Scrubbing mit Doppelklick-Eingabe, Keyframe-Navigation ◀ ◆ ▶), rechts
//! je Parameter eine Keyframe-Spur über die Clipdauer mit Playhead-Lineal.
//! Keyframes: ziehen (auch Mehrfachauswahl), Strg/Shift-Klick, Box-Auswahl,
//! Doppelklick legt neue an, Entf löscht, Rechtsklick öffnet das
//! Interpolations-Menü. Bei verknüpften A/V-Paaren erscheint zusätzlich die
//! Lautstärke des Audio-Partners.

use crate::core::animation::{AnimatedParam, Keyframe, ParamId, KF_TIME_EPS};
use crate::core::compose;
use crate::core::timeline::{TimelineClip, TrackKind};
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
use crate::ui::{FontKind, Ui};
use raylib::consts::{KeyboardKey, MouseCursor};
use raylib::math::Vector2;
use raylib::prelude::RaylibDraw;

const ROW_H: f32 = 26.0;
const SECTION_H: f32 = 32.0;
const RULER_H: f32 = 22.0;
/// Breite der linken Parameter-Spalte; rechts beginnen die Keyframe-Spuren.
const LEFT_W: f32 = 300.0;
const KEY_R: f32 = 5.0;
const KEY_HIT: f32 = 7.0;
const DRAG_THRESHOLD: f32 = 2.0;

/// Ausgewählter Keyframe (Medienzeit als Identität).
#[derive(Clone, Debug)]
struct SelKey {
    clip_id: String,
    param: ParamId,
    t: f64,
}

impl SelKey {
    fn matches(&self, clip_id: &str, param: ParamId, t: f64) -> bool {
        self.clip_id == clip_id && self.param == param && (self.t - t).abs() < KF_TIME_EPS
    }
}

/// Laufende Keyframe-Verschiebung: Originalkurven + Auswahl bei Gestenbeginn.
struct KeyDrag {
    start_mouse: Vector2,
    curves: Vec<(String, ParamId, Vec<Keyframe>)>,
    orig_sel: Vec<SelKey>,
    history_pushed: bool,
}

/// Laufendes Wert-Scrubbing einer Parameter-Zeile.
struct ValueDrag {
    clip_id: String,
    param: ParamId,
    start_value: f64,
    start_x: f32,
    history_pushed: bool,
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
        param: ParamId,
    },
    UniformToggle {
        clip: usize,
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
    scroll: ScrollState,
    selected_keys: Vec<SelKey>,
    key_drag: Option<KeyDrag>,
    value_drag: Option<ValueDrag>,
    /// Inline-Eingabe eines Werts: (Clip, Parameter, Feld).
    edit: Option<(String, ParamId, TextInputState)>,
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
            scroll: ScrollState::default(),
            selected_keys: Vec::new(),
            key_drag: None,
            value_drag: None,
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
    ui.d.draw_poly(v2(cx, cy), 4, r, 0.0, fill);
    ui.d.draw_poly_lines(v2(cx, cy), 4, r, 0.0, line);
}

/// Medienzeit ↔ x-Position in der Keyframe-Spur (Clip-lokal).
fn t_to_x(lane: Rect, clip: &TimelineClip, media_t: f64) -> f32 {
    let local = ((media_t - clip.src_in) / clip.duration.max(1e-9)) as f32;
    lane.x + local * lane.w
}

fn x_to_media_t(lane: Rect, clip: &TimelineClip, x: f32) -> f64 {
    let local = ((x - lane.x) / lane.w.max(1.0)) as f64;
    clip.src_in + local * clip.duration
}

impl EffectControlsPanel {
    /// Medienzeit des Playheads im Clip (für Werte/Keyframes geklemmt).
    fn playhead_media_t(clip: &TimelineClip, playhead: f64) -> f64 {
        compose::clip_media_time(clip, playhead)
            .clamp(clip.src_in, clip.src_in + clip.duration)
    }

    fn is_selected(&self, clip_id: &str, param: ParamId, t: f64) -> bool {
        self.selected_keys
            .iter()
            .any(|k| k.matches(clip_id, param, t))
    }

    /// Auswahl bereinigen: nur Keys behalten, die noch existieren.
    fn prune_selection(&mut self, clips: &[TimelineClip]) {
        self.selected_keys.retain(|sel| {
            clips.iter().any(|c| {
                c.id == sel.clip_id && c.fx.param(sel.param).key_index_at(sel.t).is_some()
            })
        });
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
                TrackKind::Video if video_idx.is_none() => {
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
                rows.push(Row::Param { clip: vi, param: ParamId::PosX });
                rows.push(Row::Param { clip: vi, param: ParamId::PosY });
                rows.push(Row::Param { clip: vi, param: ParamId::ScaleX });
                if !uniform {
                    rows.push(Row::Param { clip: vi, param: ParamId::ScaleY });
                }
                rows.push(Row::UniformToggle { clip: vi });
                rows.push(Row::Param { clip: vi, param: ParamId::Rotation });
            }
            rows.push(Row::Section {
                key: "fx.sec.opacity",
                title: "Deckkraft",
                reset: ResetKind::Opacity,
            });
            if self.open_opacity {
                rows.push(Row::Param { clip: vi, param: ParamId::Opacity });
            }
        }
        if let Some(ai) = audio_idx {
            rows.push(Row::Section {
                key: "fx.sec.audio",
                title: "Audio",
                reset: ResetKind::Audio,
            });
            if self.open_audio {
                rows.push(Row::Param { clip: ai, param: ParamId::VolumeDb });
            }
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
            crate::core::timecode::format_timecode(primary.start, crate::core::timeline::SEQUENCE_FPS),
            crate::core::timecode::format_timecode(primary.end(), crate::core::timeline::SEQUENCE_FPS)
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
                _ => ROW_H,
            })
            .sum::<f32>()
            + 8.0;
        let view = self.scroll.begin(ui, area, 0.0, content_h);
        let x = view.viewport.x;
        let mut y = view.origin_y;

        // Gesammelte Aktionen (nach dem Zeichnen ausgeführt — Borrow-Trennung).
        enum Act {
            ToggleAnimated(String, ParamId, f64),
            ToggleKeyframe(String, ParamId, f64),
            SeekTo(f64),
            BeginValueDrag(ValueDrag),
            OpenEdit(String, ParamId, f64),
            CommitEdit(String, ParamId, f64),
            Reset(ResetKind, usize),
            SetUniform(String, bool),
            SelectKey { key: SelKey, additive: bool, toggle: bool },
            StartKeyDrag,
            AddKeyframe(String, ParamId, f64),
            OpenKeyMenu { key: SelKey },
        }
        let mut acts: Vec<Act> = Vec::new();
        let mut hover_any_key = false;
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
                Row::Param { clip, param } => {
                    let clip_ref = &clips[*clip];
                    let param = *param;
                    let p: &AnimatedParam = clip_ref.fx.param(param);
                    let media_t = Self::playhead_media_t(clip_ref, playhead);
                    let value = p.eval(media_t);
                    let animated = p.is_animated();
                    let mut inner = Rect::new(x + 8.0, y, left_w - 16.0, ROW_H);

                    // Label-Anpassung: „Skalierung“ → „Skalierung X“, wenn entkoppelt.
                    let label = if param == ParamId::ScaleX && !clip_ref.fx.uniform_scale {
                        "Skalierung X"
                    } else {
                        param.label()
                    };

                    // -- Stopwatch --
                    let sw = inner.cut_left(20.0);
                    let sw_rect = Rect::new(sw.x, y + (ROW_H - 18.0) / 2.0, 18.0, 18.0);
                    let sw_id = ui.id(("fx.sw", &clip_ref.id, param));
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
                        acts.push(Act::ToggleAnimated(clip_ref.id.clone(), param, media_t));
                    }
                    inner.cut_left(4.0);

                    // -- Label --
                    let label_cell = inner.cut_left(92.0);
                    let display = ui.font(FontKind::Sans12).ellipsize(label, label_cell.w);
                    ui.text_left(&display, label_cell, theme::TEXT_2, FontKind::Sans12);
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
                            .show(ui, ("fx.prev", &clip_ref.id, param), b);
                        if it.clicked {
                            if let Some(t) = prev_t {
                                acts.push(Act::SeekTo(clip_ref.start + (t - clip_ref.src_in)));
                            }
                        }
                        nx += 20.0;
                        // ◆ (setzen/entfernen)
                        let b = Rect::new(nx, y + (ROW_H - 18.0) / 2.0, 18.0, 18.0);
                        let kid = ui.id(("fx.key", &clip_ref.id, param));
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
                            acts.push(Act::ToggleKeyframe(clip_ref.id.clone(), param, media_t));
                        }
                        nx += 20.0;
                        // ▶
                        let b = Rect::new(nx, y + (ROW_H - 18.0) / 2.0, 18.0, 18.0);
                        let it = IconButton::new("chevron-right")
                            .size(13.0)
                            .disabled(next_t.is_none())
                            .tooltip("Zum nächsten Keyframe")
                            .show(ui, ("fx.next", &clip_ref.id, param), b);
                        if it.clicked {
                            if let Some(t) = next_t {
                                acts.push(Act::SeekTo(clip_ref.start + (t - clip_ref.src_in)));
                            }
                        }
                        inner.cut_right(6.0);
                    }

                    // -- Wert (Scrubbing / Inline-Eingabe) --
                    let unit = param.unit();
                    let unit_w = if unit.is_empty() { 0.0 } else { ui.font(FontKind::Sans12).width(unit) + 4.0 };
                    let unit_cell = inner.cut_right(unit_w);
                    let value_cell = Rect::new(inner.x, y + 3.0, inner.w.min(72.0), ROW_H - 6.0);
                    if !unit.is_empty() {
                        ui.text_left(unit, unit_cell, theme::TEXT_3, FontKind::Sans12);
                    }

                    let editing_here = matches!(&self.edit, Some((cid, pid, _)) if cid == &clip_ref.id && *pid == param);
                    if editing_here {
                        let mut taken = self.edit.take().expect("edit state");
                        let res = taken.2.show(ui, ("fx.edit", &clip_ref.id, param), value_cell, "");
                        let lost_focus = !res.focused;
                        if res.submitted || lost_focus {
                            if let Some(v) = parse_value(&taken.2.text) {
                                acts.push(Act::CommitEdit(clip_ref.id.clone(), param, v));
                            }
                            if res.submitted {
                                ui.persist.keyboard_focus = 0;
                            }
                        } else {
                            self.edit = Some(taken);
                        }
                    } else {
                        let vid = ui.id(("fx.value", &clip_ref.id, param));
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
                            &fmt_value(value, param.decimals()),
                            value_cell.inset_xy(6.0, 0.0),
                            col,
                            FontKind::Mono12,
                        );
                        if vit.hovered {
                            ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
                            ui.tooltip(vid, value_cell, "Ziehen ändert den Wert — Doppelklick zum Eingeben");
                        }
                        if vit.double_clicked {
                            acts.push(Act::OpenEdit(clip_ref.id.clone(), param, value));
                        } else if ui.input.left_pressed && vit.hovered && self.value_drag.is_none()
                        {
                            acts.push(Act::BeginValueDrag(ValueDrag {
                                clip_id: clip_ref.id.clone(),
                                param,
                                start_value: value,
                                start_x: ui.input.mouse.x,
                                history_pushed: false,
                            }));
                        }
                    }

                    // -- Keyframe-Spur --
                    if has_lanes {
                        let lane = Rect::new(lane_x, y, lane_w, ROW_H);
                        ui.fill(lane, theme::with_alpha(theme::SURFACE_0, 120));
                        ui.hline(lane.x, lane.bottom() - 1.0, lane.w, theme::LINE);
                        let cy = y + ROW_H / 2.0;
                        for k in &p.keyframes {
                            let kx = t_to_x(lane, clip_ref, k.t);
                            if kx < lane.x - KEY_R || kx > lane.right() + KEY_R {
                                continue;
                            }
                            let sel = self.is_selected(&clip_ref.id, param, k.t);
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
                                param,
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
                            acts.push(Act::AddKeyframe(clip_ref.id.clone(), param, t));
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
            ui.d.draw_triangle(
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
                            && !self.is_selected(&key.clip_id, key.param, key.t)
                        {
                            self.selected_keys.push(key.clone());
                        }
                    }
                    self.box_select = None;
                }
            }
        }

        // ---- Aktionen anwenden ----
        for act in acts {
            match act {
                Act::ToggleAnimated(id, param, t) => {
                    app.timeline.fx_toggle_animated(&id, param, t)
                }
                Act::ToggleKeyframe(id, param, t) => {
                    app.timeline.fx_toggle_keyframe(&id, param, t)
                }
                Act::SeekTo(t) => app.timeline.set_playhead(t),
                Act::BeginValueDrag(d) => self.value_drag = Some(d),
                Act::OpenEdit(id, param, value) => {
                    let mut state = TextInputState::default();
                    state.set_text(fmt_value(value, param.decimals()));
                    let edit_id = ui.id(("fx.edit", &id, param));
                    ui.persist.keyboard_focus = edit_id;
                    self.edit = Some((id, param, state));
                }
                Act::CommitEdit(id, param, v) => {
                    let clip = clips.iter().find(|c| c.id == id);
                    if let Some(clip) = clip {
                        let t = Self::playhead_media_t(clip, playhead);
                        app.timeline.begin_fx_edit();
                        app.timeline.fx_set_value_live(&id, param, t, v);
                    }
                    self.edit = None;
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
                    let already = self.is_selected(&key.clip_id, key.param, key.t);
                    if toggle {
                        if already {
                            self.selected_keys
                                .retain(|k| !k.matches(&key.clip_id, key.param, key.t));
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
                    let mut curves: Vec<(String, ParamId, Vec<Keyframe>)> = Vec::new();
                    for sel in &self.selected_keys {
                        if !curves
                            .iter()
                            .any(|(c, p, _)| c == &sel.clip_id && *p == sel.param)
                        {
                            if let Some(clip) = clips.iter().find(|c| c.id == sel.clip_id) {
                                curves.push((
                                    sel.clip_id.clone(),
                                    sel.param,
                                    clip.fx.param(sel.param).keyframes.clone(),
                                ));
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
                Act::AddKeyframe(id, param, t) => {
                    app.timeline.fx_toggle_keyframe(&id, param, t);
                }
                Act::OpenKeyMenu { key } => {
                    if !self.is_selected(&key.clip_id, key.param, key.t) {
                        self.selected_keys = vec![key.clone()];
                    }
                    let keys: Vec<(String, ParamId, f64)> = self
                        .selected_keys
                        .iter()
                        .map(|k| (k.clip_id.clone(), k.param, k.t))
                        .collect();
                    // Aktuelle Interpolation des angeklickten Keys (Häkchen).
                    let current = clips
                        .iter()
                        .find(|c| c.id == key.clip_id)
                        .and_then(|c| {
                            let p = c.fx.param(key.param);
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
                    let mut step = drag.param.drag_step();
                    if ui.input.shift {
                        step *= 10.0;
                    } else if ui.input.ctrl || ui.input.meta {
                        step *= 0.1;
                    }
                    let v = drag.start_value + dx as f64 * step;
                    if let Some(clip) = clips.iter().find(|c| c.id == drag.clip_id) {
                        let t = Self::playhead_media_t(clip, playhead);
                        app.timeline.fx_set_value_live(&drag.clip_id, drag.param, t, v);
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
                    for (clip_id, param, orig) in &drag.curves {
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
                                    .any(|s| s.matches(clip_id, *param, k.t));
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
                        app.timeline.fx_replace_keys_live(clip_id, *param, moved);
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
                                param: s.param,
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
                let mut by_clip: std::collections::HashMap<String, Vec<(ParamId, f64)>> =
                    Default::default();
                for k in &self.selected_keys {
                    by_clip
                        .entry(k.clip_id.clone())
                        .or_default()
                        .push((k.param, k.t));
                }
                for (clip_id, keys) in by_clip {
                    app.timeline.fx_remove_keyframes(&clip_id, &keys);
                }
                self.selected_keys.clear();
            }
        }
    }
}
