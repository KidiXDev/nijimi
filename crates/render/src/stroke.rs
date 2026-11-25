use std::{
    collections::{BTreeMap, BTreeSet},
    mem,
    rc::Rc,
};

use bytemuck::{Pod, Zeroable};
use nijimi_core::{Brush, BrushDynamics, Dab, LayerId, Stroke, StrokePoint, TileCoord, Tiles};
use wgpu::{BindingResource::TextureView, util::DeviceExt};

use crate::{
    gpu::{CLEAR, Gpu, begin_pass, draw_fullscreen},
    tile::{COVERAGE_FORMAT, Tile, TileRef},
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct DabInstance {
    center: [f32; 2],
    radius: f32,
    inner: f32,
    flow: f32,
}

impl DabInstance {
    pub const ATTRIBUTES: [wgpu::VertexAttribute; 4] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32, 2 => Float32, 3 => Float32];

    fn new(dab: &Dab, coord: TileCoord) -> Self {
        let (x, y) = coord.origin();
        let softest_edge = 1.0 - 1.0 / dab.radius.max(1.0);
        Self {
            center: [dab.center.x - x as f32, dab.center.y - y as f32],
            radius: dab.radius,
            inner: dab.hardness.min(softest_edge),
            flow: dab.flow,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PaintUniform {
    color: [f32; 3],
    opacity: f32,
}

struct StrokeTile {
    coverage: Tile,
    coverage_is_new: bool,
    result: TileRef,
    bindings: wgpu::BindGroup,
}

#[derive(Clone, Copy)]
pub(crate) enum StrokeMode {
    Paint,
    Erase,
    Mask(f32),
}

impl StrokeMode {
    pub fn is_mask(self) -> bool {
        matches!(self, Self::Mask(_))
    }

    pub fn painting(erase: bool) -> Self {
        if erase { Self::Erase } else { Self::Paint }
    }
}

pub(crate) struct ActiveStroke {
    layer: LayerId,
    stroke: Stroke,
    pending: Vec<Dab>,
    paint: wgpu::Buffer,
    mode: StrokeMode,
    tiles: BTreeMap<TileCoord, StrokeTile>,
}

impl ActiveStroke {
    pub fn begin(
        gpu: &Gpu,
        layer: LayerId,
        brush: Brush,
        dynamics: BrushDynamics,
        mode: StrokeMode,
        start: StrokePoint,
    ) -> Self {
        let (stroke, dab) = Stroke::with_dynamics(brush, dynamics, start);
        let paint = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("paint"),
                contents: bytemuck::bytes_of(&PaintUniform {
                    color: if let StrokeMode::Mask(value) = mode {
                        [value, 0.0, 0.0]
                    } else {
                        brush.color
                    },
                    opacity: brush.opacity,
                }),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        Self {
            layer,
            stroke,
            pending: vec![dab],
            paint,
            mode,
            tiles: BTreeMap::new(),
        }
    }

    pub fn extend(&mut self, point: StrokePoint) {
        self.stroke.extend(point, &mut self.pending);
    }

    pub fn complete(&mut self) {
        self.stroke.complete(&mut self.pending);
    }

    pub fn layer(&self) -> LayerId {
        self.layer
    }

    pub fn is_mask(&self) -> bool {
        self.mode.is_mask()
    }

    pub fn result(&self, layer: LayerId, coord: TileCoord, mask: bool) -> Option<&TileRef> {
        if layer != self.layer || self.is_mask() != mask {
            return None;
        }
        self.tiles.get(&coord).map(|tile| &tile.result)
    }

    pub fn touched(&self) -> impl Iterator<Item = TileCoord> {
        self.tiles.keys().copied()
    }

    pub fn finish(self, coverage_pool: &mut Vec<Tile>) -> Vec<(TileCoord, Option<TileRef>)> {
        self.tiles
            .into_iter()
            .map(|(coord, tile)| {
                coverage_pool.push(tile.coverage);
                (coord, Some(tile.result))
            })
            .collect()
    }

    pub fn record(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        document_size: (u32, u32),
        (before, reveal): (&Tiles<TileRef>, bool),
        coverage_pool: &mut Vec<Tile>,
    ) -> BTreeSet<TileCoord> {
        let mut dabs_by_tile: BTreeMap<TileCoord, Vec<DabInstance>> = BTreeMap::new();
        for dab in mem::take(&mut self.pending) {
            for coord in dab.tiles(document_size) {
                let default_only = match self.mode {
                    StrokeMode::Paint => false,
                    StrokeMode::Erase => true,
                    StrokeMode::Mask(value) => value == f32::from(u8::from(reveal)),
                };
                if default_only && !before.contains_key(&coord) {
                    continue;
                }
                let instance = DabInstance::new(&dab, coord);
                dabs_by_tile.entry(coord).or_default().push(instance);
            }
        }
        if dabs_by_tile.is_empty() {
            return BTreeSet::new();
        }

        let instances: Vec<_> = dabs_by_tile.values().flatten().copied().collect();
        let buffer = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("dabs"),
                contents: bytemuck::cast_slice(&instances),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let mut first = 0;
        for (coord, dabs) in &dabs_by_tile {
            let tile = self.tiles.entry(*coord).or_insert_with(|| {
                let source = before
                    .get(coord)
                    .unwrap_or_else(|| gpu.mask_default(reveal));
                StrokeTile::new(gpu, source, &self.paint, coverage_pool)
            });
            let (width, height) = coord.visible_size(document_size);
            let last = first + dabs.len() as u32;
            {
                let load = if mem::take(&mut tile.coverage_is_new) {
                    CLEAR
                } else {
                    wgpu::LoadOp::Load
                };
                let mut pass = begin_pass(encoder, tile.coverage.view(), load);
                pass.set_scissor_rect(0, 0, width, height);
                pass.set_pipeline(&gpu.pipelines.dab);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..4, first..last);
            }
            draw_fullscreen(
                encoder,
                tile.result.view(),
                match self.mode {
                    StrokeMode::Paint => &gpu.pipelines.stroke,
                    StrokeMode::Erase => &gpu.pipelines.erase,
                    StrokeMode::Mask(_) => &gpu.pipelines.mask_stroke,
                },
                &tile.bindings,
            );
            first = last;
        }
        dabs_by_tile.into_keys().collect()
    }
}

impl StrokeTile {
    fn new(
        gpu: &Gpu,
        before: &TileRef,
        paint: &wgpu::Buffer,
        coverage_pool: &mut Vec<Tile>,
    ) -> Self {
        let coverage = coverage_pool
            .pop()
            .unwrap_or_else(|| Tile::new(&gpu.device, COVERAGE_FORMAT));
        let bindings = gpu.bind(
            &gpu.layouts.stroke,
            &[
                TextureView(before.view()),
                TextureView(coverage.view()),
                paint.as_entire_binding(),
            ],
        );
        Self {
            coverage,
            coverage_is_new: true,
            result: Rc::new(gpu.new_tile()),
            bindings,
        }
    }
}
