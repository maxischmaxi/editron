//! Timecode-Formatierung: SMPTE-Timecode für Sequenzen (non-drop mit
//! Doppelpunkten, Drop-Frame mit Semikolon vor der Frame-Spalte) und
//! einfache Medien-/Dauer-Formate.

use crate::core::sequence::{FrameRate, SequenceSettings};

/// SMPTE-artiger Timecode "HH:MM:SS:FF" (non-drop) für Medien-fps —
/// Quellmonitor/Info-Panel, wo nur eine gemessene f64-Rate vorliegt.
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

/// Sequenz-Timecode an den Einstellungen ausgerichtet: exakte rationale
/// Frame-Zählung, Drop-Frame-Notation wenn aktiviert (und von der Rate
/// unterstützt).
pub fn format_sequence_timecode(seconds: f64, settings: &SequenceSettings) -> String {
    let t = if seconds.is_finite() && seconds > 0.0 {
        seconds
    } else {
        0.0
    };
    let frames = settings.rate.frame_floor(t).max(0) as u64;
    format_frames(frames, settings.rate, settings.drop_frame)
}

/// Frame-Nummer → SMPTE-Timecode. Drop-Frame (nur 29,97/59,94): pro Minute
/// werden 2 bzw. 4 Frame-NUMMERN übersprungen, außer in jeder 10. Minute —
/// die Anzeige bleibt so über lange Strecken deckungsgleich mit der Echtzeit.
pub fn format_frames(frames: u64, rate: FrameRate, drop_frame: bool) -> String {
    if drop_frame && rate.supports_drop_frame() {
        let nominal = rate.nominal(); // 30 bzw. 60
        let drop = nominal / 15; // 2 bzw. 4
        let per_min = nominal * 60 - drop;
        let per_10min = nominal * 600 - 9 * drop;
        let blocks = frames / per_10min;
        let mut rem = frames % per_10min;
        let mut minutes = blocks * 10;
        // Minute 0 jedes 10er-Blocks trägt alle Frame-Nummern; in den
        // Minuten 1–9 existieren die Nummern 0..drop nicht.
        if rem >= nominal * 60 {
            rem -= nominal * 60;
            minutes += 1 + rem / per_min;
            rem = rem % per_min + drop;
        }
        let h = minutes / 60;
        let m = minutes % 60;
        let s = rem / nominal;
        let f = rem % nominal;
        format!("{h:02}:{m:02}:{s:02};{f:02}")
    } else {
        let nominal = rate.nominal().max(1);
        let total_s = frames / nominal;
        let f = frames % nominal;
        let h = total_s / 3600;
        let m = (total_s % 3600) / 60;
        let s = total_s % 60;
        format!("{h:02}:{m:02}:{s:02}:{f:02}")
    }
}

/// SMPTE-Timecode → Frame-Nummer (Umkehrung von `format_frames`).
pub fn frames_of(h: u64, m: u64, s: u64, f: u64, rate: FrameRate, drop_frame: bool) -> u64 {
    let nominal = rate.nominal().max(1);
    // Overflow-sicher: feindliche/korrupte Timecodes (riesige Stundenwerte aus
    // fremden EDLs/OTIO-Dateien) dürfen nicht paniken, sondern müssen sättigen.
    let total_s = h
        .saturating_mul(3600)
        .saturating_add(m.saturating_mul(60))
        .saturating_add(s);
    let base = total_s.saturating_mul(nominal).saturating_add(f);
    if drop_frame && rate.supports_drop_frame() {
        let drop = nominal / 15;
        let total_min = h.saturating_mul(60).saturating_add(m);
        base.saturating_sub(drop.saturating_mul(total_min - total_min / 10))
    } else {
        base
    }
}

/// Timecode-Eingabe parsen: "HH:MM:SS:FF" (Semikolon vor den Frames
/// erlaubt, führende Gruppen dürfen fehlen — "SS:FF", "MM:SS:FF") oder
/// Sekunden mit Komma ("12,5"). Liefert Sekunden auf dem Frame-Raster.
pub fn parse_sequence_timecode(text: &str, settings: &SequenceSettings) -> Option<f64> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let rate = settings.rate;
    if !t.contains(':') && !t.contains(';') {
        let secs: f64 = t.replace(',', ".").parse().ok()?;
        if !secs.is_finite() || secs < 0.0 {
            return None;
        }
        return Some(rate.time_of_frame(rate.frame_round(secs).max(0) as f64));
    }
    let parts: Vec<&str> = t.split([':', ';']).collect();
    if parts.len() > 4 {
        return None;
    }
    let mut nums = [0u64; 4]; // h, m, s, f
    let off = 4 - parts.len();
    for (slot, p) in nums[off..].iter_mut().zip(parts.iter()) {
        *slot = p.trim().parse().ok()?;
    }
    let drop = settings.drop_frame && rate.supports_drop_frame();
    let frames = frames_of(nums[0], nums[1], nums[2], nums[3], rate, drop);
    Some(rate.time_of_frame(frames as f64))
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

#[cfg(test)]
mod tests {
    use super::*;

    const NTSC_30: FrameRate = FrameRate::new(30000, 1001);
    const NTSC_60: FrameRate = FrameRate::new(60000, 1001);

    #[test]
    fn drop_frame_skips_two_numbers_each_minute() {
        // 00:00:59;29 → +1 Frame → 00:01:00;02 (Nummern ;00/;01 entfallen).
        let f = frames_of(0, 0, 59, 29, NTSC_30, true);
        assert_eq!(format_frames(f, NTSC_30, true), "00:00:59;29");
        assert_eq!(format_frames(f + 1, NTSC_30, true), "00:01:00;02");
    }

    #[test]
    fn drop_frame_keeps_every_tenth_minute() {
        // Minute 10 ist KEINE Drop-Minute: 00:09:59;29 → +1 → 00:10:00;00.
        let f = frames_of(0, 9, 59, 28, NTSC_30, true);
        assert_eq!(format_frames(f, NTSC_30, true), "00:09:59;28");
        assert_eq!(format_frames(f + 2, NTSC_30, true), "00:10:00;00");
        assert_eq!(format_frames(f + 4, NTSC_30, true), "00:10:00;02");
    }

    #[test]
    fn drop_frame_matches_wall_clock_after_one_hour() {
        // 1 Stunde Echtzeit bei 30000/1001 = 107.892 Frames → exakt 01:00:00;00.
        let frames = NTSC_30.frame_floor(3600.0) as u64;
        assert_eq!(frames, 107_892);
        assert_eq!(format_frames(frames, NTSC_30, true), "01:00:00;00");
        // Non-Drop läuft dagegen 3,6 s nach.
        assert_eq!(format_frames(frames, NTSC_30, false), "00:59:56:12");
    }

    #[test]
    fn drop_frame_5994_drops_four_numbers() {
        let f = frames_of(0, 0, 59, 59, NTSC_60, true);
        assert_eq!(format_frames(f, NTSC_60, true), "00:00:59;59");
        assert_eq!(format_frames(f + 1, NTSC_60, true), "00:01:00;04");
        // 1 Stunde Echtzeit = 215.784 Frames → 01:00:00;00.
        let frames = NTSC_60.frame_floor(3600.0) as u64;
        assert_eq!(frames, 215_784);
        assert_eq!(format_frames(frames, NTSC_60, true), "01:00:00;00");
    }

    #[test]
    fn drop_frame_roundtrip_is_lossless() {
        // Jede Frame-Nummer → Timecode → Frame-Nummer (rund um kritische
        // Minuten- und 10-Minuten-Grenzen sowie tief in der Stunde).
        for base in [0u64, 1797, 1798, 1799, 1800, 17981, 17982, 107_891, 107_892, 1_078_920] {
            for f in base.saturating_sub(2)..=base + 2 {
                let tc = format_frames(f, NTSC_30, true);
                let parts: Vec<u64> = tc
                    .split([':', ';'])
                    .map(|p| p.parse().unwrap())
                    .collect();
                let back = frames_of(parts[0], parts[1], parts[2], parts[3], NTSC_30, true);
                assert_eq!(back, f, "Roundtrip für Frame {f} ({tc})");
            }
        }
    }

    #[test]
    fn frames_of_saturates_instead_of_panicking_on_huge_hours() {
        // Feindliche EDL-/OTIO-Zeile mit absurdem Stundenwert darf nicht paniken
        // (dev-Profil läuft mit overflow-checks=on).
        let huge = frames_of(99_999_999_999_999_999, 0, 0, 0, NTSC_30, false);
        assert_eq!(huge, u64::MAX, "Stundenwert sättigt statt zu überlaufen");
        // Auch mit Drop-Frame kein Panic.
        let _ = frames_of(u64::MAX, 59, 29, 29, NTSC_30, true);
    }

    #[test]
    fn drop_frame_ignored_for_non_ntsc_rates() {
        let pal = FrameRate::new(25, 1);
        assert_eq!(format_frames(50, pal, true), "00:00:02:00");
    }

    #[test]
    fn parse_timecode_handles_groups_and_seconds() {
        let s = crate::core::sequence::SequenceSettings {
            rate: FrameRate::new(25, 1),
            ..Default::default()
        };
        // Volle Notation, fehlende führende Gruppen, Sekunden mit Komma.
        assert_eq!(parse_sequence_timecode("00:00:02:00", &s), Some(2.0));
        assert_eq!(parse_sequence_timecode("02:00", &s), Some(2.0));
        assert_eq!(parse_sequence_timecode("00:00:01:12", &s).unwrap(), 1.48);
        // Sekunden werden aufs Frame-Raster gerundet (25 fps: Frame 37 = 1,48 s).
        assert_eq!(parse_sequence_timecode("1,48", &s), Some(1.48));
        assert_eq!(parse_sequence_timecode("2,0", &s), Some(2.0));
        assert!(parse_sequence_timecode("quatsch", &s).is_none());
        // Roundtrip mit Drop-Frame.
        let df = crate::core::sequence::SequenceSettings {
            rate: NTSC_30,
            drop_frame: true,
            ..Default::default()
        };
        let tc = format_sequence_timecode(3600.0, &df);
        let back = parse_sequence_timecode(&tc, &df).unwrap();
        assert_eq!(format_sequence_timecode(back, &df), tc);
    }

    #[test]
    fn sequence_timecode_uses_settings() {
        let mut s = crate::core::sequence::SequenceSettings {
            rate: NTSC_30,
            drop_frame: true,
            ..Default::default()
        };
        assert_eq!(format_sequence_timecode(3600.0, &s), "01:00:00;00");
        s.drop_frame = false;
        assert_eq!(format_sequence_timecode(3600.0, &s), "00:59:56:12");
        s.rate = FrameRate::new(25, 1);
        assert_eq!(format_sequence_timecode(1.0, &s), "00:00:01:00");
        assert_eq!(format_sequence_timecode(-3.0, &s), "00:00:00:00");
    }
}
