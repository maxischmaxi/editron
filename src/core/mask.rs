//! Geometrische Masken: begrenzen einen Effekt (oder Grade) auf eine Region
//! des Bildes. Eine [`Mask`] beschreibt eine Ellipse, ein (abgerundetes)
//! Rechteck oder ein Polygon in normierten Inhalts-UVs (0..1), mit weicher
//! Kante (Feather), Invertierung und Deckkraft. Mehrere Masken eines Effekts
//! werden per Vereinigung (probabilistisches ODER) kombiniert.
//!
//! Die Maske moduliert die Effekt-Anwendung über `mix(vorher, nachher, m)`:
//! `m=1` ⇒ voller Effekt, `m=0` ⇒ Originalpixel. Das passiert ZWEIMAL mit
//! formelgleichen Funktionen — im CPU-Pfad (`core/effects::apply_effects_buffer`,
//! Export/Scopes/Tests) und im GPU-Shader (`ui/fx_shader`, Echtzeitvorschau).
//! Die Signed-Distance-Funktionen unten und die GLSL-Quelle [`MASK_GLSL`]
//! MÜSSEN deshalb Zeichen für Zeichen dieselbe Mathematik tragen.

use crate::core::types::new_id;
use serde::{Deserialize, Serialize};

/// Maximale Maskenanzahl je Effekt im GPU-Shader (Uniform-Array-Größe). Der
/// CPU-Pfad ist unbegrenzt, deckelt aber identisch, damit Vorschau == Export.
pub const MAX_MASKS: usize = 8;
/// Gemeinsamer Polygon-Stützpunkt-Pool im Shader (alle Polygon-Masken zusammen).
pub const MAX_POLY_PTS: usize = 64;

/// Form einer Maske.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MaskShape {
    Ellipse,
    Rectangle,
    Polygon,
}

impl MaskShape {
    pub fn label(&self) -> &'static str {
        match self {
            MaskShape::Ellipse => "Ellipse",
            MaskShape::Rectangle => "Rechteck",
            MaskShape::Polygon => "Polygon",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            MaskShape::Ellipse => "focus",
            MaskShape::Rectangle => "crop",
            MaskShape::Polygon => "layers",
        }
    }

    /// camelCase-Schlüssel (Commands, Test-Flag).
    pub fn key(&self) -> &'static str {
        match self {
            MaskShape::Ellipse => "ellipse",
            MaskShape::Rectangle => "rectangle",
            MaskShape::Polygon => "polygon",
        }
    }

    pub const ALL: [MaskShape; 3] = [MaskShape::Ellipse, MaskShape::Rectangle, MaskShape::Polygon];
}

fn default_true() -> bool {
    true
}
fn default_opacity() -> f32 {
    1.0
}

/// Eine geometrische Maske eines Effekts. Alle Ortsangaben sind in normierten
/// Inhalts-UVs (0..1, auflösungsunabhängig wie die Effekte selbst).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mask {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub shape: MaskShape,
    /// Mittelpunkt (Ellipse/Rechteck). Beim Polygon der Drehpunkt für „Maske
    /// verschieben“ — die Stützpunkte selbst liegen in `points`.
    pub center: [f32; 2],
    /// Halbe Ausdehnung: Radien (Ellipse) bzw. halbe Kantenlängen (Rechteck).
    pub radius: [f32; 2],
    /// Rotation in Grad (Ellipse/Rechteck).
    #[serde(default)]
    pub rotation: f32,
    /// Eckenradius des Rechtecks in UV (0 = scharfe Ecken).
    #[serde(default)]
    pub corner: f32,
    /// Polygon-Stützpunkte in UV (nur `MaskShape::Polygon`).
    #[serde(default)]
    pub points: Vec<[f32; 2]>,
    /// Weiche Kante in UV (Rampenbreite um die Kante; 0 = harte Kante).
    #[serde(default)]
    pub feather: f32,
    /// Maske invertieren (Effekt außerhalb statt innerhalb der Form).
    #[serde(default)]
    pub inverted: bool,
    /// Maskenstärke 0..1 (skaliert die Deckung — partielle Anwendung).
    #[serde(default = "default_opacity")]
    pub opacity: f32,
}

impl Mask {
    /// Neue Maske der angegebenen Form, mittig auf dem Bild.
    pub fn new(shape: MaskShape) -> Mask {
        let points = if shape == MaskShape::Polygon {
            // Standard-Vierecks-Polygon (Raute) um die Mitte.
            vec![[0.5, 0.22], [0.78, 0.5], [0.5, 0.78], [0.22, 0.5]]
        } else {
            Vec::new()
        };
        Mask {
            id: new_id(),
            enabled: true,
            shape,
            center: [0.5, 0.5],
            radius: [0.28, 0.28],
            rotation: 0.0,
            corner: 0.0,
            points,
            feather: 0.06,
            inverted: false,
            opacity: 1.0,
        }
    }

    /// Punkt in lokale (entrotierte) Koordinaten relativ zum Zentrum bringen
    /// (formelgleich zu `ui::fx_shader` und `panels::transform_gizmo::to_local`).
    #[inline]
    fn to_local(&self, u: f32, v: f32) -> (f32, f32) {
        let (cx, cy) = (self.center[0], self.center[1]);
        let (dx, dy) = (u - cx, v - cy);
        let a = self.rotation.to_radians();
        let (s, c) = (a.sin(), a.cos());
        (dx * c + dy * s, -dx * s + dy * c)
    }

    /// Vorzeichenbehafteter Abstand zur Maskenkante (negativ = innen),
    /// in UV-Einheiten. Formelgleich zur GLSL-Variante in [`MASK_GLSL`].
    pub fn signed_distance(&self, u: f32, v: f32) -> f32 {
        match self.shape {
            MaskShape::Ellipse => {
                let (lx, ly) = self.to_local(u, v);
                sd_ellipse(lx, ly, self.radius[0].max(1e-5), self.radius[1].max(1e-5))
            }
            MaskShape::Rectangle => {
                let (lx, ly) = self.to_local(u, v);
                sd_round_box(
                    lx,
                    ly,
                    self.radius[0].max(1e-5),
                    self.radius[1].max(1e-5),
                    self.corner.max(0.0),
                )
            }
            MaskShape::Polygon => sd_polygon(u, v, &self.points),
        }
    }

    /// Deckung dieser Maske an (u, v): 1 innen, über `feather` auf 0 abfallend,
    /// invertiert/skaliert nach `inverted`/`opacity`.
    pub fn coverage(&self, u: f32, v: f32) -> f32 {
        if self.shape == MaskShape::Polygon && self.points.len() < 3 {
            return 0.0;
        }
        let d = self.signed_distance(u, v);
        let mut c = coverage_from_sd(d, self.feather.max(0.0));
        if self.inverted {
            c = 1.0 - c;
        }
        c * self.opacity.clamp(0.0, 1.0)
    }

    /// Schwerpunkt der Maske (für „Maske verschieben“-Gizmo des Polygons).
    pub fn centroid(&self) -> [f32; 2] {
        if self.shape == MaskShape::Polygon && !self.points.is_empty() {
            let (mut sx, mut sy) = (0.0f32, 0.0f32);
            for p in &self.points {
                sx += p[0];
                sy += p[1];
            }
            let n = self.points.len() as f32;
            [sx / n, sy / n]
        } else {
            self.center
        }
    }
}

/// Glatter Übergang (identisch zu `core::effects::smoothstep` und GLSL `smoothstep`).
#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Deckung aus dem vorzeichenbehafteten Abstand: die Feather-Rampe straddelt
/// die Kante (bei `d=0` genau 0,5). `feather<=0` ⇒ harte Stufe.
#[inline]
pub fn coverage_from_sd(d: f32, feather: f32) -> f32 {
    if feather < 1e-6 {
        if d <= 0.0 {
            1.0
        } else {
            0.0
        }
    } else {
        1.0 - smoothstep(-feather * 0.5, feather * 0.5, d)
    }
}

/// Näherungsweiser Signed-Distance einer achsparallelen Ellipse (iq), negativ
/// innen. `ab` = Radien. Formelgleich in GLSL.
#[inline]
fn sd_ellipse(px: f32, py: f32, ax: f32, ay: f32) -> f32 {
    let k1 = ((px / ax).powi(2) + (py / ay).powi(2)).sqrt();
    if k1 < 1e-6 {
        // Im Zentrum: Abstand ≈ −kleinster Radius.
        return -ax.min(ay);
    }
    let k2 = ((px / (ax * ax)).powi(2) + (py / (ay * ay)).powi(2)).sqrt();
    k1 * (k1 - 1.0) / k2.max(1e-12)
}

/// Signed-Distance eines abgerundeten Rechtecks (iq), negativ innen.
#[inline]
fn sd_round_box(px: f32, py: f32, bx: f32, by: f32, r: f32) -> f32 {
    let r = r.min(bx.min(by));
    let qx = px.abs() - bx + r;
    let qy = py.abs() - by + r;
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let inside = qx.max(qy).min(0.0);
    outside + inside - r
}

/// Signed-Distance eines Polygons (iq), negativ innen. Punkte in UV.
#[inline]
fn sd_polygon(px: f32, py: f32, pts: &[[f32; 2]]) -> f32 {
    let n = pts.len();
    if n < 3 {
        return 1.0;
    }
    let mut d = {
        let dx = px - pts[0][0];
        let dy = py - pts[0][1];
        dx * dx + dy * dy
    };
    let mut s = 1.0f32;
    let mut j = n - 1;
    for i in 0..n {
        let (vix, viy) = (pts[i][0], pts[i][1]);
        let (vjx, vjy) = (pts[j][0], pts[j][1]);
        let (ex, ey) = (vjx - vix, vjy - viy);
        let (wx, wy) = (px - vix, py - viy);
        let t = ((wx * ex + wy * ey) / (ex * ex + ey * ey).max(1e-12)).clamp(0.0, 1.0);
        let (bx, by) = (wx - ex * t, wy - ey * t);
        d = d.min(bx * bx + by * by);
        let c0 = py >= viy;
        let c1 = py < vjy;
        let c2 = ex * wy > ey * wx;
        if (c0 == c1) && (c1 == c2) {
            s = -s;
        }
        j = i;
    }
    s * d.sqrt()
}

/// Kombinierte Deckung mehrerer Masken an (u, v): Vereinigung per
/// probabilistischem ODER (`a + b − a·b`, kommutativ + assoziativ). Leere
/// Liste ⇒ 0 (kein Effekt) — Aufrufer maskiert nur bei nicht-leerer Liste.
pub fn combined_mask(masks: &[Mask], u: f32, v: f32) -> f32 {
    let mut m = 0.0f32;
    // Auf MAX_MASKS deckeln (wie der GPU-Shader), damit Vorschau == Export
    // auch bei hand-editierten Dateien mit zu vielen Masken.
    for mk in masks.iter().filter(|m| m.enabled).take(MAX_MASKS) {
        let c = mk.coverage(u, v);
        m = m + c - m * c;
    }
    m.clamp(0.0, 1.0)
}

// ----------------------------------------------------------- GPU-Uniforms

/// In Uniform-Arrays gepackte Maskendaten für den Shader. `count` Masken sind
/// gültig; `pts` enthält die zusammengefassten Polygon-Stützpunkte.
#[derive(Clone, Debug)]
pub struct MaskUniforms {
    pub count: i32,
    /// cx, cy, rx, ry.
    pub a: [[f32; 4]; MAX_MASKS],
    /// cosR, sinR, corner, feather.
    pub b: [[f32; 4]; MAX_MASKS],
    /// invert(0/1), opacity, shape(0/1/2), polyOffset.
    pub c: [[f32; 4]; MAX_MASKS],
    /// polyCount, 0, 0, 0.
    pub d: [[f32; 4]; MAX_MASKS],
    pub pts: [[f32; 2]; MAX_POLY_PTS],
}

/// Maskenliste in die Shader-Uniform-Form bringen (deckelt auf [`MAX_MASKS`]
/// bzw. [`MAX_POLY_PTS`]).
pub fn pack_uniforms(masks: &[Mask]) -> MaskUniforms {
    let mut u = MaskUniforms {
        count: 0,
        a: [[0.0; 4]; MAX_MASKS],
        b: [[0.0; 4]; MAX_MASKS],
        c: [[0.0; 4]; MAX_MASKS],
        d: [[0.0; 4]; MAX_MASKS],
        pts: [[0.0; 2]; MAX_POLY_PTS],
    };
    let mut pt_off = 0usize;
    let mut n = 0usize;
    for mk in masks.iter().filter(|m| m.enabled) {
        if n >= MAX_MASKS {
            break;
        }
        let shape_f = match mk.shape {
            MaskShape::Ellipse => 0.0,
            MaskShape::Rectangle => 1.0,
            MaskShape::Polygon => 2.0,
        };
        let a = mk.rotation.to_radians();
        let (mut off, mut cnt) = (0usize, 0usize);
        if mk.shape == MaskShape::Polygon {
            off = pt_off;
            for p in &mk.points {
                if pt_off >= MAX_POLY_PTS {
                    break;
                }
                u.pts[pt_off] = *p;
                pt_off += 1;
                cnt += 1;
            }
            if cnt < 3 {
                continue; // entartetes Polygon überspringen
            }
        }
        u.a[n] = [mk.center[0], mk.center[1], mk.radius[0].max(1e-5), mk.radius[1].max(1e-5)];
        u.b[n] = [a.cos(), a.sin(), mk.corner.max(0.0), mk.feather.max(0.0)];
        u.c[n] = [
            if mk.inverted { 1.0 } else { 0.0 },
            mk.opacity.clamp(0.0, 1.0),
            shape_f,
            off as f32,
        ];
        u.d[n] = [cnt as f32, 0.0, 0.0, 0.0];
        n += 1;
    }
    u.count = n as i32;
    u
}

/// GLSL-Pendant zu den Signed-Distance-Funktionen + [`combined_mask`].
/// Definiert `float maskCoverage(vec2 uv)` über die Uniform-Arrays. MUSS
/// formelgleich zum CPU-Pfad bleiben. Wird in `ui/fx_shader` eingebunden.
pub const MASK_GLSL: &str = r#"
#define MAX_MASKS 8
#define MAX_PTS 64
uniform int  uMaskCount;
uniform vec4 uMaskA[MAX_MASKS]; // cx, cy, rx, ry
uniform vec4 uMaskB[MAX_MASKS]; // cosR, sinR, corner, feather
uniform vec4 uMaskC[MAX_MASKS]; // invert, opacity, shape, polyOff
uniform vec4 uMaskD[MAX_MASKS]; // polyCount, _, _, _
uniform vec2 uPolyPts[MAX_PTS];

float covFromSD(float d, float feather) {
    if (feather < 1e-6) { return d <= 0.0 ? 1.0 : 0.0; }
    return 1.0 - smoothstep(-feather * 0.5, feather * 0.5, d);
}
float sdEllipse(vec2 p, vec2 ab) {
    float k1 = length(p / ab);
    if (k1 < 1e-6) { return -min(ab.x, ab.y); }
    float k2 = length(p / (ab * ab));
    return k1 * (k1 - 1.0) / max(k2, 1e-12);
}
float sdRoundBox(vec2 p, vec2 b, float r) {
    r = min(r, min(b.x, b.y));
    vec2 q = abs(p) - b + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}
float sdPolygon(vec2 p, int off, int cnt) {
    vec2 v0 = uPolyPts[off];
    float d = dot(p - v0, p - v0);
    float s = 1.0;
    int j = cnt - 1;
    for (int i = 0; i < cnt; i++) {
        vec2 vi = uPolyPts[off + i];
        vec2 vj = uPolyPts[off + j];
        vec2 e = vj - vi;
        vec2 w = p - vi;
        float t = clamp(dot(w, e) / max(dot(e, e), 1e-12), 0.0, 1.0);
        vec2 b = w - e * t;
        d = min(d, dot(b, b));
        bvec3 c = bvec3(p.y >= vi.y, p.y < vj.y, e.x * w.y > e.y * w.x);
        if (all(c) || all(not(c))) { s = -s; }
        j = i;
    }
    return s * sqrt(d);
}
float maskCoverage(vec2 uv) {
    float m = 0.0;
    for (int i = 0; i < uMaskCount; i++) {
        vec4 A = uMaskA[i];
        vec4 B = uMaskB[i];
        vec4 C = uMaskC[i];
        int shape = int(C.z + 0.5);
        float d;
        if (shape == 2) {
            int off = int(C.w + 0.5);
            int cnt = int(uMaskD[i].x + 0.5);
            d = sdPolygon(uv, off, cnt);
        } else {
            vec2 dxy = uv - A.xy;
            vec2 p = vec2(dxy.x * B.x + dxy.y * B.y, -dxy.x * B.y + dxy.y * B.x);
            d = (shape == 1) ? sdRoundBox(p, A.zw, B.z) : sdEllipse(p, A.zw);
        }
        float c = covFromSD(d, B.w);
        if (C.x > 0.5) { c = 1.0 - c; }
        c *= C.y;
        m = m + c - m * c;
    }
    return clamp(m, 0.0, 1.0);
}
"#;

// ----------------------------------------------------------------- Testmodus

/// `EDITRON_TEST_MASK="ellipse:cx=0.5,cy=0.5,rx=0.3,ry=0.3,feather=0.1,invert=1"`
/// → Maskenliste für visuelle Smoke-Tests (mehrere mit `;` getrennt). Schlüssel:
/// `cx,cy,rx,ry,rot,corner,feather,invert,opacity`. Polygon nutzt die Default-Punkte.
pub fn parse_test_masks(spec: &str) -> Vec<Mask> {
    let mut out = Vec::new();
    for part in spec.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (name, args) = match part.split_once(':') {
            Some((n, a)) => (n.trim(), a.trim()),
            None => (part, ""),
        };
        let Some(shape) = MaskShape::ALL
            .iter()
            .find(|s| s.key().eq_ignore_ascii_case(name))
        else {
            continue;
        };
        let mut m = Mask::new(*shape);
        for pair in args.split(',') {
            let Some((k, v)) = pair.split_once('=') else { continue };
            let Ok(val) = v.trim().parse::<f32>() else { continue };
            match k.trim() {
                "cx" => m.center[0] = val,
                "cy" => m.center[1] = val,
                "rx" => m.radius[0] = val,
                "ry" => m.radius[1] = val,
                "rot" => m.rotation = val,
                "corner" => m.corner = val,
                "feather" => m.feather = val,
                "invert" => m.inverted = val >= 0.5,
                "opacity" => m.opacity = val,
                _ => {}
            }
        }
        out.push(m);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ellipse_inside_outside_and_feather_ramp() {
        let mut m = Mask::new(MaskShape::Ellipse);
        m.center = [0.5, 0.5];
        m.radius = [0.3, 0.3];
        m.feather = 0.1;
        // Zentrum: voll gedeckt.
        assert!((m.coverage(0.5, 0.5) - 1.0).abs() < 1e-4, "Zentrum innen");
        // Weit außen: nichts.
        assert!(m.coverage(0.95, 0.5) < 1e-4, "weit außen");
        // Auf der Kante (d≈0): ~0,5 (Feather straddelt die Kante).
        let edge = m.coverage(0.8, 0.5);
        assert!((edge - 0.5).abs() < 0.12, "Kante ~0,5: {edge}");
        // Feather-Rampe ist monoton fallend von innen nach außen.
        let a = m.coverage(0.74, 0.5); // innerhalb der Rampe
        let b = m.coverage(0.80, 0.5); // auf der Kante
        let c = m.coverage(0.86, 0.5); // außerhalb der Rampe
        assert!(a > b && b > c, "monotone Rampe: {a} > {b} > {c}");
        assert!(a > 0.5 && c < 0.5);
    }

    #[test]
    fn hard_edge_when_feather_zero() {
        let mut m = Mask::new(MaskShape::Ellipse);
        m.radius = [0.3, 0.3];
        m.feather = 0.0;
        assert_eq!(m.coverage(0.5, 0.5), 1.0);
        assert_eq!(m.coverage(0.95, 0.5), 0.0);
    }

    #[test]
    fn inverted_flips_coverage() {
        let mut m = Mask::new(MaskShape::Ellipse);
        m.radius = [0.3, 0.3];
        m.feather = 0.0;
        m.inverted = true;
        assert_eq!(m.coverage(0.5, 0.5), 0.0, "innen invertiert = 0");
        assert_eq!(m.coverage(0.95, 0.5), 1.0, "außen invertiert = 1");
    }

    #[test]
    fn opacity_scales_coverage() {
        let mut m = Mask::new(MaskShape::Ellipse);
        m.radius = [0.3, 0.3];
        m.feather = 0.0;
        m.opacity = 0.4;
        assert!((m.coverage(0.5, 0.5) - 0.4).abs() < 1e-5);
    }

    #[test]
    fn rectangle_covers_axis_aligned_region() {
        let mut m = Mask::new(MaskShape::Rectangle);
        m.center = [0.5, 0.5];
        m.radius = [0.25, 0.15];
        m.feather = 0.0;
        assert_eq!(m.coverage(0.5, 0.5), 1.0);
        assert_eq!(m.coverage(0.6, 0.55), 1.0, "innerhalb der Halb-Kanten");
        assert_eq!(m.coverage(0.8, 0.5), 0.0, "rechts außerhalb");
        assert_eq!(m.coverage(0.5, 0.8), 0.0, "unten außerhalb");
    }

    #[test]
    fn polygon_diamond_inside_outside() {
        let m = Mask::new(MaskShape::Polygon); // Raute um (0,5; 0,5)
        assert_eq!(m.feather, 0.06);
        let mut m2 = m.clone();
        m2.feather = 0.0;
        assert_eq!(m2.coverage(0.5, 0.5), 1.0, "Zentrum innen");
        assert_eq!(m2.coverage(0.05, 0.05), 0.0, "Ecke außen");
    }

    #[test]
    fn combined_union_of_two_masks() {
        let mut a = Mask::new(MaskShape::Ellipse);
        a.center = [0.3, 0.5];
        a.radius = [0.15, 0.15];
        a.feather = 0.0;
        let mut b = Mask::new(MaskShape::Ellipse);
        b.center = [0.7, 0.5];
        b.radius = [0.15, 0.15];
        b.feather = 0.0;
        let masks = [a, b];
        assert_eq!(combined_mask(&masks, 0.3, 0.5), 1.0, "in A");
        assert_eq!(combined_mask(&masks, 0.7, 0.5), 1.0, "in B");
        assert_eq!(combined_mask(&masks, 0.5, 0.5), 0.0, "zwischen beiden");
    }

    #[test]
    fn pack_uniforms_flattens_polygons() {
        let mut poly = Mask::new(MaskShape::Polygon);
        poly.points = vec![[0.1, 0.1], [0.9, 0.1], [0.5, 0.9]];
        let ell = Mask::new(MaskShape::Ellipse);
        let u = pack_uniforms(&[ell, poly]);
        assert_eq!(u.count, 2);
        // Ellipse: shape 0, Polygon: shape 2 mit Offset 0 und Count 3.
        assert_eq!(u.c[0][2], 0.0);
        assert_eq!(u.c[1][2], 2.0);
        assert_eq!(u.d[1][0], 3.0);
        assert_eq!(u.pts[0], [0.1, 0.1]);
        assert_eq!(u.pts[2], [0.5, 0.9]);
    }

    #[test]
    fn disabled_masks_excluded_from_pack_and_combine() {
        let mut a = Mask::new(MaskShape::Ellipse);
        a.enabled = false;
        let u = pack_uniforms(&[a.clone()]);
        assert_eq!(u.count, 0);
        assert_eq!(combined_mask(&[a], 0.5, 0.5), 0.0);
    }

    #[test]
    fn combined_mask_caps_at_max_masks() {
        // 8 winzige Masken fern von P, eine 9. genau auf P → die 9. wird
        // gedeckelt (ignoriert), Deckung an P bleibt 0 (GPU==CPU-Grenze).
        let mut masks: Vec<Mask> = (0..MAX_MASKS)
            .map(|_| {
                let mut m = Mask::new(MaskShape::Ellipse);
                m.center = [0.95, 0.95];
                m.radius = [0.01, 0.01];
                m.feather = 0.0;
                m
            })
            .collect();
        let mut ninth = Mask::new(MaskShape::Ellipse);
        ninth.center = [0.5, 0.5];
        ninth.radius = [0.3, 0.3];
        ninth.feather = 0.0;
        masks.push(ninth);
        assert_eq!(masks.len(), MAX_MASKS + 1);
        assert_eq!(combined_mask(&masks, 0.5, 0.5), 0.0, "9. Maske gedeckelt");
        // Ohne die ersten 8 (nur die große) deckt P voll.
        assert_eq!(combined_mask(&masks[MAX_MASKS..], 0.5, 0.5), 1.0);
    }

    #[test]
    fn parse_test_masks_reads_keys() {
        let m = parse_test_masks("ellipse:cx=0.4,cy=0.6,rx=0.2,ry=0.1,feather=0.05,invert=1");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].shape, MaskShape::Ellipse);
        assert_eq!(m[0].center, [0.4, 0.6]);
        assert_eq!(m[0].radius, [0.2, 0.1]);
        assert!(m[0].inverted);
        assert!(parse_test_masks("unbekannt:cx=1").is_empty());
    }
}
