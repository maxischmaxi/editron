//! Übergänge (Transitions): Datenmodell + gemeinsame Auswertungs-Mathematik.
//!
//! Ein [`Transition`] sitzt an einer Schnittkante einer Spur — zwischen zwei
//! direkt benachbarten Clips (`from` → `to`), am Anfang eines Clips (nur
//! `to`, z. B. Einblenden) oder am Ende (nur `from`, Ausblenden). Die
//! Premiere-Semantik der Handles gilt strikt: über die Schnittkante hinaus
//! gezeigtes Material muss in der Quelle vorhanden sein; [`max_duration`]
//! begrenzt die Dauer entsprechend (Bilder haben unbegrenzte Handles).
//!
//! [`eval_video`] ist die EINZIGE Formelquelle für das visuelle Verhalten —
//! Programmmonitor (GPU) und Export-Compositor (CPU) rufen dieselbe Funktion
//! und liefern damit pro Frame dasselbe Ergebnis. [`audio_gain`] ist das
//! Pendant für Audio-Crossfades (Player-Mixdown UND Export-Phase A).

use crate::core::timeline::TimelineClip;
use crate::core::types::new_id;
use serde::{Deserialize, Serialize};

/// Standarddauer neuer Übergänge in Sekunden (Premiere: 1 s Video/Audio).
pub const DEFAULT_TRANSITION_DURATION: f64 = 1.0;

// ------------------------------------------------------------------ Katalog

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransitionKind {
    // ---- Video ----
    CrossDissolve,
    DipToBlack,
    DipToWhite,
    Wipe,
    Push,
    Zoom,
    // ---- Audio ----
    ConstantPower,
    ConstantGain,
}

impl TransitionKind {
    pub const ALL: [TransitionKind; 8] = [
        TransitionKind::CrossDissolve,
        TransitionKind::DipToBlack,
        TransitionKind::DipToWhite,
        TransitionKind::Wipe,
        TransitionKind::Push,
        TransitionKind::Zoom,
        TransitionKind::ConstantPower,
        TransitionKind::ConstantGain,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            TransitionKind::CrossDissolve => "Überblendung",
            TransitionKind::DipToBlack => "Blende zu Schwarz",
            TransitionKind::DipToWhite => "Blende zu Weiß",
            TransitionKind::Wipe => "Wischen",
            TransitionKind::Push => "Schieben",
            TransitionKind::Zoom => "Zoom",
            TransitionKind::ConstantPower => "Konstante Leistung",
            TransitionKind::ConstantGain => "Konstante Verstärkung",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            TransitionKind::CrossDissolve => {
                "Weiche Überblendung: das neue Bild blendet linear über das alte."
            }
            TransitionKind::DipToBlack => {
                "Abblende über Schwarz: erst zu Schwarz, dann ins neue Bild."
            }
            TransitionKind::DipToWhite => {
                "Abblende über Weiß: erst zu Weiß, dann ins neue Bild."
            }
            TransitionKind::Wipe => {
                "Wischblende mit harter Kante — Richtung über das Kontextmenü wählbar."
            }
            TransitionKind::Push => {
                "Das neue Bild schiebt das alte aus dem Frame — Richtung wählbar."
            }
            TransitionKind::Zoom => {
                "Zoom-Übergang: ins alte Bild hineinzoomen, aus dem neuen herauszoomen."
            }
            TransitionKind::ConstantPower => {
                "Audio-Crossfade mit konstanter Leistung (sin/cos) — der Standard für Musik und Atmo."
            }
            TransitionKind::ConstantGain => {
                "Lineares Audio-Crossfade mit konstanter Verstärkung — präzise, aber mit hörbarer Mittensenke."
            }
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            TransitionKind::CrossDissolve => "blend",
            TransitionKind::DipToBlack => "contrast",
            TransitionKind::DipToWhite => "sun",
            TransitionKind::Wipe => "arrow-right-from-line",
            TransitionKind::Push => "move-horizontal",
            TransitionKind::Zoom => "zoom-in",
            TransitionKind::ConstantPower => "audio-waveform",
            TransitionKind::ConstantGain => "audio-lines",
        }
    }

    /// camelCase-Schlüssel (Commands, Test-Flags, Suche).
    pub fn key(&self) -> &'static str {
        match self {
            TransitionKind::CrossDissolve => "crossDissolve",
            TransitionKind::DipToBlack => "dipToBlack",
            TransitionKind::DipToWhite => "dipToWhite",
            TransitionKind::Wipe => "wipe",
            TransitionKind::Push => "push",
            TransitionKind::Zoom => "zoom",
            TransitionKind::ConstantPower => "constantPower",
            TransitionKind::ConstantGain => "constantGain",
        }
    }

    pub fn is_audio(&self) -> bool {
        matches!(
            self,
            TransitionKind::ConstantPower | TransitionKind::ConstantGain
        )
    }

    /// Blendet über eine deckende Farbe (Schwarz/Weiß)?
    pub fn is_dip(&self) -> bool {
        matches!(self, TransitionKind::DipToBlack | TransitionKind::DipToWhite)
    }

    /// Nutzt die Richtungs-Einstellung?
    pub fn directional(&self) -> bool {
        matches!(self, TransitionKind::Wipe | TransitionKind::Push)
    }

    pub fn default_direction(&self) -> TransitionDirection {
        match self {
            // Wischkante läuft von links nach rechts; Schieben nach links.
            TransitionKind::Push => TransitionDirection::Left,
            _ => TransitionDirection::Right,
        }
    }

    /// Standardübergang je Spurart (Mod+D / Mod+Shift+D).
    pub fn default_for_audio(audio: bool) -> TransitionKind {
        if audio {
            TransitionKind::ConstantPower
        } else {
            TransitionKind::CrossDissolve
        }
    }
}

/// Lage des Übergangs relativ zur Schnittkante (Premiere-Ausrichtung).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransitionAlignment {
    /// Mittig über dem Schnitt (Standard).
    #[default]
    Center,
    /// Beginnt am Schnitt (liegt komplett auf dem eingehenden Clip).
    StartAtCut,
    /// Endet am Schnitt (liegt komplett auf dem ausgehenden Clip).
    EndAtCut,
}

impl TransitionAlignment {
    pub fn label(&self) -> &'static str {
        match self {
            TransitionAlignment::Center => "Schnitt zentrieren",
            TransitionAlignment::StartAtCut => "Am Schnittpunkt beginnen",
            TransitionAlignment::EndAtCut => "Am Schnittpunkt enden",
        }
    }

    pub const ALL: [TransitionAlignment; 3] = [
        TransitionAlignment::Center,
        TransitionAlignment::StartAtCut,
        TransitionAlignment::EndAtCut,
    ];
}

/// Bewegungsrichtung: beim Wischen die Laufrichtung der Kante, beim
/// Schieben die Richtung, in die das alte Bild hinausgeschoben wird.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransitionDirection {
    Left,
    #[default]
    Right,
    Up,
    Down,
}

impl TransitionDirection {
    pub fn label(&self) -> &'static str {
        match self {
            TransitionDirection::Left => "Nach links",
            TransitionDirection::Right => "Nach rechts",
            TransitionDirection::Up => "Nach oben",
            TransitionDirection::Down => "Nach unten",
        }
    }

    pub const ALL: [TransitionDirection; 4] = [
        TransitionDirection::Left,
        TransitionDirection::Right,
        TransitionDirection::Up,
        TransitionDirection::Down,
    ];

    /// Einheitsvektor in Frame-Bruchteilen (x nach rechts, y nach unten).
    fn vector(&self) -> (f64, f64) {
        match self {
            TransitionDirection::Left => (-1.0, 0.0),
            TransitionDirection::Right => (1.0, 0.0),
            TransitionDirection::Up => (0.0, -1.0),
            TransitionDirection::Down => (0.0, 1.0),
        }
    }
}

// ------------------------------------------------------------------- Modell

/// Übergang an einer Schnittkante. Verankert über Clip-IDs: genau eine
/// Seite gesetzt = Ein-/Ausblenden am Cliprand; beide gesetzt = Übergang
/// zwischen benachbarten Clips derselben Spur (from.end == to.start).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transition {
    pub id: String,
    pub kind: TransitionKind,
    /// Ausgehender Clip (links der Kante).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_clip_id: Option<String>,
    /// Eingehender Clip (rechts der Kante).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_clip_id: Option<String>,
    /// Gesamtdauer in Sekunden.
    pub duration: f64,
    #[serde(default)]
    pub alignment: TransitionAlignment,
    #[serde(default)]
    pub direction: TransitionDirection,
}

impl Transition {
    pub fn new(
        kind: TransitionKind,
        from_clip_id: Option<String>,
        to_clip_id: Option<String>,
        duration: f64,
    ) -> Transition {
        Transition {
            id: new_id(),
            kind,
            from_clip_id,
            to_clip_id,
            duration,
            alignment: TransitionAlignment::Center,
            direction: kind.default_direction(),
        }
    }

    pub fn is_two_sided(&self) -> bool {
        self.from_clip_id.is_some() && self.to_clip_id.is_some()
    }
}

/// from-/to-Clips eines Übergangs in einer Clipliste auflösen.
pub fn resolve_clips<'a>(
    clips: &'a [TimelineClip],
    tr: &Transition,
) -> (Option<&'a TimelineClip>, Option<&'a TimelineClip>) {
    let find = |id: &Option<String>| {
        id.as_deref()
            .and_then(|id| clips.iter().find(|c| c.id == id))
    };
    (find(&tr.from_clip_id), find(&tr.to_clip_id))
}

/// Schnittkante des Übergangs in Sequenzzeit. Einseitige Übergänge nutzen
/// den betroffenen Cliprand als „Kante“.
pub fn cut_time(from: Option<&TimelineClip>, to: Option<&TimelineClip>) -> Option<f64> {
    match (from, to) {
        (Some(f), _) => Some(f.end()),
        (None, Some(t)) => Some(t.start),
        (None, None) => None,
    }
}

/// Zeitfenster `[w0, w1)` des Übergangs in Sequenzzeit. Einseitige
/// Übergänge liegen vollständig auf ihrem Clip (Ausrichtung irrelevant).
pub fn window(
    from: Option<&TimelineClip>,
    to: Option<&TimelineClip>,
    alignment: TransitionAlignment,
    duration: f64,
) -> Option<(f64, f64)> {
    match (from, to) {
        (Some(f), Some(_)) => {
            let cut = f.end();
            let (pre, post) = alignment_split(alignment, duration);
            Some((cut - pre, cut + post))
        }
        (Some(f), None) => Some((f.end() - duration, f.end())),
        (None, Some(t)) => Some((t.start, t.start + duration)),
        (None, None) => None,
    }
}

/// Anteile vor/nach der Schnittkante je Ausrichtung.
fn alignment_split(alignment: TransitionAlignment, duration: f64) -> (f64, f64) {
    match alignment {
        TransitionAlignment::Center => (duration / 2.0, duration / 2.0),
        TransitionAlignment::StartAtCut => (0.0, duration),
        TransitionAlignment::EndAtCut => (duration, 0.0),
    }
}

/// Frame-genaue Anteile vor/nach der Schnittkante (Interop-Export). Die Summe
/// ergibt stets exakt `frames` (zentriert: vorne abgerundet, hinten der Rest).
pub fn alignment_split_frames(alignment: TransitionAlignment, frames: i64) -> (i64, i64) {
    match alignment {
        TransitionAlignment::Center => {
            let pre = frames / 2;
            (pre, frames - pre)
        }
        TransitionAlignment::StartAtCut => (0, frames),
        TransitionAlignment::EndAtCut => (frames, 0),
    }
}

// ------------------------------------------------------------------ Handles

/// Quellmaterial VOR der Timeline-Kopfkante, in Timeline-Sekunden
/// (speed-bewusst; Bilder/Standbilder sind frei dehnbar).
pub fn head_handle(clip: &TimelineClip) -> f64 {
    crate::core::timeline::head_room(clip)
}

/// Quellmaterial NACH der Timeline-Endkante, in Timeline-Sekunden.
pub fn tail_handle(clip: &TimelineClip) -> f64 {
    crate::core::timeline::tail_room(clip)
}

/// Maximal mögliche Übergangsdauer an dieser Kante (Premiere-Regeln):
/// Der Teil nach dem Schnitt braucht Schwanz-Handle des ausgehenden Clips
/// und darf den eingehenden Clip nicht überragen; der Teil davor braucht
/// Kopf-Handle des eingehenden Clips und darf den ausgehenden Clip nicht
/// überragen. Einseitige Übergänge brauchen keine Handles — sie sind durch
/// die Cliplänge begrenzt.
pub fn max_duration(
    from: Option<&TimelineClip>,
    to: Option<&TimelineClip>,
    alignment: TransitionAlignment,
) -> f64 {
    match (from, to) {
        (Some(f), Some(t)) => {
            let post_limit = tail_handle(f).min(t.duration);
            let pre_limit = head_handle(t).min(f.duration);
            match alignment {
                TransitionAlignment::Center => 2.0 * post_limit.min(pre_limit),
                TransitionAlignment::StartAtCut => post_limit,
                TransitionAlignment::EndAtCut => pre_limit,
            }
        }
        (Some(f), None) => f.duration,
        (None, Some(t)) => t.duration,
        (None, None) => 0.0,
    }
}

// ----------------------------------------------------------- Video-Auswertung

/// Rolle eines Layers innerhalb des Übergangs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionRole {
    /// Ausgehender Clip eines zweiseitigen Übergangs.
    Out,
    /// Eingehender Clip eines zweiseitigen Übergangs.
    In,
    /// Einseitiges Ausblenden am Clipende.
    OutSolo,
    /// Einseitiges Einblenden am Clipanfang.
    InSolo,
    /// Farbfläche eines zweiseitigen Dips (Schwarz/Weiß).
    Dip,
    /// Farbfläche eines einseitigen Dips am Clipende.
    DipOut,
    /// Farbfläche eines einseitigen Dips am Clipanfang.
    DipIn,
}

/// Rechteckmaske in Frame-Bruchteilen (x0, y0, x1, y1), jeweils 0–1.
pub type MaskFrac = [f64; 4];

/// Auswirkung des Übergangs auf einen Layer zum Fortschritt `p`.
/// Wird MULTIPLIKATIV/ADDITIV mit den Clip-Transformationen kombiniert.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionFx {
    /// Deckkraft-Faktor 0–1.
    pub opacity: f64,
    /// Verschiebung des Layer-Mittelpunkts in Bruchteilen der Framebreite/-höhe.
    pub offset_x: f64,
    pub offset_y: f64,
    /// Skalierungsfaktor (1 = unverändert).
    pub scale: f64,
    /// Sichtbarer Frame-Ausschnitt (Wipe); None = voll sichtbar.
    pub mask: Option<MaskFrac>,
}

impl TransitionFx {
    pub const IDENTITY: TransitionFx = TransitionFx {
        opacity: 1.0,
        offset_x: 0.0,
        offset_y: 0.0,
        scale: 1.0,
        mask: None,
    };

    /// Zwei Auswirkungen kombinieren (Clip mit Übergang an Anfang UND Ende).
    pub fn combine(&self, other: &TransitionFx) -> TransitionFx {
        TransitionFx {
            opacity: self.opacity * other.opacity,
            offset_x: self.offset_x + other.offset_x,
            offset_y: self.offset_y + other.offset_y,
            scale: self.scale * other.scale,
            mask: match (self.mask, other.mask) {
                (Some(a), Some(b)) => Some([
                    a[0].max(b[0]),
                    a[1].max(b[1]),
                    a[2].min(b[2]),
                    a[3].min(b[3]),
                ]),
                (m, None) => m,
                (None, m) => m,
            },
        }
    }
}

/// Sichtbarer Frame-Ausschnitt des EINGEHENDEN Bildes beim Wischen:
/// die Kante läuft in `direction`, `p` = Fortschritt 0–1.
fn wipe_mask(direction: TransitionDirection, p: f64) -> MaskFrac {
    match direction {
        TransitionDirection::Right => [0.0, 0.0, p, 1.0],
        TransitionDirection::Left => [1.0 - p, 0.0, 1.0, 1.0],
        TransitionDirection::Down => [0.0, 0.0, 1.0, p],
        TransitionDirection::Up => [0.0, 1.0 - p, 1.0, 1.0],
    }
}

/// Komplement der Wipe-Maske (für einseitiges Auswischen).
fn wipe_mask_inverse(direction: TransitionDirection, p: f64) -> MaskFrac {
    match direction {
        TransitionDirection::Right => [p, 0.0, 1.0, 1.0],
        TransitionDirection::Left => [0.0, 0.0, 1.0 - p, 1.0],
        TransitionDirection::Down => [0.0, p, 1.0, 1.0],
        TransitionDirection::Up => [0.0, 0.0, 1.0, 1.0 - p],
    }
}

/// DIE Formelquelle des visuellen Verhaltens: Auswirkung des Übergangs auf
/// einen Layer mit Rolle `role` beim Fortschritt `p` (0–1, geklemmt).
/// GPU-Vorschau und CPU-Export rufen exakt diese Funktion.
pub fn eval_video(
    kind: TransitionKind,
    direction: TransitionDirection,
    role: TransitionRole,
    p: f64,
) -> TransitionFx {
    let p = p.clamp(0.0, 1.0);
    let mut fx = TransitionFx::IDENTITY;
    match kind {
        TransitionKind::CrossDissolve => match role {
            // Echtes Lerp: B mit Alpha p ÜBER unverändertem A.
            TransitionRole::In | TransitionRole::InSolo => fx.opacity = p,
            TransitionRole::OutSolo => fx.opacity = 1.0 - p,
            _ => {}
        },
        TransitionKind::DipToBlack | TransitionKind::DipToWhite => match role {
            // Die Farbfläche trägt die Blende; der Clipwechsel passiert
            // unsichtbar hinter der voll deckenden Fläche bei p = 0,5.
            TransitionRole::In => fx.opacity = if p < 0.5 { 0.0 } else { 1.0 },
            TransitionRole::Dip => fx.opacity = 1.0 - (2.0 * p - 1.0).abs(),
            TransitionRole::DipOut => fx.opacity = p,
            TransitionRole::DipIn => fx.opacity = 1.0 - p,
            _ => {}
        },
        TransitionKind::Wipe => match role {
            TransitionRole::In | TransitionRole::InSolo => {
                fx.mask = Some(wipe_mask(direction, p));
            }
            TransitionRole::OutSolo => {
                fx.mask = Some(wipe_mask_inverse(direction, p));
            }
            _ => {}
        },
        TransitionKind::Push => {
            let (dx, dy) = direction.vector();
            match role {
                TransitionRole::Out | TransitionRole::OutSolo => {
                    fx.offset_x = dx * p;
                    fx.offset_y = dy * p;
                }
                TransitionRole::In | TransitionRole::InSolo => {
                    fx.offset_x = -dx * (1.0 - p);
                    fx.offset_y = -dy * (1.0 - p);
                }
                _ => {}
            }
        }
        TransitionKind::Zoom => match role {
            // In A hineinzoomen, aus B (groß startend) herauszoomen.
            TransitionRole::Out => fx.scale = 1.0 + p,
            TransitionRole::OutSolo => {
                fx.scale = 1.0 + p;
                fx.opacity = 1.0 - p;
            }
            TransitionRole::In | TransitionRole::InSolo => {
                fx.scale = 2.0 - p;
                fx.opacity = p;
            }
            _ => {}
        },
        // Audio-Übergänge haben keine visuelle Auswirkung.
        TransitionKind::ConstantPower | TransitionKind::ConstantGain => {}
    }
    fx
}

// ----------------------------------------------------------- Audio-Auswertung

/// Crossfade-Verstärkungsfaktor: `fade_in` = eingehende Seite, `p` =
/// Fortschritt 0–1. Konstante Leistung: sin/cos (sin² + cos² = 1);
/// konstante Verstärkung: linear (Summe = 1).
pub fn audio_gain(equal_power: bool, fade_in: bool, p: f64) -> f64 {
    let p = p.clamp(0.0, 1.0);
    if equal_power {
        let phase = p * std::f64::consts::FRAC_PI_2;
        if fade_in {
            phase.sin()
        } else {
            phase.cos()
        }
    } else if fade_in {
        p
    } else {
        1.0 - p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::animation::ClipFx;
    use crate::core::grade::ColorGrade;
    use crate::core::timeline::TrackKind;

    fn clip(start: f64, duration: f64, src_in: f64, src_duration: f64) -> TimelineClip {
        TimelineClip {
            id: new_id(),
            track_id: "t".into(),
            asset_id: "a".into(),
            name: "c".into(),
            kind: TrackKind::Video,
            start,
            duration,
            src_in,
            src_duration,
            link_id: None,
            enabled: true,
            gain_db: 0.0,
            fx: ClipFx::default(),
            grade: ColorGrade::default(),
            effects: Vec::new(),
            title: None,
            subtitle: None,
            speed: 1.0,
            reverse: false,
            freeze: false,
            markers: Vec::new(),
            nest_seq: None,
            multicam: None,
        }
    }

    #[test]
    fn max_duration_respects_handles_and_clip_lengths() {
        // A: 0–4 aus Quelle [1..5] von 10 s ⇒ Schwanz-Handle 5 s.
        // B: 4–8 aus Quelle [2..6] von 10 s ⇒ Kopf-Handle 2 s.
        let a = clip(0.0, 4.0, 1.0, 10.0);
        let b = clip(4.0, 4.0, 2.0, 10.0);
        // Zentriert: 2 × min(tail_a=5, dur_b=4, head_b=2, dur_a=4) = 4.
        assert!((max_duration(Some(&a), Some(&b), TransitionAlignment::Center) - 4.0).abs() < 1e-9);
        // Am Schnitt beginnend: min(tail_a, dur_b) = 4.
        assert!(
            (max_duration(Some(&a), Some(&b), TransitionAlignment::StartAtCut) - 4.0).abs() < 1e-9
        );
        // Am Schnitt endend: min(head_b, dur_a) = 2.
        assert!(
            (max_duration(Some(&a), Some(&b), TransitionAlignment::EndAtCut) - 2.0).abs() < 1e-9
        );
        // Ohne Kopf-Handle: zentriert unmöglich (0), StartAtCut bleibt.
        let b0 = clip(4.0, 4.0, 0.0, 10.0);
        assert_eq!(max_duration(Some(&a), Some(&b0), TransitionAlignment::Center), 0.0);
        assert!(max_duration(Some(&a), Some(&b0), TransitionAlignment::StartAtCut) > 0.0);
        // Bilder: unbegrenzte Handles, nur Cliplängen zählen.
        let img_a = clip(0.0, 4.0, 0.0, f64::INFINITY);
        let img_b = clip(4.0, 3.0, 0.0, f64::INFINITY);
        assert!(
            (max_duration(Some(&img_a), Some(&img_b), TransitionAlignment::Center) - 6.0).abs()
                < 1e-9
        );
        // Einseitig: Cliplänge.
        assert!((max_duration(Some(&a), None, TransitionAlignment::Center) - 4.0).abs() < 1e-9);
        assert!((max_duration(None, Some(&b), TransitionAlignment::Center) - 4.0).abs() < 1e-9);
    }

    #[test]
    fn window_follows_alignment() {
        let a = clip(0.0, 4.0, 0.0, 10.0);
        let b = clip(4.0, 4.0, 2.0, 10.0);
        let w = window(Some(&a), Some(&b), TransitionAlignment::Center, 2.0).unwrap();
        assert_eq!(w, (3.0, 5.0));
        let w = window(Some(&a), Some(&b), TransitionAlignment::StartAtCut, 2.0).unwrap();
        assert_eq!(w, (4.0, 6.0));
        let w = window(Some(&a), Some(&b), TransitionAlignment::EndAtCut, 2.0).unwrap();
        assert_eq!(w, (2.0, 4.0));
        // Einseitig: am Cliprand.
        let w = window(Some(&a), None, TransitionAlignment::Center, 1.0).unwrap();
        assert_eq!(w, (3.0, 4.0));
        let w = window(None, Some(&b), TransitionAlignment::Center, 1.0).unwrap();
        assert_eq!(w, (4.0, 5.0));
    }

    #[test]
    fn dissolve_is_true_lerp() {
        let f = |role, p| eval_video(TransitionKind::CrossDissolve, TransitionDirection::Right, role, p);
        assert_eq!(f(TransitionRole::Out, 0.5), TransitionFx::IDENTITY);
        assert_eq!(f(TransitionRole::In, 0.0).opacity, 0.0);
        assert_eq!(f(TransitionRole::In, 0.5).opacity, 0.5);
        assert_eq!(f(TransitionRole::In, 1.0).opacity, 1.0);
        assert_eq!(f(TransitionRole::OutSolo, 0.25).opacity, 0.75);
    }

    #[test]
    fn dip_peaks_at_center_and_hides_the_switch() {
        let f = |role, p| eval_video(TransitionKind::DipToBlack, TransitionDirection::Right, role, p);
        assert_eq!(f(TransitionRole::Dip, 0.0).opacity, 0.0);
        assert_eq!(f(TransitionRole::Dip, 0.5).opacity, 1.0);
        assert_eq!(f(TransitionRole::Dip, 1.0).opacity, 0.0);
        // Clipwechsel exakt unter der vollen Deckung.
        assert_eq!(f(TransitionRole::In, 0.49).opacity, 0.0);
        assert_eq!(f(TransitionRole::In, 0.51).opacity, 1.0);
        // Einseitig: linear bis zur Kante.
        assert_eq!(f(TransitionRole::DipOut, 1.0).opacity, 1.0);
        assert_eq!(f(TransitionRole::DipIn, 0.0).opacity, 1.0);
    }

    #[test]
    fn wipe_mask_moves_with_direction() {
        let f = |dir, role, p| eval_video(TransitionKind::Wipe, dir, role, p);
        let m = f(TransitionDirection::Right, TransitionRole::In, 0.25).mask.unwrap();
        assert_eq!(m, [0.0, 0.0, 0.25, 1.0]);
        let m = f(TransitionDirection::Left, TransitionRole::In, 0.25).mask.unwrap();
        assert_eq!(m, [0.75, 0.0, 1.0, 1.0]);
        let m = f(TransitionDirection::Down, TransitionRole::In, 0.5).mask.unwrap();
        assert_eq!(m, [0.0, 0.0, 1.0, 0.5]);
        // Einseitiges Auswischen: Komplement.
        let m = f(TransitionDirection::Right, TransitionRole::OutSolo, 0.25).mask.unwrap();
        assert_eq!(m, [0.25, 0.0, 1.0, 1.0]);
        // Ausgehender Clip eines zweiseitigen Wipes bleibt unmaskiert.
        assert!(f(TransitionDirection::Right, TransitionRole::Out, 0.5).mask.is_none());
    }

    #[test]
    fn push_offsets_tile_the_frame() {
        let f = |role, p| {
            eval_video(TransitionKind::Push, TransitionDirection::Left, role, p)
        };
        for p in [0.0, 0.25, 0.5, 1.0] {
            let out = f(TransitionRole::Out, p);
            let inn = f(TransitionRole::In, p);
            // A und B liegen exakt eine Framebreite auseinander.
            assert!(((inn.offset_x - out.offset_x) - 1.0).abs() < 1e-9, "p={p}");
        }
        assert_eq!(f(TransitionRole::Out, 1.0).offset_x, -1.0);
        assert_eq!(f(TransitionRole::In, 1.0).offset_x, 0.0);
    }

    #[test]
    fn audio_gain_curves_are_complementary() {
        // Konstante Leistung: Quadratsumme = 1.
        for p in [0.0, 0.3, 0.5, 0.8, 1.0] {
            let i = audio_gain(true, true, p);
            let o = audio_gain(true, false, p);
            assert!((i * i + o * o - 1.0).abs() < 1e-9, "p={p}");
        }
        assert!((audio_gain(true, true, 0.5) - audio_gain(true, false, 0.5)).abs() < 1e-9);
        // Konstante Verstärkung: Summe = 1.
        for p in [0.0, 0.25, 0.5, 1.0] {
            assert!((audio_gain(false, true, p) + audio_gain(false, false, p) - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn fx_combine_merges_masks_and_factors() {
        let a = TransitionFx {
            opacity: 0.5,
            offset_x: 0.25,
            offset_y: 0.0,
            scale: 2.0,
            mask: Some([0.0, 0.0, 0.5, 1.0]),
        };
        let b = TransitionFx {
            opacity: 0.5,
            offset_x: 0.25,
            offset_y: 0.0,
            scale: 0.5,
            mask: Some([0.25, 0.0, 1.0, 1.0]),
        };
        let c = a.combine(&b);
        assert_eq!(c.opacity, 0.25);
        assert_eq!(c.offset_x, 0.5);
        assert_eq!(c.scale, 1.0);
        assert_eq!(c.mask, Some([0.25, 0.0, 0.5, 1.0]));
    }
}
