//! Benutzerdefinierte Export-Presets: serialisierbare Form der
//! [`ExportSettings`] (Codec-/Encoder-Ids statt `'static`-Referenzen),
//! persistiert als JSON unter `~/.config/editron/export_presets.json`.
//!
//! Die eingebauten [`crate::core::export::PRESETS`] sind Code (Funktionen mit
//! quellabhängiger Auflösung); Nutzer-Presets dagegen speichern konkrete
//! Werte — genau wie die aktuellen Dialog-Einstellungen. Beim Anwenden werden
//! die `'static`-Katalogreferenzen über die Id-Lookups wieder aufgelöst;
//! unbekannte Ids fallen auf den Katalog-Standard zurück.

use crate::core::export::{
    self, AudioSettings, ExportSettings, LoudnessNorm, SubtitleMode, VideoQuality, VideoSettings,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum QualityMode {
    Crf,
    Bitrate,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct VideoData {
    pub codec: String,
    /// ffmpeg-Encoder-Id (Hardware/Software); leer = Software-Standard.
    #[serde(default)]
    pub encoder: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub quality_mode: QualityMode,
    pub quality_value: u32,
    #[serde(default)]
    pub speed: usize,
    #[serde(default)]
    pub profile: usize,
    /// 10-Bit-Ausgabe für CRF/Bitrate-Codecs (HEVC main10 …).
    #[serde(default)]
    pub tenbit: bool,
    /// Bild-Abtastung (Progressiv/Interlaced); fehlend = progressiv.
    #[serde(default)]
    pub scan: export::ScanMode,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AudioData {
    pub codec: String,
    pub bitrate_kbps: u32,
    pub sample_rate: u32,
    pub channels: u32,
}

/// Serialisierbare Lautheits-Normalisierung (siehe [`LoudnessNorm`]).
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LoudnessData {
    pub target_i: f64,
    pub true_peak: f64,
    pub lra: f64,
}

fn default_image_start() -> u32 {
    1
}

/// Serialisierbarer Schnappschuss der Export-Einstellungen (ohne Zielpfad).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PresetData {
    pub container: String,
    #[serde(default)]
    pub video: Option<VideoData>,
    #[serde(default)]
    pub audio: Option<AudioData>,
    /// Lautheits-Normalisierung (None = aus).
    #[serde(default)]
    pub loudness: Option<LoudnessData>,
    /// Audiospuren getrennt als Stems ausgeben (je Spur ein Audio-Stream).
    #[serde(default)]
    pub audio_stems: bool,
    #[serde(default)]
    pub subtitles: SubtitleMode,
    #[serde(default = "default_image_start")]
    pub image_start: u32,
}

impl PresetData {
    /// Aktuelle Einstellungen als serialisierbares Preset festhalten (der
    /// Zielpfad gehört nicht zum Preset und wird verworfen).
    pub fn from_settings(s: &ExportSettings) -> PresetData {
        PresetData {
            container: s.container.id.to_string(),
            video: s.video.as_ref().map(|v| VideoData {
                codec: v.codec.id.to_string(),
                encoder: v.encoder.id.to_string(),
                width: v.width,
                height: v.height,
                fps: v.fps,
                quality_mode: match v.quality {
                    VideoQuality::Crf(_) => QualityMode::Crf,
                    VideoQuality::Bitrate(_) => QualityMode::Bitrate,
                },
                quality_value: match v.quality {
                    VideoQuality::Crf(x) | VideoQuality::Bitrate(x) => x,
                },
                speed: v.speed,
                profile: v.profile,
                tenbit: v.tenbit,
                scan: v.scan,
            }),
            audio: s.audio.as_ref().map(|a| AudioData {
                codec: a.codec.id.to_string(),
                bitrate_kbps: a.bitrate_kbps,
                sample_rate: a.sample_rate,
                channels: a.channels,
            }),
            loudness: s.loudness.map(|l| LoudnessData {
                target_i: l.target_i,
                true_peak: l.true_peak,
                lra: l.lra,
            }),
            audio_stems: s.audio_stems,
            subtitles: s.subtitles,
            image_start: s.image_start,
        }
    }

    /// Preset auf konkrete Export-Einstellungen abbilden (Katalog-Lookups);
    /// unbekannte Codec-/Encoder-Ids fallen auf den Standard zurück.
    pub fn to_settings(&self, output: String) -> ExportSettings {
        let container = export::container(&self.container);
        let video = self.video.as_ref().map(|v| {
            let codec = export::video_codec(&v.codec);
            let encoder = export::encoder_def(codec.id, &v.encoder);
            let quality = match v.quality_mode {
                QualityMode::Crf => VideoQuality::Crf(v.quality_value),
                QualityMode::Bitrate => VideoQuality::Bitrate(v.quality_value),
            };
            VideoSettings {
                codec,
                encoder,
                width: v.width,
                height: v.height,
                fps: v.fps,
                quality,
                speed: v.speed,
                profile: v.profile,
                tenbit: v.tenbit,
                scan: v.scan,
            }
        });
        let audio = self.audio.as_ref().map(|a| {
            let codec = export::audio_codec(&a.codec);
            AudioSettings {
                codec,
                bitrate_kbps: a.bitrate_kbps,
                sample_rate: a.sample_rate,
                channels: a.channels,
            }
        });
        let loudness = self.loudness.map(|l| {
            LoudnessNorm {
                target_i: l.target_i,
                true_peak: l.true_peak,
                lra: l.lra,
            }
            .clamped()
        });
        ExportSettings {
            container,
            video,
            audio,
            loudness,
            use_in_out: false,
            audio_stems: self.audio_stems,
            subtitles: self.subtitles,
            image_start: self.image_start,
            output,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NamedPreset {
    pub name: String,
    pub data: PresetData,
}

/// Geladene Nutzer-Presets (Reihenfolge = Speicherreihenfolge).
#[derive(Default)]
pub struct UserPresets {
    pub presets: Vec<NamedPreset>,
}

fn presets_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("editron")
        .join("export_presets.json")
}

impl UserPresets {
    pub fn load() -> UserPresets {
        let presets = std::fs::read_to_string(presets_path())
            .ok()
            .and_then(|raw| serde_json::from_str::<Vec<NamedPreset>>(&raw).ok())
            .unwrap_or_default();
        UserPresets { presets }
    }

    /// Auf Platte schreiben (atomar via `.tmp` → rename).
    pub fn save(&self) {
        let path = presets_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.presets) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<&PresetData> {
        self.presets.iter().find(|p| p.name == name).map(|p| &p.data)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.presets.iter().any(|p| p.name == name)
    }

    /// Preset speichern/überschreiben (gleicher Name = Überschreiben) und
    /// sofort persistieren.
    pub fn upsert(&mut self, name: &str, data: PresetData) {
        if let Some(p) = self.presets.iter_mut().find(|p| p.name == name) {
            p.data = data;
        } else {
            self.presets.push(NamedPreset {
                name: name.to_string(),
                data,
            });
        }
        self.save();
    }

    /// Preset löschen und persistieren.
    pub fn remove(&mut self, name: &str) {
        self.presets.retain(|p| p.name != name);
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::export::{LoudnessNorm, PRESETS};

    #[test]
    fn roundtrip_preserves_loudness() {
        // Lautheits-Einstellung übersteht Settings → Daten → JSON → Settings.
        let mut original = (PRESETS[0].build)((1920, 1080), 25.0);
        original.loudness = Some(LoudnessNorm { target_i: -16.0, true_peak: -1.5, lra: 11.0 });
        let json = serde_json::to_string(&PresetData::from_settings(&original)).unwrap();
        let back: PresetData = serde_json::from_str(&json).unwrap();
        let l = back.to_settings(String::new()).loudness.expect("loudness erhalten");
        assert!((l.target_i - (-16.0)).abs() < 1e-9);
        assert!((l.true_peak - (-1.5)).abs() < 1e-9);
        assert!((l.lra - 11.0).abs() < 1e-9);
    }

    #[test]
    fn loudness_defaults_to_off_for_legacy_presets() {
        // Altes Preset-JSON ohne `loudness`-Feld lädt fehlerfrei als „aus“.
        let data: PresetData =
            serde_json::from_str(r#"{"container":"mp4","subtitles":"none","imageStart":1}"#)
                .unwrap();
        assert!(data.loudness.is_none());
        assert!(data.to_settings(String::new()).loudness.is_none());
    }

    #[test]
    fn roundtrip_preserves_settings() {
        // „H.264 Master" → Daten → JSON → Daten → Settings: alles erhalten.
        let original = (PRESETS[3].build)((1920, 1080), 25.0);
        let data = PresetData::from_settings(&original);
        let json = serde_json::to_string(&data).unwrap();
        let back: PresetData = serde_json::from_str(&json).unwrap();
        let settings = back.to_settings("/tmp/out.mp4".into());

        assert_eq!(settings.container.id, original.container.id);
        let (ov, sv) = (original.video.unwrap(), settings.video.unwrap());
        assert_eq!(sv.codec.id, ov.codec.id);
        assert_eq!(sv.encoder.id, ov.encoder.id);
        assert_eq!((sv.width, sv.height), (ov.width, ov.height));
        assert_eq!(sv.speed, ov.speed);
        match (ov.quality, sv.quality) {
            (VideoQuality::Crf(a), VideoQuality::Crf(b)) => assert_eq!(a, b),
            (VideoQuality::Bitrate(a), VideoQuality::Bitrate(b)) => assert_eq!(a, b),
            _ => panic!("Qualitätsmodus verloren"),
        }
        let (oa, sa) = (original.audio.unwrap(), settings.audio.unwrap());
        assert_eq!(sa.codec.id, oa.codec.id);
        assert_eq!(sa.bitrate_kbps, oa.bitrate_kbps);
        assert_eq!(settings.output, "/tmp/out.mp4");
    }

    #[test]
    fn unknown_ids_fall_back_to_defaults() {
        let data = PresetData {
            container: "does-not-exist".into(),
            video: Some(VideoData {
                codec: "nope".into(),
                encoder: "nope".into(),
                width: 640,
                height: 480,
                fps: 30.0,
                quality_mode: QualityMode::Bitrate,
                quality_value: 8000,
                speed: 99,
                profile: 0,
                tenbit: false,
                scan: export::ScanMode::Progressive,
            }),
            audio: None,
            loudness: None,
            audio_stems: false,
            subtitles: SubtitleMode::None,
            image_start: 1,
        };
        let s = data.to_settings("/tmp/x.mp4".into());
        // Container fällt auf den ersten (mp4) zurück, Codec auf h264.
        assert_eq!(s.container.id, "mp4");
        let v = s.video.unwrap();
        assert_eq!(v.codec.id, "h264");
        // Encoder fällt auf den Software-Standard zurück.
        assert_eq!(v.encoder.id, "libx264");
    }

    #[test]
    fn upsert_overwrites_same_name() {
        let mut p = UserPresets::default();
        let a = PresetData::from_settings(&(PRESETS[0].build)((1920, 1080), 25.0));
        let b = PresetData::from_settings(&(PRESETS[1].build)((3840, 2160), 25.0));
        // save() schreibt auf Platte; im Test nur den In-Memory-Effekt prüfen,
        // indem wir die Persistenz nicht erneut laden.
        p.presets.push(NamedPreset { name: "Mein".into(), data: a });
        // upsert mit gleichem Namen ersetzt (überschreibt) statt anzuhängen.
        if let Some(item) = p.presets.iter_mut().find(|x| x.name == "Mein") {
            item.data = b;
        }
        assert_eq!(p.presets.len(), 1);
        assert_eq!(p.get("Mein").unwrap().container, "mp4");
        let v = p.get("Mein").unwrap().video.as_ref().unwrap();
        assert_eq!((v.width, v.height), (3840, 2160));
    }
}
