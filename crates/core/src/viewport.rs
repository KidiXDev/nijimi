use crate::Point;

const MIN_ZOOM: f32 = 0.02;
const MAX_ZOOM: f32 = 64.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pan: Point,
    zoom: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            pan: Point::default(),
            zoom: 1.0,
        }
    }
}

impl Viewport {
    pub fn pan(&self) -> Point {
        self.pan
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    pub fn to_document(&self, screen: Point) -> Point {
        Point::new(
            (screen.x - self.pan.x) / self.zoom,
            (screen.y - self.pan.y) / self.zoom,
        )
    }

    pub fn pan_by(&mut self, dx: f32, dy: f32) {
        if dx.is_finite() && dy.is_finite() {
            self.pan.x += dx;
            self.pan.y += dy;
        }
    }

    pub fn zoom_at(&mut self, screen: Point, factor: f32) {
        if !(factor.is_finite() && factor > 0.0 && screen.x.is_finite() && screen.y.is_finite()) {
            return;
        }
        let anchor = self.to_document(screen);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.pan = Point::new(
            screen.x - anchor.x * self.zoom,
            screen.y - anchor.y * self.zoom,
        );
    }

    pub fn fit(&mut self, document: (u32, u32), screen: (u32, u32)) {
        let (doc_w, doc_h) = (document.0.max(1) as f32, document.1.max(1) as f32);
        let (screen_w, screen_h) = (screen.0.max(1) as f32, screen.1.max(1) as f32);
        self.zoom = (screen_w / doc_w)
            .min(screen_h / doc_h)
            .clamp(MIN_ZOOM, MAX_ZOOM);
        self.pan = Point::new(
            (screen_w - doc_w * self.zoom) / 2.0,
            (screen_h - doc_h * self.zoom) / 2.0,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(a: Point, b: Point) {
        assert!(
            (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3,
            "{a:?} != {b:?}"
        );
    }

    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let mut viewport = Viewport::default();
        viewport.pan_by(40.0, -12.0);
        let cursor = Point::new(300.0, 200.0);
        let before = viewport.to_document(cursor);
        viewport.zoom_at(cursor, 2.5);
        assert_close(viewport.to_document(cursor), before);
    }

    #[test]
    fn zoom_stays_anchored_at_its_limit() {
        let mut viewport = Viewport::default();
        let cursor = Point::new(10.0, 10.0);
        viewport.zoom_at(cursor, 1e6);
        assert_eq!(viewport.zoom(), MAX_ZOOM);
        assert_close(viewport.to_document(cursor), cursor);
    }

    #[test]
    fn invalid_zoom_is_ignored() {
        let mut viewport = Viewport::default();
        for factor in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            viewport.zoom_at(Point::default(), factor);
        }
        assert_eq!(viewport, Viewport::default());
    }

    #[test]
    fn fit_centers_the_document() {
        let mut viewport = Viewport::default();
        viewport.fit((3840, 2160), (1000, 1000));
        assert_close(
            viewport.to_document(Point::new(500.0, 500.0)),
            Point::new(1920.0, 1080.0),
        );
        assert_close(
            viewport.to_document(Point::new(0.0, 500.0)),
            Point::new(0.0, 1080.0),
        );
    }
}
