mod adjustment;
mod command;
mod document;
mod event;
mod geometry;
mod histogram;
mod history;
mod layer;
mod project;
mod selection;
mod stroke;
mod text;
mod tile;
mod viewport;

use serde::{Deserialize, Serialize};

pub use adjustment::{Adjustment, CubeError, CubeLut, LutInfo, LutKind};
pub use command::{Command, deserialize_present};
pub use document::{Document, DocumentError, LayerPlace, MAX_GROUP_DEPTH, MAX_LAYERS};
pub use event::Event;
pub use geometry::{CropRatio, PixelRect, Transform};
pub use histogram::Histogram;
pub use history::{DEFAULT_HISTORY_LIMIT, PlaneEdit, RasterEdit};
pub use layer::{
    BlendMode, Layer, LayerContent, LayerId, LayerMask, LayerProperties, MAX_NAME_LENGTH, Tiles,
    find_layer,
};
pub use project::{MAX_PROJECT_TILES, ProjectError, ProjectReader, ProjectWriter};
pub use selection::{MAX_SELECTION_RADIUS, Selection, SelectionMode, SelectionShape};
pub use stroke::{
    Brush, BrushDynamics, BrushPreset, Dab, MAX_BLUR_RADIUS, MAX_BRUSH_DIAMETER, MAX_COORDINATE,
    PressureCurve, RetouchTool, Stroke, StrokePoint,
};
pub use text::{MAX_FONT_SIZE, MAX_TEXT_LENGTH, TextAlign, TextSettings};
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
