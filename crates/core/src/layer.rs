use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::TileCoord;

pub type LayerId = u32;
pub type Tiles<T> = BTreeMap<TileCoord, T>;

pub const MAX_NAME_LENGTH: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    SoftLight,
    Add,
    Darken,
    Lighten,
}

impl BlendMode {
    pub const ALL: [Self; 8] = [
        Self::Normal,
        Self::Multiply,
        Self::Screen,
        Self::Overlay,
        Self::SoftLight,
        Self::Add,
        Self::Darken,
        Self::Lighten,
    ];
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerProperties {
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub blend_mode: BlendMode,
}

impl LayerProperties {
    pub fn named(name: String) -> Self {
        Self {
            name,
            visible: true,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
        }
    }

    pub(crate) fn appearance(&self) -> (bool, f32, BlendMode) {
        (self.visible, self.opacity, self.blend_mode)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer<T> {
    pub id: LayerId,
    pub properties: LayerProperties,
    pub content: LayerContent<T>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayerContent<T> {
    Raster(Tiles<T>),
    Group(Vec<Layer<T>>),
}

impl<T> Layer<T> {
    pub fn raster(id: LayerId, name: String) -> Self {
        Self {
            id,
            properties: LayerProperties::named(name),
            content: LayerContent::Raster(Tiles::new()),
        }
    }

    pub fn group(id: LayerId, name: String) -> Self {
        Self {
            id,
            properties: LayerProperties::named(name),
            content: LayerContent::Group(Vec::new()),
        }
    }

    pub fn children(&self) -> &[Self] {
        match &self.content {
            LayerContent::Group(children) => children,
            LayerContent::Raster(_) => &[],
        }
    }

    pub fn tiles(&self) -> Option<&Tiles<T>> {
        match &self.content {
            LayerContent::Raster(tiles) => Some(tiles),
            LayerContent::Group(_) => None,
        }
    }

    pub fn contains(&self, id: LayerId) -> bool {
        self.id == id || find_layer(self.children(), id).is_some()
    }

    pub fn visit<'a>(&'a self, visitor: &mut impl FnMut(&'a Self)) {
        visitor(self);
        for child in self.children() {
            child.visit(visitor);
        }
    }

    pub fn group_depth(&self) -> usize {
        match &self.content {
            LayerContent::Raster(_) => 0,
            LayerContent::Group(children) => {
                1 + children.iter().map(Self::group_depth).max().unwrap_or(0)
            }
        }
    }

    pub fn map_tiles<U>(&self, convert: &mut impl FnMut(LayerId, TileCoord, &T) -> U) -> Layer<U> {
        Layer {
            id: self.id,
            properties: self.properties.clone(),
            content: match &self.content {
                LayerContent::Raster(tiles) => LayerContent::Raster(
                    tiles
                        .iter()
                        .map(|(coord, tile)| (*coord, convert(self.id, *coord, tile)))
                        .collect(),
                ),
                LayerContent::Group(children) => LayerContent::Group(
                    children
                        .iter()
                        .map(|child| child.map_tiles(convert))
                        .collect(),
                ),
            },
        }
    }
}

pub fn find_layer<T>(layers: &[Layer<T>], id: LayerId) -> Option<&Layer<T>> {
    layers.iter().find_map(|layer| {
        if layer.id == id {
            Some(layer)
        } else {
            find_layer(layer.children(), id)
        }
    })
}
