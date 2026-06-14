//! Final Cut Pro XML 1.11 — Export.
//!
//! Resolve und viele Tools importieren FCPXML zuverlässig. Struktur:
//! `resources` (ein `format` + je Medium ein `asset` mit `media-rep`), darunter
//! `library → event → project → sequence → spine`. Die unterste Video-Spur (V1)
//! ist die *primäre Storyline* (Clips sequenziell, Löcher als `<gap>`); höhere
//! Video-Spuren und alle Audio-Spuren sind *verbundene Clips* (`lane` ≠ 0) mit
//! absolutem `offset`.
//!
//! **Zeiten** sind Rationalzahlen 'N/Ds' in Sekunden auf dem Frame-Raster:
//! ein Frame = `den/num` s, also F Frames = `F·den/num` s. NTSC-Raten bleiben so
//! exakt (z. B. `frameDuration="1001/24000s"`). Werte werden gekürzt.

use super::{path_to_file_url, InteropItem, InteropMedia, InteropTimeline, InteropTrack};
use crate::core::sequence::FrameRate;

/// Die IR als FCPXML-1.11-Dokument serialisieren. `warnings` ist hier i. d. R.
/// leer (Auslassungen entstehen bereits beim IR-Aufbau), bleibt aber für ein
/// einheitliches Aufrufer-Schema erhalten.
pub fn export(ir: &InteropTimeline) -> (String, Vec<String>) {
    let rate = ir.rate;
    let warnings: Vec<String> = Vec::new();

    let total = total_frames(ir);
    let tc_format = if ir.drop_frame { "DF" } else { "NDF" };

    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str("<!DOCTYPE fcpxml>\n");
    s.push_str("<fcpxml version=\"1.11\">\n");

    // --- resources ---
    s.push_str("  <resources>\n");
    s.push_str(&format!(
        "    <format id=\"r1\" name=\"FFVideoFormat\" frameDuration=\"{}\" width=\"{}\" height=\"{}\" colorSpace=\"1-1-1 (Rec. 709)\"/>\n",
        frame_duration(rate),
        ir.width,
        ir.height,
    ));
    // Je Medium ein Asset (r2..). Index in ir.media → Ressourcen-ID.
    let asset_id = |i: usize| format!("r{}", i + 2);
    for (i, m) in ir.media.iter().enumerate() {
        s.push_str(&asset_element(&asset_id(i), m, rate));
    }
    s.push_str("  </resources>\n");

    // --- library / event / project / sequence ---
    s.push_str("  <library>\n");
    s.push_str(&format!("    <event name=\"{}\">\n", xml_attr("Editron")));
    s.push_str(&format!(
        "      <project name=\"{}\">\n",
        xml_attr(&proj_name(&ir.name))
    ));
    s.push_str(&format!(
        "        <sequence format=\"r1\" duration=\"{}\" tcStart=\"{}\" tcFormat=\"{}\" audioLayout=\"stereo\" audioRate=\"48k\">\n",
        fcp_time(total, rate),
        fcp_time(ir.global_start, rate),
        tc_format,
    ));
    s.push_str("          <spine>\n");

    // Primäre Storyline = V1 (unterste Video-Spur). Fehlt sie, ein Gap über die
    // gesamte Länge als Träger der verbundenen Clips.
    let primary = ir.video_tracks.first();
    match primary {
        Some(v1) => emit_primary(&mut s, ir, v1, rate, &asset_id),
        None => {
            s.push_str(&format!(
                "            <gap name=\"Gap\" offset=\"0s\" duration=\"{}\">\n",
                fcp_time(total.max(1), rate)
            ));
            // verbundene Clips hängen am Primär-Gap.
            emit_connected(&mut s, ir, rate, &asset_id, "              ");
            s.push_str("            </gap>\n");
            s.push_str("          </spine>\n");
            s.push_str("        </sequence>\n      </project>\n    </event>\n  </library>\n</fcpxml>\n");
            return (s, warnings);
        }
    }

    // Verbundene Clips (V2+, Audio) direkt im Spine mit lane + absolutem offset.
    emit_connected(&mut s, ir, rate, &asset_id, "            ");

    s.push_str("          </spine>\n");
    s.push_str("        </sequence>\n");
    s.push_str("      </project>\n");
    s.push_str("    </event>\n");
    s.push_str("  </library>\n");
    s.push_str("</fcpxml>\n");
    (s, warnings)
}

/// Primäre Storyline (V1) als Folge von `asset-clip` und `<gap>` ausgeben.
fn emit_primary(
    s: &mut String,
    ir: &InteropTimeline,
    v1: &InteropTrack,
    rate: FrameRate,
    asset_id: &impl Fn(usize) -> String,
) {
    let mut cursor = 0i64;
    for item in &v1.items {
        match item {
            InteropItem::Gap { frames } => {
                s.push_str(&format!(
                    "            <gap name=\"Gap\" offset=\"{}\" duration=\"{}\"/>\n",
                    fcp_time(cursor, rate),
                    fcp_time(*frames, rate)
                ));
                cursor += frames;
            }
            InteropItem::Clip(c) => {
                // Lücke zwischen Cursor und Clipstart als Gap füllen (Storyline
                // muss lückenlos sein).
                if c.rec_start > cursor {
                    s.push_str(&format!(
                        "            <gap name=\"Gap\" offset=\"{}\" duration=\"{}\"/>\n",
                        fcp_time(cursor, rate),
                        fcp_time(c.rec_start - cursor, rate)
                    ));
                }
                s.push_str(&asset_clip(
                    asset_id(c.media),
                    ir.media.get(c.media),
                    &c.name,
                    c.rec_start,
                    c.src_start,
                    c.frames,
                    None,
                    rate,
                    "            ",
                ));
                cursor = c.rec_start + c.frames;
            }
            // FCPXML-Übergänge in der Storyline sind möglich, aber heikel; wir
            // lassen die Schnitte hart (die Dissolve-Info ginge sonst leicht
            // verloren). Der Übergang wurde bereits in den Warnungen vermerkt.
            InteropItem::Transition(_) => {}
        }
    }
}

/// Verbundene Clips: V2.. (lane 1..) und Audio A1.. (lane −1..).
fn emit_connected(
    s: &mut String,
    ir: &InteropTimeline,
    rate: FrameRate,
    asset_id: &impl Fn(usize) -> String,
    indent: &str,
) {
    // Höhere Video-Spuren: V2 = lane 1, V3 = lane 2 …
    for (i, vt) in ir.video_tracks.iter().enumerate().skip(1) {
        let lane = i as i64; // V2 → 1
        for c in vt.clips() {
            s.push_str(&asset_clip(
                asset_id(c.media),
                ir.media.get(c.media),
                &c.name,
                c.rec_start,
                c.src_start,
                c.frames,
                Some(lane),
                rate,
                indent,
            ));
        }
    }
    // Audio-Spuren: A1 = lane −1, A2 = lane −2 …
    for (i, at) in ir.audio_tracks.iter().enumerate() {
        let lane = -(i as i64 + 1);
        for c in at.clips() {
            s.push_str(&asset_clip(
                asset_id(c.media),
                ir.media.get(c.media),
                &c.name,
                c.rec_start,
                c.src_start,
                c.frames,
                Some(lane),
                rate,
                indent,
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn asset_clip(
    ref_id: String,
    media: Option<&InteropMedia>,
    name: &str,
    offset: i64,
    src_start: i64,
    frames: i64,
    lane: Option<i64>,
    rate: FrameRate,
    indent: &str,
) -> String {
    let lane_attr = lane.map(|l| format!(" lane=\"{l}\"")).unwrap_or_default();
    let _ = media;
    format!(
        "{indent}<asset-clip ref=\"{ref_id}\"{lane} offset=\"{off}\" name=\"{name}\" duration=\"{dur}\" start=\"{start}\" format=\"r1\"/>\n",
        indent = indent,
        ref_id = ref_id,
        lane = lane_attr,
        off = fcp_time(offset, rate),
        name = xml_attr(name),
        dur = fcp_time(frames, rate),
        start = fcp_time(src_start, rate),
    )
}

fn asset_element(id: &str, m: &InteropMedia, seq_rate: FrameRate) -> String {
    let mrate = m.rate.unwrap_or(seq_rate);
    let duration = m
        .total_frames
        .filter(|f| *f > 0)
        .map(|f| fcp_time_at(f, mrate))
        .unwrap_or_else(|| "0s".to_string());
    let has_video = if m.has_video { 1 } else { 0 };
    let has_audio = if m.has_audio { 1 } else { 0 };
    let audio_attrs = if m.has_audio {
        " audioSources=\"1\" audioChannels=\"2\" audioRate=\"48000\""
    } else {
        ""
    };
    let src = if m.path.is_empty() {
        xml_attr(&m.name)
    } else {
        xml_attr(&path_to_file_url(&m.path))
    };
    format!(
        "    <asset id=\"{id}\" name=\"{name}\" start=\"0s\" duration=\"{dur}\" hasVideo=\"{hv}\" hasAudio=\"{ha}\"{audio} format=\"r1\">\n      <media-rep kind=\"original-media\" src=\"{src}\"/>\n    </asset>\n",
        id = id,
        name = xml_attr(&m.name),
        dur = duration,
        hv = has_video,
        ha = has_audio,
        audio = audio_attrs,
        src = src,
    )
}

fn total_frames(ir: &InteropTimeline) -> i64 {
    let mut end = 0i64;
    for t in ir.video_tracks.iter().chain(ir.audio_tracks.iter()) {
        for c in t.clips() {
            end = end.max(c.rec_end());
        }
    }
    end.max(1)
}

fn proj_name(name: &str) -> String {
    if name.trim().is_empty() {
        "Editron Sequenz".to_string()
    } else {
        name.to_string()
    }
}

/// `frameDuration` einer Rate: ein Frame = den/num Sekunden, gekürzt.
fn frame_duration(rate: FrameRate) -> String {
    fraction_string(rate.den as i64, rate.num as i64)
}

/// F Frames bei der Sequenzrate als 'N/Ds'.
fn fcp_time(frames: i64, rate: FrameRate) -> String {
    fcp_time_at(frames, rate)
}

/// F Frames bei einer beliebigen Rate als 'N/Ds': F·den/num Sekunden.
fn fcp_time_at(frames: i64, rate: FrameRate) -> String {
    if frames == 0 {
        return "0s".to_string();
    }
    fraction_string(frames * rate.den as i64, rate.num as i64)
}

/// Bruch `n/d` Sekunden gekürzt als FCPXML-Zeitstring ausgeben.
fn fraction_string(n: i64, d: i64) -> String {
    if n == 0 {
        return "0s".to_string();
    }
    let g = gcd(n.unsigned_abs(), d.unsigned_abs()).max(1) as i64;
    let (mut n, mut d) = (n / g, d / g);
    if d < 0 {
        n = -n;
        d = -d;
    }
    if d == 1 {
        format!("{n}s")
    } else {
        format!("{n}/{d}s")
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// XML-Attributwert escapen (& < > " ').
fn xml_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_duration_is_exact_for_ntsc() {
        assert_eq!(frame_duration(FrameRate::new(24000, 1001)), "1001/24000s");
        assert_eq!(frame_duration(FrameRate::PAL_25), "1/25s");
    }

    #[test]
    fn fcp_time_reduces_fraction() {
        // 240 Frames bei 24000/1001 = 240·1001/24000 s = 1001/100 s.
        assert_eq!(fcp_time(240, FrameRate::new(24000, 1001)), "1001/100s");
        // 50 Frames bei 25 = 2 s.
        assert_eq!(fcp_time(50, FrameRate::PAL_25), "2s");
        assert_eq!(fcp_time(0, FrameRate::PAL_25), "0s");
    }

    #[test]
    fn xml_escapes_special_chars() {
        assert_eq!(xml_attr("a & b <\"x\">"), "a &amp; b &lt;&quot;x&quot;&gt;");
    }
}
