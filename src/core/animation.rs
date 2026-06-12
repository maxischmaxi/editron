//! Keyframe-Animation: animierbare Clip-Parameter (Position, Skalierung,
//! Rotation, Deckkraft, Lautstärke) mit Interpolation. Keyframe-Zeiten sind
//! in MEDIENZEIT verankert (Sekunden in der Quelldatei) — Keyframes kleben
//! damit am Material: Verschieben des Clips ändert nichts, Trimmen des
//! Kopfes schiebt die Animation relativ zum Clipanfang (Premiere-Semantik).

use serde::{Deserialize, Serialize};

/// Zwei Keyframes näher beieinander gelten als derselbe (halber Frame @25).
pub const KF_TIME_EPS: f64 = 0.02;

/// Interpolation zum NÄCHSTEN Keyframe.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Interp {
    #[default]
    Linear,
    /// Wert bleibt bis zum nächsten Keyframe stehen.
    Hold,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl Interp {
    pub fn label(&self) -> &'static str {
        match self {
            Interp::Linear => "Linear",
            Interp::Hold => "Halten",
            Interp::EaseIn => "Ease In",
            Interp::EaseOut => "Ease Out",
            Interp::EaseInOut => "Ease In/Out",
        }
    }

    pub const ALL: [Interp; 5] = [
        Interp::Linear,
        Interp::Hold,
        Interp::EaseIn,
        Interp::EaseOut,
        Interp::EaseInOut,
    ];

    /// Easing-Funktion auf den normierten Fortschritt u ∈ [0, 1].
    fn apply(&self, u: f64) -> f64 {
        match self {
            Interp::Linear => u,
            Interp::Hold => 0.0,
            Interp::EaseIn => u * u,
            Interp::EaseOut => 1.0 - (1.0 - u) * (1.0 - u),
            Interp::EaseInOut => u * u * (3.0 - 2.0 * u),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Keyframe {
    /// Medienzeit (Sekunden in der Quelle).
    pub t: f64,
    pub value: f64,
    #[serde(default)]
    pub interp: Interp,
}

/// Parameter mit statischem Wert oder Keyframe-Kurve.
/// `keyframes` leer ⇒ statisch (`value` gilt); sonst ist die Kurve maßgeblich
/// (Stopwatch an). Keyframes sind stets nach `t` sortiert.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimatedParam {
    pub value: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<Keyframe>,
}

impl AnimatedParam {
    pub fn fixed(value: f64) -> AnimatedParam {
        AnimatedParam {
            value,
            keyframes: Vec::new(),
        }
    }

    pub fn is_animated(&self) -> bool {
        !self.keyframes.is_empty()
    }

    /// Kurvenwert zur Medienzeit `t` (reine Interpolations-Mathematik):
    /// vor dem ersten/nach dem letzten Keyframe wird der Randwert gehalten.
    pub fn eval(&self, t: f64) -> f64 {
        let keys = &self.keyframes;
        if keys.is_empty() {
            return self.value;
        }
        if t <= keys[0].t {
            return keys[0].value;
        }
        if t >= keys[keys.len() - 1].t {
            return keys[keys.len() - 1].value;
        }
        // Segment [i, i+1] mit keys[i].t <= t < keys[i+1].t suchen.
        let i = match keys.binary_search_by(|k| k.t.partial_cmp(&t).unwrap()) {
            Ok(i) => return keys[i].value,
            Err(i) => i - 1,
        };
        let a = &keys[i];
        let b = &keys[i + 1];
        let span = b.t - a.t;
        if span <= 0.0 {
            return b.value;
        }
        let u = a.interp.apply(((t - a.t) / span).clamp(0.0, 1.0));
        a.value + (b.value - a.value) * u
    }

    pub fn key_index_at(&self, t: f64) -> Option<usize> {
        self.keyframes
            .iter()
            .position(|k| (k.t - t).abs() < KF_TIME_EPS)
    }

    /// Keyframe bei `t` setzen bzw. aktualisieren; hält die Sortierung.
    pub fn upsert_key(&mut self, t: f64, value: f64) {
        if let Some(i) = self.key_index_at(t) {
            self.keyframes[i].value = value;
            return;
        }
        let interp = self
            .keyframes
            .iter()
            .rev()
            .find(|k| k.t < t)
            .map(|k| k.interp)
            .unwrap_or_default();
        let pos = self
            .keyframes
            .iter()
            .position(|k| k.t > t)
            .unwrap_or(self.keyframes.len());
        self.keyframes.insert(pos, Keyframe { t, value, interp });
    }

    pub fn remove_key_at(&mut self, t: f64) -> bool {
        match self.key_index_at(t) {
            Some(i) => {
                self.keyframes.remove(i);
                true
            }
            None => false,
        }
    }

    /// Stopwatch an: erster Keyframe bei `t` mit dem aktuellen Wert.
    pub fn enable_animation(&mut self, t: f64) {
        if !self.is_animated() {
            let v = self.value;
            self.upsert_key(t, v);
        }
    }

    /// Stopwatch aus: Kurve verwerfen, aktuellen Wert einfrieren.
    pub fn clear_animation(&mut self, freeze_at: f64) {
        self.value = self.eval(freeze_at);
        self.keyframes.clear();
    }

    /// Wert anwenden: animiert ⇒ Keyframe bei `t`, sonst statisch.
    pub fn set_at(&mut self, t: f64, value: f64) {
        if self.is_animated() {
            self.upsert_key(t, value);
        } else {
            self.value = value;
        }
    }

    pub fn prev_key_time(&self, t: f64) -> Option<f64> {
        self.keyframes
            .iter()
            .rev()
            .find(|k| k.t < t - KF_TIME_EPS)
            .map(|k| k.t)
    }

    pub fn next_key_time(&self, t: f64) -> Option<f64> {
        self.keyframes
            .iter()
            .find(|k| k.t > t + KF_TIME_EPS)
            .map(|k| k.t)
    }

    /// Kurve ersetzen (Keyframe-Editor: Verschieben/Löschen ganzer
    /// Auswahlen); sortiert und entdoppelt defensiv.
    pub fn replace_keys(&mut self, mut keys: Vec<Keyframe>) {
        keys.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap());
        keys.dedup_by(|a, b| (a.t - b.t).abs() < KF_TIME_EPS);
        self.keyframes = keys;
    }
}

// ----------------------------------------------------------------- ClipFx

/// Alle animierbaren Parameter eines Clips.
///
/// Einheiten (auflösungsunabhängig, gelten für Vorschau UND Export):
/// - Position X/Y: Offset vom Framezentrum in % der Framebreite/-höhe
/// - Skalierung X/Y: % der „Contain-Fit“-Größe (100 = eingepasst)
/// - Rotation: Grad im Uhrzeigersinn
/// - Deckkraft: %
/// - Lautstärke: dB-Offset zusätzlich zur Clip-Verstärkung
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipFx {
    #[serde(default = "zero_param")]
    pub pos_x: AnimatedParam,
    #[serde(default = "zero_param")]
    pub pos_y: AnimatedParam,
    #[serde(default = "hundred_param")]
    pub scale_x: AnimatedParam,
    /// Nur wirksam, wenn `uniform_scale` aus ist.
    #[serde(default = "hundred_param")]
    pub scale_y: AnimatedParam,
    #[serde(default = "default_true")]
    pub uniform_scale: bool,
    #[serde(default = "zero_param")]
    pub rotation: AnimatedParam,
    #[serde(default = "hundred_param")]
    pub opacity: AnimatedParam,
    #[serde(default = "zero_param")]
    pub volume_db: AnimatedParam,
}

fn zero_param() -> AnimatedParam {
    AnimatedParam::fixed(0.0)
}

fn hundred_param() -> AnimatedParam {
    AnimatedParam::fixed(100.0)
}

fn default_true() -> bool {
    true
}

impl Default for ClipFx {
    fn default() -> Self {
        ClipFx {
            pos_x: zero_param(),
            pos_y: zero_param(),
            scale_x: hundred_param(),
            scale_y: hundred_param(),
            uniform_scale: true,
            rotation: zero_param(),
            opacity: hundred_param(),
            volume_db: zero_param(),
        }
    }
}

impl ClipFx {
    /// Für `skip_serializing_if`: unveränderte Clips bleiben in der
    /// Projektdatei schlank.
    pub fn is_default(&self) -> bool {
        *self == ClipFx::default()
    }

    pub fn param(&self, id: ParamId) -> &AnimatedParam {
        match id {
            ParamId::PosX => &self.pos_x,
            ParamId::PosY => &self.pos_y,
            ParamId::ScaleX => &self.scale_x,
            ParamId::ScaleY => &self.scale_y,
            ParamId::Rotation => &self.rotation,
            ParamId::Opacity => &self.opacity,
            ParamId::VolumeDb => &self.volume_db,
        }
    }

    pub fn param_mut(&mut self, id: ParamId) -> &mut AnimatedParam {
        match id {
            ParamId::PosX => &mut self.pos_x,
            ParamId::PosY => &mut self.pos_y,
            ParamId::ScaleX => &mut self.scale_x,
            ParamId::ScaleY => &mut self.scale_y,
            ParamId::Rotation => &mut self.rotation,
            ParamId::Opacity => &mut self.opacity,
            ParamId::VolumeDb => &mut self.volume_db,
        }
    }

    /// Hat irgendein Parameter Keyframes? (Timeline-Badge, UI-Hinweise.)
    pub fn any_animated(&self) -> bool {
        self.pos_x.is_animated()
            || self.pos_y.is_animated()
            || self.scale_x.is_animated()
            || self.scale_y.is_animated()
            || self.rotation.is_animated()
            || self.opacity.is_animated()
            || self.volume_db.is_animated()
    }

    /// Hat irgendein visueller Parameter Keyframes oder Nicht-Standardwerte?
    /// (Schnellpfad-Check für Player/Export: identisch zur alten Darstellung.)
    pub fn is_visual_identity(&self) -> bool {
        let d = ClipFx::default();
        self.pos_x == d.pos_x
            && self.pos_y == d.pos_y
            && self.scale_x == d.scale_x
            && (self.uniform_scale || self.scale_y == d.scale_y)
            && self.rotation == d.rotation
            && self.opacity == d.opacity
    }
}

/// Identität eines animierbaren Parameters (UI, Commands, Kontextmenüs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ParamId {
    PosX,
    PosY,
    ScaleX,
    ScaleY,
    Rotation,
    Opacity,
    VolumeDb,
}

impl ParamId {
    pub const ALL: [ParamId; 7] = [
        ParamId::PosX,
        ParamId::PosY,
        ParamId::ScaleX,
        ParamId::ScaleY,
        ParamId::Rotation,
        ParamId::Opacity,
        ParamId::VolumeDb,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            ParamId::PosX => "Position X",
            ParamId::PosY => "Position Y",
            ParamId::ScaleX => "Skalierung",
            ParamId::ScaleY => "Skalierung Y",
            ParamId::Rotation => "Rotation",
            ParamId::Opacity => "Deckkraft",
            ParamId::VolumeDb => "Lautstärke",
        }
    }

    pub fn unit(&self) -> &'static str {
        match self {
            ParamId::PosX | ParamId::PosY => "%",
            ParamId::ScaleX | ParamId::ScaleY => "%",
            ParamId::Rotation => "°",
            ParamId::Opacity => "%",
            ParamId::VolumeDb => "dB",
        }
    }

    pub fn range(&self) -> (f64, f64) {
        match self {
            ParamId::PosX | ParamId::PosY => (-500.0, 500.0),
            ParamId::ScaleX | ParamId::ScaleY => (0.0, 1000.0),
            ParamId::Rotation => (-3600.0, 3600.0),
            ParamId::Opacity => (0.0, 100.0),
            ParamId::VolumeDb => (-60.0, 12.0),
        }
    }

    /// Wertänderung pro gezogenem Pixel (Wert-Scrubbing).
    pub fn drag_step(&self) -> f64 {
        match self {
            ParamId::PosX | ParamId::PosY => 0.25,
            ParamId::ScaleX | ParamId::ScaleY => 0.5,
            ParamId::Rotation => 0.5,
            ParamId::Opacity => 0.5,
            ParamId::VolumeDb => 0.1,
        }
    }

    pub fn default_value(&self) -> f64 {
        match self {
            ParamId::ScaleX | ParamId::ScaleY | ParamId::Opacity => 100.0,
            _ => 0.0,
        }
    }

    /// Nachkommastellen in der Anzeige.
    pub fn decimals(&self) -> usize {
        1
    }
}

/// Vereinheitlichte Identität eines animierbaren Parameters: eingebaute
/// Bewegungs-/Deckkraft-/Lautstärke-Parameter ODER ein Parameter eines
/// angewendeten Effekts (Effekt-Instanz-ID + Spec-Index). Grundlage der
/// generischen Keyframe-Operationen im `TimelineStore` (`kf_*`) und im
/// Panel Effekteinstellungen.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ParamRef {
    Builtin(ParamId),
    Effect { fx_id: String, index: usize },
}

impl From<ParamId> for ParamRef {
    fn from(id: ParamId) -> ParamRef {
        ParamRef::Builtin(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_param_evaluates_to_value() {
        let p = AnimatedParam::fixed(42.0);
        assert_eq!(p.eval(0.0), 42.0);
        assert_eq!(p.eval(100.0), 42.0);
        assert!(!p.is_animated());
    }

    #[test]
    fn linear_interpolation_and_edge_hold() {
        let mut p = AnimatedParam::fixed(0.0);
        p.upsert_key(1.0, 0.0);
        p.upsert_key(3.0, 100.0);
        assert_eq!(p.eval(0.0), 0.0); // vor dem ersten Key
        assert_eq!(p.eval(1.0), 0.0);
        assert!((p.eval(2.0) - 50.0).abs() < 1e-9);
        assert_eq!(p.eval(3.0), 100.0);
        assert_eq!(p.eval(10.0), 100.0); // nach dem letzten Key
    }

    #[test]
    fn hold_keeps_value_until_next_key() {
        let mut p = AnimatedParam::fixed(0.0);
        p.upsert_key(0.0, 10.0);
        p.upsert_key(2.0, 20.0);
        p.keyframes[0].interp = Interp::Hold;
        assert_eq!(p.eval(1.999), 10.0);
        assert_eq!(p.eval(2.0), 20.0);
    }

    #[test]
    fn easing_is_monotonic_and_hits_endpoints() {
        for interp in [Interp::EaseIn, Interp::EaseOut, Interp::EaseInOut] {
            let mut p = AnimatedParam::fixed(0.0);
            p.upsert_key(0.0, 0.0);
            p.upsert_key(1.0, 100.0);
            p.keyframes[0].interp = interp;
            assert_eq!(p.eval(0.0), 0.0);
            assert_eq!(p.eval(1.0), 100.0);
            let mut last = -1.0;
            for i in 0..=20 {
                let v = p.eval(i as f64 / 20.0);
                assert!(v >= last - 1e-9, "{interp:?} nicht monoton");
                last = v;
            }
        }
    }

    #[test]
    fn upsert_updates_existing_key_and_keeps_sorted() {
        let mut p = AnimatedParam::fixed(0.0);
        p.upsert_key(2.0, 5.0);
        p.upsert_key(1.0, 1.0);
        p.upsert_key(3.0, 9.0);
        assert_eq!(p.keyframes.len(), 3);
        assert!(p.keyframes.windows(2).all(|w| w[0].t < w[1].t));
        p.upsert_key(2.005, 7.0); // innerhalb EPS → Update
        assert_eq!(p.keyframes.len(), 3);
        assert_eq!(p.keyframes[1].value, 7.0);
    }

    #[test]
    fn stopwatch_toggle_freezes_current_value() {
        let mut p = AnimatedParam::fixed(0.0);
        p.enable_animation(1.0);
        p.upsert_key(3.0, 100.0);
        assert!(p.is_animated());
        p.clear_animation(2.0);
        assert!(!p.is_animated());
        assert!((p.value - 50.0).abs() < 1e-9);
    }

    #[test]
    fn clip_fx_default_roundtrip_is_lean() {
        let fx = ClipFx::default();
        assert!(fx.is_default());
        assert!(fx.is_visual_identity());
        let mut fx2 = fx.clone();
        fx2.pos_x.set_at(0.0, 10.0);
        assert!(!fx2.is_default());
        assert!(!fx2.is_visual_identity());
        let json = serde_json::to_string(&fx2).unwrap();
        let back: ClipFx = serde_json::from_str(&json).unwrap();
        assert_eq!(fx2, back);
    }

    #[test]
    fn prev_next_key_navigation() {
        let mut p = AnimatedParam::fixed(0.0);
        p.upsert_key(1.0, 0.0);
        p.upsert_key(2.0, 1.0);
        p.upsert_key(3.0, 2.0);
        assert_eq!(p.prev_key_time(2.5), Some(2.0));
        assert_eq!(p.next_key_time(2.5), Some(3.0));
        assert_eq!(p.prev_key_time(1.0), None);
        assert_eq!(p.next_key_time(3.0), None);
        assert_eq!(p.next_key_time(2.0), Some(3.0));
    }
}
