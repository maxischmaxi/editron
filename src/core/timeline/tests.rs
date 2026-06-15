    use super::*;

    fn test_clip(track_id: &str, kind: TrackKind, start: f64, duration: f64) -> TimelineClip {
        TimelineClip {
            id: new_id(),
            track_id: track_id.into(),
            asset_id: "asset".into(),
            name: "Clip".into(),
            kind,
            start,
            duration,
            src_in: 0.0,
            src_duration: f64::INFINITY,
            link_id: None,
            enabled: true,
            gain_db: 0.0,
            fx: ClipFx::default(),
            grade: ColorGrade::default(),
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

    fn track_ids(store: &TimelineStore, kind: TrackKind) -> Vec<String> {
        store
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .map(|t| t.id.clone())
            .collect()
    }

    fn placed_clip(track_id: &str, kind: TrackKind, start: f64, duration: f64, src_in: f64) -> TimelineClip {
        let mut c = test_clip(track_id, kind, start, duration);
        c.src_in = src_in;
        c.src_duration = 1000.0;
        c
    }

    /// Clips einer Spur nach Startzeit sortiert.
    fn clips_on(store: &TimelineStore, track_id: &str) -> Vec<TimelineClip> {
        let mut v: Vec<TimelineClip> = store
            .clips
            .iter()
            .filter(|c| c.track_id == track_id)
            .cloned()
            .collect();
        v.sort_by(|a, b| a.start.total_cmp(&b.start));
        v
    }

    #[test]
    fn ripple_delete_closes_per_track_gap_without_touching_unrelated_tracks() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let a2 = track_ids(&store, TrackKind::Audio)[1].clone();
        // V1: zwei Clips; A2 (kein Sync-Lock): ein Clip hinter der Lücke.
        store.clips.push(placed_clip(&v1, TrackKind::Video, 0.0, 10.0, 0.0));
        store.clips.push(placed_clip(&v1, TrackKind::Video, 20.0, 10.0, 0.0));
        store.clips.push(placed_clip(&a2, TrackKind::Audio, 20.0, 10.0, 0.0));
        let first_id = clips_on(&store, &v1)[0].id.clone();
        store.delete_clips(std::slice::from_ref(&first_id), true);
        // V1-Folgeclip rückt um die 10-s-Lücke nach links …
        let on_v1 = clips_on(&store, &v1);
        assert_eq!(on_v1.len(), 1);
        assert!((on_v1[0].start - 10.0).abs() < 1e-6, "V1 schließt die Lücke");
        // … aber der unbeteiligte A2-Clip bleibt (früher fälschlich mitverschoben).
        let on_a2 = clips_on(&store, &a2);
        assert_eq!(on_a2.len(), 1);
        assert!((on_a2[0].start - 20.0).abs() < 1e-6, "A2 wird NICHT mitgezogen");
    }

    #[test]
    fn ripple_delete_handles_non_contiguous_selection() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        store.clips.push(placed_clip(&v1, TrackKind::Video, 0.0, 5.0, 0.0));
        store.clips.push(placed_clip(&v1, TrackKind::Video, 10.0, 5.0, 0.0));
        store.clips.push(placed_clip(&v1, TrackKind::Video, 20.0, 5.0, 0.0));
        let on = clips_on(&store, &v1);
        let ids = vec![on[0].id.clone(), on[2].id.clone()];
        store.delete_clips(&ids, true);
        // Der mittlere Clip schließt nur die 5-s-Lücke davor → [5,10), kein 25-s-Sprung.
        let on = clips_on(&store, &v1);
        assert_eq!(on.len(), 1);
        assert!((on[0].start - 5.0).abs() < 1e-6, "schließt nur die Lücke davor");
    }

    #[test]
    fn commit_edit_insert_splits_clip_at_insert_point() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        store.clips.push(placed_clip(&v1, TrackKind::Video, 0.0, 10.0, 0.0));
        // 4-s-Clip bei t=5 einfügen (Ripple).
        let new = placed_clip(&v1, TrackKind::Video, 5.0, 4.0, 0.0);
        store.commit_edit(vec![new], 5.0, 4.0, true);
        let on_v1 = clips_on(&store, &v1);
        assert_eq!(on_v1.len(), 3, "Split + eingefügter Clip");
        // Linke Hälfte [0,5), neuer Clip [5,9), rechte Hälfte verschoben [9,14).
        assert!((on_v1[0].start - 0.0).abs() < 1e-6 && (on_v1[0].duration - 5.0).abs() < 1e-6);
        assert!((on_v1[1].start - 5.0).abs() < 1e-6 && (on_v1[1].duration - 4.0).abs() < 1e-6);
        assert!((on_v1[2].start - 9.0).abs() < 1e-6 && (on_v1[2].duration - 5.0).abs() < 1e-6);
        // Rechte Hälfte trägt den Medien-Versatz weiter (src_in = 5).
        assert!((on_v1[2].src_in - 5.0).abs() < 1e-6);
    }

    #[test]
    fn commit_edit_overwrite_replaces_range() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        store.clips.push(placed_clip(&v1, TrackKind::Video, 0.0, 10.0, 0.0));
        let new = placed_clip(&v1, TrackKind::Video, 5.0, 4.0, 0.0);
        store.commit_edit(vec![new], 5.0, 4.0, false);
        let on_v1 = clips_on(&store, &v1);
        assert_eq!(on_v1.len(), 3);
        // [0,5) bleibt, [5,9) ist neu, [9,10) Rest — keine Verschiebung.
        assert!((on_v1[0].duration - 5.0).abs() < 1e-6);
        assert!((on_v1[1].start - 5.0).abs() < 1e-6 && (on_v1[1].duration - 4.0).abs() < 1e-6);
        assert!((on_v1[2].start - 9.0).abs() < 1e-6 && (on_v1[2].duration - 1.0).abs() < 1e-6);
    }

    #[test]
    fn insert_ripples_sync_locked_track_only() {
        // V1 + A1 je ein 10-s-Clip; A1 mit Sync-Lock, A2 ohne.
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let audios = track_ids(&store, TrackKind::Audio);
        let (a1, a2) = (audios[0].clone(), audios[1].clone());
        store.clips.push(placed_clip(&v1, TrackKind::Video, 0.0, 10.0, 0.0));
        store.clips.push(placed_clip(&a1, TrackKind::Audio, 0.0, 10.0, 0.0));
        store.clips.push(placed_clip(&a2, TrackKind::Audio, 0.0, 10.0, 0.0));
        // A1 sync-locked, A2 nicht.
        store.toggle_track_flag(&a1, TrackFlag::SyncLock);

        let new = placed_clip(&v1, TrackKind::Video, 2.0, 4.0, 0.0);
        store.commit_edit(vec![new], 2.0, 4.0, true);

        // A1 rippelt mit: Split bei 2 → [0,2) + [6,14) (Lücke [2,6)).
        let on_a1 = clips_on(&store, &a1);
        assert_eq!(on_a1.len(), 2, "Sync-Lock-Spur wird geteilt");
        assert!((on_a1[0].duration - 2.0).abs() < 1e-6);
        assert!((on_a1[1].start - 6.0).abs() < 1e-6);
        // A2 bleibt unangetastet (kein Sync-Lock).
        let on_a2 = clips_on(&store, &a2);
        assert_eq!(on_a2.len(), 1);
        assert!((on_a2[0].start - 0.0).abs() < 1e-6 && (on_a2[0].duration - 10.0).abs() < 1e-6);
    }

    #[test]
    fn lift_clears_targeted_range_and_keeps_gap() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        store.clips.push(placed_clip(&v1, TrackKind::Video, 0.0, 10.0, 0.0));
        store.set_in_out_range(3.0, 7.0);
        assert!(store.lift_range());
        let on_v1 = clips_on(&store, &v1);
        assert_eq!(on_v1.len(), 2, "Lücke bleibt stehen");
        assert!((on_v1[0].duration - 3.0).abs() < 1e-6);
        assert!((on_v1[1].start - 7.0).abs() < 1e-6 && (on_v1[1].duration - 3.0).abs() < 1e-6);
    }

    #[test]
    fn extract_removes_range_and_ripples_left() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        store.clips.push(placed_clip(&v1, TrackKind::Video, 0.0, 10.0, 0.0));
        store.set_in_out_range(3.0, 7.0);
        assert!(store.extract_range());
        let on_v1 = clips_on(&store, &v1);
        assert_eq!(on_v1.len(), 2);
        // [0,3) bleibt, [7,10) rückt auf [3,6).
        assert!((on_v1[0].duration - 3.0).abs() < 1e-6);
        assert!((on_v1[1].start - 3.0).abs() < 1e-6 && (on_v1[1].duration - 3.0).abs() < 1e-6);
        assert!((store.playhead_sec - 3.0).abs() < 1e-6);
    }

    #[test]
    fn extract_sync_lock_keeps_other_track_in_sync() {
        // V1 (targeted) + A1 (nur sync-locked, NICHT targeted).
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        // A1 von Targeting befreien, dafür Sync-Lock.
        if let Some(t) = store.tracks.iter_mut().find(|t| t.id == a1) {
            t.targeted = false;
            t.sync_lock = true;
        }
        store.clips.push(placed_clip(&v1, TrackKind::Video, 0.0, 10.0, 0.0));
        store.clips.push(placed_clip(&a1, TrackKind::Audio, 0.0, 10.0, 0.0));
        store.set_in_out_range(3.0, 7.0);
        assert!(store.extract_range());
        // Beide Spuren verlieren [3,7) und schließen die Lücke.
        for tid in [&v1, &a1] {
            let on = clips_on(&store, tid);
            assert_eq!(on.len(), 2, "Spur {tid} extrahiert + rippelt");
            assert!((on[0].duration - 3.0).abs() < 1e-6);
            assert!((on[1].start - 3.0).abs() < 1e-6 && (on[1].duration - 3.0).abs() < 1e-6);
        }
    }

    #[test]
    fn source_patch_is_radio_per_kind() {
        let mut store = TimelineStore::default();
        let videos = track_ids(&store, TrackKind::Video);
        // Standard: V1 (idx 1) gepatcht.
        assert_eq!(store.source_patch_track(TrackKind::Video), Some(videos[1].as_str()));
        // Auf V2 (idx 0) umschalten → V1 verliert den Patch.
        store.toggle_source_patch(&videos[0]);
        assert_eq!(store.source_patch_track(TrackKind::Video), Some(videos[0].as_str()));
        assert!(!store.tracks.iter().find(|t| t.id == videos[1]).unwrap().source_patched);
        // Erneut V2 klicken → Patch aus (kein Ziel mehr).
        store.toggle_source_patch(&videos[0]);
        assert_eq!(store.source_patch_track(TrackKind::Video), None);
    }

    #[test]
    fn ensure_patch_target_defaults_picks_v1_a1() {
        let mut store = TimelineStore::default();
        // Alle Flags löschen (Altprojekt-Zustand).
        for t in &mut store.tracks {
            t.source_patched = false;
            t.targeted = false;
        }
        store.ensure_patch_target_defaults();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        assert_eq!(store.source_patch_track(TrackKind::Video), Some(v1.as_str()));
        assert_eq!(store.source_patch_track(TrackKind::Audio), Some(a1.as_str()));
        assert!(store.tracks.iter().find(|t| t.id == v1).unwrap().targeted);
        assert!(store.tracks.iter().find(|t| t.id == a1).unwrap().targeted);
    }

    #[test]
    fn match_frame_source_maps_media_time_on_targeted_track() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        // Clip bei [4,14) mit src_in=2 → bei Sequenzzeit 6 ist Medienzeit 4.
        store.clips.push(placed_clip(&v1, TrackKind::Video, 4.0, 10.0, 2.0));
        let (asset, media_t) = store.match_frame_source(6.0).expect("Clip am Playhead");
        assert_eq!(asset, "asset");
        assert!((media_t - 4.0).abs() < 1e-6);
    }

    #[test]
    fn commit_edit_is_single_undo_step() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        store.clips.push(placed_clip(&v1, TrackKind::Video, 0.0, 10.0, 0.0));
        let before = store.clips.len();
        let new = placed_clip(&v1, TrackKind::Video, 5.0, 4.0, 0.0);
        store.commit_edit(vec![new], 5.0, 4.0, true);
        assert!(store.can_undo());
        store.undo();
        assert_eq!(store.clips.len(), before, "ein Undo stellt den Ausgangszustand her");
        assert_eq!(clips_on(&store, &v1).len(), 1);
    }

    #[test]
    fn paste_keeps_original_track_when_free() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        store.clipboard = vec![test_clip(&v1, TrackKind::Video, 0.0, 4.0)];
        store.playhead_sec = 10.0;
        store.paste(None);
        assert_eq!(store.clips.len(), 1);
        assert_eq!(store.clips[0].track_id, v1);
        assert_eq!(store.clips[0].start, 10.0);
        assert_eq!(store.tracks.len(), 4);
    }

    #[test]
    fn paste_evades_to_track_above_instead_of_overwriting() {
        let mut store = TimelineStore::default();
        let videos = track_ids(&store, TrackKind::Video);
        let existing = test_clip(&videos[1], TrackKind::Video, 0.0, 8.0);
        let existing_id = existing.id.clone();
        store.clips.push(existing);
        store.clipboard = vec![test_clip(&videos[1], TrackKind::Video, 0.0, 4.0)];
        store.playhead_sec = 2.0;
        store.paste(None);
        // Bestehender Clip unangetastet, Kopie auf der Spur darüber.
        assert_eq!(store.clips.len(), 2);
        let original = store.clips.iter().find(|c| c.id == existing_id).unwrap();
        assert_eq!(original.duration, 8.0);
        let pasted = store.clips.iter().find(|c| c.id != existing_id).unwrap();
        assert_eq!(pasted.track_id, videos[0]);
        assert_eq!(pasted.start, 2.0);
        assert_eq!(store.tracks.len(), 4);
    }

    #[test]
    fn paste_creates_track_when_all_lanes_occupied() {
        let mut store = TimelineStore::default();
        let videos = track_ids(&store, TrackKind::Video);
        for id in &videos {
            store.clips.push(test_clip(id, TrackKind::Video, 0.0, 8.0));
        }
        store.clipboard = vec![test_clip(&videos[1], TrackKind::Video, 0.0, 4.0)];
        store.playhead_sec = 2.0;
        store.paste(None);
        assert_eq!(store.clips.len(), 3);
        // Neue Videospur entsteht oben (Index 0) und trägt die Kopie.
        assert_eq!(store.tracks.len(), 5);
        assert_eq!(store.tracks[0].kind, TrackKind::Video);
        let pasted = store.clips.last().unwrap();
        assert_eq!(pasted.track_id, store.tracks[0].id);
    }

    #[test]
    fn paste_audio_evades_downwards() {
        let mut store = TimelineStore::default();
        let audios = track_ids(&store, TrackKind::Audio);
        store.clips.push(test_clip(&audios[0], TrackKind::Audio, 0.0, 8.0));
        store.clipboard = vec![test_clip(&audios[0], TrackKind::Audio, 0.0, 4.0)];
        store.playhead_sec = 0.0;
        store.paste(None);
        assert_eq!(store.clips.len(), 2);
        assert_eq!(store.clips.last().unwrap().track_id, audios[1]);
    }

    #[test]
    fn duplicate_clips_keeps_originals_and_remaps_links() {
        let mut store = TimelineStore::default();
        let videos = track_ids(&store, TrackKind::Video);
        let audios = track_ids(&store, TrackKind::Audio);
        let link = new_id();
        let mut v = test_clip(&videos[1], TrackKind::Video, 0.0, 4.0);
        v.link_id = Some(link.clone());
        let mut a = test_clip(&audios[0], TrackKind::Audio, 0.0, 4.0);
        a.link_id = Some(link.clone());
        let ids = vec![v.id.clone(), a.id.clone()];
        store.clips.push(v);
        store.clips.push(a);

        store.duplicate_clips(&ids, 6.0, 0);

        assert_eq!(store.clips.len(), 4);
        // Originale unverändert bei 0 mit alter link_id.
        for id in &ids {
            let c = store.clips.iter().find(|c| &c.id == id).unwrap();
            assert_eq!(c.start, 0.0);
            assert_eq!(c.link_id.as_deref(), Some(link.as_str()));
        }
        // Kopien bei 6, selektiert, mit gemeinsamer neuer link_id.
        let copies: Vec<&TimelineClip> =
            store.clips.iter().filter(|c| !ids.contains(&c.id)).collect();
        assert_eq!(copies.len(), 2);
        for c in &copies {
            assert_eq!(c.start, 6.0);
            assert!(store.selected_clip_ids.contains(&c.id));
        }
        assert_eq!(copies[0].link_id, copies[1].link_id);
        assert_ne!(copies[0].link_id.as_deref(), Some(link.as_str()));
    }

    #[test]
    fn duplicate_without_movement_is_a_noop() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let clip = test_clip(&v1, TrackKind::Video, 0.0, 4.0);
        let id = clip.id.clone();
        store.clips.push(clip);
        store.duplicate_clips(&[id], 0.0, 0);
        assert_eq!(store.clips.len(), 1);
        assert!(!store.can_undo());
    }

    #[test]
    fn grade_update_is_undoable_and_live_writes_share_one_snapshot() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let clip = test_clip(&v1, TrackKind::Video, 0.0, 4.0);
        let id = clip.id.clone();
        store.clips.push(clip);

        // Einzelaktion: eigener Snapshot.
        store.grade_update(&id, |g| g.saturation = 0.0);
        assert_eq!(store.clip(&id).unwrap().grade.saturation, 0.0);
        // Geste: ein Snapshot, viele Live-Writes.
        store.begin_fx_edit();
        store.grade_update_live(&id, |g| g.temperature = 10.0);
        store.grade_update_live(&id, |g| g.temperature = 60.0);
        assert_eq!(store.clip(&id).unwrap().grade.temperature, 60.0);

        store.undo(); // ganze Geste zurück
        assert_eq!(store.clip(&id).unwrap().grade.temperature, 0.0);
        assert_eq!(store.clip(&id).unwrap().grade.saturation, 0.0);
        store.undo(); // Einzelaktion zurück
        assert_eq!(store.clip(&id).unwrap().grade.saturation, 100.0);
        store.redo();
        assert_eq!(store.clip(&id).unwrap().grade.saturation, 0.0);
    }

    #[test]
    fn grade_reset_and_toggle_respect_locked_tracks_and_noops() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let clip = test_clip(&v1, TrackKind::Video, 0.0, 4.0);
        let id = clip.id.clone();
        store.clips.push(clip);

        // Reset ohne Änderungen: kein Undo-Eintrag.
        store.grade_reset(&[id.clone()]);
        assert!(!store.can_undo());

        store.grade_update(&id, |g| g.exposure = 2.0);
        store.grade_toggle_enabled(&id);
        assert!(!store.clip(&id).unwrap().grade.enabled);
        assert!(!store.clip(&id).unwrap().grade.is_active(), "Bypass deaktiviert");

        // Gesperrte Spur: keine Grade-Edits.
        let track_idx = store.tracks.iter().position(|t| t.id == v1).unwrap();
        store.tracks[track_idx].locked = true;
        store.grade_update(&id, |g| g.exposure = 5.0);
        assert_eq!(store.clip(&id).unwrap().grade.exposure, 2.0);
        store.grade_reset(&[id.clone()]);
        assert!(!store.clip(&id).unwrap().grade.is_default());

        store.tracks[track_idx].locked = false;
        store.grade_reset(&[id.clone()]);
        assert!(store.clip(&id).unwrap().grade.is_default());
    }

    // ------------------------------------------------------ Effekt-Stapel

    /// Store mit verknüpftem A/V-Paar; liefert (Store, Video-ID, Audio-ID).
    fn store_with_av_pair() -> (TimelineStore, String, String) {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        let mut video = test_clip(&v1, TrackKind::Video, 0.0, 4.0);
        let mut audio = test_clip(&a1, TrackKind::Audio, 0.0, 4.0);
        let link = new_id();
        video.link_id = Some(link.clone());
        audio.link_id = Some(link);
        let (vid, aid) = (video.id.clone(), audio.id.clone());
        store.clips.push(video);
        store.clips.push(audio);
        (store, vid, aid)
    }

    #[test]
    fn effects_add_routes_by_kind_to_av_partner() {
        let (mut store, vid, aid) = store_with_av_pair();
        // Video-Effekt auf den Audio-Clip → landet beim Video-Partner.
        let target = store.effects_add(&aid, EffectKind::GaussianBlur).unwrap();
        assert_eq!(target, vid);
        assert_eq!(store.clip(&vid).unwrap().effects.len(), 1);
        assert!(store.clip(&aid).unwrap().effects.is_empty());
        // Audio-Effekt auf den Video-Clip → landet beim Audio-Partner.
        let target = store.effects_add(&vid, EffectKind::Reverb).unwrap();
        assert_eq!(target, aid);
        assert_eq!(store.clip(&aid).unwrap().effects.len(), 1);
        // Undo entfernt den letzten Effekt wieder.
        store.undo();
        assert!(store.clip(&aid).unwrap().effects.is_empty());
    }

    #[test]
    fn effects_add_rejects_mismatched_kind_without_partner() {
        let mut store = TimelineStore::default();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        let clip = test_clip(&a1, TrackKind::Audio, 0.0, 4.0);
        let id = clip.id.clone();
        store.clips.push(clip);
        assert!(store.effects_add(&id, EffectKind::GaussianBlur).is_none());
        assert!(store.effects_add(&id, EffectKind::Compressor).is_some());
    }

    #[test]
    fn effects_move_toggle_reset_remove_with_undo() {
        let (mut store, vid, _) = store_with_av_pair();
        store.effects_add(&vid, EffectKind::GaussianBlur);
        store.effects_add(&vid, EffectKind::Invert);
        let fx: Vec<String> = store.clip(&vid).unwrap().effects.iter().map(|e| e.id.clone()).collect();

        // Reorder: Invert nach oben.
        store.effects_move(&vid, &fx[1], -1);
        assert_eq!(store.clip(&vid).unwrap().effects[0].id, fx[1]);
        // Außerhalb der Grenzen: No-op (kein Undo-Eintrag).
        let undo_before = store.revision;
        store.effects_move(&vid, &fx[1], -1);
        assert_eq!(store.revision, undo_before);

        // Bypass.
        store.effects_toggle_enabled(&vid, &fx[0]);
        assert!(!store.clip(&vid).unwrap().effects.iter().find(|e| e.id == fx[0]).unwrap().enabled);

        // Parameter ändern + Reset.
        let pref = ParamRef::Effect { fx_id: fx[0].clone(), index: 0 };
        store.begin_fx_edit();
        store.kf_set_value_live(&vid, &pref, 0.0, 55.0);
        assert_eq!(
            TimelineStore::clip_param(store.clip(&vid).unwrap(), &pref).unwrap().value,
            55.0
        );
        store.effects_reset(&vid, &fx[0]);
        let default = EffectKind::GaussianBlur.specs()[0].default;
        assert_eq!(
            TimelineStore::clip_param(store.clip(&vid).unwrap(), &pref).unwrap().value,
            default
        );

        // Entfernen + Undo.
        store.effects_remove(&vid, &fx[0]);
        assert_eq!(store.clip(&vid).unwrap().effects.len(), 1);
        store.undo();
        assert_eq!(store.clip(&vid).unwrap().effects.len(), 2);
    }

    // ---------------------------------------------- Spur-Effekte/Automation

    #[test]
    fn track_effects_chain_with_undo_and_locking() {
        let mut store = TimelineStore::default();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        // Nur Audio-Effekte auf Audio-Spuren.
        assert!(!store.track_effects_add(&a1, EffectKind::GaussianBlur));
        assert!(store.track_effects_add(&a1, EffectKind::Equalizer));
        assert!(store.track_effects_add(&a1, EffectKind::Limiter));
        let fx: Vec<String> = store
            .tracks
            .iter()
            .find(|t| t.id == a1)
            .unwrap()
            .effects
            .iter()
            .map(|e| e.id.clone())
            .collect();
        assert_eq!(fx.len(), 2);
        // Reorder: Limiter nach oben.
        store.track_effects_move(&a1, &fx[1], -1);
        assert_eq!(
            store.tracks.iter().find(|t| t.id == a1).unwrap().effects[0].id,
            fx[1]
        );
        // Bypass.
        store.track_effects_toggle_enabled(&a1, &fx[0]);
        assert!(!store
            .tracks
            .iter()
            .find(|t| t.id == a1)
            .unwrap()
            .effects
            .iter()
            .find(|e| e.id == fx[0])
            .unwrap()
            .enabled);
        // Entfernen + Undo.
        store.track_effects_remove(&a1, &fx[0]);
        assert_eq!(store.tracks.iter().find(|t| t.id == a1).unwrap().effects.len(), 1);
        store.undo();
        assert_eq!(store.tracks.iter().find(|t| t.id == a1).unwrap().effects.len(), 2);
        // Gesperrte Spur: kein Add.
        let idx = store.tracks.iter().position(|t| t.id == a1).unwrap();
        store.tracks[idx].locked = true;
        assert!(!store.track_effects_add(&a1, EffectKind::Compressor));
    }

    #[test]
    fn track_automation_interpolates_and_is_undoable() {
        let mut store = TimelineStore::default();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        store.track_auto_add_point(&a1, TrackAutoParam::Volume, 0.0, 0.0);
        store.track_auto_add_point(&a1, TrackAutoParam::Volume, 4.0, 6.0);
        store.set_track_gain_db(&a1, -3.0);
        let tr = store.tracks.iter().find(|t| t.id == a1).unwrap();
        assert!(tr.has_automation());
        assert!((tr.gain_db_at(0.0) - (-3.0)).abs() < 1e-9);
        assert!((tr.gain_db_at(2.0)).abs() < 1e-9, "Mitte: -3 + 3 = 0");
        assert!((tr.gain_db_at(4.0) - 3.0).abs() < 1e-9);
        // Pan-Automation.
        store.track_auto_add_point(&a1, TrackAutoParam::Pan, 0.0, 0.0);
        store.track_auto_add_point(&a1, TrackAutoParam::Pan, 2.0, 1.0);
        let tr = store.tracks.iter().find(|t| t.id == a1).unwrap();
        assert!((tr.pan_at(1.0) - 0.5).abs() < 1e-9, "Mitte 0..1 → 0.5");
        // Fader-Offset + Clamp auf [−1, 1].
        store.set_track_pan(&a1, 0.5);
        let tr = store.tracks.iter().find(|t| t.id == a1).unwrap();
        assert!((tr.pan_at(2.0) - 1.0).abs() < 1e-9, "0.5 + 1.0 → clamp 1.0");
        // Undo des letzten Punkt-Adds.
        store.undo();
        assert!(store
            .tracks
            .iter()
            .find(|t| t.id == a1)
            .unwrap()
            .pan_auto
            .key_index_at(2.0)
            .is_none());
    }

    #[test]
    fn track_serde_roundtrip_preserves_fx_and_automation() {
        let mut store = TimelineStore::default();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        store.track_effects_add(&a1, EffectKind::Equalizer);
        store.track_auto_add_point(&a1, TrackAutoParam::Volume, 1.0, 4.0);
        let track = store.tracks.iter().find(|t| t.id == a1).unwrap().clone();
        let json = serde_json::to_string(&track).unwrap();
        let back: TimelineTrack = serde_json::from_str(&json).unwrap();
        assert_eq!(back.effects.len(), 1);
        assert_eq!(back.effects[0].kind, EffectKind::Equalizer);
        assert!(back.volume_auto.is_animated());
        // Standardspur bleibt schlank (keine neuen Felder im JSON).
        let plain = serde_json::to_string(&make_track(TrackKind::Audio)).unwrap();
        assert!(!plain.contains("effects"), "leere FX-Liste nicht serialisiert");
        assert!(!plain.contains("volumeAuto"), "Null-Automation nicht serialisiert");
    }

    #[test]
    fn effects_reorder_moves_to_index_with_undo() {
        let (mut store, vid, _) = store_with_av_pair();
        store.effects_add(&vid, EffectKind::GaussianBlur);
        store.effects_add(&vid, EffectKind::Invert);
        store.effects_add(&vid, EffectKind::Sharpen);
        let ids: Vec<String> = store
            .clip(&vid)
            .unwrap()
            .effects
            .iter()
            .map(|e| e.id.clone())
            .collect();
        // Letzten (Sharpen) an Position 0 ziehen.
        store.effects_reorder(&vid, &ids[2], 0);
        let order: Vec<String> = store
            .clip(&vid)
            .unwrap()
            .effects
            .iter()
            .map(|e| e.id.clone())
            .collect();
        assert_eq!(order, vec![ids[2].clone(), ids[0].clone(), ids[1].clone()]);
        store.undo();
        assert_eq!(store.clip(&vid).unwrap().effects[2].id, ids[2]);
        // No-op an gleiche Position: kein Undo-Eintrag.
        let rev = store.revision;
        store.effects_reorder(&vid, &ids[0], 0);
        assert_eq!(store.revision, rev);
    }

    #[test]
    fn kf_ops_animate_effect_params_like_builtin() {
        let (mut store, vid, _) = store_with_av_pair();
        store.effects_add(&vid, EffectKind::GaussianBlur);
        let fx_id = store.clip(&vid).unwrap().effects[0].id.clone();
        let pref = ParamRef::Effect { fx_id, index: 0 };

        // Stopwatch an → erster Keyframe; Wert setzen → zweiter.
        store.kf_toggle_animated(&vid, &pref, 1.0);
        store.kf_set_value_live(&vid, &pref, 3.0, 80.0);
        let p = TimelineStore::clip_param(store.clip(&vid).unwrap(), &pref).unwrap();
        assert_eq!(p.keyframes.len(), 2);
        assert!((p.eval(2.0) - 45.0).abs() < 1e-9, "linear zwischen 10 und 80");

        // Interpolation setzen.
        store.kf_set_interp(&vid, &[(pref.clone(), 1.0)], Interp::Hold);
        let p = TimelineStore::clip_param(store.clip(&vid).unwrap(), &pref).unwrap();
        assert_eq!(p.keyframes[0].interp, Interp::Hold);

        // Klemmen an die Spec-Grenzen.
        store.kf_set_value_live(&vid, &pref, 3.0, 9999.0);
        let p = TimelineStore::clip_param(store.clip(&vid).unwrap(), &pref).unwrap();
        assert_eq!(p.eval(3.0), 100.0);

        // Keyframes entfernen; Stopwatch aus friert den Wert ein.
        store.kf_remove_keyframes(&vid, &[(pref.clone(), 3.0)]);
        store.kf_toggle_animated(&vid, &pref, 1.0);
        let p = TimelineStore::clip_param(store.clip(&vid).unwrap(), &pref).unwrap();
        assert!(!p.is_animated());

        // Reset auf Spec-Default.
        store.kf_reset_param(&vid, &pref);
        let p = TimelineStore::clip_param(store.clip(&vid).unwrap(), &pref).unwrap();
        assert_eq!(p.value, EffectKind::GaussianBlur.specs()[0].default);
    }

    #[test]
    fn copy_paste_attributes_is_kind_aware_and_remints_ids() {
        let (mut store, vid, aid) = store_with_av_pair();
        // Quelle präparieren: Transform + Grade + Effekte auf beiden Hälften.
        store.begin_fx_edit();
        store.kf_set_value_live(&vid, &ParamRef::Builtin(ParamId::PosX), 0.0, 25.0);
        store.grade_update(&vid, |g| g.saturation = 0.0);
        store.effects_add(&vid, EffectKind::Invert);
        store.effects_add(&aid, EffectKind::Reverb);
        let src_fx_id = store.clip(&vid).unwrap().effects[0].id.clone();

        assert!(store.copy_attributes(&vid));

        // Ziel: zweites A/V-Paar.
        let (vid2, aid2) = {
            let v1 = track_ids(&store, TrackKind::Video)[1].clone();
            let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
            let mut video = test_clip(&v1, TrackKind::Video, 10.0, 4.0);
            let mut audio = test_clip(&a1, TrackKind::Audio, 10.0, 4.0);
            let link = new_id();
            video.link_id = Some(link.clone());
            audio.link_id = Some(link);
            let ids = (video.id.clone(), audio.id.clone());
            store.clips.push(video);
            store.clips.push(audio);
            ids
        };
        store.paste_attributes(&[vid2.clone(), aid2.clone()]);

        let v2 = store.clip(&vid2).unwrap();
        assert_eq!(v2.fx.pos_x.value, 25.0, "Transform übernommen");
        assert_eq!(v2.grade.saturation, 0.0, "Grade übernommen");
        assert_eq!(v2.effects.len(), 1, "nur Video-Effekte auf Video-Clip");
        assert_eq!(v2.effects[0].kind, EffectKind::Invert);
        assert_ne!(v2.effects[0].id, src_fx_id, "frische Instanz-ID");

        let a2 = store.clip(&aid2).unwrap();
        assert_eq!(a2.effects.len(), 1, "Audio-Effekte vom Partner");
        assert_eq!(a2.effects[0].kind, EffectKind::Reverb);

        // Undo macht das Einfügen ungeschehen.
        store.undo();
        assert!(store.clip(&vid2).unwrap().effects.is_empty());
    }

    // -------------------------------------------------------- Übergänge

    /// Store mit zwei benachbarten Video-Clips auf V1 (mit Handles):
    /// A: 0–4 aus Quelle [1..5] von 10 s (Schwanz-Handle 5 s),
    /// B: 4–8 aus Quelle [2..6] von 10 s (Kopf-Handle 2 s).
    fn store_with_cut() -> (TimelineStore, String, String) {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let mut a = test_clip(&v1, TrackKind::Video, 0.0, 4.0);
        a.src_in = 1.0;
        a.src_duration = 10.0;
        let mut b = test_clip(&v1, TrackKind::Video, 4.0, 4.0);
        b.src_in = 2.0;
        b.src_duration = 10.0;
        let (aid, bid) = (a.id.clone(), b.id.clone());
        store.clips.push(a);
        store.clips.push(b);
        (store, aid, bid)
    }

    #[test]
    fn add_transition_clamps_to_handles_with_undo() {
        let (mut store, aid, bid) = store_with_cut();
        // Gewünscht 100 s — zentriert maximal 2 × min(tail_A=5, dur_B=4,
        // head_B=2, dur_A=4) = 4 s.
        let id = store
            .add_transition(TransitionKind::CrossDissolve, &aid, TrimEdge::End, 100.0)
            .expect("Übergang");
        let tr = store.transition(&id).unwrap().clone();
        assert!((tr.duration - 4.0).abs() < 1e-9, "geklemmt: {}", tr.duration);
        assert_eq!(tr.from_clip_id.as_deref(), Some(aid.as_str()));
        assert_eq!(tr.to_clip_id.as_deref(), Some(bid.as_str()));
        assert_eq!(store.transition_window(&tr), Some((2.0, 6.0)));
        assert_eq!(store.selected_transition_ids, vec![id.clone()]);

        store.undo();
        assert!(store.transitions.is_empty());
        store.redo();
        assert_eq!(store.transitions.len(), 1);
    }

    #[test]
    fn add_transition_single_sided_and_kind_checks() {
        let (mut store, aid, _) = store_with_cut();
        // Kante am Clipanfang ohne Vorgänger ⇒ Einblenden (nur to).
        let id = store
            .add_transition(TransitionKind::CrossDissolve, &aid, TrimEdge::Start, 1.0)
            .expect("Fade-In");
        let tr = store.transition(&id).unwrap();
        assert!(tr.from_clip_id.is_none());
        assert_eq!(store.transition_window(tr), Some((0.0, 1.0)));
        // Audio-Übergang auf Video-Clip: abgelehnt.
        assert!(store
            .add_transition(TransitionKind::ConstantPower, &aid, TrimEdge::End, 1.0)
            .is_err());
        // Gesperrte Spur: abgelehnt.
        let track_id = store.clip(&aid).unwrap().track_id.clone();
        store.tracks.iter_mut().find(|t| t.id == track_id).unwrap().locked = true;
        assert!(store
            .add_transition(TransitionKind::Wipe, &aid, TrimEdge::End, 1.0)
            .is_err());
    }

    #[test]
    fn transitions_follow_trims_and_die_with_adjacency() {
        let (mut store, aid, bid) = store_with_cut();
        store
            .add_transition(TransitionKind::CrossDissolve, &aid, TrimEdge::End, 2.0)
            .unwrap();
        // Slip schrumpft den Kopf-Handle von B auf 0,1 s ⇒ zentriert max 0,2 s.
        store.slip_clip(&bid, -1.9);
        let tr = &store.transitions[0];
        assert!((tr.duration - 0.2).abs() < 1e-9, "nachgeklemmt: {}", tr.duration);
        // Clip wegbewegen: Nachbarschaft bricht ⇒ Übergang entfällt.
        store.move_clips(&[bid.clone()], 2.0, 0);
        assert!(store.transitions.is_empty());
        // Undo stellt Übergang UND Lage wieder her.
        store.undo();
        assert_eq!(store.transitions.len(), 1);
    }

    #[test]
    fn ripple_trim_keeps_transition_at_moved_cut() {
        let (mut store, aid, _) = store_with_cut();
        store
            .add_transition(TransitionKind::CrossDissolve, &aid, TrimEdge::End, 2.0)
            .unwrap();
        // Ripple-Trim am Ende von A um −1 s: B rückt nach, Kante bleibt dicht.
        store.ripple_trim_clip(&aid, TrimEdge::End, -1.0);
        assert_eq!(store.transitions.len(), 1, "Übergang überlebt Ripple");
        let tr = store.transitions[0].clone();
        let (w0, w1) = store.transition_window(&tr).unwrap();
        assert!((w0 - 2.0).abs() < 1e-9 && (w1 - 4.0).abs() < 1e-9, "{w0}..{w1}");
    }

    #[test]
    fn split_remaps_end_transition_to_right_half() {
        let (mut store, aid, _) = store_with_cut();
        let id = store
            .add_transition(TransitionKind::Wipe, &aid, TrimEdge::End, 1.0)
            .unwrap();
        store.split_at(2.0, None);
        let tr = store.transition(&id).expect("Übergang überlebt Split");
        let from = tr.from_clip_id.clone().unwrap();
        let right = store.clip(&from).unwrap();
        assert!((right.end() - 4.0).abs() < 1e-9, "hängt an der rechten Hälfte");
        assert_ne!(from, aid);
    }

    #[test]
    fn delete_and_asset_removal_drop_transitions() {
        let (mut store, aid, bid) = store_with_cut();
        store
            .add_transition(TransitionKind::CrossDissolve, &aid, TrimEdge::End, 2.0)
            .unwrap();
        store.delete_clips(&[bid], false);
        assert!(store.transitions.is_empty());
        store.undo();
        assert_eq!(store.transitions.len(), 1);
        store.remove_clips_for_assets(&["asset".to_string()]);
        assert!(store.transitions.is_empty());
    }

    #[test]
    fn copy_paste_and_duplicate_carry_transitions() {
        let (mut store, aid, bid) = store_with_cut();
        store
            .add_transition(TransitionKind::Push, &aid, TrimEdge::End, 2.0)
            .unwrap();
        // Kopieren + Einfügen bei 20 s: Übergang folgt auf frische IDs.
        store.select_clips(&[aid.clone(), bid.clone()], SelectMode::Replace, false);
        store.copy_selection();
        store.playhead_sec = 20.0;
        store.paste(None);
        assert_eq!(store.transitions.len(), 2);
        let pasted = store
            .transitions
            .iter()
            .find(|t| t.from_clip_id.as_deref() != Some(aid.as_str()))
            .unwrap()
            .clone();
        let (w0, _) = store.transition_window(&pasted).unwrap();
        assert!((w0 - 23.0).abs() < 1e-9, "Fenster an der neuen Kante: {w0}");
        // Alt+Drag-Duplizieren nimmt den Übergang ebenfalls mit.
        store.duplicate_clips(&[aid.clone(), bid.clone()], 40.0, 0);
        assert_eq!(store.transitions.len(), 3);
    }

    #[test]
    fn apply_default_transition_covers_selection_edges_once() {
        let (mut store, aid, bid) = store_with_cut();
        store.select_clips(&[bid.clone()], SelectMode::Replace, false);
        let n = store.apply_transition_to_selection(TransitionKind::CrossDissolve);
        // Kante zu A (zweiseitig) + Clipende (Ausblenden).
        assert_eq!(n, 2);
        // Zweiter Aufruf: alle Kanten belegt.
        assert_eq!(store.apply_transition_to_selection(TransitionKind::CrossDissolve), 0);
        // Audio-Standard auf Video-Auswahl: nichts.
        assert_eq!(store.apply_transition_to_selection(TransitionKind::ConstantPower), 0);
        let _ = aid;
    }

    #[test]
    fn transition_ops_duration_alignment_kind() {
        let (mut store, aid, _) = store_with_cut();
        let id = store
            .add_transition(TransitionKind::CrossDissolve, &aid, TrimEdge::End, 2.0)
            .unwrap();
        store.set_transition_duration(&id, 3.0);
        assert!((store.transition(&id).unwrap().duration - 3.0).abs() < 1e-9);
        // EndAtCut: max = min(head_B=2, dur_A=4) = 2 ⇒ Dauer wird nachgeklemmt.
        store.set_transition_alignment(&id, TransitionAlignment::EndAtCut);
        let tr = store.transition(&id).unwrap().clone();
        assert_eq!(tr.alignment, TransitionAlignment::EndAtCut);
        assert!(tr.duration <= 2.0 + 1e-9, "{}", tr.duration);
        assert_eq!(store.transition_window(&tr).unwrap().1, 4.0, "endet am Schnitt");
        // Ersetzen innerhalb derselben Spurart.
        store.set_transition_kind(&id, TransitionKind::Wipe);
        assert_eq!(store.transition(&id).unwrap().kind, TransitionKind::Wipe);
        store.set_transition_kind(&id, TransitionKind::ConstantPower);
        assert_eq!(store.transition(&id).unwrap().kind, TransitionKind::Wipe, "Artwechsel Video→Audio abgelehnt");
    }

    #[test]
    fn audio_fades_extend_audible_range() {
        let mut store = TimelineStore::default();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        let mut a = test_clip(&a1, TrackKind::Audio, 0.0, 4.0);
        a.kind = TrackKind::Audio;
        a.src_in = 1.0;
        a.src_duration = 10.0;
        let mut b = test_clip(&a1, TrackKind::Audio, 4.0, 4.0);
        b.kind = TrackKind::Audio;
        b.src_in = 2.0;
        b.src_duration = 10.0;
        let (aid, bid) = (a.id.clone(), b.id.clone());
        store.clips.push(a);
        store.clips.push(b);
        store
            .add_transition(TransitionKind::ConstantPower, &aid, TrimEdge::End, 2.0)
            .unwrap();
        let a = store.clip(&aid).unwrap().clone();
        let b = store.clip(&bid).unwrap().clone();
        let fades_a = store.audio_fades(&a);
        let fades_b = store.audio_fades(&b);
        assert_eq!(fades_a, vec![(3.0, 5.0, false, true)]);
        assert_eq!(fades_b, vec![(3.0, 5.0, true, true)]);
        assert_eq!(store.audio_extent(&a, &fades_a), (0.0, 5.0));
        assert_eq!(store.audio_extent(&b, &fades_b), (3.0, 8.0));
    }

    #[test]
    fn overlapping_transition_windows_are_rejected() {
        let (mut store, aid, bid) = store_with_cut();
        // Dritter Clip C: 8–12 mit Handles.
        let v1 = store.clip(&aid).unwrap().track_id.clone();
        let mut c = test_clip(&v1, TrackKind::Video, 8.0, 4.0);
        c.src_in = 2.0;
        c.src_duration = 10.0;
        store.clips.push(c);
        // Übergang an Kante A|B (zentriert 4 s: Fenster 2..6).
        store
            .add_transition(TransitionKind::CrossDissolve, &aid, TrimEdge::End, 4.0)
            .unwrap();
        // Kante B|C zentriert 4 s wollen: Fenster 6..10 — kollidiert nicht.
        let id2 = store
            .add_transition(TransitionKind::CrossDissolve, &bid, TrimEdge::End, 4.0)
            .unwrap();
        assert_eq!(store.transitions.len(), 2);
        // Erstes Fenster auf 2..6, zweites 6..10 — beide bleiben.
        let w2 = store
            .transition_window(store.transition(&id2).unwrap())
            .unwrap();
        assert!((w2.0 - 6.0).abs() < 1e-9);
    }

    #[test]
    fn effects_clear_and_bypass_affect_all_selected() {
        let (mut store, vid, aid) = store_with_av_pair();
        store.effects_add(&vid, EffectKind::Invert);
        store.effects_add(&aid, EffectKind::Reverb);

        store.effects_toggle_bypass(&[vid.clone(), aid.clone()]);
        assert!(store.clip(&vid).unwrap().effects.iter().all(|e| !e.enabled));
        assert!(store.clip(&aid).unwrap().effects.iter().all(|e| !e.enabled));
        store.effects_toggle_bypass(&[vid.clone(), aid.clone()]);
        assert!(store.clip(&vid).unwrap().effects.iter().all(|e| e.enabled));

        store.effects_clear(&[vid.clone(), aid.clone()]);
        assert!(store.clip(&vid).unwrap().effects.is_empty());
        assert!(store.clip(&aid).unwrap().effects.is_empty());
        store.undo();
        assert_eq!(store.clip(&vid).unwrap().effects.len(), 1);
    }
    // ------------------------------------------------------------- Titel

    #[test]
    fn add_title_clip_lands_on_free_track_above_content() {
        let mut store = TimelineStore::default();
        let videos = track_ids(&store, TrackKind::Video);
        // Material auf der UNTEREN Videospur (V1) — der Titel muss auf die
        // freie Spur DARÜBER (V2).
        store.clips.push(test_clip(&videos[1], TrackKind::Video, 0.0, 8.0));
        let id = store.add_title_clip(crate::core::title::TitleSpec::default(), 1.0, 4.0);
        let title = store.clip(&id).expect("Titel-Clip");
        assert!(title.is_title());
        assert_eq!(title.kind, TrackKind::Video);
        assert_eq!(title.track_id, videos[0]);
        assert!(title.asset_id.is_empty());
        assert!(title.src_duration.is_infinite(), "frei dehnbar wie Standbilder");
        assert_eq!(store.selected_clip_ids, vec![id.clone()]);

        // Beide Videospuren belegt → neue Spur entsteht oben.
        let id2 = store.add_title_clip(crate::core::title::TitleSpec::default(), 1.0, 4.0);
        assert_eq!(store.tracks.len(), 5);
        assert_eq!(store.clip(&id2).unwrap().track_id, store.tracks[0].id);

        // Undo räumt Titel UND Spur wieder auf.
        store.undo();
        store.undo();
        assert!(store.clips.iter().all(|c| !c.is_title()));
        assert_eq!(store.tracks.len(), 4);
    }

    #[test]
    fn add_title_clip_without_content_uses_bottom_video_track() {
        let mut store = TimelineStore::default();
        let videos = track_ids(&store, TrackKind::Video);
        let id = store.add_title_clip(crate::core::title::TitleSpec::default(), 0.0, 5.0);
        assert_eq!(store.clip(&id).unwrap().track_id, videos[1], "unterste freie Spur");
    }

    #[test]
    fn title_clip_moves_and_trims_like_a_still() {
        let mut store = TimelineStore::default();
        let id = store.add_title_clip(crate::core::title::TitleSpec::default(), 2.0, 5.0);
        store.move_clips(&[id.clone()], 3.0, 0);
        assert_eq!(store.clip(&id).unwrap().start, 5.0);
        // Ende frei dehnbar (unendliche Quelle).
        store.trim_clip(&id, TrimEdge::End, 10.0);
        assert!((store.clip(&id).unwrap().duration - 15.0).abs() < 1e-9);
        store.undo();
        store.undo();
        assert_eq!(store.clip(&id).unwrap().start, 2.0);
    }

    #[test]
    fn title_update_live_syncs_name_and_shares_one_snapshot() {
        let mut store = TimelineStore::default();
        let id = store.add_title_clip(crate::core::title::TitleSpec::default(), 0.0, 5.0);
        store.begin_title_edit();
        store.title_update_live(&id, |s| s.text = "N".into());
        store.title_update_live(&id, |s| s.text = "Nachrichten\nLive".into());
        let clip = store.clip(&id).unwrap();
        assert_eq!(clip.name, "Nachrichten");
        assert_eq!(clip.title.as_ref().unwrap().text, "Nachrichten\nLive");
        // EIN Undo stellt den Zustand vor der Tipp-Sitzung wieder her.
        store.undo();
        assert_eq!(store.clip(&id).unwrap().title.as_ref().unwrap().text, "Titel");
        // title_update (Einzelklick) legt einen eigenen Snapshot an.
        store.title_update(&id, |s| s.stroke_width = 4.0);
        assert_eq!(store.clip(&id).unwrap().title.as_ref().unwrap().stroke_width, 4.0);
        store.undo();
        assert_eq!(store.clip(&id).unwrap().title.as_ref().unwrap().stroke_width, 0.0);
    }

    // ------------------------------------------------------------ Untertitel

    #[test]
    fn subtitle_track_sits_on_top_and_is_named_u() {
        let mut store = TimelineStore::default();
        let u1 = store.add_track(TrackKind::Subtitle);
        let u2 = store.add_track(TrackKind::Subtitle);
        // Neue Untertitelspur ganz oben; U1 bleibt die unterste des Blocks.
        assert_eq!(store.tracks[0].id, u2);
        assert_eq!(store.tracks[1].id, u1);
        assert_eq!(store.tracks[2].kind, TrackKind::Video);
        let name = |id: &str| {
            let t = store.tracks.iter().find(|t| t.id == id).unwrap();
            track_name(t, &store.tracks)
        };
        assert_eq!(name(&u1), "U1");
        assert_eq!(name(&u2), "U2");
        // Aktive Spur folgt der zuletzt angelegten; Fallback nach Entfernen.
        assert_eq!(store.active_subtitle_track().unwrap().id, u2);
        store.remove_track(&u2);
        assert_eq!(store.active_subtitle_track().unwrap().id, u1);
        // Neue Videospur bleibt UNTER dem Untertitel-Block.
        store.add_track(TrackKind::Video);
        assert_eq!(store.tracks[0].id, u1);
        assert_eq!(store.tracks[1].kind, TrackKind::Video);
    }

    #[test]
    fn add_subtitle_clip_snaps_to_frames_and_respects_neighbors() {
        let mut store = TimelineStore::default();
        store.settings = SequenceSettings {
            rate: sequence::FrameRate::new(25, 1),
            ..SequenceSettings::default()
        };
        // Leicht neben dem Frame-Raster → rastet auf 1,0 s (Frame 25) ein.
        let id = store.add_subtitle_clip("Hallo", 1.013).expect("anlegen");
        let clip = store.clip(&id).unwrap().clone();
        assert_eq!(clip.kind, TrackKind::Subtitle);
        assert!((clip.start - 1.0).abs() < 1e-9);
        assert!((clip.duration - crate::core::subtitle::DEFAULT_CUE_DURATION).abs() < 1e-9);
        assert_eq!(clip.subtitle.as_ref().unwrap().text, "Hallo");
        assert_eq!(clip.name, "Hallo");
        assert!(clip.asset_id.is_empty(), "Generator ohne Mediendatei");

        // Belegte Position → Fehler statt Überschreiben.
        assert!(store.add_subtitle_clip("Kollision", 2.0).is_err());
        // Dahinter: Dauer wird bis zum nächsten Segment geklemmt.
        let id2 = store.add_subtitle_clip("Zweites", 6.0).expect("anlegen");
        store.move_clips(&[id2.clone()], 0.0, 0);
        let id3 = store.add_subtitle_clip("Drittes", 4.0).expect("anlegen");
        let c3 = store.clip(&id3).unwrap();
        assert!((c3.end() - 6.0).abs() < 1e-9, "endet am nächsten Segment");
        // Alles auf einer Spur gelandet.
        let subs = store.subtitle_tracks();
        assert_eq!(subs.len(), 1);
    }

    #[test]
    fn subtitle_segments_split_and_trim_like_clips() {
        let mut store = TimelineStore::default();
        let id = store.add_subtitle_clip("Erste Zeile\nZweite", 0.0).expect("anlegen");
        store.split_at(1.5, None);
        let track_id = store.clip(&id).map(|c| c.track_id.clone()).unwrap();
        let parts: Vec<&TimelineClip> = store
            .clips
            .iter()
            .filter(|c| c.track_id == track_id)
            .collect();
        assert_eq!(parts.len(), 2);
        // Beide Hälften tragen den Text weiter.
        assert!(parts
            .iter()
            .all(|c| c.subtitle.as_ref().unwrap().text == "Erste Zeile\nZweite"));
        // Undo stellt das ungeteilte Segment wieder her.
        store.undo();
        assert_eq!(
            store
                .clips
                .iter()
                .filter(|c| c.track_id == track_id)
                .count(),
            1
        );
    }

    #[test]
    fn import_subtitle_cues_is_frame_accurate_and_resolves_overlaps() {
        use crate::core::subtitle::SrtCue;
        let mut store = TimelineStore::default();
        // NTSC: 29,97 — die kritische Rate für Frame-Genauigkeit.
        store.settings = SequenceSettings {
            rate: sequence::FrameRate::new(30000, 1001),
            ..SequenceSettings::default()
        };
        let rate = store.settings.rate;
        let cues = vec![
            SrtCue { start: 0.5, end: 2.0, text: "Eins".into() },
            // Überlappt den ersten Cue → Start wird ans Ende geklemmt.
            SrtCue { start: 1.8, end: 3.0, text: "Zwei".into() },
            // Kürzer als ein Frame → auf einen Frame verlängert.
            SrtCue { start: 10.0, end: 10.001, text: "Mini".into() },
            // Leerer Text fliegt raus.
            SrtCue { start: 20.0, end: 21.0, text: "   ".into() },
        ];
        let (track_id, n) = store.import_subtitle_cues(&cues);
        assert_eq!(n, 3);
        let mut clips: Vec<&TimelineClip> = store
            .clips
            .iter()
            .filter(|c| c.track_id == track_id)
            .collect();
        clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        // Alle Kanten liegen exakt auf dem NTSC-Frame-Raster.
        for c in &clips {
            let f0 = rate.frame_round(c.start);
            let f1 = rate.frame_round(c.end());
            assert!((c.start - rate.time_of_frame(f0 as f64)).abs() < 1e-9);
            assert!((c.end() - rate.time_of_frame(f1 as f64)).abs() < 1e-9);
        }
        // Überlappung aufgelöst: Cue 2 beginnt am Ende von Cue 1.
        assert!((clips[1].start - clips[0].end()).abs() < 1e-9);
        // Mini-Cue: exakt ein Frame.
        assert_eq!(
            rate.frame_round(clips[2].end()) - rate.frame_round(clips[2].start),
            1
        );
        // EIN Undo entfernt den kompletten Import (Spur + Segmente).
        store.undo();
        assert!(store.subtitle_tracks().is_empty());
    }

    #[test]
    fn subtitle_cues_roundtrip_through_srt_is_frame_exact() {
        use crate::core::subtitle::{format_srt, parse_srt};
        let mut store = TimelineStore::default();
        store.settings = SequenceSettings {
            rate: sequence::FrameRate::new(30000, 1001),
            ..SequenceSettings::default()
        };
        let rate = store.settings.rate;
        // Segmente auf krummen NTSC-Frame-Zeiten (Frame 30 ≈ 1,001 s …).
        let cues_in = vec![
            crate::core::subtitle::SrtCue {
                start: rate.time_of_frame(30.0),
                end: rate.time_of_frame(90.0),
                text: "Erster Satz".into(),
            },
            crate::core::subtitle::SrtCue {
                start: rate.time_of_frame(120.0),
                end: rate.time_of_frame(200.0),
                text: "Zweiter Satz\nmit Umbruch".into(),
            },
        ];
        let (track_id, _) = store.import_subtitle_cues(&cues_in);
        let exported = store.subtitle_cues(&track_id);
        let parsed = parse_srt(&format_srt(&exported)).expect("SRT-Roundtrip");
        // SRT speichert Millisekunden — die Rück-Rundung auf das Frame-
        // Raster muss exakt die ursprünglichen Frames liefern.
        for (orig, back) in cues_in.iter().zip(parsed.iter()) {
            assert_eq!(rate.frame_round(back.start), rate.frame_round(orig.start));
            assert_eq!(rate.frame_round(back.end), rate.frame_round(orig.end));
            assert_eq!(back.text, orig.text);
        }
    }

    #[test]
    fn subtitle_style_and_text_updates_are_undoable() {
        let mut store = TimelineStore::default();
        let id = store.add_subtitle_clip("Anfang", 0.0).expect("anlegen");
        let track_id = store.clip(&id).unwrap().track_id.clone();

        // Spurstil: Einzelklick-Update mit eigenem Snapshot.
        store.subtitle_style_update(&track_id, |s| s.size = 64.0);
        assert_eq!(store.subtitle_style(&track_id).size, 64.0);
        store.undo();
        assert_eq!(store.subtitle_style(&track_id).size, 48.0);
        store.redo();

        // Tipp-Sitzung: EIN Snapshot für mehrere Live-Updates.
        store.begin_subtitle_edit();
        store.subtitle_update_live(&id, |s| s.text = "N".into());
        store.subtitle_update_live(&id, |s| s.text = "Neuer Text".into());
        let clip = store.clip(&id).unwrap();
        assert_eq!(clip.subtitle.as_ref().unwrap().text, "Neuer Text");
        assert_eq!(clip.name, "Neuer Text");
        store.undo();
        assert_eq!(store.clip(&id).unwrap().subtitle.as_ref().unwrap().text, "Anfang");
    }

    #[test]
    fn hidden_subtitle_tracks_are_excluded_from_program_layers() {
        use crate::core::compose;
        let mut store = TimelineStore::default();
        let id = store.add_subtitle_clip("Sichtbar", 0.0).expect("anlegen");
        let track_id = store.clip(&id).unwrap().track_id.clone();
        let layer_ids = |store: &TimelineStore| -> Vec<String> {
            compose::visible_program_layers(store, 1.0)
                .iter()
                .filter_map(|l| match l {
                    compose::ProgramLayer::Clip { clip, .. } => Some(clip.id.clone()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(layer_ids(&store), vec![id.clone()]);
        // Auge zu (muted = ausgeblendet) → Layer verschwindet.
        store.toggle_track_flag(&track_id, TrackFlag::Muted);
        assert!(layer_ids(&store).is_empty());
        // Video-Solo blendet Untertitel NICHT aus.
        store.toggle_track_flag(&track_id, TrackFlag::Muted);
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let video = test_clip(&v1, TrackKind::Video, 0.0, 4.0);
        let vid = video.id.clone();
        store.clips.push(video);
        let v_track = store.tracks.iter().find(|t| t.id == v1).unwrap().id.clone();
        store.toggle_track_flag(&v_track, TrackFlag::Solo);
        let ids = layer_ids(&store);
        assert!(ids.contains(&vid));
        assert!(ids.contains(&id), "Untertitel bleiben bei Video-Solo sichtbar");
        // Untertitel-Layer liegt ÜBER dem Video-Layer.
        assert_eq!(ids.last(), Some(&id));
    }

    #[test]
    fn subtitle_layer_spec_follows_track_style() {
        use crate::core::compose;
        let mut store = TimelineStore::default();
        let id = store.add_subtitle_clip("Hallo Welt", 0.0).expect("anlegen");
        let track_id = store.clip(&id).unwrap().track_id.clone();
        let spec = compose::layer_title_spec(&store, store.clip(&id).unwrap()).unwrap();
        assert_eq!(spec.text, "Hallo Welt");
        assert_eq!(spec.size, 48.0);
        let before = spec.content_hash();
        store.subtitle_style_update(&track_id, |s| s.size = 72.0);
        let spec = compose::layer_title_spec(&store, store.clip(&id).unwrap()).unwrap();
        assert_eq!(spec.size, 72.0);
        assert_ne!(spec.content_hash(), before, "Stiländerung invalidiert den Raster-Cache");
    }

    // ------------------------------------------------ Geschwindigkeit/Dauer

    /// Clip mit Geschwindigkeit: 0–4 s Timeline aus Quelle [2..(2+4·speed)].
    fn speed_clip(track_id: &str, speed: f64, reverse: bool) -> TimelineClip {
        let mut c = test_clip(track_id, TrackKind::Video, 0.0, 4.0);
        c.src_in = 2.0;
        c.src_duration = 30.0;
        c.speed = speed;
        c.reverse = reverse;
        c
    }

    #[test]
    fn media_time_mapping_roundtrips_for_speed_reverse_freeze() {
        let mut c = speed_clip("t", 0.37, false);
        // Vorwärts: media = src_in + (t−start)·speed; Umkehrung exakt.
        for t in [0.0, 0.5, 1.7, 4.0] {
            let m = c.media_time_at(t);
            assert!((m - (2.0 + t * 0.37)).abs() < 1e-12);
            assert!((c.seq_time_of_media(m) - t).abs() < 1e-9);
        }
        // Rückwärts: am Clipanfang liegt der Medien-Out, am Ende der In.
        c.reverse = true;
        assert!((c.media_time_at(0.0) - c.media_out()).abs() < 1e-12);
        assert!((c.media_time_at(4.0) - c.src_in).abs() < 1e-12);
        for t in [0.0, 1.3, 4.0] {
            assert!((c.seq_time_of_media(c.media_time_at(t)) - t).abs() < 1e-9);
        }
        // Standbild: Medienzeit konstant am In-Punkt, Spanne 0.
        c.freeze = true;
        assert_eq!(c.media_time_at(0.0), 2.0);
        assert_eq!(c.media_time_at(3.0), 2.0);
        assert_eq!(c.media_span(), 0.0);
    }

    #[test]
    fn split_preserves_media_continuity_at_any_speed() {
        for (speed, reverse) in [(0.37, false), (2.5, false), (0.5, true), (1.0, true)] {
            let mut store = TimelineStore::default();
            let v1 = track_ids(&store, TrackKind::Video)[1].clone();
            let clip = speed_clip(&v1, speed, reverse);
            let span = clip.media_span();
            store.clips.push(clip);
            store.split_at(1.5, None);
            let mut parts: Vec<&TimelineClip> = store.clips.iter().collect();
            parts.sort_by(|a, b| a.start.total_cmp(&b.start));
            assert_eq!(parts.len(), 2, "speed={speed} reverse={reverse}");
            let (l, r) = (parts[0], parts[1]);
            // Medienspanne bleibt in Summe erhalten …
            assert!(
                (l.media_span() + r.media_span() - span).abs() < 1e-9,
                "Spanne speed={speed} reverse={reverse}"
            );
            // … und die Abbildung ist an der Schnittkante stetig.
            assert!(
                (l.media_time_at(1.5) - r.media_time_at(1.5)).abs() < 1e-9,
                "Kante speed={speed} reverse={reverse}: {} vs {}",
                l.media_time_at(1.5),
                r.media_time_at(1.5)
            );
        }
    }

    #[test]
    fn trim_handles_scale_with_speed() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        // 2× Tempo: Kopf-Handle 2 s Medien = 1 s Timeline.
        let clip = speed_clip(&v1, 2.0, false);
        let id = clip.id.clone();
        store.clips.push(clip);
        store.move_clips(&[id.clone()], 2.0, 0); // Platz vor dem Clip schaffen
        let (lo, _) = trim_range(store.clip(&id).unwrap(), TrimEdge::Start, &store.clips, false);
        assert!((lo + 1.0).abs() < 1e-9, "Kopf-Handle in Timeline-Sekunden: {lo}");
        // Kopf 0,5 s verlängern: src_in sinkt um 0,5·2 = 1 s Medien.
        store.trim_clip(&id, TrimEdge::Start, -0.5);
        let c = store.clip(&id).unwrap();
        assert!((c.src_in - 1.0).abs() < 1e-9, "src_in: {}", c.src_in);
        assert!((c.duration - 4.5).abs() < 1e-9);
        // Ende: Schwanz-Handle (30 − media_out)/speed.
        let (_, hi) = trim_range(c, TrimEdge::End, &store.clips, false);
        assert!((hi - (30.0 - c.media_out()) / 2.0).abs() < 1e-9);
    }

    #[test]
    fn reverse_end_trim_extends_into_earlier_media() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let clip = speed_clip(&v1, 1.0, true); // Quelle [2..6] rückwärts
        let id = clip.id.clone();
        store.clips.push(clip);
        // Ende +1 s: spielt FRÜHERES Material — src_in sinkt auf 1.
        store.trim_clip(&id, TrimEdge::End, 1.0);
        let c = store.clip(&id).unwrap();
        assert!((c.src_in - 1.0).abs() < 1e-9, "src_in: {}", c.src_in);
        assert!((c.media_out() - 6.0).abs() < 1e-9, "Medien-Out bleibt verankert");
        // Schwanz-Handle rückwärts = src_in/speed ⇒ maximal noch 1 s.
        let (_, hi) = trim_range(c, TrimEdge::End, &store.clips, false);
        assert!((hi - 1.0).abs() < 1e-9, "{hi}");
    }

    #[test]
    fn slip_scales_with_speed() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let clip = speed_clip(&v1, 2.0, false); // Quelle [2..10] von 30
        let id = clip.id.clone();
        store.clips.push(clip);
        store.slip_clip(&id, 1.0); // 1 s Timeline = 2 s Medien
        assert!((store.clip(&id).unwrap().src_in - 4.0).abs() < 1e-9);
        // Klemmen: maximal (30 − media_out)/speed nach rechts.
        store.slip_clip(&id, 100.0);
        let c = store.clip(&id).unwrap();
        assert!((c.media_out() - 30.0).abs() < 1e-9, "an der Quelle geklemmt");
    }

    #[test]
    fn set_clip_speed_couples_duration_and_ripples() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        // Verknüpftes A/V-Paar 0–4 + Folgeclip bei 4 auf V1 und A1.
        let link = new_id();
        let mut av = test_clip(&v1, TrackKind::Video, 0.0, 4.0);
        av.src_duration = 30.0;
        av.link_id = Some(link.clone());
        let mut aa = test_clip(&a1, TrackKind::Audio, 0.0, 4.0);
        aa.src_duration = 30.0;
        aa.link_id = Some(link);
        let mut next_v = test_clip(&v1, TrackKind::Video, 4.0, 3.0);
        next_v.src_duration = 30.0;
        let mut next_a = test_clip(&a1, TrackKind::Audio, 4.0, 3.0);
        next_a.src_duration = 30.0;
        let (vid, nid, naid) = (av.id.clone(), next_v.id.clone(), next_a.id.clone());
        store.clips.extend([av, aa, next_v, next_a]);

        // 50 % mit Ripple: Paar wird 8 s lang, Folgeclips rücken auf 8 —
        // EIN Ripple je Verknüpfungsgruppe (nicht doppelt).
        store.set_clip_speed(&[vid.clone()], 0.5, false, false, true);
        let v = store.clip(&vid).unwrap();
        assert!((v.duration - 8.0).abs() < 1e-9, "{}", v.duration);
        assert!((v.eff_speed() - 0.5).abs() < 1e-12);
        assert!((store.clip(&nid).unwrap().start - 8.0).abs() < 1e-9);
        assert!((store.clip(&naid).unwrap().start - 8.0).abs() < 1e-9);
        // Der verknüpfte Audio-Partner folgt mit.
        let partner = store
            .clips
            .iter()
            .find(|c| c.kind == TrackKind::Audio && c.link_id.is_some())
            .unwrap();
        assert!((partner.duration - 8.0).abs() < 1e-9);

        store.undo();
        assert!((store.clip(&vid).unwrap().duration - 4.0).abs() < 1e-9);
        assert!((store.clip(&nid).unwrap().start - 4.0).abs() < 1e-9);

        // Ohne Ripple: am Folgeclip gekappt (Medien werden geschnitten).
        store.set_clip_speed(&[vid.clone()], 0.5, false, false, false);
        let v = store.clip(&vid).unwrap();
        assert!((v.duration - 4.0).abs() < 1e-9, "gekappt: {}", v.duration);
        assert!((store.clip(&nid).unwrap().start - 4.0).abs() < 1e-9, "kein Ripple");
    }

    #[test]
    fn freeze_frame_at_playhead_freezes_current_media_time() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let clip = speed_clip(&v1, 2.0, false); // media = 2 + 2·t
        let id = clip.id.clone();
        store.clips.push(clip);
        store.set_playhead(1.0);
        store.freeze_frame_at_playhead(&[id.clone()]);
        let c = store.clip(&id).unwrap();
        assert!(c.freeze);
        assert!((c.src_in - 4.0).abs() < 1e-9, "Frame bei Medienzeit 4: {}", c.src_in);
        assert_eq!(c.media_time_at(0.0), c.media_time_at(3.9));
        // Standbild ist frei dehnbar.
        let (_, hi) = trim_range(c, TrimEdge::End, &store.clips, false);
        assert!(hi.is_infinite());
        store.undo();
        assert!(!store.clip(&id).unwrap().freeze);
    }

    #[test]
    fn speed_labels_match_premiere_conventions() {
        let mut c = speed_clip("t", 1.0, false);
        assert_eq!(c.speed_label(), None);
        c.speed = 0.5;
        assert_eq!(c.speed_label().as_deref(), Some("50 %"));
        c.reverse = true;
        c.speed = 1.0;
        assert_eq!(c.speed_label().as_deref(), Some("−100 %"));
        c.freeze = true;
        assert_eq!(c.speed_label().as_deref(), Some("Standbild"));
    }

    // ------------------------------------------------------------ Marker

    #[test]
    fn marker_add_is_frame_snapped_and_idempotent() {
        let mut store = TimelineStore::default(); // 25 fps ⇒ 0,04 s/Frame
        let id1 = store.add_marker_at(0.41); // Frame 10 → 0,40 s
        assert_eq!(store.markers.len(), 1);
        assert!((store.markers[0].time - 0.40).abs() < 1e-9, "frame-gerastet");
        // Erneut im selben Frame (0,405 → Frame 10): kein zweiter Marker.
        let id2 = store.add_marker_at(0.405);
        assert_eq!(store.markers.len(), 1);
        assert_eq!(id1, id2);
        // Anderer Frame: neuer Marker, Liste bleibt sortiert.
        store.add_marker_at(0.10);
        assert_eq!(store.markers.len(), 2);
        assert!(store.markers[0].time < store.markers[1].time);
    }

    #[test]
    fn marker_navigation_handles_edges() {
        let mut store = TimelineStore::default();
        // Keine Marker: Navigation bewegt nichts.
        store.playhead_sec = 5.0;
        assert!(!store.go_to_next_marker());
        assert!(!store.go_to_prev_marker());
        assert_eq!(store.playhead_sec, 5.0);

        for t in [1.0, 2.0, 3.0] {
            store.add_marker_at(t);
        }
        // Vor dem ersten: prev findet nichts, next springt auf 1,0.
        store.playhead_sec = 0.0;
        assert!(!store.go_to_prev_marker());
        assert!(store.go_to_next_marker());
        assert!((store.playhead_sec - 1.0).abs() < 1e-9);
        // Exakt auf einem Marker: next/prev springen zum Nachbarn, nicht
        // zum eigenen Frame.
        store.playhead_sec = 2.0;
        assert!(store.go_to_next_marker());
        assert!((store.playhead_sec - 3.0).abs() < 1e-9);
        store.playhead_sec = 2.0;
        assert!(store.go_to_prev_marker());
        assert!((store.playhead_sec - 1.0).abs() < 1e-9);
        // Hinter dem letzten: next findet nichts.
        store.playhead_sec = 9.0;
        assert!(!store.go_to_next_marker());
    }

    #[test]
    fn marker_undo_redo_roundtrip() {
        use crate::core::marker::MarkerColor;
        let mut store = TimelineStore::default();
        let id = store.add_marker_at(2.0);
        store.marker_update(&id, |m| {
            m.name = "Take 1".into();
            m.color = MarkerColor::Red;
        });
        assert_eq!(store.markers[0].name, "Take 1");
        store.undo(); // Name/Farbe zurück
        assert_eq!(store.markers[0].name, "");
        store.undo(); // Marker weg
        assert!(store.markers.is_empty());
        store.redo();
        assert_eq!(store.markers.len(), 1);
        store.redo();
        assert_eq!(store.markers[0].name, "Take 1");
    }

    #[test]
    fn clip_marker_split_partitions_by_media_time() {
        let mut store = TimelineStore::default();
        let v = track_ids(&store, TrackKind::Video)[1].clone();
        // Clip 0–10 s, Quelle ab 0, 1× Tempo: Medienzeit == Sequenzzeit.
        let mut clip = test_clip(&v, TrackKind::Video, 0.0, 10.0);
        clip.src_duration = 30.0;
        clip.markers = vec![Marker::new(2.0), Marker::new(7.0)];
        let cid = clip.id.clone();
        store.clips.push(clip);
        // Bei Sequenzzeit 5 schneiden.
        store.split_at(5.0, Some(&[cid.clone()]));
        let left = store.clips.iter().find(|c| c.id == cid).unwrap();
        let right = store.clips.iter().find(|c| c.id != cid).unwrap();
        assert_eq!(left.markers.len(), 1, "linke Hälfte behält Marker < Schnitt");
        assert!((left.markers[0].time - 2.0).abs() < 1e-9);
        assert_eq!(right.markers.len(), 1, "rechte Hälfte behält Marker ≥ Schnitt");
        assert!((right.markers[0].time - 7.0).abs() < 1e-9);
    }

    #[test]
    fn clip_marker_anchors_to_media_through_trim_and_move() {
        let mut store = TimelineStore::default();
        let v = track_ids(&store, TrackKind::Video)[1].clone();
        let mut clip = test_clip(&v, TrackKind::Video, 4.0, 10.0);
        clip.src_in = 0.0;
        clip.src_duration = 30.0;
        // Marker bei Medienzeit 3 ⇒ Sequenzzeit 4+3 = 7.
        clip.markers = vec![Marker::new(3.0)];
        let cid = clip.id.clone();
        store.clips.push(clip);
        let seq0 = store.clip(&cid).unwrap().visible_markers().next().unwrap().0;
        assert!((seq0 - 7.0).abs() < 1e-9);

        // Kopf um 2 s trimmen (src_in → 2, start → 6): Medienzeit bleibt 3,
        // Sequenzposition wandert auf 6 + (3−2) = 7? Nein: media 3, src_in 2,
        // off = 3−2 = 1, seq = 6+1 = 7 bleibt gleich — der Marker hängt am
        // Frame, nicht an der Clipkante.
        store.trim_clip(&cid, TrimEdge::Start, 2.0);
        let c = store.clip(&cid).unwrap();
        assert!((c.src_in - 2.0).abs() < 1e-9);
        assert_eq!(c.markers.len(), 1, "Marker bleibt erhalten");
        assert!((c.markers[0].time - 3.0).abs() < 1e-9, "Medienzeit unverändert");
        let seq1 = c.visible_markers().next().unwrap().0;
        assert!((seq1 - 7.0).abs() < 1e-9, "Frame-Position stabil: {seq1}");

        // Clip um +3 s verschieben (start 6 → 9): Medienzeit bleibt, die
        // Sequenzposition wandert mit dem Clip auf 10.
        store.move_clips(&[cid.clone()], 3.0, 0);
        let c = store.clip(&cid).unwrap();
        assert!((c.markers[0].time - 3.0).abs() < 1e-9);
        let seq2 = c.visible_markers().next().unwrap().0;
        assert!((seq2 - 10.0).abs() < 1e-9, "wandert mit dem Clip: {seq2}");
    }

    #[test]
    fn insert_copies_asset_markers_into_clip() {
        let mut store = TimelineStore::default();
        let mut asset = MediaAsset {
            id: "a1".into(),
            path: "/tmp/x.mp4".into(),
            name: "x.mp4".into(),
            kind: MediaKind::Video,
            info: crate::core::types::MediaInfo {
                path: "/tmp/x.mp4".into(),
                file_name: "x.mp4".into(),
                container: "mp4".into(),
                duration_sec: 10.0,
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
            markers: vec![Marker::new(2.0), Marker::new(8.0)],
            proxy_path: None,
            proxy_src_mtime: None,
            proxy_offline: false,
        };
        asset.markers[0].name = "Beat".into();
        let assets = vec![asset];
        store.insert_assets(&assets, &["a1".into()], 0.0, None);
        let clip = store
            .clips
            .iter()
            .find(|c| c.asset_id == "a1")
            .expect("Clip eingefügt");
        // Beide Asset-Marker fallen in [0, 10] ⇒ als Clip-Marker kopiert,
        // mit frischen IDs (kein Verweis auf die Asset-Marker-IDs).
        assert_eq!(clip.markers.len(), 2);
        assert!(clip.markers.iter().any(|m| m.name == "Beat"));
        assert!(clip.markers.iter().all(|m| m.id != "" ));
    }

    #[test]
    fn clip_marker_outside_view_is_hidden_but_kept() {
        let mut store = TimelineStore::default();
        let v = track_ids(&store, TrackKind::Video)[1].clone();
        let mut clip = test_clip(&v, TrackKind::Video, 0.0, 5.0);
        clip.src_in = 2.0;
        clip.src_duration = 30.0;
        // Marker bei Medienzeit 1 liegt VOR src_in (weggetrimmt).
        clip.markers = vec![Marker::new(1.0), Marker::new(3.0)];
        let cid = clip.id.clone();
        store.clips.push(clip);
        let c = store.clip(&cid).unwrap();
        // Beide bleiben gespeichert, nur einer ist sichtbar.
        assert_eq!(c.markers.len(), 2);
        assert_eq!(c.visible_markers().count(), 1);
    }

    #[test]
    fn asset_usage_tracking() {
        let mut store = TimelineStore::default();
        let v = track_ids(&store, TrackKind::Video)[1].clone();
        // Zwei Clips referenzieren a1 (bei 8 s und bei 2 s), einer a2.
        let mut c1 = test_clip(&v, TrackKind::Video, 8.0, 3.0);
        c1.asset_id = "a1".into();
        let mut c2 = test_clip(&v, TrackKind::Video, 2.0, 3.0);
        c2.asset_id = "a1".into();
        let mut c3 = test_clip(&v, TrackKind::Video, 20.0, 3.0);
        c3.asset_id = "a2".into();
        store.clips.push(c1);
        store.clips.push(c2.clone());
        store.clips.push(c3);

        assert_eq!(store.asset_usage_count("a1"), 2);
        assert_eq!(store.asset_usage_count("a2"), 1);
        assert_eq!(store.asset_usage_count("a3"), 0);

        // Erste Verwendung = der frühere Clip (Start 2 s).
        let (first_id, start) = store.first_use_of_asset("a1").unwrap();
        assert_eq!(first_id, c2.id);
        assert!((start - 2.0).abs() < 1e-9);

        // reveal springt zum Start und selektiert den Clip.
        assert!(store.reveal_asset_usage("a1"));
        assert!((store.playhead_sec - 2.0).abs() < 1e-9);
        assert!(store.selected_clip_ids.contains(&c2.id));
        // Unbenutztes Asset: kein Sprung.
        assert!(!store.reveal_asset_usage("a3"));
    }

    #[test]
    fn multicam_angle_switch_and_live_cut() {
        let mut s = TimelineStore::default();
        let vid_id = s.insert_multicam_clip("src", "Cam", 10.0, true, 0.0, None);
        // Video + verknüpftes Audio, beide Winkel 0.
        assert_eq!(s.clips.iter().filter(|c| c.is_multicam()).count(), 2);
        assert!(s
            .clips
            .iter()
            .filter(|c| c.is_multicam())
            .all(|c| c.multicam.as_ref().unwrap().angle == 0));

        // Winkelwechsel ohne Wiedergabe: ganze Link-Gruppe, rückgängig-machbar.
        assert!(s.set_multicam_angle_undoable(&vid_id, 3));
        assert!(s
            .clips
            .iter()
            .filter(|c| c.is_multicam())
            .all(|c| c.multicam.as_ref().unwrap().angle == 3));
        s.undo();
        assert!(s
            .clips
            .iter()
            .filter(|c| c.is_multicam())
            .all(|c| c.multicam.as_ref().unwrap().angle == 0));

        // Live-Schnitt am Playhead 4 s, Winkel 2: teilt Video + Audio, die
        // rechten Hälften tragen den neuen Winkel, die linken den alten.
        s.set_playhead(4.0);
        assert!(s.multicam_live_cut(4.0, 2));
        let mc: Vec<&TimelineClip> = s.clips.iter().filter(|c| c.is_multicam()).collect();
        assert_eq!(mc.len(), 4, "je Video/Audio eine linke + rechte Hälfte");
        for c in &mc {
            let a = c.multicam.as_ref().unwrap().angle;
            if (c.start - 4.0).abs() < 1e-6 {
                assert_eq!(a, 2, "rechte Hälfte = neuer Winkel");
            } else {
                assert_eq!(a, 0, "linke Hälfte = alter Winkel");
            }
        }
    }
