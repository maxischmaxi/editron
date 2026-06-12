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
//! 5. Sättigung/Dynamik (luma-erhaltend)
//! 6. Vignette über die normierten Clip-Koordinaten

use serde::{Deserialize, Serialize};

// -------------------------------------------------------------- Datenmodell

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
/// Referenz-Implementierung der vollen Pipeline (Tests, künftige Aufrufer);
/// der Puffer-Pfad nutzt die LUT-Variante `grade_pixel_post_wb`.
#[inline]
#[allow(clippy::needless_range_loop)] // Kanal-Indizes spiegeln den Shader
#[cfg_attr(not(test), allow(dead_code))]
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

/// RGBA-Puffer in place graden. `content` = (x, y, w, h) des sichtbaren
/// Inhalts im Puffer (Vignetten-Bezugsrahmen — beim Export ist der Puffer
/// das volle, transparent gepolsterte Frame, der Clip liegt contain-fit
/// darin). Zeilenbänder laufen parallel auf `threads` Threads.
/// Weißabgleich (Schritt 1) läuft über eine 256er-LUT je Kanal.
#[allow(clippy::needless_range_loop)] // Kanal-Indizes spiegeln den Shader
pub fn grade_buffer(
    data: &mut [u8],
    w: usize,
    h: usize,
    content: (usize, usize, usize, usize),
    p: &GradeParams,
    threads: usize,
) {
    debug_assert_eq!(data.len(), w * h * 4);
    if p.is_identity() || w == 0 || h == 0 {
        return;
    }
    // LUT für Schritt 1 (per Kanal): u8 → Gamma-Wert nach WB/Belichtung.
    let mut wb_lut = [[0f32; 256]; 3];
    for ch in 0..3 {
        for (i, slot) in wb_lut[ch].iter_mut().enumerate() {
            let lin = (i as f32 / 255.0).powf(2.2) * p.wb_gain[ch];
            *slot = lin.max(0.0).powf(1.0 / 2.2);
        }
    }
    let (cx, cy, cw, ch_) = content;
    let inv_cw = 1.0 / cw.max(1) as f32;
    let inv_ch = 1.0 / ch_.max(1) as f32;

    let threads = threads.clamp(1, 64);
    let band_rows = h.div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for (band_idx, band) in data.chunks_mut(band_rows * w * 4).enumerate() {
            let wb_lut = &wb_lut;
            scope.spawn(move || {
                let y0 = band_idx * band_rows;
                let rows = band.len() / (w * 4);
                for row in 0..rows {
                    let y = y0 + row;
                    let v = (y as f32 - cy as f32 + 0.5) * inv_ch;
                    let line = &mut band[row * w * 4..(row + 1) * w * 4];
                    for (x, px) in line.chunks_exact_mut(4).enumerate() {
                        if px[3] == 0 {
                            continue; // transparentes Padding
                        }
                        let u = (x as f32 - cx as f32 + 0.5) * inv_cw;
                        let rgb = [
                            wb_lut[0][px[0] as usize],
                            wb_lut[1][px[1] as usize],
                            wb_lut[2][px[2] as usize],
                        ];
                        let out = grade_pixel_post_wb(p, rgb, u, v);
                        px[0] = (out[0] * 255.0 + 0.5) as u8;
                        px[1] = (out[1] * 255.0 + 0.5) as u8;
                        px[2] = (out[2] * 255.0 + 0.5) as u8;
                    }
                }
            });
        }
    });
}

/// Pipeline ab Schritt 2 (Weißabgleich bereits angewendet — LUT-Pfad).
#[inline]
#[allow(clippy::needless_range_loop)] // Kanal-Indizes spiegeln den Shader
fn grade_pixel_post_wb(p: &GradeParams, rgb: [f32; 3], u: f32, v: f32) -> [f32; 3] {
    // Identisch zu `grade_pixel` ab den Tonwerten; eigene Funktion, damit
    // der LUT-Pfad Schritt 1 überspringen kann.
    let mut c = rgb;
    let l = luma(c[0], c[1], c[2]);
    let tonal = p.blacks * (1.0 - smoothstep(0.0, 0.25, l))
        + p.shadows * (1.0 - smoothstep(0.0, 0.65, l))
        + p.highlights * smoothstep(0.35, 1.0, l)
        + p.whites * smoothstep(0.75, 1.0, l);
    for ch in 0..3 {
        let g = ((c[ch] + tonal - 0.5) * p.slope + 0.5).clamp(0.0, 1.0);
        let g = (g * p.gain[ch] + p.lift[ch] * (1.0 - g)).clamp(0.0, 1.0);
        c[ch] = if p.inv_gamma[ch] == 1.0 {
            g
        } else {
            g.powf(p.inv_gamma[ch])
        };
    }
    let l = luma(c[0], c[1], c[2]);
    let max_c = c[0].max(c[1]).max(c[2]);
    let min_c = c[0].min(c[1]).min(c[2]);
    let sat_now = (max_c - min_c).clamp(0.0, 1.0);
    let sat = (p.saturation * (1.0 + p.vibrance * (1.0 - smoothstep(0.0, 0.5, sat_now)))).max(0.0);
    for ch in 0..3 {
        c[ch] = l + (c[ch] - l) * sat;
    }
    let amount = p.vignette[0];
    if amount != 0.0 {
        let px = (u - 0.5) * 2.0;
        let py = (v - 0.5) * 2.0;
        let circ = (px * px + py * py).sqrt() * std::f32::consts::FRAC_1_SQRT_2;
        let rect = px.abs().max(py.abs());
        let shape = (p.vignette[3] + 1.0) * 0.5;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
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
        let mut buf = vec![0u8; w * h * 4];
        for (i, px) in buf.chunks_exact_mut(4).enumerate() {
            px[0] = 100;
            px[1] = 150;
            px[2] = 200;
            px[3] = if i == 0 { 0 } else { 255 };
        }
        grade_buffer(&mut buf, w, h, (0, 0, w, h), &p, 2);
        // Transparenter Pixel unangetastet.
        assert_eq!(&buf[0..3], &[100, 150, 200]);
        // Opaker Pixel = grade_pixel-Ergebnis (LUT-Pfad ≈ powf-Pfad).
        let expected = grade_pixel(
            &p,
            [100.0 / 255.0, 150.0 / 255.0, 200.0 / 255.0],
            (1.0 + 0.5) / w as f32,
            0.5 / h as f32,
        );
        for ch in 0..3 {
            let got = buf[4 + ch] as f32 / 255.0;
            assert!(close(got, expected[ch], 0.01), "ch{ch}: {got} vs {}", expected[ch]);
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
    fn wheel_rgb_directions() {
        let red = wheel_rgb(1.0, 0.0);
        assert!(red[0] > 0.9 && red[1] < 0.0 && red[2] < 0.0, "{red:?}");
        let zero = wheel_rgb(0.0, 0.0);
        assert_eq!(zero, [0.0; 3]);
    }
}
