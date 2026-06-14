//! CMX-3600-EDL — Export und Import.
//!
//! Eine EDL ist eine nummerierte Liste von Schnitt-Ereignissen. Jedes Ereignis
//! trägt Reel/Tape-Name, Kanal (V, A, A2 …), Übergangsart (C = Schnitt,
//! D = Dissolve mit Frame-Länge) sowie Quell- und Record-Timecode. Es gibt
//! GENAU eine Video-Spur — Editrons höhere Video-Spuren werden gemeldet, nicht
//! still verworfen.
//!
//! **Timecode:** Quell- und Record-TC laufen an der Sequenzrate (CMX-3600 kennt
//! nur eine Rate je EDL). Wir nutzen die driftfreie [`crate::core::timecode`]-
//! Mathematik. Die EDL selbst trägt KEINE Bildrate — nur DROP/NON-DROP FRAME;
//! beim Import nehmen wir 29,97 (DF) bzw. 25 (NDF, europäischer Standard) an und
//! melden das, damit der Nutzer die Sequenzrate prüfen kann.
//!
//! Out-Punkte sind in CMX exklusiv (ein Frame hinter dem letzten Frame).

use super::{
    reel_from_name, InteropClip, InteropItem, InteropMedia, InteropTimeline, InteropTrack,
    InteropTransition,
};
use crate::core::sequence::FrameRate;
use crate::core::timecode::format_frames;
use crate::core::timeline::TrackKind;
use crate::core::transitions::TransitionKind;

// ---------------------------------------------------------------- Export

/// Die IR als CMX-3600-EDL serialisieren. `warnings` sammelt Auslassungen.
pub fn export(ir: &InteropTimeline) -> (String, Vec<String>) {
    let rate = ir.rate;
    let df = ir.drop_frame && rate.supports_drop_frame();
    let mut warnings: Vec<String> = Vec::new();

    let mut out = String::new();
    out.push_str(&format!("TITLE: {}\n", sanitize_title(&ir.name)));
    out.push_str(if df {
        "FCM: DROP FRAME\n"
    } else {
        "FCM: NON-DROP FRAME\n"
    });

    // Genau EINE Video-Spur: die erste nicht-leere (bevorzugt V1).
    let chosen_video = ir
        .video_tracks
        .iter()
        .find(|t| t.clips().next().is_some());
    let extra_video = ir
        .video_tracks
        .iter()
        .filter(|t| t.clips().next().is_some())
        .count()
        .saturating_sub(1);
    if extra_video > 0 {
        warnings.push(format!(
            "EDL kennt nur eine Video-Spur — {extra_video} weitere Video-Spur(en) ausgelassen."
        ));
    }

    // Ereignisse einsammeln (Video + Audio), dann nach Record-In sortieren.
    let mut events: Vec<Event> = Vec::new();
    if let Some(vt) = chosen_video {
        collect_events(ir, vt, "V", rate, &mut events, &mut warnings);
    }
    for (idx, at) in ir.audio_tracks.iter().enumerate() {
        let chan = audio_channel(idx);
        collect_events(ir, at, &chan, rate, &mut events, &mut warnings);
    }
    events.sort_by(|a, b| {
        a.rec_in
            .cmp(&b.rec_in)
            .then_with(|| a.channel.cmp(&b.channel))
    });

    let mut number = 1;
    for ev in &events {
        out.push_str(&ev.format(number, rate, df));
        if !ev.clip_name.is_empty() {
            out.push_str(&format!("* FROM CLIP NAME: {}\n", ev.clip_name));
        }
        number += 1;
    }

    (out, warnings)
}

/// Ein EDL-Ereignis in absoluten Frames (Sequenzrate).
struct Event {
    reel: String,
    channel: String,
    /// Dissolve-Länge in Frames (>0 ⇒ 'D', sonst 'C').
    dissolve: i64,
    src_in: i64,
    src_out: i64,
    rec_in: i64,
    rec_out: i64,
    clip_name: String,
}

impl Event {
    fn format(&self, number: u32, rate: FrameRate, df: bool) -> String {
        let tc = |f: i64| format_frames(f.max(0) as u64, rate, df);
        if self.dissolve > 0 {
            format!(
                "{:03}  {:<8} {:<5} D    {:03} {} {} {} {}\n",
                number,
                self.reel,
                self.channel,
                self.dissolve,
                tc(self.src_in),
                tc(self.src_out),
                tc(self.rec_in),
                tc(self.rec_out),
            )
        } else {
            format!(
                "{:03}  {:<8} {:<5} C        {} {} {} {}\n",
                number,
                self.reel,
                self.channel,
                tc(self.src_in),
                tc(self.src_out),
                tc(self.rec_in),
                tc(self.rec_out),
            )
        }
    }
}

/// Ereignisse einer Spur erzeugen; Übergänge als Dissolve-Ereignisse abbilden
/// (Standard-CMX-Form: der ausgehende Clip endet am Dissolve-Beginn, der
/// eingehende trägt das `D nnn`).
fn collect_events(
    ir: &InteropTimeline,
    track: &InteropTrack,
    channel: &str,
    _rate: FrameRate,
    events: &mut Vec<Event>,
    warnings: &mut Vec<String>,
) {
    let mut pending: Option<InteropTransition> = None;
    for item in &track.items {
        match item {
            InteropItem::Gap { .. } => pending = None,
            InteropItem::Transition(t) => pending = Some(*t),
            InteropItem::Clip(c) => {
                let media = ir.media.get(c.media);
                let reel = media
                    .map(|m| {
                        if m.reel.is_empty() {
                            reel_from_name(&m.name)
                        } else {
                            m.reel.clone()
                        }
                    })
                    .unwrap_or_else(|| "AX".to_string());
                let name = media.map(|m| m.name.clone()).unwrap_or_else(|| c.name.clone());

                if let Some(tr) = pending.take() {
                    if tr.kind != TransitionKind::CrossDissolve && !tr.kind.is_audio() {
                        warnings.push(format!(
                            "Übergang '{}' wird als Dissolve exportiert.",
                            tr.kind.label()
                        ));
                    }
                    // Standard-CMX-Dissolve: das ausgehende Ereignis am Dissolve-
                    // Beginn (cut − pre) kürzen, das eingehende mit 'D nnn' tragen.
                    let pre = tr.pre.clamp(0, c.frames);
                    if let Some(prev) = events.iter_mut().rev().find(|e| e.channel == channel) {
                        let new_rec_out = (prev.rec_out - pre).max(prev.rec_in);
                        let shrink = prev.rec_out - new_rec_out;
                        prev.rec_out = new_rec_out;
                        prev.src_out -= shrink;
                    }
                    let src_in = incoming_src_in(c.src_start, pre, &name, warnings);
                    events.push(Event {
                        reel,
                        channel: channel.to_string(),
                        dissolve: tr.frames.max(1),
                        src_in,
                        src_out: c.src_start + c.frames,
                        rec_in: c.rec_start - pre,
                        rec_out: c.rec_start + c.frames,
                        clip_name: name,
                    });
                } else {
                    events.push(Event {
                        reel,
                        channel: channel.to_string(),
                        dissolve: 0,
                        src_in: c.src_start,
                        src_out: c.src_start + c.frames,
                        rec_in: c.rec_start,
                        rec_out: c.rec_start + c.frames,
                        clip_name: name,
                    });
                }
            }
        }
    }
}

/// Quell-In des eingehenden Dissolve-Clips (um `pre` Frames nach vorn gezogen,
/// auf den Quellanfang begrenzt).
fn incoming_src_in(src_start: i64, pre: i64, name: &str, warnings: &mut Vec<String>) -> i64 {
    if src_start - pre < 0 {
        warnings.push(format!(
            "Dissolve an '{name}' überschreitet den Quellanfang — auf 0 begrenzt."
        ));
    }
    (src_start - pre).max(0)
}

fn audio_channel(idx: usize) -> String {
    match idx {
        0 => "A".to_string(),
        n => format!("A{}", n + 1),
    }
}

fn sanitize_title(name: &str) -> String {
    let t: String = name.chars().filter(|c| *c != '\n' && *c != '\r').collect();
    if t.trim().is_empty() {
        "Editron Sequenz".to_string()
    } else {
        t
    }
}

// ---------------------------------------------------------------- Import

/// Eine EDL in die IR parsen. Liefert IR + Warnungen (u. a. die angenommene Rate).
pub fn parse(text: &str) -> Result<(InteropTimeline, Vec<String>), String> {
    let mut warnings: Vec<String> = Vec::new();
    let mut title = String::new();
    let mut drop_frame = false;

    // Erst FCM/TITLE einsammeln (Rate-Annahme hängt an DROP/NON-DROP).
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("TITLE:") {
            title = rest.trim().to_string();
        } else if l.eq_ignore_ascii_case("FCM: DROP FRAME") {
            drop_frame = true;
        } else if l.eq_ignore_ascii_case("FCM: NON-DROP FRAME") {
            drop_frame = false;
        }
    }
    let rate = if drop_frame {
        FrameRate::new(30000, 1001)
    } else {
        FrameRate::PAL_25
    };
    warnings.push(format!(
        "EDL trägt keine Bildrate — als {} fps{} interpretiert; bei Bedarf die Sequenzrate anpassen.",
        rate.label(),
        if drop_frame { " (Drop-Frame)" } else { "" }
    ));

    // Ereignisse parsen.
    let mut raw: Vec<RawEvent> = Vec::new();
    let mut last_idx: Option<usize> = None;
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if let Some(rest) = l.strip_prefix('*') {
            // Kommentar — Clipname an das letzte Ereignis hängen.
            let c = rest.trim();
            if let Some(name) = c
                .strip_prefix("FROM CLIP NAME:")
                .or_else(|| c.strip_prefix("FROM CLIP:"))
                .or_else(|| c.strip_prefix("SOURCE FILE:"))
            {
                if let Some(i) = last_idx {
                    if raw[i].clip_name.is_empty() {
                        raw[i].clip_name = name.trim().to_string();
                    }
                }
            }
            continue;
        }
        if let Some(ev) = parse_event_line(l, rate, drop_frame, &mut warnings) {
            raw.push(ev);
            last_idx = Some(raw.len() - 1);
        }
    }

    // Ereignisse nach Kanal in Spuren einsortieren.
    let mut video_items: Vec<InteropItem> = Vec::new();
    let mut audio_tracks_items: std::collections::BTreeMap<String, Vec<InteropItem>> =
        std::collections::BTreeMap::new();
    let mut media: Vec<InteropMedia> = Vec::new();
    let mut media_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for ev in &raw {
        let media_idx = intern_media(&mut media, &mut media_index, ev);
        let is_audio = ev.channel.starts_with('A');
        let frames = (ev.rec_out - ev.rec_in).max(1);
        let clip = InteropClip {
            name: if ev.clip_name.is_empty() {
                ev.reel.clone()
            } else {
                ev.clip_name.clone()
            },
            media: media_idx,
            rec_start: ev.rec_in,
            src_start: ev.src_in,
            frames,
            enabled: true,
        };
        if is_audio {
            let entry = audio_tracks_items.entry(ev.channel.clone()).or_default();
            if ev.dissolve > 0 {
                entry.push(InteropItem::Transition(dissolve_transition(ev.dissolve, true)));
            }
            entry.push(InteropItem::Clip(clip));
        } else {
            if ev.dissolve > 0 {
                video_items.push(InteropItem::Transition(dissolve_transition(ev.dissolve, false)));
            }
            video_items.push(InteropItem::Clip(clip));
        }
    }

    let video_tracks = if video_items.is_empty() {
        Vec::new()
    } else {
        vec![InteropTrack {
            kind: TrackKind::Video,
            name: "V1".to_string(),
            items: video_items,
        }]
    };
    let audio_tracks: Vec<InteropTrack> = audio_tracks_items
        .into_iter()
        .enumerate()
        .map(|(i, (_chan, items))| InteropTrack {
            kind: TrackKind::Audio,
            name: format!("A{}", i + 1),
            items,
        })
        .collect();

    if video_tracks.is_empty() && audio_tracks.is_empty() {
        return Err("EDL enthält keine lesbaren Schnitt-Ereignisse".to_string());
    }

    let ir = InteropTimeline {
        name: if title.trim().is_empty() {
            "EDL-Import".to_string()
        } else {
            title
        },
        rate,
        drop_frame,
        width: 1920,
        height: 1080,
        global_start: 0,
        media,
        video_tracks,
        audio_tracks,
        markers: Vec::new(),
    };
    Ok((ir, warnings))
}

struct RawEvent {
    reel: String,
    channel: String,
    dissolve: i64,
    src_in: i64,
    rec_in: i64,
    rec_out: i64,
    clip_name: String,
}

fn parse_event_line(
    line: &str,
    rate: FrameRate,
    df: bool,
    warnings: &mut Vec<String>,
) -> Option<RawEvent> {
    let tok: Vec<&str> = line.split_whitespace().collect();
    if tok.len() < 8 {
        return None;
    }
    // tok[0] muss eine Ereignisnummer sein.
    if tok[0].parse::<u32>().is_err() {
        return None;
    }
    let reel = tok[1].to_string();
    let channel = normalize_channel(tok[2]);
    let trans = tok[3];
    let (dissolve, off) = if trans.eq_ignore_ascii_case("C") {
        (0i64, 4usize)
    } else if trans.eq_ignore_ascii_case("D") {
        // tok[4] = Dissolve-Länge in Frames.
        let n = tok.get(4).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
        (n, 5usize)
    } else {
        // Wipe/Key u. Ä. — als Schnitt behandeln.
        warnings.push(format!("EDL-Übergang '{trans}' als Schnitt importiert."));
        if tok.get(4).map(|s| s.parse::<i64>().is_ok()).unwrap_or(false) {
            (0i64, 5usize)
        } else {
            (0i64, 4usize)
        }
    };
    if tok.len() < off + 4 {
        return None;
    }
    let src_in = parse_tc(tok[off], rate, df)?;
    // src_out (tok[off+1]) wird nur zur Formatvalidierung geparst; die Länge
    // ergibt sich aus rec_out − rec_in.
    let _src_out = parse_tc(tok[off + 1], rate, df)?;
    let rec_in = parse_tc(tok[off + 2], rate, df)?;
    let rec_out = parse_tc(tok[off + 3], rate, df)?;
    Some(RawEvent {
        reel,
        channel,
        dissolve,
        src_in,
        rec_in,
        rec_out,
        clip_name: String::new(),
    })
}

/// Kanal normalisieren: führendes V/A behalten; 'AA'/'B' auf A1 abbilden.
fn normalize_channel(chan: &str) -> String {
    let up = chan.to_ascii_uppercase();
    if up.starts_with('V') {
        "V".to_string()
    } else if up == "AA" || up == "A" || up == "B" {
        "A".to_string()
    } else if up.starts_with('A') {
        up
    } else {
        "V".to_string()
    }
}

fn dissolve_transition(frames: i64, audio: bool) -> InteropTransition {
    InteropTransition {
        kind: if audio {
            TransitionKind::ConstantPower
        } else {
            TransitionKind::CrossDissolve
        },
        frames,
        // EDL-Dissolves beginnen am Schnitt und laufen in den eingehenden Clip.
        pre: 0,
        post: frames,
    }
}

fn intern_media(
    media: &mut Vec<InteropMedia>,
    media_index: &mut std::collections::HashMap<String, usize>,
    ev: &RawEvent,
) -> usize {
    let name = if ev.clip_name.is_empty() {
        ev.reel.clone()
    } else {
        ev.clip_name.clone()
    };
    let key = name.to_lowercase();
    if let Some(&i) = media_index.get(&key) {
        let entry = &mut media[i];
        if ev.channel.starts_with('A') {
            entry.has_audio = true;
        } else {
            entry.has_video = true;
        }
        return i;
    }
    let is_audio = ev.channel.starts_with('A');
    // Trägt der Clipname eine Endung, gilt er als Dateiname (sonst nur Reel).
    let has_ext = std::path::Path::new(&name).extension().is_some();
    let idx = media.len();
    media.push(InteropMedia {
        name: name.clone(),
        path: if has_ext { name.clone() } else { String::new() },
        reel: ev.reel.clone(),
        rate: None,
        total_frames: None,
        has_video: !is_audio,
        has_audio: is_audio,
    });
    media_index.insert(key, idx);
    idx
}

/// SMPTE-Timecode ("HH:MM:SS:FF" oder "…;FF") in absolute Frames parsen.
fn parse_tc(tc: &str, rate: FrameRate, df: bool) -> Option<i64> {
    let parts: Vec<&str> = tc.split([':', ';']).collect();
    if parts.len() != 4 {
        return None;
    }
    let h: u64 = parts[0].parse().ok()?;
    let m: u64 = parts[1].parse().ok()?;
    let s: u64 = parts[2].parse().ok()?;
    let f: u64 = parts[3].parse().ok()?;
    let drop = df && rate.supports_drop_frame();
    Some(crate::core::timecode::frames_of(h, m, s, f, rate, drop) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tc_roundtrip_non_drop() {
        let r = FrameRate::PAL_25;
        let f = parse_tc("00:00:04:00", r, false).unwrap();
        assert_eq!(f, 100);
        assert_eq!(format_frames(f as u64, r, false), "00:00:04:00");
    }

    #[test]
    fn tc_drop_frame_parses() {
        let r = FrameRate::new(30000, 1001);
        let f = parse_tc("01:00:00;00", r, true).unwrap();
        assert_eq!(format_frames(f as u64, r, true), "01:00:00;00");
    }
}
