use futures_channel::oneshot;
use nijimi_core::{Histogram, TILE_BYTES, TILE_SIZE};

use crate::{EngineError, tile::Tile};

const RGBA8_BYTES: u32 = 4;
const RGBA16F_BYTES: u32 = 8;

pub struct Readback {
    device: wgpu::Device,
    buffer: wgpu::Buffer,
    row: usize,
    padded_row: usize,
}

impl Readback {
    pub(crate) fn record_image(
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        image: &wgpu::Texture,
    ) -> Self {
        Self::record(device, encoder, image, RGBA8_BYTES)
    }

    pub(crate) fn record_tiles<'a>(
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        tiles: impl ExactSizeIterator<Item = &'a Tile>,
    ) -> Self {
        let row = TILE_SIZE * RGBA16F_BYTES;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tile readback"),
            size: (tiles.len() * TILE_BYTES) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        for (index, tile) in tiles.enumerate() {
            encoder.copy_texture_to_buffer(
                tile.texture().as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: (index * TILE_BYTES) as u64,
                        bytes_per_row: Some(row),
                        rows_per_image: None,
                    },
                },
                tile.texture().size(),
            );
        }
        Self {
            device: device.clone(),
            buffer,
            row: row as usize,
            padded_row: row as usize,
        }
    }

    fn record(
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        bytes_per_pixel: u32,
    ) -> Self {
        let size = texture.size();
        let row = size.width * bytes_per_pixel;
        let padded_row = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(padded_row) * u64::from(size.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: None,
                },
            },
            size,
        );
        Self {
            device: device.clone(),
            buffer,
            row: row as usize,
            padded_row: padded_row as usize,
        }
    }

    pub async fn histogram(self) -> Result<Histogram, EngineError> {
        let pixels = self.pixels().await?;
        Ok(Histogram::from_rgba8(pixels.as_chunks().0))
    }

    pub async fn pixels(self) -> Result<Vec<u8>, EngineError> {
        let (sender, receiver) = oneshot::channel();
        self.buffer
            .map_async(wgpu::MapMode::Read, .., move |result| {
                let _ = sender.send(result);
            });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(gpu_error)?;
        receiver.await.map_err(gpu_error)?.map_err(gpu_error)?;

        let mapped = self.buffer.get_mapped_range(..).map_err(gpu_error)?;
        if self.row == self.padded_row {
            return Ok(mapped.to_vec());
        }
        let mut pixels = Vec::with_capacity(mapped.len() / self.padded_row * self.row);
        for row in mapped.chunks_exact(self.padded_row) {
            pixels.extend_from_slice(&row[..self.row]);
        }
        Ok(pixels)
    }
}

impl Drop for Readback {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

fn gpu_error(error: impl std::error::Error) -> EngineError {
    EngineError::Gpu(error.to_string())
}
