//! „Geschwindigkeit/Dauer“-Dialog (modal, Premiere Mod+R): Geschwindigkeit
//! in Prozent und Dauer als Sequenz-Timecode, gekoppelt über die belegte
//! Medienspanne (duration = span / speed). Dazu Rückwärts, Standbild und
//! die Ripple-Option („nachfolgende Clips verschieben“). Wirkt auf alle
//! ausgewählten Clips (verknüpfte A/V-Paare gemeinsam).

use crate::core::timecode::{format_sequence_timecode, parse_sequence_timecode};
use crate::core::timeline::{TimelineClip, TrackKind, MAX_CLIP_SPEED, MIN_CLIP_SPEED};
use crate::overlays::sequence_dialog::{checkbox, labeled_row, primary_button, section};
use crate::state::AppState;
use crate::stores::DialogId;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::drop_shadow;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::widgets::{IconButton, TextButton, TextButtonStyle};
use crate::ui::{FontKind, Ui};
use raylib::consts::KeyboardKey;

#[derive(Default)]
pub struct SpeedDialog {
    speed_input: TextInputState,
    duration_input: TextInputState,
    reverse: bool,
    freeze: bool,
    ripple: bool,
    /// Medienspanne des Referenz-Clips beim Öffnen (Kopplung Speed↔Dauer).
    ref_span: f64,
    was_open: bool,
}

/// Referenz-Clip der Auswahl: erster Nicht-Generator, Video bevorzugt.
fn reference_clip<'a>(state: &'a AppState) -> Option<&'a TimelineClip> {
    let selected: Vec<&TimelineClip> = state
        .timeline
        .clips
        .iter()
        .filter(|c| state.timeline.selected_clip_ids.contains(&c.id) && !c.is_generator())
        .collect();
    selected
        .iter()
        .find(|c| c.kind == TrackKind::Video)
        .copied()
        .or_else(|| selected.first().copied())
}

impl SpeedDialog {
    pub fn render(&mut self, ui: &mut Ui, state: &mut AppState) {
        if state.app.open_dialog != Some(DialogId::ClipSpeed) {
            self.was_open = false;
            return;
        }
        let Some(clip) = reference_clip(state).cloned() else {
            state.app.open_dialog = None;
            self.was_open = false;
            return;
        };
        if !self.was_open {
            self.was_open = true;
            self.reverse = clip.reverse;
            self.freeze = clip.freeze;
            self.ripple = true;
            // Spanne eines Standbilds wäre 0 — dann koppelt die Dauer nicht.
            self.ref_span = if clip.freeze {
                clip.duration * clip.eff_speed()
            } else {
                clip.media_span()
            };
            self.set_speed_text(clip.eff_speed());
            self.duration_input
                .set_text(format_sequence_timecode(clip.duration, &state.timeline.settings));
        }

        // ESC schließt — außer ein Textfeld verarbeitet die Taste selbst.
        let esc = ui.input.keys.iter().any(|k| k.key == KeyboardKey::KEY_ESCAPE);
        if esc && ui.persist.keyboard_focus == 0 {
            state.app.open_dialog = None;
            return;
        }

        ui.fill(ui.screen, theme::with_alpha(theme::BLACK, 130));
        let w = 460f32.min(ui.screen.w - 32.0);
        let h = 360f32.min(ui.screen.h - 32.0);
        let rect = ui.screen.center_box(w, h);
        drop_shadow(ui, rect, theme::RADIUS_LG);
        ui.fill_rounded(rect, theme::RADIUS_LG, theme::SURFACE_1);
        ui.stroke_rounded(rect, theme::RADIUS_LG, 1.0, theme::LINE_STRONG);

        // ---- Kopfzeile ----
        let mut area = rect;
        let head = area.cut_top(48.0);
        ui.hline(head.x, head.bottom() - 1.0, head.w, theme::LINE);
        let mut hi = head.inset_xy(16.0, 0.0);
        let icon_cell = hi.cut_left(18.0);
        ui.icon("gauge", icon_cell, 18.0, theme::TEXT_2);
        hi.cut_left(8.0);
        ui.text_left("Geschwindigkeit/Dauer", hi, theme::TEXT_1, FontKind::Sans16Semibold);
        let close = Rect::new(head.right() - 16.0 - 28.0, head.y + 10.0, 28.0, 28.0);
        if IconButton::new("x")
            .tooltip("Schließen (Esc)")
            .show(ui, "speed.close", close)
            .clicked
        {
            state.app.open_dialog = None;
            return;
        }

        let footer = area.cut_bottom(52.0);
        let mut body = area.inset_xy(16.0, 12.0);

        // ================= Tempo =================
        section(ui, &mut body, "Tempo");
        let mut r = labeled_row(ui, &mut body, "Geschwindigkeit");
        let field = r.cut_left(88.0);
        r.cut_left(8.0);
        ui.text_left("%", r, theme::TEXT_3, FontKind::Sans12);
        let speed_res = self.speed_input.show(ui, "speed.pct", field, "z. B. 50");
        let mut r = labeled_row(ui, &mut body, "Dauer");
        let field = r.cut_left(120.0);
        r.cut_left(8.0);
        ui.text_left("Timecode", r, theme::TEXT_3, FontKind::Sans12);
        let dur_res = self
            .duration_input
            .show(ui, "speed.dur", field, "HH:MM:SS:FF");

        // Kopplung über die Medienspanne: Geschwindigkeit ⇒ Dauer und
        // umgekehrt (nicht bei Standbild — dort ist die Dauer frei).
        if !self.freeze && self.ref_span > 0.0 {
            if speed_res.changed {
                if let Some(s) = self.parse_speed() {
                    self.duration_input.set_text(format_sequence_timecode(
                        self.ref_span / s,
                        &state.timeline.settings,
                    ));
                }
            } else if dur_res.changed {
                if let Some(d) =
                    parse_sequence_timecode(&self.duration_input.text, &state.timeline.settings)
                {
                    if d > 0.0 {
                        let s = (self.ref_span / d).clamp(MIN_CLIP_SPEED, MAX_CLIP_SPEED);
                        self.set_speed_text(s);
                    }
                }
            }
        }

        body.cut_top(2.0);
        section(ui, &mut body, "Optionen");
        let row = body.cut_top(24.0);
        if checkbox(ui, "speed.reverse", row, "Rückwärts abspielen", self.reverse && !self.freeze, !self.freeze)
            .clicked
        {
            self.reverse = !self.reverse;
        }
        let row = body.cut_top(24.0);
        if checkbox(
            ui,
            "speed.freeze",
            row,
            "Standbild (Frame am In-Punkt einfrieren)",
            self.freeze,
            true,
        )
        .clicked
        {
            self.freeze = !self.freeze;
        }
        let row = body.cut_top(24.0);
        if checkbox(
            ui,
            "speed.ripple",
            row,
            "Ripple — nachfolgende Clips verschieben",
            self.ripple,
            true,
        )
        .clicked
        {
            self.ripple = !self.ripple;
        }
        body.cut_top(8.0);
        if self.reverse && !self.freeze {
            let note = body.cut_top(16.0);
            ui.text_left(
                "Hinweis: Audio ist bei Rückwärts-Wiedergabe vorerst stumm.",
                note,
                theme::WARNING,
                FontKind::Sans12,
            );
        }

        // ---- Fußzeile ----
        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        let f = footer.inset_xy(16.0, 0.0);
        let result = self.build(state);
        let apply_rect = Rect::new(f.right() - 130.0, f.y + 12.0, 130.0, 28.0);
        let submit = speed_res.submitted || dur_res.submitted;
        if (primary_button(ui, "speed.apply", apply_rect, "Übernehmen", result.is_ok()).clicked
            || submit)
            && result.is_ok()
        {
            if let Ok(speed) = result {
                let ids = state.timeline.selected_clip_ids.clone();
                state
                    .timeline
                    .set_clip_speed(&ids, speed, self.reverse, self.freeze, self.ripple);
                let label = if self.freeze {
                    "Standbild".to_string()
                } else {
                    format!(
                        "{}{} %",
                        if self.reverse { "−" } else { "" },
                        (speed * 100.0).round() as i64
                    )
                };
                let msg = format!("Geschwindigkeit angewendet: {label}");
                state.app.set_status_message(Some(msg), ui.time);
                state.app.open_dialog = None;
                return;
            }
        }
        let cancel = TextButton::new("Abbrechen").style(TextButtonStyle::Outline);
        let cw = cancel.measure(ui);
        if cancel
            .show(ui, "speed.cancel", Rect::new(apply_rect.x - 8.0 - cw, f.y + 12.0, cw, 28.0))
            .clicked
        {
            state.app.open_dialog = None;
            return;
        }
        if let Err(msg) = &result {
            let err_rect =
                Rect::new(f.x, f.y + 12.0, apply_rect.x - 8.0 - cw - 16.0 - f.x, 28.0);
            let msg = ui.font(FontKind::Sans12).ellipsize(msg, err_rect.w);
            ui.text_left(&msg, err_rect, theme::DANGER, FontKind::Sans12);
        }
    }

    fn set_speed_text(&mut self, speed: f64) {
        let pct = speed * 100.0;
        let text = if (pct - pct.round()).abs() < 0.005 {
            format!("{}", pct.round() as i64)
        } else {
            format!("{pct:.2}")
                .trim_end_matches('0')
                .trim_end_matches('.')
                .replace('.', ",")
        };
        self.speed_input.set_text(text);
    }

    fn parse_speed(&self) -> Option<f64> {
        let raw = self.speed_input.text.trim().trim_end_matches('%').trim();
        let pct: f64 = raw.replace(',', ".").parse().ok()?;
        let speed = pct / 100.0;
        (speed.is_finite() && speed > 0.0).then_some(speed)
    }

    fn build(&self, state: &AppState) -> Result<f64, String> {
        let speed = self
            .parse_speed()
            .ok_or_else(|| "Ungültige Geschwindigkeit (z. B. 50)".to_string())?;
        if !(MIN_CLIP_SPEED..=MAX_CLIP_SPEED).contains(&speed) {
            return Err(format!(
                "Geschwindigkeit muss zwischen {} % und {} % liegen",
                (MIN_CLIP_SPEED * 100.0) as i64,
                (MAX_CLIP_SPEED * 100.0) as i64
            ));
        }
        if parse_sequence_timecode(&self.duration_input.text, &state.timeline.settings)
            .filter(|d| *d > 0.0)
            .is_none()
        {
            return Err("Ungültige Dauer (Timecode oder Sekunden)".to_string());
        }
        Ok(speed)
    }
}
