//! CPU-Text-Rasterizer für Titel-Clips: EIN Renderer für beide Pfade —
//! der Programmmonitor lädt das Ergebnis als Textur (1:1 in Canvas-
//! Auflösung), der Export komponiert denselben Puffer in Zielauflösung.
//! Pixel-Parität ist damit konstruktionsbedingt: gleiche Shaping-,
//! Raster- und Compositing-Formeln, nur die Auflösung skaliert.
//!
//! Typografie: Shaping über swash (HarfBuzz-Klasse: GSUB/GPOS, echtes
//! Kerning, Ligaturen), Glyph-Konturen über zeno mit Subpixel-Offset
//! gerastert (sauberes Antialiasing auch bei großen Graden), Konturen als
//! echte Outline-Strokes (runde Joins — keine ausgefransten Kanten),
//! Schatten als separierter Box-Blur (3 Pässe ≈ Gauß).
//!
//! Schriften kommen über fontconfig (`fc-match`/`fc-list`) — dieselbe
//! Discovery, die auch die UI-Fonts in `src/ui/text.rs` auflöst.

use crate::core::title::{TitleAlign, TitleSpec, TitleWeight, REF_HEIGHT};
use std::collections::HashMap;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::shape::ShapeContext;
use swash::zeno;
use swash::FontRef;

// ============================================================== fontconfig

/// fontconfig-Anfrage: (Familie, Dateipfad, Face-Index) des besten Matches.
pub fn fc_match(query: &str) -> Option<(String, String, usize)> {
    let out = Command::new("fc-match")
        .arg("-f")
        .arg("%{family}\t%{file}\t%{index}")
        .arg(query)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut parts = s.split('\t');
    let family = parts.next()?.trim().to_string();
    let file = parts.next()?.trim().to_string();
    let index: usize = parts.next().and_then(|i| i.trim().parse().ok()).unwrap_or(0);
    if file.is_empty() {
        return None;
    }
    Some((family, file, index))
}

/// Alle installierten Schriftfamilien (erste Familie je Face, sortiert,
/// dedupliziert) — Datenquelle des Familien-Selects im Grafik-Panel.
pub fn list_font_families() -> &'static [String] {
    static FAMILIES: OnceLock<Vec<String>> = OnceLock::new();
    FAMILIES.get_or_init(|| {
        let out = Command::new("fc-list")
            .arg("-f")
            .arg("%{family[0]}\n")
            .output()
            .ok();
        let mut families: Vec<String> = out
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        families.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
        families.dedup();
        families
    })
}

/// Geladene Font-Datei (Bytes + Face-Index) für swash.
pub struct FontData {
    pub data: Vec<u8>,
    pub index: usize,
}

impl FontData {
    pub fn font_ref(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(&self.data, self.index)
    }
}

/// Schrift für (Familie, Schnitt, Kursiv) auflösen und cachen. Leere
/// Familie = Plattform-Sans. fc-match liefert immer einen Fallback —
/// None nur, wenn gar keine Schrift installiert ist.
pub fn font_for(family: &str, weight: TitleWeight, italic: bool) -> Option<Arc<FontData>> {
    static CACHE: OnceLock<Mutex<HashMap<(String, u32, bool), Option<Arc<FontData>>>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = (family.to_string(), weight.fc_weight(), italic);
    if let Some(hit) = cache.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        return hit.clone();
    }

    let base = if family.trim().is_empty() {
        "sans-serif".to_string()
    } else {
        family.trim().to_string()
    };
    let slant = if italic { ":slant=100" } else { "" };
    let query = format!("{base}:weight={}{slant}", weight.fc_weight());
    let loaded = fc_match(&query)
        .or_else(|| fc_match(&format!("sans-serif:weight={}", weight.fc_weight())))
        .and_then(|(_, file, index)| {
            let data = std::fs::read(&file).ok()?;
            let fd = FontData { data, index };
            fd.font_ref()?; // validieren
            Some(Arc::new(fd))
        });
    cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key, loaded.clone());
    loaded
}

// ================================================================== Layout

/// Geformte Glyphe einer Zeile: Position relativ zum Zeilenursprung
/// (x = Stiftposition, y = Offset zur Grundlinie, y-positiv nach unten).
struct ShapedGlyph {
    id: u16,
    x: f32,
    y: f32,
}

struct ShapedLine {
    glyphs: Vec<ShapedGlyph>,
    width: f64,
    /// Byte-Bereich der Zeile in `spec.text` (ohne das '\n').
    byte_range: (usize, usize),
    /// Caret-Stopps: (Byte-Index in `spec.text`, x relativ zum Zeilenanfang),
    /// aufsteigend — Grundlage für Klick-Positionierung und Caret-Zeichnung.
    carets: Vec<(usize, f64)>,
}

/// Layout einer Zeile in Framepixeln (für Editor-Overlays im Monitor).
pub struct LineLayout {
    pub byte_range: (usize, usize),
    /// Zeilenrechteck (x, y, w, h) in Framepixeln.
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub carets: Vec<(usize, f64)>,
}

/// Layout des gesamten Textblocks in Framepixeln.
pub struct TitleLayout {
    /// Textblock (ohne Box-Polster): x, y, w, h.
    pub block: (f64, f64, f64, f64),
    /// Box-Rechteck (mit Polster), falls aktiviert.
    pub box_rect: Option<(f64, f64, f64, f64)>,
    pub lines: Vec<LineLayout>,
    pub line_height: f64,
}

impl TitleLayout {
    /// Nächster Caret-Stopp (Byte-Index) zu einem Punkt in Framepixeln.
    pub fn caret_at(&self, x: f64, y: f64) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        let line = self
            .lines
            .iter()
            .min_by(|a, b| {
                let da = (y - (a.y + a.h / 2.0)).abs();
                let db = (y - (b.y + b.h / 2.0)).abs();
                da.total_cmp(&db)
            })
            .expect("mindestens eine Zeile");
        line.carets
            .iter()
            .min_by(|a, b| (x - (line.x + a.1)).abs().total_cmp(&(x - (line.x + b.1)).abs()))
            .map(|(idx, _)| *idx)
            .unwrap_or(line.byte_range.0)
    }

    /// Caret-Position (x, y_oben, höhe) eines Byte-Index in Framepixeln.
    pub fn caret_pos(&self, byte_idx: usize) -> Option<(f64, f64, f64)> {
        for line in &self.lines {
            if byte_idx >= line.byte_range.0 && byte_idx <= line.byte_range.1 {
                let x = line
                    .carets
                    .iter()
                    .find(|(idx, _)| *idx == byte_idx)
                    .map(|(_, x)| *x)
                    .unwrap_or_else(|| {
                        // Innerhalb eines Clusters (Ligatur): nächster Stopp.
                        line.carets
                            .iter()
                            .filter(|(idx, _)| *idx <= byte_idx)
                            .last()
                            .map(|(_, x)| *x)
                            .unwrap_or(0.0)
                    });
                return Some((line.x + x, line.y, line.h));
            }
        }
        None
    }
}

/// Geometrie eines Specs bei gegebener Framegröße: Skalierung, Zeilen,
/// Blockmaße. Gemeinsame Grundlage von Layout und Raster.
struct BlockGeometry {
    scale: f64,
    size_px: f32,
    line_h: f64,
    /// Grundlinien-Abstand von der Blockoberkante (erste Zeile);
    /// der Descent steckt bereits in `block_h`.
    ascent: f64,
    lines: Vec<ShapedLine>,
    block_w: f64,
    block_h: f64,
    block_x: f64,
    block_y: f64,
}

fn shape_block(spec: &TitleSpec, frame_w: u32, frame_h: u32) -> Option<BlockGeometry> {
    let font_data = font_for(&spec.font_family, spec.weight, spec.italic)?;
    let font = font_data.font_ref()?;
    let scale = frame_h as f64 / REF_HEIGHT;
    let size_px = (spec.size * scale).max(1.0) as f32;
    let tracking = (spec.letter_spacing * scale) as f32;

    let metrics = font.metrics(&[]).scale(size_px);
    let ascent = metrics.ascent as f64;
    let descent = metrics.descent as f64;
    let line_h = (size_px as f64 * spec.line_spacing).max(1.0);

    let mut shape_ctx = ShapeContext::new();
    let mut lines: Vec<ShapedLine> = Vec::new();
    let mut byte_cursor = 0usize;
    // `str::lines()` verschluckt eine leere Schlusszeile — split('\n') nicht.
    for raw_line in spec.text.split('\n') {
        let start = byte_cursor;
        let end = start + raw_line.len();
        byte_cursor = end + 1; // + '\n'

        let mut glyphs: Vec<ShapedGlyph> = Vec::new();
        let mut carets: Vec<(usize, f64)> = vec![(start, 0.0)];
        let mut pen_x = 0f64;
        if !raw_line.is_empty() {
            let mut shaper = shape_ctx.builder(font).size(size_px).build();
            shaper.add_str(raw_line);
            shaper.shape_with(|cluster| {
                for g in cluster.glyphs {
                    glyphs.push(ShapedGlyph {
                        id: g.id,
                        x: pen_x as f32 + g.x,
                        // swash-Offsets sind y-aufwärts; Raster ist y-abwärts.
                        y: -g.y,
                    });
                    pen_x += g.advance as f64;
                }
                if !cluster.glyphs.is_empty() {
                    pen_x += tracking as f64;
                }
                let cluster_end = start + cluster.source.end as usize;
                carets.push((cluster_end, pen_x));
            });
            // Tracking hinter dem letzten Cluster gehört nicht zur Breite.
            if pen_x >= tracking as f64 && tracking != 0.0 && !glyphs.is_empty() {
                pen_x -= tracking as f64;
                if let Some(last) = carets.last_mut() {
                    last.1 = pen_x;
                }
            }
        }
        carets.dedup_by_key(|(idx, _)| *idx);
        lines.push(ShapedLine {
            glyphs,
            width: pen_x,
            byte_range: (start, end),
            carets,
        });
    }

    let block_w = lines.iter().map(|l| l.width).fold(0.0, f64::max);
    let n = lines.len().max(1) as f64;
    let block_h = (n - 1.0) * line_h + ascent + descent;
    let cx = frame_w as f64 / 2.0 + spec.pos_x / 100.0 * frame_w as f64;
    let cy = frame_h as f64 / 2.0 + spec.pos_y / 100.0 * frame_h as f64;

    Some(BlockGeometry {
        scale,
        size_px,
        line_h,
        ascent,
        lines,
        block_w,
        block_h,
        block_x: cx - block_w / 2.0,
        block_y: cy - block_h / 2.0,
    })
}

fn line_x_offset(align: TitleAlign, block_w: f64, line_w: f64) -> f64 {
    match align {
        TitleAlign::Left => 0.0,
        TitleAlign::Center => (block_w - line_w) / 2.0,
        TitleAlign::Right => block_w - line_w,
    }
}

/// Layout (Block, Zeilen, Caret-Stopps) in Framepixeln — für die
/// Textbearbeitung direkt im Programmmonitor.
pub fn layout_title(spec: &TitleSpec, frame_w: u32, frame_h: u32) -> Option<TitleLayout> {
    let geo = shape_block(spec, frame_w, frame_h)?;
    let lines = geo
        .lines
        .iter()
        .enumerate()
        .map(|(i, line)| LineLayout {
            byte_range: line.byte_range,
            x: geo.block_x + line_x_offset(spec.align, geo.block_w, line.width),
            y: geo.block_y + i as f64 * geo.line_h,
            w: line.width,
            // Slot-Höhe (nicht ascent+descent): Zeilen überlappen sich so
            // nie — Klick-Zuordnung und Caret-Höhe bleiben eindeutig.
            h: geo.line_h,
            carets: line.carets.clone(),
        })
        .collect();
    let box_rect = spec.bg.enabled.then(|| {
        let px = spec.bg.pad_x * geo.scale;
        let py = spec.bg.pad_y * geo.scale;
        (
            geo.block_x - px,
            geo.block_y - py,
            geo.block_w + 2.0 * px,
            geo.block_h + 2.0 * py,
        )
    });
    Some(TitleLayout {
        block: (geo.block_x, geo.block_y, geo.block_w, geo.block_h),
        box_rect,
        lines,
        line_height: geo.line_h,
    })
}

// ================================================================== Raster

/// Gerasterter Titel: RGBA (straight alpha). Repräsentiert das volle Frame —
/// bzw. bei `extend_k > 1` ein vertikal symmetrisch erweitertes Frame
/// (Höhe = `extend_k` × Framehöhe, Framezentrum bleibt Pufferzentrum).
/// Damit passen Textblöcke, die höher als das Frame sind (Abspann-Rolle),
/// vollständig in den Raster; beide Render-Pfade strecken das Layer-Quad
/// um denselben Faktor (`quad.h *= extend_k`).
pub struct TitleRaster {
    pub w: usize,
    pub h: usize,
    /// Vertikaler Erweiterungsfaktor (ungerade, ≥ 1).
    pub extend_k: u32,
    pub data: Vec<u8>,
}

/// Alphamaske eines Block-Ausschnitts (lokales Koordinatensystem).
struct AlphaMask {
    w: usize,
    h: usize,
    data: Vec<u8>,
}

impl AlphaMask {
    fn new(w: usize, h: usize) -> AlphaMask {
        AlphaMask {
            w,
            h,
            data: vec![0; w * h],
        }
    }

    /// Glyph-Image (Alpha) mit Max-Verknüpfung einblenden.
    fn blit_max(&mut self, src: &[u8], sw: usize, sh: usize, dx: i32, dy: i32) {
        for sy in 0..sh {
            let ty = dy + sy as i32;
            if ty < 0 || ty >= self.h as i32 {
                continue;
            }
            for sx in 0..sw {
                let tx = dx + sx as i32;
                if tx < 0 || tx >= self.w as i32 {
                    continue;
                }
                let v = src[sy * sw + sx];
                let d = &mut self.data[ty as usize * self.w + tx as usize];
                *d = (*d).max(v);
            }
        }
    }
}

/// Separierter Box-Blur in 3 Pässen (≈ Gauß); `radius` in Pixeln.
fn box_blur(mask: &mut AlphaMask, radius: f64) {
    let r = (radius / 2.0).round() as i32;
    if r <= 0 {
        return;
    }
    let (w, h) = (mask.w as i32, mask.h as i32);
    let mut tmp = vec![0u8; mask.data.len()];
    for _ in 0..3 {
        // horizontal: gleitende Summe
        for y in 0..h {
            let row = &mask.data[(y * w) as usize..((y + 1) * w) as usize];
            let mut sum: u32 = 0;
            for x in -r..=r {
                sum += row[x.clamp(0, w - 1) as usize] as u32;
            }
            let norm = (2 * r + 1) as u32;
            for x in 0..w {
                tmp[(y * w + x) as usize] = (sum / norm) as u8;
                let add = row[(x + r + 1).clamp(0, w - 1) as usize] as u32;
                let sub = row[(x - r).clamp(0, w - 1) as usize] as u32;
                sum = sum + add - sub;
            }
        }
        // vertikal
        for x in 0..w {
            let mut sum: u32 = 0;
            for y in -r..=r {
                sum += tmp[(y.clamp(0, h - 1) * w + x) as usize] as u32;
            }
            let norm = (2 * r + 1) as u32;
            for y in 0..h {
                mask.data[(y * w + x) as usize] = (sum / norm) as u8;
                let add = tmp[((y + r + 1).clamp(0, h - 1) * w + x) as usize] as u32;
                let sub = tmp[((y - r).clamp(0, h - 1) * w + x) as usize] as u32;
                sum = sum + add - sub;
            }
        }
    }
}

/// Abgerundetes Rechteck als Alphamaske (SDF-Kante, 1 px Antialiasing).
fn rounded_rect_mask(mask: &mut AlphaMask, x: f64, y: f64, w: f64, h: f64, radius: f64) {
    let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    let (cx0, cy0) = (x + r, y + r);
    let (cx1, cy1) = (x + w - r, y + h - r);
    let x_start = (x.floor() - 1.0).max(0.0) as usize;
    let y_start = (y.floor() - 1.0).max(0.0) as usize;
    let x_end = ((x + w).ceil() + 1.0).min(mask.w as f64) as usize;
    let y_end = ((y + h).ceil() + 1.0).min(mask.h as f64) as usize;
    for py in y_start..y_end {
        for px in x_start..x_end {
            let sx = px as f64 + 0.5;
            let sy = py as f64 + 0.5;
            // Signierte Distanz zum abgerundeten Rechteck.
            let qx = (sx.clamp(cx0, cx1) - sx).abs();
            let qy = (sy.clamp(cy0, cy1) - sy).abs();
            let dist = (qx * qx + qy * qy).sqrt() - r;
            let cov = (0.5 - dist).clamp(0.0, 1.0);
            let v = (cov * 255.0).round() as u8;
            let d = &mut mask.data[py * mask.w + px];
            *d = (*d).max(v);
        }
    }
}

/// Premultiplied-Over: `src` (premultipliziert) über `dst` (premultipliziert).
#[inline]
fn over(dst: &mut [f32; 4], src: [f32; 4]) {
    let ia = 1.0 - src[3];
    for c in 0..4 {
        dst[c] = src[c] + dst[c] * ia;
    }
}

/// Eine Alphamaske eingefärbt auf den Premul-Puffer mischen.
fn composite_mask(
    buf: &mut [[f32; 4]],
    bw: usize,
    mask: &AlphaMask,
    offset: (i32, i32),
    color: crate::core::title::RgbaColor,
    opacity: f64,
) {
    let (r, g, b) = (
        color.r as f32 / 255.0,
        color.g as f32 / 255.0,
        color.b as f32 / 255.0,
    );
    let base_a = color.a as f32 / 255.0 * opacity.clamp(0.0, 1.0) as f32;
    if base_a <= 0.0 {
        return;
    }
    let bh = buf.len() / bw;
    for my in 0..mask.h {
        let ty = my as i32 + offset.1;
        if ty < 0 || ty >= bh as i32 {
            continue;
        }
        for mx in 0..mask.w {
            let tx = mx as i32 + offset.0;
            if tx < 0 || tx >= bw as i32 {
                continue;
            }
            let m = mask.data[my * mask.w + mx];
            if m == 0 {
                continue;
            }
            let a = m as f32 / 255.0 * base_a;
            over(
                &mut buf[ty as usize * bw + tx as usize],
                [r * a, g * a, b * a, a],
            );
        }
    }
}

/// Titel in ein volles Frame rastern (RGBA, straight alpha). `frame_w/h`
/// bestimmen die Zielauflösung — Monitor übergibt die Canvas-, der Export
/// die (ggf. überabgetastete) Zielauflösung.
pub fn render_title(spec: &TitleSpec, frame_w: u32, frame_h: u32) -> TitleRaster {
    let empty = |k: u32| TitleRaster {
        w: frame_w.max(1) as usize,
        h: (frame_h.max(1) * k) as usize,
        extend_k: k,
        data: vec![0u8; (frame_w.max(1) * frame_h.max(1) * k) as usize * 4],
    };
    let Some(geo) = shape_block(spec, frame_w, frame_h) else {
        return empty(1);
    };
    let has_text = geo.lines.iter().any(|l| !l.glyphs.is_empty());
    if !has_text && !spec.bg.enabled {
        return empty(1);
    }

    let stroke_px = (spec.stroke_width * geo.scale).max(0.0);
    let blur_px = if spec.shadow.enabled {
        (spec.shadow.blur * geo.scale).max(0.0)
    } else {
        0.0
    };
    let shadow_dx = spec.shadow.dx * geo.scale;
    let shadow_dy = spec.shadow.dy * geo.scale;
    let pad_x = if spec.bg.enabled { spec.bg.pad_x * geo.scale } else { 0.0 };
    let pad_y = if spec.bg.enabled { spec.bg.pad_y * geo.scale } else { 0.0 };

    // Arbeitsbereich: Block + Polster + Kontur + Schattenreichweite.
    let margin = (stroke_px / 2.0
        + blur_px * 1.5
        + shadow_dx.abs().max(shadow_dy.abs())
        + pad_x.max(pad_y)
        + 2.0)
        .ceil();
    let local_x0 = geo.block_x - margin;
    let local_y0 = geo.block_y - margin;
    let lw = (geo.block_w + 2.0 * margin).ceil().max(1.0) as usize;
    let lh = (geo.block_h + 2.0 * margin).ceil().max(1.0) as usize;

    // Vertikale Erweiterung: ragt der Block über das Frame hinaus
    // (Abspann-Rolle), wächst der Raster symmetrisch in Framehöhen-
    // Schritten, statt den Text abzuschneiden.
    let fh = frame_h.max(1) as f64;
    let overflow = (-local_y0)
        .max(local_y0 + lh as f64 - fh)
        .max(0.0);
    let mut k = 1 + 2 * (overflow / fh).ceil() as u32;
    while k > 1 && fh * k as f64 > 8192.0 {
        k -= 2;
    }
    let mut out = empty(k);
    // Versatz Framekoordinaten → erweiterte Pufferkoordinaten.
    let y_off = ((k - 1) / 2) as f64 * fh;

    // ---- Glyph-Masken (Füllung + Kontur) ----
    let mut fill_mask = AlphaMask::new(lw, lh);
    let mut stroke_mask = (stroke_px > 0.01).then(|| AlphaMask::new(lw, lh));
    if has_text {
        let font_data = font_for(&spec.font_family, spec.weight, spec.italic)
            .expect("Font war beim Shaping auflösbar");
        let font = font_data.font_ref().expect("FontRef war beim Shaping gültig");
        let mut scale_ctx = ScaleContext::new();
        let mut scaler = scale_ctx
            .builder(font)
            .size(geo.size_px)
            .hint(false)
            .build();
        let sources = [
            Source::ColorOutline(0),
            Source::Outline,
            Source::Bitmap(StrikeWith::BestFit),
        ];
        for (i, line) in geo.lines.iter().enumerate() {
            let line_x = margin + line_x_offset(spec.align, geo.block_w, line.width);
            let baseline = margin + i as f64 * geo.line_h + geo.ascent;
            for glyph in &line.glyphs {
                let gx = line_x + glyph.x as f64;
                let gy = baseline + glyph.y as f64;
                let (ix, iy) = (gx.floor(), gy.floor());
                let offset = zeno::Vector::new((gx - ix) as f32, (gy - iy) as f32);
                // Füllung
                if let Some(img) = Render::new(&sources)
                    .offset(offset)
                    .render(&mut scaler, glyph.id)
                {
                    let p = img.placement;
                    fill_mask.blit_max(
                        &img.data,
                        p.width as usize,
                        p.height as usize,
                        ix as i32 + p.left,
                        iy as i32 - p.top,
                    );
                }
                // Kontur (runde Joins/Caps — keine Spitzen-Artefakte)
                if let Some(mask) = stroke_mask.as_mut() {
                    let mut stroke = zeno::Stroke::new(stroke_px as f32);
                    stroke.cap(zeno::Cap::Round).join(zeno::Join::Round);
                    if let Some(img) = Render::new(&sources)
                        .style(zeno::Style::Stroke(stroke))
                        .offset(offset)
                        .render(&mut scaler, glyph.id)
                    {
                        let p = img.placement;
                        mask.blit_max(
                            &img.data,
                            p.width as usize,
                            p.height as usize,
                            ix as i32 + p.left,
                            iy as i32 - p.top,
                        );
                    }
                }
            }
        }
    }

    // ---- Schattenmaske: Text + Kontur, weichgezeichnet ----
    let shadow_mask = (spec.shadow.enabled && spec.shadow.opacity > 0.0 && has_text).then(|| {
        let mut m = AlphaMask::new(lw, lh);
        m.data.copy_from_slice(&fill_mask.data);
        if let Some(stroke) = &stroke_mask {
            for (d, s) in m.data.iter_mut().zip(stroke.data.iter()) {
                *d = (*d).max(*s);
            }
        }
        box_blur(&mut m, blur_px);
        m
    });

    // ---- Premultiplied-Komposition: Box → Schatten → Kontur → Füllung ----
    let mut buf = vec![[0f32; 4]; lw * lh];
    if spec.bg.enabled && spec.bg.opacity > 0.0 && has_text {
        let mut box_mask = AlphaMask::new(lw, lh);
        rounded_rect_mask(
            &mut box_mask,
            margin - pad_x,
            margin - pad_y,
            geo.block_w + 2.0 * pad_x,
            geo.block_h + 2.0 * pad_y,
            spec.bg.radius * geo.scale,
        );
        composite_mask(&mut buf, lw, &box_mask, (0, 0), spec.bg.color, spec.bg.opacity / 100.0);
    }
    if let Some(shadow) = &shadow_mask {
        composite_mask(
            &mut buf,
            lw,
            shadow,
            (shadow_dx.round() as i32, shadow_dy.round() as i32),
            spec.shadow.color,
            spec.shadow.opacity / 100.0,
        );
    }
    if let Some(stroke) = &stroke_mask {
        composite_mask(&mut buf, lw, stroke, (0, 0), spec.stroke_color, 1.0);
    }
    composite_mask(&mut buf, lw, &fill_mask, (0, 0), spec.fill, 1.0);

    // ---- In das (ggf. erweiterte) Frame schreiben (straight alpha) ----
    let fx0 = local_x0.floor() as i64;
    let fy0 = (local_y0 + y_off).floor() as i64;
    for ly in 0..lh {
        let ty = fy0 + ly as i64;
        if ty < 0 || ty >= out.h as i64 {
            continue;
        }
        for lx in 0..lw {
            let tx = fx0 + lx as i64;
            if tx < 0 || tx >= out.w as i64 {
                continue;
            }
            let px = buf[ly * lw + lx];
            let a = px[3].clamp(0.0, 1.0);
            if a <= 0.0 {
                continue;
            }
            let i = (ty as usize * out.w + tx as usize) * 4;
            out.data[i] = (px[0] / a * 255.0).round().clamp(0.0, 255.0) as u8;
            out.data[i + 1] = (px[1] / a * 255.0).round().clamp(0.0, 255.0) as u8;
            out.data[i + 2] = (px[2] / a * 255.0).round().clamp(0.0, 255.0) as u8;
            out.data[i + 3] = (a * 255.0).round() as u8;
        }
    }
    // Farb-Dilatation: A=0-Nachbarn von Kantenpixeln erben deren Farbe —
    // bilineares Sampling (GPU wie CPU-Compositor) zieht sonst Schwarz
    // in die Kante („dunkler Saum“).
    dilate_rgb(&mut out);
    out
}

/// Eine Dilatations-Iteration: vollständig transparente Pixel übernehmen
/// die Farbe des deckendsten Nachbarn (Alpha bleibt 0).
fn dilate_rgb(raster: &mut TitleRaster) {
    let (w, h) = (raster.w, raster.h);
    let src = raster.data.clone();
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            if src[i + 3] != 0 {
                continue;
            }
            let mut best_a = 0u8;
            let mut best_rgb = [0u8; 3];
            for (dx, dy) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
                let nx = x as i64 + dx;
                let ny = y as i64 + dy;
                if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                    continue;
                }
                let n = (ny as usize * w + nx as usize) * 4;
                if src[n + 3] > best_a {
                    best_a = src[n + 3];
                    best_rgb = [src[n], src[n + 1], src[n + 2]];
                }
            }
            if best_a > 0 {
                raster.data[i..i + 3].copy_from_slice(&best_rgb);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::title::{TitleSpec, TitleTemplate};

    fn alpha_sum(r: &TitleRaster) -> u64 {
        r.data.chunks_exact(4).map(|p| p[3] as u64).sum()
    }

    #[test]
    fn fontconfig_resolves_a_font() {
        let font = font_for("", TitleWeight::Regular, false);
        assert!(font.is_some(), "Plattform ohne installierte Schriften?");
        assert!(!list_font_families().is_empty());
    }

    #[test]
    fn renders_text_pixels_near_block_center() {
        let spec = TitleSpec::default();
        let r = render_title(&spec, 1280, 720);
        assert_eq!((r.w, r.h), (1280, 720));
        assert!(alpha_sum(&r) > 0, "Titel muss Pixel erzeugen");
        // Alphagewichteter Schwerpunkt liegt nahe der Framemitte.
        let (mut sx, mut sy, mut sa) = (0f64, 0f64, 0f64);
        for y in 0..r.h {
            for x in 0..r.w {
                let a = r.data[(y * r.w + x) * 4 + 3] as f64;
                sx += x as f64 * a;
                sy += y as f64 * a;
                sa += a;
            }
        }
        let (cx, cy) = (sx / sa, sy / sa);
        assert!((cx - 640.0).abs() < 30.0, "Schwerpunkt x = {cx}");
        assert!((cy - 360.0).abs() < 30.0, "Schwerpunkt y = {cy}");
    }

    #[test]
    fn empty_text_renders_nothing() {
        let mut spec = TitleSpec::default();
        spec.text = String::new();
        spec.bg.enabled = true;
        let r = render_title(&spec, 320, 180);
        assert_eq!(alpha_sum(&r), 0, "ohne Text auch keine Box");
    }

    #[test]
    fn stroke_box_and_shadow_add_coverage() {
        let mut spec = TitleSpec::default();
        spec.shadow.enabled = false;
        let plain = alpha_sum(&render_title(&spec, 640, 360));

        let mut with_stroke = spec.clone();
        with_stroke.stroke_width = 6.0;
        assert!(alpha_sum(&render_title(&with_stroke, 640, 360)) > plain);

        let mut with_box = spec.clone();
        with_box.bg.enabled = true;
        assert!(alpha_sum(&render_title(&with_box, 640, 360)) > plain);

        let mut with_shadow = spec.clone();
        with_shadow.shadow.enabled = true;
        with_shadow.shadow.dy = 10.0;
        assert!(alpha_sum(&render_title(&with_shadow, 640, 360)) > plain);
    }

    /// Paritätstest Monitor/Export: derselbe Spec, in zwei Auflösungen
    /// gerastert, muss nach Downsampling übereinstimmen (Toleranz für
    /// Antialiasing). Genau so unterscheiden sich Programmmonitor
    /// (Canvas-Auflösung) und Export (Zielauflösung).
    #[test]
    fn raster_is_resolution_consistent() {
        let mut spec = TitleTemplate::LowerThird.build();
        spec.text = "Parität\nMonitor / Export".into();
        let lo = render_title(&spec, 640, 360);
        let hi = render_title(&spec, 1280, 720);
        let mut total_diff = 0f64;
        let mut count = 0f64;
        for y in 0..lo.h {
            for x in 0..lo.w {
                // 2×2-Boxfilter des hochauflösenden Rasters.
                let mut acc = [0f64; 4];
                for (ox, oy) in [(0usize, 0usize), (1, 0), (0, 1), (1, 1)] {
                    let i = ((y * 2 + oy) * hi.w + x * 2 + ox) * 4;
                    for c in 0..4 {
                        acc[c] += hi.data[i + c] as f64 / 4.0;
                    }
                }
                let i = (y * lo.w + x) * 4;
                // Alphagewichteter Farbvergleich + direkter Alphavergleich.
                let a_lo = lo.data[i + 3] as f64;
                total_diff += (acc[3] - a_lo).abs();
                count += 1.0;
            }
        }
        let mean = total_diff / count;
        assert!(mean < 2.0, "mittlere Alpha-Abweichung zu groß: {mean}");
    }

    #[test]
    fn layout_carets_are_monotonic_and_clickable() {
        let mut spec = TitleSpec::default();
        spec.text = "Zeile Eins\nZwei".into();
        let layout = layout_title(&spec, 1280, 720).expect("Layout");
        assert_eq!(layout.lines.len(), 2);
        for line in &layout.lines {
            assert!(line.carets.windows(2).all(|w| w[0].0 < w[1].0));
            assert!(line.carets.windows(2).all(|w| w[0].1 <= w[1].1));
        }
        // Klick weit links in Zeile 1 → Caret am Zeilenanfang.
        let l0 = &layout.lines[0];
        assert_eq!(layout.caret_at(l0.x - 100.0, l0.y + 2.0), 0);
        // Klick weit rechts in Zeile 2 → Caret am Zeilenende (Byte-Index).
        let l1 = &layout.lines[1];
        assert_eq!(
            layout.caret_at(l1.x + l1.w + 100.0, l1.y + 2.0),
            spec.text.len()
        );
        // Caret-Positionen sind für jeden Stopp abrufbar.
        assert!(layout.caret_pos(0).is_some());
        assert!(layout.caret_pos(spec.text.len()).is_some());
    }

    #[test]
    fn kerning_tightens_pairs() {
        // „AV“ hat in praktisch jedem Sans negatives Kerning — die Paar-
        // Breite muss kleiner sein als die Summe der Einzelbreiten.
        let mut spec = TitleSpec::default();
        spec.size = 200.0;
        spec.text = "AV".into();
        let pair = layout_title(&spec, 1920, 1080).unwrap().block.2;
        spec.text = "A".into();
        let a = layout_title(&spec, 1920, 1080).unwrap().block.2;
        spec.text = "V".into();
        let v = layout_title(&spec, 1920, 1080).unwrap().block.2;
        assert!(
            pair < a + v + 0.5,
            "Kerning fehlt: AV = {pair}, A+V = {}",
            a + v
        );
    }
}

#[cfg(test)]
mod visual_dump {
    use super::*;
    use crate::core::title::TitleTemplate;

    #[test]
    #[ignore]
    fn dump_ppm() {
        let mut spec = TitleTemplate::LowerThird.build();
        spec.text = "Maria Václav-Größe AVATAR\nReporterin · Tagesthemen".into();
        let r = render_title(&spec, 1920, 1080);
        // Über dunkelgrauem Hintergrund komponieren (straight alpha over).
        let mut out = vec![0u8; r.w * r.h * 3];
        for i in 0..r.w * r.h {
            let a = r.data[i * 4 + 3] as f32 / 255.0;
            for c in 0..3 {
                let src = r.data[i * 4 + c] as f32;
                let bg = if ((i / r.w / 60) + (i % r.w / 60)) % 2 == 0 { 70.0 } else { 110.0 };
                out[i * 3 + c] = (src * a + bg * (1.0 - a)) as u8;
            }
        }
        let mut f = std::fs::File::create("/tmp/title_dump.ppm").unwrap();
        use std::io::Write;
        write!(f, "P6\n{} {}\n255\n", r.w, r.h).unwrap();
        f.write_all(&out).unwrap();
    }
    /// Export-Pfad-Probe: der Raster läuft als CpuLayerFrame durch den
    /// CPU-Compositor (straight alpha, Src-over) — weiße Titelpixel müssen
    /// auf dem opaken schwarzen Canvas ankommen.
    #[test]
    fn raster_composites_over_video_frame() {
        use crate::core::animation::ClipFx;
        use crate::core::compose;
        let mut spec = TitleSpec::default();
        spec.shadow.enabled = false;
        spec.size = 200.0;
        spec.text = "X".into();
        let (w, h) = (160usize, 90usize);
        let r = render_title(&spec, w as u32, h as u32);
        assert_eq!(r.extend_k, 1);
        let mut canvas = vec![0u8; w * h * 4];
        for px in canvas.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let fx = compose::eval_fx(&ClipFx::default(), 0.0);
        let quad = compose::layer_quad(w as f64, h as f64, w as f64, h as f64, &fx);
        compose::composite_frame(
            &mut canvas,
            w,
            h,
            &[compose::CpuLayerFrame {
                data: &r.data,
                w: r.w,
                h: r.h,
                quad,
                opacity: 1.0,
                mask: None,
            }],
            2,
        );
        let max_lum = canvas
            .chunks_exact(4)
            .map(|p| p[0] as u16 + p[1] as u16 + p[2] as u16)
            .max()
            .unwrap_or(0);
        assert!(max_lum > 700, "weiße Titelpixel fehlen: max = {max_lum}");
    }

    /// Abspann-Rolle: Blöcke höher als das Frame erweitern den Raster
    /// symmetrisch (extend_k ungerade), statt Text abzuschneiden.
    #[test]
    fn tall_credit_roll_extends_raster() {
        let mut spec = TitleTemplate::CreditRoll.build();
        spec.size = 60.0;
        spec.text = (0..40)
            .map(|i| format!("Zeile {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let r = render_title(&spec, 320, 180);
        assert!(r.extend_k >= 3, "extend_k = {}", r.extend_k);
        assert_eq!(r.extend_k % 2, 1, "symmetrische Erweiterung");
        assert_eq!(r.h, 180 * r.extend_k as usize);
        // Auch außerhalb des Kernframes liegen Textpixel.
        let core_top = (r.extend_k as usize / 2) * 180;
        let above: u64 = r.data[..core_top * r.w * 4]
            .chunks_exact(4)
            .map(|p| p[3] as u64)
            .sum();
        assert!(above > 0, "erweiterter Bereich muss Text tragen");
    }
}

