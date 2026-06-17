//! Command-Registry + when-Klausel-Evaluator + alle Builtin-Commands:
//! Jede Aktion läuft über die Registry und ist frei mit Shortcuts belegbar.

use crate::core::marker::MarkerScope;
use crate::core::playback::{self, PlaybackCmd};
use crate::core::render_cache::{render_cache_dir, RenderCacheStore, RenderProgress};
use crate::core::timeline::{sequence_end, TrackKind, TrimEdge};
use crate::services::Services;
use crate::state::{set_active_workspace, AppState};
use crate::stores::{
    tool_command_title, workspace_name, DialogId, MarkerEditTarget, TOOLS, WORKSPACE_IDS,
};
use serde_json::Value;

pub struct CommandCtx<'a> {
    pub state: &'a mut AppState,
    pub services: &'a Services,
    pub now: f64,
}

type RunFn = fn(&mut CommandCtx, Option<&Value>);

pub struct Command {
    pub id: String,
    pub title: String,
    pub category: &'static str,
    pub when: Option<&'static str>,
    pub allow_repeat: bool,
    /// Statisch gebundenes Argument (z. B. Workspace-ID); explizite Args gewinnen.
    pub bound_arg: Option<Value>,
    pub run: RunFn,
}

pub struct CommandRegistry {
    commands: Vec<Command>,
}

impl CommandRegistry {
    pub fn get(&self, id: &str) -> Option<&Command> {
        self.commands.iter().find(|c| c.id == id)
    }

    /// Alle Commands, sortiert nach Kategorie und Titel (Befehlspalette).
    pub fn all(&self) -> &[Command] {
        &self.commands
    }

    pub fn is_enabled(&self, id: &str, state: &AppState) -> bool {
        match self.get(id) {
            Some(cmd) => evaluate_when(cmd.when, state),
            None => false,
        }
    }

    /// Führt einen Command aus, sofern er existiert und sein Kontext passt.
    pub fn execute(&self, id: &str, args: Option<&Value>, ctx: &mut CommandCtx) -> bool {
        let Some(cmd) = self.get(id) else {
            eprintln!("[commands] Unbekannter Command: \"{id}\"");
            return false;
        };
        if !evaluate_when(cmd.when, ctx.state) {
            return false;
        }
        let bound = cmd.bound_arg.clone();
        let run = cmd.run;
        run(ctx, args.or(bound.as_ref()));
        true
    }
}

// ------------------------------------------------------------------ Kontext

/// Kontextwerte für when-Klauseln, direkt aus dem App-Zustand abgeleitet
/// (kein separater Kontext-Store — immer konsistent).
pub fn context_value(state: &AppState, key: &str) -> Value {
    match key {
        "panel" => state.app.focused_panel.clone().into(),
        "workspace" => state.app.active_workspace.clone().into(),
        "dialogOpen" => state.app.open_dialog.is_some().into(),
        "commandPaletteOpen" => state.app.command_palette_open.into(),
        "tool" => state.app.active_tool.into(),
        "mediaSelected" => (!state.media.selected_asset_ids.is_empty()).into(),
        "timelineHasClips" => (!state.timeline.clips.is_empty()).into(),
        "timelineClipSelected" => (!state.timeline.selected_clip_ids.is_empty()).into(),
        "timelineTransitionSelected" => {
            (!state.timeline.selected_transition_ids.is_empty()).into()
        }
        "timelineClipboard" => (!state.timeline.clipboard.is_empty()).into(),
        "timelineHasSubtitles" => state
            .timeline
            .tracks
            .iter()
            .any(|t| t.kind == TrackKind::Subtitle)
            .into(),
        "timelineAttrClipboard" => state.timeline.has_attr_clipboard().into(),
        "colorGradeClipboard" => state.grade_clipboard.is_some().into(),
        "timelineCanUndo" => state.timeline.can_undo().into(),
        "timelineCanRedo" => state.timeline.can_redo().into(),
        "mediaCanUndo" => state.media.can_undo().into(),
        "mediaCanRedo" => state.media.can_redo().into(),
        "canUndo" => (state.timeline.can_undo() || state.media.can_undo()).into(),
        "canRedo" => (state.timeline.can_redo() || state.media.can_redo()).into(),
        "timelineInOutSet" => {
            (state.timeline.in_point.is_some() || state.timeline.out_point.is_some()).into()
        }
        "hasClips" => (!state.timeline.clips.is_empty()).into(),
        "projectDirty" => state.project.dirty.into(),
        "projectHasPath" => state.project.path.is_some().into(),
        "mediaOffline" => (state.media.offline_count() > 0).into(),
        "proxyEnabled" => state.media.use_proxies.into(),
        // Genau ein Clip ausgewählt, der eine verschachtelte Sequenz ist.
        "nestClipSelected" => (state.timeline.selected_clip_ids.len() == 1
            && state
                .timeline
                .selected_clip_ids
                .first()
                .and_then(|id| state.timeline.clip(id))
                .is_some_and(|c| c.is_nest()))
        .into(),
        // Mehr als eine Sequenz vorhanden (löschbar).
        "sequenceCanDelete" => (state.timeline.len() > 1).into(),
        // Mehrfachauswahl im Browser (≥ 2 Assets) — Multicam-Quelle erstellbar.
        "mediaMultiSelected" => (state.media.selected_asset_ids.len() >= 2).into(),
        // Multicam-Monitor aktiv (Programmmonitor zeigt das Winkel-Raster):
        // gating der Zifferntasten-Live-Schnitt-Bindings.
        "multicamActive" => {
            (state.monitor.view == crate::stores::MonitorView::Multicam).into()
        }
        // Eine Effekt-Maske wird gerade im Monitor bearbeitet (Gizmo aktiv).
        "maskEditing" => state.app.active_mask.is_some().into(),
        // Mindestens ein ausgewählter Clip ist ein Multicam-Clip.
        "multicamClipSelected" => state
            .timeline
            .selected_clip_ids
            .iter()
            .any(|id| state.timeline.clip(id).is_some_and(|c| c.is_multicam()))
            .into(),
        _ => Value::Null,
    }
}

fn value_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn value_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

/// Wertet eine when-Klausel aus. Syntax (bewusst klein, keine Klammern):
/// `key`, `!key`, `key == wert`, `key != wert`, `a && b`, `a || b`.
pub fn evaluate_when(expr: Option<&str>, state: &AppState) -> bool {
    let Some(expr) = expr else { return true };
    if expr.trim().is_empty() {
        return true;
    }
    expr.split("||").any(|or_part| {
        or_part
            .split("&&")
            .all(|atom| eval_atom(atom.trim(), state))
    })
}

fn eval_atom(atom: &str, state: &AppState) -> bool {
    if atom.is_empty() {
        return true;
    }
    for op in ["==", "!="] {
        if let Some((key, raw)) = atom.split_once(op) {
            let key = key.trim();
            let raw = raw.trim().trim_matches('\'');
            let actual = value_string(&context_value(state, key));
            let equal = actual == raw;
            return if op == "==" { equal } else { !equal };
        }
    }
    let (neg, key) = match atom.strip_prefix('!') {
        Some(rest) => (true, rest.trim()),
        None => (false, atom),
    };
    let val = value_truthy(&context_value(state, key));
    if neg {
        !val
    } else {
        val
    }
}

// ----------------------------------------------------------------- Builtins

fn arg_asset_id(ctx: &CommandCtx, args: Option<&Value>) -> Option<String> {
    if let Some(Value::Object(map)) = args {
        if let Some(Value::String(id)) = map.get("assetId") {
            return Some(id.clone());
        }
    }
    ctx.state.media.selected_asset_ids.first().cloned()
}

/// String-Argument eines Commands lesen (z. B. `binId`, `label`).
fn arg_str(args: Option<&Value>, key: &str) -> Option<String> {
    args.and_then(|v| v.get(key))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Ziel-Assets einer Selektions-Aktion: explizites `assetId`-Argument, sonst
/// die gesamte Medien-Auswahl.
fn target_asset_ids(ctx: &CommandCtx, args: Option<&Value>) -> Vec<String> {
    if let Some(id) = arg_str(args, "assetId") {
        return vec![id];
    }
    ctx.state.media.selected_asset_ids.clone()
}

/// Ausgewählte Medien tatsächlich entfernen (samt verwendender Clips und
/// Quellmonitor-Bezug). Gemeinsame Logik für direktes und bestätigtes Entfernen.
fn remove_selected_media(ctx: &mut CommandCtx) {
    let ids = ctx.state.media.selected_asset_ids.clone();
    if ids.is_empty() {
        return;
    }
    ctx.state.timeline.remove_clips_for_assets(&ids);
    if let Some(src) = &ctx.state.playback.source_asset_id {
        if ids.contains(src) {
            ctx.state.playback.source_asset_id = None;
            ctx.state.playback.source = Default::default();
        }
    }
    ctx.state.media.remove_assets(&ids);
}

/// Proxy-Transcode-Aufträge für eine Asset-Auswahl bauen: nur Video-Assets mit
/// vorhandenem Original, ohne bereits gültigen Proxy und ohne laufenden Job
/// (so wirkt der Befehl zugleich als Retry für fehlgeschlagene). Pfade +
/// Encode-Argumente kommen aus den Proxy-Einstellungen des Projekts.
fn build_proxy_tasks(ctx: &CommandCtx, ids: &[String]) -> Vec<crate::services::ProxyTask> {
    use crate::core::proxy;
    use crate::core::types::MediaKind;
    use crate::stores::ProxyJobStatus;

    let project_path = ctx.state.project.path.clone();
    let settings = ctx.state.media.proxy_settings.clone();
    let mut tasks = Vec::new();
    for id in ids {
        let Some(asset) = ctx.state.media.asset(id) else { continue };
        if asset.kind != MediaKind::Video || asset.offline {
            continue; // nur Video; das Original muss vorhanden sein
        }
        if asset.has_valid_proxy() {
            continue; // schon ein gültiger Proxy vorhanden
        }
        if matches!(ctx.state.media.proxy_status(id), Some(ProxyJobStatus::Building(_))) {
            continue; // läuft bereits
        }
        let Some(v) = asset.info.video.first() else { continue };
        let (w, h) = proxy::proxy_dims(v.width, v.height, settings.scale);
        let out = proxy::proxy_output_path(&settings, project_path.as_deref(), &asset.path, &asset.id);
        tasks.push(crate::services::ProxyTask {
            asset_id: id.clone(),
            src: asset.path.clone(),
            out,
            encode_args: proxy::encode_args(settings.codec, w, h, v.fps),
            duration: asset.info.duration_sec,
        });
    }
    tasks
}

/// Proxy-Aufträge starten + Statusmeldung (gemeinsam für „Auswahl“/„alle“).
fn dispatch_proxy_tasks(ctx: &mut CommandCtx, ids: &[String]) {
    // Preflight: fehlt der gewählte Proxy-Encoder in dieser ffmpeg-Installation,
    // würde jeder Transcode mit kryptischem Fehler scheitern — klar abfangen.
    let encoder = ctx.state.media.proxy_settings.codec.encoder();
    if let Some(set) = &ctx.state.app.encoders {
        if !set.contains(encoder) {
            let label = ctx.state.media.proxy_settings.codec.label();
            status(
                ctx,
                &format!("Proxy-Encoder „{encoder}“ ({label}) fehlt in dieser FFmpeg-Installation"),
            );
            return;
        }
    }
    let tasks = build_proxy_tasks(ctx, ids);
    if tasks.is_empty() {
        status(
            ctx,
            "Keine Proxys zu erstellen (kein Video, bereits vorhanden oder Original offline)",
        );
        return;
    }
    let n = tasks.len();
    ctx.services.start_proxy_jobs(tasks);
    let msg = if n == 1 {
        "Erstelle 1 Proxy …".to_string()
    } else {
        format!("Erstelle {n} Proxys …")
    };
    status(ctx, &msg);
}

fn cycle_workspace(ctx: &mut CommandCtx, offset: i32) {
    let current = ctx.state.app.active_workspace.clone();
    let index = WORKSPACE_IDS
        .iter()
        .position(|w| *w == current)
        .unwrap_or(0) as i32;
    let len = WORKSPACE_IDS.len() as i32;
    let next = WORKSPACE_IDS[((index + offset + len) % len) as usize];
    set_active_workspace(ctx.state, next);
}

fn status(ctx: &mut CommandCtx, msg: &str) {
    let now = ctx.now;
    ctx.state.app.set_status_message(Some(msg.to_string()), now);
}

/// Ziel-Effekt für „Maske hinzufügen“: der gerade bearbeitete Masken-Effekt,
/// sonst der erste Video-Effekt des ersten ausgewählten Clips. None, wenn kein
/// ausgewählter Clip einen Video-Effekt trägt.
fn mask_target_effect(state: &AppState) -> Option<(String, String)> {
    if let Some(sel) = &state.app.active_mask {
        let still = state
            .timeline
            .clip(&sel.clip_id)
            .is_some_and(|c| c.effects.iter().any(|e| e.id == sel.fx_id));
        if still {
            return Some((sel.clip_id.clone(), sel.fx_id.clone()));
        }
    }
    for clip_id in &state.timeline.selected_clip_ids {
        if let Some(clip) = state.timeline.clip(clip_id) {
            if let Some(fx) = clip.effects.iter().find(|e| !e.kind.is_audio()) {
                return Some((clip.id.clone(), fx.id.clone()));
            }
        }
    }
    None
}

/// Tastatur-Nudge: verschiebt die Auswahl um `frames` Frames bzw. trimmt im
/// Trim-Werkzeug-Kontext (Ripple/Rolling) die aktive Kante. Das aktive
/// Werkzeug entscheidet — sonst alles wie ein Move.
fn nudge_timeline(ctx: &mut CommandCtx, frames: f64) {
    match ctx.state.app.active_tool {
        "ripple" => ctx.state.timeline.nudge_active_edge(frames, false),
        "rolling" => ctx.state.timeline.nudge_active_edge(frames, true),
        _ => ctx.state.timeline.nudge_selected_clips(frames),
    }
}

/// Quell-Farbkorrektur für „Grade kopieren“ aus der aktuellen Auswahl
/// auflösen. Bevorzugt — wie das Farbe-Panel (`panels::color`) den Ziel-Clip
/// wählt — einen ausgewählten Video-Clip bzw. den Video-Partner eines
/// ausgewählten Audio-Clips; ersatzweise den ersten ausgewählten sichtbaren
/// Clip (Untertitel/Titel). Nie ein Audio-Clip — der trägt keine
/// Farbkorrektur. `None`, wenn nur Audio ohne Video-Partner ausgewählt ist.
fn selected_grade(state: &crate::state::AppState) -> Option<crate::core::grade::ColorGrade> {
    for id in &state.timeline.selected_clip_ids {
        let Some(clip) = state.timeline.clip(id) else {
            continue;
        };
        if clip.kind == TrackKind::Video {
            return Some(clip.grade.clone());
        }
        if let Some(link) = &clip.link_id {
            if let Some(video) = state.timeline.clips.iter().find(|c| {
                c.kind == TrackKind::Video && c.link_id.as_deref() == Some(link.as_str())
            }) {
                return Some(video.grade.clone());
            }
        }
    }
    state
        .timeline
        .selected_clip_ids
        .iter()
        .filter_map(|id| state.timeline.clip(id))
        .find(|c| c.kind != TrackKind::Audio)
        .map(|c| c.grade.clone())
}

// -------------------------------------------------------------- Multicam

/// Eine Multicam-Quelle aus den aktuell ausgewählten Video-Assets erzeugen
/// (synchron — bewusste Nutzeraktion). Synchronisiert je nach Verfahren über
/// gemeinsamen Start, Medien-Timecode oder Audio-Kreuzkorrelation und legt die
/// Quelle als Hintergrund-Sequenz an (erscheint im Browser). Liefert die ID.
fn create_multicam_source(
    ctx: &mut CommandCtx,
    sync: crate::core::multicam::MulticamSync,
) -> Result<String, String> {
    use crate::core::multicam as mc;
    use crate::core::types::MediaKind;
    let ids = ctx.state.media.selected_asset_ids.clone();
    let assets: Vec<crate::core::types::MediaAsset> = ids
        .iter()
        .filter_map(|id| ctx.state.media.asset(id).cloned())
        .filter(|a| a.kind == MediaKind::Video && !a.info.video.is_empty() && !a.offline)
        .collect();
    if assets.len() < 2 {
        return Err("Mindestens zwei Video-Clips auswählen".into());
    }
    let n = assets.len();
    let positions: Vec<f64> = match sync {
        mc::MulticamSync::Start => mc::positions_from_start(n),
        mc::MulticamSync::Timecode => {
            let tcs: Vec<Option<f64>> = assets
                .iter()
                .map(|a| {
                    let fps = a.info.video.first().map(|v| v.fps).unwrap_or(25.0);
                    crate::services::probe_start_timecode(&a.path)
                        .and_then(|tc| mc::timecode_to_seconds(&tc, fps))
                })
                .collect();
            if tcs.iter().all(|t| t.is_none()) {
                return Err("Kein Medien-Timecode vorhanden — anderes Verfahren wählen".into());
            }
            mc::positions_from_timecodes(&tcs)
        }
        mc::MulticamSync::Audio => {
            let envs: Vec<Option<Vec<f32>>> = assets
                .iter()
                .map(|a| mc::extract_sync_envelope(&a.path))
                .collect();
            if envs.iter().filter(|e| e.is_some()).count() < 2 {
                return Err("Zu wenig Audio für die Analyse — anderes Verfahren wählen".into());
            }
            mc::positions_from_audio(&envs, mc::SYNC_RATE)
        }
    };
    let refs: Vec<&crate::core::types::MediaAsset> = assets.iter().collect();
    let source = mc::build_source(&refs, &positions, None, sync);
    let inner = mc::build_inner_timeline(&source);
    let bin_id = assets[0].bin_id.clone();
    let name = format!("Multicam – {}", assets[0].name);
    let mut seq = crate::core::sequences::Sequence::new(name, bin_id, inner);
    seq.timeline.multicam = Some(source);
    Ok(ctx.state.timeline.add_background(seq))
}

/// Handler der Zifferntasten-Multicam-Befehle (`multicam.angle{N}`): bei
/// Wiedergabe Live-Schnitt am Playhead + Winkelwechsel, sonst nur Winkelwechsel
/// des ausgewählten/aktiven Multicam-Clips.
fn multicam_angle(ctx: &mut CommandCtx, args: Option<&Value>) {
    let Some(angle) = args
        .and_then(|v| v.get("angle"))
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
    else {
        return;
    };
    let playing = ctx.state.playback.program_playing;
    let t = ctx.state.timeline.playhead_sec;
    let clip_id: Option<String> = if playing {
        ctx.state
            .timeline
            .topmost_multicam_video_at(t)
            .map(|c| c.id.clone())
    } else {
        let sel = ctx.state.timeline.selected_clip_ids.clone();
        sel.iter()
            .find(|id| ctx.state.timeline.clip(id).is_some_and(|c| c.is_multicam()))
            .cloned()
            .or_else(|| {
                ctx.state
                    .timeline
                    .topmost_multicam_video_at(t)
                    .map(|c| c.id.clone())
            })
    };
    let Some(clip_id) = clip_id else { return };
    // Winkel muss in der Quelle existieren.
    let source_id = ctx
        .state
        .timeline
        .clip(&clip_id)
        .and_then(|c| c.multicam.as_ref())
        .map(|mc| mc.source.clone());
    let n = source_id
        .as_ref()
        .and_then(|s| ctx.state.timeline.multicam_source(s))
        .map(|s| s.angle_count())
        .unwrap_or(0);
    if angle as usize >= n {
        return;
    }
    if playing {
        ctx.state.timeline.multicam_live_cut(t, angle);
    } else {
        ctx.state.timeline.set_multicam_angle_undoable(&clip_id, angle);
    }
}

// -------------------------------------------------------------- Marker

/// Asset-ID, wenn die Marker-Aktionen auf den Quellmonitor wirken sollen
/// (fokussierter Quellmonitor mit geladenem Asset); sonst None ⇒ Sequenz.
fn source_marker_target(state: &AppState) -> Option<String> {
    if state.app.focused_panel != "source" {
        return None;
    }
    state
        .playback
        .source_asset_id
        .as_ref()
        .filter(|id| state.media.asset(id).is_some())
        .cloned()
}

/// Marker-Bearbeiten-Dialog auf ein Ziel öffnen.
fn open_marker_dialog(state: &mut AppState, scope: MarkerScope, marker_id: String) {
    state.app.marker_editor = Some(MarkerEditTarget { scope, marker_id });
    state.app.open_dialog = Some(DialogId::Marker);
}

/// Quellmonitor-Position auf den nächsten/vorherigen Asset-Marker setzen.
fn source_marker_step(state: &mut AppState, asset_id: &str, dir: i32) {
    let pos = state.playback.source.position;
    let Some(asset) = state.media.asset(asset_id) else { return };
    let target = if dir >= 0 {
        asset
            .markers
            .iter()
            .map(|m| m.time)
            .filter(|t| *t > pos + 1e-4)
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    } else {
        asset
            .markers
            .iter()
            .map(|m| m.time)
            .filter(|t| *t < pos - 1e-4)
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    };
    if let Some(t) = target {
        state.playback.source.position = t.max(0.0);
        state.playback.source.playing = false;
    }
}

/// Alle Asset-Marker eines Assets entfernen.
fn clear_asset_markers(state: &mut AppState, asset_id: &str) {
    if let Some(asset) = state.media.assets.iter_mut().find(|a| a.id == asset_id) {
        if !asset.markers.is_empty() {
            asset.markers.clear();
            state.media.revision += 1;
        }
    }
}

/// Projekt öffnen + Statusmeldung; defekte Recent-Einträge aufräumen.
fn open_project_with_status(ctx: &mut CommandCtx, path: &std::path::Path) {
    match crate::core::project::open_into(ctx.state, path) {
        Ok(0) => {
            let msg = format!("Projekt geöffnet: {}", ctx.state.project.display_name());
            status(ctx, &msg);
        }
        Ok(offline) => {
            let msg = format!("Projekt geöffnet — {offline} Medien fehlen");
            status(ctx, &msg);
        }
        Err(err) => {
            if !path.exists() {
                let entry = path.to_string_lossy().into_owned();
                ctx.state.project.remove_recent(&entry);
            }
            let msg = format!("Öffnen fehlgeschlagen: {err}");
            status(ctx, &msg);
        }
    }
}

/// Übergang auf die Kanten der Auswahl anwenden + Statusmeldung.
fn apply_transition_kind(ctx: &mut CommandCtx, kind: crate::core::transitions::TransitionKind) {
    let n = ctx.state.timeline.apply_transition_to_selection(kind);
    let msg = if n == 0 {
        format!(
            "„{}“ nicht anwendbar — Kanten belegt, falsche Spurart oder zu wenig Material (Handles)",
            kind.label()
        )
    } else {
        format!("„{}“ auf {} Schnittkante(n) angewendet", kind.label(), n)
    };
    status(ctx, &msg);
}

/// Playhead zum nächsten/vorherigen Keyframe (alle Parameter der Auswahl).
fn jump_to_keyframe(ctx: &mut CommandCtx, dir: i32) {
    use crate::core::animation::ParamId;
    let playhead = ctx.state.timeline.playhead_sec;
    let mut best: Option<f64> = None;
    for clip in ctx
        .state
        .timeline
        .clips
        .iter()
        .filter(|c| ctx.state.timeline.selected_clip_ids.contains(&c.id))
    {
        for param in ParamId::ALL {
            for k in &clip.fx.param(param).keyframes {
                // Keyframe-Medienzeit → Sequenzzeit (speed-bewusst).
                let t_seq = clip.seq_time_of_media(k.t);
                let candidate = if dir > 0 {
                    t_seq > playhead + 1e-4
                } else {
                    t_seq < playhead - 1e-4
                };
                if !candidate {
                    continue;
                }
                best = Some(match best {
                    None => t_seq,
                    Some(b) if dir > 0 => b.min(t_seq),
                    Some(b) => b.max(t_seq),
                });
            }
        }
    }
    if let Some(t) = best {
        ctx.state.timeline.set_playhead(t.max(0.0));
    }
}

pub fn build_registry() -> CommandRegistry {
    let mut commands: Vec<Command> = Vec::new();

    fn cmd(
        id: &str,
        title: &str,
        category: &'static str,
        run: RunFn,
    ) -> Command {
        Command {
            id: id.to_string(),
            title: title.to_string(),
            category,
            when: None,
            allow_repeat: false,
            bound_arg: None,
            run,
        }
    }

    fn with_when(mut c: Command, when: &'static str) -> Command {
        c.when = Some(when);
        c
    }

    fn with_repeat(mut c: Command) -> Command {
        c.allow_repeat = true;
        c
    }

    fn with_arg(mut c: Command, arg: Value) -> Command {
        c.bound_arg = Some(arg);
        c
    }

    // ----------------------------------------------------------- Anwendung
    commands.push(cmd(
        "app.commandPalette",
        "Befehlspalette öffnen",
        "Anwendung",
        |ctx, _| ctx.state.app.command_palette_open = true,
    ));
    commands.push(cmd(
        "app.shortcutEditor",
        "Tastaturbefehle…",
        "Anwendung",
        |ctx, _| ctx.state.app.open_dialog = Some(DialogId::Shortcuts),
    ));
    commands.push(cmd("app.export", "Exportieren…", "Anwendung", |ctx, _| {
        ctx.state.app.open_dialog = Some(DialogId::Export)
    }));
    commands.push(cmd(
        "queue.open",
        "Render-Warteschlange…",
        "Anwendung",
        |ctx, _| {
            ctx.state.app.open_dialog = Some(DialogId::Export);
            ctx.state.app.export_open_queue = true;
        },
    ));
    commands.push(cmd(
        "export.frame",
        "Frame exportieren…",
        "Anwendung",
        |ctx, _| {
            // Pfad-Dialog; der eigentliche Render läuft nach Auswahl im
            // Event-Handler (1-Frame-Plan am Playhead → Bilddatei).
            let name = format!("{}_frame", ctx.state.project.display_name());
            ctx.services.pick_frame_export_target(&name);
        },
    ));
    commands.push(cmd(
        "sequence.settings",
        "Sequenzeinstellungen…",
        "Sequenz",
        |ctx, _| ctx.state.app.open_dialog = Some(DialogId::SequenceSettings),
    ));
    commands.push(cmd(
        "sequence.new",
        "Neue Sequenz",
        "Sequenz",
        |ctx, _| {
            // Neue Sequenz erbt die Einstellungen der aktiven (Projektformat)
            // und landet im aktuell geöffneten Medien-Bin.
            let settings = ctx.state.timeline.settings;
            let bin = ctx.state.media.current_bin().to_string();
            let id = ctx.state.timeline.add(None, settings, &bin);
            let name = ctx.state.timeline.name_of(&id).unwrap_or("Sequenz").to_string();
            ctx.state.dock.open_panel("timeline");
            status(ctx, &format!("Neue Sequenz „{name}“"));
        },
    ));
    commands.push(cmd(
        "sequence.duplicate",
        "Sequenz duplizieren",
        "Sequenz",
        |ctx, args| {
            let target = arg_str(args, "sequenceId")
                .unwrap_or_else(|| ctx.state.timeline.active_id().to_string());
            if let Some(id) = ctx.state.timeline.duplicate(&target) {
                let name = ctx.state.timeline.name_of(&id).unwrap_or("Sequenz").to_string();
                ctx.state.dock.open_panel("timeline");
                status(ctx, &format!("Sequenz dupliziert: „{name}“"));
            }
        },
    ));
    commands.push(cmd(
        "sequence.rename",
        "Sequenz umbenennen",
        "Sequenz",
        |ctx, args| {
            let target = arg_str(args, "sequenceId")
                .unwrap_or_else(|| ctx.state.timeline.active_id().to_string());
            // Tab/Browser-Eintrag in den Inline-Umbenennen-Modus versetzen.
            ctx.state.timeline.set_active(&target);
            ctx.state.app.rename_sequence = Some(target);
            ctx.state.dock.open_panel("timeline");
        },
    ));
    commands.push(cmd(
        "sequence.delete",
        "Sequenz löschen",
        "Sequenz",
        |ctx, args| {
            let target = arg_str(args, "sequenceId")
                .unwrap_or_else(|| ctx.state.timeline.active_id().to_string());
            if ctx.state.timeline.len() <= 1 {
                status(ctx, "Die letzte Sequenz kann nicht gelöscht werden");
                return;
            }
            // Wird die Sequenz als Nest ODER als Multicam-Quelle verwendet, erst
            // bestätigen lassen (beim Löschen werden Multicam-Clips auf ihren
            // aktiven Winkel flachgeklopft, Nest-Clips entfernt).
            if ctx.state.timeline.nest_usage_count(&target) > 0
                || ctx.state.timeline.multicam_usage_count(&target) > 0
            {
                ctx.state.app.sequence_delete_target = Some(target);
                ctx.state.app.open_dialog = Some(DialogId::ConfirmDeleteSequence);
                return;
            }
            let name = ctx.state.timeline.name_of(&target).unwrap_or("Sequenz").to_string();
            if ctx.state.timeline.remove(&target) {
                status(ctx, &format!("Sequenz gelöscht: „{name}“"));
            }
        },
    ));
    commands.push(cmd(
        "sequence.open",
        "Sequenz öffnen",
        "Sequenz",
        |ctx, args| {
            let Some(id) = arg_str(args, "sequenceId") else { return };
            if ctx.state.timeline.set_active(&id) || ctx.state.timeline.is_tab_open(&id) {
                ctx.state.dock.open_panel("timeline");
            }
        },
    ));
    commands.push(with_when(
        cmd(
            "sequence.openNested",
            "Verschachtelte Sequenz öffnen",
            "Sequenz",
            |ctx, _| {
                // Innere Sequenz des (einzeln) ausgewählten Nest-Clips öffnen.
                let nested = ctx
                    .state
                    .timeline
                    .selected_clip_ids
                    .first()
                    .and_then(|id| ctx.state.timeline.clip(id))
                    .and_then(|c| c.nest_seq.clone());
                if let Some(id) = nested {
                    ctx.state.timeline.set_active(&id);
                    ctx.state.dock.open_panel("timeline");
                } else {
                    status(ctx, "Kein verschachtelter Clip ausgewählt");
                }
            },
        ),
        "nestClipSelected",
    ));
    // ------------------------------------------ Interop (Austauschformate)
    // Schnitte mit DaVinci Resolve & Co. austauschen: Export der aktiven
    // Sequenz, Import als neue Sequenz (FCPXML vorerst nur Export).
    {
        use crate::core::interop::InteropFormat;
        for format in InteropFormat::ALL {
            let key = format.key();
            commands.push(with_when(
                with_arg(
                    cmd(
                        &format!("sequence.export.{key}"),
                        &format!("Exportieren: {}", format.label()),
                        "Interop",
                        |ctx, args| {
                            let Some(format) = arg_str(args, "format")
                                .and_then(|k| InteropFormat::from_key(&k))
                            else {
                                return;
                            };
                            let name = ctx.state.timeline.active_name();
                            let default = format!("{name}.{}", format.extension());
                            ctx.services.pick_interop_export_target(format, &default);
                        },
                    ),
                    serde_json::json!({ "format": key }),
                ),
                "timelineHasClips",
            ));
            if format.can_import() {
                commands.push(with_arg(
                    cmd(
                        &format!("sequence.import.{key}"),
                        &format!("Importieren: {}", format.label()),
                        "Interop",
                        |ctx, args| {
                            let Some(format) = arg_str(args, "format")
                                .and_then(|k| InteropFormat::from_key(&k))
                            else {
                                return;
                            };
                            ctx.services.pick_interop_import(format);
                        },
                    ),
                    serde_json::json!({ "format": key }),
                ));
            }
        }
    }

    commands.push(cmd(
        "app.settings",
        "Einstellungen…",
        "Anwendung",
        |ctx, _| ctx.state.app.open_dialog = Some(DialogId::Settings),
    ));

    // ------------------------------------------------------------- Projekt
    commands.push(cmd(
        "project.new",
        "Neues Projekt",
        "Projekt",
        |ctx, _| {
            if let Some(msg) = crate::core::project::safeguard_unsaved(ctx.state) {
                status(ctx, &msg);
            }
            crate::core::project::reset_to_new(ctx.state);
        },
    ));
    commands.push(cmd(
        "project.open",
        "Projekt öffnen…",
        "Projekt",
        |ctx, _| ctx.services.pick_project_open(),
    ));
    commands.push(cmd(
        "project.save",
        "Projekt speichern",
        "Projekt",
        |ctx, _| match ctx.state.project.path.clone() {
            Some(path) => {
                let msg = match crate::core::project::save_to(ctx.state, &path) {
                    Ok(()) => format!("Projekt gespeichert: {}", path.display()),
                    Err(err) => format!("Speichern fehlgeschlagen: {err}"),
                };
                status(ctx, &msg);
            }
            None => {
                let name = format!("{}.{}", ctx.state.project.display_name(), crate::core::project::PROJECT_EXT);
                ctx.services.pick_project_save_target(&name);
            }
        },
    ));
    commands.push(cmd(
        "project.saveAs",
        "Projekt speichern unter…",
        "Projekt",
        |ctx, _| {
            let name = format!("{}.{}", ctx.state.project.display_name(), crate::core::project::PROJECT_EXT);
            ctx.services.pick_project_save_target(&name);
        },
    ));
    commands.push(cmd(
        "project.openRecent",
        "Zuletzt verwendetes Projekt öffnen",
        "Projekt",
        |ctx, args| {
            let path = match args.and_then(|v| v.get("path")).and_then(|v| v.as_str()) {
                Some(p) => Some(p.to_string()),
                None => ctx.state.project.recent.first().cloned(),
            };
            let Some(path) = path else {
                status(ctx, "Keine zuletzt verwendeten Projekte");
                return;
            };
            if let Some(msg) = crate::core::project::safeguard_unsaved(ctx.state) {
                status(ctx, &msg);
            }
            open_project_with_status(ctx, std::path::Path::new(&path));
        },
    ));
    commands.push(cmd(
        "project.restoreAutosave",
        "Letzte Sitzung wiederherstellen",
        "Projekt",
        |ctx, _| {
            let path = crate::core::project::autosave_path();
            if !path.exists() {
                status(ctx, "Kein Sitzungs-Autosave vorhanden");
                return;
            }
            if let Some(msg) = crate::core::project::safeguard_unsaved(ctx.state) {
                status(ctx, &msg);
            }
            match crate::core::project::open_into(ctx.state, &path) {
                Ok(_) => {
                    // Autosave ist kein echtes Projekt: als „Unbenannt“ weiterführen.
                    ctx.state.project.path = None;
                    let entry = path.to_string_lossy().into_owned();
                    ctx.state.project.remove_recent(&entry);
                    status(ctx, "Letzte Sitzung wiederhergestellt");
                }
                Err(err) => {
                    let msg = format!("Wiederherstellen fehlgeschlagen: {err}");
                    status(ctx, &msg);
                }
            }
        },
    ));
    commands.push(cmd(
        "project.autosaveVersions",
        "Autosave-Versionen…",
        "Projekt",
        |ctx, _| {
            // Beim manuellen Öffnen keinen Absturz-Hinweis zeigen.
            ctx.state.app.autosave_recover_hint = None;
            ctx.state.app.open_dialog = Some(DialogId::AutosaveVersions);
        },
    ));
    commands.push(cmd(
        "project.relink",
        "Fehlende Medien neu verknüpfen…",
        "Projekt",
        |ctx, _| ctx.state.app.open_dialog = Some(DialogId::Relink),
    ));

    // ----------------------------------------------------------- Bearbeiten
    // Rückgängig/Wiederholen koordinieren Timeline- und Medien-History: beide
    // führen eine eigene Snapshot-Liste, jeder Snapshot trägt eine globale
    // op-Sequenz. Undo macht die jüngste Operation (höchste Sequenz) rückgängig;
    // Redo stellt die älteste vorgemerkte (kleinste Sequenz) wieder her.
    commands.push(with_repeat(with_when(
        cmd("edit.undo", "Rückgängig", "Bearbeiten", |ctx, _| {
            let t = ctx.state.timeline.undo_seq();
            let m = ctx.state.media.undo_seq();
            match (t, m) {
                (Some(ts), Some(ms)) if ms > ts => ctx.state.media.undo(),
                (Some(_), _) => ctx.state.timeline.undo(),
                (None, Some(_)) => ctx.state.media.undo(),
                (None, None) => {}
            }
        }),
        "canUndo",
    )));
    commands.push(with_repeat(with_when(
        cmd("edit.redo", "Wiederholen", "Bearbeiten", |ctx, _| {
            let t = ctx.state.timeline.redo_seq();
            let m = ctx.state.media.redo_seq();
            match (t, m) {
                (Some(ts), Some(ms)) if ms < ts => ctx.state.media.redo(),
                (Some(_), _) => ctx.state.timeline.redo(),
                (None, Some(_)) => ctx.state.media.redo(),
                (None, None) => {}
            }
        }),
        "canRedo",
    )));

    // -------------------------------------------------------------- Medien
    commands.push(cmd(
        "media.import",
        "Medien importieren…",
        "Medien",
        |ctx, _| {
            ctx.state.media.importing = true;
            ctx.services.open_import_dialog();
        },
    ));
    commands.push(cmd(
        "media.importFolder",
        "Ordner importieren…",
        "Medien",
        |ctx, _| {
            ctx.state.media.importing = true;
            ctx.services.open_import_folder_dialog();
        },
    ));
    commands.push(with_when(
        cmd(
            "media.removeSelected",
            "Ausgewählte Medien entfernen",
            "Medien",
            |ctx, _| {
                let ids = ctx.state.media.selected_asset_ids.clone();
                if ids.is_empty() {
                    return;
                }
                // Wird ein ausgewähltes Asset in der Timeline verwendet, erst
                // bestätigen lassen (Clips würden mit entfernt).
                let used = ids
                    .iter()
                    .any(|id| ctx.state.timeline.asset_usage_count(id) > 0);
                if used {
                    ctx.state.app.open_dialog = Some(DialogId::ConfirmRemoveMedia);
                    return;
                }
                remove_selected_media(ctx);
            },
        ),
        "mediaSelected",
    ));
    commands.push(with_when(
        cmd(
            "media.removeSelectedConfirmed",
            "Ausgewählte Medien entfernen (bestätigt)",
            "Medien",
            |ctx, _| {
                ctx.state.app.open_dialog = None;
                remove_selected_media(ctx);
            },
        ),
        "mediaSelected",
    ));
    commands.push(with_when(
        cmd(
            "media.addSelectionToTimeline",
            "Auswahl an Playhead in Timeline einfügen",
            "Medien",
            |ctx, _| {
                let ids = ctx.state.media.selected_asset_ids.clone();
                if ids.is_empty() {
                    return;
                }
                let at = ctx.state.timeline.playhead_sec;
                let assets = ctx.state.media.assets.clone();
                ctx.state.timeline.insert_assets(&assets, &ids, at, None);
                // Playhead ans Ende des Eingefügten (Overwrite-Konvention).
                let end = ctx
                    .state
                    .timeline
                    .clips
                    .iter()
                    .filter(|c| ctx.state.timeline.selected_clip_ids.contains(&c.id))
                    .map(|c| c.end())
                    .fold(f64::NEG_INFINITY, f64::max);
                if end.is_finite() {
                    ctx.state.timeline.set_playhead(end);
                }
                ctx.state.dock.open_panel("timeline");
            },
        ),
        "mediaSelected",
    ));
    commands.push(with_when(
        cmd(
            "media.openInSource",
            "In Quellmonitor laden",
            "Medien",
            |ctx, args| {
                let Some(asset_id) = arg_asset_id(ctx, args) else { return };
                ctx.state.playback.source_asset_id = Some(asset_id);
                ctx.state.playback.source = Default::default();
                ctx.state.playback.source.rate = 1.0;
                ctx.state.dock.open_panel("source");
            },
        ),
        "mediaSelected",
    ));
    commands.push(with_when(
        cmd(
            "media.revealInFileManager",
            "Im Dateimanager anzeigen",
            "Medien",
            |ctx, args| {
                let Some(asset_id) = arg_asset_id(ctx, args) else { return };
                let Some(asset) = ctx.state.media.asset(&asset_id) else { return };
                if ctx.services.reveal_in_file_manager(&asset.path).is_err() {
                    status(ctx, "Dateimanager konnte nicht geöffnet werden");
                }
            },
        ),
        "mediaSelected",
    ));
    commands.push(with_when(
        cmd(
            "media.relinkAsset",
            "Medium neu verknüpfen…",
            "Medien",
            |ctx, args| {
                let Some(asset_id) = arg_asset_id(ctx, args) else { return };
                ctx.services.pick_relink_file(&asset_id);
            },
        ),
        "mediaSelected",
    ));

    // ----------------------------------------------------------- Multicam
    // Multicam-Quelle aus der Mehrfachauswahl erstellen (Verfahren via `method`-
    // Argument; ohne Argument: Audio-Analyse — der Premiere-Killer-Workflow).
    commands.push(with_when(
        cmd(
            "media.createMulticamSource",
            "Multicam-Quelle erstellen…",
            "Multicam",
            |ctx, args| {
                let sync = arg_str(args, "method")
                    .and_then(|k| crate::core::multicam::MulticamSync::from_key(&k))
                    .unwrap_or(crate::core::multicam::MulticamSync::Audio);
                match create_multicam_source(ctx, sync) {
                    Ok(_) => status(
                        ctx,
                        &format!("Multicam-Quelle erstellt (Sync: {})", sync.label()),
                    ),
                    Err(e) => status(ctx, &e),
                }
            },
        ),
        "mediaMultiSelected",
    ));
    // Multicam-Monitor (Programmmonitor-Raster) umschalten.
    commands.push(cmd(
        "multicam.toggleMonitor",
        "Multicam-Monitor umschalten",
        "Multicam",
        |ctx, _| {
            use crate::stores::MonitorView;
            ctx.state.monitor.view = match ctx.state.monitor.view {
                MonitorView::Program => MonitorView::Multicam,
                MonitorView::Multicam => MonitorView::Program,
            };
            let on = ctx.state.monitor.view == MonitorView::Multicam;
            status(
                ctx,
                if on {
                    "Multicam-Monitor an"
                } else {
                    "Multicam-Monitor aus"
                },
            );
        },
    ));
    // Zifferntasten 1–9: Live-Schnitt/Winkelwechsel (nur im Multicam-Monitor).
    for n in 1..=9u32 {
        commands.push(with_when(
            with_repeat(with_arg(
                cmd(
                    &format!("multicam.angle{n}"),
                    &format!("Multicam: Winkel {n}"),
                    "Multicam",
                    multicam_angle,
                ),
                serde_json::json!({ "angle": n - 1 }),
            )),
            "multicamActive",
        ));
    }
    // Multicam-Clips „auf einzelne Clips reduzieren" (Flatten): ausgewählte,
    // sonst alle der aktiven Sequenz.
    commands.push(with_when(
        cmd(
            "multicam.flatten",
            "Multicam auf einzelne Clips reduzieren",
            "Multicam",
            |ctx, _| {
                let sel = ctx.state.timeline.selected_clip_ids.clone();
                let use_sel = sel
                    .iter()
                    .any(|id| ctx.state.timeline.clip(id).is_some_and(|c| c.is_multicam()));
                let count = if use_sel {
                    ctx.state.timeline.flatten_multicam(Some(&sel))
                } else {
                    ctx.state.timeline.flatten_multicam(None)
                };
                if count > 0 {
                    status(ctx, &format!("{count} Multicam-Clip(s) reduziert"));
                } else {
                    status(ctx, "Keine Multicam-Clips zum Reduzieren");
                }
            },
        ),
        "timelineHasClips",
    ));

    // ----------------------------------------------------------- Proxys
    // Proxys sind leichtgewichtige Transcodes (ProRes Proxy / DNxHR LB) für
    // flüssiges Schneiden von 4K/8K. Der EXPORT nutzt IMMER die Originale.
    commands.push(with_when(
        cmd(
            "media.createProxies",
            "Proxies erstellen",
            "Medien",
            |ctx, args| {
                let ids = target_asset_ids(ctx, args);
                dispatch_proxy_tasks(ctx, &ids);
            },
        ),
        "mediaSelected",
    ));
    commands.push(cmd(
        "media.createProxiesAll",
        "Proxies für alle Medien erstellen",
        "Medien",
        |ctx, _| {
            let ids: Vec<String> = ctx
                .state
                .media
                .assets
                .iter()
                .filter(|a| a.kind == crate::core::types::MediaKind::Video)
                .map(|a| a.id.clone())
                .collect();
            dispatch_proxy_tasks(ctx, &ids);
        },
    ));
    commands.push(with_when(
        cmd(
            "media.cancelProxy",
            "Proxy-Erstellung abbrechen",
            "Medien",
            |ctx, args| {
                let ids = target_asset_ids(ctx, args);
                for id in &ids {
                    ctx.services.cancel_proxy(id);
                    ctx.state.media.clear_proxy_job(id);
                }
            },
        ),
        "mediaSelected",
    ));
    commands.push(with_when(
        cmd(
            "media.deleteProxies",
            "Proxies entfernen",
            "Medien",
            |ctx, args| {
                let ids = target_asset_ids(ctx, args);
                let mut removed = 0usize;
                for id in &ids {
                    ctx.services.cancel_proxy(id);
                    if let Some(path) = ctx.state.media.detach_proxy(id) {
                        let _ = std::fs::remove_file(&path);
                        removed += 1;
                    }
                }
                if removed > 0 {
                    status(ctx, &format!("{removed} Proxy/Proxys entfernt"));
                }
            },
        ),
        "mediaSelected",
    ));
    commands.push(cmd(
        "proxy.toggle",
        "Proxies verwenden umschalten",
        "Medien",
        |ctx, _| {
            let on = !ctx.state.media.use_proxies;
            ctx.state.media.set_use_proxies(on);
            status(
                ctx,
                if on {
                    "Proxies verwenden: an (Vorschau aus Proxy, Export bleibt Original)"
                } else {
                    "Proxies verwenden: aus"
                },
            );
        },
    ));
    commands.push(cmd(
        "proxy.settings",
        "Proxy-Einstellungen…",
        "Medien",
        |ctx, _| ctx.state.app.open_dialog = Some(DialogId::ProxySettings),
    ));

    // ---- Effekt-Masken (geometrische Begrenzung eines Effekts) ----
    fn add_mask(ctx: &mut CommandCtx, shape: crate::core::mask::MaskShape) {
        let Some((clip_id, fx_id)) = mask_target_effect(ctx.state) else {
            status(
                ctx,
                "Keine Maske möglich — erst einen Clip mit Video-Effekt auswählen",
            );
            return;
        };
        // Maskenlimit je Effekt (GPU-Grenze) erreicht?
        let at_cap = ctx
            .state
            .timeline
            .clip(&clip_id)
            .and_then(|c| c.effects.iter().find(|e| e.id == fx_id))
            .is_some_and(|e| e.masks.len() >= crate::core::mask::MAX_MASKS);
        if at_cap {
            status(ctx, "Maximal 8 Masken pro Effekt erreicht");
            return;
        }
        if let Some(mask_id) = ctx.state.timeline.mask_add(&clip_id, &fx_id, shape) {
            ctx.state.app.active_mask = Some(crate::stores::MaskSelection {
                clip_id,
                fx_id,
                mask_id,
            });
            status(ctx, &format!("{}-Maske hinzugefügt — im Monitor ziehen", shape.label()));
        }
    }
    commands.push(with_when(
        cmd("mask.addEllipse", "Maske: Ellipse", "Effekte", |ctx, _| {
            add_mask(ctx, crate::core::mask::MaskShape::Ellipse)
        }),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd("mask.addRectangle", "Maske: Rechteck", "Effekte", |ctx, _| {
            add_mask(ctx, crate::core::mask::MaskShape::Rectangle)
        }),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd("mask.addPolygon", "Maske: Polygon", "Effekte", |ctx, _| {
            add_mask(ctx, crate::core::mask::MaskShape::Polygon)
        }),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd(
            "mask.toggleInvert",
            "Maske invertieren",
            "Effekte",
            |ctx, _| {
                if let Some(sel) = ctx.state.app.active_mask.clone() {
                    ctx.state
                        .timeline
                        .mask_toggle_invert(&sel.clip_id, &sel.fx_id, &sel.mask_id);
                    status(ctx, "Maske invertiert");
                }
            },
        ),
        "maskEditing",
    ));
    commands.push(with_when(
        cmd("mask.delete", "Maske löschen", "Effekte", |ctx, _| {
            if let Some(sel) = ctx.state.app.active_mask.take() {
                ctx.state
                    .timeline
                    .mask_remove(&sel.clip_id, &sel.fx_id, &sel.mask_id);
                status(ctx, "Maske gelöscht");
            }
        }),
        "maskEditing",
    ));
    commands.push(with_when(
        cmd(
            "mask.finishEdit",
            "Maskenbearbeitung beenden",
            "Effekte",
            |ctx, _| ctx.state.app.active_mask = None,
        ),
        "maskEditing",
    ));

    // ---- Wiedergabe-Performance: Render-Cache, Hardware-Decode, Overlay ----
    commands.push(with_when(
        cmd(
            "render.inToOut",
            "Render In to Out (Sequenz-Render-Cache)",
            "Wiedergabe",
            |ctx, _| {
                let rate = ctx.state.timeline.settings.rate;
                let end_sec = sequence_end(&ctx.state.timeline.clips);
                let (a, b) = match (ctx.state.timeline.in_point, ctx.state.timeline.out_point) {
                    (Some(i), Some(o)) if o - i > 1e-9 => (i.max(0.0), o.max(0.0)),
                    _ => (0.0, end_sec),
                };
                let start_frame = rate.frame_round(a).max(0);
                let end_frame = rate.frame_round(b);
                if end_frame <= start_frame {
                    status(ctx, "Render-Cache: leerer Bereich");
                    return;
                }
                // Laufenden Render zuerst abbrechen (nur ein aktiver Job).
                if let Some(r) = ctx.state.render_cache.rendering.take() {
                    ctx.services.cancel_job(&format!("rendercache-{}", r.job_id));
                }
                let (w, h, fps) = (
                    ctx.state.timeline.settings.width,
                    ctx.state.timeline.settings.height,
                    rate.fps(),
                );
                let plan = crate::core::export::build_cache_plan(
                    &ctx.state.timeline,
                    &ctx.state.media,
                    w,
                    h,
                    fps,
                    start_frame as u64,
                    end_frame as u64,
                );
                if !plan.has_video_media() {
                    status(ctx, "Render-Cache: kein Material im Bereich");
                    return;
                }
                let hash = RenderCacheStore::range_signature(
                    &ctx.state.timeline,
                    &ctx.state.media,
                    start_frame,
                    end_frame,
                );
                let codec = ctx.state.settings.render_cache_codec;
                let dir = render_cache_dir(&ctx.state.settings);
                if std::fs::create_dir_all(&dir).is_err() {
                    status(ctx, "Render-Cache: Ordner nicht beschreibbar");
                    return;
                }
                let file = dir.join(format!("seq-{start_frame}-{end_frame}-{hash:016x}.{}", codec.ext()));
                let encode_args: Vec<String> =
                    codec.encode_args().iter().map(|s| s.to_string()).collect();
                let job = ctx.services.start_render_cache(
                    plan,
                    encode_args,
                    codec.ext(),
                    file,
                    start_frame,
                    end_frame,
                    hash,
                );
                ctx.state.render_cache.rendering = Some(RenderProgress {
                    start_frame,
                    end_frame,
                    pct: 0.0,
                    job_id: job.rsplit('-').next().and_then(|n| n.parse().ok()).unwrap_or(0),
                });
                status(ctx, "Render-Cache wird erstellt …");
            },
        ),
        "hasClips",
    ));
    commands.push(cmd(
        "render.clearCache",
        "Render-Cache leeren",
        "Wiedergabe",
        |ctx, _| {
            // Laufenden Render-Cache-Job abbrechen.
            if let Some(r) = ctx.state.render_cache.rendering.take() {
                ctx.services.cancel_job(&format!("rendercache-{}", r.job_id));
            }
            for f in ctx.state.render_cache.clear() {
                let _ = std::fs::remove_file(f);
            }
            status(ctx, "Render-Cache geleert");
        },
    ));
    commands.push(cmd(
        "hwaccel.toggle",
        "Hardware-Decode umschalten",
        "Wiedergabe",
        |ctx, _| {
            let on = !ctx.state.settings.hwaccel;
            ctx.state.settings.hwaccel = on;
            ctx.state.settings.save();
            status(
                ctx,
                if on {
                    "Hardware-Decode: an (mit Software-Fallback)"
                } else {
                    "Hardware-Decode: aus (reiner Software-Decode)"
                },
            );
        },
    ));
    commands.push(cmd(
        "monitor.togglePerfOverlay",
        "Performance-Overlay umschalten",
        "Wiedergabe",
        |ctx, _| {
            let on = !ctx.state.monitor.show_perf_overlay;
            ctx.state.monitor.show_perf_overlay = on;
            status(
                ctx,
                if on {
                    "Performance-Overlay: an"
                } else {
                    "Performance-Overlay: aus"
                },
            );
        },
    ));
    commands.push(cmd(
        "ui.scaleUp",
        "UI vergrößern (HiDPI)",
        "Ansicht",
        |ctx, _| {
            let next = (ctx.state.app.ui_scale + 0.25)
                .clamp(crate::core::settings::UI_SCALE_MIN, crate::core::settings::UI_SCALE_MAX);
            ctx.state.settings.ui_scale = Some(next);
            ctx.state.settings.save();
            status(ctx, &format!("UI-Skalierung: {:.0}%", next * 100.0));
        },
    ));
    commands.push(cmd(
        "ui.scaleDown",
        "UI verkleinern (HiDPI)",
        "Ansicht",
        |ctx, _| {
            let next = (ctx.state.app.ui_scale - 0.25)
                .clamp(crate::core::settings::UI_SCALE_MIN, crate::core::settings::UI_SCALE_MAX);
            ctx.state.settings.ui_scale = Some(next);
            ctx.state.settings.save();
            status(ctx, &format!("UI-Skalierung: {:.0}%", next * 100.0));
        },
    ));
    commands.push(cmd(
        "ui.scaleAuto",
        "UI-Skalierung automatisch (Monitor-DPI)",
        "Ansicht",
        |ctx, _| {
            ctx.state.settings.ui_scale = None;
            ctx.state.settings.save();
            status(ctx, "UI-Skalierung: automatisch (Monitor-DPI)");
        },
    ));
    commands.push(cmd(
        "proxy.pickFolder",
        "Proxy-Ordner wählen…",
        "Medien",
        |ctx, _| ctx.services.pick_proxy_folder(),
    ));
    commands.push(cmd(
        "proxy.resetFolder",
        "Proxy-Ordner auf Standard zurücksetzen",
        "Medien",
        |ctx, _| {
            if ctx.state.media.proxy_settings.folder.take().is_some() {
                ctx.state.media.revision += 1;
            }
        },
    ));

    // ----------------------------------------------- Medien: Bins / Ordner
    commands.push(cmd(
        "media.createBin",
        "Neuen Ordner anlegen",
        "Medien",
        |ctx, args| {
            // Eltern-Bin: explizites Argument, sonst der geöffnete Bin.
            let parent = arg_str(args, "binId")
                .unwrap_or_else(|| ctx.state.media.current_bin().to_string());
            let id = ctx.state.media.create_bin(&parent, "Neuer Ordner");
            // Direkt zum Umbenennen freigeben (Premiere-Verhalten).
            ctx.state.media.rename_request = Some(crate::stores::RenameTarget::Bin(id));
            ctx.state.dock.open_panel("media");
        },
    ));
    commands.push(cmd(
        "media.renameBin",
        "Ordner umbenennen",
        "Medien",
        |ctx, args| {
            if let Some(id) = arg_str(args, "binId") {
                if ctx.state.media.bin_exists(&id) {
                    ctx.state.media.rename_request = Some(crate::stores::RenameTarget::Bin(id));
                }
            }
        },
    ));
    commands.push(cmd(
        "media.deleteBin",
        "Ordner löschen",
        "Medien",
        |ctx, args| {
            let Some(id) = arg_str(args, "binId") else { return };
            if !ctx.state.media.bin_exists(&id) || id == crate::core::bin::ROOT_BIN_ID {
                return;
            }
            // Leerer Ordner (keine Unter-Ordner, keine Assets): direkt löschen.
            let empty = ctx.state.media.bin_subtree(&id).len() == 1
                && ctx.state.media.count_assets_in_subtree(&id) == 0;
            if empty {
                ctx.state.media.delete_bin(&id, true);
            } else {
                ctx.state.app.bin_delete_target = Some(id);
                ctx.state.app.open_dialog = Some(DialogId::DeleteBin);
            }
        },
    ));
    commands.push(cmd(
        "media.openBin",
        "Ordner öffnen",
        "Medien",
        |ctx, args| {
            if let Some(id) = arg_str(args, "binId") {
                ctx.state.media.set_current_bin(&id);
                ctx.state.dock.open_panel("media");
            }
        },
    ));
    commands.push(cmd(
        "media.goToParentBin",
        "Eine Ebene nach oben",
        "Medien",
        |ctx, _| {
            let cur = ctx.state.media.current_bin().to_string();
            let parent = ctx
                .state
                .media
                .bin(&cur)
                .map(|b| b.parent.clone())
                .unwrap_or_else(|| crate::core::bin::ROOT_BIN_ID.to_string());
            ctx.state.media.set_current_bin(&parent);
        },
    ));
    commands.push(cmd(
        "media.moveToBin",
        "In Ordner verschieben",
        "Medien",
        |ctx, args| {
            let Some(bin_id) = arg_str(args, "binId") else { return };
            let ids = target_asset_ids(ctx, args);
            if !ids.is_empty() {
                ctx.state.media.move_assets_to_bin(&ids, &bin_id);
            }
        },
    ));

    // ------------------------------------------- Medien: Metadaten / Etikett
    commands.push(with_when(
        cmd(
            "media.renameAsset",
            "Medium umbenennen",
            "Medien",
            |ctx, args| {
                if let Some(id) = arg_asset_id(ctx, args) {
                    if ctx.state.media.asset(&id).is_some() {
                        ctx.state.media.rename_request =
                            Some(crate::stores::RenameTarget::Asset(id));
                    }
                }
            },
        ),
        "mediaSelected",
    ));
    commands.push(with_when(
        cmd(
            "media.setLabel",
            "Farbetikett setzen",
            "Medien",
            |ctx, args| {
                let ids = target_asset_ids(ctx, args);
                if ids.is_empty() {
                    return;
                }
                let label = arg_str(args, "label")
                    .as_deref()
                    .and_then(crate::core::bin::MediaLabel::from_key);
                ctx.state.media.set_label(&ids, label);
            },
        ),
        "mediaSelected",
    ));
    // Je Etikettfarbe ein Eintrag (Palette + Kontextmenü-Argument).
    for label in crate::core::bin::MediaLabel::ALL {
        let id = format!("media.setLabel.{}", label.key());
        let title = format!("Farbetikett: {}", label.label());
        let mut c = cmd(&id, &title, "Medien", |ctx, args| {
            let ids = target_asset_ids(ctx, args);
            let label = arg_str(args, "label")
                .as_deref()
                .and_then(crate::core::bin::MediaLabel::from_key);
            if !ids.is_empty() {
                ctx.state.media.set_label(&ids, label);
            }
        });
        c.when = Some("mediaSelected");
        c.bound_arg = Some(serde_json::json!({ "label": label.key() }));
        commands.push(c);
    }
    commands.push(with_when(
        cmd(
            "media.clearLabel",
            "Farbetikett entfernen",
            "Medien",
            |ctx, args| {
                let ids = target_asset_ids(ctx, args);
                if !ids.is_empty() {
                    ctx.state.media.set_label(&ids, None);
                }
            },
        ),
        "mediaSelected",
    ));
    commands.push(with_when(
        cmd(
            "media.showInTimeline",
            "In Timeline anzeigen",
            "Medien",
            |ctx, args| {
                let Some(id) = arg_asset_id(ctx, args) else { return };
                if ctx.state.timeline.reveal_asset_usage(&id) {
                    ctx.state.dock.open_panel("timeline");
                } else {
                    status(ctx, "Dieses Medium wird in der Sequenz nicht verwendet");
                }
            },
        ),
        "mediaSelected",
    ));

    // ----------------------------------------------------------- Workspace
    for ws in WORKSPACE_IDS {
        commands.push(with_arg(
            cmd(
                &format!("workspace.switch.{ws}"),
                &format!("Workspace: {}", workspace_name(ws)),
                "Workspace",
                |ctx, arg| {
                    if let Some(Value::String(id)) = arg {
                        let id = id.clone();
                        set_active_workspace(ctx.state, &id);
                    }
                },
            ),
            Value::String(ws.to_string()),
        ));
    }
    commands.push(cmd(
        "workspace.next",
        "Nächster Workspace",
        "Workspace",
        |ctx, _| cycle_workspace(ctx, 1),
    ));
    commands.push(cmd(
        "workspace.previous",
        "Vorheriger Workspace",
        "Workspace",
        |ctx, _| cycle_workspace(ctx, -1),
    ));
    commands.push(cmd(
        "workspace.resetLayout",
        "Layout zurücksetzen",
        "Workspace",
        |ctx, _| ctx.state.dock.reset_current_layout(),
    ));

    // ---------------------------------------------------------- Wiedergabe
    commands.push(cmd(
        "playback.togglePlay",
        "Wiedergabe starten/pausieren",
        "Wiedergabe",
        |ctx, _| playback::dispatch(ctx.state, PlaybackCmd::Toggle),
    ));
    commands.push(cmd(
        "playback.shuttleForward",
        "Shuttle vorwärts",
        "Wiedergabe",
        |ctx, _| playback::dispatch(ctx.state, PlaybackCmd::Shuttle(1.0)),
    ));
    commands.push(cmd(
        "playback.shuttleReverse",
        "Shuttle rückwärts",
        "Wiedergabe",
        |ctx, _| playback::dispatch(ctx.state, PlaybackCmd::Shuttle(-1.0)),
    ));
    commands.push(cmd(
        "playback.shuttleStop",
        "Shuttle stoppen",
        "Wiedergabe",
        |ctx, _| playback::dispatch(ctx.state, PlaybackCmd::Pause),
    ));
    commands.push(with_repeat(cmd(
        "playback.stepForward",
        "Ein Frame vorwärts",
        "Wiedergabe",
        |ctx, _| playback::dispatch(ctx.state, PlaybackCmd::StepFrames(1.0)),
    )));
    commands.push(with_repeat(cmd(
        "playback.stepBackward",
        "Ein Frame zurück",
        "Wiedergabe",
        |ctx, _| playback::dispatch(ctx.state, PlaybackCmd::StepFrames(-1.0)),
    )));
    commands.push(with_repeat(cmd(
        "playback.stepForward5",
        "Fünf Frames vorwärts",
        "Wiedergabe",
        |ctx, _| playback::dispatch(ctx.state, PlaybackCmd::StepFrames(5.0)),
    )));
    commands.push(with_repeat(cmd(
        "playback.stepBackward5",
        "Fünf Frames zurück",
        "Wiedergabe",
        |ctx, _| playback::dispatch(ctx.state, PlaybackCmd::StepFrames(-5.0)),
    )));
    commands.push(cmd(
        "playback.goToStart",
        "Zum Anfang",
        "Wiedergabe",
        |ctx, _| playback::dispatch(ctx.state, PlaybackCmd::GoToStart),
    ));
    commands.push(cmd(
        "playback.goToEnd",
        "Zum Ende",
        "Wiedergabe",
        |ctx, _| playback::dispatch(ctx.state, PlaybackCmd::GoToEnd),
    ));
    commands.push(cmd(
        "playback.setInPoint",
        "In-Punkt setzen",
        "Wiedergabe",
        |ctx, _| playback::dispatch(ctx.state, PlaybackCmd::MarkIn),
    ));
    commands.push(cmd(
        "playback.setOutPoint",
        "Out-Punkt setzen",
        "Wiedergabe",
        |ctx, _| playback::dispatch(ctx.state, PlaybackCmd::MarkOut),
    ));
    commands.push(cmd(
        "playback.clearInOut",
        "In- und Out-Punkt löschen",
        "Wiedergabe",
        |ctx, _| playback::dispatch(ctx.state, PlaybackCmd::ClearMarks),
    ));
    commands.push(cmd(
        "playback.toggleLoop",
        "Loop-Wiedergabe umschalten",
        "Wiedergabe",
        |ctx, _| playback::dispatch(ctx.state, PlaybackCmd::ToggleLoop),
    ));
    commands.push(cmd(
        "playback.toggleAudioScrub",
        "Audio-Scrubbing umschalten",
        "Wiedergabe",
        |ctx, _| {
            let on = !ctx.state.playback.audio_scrub_enabled;
            ctx.state.playback.audio_scrub_enabled = on;
            status(ctx, if on { "Audio-Scrubbing: an" } else { "Audio-Scrubbing: aus" });
        },
    ));

    // ----------------------------------------------------------- Werkzeuge
    for tool in TOOLS {
        commands.push(with_arg(
            cmd(
                &format!("tools.{tool}"),
                tool_command_title(tool),
                "Werkzeuge",
                |ctx, arg| {
                    if let Some(Value::String(tool)) = arg {
                        if let Some(t) = TOOLS.iter().find(|t| *t == tool) {
                            ctx.state.app.active_tool = t;
                        }
                    }
                },
            ),
            Value::String(tool.to_string()),
        ));
    }

    // ------------------------------------------------- Timeline: Ansicht
    commands.push(with_repeat(cmd(
        "timeline.zoomIn",
        "Timeline vergrößern",
        "Timeline",
        |ctx, _| ctx.state.timeline.zoom_in(),
    )));
    commands.push(with_repeat(cmd(
        "timeline.zoomOut",
        "Timeline verkleinern",
        "Timeline",
        |ctx, _| ctx.state.timeline.zoom_out(),
    )));
    commands.push(with_when(
        cmd(
            "timeline.zoomFit",
            "Timeline an Sequenz anpassen",
            "Timeline",
            |ctx, _| ctx.state.timeline.zoom_to_fit(),
        ),
        "timelineHasClips",
    ));
    commands.push(cmd(
        "timeline.toggleSnapping",
        "Einrasten umschalten",
        "Timeline",
        |ctx, _| {
            ctx.state.timeline.toggle_snapping();
            let msg = if ctx.state.timeline.snapping {
                "Einrasten aktiviert"
            } else {
                "Einrasten deaktiviert"
            };
            status(ctx, msg);
        },
    ));
    // ------------------------------------------------------- Marker
    // Kontextabhängig: bei fokussiertem Quellmonitor wirken die Marker-
    // Aktionen auf das geladene Asset (Quellzeit), sonst auf die Sequenz.
    // Bewusst OHNE allow_repeat: ein Tastendruck = ein Marker (sonst würde
    // gehaltenes M während der Wiedergabe die Timeline mit Markern fluten).
    commands.push(cmd(
        "marker.add",
        "Marker am Playhead",
        "Marker",
        |ctx, _| {
            if let Some(aid) = source_marker_target(ctx.state) {
                let pos = ctx.state.playback.source.position;
                ctx.state.media.add_asset_marker(&aid, pos);
                status(ctx, "Quell-Marker gesetzt");
            } else {
                let t = ctx.state.timeline.playhead_sec;
                ctx.state.timeline.add_marker_at(t);
            }
        },
    ));
    commands.push(cmd(
        "marker.addDialog",
        "Marker hinzufügen + bearbeiten…",
        "Marker",
        |ctx, _| {
            if let Some(aid) = source_marker_target(ctx.state) {
                let pos = ctx.state.playback.source.position;
                if let Some(mid) = ctx.state.media.add_asset_marker(&aid, pos) {
                    open_marker_dialog(ctx.state, MarkerScope::Asset(aid), mid);
                }
            } else {
                let t = ctx.state.timeline.playhead_sec;
                let id = ctx.state.timeline.add_marker_at(t);
                open_marker_dialog(ctx.state, MarkerScope::Sequence, id);
            }
        },
    ));
    commands.push(with_repeat(cmd(
        "marker.next",
        "Zum nächsten Marker",
        "Marker",
        |ctx, _| {
            if let Some(aid) = source_marker_target(ctx.state) {
                source_marker_step(ctx.state, &aid, 1);
            } else if !ctx.state.timeline.go_to_next_marker() {
                status(ctx, "Kein weiterer Marker");
            }
        },
    )));
    commands.push(with_repeat(cmd(
        "marker.prev",
        "Zum vorherigen Marker",
        "Marker",
        |ctx, _| {
            if let Some(aid) = source_marker_target(ctx.state) {
                source_marker_step(ctx.state, &aid, -1);
            } else if !ctx.state.timeline.go_to_prev_marker() {
                status(ctx, "Kein vorheriger Marker");
            }
        },
    )));
    commands.push(cmd(
        "marker.deleteAtPlayhead",
        "Marker am Playhead löschen",
        "Marker",
        |ctx, _| {
            if let Some(aid) = source_marker_target(ctx.state) {
                let pos = ctx.state.playback.source.position;
                if let Some(mid) = ctx.state.media.asset_marker_at(&aid, pos) {
                    ctx.state.media.remove_asset_marker(&aid, &mid);
                    status(ctx, "Quell-Marker gelöscht");
                }
            } else if ctx.state.timeline.remove_marker_at_playhead() {
                status(ctx, "Marker gelöscht");
            } else {
                status(ctx, "Kein Marker am Playhead");
            }
        },
    ));
    commands.push(cmd(
        "marker.clearAll",
        "Alle Marker löschen",
        "Marker",
        |ctx, _| {
            if let Some(aid) = source_marker_target(ctx.state) {
                clear_asset_markers(ctx.state, &aid);
                status(ctx, "Alle Quell-Marker gelöscht");
            } else {
                ctx.state.timeline.clear_markers();
                status(ctx, "Alle Marker gelöscht");
            }
        },
    ));
    commands.push(cmd(
        "markers.openPanel",
        "Marker-Panel öffnen",
        "Marker",
        |ctx, _| ctx.state.dock.open_panel("markers"),
    ));

    // -------------------------------------------- Timeline: Bearbeitung
    commands.push(with_when(
        cmd(
            "timeline.splitAtPlayhead",
            "Am Playhead schneiden",
            "Timeline",
            |ctx, _| {
                let t = ctx.state.timeline.playhead_sec;
                let sel = ctx.state.timeline.selected_clip_ids.clone();
                ctx.state
                    .timeline
                    .split_at(t, if sel.is_empty() { None } else { Some(&sel) });
            },
        ),
        "timelineHasClips",
    ));
    commands.push(with_when(
        cmd(
            "timeline.deleteSelected",
            "Auswahl löschen (Clips/Übergänge)",
            "Timeline",
            |ctx, _| {
                // Übergangsauswahl hat Vorrang (sie schließt Clips aus).
                let trs = ctx.state.timeline.selected_transition_ids.clone();
                if !trs.is_empty() {
                    ctx.state.timeline.remove_transitions(&trs);
                    return;
                }
                let ids = ctx.state.timeline.selected_clip_ids.clone();
                ctx.state.timeline.delete_clips(&ids, false);
            },
        ),
        "timelineClipSelected || timelineTransitionSelected",
    ));
    commands.push(with_when(
        cmd(
            "timeline.rippleDelete",
            "Clips löschen und Lücke schließen",
            "Timeline",
            |ctx, _| {
                let ids = ctx.state.timeline.selected_clip_ids.clone();
                ctx.state.timeline.delete_clips(&ids, true);
            },
        ),
        "timelineClipSelected",
    ));
    // ----------------------------------------- Three-Point-Editing
    // Einfügen (Komma) und Überschreiben (Punkt) wie Premiere: das im
    // Quellmonitor geladene Material (mit In/Out) bzw. ein einzeln im Bin
    // gewähltes Asset wird gemäß Source-Patching in die Timeline geschnitten.
    // Bewusst ohne `when`, damit der Schnitt rein tastaturgetrieben
    // unabhängig vom fokussierten Panel funktioniert.
    commands.push(cmd(
        "timeline.insert",
        "Einfügen (Insert)",
        "Timeline",
        |ctx, _| {
            if let Some(msg) =
                crate::core::edit::perform_source_edit(ctx.state, crate::core::edit::EditMode::Insert)
            {
                status(ctx, &msg);
            }
        },
    ));
    commands.push(cmd(
        "timeline.overwrite",
        "Überschreiben (Overwrite)",
        "Timeline",
        |ctx, _| {
            if let Some(msg) = crate::core::edit::perform_source_edit(
                ctx.state,
                crate::core::edit::EditMode::Overwrite,
            ) {
                status(ctx, &msg);
            }
        },
    ));
    commands.push(with_when(
        cmd("timeline.liftRange", "Heben (Lift)", "Timeline", |ctx, _| {
            if !ctx.state.timeline.lift_range() {
                status(ctx, "Nichts zum Heben (In/Out + Zielspur nötig)");
            }
        }),
        "timelineInOutSet",
    ));
    commands.push(with_when(
        cmd(
            "timeline.extractRange",
            "Entnehmen (Extract)",
            "Timeline",
            |ctx, _| {
                if !ctx.state.timeline.extract_range() {
                    status(ctx, "Nichts zum Entnehmen (In/Out + Zielspur nötig)");
                }
            },
        ),
        "timelineInOutSet",
    ));
    commands.push(with_when(
        cmd(
            "timeline.matchFrame",
            "Frame abgleichen (Match Frame)",
            "Timeline",
            |ctx, _| {
                if let Some(msg) = crate::core::edit::match_frame(ctx.state) {
                    status(ctx, &msg);
                }
            },
        ),
        "timelineHasClips",
    ));
    commands.push(with_when(
        cmd(
            "timeline.extendEdit",
            "Schnitt zum Playhead ziehen (Extend Edit)",
            "Timeline",
            |ctx, _| {
                if !ctx.state.timeline.extend_edit() {
                    status(ctx, "Keine Schnittkante auf den Ziel-Spuren");
                }
            },
        ),
        "timelineHasClips",
    ));

    commands.push(with_when(
        cmd("timeline.copy", "Clips kopieren", "Timeline", |ctx, _| {
            ctx.state.timeline.copy_selection()
        }),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd("timeline.cut", "Clips ausschneiden", "Timeline", |ctx, _| {
            ctx.state.timeline.cut_selection()
        }),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd(
            "timeline.paste",
            "Clips am Playhead einfügen",
            "Timeline",
            |ctx, _| ctx.state.timeline.paste(None),
        ),
        "timelineClipboard",
    ));
    commands.push(with_when(
        cmd(
            "timeline.selectAll",
            "Alle Clips auswählen",
            "Timeline",
            |ctx, _| ctx.state.timeline.select_all(),
        ),
        "timelineHasClips",
    ));
    commands.push(with_when(
        cmd(
            "timeline.deselectAll",
            "Clip-Auswahl aufheben",
            "Timeline",
            |ctx, _| ctx.state.timeline.clear_selection(),
        ),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd(
            "timeline.toggleLink",
            "Video/Audio verknüpfen bzw. lösen",
            "Timeline",
            |ctx, _| ctx.state.timeline.toggle_link_selected(),
        ),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd(
            "timeline.toggleClipEnabled",
            "Clip aktivieren/deaktivieren",
            "Timeline",
            |ctx, _| {
                let sel = ctx.state.timeline.selected_clip_ids.clone();
                let all_enabled = ctx
                    .state
                    .timeline
                    .clips
                    .iter()
                    .filter(|c| sel.contains(&c.id))
                    .all(|c| c.enabled);
                ctx.state.timeline.set_clips_enabled(&sel, !all_enabled);
            },
        ),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd(
            "timeline.clipGainUp",
            "Clip-Verstärkung +1 dB",
            "Timeline",
            |ctx, _| ctx.state.timeline.nudge_selected_clip_gain(1.0),
        ),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd(
            "timeline.clipGainDown",
            "Clip-Verstärkung −1 dB",
            "Timeline",
            |ctx, _| ctx.state.timeline.nudge_selected_clip_gain(-1.0),
        ),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd(
            "timeline.clipGainReset",
            "Clip-Verstärkung zurücksetzen",
            "Timeline",
            |ctx, _| ctx.state.timeline.reset_selected_clip_gain(),
        ),
        "timelineClipSelected",
    ));
    commands.push(cmd(
        "timeline.clearTrackAutomation",
        "Spur-Automation entfernen",
        "Timeline",
        |ctx, _| ctx.state.timeline.clear_track_automation_targeted(),
    ));
    // ------------------------------------------- Clip: Geschwindigkeit/Dauer
    commands.push(with_when(
        cmd(
            "clip.speedDuration",
            "Geschwindigkeit/Dauer…",
            "Clip",
            |ctx, _| ctx.state.app.open_dialog = Some(DialogId::ClipSpeed),
        ),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd(
            "clip.freezeFrame",
            "Frame einfrieren (am Playhead)",
            "Clip",
            |ctx, _| {
                let ids = ctx.state.timeline.selected_clip_ids.clone();
                ctx.state.timeline.freeze_frame_at_playhead(&ids);
            },
        ),
        "timelineClipSelected",
    ));

    // ------------------------------------------------ Clip: Effekte/Keyframes
    commands.push(with_when(
        cmd(
            "clip.resetMotion",
            "Bewegung zurücksetzen",
            "Clip",
            |ctx, _| {
                let ids = ctx.state.timeline.selected_clip_ids.clone();
                ctx.state.timeline.fx_reset_motion(&ids);
            },
        ),
        "timelineClipSelected",
    ));
    commands.push(with_repeat(with_when(
        cmd(
            "clip.prevKeyframe",
            "Zum vorherigen Keyframe",
            "Clip",
            |ctx, _| jump_to_keyframe(ctx, -1),
        ),
        "timelineClipSelected",
    )));
    commands.push(with_repeat(with_when(
        cmd(
            "clip.nextKeyframe",
            "Zum nächsten Keyframe",
            "Clip",
            |ctx, _| jump_to_keyframe(ctx, 1),
        ),
        "timelineClipSelected",
    )));

    // ----------------------------------------------------- Clip: Farbe
    commands.push(with_when(
        cmd(
            "clip.resetGrade",
            "Farbkorrektur zurücksetzen",
            "Clip",
            |ctx, _| {
                let ids = ctx.state.timeline.selected_clip_ids.clone();
                ctx.state.timeline.grade_reset(&ids);
            },
        ),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd(
            "clip.toggleGrade",
            "Farbkorrektur umgehen (Bypass)",
            "Clip",
            |ctx, _| {
                // Alle ausgewählten Video-Clips gemeinsam umschalten.
                let ids: Vec<String> = ctx
                    .state
                    .timeline
                    .selected_clip_ids
                    .iter()
                    .filter(|id| {
                        ctx.state
                            .timeline
                            .clip(id)
                            .is_some_and(|c| c.kind == TrackKind::Video)
                    })
                    .cloned()
                    .collect();
                for id in ids {
                    ctx.state.timeline.grade_toggle_enabled(&id);
                }
            },
        ),
        "timelineClipSelected",
    ));
    // Grade kopieren/einfügen: die komplette Farbkorrektur eines Clips ins
    // interne Klemmbrett (AppState, sequenzübergreifend) und von dort auf alle
    // selektierten Clips. Quelle wird wie im Farbe-Panel aufgelöst (bevorzugt
    // ein ausgewählter Video-Clip bzw. der Video-Partner eines Audio-Clips).
    commands.push(with_when(
        cmd(
            "color.copyGrade",
            "Farbkorrektur kopieren",
            "Clip",
            |ctx, _| {
                if let Some(grade) = selected_grade(ctx.state) {
                    ctx.state.grade_clipboard = Some(grade);
                    status(ctx, "Farbkorrektur kopiert");
                } else {
                    status(ctx, "Kein Bild-Clip ausgewählt – Farbkorrektur nicht kopiert");
                }
            },
        ),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd(
            "color.pasteGrade",
            "Farbkorrektur einfügen",
            "Clip",
            |ctx, _| {
                let Some(grade) = ctx.state.grade_clipboard.clone() else {
                    return;
                };
                let ids = ctx.state.timeline.selected_clip_ids.clone();
                let n = ctx.state.timeline.paste_grade(&grade, &ids);
                if n > 0 {
                    let word = if n == 1 { "Clip" } else { "Clips" };
                    status(ctx, &format!("Farbkorrektur auf {n} {word} angewendet"));
                }
            },
        ),
        "timelineClipSelected && colorGradeClipboard",
    ));

    // ----------------------------------------------------- Clip: Effekte
    commands.push(with_when(
        cmd(
            "clip.addEffect",
            "Effekt anwenden…",
            "Effekte",
            |ctx, args| {
                let Some(kind) = args
                    .and_then(|v| v.get("kind"))
                    .and_then(|v| v.as_str())
                    .and_then(|key| {
                        crate::core::effects::EffectKind::ALL
                            .iter()
                            .find(|k| k.key() == key)
                            .copied()
                    })
                else {
                    status(ctx, "Unbekannter Effekt");
                    return;
                };
                let ids = ctx.state.timeline.selected_clip_ids.clone();
                // Doppelte Ziele vermeiden (A/V-Paare zeigen auf denselben Clip).
                let mut applied: Vec<String> = Vec::new();
                for id in &ids {
                    if let Some(target) = ctx.state.timeline.effect_target_clip(id, kind) {
                        if !applied.contains(&target) {
                            ctx.state.timeline.effects_add(id, kind);
                            applied.push(target);
                        }
                    }
                }
                if applied.is_empty() {
                    status(
                        ctx,
                        &format!("„{}“ passt nicht zur Auswahl", kind.label()),
                    );
                } else {
                    status(
                        ctx,
                        &format!("„{}“ auf {} Clip(s) angewendet", kind.label(), applied.len()),
                    );
                }
            },
        ),
        "timelineClipSelected",
    ));
    // Je Effekt ein Palette-Eintrag mit gebundenem Argument.
    for kind in crate::core::effects::EffectKind::ALL {
        let id = format!("clip.addEffect.{}", kind.key());
        let title = format!("Effekt anwenden: {}", kind.label());
        let mut c = cmd(&id, &title, "Effekte", |ctx, args| {
            if let Some(registry_cmd) = args {
                let arg = registry_cmd.clone();
                let ids = ctx.state.timeline.selected_clip_ids.clone();
                let Some(kind) = arg
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .and_then(|key| {
                        crate::core::effects::EffectKind::ALL
                            .iter()
                            .find(|k| k.key() == key)
                            .copied()
                    })
                else {
                    return;
                };
                let mut applied: Vec<String> = Vec::new();
                for id in &ids {
                    if let Some(target) = ctx.state.timeline.effect_target_clip(id, kind) {
                        if !applied.contains(&target) {
                            ctx.state.timeline.effects_add(id, kind);
                            applied.push(target);
                        }
                    }
                }
            }
        });
        c.when = Some("timelineClipSelected");
        c.bound_arg = Some(serde_json::json!({ "kind": kind.key() }));
        commands.push(c);
    }
    // ----------------------------------------------------- Übergänge
    commands.push(with_when(
        cmd(
            "clip.applyDefaultVideoTransition",
            "Standard-Videoübergang anwenden",
            "Übergänge",
            |ctx, _| {
                apply_transition_kind(
                    ctx,
                    crate::core::transitions::TransitionKind::default_for_audio(false),
                )
            },
        ),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd(
            "clip.applyDefaultAudioTransition",
            "Standard-Audioübergang anwenden",
            "Übergänge",
            |ctx, _| {
                apply_transition_kind(
                    ctx,
                    crate::core::transitions::TransitionKind::default_for_audio(true),
                )
            },
        ),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd(
            "clip.applyTransition",
            "Übergang anwenden…",
            "Übergänge",
            |ctx, args| {
                let Some(kind) = args
                    .and_then(|v| v.get("kind"))
                    .and_then(|v| v.as_str())
                    .and_then(|key| {
                        crate::core::transitions::TransitionKind::ALL
                            .iter()
                            .find(|k| k.key() == key)
                            .copied()
                    })
                else {
                    status(ctx, "Unbekannter Übergang");
                    return;
                };
                apply_transition_kind(ctx, kind);
            },
        ),
        "timelineClipSelected",
    ));
    // Je Übergang ein Palette-Eintrag mit gebundenem Argument.
    for kind in crate::core::transitions::TransitionKind::ALL {
        let id = format!("clip.applyTransition.{}", kind.key());
        let title = format!("Übergang anwenden: {}", kind.label());
        let mut c = cmd(&id, &title, "Übergänge", |ctx, args| {
            let Some(kind) = args
                .and_then(|v| v.get("kind"))
                .and_then(|v| v.as_str())
                .and_then(|key| {
                    crate::core::transitions::TransitionKind::ALL
                        .iter()
                        .find(|k| k.key() == key)
                        .copied()
                })
            else {
                return;
            };
            apply_transition_kind(ctx, kind);
        });
        c.when = Some("timelineClipSelected");
        c.bound_arg = Some(serde_json::json!({ "kind": kind.key() }));
        commands.push(c);
    }
    commands.push(with_when(
        cmd(
            "transition.remove",
            "Ausgewählte Übergänge entfernen",
            "Übergänge",
            |ctx, _| {
                let ids = ctx.state.timeline.selected_transition_ids.clone();
                ctx.state.timeline.remove_transitions(&ids);
            },
        ),
        "timelineTransitionSelected",
    ));

    commands.push(with_when(
        cmd(
            "clip.removeAllEffects",
            "Alle Effekte entfernen",
            "Effekte",
            |ctx, _| {
                let ids = ctx.state.timeline.selected_clip_ids.clone();
                ctx.state.timeline.effects_clear(&ids);
            },
        ),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd(
            "clip.toggleEffects",
            "Effekte umgehen (Bypass)",
            "Effekte",
            |ctx, _| {
                let ids = ctx.state.timeline.selected_clip_ids.clone();
                ctx.state.timeline.effects_toggle_bypass(&ids);
            },
        ),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd(
            "clip.copyAttributes",
            "Attribute kopieren",
            "Clip",
            |ctx, _| {
                let Some(id) = ctx.state.timeline.selected_clip_ids.first().cloned() else {
                    return;
                };
                if ctx.state.timeline.copy_attributes(&id) {
                    status(ctx, "Attribute kopiert (Bewegung, Farbe, Effekte)");
                }
            },
        ),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        cmd(
            "clip.pasteAttributes",
            "Attribute einfügen",
            "Clip",
            |ctx, _| {
                let ids = ctx.state.timeline.selected_clip_ids.clone();
                ctx.state.timeline.paste_attributes(&ids);
            },
        ),
        "timelineClipSelected && timelineAttrClipboard",
    ));

    // ----------------------------------------------------------- Grafik/Titel
    // „Titel hinzufügen“: legt einen Titel-Clip am Playhead auf der nächsten
    // freien Videospur an; je Vorlage ein Palette-Eintrag mit gebundenem
    // Argument. Die Abspann-Vorlage erhält ihre Scroll-Keyframes hier.
    fn add_title(ctx: &mut CommandCtx, args: Option<&Value>) {
        use crate::core::title::TitleTemplate;
        let template = args
            .and_then(|v| v.get("template"))
            .and_then(|v| v.as_str())
            .and_then(TitleTemplate::from_key)
            .unwrap_or(TitleTemplate::Plain);
        let at = ctx.state.timeline.playhead_sec;
        let duration = template.default_duration();
        let id = ctx
            .state
            .timeline
            .add_title_clip(template.build(), at, duration);
        if template.scrolls() {
            // Scroll-Animation: Block läuft von unterhalb des Frames nach
            // oben hinaus (Keyframes in Medienzeit — Teil desselben Snapshots).
            if let Some(clip) = ctx.state.timeline.clips.iter_mut().find(|c| c.id == id) {
                clip.fx.pos_y.upsert_key(0.0, 110.0);
                clip.fx.pos_y.upsert_key(duration, -110.0);
            }
        }
        ctx.state.dock.open_panel("graphics");
        let msg = format!("„{}“ am Playhead eingefügt", template.label());
        status(ctx, &msg);
    }
    commands.push(cmd(
        "title.add",
        "Titel hinzufügen",
        "Grafik",
        add_title,
    ));
    for template in crate::core::title::TitleTemplate::ALL {
        let mut c = cmd(
            &format!("title.add.{}", template.key()),
            &format!("Titel hinzufügen: {}", template.label()),
            "Grafik",
            add_title,
        );
        c.bound_arg = Some(serde_json::json!({ "template": template.key() }));
        commands.push(c);
    }
    commands.push(cmd(
        "monitor.toggleSafeMargins",
        "Sichere Ränder im Programmmonitor",
        "Wiedergabe",
        |ctx, _| {
            ctx.state.monitor.safe_margins = !ctx.state.monitor.safe_margins;
        },
    ));

    // ------------------------------------------------------------ Untertitel
    commands.push(cmd(
        "subtitle.addAtPlayhead",
        "Untertitel am Playhead hinzufügen",
        "Untertitel",
        |ctx, _| {
            let at = ctx.state.timeline.playhead_sec;
            match ctx.state.timeline.add_subtitle_clip("Untertitel", at) {
                Ok(_) => {
                    ctx.state.dock.open_panel("subtitles");
                }
                Err(err) => {
                    let msg = err.clone();
                    status(ctx, &msg);
                }
            }
        },
    ));
    commands.push(cmd(
        "subtitle.addTrack",
        "Untertitelspur hinzufügen",
        "Untertitel",
        |ctx, _| {
            ctx.state.timeline.add_track(TrackKind::Subtitle);
        },
    ));
    commands.push(cmd(
        "subtitle.importSrt",
        "Untertitel importieren (SRT)…",
        "Untertitel",
        |ctx, _| ctx.services.pick_subtitle_import(),
    ));
    commands.push(with_when(
        cmd(
            "subtitle.exportSrt",
            "Untertitel exportieren (SRT)…",
            "Untertitel",
            |ctx, _| {
                let Some(track) = ctx.state.timeline.active_subtitle_track() else {
                    status(ctx, "Keine Untertitel-Spur vorhanden");
                    return;
                };
                let track_id = track.id.clone();
                if ctx.state.timeline.subtitle_cues(&track_id).is_empty() {
                    status(ctx, "Die aktive Untertitel-Spur enthält keine Segmente");
                    return;
                }
                let name = format!("{}.srt", ctx.state.project.display_name());
                ctx.services.pick_subtitle_export_target(&name);
            },
        ),
        "timelineHasSubtitles",
    ));
    commands.push(with_when(
        cmd(
            "subtitle.split",
            "Untertitel am Playhead teilen",
            "Untertitel",
            |ctx, _| match ctx.state.timeline.subtitle_split_at_playhead() {
                Ok(_) => ctx.state.dock.open_panel("subtitles"),
                Err(err) => {
                    let msg = err.clone();
                    status(ctx, &msg);
                }
            },
        ),
        "timelineHasSubtitles",
    ));
    commands.push(with_when(
        cmd(
            "subtitle.merge",
            "Untertitel mit Nachbarn zusammenführen",
            "Untertitel",
            |ctx, _| match ctx.state.timeline.subtitle_merge() {
                Ok(_) => ctx.state.dock.open_panel("subtitles"),
                Err(err) => {
                    let msg = err.clone();
                    status(ctx, &msg);
                }
            },
        ),
        "timelineHasSubtitles",
    ));

    commands.push(cmd(
        "timeline.addVideoTrack",
        "Videospur hinzufügen",
        "Timeline",
        |ctx, _| {
            ctx.state.timeline.add_track(TrackKind::Video);
        },
    ));
    commands.push(cmd(
        "timeline.addAudioTrack",
        "Audiospur hinzufügen",
        "Timeline",
        |ctx, _| {
            ctx.state.timeline.add_track(TrackKind::Audio);
        },
    ));
    commands.push(cmd(
        "timeline.removeTrack",
        "Spur entfernen",
        "Timeline",
        |ctx, args| {
            // Ziel: explizites Argument (Kontextmenü), sonst die fokussierte
            // Spur (Tastatur — Spur des ausgewählten Clips bzw. anvisierte Spur).
            let Some(target) = arg_str(args, "trackId")
                .or_else(|| ctx.state.timeline.focused_track_id())
            else {
                status(ctx, "Keine Spur ausgewählt — zum Entfernen den Spurkopf rechtsklicken");
                return;
            };
            if !ctx.state.timeline.tracks.iter().any(|t| t.id == target) {
                return;
            }
            // Belegte Spur: erst bestätigen (die Clips würden mit gelöscht).
            if ctx.state.timeline.track_clip_count(&target) > 0 {
                ctx.state.app.remove_track_target = Some(target);
                ctx.state.app.open_dialog = Some(DialogId::ConfirmRemoveTrack);
                return;
            }
            ctx.state.timeline.remove_track(&target);
        },
    ));

    // ---------------------------------------------- Timeline: Playhead
    commands.push(cmd(
        "timeline.goToStart",
        "Playhead zum Sequenzanfang",
        "Timeline",
        |ctx, _| ctx.state.timeline.go_to_start(),
    ));
    commands.push(cmd(
        "timeline.goToEnd",
        "Playhead zum Sequenzende",
        "Timeline",
        |ctx, _| ctx.state.timeline.go_to_end(),
    ));
    commands.push(with_repeat(cmd(
        "timeline.stepBackward",
        "Playhead: ein Frame zurück",
        "Timeline",
        |ctx, _| ctx.state.timeline.step_playhead_frames(-1.0),
    )));
    commands.push(with_repeat(cmd(
        "timeline.stepForward",
        "Playhead: ein Frame vorwärts",
        "Timeline",
        |ctx, _| ctx.state.timeline.step_playhead_frames(1.0),
    )));
    commands.push(with_repeat(cmd(
        "timeline.stepBackward5",
        "Playhead: fünf Frames zurück",
        "Timeline",
        |ctx, _| ctx.state.timeline.step_playhead_frames(-5.0),
    )));
    commands.push(with_repeat(cmd(
        "timeline.stepForward5",
        "Playhead: fünf Frames vorwärts",
        "Timeline",
        |ctx, _| ctx.state.timeline.step_playhead_frames(5.0),
    )));
    // Feinpositionierung per Tastatur: Auswahl bzw. (im Ripple-/Rolling-
    // Werkzeug) die aktive Schnittkante frame-genau verschieben/trimmen.
    commands.push(with_when(
        with_repeat(cmd(
            "clip.nudgeLeft",
            "Clip/Kante: ein Frame nach links",
            "Timeline",
            |ctx, _| nudge_timeline(ctx, -1.0),
        )),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        with_repeat(cmd(
            "clip.nudgeRight",
            "Clip/Kante: ein Frame nach rechts",
            "Timeline",
            |ctx, _| nudge_timeline(ctx, 1.0),
        )),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        with_repeat(cmd(
            "clip.nudgeLeftMany",
            "Clip/Kante: fünf Frames nach links",
            "Timeline",
            |ctx, _| nudge_timeline(ctx, -5.0),
        )),
        "timelineClipSelected",
    ));
    commands.push(with_when(
        with_repeat(cmd(
            "clip.nudgeRightMany",
            "Clip/Kante: fünf Frames nach rechts",
            "Timeline",
            |ctx, _| nudge_timeline(ctx, 5.0),
        )),
        "timelineClipSelected",
    ));
    commands.push(with_repeat(cmd(
        "timeline.prevEdit",
        "Zum vorherigen Schnittpunkt",
        "Timeline",
        |ctx, _| ctx.state.timeline.go_to_prev_edit(),
    )));
    commands.push(with_repeat(cmd(
        "timeline.nextEdit",
        "Zum nächsten Schnittpunkt",
        "Timeline",
        |ctx, _| ctx.state.timeline.go_to_next_edit(),
    )));

    // -------------------------------------------- Timeline: intern (Menüs)
    commands.push(with_when(
        cmd(
            "timeline.trimEdge",
            "Kante trimmen",
            "Timeline",
            |ctx, args| {
                let Some(Value::Object(map)) = args else { return };
                let (Some(Value::String(id)), Some(edge), Some(delta)) = (
                    map.get("clipId"),
                    map.get("edge").and_then(|v| v.as_str()),
                    map.get("delta").and_then(|v| v.as_f64()),
                ) else {
                    return;
                };
                let edge = if edge == "start" {
                    TrimEdge::Start
                } else {
                    TrimEdge::End
                };
                ctx.state.timeline.trim_clip(id, edge, delta);
            },
        ),
        "timelineHasClips",
    ));

    // ------------------------------------------------------------- Fenster
    for (panel_id, title, _) in crate::panels::PANEL_DEFS {
        commands.push(with_arg(
            cmd(
                &format!("window.openPanel.{panel_id}"),
                &format!("Panel: {title}"),
                "Fenster",
                |ctx, arg| {
                    if let Some(Value::String(panel)) = arg {
                        let panel = panel.clone();
                        ctx.state.dock.open_panel(&panel);
                    }
                },
            ),
            Value::String(panel_id.to_string()),
        ));
    }

    // Stabil sortiert nach Kategorie und Titel (Befehlspalette).
    commands.sort_by(|a, b| {
        let cat = a.category.to_lowercase().cmp(&b.category.to_lowercase());
        cat.then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    CommandRegistry { commands }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bin::ROOT_BIN_ID;
    use crate::services::Services;

    fn run(reg: &CommandRegistry, state: &mut AppState, services: &Services, id: &str) {
        let mut ctx = CommandCtx { state, services, now: 0.0 };
        reg.execute(id, None, &mut ctx);
    }

    /// Extend Edit ist registriert UND in allen drei Presets auf „E" gebunden
    /// (Premiere-Konvention) — Verdrahtung gegen versehentliches Lösen sichern.
    #[test]
    fn extend_edit_command_bound_to_e_in_all_presets() {
        let reg = build_registry();
        assert!(reg.get("timeline.extendEdit").is_some(), "Command registriert");
        for preset in crate::core::keyboard::presets() {
            let bound = preset
                .bindings
                .iter()
                .any(|b| b.command == "timeline.extendEdit" && b.keys == "E");
            assert!(bound, "Preset {} bindet E auf Extend Edit", preset.id);
        }
    }

    /// edit.undo/redo machen Timeline- und Medien-Operationen in globaler
    /// zeitlicher Reihenfolge rückgängig (jüngste zuerst) bzw. wieder her.
    #[test]
    fn undo_redo_coordinates_timeline_and_media() {
        let reg = build_registry();
        let services = Services::new();
        let mut state = AppState::default();

        // op1 (älter): Timeline-Marker. op2 (jünger): Bin anlegen.
        state.timeline.add_marker_at(1.0);
        let bin = state.media.create_bin(ROOT_BIN_ID, "Footage");
        assert!(reg.is_enabled("edit.undo", &state));

        // Undo #1: jüngste Operation = Bin-Anlage.
        run(&reg, &mut state, &services, "edit.undo");
        assert!(state.media.bin(&bin).is_none(), "Bin rückgängig");
        assert_eq!(state.timeline.markers.len(), 1, "Marker bleibt");

        // Undo #2: nun der Timeline-Marker.
        run(&reg, &mut state, &services, "edit.undo");
        assert!(state.timeline.markers.is_empty(), "Marker rückgängig");
        assert!(!reg.is_enabled("edit.undo", &state), "nichts mehr rückgängig");

        // Redo #1: älteste vorgemerkte Operation = Marker.
        run(&reg, &mut state, &services, "edit.redo");
        assert_eq!(state.timeline.markers.len(), 1, "Marker wieder da");
        assert!(state.media.bin(&bin).is_none(), "Bin noch nicht zurück");

        // Redo #2: Bin-Anlage.
        run(&reg, &mut state, &services, "edit.redo");
        assert!(state.media.bin(&bin).is_some(), "Bin wiederhergestellt");
    }

    #[test]
    fn remove_used_media_opens_confirmation() {
        let reg = build_registry();
        let services = Services::new();
        let mut state = AppState::default();
        // Asset + verwendender Clip.
        let mut a = crate::core::types::MediaAsset {
            extra: Default::default(),
            id: "a1".into(),
            path: "/tmp/a1.mp4".into(),
            name: "a1".into(),
            kind: crate::core::types::MediaKind::Video,
            info: crate::core::types::MediaInfo {
                path: "/tmp/a1.mp4".into(),
                file_name: "a1.mp4".into(),
                container: "mp4".into(),
                duration_sec: 5.0,
                size_bytes: 1,
                video: Vec::new(),
                audio: Vec::new(),
                recorded_at: None,
            },
            thumbnail_path: None,
            imported_at: 0.0,
            bin_id: ROOT_BIN_ID.to_string(),
            label: None,
            offline: false,
            markers: Vec::new(),
            proxy_path: None,
            proxy_src_mtime: None,
            proxy_offline: false,
        };
        a.bin_id = ROOT_BIN_ID.to_string();
        state.media.add_asset(a);
        let v = state.timeline.tracks.iter().find(|t| t.kind == TrackKind::Video).unwrap().id.clone();
        state.timeline.clips.push(crate::core::timeline::TimelineClip {
            extra: Default::default(),
            id: "c1".into(),
            track_id: v,
            asset_id: "a1".into(),
            name: "a1".into(),
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
            markers: Vec::new(),
            nest_seq: None,
            multicam: None,
            blend_mode: crate::core::compose::BlendMode::default(),
        });
        state.media.select(vec!["a1".into()]);

        // Verwendetes Medium → Bestätigungsdialog statt sofortigem Entfernen.
        run(&reg, &mut state, &services, "media.removeSelected");
        assert_eq!(state.app.open_dialog, Some(DialogId::ConfirmRemoveMedia));
        assert!(state.media.asset("a1").is_some(), "noch nicht entfernt");

        // Bestätigen entfernt Asset + Clip und schließt den Dialog.
        run(&reg, &mut state, &services, "media.removeSelectedConfirmed");
        assert!(state.media.asset("a1").is_none());
        assert!(state.timeline.clips.is_empty());
        assert_eq!(state.app.open_dialog, None);
    }

    /// `timeline.removeTrack`: leere Spur sofort weg, belegte Spur erst nach
    /// Bestätigung (Clips würden mit gelöscht).
    #[test]
    fn remove_track_prompts_only_when_occupied() {
        let reg = build_registry();
        let services = Services::new();
        let mut state = AppState::default();
        let empty = state.timeline.tracks[0].id.clone();
        let used = state.timeline.tracks[1].id.clone();
        state.timeline.clips.push(crate::core::timeline::TimelineClip {
            extra: Default::default(),
            id: "c1".into(),
            track_id: used.clone(),
            asset_id: String::new(),
            name: "Titel".into(),
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
            markers: Vec::new(),
            nest_seq: None,
            multicam: None,
            blend_mode: crate::core::compose::BlendMode::default(),
        });
        let before = state.timeline.tracks.len();

        // Leere Spur: ohne Rückfrage entfernt.
        let args = serde_json::json!({ "trackId": empty });
        {
            let mut ctx = CommandCtx { state: &mut state, services: &services, now: 0.0 };
            reg.execute("timeline.removeTrack", Some(&args), &mut ctx);
        }
        assert_eq!(state.app.open_dialog, None);
        assert_eq!(state.timeline.tracks.len(), before - 1);
        assert!(!state.timeline.tracks.iter().any(|t| t.id == empty));

        // Belegte Spur: Bestätigungsdialog, Spur bleibt zunächst stehen.
        let args = serde_json::json!({ "trackId": used });
        {
            let mut ctx = CommandCtx { state: &mut state, services: &services, now: 0.0 };
            reg.execute("timeline.removeTrack", Some(&args), &mut ctx);
        }
        assert_eq!(state.app.open_dialog, Some(DialogId::ConfirmRemoveTrack));
        assert_eq!(state.app.remove_track_target.as_deref(), Some(used.as_str()));
        assert!(state.timeline.tracks.iter().any(|t| t.id == used), "noch nicht entfernt");
    }

    /// `timeline.removeTrack` ist in allen Presets gebunden (Verdrahtung sichern).
    #[test]
    fn track_commands_bound_in_all_presets() {
        let reg = build_registry();
        for id in ["timeline.addVideoTrack", "timeline.addAudioTrack", "timeline.removeTrack"] {
            assert!(reg.get(id).is_some(), "Command {id} registriert");
            for preset in crate::core::keyboard::presets() {
                assert!(
                    preset.bindings.iter().any(|b| b.command == id),
                    "Preset {} bindet {id}",
                    preset.id
                );
            }
        }
    }
}
