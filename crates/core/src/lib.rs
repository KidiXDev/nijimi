mod stroke;
mod viewport;

pub use stroke::{Brush, Dab, Stroke, StrokePoint};
pub use viewport::Viewport;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}
