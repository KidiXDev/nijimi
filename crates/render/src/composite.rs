use std::{collections::BTreeSet, rc::Rc};

use bytemuck::{Pod, Zeroable};
use nijimi_core::{BlendMode, Document, Layer, LayerContent, TILE_SIZE, TileCoord};
use wgpu::{
    BindingResource::{Buffer, Sampler, TextureView},
    util::DeviceExt,
};

use crate::{
    gpu::{CLEAR, Gpu, begin_pass},
    stroke::ActiveStroke,
    tile::{TILE_FORMAT, TileRef},
};

pub(crate) const LAYERS_PER_PASS: usize = 8;

#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
struct LayersUniform {
    origin: [i32; 2],
    padding: [i32; 2],
    appearance: [[f32; 4]; LAYERS_PER_PASS],
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Rect {
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}

impl Rect {
    fn halved(self, from: (u32, u32), to: (u32, u32)) -> Self {
        let start = |value: u32, from: u32, to: u32| (value * to / from).saturating_sub(1);
        let end = |value: u32, from: u32, to: u32| ((value * to).div_ceil(from) + 1).min(to);
        Self {
            left: start(self.left, from.0, to.0),
            top: start(self.top, from.1, to.1),
            right: end(self.right, from.0, to.0),
            bottom: end(self.bottom, from.1, to.1),
        }
    }

    fn scissor(self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_scissor_rect(
            self.left,
            self.top,
            self.right - self.left,
            self.bottom - self.top,
        );
    }
}

struct Mipmap {
    view: wgpu::TextureView,
    size: (u32, u32),
    source_bindings: wgpu::BindGroup,
}

pub(crate) struct Composite {
    texture: wgpu::Texture,
    mipmaps: Vec<Mipmap>,
    present_bindings: wgpu::BindGroup,
    scratch: Vec<TileRef>,
}

impl Composite {
    pub fn new(gpu: &Gpu, size: (u32, u32), view_buffer: &wgpu::Buffer) -> Self {
        let level_count = size.0.max(size.1).ilog2() + 1;
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            mip_level_count: level_count,
            ..crate::gpu::texture_descriptor(
                "composite",
                size,
                TILE_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            )
        });
        let full_view = texture.create_view(&Default::default());
        let mipmaps = (0..level_count)
            .map(|level| {
                let view = texture.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    ..Default::default()
                });
                let source_bindings = gpu.bind(
                    &gpu.layouts.mipmap,
                    &[TextureView(&view), Sampler(&gpu.sampler)],
                );
                Mipmap {
                    view,
                    size: ((size.0 >> level).max(1), (size.1 >> level).max(1)),
                    source_bindings,
                }
            })
            .collect();
        let present_bindings = gpu.bind(
            &gpu.layouts.present,
            &[
                TextureView(&full_view),
                Sampler(&gpu.sampler),
                view_buffer.as_entire_binding(),
            ],
        );
        Self {
            texture,
            mipmaps,
            present_bindings,
            scratch: Vec::new(),
        }
    }

    pub fn present_bindings(&self) -> &wgpu::BindGroup {
        &self.present_bindings
    }

    pub fn size(&self) -> (u32, u32) {
        self.mipmaps[0].size
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.mipmaps[0].view
    }

    pub fn update(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        document: &Document<TileRef>,
        stroke: Option<&ActiveStroke>,
        dirty: &BTreeSet<TileCoord>,
    ) {
        let mut planner = Planner {
            gpu,
            stroke,
            coord: TileCoord::new(0, 0),
            scratch: &mut self.scratch,
            passes: Vec::new(),
        };
        for coord in dirty {
            planner.coord = *coord;
            let sources = planner.sources(document.layers());
            planner.chain(sources, Target::Canvas(*coord));
        }
        let passes = planner.passes;
        self.draw_layers(gpu, encoder, &passes);
        self.draw_mipmaps(gpu, encoder, dirty);
    }

    fn draw_layers(&self, gpu: &Gpu, encoder: &mut wgpu::CommandEncoder, passes: &[Pass]) {
        let stride = size_of::<LayersUniform>()
            .next_multiple_of(gpu.device.limits().min_uniform_buffer_offset_alignment as usize);
        let mut uniforms = vec![0; stride * passes.len()];
        for (pass, slot) in passes.iter().zip(uniforms.chunks_exact_mut(stride)) {
            slot[..size_of::<LayersUniform>()].copy_from_slice(bytemuck::bytes_of(&pass.uniform()));
        }
        let buffer = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("layers"),
                contents: &uniforms,
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bindings: Vec<_> = passes
            .iter()
            .map(|pass| pass.bindings(gpu, &buffer))
            .collect();

        let mut index = 0;
        for run in passes.chunk_by(|a, b| a.target.is_canvas() && b.target.is_canvas()) {
            let (view, load) = match &run[0].target {
                Target::Canvas(_) => (&self.mipmaps[0].view, wgpu::LoadOp::Load),
                Target::Scratch(tile) => (tile.view(), CLEAR),
            };
            let mut render_pass = begin_pass(encoder, view, load);
            render_pass.set_pipeline(&gpu.pipelines.composite);
            for pass in run {
                if let Target::Canvas(coord) = pass.target {
                    self.canvas_rect(coord).scissor(&mut render_pass);
                }
                render_pass.set_bind_group(0, &bindings[index], &[(index * stride) as u32]);
                render_pass.draw(0..3, 0..1);
                index += 1;
            }
        }
    }

    fn draw_mipmaps(
        &self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        dirty: &BTreeSet<TileCoord>,
    ) {
        let mut rects: BTreeSet<_> = dirty.iter().map(|coord| self.canvas_rect(*coord)).collect();
        for levels in self.mipmaps.windows(2) {
            let [source, target] = levels else {
                unreachable!("windows of two levels");
            };
            rects = rects
                .into_iter()
                .map(|rect| rect.halved(source.size, target.size))
                .collect();
            let mut pass = begin_pass(encoder, &target.view, wgpu::LoadOp::Load);
            pass.set_pipeline(&gpu.pipelines.mipmap);
            pass.set_bind_group(0, &source.source_bindings, &[]);
            for rect in &rects {
                rect.scissor(&mut pass);
                pass.draw(0..3, 0..1);
            }
        }
    }

    fn canvas_rect(&self, coord: TileCoord) -> Rect {
        let (left, top) = coord.origin();
        let (width, height) = coord.visible_size(self.mipmaps[0].size);
        Rect {
            left,
            top,
            right: left + width,
            bottom: top + height,
        }
    }
}

impl Drop for Composite {
    fn drop(&mut self) {
        self.texture.destroy();
    }
}

enum Target {
    Canvas(TileCoord),
    Scratch(TileRef),
}

impl Target {
    fn is_canvas(&self) -> bool {
        matches!(self, Self::Canvas(_))
    }
}

struct Source {
    tile: TileRef,
    opacity: f32,
    blend_mode: BlendMode,
    is_scratch: bool,
}

struct Pass {
    target: Target,
    backdrop: TileRef,
    sources: Vec<Source>,
}

impl Pass {
    fn uniform(&self) -> LayersUniform {
        let mut uniform = LayersUniform::default();
        if let Target::Canvas(coord) = self.target {
            let (x, y) = coord.origin();
            uniform.origin = [x as i32, y as i32];
        }
        for (slot, source) in uniform.appearance.iter_mut().zip(&self.sources) {
            *slot = [source.opacity, shader_mode(source.blend_mode), 0.0, 0.0];
        }
        uniform
    }

    fn bindings(&self, gpu: &Gpu, uniforms: &wgpu::Buffer) -> wgpu::BindGroup {
        let mut resources = vec![TextureView(self.backdrop.view())];
        resources.extend((0..LAYERS_PER_PASS).map(|index| {
            let tile = self.sources.get(index).map(|source| &source.tile);
            TextureView(tile.unwrap_or(&gpu.empty_tile).view())
        }));
        resources.push(Buffer(wgpu::BufferBinding {
            buffer: uniforms,
            offset: 0,
            size: wgpu::BufferSize::new(size_of::<LayersUniform>() as u64),
        }));
        gpu.bind(&gpu.layouts.composite, &resources)
    }
}

struct Planner<'a> {
    gpu: &'a Gpu,
    stroke: Option<&'a ActiveStroke>,
    coord: TileCoord,
    scratch: &'a mut Vec<TileRef>,
    passes: Vec<Pass>,
}

impl Planner<'_> {
    fn sources(&mut self, layers: &[Layer<TileRef>]) -> Vec<Source> {
        layers
            .iter()
            .filter(|layer| layer.properties.visible && layer.properties.opacity > 0.0)
            .filter_map(|layer| {
                let (tile, is_scratch) = match &layer.content {
                    LayerContent::Raster(tiles) => {
                        let painted = self
                            .stroke
                            .and_then(|stroke| stroke.result(layer.id, self.coord));
                        (painted.or(tiles.get(&self.coord))?.clone(), false)
                    }
                    LayerContent::Group(children) => (self.flatten_group(children)?, true),
                };
                Some(Source {
                    tile,
                    opacity: layer.properties.opacity,
                    blend_mode: layer.properties.blend_mode,
                    is_scratch,
                })
            })
            .collect()
    }

    fn flatten_group(&mut self, children: &[Layer<TileRef>]) -> Option<TileRef> {
        let sources = self.sources(children);
        if sources.is_empty() {
            return None;
        }
        let tile = self.take_scratch();
        self.chain(sources, Target::Scratch(tile.clone()));
        Some(tile)
    }

    fn chain(&mut self, sources: Vec<Source>, target: Target) {
        let mut backdrop = self.gpu.empty_tile.clone();
        let mut remaining = sources;
        while remaining.len() > LAYERS_PER_PASS {
            let rest = remaining.split_off(LAYERS_PER_PASS);
            let accumulated = self.take_scratch();
            self.push_pass(Target::Scratch(accumulated.clone()), backdrop, remaining);
            backdrop = accumulated;
            remaining = rest;
        }
        self.push_pass(target, backdrop, remaining);
    }

    fn push_pass(&mut self, target: Target, backdrop: TileRef, sources: Vec<Source>) {
        let finished = sources.iter().filter(|source| source.is_scratch);
        self.scratch
            .extend(finished.map(|source| source.tile.clone()));
        if !Rc::ptr_eq(&backdrop, &self.gpu.empty_tile) {
            self.scratch.push(backdrop.clone());
        }
        self.passes.push(Pass {
            target,
            backdrop,
            sources,
        });
    }

    fn take_scratch(&mut self) -> TileRef {
        self.scratch
            .pop()
            .unwrap_or_else(|| Rc::new(self.gpu.new_tile()))
    }
}

fn shader_mode(blend_mode: BlendMode) -> f32 {
    match blend_mode {
        BlendMode::Normal => 0.0,
        BlendMode::Multiply => 1.0,
        BlendMode::Screen => 2.0,
        BlendMode::Overlay => 3.0,
        BlendMode::SoftLight => 4.0,
        BlendMode::Add => 5.0,
        BlendMode::Darken => 6.0,
        BlendMode::Lighten => 7.0,
    }
}

const _: () = assert!(TILE_SIZE == 256, "shaders.wgsl hard-codes the tile size");
