//! Sequenz-Export: Container-/Codec-Katalog, Settings + Render-Presets,
//! Validierung, Renderplan und der Render-Worker.
//!
//! Der Plan reproduziert exakt die Wiedergabe-Semantik des Players und des
//! Programmmonitors: alle sichtbaren Video-Layer werden mit ihren animierten
//! Transformationen (Position/Skalierung/Rotation/Deckkraft, Keyframes in
//! Medienzeit) von unten nach oben komponiert; Audio = Summe aller hörbaren
//! Clips mit Spur-Gain/Pan, Clip-Gain inkl. Lautstärke-Keyframes und
//! Master-Fader. Der Worker rendert in zwei Phasen — Audio-Mixdown in eine
//! temporäre f32-WAV, dann Video segmentweise: untransformierte Einzel-Layer
//! laufen direkt durch eine ffmpeg-Pipe (Schnellpfad), alles andere durch
//! den CPU-Compositor (`core/compose.rs`) mit einem Decoder je Layer
//! (transparent gepolsterte rawvideo/rgba-Frames). Finalisiert wird atomar
//! (`<ziel>.part` → rename). Abbruch über ein geteiltes Flag; jeder Fehler
//! wird als Event gemeldet, nie gepanict (`catch_unwind` als letzte Linie).


// Der Export ist in thematische Submodule zerlegt (Katalog, Settings/Presets,
// Renderplan, Validierung, Format-Helfer, Render-Worker). Sie sind crate-intern;
// ihre öffentlichen Items werden hier flach re-exportiert, damit
// `crate::core::export::…` unverändert auflöst.
mod catalog;
mod plan;
mod settings;
mod util;
mod validate;
mod worker;

pub use catalog::*;
pub use plan::*;
pub use settings::*;
pub use util::*;
pub use validate::*;
pub use worker::*;

#[cfg(test)]
mod tests;
