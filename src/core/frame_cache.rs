//! RAM-begrenzter LRU-Cache für dekodierte RGBA-Frames (Scrubbing/Read-Ahead)
//! plus reine Helfer für smarteres Seeking. Der Cache hält dekodierte Frames
//! in Wiedergabeauflösung; beim Scrubben wird zuerst hier getroffen, nur bei
//! Miss dekodiert ([`super::player`]). Die Seek-Helfer ([`seek_decision`],
//! [`ScrubCoalescer`]) sind rein/funktional und unit-getestet.
//!
//! Einige Cache-/Coalescer-Methoden gehören bewusst zur API und werden von den
//! Unit-Tests abgedeckt, aber (noch) nicht überall im Binary aufgerufen — daher
//! modulweites `allow(dead_code)`.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Eindeutiger Schlüssel eines gecachten Frames. Der Pixelinhalt ist durch
/// (Decode-Pfad, Decode-Maße, Frame auf dem Decode-Raster) eindeutig bestimmt:
/// Clip-Speed/Reverse beeinflussen nur die Abbildung Index↔Zeit, nicht die
/// Pixel zu einer gegebenen Medienzeit — der Cache ist daher
/// speed-unabhängig.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct FrameKey {
    /// Decode-Pfad (Original oder Proxy — wird vom Aufrufer eingesetzt).
    pub path: String,
    pub w: i32,
    pub h: i32,
    /// Frame-Index auf dem Decode-Raster: `round(media_time * fps_milli/1000)`.
    pub frame: i64,
    /// Decode-Raster in fps×1000 — trennt Einträge bei Ratenwechsel sauber.
    pub fps_milli: u32,
}

impl FrameKey {
    /// Schlüssel aus Medienzeit (Sekunden) auf das Decode-Raster runden.
    pub fn at_time(path: &str, w: i32, h: i32, fps: f64, media_time: f64) -> FrameKey {
        let fps = if fps > 0.0 { fps } else { 25.0 };
        let frame = (media_time.max(0.0) * fps).round() as i64;
        FrameKey {
            path: path.to_string(),
            w,
            h,
            frame,
            fps_milli: (fps * 1000.0).round() as u32,
        }
    }
}

struct Entry {
    data: Arc<Vec<u8>>,
    bytes: usize,
    /// Zugriffssequenz (höher = jünger) — Schlüssel der LRU-Ordnung.
    seq: u64,
}

/// LRU-Frame-Cache mit hartem Byte-Budget. Eviction in O(log n) über eine
/// nach Zugriffssequenz sortierte Hilfsstruktur.
pub struct FrameCache {
    map: HashMap<FrameKey, Entry>,
    /// seq → key, für O(log n)-LRU-Eviction (kleinste seq = älteste).
    order: BTreeMap<u64, FrameKey>,
    used: usize,
    budget: usize,
    next_seq: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl FrameCache {
    pub fn new(budget_bytes: usize) -> FrameCache {
        FrameCache {
            map: HashMap::new(),
            order: BTreeMap::new(),
            used: 0,
            budget: budget_bytes,
            next_seq: 1,
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }

    /// Budget zur Laufzeit ändern (Einstellung); ggf. sofort verkleinern.
    pub fn set_budget(&mut self, budget_bytes: usize) {
        self.budget = budget_bytes;
        self.evict_to_budget();
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Frame holen und als zuletzt genutzt markieren. Zählt Hit/Miss
    /// (Telemetrie für das Perf-Overlay).
    pub fn get(&mut self, key: &FrameKey) -> Option<Arc<Vec<u8>>> {
        // Erst die neue Sequenz reservieren, dann den Eintrag mutieren —
        // umgeht den Borrow-Konflikt zwischen `self.next_seq` und `&mut entry`.
        let seq = self.next_seq;
        if let Some(entry) = self.map.get_mut(key) {
            self.order.remove(&entry.seq);
            entry.seq = seq;
            let data = entry.data.clone();
            self.next_seq += 1;
            self.order.insert(seq, key.clone());
            self.hits += 1;
            Some(data)
        } else {
            self.misses += 1;
            None
        }
    }

    /// Enthält der Cache diesen Frame? Ohne Touch, ohne Hit/Miss-Zählung —
    /// für Prefetch-Entscheidungen (nicht jeden geprüften Nachbarn als
    /// Cache-Treffer werten).
    pub fn contains(&self, key: &FrameKey) -> bool {
        self.map.contains_key(key)
    }

    /// Frame einlagern (überschreibt vorhandenen). Räumt auf Budget.
    pub fn insert(&mut self, key: FrameKey, data: Arc<Vec<u8>>) {
        let bytes = data.len();
        // Einzelframe größer als das gesamte Budget → nicht cachen (sonst
        // würde er sofort wieder evictet, reine Verschwendung).
        if bytes > self.budget {
            return;
        }
        if let Some(old) = self.map.remove(&key) {
            self.order.remove(&old.seq);
            self.used -= old.bytes;
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        self.order.insert(seq, key.clone());
        self.map.insert(key, Entry { data, bytes, seq });
        self.used += bytes;
        self.evict_to_budget();
    }

    fn evict_to_budget(&mut self) {
        while self.used > self.budget {
            // Ältesten Eintrag (kleinste seq) entfernen.
            let Some((&seq, _)) = self.order.iter().next() else {
                break;
            };
            if let Some(key) = self.order.remove(&seq) {
                if let Some(entry) = self.map.remove(&key) {
                    self.used -= entry.bytes;
                    self.evictions += 1;
                }
            }
        }
    }

    /// Alle Einträge eines Decode-Pfads verwerfen (Proxy umgeschaltet, Asset
    /// relinkt, Render-Cache-Datei ersetzt …).
    pub fn invalidate_path(&mut self, path: &str) {
        let keys: Vec<FrameKey> = self
            .map
            .keys()
            .filter(|k| k.path == path)
            .cloned()
            .collect();
        for k in keys {
            if let Some(entry) = self.map.remove(&k) {
                self.order.remove(&entry.seq);
                self.used -= entry.bytes;
            }
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.used = 0;
    }

    pub fn used_bytes(&self) -> usize {
        self.used
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn hits(&self) -> u64 {
        self.hits
    }

    pub fn misses(&self) -> u64 {
        self.misses
    }

    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Trefferquote 0..1 über die gesamte Sitzung (für das Perf-Overlay).
    pub fn hit_ratio(&self) -> f32 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f32 / total as f32
        }
    }
}

/// Mindestabstand zwischen Scrub-Restarts (Sekunden) — drosselt Decoder-
/// Neustarts beim schnellen Ziehen (Spiegel von `player::SCRUB_RESTART_INTERVAL`).
pub const RESYNC_TOLERANCE: f64 = 0.25;

/// Ergebnis der Reuse-vs-Restart-Entscheidung beim Seeking.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeekAction {
    /// Laufende Vorwärts-Session weiterverwenden und bis zur Zielzeit lesen —
    /// kleiner Vorwärtssprung, kein Decoder-Neustart, keine spürbare Latenz.
    Reuse,
    /// Session neu aufsetzen — Rücksprung oder zu großer Vorwärtssprung.
    Restart,
}

/// Reine Entscheidung, ob ein laufender Vorwärts-Decoder für die Zielzeit
/// weiterverwendet werden kann. Spiegelt die Toleranzlogik aus
/// `player::drive_video`, herausgelöst zur Unit-Verifikation.
///
/// - `decoded_time`: Medienzeit des zuletzt dekodierten Frames.
/// - `target_time`: gewünschte Medienzeit.
/// - `fps`: Decode-Framerate.
/// - `speed`: |Medienfortschritt| pro Ausgabesekunde (Clip-Speed; 0 = Standbild).
/// - `playing`/`rate`: Wiedergabezustand (bei Vorwärtswiedergabe ist mehr
///   Decoder-Rückstand erlaubt, statt zu stocken).
pub fn seek_decision(
    decoded_time: f64,
    target_time: f64,
    fps: f64,
    speed: f64,
    playing: bool,
    rate: f64,
) -> SeekAction {
    let lead = decoded_time - target_time; // > 0: Decoder voraus (Rücksprung)
    let lag = target_time - decoded_time; // > 0: Decoder hinterher (Vorsprung)
    let tempo = speed.max(1.0);
    let (lead_tol, lag_limit) = if speed == 0.0 {
        let t = 0.5 / fps.max(1.0);
        (t, t)
    } else if playing && rate > 0.0 {
        (0.15 * tempo, 1.5 * tempo)
    } else {
        (0.15 * tempo, RESYNC_TOLERANCE * tempo)
    };
    if lead > lead_tol || lag > lag_limit {
        SeekAction::Restart
    } else {
        SeekAction::Reuse
    }
}

/// Koalesziert eine Folge von Scrub-Anfragen auf die jeweils zuletzt
/// angefragte Zielzeit: pro Verarbeitungstakt wird genau EIN Frame (der
/// letzte) bedient, nicht jeder Maus-Tick. Verhindert Decoder-Thrashing beim
/// schnellen Ziehen und steuert das Read-Ahead auf die „eingerastete“
/// Position statt auf jede Zwischenposition.
#[derive(Default)]
pub struct ScrubCoalescer {
    pending: Option<f64>,
    /// Zeitstempel der letzten Anfrage (Sekunden, App-Uhr).
    last_request: f64,
    /// Anzahl übersprungener Zwischenanfragen (Telemetrie/Test).
    coalesced: u64,
}

impl ScrubCoalescer {
    /// Neue Zielzeit anmelden. Eine bereits anstehende, noch nicht bediente
    /// Anfrage gilt als übersprungen (koalesziert).
    pub fn request(&mut self, target_time: f64, now: f64) {
        if self.pending.is_some() {
            self.coalesced += 1;
        }
        self.pending = Some(target_time);
        self.last_request = now;
    }

    /// Jüngste anstehende Zielzeit herausnehmen (leert den Puffer).
    pub fn take(&mut self) -> Option<f64> {
        self.pending.take()
    }

    /// Anstehende Anfrage nur herausnehmen, wenn sie seit `min_idle` Sekunden
    /// stabil ist (Debounce) — z. B. um Read-Ahead erst nach Loslassen der
    /// Maus zu starten.
    pub fn take_settled(&mut self, now: f64, min_idle: f64) -> Option<f64> {
        if self.pending.is_some() && now - self.last_request >= min_idle {
            self.pending.take()
        } else {
            None
        }
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn coalesced(&self) -> u64 {
        self.coalesced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(n: usize) -> Arc<Vec<u8>> {
        Arc::new(vec![n as u8; 100]) // 100 Bytes pro Frame
    }

    fn key(frame: i64) -> FrameKey {
        FrameKey {
            path: "/a.mp4".into(),
            w: 10,
            h: 10,
            frame,
            fps_milli: 25000,
        }
    }

    #[test]
    fn hit_and_miss_counting() {
        let mut c = FrameCache::new(1000);
        assert!(c.get(&key(0)).is_none());
        c.insert(key(0), frame(0));
        assert!(c.get(&key(0)).is_some());
        assert_eq!(c.hits(), 1);
        assert_eq!(c.misses(), 1);
    }

    #[test]
    fn evicts_least_recently_used_over_budget() {
        // Budget = 250 Bytes → genau 2 Frames à 100 Bytes (+Overhead ignoriert).
        let mut c = FrameCache::new(250);
        c.insert(key(0), frame(0));
        c.insert(key(1), frame(1));
        // 0 anfassen → 1 wird zum ältesten.
        assert!(c.get(&key(0)).is_some());
        c.insert(key(2), frame(2)); // verdrängt das LRU-Element (key 1)
        assert!(c.contains(&key(0)));
        assert!(!c.contains(&key(1)), "key 1 sollte verdrängt sein");
        assert!(c.contains(&key(2)));
        assert!(c.used_bytes() <= c.budget());
        assert_eq!(c.evictions(), 1);
    }

    #[test]
    fn shrinking_budget_evicts_immediately() {
        let mut c = FrameCache::new(1000);
        for i in 0..8 {
            c.insert(key(i), frame(i as usize));
        }
        assert_eq!(c.len(), 8);
        c.set_budget(250);
        assert!(c.used_bytes() <= 250);
        assert!(c.len() <= 2);
    }

    #[test]
    fn oversized_frame_is_not_cached() {
        let mut c = FrameCache::new(50); // kleiner als ein 100-Byte-Frame
        c.insert(key(0), frame(0));
        assert!(c.is_empty());
    }

    #[test]
    fn invalidate_path_drops_only_matching() {
        let mut c = FrameCache::new(10_000);
        c.insert(key(0), frame(0));
        let other = FrameKey {
            path: "/b.mp4".into(),
            w: 10,
            h: 10,
            frame: 0,
            fps_milli: 25000,
        };
        c.insert(other.clone(), frame(1));
        c.invalidate_path("/a.mp4");
        assert!(!c.contains(&key(0)));
        assert!(c.contains(&other));
    }

    #[test]
    fn frame_key_quantizes_time_to_grid() {
        // 25 fps: t=0.50 → Frame 12 oder 13 (round(12.5)=13 in Rust, ties→even? nein, round = half away from zero → 13)
        let k = FrameKey::at_time("/a.mp4", 320, 180, 25.0, 0.52);
        assert_eq!(k.frame, 13); // round(0.52*25)=round(13.0)=13
        assert_eq!(k.fps_milli, 25000);
    }

    #[test]
    fn seek_reuse_for_small_forward_jump() {
        // Decoder bei 10.0, Ziel 10.1, Vorwärtswiedergabe → weiterlesen.
        assert_eq!(
            seek_decision(10.0, 10.1, 25.0, 1.0, true, 1.0),
            SeekAction::Reuse
        );
    }

    #[test]
    fn seek_restart_on_rewind() {
        // Ziel liegt VOR der dekodierten Zeit (Rücksprung) → Neustart.
        assert_eq!(
            seek_decision(10.0, 9.5, 25.0, 1.0, true, 1.0),
            SeekAction::Restart
        );
    }

    #[test]
    fn seek_restart_on_large_forward_jump_when_paused() {
        // Pausiert: nur 0.25 s Vorsprung toleriert → 2 s Sprung = Neustart.
        assert_eq!(
            seek_decision(10.0, 12.0, 25.0, 1.0, false, 0.0),
            SeekAction::Restart
        );
        // Während Wiedergabe sind 1.5 s erlaubt → 1.0 s Vorsprung = Reuse.
        assert_eq!(
            seek_decision(10.0, 11.0, 25.0, 1.0, true, 1.0),
            SeekAction::Reuse
        );
    }

    #[test]
    fn scrub_coalescer_keeps_only_latest() {
        let mut sc = ScrubCoalescer::default();
        sc.request(1.0, 0.0);
        sc.request(2.0, 0.01);
        sc.request(3.0, 0.02);
        // Drei Anfragen, zwei wurden übersprungen.
        assert_eq!(sc.coalesced(), 2);
        // Es wird NUR die letzte bedient.
        assert_eq!(sc.take(), Some(3.0));
        assert!(!sc.has_pending());
        assert_eq!(sc.take(), None);
    }

    #[test]
    fn scrub_coalescer_debounces_until_settled() {
        let mut sc = ScrubCoalescer::default();
        sc.request(5.0, 1.00);
        // Noch nicht stabil (0.05 s < 0.12 s) → nichts.
        assert_eq!(sc.take_settled(1.05, 0.12), None);
        // Stabil → liefert die Position.
        assert_eq!(sc.take_settled(1.20, 0.12), Some(5.0));
    }
}
