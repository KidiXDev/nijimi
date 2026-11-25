use std::collections::VecDeque;

use crate::{Adjustment, Layer, LayerId, LayerMask, LayerProperties, TILE_BYTES, TileCoord};

pub const DEFAULT_HISTORY_LIMIT: u64 = 256 * 1024 * 1024;

pub type RasterEdit<T> = (LayerId, Vec<(TileCoord, Option<T>)>);

const ENTRY_BYTES: u64 = 512;

#[derive(Debug)]
pub(crate) enum Edit<T> {
    Insert {
        parent: Option<LayerId>,
        index: usize,
        layer: Box<Layer<T>>,
    },
    Remove {
        id: LayerId,
    },
    Move {
        id: LayerId,
        parent: Option<LayerId>,
        index: usize,
    },
    Properties {
        id: LayerId,
        properties: LayerProperties,
    },
    Pixels {
        size: (u32, u32),
        rasters: Vec<RasterEdit<T>>,
        masks: Vec<RasterEdit<T>>,
    },
    Mask {
        id: LayerId,
        mask: Option<LayerMask<T>>,
    },
    Adjustment {
        id: LayerId,
        adjustment: Adjustment,
    },
}

impl<T> Edit<T> {
    fn bytes(&self) -> u64 {
        let payload = match self {
            Self::Insert { layer, .. } => {
                let mut bytes = 0;
                layer.visit(&mut |layer| {
                    bytes += ENTRY_BYTES
                        + layer.tiles().map_or(0, |tiles| tile_bytes(tiles.len()))
                        + layer
                            .mask
                            .as_ref()
                            .map_or(0, |mask| tile_bytes(mask.tiles.len()))
                        + layer.grade().map_or(0, Adjustment::data_bytes);
                });
                bytes
            }
            Self::Pixels { rasters, masks, .. } => rasters
                .iter()
                .chain(masks)
                .map(|(_, tiles)| {
                    let kept = tiles.iter().filter(|(_, tile)| tile.is_some()).count();
                    tile_bytes(kept) + size_of_val(tiles.as_slice()) as u64
                })
                .sum(),
            Self::Mask { mask, .. } => mask.as_ref().map_or(0, |mask| tile_bytes(mask.tiles.len())),
            Self::Adjustment { adjustment, .. } => adjustment.data_bytes(),
            Self::Remove { .. } | Self::Move { .. } | Self::Properties { .. } => 0,
        };
        ENTRY_BYTES + payload
    }
}

#[derive(Debug)]
pub(crate) struct Entry<T> {
    pub edit: Edit<T>,
    pub active: Option<LayerId>,
}

#[derive(Debug)]
pub(crate) struct History<T> {
    undo: VecDeque<Entry<T>>,
    redo: VecDeque<Entry<T>>,
    bytes: u64,
    limit: u64,
}

impl<T> History<T> {
    pub fn new(limit: u64) -> Self {
        Self {
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            bytes: 0,
            limit,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    pub fn set_limit(&mut self, limit: u64) {
        self.limit = limit;
        self.trim();
    }

    pub fn record(&mut self, entry: Entry<T>) {
        for dropped in self.redo.drain(..) {
            self.bytes -= dropped.edit.bytes();
        }
        self.push_undo(entry);
    }

    pub fn push_undo(&mut self, entry: Entry<T>) {
        self.bytes += entry.edit.bytes();
        self.undo.push_back(entry);
        self.trim();
    }

    pub fn push_redo(&mut self, entry: Entry<T>) {
        self.bytes += entry.edit.bytes();
        self.redo.push_back(entry);
        self.trim();
    }

    pub fn pop_undo(&mut self) -> Option<Entry<T>> {
        let entry = self.undo.pop_back()?;
        self.bytes -= entry.edit.bytes();
        Some(entry)
    }

    pub fn pop_redo(&mut self) -> Option<Entry<T>> {
        let entry = self.redo.pop_back()?;
        self.bytes -= entry.edit.bytes();
        Some(entry)
    }

    fn trim(&mut self) {
        while self.bytes > self.limit {
            let Some(oldest) = self.undo.pop_front().or_else(|| self.redo.pop_front()) else {
                return;
            };
            self.bytes -= oldest.edit.bytes();
        }
    }
}

fn tile_bytes(count: usize) -> u64 {
    count as u64 * TILE_BYTES as u64
}
