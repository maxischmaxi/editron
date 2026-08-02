//! Projekt konsolidieren („Consolidate“): alle benutzten (oder alle
//! importierten) Medien in einen Zielordner `<ziel>/media` einsammeln, das
//! Projekt portabel (mit relativen Pfaden) als `<ziel>/<name>.etron` ablegen
//! und die Assets auf die Kopien umbiegen — damit Projekte übergebbar und
//! archivierbar werden.
//!
//! Aufteilung: dieses Modul plant ([`build_plan`]) und übernimmt das Ergebnis
//! ([`finish`]) im UI-Thread (reine, testbare Logik). Das eigentliche Kopieren/
//! Trimmen läuft in einem Worker (`services::Services::start_consolidate`), der
//! je Item ein [`ConsolidateResult`] zurückmeldet.

use crate::core::types::{MediaInfo, MediaKind};
use crate::state::AppState;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const EPS: f64 = 1e-6;
/// Kürzeste sinnvolle getrimmte Mediendauer; darunter wird die ganze Datei
/// kopiert (degenerierte Bereiche vermeiden).
const MIN_TRIM_DUR: f64 = 0.1;

/// Welche Medien eingesammelt werden.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AssetScope {
    /// Nur in der Timeline (irgendeiner Sequenz) benutzte Medien.
    UsedOnly,
    /// Alle importierten Medien des Projekts.
    All,
}

/// Optionen des Konsolidierungs-Dialogs.
#[derive(Clone, Debug)]
pub struct ConsolidateOptions {
    pub scope: AssetScope,
    /// Medien auf die benutzten Bereiche kürzen (mit Reserve/Handles). Trimmen
    /// kodiert neu (best-effort; schlägt das fehl, wird die ganze Datei kopiert).
    pub trim: bool,
    /// Reserve vor/nach dem benutzten Bereich in Sekunden (nur bei `trim`).
    pub handle_sec: f64,
    /// Name der Projektdatei (ohne Endung) im Zielordner.
    pub project_name: String,
}

impl Default for ConsolidateOptions {
    fn default() -> Self {
        ConsolidateOptions {
            scope: AssetScope::UsedOnly,
            trim: false,
            handle_sec: 1.0,
            project_name: "Projekt".to_string(),
        }
    }
}

/// Ein zu kopierendes/trimmendes Medium.
#[derive(Clone, Debug)]
pub struct ConsolidateItem {
    pub asset_id: String,
    /// ORIGINAL-Quelldatei (absolut).
    pub src: String,
    /// Zieldatei unterhalb `<ziel>/media` (absolut).
    pub dst: PathBuf,
    pub kind: MediaKind,
    /// Bei `Some((start, dur))` wird neu kodiert getrimmt; `None` = ganze Datei.
    pub trim: Option<(f64, f64)>,
    /// Bestehende Asset-Info (Kopier-Modus aktualisiert nur Pfad/Größe daraus).
    pub info: MediaInfo,
}

/// Ergebnis eines Items (vom Worker erzeugt, von [`finish`] übernommen).
#[derive(Clone, Debug)]
pub struct ConsolidateResult {
    pub asset_id: String,
    pub ok: bool,
    pub error: Option<String>,
    /// Neue Asset-Info (Pfad zeigt auf die Kopie). `None` bei Fehler.
    pub info: Option<MediaInfo>,
    pub thumbnail_path: Option<String>,
    /// Tatsächlich angewandter Trim-Versatz in Quell-Sekunden (0 = ganze Datei
    /// kopiert / Trim fehlgeschlagen). Clips werden um diesen Wert verschoben.
    pub trim_start: f64,
}

/// Geplante Konsolidierung.
#[derive(Clone, Debug)]
pub struct ConsolidatePlan {
    pub items: Vec<ConsolidateItem>,
    /// Zielpfad der Projektdatei (`<ziel>/<name>.etron`).
    pub etron_path: PathBuf,
    /// Übersprungene Assets (Quelle offline/fehlt) — Namen für die Meldung.
    pub skipped: Vec<String>,
}

/// Zusammenfassung der übernommenen Konsolidierung (für die Statusmeldung).
#[derive(Clone, Debug, Default)]
pub struct ConsolidateOutcome {
    pub copied: usize,
    pub failed: Vec<String>,
    pub skipped: usize,
    pub saved: bool,
    pub save_error: Option<String>,
}

/// Medien-Nutzung des Projekts: benutzte Asset-IDs, je Asset die belegte
/// Medienspanne (lo..hi in Quell-Sekunden) und die Multicam-gebundenen Assets
/// (die nicht getrimmt werden dürfen, weil ihre τ-Abbildung sonst verrutscht).
struct Usage {
    referenced: HashSet<String>,
    ranges: HashMap<String, (f64, f64)>,
    multicam_locked: HashSet<String>,
}

fn analyze_usage(state: &AppState) -> Usage {
    let mut referenced = HashSet::new();
    let mut ranges: HashMap<String, (f64, f64)> = HashMap::new();
    let mut multicam_locked = HashSet::new();
    for seq in state.timeline.iter() {
        let is_mc_source = seq.timeline.multicam.is_some();
        // Winkel-Assets einer Multicam-Quelle sind direkt referenziert und
        // dürfen nicht getrimmt werden (Sync-Offset pos auf τ).
        if let Some(mc) = &seq.timeline.multicam {
            for a in &mc.angles {
                if !a.asset_id.is_empty() {
                    referenced.insert(a.asset_id.clone());
                    multicam_locked.insert(a.asset_id.clone());
                }
            }
        }
        for clip in &seq.timeline.clips {
            if clip.asset_id.is_empty() {
                continue; // Generator/Nest/Multicam-Clip ohne Mediendatei
            }
            referenced.insert(clip.asset_id.clone());
            if is_mc_source {
                multicam_locked.insert(clip.asset_id.clone());
            }
            let lo = clip.media_in();
            let hi = clip.media_out();
            if lo.is_finite() && hi.is_finite() && hi > lo {
                let e = ranges.entry(clip.asset_id.clone()).or_insert((lo, hi));
                e.0 = e.0.min(lo);
                e.1 = e.1.max(hi);
            }
        }
    }
    Usage {
        referenced,
        ranges,
        multicam_locked,
    }
}

/// Projektname auf einen sicheren Dateistamm reduzieren (Pfadtrenner raus).
fn sanitize_stem(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| if std::path::is_separator(c) || c == '\0' { '_' } else { c })
        .collect();
    if cleaned.trim().is_empty() {
        "Projekt".to_string()
    } else {
        cleaned
    }
}

/// Eindeutigen Zieldateinamen unter `media/` finden (Kollisionen → `_1`, `_2`).
fn unique_name(file_name: &str, taken: &mut HashSet<String>) -> String {
    // Nur die letzte Pfadkomponente verwenden (Sicherheit).
    let base = Path::new(file_name)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "media".to_string());
    if taken.insert(base.to_lowercase()) {
        return base;
    }
    let p = Path::new(&base);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|s| s.to_string_lossy().into_owned());
    for n in 1.. {
        let candidate = match &ext {
            Some(e) => format!("{stem}_{n}.{e}"),
            None => format!("{stem}_{n}"),
        };
        if taken.insert(candidate.to_lowercase()) {
            return candidate;
        }
    }
    unreachable!()
}

/// Eine Konsolidierung aus dem aktuellen Zustand planen. `target_dir` ist der
/// vom Nutzer gewählte (noch leere/neue) Zielordner.
pub fn build_plan(
    state: &AppState,
    target_dir: &Path,
    opts: &ConsolidateOptions,
) -> Result<ConsolidatePlan, String> {
    let base_dir = target_dir.to_path_buf();
    let media_dir = base_dir.join("media");
    let stem = sanitize_stem(&opts.project_name);
    let etron_path = base_dir.join(format!("{stem}.{}", crate::core::project::PROJECT_EXT));

    let usage = analyze_usage(state);
    let mut taken: HashSet<String> = HashSet::new();
    let mut items = Vec::new();
    let mut skipped = Vec::new();

    for asset in &state.media.assets {
        let include = match opts.scope {
            AssetScope::All => true,
            AssetScope::UsedOnly => usage.referenced.contains(&asset.id),
        };
        if !include {
            continue;
        }
        // Quelle muss vorhanden sein (Offline/relinkbar wird übersprungen).
        if asset.offline || !Path::new(&asset.path).exists() {
            skipped.push(asset.name.clone());
            continue;
        }
        // Bildsequenzen (VFX-Renders) bestehen aus vielen Einzelframes; das
        // Konsolidieren würde nur den ersten Frame kopieren (Datenverlust).
        // Bis zum vollständigen Folder-Copy bleiben sie bewusst ausgeklammert.
        if asset.image_seq.is_some() {
            skipped.push(asset.name.clone());
            continue;
        }

        let dst_name = unique_name(&asset.info.file_name, &mut taken);
        let dst = media_dir.join(&dst_name);

        // Trim-Entscheidung: nur benutzte, nicht-Multicam, zeitbasierte Medien
        // mit endlicher Dauer und bekannter benutzter Spanne.
        let trim = if opts.trim
            && asset.kind != MediaKind::Image
            && asset.info.duration_sec.is_finite()
            && asset.info.duration_sec > MIN_TRIM_DUR
            && !usage.multicam_locked.contains(&asset.id)
        {
            usage.ranges.get(&asset.id).and_then(|&(lo, hi)| {
                let dur = asset.info.duration_sec;
                let handle = opts.handle_sec.max(0.0);
                let start = (lo - handle).max(0.0);
                let end = (hi + handle).min(dur);
                let span = end - start;
                // Bringt nichts, wenn quasi die ganze Datei gebraucht wird.
                if start <= EPS && end >= dur - EPS {
                    None
                } else if span < MIN_TRIM_DUR {
                    None
                } else {
                    Some((start, span))
                }
            })
        } else {
            None
        };

        items.push(ConsolidateItem {
            asset_id: asset.id.clone(),
            src: asset.path.clone(),
            dst,
            kind: asset.kind,
            trim,
            info: asset.info.clone(),
        });
    }

    if items.is_empty() {
        return Err(if skipped.is_empty() {
            "Keine Medien zum Konsolidieren gefunden".to_string()
        } else {
            "Alle infrage kommenden Medien sind offline".to_string()
        });
    }

    Ok(ConsolidatePlan {
        items,
        etron_path,
        skipped,
    })
}

/// Das Worker-Ergebnis übernehmen: Assets auf die Kopien umbiegen, getrimmte
/// Clips verschieben, das Projekt portabel speichern. Mutiert den App-Zustand
/// und schreibt die `.etron`.
pub fn finish(
    state: &mut AppState,
    plan: &ConsolidatePlan,
    results: Vec<ConsolidateResult>,
) -> ConsolidateOutcome {
    let mut outcome = ConsolidateOutcome {
        skipped: plan.skipped.len(),
        ..Default::default()
    };

    for res in results {
        if !res.ok {
            let name = plan
                .items
                .iter()
                .find(|i| i.asset_id == res.asset_id)
                .map(|i| {
                    Path::new(&i.src)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| i.src.clone())
                })
                .unwrap_or_else(|| res.asset_id.clone());
            outcome.failed.push(match &res.error {
                Some(e) => format!("{name}: {e}"),
                None => name,
            });
            continue;
        }
        let Some(info) = res.info else { continue };
        // Asset auf die Kopie umbiegen.
        if let Some(asset) = state.media.assets.iter_mut().find(|a| a.id == res.asset_id) {
            asset.path = info.path.clone();
            asset.info = info.clone();
            asset.name = info.file_name.clone();
            asset.offline = false;
            // Proxy verweist auf das (alte) Original und ist nach Trim ohnehin
            // ungültig — verwerfen, regeneriert bei Bedarf.
            asset.proxy_path = None;
            asset.proxy_src_mtime = None;
            asset.proxy_offline = false;
            asset.thumbnail_path = res.thumbnail_path.clone();
            // Asset-/Quell-Marker liegen in Quell-Sekunden → mit dem Trim
            // verschieben.
            if res.trim_start > EPS {
                for m in &mut asset.markers {
                    m.time = (m.time - res.trim_start).max(0.0);
                }
            }
            outcome.copied += 1;
        }

        // Getrimmt: alle referenzierenden Clips in ALLEN Sequenzen verschieben.
        if res.trim_start > EPS {
            let new_dur = info.duration_sec;
            for seq in state.timeline.iter_mut() {
                let mut changed = false;
                for clip in seq.timeline.clips.iter_mut() {
                    if clip.asset_id == res.asset_id {
                        clip.src_in = (clip.src_in - res.trim_start).max(0.0);
                        if clip.src_duration.is_finite() {
                            clip.src_duration = new_dur;
                        }
                        changed = true;
                    }
                }
                if changed {
                    seq.timeline.revision += 1;
                }
            }
        }
    }
    state.media.revision += 1;

    // Portabel speichern: Pfade unterhalb des Zielordners werden relativ.
    state.project.portable = true;
    match crate::core::project::save_to(state, &plan.etron_path) {
        Ok(()) => outcome.saved = true,
        Err(e) => outcome.save_error = Some(e),
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bin::ROOT_BIN_ID;
    use crate::core::timeline::{TimelineClip, TrackKind};
    use crate::core::types::{MediaAsset, MediaInfo};

    fn mk_info(path: &str, file_name: &str, dur: f64) -> MediaInfo {
        MediaInfo {
            path: path.into(),
            file_name: file_name.into(),
            container: "mov,mp4".into(),
            duration_sec: dur,
            size_bytes: 10,
            video: Vec::new(),
            audio: Vec::new(),
            recorded_at: None,
        }
    }

    fn mk_asset(id: &str, path: &str, file_name: &str, dur: f64) -> MediaAsset {
        MediaAsset {
            extra: Default::default(),
            id: id.into(),
            path: path.into(),
            name: file_name.into(),
            kind: MediaKind::Video,
            info: mk_info(path, file_name, dur),
            thumbnail_path: None,
            imported_at: 0.0,
            bin_id: ROOT_BIN_ID.to_string(),
            label: None,
            offline: false,
            markers: Vec::new(),
            proxy_path: None,
            proxy_src_mtime: None,
            proxy_offline: false,
            image_seq: None,
        }
    }

    fn clip_using(asset_id: &str, track_id: &str, start: f64, dur: f64, src_in: f64, src_dur: f64) -> TimelineClip {
        TimelineClip {
            extra: Default::default(),
            id: crate::core::types::new_id(),
            track_id: track_id.into(),
            asset_id: asset_id.into(),
            name: "clip".into(),
            kind: TrackKind::Video,
            start,
            duration: dur,
            src_in,
            src_duration: src_dur,
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
        }
    }

    /// Zwei reale Quelldateien in einem Temp-Verzeichnis erzeugen.
    fn temp_sources(tag: &str) -> (PathBuf, String, String) {
        let dir = std::env::temp_dir().join(format!("editron-consol-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.mp4");
        let b = dir.join("b.mp4");
        std::fs::write(&a, b"aaa").unwrap();
        std::fs::write(&b, b"bbb").unwrap();
        (dir.clone(), a.to_string_lossy().into_owned(), b.to_string_lossy().into_owned())
    }

    #[test]
    fn used_only_scope_excludes_unreferenced_assets() {
        let (_dir, a_path, b_path) = temp_sources("used");
        let mut state = AppState::default();
        state.media.add_asset(mk_asset("a", &a_path, "a.mp4", 10.0));
        state.media.add_asset(mk_asset("b", &b_path, "b.mp4", 10.0)); // ungenutzt
        let tid = state.timeline.tracks[0].id.clone();
        state.timeline.clips.push(clip_using("a", &tid, 0.0, 4.0, 2.0, 10.0));

        let target = std::env::temp_dir().join(format!("editron-consol-out-{}", std::process::id()));
        let opts = ConsolidateOptions {
            scope: AssetScope::UsedOnly,
            ..Default::default()
        };
        let plan = build_plan(&state, &target, &opts).expect("plan");
        assert_eq!(plan.items.len(), 1, "nur das benutzte Asset");
        assert_eq!(plan.items[0].asset_id, "a");
        assert!(plan.items[0].dst.ends_with("media/a.mp4"));
        assert!(plan.items[0].trim.is_none(), "kein Trim ohne Option");

        // All-Scope nimmt beide.
        let all = build_plan(&state, &target, &ConsolidateOptions { scope: AssetScope::All, ..Default::default() }).unwrap();
        assert_eq!(all.items.len(), 2);
    }

    #[test]
    fn trim_computes_used_range_with_handles() {
        let (_dir, a_path, _b) = temp_sources("trim");
        let mut state = AppState::default();
        state.media.add_asset(mk_asset("a", &a_path, "a.mp4", 60.0));
        let tid = state.timeline.tracks[0].id.clone();
        // Benutzt Medienzeit [20, 24] (src_in 20, dauer 4, speed 1).
        state.timeline.clips.push(clip_using("a", &tid, 0.0, 4.0, 20.0, 60.0));

        let target = std::env::temp_dir().join("editron-consol-trim-out");
        let opts = ConsolidateOptions { scope: AssetScope::UsedOnly, trim: true, handle_sec: 1.0, ..Default::default() };
        let plan = build_plan(&state, &target, &opts).unwrap();
        let (start, span) = plan.items[0].trim.expect("getrimmt");
        // [20-1, 24+1] = [19, 25] → start 19, span 6.
        assert!((start - 19.0).abs() < 1e-6, "start war {start}");
        assert!((span - 6.0).abs() < 1e-6, "span war {span}");
    }

    #[test]
    fn duplicate_file_names_are_deduplicated() {
        let dir = std::env::temp_dir().join(format!("editron-consol-dup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("x")).unwrap();
        std::fs::create_dir_all(dir.join("y")).unwrap();
        let p1 = dir.join("x").join("clip.mp4");
        let p2 = dir.join("y").join("clip.mp4");
        std::fs::write(&p1, b"1").unwrap();
        std::fs::write(&p2, b"2").unwrap();
        let mut state = AppState::default();
        state.media.add_asset(mk_asset("a", &p1.to_string_lossy(), "clip.mp4", 5.0));
        state.media.add_asset(mk_asset("b", &p2.to_string_lossy(), "clip.mp4", 5.0));
        let plan = build_plan(&state, &dir.join("out"), &ConsolidateOptions { scope: AssetScope::All, ..Default::default() }).unwrap();
        let names: Vec<String> = plan
            .items
            .iter()
            .map(|i| i.dst.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"clip.mp4".to_string()));
        assert!(names.contains(&"clip_1.mp4".to_string()), "Kollision umbenannt: {names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finish_shifts_clip_src_in_by_trim_start() {
        let (_dir, a_path, _b) = temp_sources("finish");
        let mut state = AppState::default();
        state.media.add_asset(mk_asset("a", &a_path, "a.mp4", 60.0));
        let tid = state.timeline.tracks[0].id.clone();
        state.timeline.clips.push(clip_using("a", &tid, 0.0, 4.0, 20.0, 60.0));

        let target = std::env::temp_dir().join(format!("editron-consol-fin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&target);
        let plan = build_plan(
            &state,
            &target,
            &ConsolidateOptions { scope: AssetScope::UsedOnly, trim: true, handle_sec: 1.0, ..Default::default() },
        )
        .unwrap();
        // Worker simulieren: getrimmte Datei mit start=19, neue Dauer 6.
        let dst = plan.items[0].dst.clone();
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::write(&dst, b"trimmed").unwrap();
        let new_info = mk_info(&dst.to_string_lossy(), "a.mp4", 6.0);
        let results = vec![ConsolidateResult {
            asset_id: "a".into(),
            ok: true,
            error: None,
            info: Some(new_info),
            thumbnail_path: None,
            trim_start: 19.0,
        }];
        let outcome = finish(&mut state, &plan, results);
        assert_eq!(outcome.copied, 1);
        assert!(outcome.saved, "Projekt gespeichert: {:?}", outcome.save_error);
        // Clip-src_in um 19 verschoben (20 → 1), src_duration = neue Dauer.
        let clip = &state.timeline.clips[0];
        assert!((clip.src_in - 1.0).abs() < 1e-6, "src_in war {}", clip.src_in);
        assert!((clip.src_duration - 6.0).abs() < 1e-6);
        // Asset zeigt auf die Kopie, portabel-Flag gesetzt.
        assert_eq!(state.media.assets[0].path, dst.to_string_lossy());
        assert!(state.project.portable);
        assert!(plan.etron_path.exists());
        let _ = std::fs::remove_dir_all(&target);
    }
}
