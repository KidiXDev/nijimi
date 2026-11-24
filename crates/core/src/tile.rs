use serde::{Deserialize, Serialize};

pub const TILE_SIZE: u32 = 256;
pub const TILE_BYTES: usize = (TILE_SIZE * TILE_SIZE) as usize * 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(from = "(u32, u32)", into = "(u32, u32)")]
pub struct TileCoord {
    pub x: u32,
    pub y: u32,
}

impl From<(u32, u32)> for TileCoord {
    fn from((x, y): (u32, u32)) -> Self {
        Self { x, y }
    }
}

impl From<TileCoord> for (u32, u32) {
    fn from(coord: TileCoord) -> Self {
        (coord.x, coord.y)
    }
}

impl TileCoord {
    pub const fn new(x: u32, y: u32) -> Self {
        Self { x, y }
    }

    pub fn origin(self) -> (u32, u32) {
        (self.x * TILE_SIZE, self.y * TILE_SIZE)
    }

    pub fn is_inside(self, size: (u32, u32)) -> bool {
        let (columns, rows) = grid(size);
        self.x < columns && self.y < rows
    }

    pub fn visible_size(self, size: (u32, u32)) -> (u32, u32) {
        let (x, y) = self.origin();
        ((size.0 - x).min(TILE_SIZE), (size.1 - y).min(TILE_SIZE))
    }

    pub fn all(size: (u32, u32)) -> impl Iterator<Item = Self> {
        let (columns, rows) = grid(size);
        (0..rows).flat_map(move |y| (0..columns).map(move |x| Self { x, y }))
    }
}

fn grid((width, height): (u32, u32)) -> (u32, u32) {
    (width.div_ceil(TILE_SIZE), height.div_ceil(TILE_SIZE))
}
