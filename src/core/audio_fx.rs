//! Audio-Effekt-DSP: verarbeitet dekodierte f32-Samples pro Clip — im
//! Player-Mixdown (`core/player.rs`, vor Gain/Pan) und im Export-Audio-Mix
//! (`core/export.rs`) mit IDENTISCHEM Code, damit Vorschau und Export gleich
//! klingen. Eine [`AudioFxChain`] wird aus den aktiven Audio-Effekten eines
//! Clips gebaut; Filterzustände leben über Blockgrenzen hinweg. Animierte
//! Parameter werden blockweise über [`AudioFxChain::retune`] nachgeführt
//! (Block-Rate-Automation), ohne die Zustände zu verwerfen.

use crate::core::effects::{EffectInstance, EffectKind};

/// dB → linearer Faktor.
#[inline]
fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Linear → dB (Untergrenze −120 dB).
#[inline]
fn linear_to_db(v: f32) -> f32 {
    20.0 * v.max(1e-6).log10()
}

/// Einpol-Glättungskoeffizient für eine Zeitkonstante in Millisekunden.
#[inline]
fn smoothing_coef(ms: f64, rate: u32) -> f32 {
    let ms = ms.max(0.01);
    (-(1.0 / (ms / 1000.0 * rate as f64))).exp() as f32
}

// ------------------------------------------------------------------ Biquad

/// RBJ-Biquad (Audio EQ Cookbook), Direct Form 1.
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
    fn set_low_shelf(&mut self, rate: u32, freq: f64, gain_db: f64) {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * freq / rate as f64;
        let (sin, cos) = w0.sin_cos();
        // Shelf-Steilheit S = 1.
        let alpha = sin / 2.0 * (2.0f64).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        let a0 = (a + 1.0) + (a - 1.0) * cos + two_sqrt_a_alpha;
        self.b0 = ((a * ((a + 1.0) - (a - 1.0) * cos + two_sqrt_a_alpha)) / a0) as f32;
        self.b1 = ((2.0 * a * ((a - 1.0) - (a + 1.0) * cos)) / a0) as f32;
        self.b2 = ((a * ((a + 1.0) - (a - 1.0) * cos - two_sqrt_a_alpha)) / a0) as f32;
        self.a1 = ((-2.0 * ((a - 1.0) + (a + 1.0) * cos)) / a0) as f32;
        self.a2 = (((a + 1.0) + (a - 1.0) * cos - two_sqrt_a_alpha) / a0) as f32;
    }

    fn set_high_shelf(&mut self, rate: u32, freq: f64, gain_db: f64) {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * freq / rate as f64;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / 2.0 * (2.0f64).sqrt();
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
        let w0 = 2.0 * std::f64::consts::PI * freq / rate as f64;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha / a;
        self.b0 = ((1.0 + alpha * a) / a0) as f32;
        self.b1 = ((-2.0 * cos) / a0) as f32;
        self.b2 = ((1.0 - alpha * a) / a0) as f32;
        self.a1 = ((-2.0 * cos) / a0) as f32;
        self.a2 = ((1.0 - alpha / a) / a0) as f32;
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
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
    Eq {
        low: [Biquad; MAX_CHANNELS],
        mid: [Biquad; MAX_CHANNELS],
        high: [Biquad; MAX_CHANNELS],
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

/// DSP-Kette eines Clips (Reihenfolge = Effekt-Stapel).
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

    /// Parameter zur Medienzeit nachführen (Filterzustände bleiben).
    pub fn retune(&mut self, effects: &[&EffectInstance], media_t: f64) {
        let rate = self.rate;
        for stage in &mut self.stages {
            let Some(inst) = effects.iter().find(|e| e.id == stage.fx_id) else {
                continue;
            };
            let r = inst.eval(media_t);
            let v = |i: usize| r.values.get(i).copied().unwrap_or(0.0);
            match &mut stage.stage {
                Stage::Eq { low, mid, high } => {
                    for ch in 0..MAX_CHANNELS {
                        low[ch].set_low_shelf(rate, 120.0, v(0));
                        mid[ch].set_peaking(rate, v(2).clamp(40.0, 16000.0), v(1), 0.9);
                        high[ch].set_high_shelf(rate, 8000.0, v(3));
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
                    *makeup = db_to_linear(v(4) as f32);
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
                    let max = buf[0].len();
                    *delay_samples = ((v(0) / 1000.0 * rate as f64) as usize).clamp(1, max - 1);
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
        for stage in &mut self.stages {
            match &mut stage.stage {
                Stage::Eq { low, mid, high } => {
                    for frame in samples.chunks_exact_mut(ch) {
                        for (c, s) in frame.iter_mut().enumerate() {
                            let y = low[c].process(*s);
                            let y = mid[c].process(y);
                            *s = high[c].process(y);
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
                        let gain = db_to_linear(-*gr_db) * *makeup;
                        for s in frame.iter_mut() {
                            *s *= gain;
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
            low: Default::default(),
            mid: Default::default(),
            high: Default::default(),
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
        _ => Stage::Gate {
            threshold_db: -200.0,
            attack: 0.0,
            release: 0.0,
            env: 1.0,
            gain: 1.0,
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

    fn instance(kind: EffectKind, values: &[(usize, f64)]) -> EffectInstance {
        let mut inst = EffectInstance::new(kind);
        for (i, v) in values {
            inst.params[*i] = AnimatedParam::fixed(*v);
        }
        inst
    }

    #[test]
    fn eq_low_shelf_boosts_bass_not_treble() {
        let inst = instance(EffectKind::Equalizer, &[(0, 12.0)]);
        let fx = [&inst];
        let mut chain = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let mut low = sine(80.0, 48000);
        chain.process(&mut low);
        let mut chain = AudioFxChain::build(&fx, RATE, 2, 0.0).unwrap();
        let mut high = sine(10000.0, 48000);
        chain.process(&mut high);
        // Einschwingphase überspringen.
        let low_gain = rms(&low[24000..]) / 0.7071;
        let high_gain = rms(&high[24000..]) / 0.7071;
        assert!(low_gain > 3.0, "Bass ~+12 dB: {low_gain}");
        assert!((0.8..1.2).contains(&high_gain), "Höhen neutral: {high_gain}");
    }

    #[test]
    fn compressor_reduces_loud_signal() {
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
    fn chain_matches_and_retunes_without_rebuild() {
        let inst = instance(EffectKind::Equalizer, &[(0, 6.0)]);
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
