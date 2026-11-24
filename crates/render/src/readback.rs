use futures_channel::oneshot;

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

    pub(crate) fn record_tile(
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        tile: &Tile,
    ) -> Self {
        Self::record(device, encoder, tile.texture(), RGBA16F_BYTES)
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
        Ok(mapped
            .chunks_exact(self.padded_row)
            .flat_map(|padded| &padded[..self.row])
            .copied()
            .collect())
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
