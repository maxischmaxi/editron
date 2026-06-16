//! Wiedergabe-Routing + Transport-Logik (PlaybackController-Pendant).
//! Commands wirken auf den aktiven Monitor: Fokus auf dem Quellmonitor
//! steuert die Quelle, alles andere das Programm (Timeline).

use crate::core::timeline::sequence_end;
use crate::state::AppState;

#[derive(Clone, Copy, Debug)]
pub enum PlaybackCmd {
    Toggle,
    Pause,
    Shuttle(f64), // +1 / -1
    StepFrames(f64),
    GoToStart,
    GoToEnd,
    MarkIn,
    MarkOut,
    ClearMarks,
    ToggleLoop,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Source,
    Program,
}

fn active_target(state: &AppState) -> Target {
    if state.app.focused_panel == "source" && state.playback.source_asset_id.is_some() {
        Target::Source
    } else {
        Target::Program
    }
}

fn source_duration(state: &AppState) -> f64 {
    state
        .playback
        .source_asset_id
        .as_ref()
        .and_then(|id| state.media.asset(id))
        .map(|a| a.info.duration_sec)
        .unwrap_or(0.0)
}

/// Framerate des Quellmonitor-Mediums (Frame-Stepping läuft dort gegen die
/// native Rate des Materials, nicht gegen die Sequenzrate).
fn source_fps(state: &AppState) -> f64 {
    state
        .playback
        .source_asset_id
        .as_ref()
        .and_then(|id| state.media.asset(id))
        .and_then(|a| a.info.video.first())
        .map(|v| v.fps)
        .filter(|f| *f > 0.0)
        .unwrap_or(25.0)
}

/// JKL-Shuttle: startet bei ±1, verdoppelt bis ±8, Richtungswechsel resettet.
fn shuttle_rate(current: f64, playing: bool, dir: f64) -> f64 {
    if !playing || current.signum() != dir.signum() || current == 0.0 {
        dir
    } else {
        (current * 2.0).clamp(-8.0, 8.0)
    }
}

pub fn dispatch(state: &mut AppState, cmd: PlaybackCmd) {
    match active_target(state) {
        Target::Source => source_dispatch(state, cmd),
        Target::Program => program_dispatch(state, cmd),
    }
}

fn source_dispatch(state: &mut AppState, cmd: PlaybackCmd) {
    let dur = source_duration(state);
    let fps = source_fps(state);
    let s = &mut state.playback.source;
    match cmd {
        PlaybackCmd::Toggle => {
            if !s.playing && s.position >= dur && dur > 0.0 {
                s.position = 0.0; // am Ende: von vorn
            }
            s.playing = !s.playing;
            if s.playing && (s.rate == 0.0 || s.rate.abs() > 1.0) {
                s.rate = 1.0;
            }
        }
        PlaybackCmd::Pause => s.playing = false,
        PlaybackCmd::Shuttle(dir) => {
            s.rate = shuttle_rate(s.rate, s.playing, dir);
            s.playing = true;
        }
        PlaybackCmd::StepFrames(frames) => {
            s.playing = false;
            s.position = (s.position + frames / fps).clamp(0.0, dur);
        }
        PlaybackCmd::GoToStart => s.position = 0.0,
        PlaybackCmd::GoToEnd => s.position = dur,
        PlaybackCmd::MarkIn => {
            s.in_mark = Some(s.position);
            if let Some(out) = s.out_mark {
                if out <= s.position {
                    s.out_mark = None;
                }
            }
        }
        PlaybackCmd::MarkOut => {
            s.out_mark = Some(s.position);
            if let Some(inp) = s.in_mark {
                if inp >= s.position {
                    s.in_mark = None;
                }
            }
        }
        PlaybackCmd::ClearMarks => {
            s.in_mark = None;
            s.out_mark = None;
        }
        PlaybackCmd::ToggleLoop => s.looping = !s.looping,
    }
}

fn program_dispatch(state: &mut AppState, cmd: PlaybackCmd) {
    match cmd {
        PlaybackCmd::Toggle => {
            let end = sequence_end(&state.timeline.clips);
            if !state.playback.program_playing
                && end > 0.0
                && state.timeline.playhead_sec >= end
            {
                state.timeline.playhead_sec = 0.0;
            }
            state.playback.program_playing = !state.playback.program_playing;
            if state.playback.program_playing
                && (state.playback.program_rate == 0.0 || state.playback.program_rate.abs() > 1.0)
            {
                state.playback.program_rate = 1.0;
            }
        }
        PlaybackCmd::Pause => state.playback.program_playing = false,
        PlaybackCmd::Shuttle(dir) => {
            state.playback.program_rate = shuttle_rate(
                state.playback.program_rate,
                state.playback.program_playing,
                dir,
            );
            state.playback.program_playing = true;
        }
        PlaybackCmd::StepFrames(frames) => {
            state.playback.program_playing = false;
            state.timeline.step_playhead_frames(frames);
        }
        PlaybackCmd::GoToStart => state.timeline.go_to_start(),
        PlaybackCmd::GoToEnd => state.timeline.go_to_end(),
        PlaybackCmd::MarkIn => {
            let t = state.timeline.playhead_sec;
            state.timeline.set_in_point(Some(t));
        }
        PlaybackCmd::MarkOut => {
            let t = state.timeline.playhead_sec;
            state.timeline.set_out_point(Some(t));
        }
        PlaybackCmd::ClearMarks => state.timeline.clear_in_out(),
        PlaybackCmd::ToggleLoop => {
            state.playback.program_looping = !state.playback.program_looping;
        }
    }
}

/// Pro Frame: laufende Wiedergabe fortschreiben (Master-Clock-Tick).
pub fn tick(state: &mut AppState, dt: f64) {
    // Quellmonitor
    let dur = source_duration(state);
    {
        let s = &mut state.playback.source;
        if s.playing && dur > 0.0 {
            s.position += dt * s.rate;
            let (lo, hi) = match (s.in_mark, s.out_mark) {
                (Some(i), Some(o)) if s.looping => (i, o),
                _ => (0.0, dur),
            };
            if s.looping {
                if s.position >= hi {
                    s.position = lo + (s.position - hi) % (hi - lo).max(0.01);
                } else if s.position < lo {
                    s.position = hi - (lo - s.position) % (hi - lo).max(0.01);
                }
            } else if s.position >= dur {
                s.position = dur;
                s.playing = false;
            } else if s.position <= 0.0 {
                s.position = 0.0;
                s.playing = false;
            }
        }
    }

    // Programm (Timeline)
    if state.playback.program_playing {
        let end = sequence_end(&state.timeline.clips);
        let (t, playing) = advance_program(
            state.timeline.playhead_sec,
            dt,
            state.playback.program_rate,
            state.playback.program_looping,
            state.timeline.in_point,
            state.timeline.out_point,
            end,
        );
        state.timeline.playhead_sec = t;
        state.playback.program_playing = playing;
    }
}

/// Reine Programm-Transport-Mathematik für einen Tick: schreibt die Position
/// fort und behandelt entweder den Loop (zwischen In/Out, sonst über die ganze
/// Sequenz `0..end`) oder das Anhalten an den Sequenzgrenzen. Gibt die neue
/// Position (≥ 0) und zurück, ob die Wiedergabe weiterläuft.
fn advance_program(
    playhead: f64,
    dt: f64,
    rate: f64,
    looping: bool,
    in_point: Option<f64>,
    out_point: Option<f64>,
    end: f64,
) -> (f64, bool) {
    let mut t = playhead + dt * rate;
    let mut playing = true;
    // Loop-Grenzen: gesetzte In/Out-Punkte, sonst die ganze Sequenz.
    let (lo, hi) = match (in_point, out_point) {
        (Some(i), Some(o)) if o > i => (i, o),
        _ => (0.0, end),
    };
    if looping && hi > lo {
        // Loop aktiv: an den Grenzen umschlagen statt anhalten. Der Modulo
        // fängt auch große Sprünge (hohe Rate, Playhead außerhalb) korrekt ab.
        if rate > 0.0 && t >= hi {
            t = lo + (t - hi) % (hi - lo);
        } else if rate < 0.0 && t < lo {
            t = hi - (lo - t) % (hi - lo);
        }
    } else {
        // Kein Loop: am Sequenzende bzw. -anfang anhalten.
        if t >= end && rate > 0.0 {
            t = end;
            playing = false;
        }
        if t <= 0.0 && rate < 0.0 {
            t = 0.0;
            playing = false;
        }
    }
    (t.max(0.0), playing)
}

#[cfg(test)]
mod tests {
    use super::advance_program;

    const DT: f64 = 0.1;

    #[test]
    fn no_loop_stops_at_sequence_end() {
        // Ohne Loop läuft das Programm bis zum Sequenzende und hält an —
        // auch wenn In/Out gesetzt sind (In/Out stoppen die Wiedergabe nicht).
        let (t, playing) = advance_program(9.97, DT, 1.0, false, Some(2.0), Some(5.0), 10.0);
        assert_eq!(t, 10.0);
        assert!(!playing);
    }

    #[test]
    fn no_loop_does_not_wrap_past_out_point() {
        // Ohne Loop ignoriert der Out-Punkt das Umschlagen: der Playhead läuft
        // einfach über den Out-Punkt hinaus weiter.
        let (t, playing) = advance_program(4.95, DT, 1.0, false, Some(2.0), Some(5.0), 10.0);
        assert!((t - 5.05).abs() < 1e-9, "t = {t}");
        assert!(playing);
    }

    #[test]
    fn loop_wraps_between_in_and_out() {
        // Loop an + In/Out: am Out-Punkt zurück zum In-Punkt umschlagen.
        let (t, playing) = advance_program(4.95, DT, 1.0, true, Some(2.0), Some(5.0), 10.0);
        assert!((t - 2.05).abs() < 1e-9, "t = {t}");
        assert!(playing);
    }

    #[test]
    fn loop_wraps_over_whole_sequence_without_marks() {
        // Loop an, keine In/Out: über die ganze Sequenz umschlagen (end -> 0).
        let (t, playing) = advance_program(9.95, DT, 1.0, true, None, None, 10.0);
        assert!((t - 0.05).abs() < 1e-9, "t = {t}");
        assert!(playing);
    }

    #[test]
    fn loop_backward_wraps_to_out_point() {
        // Rückwärts-Loop: am In-Punkt zurück zum Out-Punkt umschlagen.
        let (t, playing) = advance_program(2.05, DT, -1.0, true, Some(2.0), Some(5.0), 10.0);
        assert!((t - 4.95).abs() < 1e-9, "t = {t}");
        assert!(playing);
    }

    #[test]
    fn loop_backward_over_whole_sequence_wraps_to_end() {
        // Rückwärts-Loop ohne Marken: am Anfang zurück zum Sequenzende.
        let (t, playing) = advance_program(0.05, DT, -1.0, true, None, None, 10.0);
        assert!((t - 9.95).abs() < 1e-9, "t = {t}");
        assert!(playing);
    }

    #[test]
    fn loop_handles_jump_larger_than_region() {
        // Sprung > Loop-Länge (hohe Rate / Playhead weit außerhalb): der Modulo
        // bringt die Position korrekt in den Loop-Bereich, ohne zu überlaufen.
        let region = 5.0 - 2.0;
        let (t, playing) = advance_program(8.0, DT, 100.0, true, Some(2.0), Some(5.0), 10.0);
        assert!(t >= 2.0 && t < 5.0, "t = {t} außerhalb [2,5)");
        // 8 + 0.1*100 = 18; (18 - 5) % 3 = 1 -> lo + 1 = 3
        assert!((t - (2.0 + (18.0 - 5.0) % region)).abs() < 1e-9, "t = {t}");
        assert!(playing);
    }

    #[test]
    fn loop_on_empty_sequence_does_not_panic_and_stops() {
        // Leere Sequenz (end == 0): kein Loop-Bereich (hi == lo), kein
        // Div-by-Zero; die Wiedergabe hält an.
        let (t, playing) = advance_program(0.0, DT, 1.0, true, None, None, 0.0);
        assert_eq!(t, 0.0);
        assert!(!playing);
    }

    #[test]
    fn loop_ignores_inverted_marks() {
        // Ungültige Marken (Out <= In): fallen auf die ganze Sequenz zurück.
        let (t, playing) = advance_program(9.95, DT, 1.0, true, Some(5.0), Some(2.0), 10.0);
        assert!((t - 0.05).abs() < 1e-9, "t = {t}");
        assert!(playing);
    }
}
