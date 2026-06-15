use std::collections::HashSet;

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

