//! Zeitgesteuertes Autosave mit Versionshistorie — der Sicherheitsgurt für
//! lange Schnitt-Sessions (Premiere/Resolve-Niveau: ein Stromausfall darf
//! höchstens ein Autosave-Intervall kosten).
//!
//! Versionskopien landen NEBEN der Projektdatei in `<ordner>/.etron-autosave/`
//! (`<name>_JJJJ-MM-TT_HH-MM-SS.etron`); die Originaldatei wird nie angefasst.
//! Ungespeicherte Projekte sichern in ein XDG-Verzeichnis. Geschrieben wird
//! atomar (tmp + rename) wie beim normalen Speichern, und es werden maximal K
//! Versionen je Projekt aufbewahrt (älteste rotieren raus).
//!
//! Die reine Logik (Zeitstempel-Format, Dateinamen, Rotation, Crash-Recovery-
//! Erkennung) ist von der I/O getrennt und unit-getestet.

use crate::state::AppState;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Ordnername der Versionskopien neben der Projektdatei.
pub const AUTOSAVE_DIR_NAME: &str = ".etron-autosave";
/// Länge des eingebetteten Zeitstempels `JJJJ-MM-TT_HH-MM-SS`.
const STAMP_LEN: usize = 19;
/// Basis-Stamm für noch nie gespeicherte Projekte. Geschrieben/rotiert wird
/// pro Prozess als `Unbenannt-<pid>` (damit parallele Sessions sich nicht
/// gegenseitig die Versionen wegrotieren), gelistet wird über den Basis-Stamm
/// als Umbrella (damit die Recovery alle Sessions sieht).
const UNSAVED_STEM_BASE: &str = "Unbenannt";

// ----------------------------------------------------------- Zeitstempel

/// UNIX-Sekunden einer [`SystemTime`] (vor 1970 negativ).
fn unix_secs(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// UNIX-Sekunden → `(Jahr, Monat, Tag, Stunde, Minute, Sekunde)` in UTC.
/// Standard-Algorithmus (Howard Hinnant, „chrono-Compatible Low-Level Date
/// Algorithms“) — bewusst dependency-frei. UTC ist sortierbar und eindeutig.
fn civil_utc(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let hour = (rem / 3600) as u32;
    let minute = ((rem % 3600) / 60) as u32;
    let second = (rem % 60) as u32;

    // Tage seit 1970-01-01 → bürgerliches Datum.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let year = y + if month <= 2 { 1 } else { 0 };
    (year, month, day, hour, minute, second)
}

/// Zeitstempel fürs Dateinamen-Muster: `JJJJ-MM-TT_HH-MM-SS` (UTC).
pub fn format_timestamp(t: SystemTime) -> String {
    let (y, mo, d, h, mi, s) = civil_utc(unix_secs(t));
    format!("{y:04}-{mo:02}-{d:02}_{h:02}-{mi:02}-{s:02}")
}

/// Dateiname einer Versionskopie: `<stem>_<zeitstempel>.etron`.
pub fn version_filename(stem: &str, t: SystemTime) -> String {
    format!("{stem}_{}.{}", format_timestamp(t), crate::core::project::PROJECT_EXT)
}

/// Eingebetteten Zeitstempel aus einem Dateinamen lesen und menschenlesbar
/// aufbereiten (`2026-06-14 12:30:05`). `None`, wenn das Muster nicht passt.
pub fn display_label(file_name: &str) -> Option<String> {
    let stem = file_name.strip_suffix(&format!(".{}", crate::core::project::PROJECT_EXT))?;
    if stem.len() < STAMP_LEN + 1 {
        return None;
    }
    let stamp = &stem[stem.len() - STAMP_LEN..];
    // Das Zeichen vor dem Stempel muss der `_`-Trenner sein.
    if !stem[..stem.len() - STAMP_LEN].ends_with('_') {
        return None;
    }
    // Format grob prüfen: `JJJJ-MM-TT_HH-MM-SS`.
    let ok = stamp.len() == STAMP_LEN
        && stamp.as_bytes()[10] == b'_'
        && stamp[..10].bytes().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 { c == b'-' } else { c.is_ascii_digit() }
        });
    if !ok {
        return None;
    }
    let date = &stamp[..10];
    let time = stamp[11..].replace('-', ":");
    Some(format!("{date} {time}"))
}

/// Projekt-Stamm aus einem Versionsdateinamen ableiten (Umkehr von
/// [`version_filename`]). Nützlich, wenn beim Start noch kein Projekt geladen
/// ist (Crash-Recovery): aus dem Versionspfad allein die übrigen Versionen
/// desselben Projekts finden.
pub fn stem_of_version(file_name: &str) -> Option<String> {
    let rest = file_name.strip_suffix(&format!(".{}", crate::core::project::PROJECT_EXT))?;
    if rest.len() < STAMP_LEN + 1 {
        return None;
    }
    let cut = rest.len() - STAMP_LEN - 1; // Position des `_`-Trenners
    if rest.as_bytes()[cut] != b'_' {
        return None;
    }
    Some(rest[..cut].to_string())
}

/// Prüft, ob `file_name` eine Versionskopie von `stem` ist.
fn matches_stem(file_name: &str, stem: &str) -> bool {
    let Some(rest) = file_name.strip_suffix(&format!(".{}", crate::core::project::PROJECT_EXT))
    else {
        return false;
    };
    // Umbrella: der Basis-Stamm "Unbenannt" matcht auch die session-spezifischen
    // Varianten "Unbenannt-<pid>" — so sieht die Recovery ALLE ungespeicherten
    // Sessions, während eine Rotation mit dem konkreten "Unbenannt-<pid>"-Stamm
    // nur die eigenen Versionen trifft.
    if stem == UNSAVED_STEM_BASE {
        let Some(after) = rest.strip_prefix(UNSAVED_STEM_BASE) else {
            return false;
        };
        // after = "_<STAMP>"  oder  "-<pid>_<STAMP>"
        let stamp = match after.strip_prefix('_') {
            Some(s) => s,
            None => match after.strip_prefix('-').and_then(|s| s.split_once('_')) {
                Some((pid, s)) if !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()) => s,
                _ => return false,
            },
        };
        return stamp.len() == STAMP_LEN;
    }
    let prefix = format!("{stem}_");
    match rest.strip_prefix(&prefix) {
        Some(stamp) => stamp.len() == STAMP_LEN,
        None => false,
    }
}

// ---------------------------------------------------------------- Pfade

/// Versions-Ordner neben einer Projektdatei (`<ordner>/.etron-autosave`).
pub fn autosave_dir_for(project_path: &Path) -> PathBuf {
    project_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(AUTOSAVE_DIR_NAME)
}

/// Versions-Ordner für noch nie gespeicherte Projekte (XDG-Datenverzeichnis).
pub fn unsaved_versions_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("editron")
        .join("autosave-versions")
}

/// Stamm-Dateiname (ohne Endung) einer Projektdatei, fallback „Unbenannt“.
fn project_stem(project_path: Option<&Path>) -> String {
    project_path
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| UNSAVED_STEM_BASE.to_string())
}

/// Schreib-/Rotations-Stamm für ungespeicherte Projekte: pro Prozess eindeutig,
/// damit parallele Sessions sich nicht gegenseitig die Versionen wegrotieren.
fn unsaved_write_stem() -> String {
    format!("{UNSAVED_STEM_BASE}-{}", std::process::id())
}

// ------------------------------------------------------------- Versionen

/// Eine Autosave-Version (für die Wiederherstellungs-Liste).
#[derive(Clone, Debug)]
pub struct Version {
    pub path: PathBuf,
    pub modified: SystemTime,
    pub size: u64,
    /// Menschenlesbarer Zeitstempel (`2026-06-14 12:30:05`).
    pub label: String,
}

/// Alle Versionen eines Projekts in einem Ordner, neueste zuerst (sortiert
/// nach dem eingebetteten Zeitstempel im Dateinamen ⇒ deterministisch).
pub fn list_versions(dir: &Path, stem: &str) -> Vec<Version> {
    let mut out: Vec<Version> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !matches_stem(&name, stem) {
            continue;
        }
        let meta = entry.metadata().ok();
        let modified = meta.as_ref().and_then(|m| m.modified().ok()).unwrap_or(UNIX_EPOCH);
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let label = display_label(&name).unwrap_or_else(|| format_timestamp(modified));
        out.push(Version { path: entry.path(), modified, size, label });
    }
    // Nach Dateiname absteigend = neuester Zeitstempel zuerst.
    out.sort_by(|a, b| b.path.file_name().cmp(&a.path.file_name()));
    out
}

/// Älteste Versionen über das Limit hinaus löschen. Liefert die gelöschten
/// Pfade. Sortierung nach Dateiname (= Zeitstempel) absteigend, die ersten
/// `max` bleiben.
pub fn rotate(dir: &Path, stem: &str, max: usize) -> Vec<PathBuf> {
    let max = max.max(1);
    let versions = list_versions(dir, stem);
    let mut removed = Vec::new();
    for v in versions.into_iter().skip(max) {
        if std::fs::remove_file(&v.path).is_ok() {
            removed.push(v.path);
        }
    }
    removed
}

/// Versionskopie atomar schreiben (tmp + rename), dann rotieren. Die
/// Originaldatei wird nie berührt.
pub fn write_version(
    json: &str,
    dir: &Path,
    stem: &str,
    t: SystemTime,
    max_versions: usize,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Autosave-Ordner anlegen: {e}"))?;
    let final_path = dir.join(version_filename(stem, t));
    // Durabel + atomar (fsync), damit ein Absturz beim Autosave keine korrupte
    // Versionsdatei hinterlässt.
    crate::core::atomic_write(&final_path, json.as_bytes())
        .map_err(|e| format!("Autosave schreiben: {e}"))?;
    rotate(dir, stem, max_versions);
    Ok(final_path)
}

/// Ablageort (Ordner + Stamm) zum SCHREIBEN/ROTIEREN der Versionen des
/// aktuellen Projekts (ungespeichert ⇒ prozess-eindeutiger Stamm).
pub fn target_for(state: &AppState) -> (PathBuf, String) {
    match state.project.path.as_deref() {
        Some(p) => (autosave_dir_for(p), project_stem(Some(p))),
        None => (unsaved_versions_dir(), unsaved_write_stem()),
    }
}

/// Ablageort (Ordner + Stamm) zum LISTEN/Wiederherstellen — ungespeichert über
/// den Umbrella-Basis-Stamm, damit Versionen ALLER (auch abgestürzter) Sessions
/// erscheinen, nicht nur die der aktuellen.
pub fn list_target_for(state: &AppState) -> (PathBuf, String) {
    match state.project.path.as_deref() {
        Some(p) => (autosave_dir_for(p), project_stem(Some(p))),
        None => (unsaved_versions_dir(), UNSAVED_STEM_BASE.to_string()),
    }
}

/// Eine zeitgesteuerte Versionskopie des aktuellen Projektzustands schreiben.
/// Serialisiert exakt wie das normale Speichern, fasst aber weder die
/// Originaldatei noch den Dirty-/Pfad-Zustand der Session an.
pub fn write_timed_autosave(
    state: &AppState,
    max_versions: usize,
    t: SystemTime,
) -> Result<PathBuf, String> {
    let file = crate::core::project::collect(state);
    let json = serde_json::to_string(&file).map_err(|e| format!("Serialisierung: {e}"))?;
    let (dir, stem) = target_for(state);
    write_version(&json, &dir, &stem, t, max_versions)
}

// --------------------------------------------------------- Crash-Recovery

/// Aus einer Versionsliste die jüngste Version ermitteln, die NEUER ist als
/// die Projektdatei (mtime-Vergleich). Ist die jüngste Version neuer, deutet
/// das auf einen Absturz nach dem letzten Autosave hin — beim normalen
/// Beenden würde Editron die Projektdatei speichern und sie damit zur
/// jüngsten Datei machen.
pub fn recovery_candidate(
    project_mtime: SystemTime,
    versions: &[Version],
) -> Option<&Version> {
    let newest = versions.iter().max_by_key(|v| v.modified)?;
    if newest.modified > project_mtime {
        Some(newest)
    } else {
        None
    }
}

/// Prüft beim Start, ob für `project_path` eine Autosave-Version existiert, die
/// neuer ist als die Projektdatei selbst (Absturz-Erkennung). Liefert die zu
/// empfehlende Version.
pub fn find_crash_recovery(project_path: &Path) -> Option<PathBuf> {
    let project_mtime = std::fs::metadata(project_path).and_then(|m| m.modified()).ok()?;
    let dir = autosave_dir_for(project_path);
    let stem = project_stem(Some(project_path));
    let versions = list_versions(&dir, &stem);
    recovery_candidate(project_mtime, &versions).map(|v| v.path.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(unix: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(unix)
    }

    #[test]
    fn timestamp_format_is_fixed_width_utc() {
        // 2021-01-01 00:00:00 UTC = 1609459200.
        assert_eq!(format_timestamp(at(1_609_459_200)), "2021-01-01_00-00-00");
        // 2026-06-18 13:45:30 UTC = 1781790330.
        assert_eq!(format_timestamp(at(1_781_790_330)), "2026-06-18_13-45-30");
    }

    #[test]
    fn unsaved_umbrella_matches_per_session_stems_but_rotation_is_isolated() {
        // Drei ungespeicherte Versionen: zwei Sessions (pids) + ein Legacy-Name.
        let a = version_filename("Unbenannt-1234", at(1_781_790_330));
        let b = version_filename("Unbenannt-5678", at(1_781_790_331));
        let legacy = version_filename(UNSAVED_STEM_BASE, at(1_781_790_332));
        // Umbrella-Stamm "Unbenannt" sieht ALLE (Recovery über Sessions hinweg).
        assert!(matches_stem(&a, UNSAVED_STEM_BASE));
        assert!(matches_stem(&b, UNSAVED_STEM_BASE));
        assert!(matches_stem(&legacy, UNSAVED_STEM_BASE));
        // Der konkrete Session-Stamm trifft nur die EIGENEN (Rotation isoliert).
        assert!(matches_stem(&a, "Unbenannt-1234"));
        assert!(!matches_stem(&b, "Unbenannt-1234"));
        assert!(!matches_stem(&legacy, "Unbenannt-1234"));
        // Kein falsches Matching: anderer Name, Nicht-Ziffern-Suffix.
        assert!(!matches_stem(&version_filename("Projekt", at(1)), UNSAVED_STEM_BASE));
        assert!(!matches_stem(&version_filename("Unbenannt-abc", at(1)), UNSAVED_STEM_BASE));
    }

    #[test]
    fn version_filename_matches_pattern() {
        let name = version_filename("Mein Film", at(1_781_790_330));
        assert_eq!(name, "Mein Film_2026-06-18_13-45-30.etron");
        assert!(matches_stem(&name, "Mein Film"));
        assert!(!matches_stem(&name, "Anderer"));
        assert_eq!(
            display_label(&name).as_deref(),
            Some("2026-06-18 13:45:30")
        );
        // Stamm-Rückgewinnung — auch bei Unterstrichen im Projektnamen.
        assert_eq!(stem_of_version(&name).as_deref(), Some("Mein Film"));
        let underscored = version_filename("my_cut_v2", at(1_781_790_330));
        assert_eq!(stem_of_version(&underscored).as_deref(), Some("my_cut_v2"));
        assert!(stem_of_version("fremd.etron").is_none());
    }

    #[test]
    fn display_label_rejects_foreign_names() {
        assert!(display_label("projekt.etron").is_none());
        assert!(display_label("backup.txt").is_none());
    }

    #[test]
    fn rotation_keeps_only_newest_k() {
        let dir = std::env::temp_dir().join(format!("editron-autosave-rot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // 6 Versionen mit aufsteigendem Zeitstempel schreiben, Limit 3.
        let base = 1_700_000_000u64;
        for i in 0..6 {
            write_version("{}", &dir, "proj", at(base + i * 60), 3).unwrap();
        }
        let kept = list_versions(&dir, "proj");
        assert_eq!(kept.len(), 3, "nur K Versionen bleiben");
        // base = 2023-11-14 22:13:20 UTC; i=5 (+300s) = 22:18:20 ist neueste.
        assert_eq!(kept[0].label, "2023-11-14 22:18:20");
        assert!(kept[0].path.file_name().unwrap().to_string_lossy()
            .contains("2023-11-14_22-18-20"));
        // Älteste (i=0, 22:13:20) wurde rotiert.
        assert!(!kept
            .iter()
            .any(|v| v.path.file_name().unwrap().to_string_lossy().contains("22-13-20")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_ignores_other_projects() {
        let dir = std::env::temp_dir().join(format!("editron-autosave-mix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let base = 1_700_000_000u64;
        for i in 0..3 {
            write_version("{}", &dir, "alpha", at(base + i * 60), 2).unwrap();
            write_version("{}", &dir, "beta", at(base + i * 60), 2).unwrap();
        }
        // Jedes Projekt rotiert unabhängig auf 2.
        assert_eq!(list_versions(&dir, "alpha").len(), 2);
        assert_eq!(list_versions(&dir, "beta").len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_detects_newer_autosave() {
        let project_mtime = at(1_000);
        let mk = |unix: u64| Version {
            path: PathBuf::from(format!("v_{unix}.etron")),
            modified: at(unix),
            size: 1,
            label: String::new(),
        };
        // Eine Version NEUER als das Projekt ⇒ Recovery angeboten.
        let newer = vec![mk(500), mk(2_000)];
        assert_eq!(
            recovery_candidate(project_mtime, &newer).map(|v| v.modified),
            Some(at(2_000))
        );
        // Alle Versionen älter ⇒ kein Recovery (sauber beendet, Projekt jünger).
        let older = vec![mk(500), mk(900)];
        assert!(recovery_candidate(project_mtime, &older).is_none());
        // Keine Versionen ⇒ kein Recovery.
        assert!(recovery_candidate(project_mtime, &[]).is_none());
    }

    #[test]
    fn autosave_dir_sits_next_to_project() {
        let p = Path::new("/work/cut/film.etron");
        assert_eq!(autosave_dir_for(p), PathBuf::from("/work/cut/.etron-autosave"));
    }
}
