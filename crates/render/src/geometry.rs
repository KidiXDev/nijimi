use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    rc::Rc,
};

use bytemuck::{Pod, Zeroable};
use nijimi_core::{
    MAX_COORDINATE, PixelRect, Point, Selection, TILE_SIZE, TextSettings, TileCoord, Tiles,
    Transform,
};
use wgpu::{BindingResource::TextureView, util::DeviceExt};

use crate::{
    EngineError,
    gpu::{CLEAR, Gpu, REVEALED, begin_pass, draw_tile},
    selection::SelectionView,
    stroke::PaintBindings,
    tile::TileRef,
};

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Affine {
    x: [f32; 2],
    y: [f32; 2],
    offset: [f32; 2],
}

impl Affine {
    pub fn translation(x: i32, y: i32) -> Self {
        Self {
            x: [1.0, 0.0],
            y: [0.0, 1.0],
            offset: [x as f32, y as f32],
        }
    }

    pub fn from_transform(transform: Transform, size: (u32, u32)) -> Result<Self, EngineError> {
        let Transform {
            x,
            y,
            scale_x,
            scale_y,
            rotation,
            pivot,
        } = transform;
        let pivot = pivot.unwrap_or(Point::new(size.0 as f32 / 2.0, size.1 as f32 / 2.0));
        if ![x, y, rotation, pivot.x, pivot.y]
            .into_iter()
            .all(|v| v.abs() <= MAX_COORDINATE)
            || ![scale_x, scale_y]
                .into_iter()
                .all(|s| (0.0001..=1024.0).contains(&s.abs()))
        {
            return Err(EngineError::InvalidTransform);
        }
        let rotation = rotation.rem_euclid(360.0);
        let (sin, cos) = match rotation {
            0.0 => (0.0, 1.0),
            90.0 => (1.0, 0.0),
            180.0 => (0.0, -1.0),
            270.0 => (-1.0, 0.0),
            angle => angle.to_radians().sin_cos(),
        };
        let mut affine = Self {
            x: [cos * scale_x, -sin * scale_y],
            y: [sin * scale_x, cos * scale_y],
            offset: [0.0; 2],
        };
        let [px, py] = affine.map([pivot.x, pivot.y]);
        affine.offset = [pivot.x - px + x, pivot.y - py + y];
        Ok(affine)
    }

    fn map(self, [x, y]: [f32; 2]) -> [f32; 2] {
        [
            self.x[0] * x + self.x[1] * y + self.offset[0],
            self.y[0] * x + self.y[1] * y + self.offset[1],
        ]
    }

    pub fn text(self, text: &TextSettings) -> TextSettings {
        let Point { x, y } = text.position;
        let [x, y] = self.map([x, y]);
        let [a, b, c, d] = text.transform;
        let linear =
            |x: f32, y: f32| [self.x[0] * x + self.x[1] * y, self.y[0] * x + self.y[1] * y];
        let ([a, b], [c, d]) = (linear(a, b), linear(c, d));
        TextSettings {
            position: Point::new(x, y),
            transform: [a, b, c, d],
            ..text.clone()
        }
    }

    pub fn selection(self, selection: &Selection, size: (u32, u32)) -> Option<Selection> {
        let PixelRect {
            x,
            y,
            width,
            height,
        } = selection.bounds();
        let (columns, rows) = self.extent((x, y), (width, height), size);
        let bounds = PixelRect {
            x: columns.start,
            y: rows.start,
            width: columns.len() as u32,
            height: rows.len() as u32,
        };
        let inverse = self.inverse();
        Selection::from_fn(bounds, |x, y| {
            let [x, y] = inverse.map([x as f32 + 0.5, y as f32 + 0.5]);
            if x < 0.0 || y < 0.0 {
                return 0;
            }
            selection.value(x as u32, y as u32)
        })
    }

    fn corners(self, (left, top): (u32, u32), (width, height): (u32, u32)) -> [[f32; 2]; 4] {
        [
            self.map([left as f32, top as f32]),
            self.map([(left + width) as f32, top as f32]),
            self.map([left as f32, (top + height) as f32]),
            self.map([(left + width) as f32, (top + height) as f32]),
        ]
    }

    fn extent(
        self,
        corner: (u32, u32),
        area: (u32, u32),
        limit: (u32, u32),
    ) -> (Range<u32>, Range<u32>) {
        let corners = self.corners(corner, area);
        let range = |axis, limit| {
            let values = corners.map(|point| point[axis]);
            let start =
                (values.into_iter().fold(f32::INFINITY, f32::min).floor() as u32).min(limit);
            let end =
                (values.into_iter().fold(f32::NEG_INFINITY, f32::max).ceil() as u32).min(limit);
            start..end
        };
        (range(0, limit.0), range(1, limit.1))
    }

    fn inverse(self) -> Self {
        let determinant = self.x[0] * self.y[1] - self.x[1] * self.y[0];
        let mut inverse = Self {
            x: [self.y[1] / determinant, -self.x[1] / determinant],
            y: [-self.y[0] / determinant, self.x[0] / determinant],
            offset: [0.0; 2],
        };
        inverse.offset = inverse.map(self.offset.map(|v| -v));
        inverse
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Resample {
    x: [f32; 2],
    y: [f32; 2],
    offset: [f32; 2],
    origin: [i32; 2],
    size: [i32; 2],
}

struct Source<'a> {
    coord: TileCoord,
    tile: &'a TileRef,
    size: (u32, u32),
    clip: (u32, u32, u32, u32),
}

pub(crate) fn move_selected(
    gpu: &Gpu,
    before: &Tiles<TileRef>,
    size: (u32, u32),
    affine: Affine,
    selection: &SelectionView,
) -> Tiles<TileRef> {
    if affine == Affine::translation(0, 0) {
        return before.clone();
    }
    let paint = PaintBindings::new(gpu, [0.0; 3], 1.0, size, selection);
    let lifted: Tiles<TileRef> = gpu.submit(|encoder| {
        before
            .iter()
            .filter(|(coord, _)| paint.touches(**coord))
            .map(|(coord, tile)| {
                let target = Rc::new(gpu.new_tile());
                let bindings = paint.bind(gpu, tile, &gpu.empty_tile, &gpu.empty_tile);
                draw_tile(
                    encoder,
                    target.view(),
                    &gpu.pipelines.lift,
                    &bindings,
                    *coord,
                );
                (*coord, target)
            })
            .collect()
    });
    let moved = remap(gpu, &lifted, size, size, affine, false);
    let mut result = before.clone();
    gpu.submit(|encoder| {
        let coords: BTreeSet<_> = lifted.keys().chain(moved.keys()).collect();
        for coord in coords {
            let source = before.get(coord).unwrap_or(&gpu.empty_tile);
            let arriving = moved.get(coord).unwrap_or(&gpu.empty_tile);
            let target = Rc::new(gpu.new_tile());
            let bindings = paint.bind(gpu, source, &gpu.empty_tile, arriving);
            draw_tile(
                encoder,
                target.view(),
                &gpu.pipelines.drop,
                &bindings,
                *coord,
            );
            result.insert(*coord, target);
        }
    });
    result
}

pub(crate) fn remap(
    gpu: &Gpu,
    before: &Tiles<TileRef>,
    old_size: (u32, u32),
    new_size: (u32, u32),
    affine: Affine,
    reveal: bool,
) -> Tiles<TileRef> {
    if before.is_empty() || (affine == Affine::translation(0, 0) && old_size == new_size) {
        return before.clone();
    }
    let mut reused = Tiles::new();
    let mut draws: BTreeMap<TileCoord, Vec<Source<'_>>> = BTreeMap::new();
    for (&coord, tile) in before {
        let (left, top) = coord.origin();
        let (width, height) = coord.visible_size(old_size);
        let [corner, ..] = affine.corners((left, top), (width, height));
        let (xs, ys) = affine.extent((left, top), (width, height), new_size);
        if xs.is_empty() || ys.is_empty() {
            continue;
        }

        let first = TileCoord::new(xs.start / TILE_SIZE, ys.start / TILE_SIZE);
        let rotated = affine.x != [1.0, 0.0] || affine.y != [0.0, 1.0];
        if !rotated
            && corner == [xs.start as f32, ys.start as f32]
            && first.origin() == (xs.start, ys.start)
            && first.visible_size(new_size) == (width, height)
        {
            reused.insert(first, tile.clone());
            continue;
        }
        for y in first.y..ys.end.div_ceil(TILE_SIZE) {
            for x in first.x..xs.end.div_ceil(TILE_SIZE) {
                let target = TileCoord::new(x, y);
                let (tx, ty) = target.origin();
                let left = xs.start.max(tx);
                let top = ys.start.max(ty);
                let right = xs.end.min(tx + TILE_SIZE);
                let bottom = ys.end.min(ty + TILE_SIZE);
                draws.entry(target).or_default().push(Source {
                    coord,
                    tile,
                    size: (width, height),
                    clip: (left - tx, top - ty, right - left, bottom - top),
                });
            }
        }
    }
    if draws.is_empty() {
        return reused;
    }
    let inverse = affine.inverse();
    gpu.submit(|encoder| {
        for (coord, sources) in draws {
            let target = Rc::new(gpu.new_tile());
            let (left, top) = coord.origin();
            let [x, y] = inverse.map([left as f32, top as f32]);
            let mut pass = begin_pass(
                encoder,
                target.view(),
                if reveal { REVEALED } else { CLEAR },
            );
            pass.set_pipeline(&gpu.pipelines.resample);
            for source in sources {
                let (left, top, width, height) = source.clip;
                pass.set_scissor_rect(left, top, width, height);
                let (left, top) = source.coord.origin();
                let uniform = Resample {
                    x: inverse.x,
                    y: inverse.y,
                    offset: [x, y],
                    origin: [left as i32, top as i32],
                    size: [source.size.0 as i32, source.size.1 as i32],
                };
                let buffer = gpu
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("resample"),
                        contents: bytemuck::bytes_of(&uniform),
                        usage: wgpu::BufferUsages::UNIFORM,
                    });
                let bindings = gpu.bind(
                    &gpu.layouts.resample,
                    &[TextureView(source.tile.view()), buffer.as_entire_binding()],
                );
                pass.set_bind_group(0, &bindings, &[]);
                pass.draw(0..3, 0..1);
            }
            drop(pass);
            reused.insert(coord, target);
        }
    });
    reused
}
