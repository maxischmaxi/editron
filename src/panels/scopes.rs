//! Scopes-Panel: Waveform (Luma), RGB-Parade, Vektorskop und Histogramm
//! des Programm-Bilds am Playhead sowie das Goniometer (Lissajous) mit
//! Korrelationsmeter des Audio-Mixdowns. Die Bildquelle spiegelt den
//! Programmmonitor: alle sichtbaren Video-Layer werden CPU-seitig in ein
//! kleines Analyse-Canvas komponiert — Live-Frames aus der Decode-Engine
//! (`monitor.preview_frames`), Thumbnails als Fallback (Bilder, noch kein
//! Frame), inklusive Transformationen UND Farbkorrektur (gleiche Mathematik
//! wie Export/Shader). Die Scopes zeigen damit das gegradete Bild. Der
//! Audio-Modus liest den Stereo-Snapshot des Master-Mixdowns
//! (`AudioStore::scope_stereo`/`correlation`, gespeist vom Player).

use crate::core::{compose, effects, grade};
use crate::core::types::MediaKind;
use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::stores::AudioStore;
use crate::theme;
use crate::ui::geom::{v2, Rect};
use crate::ui::widgets::select::select;
use crate::ui::{FontKind, Ui};

/// Analyse-Auflösung des komponierten Programm-Bilds.
const CANVAS_W: usize = 192;
const CANVAS_H: usize = 108;

const MODES: [&str; 5] = ["Waveform", "RGB-Parade", "Vektorskop", "Histogramm", "Goniometer"];
/// Index des Audio-Modus (Goniometer + Korrelationsmeter); die übrigen Modi
/// analysieren das Programmbild.
const AUDIO_MODE: usize = 4;

#[derive(Default)]
pub struct ScopesPanel {
    mode: usize,
    /// Thumbnail-Fallback-Cache: Pfad → RGBA in natürlicher Thumb-Größe.
    thumb_cache: Option<(String, ThumbSample)>,
    /// Panel-eigener `.cube`-LUT-Cache (Scopes laufen jeden Frame, daher nicht
    /// pro Frame neu parsen). Spiegelt den Export-/Programm-LUT-Pfad.
    lut_cache: crate::core::lut::LutCache,
}

struct ThumbSample {
    w: usize,
    h: usize,
    rgba: Vec<u8>,
}

fn load_thumb(path: &str) -> Option<ThumbSample> {
    let image = raylib::core::texture::Image::load_image(path).ok()?;
    let (w, h) = (image.width as usize, image.height as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let colors = image.get_image_data();
    let mut rgba = Vec::with_capacity(w * h * 4);
    for c in colors.iter() {
        rgba.extend_from_slice(&[c.r, c.g, c.b, c.a]);
    }
    Some(ThumbSample { w, h, rgba })
}

impl ScopesPanel {
    /// Programm-Bild am Playhead in ein Analyse-Canvas komponieren
    /// (Spiegel von `resolve_program_layers` + Export-Compositing).
    fn compose_program(&mut self, app: &AppState) -> Option<Vec<u8>> {
        let t = app.timeline.playhead_sec;
        let clips = compose::visible_video_clips(&app.timeline, t);
        if clips.is_empty() {
            return None;
        }

        // Opakes schwarzes f32-Canvas (Analyse läuft in voller Präzision wie
        // der Export; am Ende für die Scope-Darstellung nach u8 quantisiert).
        let mut canvas = vec![0f32; CANVAS_W * CANVAS_H * 4];
        for px in canvas.chunks_exact_mut(4) {
            px[3] = 1.0;
        }

        struct Layer {
            data: Vec<f32>,
            w: usize,
            h: usize,
            quad: compose::LayerQuad,
            opacity: f64,
        }
        let mut layers: Vec<Layer> = Vec::new();
        let mut any = false;

        for clip in clips {
            let media_t = compose::clip_media_time(clip, t);
            let fx = compose::eval_fx(&clip.fx, media_t);
            if fx.opacity <= 0.0 {
                continue;
            }
            // Bildquelle: Text-Raster (Mini der Titel-Engine — Titel und
            // Untertitel), Live-Frame, sonst Thumbnail.
            let mut title_extend = 1.0f64;
            let (w, h, data_u8) = if clip.is_generator() {
                let Some(mini) = app.monitor.preview_frames.get(&clip.id) else { continue };
                // Vertikale Raster-Erweiterung (Abspann) aus dem Mini ableiten.
                title_extend = ((mini.h as f64 * CANVAS_W as f64)
                    / (mini.w as f64 * CANVAS_H as f64))
                    .round()
                    .max(1.0);
                (mini.w, mini.h, mini.rgba.clone())
            } else {
                let Some(asset) = app.media.asset(&clip.asset_id) else { continue };
                if asset.offline || asset.kind == MediaKind::Audio {
                    continue;
                }
                if let Some(mini) = app.monitor.preview_frames.get(&clip.id) {
                    (mini.w, mini.h, mini.rgba.clone())
                } else {
                    let path = if asset.kind == MediaKind::Image {
                        Some(asset.path.clone())
                    } else {
                        asset.thumbnail_path.clone()
                    };
                    let Some(path) = path else { continue };
                    if self.thumb_cache.as_ref().map(|(p, _)| p.as_str()) != Some(path.as_str()) {
                        let Some(sample) = load_thumb(&path) else { continue };
                        self.thumb_cache = Some((path.clone(), sample));
                    }
                    let (_, s) = self.thumb_cache.as_ref().expect("thumb cache");
                    (s.w, s.h, s.rgba.clone())
                }
            };
            // Quelle (u8) → f32; Effekte + Farbkorrektur identisch zum Export.
            let mut data = crate::core::pixbuf::rgba8_to_f32(&data_u8);
            let resolved = effects::resolve_video_effects(&clip.effects, media_t);
            if !resolved.is_empty() {
                effects::apply_effects_buffer(&mut data, w, h, (0, 0, w, h), &resolved, 1);
            }
            let params = grade::precompute(&clip.grade);
            let luts = grade::resolve_luts(&clip.grade, &mut self.lut_cache);
            if !params.is_identity() || luts.is_active() {
                grade::grade_buffer(&mut data, w, h, (0, 0, w, h), &params, &luts.borrow(), 1);
            }
            let quad = if clip.is_generator() {
                // Titel-Raster repräsentiert das volle Frame (ggf. erweitert).
                let mut q = compose::layer_quad(
                    CANVAS_W as f64,
                    CANVAS_H as f64,
                    CANVAS_W as f64,
                    CANVAS_H as f64,
                    &fx,
                );
                q.h *= title_extend;
                q
            } else {
                compose::layer_quad(
                    CANVAS_W as f64,
                    CANVAS_H as f64,
                    w as f64,
                    h as f64,
                    &fx,
                )
            };
            layers.push(Layer { data, w, h, quad, opacity: fx.opacity });
            any = true;
        }
        if !any {
            return None;
        }
        let frames: Vec<compose::CpuLayerFrame> = layers
            .iter()
            .map(|l| compose::CpuLayerFrame {
                data: &l.data,
                w: l.w,
                h: l.h,
                quad: l.quad,
                opacity: l.opacity,
                mask: None,
                blend_mode: compose::BlendMode::Normal,
            })
            .collect();
        compose::composite_frame(&mut canvas, CANVAS_W, CANVAS_H, &frames, 1);
        // f32-Analyse-Canvas → u8 für die Scope-Darstellung (Histogramm/WFM).
        Some(crate::core::pixbuf::f32_to_rgba8(&canvas))
    }
}

/// Graticule-Linien bei 0/25/50/75/100 %.
fn draw_graticule(ui: &mut Ui, area: Rect) {
    for i in 0..=4 {
        let y = area.y + (area.h * i as f32 / 4.0).round();
        ui.fill(
            Rect::new(area.x, y.min(area.bottom() - 1.0), area.w, 1.0),
            raylib::color::Color::new(255, 255, 255, 20),
        );
    }
}

/// Audio-Goniometer (Lissajous) + Korrelationsmeter aus dem Master-Mixdown.
/// Der Plot ist um 45° gedreht: mono (L=R) ⇒ senkrechte Spur, gegenphasig
/// (L=−R) ⇒ waagerechte Spur, volle Stereobreite ⇒ runde Wolke. Software-Plot
/// wie die Video-Scopes (kein GPU-Readback-Stall).
fn draw_audio_scope(ui: &mut Ui, audio: &AudioStore, mut area: Rect) {
    use raylib::color::Color;

    // Korrelationsmeter als unterer Streifen, darüber das Plotfeld.
    let meter = area.cut_bottom(74.0);
    ui.hline(meter.x, meter.y, meter.w, theme::LINE);
    draw_correlation_meter(ui, audio, meter.inset_xy(14.0, 10.0));

    // Quadratisches Plotfeld zentrieren.
    let side = area.w.min(area.h);
    let plot = area.center_box(side, side);
    let cx = plot.x + plot.w / 2.0;
    let cy = plot.y + plot.h / 2.0;
    let half = (side / 2.0 - 14.0).max(10.0);
    // Vollausschlag eines EINZELNEN Kanals liegt auf dem Referenzkreis; mono
    // (beide Kanäle voll) erreicht die obere Kante (= half), gegenphasig die
    // seitlichen Kanten — daraus der Plot-Gain g = half/2.
    let r_circle = half * std::f32::consts::FRAC_1_SQRT_2;
    let g = half / 2.0;

    // ---- Graticule -------------------------------------------------------
    let line = Color::new(255, 255, 255, 26);
    let line_soft = Color::new(255, 255, 255, 14);
    ui.circle_outline(v2(cx, cy), r_circle, line);
    // M-Achse (senkrecht = mono) betont, S-Achse (waagerecht = Seite) zart.
    ui.line_thin(v2(cx, cy - half), v2(cx, cy + half), line);
    ui.line_thin(v2(cx - half, cy), v2(cx + half, cy), line_soft);
    // L/R-Diagonalen = Einzelkanal-Richtungen (oben links/rechts).
    ui.line_thin(v2(cx, cy), v2(cx - r_circle, cy - r_circle), line_soft);
    ui.line_thin(v2(cx, cy), v2(cx + r_circle, cy - r_circle), line_soft);
    // Beschriftung der Pole.
    ui.text(
        "L",
        v2(cx - r_circle - 10.0, cy - r_circle - 6.0),
        theme::TEXT_3,
        FontKind::Sans12,
    );
    ui.text(
        "R",
        v2(cx + r_circle + 3.0, cy - r_circle - 6.0),
        theme::TEXT_3,
        FontKind::Sans12,
    );
    ui.text("M", v2(cx + 4.0, cy - half - 1.0), theme::TEXT_3, FontKind::Sans12);

    // ---- Streupunkte (oder Hinweis) --------------------------------------
    if audio.scope_stereo.is_empty() {
        let center = plot.center_box(260.0, 52.0);
        let mut c = center;
        let ic = c.cut_top(20.0);
        ui.icon("activity", ic, 20.0, theme::TEXT_3);
        c.cut_top(8.0);
        ui.text_centered(
            "Kein Audiosignal — Wiedergabe starten.",
            c,
            theme::TEXT_3,
            FontKind::Sans12,
        );
        return;
    }
    let dot = Color::new(110, 240, 165, 46);
    for &[l, r] in &audio.scope_stereo {
        let px = cx + g * (r - l);
        let py = cy - g * (l + r);
        ui.fill(Rect::new(px, py, 1.5, 1.5), dot);
    }
}

/// Horizontaler Korrelationsbalken (−1 … +1) mit numerischem Wert. +1 grün
/// (mono-kompatibel), 0 neutral (volle Breite), −1 rot (gegenphasig,
/// mono-inkompatibel — Phasenwarnung).
fn draw_correlation_meter(ui: &mut Ui, audio: &AudioStore, area: Rect) {
    use raylib::color::Color;
    let have = !audio.scope_stereo.is_empty();
    let val = audio.correlation.clamp(-1.0, 1.0);
    let pos = Color::new(90, 215, 140, 255);

    // Kopfzeile: Label links, Wert rechts (vorzeichenfarbig).
    let mut a = area;
    let head = a.cut_top(20.0);
    ui.text_left("Korrelation", head, theme::TEXT_2, FontKind::Sans12);
    let val_col = if !have {
        theme::TEXT_3
    } else if val >= 0.0 {
        pos
    } else {
        theme::DANGER
    };
    let val_txt = if have { format!("{val:+.2}") } else { "—".to_string() };
    ui.text_right(&val_txt, head, val_col, FontKind::Mono12);

    // Messbalken.
    a.cut_top(2.0);
    let bar = a.cut_top(14.0);
    ui.fill(bar, theme::SURFACE_2);
    let mid_x = bar.x + bar.w / 2.0;
    ui.vline(mid_x, bar.y, bar.h, theme::LINE_STRONG);
    if have {
        // Füllung von der Mitte bis zum Wert.
        let x = bar.x + (val * 0.5 + 0.5) * bar.w;
        let (fx, fw) = if val >= 0.0 {
            (mid_x, x - mid_x)
        } else {
            (x, mid_x - x)
        };
        let fill_col = if val >= 0.0 {
            theme::with_alpha(pos, 200)
        } else {
            theme::with_alpha(theme::DANGER, 200)
        };
        ui.fill(Rect::new(fx, bar.y, fw.max(1.0), bar.h), fill_col);
        ui.vline(
            x.clamp(bar.x, bar.right() - 1.0),
            bar.y - 2.0,
            bar.h + 4.0,
            theme::TEXT_1,
        );
    }

    // Skala.
    let scale = a.cut_top(14.0);
    ui.text_left("-1", scale, theme::TEXT_3, FontKind::Sans12);
    ui.text_centered("0", scale, theme::TEXT_3, FontKind::Sans12);
    ui.text_right("+1", scale, theme::TEXT_3, FontKind::Sans12);
}

impl Panel for ScopesPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);
        let mut area = rect;

        // Kopfzeile: Modus-Select links, Label rechts
        let bar = area.cut_top(36.0);
        ui.hline(bar.x, bar.bottom() - 1.0, bar.w, theme::LINE);
        let bar_inner = bar.inset_xy(8.0, 0.0);
        let select_rect = Rect::new(bar_inner.x, bar.y + 6.0, 120.0, 24.0);
        if let Some(m) = select(ui, "scopes.mode", select_rect, &MODES, self.mode) {
            self.mode = m;
        }
        ui.text_right(
            match self.mode {
                0 => "Luma (709)",
                1 => "R / G / B",
                2 => "Cb/Cr (709)",
                3 => "RGB",
                _ => "L/R · Phase",
            },
            bar_inner,
            theme::TEXT_3,
            FontKind::Sans12,
        );

        ui.fill(area, theme::BLACK);

        // ---- Audio-Modus: Goniometer + Korrelationsmeter (Mixdown) ----------
        if self.mode == AUDIO_MODE {
            draw_audio_scope(ui, &app.audio, area);
            return;
        }

        let Some(canvas) = self.compose_program(app) else {
            let center = area.center_box(260.0, 52.0);
            let mut c = center;
            let ic = c.cut_top(20.0);
            ui.icon("monitor-off", ic, 20.0, theme::TEXT_3);
            c.cut_top(8.0);
            ui.text_centered(
                "Kein Bildsignal — Playhead über einen Clip bewegen.",
                c,
                theme::TEXT_3,
                FontKind::Sans12,
            );
            return;
        };

        let w = area.w;
        let h = area.h;
        let px_at = |x: usize, y: usize| -> [u8; 3] {
            let i = (y * CANVAS_W + x) * 4;
            [canvas[i], canvas[i + 1], canvas[i + 2]]
        };

        match self.mode {
            0 => {
                // ---- Waveform: Luma je Spalte (additiv hell) ----
                draw_graticule(ui, area);
                let col_w = w / CANVAS_W as f32;
                let color = raylib::color::Color::new(70, 240, 130, 26);
                for x in 0..CANVAS_W {
                    for y in 0..CANVAS_H {
                        let [r, g, b] = px_at(x, y);
                        let luma =
                            0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32;
                        let py = area.y + h - 1.0 - (luma / 255.0) * (h - 2.0);
                        ui.fill(
                            Rect::new(area.x + x as f32 * col_w, py, col_w.max(1.0), 1.0),
                            color,
                        );
                    }
                }
            }
            1 => {
                // ---- RGB-Parade: drei Waveforms nebeneinander ----
                draw_graticule(ui, area);
                let third = w / 3.0;
                let colors = [
                    raylib::color::Color::new(255, 90, 90, 26),
                    raylib::color::Color::new(90, 240, 130, 26),
                    raylib::color::Color::new(110, 150, 255, 26),
                ];
                for (c, color) in colors.iter().enumerate() {
                    let x0 = area.x + third * c as f32;
                    let col_w = third / CANVAS_W as f32;
                    for x in 0..CANVAS_W {
                        for y in 0..CANVAS_H {
                            let v = px_at(x, y)[c] as f32;
                            let py = area.y + h - 1.0 - (v / 255.0) * (h - 2.0);
                            ui.fill(
                                Rect::new(x0 + x as f32 * col_w, py, col_w.max(1.0), 1.0),
                                *color,
                            );
                        }
                    }
                    if c > 0 {
                        ui.vline(x0, area.y, h, theme::with_alpha(theme::LINE, 120));
                    }
                }
            }
            2 => {
                // ---- Vektorskop: Cb/Cr-Punktwolke (BT.709) ----
                let cx = area.x + w / 2.0;
                let cy = area.y + h / 2.0;
                let radius = (w.min(h) / 2.0 - 8.0).max(10.0);
                // Graticule: Kreise bei 75 % / 100 %, Achsenkreuz.
                let line = raylib::color::Color::new(255, 255, 255, 26);
                ui.circle_outline(v2(cx, cy), radius, line);
                ui.circle_outline(v2(cx, cy), radius * 0.75, line);
                ui.line_thin(v2(cx - radius, cy), v2(cx + radius, cy), line);
                ui.line_thin(v2(cx, cy - radius), v2(cx, cy + radius), line);
                // Hauttonlinie (~123° im Vektorskop).
                let skin = 123.0f32.to_radians();
                ui.line_thin(
                    v2(cx, cy),
                    v2(cx + skin.cos() * radius, cy - skin.sin() * radius),
                    raylib::color::Color::new(255, 255, 255, 40),
                );
                let color = raylib::color::Color::new(120, 235, 170, 22);
                for y in 0..CANVAS_H {
                    for x in 0..CANVAS_W {
                        let [r, g, b] = px_at(x, y);
                        let (rf, gf, bf) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
                        // BT.709-Chroma, normiert auf ±0,5.
                        let cb = -0.1146 * rf - 0.3854 * gf + 0.5 * bf;
                        let cr = 0.5 * rf - 0.4542 * gf - 0.0458 * bf;
                        let px = cx + cb * 2.0 * radius;
                        let py = cy - cr * 2.0 * radius;
                        ui.fill(Rect::new(px, py, 1.5, 1.5), color);
                    }
                }
            }
            _ => {
                // ---- Histogramm: RGB-Verteilungen als Linienzüge ----
                draw_graticule(ui, area);
                let mut bins = [[0f32; 256]; 3];
                for px in canvas.chunks_exact(4) {
                    bins[0][px[0] as usize] += 1.0;
                    bins[1][px[1] as usize] += 1.0;
                    bins[2][px[2] as usize] += 1.0;
                }
                let mut max = 1.0f32;
                for channel in &bins {
                    for v in channel {
                        max = max.max(*v);
                    }
                }
                let colors = [
                    raylib::color::Color::new(255, 90, 90, 217),
                    raylib::color::Color::new(90, 240, 130, 217),
                    raylib::color::Color::new(110, 150, 255, 217),
                ];
                for (c, channel) in bins.iter().enumerate() {
                    let mut prev = raylib::math::Vector2::new(area.x, area.y + h);
                    for (b, v) in channel.iter().enumerate() {
                        let x = area.x + (b as f32 / 255.0) * (w - 1.0);
                        let y = area.y + h - 1.0 - (v / max) * (h - 4.0);
                        ui.line_thin(prev, raylib::math::Vector2::new(x, y), colors[c]);
                        prev = raylib::math::Vector2::new(x, y);
                    }
                }
            }
        }
    }
}
