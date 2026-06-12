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

/// Medienzeit eines Clips zur Sequenzzeit `t_seq`.
pub fn clip_media_time(clip: &TimelineClip, t_seq: f64) -> f64 {
    clip.src_in + (t_seq - clip.start)
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
    let x0 = (min_x.floor().max(0.0)) as usize;
    let x1 = (max_x.ceil().min(w as f64)).max(0.0) as usize;
    let row_start = (min_y.floor().max(y0 as f64)) as usize;
    let row1 = (max_y.ceil().min((y0 + rows) as f64)).max(0.0) as usize;
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
            &[CpuLayerFrame { data: &red, w, h, quad, opacity: 1.0 }],
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
            &[CpuLayerFrame { data: &white, w, h, quad, opacity: 0.5 }],
            1,
        );
        let p = px(&canvas, w, 2, 2);
        assert!((p[0] as i32 - 128).abs() <= 2, "50 % Weiß auf Schwarz ≈ 128: {p:?}");
        assert_eq!(p[3], 255);
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
                CpuLayerFrame { data: &red, w, h, quad, opacity: 1.0 },
                CpuLayerFrame { data: &green, w, h, quad, opacity: 1.0 },
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
