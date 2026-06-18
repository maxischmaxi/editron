//! Three-Point-Editing: aus Quellmonitor-In/Out (bzw. ganzem Clip) plus
//! Sequenz-In/Out/Playhead und dem Source-Patching das Insert-/Overwrite-Edit
//! berechnen und über `TimelineStore::commit_edit` anwenden. Plus Match Frame,
//! Replace und Fit-to-Fill.
//!
//! Die klassische Premiere-Logik:
//!   • Quelle In+Out + Ziel-Punkt  → Material [In,Out) am Zielpunkt.
//!   • Quelle In + Ziel In+Out      → Out der Quelle ergibt sich aus der Dauer.
//!   • Quelle Out + Ziel In+Out     → In der Quelle ergibt sich.
//!   • Vier-Punkt (alle vier)       → `perform_source_edit` lässt die Quelldauer
//!     gewinnen (Warnung); für den echten Vier-Punkt-Schnitt → `perform_fit_to_fill`.
//!   • Keine Quellmarken            → ganzer Clip.
//!
//! `perform_replace` (Premiere „Replace Edit", Resolve F11): das Medium eines
//! vorhandenen Clips austauschen, Position UND Dauer bleiben; Match-Frame-Sync
//! richtet den Quellmonitor-Frame am Timeline-Playhead aus. `perform_fit_to_fill`
//! (Resolve Shift+F11): Vier-Punkt-Schnitt, der die Clip-Geschwindigkeit so
//! setzt, dass die Quell-Range die Ziel-Range exakt füllt.

use crate::core::animation::{AnimatedParam, ClipFx};
use crate::core::grade::ColorGrade;
use crate::core::marker::Marker;
use crate::core::timeline::{
    expand_links, TimelineClip, TrackKind, IMAGE_DEFAULT_DURATION, MAX_CLIP_SPEED, MIN_CLIP_DURATION,
    MIN_CLIP_SPEED,
};
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

/// Aufgelöste Quelle für einen Schnitt: Quellmonitor bevorzugt, sonst ein
/// einzeln im Bin gewähltes Asset. Gemeinsame Basis von `perform_source_edit`,
/// `perform_replace` und `perform_fit_to_fill`.
struct SourceInfo {
    asset_id: String,
    name: String,
    /// Quelldauer in Sekunden (0 bei Standbild — dann zählt `src_total`).
    media_dur: f64,
    /// Frei dehnbare Gesamtspanne: `media_dur`, bei Standbildern INFINITY.
    src_total: f64,
    is_image: bool,
    has_video: bool,
    has_audio: bool,
    markers: Vec<Marker>,
    /// Quellmonitor geladen ⇒ In/Out-Marken gelten (Bin-Fallback: ganzer Clip).
    use_marks: bool,
}

/// Quelle bestimmen + validieren (Schritt 1 der klassischen Premiere-Logik).
fn resolve_source(state: &AppState) -> Result<SourceInfo, String> {
    let (asset_id, use_marks) = if let Some(id) = state.playback.source_asset_id.clone() {
        (id, true)
    } else {
        match state.media.selected_asset_ids.as_slice() {
            [one] => (one.clone(), false),
            [] => return Err("Kein Clip im Quellmonitor".into()),
            _ => return Err("Mehrfachauswahl: Clip in den Quellmonitor laden".into()),
        }
    };
    let Some(asset) = state.media.asset(&asset_id) else {
        return Err("Quellmaterial offline".into());
    };
    let media_dur = asset.info.duration_sec.max(0.0);
    let is_image = asset.kind == MediaKind::Image;
    let has_video = asset.kind != MediaKind::Audio;
    let has_audio = !is_image && !asset.info.audio.is_empty();
    if !has_video && !has_audio {
        return Err("Quelle hat kein Material".into());
    }
    Ok(SourceInfo {
        name: asset.name.clone(),
        markers: asset.markers.clone(),
        media_dur,
        // Standbilder sind frei dehnbar (unendliche Quellspanne).
        src_total: if is_image { f64::INFINITY } else { media_dur },
        is_image,
        has_video,
        has_audio,
        use_marks,
        asset_id,
    })
}

/// Three-Point-Edit aus dem Quellmonitor (oder einem einzelnen Bin-Asset)
/// ausführen. Liefert optional eine Status-/Warnmeldung.
pub fn perform_source_edit(state: &mut AppState, mode: EditMode) -> Option<String> {
    // 1) Quelle bestimmen: Quellmonitor bevorzugt, sonst einzelnes Bin-Asset.
    let src = match resolve_source(state) {
        Ok(s) => s,
        Err(msg) => return Some(msg),
    };
    let SourceInfo {
        asset_id,
        name: asset_name,
        media_dur,
        src_total,
        is_image,
        has_video,
        has_audio,
        markers: asset_markers,
        use_marks,
    } = src;

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

/// Zielgruppe für `perform_replace`: erst die (link-expandierte) Auswahl,
/// sonst der Clip unter dem Playhead — bevorzugt auf einer anvisierten Video-,
/// dann Audiospur, dann beliebig (gleiche Reihenfolge wie Match Frame).
fn replace_target_ids(state: &AppState) -> Vec<String> {
    let tl = &state.timeline;
    if !tl.selected_clip_ids.is_empty() {
        return expand_links(&tl.clips, &tl.selected_clip_ids);
    }
    let t = tl.playhead_sec;
    let find = |kind: TrackKind, targeted_only: bool| -> Option<String> {
        for track in tl.tracks.iter().filter(|tr| tr.kind == kind) {
            if targeted_only && !track.targeted {
                continue;
            }
            if let Some(c) = tl.clips.iter().find(|c| {
                c.track_id == track.id
                    && !c.is_generator()
                    && c.start <= t + EPS
                    && c.end() > t - EPS
            }) {
                return Some(c.id.clone());
            }
        }
        None
    };
    let id = find(TrackKind::Video, true)
        .or_else(|| find(TrackKind::Audio, true))
        .or_else(|| find(TrackKind::Video, false))
        .or_else(|| find(TrackKind::Audio, false));
    match id {
        Some(id) => expand_links(&tl.clips, &[id]),
        None => Vec::new(),
    }
}

/// Replace (Premiere „Replace Edit", Resolve F11): den ausgewählten Clip bzw.
/// den Clip unter dem Playhead durch das aktuelle Quellmaterial ersetzen —
/// Position UND Dauer bleiben erhalten, nur Medium und In-Zeit ändern sich.
/// Effekte/Farbkorrektur/Lautstärke des Zielclips bleiben (Premiere-Semantik);
/// medienzeit-verankerte Keyframes werden um den In-Zeit-Versatz mitgeschoben,
/// damit die Animation clip-relativ stehen bleibt. Generatoren (Titel/Unter-
/// titel) werden nicht ersetzt. Match-Frame-Sync: der Frame am Quellmonitor-
/// Playhead (sonst In-Marke/0)
/// landet auf dem Timeline-Playhead, falls dieser im Clip liegt, sonst auf
/// dem Clipanfang. Liefert optional eine Status-/Warnmeldung.
pub fn perform_replace(state: &mut AppState) -> Option<String> {
    let src = match resolve_source(state) {
        Ok(s) => s,
        Err(msg) => return Some(msg),
    };

    // Quell-Ankerframe (Match-Frame): Quellmonitor-Position, sonst In-Marke/0.
    let src_anchor = if src.use_marks {
        let s = &state.playback.source;
        if s.position > EPS {
            s.position
        } else {
            s.in_mark.unwrap_or(0.0)
        }
    } else {
        0.0
    }
    .max(0.0);

    let group_ids = replace_target_ids(state);
    if group_ids.is_empty() {
        return Some("Kein Clip zum Ersetzen (Auswahl oder Playhead)".into());
    }
    let ph = state.timeline.playhead_sec;

    // Plan unter unveränderlicher Leihe sammeln, dann anwenden.
    struct Plan {
        id: String,
        src_in: f64,
        is_audio: bool,
    }
    let mut plans: Vec<Plan> = Vec::new();
    let mut clamped = false;
    {
        let tl = &state.timeline;
        let locked: std::collections::HashSet<&str> = tl
            .tracks
            .iter()
            .filter(|t| t.locked)
            .map(|t| t.id.as_str())
            .collect();
        for id in &group_ids {
            let Some(c) = tl.clips.iter().find(|c| &c.id == id) else {
                continue;
            };
            if locked.contains(c.track_id.as_str()) {
                continue;
            }
            // Generatoren (Titel/Untertitel) haben kein Medium zum Match-Framen
            // und werden nicht still in einen Medien-Clip verwandelt.
            if c.is_generator() {
                continue;
            }
            // Nur Streams ersetzen, die die Quelle liefert (Video↔Video, Audio↔Audio).
            let is_audio = c.kind == TrackKind::Audio;
            match c.kind {
                TrackKind::Video if !src.has_video => continue,
                TrackKind::Audio if !src.has_audio => continue,
                TrackKind::Subtitle => continue,
                _ => {}
            }
            // Match-Frame: nur mit echtem Quell-Ankerframe (Quellmonitor) den
            // Playhead anvisieren; bei einem Bin-Asset (kein Quell-Playhead)
            // den Quellanfang am Clipanfang ausrichten. Liegt der Playhead
            // außerhalb des Clips, ankert der Quellframe am Clipanfang.
            let target_anchor = if src.use_marks && ph >= c.start - EPS && ph <= c.end() + EPS {
                ph
            } else {
                c.start
            };
            let mut src_in = src_anchor - (target_anchor - c.start);
            // An die Quelle klemmen, sodass die volle Clipdauer Material hat.
            if src.src_total.is_finite() {
                let max_in = (src.src_total - c.duration).max(0.0);
                let fit = src_in.clamp(0.0, max_in);
                if (fit - src_in).abs() > EPS {
                    clamped = true;
                }
                src_in = fit;
            } else {
                src_in = src_in.max(0.0);
            }
            plans.push(Plan {
                id: c.id.clone(),
                src_in,
                is_audio,
            });
        }
    }
    if plans.is_empty() {
        return Some("Kein ersetzbarer Clip (gesperrt, Titel oder Stream-Konflikt)".into());
    }

    state.timeline.push_history();
    for p in &plans {
        if let Some(c) = state.timeline.clips.iter_mut().find(|c| c.id == p.id) {
            // Verschiebung der Medien-In-Zeit: hält die (medienzeit-verankerten)
            // fx-/Effekt-Keyframes nachher an derselben CLIP-RELATIVEN Stelle.
            let delta = p.src_in - c.src_in;
            c.asset_id = src.asset_id.clone();
            c.name = if p.is_audio && src.has_video {
                format!("{} (Audio)", src.name)
            } else {
                src.name.clone()
            };
            c.src_in = p.src_in;
            c.src_duration = src.src_total;
            // Position/Dauer/Effekte/Grade/Gain/Verknüpfung/Blend bleiben.
            c.speed = AnimatedParam::fixed(1.0);
            c.reverse = false;
            c.freeze = false;
            // Auf altes Medium bezogene Generator-/Sonderzustände aufheben.
            c.markers.clear();
            c.nest_seq = None;
            c.multicam = None;
            c.title = None;
            c.subtitle = None;
            // Animation clip-relativ halten (exakt für Vorwärts/Tempo 1 — der
            // Normalfall eines Replace-Ziels; sonst leichte Drift).
            shift_clip_keyframes(c, delta);
        }
    }
    state.timeline.reconcile_transitions();
    state.dock.open_panel("timeline");
    if clamped {
        return Some("Zu wenig Quellmaterial — Match-Frame verschoben".into());
    }
    None
}

/// Fit-to-Fill (Resolve Shift+F11): echter Vier-Punkt-Schnitt. Aus dem
/// Quell-In/Out (4. Punkt) und dem Sequenz-In/Out wird die Clip-Geschwindigkeit
/// so gesetzt, dass die Quell-Range die Ziel-Range exakt füllt
/// (speed = Quelldauer ÷ Zieldauer). Overwrite auf die gepatchten Spuren.
/// Liefert optional eine Status-/Warnmeldung.
pub fn perform_fit_to_fill(state: &mut AppState) -> Option<String> {
    let src = match resolve_source(state) {
        Ok(s) => s,
        Err(msg) => return Some(msg),
    };

    // Quellbereich (4. Punkt): In/Out aus dem Quellmonitor, sonst ganzer Clip.
    let (s_in, s_out) = if src.use_marks {
        (state.playback.source.in_mark, state.playback.source.out_mark)
    } else {
        (None, None)
    };
    let src_lo = s_in.unwrap_or(0.0).max(0.0);
    let src_hi = s_out.unwrap_or(src.media_dur);
    let src_len = src_hi - src_lo;
    if !src.is_image && src_len < MIN_CLIP_DURATION {
        return Some("Quellbereich zu kurz".into());
    }

    // Zielbereich: Sequenz-In UND -Out nötig (die übrigen zwei Punkte).
    let (Some(ti), Some(to)) = (state.timeline.in_point, state.timeline.out_point) else {
        return Some("Fit-to-Fill braucht Sequenz-In und -Out".into());
    };
    let (lo, hi) = (ti.min(to), ti.max(to));
    if hi - lo < MIN_CLIP_DURATION {
        return Some("Zielbereich zu kurz".into());
    }

    // Frame-genau aufs Sequenzraster; die Geschwindigkeit aus der GERASTERTEN
    // Zieldauer ableiten, damit Dauer × Speed == Quelldauer exakt aufgeht.
    let place_at = state.timeline.snap_to_frame(lo);
    let dur = (state.timeline.snap_to_frame(hi) - place_at).max(MIN_CLIP_DURATION);
    let speed_raw = if src.is_image { 1.0 } else { src_len / dur };
    let speed = speed_raw.clamp(MIN_CLIP_SPEED, MAX_CLIP_SPEED);

    // Zielspuren aus dem Patching.
    let v_track = state.timeline.source_patch_track(TrackKind::Video).map(str::to_string);
    let a_track = state.timeline.source_patch_track(TrackKind::Audio).map(str::to_string);
    let want_video = src.has_video && v_track.is_some();
    let want_audio = src.has_audio && a_track.is_some();
    if !want_video && !want_audio {
        return Some("Keine Zielspur gepatcht".into());
    }

    let link = want_video && want_audio;
    let link_id = if link { Some(new_id()) } else { None };
    // Marker im Quellbereich [src_lo, src_hi] übernehmen (Medienzeit-Achse).
    let markers: Vec<Marker> = src
        .markers
        .iter()
        .filter(|m| m.time >= src_lo - EPS && m.time <= src_hi + EPS)
        .map(|m| {
            let mut nm = m.clone();
            nm.id = new_id();
            nm
        })
        .collect();
    let mut new_clips: Vec<TimelineClip> = Vec::new();
    if want_video {
        let mut c = source_clip(
            v_track.unwrap(),
            &src.asset_id,
            src.name.clone(),
            TrackKind::Video,
            place_at,
            dur,
            src_lo,
            src.src_total,
            link_id.clone(),
            markers.clone(),
        );
        c.speed = AnimatedParam::fixed(speed);
        new_clips.push(c);
    }
    if want_audio {
        let name = if link {
            format!("{} (Audio)", src.name)
        } else {
            src.name.clone()
        };
        let mut c = source_clip(
            a_track.unwrap(),
            &src.asset_id,
            name,
            TrackKind::Audio,
            place_at,
            dur,
            src_lo,
            src.src_total,
            link_id,
            markers,
        );
        c.speed = AnimatedParam::fixed(speed);
        new_clips.push(c);
    }

    state.timeline.commit_edit(new_clips, place_at, dur, false);
    state.timeline.set_playhead(place_at + dur);
    state.dock.open_panel("timeline");
    if (speed - speed_raw).abs() > EPS {
        let pct = speed * 100.0;
        return Some(format!(
            "Geschwindigkeit auf {} % begrenzt — Quelle füllt nicht exakt",
            (pct.round() as i64)
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{AudioStreamInfo, MediaAsset, MediaInfo, VideoStreamInfo};

    fn av_asset(id: &str, dur: f64, with_audio: bool) -> MediaAsset {
        MediaAsset {
            extra: Default::default(),
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
                extra: Default::default(),
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
                adjustment: None,
                speed: crate::core::animation::AnimatedParam::fixed(1.0),
                reverse: false,
                freeze: false,
                markers: vec![],
                nest_seq: None,
                multicam: None,
            blend_mode: crate::core::compose::BlendMode::default(),
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
            extra: Default::default(),
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
            adjustment: None,
            speed: crate::core::animation::AnimatedParam::fixed(1.0),
            reverse: false,
            freeze: false,
            markers: vec![],
            nest_seq: None,
            multicam: None,
            blend_mode: crate::core::compose::BlendMode::default(),
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

    // ----------------------------------------------------------- Replace

    /// Existierenden Video-Clip auf der Patch-Spur platzieren und auswählen.
    fn place_selected_clip(s: &mut AppState, asset_id: &str, start: f64, duration: f64) -> String {
        let vt = s.timeline.source_patch_track(TrackKind::Video).unwrap().to_string();
        let mut c = crate::core::timeline::test_clip(&vt);
        c.asset_id = asset_id.into();
        c.name = "alt".into();
        c.start = start;
        c.duration = duration;
        c.src_in = 3.0;
        c.src_duration = 40.0;
        let id = c.id.clone();
        s.timeline.clips.push(c);
        s.timeline.selected_clip_ids = vec![id.clone()];
        id
    }

    #[test]
    fn replace_keeps_duration_and_position() {
        // Quelle "a" (30 s) im Quellmonitor; Zielclip nutzt Medium "b".
        let mut s = setup(30.0);
        let id = place_selected_clip(&mut s, "b", 10.0, 5.0);
        // Quellmonitor-Position 8 s, Playhead 12 s (im Clip [10,15)).
        s.playback.source.position = 8.0;
        s.timeline.set_playhead(12.0);

        assert!(perform_replace(&mut s).is_none());
        let c = s.timeline.clip(&id).unwrap();
        // Medium getauscht, Position UND Dauer unverändert.
        assert_eq!(c.asset_id, "a");
        assert!((c.start - 10.0).abs() < EPS, "Position bleibt");
        assert!((c.duration - 5.0).abs() < EPS, "Dauer bleibt");
        // Match-Frame: src_anchor 8 − (Playhead 12 − Start 10) = 6.
        assert!((c.src_in - 6.0).abs() < EPS, "Match-Frame-Sync");
        assert!((c.eff_speed() - 1.0).abs() < EPS && !c.reverse && !c.freeze);
    }

    #[test]
    fn replace_keeps_effects_and_anchors_to_clip_start_when_playhead_outside() {
        let mut s = setup(30.0);
        let id = place_selected_clip(&mut s, "b", 10.0, 5.0);
        // Effekt-/Lautstärkezustand, der erhalten bleiben muss (Premiere).
        if let Some(c) = s.timeline.clips.iter_mut().find(|c| c.id == id) {
            c.gain_db = -6.0;
            c.markers.push(crate::core::marker::Marker::new(3.5));
        }
        s.playback.source.position = 8.0;
        s.timeline.set_playhead(0.0); // außerhalb des Clips ⇒ Anker = Clipanfang.

        assert!(perform_replace(&mut s).is_none());
        let c = s.timeline.clip(&id).unwrap();
        assert_eq!(c.asset_id, "a");
        assert!((c.gain_db + 6.0).abs() < EPS, "Clip-Gain bleibt erhalten");
        assert!(c.markers.is_empty(), "alte Medien-Marker entfallen");
        // Anker = Clipanfang: src_in = src_anchor 8 − (10 − 10) = 8.
        assert!((c.src_in - 8.0).abs() < EPS);
    }

    #[test]
    fn replace_uses_clip_under_playhead_without_selection() {
        let mut s = setup(30.0);
        let id = place_selected_clip(&mut s, "b", 4.0, 6.0);
        s.timeline.clear_selection();
        s.playback.source.position = 5.0;
        s.timeline.set_playhead(7.0); // im Clip [4,10), keine Auswahl.

        assert!(perform_replace(&mut s).is_none());
        let c = s.timeline.clip(&id).unwrap();
        assert_eq!(c.asset_id, "a");
        // src_in = 5 − (7 − 4) = 2 (passt, keine Klemmung).
        assert!((c.src_in - 2.0).abs() < EPS);
        assert!((c.duration - 6.0).abs() < EPS);
    }

    #[test]
    fn replace_without_target_reports() {
        let mut s = setup(30.0);
        // Keine Clips, keine Auswahl, Playhead im Leeren.
        assert!(perform_replace(&mut s).is_some());
    }

    #[test]
    fn replace_bin_asset_anchors_source_start_to_clip_start() {
        let mut s = setup(30.0);
        let id = place_selected_clip(&mut s, "b", 10.0, 5.0);
        // Kein Quellmonitor: Asset "a" nur im Bin gewählt (kein Quell-Playhead).
        s.playback.source_asset_id = None;
        s.media.selected_asset_ids = vec!["a".into()];
        s.timeline.set_playhead(12.0); // mitten im Clip — darf KEINE Warnung erzeugen.

        assert!(perform_replace(&mut s).is_none(), "Bin-Replace ohne Fehl-Warnung");
        let c = s.timeline.clip(&id).unwrap();
        assert_eq!(c.asset_id, "a");
        assert!((c.src_in - 0.0).abs() < EPS, "Quellanfang am Clipanfang");
        assert!((c.start - 10.0).abs() < EPS && (c.duration - 5.0).abs() < EPS);
    }

    #[test]
    fn replace_skips_title_generator() {
        let mut s = setup(30.0);
        let vt = s.timeline.source_patch_track(TrackKind::Video).unwrap().to_string();
        let mut c = crate::core::timeline::test_clip(&vt);
        c.asset_id = String::new();
        c.title = Some(crate::core::title::TitleSpec::default());
        c.start = 2.0;
        c.duration = 4.0;
        let id = c.id.clone();
        s.timeline.clips.push(c);
        s.timeline.selected_clip_ids = vec![id.clone()];
        s.timeline.set_playhead(3.0);

        // Quelle "a" im Monitor; Replace darf den Titel NICHT umwandeln.
        assert!(perform_replace(&mut s).is_some(), "kein ersetzbarer Clip");
        let c = s.timeline.clip(&id).unwrap();
        assert!(c.is_title(), "Titel bleibt Titel");
        assert!(c.asset_id.is_empty());
    }

    #[test]
    fn replace_reanchors_media_time_keyframes() {
        let mut s = setup(30.0);
        let id = place_selected_clip(&mut s, "b", 10.0, 5.0); // alt: src_in 3.0
        // Opacity-Keyframe bei Medienzeit 4.0 ⇒ clip-relativ +1.0 s ab Clipanfang.
        if let Some(c) = s.timeline.clips.iter_mut().find(|c| c.id == id) {
            c.fx.opacity.keyframes = vec![crate::core::animation::Keyframe {
                t: 4.0,
                value: 50.0,
                interp: Default::default(),
            }];
        }
        s.playback.source.position = 8.0;
        s.timeline.set_playhead(12.0); // neue src_in = 8 − (12 − 10) = 6 ⇒ delta +3.

        assert!(perform_replace(&mut s).is_none());
        let c = s.timeline.clip(&id).unwrap();
        assert!((c.src_in - 6.0).abs() < EPS);
        let kf = &c.fx.opacity.keyframes[0];
        // Um den In-Zeit-Versatz (+3) mitgeschoben ⇒ 7.0; clip-relativ unverändert.
        assert!((kf.t - 7.0).abs() < EPS, "Keyframe clip-relativ gehalten");
        assert!((kf.t - c.src_in - 1.0).abs() < EPS);
    }

    // -------------------------------------------------------- Fit-to-Fill

    #[test]
    fn fit_to_fill_speed_exactly_src_over_target() {
        let mut s = setup(20.0);
        s.playback.source.in_mark = Some(2.0);
        s.playback.source.out_mark = Some(10.0); // Quelldauer 8
        s.timeline.set_in_out_range(0.0, 4.0); // Zieldauer 4

        assert!(perform_fit_to_fill(&mut s).is_none());
        let v = video_clip(&s);
        assert!((v.start - 0.0).abs() < EPS);
        assert!((v.duration - 4.0).abs() < EPS, "Dauer = Zielbereich");
        assert!((v.src_in - 2.0).abs() < EPS);
        // Kernzusicherung: Speed exakt = src_len / target_len.
        assert!((v.eff_speed() - 8.0 / 4.0).abs() < EPS, "speed = 2,0");
        // Folgerung: belegte Medienspanne = Quelldauer.
        assert!((v.media_span() - 8.0).abs() < EPS, "Quell-Range füllt Ziel");
        // Audio-Partner trägt denselben Faktor.
        let a = audio_clip(&s);
        assert!((a.eff_speed() - 2.0).abs() < EPS && v.link_id == a.link_id);
    }

    #[test]
    fn fit_to_fill_slow_motion_speed() {
        let mut s = setup(20.0);
        s.playback.source.in_mark = Some(0.0);
        s.playback.source.out_mark = Some(4.0); // Quelldauer 4
        s.timeline.set_in_out_range(2.0, 10.0); // Zieldauer 8

        assert!(perform_fit_to_fill(&mut s).is_none());
        let v = video_clip(&s);
        assert!((v.eff_speed() - 4.0 / 8.0).abs() < EPS, "Zeitlupe: speed = 0,5");
        assert!((v.duration - 8.0).abs() < EPS);
        assert!((v.media_span() - 4.0).abs() < EPS);
    }

    #[test]
    fn fit_to_fill_clamps_extreme_ratio_and_warns() {
        let mut s = setup(20.0);
        s.playback.source.in_mark = Some(0.0);
        s.playback.source.out_mark = Some(16.0); // Quelldauer 16
        s.timeline.set_in_out_range(0.0, 1.0); // Zieldauer 1 ⇒ Ratio 16

        let warn = perform_fit_to_fill(&mut s);
        assert!(warn.is_some(), "Begrenzung wird gemeldet");
        let v = video_clip(&s);
        // 16,0 überschreitet MAX_CLIP_SPEED (10,0) → geklemmt.
        assert!((v.eff_speed() - MAX_CLIP_SPEED).abs() < EPS);
    }

    #[test]
    fn fit_to_fill_needs_target_range() {
        let mut s = setup(20.0);
        s.playback.source.in_mark = Some(2.0);
        s.playback.source.out_mark = Some(10.0);
        // Kein Sequenz-In/Out gesetzt.
        assert!(perform_fit_to_fill(&mut s).is_some());
    }
}

/// Alle medienzeit-verankerten Keyframes eines Clips (ClipFx-Parameter +
/// Effekt-Parameter) um `delta` Sekunden verschieben. Masken sind statisch
/// und bleiben unberührt. Ordnung bleibt erhalten (gleiche Verschiebung).
fn shift_clip_keyframes(c: &mut TimelineClip, delta: f64) {
    if delta.abs() <= EPS {
        return;
    }
    fn shift(p: &mut crate::core::animation::AnimatedParam, delta: f64) {
        for k in &mut p.keyframes {
            k.t += delta;
        }
    }
    let fx = &mut c.fx;
    shift(&mut fx.pos_x, delta);
    shift(&mut fx.pos_y, delta);
    shift(&mut fx.scale_x, delta);
    shift(&mut fx.scale_y, delta);
    shift(&mut fx.rotation, delta);
    shift(&mut fx.opacity, delta);
    shift(&mut fx.volume_db, delta);
    for eff in &mut c.effects {
        for p in &mut eff.params {
            shift(p, delta);
        }
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
        extra: Default::default(),
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
        adjustment: None,
        speed: crate::core::animation::AnimatedParam::fixed(1.0),
        reverse: false,
        freeze: false,
        markers,
        nest_seq: None,
        multicam: None,
        blend_mode: crate::core::compose::BlendMode::default(),
    }
}
