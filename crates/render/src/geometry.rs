use std::{collections::BTreeMap, rc::Rc};

use bytemuck::{Pod, Zeroable};
use nijimi_core::{MAX_COORDINATE, Point, TILE_SIZE, TileCoord, Tiles, Transform};
use wgpu::{BindingResource::TextureView, util::DeviceExt};

use crate::{
    EngineError,
    gpu::{CLEAR, Gpu, REVEALED, begin_pass},
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
    size: [f32; 2],
}

struct Source<'a> {
    coord: TileCoord,
    tile: &'a TileRef,
    size: (u32, u32),
    clip: (u32, u32, u32, u32),
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
        let corners = [
            affine.map([left as f32, top as f32]),
            affine.map([(left + width) as f32, top as f32]),
            affine.map([left as f32, (top + height) as f32]),
            affine.map([(left + width) as f32, (top + height) as f32]),
        ];
        let extent = |axis, limit| {
            let values = corners.map(|point| point[axis]);
            let start =
                (values.into_iter().fold(f32::INFINITY, f32::min).floor() as u32).min(limit);
            let end =
                (values.into_iter().fold(f32::NEG_INFINITY, f32::max).ceil() as u32).min(limit);
            start..end
        };
        let xs = extent(0, new_size.0);
        let ys = extent(1, new_size.1);
        if xs.is_empty() || ys.is_empty() {
            continue;
        }

        let first = TileCoord::new(xs.start / TILE_SIZE, ys.start / TILE_SIZE);
        let rotated = affine.x != [1.0, 0.0] || affine.y != [0.0, 1.0];
        if !rotated
            && corners[0] == [xs.start as f32, ys.start as f32]
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
                    offset: [x - left as f32, y - top as f32],
                    size: [source.size.0 as f32, source.size.1 as f32],
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
