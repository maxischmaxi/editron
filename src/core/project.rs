//! Projektdateien (.etron): versioniertes JSON-Format mit allen Projektdaten
//! (Workspace, Medien, Timeline, Quellmonitor), atomarem Speichern
//! (tmp + rename + .bak), Zuletzt-geöffnet-Liste und Autosave der Sitzung.

use crate::core::timeline::{TimelineClip, TimelineTrack};
use crate::core::types::MediaAsset;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PROJECT_EXT: &str = "etron";
pub const PROJECT_FORMAT: &str = "editron-project";
pub const PROJECT_VERSION: u32 = 1;
const RECENT_LIMIT: usize = 10;

// ------------------------------------------------------------------- Format

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFile {
    /// Magic zum Erkennen fremder JSON-Dateien.
    pub format: String,
    /// Formatversion für Migrationen; neuere Dateien werden abgelehnt.
    pub version: u32,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub saved_at_unix: f64,
    pub active_workspace: String,
    #[serde(default)]
    pub media: Vec<MediaAsset>,
    #[serde(default)]
    pub selected_asset_ids: Vec<String>,
    #[serde(default)]
    pub timeline: TimelineDoc,
    #[serde(default)]
    pub source_monitor: SourceMonitorDoc,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TimelineDoc {
    #[serde(default)]
    pub tracks: Vec<TimelineTrack>,
    #[serde(default)]
    pub clips: Vec<TimelineClip>,
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
    pub recent: Vec<String>,
    seen_timeline_rev: u64,
    seen_media_rev: u64,
}

impl Default for ProjectStore {
    fn default() -> Self {
        ProjectStore {
            path: None,
            dirty: false,
            recent: load_recent(),
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
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(recent) {
        let _ = std::fs::write(path, json);
    }
}

/// Ablageort des Sitzungs-Autosaves (ungespeicherte Projekte beim Beenden).
pub fn autosave_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("editron")
        .join("autosave.etron")
}

// -------------------------------------------------------------- Save / Load

/// Projektdaten aus dem App-Zustand einsammeln.
pub fn collect(state: &AppState) -> ProjectFile {
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
        selected_asset_ids: state.media.selected_asset_ids.clone(),
        timeline: TimelineDoc {
            tracks: state.timeline.tracks.clone(),
            clips: state.timeline.clips.clone(),
            playhead_sec: state.timeline.playhead_sec,
            in_point: state.timeline.in_point,
            out_point: state.timeline.out_point,
            zoom_px_per_sec: state.timeline.zoom_px_per_sec,
            snapping: state.timeline.snapping,
            selected_clip_ids: state.timeline.selected_clip_ids.clone(),
            master_gain_db: state.timeline.master_gain_db,
        },
        source_monitor: SourceMonitorDoc {
            asset_id: state.playback.source_asset_id.clone(),
            position: state.playback.source.position,
            in_mark: state.playback.source.in_mark,
            out_mark: state.playback.source.out_mark,
            looping: state.playback.source.looping,
        },
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
    let file = collect(state);
    let json = serde_json::to_string(&file).map_err(|e| format!("Serialisierung: {e}"))?;

    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| format!("Ordner anlegen: {e}"))?;
        }
    }
    // Temp-Datei im Zielordner (rename über Dateisystemgrenzen schlägt fehl).
    let tmp = path.with_extension(format!("{PROJECT_EXT}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, json.as_bytes()).map_err(|e| format!("Schreiben: {e}"))?;
    if path.exists() {
        let _ = std::fs::copy(path, path.with_extension(format!("{PROJECT_EXT}.bak")));
    }
    if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("Speichern: {err}"));
    }

    state.project.path = Some(path.to_path_buf());
    state.project.push_recent(path);
    let (t_rev, m_rev) = (state.timeline.revision, state.media.revision);
    state.project.mark_clean(t_rev, m_rev);
    Ok(())
}

/// Projektdatei lesen und validieren (Format-Magic, Versionsfenster).
pub fn load_from(path: &Path) -> Result<ProjectFile, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("{} konnte nicht gelesen werden: {e}", path.display()))?;
    let file: ProjectFile =
        serde_json::from_str(&raw).map_err(|e| format!("Keine gültige Projektdatei: {e}"))?;
    if file.format != PROJECT_FORMAT {
        return Err("Keine Editron-Projektdatei".to_string());
    }
    if file.version > PROJECT_VERSION {
        return Err(format!(
            "Projekt wurde mit einer neueren Editron-Version gespeichert (Format v{} > v{PROJECT_VERSION})",
            file.version
        ));
    }
    Ok(file)
}

/// Geladenes Projekt in den App-Zustand übernehmen. Liefert die Anzahl
/// fehlender Medien (Offline-Check passiert hier).
pub fn apply(state: &mut AppState, file: ProjectFile, path: Option<PathBuf>) -> usize {
    // Medien übernehmen + Offline-Status prüfen; verwaiste Thumbnails
    // (Cache geleert) nicht weiterreichen, damit sie neu entstehen können.
    let mut media = file.media;
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
    state.media.selected_asset_ids = file
        .selected_asset_ids
        .into_iter()
        .filter(|id| asset_ids.contains(id.as_str()))
        .collect();
    state.media.waveforms.clear();
    state.media.importing = false;
    state.media.revision += 1;

    let t = file.timeline;
    state.timeline.load_document(
        t.tracks,
        t.clips,
        t.playhead_sec,
        t.in_point,
        t.out_point,
        t.zoom_px_per_sec,
        t.snapping,
        t.selected_clip_ids,
        t.master_gain_db,
    );
    // Clips verwaister Assets entfernen (Asset aus der Datei gelöscht o. ä.).
    let orphans: Vec<String> = state
        .timeline
        .clips
        .iter()
        .filter(|c| !asset_ids.contains(c.asset_id.as_str()))
        .map(|c| c.id.clone())
        .collect();
    if !orphans.is_empty() {
        state.timeline.clips.retain(|c| !orphans.contains(&c.id));
    }

    let sm = file.source_monitor;
    state.playback = Default::default();
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
    if let Some(p) = &path {
        state.project.push_recent(p);
    }
    let (t_rev, m_rev) = (state.timeline.revision, state.media.revision);
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
    let (t_rev, m_rev) = (state.timeline.revision, state.media.revision);
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
    // Nichts zu sichern, wenn das Projekt faktisch leer ist.
    if state.media.assets.is_empty() && state.timeline.clips.is_empty() {
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
    use crate::core::timeline::{TrackKind, SEQUENCE_FPS};

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
            },
            thumbnail_path: None,
            imported_at: 0.0,
            offline: false,
        };
        state.media.add_asset(asset);
        let track_id = state.timeline.tracks[0].id.clone();
        state.timeline.clips.push(TimelineClip {
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
        });
        // Standbild mit unendlicher Quelldauer (Infinity-Roundtrip).
        let track_id = state.timeline.tracks[1].id.clone();
        state.timeline.clips.push(TimelineClip {
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
        });
        state.timeline.playhead_sec = 3.25;
        state.timeline.in_point = Some(1.0);
        state.timeline.out_point = Some(9.0);
        state.timeline.master_gain_db = -4.5;
        state.timeline.tracks[2].gain_db = 2.0;
        state.timeline.tracks[2].pan = -0.5;
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
        assert_eq!(file.timeline.clips.len(), 2);
        assert_eq!(file.timeline.playhead_sec, 3.25);
        assert_eq!(file.timeline.in_point, Some(1.0));
        assert!(file.timeline.clips[1].src_duration.is_infinite());
        assert!(!file.timeline.clips[1].enabled);
        assert_eq!(file.timeline.master_gain_db, -4.5);
        assert_eq!(file.timeline.clips[0].gain_db, -3.0);
        let fx = &file.timeline.clips[0].fx;
        assert_eq!(fx.pos_x.keyframes.len(), 2);
        assert_eq!(fx.pos_x.keyframes[0].interp, crate::core::animation::Interp::EaseInOut);
        assert_eq!(fx.opacity.value, 80.0);
        assert!(fx.pos_x.is_animated());
        // Unveränderte Clips speichern kein fx-Feld (schlanke Datei).
        assert!(file.timeline.clips[1].fx.is_default());
        assert_eq!(file.timeline.tracks[2].gain_db, 2.0);
        assert_eq!(file.timeline.tracks[2].pan, -0.5);

        let mut target = AppState::default();
        let offline = apply(&mut target, file, Some(path.clone()));
        // Quelldatei existiert nicht → offline erkannt.
        assert_eq!(offline, 1);
        assert!(target.media.assets[0].offline);
        assert_eq!(target.timeline.clips.len(), 2);
        assert_eq!(target.timeline.clips[0].start, 1.0);
        assert_eq!(target.timeline.master_gain_db, -4.5);
        assert_eq!(target.timeline.tracks[2].pan, -0.5);
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
    fn rejects_foreign_and_newer_files() {
        let dir = std::env::temp_dir().join(format!("editron-proj-rej-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let foreign = dir.join("foreign.etron");
        std::fs::write(&foreign, r#"{"hello": 1}"#).unwrap();
        assert!(load_from(&foreign).is_err());

        let newer = dir.join("newer.etron");
        std::fs::write(
            &newer,
            format!(
                r#"{{"format":"{PROJECT_FORMAT}","version":{},"activeWorkspace":"edit"}}"#,
                PROJECT_VERSION + 1
            ),
        )
        .unwrap();
        let err = match load_from(&newer) {
            Err(e) => e,
            Ok(_) => panic!("neuere Formatversion wurde nicht abgelehnt"),
        };
        assert!(err.contains("neueren"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_document_drops_invalid_clips() {
        let mut tl = crate::core::timeline::TimelineStore::default();
        let track = tl.tracks[0].clone();
        let good = TimelineClip {
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
        };
        let mut orphan = good.clone();
        orphan.id = "orphan".into();
        orphan.track_id = "missing-track".into();
        let mut broken = good.clone();
        broken.id = "nan".into();
        broken.start = f64::NAN;
        let mut tiny = good.clone();
        tiny.id = "tiny".into();
        tiny.duration = 0.5 / SEQUENCE_FPS;
        tl.load_document(
            vec![track],
            vec![good, orphan, broken, tiny],
            f64::NAN,
            None,
            None,
            40.0,
            true,
            vec!["ok".into(), "orphan".into()],
            0.0,
        );
        assert_eq!(tl.clips.len(), 1);
        assert_eq!(tl.clips[0].id, "ok");
        assert_eq!(tl.playhead_sec, 0.0);
        assert_eq!(tl.selected_clip_ids, vec!["ok".to_string()]);
    }
}
