//! Untertitel: Datenmodell (Segment-Text pro Clip, Gestaltung pro Spur)
//! und SRT-Import/-Export. Die Darstellung läuft über den gemeinsamen
//! Text-Rasterizer der Titel (`core/text_raster.rs`): pro Untertitel-Layer
//! wird aus Spurstil + Segmenttext ein `TitleSpec` synthetisiert — Monitor,
//! Scopes und Export (Einbrennen) sind damit garantiert deckungsgleich.

use crate::core::title::{RgbaColor, TitleAlign, TitleBox, TitleShadow, TitleSpec, TitleWeight};
use serde::{Deserialize, Serialize};

/// Standarddauer eines neu angelegten Untertitel-Segments (Premiere: 3 s).
pub const DEFAULT_CUE_DURATION: f64 = 3.0;

/// Profi-Lesbarkeitsschwellen (Netflix/ARD-Untertitelungsnorm): bis 17
/// Zeichen pro Sekunde und höchstens 42 Zeichen je Zeile gelten als gut
/// lesbar — darüber warnt das Untertitel-Panel rot.
pub const MAX_CPS: f64 = 17.0;
pub const MAX_CHARS_PER_LINE: usize = 42;

/// Zeichen pro Sekunde (CPS) bei gegebener Anzeigedauer; 0 bei nicht-
/// positiver Dauer.
pub fn chars_per_second(char_count: usize, duration: f64) -> f64 {
    if duration > 0.0 {
        char_count as f64 / duration
    } else {
        0.0
    }
}

/// Lesbarkeit eines Segmenttexts bei gegebener Anzeigedauer:
/// `(cps, längste Zeile in Zeichen, Schwelle gerissen)`. „Gerissen" heißt
/// CPS > [`MAX_CPS`] **oder** Zeile > [`MAX_CHARS_PER_LINE`]. Zeichen ohne
/// Zeilenumbrüche, Leerzeichen zählen mit (Untertitelungsnorm). Eine Quelle
/// für die Panel-Warnung (und künftige Export-Hinweise).
pub fn readability(text: &str, duration: f64) -> (f64, usize, bool) {
    let chars = text.chars().filter(|c| *c != '\n').count();
    let max_line = text.lines().map(|l| l.chars().count()).max().unwrap_or(0);
    let cps = chars_per_second(chars, duration);
    (cps, max_line, cps > MAX_CPS || max_line > MAX_CHARS_PER_LINE)
}

// ------------------------------------------------------------------ Segment

/// Inhalt eines Untertitel-Segments (Clip ohne Mediendatei auf einer
/// Untertitel-Spur). Bewusst eine Struktur statt nackter String —
/// erweiterbar (z. B. Sprecher-Label), ohne das Format zu brechen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleSpec {
    pub text: String,
}

impl SubtitleSpec {
    pub fn new(text: impl Into<String>) -> SubtitleSpec {
        SubtitleSpec { text: text.into() }
    }

    /// Anzeigename des Clips: erste nicht-leere Zeile, gekürzt.
    pub fn display_name(&self) -> String {
        let line = self
            .text
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        if line.is_empty() {
            "Untertitel".to_string()
        } else {
            line.chars().take(40).collect()
        }
    }
}

// ----------------------------------------------------------------- Spurstil

/// Vertikale Positions-Presets (Anteil der Framehöhe als Offset vom
/// Zentrum, wie `TitleSpec::pos_y`): unteres Drittel innerhalb der
/// Title-Safe-Zone (±40 %) ist der Broadcast-Standard.
pub const SUBTITLE_POS_PRESETS: [(&str, f64); 2] = [("Unten", 38.0), ("Oben", -38.0)];

/// Gestaltung einer Untertitel-Spur — gilt für alle Segmente der Spur.
/// Maße in px @1080p (auflösungsunabhängig wie `TitleSpec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleStyle {
    /// Schriftfamilie (fontconfig); leer = Standard-Sans.
    #[serde(default)]
    pub font_family: String,
    /// Schriftschnitt (Gewicht); Profi-Untertitel sind meist halbfett.
    #[serde(default = "default_weight")]
    pub weight: TitleWeight,
    /// Schriftgröße in px @1080p.
    #[serde(default = "default_size")]
    pub size: f64,
    #[serde(default = "default_color")]
    pub color: RgbaColor,
    /// Schwarze Halbtransparenz-Box hinter dem Text.
    #[serde(default = "default_true")]
    pub bg_enabled: bool,
    /// Deckkraft der Box 0–100 %.
    #[serde(default = "default_bg_opacity")]
    pub bg_opacity: f64,
    /// Schwarze Kontur in px @1080p; 0 = keine (Alternative zur Box).
    #[serde(default)]
    pub stroke_width: f64,
    /// Blockmitte als Offset vom Framezentrum in % der Framehöhe
    /// (+38 = unteres Drittel, Title-Safe respektiert).
    #[serde(default = "default_pos_y")]
    pub pos_y: f64,
}

fn default_weight() -> TitleWeight {
    TitleWeight::SemiBold
}

fn default_size() -> f64 {
    48.0
}

fn default_color() -> RgbaColor {
    RgbaColor::WHITE
}

fn default_bg_opacity() -> f64 {
    70.0
}

fn default_pos_y() -> f64 {
    38.0
}

fn default_true() -> bool {
    true
}

impl Default for SubtitleStyle {
    fn default() -> Self {
        SubtitleStyle {
            font_family: String::new(),
            weight: default_weight(),
            size: default_size(),
            color: default_color(),
            bg_enabled: true,
            bg_opacity: default_bg_opacity(),
            stroke_width: 0.0,
            pos_y: default_pos_y(),
        }
    }
}

impl SubtitleStyle {
    /// Synthetisiert den `TitleSpec` eines Segments — die eine Quelle der
    /// Optik für Monitor-Vorschau UND Export-Einbrennen.
    pub fn title_spec(&self, text: &str) -> TitleSpec {
        TitleSpec {
            text: text.to_string(),
            font_family: self.font_family.clone(),
            weight: self.weight,
            italic: false,
            size: self.size,
            line_spacing: 1.2,
            letter_spacing: 0.0,
            align: TitleAlign::Center,
            pos_x: 0.0,
            pos_y: self.pos_y,
            fill: self.color,
            stroke_width: self.stroke_width,
            stroke_color: RgbaColor::BLACK,
            shadow: TitleShadow {
                enabled: false,
                ..Default::default()
            },
            bg: TitleBox {
                enabled: self.bg_enabled,
                color: RgbaColor::BLACK,
                opacity: self.bg_opacity,
                pad_x: 18.0,
                pad_y: 9.0,
                radius: 4.0,
            },
        }
    }
}

// ---------------------------------------------------------------------- SRT

/// Ein SRT-Cue: Zeiten in Sekunden, Text mit `\n`-Umbrüchen.
#[derive(Clone, Debug, PartialEq)]
pub struct SrtCue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// `HH:MM:SS,mmm` (tolerant: `.` statt `,`, fehlende Stunden, 1–3
/// ms-Stellen, Leerraum).
fn parse_srt_time(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    let (clock, ms_part) = match raw.rsplit_once([',', '.']) {
        Some((c, m)) => (c, m),
        None => (raw, "0"),
    };
    let ms_digits: String = ms_part.chars().take_while(|c| c.is_ascii_digit()).collect();
    if ms_digits.is_empty() && !ms_part.trim().is_empty() {
        return None;
    }
    // 1–3 Stellen: "5" = 500 ms, "50" = 500 ms, "500" = 500 ms.
    let ms = format!("{:0<3}", ms_digits)
        .get(..3)?
        .parse::<u64>()
        .ok()?;
    let parts: Vec<&str> = clock.split(':').collect();
    let (h, m, s) = match parts.as_slice() {
        [h, m, s] => (h.trim().parse::<u64>().ok()?, m.trim().parse::<u64>().ok()?, s.trim().parse::<u64>().ok()?),
        [m, s] => (0, m.trim().parse::<u64>().ok()?, s.trim().parse::<u64>().ok()?),
        _ => return None,
    };
    if m >= 60 || s >= 60 {
        return None;
    }
    // saturating gegen pathologische Stundenwerte in fremden/korrupten SRTs.
    let secs = h.saturating_mul(3600).saturating_add(m * 60).saturating_add(s);
    Some(secs as f64 + ms as f64 / 1000.0)
}

/// Sekunden → `HH:MM:SS,mmm` (auf Millisekunden gerundet).
pub fn format_srt_time(t: f64) -> String {
    let total_ms = (t.max(0.0) * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let s = (total_ms / 1000) % 60;
    let m = (total_ms / 60_000) % 60;
    let h = total_ms / 3_600_000;
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}

/// Einfache Inline-Formatierung entfernen: `<i>`/`<b>`/`<font …>`-Tags und
/// `{…}`-Blöcke (ASS-Reste) — Editron rendert Untertitel ohne Inline-Stile.
fn strip_inline_tags(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                // Nur echte Tags schlucken (Buchstabe oder '/' nach '<');
                // ein nacktes '<' im Text bleibt erhalten.
                if chars
                    .peek()
                    .is_some_and(|n| n.is_ascii_alphabetic() || *n == '/')
                {
                    for n in chars.by_ref() {
                        if n == '>' {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            '{' => {
                if chars.peek().is_some_and(|n| *n == '\\') {
                    for n in chars.by_ref() {
                        if n == '}' {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Datei-Bytes einer SRT dekodieren: UTF-8 (mit/ohne BOM), UTF-16 (BOM),
/// sonst Latin-1-Fallback (jedes Byte = Codepoint) — Windows-Altbestände
/// liefern sonst kaputte Umlaute.
pub fn decode_subtitle_bytes(bytes: &[u8]) -> String {
    match bytes {
        [0xff, 0xfe, rest @ ..] => {
            let units: Vec<u16> = rest
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        [0xfe, 0xff, rest @ ..] => {
            let units: Vec<u16> = rest
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => match std::str::from_utf8(bytes) {
            Ok(s) => s.to_string(),
            Err(_) => bytes.iter().map(|&b| b as char).collect(),
        },
    }
}

/// Robuster SRT-Parser: BOM, CRLF, mehrzeilige Texte, fehlerhafte/fehlende
/// Indizes und defekte Einzel-Cues werden toleriert. Fehler nur, wenn die
/// Datei keinen einzigen brauchbaren Cue enthält.
pub fn parse_srt(raw: &str) -> Result<Vec<SrtCue>, String> {
    let text = raw.trim_start_matches('\u{feff}');
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end_matches('\r')).collect();

    let mut cues: Vec<SrtCue> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        // Leerzeilen zwischen Blöcken überspringen.
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }
        // Blockanfang: optionale Indexzeile, dann die Timing-Zeile. Die
        // Timing-Zeile innerhalb der nächsten zwei Zeilen suchen — kaputte
        // Indizes ("3a", doppelt, fehlend) sind damit egal.
        let timing_idx = (i..lines.len().min(i + 2)).find(|&j| lines[j].contains("-->"));
        let Some(ti) = timing_idx else {
            // Kein Timing in Sicht: Block bis zur nächsten Leerzeile verwerfen.
            while i < lines.len() && !lines[i].trim().is_empty() {
                i += 1;
            }
            continue;
        };
        let (start, end) = {
            let mut halves = lines[ti].splitn(2, "-->");
            let a = halves.next().unwrap_or("");
            // Hinter der Endzeit können Positions-Hints stehen ("X1:…").
            let b = halves.next().unwrap_or("");
            let b = b.split_whitespace().next().unwrap_or("");
            (parse_srt_time(a), parse_srt_time(b))
        };
        // Textzeilen bis zur Leerzeile (oder bis eine neue Indexzeile +
        // Timing-Zeile beginnt — fehlende Leerzeilen tolerieren).
        let mut text_lines: Vec<&str> = Vec::new();
        let mut j = ti + 1;
        while j < lines.len() && !lines[j].trim().is_empty() {
            let looks_like_next_block = lines[j].trim().chars().all(|c| c.is_ascii_digit())
                && j + 1 < lines.len()
                && lines[j + 1].contains("-->");
            if looks_like_next_block || lines[j].contains("-->") {
                break;
            }
            text_lines.push(lines[j]);
            j += 1;
        }
        i = j;

        if let (Some(start), Some(end)) = (start, end) {
            let body = strip_inline_tags(&text_lines.join("\n"));
            let body = body.trim();
            if end > start && !body.is_empty() {
                cues.push(SrtCue {
                    start,
                    end,
                    text: body.to_string(),
                });
            }
        }
    }

    if cues.is_empty() {
        return Err("Keine gültigen Untertitel-Cues gefunden".to_string());
    }
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    Ok(cues)
}

/// Cues als SRT-Text (Index ab 1, `HH:MM:SS,mmm`, Leerzeile je Block).
pub fn format_srt(cues: &[SrtCue]) -> String {
    let mut out = String::new();
    for (i, cue) in cues.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            format_srt_time(cue.start),
            format_srt_time(cue.end),
            cue.text
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_spec_carries_track_styling() {
        let mut style = SubtitleStyle::default();
        style.size = 56.0;
        style.color = RgbaColor::rgb(255, 230, 0);
        style.pos_y = -38.0;
        style.bg_enabled = false;
        style.stroke_width = 3.0;
        style.weight = TitleWeight::Bold;
        let spec = style.title_spec("Hallo\nWelt");
        assert_eq!(spec.text, "Hallo\nWelt");
        assert_eq!(spec.size, 56.0);
        assert_eq!(spec.fill, RgbaColor::rgb(255, 230, 0));
        assert_eq!(spec.pos_y, -38.0);
        assert!(!spec.bg.enabled);
        assert_eq!(spec.stroke_width, 3.0);
        assert_eq!(spec.weight, TitleWeight::Bold);
        // Stil-/Textänderungen müssen den Raster-Cache invalidieren.
        let other = style.title_spec("Hallo\nWelt!");
        assert_ne!(spec.content_hash(), other.content_hash());
    }

    #[test]
    fn readability_counts_chars_lines_and_flags_thresholds() {
        // "Hallo Welt" (10) + "zweite Zeile" (12) = 22 Zeichen ohne Umbruch,
        // längste Zeile 12; bei 2 s ⇒ 11 CPS, beides unter der Schwelle.
        let (cps, max_line, over) = readability("Hallo Welt\nzweite Zeile", 2.0);
        assert!((cps - 11.0).abs() < 1e-9);
        assert_eq!(max_line, 12);
        assert!(!over);
        // Lange Einzelzeile (>42 Zeichen) reißt die Zeilenschwelle.
        let long = "a".repeat(50);
        let (_, ml, over_line) = readability(&long, 10.0);
        assert_eq!(ml, 50);
        assert!(over_line);
        // Zu schnell (>17 CPS) reißt die CPS-Schwelle trotz kurzer Zeile.
        let (cps_fast, _, over_cps) = readability("kurz aber hektisch", 0.5);
        assert!(cps_fast > MAX_CPS);
        assert!(over_cps);
        // Nicht-positive Dauer ⇒ CPS 0, keine CPS-Warnung.
        let (cps_zero, _, _) = readability("Text", 0.0);
        assert_eq!(cps_zero, 0.0);
    }

    #[test]
    fn parses_standard_srt_with_bom_and_crlf() {
        let raw = "\u{feff}1\r\n00:00:01,000 --> 00:00:02,500\r\nErste Zeile\r\nZweite Zeile\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000\r\nZweiter Cue\r\n";
        let cues = parse_srt(raw).expect("parse");
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start, 1.0);
        assert_eq!(cues[0].end, 2.5);
        assert_eq!(cues[0].text, "Erste Zeile\nZweite Zeile");
        assert_eq!(cues[1].text, "Zweiter Cue");
    }

    #[test]
    fn tolerates_broken_indices_and_missing_blank_lines() {
        // Kaputter Index, fehlender Index, fehlende Leerzeile zwischen Blöcken.
        let raw = "3a\n00:00:01,000 --> 00:00:02,000\nEins\n2\n00:00:03,000 --> 00:00:04,000\nZwei\n00:00:05,000 --> 00:00:06,000\nDrei";
        let cues = parse_srt(raw).expect("parse");
        assert_eq!(cues.len(), 3);
        assert_eq!(cues[0].text, "Eins");
        assert_eq!(cues[1].text, "Zwei");
        assert_eq!(cues[2].text, "Drei");
    }

    #[test]
    fn tolerates_dot_milliseconds_and_position_hints() {
        let raw = "1\n00:00:01.500 --> 00:00:02.500 X1:100 X2:200\nText";
        let cues = parse_srt(raw).expect("parse");
        assert_eq!(cues[0].start, 1.5);
        assert_eq!(cues[0].end, 2.5);
    }

    #[test]
    fn skips_invalid_cues_and_sorts_by_start() {
        let raw = "1\n00:00:09,000 --> 00:00:10,000\nSpät\n\n2\nkaputt --> 00:00:05,000\nWeg\n\n3\n00:00:04,000 --> 00:00:02,000\nRückwärts weg\n\n4\n00:00:01,000 --> 00:00:02,000\nFrüh\n";
        let cues = parse_srt(raw).expect("parse");
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "Früh");
        assert_eq!(cues[1].text, "Spät");
    }

    #[test]
    fn strips_inline_tags_but_keeps_plain_angle_brackets() {
        let raw = "1\n00:00:01,000 --> 00:00:02,000\n<i>Kursiv</i> und {\\an8}oben, 2 < 3 bleibt\n";
        let cues = parse_srt(raw).expect("parse");
        assert_eq!(cues[0].text, "Kursiv und oben, 2 < 3 bleibt");
    }

    #[test]
    fn decodes_utf8_utf16_and_latin1() {
        assert_eq!(decode_subtitle_bytes("Grüße".as_bytes()), "Grüße");
        // UTF-16LE mit BOM ("Aü").
        assert_eq!(
            decode_subtitle_bytes(&[0xff, 0xfe, 0x41, 0x00, 0xfc, 0x00]),
            "Aü"
        );
        // Latin-1: 0xfc = 'ü'.
        assert_eq!(decode_subtitle_bytes(&[b'G', b'r', 0xfc]), "Grü");
    }

    #[test]
    fn rejects_files_without_any_cue() {
        assert!(parse_srt("kein srt\nnur text\n").is_err());
        assert!(parse_srt("").is_err());
    }

    #[test]
    fn time_formatting_is_millisecond_exact() {
        assert_eq!(format_srt_time(0.0), "00:00:00,000");
        assert_eq!(format_srt_time(1.5), "00:00:01,500");
        assert_eq!(format_srt_time(3599.999), "00:59:59,999");
        assert_eq!(format_srt_time(3661.25), "01:01:01,250");
        // Rundung statt Abschneiden (Frame-Genauigkeit bei NTSC-Zeiten).
        assert_eq!(format_srt_time(2.0 / 3.0), "00:00:00,667");
    }

    #[test]
    fn srt_roundtrip_preserves_cues() {
        let cues = vec![
            SrtCue { start: 1.0, end: 2.5, text: "Erste Zeile\nZweite Zeile".into() },
            SrtCue { start: 3.0, end: 4.0, text: "Zweiter Cue".into() },
        ];
        let raw = format_srt(&cues);
        let back = parse_srt(&raw).expect("roundtrip");
        assert_eq!(back, cues);
    }

    #[test]
    fn style_serde_roundtrip_and_defaults() {
        let style = SubtitleStyle {
            font_family: "Inter".into(),
            weight: TitleWeight::Bold,
            size: 52.0,
            color: RgbaColor::rgb(255, 255, 0),
            bg_enabled: false,
            bg_opacity: 50.0,
            stroke_width: 2.5,
            pos_y: -38.0,
        };
        let json = serde_json::to_string(&style).unwrap();
        let back: SubtitleStyle = serde_json::from_str(&json).unwrap();
        assert_eq!(style, back);
        // Leeres Objekt (ältere Datei) → Standardstil.
        let from_empty: SubtitleStyle = serde_json::from_str("{}").unwrap();
        assert_eq!(from_empty, SubtitleStyle::default());
    }
}
