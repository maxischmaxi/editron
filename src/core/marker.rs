//! Marker: Sequenz-, Clip- und Asset-Marker auf Premiere-Pro-Niveau.
//! Ein Marker ist ein benannter, gefärbter Zeitpunkt (optional ein Bereich
//! mit Dauer) mit einer Notiz. Sequenz-Marker liegen in Sequenz-Sekunden,
//! Clip- und Asset-Marker in Medien-/Quell-Sekunden (sie wandern dadurch
//! beim Trimmen/Verschieben korrekt mit dem Material mit).

use crate::core::types::new_id;
use serde::{Deserialize, Serialize};

/// Die acht Standard-Markerfarben (Reihenfolge wie in Adobe Premiere Pro).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkerColor {
    Green,
    Red,
    Purple,
    Orange,
    Yellow,
    White,
    Blue,
    Cyan,
}

impl Default for MarkerColor {
    /// Premiere setzt einen frischen Marker grün.
    fn default() -> Self {
        MarkerColor::Green
    }
}

impl MarkerColor {
    /// Alle Farben in Anzeigereihenfolge (für Swatch-Reihen, Filter, Zyklus).
    pub const ALL: [MarkerColor; 8] = [
        MarkerColor::Green,
        MarkerColor::Red,
        MarkerColor::Purple,
        MarkerColor::Orange,
        MarkerColor::Yellow,
        MarkerColor::White,
        MarkerColor::Blue,
        MarkerColor::Cyan,
    ];

    /// Darstellungsfarbe als RGB (raylib-frei, damit `core` UI-unabhängig
    /// bleibt — der UI-Layer wandelt in `Color`).
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            MarkerColor::Green => (0x3e, 0xc8, 0x6a),
            MarkerColor::Red => (0xeb, 0x4d, 0x4d),
            MarkerColor::Purple => (0xa9, 0x60, 0xdf),
            MarkerColor::Orange => (0xf0, 0x97, 0x33),
            MarkerColor::Yellow => (0xe8, 0xd0, 0x44),
            MarkerColor::White => (0xe6, 0xe8, 0xee),
            MarkerColor::Blue => (0x4f, 0x8d, 0xff),
            MarkerColor::Cyan => (0x3a, 0xc8, 0xd8),
        }
    }

    /// Deutscher Anzeigename (Kontextmenü, Tooltip, Dialog).
    pub fn label(self) -> &'static str {
        match self {
            MarkerColor::Green => "Grün",
            MarkerColor::Red => "Rot",
            MarkerColor::Purple => "Violett",
            MarkerColor::Orange => "Orange",
            MarkerColor::Yellow => "Gelb",
            MarkerColor::White => "Weiß",
            MarkerColor::Blue => "Blau",
            MarkerColor::Cyan => "Cyan",
        }
    }

    /// Stabiler Schlüssel für Command-Argumente (Kontextmenü → Command).
    pub fn key(self) -> &'static str {
        match self {
            MarkerColor::Green => "green",
            MarkerColor::Red => "red",
            MarkerColor::Purple => "purple",
            MarkerColor::Orange => "orange",
            MarkerColor::Yellow => "yellow",
            MarkerColor::White => "white",
            MarkerColor::Blue => "blue",
            MarkerColor::Cyan => "cyan",
        }
    }

    /// Aus dem Command-Schlüssel zurück (None bei Unbekanntem).
    pub fn from_key(key: &str) -> Option<MarkerColor> {
        MarkerColor::ALL.into_iter().find(|c| c.key() == key)
    }
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// Ein Marker. `time` ist je nach Träger Sequenz- oder Medien-Sekunden;
/// `duration` > 0 macht ihn zum Bereichsmarker (Premiere: „Marker mit Dauer").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Marker {
    pub id: String,
    pub time: f64,
    /// Bereichslänge in Sekunden (0 = Punktmarker).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub duration: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    #[serde(default)]
    pub color: MarkerColor,
}

impl Marker {
    /// Frischer Punktmarker bei `time` mit Standardfarbe und leerem Namen
    /// (Premiere zeigt dann den Timecode an).
    pub fn new(time: f64) -> Marker {
        Marker {
            id: new_id(),
            time,
            duration: 0.0,
            name: String::new(),
            note: String::new(),
            color: MarkerColor::default(),
        }
    }

    /// Ende eines Bereichsmarkers (= `time` bei Punktmarkern).
    pub fn end(&self) -> f64 {
        self.time + self.duration.max(0.0)
    }

    /// Defensive Bereinigung beim Laden (Fremd-/Altdateien).
    pub fn sanitize(&mut self) {
        if !self.time.is_finite() {
            self.time = 0.0;
        }
        self.time = self.time.max(0.0);
        if !self.duration.is_finite() || self.duration < 0.0 {
            self.duration = 0.0;
        }
        if self.id.is_empty() {
            self.id = new_id();
        }
    }
}

/// Welche Marker-Sammlung der Bearbeiten-Dialog / die Commands meinen.
#[derive(Clone, Debug, PartialEq)]
pub enum MarkerScope {
    /// Sequenz-Marker (im TimelineStore).
    Sequence,
    /// Clip-Marker am angegebenen Clip (Medienzeit).
    Clip(String),
    /// Asset-Marker am angegebenen Asset (Quellzeit, Quellmonitor).
    Asset(String),
}
