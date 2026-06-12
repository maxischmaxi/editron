//! Gemeinsame Compositing-Mathematik für Programmmonitor und Export:
//! sichtbare Video-Layer am Playhead (unten → oben) und die Abbildung der
//! animierten Clip-Parameter auf ein Transform-Quad im Zielframe.
//!
//! Einheiten siehe `core/animation.rs`: Position in % der Framemaße
//! (Offset vom Zentrum), Skalierung in % der Contain-Fit-Größe, Rotation in
//! Grad, Deckkraft 0–100. Dadurch sehen Vorschau (beliebige Monitorgröße)
//! und Export (beliebige Zielauflösung) identisch aus.

use crate::core::animation::ClipFx;
use crate::core::timeline::{TimelineClip, TimelineStore, TrackKind};
use crate::core::title::TitleSpec;
use crate::core::transitions::{self, TransitionFx, TransitionRole};

/// Ausgewertete Parameter zu einem Zeitpunkt (Deckkraft normiert 0–1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EvalFx {
    pub pos_x: f64,
    pub pos_y: f64,
    pub scale_x: f64,
    pub scale_y: f64,
    pub rotation: f64,
    pub opacity: f64,
}

/// Medienzeit eines Clips zur Sequenzzeit `t_seq` — DIE gemeinsame
/// Abbildung (speed-, rückwärts- und standbild-bewusst) für Player,
/// Compositor, Renderplan, Scopes, Gizmo und Keyframe-Editor.
pub fn clip_media_time(clip: &TimelineClip, t_seq: f64) -> f64 {
    clip.media_time_at(t_seq)
}

pub fn eval_fx(fx: &ClipFx, media_t: f64) -> EvalFx {
    let sx = fx.scale_x.eval(media_t);
    let sy = if fx.uniform_scale {
        sx
    } else {
        fx.scale_y.eval(media_t)
    };
    EvalFx {
        pos_x: fx.pos_x.eval(media_t),
        pos_y: fx.pos_y.eval(media_t),
        scale_x: sx,
        scale_y: sy,
        rotation: fx.rotation.eval(media_t),
        opacity: (fx.opacity.eval(media_t) / 100.0).clamp(0.0, 1.0),
    }
}

/// Transform-Quad eines Layers im Zielframe: Mittelpunkt, Größe, Rotation.
#[derive(Clone, Copy, Debug)]
pub struct LayerQuad {
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
    pub rot_deg: f64,
}

/// Quad aus Framegröße, natürlicher Mediengröße und ausgewerteten Parametern.
/// Basis (Skalierung 100 %) ist die Contain-Fit-Größe im Frame.
pub fn layer_quad(
    frame_w: f64,
    frame_h: f64,
    natural_w: f64,
    natural_h: f64,
    fx: &EvalFx,
) -> LayerQuad {
    let (nw, nh) = (natural_w.max(1.0), natural_h.max(1.0));
    let fit = (frame_w / nw).min(frame_h / nh);
    LayerQuad {
        cx: frame_w / 2.0 + fx.pos_x / 100.0 * frame_w,
        cy: frame_h / 2.0 + fx.pos_y / 100.0 * frame_h,
        w: nw * fit * fx.scale_x / 100.0,
        h: nh * fit * fx.scale_y / 100.0,
        rot_deg: fx.rotation,
    }
}

impl LayerQuad {
    /// Eckpunkte (im Uhrzeigersinn ab oben links), rotiert um den Mittelpunkt.
    pub fn corners(&self) -> [(f64, f64); 4] {
        let (s, c) = self.rot_deg.to_radians().sin_cos();
        let (hw, hh) = (self.w / 2.0, self.h / 2.0);
        [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)]
            .map(|(x, y)| (self.cx + x * c - y * s, self.cy + x * s + y * c))
    }
}

/// Sichtbare Video-Clips am Zeitpunkt `t`, von der untersten zur obersten
/// Spur (Zeichenreihenfolge). Mute/Solo der Spuren greifen wie im Player.
/// OHNE Übergangs-Erweiterung — für Scopes/Farbe-Panel (ein Clip je Spur).
pub fn visible_video_clips<'a>(timeline: &'a TimelineStore, t: f64) -> Vec<&'a TimelineClip> {
    let solo_any = timeline.tracks.iter().any(|tr| tr.solo);
    timeline
        .tracks
        .iter()
        // Spur-Index 0 ist die OBERSTE Videospur → rückwärts = unten zuerst.
        .rev()
        .filter(|tr| tr.kind == TrackKind::Video && !tr.muted && (!solo_any || tr.solo))
        .filter_map(|tr| {
            timeline
                .clips
                .iter()
                .find(|c| c.track_id == tr.id && c.enabled && t >= c.start && t < c.end())
        })
        .collect()
}

// ------------------------------------------------------ Programm-Layer
// Layer-Auflösung MIT Übergängen: während eines Übergangs trägt eine Spur
// zwei Clip-Layer (ausgehend unten, eingehend darüber) und bei Dips eine
// Farbfläche. Programmmonitor und Player-Decoder-Targets nutzen diese
// Funktion; der Export-Planer bildet dieselbe Semantik segmentweise ab.

/// Ein aufgelöster Layer der Programmausgabe (Zeichenreihenfolge).
pub enum ProgramLayer<'a> {
    Clip {
        clip: &'a TimelineClip,
        /// Übergangs-Auswirkung (Identität, wenn kein Übergang aktiv).
        t_fx: TransitionFx,
    },
    /// Volldeckende Farbfläche (Dip zu Schwarz/Weiß) mit Alpha 0–1.
    Solid { white: bool, alpha: f64 },
}

/// Sichtbare Programm-Layer am Zeitpunkt `t` (unten → oben), inklusive der
/// Übergangs-Logik: innerhalb eines Übergangsfensters laufen ausgehender
/// und eingehender Clip parallel (Medienzeit über die Schnittkante hinaus —
/// durch die Handles-Klemmung im Modell garantiert vorhanden).
/// Untertitel-Spuren liegen über dem Video-Block und werden zuletzt
/// gezeichnet; Solo (Video) blendet sie nicht aus, `muted` (= ausgeblendet)
/// schon.
pub fn visible_program_layers<'a>(timeline: &'a TimelineStore, t: f64) -> Vec<ProgramLayer<'a>> {
    let solo_any = timeline.tracks.iter().any(|tr| tr.solo);
    let mut out: Vec<ProgramLayer<'a>> = Vec::new();
    for track in timeline.tracks.iter().rev().filter(|tr| match tr.kind {
        TrackKind::Video => !tr.muted && (!solo_any || tr.solo),
        TrackKind::Subtitle => !tr.muted,
        TrackKind::Audio => false,
    }) {
        // Aktiver Übergang dieser Spur am Zeitpunkt t?
        let active = timeline.transitions.iter().find_map(|tr| {
            if tr.kind.is_audio() {
                return None;
            }
            let (from, to) = transitions::resolve_clips(&timeline.clips, tr);
            if from.or(to)?.track_id != track.id {
                return None;
            }
            let (w0, w1) = transitions::window(from, to, tr.alignment, tr.duration)?;
            (t >= w0 && t < w1 && w1 > w0).then_some((tr, from, to, w0, w1))
        });
        if let Some((tr, from, to, w0, w1)) = active {
            let p = ((t - w0) / (w1 - w0)).clamp(0.0, 1.0);
            let two_sided = from.is_some() && to.is_some();
            if let Some(f) = from.filter(|c| c.enabled) {
                let role = if two_sided { TransitionRole::Out } else { TransitionRole::OutSolo };
                out.push(ProgramLayer::Clip {
                    clip: f,
                    t_fx: transitions::eval_video(tr.kind, tr.direction, role, p),
                });
            }
            if let Some(c) = to.filter(|c| c.enabled) {
                let role = if two_sided { TransitionRole::In } else { TransitionRole::InSolo };
                out.push(ProgramLayer::Clip {
                    clip: c,
                    t_fx: transitions::eval_video(tr.kind, tr.direction, role, p),
                });
            }
            if tr.kind.is_dip() {
                let role = if two_sided {
                    TransitionRole::Dip
                } else if from.is_some() {
                    TransitionRole::DipOut
                } else {
                    TransitionRole::DipIn
                };
                let fx = transitions::eval_video(tr.kind, tr.direction, role, p);
                if fx.opacity > 0.0 {
                    out.push(ProgramLayer::Solid {
                        white: tr.kind == crate::core::transitions::TransitionKind::DipToWhite,
                        alpha: fx.opacity,
                    });
                }
            }
        } else if let Some(clip) = timeline
            .clips
            .iter()
            .find(|c| c.track_id == track.id && c.enabled && t >= c.start && t < c.end())
        {
            out.push(ProgramLayer::Clip {
                clip,
                t_fx: TransitionFx::IDENTITY,
            });
        }
    }
    out
}

/// Text-Spec eines Layers auflösen: Titel-Clips tragen ihren Spec selbst,
/// Untertitel-Segmente synthetisieren ihn aus Spurstil + Text. Eine
/// Funktion für Titel-Engine (Monitor), Scopes und Export — die Optik ist
/// damit überall identisch und Stiländerungen invalidieren den Raster-Cache
/// über den `content_hash` des synthetisierten Specs.
pub fn layer_title_spec(timeline: &TimelineStore, clip: &TimelineClip) -> Option<TitleSpec> {
    if let Some(spec) = &clip.title {
        return Some(spec.clone());
    }
    let sub = clip.subtitle.as_ref()?;
    Some(timeline.subtitle_style(&clip.track_id).title_spec(&sub.text))
}

/// Übergangs-Auswirkung auf ein Layer-Quad anwenden: Versatz in Frame-
/// Bruchteilen, Skalierung um den Quad-Mittelpunkt. Eine Formel für GPU-
/// Vorschau (Canvas-Maße) und CPU-Export (Zielauflösung).
pub fn apply_transition_to_quad(quad: &mut LayerQuad, t_fx: &TransitionFx, frame_w: f64, frame_h: f64) {
    quad.cx += t_fx.offset_x * frame_w;
    quad.cy += t_fx.offset_y * frame_h;
    quad.w *= t_fx.scale;
    quad.h *= t_fx.scale;
}

/// Wipe-Maske in ganzzahlige Framepixel umrechnen (x0, y0, x1, y1 — Ende
/// exklusiv). Identische Rundung in Vorschau und Export.
pub fn mask_to_pixels(
    mask: &crate::core::transitions::MaskFrac,
    frame_w: usize,
    frame_h: usize,
) -> (usize, usize, usize, usize) {
    let px = |f: f64, n: usize| ((f * n as f64).round().clamp(0.0, n as f64)) as usize;
    (
        px(mask[0], frame_w),
        px(mask[1], frame_h),
        px(mask[2], frame_w),
        px(mask[3], frame_h),
    )
}

// ------------------------------------------------------- CPU-Compositing
// Software-Renderer für den Export: Layer (RGBA-Puffer, die das volle Frame
// in Decode-Auflösung repräsentieren — transparent gepolstert) werden per
// inverser Abbildung (Rotation/Skalierung/Translation) bilinear gesampelt
// und mit Src-over-Alpha auf das Canvas gemischt.

/// Ein zu komponierender Layer-Frame.
pub struct CpuLayerFrame<'a> {
    /// RGBA, `w`×`h`; repräsentiert das volle Zielframe (ggf. höher aufgelöst).
    pub data: &'a [u8],
    pub w: usize,
    pub h: usize,
    pub quad: LayerQuad,
    /// 0–1; multipliziert das Sample-Alpha.
    pub opacity: f64,
    /// Sichtbarer Canvas-Ausschnitt in Pixeln (x0, y0, x1, y1 — Ende
    /// exklusiv); None = voll sichtbar. Harte Kante (Wipe-Übergang).
    pub mask: Option<(usize, usize, usize, usize)>,
}

/// Alle Layer (unten → oben) auf ein opakes Canvas mischen; Zeilenbänder
/// laufen parallel auf `threads` Threads.
pub fn composite_frame(
    canvas: &mut [u8],
    w: usize,
    h: usize,
    layers: &[CpuLayerFrame],
    threads: usize,
) {
    debug_assert_eq!(canvas.len(), w * h * 4);
    let threads = threads.clamp(1, 64);
    let band_rows = h.div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for (band_idx, band) in canvas.chunks_mut(band_rows * w * 4).enumerate() {
            scope.spawn(move || {
                let y0 = band_idx * band_rows;
                let rows = band.len() / (w * 4);
                for layer in layers {
                    composite_band(band, w, y0, rows, layer);
                }
            });
        }
    });
}

/// Einen Layer auf ein Zeilenband mischen (`band` beginnt bei Canvas-Zeile `y0`).
fn composite_band(band: &mut [u8], w: usize, y0: usize, rows: usize, layer: &CpuLayerFrame) {
    if layer.opacity <= 0.0 || layer.quad.w <= 0.0 || layer.quad.h <= 0.0 {
        return;
    }
    // Bounding-Box des rotierten Quads, beschnitten auf das Band.
    let corners = layer.quad.corners();
    let min_x = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
    let max_x = corners.iter().map(|c| c.0).fold(f64::NEG_INFINITY, f64::max);
    let min_y = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
    let max_y = corners.iter().map(|c| c.1).fold(f64::NEG_INFINITY, f64::max);
    let mut x0 = (min_x.floor().max(0.0)) as usize;
    let mut x1 = (max_x.ceil().min(w as f64)).max(0.0) as usize;
    let mut row_start = (min_y.floor().max(y0 as f64)) as usize;
    let mut row1 = (max_y.ceil().min((y0 + rows) as f64)).max(0.0) as usize;
    // Wipe-Maske: zusätzlich auf den sichtbaren Frame-Ausschnitt beschneiden.
    if let Some((mx0, my0, mx1, my1)) = layer.mask {
        x0 = x0.max(mx0);
        x1 = x1.min(mx1);
        row_start = row_start.max(my0);
        row1 = row1.min(my1);
    }
    if x1 <= x0 || row1 <= row_start {
        return;
    }

    // Inverse Abbildung: Canvas-Pixel → lokale Quad-Koordinaten → Layer-UV.
    let (sin_r, cos_r) = (-layer.quad.rot_deg.to_radians()).sin_cos();
    let (cx, cy) = (layer.quad.cx, layer.quad.cy);
    let (qw, qh) = (layer.quad.w, layer.quad.h);
    let (lw, lh) = (layer.w as f64, layer.h as f64);
    let opacity = layer.opacity.clamp(0.0, 1.0) as f32;

    for y in row_start..row1 {
        let band_row = y - y0;
        let row = &mut band[band_row * w * 4..(band_row + 1) * w * 4];
        for x in x0..x1 {
            let dx = x as f64 + 0.5 - cx;
            let dy = y as f64 + 0.5 - cy;
            let lx = dx * cos_r - dy * sin_r;
            let ly = dx * sin_r + dy * cos_r;
            let u = lx / qw + 0.5;
            let v = ly / qh + 0.5;
            if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                continue;
            }
            // Bilineares Sampling mit Randklemmung.
            let fx = (u * lw - 0.5).max(0.0);
            let fy = (v * lh - 0.5).max(0.0);
            let ix = fx as usize;
            let iy = fy as usize;
            let ix1 = (ix + 1).min(layer.w - 1);
            let iy1 = (iy + 1).min(layer.h - 1);
            let tx = (fx - ix as f64) as f32;
            let ty = (fy - iy as f64) as f32;
            let p00 = &layer.data[(iy * layer.w + ix) * 4..(iy * layer.w + ix) * 4 + 4];
            let p10 = &layer.data[(iy * layer.w + ix1) * 4..(iy * layer.w + ix1) * 4 + 4];
            let p01 = &layer.data[(iy1 * layer.w + ix) * 4..(iy1 * layer.w + ix) * 4 + 4];
            let p11 = &layer.data[(iy1 * layer.w + ix1) * 4..(iy1 * layer.w + ix1) * 4 + 4];
            let mut sample = [0f32; 4];
            for c in 0..4 {
                let top = p00[c] as f32 * (1.0 - tx) + p10[c] as f32 * tx;
                let bot = p01[c] as f32 * (1.0 - tx) + p11[c] as f32 * tx;
                sample[c] = top * (1.0 - ty) + bot * ty;
            }
            let alpha = sample[3] / 255.0 * opacity;
            if alpha <= 0.0 {
                continue;
            }
            let dst = &mut row[x * 4..x * 4 + 4];
            for c in 0..3 {
                let blended = sample[c] * alpha + dst[c] as f32 * (1.0 - alpha);
                dst[c] = blended.round().clamp(0.0, 255.0) as u8;
            }
            // Canvas bleibt opak (Hintergrund ist deckend schwarz).
            dst[3] = 255;
        }
    }
}

/// Maximale visuelle Skalierung (X/Y, in %→Faktor) im Medienzeit-Fenster —
/// bestimmt die Decode-Auflösung eines Export-Layers (Qualität bei Zoom).
pub fn max_scale_in_window(fx: &ClipFx, t0: f64, t1: f64) -> f64 {
    let mut max_pct: f64 = 0.0;
    let mut probe = |p: &crate::core::animation::AnimatedParam| {
        max_pct = max_pct.max(p.eval(t0).abs()).max(p.eval(t1).abs());
        for k in &p.keyframes {
            if k.t >= t0 && k.t <= t1 {
                max_pct = max_pct.max(k.value.abs());
            }
        }
    };
    probe(&fx.scale_x);
    if !fx.uniform_scale {
        probe(&fx.scale_y);
    }
    max_pct / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::animation::AnimatedParam;

    #[test]
    fn identity_quad_is_contain_fit() {
        let fx = eval_fx(&ClipFx::default(), 0.0);
        // 1920×1080-Frame, 4:3-Quelle → Höhe füllt, Breite 1440.
        let q = layer_quad(1920.0, 1080.0, 1600.0, 1200.0, &fx);
        assert!((q.cx - 960.0).abs() < 1e-9);
        assert!((q.cy - 540.0).abs() < 1e-9);
        assert!((q.w - 1440.0).abs() < 1e-9);
        assert!((q.h - 1080.0).abs() < 1e-9);
        assert_eq!(q.rot_deg, 0.0);
    }

    #[test]
    fn position_offsets_are_percent_of_frame() {
        let mut fx = ClipFx::default();
        fx.pos_x = AnimatedParam::fixed(25.0); // +25 % der Breite
        fx.pos_y = AnimatedParam::fixed(-50.0);
        let e = eval_fx(&fx, 0.0);
        let q = layer_quad(1000.0, 500.0, 100.0, 100.0, &e);
        assert!((q.cx - 750.0).abs() < 1e-9);
        assert!((q.cy - 0.0).abs() < 1e-9);
    }

    #[test]
    fn uniform_scale_mirrors_x() {
        let mut fx = ClipFx::default();
        fx.scale_x = AnimatedParam::fixed(200.0);
        fx.scale_y = AnimatedParam::fixed(50.0); // wird ignoriert (uniform)
        let e = eval_fx(&fx, 0.0);
        assert_eq!(e.scale_x, 200.0);
        assert_eq!(e.scale_y, 200.0);
        fx.uniform_scale = false;
        let e = eval_fx(&fx, 0.0);
        assert_eq!(e.scale_y, 50.0);
    }

    /// Opakes Canvas (schwarz) bauen.
    fn black_canvas(w: usize, h: usize) -> Vec<u8> {
        let mut c = vec![0u8; w * h * 4];
        for px in c.chunks_exact_mut(4) {
            px[3] = 255;
        }
        c
    }

    fn px(canvas: &[u8], w: usize, x: usize, y: usize) -> [u8; 4] {
        let i = (y * w + x) * 4;
        [canvas[i], canvas[i + 1], canvas[i + 2], canvas[i + 3]]
    }

    #[test]
    fn composite_scales_and_positions_layer() {
        // 8×8-Frame, roter Layer in voller Framegröße, auf 50 % skaliert
        // und zentriert → Mitte rot, Ecken schwarz.
        let (w, h) = (8usize, 8usize);
        let mut canvas = black_canvas(w, h);
        let red: Vec<u8> = std::iter::repeat([255u8, 0, 0, 255])
            .take(w * h)
            .flatten()
            .collect();
        let fx = EvalFx {
            pos_x: 0.0,
            pos_y: 0.0,
            scale_x: 50.0,
            scale_y: 50.0,
            rotation: 0.0,
            opacity: 1.0,
        };
        let quad = layer_quad(w as f64, h as f64, w as f64, h as f64, &fx);
        composite_frame(
            &mut canvas,
            w,
            h,
            &[CpuLayerFrame { data: &red, w, h, quad, opacity: 1.0, mask: None }],
            2,
        );
        assert_eq!(px(&canvas, w, 4, 4)[0], 255, "Mitte muss rot sein");
        assert_eq!(px(&canvas, w, 0, 0), [0, 0, 0, 255], "Ecke bleibt schwarz");
        assert_eq!(px(&canvas, w, 7, 7), [0, 0, 0, 255]);
    }

    #[test]
    fn composite_applies_opacity_blend() {
        let (w, h) = (4usize, 4usize);
        let mut canvas = black_canvas(w, h);
        let white: Vec<u8> = std::iter::repeat([255u8; 4]).take(w * h).flatten().collect();
        let fx = eval_fx(&ClipFx::default(), 0.0);
        let quad = layer_quad(w as f64, h as f64, w as f64, h as f64, &fx);
        composite_frame(
            &mut canvas,
            w,
            h,
            &[CpuLayerFrame { data: &white, w, h, quad, opacity: 0.5, mask: None }],
            1,
        );
        let p = px(&canvas, w, 2, 2);
        assert!((p[0] as i32 - 128).abs() <= 2, "50 % Weiß auf Schwarz ≈ 128: {p:?}");
        assert_eq!(p[3], 255);
    }

    #[test]
    fn composite_respects_mask() {
        // Wipe-Halbzeit: weißer Layer nur in der linken Hälfte sichtbar.
        let (w, h) = (4usize, 4usize);
        let mut canvas = black_canvas(w, h);
        let white: Vec<u8> = std::iter::repeat([255u8; 4]).take(w * h).flatten().collect();
        let fx = eval_fx(&ClipFx::default(), 0.0);
        let quad = layer_quad(w as f64, h as f64, w as f64, h as f64, &fx);
        let mask = mask_to_pixels(&[0.0, 0.0, 0.5, 1.0], w, h);
        composite_frame(
            &mut canvas,
            w,
            h,
            &[CpuLayerFrame { data: &white, w, h, quad, opacity: 1.0, mask: Some(mask) }],
            1,
        );
        assert_eq!(px(&canvas, w, 1, 2)[0], 255, "links sichtbar");
        assert_eq!(px(&canvas, w, 2, 2), [0, 0, 0, 255], "rechts maskiert");
    }

    #[test]
    fn composite_respects_layer_order() {
        let (w, h) = (4usize, 4usize);
        let mut canvas = black_canvas(w, h);
        let red: Vec<u8> = std::iter::repeat([255u8, 0, 0, 255]).take(w * h).flatten().collect();
        let green: Vec<u8> = std::iter::repeat([0u8, 255, 0, 255]).take(w * h).flatten().collect();
        let fx = eval_fx(&ClipFx::default(), 0.0);
        let quad = layer_quad(w as f64, h as f64, w as f64, h as f64, &fx);
        composite_frame(
            &mut canvas,
            w,
            h,
            &[
                CpuLayerFrame { data: &red, w, h, quad, opacity: 1.0, mask: None },
                CpuLayerFrame { data: &green, w, h, quad, opacity: 1.0, mask: None },
            ],
            1,
        );
        assert_eq!(px(&canvas, w, 1, 1)[1], 255, "oberer Layer (grün) gewinnt");
    }

    #[test]
    fn max_scale_covers_keyframes_and_endpoints() {
        let mut fx = ClipFx::default();
        fx.scale_x.upsert_key(0.0, 100.0);
        fx.scale_x.upsert_key(2.0, 300.0);
        fx.scale_x.upsert_key(4.0, 50.0);
        assert!((max_scale_in_window(&fx, 0.0, 4.0) - 3.0).abs() < 1e-9);
        assert!((max_scale_in_window(&fx, 2.5, 4.0) - 2.40625).abs() < 0.2);
        let plain = ClipFx::default();
        assert!((max_scale_in_window(&plain, 0.0, 1.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn corners_rotate_around_center() {
        let q = LayerQuad {
            cx: 100.0,
            cy: 100.0,
            w: 20.0,
            h: 20.0,
            rot_deg: 90.0,
        };
        let c = q.corners();
        // Oben-links (-10,-10) wird zu (+10,-10) relativ zum Zentrum.
        assert!((c[0].0 - 110.0).abs() < 1e-9);
        assert!((c[0].1 - 90.0).abs() < 1e-9);
    }
}
