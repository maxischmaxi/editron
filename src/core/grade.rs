//! Farbkorrektur (Lumetri-Pendant): pro Clip ein [`ColorGrade`] mit
//! Basiskorrektur, Kreativ-Look, Lift/Gamma/Gain-Farbrädern und Vignette.
//!
//! Die Pixel-Mathematik existiert ZWEIMAL mit identischen Formeln:
//! GPU-Fragment-Shader für den Programmmonitor (`ui/grade_shader.rs`) und
//! der CPU-Pfad hier (Export, Scopes). [`GradeParams`] ist die gemeinsame
//! Vorberechnung — Looks/Wheels/Slider werden einmal pro Frame auf wenige
//! per-Pixel-Uniforms reduziert, per Pixel bleibt branchfreie Arithmetik.
//!
//! Pipeline (Gamma-Domäne sRGB-approximiert mit γ = 2,2):
//! 1. Weißabgleich + Belichtung in linearem Licht (gefaltet zu `wb_gain`)
//! 2. Tonwerte: Schwarz/Schatten/Lichter/Weiß als luma-gewichtete Offsets
//! 3. Kontrast: lineare Steigung um den Pivot 0,5
//! 4. Lift/Gamma/Gain pro Kanal (Farbräder + Look-Anteile)
//! 4.5 Tonwertkurven: Luma-Master- + R/G/B-Kurven (kombinierte 1D-LUT)
//! 5. Sättigung/Dynamik (luma-erhaltend)
//! 6. Vignette über die normierten Clip-Koordinaten

use crate::core::lut::{LutCache, LutStack, OwnedLutStack};
use serde::{Deserialize, Serialize};

// -------------------------------------------------------------- Datenmodell

/// Referenz eines Clips auf eine externe `.cube`-LUT-Datei (Input- oder
/// Look-Slot). Im Projekt wird NUR Pfad + Stärke (+ Anzeigename) gespeichert;
/// die LUT-Daten liegen extern und werden über den [`crate::core::lut::LutCache`]
/// aufgelöst (fehlende Datei ⇒ Offline-Hinweis im Farbe-Panel).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LutSlot {
    /// Pfad zur `.cube`-Datei.
    pub path: String,
    /// Anzeigename (Dateiname) für die UI; rein kosmetisch.
    #[serde(default)]
    pub name: String,
    /// Wirkstärke 0…100 (100 = voll).
    #[serde(default = "default_hundred")]
    pub strength: f64,
}

impl LutSlot {
    /// Wirkt der Slot (Pfad gesetzt UND Stärke > 0)?
    pub fn is_active(&self) -> bool {
        self.strength > 0.0 && !self.path.is_empty()
    }

    /// Stärke auf 0…1 normiert (Shader/CPU-Mischfaktor).
    pub fn strength01(&self) -> f32 {
        (self.strength / 100.0).clamp(0.0, 1.0) as f32
    }
}

/// Farb-Offset eines Farbrads: (x, y) im Einheitskreis + Luma-Regler −1…1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WheelValue {
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default)]
    pub luma: f64,
}

impl WheelValue {
    pub fn is_zero(&self) -> bool {
        self.x == 0.0 && self.y == 0.0 && self.luma == 0.0
    }
}

/// Ein Stützpunkt einer Tonwertkurve: Eingang/Ausgang je 0…1.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurvePoint {
    pub x: f64,
    pub y: f64,
}

/// Eine Tonwertkurve als aufsteigend in x sortierte Stützpunktfolge.
/// Leer ODER alle Punkte auf der Diagonale ⇒ Identität. Die Auswertung
/// nutzt einen monotonen kubischen Hermite-Spline (Fritsch–Carlson, kein
/// Überschwingen), siehe [`Curve::eval`]. Serialisiert als reines Array.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Curve {
    pub points: Vec<CurvePoint>,
}

impl Default for Curve {
    fn default() -> Self {
        Curve::identity()
    }
}

impl Curve {
    /// Identitätskurve: Eckpunkte (0,0) und (1,1).
    pub fn identity() -> Self {
        Curve {
            points: vec![
                CurvePoint { x: 0.0, y: 0.0 },
                CurvePoint { x: 1.0, y: 1.0 },
            ],
        }
    }

    /// Wirkt die Kurve wie die Identität (alle Stützpunkte auf y = x)?
    /// Toleranter Check (für Neutral-Erkennung/Schlankhalten der Datei) —
    /// nicht zu verwechseln mit dem strikten `PartialEq`-Default.
    pub fn is_identity(&self) -> bool {
        self.points.iter().all(|p| (p.y - p.x).abs() < 1e-9)
    }

    /// Auswertung an `x` ∈ 0…1. Außerhalb des Stützbereichs wird der
    /// jeweilige Endpunktwert gehalten. Monotoner kubischer Hermite-Spline.
    pub fn eval(&self, x: f64) -> f64 {
        self.eval_prepared(&self.prepare_tangents(), x)
    }

    /// Tangenten einmal vorberechnen (leer bei < 2 Punkten). Der LUT-Aufbau in
    /// [`precompute`] ruft das je Kurve EINMAL und sampelt dann mit
    /// [`Curve::eval_prepared`] — sonst würde jeder der 256 Stützstellen-Aufrufe
    /// die Tangenten (zwei `Vec`s) neu allozieren.
    fn prepare_tangents(&self) -> Vec<f64> {
        if self.points.len() >= 2 {
            monotone_tangents(&self.points)
        } else {
            Vec::new()
        }
    }

    /// Auswertung mit vorberechneten Tangenten (`prepare_tangents`, Länge = Zahl
    /// der Stützpunkte). Formelgleich zu [`Curve::eval`].
    fn eval_prepared(&self, tangents: &[f64], x: f64) -> f64 {
        let p = &self.points;
        let n = p.len();
        if n == 0 {
            return x.clamp(0.0, 1.0);
        }
        if n == 1 {
            return p[0].y.clamp(0.0, 1.0);
        }
        if x <= p[0].x {
            return p[0].y.clamp(0.0, 1.0);
        }
        if x >= p[n - 1].x {
            return p[n - 1].y.clamp(0.0, 1.0);
        }
        let m = tangents;
        let mut k = 0;
        while k + 1 < n - 1 && x > p[k + 1].x {
            k += 1;
        }
        let h = p[k + 1].x - p[k].x;
        if h <= 0.0 {
            return p[k].y.clamp(0.0, 1.0);
        }
        let t = (x - p[k].x) / h;
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        let y = h00 * p[k].y + h10 * h * m[k] + h01 * p[k + 1].y + h11 * h * m[k + 1];
        y.clamp(0.0, 1.0)
    }
}

/// Tangenten für den monotonen kubischen Hermite-Spline (Fritsch–Carlson):
/// zuerst die Standard-Tangenten (Mittel der Sekanten), dann auf das
/// Monotonie-Kreissegment beschränkt, damit zwischen den Stützpunkten kein
/// Über-/Unterschwingen entsteht.
fn monotone_tangents(p: &[CurvePoint]) -> Vec<f64> {
    let n = p.len();
    let mut d = vec![0.0f64; n - 1];
    for k in 0..n - 1 {
        let dx = p[k + 1].x - p[k].x;
        d[k] = if dx > 0.0 { (p[k + 1].y - p[k].y) / dx } else { 0.0 };
    }
    let mut m = vec![0.0f64; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for k in 1..n - 1 {
        m[k] = (d[k - 1] + d[k]) / 2.0;
    }
    for k in 0..n - 1 {
        if d[k] == 0.0 {
            m[k] = 0.0;
            m[k + 1] = 0.0;
        } else {
            let alpha = m[k] / d[k];
            let beta = m[k + 1] / d[k];
            let s = alpha * alpha + beta * beta;
            if s > 9.0 {
                let tau = 3.0 / s.sqrt();
                m[k] = tau * alpha * d[k];
                m[k + 1] = tau * beta * d[k];
            }
        }
    }
    m
}

/// Tonwertkurven eines Clips: Luma-Master-Kurve plus separate Kurven je
/// RGB-Kanal. Anwendung: pro Kanal `kanal(master(wert))` (Master zuerst,
/// in [`precompute`] zu einer kombinierten 1D-LUT je Kanal gefaltet).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GradeCurves {
    #[serde(default, skip_serializing_if = "Curve::is_identity")]
    pub luma: Curve,
    #[serde(default, skip_serializing_if = "Curve::is_identity")]
    pub red: Curve,
    #[serde(default, skip_serializing_if = "Curve::is_identity")]
    pub green: Curve,
    #[serde(default, skip_serializing_if = "Curve::is_identity")]
    pub blue: Curve,
}

impl GradeCurves {
    /// Alle vier Kurven neutral?
    pub fn is_identity(&self) -> bool {
        self.luma.is_identity()
            && self.red.is_identity()
            && self.green.is_identity()
            && self.blue.is_identity()
    }

    /// Kurve nach Kanal-Index (0 = Luma, 1 = Rot, 2 = Grün, 3 = Blau).
    pub fn channel(&self, ch: usize) -> &Curve {
        match ch {
            0 => &self.luma,
            1 => &self.red,
            2 => &self.green,
            _ => &self.blue,
        }
    }

    pub fn channel_mut(&mut self, ch: usize) -> &mut Curve {
        match ch {
            0 => &mut self.luma,
            1 => &mut self.red,
            2 => &mut self.green,
            _ => &mut self.blue,
        }
    }
}

/// Kreativ-Look (vordefinierte Korrektur, per Intensität zugemischt).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GradeLook {
    #[default]
    Neutral,
    FilmWarm,
    FilmCold,
    Mono,
    BleachBypass,
    TealOrange,
    Vintage,
}

impl GradeLook {
    pub const ALL: [GradeLook; 7] = [
        GradeLook::Neutral,
        GradeLook::FilmWarm,
        GradeLook::FilmCold,
        GradeLook::Mono,
        GradeLook::BleachBypass,
        GradeLook::TealOrange,
        GradeLook::Vintage,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            GradeLook::Neutral => "Neutral",
            GradeLook::FilmWarm => "Film warm",
            GradeLook::FilmCold => "Film kalt",
            GradeLook::Mono => "Schwarzweiß",
            GradeLook::BleachBypass => "Bleach Bypass",
            GradeLook::TealOrange => "Teal & Orange",
            GradeLook::Vintage => "Vintage",
        }
    }
}

/// Alle Farbkorrektur-Parameter eines Clips (statisch, Premiere-Semantik:
/// die Korrektur gilt für den ganzen Clip).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorGrade {
    /// Bypass-Schalter (fx-Toggle im Panel) — Werte bleiben erhalten.
    #[serde(default = "default_true")]
    pub enabled: bool,
    // ---- Basiskorrektur ----
    /// −100 (kühl) … 100 (warm)
    #[serde(default)]
    pub temperature: f64,
    /// −100 (grün) … 100 (magenta)
    #[serde(default)]
    pub tint: f64,
    /// Blendenstufen −5 … 5
    #[serde(default)]
    pub exposure: f64,
    /// −100 … 100
    #[serde(default)]
    pub contrast: f64,
    /// −100 … 100
    #[serde(default)]
    pub highlights: f64,
    /// −100 … 100
    #[serde(default)]
    pub shadows: f64,
    /// −100 … 100
    #[serde(default)]
    pub whites: f64,
    /// −100 … 100
    #[serde(default)]
    pub blacks: f64,
    /// 0 … 200, 100 = neutral
    #[serde(default = "default_hundred")]
    pub saturation: f64,
    /// Dynamik: −100 … 100 (schützt bereits gesättigte Farben)
    #[serde(default)]
    pub vibrance: f64,
    // ---- Kreativ ----
    #[serde(default)]
    pub look: GradeLook,
    /// 0 … 100
    #[serde(default = "default_hundred")]
    pub look_intensity: f64,
    /// Verblasster Film 0 … 100
    #[serde(default)]
    pub faded_film: f64,
    // ---- Farbräder ----
    #[serde(default)]
    pub lift: WheelValue,
    #[serde(default)]
    pub gamma: WheelValue,
    #[serde(default)]
    pub gain: WheelValue,
    // ---- Vignette ----
    /// −100 (aufhellen) … 100 (abdunkeln)
    #[serde(default)]
    pub vignette_amount: f64,
    /// Mittelpunkt (Größe des unbeeinflussten Bereichs) 0 … 100
    #[serde(default = "default_fifty")]
    pub vignette_midpoint: f64,
    /// −100 (rechteckig) … 100 (kreisrund)
    #[serde(default)]
    pub vignette_roundness: f64,
    /// Weiche Kante 0 … 100
    #[serde(default = "default_fifty")]
    pub vignette_feather: f64,
    // ---- Tonwertkurven ----
    /// Luma-Master- + R/G/B-Kanalkurven (monotone Splines, je 0…1).
    #[serde(default, skip_serializing_if = "GradeCurves::is_identity")]
    pub curves: GradeCurves,
    // ---- 3D-LUTs ----
    /// Input-LUT (`.cube`): technische Normalisierung am PIPELINE-ANFANG
    /// (vor Weißabgleich). None = kein LUT.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_lut: Option<LutSlot>,
    /// Look-LUT (`.cube`): kreativer Schluss-Stempel am PIPELINE-ENDE
    /// (nach Lift/Gamma/Gain, Kurven, Sättigung, Vignette). None = kein LUT.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look_lut: Option<LutSlot>,
}

fn default_true() -> bool {
    true
}

fn default_hundred() -> f64 {
    100.0
}

fn default_fifty() -> f64 {
    50.0
}

impl Default for ColorGrade {
    fn default() -> Self {
        ColorGrade {
            enabled: true,
            temperature: 0.0,
            tint: 0.0,
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            saturation: 100.0,
            vibrance: 0.0,
            look: GradeLook::Neutral,
            look_intensity: 100.0,
            faded_film: 0.0,
            lift: WheelValue::default(),
            gamma: WheelValue::default(),
            gain: WheelValue::default(),
            vignette_amount: 0.0,
            vignette_midpoint: 50.0,
            vignette_roundness: 0.0,
            vignette_feather: 50.0,
            curves: GradeCurves::default(),
            input_lut: None,
            look_lut: None,
        }
    }
}

impl ColorGrade {
    /// Für `skip_serializing_if`: ungegradete Clips bleiben schlank.
    pub fn is_default(&self) -> bool {
        *self == ColorGrade::default()
    }

    /// Verändert die Korrektur das Bild? (Schnellpfad Player/Export.)
    pub fn is_active(&self) -> bool {
        self.enabled && !self.is_neutral()
    }

    /// Alle Werte neutral (unabhängig vom Bypass-Schalter)?
    pub fn is_neutral(&self) -> bool {
        let d = ColorGrade::default();
        self.temperature == d.temperature
            && self.tint == d.tint
            && self.exposure == d.exposure
            && self.contrast == d.contrast
            && self.highlights == d.highlights
            && self.shadows == d.shadows
            && self.whites == d.whites
            && self.blacks == d.blacks
            && self.saturation == d.saturation
            && self.vibrance == d.vibrance
            && (self.look == GradeLook::Neutral || self.look_intensity == 0.0)
            && self.faded_film == d.faded_film
            && self.lift.is_zero()
            && self.gamma.is_zero()
            && self.gain.is_zero()
            && self.vignette_amount == d.vignette_amount
            && self.curves.is_identity()
            && !self.has_active_lut()
    }

    /// Trägt der Clip mindestens eine wirkende LUT (Input oder Look)?
    pub fn has_active_lut(&self) -> bool {
        self.input_lut.as_ref().is_some_and(|s| s.is_active())
            || self.look_lut.as_ref().is_some_and(|s| s.is_active())
    }
}

/// Die Input-/Look-LUT-Slots eines Grades über einen [`LutCache`] zu einem
/// besitzenden [`OwnedLutStack`] auflösen (Player-Vorschau, Scopes). Fehlende
/// Dateien fallen still weg (im Panel separat als Offline-Hinweis gemeldet).
pub fn resolve_luts(grade: &ColorGrade, cache: &mut LutCache) -> OwnedLutStack {
    OwnedLutStack {
        input: grade
            .input_lut
            .as_ref()
            .and_then(|s| cache.resolve(&s.path, s.strength01())),
        look: grade
            .look_lut
            .as_ref()
            .and_then(|s| cache.resolve(&s.path, s.strength01())),
    }
}

/// Testmodus (`EDITRON_TEST_GRADE`): "saturation=0,exposure=1,look=mono"
/// → ColorGrade für visuelle Smoke-Tests. Unbekannte Schlüssel werden
/// ignoriert.
pub fn parse_test_grade(spec: &str) -> ColorGrade {
    let mut g = ColorGrade::default();
    for pair in spec.split(',') {
        let Some((key, value)) = pair.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.trim());
        if key == "look" {
            if let Some(look) = GradeLook::ALL
                .iter()
                .find(|l| l.label().eq_ignore_ascii_case(value) || format!("{l:?}").eq_ignore_ascii_case(value))
            {
                g.look = *look;
            }
            continue;
        }
        // 3D-LUT-Slots: "inputLut=/pfad.cube" / "lookLut=/pfad.cube"
        // (optional ":stärke" am Ende, z. B. "lookLut=/x.cube:60").
        if key == "inputLut" || key == "lookLut" {
            let (path, strength) = match value.rsplit_once(':') {
                Some((p, s)) if s.parse::<f64>().is_ok() => {
                    (p.to_string(), s.parse::<f64>().unwrap_or(100.0))
                }
                _ => (value.to_string(), 100.0),
            };
            if !path.is_empty() {
                let name = std::path::Path::new(&path)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let slot = LutSlot { path, name, strength };
                if key == "inputLut" {
                    g.input_lut = Some(slot);
                } else {
                    g.look_lut = Some(slot);
                }
            }
            continue;
        }
        // Kurven: "lumaCurve=0/0;0.5/0.75;1/1" (Punkte x/y, mit ';' getrennt;
        // Schlüssel luma|master|red|r|green|g|blue|b + "Curve").
        if let Some(chan) = key.strip_suffix("Curve") {
            let mut pts = Vec::new();
            for seg in value.split(';') {
                if let Some((xs, ys)) = seg.split_once('/') {
                    if let (Ok(x), Ok(y)) = (xs.trim().parse::<f64>(), ys.trim().parse::<f64>()) {
                        pts.push(CurvePoint {
                            x: x.clamp(0.0, 1.0),
                            y: y.clamp(0.0, 1.0),
                        });
                    }
                }
            }
            if pts.len() >= 2 {
                pts.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
                let curve = Curve { points: pts };
                match chan {
                    "luma" | "master" => g.curves.luma = curve,
                    "red" | "r" => g.curves.red = curve,
                    "green" | "g" => g.curves.green = curve,
                    "blue" | "b" => g.curves.blue = curve,
                    _ => {}
                }
            }
            continue;
        }
        let Ok(v) = value.parse::<f64>() else { continue };
        match key {
            "temperature" => g.temperature = v,
            "tint" => g.tint = v,
            "exposure" => g.exposure = v,
            "contrast" => g.contrast = v,
            "highlights" => g.highlights = v,
            "shadows" => g.shadows = v,
            "whites" => g.whites = v,
            "blacks" => g.blacks = v,
            "saturation" => g.saturation = v,
            "vibrance" => g.vibrance = v,
            "intensity" => g.look_intensity = v,
            "faded" => g.faded_film = v,
            "vignette" => g.vignette_amount = v,
            "liftLuma" => g.lift.luma = v,
            "gammaLuma" => g.gamma.luma = v,
            "gainLuma" => g.gain.luma = v,
            "liftX" => g.lift.x = v,
            "liftY" => g.lift.y = v,
            "gainX" => g.gain.x = v,
            "gainY" => g.gain.y = v,
            _ => {}
        }
    }
    g
}

// ------------------------------------------------------------ Vorberechnung

/// Auflösung der vorberechneten Kurven-LUT (ein Eintrag je 8-Bit-Code).
/// MUSS mit dem Shader (`lutR/lutG/lutB[256]` in `ui/grade_shader.rs`)
/// übereinstimmen — dort per `const _`-Assert abgesichert.
pub const LUT_N: usize = 256;

/// Identitäts-LUT: Eintrag i = i/(N−1). Const, damit [`IDENTITY`] eine
/// echte Konstante bleibt und neutrale Kurven bit-genau erkannt werden.
const fn identity_lut() -> [f32; LUT_N] {
    let mut lut = [0.0f32; LUT_N];
    let mut i = 0;
    while i < LUT_N {
        lut[i] = i as f32 / (LUT_N as f32 - 1.0);
        i += 1;
    }
    lut
}

const IDENTITY_LUT: [f32; LUT_N] = identity_lut();

/// LUT an `x` ∈ 0…1 abtasten — lineare Interpolation mit Klemmung an den
/// Rändern (entspricht GL_LINEAR + CLAMP_TO_EDGE des Shader-Pendants, daher
/// formelgleich: `a + (b − a)·frac`).
#[inline]
pub fn sample_lut(lut: &[f32; LUT_N], x: f32) -> f32 {
    let f = x.clamp(0.0, 1.0) * (LUT_N as f32 - 1.0);
    let fi = f.floor();
    let idx = fi as usize;
    let frac = f - fi;
    let a = lut[idx];
    let b = lut[(idx + 1).min(LUT_N - 1)];
    a + (b - a) * frac
}

/// Per-Pixel-Parameter (CPU-Pfad und Shader-Uniforms): das gesamte Grading
/// inkl. Look auf wenige branchfreie Operationen reduziert.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradeParams {
    /// RGB-Gains in linearem Licht (Weißabgleich × 2^Belichtung), luma-normiert.
    pub wb_gain: [f32; 3],
    /// Tonwert-Offsets (bereits auf Wirkstärke skaliert).
    pub blacks: f32,
    pub shadows: f32,
    pub highlights: f32,
    pub whites: f32,
    /// Kontrast-Steigung um den Pivot 0,5.
    pub slope: f32,
    /// Lift/Gamma/Gain pro Kanal (Farbräder + Look + Faded Film gefaltet).
    pub lift: [f32; 3],
    pub inv_gamma: [f32; 3],
    pub gain: [f32; 3],
    /// Sättigungsfaktor (1 = neutral) und Dynamik −1…1.
    pub saturation: f32,
    pub vibrance: f32,
    /// [Stärke (signiert −1…1), Mittelpunkt 0…1, Weichkante 0…1, Rundheit −1…1]
    pub vignette: [f32; 4],
    /// Sind Tonwertkurven aktiv? (Sonst wird die LUT-Stufe übersprungen —
    /// exakter, billiger Durchlass wie vor dem Kurven-Feature.)
    pub curves_on: bool,
    /// Kombinierte Kurven-LUT je Kanal (R/G/B), Master bereits gefaltet:
    /// `curve_lut[ch][i] = kanal_ch(master(i/(N−1)))`. Bei inaktiven Kurven
    /// die Identitäts-LUT.
    pub curve_lut: [[f32; LUT_N]; 3],
}

pub const IDENTITY: GradeParams = GradeParams {
    wb_gain: [1.0, 1.0, 1.0],
    blacks: 0.0,
    shadows: 0.0,
    highlights: 0.0,
    whites: 0.0,
    slope: 1.0,
    lift: [0.0, 0.0, 0.0],
    inv_gamma: [1.0, 1.0, 1.0],
    gain: [1.0, 1.0, 1.0],
    saturation: 1.0,
    vibrance: 0.0,
    vignette: [0.0, 0.5, 0.5, 0.0],
    curves_on: false,
    curve_lut: [IDENTITY_LUT; 3],
};

impl GradeParams {
    pub fn is_identity(&self) -> bool {
        *self == IDENTITY
    }
}

/// Wheel-Position (x, y im Einheitskreis) → RGB-Richtung (−1…1 je Kanal).
/// Winkel wie im UI-Farbrad: 0° = Rot, 120° = Grün, 240° = Blau (HSV-Rad).
fn wheel_rgb(x: f64, y: f64) -> [f32; 3] {
    let r = (x * x + y * y).sqrt().min(1.0);
    if r < 1e-9 {
        return [0.0, 0.0, 0.0];
    }
    let hue = y.atan2(x).to_degrees().rem_euclid(360.0);
    // HSV(hue, 1, 1) − Grau(0,5), auf −1…1 skaliert: reine Farbrichtung.
    let h = hue / 60.0;
    let f = |n: f64| {
        let k = (n + h).rem_euclid(6.0);
        1.0 - (k.min(4.0 - k).clamp(0.0, 1.0))
    };
    let (cr, cg, cb) = (f(5.0), f(3.0), f(1.0));
    [
        (((cr - 0.5) * 2.0) * r) as f32,
        (((cg - 0.5) * 2.0) * r) as f32,
        (((cb - 0.5) * 2.0) * r) as f32,
    ]
}

/// Look-Deltas bei Intensität 1,0 — als Anpassung der GradeParams-Bausteine.
struct LookDelta {
    temp: f64,
    contrast: f64,
    sat_mul: f64,
    faded: f64,
    lift_rgb: [f32; 3],
    gain_rgb: [f32; 3],
}

const NO_LOOK: LookDelta = LookDelta {
    temp: 0.0,
    contrast: 0.0,
    sat_mul: 1.0,
    faded: 0.0,
    lift_rgb: [0.0; 3],
    gain_rgb: [0.0; 3],
};

fn look_delta(look: GradeLook) -> LookDelta {
    match look {
        GradeLook::Neutral => NO_LOOK,
        GradeLook::FilmWarm => LookDelta {
            temp: 22.0,
            contrast: 10.0,
            sat_mul: 1.06,
            ..NO_LOOK
        },
        GradeLook::FilmCold => LookDelta {
            temp: -22.0,
            contrast: 10.0,
            sat_mul: 1.06,
            ..NO_LOOK
        },
        GradeLook::Mono => LookDelta {
            contrast: 12.0,
            sat_mul: 0.0,
            ..NO_LOOK
        },
        GradeLook::BleachBypass => LookDelta {
            contrast: 35.0,
            sat_mul: 0.35,
            gain_rgb: [0.04, 0.04, 0.04],
            ..NO_LOOK
        },
        GradeLook::TealOrange => LookDelta {
            sat_mul: 1.10,
            contrast: 8.0,
            lift_rgb: [-0.030, 0.012, 0.038],
            gain_rgb: [0.045, 0.004, -0.045],
            ..NO_LOOK
        },
        GradeLook::Vintage => LookDelta {
            temp: 12.0,
            sat_mul: 0.85,
            faded: 35.0,
            gain_rgb: [0.02, 0.0, -0.02],
            ..NO_LOOK
        },
    }
}

/// `ColorGrade` → per-Pixel-Parameter. Bypass/Neutral ⇒ Identität.
#[allow(clippy::needless_range_loop)] // Kanal-Indizes spiegeln den Shader
pub fn precompute(grade: &ColorGrade) -> GradeParams {
    if !grade.is_active() {
        return IDENTITY;
    }
    let mix = (grade.look_intensity / 100.0).clamp(0.0, 1.0);
    let look = look_delta(grade.look);

    // ---- Weißabgleich + Belichtung (linear, luma-normiert) ----
    let t = ((grade.temperature + look.temp * mix) / 100.0).clamp(-1.5, 1.5);
    let ti = (grade.tint / 100.0).clamp(-1.0, 1.0);
    let mut gr = 1.0 + 0.30 * t + 0.10 * ti;
    let mut gg = 1.0 - 0.20 * ti;
    let mut gb = 1.0 - 0.30 * t + 0.10 * ti;
    let norm = 0.2126 * gr + 0.7152 * gg + 0.0722 * gb;
    if norm > 1e-6 {
        gr /= norm;
        gg /= norm;
        gb /= norm;
    }
    let expo = 2f64.powf(grade.exposure.clamp(-5.0, 5.0));
    let wb_gain = [
        (gr.max(0.0) * expo) as f32,
        (gg.max(0.0) * expo) as f32,
        (gb.max(0.0) * expo) as f32,
    ];

    // ---- Tonwerte ----
    let blacks = (grade.blacks / 100.0 * 0.20) as f32;
    let shadows = (grade.shadows / 100.0 * 0.25) as f32;
    let highlights = (grade.highlights / 100.0 * 0.25) as f32;
    let whites = (grade.whites / 100.0 * 0.20) as f32;

    // ---- Kontrast ----
    let contrast = (grade.contrast + look.contrast * mix).clamp(-100.0, 100.0);
    let slope = (1.0 + contrast / 100.0 * 0.8).max(0.0) as f32;

    // ---- Farbräder + Look + Faded Film → Lift/Gamma/Gain ----
    let faded = (grade.faded_film / 100.0 + look.faded / 100.0 * mix).clamp(0.0, 1.0) as f32;
    let lift_c = wheel_rgb(grade.lift.x, grade.lift.y);
    let gamma_c = wheel_rgb(grade.gamma.x, grade.gamma.y);
    let gain_c = wheel_rgb(grade.gain.x, grade.gain.y);
    let mut lift = [0f32; 3];
    let mut inv_gamma = [1f32; 3];
    let mut gain = [1f32; 3];
    for ch in 0..3 {
        lift[ch] = lift_c[ch] * 0.20
            + grade.lift.luma as f32 * 0.25
            + look.lift_rgb[ch] * mix as f32
            + faded * 0.10;
        let g = (1.0 + gamma_c[ch] * 0.40 + grade.gamma.luma as f32 * 0.50).clamp(0.20, 5.0);
        inv_gamma[ch] = 1.0 / g;
        gain[ch] = (1.0
            + gain_c[ch] * 0.30
            + grade.gain.luma as f32 * 0.30
            + look.gain_rgb[ch] * mix as f32
            - faded * 0.06)
            .max(0.0);
    }

    // ---- Sättigung / Dynamik ----
    let sat_mul = 1.0 + (look.sat_mul - 1.0) * mix;
    let saturation = ((grade.saturation / 100.0) * sat_mul).clamp(0.0, 4.0) as f32
        * (1.0 - faded * 0.15);
    let vibrance = (grade.vibrance / 100.0).clamp(-1.0, 1.0) as f32;

    // ---- Vignette ----
    let vignette = [
        (grade.vignette_amount / 100.0).clamp(-1.0, 1.0) as f32,
        (grade.vignette_midpoint / 100.0).clamp(0.0, 1.0) as f32,
        (grade.vignette_feather / 100.0).clamp(0.0, 1.0) as f32,
        (grade.vignette_roundness / 100.0).clamp(-1.0, 1.0) as f32,
    ];

    // ---- Tonwertkurven → kombinierte 1D-LUT je Kanal (Master ∘ Kanal) ----
    // Neutrale Kurven ⇒ bit-genaue Identitäts-LUT (kein Beinahe-Identitäts-
    // Drift durch f64→f32, damit is_identity() greift und die Stufe entfällt).
    let curves_on = !grade.curves.is_identity();
    let curve_lut = if curves_on {
        let curves = &grade.curves;
        // Tangenten einmal je Kurve (statt je Stützstelle) vorberechnen.
        let (lt, rt, gt, bt) = (
            curves.luma.prepare_tangents(),
            curves.red.prepare_tangents(),
            curves.green.prepare_tangents(),
            curves.blue.prepare_tangents(),
        );
        let mut lut = [[0f32; LUT_N]; 3];
        for i in 0..LUT_N {
            let x = i as f64 / (LUT_N as f64 - 1.0);
            let m = curves.luma.eval_prepared(&lt, x); // Master zuerst
            lut[0][i] = curves.red.eval_prepared(&rt, m) as f32;
            lut[1][i] = curves.green.eval_prepared(&gt, m) as f32;
            lut[2][i] = curves.blue.eval_prepared(&bt, m) as f32;
        }
        lut
    } else {
        [IDENTITY_LUT; 3]
    };

    GradeParams {
        wb_gain,
        blacks,
        shadows,
        highlights,
        whites,
        slope,
        lift,
        inv_gamma,
        gain,
        saturation,
        vibrance,
        vignette,
        curves_on,
        curve_lut,
    }
}

// ------------------------------------------------------------- CPU-Renderer

#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Ein Pixel (Gamma-RGB 0…1) durch die Grading-Pipeline schicken.
/// `u`, `v` = normierte Position im Clip-Frame (Vignette).
/// MUSS formelgleich mit dem Fragment-Shader bleiben (`ui/grade_shader.rs`).
/// EINZIGE Referenz-Implementierung der vollen Pipeline — der Puffer-Pfad
/// [`grade_buffer`] ruft sie direkt pro Pixel (keine separate LUT-Variante
/// mehr, damit CPU-Export und GPU-Vorschau garantiert identisch rechnen).
#[inline]
#[allow(clippy::needless_range_loop)] // Kanal-Indizes spiegeln den Shader
pub fn grade_pixel(p: &GradeParams, rgb: [f32; 3], u: f32, v: f32) -> [f32; 3] {
    let mut c = [0f32; 3];
    // 1) Weißabgleich + Belichtung in linearem Licht (γ 2,2).
    for ch in 0..3 {
        let lin = rgb[ch].max(0.0).powf(2.2) * p.wb_gain[ch];
        c[ch] = lin.max(0.0).powf(1.0 / 2.2);
    }
    // 2) Tonwerte: luma-gewichtete Offsets.
    let l = luma(c[0], c[1], c[2]);
    let tonal = p.blacks * (1.0 - smoothstep(0.0, 0.25, l))
        + p.shadows * (1.0 - smoothstep(0.0, 0.65, l))
        + p.highlights * smoothstep(0.35, 1.0, l)
        + p.whites * smoothstep(0.75, 1.0, l);
    // 3) Kontrast um 0,5; 4) Lift/Gamma/Gain.
    for ch in 0..3 {
        let g = ((c[ch] + tonal - 0.5) * p.slope + 0.5).clamp(0.0, 1.0);
        let g = (g * p.gain[ch] + p.lift[ch] * (1.0 - g)).clamp(0.0, 1.0);
        c[ch] = g.powf(p.inv_gamma[ch]);
    }
    // 4.5) Tonwertkurven (Master ∘ Kanal als kombinierte LUT je Kanal).
    if p.curves_on {
        c[0] = sample_lut(&p.curve_lut[0], c[0]);
        c[1] = sample_lut(&p.curve_lut[1], c[1]);
        c[2] = sample_lut(&p.curve_lut[2], c[2]);
    }
    // 5) Sättigung/Dynamik (luma-erhaltend).
    let l = luma(c[0], c[1], c[2]);
    let max_c = c[0].max(c[1]).max(c[2]);
    let min_c = c[0].min(c[1]).min(c[2]);
    let sat_now = (max_c - min_c).clamp(0.0, 1.0);
    let sat = (p.saturation * (1.0 + p.vibrance * (1.0 - smoothstep(0.0, 0.5, sat_now)))).max(0.0);
    for ch in 0..3 {
        c[ch] = l + (c[ch] - l) * sat;
    }
    // 6) Vignette.
    let amount = p.vignette[0];
    if amount != 0.0 {
        let px = (u - 0.5) * 2.0;
        let py = (v - 0.5) * 2.0;
        let circ = (px * px + py * py).sqrt() * std::f32::consts::FRAC_1_SQRT_2;
        let rect = px.abs().max(py.abs());
        let shape = (p.vignette[3] + 1.0) * 0.5; // 0 = rechteckig, 1 = rund
        let d = rect + (circ - rect) * shape;
        let mid = p.vignette[1];
        let feather = p.vignette[2].max(0.01);
        let f = smoothstep(mid, (mid + feather).min(1.5), d);
        if amount > 0.0 {
            for ch in 0..3 {
                c[ch] *= 1.0 - amount * f;
            }
        } else {
            for ch in 0..3 {
                c[ch] += (1.0 - c[ch]) * (-amount) * f;
            }
        }
    }
    [
        c[0].clamp(0.0, 1.0),
        c[1].clamp(0.0, 1.0),
        c[2].clamp(0.0, 1.0),
    ]
}

/// Volle Per-Pixel-Pipeline INKL. 3D-LUTs (CPU-Pendant zum Shader). Die LUTs
/// umschließen das Grading sauber und lassen [`grade_pixel`] unberührt:
///
/// 1. **Input-LUT** (falls aktiv) auf das Quellpixel — ganz am Anfang.
/// 2. Vollständiges Grading [`grade_pixel`] (Weißabgleich … Vignette);
///    bei Identitäts-Parametern übersprungen (exakter Durchlass für den
///    „Identitäts-LUT verändert nichts“-Fall).
/// 3. **Look-LUT** (falls aktiv) auf das gegradete Pixel — ganz am Ende.
/// 4. Finale Klemmung auf 0…1 (LUTs dürfen zwischendurch über den Bereich
///    hinausgehen; die GPU klemmt ebenfalls erst zum Schluss).
///
/// MUSS formelgleich mit dem Fragment-Shader (`ui/grade_shader.rs`, `applyLut`)
/// bleiben.
#[inline]
pub fn grade_lut_pixel(
    p: &GradeParams,
    luts: &LutStack,
    rgb: [f32; 3],
    u: f32,
    v: f32,
) -> [f32; 3] {
    let rgb = match luts.input {
        Some(a) => a.lut.apply(rgb, a.strength),
        None => rgb,
    };
    let mut c = if p.is_identity() {
        rgb
    } else {
        grade_pixel(p, rgb, u, v)
    };
    if let Some(a) = luts.look {
        c = a.lut.apply(c, a.strength);
    }
    [
        c[0].clamp(0.0, 1.0),
        c[1].clamp(0.0, 1.0),
        c[2].clamp(0.0, 1.0),
    ]
}

/// f32-RGBA-Puffer (0..1, display-referred) in place graden. `content` =
/// (x, y, w, h) des sichtbaren Inhalts im Puffer (Vignetten-Bezugsrahmen —
/// beim Export ist der Puffer das volle, transparent gepolsterte Frame, der
/// Clip liegt contain-fit darin). Zeilenbänder laufen parallel auf `threads`
/// Threads. Ruft pro Pixel direkt [`grade_pixel`] ⇒ formelgleich mit dem
/// GPU-Shader, kein Banding (volle f32-Präzision bis zur finalen
/// Quantisierung in der Encoder-Pipe).
pub fn grade_buffer(
    data: &mut [f32],
    w: usize,
    h: usize,
    content: (usize, usize, usize, usize),
    p: &GradeParams,
    luts: &LutStack,
    threads: usize,
) {
    debug_assert_eq!(data.len(), w * h * 4);
    if (p.is_identity() && !luts.is_active()) || w == 0 || h == 0 {
        return;
    }
    let (cx, cy, cw, ch_) = content;
    let inv_cw = 1.0 / cw.max(1) as f32;
    let inv_ch = 1.0 / ch_.max(1) as f32;

    let threads = threads.clamp(1, 64);
    let band_rows = h.div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for (band_idx, band) in data.chunks_mut(band_rows * w * 4).enumerate() {
            scope.spawn(move || {
                let y0 = band_idx * band_rows;
                let rows = band.len() / (w * 4);
                for row in 0..rows {
                    let y = y0 + row;
                    let v = (y as f32 - cy as f32 + 0.5) * inv_ch;
                    let line = &mut band[row * w * 4..(row + 1) * w * 4];
                    for (x, px) in line.chunks_exact_mut(4).enumerate() {
                        if px[3] <= 0.0 {
                            continue; // transparentes Padding
                        }
                        let u = (x as f32 - cx as f32 + 0.5) * inv_cw;
                        let out = grade_lut_pixel(p, luts, [px[0], px[1], px[2]], u, v);
                        px[0] = out[0];
                        px[1] = out[1];
                        px[2] = out[2];
                    }
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::lut::{parse_cube, LutApply, LutStack};

    fn close(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    /// Invertierende 2³-LUT (`out = 1 − in`) zum Testen der LUT-Slots.
    fn invert_lut() -> crate::core::lut::Lut {
        let n = 2usize;
        let mut s = format!("LUT_3D_SIZE {n}\n");
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let f = |i: usize| i as f32 / (n as f32 - 1.0);
                    s.push_str(&format!("{} {} {}\n", 1.0 - f(r), 1.0 - f(g), 1.0 - f(b)));
                }
            }
        }
        parse_cube(&s).expect("invert lut")
    }

    #[test]
    fn default_grade_is_identity() {
        let g = ColorGrade::default();
        assert!(g.is_default());
        assert!(g.is_neutral());
        assert!(!g.is_active());
        assert!(precompute(&g).is_identity());
        let px = grade_pixel(&IDENTITY, [0.25, 0.5, 0.75], 0.5, 0.5);
        assert!(close(px[0], 0.25, 1e-4) && close(px[1], 0.5, 1e-4) && close(px[2], 0.75, 1e-4));
    }

    #[test]
    fn bypass_disables_grading() {
        let mut g = ColorGrade::default();
        g.exposure = 2.0;
        assert!(g.is_active());
        g.enabled = false;
        assert!(!g.is_active());
        assert!(precompute(&g).is_identity());
    }

    #[test]
    fn exposure_one_stop_doubles_linear_light() {
        let mut g = ColorGrade::default();
        g.exposure = 1.0;
        let p = precompute(&g);
        let input = 0.3f32;
        let out = grade_pixel(&p, [input; 3], 0.5, 0.5);
        let expected = (input.powf(2.2) * 2.0).powf(1.0 / 2.2);
        assert!(close(out[0], expected, 1e-3), "{} vs {}", out[0], expected);
        assert!(close(out[1], out[0], 1e-5) && close(out[2], out[0], 1e-5));
    }

    #[test]
    fn saturation_zero_yields_gray() {
        let mut g = ColorGrade::default();
        g.saturation = 0.0;
        let p = precompute(&g);
        let out = grade_pixel(&p, [0.8, 0.3, 0.1], 0.5, 0.5);
        assert!(close(out[0], out[1], 1e-4) && close(out[1], out[2], 1e-4));
    }

    #[test]
    fn mono_look_yields_gray_and_intensity_scales_back() {
        let mut g = ColorGrade::default();
        g.look = GradeLook::Mono;
        let p = precompute(&g);
        let out = grade_pixel(&p, [0.8, 0.3, 0.1], 0.5, 0.5);
        assert!(close(out[0], out[1], 1e-4) && close(out[1], out[2], 1e-4));
        g.look_intensity = 0.0;
        assert!(!g.is_active(), "Look mit Intensität 0 ist neutral");
    }

    #[test]
    fn warm_temperature_shifts_red_up_blue_down() {
        let mut g = ColorGrade::default();
        g.temperature = 50.0;
        let p = precompute(&g);
        let out = grade_pixel(&p, [0.5, 0.5, 0.5], 0.5, 0.5);
        assert!(out[0] > 0.5 && out[2] < 0.5, "warm: {:?}", out);
        // Luma-normiert: Helligkeit bleibt ungefähr erhalten.
        let l = luma(out[0], out[1], out[2]);
        assert!(close(l, 0.5, 0.02), "Luma erhalten: {l}");
    }

    #[test]
    fn contrast_pushes_values_apart_around_pivot() {
        let mut g = ColorGrade::default();
        g.contrast = 50.0;
        let p = precompute(&g);
        let lo = grade_pixel(&p, [0.3; 3], 0.5, 0.5)[0];
        let hi = grade_pixel(&p, [0.7; 3], 0.5, 0.5)[0];
        assert!(lo < 0.3 && hi > 0.7, "Kontrast: {lo} / {hi}");
    }

    #[test]
    fn shadows_lift_dark_pixels_more_than_bright() {
        let mut g = ColorGrade::default();
        g.shadows = 100.0;
        let p = precompute(&g);
        let dark = grade_pixel(&p, [0.15; 3], 0.5, 0.5)[0] - 0.15;
        let bright = grade_pixel(&p, [0.85; 3], 0.5, 0.5)[0] - 0.85;
        assert!(dark > 0.05, "Schatten heben: {dark}");
        assert!(bright < dark * 0.3, "Lichter kaum betroffen: {bright}");
    }

    #[test]
    fn gain_wheel_luma_brightens_highlights() {
        let mut g = ColorGrade::default();
        g.gain.luma = 0.5;
        let p = precompute(&g);
        let out = grade_pixel(&p, [0.6; 3], 0.5, 0.5)[0];
        assert!(out > 0.6, "Gain hebt: {out}");
    }

    #[test]
    fn lift_wheel_color_tints_shadows() {
        let mut g = ColorGrade::default();
        g.lift = WheelValue { x: 1.0, y: 0.0, luma: 0.0 }; // Richtung Rot
        let p = precompute(&g);
        let out = grade_pixel(&p, [0.1; 3], 0.5, 0.5);
        assert!(out[0] > out[2], "Schatten röter: {:?}", out);
        let hi = grade_pixel(&p, [0.95; 3], 0.5, 0.5);
        assert!((hi[0] - hi[2]).abs() < (out[0] - out[2]), "Lichter weniger betroffen");
    }

    #[test]
    fn vignette_darkens_corners_not_center() {
        let mut g = ColorGrade::default();
        g.vignette_amount = 100.0;
        let p = precompute(&g);
        let center = grade_pixel(&p, [0.5; 3], 0.5, 0.5)[0];
        let corner = grade_pixel(&p, [0.5; 3], 0.0, 0.0)[0];
        assert!(close(center, 0.5, 1e-3), "Mitte unangetastet: {center}");
        assert!(corner < 0.3, "Ecke abgedunkelt: {corner}");
    }

    #[test]
    fn grade_buffer_matches_pixel_path_and_skips_transparent() {
        let mut g = ColorGrade::default();
        g.exposure = 1.0;
        g.saturation = 150.0;
        let p = precompute(&g);
        let (w, h) = (4usize, 2usize);
        let (r, gg, b) = (100.0 / 255.0, 150.0 / 255.0, 200.0 / 255.0);
        let mut buf = vec![0f32; w * h * 4];
        for (i, px) in buf.chunks_exact_mut(4).enumerate() {
            px[0] = r;
            px[1] = gg;
            px[2] = b;
            px[3] = if i == 0 { 0.0 } else { 1.0 };
        }
        grade_buffer(&mut buf, w, h, (0, 0, w, h), &p, &LutStack::EMPTY, 2);
        // Transparenter Pixel unangetastet.
        assert_eq!(&buf[0..3], &[r, gg, b]);
        // Opaker Pixel = grade_pixel-Ergebnis (jetzt exakt — gleicher Pfad).
        let expected = grade_pixel(
            &p,
            [r, gg, b],
            (1.0 + 0.5) / w as f32,
            0.5 / h as f32,
        );
        for ch in 0..3 {
            let got = buf[4 + ch];
            assert!(close(got, expected[ch], 1e-6), "ch{ch}: {got} vs {}", expected[ch]);
        }
    }

    #[test]
    fn grade_roundtrips_through_json() {
        let mut g = ColorGrade::default();
        g.temperature = 25.0;
        g.look = GradeLook::TealOrange;
        g.gain = WheelValue { x: 0.3, y: -0.2, luma: 0.1 };
        g.vignette_amount = 40.0;
        let json = serde_json::to_string(&g).unwrap();
        let back: ColorGrade = serde_json::from_str(&json).unwrap();
        assert_eq!(g, back);
        // Leeres Objekt ⇒ Default (ältere Projektdateien ohne Grade-Felder).
        let legacy: ColorGrade = serde_json::from_str("{}").unwrap();
        assert!(legacy.is_default());
    }

    #[test]
    fn float_grade_with_dither_beats_8bit_banding() {
        use crate::core::pixbuf;
        // Sanfter vertikaler Verlauf 0,45 → 0,55 über 200 Zeilen — in 8 Bit nur
        // ~26 Codes, also sichtbares Banding nach einem kräftigen Grade.
        let (w, h) = (8usize, 200usize);
        let ramp = |y: usize| 0.45 + (y as f32 / (h - 1) as f32) * 0.10;

        // f32-Quelle.
        let mut src = vec![0f32; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let v = ramp(y);
                src[i] = v;
                src[i + 1] = v;
                src[i + 2] = v;
                src[i + 3] = 1.0;
            }
        }

        // Kräftiger Grade (Kontrast spreizt den schmalen Bereich auf).
        let mut g = ColorGrade::default();
        g.contrast = 80.0;
        let p = precompute(&g);

        // NEUER Pfad: f32-Grade → TPDF-Dither auf 8 Bit.
        let mut hi = src.clone();
        grade_buffer(&mut hi, w, h, (0, 0, w, h), &p, &LutStack::EMPTY, 2);
        let new_u8 = pixbuf::f32_to_rgba8_dithered(&hi, w, h);

        // ALTER Pfad simuliert: Quelle ZUERST auf 8 Bit quantisieren, dann
        // graden (1 Eingangscode ⇒ 1 Ausgangscode = Treppenstufen), ohne Dither.
        let src8 = pixbuf::f32_to_rgba8(&src);
        let mut old = pixbuf::rgba8_to_f32(&src8);
        grade_buffer(&mut old, w, h, (0, 0, w, h), &p, &LutStack::EMPTY, 2);
        let old_u8 = pixbuf::f32_to_rgba8(&old);

        let distinct = |buf: &[u8]| -> usize {
            buf.iter().step_by(4).collect::<std::collections::HashSet<_>>().len()
        };
        let new_levels = distinct(&new_u8);
        let old_levels = distinct(&old_u8);
        // Der gespreizte schmale Verlauf belegt im Ziel eine Range von ~42
        // Codes. Der 8-Bit-zuerst-Pfad quetscht nur die ~26 Quellcodes hinein
        // (Lücken = Banding); Float+Dither füllt die Range lückenlos.
        assert!(
            (new_levels as f64) >= (old_levels as f64) * 1.5,
            "Float+Dither bricht Banding: neu={new_levels} Stufen vs alt={old_levels}"
        );
        assert!(new_levels >= 40, "Verlauf füllt die Range lückenlos: {new_levels}");
        // Und: die belegten Codes des Float-Pfads sind nahezu lückenlos
        // (kein Sprung > 2 Codes zwischen benachbarten belegten Stufen).
        let mut codes: Vec<u8> = new_u8.iter().step_by(4).copied().collect::<std::collections::HashSet<_>>().into_iter().collect();
        codes.sort_unstable();
        let max_gap = codes.windows(2).map(|w| w[1] - w[0]).max().unwrap_or(0);
        assert!(max_gap <= 2, "keine harten Banding-Sprünge: größte Lücke {max_gap}");
    }

    #[test]
    fn wheel_rgb_directions() {
        let red = wheel_rgb(1.0, 0.0);
        assert!(red[0] > 0.9 && red[1] < 0.0 && red[2] < 0.0, "{red:?}");
        let zero = wheel_rgb(0.0, 0.0);
        assert_eq!(zero, [0.0; 3]);
    }

    // -------------------------------------------------------- Tonwertkurven

    #[test]
    fn curve_identity_eval_is_passthrough() {
        let c = Curve::identity();
        for &x in &[0.0, 0.1, 0.25, 0.5, 0.751, 1.0] {
            assert!((c.eval(x) - x).abs() < 1e-9, "id eval {x} -> {}", c.eval(x));
        }
        assert!(c.is_identity());
    }

    #[test]
    fn curve_passes_through_control_points_and_is_monotone() {
        let c = Curve {
            points: vec![
                CurvePoint { x: 0.0, y: 0.05 },
                CurvePoint { x: 0.5, y: 0.8 },
                CurvePoint { x: 1.0, y: 0.95 },
            ],
        };
        // Stützpunkte werden exakt getroffen.
        assert!((c.eval(0.0) - 0.05).abs() < 1e-9);
        assert!((c.eval(0.5) - 0.8).abs() < 1e-9);
        assert!((c.eval(1.0) - 0.95).abs() < 1e-9);
        // Monoton steigend, im Einheitsintervall, kein Überschwingen.
        let mut prev = -1.0;
        for k in 0..=100 {
            let x = k as f64 / 100.0;
            let y = c.eval(x);
            assert!(y >= prev - 1e-9, "monoton bei {x}: {y} < {prev}");
            assert!((0.0..=1.0).contains(&y), "im Intervall bei {x}: {y}");
            prev = y;
        }
        // Im steigenden Segment [0,5..1,0] bleibt alles ≤ 0,95 (kein Overshoot).
        for k in 50..=100 {
            let x = k as f64 / 100.0;
            assert!(c.eval(x) <= 0.95 + 1e-9, "Overshoot bei {x}: {}", c.eval(x));
        }
        assert!(!c.is_identity());
    }

    #[test]
    fn precompute_folds_master_then_channel_into_lut() {
        let mut g = ColorGrade::default();
        g.curves.luma = Curve {
            points: vec![
                CurvePoint { x: 0.0, y: 0.0 },
                CurvePoint { x: 0.5, y: 0.6 },
                CurvePoint { x: 1.0, y: 1.0 },
            ],
        };
        g.curves.red = Curve {
            points: vec![
                CurvePoint { x: 0.0, y: 0.1 },
                CurvePoint { x: 1.0, y: 0.9 },
            ],
        };
        let p = precompute(&g);
        assert!(p.curves_on);
        for &i in &[0usize, 64, 128, 200, LUT_N - 1] {
            let x = i as f64 / (LUT_N as f64 - 1.0);
            let m = g.curves.luma.eval(x);
            let expect_r = g.curves.red.eval(m) as f32;
            assert!(
                (p.curve_lut[0][i] - expect_r).abs() < 1e-6,
                "R-LUT[{i}] = {} vs {expect_r}",
                p.curve_lut[0][i]
            );
            // Grün ist Identität ⇒ kombinierte LUT = Master.
            assert!((p.curve_lut[1][i] - m as f32).abs() < 1e-6, "G-LUT[{i}] = Master");
        }
    }

    #[test]
    fn curve_brightening_lifts_midtones_via_grade_pixel() {
        let mut g = ColorGrade::default();
        g.curves.luma = Curve {
            points: vec![
                CurvePoint { x: 0.0, y: 0.0 },
                CurvePoint { x: 0.5, y: 0.7 },
                CurvePoint { x: 1.0, y: 1.0 },
            ],
        };
        assert!(g.is_active());
        let p = precompute(&g);
        let out = grade_pixel(&p, [0.5; 3], 0.5, 0.5);
        assert!(out[0] > 0.6, "Mitte angehoben: {}", out[0]);
        assert!(close(out[0], out[1], 1e-4) && close(out[1], out[2], 1e-4), "neutral grau");
    }

    #[test]
    fn grade_buffer_matches_pixel_path_with_curves() {
        let mut g = ColorGrade::default();
        g.curves.luma = Curve {
            points: vec![
                CurvePoint { x: 0.0, y: 0.0 },
                CurvePoint { x: 0.5, y: 0.65 },
                CurvePoint { x: 1.0, y: 1.0 },
            ],
        };
        g.curves.blue = Curve {
            points: vec![
                CurvePoint { x: 0.0, y: 0.1 },
                CurvePoint { x: 1.0, y: 0.85 },
            ],
        };
        let p = precompute(&g);
        let (w, h) = (4usize, 2usize);
        let (r, gg, b) = (100.0 / 255.0, 150.0 / 255.0, 200.0 / 255.0);
        let mut buf = vec![0f32; w * h * 4];
        for (i, px) in buf.chunks_exact_mut(4).enumerate() {
            px[0] = r;
            px[1] = gg;
            px[2] = b;
            px[3] = if i == 0 { 0.0 } else { 1.0 };
        }
        grade_buffer(&mut buf, w, h, (0, 0, w, h), &p, &LutStack::EMPTY, 2);
        assert_eq!(&buf[0..3], &[r, gg, b]); // transparenter Pixel unangetastet
        let expected = grade_pixel(&p, [r, gg, b], (1.0 + 0.5) / w as f32, 0.5 / h as f32);
        for ch in 0..3 {
            assert!(close(buf[4 + ch], expected[ch], 1e-6), "ch{ch}");
        }
    }

    #[test]
    fn neutral_curve_stays_identity() {
        let mut g = ColorGrade::default();
        // Kollinearer Zusatzpunkt wirkt wie die Identität.
        g.curves.luma = Curve {
            points: vec![
                CurvePoint { x: 0.0, y: 0.0 },
                CurvePoint { x: 0.3, y: 0.3 },
                CurvePoint { x: 1.0, y: 1.0 },
            ],
        };
        assert!(g.is_neutral());
        assert!(!g.is_active());
        assert!(precompute(&g).is_identity());
    }

    #[test]
    fn curves_roundtrip_through_json() {
        let mut g = ColorGrade::default();
        g.curves.luma = Curve {
            points: vec![
                CurvePoint { x: 0.0, y: 0.02 },
                CurvePoint { x: 0.5, y: 0.55 },
                CurvePoint { x: 1.0, y: 0.98 },
            ],
        };
        g.curves.green = Curve {
            points: vec![
                CurvePoint { x: 0.0, y: 0.0 },
                CurvePoint { x: 1.0, y: 0.9 },
            ],
        };
        let json = serde_json::to_string(&g).unwrap();
        let back: ColorGrade = serde_json::from_str(&json).unwrap();
        assert_eq!(g, back);
        // Neutrale Kurven tauchen nicht in der Datei auf (skip_serializing_if).
        let js = serde_json::to_string(&ColorGrade::default()).unwrap();
        assert!(!js.contains("curves"), "neutrale Kurven nicht serialisiert: {js}");
    }

    #[test]
    fn curve_handles_degenerate_inputs_without_panic() {
        // Leer ⇒ Identität.
        let empty = Curve { points: vec![] };
        assert!(empty.is_identity());
        assert!((empty.eval(0.3) - 0.3).abs() < 1e-9);
        // Ein Punkt ⇒ konstanter Ausgang.
        let one = Curve { points: vec![CurvePoint { x: 0.5, y: 0.7 }] };
        assert!((one.eval(0.0) - 0.7).abs() < 1e-9);
        assert!((one.eval(1.0) - 0.7).abs() < 1e-9);
        // Doppeltes x ⇒ kein Div-by-zero, Ergebnis bleibt im Intervall.
        let dense = Curve {
            points: vec![
                CurvePoint { x: 0.0, y: 0.0 },
                CurvePoint { x: 0.5, y: 0.4 },
                CurvePoint { x: 0.5, y: 0.6 },
                CurvePoint { x: 1.0, y: 1.0 },
            ],
        };
        for k in 0..=20 {
            let y = dense.eval(k as f64 / 20.0);
            assert!((0.0..=1.0).contains(&y), "im Intervall: {y}");
        }
        // precompute mit leerer Kanalkurve neben aktiver Master ⇒ Kanal wirkt
        // als Identität (kein Panic in prepare_tangents/eval_prepared).
        let mut g = ColorGrade::default();
        g.curves.luma = Curve {
            points: vec![
                CurvePoint { x: 0.0, y: 0.1 },
                CurvePoint { x: 1.0, y: 0.9 },
            ],
        };
        g.curves.red = Curve { points: vec![] };
        let p = precompute(&g);
        assert!(p.curves_on);
        assert!((p.curve_lut[0][128] - p.curve_lut[1][128]).abs() < 1e-6);
    }

    #[test]
    fn parse_test_grade_reads_curve() {
        let g = parse_test_grade("lumaCurve=0/0;0.5/0.75;1/1,redCurve=0/0.1;1/0.9");
        assert_eq!(g.curves.luma.points.len(), 3);
        assert!((g.curves.luma.points[1].y - 0.75).abs() < 1e-9);
        assert!((g.curves.red.points[0].y - 0.1).abs() < 1e-9);
        assert!(g.curves.green.is_identity());
        assert!(g.is_active());
    }

    // ------------------------------------------------------------- 3D-LUTs

    #[test]
    fn identity_lut_changes_nothing() {
        // Identitäts-LUT in beiden Slots auf neutrale Parameter ⇒ exakter
        // Durchlass (grade_pixel wird bei Identitäts-Parametern übersprungen).
        let n = 4usize;
        let mut s = format!("LUT_3D_SIZE {n}\n");
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let f = |i: usize| i as f32 / (n as f32 - 1.0);
                    s.push_str(&format!("{} {} {}\n", f(r), f(g), f(b)));
                }
            }
        }
        let id = parse_cube(&s).expect("id lut");
        let luts = LutStack {
            input: Some(LutApply { lut: &id, strength: 1.0 }),
            look: Some(LutApply { lut: &id, strength: 1.0 }),
        };
        for &c in &[[0.0; 3], [0.25, 0.5, 0.75], [1.0; 3], [0.13, 0.87, 0.42]] {
            let out = grade_lut_pixel(&IDENTITY, &luts, c, 0.5, 0.5);
            assert!(
                close(out[0], c[0], 2e-3) && close(out[1], c[1], 2e-3) && close(out[2], c[2], 2e-3),
                "{c:?} -> {out:?}"
            );
        }
    }

    #[test]
    fn look_lut_applies_after_grade() {
        // Invert-Look-LUT auf neutrales Grading ⇒ invertiertes Pixel.
        let inv = invert_lut();
        let luts = LutStack {
            input: None,
            look: Some(LutApply { lut: &inv, strength: 1.0 }),
        };
        let out = grade_lut_pixel(&IDENTITY, &luts, [0.2, 0.4, 0.6], 0.5, 0.5);
        assert!(close(out[0], 0.8, 1e-3) && close(out[1], 0.6, 1e-3) && close(out[2], 0.4, 1e-3), "{out:?}");
    }

    #[test]
    fn lut_strength_scales_effect() {
        let inv = invert_lut();
        let luts = LutStack {
            input: None,
            look: Some(LutApply { lut: &inv, strength: 0.5 }),
        };
        // 0,2 invertiert = 0,8; mit 50 % ⇒ Mittel aus 0,2 und 0,8 = 0,5.
        let out = grade_lut_pixel(&IDENTITY, &luts, [0.2, 0.2, 0.2], 0.5, 0.5);
        assert!(close(out[0], 0.5, 1e-3), "{out:?}");
    }

    #[test]
    fn grade_buffer_applies_lut_matching_pixel_path() {
        let inv = invert_lut();
        let mut g = ColorGrade::default();
        g.exposure = 0.5;
        let p = precompute(&g);
        let luts = LutStack {
            input: Some(LutApply { lut: &inv, strength: 0.7 }),
            look: None,
        };
        let (w, h) = (4usize, 2usize);
        let (r, gg, b) = (0.4f32, 0.55f32, 0.7f32);
        let mut buf = vec![0f32; w * h * 4];
        for (i, px) in buf.chunks_exact_mut(4).enumerate() {
            px[0] = r;
            px[1] = gg;
            px[2] = b;
            px[3] = if i == 0 { 0.0 } else { 1.0 };
        }
        grade_buffer(&mut buf, w, h, (0, 0, w, h), &p, &luts, 2);
        assert_eq!(&buf[0..3], &[r, gg, b]); // transparent unangetastet
        let expected = grade_lut_pixel(&p, &luts, [r, gg, b], (1.0 + 0.5) / w as f32, 0.5 / h as f32);
        for ch in 0..3 {
            assert!(close(buf[4 + ch], expected[ch], 1e-6), "ch{ch}: {} vs {}", buf[4 + ch], expected[ch]);
        }
    }

    #[test]
    fn lut_slot_makes_grade_active_and_roundtrips() {
        let mut g = ColorGrade::default();
        assert!(!g.is_active());
        g.look_lut = Some(LutSlot {
            path: "/luts/teal_orange.cube".into(),
            name: "teal_orange".into(),
            strength: 80.0,
        });
        assert!(g.has_active_lut());
        assert!(g.is_active(), "LUT-Slot aktiviert das Grading");
        assert!(!g.is_default());
        let json = serde_json::to_string(&g).unwrap();
        let back: ColorGrade = serde_json::from_str(&json).unwrap();
        assert_eq!(g, back);
        // Strength 0 ⇒ Slot inaktiv (aber serialisiert, da non-default).
        g.look_lut.as_mut().unwrap().strength = 0.0;
        assert!(!g.has_active_lut());
        assert!(!g.is_active());
    }

    #[test]
    fn parse_test_grade_reads_lut_slots() {
        let g = parse_test_grade("lookLut=/tmp/teal.cube:60,inputLut=/tmp/log2rec709.cube");
        let look = g.look_lut.as_ref().expect("look slot");
        assert_eq!(look.path, "/tmp/teal.cube");
        assert_eq!(look.name, "teal");
        assert!((look.strength - 60.0).abs() < 1e-9);
        let input = g.input_lut.as_ref().expect("input slot");
        assert_eq!(input.path, "/tmp/log2rec709.cube");
        assert!((input.strength - 100.0).abs() < 1e-9);
        assert!(g.has_active_lut());
    }

    #[test]
    fn empty_lut_path_or_zero_strength_is_inactive() {
        let mut g = ColorGrade::default();
        g.input_lut = Some(LutSlot { path: String::new(), name: String::new(), strength: 100.0 });
        assert!(!g.has_active_lut(), "leerer Pfad ⇒ inaktiv");
        g.input_lut = Some(LutSlot { path: "/x.cube".into(), name: "x".into(), strength: 0.0 });
        assert!(!g.has_active_lut(), "Stärke 0 ⇒ inaktiv");
    }
}
