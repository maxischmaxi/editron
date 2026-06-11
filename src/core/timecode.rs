//! Timecode-Formatierung (SMPTE-artig, non-drop).

/// SMPTE-artiger Timecode "HH:MM:SS:FF" (non-drop).
pub fn format_timecode(seconds: f64, fps: f64) -> String {
    let safe_fps = if fps.is_finite() && fps > 0.0 { fps } else { 25.0 };
    let total = if seconds.is_finite() && seconds > 0.0 {
        seconds
    } else {
        0.0
    };
    let whole = total.floor();
    let max_frame = (safe_fps.round().max(1.0) - 1.0) as i64;
    let frames = (((total - whole) * safe_fps + 1e-6).floor() as i64).min(max_frame);
    let whole = whole as i64;
    let h = whole / 3600;
    let m = (whole % 3600) / 60;
    let s = whole % 60;
    format!("{h:02}:{m:02}:{s:02}:{frames:02}")
}

/// Kompakte Dauer: "M:SS", ab einer Stunde "H:MM:SS".
pub fn format_duration(seconds: f64) -> String {
    let total = if seconds.is_finite() && seconds > 0.0 {
        seconds.round() as i64
    } else {
        0
    };
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
