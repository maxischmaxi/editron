    use super::*;
    // Die Produktion wohnt jetzt in Submodulen; das Test-Modul braucht seine
    // externen Abhängigkeiten explizit (super::* bringt nur die re-exportierten
    // Export-Items, nicht die crate-/std-Imports der Submodule).
    use crate::core::animation::AnimatedParam;
    use crate::core::timeline::{TimelineClip, TimelineStore, TimelineTrack, TrackKind};
    use crate::core::transitions::TransitionRole;
    use crate::core::types::{MediaAsset, MediaInfo, MediaKind, VideoStreamInfo};
    use crate::services::ServiceEvent;
    use crate::stores::MediaStore;
    use std::collections::{HashMap, HashSet};
    use std::process::Command;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    fn track(id: &str, kind: TrackKind) -> TimelineTrack {
        TimelineTrack {
            extra: Default::default(),
            id: id.into(),
            kind,
            muted: false,
            solo: false,
            locked: false,
            gain_db: 0.0,
            pan: 0.0,
            sync_lock: false,
            targeted: false,
            source_patched: false,
            subtitle_style: None,
            effects: Vec::new(),
            volume_auto: crate::core::animation::AnimatedParam::fixed(0.0),
            pan_auto: crate::core::animation::AnimatedParam::fixed(0.0),
            height: None,
        }
    }

    fn clip(id: &str, track_id: &str, kind: TrackKind, asset: &str, start: f64, dur: f64) -> TimelineClip {
        TimelineClip {
            extra: Default::default(),
            id: id.into(),
            track_id: track_id.into(),
            asset_id: asset.into(),
            name: id.into(),
            kind,
            start,
            duration: dur,
            src_in: 0.0,
            src_duration: 3600.0,
            link_id: None,
            enabled: true,
            gain_db: 0.0,
            fx: Default::default(),
            grade: Default::default(),
            effects: Vec::new(),
            title: None,
            subtitle: None,
            adjustment: None,
            speed: crate::core::animation::AnimatedParam::fixed(1.0),
            reverse: false,
            freeze: false,
            markers: Vec::new(),
            nest_seq: None,
            multicam: None,
            blend_mode: crate::core::compose::BlendMode::default(),
        }
    }

    fn video_asset(id: &str, path: &str) -> MediaAsset {
        MediaAsset {
            extra: Default::default(),
            id: id.into(),
            path: path.into(),
            name: path.into(),
            kind: MediaKind::Video,
            info: MediaInfo {
                path: path.into(),
                file_name: path.into(),
                container: "mov".into(),
                duration_sec: 3600.0,
                size_bytes: 1,
                video: vec![VideoStreamInfo {
                    index: 0,
                    codec: "h264".into(),
                    width: 1920,
                    height: 1080,
                    fps: 25.0,
                    pix_fmt: None,
                    bitrate: None,
                    bit_depth: 8,
                    color_transfer: None,
                    color_primaries: None,
                    color_space: None,
                    color_range: None,
                }],
                audio: vec![crate::core::types::AudioStreamInfo {
                    index: 1,
                    codec: "aac".into(),
                    channels: 2,
                    sample_rate: 48000,
                    bitrate: None,
                }],
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
            image_seq: None,
        }
    }

    fn test_settings() -> ExportSettings {
        ExportSettings {
            container: container("mp4"),
            video: Some(default_video("h264", 1280, 720, 25.0)),
            audio: Some(default_audio("aac", None)),
            loudness: None,
            use_in_out: false,
            audio_stems: false,
            subtitles: SubtitleMode::None,
            image_start: 1,
            output: "/tmp/out.mp4".into(),
        }
    }

    fn state_with(
        tracks: Vec<TimelineTrack>,
        clips: Vec<TimelineClip>,
        assets: Vec<MediaAsset>,
    ) -> (TimelineStore, MediaStore) {
        let mut tl = TimelineStore::default();
        tl.tracks = tracks;
        tl.clips = clips;
        let mut media = MediaStore::default();
        for a in assets {
            media.add_asset(a);
        }
        (tl, media)
    }

    #[test]
    fn export_always_uses_original_never_proxy() {
        // Kern-Invariante des Proxy-Workflows: Auch bei global aktivem Proxy-
        // Modus und vorhandenem, gültigem Proxy referenziert der Render-Plan
        // ausschließlich die ORIGINALdatei — der Export darf nie versehentlich
        // Proxy-Qualität ausgeben.
        let mut asset = video_asset("A", "/orig/clip.mp4");
        asset.proxy_path = Some("/proxies/clip_proxy.mov".into());
        asset.proxy_offline = false; // gültiger Proxy
        let (tl, mut media) = state_with(
            vec![track("v1", TrackKind::Video), track("a1", TrackKind::Audio)],
            vec![
                clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0),
                clip("au", "a1", TrackKind::Audio, "A", 0.0, 4.0),
            ],
            vec![asset],
        );
        media.use_proxies = true; // Proxy-Modus aktiv (würde die Vorschau lenken)
        assert!(media.asset("A").unwrap().has_valid_proxy());
        // Decode-Pfad der VORSCHAU nähme hier den Proxy …
        assert_eq!(media.asset("A").unwrap().decode_path(true), "/proxies/clip_proxy.mov");

        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        // … der EXPORT-Plan dagegen niemals.
        let video_paths: Vec<&str> = plan
            .segments
            .iter()
            .flat_map(|s| s.layers.iter())
            .map(|l| l.path.as_str())
            .collect();
        assert!(!video_paths.is_empty(), "Video-Segmente erwartet");
        for p in &video_paths {
            assert_eq!(*p, "/orig/clip.mp4", "Export-Video muss das Original nutzen");
        }
        assert!(!plan.audio.is_empty(), "Audio-Clips erwartet");
        for c in &plan.audio {
            assert_eq!(c.path, "/orig/clip.mp4", "Export-Audio muss das Original nutzen");
        }
    }

    #[test]
    fn nested_sequence_resolves_in_render_plan() {
        use crate::core::compose::NestResolver;
        // Innere Sequenz: ein Medien-Clip „A" (mit Asset).
        let (inner, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("a", "v1", TrackKind::Video, "A", 0.0, 10.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        // Äußere Sequenz: ein Nest-Clip ab 3 s, src_in 5 s (innere Sequenzzeit).
        let (mut outer, _) = state_with(
            vec![track("ov", TrackKind::Video)],
            vec![clip("n", "ov", TrackKind::Video, "A", 3.0, 4.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        outer.clips[0].asset_id = String::new();
        outer.clips[0].nest_seq = Some("inner".into());
        outer.clips[0].src_in = 5.0;
        let inner_w = inner.settings.width;

        struct R<'a>(HashMap<String, &'a TimelineStore>);
        impl NestResolver for R<'_> {
            fn nested_timeline(&self, id: &str) -> Option<&TimelineStore> {
                self.0.get(id).copied()
            }
        }
        let resolver = R(HashMap::from([("inner".to_string(), &inner)]));

        let plan = build_render_plan(&outer, &media, &test_settings(), &resolver);
        let nest_layer = plan
            .segments
            .iter()
            .flat_map(|s| s.layers.iter())
            .find(|l| l.nest_seq.as_deref() == Some("inner"))
            .expect("Nest-Ebene im Renderplan");
        // Auflösung der Nest-Ebene = innere Sequenzgröße.
        assert_eq!(nest_layer.natural_w, inner_w);
        // Frame-Zuordnung: Segment ab t=3 → innere Sequenzzeit = src_in 5.
        assert!((nest_layer.src_in - 5.0).abs() < 1e-6, "src_in = {}", nest_layer.src_in);
        assert!((nest_layer.media_step - 1.0).abs() < 1e-6);
        // Worker-Kontext eingesammelt: innere Sequenz + ihr Blatt-Medium.
        assert!(plan.nests.contains_key("inner"), "innere Sequenz im Kontext");
        let ctx = &plan.nests["inner"];
        assert_eq!(ctx.clips.len(), 1);
        assert!(plan.nest_media.contains_key("A"), "Blatt-Medium der inneren Sequenz");
        assert_eq!(plan.nest_media["A"].path, "/a.mp4");
        // Nest-Ebene zählt als Video (keine falsche „leer"-Erkennung).
        assert!(plan.has_video_media());
    }

    #[test]
    fn multicam_resolves_active_angle_in_render_plan() {
        use crate::core::compose::NestResolver;
        use crate::core::multicam::{MulticamAngle, MulticamClip, MulticamSource, MulticamSync};
        // Asset B genuin 1280×720, damit die natürliche Größe den aktiven
        // Winkel (B) eindeutig von A (1920×1080) unterscheidet.
        let mut asset_b = video_asset("B", "/b.mp4");
        asset_b.info.video[0].width = 1280;
        asset_b.info.video[0].height = 720;
        // Quell-Sequenz mit Multicam-Metadaten: Winkel A (pos 0) und B (pos 2).
        let (mut src, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            Vec::new(),
            vec![video_asset("A", "/a.mp4"), asset_b.clone()],
        );
        src.multicam = Some(MulticamSource {
            angles: vec![
                MulticamAngle {
                    name: "Kamera 1".into(),
                    asset_id: "A".into(),
                    pos: 0.0,
                    duration: 100.0,
                    width: 1920,
                    height: 1080,
                    fps: 25.0,
                    has_audio: true,
                },
                MulticamAngle {
                    name: "Kamera 2".into(),
                    asset_id: "B".into(),
                    pos: 2.0,
                    duration: 100.0,
                    width: 1280,
                    height: 720,
                    fps: 25.0,
                    has_audio: true,
                },
            ],
            audio_angle: None,
            sync: MulticamSync::Audio,
            duration: 102.0,
        });
        // Äußere Sequenz: Multicam-Clip ab 3 s, gemeinsame Zeit src_in 5 s,
        // aktiver Winkel 1 (= B).
        let (mut outer, _) = state_with(
            vec![track("ov", TrackKind::Video), track("oa", TrackKind::Audio)],
            vec![
                clip("mc", "ov", TrackKind::Video, "", 3.0, 4.0),
                clip("mca", "oa", TrackKind::Audio, "", 3.0, 4.0),
            ],
            vec![video_asset("A", "/a.mp4"), asset_b],
        );
        for c in outer.clips.iter_mut() {
            c.src_in = 5.0;
            c.multicam = Some(MulticamClip {
                source: "src".into(),
                angle: 1,
            });
        }

        struct R<'a>(HashMap<String, &'a TimelineStore>);
        impl NestResolver for R<'_> {
            fn nested_timeline(&self, id: &str) -> Option<&TimelineStore> {
                self.0.get(id).copied()
            }
        }
        let resolver = R(HashMap::from([("src".to_string(), &src)]));

        let plan = build_render_plan(&outer, &media, &test_settings(), &resolver);
        let layer = plan
            .segments
            .iter()
            .flat_map(|s| s.layers.iter())
            .find(|l| l.clip_id == "mc")
            .expect("Multicam-Ebene im Renderplan");
        // Aktiver Winkel B → Original /b.mp4, natürliche Größe 1280×720.
        assert_eq!(layer.path, "/b.mp4", "aktiver Winkel B");
        assert_eq!(layer.natural_w, 1280);
        assert_eq!(layer.natural_h, 720);
        // Asset-Medienzeit am Segmentbeginn = gemeinsame Zeit 5 − Winkel-pos 2 = 3.
        assert!((layer.src_in - 3.0).abs() < 1e-6, "src_in = {}", layer.src_in);
        // Audio folgt dem aktiven Winkel (audio_angle = None) ⇒ /b.mp4.
        assert!(
            plan.audio.iter().any(|c| c.path == "/b.mp4"),
            "Audio des aktiven Winkels"
        );
    }

    #[test]
    fn nested_sequence_audio_flattens_into_plan() {
        use crate::core::compose::NestResolver;
        // Innere Sequenz: ein Audio-Clip 0..10 auf einer Audiospur.
        let (inner, media) = state_with(
            vec![track("a1", TrackKind::Audio)],
            vec![clip("au", "a1", TrackKind::Audio, "AUD", 0.0, 10.0)],
            vec![video_asset("AUD", "/aud.mov")],
        );
        // Äußere Sequenz: Nest-Clip auf einer Audiospur, 3..7, src_in 5.
        let (mut outer, _) = state_with(
            vec![track("oa", TrackKind::Audio)],
            vec![clip("n", "oa", TrackKind::Audio, "AUD", 3.0, 4.0)],
            vec![video_asset("AUD", "/aud.mov")],
        );
        outer.clips[0].asset_id = String::new();
        outer.clips[0].nest_seq = Some("inner".into());
        outer.clips[0].src_in = 5.0;

        struct R<'a>(HashMap<String, &'a TimelineStore>);
        impl NestResolver for R<'_> {
            fn nested_timeline(&self, id: &str) -> Option<&TimelineStore> {
                self.0.get(id).copied()
            }
        }
        let resolver = R(HashMap::from([("inner".to_string(), &inner)]));
        let plan = build_render_plan(&outer, &media, &test_settings(), &resolver);
        // Inneres Audio zeitverschoben in den äußeren Mix eingeflacht.
        assert_eq!(plan.audio.len(), 1, "inneres Audio im Mix");
        let a = &plan.audio[0];
        assert_eq!(a.path, "/aud.mov");
        assert!((a.start_in_mix - 3.0).abs() < 1e-6, "start {}", a.start_in_mix);
        assert!((a.duration - 4.0).abs() < 1e-6, "dur {}", a.duration);
        assert!((a.src_in - 5.0).abs() < 1e-6, "src_in {}", a.src_in);
        assert!((a.gain_l - 1.0).abs() < 1e-3 && (a.gain_r - 1.0).abs() < 1e-3);
    }

    #[test]
    fn plan_splits_overlaps_into_layer_stacks() {
        // V2 (oben): Clip 2..4 — V1 (unten): Clip 0..10. 2..4 trägt BEIDE
        // Layer (A unten, B oben), davor/danach nur A; src_in läuft weiter.
        let (tl, media) = state_with(
            vec![track("v2", TrackKind::Video), track("v1", TrackKind::Video)],
            vec![
                clip("a", "v1", TrackKind::Video, "A", 0.0, 10.0),
                clip("b", "v2", TrackKind::Video, "B", 2.0, 2.0),
            ],
            vec![video_asset("A", "/a.mp4"), video_asset("B", "/b.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert_eq!(plan.total_frames, 250);
        assert_eq!(plan.segments.len(), 3);
        assert_eq!(plan.segments[0].frames, 50);
        assert_eq!(plan.segments[1].frames, 50);
        assert_eq!(plan.segments[2].frames, 150);
        assert_eq!(plan.segments[0].layers.len(), 1);
        // Überlappung: unten A, oben B (Zeichenreihenfolge).
        let mid = &plan.segments[1].layers;
        assert_eq!(mid.len(), 2, "Überlappung muss beide Layer tragen");
        assert_eq!(mid[0].path, "/a.mp4");
        assert_eq!(mid[1].path, "/b.mp4");
        let tail = &plan.segments[2].layers;
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].path, "/a.mp4");
        assert!(
            (tail[0].src_in - 4.0).abs() < 1e-6,
            "src_in muss weiterlaufen: {}",
            tail[0].src_in
        );
    }

    #[test]
    fn plan_carries_effects_and_disables_fast_path() {
        let mut c = clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0);
        c.effects
            .push(crate::core::effects::EffectInstance::new(
                crate::core::effects::EffectKind::GaussianBlur,
            ));
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![c],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        let layer = &plan.segments[0].layers[0];
        assert_eq!(layer.effects.len(), 1);
        assert!(
            !layer.is_identity(),
            "aktiver Effekt erzwingt den Compositing-Pfad"
        );
        // Deaktivierter Effekt ⇒ Schnellpfad bleibt erlaubt.
        let mut layer2 = layer.clone();
        layer2.effects[0].enabled = false;
        assert!(layer2.is_identity());
    }

    #[test]
    fn plan_audio_carries_audio_effects_only() {
        let mut c = clip("a", "a1", TrackKind::Audio, "A", 0.0, 4.0);
        c.effects
            .push(crate::core::effects::EffectInstance::new(
                crate::core::effects::EffectKind::Reverb,
            ));
        c.effects
            .push(crate::core::effects::EffectInstance::new(
                crate::core::effects::EffectKind::GaussianBlur,
            ));
        let (tl, media) = state_with(
            vec![track("a1", TrackKind::Audio)],
            vec![c],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert_eq!(plan.audio.len(), 1);
        assert_eq!(plan.audio[0].effects.len(), 1, "nur Audio-Effekte");
        assert_eq!(
            plan.audio[0].effects[0].kind,
            crate::core::effects::EffectKind::Reverb
        );
    }

    #[test]
    fn plan_routes_track_bus_fx_and_automation_to_processed() {
        use crate::core::effects::{EffectInstance, EffectKind};
        // Spur mit Bus-Effekt.
        let mut t1 = track("a1", TrackKind::Audio);
        t1.gain_db = -6.0;
        t1.effects.push(EffectInstance::new(EffectKind::Equalizer));
        let mut c1 = clip("c1", "a1", TrackKind::Audio, "A", 0.0, 4.0);
        c1.gain_db = 3.0;
        // Spur mit Automation (ohne FX).
        let mut t2 = track("a2", TrackKind::Audio);
        t2.volume_auto.upsert_key(0.0, 0.0);
        t2.volume_auto.upsert_key(2.0, 6.0);
        let c2 = clip("c2", "a2", TrackKind::Audio, "A", 0.0, 4.0);
        // Einfache Spur.
        let t3 = track("a3", TrackKind::Audio);
        let c3 = clip("c3", "a3", TrackKind::Audio, "A", 0.0, 4.0);
        let (tl, media) = state_with(
            vec![t1, t2, t3],
            vec![c1, c2, c3],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        // Eine einfache Spur → Schnellpfad; zwei verarbeitete → Bus-Pfad.
        assert_eq!(plan.audio.len(), 1, "nur die einfache Spur im Schnellpfad");
        assert_eq!(plan.audio_tracks.len(), 2);
        // Verarbeitete Clip-Gains tragen NUR den Clip-Anteil (kein Master/
        // Spur/Pan — das folgt in der Bus-Verarbeitung).
        let bus = plan
            .audio_tracks
            .iter()
            .find(|t| !t.effects.is_empty())
            .expect("Bus-FX-Spur");
        let expect = db_to_linear(3.0);
        assert!((bus.clips[0].gain_l - expect).abs() < 1e-6, "nur Clip-Gain");
        assert!((bus.clips[0].gain_r - expect).abs() < 1e-6);
        assert_eq!(bus.gain_db, -6.0);
    }

    #[test]
    fn plan_ducking_forces_all_tracks_to_bus_path() {
        use crate::core::effects::{EffectInstance, EffectKind};
        // Musikspur mit Ducking-Effekt; Key-Spur OHNE FX, aber mit Spur-Gain.
        let mut music = track("a1", TrackKind::Audio);
        music.effects.push(EffectInstance::new(EffectKind::Ducking));
        let mut key = track("a2", TrackKind::Audio);
        key.gain_db = -6.0; // würde im Schnellpfad in die Clip-Gains eingebacken
        let (tl, media) = state_with(
            vec![music, key],
            vec![
                clip("c1", "a1", TrackKind::Audio, "A", 0.0, 4.0),
                clip("c2", "a2", TrackKind::Audio, "A", 0.0, 4.0),
            ],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        // Sobald geduckt wird, MUSS jede Spur über die Bus-Verarbeitung laufen
        // (kein Schnellpfad), damit der Sidechain-Key auf Clip-Gain-Ebene liegt
        // — formelgleich zum Player (Key = Summe der rohen anderen Spuren).
        assert!(
            plan.audio.is_empty(),
            "Ducking ⇒ kein Schnellpfad-Master (Key bliebe sonst auf Fader-Ebene)"
        );
        assert_eq!(plan.audio_tracks.len(), 2, "beide Spuren als Bus-Spuren");
        // Die Key-Spur trägt ihren Spur-Gain als Bus-Wert, NICHT eingebacken.
        let key_track = plan
            .audio_tracks
            .iter()
            .find(|t| t.gain_db != 0.0)
            .expect("Key-Spur");
        assert_eq!(key_track.gain_db, -6.0);
        let g = db_to_linear(0.0);
        assert!(
            (key_track.clips[0].gain_l - g).abs() < 1e-6,
            "Key-Clip-Gain roh (Spur-Fader NICHT eingebacken)"
        );
    }

    #[test]
    fn processed_track_gain_matches_player_semantics() {
        // AudioTrackPlan (Export) und TimelineTrack (Player) werten Spur-Gain/
        // Pan inkl. Automation identisch aus — gemeinsame Mathematik, damit
        // Wiedergabe und Export gleich klingen.
        let mut t = track("a1", TrackKind::Audio);
        t.gain_db = -4.0;
        t.pan = 0.2;
        t.volume_auto.upsert_key(0.0, 0.0);
        t.volume_auto.upsert_key(4.0, 8.0);
        t.pan_auto.upsert_key(0.0, 0.0);
        t.pan_auto.upsert_key(4.0, 0.5);
        let plan = AudioTrackPlan {
            name: "A1".into(),
            clips: vec![],
            effects: vec![],
            volume_auto: t.volume_auto.clone(),
            pan_auto: t.pan_auto.clone(),
            gain_db: t.gain_db,
            pan: t.pan,
            master_db: 0.0,
            seq_start: 3.0,
        };
        for mix_t in [0.0, 1.0, 2.5, 4.0] {
            let seq_t = 3.0 + mix_t;
            assert!((plan.gain_db_at(mix_t) - t.gain_db_at(seq_t)).abs() < 1e-9);
            assert!((plan.pan_at(mix_t) - t.pan_at(seq_t)).abs() < 1e-9);
        }
    }

    #[test]
    fn plan_extends_layers_across_transition_and_disables_fast_path() {
        // A: 0–4 (src_in 1 von 10 s), B: 4–8 (src_in 2 von 10 s) auf V1;
        // Überblendung zentriert 2 s ⇒ Fenster 3..5: dort laufen BEIDE Layer.
        let (mut tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![
                {
                    let mut c = clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0);
                    c.src_in = 1.0;
                    c.src_duration = 10.0;
                    c
                },
                {
                    let mut c = clip("b", "v1", TrackKind::Video, "B", 4.0, 4.0);
                    c.src_in = 2.0;
                    c.src_duration = 10.0;
                    c
                },
            ],
            vec![video_asset("A", "/a.mp4"), video_asset("B", "/b.mp4")],
        );
        tl.add_transition(
            crate::core::transitions::TransitionKind::CrossDissolve,
            "a",
            crate::core::timeline::TrimEdge::End,
            2.0,
        )
        .unwrap();
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        // Segmente: A allein (0..3), A+B (3..5), B allein (5..8).
        assert_eq!(plan.segments.len(), 3, "{:?}", plan.segments);
        assert_eq!(plan.segments[0].frames, 75);
        assert_eq!(plan.segments[1].frames, 50);
        assert_eq!(plan.segments[2].frames, 75);
        // Vor dem Fenster: ein Layer ohne Übergang ⇒ Schnellpfad bleibt.
        assert_eq!(plan.segments[0].layers.len(), 1);
        assert!(plan.segments[0].layers[0].transitions.is_empty());
        assert!(plan.segments[0].layers[0].is_identity());
        // Im Fenster: A (Out) unten, B (In) oben — Medienzeit über die Kante.
        let mid = &plan.segments[1].layers;
        assert_eq!(mid.len(), 2);
        assert_eq!(mid[0].path, "/a.mp4");
        assert_eq!(mid[1].path, "/b.mp4");
        assert_eq!(mid[0].transitions[0].role, TransitionRole::Out);
        assert_eq!(mid[1].transitions[0].role, TransitionRole::In);
        assert!((mid[0].transitions[0].t0 - 3.0).abs() < 1e-9);
        assert!((mid[0].transitions[0].t1 - 5.0).abs() < 1e-9);
        // B beginnt im Fenster eine Sekunde VOR seinem In-Punkt (Kopf-Handle).
        assert!((mid[1].src_in - 1.0).abs() < 1e-6, "src_in = {}", mid[1].src_in);
        assert!(!mid[0].is_identity() && !mid[1].is_identity());
        // Übergangs-Auswertung: Mitte des Fensters ⇒ B halbtransparent.
        let fx = eval_plan_transitions(&mid[1].transitions, 4.0);
        assert!((fx.opacity - 0.5).abs() < 1e-9);
        // Nach dem Fenster: B allein, nahtlose Medienzeit.
        assert_eq!(plan.segments[2].layers.len(), 1);
        assert!((plan.segments[2].layers[0].src_in - 3.0).abs() < 1e-6);
        assert!(plan.segments[2].layers[0].transitions.is_empty());
    }

    #[test]
    fn plan_dip_adds_solid_layer() {
        let (mut tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![
                {
                    let mut c = clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0);
                    c.src_in = 1.0;
                    c.src_duration = 10.0;
                    c
                },
                {
                    let mut c = clip("b", "v1", TrackKind::Video, "B", 4.0, 4.0);
                    c.src_in = 2.0;
                    c.src_duration = 10.0;
                    c
                },
            ],
            vec![video_asset("A", "/a.mp4"), video_asset("B", "/b.mp4")],
        );
        tl.add_transition(
            crate::core::transitions::TransitionKind::DipToWhite,
            "a",
            crate::core::timeline::TrimEdge::End,
            2.0,
        )
        .unwrap();
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        let mid = &plan.segments[1].layers;
        assert_eq!(mid.len(), 3, "A + B + Farbfläche");
        assert_eq!(mid[2].solid, Some([255, 255, 255]));
        assert_eq!(mid[2].transitions[0].role, TransitionRole::Dip);
        // Farbfläche voll deckend exakt am Schnitt (p = 0,5).
        let fx = eval_plan_transitions(&mid[2].transitions, 4.0);
        assert!((fx.opacity - 1.0).abs() < 1e-9);
    }

    #[test]
    fn plan_audio_carries_crossfades() {
        let (mut tl, media) = state_with(
            vec![track("a1", TrackKind::Audio)],
            vec![
                {
                    let mut c = clip("a", "a1", TrackKind::Audio, "A", 0.0, 4.0);
                    c.src_in = 1.0;
                    c.src_duration = 10.0;
                    c
                },
                {
                    let mut c = clip("b", "a1", TrackKind::Audio, "B", 4.0, 4.0);
                    c.src_in = 2.0;
                    c.src_duration = 10.0;
                    c
                },
            ],
            vec![video_asset("A", "/a.mp4"), video_asset("B", "/b.mp4")],
        );
        tl.add_transition(
            crate::core::transitions::TransitionKind::ConstantPower,
            "a",
            crate::core::timeline::TrimEdge::End,
            2.0,
        )
        .unwrap();
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert_eq!(plan.audio.len(), 2);
        let a = &plan.audio[0];
        let b = &plan.audio[1];
        // A spielt bis 5 s (1 s über die Kante), B ab 3 s (1 s davor).
        assert!((a.duration - 5.0).abs() < 1e-9);
        assert!((b.start_in_mix - 3.0).abs() < 1e-9);
        assert!((b.src_in - 1.0).abs() < 1e-9, "Kopf-Handle: {}", b.src_in);
        assert_eq!(a.fades, vec![PlanAudioFade { t0: 3.0, t1: 5.0, fade_in: false, equal_power: true }]);
        assert_eq!(b.fades, vec![PlanAudioFade { t0: 3.0, t1: 5.0, fade_in: true, equal_power: true }]);
        // Hüllkurven: konstante Leistung in der Fenstermitte.
        let g_out = a.fades[0].gain_at(4.0);
        let g_in = b.fades[0].gain_at(4.0);
        assert!((g_out * g_out + g_in * g_in - 1.0).abs() < 1e-9);
    }

    #[test]
    fn plan_inserts_black_for_gaps_and_respects_mute() {
        let mut t = track("v1", TrackKind::Video);
        t.muted = false;
        let (mut tl, media) = state_with(
            vec![t],
            vec![clip("a", "v1", TrackKind::Video, "A", 2.0, 2.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert_eq!(plan.segments.len(), 2); // Schwarz 0..2, dann Clip
        assert!(plan.segments[0].layers.is_empty());
        assert_eq!(plan.segments[0].frames, 50);

        tl.tracks[0].muted = true;
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert!(!plan.has_video_media(), "gemutete Spur darf nicht rendern");
    }

    /// Timeline mit einem Video-Clip und einer Untertitel-Spur (2 Segmente).
    fn state_with_subtitles() -> (TimelineStore, MediaStore) {
        let (mut tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("a", "v1", TrackKind::Video, "A", 0.0, 8.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        tl.import_subtitle_cues(&[
            crate::core::subtitle::SrtCue { start: 1.0, end: 3.0, text: "Erster Satz".into() },
            crate::core::subtitle::SrtCue { start: 4.0, end: 6.0, text: "Zweiter Satz".into() },
        ]);
        (tl, media)
    }

    #[test]
    fn plan_burns_subtitles_as_top_title_layers() {
        let (tl, media) = state_with_subtitles();
        let mut settings = test_settings();

        // Ohne Einbrennen: Untertitel tauchen in keinem Segment auf.
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert!(plan
            .segments
            .iter()
            .all(|s| s.layers.iter().all(|l| l.title.is_none())));
        assert!(plan.subtitle_tracks.is_empty());

        settings.subtitles = SubtitleMode::BurnIn;
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        // Segment bei t=2 (Frame 50): Video unten, Untertitel-Titel oben.
        let seg_at = |f: u64| -> &VideoSegment {
            let mut cursor = 0u64;
            for s in &plan.segments {
                if f < cursor + s.frames {
                    return s;
                }
                cursor += s.frames;
            }
            plan.segments.last().unwrap()
        };
        let seg = seg_at(50);
        assert_eq!(seg.layers.len(), 2);
        assert!(seg.layers[0].title.is_none(), "Video unten");
        let spec = seg.layers[1].title.as_ref().expect("Untertitel oben");
        assert_eq!(spec.text, "Erster Satz");
        // Zwischen den Segmenten (t=3,5): nur das Video.
        let seg = seg_at(87);
        assert_eq!(seg.layers.len(), 1);
        // Einbrennen erzwingt den Compositing-Pfad (kein Schnellpfad).
        assert!(!seg_at(50).layers[1].is_identity());

        // Ausgeblendete Spur (Auge zu) wird nicht eingebrannt.
        let mut tl = tl;
        let sub_track = tl.subtitle_tracks()[0].id.clone();
        if let Some(t) = tl.tracks.iter_mut().find(|t| t.id == sub_track) {
            t.muted = true;
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert!(plan
            .segments
            .iter()
            .all(|s| s.layers.iter().all(|l| l.title.is_none())));
    }

    #[test]
    fn plan_collects_subtitle_tracks_for_sidecar_clipped_to_range() {
        let (mut tl, media) = state_with_subtitles();
        tl.in_point = Some(2.0);
        tl.out_point = Some(5.0);
        let mut settings = test_settings();
        settings.subtitles = SubtitleMode::Sidecar;
        settings.use_in_out = true;
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.subtitle_tracks.len(), 1);
        let track = &plan.subtitle_tracks[0];
        assert_eq!(track.name, "U1");
        // Beide Cues berühren den Bereich; Zeiten relativ zum Exportbeginn
        // und auf den Bereich beschnitten.
        assert_eq!(track.cues.len(), 2);
        assert!((track.cues[0].start - 0.0).abs() < 1e-9);
        assert!((track.cues[0].end - 1.0).abs() < 1e-9);
        assert!((track.cues[1].start - 2.0).abs() < 1e-9);
        assert!((track.cues[1].end - 3.0).abs() < 1e-9);
        assert_eq!(track.cues[0].text, "Erster Satz");
        // Sidecar-Pfade: einspurig <ziel>.srt, mehrspurig mit Spurname.
        assert_eq!(
            sidecar_srt_path("/tmp/film.mp4", "U1", true),
            std::path::PathBuf::from("/tmp/film.srt")
        );
        assert_eq!(
            sidecar_srt_path("/tmp/film.mp4", "U2", false),
            std::path::PathBuf::from("/tmp/film.U2.srt")
        );
    }

    #[test]
    fn validate_checks_subtitle_modes() {
        let (tl, media) = state_with_subtitles();
        // Einbetten in einen Container ohne Untertitel-Streams → Fehler.
        let mut settings = test_settings();
        settings.container = container("wav");
        settings.video = None;
        settings.audio = Some(default_audio("pcm16", None));
        settings.output = "/tmp/out.wav".into();
        settings.subtitles = SubtitleMode::Embed;
        let issues = validate(&tl, &media, Some(true), None, &settings, &NoNests);
        assert!(
            issues
                .iter()
                .any(|i| i.severity == Severity::Error && i.message.contains("unterstützt keine")),
            "{issues:?}"
        );
        // Einbrennen ohne Video-Export → Fehler.
        let mut settings = test_settings();
        settings.video = None;
        settings.subtitles = SubtitleMode::BurnIn;
        let issues = validate(&tl, &media, Some(true), None, &settings, &NoNests);
        assert!(issues
            .iter()
            .any(|i| i.severity == Severity::Error && i.message.contains("erfordert einen Video-Export")));
        // Untertitel-Option ohne sichtbare Untertitel → Warnung.
        let (tl_empty, media_empty) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        let mut settings = test_settings();
        settings.subtitles = SubtitleMode::Sidecar;
        let issues = validate(&tl_empty, &media_empty, Some(true), None, &settings, &NoNests);
        assert!(issues
            .iter()
            .any(|i| i.severity == Severity::Warning && i.message.contains("Keine sichtbaren Untertitel")));
        // MP4 + Einbetten mit vorhandenen Untertiteln: kein Untertitel-Fehler.
        let mut settings = test_settings();
        settings.subtitles = SubtitleMode::Embed;
        let issues = validate(&tl, &media, Some(true), None, &settings, &NoNests);
        assert!(!issues
            .iter()
            .any(|i| i.severity == Severity::Error && i.message.contains("Untertitel")));
    }

    #[test]
    fn plan_audio_clips_carry_gains_and_range() {
        let mut at = track("a1", TrackKind::Audio);
        at.gain_db = -6.0;
        at.pan = -1.0; // hart links
        let mut c = clip("a", "a1", TrackKind::Audio, "A", 1.0, 4.0);
        c.gain_db = -6.0;
        let (mut tl, media) = state_with(vec![at], vec![c], vec![video_asset("A", "/a.mp4")]);
        tl.master_gain_db = -6.0;
        tl.in_point = Some(2.0);
        tl.out_point = Some(4.0);
        let mut settings = test_settings();
        settings.use_in_out = true;
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.audio.len(), 1);
        let a = &plan.audio[0];
        assert!((a.start_in_mix - 0.0).abs() < 1e-9);
        assert!((a.duration - 2.0).abs() < 1e-9);
        assert!((a.src_in - 1.0).abs() < 1e-9);
        // −18 dB gesamt ≈ 0.1259; rechts durch Pan stumm.
        assert!((a.gain_l - 0.1259).abs() < 0.001, "gain_l = {}", a.gain_l);
        assert!(a.gain_r.abs() < 1e-6);
    }

    #[test]
    fn validate_blocks_empty_timeline_and_source_overwrite() {
        let (tl, media) = state_with(vec![track("v1", TrackKind::Video)], vec![], vec![]);
        let issues = validate(&tl, &media, Some(true), None, &test_settings(), &NoNests);
        assert!(issues.iter().any(|i| i.severity == Severity::Error));

        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("a", "v1", TrackKind::Video, "A", 0.0, 5.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        let mut settings = test_settings();
        settings.output = "/a.mp4".into();
        let issues = validate(&tl, &media, Some(true), None, &settings, &NoNests);
        assert!(
            issues.iter().any(|i| i.severity == Severity::Error && i.message.contains("Quelldatei")),
            "{issues:?}"
        );
    }

    #[test]
    fn validate_flags_missing_encoder_and_odd_resolution() {
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("a", "v1", TrackKind::Video, "A", 0.0, 5.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        let mut settings = test_settings();
        settings.video.as_mut().unwrap().width = 1281;
        let encoders: HashSet<String> = ["aac".to_string()].into();
        let issues = validate(&tl, &media, Some(true), Some(&encoders), &settings, &NoNests);
        assert!(issues.iter().any(|i| i.message.contains("gerade")));
        assert!(issues.iter().any(|i| i.message.contains("libx264")));
    }

    #[test]
    fn speed_setpts_and_atempo_chains() {
        assert_eq!(speed_setpts_filter(1.0), "");
        assert_eq!(speed_setpts_filter(0.0), "");
        assert_eq!(speed_setpts_filter(2.0), "setpts=(PTS-STARTPTS)/2.000000,");
        assert!(atempo_chain(1.0).is_none());
        assert_eq!(atempo_chain(0.5).unwrap(), "atempo=0.500000");
        assert_eq!(atempo_chain(2.0).unwrap(), "atempo=2.000000");
        // Außerhalb [0,5..2,0]: kaskadiert. 4× ⇒ 2 × 2; 0,1 ⇒ 0,5 × 0,5 × 0,4.
        assert_eq!(atempo_chain(4.0).unwrap(), "atempo=2.0,atempo=2.000000");
        let chain = atempo_chain(0.1).unwrap();
        let factors: f64 = chain
            .split(',')
            .map(|p| p.trim_start_matches("atempo=").parse::<f64>().unwrap())
            .product();
        assert!((factors - 0.1).abs() < 1e-6, "{chain}");
    }

    #[test]
    fn plan_maps_constant_speed_frame_accurate() {
        // 37 % auf einem 4-s-Clip ⇒ Dauer 4 s, Medienspanne 1,48 s.
        let mut c = clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0);
        c.speed = AnimatedParam::fixed(0.37);
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![c],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert_eq!(plan.total_frames, 100); // 4 s × 25 fps
        let layer = &plan.segments[0].layers[0];
        assert!((layer.media_step - 0.37).abs() < 1e-12);
        // Krummer Faktor erzwingt den Compositing-Pfad (setpts ≠ Identität)?
        // Nein — Vorwärts-Speed bleibt schnellpfad-fähig.
        assert!(layer.is_identity());
        // Medienzeit von Frame f = src_in + f/fps · media_step.
        let sum: u64 = plan.segments.iter().map(|s| s.frames).sum();
        assert_eq!(sum, plan.total_frames);
    }

    #[test]
    fn plan_time_remap_emits_per_frame_segments_with_integrated_media_time() {
        // Speed-Rampe 1× → 3× linear über 4 s (clip-lokale Zeit).
        // Medienzeit = ∫₀ᵗᶜ (1 + 0,5·t) dt = tc + 0,25·tc² (src_in 0).
        let mut c = clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0);
        let mut sp = AnimatedParam::fixed(1.0);
        sp.upsert_key(0.0, 1.0);
        sp.upsert_key(4.0, 3.0);
        c.speed = sp;
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![c],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert_eq!(plan.total_frames, 100); // 4 s × 25 fps
        let sum: u64 = plan.segments.iter().map(|s| s.frames).sum();
        assert_eq!(sum, 100);
        // Variable Rate ⇒ keine Koaleszenz: jedes Segment ist ein Frame.
        assert!(plan.segments.iter().all(|s| s.frames == 1), "Pro-Frame-Segmente");
        // src_in jedes Frames folgt dem Integral der Speed-Kurve.
        let fps = 25.0;
        let media_at = |tc: f64| tc + 0.25 * tc * tc;
        let mut f = 0u64;
        for seg in &plan.segments {
            let seq_t = f as f64 / fps;
            let expected = media_at(seq_t);
            assert!(
                (seg.layers[0].src_in - expected).abs() < 1e-6,
                "f={f}: {} vs {expected}",
                seg.layers[0].src_in
            );
            f += seg.frames;
        }
    }

    #[test]
    fn plan_time_remap_mutes_audio() {
        // Time-Remap-Audio ist stumm (Parität Player ↔ Export): eine konstante
        // atempo-Kette kann die variable Kurve nicht abbilden.
        let mut c = clip("a", "a1", TrackKind::Audio, "A", 0.0, 4.0);
        let mut sp = AnimatedParam::fixed(1.0);
        sp.upsert_key(0.0, 0.5);
        sp.upsert_key(4.0, 2.0);
        c.speed = sp;
        let (tl, media) = state_with(
            vec![track("a1", TrackKind::Audio)],
            vec![c],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert!(plan.audio.is_empty(), "Time-Remap-Audio stumm");
    }

    #[test]
    fn plan_reverse_disables_fast_path_and_audio() {
        let mut c = clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0);
        c.reverse = true;
        let mut a = clip("b", "a1", TrackKind::Audio, "A", 0.0, 4.0);
        a.reverse = true;
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video), track("a1", TrackKind::Audio)],
            vec![c, a],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        let layer = &plan.segments[0].layers[0];
        assert!((layer.media_step + 1.0).abs() < 1e-12, "rückwärts: −1");
        assert!(!layer.is_identity(), "Rückwärts erzwingt den Compositing-Pfad");
        assert!(plan.audio.is_empty(), "Rückwärts-Audio ist stumm");
    }

    #[test]
    fn plan_freeze_holds_media_time() {
        let mut c = clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0);
        c.src_in = 3.0;
        c.freeze = true;
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![c],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        // Trotz Mehrfach-Segmenten bleibt die Medienzeit am In-Punkt.
        for seg in &plan.segments {
            for l in &seg.layers {
                assert_eq!(l.media_step, 0.0);
                assert!((l.src_in - 3.0).abs() < 1e-9, "Standbild-Medienzeit: {}", l.src_in);
            }
        }
    }

    #[test]
    fn plan_audio_doubles_source_span_for_speed() {
        // 2× Tempo: 2-s-Clip zieht 4 s Quelle, pitch-korrigiert.
        let mut c = clip("a", "a1", TrackKind::Audio, "A", 0.0, 2.0);
        c.speed = AnimatedParam::fixed(2.0);
        let (tl, media) = state_with(
            vec![track("a1", TrackKind::Audio)],
            vec![c],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert_eq!(plan.audio.len(), 1);
        assert!((plan.audio[0].duration - 2.0).abs() < 1e-9);
        assert!((plan.audio[0].speed - 2.0).abs() < 1e-9);
    }

    #[test]
    fn fps_arg_maps_ntsc_rates() {
        assert_eq!(fps_arg(23.976), "24000/1001");
        assert_eq!(fps_arg(29.97), "30000/1001");
        assert_eq!(fps_arg(59.94), "60000/1001");
        assert_eq!(fps_arg(25.0), "25");
        assert_eq!(fps_arg(60.0), "60");
        // Auch die exakten Brüche aus FRAMERATES landen auf dem Bruch-Argument.
        assert_eq!(fps_arg(30000.0 / 1001.0), "30000/1001");
        assert_eq!(fps_arg(24000.0 / 1001.0), "24000/1001");
    }

    #[test]
    fn plan_has_no_ntsc_drift_over_long_durations() {
        // 10-Stunden-Sequenz bei 29,97: exakte Rate ⇒ 1.078.921 Frames
        // (36000 s × 30000/1001). Die gerundete 29.97 ergäbe 1.078.920 —
        // ein Frame Drift, der über die Tonspur sichtbar würde.
        let mut c = clip("a", "v1", TrackKind::Video, "A", 0.0, 36000.0);
        c.src_duration = 36000.0;
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![c],
            vec![video_asset("A", "/a.mp4")],
        );
        let mut settings = test_settings();
        let ntsc = FRAMERATES.iter().find(|(l, _)| *l == "29,97").unwrap().1;
        settings.video.as_mut().unwrap().fps = ntsc;
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.total_frames, 1_078_921);
        assert_eq!((36000.0_f64 * 29.97).round() as u64, 1_078_920, "gerundet drifted");
        // Segmentgrenzen decken die Gesamtdauer lückenlos ab.
        let sum: u64 = plan.segments.iter().map(|s| s.frames).sum();
        assert_eq!(sum, plan.total_frames);
    }

    #[test]
    fn estimate_size_bitrate_and_pcm() {
        let mut settings = test_settings();
        settings.video.as_mut().unwrap().quality = VideoQuality::Bitrate(8000);
        settings.audio.as_mut().unwrap().bitrate_kbps = 192;
        // (8000 + 192) kbit/s × 10 s / 8 = 10,24 MB
        assert_eq!(estimate_size(&settings, 10.0), Some(10_240_000));
        settings.video = None;
        settings.audio = Some(default_audio("pcm16", None));
        // 48000 × 2 × 16 bit × 10 s / 8 = 1.920.000 B
        assert_eq!(estimate_size(&settings, 10.0), Some(1_920_000));
        settings.audio = Some(default_audio("flac", None));
        assert_eq!(estimate_size(&settings, 10.0), None);
    }

    #[test]
    fn encoder_args_cover_codec_specifics() {
        let v = VideoSettings {
            quality: VideoQuality::Crf(30),
            ..default_video("vp9", 1920, 1080, 25.0)
        };
        let args = video_codec_args(&v, container("webm"), OutputColor::Bt709);
        let joined = args.join(" ");
        assert!(joined.contains("-crf 30"));
        assert!(joined.contains("-b:v 0"), "VP9-CRF braucht -b:v 0: {joined}");

        let mut v = default_video("prores", 1920, 1080, 25.0);
        v.profile = 4;
        let args = video_codec_args(&v, container("mov"), OutputColor::Bt709);
        let joined = args.join(" ");
        assert!(joined.contains("-profile:v 4"));
        assert!(joined.contains("yuv444p10le"));

        let v = default_video("hevc", 1920, 1080, 25.0);
        let joined = video_codec_args(&v, container("mp4"), OutputColor::Bt709).join(" ");
        assert!(joined.contains("-tag:v hvc1"));
        // 8-Bit-Standard ⇒ yuv420p, kein main10.
        assert!(joined.contains("yuv420p") && !joined.contains("yuv420p10le"));

        // 10-Bit-Schalter ⇒ HEVC main10 + yuv420p10le.
        let mut v10 = default_video("hevc", 1920, 1080, 25.0);
        v10.tenbit = true;
        let joined = video_codec_args(&v10, container("mp4"), OutputColor::Bt709).join(" ");
        assert!(joined.contains("-profile:v main10"), "HEVC main10: {joined}");
        assert!(joined.contains("yuv420p10le"), "10-Bit-Pixelformat: {joined}");
        assert!(pipe_hi_bit(&v10), "10-Bit-Schalter ⇒ 16-Bit-Pipe");

        // H.264 High 10 + AV1 10-Bit.
        let mut h10 = default_video("h264", 1920, 1080, 25.0);
        h10.tenbit = true;
        assert!(video_codec_args(&h10, container("mp4"), OutputColor::Bt709).join(" ").contains("-profile:v high10"));
        let mut av10 = default_video("av1", 1920, 1080, 25.0);
        av10.tenbit = true;
        assert!(video_codec_args(&av10, container("mp4"), OutputColor::Bt709).join(" ").contains("yuv420p10le"));
    }

    #[test]
    fn mxf_broadcast_catalog_wired() {
        // MXF-Container ist vorhanden (OP1a-Muxer) und bietet die
        // Broadcast-Codecs an; XDCAM HD422 ist als Codec registriert.
        let mxf = container("mxf");
        assert_eq!(mxf.muxer, "mxf");
        assert_eq!(mxf.ext, "mxf");
        assert!(mxf.video);
        assert!(mxf.video_codecs.contains(&"xdcamhd422"));
        assert!(mxf.video_codecs.contains(&"dnxhr"));
        assert!(mxf.video_codecs.contains(&"prores"));
        // MXF trägt PCM, keine verlustbehaftete Tonspur.
        assert!(mxf.audio_codecs.contains(&"pcm24"));
        assert!(!mxf.audio_codecs.contains(&"aac"));
        // XDCAM-Codec aufgelöst.
        assert_eq!(video_codec("xdcamhd422").encoder, "mpeg2video");
        // Nur XDCAM HD422 kann in dieser Pipeline echtes Interlaced.
        assert!(codec_supports_interlace("xdcamhd422"));
        assert!(!codec_supports_interlace("dnxhr"));
        assert!(!codec_supports_interlace("prores"));
        assert!(!codec_supports_interlace("h264"));
        // Es gibt Broadcast-Presets, die nach MXF schreiben.
        let mxf_presets = PRESETS
            .iter()
            .filter(|p| (p.build)((1920, 1080), 25.0).container.id == "mxf")
            .count();
        assert!(mxf_presets >= 3, "mind. 3 MXF-Presets erwartet, fand {mxf_presets}");
    }

    #[test]
    fn xdcam_hd422_args_are_broadcast_grade() {
        // 1080i25 (interlaced, oberes Feld zuerst).
        let mut v = default_video("xdcamhd422", 1920, 1080, 25.0);
        v.scan = ScanMode::InterlacedTff;
        let joined = video_codec_args(&v, container("mxf"), OutputColor::Bt709).join(" ");
        // MPEG-2 4:2:2 @ 50 Mbit/s CBR.
        assert!(joined.contains("-c:v mpeg2video"), "{joined}");
        assert!(joined.contains("-pix_fmt yuv422p"), "{joined}");
        assert!(joined.contains("-b:v 50M") && joined.contains("-minrate 50M") && joined.contains("-maxrate 50M"));
        // Kein explizites -profile:v (würde den MXF-Muxer brechen).
        assert!(!joined.contains("-profile:v"), "XDCAM darf kein -profile:v setzen: {joined}");
        // non_linear_quant verlangt qmax ≤ 28.
        assert!(joined.contains("-non_linear_quant 1") && joined.contains("-qmax 28"), "{joined}");
        // Interlaced: Feldkodierung + oberes Feld zuerst, auch im setparams-Filter.
        assert!(joined.contains("-flags +ilme+ildct") && joined.contains("-top 1"), "{joined}");
        assert!(joined.contains("field_mode=tff"), "{joined}");
        // Ehrliche Color-Tags inkl. setparams (alle drei überleben den Mux).
        assert!(joined.contains("setparams=range=tv:color_primaries=bt709:color_trc=bt709:colorspace=bt709"), "{joined}");
        assert!(joined.contains("-color_primaries bt709") && joined.contains("-color_trc bt709"));

        // Progressiv (1080p25): keine Feld-Flags.
        let vp = default_video("xdcamhd422", 1920, 1080, 25.0);
        let prog = video_codec_args(&vp, container("mxf"), OutputColor::Bt709).join(" ");
        assert!(!prog.contains("-flags +ilme+ildct") && !prog.contains("-top"), "{prog}");
        assert!(prog.contains("field_mode=prog"), "{prog}");
    }

    #[test]
    fn setparams_makes_all_color_tags_survive() {
        // Der setparams-Schritt schreibt Primaries/Transfer auf die Frames,
        // sonst meldet ffprobe „unknown" (scale taggt nur die Matrix).
        let v = default_video("prores", 1920, 1080, 25.0);
        let hdr = video_codec_args(&v, container("mxf"), OutputColor::Bt2020Pq).join(" ");
        assert!(hdr.contains("setparams="), "setparams fehlt: {hdr}");
        assert!(hdr.contains("color_primaries=bt2020"), "{hdr}");
        assert!(hdr.contains("color_trc=smpte2084"), "PQ-Transfer durchgereicht: {hdr}");
        // Interlaced wird auf Codecs geklemmt, die es WIRKLICH können: der
        // ffmpeg-dnxhd-Encoder bricht bei interlaced DNxHR ab, prores_ks bleibt
        // trotzdem progressiv. Beide ⇒ erzwungen progressiv (keine
        // Feldkodierung, keine irreführende Interlaced-Markierung).
        assert!(!codec_supports_interlace("prores"));
        assert!(!codec_supports_interlace("dnxhr"));
        for codec in ["prores", "dnxhr"] {
            let mut vi = default_video(codec, 1920, 1080, 25.0);
            vi.scan = ScanMode::InterlacedBff;
            let il = video_codec_args(&vi, container("mxf"), OutputColor::Bt709).join(" ");
            assert!(il.contains("field_mode=prog"), "{codec}: progressiv erzwungen: {il}");
            assert!(!il.contains("-field_order"), "{codec}: keine Feldreihenfolge: {il}");
            assert!(!il.contains("ilme"), "{codec}: keine Feldkodierung: {il}");
        }
    }

    #[test]
    fn validate_rejects_codec_container_mismatch() {
        let (tl, media) = state_with(
            vec![track("v", TrackKind::Video)],
            vec![clip("c", "v", TrackKind::Video, "V", 0.0, 4.0)],
            vec![video_asset("V", "/v.mov")],
        );
        // ProRes (nur MOV/MXF) in einen MP4-Container → Fehler.
        let mut settings = test_settings();
        settings.video = Some(default_video("prores", 1920, 1080, 25.0));
        settings.container = container("mp4");
        settings.output = "/tmp/out.mp4".into();
        let issues = validate(&tl, &media, Some(true), None, &settings, &NoNests);
        assert!(
            issues.iter().any(|i| i.severity == Severity::Error
                && i.message.contains("passt nicht in den Container")),
            "Codec/Container-Mismatch muss blockieren: {issues:?}"
        );

        // Korrekte Kombination (ProRes in MXF) → kein Kombinations-Fehler.
        let mut ok = test_settings();
        ok.video = Some(default_video("prores", 1920, 1080, 25.0));
        ok.audio = Some(default_audio("pcm24", None));
        ok.container = container("mxf");
        ok.output = "/tmp/out.mxf".into();
        let issues = validate(&tl, &media, Some(true), None, &ok, &NoNests);
        assert!(
            !issues.iter().any(|i| i.message.contains("passt nicht in den Container")),
            "gültige MXF-Kombination darf nicht meckern: {issues:?}"
        );
    }

    #[test]
    fn output_color_detection_and_honest_tags() {
        let stream = |trc: &str, prim: &str, space: &str| crate::core::types::VideoStreamInfo {
            index: 0,
            codec: "x".into(),
            width: 1920,
            height: 1080,
            fps: 25.0,
            pix_fmt: None,
            bitrate: None,
            bit_depth: 10,
            color_transfer: (!trc.is_empty()).then(|| trc.into()),
            color_primaries: (!prim.is_empty()).then(|| prim.into()),
            color_space: (!space.is_empty()).then(|| space.into()),
            color_range: None,
        };
        assert_eq!(OutputColor::from_stream(&stream("", "", "")), OutputColor::Bt709);
        assert_eq!(OutputColor::from_stream(&stream("bt709", "bt709", "bt709")), OutputColor::Bt709);
        assert_eq!(
            OutputColor::from_stream(&stream("smpte2084", "bt2020", "bt2020nc")),
            OutputColor::Bt2020Pq
        );
        assert_eq!(
            OutputColor::from_stream(&stream("arib-std-b67", "bt2020", "bt2020nc")),
            OutputColor::Bt2020Hlg
        );
        assert_eq!(
            OutputColor::from_stream(&stream("bt2020-10", "bt2020", "bt2020nc")),
            OutputColor::Bt2020
        );
        // Ehrliche Tags + passende Matrix.
        assert_eq!(OutputColor::Bt709.tags(), ("bt709", "bt709", "bt709"));
        assert_eq!(OutputColor::Bt2020Pq.tags(), ("bt2020", "smpte2084", "bt2020nc"));
        assert_eq!(OutputColor::Bt709.scale_matrix(), "bt709");
        assert_eq!(OutputColor::Bt2020Pq.scale_matrix(), "bt2020nc");

        // video_codec_args trägt den erkannten Farbraum in Filter + Tags.
        let v = default_video("hevc", 1920, 1080, 25.0);
        let sdr = video_codec_args(&v, container("mp4"), OutputColor::Bt709).join(" ");
        assert!(sdr.contains("out_color_matrix=bt709") && sdr.contains("-colorspace bt709"));
        let hdr = video_codec_args(&v, container("mp4"), OutputColor::Bt2020Pq).join(" ");
        assert!(hdr.contains("out_color_matrix=bt2020nc"), "HDR-Matrix: {hdr}");
        assert!(hdr.contains("-color_trc smpte2084"), "PQ-Transfer-Tag: {hdr}");
        assert!(hdr.contains("-color_primaries bt2020"), "BT.2020-Primaries: {hdr}");
    }

    #[test]
    fn pipe_format_follows_target_bit_depth() {
        // 8-Bit-Ziele ⇒ rgba-Pipe (mit Dithering quantisiert).
        let h264 = default_video("h264", 1920, 1080, 25.0);
        assert!(!pipe_hi_bit(&h264));
        assert_eq!(pipe_pix_fmt(&h264), "rgba");
        assert_eq!(pipe_bytes_per_px(&h264), 4);

        // ProRes ist immer 10-Bit ⇒ rgba64le-Pipe (16 Bit/Kanal).
        let prores = default_video("prores", 1920, 1080, 25.0);
        assert_eq!(resolved_output_pix_fmt(&prores), "yuv422p10le");
        assert!(pipe_hi_bit(&prores));
        assert_eq!(pipe_pix_fmt(&prores), "rgba64le");
        assert_eq!(pipe_bytes_per_px(&prores), 8);

        // DNxHR: 8-Bit-Profile ⇒ rgba, 10-Bit-Profile (HQX) ⇒ rgba64le.
        let mut dnx = default_video("dnxhr", 1920, 1080, 25.0);
        dnx.profile = 0; // LB → yuv422p (8 Bit)
        assert!(!pipe_hi_bit(&dnx));
        dnx.profile = 3; // HQX → yuv422p10le
        assert!(pipe_hi_bit(&dnx), "DNxHR HQX ist 10-Bit");
    }

    #[test]
    fn export_range_uses_in_out_only_when_valid() {
        let (mut tl, _media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("a", "v1", TrackKind::Video, "A", 0.0, 10.0)],
            vec![],
        );
        assert_eq!(export_range(&tl, false), (0.0, 10.0));
        assert_eq!(export_range(&tl, true), (0.0, 10.0)); // kein In/Out gesetzt
        tl.in_point = Some(2.0);
        tl.out_point = Some(6.0);
        assert_eq!(export_range(&tl, true), (2.0, 6.0));
        tl.out_point = Some(2.0); // leerer Bereich → Fallback ganze Sequenz
        assert_eq!(export_range(&tl, true), (0.0, 10.0));
    }

    /// End-to-End: echte Medien erzeugen, Sequenz rendern, Ergebnis proben.
    /// Timeline: Video (0–2 s, src_in 0,5), Bild (2–3 s), Lücke (3–4 s),
    /// Audio über die volle Länge.
    #[test]
    fn end_to_end_export_renders_playable_file() {
        let dir = std::env::temp_dir().join(format!("editron-export-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_video = dir.join("quelle.mp4");
        let src_image = dir.join("bild.png");
        let out = dir.join("ergebnis.mp4");

        // Testquellen generieren (testsrc + Sinuston, rotes Standbild).
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "testsrc=duration=4:size=640x360:rate=25"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=4"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-shortest"])
            .arg(&src_video)
            .status()
            .expect("ffmpeg nicht startbar — Tests brauchen ffmpeg im PATH");
        assert!(gen.success(), "Testvideo konnte nicht erzeugt werden");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "color=red:size=320x240", "-frames:v", "1"])
            .arg(&src_image)
            .status()
            .unwrap();
        assert!(gen.success(), "Testbild konnte nicht erzeugt werden");

        let mut image_asset = video_asset("IMG", &src_image.to_string_lossy());
        image_asset.kind = MediaKind::Image;
        image_asset.info.video.clear();
        image_asset.info.audio.clear();
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video), track("a1", TrackKind::Audio)],
            vec![
                {
                    let mut c = clip("v", "v1", TrackKind::Video, "VID", 0.0, 2.0);
                    c.src_in = 0.5;
                    c
                },
                {
                    let mut c = clip("img", "v1", TrackKind::Video, "IMG", 2.0, 1.0);
                    c.src_duration = f64::INFINITY;
                    c
                },
                clip("a", "a1", TrackKind::Audio, "VID", 0.0, 4.0),
            ],
            vec![video_asset("VID", &src_video.to_string_lossy()), image_asset],
        );

        let mut settings = test_settings();
        settings.output = out.to_string_lossy().into_owned();
        if let Some(v) = settings.video.as_mut() {
            v.width = 640;
            v.height = 360;
            v.speed = 0; // ultrafast
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.total_frames, 100);
        assert_eq!(plan.segments.len(), 3); // Video, Bild, Schwarz

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let children = Arc::new(Mutex::new(Vec::new()));
        run_export_worker(
            "test-job".into(),
            plan,
            settings,
            tx,
            cancel,
            children,
        );

        let mut done: Option<(bool, Option<String>)> = None;
        let mut saw_progress = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                ServiceEvent::SequenceExportProgress { eta_sec: _, .. } => saw_progress = true,
                ServiceEvent::SequenceExportDone { ok, error, .. } => done = Some((ok, error)),
                _ => {}
            }
        }
        let (ok, error) = done.expect("Done-Event fehlt");
        assert!(ok, "Export fehlgeschlagen: {error:?}");
        assert!(saw_progress, "keine Progress-Events");
        assert!(out.exists(), "Zieldatei fehlt");
        assert!(!part_path(&out.to_string_lossy()).exists(), ".part nicht aufgeräumt");

        // Ergebnis proben: Dauer ≈ 4 s, 640×360 @ 25, Video + Audio vorhanden.
        let info = crate::services::probe_media(&out.to_string_lossy()).expect("probe");
        assert!((info.duration_sec - 4.0).abs() < 0.25, "Dauer: {}", info.duration_sec);
        assert_eq!(info.video.len(), 1);
        assert_eq!(info.audio.len(), 1);
        assert_eq!((info.video[0].width, info.video[0].height), (640, 360));
        assert!((info.video[0].fps - 25.0).abs() < 0.01);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Integrierte Lautheit eines Mediums per ffmpeg `loudnorm` (JSON-Messung)
    /// auslesen — `input_i` aus dem an den Log angehängten JSON-Block.
    fn measure_integrated_lufs(path: &std::path::Path) -> f64 {
        let out = Command::new(crate::services::ffmpeg_bin())
            .args(["-hide_banner", "-nostats", "-v", "info"])
            .args(["-i", &path.to_string_lossy()])
            .args(["-af", "loudnorm=I=-23:print_format=json"])
            .args(["-f", "null", "-"])
            .output()
            .expect("ffmpeg-Messung startbar");
        let log = String::from_utf8_lossy(&out.stderr);
        let start = log.find('{').expect("loudnorm-JSON fehlt");
        let end = log.rfind('}').expect("loudnorm-JSON fehlt");
        let v: serde_json::Value =
            serde_json::from_str(&log[start..=end]).expect("loudnorm-JSON parsebar");
        v.get("input_i")
            .and_then(|x| x.as_str())
            .and_then(|s| s.trim().parse::<f64>().ok())
            .expect("input_i")
    }

    /// Mittlerer Pegel (mean_volume, dBFS) eines einzelnen Audio-Streams einer
    /// Datei — via ffmpeg `volumedetect`. Für die Stem-Verifikation.
    fn stream_mean_db(path: &std::path::Path, stream: usize) -> f64 {
        let out = Command::new(crate::services::ffmpeg_bin())
            .args(["-hide_banner", "-nostats", "-v", "info"])
            .args(["-i", &path.to_string_lossy()])
            .args(["-map", &format!("0:a:{stream}")])
            .args(["-af", "volumedetect"])
            .args(["-f", "null", "-"])
            .output()
            .expect("ffmpeg-Messung startbar");
        let log = String::from_utf8_lossy(&out.stderr);
        for line in log.lines() {
            if let Some(i) = line.find("mean_volume:") {
                let rest = line[i + "mean_volume:".len()..].trim();
                if let Some(num) = rest.split_whitespace().next() {
                    if let Ok(v) = num.parse::<f64>() {
                        return v;
                    }
                }
            }
        }
        panic!("mean_volume nicht gefunden für Stream {stream}: {log}");
    }

    #[test]
    fn stems_route_every_audio_track_separately() {
        // Zwei Audiospuren: im Stems-Modus wird KEINE Master-Summe gebaut,
        // sondern jede Spur als eigener Stem-Track mit Spurname.
        let (tl, media) = state_with(
            vec![track("a1", TrackKind::Audio), track("a2", TrackKind::Audio)],
            vec![
                clip("c1", "a1", TrackKind::Audio, "A", 0.0, 4.0),
                clip("c2", "a2", TrackKind::Audio, "A", 0.0, 4.0),
            ],
            vec![video_asset("A", "/a.mp4")],
        );
        let mut settings = test_settings(); // mp4 trägt mehrere Audio-Streams
        settings.audio_stems = true;
        assert!(stems_enabled(&settings));
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert!(plan.audio.is_empty(), "Stems: kein Schnellpfad-Master");
        assert_eq!(plan.audio_tracks.len(), 2, "je Spur ein Stem");
        assert_eq!(plan.audio_tracks[0].name, "A1");
        assert_eq!(plan.audio_tracks[1].name, "A2");

        // WAV trägt nur EINEN Audio-Stream ⇒ kein Stems-Modus, Schnellpfad bleibt.
        settings.container = container("wav");
        settings.video = None;
        settings.audio = Some(default_audio("pcm24", None));
        assert!(!stems_enabled(&settings), "WAV kann keine Stems tragen");
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.audio.len(), 2, "ohne Stems: summierter Schnellpfad");
        assert!(plan.audio_tracks.is_empty());
    }

    #[test]
    fn validate_accepts_audio_only_when_all_tracks_are_processed() {
        // Regression: Audio-only-Export, dessen Spuren ALLE getrennt verarbeitet
        // werden (Bus-FX bzw. Stems), landet vollständig in `plan.audio_tracks`
        // und `plan.audio` bleibt leer — die Validierung darf das NICHT als
        // „keine abspielbaren Clips" blockieren (has_audio_media zählt beide).
        use crate::core::effects::{EffectInstance, EffectKind};
        let audio_asset = |id: &str, path: &str| {
            let mut a = video_asset(id, path);
            a.kind = MediaKind::Audio;
            a.info.video.clear();
            a
        };

        // (a) Einzelspur mit Bus-EQ → Audio-only WAV (kein Stems-Container).
        let mut t = track("a1", TrackKind::Audio);
        t.effects.push(EffectInstance::new(EffectKind::Equalizer));
        let (tl, media) = state_with(
            vec![t],
            vec![clip("c", "a1", TrackKind::Audio, "A", 0.0, 4.0)],
            vec![audio_asset("A", "/a.mov")],
        );
        let mut settings = test_settings();
        settings.container = container("wav");
        settings.video = None;
        settings.audio = Some(default_audio("pcm24", None));
        settings.output = "/tmp/out.wav".into();
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert!(plan.audio.is_empty() && plan.audio_tracks.len() == 1, "Bus-Spur ⇒ audio_tracks");
        assert!(plan.has_audio_media(), "Bus-Spur zählt als Audio");
        let issues = validate(&tl, &media, Some(true), None, &settings, &NoNests);
        assert!(
            !issues.iter().any(|i| i.severity == Severity::Error),
            "Bus-FX-Audiospur darf nicht blockiert werden: {issues:?}"
        );

        // (b) Zwei Spuren, Audio-only Stems nach M4A (multi_audio).
        let (tl, media) = state_with(
            vec![track("a1", TrackKind::Audio), track("a2", TrackKind::Audio)],
            vec![
                clip("c1", "a1", TrackKind::Audio, "A", 0.0, 4.0),
                clip("c2", "a2", TrackKind::Audio, "A", 0.0, 4.0),
            ],
            vec![audio_asset("A", "/a.mov")],
        );
        let mut settings = test_settings();
        settings.container = container("m4a");
        settings.video = None;
        settings.audio = Some(default_audio("aac", Some(256)));
        settings.audio_stems = true;
        settings.output = "/tmp/out.m4a".into();
        assert!(stems_enabled(&settings));
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.audio_tracks.len(), 2, "zwei Stems");
        assert!(plan.audio.is_empty());
        let issues = validate(&tl, &media, Some(true), None, &settings, &NoNests);
        assert!(
            !issues.iter().any(|i| i.severity == Severity::Error),
            "Audio-only-Stems-Export darf nicht blockiert werden: {issues:?}"
        );
    }

    /// End-to-End: Ein Export mit zwei Audiospuren erzeugt im Stems-Modus zwei
    /// getrennte Audio-Streams im Container, deren Inhalt den Einzelspuren
    /// entspricht (lauter A1-Stem vs. um 18 dB leiserer A2-Stem). Bestätigt:
    /// keine Master-Summe, Spur-Gain pro Stem angewandt, Streams nicht vertauscht.
    #[test]
    fn end_to_end_export_writes_separate_stems() {
        let dir = std::env::temp_dir().join(format!("editron-export-stems-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_a = dir.join("a.wav");
        let src_b = dir.join("b.wav");
        let out = dir.join("stems.mov");

        // Beide Quellen Vollpegel-Sinus (A 440 Hz, B 2 kHz) — gleicher Pegel.
        for (path, freq) in [(&src_a, 440), (&src_b, 2000)] {
            let gen = Command::new(crate::services::ffmpeg_bin())
                .args(["-y", "-v", "error"])
                .args(["-f", "lavfi", "-i", &format!("sine=frequency={freq}:duration=3:sample_rate=48000")])
                .args(["-c:a", "pcm_s16le"])
                .arg(path)
                .status()
                .expect("ffmpeg nicht startbar — Tests brauchen ffmpeg im PATH");
            assert!(gen.success(), "Testton nicht erzeugt");
        }

        let audio_asset = |id: &str, path: &std::path::Path| {
            let mut a = video_asset(id, &path.to_string_lossy());
            a.kind = MediaKind::Audio;
            a.info.video.clear();
            a
        };
        // Spur A2 hängt 18 dB am SPUR-Fader herunter — beweist, dass Spur-Gain
        // pro Stem angewandt bleibt (nicht erst in einer Master-Summe).
        let mut t2 = track("a2", TrackKind::Audio);
        t2.gain_db = -18.0;
        let (tl, media) = state_with(
            vec![track("a1", TrackKind::Audio), t2],
            vec![
                clip("c1", "a1", TrackKind::Audio, "A", 0.0, 3.0),
                clip("c2", "a2", TrackKind::Audio, "B", 0.0, 3.0),
            ],
            vec![audio_asset("A", &src_a), audio_asset("B", &src_b)],
        );

        let mut settings = test_settings();
        settings.container = container("mov");
        settings.video = None;
        settings.audio = Some(default_audio("pcm24", None));
        settings.audio_stems = true;
        settings.output = out.to_string_lossy().into_owned();

        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.audio_tracks.len(), 2, "zwei Stem-Tracks im Plan");
        assert!(plan.audio.is_empty(), "kein Schnellpfad-Master");

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let children = Arc::new(Mutex::new(Vec::new()));
        run_export_worker("stems-job".into(), plan, settings, tx, cancel, children);

        let mut done: Option<(bool, Option<String>)> = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok, error, .. } = ev {
                done = Some((ok, error));
            }
        }
        let (ok, error) = done.expect("Done-Event fehlt");
        assert!(ok, "Stems-Export fehlgeschlagen: {error:?}");
        assert!(out.exists(), "Zieldatei fehlt");

        // Genau zwei Audio-Streams (Stems) — nicht ein summierter Master.
        let info = crate::services::probe_media(&out.to_string_lossy()).expect("probe");
        assert_eq!(info.audio.len(), 2, "zwei Stems erwartet, kein Master-Mix");

        // Inhalt = die Einzelspuren: Stem 0 (A1, Spur-Gain 0 dB) trägt den
        // Vollpegel-Ton, Stem 1 (A2, Spur-Gain −18 dB) liegt 18 dB darunter.
        // Wären die Spuren zu einem Master summiert, hätten beide Streams den
        // gleichen Pegel — die Differenz beweist die getrennten Stems samt
        // angewandtem Spur-Gain.
        let mean_a = stream_mean_db(&out, 0);
        let mean_b = stream_mean_db(&out, 1);
        assert!(mean_a > -40.0, "Stem A1 muss hörbar sein, war {mean_a}");
        assert!(mean_b > -60.0, "Stem A2 muss hörbar sein, war {mean_b}");
        let diff = mean_a - mean_b;
        assert!(
            (diff - 18.0).abs() <= 4.0,
            "Stem A2 muss ~18 dB (Spur-Gain) unter A1 liegen: A1={mean_a}, A2={mean_b}, diff={diff}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Mittlerer Pegel (mean_volume, dBFS) eines Zeitfensters eines Streams —
    /// via ffmpeg `atrim`+`volumedetect`. Für die fenster-genaue Ducking-
    /// Verifikation (Pegel vor vs. während des Key-Signals).
    fn segment_mean_db(path: &std::path::Path, stream: usize, start: f64, end: f64) -> f64 {
        let out = Command::new(crate::services::ffmpeg_bin())
            .args(["-hide_banner", "-nostats", "-v", "info"])
            .args(["-i", &path.to_string_lossy()])
            .args(["-map", &format!("0:a:{stream}")])
            .args([
                "-af",
                &format!("atrim=start={start}:end={end},volumedetect"),
            ])
            .args(["-f", "null", "-"])
            .output()
            .expect("ffmpeg-Messung startbar");
        let log = String::from_utf8_lossy(&out.stderr);
        for line in log.lines() {
            if let Some(i) = line.find("mean_volume:") {
                let rest = line[i + "mean_volume:".len()..].trim();
                if let Some(num) = rest.split_whitespace().next() {
                    if let Ok(v) = num.parse::<f64>() {
                        return v;
                    }
                }
            }
        }
        panic!("mean_volume nicht gefunden für Stream {stream} [{start}..{end}]: {log}");
    }

    /// End-to-End: Auto-Ducking im EXPORT-Pfad. Die Musikspur (a1) trägt einen
    /// Ducking-Bus-Effekt, dessen Sidechain-Key die andere Spur (a2, „Sprache")
    /// ist. Solange a2 still ist, bleibt die Musik laut; sobald a2 ab Sekunde 2
    /// einsetzt, muss der gerenderte Musik-Stem deutlich abgesenkt sein —
    /// formelgleich zum Player (gleiche `AudioFxChain`, Key = Summe der anderen
    /// Spuren). Stems-Export, damit die Musikspur isoliert messbar ist.
    #[test]
    fn end_to_end_ducking_lowers_music_under_speech_key() {
        let dir = std::env::temp_dir().join(format!("editron-export-duck-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_music = dir.join("music.wav");
        let src_key = dir.join("key.wav");
        let out = dir.join("duck.mov");

        // Musik: 200-Hz-Sinus, 4 s, Vollpegel. Key/„Sprache": 1-kHz-Sinus, 2 s
        // (wird ab Sekunde 2 platziert).
        for (path, freq, dur) in [(&src_music, 200, 4), (&src_key, 1000, 2)] {
            let gen = Command::new(crate::services::ffmpeg_bin())
                .args(["-y", "-v", "error"])
                .args([
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("sine=frequency={freq}:duration={dur}:sample_rate=48000"),
                ])
                .args(["-c:a", "pcm_s16le"])
                .arg(path)
                .status()
                .expect("ffmpeg nicht startbar — Tests brauchen ffmpeg im PATH");
            assert!(gen.success(), "Testton nicht erzeugt");
        }

        let audio_asset = |id: &str, path: &std::path::Path| {
            let mut a = video_asset(id, &path.to_string_lossy());
            a.kind = MediaKind::Audio;
            a.info.video.clear();
            a
        };

        // Musikspur a1 mit Ducking-Bus-Effekt (niedrige Schwelle, schneller
        // Attack ⇒ der Vollpegel-Key drückt die Musik kräftig herunter).
        let mut duck = crate::core::effects::EffectInstance::new(
            crate::core::effects::EffectKind::Ducking,
        );
        duck.params[0] = AnimatedParam::fixed(-45.0); // Threshold
        duck.params[1] = AnimatedParam::fixed(12.0); // Ratio
        duck.params[2] = AnimatedParam::fixed(5.0); // Attack ms
        duck.params[3] = AnimatedParam::fixed(80.0); // Release ms
        let mut t1 = track("a1", TrackKind::Audio);
        t1.effects.push(duck);
        let (tl, media) = state_with(
            vec![t1, track("a2", TrackKind::Audio)],
            vec![
                clip("c1", "a1", TrackKind::Audio, "MUSIC", 0.0, 4.0),
                // Key setzt erst ab Sekunde 2 ein.
                clip("c2", "a2", TrackKind::Audio, "KEY", 2.0, 2.0),
            ],
            vec![
                audio_asset("MUSIC", &src_music),
                audio_asset("KEY", &src_key),
            ],
        );

        let mut settings = test_settings();
        settings.container = container("mov");
        settings.video = None;
        settings.audio = Some(default_audio("pcm24", None));
        settings.audio_stems = true;
        settings.output = out.to_string_lossy().into_owned();

        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.audio_tracks.len(), 2, "zwei Stem-Tracks (Musik + Key)");

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let children = Arc::new(Mutex::new(Vec::new()));
        run_export_worker("duck-job".into(), plan, settings, tx, cancel, children);

        let mut done: Option<(bool, Option<String>)> = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok, error, .. } = ev {
                done = Some((ok, error));
            }
        }
        let (ok, error) = done.expect("Done-Event fehlt");
        assert!(ok, "Ducking-Export fehlgeschlagen: {error:?}");
        assert!(out.exists(), "Zieldatei fehlt");

        // Musik-Stem (Stream 0): vor dem Key laut, während des Keys abgesenkt.
        let open = segment_mean_db(&out, 0, 0.3, 1.5);
        let ducked = segment_mean_db(&out, 0, 2.6, 3.8);
        assert!(open > -40.0, "Musik vor dem Key hörbar: {open} dBFS");
        assert!(
            open - ducked > 15.0,
            "Key senkt die Musik deutlich ab: vorher {open} dBFS, geduckt {ducked} dBFS (Δ {})",
            open - ducked
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loudnorm_json_parses_measured_values() {
        // Realistischer loudnorm-Log: Text-Präfix + angehängter JSON-Block.
        let log = "[Parsed_loudnorm_0 @ 0x55] \n\
            {\n\
            \t\"input_i\" : \"-3.01\",\n\
            \t\"input_tp\" : \"-0.00\",\n\
            \t\"input_lra\" : \"0.00\",\n\
            \t\"input_thresh\" : \"-13.20\",\n\
            \t\"output_i\" : \"-23.00\",\n\
            \t\"target_offset\" : \"0.05\"\n\
            }\n";
        let m = parse_loudnorm_json(log).expect("Messwerte");
        assert!((m.input_i - (-3.01)).abs() < 1e-9);
        assert!((m.input_thresh - (-13.20)).abs() < 1e-9);
        assert!((m.target_offset - 0.05).abs() < 1e-9);
        assert!(m.all_finite());
    }

    #[test]
    fn loudnorm_json_handles_silence_as_non_finite() {
        // Stille liefert -inf-Messwerte → all_finite() = false (Pass 2 entfällt).
        let log = "{ \"input_i\" : \"-inf\", \"input_tp\" : \"-inf\", \
            \"input_lra\" : \"0.00\", \"input_thresh\" : \"-inf\", \
            \"target_offset\" : \"0.00\" }";
        let m = parse_loudnorm_json(log).expect("Messwerte");
        assert!(!m.all_finite(), "stille Messung darf nicht normalisiert werden");
    }

    /// End-to-End: Lautheits-Normalisierung bringt einen lauten Testton (1 kHz
    /// Vollpegel, ≈ −3 LUFS) beim Export auf das EBU-R128-Ziel von −23 LUFS.
    /// Verifikation der 2-Pass-loudnorm-Kette im Audio-Export-Pfad (±1 LU).
    #[test]
    fn end_to_end_loudness_normalization_hits_target() {
        let dir =
            std::env::temp_dir().join(format!("editron-export-loud-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("ton.wav");
        let out = dir.join("norm.wav");

        // Lauter 1-kHz-Sinus (+15 dB über dem leisen lavfi-Default ≈ −6 LUFS)
        // als Quelle — deutlich über dem −23-LUFS-Ziel, damit die
        // Normalisierung echte Arbeit leistet.
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "sine=frequency=1000:duration=5:sample_rate=48000"])
            .args(["-af", "volume=15dB"])
            .args(["-c:a", "pcm_s16le"])
            .arg(&src)
            .status()
            .expect("ffmpeg nicht startbar — Tests brauchen ffmpeg im PATH");
        assert!(gen.success(), "Testton konnte nicht erzeugt werden");

        // Sanity: die unnormalisierte Quelle ist deutlich lauter als −23 LUFS.
        let src_lufs = measure_integrated_lufs(&src);
        assert!(src_lufs > -12.0, "Quelle unerwartet leise: {src_lufs}");

        let (tl, media) = state_with(
            vec![track("a1", TrackKind::Audio)],
            vec![clip("a", "a1", TrackKind::Audio, "TON", 0.0, 5.0)],
            vec![{
                let mut a = video_asset("TON", &src.to_string_lossy());
                a.kind = MediaKind::Audio;
                a.info.video.clear();
                a
            }],
        );

        let mut settings = test_settings();
        settings.container = container("wav");
        settings.video = None;
        settings.audio = Some(default_audio("pcm24", None));
        settings.loudness = Some(LoudnessNorm::EBU_R128);
        settings.output = out.to_string_lossy().into_owned();

        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let children = Arc::new(Mutex::new(Vec::new()));
        run_export_worker("loud-job".into(), plan, settings, tx, cancel, children);

        let mut done: Option<(bool, Option<String>)> = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok, error, .. } = ev {
                done = Some((ok, error));
            }
        }
        let (ok, error) = done.expect("Done-Event fehlt");
        assert!(ok, "Export fehlgeschlagen: {error:?}");
        assert!(out.exists(), "Zieldatei fehlt");

        // Gemessene integrierte Lautheit der Ausgabe muss am Ziel liegen (±1 LU).
        let measured = measure_integrated_lufs(&out);
        assert!(
            (measured - (-23.0)).abs() <= 1.0,
            "Integrated LUFS {measured} weicht > 1 LU von −23 ab"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-End: Lautheits-Normalisierung im VIDEO-Export-Pfad. Sichert ab,
    /// dass die normalisierte WAV nach dem Rename auch vom Encoder gemuxt wird
    /// (anderer Pfad als Audio-only: render_video statt encode_audio_only, plus
    /// die Phasen-Basis-Arithmetik bei Video+Normalisierung). Ziel −16 LUFS.
    #[test]
    fn end_to_end_loudness_normalization_in_video_export() {
        let dir =
            std::env::temp_dir().join(format!("editron-export-loudvid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("clip.mp4");
        let out = dir.join("norm.mp4");

        // Kleines Testvideo mit lautem Ton (+15 dB) als Quelle.
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "testsrc=duration=4:size=160x120:rate=25"])
            .args(["-f", "lavfi", "-i", "sine=frequency=1000:duration=4"])
            .args(["-af", "volume=15dB"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-shortest"])
            .arg(&src)
            .status()
            .expect("ffmpeg nicht startbar — Tests brauchen ffmpeg im PATH");
        assert!(gen.success(), "Testvideo konnte nicht erzeugt werden");

        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video), track("a1", TrackKind::Audio)],
            vec![
                clip("v", "v1", TrackKind::Video, "VID", 0.0, 4.0),
                clip("a", "a1", TrackKind::Audio, "VID", 0.0, 4.0),
            ],
            vec![video_asset("VID", &src.to_string_lossy())],
        );

        let mut settings = test_settings();
        settings.audio = Some(default_audio("aac", Some(192)));
        settings.loudness = Some(LOUDNESS_PRESETS[1].norm); // −16 LUFS
        settings.output = out.to_string_lossy().into_owned();
        if let Some(v) = settings.video.as_mut() {
            v.width = 160;
            v.height = 120;
            v.speed = 0; // ultrafast
        }

        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let children = Arc::new(Mutex::new(Vec::new()));
        run_export_worker("loudvid-job".into(), plan, settings, tx, cancel, children);

        let mut done: Option<(bool, Option<String>)> = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok, error, .. } = ev {
                done = Some((ok, error));
            }
        }
        let (ok, error) = done.expect("Done-Event fehlt");
        assert!(ok, "Video-Export mit Normalisierung fehlgeschlagen: {error:?}");
        assert!(out.exists(), "Zieldatei fehlt");

        // Datei trägt Video + Audio, und die Tonspur liegt am −16-LUFS-Ziel.
        let info = crate::services::probe_media(&out.to_string_lossy()).expect("probe");
        assert_eq!(info.video.len(), 1, "Videospur fehlt");
        assert_eq!(info.audio.len(), 1, "Tonspur fehlt");
        let measured = measure_integrated_lufs(&out);
        assert!(
            (measured - (-16.0)).abs() <= 1.5, // AAC-Verlust ⇒ etwas weiter
            "Integrated LUFS {measured} weicht > 1,5 LU von −16 ab"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-End: eine verschachtelte Sequenz (Nest-Clip) wird rekursiv
    /// komponiert. Das innere Material ist grün → der Ausgabe-Mittelpixel muss
    /// grün sein (nicht schwarz), und die Datei abspielbar mit Zielraster.
    #[test]
    fn end_to_end_export_renders_nested_sequence() {
        use crate::core::compose::NestResolver;
        let dir = std::env::temp_dir().join(format!("editron-export-nest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_video = dir.join("gruen.mp4");
        let out = dir.join("nest.mp4");

        // Grünes Vollbild-Testvideo (füllt das Zielraster ohne Letterbox).
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "color=0x00B000:size=640x360:rate=25:duration=2"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
            .arg(&src_video)
            .status()
            .expect("ffmpeg nicht startbar — Tests brauchen ffmpeg im PATH");
        assert!(gen.success(), "Testvideo konnte nicht erzeugt werden");

        // Innere Sequenz (640×360): grüner Clip 0..2.
        let (mut inner, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("g", "v1", TrackKind::Video, "GREEN", 0.0, 2.0)],
            vec![video_asset("GREEN", &src_video.to_string_lossy())],
        );
        inner.settings.width = 640;
        inner.settings.height = 360;
        // Äußere Sequenz: ein Nest-Clip 0..2 (volle Deckkraft, Identität).
        let (mut outer, _) = state_with(
            vec![track("ov", TrackKind::Video)],
            vec![clip("n", "ov", TrackKind::Video, "GREEN", 0.0, 2.0)],
            vec![video_asset("GREEN", &src_video.to_string_lossy())],
        );
        outer.clips[0].asset_id = String::new();
        outer.clips[0].nest_seq = Some("inner".into());

        struct R<'a>(HashMap<String, &'a TimelineStore>);
        impl NestResolver for R<'_> {
            fn nested_timeline(&self, id: &str) -> Option<&TimelineStore> {
                self.0.get(id).copied()
            }
        }
        let resolver = R(HashMap::from([("inner".to_string(), &inner)]));

        let mut settings = test_settings();
        settings.output = out.to_string_lossy().into_owned();
        settings.audio = None; // Video-only (Nest-Audio ist hier nicht Gegenstand)
        if let Some(v) = settings.video.as_mut() {
            v.width = 640;
            v.height = 360;
            v.fps = 25.0;
            v.speed = 0;
        }
        let plan = build_render_plan(&outer, &media, &settings, &resolver);
        // Nest-Ebene im Plan + Kontext eingesammelt.
        assert!(plan
            .segments
            .iter()
            .flat_map(|s| s.layers.iter())
            .any(|l| l.nest_seq.as_deref() == Some("inner")));
        assert!(plan.nests.contains_key("inner"));

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let children = Arc::new(Mutex::new(Vec::new()));
        run_export_worker("nest-job".into(), plan, settings, tx, cancel, children);

        let mut done: Option<(bool, Option<String>)> = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok, error, .. } = ev {
                done = Some((ok, error));
            }
        }
        let (ok, error) = done.expect("Done-Event fehlt");
        assert!(ok, "Nest-Export fehlgeschlagen: {error:?}");
        assert!(out.exists(), "Zieldatei fehlt");

        // Mittelframe extrahieren und Mittelpixel prüfen (grün, nicht schwarz).
        let probe = Command::new(crate::services::ffmpeg_bin())
            .args(["-v", "error", "-ss", "1", "-i"])
            .arg(&out)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
            .output()
            .expect("ffmpeg-Probe");
        let data = probe.stdout;
        assert_eq!(data.len(), 640 * 360 * 4, "ein RGBA-Frame erwartet");
        let center = (180 * 640 + 320) * 4;
        let (r, g, b) = (data[center], data[center + 1], data[center + 2]);
        assert!(g > 120 && r < 100 && b < 100, "Mittelpixel grün erwartet, war ({r},{g},{b})");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-End: PNG-Sequenz-Export schreibt nummerierte Frames an den
    /// Zielort und räumt das temporäre Verzeichnis auf (atomar).
    #[test]
    fn end_to_end_export_renders_png_sequence() {
        let dir = std::env::temp_dir().join(format!("editron-export-seq-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_video = dir.join("quelle.mp4");
        let out = dir.join("frame.png");

        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "testsrc=duration=1:size=320x180:rate=25"])
            .args(["-c:v", "libx264", "-preset", "ultrafast"])
            .arg(&src_video)
            .status()
            .expect("ffmpeg nicht startbar");
        assert!(gen.success());

        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("v", "v1", TrackKind::Video, "VID", 0.0, 0.4)],
            vec![video_asset("VID", &src_video.to_string_lossy())],
        );

        let mut settings = ExportSettings {
            container: container("png_seq"),
            video: Some(default_video("png", 320, 180, 25.0)),
            audio: None,
            loudness: None,
            use_in_out: false,
            audio_stems: false,
            subtitles: SubtitleMode::None,
            image_start: 1,
            output: out.to_string_lossy().into_owned(),
        };
        if let Some(v) = settings.video.as_mut() {
            v.width = 320;
            v.height = 180;
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.total_frames, 10); // 0,4 s @ 25 fps

        let (tx, rx) = std::sync::mpsc::channel();
        run_export_worker(
            "seq-job".into(),
            plan,
            settings,
            tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let mut done: Option<(bool, Option<String>)> = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok, error, .. } = ev {
                done = Some((ok, error));
            }
        }
        let (ok, error) = done.expect("Done-Event fehlt");
        assert!(ok, "Sequenz-Export fehlgeschlagen: {error:?}");

        // Nummerierte Frames liegen am Ziel, das Temp-Verzeichnis ist weg.
        let frames: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.starts_with("frame_") && n.ends_with(".png"))
            .collect();
        assert_eq!(frames.len(), 10, "10 Frames erwartet: {frames:?}");
        assert!(frames.contains(&"frame_000001.png".to_string()));
        let tmp = dir.join(".editron-seq-seq-job");
        assert!(!tmp.exists(), "Temp-Sequenz-Verzeichnis nicht aufgeräumt");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-End: Geschwindigkeit (50 %), Rückwärts und Standbild real
    /// rendern und proben — alle drei Pfade (Schnellpfad-setpts,
    /// Reverse-Chunk, Freeze-Halten) laufen durch.
    #[test]
    fn end_to_end_export_renders_speed_reverse_freeze() {
        let dir = std::env::temp_dir().join(format!("editron-export-speed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_video = dir.join("quelle.mp4");
        // 8-s-Quelle, damit 50 % (Spanne 4 s) und die Reverse-Handles passen.
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "testsrc=duration=8:size=320x240:rate=25"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=8"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-shortest"])
            .arg(&src_video)
            .status()
            .expect("ffmpeg nicht startbar");
        assert!(gen.success());

        for (name, speed, reverse, freeze) in [
            ("slow.mp4", 0.5, false, false),
            ("rev.mp4", 1.0, true, false),
            ("freeze.mp4", 1.0, false, true),
        ] {
            let out = dir.join(name);
            let mut v = clip("v", "v1", TrackKind::Video, "VID", 0.0, 2.0);
            v.src_in = 2.0;
            v.src_duration = 8.0;
            v.speed = crate::core::animation::AnimatedParam::fixed(speed);
            v.reverse = reverse;
            v.freeze = freeze;
            let (tl, media) = state_with(
                vec![track("v1", TrackKind::Video)],
                vec![v],
                vec![video_asset("VID", &src_video.to_string_lossy())],
            );
            let mut settings = test_settings();
            settings.audio = None; // Rückwärts/Standbild sind stumm
            settings.output = out.to_string_lossy().into_owned();
            if let Some(vs) = settings.video.as_mut() {
                vs.width = 320;
                vs.height = 240;
                vs.speed = 0;
            }
            let plan = build_render_plan(&tl, &media, &settings, &NoNests);
            assert_eq!(plan.total_frames, 50, "{name}");

            let (tx, rx) = std::sync::mpsc::channel();
            let cancel = Arc::new(AtomicBool::new(false));
            let children = Arc::new(Mutex::new(Vec::new()));
            run_export_worker(format!("speed-{name}"), plan, settings, tx, cancel, children);
            let mut done = None;
            while let Ok(ev) = rx.try_recv() {
                if let ServiceEvent::SequenceExportDone { ok, error, .. } = ev {
                    done = Some((ok, error));
                }
            }
            let (ok, error) = done.expect("Done-Event");
            assert!(ok, "{name} fehlgeschlagen: {error:?}");
            let info = crate::services::probe_media(&out.to_string_lossy()).expect("probe");
            assert!((info.duration_sec - 2.0).abs() < 0.25, "{name} Dauer: {}", info.duration_sec);
            assert_eq!((info.video[0].width, info.video[0].height), (320, 240), "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Format-Matrix: ProRes/MOV (Profil-Codec + PCM), VP9/WebM (CRF-Sonderfall
    /// + Opus) und Audio-only M4A (ipod-Muxer + faststart) real rendern.
    #[test]
    fn format_matrix_renders_prores_webm_and_audio_only() {
        let dir = std::env::temp_dir().join(format!("editron-export-matrix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("quelle.mp4");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "testsrc=duration=2:size=320x180:rate=25"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-shortest"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(gen.success());

        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video), track("a1", TrackKind::Audio)],
            vec![
                clip("v", "v1", TrackKind::Video, "VID", 0.0, 2.0),
                clip("a", "a1", TrackKind::Audio, "VID", 0.0, 2.0),
            ],
            vec![video_asset("VID", &src.to_string_lossy())],
        );

        let mut prores = default_video("prores", 320, 180, 25.0);
        prores.profile = 3;
        let mut vp9 = default_video("vp9", 320, 180, 25.0);
        vp9.quality = VideoQuality::Crf(40);
        let cases: Vec<(&str, ExportSettings)> = vec![
            (
                "prores.mov",
                ExportSettings {
                    container: container("mov"),
                    video: Some(prores),
                    audio: Some(default_audio("pcm24", None)),
                    loudness: None,
                    use_in_out: false,
                    audio_stems: false,
                    subtitles: SubtitleMode::None,
                    image_start: 1,
                    output: String::new(),
                },
            ),
            (
                "vp9.webm",
                ExportSettings {
                    container: container("webm"),
                    video: Some(vp9),
                    audio: Some(default_audio("opus", Some(96))),
                    loudness: None,
                    use_in_out: false,
                    audio_stems: false,
                    subtitles: SubtitleMode::None,
                    image_start: 1,
                    output: String::new(),
                },
            ),
            (
                "audio.m4a",
                ExportSettings {
                    container: container("m4a"),
                    video: None,
                    audio: Some(default_audio("aac", Some(128))),
                    loudness: None,
                    use_in_out: false,
                    audio_stems: false,
                    subtitles: SubtitleMode::None,
                    image_start: 1,
                    output: String::new(),
                },
            ),
        ];

        for (name, mut settings) in cases {
            let out = dir.join(name);
            settings.output = out.to_string_lossy().into_owned();
            let plan = build_render_plan(&tl, &media, &settings, &NoNests);
            let (tx, rx) = std::sync::mpsc::channel();
            run_export_worker(
                format!("matrix-{name}"),
                plan,
                settings.clone(),
                tx,
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(Vec::new())),
            );
            let mut ok = false;
            let mut error = None;
            while let Ok(ev) = rx.try_recv() {
                if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                    ok = o;
                    error = e;
                }
            }
            assert!(ok, "{name}: {error:?}");
            let info = crate::services::probe_media(&out.to_string_lossy()).expect(name);
            assert!((info.duration_sec - 2.0).abs() < 0.3, "{name}: Dauer {}", info.duration_sec);
            match name {
                "prores.mov" => {
                    assert_eq!(info.video[0].codec, "prores", "{name}");
                    // ProRes 422 HQ ist echtes 10-Bit (yuv422p10le) — der
                    // Compositor speist über die 16-Bit-rgba64le-Pipe ein.
                    let pf = info.video[0].pix_fmt.as_deref().unwrap_or("");
                    assert!(pf.contains("10"), "{name}: 10-Bit-Ausgabe erwartet, war {pf}");
                    assert_eq!(info.audio[0].codec, "pcm_s24le", "{name}");
                }
                "vp9.webm" => {
                    assert_eq!(info.video[0].codec, "vp9", "{name}");
                    assert_eq!(info.audio[0].codec, "opus", "{name}");
                    assert_eq!(info.audio[0].sample_rate, 48000, "{name}");
                }
                "audio.m4a" => {
                    assert!(info.video.is_empty(), "{name}: darf kein Video haben");
                    assert_eq!(info.audio[0].codec, "aac", "{name}");
                }
                _ => unreachable!(),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Ein einzelnes ffprobe-Stream-/Format-Feld als String (leer = unbekannt).
    fn ffprobe_field(path: &std::path::Path, entries: &str) -> String {
        let out = Command::new(crate::services::ffprobe_bin())
            .args(["-v", "error", "-select_streams", "v:0"])
            .args(["-show_entries", entries])
            .args(["-of", "default=noprint_wrappers=1:nokey=1"])
            .arg(path)
            .output()
            .expect("ffprobe");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// End-to-End: Sendeserver-tauglicher MXF-Master (OP1a). Exportiert XDCAM
    /// HD422 (1080i25) und ProRes-422-HQ-MXF durch den echten App-Pfad und
    /// bestätigt per ffprobe Container, Codec, 4:2:2-Pixelformat, Interlaced-
    /// Feldreihenfolge und — der Kern des Color-Taggings — dass ALLE drei
    /// Color-Tags (Primaries/Transfer/Matrix) den Mux überleben.
    #[test]
    fn end_to_end_mxf_broadcast_master_tags_color() {
        let dir = std::env::temp_dir().join(format!("editron-export-mxf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("quelle.mp4");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "testsrc2=duration=0.5:size=1920x1080:rate=25"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=0.5"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-shortest"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(gen.success());

        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video), track("a1", TrackKind::Audio)],
            vec![
                clip("v", "v1", TrackKind::Video, "VID", 0.0, 0.5),
                clip("a", "a1", TrackKind::Audio, "VID", 0.0, 0.5),
            ],
            vec![video_asset("VID", &src.to_string_lossy())],
        );

        // (a) XDCAM HD422 1080i25 (interlaced, oberes Feld zuerst).
        let mut xdcam = default_video("xdcamhd422", 1920, 1080, 25.0);
        xdcam.scan = ScanMode::InterlacedTff;
        // (b) ProRes 422 HQ in MXF (progressiv).
        let mut prores = default_video("prores", 1920, 1080, 25.0);
        prores.profile = 3;
        // (c) DNxHR HQ in MXF mit interlaced ANGEFORDERT — der dnxhd-Encoder
        // lehnt interlaced DNxHR ab; muss auf progressiv geklemmt werden und
        // DARF NICHT abstürzen (Regressionsschutz).
        let mut dnxhr = default_video("dnxhr", 1920, 1080, 25.0);
        dnxhr.profile = 2;
        dnxhr.scan = ScanMode::InterlacedTff;
        let cases: Vec<(&str, VideoSettings)> =
            vec![("xdcam.mxf", xdcam), ("prores.mxf", prores), ("dnxhr.mxf", dnxhr)];

        for (name, video) in cases {
            let out = dir.join(name);
            let settings = ExportSettings {
                container: container("mxf"),
                video: Some(video),
                audio: Some(default_audio("pcm24", None)),
                loudness: None,
                use_in_out: false,
                audio_stems: false,
                subtitles: SubtitleMode::None,
                image_start: 1,
                output: out.to_string_lossy().into_owned(),
            };
            let plan = build_render_plan(&tl, &media, &settings, &NoNests);
            let (tx, rx) = std::sync::mpsc::channel();
            run_export_worker(
                format!("mxf-{name}"),
                plan,
                settings.clone(),
                tx,
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(Vec::new())),
            );
            let mut ok = false;
            let mut error = None;
            while let Ok(ev) = rx.try_recv() {
                if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                    ok = o;
                    error = e;
                }
            }
            assert!(ok, "{name}: {error:?}");

            // Container ist MXF (OP1a).
            let fmt = ffprobe_field(&out, "format=format_name");
            assert!(fmt.contains("mxf"), "{name}: Container {fmt}");

            let info = crate::services::probe_media(&out.to_string_lossy()).expect(name);
            let v0 = &info.video[0];
            // Tonspur ist PCM (Broadcast-Norm).
            assert_eq!(info.audio[0].codec, "pcm_s24le", "{name}: PCM-Audio");
            // ALLE drei Color-Tags müssen den Mux überleben (Kern des Tickets).
            assert_eq!(v0.color_primaries.as_deref(), Some("bt709"), "{name}: Primaries");
            assert_eq!(v0.color_transfer.as_deref(), Some("bt709"), "{name}: Transfer");
            assert_eq!(v0.color_space.as_deref(), Some("bt709"), "{name}: Matrix");

            match name {
                "xdcam.mxf" => {
                    assert_eq!(v0.codec, "mpeg2video", "{name}: Codec");
                    assert_eq!(v0.pix_fmt.as_deref(), Some("yuv422p"), "{name}: 4:2:2");
                    // Interlaced, oberes Feld zuerst.
                    let fo = ffprobe_field(&out, "stream=field_order");
                    assert_eq!(fo, "tt", "{name}: Feldreihenfolge {fo}");
                }
                "prores.mxf" => {
                    assert_eq!(v0.codec, "prores", "{name}: Codec");
                }
                "dnxhr.mxf" => {
                    assert_eq!(v0.codec, "dnxhd", "{name}: Codec");
                    // Interlaced wurde auf progressiv geklemmt (dnxhd kann kein
                    // interlaced DNxHR) — nicht „tt"/„bb".
                    let fo = ffprobe_field(&out, "stream=field_order");
                    assert!(fo != "tt" && fo != "bb", "{name}: progressiv erwartet, war {fo}");
                }
                _ => unreachable!(),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Einen Frame des Exports als RGB24 dekodieren (Pixel-Verifikation).
    fn decode_frame_rgb(path: &std::path::Path, at: f64, w: usize, h: usize) -> Vec<u8> {
        let out = Command::new(crate::services::ffmpeg_bin())
            .args(["-v", "error", "-ss", &format!("{at:.3}")])
            .args(["-i", &path.to_string_lossy()])
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
            .output()
            .expect("ffmpeg decode");
        assert_eq!(out.stdout.len(), w * h * 3, "Framegröße");
        out.stdout
    }

    fn rgb_at(frame: &[u8], w: usize, x: usize, y: usize) -> [u8; 3] {
        let i = (y * w + x) * 3;
        [frame[i], frame[i + 1], frame[i + 2]]
    }

    /// End-to-End mit Keyframes: ein rotes Bild wandert per Position-X-
    /// Animation von links nach rechts; der Export muss das Compositing
    /// (Skalierung 50 %, animierte Position) pixelgenau wiedergeben.
    #[test]
    fn end_to_end_export_renders_animated_transform() {
        let dir = std::env::temp_dir().join(format!("editron-export-anim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_image = dir.join("rot.png");
        let out = dir.join("anim.mp4");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "color=red:size=320x240", "-frames:v", "1"])
            .arg(&src_image)
            .status()
            .unwrap();
        assert!(gen.success());

        let mut image_asset = video_asset("IMG", &src_image.to_string_lossy());
        image_asset.kind = MediaKind::Image;
        image_asset.info.video.clear();
        image_asset.info.audio.clear();
        let mut c = clip("img", "v1", TrackKind::Video, "IMG", 0.0, 2.0);
        c.src_duration = f64::INFINITY;
        // Skalierung fest 50 %, Position X animiert −25 % → +25 % (linear).
        c.fx.scale_x.value = 50.0;
        c.fx.pos_x.upsert_key(0.0, -25.0);
        c.fx.pos_x.upsert_key(2.0, 25.0);
        let (tl, media) = state_with(vec![track("v1", TrackKind::Video)], vec![c], vec![image_asset]);

        let mut settings = test_settings();
        settings.audio = None;
        settings.output = out.to_string_lossy().into_owned();
        if let Some(v) = settings.video.as_mut() {
            v.width = 640;
            v.height = 360;
            v.speed = 0;
            v.quality = VideoQuality::Crf(16);
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.segments.len(), 1);
        assert!(!plan.segments[0].layers[0].is_identity(), "muss Compositing-Pfad nehmen");

        let (tx, rx) = std::sync::mpsc::channel();
        run_export_worker(
            "anim-job".into(),
            plan,
            settings,
            tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let mut ok = false;
        let mut error = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                ok = o;
                error = e;
            }
        }
        assert!(ok, "Export fehlgeschlagen: {error:?}");

        // Bildgeometrie: 320×240 in 640×360 contain → Basis 480×360; bei
        // 50 % → 240×180, Mittelpunkt cx = 320 + pos_x % · 640.
        // t≈0,1 s: pos_x ≈ −22,5 % → cx ≈ 176; t≈1,9 s: ≈ +22,5 % → cx ≈ 464.
        let (w, h) = (640usize, 360usize);
        let early = decode_frame_rgb(&out, 0.1, w, h);
        let p = rgb_at(&early, w, 176, 180);
        assert!(p[0] > 150 && p[1] < 100 && p[2] < 100, "links muss rot sein: {p:?}");
        let p = rgb_at(&early, w, 560, 180);
        assert!(p[0] < 60 && p[1] < 60 && p[2] < 60, "rechts muss schwarz sein: {p:?}");

        let late = decode_frame_rgb(&out, 1.9, w, h);
        let p = rgb_at(&late, w, 464, 180);
        assert!(p[0] > 150 && p[1] < 100 && p[2] < 100, "rechts muss rot sein: {p:?}");
        let p = rgb_at(&late, w, 80, 180);
        assert!(p[0] < 60 && p[1] < 60 && p[2] < 60, "links muss schwarz sein: {p:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Farbkorrektur muss im CPU-Renderpfad ankommen: Sättigung 0 macht
    /// ein rotes Bild grau, Vignette dunkelt die Inhaltsecken ab.
    #[test]
    fn end_to_end_export_renders_effects() {
        let dir = std::env::temp_dir().join(format!("editron-export-fx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_image = dir.join("rot.png");
        let out = dir.join("fx.mp4");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "color=red:size=320x240", "-frames:v", "1"])
            .arg(&src_image)
            .status()
            .unwrap();
        assert!(gen.success());

        let mut image_asset = video_asset("IMG", &src_image.to_string_lossy());
        image_asset.kind = MediaKind::Image;
        image_asset.info.video[0].width = 320;
        image_asset.info.video[0].height = 240;
        image_asset.info.audio.clear();
        let mut c = clip("img", "v1", TrackKind::Video, "IMG", 0.0, 2.0);
        c.src_duration = f64::INFINITY;
        // Negativ: Rot → Cyan, animiert hier nicht — deterministisch prüfbar.
        c.effects.push(crate::core::effects::EffectInstance::new(
            crate::core::effects::EffectKind::Invert,
        ));
        // Zuschneiden: linke 25 % transparent → schwarzer Hintergrund.
        let mut crop = crate::core::effects::EffectInstance::new(
            crate::core::effects::EffectKind::Crop,
        );
        crop.params[0] = AnimatedParam::fixed(25.0);
        c.effects.push(crop);
        let (tl, media) = state_with(vec![track("v1", TrackKind::Video)], vec![c], vec![image_asset]);

        let mut settings = test_settings();
        settings.audio = None;
        settings.output = out.to_string_lossy().into_owned();
        if let Some(v) = settings.video.as_mut() {
            v.width = 640;
            v.height = 360;
            v.speed = 0;
            v.quality = VideoQuality::Crf(16);
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert!(
            !plan.segments[0].layers[0].is_identity(),
            "aktive Effekte müssen den Compositing-Pfad erzwingen"
        );

        let (tx, rx) = std::sync::mpsc::channel();
        run_export_worker(
            "fx-job".into(),
            plan,
            settings,
            tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let mut ok = false;
        let mut error = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                ok = o;
                error = e;
            }
        }
        assert!(ok, "Export fehlgeschlagen: {error:?}");

        // 320×240 in 640×360 contain → Inhalt 480×360, x 80…560.
        let (w, h) = (640usize, 360usize);
        let frame = decode_frame_rgb(&out, 0.5, w, h);
        // Mitte: invertiertes Rot = Cyan.
        let p = rgb_at(&frame, w, 320, 180);
        assert!(
            p[0] < 60 && p[1] > 200 && p[2] > 200,
            "Mitte muss Cyan sein: {p:?}"
        );
        // Linke 25 % des Inhalts (x 80…200) sind weggeschnitten → Schwarz.
        let cropped = rgb_at(&frame, w, 120, 180);
        assert!(
            cropped[0] < 30 && cropped[1] < 30 && cropped[2] < 30,
            "Crop-Bereich muss schwarz sein: {cropped:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn end_to_end_export_renders_color_grade() {
        let dir = std::env::temp_dir().join(format!("editron-export-grade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_image = dir.join("rot.png");
        let out = dir.join("grade.mp4");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "color=red:size=320x240", "-frames:v", "1"])
            .arg(&src_image)
            .status()
            .unwrap();
        assert!(gen.success());

        let mut image_asset = video_asset("IMG", &src_image.to_string_lossy());
        image_asset.kind = MediaKind::Image;
        // Natürliche Maße fürs Vignetten-Content-Rect (320×240).
        image_asset.info.video[0].width = 320;
        image_asset.info.video[0].height = 240;
        image_asset.info.audio.clear();
        let mut c = clip("img", "v1", TrackKind::Video, "IMG", 0.0, 2.0);
        c.src_duration = f64::INFINITY;
        c.grade.saturation = 0.0;
        c.grade.vignette_amount = 100.0;
        let (tl, media) = state_with(vec![track("v1", TrackKind::Video)], vec![c], vec![image_asset]);

        let mut settings = test_settings();
        settings.audio = None;
        settings.output = out.to_string_lossy().into_owned();
        if let Some(v) = settings.video.as_mut() {
            v.width = 640;
            v.height = 360;
            v.speed = 0;
            v.quality = VideoQuality::Crf(16);
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.segments.len(), 1);
        assert!(
            !plan.segments[0].layers[0].is_identity(),
            "aktive Farbkorrektur muss den Compositing-Pfad erzwingen"
        );

        let (tx, rx) = std::sync::mpsc::channel();
        run_export_worker(
            "grade-job".into(),
            plan,
            settings,
            tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let mut ok = false;
        let mut error = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                ok = o;
                error = e;
            }
        }
        assert!(ok, "Export fehlgeschlagen: {error:?}");

        // 320×240 in 640×360 contain → Inhalt 480×360, x 80…560.
        let (w, h) = (640usize, 360usize);
        let frame = decode_frame_rgb(&out, 0.5, w, h);
        // Mitte: entsättigtes Rot = Grau (Gamma-Luma von Rot ≈ 0,21 → ~54).
        let p = rgb_at(&frame, w, 320, 180);
        assert!(
            (p[0] as i32 - p[1] as i32).abs() <= 8 && (p[1] as i32 - p[2] as i32).abs() <= 8,
            "Mitte muss grau sein: {p:?}"
        );
        assert!(p[0] >= 30 && p[0] <= 90, "Graupegel plausibel: {p:?}");
        // Inhaltsecke: Vignette dunkelt deutlich ab.
        let corner = rgb_at(&frame, w, 100, 20);
        assert!(
            corner[0] < p[0].saturating_sub(20),
            "Ecke muss dunkler sein: {corner:?} vs Mitte {p:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Einen Frame des Exports als 16-Bit-RGBA (`rgba64le`) dekodieren — für
    /// die Bittiefe-Verifikation (>256 Helligkeitsstufen ⇒ echt >8 Bit).
    fn decode_frame_rgba64le(path: &std::path::Path, at: f64, w: usize, h: usize) -> Vec<u16> {
        let out = Command::new(crate::services::ffmpeg_bin())
            .args(["-v", "error", "-ss", &format!("{at:.3}")])
            .args(["-i", &path.to_string_lossy()])
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba64le", "pipe:1"])
            .output()
            .expect("ffmpeg decode rgba64le");
        assert_eq!(out.stdout.len(), w * h * 8, "Framegröße rgba64le");
        out.stdout
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect()
    }

    /// Distinct 16-Bit-R-Werte über das ganze Frame (Banding-Maß).
    fn distinct_r16(px: &[u16]) -> usize {
        px.chunks_exact(4)
            .map(|p| p[0])
            .collect::<std::collections::HashSet<_>>()
            .len()
    }

    /// STUFE-1-BEWEIS (Kernversprechen des Ziels): 10-Bit-Quellmaterial mit
    /// Grade überlebt end-to-end bis zur 10-Bit-Ausgabe mit >256 Helligkeits-
    /// stufen — in einer 8-Bit-Pipeline physikalisch unmöglich (≤256). Prüft
    /// zugleich die ffprobe-Bittiefe-Erkennung und den 16-Bit-Decode-Pfad.
    #[test]
    fn end_to_end_export_preserves_10bit_gradient() {
        let dir = std::env::temp_dir().join(format!("editron-export-10bit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("ramp10.mov");
        let out = dir.join("out10.mov");
        let (w, h) = (1024usize, 64usize);

        // Glatter 10-Bit-Graustufenverlauf (ProRes 4444, yuv444p10le) als Quelle.
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", &format!("gradients=s={w}x{h}:c0=black:c1=white:x0=0:y0=0:x1={}:y1=0:duration=1:rate=25", w - 1)])
            .args(["-frames:v", "1"])
            .args(["-c:v", "prores_ks", "-profile:v", "4", "-pix_fmt", "yuv444p10le"])
            .arg(&src)
            .status()
            .expect("ffmpeg nicht startbar");
        assert!(gen.success(), "10-Bit-Quelle konnte nicht erzeugt werden");

        // Bittiefe-Erkennung (ffprobe) — muss 10 Bit melden.
        let info = crate::services::probe_media(&src.to_string_lossy()).expect("probe");
        assert!(info.video[0].bit_depth >= 10, "Quelle als 10-Bit erkannt: {}", info.video[0].bit_depth);

        // Quelle selbst hat deutlich >256 Helligkeitsstufen (sonst ist der
        // Verlauf zu grob und der Test nichtssagend).
        let src_levels = distinct_r16(&decode_frame_rgba64le(&src, 0.0, w, h));
        assert!(src_levels > 256, "10-Bit-Quelle hat >256 Stufen: {src_levels}");

        // Asset mit echten Probe-Infos (bit_depth=10 ⇒ 16-Bit-Decode-Pfad).
        let mut asset = video_asset("VID", &src.to_string_lossy());
        asset.info = info;
        asset.info.audio.clear();

        // Clip MIT Grade ⇒ Compositing-Pfad (rgba64le-Decode → f32 → 10-Bit-Out).
        let mut c = clip("v", "v1", TrackKind::Video, "VID", 0.0, 1.0);
        c.src_duration = f64::INFINITY;
        c.grade.contrast = 12.0; // sanft, erhält den Verlauf
        let (tl, media) = state_with(vec![track("v1", TrackKind::Video)], vec![c], vec![asset]);
        assert!(
            media.asset("VID").unwrap().info.video[0].bit_depth >= 10,
            "Asset trägt 10-Bit-Info"
        );

        // ProRes 4444 (yuv444p10le) als Ziel — volle Luma-Auflösung, kein
        // Chroma-Subsampling, das den Verlauf glätten würde.
        let mut settings = test_settings();
        settings.audio = None;
        settings.container = container("mov");
        let mut prores = default_video("prores", w as u32, h as u32, 25.0);
        prores.profile = 4; // 4444
        settings.video = Some(prores);
        settings.output = out.to_string_lossy().into_owned();

        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert!(
            !plan.segments[0].layers[0].is_identity(),
            "Grade erzwingt den Compositing-Pfad (16-Bit-Decode)"
        );
        assert!(plan.segments[0].layers[0].src_bit_depth >= 10, "Plan trägt >8-Bit-Quelle");

        let (tx, rx) = std::sync::mpsc::channel();
        run_export_worker(
            "10bit-job".into(),
            plan,
            settings,
            tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let mut ok = false;
        let mut error = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                ok = o;
                error = e;
            }
        }
        assert!(ok, "10-Bit-Export fehlgeschlagen: {error:?}");

        let oinfo = crate::services::probe_media(&out.to_string_lossy()).expect("probe out");
        assert!(oinfo.video[0].bit_depth >= 10, "Ausgabe ist 10-Bit: {}", oinfo.video[0].bit_depth);

        // Kernaussage: die Ausgabe trägt >256 Helligkeitsstufen ⇒ die >8-Bit-
        // Quellpräzision hat die gesamte (f32-)Pipeline überlebt. Eine reine
        // 8-Bit-Pipeline könnte hier physikalisch nie über 256 kommen.
        let out_levels = distinct_r16(&decode_frame_rgba64le(&out, 0.0, w, h));
        assert!(
            out_levels > 256,
            "10-Bit-Verlauf überlebt end-to-end (>256 Stufen), war {out_levels}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression: 10-Bit-Material RÜCKWÄRTS exportieren. Der Reverse-Decoder
    /// muss das Pipe-Pixelformat der Quelle (rgba64le bei >8 Bit) verwenden —
    /// früher war `-pix_fmt rgba` hartkodiert, während der Puffer mit src_bpp=8
    /// dimensioniert war. Folge: ffmpeg schreibt 4 B/px, gelesen wird in
    /// 8-B/px-Puffer ⇒ ZWEI aufeinanderfolgende (unterschiedlich helle) Frames
    /// landen in EINEM Puffer → die obere Frame-Hälfte zeigt Frame k, die untere
    /// Frame k+1. Testquelle: pro Frame eine solide Helligkeit (zeitliche Rampe);
    /// ein korrekter Frame ist räumlich UNIFORM, der Bug-Frame zerfällt in zwei
    /// Helligkeitsbänder.
    #[test]
    fn end_to_end_export_reverse_preserves_10bit() {
        let dir = std::env::temp_dir().join(format!("editron-export-rev10-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("ramp10.mov");
        let out = dir.join("rev10.mov");
        let (w, h) = (64usize, 64usize);

        // Pro Frame eine SOLIDE Helligkeit, die über 2 s von schwarz nach weiß
        // rampt (fade auf weiß). 10-Bit ProRes 4444 ⇒ rgba64le-Decode-Pfad.
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", &format!("color=c=white:s={w}x{h}:d=2:r=25")])
            .args(["-vf", "fade=t=in:st=0:d=2"])
            .args(["-c:v", "prores_ks", "-profile:v", "4", "-pix_fmt", "yuv444p10le"])
            .arg(&src)
            .status()
            .expect("ffmpeg nicht startbar");
        assert!(gen.success(), "10-Bit-Quelle konnte nicht erzeugt werden");

        let info = crate::services::probe_media(&src.to_string_lossy()).expect("probe");
        assert!(info.video[0].bit_depth >= 10, "Quelle als 10-Bit erkannt");
        let mut asset = video_asset("VID", &src.to_string_lossy());
        asset.info = info;
        asset.info.audio.clear();

        // Rückwärts laufender Clip (media_step < 0 ⇒ ReverseDecode-Pfad).
        let mut c = clip("v", "v1", TrackKind::Video, "VID", 0.0, 1.0);
        c.src_in = 1.5;
        c.src_duration = 2.0;
        c.reverse = true;
        let (tl, media) = state_with(vec![track("v1", TrackKind::Video)], vec![c], vec![asset]);

        let mut settings = test_settings();
        settings.audio = None; // Rückwärts ist stumm
        settings.container = container("mov");
        let mut prores = default_video("prores", w as u32, h as u32, 25.0);
        prores.profile = 4; // 4444, yuv444p10le
        prores.speed = 0;
        settings.video = Some(prores);
        settings.output = out.to_string_lossy().into_owned();

        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert!(plan.segments[0].layers[0].src_bit_depth >= 10, "Plan trägt >8-Bit-Quelle");
        assert!(plan.segments[0].layers[0].media_step < 0.0, "Reverse-Layer (media_step < 0)");

        let (tx, rx) = std::sync::mpsc::channel();
        run_export_worker(
            "rev10-job".into(),
            plan,
            settings,
            tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let mut ok = false;
        let mut error = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                ok = o;
                error = e;
            }
        }
        assert!(ok, "10-Bit-Reverse-Export fehlgeschlagen: {error:?}");

        let oinfo = crate::services::probe_media(&out.to_string_lossy()).expect("probe out");
        assert!(oinfo.video[0].bit_depth >= 10, "Ausgabe ist 10-Bit");

        // Jeder korrekte Frame ist räumlich uniform (solide Helligkeit). Der alte
        // Bug zerlegt ihn in zwei Bänder (obere Hälfte = Frame k, untere = k+1,
        // die sich um einen Rampen-Schritt unterscheiden). Wir messen die
        // R16-Differenz zwischen oberer und unterer Bildhälfte.
        let buf = decode_frame_rgba64le(&out, 0.4, w, h);
        // buf: 4 u16 je Pixel (R,G,B,A); R liegt bei Index (y*w+x)*4.
        let r16 = |x: usize, y: usize| -> i64 { buf[(y * w + x) * 4] as i64 };
        let top = r16(w / 2, 1);
        let bottom = r16(w / 2, h - 2);
        let band_diff = (top - bottom).abs();
        // Ein Rampen-Schritt = 1/50 Vollskala ≈ 1310 in 16 Bit; uniform ≈ wenige.
        assert!(
            band_diff < 600,
            "Reverse-Frame muss räumlich uniform sein (oben {top} vs unten {bottom}, Δ={band_diff}) — Band-Split = Pipe-Format-Bug"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// STUFE-4-BEWEIS: HDR-Quellmaterial (BT.2020 + PQ) wird über die ganze
    /// Kette (ffprobe → Plan → Encoder-Args) ehrlich erkannt und getaggt —
    /// nicht mehr stumm nach BT.709 fehlgetaggt. Generiert eine real
    /// PQ-getaggte Quelle und prüft Erkennung + resultierende ffmpeg-Flags.
    #[test]
    fn detects_and_tags_bt2020_pq_source() {
        let dir = std::env::temp_dir().join(format!("editron-pq-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("pq.mp4");
        // BT.2020 + PQ, 10-Bit (libx265 main10, korrekt im Bitstream getaggt).
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "gradients=s=64x64:c0=black:c1=white:duration=1:rate=25"])
            .args(["-frames:v", "2", "-c:v", "libx265", "-preset", "ultrafast", "-pix_fmt", "yuv420p10le"])
            .args(["-x265-params", "log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:range=limited"])
            .args(["-tag:v", "hvc1"])
            .arg(&src)
            .status();
        // Ohne libx265 still überspringen (kein Hard-Fail in eingeschränkten Umgebungen).
        let Ok(st) = gen else { eprintln!("libx265 fehlt — PQ-Test übersprungen"); return };
        if !st.success() {
            eprintln!("libx265 nicht verfügbar — PQ-Test übersprungen");
            return;
        }

        // ffprobe erkennt PQ/BT.2020 + 10 Bit.
        let info = crate::services::probe_media(&src.to_string_lossy()).expect("probe");
        let v0 = &info.video[0];
        assert_eq!(v0.color_transfer.as_deref(), Some("smpte2084"), "PQ erkannt");
        assert_eq!(v0.color_primaries.as_deref(), Some("bt2020"), "BT.2020 erkannt");
        assert!(v0.bit_depth >= 10, "10-Bit erkannt: {}", v0.bit_depth);
        assert_eq!(OutputColor::from_stream(v0), OutputColor::Bt2020Pq);

        // Plan trägt den erkannten Farbraum durch.
        let mut asset = video_asset("VID", &src.to_string_lossy());
        asset.info = info;
        asset.info.audio.clear();
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("v", "v1", TrackKind::Video, "VID", 0.0, 1.0)],
            vec![asset],
        );
        let mut settings = test_settings();
        settings.audio = None;
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.color, OutputColor::Bt2020Pq, "Plan reicht PQ durch");

        // Encoder-Args: ehrliche PQ/BT.2020-Tags statt hart bt709.
        let v = settings.video.as_ref().unwrap();
        let args = video_codec_args(v, settings.container, plan.color).join(" ");
        assert!(args.contains("-color_trc smpte2084"), "PQ-Tag im Export: {args}");
        assert!(args.contains("-color_primaries bt2020"), "BT.2020-Tag im Export: {args}");
        assert!(args.contains("out_color_matrix=bt2020nc"), "BT.2020-Matrix: {args}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// STUFE-5-MESSUNG: Export-Performance 8-Bit-Schnellpfad vs. 8-Bit-Float-
    /// Compositing vs. 10-Bit-Float-Compositing (16-Bit-Pipe). Misst Frames/s
    /// und belegt, dass (a) der 8-Bit-Schnellpfad erhalten und am schnellsten
    /// ist, (b) der 16-Bit-Pfad bewusst Bandbreite kostet. `#[ignore]` —
    /// Messung, kein Pass/Fail: `cargo test export_perf -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn export_perf_8bit_vs_16bit() {
        use std::time::Instant;
        let dir = std::env::temp_dir().join(format!("editron-perf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4");
        let (w, h, secs) = (1280u32, 720u32, 4.0f64);
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", &format!("testsrc=duration={secs}:size={w}x{h}:rate=25")])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
            .arg(&src)
            .status()
            .expect("ffmpeg");
        assert!(gen.success());

        // Eine Konfiguration real exportieren und Frames/s zurückgeben.
        let run = |label: &str, with_grade: bool, codec: &str, profile: usize| -> f64 {
            let mut c = clip("v", "v1", TrackKind::Video, "VID", 0.0, secs);
            if with_grade {
                c.grade.contrast = 30.0;
                c.grade.saturation = 120.0;
            }
            let (tl, media) = state_with(
                vec![track("v1", TrackKind::Video)],
                vec![c],
                vec![video_asset("VID", &src.to_string_lossy())],
            );
            let mut settings = test_settings();
            settings.audio = None;
            settings.container = container(if codec == "prores" { "mov" } else { "mp4" });
            let mut v = default_video(codec, w, h, 25.0);
            v.profile = profile;
            v.speed = 0; // ultrafast für x264
            settings.video = Some(v);
            let out = dir.join(format!("{label}.{}", if codec == "prores" { "mov" } else { "mp4" }));
            settings.output = out.to_string_lossy().into_owned();
            let plan = build_render_plan(&tl, &media, &settings, &NoNests);
            let frames = plan.total_frames;
            let fast = plan.segments.iter().all(|s| s.layers.len() == 1 && s.layers[0].is_identity());
            let t0 = Instant::now();
            let (tx, rx) = std::sync::mpsc::channel();
            run_export_worker(
                label.into(),
                plan,
                settings,
                tx,
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(Vec::new())),
            );
            let mut ok = false;
            while let Ok(ev) = rx.try_recv() {
                if let ServiceEvent::SequenceExportDone { ok: o, .. } = ev {
                    ok = o;
                }
            }
            assert!(ok, "{label} fehlgeschlagen");
            let secs_elapsed = t0.elapsed().as_secs_f64();
            let fps = frames as f64 / secs_elapsed;
            println!(
                "PERF {label}: {frames} Frames in {secs_elapsed:.2}s = {fps:.1} fps  (Pfad: {})",
                if fast { "8-Bit-Schnellpfad (ffmpeg-direkt)" } else { "Float-Compositing" }
            );
            fps
        };

        let fast8 = run("schnellpfad_h264_8bit", false, "h264", 0);
        let comp8 = run("composited_h264_8bit", true, "h264", 0);
        let comp10 = run("composited_prores_10bit", true, "prores", 3);
        println!(
            "PERF Zusammenfassung @ {w}x{h}: Schnellpfad={fast8:.1} fps, \
             Float-8Bit={comp8:.1} fps, Float-10Bit(16-Bit-Pipe)={comp10:.1} fps"
        );
        // Der 8-Bit-Schnellpfad bleibt erhalten und ist nicht langsamer als der
        // Float-Compositing-Pfad (Sanity, keine harte Schwelle).
        assert!(fast8 > 0.0 && comp8 > 0.0 && comp10 > 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-End-Übergang: Rot → Blau per Überblendung. Mitten im Fenster
    /// muss der Export beide Decoder mischen (≈ 50 % Rot + 50 % Blau); vor
    /// und nach dem Fenster liegen die reinen Farben an.
    #[test]
    fn end_to_end_export_renders_cross_dissolve() {
        let dir = std::env::temp_dir().join(format!("editron-export-trans-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let red = dir.join("rot.png");
        let blue = dir.join("blau.png");
        let out = dir.join("dissolve.mp4");
        for (path, color) in [(&red, "red"), (&blue, "blue")] {
            let gen = Command::new(crate::services::ffmpeg_bin())
                .args(["-y", "-v", "error"])
                .args(["-f", "lavfi", "-i", &format!("color={color}:size=640x360"), "-frames:v", "1"])
                .arg(path)
                .status()
                .unwrap();
            assert!(gen.success());
        }

        let mk_asset = |id: &str, path: &std::path::Path| {
            let mut a = video_asset(id, &path.to_string_lossy());
            a.kind = MediaKind::Image;
            a.info.video[0].width = 640;
            a.info.video[0].height = 360;
            a.info.audio.clear();
            a
        };
        let mk_clip = |id: &str, asset: &str, start: f64| {
            let mut c = clip(id, "v1", TrackKind::Video, asset, start, 2.0);
            c.src_duration = f64::INFINITY;
            c
        };
        let (mut tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![mk_clip("r", "RED", 0.0), mk_clip("b", "BLUE", 2.0)],
            vec![mk_asset("RED", &red), mk_asset("BLUE", &blue)],
        );
        tl.add_transition(
            crate::core::transitions::TransitionKind::CrossDissolve,
            "r",
            crate::core::timeline::TrimEdge::End,
            1.0,
        )
        .unwrap();

        let mut settings = test_settings();
        settings.audio = None;
        settings.output = out.to_string_lossy().into_owned();
        if let Some(v) = settings.video.as_mut() {
            v.width = 640;
            v.height = 360;
            v.speed = 0;
            v.quality = VideoQuality::Crf(16);
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.total_frames, 100);

        let (tx, rx) = std::sync::mpsc::channel();
        run_export_worker(
            "dissolve-job".into(),
            plan,
            settings,
            tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let mut ok = false;
        let mut error = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                ok = o;
                error = e;
            }
        }
        assert!(ok, "Export fehlgeschlagen: {error:?}");

        let (w, h) = (640usize, 360usize);
        // Vor dem Fenster (t = 1,0): reines Rot.
        let early = rgb_at(&decode_frame_rgb(&out, 1.0, w, h), w, 320, 180);
        assert!(early[0] > 200 && early[2] < 60, "vorher rot: {early:?}");
        // Mitte des Fensters (t = 2,0; p = 0,5): halb Rot, halb Blau.
        let mid = rgb_at(&decode_frame_rgb(&out, 2.0, w, h), w, 320, 180);
        assert!(
            (mid[0] as i32 - 127).abs() < 28 && (mid[2] as i32 - 127).abs() < 28 && mid[1] < 50,
            "Mitte muss Mischfarbe sein: {mid:?}"
        );
        // Nach dem Fenster (t = 3,2): reines Blau.
        let late = rgb_at(&decode_frame_rgb(&out, 3.2, w, h), w, 320, 180);
        assert!(late[2] > 200 && late[0] < 60, "nachher blau: {late:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Crossfade-Hüllkurven landen im Mix: ein lineares Ausblenden über die
    /// volle Cliplänge macht das Ende praktisch stumm.
    #[test]
    fn audio_mix_applies_transition_fades() {
        let dir = std::env::temp_dir().join(format!("editron-export-xfade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("ton.wav");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
            .args(["-c:a", "pcm_s16le"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(gen.success());

        let plan = RenderPlan {
            duration: 2.0,
            audio: vec![AudioClipPlan {
                path: src.to_string_lossy().into_owned(),
                start_in_mix: 0.0,
                duration: 2.0,
                src_in: 0.0,
                speed: 1.0,
                gain_l: 1.0,
                gain_r: 1.0,
                volume: AnimatedParam::fixed(0.0),
                effects: Vec::new(),
                fades: vec![PlanAudioFade {
                    t0: 0.0,
                    t1: 2.0,
                    fade_in: false,
                    equal_power: false,
                }],
            }],
            ..Default::default()
        };
        let audio = default_audio("pcm32f", None);
        let wav = dir.join("mix.wav");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut progress = Progress::new(&tx, "xfade-job");
        let children = Arc::new(Mutex::new(Vec::new()));
        let mut registry = ChildRegistry::new(children);
        mix_audio_to_wav(
            &plan,
            &audio,
            &wav,
            &AtomicBool::new(false),
            &mut registry,
            &mut progress,
        )
        .expect("mix");

        let bytes = std::fs::read(&wav).unwrap();
        let data = &bytes[58..];
        let rms = |range: std::ops::Range<usize>| -> f32 {
            let mut sum = 0f64;
            let mut n = 0usize;
            for i in range {
                let off = i * 4;
                let v = f32::from_le_bytes([
                    data[off],
                    data[off + 1],
                    data[off + 2],
                    data[off + 3],
                ]);
                sum += (v as f64) * (v as f64);
                n += 1;
            }
            ((sum / n as f64).sqrt()) as f32
        };
        let total = data.len() / 4;
        let head = rms(0..total / 10);
        let tail = rms(total - total / 20..total);
        assert!(head > 0.02, "Anfang hörbar: {head}");
        assert!(tail < head * 0.1, "Ende ausgeblendet: {head} → {tail}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Lautstärke-Keyframes (0 dB → −60 dB) müssen den Mix hörbar ausblenden.
    #[test]
    fn audio_mix_applies_volume_envelope() {
        let dir = std::env::temp_dir().join(format!("editron-export-vol-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("ton.wav");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
            .args(["-c:a", "pcm_s16le"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(gen.success());

        let mut volume = AnimatedParam::fixed(0.0);
        volume.upsert_key(0.0, 0.0);
        volume.upsert_key(2.0, -60.0);
        let plan = RenderPlan {
            duration: 2.0,
            audio: vec![AudioClipPlan {
                path: src.to_string_lossy().into_owned(),
                start_in_mix: 0.0,
                duration: 2.0,
                src_in: 0.0,
                speed: 1.0,
                gain_l: 1.0,
                gain_r: 1.0,
                volume,
                effects: Vec::new(),
                fades: Vec::new(),
            }],
            ..Default::default()
        };
        let audio = default_audio("pcm32f", None);
        let wav = dir.join("mix.wav");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut progress = Progress::new(&tx, "vol-job");
        let children = Arc::new(Mutex::new(Vec::new()));
        let mut registry = ChildRegistry::new(children);
        mix_audio_to_wav(
            &plan,
            &audio,
            &wav,
            &AtomicBool::new(false),
            &mut registry,
            &mut progress,
        )
        .expect("mix");

        // f32-Samples direkt aus der Zwischendatei lesen (Header 58 Bytes).
        let bytes = std::fs::read(&wav).unwrap();
        let data = &bytes[58..];
        let rms = |range: std::ops::Range<usize>| -> f32 {
            let mut sum = 0f64;
            let mut n = 0usize;
            for i in range {
                let off = i * 4;
                let v = f32::from_le_bytes([
                    data[off],
                    data[off + 1],
                    data[off + 2],
                    data[off + 3],
                ]);
                sum += (v as f64) * (v as f64);
                n += 1;
            }
            ((sum / n as f64).sqrt()) as f32
        };
        let total = data.len() / 4;
        let head = rms(0..total / 10);
        let tail = rms(total - total / 10..total);
        // lavfi-sine liegt bei ≈ −21 dB RMS (plus Upmix-Dämpfung) — wichtig
        // ist das Verhältnis: das Ende muss praktisch ausgeblendet sein.
        assert!(head > 0.02, "Anfang muss hörbar sein: {head}");
        assert!(tail < head * 0.05, "Ende muss ausgeblendet sein: {head} → {tail}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Abbruch räumt auf: kein Ziel, keine .part-Datei, Cancelled-Event.
    #[test]
    fn export_cancel_cleans_up() {
        let dir = std::env::temp_dir().join(format!("editron-export-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("quelle.mp4");
        let out = dir.join("ergebnis.mp4");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "testsrc=duration=8:size=640x360:rate=25"])
            .args(["-c:v", "libx264", "-preset", "ultrafast"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(gen.success());

        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("v", "v1", TrackKind::Video, "VID", 0.0, 8.0)],
            vec![video_asset("VID", &src.to_string_lossy())],
        );
        let mut settings = test_settings();
        settings.audio = None;
        settings.output = out.to_string_lossy().into_owned();
        if let Some(v) = settings.video.as_mut() {
            v.speed = 0;
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(true)); // sofort abgebrochen
        let children = Arc::new(Mutex::new(Vec::new()));
        run_export_worker("cancel-job".into(), plan, settings, tx, cancel, children);

        let mut cancelled = false;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok, cancelled: c, .. } = ev {
                assert!(!ok);
                cancelled = c;
            }
        }
        assert!(cancelled, "Abbruch muss als cancelled gemeldet werden");
        assert!(!out.exists(), "Zieldatei darf bei Abbruch nicht entstehen");
        assert!(!part_path(&out.to_string_lossy()).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_merges_contiguous_segments_of_same_source() {
        // Ein durchgehender Clip auf V1, ein V2-Track ohne Überlappung an
        // anderer Stelle erzeugt Grenzen — Segmente desselben Clips mit
        // nahtloser Medienzeit müssen verschmelzen.
        let (tl, media) = state_with(
            vec![track("v2", TrackKind::Video), track("v1", TrackKind::Video)],
            vec![
                clip("a", "v1", TrackKind::Video, "A", 0.0, 10.0),
                clip("b", "v2", TrackKind::Video, "B", 12.0, 2.0),
            ],
            vec![video_asset("A", "/a.mp4"), video_asset("B", "/b.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        // A (0..10) bleibt EIN Segment, Lücke (10..12) schwarz, dann B.
        assert_eq!(plan.segments.len(), 3, "{:?}", plan.segments);
        assert_eq!(plan.segments[0].frames, 250);
        assert!(plan.segments[1].layers.is_empty());
    }
    #[test]
    fn plan_includes_title_layers_and_forces_compositor() {
        // V2 (oben): Titel 1..3 — V1 (unten): Video 0..4.
        let (mut tl, media) = state_with(
            vec![track("v2", TrackKind::Video), track("v1", TrackKind::Video)],
            vec![clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        let mut title = clip("t", "v2", TrackKind::Video, "", 1.0, 2.0);
        title.src_duration = f64::INFINITY;
        title.title = Some(crate::core::title::TitleSpec::default());
        tl.clips.push(title);

        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert_eq!(plan.segments.len(), 3, "{:?}", plan.segments);
        // Mitte: Video unten, Titel oben (Zeichenreihenfolge).
        let mid = &plan.segments[1].layers;
        assert_eq!(mid.len(), 2);
        assert!(mid[0].title.is_none());
        assert!(mid[1].title.is_some());
        assert!(
            !mid[1].is_identity(),
            "Titel-Layer dürfen nie in den ffmpeg-Schnellpfad"
        );
        // Vor/nach dem Titel: Video allein, Schnellpfad bleibt erhalten.
        assert!(plan.segments[0].layers[0].is_identity());
        assert!(plan.segments[2].layers[0].is_identity());
    }

    #[test]
    fn title_only_timeline_validates_and_renders_video() {
        let (mut tl, media) = state_with(vec![track("v1", TrackKind::Video)], vec![], vec![]);
        let mut title = clip("t", "v1", TrackKind::Video, "", 0.0, 5.0);
        title.src_duration = f64::INFINITY;
        title.title = Some(crate::core::title::TitleSpec::default());
        tl.clips.push(title);

        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert!(plan.has_video_media(), "Titel zählt als Videoinhalt");
        assert_eq!(plan.segments.len(), 1);
        assert_eq!(plan.total_frames, 125);

        let issues = validate(&tl, &media, Some(true), None, &test_settings(), &NoNests);
        assert!(
            !issues.iter().any(|i| i.severity == Severity::Error),
            "Titel-only-Export muss validieren: {issues:?}"
        );
    }

    // -------------------------------------------------- Encoder/Bild-Sequenz

    #[test]
    fn encoder_catalog_has_software_first_and_hardware_backends() {
        // Software ([0]) muss zur Codec-`encoder`-Id passen.
        assert_eq!(encoders_for("h264")[0].id, "libx264");
        assert_eq!(encoders_for("hevc")[0].id, "libx265");
        assert!(!encoders_for("h264")[0].is_hardware());
        // Hardware-Backends vorhanden.
        let ids: Vec<&str> = encoders_for("h264").iter().map(|e| e.id).collect();
        for want in ["h264_nvenc", "h264_qsv", "h264_vaapi", "h264_videotoolbox"] {
            assert!(ids.contains(&want), "fehlt: {want}");
        }
        // Codecs ohne Hardware haben genau ein (Software-)Backend.
        assert_eq!(encoders_for("prores").len(), 1);
        assert_eq!(encoders_for("av1").len(), 1);
    }

    #[test]
    fn available_video_encoders_hides_unlisted_hardware() {
        let mut set = HashSet::new();
        set.insert("libx264".to_string());
        set.insert("h264_nvenc".to_string());
        let shown = available_video_encoders("h264", Some(&set));
        let ids: Vec<&str> = shown.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec!["libx264", "h264_nvenc"]);
        // Ohne bekannte Liste werden alle gezeigt.
        assert_eq!(available_video_encoders("h264", None).len(), encoders_for("h264").len());
    }

    #[test]
    fn video_codec_args_pick_quality_flag_per_encoder() {
        let cont = container("mp4");
        // Software → -crf.
        let mut v = default_video("h264", 1920, 1080, 25.0);
        v.quality = VideoQuality::Crf(20);
        let a = video_codec_args(&v, cont, OutputColor::Bt709);
        assert!(a.windows(2).any(|w| w == ["-crf", "20"]), "{a:?}");

        // NVENC → -cq + -b:v 0, kein -crf.
        v.encoder = encoder_def("h264", "h264_nvenc");
        let a = video_codec_args(&v, cont, OutputColor::Bt709);
        assert!(a.windows(2).any(|w| w == ["-cq", "20"]), "{a:?}");
        assert!(!a.iter().any(|x| x == "-crf"));

        // VAAPI → Render-Device + -qp + hwupload im Filter.
        v.encoder = encoder_def("h264", "h264_vaapi");
        let a = video_codec_args(&v, cont, OutputColor::Bt709);
        assert!(a.iter().any(|x| x == "-vaapi_device"), "{a:?}");
        assert!(a.windows(2).any(|w| w == ["-qp", "20"]), "{a:?}");
        assert!(a.iter().any(|x| x.contains("hwupload")), "{a:?}");

        // Bitrate-Modus wirkt encoder-unabhängig.
        v.encoder = encoder_def("h264", "libx264");
        v.quality = VideoQuality::Bitrate(8000);
        let a = video_codec_args(&v, cont, OutputColor::Bt709);
        assert!(a.windows(2).any(|w| w == ["-b:v", "8000k"]), "{a:?}");
    }

    #[test]
    fn image_sequence_pattern_and_frame_args() {
        assert_eq!(
            image_sequence_pattern("/out/clip.png"),
            "/out/clip_%06d.png"
        );
        let png = frame_export_args("png");
        assert!(png.windows(2).any(|w| w == ["-c:v", "png"]), "{png:?}");
        let jpg = frame_export_args("jpeg");
        assert!(jpg.windows(2).any(|w| w == ["-c:v", "mjpeg"]), "{jpg:?}");
        let tif = frame_export_args("TIFF");
        assert!(tif.windows(2).any(|w| w == ["-c:v", "tiff"]), "{tif:?}");
    }

    #[test]
    fn image_sequence_container_is_video_only() {
        let c = container("png_seq");
        assert!(c.image_sequence);
        assert!(c.video);
        assert!(c.audio_codecs.is_empty());
        assert_eq!(c.video_codecs, &["png"]);
    }

    /// End-to-End Untertitel: Einbrennen erzeugt sichtbar helle Pixel im
    /// unteren Drittel (weiße Schrift, Standardstil), Sidecar schreibt die
    /// SRT-Datei millisekundengenau neben die Zieldatei.
    #[test]
    fn end_to_end_export_burns_and_sidecars_subtitles() {
        let dir = std::env::temp_dir().join(format!("editron-export-subs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Reine Untertitel-Timeline (Segmente über Schwarz) — kein Decoder.
        let mut tl = TimelineStore::default();
        tl.import_subtitle_cues(&[crate::core::subtitle::SrtCue {
            start: 0.0,
            end: 2.0,
            text: "UNTERTITEL TEST".into(),
        }]);
        let media = MediaStore::default();

        let render = |mode: SubtitleMode, out: &std::path::Path| {
            let mut settings = test_settings();
            settings.audio = None;
            settings.subtitles = mode;
            settings.output = out.to_string_lossy().into_owned();
            if let Some(v) = settings.video.as_mut() {
                v.width = 640;
                v.height = 360;
                v.speed = 0;
                v.quality = VideoQuality::Crf(16);
            }
            let plan = build_render_plan(&tl, &media, &settings, &NoNests);
            let (tx, rx) = std::sync::mpsc::channel();
            run_export_worker(
                "subs-job".into(),
                plan,
                settings,
                tx,
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(Vec::new())),
            );
            let mut ok = false;
            let mut error = None;
            while let Ok(ev) = rx.try_recv() {
                if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                    ok = o;
                    error = e;
                }
            }
            assert!(ok, "Export fehlgeschlagen ({mode:?}): {error:?}");
        };

        // ---- Einbrennen: Pixel-Verifikation ----
        let burn_out = dir.join("burn.mp4");
        render(SubtitleMode::BurnIn, &burn_out);
        let (w, h) = (640usize, 360usize);
        let frame = decode_frame_rgb(&burn_out, 1.0, w, h);
        // Unteres Drittel (Blockmitte bei +38 % ≈ Zeile 317): helle Pixel.
        let band_max = (280..350)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| rgb_at(&frame, w, x, y)[0])
            .max()
            .unwrap();
        assert!(band_max > 180, "weiße Schrift im unteren Drittel: max {band_max}");
        // Oberes Drittel bleibt schwarz.
        let top_max = (0..100)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| rgb_at(&frame, w, x, y)[0])
            .max()
            .unwrap();
        assert!(top_max < 60, "oben muss schwarz bleiben: max {top_max}");

        // ---- Sidecar: SRT neben der Zieldatei, ms-genau ----
        let side_out = dir.join("side.mp4");
        render(SubtitleMode::Sidecar, &side_out);
        let srt_path = sidecar_srt_path(&side_out.to_string_lossy(), "U1", true);
        let raw = std::fs::read_to_string(&srt_path).expect("Sidecar-SRT fehlt");
        let cues = crate::core::subtitle::parse_srt(&raw).expect("Sidecar parsebar");
        assert_eq!(cues.len(), 1);
        assert!((cues[0].start - 0.0).abs() < 0.001);
        assert!((cues[0].end - 2.0).abs() < 0.001);
        assert_eq!(cues[0].text, "UNTERTITEL TEST");
        // Sidecar brennt nicht ein: unteres Drittel bleibt schwarz.
        let frame = decode_frame_rgb(&side_out, 1.0, w, h);
        let band_max = (280..350)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| rgb_at(&frame, w, x, y)[0])
            .max()
            .unwrap();
        assert!(band_max < 60, "Sidecar darf nicht einbrennen: max {band_max}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-End Untertitel einbetten: MKV erhält einen SubRip-Stream
    /// (zweispurig → zwei Streams mit Spurnamen als Titel-Metadatum).
    #[test]
    fn end_to_end_export_embeds_subtitle_streams() {
        let dir = std::env::temp_dir().join(format!("editron-export-mux-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("mux.mkv");

        let mut tl = TimelineStore::default();
        tl.import_subtitle_cues(&[crate::core::subtitle::SrtCue {
            start: 0.0,
            end: 2.0,
            text: "Erste Sprache".into(),
        }]);
        tl.import_subtitle_cues(&[crate::core::subtitle::SrtCue {
            start: 0.5,
            end: 1.5,
            text: "Zweite Sprache".into(),
        }]);
        let media = MediaStore::default();

        let mut settings = ExportSettings {
            container: container("mkv"),
            video: Some(default_video("h264", 640, 360, 25.0)),
            audio: None,
            loudness: None,
            use_in_out: false,
            audio_stems: false,
            subtitles: SubtitleMode::Embed,
            image_start: 1,
            output: out.to_string_lossy().into_owned(),
        };
        if let Some(v) = settings.video.as_mut() {
            v.speed = 0;
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.subtitle_tracks.len(), 2);

        let (tx, rx) = std::sync::mpsc::channel();
        run_export_worker(
            "mux-job".into(),
            plan,
            settings,
            tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let mut ok = false;
        let mut error = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                ok = o;
                error = e;
            }
        }
        assert!(ok, "Export fehlgeschlagen: {error:?}");

        // ffprobe: genau zwei SubRip-Streams mit den Spurnamen U1/U2.
        let probe = Command::new(crate::services::ffprobe_bin())
            .args(["-v", "error", "-select_streams", "s"])
            .args(["-show_entries", "stream=codec_name:stream_tags=title"])
            .args(["-of", "csv=p=0", &out.to_string_lossy()])
            .output()
            .expect("ffprobe");
        let report = String::from_utf8_lossy(&probe.stdout);
        let lines: Vec<&str> = report.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 2, "zwei Untertitel-Streams erwartet: {report}");
        assert!(lines.iter().all(|l| l.starts_with("subrip")), "{report}");
        assert!(report.contains("U1") && report.contains("U2"), "{report}");

        let _ = std::fs::remove_dir_all(&dir);
    }
