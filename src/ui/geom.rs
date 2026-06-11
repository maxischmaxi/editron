//! Rechteck-Geometrie mit RectCut-Layout-Helfern (CSS-Flexbox-Ersatz).

use raylib::math::{Rectangle, Vector2};

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn center(&self) -> Vector2 {
        Vector2::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn contains(&self, p: Vector2) -> bool {
        p.x >= self.x && p.x < self.x + self.w && p.y >= self.y && p.y < self.y + self.h
    }

    pub fn intersect(&self, o: Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect::new(x, y, (r - x).max(0.0), (b - y).max(0.0))
    }

    pub fn overlaps(&self, o: Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }

    pub fn inset(&self, amount: f32) -> Rect {
        Rect::new(
            self.x + amount,
            self.y + amount,
            (self.w - 2.0 * amount).max(0.0),
            (self.h - 2.0 * amount).max(0.0),
        )
    }

    pub fn inset_xy(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(
            self.x + dx,
            self.y + dy,
            (self.w - 2.0 * dx).max(0.0),
            (self.h - 2.0 * dy).max(0.0),
        )
    }

    pub fn offset(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// Schneidet `amount` von links ab und liefert das abgeschnittene Stück.
    pub fn cut_left(&mut self, amount: f32) -> Rect {
        let a = amount.min(self.w);
        let r = Rect::new(self.x, self.y, a, self.h);
        self.x += a;
        self.w -= a;
        r
    }

    pub fn cut_right(&mut self, amount: f32) -> Rect {
        let a = amount.min(self.w);
        let r = Rect::new(self.x + self.w - a, self.y, a, self.h);
        self.w -= a;
        r
    }

    pub fn cut_top(&mut self, amount: f32) -> Rect {
        let a = amount.min(self.h);
        let r = Rect::new(self.x, self.y, self.w, a);
        self.y += a;
        self.h -= a;
        r
    }

    pub fn cut_bottom(&mut self, amount: f32) -> Rect {
        let a = amount.min(self.h);
        let r = Rect::new(self.x, self.y + self.h - a, self.w, a);
        self.h -= a;
        r
    }

    /// Zentriertes Quadrat/Rechteck der Größe (w, h) innerhalb von self.
    pub fn center_box(&self, w: f32, h: f32) -> Rect {
        Rect::new(
            self.x + (self.w - w) / 2.0,
            self.y + (self.h - h) / 2.0,
            w,
            h,
        )
    }

    /// In `self` einpassen, Seitenverhältnis wahren (CSS object-fit: contain).
    pub fn fit_contain(&self, content_w: f32, content_h: f32) -> Rect {
        if content_w <= 0.0 || content_h <= 0.0 || self.w <= 0.0 || self.h <= 0.0 {
            return *self;
        }
        let scale = (self.w / content_w).min(self.h / content_h);
        self.center_box(content_w * scale, content_h * scale)
    }
}

impl From<Rect> for Rectangle {
    fn from(r: Rect) -> Rectangle {
        Rectangle::new(r.x, r.y, r.w, r.h)
    }
}

impl From<Rect> for raylib::ffi::Rectangle {
    fn from(r: Rect) -> raylib::ffi::Rectangle {
        raylib::ffi::Rectangle {
            x: r.x,
            y: r.y,
            width: r.w,
            height: r.h,
        }
    }
}

pub fn v2(x: f32, y: f32) -> Vector2 {
    Vector2::new(x, y)
}
