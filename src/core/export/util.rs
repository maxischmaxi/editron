use super::*;

// =========================================================== Format-Helfer

/// fps als exaktes ffmpeg-Argument (NTSC-Raten als Bruch).
pub fn fps_arg(fps: f64) -> String {
    const NTSC: [(f64, &str); 3] = [
        (23.976, "24000/1001"),
        (29.97, "30000/1001"),
        (59.94, "60000/1001"),
    ];
    for (v, s) in NTSC {
        if (fps - v).abs() < 0.005 {
            return s.to_string();
        }
    }
    if (fps - fps.round()).abs() < 1e-9 {
        format!("{}", fps.round() as u64)
    } else {
        format!("{fps}")
    }
}

/// Decode-Parameter einer nummerierten Bildsequenz (VFX-Render). `start` =
/// Nummer des ersten Frames der Folge (`ImageSequence::start`), `fps` = native
/// Bildrate der Folge (`info.video[0].fps`). Begleitet den printf-Muster-Pfad
/// durch Renderplan und Player-Ziele.
#[derive(Clone, Copy, Debug)]
pub struct SeqInput {
    pub start: u64,
    pub fps: f64,
}

/// Eingabe-Argumente (`-i` + Seek) für einen ffmpeg-Decode-Befehl. EINE Quelle
/// für Player-Decoder UND Export-Worker, damit Wiedergabe und Export dieselbe
/// Quellzeit treffen.
///
/// - Bildsequenz (`seq = Some`): image2-Demuxer mit der nativen `seq.fps` und
///   einer aus `media_t` berechneten `-start_number` (= frame-genauer Einstieg
///   OHNE `-ss`, da der image2-Input-Seek unzuverlässig ist). Der erste gelesene
///   Frame bekommt PTS 0 — wie `-ss` ihn auf 0 zurücksetzen würde, damit die
///   nachgelagerte `fps`/`setpts`-Kette unverändert greift.
/// - Normales Medium (`seq = None`): `-ss media_t -i path` wie gehabt.
///
/// Die Argumente gehören VOR Filter/Output und können nach `-hwaccel` angehängt
/// werden (alles Eingabe-Optionen des nächsten `-i`).
pub fn decode_input_args(path: &str, media_t: f64, seq: Option<SeqInput>) -> Vec<String> {
    match seq {
        Some(s) => {
            let fps = if s.fps > 0.0 { s.fps } else { 24.0 };
            let off = (media_t.max(0.0) * fps).round().max(0.0) as u64;
            vec![
                "-framerate".to_string(),
                fps_arg(fps),
                "-start_number".to_string(),
                s.start.saturating_add(off).to_string(),
                "-i".to_string(),
                path.to_string(),
            ]
        }
        None => vec![
            "-ss".to_string(),
            format!("{:.4}", media_t.max(0.0)),
            "-i".to_string(),
            path.to_string(),
        ],
    }
}

/// setpts-Filterpräfix für konstante Vorwärts-Geschwindigkeit (inkl.
/// abschließendem Komma); leer bei 1×. EINE Formelquelle für Player-Decoder,
/// Export-Schnellpfad und Compositing-Decoder — identische Frame-Auswahl.
pub fn speed_setpts_filter(speed: f64) -> String {
    if !(speed.is_finite() && speed > 0.0) || (speed - 1.0).abs() < 1e-9 {
        String::new()
    } else {
        format!("setpts=(PTS-STARTPTS)/{speed:.6},")
    }
}

/// Pitch-korrigierte Tempo-Kette: atempo arbeitet nur in [0,5 … 2,0] —
/// Faktoren außerhalb werden kaskadiert (0,1 ⇒ 0,5 × 0,5 × 0,4).
/// None bei 1×. Player-Wiedergabe und Export-Mix nutzen dieselbe Kette.
pub fn atempo_chain(speed: f64) -> Option<String> {
    if !(speed.is_finite() && speed > 0.0) || (speed - 1.0).abs() < 1e-9 {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    let mut rest = speed;
    while rest > 2.0 + 1e-9 {
        parts.push("atempo=2.0".into());
        rest /= 2.0;
    }
    while rest < 0.5 - 1e-9 {
        parts.push("atempo=0.5".into());
        rest *= 2.0;
    }
    parts.push(format!("atempo={rest:.6}"));
    Some(parts.join(","))
}

/// Geschätzte Zieldateigröße in Bytes (None = nicht seriös schätzbar, z. B. CRF).
pub fn estimate_size(settings: &ExportSettings, duration: f64) -> Option<u64> {
    if duration <= 0.0 {
        return None;
    }
    let mut bits_per_sec: f64 = 0.0;
    if let Some(v) = &settings.video {
        match v.quality {
            VideoQuality::Bitrate(kbps) => bits_per_sec += kbps as f64 * 1000.0,
            VideoQuality::Crf(_) => return None,
        }
        if matches!(v.codec.quality, QualityKind::Profiles(_)) {
            return None;
        }
    }
    if let Some(a) = &settings.audio {
        if a.codec.bitrates.is_empty() {
            match a.codec.id {
                "pcm16" => bits_per_sec += (a.sample_rate * a.channels * 16) as f64,
                "pcm24" => bits_per_sec += (a.sample_rate * a.channels * 24) as f64,
                "pcm32f" => bits_per_sec += (a.sample_rate * a.channels * 32) as f64,
                _ => return None, // FLAC: inhaltsabhängig
            }
        } else {
            bits_per_sec += a.bitrate_kbps as f64 * 1000.0;
        }
    }
    Some((bits_per_sec * duration / 8.0) as u64)
}

/// Bytes menschenlesbar (de: Komma als Dezimaltrenner).
pub fn format_bytes(bytes: u64) -> String {
    let b = bytes as f64;
    let (value, unit) = if b >= 1e9 {
        (b / 1e9, "GB")
    } else if b >= 1e6 {
        (b / 1e6, "MB")
    } else if b >= 1e3 {
        (b / 1e3, "KB")
    } else {
        return format!("{bytes} B");
    };
    format!("{:.1} {unit}", value).replace('.', ",")
}

