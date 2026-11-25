use std::{collections::BTreeMap, rc::Rc};

use nijimi_core::{
    Document, Layer, LayerId, MAX_PROJECT_TILES, ProjectReader, ProjectWriter, TILE_BYTES,
    TileCoord, Tiles,
};

use crate::{
    EngineError,
    composite::Composite,
    gpu::Gpu,
    readback::Readback,
    tile::{Tile, TileRef},
};

const TILES_PER_LOAD_BATCH: usize = 8;
pub(crate) const TILES_PER_READBACK: usize = 32;

pub struct ProjectSnapshot {
    pub(crate) gpu: Rc<Gpu>,
    pub(crate) size: (u32, u32),
    pub(crate) layers: Vec<Layer<TileRef>>,
    pub(crate) active: Option<LayerId>,
}

impl ProjectSnapshot {
    pub async fn encode(self) -> Result<Vec<u8>, EngineError> {
        let tiles = tile_list(&self.layers);
        if tiles.len() > MAX_PROJECT_TILES {
            return Err(nijimi_core::ProjectError::TooManyTiles.into());
        }
        let mut writer = ProjectWriter::default();
        for batch in tiles.chunks(TILES_PER_READBACK) {
            self.gpu.check_device()?;
            let readback = self.gpu.submit(|encoder| {
                Readback::record_tiles(
                    &self.gpu.device,
                    encoder,
                    batch.iter().map(|(_, _, tile)| tile.as_ref()),
                )
            });
            let pixels = readback.pixels().await?;
            for ((layer, coord, _), pixels) in batch.iter().zip(pixels.chunks_exact(TILE_BYTES)) {
                writer.add_tile(*layer, *coord, pixels)?;
            }
        }
        Ok(writer.finish(self.size, &self.layers, self.active)?)
    }
}

pub struct ProjectLoader {
    pub(crate) gpu: Rc<Gpu>,
    pub(crate) view_buffer: wgpu::Buffer,
    pub(crate) file: Vec<u8>,
}

pub struct LoadedProject {
    pub(crate) document: Document<TileRef>,
    pub(crate) composite: Composite,
}

impl ProjectLoader {
    pub async fn load(self) -> Result<LoadedProject, EngineError> {
        let Self {
            gpu,
            view_buffer,
            file,
        } = self;
        let mut reader = ProjectReader::open(file)?;
        let size = reader.document().size();
        gpu.check_size(size)?;
        let composite = gpu
            .catch_errors(|| Composite::new(&gpu, size, &view_buffer))
            .await?;

        let mut uploaded = BTreeMap::new();
        for batch in tile_list(reader.document().layers()).chunks(TILES_PER_LOAD_BATCH) {
            let pixels = batch
                .iter()
                .map(|(layer, coord, ())| reader.read_tile(*layer, *coord))
                .collect::<Result<Vec<_>, _>>()?;
            let tiles = gpu
                .catch_errors(|| {
                    let tiles: Vec<_> = pixels
                        .iter()
                        .map(|pixels| Rc::new(Tile::with_pixels(&gpu.device, &gpu.queue, pixels)))
                        .collect();
                    gpu.queue.submit([]);
                    tiles
                })
                .await?;
            uploaded.extend(
                batch
                    .iter()
                    .zip(tiles)
                    .map(|((layer, coord, ()), tile)| ((*layer, *coord), tile)),
            );
        }

        let layers = reader
            .document()
            .layers()
            .iter()
            .map(|layer| layer.map_tiles(&mut |id, coord, ()| uploaded[&(id, coord)].clone()))
            .collect();
        let document = Document::with_layers(size, layers, reader.document().active())?;
        Ok(LoadedProject {
            document,
            composite,
        })
    }
}

fn tile_list<T: Clone>(layers: &[Layer<T>]) -> Vec<(LayerId, TileCoord, T)> {
    let mut list = Vec::new();
    for layer in layers {
        layer.visit(&mut |layer| {
            let tiles = layer.tiles().into_iter().flat_map(Tiles::iter);
            list.extend(tiles.map(|(coord, tile)| (layer.id, *coord, tile.clone())));
        });
    }
    list
}
