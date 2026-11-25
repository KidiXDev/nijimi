mod command;
mod document;
mod event;
mod geometry;
mod history;
mod histogram;
mod layer;
mod project;
mod stroke;
mod tile;
mod viewport;

use serde::{Deserialize, Serialize};

pub use command::Command;
pub use document::{Document, DocumentError, LayerPlace, MAX_GROUP_DEPTH, MAX_LAYERS};
pub use event::Event;
pub use geometry::{CropRatio, PixelRect, Transform};
pub use histogram::Histogram;
pub use history::{DEFAULT_HISTORY_LIMIT, RasterEdit};
pub use layer::{
    BlendMode, Layer, LayerContent, LayerId, LayerProperties, MAX_NAME_LENGTH, Tiles, find_layer,
};
pub use project::{MAX_PROJECT_TILES, ProjectError, ProjectReader, ProjectWriter};
pub use stroke::{
    Brush, BrushDynamics, BrushPreset, Dab, MAX_BRUSH_DIAMETER, MAX_COORDINATE, PressureCurve,
    Stroke, StrokePoint,
};
pub use tile::{TILE_BYTES, TILE_SIZE, TileCoord};
pub use viewport::Viewport;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}
