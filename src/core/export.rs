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
use crate::core::compose;
use crate::core::timeline::{sequence_end, TimelineStore, TrackKind};
use crate::core::types::MediaKind;
use crate::services::ServiceEvent;
use crate::stores::MediaStore;
use std::collections::HashSet;
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
    },
    VideoCodecDef {
        id: "vp9",
        label: "VP9 (libvpx)",
        encoder: "libvpx-vp9",
        quality: QualityKind::CrfOrBitrate { crf: (15, 50, 32) },
        speed_presets: &[],
        default_speed: 0,
        pix_fmt: "yuv420p",
    },
    VideoCodecDef {
        id: "av1",
        label: "AV1 (SVT-AV1)",
        encoder: "libsvtav1",
        quality: QualityKind::CrfOrBitrate { crf: (15, 50, 30) },
        speed_presets: &["12", "10", "8", "6", "4", "2"],
        default_speed: 2,
        pix_fmt: "yuv420p",
    },
];

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

/// Framerate-Vorgaben (Label, fps).
pub const FRAMERATES: &[(&str, f64)] = &[
    ("23,976", 23.976),
    ("24", 24.0),
    ("25", 25.0),
    ("29,97", 29.97),
    ("30", 30.0),
    ("50", 50.0),
    ("59,94", 59.94),
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
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub quality: VideoQuality,
    /// Index in `codec.speed_presets` (ignoriert, wenn leer).
    pub speed: usize,
    /// Index in die Profil-Liste (nur `QualityKind::Profiles`).
    pub profile: usize,
}

#[derive(Clone)]
pub struct AudioSettings {
    pub codec: &'static AudioCodecDef,
    pub bitrate_kbps: u32,
    pub sample_rate: u32,
    pub channels: u32,
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
        width,
        height,
        fps,
        quality,
        speed: codec.default_speed,
        profile: 0,
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

/// Ein Video-Layer eines Segments (Zeichenreihenfolge: unten → oben).
#[derive(Clone, Debug)]
pub struct VideoLayerPlan {
    pub clip_id: String,
    pub path: String,
    pub image: bool,
    /// Medienzeit des ersten Segment-Frames.
    pub src_in: f64,
    /// Animierbare Parameter (Keyframes in Medienzeit).
    pub fx: ClipFx,
}

impl VideoLayerPlan {
    /// Schnellpfad-Kriterium: keinerlei visuelle Transformation.
    pub fn is_identity(&self) -> bool {
        self.fx.is_visual_identity()
    }
}

#[derive(Clone, Debug)]
pub struct VideoSegment {
    pub frames: u64,
    /// Leer = Schwarzbild (Lücke).
    pub layers: Vec<VideoLayerPlan>,
}

#[derive(Clone, Debug)]
pub struct AudioClipPlan {
    pub path: String,
    /// Startzeit im Mix (Sekunden ab Exportbeginn).
    pub start_in_mix: f64,
    pub duration: f64,
    pub src_in: f64,
    /// Wirksamer Faktor je Seite: Master × Spur × Clip × Balance.
    pub gain_l: f32,
    pub gain_r: f32,
    /// Lautstärke-Kurve des Clips (dB, Keyframes in Medienzeit).
    pub volume: AnimatedParam,
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
    pub audio: Vec<AudioClipPlan>,
}

impl RenderPlan {
    /// Mindestens ein echtes Video-Segment (kein reines Schwarzbild)?
    pub fn has_video_media(&self) -> bool {
        self.segments.iter().any(|s| !s.layers.is_empty())
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

pub fn build_render_plan(
    timeline: &TimelineStore,
    media: &MediaStore,
    settings: &ExportSettings,
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
        plan.segments = plan_video_segments(timeline, media, start, plan.total_frames, video.fps, solo_any);
    }

    if settings.audio.is_some() {
        for track in timeline
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio && !t.muted && (!solo_any || t.solo))
        {
            let track_gain = db_to_linear(track.gain_db);
            let (pan_l, pan_r) = pan_gains(track.pan);
            for clip in timeline
                .clips
                .iter()
                .filter(|c| c.track_id == track.id && c.enabled)
            {
                let clip_start = clip.start.max(start);
                let clip_end = clip.end().min(end);
                if clip_end - clip_start <= 1e-9 {
                    continue;
                }
                let Some(asset) = media.asset(&clip.asset_id) else {
                    continue;
                };
                if asset.offline || asset.info.audio.is_empty() {
                    continue;
                }
                let gain = master * track_gain * db_to_linear(clip.gain_db);
                plan.audio.push(AudioClipPlan {
                    path: asset.path.clone(),
                    start_in_mix: clip_start - start,
                    duration: clip_end - clip_start,
                    src_in: clip.src_in + (clip_start - clip.start),
                    gain_l: gain * pan_l,
                    gain_r: gain * pan_r,
                    volume: clip.fx.volume_db.clone(),
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
fn plan_video_segments(
    timeline: &TimelineStore,
    media: &MediaStore,
    range_start: f64,
    total_frames: u64,
    fps: f64,
    solo_any: bool,
) -> Vec<VideoSegment> {
    struct Candidate {
        /// 0 = unterste sichtbare Videospur (Zeichenreihenfolge).
        draw_order: usize,
        f0: u64,
        f1: u64,
        clip_id: String,
        clip_start: f64,
        src_in: f64,
        path: String,
        image: bool,
        fx: ClipFx,
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    // Spur-Index 0 ist die OBERSTE Videospur → rückwärts = Zeichenreihenfolge.
    let video_tracks: Vec<&str> = timeline
        .tracks
        .iter()
        .rev()
        .filter(|t| t.kind == TrackKind::Video && !t.muted && (!solo_any || t.solo))
        .map(|t| t.id.as_str())
        .collect();

    for clip in timeline.clips.iter().filter(|c| c.enabled) {
        let Some(order) = video_tracks.iter().position(|id| *id == clip.track_id) else {
            continue;
        };
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
        let f0 = (((clip.start - range_start) * fps).round().max(0.0) as u64).min(total_frames);
        let f1 = (((clip.end() - range_start) * fps).round().max(0.0) as u64).min(total_frames);
        if f1 <= f0 {
            continue;
        }
        candidates.push(Candidate {
            draw_order: order,
            f0,
            f1,
            clip_id: clip.id.clone(),
            clip_start: clip.start,
            src_in: clip.src_in,
            path: asset.path.clone(),
            image,
            fx: clip.fx.clone(),
        });
    }

    // Schnittpunkte (in Frames) sammeln.
    let mut bounds: Vec<u64> = vec![0, total_frames];
    for c in &candidates {
        bounds.push(c.f0);
        bounds.push(c.f1);
    }
    bounds.sort_unstable();
    bounds.dedup();

    let mut segments: Vec<VideoSegment> = Vec::new();
    for pair in bounds.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if b <= a {
            continue;
        }
        let mut active: Vec<&Candidate> = candidates
            .iter()
            .filter(|c| c.f0 <= a && c.f1 >= b)
            .collect();
        active.sort_by_key(|c| c.draw_order);
        let layers: Vec<VideoLayerPlan> = active
            .iter()
            .map(|c| VideoLayerPlan {
                clip_id: c.clip_id.clone(),
                path: c.path.clone(),
                image: c.image,
                // Medienzeit des Segmentbeginns aus der Sequenzzeit ableiten.
                src_in: (c.src_in + (range_start + a as f64 / fps - c.clip_start)).max(0.0),
                fx: c.fx.clone(),
            })
            .collect();
        // Fortsetzungen desselben Layer-Stapels verschmelzen (spart
        // Decoder-Starts): gleiche Clips in gleicher Reihenfolge, und die
        // Medienzeit jedes Video-Layers läuft nahtlos weiter.
        let frames = b - a;
        if let Some(last) = segments.last_mut() {
            let merge = last.layers.len() == layers.len()
                && last
                    .layers
                    .iter()
                    .zip(layers.iter())
                    .all(|(l1, l2)| {
                        let continues =
                            (l1.src_in + last.frames as f64 / fps - l2.src_in).abs() < 0.5 / fps;
                        l1.clip_id == l2.clip_id && (l2.image || continues)
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

    let plan = build_render_plan(timeline, media, settings);
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
            if !set.contains(v.codec.encoder) {
                issues.push(error(format!(
                    "Encoder „{}“ fehlt in dieser FFmpeg-Installation.",
                    v.codec.encoder
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

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        export_inner(&job_id, &plan, &settings, &tx, &cancel, registry, &part, &wav)
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
) -> Result<(), ExportError> {
    let mut progress = Progress::new(tx, job_id);
    let _ = std::fs::remove_file(part);

    let with_audio = settings.audio.is_some();
    let with_video = settings.video.is_some();

    // ---- Phase A: Audio-Mixdown in eine temporäre f32-WAV ----
    if with_audio {
        let audio = settings.audio.as_ref().expect("audio settings");
        let span = if with_video { 6.0 } else { 85.0 };
        let total_units: u64 = plan
            .audio
            .iter()
            .map(|c| (c.duration * audio.sample_rate as f64) as u64)
            .sum();
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

    let mut file = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(wav)
        .map_err(|e| format!("Audio-Zwischendatei konnte nicht angelegt werden: {e}"))?;
    let data_off = write_wav_header(&mut file, rate, ch as u16, data_bytes as u32)
        .map_err(|e| format!("WAV-Header: {e}"))?;
    // Mit Stille auffüllen (f32 0.0 = Null-Bytes — set_len reicht).
    file.set_len(data_off + data_bytes)
        .map_err(|e| format!("Audio-Zwischendatei: {e}"))?;

    for clip in &plan.audio {
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
            .args(["-t", &format!("{:.4}", clip.duration)])
            .args(["-i", &clip.path])
            .args(["-vn", "-sn"])
            .args(["-f", "f32le", "-ac", &ch.to_string(), "-ar", &rate.to_string()])
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let (id, _, stdout, _) = children.spawn(&mut cmd)?;
        let mut stdout = stdout.ok_or("ffmpeg-stdout nicht verfügbar")?;

        // Pro Seite wirksamer Faktor; Mono mittelt beide Seiten.
        let gains: [f32; 2] = [clip.gain_l, clip.gain_r];
        let mono_gain = (clip.gain_l + clip.gain_r) * 0.5;
        // Lautstärke-Kurve (dB-Keyframes): blockweise ausgewertet —
        // 256 Frames ≈ 5 ms bei 48 kHz, glatt genug für Fades.
        let has_envelope = clip.volume.is_animated() || clip.volume.value != 0.0;
        const ENV_BLOCK: usize = 256;

        const CHUNK_FRAMES: usize = 32768;
        let mut decoded = vec![0u8; CHUNK_FRAMES * ch * 4];
        let mut existing = vec![0u8; CHUNK_FRAMES * ch * 4];
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
            let byte_pos = data_off + (offset_frames + frames_done) * ch as u64 * 4;
            let block = &mut existing[..got_frames * ch * 4];
            file.seek(SeekFrom::Start(byte_pos)).map_err(|e| e.to_string())?;
            file.read_exact(block).map_err(|e| format!("Mix-Lesen: {e}"))?;
            let mut fi = 0usize;
            while fi < got_frames {
                let n = ENV_BLOCK.min(got_frames - fi);
                let env = if has_envelope {
                    let media_t = clip.src_in + (frames_done + fi as u64) as f64 / rate as f64;
                    db_to_linear(clip.volume.eval(media_t))
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
                    let add = f32::from_le_bytes([
                        decoded[off],
                        decoded[off + 1],
                        decoded[off + 2],
                        decoded[off + 3],
                    ]);
                    let sum = old + add * gain;
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

// ----------------------------------------------------------- Video-Render

/// Encoder-Argumente für den Video-Codec (pure Funktion, testbar).
pub fn video_codec_args(v: &VideoSettings, container: &ContainerDef) -> Vec<String> {
    let mut args: Vec<String> = vec!["-c:v".into(), v.codec.encoder.into()];
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
        QualityKind::CrfOrBitrate { .. } => match v.quality {
            VideoQuality::Crf(crf) => {
                args.extend(["-crf".into(), crf.to_string()]);
                if v.codec.id == "vp9" {
                    // libvpx: CRF wirkt nur mit -b:v 0.
                    args.extend(["-b:v".into(), "0".into()]);
                }
            }
            VideoQuality::Bitrate(kbps) => {
                args.extend(["-b:v".into(), format!("{kbps}k")]);
            }
        },
    }
    if !v.codec.speed_presets.is_empty() {
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
    args.extend(["-pix_fmt".into(), pix_fmt.into()]);
    // RGBA → YUV explizit nach BT.709 wandeln und taggen.
    args.extend([
        "-vf".into(),
        "scale=out_color_matrix=bt709:out_range=tv".into(),
        "-color_primaries".into(),
        "bt709".into(),
        "-color_trc".into(),
        "bt709".into(),
        "-colorspace".into(),
        "bt709".into(),
    ]);
    args
}

/// Audio-Encoder-Argumente (pure Funktion, testbar).
pub fn audio_codec_args(a: &AudioSettings) -> Vec<String> {
    let mut args: Vec<String> = vec!["-c:a".into(), a.codec.encoder.into()];
    if !a.codec.bitrates.is_empty() {
        args.extend(["-b:a".into(), format!("{}k", a.bitrate_kbps)]);
    }
    args
}

fn render_video(
    plan: &RenderPlan,
    settings: &ExportSettings,
    wav: Option<&Path>,
    part: &Path,
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
    progress: &mut Progress,
) -> Result<(), String> {
    let video = settings.video.as_ref().expect("video settings");
    let frame_size = video.width as usize * video.height as usize * 4;
    let fps = fps_arg(video.fps);

    // ---- Encoder-Prozess ----
    let mut cmd = Command::new(crate::services::ffmpeg_bin());
    cmd.args(["-y", "-v", "error"])
        .args(["-f", "rawvideo", "-pixel_format", "rgba"])
        .args(["-video_size", &format!("{}x{}", video.width, video.height)])
        .args(["-framerate", &fps])
        .args(["-i", "pipe:0"]);
    if let Some(wav) = wav {
        cmd.args(["-i", &wav.to_string_lossy()]);
    }
    cmd.args(["-map", "0:v:0"]);
    if wav.is_some() {
        cmd.args(["-map", "1:a:0"]);
    }
    cmd.args(video_codec_args(video, settings.container));
    if let (Some(_), Some(a)) = (wav, settings.audio.as_ref()) {
        cmd.args(audio_codec_args(a));
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

    let black: Vec<u8> = {
        let mut px = vec![0u8; frame_size];
        for p in px.chunks_exact_mut(4) {
            p[3] = 255;
        }
        px
    };

    // ---- Segmente sequenziell durchpumpen ----
    let mut write_err: Option<String> = None;
    'segments: for segment in &plan.segments {
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
            let mut cmd = Command::new(crate::services::ffmpeg_bin());
            cmd.args(["-v", "error"]);
            if layer.image {
                cmd.args(["-loop", "1", "-framerate", &fps]);
            } else {
                cmd.args(["-ss", &format!("{:.4}", layer.src_in)]);
            }
            cmd.args(["-i", &layer.path]).args(["-an", "-sn"]);
            let filter = format!(
                "fps={fps},scale={w}:{h}:force_original_aspect_ratio=decrease:flags=bicubic,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black",
                w = video.width,
                h = video.height
            );
            cmd.args(["-vf", &filter])
                .args(["-frames:v", &segment.frames.to_string()])
                .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
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
        match render_segment_composited(segment, video, &fps, &mut enc_in, cancel, children, progress)
        {
            Ok(()) => {}
            Err(CompErr::Cancelled) => break 'segments,
            Err(CompErr::Failed(e)) => {
                write_err = Some(e);
                break 'segments;
            }
        }
    }

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

enum CompErr {
    Cancelled,
    Failed(String),
}

/// Ein Layer-Decoder im Compositing-Pfad: liefert transparent gepolsterte
/// RGBA-Frames in Decode-Auflösung (das volle Zielframe repräsentierend).
struct LayerStream {
    dec_id: u64,
    out: ChildStdout,
    /// Letzter vollständiger Frame (initial transparent — unsichtbar).
    frame: Vec<u8>,
    scratch: Vec<u8>,
    dead: bool,
    w: usize,
    h: usize,
    src_in: f64,
    fx: ClipFx,
}

impl LayerStream {
    /// Nächsten Frame einlesen; bei EOF/Kurz-Read bleibt der letzte stehen.
    fn advance(&mut self) {
        if self.dead {
            return;
        }
        let size = self.w * self.h * 4;
        let mut filled = 0;
        while filled < size {
            match self.out.read(&mut self.scratch[filled..size]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(_) => break,
            }
        }
        if filled == size {
            std::mem::swap(&mut self.frame, &mut self.scratch);
        } else {
            self.dead = true;
        }
    }
}

/// Segment mit Transformationen rendern: je Layer ein ffmpeg-Decoder
/// (Decode-Auflösung wächst mit der maximalen Skalierung im Segment, damit
/// Zooms scharf bleiben), pro Frame werden die animierten Parameter
/// ausgewertet und die Layer per CPU-Compositor auf das Canvas gemischt.
fn render_segment_composited(
    segment: &VideoSegment,
    video: &VideoSettings,
    fps_arg: &str,
    enc_in: &mut std::process::ChildStdin,
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
    progress: &mut Progress,
) -> Result<(), CompErr> {
    let (tw, th) = (video.width as usize, video.height as usize);
    let fps = video.fps;
    let seg_dur = segment.frames as f64 / fps;

    let mut layers: Vec<LayerStream> = Vec::new();
    let kill_layers = |children: &ChildRegistry, layers: &[LayerStream]| {
        for l in layers {
            children.kill(l.dec_id);
        }
    };

    for plan_layer in &segment.layers {
        // Decode-Auflösung: Zielgröße × max. Skalierung (gedeckelt) — mehr
        // als die Quelle hergibt, skaliert ffmpeg ohnehin nicht hoch (Schärfe
        // gewinnt nur, solange Quellpixel vorhanden sind).
        let max_s = compose::max_scale_in_window(
            &plan_layer.fx,
            plan_layer.src_in,
            plan_layer.src_in + seg_dur,
        )
        .clamp(1.0, 2.0);
        let dw = ((((tw as f64 * max_s) / 2.0).round() as usize) * 2).clamp(2, 4096);
        let dh = ((((th as f64 * max_s) / 2.0).round() as usize) * 2).clamp(2, 4096);

        let mut cmd = Command::new(crate::services::ffmpeg_bin());
        cmd.args(["-v", "error"]);
        if plan_layer.image {
            cmd.args(["-loop", "1", "-framerate", fps_arg]);
        } else {
            cmd.args(["-ss", &format!("{:.4}", plan_layer.src_in)]);
        }
        // Contain-Fit + TRANSPARENTES Padding: der Puffer repräsentiert das
        // volle Frame, Alpha (z. B. PNG) bleibt erhalten.
        let filter = format!(
            "fps={fps_arg},scale={dw}:{dh}:force_original_aspect_ratio=decrease:flags=bicubic,format=rgba,pad={dw}:{dh}:(ow-iw)/2:(oh-ih)/2:color=black@0.0"
        );
        cmd.args(["-i", &plan_layer.path])
            .args(["-an", "-sn"])
            .args(["-vf", &filter])
            .args(["-frames:v", &segment.frames.to_string()])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
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
        layers.push(LayerStream {
            dec_id,
            out,
            frame: vec![0u8; dw * dh * 4],
            scratch: vec![0u8; dw * dh * 4],
            dead: false,
            w: dw,
            h: dh,
            src_in: plan_layer.src_in,
            fx: plan_layer.fx.clone(),
        });
    }

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8);
    let mut canvas = vec![0u8; tw * th * 4];

    for f in 0..segment.frames {
        if cancel.load(Ordering::Relaxed) {
            kill_layers(children, &layers);
            return Err(CompErr::Cancelled);
        }
        for layer in &mut layers {
            layer.advance();
        }
        // Canvas opak schwarz zurücksetzen.
        for px in canvas.chunks_exact_mut(4) {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            px[3] = 255;
        }
        let t_off = f as f64 / fps;
        let frames: Vec<compose::CpuLayerFrame> = layers
            .iter()
            .filter_map(|l| {
                let fx = compose::eval_fx(&l.fx, l.src_in + t_off);
                if fx.opacity <= 0.0 {
                    return None;
                }
                Some(compose::CpuLayerFrame {
                    data: &l.frame,
                    w: l.w,
                    h: l.h,
                    // Der Layer-Puffer repräsentiert das volle Frame →
                    // natürliche Größe = Framegröße (Fit-Faktor 1).
                    quad: compose::layer_quad(tw as f64, th as f64, tw as f64, th as f64, &fx),
                    opacity: fx.opacity,
                })
            })
            .collect();
        compose::composite_frame(&mut canvas, tw, th, &frames, threads);
        if let Err(e) = enc_in.write_all(&canvas) {
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
                }],
                audio: vec![crate::core::types::AudioStreamInfo {
                    index: 1,
                    codec: "aac".into(),
                    channels: 2,
                    sample_rate: 48000,
                    bitrate: None,
                }],
            },
            thumbnail_path: None,
            imported_at: 0.0,
            offline: false,
        }
    }

    fn test_settings() -> ExportSettings {
        ExportSettings {
            container: container("mp4"),
            video: Some(default_video("h264", 1280, 720, 25.0)),
            audio: Some(default_audio("aac", None)),
            use_in_out: false,
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
        let plan = build_render_plan(&tl, &media, &test_settings());
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
    fn plan_inserts_black_for_gaps_and_respects_mute() {
        let mut t = track("v1", TrackKind::Video);
        t.muted = false;
        let (mut tl, media) = state_with(
            vec![t],
            vec![clip("a", "v1", TrackKind::Video, "A", 2.0, 2.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        let plan = build_render_plan(&tl, &media, &test_settings());
        assert_eq!(plan.segments.len(), 2); // Schwarz 0..2, dann Clip
        assert!(plan.segments[0].layers.is_empty());
        assert_eq!(plan.segments[0].frames, 50);

        tl.tracks[0].muted = true;
        let plan = build_render_plan(&tl, &media, &test_settings());
        assert!(!plan.has_video_media(), "gemutete Spur darf nicht rendern");
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
        let plan = build_render_plan(&tl, &media, &settings);
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
        let issues = validate(&tl, &media, Some(true), None, &test_settings());
        assert!(issues.iter().any(|i| i.severity == Severity::Error));

        let (tl, media) = state_with(
            vec![track("v1", TrackKind::Video)],
            vec![clip("a", "v1", TrackKind::Video, "A", 0.0, 5.0)],
            vec![video_asset("A", "/a.mp4")],
        );
        let mut settings = test_settings();
        settings.output = "/a.mp4".into();
        let issues = validate(&tl, &media, Some(true), None, &settings);
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
        let issues = validate(&tl, &media, Some(true), Some(&encoders), &settings);
        assert!(issues.iter().any(|i| i.message.contains("gerade")));
        assert!(issues.iter().any(|i| i.message.contains("libx264")));
    }

    #[test]
    fn fps_arg_maps_ntsc_rates() {
        assert_eq!(fps_arg(23.976), "24000/1001");
        assert_eq!(fps_arg(29.97), "30000/1001");
        assert_eq!(fps_arg(59.94), "60000/1001");
        assert_eq!(fps_arg(25.0), "25");
        assert_eq!(fps_arg(60.0), "60");
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
        let args = video_codec_args(&v, container("webm"));
        let joined = args.join(" ");
        assert!(joined.contains("-crf 30"));
        assert!(joined.contains("-b:v 0"), "VP9-CRF braucht -b:v 0: {joined}");

        let mut v = default_video("prores", 1920, 1080, 25.0);
        v.profile = 4;
        let args = video_codec_args(&v, container("mov"));
        let joined = args.join(" ");
        assert!(joined.contains("-profile:v 4"));
        assert!(joined.contains("yuv444p10le"));

        let v = default_video("hevc", 1920, 1080, 25.0);
        let joined = video_codec_args(&v, container("mp4")).join(" ");
        assert!(joined.contains("-tag:v hvc1"));
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
        let plan = build_render_plan(&tl, &media, &settings);
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
                    output: String::new(),
                },
            ),
        ];

        for (name, mut settings) in cases {
            let out = dir.join(name);
            settings.output = out.to_string_lossy().into_owned();
            let plan = build_render_plan(&tl, &media, &settings);
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
        let plan = build_render_plan(&tl, &media, &settings);
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
                gain_l: 1.0,
                gain_r: 1.0,
                volume,
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
        let plan = build_render_plan(&tl, &media, &settings);

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
        let plan = build_render_plan(&tl, &media, &test_settings());
        // A (0..10) bleibt EIN Segment, Lücke (10..12) schwarz, dann B.
        assert_eq!(plan.segments.len(), 3, "{:?}", plan.segments);
        assert_eq!(plan.segments[0].frames, 250);
        assert!(plan.segments[1].layers.is_empty());
    }
}
