use super::*;
use crate::core::compose;
use crate::core::timeline::{
    TimelineStore, TrackKind,
};
use crate::stores::MediaStore;
use std::collections::HashSet;
use std::path::Path;

// ============================================================== Validierung

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug)]
pub struct ValidationIssue {
    pub severity: Severity,
    pub message: String,
}

fn error(msg: impl Into<String>) -> ValidationIssue {
    ValidationIssue {
        severity: Severity::Error,
        message: msg.into(),
    }
}

fn warning(msg: impl Into<String>) -> ValidationIssue {
    ValidationIssue {
        severity: Severity::Warning,
        message: msg.into(),
    }
}

/// Prüft Settings + Timeline vor dem Start; Errors blockieren den Export.
pub fn validate(
    timeline: &TimelineStore,
    media: &MediaStore,
    ffmpeg_available: Option<bool>,
    encoders: Option<&HashSet<String>>,
    settings: &ExportSettings,
    nests: &dyn compose::NestResolver,
) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();

    match ffmpeg_available {
        Some(true) => {}
        Some(false) => issues.push(error(
            "FFmpeg wurde nicht gefunden — ohne FFmpeg ist kein Export möglich.",
        )),
        None => issues.push(warning("FFmpeg wird noch gesucht …")),
    }

    if settings.video.is_none() && settings.audio.is_none() {
        issues.push(error("Weder Video noch Audio ausgewählt — nichts zu exportieren."));
        return issues;
    }

    // ---- Bereich + Inhalt ----
    let (start, end) = export_range(timeline, settings.use_in_out);
    // ---- Untertitel ----
    if settings.subtitles != SubtitleMode::None {
        if settings.subtitles == SubtitleMode::Embed && settings.container.subtitle_codec.is_none()
        {
            issues.push(error(format!(
                "Container „{}“ unterstützt keine eingebetteten Untertitel — Sidecar oder Einbrennen wählen.",
                settings.container.label
            )));
        }
        if matches!(settings.subtitles, SubtitleMode::Embed | SubtitleMode::BurnIn)
            && settings.video.is_none()
        {
            issues.push(error(
                "Untertitel einbetten/einbrennen erfordert einen Video-Export.",
            ));
        }
        // Gibt es überhaupt sichtbare Untertitel im Exportbereich?
        let any_visible = timeline
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Subtitle && !t.muted)
            .any(|t| {
                timeline
                    .subtitle_cues(&t.id)
                    .iter()
                    .any(|c| c.end > start && c.start < end)
            });
        if !any_visible {
            issues.push(warning(
                "Keine sichtbaren Untertitel im Exportbereich — die Untertitel-Option bleibt wirkungslos.",
            ));
        }
    }

    if timeline.clips.is_empty() {
        issues.push(error("Die Timeline ist leer — es gibt nichts zu exportieren."));
        return issues;
    }
    if settings.use_in_out {
        match (timeline.in_point, timeline.out_point) {
            (Some(i), Some(o)) if o - i <= 1e-9 => {
                issues.push(error("Der Out-Punkt liegt nicht hinter dem In-Punkt."));
            }
            (Some(_), Some(_)) => {}
            _ => issues.push(error("In- und Out-Punkt sind nicht gesetzt.")),
        }
    }
    if end - start <= 1e-9 {
        issues.push(error("Der Exportbereich ist leer."));
        return issues;
    }

    let plan = build_render_plan(timeline, media, settings, nests);
    if !plan.has_video_media() && !plan.has_audio_media() {
        issues.push(error(
            "Im Exportbereich liegen keine abspielbaren Clips (alles leer, offline oder stumm).",
        ));
        return issues;
    }
    if settings.video.is_some() && !plan.has_video_media() {
        issues.push(warning(
            "Im Exportbereich liegt kein Video — es wird nur Schwarzbild exportiert.",
        ));
    }
    if settings.audio.is_some() && !plan.has_audio_media() && settings.video.is_some() {
        issues.push(warning("Im Exportbereich liegt kein Audio — die Tonspur bleibt stumm."));
    }
    if settings.audio.is_none() && settings.video.is_some() {
        issues.push(warning("Audio ist deaktiviert — die Datei enthält keine Tonspur."));
    }

    // Offline-Medien, deren Clips den Bereich berühren.
    let offline_hit = timeline
        .clips
        .iter()
        .filter(|c| c.enabled && c.end() > start && c.start < end)
        .filter(|c| media.asset(&c.asset_id).is_some_and(|a| a.offline))
        .count();
    if offline_hit > 0 {
        issues.push(warning(format!(
            "{offline_hit} Clip(s) mit Offline-Medien im Exportbereich — sie werden als Schwarzbild/Stille exportiert. Tipp: Datei → Medien neu verknüpfen."
        )));
    }
    // Offline-Originale, für die ein Proxy existiert: der Export nutzt IMMER das
    // Original — der Proxy ersetzt es NICHT. Klar darauf hinweisen.
    let offline_with_proxy = timeline
        .clips
        .iter()
        .filter(|c| c.enabled && c.end() > start && c.start < end)
        .filter_map(|c| media.asset(&c.asset_id))
        .filter(|a| a.offline && a.proxy_path.is_some())
        .map(|a| a.id.clone())
        .collect::<std::collections::HashSet<_>>()
        .len();
    if offline_with_proxy > 0 {
        issues.push(warning(format!(
            "{offline_with_proxy} Medium/Medien sind offline, haben aber einen Proxy — der Export nutzt IMMER die Originale, nicht den Proxy. Originale neu verknüpfen, sonst Schwarzbild/Stille."
        )));
    }

    // ---- Video-Parameter ----
    if let Some(v) = &settings.video {
        if v.width < 16 || v.height < 16 || v.width > 8192 || v.height > 8192 {
            issues.push(error("Auflösung muss zwischen 16 und 8192 Pixeln liegen."));
        }
        if v.width % 2 != 0 || v.height % 2 != 0 {
            issues.push(error(
                "Breite und Höhe müssen gerade Zahlen sein (Farb-Subsampling).",
            ));
        }
        if !(v.fps > 0.0) || v.fps > 240.0 {
            issues.push(error("Framerate muss zwischen 1 und 240 liegen."));
        }
        if let VideoQuality::Bitrate(kbps) = v.quality {
            if kbps == 0 {
                issues.push(error("Ziel-Bitrate darf nicht 0 sein."));
            }
        }
        if let Some(set) = encoders {
            if !set.contains(v.encoder.id) {
                issues.push(error(format!(
                    "Encoder „{}“ ({}) fehlt in dieser FFmpeg-Installation.",
                    v.encoder.label, v.encoder.id
                )));
            }
        }
    }

    // ---- Audio-Parameter ----
    if let Some(a) = &settings.audio {
        if let Some(set) = encoders {
            if !set.contains(a.codec.encoder) {
                issues.push(error(format!(
                    "Encoder „{}“ fehlt in dieser FFmpeg-Installation.",
                    a.codec.encoder
                )));
            }
        }
        // Zwischendatei ist klassisches RIFF/WAV (32-bit-Größenfeld).
        let mix_bytes = (plan.duration * a.sample_rate as f64) as u64 * a.channels as u64 * 4;
        if mix_bytes >= u32::MAX as u64 - 1024 {
            issues.push(error(
                "Audio-Mix überschreitet 4 GB — Bereich verkleinern oder Samplerate/Kanäle reduzieren.",
            ));
        }
    }

    // ---- Zieldatei ----
    if settings.output.trim().is_empty() {
        issues.push(error("Keine Zieldatei gewählt."));
    } else {
        let out = Path::new(&settings.output);
        match out.parent() {
            Some(dir) if dir.as_os_str().is_empty() => {
                issues.push(error("Zielpfad muss absolut sein."));
            }
            Some(dir) => {
                if !dir.is_dir() {
                    issues.push(error(format!(
                        "Zielordner existiert nicht: {}",
                        dir.display()
                    )));
                }
            }
            None => issues.push(error("Ungültiger Zielpfad.")),
        }
        // Niemals eine Quelldatei der Timeline überschreiben.
        let overwrites_source = media
            .assets
            .iter()
            .any(|a| same_file(Path::new(&a.path), out));
        if overwrites_source {
            issues.push(error(
                "Die Zieldatei ist eine Quelldatei dieses Projekts — bitte anderen Namen wählen.",
            ));
        } else if out.exists() {
            issues.push(warning("Die Zieldatei existiert bereits und wird überschrieben."));
        }
        let expected = format!(".{}", settings.container.ext);
        if !settings.output.to_lowercase().ends_with(&expected) {
            issues.push(warning(format!(
                "Dateiname endet nicht auf „{expected}“ — einige Player erwarten die passende Endung."
            )));
        }
    }

    issues
}

fn same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

