use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Adjustment, TextSettings, TileCoord};

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
    pub text: Option<TextSettings>,
    pub mask: Option<LayerMask<T>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayerMask<T> {
    pub reveal: bool,
    pub tiles: Tiles<T>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayerContent<T> {
    Raster(Tiles<T>),
    Group(Vec<Layer<T>>),
    Adjustment(Adjustment),
}

impl<T> Layer<T> {
    pub fn raster(id: LayerId, name: String) -> Self {
        Self {
            id,
            properties: LayerProperties::named(name),
            content: LayerContent::Raster(Tiles::new()),
            text: None,
            mask: None,
        }
    }

    pub fn group(id: LayerId, name: String) -> Self {
        Self {
            id,
            properties: LayerProperties::named(name),
            content: LayerContent::Group(Vec::new()),
            text: None,
            mask: None,
        }
    }

    pub fn adjustment(id: LayerId, name: String, adjustment: Adjustment) -> Self {
        Self {
            id,
            properties: LayerProperties::named(name),
            content: LayerContent::Adjustment(adjustment),
            text: None,
            mask: None,
        }
    }

    pub fn grade(&self) -> Option<&Adjustment> {
        match &self.content {
            LayerContent::Adjustment(adjustment) => Some(adjustment),
            _ => None,
        }
    }

    pub fn has_adjustments(&self) -> bool {
        self.grade().is_some() || self.children().iter().any(Self::has_adjustments)
    }

    pub fn children(&self) -> &[Self] {
        match &self.content {
            LayerContent::Group(children) => children,
            _ => &[],
        }
    }

    pub fn tiles(&self) -> Option<&Tiles<T>> {
        match &self.content {
            LayerContent::Raster(tiles) => Some(tiles),
            _ => None,
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
            LayerContent::Group(children) => {
                1 + children.iter().map(Self::group_depth).max().unwrap_or(0)
            }
            _ => 0,
        }
    }

    pub fn map_tiles<U>(&self, convert: &mut impl FnMut(LayerId, TileCoord, &T) -> U) -> Layer<U> {
        self.map_planes(&mut |id, coord, _, tile| convert(id, coord, tile))
    }

    pub fn map_planes<U>(
        &self,
        convert: &mut impl FnMut(LayerId, TileCoord, bool, &T) -> U,
    ) -> Layer<U> {
        let mut plane = |tiles: &Tiles<T>, is_mask| {
            tiles
                .iter()
                .map(|(coord, tile)| (*coord, convert(self.id, *coord, is_mask, tile)))
                .collect()
        };
        Layer {
            id: self.id,
            properties: self.properties.clone(),
            text: self.text.clone(),
            mask: self.mask.as_ref().map(|mask| LayerMask {
                reveal: mask.reveal,
                tiles: plane(&mask.tiles, true),
            }),
            content: match &self.content {
                LayerContent::Raster(tiles) => LayerContent::Raster(plane(tiles, false)),
                LayerContent::Group(children) => LayerContent::Group(
                    children
                        .iter()
                        .map(|child| child.map_planes(convert))
                        .collect(),
                ),
                LayerContent::Adjustment(adjustment) => {
                    LayerContent::Adjustment(adjustment.clone())
                }
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
