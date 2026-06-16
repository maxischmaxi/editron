use super::*;
use crate::core::timeline::{
    TimelineStore, TrackKind,
};
use crate::stores::MediaStore;

// =============================================================== Settings

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum VideoQuality {
    Crf(u32),
    /// Ziel-Bitrate in kbit/s (1-Pass-VBR).
    Bitrate(u32),
}

#[derive(Clone)]
pub struct VideoSettings {
    pub codec: &'static VideoCodecDef,
    /// Encoder-Backend (Software/Hardware) — bestimmt `-c:v` + Qualitäts-Flag.
    /// Gehört immer zu `codec.encoders`.
    pub encoder: &'static EncoderDef,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub quality: VideoQuality,
    /// Index in `codec.speed_presets` (ignoriert, wenn leer).
    pub speed: usize,
    /// Index in die Profil-Liste (nur `QualityKind::Profiles`).
    pub profile: usize,
    /// 10-Bit-Ausgabe für CRF/Bitrate-Codecs erzwingen (HEVC main10, AV1,
    /// VP9 Profil 2, H.264 High 10). Profil-Codecs (ProRes/DNxHR) steuern die
    /// Bittiefe über `profile` und ignorieren dieses Flag. Siehe
    /// [`codec_tenbit_pix_fmt`].
    pub tenbit: bool,
}

/// 10-Bit-Pixelformat eines CRF/Bitrate-Codecs (`None` = kein 10-Bit-Pfad).
/// Profil-Codecs (ProRes/DNxHR) liefern ihre Bittiefe über das Profil.
pub fn codec_tenbit_pix_fmt(codec_id: &str) -> Option<&'static str> {
    match codec_id {
        "hevc" | "av1" | "vp9" | "h264" => Some("yuv420p10le"),
        _ => None,
    }
}

/// Unterstützt der Codec einen 10-Bit-Schalter im Export-Dialog?
pub fn codec_supports_tenbit(codec_id: &str) -> bool {
    codec_tenbit_pix_fmt(codec_id).is_some()
}

#[derive(Clone)]
pub struct AudioSettings {
    pub codec: &'static AudioCodecDef,
    pub bitrate_kbps: u32,
    pub sample_rate: u32,
    pub channels: u32,
}

/// Lautheits-Normalisierung der Tonspur beim Export (ffmpeg `loudnorm`,
/// 2-Pass: erst die integrierte Lautheit/True-Peak messen, dann linear auf
/// das Ziel bringen). In [`ExportSettings::loudness`] steht `None` für „aus“.
/// Delivery-Spezifikationen (EBU R128, ATSC A/85, Streaming-Plattformen)
/// verlangen ein definiertes Ziel-Lautheitsmaß plus True-Peak-Obergrenze.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LoudnessNorm {
    /// Integriertes Ziel-Lautheitsmaß (LUFS bzw. LKFS), typ. −14 … −24.
    pub target_i: f64,
    /// True-Peak-Obergrenze in dBTP (z. B. −1,0).
    pub true_peak: f64,
    /// Ziel-Lautheitsumfang (LRA) in LU.
    pub lra: f64,
}

impl LoudnessNorm {
    /// EBU R128 (−23 LUFS, −1 dBTP) — europäischer Broadcast-Standard.
    pub const EBU_R128: LoudnessNorm = LoudnessNorm {
        target_i: -23.0,
        true_peak: -1.0,
        lra: 18.0,
    };

    /// Sinnvolle Schranken für frei eingestellte Werte (ffmpeg `loudnorm`
    /// akzeptiert I −70…−5, TP −9…0, LRA 1…50).
    pub fn clamped(self) -> LoudnessNorm {
        LoudnessNorm {
            target_i: self.target_i.clamp(-70.0, -5.0),
            true_peak: self.true_peak.clamp(-9.0, 0.0),
            lra: self.lra.clamp(1.0, 50.0),
        }
    }
}

/// Benanntes Lautheits-Preset für den Export-Dialog (delivery-konforme Ziele).
pub struct LoudnessPreset {
    pub label: &'static str,
    pub norm: LoudnessNorm,
}

pub const LOUDNESS_PRESETS: &[LoudnessPreset] = &[
    LoudnessPreset {
        label: "EBU R128 (−23 LUFS)",
        norm: LoudnessNorm { target_i: -23.0, true_peak: -1.0, lra: 18.0 },
    },
    LoudnessPreset {
        label: "−16 LUFS (Podcast / Mobil)",
        norm: LoudnessNorm { target_i: -16.0, true_peak: -1.0, lra: 11.0 },
    },
    LoudnessPreset {
        label: "−14 LUFS (Streaming)",
        norm: LoudnessNorm { target_i: -14.0, true_peak: -1.0, lra: 11.0 },
    },
    LoudnessPreset {
        label: "ATSC A/85 (−24 LKFS)",
        norm: LoudnessNorm { target_i: -24.0, true_peak: -2.0, lra: 7.0 },
    },
];

/// Index des Presets, dessen Zielwerte exakt zu `norm` passen (für die
/// Auswahl im Dialog); `None` = frei eingestellt.
pub fn loudness_preset_index(norm: &LoudnessNorm) -> Option<usize> {
    LOUDNESS_PRESETS.iter().position(|p| p.norm == *norm)
}

/// Umgang mit sichtbaren Untertitel-Spuren beim Sequenz-Export.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubtitleMode {
    /// Untertitel ignorieren.
    #[default]
    None,
    /// Je sichtbarer Spur eine SRT-Datei neben der Zieldatei.
    Sidecar,
    /// Als Untertitel-Streams in den Container muxen (mov_text/srt/webvtt).
    Embed,
    /// Ins Bild einbrennen (CPU-Compositor, identisch zur Vorschau).
    BurnIn,
}

impl SubtitleMode {
    pub const ALL: [SubtitleMode; 4] = [
        SubtitleMode::None,
        SubtitleMode::Sidecar,
        SubtitleMode::Embed,
        SubtitleMode::BurnIn,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            SubtitleMode::None => "Keine",
            SubtitleMode::Sidecar => "Sidecar-Datei (.srt)",
            SubtitleMode::Embed => "In Datei einbetten",
            SubtitleMode::BurnIn => "Ins Bild einbrennen",
        }
    }
}

#[derive(Clone)]
pub struct ExportSettings {
    pub container: &'static ContainerDef,
    /// None = Audio-only-Export.
    pub video: Option<VideoSettings>,
    /// None = ohne Tonspur.
    pub audio: Option<AudioSettings>,
    /// Lautheits-Normalisierung der Tonspur (None = aus). Wirkt nur, wenn
    /// auch `audio` gesetzt ist.
    pub loudness: Option<LoudnessNorm>,
    /// Nur den Bereich zwischen Sequenz-In/Out exportieren.
    pub use_in_out: bool,
    /// Audiospuren getrennt als Stems ausgeben: je Audiospur ein eigener
    /// Audio-Stream im Container (mit Spurname-Titel) statt einer summierten
    /// Stereo-Master-Spur. Bus-FX, Automation, Spur-Gain/Pan und Master bleiben
    /// pro Spur angewandt (die Summe aller Stems ergibt den Master-Mix).
    /// Wirkt nur, wenn auch `audio` gesetzt ist und der Container mehrere
    /// Audio-Streams trägt ([`ContainerDef::multi_audio`]) — siehe
    /// [`stems_enabled`].
    pub audio_stems: bool,
    /// Sichtbare Untertitel-Spuren: ignorieren, Sidecar, muxen, einbrennen.
    pub subtitles: SubtitleMode,
    /// Startnummer der Bild-Sequenz (nur bei `container.image_sequence`).
    pub image_start: u32,
    pub output: String,
}

pub(crate) fn default_video(codec_id: &str, width: u32, height: u32, fps: f64) -> VideoSettings {
    let codec = video_codec(codec_id);
    let quality = match codec.quality {
        QualityKind::CrfOrBitrate { crf } => VideoQuality::Crf(crf.2),
        QualityKind::Profiles(_) => VideoQuality::Crf(0),
    };
    VideoSettings {
        codec,
        encoder: &codec.encoders[0],
        width,
        height,
        fps,
        quality,
        speed: codec.default_speed,
        profile: 0,
        tenbit: false,
    }
}

/// Aufgelöstes Ausgabe-Pixelformat (bei Profil-Codecs liefert das Profil das
/// Format). Spiegelt die Auswahl in [`encoder_args`].
pub fn resolved_output_pix_fmt(v: &VideoSettings) -> &'static str {
    match v.codec.quality {
        QualityKind::Profiles(profiles) => profiles[v.profile.min(profiles.len() - 1)].2,
        QualityKind::CrfOrBitrate { .. } => {
            if v.tenbit {
                if let Some(fmt) = codec_tenbit_pix_fmt(v.codec.id) {
                    return fmt;
                }
            }
            v.codec.pix_fmt
        }
    }
}

/// Gibt das Ziel mehr als 8 Bit pro Kanal aus (ProRes/DNxHR-10bit-Profile,
/// HEVC main10 …)? Bestimmt das Pipe-Format zwischen Compositor und Encoder:
/// `rgba64le` (16 Bit/Kanal, verlustarme f32-Quantisierung ⇒ kein Banding auf
/// 10-Bit-Verläufen) statt `rgba` (8 Bit, mit Dithering quantisiert).
pub fn pipe_hi_bit(v: &VideoSettings) -> bool {
    crate::core::pixbuf::pix_fmt_bit_depth(resolved_output_pix_fmt(v)) > 8
}

/// Rohbild-Pipe-Format zwischen Compositor und Encoder (`-pixel_format`).
pub fn pipe_pix_fmt(v: &VideoSettings) -> &'static str {
    if pipe_hi_bit(v) {
        "rgba64le"
    } else {
        "rgba"
    }
}

/// Bytes pro Pixel im Pipe-Format (4 = rgba8, 8 = rgba64le).
pub fn pipe_bytes_per_px(v: &VideoSettings) -> usize {
    if pipe_hi_bit(v) {
        8
    } else {
        4
    }
}

pub(crate) fn default_audio(codec_id: &str, bitrate: Option<u32>) -> AudioSettings {
    let codec = audio_codec(codec_id);
    AudioSettings {
        codec,
        bitrate_kbps: bitrate.unwrap_or(codec.default_bitrate),
        sample_rate: codec.forced_rate.unwrap_or(48000),
        channels: 2,
    }
}

// ================================================================ Presets

pub struct ExportPreset {
    pub label: &'static str,
    /// Baut Settings; `(w, h)` = vorgeschlagene Quellauflösung der Timeline.
    pub build: fn(source: (u32, u32), fps: f64) -> ExportSettings,
}

fn preset_settings(
    container_id: &str,
    video: Option<VideoSettings>,
    audio: Option<AudioSettings>,
) -> ExportSettings {
    ExportSettings {
        container: container(container_id),
        video,
        audio,
        loudness: None,
        use_in_out: false,
        audio_stems: false,
        subtitles: SubtitleMode::None,
        image_start: 1,
        output: String::new(),
    }
}

/// Werden beim Export getrennte Audio-Stems (je Spur einer) ausgegeben? Nur,
/// wenn der Nutzer es gewählt hat, es überhaupt eine Tonspur gibt und der
/// Zielcontainer mehrere Audio-Streams tragen kann. EINE Quelle für die
/// Renderplan-Routing-Entscheidung (`build_render_plan`) und den Worker
/// (Mischen/Muxen) — sonst liefen Plan und Worker auseinander.
pub fn stems_enabled(settings: &ExportSettings) -> bool {
    settings.audio_stems && settings.audio.is_some() && settings.container.multi_audio()
}

pub const PRESETS: &[ExportPreset] = &[
    ExportPreset {
        label: "YouTube 1080p",
        build: |_, fps| {
            let mut v = default_video("h264", 1920, 1080, fps);
            v.quality = VideoQuality::Bitrate(12000);
            preset_settings("mp4", Some(v), Some(default_audio("aac", Some(192))))
        },
    },
    ExportPreset {
        label: "YouTube 4K (2160p)",
        build: |_, fps| {
            let mut v = default_video("h264", 3840, 2160, fps);
            v.quality = VideoQuality::Bitrate(45000);
            preset_settings("mp4", Some(v), Some(default_audio("aac", Some(192))))
        },
    },
    ExportPreset {
        label: "Vimeo 1080p",
        build: |_, fps| {
            let mut v = default_video("h264", 1920, 1080, fps);
            v.quality = VideoQuality::Bitrate(20000);
            preset_settings("mp4", Some(v), Some(default_audio("aac", Some(320))))
        },
    },
    ExportPreset {
        label: "H.264 Master",
        build: |(w, h), fps| {
            let mut v = default_video("h264", w, h, fps);
            v.quality = VideoQuality::Crf(18);
            v.speed = 6; // slow
            preset_settings("mp4", Some(v), Some(default_audio("aac", Some(256))))
        },
    },
    ExportPreset {
        label: "H.265 Master",
        build: |(w, h), fps| {
            let mut v = default_video("hevc", w, h, fps);
            v.quality = VideoQuality::Crf(22);
            preset_settings("mp4", Some(v), Some(default_audio("aac", Some(256))))
        },
    },
    ExportPreset {
        label: "ProRes 422 HQ",
        build: |(w, h), fps| {
            let mut v = default_video("prores", w, h, fps);
            v.profile = 3;
            preset_settings("mov", Some(v), Some(default_audio("pcm24", None)))
        },
    },
    ExportPreset {
        label: "ProRes 4444",
        build: |(w, h), fps| {
            let mut v = default_video("prores", w, h, fps);
            v.profile = 4;
            preset_settings("mov", Some(v), Some(default_audio("pcm24", None)))
        },
    },
    ExportPreset {
        label: "DNxHR HQ",
        build: |(w, h), fps| {
            let mut v = default_video("dnxhr", w, h, fps);
            v.profile = 2;
            preset_settings("mov", Some(v), Some(default_audio("pcm24", None)))
        },
    },
    ExportPreset {
        label: "VP9 (WebM)",
        build: |(w, h), fps| {
            let v = default_video("vp9", w, h, fps);
            preset_settings("webm", Some(v), Some(default_audio("opus", Some(160))))
        },
    },
    ExportPreset {
        label: "AV1 (SVT)",
        build: |(w, h), fps| {
            let v = default_video("av1", w, h, fps);
            preset_settings("mp4", Some(v), Some(default_audio("aac", Some(192))))
        },
    },
    ExportPreset {
        label: "Audio — WAV 24-bit",
        build: |_, _| preset_settings("wav", None, Some(default_audio("pcm24", None))),
    },
    ExportPreset {
        label: "Audio — MP3 320",
        build: |_, _| preset_settings("mp3", None, Some(default_audio("mp3", Some(320)))),
    },
    ExportPreset {
        label: "Audio — FLAC",
        build: |_, _| preset_settings("flac", None, Some(default_audio("flac", None))),
    },
    ExportPreset {
        label: "Audio — AAC (M4A)",
        build: |_, _| preset_settings("m4a", None, Some(default_audio("aac", Some(256)))),
    },
];

/// Auflösung des ersten Video-Clips der Sequenz (gerade gerundet),
/// Fallback Full HD — Vorschlag für „Wie Quelle“-Presets.
pub fn suggested_resolution(timeline: &TimelineStore, media: &MediaStore) -> (u32, u32) {
    let mut clips: Vec<_> = timeline
        .clips
        .iter()
        .filter(|c| c.kind == TrackKind::Video)
        .collect();
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    for clip in clips {
        if let Some(asset) = media.asset(&clip.asset_id) {
            if let Some(v) = asset.info.video.first() {
                if v.width >= 16 && v.height >= 16 {
                    return (v.width & !1, v.height & !1);
                }
            }
        }
    }
    (1920, 1080)
}

