//! Sequenz-Einstellungen: Framerate als exakte Rationalzahl (NTSC-Raten
//! 24000/1001 etc. ohne Rundungsfehler), Auflösung mit Presets von HD bis
//! DCI 4K und Drop-Frame-Timecode-Flag. Pixel-Seitenverhältnis ist (vorerst)
//! immer quadratisch.

use crate::core::types::{MediaAsset, MediaKind};
use serde::{Deserialize, Serialize};

/// Framerate als exakter Bruch `num/den` — NTSC-Raten (23,976 / 29,97 /
/// 59,94) sind nur so drift-frei darstellbar: 30000/1001 statt 29.97.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameRate {
    pub num: u32,
    pub den: u32,
}

impl FrameRate {
    pub const fn new(num: u32, den: u32) -> FrameRate {
        FrameRate { num, den }
    }

    /// Gängige Sequenzraten (Reihenfolge = Dialog/Export-Auswahl).
    pub const PRESETS: [FrameRate; 8] = [
        FrameRate::new(24000, 1001), // 23,976
        FrameRate::new(24, 1),
        FrameRate::new(25, 1),
        FrameRate::new(30000, 1001), // 29,97
        FrameRate::new(30, 1),
        FrameRate::new(50, 1),
        FrameRate::new(60000, 1001), // 59,94
        FrameRate::new(60, 1),
    ];

    pub const PAL_25: FrameRate = FrameRate::new(25, 1);

    pub fn fps(&self) -> f64 {
        self.num as f64 / self.den.max(1) as f64
    }

    /// Frame-Index zur Zeit `t` (nächstgelegener Frame).
    pub fn frame_round(&self, t: f64) -> i64 {
        (t * self.num as f64 / self.den.max(1) as f64).round() as i64
    }

    /// Frame, in dem die Zeit `t` liegt (für Timecode-Anzeige); die kleine
    /// Epsilon fängt Zeiten exakt auf der Frame-Grenze nach f64-Rundung ab.
    pub fn frame_floor(&self, t: f64) -> i64 {
        (t * self.num as f64 / self.den.max(1) as f64 + 1e-6).floor() as i64
    }

    /// Startzeit eines Frames in Sekunden.
    pub fn time_of_frame(&self, frame: f64) -> f64 {
        frame * self.den as f64 / self.num.max(1) as f64
    }

    /// Drop-Frame-Timecode ist nur für 29,97 und 59,94 definiert.
    pub fn supports_drop_frame(&self) -> bool {
        self.den == 1001 && self.num % 30000 == 0
    }

    /// Nominale ganzzahlige Rate (30 für 29,97, 60 für 59,94) — Basis der
    /// Timecode-Frame-Spalte.
    pub fn nominal(&self) -> u64 {
        (self.num as u64).div_ceil(self.den.max(1) as u64)
    }

    /// Anzeige-Label mit deutschem Dezimalkomma: "25", "29,97".
    pub fn label(&self) -> String {
        let fps = self.fps();
        if (fps - fps.round()).abs() < 1e-9 {
            format!("{}", fps.round() as u64)
        } else {
            format!("{fps:.3}")
                .trim_end_matches('0')
                .trim_end_matches('.')
                .replace('.', ",")
        }
    }

    /// Beste rationale Darstellung einer (Medien-)fps: NTSC-Raten werden
    /// als exakte 1001er-Brüche erkannt, Ganzzahlen exakt, der Rest über
    /// Tausendstel angenähert.
    pub fn from_fps(fps: f64) -> Option<FrameRate> {
        if !fps.is_finite() || fps <= 0.0 || fps > 240.0 {
            return None;
        }
        for ntsc in [24000u32, 30000, 48000, 60000, 120000] {
            if (fps - ntsc as f64 / 1001.0).abs() < 0.005 {
                return Some(FrameRate::new(ntsc, 1001));
            }
        }
        if (fps - fps.round()).abs() < 1e-6 {
            return Some(FrameRate::new(fps.round() as u32, 1));
        }
        let num = (fps * 1000.0).round() as u32;
        let g = gcd(num, 1000);
        Some(FrameRate::new(num / g, 1000 / g))
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 {
        a.max(1)
    } else {
        gcd(b, a % b)
    }
}

/// Auflösungs-Presets (Label, Breite, Höhe) — HD bis DCI 4K.
pub const RESOLUTION_PRESETS: &[(&str, u32, u32)] = &[
    ("HD — 1280×720", 1280, 720),
    ("Full HD — 1920×1080", 1920, 1080),
    ("2K DCI — 2048×1080", 2048, 1080),
    ("QHD — 2560×1440", 2560, 1440),
    ("UHD 4K — 3840×2160", 3840, 2160),
    ("DCI 4K — 4096×2160", 4096, 2160),
];

pub const MIN_DIMENSION: u32 = 16;
pub const MAX_DIMENSION: u32 = 8192;

/// Einstellungen einer Sequenz: Zielraster + Zeitbasis. Pixel-
/// Seitenverhältnis ist immer quadratisch (1,0).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SequenceSettings {
    pub rate: FrameRate,
    pub width: u32,
    pub height: u32,
    /// Drop-Frame-Timecode (Semikolon-Notation) — nur bei 29,97/59,94 wirksam.
    #[serde(default)]
    pub drop_frame: bool,
}

impl Default for SequenceSettings {
    fn default() -> Self {
        SequenceSettings {
            rate: FrameRate::PAL_25,
            width: 1920,
            height: 1080,
            drop_frame: false,
        }
    }
}

impl SequenceSettings {
    /// Defensive Validierung beim Laden von Projektdateien.
    pub fn sanitized(mut self) -> SequenceSettings {
        if self.rate.num == 0 || self.rate.den == 0 || self.rate.fps() > 240.0 {
            self.rate = FrameRate::PAL_25;
        }
        self.width = self.width.clamp(MIN_DIMENSION, MAX_DIMENSION);
        self.height = self.height.clamp(MIN_DIMENSION, MAX_DIMENSION);
        self.drop_frame = self.drop_frame && self.rate.supports_drop_frame();
        self
    }

    /// Kurzbeschreibung für Statuszeile/Dialoge: "1920×1080 · 29,97 fps DF".
    pub fn describe(&self) -> String {
        format!(
            "{}×{} · {} fps{}",
            self.width,
            self.height,
            self.rate.label(),
            if self.drop_frame && self.rate.supports_drop_frame() {
                " DF"
            } else {
                ""
            }
        )
    }
}

/// Vorschlag passender Sequenz-Einstellungen aus eingefügtem Material
/// (Premiere: „Sequenz an Clip anpassen?“). Liefert die Einstellungen und
/// den Namen des maßgeblichen Assets; None ohne brauchbaren Videostream.
pub fn suggest_from_assets(
    current: &SequenceSettings,
    assets: &[MediaAsset],
    asset_ids: &[String],
) -> Option<(SequenceSettings, String)> {
    for id in asset_ids {
        let Some(asset) = assets.iter().find(|a| &a.id == id) else {
            continue;
        };
        if asset.kind == MediaKind::Audio {
            continue;
        }
        let Some(v) = asset.info.video.first() else {
            continue;
        };
        if v.width < MIN_DIMENSION || v.height < MIN_DIMENSION {
            continue;
        }
        // Bilder/Streams ohne echte Rate: nur die Auflösung übernehmen.
        let rate = if asset.kind == MediaKind::Video {
            FrameRate::from_fps(v.fps).unwrap_or(current.rate)
        } else {
            current.rate
        };
        let suggestion = SequenceSettings {
            rate,
            // Gerade Maße (Encoder-Kompatibilität), wie suggested_resolution.
            width: (v.width & !1).clamp(MIN_DIMENSION, MAX_DIMENSION),
            height: (v.height & !1).clamp(MIN_DIMENSION, MAX_DIMENSION),
            drop_frame: rate.supports_drop_frame(),
        };
        return Some((suggestion, asset.name.clone()));
    }
    None
}

/// Testmodus: `EDITRON_TEST_SEQUENCE="29.97df@1280x720"` — fps (Punkt oder
/// Komma), optionales `df`, optional `@BreitexHöhe`.
pub fn parse_test_sequence(spec: &str) -> Option<SequenceSettings> {
    let spec = spec.trim();
    let (rate_part, res_part) = match spec.split_once('@') {
        Some((r, res)) => (r, Some(res)),
        None => (spec, None),
    };
    let drop = rate_part.to_lowercase().ends_with("df");
    let fps_str = rate_part
        .trim_end_matches(['d', 'f', 'D', 'F'])
        .replace(',', ".");
    let rate = FrameRate::from_fps(fps_str.trim().parse().ok()?)?;
    let mut s = SequenceSettings {
        rate,
        drop_frame: drop,
        ..SequenceSettings::default()
    };
    if let Some(res) = res_part {
        let (w, h) = res.split_once(['x', 'X', '×'])?;
        s.width = w.trim().parse().ok()?;
        s.height = h.trim().parse().ok()?;
    }
    Some(s.sanitized())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntsc_rates_are_exact_rationals() {
        let r = FrameRate::from_fps(29.97).unwrap();
        assert_eq!(r, FrameRate::new(30000, 1001));
        // ffprobe liefert die exakte Division — wird ebenfalls erkannt.
        let r = FrameRate::from_fps(30000.0 / 1001.0).unwrap();
        assert_eq!(r, FrameRate::new(30000, 1001));
        assert_eq!(FrameRate::from_fps(23.976).unwrap(), FrameRate::new(24000, 1001));
        assert_eq!(FrameRate::from_fps(59.94).unwrap(), FrameRate::new(60000, 1001));
        assert_eq!(FrameRate::from_fps(25.0).unwrap(), FrameRate::new(25, 1));
        assert_eq!(FrameRate::from_fps(24.0).unwrap(), FrameRate::new(24, 1));
    }

    #[test]
    fn frame_math_is_drift_free_over_long_durations() {
        let r = FrameRate::new(30000, 1001);
        // 10 Stunden: exakt 36000 s × 30000/1001 = 1.078.921,078… → Frame 1.078.921.
        assert_eq!(r.frame_floor(36000.0), 1_078_921);
        // Mit gerundeten 29.97 fps wären es 1.078.920 — ein Frame Drift.
        assert_eq!((36000.0_f64 * 29.97).floor() as i64, 1_078_920);
        // Frame → Zeit → Frame ist verlustfrei.
        for f in [0i64, 1, 1799, 17982, 107_892, 1_078_921] {
            let t = r.time_of_frame(f as f64);
            assert_eq!(r.frame_round(t), f);
        }
    }

    #[test]
    fn drop_frame_only_for_ntsc_30_multiples() {
        assert!(FrameRate::new(30000, 1001).supports_drop_frame());
        assert!(FrameRate::new(60000, 1001).supports_drop_frame());
        assert!(!FrameRate::new(24000, 1001).supports_drop_frame());
        assert!(!FrameRate::new(25, 1).supports_drop_frame());
        assert!(!FrameRate::new(30, 1).supports_drop_frame());
    }

    #[test]
    fn labels_use_german_decimal_comma() {
        assert_eq!(FrameRate::new(30000, 1001).label(), "29,97");
        assert_eq!(FrameRate::new(24000, 1001).label(), "23,976");
        assert_eq!(FrameRate::new(25, 1).label(), "25");
    }

    #[test]
    fn sanitize_rejects_broken_values() {
        let s = SequenceSettings {
            rate: FrameRate::new(0, 0),
            width: 2,
            height: 100_000,
            drop_frame: true,
        }
        .sanitized();
        assert_eq!(s.rate, FrameRate::PAL_25);
        assert_eq!(s.width, MIN_DIMENSION);
        assert_eq!(s.height, MAX_DIMENSION);
        assert!(!s.drop_frame, "Drop-Frame ohne NTSC-Rate wird verworfen");
    }

    #[test]
    fn parse_test_sequence_specs() {
        let s = parse_test_sequence("29.97df@1280x720").unwrap();
        assert_eq!(s.rate, FrameRate::new(30000, 1001));
        assert!(s.drop_frame);
        assert_eq!((s.width, s.height), (1280, 720));
        let s = parse_test_sequence("25").unwrap();
        assert_eq!(s.rate, FrameRate::PAL_25);
        assert!(!s.drop_frame);
    }
}
