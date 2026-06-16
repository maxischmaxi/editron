//! Interop-Roundtrip- und Fixture-Tests.
//!
//! Interop ist binär — entweder der Schnitt kommt frame-genau drüben an oder
//! das Feature ist wertlos. Diese Tests sichern genau das ab:
//!
//! * **Verlustfreier Roundtrip** Editron→OTIO→Editron für alle Kernfelder
//!   (Start/Dauer/Quell-In, Marker, Auflösung/Rate) — auch an krummen Raten
//!   wie 23,976 (rationale Genauigkeit).
//! * **EDL-Timecode-Mathematik** und Cut-Roundtrip an 25 fps.
//! * **FCPXML-Struktur** (Wohlgeformtheit + rationale Zeitangaben; bei
//!   vorhandenem `xmllint` echte XML-Validierung).
//! * **Fixtures** im Resolve/Premiere-Stil (`tests/fixtures/`) parsen sauber.

#![cfg(test)]

use super::*;
use crate::core::sequence::{FrameRate, SequenceSettings};
use crate::core::timeline::{new_track, test_clip, TimelineStore, TimelineTrack, TrackKind};
use crate::core::types::{AudioStreamInfo, MediaAsset, MediaInfo, MediaKind, VideoStreamInfo};

const NTSC_24: FrameRate = FrameRate::new(24000, 1001);

// ----------------------------------------------------------- Test-Helfer

fn asset(id: &str, name: &str, path: &str, video: bool, audio: bool, fps: f64) -> MediaAsset {
    MediaAsset {
        extra: Default::default(),
        id: id.into(),
        path: path.into(),
        name: name.into(),
        kind: if video { MediaKind::Video } else { MediaKind::Audio },
        info: MediaInfo {
            path: path.into(),
            file_name: std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| name.into()),
            container: "mov".into(),
            duration_sec: 100.0,
            size_bytes: 1234,
            video: if video {
                vec![VideoStreamInfo {
                    index: 0,
                    codec: "prores".into(),
                    width: 1920,
                    height: 1080,
                    fps,
                    pix_fmt: None,
                    bitrate: None,
                    bit_depth: 8,
                    color_transfer: None,
                    color_primaries: None,
                    color_space: None,
                    color_range: None,
                }]
            } else {
                Vec::new()
            },
            audio: if audio {
                vec![AudioStreamInfo {
                    index: if video { 1 } else { 0 },
                    codec: "pcm".into(),
                    channels: 2,
                    sample_rate: 48_000,
                    bitrate: None,
                }]
            } else {
                Vec::new()
            },
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
    }
}

fn clip(
    track_id: &str,
    asset_id: &str,
    name: &str,
    kind: TrackKind,
    start_f: i64,
    dur_f: i64,
    src_f: i64,
    rate: FrameRate,
) -> crate::core::timeline::TimelineClip {
    let mut c = test_clip(track_id);
    c.asset_id = asset_id.into();
    c.name = name.into();
    c.kind = kind;
    c.start = rate.time_of_frame(start_f as f64);
    c.duration = rate.time_of_frame(dur_f as f64);
    c.src_in = rate.time_of_frame(src_f as f64);
    c.src_duration = rate.time_of_frame(2400.0);
    c
}

fn store_with(
    rate: FrameRate,
    drop_frame: bool,
    tracks: Vec<TimelineTrack>,
    clips: Vec<crate::core::timeline::TimelineClip>,
    markers: Vec<crate::core::marker::Marker>,
) -> TimelineStore {
    let mut s = TimelineStore::default();
    s.load_document(
        Some(SequenceSettings {
            rate,
            width: 1920,
            height: 1080,
            drop_frame,
        }),
        tracks,
        clips,
        Vec::new(),
        markers,
        0.0,
        None,
        None,
        40.0,
        true,
        Vec::new(),
        0.0,
        None,
    );
    s
}

/// Frame-Index einer Sekunde an der Rate (Vergleichsbasis).
fn fr(rate: FrameRate, t: f64) -> i64 {
    rate.frame_round(t)
}

/// Findet im Import-Ergebnis den Clip gleicher Art an gleichem Record-Frame.
fn find_clip<'a>(
    build: &'a ImportBuild,
    rate: FrameRate,
    kind: TrackKind,
    start_f: i64,
) -> &'a crate::core::timeline::TimelineClip {
    build
        .clips
        .iter()
        .find(|c| c.kind == kind && fr(rate, c.start) == start_f)
        .unwrap_or_else(|| panic!("kein {kind:?}-Clip bei Frame {start_f}"))
}

// ------------------------------------------------------------- OTIO

#[test]
fn otio_roundtrip_is_frame_lossless_at_23_976() {
    let rate = NTSC_24;
    let v = new_track(TrackKind::Video);
    let a = new_track(TrackKind::Audio);
    let (vid, aid) = (v.id.clone(), a.id.clone());
    let clips = vec![
        clip(&vid, "A1", "A001_C001.mov", TrackKind::Video, 0, 100, 24, rate),
        clip(&vid, "A2", "A001_C002.mov", TrackKind::Video, 150, 100, 0, rate),
        clip(&aid, "A1", "A001_C001.mov", TrackKind::Audio, 0, 100, 24, rate),
    ];
    let markers = vec![{
        let mut m = crate::core::marker::Marker::new(rate.time_of_frame(50.0));
        m.name = "Schnitt".into();
        m.duration = rate.time_of_frame(20.0);
        m.color = crate::core::marker::MarkerColor::Cyan;
        m
    }];
    let store = store_with(rate, false, vec![v, a], clips, markers);
    let assets = vec![
        asset("A1", "A001_C001.mov", "/media/A001_C001.mov", true, true, rate.fps()),
        asset("A2", "A001_C002.mov", "/media/A001_C002.mov", true, false, rate.fps()),
    ];

    let (text, warnings) = export_text(InteropFormat::Otio, &store, &assets, "Demo 23.976");
    assert!(warnings.is_empty(), "unerwartete Export-Warnungen: {warnings:?}");

    let build = import_text(InteropFormat::Otio, &text, &[]).expect("OTIO parst");

    // Rate, Auflösung, Drop-Frame verlustfrei (über Editron-Metadaten).
    assert_eq!(build.settings.rate, rate);
    assert_eq!((build.settings.width, build.settings.height), (1920, 1080));
    assert!(!build.settings.drop_frame);

    // Clip-Zeiten frame-genau.
    let v0 = find_clip(&build, rate, TrackKind::Video, 0);
    assert_eq!(fr(rate, v0.duration), 100);
    assert_eq!(fr(rate, v0.src_in), 24);
    let v1 = find_clip(&build, rate, TrackKind::Video, 150);
    assert_eq!(fr(rate, v1.duration), 100);
    assert_eq!(fr(rate, v1.src_in), 0);
    let a0 = find_clip(&build, rate, TrackKind::Audio, 0);
    assert_eq!(fr(rate, a0.duration), 100);
    assert_eq!(fr(rate, a0.src_in), 24);

    // Marker frame-genau + Farbe/Name/Dauer.
    assert_eq!(build.markers.len(), 1);
    let m = &build.markers[0];
    assert_eq!(fr(rate, m.time), 50);
    assert_eq!(fr(rate, m.duration), 20);
    assert_eq!(m.name, "Schnitt");
    assert_eq!(m.color, crate::core::marker::MarkerColor::Cyan);

    // Zwei verschiedene Medien, beide offline (Pfade existieren im Test nicht).
    assert_eq!(build.summary.media_total, 2);
    assert_eq!(build.summary.media_offline, 2);
    // A/V-Verknüpfung des ersten Clips erkannt.
    assert!(v0.link_id.is_some() && v0.link_id == a0.link_id);
}

#[test]
fn otio_dissolve_survives_roundtrip() {
    let rate = FrameRate::PAL_25;
    let v = new_track(TrackKind::Video);
    let vid = v.id.clone();
    let clips = vec![
        clip(&vid, "A1", "a.mov", TrackKind::Video, 0, 100, 24, rate),
        clip(&vid, "A2", "b.mov", TrackKind::Video, 100, 100, 24, rate),
    ];
    let mut store = store_with(rate, false, vec![v], clips, Vec::new());
    // Cross-Dissolve direkt an die Kante setzen (ID-Bezug auf die Clips).
    let a_id = store.clips[0].id.clone();
    let b_id = store.clips[1].id.clone();
    store.transitions.push(crate::core::transitions::Transition::new(
        crate::core::transitions::TransitionKind::CrossDissolve,
        Some(a_id),
        Some(b_id),
        rate.time_of_frame(24.0),
    ));
    let assets = vec![
        asset("A1", "a.mov", "/m/a.mov", true, false, rate.fps()),
        asset("A2", "b.mov", "/m/b.mov", true, false, rate.fps()),
    ];

    let (text, _w) = export_text(InteropFormat::Otio, &store, &assets, "Dissolve");
    assert!(text.contains("\"Transition.1\""), "OTIO-Transition fehlt");
    assert!(text.contains("SMPTE_Dissolve"));

    let build = import_text(InteropFormat::Otio, &text, &[]).expect("parst");
    assert_eq!(build.transitions.len(), 1, "Übergang muss überleben");
    assert_eq!(
        fr(rate, build.transitions[0].duration),
        24,
        "Übergangsdauer frame-genau"
    );
}

// ------------------------------------------------------------- EDL

#[test]
fn edl_cut_roundtrip_is_lossless_at_25() {
    let rate = FrameRate::PAL_25;
    let v = new_track(TrackKind::Video);
    let a = new_track(TrackKind::Audio);
    let (vid, aid) = (v.id.clone(), a.id.clone());
    let clips = vec![
        clip(&vid, "A1", "shot1.mov", TrackKind::Video, 0, 50, 0, rate),
        clip(&vid, "A2", "shot2.mov", TrackKind::Video, 50, 75, 125, rate),
        clip(&aid, "A1", "shot1.mov", TrackKind::Audio, 0, 50, 0, rate),
    ];
    let store = store_with(rate, false, vec![v, a], clips, Vec::new());
    let assets = vec![
        asset("A1", "shot1.mov", "/m/shot1.mov", true, true, 25.0),
        asset("A2", "shot2.mov", "/m/shot2.mov", true, false, 25.0),
    ];

    let (text, _w) = export_text(InteropFormat::Edl, &store, &assets, "EDL Demo");
    assert!(text.contains("FCM: NON-DROP FRAME"));
    assert!(text.contains("FROM CLIP NAME: shot1.mov"));

    let build = import_text(InteropFormat::Edl, &text, &[]).expect("EDL parst");
    let v0 = find_clip(&build, rate, TrackKind::Video, 0);
    assert_eq!(fr(rate, v0.duration), 50);
    let v1 = find_clip(&build, rate, TrackKind::Video, 50);
    assert_eq!(fr(rate, v1.duration), 75);
    assert_eq!(fr(rate, v1.src_in), 125);
    let a0 = find_clip(&build, rate, TrackKind::Audio, 0);
    assert_eq!(fr(rate, a0.duration), 50);
}

#[test]
fn edl_drop_frame_export_uses_semicolons() {
    let rate = FrameRate::new(30000, 1001);
    let v = new_track(TrackKind::Video);
    let vid = v.id.clone();
    let clips = vec![clip(&vid, "A1", "x.mov", TrackKind::Video, 0, 30, 0, rate)];
    let store = store_with(rate, true, vec![v], clips, Vec::new());
    let assets = vec![asset("A1", "x.mov", "/m/x.mov", true, false, 29.97)];
    let (text, _w) = export_text(InteropFormat::Edl, &store, &assets, "DF");
    assert!(text.contains("FCM: DROP FRAME"));
    // Drop-Frame nutzt Semikolon vor der Frame-Spalte.
    assert!(text.contains(';'), "Drop-Frame-Timecode fehlt das Semikolon");
}

// ------------------------------------------------------------- FCPXML

/// Alle Werte eines Attributs `name="..."` einsammeln.
fn attr_values(xml: &str, name: &str) -> Vec<String> {
    let needle = format!("{name}=\"");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(pos) = rest.find(&needle) {
        let after = &rest[pos + needle.len()..];
        if let Some(end) = after.find('"') {
            out.push(after[..end].to_string());
            rest = &after[end + 1..];
        } else {
            break;
        }
    }
    out
}

fn is_fcp_time(s: &str) -> bool {
    if s == "0s" {
        return true;
    }
    let Some(body) = s.strip_suffix('s') else {
        return false;
    };
    match body.split_once('/') {
        Some((n, d)) => {
            !n.is_empty()
                && !d.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
                && d.bytes().all(|b| b.is_ascii_digit())
        }
        None => body.bytes().all(|b| b.is_ascii_digit()),
    }
}

#[test]
fn fcpxml_is_well_formed_and_times_are_rational() {
    let rate = NTSC_24;
    let v = new_track(TrackKind::Video);
    let v2 = new_track(TrackKind::Video);
    let a = new_track(TrackKind::Audio);
    let (vid, v2id, aid) = (v.id.clone(), v2.id.clone(), a.id.clone());
    let clips = vec![
        clip(&vid, "A1", "a.mov", TrackKind::Video, 0, 100, 24, rate),
        clip(&v2id, "A2", "b.mov", TrackKind::Video, 40, 60, 0, rate), // höhere Spur
        clip(&aid, "A1", "a.mov", TrackKind::Audio, 0, 100, 24, rate),
    ];
    let store = store_with(rate, false, vec![v, v2, a], clips, Vec::new());
    let assets = vec![
        asset("A1", "a.mov", "/m/a.mov", true, true, rate.fps()),
        asset("A2", "b.mov", "/m/b.mov", true, false, rate.fps()),
    ];
    let (xml, _w) = export_text(InteropFormat::Fcpxml, &store, &assets, "FCP Demo");

    assert!(xml.starts_with("<?xml"));
    assert!(xml.contains("<fcpxml version=\"1.11\">"));
    assert!(xml.contains("frameDuration=\"1001/24000s\""));
    assert!(xml.contains("lane=\""), "höhere Spur muss verbundener Clip sein");

    // Alle Zeitangaben sind rationale FCPXML-Zeiten.
    for attr in ["offset", "duration", "start", "tcStart", "frameDuration"] {
        for v in attr_values(&xml, attr) {
            assert!(is_fcp_time(&v), "{attr}=\"{v}\" ist keine gültige FCPXML-Zeit");
        }
    }

    // Jede asset-clip-Referenz löst auf ein vorhandenes Asset/Format auf.
    let ids: std::collections::HashSet<String> = attr_values(&xml, "id").into_iter().collect();
    for r in attr_values(&xml, "ref") {
        assert!(ids.contains(&r), "ref=\"{r}\" ohne passendes id=");
    }

    // Optional: echte XML-Wohlgeformtheit via xmllint, falls vorhanden.
    if let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR") {
        let tmp = std::path::Path::new(&dir).join("target/test-fcpxml.xml");
        if std::fs::write(&tmp, &xml).is_ok() {
            if let Ok(out) = std::process::Command::new("xmllint")
                .arg("--noout")
                .arg(&tmp)
                .output()
            {
                assert!(
                    out.status.success(),
                    "xmllint meldet ungültiges XML: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

// ------------------------------------------------------------- Fixtures

#[test]
fn parses_resolve_otio_fixture() {
    let text = include_str!("../../../tests/fixtures/resolve_basic.otio");
    let build = import_text(InteropFormat::Otio, text, &[]).expect("Resolve-OTIO parst");
    let rate = FrameRate::new(24, 1);
    assert_eq!(build.settings.rate, rate);
    // V1: zwei Clips (mit Lücke dazwischen), A1: ein Clip.
    let v_clips: Vec<_> = build
        .clips
        .iter()
        .filter(|c| c.kind == TrackKind::Video)
        .collect();
    assert_eq!(v_clips.len(), 2);
    // Erster Clip: src-in Frame 24, Dauer 48 (2 s @ 24).
    let v0 = find_clip(&build, rate, TrackKind::Video, 0);
    assert_eq!(fr(rate, v0.src_in), 24);
    assert_eq!(fr(rate, v0.duration), 48);
    // Zweiter Clip beginnt nach Clip(48) + Gap(12) = Frame 60.
    let v1 = find_clip(&build, rate, TrackKind::Video, 60);
    assert_eq!(fr(rate, v1.duration), 72);
    // Marker importiert.
    assert_eq!(build.markers.len(), 1);
    assert_eq!(fr(rate, build.markers[0].time), 60);
    assert_eq!(build.markers[0].color, crate::core::marker::MarkerColor::Red);
    // Medien als offline (Demo-Pfade existieren nicht).
    assert!(build.summary.media_offline >= 1);
}

#[test]
fn parses_premiere_edl_fixture() {
    let text = include_str!("../../../tests/fixtures/premiere_basic.edl");
    let build = import_text(InteropFormat::Edl, text, &[]).expect("Premiere-EDL parst");
    let rate = FrameRate::PAL_25;
    let v0 = find_clip(&build, rate, TrackKind::Video, 0);
    assert_eq!(fr(rate, v0.duration), 50); // 00:00:02:00 @ 25 = 50 Frames
    let v1 = find_clip(&build, rate, TrackKind::Video, 50);
    assert_eq!(fr(rate, v1.src_in), 125); // 00:00:05:00 = 125 Frames
    assert_eq!(fr(rate, v1.duration), 75); // bis 00:00:08:00 = 200 → 75 Frames
    // Audio-Ereignis (AA → A1) vorhanden.
    assert!(build.clips.iter().any(|c| c.kind == TrackKind::Audio));
}

// ----------------------------------------------- Auslassungen melden

#[test]
fn export_reports_unmappable_clips_as_gaps_with_warning() {
    // Ein nicht abbildbarer Clip (verschachtelte Sequenz) muss als Lücke
    // exportiert UND gemeldet werden — niemals stillschweigend verschwinden.
    let rate = FrameRate::PAL_25;
    let v = new_track(TrackKind::Video);
    let vid = v.id.clone();
    let media_clip = clip(&vid, "A1", "a.mov", TrackKind::Video, 0, 50, 0, rate);
    let mut nest = test_clip(&vid);
    nest.kind = TrackKind::Video;
    nest.asset_id = String::new();
    nest.nest_seq = Some("inner".into());
    nest.name = "Verschachtelt".into();
    nest.start = rate.time_of_frame(50.0);
    nest.duration = rate.time_of_frame(40.0);
    let store = store_with(rate, false, vec![v], vec![media_clip, nest], Vec::new());
    let assets = vec![asset("A1", "a.mov", "/m/a.mov", true, false, 25.0)];
    let (_text, warnings) = export_text(InteropFormat::Otio, &store, &assets, "X");
    assert!(
        warnings.iter().any(|w| w.contains("Lücke")),
        "Nest-Clip muss als Auslassung gemeldet werden: {warnings:?}"
    );
}

#[test]
fn edl_warns_about_extra_video_tracks() {
    // EDL kennt nur eine Video-Spur — höhere Spuren müssen gemeldet werden.
    let rate = FrameRate::PAL_25;
    let v1 = new_track(TrackKind::Video);
    let v2 = new_track(TrackKind::Video);
    let (id1, id2) = (v1.id.clone(), v2.id.clone());
    let clips = vec![
        clip(&id1, "A1", "a.mov", TrackKind::Video, 0, 50, 0, rate),
        clip(&id2, "A2", "b.mov", TrackKind::Video, 0, 50, 0, rate),
    ];
    let store = store_with(rate, false, vec![v1, v2], clips, Vec::new());
    let assets = vec![
        asset("A1", "a.mov", "/m/a.mov", true, false, 25.0),
        asset("A2", "b.mov", "/m/b.mov", true, false, 25.0),
    ];
    let (_text, warnings) = export_text(InteropFormat::Edl, &store, &assets, "X");
    assert!(
        warnings.iter().any(|w| w.contains("Video-Spur")),
        "EDL muss höhere Video-Spuren melden: {warnings:?}"
    );
}

#[test]
fn parses_edl_dissolve_fixture() {
    let text = include_str!("../../../tests/fixtures/resolve_dissolve.edl");
    let build = import_text(InteropFormat::Edl, text, &[]).expect("Dissolve-EDL parst");
    let rate = FrameRate::PAL_25;
    // Zwei Clips + ein Übergang (25-Frame-Dissolve).
    assert_eq!(build.clips.iter().filter(|c| c.kind == TrackKind::Video).count(), 2);
    assert_eq!(build.transitions.len(), 1);
    assert_eq!(fr(rate, build.transitions[0].duration), 25);
}
