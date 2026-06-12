//! Titel-Generator-Clips: Datenmodell der Titelgestaltung (Text, Schrift,
//! Füllung, Kontur, Schatten, Hintergrund-Box, Ausrichtung/Position) plus
//! professionell gestaltete Vorlagen (Bauchbinde, zentrierter Titel,
//! Abspann-Rolle). Alle Maße sind in Pixeln bei 1080p-Referenzhöhe bzw. in
//! Prozent der Framemaße angegeben — Vorschau (Monitor-Canvas) und Export
//! (Zielauflösung) rastern denselben Spec auflösungsunabhängig identisch
//! (`core/text_raster.rs`).

use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};

/// Referenzhöhe der Pixelmaße: ein 48-px-Titel ist bei jeder Auflösung
/// 48/1080 der Framehöhe hoch.
pub const REF_HEIGHT: f64 = 1080.0;

// ---------------------------------------------------------------- Farben

/// RGBA-Farbe; serialisiert als Hex-String („#rrggbb“ bzw. „#rrggbbaa“).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RgbaColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl RgbaColor {
    pub const fn rgb(r: u8, g: u8, b: u8) -> RgbaColor {
        RgbaColor { r, g, b, a: 255 }
    }

    pub const WHITE: RgbaColor = RgbaColor::rgb(255, 255, 255);
    pub const BLACK: RgbaColor = RgbaColor::rgb(0, 0, 0);

    pub fn to_hex(&self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }

    pub fn from_hex(hex: &str) -> Option<RgbaColor> {
        let h = hex.trim().trim_start_matches('#');
        let parse = |s: &str| u8::from_str_radix(s, 16).ok();
        match h.len() {
            6 => Some(RgbaColor {
                r: parse(&h[0..2])?,
                g: parse(&h[2..4])?,
                b: parse(&h[4..6])?,
                a: 255,
            }),
            8 => Some(RgbaColor {
                r: parse(&h[0..2])?,
                g: parse(&h[2..4])?,
                b: parse(&h[4..6])?,
                a: parse(&h[6..8])?,
            }),
            _ => None,
        }
    }
}

impl Serialize for RgbaColor {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for RgbaColor {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        RgbaColor::from_hex(&raw)
            .ok_or_else(|| serde::de::Error::custom(format!("ungültige Farbe: {raw}")))
    }
}

// ------------------------------------------------------------- Aufzählungen

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TitleAlign {
    Left,
    #[default]
    Center,
    Right,
}

impl TitleAlign {
    pub const ALL: [TitleAlign; 3] = [TitleAlign::Left, TitleAlign::Center, TitleAlign::Right];

    pub fn label(&self) -> &'static str {
        match self {
            TitleAlign::Left => "Links",
            TitleAlign::Center => "Zentriert",
            TitleAlign::Right => "Rechts",
        }
    }
}

/// Schriftschnitt; wird über fontconfig-Gewichte aufgelöst.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TitleWeight {
    Regular,
    Medium,
    #[default]
    SemiBold,
    Bold,
}

impl TitleWeight {
    pub const ALL: [TitleWeight; 4] = [
        TitleWeight::Regular,
        TitleWeight::Medium,
        TitleWeight::SemiBold,
        TitleWeight::Bold,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            TitleWeight::Regular => "Regular",
            TitleWeight::Medium => "Medium",
            TitleWeight::SemiBold => "SemiBold",
            TitleWeight::Bold => "Bold",
        }
    }

    /// fontconfig-Gewichtsskala (FC_WEIGHT).
    pub fn fc_weight(&self) -> u32 {
        match self {
            TitleWeight::Regular => 80,
            TitleWeight::Medium => 100,
            TitleWeight::SemiBold => 180,
            TitleWeight::Bold => 200,
        }
    }
}

// ------------------------------------------------------------ Teilstrukturen

/// Schlagschatten: Versatz/Weichheit in px @1080p, Deckkraft 0–100 %.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleShadow {
    pub enabled: bool,
    pub dx: f64,
    pub dy: f64,
    pub blur: f64,
    pub color: RgbaColor,
    pub opacity: f64,
}

impl Default for TitleShadow {
    fn default() -> Self {
        TitleShadow {
            enabled: false,
            dx: 0.0,
            dy: 3.0,
            blur: 10.0,
            color: RgbaColor::BLACK,
            opacity: 70.0,
        }
    }
}

/// Hintergrund-Box hinter dem Textblock: Polster/Rundung in px @1080p,
/// Deckkraft 0–100 % (zusätzlich zum Alpha der Farbe).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleBox {
    pub enabled: bool,
    pub color: RgbaColor,
    pub opacity: f64,
    pub pad_x: f64,
    pub pad_y: f64,
    pub radius: f64,
}

impl Default for TitleBox {
    fn default() -> Self {
        TitleBox {
            enabled: false,
            color: RgbaColor::rgb(0x10, 0x10, 0x14),
            opacity: 70.0,
            pad_x: 22.0,
            pad_y: 12.0,
            radius: 6.0,
        }
    }
}

// ----------------------------------------------------------------- TitleSpec

/// Vollständige Gestaltung eines Titel-Clips. Der Clip-Transform
/// (Position/Skalierung/Rotation/Deckkraft inkl. Keyframes) liegt wie bei
/// Video-Clips in `TimelineClip.fx` — `pos_x`/`pos_y` hier verorten den
/// Textblock IM Frame (gebacken in den Raster), bevor der Transform greift.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleSpec {
    pub text: String,
    /// Schriftfamilie (fontconfig); leer = Standard-Sans der Plattform.
    #[serde(default)]
    pub font_family: String,
    #[serde(default)]
    pub weight: TitleWeight,
    #[serde(default)]
    pub italic: bool,
    /// Schriftgröße in px @1080p.
    pub size: f64,
    /// Zeilenhöhe als Faktor der Schriftgröße.
    #[serde(default = "default_line_spacing")]
    pub line_spacing: f64,
    /// Laufweite (Tracking) in px @1080p, zusätzlich zum Vorschub.
    #[serde(default)]
    pub letter_spacing: f64,
    #[serde(default)]
    pub align: TitleAlign,
    /// Blockmitte als Offset vom Framezentrum in % der Framebreite/-höhe
    /// (gleiche Einheiten wie `ClipFx`-Position).
    #[serde(default)]
    pub pos_x: f64,
    #[serde(default)]
    pub pos_y: f64,
    pub fill: RgbaColor,
    /// Konturbreite in px @1080p; 0 = keine Kontur.
    #[serde(default)]
    pub stroke_width: f64,
    #[serde(default = "default_stroke_color")]
    pub stroke_color: RgbaColor,
    #[serde(default)]
    pub shadow: TitleShadow,
    #[serde(default, rename = "box")]
    pub bg: TitleBox,
}

fn default_line_spacing() -> f64 {
    1.15
}

fn default_stroke_color() -> RgbaColor {
    RgbaColor::BLACK
}

impl Default for TitleSpec {
    fn default() -> Self {
        TitleSpec {
            text: "Titel".to_string(),
            font_family: String::new(),
            weight: TitleWeight::SemiBold,
            italic: false,
            size: 64.0,
            line_spacing: default_line_spacing(),
            letter_spacing: 0.0,
            align: TitleAlign::Center,
            pos_x: 0.0,
            pos_y: 0.0,
            fill: RgbaColor::WHITE,
            stroke_width: 0.0,
            stroke_color: default_stroke_color(),
            shadow: TitleShadow {
                enabled: true,
                ..Default::default()
            },
            bg: TitleBox::default(),
        }
    }
}

impl TitleSpec {
    /// Anzeigename des Clips: erste nicht-leere Textzeile, gekürzt.
    pub fn display_name(&self) -> String {
        let line = self
            .text
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        if line.is_empty() {
            "Titel".to_string()
        } else {
            line.chars().take(32).collect()
        }
    }

    /// Inhalts-Hash für Raster-Caches (deckt alle sichtbaren Felder ab).
    pub fn content_hash(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let f = |h: &mut std::collections::hash_map::DefaultHasher, v: f64| {
            v.to_bits().hash(h)
        };
        self.text.hash(&mut h);
        self.font_family.hash(&mut h);
        (self.weight as u8).hash(&mut h);
        self.italic.hash(&mut h);
        f(&mut h, self.size);
        f(&mut h, self.line_spacing);
        f(&mut h, self.letter_spacing);
        (self.align as u8).hash(&mut h);
        f(&mut h, self.pos_x);
        f(&mut h, self.pos_y);
        self.fill.to_hex().hash(&mut h);
        f(&mut h, self.stroke_width);
        self.stroke_color.to_hex().hash(&mut h);
        self.shadow.enabled.hash(&mut h);
        f(&mut h, self.shadow.dx);
        f(&mut h, self.shadow.dy);
        f(&mut h, self.shadow.blur);
        self.shadow.color.to_hex().hash(&mut h);
        f(&mut h, self.shadow.opacity);
        self.bg.enabled.hash(&mut h);
        self.bg.color.to_hex().hash(&mut h);
        f(&mut h, self.bg.opacity);
        f(&mut h, self.bg.pad_x);
        f(&mut h, self.bg.pad_y);
        f(&mut h, self.bg.radius);
        h.finish()
    }
}

// ------------------------------------------------------------------ Vorlagen

/// Titel-Vorlagen: ID, deutscher Name und Spec-Fabrik. Die Abspann-Rolle
/// erhält ihre Scroll-Animation beim Anlegen als `ClipFx`-Keyframes
/// (`commands::title.add`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleTemplate {
    Plain,
    LowerThird,
    Centered,
    CreditRoll,
}

impl TitleTemplate {
    pub const ALL: [TitleTemplate; 4] = [
        TitleTemplate::Plain,
        TitleTemplate::LowerThird,
        TitleTemplate::Centered,
        TitleTemplate::CreditRoll,
    ];

    pub fn key(&self) -> &'static str {
        match self {
            TitleTemplate::Plain => "plain",
            TitleTemplate::LowerThird => "lowerThird",
            TitleTemplate::Centered => "centered",
            TitleTemplate::CreditRoll => "creditRoll",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            TitleTemplate::Plain => "Einfacher Titel",
            TitleTemplate::LowerThird => "Bauchbinde",
            TitleTemplate::Centered => "Titel zentriert",
            TitleTemplate::CreditRoll => "Abspann (Rolle)",
        }
    }

    pub fn from_key(key: &str) -> Option<TitleTemplate> {
        TitleTemplate::ALL.iter().find(|t| t.key() == key).copied()
    }

    /// Standard-Clipdauer der Vorlage in Sekunden.
    pub fn default_duration(&self) -> f64 {
        match self {
            TitleTemplate::CreditRoll => 15.0,
            _ => 5.0,
        }
    }

    /// Scrollt die Vorlage über die Clipdauer (Abspann)?
    pub fn scrolls(&self) -> bool {
        *self == TitleTemplate::CreditRoll
    }

    pub fn build(&self) -> TitleSpec {
        match self {
            TitleTemplate::Plain => TitleSpec::default(),
            TitleTemplate::Centered => TitleSpec {
                text: "Titel".into(),
                size: 96.0,
                weight: TitleWeight::Bold,
                letter_spacing: 2.0,
                shadow: TitleShadow {
                    enabled: true,
                    dx: 0.0,
                    dy: 4.0,
                    blur: 18.0,
                    color: RgbaColor::BLACK,
                    opacity: 80.0,
                },
                ..TitleSpec::default()
            },
            // Sender-taugliche Bauchbinde: linksbündig im Title-Safe-Bereich
            // (±40 % / ±40 %), halbtransparente Box, kräftiger Schnitt.
            TitleTemplate::LowerThird => TitleSpec {
                text: "Vorname Nachname\nFunktion".into(),
                size: 44.0,
                weight: TitleWeight::Bold,
                align: TitleAlign::Left,
                pos_x: -24.0,
                pos_y: 33.0,
                fill: RgbaColor::WHITE,
                shadow: TitleShadow {
                    enabled: false,
                    ..Default::default()
                },
                bg: TitleBox {
                    enabled: true,
                    color: RgbaColor::rgb(0x10, 0x10, 0x14),
                    opacity: 72.0,
                    pad_x: 26.0,
                    pad_y: 14.0,
                    radius: 4.0,
                },
                ..TitleSpec::default()
            },
            TitleTemplate::CreditRoll => TitleSpec {
                text: "ABSPANN\n\nRegie\nMax Mustermann\n\nKamera\nErika Beispiel\n\nSchnitt\nEditron".into(),
                size: 40.0,
                weight: TitleWeight::Medium,
                line_spacing: 1.4,
                align: TitleAlign::Center,
                shadow: TitleShadow {
                    enabled: false,
                    ..Default::default()
                },
                ..TitleSpec::default()
            },
        }
    }
}

/// `EDITRON_TEST_TITLE`-Parser: `"text"` oder `"vorlage:text"` (Vorlagen-Keys
/// wie `lowerThird`); `\n` im Text wird zur neuen Zeile.
pub fn parse_test_title(raw: &str) -> (TitleTemplate, TitleSpec) {
    let (template, text) = match raw.split_once(':') {
        Some((key, rest)) => match TitleTemplate::from_key(key) {
            Some(t) => (t, rest),
            None => (TitleTemplate::Plain, raw),
        },
        None => (TitleTemplate::Plain, raw),
    };
    let mut spec = template.build();
    if !text.trim().is_empty() {
        spec.text = text.replace("\\n", "\n");
    }
    (template, spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_hex_roundtrip() {
        let c = RgbaColor::rgb(0x4f, 0x8d, 0xff);
        assert_eq!(c.to_hex(), "#4f8dff");
        assert_eq!(RgbaColor::from_hex("#4f8dff"), Some(c));
        let t = RgbaColor { a: 0x80, ..c };
        assert_eq!(RgbaColor::from_hex(&t.to_hex()), Some(t));
        assert_eq!(RgbaColor::from_hex("hellgrün"), None);
    }

    #[test]
    fn spec_serde_roundtrip_preserves_everything() {
        let mut spec = TitleTemplate::LowerThird.build();
        spec.italic = true;
        spec.stroke_width = 3.0;
        spec.stroke_color = RgbaColor::rgb(10, 20, 30);
        let json = serde_json::to_string(&spec).unwrap();
        let back: TitleSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
        assert_eq!(spec.content_hash(), back.content_hash());
    }

    #[test]
    fn content_hash_reacts_to_visible_changes() {
        let a = TitleSpec::default();
        let mut b = a.clone();
        b.text = "Anders".into();
        assert_ne!(a.content_hash(), b.content_hash());
        let mut c = a.clone();
        c.shadow.blur += 1.0;
        assert_ne!(a.content_hash(), c.content_hash());
    }

    #[test]
    fn display_name_uses_first_nonempty_line() {
        let mut spec = TitleSpec::default();
        spec.text = "\n  \nNachrichten\nUntertitel".into();
        assert_eq!(spec.display_name(), "Nachrichten");
        spec.text = "   ".into();
        assert_eq!(spec.display_name(), "Titel");
    }

    #[test]
    fn test_title_parser_supports_templates_and_newlines() {
        let (t, spec) = parse_test_title("lowerThird:Maria Muster\\nReporterin");
        assert_eq!(t, TitleTemplate::LowerThird);
        assert_eq!(spec.text, "Maria Muster\nReporterin");
        assert!(spec.bg.enabled);
        let (t2, spec2) = parse_test_title("Nur Text");
        assert_eq!(t2, TitleTemplate::Plain);
        assert_eq!(spec2.text, "Nur Text");
    }
}
