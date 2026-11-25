use std::{collections::BTreeSet, error::Error, fmt, mem};

use crate::{
    BlendMode, DEFAULT_HISTORY_LIMIT, Event, Layer, LayerContent, LayerId, LayerProperties,
    MAX_NAME_LENGTH, RasterEdit, TileCoord, Tiles, find_layer,
    history::{Edit, Entry, History},
};

pub const MAX_LAYERS: usize = 1000;
pub const MAX_GROUP_DEPTH: usize = 16;

#[derive(Debug, PartialEq)]
pub enum DocumentError {
    LayerNotFound(LayerId),
    NotAGroup(LayerId),
    NotARasterLayer(LayerId),
    MoveIntoItself,
    TooManyLayers,
    TooDeep,
    NameTooLong,
    InvalidOpacity,
    InvalidSize,
    DuplicateTileEdit(LayerId),
    DuplicateLayer(LayerId),
    TileOutOfBounds(TileCoord),
}

impl fmt::Display for DocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LayerNotFound(id) => write!(formatter, "Layer {id} does not exist."),
            Self::NotAGroup(id) => write!(formatter, "Layer {id} is not a group."),
            Self::NotARasterLayer(id) => write!(formatter, "Layer {id} is a group."),
            Self::MoveIntoItself => write!(formatter, "A group cannot be moved into itself."),
            Self::TooManyLayers => {
                write!(
                    formatter,
                    "A document can have at most {MAX_LAYERS} layers."
                )
            }
            Self::TooDeep => {
                write!(
                    formatter,
                    "Groups can be nested at most {MAX_GROUP_DEPTH} deep."
                )
            }
            Self::NameTooLong => {
                write!(
                    formatter,
                    "Names can be at most {MAX_NAME_LENGTH} bytes long."
                )
            }
            Self::InvalidOpacity => write!(formatter, "Opacity must be between 0 and 1."),
            Self::InvalidSize => write!(formatter, "Canvas dimensions must be positive."),
            Self::DuplicateTileEdit(id) => {
                write!(formatter, "Layer {id} has duplicate tile edits.")
            }
            Self::DuplicateLayer(id) => write!(formatter, "Layer {id} appears twice."),
            Self::TileOutOfBounds(TileCoord { x, y }) => {
                write!(formatter, "Tile {x}, {y} is outside the document.")
            }
        }
    }
}

impl Error for DocumentError {}

#[derive(Clone, Copy, Debug)]
pub struct LayerPlace {
    pub parent: Option<LayerId>,
    pub index: usize,
}

#[derive(Debug)]
pub struct Document<T> {
    size: (u32, u32),
    layers: Vec<Layer<T>>,
    active: Option<LayerId>,
    next_id: LayerId,
    history: History<T>,
    dirty: BTreeSet<TileCoord>,
    events: Vec<Event>,
}

impl<T: Clone> Document<T> {
    pub fn new(size: (u32, u32)) -> Self {
        Self {
            size,
            layers: Vec::new(),
            active: None,
            next_id: 1,
            history: History::new(DEFAULT_HISTORY_LIMIT),
            dirty: BTreeSet::new(),
            events: Vec::new(),
        }
    }

    pub fn with_layers(
        size: (u32, u32),
        layers: Vec<Layer<T>>,
        active: Option<LayerId>,
    ) -> Result<Self, DocumentError> {
        let mut ids = BTreeSet::new();
        let mut dirty = BTreeSet::new();
        let mut error = None;
        for layer in &layers {
            if layer.group_depth() > MAX_GROUP_DEPTH {
                return Err(DocumentError::TooDeep);
            }
            layer.visit(&mut |layer| {
                let checked = check_layer(layer, size);
                let duplicate = !ids.insert(layer.id);
                error = error
                    .take()
                    .or(checked.err())
                    .or(duplicate.then_some(DocumentError::DuplicateLayer(layer.id)));
            });
            dirty.append(&mut tile_coords(layer));
        }
        if let Some(error) = error {
            return Err(error);
        }
        if ids.len() > MAX_LAYERS {
            return Err(DocumentError::TooManyLayers);
        }
        if let Some(id) = active.filter(|id| !ids.contains(id)) {
            return Err(DocumentError::LayerNotFound(id));
        }
        let next_id = match ids.last() {
            Some(id) => id.checked_add(1).ok_or(DocumentError::TooManyLayers)?,
            None => 1,
        };
        Ok(Self {
            layers,
            active,
            next_id,
            dirty,
            ..Self::new(size)
        })
    }

    pub fn fork_for_preview(&self) -> Self {
        let mut preview = Self::new(self.size);
        preview.layers = self.layers.clone();
        preview.active = self.active;
        preview.next_id = self.next_id;
        preview.dirty = self.dirty.clone();
        preview.history.set_limit(0);
        preview
    }

    pub fn mark_all_dirty(&mut self) {
        for layer in &self.layers {
            self.dirty.extend(tile_coords(layer));
        }
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn layers(&self) -> &[Layer<T>] {
        &self.layers
    }

    pub fn layer(&self, id: LayerId) -> Result<&Layer<T>, DocumentError> {
        find_layer(&self.layers, id).ok_or(DocumentError::LayerNotFound(id))
    }

    pub fn active(&self) -> Option<LayerId> {
        self.active
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    pub fn history_bytes(&self) -> u64 {
        self.history.bytes()
    }

    pub fn history_limit(&self) -> u64 {
        self.history.limit()
    }

    pub fn set_history_limit(&mut self, bytes: u64) {
        self.observed(|document| document.history.set_limit(bytes));
    }

    pub fn take_dirty(&mut self) -> BTreeSet<TileCoord> {
        mem::take(&mut self.dirty)
    }

    pub fn mark_dirty(&mut self, coords: impl IntoIterator<Item = TileCoord>) {
        self.dirty.extend(coords);
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        mem::take(&mut self.events)
    }

    pub fn add_layer(
        &mut self,
        name: Option<String>,
        place: Option<LayerPlace>,
    ) -> Result<LayerId, DocumentError> {
        self.add_raster(name, place, Tiles::new())
    }

    pub fn add_raster(
        &mut self,
        name: Option<String>,
        place: Option<LayerPlace>,
        tiles: Tiles<T>,
    ) -> Result<LayerId, DocumentError> {
        let name = name.unwrap_or_else(|| format!("Layer {}", self.next_id));
        let mut layer = Layer::raster(self.next_id, name);
        layer.content = LayerContent::Raster(tiles);
        check_layer(&layer, self.size)?;
        self.insert(layer, place)
    }

    pub fn add_group(
        &mut self,
        name: Option<String>,
        place: Option<LayerPlace>,
    ) -> Result<LayerId, DocumentError> {
        let name = name.unwrap_or_else(|| format!("Group {}", self.next_id));
        let group = Layer::group(self.next_id, name);
        self.insert(group, place)
    }

    pub fn delete_layer(&mut self, id: LayerId) -> Result<(), DocumentError> {
        let (parent, index) = self.position(id)?;
        let active_is_deleted = self
            .active
            .is_some_and(|active| self.layer_ref(id).contains(active));
        self.observed(|document| {
            document.commit(Edit::Remove { id });
            if active_is_deleted {
                let siblings = document.siblings(parent);
                let neighbor = siblings.get(index).or(siblings.last());
                document.active = neighbor.map(|layer| layer.id).or(parent);
            }
        });
        Ok(())
    }

    pub fn move_layer(
        &mut self,
        id: LayerId,
        parent: Option<LayerId>,
        index: usize,
    ) -> Result<(), DocumentError> {
        let from = self.position(id)?;
        let moved = self.layer(id)?;
        if let Some(parent) = parent {
            if moved.contains(parent) {
                return Err(DocumentError::MoveIntoItself);
            }
            self.check_parent(parent, moved.group_depth())?;
        }
        let sibling_count = self.siblings(parent).len() - usize::from(from.0 == parent);
        let index = index.min(sibling_count);
        if from != (parent, index) {
            self.observed(|document| document.commit(Edit::Move { id, parent, index }));
        }
        Ok(())
    }

    pub fn rename_layer(&mut self, id: LayerId, name: String) -> Result<(), DocumentError> {
        check_name(&name)?;
        self.edit_properties(id, |properties| properties.name = name)
    }

    pub fn set_layer_visible(&mut self, id: LayerId, visible: bool) -> Result<(), DocumentError> {
        self.edit_properties(id, |properties| properties.visible = visible)
    }

    pub fn set_layer_opacity(&mut self, id: LayerId, opacity: f32) -> Result<(), DocumentError> {
        if !(0.0..=1.0).contains(&opacity) {
            return Err(DocumentError::InvalidOpacity);
        }
        self.edit_properties(id, |properties| properties.opacity = opacity)
    }

    pub fn set_layer_blend_mode(
        &mut self,
        id: LayerId,
        blend_mode: BlendMode,
    ) -> Result<(), DocumentError> {
        self.edit_properties(id, |properties| properties.blend_mode = blend_mode)
    }

    pub fn select_layer(&mut self, id: LayerId) -> Result<(), DocumentError> {
        self.layer(id)?;
        self.observed(|document| document.active = Some(id));
        Ok(())
    }

    pub fn replace_tiles(
        &mut self,
        id: LayerId,
        tiles: Vec<(TileCoord, Option<T>)>,
    ) -> Result<(), DocumentError> {
        self.replace_rasters(self.size, vec![(id, tiles)])
    }

    pub fn replace_rasters(
        &mut self,
        size: (u32, u32),
        mut layers: Vec<RasterEdit<T>>,
    ) -> Result<(), DocumentError> {
        if size.0 == 0 || size.1 == 0 {
            return Err(DocumentError::InvalidSize);
        }
        let mut touched = BTreeSet::new();
        for (id, tiles) in &mut layers {
            let current = self
                .layer(*id)?
                .tiles()
                .ok_or(DocumentError::NotARasterLayer(*id))?;
            if !touched.insert(*id) {
                return Err(DocumentError::DuplicateTileEdit(*id));
            }
            let mut coords = BTreeSet::new();
            for (coord, tile) in tiles.iter() {
                if !coords.insert(*coord) {
                    return Err(DocumentError::DuplicateTileEdit(*id));
                }
                let bounds = if tile.is_some() { size } else { self.size };
                if !coord.is_inside(bounds) {
                    return Err(DocumentError::TileOutOfBounds(*coord));
                }
            }
            if size.0 < self.size.0 || size.1 < self.size.1 {
                tiles.extend(
                    current
                        .keys()
                        .filter(|coord| !coord.is_inside(size) && !coords.contains(coord))
                        .map(|coord| (*coord, None)),
                );
            }
        }
        if size.0 < self.size.0 || size.1 < self.size.1 {
            for layer in &self.layers {
                layer.visit(&mut |layer| {
                    if !touched.contains(&layer.id)
                        && let Some(tiles) = layer.tiles()
                    {
                        layers.push((
                            layer.id,
                            tiles
                                .keys()
                                .filter(|coord| !coord.is_inside(size))
                                .map(|coord| (*coord, None))
                                .collect(),
                        ));
                    }
                });
            }
        }
        layers.retain(|(_, tiles)| !tiles.is_empty());
        if self.size != size || !layers.is_empty() {
            self.observed(|document| document.commit(Edit::Rasters { size, layers }));
        }
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        self.observed(|document| {
            let Some(entry) = document.history.pop_undo() else {
                return false;
            };
            let reverse = document.revert(entry);
            document.history.push_redo(reverse);
            true
        })
    }

    pub fn redo(&mut self) -> bool {
        self.observed(|document| {
            let Some(entry) = document.history.pop_redo() else {
                return false;
            };
            let reverse = document.revert(entry);
            document.history.push_undo(reverse);
            true
        })
    }

    fn insert(
        &mut self,
        layer: Layer<T>,
        place: Option<LayerPlace>,
    ) -> Result<LayerId, DocumentError> {
        let id = layer.id;
        check_name(&layer.properties.name)?;
        if self.layer_count() >= MAX_LAYERS {
            return Err(DocumentError::TooManyLayers);
        }
        let next_id = id.checked_add(1).ok_or(DocumentError::TooManyLayers)?;

        let (parent, index) = match (place, self.active) {
            (Some(LayerPlace { parent, index }), _) => (parent, index),
            (None, Some(active)) => {
                let (parent, index) = self.position(active)?;
                (parent, index + 1)
            }
            (None, None) => (None, 0),
        };
        if let Some(parent) = parent {
            self.check_parent(parent, layer.group_depth())?;
        }
        let index = index.min(self.siblings(parent).len());
        self.next_id = next_id;
        self.observed(|document| {
            document.commit(Edit::Insert {
                parent,
                index,
                layer: Box::new(layer),
            });
            document.active = Some(id);
        });
        Ok(id)
    }

    fn edit_properties(
        &mut self,
        id: LayerId,
        change: impl FnOnce(&mut LayerProperties),
    ) -> Result<(), DocumentError> {
        let current = &self.layer(id)?.properties;
        let mut properties = current.clone();
        change(&mut properties);
        if properties != *current {
            self.observed(|document| document.commit(Edit::Properties { id, properties }));
        }
        Ok(())
    }

    fn observed<R>(&mut self, change: impl FnOnce(&mut Self) -> R) -> R {
        let active = self.active;
        let history = (self.can_undo(), self.can_redo());
        let result = change(self);
        if self.active != active {
            self.events
                .push(Event::ActiveLayerChanged { id: self.active });
        }
        let (can_undo, can_redo) = (self.can_undo(), self.can_redo());
        if history != (can_undo, can_redo) {
            self.events
                .push(Event::HistoryChanged { can_undo, can_redo });
        }
        result
    }

    fn commit(&mut self, edit: Edit<T>) {
        let active = self.active;
        let edit = self.apply(edit);
        self.history.record(Entry { edit, active });
    }

    fn revert(&mut self, entry: Entry<T>) -> Entry<T> {
        let active = mem::replace(&mut self.active, entry.active);
        let edit = self.apply(entry.edit);
        Entry { edit, active }
    }

    fn apply(&mut self, edit: Edit<T>) -> Edit<T> {
        match edit {
            Edit::Insert {
                parent,
                index,
                layer,
            } => {
                let id = layer.id;
                self.mark_layer_dirty(&layer);
                self.siblings_mut(parent).insert(index, *layer);
                self.events.push(Event::LayerAdded { id });
                Edit::Remove { id }
            }
            Edit::Remove { id } => {
                let (parent, index, layer) = self.detach(id);
                self.mark_layer_dirty(&layer);
                self.events.push(Event::LayerRemoved { id });
                Edit::Insert {
                    parent,
                    index,
                    layer: Box::new(layer),
                }
            }
            Edit::Move { id, parent, index } => {
                let (old_parent, old_index, layer) = self.detach(id);
                self.mark_layer_dirty(&layer);
                self.siblings_mut(parent).insert(index, layer);
                self.events.push(Event::LayerMoved { id });
                Edit::Move {
                    id,
                    parent: old_parent,
                    index: old_index,
                }
            }
            Edit::Properties { id, properties } => {
                let layer = self.layer_mut(id);
                let previous = mem::replace(&mut layer.properties, properties);
                if previous.appearance() != layer.properties.appearance() {
                    let coords = tile_coords(layer);
                    self.mark_dirty(coords);
                }
                self.events.push(Event::LayerChanged { id });
                Edit::Properties {
                    id,
                    properties: previous,
                }
            }
            Edit::Rasters { size, layers } => {
                let old_size = mem::replace(&mut self.size, size);
                let layers = layers
                    .into_iter()
                    .map(|(id, tiles)| {
                        self.mark_dirty(
                            tiles
                                .iter()
                                .map(|(coord, _)| *coord)
                                .filter(|coord| coord.is_inside(size)),
                        );
                        let LayerContent::Raster(current) = &mut self.layer_mut(id).content else {
                            unreachable!("tile edits are only recorded for raster layers");
                        };
                        let previous = tiles
                            .into_iter()
                            .map(|(coord, tile)| {
                                let previous = match tile {
                                    Some(tile) => current.insert(coord, tile),
                                    None => current.remove(&coord),
                                };
                                (coord, previous)
                            })
                            .collect();
                        self.events.push(Event::PixelsChanged { id });
                        (id, previous)
                    })
                    .collect();
                if old_size != size {
                    self.dirty.retain(|coord| coord.is_inside(size));
                    self.mark_all_dirty();
                    self.events.push(Event::CanvasResized {
                        width: size.0,
                        height: size.1,
                    });
                }
                Edit::Rasters {
                    size: old_size,
                    layers,
                }
            }
        }
    }

    fn mark_layer_dirty(&mut self, layer: &Layer<T>) {
        self.mark_dirty(tile_coords(layer));
    }

    fn layer_count(&self) -> usize {
        let mut count = 0;
        for layer in &self.layers {
            layer.visit(&mut |_| count += 1);
        }
        count
    }

    fn check_parent(&self, parent: LayerId, added_depth: usize) -> Result<(), DocumentError> {
        if let LayerContent::Raster(_) = self.layer(parent)?.content {
            return Err(DocumentError::NotAGroup(parent));
        }
        let mut depth = added_depth;
        let mut ancestor = Some(parent);
        while let Some(id) = ancestor {
            depth += 1;
            ancestor = self.position(id)?.0;
        }
        if depth > MAX_GROUP_DEPTH {
            return Err(DocumentError::TooDeep);
        }
        Ok(())
    }

    fn position(&self, id: LayerId) -> Result<(Option<LayerId>, usize), DocumentError> {
        position(&self.layers, None, id).ok_or(DocumentError::LayerNotFound(id))
    }

    fn layer_ref(&self, id: LayerId) -> &Layer<T> {
        find_layer(&self.layers, id).expect("layer ids are checked before edits")
    }

    fn layer_mut(&mut self, id: LayerId) -> &mut Layer<T> {
        find_layer_mut(&mut self.layers, id).expect("layer ids are checked before edits")
    }

    fn siblings(&self, parent: Option<LayerId>) -> &[Layer<T>] {
        match parent {
            Some(id) => self.layer_ref(id).children(),
            None => &self.layers,
        }
    }

    fn siblings_mut(&mut self, parent: Option<LayerId>) -> &mut Vec<Layer<T>> {
        let Some(id) = parent else {
            return &mut self.layers;
        };
        match &mut self.layer_mut(id).content {
            LayerContent::Group(children) => children,
            LayerContent::Raster(_) => unreachable!("parents are checked before edits"),
        }
    }

    fn detach(&mut self, id: LayerId) -> (Option<LayerId>, usize, Layer<T>) {
        let (parent, index) = self
            .position(id)
            .expect("layer ids are checked before edits");
        (parent, index, self.siblings_mut(parent).remove(index))
    }
}

fn position<T>(
    layers: &[Layer<T>],
    parent: Option<LayerId>,
    id: LayerId,
) -> Option<(Option<LayerId>, usize)> {
    layers.iter().enumerate().find_map(|(index, layer)| {
        if layer.id == id {
            Some((parent, index))
        } else {
            position(layer.children(), Some(layer.id), id)
        }
    })
}

fn find_layer_mut<T>(layers: &mut [Layer<T>], id: LayerId) -> Option<&mut Layer<T>> {
    layers.iter_mut().find_map(|layer| {
        if layer.id == id {
            return Some(layer);
        }
        match &mut layer.content {
            LayerContent::Group(children) => find_layer_mut(children, id),
            LayerContent::Raster(_) => None,
        }
    })
}

fn tile_coords<T>(layer: &Layer<T>) -> BTreeSet<TileCoord> {
    let mut coords = BTreeSet::new();
    layer.visit(&mut |layer| {
        coords.extend(layer.tiles().into_iter().flat_map(|tiles| tiles.keys()));
    });
    coords
}

fn check_name(name: &str) -> Result<(), DocumentError> {
    if name.len() > MAX_NAME_LENGTH {
        return Err(DocumentError::NameTooLong);
    }
    Ok(())
}

fn check_layer<T>(layer: &Layer<T>, size: (u32, u32)) -> Result<(), DocumentError> {
    check_name(&layer.properties.name)?;
    if !(0.0..=1.0).contains(&layer.properties.opacity) {
        return Err(DocumentError::InvalidOpacity);
    }
    let outside = layer
        .tiles()
        .and_then(|tiles| tiles.keys().find(|coord| !coord.is_inside(size)));
    match outside {
        Some(coord) => Err(DocumentError::TileOutOfBounds(*coord)),
        None => Ok(()),
    }
}
