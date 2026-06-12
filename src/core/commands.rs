//! Command-Registry + when-Klausel-Evaluator + alle Builtin-Commands:
//! Jede Aktion läuft über die Registry und ist frei mit Shortcuts belegbar.

use crate::core::playback::{self, PlaybackCmd};
use crate::core::timeline::{TrackKind, TrimEdge};
use crate::services::Services;
use crate::state::{set_active_workspace, AppState};
use crate::stores::{tool_command_title, workspace_name, DialogId, TOOLS, WORKSPACE_IDS};
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
        "timelineClipboard" => (!state.timeline.clipboard.is_empty()).into(),
        "timelineCanUndo" => state.timeline.can_undo().into(),
        "timelineCanRedo" => state.timeline.can_redo().into(),
        "timelineInOutSet" => {
            (state.timeline.in_point.is_some() || state.timeline.out_point.is_some()).into()
        }
        "projectDirty" => state.project.dirty.into(),
        "projectHasPath" => state.project.path.is_some().into(),
        "mediaOffline" => (state.media.offline_count() > 0).into(),
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
                // Keyframe-Medienzeit → Sequenzzeit.
                let t_seq = clip.start + (k.t - clip.src_in);
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
        "app.settings",
        "Einstellungen…",
        "Anwendung",
        |ctx, _| status(ctx, "Einstellungen folgen"),
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
        "project.relink",
        "Fehlende Medien neu verknüpfen…",
        "Projekt",
        |ctx, _| ctx.state.app.open_dialog = Some(DialogId::Relink),
    ));

    // ----------------------------------------------------------- Bearbeiten
    commands.push(with_repeat(with_when(
        cmd("edit.undo", "Rückgängig", "Bearbeiten", |ctx, _| {
            ctx.state.timeline.undo()
        }),
        "timelineCanUndo",
    )));
    commands.push(with_repeat(with_when(
        cmd("edit.redo", "Wiederholen", "Bearbeiten", |ctx, _| {
            ctx.state.timeline.redo()
        }),
        "timelineCanRedo",
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
                // Clips in der Timeline, die auf die Assets zeigen, mit entfernen.
                ctx.state.timeline.remove_clips_for_assets(&ids);
                if let Some(src) = &ctx.state.playback.source_asset_id {
                    if ids.contains(src) {
                        ctx.state.playback.source_asset_id = None;
                        ctx.state.playback.source = Default::default();
                    }
                }
                ctx.state.media.remove_assets(&ids);
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
    commands.push(cmd(
        "timeline.addMarker",
        "Marker hinzufügen",
        "Timeline",
        |ctx, _| status(ctx, "Marker folgen in einer späteren Version"),
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
            "Ausgewählte Clips löschen",
            "Timeline",
            |ctx, _| {
                let ids = ctx.state.timeline.selected_clip_ids.clone();
                ctx.state.timeline.delete_clips(&ids, false);
            },
        ),
        "timelineClipSelected",
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
