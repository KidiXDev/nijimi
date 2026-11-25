use std::{
    collections::{BTreeMap, BTreeSet},
    mem,
    rc::Rc,
    sync::Arc,
};

use bytemuck::{Pod, Zeroable};
use nijimi_core::{
    Brush, BrushDynamics, Dab, LayerId, RetouchTool, Selection, Stroke, StrokePoint, TileCoord,
    Tiles,
};
use wgpu::{BindingResource::TextureView, util::DeviceExt};

use crate::{
    gpu::{CLEAR, Gpu, begin_pass, draw_tile},
    retouch::{Blur, Smudge, heal, shifted_tile},
    selection::SelectionView,
    tile::{COVERAGE_FORMAT, Tile, TileRef},
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct DabInstance {
    center: [f32; 2],
    radius: f32,
    inner: f32,
    flow: f32,
    origin: [i32; 2],
    delta: [i32; 2],
}

impl DabInstance {
    pub const ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
        0 => Float32x2, 1 => Float32, 2 => Float32, 3 => Float32, 4 => Sint32x2, 5 => Sint32x2
    ];

    pub fn new(dab: &Dab, coord: TileCoord, delta: (i32, i32)) -> Self {
        let (x, y) = coord.origin();
        let softest_edge = 1.0 - 1.0 / dab.radius.max(1.0);
        Self {
            center: [dab.center.x - x as f32, dab.center.y - y as f32],
            radius: dab.radius,
            inner: dab.hardness.min(softest_edge),
            flow: dab.flow,
            origin: [x as i32, y as i32],
            delta: delta.into(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PaintUniform {
    color: [f32; 3],
    opacity: f32,
    selection: [i32; 2],
    canvas: [i32; 2],
    unselected: f32,
    padding: [f32; 3],
}

pub(crate) struct PaintBindings {
    uniform: wgpu::Buffer,
    selection: Option<Arc<Selection>>,
    selection_view: wgpu::TextureView,
}

impl PaintBindings {
    pub fn new(
        gpu: &Gpu,
        color: [f32; 3],
        opacity: f32,
        size: (u32, u32),
        selection: &SelectionView,
    ) -> Self {
        let uniform = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("paint"),
                contents: bytemuck::bytes_of(&PaintUniform {
                    color,
                    opacity,
                    selection: selection.origin(),
                    canvas: [size.0 as i32, size.1 as i32],
                    unselected: f32::from(u8::from(selection.selection().is_none())),
                    padding: [0.0; 3],
                }),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        Self {
            uniform,
            selection: selection.selection().cloned(),
            selection_view: selection.view().clone(),
        }
    }

    pub fn touches(&self, coord: TileCoord) -> bool {
        self.selection
            .as_ref()
            .is_none_or(|selection| selection.touches(coord))
    }

    pub fn bind(
        &self,
        gpu: &Gpu,
        source: &Tile,
        coverage: &Tile,
        replacement: &Tile,
    ) -> wgpu::BindGroup {
        self.bind_coverage(gpu, source, coverage.view(), replacement)
    }

    pub fn bind_coverage(
        &self,
        gpu: &Gpu,
        source: &Tile,
        coverage: &wgpu::TextureView,
        replacement: &Tile,
    ) -> wgpu::BindGroup {
        gpu.bind(
            &gpu.layouts.stroke,
            &[
                TextureView(source.view()),
                TextureView(coverage),
                self.uniform.as_entire_binding(),
                TextureView(&self.selection_view),
                TextureView(replacement.view()),
            ],
        )
    }
}

pub(crate) struct Frame<'a> {
    pub gpu: &'a Gpu,
    pub encoder: &'a mut wgpu::CommandEncoder,
    pub size: (u32, u32),
    pub before: &'a Tiles<TileRef>,
    pub reveal: bool,
}

struct Coverage {
    tile: Tile,
    is_new: bool,
    bindings: wgpu::BindGroup,
}

pub(crate) struct StrokeTile {
    coverage: Option<Coverage>,
    replacement: TileRef,
    result: TileRef,
}

impl StrokeTile {
    fn covered(
        gpu: &Gpu,
        before: &TileRef,
        replacement: TileRef,
        paint: &PaintBindings,
        coverage_pool: &mut Vec<Tile>,
    ) -> Self {
        let tile = coverage_pool
            .pop()
            .unwrap_or_else(|| Tile::new(&gpu.device, COVERAGE_FORMAT));
        let bindings = paint.bind(gpu, before, &tile, &replacement);
        Self {
            coverage: Some(Coverage {
                tile,
                is_new: true,
                bindings,
            }),
            replacement,
            result: Rc::new(gpu.new_tile()),
        }
    }

    pub fn copied(frame: &mut Frame<'_>, coord: TileCoord) -> Self {
        let result = frame.gpu.new_tile();
        if let Some(before) = frame.before.get(&coord) {
            frame.encoder.copy_texture_to_texture(
                before.texture().as_image_copy(),
                result.texture().as_image_copy(),
                before.texture().size(),
            );
        }
        Self {
            coverage: None,
            replacement: frame.gpu.empty_tile.clone(),
            result: Rc::new(result),
        }
    }

    pub fn result(&self) -> &TileRef {
        &self.result
    }

    pub fn replacement(&self) -> &TileRef {
        &self.replacement
    }

    pub fn coverage<'a>(&'a self, gpu: &'a Gpu) -> &'a wgpu::TextureView {
        let tile = self.coverage.as_ref().map(|coverage| &coverage.tile);
        tile.unwrap_or(&gpu.empty_tile).view()
    }
}

#[derive(Clone, Copy)]
pub(crate) enum StrokeMode {
    Paint,
    Erase,
    Mask(f32),
    Retouch(RetouchTool),
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
    paint: PaintBindings,
    mode: StrokeMode,
    blur: Option<Blur>,
    smudge: Smudge,
    tiles: BTreeMap<TileCoord, StrokeTile>,
}

impl ActiveStroke {
    pub fn begin(
        gpu: &Gpu,
        (layer, mode): (LayerId, StrokeMode),
        brush: Brush,
        dynamics: BrushDynamics,
        start: StrokePoint,
        size: (u32, u32),
        selection: &SelectionView,
    ) -> Self {
        let (stroke, dab) = Stroke::with_dynamics(brush, dynamics, start);
        let color = match mode {
            StrokeMode::Mask(value) => [value, 0.0, 0.0],
            _ => brush.color,
        };
        let blur = match mode {
            StrokeMode::Retouch(RetouchTool::Blur { radius }) => Some(Blur::new(radius)),
            _ => None,
        };
        Self {
            layer,
            stroke,
            pending: vec![dab],
            paint: PaintBindings::new(gpu, color, brush.opacity, size, selection),
            mode,
            blur,
            smudge: Smudge::default(),
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

    pub fn heals(&self) -> bool {
        matches!(self.mode, StrokeMode::Retouch(RetouchTool::Heal { .. }))
    }

    pub fn heal(&self, gpu: &Gpu, before: &Tiles<TileRef>) {
        heal(gpu, before, &self.tiles, &self.paint);
    }

    pub fn finish(self, coverage_pool: &mut Vec<Tile>) -> Vec<(TileCoord, Option<TileRef>)> {
        self.tiles
            .into_iter()
            .map(|(coord, tile)| {
                coverage_pool.extend(tile.coverage.map(|coverage| coverage.tile));
                (coord, Some(tile.result))
            })
            .collect()
    }

    pub fn record(
        &mut self,
        frame: &mut Frame<'_>,
        coverage_pool: &mut Vec<Tile>,
    ) -> BTreeSet<TileCoord> {
        let dabs = mem::take(&mut self.pending);
        if let StrokeMode::Retouch(RetouchTool::Smudge) = self.mode {
            return dabs
                .iter()
                .flat_map(|dab| self.smudge.record(frame, dab, &mut self.tiles, &self.paint))
                .collect();
        }
        let mut dabs_by_tile: BTreeMap<TileCoord, Vec<DabInstance>> = BTreeMap::new();
        for dab in dabs {
            for coord in dab.tiles(frame.size) {
                let default_only = match self.mode {
                    StrokeMode::Paint | StrokeMode::Retouch(_) => false,
                    StrokeMode::Erase => true,
                    StrokeMode::Mask(value) => value == f32::from(u8::from(frame.reveal)),
                };
                if !self.paint.touches(coord) || default_only && !frame.before.contains_key(&coord)
                {
                    continue;
                }
                let instance = DabInstance::new(&dab, coord, (0, 0));
                dabs_by_tile.entry(coord).or_default().push(instance);
            }
        }
        dabs_by_tile.retain(|coord, _| {
            self.tiles.contains_key(coord) || self.add_tile(frame, *coord, coverage_pool)
        });
        if dabs_by_tile.is_empty() {
            return BTreeSet::new();
        }

        let instances: Vec<_> = dabs_by_tile.values().flatten().copied().collect();
        let buffer = frame
            .gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("dabs"),
                contents: bytemuck::cast_slice(&instances),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let Frame {
            gpu, encoder, size, ..
        } = frame;
        let mut first = 0;
        for (coord, dabs) in &dabs_by_tile {
            let tile = self.tiles.get_mut(coord).expect("tiles are added above");
            let coverage = tile.coverage.as_mut().expect("painted tiles have coverage");
            let (width, height) = coord.visible_size(*size);
            let last = first + dabs.len() as u32;
            {
                let load = if mem::take(&mut coverage.is_new) {
                    CLEAR
                } else {
                    wgpu::LoadOp::Load
                };
                let mut pass = begin_pass(encoder, coverage.tile.view(), load);
                pass.set_scissor_rect(0, 0, width, height);
                pass.set_pipeline(&gpu.pipelines.dab);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..4, first..last);
            }
            draw_tile(
                encoder,
                tile.result.view(),
                match self.mode {
                    StrokeMode::Paint => &gpu.pipelines.stroke,
                    StrokeMode::Erase => &gpu.pipelines.erase,
                    StrokeMode::Mask(_) => &gpu.pipelines.mask_stroke,
                    StrokeMode::Retouch(RetouchTool::Blur { .. }) => &gpu.pipelines.replace_stroke,
                    StrokeMode::Retouch(_) => &gpu.pipelines.clone_stroke,
                },
                &coverage.bindings,
                *coord,
            );
            first = last;
        }
        dabs_by_tile.into_keys().collect()
    }

    fn add_tile(
        &mut self,
        frame: &mut Frame<'_>,
        coord: TileCoord,
        coverage_pool: &mut Vec<Tile>,
    ) -> bool {
        let replacement = match self.mode {
            StrokeMode::Retouch(
                RetouchTool::Clone { offset_x, offset_y }
                | RetouchTool::Heal { offset_x, offset_y },
            ) => shifted_tile(frame, coord, (offset_x, offset_y)),
            _ => match &mut self.blur {
                Some(blur) => blur.tile(frame, coord),
                None => Some(frame.gpu.empty_tile.clone()),
            },
        };
        let Some(replacement) = replacement else {
            return false;
        };
        let Frame {
            gpu,
            before,
            reveal,
            ..
        } = frame;
        let source = before
            .get(&coord)
            .unwrap_or_else(|| gpu.mask_default(*reveal));
        let tile = StrokeTile::covered(gpu, source, replacement, &self.paint, coverage_pool);
        self.tiles.insert(coord, tile);
        true
    }
}
