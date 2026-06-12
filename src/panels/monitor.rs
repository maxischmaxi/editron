//! Quell- und Programmmonitor: schwarze Bühne, Scrubber (h-2, Playhead +
//! In/Out-Bereich), Transportleiste (h-12) mit Timecode, Transport-Buttons,
//! In/Out/Loop und Wiedergabeauflösung. Der Quellmonitor zeigt das geladene
//! Asset (object-contain), der Programmmonitor komponiert alle sichtbaren
//! Video-Layer mit ihren animierten Transformationen (Position, Skalierung,
//! Rotation, Deckkraft) auf ein Programm-Canvas und bietet darüber das
//! Transform-Gizmo für die direkte Manipulation des ausgewählten Clips.
//! Bis die Decode-Engine den ersten Frame liefert, zeigt die Bühne das
//! Asset-Thumbnail (Video) bzw. Bild/Music-Icon.

use crate::core::compose::{self, LayerQuad};
use crate::core::player::clip_texture_key;
use crate::core::timeline::{sequence_end, SEQUENCE_FPS};
use crate::core::timecode::format_timecode;
use crate::core::types::{MediaAsset, MediaKind};
use crate::panels::transform_gizmo::TransformGizmo;
use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::stores::PREVIEW_SCALES;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::select::select;
use crate::ui::{FontKind, Ui};
use raylib::consts::MouseCursor;

const MIN_MARK_GAP: f64 = 0.02;
/// Höhen der Monitor-Chrome-Zonen (auch für die Bühnen-Geometrie im Panel).
const TRANSPORT_H: f32 = 48.0;
const SCRUBBER_H: f32 = 8.0;

/// Aufgelöster Programm-Layer: Texture + Transform-Quad in Bühnenkoordinaten.
pub struct ResolvedLayer {
    pub clip_id: String,
    pub tex_key: String,
    pub quad: LayerQuad,
    pub alpha: u8,
}

/// Bühnen-Inhalt eines Monitors.
enum StageContent<'a> {
    Empty,
    /// Quellmonitor: ein Asset, object-contain.
    Asset(&'a MediaAsset),
    /// Programmmonitor: komponierte Layer auf dem Programm-Canvas.
    Program {
        canvas: Rect,
        layers: &'a [ResolvedLayer],
    },
}

/// Gemeinsame UI beider Monitore.
struct MonitorChrome<'a> {
    monitor: &'static str, // "source" | "program"
    time: f64,
    duration: f64,
    fps: f64,
    playing: bool,
    has_media: bool,
    in_point: Option<f64>,
    out_point: Option<f64>,
    loop_button: Option<bool>,
    empty_hint: &'a str,
    stage: StageContent<'a>,
}

enum MonitorAction {
    None,
    Seek(f64),
    GoToStart,
    GoToEnd,
    StepFrames(f64),
    Toggle,
    MarkIn,
    MarkOut,
    ClearMarks,
    ToggleLoop,
    SetScale(f64),
}

fn render_monitor(
    ui: &mut Ui,
    chrome: &MonitorChrome,
    scrub_active: &mut bool,
    scale: f64,
    rect: Rect,
) -> MonitorAction {
    let mut action = MonitorAction::None;
    ui.fill(rect, theme::SURFACE_0);
    let mut area = rect;

    // ---- Transportleiste unten (h-12) ----
    let transport = area.cut_bottom(TRANSPORT_H);
    // ---- Scrubber (h-2) ----
    let scrubber = area.cut_bottom(SCRUBBER_H);
    // ---- Bühne (flex-1, schwarz) ----
    let stage = area;
    ui.fill(stage, theme::BLACK);

    match &chrome.stage {
        StageContent::Empty => {
            let hint = stage.center_box(288.0, 60.0);
            draw_hint(ui, chrome.empty_hint, hint);
        }
        StageContent::Asset(asset) => match asset.kind {
            MediaKind::Audio => {
                ui.icon("music", stage, 64.0, theme::TEXT_3);
            }
            MediaKind::Image => {
                ui.push_clip(stage);
                ui.draw_texture_contain(&asset.path, stage);
                ui.pop_clip();
            }
            MediaKind::Video => {
                ui.push_clip(stage);
                if ui.textures.get(crate::core::player::SOURCE_KEY).is_some() {
                    // Live-Frame aus der Decode-Engine
                    ui.draw_texture_contain(crate::core::player::SOURCE_KEY, stage);
                } else if let Some(thumb) = &asset.thumbnail_path {
                    // Fallback, bis der erste Frame dekodiert ist
                    ui.draw_texture_contain(thumb, stage);
                }
                ui.pop_clip();
            }
        },
        StageContent::Program { canvas, layers } => {
            // Layer von unten nach oben auf das Canvas komponieren; alles
            // außerhalb des Programm-Frames wird beschnitten (wie im Export).
            ui.push_clip(*canvas);
            for layer in layers.iter() {
                let q = &layer.quad;
                ui.draw_texture_quad(
                    &layer.tex_key,
                    q.cx as f32,
                    q.cy as f32,
                    q.w as f32,
                    q.h as f32,
                    q.rot_deg as f32,
                    layer.alpha,
                );
            }
            ui.pop_clip();
        }
    }

    // ---- Scrubber ----
    ui.fill(scrubber, theme::SURFACE_2);
    if chrome.duration > 0.0 {
        let in_pct = (chrome.in_point.unwrap_or(0.0).clamp(0.0, chrome.duration)
            / chrome.duration) as f32;
        let out_pct = (chrome
            .out_point
            .unwrap_or(chrome.duration)
            .clamp(0.0, chrome.duration)
            / chrome.duration) as f32;
        let show_in_out = (chrome.in_point.is_some() || chrome.out_point.is_some())
            && out_pct > in_pct;
        if show_in_out {
            ui.fill(
                Rect::new(
                    scrubber.x + in_pct * scrubber.w,
                    scrubber.y,
                    (out_pct - in_pct) * scrubber.w,
                    scrubber.h,
                ),
                theme::with_alpha(theme::ACCENT, 77),
            );
        }
        let ph = (chrome.time / chrome.duration).min(1.0) as f32;
        ui.fill(
            Rect::new(scrubber.x + ph * scrubber.w, scrubber.y, 1.0, scrubber.h),
            theme::ACCENT,
        );

        // Interaktion: klicken/ziehen = seek
        let id = ui.id((chrome.monitor, "scrubber"));
        let it = ui.interact(id, scrubber);
        if it.hovered || *scrub_active {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_RESIZE_EW);
        }
        if it.hovered && ui.input.left_pressed {
            *scrub_active = true;
        }
        if *scrub_active {
            if ui.input.left_down {
                let frac = ((ui.input.mouse.x - scrubber.x) / scrubber.w).clamp(0.0, 1.0);
                action = MonitorAction::Seek(frac as f64 * chrome.duration);
            } else {
                *scrub_active = false;
            }
        }
    }

    // ---- Transportleiste ----
    ui.fill(transport, theme::SURFACE_1);
    ui.hline(transport.x, transport.y, transport.w, theme::LINE);
    let inner = transport.inset_xy(12.0, 0.0);

    // Timecode links (w-24, mono text-xs)
    let tc = format_timecode(chrome.time, chrome.fps);
    ui.text_left(
        &tc,
        Rect::new(inner.x, inner.y, 96.0, inner.h),
        theme::TEXT_1,
        FontKind::Mono12,
    );

    // Rechts: Auflösungs-Select + Gesamtdauer (w-40)
    let right = Rect::new(inner.right() - 160.0, inner.y, 160.0, inner.h);
    let dur_label = format_timecode(chrome.duration, chrome.fps);
    let dur_w = ui.font(FontKind::Mono12).width(&dur_label);
    ui.text_left(
        &dur_label,
        Rect::new(right.right() - dur_w, right.y, dur_w, right.h),
        theme::TEXT_3,
        FontKind::Mono12,
    );
    let scale_idx = PREVIEW_SCALES
        .iter()
        .position(|(v, _)| (*v - scale).abs() < 0.001)
        .unwrap_or(0);
    let labels: Vec<&str> = PREVIEW_SCALES.iter().map(|(_, l)| *l).collect();
    let select_rect = Rect::new(
        right.right() - dur_w - 8.0 - 56.0,
        right.y + (right.h - 24.0) / 2.0,
        56.0,
        24.0,
    );
    if let Some(new_idx) = select(ui, (chrome.monitor, "scale"), select_rect, &labels, scale_idx)
    {
        action = MonitorAction::SetScale(PREVIEW_SCALES[new_idx].0);
    }

    // Mitte: Transport-Buttons (zentriert, size-7, gap-1)
    let has = chrome.has_media;
    let has_marks = chrome.in_point.is_some() || chrome.out_point.is_some();
    struct Btn {
        icon: &'static str,
        tip: &'static str,
        enabled: bool,
        active: bool,
        action_id: u8,
    }
    let mut buttons: Vec<Btn> = vec![
        Btn { icon: "skip-back", tip: "Zum Anfang", enabled: has, active: false, action_id: 1 },
        Btn { icon: "step-back", tip: "1 Frame zurück", enabled: has, active: false, action_id: 2 },
        Btn {
            icon: if chrome.playing { "pause" } else { "play" },
            tip: if chrome.playing { "Pause" } else { "Wiedergabe" },
            enabled: has,
            active: false,
            action_id: 3,
        },
        Btn { icon: "step-forward", tip: "1 Frame vor", enabled: has, active: false, action_id: 4 },
        Btn { icon: "skip-forward", tip: "Zum Ende", enabled: has, active: false, action_id: 5 },
        Btn { icon: "", tip: "", enabled: false, active: false, action_id: 0 }, // Divider
        Btn {
            icon: "arrow-right-from-line",
            tip: if chrome.monitor == "program" { "In-Punkt setzen (Loop-Anfang)" } else { "In-Punkt setzen" },
            enabled: has,
            active: false,
            action_id: 6,
        },
        Btn {
            icon: "arrow-right-to-line",
            tip: if chrome.monitor == "program" { "Out-Punkt setzen (Loop-Ende)" } else { "Out-Punkt setzen" },
            enabled: has,
            active: false,
            action_id: 7,
        },
        Btn {
            icon: "x",
            tip: if chrome.monitor == "program" { "Loop-Bereich entfernen" } else { "In/Out entfernen" },
            enabled: has_marks,
            active: false,
            action_id: 8,
        },
    ];
    if let Some(loop_on) = chrome.loop_button {
        buttons.push(Btn {
            icon: "repeat",
            tip: "Loop-Wiedergabe (zwischen In/Out)",
            enabled: has,
            active: loop_on,
            action_id: 9,
        });
    }

    let total_w: f32 = buttons
        .iter()
        .map(|b| if b.action_id == 0 { 10.0 } else { 32.0 })
        .sum::<f32>()
        - 4.0;
    let mut bx = inner.x + (inner.w - total_w) / 2.0;
    for b in &buttons {
        if b.action_id == 0 {
            // Divider mx-1 h-4 w-px
            ui.fill(
                Rect::new(bx + 4.0, inner.y + (inner.h - 16.0) / 2.0, 1.0, 16.0),
                theme::LINE,
            );
            bx += 10.0;
            continue;
        }
        let btn = Rect::new(bx, inner.y + (inner.h - 28.0) / 2.0, 28.0, 28.0);
        let id = ui.id((chrome.monitor, "transport", b.action_id));
        let alpha = if b.enabled { 255 } else { 102 };
        let it = if b.enabled {
            ui.interact(id, btn)
        } else {
            Default::default()
        };
        if b.active {
            ui.fill_rounded(btn, theme::RADIUS_SM, theme::SURFACE_3);
        } else if it.hovered {
            ui.fill_rounded(btn, theme::RADIUS_SM, theme::SURFACE_3);
        }
        let fg = if b.active {
            theme::ACCENT
        } else if it.hovered {
            theme::TEXT_1
        } else {
            theme::TEXT_2
        };
        ui.icon(b.icon, btn, 16.0, theme::with_alpha(fg, alpha));
        if it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            ui.tooltip(id, btn, b.tip);
        }
        if it.clicked {
            action = match b.action_id {
                1 => MonitorAction::GoToStart,
                2 => MonitorAction::StepFrames(-1.0),
                3 => MonitorAction::Toggle,
                4 => MonitorAction::StepFrames(1.0),
                5 => MonitorAction::GoToEnd,
                6 => MonitorAction::MarkIn,
                7 => MonitorAction::MarkOut,
                8 => MonitorAction::ClearMarks,
                9 => MonitorAction::ToggleLoop,
                _ => MonitorAction::None,
            };
        }
        bx += 32.0;
    }

    action
}

/// Mehrzeiliger Hinweistext (max-w-72, zentriert, text-xs leading-5 text-3).
fn draw_hint(ui: &mut Ui, text: &str, rect: Rect) {
    let font = ui.font(FontKind::Sans12);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if font.width(&candidate) > rect.w && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current = word.to_string();
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    let line_h = 20.0;
    let total = lines.len() as f32 * line_h;
    let mut y = rect.y + (rect.h - total) / 2.0;
    for line in lines {
        ui.text_centered(
            &line,
            Rect::new(rect.x, y, rect.w, line_h),
            theme::TEXT_3,
            FontKind::Sans12,
        );
        y += line_h;
    }
}

// ----------------------------------------------------------- Quellmonitor

#[derive(Default)]
pub struct SourceMonitorPanel {
    scrub_active: bool,
}

impl Panel for SourceMonitorPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, _services: &Services, rect: Rect) {
        let asset = app
            .playback
            .source_asset_id
            .as_ref()
            .and_then(|id| app.media.asset(id))
            .cloned();
        let duration = asset.as_ref().map(|a| a.info.duration_sec).unwrap_or(0.0);
        let fps = asset
            .as_ref()
            .and_then(|a| a.info.video.first())
            .map(|v| v.fps)
            .filter(|f| *f > 0.0)
            .unwrap_or(25.0);
        let has_media = asset.as_ref().is_some_and(|a| a.kind != MediaKind::Image);

        let chrome = MonitorChrome {
            monitor: "source",
            time: app.playback.source.position,
            duration,
            fps,
            playing: app.playback.source.playing,
            has_media,
            in_point: app.playback.source.in_mark,
            out_point: app.playback.source.out_mark,
            loop_button: Some(app.playback.source.looping),
            empty_hint: "Doppelklick im Medien-Browser lädt einen Clip in den Quellmonitor.",
            stage: match &asset {
                Some(a) => StageContent::Asset(a),
                None => StageContent::Empty,
            },
        };
        let scale = app.monitor.source_scale;
        let action = render_monitor(ui, &chrome, &mut self.scrub_active, scale, rect);

        let s = &mut app.playback.source;
        match action {
            MonitorAction::None => {}
            MonitorAction::Seek(t) => s.position = t.clamp(0.0, duration),
            MonitorAction::GoToStart => s.position = 0.0,
            MonitorAction::GoToEnd => s.position = duration,
            MonitorAction::StepFrames(frames) => {
                s.playing = false;
                s.position = (s.position + frames / fps).clamp(0.0, duration);
            }
            MonitorAction::Toggle => {
                if !s.playing && s.position >= duration && duration > 0.0 {
                    s.position = 0.0;
                }
                s.playing = !s.playing;
                if s.playing && (s.rate == 0.0 || s.rate.abs() > 1.0) {
                    s.rate = 1.0;
                }
            }
            MonitorAction::MarkIn => {
                let t = s.position;
                s.in_mark = Some(t);
                if s.out_mark.is_some_and(|o| o <= t + MIN_MARK_GAP) {
                    s.out_mark = None;
                }
            }
            MonitorAction::MarkOut => {
                let t = s.position;
                s.out_mark = Some(t);
                if s.in_mark.is_some_and(|i| i >= t - MIN_MARK_GAP) {
                    s.in_mark = None;
                }
            }
            MonitorAction::ClearMarks => {
                s.in_mark = None;
                s.out_mark = None;
            }
            MonitorAction::ToggleLoop => s.looping = !s.looping,
            MonitorAction::SetScale(v) => app.monitor.source_scale = v,
        }

        // Fokus-Tracking für die Wiedergabe-Command-Weiche
        if ui.mouse_in(rect) && (ui.input.left_pressed || ui.input.right_pressed) {
            app.app.focused_panel = "source".into();
        }
    }
}

// --------------------------------------------------------- Programmmonitor

#[derive(Default)]
pub struct ProgramMonitorPanel {
    scrub_active: bool,
    gizmo: TransformGizmo,
}

/// Sichtbare Layer am Playhead in Zeichenreihenfolge auflösen: Texture-Key
/// (Bild = Datei, Video = Live-Frame, Fallback Thumbnail), Transform-Quad in
/// Bühnenkoordinaten und Deckkraft. Layer ohne geladene Texture liefern noch
/// kein Quad (Anforderung läuft über den TextureCache).
fn resolve_program_layers(
    ui: &mut Ui,
    app: &AppState,
    canvas: Rect,
    t: f64,
) -> Vec<ResolvedLayer> {
    compose::visible_video_clips(&app.timeline, t)
        .into_iter()
        .filter_map(|clip| {
            let asset = app.media.asset(&clip.asset_id)?;
            if asset.offline {
                return None;
            }
            let fx = compose::eval_fx(&clip.fx, compose::clip_media_time(clip, t));
            if fx.opacity <= 0.0 {
                return None;
            }
            let mut keys: Vec<String> = Vec::new();
            match asset.kind {
                MediaKind::Image => keys.push(asset.path.clone()),
                MediaKind::Video => {
                    keys.push(clip_texture_key(&clip.id));
                    if let Some(thumb) = &asset.thumbnail_path {
                        keys.push(thumb.clone());
                    }
                }
                MediaKind::Audio => return None,
            }
            let (tex_key, (nw, nh)) = keys
                .iter()
                .find_map(|k| ui.texture_size(k).map(|s| (k.clone(), s)))?;
            let q = compose::layer_quad(canvas.w as f64, canvas.h as f64, nw as f64, nh as f64, &fx);
            Some(ResolvedLayer {
                clip_id: clip.id.clone(),
                tex_key,
                quad: LayerQuad {
                    cx: q.cx + canvas.x as f64,
                    cy: q.cy + canvas.y as f64,
                    ..q
                },
                alpha: (fx.opacity * 255.0).round().clamp(0.0, 255.0) as u8,
            })
        })
        .collect()
}

impl Panel for ProgramMonitorPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, _services: &Services, rect: Rect) {
        let duration = sequence_end(&app.timeline.clips);
        let has_clips = !app.timeline.clips.is_empty();
        let t = app.timeline.playhead_sec;

        // Bühnen-Geometrie (identisch zu render_monitor) + Programm-Canvas:
        // Seitenverhältnis aus der vorgeschlagenen Sequenzauflösung.
        let mut stage = rect;
        let _ = stage.cut_bottom(TRANSPORT_H);
        let _ = stage.cut_bottom(SCRUBBER_H);
        let (aw, ah) = crate::core::export::suggested_resolution(&app.timeline, &app.media);
        let canvas = stage.fit_contain(aw as f32, ah as f32);

        let layers = resolve_program_layers(ui, app, canvas, t);

        let chrome = MonitorChrome {
            monitor: "program",
            time: app.timeline.playhead_sec,
            duration,
            fps: SEQUENCE_FPS,
            playing: app.playback.program_playing,
            has_media: has_clips,
            in_point: app.timeline.in_point,
            out_point: app.timeline.out_point,
            loop_button: None,
            empty_hint:
                "Keine Clips in der Sequenz — Medien aus dem Medien-Browser in die Timeline ziehen.",
            stage: if has_clips {
                StageContent::Program {
                    canvas,
                    layers: &layers,
                }
            } else {
                StageContent::Empty
            },
        };
        let scale = app.monitor.program_scale;
        let action = render_monitor(ui, &chrome, &mut self.scrub_active, scale, rect);

        // Direkte Manipulation des ausgewählten Clips (über den Layern).
        self.gizmo.update(ui, app, stage, canvas, &layers);

        match action {
            MonitorAction::None => {}
            MonitorAction::Seek(t) => app.timeline.set_playhead(t),
            MonitorAction::GoToStart => app.timeline.go_to_start(),
            MonitorAction::GoToEnd => app.timeline.go_to_end(),
            MonitorAction::StepFrames(frames) => {
                app.playback.program_playing = false;
                app.timeline.step_playhead_frames(frames);
            }
            MonitorAction::Toggle => {
                if !app.playback.program_playing
                    && duration > 0.0
                    && app.timeline.playhead_sec >= duration
                {
                    app.timeline.playhead_sec = 0.0;
                }
                app.playback.program_playing = !app.playback.program_playing;
                if app.playback.program_playing
                    && (app.playback.program_rate == 0.0
                        || app.playback.program_rate.abs() > 1.0)
                {
                    app.playback.program_rate = 1.0;
                }
            }
            MonitorAction::MarkIn => {
                let t = app.timeline.playhead_sec;
                app.timeline.set_in_point(Some(t));
            }
            MonitorAction::MarkOut => {
                let t = app.timeline.playhead_sec;
                app.timeline.set_out_point(Some(t));
            }
            MonitorAction::ClearMarks => app.timeline.clear_in_out(),
            MonitorAction::ToggleLoop => {}
            MonitorAction::SetScale(v) => app.monitor.program_scale = v,
        }

        if ui.mouse_in(rect) && (ui.input.left_pressed || ui.input.right_pressed) {
            app.app.focused_panel = "program".into();
        }
    }
}
