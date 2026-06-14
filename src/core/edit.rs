//! Three-Point-Editing: aus Quellmonitor-In/Out (bzw. ganzem Clip) plus
//! Sequenz-In/Out/Playhead und dem Source-Patching das Insert-/Overwrite-Edit
//! berechnen und über `TimelineStore::commit_edit` anwenden. Plus Match Frame.
//!
//! Die klassische Premiere-Logik:
//!   • Quelle In+Out + Ziel-Punkt  → Material [In,Out) am Zielpunkt.
//!   • Quelle In + Ziel In+Out      → Out der Quelle ergibt sich aus der Dauer.
//!   • Quelle Out + Ziel In+Out     → In der Quelle ergibt sich.
//!   • Vier-Punkt (alle vier)       → Warnung, Quelldauer hat Vorrang.
//!   • Keine Quellmarken            → ganzer Clip.

use crate::core::animation::ClipFx;
use crate::core::grade::ColorGrade;
use crate::core::marker::Marker;
use crate::core::timeline::{TimelineClip, TrackKind, IMAGE_DEFAULT_DURATION, MIN_CLIP_DURATION};
use crate::core::types::{new_id, MediaKind};
use crate::state::AppState;

const EPS: f64 = 1e-6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditMode {
    /// Einfügen mit Ripple (Komma).
    Insert,
    /// Überschreiben am Zielpunkt (Punkt).
    Overwrite,
}

/// Three-Point-Edit aus dem Quellmonitor (oder einem einzelnen Bin-Asset)
/// ausführen. Liefert optional eine Status-/Warnmeldung.
pub fn perform_source_edit(state: &mut AppState, mode: EditMode) -> Option<String> {
    // 1) Quelle bestimmen: Quellmonitor bevorzugt, sonst einzelnes Bin-Asset.
    let (asset_id, use_marks) = if let Some(id) = state.playback.source_asset_id.clone() {
        (id, true)
    } else {
        match state.media.selected_asset_ids.as_slice() {
            [one] => (one.clone(), false),
            [] => return Some("Kein Clip im Quellmonitor".into()),
            _ => return Some("Mehrfachauswahl: Clip in den Quellmonitor laden".into()),
        }
    };
    let Some(asset) = state.media.asset(&asset_id) else {
        return Some("Quellmaterial offline".into());
    };
    let media_dur = asset.info.duration_sec.max(0.0);
    let is_image = asset.kind == MediaKind::Image;
    let has_video = asset.kind != MediaKind::Audio;
    let has_audio = !is_image && !asset.info.audio.is_empty();
    if !has_video && !has_audio {
        return Some("Quelle hat kein Material".into());
    }
    let asset_name = asset.name.clone();
    let asset_markers = asset.markers.clone();
    // Standbilder sind frei dehnbar (unendliche Quellspanne).
    let src_total = if is_image { f64::INFINITY } else { media_dur };

    // 2) Quell-In/Out (nur aus dem Quellmonitor; Bin-Fallback = ganzer Clip).
    let (s_in, s_out) = if use_marks {
        (state.playback.source.in_mark, state.playback.source.out_mark)
    } else {
        (None, None)
    };
    let src_lo = s_in.unwrap_or(0.0).max(0.0);
    let src_hi = s_out.unwrap_or(media_dur);

    // 3) Ziel-In/Out + Playhead.
    let t_in = state.timeline.in_point;
    let t_out = state.timeline.out_point;
    let playhead = state.timeline.playhead_sec;

    // 4) Drei-/Vier-Punkt-Fallunterscheidung → Zielposition, Dauer, Quell-In.
    let mut warning: Option<String> = None;
    let place_at;
    let mut dur;
    let src_in;
    match (t_in, t_out) {
        (Some(ti), Some(to)) if to - ti >= MIN_CLIP_DURATION => {
            let target_dur = to - ti;
            place_at = ti.max(0.0);
            if s_in.is_some() && s_out.is_some() {
                // Vier-Punkt: Quelldauer hat Vorrang, Sequenz-Out wird ignoriert.
                warning = Some("Vier-Punkt-Edit: Sequenz-Out ignoriert (Quelldauer)".into());
                src_in = src_lo;
                dur = (src_hi - src_lo).max(MIN_CLIP_DURATION);
            } else if s_out.is_some() {
                // Quell-Out + Ziel-Dauer → Quell-In ergibt sich rückwärts.
                dur = target_dur;
                src_in = (src_hi - target_dur).max(0.0);
            } else {
                // Quell-In (oder keine Quellmarke) + Ziel-Dauer.
                dur = target_dur;
                src_in = src_lo;
            }
        }
        _ => {
            // Zielpunkt = Sequenz-In sonst Playhead; Quellbereich gibt die Dauer.
            place_at = t_in.unwrap_or(playhead).max(0.0);
            src_in = src_lo;
            dur = if is_image && s_in.is_none() && s_out.is_none() {
                IMAGE_DEFAULT_DURATION
            } else {
                (src_hi - src_lo).max(MIN_CLIP_DURATION)
            };
        }
    }

    // 5) An verfügbares Quellmaterial klemmen (nicht über das Dateiende hinaus).
    if src_total.is_finite() {
        let avail = (src_total - src_in).max(MIN_CLIP_DURATION);
        if dur > avail + EPS {
            dur = avail;
            warning.get_or_insert_with(|| "Zu wenig Quellmaterial — gekürzt".into());
        }
    }

    // 6) Frame-genau auf das Sequenzraster rasten.
    let place_at = state.timeline.snap_to_frame(place_at);
    let dur = (state.timeline.snap_to_frame(place_at + dur) - place_at).max(MIN_CLIP_DURATION);

    // 7) Zielspuren aus dem Patching.
    let v_track = state.timeline.source_patch_track(TrackKind::Video).map(str::to_string);
    let a_track = state.timeline.source_patch_track(TrackKind::Audio).map(str::to_string);
    let want_video = has_video && v_track.is_some();
    let want_audio = has_audio && a_track.is_some();
    if !want_video && !want_audio {
        return Some("Keine Zielspur gepatcht".into());
    }

    // 8) Quell-Clips bauen (verknüpft, wenn Video + Audio gemeinsam landen).
    let link = want_video && want_audio;
    let link_id = if link { Some(new_id()) } else { None };
    let markers: Vec<Marker> = asset_markers
        .iter()
        .filter(|m| m.time >= src_in - EPS && m.time <= src_in + dur + EPS)
        .map(|m| {
            let mut nm = m.clone();
            nm.id = new_id();
            nm
        })
        .collect();
    let mut new_clips: Vec<TimelineClip> = Vec::new();
    if want_video {
        new_clips.push(source_clip(
            v_track.unwrap(),
            &asset_id,
            asset_name.clone(),
            TrackKind::Video,
            place_at,
            dur,
            src_in,
            src_total,
            link_id.clone(),
            markers.clone(),
        ));
    }
    if want_audio {
        let name = if link {
            format!("{asset_name} (Audio)")
        } else {
            asset_name.clone()
        };
        new_clips.push(source_clip(
            a_track.unwrap(),
            &asset_id,
            name,
            TrackKind::Audio,
            place_at,
            dur,
            src_in,
            src_total,
            link_id,
            markers,
        ));
    }

    let ripple = matches!(mode, EditMode::Insert);
    state.timeline.commit_edit(new_clips, place_at, dur, ripple);
    // Playhead ans Ende des Eingefügten (Premiere-Konvention).
    state.timeline.set_playhead(place_at + dur);
    state.dock.open_panel("timeline");
    warning
}

/// Match Frame (F): Frame unter dem Playhead im Quellmonitor mit exakter
/// Medienzeit öffnen. Liefert optional eine Statusmeldung.
pub fn match_frame(state: &mut AppState) -> Option<String> {
    let t = state.timeline.playhead_sec;
    let Some((asset_id, media_t)) = state.timeline.match_frame_source(t) else {
        return Some("Kein Clip am Playhead".into());
    };
    if state.media.asset(&asset_id).is_none() {
        return Some("Quellmaterial offline".into());
    }
    state.playback.source_asset_id = Some(asset_id);
    state.playback.source = Default::default();
    state.playback.source.rate = 1.0;
    state.playback.source.position = media_t.max(0.0);
    state.app.focused_panel = "source".into();
    state.dock.open_panel("source");
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{AudioStreamInfo, MediaAsset, MediaInfo, VideoStreamInfo};

    fn av_asset(id: &str, dur: f64, with_audio: bool) -> MediaAsset {
        MediaAsset {
            id: id.into(),
            path: "/x.mp4".into(),
            name: "Clip".into(),
            kind: MediaKind::Video,
            info: MediaInfo {
                path: "/x.mp4".into(),
                file_name: "x.mp4".into(),
                container: "mp4".into(),
                duration_sec: dur,
                size_bytes: 0,
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
                audio: if with_audio {
                    vec![AudioStreamInfo {
                        index: 1,
                        codec: "aac".into(),
                        channels: 2,
                        sample_rate: 48000,
                        bitrate: None,
                    }]
                } else {
                    vec![]
                },
                recorded_at: None,
            },
            thumbnail_path: None,
            imported_at: 0.0,
            bin_id: crate::core::bin::ROOT_BIN_ID.to_string(),
            label: None,
            offline: false,
            markers: vec![],
            proxy_path: None,
            proxy_src_mtime: None,
            proxy_offline: false,
        }
    }

    fn setup(dur: f64) -> AppState {
        let mut s = AppState::default();
        s.media.add_asset(av_asset("a", dur, true));
        s.playback.source_asset_id = Some("a".into());
        s
    }

    fn video_clip(s: &AppState) -> TimelineClip {
        let vt = s.timeline.source_patch_track(TrackKind::Video).unwrap().to_string();
        s.timeline.clips.iter().find(|c| c.track_id == vt).unwrap().clone()
    }

    fn audio_clip(s: &AppState) -> TimelineClip {
        let at = s.timeline.source_patch_track(TrackKind::Audio).unwrap().to_string();
        s.timeline.clips.iter().find(|c| c.track_id == at).unwrap().clone()
    }

    #[test]
    fn three_point_source_in_out_at_playhead() {
        let mut s = setup(20.0);
        s.playback.source.in_mark = Some(2.0);
        s.playback.source.out_mark = Some(6.0);
        s.timeline.set_playhead(10.0);
        let warn = perform_source_edit(&mut s, EditMode::Overwrite);
        assert!(warn.is_none());
        let v = video_clip(&s);
        assert!((v.start - 10.0).abs() < EPS);
        assert!((v.duration - 4.0).abs() < EPS);
        assert!((v.src_in - 2.0).abs() < EPS);
        // Video + Audio verknüpft am selben Ort.
        let a = audio_clip(&s);
        assert!((a.start - 10.0).abs() < EPS && (a.duration - 4.0).abs() < EPS);
        assert!(v.link_id.is_some() && v.link_id == a.link_id);
        // Playhead ans Ende (Premiere).
        assert!((s.timeline.playhead_sec - 14.0).abs() < EPS);
    }

    #[test]
    fn three_point_source_in_with_target_range() {
        let mut s = setup(20.0);
        s.playback.source.in_mark = Some(2.0);
        s.timeline.set_in_out_range(10.0, 18.0); // Dauer 8
        assert!(perform_source_edit(&mut s, EditMode::Overwrite).is_none());
        let v = video_clip(&s);
        assert!((v.start - 10.0).abs() < EPS);
        assert!((v.duration - 8.0).abs() < EPS, "Dauer aus Ziel-Bereich");
        assert!((v.src_in - 2.0).abs() < EPS);
    }

    #[test]
    fn three_point_source_out_with_target_range_derives_in() {
        let mut s = setup(20.0);
        s.playback.source.out_mark = Some(12.0);
        s.timeline.set_in_out_range(10.0, 18.0); // Dauer 8
        assert!(perform_source_edit(&mut s, EditMode::Overwrite).is_none());
        let v = video_clip(&s);
        assert!((v.duration - 8.0).abs() < EPS);
        // Quell-In ergibt sich: 12 − 8 = 4.
        assert!((v.src_in - 4.0).abs() < EPS);
    }

    #[test]
    fn four_point_warns_and_uses_source_duration() {
        let mut s = setup(20.0);
        s.playback.source.in_mark = Some(2.0);
        s.playback.source.out_mark = Some(6.0); // Quelldauer 4
        s.timeline.set_in_out_range(10.0, 20.0); // Zieldauer 10
        let warn = perform_source_edit(&mut s, EditMode::Overwrite);
        assert!(warn.is_some(), "Vier-Punkt-Warnung");
        let v = video_clip(&s);
        assert!((v.duration - 4.0).abs() < EPS, "Quelldauer hat Vorrang");
        assert!((v.start - 10.0).abs() < EPS);
    }

    #[test]
    fn no_marks_uses_whole_clip() {
        let mut s = setup(20.0);
        s.timeline.set_playhead(0.0);
        assert!(perform_source_edit(&mut s, EditMode::Overwrite).is_none());
        let v = video_clip(&s);
        assert!((v.start - 0.0).abs() < EPS);
        assert!((v.duration - 20.0).abs() < EPS);
        assert!((v.src_in - 0.0).abs() < EPS);
    }

    #[test]
    fn insert_ripples_existing_material() {
        let mut s = setup(20.0);
        // Bereits Material auf der Patch-Video-Spur.
        let vt = s.timeline.source_patch_track(TrackKind::Video).unwrap().to_string();
        let existing = {
            let mut c = TimelineClip {
                id: crate::core::types::new_id(),
                track_id: vt.clone(),
                asset_id: "a".into(),
                name: "alt".into(),
                kind: TrackKind::Video,
                start: 0.0,
                duration: 20.0,
                src_in: 0.0,
                src_duration: 20.0,
                link_id: None,
                enabled: true,
                gain_db: 0.0,
                fx: ClipFx::default(),
                grade: ColorGrade::default(),
                effects: vec![],
                title: None,
                subtitle: None,
                speed: 1.0,
                reverse: false,
                freeze: false,
                markers: vec![],
                nest_seq: None,
                multicam: None,
            };
            c.name = "alt".into();
            c
        };
        s.timeline.clips.push(existing);
        s.playback.source.in_mark = Some(0.0);
        s.playback.source.out_mark = Some(4.0);
        s.timeline.set_playhead(5.0);
        assert!(perform_source_edit(&mut s, EditMode::Insert).is_none());
        // Bestehender Clip bei 5 gesplittet, rechte Hälfte um 4 nach hinten.
        let mut on_v: Vec<f64> = s
            .timeline
            .clips
            .iter()
            .filter(|c| c.track_id == vt)
            .map(|c| c.start)
            .collect();
        on_v.sort_by(|a, b| a.total_cmp(b));
        assert_eq!(on_v.len(), 3);
        assert!((on_v[0] - 0.0).abs() < EPS); // linke Hälfte
        assert!((on_v[1] - 5.0).abs() < EPS); // neuer Clip
        assert!((on_v[2] - 9.0).abs() < EPS); // rechte Hälfte verschoben
    }

    #[test]
    fn match_frame_loads_source_at_media_time() {
        let mut s = AppState::default();
        s.media.add_asset(av_asset("clipA", 30.0, true));
        let vt = s.timeline.source_patch_track(TrackKind::Video).unwrap().to_string();
        s.timeline.clips.push(TimelineClip {
            id: crate::core::types::new_id(),
            track_id: vt,
            asset_id: "clipA".into(),
            name: "c".into(),
            kind: TrackKind::Video,
            start: 4.0,
            duration: 10.0,
            src_in: 2.0,
            src_duration: 30.0,
            link_id: None,
            enabled: true,
            gain_db: 0.0,
            fx: ClipFx::default(),
            grade: ColorGrade::default(),
            effects: vec![],
            title: None,
            subtitle: None,
            speed: 1.0,
            reverse: false,
            freeze: false,
            markers: vec![],
            nest_seq: None,
            multicam: None,
        });
        s.timeline.set_playhead(6.0); // Medienzeit 2 + (6−4) = 4
        assert!(match_frame(&mut s).is_none());
        assert_eq!(s.playback.source_asset_id.as_deref(), Some("clipA"));
        assert!((s.playback.source.position - 4.0).abs() < EPS);
    }

    #[test]
    fn unpatched_audio_skips_audio_stream() {
        let mut s = setup(20.0);
        // Audio-Patch deaktivieren.
        let at = s.timeline.source_patch_track(TrackKind::Audio).unwrap().to_string();
        s.timeline.toggle_source_patch(&at);
        s.timeline.set_playhead(0.0);
        assert!(perform_source_edit(&mut s, EditMode::Overwrite).is_none());
        // Nur Video gelandet, keine Audiospur bekommt Material.
        assert!(s.timeline.clips.iter().all(|c| c.kind == TrackKind::Video));
        let v = video_clip(&s);
        assert!(v.link_id.is_none(), "ohne Audio kein Link");
    }
}

#[allow(clippy::too_many_arguments)]
fn source_clip(
    track_id: String,
    asset_id: &str,
    name: String,
    kind: TrackKind,
    start: f64,
    duration: f64,
    src_in: f64,
    src_duration: f64,
    link_id: Option<String>,
    markers: Vec<Marker>,
) -> TimelineClip {
    TimelineClip {
        id: new_id(),
        track_id,
        asset_id: asset_id.to_string(),
        name,
        kind,
        start,
        duration,
        src_in,
        src_duration,
        link_id,
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
        markers,
        nest_seq: None,
        multicam: None,
    }
}
