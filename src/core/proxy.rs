//! Proxy-Workflow (Premiere/Resolve-Pendant): leichtgewichtige Transcodes des
//! Originalmaterials (ProRes Proxy / DNxHR LB in halber oder viertel Auflösung)
//! für flüssiges Schneiden von 4K/8K-Material auf normaler Hardware.
//!
//! Dieses Modul kapselt die reine, testbare Logik: Format-/Auflösungs-Auswahl,
//! Ablagepfad neben dem Projekt, ffmpeg-Encode-Argumente und die
//! Gültigkeits-/Staleness-Prüfung. Die eigentliche Transcode-Ausführung (Worker-
//! Threads, Job-Registry, Fortschritt) liegt in [`crate::services`]; die
//! Decoder-Substitution (Vorschau aus dem Proxy) in [`crate::core::player`].
//!
//! KERN-INVARIANTE: Der EXPORT verwendet IMMER die Originaldatei
//! ([`crate::core::types::MediaAsset::path`]) — niemals den Proxy. Proxys
//! existieren ausschließlich für die Vorschau (Player, Hover-Scrub, Waveform).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Proxy-Codec. Beide sind Intra-Frame-Codecs (jeder Frame eigenständig) und
/// damit ideal zum Scrubben/Schneiden — anders als Long-GOP-Material.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProxyCodec {
    /// Apple ProRes 422 Proxy (`prores_ks`, Profil 0).
    ProResProxy,
    /// Avid DNxHR LB (Low Bandwidth) (`dnxhd`, Profil `dnxhr_lb`).
    DnxhrLb,
}

impl Default for ProxyCodec {
    fn default() -> Self {
        ProxyCodec::ProResProxy
    }
}

impl ProxyCodec {
    pub const ALL: [ProxyCodec; 2] = [ProxyCodec::ProResProxy, ProxyCodec::DnxhrLb];

    pub fn label(self) -> &'static str {
        match self {
            ProxyCodec::ProResProxy => "ProRes 422 Proxy",
            ProxyCodec::DnxhrLb => "DNxHR LB",
        }
    }

    /// ffmpeg-Video-Encoder dieses Proxy-Codecs (für die Verfügbarkeits-Prüfung
    /// gegen `ffmpeg -encoders`).
    pub fn encoder(self) -> &'static str {
        match self {
            ProxyCodec::ProResProxy => "prores_ks",
            ProxyCodec::DnxhrLb => "dnxhd",
        }
    }

    /// Containerendung (beide passen in MOV; DNxHR ebenfalls).
    pub fn extension(self) -> &'static str {
        "mov"
    }
}

/// Auflösungsteiler des Proxys relativ zum Original.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProxyScale {
    /// Halbe Kantenlänge (1/4 der Pixel) — der gängige Standard.
    Half,
    /// Viertel Kantenlänge (1/16 der Pixel) — für sehr schwache Hardware.
    Quarter,
}

impl Default for ProxyScale {
    fn default() -> Self {
        ProxyScale::Half
    }
}

impl ProxyScale {
    pub const ALL: [ProxyScale; 2] = [ProxyScale::Half, ProxyScale::Quarter];

    pub fn label(self) -> &'static str {
        match self {
            ProxyScale::Half => "Halbe Auflösung",
            ProxyScale::Quarter => "Viertel-Auflösung",
        }
    }

    pub fn divisor(self) -> u32 {
        match self {
            ProxyScale::Half => 2,
            ProxyScale::Quarter => 4,
        }
    }
}

/// Persistierte Proxy-Einstellungen des Projekts (Teil der .etron-Datei).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProxySettings {
    #[serde(default)]
    pub codec: ProxyCodec,
    #[serde(default)]
    pub scale: ProxyScale,
    /// Eigener Ablageordner für Proxys. None = Standard („Proxies“ neben der
    /// Projektdatei bzw. der Quelle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

/// Proxy-Maße aus Quellgröße und Teiler: gerade Kanten, mindestens 16 px
/// (Encoder-Anforderung). Auflösungsunabhängige Transform-/Grade-Mathematik
/// rahmt Vorschau (Proxy) und Export (Original) deshalb deckungsgleich.
pub fn proxy_dims(src_w: u32, src_h: u32, scale: ProxyScale) -> (u32, u32) {
    let div = scale.divisor();
    let round_even = |v: u32| -> u32 {
        let scaled = ((v as f64 / div as f64).round() as u32).max(16);
        scaled & !1 // auf gerade abrunden
    };
    (round_even(src_w.max(2)), round_even(src_h.max(2)))
}

/// Ablageordner der Proxys: konfigurierter Ordner, sonst „Proxies“ neben der
/// Projektdatei; ohne gespeichertes Projekt neben der Quelldatei.
/// Premiere/Resolve legen Proxys ebenfalls in einen festen Ordner relativ zum
/// Projekt (dort konfigurierbar in den Ingest-Einstellungen).
pub fn proxy_dir(folder: Option<&str>, project_path: Option<&Path>, src_path: &str) -> PathBuf {
    if let Some(custom) = folder.map(str::trim).filter(|f| !f.is_empty()) {
        return PathBuf::from(custom);
    }
    if let Some(proj) = project_path {
        if let Some(parent) = proj.parent() {
            if !parent.as_os_str().is_empty() {
                return parent.join("Proxies");
            }
        }
    }
    Path::new(src_path)
        .parent()
        .map(|p| p.join("Proxies"))
        .unwrap_or_else(|| PathBuf::from("Proxies"))
}

/// Vollständiger Proxy-Zielpfad für ein Asset:
/// `<proxy_dir>/<stem>_<assetId>_proxy.mov`. Die (eindeutige, dateinamen-
/// sichere) Asset-ID hält gleichnamige Quellen aus verschiedenen Ordnern UND
/// über Sitzungen hinweg auseinander (kein Proxy-Kollidieren im Ablageordner).
pub fn proxy_output_path(
    settings: &ProxySettings,
    project_path: Option<&Path>,
    src_path: &str,
    asset_id: &str,
) -> PathBuf {
    let dir = proxy_dir(settings.folder.as_deref(), project_path, src_path);
    let codec = settings.codec;
    let stem = Path::new(src_path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "media".to_string());
    // Asset-IDs sind hex + Bindestrich (siehe types::new_id) — dateinamen-sicher
    // und projektweit eindeutig; defensiv dennoch fremde Zeichen ersetzen.
    let tag: String = asset_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    dir.join(format!("{stem}_{tag}_proxy.{}", codec.extension()))
}

/// ffmpeg-Argumente für den Proxy-Transcode (ohne `-i`/Ausgabe). CFR-
/// Normalisierung bei VFR-Material (`-fps_mode cfr` + Quellrate), Audio als
/// seekbares PCM durchgereicht. So sind Proxy und Original gleich lang und ein
/// `-ss media_time` landet auf demselben Inhalt.
pub fn encode_args(codec: ProxyCodec, w: u32, h: u32, src_fps: f64) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    // Skalierung (bicubic) auf die Proxy-Maße.
    args.push("-vf".into());
    args.push(format!("scale={w}:{h}:flags=bicubic"));
    match codec {
        ProxyCodec::ProResProxy => {
            args.extend(
                [
                    "-c:v",
                    "prores_ks",
                    "-profile:v",
                    "0",
                    "-pix_fmt",
                    "yuv422p10le",
                    "-vendor",
                    "apl0",
                ]
                .map(String::from),
            );
        }
        ProxyCodec::DnxhrLb => {
            args.extend(
                [
                    "-c:v",
                    "dnxhd",
                    "-profile:v",
                    "dnxhr_lb",
                    "-pix_fmt",
                    "yuv422p",
                ]
                .map(String::from),
            );
        }
    }
    // CFR-Normalisierung: VFR-Quellen bekommen eine konstante Bildrate, damit
    // Vorschau-Seeks frame-stabil sind. Quellrate übernehmen, falls bekannt.
    if src_fps.is_finite() && src_fps > 0.0 {
        args.push("-r".into());
        args.push(crate::core::export::fps_arg(src_fps));
    }
    args.extend(["-fps_mode", "cfr"].map(String::from));
    // Audio durchgereicht als PCM (in MOV seekbar, jede Samplerate erhalten).
    args.extend(["-c:a", "pcm_s16le", "-ar", "48000"].map(String::from));
    args
}

/// Dateimtime in Unix-Sekunden (f64) — für die Staleness-Prüfung des Proxys
/// gegen die Quelle.
pub fn file_mtime_secs(path: &str) -> Option<f64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
}

/// Ist ein Proxy noch gültig? Proxy-Datei muss existieren; ist die Quelle
/// online, darf sie nicht neuer sein als zum Zeitpunkt der Proxy-Erzeugung
/// (`proxy_src_mtime`). Fehlt die Quelle (offline), gilt der vorhandene Proxy
/// als nutzbar — er ist dann die einzige Vorschauquelle.
pub fn proxy_is_valid(proxy_path: &str, src_path: &str, proxy_src_mtime: Option<f64>) -> bool {
    if !Path::new(proxy_path).exists() {
        return false;
    }
    let Some(stamp) = proxy_src_mtime else {
        // Kein Stempel (Altprojekt): Existenz genügt.
        return true;
    };
    match file_mtime_secs(src_path) {
        // Quelle vorhanden: Proxy nur gültig, solange die Quelle nicht (deutlich)
        // neuer ist als der Stempel (1 s Toleranz gegen Dateisystem-Granularität).
        Some(src_mtime) => src_mtime <= stamp + 1.0,
        // Quelle offline: vorhandener Proxy bleibt nutzbar.
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dims_halve_and_quarter_even_and_clamped() {
        assert_eq!(proxy_dims(3840, 2160, ProxyScale::Half), (1920, 1080));
        assert_eq!(proxy_dims(3840, 2160, ProxyScale::Quarter), (960, 540));
        // Ungerade Quelle → gerade Proxy-Kante.
        assert_eq!(proxy_dims(1921, 1081, ProxyScale::Half), (960, 540));
        // Winzige Quelle wird auf das Encoder-Minimum (16) angehoben.
        let (w, h) = proxy_dims(20, 20, ProxyScale::Quarter);
        assert!(w >= 16 && h >= 16 && w % 2 == 0 && h % 2 == 0);
    }

    #[test]
    fn output_path_lands_in_proxy_folder_next_to_project() {
        let proj = PathBuf::from("/work/film/cut.etron");
        let s = ProxySettings::default();
        let out = proxy_output_path(&s, Some(&proj), "/footage/clipA.mp4", "abcd00001234");
        assert_eq!(out.parent().unwrap(), Path::new("/work/film/Proxies"));
        let name = out.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("clipA_"));
        assert!(name.ends_with("_proxy.mov"));
        // Ohne Projektpfad: Proxies-Ordner neben der Quelle.
        let out2 = proxy_output_path(&s, None, "/footage/clipA.mp4", "id");
        assert_eq!(out2.parent().unwrap(), Path::new("/footage/Proxies"));
        // Konfigurierter Ordner hat Vorrang.
        let custom = ProxySettings {
            folder: Some("/mnt/fast/proxys".into()),
            ..ProxySettings::default()
        };
        let out3 = proxy_output_path(&custom, Some(&proj), "/footage/clipA.mp4", "id");
        assert_eq!(out3.parent().unwrap(), Path::new("/mnt/fast/proxys"));
    }

    #[test]
    fn distinct_assets_get_distinct_proxy_names() {
        let s = ProxySettings::default();
        let a = proxy_output_path(&s, None, "/x/clip.mp4", "00000000aaaa1111");
        let b = proxy_output_path(&s, None, "/y/clip.mp4", "00000000bbbb2222");
        assert_ne!(a.file_name(), b.file_name());
    }

    #[test]
    fn encode_args_pick_right_encoder() {
        let p = encode_args(ProxyCodec::ProResProxy, 1920, 1080, 25.0);
        assert!(p.iter().any(|a| a == "prores_ks"));
        assert!(p.windows(2).any(|w| w[0] == "-vf" && w[1].contains("scale=1920:1080")));
        let d = encode_args(ProxyCodec::DnxhrLb, 960, 540, 0.0);
        assert!(d.iter().any(|a| a == "dnxhd"));
        assert!(d.iter().any(|a| a == "dnxhr_lb"));
        // Ohne bekannte Quellrate kein explizites -r, aber CFR-Normalisierung.
        assert!(!d.iter().any(|a| a == "-r"));
        assert!(d.windows(2).any(|w| w[0] == "-fps_mode" && w[1] == "cfr"));
    }

    #[test]
    fn proxy_validity_handles_missing_and_stale() {
        // Nicht existente Proxy-Datei ist ungültig.
        assert!(!proxy_is_valid("/nope/none.mov", "/nope/src.mp4", Some(1.0)));
        // Vorhandener Proxy, offline-Quelle, Stempel gesetzt → gültig (Fallback).
        let dir = std::env::temp_dir().join(format!("editron-proxy-valid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let proxy = dir.join("p.mov");
        std::fs::write(&proxy, b"x").unwrap();
        assert!(proxy_is_valid(
            &proxy.to_string_lossy(),
            "/missing/original.mp4",
            Some(123.0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
