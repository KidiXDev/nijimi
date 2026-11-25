use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use half::f16;
use nijimi_core::{LayerId, TILE_BYTES, TILE_SIZE, TileCoord, Tiles};

use crate::{
    EngineError,
    gpu::Gpu,
    project::TILES_PER_READBACK,
    readback::Readback,
    tile::{Tile, TileRef},
};

pub struct Fill {
    pub(crate) gpu: Rc<Gpu>,
    pub(crate) revision: u64,
    pub(crate) id: LayerId,
    pub(crate) size: (u32, u32),
    pub(crate) seed: (u32, u32),
    pub(crate) color: [f32; 4],
    pub(crate) tolerance: f32,
    pub(crate) tiles: Tiles<TileRef>,
}

pub struct Filled {
    pub(crate) gpu: Rc<Gpu>,
    pub(crate) revision: u64,
    pub(crate) id: LayerId,
    pub(crate) tiles: Vec<(TileCoord, Option<TileRef>)>,
}

impl Fill {
    pub async fn prepare(self) -> Result<Filled, EngineError> {
        let mut pixels = BTreeMap::new();
        let tiles: Vec<_> = self.tiles.iter().collect();
        for batch in tiles.chunks(TILES_PER_READBACK) {
            self.gpu.check_device()?;
            let readback = self.gpu.submit(|encoder| {
                Readback::record_tiles(
                    &self.gpu.device,
                    encoder,
                    batch.iter().map(|(_, tile)| tile.as_ref()),
                )
            });
            let data = readback.pixels().await?;
            for ((coord, _), tile) in batch.iter().zip(data.chunks_exact(TILE_BYTES)) {
                pixels.insert(**coord, tile.to_vec());
            }
        }
        let mut changes = Vec::new();
        let mut pixels =
            flood(pixels, self.size, self.seed, self.color, self.tolerance).into_iter();
        while pixels.len() > 0 {
            let batch: Vec<_> = pixels.by_ref().take(TILES_PER_READBACK).collect();
            let tiles = self
                .gpu
                .catch_errors(|| {
                    let tiles: Vec<_> = batch
                        .iter()
                        .map(|(coord, data)| {
                            let tile = data
                                .chunks_exact(8)
                                .any(|pixel| pixel[6] != 0 || pixel[7] & 0x7f != 0)
                                .then(|| {
                                    Rc::new(Tile::with_pixels(
                                        &self.gpu.device,
                                        &self.gpu.queue,
                                        data,
                                    ))
                                });
                            (*coord, tile)
                        })
                        .collect();
                    self.gpu.queue.submit([]);
                    tiles
                })
                .await?;
            changes.extend(tiles);
        }
        Ok(Filled {
            gpu: self.gpu,
            revision: self.revision,
            id: self.id,
            tiles: changes,
        })
    }
}

fn flood(
    mut tiles: BTreeMap<TileCoord, Vec<u8>>,
    (width, height): (u32, u32),
    seed: (u32, u32),
    [r, g, b, a]: [f32; 4],
    tolerance: f32,
) -> BTreeMap<TileCoord, Vec<u8>> {
    let [r, g, b, a] = [r * a, g * a, b * a, a].map(|value| f16::from_f32(value).to_le_bytes());
    let replacement = [r[0], r[1], g[0], g[1], b[0], b[1], a[0], a[1]];

    let sample = |tiles: &BTreeMap<TileCoord, Vec<u8>>, x, y| {
        let (coord, offset) = address(x, y);
        tiles.get(&coord).map_or([0; 8], |tile| {
            tile[offset..offset + 8].try_into().expect("RGBA16F pixel")
        })
    };
    let seed_pixel = sample(&tiles, seed.0, seed.1);
    if seed_pixel == replacement && tolerance == 0.0 {
        return BTreeMap::new();
    }
    let reference = straight(seed_pixel);
    let mut visited = vec![0u64; (width as usize * height as usize).div_ceil(64)];
    let mut changed = BTreeSet::new();
    let mut pending = vec![seed];
    let bit = |x: u32, y: u32| {
        let index = y as usize * width as usize + x as usize;
        (index / 64, 1u64 << (index % 64))
    };
    let matches = |tiles: &BTreeMap<TileCoord, Vec<u8>>, visited: &[u64], x, y| {
        let (index, mask) = bit(x, y);
        if visited[index] & mask != 0 {
            return false;
        }
        let pixel = sample(tiles, x, y);
        pixel == seed_pixel
            || (tolerance > 0.0
                && straight(pixel)
                    .into_iter()
                    .zip(reference)
                    .all(|(channel, reference)| (channel - reference).abs() <= tolerance))
    };
    while let Some((x, y)) = pending.pop() {
        if !matches(&tiles, &visited, x, y) {
            continue;
        }
        let mut left = x;
        while left > 0 && matches(&tiles, &visited, left - 1, y) {
            left -= 1;
        }
        let mut right = x + 1;
        while right < width && matches(&tiles, &visited, right, y) {
            right += 1;
        }
        for column in left..right {
            let (index, mask) = bit(column, y);
            visited[index] |= mask;
            if sample(&tiles, column, y) != replacement {
                let (coord, offset) = address(column, y);
                let tile = tiles.entry(coord).or_insert_with(|| vec![0; TILE_BYTES]);
                tile[offset..offset + 8].copy_from_slice(&replacement);
                changed.insert(coord);
            }
        }
        for row in [y.checked_sub(1), (y + 1 < height).then_some(y + 1)]
            .into_iter()
            .flatten()
        {
            let mut previous_matches = false;
            for column in left..right {
                let current_matches = matches(&tiles, &visited, column, row);
                if current_matches && !previous_matches {
                    pending.push((column, row));
                }
                previous_matches = current_matches;
            }
        }
    }
    tiles.retain(|coord, _| changed.contains(coord));
    tiles
}

fn address(x: u32, y: u32) -> (TileCoord, usize) {
    (
        TileCoord::new(x / TILE_SIZE, y / TILE_SIZE),
        ((y % TILE_SIZE * TILE_SIZE + x % TILE_SIZE) * 8) as usize,
    )
}

fn straight(pixel: [u8; 8]) -> [f32; 4] {
    let channel = |i| f16::from_le_bytes([pixel[i], pixel[i + 1]]).to_f32();
    let alpha = channel(6);
    if alpha <= 0.0 {
        [0.0; 4]
    } else {
        [
            channel(0) / alpha,
            channel(2) / alpha,
            channel(4) / alpha,
            alpha,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fill_crosses_tile_boundaries_and_stops_at_a_barrier() {
        let mut second = vec![0; TILE_BYTES];
        let (_, offset) = address(257, 0);
        second[offset + 6..offset + 8].copy_from_slice(&f16::ONE.to_le_bytes());
        let initial = BTreeMap::from([(TileCoord::new(1, 0), second)]);
        let tiles = flood(initial, (259, 1), (254, 0), [1.0, 0.0, 0.0, 1.0], 0.0);
        assert_eq!(tiles.len(), 2);
        let second = &tiles[&TileCoord::new(1, 0)];
        assert_eq!(&second[..2], &f16::ONE.to_le_bytes());
        assert_eq!(&second[offset..offset + 2], &[0; 2]);
        assert_eq!(&second[offset + 8..offset + 16], &[0; 8]);
    }
}
