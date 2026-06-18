//! BS.1770-Lautheitsmessung (LUFS) und True-Peak-Metering für den Mixer.
//!
//! Speist sich aus dem Master-Mixblock des Players (`core/player.rs`,
//! `drive_audio`) — denselben Samples, die das Gerät hört bzw. der Export
//! liefert. Implementiert ITU-R BS.1770-4: K-Weighting (zwei kaskadierte
//! Biquads), gleitende Fenster für Momentary (400 ms) und Short-Term (3 s)
//! sowie die gegatete Integrated-Loudness (absolutes Gate −70 LUFS,
//! relatives Gate −10 LU). True-Peak per 4×-Oversampling (Polyphasen-FIR,
//! ITU-R BS.1770-4 Annex 2).
//!
//! Die DSP ist blockgrößen-invariant (Filterzustände, Sub-Block-Akkumulator
//! und True-Peak-Verzögerungsleitung leben über `feed`-Aufrufe hinweg) — wie
//! `core/audio_fx.rs`. Damit liefert der Player unabhängig von der
//! Sub-Buffer-Größe dasselbe Ergebnis.

use std::collections::VecDeque;

/// Kalibrierkonstante aus BS.1770 (so liest ein 0 dBFS-Stereo-Sinus seinen
/// dBFS-Wert als LUFS; das K-Weighting hat bei 1 kHz exakt +0,691 dB Gewinn,
/// das diesen Offset für einen Zweikanal-Identsignal aufhebt).
const LUFS_OFFSET: f64 = -0.691;
/// Absolutes Gate der Integrated-Messung (Blöcke darunter zählen nie).
const ABS_GATE_LUFS: f64 = -70.0;
/// Relatives Gate: 10 LU unter dem ungegateten Mittel der lauten Blöcke.
const REL_GATE_LU: f64 = 10.0;
/// Sub-Block-Länge in Millisekunden (Gating-Schrittweite, 75 % Overlap).
const SUBBLOCK_MS: f64 = 100.0;
/// Gating-/Momentary-Fenster = 400 ms = 4 Sub-Blöcke.
const MOMENTARY_BLOCKS: usize = 4;
/// Short-Term-Fenster = 3 s = 30 Sub-Blöcke.
const SHORT_TERM_BLOCKS: usize = 30;

/// True-Peak-Oversampling-Faktor (BS.1770-4 fordert ≥ 4× bis 48 kHz).
const TP_OVERSAMPLE: usize = 4;
/// FIR-Stützstellen je Polyphase (4 × 16 = 64-Tap-Prototyp).
const TP_TAPS_PER_PHASE: usize = 16;

// ------------------------------------------------------------------ Biquad

/// Biquad in Direct Form 1, `a0` auf 1 normiert. Rechnet in f64, weil die
/// Quadratsummen über lange Fenster sonst Genauigkeit verlieren.
#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

/// Die beiden Stufen des K-Weighting-Filters aus der Abtastrate berechnen
/// (Bilineartransformation, identisch zu libebur128). Stufe 1 ist ein
/// High-Shelf (+4 dB Hochton, Kopf-/Torso-Modell), Stufe 2 der RLB-Hochpass.
/// Bei 48 kHz ergeben sich exakt die in BS.1770 tabellierten Koeffizienten.
fn k_weighting(rate: f64) -> (Biquad, Biquad) {
    // ---- Stufe 1: High-Shelf ----
    let f0 = 1681.974450955533;
    let g = 3.999843853973347;
    let q = 0.7071752369554196;
    let k = (std::f64::consts::PI * f0 / rate).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b0: (vh + vb * k / q + k * k) / a0,
        b1: 2.0 * (k * k - vh) / a0,
        b2: (vh - vb * k / q + k * k) / a0,
        a1: 2.0 * (k * k - 1.0) / a0,
        a2: (1.0 - k / q + k * k) / a0,
        ..Biquad::default()
    };

    // ---- Stufe 2: RLB-Hochpass (Zähler {1,−2,1} bleibt unnormiert — so legt
    // BS.1770 die tabellierten Koeffizienten fest) ----
    let f0 = 38.13547087602444;
    let q = 0.5003270373238773;
    let k = (std::f64::consts::PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let hpf = Biquad {
        b0: 1.0,
        b1: -2.0,
        b2: 1.0,
        a1: 2.0 * (k * k - 1.0) / a0,
        a2: (1.0 - k / q + k * k) / a0,
        ..Biquad::default()
    };

    (shelf, hpf)
}

// --------------------------------------------------------------- True Peak

/// 4×-Oversampling-Schätzer für Inter-Sample-Peaks: ein Polyphasen-FIR
/// rekonstruiert drei Zwischenwerte je Eingangssample, der Betrag des größten
/// (inkl. Rohwert) ist der True-Peak. Verzögerungsleitung je Kanal lebt über
/// Blockgrenzen → blockgrößen-invariant.
struct TruePeak {
    /// Koeffizienten je Phase, neueste Stützstelle zuerst (`coeffs[p][0]`
    /// multipliziert das jüngste Eingangssample).
    coeffs: Vec<Vec<f64>>,
    /// Verzögerungsleitung je Kanal (Index 0 = jüngstes Sample).
    hist: Vec<Vec<f64>>,
    taps: usize,
}

impl TruePeak {
    fn new(channels: usize) -> TruePeak {
        TruePeak {
            coeffs: design_polyphase(TP_OVERSAMPLE, TP_TAPS_PER_PHASE),
            hist: (0..channels).map(|_| vec![0.0; TP_TAPS_PER_PHASE]).collect(),
            taps: TP_TAPS_PER_PHASE,
        }
    }

    /// Sample einspeisen und den True-Peak (linear) an dieser Position liefern.
    #[inline]
    fn push(&mut self, ch: usize, sample: f64) -> f64 {
        let h = &mut self.hist[ch];
        h.rotate_right(1);
        h[0] = sample;
        let mut peak = sample.abs();
        for phase in &self.coeffs {
            let mut acc = 0.0;
            for n in 0..self.taps {
                acc += phase[n] * h[n];
            }
            let a = acc.abs();
            if a > peak {
                peak = a;
            }
        }
        peak
    }

    fn reset(&mut self) {
        for h in &mut self.hist {
            h.iter_mut().for_each(|s| *s = 0.0);
        }
    }
}

/// Polyphasen-Zerlegung eines fenstergewichteten Sinc-Tiefpasses (Cutoff =
/// Original-Nyquist). Jede Phase wird auf DC-Gewinn 1 normiert, damit ein
/// konstantes Signal exakt reproduziert und Peaks nicht verfälscht werden.
fn design_polyphase(oversample: usize, taps_per_phase: usize) -> Vec<Vec<f64>> {
    let m = oversample * taps_per_phase;
    let center = (m as f64 - 1.0) / 2.0;
    let mut proto = vec![0.0f64; m];
    for (i, p) in proto.iter_mut().enumerate() {
        let x = i as f64 - center;
        let sinc = if x.abs() < 1e-9 {
            1.0
        } else {
            let t = std::f64::consts::PI * x / oversample as f64;
            t.sin() / t
        };
        *p = sinc * blackman_harris(i, m);
    }
    let mut coeffs = vec![vec![0.0f64; taps_per_phase]; oversample];
    for (p, phase) in coeffs.iter_mut().enumerate() {
        // coeffs[p][n] = proto[p + oversample*n]; neueste Stützstelle bei n=0.
        for (n, c) in phase.iter_mut().enumerate() {
            *c = proto[p + oversample * n];
        }
        let sum: f64 = phase.iter().sum();
        if sum.abs() > 1e-12 {
            phase.iter_mut().for_each(|c| *c /= sum);
        }
    }
    coeffs
}

/// 4-Term-Blackman-Harris-Fenster (Seitenkeulen ≈ −92 dB) für einen sauberen
/// Sperrbereich des Oversampling-FIR.
fn blackman_harris(i: usize, n: usize) -> f64 {
    let a = [0.35875, 0.48829, 0.14128, 0.01168];
    let w = 2.0 * std::f64::consts::PI * i as f64 / (n as f64 - 1.0);
    a[0] - a[1] * w.cos() + a[2] * (2.0 * w).cos() - a[3] * (3.0 * w).cos()
}

// ---------------------------------------------------------------- Snapshot

/// Anzeigbare Messwerte für das Mixer-Panel (alle in LUFS bzw. dBTP;
/// [`f32::NEG_INFINITY`] = „kein Signal“/„noch nicht definiert“).
#[derive(Clone, Copy)]
pub struct LoudnessSnapshot {
    /// Momentary-Loudness (400-ms-Fenster).
    pub momentary: f32,
    /// Short-Term-Loudness (3-s-Fenster).
    pub short_term: f32,
    /// Integrated-Loudness (gegatet, seit letztem Reset/Seek).
    pub integrated: f32,
    /// Größter True-Peak seit letztem Reset (dBTP, Max-Hold).
    pub true_peak: f32,
}

impl Default for LoudnessSnapshot {
    fn default() -> LoudnessSnapshot {
        LoudnessSnapshot {
            momentary: f32::NEG_INFINITY,
            short_term: f32::NEG_INFINITY,
            integrated: f32::NEG_INFINITY,
            true_peak: f32::NEG_INFINITY,
        }
    }
}

// ------------------------------------------------------------------ Meter

/// Ein abgeschlossener 100-ms-Sub-Block: Quadratsumme der K-gewichteten
/// Samples je Kanal (noch nicht durch die Sample-Zahl geteilt) + Sample-Zahl.
struct SubBlock {
    sums: Vec<f64>,
    count: usize,
}

/// BS.1770-Lautheitsmesser für einen interleaved-Stereo-Strom. Wird vom Player
/// pro Mix-Block gefüttert; der Mixer liest den [`LoudnessSnapshot`].
pub struct LoudnessMeter {
    channels: usize,
    /// K-Weighting je Kanal (Shelf + Hochpass).
    filters: Vec<(Biquad, Biquad)>,
    /// Kanalgewichte G_i (Stereo: 1,0/1,0).
    channel_gains: Vec<f64>,
    /// Samples je 100-ms-Sub-Block.
    block_samples: usize,
    /// Laufende Quadratsummen des offenen Sub-Blocks + Sample-Zähler.
    cur_sumsq: Vec<f64>,
    cur_count: usize,
    /// Ring der jüngsten Sub-Blöcke (genug für das 3-s-Short-Term-Fenster).
    subblocks: VecDeque<SubBlock>,
    /// Pro 400-ms-Gating-Block die gewichtete mittlere Leistung
    /// (`Σ_i G_i · meanSquare_i`) — Grundlage der gegateten Integrated-Messung.
    gating_powers: Vec<f64>,
    /// Zuletzt berechnete Integrated-Loudness (LUFS). Wird nur bei einem neuen
    /// Gating-Block neu berechnet (alle 100 ms), nicht pro Anzeige-Frame.
    integrated_cache: f64,
    /// True-Peak-Schätzer + Max-Hold (linear).
    tp: TruePeak,
    tp_max: f64,
}

impl LoudnessMeter {
    pub fn new(rate: u32, channels: usize) -> LoudnessMeter {
        let block_samples = (rate as f64 * SUBBLOCK_MS / 1000.0).round().max(1.0) as usize;
        LoudnessMeter {
            channels,
            filters: (0..channels).map(|_| k_weighting(rate as f64)).collect(),
            channel_gains: vec![1.0; channels],
            block_samples,
            cur_sumsq: vec![0.0; channels],
            cur_count: 0,
            subblocks: VecDeque::with_capacity(SHORT_TERM_BLOCKS + 1),
            gating_powers: Vec::new(),
            integrated_cache: f64::NEG_INFINITY,
            tp: TruePeak::new(channels),
            tp_max: 0.0,
        }
    }

    /// Einen interleaved-Mixblock (L,R,L,R,…) einspeisen.
    pub fn feed(&mut self, interleaved: &[f32]) {
        if self.channels == 0 {
            return;
        }
        for frame in interleaved.chunks_exact(self.channels) {
            for c in 0..self.channels {
                // Nicht-finite Samples (NaN/∞ aus einem defekten Decoder/Effekt)
                // abfangen: sie würden sonst die Biquad-Rückkopplung dauerhaft
                // vergiften (kein Self-Heal wie in `audio_fx.rs`).
                let x = frame[c] as f64;
                let x = if x.is_finite() { x } else { 0.0 };
                let tp = self.tp.push(c, x);
                if tp > self.tp_max {
                    self.tp_max = tp;
                }
                let (s1, s2) = &mut self.filters[c];
                let w = s2.process(s1.process(x));
                self.cur_sumsq[c] += w * w;
            }
            self.cur_count += 1;
            if self.cur_count >= self.block_samples {
                self.close_subblock();
            }
        }
    }

    /// Offenen Sub-Block abschließen, in den Ring schieben und — sobald 400 ms
    /// vorliegen — einen Gating-Block für die Integrated-Messung ablegen.
    fn close_subblock(&mut self) {
        let sums = std::mem::replace(&mut self.cur_sumsq, vec![0.0; self.channels]);
        let count = self.cur_count;
        self.cur_count = 0;
        self.subblocks.push_back(SubBlock { sums, count });
        while self.subblocks.len() > SHORT_TERM_BLOCKS {
            self.subblocks.pop_front();
        }
        if self.subblocks.len() >= MOMENTARY_BLOCKS {
            let power = self.window_power(MOMENTARY_BLOCKS);
            if power > 0.0 {
                self.gating_powers.push(power);
                // Integrated nur bei neuem Block neu berechnen (nicht pro Frame).
                self.integrated_cache = self.integrated();
            }
        }
    }

    /// Gewichtete mittlere Leistung `Σ_i G_i · meanSquare_i` über die jüngsten
    /// `n` Sub-Blöcke (≤ vorhandene).
    fn window_power(&self, n: usize) -> f64 {
        let take = n.min(self.subblocks.len());
        if take == 0 {
            return 0.0;
        }
        let mut chan_sum = vec![0.0f64; self.channels];
        let mut total = 0usize;
        for sb in self.subblocks.iter().rev().take(take) {
            for c in 0..self.channels {
                chan_sum[c] += sb.sums[c];
            }
            total += sb.count;
        }
        if total == 0 {
            return 0.0;
        }
        let mut power = 0.0;
        for c in 0..self.channels {
            power += self.channel_gains[c] * chan_sum[c] / total as f64;
        }
        power
    }

    /// Gegatete Integrated-Loudness (absolutes + relatives Gate).
    fn integrated(&self) -> f64 {
        if self.gating_powers.is_empty() {
            return f64::NEG_INFINITY;
        }
        // Absolutes Gate (−70 LUFS).
        let abs_thresh = lufs_to_power(ABS_GATE_LUFS);
        let mut abs_sum = 0.0;
        let mut abs_n = 0usize;
        for &p in &self.gating_powers {
            if p >= abs_thresh {
                abs_sum += p;
                abs_n += 1;
            }
        }
        if abs_n == 0 {
            return f64::NEG_INFINITY;
        }
        // Relatives Gate: 10 LU unter dem Mittel der absolut-gegateten Blöcke.
        let rel_thresh = lufs_to_power(power_to_lufs(abs_sum / abs_n as f64) - REL_GATE_LU);
        let mut rel_sum = 0.0;
        let mut rel_n = 0usize;
        for &p in &self.gating_powers {
            if p >= abs_thresh && p >= rel_thresh {
                rel_sum += p;
                rel_n += 1;
            }
        }
        if rel_n == 0 {
            return f64::NEG_INFINITY;
        }
        power_to_lufs(rel_sum / rel_n as f64)
    }

    /// Aktuelle Messwerte als Anzeige-Snapshot.
    pub fn snapshot(&self) -> LoudnessSnapshot {
        LoudnessSnapshot {
            momentary: power_to_lufs(self.window_power(MOMENTARY_BLOCKS)) as f32,
            short_term: power_to_lufs(self.window_power(SHORT_TERM_BLOCKS)) as f32,
            integrated: self.integrated_cache as f32,
            true_peak: lin_to_dbtp(self.tp_max) as f32,
        }
    }

    /// Vollständiger Neustart der Messung (Seek / manueller Reset).
    pub fn reset(&mut self) {
        for (a, b) in &mut self.filters {
            a.reset();
            b.reset();
        }
        self.cur_sumsq.iter_mut().for_each(|s| *s = 0.0);
        self.cur_count = 0;
        self.subblocks.clear();
        self.gating_powers.clear();
        self.integrated_cache = f64::NEG_INFINITY;
        self.tp.reset();
        self.tp_max = 0.0;
    }

    /// Wiedergabe-Stopp: gleitende Fenster (Momentary/Short-Term) verwerfen,
    /// damit sie nicht eingefroren stehen bleiben — Integrated-Historie und
    /// True-Peak-Max-Hold bleiben als Messergebnis erhalten.
    pub fn pause(&mut self) {
        for (a, b) in &mut self.filters {
            a.reset();
            b.reset();
        }
        self.cur_sumsq.iter_mut().for_each(|s| *s = 0.0);
        self.cur_count = 0;
        self.subblocks.clear();
    }
}

/// Leistung (linearer mittlerer K-gewichteter Quadratwert) → LUFS.
#[inline]
fn power_to_lufs(power: f64) -> f64 {
    if power > 0.0 {
        LUFS_OFFSET + 10.0 * power.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// LUFS → Leistung (Umkehrung von [`power_to_lufs`]).
#[inline]
fn lufs_to_power(lufs: f64) -> f64 {
    10f64.powf((lufs - LUFS_OFFSET) / 10.0)
}

/// Linearer Peak → dBTP.
#[inline]
fn lin_to_dbtp(v: f64) -> f64 {
    if v > 0.0 {
        20.0 * v.log10()
    } else {
        f64::NEG_INFINITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Interleaved-Stereo-Sinus erzeugen (gleiches Signal auf L/R).
    fn stereo_sine(freq: f64, amp: f64, rate: u32, secs: f64) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        let mut out = Vec::with_capacity(n * 2);
        for i in 0..n {
            let s = (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin()) as f32;
            out.push(s);
            out.push(s);
        }
        out
    }

    /// BS.1770-Referenz: ein 1-kHz-Stereo-Sinus bei −23 dBFS auf beiden Kanälen
    /// liest exakt −23,0 LUFS (Momentary, Short-Term, Integrated). Validiert
    /// K-Weighting-Koeffizienten, Kanalsummierung, Gating und Kalibrierung.
    #[test]
    fn bs1770_stereo_minus23() {
        let rate = 48000;
        let amp = 10f64.powf(-23.0 / 20.0); // −23 dBFS Spitzenamplitude
        let mut meter = LoudnessMeter::new(rate, 2);
        meter.feed(&stereo_sine(1000.0, amp, rate, 10.0));
        let snap = meter.snapshot();
        assert!(
            (snap.integrated as f64 - (-23.0)).abs() < 0.1,
            "Integrated {} LUFS, erwartet ≈ −23,0",
            snap.integrated
        );
        assert!(
            (snap.momentary as f64 - (-23.0)).abs() < 0.1,
            "Momentary {} LUFS, erwartet ≈ −23,0",
            snap.momentary
        );
        assert!(
            (snap.short_term as f64 - (-23.0)).abs() < 0.1,
            "Short-Term {} LUFS, erwartet ≈ −23,0",
            snap.short_term
        );
    }

    /// Ein 0-dBFS-Sinus auf nur EINEM Kanal liest −3,01 LUFS (10·log10(2)-Term
    /// der Kanalsummierung greift; prüft Offset + Single-Channel-Pfad).
    #[test]
    fn bs1770_mono_full_scale() {
        let rate = 48000;
        let mut meter = LoudnessMeter::new(rate, 2);
        // Sinus nur auf L, R = Stille.
        let n = (rate as f64 * 8.0) as usize;
        let mut buf = Vec::with_capacity(n * 2);
        for i in 0..n {
            let s = (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / rate as f64).sin() as f32;
            buf.push(s);
            buf.push(0.0);
        }
        meter.feed(&buf);
        let snap = meter.snapshot();
        assert!(
            (snap.integrated as f64 - (-3.01)).abs() < 0.1,
            "Integrated {} LUFS, erwartet ≈ −3,01",
            snap.integrated
        );
    }

    /// Gating: ein lauter Abschnitt gefolgt von langer Stille — die Stille darf
    /// die Integrated-Messung nicht herunterziehen (absolutes/relatives Gate).
    #[test]
    fn gating_ignores_silence() {
        let rate = 48000;
        let amp = 10f64.powf(-23.0 / 20.0);
        let mut meter = LoudnessMeter::new(rate, 2);
        meter.feed(&stereo_sine(1000.0, amp, rate, 5.0));
        meter.feed(&vec![0.0f32; rate as usize * 2 * 10]); // 10 s Stille
        let snap = meter.snapshot();
        assert!(
            (snap.integrated as f64 - (-23.0)).abs() < 0.2,
            "Integrated {} LUFS trotz Gating, erwartet ≈ −23,0",
            snap.integrated
        );
    }

    /// True-Peak: ein 0-dBFS-Sinus bei fs/4, dessen Samples die Kämme
    /// verfehlen (Sample-Peak ≈ −3 dBFS), muss per Oversampling nahe 0 dBTP
    /// erkannt werden.
    #[test]
    fn true_peak_intersample() {
        let rate = 48000;
        let mut meter = LoudnessMeter::new(rate, 2);
        // fs/4-Sinus mit 45°-Phase: Samples landen bei ±1/√2 ≈ ±0,707.
        let n = rate as usize;
        let mut buf = Vec::with_capacity(n * 2);
        let phase = std::f64::consts::FRAC_PI_4;
        for i in 0..n {
            let s = (2.0 * std::f64::consts::PI * (rate as f64 / 4.0) * i as f64 / rate as f64
                + phase)
                .sin() as f32;
            buf.push(s);
            buf.push(s);
        }
        meter.feed(&buf);
        let snap = meter.snapshot();
        // Sample-Peak liegt bei ≈ −3,01 dBFS; True-Peak muss klar darüber und
        // nahe 0 dBTP liegen.
        assert!(
            snap.true_peak > -1.0 && snap.true_peak < 0.5,
            "True-Peak {} dBTP, erwartet ≈ 0 dBTP",
            snap.true_peak
        );
    }
}
