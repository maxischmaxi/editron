//! Sequenz-Render-Cache („Render In to Out“): Bereiche der Timeline werden im
//! Hintergrund über den Export-Compositor in eine Intra-Frame-Cache-Datei
//! gerendert; bei Wiedergabe wird der gecachte Bereich aus dieser Datei
//! dekodiert statt live komponiert (ein Decoder statt N Layer-Decoder +
//! CPU-Compositing). Dieser Store hält die gerenderten Segmente und ihre
//! Gültigkeit.
//!
//! **Invalidierung — inhaltsbasiert, nicht global.** Jedes Segment merkt sich
//! eine Signatur ([`RenderCacheStore::range_signature`]) der visuell
//! relevanten Zustände (Sequenz-Einstellungen + überlappende Clips/Übergänge +
//! Asset-Pfade) in seinem Frame-Bereich. Ein Edit erhöht zwar global
//! `TimelineStore::revision`; ob ein Segment *wirklich* veraltet ist, wird
//! über den Signaturvergleich entschieden — ein Schnitt am Sequenzende lässt
//! gecachte Bereiche am Anfang gültig. Der Revisions-Zähler dient nur als
//! billiger „Hat-sich-irgendwas-geändert“-Trigger, der die Neuberechnung der
//! Signaturen auslöst.
//!
//! Teile der Store-API (Aufräum-/Abfrage-Helfer, `codec`-Metadaten) sind
//! vorausschauend und (noch) nicht überall verdrahtet — daher modulweites
//! `allow(dead_code)`.
#![allow(dead_code)]

use crate::core::settings::{AppSettings, RenderCacheCodec};
use crate::core::timeline::{TimelineClip, TimelineStore};
use crate::core::transitions::Transition;
use crate::stores::MediaStore;
use std::path::{Path, PathBuf};

/// Ablageordner für Render-Cache-Dateien: konfiguriert oder App-Cache.
pub fn render_cache_dir(settings: &AppSettings) -> PathBuf {
    settings
        .render_cache_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("editron")
                .join("rendercache")
        })
}

/// Ein gerendertes Cache-Segment der Sequenz.
pub struct CacheSegment {
    /// Erster gecachter Sequenz-Frame (inklusive).
    pub start_frame: i64,
    /// Erster nicht mehr gecachter Sequenz-Frame (exklusive).
    pub end_frame: i64,
    /// Cache-Datei (Intra-Frame, an jeder Stelle seekbar).
    pub file: PathBuf,
    /// Signatur des Inhalts zum Render-Zeitpunkt (Invalidierungs-Basis).
    pub content_hash: u64,
    pub codec: RenderCacheCodec,
}

/// Laufender Hintergrund-Render (für die Render-Leiste).
#[derive(Clone)]
pub struct RenderProgress {
    pub start_frame: i64,
    pub end_frame: i64,
    pub pct: f32,
    /// Job-ID des Dispatchers (zum Abbrechen).
    pub job_id: u64,
}

#[derive(Default)]
pub struct RenderCacheStore {
    pub segments: Vec<CacheSegment>,
    /// Revisions-Paar (timeline, media), für das `valid` aktuell ist.
    cached_revs: Option<(u64, u64)>,
    /// Pro Segment: Datei noch inhaltsgültig? (Index-gleich zu `segments`.)
    valid: Vec<bool>,
    /// Aktuell im Hintergrund gerenderter Bereich, falls einer läuft.
    pub rendering: Option<RenderProgress>,
}

/// Zeit-Bereiche der Sequenz (Sekunden), die vom Vorrendern profitieren — also
/// NICHT trivial in Echtzeit abspielbar sind: überlappende Video-Layer, Clips
/// mit Effekten/Farbkorrektur/Transform-Animation/Speed/Reverse/Freeze, Titel-/
/// Untertitel-Generatoren und Übergänge. Über einfachen Einzelschnitten zeigt
/// die Render-Leiste nichts (kein „Meer aus Rot“). Ergebnis ist zusammengeführt
/// und aufsteigend.
pub fn complex_spans(timeline: &TimelineStore) -> Vec<(f64, f64)> {
    use crate::core::timeline::TrackKind;
    let mut spans: Vec<(f64, f64)> = Vec::new();

    let vid: Vec<&TimelineClip> = timeline
        .clips
        .iter()
        .filter(|c| c.enabled && matches!(c.kind, TrackKind::Video))
        .collect();

    // (a) Per-Clip-Komplexität (inkl. Generatoren auf allen Spuren).
    for c in timeline.clips.iter().filter(|c| c.enabled) {
        if clip_is_complex(c) {
            spans.push((c.start, c.end()));
        }
    }

    // (b) Überlappende Video-Layer (≥ 2 gleichzeitig): Sweep über die Kanten.
    let mut edges: Vec<f64> = Vec::new();
    for c in &vid {
        edges.push(c.start);
        edges.push(c.end());
    }
    edges.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    edges.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    for w in edges.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b - a <= 1e-9 {
            continue;
        }
        let mid = (a + b) * 0.5;
        let count = vid.iter().filter(|c| c.start <= mid && c.end() > mid).count();
        if count >= 2 {
            spans.push((a, b));
        }
    }

    // (c) Übergänge.
    for tr in &timeline.transitions {
        if let Some((w0, w1)) = timeline.transition_window(tr) {
            spans.push((w0, w1));
        }
    }

    merge_spans(spans)
}

fn clip_is_complex(c: &TimelineClip) -> bool {
    !c.effects.is_empty()
        || !c.grade.is_default()
        || !c.fx.is_default()
        || (c.speed - 1.0).abs() > 1e-6
        || c.reverse
        || c.freeze
        || c.title.is_some()
        || c.subtitle.is_some()
}

fn merge_spans(mut spans: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    spans.retain(|(a, b)| b - a > 1e-9);
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (a, b) in spans {
        if let Some(last) = out.last_mut() {
            if a <= last.1 + 1e-9 {
                last.1 = last.1.max(b);
                continue;
            }
        }
        out.push((a, b));
    }
    out
}

/// Zustand eines Frame-Bereichs in der Render-Leiste.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RangeState {
    /// Gültig gecacht (grün).
    Cached,
    /// Wird gerade gerendert (gelb).
    Rendering,
    /// Inhalt vorhanden, aber nicht (mehr) gecacht (rot).
    Dirty,
}

impl RenderCacheStore {
    /// Signatur der visuell relevanten Zustände in einem Frame-Bereich.
    /// Identisch berechnet beim Rendern (gespeichert) und beim Refresh
    /// (verglichen). Speed/Proxy fließen NICHT als Bildänderung ein: der
    /// Cache rendert immer das Original in Sequenzauflösung.
    pub fn range_signature(
        timeline: &TimelineStore,
        media: &MediaStore,
        start_frame: i64,
        end_frame: i64,
    ) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let rate = timeline.settings.rate;
        timeline.settings.width.hash(&mut h);
        timeline.settings.height.hash(&mut h);
        rate.num.hash(&mut h);
        rate.den.hash(&mut h);
        let start_sec = rate.time_of_frame(start_frame as f64);
        let end_sec = rate.time_of_frame(end_frame as f64);

        // Überlappende, aktive Clips — deterministisch nach ID sortiert.
        let mut clips: Vec<&TimelineClip> = timeline
            .clips
            .iter()
            .filter(|c| c.enabled && c.start < end_sec && c.end() > start_sec)
            .collect();
        clips.sort_by(|a, b| a.id.cmp(&b.id));
        for c in clips {
            if let Ok(j) = serde_json::to_string(c) {
                j.hash(&mut h);
            }
            // Asset-Pfad/Offline-Zustand → Relink/Verlust invalidiert.
            if let Some(a) = media.asset(&c.asset_id) {
                a.path.hash(&mut h);
                a.offline.hash(&mut h);
            }
        }

        // Überlappende Übergänge.
        let mut trs: Vec<&Transition> = timeline
            .transitions
            .iter()
            .filter(|tr| {
                timeline
                    .transition_window(tr)
                    .map(|(w0, w1)| w0 < end_sec && w1 > start_sec)
                    .unwrap_or(false)
            })
            .collect();
        trs.sort_by(|a, b| a.id.cmp(&b.id));
        for tr in trs {
            if let Ok(j) = serde_json::to_string(tr) {
                j.hash(&mut h);
            }
        }
        h.finish()
    }

    /// Gültigkeit aller Segmente neu bewerten, wenn sich seit dem letzten
    /// Refresh die Revision geändert hat. Billig, wenn nichts passiert ist.
    pub fn refresh(&mut self, timeline: &TimelineStore, media: &MediaStore) {
        let revs = (timeline.revision, media.revision);
        if self.cached_revs == Some(revs) {
            return;
        }
        self.cached_revs = Some(revs);
        self.valid = self
            .segments
            .iter()
            .map(|s| {
                Self::range_signature(timeline, media, s.start_frame, s.end_frame) == s.content_hash
            })
            .collect();
    }

    /// Neues Segment aufnehmen; überlappende ersetzen. Liefert die Dateien der
    /// ersetzten Segmente zurück (vom Aufrufer zu löschen).
    pub fn add_segment(&mut self, seg: CacheSegment) -> Vec<PathBuf> {
        let (a, b) = (seg.start_frame, seg.end_frame);
        let mut removed = Vec::new();
        self.segments.retain(|s| {
            let overlaps = s.start_frame < b && s.end_frame > a;
            if overlaps {
                removed.push(s.file.clone());
            }
            !overlaps
        });
        self.segments.push(seg);
        self.cached_revs = None; // erzwingt Neubewertung beim nächsten refresh
        removed
    }

    /// Gültige Cache-Datei + lokaler Frame für einen Sequenz-Frame, falls ein
    /// inhaltsgültiges Segment ihn abdeckt. `refresh` muss vorher gelaufen sein.
    pub fn valid_file_at(&self, frame: i64) -> Option<(&Path, i64)> {
        for (s, ok) in self.segments.iter().zip(self.valid.iter()) {
            if *ok && frame >= s.start_frame && frame < s.end_frame {
                return Some((s.file.as_path(), frame - s.start_frame));
            }
        }
        None
    }

    /// Zusammengeführte, gültig gecachte Frame-Bereiche (für die Render-Leiste).
    pub fn cached_spans(&self) -> Vec<(i64, i64)> {
        let mut spans: Vec<(i64, i64)> = self
            .segments
            .iter()
            .zip(self.valid.iter())
            .filter(|(_, ok)| **ok)
            .map(|(s, _)| (s.start_frame, s.end_frame))
            .collect();
        spans.sort_by_key(|(a, _)| *a);
        // Benachbarte/überlappende verschmelzen.
        let mut merged: Vec<(i64, i64)> = Vec::new();
        for (a, b) in spans {
            if let Some(last) = merged.last_mut() {
                if a <= last.1 {
                    last.1 = last.1.max(b);
                    continue;
                }
            }
            merged.push((a, b));
        }
        merged
    }

    /// Existiert (irgend)ein Segment, das diesen Frame abdeckt — gültig oder
    /// veraltet? (Für die Aufräumlogik.)
    pub fn any_segment_covers(&self, frame: i64) -> bool {
        self.segments
            .iter()
            .any(|s| frame >= s.start_frame && frame < s.end_frame)
    }

    /// Veraltete Segmente entfernen; ihre Dateien zum Löschen zurückgeben.
    pub fn drop_invalid(&mut self) -> Vec<PathBuf> {
        let mut removed = Vec::new();
        let valid = std::mem::take(&mut self.valid);
        let mut iter = valid.into_iter();
        self.segments.retain(|s| {
            let keep = iter.next().unwrap_or(true);
            if !keep {
                removed.push(s.file.clone());
            }
            keep
        });
        self.valid = vec![true; self.segments.len()];
        removed
    }

    /// Alles verwerfen; alle Dateien zum Löschen zurückgeben.
    pub fn clear(&mut self) -> Vec<PathBuf> {
        self.cached_revs = None;
        self.valid.clear();
        self.segments.drain(..).map(|s| s.file).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty() && self.rendering.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::timeline::{TimelineClip, TrackKind};
    use crate::core::types::{MediaAsset, MediaInfo, MediaKind};
    use crate::stores::MediaStore;

    fn clip(id: &str, start: f64, duration: f64) -> TimelineClip {
        TimelineClip {
            extra: Default::default(),
            id: id.into(),
            track_id: "v1".into(),
            asset_id: "asset".into(),
            name: "Clip".into(),
            kind: TrackKind::Video,
            start,
            duration,
            src_in: 0.0,
            src_duration: 100.0,
            link_id: None,
            enabled: true,
            gain_db: 0.0,
            fx: Default::default(),
            grade: Default::default(),
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

    fn media_with_asset() -> MediaStore {
        let mut m = MediaStore::default();
        m.add_asset(MediaAsset {
            extra: Default::default(),
            id: "asset".into(),
            path: "/tmp/asset.mp4".into(),
            name: "asset".into(),
            kind: MediaKind::Video,
            info: MediaInfo {
                path: "/tmp/asset.mp4".into(),
                file_name: "asset.mp4".into(),
                container: "mp4".into(),
                duration_sec: 100.0,
                size_bytes: 1,
                video: Vec::new(),
                audio: Vec::new(),
                recorded_at: None,
            },
            thumbnail_path: None,
            imported_at: 0.0,
            bin_id: crate::core::bin::ROOT_BIN_ID.to_string(),
            label: None,
            offline: false,
            markers: Vec::new(),
            proxy_path: None,
            proxy_src_mtime: None,
            proxy_offline: false,
        });
        m
    }

    /// Zwei weit getrennte Clips; ein Edit am einen invalidiert nur den
    /// überlappenden Bereich, nicht den anderen.
    #[test]
    fn editing_clip_invalidates_only_overlapping_range() {
        let media = media_with_asset();
        let mut tl = TimelineStore::default();
        // 25 fps Standardrate.
        tl.clips.push(clip("a", 0.0, 2.0)); // Frames 0..50
        tl.clips.push(clip("b", 10.0, 2.0)); // Frames 250..300

        let fps = tl.settings.rate.fps();
        assert!((fps - 25.0).abs() < 1e-9);

        let sig_a = RenderCacheStore::range_signature(&tl, &media, 0, 50);
        let sig_b = RenderCacheStore::range_signature(&tl, &media, 250, 300);

        // Clip A verschieben (Edit im Bereich A).
        tl.clips[0].start = 0.5;
        let sig_a2 = RenderCacheStore::range_signature(&tl, &media, 0, 50);
        let sig_b2 = RenderCacheStore::range_signature(&tl, &media, 250, 300);

        assert_ne!(sig_a, sig_a2, "Bereich A muss nach dem Edit ungültig sein");
        assert_eq!(sig_b, sig_b2, "Bereich B darf NICHT invalidiert werden");
    }

    #[test]
    fn refresh_marks_stale_segment_and_keeps_valid_one() {
        let media = media_with_asset();
        let mut tl = TimelineStore::default();
        tl.clips.push(clip("a", 0.0, 2.0));
        tl.clips.push(clip("b", 10.0, 2.0));

        let mut store = RenderCacheStore::default();
        store.add_segment(CacheSegment {
            start_frame: 0,
            end_frame: 50,
            file: PathBuf::from("/tmp/seg_a.mov"),
            content_hash: RenderCacheStore::range_signature(&tl, &media, 0, 50),
            codec: RenderCacheCodec::ProresProxy,
        });
        store.add_segment(CacheSegment {
            start_frame: 250,
            end_frame: 300,
            file: PathBuf::from("/tmp/seg_b.mov"),
            content_hash: RenderCacheStore::range_signature(&tl, &media, 250, 300),
            codec: RenderCacheCodec::ProresProxy,
        });

        store.refresh(&tl, &media);
        assert!(store.valid_file_at(10).is_some());
        assert!(store.valid_file_at(260).is_some());
        assert_eq!(store.cached_spans().len(), 2);

        // Edit an Clip A + Revision erhöhen → Refresh muss A verwerfen, B halten.
        tl.clips[0].start = 0.5;
        tl.revision += 1;
        store.refresh(&tl, &media);

        assert!(store.valid_file_at(10).is_none(), "A muss invalidiert sein");
        assert!(store.valid_file_at(260).is_some(), "B muss gültig bleiben");
        let spans = store.cached_spans();
        assert_eq!(spans, vec![(250, 300)]);
    }

    #[test]
    fn local_frame_offset_is_relative_to_segment_start() {
        let media = media_with_asset();
        let mut tl = TimelineStore::default();
        tl.clips.push(clip("a", 0.0, 20.0));
        let mut store = RenderCacheStore::default();
        store.add_segment(CacheSegment {
            start_frame: 100,
            end_frame: 200,
            file: PathBuf::from("/tmp/seg.mov"),
            content_hash: RenderCacheStore::range_signature(&tl, &media, 100, 200),
            codec: RenderCacheCodec::ProresProxy,
        });
        store.refresh(&tl, &media);
        let (path, local) = store.valid_file_at(150).unwrap();
        assert_eq!(path, Path::new("/tmp/seg.mov"));
        assert_eq!(local, 50);
    }

    #[test]
    fn complex_spans_flag_overlaps_and_effects_not_plain_cuts() {
        let mut tl = TimelineStore::default();
        // Zwei aneinandergrenzende, schlichte Clips auf EINER Spur (Schnitt) —
        // kein Overlap, keine Effekte → kein komplexer Bereich.
        tl.clips.push(clip("a", 0.0, 2.0));
        tl.clips.push(clip("b", 2.0, 2.0));
        assert!(
            complex_spans(&tl).is_empty(),
            "schlichte Schnitte brauchen keinen Render-Cache"
        );

        // Clip mit Effekt → sein Span wird komplex.
        let mut c = clip("fx", 5.0, 2.0);
        c.reverse = true;
        tl.clips.push(c);
        let spans = complex_spans(&tl);
        assert!(spans.iter().any(|(a, b)| *a <= 5.0 && *b >= 7.0));

        // Zwei überlappende Clips auf VERSCHIEDENEN Spuren → Overlap-Bereich.
        let mut over = clip("v2", 0.5, 1.0);
        over.track_id = "v2".into();
        tl.clips.push(over);
        let spans = complex_spans(&tl);
        assert!(
            spans.iter().any(|(a, b)| *a <= 0.6 && *b >= 1.4),
            "überlappende Layer sind vorrender-relevant"
        );
    }

    #[test]
    fn adding_overlapping_segment_replaces_and_returns_old_file() {
        let media = media_with_asset();
        let mut tl = TimelineStore::default();
        tl.clips.push(clip("a", 0.0, 20.0));
        let mut store = RenderCacheStore::default();
        store.add_segment(CacheSegment {
            start_frame: 0,
            end_frame: 100,
            file: PathBuf::from("/tmp/old.mov"),
            content_hash: 0,
            codec: RenderCacheCodec::ProresProxy,
        });
        let removed = store.add_segment(CacheSegment {
            start_frame: 50,
            end_frame: 150,
            file: PathBuf::from("/tmp/new.mov"),
            content_hash: 0,
            codec: RenderCacheCodec::ProresProxy,
        });
        assert_eq!(removed, vec![PathBuf::from("/tmp/old.mov")]);
        assert_eq!(store.segments.len(), 1);
    }
}
