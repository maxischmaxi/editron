//! Sequenz-Export: Container-/Codec-Katalog, Settings + Render-Presets,
//! Validierung, Renderplan und der Render-Worker.
//!
//! Der Plan reproduziert exakt die Wiedergabe-Semantik des Players und des
//! Programmmonitors: alle sichtbaren Video-Layer werden mit ihren animierten
//! Transformationen (Position/Skalierung/Rotation/Deckkraft, Keyframes in
//! Medienzeit) von unten nach oben komponiert; Audio = Summe aller hörbaren
//! Clips mit Spur-Gain/Pan, Clip-Gain inkl. Lautstärke-Keyframes und
//! Master-Fader. Der Worker rendert in zwei Phasen — Audio-Mixdown in eine
//! temporäre f32-WAV, dann Video segmentweise: untransformierte Einzel-Layer
//! laufen direkt durch eine ffmpeg-Pipe (Schnellpfad), alles andere durch
//! den CPU-Compositor (`core/compose.rs`) mit einem Decoder je Layer
//! (transparent gepolsterte rawvideo/rgba-Frames). Finalisiert wird atomar
//! (`<ziel>.part` → rename). Abbruch über ein geteiltes Flag; jeder Fehler
//! wird als Event gemeldet, nie gepanict (`catch_unwind` als letzte Linie).

use crate::core::animation::{AnimatedParam, ClipFx};
use crate::core::audio_fx::AudioFxChain;
use crate::core::compose;
use crate::core::effects::{self, EffectInstance};
use crate::core::grade::{self, ColorGrade};
use crate::core::sequence::SequenceSettings;
use crate::core::timeline::{
    sequence_end, TimelineClip, TimelineStore, TimelineTrack, TrackKind,
};
use crate::core::transitions::{
    self, Transition, TransitionDirection, TransitionFx, TransitionKind, TransitionRole,
};
use crate::core::types::MediaKind;
use crate::services::ServiceEvent;
use crate::stores::MediaStore;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

// ================================================================ Katalog

/// Zielcontainer; `video: false` = reines Audioformat.
pub struct ContainerDef {
    pub id: &'static str,
    pub label: &'static str,
    pub ext: &'static str,
    pub video: bool,
    pub video_codecs: &'static [&'static str],
    pub audio_codecs: &'static [&'static str],
    /// `-movflags +faststart` (Moov-Atom an den Anfang, Streaming-tauglich).
    pub faststart: bool,
    /// ffmpeg-Muxer — explizit, weil die `.part`-Zwischendatei keine
    /// aussagekräftige Endung hat.
    pub muxer: &'static str,
    /// Untertitel-Codec fürs Einbetten (`-c:s`); None = Container kann
    /// keine Untertitel-Streams.
    pub subtitle_codec: Option<&'static str>,
    /// Bild-Sequenz statt einer einzelnen Datei: jeder Frame wird als
    /// nummerierte Datei geschrieben (`out_%06d.png`). Nur Video-Phase, nie
    /// Audio/Untertitel; `video_codecs[0]` ist der Bild-Encoder.
    pub image_sequence: bool,
}

pub const CONTAINERS: &[ContainerDef] = &[
    ContainerDef {
        id: "mp4",
        label: "MP4",
        ext: "mp4",
        video: true,
        video_codecs: &["h264", "hevc", "av1"],
        audio_codecs: &["aac", "mp3"],
        faststart: true,
        muxer: "mp4",
        subtitle_codec: Some("mov_text"),
        image_sequence: false,
    },
    ContainerDef {
        id: "mov",
        label: "QuickTime (MOV)",
        ext: "mov",
        video: true,
        video_codecs: &["prores", "dnxhr", "h264", "hevc"],
        audio_codecs: &["pcm24", "pcm16", "pcm32f", "aac"],
        faststart: true,
        muxer: "mov",
        subtitle_codec: Some("mov_text"),
        image_sequence: false,
    },
    ContainerDef {
        id: "mkv",
        label: "Matroska (MKV)",
        ext: "mkv",
        video: true,
        video_codecs: &["h264", "hevc", "vp9", "av1"],
        audio_codecs: &["aac", "opus", "mp3", "flac", "pcm24", "pcm16", "pcm32f"],
        faststart: false,
        muxer: "matroska",
        subtitle_codec: Some("srt"),
        image_sequence: false,
    },
    ContainerDef {
        id: "webm",
        label: "WebM",
        ext: "webm",
        video: true,
        video_codecs: &["vp9", "av1"],
        audio_codecs: &["opus"],
        faststart: false,
        muxer: "webm",
        subtitle_codec: Some("webvtt"),
        image_sequence: false,
    },
    ContainerDef {
        id: "wav",
        label: "WAV (nur Audio)",
        ext: "wav",
        video: false,
        video_codecs: &[],
        audio_codecs: &["pcm24", "pcm16", "pcm32f"],
        faststart: false,
        muxer: "wav",
        subtitle_codec: None,
        image_sequence: false,
    },
    ContainerDef {
        id: "mp3",
        label: "MP3 (nur Audio)",
        ext: "mp3",
        video: false,
        video_codecs: &[],
        audio_codecs: &["mp3"],
        faststart: false,
        muxer: "mp3",
        subtitle_codec: None,
        image_sequence: false,
    },
    ContainerDef {
        id: "flac",
        label: "FLAC (nur Audio)",
        ext: "flac",
        video: false,
        video_codecs: &[],
        audio_codecs: &["flac"],
        faststart: false,
        muxer: "flac",
        subtitle_codec: None,
        image_sequence: false,
    },
    ContainerDef {
        id: "m4a",
        label: "AAC (M4A, nur Audio)",
        ext: "m4a",
        video: false,
        video_codecs: &[],
        audio_codecs: &["aac"],
        faststart: true,
        muxer: "ipod",
        subtitle_codec: None,
        image_sequence: false,
    },
    // ---- Bild-Sequenzen (nur Video; jeder Frame eine Datei) ----
    ContainerDef {
        id: "png_seq",
        label: "PNG-Sequenz",
        ext: "png",
        video: true,
        video_codecs: &["png"],
        audio_codecs: &[],
        faststart: false,
        muxer: "image2",
        subtitle_codec: None,
        image_sequence: true,
    },
    ContainerDef {
        id: "jpg_seq",
        label: "JPEG-Sequenz",
        ext: "jpg",
        video: true,
        video_codecs: &["mjpeg"],
        audio_codecs: &[],
        faststart: false,
        muxer: "image2",
        subtitle_codec: None,
        image_sequence: true,
    },
    ContainerDef {
        id: "tiff_seq",
        label: "TIFF-Sequenz",
        ext: "tiff",
        video: true,
        video_codecs: &["tiff"],
        audio_codecs: &[],
        faststart: false,
        muxer: "image2",
        subtitle_codec: None,
        image_sequence: true,
    },
];

pub fn container(id: &str) -> &'static ContainerDef {
    CONTAINERS
        .iter()
        .find(|c| c.id == id)
        .unwrap_or(&CONTAINERS[0])
}

/// Qualitätssteuerung eines Video-Codecs.
pub enum QualityKind {
    /// CRF-Slider (min, max, default) — alternativ Ziel-Bitrate.
    CrfOrBitrate { crf: (u32, u32, u32) },
    /// Feste Profile (ffmpeg-`-profile:v`-Wert, Label, pix_fmt).
    Profiles(&'static [(&'static str, &'static str, &'static str)]),
}

/// Wie ein Encoder-Backend „konstante Qualität" ausdrückt — bestimmt das
/// ffmpeg-Flag und den Wertebereich des Qualitäts-Sliders. Hardware-Encoder
/// kennen kein CRF: NVENC nutzt CQ, Intel QSV `global_quality`, VAAPI QP,
/// VideoToolbox kann nur eine Ziel-Bitrate.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum EncoderQuality {
    /// `-crf N` (x264/x265/libvpx/SVT-AV1) — Bereich kommt vom Codec.
    Crf,
    /// NVENC: `-rc vbr -cq N -b:v 0` (min, max, default).
    Cq(u32, u32, u32),
    /// Intel QSV: `-global_quality N` (min, max, default).
    GlobalQuality(u32, u32, u32),
    /// VAAPI: `-rc_mode CQP -qp N` (min, max, default).
    Qp(u32, u32, u32),
    /// VideoToolbox: keine echte konstante Qualität — nur Ziel-Bitrate.
    BitrateOnly,
}

/// Ein Encoder-Backend einer Codec-Familie (Software oder Hardware). Ein
/// Codec (z. B. H.264) kann mehrere Backends haben; Hardware-Backends
/// erscheinen im Dialog nur, wenn `ffmpeg -encoders` sie listet.
pub struct EncoderDef {
    /// ffmpeg-Encodername (`-c:v`), zugleich Schlüssel für Verfügbarkeit.
    pub id: &'static str,
    pub label: &'static str,
    pub quality: EncoderQuality,
    /// VAAPI braucht ein Render-Device + Upload in eine GPU-Surface
    /// (`hwupload`) — eigener Argument-/Filterpfad.
    pub vaapi: bool,
}

impl EncoderDef {
    /// Software-Encoder (CRF) vs. Hardware-Backend (CQ/QP/Bitrate).
    pub fn is_hardware(&self) -> bool {
        !matches!(self.quality, EncoderQuality::Crf)
    }
    /// Slider-Bereich (min, max, default) der konstanten Qualität dieses
    /// Backends. Für `Crf`/`BitrateOnly` muss der Codec-Bereich herhalten.
    pub fn quality_range(&self, codec_crf: (u32, u32, u32)) -> (u32, u32, u32) {
        match self.quality {
            EncoderQuality::Crf => codec_crf,
            EncoderQuality::Cq(a, b, c)
            | EncoderQuality::GlobalQuality(a, b, c)
            | EncoderQuality::Qp(a, b, c) => (a, b, c),
            EncoderQuality::BitrateOnly => codec_crf,
        }
    }
    /// Kurzlabel des Qualitäts-Reglers im Dialog.
    pub fn quality_label(&self) -> &'static str {
        match self.quality {
            EncoderQuality::Crf => "CRF",
            EncoderQuality::Cq(..) => "CQ",
            EncoderQuality::GlobalQuality(..) => "Qualität",
            EncoderQuality::Qp(..) => "QP",
            EncoderQuality::BitrateOnly => "Bitrate",
        }
    }
    /// Backend bietet konstante Qualität (sonst nur Ziel-Bitrate).
    pub fn supports_constant_quality(&self) -> bool {
        !matches!(self.quality, EncoderQuality::BitrateOnly)
    }
}

/// Standard-Render-Node für VAAPI (überschreibbar via `EDITRON_VAAPI_DEVICE`).
pub fn vaapi_device() -> String {
    std::env::var("EDITRON_VAAPI_DEVICE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/dev/dri/renderD128".to_string())
}

const H264_ENCODERS: &[EncoderDef] = &[
    EncoderDef { id: "libx264", label: "Software (x264)", quality: EncoderQuality::Crf, vaapi: false },
    EncoderDef { id: "h264_nvenc", label: "Hardware — NVIDIA NVENC", quality: EncoderQuality::Cq(0, 51, 23), vaapi: false },
    EncoderDef { id: "h264_qsv", label: "Hardware — Intel QSV", quality: EncoderQuality::GlobalQuality(1, 51, 23), vaapi: false },
    EncoderDef { id: "h264_vaapi", label: "Hardware — VAAPI", quality: EncoderQuality::Qp(0, 52, 24), vaapi: true },
    EncoderDef { id: "h264_videotoolbox", label: "Hardware — VideoToolbox", quality: EncoderQuality::BitrateOnly, vaapi: false },
];
const HEVC_ENCODERS: &[EncoderDef] = &[
    EncoderDef { id: "libx265", label: "Software (x265)", quality: EncoderQuality::Crf, vaapi: false },
    EncoderDef { id: "hevc_nvenc", label: "Hardware — NVIDIA NVENC", quality: EncoderQuality::Cq(0, 51, 25), vaapi: false },
    EncoderDef { id: "hevc_qsv", label: "Hardware — Intel QSV", quality: EncoderQuality::GlobalQuality(1, 51, 25), vaapi: false },
    EncoderDef { id: "hevc_vaapi", label: "Hardware — VAAPI", quality: EncoderQuality::Qp(0, 52, 26), vaapi: true },
    EncoderDef { id: "hevc_videotoolbox", label: "Hardware — VideoToolbox", quality: EncoderQuality::BitrateOnly, vaapi: false },
];
const PRORES_ENCODERS: &[EncoderDef] =
    &[EncoderDef { id: "prores_ks", label: "Software (prores_ks)", quality: EncoderQuality::Crf, vaapi: false }];
const DNXHD_ENCODERS: &[EncoderDef] =
    &[EncoderDef { id: "dnxhd", label: "Software (dnxhd)", quality: EncoderQuality::Crf, vaapi: false }];
const VP9_ENCODERS: &[EncoderDef] =
    &[EncoderDef { id: "libvpx-vp9", label: "Software (libvpx)", quality: EncoderQuality::Crf, vaapi: false }];
const AV1_ENCODERS: &[EncoderDef] =
    &[EncoderDef { id: "libsvtav1", label: "Software (SVT-AV1)", quality: EncoderQuality::Crf, vaapi: false }];
const PNG_ENCODERS: &[EncoderDef] =
    &[EncoderDef { id: "png", label: "PNG", quality: EncoderQuality::Crf, vaapi: false }];
const MJPEG_ENCODERS: &[EncoderDef] =
    &[EncoderDef { id: "mjpeg", label: "JPEG", quality: EncoderQuality::Crf, vaapi: false }];
const TIFF_ENCODERS: &[EncoderDef] =
    &[EncoderDef { id: "tiff", label: "TIFF", quality: EncoderQuality::Crf, vaapi: false }];

pub struct VideoCodecDef {
    pub id: &'static str,
    pub label: &'static str,
    pub encoder: &'static str,
    pub quality: QualityKind,
    /// Encoder-Tempo (x264/x265: preset-Namen; SVT-AV1: Preset-Stufen).
    pub speed_presets: &'static [&'static str],
    pub default_speed: usize,
    /// Standard-Pixelformat (bei Profil-Codecs liefert das Profil das Format).
    pub pix_fmt: &'static str,
    /// Verfügbare Encoder-Backends; `[0]` ist immer der Software-Encoder
    /// (= `encoder`). Weitere Einträge sind Hardware-Backends.
    pub encoders: &'static [EncoderDef],
}

pub const VIDEO_CODECS: &[VideoCodecDef] = &[
    VideoCodecDef {
        id: "h264",
        label: "H.264 (libx264)",
        encoder: "libx264",
        quality: QualityKind::CrfOrBitrate { crf: (10, 36, 20) },
        speed_presets: &[
            "ultrafast", "superfast", "veryfast", "faster", "fast", "medium", "slow", "slower",
            "veryslow",
        ],
        default_speed: 5,
        pix_fmt: "yuv420p",
        encoders: H264_ENCODERS,
    },
    VideoCodecDef {
        id: "hevc",
        label: "H.265 / HEVC (libx265)",
        encoder: "libx265",
        quality: QualityKind::CrfOrBitrate { crf: (12, 40, 24) },
        speed_presets: &[
            "ultrafast", "superfast", "veryfast", "faster", "fast", "medium", "slow", "slower",
            "veryslow",
        ],
        default_speed: 5,
        pix_fmt: "yuv420p",
        encoders: HEVC_ENCODERS,
    },
    VideoCodecDef {
        id: "prores",
        label: "Apple ProRes",
        encoder: "prores_ks",
        quality: QualityKind::Profiles(&[
            ("0", "Proxy", "yuv422p10le"),
            ("1", "LT", "yuv422p10le"),
            ("2", "422", "yuv422p10le"),
            ("3", "422 HQ", "yuv422p10le"),
            ("4", "4444", "yuv444p10le"),
        ]),
        speed_presets: &[],
        default_speed: 0,
        pix_fmt: "yuv422p10le",
        encoders: PRORES_ENCODERS,
    },
    VideoCodecDef {
        id: "dnxhr",
        label: "Avid DNxHR",
        encoder: "dnxhd",
        quality: QualityKind::Profiles(&[
            ("dnxhr_lb", "LB (Proxy)", "yuv422p"),
            ("dnxhr_sq", "SQ", "yuv422p"),
            ("dnxhr_hq", "HQ", "yuv422p"),
            ("dnxhr_hqx", "HQX (10-bit)", "yuv422p10le"),
            ("dnxhr_444", "444 (10-bit)", "yuv444p10le"),
        ]),
        speed_presets: &[],
        default_speed: 0,
        pix_fmt: "yuv422p",
        encoders: DNXHD_ENCODERS,
    },
    VideoCodecDef {
        id: "vp9",
        label: "VP9 (libvpx)",
        encoder: "libvpx-vp9",
        quality: QualityKind::CrfOrBitrate { crf: (15, 50, 32) },
        speed_presets: &[],
        default_speed: 0,
        pix_fmt: "yuv420p",
        encoders: VP9_ENCODERS,
    },
    VideoCodecDef {
        id: "av1",
        label: "AV1 (SVT-AV1)",
        encoder: "libsvtav1",
        quality: QualityKind::CrfOrBitrate { crf: (15, 50, 30) },
        speed_presets: &["12", "10", "8", "6", "4", "2"],
        default_speed: 2,
        pix_fmt: "yuv420p",
        encoders: AV1_ENCODERS,
    },
    // ---- Bild-Codecs (nur für Bild-Sequenz-Container) ----
    VideoCodecDef {
        id: "png",
        label: "PNG (verlustfrei)",
        encoder: "png",
        quality: QualityKind::CrfOrBitrate { crf: (0, 0, 0) },
        speed_presets: &[],
        default_speed: 0,
        pix_fmt: "rgba",
        encoders: PNG_ENCODERS,
    },
    VideoCodecDef {
        id: "mjpeg",
        label: "JPEG",
        encoder: "mjpeg",
        // CRF-Feld trägt hier die JPEG-Qualität (`-q:v`, 2 = beste … 31).
        quality: QualityKind::CrfOrBitrate { crf: (2, 31, 3) },
        speed_presets: &[],
        default_speed: 0,
        pix_fmt: "yuvj420p",
        encoders: MJPEG_ENCODERS,
    },
    VideoCodecDef {
        id: "tiff",
        label: "TIFF (verlustfrei)",
        encoder: "tiff",
        quality: QualityKind::CrfOrBitrate { crf: (0, 0, 0) },
        speed_presets: &[],
        default_speed: 0,
        pix_fmt: "rgb24",
        encoders: TIFF_ENCODERS,
    },
];

/// Encoder-Backends einer Codec-Familie (`[0]` = Software).
pub fn encoders_for(codec_id: &str) -> &'static [EncoderDef] {
    video_codec(codec_id).encoders
}

/// Encoder-Backend nach ffmpeg-Id finden; Fallback = Software (`[0]`).
pub fn encoder_def(codec_id: &str, encoder_id: &str) -> &'static EncoderDef {
    let list = encoders_for(codec_id);
    list.iter().find(|e| e.id == encoder_id).unwrap_or(&list[0])
}

/// Encoder-Backends eines Codecs, gefiltert nach Verfügbarkeit: der
/// Software-Encoder ist immer dabei; Hardware-Backends nur, wenn die
/// ffmpeg-Encoder-Liste sie listet. Ist die Liste noch unbekannt (`None`),
/// werden alle gezeigt (die Validierung blockiert fehlende später).
pub fn available_video_encoders(
    codec_id: &str,
    available: Option<&HashSet<String>>,
) -> Vec<&'static EncoderDef> {
    encoders_for(codec_id)
        .iter()
        .filter(|e| match available {
            Some(set) => !e.is_hardware() || set.contains(e.id),
            None => true,
        })
        .collect()
}

pub fn video_codec(id: &str) -> &'static VideoCodecDef {
    VIDEO_CODECS
        .iter()
        .find(|c| c.id == id)
        .unwrap_or(&VIDEO_CODECS[0])
}

pub struct AudioCodecDef {
    pub id: &'static str,
    pub label: &'static str,
    pub encoder: &'static str,
    /// Wählbare Bitraten in kbit/s; leer = verlustfrei (keine Bitrate).
    pub bitrates: &'static [u32],
    pub default_bitrate: u32,
    /// Erzwungene Samplerate (Opus kann nur 48 kHz).
    pub forced_rate: Option<u32>,
}

pub const AUDIO_CODECS: &[AudioCodecDef] = &[
    AudioCodecDef {
        id: "aac",
        label: "AAC",
        encoder: "aac",
        bitrates: &[96, 128, 160, 192, 256, 320],
        default_bitrate: 192,
        forced_rate: None,
    },
    AudioCodecDef {
        id: "mp3",
        label: "MP3 (LAME)",
        encoder: "libmp3lame",
        bitrates: &[128, 160, 192, 256, 320],
        default_bitrate: 256,
        forced_rate: None,
    },
    AudioCodecDef {
        id: "opus",
        label: "Opus",
        encoder: "libopus",
        bitrates: &[64, 96, 128, 160, 192, 256],
        default_bitrate: 128,
        forced_rate: Some(48000),
    },
    AudioCodecDef {
        id: "flac",
        label: "FLAC (verlustfrei)",
        encoder: "flac",
        bitrates: &[],
        default_bitrate: 0,
        forced_rate: None,
    },
    AudioCodecDef {
        id: "pcm16",
        label: "PCM 16-bit",
        encoder: "pcm_s16le",
        bitrates: &[],
        default_bitrate: 0,
        forced_rate: None,
    },
    AudioCodecDef {
        id: "pcm24",
        label: "PCM 24-bit",
        encoder: "pcm_s24le",
        bitrates: &[],
        default_bitrate: 0,
        forced_rate: None,
    },
    AudioCodecDef {
        id: "pcm32f",
        label: "PCM 32-bit Float",
        encoder: "pcm_f32le",
        bitrates: &[],
        default_bitrate: 0,
        forced_rate: None,
    },
];

pub fn audio_codec(id: &str) -> &'static AudioCodecDef {
    AUDIO_CODECS
        .iter()
        .find(|c| c.id == id)
        .unwrap_or(&AUDIO_CODECS[0])
}

/// Auflösungs-Vorgaben (Label, Breite, Höhe).
pub const RESOLUTIONS: &[(&str, u32, u32)] = &[
    ("4K UHD — 3840×2160", 3840, 2160),
    ("QHD — 2560×1440", 2560, 1440),
    ("Full HD — 1920×1080", 1920, 1080),
    ("HD — 1280×720", 1280, 720),
    ("SD — 854×480", 854, 480),
];

/// Framerate-Vorgaben (Label, fps). NTSC-Raten als exakte Brüche — die
/// Renderplan-Quantisierung über lange Dauern verlangt die echte Rate
/// (29,97 gerundet driftet nach 10 h um einen Frame).
pub const FRAMERATES: &[(&str, f64)] = &[
    ("23,976", 24000.0 / 1001.0),
    ("24", 24.0),
    ("25", 25.0),
    ("29,97", 30000.0 / 1001.0),
    ("30", 30.0),
    ("50", 50.0),
    ("59,94", 60000.0 / 1001.0),
    ("60", 60.0),
];

pub const SAMPLE_RATES: &[u32] = &[44100, 48000, 96000];

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
    /// Nur den Bereich zwischen Sequenz-In/Out exportieren.
    pub use_in_out: bool,
    /// Sichtbare Untertitel-Spuren: ignorieren, Sidecar, muxen, einbrennen.
    pub subtitles: SubtitleMode,
    /// Startnummer der Bild-Sequenz (nur bei `container.image_sequence`).
    pub image_start: u32,
    pub output: String,
}

fn default_video(codec_id: &str, width: u32, height: u32, fps: f64) -> VideoSettings {
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

fn default_audio(codec_id: &str, bitrate: Option<u32>) -> AudioSettings {
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
        use_in_out: false,
        subtitles: SubtitleMode::None,
        image_start: 1,
        output: String::new(),
    }
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

// ============================================================== Renderplan

/// Übergangs-Fenster eines Layers: Zeiten relativ zum Exportbeginn.
/// Außerhalb des Fensters wird der Fortschritt geklemmt (0 davor, 1 danach)
/// — die Rollen-Formeln liefern dort die Identität.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanTransition {
    pub kind: TransitionKind,
    pub direction: TransitionDirection,
    pub role: TransitionRole,
    pub t0: f64,
    pub t1: f64,
}

impl PlanTransition {
    /// Auswirkung zur Exportzeit `t` (gemeinsame Formeln mit der Vorschau).
    pub fn eval(&self, t: f64) -> TransitionFx {
        let span = (self.t1 - self.t0).max(1e-9);
        let p = ((t - self.t0) / span).clamp(0.0, 1.0);
        transitions::eval_video(self.kind, self.direction, self.role, p)
    }
}

/// Kombinierte Übergangs-Auswirkung mehrerer Fenster (Clipanfang + -ende).
pub fn eval_plan_transitions(list: &[PlanTransition], t: f64) -> TransitionFx {
    list.iter()
        .fold(TransitionFx::IDENTITY, |acc, tr| acc.combine(&tr.eval(t)))
}

/// Ein Video-Layer eines Segments (Zeichenreihenfolge: unten → oben).
#[derive(Clone, Debug)]
pub struct VideoLayerPlan {
    pub clip_id: String,
    pub path: String,
    pub image: bool,
    /// Medienzeit des ersten Segment-Frames.
    pub src_in: f64,
    /// Medienfortschritt pro Ausgabesekunde (signiert): speed vorwärts,
    /// −speed rückwärts, 0 = Standbild. Medienzeit von Frame f =
    /// src_in + (f/fps)·media_step — identisch zur Vorschau-Abbildung.
    pub media_step: f64,
    /// Animierbare Parameter (Keyframes in Medienzeit).
    pub fx: ClipFx,
    /// Farbkorrektur des Clips (statisch).
    pub grade: ColorGrade,
    /// Effekt-Stapel des Clips (Keyframes in Medienzeit).
    pub effects: Vec<EffectInstance>,
    /// Natürliche Quellmaße (0 = unbekannt) — Bezugsrahmen der Vignette
    /// im transparent gepolsterten Decode-Puffer.
    pub natural_w: u32,
    pub natural_h: u32,
    /// Bittiefe der Quelle (8/10/12/16). >8 ⇒ Decode in 16 Bit (rgba64le)
    /// statt 8 (rgba), damit 10-Bit-Material ohne Banding durch die
    /// f32-Pipeline läuft. Generatoren/Farbflächen: 8.
    pub src_bit_depth: u32,
    /// Aktive Übergangs-Fenster dieses Layers im Segment.
    pub transitions: Vec<PlanTransition>,
    /// Farbfläche (Dip zu Schwarz/Weiß) statt Medien — ohne Decoder.
    pub solid: Option<[u8; 3]>,
    /// Titel-Generator statt Medien — CPU-gerastert, ohne Decoder.
    pub title: Option<crate::core::title::TitleSpec>,
    /// Verschachtelte Sequenz statt Medien: die Ebene wird rekursiv aus
    /// `RenderPlan::nests` komponiert (ID der inneren Sequenz).
    pub nest_seq: Option<String>,
}

impl VideoLayerPlan {
    /// Schnellpfad-Kriterium: keinerlei visuelle Transformation/Korrektur.
    /// Titel laufen immer durch den Compositor (es gibt keine Quelldatei,
    /// die ffmpeg direkt durchpumpen könnte).
    pub fn is_identity(&self) -> bool {
        self.transitions.is_empty()
            && self.solid.is_none()
            && self.title.is_none()
            // Nest-Ebenen werden immer rekursiv komponiert (kein Schnellpfad).
            && self.nest_seq.is_none()
            && self.media_step >= 0.0
            && self.fx.is_visual_identity()
            && !self.grade.is_active()
            && !effects::has_active_video_effects(&self.effects)
    }
}

#[derive(Clone, Debug)]
pub struct VideoSegment {
    pub frames: u64,
    /// Leer = Schwarzbild (Lücke).
    pub layers: Vec<VideoLayerPlan>,
}

/// Crossfade-Hüllkurve im Mix: Fenster in Mix-Zeit (Sekunden ab Exportbeginn).
#[derive(Clone, Debug, PartialEq)]
pub struct PlanAudioFade {
    pub t0: f64,
    pub t1: f64,
    /// true = eingehende Seite (Einblenden), false = ausgehende (Ausblenden).
    pub fade_in: bool,
    /// Konstante Leistung (sin/cos) statt konstanter Verstärkung (linear).
    pub equal_power: bool,
}

impl PlanAudioFade {
    /// Verstärkungsfaktor zur Mix-Zeit `t` (identische Kurven wie der Player).
    pub fn gain_at(&self, t: f64) -> f64 {
        let span = (self.t1 - self.t0).max(1e-9);
        let p = ((t - self.t0) / span).clamp(0.0, 1.0);
        transitions::audio_gain(self.equal_power, self.fade_in, p)
    }
}

#[derive(Clone, Debug)]
pub struct AudioClipPlan {
    pub path: String,
    /// Startzeit im Mix (Sekunden ab Exportbeginn).
    pub start_in_mix: f64,
    pub duration: f64,
    pub src_in: f64,
    /// Clip-Geschwindigkeit: Medienspanne = duration × speed; die Wiedergabe
    /// läuft pitch-korrigiert über dieselbe atempo-Kette wie der Player.
    pub speed: f64,
    /// Wirksamer Faktor je Seite: Master × Spur × Clip × Balance.
    pub gain_l: f32,
    pub gain_r: f32,
    /// Lautstärke-Kurve des Clips (dB, Keyframes in Medienzeit).
    pub volume: AnimatedParam,
    /// Audio-Effekt-Stapel des Clips (DSP vor Gain/Hüllkurve).
    pub effects: Vec<EffectInstance>,
    /// Crossfade-Fenster (Übergänge an den Clipkanten).
    pub fades: Vec<PlanAudioFade>,
}

/// Audio-Spur mit Bus-Effekten und/oder Automation: wird getrennt gemischt
/// (Clips → Per-Spur-WAV → Bus-FX + Spur-Gain/Pan + Master → Master-WAV),
/// damit die Effekte auf die SUMME der Spur wirken — exakt wie der Player-
/// Mixdown. Spuren ohne FX/Automation laufen über den Schnellpfad
/// (`RenderPlan::audio`, Gains fertig eingebacken).
#[derive(Clone, Debug)]
pub struct AudioTrackPlan {
    /// Clips der Spur; `gain_l`/`gain_r` enthalten NUR den Clip-Anteil
    /// (Clip-Gain), Spur-Gain/Pan und Master folgen in der Bus-Verarbeitung.
    pub clips: Vec<AudioClipPlan>,
    /// Bus-Effekt-Kette (Insert) der Spur.
    pub effects: Vec<EffectInstance>,
    /// Lautstärke-Automation (dB-Offset, Keyframes in Sequenzzeit).
    pub volume_auto: AnimatedParam,
    /// Pan-Automation (Offset, Keyframes in Sequenzzeit).
    pub pan_auto: AnimatedParam,
    /// Statischer Spur-Fader (dB) und Balance.
    pub gain_db: f64,
    pub pan: f64,
    /// Master-Fader (dB) — beim Summieren in den Master angewendet.
    pub master_db: f64,
    /// Sequenzzeit, die Mix-Zeit t=0 entspricht (Exportbeginn) — für die
    /// Automations-Auswertung.
    pub seq_start: f64,
}

impl AudioTrackPlan {
    /// Wirksame Spur-Verstärkung (dB) zur Mix-Zeit `mix_t`: Fader + Automation.
    fn gain_db_at(&self, mix_t: f64) -> f64 {
        self.gain_db + self.volume_auto.eval(self.seq_start + mix_t)
    }

    /// Wirksame Balance zur Mix-Zeit `mix_t`: Fader + Automation (geklemmt).
    fn pan_at(&self, mix_t: f64) -> f64 {
        (self.pan + self.pan_auto.eval(self.seq_start + mix_t)).clamp(-1.0, 1.0)
    }
}

/// Untertitel-Spur im Renderplan (Sidecar/Einbetten): Cue-Zeiten relativ
/// zum Exportbeginn, frame-genau aufs Sequenzraster gerundet.
#[derive(Clone, Debug)]
pub struct SubtitlePlanTrack {
    /// Spurname (U1, U2, …) — Dateisuffix bzw. Stream-Titel.
    pub name: String,
    pub cues: Vec<crate::core::subtitle::SrtCue>,
}

/// Renderbare Momentaufnahme einer (verschachtelten) Sequenz für den Worker:
/// reine Daten, aus denen sich eine [`TimelineStore`] rekonstruieren lässt.
#[derive(Clone, Debug)]
pub struct NestSeq {
    pub settings: SequenceSettings,
    pub tracks: Vec<TimelineTrack>,
    pub clips: Vec<TimelineClip>,
    pub transitions: Vec<Transition>,
}

impl NestSeq {
    /// In eine voll funktionsfähige Timeline überführen (für die Komposition).
    pub fn to_timeline(&self) -> TimelineStore {
        let mut tl = TimelineStore::default();
        tl.load_document(
            Some(self.settings),
            self.tracks.clone(),
            self.clips.clone(),
            self.transitions.clone(),
            Vec::new(),
            0.0,
            None,
            None,
            40.0,
            true,
            Vec::new(),
            0.0,
            None,
        );
        tl
    }
}

/// Decode-Info eines Blatt-Clips innerhalb verschachtelter Sequenzen
/// (asset_id → Originalpfad + natürliche Maße). Der Export nutzt IMMER das
/// Original.
#[derive(Clone, Debug)]
pub struct NestMediaInfo {
    pub path: String,
    pub natural_w: u32,
    pub natural_h: u32,
    pub image: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RenderPlan {
    pub duration: f64,
    /// Zielraster (0 bei Audio-only).
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub total_frames: u64,
    pub segments: Vec<VideoSegment>,
    /// Verschachtelte Sequenzen, die von Nest-Clips referenziert werden
    /// (transitiv aufgelöst) — vom Worker zur rekursiven Komposition genutzt.
    pub nests: HashMap<String, NestSeq>,
    /// Medien-Decode-Info der Blatt-Clips in den verschachtelten Sequenzen.
    pub nest_media: HashMap<String, NestMediaInfo>,
    /// Clips von Spuren OHNE Bus-FX/Automation (Schnellpfad: Gains fertig
    /// eingebacken, mischen direkt in den Master).
    pub audio: Vec<AudioClipPlan>,
    /// Spuren MIT Bus-FX und/oder Automation — getrennte Bus-Verarbeitung.
    pub audio_tracks: Vec<AudioTrackPlan>,
    /// Sichtbare Untertitel-Spuren mit Cues im Exportbereich (nur bei
    /// `SubtitleMode::Sidecar`/`Embed` befüllt; Einbrennen läuft über die
    /// Video-Segmente).
    pub subtitle_tracks: Vec<SubtitlePlanTrack>,
    /// Erkannter Ausgabe-Farbraum (ehrliche Tags); aus dem dominanten
    /// Quellmaterial, BT.709 als SDR-Default.
    pub color: OutputColor,
}

impl RenderPlan {
    /// Mindestens ein echtes Video-Segment (kein reines Schwarzbild)?
    pub fn has_video_media(&self) -> bool {
        self.segments.iter().any(|s| !s.layers.is_empty())
    }

    /// Gesamte Audio-Arbeitseinheiten (Frames) für den Fortschritt: einfache
    /// Clips + Per-Spur-Clips + die Bus-Verarbeitungs-Durchläufe der Spuren.
    pub fn audio_total_units(&self, rate: u32) -> u64 {
        let frames = |d: f64| (d * rate as f64) as u64;
        let simple: u64 = self.audio.iter().map(|c| frames(c.duration)).sum();
        let tracks: u64 = self
            .audio_tracks
            .iter()
            .map(|t| {
                let clips: u64 = t.clips.iter().map(|c| frames(c.duration)).sum();
                // + ein voller Durchlauf über die Sequenzdauer (Bus-FX/Gain).
                clips + frames(self.duration)
            })
            .sum();
        simple + tracks
    }
}

/// dB → linearer Faktor; ≤ −60 dB gilt als −∞ (stumm). Identisch zum Player.
fn db_to_linear(db: f64) -> f32 {
    if db <= -60.0 {
        0.0
    } else {
        10f32.powf(db as f32 / 20.0)
    }
}

/// Stereo-Balance wie im Player: dämpft die abgewandte Seite.
fn pan_gains(pan: f64) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0) as f32;
    (1.0 - p.max(0.0), 1.0 + p.min(0.0))
}

/// Exportbereich: ganze Sequenz oder In/Out (sofern gültig gesetzt).
pub fn export_range(timeline: &TimelineStore, use_in_out: bool) -> (f64, f64) {
    let end = sequence_end(&timeline.clips);
    if use_in_out {
        if let (Some(i), Some(o)) = (timeline.in_point, timeline.out_point) {
            let a = i.max(0.0);
            let b = o.max(0.0);
            if b - a > 1e-9 {
                return (a, b);
            }
        }
    }
    (0.0, end)
}

/// Leerer Nest-Resolver für Aufrufer ohne Sequenz-Kontext (Validierung, Tests,
/// Einzelbild-Export). Nest-Clips bleiben dann unaufgelöst (schwarz).
pub struct NoNests;
impl compose::NestResolver for NoNests {
    fn nested_timeline(&self, _id: &str) -> Option<&TimelineStore> {
        None
    }
}

/// Verschachtelte Sequenzen (transitiv) + ihre Blatt-Medien in den Plan
/// einsammeln, damit der Worker sie self-contained rekursiv komponieren kann.
fn gather_nests(
    timeline: &TimelineStore,
    media: &MediaStore,
    nests: &dyn compose::NestResolver,
    plan: &mut RenderPlan,
) {
    use std::collections::VecDeque;
    let mut queue: VecDeque<String> =
        timeline.clips.iter().filter_map(|c| c.nest_seq.clone()).collect();
    while let Some(id) = queue.pop_front() {
        if plan.nests.contains_key(&id) {
            continue;
        }
        let Some(inner) = nests.nested_timeline(&id) else {
            continue;
        };
        plan.nests.insert(
            id.clone(),
            NestSeq {
                settings: inner.settings,
                tracks: inner.tracks.clone(),
                clips: inner.clips.clone(),
                transitions: inner.transitions.clone(),
            },
        );
        for c in &inner.clips {
            if let Some(n) = &c.nest_seq {
                queue.push_back(n.clone());
            } else if !c.is_generator() {
                if let Some(a) = media.asset(&c.asset_id) {
                    if a.kind != MediaKind::Audio {
                        let (nw, nh) = a
                            .info
                            .video
                            .first()
                            .map(|v| (v.width, v.height))
                            .unwrap_or((0, 0));
                        plan.nest_media.entry(c.asset_id.clone()).or_insert(NestMediaInfo {
                            path: a.path.clone(),
                            natural_w: nw,
                            natural_h: nh,
                            image: a.kind == MediaKind::Image,
                        });
                    }
                }
            }
        }
    }
}

/// Audio einer verschachtelten Sequenz in den äußeren Mix einflachen: innere
/// Audio-Clips werden zeitverschoben und mit den inneren (Master/Spur/Clip)
/// und äußeren Gains gefaltet in `out` geschrieben. Rekursiv für tiefere
/// Nests. Bewusste v1-Grenzen: innere Spur-Bus-Effekte/-Automation und
/// Crossfades innerhalb der inneren Sequenz werden (noch) nicht berücksichtigt;
/// Nest-Clips werden mit Geschwindigkeit 1 angenommen.
#[allow(clippy::too_many_arguments)]
fn flatten_nest_audio(
    inner: &TimelineStore,
    media: &MediaStore,
    nests: &dyn compose::NestResolver,
    src_in: f64,
    dur: f64,
    outer_offset: f64,
    gain_l_acc: f32,
    gain_r_acc: f32,
    depth: usize,
    out: &mut Vec<AudioClipPlan>,
) {
    if depth >= compose::MAX_NEST_DEPTH {
        return;
    }
    let (win_lo, win_hi) = (src_in, src_in + dur);
    let solo_any = inner.tracks.iter().any(|t| t.solo);
    let inner_master = db_to_linear(inner.master_gain_db);
    for track in inner
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio && !t.muted && (!solo_any || t.solo))
    {
        let track_gain = db_to_linear(track.gain_db);
        let (pan_l, pan_r) = pan_gains(track.pan);
        for clip in inner.clips.iter().filter(|c| c.track_id == track.id && c.enabled) {
            if clip.reverse || clip.freeze {
                continue;
            }
            let lo = clip.start.max(win_lo);
            let hi = clip.end().min(win_hi);
            if hi - lo <= 1e-9 {
                continue;
            }
            let out_start = outer_offset + (lo - win_lo);
            // Tiefere Verschachtelung rekursiv einflachen.
            if let Some(inner_id) = &clip.nest_seq {
                if let Some(deeper) = nests.nested_timeline(inner_id) {
                    let gl = gain_l_acc * inner_master * track_gain * pan_l;
                    let gr = gain_r_acc * inner_master * track_gain * pan_r;
                    flatten_nest_audio(
                        deeper,
                        media,
                        nests,
                        clip.media_time_at(lo).max(0.0),
                        hi - lo,
                        out_start,
                        gl,
                        gr,
                        depth + 1,
                        out,
                    );
                }
                continue;
            }
            let Some(asset) = media.asset(&clip.asset_id) else {
                continue;
            };
            if asset.offline || asset.info.audio.is_empty() {
                continue;
            }
            let clip_gain = db_to_linear(clip.gain_db);
            out.push(AudioClipPlan {
                path: asset.path.clone(),
                start_in_mix: out_start,
                duration: hi - lo,
                src_in: clip.media_time_at(lo).max(0.0),
                speed: clip.eff_speed(),
                gain_l: gain_l_acc * inner_master * track_gain * pan_l * clip_gain,
                gain_r: gain_r_acc * inner_master * track_gain * pan_r * clip_gain,
                volume: clip.fx.volume_db.clone(),
                effects: clip.effects.iter().filter(|e| e.kind.is_audio()).cloned().collect(),
                fades: Vec::new(),
            });
        }
    }
}

/// Ausgabe-Farbraum aus dem Quellmaterial im Exportbereich erkennen. Ist eine
/// sichtbare Video-Quelle HDR (PQ/HLG) oder BT.2020, wird der „stärkste"
/// Farbraum gewählt (PQ > HLG > BT.2020 > BT.709) und durchgereicht; sonst
/// BT.709. Multicam-Winkel und (rekursiv) Nest-Inhalte werden hier (noch)
/// nicht inspiziert — sie fallen auf BT.709 zurück.
fn detect_output_color(
    timeline: &TimelineStore,
    media: &MediaStore,
    start: f64,
    end: f64,
) -> OutputColor {
    let mut best = OutputColor::Bt709;
    let rank = |c: OutputColor| match c {
        OutputColor::Bt709 => 0,
        OutputColor::Bt2020 => 1,
        OutputColor::Bt2020Hlg => 2,
        OutputColor::Bt2020Pq => 3,
    };
    for clip in &timeline.clips {
        if !clip.enabled || clip.start >= end || clip.end() <= start {
            continue;
        }
        let Some(track) = timeline.tracks.iter().find(|t| t.id == clip.track_id) else {
            continue;
        };
        if track.kind != TrackKind::Video {
            continue;
        }
        if let Some(asset) = media.asset(&clip.asset_id) {
            if let Some(v) = asset.info.video.first() {
                let c = OutputColor::from_stream(v);
                if rank(c) > rank(best) {
                    best = c;
                }
            }
        }
    }
    best
}

pub fn build_render_plan(
    timeline: &TimelineStore,
    media: &MediaStore,
    settings: &ExportSettings,
    nests: &dyn compose::NestResolver,
) -> RenderPlan {
    let (start, end) = export_range(timeline, settings.use_in_out);
    let duration = (end - start).max(0.0);
    let mut plan = RenderPlan {
        duration,
        ..Default::default()
    };
    if duration <= 0.0 {
        return plan;
    }

    // Solo wirkt global über alle Spuren (Player-Semantik).
    let solo_any = timeline.tracks.iter().any(|t| t.solo);
    let master = db_to_linear(timeline.master_gain_db);

    if let Some(video) = &settings.video {
        plan.width = video.width;
        plan.height = video.height;
        plan.fps = video.fps;
        plan.total_frames = ((duration * video.fps).round() as u64).max(1);
        // Ausgabe-Farbraum aus dem Quellmaterial im Exportbereich erkennen:
        // ist eine sichtbare Quelle Wide-Gamut/HDR (BT.2020/PQ/HLG), wird der
        // Farbraum durchgereicht statt nach BT.709 fehlgetaggt. Sonst BT.709.
        plan.color = detect_output_color(timeline, media, start, end);
        plan.segments = plan_video_segments(
            timeline,
            media,
            nests,
            start,
            plan.total_frames,
            video.fps,
            solo_any,
            settings.subtitles == SubtitleMode::BurnIn,
        );
        // Verschachtelte Sequenzen (transitiv) + ihre Blatt-Medien einsammeln,
        // damit der Worker sie self-contained rekursiv komponieren kann.
        gather_nests(timeline, media, nests, &mut plan);
    }

    // Untertitel für Sidecar/Einbetten: sichtbare Spuren (U1 zuerst), Cues
    // auf den Exportbereich beschnitten und auf ihn bezogen (t=0 = Beginn).
    if matches!(settings.subtitles, SubtitleMode::Sidecar | SubtitleMode::Embed) {
        for track in timeline
            .tracks
            .iter()
            .rev()
            .filter(|t| t.kind == TrackKind::Subtitle && !t.muted)
        {
            let cues: Vec<crate::core::subtitle::SrtCue> = timeline
                .subtitle_cues(&track.id)
                .into_iter()
                .filter_map(|c| {
                    let s = c.start.max(start);
                    let e = c.end.min(end);
                    (e - s > 1e-9).then_some(crate::core::subtitle::SrtCue {
                        start: s - start,
                        end: e - start,
                        text: c.text,
                    })
                })
                .collect();
            if !cues.is_empty() {
                plan.subtitle_tracks.push(SubtitlePlanTrack {
                    name: crate::core::timeline::track_name(track, &timeline.tracks),
                    cues,
                });
            }
        }
    }

    if settings.audio.is_some() {
        for track in timeline
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio && !t.muted && (!solo_any || t.solo))
        {
            // Spuren mit Bus-FX oder Automation brauchen die getrennte Bus-
            // Verarbeitung (Effekte/Automation wirken auf die Spur-SUMME);
            // alle anderen laufen über den Schnellpfad mit fertig
            // eingebackenen Gains.
            let processed = track.has_audio_effects() || track.has_automation();
            let track_gain = db_to_linear(track.gain_db);
            let (pan_l, pan_r) = pan_gains(track.pan);
            let mut track_clips: Vec<AudioClipPlan> = Vec::new();
            for clip in timeline
                .clips
                .iter()
                .filter(|c| c.track_id == track.id && c.enabled)
            {
                // Rückwärts-Clips sind (vorerst) stumm, Standbilder ohnehin —
                // identisch zur Wiedergabe.
                if clip.reverse || clip.freeze {
                    continue;
                }
                // Verschachtelte Sequenz: das innere Audio rekursiv einflachen
                // (zeitverschoben, Gains gefaltet). Bypasst die Bus-FX der
                // äußeren Spur (v1) → direkt in den Schnellpfad-Master.
                if let Some(inner_id) = &clip.nest_seq {
                    if let Some(inner) = nests.nested_timeline(inner_id) {
                        let lo = clip.start.max(start);
                        let hi = clip.end().min(end);
                        if hi - lo > 1e-9 {
                            let cg = db_to_linear(clip.gain_db);
                            let gl = master * track_gain * pan_l * cg;
                            let gr = master * track_gain * pan_r * cg;
                            flatten_nest_audio(
                                inner,
                                media,
                                nests,
                                clip.media_time_at(lo).max(0.0),
                                hi - lo,
                                lo - start,
                                gl,
                                gr,
                                1,
                                &mut plan.audio,
                            );
                        }
                    }
                    continue;
                }
                // Audio-Crossfades verlängern den hörbaren Bereich über die
                // Clipkanten hinaus (Handles im Modell garantiert).
                let fades = timeline.audio_fades(clip);
                let (ext0, ext1) = timeline.audio_extent(clip, &fades);
                let clip_start = ext0.max(start);
                let clip_end = ext1.min(end);
                if clip_end - clip_start <= 1e-9 {
                    continue;
                }
                // Multicam: Audio-Winkel-Asset auflösen (Original; Medienzeit um
                // die Winkel-`pos` versetzt). Sonst das Clip-Asset.
                let (asset, audio_pos) = if let Some(mc) = &clip.multicam {
                    let Some(src) = nests
                        .nested_timeline(&mc.source)
                        .and_then(|t| t.multicam.as_ref())
                    else {
                        continue;
                    };
                    let aidx = src.audio_angle_idx(mc.angle);
                    let Some(angle) = src.angles.get(aidx).filter(|a| a.has_audio) else {
                        continue;
                    };
                    let Some(asset) = media.asset(&angle.asset_id) else {
                        continue;
                    };
                    (asset, angle.pos)
                } else {
                    let Some(asset) = media.asset(&clip.asset_id) else {
                        continue;
                    };
                    (asset, 0.0)
                };
                if asset.offline || asset.info.audio.is_empty() {
                    continue;
                }
                // Schnellpfad: Master × Spur × Clip × Balance eingebacken.
                // Bus-Pfad: nur Clip-Gain (mono) — Spur/Pan/Master folgen in
                // der Bus-Verarbeitung.
                let (gl, gr) = if processed {
                    let g = db_to_linear(clip.gain_db);
                    (g, g)
                } else {
                    let gain = master * track_gain * db_to_linear(clip.gain_db);
                    (gain * pan_l, gain * pan_r)
                };
                let cp = AudioClipPlan {
                    path: asset.path.clone(),
                    start_in_mix: clip_start - start,
                    duration: clip_end - clip_start,
                    // Medienzeit am Mix-Beginn (zentrale Abbildung, vorwärts);
                    // bei Multicam um die Audio-Winkel-`pos` versetzt.
                    src_in: (clip.media_time_at(clip_start) - audio_pos).max(0.0),
                    speed: clip.eff_speed(),
                    gain_l: gl,
                    gain_r: gr,
                    volume: clip.fx.volume_db.clone(),
                    effects: clip
                        .effects
                        .iter()
                        .filter(|e| e.kind.is_audio())
                        .cloned()
                        .collect(),
                    fades: fades
                        .iter()
                        .map(|(w0, w1, fade_in, equal_power)| PlanAudioFade {
                            t0: w0 - start,
                            t1: w1 - start,
                            fade_in: *fade_in,
                            equal_power: *equal_power,
                        })
                        .collect(),
                };
                if processed {
                    track_clips.push(cp);
                } else {
                    plan.audio.push(cp);
                }
            }
            if processed && !track_clips.is_empty() {
                track_clips.sort_by(|a, b| a.start_in_mix.total_cmp(&b.start_in_mix));
                plan.audio_tracks.push(AudioTrackPlan {
                    clips: track_clips,
                    effects: track
                        .effects
                        .iter()
                        .filter(|e| e.kind.is_audio())
                        .cloned()
                        .collect(),
                    volume_auto: track.volume_auto.clone(),
                    pan_auto: track.pan_auto.clone(),
                    gain_db: track.gain_db,
                    pan: track.pan,
                    master_db: timeline.master_gain_db,
                    seq_start: start,
                });
            }
        }
        plan.audio
            .sort_by(|a, b| a.start_in_mix.total_cmp(&b.start_in_mix));
    }

    plan
}

/// Video-Segmente: Zeitachse in Ziel-Frames quantisieren, an jeder
/// Clip-Grenze schneiden; je Abschnitt der komplette Layer-Stapel
/// (unten → oben) — der Renderer komponiert wie der Programmmonitor.
#[allow(clippy::too_many_arguments)]
fn plan_video_segments(
    timeline: &TimelineStore,
    media: &MediaStore,
    nests: &dyn compose::NestResolver,
    range_start: f64,
    total_frames: u64,
    fps: f64,
    solo_any: bool,
    burn_subtitles: bool,
) -> Vec<VideoSegment> {
    struct Candidate {
        /// 0 = unterste sichtbare Videospur (Zeichenreihenfolge).
        draw_order: usize,
        /// Innerhalb der Spur: Farbflächen (Dips) über den Clips.
        is_solid: bool,
        f0: u64,
        f1: u64,
        clip_id: String,
        clip_start: f64,
        clip_duration: f64,
        src_in: f64,
        /// Medienfortschritt pro Sequenzsekunde (signiert; 0 = Standbild).
        media_step: f64,
        path: String,
        image: bool,
        fx: ClipFx,
        grade: ColorGrade,
        effects: Vec<EffectInstance>,
        natural_w: u32,
        natural_h: u32,
        src_bit_depth: u32,
        transitions: Vec<PlanTransition>,
        solid: Option<[u8; 3]>,
        title: Option<crate::core::title::TitleSpec>,
        nest_seq: Option<String>,
    }

    let frame_of = |t: f64| -> u64 {
        (((t - range_start) * fps).round().max(0.0) as u64).min(total_frames)
    };

    let mut candidates: Vec<Candidate> = Vec::new();
    // Spur-Index 0 ist die OBERSTE Videospur → rückwärts = Zeichenreihenfolge.
    let video_tracks: Vec<&str> = timeline
        .tracks
        .iter()
        .rev()
        .filter(|t| t.kind == TrackKind::Video && !t.muted && (!solo_any || t.solo))
        .map(|t| t.id.as_str())
        .collect();
    // Sichtbare Untertitel-Spuren (nur beim Einbrennen): zeichnen über
    // ALLEN Videospuren, in derselben Reihenfolge wie der Programmmonitor.
    let subtitle_tracks: Vec<&str> = if burn_subtitles {
        timeline
            .tracks
            .iter()
            .rev()
            .filter(|t| t.kind == TrackKind::Subtitle && !t.muted)
            .map(|t| t.id.as_str())
            .collect()
    } else {
        Vec::new()
    };

    for clip in timeline.clips.iter().filter(|c| c.enabled) {
        // Untertitel-Segment: synthetisierter Titel-Spec aus Spurstil + Text
        // (identischer Rasterizer wie der Programmmonitor), ohne Decoder.
        if clip.is_subtitle() {
            let Some(sub_order) = subtitle_tracks.iter().position(|id| *id == clip.track_id)
            else {
                continue;
            };
            let f0 = frame_of(clip.start);
            let f1 = frame_of(clip.end());
            if f1 <= f0 {
                continue;
            }
            let Some(spec) = compose::layer_title_spec(timeline, clip) else {
                continue;
            };
            candidates.push(Candidate {
                draw_order: video_tracks.len() + sub_order,
                is_solid: false,
                f0,
                f1,
                clip_id: clip.id.clone(),
                clip_start: clip.start,
                clip_duration: clip.duration,
                src_in: clip.src_in,
                media_step: clip.media_step(),
                path: String::new(),
                image: false,
                fx: clip.fx.clone(),
                grade: clip.grade.clone(),
                effects: clip
                    .effects
                    .iter()
                    .filter(|e| !e.kind.is_audio())
                    .cloned()
                    .collect(),
                natural_w: 0,
                natural_h: 0,
                src_bit_depth: 8,
                transitions: Vec::new(),
                solid: None,
                title: Some(spec),
                nest_seq: None,
            });
            continue;
        }
        let Some(order) = video_tracks.iter().position(|id| *id == clip.track_id) else {
            continue;
        };
        let f0 = frame_of(clip.start);
        let f1 = frame_of(clip.end());
        if f1 <= f0 {
            continue;
        }
        // Titel-Generator: kein Asset/Decoder — der Renderer rastert den
        // Spec selbst (identischer Rasterizer wie der Programmmonitor).
        if let Some(spec) = &clip.title {
            candidates.push(Candidate {
                draw_order: order,
                is_solid: false,
                f0,
                f1,
                clip_id: clip.id.clone(),
                clip_start: clip.start,
                clip_duration: clip.duration,
                src_in: clip.src_in,
                media_step: clip.media_step(),
                path: String::new(),
                image: false,
                fx: clip.fx.clone(),
                grade: clip.grade.clone(),
                effects: clip
                    .effects
                    .iter()
                    .filter(|e| !e.kind.is_audio())
                    .cloned()
                    .collect(),
                natural_w: 0,
                natural_h: 0,
                src_bit_depth: 8,
                transitions: Vec::new(),
                solid: None,
                title: Some(spec.clone()),
                nest_seq: None,
            });
            continue;
        }
        // Verschachtelte Sequenz: kein Asset/Decoder — der Renderer komponiert
        // die innere Sequenz rekursiv (Auflösung = innere Sequenzgröße, damit
        // die Skalierung im äußeren Frame stimmt).
        if let Some(inner_id) = &clip.nest_seq {
            let Some(inner) = nests.nested_timeline(inner_id) else {
                continue;
            };
            let (nw, nh) = (inner.settings.width, inner.settings.height);
            candidates.push(Candidate {
                draw_order: order,
                is_solid: false,
                f0,
                f1,
                clip_id: clip.id.clone(),
                clip_start: clip.start,
                clip_duration: clip.duration,
                src_in: clip.src_in,
                media_step: clip.media_step(),
                path: String::new(),
                image: false,
                fx: clip.fx.clone(),
                grade: clip.grade.clone(),
                effects: clip
                    .effects
                    .iter()
                    .filter(|e| !e.kind.is_audio())
                    .cloned()
                    .collect(),
                natural_w: nw,
                natural_h: nh,
                src_bit_depth: 8,
                transitions: Vec::new(),
                solid: None,
                title: None,
                nest_seq: Some(inner_id.clone()),
            });
            continue;
        }
        // Multicam: aktiven Winkel zu einem normalen Medien-Blatt auflösen
        // (Asset = Winkel-Original, Medienzeit = gemeinsame Zeit − Winkel-pos).
        // Der EXPORT nutzt — wie überall — das ORIGINAL, nie den Proxy.
        if let Some(mc) = &clip.multicam {
            let Some(angle) = nests
                .nested_timeline(&mc.source)
                .and_then(|t| t.multicam.as_ref())
                .and_then(|s| s.angle(mc.angle))
            else {
                continue;
            };
            let Some(asset) = media.asset(&angle.asset_id) else {
                continue;
            };
            if asset.offline || asset.kind == MediaKind::Audio {
                continue;
            }
            let image = asset.kind == MediaKind::Image;
            if !image && asset.info.video.is_empty() {
                continue;
            }
            let (natural_w, natural_h, src_bit_depth) = asset
                .info
                .video
                .first()
                .map(|v| (v.width, v.height, v.bit_depth))
                .unwrap_or((0, 0, 8));
            candidates.push(Candidate {
                draw_order: order,
                is_solid: false,
                f0,
                f1,
                clip_id: clip.id.clone(),
                clip_start: clip.start,
                clip_duration: clip.duration,
                src_in: clip.src_in - angle.pos,
                media_step: clip.media_step(),
                path: asset.path.clone(),
                image,
                fx: clip.fx.clone(),
                grade: clip.grade.clone(),
                effects: clip
                    .effects
                    .iter()
                    .filter(|e| !e.kind.is_audio())
                    .cloned()
                    .collect(),
                natural_w,
                natural_h,
                src_bit_depth,
                transitions: Vec::new(),
                solid: None,
                title: None,
                nest_seq: None,
            });
            continue;
        }
        let Some(asset) = media.asset(&clip.asset_id) else {
            continue;
        };
        if asset.offline || asset.kind == MediaKind::Audio {
            continue;
        }
        let image = asset.kind == MediaKind::Image;
        if !image && asset.info.video.is_empty() {
            continue;
        }
        let (natural_w, natural_h, src_bit_depth) = asset
            .info
            .video
            .first()
            .map(|v| (v.width, v.height, v.bit_depth))
            .unwrap_or((0, 0, 8));
        candidates.push(Candidate {
            draw_order: order,
            is_solid: false,
            f0,
            f1,
            clip_id: clip.id.clone(),
            clip_start: clip.start,
            clip_duration: clip.duration,
            src_in: clip.src_in,
            media_step: clip.media_step(),
            path: asset.path.clone(),
            image,
            fx: clip.fx.clone(),
            grade: clip.grade.clone(),
            effects: clip
                .effects
                .iter()
                .filter(|e| !e.kind.is_audio())
                .cloned()
                .collect(),
            natural_w,
            natural_h,
            src_bit_depth,
            transitions: Vec::new(),
            solid: None,
            title: None,
            nest_seq: None,
        });
    }

    // Übergänge: Kandidaten über die Schnittkante hinaus verlängern (zwei
    // Decoder laufen im Fenster parallel), Fenster anheften, Dips als
    // Farbflächen-Kandidaten einplanen. Fensterkanten werden zusätzliche
    // Segmentgrenzen, damit Abschnitte ohne Übergang den Schnellpfad behalten.
    let mut extra_bounds: Vec<u64> = Vec::new();
    for tr in &timeline.transitions {
        if tr.kind.is_audio() {
            continue;
        }
        let (from, to) = transitions::resolve_clips(&timeline.clips, tr);
        let Some(anchor) = from.or(to) else { continue };
        let Some(track_order) = video_tracks.iter().position(|id| *id == anchor.track_id) else {
            continue;
        };
        let Some((w0, w1)) = transitions::window(from, to, tr.alignment, tr.duration) else {
            continue;
        };
        let (wf0, wf1) = (frame_of(w0), frame_of(w1));
        if wf1 <= wf0 {
            continue;
        }
        extra_bounds.push(wf0);
        extra_bounds.push(wf1);
        let two_sided = from.is_some() && to.is_some();
        let (t0, t1) = (w0 - range_start, w1 - range_start);
        if let Some(f) = from {
            if let Some(c) = candidates.iter_mut().find(|c| c.clip_id == f.id) {
                c.f1 = c.f1.max(wf1);
                c.transitions.push(PlanTransition {
                    kind: tr.kind,
                    direction: tr.direction,
                    role: if two_sided { TransitionRole::Out } else { TransitionRole::OutSolo },
                    t0,
                    t1,
                });
            }
        }
        if let Some(t) = to {
            if let Some(c) = candidates.iter_mut().find(|c| c.clip_id == t.id) {
                c.f0 = c.f0.min(wf0);
                c.transitions.push(PlanTransition {
                    kind: tr.kind,
                    direction: tr.direction,
                    role: if two_sided { TransitionRole::In } else { TransitionRole::InSolo },
                    t0,
                    t1,
                });
            }
        }
        if tr.kind.is_dip() {
            let role = if two_sided {
                TransitionRole::Dip
            } else if from.is_some() {
                TransitionRole::DipOut
            } else {
                TransitionRole::DipIn
            };
            let color = if tr.kind == TransitionKind::DipToWhite {
                [255u8, 255, 255]
            } else {
                [0u8, 0, 0]
            };
            candidates.push(Candidate {
                draw_order: track_order,
                is_solid: true,
                f0: wf0,
                f1: wf1,
                clip_id: format!("solid:{}", tr.id),
                clip_start: w0,
                clip_duration: w1 - w0,
                src_in: 0.0,
                media_step: 1.0,
                path: String::new(),
                image: false,
                fx: ClipFx::default(),
                grade: ColorGrade::default(),
                effects: Vec::new(),
                natural_w: 0,
                natural_h: 0,
                src_bit_depth: 8,
                transitions: vec![PlanTransition {
                    kind: tr.kind,
                    direction: tr.direction,
                    role,
                    t0,
                    t1,
                }],
                solid: Some(color),
                title: None,
                nest_seq: None,
            });
        }
    }

    // Schnittpunkte (in Frames) sammeln.
    let mut bounds: Vec<u64> = vec![0, total_frames];
    for c in &candidates {
        bounds.push(c.f0);
        bounds.push(c.f1);
    }
    bounds.extend(extra_bounds);
    bounds.sort_unstable();
    bounds.dedup();

    let mut segments: Vec<VideoSegment> = Vec::new();
    for pair in bounds.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if b <= a {
            continue;
        }
        let (seg_t0, seg_t1) = (a as f64 / fps, b as f64 / fps);
        let mut active: Vec<&Candidate> = candidates
            .iter()
            .filter(|c| c.f0 <= a && c.f1 >= b)
            .collect();
        // Zeichenreihenfolge: Spur, darin Clips vor Farbflächen; während
        // eines Übergangs liegt der später startende Clip OBEN.
        active.sort_by(|x, y| {
            x.draw_order
                .cmp(&y.draw_order)
                .then(x.is_solid.cmp(&y.is_solid))
                .then(x.clip_start.total_cmp(&y.clip_start))
        });
        let layers: Vec<VideoLayerPlan> = active
            .iter()
            .map(|c| VideoLayerPlan {
                clip_id: c.clip_id.clone(),
                path: c.path.clone(),
                image: c.image,
                // Medienzeit des Segmentbeginns aus der Sequenzzeit ableiten
                // (identische Formel wie `TimelineClip::media_time_at` —
                // rückwärts läuft die Spanne vom Medien-Out abwärts).
                src_in: {
                    let seq_t = range_start + a as f64 / fps;
                    let m = if c.media_step == 0.0 {
                        c.src_in
                    } else if c.media_step < 0.0 {
                        c.src_in + (c.clip_start + c.clip_duration - seq_t) * (-c.media_step)
                    } else {
                        c.src_in + (seq_t - c.clip_start) * c.media_step
                    };
                    m.max(0.0)
                },
                media_step: c.media_step,
                fx: c.fx.clone(),
                grade: c.grade.clone(),
                effects: c.effects.clone(),
                natural_w: c.natural_w,
                natural_h: c.natural_h,
                src_bit_depth: c.src_bit_depth,
                // Nur Fenster, die dieses Segment berühren — Abschnitte
                // außerhalb behalten den Schnellpfad und dürfen verschmelzen.
                transitions: c
                    .transitions
                    .iter()
                    .filter(|t| t.t1 > seg_t0 && t.t0 < seg_t1)
                    .cloned()
                    .collect(),
                solid: c.solid,
                title: c.title.clone(),
                nest_seq: c.nest_seq.clone(),
            })
            .collect();
        // Fortsetzungen desselben Layer-Stapels verschmelzen (spart
        // Decoder-Starts): gleiche Clips in gleicher Reihenfolge, die
        // Medienzeit jedes Video-Layers läuft nahtlos weiter und die
        // Übergangs-Fenster sind identisch.
        let frames = b - a;
        if let Some(last) = segments.last_mut() {
            let merge = last.layers.len() == layers.len()
                && last
                    .layers
                    .iter()
                    .zip(layers.iter())
                    .all(|(l1, l2)| {
                        let expected = l1.src_in + last.frames as f64 / fps * l1.media_step;
                        let tol = 0.5 / fps * l1.media_step.abs().max(1.0);
                        let continues = l1.media_step == l2.media_step
                            && (expected - l2.src_in).abs() < tol;
                        l1.clip_id == l2.clip_id
                            && (l2.image || l2.solid.is_some() || continues)
                            && l1.transitions == l2.transitions
                            && l1.solid == l2.solid
                    });
            if merge {
                last.frames += frames;
                continue;
            }
        }
        segments.push(VideoSegment { frames, layers });
    }
    segments
}

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
    if !plan.has_video_media() && plan.audio.is_empty() {
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
    if settings.audio.is_some() && plan.audio.is_empty() && settings.video.is_some() {
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

// ============================================================ Render-Worker

/// Phase des laufenden Exports (für die Fortschrittsanzeige).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExportPhase {
    MixAudio,
    RenderVideo,
    EncodeAudio,
    Finalize,
}

impl ExportPhase {
    pub fn label(&self) -> &'static str {
        match self {
            ExportPhase::MixAudio => "Audio wird gemischt",
            ExportPhase::RenderVideo => "Video wird gerendert",
            ExportPhase::EncodeAudio => "Audio wird kodiert",
            ExportPhase::Finalize => "Datei wird abgeschlossen",
        }
    }
}

enum ExportError {
    Cancelled,
    Failed(String),
}

type ChildList = Arc<Mutex<Vec<(u64, Child)>>>;

/// Vom Worker getrackte Kindprozesse — `cancel_job` killt sie von außen,
/// damit blockierende Pipe-Reads/-Writes sofort enden.
struct ChildRegistry {
    list: ChildList,
    next: u64,
}

impl ChildRegistry {
    fn new(list: ChildList) -> ChildRegistry {
        ChildRegistry { list, next: 0 }
    }

    fn spawn(&mut self, cmd: &mut Command) -> Result<(u64, Option<std::process::ChildStdin>, Option<std::process::ChildStdout>, Option<std::process::ChildStderr>), String> {
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("ffmpeg konnte nicht gestartet werden: {e}"))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let id = self.next;
        self.next += 1;
        self.list.lock().unwrap_or_else(|p| p.into_inner()).push((id, child));
        Ok((id, stdin, stdout, stderr))
    }

    /// Prozess beenden lassen und Status liefern (entfernt ihn aus der Liste).
    fn wait(&self, id: u64) -> Option<std::process::ExitStatus> {
        let mut list = self.list.lock().unwrap_or_else(|p| p.into_inner());
        let idx = list.iter().position(|(i, _)| *i == id)?;
        let (_, mut child) = list.swap_remove(idx);
        drop(list);
        child.wait().ok()
    }

    /// Prozess hart beenden (z. B. Decoder am Segmentende).
    fn kill(&self, id: u64) {
        let mut list = self.list.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(idx) = list.iter().position(|(i, _)| *i == id) {
            let (_, mut child) = list.swap_remove(idx);
            drop(list);
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn kill_all(&self) {
        let mut list = self.list.lock().unwrap_or_else(|p| p.into_inner());
        for (_, child) in list.iter_mut() {
            let _ = child.kill();
        }
        for (_, mut child) in list.drain(..) {
            let _ = child.wait();
        }
    }
}

/// Fortschritts-Tracker: Einheiten (Frames/Samples) → %, Rate, ETA.
/// Fortschritts-Senke des Compositing-Kerns (`render_segments`). Entkoppelt
/// den geteilten Renderpfad vom konkreten Event-Typ: Voll-Export meldet über
/// [`Progress`] (Export-Events), der Sequenz-Render-Cache über [`CountProgress`]
/// (eigener Callback).
trait FrameProgress {
    fn advance(&mut self, units: u64);
}

impl FrameProgress for Progress<'_> {
    fn advance(&mut self, units: u64) {
        Progress::advance(self, units);
    }
}

/// Schlanke Fortschritts-Senke für den Render-Cache: zählt Frames und ruft
/// gedrosselt (≤ 10/s) einen Callback `(done, total)`.
struct CountProgress<'a> {
    done: u64,
    total: u64,
    last_emit: std::time::Instant,
    cb: &'a mut dyn FnMut(u64, u64),
}

impl<'a> CountProgress<'a> {
    fn new(total: u64, cb: &'a mut dyn FnMut(u64, u64)) -> CountProgress<'a> {
        CountProgress {
            done: 0,
            total: total.max(1),
            last_emit: std::time::Instant::now() - std::time::Duration::from_secs(10),
            cb,
        }
    }
}

impl FrameProgress for CountProgress<'_> {
    fn advance(&mut self, units: u64) {
        self.done = (self.done + units).min(self.total);
        let now = std::time::Instant::now();
        if self.done >= self.total || now.duration_since(self.last_emit).as_millis() >= 100 {
            self.last_emit = now;
            (self.cb)(self.done, self.total);
        }
    }
}

struct Progress<'a> {
    tx: &'a Sender<ServiceEvent>,
    job_id: &'a str,
    phase: ExportPhase,
    base_pct: f64,
    span_pct: f64,
    total: u64,
    done: u64,
    /// Frames/Sekunde, exponentiell geglättet.
    rate: f64,
    last_rate_at: std::time::Instant,
    last_rate_done: u64,
    last_emit: std::time::Instant,
    /// Anzeige-Skala (Video: 1 = Frames; Audio: Samples → Sekundenanzeige).
    show_frames: bool,
}

impl<'a> Progress<'a> {
    fn new(tx: &'a Sender<ServiceEvent>, job_id: &'a str) -> Progress<'a> {
        let now = std::time::Instant::now();
        Progress {
            tx,
            job_id,
            phase: ExportPhase::MixAudio,
            base_pct: 0.0,
            span_pct: 0.0,
            total: 1,
            done: 0,
            rate: 0.0,
            last_rate_at: now,
            last_rate_done: 0,
            last_emit: now - std::time::Duration::from_secs(10),
            show_frames: false,
        }
    }

    fn begin_phase(&mut self, phase: ExportPhase, base: f64, span: f64, total: u64, frames: bool) {
        self.phase = phase;
        self.base_pct = base;
        self.span_pct = span;
        self.total = total.max(1);
        self.done = 0;
        self.rate = 0.0;
        self.last_rate_at = std::time::Instant::now();
        self.last_rate_done = 0;
        self.show_frames = frames;
        self.emit(true);
    }

    fn advance(&mut self, units: u64) {
        self.done = (self.done + units).min(self.total);
        self.emit(false);
    }

    fn emit(&mut self, force: bool) {
        let now = std::time::Instant::now();
        if !force && now.duration_since(self.last_emit).as_millis() < 100 {
            return;
        }
        self.last_emit = now;
        // Rate über ein ~0,5-s-Fenster glätten.
        let dt = now.duration_since(self.last_rate_at).as_secs_f64();
        if dt >= 0.5 {
            let inst = (self.done - self.last_rate_done) as f64 / dt;
            self.rate = if self.rate <= 0.0 {
                inst
            } else {
                self.rate * 0.6 + inst * 0.4
            };
            self.last_rate_at = now;
            self.last_rate_done = self.done;
        }
        let frac = self.done as f64 / self.total as f64;
        let eta = if self.rate > 0.0 && self.done > 0 {
            Some(((self.total - self.done) as f64 / self.rate).max(0.0))
        } else {
            None
        };
        let _ = self.tx.send(ServiceEvent::SequenceExportProgress {
            job_id: self.job_id.to_string(),
            pct: (self.base_pct + self.span_pct * frac).clamp(0.0, 100.0),
            phase: self.phase,
            frames_done: if self.show_frames { self.done } else { 0 },
            frames_total: if self.show_frames { self.total } else { 0 },
            render_fps: if self.show_frames { self.rate } else { 0.0 },
            eta_sec: eta,
        });
    }
}

/// Einstieg für den Worker-Thread (von `Services::start_sequence_export`).
pub fn run_export_worker(
    job_id: String,
    plan: RenderPlan,
    settings: ExportSettings,
    tx: Sender<ServiceEvent>,
    cancel: Arc<AtomicBool>,
    children: ChildList,
) {
    let registry = ChildRegistry::new(Arc::clone(&children));
    let part = part_path(&settings.output);
    let wav = std::env::temp_dir().join(format!("editron-mix-{job_id}.wav"));
    // Temp-SRTs fürs Einbetten (eine je Spur; .srt-Endung für den Demuxer).
    let subs: Vec<PathBuf> = if settings.subtitles == SubtitleMode::Embed {
        plan.subtitle_tracks
            .iter()
            .enumerate()
            .map(|(i, _)| std::env::temp_dir().join(format!("editron-sub-{job_id}-{i}.srt")))
            .collect()
    } else {
        Vec::new()
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        export_inner(&job_id, &plan, &settings, &tx, &cancel, registry, &part, &wav, &subs)
    }));
    let outcome = match result {
        Ok(r) => r,
        Err(panic) => {
            let msg = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unbekannte Ursache".to_string());
            Err(ExportError::Failed(format!("Interner Fehler: {msg}")))
        }
    };

    // Aufräumen: Kinder sind tot, tmp-Dateien weg; .part nur bei Erfolg renamed.
    ChildRegistry::new(children).kill_all();
    let _ = std::fs::remove_file(&wav);
    for sub in &subs {
        let _ = std::fs::remove_file(sub);
    }
    let (ok, cancelled, error) = match outcome {
        Ok(()) => (true, false, None),
        Err(ExportError::Cancelled) => {
            let _ = std::fs::remove_file(&part);
            (false, true, None)
        }
        Err(ExportError::Failed(msg)) => {
            let _ = std::fs::remove_file(&part);
            (false, false, Some(msg))
        }
    };
    let _ = tx.send(ServiceEvent::SequenceExportDone {
        job_id,
        ok,
        cancelled,
        error,
        output: settings.output.clone(),
    });
}

fn part_path(output: &str) -> PathBuf {
    PathBuf::from(format!("{output}.part"))
}

/// Pfad einer Sidecar-SRT: `<ziel>.srt` bei einer Spur, sonst
/// `<ziel>.<spurname>.srt` (z. B. `film.U2.srt`).
pub fn sidecar_srt_path(output: &str, track_name: &str, single: bool) -> PathBuf {
    let stem = Path::new(output).with_extension("");
    if single {
        PathBuf::from(format!("{}.srt", stem.display()))
    } else {
        PathBuf::from(format!("{}.{}.srt", stem.display(), track_name))
    }
}

#[allow(clippy::too_many_arguments)]
fn export_inner(
    job_id: &str,
    plan: &RenderPlan,
    settings: &ExportSettings,
    tx: &Sender<ServiceEvent>,
    cancel: &AtomicBool,
    mut children: ChildRegistry,
    part: &Path,
    wav: &Path,
    subs: &[PathBuf],
) -> Result<(), ExportError> {
    let mut progress = Progress::new(tx, job_id);
    let _ = std::fs::remove_file(part);

    // ---- Sonderfall: Bild-Sequenz (nur Video, jeder Frame eine Datei) ----
    if settings.container.image_sequence {
        progress.begin_phase(ExportPhase::RenderVideo, 0.0, 99.0, plan.total_frames, true);
        render_image_sequence(job_id, plan, settings, cancel, &mut children, &mut progress)
            .map_err(fail_or_cancel(cancel))?;
        if cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        progress.begin_phase(ExportPhase::Finalize, 99.0, 1.0, 1, false);
        progress.advance(1);
        progress.emit(true);
        return Ok(());
    }

    let with_audio = settings.audio.is_some();
    let with_video = settings.video.is_some();

    // Einzubettende Untertitel als Temp-SRTs schreiben (Encoder-Inputs).
    for (sub, track) in subs.iter().zip(&plan.subtitle_tracks) {
        std::fs::write(sub, crate::core::subtitle::format_srt(&track.cues)).map_err(|e| {
            ExportError::Failed(format!("Untertitel-Zwischendatei konnte nicht geschrieben werden: {e}"))
        })?;
    }

    // ---- Phase A: Audio-Mixdown in eine temporäre f32-WAV ----
    if with_audio {
        let audio = settings.audio.as_ref().expect("audio settings");
        let span = if with_video { 6.0 } else { 85.0 };
        let total_units = plan.audio_total_units(audio.sample_rate);
        progress.begin_phase(ExportPhase::MixAudio, 0.0, span, total_units.max(1), false);
        mix_audio_to_wav(plan, audio, wav, cancel, &mut children, &mut progress)
            .map_err(fail_or_cancel(cancel))?;
    }

    // ---- Phase B: Video rendern bzw. Audio-only kodieren ----
    if with_video {
        let base = if with_audio { 6.0 } else { 0.0 };
        progress.begin_phase(ExportPhase::RenderVideo, base, 93.0 - base, plan.total_frames, true);
        render_video(
            plan,
            settings,
            with_audio.then_some(wav),
            subs,
            part,
            cancel,
            &mut children,
            &mut progress,
        )
        .map_err(fail_or_cancel(cancel))?;
    } else {
        progress.begin_phase(ExportPhase::EncodeAudio, 85.0, 14.0, 1, false);
        encode_audio_only(settings, wav, part, cancel, &mut children)
            .map_err(fail_or_cancel(cancel))?;
    }

    if cancel.load(Ordering::Relaxed) {
        return Err(ExportError::Cancelled);
    }

    // ---- Sidecar-Untertitel neben die Zieldatei schreiben ----
    if settings.subtitles == SubtitleMode::Sidecar {
        let single = plan.subtitle_tracks.len() == 1;
        for track in &plan.subtitle_tracks {
            let path = sidecar_srt_path(&settings.output, &track.name, single);
            std::fs::write(&path, crate::core::subtitle::format_srt(&track.cues)).map_err(
                |e| {
                    ExportError::Failed(format!(
                        "Untertitel-Datei konnte nicht geschrieben werden ({}): {e}",
                        path.display()
                    ))
                },
            )?;
        }
    }

    // ---- Finalisieren: atomar an den Zielort ----
    progress.begin_phase(ExportPhase::Finalize, 99.0, 1.0, 1, false);
    std::fs::rename(part, &settings.output).map_err(|e| {
        ExportError::Failed(format!(
            "Fertige Datei konnte nicht umbenannt werden ({} → {}): {e}",
            part.display(),
            settings.output
        ))
    })?;
    progress.advance(1);
    progress.emit(true);
    Ok(())
}

/// Bei gesetztem Abbruch-Flag wird jeder Folgefehler (gekillte Pipes) zu
/// `Cancelled` statt zu einem irreführenden Fehlertext.
fn fail_or_cancel(cancel: &AtomicBool) -> impl Fn(String) -> ExportError + '_ {
    move |msg| {
        if cancel.load(Ordering::Relaxed) {
            ExportError::Cancelled
        } else {
            ExportError::Failed(msg)
        }
    }
}

// ----------------------------------------------------------- Audio-Mixdown

/// WAV-Header für IEEE-Float (Format 3) inkl. fact-Chunk; liefert den
/// Daten-Offset.
fn write_wav_header(
    f: &mut std::fs::File,
    sample_rate: u32,
    channels: u16,
    data_bytes: u32,
) -> std::io::Result<u64> {
    let byte_rate = sample_rate * channels as u32 * 4;
    let block_align = channels * 4;
    let sample_frames = data_bytes / block_align as u32;
    let mut h: Vec<u8> = Vec::with_capacity(58);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(50u32 + data_bytes).to_le_bytes()); // Chunks nach "WAVE"
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&18u32.to_le_bytes());
    h.extend_from_slice(&3u16.to_le_bytes()); // WAVE_FORMAT_IEEE_FLOAT
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&sample_rate.to_le_bytes());
    h.extend_from_slice(&byte_rate.to_le_bytes());
    h.extend_from_slice(&block_align.to_le_bytes());
    h.extend_from_slice(&32u16.to_le_bytes()); // Bits pro Sample
    h.extend_from_slice(&0u16.to_le_bytes()); // cbSize
    h.extend_from_slice(b"fact");
    h.extend_from_slice(&4u32.to_le_bytes());
    h.extend_from_slice(&sample_frames.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_bytes.to_le_bytes());
    f.write_all(&h)?;
    Ok(h.len() as u64)
}

/// Alle Audio-Clips nacheinander in die WAV mischen (Read-Modify-Write an
/// der Zielposition) — konstanter Speicherbedarf, beliebig viele Clips.
fn mix_audio_to_wav(
    plan: &RenderPlan,
    audio: &AudioSettings,
    wav: &Path,
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
    progress: &mut Progress,
) -> Result<(), String> {
    let rate = audio.sample_rate;
    let ch = audio.channels.clamp(1, 2) as usize;
    let total_frames = (plan.duration * rate as f64).round().max(1.0) as u64;
    let data_bytes = total_frames * ch as u64 * 4;
    if data_bytes >= u32::MAX as u64 - 1024 {
        return Err("Audio-Mix überschreitet die 4-GB-Grenze des WAV-Zwischenformats.".into());
    }

    let (mut file, data_off) = create_silent_wav(wav, rate, ch, data_bytes)?;

    // Schnellpfad-Clips (Spuren ohne Bus-FX/Automation): direkt in den Master.
    mix_clips_into_wav(
        &mut file, data_off, total_frames, rate, ch, &plan.audio, cancel, children, progress,
    )?;

    // Spuren mit Bus-FX und/oder Automation: getrennt mischen und einsummieren
    // (Bus-FX wirken auf die Spur-Summe — exakt wie der Player-Mixdown).
    for (idx, track) in plan.audio_tracks.iter().enumerate() {
        process_audio_track(
            &mut file, data_off, total_frames, rate, ch, wav, idx, track, cancel, children,
            progress,
        )?;
    }

    file.sync_all().ok();
    Ok(())
}

/// Leere f32-WAV (Stille) anlegen; liefert (Datei, Daten-Offset).
fn create_silent_wav(
    path: &Path,
    rate: u32,
    ch: usize,
    data_bytes: u64,
) -> Result<(std::fs::File, u64), String> {
    let mut file = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| format!("Audio-Zwischendatei konnte nicht angelegt werden: {e}"))?;
    let data_off = write_wav_header(&mut file, rate, ch as u16, data_bytes as u32)
        .map_err(|e| format!("WAV-Header: {e}"))?;
    // Mit Stille auffüllen (f32 0.0 = Null-Bytes — set_len reicht).
    file.set_len(data_off + data_bytes)
        .map_err(|e| format!("Audio-Zwischendatei: {e}"))?;
    Ok((file, data_off))
}

/// Alle übergebenen Clips nacheinander in die WAV mischen (Read-Modify-Write
/// an der Zielposition) — konstanter Speicher, beliebig viele Clips. Wird
/// für den Master (Schnellpfad-Clips) UND für Per-Spur-WAVs genutzt.
#[allow(clippy::too_many_arguments)]
fn mix_clips_into_wav(
    file: &mut std::fs::File,
    data_off: u64,
    total_frames: u64,
    rate: u32,
    ch: usize,
    clips: &[AudioClipPlan],
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
    progress: &mut Progress,
) -> Result<(), String> {
    for clip in clips {
        if cancel.load(Ordering::Relaxed) {
            return Err("abgebrochen".into());
        }
        let offset_frames = (clip.start_in_mix * rate as f64).round().max(0.0) as u64;
        let want_frames =
            ((clip.duration * rate as f64).round() as u64).min(total_frames.saturating_sub(offset_frames));
        if want_frames == 0 {
            continue;
        }

        let mut cmd = Command::new(crate::services::ffmpeg_bin());
        cmd.args(["-v", "error", "-ss", &format!("{:.4}", clip.src_in)])
            // -t schneidet die QUELLE: Medienspanne = Ausgabedauer × speed.
            .args(["-t", &format!("{:.4}", clip.duration * clip.speed)])
            .args(["-i", &clip.path])
            .args(["-vn", "-sn"]);
        // Pitch-korrigiertes Tempo — identische Kette wie die Wiedergabe.
        if let Some(chain) = atempo_chain(clip.speed) {
            cmd.args(["-filter:a", &chain]);
        }
        cmd.args(["-f", "f32le", "-ac", &ch.to_string(), "-ar", &rate.to_string()])
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let (id, _, stdout, _) = children.spawn(&mut cmd)?;
        let mut stdout = stdout.ok_or("ffmpeg-stdout nicht verfügbar")?;

        // Pro Seite wirksamer Faktor; Mono mittelt beide Seiten.
        let gains: [f32; 2] = [clip.gain_l, clip.gain_r];
        let mono_gain = (clip.gain_l + clip.gain_r) * 0.5;
        // Lautstärke-Kurve (dB-Keyframes) und Übergangs-Crossfades:
        // blockweise ausgewertet — 256 Frames ≈ 5 ms bei 48 kHz, glatt
        // genug für Fades.
        let has_envelope =
            clip.volume.is_animated() || clip.volume.value != 0.0 || !clip.fades.is_empty();
        const ENV_BLOCK: usize = 256;

        // Audio-Effekt-Kette (identischer DSP wie im Player-Mixdown);
        // animierte Parameter werden je ENV_BLOCK nachgestimmt.
        let fx_refs: Vec<&EffectInstance> = clip.effects.iter().collect();
        let mut fx_chain = AudioFxChain::build(&fx_refs, rate, ch, clip.src_in);
        let fx_animated = clip.effects.iter().any(|e| e.any_animated());

        const CHUNK_FRAMES: usize = 32768;
        let mut decoded = vec![0u8; CHUNK_FRAMES * ch * 4];
        let mut existing = vec![0u8; CHUNK_FRAMES * ch * 4];
        let mut fresh = vec![0f32; CHUNK_FRAMES * ch];
        let mut frames_done: u64 = 0;
        'clip: while frames_done < want_frames {
            if cancel.load(Ordering::Relaxed) {
                children.kill(id);
                return Err("abgebrochen".into());
            }
            let want_now = ((want_frames - frames_done) as usize).min(CHUNK_FRAMES);
            let want_bytes = want_now * ch * 4;
            // Block vollständig lesen (EOF beendet den Clip vorzeitig — Rest bleibt Stille).
            let mut filled = 0;
            while filled < want_bytes {
                match stdout.read(&mut decoded[filled..want_bytes]) {
                    Ok(0) => break,
                    Ok(n) => filled += n,
                    Err(e) => {
                        children.kill(id);
                        return Err(format!("Audio-Decoder ({}): {e}", clip.path));
                    }
                }
            }
            let got_frames = filled / (ch * 4);
            if got_frames == 0 {
                break 'clip;
            }
            for (i, s) in fresh[..got_frames * ch].iter_mut().enumerate() {
                let off = i * 4;
                *s = f32::from_le_bytes([
                    decoded[off],
                    decoded[off + 1],
                    decoded[off + 2],
                    decoded[off + 3],
                ]);
            }
            let byte_pos = data_off + (offset_frames + frames_done) * ch as u64 * 4;
            let block = &mut existing[..got_frames * ch * 4];
            file.seek(SeekFrom::Start(byte_pos)).map_err(|e| e.to_string())?;
            file.read_exact(block).map_err(|e| format!("Mix-Lesen: {e}"))?;
            let mut fi = 0usize;
            while fi < got_frames {
                let n = ENV_BLOCK.min(got_frames - fi);
                let media_t =
                    clip.src_in + (frames_done + fi as u64) as f64 / rate as f64 * clip.speed;
                if let Some(chain) = fx_chain.as_mut() {
                    if fx_animated {
                        chain.retune(&fx_refs, media_t);
                    }
                    chain.process(&mut fresh[fi * ch..(fi + n) * ch]);
                }
                let env = if has_envelope {
                    // Crossfade-Hüllkurve in Mix-Zeit (identische Kurven
                    // wie der Player-Mixdown).
                    let mix_t =
                        clip.start_in_mix + (frames_done + fi as u64) as f64 / rate as f64;
                    let fade: f64 = clip
                        .fades
                        .iter()
                        .map(|f| f.gain_at(mix_t))
                        .product();
                    db_to_linear(clip.volume.eval(media_t)) * fade as f32
                } else {
                    1.0
                };
                for i in fi * ch..(fi + n) * ch {
                    let gain = env * if ch == 2 { gains[i % 2] } else { mono_gain };
                    let off = i * 4;
                    let old = f32::from_le_bytes([
                        block[off],
                        block[off + 1],
                        block[off + 2],
                        block[off + 3],
                    ]);
                    let sum = old + fresh[i] * gain;
                    block[off..off + 4].copy_from_slice(&sum.to_le_bytes());
                }
                fi += n;
            }
            file.seek(SeekFrom::Start(byte_pos)).map_err(|e| e.to_string())?;
            file.write_all(block).map_err(|e| format!("Mix-Schreiben: {e}"))?;
            frames_done += got_frames as u64;
            progress.advance(got_frames as u64);
            if filled < want_bytes {
                break 'clip; // EOF des Decoders
            }
        }
        // Nicht gelieferte Samples als erledigt verbuchen (Fortschritt stimmt).
        progress.advance(want_frames.saturating_sub(frames_done));
        children.kill(id);
    }
    file.sync_all().ok();
    Ok(())
}

/// Eine Spur MIT Bus-FX/Automation verarbeiten: Clips in eine temporäre WAV
/// summieren, diese blockweise durch die Bus-Effektkette schicken, Spur-
/// Gain/Pan (inkl. Automation, Sequenzzeit) und Master anwenden und additiv
/// in die Master-WAV mischen. Identische DSP-Kette (`AudioFxChain`) und
/// Gain-Mathematik wie der Player → Wiedergabe und Export klingen gleich.
#[allow(clippy::too_many_arguments)]
fn process_audio_track(
    master: &mut std::fs::File,
    data_off: u64,
    total_frames: u64,
    rate: u32,
    ch: usize,
    base_wav: &Path,
    idx: usize,
    track: &AudioTrackPlan,
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
    progress: &mut Progress,
) -> Result<(), String> {
    let data_bytes = total_frames * ch as u64 * 4;
    let tmp = base_wav.with_extension(format!("track{idx}.wav"));
    let mut run = || -> Result<(), String> {
        // 1. Clips der Spur in die Temp-WAV (nur Clip-Gain).
        let (mut tfile, toff) = create_silent_wav(&tmp, rate, ch, data_bytes)?;
        mix_clips_into_wav(
            &mut tfile, toff, total_frames, rate, ch, &track.clips, cancel, children, progress,
        )?;

        // 2. Bus-FX + Spur-Gain/Pan + Master, blockweise, in den Master.
        let master_lin = db_to_linear(track.master_db);
        let fx_refs: Vec<&EffectInstance> = track.effects.iter().collect();
        let mut fx_chain = AudioFxChain::build(&fx_refs, rate, ch, track.seq_start);
        let fx_animated = track.effects.iter().any(|e| e.any_animated());
        const ENV_BLOCK: usize = 256;
        const CHUNK_FRAMES: usize = 32768;
        let mut tbuf = vec![0u8; CHUNK_FRAMES * ch * 4];
        let mut mbuf = vec![0u8; CHUNK_FRAMES * ch * 4];
        let mut fresh = vec![0f32; CHUNK_FRAMES * ch];
        let mut frames_done: u64 = 0;
        while frames_done < total_frames {
            if cancel.load(Ordering::Relaxed) {
                return Err("abgebrochen".into());
            }
            let now = ((total_frames - frames_done) as usize).min(CHUNK_FRAMES);
            let bytes = now * ch * 4;
            let tpos = toff + frames_done * ch as u64 * 4;
            tfile.seek(SeekFrom::Start(tpos)).map_err(|e| e.to_string())?;
            tfile
                .read_exact(&mut tbuf[..bytes])
                .map_err(|e| format!("Spur-Lesen: {e}"))?;
            for i in 0..now * ch {
                let off = i * 4;
                fresh[i] =
                    f32::from_le_bytes([tbuf[off], tbuf[off + 1], tbuf[off + 2], tbuf[off + 3]]);
            }
            let mpos = data_off + frames_done * ch as u64 * 4;
            master.seek(SeekFrom::Start(mpos)).map_err(|e| e.to_string())?;
            master
                .read_exact(&mut mbuf[..bytes])
                .map_err(|e| format!("Mix-Lesen: {e}"))?;
            let mut fi = 0usize;
            while fi < now {
                let n = ENV_BLOCK.min(now - fi);
                let mix_t = (frames_done + fi as u64) as f64 / rate as f64;
                if let Some(chain) = fx_chain.as_mut() {
                    if fx_animated {
                        chain.retune(&fx_refs, track.seq_start + mix_t);
                    }
                    chain.process(&mut fresh[fi * ch..(fi + n) * ch]);
                }
                // Spur-Gain/Pan inkl. Automation (Sequenzzeit) × Master.
                let g = db_to_linear(track.gain_db_at(mix_t));
                let (pl, pr) = pan_gains(track.pan_at(mix_t));
                let (gl, gr) = (g * pl * master_lin, g * pr * master_lin);
                let mono = (gl + gr) * 0.5;
                for i in fi * ch..(fi + n) * ch {
                    let gain = if ch == 2 {
                        if i % 2 == 0 {
                            gl
                        } else {
                            gr
                        }
                    } else {
                        mono
                    };
                    let off = i * 4;
                    let old =
                        f32::from_le_bytes([mbuf[off], mbuf[off + 1], mbuf[off + 2], mbuf[off + 3]]);
                    let sum = old + fresh[i] * gain;
                    mbuf[off..off + 4].copy_from_slice(&sum.to_le_bytes());
                }
                fi += n;
            }
            master.seek(SeekFrom::Start(mpos)).map_err(|e| e.to_string())?;
            master
                .write_all(&mbuf[..bytes])
                .map_err(|e| format!("Mix-Schreiben: {e}"))?;
            frames_done += now as u64;
            progress.advance(now as u64);
        }
        Ok(())
    };
    let result = run();
    let _ = std::fs::remove_file(&tmp);
    result
}

// ----------------------------------------------------------- Video-Render

/// Ausgabe-Farbraum des Exports (ehrliche Tags + korrekte RGB→YUV-Matrix).
/// Wird aus dem dominanten Quellmaterial erkannt; SDR-Default ist BT.709.
/// HDR-Quellen (PQ/HLG) und BT.2020 werden durchgereicht statt nach 709
/// fehlgetaggt — „10-Bit-HDR-Material wird nicht mehr zerstört". Vollständiges
/// HDR-Grading bleibt ausgeklammert (die Korrektur rechnet weiter in 709-Gamma).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OutputColor {
    #[default]
    Bt709,
    /// BT.2020, SDR-Transfer (Wide-Gamut-Quelle ohne HDR-Kurve).
    Bt2020,
    /// BT.2020 + PQ (SMPTE ST 2084) — HDR10.
    Bt2020Pq,
    /// BT.2020 + HLG (ARIB STD-B67).
    Bt2020Hlg,
}

impl OutputColor {
    /// Aus den ffprobe-Farbtags eines Streams ableiten (siehe
    /// `VideoStreamInfo`). Unbekannt/leer ⇒ BT.709.
    pub fn from_stream(s: &crate::core::types::VideoStreamInfo) -> OutputColor {
        let trc = s.color_transfer.as_deref().unwrap_or("").to_ascii_lowercase();
        let prim = s.color_primaries.as_deref().unwrap_or("").to_ascii_lowercase();
        let space = s.color_space.as_deref().unwrap_or("").to_ascii_lowercase();
        if trc.contains("2084") || trc.contains("pq") {
            return OutputColor::Bt2020Pq;
        }
        if trc.contains("b67") || trc.contains("hlg") {
            return OutputColor::Bt2020Hlg;
        }
        if prim.contains("2020") || space.contains("2020") {
            return OutputColor::Bt2020;
        }
        OutputColor::Bt709
    }

    /// ffmpeg-Tags (color_primaries, color_trc, colorspace).
    pub fn tags(self) -> (&'static str, &'static str, &'static str) {
        match self {
            OutputColor::Bt709 => ("bt709", "bt709", "bt709"),
            OutputColor::Bt2020 => ("bt2020", "bt2020-10", "bt2020nc"),
            OutputColor::Bt2020Pq => ("bt2020", "smpte2084", "bt2020nc"),
            OutputColor::Bt2020Hlg => ("bt2020", "arib-std-b67", "bt2020nc"),
        }
    }

    /// Matrix für die RGB→YUV-Wandlung im `scale`-Filter.
    pub fn scale_matrix(self) -> &'static str {
        match self {
            OutputColor::Bt709 => "bt709",
            _ => "bt2020nc",
        }
    }

    /// HDR-Transfer (PQ/HLG)? Solche Quellen werden für die SDR-Vorschau
    /// tone-gemappt (`core/player.rs`).
    pub fn is_hdr(self) -> bool {
        matches!(self, OutputColor::Bt2020Pq | OutputColor::Bt2020Hlg)
    }
}

/// Encoder-Argumente für den Video-Codec (pure Funktion, testbar). Wählt das
/// passende Qualitäts-Flag je Encoder-Backend (CRF/CQ/global_quality/QP/
/// Bitrate) und behandelt VAAPI (Render-Device + `hwupload`) gesondert.
/// `color` = erkannter Ausgabe-Farbraum (ehrliche Tags + passende Matrix).
pub fn video_codec_args(
    v: &VideoSettings,
    container: &ContainerDef,
    color: OutputColor,
) -> Vec<String> {
    let vaapi = v.encoder.vaapi;
    let mut args: Vec<String> = Vec::new();
    // VAAPI braucht das Render-Device, bevor der Encoder initialisiert wird.
    if vaapi {
        args.extend(["-vaapi_device".into(), vaapi_device()]);
    }
    args.extend(["-c:v".into(), v.encoder.id.into()]);
    let mut pix_fmt = v.codec.pix_fmt;
    match v.codec.quality {
        QualityKind::Profiles(profiles) => {
            let (arg, _, fmt) = profiles[v.profile.min(profiles.len() - 1)];
            args.extend(["-profile:v".into(), arg.into()]);
            pix_fmt = fmt;
            if v.codec.id == "prores" {
                args.extend(["-vendor".into(), "apl0".into()]);
            }
        }
        QualityKind::CrfOrBitrate { .. } => {
            args.extend(quality_args(v));
        }
    }
    // 10-Bit-Schalter für CRF/Bitrate-Codecs (Software-Pfad): höheres
    // Pixelformat + codec-spezifisches 10-Bit-Profil. VAAPI bleibt 8-Bit
    // (nv12), da Hardware-10-Bit encoderspezifisch ist.
    if v.tenbit && !vaapi {
        if let Some(fmt) = codec_tenbit_pix_fmt(v.codec.id) {
            pix_fmt = fmt;
            match v.codec.id {
                "hevc" => args.extend(["-profile:v".into(), "main10".into()]),
                "h264" => args.extend(["-profile:v".into(), "high10".into()]),
                "vp9" => args.extend(["-profile:v".into(), "2".into()]),
                _ => {} // AV1: yuv420p10le genügt, kein Profil-Flag nötig.
            }
        }
    }
    // Encoder-Tempo: nur Software-Encoder verstehen die x264/x265/SVT-Presets.
    if !v.encoder.is_hardware() && !v.codec.speed_presets.is_empty() {
        let preset = v.codec.speed_presets[v.speed.min(v.codec.speed_presets.len() - 1)];
        args.extend(["-preset".into(), preset.into()]);
    }
    if v.codec.id == "vp9" {
        args.extend(["-row-mt".into(), "1".into(), "-deadline".into(), "good".into()]);
    }
    if v.codec.id == "hevc" && matches!(container.id, "mp4" | "mov") {
        // Apple-Player erwarten hvc1 statt hev1.
        args.extend(["-tag:v".into(), "hvc1".into()]);
    }
    // RGBA → Ziel-Farbraum wandeln + ehrlich taggen (BT.709 oder, bei
    // erkanntem Wide-Gamut/HDR-Material, BT.2020 (+ PQ/HLG) durchgereicht).
    // VAAPI lädt zusätzlich in eine GPU-Surface.
    let (prim, trc, space) = color.tags();
    let mat = color.scale_matrix();
    if vaapi {
        args.extend([
            "-vf".into(),
            format!("scale=out_color_matrix={mat}:out_range=tv,format=nv12,hwupload"),
        ]);
    } else {
        args.extend(["-pix_fmt".into(), pix_fmt.into()]);
        args.extend(["-vf".into(), format!("scale=out_color_matrix={mat}:out_range=tv")]);
    }
    args.extend([
        "-color_primaries".into(),
        prim.into(),
        "-color_trc".into(),
        trc.into(),
        "-colorspace".into(),
        space.into(),
    ]);
    args
}

/// Qualitäts-Flags für CRF-/Bitrate-Codecs je nach Encoder-Backend.
fn quality_args(v: &VideoSettings) -> Vec<String> {
    match v.quality {
        VideoQuality::Bitrate(kbps) => vec!["-b:v".into(), format!("{kbps}k")],
        VideoQuality::Crf(val) => match v.encoder.quality {
            EncoderQuality::Crf => {
                let mut a = vec!["-crf".into(), val.to_string()];
                if v.codec.id == "vp9" {
                    // libvpx: CRF wirkt nur mit -b:v 0.
                    a.extend(["-b:v".into(), "0".into()]);
                }
                a
            }
            EncoderQuality::Cq(..) => {
                // NVENC: konstante Qualität über VBR mit cq + b:v 0.
                vec![
                    "-rc".into(), "vbr".into(),
                    "-cq".into(), val.to_string(),
                    "-b:v".into(), "0".into(),
                ]
            }
            EncoderQuality::GlobalQuality(..) => {
                // Intel QSV: ICQ-ähnlich über global_quality.
                vec!["-global_quality".into(), val.to_string()]
            }
            EncoderQuality::Qp(..) => {
                // VAAPI: konstante Quantisierung.
                vec!["-rc_mode".into(), "CQP".into(), "-qp".into(), val.to_string()]
            }
            EncoderQuality::BitrateOnly => {
                // VideoToolbox kennt kein CRF — sinnvoller Bitrate-Fallback.
                vec!["-b:v".into(), "12000k".into()]
            }
        },
    }
}

/// Audio-Encoder-Argumente (pure Funktion, testbar).
pub fn audio_codec_args(a: &AudioSettings) -> Vec<String> {
    let mut args: Vec<String> = vec!["-c:a".into(), a.codec.encoder.into()];
    if !a.codec.bitrates.is_empty() {
        args.extend(["-b:a".into(), format!("{}k", a.bitrate_kbps)]);
    }
    args
}

// ------------------------------------------------------- Bild-Sequenz-Export

/// Encoder-Argumente für die Bild-Sequenz-Codecs (PNG/JPEG/TIFF). PNG/TIFF
/// sind verlustfrei (RGB), JPEG nutzt das CRF-Feld als `-q:v`-Qualität.
pub fn image_codec_args(v: &VideoSettings) -> Vec<String> {
    match v.codec.id {
        "mjpeg" => {
            let q = match v.quality {
                VideoQuality::Crf(q) => q.clamp(2, 31),
                VideoQuality::Bitrate(_) => 3,
            };
            vec![
                "-c:v".into(), "mjpeg".into(),
                "-q:v".into(), q.to_string(),
                "-pix_fmt".into(), "yuvj420p".into(),
            ]
        }
        "tiff" => vec!["-c:v".into(), "tiff".into(), "-pix_fmt".into(), "rgb24".into()],
        // PNG (Standard): verlustfrei mit Alpha.
        _ => vec!["-c:v".into(), "png".into(), "-pix_fmt".into(), "rgba".into()],
    }
}

/// ffmpeg-Encoder-Argumente für einen Einzel-Frame-Export nach Endung
/// (Programmmonitor-Kamera). Muxer ist immer `image2`.
pub fn frame_export_args(ext: &str) -> Vec<String> {
    match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => vec![
            "-c:v".into(), "mjpeg".into(),
            "-q:v".into(), "2".into(),
            "-pix_fmt".into(), "yuvj420p".into(),
            "-frames:v".into(), "1".into(),
        ],
        "tif" | "tiff" => vec![
            "-c:v".into(), "tiff".into(),
            "-pix_fmt".into(), "rgb24".into(),
            "-frames:v".into(), "1".into(),
        ],
        // PNG (Standard): verlustfrei mit Alpha.
        _ => vec![
            "-c:v".into(), "png".into(),
            "-pix_fmt".into(), "rgba".into(),
            "-frames:v".into(), "1".into(),
        ],
    }
}

/// Zielmuster einer Bild-Sequenz: `<verzeichnis>/<stamm>_%06d.<ext>` —
/// abgeleitet aus dem gewählten Basis-Pfad (`/dir/name.png`).
pub fn image_sequence_pattern(output: &str) -> String {
    let path = Path::new(output);
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "png".into());
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "frame".into());
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!("{stem}_%06d.{ext}")).to_string_lossy().into_owned()
}

/// Rendert eine Bild-Sequenz (nur Video-Phase). Atomar: erst in ein temporäres
/// Unterverzeichnis schreiben, bei Erfolg die fertigen Frames an den Zielort
/// verschieben — ein Abbruch/Fehler hinterlässt keine halbe Sequenz.
fn render_image_sequence(
    job_id: &str,
    plan: &RenderPlan,
    settings: &ExportSettings,
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
    progress: &mut Progress,
) -> Result<(), String> {
    let video = settings.video.as_ref().ok_or("Bild-Sequenz braucht Video-Einstellungen")?;
    let fps = fps_arg(video.fps);
    let out_path = Path::new(&settings.output);
    let dir = out_path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .ok_or("Zielordner der Bild-Sequenz ist ungültig")?;
    let ext = out_path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| settings.container.ext.into());
    let stem = out_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "frame".into());

    // Temporäres Unterverzeichnis (gleiche Partition ⇒ rename ist atomar/billig).
    let tmp_dir = dir.join(format!(".editron-seq-{job_id}"));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("Temporäres Sequenz-Verzeichnis fehlgeschlagen: {e}"))?;
    let tmp_pattern = tmp_dir.join(format!("f_%06d.{ext}"));

    let mut cmd = Command::new(crate::services::ffmpeg_bin());
    cmd.args(["-y", "-v", "error"])
        .args(["-f", "rawvideo", "-pixel_format", pipe_pix_fmt(video)])
        .args(["-video_size", &format!("{}x{}", video.width, video.height)])
        .args(["-framerate", &fps])
        .args(["-i", "pipe:0"])
        .arg("-an")
        .args(image_codec_args(video))
        .args(["-start_number", &settings.image_start.to_string()])
        .args(["-f", "image2"])
        .arg(&tmp_pattern)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let (enc_id, stdin, _, stderr) = children.spawn(&mut cmd)?;
    let mut enc_in = stdin.ok_or("Encoder-stdin nicht verfügbar")?;
    let mut stderr = stderr.ok_or("Encoder-stderr nicht verfügbar")?;
    let stderr_task = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let write_err = render_segments(plan, video, &fps, &mut enc_in, cancel, children, progress).err();
    drop(enc_in);
    let status = children.wait(enc_id);
    let stderr_buf = stderr_task.join().unwrap_or_default();

    let finish = (|| -> Result<(), String> {
        if cancel.load(Ordering::Relaxed) {
            return Err("abgebrochen".into());
        }
        let ok = status.map(|s| s.success()).unwrap_or(false);
        if !ok || write_err.is_some() {
            let tail = stderr_tail(&stderr_buf);
            let detail = if tail.is_empty() {
                write_err.clone().unwrap_or_else(|| "Encoder ohne Fehlermeldung beendet".into())
            } else {
                tail
            };
            return Err(format!("Bild-Sequenz-Encoder fehlgeschlagen: {detail}"));
        }
        // Fertige Frames an den Zielort verschieben: f_000123.ext → stem_000123.ext.
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&tmp_dir)
            .map_err(|e| format!("Sequenz-Verzeichnis konnte nicht gelesen werden: {e}"))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        entries.sort();
        for src in &entries {
            let num = src
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.strip_prefix("f_"))
                .unwrap_or("000000");
            let dst = dir.join(format!("{stem}_{num}.{ext}"));
            std::fs::rename(src, &dst).map_err(|e| {
                format!("Sequenz-Bild konnte nicht finalisiert werden ({}): {e}", dst.display())
            })?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&tmp_dir);
    finish
}

#[allow(clippy::too_many_arguments)]
fn render_video(
    plan: &RenderPlan,
    settings: &ExportSettings,
    wav: Option<&Path>,
    subs: &[PathBuf],
    part: &Path,
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
    progress: &mut Progress,
) -> Result<(), String> {
    let video = settings.video.as_ref().expect("video settings");
    let fps = fps_arg(video.fps);

    // ---- Encoder-Prozess ----
    let mut cmd = Command::new(crate::services::ffmpeg_bin());
    cmd.args(["-y", "-v", "error"])
        .args(["-f", "rawvideo", "-pixel_format", pipe_pix_fmt(video)])
        .args(["-video_size", &format!("{}x{}", video.width, video.height)])
        .args(["-framerate", &fps])
        .args(["-i", "pipe:0"]);
    if let Some(wav) = wav {
        cmd.args(["-i", &wav.to_string_lossy()]);
    }
    for sub in subs {
        cmd.args(["-i", &sub.to_string_lossy()]);
    }
    cmd.args(["-map", "0:v:0"]);
    if wav.is_some() {
        cmd.args(["-map", "1:a:0"]);
    }
    // Untertitel-Streams hinter Video/Audio mappen (Input-Reihenfolge).
    let sub_base = 1 + usize::from(wav.is_some());
    for i in 0..subs.len() {
        cmd.args(["-map", &format!("{}:s:0", sub_base + i)]);
    }
    cmd.args(video_codec_args(video, settings.container, plan.color));
    if let (Some(_), Some(a)) = (wav, settings.audio.as_ref()) {
        cmd.args(audio_codec_args(a));
    }
    if !subs.is_empty() {
        if let Some(codec) = settings.container.subtitle_codec {
            cmd.args(["-c:s", codec]);
        }
        for (i, track) in plan.subtitle_tracks.iter().enumerate().take(subs.len()) {
            cmd.arg(format!("-metadata:s:s:{i}"))
                .arg(format!("title={}", track.name));
        }
    }
    if settings.container.faststart {
        cmd.args(["-movflags", "+faststart"]);
    }
    // Muxer explizit — die .part-Zwischendatei hat keine Format-Endung.
    cmd.args(["-f", settings.container.muxer]);
    cmd.arg(part)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let (enc_id, stdin, _, stderr) = children.spawn(&mut cmd)?;
    let mut enc_in = stdin.ok_or("Encoder-stdin nicht verfügbar")?;
    let mut stderr = stderr.ok_or("Encoder-stderr nicht verfügbar")?;
    // stderr nebenläufig leeren, sonst blockiert ffmpeg an der vollen Pipe.
    let stderr_task = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    // ---- Segmente sequenziell in die Encoder-Pipe pumpen ----
    let write_err = render_segments(plan, video, &fps, &mut enc_in, cancel, children, progress).err();

    // ---- Encoder abschließen ----
    drop(enc_in); // EOF → Encoder finalisiert den Container
    let status = children.wait(enc_id);
    let stderr_buf = stderr_task.join().unwrap_or_default();
    if cancel.load(Ordering::Relaxed) {
        return Err("abgebrochen".into());
    }
    let ok = status.map(|s| s.success()).unwrap_or(false);
    if !ok || write_err.is_some() {
        let tail = stderr_tail(&stderr_buf);
        let detail = if tail.is_empty() {
            write_err.unwrap_or_else(|| "Encoder ohne Fehlermeldung beendet".into())
        } else {
            tail
        };
        return Err(format!("Video-Encoder fehlgeschlagen: {detail}"));
    }
    Ok(())
}

/// Renderplan für einen Sequenz-Frame-Bereich `[start_frame, end_frame)` —
/// VIDEO-ONLY (kein Audio/Untertitel-Mux). Auf dem Main-Thread bauen (greift
/// auf Timeline/Medien zu) und den OWNED Plan an [`render_cache_plan`]
/// übergeben, das ihn im Hintergrund-Thread rendert (entkoppelt wie der
/// Voll-Export von [`build_render_plan`]).
pub fn build_cache_plan(
    timeline: &TimelineStore,
    media: &MediaStore,
    width: u32,
    height: u32,
    fps: f64,
    start_frame: u64,
    end_frame: u64,
) -> RenderPlan {
    let total_frames = end_frame.saturating_sub(start_frame).max(1);
    let start_sec = start_frame as f64 / fps.max(1.0);
    let solo_any = timeline.tracks.iter().any(|t| t.solo);
    let segments =
        plan_video_segments(timeline, media, &NoNests, start_sec, total_frames, fps, solo_any, false);
    let duration = total_frames as f64 / fps.max(1.0);
    RenderPlan {
        duration,
        width,
        height,
        fps,
        total_frames,
        segments,
        audio: Vec::new(),
        audio_tracks: Vec::new(),
        subtitle_tracks: Vec::new(),
        nests: HashMap::new(),
        nest_media: HashMap::new(),
        color: detect_output_color(timeline, media, start_sec, start_sec + duration),
    }
}

/// Einen Cache-Renderplan über den Compositing-Kern ([`render_segments`]) in
/// eine Intra-Frame-Cache-Datei rendern — ohne Audio. Reiner CPU-Pfad, im
/// Hintergrund-Thread sicher (kein GL-Kontext). `encode_args`/`muxer` bestimmen
/// den Cache-Codec (ProRes Proxy o. Ä.). Schreibt erst in `<out>.part` und
/// benennt bei Erfolg atomar um. `on_progress` wird gedrosselt mit
/// `(done, total)` aufgerufen.
#[allow(clippy::too_many_arguments)]
pub fn render_cache_plan(
    plan: &RenderPlan,
    encode_args: &[String],
    muxer: &str,
    out: &Path,
    cancel: &AtomicBool,
    children: ChildList,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<(), String> {
    let mut children = ChildRegistry::new(children);
    let children = &mut children;
    let (width, height, fps) = (plan.width, plan.height, plan.fps);
    let total_frames = plan.total_frames;
    // Platzhalter-VideoSettings: `render_segments`/`render_segment_composited`
    // lesen daraus nur width/height/fps (das Compositing-Ziel) — der Codec
    // wird vom Cache-Encoder unten gesetzt, nicht hierüber.
    let video = VideoSettings {
        codec: video_codec("h264"),
        encoder: &video_codec("h264").encoders[0],
        width,
        height,
        fps,
        quality: VideoQuality::Crf(0),
        speed: 0,
        profile: 0,
        tenbit: false,
    };

    let fps_s = fps_arg(fps);
    let part = out.with_extension("part");
    let mut cmd = Command::new(crate::services::ffmpeg_bin());
    cmd.args(["-y", "-v", "error"])
        .args(["-f", "rawvideo", "-pixel_format", pipe_pix_fmt(&video)])
        .args(["-video_size", &format!("{width}x{height}")])
        .args(["-framerate", &fps_s])
        .args(["-i", "pipe:0"])
        .arg("-an");
    for a in encode_args {
        cmd.arg(a);
    }
    cmd.args(["-f", muxer])
        .arg(&part)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let (enc_id, stdin, _, stderr) = children.spawn(&mut cmd)?;
    let mut enc_in = stdin.ok_or("Cache-Encoder-stdin nicht verfügbar")?;
    let mut stderr = stderr.ok_or("Cache-Encoder-stderr nicht verfügbar")?;
    let stderr_task = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let write_err = {
        let mut progress = CountProgress::new(total_frames, on_progress);
        render_segments(plan, &video, &fps_s, &mut enc_in, cancel, children, &mut progress).err()
    };
    drop(enc_in); // EOF → Encoder finalisiert die Cache-Datei

    let status = children.wait(enc_id);
    let stderr_buf = stderr_task.join().unwrap_or_default();
    if cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(&part);
        return Err("abgebrochen".into());
    }
    let ok = status.map(|s| s.success()).unwrap_or(false);
    if !ok || write_err.is_some() {
        let tail = stderr_tail(&stderr_buf);
        let _ = std::fs::remove_file(&part);
        let detail = if tail.is_empty() {
            write_err.unwrap_or_else(|| "Cache-Encoder ohne Fehlermeldung beendet".into())
        } else {
            tail
        };
        return Err(format!("Render-Cache fehlgeschlagen: {detail}"));
    }
    std::fs::rename(&part, out)
        .map_err(|e| format!("Cache-Datei konnte nicht finalisiert werden: {e}"))?;
    Ok(())
}

/// Pumpt alle Segmente eines Renderplans als rohe RGBA-Frames in eine
/// Encoder-Pipe (`enc_in`). Das ist der gemeinsame Compositing-Kern von
/// Voll-Export UND Sequenz-Render-Cache — drei Pfade je Segment: Lücke
/// (Schwarz), Schnellpfad (genau ein Layer ohne Transformation, direkt von
/// ffmpeg skaliert/pad't) und voller CPU-Compositing-Pfad. Liefert `Err` nur
/// bei echtem Schreib-/Compositing-Fehler; ein Abbruch (`cancel`) endet
/// stillschweigend mit `Ok(())` (der Aufrufer prüft `cancel` selbst).
fn render_segments(
    plan: &RenderPlan,
    video: &VideoSettings,
    fps: &str,
    enc_in: &mut std::process::ChildStdin,
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
    progress: &mut dyn FrameProgress,
) -> Result<(), String> {
    // Pipe-Format: 8 Bit (rgba) oder 16 Bit (rgba64le) je nach Ziel-Bittiefe.
    // `frame_size` ist die Bytegröße EINES Frames in der Encoder-Pipe.
    let bpp = pipe_bytes_per_px(video);
    let pipe_fmt = pipe_pix_fmt(video);
    let frame_size = video.width as usize * video.height as usize * bpp;
    let black: Vec<u8> = {
        let mut px = vec![0u8; frame_size];
        for p in px.chunks_exact_mut(bpp) {
            // Alpha opak: rgba8 ⇒ Byte 3 = 255; rgba64le ⇒ u16-Alpha = 65535.
            if bpp == 8 {
                p[6] = 255;
                p[7] = 255;
            } else {
                p[3] = 255;
            }
        }
        px
    };

    // Verschachtelte Sequenzen einmal in renderbare Timelines überführen.
    let nest_ctx = NestRenderCtx::from_plan(plan);

    let mut write_err: Option<String> = None;
    // Laufender Frame-Cursor: Exportzeit der Segmente (Übergangs-Fenster).
    let mut frame_cursor: u64 = 0;
    'segments: for segment in &plan.segments {
        let seg_start_frame = frame_cursor;
        frame_cursor += segment.frames;
        // Lücke → Schwarzbild.
        if segment.layers.is_empty() {
            for _ in 0..segment.frames {
                if cancel.load(Ordering::Relaxed) {
                    break 'segments;
                }
                if let Err(e) = enc_in.write_all(&black) {
                    write_err = Some(e.to_string());
                    break 'segments;
                }
                progress.advance(1);
            }
            continue;
        }

        // Schnellpfad: ein Layer ohne Transformation — ffmpeg skaliert/pad't
        // direkt in die Encoder-Pipe (kein CPU-Compositing nötig).
        if segment.layers.len() == 1 && segment.layers[0].is_identity() {
            let layer = &segment.layers[0];
            let freeze = !layer.image && layer.media_step == 0.0;
            let mut cmd = Command::new(crate::services::ffmpeg_bin());
            cmd.args(["-v", "error"]);
            if layer.image {
                cmd.args(["-loop", "1", "-framerate", fps]);
            } else {
                cmd.args(["-ss", &format!("{:.4}", layer.src_in)]);
            }
            cmd.args(["-i", &layer.path]).args(["-an", "-sn"]);
            // Konstante Geschwindigkeit über dieselbe setpts/fps-Kette wie
            // Vorschau und Compositing-Pfad (identische Frame-Auswahl);
            // Standbild: einen Frame dekodieren, Halte-Logik füllt den Rest.
            let setpts = if layer.image || freeze {
                String::new()
            } else {
                speed_setpts_filter(layer.media_step)
            };
            let filter = format!(
                "{setpts}fps={fps},scale={w}:{h}:force_original_aspect_ratio=decrease:flags=bicubic,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black",
                w = video.width,
                h = video.height
            );
            let dec_frames = if freeze { 1 } else { segment.frames };
            cmd.args(["-vf", &filter])
                .args(["-frames:v", &dec_frames.to_string()])
                .args(["-f", "rawvideo", "-pix_fmt", pipe_fmt])
                .arg("pipe:1")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let (dec_id, _, stdout, _) = children.spawn(&mut cmd)?;
            let mut dec_out = stdout.ok_or("Decoder-stdout nicht verfügbar")?;

            let mut frame = vec![0u8; frame_size];
            let mut last_frame: Option<Vec<u8>> = None;
            let mut decoder_dead = false;
            for _ in 0..segment.frames {
                if cancel.load(Ordering::Relaxed) {
                    children.kill(dec_id);
                    break 'segments;
                }
                let buf: &[u8] = if decoder_dead {
                    last_frame.as_deref().unwrap_or(&black)
                } else {
                    let mut filled = 0;
                    while filled < frame_size {
                        match dec_out.read(&mut frame[filled..]) {
                            Ok(0) => break,
                            Ok(n) => filled += n,
                            Err(_) => break,
                        }
                    }
                    if filled == frame_size {
                        last_frame = Some(frame.clone());
                        &frame
                    } else {
                        // Decoder liefert weniger als geplant (Quelle kürzer,
                        // defekte Datei): letzten Frame halten statt abbrechen.
                        decoder_dead = true;
                        last_frame.as_deref().unwrap_or(&black)
                    }
                };
                if let Err(e) = enc_in.write_all(buf) {
                    write_err = Some(e.to_string());
                    children.kill(dec_id);
                    break 'segments;
                }
                progress.advance(1);
            }
            children.kill(dec_id);
            continue;
        }

        // Compositing-Pfad: ein Decoder je Layer, CPU mischt jeden Frame.
        match render_segment_composited(
            segment,
            seg_start_frame,
            video,
            fps,
            enc_in,
            cancel,
            children,
            &nest_ctx,
            progress,
        ) {
            Ok(()) => {}
            Err(CompErr::Cancelled) => break 'segments,
            Err(CompErr::Failed(e)) => {
                write_err = Some(e);
                break 'segments;
            }
        }
    }

    match write_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

enum CompErr {
    Cancelled,
    Failed(String),
}

/// Frame-Quelle eines Compositing-Layers: laufende Decoder-Pipe (vorwärts)
/// oder chunkweises Rückwärts-Dekodieren mit Frame-Puffer.
enum LayerSource {
    Pipe { dec_id: u64, out: ChildStdout },
    Reverse(ReverseDecode),
}

/// Rückwärts-Wiedergabe: ffmpeg streamt nur vorwärts — Chunks VOR der
/// Zielzeit werden vorwärts dekodiert (identische setpts/fps-Kette wie der
/// Vorwärtspfad ⇒ identische Frame-Auswahl) und rückwärts ausgeliefert.
struct ReverseDecode {
    path: String,
    /// Komplette -vf-Kette (setpts + fps + scale + pad).
    filter: String,
    /// Medienzeit des NÄCHSTEN auszugebenden Frames (läuft abwärts).
    media_next: f64,
    /// Medien-Sekunden pro Ausgabeframe (|media_step| / fps).
    step: f64,
    chunk_frames: usize,
    /// Gepufferte Frames in Ausgabe-Reihenfolge (Medienzeit absteigend).
    buf: std::collections::VecDeque<Vec<u8>>,
    exhausted: bool,
}

impl ReverseDecode {
    /// Nächsten Chunk synchron dekodieren (Worker-Thread; Abbruch killt die
    /// Kindprozesse über die Registry und beendet die Reads).
    fn refill(&mut self, children: &mut ChildRegistry, frame_size: usize) {
        if self.exhausted {
            return;
        }
        if self.media_next < -0.5 * self.step {
            self.exhausted = true;
            return;
        }
        let top = self.media_next.max(0.0);
        let want = self.chunk_frames.max(1);
        let lo = (top - (want as f64 - 1.0) * self.step).max(0.0);
        let n = (((top - lo) / self.step.max(1e-9)).round() as usize) + 1;
        let mut cmd = Command::new(crate::services::ffmpeg_bin());
        cmd.args(["-v", "error", "-ss", &format!("{lo:.4}")])
            .args(["-i", &self.path])
            .args(["-an", "-sn"])
            .args(["-vf", &self.filter])
            .args(["-frames:v", &n.to_string()])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let Ok((id, _, Some(mut out), _)) = children.spawn(&mut cmd) else {
            self.exhausted = true;
            return;
        };
        let mut frames: Vec<Option<Vec<u8>>> = Vec::with_capacity(n);
        'read: for _ in 0..n {
            let mut buf = vec![0u8; frame_size];
            let mut filled = 0;
            while filled < frame_size {
                match out.read(&mut buf[filled..]) {
                    Ok(0) => break 'read,
                    Ok(k) => filled += k,
                    Err(_) => break 'read,
                }
            }
            if filled < frame_size {
                break;
            }
            frames.push(Some(buf));
        }
        children.kill(id);
        if frames.is_empty() {
            self.exhausted = true;
            return;
        }
        // Ausgabe-Reihenfolge: Medienzeit absteigend; am Chunk-Ende fehlende
        // Frames (EOF) hält der letzte dekodierte.
        let m = frames.len();
        for i in (0..n).rev() {
            let idx = i.min(m - 1);
            let f = if i == idx {
                frames[idx].take().expect("Frame einmal entnommen")
            } else {
                frames[idx].clone().expect("Frame noch vorhanden")
            };
            self.buf.push_back(f);
        }
        self.media_next = lo - self.step;
        if lo <= 0.0 {
            // Unterhalb von Medienzeit 0 gibt es nichts mehr.
            self.exhausted = true;
        }
    }
}

/// Ein Layer-Decoder im Compositing-Pfad: liefert transparent gepolsterte
/// RGBA-Frames in Decode-Auflösung (das volle Zielframe repräsentierend).
struct LayerStream {
    src: LayerSource,
    /// Letzter vollständiger Frame, f32-RGBA 0..1 (initial transparent).
    frame: Vec<f32>,
    /// Roh-Bytes aus der Decoder-Pipe (`w*h*src_bpp`), vor der f32-Wandlung.
    read_buf: Vec<u8>,
    /// Bytes pro Pixel der Quelle: 4 = rgba8, 8 = rgba64le (>8-Bit-Quelle).
    src_bpp: usize,
    dead: bool,
    w: usize,
    h: usize,
    src_in: f64,
    /// Medienfortschritt pro Ausgabesekunde (signiert; 0 = Standbild).
    media_step: f64,
    fx: ClipFx,
    /// Effekt-Stapel (Keyframes in Medienzeit) — vor dem Grading angewendet.
    effects: Vec<EffectInstance>,
    /// Vorberechnete Farbkorrektur (Identität ⇒ kein Grading-Pass).
    grade: grade::GradeParams,
    /// Sichtbarer Inhalt im gepolsterten Puffer (Vignetten-Bezugsrahmen).
    content: (usize, usize, usize, usize),
    /// Übergangs-Fenster dieses Layers (Exportzeit).
    transitions: Vec<PlanTransition>,
}

/// Ein Layer des Compositing-Pfads: Decoder-Stream, Farbfläche (Dip) oder
/// CPU-gerasterter Titel.
enum SegLayer {
    Stream(LayerStream),
    Solid {
        /// 2×2-f32-RGBA-Puffer in Volltonfarbe (bilinear gesampelt = uniform).
        data: Vec<f32>,
        transitions: Vec<PlanTransition>,
    },
    Title(TitleLayer),
    /// Verschachtelte Sequenz, rekursiv komponiert.
    Nest(NestLayer),
}

/// Titel-Layer: einmal gerastert (gemeinsamer Rasterizer mit dem
/// Programmmonitor), statisch über das Segment; Effekte mit Keyframes
/// laufen pro Frame über eine Arbeitskopie (Reihenfolge wie bei
/// Decoder-Layern: Effekte → Farbkorrektur).
struct TitleLayer {
    base: Vec<f32>,
    scratch: Vec<f32>,
    use_scratch: bool,
    w: usize,
    h: usize,
    /// Vertikale Raster-Erweiterung (Abspann-Rolle): Quad-Höhe × k.
    extend_k: u32,
    src_in: f64,
    /// Medienfortschritt pro Ausgabesekunde (signiert; 0 = Standbild).
    media_step: f64,
    fx: ClipFx,
    effects: Vec<EffectInstance>,
    grade: grade::GradeParams,
    transitions: Vec<PlanTransition>,
}

impl TitleLayer {
    /// Frame vorbereiten: ohne aktive Effekte bleibt der (ggf. vorab
    /// gegradete) Basis-Raster stehen; sonst Kopie → Effekte → Grading.
    fn advance(&mut self, threads: usize, t_off: f64) {
        self.use_scratch = false;
        if self.effects.is_empty() {
            return;
        }
        let resolved = effects::resolve_video_effects(
            &self.effects,
            self.src_in + t_off * self.media_step,
        );
        self.scratch.copy_from_slice(&self.base);
        if !resolved.is_empty() {
            effects::apply_effects_buffer(
                &mut self.scratch,
                self.w,
                self.h,
                (0, 0, self.w, self.h),
                &resolved,
                threads,
            );
        }
        if !self.grade.is_identity() {
            grade::grade_buffer(
                &mut self.scratch,
                self.w,
                self.h,
                (0, 0, self.w, self.h),
                &self.grade,
                threads,
            );
        }
        self.use_scratch = true;
    }

    fn current(&self) -> &[f32] {
        if self.use_scratch {
            &self.scratch
        } else {
            &self.base
        }
    }
}

impl LayerStream {
    /// Nächsten Frame einlesen; bei EOF/Kurz-Read bleibt der letzte stehen.
    /// Frische Frames laufen direkt durch Effekte + Farbkorrektur (in
    /// place — der gehaltene EOF-Frame ist damit bereits verarbeitet).
    /// `t_off` = Segmentzeit des Frames (Effekt-Keyframes in Medienzeit).
    fn advance(&mut self, threads: usize, t_off: f64, children: &mut ChildRegistry) {
        if self.dead {
            return;
        }
        // Roh-Bytes der Quelle (rgba8 oder rgba64le) lesen.
        let size = self.w * self.h * self.src_bpp;
        let got = match &mut self.src {
            LayerSource::Pipe { out, .. } => {
                let mut filled = 0;
                while filled < size {
                    match out.read(&mut self.read_buf[filled..size]) {
                        Ok(0) => break,
                        Ok(n) => filled += n,
                        Err(_) => break,
                    }
                }
                filled == size
            }
            LayerSource::Reverse(rev) => {
                if rev.buf.is_empty() {
                    rev.refill(children, size);
                }
                match rev.buf.pop_front() {
                    Some(f) => {
                        self.read_buf.copy_from_slice(&f);
                        true
                    }
                    None => false,
                }
            }
        };
        if got {
            // Roh-Bytes → f32-RGBA (display-referred 0..1).
            if self.src_bpp == 8 {
                crate::core::pixbuf::rgba64le_into_f32(&self.read_buf, &mut self.frame);
            } else {
                crate::core::pixbuf::rgba8_into_f32(&self.read_buf, &mut self.frame);
            }
            if !self.effects.is_empty() {
                let resolved = effects::resolve_video_effects(
                    &self.effects,
                    self.src_in + t_off * self.media_step,
                );
                if !resolved.is_empty() {
                    effects::apply_effects_buffer(
                        &mut self.frame,
                        self.w,
                        self.h,
                        self.content,
                        &resolved,
                        threads,
                    );
                }
            }
            if !self.grade.is_identity() {
                grade::grade_buffer(
                    &mut self.frame,
                    self.w,
                    self.h,
                    self.content,
                    &self.grade,
                    threads,
                );
            }
        } else {
            self.dead = true;
        }
    }
}

/// Segment mit Transformationen rendern: je Layer ein ffmpeg-Decoder
/// (Decode-Auflösung wächst mit der maximalen Skalierung im Segment, damit
/// Zooms scharf bleiben), pro Frame werden die animierten Parameter
/// ausgewertet und die Layer per CPU-Compositor auf das Canvas gemischt.
#[allow(clippy::too_many_arguments)]
/// Self-contained Nest-Kontext des Worker-Threads: rekonstruierte innere
/// Timelines + Blatt-Medien. Dient als [`compose::NestResolver`] für die
/// rekursive Komposition.
struct NestRenderCtx {
    timelines: HashMap<String, TimelineStore>,
    media: HashMap<String, NestMediaInfo>,
}

impl NestRenderCtx {
    fn from_plan(plan: &RenderPlan) -> NestRenderCtx {
        NestRenderCtx {
            timelines: plan
                .nests
                .iter()
                .map(|(id, ns)| (id.clone(), ns.to_timeline()))
                .collect(),
            media: plan.nest_media.clone(),
        }
    }
}

impl compose::NestResolver for NestRenderCtx {
    fn nested_timeline(&self, seq_id: &str) -> Option<&TimelineStore> {
        self.timelines.get(seq_id)
    }
}

/// Ein Blatt-Frame einer verschachtelten Sequenz per Einzelbild-Extraktion
/// (Original, contain-fit + transparent gepolstert, w×h). Generatoren
/// (Titel/Untertitel) innerhalb von Nests werden hier (noch) nicht gerastert.
fn leaf_frame(
    clip: &TimelineClip,
    media_t: f64,
    w: usize,
    h: usize,
    media: &HashMap<String, NestMediaInfo>,
    children: &mut ChildRegistry,
) -> Option<Vec<f32>> {
    if clip.is_generator() {
        return None;
    }
    let info = media.get(&clip.asset_id)?;
    let filter = format!(
        "scale={w}:{h}:force_original_aspect_ratio=decrease:flags=bicubic,format=rgba,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black@0.0"
    );
    let mut cmd = Command::new(crate::services::ffmpeg_bin());
    cmd.args(["-v", "error"]);
    if info.image {
        cmd.args(["-loop", "1", "-framerate", "1"]);
    } else {
        cmd.args(["-ss", &format!("{:.4}", media_t.max(0.0))]);
    }
    cmd.args(["-i", &info.path])
        .args(["-an", "-sn"])
        .args(["-vf", &filter])
        .args(["-frames:v", "1"])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
        .arg("pipe:1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let (id, _, stdout, _) = children.spawn(&mut cmd).ok()?;
    let mut out = stdout?;
    let mut buf = vec![0u8; w * h * 4];
    let mut filled = 0;
    while filled < buf.len() {
        match out.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(k) => filled += k,
            Err(_) => break,
        }
    }
    children.kill(id);
    if filled < buf.len() {
        return None;
    }
    // Blatt-Decode bleibt vorerst 8-Bit (rgba) → f32. Höhere Quell-Bittiefe
    // für Nest-Blätter ist eine spätere Verfeinerung (Stufe 1).
    Some(crate::core::pixbuf::rgba8_to_f32(&buf))
}

/// Nest-Layer im Compositing-Pfad: hält die rekursiv komponierte innere
/// Sequenz als volles Zielframe-Puffer (w×h), auf den die äußeren Clip-
/// Parameter (Effekte/Grade/Transform/Übergang) wirken.
struct NestLayer {
    seq_id: String,
    /// Compositing-Puffergröße (innere Auflösung × Skalierung).
    w: usize,
    h: usize,
    /// Natürliche Größe für die Quad-Berechnung (innere Sequenzauflösung) —
    /// das innere Frame wird contain-fit ins äußere gelegt.
    nw: usize,
    nh: usize,
    src_in: f64,
    media_step: f64,
    fx: ClipFx,
    grade: grade::GradeParams,
    effects: Vec<EffectInstance>,
    transitions: Vec<PlanTransition>,
    frame: Vec<f32>,
}

impl NestLayer {
    fn advance(
        &mut self,
        threads: usize,
        t_off: f64,
        ctx: &NestRenderCtx,
        children: &mut ChildRegistry,
    ) {
        let inner_t = self.src_in + t_off * self.media_step;
        let Some(inner_tl) = ctx.timelines.get(&self.seq_id) else {
            return;
        };
        let (w, h) = (self.w, self.h);
        let media = &ctx.media;
        let mut fetch = |clip: &TimelineClip, _media_t: f64, lw: usize, lh: usize| {
            leaf_frame(clip, _media_t, lw, lh, media, children)
        };
        self.frame =
            compose::composite_sequence_frame(inner_tl, ctx, inner_t, w, h, threads, &mut fetch, 1);
        // Effekte + Farbkorrektur des Nest-Clips auf das komponierte Frame
        // (gleiche Reihenfolge wie bei Decoder-Layern: Effekte → Grading).
        if !self.effects.is_empty() {
            let resolved = effects::resolve_video_effects(&self.effects, inner_t);
            if !resolved.is_empty() {
                effects::apply_effects_buffer(&mut self.frame, w, h, (0, 0, w, h), &resolved, threads);
            }
        }
        if !self.grade.is_identity() {
            grade::grade_buffer(&mut self.frame, w, h, (0, 0, w, h), &self.grade, threads);
        }
    }
}

fn render_segment_composited(
    segment: &VideoSegment,
    seg_start_frame: u64,
    video: &VideoSettings,
    fps_arg: &str,
    enc_in: &mut std::process::ChildStdin,
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
    nests: &NestRenderCtx,
    progress: &mut dyn FrameProgress,
) -> Result<(), CompErr> {
    let (tw, th) = (video.width as usize, video.height as usize);
    let fps = video.fps;
    let seg_dur = segment.frames as f64 / fps;
    // Pipe-Bittiefe (= Ziel-Bittiefe): bestimmt die finale Quantisierung.
    let hi_bit = pipe_hi_bit(video);

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8);
    let mut layers: Vec<SegLayer> = Vec::new();
    let kill_layers = |children: &ChildRegistry, layers: &[SegLayer]| {
        for l in layers {
            if let SegLayer::Stream(LayerStream {
                src: LayerSource::Pipe { dec_id, .. },
                ..
            }) = l
            {
                children.kill(*dec_id);
            }
        }
    };

    for plan_layer in &segment.layers {
        // Titel: CPU-Raster statt Decoder — Auflösung wächst mit der
        // maximalen Skalierung im Segment (Schärfe bei Zoom, wie Streams).
        if let Some(spec) = &plan_layer.title {
            let m1 = plan_layer.src_in + seg_dur * plan_layer.media_step;
            let max_s = compose::max_scale_in_window(
                &plan_layer.fx,
                plan_layer.src_in.min(m1),
                plan_layer.src_in.max(m1),
            )
            .clamp(1.0, 2.0);
            let dw = ((((tw as f64 * max_s) / 2.0).round() as usize) * 2).clamp(2, 4096);
            let dh = ((((th as f64 * max_s) / 2.0).round() as usize) * 2).clamp(2, 4096);
            let raster = crate::core::text_raster::render_title(spec, dw as u32, dh as u32);
            let (rw, rh, extend_k) = (raster.w, raster.h, raster.extend_k);
            let mut base = crate::core::pixbuf::rgba8_to_f32(&raster.data);
            let grade_params = grade::precompute(&plan_layer.grade);
            let dynamic = effects::has_active_video_effects(&plan_layer.effects);
            if !dynamic && !grade_params.is_identity() {
                // Ohne Effekte ist das Grading statisch — einmal einbrennen.
                grade::grade_buffer(&mut base, rw, rh, (0, 0, rw, rh), &grade_params, threads);
            }
            layers.push(SegLayer::Title(TitleLayer {
                scratch: if dynamic { base.clone() } else { Vec::new() },
                base,
                use_scratch: false,
                w: rw,
                h: rh,
                extend_k,
                src_in: plan_layer.src_in,
                media_step: plan_layer.media_step,
                fx: plan_layer.fx.clone(),
                effects: if dynamic { plan_layer.effects.clone() } else { Vec::new() },
                grade: grade_params,
                transitions: plan_layer.transitions.clone(),
            }));
            continue;
        }
        // Farbflächen (Dips) brauchen keinen Decoder (2×2-f32-Voltonfläche).
        if let Some(color) = plan_layer.solid {
            let c = [
                color[0] as f32 / 255.0,
                color[1] as f32 / 255.0,
                color[2] as f32 / 255.0,
                1.0,
            ];
            let mut data = Vec::with_capacity(2 * 2 * 4);
            for _ in 0..4 {
                data.extend_from_slice(&c);
            }
            layers.push(SegLayer::Solid {
                data,
                transitions: plan_layer.transitions.clone(),
            });
            continue;
        }
        // Verschachtelte Sequenz: rekursiv komponieren statt dekodieren. Die
        // Decode-Auflösung folgt (wie Streams) der maximalen Skalierung.
        if let Some(seq_id) = &plan_layer.nest_seq {
            let m1 = plan_layer.src_in + seg_dur * plan_layer.media_step;
            let max_s = compose::max_scale_in_window(
                &plan_layer.fx,
                plan_layer.src_in.min(m1),
                plan_layer.src_in.max(m1),
            )
            .clamp(1.0, 2.0);
            // Natürliche Größe = innere Sequenzauflösung (Fallback: Zielraster).
            let (nw, nh) = if plan_layer.natural_w > 0 && plan_layer.natural_h > 0 {
                (plan_layer.natural_w as usize, plan_layer.natural_h as usize)
            } else {
                (tw, th)
            };
            // Compositing-Puffer in INNERER Auflösung × Skalierung (Schärfe bei
            // Zoom), Seitenverhältnis der inneren Sequenz bewahrt.
            let dw = ((((nw as f64 * max_s) / 2.0).round() as usize) * 2).clamp(2, 4096);
            let dh = ((((nh as f64 * max_s) / 2.0).round() as usize) * 2).clamp(2, 4096);
            layers.push(SegLayer::Nest(NestLayer {
                seq_id: seq_id.clone(),
                w: dw,
                h: dh,
                nw,
                nh,
                src_in: plan_layer.src_in,
                media_step: plan_layer.media_step,
                fx: plan_layer.fx.clone(),
                grade: grade::precompute(&plan_layer.grade),
                effects: plan_layer.effects.clone(),
                transitions: plan_layer.transitions.clone(),
                // Initial opak schwarz, bis advance das erste Frame liefert.
                frame: {
                    let mut b = vec![0f32; dw * dh * 4];
                    for px in b.chunks_exact_mut(4) {
                        px[3] = 1.0;
                    }
                    b
                },
            }));
            continue;
        }
        // Decode-Auflösung: Zielgröße × max. Skalierung (gedeckelt) — mehr
        // als die Quelle hergibt, skaliert ffmpeg ohnehin nicht hoch (Schärfe
        // gewinnt nur, solange Quellpixel vorhanden sind).
        let m1 = plan_layer.src_in + seg_dur * plan_layer.media_step;
        let max_s = compose::max_scale_in_window(
            &plan_layer.fx,
            plan_layer.src_in.min(m1),
            plan_layer.src_in.max(m1),
        )
        .clamp(1.0, 2.0);
        let dw = ((((tw as f64 * max_s) / 2.0).round() as usize) * 2).clamp(2, 4096);
        let dh = ((((th as f64 * max_s) / 2.0).round() as usize) * 2).clamp(2, 4096);

        let freeze = !plan_layer.image && plan_layer.media_step == 0.0;
        let reverse = !plan_layer.image && plan_layer.media_step < 0.0;
        // Konstante Geschwindigkeit über dieselbe setpts/fps-Kette wie der
        // Schnellpfad und die Vorschau (identische Frame-Auswahl).
        let setpts = if plan_layer.image || freeze {
            String::new()
        } else {
            speed_setpts_filter(plan_layer.media_step.abs())
        };
        // Contain-Fit + TRANSPARENTES Padding: der Puffer repräsentiert das
        // volle Frame, Alpha (z. B. PNG) bleibt erhalten. >8-Bit-Quelle ⇒
        // 16-Bit-Decode (rgba64le), damit Log-/HDR-Material verlustfrei in die
        // f32-Pipeline läuft; 8-Bit-Quelle bleibt rgba (keine Bandbreite drauf).
        let src_hi = plan_layer.src_bit_depth > 8;
        let src_fmt = if src_hi { "rgba64le" } else { "rgba" };
        let src_bpp = if src_hi { 8 } else { 4 };
        let filter = format!(
            "{setpts}fps={fps_arg},scale={dw}:{dh}:force_original_aspect_ratio=decrease:flags=bicubic,format={src_fmt},pad={dw}:{dh}:(ow-iw)/2:(oh-ih)/2:color=black@0.0"
        );
        // Sichtbarer Inhalt im Puffer: contain-fit der Quelle, zentriert
        // (Spiegel der ffmpeg-Filterkette scale=…:decrease + pad-center).
        let content = if plan_layer.natural_w > 0 && plan_layer.natural_h > 0 {
            let (nw, nh) = (plan_layer.natural_w as f64, plan_layer.natural_h as f64);
            let fit = (dw as f64 / nw).min(dh as f64 / nh);
            let cw = ((nw * fit).round() as usize).clamp(1, dw);
            let ch = ((nh * fit).round() as usize).clamp(1, dh);
            ((dw - cw) / 2, (dh - ch) / 2, cw, ch)
        } else {
            (0, 0, dw, dh)
        };

        let source = if reverse {
            // Rückwärts: Chunk-Decode mit Frame-Puffer (Budget ≈ 192 MB).
            let frame_bytes = dw * dh * src_bpp;
            let chunk = (192 * 1024 * 1024 / frame_bytes.max(1)).clamp(2, 128);
            LayerSource::Reverse(ReverseDecode {
                path: plan_layer.path.clone(),
                filter: filter.clone(),
                media_next: plan_layer.src_in,
                step: plan_layer.media_step.abs() / fps,
                chunk_frames: chunk,
                buf: Default::default(),
                exhausted: false,
            })
        } else {
            let mut cmd = Command::new(crate::services::ffmpeg_bin());
            cmd.args(["-v", "error"]);
            if plan_layer.image {
                cmd.args(["-loop", "1", "-framerate", fps_arg]);
            } else {
                cmd.args(["-ss", &format!("{:.4}", plan_layer.src_in)]);
            }
            // Standbild: ein Frame genügt — die Halte-Logik füllt den Rest.
            let dec_frames = if freeze { 1 } else { segment.frames };
            cmd.args(["-i", &plan_layer.path])
                .args(["-an", "-sn"])
                .args(["-vf", &filter])
                .args(["-frames:v", &dec_frames.to_string()])
                .args(["-f", "rawvideo", "-pix_fmt", src_fmt])
                .arg("pipe:1")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let (dec_id, _, stdout, _) = match children.spawn(&mut cmd) {
                Ok(v) => v,
                Err(e) => {
                    kill_layers(children, &layers);
                    return Err(CompErr::Failed(e));
                }
            };
            let Some(out) = stdout else {
                kill_layers(children, &layers);
                return Err(CompErr::Failed("Decoder-stdout nicht verfügbar".into()));
            };
            LayerSource::Pipe { dec_id, out }
        };
        layers.push(SegLayer::Stream(LayerStream {
            src: source,
            // f32-Frame (verarbeitet) + Roh-Byte-Lesepuffer (Quell-Bittiefe:
            // 4 B/px = rgba8, 8 B/px = rgba64le).
            frame: vec![0f32; dw * dh * 4],
            read_buf: vec![0u8; dw * dh * src_bpp],
            src_bpp,
            dead: false,
            w: dw,
            h: dh,
            src_in: plan_layer.src_in,
            media_step: plan_layer.media_step,
            fx: plan_layer.fx.clone(),
            effects: plan_layer.effects.clone(),
            grade: grade::precompute(&plan_layer.grade),
            content,
            transitions: plan_layer.transitions.clone(),
        }));
    }

    let mut canvas = vec![0f32; tw * th * 4];

    for f in 0..segment.frames {
        if cancel.load(Ordering::Relaxed) {
            kill_layers(children, &layers);
            return Err(CompErr::Cancelled);
        }
        for layer in &mut layers {
            match layer {
                SegLayer::Stream(s) => s.advance(threads, f as f64 / fps, children),
                SegLayer::Title(t) => t.advance(threads, f as f64 / fps),
                SegLayer::Nest(n) => n.advance(threads, f as f64 / fps, nests, children),
                SegLayer::Solid { .. } => {}
            }
        }
        // Canvas opak schwarz zurücksetzen (f32).
        for px in canvas.chunks_exact_mut(4) {
            px[0] = 0.0;
            px[1] = 0.0;
            px[2] = 0.0;
            px[3] = 1.0;
        }
        let t_off = f as f64 / fps;
        // Exportzeit des Frames — Bezugssystem der Übergangs-Fenster.
        let seq_t = (seg_start_frame + f) as f64 / fps;
        let frames: Vec<compose::CpuLayerFrame> = layers
            .iter()
            .filter_map(|layer| match layer {
                SegLayer::Stream(l) => {
                    let fx = compose::eval_fx(&l.fx, l.src_in + t_off * l.media_step);
                    let t_fx = eval_plan_transitions(&l.transitions, seq_t);
                    let opacity = fx.opacity * t_fx.opacity;
                    if opacity <= 0.0 {
                        return None;
                    }
                    // Der Layer-Puffer repräsentiert das volle Frame →
                    // natürliche Größe = Framegröße (Fit-Faktor 1).
                    let mut quad =
                        compose::layer_quad(tw as f64, th as f64, tw as f64, th as f64, &fx);
                    compose::apply_transition_to_quad(&mut quad, &t_fx, tw as f64, th as f64);
                    Some(compose::CpuLayerFrame {
                        data: &l.frame,
                        w: l.w,
                        h: l.h,
                        quad,
                        opacity,
                        mask: t_fx.mask.map(|m| compose::mask_to_pixels(&m, tw, th)),
                    })
                }
                SegLayer::Solid { data, transitions } => {
                    let t_fx = eval_plan_transitions(transitions, seq_t);
                    if t_fx.opacity <= 0.0 {
                        return None;
                    }
                    Some(compose::CpuLayerFrame {
                        data,
                        w: 2,
                        h: 2,
                        quad: compose::LayerQuad {
                            cx: tw as f64 / 2.0,
                            cy: th as f64 / 2.0,
                            w: tw as f64,
                            h: th as f64,
                            rot_deg: 0.0,
                        },
                        opacity: t_fx.opacity,
                        mask: None,
                    })
                }
                // Identische Quad-Mathematik wie Streams: der Titel-Raster
                // repräsentiert das volle Frame (Fit-Faktor 1).
                SegLayer::Title(l) => {
                    let fx = compose::eval_fx(&l.fx, l.src_in + t_off * l.media_step);
                    let t_fx = eval_plan_transitions(&l.transitions, seq_t);
                    let opacity = fx.opacity * t_fx.opacity;
                    if opacity <= 0.0 {
                        return None;
                    }
                    let mut quad =
                        compose::layer_quad(tw as f64, th as f64, tw as f64, th as f64, &fx);
                    // Erweiterter Raster (Abspann): Quad vertikal strecken.
                    quad.h *= l.extend_k as f64;
                    compose::apply_transition_to_quad(&mut quad, &t_fx, tw as f64, th as f64);
                    Some(compose::CpuLayerFrame {
                        data: l.current(),
                        w: l.w,
                        h: l.h,
                        quad,
                        opacity,
                        mask: t_fx.mask.map(|m| compose::mask_to_pixels(&m, tw, th)),
                    })
                }
                // Nest: das innere Frame (innere Auflösung) wird contain-fit
                // ins äußere Frame gelegt — natürliche Größe = innere Auflösung.
                SegLayer::Nest(l) => {
                    let fx = compose::eval_fx(&l.fx, l.src_in + t_off * l.media_step);
                    let t_fx = eval_plan_transitions(&l.transitions, seq_t);
                    let opacity = fx.opacity * t_fx.opacity;
                    if opacity <= 0.0 {
                        return None;
                    }
                    let mut quad =
                        compose::layer_quad(tw as f64, th as f64, l.nw as f64, l.nh as f64, &fx);
                    compose::apply_transition_to_quad(&mut quad, &t_fx, tw as f64, th as f64);
                    Some(compose::CpuLayerFrame {
                        data: &l.frame,
                        w: l.w,
                        h: l.h,
                        quad,
                        opacity,
                        mask: t_fx.mask.map(|m| compose::mask_to_pixels(&m, tw, th)),
                    })
                }
            })
            .collect();
        compose::composite_frame(&mut canvas, tw, th, &frames, threads);
        // f32-Canvas → Pipe-Format: 16 Bit (rgba64le, verlustarm) für >8-Bit-
        // Ziele, sonst 8 Bit mit TPDF-Dithering (bricht Restbanding).
        let out_bytes = if hi_bit {
            crate::core::pixbuf::f32_to_rgba64le(&canvas)
        } else {
            crate::core::pixbuf::f32_to_rgba8_dithered(&canvas, tw, th)
        };
        if let Err(e) = enc_in.write_all(&out_bytes) {
            kill_layers(children, &layers);
            return Err(CompErr::Failed(e.to_string()));
        }
        progress.advance(1);
    }
    kill_layers(children, &layers);
    Ok(())
}

fn encode_audio_only(
    settings: &ExportSettings,
    wav: &Path,
    part: &Path,
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
) -> Result<(), String> {
    let audio = settings.audio.as_ref().expect("audio settings");
    let mut cmd = Command::new(crate::services::ffmpeg_bin());
    cmd.args(["-y", "-v", "error"])
        .args(["-i", &wav.to_string_lossy()])
        .args(["-map", "0:a:0"])
        .args(audio_codec_args(audio));
    if settings.container.faststart {
        cmd.args(["-movflags", "+faststart"]);
    }
    cmd.args(["-f", settings.container.muxer]);
    cmd.arg(part)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let (id, _, _, stderr) = children.spawn(&mut cmd)?;
    let mut stderr = stderr.ok_or("Encoder-stderr nicht verfügbar")?;
    let stderr_task = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let status = children.wait(id);
    let stderr_buf = stderr_task.join().unwrap_or_default();
    if cancel.load(Ordering::Relaxed) {
        return Err("abgebrochen".into());
    }
    if !status.map(|s| s.success()).unwrap_or(false) {
        let tail = stderr_tail(&stderr_buf);
        return Err(format!(
            "Audio-Encoder fehlgeschlagen: {}",
            if tail.is_empty() { "ohne Fehlermeldung".into() } else { tail }
        ));
    }
    Ok(())
}

fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines
        .iter()
        .rev()
        .take(4)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

// ==================================================================== Tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::timeline::{TimelineClip, TimelineTrack};
    use crate::core::types::{MediaAsset, MediaInfo, VideoStreamInfo};

    fn track(id: &str, kind: TrackKind) -> TimelineTrack {
        TimelineTrack {
            id: id.into(),
            kind,
            muted: false,
            solo: false,
            locked: false,
            gain_db: 0.0,
            pan: 0.0,
            sync_lock: false,
            targeted: false,
            source_patched: false,
            subtitle_style: None,
            effects: Vec::new(),
            volume_auto: crate::core::animation::AnimatedParam::fixed(0.0),
            pan_auto: crate::core::animation::AnimatedParam::fixed(0.0),
        }
    }

    fn clip(id: &str, track_id: &str, kind: TrackKind, asset: &str, start: f64, dur: f64) -> TimelineClip {
        TimelineClip {
            id: id.into(),
            track_id: track_id.into(),
            asset_id: asset.into(),
            name: id.into(),
            kind,
            start,
            duration: dur,
            src_in: 0.0,
            src_duration: 3600.0,
            link_id: None,
            enabled: true,
            gain_db: 0.0,
            fx: Default::default(),
            grade: Default::default(),
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

    fn video_asset(id: &str, path: &str) -> MediaAsset {
        MediaAsset {
            id: id.into(),
            path: path.into(),
            name: path.into(),
            kind: MediaKind::Video,
            info: MediaInfo {
                path: path.into(),
                file_name: path.into(),
                container: "mov".into(),
                duration_sec: 3600.0,
                size_bytes: 1,
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
                audio: vec![crate::core::types::AudioStreamInfo {
                    index: 1,
                    codec: "aac".into(),
                    channels: 2,
                    sample_rate: 48000,
                    bitrate: None,
                }],
                recorded_at: None,
            },
            thumbnail_path: None,
            imported_at: 0.0,
            bin_id: crate::core::bin::ROOT_BIN_ID.to_string(),
            label: None,
            offline: false,
            markers: Vec::new(),
            proxy_path: None,
            proxy_src_mtime: None,
            proxy_offline: false,
        }
    }

    fn test_settings() -> ExportSettings {
        ExportSettings {
            container: container("mp4"),
            video: Some(default_video("h264", 1280, 720, 25.0)),
            audio: Some(default_audio("aac", None)),
            use_in_out: false,
            subtitles: SubtitleMode::None,
            image_start: 1,
            output: "/tmp/out.mp4".into(),
        }
    }

    fn state_with(
        tracks: Vec<TimelineTrack>,
        clips: Vec<TimelineClip>,
        assets: Vec<MediaAsset>,
    ) -> (TimelineStore, MediaStore) {
        let mut tl = TimelineStore::default();
        tl.tracks = tracks;
        tl.clips = clips;
        let mut media = MediaStore::default();
        for a in assets {
            media.add_asset(a);
        }
        (tl, media)
    }

    #[test]
    fn export_always_uses_original_never_proxy() {
        // Kern-Invariante des Proxy-Workflows: Auch bei global aktivem Proxy-
        // Modus und vorhandenem, gültigem Proxy referenziert der Render-Plan
        // ausschließlich die ORIGINALdatei — der Export darf nie versehentlich
        // Proxy-Qualität ausgeben.
        let mut asset = video_asset("A", "/orig/clip.mp4");
        asset.proxy_path = Some("/proxies/clip_proxy.mov".into());
        asset.proxy_offline = false; // gültiger Proxy
        let (tl, mut media) = state_with(
            vec![track("v1", TrackKind::Video), track("a1", TrackKind::Audio)],
            vec![
                clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0),
                clip("au", "a1", TrackKind::Audio, "A", 0.0, 4.0),
            ],
            vec![asset],
        );
        media.use_proxies = true; // Proxy-Modus aktiv (würde die Vorschau lenken)
        assert!(media.asset("A").unwrap().has_valid_proxy());
        // Decode-Pfad der VORSCHAU nähme hier den Proxy …
        assert_eq!(media.asset("A").unwrap().decode_path(true), "/proxies/clip_proxy.mov");

        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        // … der EXPORT-Plan dagegen niemals.
        let video_paths: Vec<&str> = plan
            .segments
            .iter()
            .flat_map(|s| s.layers.iter())
            .map(|l| l.path.as_str())
            .collect();
        assert!(!video_paths.is_empty(), "Video-Segmente erwartet");
        for p in &video_paths {
            assert_eq!(*p, "/orig/clip.mp4", "Export-Video muss das Original nutzen");
        }
        assert!(!plan.audio.is_empty(), "Audio-Clips erwartet");
        for c in &plan.audio {
            assert_eq!(c.path, "/orig/clip.mp4", "Export-Audio muss das Original nutzen");
        }
    }

    #[test]
    fn nested_sequence_resolves_in_render_plan() {
        use crate::core::compose::NestResolver;
        // Innere Sequenz: ein Medien-Clip „A" (mit Asset).
        let (inner, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("a", "v1", TrackKind::Video, "A", 0.0, 10.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        // Äußere Sequenz: ein Nest-Clip ab 3 s, src_in 5 s (innere Sequenzzeit).
        let (mut outer, _) = state_with(
            vec![track("ov", TrackKind::Video)],
            vec![clip("n", "ov", TrackKind::Video, "A", 3.0, 4.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        outer.clips[0].asset_id = String::new();
        outer.clips[0].nest_seq = Some("inner".into());
        outer.clips[0].src_in = 5.0;
        let inner_w = inner.settings.width;

        struct R<'a>(HashMap<String, &'a TimelineStore>);
        impl NestResolver for R<'_> {
            fn nested_timeline(&self, id: &str) -> Option<&TimelineStore> {
                self.0.get(id).copied()
            }
        }
        let resolver = R(HashMap::from([("inner".to_string(), &inner)]));

        let plan = build_render_plan(&outer, &media, &test_settings(), &resolver);
        let nest_layer = plan
            .segments
            .iter()
            .flat_map(|s| s.layers.iter())
            .find(|l| l.nest_seq.as_deref() == Some("inner"))
            .expect("Nest-Ebene im Renderplan");
        // Auflösung der Nest-Ebene = innere Sequenzgröße.
        assert_eq!(nest_layer.natural_w, inner_w);
        // Frame-Zuordnung: Segment ab t=3 → innere Sequenzzeit = src_in 5.
        assert!((nest_layer.src_in - 5.0).abs() < 1e-6, "src_in = {}", nest_layer.src_in);
        assert!((nest_layer.media_step - 1.0).abs() < 1e-6);
        // Worker-Kontext eingesammelt: innere Sequenz + ihr Blatt-Medium.
        assert!(plan.nests.contains_key("inner"), "innere Sequenz im Kontext");
        let ctx = &plan.nests["inner"];
        assert_eq!(ctx.clips.len(), 1);
        assert!(plan.nest_media.contains_key("A"), "Blatt-Medium der inneren Sequenz");
        assert_eq!(plan.nest_media["A"].path, "/a.mp4");
        // Nest-Ebene zählt als Video (keine falsche „leer"-Erkennung).
        assert!(plan.has_video_media());
    }

    #[test]
    fn multicam_resolves_active_angle_in_render_plan() {
        use crate::core::compose::NestResolver;
        use crate::core::multicam::{MulticamAngle, MulticamClip, MulticamSource, MulticamSync};
        // Asset B genuin 1280×720, damit die natürliche Größe den aktiven
        // Winkel (B) eindeutig von A (1920×1080) unterscheidet.
        let mut asset_b = video_asset("B", "/b.mp4");
        asset_b.info.video[0].width = 1280;
        asset_b.info.video[0].height = 720;
        // Quell-Sequenz mit Multicam-Metadaten: Winkel A (pos 0) und B (pos 2).
        let (mut src, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            Vec::new(),
            vec![video_asset("A", "/a.mp4"), asset_b.clone()],
        );
        src.multicam = Some(MulticamSource {
            angles: vec![
                MulticamAngle {
                    name: "Kamera 1".into(),
                    asset_id: "A".into(),
                    pos: 0.0,
                    duration: 100.0,
                    width: 1920,
                    height: 1080,
                    fps: 25.0,
                    has_audio: true,
                },
                MulticamAngle {
                    name: "Kamera 2".into(),
                    asset_id: "B".into(),
                    pos: 2.0,
                    duration: 100.0,
                    width: 1280,
                    height: 720,
                    fps: 25.0,
                    has_audio: true,
                },
            ],
            audio_angle: None,
            sync: MulticamSync::Audio,
            duration: 102.0,
        });
        // Äußere Sequenz: Multicam-Clip ab 3 s, gemeinsame Zeit src_in 5 s,
        // aktiver Winkel 1 (= B).
        let (mut outer, _) = state_with(
            vec![track("ov", TrackKind::Video), track("oa", TrackKind::Audio)],
            vec![
                clip("mc", "ov", TrackKind::Video, "", 3.0, 4.0),
                clip("mca", "oa", TrackKind::Audio, "", 3.0, 4.0),
            ],
            vec![video_asset("A", "/a.mp4"), asset_b],
        );
        for c in outer.clips.iter_mut() {
            c.src_in = 5.0;
            c.multicam = Some(MulticamClip {
                source: "src".into(),
                angle: 1,
            });
        }

        struct R<'a>(HashMap<String, &'a TimelineStore>);
        impl NestResolver for R<'_> {
            fn nested_timeline(&self, id: &str) -> Option<&TimelineStore> {
                self.0.get(id).copied()
            }
        }
        let resolver = R(HashMap::from([("src".to_string(), &src)]));

        let plan = build_render_plan(&outer, &media, &test_settings(), &resolver);
        let layer = plan
            .segments
            .iter()
            .flat_map(|s| s.layers.iter())
            .find(|l| l.clip_id == "mc")
            .expect("Multicam-Ebene im Renderplan");
        // Aktiver Winkel B → Original /b.mp4, natürliche Größe 1280×720.
        assert_eq!(layer.path, "/b.mp4", "aktiver Winkel B");
        assert_eq!(layer.natural_w, 1280);
        assert_eq!(layer.natural_h, 720);
        // Asset-Medienzeit am Segmentbeginn = gemeinsame Zeit 5 − Winkel-pos 2 = 3.
        assert!((layer.src_in - 3.0).abs() < 1e-6, "src_in = {}", layer.src_in);
        // Audio folgt dem aktiven Winkel (audio_angle = None) ⇒ /b.mp4.
        assert!(
            plan.audio.iter().any(|c| c.path == "/b.mp4"),
            "Audio des aktiven Winkels"
        );
    }

    #[test]
    fn nested_sequence_audio_flattens_into_plan() {
        use crate::core::compose::NestResolver;
        // Innere Sequenz: ein Audio-Clip 0..10 auf einer Audiospur.
        let (inner, media) = state_with(
            vec![track("a1", TrackKind::Audio)],
            vec![clip("au", "a1", TrackKind::Audio, "AUD", 0.0, 10.0)],
            vec![video_asset("AUD", "/aud.mov")],
        );
        // Äußere Sequenz: Nest-Clip auf einer Audiospur, 3..7, src_in 5.
        let (mut outer, _) = state_with(
            vec![track("oa", TrackKind::Audio)],
            vec![clip("n", "oa", TrackKind::Audio, "AUD", 3.0, 4.0)],
            vec![video_asset("AUD", "/aud.mov")],
        );
        outer.clips[0].asset_id = String::new();
        outer.clips[0].nest_seq = Some("inner".into());
        outer.clips[0].src_in = 5.0;

        struct R<'a>(HashMap<String, &'a TimelineStore>);
        impl NestResolver for R<'_> {
            fn nested_timeline(&self, id: &str) -> Option<&TimelineStore> {
                self.0.get(id).copied()
            }
        }
        let resolver = R(HashMap::from([("inner".to_string(), &inner)]));
        let plan = build_render_plan(&outer, &media, &test_settings(), &resolver);
        // Inneres Audio zeitverschoben in den äußeren Mix eingeflacht.
        assert_eq!(plan.audio.len(), 1, "inneres Audio im Mix");
        let a = &plan.audio[0];
        assert_eq!(a.path, "/aud.mov");
        assert!((a.start_in_mix - 3.0).abs() < 1e-6, "start {}", a.start_in_mix);
        assert!((a.duration - 4.0).abs() < 1e-6, "dur {}", a.duration);
        assert!((a.src_in - 5.0).abs() < 1e-6, "src_in {}", a.src_in);
        assert!((a.gain_l - 1.0).abs() < 1e-3 && (a.gain_r - 1.0).abs() < 1e-3);
    }

    #[test]
    fn plan_splits_overlaps_into_layer_stacks() {
        // V2 (oben): Clip 2..4 — V1 (unten): Clip 0..10. 2..4 trägt BEIDE
        // Layer (A unten, B oben), davor/danach nur A; src_in läuft weiter.
        let (tl, media) = state_with(
            vec![track("v2", TrackKind::Video), track("v1", TrackKind::Video)],
            vec![
                clip("a", "v1", TrackKind::Video, "A", 0.0, 10.0),
                clip("b", "v2", TrackKind::Video, "B", 2.0, 2.0),
            ],
            vec![video_asset("A", "/a.mp4"), video_asset("B", "/b.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert_eq!(plan.total_frames, 250);
        assert_eq!(plan.segments.len(), 3);
        assert_eq!(plan.segments[0].frames, 50);
        assert_eq!(plan.segments[1].frames, 50);
        assert_eq!(plan.segments[2].frames, 150);
        assert_eq!(plan.segments[0].layers.len(), 1);
        // Überlappung: unten A, oben B (Zeichenreihenfolge).
        let mid = &plan.segments[1].layers;
        assert_eq!(mid.len(), 2, "Überlappung muss beide Layer tragen");
        assert_eq!(mid[0].path, "/a.mp4");
        assert_eq!(mid[1].path, "/b.mp4");
        let tail = &plan.segments[2].layers;
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].path, "/a.mp4");
        assert!(
            (tail[0].src_in - 4.0).abs() < 1e-6,
            "src_in muss weiterlaufen: {}",
            tail[0].src_in
        );
    }

    #[test]
    fn plan_carries_effects_and_disables_fast_path() {
        let mut c = clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0);
        c.effects
            .push(crate::core::effects::EffectInstance::new(
                crate::core::effects::EffectKind::GaussianBlur,
            ));
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![c],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        let layer = &plan.segments[0].layers[0];
        assert_eq!(layer.effects.len(), 1);
        assert!(
            !layer.is_identity(),
            "aktiver Effekt erzwingt den Compositing-Pfad"
        );
        // Deaktivierter Effekt ⇒ Schnellpfad bleibt erlaubt.
        let mut layer2 = layer.clone();
        layer2.effects[0].enabled = false;
        assert!(layer2.is_identity());
    }

    #[test]
    fn plan_audio_carries_audio_effects_only() {
        let mut c = clip("a", "a1", TrackKind::Audio, "A", 0.0, 4.0);
        c.effects
            .push(crate::core::effects::EffectInstance::new(
                crate::core::effects::EffectKind::Reverb,
            ));
        c.effects
            .push(crate::core::effects::EffectInstance::new(
                crate::core::effects::EffectKind::GaussianBlur,
            ));
        let (tl, media) = state_with(
            vec![track("a1", TrackKind::Audio)],
            vec![c],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert_eq!(plan.audio.len(), 1);
        assert_eq!(plan.audio[0].effects.len(), 1, "nur Audio-Effekte");
        assert_eq!(
            plan.audio[0].effects[0].kind,
            crate::core::effects::EffectKind::Reverb
        );
    }

    #[test]
    fn plan_routes_track_bus_fx_and_automation_to_processed() {
        use crate::core::effects::{EffectInstance, EffectKind};
        // Spur mit Bus-Effekt.
        let mut t1 = track("a1", TrackKind::Audio);
        t1.gain_db = -6.0;
        t1.effects.push(EffectInstance::new(EffectKind::Equalizer));
        let mut c1 = clip("c1", "a1", TrackKind::Audio, "A", 0.0, 4.0);
        c1.gain_db = 3.0;
        // Spur mit Automation (ohne FX).
        let mut t2 = track("a2", TrackKind::Audio);
        t2.volume_auto.upsert_key(0.0, 0.0);
        t2.volume_auto.upsert_key(2.0, 6.0);
        let c2 = clip("c2", "a2", TrackKind::Audio, "A", 0.0, 4.0);
        // Einfache Spur.
        let t3 = track("a3", TrackKind::Audio);
        let c3 = clip("c3", "a3", TrackKind::Audio, "A", 0.0, 4.0);
        let (tl, media) = state_with(
            vec![t1, t2, t3],
            vec![c1, c2, c3],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        // Eine einfache Spur → Schnellpfad; zwei verarbeitete → Bus-Pfad.
        assert_eq!(plan.audio.len(), 1, "nur die einfache Spur im Schnellpfad");
        assert_eq!(plan.audio_tracks.len(), 2);
        // Verarbeitete Clip-Gains tragen NUR den Clip-Anteil (kein Master/
        // Spur/Pan — das folgt in der Bus-Verarbeitung).
        let bus = plan
            .audio_tracks
            .iter()
            .find(|t| !t.effects.is_empty())
            .expect("Bus-FX-Spur");
        let expect = db_to_linear(3.0);
        assert!((bus.clips[0].gain_l - expect).abs() < 1e-6, "nur Clip-Gain");
        assert!((bus.clips[0].gain_r - expect).abs() < 1e-6);
        assert_eq!(bus.gain_db, -6.0);
    }

    #[test]
    fn processed_track_gain_matches_player_semantics() {
        // AudioTrackPlan (Export) und TimelineTrack (Player) werten Spur-Gain/
        // Pan inkl. Automation identisch aus — gemeinsame Mathematik, damit
        // Wiedergabe und Export gleich klingen.
        let mut t = track("a1", TrackKind::Audio);
        t.gain_db = -4.0;
        t.pan = 0.2;
        t.volume_auto.upsert_key(0.0, 0.0);
        t.volume_auto.upsert_key(4.0, 8.0);
        t.pan_auto.upsert_key(0.0, 0.0);
        t.pan_auto.upsert_key(4.0, 0.5);
        let plan = AudioTrackPlan {
            clips: vec![],
            effects: vec![],
            volume_auto: t.volume_auto.clone(),
            pan_auto: t.pan_auto.clone(),
            gain_db: t.gain_db,
            pan: t.pan,
            master_db: 0.0,
            seq_start: 3.0,
        };
        for mix_t in [0.0, 1.0, 2.5, 4.0] {
            let seq_t = 3.0 + mix_t;
            assert!((plan.gain_db_at(mix_t) - t.gain_db_at(seq_t)).abs() < 1e-9);
            assert!((plan.pan_at(mix_t) - t.pan_at(seq_t)).abs() < 1e-9);
        }
    }

    #[test]
    fn plan_extends_layers_across_transition_and_disables_fast_path() {
        // A: 0–4 (src_in 1 von 10 s), B: 4–8 (src_in 2 von 10 s) auf V1;
        // Überblendung zentriert 2 s ⇒ Fenster 3..5: dort laufen BEIDE Layer.
        let (mut tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![
                {
                    let mut c = clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0);
                    c.src_in = 1.0;
                    c.src_duration = 10.0;
                    c
                },
                {
                    let mut c = clip("b", "v1", TrackKind::Video, "B", 4.0, 4.0);
                    c.src_in = 2.0;
                    c.src_duration = 10.0;
                    c
                },
            ],
            vec![video_asset("A", "/a.mp4"), video_asset("B", "/b.mp4")],
        );
        tl.add_transition(
            crate::core::transitions::TransitionKind::CrossDissolve,
            "a",
            crate::core::timeline::TrimEdge::End,
            2.0,
        )
        .unwrap();
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        // Segmente: A allein (0..3), A+B (3..5), B allein (5..8).
        assert_eq!(plan.segments.len(), 3, "{:?}", plan.segments);
        assert_eq!(plan.segments[0].frames, 75);
        assert_eq!(plan.segments[1].frames, 50);
        assert_eq!(plan.segments[2].frames, 75);
        // Vor dem Fenster: ein Layer ohne Übergang ⇒ Schnellpfad bleibt.
        assert_eq!(plan.segments[0].layers.len(), 1);
        assert!(plan.segments[0].layers[0].transitions.is_empty());
        assert!(plan.segments[0].layers[0].is_identity());
        // Im Fenster: A (Out) unten, B (In) oben — Medienzeit über die Kante.
        let mid = &plan.segments[1].layers;
        assert_eq!(mid.len(), 2);
        assert_eq!(mid[0].path, "/a.mp4");
        assert_eq!(mid[1].path, "/b.mp4");
        assert_eq!(mid[0].transitions[0].role, TransitionRole::Out);
        assert_eq!(mid[1].transitions[0].role, TransitionRole::In);
        assert!((mid[0].transitions[0].t0 - 3.0).abs() < 1e-9);
        assert!((mid[0].transitions[0].t1 - 5.0).abs() < 1e-9);
        // B beginnt im Fenster eine Sekunde VOR seinem In-Punkt (Kopf-Handle).
        assert!((mid[1].src_in - 1.0).abs() < 1e-6, "src_in = {}", mid[1].src_in);
        assert!(!mid[0].is_identity() && !mid[1].is_identity());
        // Übergangs-Auswertung: Mitte des Fensters ⇒ B halbtransparent.
        let fx = eval_plan_transitions(&mid[1].transitions, 4.0);
        assert!((fx.opacity - 0.5).abs() < 1e-9);
        // Nach dem Fenster: B allein, nahtlose Medienzeit.
        assert_eq!(plan.segments[2].layers.len(), 1);
        assert!((plan.segments[2].layers[0].src_in - 3.0).abs() < 1e-6);
        assert!(plan.segments[2].layers[0].transitions.is_empty());
    }

    #[test]
    fn plan_dip_adds_solid_layer() {
        let (mut tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![
                {
                    let mut c = clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0);
                    c.src_in = 1.0;
                    c.src_duration = 10.0;
                    c
                },
                {
                    let mut c = clip("b", "v1", TrackKind::Video, "B", 4.0, 4.0);
                    c.src_in = 2.0;
                    c.src_duration = 10.0;
                    c
                },
            ],
            vec![video_asset("A", "/a.mp4"), video_asset("B", "/b.mp4")],
        );
        tl.add_transition(
            crate::core::transitions::TransitionKind::DipToWhite,
            "a",
            crate::core::timeline::TrimEdge::End,
            2.0,
        )
        .unwrap();
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        let mid = &plan.segments[1].layers;
        assert_eq!(mid.len(), 3, "A + B + Farbfläche");
        assert_eq!(mid[2].solid, Some([255, 255, 255]));
        assert_eq!(mid[2].transitions[0].role, TransitionRole::Dip);
        // Farbfläche voll deckend exakt am Schnitt (p = 0,5).
        let fx = eval_plan_transitions(&mid[2].transitions, 4.0);
        assert!((fx.opacity - 1.0).abs() < 1e-9);
    }

    #[test]
    fn plan_audio_carries_crossfades() {
        let (mut tl, media) = state_with(
            vec![track("a1", TrackKind::Audio)],
            vec![
                {
                    let mut c = clip("a", "a1", TrackKind::Audio, "A", 0.0, 4.0);
                    c.src_in = 1.0;
                    c.src_duration = 10.0;
                    c
                },
                {
                    let mut c = clip("b", "a1", TrackKind::Audio, "B", 4.0, 4.0);
                    c.src_in = 2.0;
                    c.src_duration = 10.0;
                    c
                },
            ],
            vec![video_asset("A", "/a.mp4"), video_asset("B", "/b.mp4")],
        );
        tl.add_transition(
            crate::core::transitions::TransitionKind::ConstantPower,
            "a",
            crate::core::timeline::TrimEdge::End,
            2.0,
        )
        .unwrap();
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert_eq!(plan.audio.len(), 2);
        let a = &plan.audio[0];
        let b = &plan.audio[1];
        // A spielt bis 5 s (1 s über die Kante), B ab 3 s (1 s davor).
        assert!((a.duration - 5.0).abs() < 1e-9);
        assert!((b.start_in_mix - 3.0).abs() < 1e-9);
        assert!((b.src_in - 1.0).abs() < 1e-9, "Kopf-Handle: {}", b.src_in);
        assert_eq!(a.fades, vec![PlanAudioFade { t0: 3.0, t1: 5.0, fade_in: false, equal_power: true }]);
        assert_eq!(b.fades, vec![PlanAudioFade { t0: 3.0, t1: 5.0, fade_in: true, equal_power: true }]);
        // Hüllkurven: konstante Leistung in der Fenstermitte.
        let g_out = a.fades[0].gain_at(4.0);
        let g_in = b.fades[0].gain_at(4.0);
        assert!((g_out * g_out + g_in * g_in - 1.0).abs() < 1e-9);
    }

    #[test]
    fn plan_inserts_black_for_gaps_and_respects_mute() {
        let mut t = track("v1", TrackKind::Video);
        t.muted = false;
        let (mut tl, media) = state_with(
            vec![t],
            vec![clip("a", "v1", TrackKind::Video, "A", 2.0, 2.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert_eq!(plan.segments.len(), 2); // Schwarz 0..2, dann Clip
        assert!(plan.segments[0].layers.is_empty());
        assert_eq!(plan.segments[0].frames, 50);

        tl.tracks[0].muted = true;
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert!(!plan.has_video_media(), "gemutete Spur darf nicht rendern");
    }

    /// Timeline mit einem Video-Clip und einer Untertitel-Spur (2 Segmente).
    fn state_with_subtitles() -> (TimelineStore, MediaStore) {
        let (mut tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("a", "v1", TrackKind::Video, "A", 0.0, 8.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        tl.import_subtitle_cues(&[
            crate::core::subtitle::SrtCue { start: 1.0, end: 3.0, text: "Erster Satz".into() },
            crate::core::subtitle::SrtCue { start: 4.0, end: 6.0, text: "Zweiter Satz".into() },
        ]);
        (tl, media)
    }

    #[test]
    fn plan_burns_subtitles_as_top_title_layers() {
        let (tl, media) = state_with_subtitles();
        let mut settings = test_settings();

        // Ohne Einbrennen: Untertitel tauchen in keinem Segment auf.
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert!(plan
            .segments
            .iter()
            .all(|s| s.layers.iter().all(|l| l.title.is_none())));
        assert!(plan.subtitle_tracks.is_empty());

        settings.subtitles = SubtitleMode::BurnIn;
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        // Segment bei t=2 (Frame 50): Video unten, Untertitel-Titel oben.
        let seg_at = |f: u64| -> &VideoSegment {
            let mut cursor = 0u64;
            for s in &plan.segments {
                if f < cursor + s.frames {
                    return s;
                }
                cursor += s.frames;
            }
            plan.segments.last().unwrap()
        };
        let seg = seg_at(50);
        assert_eq!(seg.layers.len(), 2);
        assert!(seg.layers[0].title.is_none(), "Video unten");
        let spec = seg.layers[1].title.as_ref().expect("Untertitel oben");
        assert_eq!(spec.text, "Erster Satz");
        // Zwischen den Segmenten (t=3,5): nur das Video.
        let seg = seg_at(87);
        assert_eq!(seg.layers.len(), 1);
        // Einbrennen erzwingt den Compositing-Pfad (kein Schnellpfad).
        assert!(!seg_at(50).layers[1].is_identity());

        // Ausgeblendete Spur (Auge zu) wird nicht eingebrannt.
        let mut tl = tl;
        let sub_track = tl.subtitle_tracks()[0].id.clone();
        if let Some(t) = tl.tracks.iter_mut().find(|t| t.id == sub_track) {
            t.muted = true;
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert!(plan
            .segments
            .iter()
            .all(|s| s.layers.iter().all(|l| l.title.is_none())));
    }

    #[test]
    fn plan_collects_subtitle_tracks_for_sidecar_clipped_to_range() {
        let (mut tl, media) = state_with_subtitles();
        tl.in_point = Some(2.0);
        tl.out_point = Some(5.0);
        let mut settings = test_settings();
        settings.subtitles = SubtitleMode::Sidecar;
        settings.use_in_out = true;
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.subtitle_tracks.len(), 1);
        let track = &plan.subtitle_tracks[0];
        assert_eq!(track.name, "U1");
        // Beide Cues berühren den Bereich; Zeiten relativ zum Exportbeginn
        // und auf den Bereich beschnitten.
        assert_eq!(track.cues.len(), 2);
        assert!((track.cues[0].start - 0.0).abs() < 1e-9);
        assert!((track.cues[0].end - 1.0).abs() < 1e-9);
        assert!((track.cues[1].start - 2.0).abs() < 1e-9);
        assert!((track.cues[1].end - 3.0).abs() < 1e-9);
        assert_eq!(track.cues[0].text, "Erster Satz");
        // Sidecar-Pfade: einspurig <ziel>.srt, mehrspurig mit Spurname.
        assert_eq!(
            sidecar_srt_path("/tmp/film.mp4", "U1", true),
            std::path::PathBuf::from("/tmp/film.srt")
        );
        assert_eq!(
            sidecar_srt_path("/tmp/film.mp4", "U2", false),
            std::path::PathBuf::from("/tmp/film.U2.srt")
        );
    }

    #[test]
    fn validate_checks_subtitle_modes() {
        let (tl, media) = state_with_subtitles();
        // Einbetten in einen Container ohne Untertitel-Streams → Fehler.
        let mut settings = test_settings();
        settings.container = container("wav");
        settings.video = None;
        settings.audio = Some(default_audio("pcm16", None));
        settings.output = "/tmp/out.wav".into();
        settings.subtitles = SubtitleMode::Embed;
        let issues = validate(&tl, &media, Some(true), None, &settings, &NoNests);
        assert!(
            issues
                .iter()
                .any(|i| i.severity == Severity::Error && i.message.contains("unterstützt keine")),
            "{issues:?}"
        );
        // Einbrennen ohne Video-Export → Fehler.
        let mut settings = test_settings();
        settings.video = None;
        settings.subtitles = SubtitleMode::BurnIn;
        let issues = validate(&tl, &media, Some(true), None, &settings, &NoNests);
        assert!(issues
            .iter()
            .any(|i| i.severity == Severity::Error && i.message.contains("erfordert einen Video-Export")));
        // Untertitel-Option ohne sichtbare Untertitel → Warnung.
        let (tl_empty, media_empty) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        let mut settings = test_settings();
        settings.subtitles = SubtitleMode::Sidecar;
        let issues = validate(&tl_empty, &media_empty, Some(true), None, &settings, &NoNests);
        assert!(issues
            .iter()
            .any(|i| i.severity == Severity::Warning && i.message.contains("Keine sichtbaren Untertitel")));
        // MP4 + Einbetten mit vorhandenen Untertiteln: kein Untertitel-Fehler.
        let mut settings = test_settings();
        settings.subtitles = SubtitleMode::Embed;
        let issues = validate(&tl, &media, Some(true), None, &settings, &NoNests);
        assert!(!issues
            .iter()
            .any(|i| i.severity == Severity::Error && i.message.contains("Untertitel")));
    }

    #[test]
    fn plan_audio_clips_carry_gains_and_range() {
        let mut at = track("a1", TrackKind::Audio);
        at.gain_db = -6.0;
        at.pan = -1.0; // hart links
        let mut c = clip("a", "a1", TrackKind::Audio, "A", 1.0, 4.0);
        c.gain_db = -6.0;
        let (mut tl, media) = state_with(vec![at], vec![c], vec![video_asset("A", "/a.mp4")]);
        tl.master_gain_db = -6.0;
        tl.in_point = Some(2.0);
        tl.out_point = Some(4.0);
        let mut settings = test_settings();
        settings.use_in_out = true;
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.audio.len(), 1);
        let a = &plan.audio[0];
        assert!((a.start_in_mix - 0.0).abs() < 1e-9);
        assert!((a.duration - 2.0).abs() < 1e-9);
        assert!((a.src_in - 1.0).abs() < 1e-9);
        // −18 dB gesamt ≈ 0.1259; rechts durch Pan stumm.
        assert!((a.gain_l - 0.1259).abs() < 0.001, "gain_l = {}", a.gain_l);
        assert!(a.gain_r.abs() < 1e-6);
    }

    #[test]
    fn validate_blocks_empty_timeline_and_source_overwrite() {
        let (tl, media) = state_with(vec![track("v1", TrackKind::Video)], vec![], vec![]);
        let issues = validate(&tl, &media, Some(true), None, &test_settings(), &NoNests);
        assert!(issues.iter().any(|i| i.severity == Severity::Error));

        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("a", "v1", TrackKind::Video, "A", 0.0, 5.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        let mut settings = test_settings();
        settings.output = "/a.mp4".into();
        let issues = validate(&tl, &media, Some(true), None, &settings, &NoNests);
        assert!(
            issues.iter().any(|i| i.severity == Severity::Error && i.message.contains("Quelldatei")),
            "{issues:?}"
        );
    }

    #[test]
    fn validate_flags_missing_encoder_and_odd_resolution() {
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("a", "v1", TrackKind::Video, "A", 0.0, 5.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        let mut settings = test_settings();
        settings.video.as_mut().unwrap().width = 1281;
        let encoders: HashSet<String> = ["aac".to_string()].into();
        let issues = validate(&tl, &media, Some(true), Some(&encoders), &settings, &NoNests);
        assert!(issues.iter().any(|i| i.message.contains("gerade")));
        assert!(issues.iter().any(|i| i.message.contains("libx264")));
    }

    #[test]
    fn speed_setpts_and_atempo_chains() {
        assert_eq!(speed_setpts_filter(1.0), "");
        assert_eq!(speed_setpts_filter(0.0), "");
        assert_eq!(speed_setpts_filter(2.0), "setpts=(PTS-STARTPTS)/2.000000,");
        assert!(atempo_chain(1.0).is_none());
        assert_eq!(atempo_chain(0.5).unwrap(), "atempo=0.500000");
        assert_eq!(atempo_chain(2.0).unwrap(), "atempo=2.000000");
        // Außerhalb [0,5..2,0]: kaskadiert. 4× ⇒ 2 × 2; 0,1 ⇒ 0,5 × 0,5 × 0,4.
        assert_eq!(atempo_chain(4.0).unwrap(), "atempo=2.0,atempo=2.000000");
        let chain = atempo_chain(0.1).unwrap();
        let factors: f64 = chain
            .split(',')
            .map(|p| p.trim_start_matches("atempo=").parse::<f64>().unwrap())
            .product();
        assert!((factors - 0.1).abs() < 1e-6, "{chain}");
    }

    #[test]
    fn plan_maps_constant_speed_frame_accurate() {
        // 37 % auf einem 4-s-Clip ⇒ Dauer 4 s, Medienspanne 1,48 s.
        let mut c = clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0);
        c.speed = 0.37;
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![c],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert_eq!(plan.total_frames, 100); // 4 s × 25 fps
        let layer = &plan.segments[0].layers[0];
        assert!((layer.media_step - 0.37).abs() < 1e-12);
        // Krummer Faktor erzwingt den Compositing-Pfad (setpts ≠ Identität)?
        // Nein — Vorwärts-Speed bleibt schnellpfad-fähig.
        assert!(layer.is_identity());
        // Medienzeit von Frame f = src_in + f/fps · media_step.
        let sum: u64 = plan.segments.iter().map(|s| s.frames).sum();
        assert_eq!(sum, plan.total_frames);
    }

    #[test]
    fn plan_reverse_disables_fast_path_and_audio() {
        let mut c = clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0);
        c.reverse = true;
        let mut a = clip("b", "a1", TrackKind::Audio, "A", 0.0, 4.0);
        a.reverse = true;
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video), track("a1", TrackKind::Audio)],
            vec![c, a],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        let layer = &plan.segments[0].layers[0];
        assert!((layer.media_step + 1.0).abs() < 1e-12, "rückwärts: −1");
        assert!(!layer.is_identity(), "Rückwärts erzwingt den Compositing-Pfad");
        assert!(plan.audio.is_empty(), "Rückwärts-Audio ist stumm");
    }

    #[test]
    fn plan_freeze_holds_media_time() {
        let mut c = clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0);
        c.src_in = 3.0;
        c.freeze = true;
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![c],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        // Trotz Mehrfach-Segmenten bleibt die Medienzeit am In-Punkt.
        for seg in &plan.segments {
            for l in &seg.layers {
                assert_eq!(l.media_step, 0.0);
                assert!((l.src_in - 3.0).abs() < 1e-9, "Standbild-Medienzeit: {}", l.src_in);
            }
        }
    }

    #[test]
    fn plan_audio_doubles_source_span_for_speed() {
        // 2× Tempo: 2-s-Clip zieht 4 s Quelle, pitch-korrigiert.
        let mut c = clip("a", "a1", TrackKind::Audio, "A", 0.0, 2.0);
        c.speed = 2.0;
        let (tl, media) = state_with(
            vec![track("a1", TrackKind::Audio)],
            vec![c],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert_eq!(plan.audio.len(), 1);
        assert!((plan.audio[0].duration - 2.0).abs() < 1e-9);
        assert!((plan.audio[0].speed - 2.0).abs() < 1e-9);
    }

    #[test]
    fn fps_arg_maps_ntsc_rates() {
        assert_eq!(fps_arg(23.976), "24000/1001");
        assert_eq!(fps_arg(29.97), "30000/1001");
        assert_eq!(fps_arg(59.94), "60000/1001");
        assert_eq!(fps_arg(25.0), "25");
        assert_eq!(fps_arg(60.0), "60");
        // Auch die exakten Brüche aus FRAMERATES landen auf dem Bruch-Argument.
        assert_eq!(fps_arg(30000.0 / 1001.0), "30000/1001");
        assert_eq!(fps_arg(24000.0 / 1001.0), "24000/1001");
    }

    #[test]
    fn plan_has_no_ntsc_drift_over_long_durations() {
        // 10-Stunden-Sequenz bei 29,97: exakte Rate ⇒ 1.078.921 Frames
        // (36000 s × 30000/1001). Die gerundete 29.97 ergäbe 1.078.920 —
        // ein Frame Drift, der über die Tonspur sichtbar würde.
        let mut c = clip("a", "v1", TrackKind::Video, "A", 0.0, 36000.0);
        c.src_duration = 36000.0;
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![c],
            vec![video_asset("A", "/a.mp4")],
        );
        let mut settings = test_settings();
        let ntsc = FRAMERATES.iter().find(|(l, _)| *l == "29,97").unwrap().1;
        settings.video.as_mut().unwrap().fps = ntsc;
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.total_frames, 1_078_921);
        assert_eq!((36000.0_f64 * 29.97).round() as u64, 1_078_920, "gerundet drifted");
        // Segmentgrenzen decken die Gesamtdauer lückenlos ab.
        let sum: u64 = plan.segments.iter().map(|s| s.frames).sum();
        assert_eq!(sum, plan.total_frames);
    }

    #[test]
    fn estimate_size_bitrate_and_pcm() {
        let mut settings = test_settings();
        settings.video.as_mut().unwrap().quality = VideoQuality::Bitrate(8000);
        settings.audio.as_mut().unwrap().bitrate_kbps = 192;
        // (8000 + 192) kbit/s × 10 s / 8 = 10,24 MB
        assert_eq!(estimate_size(&settings, 10.0), Some(10_240_000));
        settings.video = None;
        settings.audio = Some(default_audio("pcm16", None));
        // 48000 × 2 × 16 bit × 10 s / 8 = 1.920.000 B
        assert_eq!(estimate_size(&settings, 10.0), Some(1_920_000));
        settings.audio = Some(default_audio("flac", None));
        assert_eq!(estimate_size(&settings, 10.0), None);
    }

    #[test]
    fn encoder_args_cover_codec_specifics() {
        let v = VideoSettings {
            quality: VideoQuality::Crf(30),
            ..default_video("vp9", 1920, 1080, 25.0)
        };
        let args = video_codec_args(&v, container("webm"), OutputColor::Bt709);
        let joined = args.join(" ");
        assert!(joined.contains("-crf 30"));
        assert!(joined.contains("-b:v 0"), "VP9-CRF braucht -b:v 0: {joined}");

        let mut v = default_video("prores", 1920, 1080, 25.0);
        v.profile = 4;
        let args = video_codec_args(&v, container("mov"), OutputColor::Bt709);
        let joined = args.join(" ");
        assert!(joined.contains("-profile:v 4"));
        assert!(joined.contains("yuv444p10le"));

        let v = default_video("hevc", 1920, 1080, 25.0);
        let joined = video_codec_args(&v, container("mp4"), OutputColor::Bt709).join(" ");
        assert!(joined.contains("-tag:v hvc1"));
        // 8-Bit-Standard ⇒ yuv420p, kein main10.
        assert!(joined.contains("yuv420p") && !joined.contains("yuv420p10le"));

        // 10-Bit-Schalter ⇒ HEVC main10 + yuv420p10le.
        let mut v10 = default_video("hevc", 1920, 1080, 25.0);
        v10.tenbit = true;
        let joined = video_codec_args(&v10, container("mp4"), OutputColor::Bt709).join(" ");
        assert!(joined.contains("-profile:v main10"), "HEVC main10: {joined}");
        assert!(joined.contains("yuv420p10le"), "10-Bit-Pixelformat: {joined}");
        assert!(pipe_hi_bit(&v10), "10-Bit-Schalter ⇒ 16-Bit-Pipe");

        // H.264 High 10 + AV1 10-Bit.
        let mut h10 = default_video("h264", 1920, 1080, 25.0);
        h10.tenbit = true;
        assert!(video_codec_args(&h10, container("mp4"), OutputColor::Bt709).join(" ").contains("-profile:v high10"));
        let mut av10 = default_video("av1", 1920, 1080, 25.0);
        av10.tenbit = true;
        assert!(video_codec_args(&av10, container("mp4"), OutputColor::Bt709).join(" ").contains("yuv420p10le"));
    }

    #[test]
    fn output_color_detection_and_honest_tags() {
        let stream = |trc: &str, prim: &str, space: &str| crate::core::types::VideoStreamInfo {
            index: 0,
            codec: "x".into(),
            width: 1920,
            height: 1080,
            fps: 25.0,
            pix_fmt: None,
            bitrate: None,
            bit_depth: 10,
            color_transfer: (!trc.is_empty()).then(|| trc.into()),
            color_primaries: (!prim.is_empty()).then(|| prim.into()),
            color_space: (!space.is_empty()).then(|| space.into()),
            color_range: None,
        };
        assert_eq!(OutputColor::from_stream(&stream("", "", "")), OutputColor::Bt709);
        assert_eq!(OutputColor::from_stream(&stream("bt709", "bt709", "bt709")), OutputColor::Bt709);
        assert_eq!(
            OutputColor::from_stream(&stream("smpte2084", "bt2020", "bt2020nc")),
            OutputColor::Bt2020Pq
        );
        assert_eq!(
            OutputColor::from_stream(&stream("arib-std-b67", "bt2020", "bt2020nc")),
            OutputColor::Bt2020Hlg
        );
        assert_eq!(
            OutputColor::from_stream(&stream("bt2020-10", "bt2020", "bt2020nc")),
            OutputColor::Bt2020
        );
        // Ehrliche Tags + passende Matrix.
        assert_eq!(OutputColor::Bt709.tags(), ("bt709", "bt709", "bt709"));
        assert_eq!(OutputColor::Bt2020Pq.tags(), ("bt2020", "smpte2084", "bt2020nc"));
        assert_eq!(OutputColor::Bt709.scale_matrix(), "bt709");
        assert_eq!(OutputColor::Bt2020Pq.scale_matrix(), "bt2020nc");

        // video_codec_args trägt den erkannten Farbraum in Filter + Tags.
        let v = default_video("hevc", 1920, 1080, 25.0);
        let sdr = video_codec_args(&v, container("mp4"), OutputColor::Bt709).join(" ");
        assert!(sdr.contains("out_color_matrix=bt709") && sdr.contains("-colorspace bt709"));
        let hdr = video_codec_args(&v, container("mp4"), OutputColor::Bt2020Pq).join(" ");
        assert!(hdr.contains("out_color_matrix=bt2020nc"), "HDR-Matrix: {hdr}");
        assert!(hdr.contains("-color_trc smpte2084"), "PQ-Transfer-Tag: {hdr}");
        assert!(hdr.contains("-color_primaries bt2020"), "BT.2020-Primaries: {hdr}");
    }

    #[test]
    fn pipe_format_follows_target_bit_depth() {
        // 8-Bit-Ziele ⇒ rgba-Pipe (mit Dithering quantisiert).
        let h264 = default_video("h264", 1920, 1080, 25.0);
        assert!(!pipe_hi_bit(&h264));
        assert_eq!(pipe_pix_fmt(&h264), "rgba");
        assert_eq!(pipe_bytes_per_px(&h264), 4);

        // ProRes ist immer 10-Bit ⇒ rgba64le-Pipe (16 Bit/Kanal).
        let prores = default_video("prores", 1920, 1080, 25.0);
        assert_eq!(resolved_output_pix_fmt(&prores), "yuv422p10le");
        assert!(pipe_hi_bit(&prores));
        assert_eq!(pipe_pix_fmt(&prores), "rgba64le");
        assert_eq!(pipe_bytes_per_px(&prores), 8);

        // DNxHR: 8-Bit-Profile ⇒ rgba, 10-Bit-Profile (HQX) ⇒ rgba64le.
        let mut dnx = default_video("dnxhr", 1920, 1080, 25.0);
        dnx.profile = 0; // LB → yuv422p (8 Bit)
        assert!(!pipe_hi_bit(&dnx));
        dnx.profile = 3; // HQX → yuv422p10le
        assert!(pipe_hi_bit(&dnx), "DNxHR HQX ist 10-Bit");
    }

    #[test]
    fn export_range_uses_in_out_only_when_valid() {
        let (mut tl, _media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("a", "v1", TrackKind::Video, "A", 0.0, 10.0)],
            vec![],
        );
        assert_eq!(export_range(&tl, false), (0.0, 10.0));
        assert_eq!(export_range(&tl, true), (0.0, 10.0)); // kein In/Out gesetzt
        tl.in_point = Some(2.0);
        tl.out_point = Some(6.0);
        assert_eq!(export_range(&tl, true), (2.0, 6.0));
        tl.out_point = Some(2.0); // leerer Bereich → Fallback ganze Sequenz
        assert_eq!(export_range(&tl, true), (0.0, 10.0));
    }

    /// End-to-End: echte Medien erzeugen, Sequenz rendern, Ergebnis proben.
    /// Timeline: Video (0–2 s, src_in 0,5), Bild (2–3 s), Lücke (3–4 s),
    /// Audio über die volle Länge.
    #[test]
    fn end_to_end_export_renders_playable_file() {
        let dir = std::env::temp_dir().join(format!("editron-export-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_video = dir.join("quelle.mp4");
        let src_image = dir.join("bild.png");
        let out = dir.join("ergebnis.mp4");

        // Testquellen generieren (testsrc + Sinuston, rotes Standbild).
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "testsrc=duration=4:size=640x360:rate=25"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=4"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-shortest"])
            .arg(&src_video)
            .status()
            .expect("ffmpeg nicht startbar — Tests brauchen ffmpeg im PATH");
        assert!(gen.success(), "Testvideo konnte nicht erzeugt werden");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "color=red:size=320x240", "-frames:v", "1"])
            .arg(&src_image)
            .status()
            .unwrap();
        assert!(gen.success(), "Testbild konnte nicht erzeugt werden");

        let mut image_asset = video_asset("IMG", &src_image.to_string_lossy());
        image_asset.kind = MediaKind::Image;
        image_asset.info.video.clear();
        image_asset.info.audio.clear();
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video), track("a1", TrackKind::Audio)],
            vec![
                {
                    let mut c = clip("v", "v1", TrackKind::Video, "VID", 0.0, 2.0);
                    c.src_in = 0.5;
                    c
                },
                {
                    let mut c = clip("img", "v1", TrackKind::Video, "IMG", 2.0, 1.0);
                    c.src_duration = f64::INFINITY;
                    c
                },
                clip("a", "a1", TrackKind::Audio, "VID", 0.0, 4.0),
            ],
            vec![video_asset("VID", &src_video.to_string_lossy()), image_asset],
        );

        let mut settings = test_settings();
        settings.output = out.to_string_lossy().into_owned();
        if let Some(v) = settings.video.as_mut() {
            v.width = 640;
            v.height = 360;
            v.speed = 0; // ultrafast
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.total_frames, 100);
        assert_eq!(plan.segments.len(), 3); // Video, Bild, Schwarz

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let children = Arc::new(Mutex::new(Vec::new()));
        run_export_worker(
            "test-job".into(),
            plan,
            settings,
            tx,
            cancel,
            children,
        );

        let mut done: Option<(bool, Option<String>)> = None;
        let mut saw_progress = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                ServiceEvent::SequenceExportProgress { eta_sec: _, .. } => saw_progress = true,
                ServiceEvent::SequenceExportDone { ok, error, .. } => done = Some((ok, error)),
                _ => {}
            }
        }
        let (ok, error) = done.expect("Done-Event fehlt");
        assert!(ok, "Export fehlgeschlagen: {error:?}");
        assert!(saw_progress, "keine Progress-Events");
        assert!(out.exists(), "Zieldatei fehlt");
        assert!(!part_path(&out.to_string_lossy()).exists(), ".part nicht aufgeräumt");

        // Ergebnis proben: Dauer ≈ 4 s, 640×360 @ 25, Video + Audio vorhanden.
        let info = crate::services::probe_media(&out.to_string_lossy()).expect("probe");
        assert!((info.duration_sec - 4.0).abs() < 0.25, "Dauer: {}", info.duration_sec);
        assert_eq!(info.video.len(), 1);
        assert_eq!(info.audio.len(), 1);
        assert_eq!((info.video[0].width, info.video[0].height), (640, 360));
        assert!((info.video[0].fps - 25.0).abs() < 0.01);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-End: eine verschachtelte Sequenz (Nest-Clip) wird rekursiv
    /// komponiert. Das innere Material ist grün → der Ausgabe-Mittelpixel muss
    /// grün sein (nicht schwarz), und die Datei abspielbar mit Zielraster.
    #[test]
    fn end_to_end_export_renders_nested_sequence() {
        use crate::core::compose::NestResolver;
        let dir = std::env::temp_dir().join(format!("editron-export-nest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_video = dir.join("gruen.mp4");
        let out = dir.join("nest.mp4");

        // Grünes Vollbild-Testvideo (füllt das Zielraster ohne Letterbox).
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "color=0x00B000:size=640x360:rate=25:duration=2"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
            .arg(&src_video)
            .status()
            .expect("ffmpeg nicht startbar — Tests brauchen ffmpeg im PATH");
        assert!(gen.success(), "Testvideo konnte nicht erzeugt werden");

        // Innere Sequenz (640×360): grüner Clip 0..2.
        let (mut inner, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("g", "v1", TrackKind::Video, "GREEN", 0.0, 2.0)],
            vec![video_asset("GREEN", &src_video.to_string_lossy())],
        );
        inner.settings.width = 640;
        inner.settings.height = 360;
        // Äußere Sequenz: ein Nest-Clip 0..2 (volle Deckkraft, Identität).
        let (mut outer, _) = state_with(
            vec![track("ov", TrackKind::Video)],
            vec![clip("n", "ov", TrackKind::Video, "GREEN", 0.0, 2.0)],
            vec![video_asset("GREEN", &src_video.to_string_lossy())],
        );
        outer.clips[0].asset_id = String::new();
        outer.clips[0].nest_seq = Some("inner".into());

        struct R<'a>(HashMap<String, &'a TimelineStore>);
        impl NestResolver for R<'_> {
            fn nested_timeline(&self, id: &str) -> Option<&TimelineStore> {
                self.0.get(id).copied()
            }
        }
        let resolver = R(HashMap::from([("inner".to_string(), &inner)]));

        let mut settings = test_settings();
        settings.output = out.to_string_lossy().into_owned();
        settings.audio = None; // Video-only (Nest-Audio ist hier nicht Gegenstand)
        if let Some(v) = settings.video.as_mut() {
            v.width = 640;
            v.height = 360;
            v.fps = 25.0;
            v.speed = 0;
        }
        let plan = build_render_plan(&outer, &media, &settings, &resolver);
        // Nest-Ebene im Plan + Kontext eingesammelt.
        assert!(plan
            .segments
            .iter()
            .flat_map(|s| s.layers.iter())
            .any(|l| l.nest_seq.as_deref() == Some("inner")));
        assert!(plan.nests.contains_key("inner"));

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let children = Arc::new(Mutex::new(Vec::new()));
        run_export_worker("nest-job".into(), plan, settings, tx, cancel, children);

        let mut done: Option<(bool, Option<String>)> = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok, error, .. } = ev {
                done = Some((ok, error));
            }
        }
        let (ok, error) = done.expect("Done-Event fehlt");
        assert!(ok, "Nest-Export fehlgeschlagen: {error:?}");
        assert!(out.exists(), "Zieldatei fehlt");

        // Mittelframe extrahieren und Mittelpixel prüfen (grün, nicht schwarz).
        let probe = Command::new(crate::services::ffmpeg_bin())
            .args(["-v", "error", "-ss", "1", "-i"])
            .arg(&out)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
            .output()
            .expect("ffmpeg-Probe");
        let data = probe.stdout;
        assert_eq!(data.len(), 640 * 360 * 4, "ein RGBA-Frame erwartet");
        let center = (180 * 640 + 320) * 4;
        let (r, g, b) = (data[center], data[center + 1], data[center + 2]);
        assert!(g > 120 && r < 100 && b < 100, "Mittelpixel grün erwartet, war ({r},{g},{b})");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-End: PNG-Sequenz-Export schreibt nummerierte Frames an den
    /// Zielort und räumt das temporäre Verzeichnis auf (atomar).
    #[test]
    fn end_to_end_export_renders_png_sequence() {
        let dir = std::env::temp_dir().join(format!("editron-export-seq-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_video = dir.join("quelle.mp4");
        let out = dir.join("frame.png");

        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "testsrc=duration=1:size=320x180:rate=25"])
            .args(["-c:v", "libx264", "-preset", "ultrafast"])
            .arg(&src_video)
            .status()
            .expect("ffmpeg nicht startbar");
        assert!(gen.success());

        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("v", "v1", TrackKind::Video, "VID", 0.0, 0.4)],
            vec![video_asset("VID", &src_video.to_string_lossy())],
        );

        let mut settings = ExportSettings {
            container: container("png_seq"),
            video: Some(default_video("png", 320, 180, 25.0)),
            audio: None,
            use_in_out: false,
            subtitles: SubtitleMode::None,
            image_start: 1,
            output: out.to_string_lossy().into_owned(),
        };
        if let Some(v) = settings.video.as_mut() {
            v.width = 320;
            v.height = 180;
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.total_frames, 10); // 0,4 s @ 25 fps

        let (tx, rx) = std::sync::mpsc::channel();
        run_export_worker(
            "seq-job".into(),
            plan,
            settings,
            tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let mut done: Option<(bool, Option<String>)> = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok, error, .. } = ev {
                done = Some((ok, error));
            }
        }
        let (ok, error) = done.expect("Done-Event fehlt");
        assert!(ok, "Sequenz-Export fehlgeschlagen: {error:?}");

        // Nummerierte Frames liegen am Ziel, das Temp-Verzeichnis ist weg.
        let frames: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.starts_with("frame_") && n.ends_with(".png"))
            .collect();
        assert_eq!(frames.len(), 10, "10 Frames erwartet: {frames:?}");
        assert!(frames.contains(&"frame_000001.png".to_string()));
        let tmp = dir.join(".editron-seq-seq-job");
        assert!(!tmp.exists(), "Temp-Sequenz-Verzeichnis nicht aufgeräumt");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-End: Geschwindigkeit (50 %), Rückwärts und Standbild real
    /// rendern und proben — alle drei Pfade (Schnellpfad-setpts,
    /// Reverse-Chunk, Freeze-Halten) laufen durch.
    #[test]
    fn end_to_end_export_renders_speed_reverse_freeze() {
        let dir = std::env::temp_dir().join(format!("editron-export-speed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_video = dir.join("quelle.mp4");
        // 8-s-Quelle, damit 50 % (Spanne 4 s) und die Reverse-Handles passen.
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "testsrc=duration=8:size=320x240:rate=25"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=8"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-shortest"])
            .arg(&src_video)
            .status()
            .expect("ffmpeg nicht startbar");
        assert!(gen.success());

        for (name, speed, reverse, freeze) in [
            ("slow.mp4", 0.5, false, false),
            ("rev.mp4", 1.0, true, false),
            ("freeze.mp4", 1.0, false, true),
        ] {
            let out = dir.join(name);
            let mut v = clip("v", "v1", TrackKind::Video, "VID", 0.0, 2.0);
            v.src_in = 2.0;
            v.src_duration = 8.0;
            v.speed = speed;
            v.reverse = reverse;
            v.freeze = freeze;
            let (tl, media) = state_with(
                vec![track("v1", TrackKind::Video)],
                vec![v],
                vec![video_asset("VID", &src_video.to_string_lossy())],
            );
            let mut settings = test_settings();
            settings.audio = None; // Rückwärts/Standbild sind stumm
            settings.output = out.to_string_lossy().into_owned();
            if let Some(vs) = settings.video.as_mut() {
                vs.width = 320;
                vs.height = 240;
                vs.speed = 0;
            }
            let plan = build_render_plan(&tl, &media, &settings, &NoNests);
            assert_eq!(plan.total_frames, 50, "{name}");

            let (tx, rx) = std::sync::mpsc::channel();
            let cancel = Arc::new(AtomicBool::new(false));
            let children = Arc::new(Mutex::new(Vec::new()));
            run_export_worker(format!("speed-{name}"), plan, settings, tx, cancel, children);
            let mut done = None;
            while let Ok(ev) = rx.try_recv() {
                if let ServiceEvent::SequenceExportDone { ok, error, .. } = ev {
                    done = Some((ok, error));
                }
            }
            let (ok, error) = done.expect("Done-Event");
            assert!(ok, "{name} fehlgeschlagen: {error:?}");
            let info = crate::services::probe_media(&out.to_string_lossy()).expect("probe");
            assert!((info.duration_sec - 2.0).abs() < 0.25, "{name} Dauer: {}", info.duration_sec);
            assert_eq!((info.video[0].width, info.video[0].height), (320, 240), "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Format-Matrix: ProRes/MOV (Profil-Codec + PCM), VP9/WebM (CRF-Sonderfall
    /// + Opus) und Audio-only M4A (ipod-Muxer + faststart) real rendern.
    #[test]
    fn format_matrix_renders_prores_webm_and_audio_only() {
        let dir = std::env::temp_dir().join(format!("editron-export-matrix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("quelle.mp4");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "testsrc=duration=2:size=320x180:rate=25"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-shortest"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(gen.success());

        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video), track("a1", TrackKind::Audio)],
            vec![
                clip("v", "v1", TrackKind::Video, "VID", 0.0, 2.0),
                clip("a", "a1", TrackKind::Audio, "VID", 0.0, 2.0),
            ],
            vec![video_asset("VID", &src.to_string_lossy())],
        );

        let mut prores = default_video("prores", 320, 180, 25.0);
        prores.profile = 3;
        let mut vp9 = default_video("vp9", 320, 180, 25.0);
        vp9.quality = VideoQuality::Crf(40);
        let cases: Vec<(&str, ExportSettings)> = vec![
            (
                "prores.mov",
                ExportSettings {
                    container: container("mov"),
                    video: Some(prores),
                    audio: Some(default_audio("pcm24", None)),
                    use_in_out: false,
                    subtitles: SubtitleMode::None,
                    image_start: 1,
                    output: String::new(),
                },
            ),
            (
                "vp9.webm",
                ExportSettings {
                    container: container("webm"),
                    video: Some(vp9),
                    audio: Some(default_audio("opus", Some(96))),
                    use_in_out: false,
                    subtitles: SubtitleMode::None,
                    image_start: 1,
                    output: String::new(),
                },
            ),
            (
                "audio.m4a",
                ExportSettings {
                    container: container("m4a"),
                    video: None,
                    audio: Some(default_audio("aac", Some(128))),
                    use_in_out: false,
                    subtitles: SubtitleMode::None,
                    image_start: 1,
                    output: String::new(),
                },
            ),
        ];

        for (name, mut settings) in cases {
            let out = dir.join(name);
            settings.output = out.to_string_lossy().into_owned();
            let plan = build_render_plan(&tl, &media, &settings, &NoNests);
            let (tx, rx) = std::sync::mpsc::channel();
            run_export_worker(
                format!("matrix-{name}"),
                plan,
                settings.clone(),
                tx,
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(Vec::new())),
            );
            let mut ok = false;
            let mut error = None;
            while let Ok(ev) = rx.try_recv() {
                if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                    ok = o;
                    error = e;
                }
            }
            assert!(ok, "{name}: {error:?}");
            let info = crate::services::probe_media(&out.to_string_lossy()).expect(name);
            assert!((info.duration_sec - 2.0).abs() < 0.3, "{name}: Dauer {}", info.duration_sec);
            match name {
                "prores.mov" => {
                    assert_eq!(info.video[0].codec, "prores", "{name}");
                    // ProRes 422 HQ ist echtes 10-Bit (yuv422p10le) — der
                    // Compositor speist über die 16-Bit-rgba64le-Pipe ein.
                    let pf = info.video[0].pix_fmt.as_deref().unwrap_or("");
                    assert!(pf.contains("10"), "{name}: 10-Bit-Ausgabe erwartet, war {pf}");
                    assert_eq!(info.audio[0].codec, "pcm_s24le", "{name}");
                }
                "vp9.webm" => {
                    assert_eq!(info.video[0].codec, "vp9", "{name}");
                    assert_eq!(info.audio[0].codec, "opus", "{name}");
                    assert_eq!(info.audio[0].sample_rate, 48000, "{name}");
                }
                "audio.m4a" => {
                    assert!(info.video.is_empty(), "{name}: darf kein Video haben");
                    assert_eq!(info.audio[0].codec, "aac", "{name}");
                }
                _ => unreachable!(),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Einen Frame des Exports als RGB24 dekodieren (Pixel-Verifikation).
    fn decode_frame_rgb(path: &std::path::Path, at: f64, w: usize, h: usize) -> Vec<u8> {
        let out = Command::new(crate::services::ffmpeg_bin())
            .args(["-v", "error", "-ss", &format!("{at:.3}")])
            .args(["-i", &path.to_string_lossy()])
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
            .output()
            .expect("ffmpeg decode");
        assert_eq!(out.stdout.len(), w * h * 3, "Framegröße");
        out.stdout
    }

    fn rgb_at(frame: &[u8], w: usize, x: usize, y: usize) -> [u8; 3] {
        let i = (y * w + x) * 3;
        [frame[i], frame[i + 1], frame[i + 2]]
    }

    /// End-to-End mit Keyframes: ein rotes Bild wandert per Position-X-
    /// Animation von links nach rechts; der Export muss das Compositing
    /// (Skalierung 50 %, animierte Position) pixelgenau wiedergeben.
    #[test]
    fn end_to_end_export_renders_animated_transform() {
        let dir = std::env::temp_dir().join(format!("editron-export-anim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_image = dir.join("rot.png");
        let out = dir.join("anim.mp4");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "color=red:size=320x240", "-frames:v", "1"])
            .arg(&src_image)
            .status()
            .unwrap();
        assert!(gen.success());

        let mut image_asset = video_asset("IMG", &src_image.to_string_lossy());
        image_asset.kind = MediaKind::Image;
        image_asset.info.video.clear();
        image_asset.info.audio.clear();
        let mut c = clip("img", "v1", TrackKind::Video, "IMG", 0.0, 2.0);
        c.src_duration = f64::INFINITY;
        // Skalierung fest 50 %, Position X animiert −25 % → +25 % (linear).
        c.fx.scale_x.value = 50.0;
        c.fx.pos_x.upsert_key(0.0, -25.0);
        c.fx.pos_x.upsert_key(2.0, 25.0);
        let (tl, media) = state_with(vec![track("v1", TrackKind::Video)], vec![c], vec![image_asset]);

        let mut settings = test_settings();
        settings.audio = None;
        settings.output = out.to_string_lossy().into_owned();
        if let Some(v) = settings.video.as_mut() {
            v.width = 640;
            v.height = 360;
            v.speed = 0;
            v.quality = VideoQuality::Crf(16);
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.segments.len(), 1);
        assert!(!plan.segments[0].layers[0].is_identity(), "muss Compositing-Pfad nehmen");

        let (tx, rx) = std::sync::mpsc::channel();
        run_export_worker(
            "anim-job".into(),
            plan,
            settings,
            tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let mut ok = false;
        let mut error = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                ok = o;
                error = e;
            }
        }
        assert!(ok, "Export fehlgeschlagen: {error:?}");

        // Bildgeometrie: 320×240 in 640×360 contain → Basis 480×360; bei
        // 50 % → 240×180, Mittelpunkt cx = 320 + pos_x % · 640.
        // t≈0,1 s: pos_x ≈ −22,5 % → cx ≈ 176; t≈1,9 s: ≈ +22,5 % → cx ≈ 464.
        let (w, h) = (640usize, 360usize);
        let early = decode_frame_rgb(&out, 0.1, w, h);
        let p = rgb_at(&early, w, 176, 180);
        assert!(p[0] > 150 && p[1] < 100 && p[2] < 100, "links muss rot sein: {p:?}");
        let p = rgb_at(&early, w, 560, 180);
        assert!(p[0] < 60 && p[1] < 60 && p[2] < 60, "rechts muss schwarz sein: {p:?}");

        let late = decode_frame_rgb(&out, 1.9, w, h);
        let p = rgb_at(&late, w, 464, 180);
        assert!(p[0] > 150 && p[1] < 100 && p[2] < 100, "rechts muss rot sein: {p:?}");
        let p = rgb_at(&late, w, 80, 180);
        assert!(p[0] < 60 && p[1] < 60 && p[2] < 60, "links muss schwarz sein: {p:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Farbkorrektur muss im CPU-Renderpfad ankommen: Sättigung 0 macht
    /// ein rotes Bild grau, Vignette dunkelt die Inhaltsecken ab.
    #[test]
    fn end_to_end_export_renders_effects() {
        let dir = std::env::temp_dir().join(format!("editron-export-fx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_image = dir.join("rot.png");
        let out = dir.join("fx.mp4");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "color=red:size=320x240", "-frames:v", "1"])
            .arg(&src_image)
            .status()
            .unwrap();
        assert!(gen.success());

        let mut image_asset = video_asset("IMG", &src_image.to_string_lossy());
        image_asset.kind = MediaKind::Image;
        image_asset.info.video[0].width = 320;
        image_asset.info.video[0].height = 240;
        image_asset.info.audio.clear();
        let mut c = clip("img", "v1", TrackKind::Video, "IMG", 0.0, 2.0);
        c.src_duration = f64::INFINITY;
        // Negativ: Rot → Cyan, animiert hier nicht — deterministisch prüfbar.
        c.effects.push(crate::core::effects::EffectInstance::new(
            crate::core::effects::EffectKind::Invert,
        ));
        // Zuschneiden: linke 25 % transparent → schwarzer Hintergrund.
        let mut crop = crate::core::effects::EffectInstance::new(
            crate::core::effects::EffectKind::Crop,
        );
        crop.params[0] = AnimatedParam::fixed(25.0);
        c.effects.push(crop);
        let (tl, media) = state_with(vec![track("v1", TrackKind::Video)], vec![c], vec![image_asset]);

        let mut settings = test_settings();
        settings.audio = None;
        settings.output = out.to_string_lossy().into_owned();
        if let Some(v) = settings.video.as_mut() {
            v.width = 640;
            v.height = 360;
            v.speed = 0;
            v.quality = VideoQuality::Crf(16);
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert!(
            !plan.segments[0].layers[0].is_identity(),
            "aktive Effekte müssen den Compositing-Pfad erzwingen"
        );

        let (tx, rx) = std::sync::mpsc::channel();
        run_export_worker(
            "fx-job".into(),
            plan,
            settings,
            tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let mut ok = false;
        let mut error = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                ok = o;
                error = e;
            }
        }
        assert!(ok, "Export fehlgeschlagen: {error:?}");

        // 320×240 in 640×360 contain → Inhalt 480×360, x 80…560.
        let (w, h) = (640usize, 360usize);
        let frame = decode_frame_rgb(&out, 0.5, w, h);
        // Mitte: invertiertes Rot = Cyan.
        let p = rgb_at(&frame, w, 320, 180);
        assert!(
            p[0] < 60 && p[1] > 200 && p[2] > 200,
            "Mitte muss Cyan sein: {p:?}"
        );
        // Linke 25 % des Inhalts (x 80…200) sind weggeschnitten → Schwarz.
        let cropped = rgb_at(&frame, w, 120, 180);
        assert!(
            cropped[0] < 30 && cropped[1] < 30 && cropped[2] < 30,
            "Crop-Bereich muss schwarz sein: {cropped:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn end_to_end_export_renders_color_grade() {
        let dir = std::env::temp_dir().join(format!("editron-export-grade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_image = dir.join("rot.png");
        let out = dir.join("grade.mp4");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "color=red:size=320x240", "-frames:v", "1"])
            .arg(&src_image)
            .status()
            .unwrap();
        assert!(gen.success());

        let mut image_asset = video_asset("IMG", &src_image.to_string_lossy());
        image_asset.kind = MediaKind::Image;
        // Natürliche Maße fürs Vignetten-Content-Rect (320×240).
        image_asset.info.video[0].width = 320;
        image_asset.info.video[0].height = 240;
        image_asset.info.audio.clear();
        let mut c = clip("img", "v1", TrackKind::Video, "IMG", 0.0, 2.0);
        c.src_duration = f64::INFINITY;
        c.grade.saturation = 0.0;
        c.grade.vignette_amount = 100.0;
        let (tl, media) = state_with(vec![track("v1", TrackKind::Video)], vec![c], vec![image_asset]);

        let mut settings = test_settings();
        settings.audio = None;
        settings.output = out.to_string_lossy().into_owned();
        if let Some(v) = settings.video.as_mut() {
            v.width = 640;
            v.height = 360;
            v.speed = 0;
            v.quality = VideoQuality::Crf(16);
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.segments.len(), 1);
        assert!(
            !plan.segments[0].layers[0].is_identity(),
            "aktive Farbkorrektur muss den Compositing-Pfad erzwingen"
        );

        let (tx, rx) = std::sync::mpsc::channel();
        run_export_worker(
            "grade-job".into(),
            plan,
            settings,
            tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let mut ok = false;
        let mut error = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                ok = o;
                error = e;
            }
        }
        assert!(ok, "Export fehlgeschlagen: {error:?}");

        // 320×240 in 640×360 contain → Inhalt 480×360, x 80…560.
        let (w, h) = (640usize, 360usize);
        let frame = decode_frame_rgb(&out, 0.5, w, h);
        // Mitte: entsättigtes Rot = Grau (Gamma-Luma von Rot ≈ 0,21 → ~54).
        let p = rgb_at(&frame, w, 320, 180);
        assert!(
            (p[0] as i32 - p[1] as i32).abs() <= 8 && (p[1] as i32 - p[2] as i32).abs() <= 8,
            "Mitte muss grau sein: {p:?}"
        );
        assert!(p[0] >= 30 && p[0] <= 90, "Graupegel plausibel: {p:?}");
        // Inhaltsecke: Vignette dunkelt deutlich ab.
        let corner = rgb_at(&frame, w, 100, 20);
        assert!(
            corner[0] < p[0].saturating_sub(20),
            "Ecke muss dunkler sein: {corner:?} vs Mitte {p:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Einen Frame des Exports als 16-Bit-RGBA (`rgba64le`) dekodieren — für
    /// die Bittiefe-Verifikation (>256 Helligkeitsstufen ⇒ echt >8 Bit).
    fn decode_frame_rgba64le(path: &std::path::Path, at: f64, w: usize, h: usize) -> Vec<u16> {
        let out = Command::new(crate::services::ffmpeg_bin())
            .args(["-v", "error", "-ss", &format!("{at:.3}")])
            .args(["-i", &path.to_string_lossy()])
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba64le", "pipe:1"])
            .output()
            .expect("ffmpeg decode rgba64le");
        assert_eq!(out.stdout.len(), w * h * 8, "Framegröße rgba64le");
        out.stdout
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect()
    }

    /// Distinct 16-Bit-R-Werte über das ganze Frame (Banding-Maß).
    fn distinct_r16(px: &[u16]) -> usize {
        px.chunks_exact(4)
            .map(|p| p[0])
            .collect::<std::collections::HashSet<_>>()
            .len()
    }

    /// STUFE-1-BEWEIS (Kernversprechen des Ziels): 10-Bit-Quellmaterial mit
    /// Grade überlebt end-to-end bis zur 10-Bit-Ausgabe mit >256 Helligkeits-
    /// stufen — in einer 8-Bit-Pipeline physikalisch unmöglich (≤256). Prüft
    /// zugleich die ffprobe-Bittiefe-Erkennung und den 16-Bit-Decode-Pfad.
    #[test]
    fn end_to_end_export_preserves_10bit_gradient() {
        let dir = std::env::temp_dir().join(format!("editron-export-10bit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("ramp10.mov");
        let out = dir.join("out10.mov");
        let (w, h) = (1024usize, 64usize);

        // Glatter 10-Bit-Graustufenverlauf (ProRes 4444, yuv444p10le) als Quelle.
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", &format!("gradients=s={w}x{h}:c0=black:c1=white:x0=0:y0=0:x1={}:y1=0:duration=1:rate=25", w - 1)])
            .args(["-frames:v", "1"])
            .args(["-c:v", "prores_ks", "-profile:v", "4", "-pix_fmt", "yuv444p10le"])
            .arg(&src)
            .status()
            .expect("ffmpeg nicht startbar");
        assert!(gen.success(), "10-Bit-Quelle konnte nicht erzeugt werden");

        // Bittiefe-Erkennung (ffprobe) — muss 10 Bit melden.
        let info = crate::services::probe_media(&src.to_string_lossy()).expect("probe");
        assert!(info.video[0].bit_depth >= 10, "Quelle als 10-Bit erkannt: {}", info.video[0].bit_depth);

        // Quelle selbst hat deutlich >256 Helligkeitsstufen (sonst ist der
        // Verlauf zu grob und der Test nichtssagend).
        let src_levels = distinct_r16(&decode_frame_rgba64le(&src, 0.0, w, h));
        assert!(src_levels > 256, "10-Bit-Quelle hat >256 Stufen: {src_levels}");

        // Asset mit echten Probe-Infos (bit_depth=10 ⇒ 16-Bit-Decode-Pfad).
        let mut asset = video_asset("VID", &src.to_string_lossy());
        asset.info = info;
        asset.info.audio.clear();

        // Clip MIT Grade ⇒ Compositing-Pfad (rgba64le-Decode → f32 → 10-Bit-Out).
        let mut c = clip("v", "v1", TrackKind::Video, "VID", 0.0, 1.0);
        c.src_duration = f64::INFINITY;
        c.grade.contrast = 12.0; // sanft, erhält den Verlauf
        let (tl, media) = state_with(vec![track("v1", TrackKind::Video)], vec![c], vec![asset]);
        assert!(
            media.asset("VID").unwrap().info.video[0].bit_depth >= 10,
            "Asset trägt 10-Bit-Info"
        );

        // ProRes 4444 (yuv444p10le) als Ziel — volle Luma-Auflösung, kein
        // Chroma-Subsampling, das den Verlauf glätten würde.
        let mut settings = test_settings();
        settings.audio = None;
        settings.container = container("mov");
        let mut prores = default_video("prores", w as u32, h as u32, 25.0);
        prores.profile = 4; // 4444
        settings.video = Some(prores);
        settings.output = out.to_string_lossy().into_owned();

        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert!(
            !plan.segments[0].layers[0].is_identity(),
            "Grade erzwingt den Compositing-Pfad (16-Bit-Decode)"
        );
        assert!(plan.segments[0].layers[0].src_bit_depth >= 10, "Plan trägt >8-Bit-Quelle");

        let (tx, rx) = std::sync::mpsc::channel();
        run_export_worker(
            "10bit-job".into(),
            plan,
            settings,
            tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let mut ok = false;
        let mut error = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                ok = o;
                error = e;
            }
        }
        assert!(ok, "10-Bit-Export fehlgeschlagen: {error:?}");

        let oinfo = crate::services::probe_media(&out.to_string_lossy()).expect("probe out");
        assert!(oinfo.video[0].bit_depth >= 10, "Ausgabe ist 10-Bit: {}", oinfo.video[0].bit_depth);

        // Kernaussage: die Ausgabe trägt >256 Helligkeitsstufen ⇒ die >8-Bit-
        // Quellpräzision hat die gesamte (f32-)Pipeline überlebt. Eine reine
        // 8-Bit-Pipeline könnte hier physikalisch nie über 256 kommen.
        let out_levels = distinct_r16(&decode_frame_rgba64le(&out, 0.0, w, h));
        assert!(
            out_levels > 256,
            "10-Bit-Verlauf überlebt end-to-end (>256 Stufen), war {out_levels}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// STUFE-4-BEWEIS: HDR-Quellmaterial (BT.2020 + PQ) wird über die ganze
    /// Kette (ffprobe → Plan → Encoder-Args) ehrlich erkannt und getaggt —
    /// nicht mehr stumm nach BT.709 fehlgetaggt. Generiert eine real
    /// PQ-getaggte Quelle und prüft Erkennung + resultierende ffmpeg-Flags.
    #[test]
    fn detects_and_tags_bt2020_pq_source() {
        let dir = std::env::temp_dir().join(format!("editron-pq-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("pq.mp4");
        // BT.2020 + PQ, 10-Bit (libx265 main10, korrekt im Bitstream getaggt).
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "gradients=s=64x64:c0=black:c1=white:duration=1:rate=25"])
            .args(["-frames:v", "2", "-c:v", "libx265", "-preset", "ultrafast", "-pix_fmt", "yuv420p10le"])
            .args(["-x265-params", "log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:range=limited"])
            .args(["-tag:v", "hvc1"])
            .arg(&src)
            .status();
        // Ohne libx265 still überspringen (kein Hard-Fail in eingeschränkten Umgebungen).
        let Ok(st) = gen else { eprintln!("libx265 fehlt — PQ-Test übersprungen"); return };
        if !st.success() {
            eprintln!("libx265 nicht verfügbar — PQ-Test übersprungen");
            return;
        }

        // ffprobe erkennt PQ/BT.2020 + 10 Bit.
        let info = crate::services::probe_media(&src.to_string_lossy()).expect("probe");
        let v0 = &info.video[0];
        assert_eq!(v0.color_transfer.as_deref(), Some("smpte2084"), "PQ erkannt");
        assert_eq!(v0.color_primaries.as_deref(), Some("bt2020"), "BT.2020 erkannt");
        assert!(v0.bit_depth >= 10, "10-Bit erkannt: {}", v0.bit_depth);
        assert_eq!(OutputColor::from_stream(v0), OutputColor::Bt2020Pq);

        // Plan trägt den erkannten Farbraum durch.
        let mut asset = video_asset("VID", &src.to_string_lossy());
        asset.info = info;
        asset.info.audio.clear();
        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("v", "v1", TrackKind::Video, "VID", 0.0, 1.0)],
            vec![asset],
        );
        let mut settings = test_settings();
        settings.audio = None;
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.color, OutputColor::Bt2020Pq, "Plan reicht PQ durch");

        // Encoder-Args: ehrliche PQ/BT.2020-Tags statt hart bt709.
        let v = settings.video.as_ref().unwrap();
        let args = video_codec_args(v, settings.container, plan.color).join(" ");
        assert!(args.contains("-color_trc smpte2084"), "PQ-Tag im Export: {args}");
        assert!(args.contains("-color_primaries bt2020"), "BT.2020-Tag im Export: {args}");
        assert!(args.contains("out_color_matrix=bt2020nc"), "BT.2020-Matrix: {args}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// STUFE-5-MESSUNG: Export-Performance 8-Bit-Schnellpfad vs. 8-Bit-Float-
    /// Compositing vs. 10-Bit-Float-Compositing (16-Bit-Pipe). Misst Frames/s
    /// und belegt, dass (a) der 8-Bit-Schnellpfad erhalten und am schnellsten
    /// ist, (b) der 16-Bit-Pfad bewusst Bandbreite kostet. `#[ignore]` —
    /// Messung, kein Pass/Fail: `cargo test export_perf -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn export_perf_8bit_vs_16bit() {
        use std::time::Instant;
        let dir = std::env::temp_dir().join(format!("editron-perf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4");
        let (w, h, secs) = (1280u32, 720u32, 4.0f64);
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", &format!("testsrc=duration={secs}:size={w}x{h}:rate=25")])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
            .arg(&src)
            .status()
            .expect("ffmpeg");
        assert!(gen.success());

        // Eine Konfiguration real exportieren und Frames/s zurückgeben.
        let run = |label: &str, with_grade: bool, codec: &str, profile: usize| -> f64 {
            let mut c = clip("v", "v1", TrackKind::Video, "VID", 0.0, secs);
            if with_grade {
                c.grade.contrast = 30.0;
                c.grade.saturation = 120.0;
            }
            let (tl, media) = state_with(
                vec![track("v1", TrackKind::Video)],
                vec![c],
                vec![video_asset("VID", &src.to_string_lossy())],
            );
            let mut settings = test_settings();
            settings.audio = None;
            settings.container = container(if codec == "prores" { "mov" } else { "mp4" });
            let mut v = default_video(codec, w, h, 25.0);
            v.profile = profile;
            v.speed = 0; // ultrafast für x264
            settings.video = Some(v);
            let out = dir.join(format!("{label}.{}", if codec == "prores" { "mov" } else { "mp4" }));
            settings.output = out.to_string_lossy().into_owned();
            let plan = build_render_plan(&tl, &media, &settings, &NoNests);
            let frames = plan.total_frames;
            let fast = plan.segments.iter().all(|s| s.layers.len() == 1 && s.layers[0].is_identity());
            let t0 = Instant::now();
            let (tx, rx) = std::sync::mpsc::channel();
            run_export_worker(
                label.into(),
                plan,
                settings,
                tx,
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(Vec::new())),
            );
            let mut ok = false;
            while let Ok(ev) = rx.try_recv() {
                if let ServiceEvent::SequenceExportDone { ok: o, .. } = ev {
                    ok = o;
                }
            }
            assert!(ok, "{label} fehlgeschlagen");
            let secs_elapsed = t0.elapsed().as_secs_f64();
            let fps = frames as f64 / secs_elapsed;
            println!(
                "PERF {label}: {frames} Frames in {secs_elapsed:.2}s = {fps:.1} fps  (Pfad: {})",
                if fast { "8-Bit-Schnellpfad (ffmpeg-direkt)" } else { "Float-Compositing" }
            );
            fps
        };

        let fast8 = run("schnellpfad_h264_8bit", false, "h264", 0);
        let comp8 = run("composited_h264_8bit", true, "h264", 0);
        let comp10 = run("composited_prores_10bit", true, "prores", 3);
        println!(
            "PERF Zusammenfassung @ {w}x{h}: Schnellpfad={fast8:.1} fps, \
             Float-8Bit={comp8:.1} fps, Float-10Bit(16-Bit-Pipe)={comp10:.1} fps"
        );
        // Der 8-Bit-Schnellpfad bleibt erhalten und ist nicht langsamer als der
        // Float-Compositing-Pfad (Sanity, keine harte Schwelle).
        assert!(fast8 > 0.0 && comp8 > 0.0 && comp10 > 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-End-Übergang: Rot → Blau per Überblendung. Mitten im Fenster
    /// muss der Export beide Decoder mischen (≈ 50 % Rot + 50 % Blau); vor
    /// und nach dem Fenster liegen die reinen Farben an.
    #[test]
    fn end_to_end_export_renders_cross_dissolve() {
        let dir = std::env::temp_dir().join(format!("editron-export-trans-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let red = dir.join("rot.png");
        let blue = dir.join("blau.png");
        let out = dir.join("dissolve.mp4");
        for (path, color) in [(&red, "red"), (&blue, "blue")] {
            let gen = Command::new(crate::services::ffmpeg_bin())
                .args(["-y", "-v", "error"])
                .args(["-f", "lavfi", "-i", &format!("color={color}:size=640x360"), "-frames:v", "1"])
                .arg(path)
                .status()
                .unwrap();
            assert!(gen.success());
        }

        let mk_asset = |id: &str, path: &std::path::Path| {
            let mut a = video_asset(id, &path.to_string_lossy());
            a.kind = MediaKind::Image;
            a.info.video[0].width = 640;
            a.info.video[0].height = 360;
            a.info.audio.clear();
            a
        };
        let mk_clip = |id: &str, asset: &str, start: f64| {
            let mut c = clip(id, "v1", TrackKind::Video, asset, start, 2.0);
            c.src_duration = f64::INFINITY;
            c
        };
        let (mut tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![mk_clip("r", "RED", 0.0), mk_clip("b", "BLUE", 2.0)],
            vec![mk_asset("RED", &red), mk_asset("BLUE", &blue)],
        );
        tl.add_transition(
            crate::core::transitions::TransitionKind::CrossDissolve,
            "r",
            crate::core::timeline::TrimEdge::End,
            1.0,
        )
        .unwrap();

        let mut settings = test_settings();
        settings.audio = None;
        settings.output = out.to_string_lossy().into_owned();
        if let Some(v) = settings.video.as_mut() {
            v.width = 640;
            v.height = 360;
            v.speed = 0;
            v.quality = VideoQuality::Crf(16);
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.total_frames, 100);

        let (tx, rx) = std::sync::mpsc::channel();
        run_export_worker(
            "dissolve-job".into(),
            plan,
            settings,
            tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let mut ok = false;
        let mut error = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                ok = o;
                error = e;
            }
        }
        assert!(ok, "Export fehlgeschlagen: {error:?}");

        let (w, h) = (640usize, 360usize);
        // Vor dem Fenster (t = 1,0): reines Rot.
        let early = rgb_at(&decode_frame_rgb(&out, 1.0, w, h), w, 320, 180);
        assert!(early[0] > 200 && early[2] < 60, "vorher rot: {early:?}");
        // Mitte des Fensters (t = 2,0; p = 0,5): halb Rot, halb Blau.
        let mid = rgb_at(&decode_frame_rgb(&out, 2.0, w, h), w, 320, 180);
        assert!(
            (mid[0] as i32 - 127).abs() < 28 && (mid[2] as i32 - 127).abs() < 28 && mid[1] < 50,
            "Mitte muss Mischfarbe sein: {mid:?}"
        );
        // Nach dem Fenster (t = 3,2): reines Blau.
        let late = rgb_at(&decode_frame_rgb(&out, 3.2, w, h), w, 320, 180);
        assert!(late[2] > 200 && late[0] < 60, "nachher blau: {late:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Crossfade-Hüllkurven landen im Mix: ein lineares Ausblenden über die
    /// volle Cliplänge macht das Ende praktisch stumm.
    #[test]
    fn audio_mix_applies_transition_fades() {
        let dir = std::env::temp_dir().join(format!("editron-export-xfade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("ton.wav");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
            .args(["-c:a", "pcm_s16le"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(gen.success());

        let plan = RenderPlan {
            duration: 2.0,
            audio: vec![AudioClipPlan {
                path: src.to_string_lossy().into_owned(),
                start_in_mix: 0.0,
                duration: 2.0,
                src_in: 0.0,
                speed: 1.0,
                gain_l: 1.0,
                gain_r: 1.0,
                volume: AnimatedParam::fixed(0.0),
                effects: Vec::new(),
                fades: vec![PlanAudioFade {
                    t0: 0.0,
                    t1: 2.0,
                    fade_in: false,
                    equal_power: false,
                }],
            }],
            ..Default::default()
        };
        let audio = default_audio("pcm32f", None);
        let wav = dir.join("mix.wav");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut progress = Progress::new(&tx, "xfade-job");
        let children = Arc::new(Mutex::new(Vec::new()));
        let mut registry = ChildRegistry::new(children);
        mix_audio_to_wav(
            &plan,
            &audio,
            &wav,
            &AtomicBool::new(false),
            &mut registry,
            &mut progress,
        )
        .expect("mix");

        let bytes = std::fs::read(&wav).unwrap();
        let data = &bytes[58..];
        let rms = |range: std::ops::Range<usize>| -> f32 {
            let mut sum = 0f64;
            let mut n = 0usize;
            for i in range {
                let off = i * 4;
                let v = f32::from_le_bytes([
                    data[off],
                    data[off + 1],
                    data[off + 2],
                    data[off + 3],
                ]);
                sum += (v as f64) * (v as f64);
                n += 1;
            }
            ((sum / n as f64).sqrt()) as f32
        };
        let total = data.len() / 4;
        let head = rms(0..total / 10);
        let tail = rms(total - total / 20..total);
        assert!(head > 0.02, "Anfang hörbar: {head}");
        assert!(tail < head * 0.1, "Ende ausgeblendet: {head} → {tail}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Lautstärke-Keyframes (0 dB → −60 dB) müssen den Mix hörbar ausblenden.
    #[test]
    fn audio_mix_applies_volume_envelope() {
        let dir = std::env::temp_dir().join(format!("editron-export-vol-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("ton.wav");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
            .args(["-c:a", "pcm_s16le"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(gen.success());

        let mut volume = AnimatedParam::fixed(0.0);
        volume.upsert_key(0.0, 0.0);
        volume.upsert_key(2.0, -60.0);
        let plan = RenderPlan {
            duration: 2.0,
            audio: vec![AudioClipPlan {
                path: src.to_string_lossy().into_owned(),
                start_in_mix: 0.0,
                duration: 2.0,
                src_in: 0.0,
                speed: 1.0,
                gain_l: 1.0,
                gain_r: 1.0,
                volume,
                effects: Vec::new(),
                fades: Vec::new(),
            }],
            ..Default::default()
        };
        let audio = default_audio("pcm32f", None);
        let wav = dir.join("mix.wav");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut progress = Progress::new(&tx, "vol-job");
        let children = Arc::new(Mutex::new(Vec::new()));
        let mut registry = ChildRegistry::new(children);
        mix_audio_to_wav(
            &plan,
            &audio,
            &wav,
            &AtomicBool::new(false),
            &mut registry,
            &mut progress,
        )
        .expect("mix");

        // f32-Samples direkt aus der Zwischendatei lesen (Header 58 Bytes).
        let bytes = std::fs::read(&wav).unwrap();
        let data = &bytes[58..];
        let rms = |range: std::ops::Range<usize>| -> f32 {
            let mut sum = 0f64;
            let mut n = 0usize;
            for i in range {
                let off = i * 4;
                let v = f32::from_le_bytes([
                    data[off],
                    data[off + 1],
                    data[off + 2],
                    data[off + 3],
                ]);
                sum += (v as f64) * (v as f64);
                n += 1;
            }
            ((sum / n as f64).sqrt()) as f32
        };
        let total = data.len() / 4;
        let head = rms(0..total / 10);
        let tail = rms(total - total / 10..total);
        // lavfi-sine liegt bei ≈ −21 dB RMS (plus Upmix-Dämpfung) — wichtig
        // ist das Verhältnis: das Ende muss praktisch ausgeblendet sein.
        assert!(head > 0.02, "Anfang muss hörbar sein: {head}");
        assert!(tail < head * 0.05, "Ende muss ausgeblendet sein: {head} → {tail}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Abbruch räumt auf: kein Ziel, keine .part-Datei, Cancelled-Event.
    #[test]
    fn export_cancel_cleans_up() {
        let dir = std::env::temp_dir().join(format!("editron-export-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("quelle.mp4");
        let out = dir.join("ergebnis.mp4");
        let gen = Command::new(crate::services::ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "testsrc=duration=8:size=640x360:rate=25"])
            .args(["-c:v", "libx264", "-preset", "ultrafast"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(gen.success());

        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("v", "v1", TrackKind::Video, "VID", 0.0, 8.0)],
            vec![video_asset("VID", &src.to_string_lossy())],
        );
        let mut settings = test_settings();
        settings.audio = None;
        settings.output = out.to_string_lossy().into_owned();
        if let Some(v) = settings.video.as_mut() {
            v.speed = 0;
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(true)); // sofort abgebrochen
        let children = Arc::new(Mutex::new(Vec::new()));
        run_export_worker("cancel-job".into(), plan, settings, tx, cancel, children);

        let mut cancelled = false;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok, cancelled: c, .. } = ev {
                assert!(!ok);
                cancelled = c;
            }
        }
        assert!(cancelled, "Abbruch muss als cancelled gemeldet werden");
        assert!(!out.exists(), "Zieldatei darf bei Abbruch nicht entstehen");
        assert!(!part_path(&out.to_string_lossy()).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_merges_contiguous_segments_of_same_source() {
        // Ein durchgehender Clip auf V1, ein V2-Track ohne Überlappung an
        // anderer Stelle erzeugt Grenzen — Segmente desselben Clips mit
        // nahtloser Medienzeit müssen verschmelzen.
        let (tl, media) = state_with(
            vec![track("v2", TrackKind::Video), track("v1", TrackKind::Video)],
            vec![
                clip("a", "v1", TrackKind::Video, "A", 0.0, 10.0),
                clip("b", "v2", TrackKind::Video, "B", 12.0, 2.0),
            ],
            vec![video_asset("A", "/a.mp4"), video_asset("B", "/b.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        // A (0..10) bleibt EIN Segment, Lücke (10..12) schwarz, dann B.
        assert_eq!(plan.segments.len(), 3, "{:?}", plan.segments);
        assert_eq!(plan.segments[0].frames, 250);
        assert!(plan.segments[1].layers.is_empty());
    }
    #[test]
    fn plan_includes_title_layers_and_forces_compositor() {
        // V2 (oben): Titel 1..3 — V1 (unten): Video 0..4.
        let (mut tl, media) = state_with(
            vec![track("v2", TrackKind::Video), track("v1", TrackKind::Video)],
            vec![clip("a", "v1", TrackKind::Video, "A", 0.0, 4.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        let mut title = clip("t", "v2", TrackKind::Video, "", 1.0, 2.0);
        title.src_duration = f64::INFINITY;
        title.title = Some(crate::core::title::TitleSpec::default());
        tl.clips.push(title);

        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert_eq!(plan.segments.len(), 3, "{:?}", plan.segments);
        // Mitte: Video unten, Titel oben (Zeichenreihenfolge).
        let mid = &plan.segments[1].layers;
        assert_eq!(mid.len(), 2);
        assert!(mid[0].title.is_none());
        assert!(mid[1].title.is_some());
        assert!(
            !mid[1].is_identity(),
            "Titel-Layer dürfen nie in den ffmpeg-Schnellpfad"
        );
        // Vor/nach dem Titel: Video allein, Schnellpfad bleibt erhalten.
        assert!(plan.segments[0].layers[0].is_identity());
        assert!(plan.segments[2].layers[0].is_identity());
    }

    #[test]
    fn title_only_timeline_validates_and_renders_video() {
        let (mut tl, media) = state_with(vec![track("v1", TrackKind::Video)], vec![], vec![]);
        let mut title = clip("t", "v1", TrackKind::Video, "", 0.0, 5.0);
        title.src_duration = f64::INFINITY;
        title.title = Some(crate::core::title::TitleSpec::default());
        tl.clips.push(title);

        let plan = build_render_plan(&tl, &media, &test_settings(), &NoNests);
        assert!(plan.has_video_media(), "Titel zählt als Videoinhalt");
        assert_eq!(plan.segments.len(), 1);
        assert_eq!(plan.total_frames, 125);

        let issues = validate(&tl, &media, Some(true), None, &test_settings(), &NoNests);
        assert!(
            !issues.iter().any(|i| i.severity == Severity::Error),
            "Titel-only-Export muss validieren: {issues:?}"
        );
    }

    // -------------------------------------------------- Encoder/Bild-Sequenz

    #[test]
    fn encoder_catalog_has_software_first_and_hardware_backends() {
        // Software ([0]) muss zur Codec-`encoder`-Id passen.
        assert_eq!(encoders_for("h264")[0].id, "libx264");
        assert_eq!(encoders_for("hevc")[0].id, "libx265");
        assert!(!encoders_for("h264")[0].is_hardware());
        // Hardware-Backends vorhanden.
        let ids: Vec<&str> = encoders_for("h264").iter().map(|e| e.id).collect();
        for want in ["h264_nvenc", "h264_qsv", "h264_vaapi", "h264_videotoolbox"] {
            assert!(ids.contains(&want), "fehlt: {want}");
        }
        // Codecs ohne Hardware haben genau ein (Software-)Backend.
        assert_eq!(encoders_for("prores").len(), 1);
        assert_eq!(encoders_for("av1").len(), 1);
    }

    #[test]
    fn available_video_encoders_hides_unlisted_hardware() {
        let mut set = HashSet::new();
        set.insert("libx264".to_string());
        set.insert("h264_nvenc".to_string());
        let shown = available_video_encoders("h264", Some(&set));
        let ids: Vec<&str> = shown.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec!["libx264", "h264_nvenc"]);
        // Ohne bekannte Liste werden alle gezeigt.
        assert_eq!(available_video_encoders("h264", None).len(), encoders_for("h264").len());
    }

    #[test]
    fn video_codec_args_pick_quality_flag_per_encoder() {
        let cont = container("mp4");
        // Software → -crf.
        let mut v = default_video("h264", 1920, 1080, 25.0);
        v.quality = VideoQuality::Crf(20);
        let a = video_codec_args(&v, cont, OutputColor::Bt709);
        assert!(a.windows(2).any(|w| w == ["-crf", "20"]), "{a:?}");

        // NVENC → -cq + -b:v 0, kein -crf.
        v.encoder = encoder_def("h264", "h264_nvenc");
        let a = video_codec_args(&v, cont, OutputColor::Bt709);
        assert!(a.windows(2).any(|w| w == ["-cq", "20"]), "{a:?}");
        assert!(!a.iter().any(|x| x == "-crf"));

        // VAAPI → Render-Device + -qp + hwupload im Filter.
        v.encoder = encoder_def("h264", "h264_vaapi");
        let a = video_codec_args(&v, cont, OutputColor::Bt709);
        assert!(a.iter().any(|x| x == "-vaapi_device"), "{a:?}");
        assert!(a.windows(2).any(|w| w == ["-qp", "20"]), "{a:?}");
        assert!(a.iter().any(|x| x.contains("hwupload")), "{a:?}");

        // Bitrate-Modus wirkt encoder-unabhängig.
        v.encoder = encoder_def("h264", "libx264");
        v.quality = VideoQuality::Bitrate(8000);
        let a = video_codec_args(&v, cont, OutputColor::Bt709);
        assert!(a.windows(2).any(|w| w == ["-b:v", "8000k"]), "{a:?}");
    }

    #[test]
    fn image_sequence_pattern_and_frame_args() {
        assert_eq!(
            image_sequence_pattern("/out/clip.png"),
            "/out/clip_%06d.png"
        );
        let png = frame_export_args("png");
        assert!(png.windows(2).any(|w| w == ["-c:v", "png"]), "{png:?}");
        let jpg = frame_export_args("jpeg");
        assert!(jpg.windows(2).any(|w| w == ["-c:v", "mjpeg"]), "{jpg:?}");
        let tif = frame_export_args("TIFF");
        assert!(tif.windows(2).any(|w| w == ["-c:v", "tiff"]), "{tif:?}");
    }

    #[test]
    fn image_sequence_container_is_video_only() {
        let c = container("png_seq");
        assert!(c.image_sequence);
        assert!(c.video);
        assert!(c.audio_codecs.is_empty());
        assert_eq!(c.video_codecs, &["png"]);
    }

    /// End-to-End Untertitel: Einbrennen erzeugt sichtbar helle Pixel im
    /// unteren Drittel (weiße Schrift, Standardstil), Sidecar schreibt die
    /// SRT-Datei millisekundengenau neben die Zieldatei.
    #[test]
    fn end_to_end_export_burns_and_sidecars_subtitles() {
        let dir = std::env::temp_dir().join(format!("editron-export-subs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Reine Untertitel-Timeline (Segmente über Schwarz) — kein Decoder.
        let mut tl = TimelineStore::default();
        tl.import_subtitle_cues(&[crate::core::subtitle::SrtCue {
            start: 0.0,
            end: 2.0,
            text: "UNTERTITEL TEST".into(),
        }]);
        let media = MediaStore::default();

        let render = |mode: SubtitleMode, out: &std::path::Path| {
            let mut settings = test_settings();
            settings.audio = None;
            settings.subtitles = mode;
            settings.output = out.to_string_lossy().into_owned();
            if let Some(v) = settings.video.as_mut() {
                v.width = 640;
                v.height = 360;
                v.speed = 0;
                v.quality = VideoQuality::Crf(16);
            }
            let plan = build_render_plan(&tl, &media, &settings, &NoNests);
            let (tx, rx) = std::sync::mpsc::channel();
            run_export_worker(
                "subs-job".into(),
                plan,
                settings,
                tx,
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(Vec::new())),
            );
            let mut ok = false;
            let mut error = None;
            while let Ok(ev) = rx.try_recv() {
                if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                    ok = o;
                    error = e;
                }
            }
            assert!(ok, "Export fehlgeschlagen ({mode:?}): {error:?}");
        };

        // ---- Einbrennen: Pixel-Verifikation ----
        let burn_out = dir.join("burn.mp4");
        render(SubtitleMode::BurnIn, &burn_out);
        let (w, h) = (640usize, 360usize);
        let frame = decode_frame_rgb(&burn_out, 1.0, w, h);
        // Unteres Drittel (Blockmitte bei +38 % ≈ Zeile 317): helle Pixel.
        let band_max = (280..350)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| rgb_at(&frame, w, x, y)[0])
            .max()
            .unwrap();
        assert!(band_max > 180, "weiße Schrift im unteren Drittel: max {band_max}");
        // Oberes Drittel bleibt schwarz.
        let top_max = (0..100)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| rgb_at(&frame, w, x, y)[0])
            .max()
            .unwrap();
        assert!(top_max < 60, "oben muss schwarz bleiben: max {top_max}");

        // ---- Sidecar: SRT neben der Zieldatei, ms-genau ----
        let side_out = dir.join("side.mp4");
        render(SubtitleMode::Sidecar, &side_out);
        let srt_path = sidecar_srt_path(&side_out.to_string_lossy(), "U1", true);
        let raw = std::fs::read_to_string(&srt_path).expect("Sidecar-SRT fehlt");
        let cues = crate::core::subtitle::parse_srt(&raw).expect("Sidecar parsebar");
        assert_eq!(cues.len(), 1);
        assert!((cues[0].start - 0.0).abs() < 0.001);
        assert!((cues[0].end - 2.0).abs() < 0.001);
        assert_eq!(cues[0].text, "UNTERTITEL TEST");
        // Sidecar brennt nicht ein: unteres Drittel bleibt schwarz.
        let frame = decode_frame_rgb(&side_out, 1.0, w, h);
        let band_max = (280..350)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| rgb_at(&frame, w, x, y)[0])
            .max()
            .unwrap();
        assert!(band_max < 60, "Sidecar darf nicht einbrennen: max {band_max}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-End Untertitel einbetten: MKV erhält einen SubRip-Stream
    /// (zweispurig → zwei Streams mit Spurnamen als Titel-Metadatum).
    #[test]
    fn end_to_end_export_embeds_subtitle_streams() {
        let dir = std::env::temp_dir().join(format!("editron-export-mux-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("mux.mkv");

        let mut tl = TimelineStore::default();
        tl.import_subtitle_cues(&[crate::core::subtitle::SrtCue {
            start: 0.0,
            end: 2.0,
            text: "Erste Sprache".into(),
        }]);
        tl.import_subtitle_cues(&[crate::core::subtitle::SrtCue {
            start: 0.5,
            end: 1.5,
            text: "Zweite Sprache".into(),
        }]);
        let media = MediaStore::default();

        let mut settings = ExportSettings {
            container: container("mkv"),
            video: Some(default_video("h264", 640, 360, 25.0)),
            audio: None,
            use_in_out: false,
            subtitles: SubtitleMode::Embed,
            image_start: 1,
            output: out.to_string_lossy().into_owned(),
        };
        if let Some(v) = settings.video.as_mut() {
            v.speed = 0;
        }
        let plan = build_render_plan(&tl, &media, &settings, &NoNests);
        assert_eq!(plan.subtitle_tracks.len(), 2);

        let (tx, rx) = std::sync::mpsc::channel();
        run_export_worker(
            "mux-job".into(),
            plan,
            settings,
            tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let mut ok = false;
        let mut error = None;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::SequenceExportDone { ok: o, error: e, .. } = ev {
                ok = o;
                error = e;
            }
        }
        assert!(ok, "Export fehlgeschlagen: {error:?}");

        // ffprobe: genau zwei SubRip-Streams mit den Spurnamen U1/U2.
        let probe = Command::new(crate::services::ffprobe_bin())
            .args(["-v", "error", "-select_streams", "s"])
            .args(["-show_entries", "stream=codec_name:stream_tags=title"])
            .args(["-of", "csv=p=0", &out.to_string_lossy()])
            .output()
            .expect("ffprobe");
        let report = String::from_utf8_lossy(&probe.stdout);
        let lines: Vec<&str> = report.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 2, "zwei Untertitel-Streams erwartet: {report}");
        assert!(lines.iter().all(|l| l.starts_with("subrip")), "{report}");
        assert!(report.contains("U1") && report.contains("U2"), "{report}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

