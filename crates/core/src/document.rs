use std::{collections::BTreeSet, error::Error, fmt, mem, sync::Arc};

use crate::{
    Adjustment, BlendMode, DEFAULT_HISTORY_LIMIT, Event, Layer, LayerContent, LayerId, LayerMask,
    LayerProperties, MAX_NAME_LENGTH, MAX_SELECTION_RADIUS, PlaneEdit, RasterEdit, Selection,
    SelectionMode, TextSettings, TileCoord, Tiles, find_layer,
    history::{Edit, Entry, History},
};

pub const MAX_LAYERS: usize = 1000;
pub const MAX_GROUP_DEPTH: usize = 16;

#[derive(Debug, PartialEq)]
pub enum DocumentError {
    LayerNotFound(LayerId),
    NotAGroup(LayerId),
    NotARasterLayer(LayerId),
    NotAnAdjustment(LayerId),
    TextLayer(LayerId),
    NotAText(LayerId),
    InvalidText,
    NoSelection,
    EmptySelection,
    InvalidSelection,
    NoMask(LayerId),
    MaskExists(LayerId),
    InvalidAdjustment,
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
            Self::NotARasterLayer(id) => write!(formatter, "Layer {id} has no raster pixels."),
            Self::NotAnAdjustment(id) => {
                write!(formatter, "Layer {id} is not an adjustment layer.")
            }
            Self::TextLayer(id) => {
                write!(formatter, "Layer {id} is a text layer. Rasterize it first.")
            }
            Self::NotAText(id) => write!(formatter, "Layer {id} is not a text layer."),
            Self::InvalidText => write!(formatter, "A text setting is out of range."),
            Self::NoSelection => write!(formatter, "Nothing is selected."),
            Self::EmptySelection => write!(formatter, "No pixels would be selected."),
            Self::InvalidSelection => write!(formatter, "A selection value is out of range."),
            Self::NoMask(id) => write!(formatter, "Layer {id} has no mask."),
            Self::MaskExists(id) => write!(formatter, "Layer {id} already has a mask."),
            Self::InvalidAdjustment => {
                write!(formatter, "An adjustment parameter is out of range.")
            }
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

impl LayerPlace {
    pub fn requested(parent: Option<Option<LayerId>>, index: Option<usize>) -> Option<Self> {
        (parent.is_some() || index.is_some()).then(|| Self {
            parent: parent.flatten(),
            index: index.unwrap_or(usize::MAX),
        })
    }
}

#[derive(Debug)]
pub struct Document<T> {
    size: (u32, u32),
    layers: Vec<Layer<T>>,
    active: Option<LayerId>,
    selection: Option<Arc<Selection>>,
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
            selection: None,
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
        preview.selection = self.selection.clone();
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

    pub fn selection(&self) -> Option<&Arc<Selection>> {
        self.selection.as_ref()
    }

    pub fn paintable(&self, id: LayerId) -> Result<&Tiles<T>, DocumentError> {
        let layer = self.layer(id)?;
        if layer.text.is_some() {
            return Err(DocumentError::TextLayer(id));
        }
        layer.tiles().ok_or(DocumentError::NotARasterLayer(id))
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
        self.insert_raster(name, place, tiles, None)
    }

    pub fn add_text(
        &mut self,
        name: Option<String>,
        place: Option<LayerPlace>,
        text: TextSettings,
        tiles: Tiles<T>,
    ) -> Result<LayerId, DocumentError> {
        let name = name.unwrap_or_else(|| format!("Text {}", self.next_id));
        self.insert_raster(name, place, tiles, Some(text))
    }

    fn insert_raster(
        &mut self,
        name: String,
        place: Option<LayerPlace>,
        tiles: Tiles<T>,
        text: Option<TextSettings>,
    ) -> Result<LayerId, DocumentError> {
        let mut layer = Layer::raster(self.next_id, name);
        layer.content = LayerContent::Raster(tiles);
        layer.text = text;
        check_layer(&layer, self.size)?;
        self.insert(layer, place)
    }

    pub fn set_text(
        &mut self,
        id: LayerId,
        text: TextSettings,
        tiles: Tiles<T>,
    ) -> Result<(), DocumentError> {
        let layer = self.layer(id)?;
        let before = layer.tiles().filter(|_| layer.text.is_some());
        let before = before.ok_or(DocumentError::NotAText(id))?;
        let mut changes: Vec<_> = before
            .keys()
            .filter(|coord| !tiles.contains_key(coord))
            .map(|coord| (*coord, None))
            .collect();
        changes.extend(tiles.into_iter().map(|(coord, tile)| (coord, Some(tile))));
        self.replace_planes(PlaneEdit {
            rasters: vec![(id, changes)],
            texts: vec![(id, Some(text))],
            ..PlaneEdit::new(self.size)
        })
    }

    pub fn rasterize_text(&mut self, id: LayerId) -> Result<(), DocumentError> {
        if self.layer(id)?.text.is_none() {
            return Err(DocumentError::NotAText(id));
        }
        self.replace_planes(PlaneEdit {
            texts: vec![(id, None)],
            ..PlaneEdit::new(self.size)
        })
    }

    pub fn select(
        &mut self,
        next: Option<Selection>,
        mode: SelectionMode,
    ) -> Result<(), DocumentError> {
        let selection = match (&self.selection, next) {
            (Some(current), Some(next)) => current.combined(&next, mode),
            (Some(_), None) if matches!(mode, SelectionMode::Add | SelectionMode::Subtract) => {
                return Ok(());
            }
            (_, next) => next,
        };
        self.set_selection(Some(selection.ok_or(DocumentError::EmptySelection)?))
    }

    pub fn deselect(&mut self) -> Result<(), DocumentError> {
        self.set_selection(None)
    }

    pub fn invert_selection(&mut self) -> Result<(), DocumentError> {
        self.change_selection(Selection::inverted)
    }

    pub fn feather_selection(&mut self, radius: u32) -> Result<(), DocumentError> {
        self.resize_selection(radius, Selection::feathered)
    }

    pub fn grow_selection(&mut self, radius: u32) -> Result<(), DocumentError> {
        self.resize_selection(radius, Selection::expanded)
    }

    pub fn shrink_selection(&mut self, radius: u32) -> Result<(), DocumentError> {
        self.resize_selection(radius, Selection::contracted)
    }

    fn resize_selection(
        &mut self,
        radius: u32,
        resize: fn(&Selection, u32, (u32, u32)) -> Option<Selection>,
    ) -> Result<(), DocumentError> {
        if !(1..=MAX_SELECTION_RADIUS).contains(&radius) {
            return Err(DocumentError::InvalidSelection);
        }
        self.change_selection(|selection, size| resize(selection, radius, size))
    }

    fn change_selection(
        &mut self,
        change: impl FnOnce(&Selection, (u32, u32)) -> Option<Selection>,
    ) -> Result<(), DocumentError> {
        let current = self.selection.as_ref().ok_or(DocumentError::NoSelection)?;
        let changed = change(current, self.size).ok_or(DocumentError::EmptySelection)?;
        self.set_selection(Some(changed))
    }

    fn set_selection(&mut self, selection: Option<Selection>) -> Result<(), DocumentError> {
        self.replace_planes(PlaneEdit {
            selection: Some(selection.map(Arc::new)),
            ..PlaneEdit::new(self.size)
        })
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

    pub fn add_adjustment(
        &mut self,
        name: Option<String>,
        place: Option<LayerPlace>,
        adjustment: Adjustment,
    ) -> Result<LayerId, DocumentError> {
        if !adjustment.is_valid() {
            return Err(DocumentError::InvalidAdjustment);
        }
        let name = name.unwrap_or_else(|| format!("Adjustment {}", self.next_id));
        self.insert(Layer::adjustment(self.next_id, name, adjustment), place)
    }

    pub fn set_adjustment(
        &mut self,
        id: LayerId,
        adjustment: Adjustment,
    ) -> Result<(), DocumentError> {
        let previous = self
            .layer(id)?
            .grade()
            .ok_or(DocumentError::NotAnAdjustment(id))?;
        if !adjustment.is_valid() {
            return Err(DocumentError::InvalidAdjustment);
        }
        if *previous != adjustment {
            self.observed(|document| document.commit(Edit::Adjustment { id, adjustment }));
        }
        Ok(())
    }

    pub fn add_mask(&mut self, id: LayerId, reveal: bool) -> Result<(), DocumentError> {
        if self.layer(id)?.mask.is_some() {
            return Err(DocumentError::MaskExists(id));
        }
        self.replace_mask(
            id,
            LayerMask {
                reveal,
                tiles: Tiles::new(),
            },
        )
    }

    pub fn replace_mask(&mut self, id: LayerId, mask: LayerMask<T>) -> Result<(), DocumentError> {
        self.layer(id)?;
        if let Some(coord) = mask.tiles.keys().find(|coord| !coord.is_inside(self.size)) {
            return Err(DocumentError::TileOutOfBounds(*coord));
        }
        self.observed(|document| {
            document.commit(Edit::Mask {
                id,
                mask: Some(mask),
            })
        });
        Ok(())
    }

    pub fn remove_mask(&mut self, id: LayerId) -> Result<(), DocumentError> {
        if self.layer(id)?.mask.is_none() {
            return Err(DocumentError::NoMask(id));
        }
        self.observed(|document| document.commit(Edit::Mask { id, mask: None }));
        Ok(())
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
        layers: Vec<RasterEdit<T>>,
    ) -> Result<(), DocumentError> {
        self.replace_planes(PlaneEdit {
            rasters: layers,
            ..PlaneEdit::new(size)
        })
    }

    pub fn replace_mask_tiles(
        &mut self,
        id: LayerId,
        tiles: Vec<(TileCoord, Option<T>)>,
    ) -> Result<(), DocumentError> {
        self.replace_planes(PlaneEdit {
            masks: vec![(id, tiles)],
            ..PlaneEdit::new(self.size)
        })
    }

    pub fn replace_planes(&mut self, mut edit: PlaneEdit<T>) -> Result<(), DocumentError> {
        let size = edit.size;
        if size.0 == 0 || size.1 == 0 {
            return Err(DocumentError::InvalidSize);
        }
        self.validate_plane_edits(size, &mut edit.rasters, false)?;
        self.validate_plane_edits(size, &mut edit.masks, true)?;
        for (id, text) in &edit.texts {
            if self.layer(*id)?.text.is_none() {
                return Err(DocumentError::NotAText(*id));
            }
            if text.as_ref().is_some_and(|text| !text.is_valid()) {
                return Err(DocumentError::InvalidText);
            }
        }
        let selection = edit.selection.take().unwrap_or_else(|| {
            let clipped = |selection: &Arc<Selection>| selection.translated((0, 0), size);
            match &self.selection {
                Some(selection) if !selection.bounds().is_inside(size) => {
                    clipped(selection).map(Arc::new)
                }
                unchanged => unchanged.clone(),
            }
        });
        if selection
            .as_ref()
            .is_some_and(|selection| !selection.bounds().is_inside(size))
        {
            return Err(DocumentError::InvalidSelection);
        }
        edit.selection = (self.selection != selection).then_some(selection);
        edit.texts
            .retain(|(id, text)| self.layer_ref(*id).text != *text);
        if self.size != size
            || edit.selection.is_some()
            || !edit.rasters.is_empty()
            || !edit.masks.is_empty()
            || !edit.texts.is_empty()
        {
            self.observed(|document| document.commit(Edit::Pixels(edit)));
        }
        Ok(())
    }

    fn validate_plane_edits(
        &self,
        size: (u32, u32),
        layers: &mut Vec<RasterEdit<T>>,
        mask: bool,
    ) -> Result<(), DocumentError> {
        let shrinking = size.0 < self.size.0 || size.1 < self.size.1;
        let mut touched = BTreeSet::new();
        for (id, tiles) in layers.iter_mut() {
            let layer = self.layer(*id)?;
            let current = if mask {
                &layer.mask.as_ref().ok_or(DocumentError::NoMask(*id))?.tiles
            } else {
                layer.tiles().ok_or(DocumentError::NotARasterLayer(*id))?
            };
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
            if shrinking {
                tiles.extend(
                    current
                        .keys()
                        .filter(|coord| !coord.is_inside(size) && !coords.contains(coord))
                        .map(|coord| (*coord, None)),
                );
            }
        }
        if shrinking {
            for layer in &self.layers {
                layer.visit(&mut |layer| {
                    let plane = if mask {
                        layer.mask.as_ref().map(|mask| &mask.tiles)
                    } else {
                        layer.tiles()
                    };
                    if !touched.contains(&layer.id)
                        && let Some(tiles) = plane
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
                    let all = layer.has_adjustments();
                    let coords = tile_coords(layer);
                    self.mark_dirty(coords);
                    if all {
                        self.dirty.extend(TileCoord::all(self.size));
                    }
                }
                self.events.push(Event::LayerChanged { id });
                Edit::Properties {
                    id,
                    properties: previous,
                }
            }
            Edit::Pixels(PlaneEdit {
                size,
                rasters,
                masks,
                texts,
                selection,
            }) => {
                let old_size = mem::replace(&mut self.size, size);
                let rasters = self.swap_plane(rasters, false);
                let masks = self.swap_plane(masks, true);
                let texts = texts
                    .into_iter()
                    .map(|(id, text)| {
                        let previous = mem::replace(&mut self.layer_mut(id).text, text);
                        self.events.push(Event::LayerChanged { id });
                        (id, previous)
                    })
                    .collect();
                let selection = selection.map(|selection| {
                    self.events.push(Event::SelectionChanged);
                    mem::replace(&mut self.selection, selection)
                });
                if old_size != size {
                    self.dirty = TileCoord::all(size).collect();
                    self.events.push(Event::CanvasResized {
                        width: size.0,
                        height: size.1,
                    });
                }
                Edit::Pixels(PlaneEdit {
                    size: old_size,
                    rasters,
                    masks,
                    texts,
                    selection,
                })
            }
            Edit::Mask { id, mask } => {
                let layer = self.layer_mut(id);
                let previous = mem::replace(&mut layer.mask, mask);
                let all = layer.has_adjustments();
                let coords = tile_coords(layer);
                self.mark_dirty(coords);
                if all {
                    self.dirty.extend(TileCoord::all(self.size));
                }
                self.events.push(Event::LayerChanged { id });
                Edit::Mask { id, mask: previous }
            }
            Edit::Adjustment { id, adjustment } => {
                let LayerContent::Adjustment(current) = &mut self.layer_mut(id).content else {
                    unreachable!("adjustment layer checked before applying edit");
                };
                let previous = mem::replace(current, adjustment);
                self.dirty.extend(TileCoord::all(self.size));
                self.events.push(Event::LayerChanged { id });
                Edit::Adjustment {
                    id,
                    adjustment: previous,
                }
            }
        }
    }

    fn swap_plane(&mut self, layers: Vec<RasterEdit<T>>, mask: bool) -> Vec<RasterEdit<T>> {
        layers
            .into_iter()
            .map(|(id, tiles)| {
                let size = self.size;
                self.mark_dirty(
                    tiles
                        .iter()
                        .map(|(coord, _)| *coord)
                        .filter(|coord| coord.is_inside(size)),
                );
                let layer = self.layer_mut(id);
                let current = if mask {
                    &mut layer
                        .mask
                        .as_mut()
                        .expect("mask checked before applying edit")
                        .tiles
                } else {
                    let LayerContent::Raster(tiles) = &mut layer.content else {
                        unreachable!("raster layer checked before applying edit");
                    };
                    tiles
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
                self.events.push(if mask {
                    Event::MaskChanged { id }
                } else {
                    Event::PixelsChanged { id }
                });
                (id, previous)
            })
            .collect()
    }

    fn mark_layer_dirty(&mut self, layer: &Layer<T>) {
        if layer.has_adjustments() {
            self.dirty.extend(TileCoord::all(self.size));
        } else {
            self.mark_dirty(tile_coords(layer));
        }
    }

    fn layer_count(&self) -> usize {
        let mut count = 0;
        for layer in &self.layers {
            layer.visit(&mut |_| count += 1);
        }
        count
    }

    fn check_parent(&self, parent: LayerId, added_depth: usize) -> Result<(), DocumentError> {
        if !matches!(self.layer(parent)?.content, LayerContent::Group(_)) {
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
            _ => unreachable!("parents are checked before edits"),
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
            _ => None,
        }
    })
}

fn tile_coords<T>(layer: &Layer<T>) -> BTreeSet<TileCoord> {
    let mut coords = BTreeSet::new();
    layer.visit(&mut |layer| {
        coords.extend(layer.tiles().into_iter().flat_map(|tiles| tiles.keys()));
        coords.extend(layer.mask.iter().flat_map(|mask| mask.tiles.keys()));
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
    if layer
        .grade()
        .is_some_and(|adjustment| !adjustment.is_valid())
    {
        return Err(DocumentError::InvalidAdjustment);
    }
    if let Some(text) = &layer.text
        && (layer.tiles().is_none() || !text.is_valid())
    {
        return Err(DocumentError::InvalidText);
    }
    let outside = layer
        .tiles()
        .into_iter()
        .chain(layer.mask.iter().map(|mask| &mask.tiles))
        .flat_map(Tiles::keys)
        .find(|coord| !coord.is_inside(size));
    match outside {
        Some(coord) => Err(DocumentError::TileOutOfBounds(*coord)),
        None => Ok(()),
    }
}
