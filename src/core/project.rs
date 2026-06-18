//! Projektdateien (.etron): versioniertes JSON-Format mit allen Projektdaten
//! (Workspace, Medien, Timeline, Quellmonitor), atomarem Speichern
//! (tmp + rename + .bak), Zuletzt-geöffnet-Liste und Autosave der Sitzung.

use crate::core::timeline::{TimelineClip, TimelineTrack};
use crate::core::transitions::Transition;
use crate::core::types::MediaAsset;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

pub const PROJECT_EXT: &str = "etron";
pub const PROJECT_FORMAT: &str = "editron-project";
/// v2: Sequenz-Einstellungen (`timeline.sequence`); v1-Dateien laden mit
/// 25 fps und aus den Medien geratener Auflösung weiter.
/// v3: Titel-Clips (`clips[].title`, Generator ohne Mediendatei) — ältere
/// App-Versionen würden sie als verwaiste Clips verwerfen, deshalb der
/// Versionssprung; v2-Dateien laden unverändert.
/// v4: Untertitel (Spurtyp `subtitle`, `tracks[].subtitleStyle`,
/// `clips[].subtitle`, `timeline.activeSubtitleTrackId`) — ältere App-
/// Versionen können den Spurtyp nicht deserialisieren, deshalb der
/// Versionssprung; v3-Dateien laden unverändert.
/// v5: Clip-Geschwindigkeit (`clips[].speed/reverse/freeze`) — ältere
/// App-Versionen würden die Felder ignorieren und Clips mit falschem
/// Tempo abspielen, deshalb der Versionssprung; v4-Dateien laden
/// unverändert (Default: 100 % vorwärts).
/// v6: Marker (`timeline.markers`, `clips[].markers`, `media[].markers`) —
/// Sequenz-, Clip- und Asset-Marker mit Farbe/Name/Notiz. Ältere App-
/// Versionen ignorieren die neuen Felder; v5-Dateien laden unverändert
/// (Default: keine Marker).
/// v7: Spur-Targeting/Source-Patching/Sync-Lock (`tracks[].sourcePatched`,
/// `tracks[].targeted`, `tracks[].syncLock`) für Insert-/Three-Point-Editing.
/// Ältere App-Versionen ignorieren die neuen Felder. v6-und-älter-Dateien
/// laden unverändert; beim Laden wird je Art ein Patch-/Target-Standard
/// gesetzt (V1/A1), falls keiner vorhanden ist.
/// v8: Spur-Audio-Effekte (`tracks[].effects`, Bus-Insert) und Spur-
/// Automation (`tracks[].volumeAuto`, `tracks[].panAuto` — Lautstärke/Pan
/// als Keyframe-Kurven in Sequenzzeit). Ältere App-Versionen ignorieren die
/// neuen Felder; v7-und-älter-Dateien laden unverändert (Default: keine
/// Spur-Effekte, keine Automation).
/// v9: Medien-Organisation — Bins (`mediaBins`), Asset-Zuordnung
/// (`media[].binId`), Farbetiketten (`media[].label`), Aufnahmedatum
/// (`media[].info.recordedAt`) und der Ansichts-Zustand des Browsers
/// (`mediaView`). Ältere App-Versionen ignorieren die neuen Felder; v8-und-
/// älter-Dateien laden unverändert — alle Assets landen in der Wurzel.
/// v10: Proxy-Workflow — Proxy-Pfad/Quell-mtime je Asset
/// (`media[].proxyPath`, `media[].proxySrcMtime`), globaler Schalter
/// `useProxies` und Proxy-Einstellungen (`proxySettings`). Ältere App-
/// Versionen ignorieren die neuen Felder; v9-und-älter-Dateien laden
/// unverändert (kein Proxy, Schalter aus).
/// v11: Mehrere Sequenzen pro Projekt (`sequences[]` mit je eigener Timeline,
/// `activeSequenceId`) und verschachtelte Sequenzen (`clips[].nestSeq`). Das
/// alte Einzel-`timeline`-Feld wird beim Speichern leer gelassen und nur noch
/// zum Laden von v≤10-Dateien gelesen (= eine Sequenz). Ältere App-Versionen
/// können das neue Format nicht öffnen (Versionssprung), daher der Bump.
/// v12: Multicam-Schnitt — Multicam-Quellen (`sequences[].timeline.multicam`
/// mit Winkeln + Sync-Offsets) und Multicam-Clips (`clips[].multicam` = Quelle
/// + aktiver Winkel). Ältere App-Versionen können den Multicam-Clip nicht
/// auflösen (er hätte kein `asset_id`), deshalb der Versionssprung; v11-und-
/// älter-Dateien laden unverändert (kein Multicam).
/// v13: Manuell verstellbare Spurhöhe (`sequences[].timeline.tracks[].height`,
/// logische Pixel, per Sash-Drag am Spurkopf). Ältere App-Versionen ignorieren
/// das Feld; v12-und-älter-Dateien laden unverändert (Standardhöhe der Spurart).
/// v14: Tonwertkurven in der Farbkorrektur (`clips[].grade.curves` = Luma-
/// Master- + R/G/B-Kanalkurven als monotone Splines). Ältere App-Versionen
/// ignorieren das Feld; v13-und-älter-Dateien laden unverändert (Identität).
/// v15: 3D-LUTs in der Farbkorrektur (`clips[].grade.inputLut`/`lookLut` =
/// Pfad + Stärke einer externen `.cube`-Datei; Input am Pipeline-Anfang, Look
/// am -Ende). Nur die Referenz wird gespeichert; fehlt die Datei, zeigt das
/// Farbe-Panel einen Offline-Hinweis. Ältere App-Versionen ignorieren die
/// Felder; v14-und-älter-Dateien laden unverändert (kein LUT).
/// v16: Geometrische Effekt-Masken (`clips[].effects[].masks` = Ellipse/
/// Rechteck/Polygon in normierten UVs, mit Feather, Invertierung, Deckkraft).
/// Begrenzen den Effekt auf eine Region; mehrere werden vereinigt. Ältere App-
/// Versionen ignorieren das Feld; v15-und-älter-Dateien laden unverändert
/// (Effekt wirkt aufs ganze Bild).
/// v17: Ebenen-Mischmodi (`clips[].blendMode` = Normal/Multiply/Screen/
/// Overlay/Add/...). W3C-Compositing-Formeln, formelgleich auf CPU und GPU.
/// `#[serde(default)]` ⇒ ältere Dateien laden als Normal (klassisches Src-over).
/// v18: Time-Remap — `clips[].speed` ist eine animierbare Kurve (AnimatedParam,
/// Keyframe-Zeiten in clip-lokalen Timeline-Sekunden). Statische Speed wird
/// weiter als blanke Zahl serialisiert ⇒ v5-Dateien (`"speed": 0.37`) laden
/// unverändert; nur Time-Remap-Clips schreiben das Objekt `{value, keyframes}`.
/// v19: Audio-Cleanup-Effekte De-Esser (`deEsser`) und Auto-Ducking
/// (`ducking`) im Effekt-Katalog (`clips[].effects[].kind`/`tracks[].effects`).
/// Rein additive `EffectKind`-Werte; ältere Dateien enthalten sie nie und laden
/// unverändert. Auto-Ducking nutzt als Sidechain-Key die Summe der anderen
/// Spuren — kein zusätzliches serialisiertes Feld.
/// v20: Adjustment Layer / Einstellungsebene (`clips[].adjustment`): ein
/// synthetischer Clip ohne Mediendatei, dessen `grade`/`effects` als
/// Korrektur-Pass auf das zusammengesetzte Bild ALLER darunterliegenden Spuren
/// wirken (Player == Export über den CPU-Compositing-Kern). Additiv mit
/// `#[serde(default)]`; ältere Dateien enthalten das Feld nie.
/// v21: Portable/relative Medienpfade (`portable`). Ein konsolidiertes Projekt
/// (Datei-Menü „Projekt konsolidieren …“) liegt mit seinen Medien in einem
/// gemeinsamen Ordner; ist `portable` gesetzt, schreibt der Speichervorgang
/// alle Medienpfade UNTERHALB des Projektordners RELATIV zur `.etron`-Datei
/// (`media/clip.mp4` statt `/abs/.../media/clip.mp4`). Beim Laden werden
/// relative Pfade gegen den Ordner der Projektdatei aufgelöst, sodass das
/// gesamte Projekt verschiebbar/archivierbar wird. Ältere App-Versionen würden
/// die relativen Pfade als Literale lesen und alle Medien offline melden,
/// deshalb der Versionssprung; v20-und-älter-Dateien (absolute Pfade) laden
/// unverändert (`portable` default = false).
pub const PROJECT_VERSION: u32 = 21;
const RECENT_LIMIT: usize = 10;

// ------------------------------------------------------------------- Format

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFile {
    /// Magic zum Erkennen fremder JSON-Dateien.
    pub format: String,
    /// Formatversion für Migrationen. Neuere Dateien werden best-effort geladen
    /// (Warn-Hinweis statt Abbruch); unbekannte Felder fängt `extra` auf.
    pub version: u32,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub saved_at_unix: f64,
    pub active_workspace: String,
    #[serde(default)]
    pub media: Vec<MediaAsset>,
    /// Bins (Ordner) der Medienverwaltung (ab Formatversion 9). `default`
    /// hält ältere Dateien lesbar; ohne Bins liegen alle Assets in der Wurzel.
    #[serde(default)]
    pub media_bins: Vec<crate::core::bin::Bin>,
    /// Ansichts-Zustand des Medien-Browsers (Modus, Sortierung, Spaltenbreiten,
    /// geöffneter Bin). `default` → Standardansicht.
    #[serde(default)]
    pub media_view: crate::core::bin::MediaViewState,
    /// Proxy-Workflow: globaler „Proxies verwenden“-Schalter (ab Formatversion
    /// 10). `default` ⇒ aus.
    #[serde(default)]
    pub use_proxies: bool,
    /// Proxy-Format/-Auflösung für neue Transcodes (ab Formatversion 10).
    #[serde(default)]
    pub proxy_settings: crate::core::proxy::ProxySettings,
    #[serde(default)]
    pub selected_asset_ids: Vec<String>,
    /// Portables Projekt (ab Formatversion 21): Medienpfade unterhalb des
    /// Projektordners werden RELATIV zur `.etron`-Datei gespeichert und beim
    /// Laden gegen deren Ordner aufgelöst. Wird von „Projekt konsolidieren“
    /// gesetzt. `default` = false ⇒ absolute Pfade (Altverhalten).
    #[serde(default)]
    pub portable: bool,
    /// Einzel-Timeline (Formatversionen ≤ 10). Ab v11 leer; der Loader
    /// bevorzugt `sequences`, wenn vorhanden, und liest dieses Feld nur als
    /// Altprojekt-Fallback (= genau eine Sequenz).
    #[serde(default, skip_serializing_if = "TimelineDoc::is_empty")]
    pub timeline: TimelineDoc,
    /// Alle Sequenzen des Projekts (ab Formatversion 11). Leer ⇒ Altprojekt,
    /// dann gilt `timeline` als einzige Sequenz.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sequences: Vec<SequenceDoc>,
    /// ID der aktiven Sequenz (ab v11). None ⇒ erste Sequenz.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_sequence_id: Option<String>,
    #[serde(default)]
    pub source_monitor: SourceMonitorDoc,
    /// Felder einer NEUEREN Editron-Version, die dieser Build noch nicht kennt.
    /// Sie werden beim Speichern unverändert wieder herausgeschrieben, damit ein
    /// älterer Build ein neueres Projekt öffnen, bearbeiten und speichern kann,
    /// ohne die neuen Felder stillschweigend zu verlieren (Vorwärtskompatibilität,
    /// siehe `AppSettings::extra` in `core::settings`).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Eine persistierte Sequenz: Identität + Bin-Zuordnung + Timeline-Dokument.
#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SequenceDoc {
    pub id: String,
    pub name: String,
    #[serde(default = "default_seq_bin")]
    pub bin_id: String,
    #[serde(default)]
    pub timeline: TimelineDoc,
    /// Unbekannte Felder einer neueren Version auf Sequenz-Ebene (siehe
    /// [`ProjectFile::extra`]).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn default_seq_bin() -> String {
    crate::core::bin::ROOT_BIN_ID.to_string()
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TimelineDoc {
    /// Sequenz-Einstellungen (ab Formatversion 2). None = Altprojekt:
    /// 25 fps, Auflösung wird beim Laden aus den Medien geraten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<crate::core::sequence::SequenceSettings>,
    #[serde(default)]
    pub tracks: Vec<TimelineTrack>,
    #[serde(default)]
    pub clips: Vec<TimelineClip>,
    /// Übergänge an Schnittkanten — `default` hält ältere Projektdateien
    /// (ohne Feld) lesbar; ältere App-Versionen ignorieren das Feld.
    #[serde(default)]
    pub transitions: Vec<Transition>,
    /// Sequenz-Marker — `default` hält ältere Projektdateien (ohne Feld)
    /// lesbar; ältere App-Versionen ignorieren das Feld.
    #[serde(default)]
    pub markers: Vec<crate::core::marker::Marker>,
    #[serde(default)]
    pub playhead_sec: f64,
    #[serde(default)]
    pub in_point: Option<f64>,
    #[serde(default)]
    pub out_point: Option<f64>,
    #[serde(default = "default_zoom")]
    pub zoom_px_per_sec: f64,
    #[serde(default = "default_true")]
    pub snapping: bool,
    #[serde(default)]
    pub selected_clip_ids: Vec<String>,
    /// Summen-Fader des Audio-Mixers in dB.
    #[serde(default)]
    pub master_gain_db: f64,
    /// Aktive Untertitel-Spur (Ziel von „Untertitel hinzufügen“/SRT-Export).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_subtitle_track_id: Option<String>,
    /// Multicam-Quelle (ab Formatversion 12): ist dies gesetzt, ist die Sequenz
    /// eine Multicam-Quelle mit Winkeln + Sync-Offsets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multicam: Option<crate::core::multicam::MulticamSource>,
    /// Unbekannte Felder einer neueren Version innerhalb des Timeline-Objekts
    /// (siehe [`ProjectFile::extra`]).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SourceMonitorDoc {
    #[serde(default)]
    pub asset_id: Option<String>,
    #[serde(default)]
    pub position: f64,
    #[serde(default)]
    pub in_mark: Option<f64>,
    #[serde(default)]
    pub out_mark: Option<f64>,
    #[serde(default)]
    pub looping: bool,
    /// Unbekannte Felder einer neueren Version im Quellmonitor-Objekt (siehe
    /// [`ProjectFile::extra`]).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl TimelineDoc {
    /// Leeres Timeline-Dokument (keine Spuren/Clips/Übergänge/Marker/Settings)
    /// — beim Speichern eines v11-Projekts wird das Alt-Feld `timeline` so
    /// weggelassen, weil alle Daten in `sequences` stehen.
    pub fn is_empty(&self) -> bool {
        self.sequence.is_none()
            && self.tracks.is_empty()
            && self.clips.is_empty()
            && self.transitions.is_empty()
            && self.markers.is_empty()
            && self.extra.is_empty()
    }
}

fn default_zoom() -> f64 {
    40.0
}

fn default_true() -> bool {
    true
}

// -------------------------------------------------------------- ProjectStore

/// Projekt-Zustand der Session: Dateipfad, Dirty-Flag und die persistierte
/// Zuletzt-geöffnet-Liste.
pub struct ProjectStore {
    pub path: Option<PathBuf>,
    pub dirty: bool,
    /// Portables Projekt: Medien liegen relativ zur `.etron` (konsolidiert).
    /// Steuert, ob [`save_to`] Medienpfade unterhalb des Projektordners relativ
    /// schreibt. Wird beim Laden aus der Datei übernommen und von „Projekt
    /// konsolidieren“ gesetzt.
    pub portable: bool,
    pub recent: Vec<String>,
    /// Unbekannte Top-Level-Felder der zuletzt geladenen Projektdatei (Felder
    /// einer neueren Version). Werden beim Speichern unverändert mitgeschrieben,
    /// damit ein älterer Build neuere Projekte nicht beschneidet.
    pub extra: Map<String, Value>,
    seen_timeline_rev: u64,
    seen_media_rev: u64,
}

impl Default for ProjectStore {
    fn default() -> Self {
        ProjectStore {
            path: None,
            dirty: false,
            portable: false,
            recent: load_recent(),
            extra: Map::new(),
            seen_timeline_rev: 0,
            seen_media_rev: 0,
        }
    }
}

impl ProjectStore {
    /// Anzeigename: Dateistamm oder „Unbenannt“.
    pub fn display_name(&self) -> String {
        self.path
            .as_deref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Unbenannt".to_string())
    }

    /// Pro Frame: strukturelle Änderungen an Timeline/Medien erkennen.
    pub fn track_changes(&mut self, timeline_rev: u64, media_rev: u64) {
        if timeline_rev != self.seen_timeline_rev || media_rev != self.seen_media_rev {
            self.seen_timeline_rev = timeline_rev;
            self.seen_media_rev = media_rev;
            self.dirty = true;
        }
    }

    /// Nach Speichern/Laden/Neu: aktueller Stand gilt als sauber.
    pub fn mark_clean(&mut self, timeline_rev: u64, media_rev: u64) {
        self.seen_timeline_rev = timeline_rev;
        self.seen_media_rev = media_rev;
        self.dirty = false;
    }

    pub fn push_recent(&mut self, path: &Path) {
        let entry = path.to_string_lossy().into_owned();
        self.recent.retain(|p| p != &entry);
        self.recent.insert(0, entry);
        self.recent.truncate(RECENT_LIMIT);
        save_recent(&self.recent);
    }

    pub fn remove_recent(&mut self, path: &str) {
        self.recent.retain(|p| p != path);
        save_recent(&self.recent);
    }
}

fn recent_path() -> PathBuf {
    // Override für Tests/portable Setups.
    if let Ok(dir) = std::env::var("EDITRON_CONFIG_DIR") {
        return PathBuf::from(dir).join("recent_projects.json");
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("editron")
        .join("recent_projects.json")
}

fn load_recent() -> Vec<String> {
    std::fs::read_to_string(recent_path())
        .ok()
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default()
}

fn save_recent(recent: &[String]) {
    let path = recent_path();
    if let Ok(json) = serde_json::to_string(recent) {
        // Atomar + fsync (siehe core::atomic_write).
        let _ = crate::core::atomic_write(&path, json.as_bytes());
    }
}

/// Ablageort des Sitzungs-Autosaves (ungespeicherte Projekte beim Beenden).
pub fn autosave_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("editron")
        .join("autosave.etron")
}

// ------------------------------------------------------ Portable Medienpfade

/// Einen Medienpfad RELATIV zum Projektordner machen, sofern er unterhalb davon
/// liegt — sonst unverändert lassen. Für portable (konsolidierte) Projekte:
/// `<ziel>/media/clip.mp4` mit base `<ziel>` ⇒ `media/clip.mp4`. Pfade außerhalb
/// des Projektordners (z. B. Thumbnails im App-Cache) bleiben absolut.
fn relativize_media_path(base: &Path, p: &str) -> String {
    let path = Path::new(p);
    if path.is_relative() {
        return p.to_string();
    }
    match path.strip_prefix(base) {
        // Auf '/' normieren, damit der Pfad plattformübergreifend portabel ist.
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => p.to_string(),
    }
}

/// Umkehrung: einen ggf. relativen Medienpfad gegen den Projektordner auflösen.
/// Absolute Pfade bleiben unverändert.
fn resolve_media_path(base: &Path, p: &str) -> String {
    let path = Path::new(p);
    if path.is_absolute() || p.is_empty() {
        return p.to_string();
    }
    base.join(path).to_string_lossy().into_owned()
}

/// Alle Medienpfade eines Projektdokuments relativ zum Projektordner schreiben
/// (nur Pfade unterhalb von `base`). Für portable Projekte vor dem
/// Serialisieren angewandt; der App-Zustand selbst bleibt absolut.
fn relativize_media(file: &mut ProjectFile, base: &Path) {
    for asset in &mut file.media {
        asset.path = relativize_media_path(base, &asset.path);
        asset.info.path = relativize_media_path(base, &asset.info.path);
        if let Some(t) = asset.thumbnail_path.take() {
            asset.thumbnail_path = Some(relativize_media_path(base, &t));
        }
        if let Some(pp) = asset.proxy_path.take() {
            asset.proxy_path = Some(relativize_media_path(base, &pp));
        }
    }
}

/// Umkehrung von [`relativize_media`] beim Laden: relative Medienpfade gegen den
/// Ordner der Projektdatei auflösen, sodass der App-Zustand wieder absolute
/// Pfade führt (Decoder/ffprobe brauchen absolute Pfade).
fn resolve_media(assets: &mut [MediaAsset], base: &Path) {
    for asset in assets {
        asset.path = resolve_media_path(base, &asset.path);
        asset.info.path = resolve_media_path(base, &asset.info.path);
        if let Some(t) = asset.thumbnail_path.take() {
            asset.thumbnail_path = Some(resolve_media_path(base, &t));
        }
        if let Some(pp) = asset.proxy_path.take() {
            asset.proxy_path = Some(resolve_media_path(base, &pp));
        }
    }
}

// -------------------------------------------------------------- Save / Load

/// Eine Timeline in ihr Persistenz-Dokument übersetzen. `extra` reicht die beim
/// Laden aufgefangenen Felder einer neueren Version unverändert wieder durch.
fn timeline_doc(t: &crate::core::timeline::TimelineStore, extra: &Map<String, Value>) -> TimelineDoc {
    TimelineDoc {
        sequence: Some(t.settings),
        tracks: t.tracks.clone(),
        clips: t.clips.clone(),
        transitions: t.transitions.clone(),
        markers: t.markers.clone(),
        playhead_sec: t.playhead_sec,
        in_point: t.in_point,
        out_point: t.out_point,
        zoom_px_per_sec: t.zoom_px_per_sec,
        snapping: t.snapping,
        selected_clip_ids: t.selected_clip_ids.clone(),
        master_gain_db: t.master_gain_db,
        active_subtitle_track_id: t.active_subtitle_track_id.clone(),
        multicam: t.multicam.clone(),
        extra: extra.clone(),
    }
}

/// Projektdaten aus dem App-Zustand einsammeln.
pub fn collect(state: &AppState) -> ProjectFile {
    let sequences: Vec<SequenceDoc> = state
        .timeline
        .iter()
        .map(|seq| SequenceDoc {
            id: seq.id.clone(),
            name: seq.name.clone(),
            bin_id: seq.bin_id.clone(),
            timeline: timeline_doc(&seq.timeline, &seq.timeline_extra),
            extra: seq.extra.clone(),
        })
        .collect();
    ProjectFile {
        format: PROJECT_FORMAT.to_string(),
        version: PROJECT_VERSION,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        saved_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0),
        active_workspace: state.app.active_workspace.clone(),
        media: state.media.assets.clone(),
        media_bins: state.media.bins.clone(),
        media_view: state.media.view.clone(),
        use_proxies: state.media.use_proxies,
        proxy_settings: state.media.proxy_settings.clone(),
        selected_asset_ids: state.media.selected_asset_ids.clone(),
        portable: state.project.portable,
        // Ab v11 leer (skip_serializing_if); alle Daten stehen in `sequences`.
        timeline: TimelineDoc::default(),
        sequences,
        active_sequence_id: Some(state.timeline.active_id().to_string()),
        source_monitor: SourceMonitorDoc {
            asset_id: state.playback.source_asset_id.clone(),
            position: state.playback.source.position,
            in_mark: state.playback.source.in_mark,
            out_mark: state.playback.source.out_mark,
            looping: state.playback.source.looping,
            extra: state.playback.source_extra.clone(),
        },
        extra: state.project.extra.clone(),
    }
}

/// Sorgt für die kanonische Endung `.etron` (Dialoge liefern teils ohne).
pub fn ensure_extension(path: PathBuf) -> PathBuf {
    match path.extension() {
        Some(ext) if ext.eq_ignore_ascii_case(PROJECT_EXT) => path,
        _ => path.with_extension(PROJECT_EXT),
    }
}

/// Atomar speichern: in Temp-Datei schreiben, bestehende Datei als .bak
/// sichern, dann rename. Eine halbe/korrupte Projektdatei gibt es so nie.
pub fn save_to(state: &mut AppState, path: &Path) -> Result<(), String> {
    let mut file = collect(state);
    // Portables Projekt: Medienpfade unterhalb des Projektordners relativ zur
    // `.etron` schreiben (der App-Zustand bleibt absolut). So bleibt das Projekt
    // samt `media/`-Ordner verschiebbar/archivierbar.
    if file.portable {
        if let Some(base) = path.parent() {
            if !base.as_os_str().is_empty() {
                relativize_media(&mut file, base);
            }
        }
    }
    let json = serde_json::to_string(&file).map_err(|e| format!("Serialisierung: {e}"))?;

    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| format!("Ordner anlegen: {e}"))?;
        }
    }
    // Bestehende Datei vor dem Überschreiben als .bak sichern (Original noch intakt).
    if path.exists() {
        let _ = std::fs::copy(path, path.with_extension(format!("{PROJECT_EXT}.bak")));
    }
    // Durabel + atomar: tmp schreiben → fsync → rename → dir-fsync. Ein
    // Stromausfall hinterlässt nie eine halbe/0-Byte-.etron.
    crate::core::atomic_write(path, json.as_bytes()).map_err(|e| format!("Speichern: {e}"))?;

    state.project.path = Some(path.to_path_buf());
    state.project.push_recent(path);
    let (t_rev, m_rev) = (state.timeline.aggregate_revision(), state.media.revision);
    state.project.mark_clean(t_rev, m_rev);
    Ok(())
}

/// Projektdatei lesen und validieren (Format-Magic, Deserialisierbarkeit).
///
/// Eine NEUERE Formatversion wird NICHT mehr hart abgelehnt: solange das
/// Basismodell deserialisierbar ist (alle bekannten Felder passen), lädt das
/// Projekt best-effort weiter. Unbekannte Felder fängt `#[serde(flatten)] extra`
/// auf jeder Ebene auf und schreibt sie beim Speichern unverändert zurück, sodass
/// ein älterer Build ein neueres Projekt bearbeiten kann, ohne neue Daten zu
/// verlieren. Der Warn-Hinweis auf die Versionsdifferenz wird in [`apply`] gesetzt.
pub fn load_from(path: &Path) -> Result<ProjectFile, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("{} konnte nicht gelesen werden: {e}", path.display()))?;
    let file: ProjectFile =
        serde_json::from_str(&raw).map_err(|e| format!("Keine gültige Projektdatei: {e}"))?;
    if file.format != PROJECT_FORMAT {
        return Err("Keine Editron-Projektdatei".to_string());
    }
    Ok(file)
}

/// Warn-Hinweis, falls das Projekt mit einer neueren Editron-Version gespeichert
/// wurde. `None`, wenn die Version bekannt ist.
fn newer_version_warning(version: u32) -> Option<String> {
    (version > PROJECT_VERSION).then(|| {
        format!(
            "Projekt mit einer neueren Editron-Version gespeichert (Format v{version} > v{PROJECT_VERSION}) — \
             unbekannte Felder bleiben beim Speichern erhalten."
        )
    })
}

/// Ein Timeline-Dokument in eine fertige [`TimelineStore`] laden: Inhalt
/// übernehmen, Altprojekt-Auflösung raten, Patch-/Target-Defaults setzen und
/// verwaiste Medien-Clips entfernen (Generatoren UND Nests bleiben erhalten).
fn load_timeline_doc(
    doc: TimelineDoc,
    version: u32,
    asset_ids: &std::collections::HashSet<String>,
    media: &crate::stores::MediaStore,
) -> crate::core::timeline::TimelineStore {
    let mut store = crate::core::timeline::TimelineStore::default();
    let legacy_sequence = doc.sequence.is_none();
    store.load_document(
        doc.sequence,
        doc.tracks,
        doc.clips,
        doc.transitions,
        doc.markers,
        doc.playhead_sec,
        doc.in_point,
        doc.out_point,
        doc.zoom_px_per_sec,
        doc.snapping,
        doc.selected_clip_ids,
        doc.master_gain_db,
        doc.active_subtitle_track_id,
    );
    store.multicam = doc.multicam;
    if legacy_sequence {
        let (w, h) = crate::core::export::suggested_resolution(&store, media);
        store.settings.width = w;
        store.settings.height = h;
    }
    if version < 7 {
        store.ensure_patch_target_defaults();
    }
    // Clips verwaister Assets entfernen (Asset aus der Datei gelöscht o. ä.).
    // Titel-/Untertitel-Generatoren, Nest- und Multicam-Clips haben kein
    // `asset_id` und bleiben erhalten.
    let orphans: Vec<String> = store
        .clips
        .iter()
        .filter(|c| {
            !c.is_generator()
                && !c.is_nest()
                && !c.is_multicam()
                && !asset_ids.contains(c.asset_id.as_str())
        })
        .map(|c| c.id.clone())
        .collect();
    if !orphans.is_empty() {
        store.clips.retain(|c| !orphans.contains(&c.id));
        store.transitions.retain(|t| {
            let gone = |id: &Option<String>| id.as_ref().is_some_and(|id| orphans.contains(id));
            !gone(&t.from_clip_id) && !gone(&t.to_clip_id)
        });
    }
    store
}

/// Geladenes Projekt in den App-Zustand übernehmen. Liefert die Anzahl
/// fehlender Medien (Offline-Check passiert hier).
pub fn apply(state: &mut AppState, mut file: ProjectFile, path: Option<PathBuf>) -> usize {
    // Unbekannte Felder einer neueren Version (Top-Level) für späteres
    // Wieder-Speichern aufbewahren; Warn-Hinweis bei neuerer Formatversion
    // setzen (best-effort statt Abbruch).
    let project_extra = std::mem::take(&mut file.extra);
    state.app.load_warning = newer_version_warning(file.version);
    // Geparste 3D-LUTs des Vorgängerprojekts verwerfen (Offline-Status der
    // neuen Referenzen wird beim ersten Panel-/Monitor-Zugriff frisch geprüft).
    state.luts = crate::core::lut::LutCache::default();
    state.lut_reload.clear();
    // Medien übernehmen + Offline-Status prüfen; verwaiste Thumbnails
    // (Cache geleert) nicht weiterreichen, damit sie neu entstehen können.
    let mut media = file.media;
    // Portable Projekte: relative Medienpfade gegen den Ordner der Projektdatei
    // auflösen, bevor Existenz/Decode-Pfade greifen (App-Zustand = absolut).
    if let Some(dir) = path.as_deref().and_then(|p| p.parent()) {
        if !dir.as_os_str().is_empty() {
            resolve_media(&mut media, dir);
        }
    }
    let mut offline = 0usize;
    for asset in &mut media {
        asset.offline = !Path::new(&asset.path).exists();
        if asset.offline {
            offline += 1;
        }
        if asset
            .thumbnail_path
            .as_deref()
            .is_some_and(|t| !Path::new(t).exists())
        {
            asset.thumbnail_path = None;
        }
    }
    let asset_ids: std::collections::HashSet<String> =
        media.iter().map(|a| a.id.clone()).collect();

    state.media.assets = media;
    state.media.bins = file.media_bins;
    state.media.view = file.media_view;
    state.media.view.sanitize();
    // Proxy-Workflow übernehmen + Proxys validieren (Existenz/Staleness →
    // `proxy_offline`). Fehlende/veraltete Proxys fallen so automatisch aufs
    // Original zurück.
    state.media.use_proxies = file.use_proxies;
    state.media.proxy_settings = file.proxy_settings;
    state.media.proxy_jobs.clear();
    state.media.revalidate_proxies();
    // Bin-Eltern, Asset-Bins und Navigation auf existierende Ziele bringen
    // (Zyklen → Wurzel); danach ist der Baum konsistent.
    state.media.reconcile_bins();
    state.media.clear_history();
    state.media.rename_request = None;
    state.media.scrub_thumbs.clear();
    state.media.selected_asset_ids = file
        .selected_asset_ids
        .into_iter()
        .filter(|id| asset_ids.contains(id.as_str()))
        .collect();
    state.media.waveforms.clear();
    state.media.importing = false;
    state.media.revision += 1;

    // Sequenzen aufbauen: ab v11 aus `sequences`, sonst Altprojekt mit genau
    // einer Sequenz aus dem alten `timeline`-Feld.
    use crate::core::sequences::{Sequence, SequenceStore};
    let mut sequences: Vec<Sequence> = Vec::new();
    if !file.sequences.is_empty() {
        for mut sd in file.sequences {
            // Extras VOR dem Konsumieren des Timeline-Dokuments sichern.
            let seq_extra = std::mem::take(&mut sd.extra);
            let tl_extra = std::mem::take(&mut sd.timeline.extra);
            let store = load_timeline_doc(sd.timeline, file.version, &asset_ids, &state.media);
            let bin_id = if state.media.bin_exists(&sd.bin_id) {
                sd.bin_id
            } else {
                crate::core::bin::ROOT_BIN_ID.to_string()
            };
            let name = if sd.name.trim().is_empty() {
                "Sequenz".to_string()
            } else {
                sd.name
            };
            let mut seq = Sequence::new(name, bin_id, store);
            if !sd.id.trim().is_empty() {
                seq.id = sd.id;
            }
            seq.extra = seq_extra;
            seq.timeline_extra = tl_extra;
            sequences.push(seq);
        }
    } else {
        // Altprojekt (v≤10): das einzige `timeline`-Dokument wird zur einzigen
        // Sequenz; etwaige unbekannte Felder wandern in deren `timeline_extra`.
        let tl_extra = std::mem::take(&mut file.timeline.extra);
        let store = load_timeline_doc(file.timeline, file.version, &asset_ids, &state.media);
        let mut seq = Sequence::new("Sequenz 01", crate::core::bin::ROOT_BIN_ID, store);
        seq.timeline_extra = tl_extra;
        sequences.push(seq);
    }
    // Verwaiste Nest-Verweise (auf nicht existierende Sequenzen) entfernen.
    let seq_ids: std::collections::HashSet<String> =
        sequences.iter().map(|s| s.id.clone()).collect();
    for seq in sequences.iter_mut() {
        let dangling: Vec<String> = seq
            .timeline
            .clips
            .iter()
            .filter_map(|c| c.nest_seq.clone())
            .filter(|n| !seq_ids.contains(n))
            .collect();
        for d in dangling {
            seq.timeline.remove_nest_clips_of(&d);
        }
        // Multicam-Clips verwaister Quellen (Quell-Sequenz fehlt) entfernen.
        let gone: Vec<String> = seq
            .timeline
            .clips
            .iter()
            .filter(|c| {
                c.multicam
                    .as_ref()
                    .is_some_and(|m| !seq_ids.contains(&m.source))
            })
            .map(|c| c.id.clone())
            .collect();
        if !gone.is_empty() {
            seq.timeline.clips.retain(|c| !gone.contains(&c.id));
        }
    }
    state.timeline = SequenceStore::from_sequences(sequences, file.active_sequence_id.as_deref());

    let sm = file.source_monitor;
    state.playback = Default::default();
    state.playback.source_extra = sm.extra;
    state.playback.source_asset_id = sm.asset_id.filter(|id| asset_ids.contains(id.as_str()));
    if state.playback.source_asset_id.is_some() {
        state.playback.source.position = sm.position.max(0.0);
        state.playback.source.in_mark = sm.in_mark;
        state.playback.source.out_mark = sm.out_mark;
        state.playback.source.looping = sm.looping;
    }

    if crate::stores::WORKSPACE_IDS.contains(&file.active_workspace.as_str()) {
        crate::state::set_active_workspace(state, &file.active_workspace);
    }

    state.project.path = path.clone();
    state.project.portable = file.portable;
    state.project.extra = project_extra;
    if let Some(p) = &path {
        state.project.push_recent(p);
    }
    let (t_rev, m_rev) = (state.timeline.aggregate_revision(), state.media.revision);
    state.project.mark_clean(t_rev, m_rev);
    offline
}

/// Projekt von Pfad laden und übernehmen; öffnet bei fehlenden Medien
/// direkt den Relink-Wizard. Liefert die Anzahl fehlender Medien.
pub fn open_into(state: &mut AppState, path: &Path) -> Result<usize, String> {
    let file = load_from(path)?;
    let offline = apply(state, file, Some(path.to_path_buf()));
    if offline > 0 {
        state.app.open_dialog = Some(crate::stores::DialogId::Relink);
    }
    Ok(offline)
}

/// Frisches, leeres Projekt (Workspace bleibt erhalten).
pub fn reset_to_new(state: &mut AppState) {
    state.media = Default::default();
    state.media.revision += 1;
    state.timeline = Default::default();
    state.timeline.revision += 1;
    state.playback = Default::default();
    state.project.path = None;
    state.project.portable = false;
    // Unbekannte Felder eines zuvor geladenen (neueren) Projekts dürfen nicht
    // ins frische Projekt durchsickern.
    state.project.extra.clear();
    let (t_rev, m_rev) = (state.timeline.aggregate_revision(), state.media.revision);
    state.project.mark_clean(t_rev, m_rev);
}

/// Ungespeicherte Änderungen sichern, bevor das Projekt gewechselt wird:
/// mit Pfad → normales Speichern; ohne Pfad → Sitzungs-Autosave.
/// Liefert eine Statusmeldung, wenn ein Autosave geschrieben wurde.
pub fn safeguard_unsaved(state: &mut AppState) -> Option<String> {
    if !state.project.dirty {
        return None;
    }
    if let Some(path) = state.project.path.clone() {
        if let Err(err) = save_to(state, &path) {
            return Some(format!("Automatisches Speichern fehlgeschlagen: {err}"));
        }
        return None;
    }
    // Nichts zu sichern, wenn das Projekt faktisch leer ist (keine Medien und
    // keine einzige Sequenz mit Clips).
    let any_clips = state.timeline.iter().any(|s| !s.timeline.clips.is_empty());
    if state.media.assets.is_empty() && !any_clips {
        return None;
    }
    let path = autosave_path();
    match save_to(state, &path) {
        Ok(()) => {
            // Autosave ist kein „echtes“ Projekt: Pfad und Recent-Eintrag
            // nicht behalten.
            state.project.path = None;
            let entry = path.to_string_lossy().into_owned();
            state.project.remove_recent(&entry);
            Some("Ungespeichertes Projekt als Sitzungs-Autosave gesichert".to_string())
        }
        Err(err) => Some(format!("Autosave fehlgeschlagen: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sequence::{FrameRate, SequenceSettings};
    use crate::core::timeline::{TrackKind, MIN_CLIP_DURATION};

    /// Recent-Liste in ein Test-Verzeichnis umlenken, damit Tests nie die
    /// echte Nutzer-Config anfassen. Alle Tests setzen denselben Wert —
    /// der Race zwischen parallelen Tests ist damit harmlos.
    fn isolate_config() {
        let dir = std::env::temp_dir().join(format!("editron-test-config-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("EDITRON_CONFIG_DIR", &dir);
    }

    fn sample_state() -> AppState {
        let mut state = AppState::default();
        let asset = MediaAsset {
            extra: Default::default(),
            id: "asset-1".into(),
            path: "/tmp/editron-test/clip.mp4".into(),
            name: "clip.mp4".into(),
            kind: crate::core::types::MediaKind::Video,
            info: crate::core::types::MediaInfo {
                path: "/tmp/editron-test/clip.mp4".into(),
                file_name: "clip.mp4".into(),
                container: "mov,mp4".into(),
                duration_sec: 12.0,
                size_bytes: 1234,
                video: Vec::new(),
                audio: Vec::new(),
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
        };
        state.media.add_asset(asset);
        let track_id = state.timeline.tracks[0].id.clone();
        state.timeline.clips.push(TimelineClip {
            extra: Default::default(),
            id: "clip-1".into(),
            track_id,
            asset_id: "asset-1".into(),
            name: "clip.mp4".into(),
            kind: TrackKind::Video,
            start: 1.0,
            duration: 5.0,
            src_in: 0.5,
            src_duration: 12.0,
            link_id: None,
            enabled: true,
            gain_db: -3.0,
            fx: {
                // Animierte Parameter müssen den Roundtrip überleben.
                let mut fx = crate::core::animation::ClipFx::default();
                fx.pos_x.upsert_key(0.5, -20.0);
                fx.pos_x.upsert_key(4.5, 20.0);
                fx.pos_x.keyframes[0].interp = crate::core::animation::Interp::EaseInOut;
                fx.opacity.value = 80.0;
                fx
            },
            grade: {
                // Farbkorrektur muss den Roundtrip überleben.
                let mut g = crate::core::grade::ColorGrade::default();
                g.temperature = 25.0;
                g.look = crate::core::grade::GradeLook::TealOrange;
                g.gain = crate::core::grade::WheelValue { x: 0.3, y: -0.1, luma: 0.05 };
                g.vignette_amount = 30.0;
                g.curves.luma = crate::core::grade::Curve {
                    points: vec![
                        crate::core::grade::CurvePoint { x: 0.0, y: 0.02 },
                        crate::core::grade::CurvePoint { x: 0.5, y: 0.62 },
                        crate::core::grade::CurvePoint { x: 1.0, y: 0.97 },
                    ],
                };
                g
            },
            effects: {
                // Effekt-Stapel inkl. Keyframes muss den Roundtrip überleben.
                let mut blur =
                    crate::core::effects::EffectInstance::new(crate::core::effects::EffectKind::GaussianBlur);
                blur.params[0].upsert_key(0.5, 0.0);
                blur.params[0].upsert_key(4.5, 60.0);
                // Effekt-Masken (Ellipse + invertiertes Polygon) müssen den
                // Roundtrip überleben.
                let mut ell = crate::core::mask::Mask::new(crate::core::mask::MaskShape::Ellipse);
                ell.center = [0.4, 0.6];
                ell.radius = [0.25, 0.18];
                ell.rotation = 12.0;
                ell.feather = 0.08;
                let mut poly = crate::core::mask::Mask::new(crate::core::mask::MaskShape::Polygon);
                poly.points = vec![[0.1, 0.1], [0.9, 0.2], [0.7, 0.8]];
                poly.inverted = true;
                poly.opacity = 0.5;
                blur.masks = vec![ell, poly];
                let mut key =
                    crate::core::effects::EffectInstance::new(crate::core::effects::EffectKind::ChromaKey);
                key.enabled = false;
                vec![blur, key]
            },
            title: None,
            subtitle: None,
            adjustment: None,
            speed: crate::core::animation::AnimatedParam::fixed(1.0),
            reverse: false,
            freeze: false,
            markers: Vec::new(),
            nest_seq: None,
            multicam: None,
            blend_mode: crate::core::compose::BlendMode::default(),
        });
        // Standbild mit unendlicher Quelldauer (Infinity-Roundtrip).
        let track_id = state.timeline.tracks[1].id.clone();
        state.timeline.clips.push(TimelineClip {
            extra: Default::default(),
            id: "clip-2".into(),
            track_id,
            asset_id: "asset-1".into(),
            name: "still.png".into(),
            kind: TrackKind::Video,
            start: 6.0,
            duration: 5.0,
            src_in: 0.0,
            src_duration: f64::INFINITY,
            link_id: None,
            enabled: false,
            gain_db: 0.0,
            fx: Default::default(),
            grade: Default::default(),
            effects: Vec::new(),
            title: None,
            subtitle: None,
            adjustment: None,
            speed: crate::core::animation::AnimatedParam::fixed(1.0),
            reverse: false,
            freeze: false,
            markers: Vec::new(),
            nest_seq: None,
            multicam: None,
            blend_mode: crate::core::compose::BlendMode::default(),
        });
        // Rückwärts-Clip mit 37 % muss den Roundtrip exakt überleben.
        let track_id = state.timeline.tracks[0].id.clone();
        state.timeline.clips.push(TimelineClip {
            extra: Default::default(),
            id: "clip-3".into(),
            track_id,
            asset_id: "asset-1".into(),
            name: "speed.mp4".into(),
            kind: TrackKind::Video,
            start: 12.0,
            duration: 3.0,
            src_in: 1.0,
            src_duration: 12.0,
            link_id: None,
            enabled: true,
            gain_db: 0.0,
            fx: Default::default(),
            grade: Default::default(),
            effects: Vec::new(),
            title: None,
            subtitle: None,
            adjustment: None,
            speed: crate::core::animation::AnimatedParam::fixed(0.37),
            reverse: true,
            freeze: false,
            markers: Vec::new(),
            nest_seq: None,
            multicam: None,
            blend_mode: crate::core::compose::BlendMode::default(),
        });
        // Übergang (Einblenden auf clip-1) muss den Roundtrip überleben.
        state.timeline.transitions.push({
            let mut tr = crate::core::transitions::Transition::new(
                crate::core::transitions::TransitionKind::Wipe,
                None,
                Some("clip-1".into()),
                1.5,
            );
            tr.direction = crate::core::transitions::TransitionDirection::Down;
            tr
        });
        state.timeline.playhead_sec = 3.25;
        state.timeline.in_point = Some(1.0);
        state.timeline.out_point = Some(9.0);
        state.timeline.master_gain_db = -4.5;
        state.timeline.tracks[2].gain_db = 2.0;
        state.timeline.tracks[2].pan = -0.5;
        // NTSC-Sequenz mit Drop-Frame muss den Roundtrip exakt überleben.
        state.timeline.settings = SequenceSettings {
            rate: FrameRate::new(30000, 1001),
            width: 1280,
            height: 720,
            drop_frame: true,
        };
        // Marker (Sequenz/Clip/Asset) müssen den Roundtrip überleben.
        {
            use crate::core::marker::{Marker, MarkerColor};
            let mut m = Marker::new(2.0);
            m.name = "Intro".into();
            m.note = "Schnittidee".into();
            m.color = MarkerColor::Red;
            state.timeline.markers.push(m);
            let mut range = Marker::new(8.0);
            range.duration = 1.0; // Bereichsmarker
            range.color = MarkerColor::Cyan;
            state.timeline.markers.push(range);
            if let Some(c) = state.timeline.clips.iter_mut().find(|c| c.id == "clip-1") {
                let mut cm = Marker::new(1.5);
                cm.name = "Beat".into();
                c.markers.push(cm);
            }
            if let Some(a) = state.media.assets.iter_mut().find(|a| a.id == "asset-1") {
                a.markers.push(Marker::new(4.0));
            }
        }
        state
    }

    #[test]
    fn roundtrip_preserves_project() {
        isolate_config();
        let mut state = sample_state();
        let dir = std::env::temp_dir().join(format!("editron-proj-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.etron");

        save_to(&mut state, &path).expect("save");
        assert!(!state.project.dirty);

        let file = load_from(&path).expect("load");
        assert_eq!(file.format, PROJECT_FORMAT);
        assert_eq!(file.version, PROJECT_VERSION);
        assert_eq!(file.media.len(), 1);
        assert_eq!(file.sequences[0].timeline.clips.len(), 3);
        assert_eq!(file.sequences[0].timeline.playhead_sec, 3.25);
        // Clip-Geschwindigkeit exakt erhalten (rückwärts, 37 %).
        let speedy = file.sequences[0].timeline.clips.iter().find(|c| c.id == "clip-3").unwrap();
        assert_eq!(speedy.eff_speed(), 0.37);
        assert!(speedy.reverse);
        assert!(!speedy.freeze);
        // Normale Clips bleiben bei 100 % vorwärts.
        assert_eq!(file.sequences[0].timeline.clips[0].eff_speed(), 1.0);
        assert!(!file.sequences[0].timeline.clips[0].reverse);
        assert_eq!(file.sequences[0].timeline.in_point, Some(1.0));
        assert!(file.sequences[0].timeline.clips[1].src_duration.is_infinite());
        assert!(!file.sequences[0].timeline.clips[1].enabled);
        assert_eq!(file.sequences[0].timeline.master_gain_db, -4.5);
        assert_eq!(file.sequences[0].timeline.clips[0].gain_db, -3.0);
        let fx = &file.sequences[0].timeline.clips[0].fx;
        assert_eq!(fx.pos_x.keyframes.len(), 2);
        assert_eq!(fx.pos_x.keyframes[0].interp, crate::core::animation::Interp::EaseInOut);
        assert_eq!(fx.opacity.value, 80.0);
        assert!(fx.pos_x.is_animated());
        let g = &file.sequences[0].timeline.clips[0].grade;
        assert_eq!(g.temperature, 25.0);
        assert_eq!(g.look, crate::core::grade::GradeLook::TealOrange);
        assert_eq!(g.gain.x, 0.3);
        assert_eq!(g.vignette_amount, 30.0);
        assert_eq!(g.curves.luma.points.len(), 3);
        assert_eq!(g.curves.luma.points[1].y, 0.62);
        assert!(g.curves.red.is_identity());
        // Effekt-Masken vollständig erhalten (Ellipse + invertiertes Polygon).
        let masks = &file.sequences[0].timeline.clips[0].effects[0].masks;
        assert_eq!(masks.len(), 2);
        assert_eq!(masks[0].shape, crate::core::mask::MaskShape::Ellipse);
        assert_eq!(masks[0].center, [0.4, 0.6]);
        assert_eq!(masks[0].radius, [0.25, 0.18]);
        assert_eq!(masks[0].rotation, 12.0);
        assert_eq!(masks[0].feather, 0.08);
        assert_eq!(masks[1].shape, crate::core::mask::MaskShape::Polygon);
        assert_eq!(masks[1].points.len(), 3);
        assert!(masks[1].inverted);
        assert_eq!(masks[1].opacity, 0.5);
        // Unmaskierter Effekt (ChromaKey) trägt keine Masken.
        assert!(file.sequences[0].timeline.clips[0].effects[1].masks.is_empty());
        // Unveränderte Clips speichern kein fx-/grade-Feld (schlanke Datei).
        assert!(file.sequences[0].timeline.clips[1].fx.is_default());
        assert!(file.sequences[0].timeline.clips[1].grade.is_default());
        assert_eq!(file.sequences[0].timeline.tracks[2].gain_db, 2.0);
        assert_eq!(file.sequences[0].timeline.tracks[2].pan, -0.5);
        // Übergang vollständig erhalten.
        assert_eq!(file.sequences[0].timeline.transitions.len(), 1);
        let tr = &file.sequences[0].timeline.transitions[0];
        assert_eq!(tr.kind, crate::core::transitions::TransitionKind::Wipe);
        assert_eq!(tr.direction, crate::core::transitions::TransitionDirection::Down);
        assert_eq!(tr.to_clip_id.as_deref(), Some("clip-1"));
        assert_eq!(tr.duration, 1.5);

        // Sequenz-Einstellungen exakt erhalten (NTSC-Bruch, kein Float).
        let seq = file.sequences[0].timeline.sequence.expect("Sequenz-Einstellungen gespeichert");
        assert_eq!(seq.rate, FrameRate::new(30000, 1001));
        assert_eq!((seq.width, seq.height), (1280, 720));
        assert!(seq.drop_frame);

        // Marker (Sequenz/Clip/Asset) vollständig erhalten.
        use crate::core::marker::MarkerColor;
        assert_eq!(file.sequences[0].timeline.markers.len(), 2);
        let intro = file.sequences[0].timeline.markers.iter().find(|m| m.name == "Intro").unwrap();
        assert_eq!(intro.color, MarkerColor::Red);
        assert_eq!(intro.note, "Schnittidee");
        assert!(file.sequences[0].timeline.markers.iter().any(|m| (m.duration - 1.0).abs() < 1e-9));
        let c1 = file.sequences[0].timeline.clips.iter().find(|c| c.id == "clip-1").unwrap();
        assert_eq!(c1.markers.len(), 1);
        assert_eq!(c1.markers[0].name, "Beat");
        assert_eq!(file.media[0].markers.len(), 1);

        let mut target = AppState::default();
        let offline = apply(&mut target, file, Some(path.clone()));
        // Quelldatei existiert nicht → offline erkannt.
        assert_eq!(offline, 1);
        assert!(target.media.assets[0].offline);
        assert_eq!(target.timeline.settings.rate, FrameRate::new(30000, 1001));
        assert!(target.timeline.settings.drop_frame);
        assert_eq!(target.timeline.clips.len(), 3);
        assert_eq!(target.timeline.clips[0].start, 1.0);
        assert_eq!(target.timeline.clip("clip-3").unwrap().eff_speed(), 0.37);
        assert!(target.timeline.clip("clip-3").unwrap().reverse);
        assert_eq!(target.timeline.transitions.len(), 1, "Übergang geladen");
        assert_eq!(target.timeline.master_gain_db, -4.5);
        assert_eq!(target.timeline.tracks[2].pan, -0.5);
        // Marker nach dem Laden vorhanden (sortiert, Clip-/Asset-Marker).
        assert_eq!(target.timeline.markers.len(), 2);
        assert!(target.timeline.markers[0].time < target.timeline.markers[1].time);
        assert_eq!(target.timeline.clip("clip-1").unwrap().markers.len(), 1);
        assert_eq!(target.media.assets[0].markers.len(), 1);
        assert!(!target.project.dirty);
        assert_eq!(target.project.display_name(), "test");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_is_atomic_and_keeps_backup() {
        isolate_config();
        let mut state = sample_state();
        let dir = std::env::temp_dir().join(format!("editron-proj-bak-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.etron");
        save_to(&mut state, &path).unwrap();
        state.timeline.set_playhead(7.0);
        save_to(&mut state, &path).unwrap();
        assert!(path.exists());
        assert!(dir.join("p.etron.bak").exists());
        // Keine Temp-Reste
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relativize_and_resolve_media_paths_roundtrip() {
        let base = Path::new("/projects/film");
        // Unterhalb des Projektordners → relativ (mit '/'-Trennern).
        assert_eq!(
            relativize_media_path(base, "/projects/film/media/clip.mp4"),
            "media/clip.mp4"
        );
        // Außerhalb → unverändert absolut.
        assert_eq!(
            relativize_media_path(base, "/cache/thumbs/a.png"),
            "/cache/thumbs/a.png"
        );
        // Bereits relativ → unverändert.
        assert_eq!(relativize_media_path(base, "media/clip.mp4"), "media/clip.mp4");
        // Auflösen kehrt das wieder um (absolut bleibt absolut).
        assert_eq!(
            resolve_media_path(base, "media/clip.mp4"),
            "/projects/film/media/clip.mp4"
        );
        assert_eq!(
            resolve_media_path(base, "/cache/thumbs/a.png"),
            "/cache/thumbs/a.png"
        );
    }

    /// Ein portables Projekt schreibt Medienpfade relativ; nach dem Verschieben
    /// des Projektordners sind alle Medien wieder online (gegen den neuen Ordner
    /// aufgelöst). Das ist die Kern-Verifikation der Konsolidierung.
    #[test]
    fn portable_project_relinks_after_move() {
        isolate_config();
        let root = std::env::temp_dir().join(format!("editron-portable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(src.join("media")).unwrap();
        // Echte Mediendatei, damit der Offline-Check greift.
        let media_file = src.join("media").join("clip.mp4");
        std::fs::write(&media_file, b"\x00\x00\x00fake").unwrap();

        // Minimaler Zustand mit einem Asset im Projektordner.
        let mut state = AppState::default();
        let abs = media_file.to_string_lossy().into_owned();
        let asset = MediaAsset {
            extra: Default::default(),
            id: "a1".into(),
            path: abs.clone(),
            name: "clip.mp4".into(),
            kind: crate::core::types::MediaKind::Video,
            info: crate::core::types::MediaInfo {
                path: abs.clone(),
                file_name: "clip.mp4".into(),
                container: "mov,mp4".into(),
                duration_sec: 5.0,
                size_bytes: 7,
                video: Vec::new(),
                audio: Vec::new(),
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
        };
        state.media.add_asset(asset);
        state.project.portable = true;

        let etron = src.join("film.etron");
        save_to(&mut state, &etron).expect("save portable");

        // Datei trägt RELATIVE Medienpfade (verschiebbar).
        let raw = std::fs::read_to_string(&etron).unwrap();
        assert!(
            raw.contains("\"path\":\"media/clip.mp4\""),
            "Medienpfad relativ gespeichert, war: {raw}"
        );
        assert!(!raw.contains(&abs), "kein absoluter Medienpfad in der Datei");

        // Ganzen Projektordner verschieben (Übergabe/Archiv).
        let moved = root.join("moved");
        std::fs::rename(&src, &moved).unwrap();
        let moved_etron = moved.join("film.etron");

        // Laden gegen den NEUEN Ordner: Medium ist online und absolut.
        let mut target = AppState::default();
        let file = load_from(&moved_etron).expect("load moved");
        assert!(file.portable);
        let offline = apply(&mut target, file, Some(moved_etron.clone()));
        assert_eq!(offline, 0, "Medium nach Verschieben online");
        assert!(target.project.portable);
        let resolved = moved.join("media").join("clip.mp4");
        assert_eq!(
            target.media.assets[0].path,
            resolved.to_string_lossy(),
            "Pfad gegen neuen Ordner aufgelöst"
        );
        assert!(!target.media.assets[0].offline);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Ohne portable-Flag bleiben Pfade absolut (Altverhalten, abwärtskompatibel).
    #[test]
    fn non_portable_save_keeps_absolute_paths() {
        isolate_config();
        let dir = std::env::temp_dir().join(format!("editron-abspath-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("media")).unwrap();
        let media_file = dir.join("media").join("x.mp4");
        std::fs::write(&media_file, b"data").unwrap();
        let abs = media_file.to_string_lossy().into_owned();

        let mut state = AppState::default();
        let mut asset = sample_asset();
        asset.path = abs.clone();
        asset.info.path = abs.clone();
        state.media.add_asset(asset);
        // portable bleibt false (Default).

        let etron = dir.join("p.etron");
        save_to(&mut state, &etron).unwrap();
        let raw = std::fs::read_to_string(&etron).unwrap();
        assert!(raw.contains(&abs), "absoluter Pfad bleibt erhalten");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Minimal-Asset für Pfad-Tests (Felder, die der Roundtrip nicht prüft).
    fn sample_asset() -> MediaAsset {
        MediaAsset {
            extra: Default::default(),
            id: crate::core::types::new_id(),
            path: String::new(),
            name: "x.mp4".into(),
            kind: crate::core::types::MediaKind::Video,
            info: crate::core::types::MediaInfo {
                path: String::new(),
                file_name: "x.mp4".into(),
                container: "mov,mp4".into(),
                duration_sec: 3.0,
                size_bytes: 4,
                video: Vec::new(),
                audio: Vec::new(),
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

    #[test]
    fn rejects_foreign_but_loads_newer_best_effort() {
        isolate_config();
        let dir = std::env::temp_dir().join(format!("editron-proj-rej-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Fremd-JSON ohne `format`-Feld: nicht deserialisierbar → abgelehnt.
        let foreign = dir.join("foreign.etron");
        std::fs::write(&foreign, r#"{"hello": 1}"#).unwrap();
        assert!(load_from(&foreign).is_err());
        // Korrekte Struktur, aber fremdes Magic → abgelehnt.
        let wrong_magic = dir.join("magic.etron");
        std::fs::write(&wrong_magic, r#"{"format":"nope","version":1,"activeWorkspace":"edit"}"#)
            .unwrap();
        assert!(load_from(&wrong_magic).is_err());

        // Neuere Formatversion: NICHT mehr abgelehnt, sondern best-effort geladen.
        // Das unbekannte Top-Level-Feld landet in `extra`.
        let newer = dir.join("newer.etron");
        std::fs::write(
            &newer,
            format!(
                r#"{{"format":"{PROJECT_FORMAT}","version":{},"activeWorkspace":"edit","futureField":7}}"#,
                PROJECT_VERSION + 1
            ),
        )
        .unwrap();
        let file = load_from(&newer).expect("neuere Version lädt best-effort");
        assert_eq!(file.version, PROJECT_VERSION + 1);
        assert_eq!(file.extra.get("futureField"), Some(&serde_json::json!(7)));

        // apply lädt den Best-Effort-Stand und setzt den Warn-Hinweis.
        let mut state = AppState::default();
        apply(&mut state, file, Some(newer.clone()));
        assert!(
            state.app.load_warning.is_some(),
            "neuere Version setzt einen Warn-Hinweis"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_future_fields_survive_load_edit_save() {
        use serde_json::json;
        isolate_config();
        // Gültiges Projekt der AKTUELLEN Version als Ausgangsbasis schreiben.
        let mut state = sample_state();
        let dir = std::env::temp_dir().join(format!("editron-proj-future-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("future.etron");
        save_to(&mut state, &path).expect("save");

        // Die Datei so anreichern, als hätte sie ein NEUERER Editron-Build
        // geschrieben: höhere Version + unbekannte Felder auf jeder Ebene.
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        {
            let obj = v.as_object_mut().unwrap();
            obj.insert("version".into(), json!(PROJECT_VERSION + 5));
            obj.insert("futureGlobalOption".into(), json!({ "hdr": true, "nits": 1000 }));
            let seqs = obj.get_mut("sequences").unwrap().as_array_mut().unwrap();
            let s0 = seqs[0].as_object_mut().unwrap();
            s0.insert("futureSeqField".into(), json!("hallo"));
            let tl = s0.get_mut("timeline").unwrap().as_object_mut().unwrap();
            tl.insert("futureTimelineField".into(), json!([1, 2, 3]));
            obj.get_mut("sourceMonitor")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("futureSourceField".into(), json!(true));
        }
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();

        // Älterer Build: best-effort laden — die Felder landen in `extra`.
        let file = load_from(&path).expect("neuere Version lädt best-effort");
        assert_eq!(file.version, PROJECT_VERSION + 5);
        assert_eq!(file.extra.get("futureGlobalOption"), Some(&json!({ "hdr": true, "nits": 1000 })));
        assert_eq!(file.sequences[0].extra.get("futureSeqField"), Some(&json!("hallo")));
        assert_eq!(
            file.sequences[0].timeline.extra.get("futureTimelineField"),
            Some(&json!([1, 2, 3]))
        );
        assert_eq!(file.source_monitor.extra.get("futureSourceField"), Some(&json!(true)));

        // In den App-Zustand laden (Warn-Hinweis), "bearbeiten" und neu speichern.
        let mut edited = AppState::default();
        apply(&mut edited, file, Some(path.clone()));
        assert!(edited.app.load_warning.is_some(), "Warn-Hinweis für neuere Version");
        edited.timeline.set_playhead(2.0); // irgendeine Bearbeitung
        save_to(&mut edited, &path).expect("re-save");

        // Erneut laden: unsere Version steht drin, ABER alle unbekannten Felder
        // sind verlustfrei erhalten geblieben.
        let reloaded = load_from(&path).expect("reload");
        assert_eq!(reloaded.version, PROJECT_VERSION, "auf unsere Version zurückgeschrieben");
        assert_eq!(
            reloaded.extra.get("futureGlobalOption"),
            Some(&json!({ "hdr": true, "nits": 1000 }))
        );
        assert_eq!(reloaded.sequences[0].extra.get("futureSeqField"), Some(&json!("hallo")));
        assert_eq!(
            reloaded.sequences[0].timeline.extra.get("futureTimelineField"),
            Some(&json!([1, 2, 3]))
        );
        assert_eq!(reloaded.source_monitor.extra.get("futureSourceField"), Some(&json!(true)));
        // Reload eines Projekts UNSERER Version setzt keinen Warn-Hinweis mehr.
        let mut fresh = AppState::default();
        apply(&mut fresh, reloaded, Some(path.clone()));
        assert!(fresh.app.load_warning.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_per_element_fields_survive_roundtrip() {
        use serde_json::json;
        isolate_config();
        // Ausgangsbasis mit Asset, Clips und Spuren.
        let mut state = sample_state();
        let dir = std::env::temp_dir().join(format!("editron-proj-elem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("elem.etron");
        save_to(&mut state, &path).expect("save");

        // Unbekannte Felder an EINEM Asset, EINEM Clip und EINER Spur anbringen
        // (als hätte sie ein neuerer Build geschrieben).
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        {
            let obj = v.as_object_mut().unwrap();
            let asset = obj.get_mut("media").unwrap().as_array_mut().unwrap()[0]
                .as_object_mut()
                .unwrap();
            asset.insert("futureAssetField".into(), json!({ "lut": "aces" }));
            let tl = obj.get_mut("sequences").unwrap().as_array_mut().unwrap()[0]
                .as_object_mut()
                .unwrap()
                .get_mut("timeline")
                .unwrap()
                .as_object_mut()
                .unwrap();
            // Clip „clip-1" anreichern.
            let clip = tl
                .get_mut("clips")
                .unwrap()
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|c| c["id"] == json!("clip-1"))
                .unwrap()
                .as_object_mut()
                .unwrap();
            clip.insert("futureClipField".into(), json!(99));
            // Erste Spur anreichern.
            tl.get_mut("tracks").unwrap().as_array_mut().unwrap()[0]
                .as_object_mut()
                .unwrap()
                .insert("futureTrackField".into(), json!([true, false]));
        }
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();

        // Laden → die Felder sitzen im jeweiligen In-Memory-Struct.
        let file = load_from(&path).expect("load");
        let mut edited = AppState::default();
        apply(&mut edited, file, Some(path.clone()));
        assert_eq!(
            edited.media.asset("asset-1").unwrap().extra.get("futureAssetField"),
            Some(&json!({ "lut": "aces" }))
        );
        assert_eq!(
            edited.timeline.clip("clip-1").unwrap().extra.get("futureClipField"),
            Some(&json!(99))
        );
        assert_eq!(
            edited.timeline.tracks[0].extra.get("futureTrackField"),
            Some(&json!([true, false]))
        );

        // Bearbeiten + neu speichern. `collect` klont alle Clips/Spuren/Assets,
        // `Clone` trägt `extra` mit — daher überlebt es ohne Sonderbehandlung.
        edited.timeline.set_playhead(3.0);
        save_to(&mut edited, &path).expect("re-save");

        // Erneut laden: per-Element-Felder verlustfrei erhalten.
        let reloaded = load_from(&path).expect("reload");
        let tl = &reloaded.sequences[0].timeline;
        assert_eq!(
            reloaded.media.iter().find(|a| a.id == "asset-1").unwrap().extra.get("futureAssetField"),
            Some(&json!({ "lut": "aces" }))
        );
        assert_eq!(
            tl.clips.iter().find(|c| c.id == "clip-1").unwrap().extra.get("futureClipField"),
            Some(&json!(99))
        );
        assert_eq!(
            tl.tracks[0].extra.get("futureTrackField"),
            Some(&json!([true, false]))
        );
        // Unveränderte Elemente tragen weiterhin KEIN extra (keine Pollution).
        assert!(tl.clips.iter().find(|c| c.id == "clip-2").unwrap().extra.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_document_drops_invalid_clips() {
        let mut tl = crate::core::timeline::TimelineStore::default();
        let track = tl.tracks[0].clone();
        let good = TimelineClip {
            extra: Default::default(),
            id: "ok".into(),
            track_id: track.id.clone(),
            asset_id: "a".into(),
            name: "ok".into(),
            kind: TrackKind::Video,
            start: 0.0,
            duration: 2.0,
            src_in: 0.0,
            src_duration: 5.0,
            link_id: None,
            enabled: true,
            gain_db: 0.0,
            fx: Default::default(),
            grade: Default::default(),
            effects: Vec::new(),
            title: None,
            subtitle: None,
            adjustment: None,
            speed: crate::core::animation::AnimatedParam::fixed(1.0),
            reverse: false,
            freeze: false,
            markers: Vec::new(),
            nest_seq: None,
            multicam: None,
            blend_mode: crate::core::compose::BlendMode::default(),
        };
        let mut orphan = good.clone();
        orphan.id = "orphan".into();
        orphan.track_id = "missing-track".into();
        let mut broken = good.clone();
        broken.id = "nan".into();
        broken.start = f64::NAN;
        let mut tiny = good.clone();
        tiny.id = "tiny".into();
        tiny.duration = 0.5 * MIN_CLIP_DURATION;
        tl.load_document(
            None,
            vec![track],
            vec![good, orphan, broken, tiny],
            Vec::new(),
            Vec::new(),
            f64::NAN,
            None,
            None,
            40.0,
            true,
            vec!["ok".into(), "orphan".into()],
            0.0,
            None,
        );
        assert_eq!(tl.clips.len(), 1);
        assert_eq!(tl.clips[0].id, "ok");
        assert_eq!(tl.playhead_sec, 0.0);
        assert_eq!(tl.selected_clip_ids, vec!["ok".to_string()]);
        // Ohne gespeicherte Sequenz-Einstellungen: 25-fps-Default.
        assert_eq!(tl.settings.rate, FrameRate::PAL_25);
    }

    #[test]
    fn legacy_v1_project_loads_with_pal_and_guessed_resolution() {
        isolate_config();
        // v1-Datei: kein `sequence`-Feld, Medium mit 4K-Videostream.
        let raw = format!(
            r#"{{
                "format": "{PROJECT_FORMAT}",
                "version": 1,
                "activeWorkspace": "edit",
                "media": [{{
                    "id": "a1",
                    "path": "/tmp/missing.mp4",
                    "name": "missing.mp4",
                    "kind": "video",
                    "info": {{
                        "path": "/tmp/missing.mp4",
                        "fileName": "missing.mp4",
                        "container": "mov,mp4",
                        "durationSec": 10.0,
                        "sizeBytes": 1,
                        "video": [{{
                            "index": 0, "codec": "h264", "width": 3840,
                            "height": 2160, "fps": 29.97, "pixFmt": null,
                            "bitrate": null
                        }}],
                        "audio": []
                    }},
                    "thumbnailPath": null,
                    "importedAt": 0.0,
                    "offline": false
                }}],
                "timeline": {{
                    "tracks": [{{ "id": "v1", "kind": "video" }}],
                    "clips": [{{
                        "id": "c1", "trackId": "v1", "assetId": "a1",
                        "name": "missing.mp4", "kind": "video",
                        "start": 0.0, "duration": 5.0, "srcIn": 0.0,
                        "srcDuration": 10.0, "linkId": null
                    }}]
                }}
            }}"#
        );
        let dir = std::env::temp_dir().join(format!("editron-proj-v1-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v1.etron");
        std::fs::write(&path, raw).unwrap();

        let file = load_from(&path).expect("v1 lädt");
        let mut state = AppState::default();
        apply(&mut state, file, Some(path));
        // Framerate bleibt 25 (Altverhalten), Auflösung aus dem Material.
        assert_eq!(state.timeline.settings.rate, FrameRate::PAL_25);
        assert!(!state.timeline.settings.drop_frame);
        assert_eq!(
            (state.timeline.settings.width, state.timeline.settings.height),
            (3840, 2160)
        );
        // Altprojekt ohne Patch-Flags (v1 < v7): die einzige Videospur wird
        // automatisch Patch- und Targeting-Ziel.
        assert!(state.timeline.tracks[0].source_patched);
        assert!(state.timeline.tracks[0].targeted);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn track_patching_and_targeting_survive_roundtrip() {
        isolate_config();
        let mut state = sample_state();
        // Patch von V1 (idx 1) auf V2 (idx 0) verschieben, V2 zusätzlich
        // anvisieren + sync-locken (Nicht-Standard-Zustand).
        let v2 = state.timeline.tracks[0].id.clone();
        state.timeline.toggle_source_patch(&v2);
        state.timeline.tracks[0].targeted = true;
        state.timeline.tracks[0].sync_lock = true;
        let dir = std::env::temp_dir().join(format!("editron-proj-patch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("patch.etron");
        save_to(&mut state, &path).unwrap();

        let file = load_from(&path).expect("load");
        assert_eq!(file.version, PROJECT_VERSION);
        let mut target = AppState::default();
        apply(&mut target, file, Some(path.clone()));
        // v7-Datei: gespeicherte Flags werden unverändert übernommen.
        assert!(target.timeline.tracks[0].source_patched, "V2 gepatcht");
        assert!(!target.timeline.tracks[1].source_patched, "V1 nicht mehr gepatcht");
        assert!(target.timeline.tracks[0].targeted);
        assert!(target.timeline.tracks[0].sync_lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn track_height_survives_roundtrip_and_is_clamped() {
        isolate_config();
        let mut state = sample_state();
        let v_id = state.timeline.tracks[0].id.clone();
        let a_id = state.timeline.tracks[2].id.clone();
        // Eine Spur manuell höher ziehen, eine über das Maximum hinaus
        // (Klemmung), die übrigen unverändert (= Standardhöhe der Spurart).
        state.timeline.set_track_height_live(&v_id, 96.0);
        state.timeline.set_track_height_live(&a_id, 10_000.0);

        let dir = std::env::temp_dir().join(format!("editron-proj-th-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("th.etron");
        save_to(&mut state, &path).expect("save");

        let file = load_from(&path).expect("load");
        assert_eq!(file.version, PROJECT_VERSION);
        let mut target = AppState::default();
        apply(&mut target, file, Some(path.clone()));

        let v = target.timeline.tracks.iter().find(|t| t.id == v_id).expect("V-Spur");
        let a = target.timeline.tracks.iter().find(|t| t.id == a_id).expect("A-Spur");
        assert_eq!(v.height, Some(96.0), "manuelle Höhe überlebt den Roundtrip");
        assert_eq!(
            a.height,
            Some(crate::core::timeline::MAX_TRACK_HEIGHT),
            "Höhe wird auf das Maximum geklemmt"
        );
        assert!(
            target.timeline.tracks.iter().any(|t| t.height.is_none()),
            "unveränderte Spuren bleiben ohne explizite Höhe (Standardhöhe)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn title_clips_survive_roundtrip_and_orphan_cleanup() {
        isolate_config();
        let mut state = AppState::default();
        let mut spec = crate::core::title::TitleTemplate::LowerThird.build();
        spec.text = "Roundtrip\nBauchbinde".into();
        spec.stroke_width = 3.5;
        let title_id = state.timeline.add_title_clip(spec.clone(), 1.0, 4.0);
        // Titel-Transform-Keyframes (Abspann-Mechanik) müssen mitkommen.
        if let Some(c) = state.timeline.clips.iter_mut().find(|c| c.id == title_id) {
            c.fx.pos_y.upsert_key(0.0, 110.0);
            c.fx.pos_y.upsert_key(4.0, -110.0);
        }
        // Verwaister Medien-Clip (Asset fehlt in der Datei): fliegt beim
        // Laden raus — der Titel-Clip (ohne Asset) muss bleiben.
        let track_id = state.timeline.tracks[0].id.clone();
        state.timeline.clips.push(TimelineClip {
            extra: Default::default(),
            id: "orphan".into(),
            track_id,
            asset_id: "missing-asset".into(),
            name: "weg".into(),
            kind: crate::core::timeline::TrackKind::Video,
            start: 10.0,
            duration: 2.0,
            src_in: 0.0,
            src_duration: 5.0,
            link_id: None,
            enabled: true,
            gain_db: 0.0,
            fx: Default::default(),
            grade: Default::default(),
            effects: Vec::new(),
            title: None,
            subtitle: None,
            adjustment: None,
            speed: crate::core::animation::AnimatedParam::fixed(1.0),
            reverse: false,
            freeze: false,
            markers: Vec::new(),
            nest_seq: None,
            multicam: None,
            blend_mode: crate::core::compose::BlendMode::default(),
        });

        let dir = std::env::temp_dir().join(format!("editron-proj-title-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("titel.etron");
        save_to(&mut state, &path).expect("save");

        let file = load_from(&path).expect("load");
        assert_eq!(file.version, PROJECT_VERSION);
        let saved = file.sequences[0]
            .timeline
            .clips
            .iter()
            .find(|c| c.id == title_id)
            .expect("Titel-Clip gespeichert");
        assert_eq!(saved.title.as_ref(), Some(&spec));
        assert_eq!(saved.fx.pos_y.keyframes.len(), 2);

        let mut target = AppState::default();
        apply(&mut target, file, Some(path));
        let loaded = target.timeline.clip(&title_id).expect("Titel-Clip überlebt das Laden");
        assert_eq!(loaded.title.as_ref(), Some(&spec));
        assert!(loaded.src_duration.is_infinite());
        assert!(
            target.timeline.clip("orphan").is_none(),
            "verwaiste Medien-Clips werden weiterhin entfernt"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn subtitle_tracks_survive_roundtrip_with_style_and_active_track() {
        isolate_config();
        let mut state = AppState::default();
        // Zwei Spuren mit Segmenten + Spurstil; U2 ist die aktive Spur.
        let (u1, _) = state.timeline.import_subtitle_cues(&[
            crate::core::subtitle::SrtCue { start: 0.0, end: 2.0, text: "Erstes Segment".into() },
            crate::core::subtitle::SrtCue {
                start: 2.0,
                end: 4.0,
                text: "Zweites\nmit Umbruch".into(),
            },
        ]);
        state.timeline.subtitle_style_update(&u1, |s| {
            s.size = 56.0;
            s.color = crate::core::title::RgbaColor::rgb(255, 230, 0);
            s.pos_y = -38.0;
            s.bg_enabled = false;
            s.stroke_width = 3.0;
        });
        let (u2, _) = state.timeline.import_subtitle_cues(&[crate::core::subtitle::SrtCue {
            start: 1.0,
            end: 3.0,
            text: "Zweite Sprache".into(),
        }]);
        // Spur U1 ausgeblendet (muted = Sichtbarkeit der Untertitel-Spur).
        if let Some(t) = state.timeline.tracks.iter_mut().find(|t| t.id == u1) {
            t.muted = true;
        }
        state.timeline.set_active_subtitle_track(&u2);

        let dir = std::env::temp_dir().join(format!("editron-proj-subs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("untertitel.etron");
        save_to(&mut state, &path).expect("save");

        let file = load_from(&path).expect("load");
        assert_eq!(file.version, PROJECT_VERSION);
        assert_eq!(file.sequences[0].timeline.active_subtitle_track_id.as_deref(), Some(u2.as_str()));

        let mut target = AppState::default();
        apply(&mut target, file, Some(path));
        let style = target.timeline.subtitle_style(&u1);
        assert_eq!(style.size, 56.0);
        assert_eq!(style.color, crate::core::title::RgbaColor::rgb(255, 230, 0));
        assert_eq!(style.pos_y, -38.0);
        assert!(!style.bg_enabled);
        assert_eq!(style.stroke_width, 3.0);
        assert!(target.timeline.tracks.iter().find(|t| t.id == u1).unwrap().muted);
        assert_eq!(target.timeline.active_subtitle_track().unwrap().id, u2);
        // Segmente (Generatoren ohne Asset) überleben die Verwaisten-Bereinigung.
        let texts: Vec<String> = target
            .timeline
            .clips
            .iter()
            .filter(|c| c.track_id == u1)
            .filter_map(|c| c.subtitle.as_ref().map(|s| s.text.clone()))
            .collect();
        assert_eq!(texts.len(), 2);
        assert!(texts.contains(&"Zweites\nmit Umbruch".to_string()));
        // SRT-Export nach dem Laden bleibt deckungsgleich.
        let cues = target.timeline.subtitle_cues(&u2);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "Zweite Sprache");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Hilfskonstruktor für Medien-Assets in den Bin-Tests.
    fn test_asset(id: &str, name: &str, recorded_at: Option<f64>) -> MediaAsset {
        MediaAsset {
            extra: Default::default(),
            id: id.into(),
            path: format!("/tmp/editron-test/{id}.mp4"),
            name: name.into(),
            kind: crate::core::types::MediaKind::Video,
            info: crate::core::types::MediaInfo {
                path: format!("/tmp/editron-test/{id}.mp4"),
                file_name: format!("{id}.mp4"),
                container: "mov,mp4".into(),
                duration_sec: 5.0,
                size_bytes: 4242,
                video: Vec::new(),
                audio: Vec::new(),
                recorded_at,
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

    #[test]
    fn media_organization_survives_roundtrip() {
        use crate::core::bin::{MediaLabel, SortKey, ViewMode, ROOT_BIN_ID};
        isolate_config();
        let mut state = AppState::default();
        state.media.add_asset(test_asset("a-root", "root.mp4", None));

        // Verschachtelter Bin-Baum: Footage / B-Roll.
        let footage = state.media.create_bin(ROOT_BIN_ID, "Footage");
        let broll = state.media.create_bin(&footage, "B-Roll");
        assert_eq!(state.media.bins.len(), 2);

        // Zweites Asset nach B-Roll verschieben, umbenennen, etikettieren.
        state.media.add_asset(test_asset("a-broll", "clip.mp4", Some(1_600_000_000.0)));
        state.media.move_assets_to_bin(&["a-broll".into()], &broll);
        state.media.rename_asset("a-broll", "Sonnenuntergang");
        state.media.set_label(&["a-broll".into()], Some(MediaLabel::Orange));

        // Ansichts-Zustand abweichend vom Standard.
        state.media.view.mode = ViewMode::List;
        state.media.view.sort = SortKey::Size;
        state.media.view.sort_desc = true;
        state.media.view.current_bin = footage.clone();
        state.media.view.col_widths[0] = 123.0;

        let dir = std::env::temp_dir().join(format!("editron-proj-bins-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bins.etron");
        save_to(&mut state, &path).expect("save");

        let file = load_from(&path).expect("load");
        assert_eq!(file.version, PROJECT_VERSION);
        assert_eq!(file.media_bins.len(), 2);
        assert_eq!(file.media_view.mode, ViewMode::List);

        let mut target = AppState::default();
        apply(&mut target, file, Some(path.clone()));
        // Bin-Baum erhalten.
        assert_eq!(target.media.bins.len(), 2);
        assert_eq!(target.media.bin_path_label(&broll), "Projekt / Footage / B-Roll");
        // Asset-Zuordnung, Anzeigename, Etikett, Aufnahmedatum.
        let a2 = target.media.asset("a-broll").unwrap();
        assert_eq!(a2.bin_id, broll);
        assert_eq!(a2.name, "Sonnenuntergang");
        assert_eq!(a2.label, Some(MediaLabel::Orange));
        assert_eq!(a2.info.recorded_at, Some(1_600_000_000.0));
        // Wurzel-Asset bleibt in der Wurzel.
        assert_eq!(target.media.asset("a-root").unwrap().bin_id, ROOT_BIN_ID);
        assert_eq!(target.media.assets_in_bin(ROOT_BIN_ID).len(), 1);
        assert_eq!(target.media.assets_in_bin(&broll).len(), 1);
        // Ansichts-Zustand erhalten.
        assert_eq!(target.media.view.sort, SortKey::Size);
        assert!(target.media.view.sort_desc);
        assert_eq!(target.media.view.current_bin, footage);
        assert_eq!(target.media.view.col_widths[0], 123.0);
        // Laden verwirft die Undo-History.
        assert!(!target.media.can_undo());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn proxy_state_survives_roundtrip_and_validates() {
        use crate::core::proxy::{ProxyCodec, ProxyScale};
        isolate_config();
        let dir = std::env::temp_dir().join(format!("editron-proj-proxy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Echte Proxy-Datei, damit die Validierung sie als vorhanden erkennt.
        let proxy = dir.join("clip_proxy.mov");
        std::fs::write(&proxy, b"proxy-bytes").unwrap();

        let mut state = AppState::default();
        state.media.add_asset(test_asset("a1", "clip.mp4", None));
        if let Some(a) = state.media.assets.iter_mut().find(|a| a.id == "a1") {
            a.proxy_path = Some(proxy.to_string_lossy().into_owned());
            a.proxy_src_mtime = Some(42.0);
        }
        state.media.use_proxies = true;
        state.media.proxy_settings.codec = ProxyCodec::DnxhrLb;
        state.media.proxy_settings.scale = ProxyScale::Quarter;

        let path = dir.join("proxy.etron");
        save_to(&mut state, &path).expect("save");

        let file = load_from(&path).expect("load");
        assert_eq!(file.version, PROJECT_VERSION);
        assert!(file.use_proxies);
        assert_eq!(file.proxy_settings.codec, ProxyCodec::DnxhrLb);
        assert_eq!(file.proxy_settings.scale, ProxyScale::Quarter);
        assert_eq!(file.media[0].proxy_path.as_deref(), Some(proxy.to_string_lossy().as_ref()));
        assert_eq!(file.media[0].proxy_src_mtime, Some(42.0));

        let mut target = AppState::default();
        apply(&mut target, file, Some(path));
        let a = target.media.asset("a1").unwrap();
        // Quelle offline (Pfad existiert nicht), aber Proxy-Datei vorhanden ⇒
        // gültiger Proxy (Fallback-Vorschau).
        assert!(a.offline);
        assert!(!a.proxy_offline, "vorhandener Proxy bleibt gültig");
        assert!(a.has_valid_proxy());
        assert!(target.media.use_proxies);
        assert_eq!(a.decode_path(true), proxy.to_string_lossy());
        // Ohne Proxy-Modus: Decode-Pfad bleibt das Original.
        assert_eq!(a.decode_path(false), a.path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_project_without_bins_lands_in_root() {
        use crate::core::bin::ROOT_BIN_ID;
        isolate_config();
        // v8-artige Datei ohne mediaBins/binId.
        let raw = format!(
            r#"{{
                "format": "{PROJECT_FORMAT}",
                "version": 8,
                "activeWorkspace": "edit",
                "media": [{{
                    "id": "a1", "path": "/tmp/missing.mp4", "name": "missing.mp4",
                    "kind": "video",
                    "info": {{
                        "path": "/tmp/missing.mp4", "fileName": "missing.mp4",
                        "container": "mov,mp4", "durationSec": 10.0, "sizeBytes": 1,
                        "video": [], "audio": []
                    }},
                    "thumbnailPath": null, "importedAt": 0.0, "offline": false
                }}]
            }}"#
        );
        let dir = std::env::temp_dir().join(format!("editron-proj-legacybin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("legacy.etron");
        std::fs::write(&path, raw).unwrap();
        let file = load_from(&path).expect("legacy lädt");
        let mut state = AppState::default();
        apply(&mut state, file, Some(path));
        assert!(state.media.bins.is_empty());
        assert_eq!(state.media.asset("a1").unwrap().bin_id, ROOT_BIN_ID);
        assert_eq!(state.media.assets_in_bin(ROOT_BIN_ID).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multiple_sequences_and_nesting_survive_roundtrip() {
        use crate::core::bin::ROOT_BIN_ID;
        use crate::core::sequence::SequenceSettings;
        isolate_config();
        let mut state = AppState::default();
        state.media.add_asset(test_asset("a1", "clip.mp4", None));

        // Innere Sequenz (Default „Sequenz 01") mit einem Medien-Clip.
        let inner_id = state.timeline.active_id().to_string();
        let track = state.timeline.tracks[0].id.clone();
        let mut media_clip = crate::core::timeline::test_clip(&track);
        media_clip.asset_id = "a1".into();
        media_clip.duration = 6.0;
        state.timeline.clips.push(media_clip);

        // Äußere Sequenz anlegen (wird aktiv) und die innere darin verschachteln.
        let outer_id = state
            .timeline
            .add(Some("Schnitt".into()), SequenceSettings::default(), ROOT_BIN_ID);
        assert_eq!(state.timeline.active_id(), outer_id);
        assert!(!state.timeline.would_create_cycle(&outer_id, &inner_id));
        let otrack = state.timeline.tracks[0].id.clone();
        let mut nest = crate::core::timeline::test_clip(&otrack);
        nest.asset_id = String::new();
        nest.nest_seq = Some(inner_id.clone());
        nest.name = "Sequenz 01".into();
        nest.duration = 6.0;
        nest.src_duration = 6.0;
        state.timeline.clips.push(nest);

        let dir = std::env::temp_dir().join(format!("editron-proj-nest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nest.etron");
        save_to(&mut state, &path).expect("save");

        let file = load_from(&path).expect("load");
        assert_eq!(file.version, PROJECT_VERSION);
        assert_eq!(file.sequences.len(), 2, "beide Sequenzen gespeichert");
        assert_eq!(file.active_sequence_id.as_deref(), Some(outer_id.as_str()));
        assert!(file.timeline.is_empty(), "Alt-Feld bleibt beim v11-Speichern leer");

        let mut target = AppState::default();
        apply(&mut target, file, Some(path.clone()));
        assert_eq!(target.timeline.len(), 2);
        assert_eq!(target.timeline.active_id(), outer_id);
        // Nest-Clip erhalten und verweist weiterhin auf die innere Sequenz.
        let outer_tl = target.timeline.timeline_of(&outer_id).unwrap();
        assert!(outer_tl
            .clips
            .iter()
            .any(|c| c.nest_seq.as_deref() == Some(inner_id.as_str())));
        // Innere Sequenz behält ihren Medien-Clip.
        let inner_tl = target.timeline.timeline_of(&inner_id).unwrap();
        assert_eq!(inner_tl.clips.iter().filter(|c| c.asset_id == "a1").count(), 1);
        // Rekursionsschutz nach dem Laden weiterhin wasserdicht.
        assert!(target.timeline.would_create_cycle(&inner_id, &outer_id));
        assert_eq!(target.timeline.nest_usage_count(&inner_id), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multicam_source_and_clip_survive_roundtrip() {
        use crate::core::bin::ROOT_BIN_ID;
        use crate::core::multicam::{MulticamAngle, MulticamClip, MulticamSource, MulticamSync};
        isolate_config();
        let mut state = AppState::default();
        state.media.add_asset(test_asset("a1", "cam1.mp4", None));
        state.media.add_asset(test_asset("a2", "cam2.mp4", None));
        let active_id = state.timeline.active_id().to_string();

        // Multicam-Quelle (Hintergrund-Sequenz) mit zwei Winkeln.
        let source = MulticamSource {
            angles: vec![
                MulticamAngle {
                    name: "Kamera 1".into(),
                    asset_id: "a1".into(),
                    pos: 0.0,
                    duration: 8.0,
                    width: 1920,
                    height: 1080,
                    fps: 25.0,
                    has_audio: true,
                },
                MulticamAngle {
                    name: "Kamera 2".into(),
                    asset_id: "a2".into(),
                    pos: 1.5,
                    duration: 8.0,
                    width: 1920,
                    height: 1080,
                    fps: 25.0,
                    has_audio: true,
                },
            ],
            audio_angle: Some(0),
            sync: MulticamSync::Audio,
            duration: 9.5,
        };
        let inner = crate::core::multicam::build_inner_timeline(&source);
        let mut seq = crate::core::sequences::Sequence::new("Multicam – Cam", ROOT_BIN_ID, inner);
        seq.timeline.multicam = Some(source);
        let src_id = state.timeline.add_background(seq);

        // Multicam-Clip in der aktiven Sequenz, aktiver Winkel 1.
        let track = state.timeline.tracks[0].id.clone();
        let mut clip = crate::core::timeline::test_clip(&track);
        clip.asset_id = String::new();
        clip.name = "Multicam".into();
        clip.duration = 9.5;
        clip.src_duration = 9.5;
        clip.multicam = Some(MulticamClip {
            source: src_id.clone(),
            angle: 1,
        });
        state.timeline.clips.push(clip);

        let dir = std::env::temp_dir().join(format!("editron-proj-mc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mc.etron");
        save_to(&mut state, &path).expect("save");

        let file = load_from(&path).expect("load");
        assert_eq!(file.version, PROJECT_VERSION);

        let mut target = AppState::default();
        apply(&mut target, file, Some(path.clone()));
        // Quelle: Multicam-Metadaten erhalten.
        let src_tl = target.timeline.timeline_of(&src_id).expect("Quell-Sequenz");
        let mc = src_tl.multicam.as_ref().expect("Multicam-Quelle erhalten");
        assert_eq!(mc.angles.len(), 2);
        assert_eq!(mc.audio_angle, Some(0));
        assert!((mc.angles[1].pos - 1.5).abs() < 1e-9);
        assert_eq!(mc.angles[0].asset_id, "a1");
        // Multicam-Clip: erhalten mit aktivem Winkel 1 und Quell-Verweis.
        let active_tl = target.timeline.timeline_of(&active_id).expect("aktive Sequenz");
        let clip = active_tl
            .clips
            .iter()
            .find(|c| c.is_multicam())
            .expect("Multicam-Clip erhalten");
        let mc_ref = clip.multicam.as_ref().unwrap();
        assert_eq!(mc_ref.source, src_id);
        assert_eq!(mc_ref.angle, 1);
        // Multicam-Clip wird NICHT als verwaist entfernt (kein asset_id).
        assert!(clip.asset_id.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_nested_sequence_cleans_dangling_nest_clips() {
        use crate::core::bin::ROOT_BIN_ID;
        use crate::core::sequence::SequenceSettings;
        let mut store = crate::core::sequences::SequenceStore::default();
        let inner = store.active_id().to_string();
        let outer = store.add(Some("Outer".into()), SequenceSettings::default(), ROOT_BIN_ID);
        // Nest-Clip in der äußeren Sequenz, der die innere referenziert.
        let track = store.tracks[0].id.clone();
        let mut nest = crate::core::timeline::test_clip(&track);
        nest.nest_seq = Some(inner.clone());
        store.clips.push(nest);
        assert_eq!(store.nest_usage_count(&inner), 1);
        // Innere Sequenz löschen → Nest-Clip in der äußeren verschwindet.
        assert!(store.remove(&inner));
        assert_eq!(store.len(), 1);
        assert_eq!(store.active_id(), outer);
        assert_eq!(store.nest_usage_count(&inner), 0);
        assert!(store.clips.iter().all(|c| c.nest_seq.is_none()));
    }
}

