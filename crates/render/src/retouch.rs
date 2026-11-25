use std::{collections::BTreeMap, rc::Rc};

use bytemuck::{Pod, Zeroable};
use nijimi_core::{Adjustment, Dab, TILE_SIZE, TileCoord, Tiles};
use wgpu::{
    BindingResource::{Sampler, TextureView},
    util::DeviceExt,
};

use crate::{
    gpu::{CLEAR, Gpu, begin_pass, draw_fullscreen, draw_tile, texture_descriptor},
    grade::{Grade, grown, neighbors},
    stroke::{DabInstance, Frame, PaintBindings, StrokeTile},
    tile::{TILE_FORMAT, Tile, TileRef},
};

const MAX_MEMBRANE_SIZE: u32 = 1024;
const RELAXATIONS: usize = 8;

// Shifted tiles
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Area {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Area {
    pub const TILE: Self = Self {
        x: 0,
        y: 0,
        width: TILE_SIZE,
        height: TILE_SIZE,
    };

    fn under(dab: &Dab, coord: TileCoord, (width, height): (u32, u32)) -> Self {
        let (left, top) = coord.origin();
        let start = |center: f32, origin: u32| {
            (center - dab.radius - origin as f32)
                .floor()
                .clamp(0.0, TILE_SIZE as f32) as u32
        };
        let end = |center: f32, origin: u32, limit: u32| {
            (center + dab.radius - origin as f32)
                .ceil()
                .clamp(0.0, TILE_SIZE.min(limit - origin) as f32) as u32
        };
        let (x, y) = (start(dab.center.x, left), start(dab.center.y, top));
        Self {
            x,
            y,
            width: end(dab.center.x, left, width).saturating_sub(x),
            height: end(dab.center.y, top, height).saturating_sub(y),
        }
    }

    fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

struct Span {
    tile: u32,
    from: u32,
    to: u32,
    length: u32,
}

fn spans(target: u32, start: u32, length: u32, offset: i32, limit: u32) -> Vec<Span> {
    let shift = i64::from(target * TILE_SIZE) + i64::from(offset);
    let first = (shift + i64::from(start)).max(0);
    let last = (shift + i64::from(start + length)).min(i64::from(limit));
    let mut spans = Vec::new();
    let mut position = first;
    while position < last {
        let tile = position as u32 / TILE_SIZE;
        let end = last.min(i64::from((tile + 1) * TILE_SIZE));
        spans.push(Span {
            tile,
            from: position as u32 % TILE_SIZE,
            to: (position - shift) as u32,
            length: (end - position) as u32,
        });
        position = end;
    }
    spans
}

pub(crate) struct Piece {
    pub coord: TileCoord,
    from: (u32, u32),
    to: (u32, u32),
    size: (u32, u32),
}

pub(crate) fn pieces(
    coord: TileCoord,
    area: Area,
    offset: (i32, i32),
    size: (u32, u32),
) -> Vec<Piece> {
    let columns = spans(coord.x, area.x, area.width, offset.0, size.0);
    let rows = spans(coord.y, area.y, area.height, offset.1, size.1);
    rows.iter()
        .flat_map(|row| {
            columns.iter().map(|column| Piece {
                coord: TileCoord::new(column.tile, row.tile),
                from: (column.from, row.from),
                to: (column.to, row.to),
                size: (column.length, row.length),
            })
        })
        .collect()
}

impl Piece {
    pub fn copy(&self, encoder: &mut wgpu::CommandEncoder, source: &Tile, target: &Tile) {
        fn corner(tile: &Tile, (x, y): (u32, u32)) -> wgpu::TexelCopyTextureInfo<'_> {
            wgpu::TexelCopyTextureInfo {
                origin: wgpu::Origin3d { x, y, z: 0 },
                ..tile.texture().as_image_copy()
            }
        }
        encoder.copy_texture_to_texture(
            corner(source, self.from),
            corner(target, self.to),
            wgpu::Extent3d {
                width: self.size.0,
                height: self.size.1,
                depth_or_array_layers: 1,
            },
        );
    }
}

pub(crate) fn shifted_tile(
    frame: &mut Frame<'_>,
    coord: TileCoord,
    offset: (i32, i32),
) -> Option<TileRef> {
    let mut target = None;
    for piece in pieces(coord, Area::TILE, offset, frame.size) {
        if let Some(source) = frame.before.get(&piece.coord) {
            let target = target.get_or_insert_with(|| frame.gpu.new_tile());
            piece.copy(frame.encoder, source, target);
        }
    }
    target.map(Rc::new)
}

// Blur
pub(crate) struct Blur {
    radius: u32,
    horizontal: Tiles<TileRef>,
}

impl Blur {
    pub fn new(radius: u32) -> Self {
        Self {
            radius,
            horizontal: Tiles::new(),
        }
    }

    pub fn tile(&mut self, frame: &mut Frame<'_>, coord: TileCoord) -> Option<TileRef> {
        let Frame {
            gpu,
            encoder,
            size,
            before,
            ..
        } = frame;
        let blur = Adjustment::Blur {
            radius: self.radius,
        };
        let mut pass = |coord, horizontal, input: &Tiles<TileRef>| {
            let nearby = neighbors(gpu, coord, input, true);
            if nearby.iter().all(|tile| Rc::ptr_eq(tile, &gpu.empty_tile)) {
                return None;
            }
            let target = Rc::new(gpu.new_tile());
            let base = before.get(&coord).unwrap_or(&gpu.empty_tile);
            let bindings = Grade::new(gpu, &blur, horizontal, 1.0, 0.0).bindings(
                gpu,
                *size,
                coord,
                &nearby,
                base,
                gpu.mask_default(true),
            );
            draw_fullscreen(encoder, target.view(), &gpu.pipelines.grade, &bindings);
            Some(target)
        };
        for row in grown(&[coord].into(), *size, (0, 1)) {
            if !self.horizontal.contains_key(&row)
                && let Some(tile) = pass(row, true, before)
            {
                self.horizontal.insert(row, tile);
            }
        }
        pass(coord, false, &self.horizontal)
    }
}

// Smudge
#[derive(Default)]
pub(crate) struct Smudge {
    last: Option<(i32, i32)>,
    slots: Vec<Slot>,
}

struct Slot {
    under: Tile,
    source: Tile,
    bindings: wgpu::BindGroup,
}

impl Smudge {
    pub fn record(
        &mut self,
        frame: &mut Frame<'_>,
        dab: &Dab,
        tiles: &mut BTreeMap<TileCoord, StrokeTile>,
        paint: &PaintBindings,
    ) -> Vec<TileCoord> {
        let position = (dab.center.x.round() as i32, dab.center.y.round() as i32);
        let Some(last) = self.last.replace(position) else {
            return Vec::new();
        };
        let delta = (position.0 - last.0, position.1 - last.1);
        if delta == (0, 0) {
            return Vec::new();
        }
        let away = (-delta.0, -delta.1);
        let (gpu, size, before) = (frame.gpu, frame.size, frame.before);
        let mut targets = Vec::new();
        for coord in dab.tiles(size).filter(|coord| paint.touches(*coord)) {
            let area = Area::under(dab, coord, size);
            let sources = pieces(coord, area, away, size);
            let exists = |coord| tiles.contains_key(&coord) || before.contains_key(&coord);
            if !area.is_empty()
                && !sources.is_empty()
                && (exists(coord) || sources.iter().any(|piece| exists(piece.coord)))
            {
                targets.push((coord, area, sources));
            }
        }
        while self.slots.len() < targets.len() {
            let (under, source) = (gpu.new_tile(), gpu.new_tile());
            let bindings = paint.bind(gpu, &under, &gpu.empty_tile, &source);
            self.slots.push(Slot {
                under,
                source,
                bindings,
            });
        }
        for ((coord, area, sources), slot) in targets.iter().zip(&self.slots) {
            tiles
                .entry(*coord)
                .or_insert_with(|| StrokeTile::copied(frame, *coord));
            for piece in pieces(*coord, *area, (0, 0), size) {
                piece.copy(frame.encoder, tiles[coord].result(), &slot.under);
            }
            for piece in sources {
                let source = tiles
                    .get(&piece.coord)
                    .map(StrokeTile::result)
                    .or(before.get(&piece.coord))
                    .unwrap_or(&gpu.empty_tile);
                piece.copy(frame.encoder, source, &slot.source);
            }
        }
        let instances: Vec<_> = targets
            .iter()
            .map(|(coord, ..)| DabInstance::new(dab, *coord, delta))
            .collect();
        if instances.is_empty() {
            return Vec::new();
        }
        let buffer = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("smudge"),
                contents: bytemuck::cast_slice(&instances),
                usage: wgpu::BufferUsages::VERTEX,
            });
        for (index, ((coord, area, _), slot)) in targets.iter().zip(&self.slots).enumerate() {
            let mut pass = begin_pass(
                frame.encoder,
                tiles[coord].result().view(),
                wgpu::LoadOp::Load,
            );
            pass.set_scissor_rect(area.x, area.y, area.width, area.height);
            pass.set_pipeline(&gpu.pipelines.smudge);
            pass.set_bind_group(0, &slot.bindings, &[]);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..4, index as u32..index as u32 + 1);
        }
        targets.into_iter().map(|(coord, ..)| coord).collect()
    }
}

// Heal
#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
struct HealUniform {
    origin: [i32; 2],
    extent: [i32; 2],
    scale: i32,
    padding: i32,
}

struct Level {
    known: wgpu::TextureView,
    solution: wgpu::TextureView,
    spare: wgpu::TextureView,
}

pub(crate) fn heal(
    gpu: &Gpu,
    before: &Tiles<TileRef>,
    tiles: &BTreeMap<TileCoord, StrokeTile>,
    paint: &PaintBindings,
) {
    let healed: Vec<_> = tiles
        .iter()
        .filter_map(|(coord, tile)| Some((*coord, tile, before.get(coord)?)))
        .collect();
    let range = |axis: fn(&TileCoord) -> u32| {
        let values = || healed.iter().map(|(coord, ..)| axis(coord));
        Some((values().min()?, values().max()? + 1))
    };
    let (Some((left, right)), Some((top, bottom))) = (range(|c| c.x), range(|c| c.y)) else {
        return;
    };
    let extent = ((right - left) * TILE_SIZE, (bottom - top) * TILE_SIZE);
    let mut scale = 1;
    while extent.0.max(extent.1) / scale > MAX_MEMBRANE_SIZE {
        scale *= 2;
    }
    let levels = create_levels(gpu, (extent.0 / scale, extent.1 / scale));
    let corner = |coord: TileCoord| ((coord.x - left) * TILE_SIZE, (coord.y - top) * TILE_SIZE);
    let empty = gpu.empty_tile.view();
    let bind = |textures: [&wgpu::TextureView; 3], uniform: HealUniform| {
        let buffer = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("heal"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let [a, b, c] = textures.map(TextureView);
        gpu.bind(
            &gpu.layouts.heal,
            &[a, b, c, Sampler(&gpu.sampler), buffer.as_entire_binding()],
        )
    };
    let placed = |coord: TileCoord, scale: u32| {
        let (x, y) = corner(coord);
        HealUniform {
            origin: [x as i32, y as i32],
            extent: [extent.0 as i32, extent.1 as i32],
            scale: scale as i32,
            ..HealUniform::default()
        }
    };
    let _kept_until_submitted: Vec<Tile> = gpu.submit(|encoder| {
        let mut pass = begin_pass(encoder, &levels[0].known, CLEAR);
        pass.set_pipeline(&gpu.pipelines.heal_base);
        for (coord, tile, before) in &healed {
            let (x, y) = corner(*coord);
            let side = TILE_SIZE / scale;
            pass.set_scissor_rect(x / scale, y / scale, side, side);
            let textures = [before.view(), tile.coverage(gpu), tile.replacement().view()];
            pass.set_bind_group(0, &bind(textures, placed(*coord, scale)), &[]);
            pass.draw(0..3, 0..1);
        }
        drop(pass);
        for pair in levels.windows(2) {
            let bindings = bind([&pair[0].known, empty, empty], HealUniform::default());
            draw_fullscreen(encoder, &pair[1].known, &gpu.pipelines.heal_pull, &bindings);
        }
        let mut coarser = empty;
        for level in levels.iter().rev() {
            let bindings = bind([&level.known, coarser, empty], HealUniform::default());
            draw_fullscreen(
                encoder,
                &level.solution,
                &gpu.pipelines.heal_push,
                &bindings,
            );
            let (mut current, mut next) = (&level.solution, &level.spare);
            for _ in 0..RELAXATIONS {
                let bindings = bind([&level.known, current, empty], HealUniform::default());
                draw_fullscreen(encoder, next, &gpu.pipelines.heal_relax, &bindings);
                (current, next) = (next, current);
            }
            coarser = &level.solution;
        }
        let mut results = Vec::new();
        for (coord, tile, before) in &healed {
            let target = gpu.new_tile();
            let textures = [
                before.view(),
                tile.replacement().view(),
                &levels[0].solution,
            ];
            let bindings = bind(textures, placed(*coord, 1));
            draw_fullscreen(encoder, target.view(), &gpu.pipelines.heal, &bindings);
            let bindings = paint.bind_coverage(gpu, before, tile.coverage(gpu), &target);
            draw_tile(
                encoder,
                tile.result().view(),
                &gpu.pipelines.replace_stroke,
                &bindings,
                *coord,
            );
            results.push(target);
        }
        results
    });
}

fn create_levels(gpu: &Gpu, mut size: (u32, u32)) -> Vec<Level> {
    let mut levels = Vec::new();
    loop {
        let texture = || {
            let usage =
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
            gpu.device
                .create_texture(&texture_descriptor("heal", size, TILE_FORMAT, usage))
                .create_view(&Default::default())
        };
        levels.push(Level {
            known: texture(),
            solution: texture(),
            spare: texture(),
        });
        if size == (1, 1) {
            return levels;
        }
        size = (size.0.div_ceil(2), size.1.div_ceil(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shifted_tile_is_cut_from_every_tile_it_overlaps() {
        let found: Vec<_> = pieces(TileCoord::new(1, 0), Area::TILE, (10, -20), (600, 300))
            .into_iter()
            .map(|piece| (piece.coord.into(), piece.from, piece.to, piece.size))
            .collect();
        let expected: [((u32, u32), _, _, _); 2] = [
            ((1, 0), (10, 0), (0, 20), (246, 236)),
            ((2, 0), (0, 0), (246, 20), (10, 236)),
        ];
        assert_eq!(found, expected);
        let outside = pieces(TileCoord::new(2, 0), Area::TILE, (200, 0), (600, 300));
        assert!(outside.is_empty());
    }
}
