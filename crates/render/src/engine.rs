use std::{collections::BTreeSet, rc::Rc, sync::Arc};

use bytemuck::{Pod, Zeroable};
use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use nijimi_core::{
    Adjustment, Brush, BrushDynamics, Command, CubeLut, Document, DocumentError, Event, Layer,
    LayerContent, LayerId, LayerMask, LayerPlace, PixelRect, RasterEdit, StrokePoint, TILE_SIZE,
    TileCoord, Tiles, Viewport,
};
use wgpu::{BindingResource::TextureView, util::DeviceExt};

use crate::{
    EngineError,
    composite::Composite,
    fill::{Fill, Filled},
    geometry::{Affine, remap},
    gpu::{CLEAR, Gpu, IMAGE_FORMAT, begin_pass, draw_fullscreen, texture_descriptor},
    project::{LoadedProject, ProjectLoader, ProjectSnapshot},
    readback::Readback,
    stroke::{ActiveStroke, StrokeMode},
    tile::{Tile, TileRef},
};

const FIRST_LAYER: LayerId = 1;
const RGBA_BYTES: usize = 4;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ViewUniform {
    pan: [f32; 2],
    zoom: f32,
    padding: f32,
}

#[derive(Clone)]
enum PreviewEdit {
    Opacity {
        id: LayerId,
        opacity: f32,
    },
    Adjustment {
        id: LayerId,
        adjustment: Adjustment,
    },
    Pixels {
        size: (u32, u32),
        rasters: Vec<RasterEdit<TileRef>>,
        masks: Vec<RasterEdit<TileRef>>,
    },
}

impl PreviewEdit {
    fn apply(self, document: &mut Document<TileRef>) -> Result<(), DocumentError> {
        match self {
            Self::Opacity { id, opacity } => document.set_layer_opacity(id, opacity),
            Self::Adjustment { id, adjustment } => document.set_adjustment(id, adjustment),
            Self::Pixels {
                size,
                rasters,
                masks,
            } => document.replace_planes(size, rasters, masks),
        }
    }
}

struct Preview {
    edit: PreviewEdit,
    document: Document<TileRef>,
    dirty: BTreeSet<TileCoord>,
}

pub struct Engine {
    gpu: Rc<Gpu>,
    document: Document<TileRef>,
    composite: Composite,
    view_buffer: wgpu::Buffer,
    stroke: Option<ActiveStroke>,
    preview: Option<Preview>,
    revision: u64,
    coverage_pool: Vec<Tile>,
    events: UnboundedSender<Event>,
}

impl Engine {
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface_format: wgpu::TextureFormat,
        size: (u32, u32),
    ) -> Result<(Self, UnboundedReceiver<Event>), EngineError> {
        let (events, receiver) = unbounded();
        let gpu = Rc::new(Gpu::new(device, queue, surface_format, &events));
        gpu.check_size(size)?;
        let view_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("view"),
            size: size_of::<ViewUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let composite = Composite::new(&gpu, size, &view_buffer);
        let layer = Layer::raster(FIRST_LAYER, "Layer 1".into());
        let document = Document::with_layers(size, vec![layer], Some(FIRST_LAYER))?;
        let engine = Self {
            gpu,
            document,
            composite,
            view_buffer,
            stroke: None,
            preview: None,
            revision: 0,
            coverage_pool: Vec::new(),
            events,
        };
        Ok((engine, receiver))
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.gpu.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.gpu.queue
    }

    pub fn document(&self) -> &Document<TileRef> {
        self.preview
            .as_ref()
            .map_or(&self.document, |preview| &preview.document)
    }

    pub fn committed_document(&self) -> &Document<TileRef> {
        &self.document
    }

    pub fn is_device_lost(&self) -> bool {
        self.gpu.check_device().is_err()
    }

    pub fn set_history_limit(&mut self, bytes: u64) {
        self.document.set_history_limit(bytes);
        self.publish_events();
    }

    pub fn dispatch(&mut self, command: Command) -> Result<Option<LayerId>, EngineError> {
        self.end_stroke()?;
        self.cancel_preview();
        let result = self.execute(command);
        self.publish_events();
        result
    }

    pub fn preview(&mut self, command: Command) -> Result<(), EngineError> {
        self.end_stroke()?;
        let edit = self.prepare_edit(command)?;
        let mut document = self.document.fork_for_preview();
        edit.clone().apply(&mut document)?;
        let mut dirty = document.take_dirty();
        if let Some(previous) = &self.preview {
            dirty.extend(previous.dirty.iter().copied());
        }
        let size = document.size();
        document.mark_dirty(dirty.iter().copied().filter(|coord| coord.is_inside(size)));
        self.preview = Some(Preview {
            edit,
            document,
            dirty,
        });
        let _ = self.events.unbounded_send(Event::PreviewChanged);
        Ok(())
    }

    pub fn commit_preview(&mut self) -> Result<(), EngineError> {
        self.gpu.check_device()?;
        if let Some(preview) = self.take_preview() {
            preview.edit.apply(&mut self.document)?;
            self.publish_events();
        }
        Ok(())
    }

    pub fn cancel_preview(&mut self) {
        self.take_preview();
    }

    fn take_preview(&mut self) -> Option<Preview> {
        let preview = self.preview.take()?;
        let size = self.document.size();
        self.document.mark_dirty(
            preview
                .dirty
                .iter()
                .copied()
                .filter(|coord| coord.is_inside(size)),
        );
        let _ = self.events.unbounded_send(Event::PreviewChanged);
        Some(preview)
    }

    pub fn begin_stroke(&mut self, brush: Brush, start: StrokePoint) -> Result<(), EngineError> {
        self.begin_brush_stroke(brush, BrushDynamics::default(), false, start)
    }

    pub fn begin_brush_stroke(
        &mut self,
        brush: Brush,
        dynamics: BrushDynamics,
        erase: bool,
        start: StrokePoint,
    ) -> Result<(), EngineError> {
        self.end_stroke()?;
        self.cancel_preview();
        let layer = self.document.active().ok_or(EngineError::NoActiveLayer)?;
        self.begin_stroke_on(layer, brush, dynamics, StrokeMode::painting(erase), start)
    }

    pub fn begin_mask_stroke(
        &mut self,
        id: LayerId,
        brush: Brush,
        dynamics: BrushDynamics,
        value: f32,
        start: StrokePoint,
    ) -> Result<(), EngineError> {
        self.end_stroke()?;
        self.cancel_preview();
        self.begin_stroke_on(id, brush, dynamics, StrokeMode::Mask(value), start)
    }

    pub fn extend_stroke(&mut self, point: StrokePoint) {
        if let Some(stroke) = &mut self.stroke {
            stroke.extend(point);
        }
    }

    pub fn end_stroke(&mut self) -> Result<(), EngineError> {
        self.gpu.check_device()?;
        if let Some(stroke) = &mut self.stroke {
            stroke.complete();
            self.flush()?;
        }
        if let Some(stroke) = self.stroke.take() {
            let layer = stroke.layer();
            let mask = stroke.is_mask();
            let tiles = stroke.finish(&mut self.coverage_pool);
            if mask {
                self.document.replace_mask_tiles(layer, tiles)?;
            } else {
                self.document.replace_tiles(layer, tiles)?;
            }
            self.document.take_dirty();
            self.publish_events();
        }
        Ok(())
    }

    pub fn cancel_stroke(&mut self) {
        if let Some(stroke) = self.stroke.take() {
            self.document.mark_dirty(stroke.touched());
            stroke.finish(&mut self.coverage_pool);
        }
    }

    pub fn load_rgba(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<(), EngineError> {
        let size = (width, height);
        self.check_pixels(size, rgba, RGBA_BYTES)?;
        let mut layer = Layer::raster(FIRST_LAYER, "Background".into());
        layer.content = LayerContent::Raster(self.import_tiles(size, rgba, size, (0, 0), false));
        let document = Document::with_layers(size, vec![layer], Some(FIRST_LAYER))?;
        let composite = Composite::new(&self.gpu, size, &self.view_buffer);
        self.open(LoadedProject {
            document,
            composite,
        });
        Ok(())
    }

    pub fn import_rgba(
        &mut self,
        size: (u32, u32),
        rgba: &[u8],
        position: (i32, i32),
        name: Option<String>,
        place: Option<LayerPlace>,
    ) -> Result<LayerId, EngineError> {
        self.check_pixels(size, rgba, RGBA_BYTES)?;
        self.end_stroke()?;
        self.cancel_preview();
        let tiles = self.import_tiles(size, rgba, self.document.size(), position, false);
        let id = self.document.add_raster(name, place, tiles)?;
        self.publish_events();
        Ok(id)
    }

    pub fn set_mask_alpha(&mut self, id: LayerId, alpha: &[u8]) -> Result<(), EngineError> {
        let size = self.document.size();
        self.check_pixels(size, alpha, 1)?;
        self.document.layer(id)?;
        self.end_stroke()?;
        self.cancel_preview();
        let tiles = self.import_tiles(size, alpha, size, (0, 0), true);
        self.document.replace_mask(
            id,
            LayerMask {
                reveal: false,
                tiles,
            },
        )?;
        self.publish_events();
        Ok(())
    }

    fn check_pixels(
        &self,
        size: (u32, u32),
        pixels: &[u8],
        channels: usize,
    ) -> Result<(), EngineError> {
        self.gpu.check_device()?;
        self.gpu.check_size(size)?;
        let (width, height) = size;
        let expected = u64::from(width) * u64::from(height) * channels as u64;
        let actual = pixels.len() as u64;
        if actual != expected {
            return Err(EngineError::PixelCount { expected, actual });
        }

        Ok(())
    }

    pub fn fill(
        &mut self,
        id: LayerId,
        seed: (u32, u32),
        color: [f32; 4],
        tolerance: f32,
    ) -> Result<Fill, EngineError> {
        if !color
            .into_iter()
            .chain([tolerance])
            .all(|v| (0.0..=1.0).contains(&v))
        {
            return Err(EngineError::InvalidFill);
        }
        self.end_stroke()?;
        self.cancel_preview();
        let size = self.document.size();
        if seed.0 >= size.0 || seed.1 >= size.1 {
            return Err(EngineError::InvalidRegion);
        }
        let tiles = self
            .document
            .layer(id)?
            .tiles()
            .ok_or(DocumentError::NotARasterLayer(id))?;
        Ok(Fill {
            gpu: self.gpu.clone(),
            revision: self.revision,
            id,
            size,
            seed,
            color,
            tolerance,
            tiles: tiles.clone(),
        })
    }

    pub fn commit_fill(&mut self, result: Filled) -> Result<(), EngineError> {
        self.gpu.check_device()?;
        if !Rc::ptr_eq(&self.gpu, &result.gpu)
            || self.revision != result.revision
            || self.stroke.is_some()
            || self.preview.is_some()
        {
            return Err(EngineError::StaleFill);
        }
        self.document.replace_tiles(result.id, result.tiles)?;
        self.publish_events();
        Ok(())
    }

    pub fn import_cube_lut(
        &mut self,
        text: &str,
        name: Option<String>,
        place: Option<LayerPlace>,
    ) -> Result<LayerId, EngineError> {
        let table = CubeLut::parse(text)?;
        let name = name.or_else(|| table.info.title.clone());
        self.end_stroke()?;
        self.cancel_preview();
        let id = self.document.add_adjustment(
            name,
            place,
            Adjustment::Lut {
                table: Arc::new(table),
            },
        )?;
        self.publish_events();
        Ok(id)
    }

    pub fn set_cube_lut(&mut self, id: LayerId, text: &str) -> Result<(), EngineError> {
        let adjustment = Adjustment::Lut {
            table: Arc::new(CubeLut::parse(text)?),
        };
        self.dispatch(Command::SetAdjustment { id, adjustment })?;
        Ok(())
    }

    pub fn save_project(&self) -> Result<ProjectSnapshot, EngineError> {
        self.gpu.check_device()?;
        Ok(ProjectSnapshot {
            gpu: self.gpu.clone(),
            size: self.document.size(),
            layers: self.document.layers().to_vec(),
            active: self.document.active(),
        })
    }

    pub fn load_project(&self, file: Vec<u8>) -> Result<ProjectLoader, EngineError> {
        self.gpu.check_device()?;
        Ok(ProjectLoader {
            gpu: self.gpu.clone(),
            view_buffer: self.view_buffer.clone(),
            file,
        })
    }

    pub fn open(&mut self, project: LoadedProject) {
        self.cancel_stroke();
        self.cancel_preview();
        self.revision = self.revision.wrapping_add(1);
        let history_limit = self.document.history_limit();
        self.document = project.document;
        self.document.set_history_limit(history_limit);
        self.composite = project.composite;
        let _ = self.events.unbounded_send(Event::DocumentReplaced);
    }

    pub fn present(
        &mut self,
        target: &wgpu::TextureView,
        viewport: &Viewport,
    ) -> Result<(), EngineError> {
        self.flush()?;
        let pan = viewport.pan();
        self.gpu.queue.write_buffer(
            &self.view_buffer,
            0,
            bytemuck::bytes_of(&ViewUniform {
                pan: [pan.x, pan.y],
                zoom: viewport.zoom(),
                padding: 0.0,
            }),
        );
        self.gpu.submit(|encoder| {
            draw_fullscreen(
                encoder,
                target,
                &self.gpu.pipelines.present,
                self.composite.present_bindings(),
            );
        });
        Ok(())
    }

    pub fn read_rgba(&mut self) -> Result<Readback, EngineError> {
        self.read_region(None)
    }

    pub fn read_region(&mut self, region: Option<PixelRect>) -> Result<Readback, EngineError> {
        let rect = checked_region(self.document().size(), region)?;
        self.flush()?;
        Ok(self.export_view(self.composite.view(), rect))
    }

    pub fn read_layer(
        &mut self,
        id: LayerId,
        region: Option<PixelRect>,
    ) -> Result<Readback, EngineError> {
        let size = self.document().size();
        let rect = checked_region(size, region)?;
        if self.document().layer(id)?.grade().is_some() {
            return Err(EngineError::AdjustmentExport);
        }
        self.flush()?;
        let mut layer = self.document().layer(id)?.clone();
        layer.properties.visible = true;
        layer.properties.opacity = 1.0;
        let mut document = Document::with_layers(size, vec![layer], Some(id))?;
        let mut dirty = document.take_dirty();
        if let Some(stroke) = &self.stroke
            && document.layer(stroke.layer()).is_ok()
        {
            dirty.extend(stroke.touched());
        }
        let mut composite = Composite::new(&self.gpu, size, &self.view_buffer);
        if !dirty.is_empty() {
            self.gpu.submit(|encoder| {
                composite.update(&self.gpu, encoder, &document, self.stroke.as_ref(), &dirty);
            });
        }
        Ok(self.export_view(composite.view(), rect))
    }

    pub fn read_mask(
        &mut self,
        id: LayerId,
        region: Option<PixelRect>,
    ) -> Result<Readback, EngineError> {
        let size = self.document().size();
        let rect = checked_region(size, region)?;
        self.flush()?;
        let mask = self
            .document()
            .layer(id)?
            .mask
            .as_ref()
            .ok_or(DocumentError::NoMask(id))?;
        let coords =
            (rect.y / TILE_SIZE..(rect.y + rect.height).div_ceil(TILE_SIZE)).flat_map(|y| {
                (rect.x / TILE_SIZE..(rect.x + rect.width).div_ceil(TILE_SIZE))
                    .map(move |x| TileCoord::new(x, y))
            });
        let tiles = coords
            .filter_map(|coord| {
                let tile = self
                    .stroke
                    .as_ref()
                    .and_then(|stroke| stroke.result(id, coord, true))
                    .or(mask.tiles.get(&coord));
                tile.or_else(|| mask.reveal.then(|| self.gpu.mask_default(true)))
                    .map(|tile| (coord, tile.clone()))
            })
            .collect();
        let mut layer = Layer::raster(id, String::new());
        layer.content = LayerContent::Raster(tiles);
        let mut document = Document::with_layers(size, vec![layer], Some(id))?;
        let dirty = document.take_dirty();
        let mut composite = Composite::new(&self.gpu, size, &self.view_buffer);
        if !dirty.is_empty() {
            self.gpu.submit(|encoder| {
                composite.update(&self.gpu, encoder, &document, None, &dirty);
            });
        }
        Ok(self.export_view(composite.view(), rect))
    }

    fn export_view(&self, source: &wgpu::TextureView, rect: PixelRect) -> Readback {
        let export = self.gpu.device.create_texture(&texture_descriptor(
            "export",
            rect.size(),
            IMAGE_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        ));
        let uniform = self
            .gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("export origin"),
                contents: bytemuck::bytes_of(&[rect.x as i32, rect.y as i32, 0, 0]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bindings = self.gpu.bind(
            &self.gpu.layouts.export,
            &[TextureView(source), uniform.as_entire_binding()],
        );
        let readback = self.gpu.submit(|encoder| {
            draw_fullscreen(
                encoder,
                &export.create_view(&Default::default()),
                &self.gpu.pipelines.export,
                &bindings,
            );
            Readback::record_image(&self.gpu.device, encoder, &export)
        });
        export.destroy();
        readback
    }

    pub fn read_texture(&self, texture: &wgpu::Texture) -> Readback {
        self.gpu
            .submit(|encoder| Readback::record_image(&self.gpu.device, encoder, texture))
    }

    fn execute(&mut self, command: Command) -> Result<Option<LayerId>, EngineError> {
        let document = &mut self.document;
        match command {
            Command::AddLayer {
                name,
                parent,
                index,
            } => return Ok(Some(document.add_layer(name, place(parent, index))?)),
            Command::AddGroup {
                name,
                parent,
                index,
            } => return Ok(Some(document.add_group(name, place(parent, index))?)),
            Command::AddAdjustment {
                name,
                parent,
                index,
                adjustment,
            } => {
                return Ok(Some(document.add_adjustment(
                    name,
                    place(parent, index),
                    adjustment,
                )?));
            }
            Command::SetAdjustment { id, adjustment } => document.set_adjustment(id, adjustment)?,
            Command::AddMask { id, reveal } => document.add_mask(id, reveal)?,
            Command::RemoveMask { id } => document.remove_mask(id)?,
            Command::PaintMaskStroke {
                id,
                value,
                brush,
                points,
                dynamics,
            } => {
                self.paint_stroke(id, brush, dynamics, StrokeMode::Mask(value), &points)?;
            }
            Command::DeleteLayer { id } => document.delete_layer(id)?,
            Command::MoveLayer { id, parent, index } => document.move_layer(id, parent, index)?,
            Command::RenameLayer { id, name } => document.rename_layer(id, name)?,
            Command::SetLayerVisible { id, visible } => document.set_layer_visible(id, visible)?,
            Command::SetLayerOpacity { id, opacity } => document.set_layer_opacity(id, opacity)?,
            Command::SetLayerBlendMode { id, blend_mode } => {
                document.set_layer_blend_mode(id, blend_mode)?;
            }
            Command::SelectLayer { id } => document.select_layer(id)?,
            Command::PaintStroke {
                layer,
                brush,
                points,
            } => self.paint_stroke(
                layer,
                brush,
                BrushDynamics::default(),
                StrokeMode::Paint,
                &points,
            )?,
            Command::PaintWithDynamics {
                layer,
                brush,
                points,
                dynamics,
            } => {
                self.paint_stroke(layer, brush, dynamics, StrokeMode::Paint, &points)?;
            }
            Command::EraseStroke {
                layer,
                brush,
                points,
                dynamics,
            } => {
                self.paint_stroke(layer, brush, dynamics, StrokeMode::Erase, &points)?;
            }
            edit @ (Command::Crop { .. }
            | Command::ResizeCanvas { .. }
            | Command::ExtendCanvas { .. }
            | Command::TransformLayer { .. }) => {
                self.prepare_edit(edit)?.apply(&mut self.document)?;
            }
            Command::Undo => {
                document.undo();
            }
            Command::Redo => {
                document.redo();
            }
        }
        Ok(None)
    }

    fn prepare_edit(&self, command: Command) -> Result<PreviewEdit, EngineError> {
        let old_size = self.document.size();
        let (size, affine, layers) = match command {
            Command::SetLayerOpacity { id, opacity } => {
                return Ok(PreviewEdit::Opacity { id, opacity });
            }
            Command::SetAdjustment { id, adjustment } => {
                return Ok(PreviewEdit::Adjustment { id, adjustment });
            }
            Command::Crop { rect } => {
                let rect = checked_region(old_size, Some(rect))?;
                (
                    rect.size(),
                    Affine::translation(-(rect.x as i32), -(rect.y as i32)),
                    self.document.layers(),
                )
            }
            Command::ResizeCanvas {
                width,
                height,
                x,
                y,
            } => (
                (width, height),
                Affine::translation(x, y),
                self.document.layers(),
            ),
            Command::ExtendCanvas {
                left,
                top,
                right,
                bottom,
            } => {
                let size = (
                    old_size.0.saturating_add(left).saturating_add(right),
                    old_size.1.saturating_add(top).saturating_add(bottom),
                );
                (
                    size,
                    Affine::translation(left as i32, top as i32),
                    self.document.layers(),
                )
            }
            Command::TransformLayer { id, transform } => {
                let layer = self.document.layer(id)?;
                (
                    old_size,
                    Affine::from_transform(transform, old_size)?,
                    std::slice::from_ref(layer),
                )
            }
            _ => return Err(EngineError::UnsupportedPreview),
        };
        self.gpu.check_size(size)?;
        let mut rasters = Vec::new();
        let mut masks = Vec::new();
        let changes = |before: &Tiles<TileRef>, reveal| {
            let after = remap(&self.gpu, before, old_size, size, affine, reveal);
            let mut changes: Vec<_> = before
                .keys()
                .filter(|coord| !after.contains_key(coord))
                .map(|coord| (*coord, None))
                .collect();
            changes.extend(
                after
                    .into_iter()
                    .filter(|(coord, tile)| {
                        !before
                            .get(coord)
                            .is_some_and(|previous| Rc::ptr_eq(previous, tile))
                    })
                    .map(|(coord, tile)| (coord, Some(tile))),
            );
            changes
        };
        for layer in layers {
            layer.visit(&mut |layer| {
                if let Some(before) = layer.tiles() {
                    rasters.push((layer.id, changes(before, false)));
                }
                if let Some(mask) = &layer.mask {
                    masks.push((layer.id, changes(&mask.tiles, mask.reveal)));
                }
            });
        }
        Ok(PreviewEdit::Pixels {
            size,
            rasters,
            masks,
        })
    }

    fn paint_stroke(
        &mut self,
        layer: LayerId,
        brush: Brush,
        dynamics: BrushDynamics,
        mode: StrokeMode,
        points: &[StrokePoint],
    ) -> Result<(), EngineError> {
        let [start, rest @ ..] = points else {
            return Err(EngineError::InvalidStroke);
        };
        if !points.iter().all(StrokePoint::is_valid) {
            return Err(EngineError::InvalidStroke);
        }
        self.begin_stroke_on(layer, brush, dynamics, mode, *start)?;
        for point in rest {
            self.extend_stroke(*point);
        }
        self.end_stroke()
    }

    fn begin_stroke_on(
        &mut self,
        layer: LayerId,
        brush: Brush,
        dynamics: BrushDynamics,
        mode: StrokeMode,
        start: StrokePoint,
    ) -> Result<(), EngineError> {
        if !brush.is_valid() || !dynamics.is_valid() {
            return Err(EngineError::InvalidBrush);
        }
        if !start.is_valid() {
            return Err(EngineError::InvalidStroke);
        }
        let target = self.document.layer(layer)?;
        if let StrokeMode::Mask(value) = mode {
            if target.mask.is_none() {
                return Err(DocumentError::NoMask(layer).into());
            }
            if !(0.0..=1.0).contains(&value) {
                return Err(EngineError::InvalidMaskValue);
            }
        } else if target.tiles().is_none() {
            return Err(DocumentError::NotARasterLayer(layer).into());
        }
        self.stroke = Some(ActiveStroke::begin(
            &self.gpu, layer, brush, dynamics, mode, start,
        ));
        Ok(())
    }

    fn flush(&mut self) -> Result<(), EngineError> {
        self.gpu.check_device()?;
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        if let Some(stroke) = &mut self.stroke {
            let layer = self.document.layer(stroke.layer())?;
            let before = if stroke.is_mask() {
                let mask = layer.mask.as_ref().ok_or(DocumentError::NoMask(layer.id))?;
                (&mask.tiles, mask.reveal)
            } else {
                (
                    layer
                        .tiles()
                        .ok_or(DocumentError::NotARasterLayer(layer.id))?,
                    false,
                )
            };
            let touched = stroke.record(
                &self.gpu,
                &mut encoder,
                self.document.size(),
                before,
                &mut self.coverage_pool,
            );
            self.document.mark_dirty(touched);
        }
        let document = self
            .preview
            .as_mut()
            .map_or(&mut self.document, |preview| &mut preview.document);
        if document.size() != self.composite.size() {
            self.composite = Composite::new(&self.gpu, document.size(), &self.view_buffer);
            document.mark_all_dirty();
        }
        let dirty = document.take_dirty();
        if dirty.is_empty() {
            return Ok(());
        }
        self.composite.update(
            &self.gpu,
            &mut encoder,
            document,
            self.stroke.as_ref(),
            &dirty,
        );
        self.gpu.queue.submit([encoder.finish()]);
        Ok(())
    }

    fn import_tiles(
        &self,
        size: (u32, u32),
        pixels: &[u8],
        canvas: (u32, u32),
        position: (i32, i32),
        mask: bool,
    ) -> Tiles<TileRef> {
        let (x, y) = position;
        if i64::from(x) + i64::from(size.0) <= 0
            || i64::from(y) + i64::from(size.1) <= 0
            || i64::from(x) >= i64::from(canvas.0)
            || i64::from(y) >= i64::from(canvas.1)
        {
            return Tiles::new();
        }
        let image = self.gpu.device.create_texture_with_data(
            &self.gpu.queue,
            &texture_descriptor(
                "imported image",
                size,
                if mask {
                    wgpu::TextureFormat::R8Unorm
                } else {
                    IMAGE_FORMAT
                },
                wgpu::TextureUsages::TEXTURE_BINDING,
            ),
            wgpu::util::TextureDataOrder::LayerMajor,
            pixels,
        );
        let uniform = self
            .gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("import origin"),
                contents: bytemuck::bytes_of(&[x, y, 0, 0]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bindings = self.gpu.bind(
            &self.gpu.layouts.image,
            &[
                TextureView(&image.create_view(&Default::default())),
                uniform.as_entire_binding(),
            ],
        );
        let channels = if mask { 1 } else { RGBA_BYTES };
        let pipeline = if mask {
            &self.gpu.pipelines.import_mask
        } else {
            &self.gpu.pipelines.import
        };
        let tiles = self.gpu.submit(|encoder| {
            TileCoord::all(canvas)
                .filter(|coord| {
                    has_visible_pixels(*coord, canvas, size, position, pixels, channels)
                })
                .map(|coord| {
                    let tile = self.gpu.new_tile();
                    let packed_coord = coord.y << 16 | coord.x;
                    let mut pass = begin_pass(encoder, tile.view(), CLEAR);
                    let (width, height) = coord.visible_size(canvas);
                    pass.set_scissor_rect(0, 0, width, height);
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(0, &bindings, &[]);
                    pass.draw(0..3, packed_coord..packed_coord + 1);
                    drop(pass);
                    (coord, Rc::new(tile))
                })
                .collect()
        });
        image.destroy();
        tiles
    }

    fn publish_events(&mut self) {
        let events = self.document.take_events();
        if !events.is_empty() {
            self.revision = self.revision.wrapping_add(1);
        }
        for event in events {
            let _ = self.events.unbounded_send(event);
        }
    }
}

fn place(parent: Option<Option<LayerId>>, index: Option<usize>) -> Option<LayerPlace> {
    if parent.is_none() && index.is_none() {
        return None;
    }
    Some(LayerPlace {
        parent: parent.flatten(),
        index: index.unwrap_or(usize::MAX),
    })
}

fn checked_region(size: (u32, u32), region: Option<PixelRect>) -> Result<PixelRect, EngineError> {
    let rect = region.unwrap_or_else(|| PixelRect::full(size));
    if !rect.is_inside(size) {
        return Err(EngineError::InvalidRegion);
    }
    Ok(rect)
}

fn has_visible_pixels(
    coord: TileCoord,
    canvas: (u32, u32),
    size: (u32, u32),
    position: (i32, i32),
    pixels: &[u8],
    channels: usize,
) -> bool {
    let (left, top) = coord.origin();
    let (width, height) = coord.visible_size(canvas);
    let left = left as i32 - position.0;
    let top = top as i32 - position.1;
    let right = (left + width as i32).min(size.0 as i32);
    let bottom = (top + height as i32).min(size.1 as i32);
    let left = left.max(0);
    right > left
        && (top.max(0)..bottom).any(|y| {
            let start = (y as usize * size.0 as usize + left as usize) * channels;
            pixels[start..start + (right - left) as usize * channels]
                .chunks_exact(channels)
                .any(|pixel| pixel[channels - 1] > 0)
        })
}
