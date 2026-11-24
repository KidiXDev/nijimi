use std::rc::Rc;

use bytemuck::{Pod, Zeroable};
use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use nijimi_core::{
    Brush, Command, Document, DocumentError, Event, Layer, LayerContent, LayerId, LayerPlace,
    StrokePoint, TileCoord, Tiles, Viewport,
};
use wgpu::{BindingResource::TextureView, util::DeviceExt};

use crate::{
    EngineError,
    composite::Composite,
    gpu::{CLEAR, Gpu, IMAGE_FORMAT, begin_pass, draw_fullscreen, texture_descriptor},
    project::{LoadedProject, ProjectLoader, ProjectSnapshot},
    readback::Readback,
    stroke::ActiveStroke,
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

pub struct Engine {
    gpu: Rc<Gpu>,
    document: Document<TileRef>,
    composite: Composite,
    view_buffer: wgpu::Buffer,
    stroke: Option<ActiveStroke>,
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
        let result = self.execute(command);
        self.publish_events();
        result
    }

    pub fn begin_stroke(&mut self, brush: Brush, start: StrokePoint) -> Result<(), EngineError> {
        self.end_stroke()?;
        let layer = self.document.active().ok_or(EngineError::NoActiveLayer)?;
        self.begin_stroke_on(layer, brush, start)
    }

    pub fn extend_stroke(&mut self, point: StrokePoint) {
        if let Some(stroke) = &mut self.stroke {
            stroke.extend(point);
        }
    }

    pub fn end_stroke(&mut self) -> Result<(), EngineError> {
        self.flush()?;
        if let Some(stroke) = self.stroke.take() {
            let layer = stroke.layer();
            let tiles = stroke.finish(&mut self.coverage_pool);
            self.document.replace_tiles(layer, tiles)?;
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
        self.gpu.check_device()?;
        let size = (width, height);
        self.gpu.check_size(size)?;
        let expected = u64::from(width) * u64::from(height) * RGBA_BYTES as u64;
        let actual = rgba.len() as u64;
        if actual != expected {
            return Err(EngineError::PixelCount { expected, actual });
        }

        let mut layer = Layer::raster(FIRST_LAYER, "Background".into());
        layer.content = LayerContent::Raster(self.import_tiles(size, rgba));
        let document = Document::with_layers(size, vec![layer], Some(FIRST_LAYER))?;
        let composite = Composite::new(&self.gpu, size, &self.view_buffer);
        self.open(LoadedProject {
            document,
            composite,
        });
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
        self.flush()?;
        let export = self.gpu.device.create_texture(&texture_descriptor(
            "export",
            self.document.size(),
            IMAGE_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        ));
        let readback = self.gpu.submit(|encoder| {
            draw_fullscreen(
                encoder,
                &export.create_view(&Default::default()),
                &self.gpu.pipelines.export,
                self.composite.export_bindings(),
            );
            Readback::record_image(&self.gpu.device, encoder, &export)
        });
        export.destroy();
        Ok(readback)
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
            } => self.paint_stroke(layer, brush, &points)?,
            Command::Undo => {
                document.undo();
            }
            Command::Redo => {
                document.redo();
            }
        }
        Ok(None)
    }

    fn paint_stroke(
        &mut self,
        layer: LayerId,
        brush: Brush,
        points: &[StrokePoint],
    ) -> Result<(), EngineError> {
        let [start, rest @ ..] = points else {
            return Err(EngineError::InvalidStroke);
        };
        if !points.iter().all(StrokePoint::is_valid) {
            return Err(EngineError::InvalidStroke);
        }
        self.begin_stroke_on(layer, brush, *start)?;
        for point in rest {
            self.extend_stroke(*point);
        }
        self.end_stroke()
    }

    fn begin_stroke_on(
        &mut self,
        layer: LayerId,
        brush: Brush,
        start: StrokePoint,
    ) -> Result<(), EngineError> {
        if !brush.is_valid() {
            return Err(EngineError::InvalidBrush);
        }
        if !start.is_valid() {
            return Err(EngineError::InvalidStroke);
        }
        if self.document.layer(layer)?.tiles().is_none() {
            return Err(DocumentError::NotARasterLayer(layer).into());
        }
        self.stroke = Some(ActiveStroke::begin(&self.gpu, layer, brush, start));
        Ok(())
    }

    fn flush(&mut self) -> Result<(), EngineError> {
        self.gpu.check_device()?;
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        if let Some(stroke) = &mut self.stroke {
            let layer = self.document.layer(stroke.layer())?;
            let before = layer
                .tiles()
                .ok_or(DocumentError::NotARasterLayer(layer.id))?;
            let touched = stroke.record(
                &self.gpu,
                &mut encoder,
                self.document.size(),
                before,
                &mut self.coverage_pool,
            );
            self.document.mark_dirty(touched);
        }
        let dirty = self.document.take_dirty();
        if dirty.is_empty() {
            return Ok(());
        }
        self.composite.update(
            &self.gpu,
            &mut encoder,
            &self.document,
            self.stroke.as_ref(),
            &dirty,
        );
        self.gpu.queue.submit([encoder.finish()]);
        Ok(())
    }

    fn import_tiles(&self, size: (u32, u32), rgba: &[u8]) -> Tiles<TileRef> {
        let image = self.gpu.device.create_texture_with_data(
            &self.gpu.queue,
            &texture_descriptor(
                "imported image",
                size,
                IMAGE_FORMAT,
                wgpu::TextureUsages::TEXTURE_BINDING,
            ),
            wgpu::util::TextureDataOrder::LayerMajor,
            rgba,
        );
        let bindings = self.gpu.bind(
            &self.gpu.layouts.image,
            &[TextureView(&image.create_view(&Default::default()))],
        );
        let tiles = self.gpu.submit(|encoder| {
            TileCoord::all(size)
                .filter(|coord| has_visible_pixels(*coord, size, rgba))
                .map(|coord| {
                    let tile = self.gpu.new_tile();
                    let packed_coord = coord.y << 16 | coord.x;
                    let mut pass = begin_pass(encoder, tile.view(), CLEAR);
                    pass.set_pipeline(&self.gpu.pipelines.import);
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
        for event in self.document.take_events() {
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

fn has_visible_pixels(coord: TileCoord, size: (u32, u32), rgba: &[u8]) -> bool {
    let (left, top) = coord.origin();
    let (width, height) = coord.visible_size(size);
    (top..top + height).any(|y| {
        let start = (y as usize * size.0 as usize + left as usize) * RGBA_BYTES;
        rgba[start..start + width as usize * RGBA_BYTES]
            .chunks_exact(RGBA_BYTES)
            .any(|pixel| pixel[RGBA_BYTES - 1] > 0)
    })
}
