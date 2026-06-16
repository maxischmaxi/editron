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
use crate::core::effects;
use crate::core::player::clip_texture_key;
use crate::ui::fx_shader::{fx_output_key, EffectJob};
use crate::core::sequence::SequenceSettings;
use crate::core::timeline::sequence_end;
use crate::core::timecode::{format_sequence_timecode, format_timecode};
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
#[derive(Clone)]
pub struct ResolvedLayer {
    pub clip_id: String,
    pub tex_key: String,
    pub quad: LayerQuad,
    pub alpha: u8,
    /// Farbkorrektur des Clips (Identität = ungegradet).
    pub grade: crate::core::grade::GradeParams,
    /// Input-/Look-3D-LUT (Pfad + Stärke 0…1) — am Programm-Compositing über
    /// den Grade-Shader gebunden (GPU-Texturen im `lut_textures`-Cache).
    pub input_lut: Option<(String, f32)>,
    pub look_lut: Option<(String, f32)>,
    /// Sichtbarer Canvas-Ausschnitt (Wipe-Übergang); None = voll sichtbar.
    pub mask: Option<Rect>,
    /// Titel-Layer: vertikaler Raster-Erweiterungsfaktor (Abspann-Rolle);
    /// 1 bei Video/Bildern. Das Quad ist bereits um den Faktor gestreckt.
    pub extend_k: f64,
    /// Titel-Clip (Doppelklick öffnet die Textbearbeitung im Monitor).
    pub is_title: bool,
}

/// Ein Eintrag der Programm-Zeichenliste: Clip-Layer oder Farbfläche (Dip).
pub enum StageLayer {
    Clip(ResolvedLayer),
    Solid { white: bool, alpha: u8 },
}

/// Bühnen-Inhalt eines Monitors.
enum StageContent<'a> {
    Empty,
    /// Quellmonitor: ein Asset, object-contain.
    Asset(&'a MediaAsset),
    /// Programmmonitor: komponierte Layer auf dem Programm-Canvas.
    Program {
        canvas: Rect,
        layers: &'a [StageLayer],
    },
}

/// Gemeinsame UI beider Monitore.
struct MonitorChrome<'a> {
    monitor: &'static str, // "source" | "program"
    time: f64,
    duration: f64,
    fps: f64,
    /// Programmmonitor: Timecode folgt den Sequenz-Einstellungen
    /// (exakte NTSC-Frame-Zählung, Drop-Frame-Notation). None = Quelle.
    seq: Option<SequenceSettings>,
    playing: bool,
    has_media: bool,
    in_point: Option<f64>,
    out_point: Option<f64>,
    loop_button: Option<bool>,
    empty_hint: &'a str,
    stage: StageContent<'a>,
    /// Marker auf dem Scrubber (Quellzeit/Sequenzzeit, Farbe).
    markers: Vec<(f64, crate::core::marker::MarkerColor)>,
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
    /// Quellmonitor: Three-Point-Insert (Ripple) in die Timeline.
    Insert,
    /// Quellmonitor: Three-Point-Overwrite in die Timeline.
    Overwrite,
    /// Programmmonitor: aktuellen Frame als Bild exportieren (Kamera-Icon).
    ExportFrame,
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
                match layer {
                    StageLayer::Clip(layer) => {
                        // Wipe-Maske: harter Frame-Ausschnitt (Scissor) —
                        // dieselbe Rechteck-Mathematik wie der CPU-Export.
                        if let Some(mask) = layer.mask {
                            ui.push_clip(mask);
                        }
                        let q = &layer.quad;
                        ui.draw_texture_quad_graded(
                            &layer.tex_key,
                            q.cx as f32,
                            q.cy as f32,
                            q.w as f32,
                            q.h as f32,
                            q.rot_deg as f32,
                            layer.alpha,
                            &layer.grade,
                            crate::ui::GradeLutRefs {
                                input: layer.input_lut.as_ref().map(|(p, s)| (p.as_str(), *s)),
                                look: layer.look_lut.as_ref().map(|(p, s)| (p.as_str(), *s)),
                            },
                        );
                        if layer.mask.is_some() {
                            ui.pop_clip();
                        }
                    }
                    StageLayer::Solid { white, alpha } => {
                        let base = if *white { theme::WHITE } else { theme::BLACK };
                        ui.fill(*canvas, theme::with_alpha(base, *alpha));
                    }
                }
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
        // Marker-Ticks (vor dem Playhead, damit dieser sichtbar bleibt).
        for (t, color) in &chrome.markers {
            if *t < 0.0 || *t > chrome.duration {
                continue;
            }
            let mx = scrubber.x + (*t / chrome.duration) as f32 * scrubber.w;
            let (r, g, b) = color.rgb();
            ui.fill(
                Rect::new(mx - 1.0, scrubber.y, 2.0, scrubber.h),
                raylib::prelude::Color::new(r, g, b, 255),
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
    let tc = match &chrome.seq {
        Some(seq) => format_sequence_timecode(chrome.time, seq),
        None => format_timecode(chrome.time, chrome.fps),
    };
    ui.text_left(
        &tc,
        Rect::new(inner.x, inner.y, 96.0, inner.h),
        theme::TEXT_1,
        FontKind::Mono12,
    );

    // Rechts: Auflösungs-Select + Gesamtdauer (w-40)
    let right = Rect::new(inner.right() - 160.0, inner.y, 160.0, inner.h);
    let dur_label = match &chrome.seq {
        Some(seq) => format_sequence_timecode(chrome.duration, seq),
        None => format_timecode(chrome.duration, chrome.fps),
    };
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
            tip: if chrome.monitor == "program" {
                "Loop-Wiedergabe (In/Out oder ganze Sequenz)"
            } else {
                "Loop-Wiedergabe (In/Out oder ganzer Clip)"
            },
            enabled: has,
            active: loop_on,
            action_id: 9,
        });
    }
    // Programmmonitor: aktuellen Frame als Bild exportieren (wie Premieres
    // Kamera-Icon — PNG/JPEG/TIFF des komponierten Frames am Playhead).
    if chrome.monitor == "program" {
        buttons.push(Btn { icon: "", tip: "", enabled: false, active: false, action_id: 0 });
        buttons.push(Btn {
            icon: "image",
            tip: "Frame exportieren (PNG/JPEG/TIFF)",
            enabled: has,
            active: false,
            action_id: 12,
        });
    }
    // Quellmonitor: Three-Point-Edit-Buttons (Insert/Overwrite in die Timeline).
    if chrome.monitor == "source" {
        let has_source = matches!(chrome.stage, StageContent::Asset(_));
        buttons.push(Btn { icon: "", tip: "", enabled: false, active: false, action_id: 0 });
        buttons.push(Btn {
            icon: "chevrons-left-right",
            tip: "Einfügen (Insert) — Komma",
            enabled: has_source,
            active: false,
            action_id: 10,
        });
        buttons.push(Btn {
            icon: "stretch-horizontal",
            tip: "Überschreiben (Overwrite) — Punkt",
            enabled: has_source,
            active: false,
            action_id: 11,
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
                10 => MonitorAction::Insert,
                11 => MonitorAction::Overwrite,
                12 => MonitorAction::ExportFrame,
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
            seq: None,
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
            markers: asset
                .as_ref()
                .map(|a| a.markers.iter().map(|m| (m.time, m.color)).collect())
                .unwrap_or_default(),
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
            MonitorAction::Insert => ui.run_command("timeline.insert"),
            MonitorAction::Overwrite => ui.run_command("timeline.overwrite"),
            MonitorAction::ExportFrame => {} // nur Programmmonitor
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
    title_editor: crate::panels::title_editor::TitleTextEditor,
    /// Zuletzt gesehene Live-Canvas-Auflösung + Zeitpunkt ihrer letzten Änderung
    /// — Debounce für die an die Titel-Engine gemeldete `program_canvas`: Während
    /// eines Fenster-/Panel-Resizes ändert sich die Pixelgröße pro Frame, was
    /// sonst jeden sichtbaren Titel/Untertitel pro Frame neu rastern würde.
    canvas_target: (u32, u32),
    canvas_target_since: f64,
}

/// Sichere Ränder über das Programm-Canvas zeichnen: Action-Safe (90 %)
/// und Title-Safe (80 %) — EBU/SMPTE-Konvention.
fn draw_safe_margins(ui: &mut Ui, canvas: Rect) {
    let line = theme::with_alpha(theme::WHITE, 90);
    for frac in [0.9f32, 0.8] {
        let w = canvas.w * frac;
        let h = canvas.h * frac;
        let r = Rect::new(
            canvas.x + (canvas.w - w) / 2.0,
            canvas.y + (canvas.h - h) / 2.0,
            w,
            h,
        );
        ui.stroke(r, 1.0, line);
    }
    // Zentrumskreuz
    let (cx, cy) = (canvas.x + canvas.w / 2.0, canvas.y + canvas.h / 2.0);
    ui.fill(Rect::new(cx - 8.0, cy, 16.0, 1.0), line);
    ui.fill(Rect::new(cx, cy - 8.0, 1.0, 16.0), line);
}

/// Sichtbare Layer am Playhead in Zeichenreihenfolge auflösen: Texture-Key
/// (Bild = Datei, Video = Live-Frame, Fallback Thumbnail), Transform-Quad in
/// Bühnenkoordinaten und Deckkraft. Layer ohne geladene Texture liefern noch
/// kein Quad (Anforderung läuft über den TextureCache). Clips mit aktiven
/// Effekten melden einen Effekt-Job an und zeichnen das `fx://`-Ergebnis,
/// sobald es vorliegt (bis dahin die Roh-Texture).
fn resolve_program_layers(
    ui: &mut Ui,
    app: &AppState,
    canvas: Rect,
    t: f64,
) -> Vec<StageLayer> {
    compose::visible_program_layers(&app.timeline, t)
        .into_iter()
        .filter_map(|layer| {
            let (clip, t_fx) = match layer {
                compose::ProgramLayer::Clip { clip, t_fx } => (clip, t_fx),
                compose::ProgramLayer::Solid { white, alpha } => {
                    return Some(StageLayer::Solid {
                        white,
                        alpha: (alpha * 255.0).round().clamp(0.0, 255.0) as u8,
                    });
                }
            };
            let media_t = compose::clip_media_time(clip, t);
            let fx = compose::eval_fx(&clip.fx, media_t);
            let opacity = fx.opacity * t_fx.opacity;
            if opacity <= 0.0 {
                return None;
            }
            // Text-Generatoren (Titel + Untertitel): Textur kommt aus der
            // Titel-Engine (CPU-gerastert in Canvas-Auflösung) statt aus
            // einem Decoder.
            let mut extend_k = 1.0f64;
            let (mut tex_key, (nw, nh)) = if clip.is_generator() {
                let key = crate::core::title_engine::title_texture_key(&clip.id);
                let size = ui.texture_size(&key)?;
                // Erweiterungsfaktor (Abspann-Rolle) aus dem Seitenverhältnis
                // der Textur gegen das Canvas ableiten (Breite = 1 Frame).
                extend_k = ((size.1 as f64 * canvas.w as f64)
                    / (size.0 as f64 * canvas.h as f64))
                    .round()
                    .max(1.0);
                // Quad-Basis ist das volle Frame — NICHT die Texturmaße
                // (Überabtastung/Erweiterung stecken in der Textur).
                (key, (canvas.w, canvas.h))
            } else if let Some(inner_id) = &clip.nest_seq {
                // Verschachtelte Sequenz: der Player komponiert sie in eine
                // Clip-Textur (innere Auflösung); natürliche Größe = innere
                // Sequenzgröße (contain-fit ins äußere Frame). Grade/Effekte
                // des Nest-Clips wendet die GPU-Pipeline unten an.
                let key = clip_texture_key(&clip.id);
                ui.texture_size(&key)?; // erst zeichnen, wenn der Player geliefert hat
                let (iw, ih) = app
                    .timeline
                    .timeline_of(inner_id)
                    .map(|t| (t.settings.width as f32, t.settings.height as f32))
                    .unwrap_or((canvas.w, canvas.h));
                (key, (iw, ih))
            } else if let Some(mc) = &clip.multicam {
                // Multicam: der Player liefert den aktiven Winkel als Clip-Textur;
                // natürliche Größe = Winkel-Auflösung (contain-fit ins Frame).
                let key = clip_texture_key(&clip.id);
                ui.texture_size(&key)?;
                let (aw, ah) = app
                    .timeline
                    .timeline_of(&mc.source)
                    .and_then(|t| t.multicam.as_ref())
                    .and_then(|s| s.angle(mc.angle))
                    .map(|a| (a.width.max(1) as f32, a.height.max(1) as f32))
                    .unwrap_or((canvas.w, canvas.h));
                (key, (aw, ah))
            } else {
                let asset = app.media.asset(&clip.asset_id)?;
                if asset.offline {
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
                keys.iter()
                    .find_map(|k| ui.texture_size(k).map(|s| (k.clone(), s)))?
            };
            // Effekt-Kette: Job für den nächsten Frame anmelden; Ergebnis
            // zeichnen, sobald der Renderer es liefert.
            if effects::has_active_video_effects(&clip.effects) {
                let resolved = effects::resolve_video_effects(&clip.effects, media_t);
                if !resolved.is_empty() {
                    let out_key = fx_output_key(&clip.id);
                    ui.effect_requests.push(EffectJob {
                        out_key: out_key.clone(),
                        source_key: tex_key.clone(),
                        effects: resolved,
                    });
                    if ui.fx_output_size(&out_key).is_some() {
                        tex_key = out_key;
                    }
                }
            }
            let mut q =
                compose::layer_quad(canvas.w as f64, canvas.h as f64, nw as f64, nh as f64, &fx);
            // Erweiterter Titel-Raster (Abspann): Quad vertikal strecken —
            // identisch zum CPU-Export-Pfad.
            q.h *= extend_k;
            // Übergangs-Auswirkung (Schieben/Zoom) — identische Mathematik
            // wie im Export, bezogen auf die Canvas-Maße.
            compose::apply_transition_to_quad(&mut q, &t_fx, canvas.w as f64, canvas.h as f64);
            // Wipe-Maske in Canvas-Pixel umrechnen (gleiche Rundung wie CPU).
            let mask = t_fx.mask.map(|m| {
                let (x0, y0, x1, y1) =
                    compose::mask_to_pixels(&m, canvas.w as usize, canvas.h as usize);
                Rect::new(
                    canvas.x + x0 as f32,
                    canvas.y + y0 as f32,
                    (x1.saturating_sub(x0)) as f32,
                    (y1.saturating_sub(y0)) as f32,
                )
            });
            Some(StageLayer::Clip(ResolvedLayer {
                clip_id: clip.id.clone(),
                tex_key,
                quad: LayerQuad {
                    cx: q.cx + canvas.x as f64,
                    cy: q.cy + canvas.y as f64,
                    ..q
                },
                alpha: (opacity * 255.0).round().clamp(0.0, 255.0) as u8,
                grade: crate::core::grade::precompute(&clip.grade),
                input_lut: lut_ref(&clip.grade.input_lut),
                look_lut: lut_ref(&clip.grade.look_lut),
                mask,
                extend_k,
                is_title: clip.is_title(),
            }))
        })
        .collect()
}

/// LUT-Slot eines Grades → (Pfad, Stärke 0…1) für den Programmmonitor; None
/// wenn leer/inaktiv.
fn lut_ref(slot: &Option<crate::core::grade::LutSlot>) -> Option<(String, f32)> {
    slot.as_ref()
        .filter(|s| s.is_active())
        .map(|s| (s.path.clone(), s.strength01()))
}

/// Einen Rahmen der Stärke `th` um `r` zeichnen (vier dünne Flächen).
fn draw_border(ui: &mut Ui, r: Rect, color: raylib::color::Color, th: f32) {
    ui.fill(Rect::new(r.x, r.y, r.w, th), color);
    ui.fill(Rect::new(r.x, r.bottom() - th, r.w, th), color);
    ui.fill(Rect::new(r.x, r.y, th, r.h), color);
    ui.fill(Rect::new(r.right() - th, r.y, th, r.h), color);
}

/// Multicam-Raster zeichnen: alle Winkel der am Playhead aktiven Multicam-Quelle
/// synchron als Kacheln (2×2, 3×3 …), der aktive Winkel mit Akzent-Rahmen.
/// Klick auf eine Kachel schaltet den Winkel um (bei Wiedergabe Live-Schnitt).
/// Die Decoder je Winkel liefert der Player (siehe `program_video_targets`).
fn draw_multicam_grid(ui: &mut Ui, app: &mut AppState, canvas: Rect, t: f64) {
    use crate::core::player::{active_multicam_clip, mc_angle_id};
    // Hintergrund (überdeckt das Programmbild vollständig).
    ui.fill(canvas, theme::SURFACE_0);
    let info = active_multicam_clip(app)
        .and_then(|c| c.multicam.as_ref().map(|mc| (c.id.clone(), mc.source.clone(), mc.angle)));
    let Some((clip_id, source_id, active_angle)) = info else {
        ui.text_centered(
            "Kein Multicam-Clip am Playhead",
            canvas,
            theme::TEXT_2,
            FontKind::Sans14,
        );
        return;
    };
    let angles: Vec<String> = match app.timeline.multicam_source(&source_id) {
        Some(src) => src.angles.iter().map(|a| a.name.clone()).collect(),
        None => return,
    };
    let n = angles.len();
    if n == 0 {
        return;
    }
    let cols = crate::core::multicam::grid_cols(n);
    let rows = n.div_ceil(cols);
    let gap = 3.0f32;
    let cell_w = ((canvas.w - gap * (cols as f32 + 1.0)) / cols as f32).max(1.0);
    let cell_h = ((canvas.h - gap * (rows as f32 + 1.0)) / rows as f32).max(1.0);
    let white = raylib::color::Color::new(255, 255, 255, 255);
    let strip_bg = raylib::color::Color::new(0, 0, 0, 160);
    let playing = app.playback.program_playing;
    let mut clicked: Option<u32> = None;
    for (i, angle_name) in angles.iter().enumerate() {
        let r = i / cols;
        let c = i % cols;
        let x = canvas.x + gap + c as f32 * (cell_w + gap);
        let y = canvas.y + gap + r as f32 * (cell_h + gap);
        let cell = Rect::new(x, y, cell_w, cell_h);
        ui.fill(cell, theme::SURFACE_1);
        let key = clip_texture_key(&mc_angle_id(&clip_id, i as u32));
        if let Some((tw, th)) = ui.texture_size(&key) {
            let dest = cell.fit_contain(tw, th);
            ui.draw_texture_in(&key, Rect::new(0.0, 0.0, tw, th), dest, white);
        } else {
            ui.text_centered("lädt …", cell, theme::TEXT_3, FontKind::Sans12);
        }
        // Nummer + Winkelname als Streifen unten links.
        let strip = Rect::new(cell.x, cell.bottom() - 20.0, cell.w, 20.0);
        ui.fill(strip, strip_bg);
        let label = Rect::new(strip.x + 6.0, strip.y, strip.w - 12.0, strip.h);
        ui.text_left(
            &format!("{}  {}", i + 1, angle_name),
            label,
            theme::TEXT_1,
            FontKind::Sans12,
        );
        // Rahmen: aktiver Winkel betont (Akzent), sonst dezent.
        if i as u32 == active_angle {
            draw_border(ui, cell, theme::ACCENT, 2.5);
        } else {
            draw_border(ui, cell, theme::LINE, 1.0);
        }
        let it = ui.interact(ui.id(("mc.tile", i)), cell);
        if it.clicked {
            clicked = Some(i as u32);
        }
    }
    if let Some(angle) = clicked {
        if playing {
            app.timeline.multicam_live_cut(t, angle);
        } else {
            app.timeline.set_multicam_angle_undoable(&clip_id, angle);
        }
    }
}

/// Nur die Clip-Layer (Gizmo/Farbpipette arbeiten auf Clips).
fn clip_layers(layers: &[StageLayer]) -> Vec<ResolvedLayer> {
    layers
        .iter()
        .filter_map(|l| match l {
            StageLayer::Clip(c) => Some(c.clone()),
            StageLayer::Solid { .. } => None,
        })
        .collect()
}

/// Aktive Farbpipette: Crosshair + Hinweis über der Bühne; Klick bildet den
/// Punkt invers über das Layer-Quad auf den Decode-MiniFrame des Ziel-Clips
/// ab (Quellfarbe VOR Effekten/Grade — genau richtig fürs Keying) und
/// schreibt R/G/B in die Zielparameter. Esc/Rechtsklick bricht ab.
fn handle_color_pick(
    ui: &mut Ui,
    app: &mut AppState,
    stage: Rect,
    layers: &[ResolvedLayer],
    t: f64,
) {
    let Some(req) = app.app.color_pick.clone() else { return };
    // Abbruch per Esc oder Rechtsklick.
    let esc = ui
        .input
        .keys
        .iter()
        .any(|k| k.key == raylib::consts::KeyboardKey::KEY_ESCAPE);
    if esc || ui.input.right_pressed {
        app.app.color_pick = None;
        return;
    }
    if ui.mouse_in(stage) {
        ui.want_cursor(MouseCursor::MOUSE_CURSOR_CROSSHAIR);
    }
    // Hinweisbanner oben in der Bühne.
    let banner = Rect::new(stage.x + (stage.w - 320.0) / 2.0, stage.y + 8.0, 320.0, 24.0);
    ui.fill_rounded(banner, theme::RADIUS_SM, theme::with_alpha(theme::SURFACE_3, 230));
    ui.text_centered(
        "Farbe aufnehmen: in das Bild klicken (Esc bricht ab)",
        banner,
        theme::TEXT_1,
        FontKind::Sans12,
    );

    if !(ui.input.left_pressed && ui.mouse_in(stage)) {
        return;
    }
    // Ziel-Layer + inverse Quad-Abbildung (wie der CPU-Compositor).
    let picked = layers.iter().find(|l| l.clip_id == req.clip_id).and_then(|layer| {
        let q = &layer.quad;
        let (sin_r, cos_r) = (-q.rot_deg.to_radians()).sin_cos();
        let dx = ui.input.mouse.x as f64 - q.cx;
        let dy = ui.input.mouse.y as f64 - q.cy;
        let lx = dx * cos_r - dy * sin_r;
        let ly = dx * sin_r + dy * cos_r;
        let u = lx / q.w + 0.5;
        let v = ly / q.h + 0.5;
        if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
            return None;
        }
        let mini = app.monitor.preview_frames.get(&req.clip_id)?;
        let px = ((u * mini.w as f64) as usize).min(mini.w - 1);
        let py = ((v * mini.h as f64) as usize).min(mini.h - 1);
        let i = (py * mini.w + px) * 4;
        Some([mini.rgba[i], mini.rgba[i + 1], mini.rgba[i + 2]])
    });
    if let Some(rgb) = picked {
        let media_t = app
            .timeline
            .clip(&req.clip_id)
            .map(|c| compose::clip_media_time(c, t))
            .unwrap_or(0.0);
        app.timeline.begin_fx_edit();
        for (ch, value) in rgb.iter().enumerate() {
            app.timeline.kf_set_value_live(
                &req.clip_id,
                &crate::core::animation::ParamRef::Effect {
                    fx_id: req.fx_id.clone(),
                    index: req.p_idx + ch,
                },
                media_t,
                *value as f64,
            );
        }
    }
    app.app.color_pick = None;
}

impl Panel for ProgramMonitorPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, _services: &Services, rect: Rect) {
        let duration = sequence_end(&app.timeline.clips);
        let has_clips = !app.timeline.clips.is_empty();
        let t = app.timeline.playhead_sec;

        // Bühnen-Geometrie (identisch zu render_monitor) + Programm-Canvas:
        // Seitenverhältnis aus den Sequenz-Einstellungen.
        let mut stage = rect;
        let _ = stage.cut_bottom(TRANSPORT_H);
        let _ = stage.cut_bottom(SCRUBBER_H);
        let seq = app.timeline.settings;
        let canvas = stage.fit_contain(seq.width as f32, seq.height as f32);
        // Canvas-Auflösung an die Titel-Engine melden (Raster in Anzeigegröße),
        // ABER gedebounced: Bei einem Resize ändert sich die Pixelgröße pro Frame;
        // die Titel-Engine würde sonst jeden sichtbaren Titel/Untertitel pro Frame
        // neu rastern + hochladen. Erst übernehmen, wenn die Größe ~120 ms stabil
        // ist (oder beim allerersten Mal). Bis dahin bleibt der alte Raster stehen
        // und wird auf das Live-Canvas skaliert — minimal weicher, dann scharf.
        let live_canvas = (
            canvas.w.round().max(1.0) as u32,
            canvas.h.round().max(1.0) as u32,
        );
        if live_canvas != self.canvas_target {
            self.canvas_target = live_canvas;
            self.canvas_target_since = ui.time;
        }
        let settled = ui.time - self.canvas_target_since > 0.12;
        if app.monitor.program_canvas == (0, 0)
            || (settled && app.monitor.program_canvas != live_canvas)
        {
            app.monitor.program_canvas = live_canvas;
        }

        let layers = resolve_program_layers(ui, app, canvas, t);

        // Wiedergabe aus dem Sequenz-Render-Cache: das fertig gerenderte
        // Programmbild als ein Vollbild-Layer zeichnen (statt N Clip-Layer zu
        // komponieren). Erst zeichnen, wenn die Cache-Textur wirklich anliegt
        // (kein Schwarz-Flackern am Bereichsanfang).
        let cache_layers: Vec<StageLayer> = if app.monitor.program_from_cache
            && ui
                .texture_size(crate::core::player::RENDER_CACHE_KEY)
                .is_some()
        {
            vec![StageLayer::Clip(ResolvedLayer {
                clip_id: String::new(),
                tex_key: crate::core::player::RENDER_CACHE_KEY.to_string(),
                quad: LayerQuad {
                    cx: (canvas.x + canvas.w / 2.0) as f64,
                    cy: (canvas.y + canvas.h / 2.0) as f64,
                    w: canvas.w as f64,
                    h: canvas.h as f64,
                    rot_deg: 0.0,
                },
                alpha: 255,
                grade: crate::core::grade::precompute(&crate::core::grade::ColorGrade::default()),
                input_lut: None,
                look_lut: None,
                mask: None,
                extend_k: 1.0,
                is_title: false,
            })]
        } else {
            Vec::new()
        };
        let stage_layers: &[StageLayer] = if cache_layers.is_empty() {
            &layers
        } else {
            &cache_layers
        };

        let chrome = MonitorChrome {
            monitor: "program",
            time: app.timeline.playhead_sec,
            duration,
            fps: seq.rate.fps(),
            seq: Some(seq),
            playing: app.playback.program_playing,
            has_media: has_clips,
            in_point: app.timeline.in_point,
            out_point: app.timeline.out_point,
            loop_button: Some(app.playback.program_looping),
            empty_hint:
                "Keine Clips in der Sequenz — Medien aus dem Medien-Browser in die Timeline ziehen.",
            stage: if has_clips {
                StageContent::Program {
                    canvas,
                    layers: stage_layers,
                }
            } else {
                StageContent::Empty
            },
            markers: app
                .timeline
                .markers
                .iter()
                .map(|m| (m.time, m.color))
                .collect(),
        };
        let scale = app.monitor.program_scale;
        let action = render_monitor(ui, &chrome, &mut self.scrub_active, scale, rect);

        // Multicam-Raster: alle Winkel synchron über das Programmbild legen.
        let multicam_view = app.monitor.view == crate::stores::MonitorView::Multicam;
        if multicam_view && has_clips {
            ui.push_clip(stage);
            draw_multicam_grid(ui, app, canvas, t);
            ui.pop_clip();
        }

        // Sichere Ränder (Action-/Title-Safe) über dem Programmbild.
        if app.monitor.safe_margins && has_clips && !multicam_view {
            ui.push_clip(stage);
            draw_safe_margins(ui, canvas);
            ui.pop_clip();
        }

        // ---- Farbpipette (Chroma-Key): Klick liest die Quellfarbe ----
        // Im Multicam-Raster ruhen Gizmo/Texteingabe/Farbpipette (das Raster
        // hat eigene Klick-Semantik: Winkel wählen).
        let clip_only = clip_layers(&layers);
        if multicam_view {
            // nichts — das Raster verarbeitet die Klicks selbst.
        } else if app.app.color_pick.is_some() {
            self.title_editor.stop(ui);
            handle_color_pick(ui, app, stage, &clip_only, t);
        } else if self.title_editor.update(ui, app, stage, canvas, &clip_only) {
            // Texteingabe direkt im Monitor — Gizmo pausiert.
        } else {
            // Direkte Manipulation des ausgewählten Clips (über den Layern).
            self.gizmo.update(ui, app, stage, canvas, &clip_only);
        }

        // Safe-Margins-Umschalter in der Transportleiste (links neben dem
        // Timecode-Feld — nur der Programmmonitor hat ihn).
        let transport = Rect::new(rect.x, rect.bottom() - TRANSPORT_H, rect.w, TRANSPORT_H);
        let btn = Rect::new(
            rect.x + 12.0 + 96.0,
            transport.y + (TRANSPORT_H - 28.0) / 2.0,
            28.0,
            28.0,
        );
        let btn_id = ui.id("program.safeMargins");
        let it = ui.interact(btn_id, btn);
        if app.monitor.safe_margins || it.hovered {
            ui.fill_rounded(btn, theme::RADIUS_SM, theme::SURFACE_3);
        }
        ui.icon(
            "focus",
            btn,
            16.0,
            if app.monitor.safe_margins {
                theme::ACCENT
            } else if it.hovered {
                theme::TEXT_1
            } else {
                theme::TEXT_2
            },
        );
        if it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            ui.tooltip(btn_id, btn, "Sichere Ränder (Action-/Title-Safe)");
        }
        if it.clicked {
            app.monitor.safe_margins = !app.monitor.safe_margins;
        }

        // Proxy-Umschalter (Premiere-Pendant): Vorschau aus dem Proxy, der
        // Export bleibt immer im Original. Aktiv = grün; ein kleiner Punkt
        // zeigt verfügbare Proxys / laufende Erstellung an.
        let use_proxy = app.media.use_proxies;
        let proxy_count = app.media.proxy_count();
        let building = !app.media.proxy_jobs.is_empty();
        let pbtn = Rect::new(btn.x + 32.0, btn.y, 28.0, 28.0);
        let pid = ui.id("program.proxyToggle");
        let pit = ui.interact(pid, pbtn);
        if use_proxy || pit.hovered {
            ui.fill_rounded(pbtn, theme::RADIUS_SM, theme::SURFACE_3);
        }
        ui.icon(
            "gauge",
            pbtn,
            16.0,
            if use_proxy {
                theme::SUCCESS
            } else if pit.hovered {
                theme::TEXT_1
            } else {
                theme::TEXT_2
            },
        );
        // Status-Punkt oben rechts: gelb = wird erstellt, grün = vorhanden.
        if building || proxy_count > 0 {
            let dot = Rect::new(pbtn.right() - 8.0, pbtn.y + 4.0, 6.0, 6.0);
            let c = if building { theme::WARNING } else { theme::SUCCESS };
            ui.fill_rounded(dot, 3.0, c);
        }
        if pit.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            let tip = if building {
                "Proxies verwenden — Erstellung läuft".to_string()
            } else if proxy_count > 0 {
                format!("Proxies verwenden ({proxy_count} vorhanden) — Export bleibt Original")
            } else {
                "Proxies verwenden — noch keine Proxys erstellt".to_string()
            };
            ui.tooltip(pid, pbtn, &tip);
        }
        if pit.clicked {
            ui.run_command("proxy.toggle");
        }

        // Performance-Overlay-Umschalter (rechts neben dem Proxy-Badge).
        let fbtn = Rect::new(pbtn.x + 32.0, pbtn.y, 28.0, 28.0);
        let fid = ui.id("program.perfOverlay");
        let fit = ui.interact(fid, fbtn);
        let perf = app.monitor.perf;
        if app.monitor.show_perf_overlay || fit.hovered {
            ui.fill_rounded(fbtn, theme::RADIUS_SM, theme::SURFACE_3);
        }
        ui.icon(
            "activity",
            fbtn,
            16.0,
            if app.monitor.show_perf_overlay {
                theme::ACCENT
            } else if fit.hovered {
                theme::TEXT_1
            } else {
                theme::TEXT_2
            },
        );
        // Roter Punkt bei kürzlich verworfenen Frames (auch ohne Overlay).
        if perf.dropped_recent > 0 {
            let dot = Rect::new(fbtn.right() - 8.0, fbtn.y + 4.0, 6.0, 6.0);
            ui.fill_rounded(dot, 3.0, theme::DANGER);
        }
        if fit.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            ui.tooltip(
                fid,
                fbtn,
                "Performance-Overlay (Decode-/Upload-/Frame-Zeiten, Cache)",
            );
        }
        if fit.clicked {
            ui.run_command("monitor.togglePerfOverlay");
        }

        // Multicam-Monitor-Umschalter (Winkel-Raster): aktiv = Akzent.
        let mc_on = app.monitor.view == crate::stores::MonitorView::Multicam;
        let mbtn = Rect::new(fbtn.x + 32.0, fbtn.y, 28.0, 28.0);
        let mid = ui.id("program.multicamToggle");
        let mit = ui.interact(mid, mbtn);
        if mc_on || mit.hovered {
            ui.fill_rounded(mbtn, theme::RADIUS_SM, theme::SURFACE_3);
        }
        ui.icon(
            "layout-grid",
            mbtn,
            16.0,
            if mc_on {
                theme::ACCENT
            } else if mit.hovered {
                theme::TEXT_1
            } else {
                theme::TEXT_2
            },
        );
        if mit.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            ui.tooltip(
                mid,
                mbtn,
                "Multicam-Monitor (alle Winkel als Raster, Zifferntasten schneiden)",
            );
        }
        if mit.clicked {
            ui.run_command("multicam.toggleMonitor");
        }

        // ---- Dropped-Frame-Indikator (Resolve-artig) oben rechts ----
        if has_clips && perf.dropped_recent > 0 {
            let label = format!("⚠ {} dropped", perf.dropped_recent);
            let w = ui.font(FontKind::Mono12).width(&label) + 16.0;
            let box_ = Rect::new(stage.right() - w - 8.0, stage.y + 8.0, w, 20.0);
            ui.fill_rounded(box_, theme::RADIUS_SM, theme::with_alpha(theme::BLACK, 180));
            ui.text_left(
                &label,
                Rect::new(box_.x + 8.0, box_.y, box_.w - 8.0, box_.h),
                theme::DANGER,
                FontKind::Mono12,
            );
        }

        // ---- Performance-Overlay (Decode-/Upload-/Frame-Zeiten, Cache) ----
        if app.monitor.show_perf_overlay {
            let cached_secs: f64 = app
                .render_cache
                .cached_spans()
                .iter()
                .map(|(a, b)| (b - a) as f64)
                .sum::<f64>()
                / app.timeline.settings.rate.fps().max(1.0);
            let lines = [
                format!("{:.0} fps · {:.1} ms/Frame", perf.fps, perf.frame_ms),
                format!("Decode {:.1} ms · Upload {:.1} ms", perf.decode_ms, perf.upload_ms),
                format!(
                    "Frame-Cache {:.0} MB · {} Frames · {:.0}% hit",
                    perf.cache_used_mb,
                    perf.cache_entries,
                    perf.cache_hit_ratio() * 100.0
                ),
                format!("Verworfen gesamt: {}", perf.dropped_total),
                format!(
                    "Render-Cache: {} Segmente · {:.1} s{}",
                    app.render_cache.segments.len(),
                    cached_secs,
                    if app.render_cache.rendering.is_some() {
                        " · rendert…"
                    } else {
                        ""
                    }
                ),
            ];
            let lh = 16.0;
            let bw = lines
                .iter()
                .map(|l| ui.font(FontKind::Mono11).width(l))
                .fold(0.0_f32, f32::max)
                + 16.0;
            let bh = lines.len() as f32 * lh + 12.0;
            let box_ = Rect::new(stage.x + 8.0, stage.y + 8.0, bw, bh);
            ui.fill_rounded(box_, theme::RADIUS_SM, theme::with_alpha(theme::BLACK, 190));
            for (i, line) in lines.iter().enumerate() {
                ui.text_left(
                    line,
                    Rect::new(box_.x + 8.0, box_.y + 6.0 + i as f32 * lh, bw - 16.0, lh),
                    theme::TEXT_1,
                    FontKind::Mono11,
                );
            }
        }

        match action {
            MonitorAction::None => {}
            MonitorAction::Seek(t) => {
                app.timeline.set_playhead(t);
                app.playback.scrub_active = true; // Audio-Scrubbing am Programm
            }
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
            MonitorAction::ToggleLoop => {
                app.playback.program_looping = !app.playback.program_looping;
            }
            MonitorAction::SetScale(v) => app.monitor.program_scale = v,
            // Insert/Overwrite gibt es nur im Quellmonitor.
            MonitorAction::Insert | MonitorAction::Overwrite => {}
            MonitorAction::ExportFrame => ui.run_command("export.frame"),
        }

        if ui.mouse_in(rect) && (ui.input.left_pressed || ui.input.right_pressed) {
            app.app.focused_panel = "program".into();
        }
    }
}
