//! Medien-Organisation: Bins (verschachtelbare Ordner), Farbetiketten und
//! der persistierte Ansichts-Zustand des Medien-Browsers (Premiere-Projekt-
//! Panel / Resolve-Media-Pool).
//!
//! Assets bleiben weiterhin eine flache Liste im [`crate::stores::MediaStore`]
//! (jedes Asset trägt ein `bin_id`); Bins sind ein getrennter Baum. Die Wurzel
//! ([`ROOT_BIN_ID`]) ist implizit und nicht Teil von `bins`.

use serde::{Deserialize, Serialize};

/// Implizite Wurzel. Assets ohne Bin (Altprojekte) sowie frisch importierte
/// Medien landen hier.
pub const ROOT_BIN_ID: &str = "root";

/// Ein Ordner im Medien-Browser. Beliebig verschachtelbar über `parent`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bin {
    pub id: String,
    pub name: String,
    /// Eltern-Bin; `ROOT_BIN_ID` für eine Top-Level-Ablage.
    pub parent: String,
}

impl Bin {
    pub fn new(name: impl Into<String>, parent: impl Into<String>) -> Bin {
        Bin {
            id: crate::core::types::new_id(),
            name: name.into(),
            parent: parent.into(),
        }
    }
}

/// Farbetikett eines Assets (Premiere-Label). Acht klar unterscheidbare
/// Farben; das Etikett ist rein organisatorisch (kein Effekt auf das Bild).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaLabel {
    Rose,
    Red,
    Orange,
    Yellow,
    Green,
    Cyan,
    Blue,
    Violet,
}

impl MediaLabel {
    pub const ALL: [MediaLabel; 8] = [
        MediaLabel::Rose,
        MediaLabel::Red,
        MediaLabel::Orange,
        MediaLabel::Yellow,
        MediaLabel::Green,
        MediaLabel::Cyan,
        MediaLabel::Blue,
        MediaLabel::Violet,
    ];

    /// RGB-Tripel für die UI-Darstellung.
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            MediaLabel::Rose => (0xe8, 0xa0, 0xb4),
            MediaLabel::Red => (0xeb, 0x4d, 0x4d),
            MediaLabel::Orange => (0xf0, 0x97, 0x33),
            MediaLabel::Yellow => (0xe8, 0xd0, 0x44),
            MediaLabel::Green => (0x3e, 0xc8, 0x6a),
            MediaLabel::Cyan => (0x3a, 0xc8, 0xd8),
            MediaLabel::Blue => (0x4f, 0x8d, 0xff),
            MediaLabel::Violet => (0xa9, 0x60, 0xdf),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MediaLabel::Rose => "Rosa",
            MediaLabel::Red => "Rot",
            MediaLabel::Orange => "Orange",
            MediaLabel::Yellow => "Gelb",
            MediaLabel::Green => "Grün",
            MediaLabel::Cyan => "Cyan",
            MediaLabel::Blue => "Blau",
            MediaLabel::Violet => "Violett",
        }
    }

    /// Stabiler Schlüssel für Command-Argumente.
    pub fn key(self) -> &'static str {
        match self {
            MediaLabel::Rose => "rose",
            MediaLabel::Red => "red",
            MediaLabel::Orange => "orange",
            MediaLabel::Yellow => "yellow",
            MediaLabel::Green => "green",
            MediaLabel::Cyan => "cyan",
            MediaLabel::Blue => "blue",
            MediaLabel::Violet => "violet",
        }
    }

    pub fn from_key(key: &str) -> Option<MediaLabel> {
        MediaLabel::ALL.into_iter().find(|c| c.key() == key)
    }
}

/// Ansichtsmodus des Browsers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    Grid,
    List,
}

impl Default for ViewMode {
    fn default() -> Self {
        ViewMode::Grid
    }
}

/// Sortierschlüssel der Listenansicht. `Name` ist die Standardsortierung.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortKey {
    Name,
    Duration,
    Fps,
    Resolution,
    Codec,
    Audio,
    Size,
    Path,
    Date,
}

impl Default for SortKey {
    fn default() -> Self {
        SortKey::Name
    }
}

/// Spaltendefinition der Listenansicht: Sortierschlüssel, Kopf-Label und
/// Standardbreite in Pixeln. Die Name-Spalte ist flexibel (füllt den Rest).
pub struct ColumnDef {
    pub key: SortKey,
    pub label: &'static str,
    pub default_w: f32,
}

/// Metadaten-Spalten in Anzeigereihenfolge (ohne die flexible Name-Spalte).
pub const COLUMNS: [ColumnDef; 8] = [
    ColumnDef { key: SortKey::Duration, label: "Dauer", default_w: 80.0 },
    ColumnDef { key: SortKey::Fps, label: "Framerate", default_w: 76.0 },
    ColumnDef { key: SortKey::Resolution, label: "Auflösung", default_w: 100.0 },
    ColumnDef { key: SortKey::Codec, label: "Codec", default_w: 84.0 },
    ColumnDef { key: SortKey::Audio, label: "Audio", default_w: 70.0 },
    ColumnDef { key: SortKey::Size, label: "Größe", default_w: 84.0 },
    ColumnDef { key: SortKey::Date, label: "Aufnahme", default_w: 130.0 },
    ColumnDef { key: SortKey::Path, label: "Pfad", default_w: 240.0 },
];

/// Persistierter Ansichts-Zustand des Medien-Browsers (Teil der .etron-Datei,
/// nicht der Undo-History). Spaltenbreiten werden parallel zu [`COLUMNS`]
/// gehalten.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaViewState {
    #[serde(default)]
    pub mode: ViewMode,
    #[serde(default)]
    pub sort: SortKey,
    #[serde(default)]
    pub sort_desc: bool,
    /// Breiten der Metadaten-Spalten (parallel zu [`COLUMNS`]).
    #[serde(default)]
    pub col_widths: Vec<f32>,
    /// Kachelgröße der Rasteransicht in Pixeln (Mindestbreite einer Spalte).
    #[serde(default = "default_tile")]
    pub tile_w: f32,
    /// Aktuell geöffneter Bin (Navigationszustand).
    #[serde(default = "default_bin")]
    pub current_bin: String,
}

fn default_tile() -> f32 {
    136.0
}

fn default_bin() -> String {
    ROOT_BIN_ID.to_string()
}

impl Default for MediaViewState {
    fn default() -> Self {
        MediaViewState {
            mode: ViewMode::Grid,
            sort: SortKey::Name,
            sort_desc: false,
            col_widths: COLUMNS.iter().map(|c| c.default_w).collect(),
            tile_w: default_tile(),
            current_bin: ROOT_BIN_ID.to_string(),
        }
    }
}

impl MediaViewState {
    /// Defensiv normalisieren (fremde/kaputte Dateien): Spaltenbreiten auf die
    /// aktuelle Spaltenzahl bringen und in einen vernünftigen Bereich klemmen.
    pub fn sanitize(&mut self) {
        if self.col_widths.len() != COLUMNS.len() {
            self.col_widths = COLUMNS.iter().map(|c| c.default_w).collect();
        }
        for (w, def) in self.col_widths.iter_mut().zip(COLUMNS.iter()) {
            if !w.is_finite() || *w < 40.0 {
                *w = def.default_w;
            }
            *w = w.min(640.0);
        }
        if !self.tile_w.is_finite() {
            self.tile_w = default_tile();
        }
        self.tile_w = self.tile_w.clamp(96.0, 280.0);
        if self.current_bin.is_empty() {
            self.current_bin = ROOT_BIN_ID.to_string();
        }
    }
}
