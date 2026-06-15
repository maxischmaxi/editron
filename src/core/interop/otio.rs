//! OpenTimelineIO (OTIO) — Export und Import.
//!
//! OTIO ist ein JSON-Format: jedes Objekt trägt ein `OTIO_SCHEMA` (z. B.
//! `"Timeline.1"`). Zeiten sind `RationalTime` (`{rate, value}`) bzw.
//! `TimeRange` (`{start_time, duration}`). Wir bilden die Struktur direkt mit
//! `serde_json` ab (kein C++-Binding).
//!
//! **Frame-Genauigkeit:** Wir schreiben alle Timeline-/Quell-Zeiten an der
//! **Sequenzrate** als ganzzahlige Frame-Werte. Da Editrons Clips bereits auf
//! dem Frame-Raster der Sequenz liegen, ist das ganzzahlig und der Roundtrip
//! Editron→OTIO→Editron damit verlustfrei — auch an 23,976/29,97. Beim Lesen
//! rechnen wir jede `RationalTime` über ihre EIGENE (als exakter Bruch
//! rekonstruierte) Rate in Sekunden und dann frame-genau auf die Sequenzrate.
//!
//! Auflösung und Drop-Frame trägt OTIO nicht im Kern; wir hinterlegen sie in
//! `metadata.Editron` (Fremdtools ignorieren unbekannte Metadaten) — so bleibt
//! der Editron-Roundtrip vollständig verlustfrei.

use super::{
    file_url_to_path, path_to_file_url, InteropClip, InteropItem, InteropMarker, InteropMedia,
    InteropTimeline, InteropTrack, InteropTransition,
};
use crate::core::marker::MarkerColor;
use crate::core::sequence::FrameRate;
use crate::core::timeline::TrackKind;
use crate::core::transitions::TransitionKind;
use serde_json::{json, Value};

// ---------------------------------------------------------------- Export

/// Die IR als OTIO-JSON (eingerückt) serialisieren. Video-Spuren stehen von
/// unten (V1) nach oben — in OTIO ist der erste Stack-Eintrag die Basisebene.
pub fn export(ir: &InteropTimeline) -> String {
    let fps = ir.rate.fps();
    let mut children: Vec<Value> = Vec::new();
    for t in &ir.video_tracks {
        children.push(track_json(ir, t, fps));
    }
    for t in &ir.audio_tracks {
        children.push(track_json(ir, t, fps));
    }
    let markers: Vec<Value> = ir.markers.iter().map(|m| marker_json(m, fps)).collect();
    let stack = json!({
        "OTIO_SCHEMA": "Stack.1",
        "metadata": {},
        "name": "tracks",
        "source_range": Value::Null,
        "effects": [],
        "markers": markers,
        "enabled": true,
        "children": children,
    });
    let timeline = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "metadata": {
            "Editron": { "width": ir.width, "height": ir.height, "dropFrame": ir.drop_frame }
        },
        "name": ir.name,
        "global_start_time": rational_time(ir.global_start, fps),
        "tracks": stack,
    });
    serde_json::to_string_pretty(&timeline).unwrap_or_else(|_| "{}".to_string())
}

fn track_json(ir: &InteropTimeline, track: &InteropTrack, fps: f64) -> Value {
    let kind = match track.kind {
        TrackKind::Audio => "Audio",
        _ => "Video",
    };
    let children: Vec<Value> = track
        .items
        .iter()
        .map(|item| match item {
            InteropItem::Gap { frames } => gap_json(*frames, fps),
            InteropItem::Clip(c) => clip_json(ir, c, fps),
            InteropItem::Transition(t) => transition_json(t, fps),
        })
        .collect();
    json!({
        "OTIO_SCHEMA": "Track.1",
        "metadata": {},
        "name": track.name,
        "source_range": Value::Null,
        "effects": [],
        "markers": [],
        "enabled": true,
        "kind": kind,
        "children": children,
    })
}

fn clip_json(ir: &InteropTimeline, c: &InteropClip, fps: f64) -> Value {
    let media_ref = ir
        .media
        .get(c.media)
        .map(external_ref)
        .unwrap_or(Value::Null);
    json!({
        "OTIO_SCHEMA": "Clip.1",
        "metadata": {},
        "name": c.name,
        "source_range": time_range(c.src_start, c.frames, fps),
        "effects": [],
        "markers": [],
        "enabled": c.enabled,
        "media_reference": media_ref,
    })
}

fn gap_json(frames: i64, fps: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "Gap.1",
        "metadata": {},
        "name": "",
        "source_range": time_range(0, frames, fps),
        "effects": [],
        "markers": [],
        "enabled": true,
    })
}

fn transition_json(t: &InteropTransition, fps: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "Transition.1",
        "metadata": {},
        "name": t.kind.label(),
        "transition_type": "SMPTE_Dissolve",
        "in_offset": rational_time(t.pre, fps),
        "out_offset": rational_time(t.post, fps),
    })
}

fn external_ref(media: &InteropMedia) -> Value {
    let available = match (media.rate, media.total_frames) {
        (Some(r), Some(total)) if total > 0 => time_range(0, total, r.fps()),
        _ => Value::Null,
    };
    let url = if media.path.is_empty() {
        media.name.clone()
    } else {
        path_to_file_url(&media.path)
    };
    json!({
        "OTIO_SCHEMA": "ExternalReference.1",
        "metadata": {},
        "name": media.name,
        "available_range": available,
        "target_url": url,
    })
}

fn marker_json(m: &InteropMarker, fps: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "Marker.2",
        "metadata": {},
        "name": m.name,
        "color": otio_color(m.color),
        "marked_range": time_range(m.frame, m.duration, fps),
        "comment": m.note,
    })
}

fn rational_time(value: i64, fps: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "RationalTime.1",
        "rate": fps,
        "value": value as f64,
    })
}

fn time_range(start: i64, duration: i64, fps: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "TimeRange.1",
        "start_time": rational_time(start, fps),
        "duration": rational_time(duration, fps),
    })
}

fn otio_color(c: MarkerColor) -> &'static str {
    match c {
        MarkerColor::Green => "GREEN",
        MarkerColor::Red => "RED",
        MarkerColor::Purple => "PURPLE",
        MarkerColor::Orange => "ORANGE",
        MarkerColor::Yellow => "YELLOW",
        MarkerColor::White => "WHITE",
        MarkerColor::Blue => "BLUE",
        MarkerColor::Cyan => "CYAN",
    }
}

fn color_from_otio(s: &str) -> MarkerColor {
    match s.to_ascii_uppercase().as_str() {
        "RED" => MarkerColor::Red,
        "PURPLE" | "MAGENTA" => MarkerColor::Purple,
        "ORANGE" => MarkerColor::Orange,
        "YELLOW" => MarkerColor::Yellow,
        "WHITE" => MarkerColor::White,
        "BLUE" => MarkerColor::Blue,
        "CYAN" => MarkerColor::Cyan,
        _ => MarkerColor::Green,
    }
}

// ---------------------------------------------------------------- Import

/// Eine OTIO-JSON-Datei in die IR parsen. Liefert IR + Auslassungs-Warnungen.
pub fn parse(text: &str) -> Result<(InteropTimeline, Vec<String>), String> {
    let root: Value = serde_json::from_str(text).map_err(|e| format!("Kein gültiges JSON: {e}"))?;
    let schema = root.get("OTIO_SCHEMA").and_then(|v| v.as_str()).unwrap_or("");
    if !schema.starts_with("Timeline") {
        return Err(format!(
            "Keine OTIO-Timeline: OTIO_SCHEMA ist '{schema}', erwartet 'Timeline.*'."
        ));
    }
    let mut warnings: Vec<String> = Vec::new();

    let name = root.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();

    // Sequenzrate bestimmen: global_start_time hat Vorrang, sonst die erste
    // RationalTime, die irgendwo auftaucht.
    let global_fps = root
        .get("global_start_time")
        .and_then(rate_of_rational)
        .or_else(|| first_rate(&root))
        .unwrap_or(25.0);
    let rate = FrameRate::from_fps(global_fps).unwrap_or(FrameRate::PAL_25);

    let global_start = root
        .get("global_start_time")
        .map(|rt| rt_to_seq_frames(rt, rate))
        .unwrap_or(0);

    // Auflösung/Drop-Frame aus Editron-Metadaten (falls vorhanden).
    let meta = root.pointer("/metadata/Editron");
    let width = meta
        .and_then(|m| m.get("width"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(1920);
    let height = meta
        .and_then(|m| m.get("height"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(1080);
    let drop_frame = meta
        .and_then(|m| m.get("dropFrame"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let stack = root
        .get("tracks")
        .ok_or_else(|| "OTIO-Timeline ohne 'tracks'-Stack".to_string())?;

    let mut media: Vec<InteropMedia> = Vec::new();
    let mut media_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut video_tracks: Vec<InteropTrack> = Vec::new();
    let mut audio_tracks: Vec<InteropTrack> = Vec::new();

    let empty = Vec::new();
    let track_children = stack.get("children").and_then(|v| v.as_array()).unwrap_or(&empty);
    for tv in track_children {
        let kind_str = tv.get("kind").and_then(|v| v.as_str()).unwrap_or("Video");
        let kind = if kind_str.eq_ignore_ascii_case("Audio") {
            TrackKind::Audio
        } else {
            TrackKind::Video
        };
        let tname = tv.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let track = parse_track(
            tv,
            kind,
            &tname,
            rate,
            &mut media,
            &mut media_index,
            &mut warnings,
        );
        match kind {
            TrackKind::Audio => audio_tracks.push(track),
            _ => video_tracks.push(track),
        }
    }
    // In der IR stehen Video-Spuren V1..Vn (unten→oben). OTIO listet den
    // untersten Stack-Eintrag zuerst — also bereits V1..Vn. Passt.

    // Marker am Stack (Timeline-Marker).
    let mut markers: Vec<InteropMarker> = Vec::new();
    if let Some(ms) = stack.get("markers").and_then(|v| v.as_array()) {
        for m in ms {
            if let Some(im) = parse_marker(m, rate) {
                markers.push(im);
            }
        }
    }

    let ir = InteropTimeline {
        name,
        rate,
        drop_frame: drop_frame && rate.supports_drop_frame(),
        width,
        height,
        global_start,
        media,
        video_tracks,
        audio_tracks,
        markers,
    };
    Ok((ir, warnings))
}

#[allow(clippy::too_many_arguments)]
fn parse_track(
    tv: &Value,
    kind: TrackKind,
    name: &str,
    rate: FrameRate,
    media: &mut Vec<InteropMedia>,
    media_index: &mut std::collections::HashMap<String, usize>,
    warnings: &mut Vec<String>,
) -> InteropTrack {
    let mut items: Vec<InteropItem> = Vec::new();
    let empty = Vec::new();
    let children = tv.get("children").and_then(|v| v.as_array()).unwrap_or(&empty);
    for child in children {
        let schema = child.get("OTIO_SCHEMA").and_then(|v| v.as_str()).unwrap_or("");
        if schema.starts_with("Gap") {
            let frames = child
                .get("source_range")
                .and_then(|sr| sr.get("duration"))
                .map(|d| rt_to_seq_frames(d, rate))
                .unwrap_or(0);
            if frames > 0 {
                items.push(InteropItem::Gap { frames });
            }
        } else if schema.starts_with("Transition") {
            let pre = child.get("in_offset").map(|r| rt_to_seq_frames(r, rate)).unwrap_or(0);
            let post = child.get("out_offset").map(|r| rt_to_seq_frames(r, rate)).unwrap_or(0);
            let ttype = child
                .get("transition_type")
                .and_then(|v| v.as_str())
                .unwrap_or("SMPTE_Dissolve");
            if ttype != "SMPTE_Dissolve" {
                warnings.push(format!(
                    "Übergangstyp '{ttype}' wird als Überblendung importiert."
                ));
            }
            let kind_tr = if kind == TrackKind::Audio {
                TransitionKind::ConstantPower
            } else {
                TransitionKind::CrossDissolve
            };
            items.push(InteropItem::Transition(InteropTransition {
                kind: kind_tr,
                // saturating: feindliche/korrupte OTIO-Offsets dürfen nicht paniken.
                frames: pre.saturating_add(post),
                pre,
                post,
            }));
        } else if schema.starts_with("Clip") {
            let cname = child.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let sr = child.get("source_range");
            let src_start = sr
                .and_then(|s| s.get("start_time"))
                .map(|r| rt_to_seq_frames(r, rate))
                .unwrap_or(0);
            let frames = sr
                .and_then(|s| s.get("duration"))
                .map(|d| rt_to_seq_frames(d, rate))
                .unwrap_or(0)
                .max(1);
            let enabled = child.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
            if child
                .get("effects")
                .and_then(|v| v.as_array())
                .is_some_and(|a| !a.is_empty())
            {
                warnings.push(format!(
                    "Effekte an Clip '{}' werden ignoriert (Schnitt bleibt erhalten).",
                    if cname.is_empty() { "Clip" } else { &cname }
                ));
            }
            let media_ref = clip_media_reference(child);
            let media_idx = intern_media(media, media_index, media_ref, &cname);
            items.push(InteropItem::Clip(InteropClip {
                name: cname,
                media: media_idx,
                rec_start: 0, // wird unten kumulativ gesetzt
                src_start,
                frames,
                enabled,
            }));
        } else if !schema.is_empty() {
            warnings.push(format!("OTIO-Element '{schema}' wird ignoriert."));
        }
    }
    // Record-Positionen kumulativ aus Gaps + Clips ableiten (Übergänge zählen
    // nicht zur Spurzeit).
    assign_rec_positions(&mut items);
    InteropTrack {
        kind,
        name: name.to_string(),
        items,
    }
}

/// Record-Start jedes Clips aus der Folge von Gaps/Clips berechnen.
fn assign_rec_positions(items: &mut [InteropItem]) {
    let mut cursor = 0i64;
    for item in items.iter_mut() {
        match item {
            InteropItem::Gap { frames } => cursor += *frames,
            InteropItem::Clip(c) => {
                c.rec_start = cursor;
                cursor += c.frames;
            }
            InteropItem::Transition(_) => {}
        }
    }
}

/// Die Mediendatei-Referenz eines Clips lesen (Clip.1 `media_reference` oder
/// Clip.2 `media_references`/`active_media_reference_key`).
fn clip_media_reference(clip: &Value) -> Option<&Value> {
    if let Some(r) = clip.get("media_reference") {
        if !r.is_null() {
            return Some(r);
        }
    }
    if let Some(map) = clip.get("media_references").and_then(|v| v.as_object()) {
        let key = clip
            .get("active_media_reference_key")
            .and_then(|v| v.as_str())
            .unwrap_or("DEFAULT_MEDIA");
        return map.get(key).or_else(|| map.values().next());
    }
    None
}

/// Eine Medienreferenz in die Tabelle aufnehmen (dedupliziert per URL/Name).
fn intern_media(
    media: &mut Vec<InteropMedia>,
    media_index: &mut std::collections::HashMap<String, usize>,
    media_ref: Option<&Value>,
    clip_name: &str,
) -> usize {
    let (path, ref_name, rate, total) = match media_ref {
        Some(r) => {
            let url = r.get("target_url").and_then(|v| v.as_str()).unwrap_or("");
            let path = if url.is_empty() {
                String::new()
            } else {
                file_url_to_path(url)
            };
            let ref_name = r.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let avail = r.get("available_range");
            let rate = avail.and_then(|a| a.get("start_time")).and_then(rate_of_rational);
            let frate = rate.and_then(FrameRate::from_fps);
            let total = avail
                .and_then(|a| a.get("duration"))
                .zip(frate)
                .map(|(d, fr)| rt_to_seq_frames(d, fr).max(0));
            (path, ref_name, frate, total)
        }
        None => (String::new(), String::new(), None, None),
    };
    let key = if !path.is_empty() {
        path.clone()
    } else if !ref_name.is_empty() {
        format!("name:{ref_name}")
    } else {
        format!("clip:{clip_name}")
    };
    if let Some(&i) = media_index.get(&key) {
        // has_audio/has_video je nach Spurkontext zusammenführen passiert nicht
        // (OTIO trennt nicht), wir lassen die erste Beobachtung stehen.
        return i;
    }
    let display = if !ref_name.is_empty() {
        ref_name
    } else if !path.is_empty() {
        std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| clip_name.to_string())
    } else {
        clip_name.to_string()
    };
    let idx = media.len();
    media.push(InteropMedia {
        name: display,
        path,
        reel: String::new(),
        rate,
        total_frames: total,
        // OTIO unterscheidet Video/Audio nicht auf Referenzebene; wir nehmen an,
        // beide könnten vorhanden sein — der Relink/Probe korrigiert das.
        has_video: true,
        has_audio: true,
    });
    media_index.insert(key, idx);
    idx
}

fn parse_marker(m: &Value, rate: FrameRate) -> Option<InteropMarker> {
    let mr = m.get("marked_range")?;
    let frame = mr.get("start_time").map(|r| rt_to_seq_frames(r, rate)).unwrap_or(0);
    let duration = mr.get("duration").map(|r| rt_to_seq_frames(r, rate)).unwrap_or(0);
    let name = m.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let note = m
        .get("comment")
        .or_else(|| m.get("note"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let color = m
        .get("color")
        .and_then(|v| v.as_str())
        .map(color_from_otio)
        .unwrap_or_default();
    Some(InteropMarker {
        frame,
        duration,
        name,
        note,
        color,
    })
}

// ----------------------------------------------------- RationalTime-Helfer

/// Rate (fps) einer RationalTime auslesen.
fn rate_of_rational(rt: &Value) -> Option<f64> {
    rt.get("rate").and_then(|v| v.as_f64()).filter(|r| *r > 0.0)
}

/// Eine RationalTime über ihre EIGENE Rate in Sekunden und dann frame-genau
/// auf die Sequenzrate umrechnen (driftfrei, verlustfrei bei gleicher Rate).
fn rt_to_seq_frames(rt: &Value, seq: FrameRate) -> i64 {
    let value = rt.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let own_fps = rate_of_rational(rt).unwrap_or_else(|| seq.fps());
    let own = FrameRate::from_fps(own_fps).unwrap_or(seq);
    let seconds = own.time_of_frame(value);
    seq.frame_round(seconds)
}

/// Erste irgendwo im Baum auftauchende RationalTime-Rate (Fallback für die
/// Sequenzrate, wenn `global_start_time` fehlt).
fn first_rate(v: &Value) -> Option<f64> {
    match v {
        Value::Object(map) => {
            if map.get("OTIO_SCHEMA").and_then(|s| s.as_str()) == Some("RationalTime.1") {
                if let Some(r) = map.get("rate").and_then(|r| r.as_f64()) {
                    if r > 0.0 {
                        return Some(r);
                    }
                }
            }
            map.values().find_map(first_rate)
        }
        Value::Array(arr) => arr.iter().find_map(first_rate),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rt_conversion_is_lossless_at_same_rate() {
        let r = FrameRate::new(24000, 1001);
        let rt = rational_time(48, r.fps());
        assert_eq!(rt_to_seq_frames(&rt, r), 48);
    }

    #[test]
    fn export_rejects_non_timeline_on_parse() {
        assert!(parse("{\"OTIO_SCHEMA\":\"Clip.1\"}").is_err());
        assert!(parse("nicht json").is_err());
    }
}
