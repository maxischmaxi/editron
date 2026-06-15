//! Audio-Effekt-DSP: verarbeitet dekodierte f32-Samples blockweise — im
//! Player-Mixdown (`core/player.rs`, vor Spur-Gain/Pan) und im Export-Audio-
//! Mix (`core/export.rs`) mit IDENTISCHEM Code, damit Vorschau und Export
//! gleich klingen. Eine [`AudioFxChain`] wird aus den aktiven Audio-Effekten
//! eines Clips ODER einer Spur gebaut; Filterzustände leben über
//! Blockgrenzen hinweg (deshalb ist das Ergebnis blockgrößen-unabhängig —
//! die Grundlage der Wiedergabe/Export-Parität). Animierte Parameter werden
//! blockweise über [`AudioFxChain::retune`] nachgeführt (Block-Rate-
//! Automation), ohne die Zustände zu verwerfen. Zipper-anfällige Stufen
//! (Gain, Limiter) glätten ihre Wirkung pro Sample.

use crate::core::effects::{EffectInstance, EffectKind};

/// dB → linearer Faktor; ≤ −60 dB gilt als −∞ (stumm). Gemeinsame Definition
/// für Player, Export und DSP — so klingen alle Pfade gleich.
#[inline]
pub fn db_to_linear(db: f64) -> f32 {
    if db <= -60.0 {
        0.0
    } else {
        10f32.powf(db as f32 / 20.0)
    }
}

/// Linear → dB (Untergrenze −120 dB).
#[inline]
fn linear_to_db(v: f32) -> f32 {
    20.0 * v.max(1e-6).log10()
}

/// Stereo-Balance (−1 = ganz links, +1 = ganz rechts): dämpft die abgewandte
/// Seite. Gemeinsame Definition für Player und Export.
#[inline]
pub fn pan_gains(pan: f64) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0) as f32;
    (1.0 - p.max(0.0), 1.0 + p.min(0.0))
}

/// Einpol-Glättungskoeffizient für eine Zeitkonstante in Millisekunden.
#[inline]
fn smoothing_coef(ms: f64, rate: u32) -> f32 {
    let ms = ms.max(0.01);
    (-(1.0 / (ms / 1000.0 * rate as f64))).exp() as f32
}

// ------------------------------------------------------------------ Biquad

/// RBJ-Biquad (Audio EQ Cookbook), Direct Form 1. `a0` ist auf 1 normiert.
#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    /// Low-Shelf mit Steilheits-Parameter `slope` (≈ Güte; 1 = maximal steil
    /// ohne Überschwingen, < 1 flacher).
    fn set_low_shelf(&mut self, rate: u32, freq: f64, gain_db: f64, slope: f64) {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * freq.clamp(1.0, rate as f64 / 2.0) / rate as f64;
        let (sin, cos) = w0.sin_cos();
        let s = slope.clamp(0.05, 5.0);
        let alpha = sin / 2.0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).max(0.0).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        let a0 = (a + 1.0) + (a - 1.0) * cos + two_sqrt_a_alpha;
        self.b0 = ((a * ((a + 1.0) - (a - 1.0) * cos + two_sqrt_a_alpha)) / a0) as f32;
        self.b1 = ((2.0 * a * ((a - 1.0) - (a + 1.0) * cos)) / a0) as f32;
        self.b2 = ((a * ((a + 1.0) - (a - 1.0) * cos - two_sqrt_a_alpha)) / a0) as f32;
        self.a1 = ((-2.0 * ((a - 1.0) + (a + 1.0) * cos)) / a0) as f32;
        self.a2 = (((a + 1.0) + (a - 1.0) * cos - two_sqrt_a_alpha) / a0) as f32;
    }

    fn set_high_shelf(&mut self, rate: u32, freq: f64, gain_db: f64, slope: f64) {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * freq.clamp(1.0, rate as f64 / 2.0) / rate as f64;
        let (sin, cos) = w0.sin_cos();
        let s = slope.clamp(0.05, 5.0);
        let alpha = sin / 2.0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).max(0.0).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        let a0 = (a + 1.0) - (a - 1.0) * cos + two_sqrt_a_alpha;
        self.b0 = ((a * ((a + 1.0) + (a - 1.0) * cos + two_sqrt_a_alpha)) / a0) as f32;
        self.b1 = ((-2.0 * a * ((a - 1.0) + (a + 1.0) * cos)) / a0) as f32;
        self.b2 = ((a * ((a + 1.0) + (a - 1.0) * cos - two_sqrt_a_alpha)) / a0) as f32;
        self.a1 = ((2.0 * ((a - 1.0) - (a + 1.0) * cos)) / a0) as f32;
        self.a2 = (((a + 1.0) - (a - 1.0) * cos - two_sqrt_a_alpha) / a0) as f32;
    }

    fn set_peaking(&mut self, rate: u32, freq: f64, gain_db: f64, q: f64) {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * freq.clamp(1.0, rate as f64 / 2.0) / rate as f64;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q.max(0.01));
        let a0 = 1.0 + alpha / a;
        self.b0 = ((1.0 + alpha * a) / a0) as f32;
        self.b1 = ((-2.0 * cos) / a0) as f32;
        self.b2 = ((1.0 - alpha * a) / a0) as f32;
        self.a1 = ((-2.0 * cos) / a0) as f32;
        self.a2 = ((1.0 - alpha / a) / a0) as f32;
    }

    fn set_highpass(&mut self, rate: u32, freq: f64, q: f64) {
        let w0 = 2.0 * std::f64::consts::PI * freq.clamp(1.0, rate as f64 / 2.0 - 1.0) / rate as f64;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q.max(0.01));
        let a0 = 1.0 + alpha;
        self.b0 = (((1.0 + cos) / 2.0) / a0) as f32;
        self.b1 = ((-(1.0 + cos)) / a0) as f32;
        self.b2 = (((1.0 + cos) / 2.0) / a0) as f32;
        self.a1 = ((-2.0 * cos) / a0) as f32;
        self.a2 = ((1.0 - alpha) / a0) as f32;
    }

    fn set_lowpass(&mut self, rate: u32, freq: f64, q: f64) {
        let w0 = 2.0 * std::f64::consts::PI * freq.clamp(1.0, rate as f64 / 2.0 - 1.0) / rate as f64;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q.max(0.01));
        let a0 = 1.0 + alpha;
        self.b0 = (((1.0 - cos) / 2.0) / a0) as f32;
        self.b1 = ((1.0 - cos) / a0) as f32;
        self.b2 = (((1.0 - cos) / 2.0) / a0) as f32;
        self.a1 = ((-2.0 * cos) / a0) as f32;
        self.a2 = ((1.0 - alpha) / a0) as f32;
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        // Selbstheilend: ein nicht-finites Ergebnis darf den Filterzustand
        // (x1/x2/y1/y2 in der Rückkopplung) nicht dauerhaft vergiften.
        let y = if y.is_finite() { y } else { 0.0 };
        let x = if x.is_finite() { x } else { 0.0 };
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    /// Betrag des Frequenzgangs |H(e^jω)| bei `freq` (linear). Grundlage der
    /// EQ-Kurvenvisualisierung und der Frequenzgang-Tests.
    fn magnitude(&self, freq: f64, rate: u32) -> f64 {
        let w = 2.0 * std::f64::consts::PI * freq / rate as f64;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        // Zähler: b0 + b1 e^{-jw} + b2 e^{-2jw}
        let nr = self.b0 as f64 + self.b1 as f64 * c1 + self.b2 as f64 * c2;
        let ni = -(self.b1 as f64 * s1) - self.b2 as f64 * s2;
        // Nenner: 1 + a1 e^{-jw} + a2 e^{-2jw}
        let dr = 1.0 + self.a1 as f64 * c1 + self.a2 as f64 * c2;
        let di = -(self.a1 as f64 * s1) - self.a2 as f64 * s2;
        let num = (nr * nr + ni * ni).sqrt();
        let den = (dr * dr + di * di).sqrt().max(1e-12);
        num / den
    }
}

// ----------------------------------------------------------- EQ-Frequenzgang

/// Die vier EQ-Bänder eines [`EffectKind::Equalizer`] aus den ausgewerteten
/// Parameterwerten lesen. Reihenfolge: (Frequenz, Gain dB, Q/Slope) je Band.
/// Band 0 = Low-Shelf, 1+2 = Glocken, 3 = High-Shelf.
fn eq_band_biquads(values: &[f64], rate: u32) -> [Biquad; 4] {
    let v = |i: usize| values.get(i).copied().unwrap_or(0.0);
    let mut b = [Biquad::default(); 4];
    b[0].set_low_shelf(rate, v(0), v(1), v(2));
    b[1].set_peaking(rate, v(3), v(4), v(5));
    b[2].set_peaking(rate, v(6), v(7), v(8));
    b[3].set_high_shelf(rate, v(9), v(10), v(11));
    b
}

/// Kombinierter EQ-Frequenzgang in dB bei `freq` für die 12 EQ-Werte
/// (gleiche Filter wie der DSP-Pfad → die Kurve zeigt exakt, was zu hören
/// ist). Genutzt von der Kurvenvisualisierung und den Frequenzgang-Tests.
pub fn eq_response_db(values: &[f64], rate: u32, freq: f64) -> f64 {
    eq_band_biquads(values, rate)
        .iter()
        .map(|b| 20.0 * b.magnitude(freq, rate).max(1e-9).log10())
        .sum()
}

// ----------------------------------------------------------------- Stages

const MAX_CHANNELS: usize = 2;
/// Maximale Echo-Zeit (Puffergröße) in Sekunden.
const MAX_DELAY_SECS: f64 = 2.0;

/// Kammfilter des Halls (Freeverb-Schema: Dämpfungs-Tiefpass in der
/// Rückkopplung).
struct Comb {
    buf: Vec<f32>,
    pos: usize,
    store: f32,
}

impl Comb {
    fn new(len: usize) -> Comb {
        Comb {
            buf: vec![0.0; len.max(1)],
            pos: 0,
            store: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        let out = self.buf[self.pos];
        self.store = out * (1.0 - damp) + self.store * damp;
        self.buf[self.pos] = x + self.store * feedback;
        self.pos = (self.pos + 1) % self.buf.len();
        out
    }
}

/// Allpass-Diffusor des Halls.
struct Allpass {
    buf: Vec<f32>,
    pos: usize,
}

impl Allpass {
    fn new(len: usize) -> Allpass {
        Allpass {
            buf: vec![0.0; len.max(1)],
            pos: 0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let b = self.buf[self.pos];
        let out = b - x;
        self.buf[self.pos] = x + b * 0.5;
        self.pos = (self.pos + 1) % self.buf.len();
        out
    }
}

/// Freeverb-Basisverzögerungen (Samples bei 44,1 kHz), pro Kanal versetzt.
const COMB_LENS: [usize; 4] = [1116, 1188, 1277, 1356];
const ALLPASS_LENS: [usize; 2] = [556, 441];
const STEREO_SPREAD: usize = 23;

enum Stage {
    /// 4-Band parametrischer EQ: Low-Shelf, 2× Glocke, High-Shelf.
    Eq {
        bands: [[Biquad; MAX_CHANNELS]; 4],
    },
    Compressor {
        threshold_db: f32,
        ratio: f32,
        attack: f32,
        release: f32,
        makeup: f32,
        /// Geglätteter Pegel (linear) und geglättete Gain-Reduktion (dB).
        env: f32,
        gr_db: f32,
    },
    /// Brick-Wall-Limiter ohne Lookahead (Latenz unverändert): sofortiger
    /// Attack hält den Pegel unter der Ceiling, sanfter Release vermeidet
    /// Pumpen. Master-tauglich.
    Limiter {
        ceiling: f32,
        release: f32,
        gain: f32,
        gr_db: f32,
    },
    Highpass {
        bq: [Biquad; MAX_CHANNELS],
    },
    Lowpass {
        bq: [Biquad; MAX_CHANNELS],
    },
    /// Verstärkung mit Parameter-Glättung (zipper-frei).
    Gain {
        target: f32,
        current: f32,
    },
    Reverb {
        combs: Vec<Vec<Comb>>,
        allpasses: Vec<Vec<Allpass>>,
        feedback: f32,
        damp: f32,
        wet: f32,
    },
    Gate {
        threshold_db: f32,
        attack: f32,
        release: f32,
        env: f32,
        gain: f32,
    },
    Delay {
        buf: Vec<Vec<f32>>,
        pos: usize,
        delay_samples: usize,
        feedback: f32,
        wet: f32,
    },
}

/// Eine Effekt-Stufe der Kette: Instanz-Identität + DSP-Zustand.
struct ChainStage {
    fx_id: String,
    kind: EffectKind,
    stage: Stage,
}

/// DSP-Kette eines Clips oder einer Spur (Reihenfolge = Effekt-Stapel).
pub struct AudioFxChain {
    rate: u32,
    channels: usize,
    stages: Vec<ChainStage>,
}

impl AudioFxChain {
    /// Kette aus den aktiven Audio-Effekten bauen; None bei leerer Liste.
    pub fn build(
        effects: &[&EffectInstance],
        rate: u32,
        channels: usize,
        media_t: f64,
    ) -> Option<AudioFxChain> {
        let audio: Vec<&&EffectInstance> = effects
            .iter()
            .filter(|e| e.enabled && e.kind.is_audio())
            .collect();
        if audio.is_empty() {
            return None;
        }
        let channels = channels.clamp(1, MAX_CHANNELS);
        let mut chain = AudioFxChain {
            rate,
            channels,
            stages: audio
                .iter()
                .map(|inst| ChainStage {
                    fx_id: inst.id.clone(),
                    kind: inst.kind,
                    stage: new_stage(inst.kind, rate, channels),
                })
                .collect(),
        };
        chain.retune(effects, media_t);
        Some(chain)
    }

    /// Passt die Kette strukturell zu diesen Effekten? (Gleiche Instanzen in
    /// gleicher Reihenfolge — sonst neu bauen.)
    pub fn matches(&self, effects: &[&EffectInstance]) -> bool {
        let audio: Vec<&&EffectInstance> = effects
            .iter()
            .filter(|e| e.enabled && e.kind.is_audio())
            .collect();
        audio.len() == self.stages.len()
            && audio
                .iter()
                .zip(&self.stages)
                .all(|(inst, st)| inst.id == st.fx_id && inst.kind == st.kind)
    }

    /// (fx_id, Gain-Reduktion dB) aller Dynamikstufen (Kompressor/Limiter) —
    /// der Player sammelt sie nach jedem Block für die Live-Meter.
    pub fn dynamic_gain_reductions(&self) -> Vec<(String, f32)> {
        self.stages
            .iter()
            .filter_map(|s| match &s.stage {
                Stage::Compressor { gr_db, .. } => Some((s.fx_id.clone(), *gr_db)),
                Stage::Limiter { gr_db, .. } => Some((s.fx_id.clone(), *gr_db)),
                _ => None,
            })
            .collect()
    }

    /// Parameter zur (Medien- bzw. Sequenz-)Zeit nachführen (Filterzustände
    /// bleiben). Für Clip-Effekte ist `t` die Medienzeit, für Spur-Effekte
    /// die Sequenzzeit.
    pub fn retune(&mut self, effects: &[&EffectInstance], t: f64) {
        let rate = self.rate;
        for stage in &mut self.stages {
            let Some(inst) = effects.iter().find(|e| e.id == stage.fx_id) else {
                continue;
            };
            let r = inst.eval(t);
            let v = |i: usize| r.values.get(i).copied().unwrap_or(0.0);
            match &mut stage.stage {
                Stage::Eq { bands } => {
                    let bq = eq_band_biquads(&r.values, rate);
                    for ch in 0..MAX_CHANNELS {
                        for (i, b) in bq.iter().enumerate() {
                            // Koeffizienten übernehmen, Zustand behalten.
                            let st = &mut bands[i][ch];
                            st.b0 = b.b0;
                            st.b1 = b.b1;
                            st.b2 = b.b2;
                            st.a1 = b.a1;
                            st.a2 = b.a2;
                        }
                    }
                }
                Stage::Compressor {
                    threshold_db,
                    ratio,
                    attack,
                    release,
                    makeup,
                    ..
                } => {
                    *threshold_db = v(0) as f32;
                    *ratio = (v(1).max(1.0)) as f32;
                    *attack = smoothing_coef(v(2), rate);
                    *release = smoothing_coef(v(3), rate);
                    *makeup = db_to_linear(v(4));
                }
                Stage::Limiter {
                    ceiling, release, ..
                } => {
                    *ceiling = db_to_linear(v(0)).max(1e-4);
                    *release = smoothing_coef(v(1), rate);
                }
                Stage::Highpass { bq } => {
                    for b in bq.iter_mut() {
                        b.set_highpass(rate, v(0), v(1));
                    }
                }
                Stage::Lowpass { bq } => {
                    for b in bq.iter_mut() {
                        b.set_lowpass(rate, v(0), v(1));
                    }
                }
                Stage::Gain { target, .. } => {
                    *target = db_to_linear(v(0));
                }
                Stage::Reverb {
                    feedback,
                    damp,
                    wet,
                    ..
                } => {
                    *feedback = 0.7 + (v(0) / 100.0).clamp(0.0, 1.0) as f32 * 0.28;
                    *damp = (v(1) / 100.0).clamp(0.0, 1.0) as f32 * 0.5;
                    *wet = (v(2) / 100.0).clamp(0.0, 1.0) as f32;
                }
                Stage::Gate {
                    threshold_db,
                    attack,
                    release,
                    ..
                } => {
                    *threshold_db = v(0) as f32;
                    *attack = smoothing_coef(5.0, rate);
                    *release = smoothing_coef(v(1), rate);
                }
                Stage::Delay {
                    delay_samples,
                    feedback,
                    wet,
                    buf,
                    ..
                } => {
                    // Gegen leeren/zu kurzen Puffer absichern (max - 1 würde sonst
                    // bei len 0 in usize unterlaufen).
                    let max = buf.first().map(|b| b.len()).unwrap_or(0);
                    if max >= 2 {
                        *delay_samples =
                            ((v(0) / 1000.0 * rate as f64) as usize).clamp(1, max - 1);
                    }
                    *feedback = (v(1) / 100.0).clamp(0.0, 0.95) as f32;
                    *wet = (v(2) / 100.0).clamp(0.0, 1.0) as f32;
                }
            }
        }
    }

    /// Interleaved-Samples in place verarbeiten (`channels` wie beim Bauen).
    pub fn process(&mut self, samples: &mut [f32]) {
        let ch = self.channels;
        if ch == 0 || samples.is_empty() {
            return;
        }
        // Nicht-finite Eingangssamples (Decoder-Glitch, vorheriger Effekt) würden
        // Filter-/Hüllkurven-/Delay-Zustände dauerhaft vergiften → am Eingang auf
        // 0 flushen, damit ein einzelnes NaN nicht die ganze Spur stummschaltet.
        for s in samples.iter_mut() {
            if !s.is_finite() {
                *s = 0.0;
            }
        }
        for stage in &mut self.stages {
            match &mut stage.stage {
                Stage::Eq { bands } => {
                    for frame in samples.chunks_exact_mut(ch) {
                        for (c, s) in frame.iter_mut().enumerate() {
                            let mut y = *s;
                            for band in bands.iter_mut() {
                                y = band[c].process(y);
                            }
                            *s = y;
                        }
                    }
                }
                Stage::Compressor {
                    threshold_db,
                    ratio,
                    attack,
                    release,
                    makeup,
                    env,
                    gr_db,
                } => {
                    for frame in samples.chunks_exact_mut(ch) {
                        let level = frame.iter().fold(0f32, |m, s| m.max(s.abs()));
                        // Pegelfolger: schneller Anstieg, Release-Abfall.
                        let coef = if level > *env { *attack } else { *release };
                        *env = level + (*env - level) * coef;
                        let env_db = linear_to_db(*env);
                        let over = (env_db - *threshold_db).max(0.0);
                        let target_gr = over * (1.0 - 1.0 / *ratio);
                        let coef = if target_gr > *gr_db { *attack } else { *release };
                        *gr_db = target_gr + (*gr_db - target_gr) * coef;
                        let gain = db_to_linear(-*gr_db as f64) * *makeup;
                        for s in frame.iter_mut() {
                            *s *= gain;
                        }
                    }
                }
                Stage::Limiter {
                    ceiling,
                    release,
                    gain,
                    gr_db,
                } => {
                    for frame in samples.chunks_exact_mut(ch) {
                        let level = frame.iter().fold(0f32, |m, s| m.max(s.abs()));
                        // Nötige Verstärkung, um unter der Ceiling zu bleiben.
                        let target = if level * *gain > *ceiling {
                            (*ceiling / level.max(1e-9)).min(1.0)
                        } else {
                            1.0
                        };
                        // Sofortiger Attack (Gain fällt), sanfter Release.
                        if target < *gain {
                            *gain = target;
                        } else {
                            *gain = target + (*gain - target) * *release;
                        }
                        *gr_db = -linear_to_db(*gain);
                        for s in frame.iter_mut() {
                            *s = (*s * *gain).clamp(-*ceiling, *ceiling);
                        }
                    }
                }
                Stage::Highpass { bq } => {
                    for frame in samples.chunks_exact_mut(ch) {
                        for (c, s) in frame.iter_mut().enumerate() {
                            *s = bq[c].process(*s);
                        }
                    }
                }
                Stage::Lowpass { bq } => {
                    for frame in samples.chunks_exact_mut(ch) {
                        for (c, s) in frame.iter_mut().enumerate() {
                            *s = bq[c].process(*s);
                        }
                    }
                }
                Stage::Gain { target, current } => {
                    // Pro Sample zum Ziel gleiten — kein Zipper bei Sprüngen.
                    let coef = smoothing_coef(5.0, self.rate);
                    for frame in samples.chunks_exact_mut(ch) {
                        *current = *target + (*current - *target) * coef;
                        for s in frame.iter_mut() {
                            *s *= *current;
                        }
                    }
                }
                Stage::Reverb {
                    combs,
                    allpasses,
                    feedback,
                    damp,
                    wet,
                } => {
                    for frame in samples.chunks_exact_mut(ch) {
                        // Mono-Einspeisung (Summe), Ausgänge pro Kanal.
                        let input = frame.iter().sum::<f32>() / ch as f32;
                        for (c, s) in frame.iter_mut().enumerate() {
                            let mut acc = 0.0;
                            for comb in &mut combs[c] {
                                acc += comb.process(input, *feedback, *damp);
                            }
                            let mut w = acc / COMB_LENS.len() as f32;
                            for ap in &mut allpasses[c] {
                                w = ap.process(w);
                            }
                            *s = *s * (1.0 - *wet) + w * *wet;
                        }
                    }
                }
                Stage::Gate {
                    threshold_db,
                    attack,
                    release,
                    env,
                    gain,
                } => {
                    for frame in samples.chunks_exact_mut(ch) {
                        let level = frame.iter().fold(0f32, |m, s| m.max(s.abs()));
                        let coef = if level > *env { *attack } else { *release };
                        *env = level + (*env - level) * coef;
                        let target = if linear_to_db(*env) >= *threshold_db {
                            1.0
                        } else {
                            0.0
                        };
                        let coef = if target > *gain { *attack } else { *release };
                        *gain = target + (*gain - target) * coef;
                        for s in frame.iter_mut() {
                            *s *= *gain;
                        }
                    }
                }
                Stage::Delay {
                    buf,
                    pos,
                    delay_samples,
                    feedback,
                    wet,
                } => {
                    let len = buf[0].len();
                    for frame in samples.chunks_exact_mut(ch) {
                        let read = (*pos + len - *delay_samples) % len;
                        for (c, s) in frame.iter_mut().enumerate() {
                            let delayed = buf[c][read];
                            buf[c][*pos] = *s + delayed * *feedback;
                            *s = *s * (1.0 - *wet * 0.5) + delayed * *wet;
                        }
                        *pos = (*pos + 1) % len;
                    }
                }
            }
        }
    }
}

fn new_stage(kind: EffectKind, rate: u32, channels: usize) -> Stage {
    let scale = rate as f64 / 44100.0;
    let scaled = |base: usize, spread: usize| ((base + spread) as f64 * scale) as usize;
    match kind {
        EffectKind::Equalizer => Stage::Eq {
            bands: Default::default(),
        },
        EffectKind::Compressor => Stage::Compressor {
            threshold_db: -18.0,
            ratio: 4.0,
            attack: smoothing_coef(10.0, rate),
            release: smoothing_coef(150.0, rate),
            makeup: 1.0,
            env: 0.0,
            gr_db: 0.0,
        },
        EffectKind::Limiter => Stage::Limiter {
            ceiling: db_to_linear(-1.0),
            release: smoothing_coef(50.0, rate),
            gain: 1.0,
            gr_db: 0.0,
        },
        EffectKind::Highpass => Stage::Highpass {
            bq: Default::default(),
        },
        EffectKind::Lowpass => Stage::Lowpass {
            bq: Default::default(),
        },
        EffectKind::Gain => Stage::Gain {
            target: 1.0,
            current: 1.0,
        },
        EffectKind::Reverb => Stage::Reverb {
            combs: (0..channels)
                .map(|c| {
                    COMB_LENS
                        .iter()
                        .map(|l| Comb::new(scaled(*l, c * STEREO_SPREAD)))
                        .collect()
                })
                .collect(),
            allpasses: (0..channels)
                .map(|c| {
                    ALLPASS_LENS
                        .iter()
                        .map(|l| Allpass::new(scaled(*l, c * STEREO_SPREAD)))
                        .collect()
                })
                .collect(),
            feedback: 0.84,
            damp: 0.25,
            wet: 0.3,
        },
        EffectKind::NoiseGate => Stage::Gate {
            threshold_db: -50.0,
            attack: smoothing_coef(5.0, rate),
            release: smoothing_coef(120.0, rate),
            env: 0.0,
            gain: 1.0,
        },
        EffectKind::Delay => Stage::Delay {
            buf: (0..channels)
                .map(|_| vec![0.0; (MAX_DELAY_SECS * rate as f64) as usize])
                .collect(),
            pos: 0,
            delay_samples: (0.35 * rate as f64) as usize,
            feedback: 0.35,
            wet: 0.4,
        },
        // Video-Effekte landen nie in der Audio-Kette.
        _ => Stage::Gain {
            target: 1.0,
            current: 1.0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::animation::AnimatedParam;
    use crate::core::effects::EffectInstance;

    const RATE: u32 = 48000;

    fn sine(freq: f64, frames: usize) -> Vec<f32> {
        // Stereo interleaved.
        (0..frames)
            .flat_map(|i| {
                let v = (2.0 * std::f64::consts::PI * freq * i as f64 / RATE as f64).sin() as f32;
                [v, v]
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    /// Gemessene Verstärkung eines reinen Sinus durch die Kette (Faktor).
    fn measured_gain(chain: &mut AudioFxChain, freq: f64) -> f32 {
        let mut buf = sine(freq, RATE as usize);
        chain.process(&mut buf);
        // Zweite Hälfte (Einschwingen vorbei).
        rms(&buf[RATE as usize..]) / (1.0 / 2f32.sqrt())
    }

    fn instance(kind: EffectKind, values: &[(usize, f64)]) -> EffectInstance {
        let mut inst = EffectInstance::new(kind);
        for (i, v) in values {
            inst.params[*i] = AnimatedParam::fixed(*v);
        }
        inst
    }

    fn eq(values: &[(usize, f64)]) -> EffectInstance {
        instance(EffectKind::Equalizer, values)
    }

    #[test]
    fn nan_input_does_not_permanently_poison_the_chain() {
        // EQ-Filter (IIR, Rückkopplung) — ein einzelnes NaN/Inf darf den
        // Zustand nicht dauerhaft vergiften und die Spur stummschalten.
        let inst = eq(&[(0, 100.0), (1, 6.0)]);
        let fx = [&inst];
        let mut chain = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        // Block aus NaN/Inf durchschicken.
        let mut bad = vec![f32::NAN; 256];
        for (i, s) in bad.iter_mut().enumerate() {
            if i % 3 == 0 {
                *s = f32::INFINITY;
            }
        }
        chain.process(&mut bad);
        assert!(bad.iter().all(|s| s.is_finite()), "Ausgabe bleibt finit");
        // Danach normales Audio: muss wieder durchkommen (nicht stumm).
        let mut good = sine(440.0, RATE as usize);
        chain.process(&mut good);
        let tail = rms(&good[RATE as usize / 2..]);
        assert!(tail > 0.1 && tail.is_finite(), "Audio nach NaN wieder hörbar: {tail}");
    }

    #[test]
    fn eq_low_shelf_boosts_bass_not_treble() {
        // Low-Shelf 100 Hz, +12 dB (Indizes: 0=freq,1=gain,2=Q).
        let inst = eq(&[(0, 100.0), (1, 12.0)]);
        let fx = [&inst];
        let mut bass = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let low_gain = measured_gain(&mut bass, 50.0);
        let mut treble = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let high_gain = measured_gain(&mut treble, 10000.0);
        assert!(low_gain > 3.0, "Bass deutlich angehoben: {low_gain}");
        assert!((0.9..1.1).contains(&high_gain), "Höhen neutral: {high_gain}");
    }

    #[test]
    fn eq_bell_boosts_center_frequency() {
        // Glocke 1 (Indizes 3=freq,4=gain,5=Q) auf 1 kHz, +12 dB, Q=2.
        let inst = eq(&[(3, 1000.0), (4, 12.0), (5, 2.0)]);
        let fx = [&inst];
        let mut at1k = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let g1k = measured_gain(&mut at1k, 1000.0);
        let mut at100 = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let g100 = measured_gain(&mut at100, 100.0);
        assert!(g1k > 3.0, "1 kHz angehoben: {g1k}");
        assert!((0.9..1.1).contains(&g100), "100 Hz unberührt: {g100}");
    }

    #[test]
    fn eq_response_matches_measured_gain() {
        // Die Visualisierungskurve muss dem gemessenen DSP-Gang entsprechen.
        let values: Vec<f64> = {
            let inst = eq(&[(0, 120.0), (1, 6.0), (6, 3000.0), (7, -9.0), (8, 3.0)]);
            inst.eval(0.0).values
        };
        for f in [60.0, 500.0, 3000.0, 12000.0] {
            let inst = eq(&[(0, 120.0), (1, 6.0), (6, 3000.0), (7, -9.0), (8, 3.0)]);
            let fx = [&inst];
            let mut chain = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
            let measured_db = 20.0 * measured_gain(&mut chain, f).log10();
            let predicted_db = eq_response_db(&values, RATE, f);
            assert!(
                (measured_db as f64 - predicted_db).abs() < 1.0,
                "{f} Hz: gemessen {measured_db:.2} dB vs. Kurve {predicted_db:.2} dB"
            );
        }
    }

    #[test]
    fn highpass_attenuates_low_passes_high() {
        let inst = instance(EffectKind::Highpass, &[(0, 1000.0), (1, 0.71)]);
        let fx = [&inst];
        let mut low = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let glow = measured_gain(&mut low, 100.0);
        let mut high = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let ghigh = measured_gain(&mut high, 8000.0);
        assert!(glow < 0.2, "100 Hz stark gedämpft: {glow}");
        assert!(ghigh > 0.85, "8 kHz passiert: {ghigh}");
    }

    #[test]
    fn lowpass_passes_low_attenuates_high() {
        let inst = instance(EffectKind::Lowpass, &[(0, 1000.0), (1, 0.71)]);
        let fx = [&inst];
        let mut low = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let glow = measured_gain(&mut low, 100.0);
        let mut high = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let ghigh = measured_gain(&mut high, 8000.0);
        assert!(glow > 0.85, "100 Hz passiert: {glow}");
        assert!(ghigh < 0.2, "8 kHz stark gedämpft: {ghigh}");
    }

    #[test]
    fn gain_scales_by_db() {
        let inst = instance(EffectKind::Gain, &[(0, 6.0)]);
        let fx = [&inst];
        let mut chain = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let g = measured_gain(&mut chain, 440.0);
        // +6 dB ≈ Faktor 1,995.
        assert!((g - 1.995).abs() < 0.05, "+6 dB: {g}");
    }

    #[test]
    fn limiter_keeps_output_below_ceiling() {
        // Ceiling −6 dB (Faktor ≈ 0,501).
        let inst = instance(EffectKind::Limiter, &[(0, -6.0), (1, 50.0)]);
        let fx = [&inst];
        let mut chain = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        // 0-dBFS-Sinus → muss unter der Ceiling bleiben.
        let mut buf = sine(220.0, RATE as usize);
        chain.process(&mut buf);
        let ceiling = db_to_linear(-6.0);
        let peak = buf[RATE as usize / 2..]
            .iter()
            .fold(0f32, |m, s| m.max(s.abs()));
        assert!(
            peak <= ceiling + 1e-3,
            "Peak {peak} ≤ Ceiling {ceiling}"
        );
        assert!(peak > ceiling * 0.5, "aber nicht übermäßig gedämpft: {peak}");
    }

    #[test]
    fn compressor_reduces_loud_signal_and_reports_gr() {
        let inst = instance(
            EffectKind::Compressor,
            &[(0, -30.0), (1, 10.0), (2, 1.0), (3, 50.0)],
        );
        let fx = [&inst];
        let mut chain = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let mut loud = sine(440.0, 48000);
        chain.process(&mut loud);
        let out = rms(&loud[24000..]);
        assert!(out < 0.3, "0 dBFS-Sinus stark komprimiert: {out}");
        let gr = chain
            .dynamic_gain_reductions()
            .iter()
            .find(|(id, _)| id == &inst.id)
            .map(|(_, g)| *g)
            .unwrap();
        assert!(gr > 3.0, "Gain-Reduktion gemeldet: {gr}");
    }

    #[test]
    fn gate_silences_quiet_signal_keeps_loud() {
        let inst = instance(EffectKind::NoiseGate, &[(0, -30.0), (1, 50.0)]);
        let fx = [&inst];
        let mut chain = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let mut quiet: Vec<f32> = sine(440.0, 24000).iter().map(|s| s * 0.005).collect();
        chain.process(&mut quiet);
        assert!(rms(&quiet[12000..]) < 0.001, "Leises gemutet");
        let mut chain = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let mut loud = sine(440.0, 24000);
        chain.process(&mut loud);
        assert!(rms(&loud[12000..]) > 0.5, "Lautes bleibt");
    }

    #[test]
    fn reverb_produces_tail_after_impulse() {
        let inst = instance(EffectKind::Reverb, &[(0, 80.0), (2, 100.0)]);
        let fx = [&inst];
        let mut chain = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let mut buf = vec![0f32; 48000 * 2];
        buf[0] = 1.0;
        buf[1] = 1.0;
        chain.process(&mut buf);
        let tail = &buf[24000..];
        assert!(tail.iter().any(|s| s.abs() > 1e-4), "Hallfahne vorhanden");
    }

    #[test]
    fn delay_echoes_after_delay_time() {
        let inst = instance(EffectKind::Delay, &[(0, 100.0), (1, 0.0), (2, 100.0)]);
        let fx = [&inst];
        let mut chain = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let mut buf = vec![0f32; 19200 * 2]; // 400 ms
        buf[0] = 1.0;
        buf[1] = 1.0;
        chain.process(&mut buf);
        // Echo nach 100 ms = Frame 4800.
        let echo = buf[4800 * 2].abs();
        assert!(echo > 0.5, "Echo bei 100 ms: {echo}");
    }

    #[test]
    fn block_size_invariance_guarantees_player_export_parity() {
        // Wiedergabe (große Blöcke) und Export (kleine Blöcke) nutzen dieselbe
        // Kette — bei statischen Parametern muss das Ergebnis identisch sein,
        // egal wie die Samples in Blöcke zerschnitten werden.
        let inst = eq(&[(1, 6.0), (4, -4.0), (10, 3.0)]);
        let comp = instance(EffectKind::Compressor, &[(0, -24.0), (1, 4.0)]);
        let fx = [&inst, &comp];
        let signal = sine(440.0, 10000);

        let mut whole = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let mut a = signal.clone();
        whole.process(&mut a);

        let mut chunked = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let mut b = signal.clone();
        // In ungleichmäßige Blöcke zerlegen (Frames → Samples ×2).
        let mut off = 0;
        for block in [37, 256, 1, 999, 4096] {
            let n = (block * 2).min(b.len() - off);
            if n == 0 {
                break;
            }
            chunked.process(&mut b[off..off + n]);
            off += n;
        }
        if off < b.len() {
            chunked.process(&mut b[off..]);
        }

        for (x, y) in a.iter().zip(&b) {
            assert!((x - y).abs() < 1e-5, "Blockgrößen-Invarianz: {x} vs {y}");
        }
    }

    #[test]
    fn chain_matches_and_retunes_without_rebuild() {
        let inst = eq(&[(1, 6.0)]);
        let fx = [&inst];
        let mut chain = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        assert!(chain.matches(&fx));
        let other = instance(EffectKind::Compressor, &[]);
        let fx2 = [&other];
        assert!(!chain.matches(&fx2));
        // Retune ändert Parameter ohne Panik.
        chain.retune(&fx, 1.0);
    }

    #[test]
    fn disabled_and_video_effects_yield_no_chain() {
        let mut inst = instance(EffectKind::Reverb, &[]);
        inst.enabled = false;
        let video = instance(EffectKind::GaussianBlur, &[]);
        assert!(AudioFxChain::build(&[&inst, &video], RATE, 2, 0.0).is_none());
    }
}
