//! Auto-Transkription (Whisper-Klasse): reine, testbare Logik für den Weg
//! Clip-Audio → getimte Untertitel-Cues. ffmpeg extrahiert 16-kHz-Mono-PCM aus
//! dem Clip-Audio, whisper.cpp (`whisper-cli`/`main`) transkribiert es nach SRT,
//! und die Cues werden in Sequenzzeit auf eine Untertitel-Spur abgebildet.
//!
//! Dieses Modul kapselt nur die reine Logik (ffmpeg-/whisper-Argumente,
//! Fortschritts-Parsing, Zeit-Abbildung der Cues). Die eigentliche Ausführung
//! (Worker-Threads, Job-Registry, Fortschritt/Abbruch) liegt in
//! [`crate::services`] — exakt das Async-Service-Muster des Proxy-Workflows
//! ([`crate::core::proxy`]). Ziel der erzeugten Cues ist die bestehende
//! Untertitel-Spur ([`crate::core::subtitle`]).

use crate::core::subtitle::SrtCue;

/// Standard-Whisper-CLI, falls in den Einstellungen keiner konfiguriert ist.
/// whisper.cpp nennt die CLI seit ~2024 `whisper-cli`; ältere Builds heißen
/// `main`. Konfigurierbar über [`crate::core::settings::AppSettings::whisper_path`].
pub const DEFAULT_WHISPER_BIN: &str = "whisper-cli";

/// Standard-Transkriptionssprache: automatische Erkennung durch whisper.cpp.
pub const DEFAULT_LANGUAGE: &str = "auto";

/// Auswählbare Transkriptionssprachen `(ISO-639-1-Code, deutsches Label)`.
/// `auto` lässt whisper.cpp die Sprache selbst erkennen. Die Liste deckt die
/// gängigsten Schnitt-Sprachen ab; whisper.cpp unterstützt darüber hinaus
/// weitere Codes (der gewählte Code wird unverändert an `-l` durchgereicht).
pub const LANGUAGES: [(&str, &str); 14] = [
    ("auto", "Automatisch erkennen"),
    ("de", "Deutsch"),
    ("en", "Englisch"),
    ("fr", "Französisch"),
    ("es", "Spanisch"),
    ("it", "Italienisch"),
    ("nl", "Niederländisch"),
    ("pt", "Portugiesisch"),
    ("pl", "Polnisch"),
    ("ru", "Russisch"),
    ("tr", "Türkisch"),
    ("uk", "Ukrainisch"),
    ("ja", "Japanisch"),
    ("zh", "Chinesisch"),
];

/// Deutsches Label eines Sprachcodes (Fallback: der Code selbst).
pub fn language_label(code: &str) -> &str {
    LANGUAGES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, l)| *l)
        .unwrap_or(code)
}

/// Index eines Sprachcodes in [`LANGUAGES`] (Fallback: 0 = Automatisch).
pub fn language_index(code: &str) -> usize {
    LANGUAGES.iter().position(|(c, _)| *c == code).unwrap_or(0)
}

/// ffmpeg-Argumente, um das Clip-Audio als 16-kHz-Mono-PCM-WAV zu extrahieren
/// (whisper.cpp erwartet genau dieses Format). Fenster
/// `[media_in, media_in+media_dur]` per Input-Seek (`-ss` vor `-i` ⇒ schnell;
/// bei Audio-Reencode sample-genau). `-vn` verwirft die Videospur, damit auch
/// 4K/8K-Quellen ohne Bilddecode auskommen. Liefert die komplette Argumentliste
/// nach dem Binary (inkl. `-i` und Ausgabedatei).
pub fn extract_args(src: &str, media_in: f64, media_dur: f64, out: &str) -> Vec<String> {
    let mut a: Vec<String> = vec!["-y".into(), "-v".into(), "error".into(), "-nostdin".into()];
    if media_in.is_finite() && media_in > 0.0 {
        a.push("-ss".into());
        a.push(format!("{media_in:.3}"));
    }
    a.push("-i".into());
    a.push(src.into());
    if media_dur.is_finite() && media_dur > 0.0 {
        a.push("-t".into());
        a.push(format!("{media_dur:.3}"));
    }
    a.extend(
        ["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le", "-f", "wav"].map(String::from),
    );
    a.push(out.into());
    a
}

/// whisper.cpp-CLI-Argumente: Modell, Eingabe-WAV, SRT-Ausgabe nach
/// `<out_base>.srt`, Sprache (oder `auto`), Fortschrittsausgabe auf stderr.
/// Leere Sprache fällt auf `auto` zurück.
pub fn whisper_args(model: &str, wav: &str, out_base: &str, lang: &str) -> Vec<String> {
    let lang = lang.trim();
    let lang = if lang.is_empty() { DEFAULT_LANGUAGE } else { lang };
    vec![
        "-m".into(),
        model.into(),
        "-f".into(),
        wav.into(),
        "-l".into(),
        lang.into(),
        "-osrt".into(),
        "-of".into(),
        out_base.into(),
        "--print-progress".into(),
    ]
}

/// Fortschritt (0..1) aus einer whisper.cpp-stderr-Zeile
/// (`whisper_print_progress_callback: progress =  42%`) — `None`, wenn die
/// Zeile keinen Fortschritt trägt.
pub fn parse_progress(line: &str) -> Option<f32> {
    // Letztes Vorkommen: die Zeile enthält „..._progress_callback: progress = NN%“
    // — der Wert steht hinter dem ZWEITEN „progress".
    let idx = line.rfind("progress")?;
    let rest = line[idx + "progress".len()..]
        .trim_start_matches([' ', '=', ':', '\t'])
        .trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let pct: f32 = digits.parse().ok()?;
    Some((pct / 100.0).clamp(0.0, 1.0))
}

/// Whisper-Cues (lokale Audiozeit ab 0) in Sequenzzeit abbilden: Offset um
/// `clip_start`, Skalierung mit der Clip-Geschwindigkeit (lokale Medienzeit `t`
/// ⇒ Timeline-Offset `t / eff_speed`, weil der Clip `media_span` Sekunden
/// Material über `media_span / eff_speed` Timeline-Sekunden zeigt) und Klemmung
/// aufs Clip-Fenster `[clip_start, clip_start+clip_dur]`. Außerhalb liegende
/// Cues werden auf den Rand geklemmt bzw. verworfen, leere/entartete (Dauer
/// ≤ 1 ms nach Klemmung) übersprungen. Ergebnis nach Startzeit sortiert.
pub fn map_cues_to_sequence(
    cues: &[SrtCue],
    clip_start: f64,
    eff_speed: f64,
    clip_dur: f64,
) -> Vec<SrtCue> {
    let speed = if eff_speed.is_finite() && eff_speed > 0.0 {
        eff_speed
    } else {
        1.0
    };
    let clip_start = clip_start.max(0.0);
    let clip_end = clip_start + clip_dur.max(0.0);
    let mut out: Vec<SrtCue> = Vec::with_capacity(cues.len());
    for c in cues {
        let text = c.text.trim();
        if text.is_empty() {
            continue;
        }
        let raw_start = clip_start + c.start.max(0.0) / speed;
        let raw_end = clip_start + c.end.max(0.0) / speed;
        let start = raw_start.clamp(clip_start, clip_end);
        let end = raw_end.clamp(clip_start, clip_end);
        if end - start <= 1e-3 {
            continue;
        }
        out.push(SrtCue {
            start,
            end,
            text: text.to_string(),
        });
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_args_window_and_format() {
        let a = extract_args("/x/clip.mp4", 12.0, 3.5, "/tmp/out.wav");
        // Input-Seek (vor -i), Fensterlänge nach -i.
        let ss = a.iter().position(|s| s == "-ss").unwrap();
        let i = a.iter().position(|s| s == "-i").unwrap();
        let t = a.iter().position(|s| s == "-t").unwrap();
        assert!(ss < i && i < t, "Reihenfolge -ss < -i < -t");
        assert_eq!(a[ss + 1], "12.000");
        assert_eq!(a[t + 1], "3.500");
        // 16-kHz-Mono-PCM, Video verworfen.
        assert!(a.windows(2).any(|w| w[0] == "-ar" && w[1] == "16000"));
        assert!(a.windows(2).any(|w| w[0] == "-ac" && w[1] == "1"));
        assert!(a.iter().any(|s| s == "-vn"));
        assert_eq!(a.last().unwrap(), "/tmp/out.wav");
        // Ohne Offset kein -ss.
        let b = extract_args("/x/clip.mp4", 0.0, 0.0, "/tmp/out.wav");
        assert!(!b.iter().any(|s| s == "-ss"));
        assert!(!b.iter().any(|s| s == "-t"));
    }

    #[test]
    fn whisper_args_carry_model_lang_and_srt_output() {
        let a = whisper_args("/m/ggml-base.bin", "/tmp/a.wav", "/tmp/a", "de");
        assert!(a.windows(2).any(|w| w[0] == "-m" && w[1] == "/m/ggml-base.bin"));
        assert!(a.windows(2).any(|w| w[0] == "-l" && w[1] == "de"));
        assert!(a.windows(2).any(|w| w[0] == "-of" && w[1] == "/tmp/a"));
        assert!(a.iter().any(|s| s == "-osrt"));
        assert!(a.iter().any(|s| s == "--print-progress"));
        // Leere Sprache ⇒ auto.
        let b = whisper_args("/m.bin", "/tmp/a.wav", "/tmp/a", "  ");
        assert!(b.windows(2).any(|w| w[0] == "-l" && w[1] == "auto"));
    }

    #[test]
    fn progress_parsing_handles_whisper_lines() {
        assert_eq!(
            parse_progress("whisper_print_progress_callback: progress =  42%"),
            Some(0.42)
        );
        assert_eq!(parse_progress("progress = 100%"), Some(1.0));
        assert_eq!(parse_progress("progress=7%"), Some(0.07));
        assert_eq!(parse_progress("whisper_full: something else"), None);
        assert_eq!(parse_progress("no numbers progress = x"), None);
    }

    #[test]
    fn maps_cues_with_offset_and_clamps_to_clip() {
        let cues = vec![
            SrtCue { start: 0.0, end: 2.0, text: "Hallo".into() },
            SrtCue { start: 2.0, end: 4.0, text: "Welt".into() },
            // Reicht über das Clip-Ende hinaus → wird geklemmt.
            SrtCue { start: 4.5, end: 9.0, text: "Rest".into() },
            // Leerer Text → verworfen.
            SrtCue { start: 1.0, end: 1.5, text: "  ".into() },
        ];
        // Clip beginnt bei 10 s, Dauer 5 s, normale Geschwindigkeit.
        let out = map_cues_to_sequence(&cues, 10.0, 1.0, 5.0);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].text, "Hallo");
        assert!((out[0].start - 10.0).abs() < 1e-9);
        assert!((out[0].end - 12.0).abs() < 1e-9);
        assert!((out[1].start - 12.0).abs() < 1e-9);
        // Letzter Cue auf das Clip-Ende (15 s) geklemmt.
        assert!((out[2].end - 15.0).abs() < 1e-9);
    }

    #[test]
    fn maps_cues_scales_by_speed() {
        let cues = vec![SrtCue { start: 0.0, end: 2.0, text: "Schnell".into() }];
        // Doppelte Geschwindigkeit: 2 s Medienzeit ⇒ 1 s Timeline.
        let out = map_cues_to_sequence(&cues, 0.0, 2.0, 10.0);
        assert_eq!(out.len(), 1);
        assert!((out[0].start - 0.0).abs() < 1e-9);
        assert!((out[0].end - 1.0).abs() < 1e-9);
    }

    #[test]
    fn language_helpers_round_trip() {
        assert_eq!(language_label("de"), "Deutsch");
        assert_eq!(language_label("auto"), "Automatisch erkennen");
        assert_eq!(language_label("xx"), "xx");
        assert_eq!(language_index("auto"), 0);
        assert_eq!(LANGUAGES[language_index("en")].0, "en");
        assert_eq!(language_index("nonsense"), 0);
    }
}
