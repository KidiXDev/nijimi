use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::Arc,
};

use half::f16;
use nijimi_core::{LayerId, Selection, TILE_BYTES, TILE_SIZE, TileCoord, Tiles};

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
    pub(crate) selection: Option<Arc<Selection>>,
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
        let selection = self.selection.as_deref();
        let filled = flood(
            pixels,
            self.size,
            self.seed,
            self.color,
            self.tolerance,
            selection,
        );
        let mut pixels = filled.into_iter();
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

pub(crate) struct Region {
    width: u32,
    bits: Vec<u64>,
}

impl Region {
    fn new((width, height): (u32, u32)) -> Self {
        Self {
            width,
            bits: vec![0; (width as usize * height as usize).div_ceil(64)],
        }
    }

    fn bit(&self, x: u32, y: u32) -> (usize, u64) {
        let index = y as usize * self.width as usize + x as usize;
        (index / 64, 1 << (index % 64))
    }

    pub fn contains(&self, x: u32, y: u32) -> bool {
        let (index, mask) = self.bit(x, y);
        self.bits[index] & mask != 0
    }

    fn insert(&mut self, x: u32, y: u32) {
        let (index, mask) = self.bit(x, y);
        self.bits[index] |= mask;
    }

    fn pixels(&self) -> impl Iterator<Item = (u32, u32)> {
        let width = self.width as usize;
        self.bits
            .iter()
            .enumerate()
            .flat_map(|(index, word)| {
                let word = *word;
                (0..if word == 0 { 0 } else { 64 })
                    .filter(move |bit| word >> bit & 1 == 1)
                    .map(move |bit| index * 64 + bit)
            })
            .map(move |index| ((index % width) as u32, (index / width) as u32))
    }
}

pub(crate) fn region(
    size: (u32, u32),
    seed: (u32, u32),
    contiguous: bool,
    matches: impl Fn(u32, u32) -> bool,
) -> Region {
    let (width, height) = size;
    let mut found = Region::new(size);
    if !contiguous {
        for y in 0..height {
            for x in 0..width {
                if matches(x, y) {
                    found.insert(x, y);
                }
            }
        }
        return found;
    }
    let mut pending = vec![seed];
    let is_new = |found: &Region, x, y| !found.contains(x, y) && matches(x, y);
    while let Some((x, y)) = pending.pop() {
        if !is_new(&found, x, y) {
            continue;
        }
        let mut left = x;
        while left > 0 && is_new(&found, left - 1, y) {
            left -= 1;
        }
        let mut right = x + 1;
        while right < width && is_new(&found, right, y) {
            right += 1;
        }
        for column in left..right {
            found.insert(column, y);
        }
        for row in [y.checked_sub(1), (y + 1 < height).then_some(y + 1)]
            .into_iter()
            .flatten()
        {
            let mut previous_matches = false;
            for column in left..right {
                let current_matches = is_new(&found, column, row);
                if current_matches && !previous_matches {
                    pending.push((column, row));
                }
                previous_matches = current_matches;
            }
        }
    }
    found
}

fn flood(
    mut tiles: BTreeMap<TileCoord, Vec<u8>>,
    size: (u32, u32),
    seed: (u32, u32),
    [r, g, b, a]: [f32; 4],
    tolerance: f32,
    selection: Option<&Selection>,
) -> BTreeMap<TileCoord, Vec<u8>> {
    let replacement = [r * a, g * a, b * a, a];
    let seed_pixel = sample(&tiles, seed.0, seed.1);
    if seed_pixel == to_bytes(replacement) && tolerance == 0.0 {
        return BTreeMap::new();
    }
    let reference = straight(seed_pixel);
    let matched = region(size, seed, true, |x, y| {
        let pixel = sample(&tiles, x, y);
        pixel == seed_pixel
            || (tolerance > 0.0
                && straight(pixel)
                    .into_iter()
                    .zip(reference)
                    .all(|(channel, reference)| (channel - reference).abs() <= tolerance))
    });
    let mut changed = BTreeSet::new();
    for (x, y) in matched.pixels() {
        let weight = selection.map_or(u8::MAX, |selection| selection.value(x, y));
        let before = sample(&tiles, x, y);
        let after = match weight {
            0 => continue,
            u8::MAX => to_bytes(replacement),
            _ => {
                let weight = f32::from(weight) / 255.0;
                let before = channels(before);
                to_bytes(std::array::from_fn(|index| {
                    before[index] + (replacement[index] - before[index]) * weight
                }))
            }
        };
        if after != before {
            let (coord, offset) = address(x, y);
            let tile = tiles.entry(coord).or_insert_with(|| vec![0; TILE_BYTES]);
            tile[offset..offset + 8].copy_from_slice(&after);
            changed.insert(coord);
        }
    }
    tiles.retain(|coord, _| changed.contains(coord));
    tiles
}

fn sample(tiles: &BTreeMap<TileCoord, Vec<u8>>, x: u32, y: u32) -> [u8; 8] {
    let (coord, offset) = address(x, y);
    tiles.get(&coord).map_or([0; 8], |tile| {
        tile[offset..offset + 8].try_into().expect("RGBA16F pixel")
    })
}

fn address(x: u32, y: u32) -> (TileCoord, usize) {
    (
        TileCoord::new(x / TILE_SIZE, y / TILE_SIZE),
        ((y % TILE_SIZE * TILE_SIZE + x % TILE_SIZE) * 8) as usize,
    )
}

fn to_bytes(pixel: [f32; 4]) -> [u8; 8] {
    let [r, g, b, a] = pixel.map(|value| f16::from_f32(value).to_le_bytes());
    [r[0], r[1], g[0], g[1], b[0], b[1], a[0], a[1]]
}

fn channels(pixel: [u8; 8]) -> [f32; 4] {
    std::array::from_fn(|index| {
        f16::from_le_bytes([pixel[index * 2], pixel[index * 2 + 1]]).to_f32()
    })
}

fn straight(pixel: [u8; 8]) -> [f32; 4] {
    let [r, g, b, alpha] = channels(pixel);
    if alpha <= 0.0 {
        [0.0; 4]
    } else {
        [r / alpha, g / alpha, b / alpha, alpha]
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
        let tiles = flood(initial, (259, 1), (254, 0), [1.0, 0.0, 0.0, 1.0], 0.0, None);
        assert_eq!(tiles.len(), 2);
        let second = &tiles[&TileCoord::new(1, 0)];
        assert_eq!(&second[..2], &f16::ONE.to_le_bytes());
        assert_eq!(&second[offset..offset + 2], &[0; 2]);
        assert_eq!(&second[offset + 8..offset + 16], &[0; 8]);
    }
}
