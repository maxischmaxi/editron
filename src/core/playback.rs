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
        let mut t = state.timeline.playhead_sec + dt * state.playback.program_rate;
        // Loop über die Sequenz-In/Out-Punkte
        if let (Some(i), Some(o)) = (state.timeline.in_point, state.timeline.out_point) {
            if o > i {
                if state.playback.program_rate > 0.0 && t >= o {
                    t = i + (t - o) % (o - i);
                } else if state.playback.program_rate < 0.0 && t < i {
                    t = o - (i - t) % (o - i);
                }
            }
        } else if t >= end && state.playback.program_rate > 0.0 {
            t = end;
            state.playback.program_playing = false;
        }
        if t <= 0.0 && state.playback.program_rate < 0.0 {
            t = 0.0;
            state.playback.program_playing = false;
        }
        state.timeline.playhead_sec = t.max(0.0);
    }
}
