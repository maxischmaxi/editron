//! Audio-Mixer: ein Kanalzug pro Audio-Spur der Sequenz plus Master-Summe.
//! Fader (dB), Stereo-Balance, Mute/Solo und Pegelmeter sind an die
//! Wiedergabe-Engine gebunden: Gains wirken live auf den Mixdown, die Meter
//! zeigen die echten Spitzenpegel aus `state.audio` (dBFS-Skala, −60..0,
//! mit Peak-Hold und Übersteuerungs-LED).

use crate::core::effects::EffectKind;
use crate::core::timeline::{track_name, TrackFlag, TrackKind};
use crate::overlays::context_menu::{CustomAction, MenuEntry, MenuItem};
use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::slider;
use crate::ui::{DragPayload, FontKind, Ui};
use raylib::consts::MouseCursor;
use std::collections::HashMap;

const STRIP_W: f32 = 64.0;
/// Inkl. Insert-Rack unter Mute/Solo (bis zu 4 sichtbare Bus-Effekte).
const STRIP_H: f32 = 430.0;
const METER_H: f32 = 160.0;
/// Maximal sichtbare Insert-Effekte je Kanalzug.
const MAX_INSERTS: usize = 4;
/// Sichtbarer dB-Bereich der Meter (−60..0 dBFS).
const METER_RANGE_DB: f32 = 60.0;
/// Nachleuchtdauer der Übersteuerungs-LED (Sekunden).
const CLIP_HOLD_SECS: f64 = 2.0;
/// Fallgeschwindigkeit der Peak-Hold-Marke (Meter-Anteil pro Sekunde).
const PEAK_FALL_PER_SEC: f32 = 0.35;

/// Geglätteter Anzeigezustand eines Meters (pro Spur bzw. Master).
#[derive(Default, Clone, Copy)]
struct MeterState {
    lvl: [f32; 2],
    peak_hold: [f32; 2],
    clip_until: f64,
}

impl MeterState {
    /// Peak (linear, 1.0 = 0 dBFS) → Meteranteil 0..1 auf der dB-Skala.
    fn norm(peak: f32) -> f32 {
        // !is_finite zuerst: ein NaN-Peak liefe durch `peak <= 0.0` (NaN-Vergleich
        // = false) und durch clamp (lässt NaN durch) bis in die Pegel-Rects.
        if !peak.is_finite() || peak <= 0.0 {
            return 0.0;
        }
        ((20.0 * peak.log10() + METER_RANGE_DB) / METER_RANGE_DB).clamp(0.0, 1.0)
    }

    fn feed(&mut self, peak: [f32; 2], time: f64, dt: f32) {
        if peak[0] >= 1.0 || peak[1] >= 1.0 {
            self.clip_until = time + CLIP_HOLD_SECS;
        }
        for ch in 0..2 {
            let target = Self::norm(peak[ch]);
            // schnelle Attack, langsamer Release
            let k = if target > self.lvl[ch] { 0.5 } else { 0.12 };
            self.lvl[ch] += (target - self.lvl[ch]) * k;
            self.peak_hold[ch] = (self.peak_hold[ch] - PEAK_FALL_PER_SEC * dt).max(0.0);
            if target > self.peak_hold[ch] {
                self.peak_hold[ch] = target;
            }
        }
    }
}

#[derive(Default)]
pub struct AudioMixerPanel {
    meters: HashMap<String, MeterState>,
    master_meter: MeterState,
    /// Eine laufende Fader-/Pan-Geste hat ihren Undo-Snapshot schon angelegt.
    gesture_pushed: bool,
    scroll: ScrollState,
}


/// Werte eines Kanalzugs für den Render-Durchlauf (entkoppelt vom Store,
/// damit Änderungen während der Schleife über die Store-API laufen können).
struct StripData {
    track_id: Option<String>, // None = Master
    label: String,
    gain_db: f64,
    pan: f64,
    muted: bool,
    solo: bool,
    level: [f32; 2],
    /// Bus-Effekte (fx_id, Art, aktiv) der Spur (Audio-Inserts).
    inserts: Vec<(String, EffectKind, bool)>,
    /// Spur hat aktive Lautstärke-/Pan-Automation.
    automated: bool,
}

enum StripEdit {
    Gain(Option<String>, f64),
    Pan(String, f64),
    ToggleMute(String),
    ToggleSolo(String),
}

impl Panel for AudioMixerPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);
        let area = rect;
        let time = ui.time;
        let dt = ui.frame_time;

        // ---- Kanalzug-Daten einsammeln (Audio-Spuren + Master) ----
        let mut strips: Vec<StripData> = Vec::new();
        for track in app
            .timeline
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
        {
            strips.push(StripData {
                track_id: Some(track.id.clone()),
                label: track_name(track, &app.timeline.tracks),
                gain_db: track.gain_db,
                pan: track.pan,
                muted: track.muted,
                solo: track.solo,
                level: app
                    .audio
                    .track_levels
                    .get(&track.id)
                    .copied()
                    .unwrap_or([0.0, 0.0]),
                inserts: track
                    .effects
                    .iter()
                    .filter(|e| e.kind.is_audio())
                    .map(|e| (e.id.clone(), e.kind, e.enabled))
                    .collect(),
                automated: track.has_automation(),
            });
        }
        strips.push(StripData {
            track_id: None,
            label: "Master".to_string(),
            gain_db: app.timeline.master_gain_db,
            pan: 0.0,
            muted: false,
            solo: false,
            level: app.audio.master_level,
            inserts: Vec::new(),
            automated: false,
        });

        // Meter-Zustände verwaister Spuren aufräumen.
        let live_ids: std::collections::HashSet<&str> = strips
            .iter()
            .filter_map(|s| s.track_id.as_deref())
            .collect();
        self.meters.retain(|id, _| live_ids.contains(id.as_str()));

        let mut edits: Vec<StripEdit> = Vec::new();
        let mut any_held = false;

        let total_w = strips.len() as f32 * (STRIP_W + 8.0) + 18.0;
        let content_h = STRIP_H + 24.0;
        let view = self.scroll.begin(ui, area, total_w, content_h);

        let mut x = view.origin_x + (view.viewport.w - total_w).max(0.0) / 2.0 + 4.0;
        let y = view.origin_y + 12.0;

        for (i, strip) in strips.iter().enumerate() {
            let is_master = strip.track_id.is_none();
            // Trenner vor dem Master
            if is_master {
                ui.fill(Rect::new(x, y, 1.0, STRIP_H), theme::LINE);
                x += 9.0;
            }
            let card = Rect::new(x, y, STRIP_W, STRIP_H);
            ui.fill_rounded(card, theme::RADIUS_SM, theme::SURFACE_2);
            ui.stroke_rounded(card, theme::RADIUS_SM, 1.0, theme::LINE);

            let mut cy = card.y + 8.0;
            ui.text_centered(
                &strip.label,
                Rect::new(card.x, cy, card.w, 16.0),
                if is_master { theme::TEXT_1 } else { theme::TEXT_2 },
                FontKind::Sans12Medium,
            );
            cy += 20.0;

            // Pan (horizontaler Mini-Slider, UI in ±100, Modell ±1)
            if is_master {
                ui.text_centered(
                    "Summe",
                    Rect::new(card.x, cy + 8.0, card.w, 16.0),
                    theme::TEXT_3,
                    FontKind::Sans12,
                );
                cy += 34.0;
            } else {
                let pan_rect = Rect::new(card.x + (card.w - 48.0) / 2.0, cy, 48.0, 16.0);
                let mut pan_ui = (strip.pan * 100.0).round();
                let pan_it = slider(
                    ui,
                    ("mixer.pan", i),
                    pan_rect,
                    &mut pan_ui,
                    -100.0,
                    100.0,
                    theme::ACCENT,
                );
                if pan_it.held {
                    any_held = true;
                }
                pan_ui = pan_ui.round();
                if pan_it.double_clicked {
                    pan_ui = 0.0;
                }
                let new_pan = (pan_ui / 100.0).clamp(-1.0, 1.0);
                if new_pan != strip.pan {
                    edits.push(StripEdit::Pan(strip.track_id.clone().unwrap(), new_pan));
                }
                cy += 18.0;
                let pan_label = if pan_ui == 0.0 {
                    "C".to_string()
                } else if pan_ui < 0.0 {
                    format!("L{}", -pan_ui as i64)
                } else {
                    format!("R{}", pan_ui as i64)
                };
                ui.text_centered(
                    &pan_label,
                    Rect::new(card.x, cy, card.w, 12.0),
                    theme::TEXT_3,
                    FontKind::Mono12,
                );
                cy += 16.0;
            }

            // Fader (vertikal) + Pegelmeter
            let fader_x = card.x + 10.0;
            let fader = Rect::new(fader_x, cy, 24.0, METER_H);
            ui.fill_rounded(
                Rect::new(fader.x + 10.0, fader.y, 4.0, fader.h),
                2.0,
                theme::SURFACE_4,
            );
            let mut gain = strip.gain_db;
            let t = ((gain - 6.0) / (-66.0)).clamp(0.0, 1.0) as f32; // +6 dB oben
            let thumb = Rect::new(fader.x + 2.0, fader.y + t * (fader.h - 10.0), 20.0, 10.0);
            let fid = ui.id(("mixer.fader", i));
            let fit = ui.interact(fid, fader);
            if fit.held {
                any_held = true;
                let ft = ((ui.input.mouse.y - fader.y) / (fader.h - 10.0)).clamp(0.0, 1.0);
                gain = (6.0 - ft as f64 * 66.0).clamp(-60.0, 6.0);
                gain = (gain * 2.0).round() / 2.0;
            }
            if fit.double_clicked {
                gain = 0.0;
            }
            if gain != strip.gain_db {
                edits.push(StripEdit::Gain(strip.track_id.clone(), gain));
            }
            if fit.hovered || fit.held {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            }
            let thumb_color = if is_master {
                if fit.hovered { theme::ACCENT_HOVER } else { theme::ACCENT }
            } else if fit.hovered {
                theme::TEXT_1
            } else {
                theme::TEXT_2
            };
            ui.fill_rounded(thumb, theme::RADIUS_XS, thumb_color);
            ui.stroke_rounded(thumb, theme::RADIUS_XS, 1.0, theme::LINE_STRONG);

            // Pegelmeter: echte Spitzenpegel der Engine, dBFS-Skala.
            let meter = if let Some(id) = strip.track_id.as_ref() {
                self.meters.entry(id.clone()).or_default()
            } else {
                &mut self.master_meter
            };
            meter.feed(strip.level, time, dt);
            let clip_lit = time < meter.clip_until;

            let meter_x = fader.x + 28.0;
            for (mi, lvl, hold) in [
                (0usize, meter.lvl[0], meter.peak_hold[0]),
                (1, meter.lvl[1], meter.peak_hold[1]),
            ] {
                let bar = Rect::new(meter_x + mi as f32 * 6.0, cy, 4.0, METER_H);
                // Gradient: unten success → warning (ab −18 dB) → danger (ab −6 dB)
                let mut seg = |from: f32, to: f32, color| {
                    ui.fill(
                        Rect::new(
                            bar.x,
                            bar.y + (1.0 - to) * bar.h,
                            bar.w,
                            (to - from) * bar.h,
                        ),
                        color,
                    );
                };
                seg(0.0, 0.7, theme::SUCCESS);
                seg(0.7, 0.9, theme::with_alpha(theme::WARNING, 230));
                seg(0.9, 1.0, theme::with_alpha(theme::DANGER, 230));
                // Abdeckung von oben (ungefüllter Anteil)
                ui.fill(
                    Rect::new(bar.x, bar.y, bar.w, (1.0 - lvl).clamp(0.0, 1.0) * bar.h),
                    theme::SURFACE_3,
                );
                // Peak-Hold-Marke
                if hold > 0.01 {
                    ui.fill(
                        Rect::new(bar.x, bar.y + (1.0 - hold) * (bar.h - 1.0), bar.w, 1.0),
                        theme::TEXT_2,
                    );
                }
                // Übersteuerungs-LED
                ui.fill(
                    Rect::new(bar.x, bar.y - 4.0, bar.w, 3.0),
                    if clip_lit {
                        theme::DANGER
                    } else {
                        theme::SURFACE_4
                    },
                );
            }
            cy += METER_H + 6.0;

            let gain_label = if gain <= -60.0 {
                "-∞ dB".to_string()
            } else {
                format!("{:.1} dB", gain)
            };
            ui.text_centered(
                &gain_label,
                Rect::new(card.x, cy, card.w, 14.0),
                theme::TEXT_2,
                FontKind::Mono12,
            );
            cy += 18.0;

            // Mute / Solo (nur für Spuren; Master hat keine Flags)
            if let Some(track_id) = strip.track_id.as_ref() {
                let m_rect = Rect::new(card.x + card.w / 2.0 - 22.0, cy, 20.0, 20.0);
                let s_rect = Rect::new(card.x + card.w / 2.0 + 2.0, cy, 20.0, 20.0);
                for (which, btn_rect, label, on, on_fg) in [
                    (0, m_rect, "M", strip.muted, theme::WARNING),
                    (1, s_rect, "S", strip.solo, theme::SUCCESS),
                ] {
                    let id = ui.id(("mixer.ms", i, which));
                    let it = ui.interact(id, btn_rect);
                    if on {
                        ui.fill_rounded(btn_rect, theme::RADIUS_SM, theme::with_alpha(on_fg, 51));
                        ui.stroke_rounded(
                            btn_rect,
                            theme::RADIUS_SM,
                            1.0,
                            theme::with_alpha(on_fg, 102),
                        );
                    } else {
                        let bc = if it.hovered { theme::LINE_STRONG } else { theme::LINE };
                        ui.stroke_rounded(btn_rect, theme::RADIUS_SM, 1.0, bc);
                    }
                    let fg = if on {
                        on_fg
                    } else if it.hovered {
                        theme::TEXT_1
                    } else {
                        theme::TEXT_3
                    };
                    ui.text_centered(label, btn_rect, fg, FontKind::Sans12Medium);
                    if it.hovered {
                        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                    }
                    if it.clicked {
                        edits.push(if which == 0 {
                            StripEdit::ToggleMute(track_id.clone())
                        } else {
                            StripEdit::ToggleSolo(track_id.clone())
                        });
                    }
                }

                // ---- Insert-Rack: Bus-Effekte der Spur (Drag&Drop aus dem
                // Effekte-Panel, Klick = Bypass, Rechtsklick = Menü) ----
                cy += 28.0;
                let hdr = if strip.automated { "Inserts · Auto" } else { "Inserts" };
                let hdr_col = if strip.automated { theme::ACCENT } else { theme::TEXT_3 };
                ui.text_centered(hdr, Rect::new(card.x, cy, card.w, 12.0), hdr_col, FontKind::Mono11);
                cy += 14.0;
                let rack_x = card.x + 6.0;
                let rack_w = card.w - 12.0;
                for (slot, (fx_id, kind, enabled)) in strip.inserts.iter().take(MAX_INSERTS).enumerate() {
                    let row = Rect::new(rack_x, cy, rack_w, 18.0);
                    let row_id = ui.id(("mixer.ins", i, slot));
                    let it = ui.interact(row_id, row);
                    ui.fill_rounded(
                        row,
                        theme::RADIUS_XS,
                        if it.hovered { theme::SURFACE_3 } else { theme::SURFACE_1 },
                    );
                    let (icol, tcol) = if *enabled {
                        (theme::ACCENT, theme::TEXT_2)
                    } else {
                        let dim = theme::with_alpha(theme::TEXT_3, 140);
                        (dim, dim)
                    };
                    ui.icon(kind.icon(), Rect::new(row.x + 2.0, row.y + 3.0, 12.0, 12.0), 12.0, icol);
                    let label = ui.font(FontKind::Mono11).ellipsize(kind.label(), rack_w - 18.0);
                    ui.text_left(
                        &label,
                        Rect::new(row.x + 16.0, row.y, rack_w - 18.0, 18.0),
                        tcol,
                        FontKind::Mono11,
                    );
                    if it.hovered {
                        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                    }
                    if it.clicked {
                        app.timeline.track_effects_toggle_enabled(track_id, fx_id);
                    }
                    if it.right_clicked {
                        let (tid, fid) = (track_id.clone(), fx_id.clone());
                        app.context_menu.show(
                            ui.input.mouse.x,
                            ui.input.mouse.y,
                            vec![
                                MenuEntry::Item(
                                    MenuItem::custom(
                                        if *enabled { "Bypass" } else { "Aktivieren" },
                                        CustomAction::TrackEffectsToggle { track_id: tid.clone(), fx_id: fid.clone() },
                                    )
                                    .with_icon("zap"),
                                ),
                                MenuEntry::Item(
                                    MenuItem::custom(
                                        "Nach oben",
                                        CustomAction::TrackEffectsMove { track_id: tid.clone(), fx_id: fid.clone(), delta: -1 },
                                    )
                                    .with_icon("chevron-up"),
                                ),
                                MenuEntry::Item(
                                    MenuItem::custom(
                                        "Nach unten",
                                        CustomAction::TrackEffectsMove { track_id: tid.clone(), fx_id: fid.clone(), delta: 1 },
                                    )
                                    .with_icon("chevron-down"),
                                ),
                                MenuEntry::Item(
                                    MenuItem::custom(
                                        "Zurücksetzen",
                                        CustomAction::TrackEffectsReset { track_id: tid.clone(), fx_id: fid.clone() },
                                    )
                                    .with_icon("rotate-ccw"),
                                ),
                                MenuEntry::Separator,
                                MenuEntry::Item(
                                    MenuItem::custom(
                                        "Entfernen",
                                        CustomAction::TrackEffectsRemove { track_id: tid, fx_id: fid },
                                    )
                                    .with_icon("trash-2"),
                                ),
                            ],
                        );
                    }
                    cy += 20.0;
                }
                if strip.inserts.len() > MAX_INSERTS {
                    ui.text_centered(
                        &format!("+{} weitere", strip.inserts.len() - MAX_INSERTS),
                        Rect::new(card.x, cy, card.w, 12.0),
                        theme::TEXT_3,
                        FontKind::Mono11,
                    );
                    cy += 14.0;
                }
                let add = Rect::new(rack_x, cy, rack_w, 18.0);
                let add_id = ui.id(("mixer.insadd", i));
                let ait = ui.interact(add_id, add);
                ui.stroke_rounded(
                    add,
                    theme::RADIUS_XS,
                    1.0,
                    if ait.hovered { theme::LINE_STRONG } else { theme::LINE },
                );
                ui.text_centered(
                    "+ Effekt",
                    add,
                    if ait.hovered { theme::TEXT_1 } else { theme::TEXT_3 },
                    FontKind::Mono11,
                );
                if ait.hovered {
                    ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                }
                if ait.clicked {
                    let tid = track_id.clone();
                    let items: Vec<MenuEntry> = EffectKind::ALL
                        .iter()
                        .filter(|k| k.is_audio())
                        .map(|k| {
                            MenuEntry::Item(
                                MenuItem::custom(
                                    k.label(),
                                    CustomAction::TrackEffectsAdd { track_id: tid.clone(), kind: *k },
                                )
                                .with_icon(k.icon()),
                            )
                        })
                        .collect();
                    app.context_menu.show(ui.input.mouse.x, ui.input.mouse.y, items);
                }

                // Drop-Ziel: Audio-Effekt aus dem Effekte-Panel auf den Kanalzug.
                if matches!(ui.drag_over(card), Some(DragPayload::Effect(k)) if k.is_audio()) {
                    ui.stroke_rounded(card, theme::RADIUS_SM, 2.0, theme::ACCENT);
                }
                if let Some(DragPayload::Effect(kind)) = ui.accept_drop(card) {
                    if kind.is_audio() {
                        app.timeline.track_effects_add(track_id, kind);
                    }
                }
            }

            x += STRIP_W + 8.0;
        }

        self.scroll.end(ui, area, total_w, content_h);

        // ---- Änderungen anwenden (Fader/Pan-Gesten mit einem Undo-Schritt) ----
        let gain_or_pan = edits.iter().any(|e| {
            matches!(e, StripEdit::Gain(..) | StripEdit::Pan(..))
        });
        if gain_or_pan && !self.gesture_pushed {
            app.timeline.begin_mix_edit();
            self.gesture_pushed = true;
        }
        if !any_held {
            self.gesture_pushed = false;
        }
        for edit in edits {
            match edit {
                StripEdit::Gain(None, v) => app.timeline.set_master_gain_db(v),
                StripEdit::Gain(Some(id), v) => app.timeline.set_track_gain_db(&id, v),
                StripEdit::Pan(id, v) => app.timeline.set_track_pan(&id, v),
                StripEdit::ToggleMute(id) => app.timeline.toggle_track_flag(&id, TrackFlag::Muted),
                StripEdit::ToggleSolo(id) => app.timeline.toggle_track_flag(&id, TrackFlag::Solo),
            }
        }
    }
}
