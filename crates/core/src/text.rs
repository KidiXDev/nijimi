use serde::{Deserialize, Serialize};

use crate::{MAX_COORDINATE, Point};

pub const MAX_TEXT_LENGTH: usize = 10_000;
pub const MAX_FONT_SIZE: f32 = 2000.0;

const MAX_FONT_FAMILY_LENGTH: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextSettings {
    pub text: String,
    #[serde(flatten)]
    pub position: Point,
    pub font_family: String,
    pub font_size: f32,
    #[serde(default = "normal_weight")]
    pub weight: u16,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub color: [f32; 3],
    #[serde(default)]
    pub align: TextAlign,
    #[serde(default = "normal_line_height")]
    pub line_height: f32,
    #[serde(default = "identity")]
    pub transform: [f32; 4],
}

fn normal_weight() -> u16 {
    400
}

fn normal_line_height() -> f32 {
    1.2
}

fn identity() -> [f32; 4] {
    [1.0, 0.0, 0.0, 1.0]
}

impl TextSettings {
    pub fn is_valid(&self) -> bool {
        let Point { x, y } = self.position;
        self.text.len() <= MAX_TEXT_LENGTH
            && (1..=MAX_FONT_FAMILY_LENGTH).contains(&self.font_family.len())
            && (1.0..=MAX_FONT_SIZE).contains(&self.font_size)
            && (1..=1000).contains(&self.weight)
            && (0.1..=10.0).contains(&self.line_height)
            && self
                .color
                .iter()
                .all(|channel| (0.0..=1.0).contains(channel))
            && [x, y]
                .iter()
                .chain(&self.transform)
                .all(|value| value.abs() <= MAX_COORDINATE)
    }

    pub(crate) fn bytes(&self) -> u64 {
        (self.text.len() + self.font_family.len()) as u64
    }
}
