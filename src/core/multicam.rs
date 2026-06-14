//! Multicam-Schnitt: synchronisierte Mehrkamera-Quellen + Sync-Algorithmen.
//!
//! Eine **Multicam-Quelle** ist konzeptionell eine spezielle Sequenz: je Kamera
//! ein *Winkel* mit einem Sync-Offset auf einer gemeinsamen Zeitachse. Die
//! Quelle wird als [`MulticamSource`] an der [`TimelineStore`](crate::core::timeline::TimelineStore)
//! der Quell-Sequenz hinterlegt (`timeline.multicam`); ihre innere Timeline hält
//! je Winkel einen Video-/Audio-Clip an seiner Sync-Position (zur Inspektion und
//! zum Relink).
//!
//! Ein **Multicam-Clip** auf einer normalen Timeline verweist über
//! [`MulticamClip`] auf die Quelle und trägt den *aktiven* Winkel. Seine
//! `src_in`/`duration` rechnen in der **gemeinsamen Multicam-Zeit** τ; zur
//! Wiedergabe/zum Export wird der aktive Winkel an den wenigen Auflösungspunkten
//! (Player-Target, Renderplan, Compositor-Blatt) zu einem ganz normalen
//! Medien-Blatt aufgelöst: Asset = Winkel-Asset, Medienzeit = τ − Winkel-`pos`.
//! Dadurch nutzt der gesamte Bestandscode (Decoder, GPU-Compositing, Audio-Mix)
//! den aktiven Winkel ohne Sonderfälle.
//!
//! ## Gemeinsame Zeitachse
//!
//! Jeder Winkel `i` besitzt eine Position `pos_i ≥ 0`: die Medienzeit-0 des
//! Winkels liegt auf der gemeinsamen Zeitachse bei τ = `pos_i`. Damit ist die
//! sichtbare Medienzeit `m_i(τ) = τ − pos_i`, gültig für τ ∈
//! [`pos_i`, `pos_i` + `duration_i`). Der früheste Winkel hat `pos = 0`
//! (normalisiert). Die gemeinsame Dauer ist die Vereinigung `max_i(pos_i + d_i)`,
//! sodass an jeder Stelle geschnitten werden kann.

use crate::core::types::MediaAsset;
use serde::{Deserialize, Serialize};

/// Multicam-Referenz eines Timeline-Clips: Quelle + aktiver Winkel.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MulticamClip {
    /// Sequenz-ID der Multicam-Quelle (deren `timeline.multicam` die Winkel hält).
    pub source: String,
    /// Aktiver Winkel-Index in [`MulticamSource::angles`].
    pub angle: u32,
}

/// Sync-Verfahren beim Erstellen der Quelle (für Info/Anzeige).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MulticamSync {
    /// Gemeinsamer Startpunkt: alle Winkel beginnen bei τ = 0.
    Start,
    /// Medien-Timecode: Versatz aus den Start-Timecodes der Quellen.
    Timecode,
    /// Audio-Waveform-Analyse: Kreuzkorrelation der Energie-Hüllkurven.
    Audio,
}

impl MulticamSync {
    pub fn label(self) -> &'static str {
        match self {
            MulticamSync::Start => "Gemeinsamer Startpunkt",
            MulticamSync::Timecode => "Timecode",
            MulticamSync::Audio => "Audio-Analyse",
        }
    }

    pub fn from_key(k: &str) -> Option<Self> {
        match k {
            "start" => Some(MulticamSync::Start),
            "timecode" => Some(MulticamSync::Timecode),
            "audio" => Some(MulticamSync::Audio),
            _ => None,
        }
    }
}

/// Ein Winkel (eine Kamera) einer Multicam-Quelle.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MulticamAngle {
    /// Anzeigename (z. B. „Kamera 1“ bzw. der Dateiname).
    pub name: String,
    /// Quell-Asset (referenziert ein [`MediaAsset`]; relinkt zentral).
    pub asset_id: String,
    /// Position der Medienzeit-0 dieses Winkels auf der gemeinsamen Zeitachse
    /// (Sekunden, ≥ 0). Sichtbare Medienzeit = τ − `pos`.
    pub pos: f64,
    /// Medien-Dauer des Winkels in Sekunden.
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_audio: bool,
}

/// Multicam-Quelle: alle Winkel + Audio-Routing + gemeinsame Dauer.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MulticamSource {
    pub angles: Vec<MulticamAngle>,
    /// Fester Audio-Winkel; `None` ⇒ Audio folgt dem aktiven Video-Winkel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_angle: Option<usize>,
    /// Verwendetes Sync-Verfahren (Info).
    pub sync: MulticamSync,
    /// Dauer der gemeinsamen Zeitachse (Vereinigung aller Winkel), Sekunden.
    pub duration: f64,
}

impl MulticamSource {
    pub fn angle_count(&self) -> usize {
        self.angles.len()
    }

    /// Winkel `idx` (Video).
    pub fn angle(&self, idx: u32) -> Option<&MulticamAngle> {
        self.angles.get(idx as usize)
    }

    /// Index des Winkels, der das Audio liefert: fester Audio-Winkel, sonst der
    /// aktive Video-Winkel (geklemmt auf gültigen Bereich).
    pub fn audio_angle_idx(&self, active: u32) -> usize {
        let n = self.angles.len();
        if n == 0 {
            return 0;
        }
        self.audio_angle
            .filter(|i| *i < n)
            .unwrap_or((active as usize).min(n - 1))
    }
}

// ----------------------------------------------------------- Sync-Berechnung

/// Hüllkurven-Abtastrate für die Audio-Synchronisierung (Hz). Ein Wert je
/// 1/`SYNC_RATE` s — fein genug für sub-Frame-genaue Verschiebungen.
pub const SYNC_RATE: f64 = 120.0;

/// Quell-PCM-Rate für die Hüllkurven-Extraktion (mono, s16le).
const PCM_RATE: u32 = 8000;

/// Maximal analysierte Dauer je Winkel (Sekunden) — begrenzt die O(N²)-
/// Korrelation bei langen Clips.
const MAX_ANALYZE_SECS: f64 = 600.0;

/// Sync-Positionen `pos_i` aus dem gemeinsamen Startpunkt: alle 0.
pub fn positions_from_start(n: usize) -> Vec<f64> {
    vec![0.0; n]
}

/// Sync-Positionen aus Start-Timecodes (Sekunden je Winkel). Der früheste
/// Timecode definiert τ = 0; spätere Winkel rücken um ihre Differenz nach
/// rechts. Fehlt ein Timecode (`None`), zählt er als 0.
pub fn positions_from_timecodes(tcs: &[Option<f64>]) -> Vec<f64> {
    let vals: Vec<f64> = tcs.iter().map(|t| t.unwrap_or(0.0)).collect();
    let min = vals.iter().copied().fold(f64::INFINITY, f64::min);
    let min = if min.is_finite() { min } else { 0.0 };
    vals.iter().map(|v| (v - min).max(0.0)).collect()
}

/// Sync-Positionen aus Energie-Hüllkurven (gleiche Abtastrate `rate` Hz). Jeder
/// Winkel wird per Kreuzkorrelation gegen den **Referenzwinkel 0** ausgerichtet;
/// die rohen Verzögerungen werden auf `min = 0` normalisiert.
///
/// Liefert je Winkel `pos_i ≥ 0` in Sekunden. Winkel ohne Hüllkurve (`None`)
/// erhalten `pos = 0`.
pub fn positions_from_audio(envelopes: &[Option<Vec<f32>>], rate: f64) -> Vec<f64> {
    let n = envelopes.len();
    if n == 0 {
        return Vec::new();
    }
    // Referenz: der erste Winkel mit Hüllkurve.
    let Some(ref_idx) = envelopes.iter().position(|e| e.is_some()) else {
        return vec![0.0; n];
    };
    let reference = envelopes[ref_idx].as_ref().unwrap();
    let mut raw = vec![0.0f64; n];
    for (i, env) in envelopes.iter().enumerate() {
        if i == ref_idx {
            raw[i] = 0.0;
            continue;
        }
        if let Some(e) = env {
            // best_delay_samples(reference, e) = Δ, sodass die Merkmale (Klappe)
            // in `e` Δ Samples SPÄTER (höherer Index) liegen als in der Referenz.
            // Liegt die Klappe in `e` später, hat Kamera i FRÜHER zu filmen
            // begonnen (mehr Material vor der Klappe) ⇒ ihr Aufnahmestart liegt
            // auf der gemeinsamen Achse FRÜHER ⇒ Position = −Δ.
            let max_lag = reference.len().max(e.len());
            let d = best_delay_samples(reference, e, max_lag);
            raw[i] = -(d as f64) / rate;
        } else {
            raw[i] = 0.0;
        }
    }
    // Auf min = 0 normalisieren (früheste Kamera bei τ = 0).
    let min = raw.iter().copied().fold(f64::INFINITY, f64::min);
    let min = if min.is_finite() { min } else { 0.0 };
    raw.iter().map(|v| (v - min).max(0.0)).collect()
}

/// Spaltenzahl des Multicam-Rasters für `n` Winkel (quadratisch aufgerundet):
/// 1→1, 2..4→2, 5..9→3, 10..16→4 …
pub fn grid_cols(n: usize) -> usize {
    if n <= 1 {
        1
    } else {
        (n as f64).sqrt().ceil() as usize
    }
}

/// Gemeinsame Dauer (Vereinigung) aus Positionen + Winkel-Dauern.
pub fn common_duration(pos: &[f64], durations: &[f64]) -> f64 {
    pos.iter()
        .zip(durations.iter())
        .map(|(p, d)| p + d)
        .fold(0.0, f64::max)
}

/// Kreuzkorrelation: Verschiebung Δ (in Samples), bei der `b` gegenüber `a`
/// am besten ausgerichtet ist. Konvention: maximiert die normalisierte
/// Korrelation `Σ_i a[i]·b[i+Δ]`, d. h. Δ ist die Verschiebung mit
/// `b[i] ≈ a[i−Δ]`. Ein **positiver** Rückgabewert bedeutet, dass die Merkmale
/// von `b` Δ Samples **später** (höherer Index) liegen als die von `a`.
///
/// Normalisiert über das jeweils überlappende Fenster (mittelwertbereinigt),
/// damit nicht trivial Δ = 0 mit der größten Überlappung gewinnt. Fenster mit
/// zu kleiner Überlappung werden ignoriert.
// Doppelter Index (a[i] gegen b[i+Δ]) ist hier inhärent — kein Iterator-Refactor.
#[allow(clippy::needless_range_loop)]
pub fn best_delay_samples(a: &[f32], b: &[f32], max_lag: usize) -> i64 {
    let (la, lb) = (a.len(), b.len());
    if la == 0 || lb == 0 {
        return 0;
    }
    let max_lag = max_lag.min(la + lb);
    // Mindest-Überlappung: verhindert Rauschspitzen an den Rändern.
    let min_overlap = (la.min(lb) / 4).max(8);
    let mut best_score = f64::NEG_INFINITY;
    let mut best_delay: i64 = 0;
    let lo = -(max_lag as i64);
    let hi = max_lag as i64;
    for delta in lo..=hi {
        // Überlappende Indizes i (in a) mit gültigem i+Δ (in b).
        let i_start = if delta < 0 { (-delta) as usize } else { 0 };
        let i_end = if delta >= 0 {
            la.min(lb.saturating_sub(delta as usize))
        } else {
            la.min(lb)
        };
        if i_end <= i_start {
            continue;
        }
        let count = i_end - i_start;
        if count < min_overlap {
            continue;
        }
        // Mittelwerte im Überlappungsfenster.
        let mut sum_a = 0.0f64;
        let mut sum_b = 0.0f64;
        for i in i_start..i_end {
            let j = (i as i64 + delta) as usize;
            sum_a += a[i] as f64;
            sum_b += b[j] as f64;
        }
        let mean_a = sum_a / count as f64;
        let mean_b = sum_b / count as f64;
        let mut dot = 0.0f64;
        let mut ea = 0.0f64;
        let mut eb = 0.0f64;
        for i in i_start..i_end {
            let j = (i as i64 + delta) as usize;
            let va = a[i] as f64 - mean_a;
            let vb = b[j] as f64 - mean_b;
            dot += va * vb;
            ea += va * va;
            eb += vb * vb;
        }
        let denom = (ea * eb).sqrt();
        if denom <= 1e-12 {
            continue;
        }
        let score = dot / denom;
        if score > best_score {
            best_score = score;
            best_delay = delta;
        }
    }
    best_delay
}

/// Energie-Hüllkurve eines Audiosignals per ffmpeg extrahieren (mono, s16le →
/// RMS je 1/`SYNC_RATE`-Bucket). Blockierend — nur beim Erstellen einer
/// Multicam-Quelle aufgerufen. `None` ⇒ kein Audio bzw. Fehler.
pub fn extract_sync_envelope(path: &str) -> Option<Vec<f32>> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut child = Command::new(crate::services::ffmpeg_bin())
        .args(["-v", "error", "-t", &format!("{MAX_ANALYZE_SECS}")])
        .args(["-i", path])
        .args(["-map", "a:0", "-ac", "1", "-ar"])
        .arg(PCM_RATE.to_string())
        .args(["-f", "s16le", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut raw = Vec::new();
    if stdout.read_to_end(&mut raw).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let status = child.wait().ok()?;
    if !status.success() || raw.len() < 2 {
        return None;
    }
    // Samples nach Bucket falten (RMS). Bucket-Breite = PCM_RATE / SYNC_RATE.
    let per_bucket = (PCM_RATE as f64 / SYNC_RATE).max(1.0);
    let sample_count = raw.len() / 2;
    let bucket_count = ((sample_count as f64) / per_bucket).ceil() as usize;
    if bucket_count == 0 {
        return None;
    }
    let mut sums = vec![0.0f64; bucket_count];
    let mut counts = vec![0u32; bucket_count];
    for (i, pair) in raw.chunks_exact(2).enumerate() {
        let v = i16::from_le_bytes([pair[0], pair[1]]) as f64 / 32768.0;
        let b = ((i as f64 / per_bucket) as usize).min(bucket_count - 1);
        sums[b] += v * v;
        counts[b] += 1;
    }
    let env: Vec<f32> = sums
        .iter()
        .zip(counts.iter())
        .map(|(s, c)| {
            if *c == 0 {
                0.0
            } else {
                (s / *c as f64).sqrt() as f32
            }
        })
        .collect();
    Some(env)
}

/// SMPTE-Timecode „HH:MM:SS:FF" (Semikolon vor den Frames erlaubt) in Sekunden,
/// mit der Frames-Rate `fps`. `None` bei ungültigem Format.
pub fn timecode_to_seconds(tc: &str, fps: f64) -> Option<f64> {
    let norm = tc.trim().replace(';', ":");
    let parts: Vec<&str> = norm.split(':').collect();
    if parts.len() != 4 {
        return None;
    }
    let h: f64 = parts[0].parse().ok()?;
    let m: f64 = parts[1].parse().ok()?;
    let s: f64 = parts[2].parse().ok()?;
    let f: f64 = parts[3].parse().ok()?;
    let fps = if fps.is_finite() && fps > 0.0 { fps } else { 25.0 };
    Some(h * 3600.0 + m * 60.0 + s + f / fps)
}

/// Die innere Timeline einer Multicam-Quelle bauen: je Winkel eine Video- und
/// eine Audiospur, jeweils ein Clip an seiner Sync-Position. Dient der
/// Inspektion/dem Relink; die Render-Auflösung läuft über
/// [`MulticamSource::angles`], nicht über diese Timeline.
pub fn build_inner_timeline(source: &MulticamSource) -> crate::core::timeline::TimelineStore {
    use crate::core::timeline::{
        new_media_clip, new_track, TimelineStore, TrackKind, MIN_CLIP_DURATION,
    };
    let mut store = TimelineStore::default();
    let (w, h) = source
        .angles
        .first()
        .map(|a| (a.width.max(2), a.height.max(2)))
        .unwrap_or((1920, 1080));
    store.settings.width = w;
    store.settings.height = h;
    let mut tracks = Vec::new();
    let mut clips = Vec::new();
    for a in &source.angles {
        let vt = new_track(TrackKind::Video);
        let dur = a.duration.max(MIN_CLIP_DURATION);
        clips.push(new_media_clip(
            &vt.id,
            &a.asset_id,
            a.name.clone(),
            TrackKind::Video,
            a.pos,
            dur,
            0.0,
            dur,
        ));
        tracks.push(vt);
    }
    for a in &source.angles {
        let at = new_track(TrackKind::Audio);
        if a.has_audio {
            let dur = a.duration.max(MIN_CLIP_DURATION);
            clips.push(new_media_clip(
                &at.id,
                &a.asset_id,
                format!("{} (Audio)", a.name),
                TrackKind::Audio,
                a.pos,
                dur,
                0.0,
                dur,
            ));
        }
        tracks.push(at);
    }
    store.tracks = tracks;
    store.clips = clips;
    store
}

/// Eine Multicam-Quelle aus Assets + vorberechneten Positionen bauen. Liefert
/// die [`MulticamSource`] (die innere Timeline baut der Aufrufer separat).
pub fn build_source(
    assets: &[&MediaAsset],
    positions: &[f64],
    audio_angle: Option<usize>,
    sync: MulticamSync,
) -> MulticamSource {
    let mut angles = Vec::with_capacity(assets.len());
    let mut durations = Vec::with_capacity(assets.len());
    for (i, asset) in assets.iter().enumerate() {
        let v = asset.info.video.first();
        let duration = asset.info.duration_sec.max(0.0);
        durations.push(duration);
        angles.push(MulticamAngle {
            name: format!("Kamera {}", i + 1),
            asset_id: asset.id.clone(),
            pos: positions.get(i).copied().unwrap_or(0.0).max(0.0),
            duration,
            width: v.map(|s| s.width).unwrap_or(0),
            height: v.map(|s| s.height).unwrap_or(0),
            fps: v.map(|s| s.fps).filter(|f| *f > 0.0).unwrap_or(25.0),
            has_audio: !asset.info.audio.is_empty(),
        });
    }
    let pos: Vec<f64> = angles.iter().map(|a| a.pos).collect();
    let duration = common_duration(&pos, &durations);
    MulticamSource {
        angles,
        audio_angle,
        sync,
        duration,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetische Hüllkurve: eine Energie-„Spitze" (Klappe) am Index `peak`.
    fn impulse_env(len: usize, peak: usize) -> Vec<f32> {
        let mut e = vec![0.02f32; len]; // leiser Grundpegel
        for d in 0..40usize {
            // glockenförmige Spitze um `peak`
            let w = 1.0 - (d as f32 / 40.0);
            if peak + d < len {
                e[peak + d] = e[peak + d].max(w);
            }
            if peak >= d {
                e[peak - d] = e[peak - d].max(w);
            }
        }
        e
    }

    #[test]
    fn cross_correlation_finds_known_delay() {
        // b ist gegenüber a um 130 Samples verzögert (Spitze später).
        let a = impulse_env(2000, 300);
        let b = impulse_env(2000, 430);
        let d = best_delay_samples(&a, &b, 2000);
        assert_eq!(d, 130, "b beginnt 130 Samples später als a");
        // Symmetrie: a gegenüber b ⇒ −130.
        let d2 = best_delay_samples(&b, &a, 2000);
        assert_eq!(d2, -130);
    }

    #[test]
    fn cross_correlation_zero_when_aligned() {
        let a = impulse_env(1500, 500);
        let b = impulse_env(1500, 500);
        assert_eq!(best_delay_samples(&a, &b, 1500), 0);
    }

    #[test]
    fn audio_positions_normalize_to_earliest() {
        let rate = 100.0;
        // Kamera 0 Referenz (Spitze @ 500), Kamera 1 startete 200 Samples
        // später (Spitze @ 300 → früher im eigenen Material), Kamera 2 startete
        // 100 Samples früher als 0 (Spitze @ 600).
        // Wir modellieren direkt die Hüllkurven:
        let env0 = impulse_env(2000, 500);
        let env1 = impulse_env(2000, 300); // Klappe früher im Material ⇒ später gestartet
        let env2 = impulse_env(2000, 600); // Klappe später im Material ⇒ früher gestartet
        let pos = positions_from_audio(
            &[Some(env0), Some(env1), Some(env2)],
            rate,
        );
        // raw: cam0 = 0, cam1 = +200/rate = 2.0, cam2 = −100/rate = −1.0.
        // normalisiert (min = −1.0): cam0 = 1.0, cam1 = 3.0, cam2 = 0.0.
        assert!((pos[2] - 0.0).abs() < 1e-9, "früheste Kamera bei 0: {pos:?}");
        assert!((pos[0] - 1.0).abs() < 1e-6, "cam0 = 1.0: {pos:?}");
        assert!((pos[1] - 3.0).abs() < 1e-6, "cam1 = 3.0: {pos:?}");
    }

    #[test]
    fn timecode_positions_relative_to_earliest() {
        // Start-TCs: 10s, 12.5s, 9s → früheste 9s ⇒ 1.0, 3.5, 0.0.
        let pos = positions_from_timecodes(&[Some(10.0), Some(12.5), Some(9.0)]);
        assert!((pos[0] - 1.0).abs() < 1e-9);
        assert!((pos[1] - 3.5).abs() < 1e-9);
        assert!((pos[2] - 0.0).abs() < 1e-9);
    }

    #[test]
    fn start_positions_all_zero() {
        assert_eq!(positions_from_start(3), vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn timecode_parsing() {
        // 01:00:00:12 @ 24 fps = 3600 + 0.5 s.
        let s = timecode_to_seconds("01:00:00:12", 24.0).unwrap();
        assert!((s - 3600.5).abs() < 1e-9);
        // Drop-Frame-Semikolon erlaubt.
        assert!(timecode_to_seconds("00:00:01;15", 30.0).is_some());
        assert!(timecode_to_seconds("kaputt", 25.0).is_none());
    }

    #[test]
    fn grid_cols_layout() {
        assert_eq!(grid_cols(1), 1);
        assert_eq!(grid_cols(2), 2);
        assert_eq!(grid_cols(4), 2);
        assert_eq!(grid_cols(5), 3);
        assert_eq!(grid_cols(9), 3);
        assert_eq!(grid_cols(10), 4);
    }

    #[test]
    fn media_time_maps_through_pos() {
        let src = MulticamSource {
            angles: vec![
                MulticamAngle {
                    name: "A".into(),
                    asset_id: "a".into(),
                    pos: 0.0,
                    duration: 10.0,
                    width: 1920,
                    height: 1080,
                    fps: 25.0,
                    has_audio: true,
                },
                MulticamAngle {
                    name: "B".into(),
                    asset_id: "b".into(),
                    pos: 2.0,
                    duration: 10.0,
                    width: 1920,
                    height: 1080,
                    fps: 25.0,
                    has_audio: true,
                },
            ],
            audio_angle: None,
            sync: MulticamSync::Audio,
            duration: 12.0,
        };
        // Bei gemeinsamer Zeit τ = 5: Winkel A (pos 0) zeigt Medienzeit 5,
        // Winkel B (pos 2) zeigt 3 (= τ − pos).
        assert!((5.0 - src.angles[0].pos - 5.0).abs() < 1e-9);
        assert!((5.0 - src.angles[1].pos - 3.0).abs() < 1e-9);
        // Audio folgt dem aktiven Winkel, wenn kein fester Audio-Winkel.
        assert_eq!(src.audio_angle_idx(1), 1);
        assert_eq!(src.duration, 12.0);
    }
}
