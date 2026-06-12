//! Projektdateien (.etron): versioniertes JSON-Format mit allen Projektdaten
//! (Workspace, Medien, Timeline, Quellmonitor), atomarem Speichern
//! (tmp + rename + .bak), Zuletzt-geöffnet-Liste und Autosave der Sitzung.

use crate::core::timeline::{TimelineClip, TimelineTrack};
use crate::core::transitions::Transition;
use crate::core::types::MediaAsset;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
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
pub const PROJECT_VERSION: u32 = 5;
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
            sequence: Some(state.timeline.settings),
            tracks: state.timeline.tracks.clone(),
            clips: state.timeline.clips.clone(),
            transitions: state.timeline.transitions.clone(),
            playhead_sec: state.timeline.playhead_sec,
            in_point: state.timeline.in_point,
            out_point: state.timeline.out_point,
            zoom_px_per_sec: state.timeline.zoom_px_per_sec,
            snapping: state.timeline.snapping,
            selected_clip_ids: state.timeline.selected_clip_ids.clone(),
            master_gain_db: state.timeline.master_gain_db,
            active_subtitle_track_id: state.timeline.active_subtitle_track_id.clone(),
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
    let legacy_sequence = t.sequence.is_none();
    state.timeline.load_document(
        t.sequence,
        t.tracks,
        t.clips,
        t.transitions,
        t.playhead_sec,
        t.in_point,
        t.out_point,
        t.zoom_px_per_sec,
        t.snapping,
        t.selected_clip_ids,
        t.master_gain_db,
        t.active_subtitle_track_id,
    );
    // Altprojekt (v1, ohne Sequenz-Einstellungen): 25 fps bleiben, die
    // Auflösung wird wie früher aus dem Material geraten.
    if legacy_sequence {
        let (w, h) = crate::core::export::suggested_resolution(&state.timeline, &state.media);
        state.timeline.settings.width = w;
        state.timeline.settings.height = h;
    }
    // Clips verwaister Assets entfernen (Asset aus der Datei gelöscht o. ä.).
    // Titel-/Untertitel-Clips sind Generatoren ohne Asset und bleiben immer.
    let orphans: Vec<String> = state
        .timeline
        .clips
        .iter()
        .filter(|c| !c.is_generator() && !asset_ids.contains(c.asset_id.as_str()))
        .map(|c| c.id.clone())
        .collect();
    if !orphans.is_empty() {
        state.timeline.clips.retain(|c| !orphans.contains(&c.id));
        // Übergänge an entfernten Clips ebenfalls aufräumen.
        state.timeline.transitions.retain(|t| {
            let gone = |id: &Option<String>| id.as_ref().is_some_and(|id| orphans.contains(id));
            !gone(&t.from_clip_id) && !gone(&t.to_clip_id)
        });
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
            grade: {
                // Farbkorrektur muss den Roundtrip überleben.
                let mut g = crate::core::grade::ColorGrade::default();
                g.temperature = 25.0;
                g.look = crate::core::grade::GradeLook::TealOrange;
                g.gain = crate::core::grade::WheelValue { x: 0.3, y: -0.1, luma: 0.05 };
                g.vignette_amount = 30.0;
                g
            },
            effects: {
                // Effekt-Stapel inkl. Keyframes muss den Roundtrip überleben.
                let mut blur =
                    crate::core::effects::EffectInstance::new(crate::core::effects::EffectKind::GaussianBlur);
                blur.params[0].upsert_key(0.5, 0.0);
                blur.params[0].upsert_key(4.5, 60.0);
                let mut key =
                    crate::core::effects::EffectInstance::new(crate::core::effects::EffectKind::ChromaKey);
                key.enabled = false;
                vec![blur, key]
            },
            title: None,
            subtitle: None,
            speed: 1.0,
            reverse: false,
            freeze: false,
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
            grade: Default::default(),
            effects: Vec::new(),
            title: None,
            subtitle: None,
            speed: 1.0,
            reverse: false,
            freeze: false,
        });
        // Rückwärts-Clip mit 37 % muss den Roundtrip exakt überleben.
        let track_id = state.timeline.tracks[0].id.clone();
        state.timeline.clips.push(TimelineClip {
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
            speed: 0.37,
            reverse: true,
            freeze: false,
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
        assert_eq!(file.timeline.clips.len(), 3);
        assert_eq!(file.timeline.playhead_sec, 3.25);
        // Clip-Geschwindigkeit exakt erhalten (rückwärts, 37 %).
        let speedy = file.timeline.clips.iter().find(|c| c.id == "clip-3").unwrap();
        assert_eq!(speedy.speed, 0.37);
        assert!(speedy.reverse);
        assert!(!speedy.freeze);
        // Normale Clips bleiben bei 100 % vorwärts.
        assert_eq!(file.timeline.clips[0].speed, 1.0);
        assert!(!file.timeline.clips[0].reverse);
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
        let g = &file.timeline.clips[0].grade;
        assert_eq!(g.temperature, 25.0);
        assert_eq!(g.look, crate::core::grade::GradeLook::TealOrange);
        assert_eq!(g.gain.x, 0.3);
        assert_eq!(g.vignette_amount, 30.0);
        // Unveränderte Clips speichern kein fx-/grade-Feld (schlanke Datei).
        assert!(file.timeline.clips[1].fx.is_default());
        assert!(file.timeline.clips[1].grade.is_default());
        assert_eq!(file.timeline.tracks[2].gain_db, 2.0);
        assert_eq!(file.timeline.tracks[2].pan, -0.5);
        // Übergang vollständig erhalten.
        assert_eq!(file.timeline.transitions.len(), 1);
        let tr = &file.timeline.transitions[0];
        assert_eq!(tr.kind, crate::core::transitions::TransitionKind::Wipe);
        assert_eq!(tr.direction, crate::core::transitions::TransitionDirection::Down);
        assert_eq!(tr.to_clip_id.as_deref(), Some("clip-1"));
        assert_eq!(tr.duration, 1.5);

        // Sequenz-Einstellungen exakt erhalten (NTSC-Bruch, kein Float).
        let seq = file.timeline.sequence.expect("Sequenz-Einstellungen gespeichert");
        assert_eq!(seq.rate, FrameRate::new(30000, 1001));
        assert_eq!((seq.width, seq.height), (1280, 720));
        assert!(seq.drop_frame);

        let mut target = AppState::default();
        let offline = apply(&mut target, file, Some(path.clone()));
        // Quelldatei existiert nicht → offline erkannt.
        assert_eq!(offline, 1);
        assert!(target.media.assets[0].offline);
        assert_eq!(target.timeline.settings.rate, FrameRate::new(30000, 1001));
        assert!(target.timeline.settings.drop_frame);
        assert_eq!(target.timeline.clips.len(), 3);
        assert_eq!(target.timeline.clips[0].start, 1.0);
        assert_eq!(target.timeline.clip("clip-3").unwrap().speed, 0.37);
        assert!(target.timeline.clip("clip-3").unwrap().reverse);
        assert_eq!(target.timeline.transitions.len(), 1, "Übergang geladen");
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
            grade: Default::default(),
            effects: Vec::new(),
            title: None,
            subtitle: None,
            speed: 1.0,
            reverse: false,
            freeze: false,
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
            speed: 1.0,
            reverse: false,
            freeze: false,
        });

        let dir = std::env::temp_dir().join(format!("editron-proj-title-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("titel.etron");
        save_to(&mut state, &path).expect("save");

        let file = load_from(&path).expect("load");
        assert_eq!(file.version, PROJECT_VERSION);
        let saved = file
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
        assert_eq!(file.timeline.active_subtitle_track_id.as_deref(), Some(u2.as_str()));

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
}

