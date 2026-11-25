use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use bytemuck::{Pod, Zeroable};
use nijimi_core::{
    Adjustment, BlendMode, Document, Layer, LayerContent, LayerId, TILE_SIZE, TileCoord, Tiles,
};
use wgpu::{
    BindingResource::{Buffer, Sampler, TextureView},
    util::DeviceExt,
};

use crate::{
    gpu::{CLEAR, Gpu, begin_pass, draw_fullscreen},
    grade::{Grade, grown, neighbors},
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
    was_graded: bool,
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
            was_graded: false,
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
        gpu.luts.prune();
        let layers = document.layers();
        let graded = has_visible_grade(layers);
        let dirty = &(0..spatial_steps(layers)).fold(dirty.clone(), |dirty, _| {
            grown(&dirty, document.size(), (1, 1))
        });
        if self.was_graded && !graded {
            self.scratch.truncate(32);
        }
        self.was_graded = graded;
        let mut planner = Planner {
            gpu,
            stroke,
            coord: TileCoord::new(0, 0),
            size: document.size(),
            scratch: &mut self.scratch,
            operations: Vec::new(),
        };
        let result = if graded {
            Some(planner.graded_stack(layers, dirty))
        } else {
            for coord in dirty {
                planner.coord = *coord;
                let sources = planner.sources(layers, &mut BTreeMap::new());
                planner.chain(sources, Target::Canvas(*coord), gpu.empty_tile.clone());
            }
            None
        };
        let operations = planner.operations;
        for run in
            operations.chunk_by(|a, b| matches!((a, b), (Operation::Blend(_), Operation::Blend(_))))
        {
            match &run[0] {
                Operation::Blend(_) => {
                    let passes: Vec<_> = run
                        .iter()
                        .map(|op| match op {
                            Operation::Blend(pass) => pass,
                            _ => unreachable!("blend passes are grouped together"),
                        })
                        .collect();
                    self.draw_layers(gpu, encoder, &passes);
                }
                Operation::Kernel {
                    target,
                    bindings,
                    mask,
                } => {
                    let pipeline = if *mask {
                        &gpu.pipelines.apply_mask
                    } else {
                        &gpu.pipelines.grade
                    };
                    draw_fullscreen(encoder, target.view(), pipeline, bindings);
                }
            }
        }
        if let Some(result) = result {
            self.draw_graded(gpu, encoder, dirty, &result);
            self.scratch.extend(result.into_values());
        }
        self.draw_mipmaps(gpu, encoder, dirty);
    }

    fn draw_graded(
        &self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        dirty: &BTreeSet<TileCoord>,
        tiles: &Tiles<TileRef>,
    ) {
        let mut pass = begin_pass(encoder, self.view(), wgpu::LoadOp::Load);
        pass.set_pipeline(&gpu.pipelines.copy_tile);
        for &coord in dirty {
            let (x, y) = coord.origin();
            let offset = gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("tile origin"),
                    contents: bytemuck::bytes_of(&[x as i32, y as i32, 0, 0]),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
            let tile = tiles.get(&coord).unwrap_or(&gpu.empty_tile);
            let bindings = gpu.bind(
                &gpu.layouts.copy_tile,
                &[TextureView(tile.view()), offset.as_entire_binding()],
            );
            self.canvas_rect(coord).scissor(&mut pass);
            pass.set_bind_group(0, &bindings, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    fn draw_layers(&self, gpu: &Gpu, encoder: &mut wgpu::CommandEncoder, passes: &[&Pass]) {
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

enum Operation {
    Blend(Pass),
    Kernel {
        target: TileRef,
        bindings: wgpu::BindGroup,
        mask: bool,
    },
}

fn shown<T>(layer: &Layer<T>) -> bool {
    layer.properties.visible && layer.properties.opacity > 0.0
}

fn has_visible_grade<T>(layers: &[Layer<T>]) -> bool {
    layers
        .iter()
        .filter(|layer| shown(layer))
        .any(|layer| layer.grade().is_some() || has_visible_grade(layer.children()))
}

fn spatial_steps<T>(layers: &[Layer<T>]) -> usize {
    let visible = || layers.iter().filter(|layer| shown(layer));
    let here = visible()
        .filter(|layer| layer.grade().is_some_and(Adjustment::is_spatial))
        .count();
    let nested = visible().map(|layer| spatial_steps(layer.children())).max();
    here + nested.unwrap_or(0)
}

struct Planner<'a> {
    gpu: &'a Gpu,
    stroke: Option<&'a ActiveStroke>,
    coord: TileCoord,
    scratch: &'a mut Vec<TileRef>,
    size: (u32, u32),
    operations: Vec<Operation>,
}

impl Planner<'_> {
    fn sources(
        &mut self,
        layers: &[Layer<TileRef>],
        groups: &mut BTreeMap<LayerId, Tiles<TileRef>>,
    ) -> Vec<Source> {
        layers
            .iter()
            .filter(|layer| shown(layer))
            .filter_map(|layer| {
                let (tile, is_scratch) = match &layer.content {
                    LayerContent::Raster(tiles) => {
                        let painted = self
                            .stroke
                            .and_then(|stroke| stroke.result(layer.id, self.coord, false));
                        (painted.or(tiles.get(&self.coord))?.clone(), false)
                    }
                    LayerContent::Group(children) => {
                        let tile = match groups.get_mut(&layer.id) {
                            Some(tiles) => tiles.remove(&self.coord),
                            None => self.flatten_group(children),
                        };
                        (tile?, true)
                    }
                    LayerContent::Adjustment(_) => return None,
                };
                let (tile, is_scratch) = self.apply_mask(layer, tile, is_scratch)?;
                Some(Source {
                    tile,
                    opacity: layer.properties.opacity,
                    blend_mode: layer.properties.blend_mode,
                    is_scratch,
                })
            })
            .collect()
    }

    fn mask_tile(&self, layer: &Layer<TileRef>, coord: TileCoord) -> TileRef {
        if let Some(mask) = &layer.mask {
            let painted = self
                .stroke
                .and_then(|stroke| stroke.result(layer.id, coord, true));
            painted
                .or(mask.tiles.get(&coord))
                .unwrap_or_else(|| self.gpu.mask_default(mask.reveal))
                .clone()
        } else {
            self.gpu.mask_default(true).clone()
        }
    }

    fn apply_mask(
        &mut self,
        layer: &Layer<TileRef>,
        tile: TileRef,
        is_scratch: bool,
    ) -> Option<(TileRef, bool)> {
        if layer.mask.is_none() {
            return Some((tile, is_scratch));
        }
        let mask = self.mask_tile(layer, self.coord);
        if Rc::ptr_eq(&mask, self.gpu.mask_default(true)) {
            return Some((tile, is_scratch));
        }
        if Rc::ptr_eq(&mask, &self.gpu.empty_tile) {
            if is_scratch {
                self.scratch.push(tile);
            }
            return None;
        }
        let target = self.take_scratch();
        let bindings = self.gpu.bind(
            &self.gpu.layouts.apply_mask,
            &[TextureView(tile.view()), TextureView(mask.view())],
        );
        self.operations.push(Operation::Kernel {
            target: target.clone(),
            bindings,
            mask: true,
        });
        if is_scratch {
            self.scratch.push(tile);
        }
        Some((target, true))
    }

    fn flatten_group(&mut self, children: &[Layer<TileRef>]) -> Option<TileRef> {
        let sources = self.sources(children, &mut BTreeMap::new());
        if sources.is_empty() {
            return None;
        }
        let tile = self.take_scratch();
        self.chain(
            sources,
            Target::Scratch(tile.clone()),
            self.gpu.empty_tile.clone(),
        );
        Some(tile)
    }

    fn graded_stack(
        &mut self,
        layers: &[Layer<TileRef>],
        coords: &BTreeSet<TileCoord>,
    ) -> Tiles<TileRef> {
        let mut output = coords.clone();
        let mut stages = Vec::new();
        for (index, layer) in layers.iter().enumerate().rev() {
            if let Some(adjustment) = layer.grade().filter(|_| shown(layer)) {
                let input = if adjustment.is_spatial() {
                    grown(&output, self.size, (1, 1))
                } else {
                    output.clone()
                };
                stages.push((index, adjustment, std::mem::replace(&mut output, input)));
            }
        }
        let mut input = output;
        let mut tiles = Tiles::new();
        let mut start = 0;
        for (index, adjustment, output) in stages.into_iter().rev() {
            self.blend_run(&layers[start..index], &input, &mut tiles);
            tiles = self.apply_grade(&layers[index], adjustment, &output, tiles);
            input = output;
            start = index + 1;
        }
        self.blend_run(&layers[start..], &input, &mut tiles);
        tiles
    }

    fn blend_run(
        &mut self,
        layers: &[Layer<TileRef>],
        coords: &BTreeSet<TileCoord>,
        tiles: &mut Tiles<TileRef>,
    ) {
        if layers.is_empty() {
            return;
        }
        let mut groups = BTreeMap::new();
        for layer in layers.iter().filter(|layer| shown(layer)) {
            if has_visible_grade(layer.children()) {
                groups.insert(layer.id, self.graded_stack(layer.children(), coords));
            }
        }
        for &coord in coords {
            self.coord = coord;
            let sources = self.sources(layers, &mut groups);
            if !sources.is_empty() {
                let backdrop = tiles
                    .remove(&coord)
                    .unwrap_or_else(|| self.gpu.empty_tile.clone());
                let target = self.take_scratch();
                self.chain(sources, Target::Scratch(target.clone()), backdrop);
                tiles.insert(coord, target);
            }
        }
    }

    fn apply_grade(
        &mut self,
        layer: &Layer<TileRef>,
        adjustment: &Adjustment,
        coords: &BTreeSet<TileCoord>,
        before: Tiles<TileRef>,
    ) -> Tiles<TileRef> {
        if before.is_empty() || matches!(adjustment, Adjustment::Blur { radius: 0 }) {
            return before;
        }
        let spatial = adjustment.is_spatial();
        let grade = |horizontal| {
            Grade::new(
                self.gpu,
                adjustment,
                horizontal,
                layer.properties.opacity,
                shader_mode(layer.properties.blend_mode),
            )
        };
        let result = if matches!(adjustment, Adjustment::Blur { .. }) {
            let rows = grown(coords, self.size, (0, 1));
            let horizontal = self.grade_pass(layer, &rows, &grade(true), &before, &before, true);
            let result = self.grade_pass(layer, coords, &grade(false), &horizontal, &before, true);
            self.scratch.extend(horizontal.into_values());
            result
        } else {
            self.grade_pass(layer, coords, &grade(false), &before, &before, spatial)
        };
        self.scratch.extend(before.into_values());
        result
    }

    fn grade_pass(
        &mut self,
        layer: &Layer<TileRef>,
        coords: &BTreeSet<TileCoord>,
        grade: &Grade,
        input: &Tiles<TileRef>,
        original: &Tiles<TileRef>,
        spatial: bool,
    ) -> Tiles<TileRef> {
        let mut result = Tiles::new();
        for &coord in coords {
            let nearby = neighbors(self.gpu, coord, input, spatial);
            let base = original.get(&coord).unwrap_or(&self.gpu.empty_tile);
            if Rc::ptr_eq(base, &self.gpu.empty_tile)
                && nearby
                    .iter()
                    .all(|tile| Rc::ptr_eq(tile, &self.gpu.empty_tile))
            {
                continue;
            }
            let mask = self.mask_tile(layer, coord);
            let target = self.take_scratch();
            let bindings = grade.bindings(self.gpu, self.size, coord, &nearby, base, &mask);
            self.operations.push(Operation::Kernel {
                target: target.clone(),
                bindings,
                mask: false,
            });
            result.insert(coord, target);
        }
        result
    }

    fn chain(&mut self, sources: Vec<Source>, target: Target, mut backdrop: TileRef) {
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
        self.operations.push(Operation::Blend(Pass {
            target,
            backdrop,
            sources,
        }));
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
