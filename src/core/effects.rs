//! Effektsystem: pro Clip ein Stapel von [`EffectInstance`]s mit animierbaren
//! Parametern (Keyframes in MEDIENZEIT wie `ClipFx`). Video-Effekte laufen
//! ZWEIMAL mit identischen Formeln: GPU-Fragment-Shader für die Echtzeit-
//! Vorschau (`ui/fx_shader.rs`) und der CPU-Pfad hier (Export, Scopes,
//! Tests). Audio-Effekte werden in `core/audio_fx.rs` als DSP-Kette auf die
//! dekodierten Samples angewendet (Player-Mixdown UND Export-Mix).
//!
//! Pipeline pro Video-Layer: Decode → Effekt-Stapel (Reihenfolge = Stapel-
//! Reihenfolge, oben zuerst) → Farbkorrektur (`core/grade.rs`) → Transform/
//! Compositing. Alle Ortsangaben sind auflösungsunabhängig (% der sichtbaren
//! Inhaltsmaße), damit Vorschau und Export identisch aussehen.

use crate::core::animation::AnimatedParam;
use crate::core::mask::{self, Mask};
use crate::core::types::new_id;
use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------ Katalog

/// Alle verfügbaren Effekte (Video + Audio).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EffectKind {
    // ---- Video ----
    GaussianBlur,
    Sharpen,
    Glow,
    ChromaKey,
    LumaKey,
    Crop,
    Flip,
    Pixelate,
    Posterize,
    Invert,
    HueSaturation,
    BrightnessContrast,
    // ---- Audio ----
    Equalizer,
    Compressor,
    Limiter,
    Highpass,
    Lowpass,
    Gain,
    Reverb,
    NoiseGate,
    Delay,
    /// De-Esser: frequenzselektive Kompression im Zisch-Band (EQ-Detektor
    /// steuert die Gain-Reduktion, die NUR auf das Band wirkt).
    DeEsser,
    /// Auto-Ducking: Kompressor mit Sidechain-Eingang. Der Pegel-Detektor
    /// folgt dem KEY-Signal (Summe der anderen Spuren), die Gain-Reduktion
    /// senkt das eigene Signal (Musik unter Sprache).
    Ducking,
}

/// Anzeige-Kategorie im Effekte-Panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectCategory {
    BlurSharpen,
    Color,
    Keying,
    Stylize,
    Transform,
    Audio,
}

impl EffectCategory {
    pub fn label(&self) -> &'static str {
        match self {
            EffectCategory::BlurSharpen => "Weichzeichnen & Schärfen",
            EffectCategory::Color => "Farbe",
            EffectCategory::Keying => "Keying",
            EffectCategory::Stylize => "Stilisieren",
            EffectCategory::Transform => "Transformieren",
            EffectCategory::Audio => "Audio-Effekte",
        }
    }

    pub const ALL: [EffectCategory; 6] = [
        EffectCategory::BlurSharpen,
        EffectCategory::Color,
        EffectCategory::Keying,
        EffectCategory::Stylize,
        EffectCategory::Transform,
        EffectCategory::Audio,
    ];
}

/// Darstellung eines Parameters im Panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamUi {
    /// Wert-Scrubbing + Inline-Eingabe (wie Bewegungs-Parameter).
    Slider,
    /// Checkbox; Wert 0/1, nicht animierbar.
    Toggle,
    /// Erster von drei aufeinanderfolgenden Parametern (R, G, B 0–255),
    /// gerendert als eine Farbzeile mit Vorschau-Swatch.
    ColorRgb,
}

/// Spezifikation eines Effekt-Parameters (statisch je [`EffectKind`]).
#[derive(Clone, Copy, Debug)]
pub struct EffectParamSpec {
    /// Stabiler Schlüssel (Test-Flags, ggf. spätere Presets).
    pub key: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    /// Wertänderung pro gezogenem Pixel (Wert-Scrubbing).
    pub step: f64,
    pub decimals: usize,
    pub ui: ParamUi,
    /// Stopwatch/Keyframes anbieten?
    pub animatable: bool,
}

const fn slider(
    key: &'static str,
    label: &'static str,
    unit: &'static str,
    min: f64,
    max: f64,
    default: f64,
    step: f64,
) -> EffectParamSpec {
    EffectParamSpec {
        key,
        label,
        unit,
        min,
        max,
        default,
        step,
        decimals: 1,
        ui: ParamUi::Slider,
        animatable: true,
    }
}

const fn toggle(key: &'static str, label: &'static str, default: bool) -> EffectParamSpec {
    EffectParamSpec {
        key,
        label,
        unit: "",
        min: 0.0,
        max: 1.0,
        default: if default { 1.0 } else { 0.0 },
        step: 1.0,
        decimals: 0,
        ui: ParamUi::Toggle,
        animatable: false,
    }
}

const fn color_channel(key: &'static str, label: &'static str, default: f64) -> EffectParamSpec {
    EffectParamSpec {
        key,
        label,
        unit: "",
        min: 0.0,
        max: 255.0,
        default,
        step: 1.0,
        decimals: 0,
        ui: ParamUi::ColorRgb,
        animatable: false,
    }
}

const fn no_decimals(mut s: EffectParamSpec) -> EffectParamSpec {
    s.decimals = 0;
    s
}

const fn dec2(mut s: EffectParamSpec) -> EffectParamSpec {
    s.decimals = 2;
    s
}

static SPECS_GAUSSIAN_BLUR: [EffectParamSpec; 1] =
    [slider("strength", "Stärke", "", 0.0, 100.0, 10.0, 0.25)];
static SPECS_SHARPEN: [EffectParamSpec; 1] =
    [slider("amount", "Stärke", "%", 0.0, 300.0, 80.0, 1.0)];
static SPECS_GLOW: [EffectParamSpec; 2] = [
    slider("intensity", "Stärke", "%", 0.0, 100.0, 50.0, 0.5),
    slider("radius", "Radius", "", 0.0, 100.0, 30.0, 0.5),
];
static SPECS_CHROMA_KEY: [EffectParamSpec; 6] = [
    color_channel("keyR", "Farbe R", 0.0),
    color_channel("keyG", "Farbe G", 255.0),
    color_channel("keyB", "Farbe B", 0.0),
    slider("tolerance", "Toleranz", "%", 0.0, 100.0, 30.0, 0.25),
    slider("softness", "Weiche Kante", "%", 0.0, 100.0, 10.0, 0.25),
    slider("spill", "Spill-Unterdrückung", "%", 0.0, 100.0, 50.0, 0.5),
];
static SPECS_LUMA_KEY: [EffectParamSpec; 3] = [
    slider("threshold", "Schwellenwert", "%", 0.0, 100.0, 25.0, 0.25),
    slider("softness", "Weiche Kante", "%", 0.0, 100.0, 15.0, 0.25),
    toggle("invert", "Invertieren (Helles ausblenden)", false),
];
static SPECS_CROP: [EffectParamSpec; 5] = [
    slider("left", "Links", "%", 0.0, 100.0, 0.0, 0.25),
    slider("right", "Rechts", "%", 0.0, 100.0, 0.0, 0.25),
    slider("top", "Oben", "%", 0.0, 100.0, 0.0, 0.25),
    slider("bottom", "Unten", "%", 0.0, 100.0, 0.0, 0.25),
    slider("feather", "Weiche Kante", "%", 0.0, 100.0, 0.0, 0.25),
];
static SPECS_FLIP: [EffectParamSpec; 2] = [
    toggle("horizontal", "Horizontal spiegeln", true),
    toggle("vertical", "Vertikal spiegeln", false),
];
static SPECS_PIXELATE: [EffectParamSpec; 1] =
    [slider("strength", "Blockgröße", "", 0.0, 100.0, 20.0, 0.25)];
static SPECS_POSTERIZE: [EffectParamSpec; 1] =
    [no_decimals(slider("levels", "Stufen", "", 2.0, 32.0, 6.0, 0.1))];
static SPECS_INVERT: [EffectParamSpec; 1] =
    [slider("mix", "Stärke", "%", 0.0, 100.0, 100.0, 0.5)];
static SPECS_HUE_SATURATION: [EffectParamSpec; 3] = [
    slider("hue", "Farbton", "°", -180.0, 180.0, 0.0, 0.5),
    slider("saturation", "Sättigung", "%", 0.0, 200.0, 100.0, 0.5),
    slider("lightness", "Helligkeit", "%", -100.0, 100.0, 0.0, 0.5),
];
static SPECS_BRIGHTNESS_CONTRAST: [EffectParamSpec; 2] = [
    slider("brightness", "Helligkeit", "", -100.0, 100.0, 0.0, 0.5),
    slider("contrast", "Kontrast", "", -100.0, 100.0, 0.0, 0.5),
];
// Parametrischer 4-Band-EQ: je Band Frequenz/Gain/Güte. Band 0 = Low-Shelf,
// 1+2 = Glocken, 3 = High-Shelf (Reihenfolge ist DSP- und kurvenbindend,
// siehe `audio_fx::eq_band_biquads`).
static SPECS_EQUALIZER: [EffectParamSpec; 12] = [
    no_decimals(slider("lowFreq", "Tiefen-Frequenz", "Hz", 20.0, 500.0, 100.0, 1.0)),
    slider("lowGain", "Tiefen", "dB", -18.0, 18.0, 0.0, 0.1),
    dec2(slider("lowQ", "Tiefen-Güte", "", 0.3, 3.0, 0.71, 0.01)),
    no_decimals(slider("mid1Freq", "Mitte 1 — Frequenz", "Hz", 40.0, 2000.0, 250.0, 2.0)),
    slider("mid1Gain", "Mitte 1 — Gain", "dB", -18.0, 18.0, 0.0, 0.1),
    dec2(slider("mid1Q", "Mitte 1 — Güte", "", 0.2, 10.0, 1.0, 0.02)),
    no_decimals(slider("mid2Freq", "Mitte 2 — Frequenz", "Hz", 200.0, 8000.0, 2000.0, 5.0)),
    slider("mid2Gain", "Mitte 2 — Gain", "dB", -18.0, 18.0, 0.0, 0.1),
    dec2(slider("mid2Q", "Mitte 2 — Güte", "", 0.2, 10.0, 1.0, 0.02)),
    no_decimals(slider("highFreq", "Höhen-Frequenz", "Hz", 1000.0, 20000.0, 8000.0, 10.0)),
    slider("highGain", "Höhen", "dB", -18.0, 18.0, 0.0, 0.1),
    dec2(slider("highQ", "Höhen-Güte", "", 0.3, 3.0, 0.71, 0.01)),
];
static SPECS_COMPRESSOR: [EffectParamSpec; 5] = [
    slider("threshold", "Threshold", "dB", -60.0, 0.0, -18.0, 0.25),
    slider("ratio", "Ratio", ": 1", 1.0, 20.0, 4.0, 0.05),
    slider("attack", "Attack", "ms", 0.1, 200.0, 10.0, 0.5),
    slider("release", "Release", "ms", 10.0, 2000.0, 150.0, 2.0),
    slider("makeup", "Makeup-Gain", "dB", 0.0, 24.0, 0.0, 0.1),
];
static SPECS_LIMITER: [EffectParamSpec; 2] = [
    slider("ceiling", "Ceiling", "dB", -24.0, 0.0, -1.0, 0.1),
    slider("release", "Release", "ms", 1.0, 500.0, 50.0, 1.0),
];
static SPECS_HIGHPASS: [EffectParamSpec; 2] = [
    no_decimals(slider("freq", "Frequenz", "Hz", 20.0, 2000.0, 80.0, 1.0)),
    dec2(slider("q", "Güte", "", 0.3, 3.0, 0.71, 0.01)),
];
static SPECS_LOWPASS: [EffectParamSpec; 2] = [
    no_decimals(slider("freq", "Frequenz", "Hz", 200.0, 20000.0, 12000.0, 10.0)),
    dec2(slider("q", "Güte", "", 0.3, 3.0, 0.71, 0.01)),
];
static SPECS_GAIN: [EffectParamSpec; 1] =
    [slider("gain", "Verstärkung", "dB", -24.0, 24.0, 0.0, 0.1)];
static SPECS_REVERB: [EffectParamSpec; 3] = [
    slider("roomSize", "Raumgröße", "%", 0.0, 100.0, 50.0, 0.5),
    slider("damping", "Dämpfung", "%", 0.0, 100.0, 50.0, 0.5),
    slider("mixWet", "Anteil", "%", 0.0, 100.0, 30.0, 0.5),
];
static SPECS_NOISE_GATE: [EffectParamSpec; 2] = [
    slider("threshold", "Schwelle", "dB", -80.0, -20.0, -50.0, 0.25),
    slider("release", "Release", "ms", 10.0, 1000.0, 120.0, 2.0),
];
static SPECS_DELAY: [EffectParamSpec; 3] = [
    slider("time", "Zeit", "ms", 10.0, 2000.0, 350.0, 2.0),
    slider("feedback", "Feedback", "%", 0.0, 90.0, 35.0, 0.5),
    slider("mixWet", "Anteil", "%", 0.0, 100.0, 40.0, 0.5),
];
// De-Esser: Zisch-Band (Frequenz/Güte) treibt einen frequenzselektiven
// Kompressor — die Gain-Reduktion wirkt NUR auf das bandgefilterte Signal.
static SPECS_DE_ESSER: [EffectParamSpec; 4] = [
    no_decimals(slider("freq", "Frequenz", "Hz", 2000.0, 16000.0, 7000.0, 20.0)),
    slider("threshold", "Threshold", "dB", -60.0, 0.0, -30.0, 0.25),
    slider("ratio", "Ratio", ": 1", 1.0, 20.0, 6.0, 0.05),
    dec2(slider("q", "Bandbreite (Güte)", "", 0.5, 6.0, 2.0, 0.02)),
];
// Auto-Ducking: Sidechain-Kompressor. Der Detektor folgt dem KEY-Signal
// (andere Spuren), die Gain-Reduktion senkt das eigene Signal.
static SPECS_DUCKING: [EffectParamSpec; 4] = [
    slider("threshold", "Threshold", "dB", -60.0, 0.0, -30.0, 0.25),
    slider("ratio", "Ratio", ": 1", 1.0, 20.0, 8.0, 0.05),
    slider("attack", "Attack", "ms", 0.1, 500.0, 10.0, 0.5),
    slider("release", "Release", "ms", 10.0, 3000.0, 300.0, 2.0),
];

impl EffectKind {
    pub const ALL: [EffectKind; 23] = [
        EffectKind::GaussianBlur,
        EffectKind::Sharpen,
        EffectKind::Glow,
        EffectKind::ChromaKey,
        EffectKind::LumaKey,
        EffectKind::Crop,
        EffectKind::Flip,
        EffectKind::Pixelate,
        EffectKind::Posterize,
        EffectKind::Invert,
        EffectKind::HueSaturation,
        EffectKind::BrightnessContrast,
        EffectKind::Equalizer,
        EffectKind::Compressor,
        EffectKind::Limiter,
        EffectKind::Highpass,
        EffectKind::Lowpass,
        EffectKind::Gain,
        EffectKind::Reverb,
        EffectKind::NoiseGate,
        EffectKind::Delay,
        EffectKind::DeEsser,
        EffectKind::Ducking,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            EffectKind::GaussianBlur => "Gaußscher Weichzeichner",
            EffectKind::Sharpen => "Schärfen",
            EffectKind::Glow => "Glühen",
            EffectKind::ChromaKey => "Chroma-Key",
            EffectKind::LumaKey => "Luma-Key",
            EffectKind::Crop => "Zuschneiden",
            EffectKind::Flip => "Spiegeln",
            EffectKind::Pixelate => "Mosaik",
            EffectKind::Posterize => "Posterisieren",
            EffectKind::Invert => "Negativ",
            EffectKind::HueSaturation => "Farbton & Sättigung",
            EffectKind::BrightnessContrast => "Helligkeit & Kontrast",
            EffectKind::Equalizer => "Parametrischer EQ",
            EffectKind::Compressor => "Kompressor",
            EffectKind::Limiter => "Limiter",
            EffectKind::Highpass => "Hochpass",
            EffectKind::Lowpass => "Tiefpass",
            EffectKind::Gain => "Verstärkung",
            EffectKind::Reverb => "Hall",
            EffectKind::NoiseGate => "Rauschgate",
            EffectKind::Delay => "Echo",
            EffectKind::DeEsser => "De-Esser",
            EffectKind::Ducking => "Auto-Ducking",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            EffectKind::GaussianBlur => "Weichzeichnung mit echter Gauß-Verteilung — Stärke ist animierbar.",
            EffectKind::Sharpen => "Unscharf-Maskierung: hebt Kanten an, ohne Rauschen zu verstärken.",
            EffectKind::Glow => "Heller Schein: weichgezeichnete Kopie wird per Screen-Mischung addiert.",
            EffectKind::ChromaKey => "Greenscreen/Bluescreen: Farbtaste mit Toleranz, weicher Kante und Spill-Unterdrückung.",
            EffectKind::LumaKey => "Helligkeits-Taste: dunkle (oder helle) Bereiche werden transparent.",
            EffectKind::Crop => "Bild von den Rändern her beschneiden, optional mit weicher Kante.",
            EffectKind::Flip => "Bild horizontal und/oder vertikal spiegeln.",
            EffectKind::Pixelate => "Mosaik-Verpixelung mit wählbarer Blockgröße.",
            EffectKind::Posterize => "Reduziert die Tonwerte je Kanal auf wenige Stufen.",
            EffectKind::Invert => "Invertiert die Farben (Negativ), stufenlos zumischbar.",
            EffectKind::HueSaturation => "Farbton drehen, Sättigung und Helligkeit anpassen.",
            EffectKind::BrightnessContrast => "Einfache Helligkeits- und Kontrastkorrektur.",
            EffectKind::Equalizer => "Parametrischer 4-Band-EQ: Tiefen-/Höhen-Shelf und zwei durchstimmbare Glockenfilter, je mit Frequenz, Gain und Güte.",
            EffectKind::Compressor => "Dynamikkompressor mit Threshold, Ratio, Attack/Release und Makeup-Gain.",
            EffectKind::Limiter => "Brick-Wall-Limiter ohne Latenz: hält den Pegel sicher unter der Ceiling (master-tauglich).",
            EffectKind::Highpass => "Hochpassfilter (12 dB/Okt.): entfernt Tieftonanteile unterhalb der Frequenz.",
            EffectKind::Lowpass => "Tiefpassfilter (12 dB/Okt.): entfernt Höhenanteile oberhalb der Frequenz.",
            EffectKind::Gain => "Einfache Verstärkung in dB (zipper-frei geglättet) — gut für Lautstärke-Keyframes.",
            EffectKind::Reverb => "Raumhall mit Raumgröße, Dämpfung und Mischanteil.",
            EffectKind::NoiseGate => "Schaltet Signal unterhalb der Schwelle stumm (Atmo/Rauschen zwischen Takes).",
            EffectKind::Delay => "Echo mit Verzögerungszeit, Feedback und Mischanteil.",
            EffectKind::DeEsser => "Frequenzselektive Kompression: dämpft scharfe Zischlaute (S-/Sch-Laute) im Zisch-Band, ohne den restlichen Klang zu verändern.",
            EffectKind::Ducking => "Sidechain-Kompressor: senkt diese Spur automatisch ab, sobald die anderen Spuren (Sprache) lauter werden — Musik unter Sprache.",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            EffectKind::GaussianBlur => "droplets",
            EffectKind::Sharpen => "focus",
            EffectKind::Glow => "sun",
            EffectKind::ChromaKey => "wand-sparkles",
            EffectKind::LumaKey => "contrast",
            EffectKind::Crop => "crop",
            EffectKind::Flip => "flip-horizontal-2",
            EffectKind::Pixelate => "grid-2x2",
            EffectKind::Posterize => "layers",
            EffectKind::Invert => "image-minus",
            EffectKind::HueSaturation => "palette",
            EffectKind::BrightnessContrast => "sun-medium",
            EffectKind::Equalizer => "sliders-horizontal",
            EffectKind::Compressor => "activity",
            EffectKind::Limiter => "gauge",
            EffectKind::Highpass => "chevron-up",
            EffectKind::Lowpass => "chevron-down",
            EffectKind::Gain => "volume-2",
            EffectKind::Reverb => "audio-waveform",
            EffectKind::NoiseGate => "mic-off",
            EffectKind::Delay => "timer-reset",
            EffectKind::DeEsser => "scissors",
            EffectKind::Ducking => "chevron-down",
        }
    }

    pub fn category(&self) -> EffectCategory {
        match self {
            EffectKind::GaussianBlur | EffectKind::Sharpen | EffectKind::Glow => {
                EffectCategory::BlurSharpen
            }
            EffectKind::HueSaturation
            | EffectKind::BrightnessContrast
            | EffectKind::Invert => EffectCategory::Color,
            EffectKind::ChromaKey | EffectKind::LumaKey => EffectCategory::Keying,
            EffectKind::Pixelate | EffectKind::Posterize => EffectCategory::Stylize,
            EffectKind::Crop | EffectKind::Flip => EffectCategory::Transform,
            EffectKind::Equalizer
            | EffectKind::Compressor
            | EffectKind::Limiter
            | EffectKind::Highpass
            | EffectKind::Lowpass
            | EffectKind::Gain
            | EffectKind::Reverb
            | EffectKind::NoiseGate
            | EffectKind::Delay
            | EffectKind::DeEsser
            | EffectKind::Ducking => EffectCategory::Audio,
        }
    }

    pub fn is_audio(&self) -> bool {
        self.category() == EffectCategory::Audio
    }

    /// Parameter-Spezifikation (Reihenfolge = `EffectInstance::params`).
    pub fn specs(&self) -> &'static [EffectParamSpec] {
        match self {
            EffectKind::GaussianBlur => &SPECS_GAUSSIAN_BLUR,
            EffectKind::Sharpen => &SPECS_SHARPEN,
            EffectKind::Glow => &SPECS_GLOW,
            EffectKind::ChromaKey => &SPECS_CHROMA_KEY,
            EffectKind::LumaKey => &SPECS_LUMA_KEY,
            EffectKind::Crop => &SPECS_CROP,
            EffectKind::Flip => &SPECS_FLIP,
            EffectKind::Pixelate => &SPECS_PIXELATE,
            EffectKind::Posterize => &SPECS_POSTERIZE,
            EffectKind::Invert => &SPECS_INVERT,
            EffectKind::HueSaturation => &SPECS_HUE_SATURATION,
            EffectKind::BrightnessContrast => &SPECS_BRIGHTNESS_CONTRAST,
            EffectKind::Equalizer => &SPECS_EQUALIZER,
            EffectKind::Compressor => &SPECS_COMPRESSOR,
            EffectKind::Limiter => &SPECS_LIMITER,
            EffectKind::Highpass => &SPECS_HIGHPASS,
            EffectKind::Lowpass => &SPECS_LOWPASS,
            EffectKind::Gain => &SPECS_GAIN,
            EffectKind::Reverb => &SPECS_REVERB,
            EffectKind::NoiseGate => &SPECS_NOISE_GATE,
            EffectKind::Delay => &SPECS_DELAY,
            EffectKind::DeEsser => &SPECS_DE_ESSER,
            EffectKind::Ducking => &SPECS_DUCKING,
        }
    }

    /// camelCase-Name (Test-Flags `EDITRON_TEST_EFFECT`, Debug).
    pub fn key(&self) -> &'static str {
        match self {
            EffectKind::GaussianBlur => "gaussianBlur",
            EffectKind::Sharpen => "sharpen",
            EffectKind::Glow => "glow",
            EffectKind::ChromaKey => "chromaKey",
            EffectKind::LumaKey => "lumaKey",
            EffectKind::Crop => "crop",
            EffectKind::Flip => "flip",
            EffectKind::Pixelate => "pixelate",
            EffectKind::Posterize => "posterize",
            EffectKind::Invert => "invert",
            EffectKind::HueSaturation => "hueSaturation",
            EffectKind::BrightnessContrast => "brightnessContrast",
            EffectKind::Equalizer => "equalizer",
            EffectKind::Compressor => "compressor",
            EffectKind::Limiter => "limiter",
            EffectKind::Highpass => "highpass",
            EffectKind::Lowpass => "lowpass",
            EffectKind::Gain => "gain",
            EffectKind::Reverb => "reverb",
            EffectKind::NoiseGate => "noiseGate",
            EffectKind::Delay => "delay",
            EffectKind::DeEsser => "deEsser",
            EffectKind::Ducking => "ducking",
        }
    }
}

// ------------------------------------------------------------------ Instanz

/// Ein angewendeter Effekt auf einem Clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectInstance {
    pub id: String,
    pub kind: EffectKind,
    /// Bypass-Schalter (fx-Toggle) — Werte bleiben erhalten.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Parameterwerte in Spec-Reihenfolge (animierbar). Fehlende Einträge
    /// (ältere Projektdateien) werden über [`EffectInstance::normalize`]
    /// mit Defaults aufgefüllt.
    #[serde(default)]
    pub params: Vec<AnimatedParam>,
    /// Geometrische Masken: begrenzen diesen Effekt auf eine Region. Leer ⇒
    /// Effekt wirkt aufs ganze Bild. Mehrere Masken werden vereinigt; jede ist
    /// invertierbar (siehe [`crate::core::mask`]).
    #[serde(default)]
    pub masks: Vec<Mask>,
}

fn default_true() -> bool {
    true
}

impl EffectInstance {
    pub fn new(kind: EffectKind) -> EffectInstance {
        EffectInstance {
            id: new_id(),
            kind,
            enabled: true,
            params: kind
                .specs()
                .iter()
                .map(|s| AnimatedParam::fixed(s.default))
                .collect(),
            masks: Vec::new(),
        }
    }

    /// Parameterliste auf die Spec-Länge bringen (Defaults auffüllen,
    /// überzählige verwerfen) — Vorwärts-/Rückwärtskompatibilität.
    pub fn normalize(&mut self) {
        let specs = self.kind.specs();
        while self.params.len() < specs.len() {
            self.params
                .push(AnimatedParam::fixed(specs[self.params.len()].default));
        }
        self.params.truncate(specs.len());
    }

    /// Alle Parameter auf Defaults zurücksetzen (Keyframes verwerfen).
    pub fn reset(&mut self) {
        self.params = self
            .kind
            .specs()
            .iter()
            .map(|s| AnimatedParam::fixed(s.default))
            .collect();
    }

    /// Hat irgendein Parameter Keyframes?
    pub fn any_animated(&self) -> bool {
        self.params.iter().any(|p| p.is_animated())
    }

    /// Parameter zur Medienzeit auswerten (geklemmte Spec-Reihenfolge).
    pub fn eval(&self, media_t: f64) -> ResolvedEffect {
        let specs = self.kind.specs();
        ResolvedEffect {
            kind: self.kind,
            values: specs
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let v = self
                        .params
                        .get(i)
                        .map(|p| p.eval(media_t))
                        .unwrap_or(s.default);
                    // NaN/Inf (korrupter Keyframe) würde clamp unverändert
                    // durchreichen → auf den Default zurückfallen.
                    let v = if v.is_finite() { v } else { s.default };
                    v.clamp(s.min, s.max)
                })
                .collect(),
            // Masken sind statisch (nicht keyframe-animiert) — nur aktive
            // übernehmen, damit GPU/CPU bei „alle Masken deaktiviert“ den
            // Effekt unmaskiert (aufs ganze Bild) anwenden.
            masks: self.masks.iter().filter(|m| m.enabled).cloned().collect(),
        }
    }
}

/// Aktive (enabled) Video-Effekte eines Stapels zur Medienzeit auswerten;
/// wirkungslose (Identitäts-)Effekte werden übersprungen.
pub fn resolve_video_effects(effects: &[EffectInstance], media_t: f64) -> Vec<ResolvedEffect> {
    effects
        .iter()
        .filter(|e| e.enabled && !e.kind.is_audio())
        .map(|e| e.eval(media_t))
        .filter(|r| !r.is_identity())
        .collect()
}

/// Verändert irgendein Effekt des Stapels das Bild (irgendwann im Clip)?
/// Konservativ: animierte Parameter gelten als aktiv.
pub fn has_active_video_effects(effects: &[EffectInstance]) -> bool {
    effects.iter().any(|e| {
        e.enabled && !e.kind.is_audio() && (e.any_animated() || !e.eval(0.0).is_identity())
    })
}

/// Zur Medienzeit ausgewertete Parameter eines Effekts (+ aktive Masken).
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedEffect {
    pub kind: EffectKind,
    pub values: Vec<f64>,
    /// Aktive Masken (leer ⇒ Effekt wirkt aufs ganze Bild).
    pub masks: Vec<Mask>,
}

impl ResolvedEffect {
    fn v(&self, i: usize) -> f64 {
        self.values.get(i).copied().unwrap_or(0.0)
    }

    /// Wirkt der Effekt mit diesen Werten überhaupt? (Schnellpfad.)
    pub fn is_identity(&self) -> bool {
        match self.kind {
            EffectKind::GaussianBlur => self.v(0) <= 0.0,
            EffectKind::Sharpen => self.v(0) <= 0.0,
            EffectKind::Glow => self.v(0) <= 0.0,
            EffectKind::ChromaKey => false,
            EffectKind::LumaKey => false,
            EffectKind::Crop => {
                self.v(0) <= 0.0 && self.v(1) <= 0.0 && self.v(2) <= 0.0 && self.v(3) <= 0.0
            }
            EffectKind::Flip => self.v(0) < 0.5 && self.v(1) < 0.5,
            EffectKind::Pixelate => self.v(0) <= 0.0,
            EffectKind::Posterize => self.v(0) >= 32.0,
            EffectKind::Invert => self.v(0) <= 0.0,
            EffectKind::HueSaturation => {
                self.v(0) == 0.0 && self.v(1) == 100.0 && self.v(2) == 0.0
            }
            EffectKind::BrightnessContrast => self.v(0) == 0.0 && self.v(1) == 0.0,
            // Audio-Effekte verändern das Bild nie.
            _ => true,
        }
    }
}

// ----------------------------------------------------------- CPU-Renderer
// Referenz-Implementierung der Video-Effekte auf RGBA-Puffern. Die GLSL-
// Shader in `ui/fx_shader.rs` MÜSSEN formelgleich bleiben. Alle Effekte
// arbeiten nur im `content`-Rechteck (sichtbarer Inhalt im ggf. transparent
// gepolsterten Puffer); Sampling klemmt an dessen Rändern (entspricht
// CLAMP_TO_EDGE der Vorschau-Texturen).

#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Weiche Kante mit Härtefall: `f <= 0` ⇒ harte Stufe bei `e`.
#[inline]
fn soft_edge(e: f32, f: f32, x: f32) -> f32 {
    if f < 1e-6 {
        if x >= e { 1.0 } else { 0.0 }
    } else {
        smoothstep(e, e + f, x)
    }
}

#[inline]
fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Alle Effekte nacheinander auf den f32-RGBA-Puffer (0..1, display-referred)
/// anwenden (in place). `content` = (x, y, w, h) des sichtbaren Inhalts.
/// Volle f32-Präzision ⇒ kein Banding zwischen Effekt- und Grading-Pass.
pub fn apply_effects_buffer(
    data: &mut [f32],
    w: usize,
    h: usize,
    content: (usize, usize, usize, usize),
    effects: &[ResolvedEffect],
    threads: usize,
) {
    debug_assert_eq!(data.len(), w * h * 4);
    let (_, _, cw, ch) = content;
    if cw == 0 || ch == 0 || effects.is_empty() {
        return;
    }
    let threads = threads.clamp(1, 64);
    for fx in effects {
        if fx.is_identity() {
            continue;
        }
        // Maskierter Effekt: Zustand VOR dem Effekt sichern, danach per
        // Maskendeckung zurückblenden (`mix(vorher, nachher, m)`).
        let pre_mask = (!fx.masks.is_empty()).then(|| extract_content(data, w, content));
        match fx.kind {
            EffectKind::GaussianBlur => {
                let sigma = blur_sigma(fx.v(0), cw);
                gaussian_blur(data, w, content, sigma, threads);
            }
            EffectKind::Sharpen => sharpen(data, w, content, fx.v(0), threads),
            EffectKind::Glow => glow(data, w, content, fx.v(0), fx.v(1), threads),
            EffectKind::ChromaKey => {
                let key = [
                    (fx.v(0) / 255.0) as f32,
                    (fx.v(1) / 255.0) as f32,
                    (fx.v(2) / 255.0) as f32,
                ];
                let (tol, soft, spill) = (fx.v(3) as f32, fx.v(4) as f32, fx.v(5) as f32);
                per_pixel(data, w, content, threads, move |px, _, _| {
                    chroma_key_pixel(px, key, tol, soft, spill)
                });
            }
            EffectKind::LumaKey => {
                let (th, soft, inv) = (
                    (fx.v(0) / 100.0) as f32,
                    (fx.v(1) / 100.0).max(0.0) as f32,
                    fx.v(2) >= 0.5,
                );
                per_pixel(data, w, content, threads, move |px, _, _| {
                    let l = luma(px[0], px[1], px[2]);
                    let mut m = soft_edge(th - soft * 0.5, soft, l);
                    if inv {
                        m = 1.0 - m;
                    }
                    px[3] *= m;
                });
            }
            EffectKind::Crop => {
                let (l, r, t, b, f) = (
                    (fx.v(0) / 100.0) as f32,
                    (fx.v(1) / 100.0) as f32,
                    (fx.v(2) / 100.0) as f32,
                    (fx.v(3) / 100.0) as f32,
                    (fx.v(4) / 100.0 * 0.1) as f32,
                );
                per_pixel(data, w, content, threads, move |px, u, v| {
                    let m = soft_edge(l, f, u)
                        * (1.0 - soft_edge(1.0 - r - f, f, u))
                        * soft_edge(t, f, v)
                        * (1.0 - soft_edge(1.0 - b - f, f, v));
                    px[3] *= m;
                });
            }
            EffectKind::Flip => flip(data, w, content, fx.v(0) >= 0.5, fx.v(1) >= 0.5),
            EffectKind::Pixelate => pixelate(data, w, content, fx.v(0)),
            EffectKind::Posterize => {
                let n = (fx.v(0).round() as f32).clamp(2.0, 256.0);
                per_pixel(data, w, content, threads, move |px, _, _| {
                    for c in px.iter_mut().take(3) {
                        *c = (*c * (n - 1.0)).round() / (n - 1.0);
                    }
                });
            }
            EffectKind::Invert => {
                let m = (fx.v(0) / 100.0) as f32;
                per_pixel(data, w, content, threads, move |px, _, _| {
                    for c in px.iter_mut().take(3) {
                        *c += (1.0 - 2.0 * *c) * m;
                    }
                });
            }
            EffectKind::HueSaturation => {
                let mat = hue_matrix(fx.v(0));
                let sat = (fx.v(1) / 100.0) as f32;
                let light = (fx.v(2) / 100.0) as f32;
                per_pixel(data, w, content, threads, move |px, _, _| {
                    let (r, g, b) = (px[0], px[1], px[2]);
                    let mut c = [
                        mat[0] * r + mat[1] * g + mat[2] * b,
                        mat[3] * r + mat[4] * g + mat[5] * b,
                        mat[6] * r + mat[7] * g + mat[8] * b,
                    ];
                    let l = luma(c[0], c[1], c[2]);
                    for ch in &mut c {
                        *ch = l + (*ch - l) * sat + light * 0.5;
                    }
                    px[0] = c[0];
                    px[1] = c[1];
                    px[2] = c[2];
                });
            }
            EffectKind::BrightnessContrast => {
                let b = (fx.v(0) / 100.0) as f32;
                let slope = (1.0 + fx.v(1) / 100.0).max(0.0) as f32;
                per_pixel(data, w, content, threads, move |px, _, _| {
                    for c in px.iter_mut().take(3) {
                        *c = (*c - 0.5) * slope + 0.5 + b * 0.5;
                    }
                });
            }
            // Audio-Effekte: kein Bildpfad.
            _ => {}
        }
        if let Some(before) = pre_mask {
            blend_masked(data, w, content, &before, &fx.masks);
        }
    }
}

/// Effekt-Ergebnis (`data`) per Maskendeckung mit dem Vorzustand `before`
/// (Inhalts-Kopie) zurückblenden: `out = mix(before, data, m)`. `m` aus
/// [`mask::combined_mask`] in denselben Inhalts-UVs wie [`per_pixel`].
/// Formelgleich zum GPU-Mask-Blend-Pass in `ui/fx_shader`.
fn blend_masked(
    data: &mut [f32],
    w: usize,
    content: (usize, usize, usize, usize),
    before: &[f32],
    masks: &[Mask],
) {
    let (cx, cy, cw, ch) = content;
    if cw == 0 || ch == 0 || masks.is_empty() {
        return;
    }
    let inv_cw = 1.0 / cw as f32;
    let inv_ch = 1.0 / ch as f32;
    for y in 0..ch {
        let v = (y as f32 + 0.5) * inv_ch;
        for x in 0..cw {
            let u = (x as f32 + 0.5) * inv_cw;
            let m = mask::combined_mask(masks, u, v);
            if m >= 1.0 {
                continue; // voller Effekt — nichts zurückzublenden
            }
            let d = ((cy + y) * w + cx + x) * 4;
            let s = (y * cw + x) * 4;
            for c in 0..4 {
                data[d + c] = before[s + c] + (data[d + c] - before[s + c]) * m;
            }
        }
    }
}

/// Stärke (0–100) → Sigma in Pixeln, relativ zur Inhaltsbreite (2 % bei 100).
pub fn blur_sigma(strength: f64, content_w: usize) -> f32 {
    (strength / 100.0 * 0.02 * content_w as f64) as f32
}

/// Sharpen-Basisradius: kleine feste Unschärfe relativ zur Inhaltsbreite.
pub fn sharpen_sigma(content_w: usize) -> f32 {
    (0.0015 * content_w as f64).max(0.6) as f32
}

/// Glow-Radius (0–100) → Sigma in Pixeln (3 % der Inhaltsbreite bei 100).
pub fn glow_sigma(radius: f64, content_w: usize) -> f32 {
    (radius / 100.0 * 0.03 * content_w as f64) as f32
}

/// Gauß-Kerngewichte für `sigma` (Radius = ceil(2,5 σ), normalisiert).
/// IDENTISCH im Shader (dort per `exp` in der Sampling-Schleife).
pub fn gaussian_kernel(sigma: f32) -> (usize, Vec<f32>) {
    let radius = ((sigma * 2.5).ceil() as usize).clamp(1, 254);
    let inv = 1.0 / (2.0 * sigma * sigma).max(1e-9);
    let mut weights: Vec<f32> = (0..=radius)
        .map(|i| (-((i * i) as f32) * inv).exp())
        .collect();
    let sum: f32 = weights[0] + 2.0 * weights[1..].iter().sum::<f32>();
    for w in &mut weights {
        *w /= sum;
    }
    (radius, weights)
}

/// Pixel-lokale Effekte parallel über Zeilenbänder des Inhalts ausführen.
/// Der Closure bekommt RGBA (0–1, NICHT premultiplied) und die normierten
/// Inhalts-UVs; Alpha wird mitgeführt.
fn per_pixel(
    data: &mut [f32],
    w: usize,
    content: (usize, usize, usize, usize),
    threads: usize,
    f: impl Fn(&mut [f32; 4], f32, f32) + Sync,
) {
    let (cx, cy, cw, ch) = content;
    let inv_cw = 1.0 / cw.max(1) as f32;
    let inv_ch = 1.0 / ch.max(1) as f32;
    let band_rows = ch.div_ceil(threads).max(1);
    // Nur die Inhaltszeilen [cy, cy+ch) bearbeiten.
    let start = cy * w * 4;
    let end = ((cy + ch) * w * 4).min(data.len());
    let rows_data = &mut data[start..end];
    std::thread::scope(|scope| {
        for (band_idx, band) in rows_data.chunks_mut(band_rows * w * 4).enumerate() {
            let f = &f;
            scope.spawn(move || {
                let y0 = cy + band_idx * band_rows;
                let rows = band.len() / (w * 4);
                for row in 0..rows {
                    let y = y0 + row;
                    let v = (y as f32 - cy as f32 + 0.5) * inv_ch;
                    let line = &mut band[row * w * 4..(row + 1) * w * 4];
                    for x in cx..(cx + cw).min(w) {
                        let px = &mut line[x * 4..x * 4 + 4];
                        let u = (x as f32 - cx as f32 + 0.5) * inv_cw;
                        let mut c = [px[0], px[1], px[2], px[3]];
                        f(&mut c, u, v);
                        px[0] = c[0].clamp(0.0, 1.0);
                        px[1] = c[1].clamp(0.0, 1.0);
                        px[2] = c[2].clamp(0.0, 1.0);
                        px[3] = c[3].clamp(0.0, 1.0);
                    }
                }
            });
        }
    });
}

/// Chroma-Key auf einen Pixel: Abstand in der (Cb, Cr)-Ebene zur Key-Farbe.
/// Formelgleich im Shader.
#[inline]
fn chroma_key_pixel(px: &mut [f32; 4], key: [f32; 3], tol: f32, soft: f32, spill: f32) {
    let l = luma(px[0], px[1], px[2]);
    let (cb, cr) = (px[2] - l, px[0] - l);
    let kl = luma(key[0], key[1], key[2]);
    let (kcb, kcr) = (key[2] - kl, key[0] - kl);
    let klen = (kcb * kcb + kcr * kcr).sqrt().max(1e-5);
    let dist = ((cb - kcb) * (cb - kcb) + (cr - kcr) * (cr - kcr)).sqrt();
    let t0 = tol / 100.0 * 0.4;
    let t1 = t0 + (soft / 100.0 * 0.4).max(0.01);
    let mask = smoothstep(t0, t1, dist);
    px[3] *= mask;
    // Spill-Unterdrückung: Chroma-Anteil in Key-Richtung abziehen, stärker
    // nahe der Key-Farbe.
    let s = spill / 100.0;
    if s > 0.0 {
        let (ux, uy) = (kcb / klen, kcr / klen);
        let proj = (cb * ux + cr * uy).max(0.0);
        let sup = s * (1.0 - smoothstep(t0, t1 + 0.2, dist));
        let (cb2, cr2) = (cb - ux * proj * sup, cr - uy * proj * sup);
        // Rückrechnung: r = l + cr, b = l + cb, g aus der Luma-Gleichung.
        let r = l + cr2;
        let b = l + cb2;
        let g = (l - 0.2126 * r - 0.0722 * b) / 0.7152;
        px[0] = r;
        px[1] = g;
        px[2] = b;
    }
}

/// Hue-Rotationsmatrix (luma-erhaltend, Rec.-709-Gewichte) — formelgleich
/// im Shader aufgebaut.
pub fn hue_matrix(hue_deg: f64) -> [f32; 9] {
    let a = hue_deg.to_radians();
    let (s, c) = (a.sin() as f32, a.cos() as f32);
    let (lr, lg, lb) = (0.2126f32, 0.7152f32, 0.0722f32);
    [
        lr + c * (1.0 - lr) - s * lr,
        lg - c * lg - s * lg,
        lb - c * lb + s * (1.0 - lb),
        lr - c * lr + s * 0.143,
        lg + c * (1.0 - lg) + s * 0.140,
        lb - c * lb - s * 0.283,
        lr - c * lr - s * (1.0 - lr),
        lg - c * lg + s * lg,
        lb + c * (1.0 - lb) + s * lb,
    ]
}

/// Separierbarer Gauß-Weichzeichner (premultipliziertes Alpha gegen dunkle
/// Säume), geklemmt am Inhaltsrand. H- und V-Pass parallel über Bänder.
fn gaussian_blur(
    data: &mut [f32],
    w: usize,
    content: (usize, usize, usize, usize),
    sigma: f32,
    threads: usize,
) {
    if sigma < 0.1 {
        return;
    }
    let (cx, cy, cw, ch) = content;
    let (_radius, weights) = gaussian_kernel(sigma);

    // Inhalt als premultipliziertes f32-Bild extrahieren.
    let mut src: Vec<f32> = vec![0.0; cw * ch * 4];
    for y in 0..ch {
        for x in 0..cw {
            let s = ((cy + y) * w + cx + x) * 4;
            let d = (y * cw + x) * 4;
            let a = data[s + 3];
            src[d] = data[s] * a;
            src[d + 1] = data[s + 1] * a;
            src[d + 2] = data[s + 2] * a;
            src[d + 3] = a;
        }
    }
    let mut tmp: Vec<f32> = vec![0.0; cw * ch * 4];

    // Horizontaler Pass: src → tmp (Zeilenbänder parallel).
    let band_rows = ch.div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for (band_idx, band) in tmp.chunks_mut(band_rows * cw * 4).enumerate() {
            let src = &src;
            let weights = &weights;
            scope.spawn(move || {
                let y0 = band_idx * band_rows;
                let rows = band.len() / (cw * 4);
                for row in 0..rows {
                    let y = y0 + row;
                    for x in 0..cw {
                        let mut acc = [0f32; 4];
                        for (i, wt) in weights.iter().enumerate() {
                            if i == 0 {
                                let s = (y * cw + x) * 4;
                                for c in 0..4 {
                                    acc[c] += src[s + c] * wt;
                                }
                            } else {
                                let xl = x.saturating_sub(i);
                                let xr = (x + i).min(cw - 1);
                                let sl = (y * cw + xl) * 4;
                                let sr = (y * cw + xr) * 4;
                                for c in 0..4 {
                                    acc[c] += (src[sl + c] + src[sr + c]) * wt;
                                }
                            }
                        }
                        let d = (row * cw + x) * 4;
                        band[d..d + 4].copy_from_slice(&acc);
                    }
                }
            });
        }
    });

    // Vertikaler Pass: tmp → src (wieder Zeilenbänder; liest beliebige Zeilen).
    std::thread::scope(|scope| {
        for (band_idx, band) in src.chunks_mut(band_rows * cw * 4).enumerate() {
            let tmp = &tmp;
            let weights = &weights;
            scope.spawn(move || {
                let y0 = band_idx * band_rows;
                let rows = band.len() / (cw * 4);
                for row in 0..rows {
                    let y = y0 + row;
                    for x in 0..cw {
                        let mut acc = [0f32; 4];
                        for (i, wt) in weights.iter().enumerate() {
                            if i == 0 {
                                let s = (y * cw + x) * 4;
                                for c in 0..4 {
                                    acc[c] += tmp[s + c] * wt;
                                }
                            } else {
                                let yt = y.saturating_sub(i);
                                let yb = (y + i).min(ch - 1);
                                let st = (yt * cw + x) * 4;
                                let sb = (yb * cw + x) * 4;
                                for c in 0..4 {
                                    acc[c] += (tmp[st + c] + tmp[sb + c]) * wt;
                                }
                            }
                        }
                        let d = (row * cw + x) * 4;
                        band[d..d + 4].copy_from_slice(&acc);
                    }
                }
            });
        }
    });

    // Zurückschreiben (unpremultiply).
    for y in 0..ch {
        for x in 0..cw {
            let s = (y * cw + x) * 4;
            let d = ((cy + y) * w + cx + x) * 4;
            let a = src[s + 3];
            let inv = if a > 1e-5 { 1.0 / a } else { 0.0 };
            data[d] = (src[s] * inv).clamp(0.0, 1.0);
            data[d + 1] = (src[s + 1] * inv).clamp(0.0, 1.0);
            data[d + 2] = (src[s + 2] * inv).clamp(0.0, 1.0);
            data[d + 3] = a.clamp(0.0, 1.0);
        }
    }
}

/// Unscharf-Maskierung: out = src + (src − blur(src)) × amount.
fn sharpen(
    data: &mut [f32],
    w: usize,
    content: (usize, usize, usize, usize),
    amount: f64,
    threads: usize,
) {
    let (_, _, cw, ch) = content;
    let sigma = sharpen_sigma(cw);
    // Weichgezeichnete Kopie des Inhalts erzeugen.
    let mut blurred = extract_content(data, w, content);
    gaussian_blur(&mut blurred, cw, (0, 0, cw, ch), sigma, threads);
    let k = (amount / 100.0) as f32;
    combine_content(data, w, content, &blurred, |src, blur| {
        [
            src[0] + (src[0] - blur[0]) * k,
            src[1] + (src[1] - blur[1]) * k,
            src[2] + (src[2] - blur[2]) * k,
            src[3],
        ]
    });
}

/// Glühen: Screen-Mischung der weichgezeichneten Kopie.
fn glow(
    data: &mut [f32],
    w: usize,
    content: (usize, usize, usize, usize),
    intensity: f64,
    radius: f64,
    threads: usize,
) {
    let (_, _, cw, ch) = content;
    let sigma = glow_sigma(radius, cw);
    let mut blurred = extract_content(data, w, content);
    if sigma >= 0.1 {
        gaussian_blur(&mut blurred, cw, (0, 0, cw, ch), sigma, threads);
    }
    let k = (intensity / 100.0) as f32;
    combine_content(data, w, content, &blurred, |src, blur| {
        [
            1.0 - (1.0 - src[0]) * (1.0 - blur[0] * k),
            1.0 - (1.0 - src[1]) * (1.0 - blur[1] * k),
            1.0 - (1.0 - src[2]) * (1.0 - blur[2] * k),
            src[3],
        ]
    });
}

/// Inhalt als eigenständigen f32-RGBA-Puffer kopieren.
fn extract_content(
    data: &[f32],
    w: usize,
    content: (usize, usize, usize, usize),
) -> Vec<f32> {
    let (cx, cy, cw, ch) = content;
    let mut out = vec![0f32; cw * ch * 4];
    for y in 0..ch {
        let s = ((cy + y) * w + cx) * 4;
        let d = y * cw * 4;
        out[d..d + cw * 4].copy_from_slice(&data[s..s + cw * 4]);
    }
    out
}

/// Inhalt mit einer Hilfskopie verrechnen (per Pixel, 0–1 f32).
fn combine_content(
    data: &mut [f32],
    w: usize,
    content: (usize, usize, usize, usize),
    aux: &[f32],
    f: impl Fn([f32; 4], [f32; 4]) -> [f32; 4],
) {
    let (cx, cy, cw, ch) = content;
    for y in 0..ch {
        for x in 0..cw {
            let d = ((cy + y) * w + cx + x) * 4;
            let s = (y * cw + x) * 4;
            let src = [data[d], data[d + 1], data[d + 2], data[d + 3]];
            let b = [aux[s], aux[s + 1], aux[s + 2], aux[s + 3]];
            let out = f(src, b);
            for c in 0..4 {
                data[d + c] = out[c].clamp(0.0, 1.0);
            }
        }
    }
}

/// Spiegeln innerhalb des Inhaltsrechtecks (in place).
fn flip(data: &mut [f32], w: usize, content: (usize, usize, usize, usize), h_flip: bool, v_flip: bool) {
    let (cx, cy, cw, ch) = content;
    if h_flip {
        for y in 0..ch {
            for x in 0..cw / 2 {
                let a = ((cy + y) * w + cx + x) * 4;
                let b = ((cy + y) * w + cx + cw - 1 - x) * 4;
                for c in 0..4 {
                    data.swap(a + c, b + c);
                }
            }
        }
    }
    if v_flip {
        for y in 0..ch / 2 {
            for x in 0..cw {
                let a = ((cy + y) * w + cx + x) * 4;
                let b = ((cy + ch - 1 - y) * w + cx + x) * 4;
                for c in 0..4 {
                    data.swap(a + c, b + c);
                }
            }
        }
    }
}

/// Blockgröße (0–100) → Pixel (5 % der Inhaltsbreite bei 100).
pub fn pixelate_block(strength: f64, content_w: usize) -> usize {
    ((strength / 100.0 * 0.05 * content_w as f64).round() as usize).max(1)
}

/// Mosaik: jeder Block übernimmt den Pixel in seiner Mitte.
fn pixelate(data: &mut [f32], w: usize, content: (usize, usize, usize, usize), strength: f64) {
    let (cx, cy, cw, ch) = content;
    let block = pixelate_block(strength, cw);
    if block <= 1 {
        return;
    }
    let src = extract_content(data, w, content);
    for y in 0..ch {
        let sy = ((y / block) * block + block / 2).min(ch - 1);
        for x in 0..cw {
            let sx = ((x / block) * block + block / 2).min(cw - 1);
            let s = (sy * cw + sx) * 4;
            let d = ((cy + y) * w + cx + x) * 4;
            data[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
}

// ----------------------------------------------------------------- Testmodus

/// `EDITRON_TEST_EFFECT="gaussianBlur:strength=40;chromaKey:tolerance=20"`
/// → Effekt-Stapel für visuelle Smoke-Tests. Unbekannte Effekte/Parameter
/// werden ignoriert.
pub fn parse_test_effects(spec: &str) -> Vec<EffectInstance> {
    let mut out = Vec::new();
    for part in spec.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (name, args) = match part.split_once(':') {
            Some((n, a)) => (n.trim(), a.trim()),
            None => (part, ""),
        };
        let Some(kind) = EffectKind::ALL
            .iter()
            .find(|k| k.key().eq_ignore_ascii_case(name))
        else {
            continue;
        };
        let mut inst = EffectInstance::new(*kind);
        for pair in args.split(',') {
            let Some((key, value)) = pair.split_once('=') else { continue };
            let Ok(v) = value.trim().parse::<f64>() else { continue };
            if let Some(i) = kind.specs().iter().position(|s| s.key == key.trim()) {
                inst.params[i] = AnimatedParam::fixed(v);
            }
        }
        out.push(inst);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// u8-RGBA → f32 (0..1) — Tests bleiben in u8-Werten lesbar.
    fn u8px(rgba: [u8; 4]) -> [f32; 4] {
        [
            rgba[0] as f32 / 255.0,
            rgba[1] as f32 / 255.0,
            rgba[2] as f32 / 255.0,
            rgba[3] as f32 / 255.0,
        ]
    }

    fn solid(w: usize, h: usize, rgba: [u8; 4]) -> Vec<f32> {
        let f = u8px(rgba);
        let mut buf = vec![0f32; w * h * 4];
        for px in buf.chunks_exact_mut(4) {
            px.copy_from_slice(&f);
        }
        buf
    }

    /// f32-Pixel als gerundetes u8-RGBA auslesen (Assertions in u8).
    fn px(buf: &[f32], w: usize, x: usize, y: usize) -> [u8; 4] {
        let i = (y * w + x) * 4;
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        [q(buf[i]), q(buf[i + 1]), q(buf[i + 2]), q(buf[i + 3])]
    }

    fn resolved(kind: EffectKind, values: &[f64]) -> ResolvedEffect {
        ResolvedEffect {
            kind,
            values: values.to_vec(),
            masks: Vec::new(),
        }
    }

    fn resolved_masked(kind: EffectKind, values: &[f64], masks: Vec<Mask>) -> ResolvedEffect {
        ResolvedEffect {
            kind,
            values: values.to_vec(),
            masks,
        }
    }

    #[test]
    fn instance_has_spec_defaults_and_roundtrips() {
        for kind in EffectKind::ALL {
            let inst = EffectInstance::new(kind);
            assert_eq!(inst.params.len(), kind.specs().len(), "{kind:?}");
            let json = serde_json::to_string(&inst).unwrap();
            let back: EffectInstance = serde_json::from_str(&json).unwrap();
            assert_eq!(inst, back);
        }
    }

    #[test]
    fn effect_instance_with_masks_roundtrips_and_old_files_default_empty() {
        use crate::core::mask::{Mask, MaskShape};
        let mut inst = EffectInstance::new(EffectKind::GaussianBlur);
        let mut m = Mask::new(MaskShape::Rectangle);
        m.center = [0.3, 0.7];
        m.corner = 0.05;
        m.inverted = true;
        inst.masks.push(m);
        let json = serde_json::to_string(&inst).unwrap();
        let back: EffectInstance = serde_json::from_str(&json).unwrap();
        assert_eq!(inst, back);
        // Alte Projektdateien ohne `masks` laden mit leerer Maskenliste.
        let old = r#"{"id":"x","kind":"gaussianBlur","enabled":true,"params":[]}"#;
        let parsed: EffectInstance = serde_json::from_str(old).unwrap();
        assert!(parsed.masks.is_empty());
    }

    #[test]
    fn normalize_pads_missing_params() {
        let mut inst = EffectInstance::new(EffectKind::ChromaKey);
        inst.params.truncate(2);
        inst.normalize();
        assert_eq!(inst.params.len(), EffectKind::ChromaKey.specs().len());
        assert_eq!(inst.params[3].value, 30.0); // tolerance-Default
    }

    #[test]
    fn default_instances_are_identity_or_active_as_expected() {
        // Blur mit Stärke 0 ist Identität, Default (10) nicht.
        let r = resolved(EffectKind::GaussianBlur, &[0.0]);
        assert!(r.is_identity());
        let inst = EffectInstance::new(EffectKind::GaussianBlur);
        assert!(!inst.eval(0.0).is_identity());
        // HueSaturation neutral.
        let r = resolved(EffectKind::HueSaturation, &[0.0, 100.0, 0.0]);
        assert!(r.is_identity());
    }

    #[test]
    fn animated_effect_param_evaluates_over_time() {
        let mut inst = EffectInstance::new(EffectKind::GaussianBlur);
        inst.params[0].upsert_key(0.0, 0.0);
        inst.params[0].upsert_key(2.0, 100.0);
        assert_eq!(inst.eval(0.0).values[0], 0.0);
        assert!((inst.eval(1.0).values[0] - 50.0).abs() < 1e-9);
        assert!(has_active_video_effects(&[inst]));
    }

    #[test]
    fn invert_inverts_and_mixes() {
        let (w, h) = (4usize, 2usize);
        let mut buf = solid(w, h, [255, 0, 51, 255]);
        apply_effects_buffer(
            &mut buf,
            w,
            h,
            (0, 0, w, h),
            &[resolved(EffectKind::Invert, &[100.0])],
            2,
        );
        assert_eq!(px(&buf, w, 0, 0), [0, 255, 204, 255]);
    }

    #[test]
    fn posterize_quantizes_levels() {
        let (w, h) = (2usize, 1usize);
        let mut buf = solid(w, h, [100, 100, 100, 255]);
        apply_effects_buffer(
            &mut buf,
            w,
            h,
            (0, 0, w, h),
            &[resolved(EffectKind::Posterize, &[2.0])],
            1,
        );
        // 2 Stufen: 100/255 ≈ 0,39 → rundet auf 0.
        assert_eq!(px(&buf, w, 0, 0)[0], 0);
    }

    #[test]
    fn crop_clears_edges_keeps_center() {
        let (w, h) = (10usize, 10usize);
        let mut buf = solid(w, h, [200, 200, 200, 255]);
        apply_effects_buffer(
            &mut buf,
            w,
            h,
            (0, 0, w, h),
            &[resolved(EffectKind::Crop, &[20.0, 20.0, 20.0, 20.0, 0.0])],
            2,
        );
        assert_eq!(px(&buf, w, 0, 0)[3], 0, "Rand transparent");
        assert_eq!(px(&buf, w, 5, 5)[3], 255, "Mitte deckend");
    }

    #[test]
    fn chroma_key_removes_green_keeps_red() {
        let (w, h) = (4usize, 1usize);
        let mut buf = vec![0f32; w * h * 4];
        // Pixel 0: Greenscreen-Grün, Pixel 1: Rot.
        buf[0..4].copy_from_slice(&u8px([0, 255, 0, 255]));
        buf[4..8].copy_from_slice(&u8px([220, 30, 30, 255]));
        apply_effects_buffer(
            &mut buf,
            w,
            h,
            (0, 0, w, h),
            &[resolved(
                EffectKind::ChromaKey,
                &[0.0, 255.0, 0.0, 30.0, 10.0, 50.0],
            )],
            1,
        );
        assert_eq!(px(&buf, w, 0, 0)[3], 0, "Grün getastet");
        assert_eq!(px(&buf, w, 1, 0)[3], 255, "Rot bleibt");
    }

    #[test]
    fn luma_key_drops_dark_pixels() {
        let (w, h) = (2usize, 1usize);
        let mut buf = vec![0f32; w * h * 4];
        buf[0..4].copy_from_slice(&u8px([10, 10, 10, 255]));
        buf[4..8].copy_from_slice(&u8px([240, 240, 240, 255]));
        apply_effects_buffer(
            &mut buf,
            w,
            h,
            (0, 0, w, h),
            &[resolved(EffectKind::LumaKey, &[25.0, 10.0, 0.0])],
            1,
        );
        assert_eq!(px(&buf, w, 0, 0)[3], 0, "Dunkel weg");
        assert_eq!(px(&buf, w, 1, 0)[3], 255, "Hell bleibt");
    }

    #[test]
    fn flip_mirrors_content() {
        let (w, h) = (4usize, 2usize);
        let mut buf = solid(w, h, [0, 0, 0, 255]);
        buf[0..4].copy_from_slice(&u8px([255, 0, 0, 255])); // oben links rot
        apply_effects_buffer(
            &mut buf,
            w,
            h,
            (0, 0, w, h),
            &[resolved(EffectKind::Flip, &[1.0, 0.0])],
            1,
        );
        assert_eq!(px(&buf, w, 3, 0), [255, 0, 0, 255], "rot jetzt rechts");
        assert_eq!(px(&buf, w, 0, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn blur_preserves_flat_color_and_softens_edge() {
        // Sigma skaliert mit der Inhaltsbreite (2 % bei Stärke 100) —
        // realistische Breite, damit der Kern mehrere Pixel umfasst.
        let (w, h) = (200usize, 8usize);
        // Linke Hälfte weiß, rechte schwarz.
        let mut buf = solid(w, h, [0, 0, 0, 255]);
        for y in 0..h {
            for x in 0..w / 2 {
                let i = (y * w + x) * 4;
                buf[i..i + 4].copy_from_slice(&u8px([255, 255, 255, 255]));
            }
        }
        apply_effects_buffer(
            &mut buf,
            w,
            h,
            (0, 0, w, h),
            &[resolved(EffectKind::GaussianBlur, &[40.0])],
            2,
        );
        // Fern der Kante bleibt es (fast) weiß bzw. schwarz, an der Kante grau.
        assert!(px(&buf, w, 5, 4)[0] > 240, "links bleibt hell");
        assert!(px(&buf, w, 195, 4)[0] < 15, "rechts bleibt dunkel");
        let edge = px(&buf, w, 100, 4)[0];
        assert!((30..220).contains(&(edge as i32)), "Kante grau: {edge}");
    }

    #[test]
    fn pixelate_makes_uniform_blocks() {
        let (w, h) = (40usize, 40usize);
        let mut buf = vec![0f32; w * h * 4];
        // Vertikaler Verlauf.
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                buf[i] = (y * 6) as f32 / 255.0;
                buf[i + 3] = 1.0;
            }
        }
        apply_effects_buffer(
            &mut buf,
            w,
            h,
            (0, 0, w, h),
            &[resolved(EffectKind::Pixelate, &[100.0])],
            1,
        );
        let block = pixelate_block(100.0, w);
        assert!(block >= 2);
        // Innerhalb eines Blocks identische Werte.
        assert_eq!(px(&buf, w, 0, 0), px(&buf, w, block - 1, block - 1));
    }

    #[test]
    fn brightness_contrast_shifts_midpoint() {
        let (w, h) = (2usize, 1usize);
        let mut buf = solid(w, h, [128, 128, 128, 255]);
        apply_effects_buffer(
            &mut buf,
            w,
            h,
            (0, 0, w, h),
            &[resolved(EffectKind::BrightnessContrast, &[50.0, 0.0])],
            1,
        );
        assert!(px(&buf, w, 0, 0)[0] > 180);
    }

    #[test]
    fn hue_rotation_preserves_luma_and_neutral_at_zero() {
        let m = hue_matrix(0.0);
        // Identität bei 0°.
        assert!((m[0] - 1.0).abs() < 1e-5 && m[1].abs() < 1e-5 && m[2].abs() < 1e-5);
        // 120°-Drehung erhält Grau.
        let (w, h) = (1usize, 1usize);
        let mut buf = solid(w, h, [128, 128, 128, 255]);
        apply_effects_buffer(
            &mut buf,
            w,
            h,
            (0, 0, w, h),
            &[resolved(EffectKind::HueSaturation, &[120.0, 100.0, 0.0])],
            1,
        );
        let p = px(&buf, w, 0, 0);
        assert!((p[0] as i32 - 128).abs() <= 1 && (p[1] as i32 - 128).abs() <= 1);
    }

    #[test]
    fn effects_respect_content_rect() {
        let (w, h) = (8usize, 4usize);
        let mut buf = solid(w, h, [100, 100, 100, 255]);
        // Nur rechte Hälfte ist „Inhalt“.
        apply_effects_buffer(
            &mut buf,
            w,
            h,
            (4, 0, 4, 4),
            &[resolved(EffectKind::Invert, &[100.0])],
            2,
        );
        assert_eq!(px(&buf, w, 0, 0)[0], 100, "Padding unangetastet");
        assert_eq!(px(&buf, w, 5, 0)[0], 155, "Inhalt invertiert");
    }

    #[test]
    fn mask_limits_invert_to_region_with_feather_ramp() {
        use crate::core::mask::{Mask, MaskShape};
        // Großes graues Bild; Invert nur in einer zentralen Ellipse (harte Kante).
        let (w, h) = (40usize, 40usize);
        let mut mk = Mask::new(MaskShape::Ellipse);
        mk.center = [0.5, 0.5];
        mk.radius = [0.25, 0.25];
        mk.feather = 0.0;
        let mut buf = solid(w, h, [200, 200, 200, 255]);
        apply_effects_buffer(
            &mut buf,
            w,
            h,
            (0, 0, w, h),
            &[resolved_masked(EffectKind::Invert, &[100.0], vec![mk])],
            2,
        );
        // Zentrum invertiert (200 → 55), Ecke unberührt (200).
        assert_eq!(px(&buf, w, 20, 20)[0], 55, "Maskenzentrum invertiert");
        assert_eq!(px(&buf, w, 0, 0)[0], 200, "außerhalb der Maske unberührt");

        // Feather-Rampe: weiche Kante ⇒ Übergang zwischen voll/ungemischt.
        let mut mk2 = Mask::new(MaskShape::Ellipse);
        mk2.center = [0.5, 0.5];
        mk2.radius = [0.25, 0.25];
        mk2.feather = 0.2;
        let mut buf2 = solid(w, h, [200, 200, 200, 255]);
        apply_effects_buffer(
            &mut buf2,
            w,
            h,
            (0, 0, w, h),
            &[resolved_masked(EffectKind::Invert, &[100.0], vec![mk2])],
            2,
        );
        // Auf dem Weg vom Zentrum nach außen steigt der Rotwert monoton
        // (55 voll invertiert → 200 unberührt) — die Feather-Rampe.
        let center = px(&buf2, w, 20, 20)[0];
        let mid = px(&buf2, w, 30, 20)[0];
        let outer = px(&buf2, w, 38, 20)[0];
        assert!(center < mid && mid < outer, "Rampe: {center} < {mid} < {outer}");
    }

    #[test]
    fn parse_test_effects_builds_stack() {
        let fx = parse_test_effects("gaussianBlur:strength=40;chromaKey:tolerance=20,spill=0");
        assert_eq!(fx.len(), 2);
        assert_eq!(fx[0].kind, EffectKind::GaussianBlur);
        assert_eq!(fx[0].params[0].value, 40.0);
        assert_eq!(fx[1].kind, EffectKind::ChromaKey);
        assert_eq!(fx[1].params[3].value, 20.0);
        assert_eq!(fx[1].params[5].value, 0.0);
        assert!(parse_test_effects("unbekannt:x=1").is_empty());
    }

    #[test]
    fn audio_effects_do_not_touch_video_path() {
        let inst = EffectInstance::new(EffectKind::Reverb);
        assert!(inst.kind.is_audio());
        assert!(!has_active_video_effects(&[inst.clone()]));
        assert!(resolve_video_effects(&[inst], 0.0).is_empty());
    }

    #[test]
    fn gaussian_kernel_is_normalized() {
        let (radius, w) = gaussian_kernel(5.0);
        assert!(radius >= 12);
        let sum: f32 = w[0] + 2.0 * w[1..].iter().sum::<f32>();
        assert!((sum - 1.0).abs() < 1e-4);
    }
}
