use serde::{Deserialize, Serialize};

use crate::Point;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub fn full((width, height): (u32, u32)) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    pub fn size(self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn is_inside(self, size: (u32, u32)) -> bool {
        self.width > 0
            && self.height > 0
            && self.width <= size.0.saturating_sub(self.x)
            && self.height <= size.1.saturating_sub(self.y)
    }

    pub fn with_ratio(self, ratio: CropRatio) -> Option<Self> {
        let (width, height) = match ratio {
            CropRatio::Square => (1, 1),
            CropRatio::Classic => (4, 3),
            CropRatio::Photo => (3, 2),
            CropRatio::Wide => (16, 9),
            CropRatio::Portrait => (9, 16),
        };
        let units = (self.width / width).min(self.height / height);
        if units == 0 {
            return None;
        }
        let (width, height) = (units * width, units * height);
        Some(Self {
            x: self.x.checked_add((self.width - width) / 2)?,
            y: self.y.checked_add((self.height - height) / 2)?,
            width,
            height,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CropRatio {
    Square,
    Classic,
    Photo,
    Wide,
    Portrait,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Transform {
    pub x: f32,
    pub y: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub rotation: f32,
    pub pivot: Option<Point>,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation: 0.0,
            pivot: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_fit_an_exact_centered_ratio() {
        assert_eq!(
            PixelRect::full((33, 25)).with_ratio(CropRatio::Wide),
            Some(PixelRect {
                x: 0,
                y: 3,
                width: 32,
                height: 18
            }),
        );
        assert_eq!(PixelRect::full((15, 25)).with_ratio(CropRatio::Wide), None);
    }
}
