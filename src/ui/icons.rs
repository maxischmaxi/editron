//! Lucide-Icon-Renderer: parst die SVG-Geometrie (24×24-Raster, stroke 2,
//! round caps/joins) aus `icons_data.rs` einmalig zu Polylinien und zeichnet
//! sie skaliert mit dicken Linien + Rundkappen (MSAA glättet).

use super::geom::{v2, Rect};
use raylib::color::Color;
use raylib::math::Vector2;
use raylib::prelude::RaylibDraw;
use std::collections::HashMap;

/// Roh-Element eines Icons (aus dem Generator).
pub enum IconElement {
    Path(&'static str),
    Circle(f32, f32, f32),
    Ellipse(f32, f32, f32, f32),
    Line(f32, f32, f32, f32),
    Rect(f32, f32, f32, f32, f32),
    Polyline(&'static str),
    Polygon(&'static str),
}

/// Tessellierte Form: Punktzug + geschlossen-Flag, im 24×24-Icon-Raum.
pub struct IconPath {
    pub points: Vec<Vector2>,
    pub closed: bool,
}

pub struct Icon {
    pub paths: Vec<IconPath>,
}

pub struct IconSet {
    icons: HashMap<&'static str, Icon>,
}

impl IconSet {
    pub fn load() -> IconSet {
        let mut icons = HashMap::new();
        for (name, elements) in super::icons_data::ICON_DATA {
            let mut paths = Vec::new();
            for el in *elements {
                tessellate(el, &mut paths);
            }
            icons.insert(*name, Icon { paths });
        }
        IconSet { icons }
    }

    pub fn get(&self, name: &str) -> Option<&Icon> {
        let icon = self.icons.get(name);
        debug_assert!(icon.is_some(), "Icon \"{name}\" fehlt in icons_data.rs");
        icon
    }

    /// Zeichnet das Icon zentriert in `rect` mit Kantenlänge `size`.
    pub fn draw(
        &self,
        d: &mut impl RaylibDraw,
        name: &str,
        rect: Rect,
        size: f32,
        color: Color,
    ) {
        let Some(icon) = self.get(name) else { return };
        let target = rect.center_box(size, size);
        let scale = size / 24.0;
        let stroke = 2.0 * scale;
        for path in &icon.paths {
            let pts: Vec<Vector2> = path
                .points
                .iter()
                .map(|p| v2(target.x + p.x * scale, target.y + p.y * scale))
                .collect();
            draw_stroke(d, &pts, path.closed, stroke, color);
        }
    }
}

/// Punktzug mit Dicke + Rundkappen/-gelenken zeichnen.
fn draw_stroke(
    d: &mut impl RaylibDraw,
    pts: &[Vector2],
    closed: bool,
    stroke: f32,
    color: Color,
) {
    if pts.is_empty() {
        return;
    }
    let r = stroke / 2.0;
    if pts.len() == 1 {
        d.draw_circle_v(pts[0], r, color);
        return;
    }
    for w in pts.windows(2) {
        d.draw_line_ex(w[0], w[1], stroke, color);
    }
    if closed && pts.len() > 2 {
        d.draw_line_ex(pts[pts.len() - 1], pts[0], stroke, color);
    }
    for p in pts {
        d.draw_circle_v(*p, r, color);
    }
}

const BEZIER_STEPS: usize = 10;
const ARC_STEPS_PER_RAD: f32 = 6.0;

fn tessellate(el: &IconElement, out: &mut Vec<IconPath>) {
    match el {
        IconElement::Path(data) => parse_path(data, out),
        IconElement::Circle(cx, cy, r) => out.push(ellipse_path(*cx, *cy, *r, *r)),
        IconElement::Ellipse(cx, cy, rx, ry) => out.push(ellipse_path(*cx, *cy, *rx, *ry)),
        IconElement::Line(x1, y1, x2, y2) => out.push(IconPath {
            points: vec![v2(*x1, *y1), v2(*x2, *y2)],
            closed: false,
        }),
        IconElement::Rect(x, y, w, h, rx) => out.push(rect_path(*x, *y, *w, *h, *rx)),
        IconElement::Polyline(points) => out.push(IconPath {
            points: parse_points(points),
            closed: false,
        }),
        IconElement::Polygon(points) => out.push(IconPath {
            points: parse_points(points),
            closed: true,
        }),
    }
}

fn ellipse_path(cx: f32, cy: f32, rx: f32, ry: f32) -> IconPath {
    let steps = 32;
    let points = (0..steps)
        .map(|i| {
            let a = i as f32 / steps as f32 * std::f32::consts::TAU;
            v2(cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect();
    IconPath {
        points,
        closed: true,
    }
}

fn rect_path(x: f32, y: f32, w: f32, h: f32, rx: f32) -> IconPath {
    if rx <= 0.0 {
        return IconPath {
            points: vec![v2(x, y), v2(x + w, y), v2(x + w, y + h), v2(x, y + h)],
            closed: true,
        };
    }
    let rx = rx.min(w / 2.0).min(h / 2.0);
    let mut points = Vec::new();
    let corner = |cx: f32, cy: f32, start: f32, points: &mut Vec<Vector2>| {
        let steps = 8;
        for i in 0..=steps {
            let a = start + i as f32 / steps as f32 * std::f32::consts::FRAC_PI_2;
            points.push(v2(cx + rx * a.cos(), cy + rx * a.sin()));
        }
    };
    use std::f32::consts::PI;
    corner(x + w - rx, y + rx, -PI / 2.0, &mut points);
    corner(x + w - rx, y + h - rx, 0.0, &mut points);
    corner(x + rx, y + h - rx, PI / 2.0, &mut points);
    corner(x + rx, y + rx, PI, &mut points);
    IconPath {
        points,
        closed: true,
    }
}

fn parse_points(s: &str) -> Vec<Vector2> {
    let nums = parse_numbers(s);
    nums.chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| v2(c[0], c[1]))
        .collect()
}

fn parse_numbers(s: &str) -> Vec<f32> {
    let mut nums = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '0'..='9' | '.' => cur.push(c),
            '-' => {
                // '-' beginnt eine neue Zahl, außer als Exponentzeichen (kommt bei Lucide nicht vor)
                if !cur.is_empty() {
                    if let Ok(n) = cur.parse() {
                        nums.push(n);
                    }
                    cur.clear();
                }
                cur.push('-');
            }
            _ => {
                if !cur.is_empty() {
                    if let Ok(n) = cur.parse() {
                        nums.push(n);
                    }
                    cur.clear();
                }
            }
        }
    }
    if !cur.is_empty() {
        if let Ok(n) = cur.parse() {
            nums.push(n);
        }
    }
    nums
}

/// SVG-Pfaddaten-Parser (M L H V C S Q T A Z, jeweils auch relativ).
fn parse_path(data: &str, out: &mut Vec<IconPath>) {
    let tokens = tokenize_path(data);
    let mut i = 0;

    let mut current = v2(0.0, 0.0);
    let mut start = v2(0.0, 0.0);
    let mut last_cubic_ctrl: Option<Vector2> = None;
    let mut last_quad_ctrl: Option<Vector2> = None;
    let mut points: Vec<Vector2> = Vec::new();

    macro_rules! flush {
        ($closed:expr) => {
            if points.len() > 1 {
                out.push(IconPath {
                    points: std::mem::take(&mut points),
                    closed: $closed,
                });
            } else {
                points.clear();
            }
        };
    }

    while i < tokens.len() {
        let PathToken::Cmd(cmd) = tokens[i] else {
            i += 1;
            continue;
        };
        i += 1;
        let rel = cmd.is_ascii_lowercase();
        let cmd_u = cmd.to_ascii_uppercase();

        let next_num = |i: &mut usize| -> f32 {
            if let Some(PathToken::Num(n)) = tokens.get(*i) {
                *i += 1;
                *n
            } else {
                0.0
            }
        };

        match cmd_u {
            'M' => {
                let mut first = true;
                while matches!(tokens.get(i), Some(PathToken::Num(_))) {
                    let x = next_num(&mut i);
                    let y = next_num(&mut i);
                    let p = if rel {
                        v2(current.x + x, current.y + y)
                    } else {
                        v2(x, y)
                    };
                    if first {
                        flush!(false);
                        points.push(p);
                        start = p;
                        first = false;
                    } else {
                        points.push(p); // implizites LineTo
                    }
                    current = p;
                }
                last_cubic_ctrl = None;
                last_quad_ctrl = None;
            }
            'L' => {
                while matches!(tokens.get(i), Some(PathToken::Num(_))) {
                    let x = next_num(&mut i);
                    let y = next_num(&mut i);
                    current = if rel {
                        v2(current.x + x, current.y + y)
                    } else {
                        v2(x, y)
                    };
                    points.push(current);
                }
                last_cubic_ctrl = None;
                last_quad_ctrl = None;
            }
            'H' => {
                while matches!(tokens.get(i), Some(PathToken::Num(_))) {
                    let x = next_num(&mut i);
                    current = v2(if rel { current.x + x } else { x }, current.y);
                    points.push(current);
                }
                last_cubic_ctrl = None;
                last_quad_ctrl = None;
            }
            'V' => {
                while matches!(tokens.get(i), Some(PathToken::Num(_))) {
                    let y = next_num(&mut i);
                    current = v2(current.x, if rel { current.y + y } else { y });
                    points.push(current);
                }
                last_cubic_ctrl = None;
                last_quad_ctrl = None;
            }
            'C' | 'S' => {
                while matches!(tokens.get(i), Some(PathToken::Num(_))) {
                    let (c1, c2, end);
                    if cmd_u == 'C' {
                        let x1 = next_num(&mut i);
                        let y1 = next_num(&mut i);
                        let x2 = next_num(&mut i);
                        let y2 = next_num(&mut i);
                        let x = next_num(&mut i);
                        let y = next_num(&mut i);
                        c1 = abs_pt(rel, current, x1, y1);
                        c2 = abs_pt(rel, current, x2, y2);
                        end = abs_pt(rel, current, x, y);
                    } else {
                        let x2 = next_num(&mut i);
                        let y2 = next_num(&mut i);
                        let x = next_num(&mut i);
                        let y = next_num(&mut i);
                        c1 = match last_cubic_ctrl {
                            Some(prev) => v2(2.0 * current.x - prev.x, 2.0 * current.y - prev.y),
                            None => current,
                        };
                        c2 = abs_pt(rel, current, x2, y2);
                        end = abs_pt(rel, current, x, y);
                    }
                    for s in 1..=BEZIER_STEPS {
                        let t = s as f32 / BEZIER_STEPS as f32;
                        points.push(cubic_at(current, c1, c2, end, t));
                    }
                    last_cubic_ctrl = Some(c2);
                    current = end;
                }
                last_quad_ctrl = None;
            }
            'Q' | 'T' => {
                while matches!(tokens.get(i), Some(PathToken::Num(_))) {
                    let (c, end);
                    if cmd_u == 'Q' {
                        let x1 = next_num(&mut i);
                        let y1 = next_num(&mut i);
                        let x = next_num(&mut i);
                        let y = next_num(&mut i);
                        c = abs_pt(rel, current, x1, y1);
                        end = abs_pt(rel, current, x, y);
                    } else {
                        let x = next_num(&mut i);
                        let y = next_num(&mut i);
                        c = match last_quad_ctrl {
                            Some(prev) => v2(2.0 * current.x - prev.x, 2.0 * current.y - prev.y),
                            None => current,
                        };
                        end = abs_pt(rel, current, x, y);
                    }
                    for s in 1..=BEZIER_STEPS {
                        let t = s as f32 / BEZIER_STEPS as f32;
                        points.push(quad_at(current, c, end, t));
                    }
                    last_quad_ctrl = Some(c);
                    current = end;
                }
                last_cubic_ctrl = None;
            }
            'A' => {
                while matches!(tokens.get(i), Some(PathToken::Num(_))) {
                    let rx = next_num(&mut i);
                    let ry = next_num(&mut i);
                    let rot = next_num(&mut i).to_radians();
                    let large = next_num(&mut i) != 0.0;
                    let sweep = next_num(&mut i) != 0.0;
                    let x = next_num(&mut i);
                    let y = next_num(&mut i);
                    let end = abs_pt(rel, current, x, y);
                    arc_to(&mut points, current, end, rx, ry, rot, large, sweep);
                    current = end;
                }
                last_cubic_ctrl = None;
                last_quad_ctrl = None;
            }
            'Z' => {
                current = start;
                flush!(true);
                last_cubic_ctrl = None;
                last_quad_ctrl = None;
            }
            _ => {}
        }
    }
    flush!(false);
}

fn abs_pt(rel: bool, current: Vector2, x: f32, y: f32) -> Vector2 {
    if rel {
        v2(current.x + x, current.y + y)
    } else {
        v2(x, y)
    }
}

fn cubic_at(p0: Vector2, p1: Vector2, p2: Vector2, p3: Vector2, t: f32) -> Vector2 {
    let u = 1.0 - t;
    v2(
        u * u * u * p0.x + 3.0 * u * u * t * p1.x + 3.0 * u * t * t * p2.x + t * t * t * p3.x,
        u * u * u * p0.y + 3.0 * u * u * t * p1.y + 3.0 * u * t * t * p2.y + t * t * t * p3.y,
    )
}

fn quad_at(p0: Vector2, p1: Vector2, p2: Vector2, t: f32) -> Vector2 {
    let u = 1.0 - t;
    v2(
        u * u * p0.x + 2.0 * u * t * p1.x + t * t * p2.x,
        u * u * p0.y + 2.0 * u * t * p1.y + t * t * p2.y,
    )
}

/// SVG-Ellipsenbogen (Endpoint-Form) → Polylinie (Center-Parametrisierung, W3C-Algorithmus).
fn arc_to(
    points: &mut Vec<Vector2>,
    from: Vector2,
    to: Vector2,
    rx: f32,
    ry: f32,
    rot: f32,
    large: bool,
    sweep: bool,
) {
    if rx == 0.0 || ry == 0.0 {
        points.push(to);
        return;
    }
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    let (cos_r, sin_r) = (rot.cos(), rot.sin());

    let dx2 = (from.x - to.x) / 2.0;
    let dy2 = (from.y - to.y) / 2.0;
    let x1 = cos_r * dx2 + sin_r * dy2;
    let y1 = -sin_r * dx2 + cos_r * dy2;

    let lambda = x1 * x1 / (rx * rx) + y1 * y1 / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }

    let sign = if large != sweep { 1.0 } else { -1.0 };
    let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let coef = sign * (num / den).max(0.0).sqrt();
    let cx1 = coef * rx * y1 / ry;
    let cy1 = -coef * ry * x1 / rx;

    let cx = cos_r * cx1 - sin_r * cy1 + (from.x + to.x) / 2.0;
    let cy = sin_r * cx1 + cos_r * cy1 + (from.y + to.y) / 2.0;

    let angle = |ux: f32, uy: f32, vx: f32, vy: f32| -> f32 {
        let dot = ux * vx + uy * vy;
        let len = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
        let mut a = (dot / len).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 {
            a = -a;
        }
        a
    };

    let theta1 = angle(1.0, 0.0, (x1 - cx1) / rx, (cy1.mul_add(-1.0, y1)) / ry);
    let mut dtheta = angle(
        (x1 - cx1) / rx,
        (y1 - cy1) / ry,
        (-x1 - cx1) / rx,
        (-y1 - cy1) / ry,
    );
    if !sweep && dtheta > 0.0 {
        dtheta -= std::f32::consts::TAU;
    } else if sweep && dtheta < 0.0 {
        dtheta += std::f32::consts::TAU;
    }

    let steps = ((dtheta.abs() * ARC_STEPS_PER_RAD).ceil() as usize).max(2);
    for s in 1..=steps {
        let t = theta1 + dtheta * s as f32 / steps as f32;
        let (sin_t, cos_t) = t.sin_cos();
        points.push(v2(
            cx + rx * cos_t * cos_r - ry * sin_t * sin_r,
            cy + rx * cos_t * sin_r + ry * sin_t * cos_r,
        ));
    }
}

enum PathToken {
    Cmd(char),
    Num(f32),
}

fn tokenize_path(data: &str) -> Vec<PathToken> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let flush_num = |cur: &mut String, tokens: &mut Vec<PathToken>| {
        if !cur.is_empty() {
            if let Ok(n) = cur.parse() {
                tokens.push(PathToken::Num(n));
            }
            cur.clear();
        }
    };
    for c in data.chars() {
        match c {
            'a'..='z' | 'A'..='Z' => {
                flush_num(&mut cur, &mut tokens);
                tokens.push(PathToken::Cmd(c));
            }
            '0'..='9' => cur.push(c),
            '.' => {
                // zweiter Punkt beendet die Zahl ("1.5.5" = 1.5, 0.5)
                if cur.contains('.') {
                    flush_num(&mut cur, &mut tokens);
                }
                cur.push(c);
            }
            '-' => {
                flush_num(&mut cur, &mut tokens);
                cur.push('-');
            }
            _ => flush_num(&mut cur, &mut tokens),
        }
    }
    flush_num(&mut cur, &mut tokens);
    tokens
}
