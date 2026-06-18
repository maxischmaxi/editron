//! Adjustment Layer (Einstellungsebene): ein synthetischer Clip OHNE eigene
//! Mediendatei (`asset_id` leer), der seine Farbkorrektur (`clip.grade`) und
//! seinen Effekt-Stapel (`clip.effects`) NICHT auf eigenes Material, sondern
//! als Pass auf das bereits zusammengesetzte Bild ALLER darunterliegenden
//! Spuren anwendet (Premiere-/After-Effects-Konvention „Adjustment Layer“).
//!
//! Wie Titel/Untertitel ist er ein Generator-Clip: der Compositor erkennt ihn
//! an `clip.adjustment` und behandelt ihn an seiner Position in der
//! Zeichenreihenfolge (unten → oben) als Korrektur-Stufe auf dem Canvas, statt
//! ein eigenes Blatt zu holen. Player (CPU-Programm-Composite) und Export
//! (`render_segment_composited`) teilen sich diese Stufe ⇒ Vorschau und Export
//! sehen identisch aus. Die Deckkraft des Clips (`fx.opacity`) regelt die
//! Wirkstärke (Überblendung Vorher/Nachher).
//!
//! Bewusste v1-Grenze: 3D-LUT-Slots eines Adjustment-Grades werden (anders als
//! bei normalen Clips) NICHT angewendet — die Pass-Stufe rechnet mit
//! [`crate::core::lut::LutStack::EMPTY`], damit der reine CPU-Pfad (Player ==
//! Export) ohne Datei-IO im Compositing-Kern formelgleich bleibt. Die übrigen
//! Grade-Werkzeuge (Räder, Belichtung, Kontrast, Sättigung, Kurven, Vignette,
//! Looks) wirken vollständig.

use serde::{Deserialize, Serialize};

/// Marker eines Adjustment-Layer-Clips. Der eigentliche Inhalt (Grade/Effekte)
/// liegt — wie bei normalen Clips — in `clip.grade`/`clip.effects`; dieser Spec
/// kennzeichnet den Clip nur als Korrektur-Stufe und hält Raum für spätere
/// adjustment-spezifische Optionen (Formatversion 20).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdjustmentSpec {
    /// Felder einer NEUEREN Editron-Version, verlustfrei durchgereicht.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl AdjustmentSpec {
    pub fn new() -> AdjustmentSpec {
        AdjustmentSpec::default()
    }

    /// Anzeigename eines frischen Adjustment-Clips.
    pub fn display_name() -> String {
        "Einstellungsebene".to_string()
    }
}
