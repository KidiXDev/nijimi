use futures_channel::oneshot;

use crate::RenderError;

const BYTES_PER_PIXEL: u32 = 4;

pub struct Readback {
    device: wgpu::Device,
    buffer: wgpu::Buffer,
    width: u32,
    padded_row: u32,
}

impl Readback {
    pub(crate) fn record(
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
    ) -> Self {
        let size = texture.size();
        let padded_row =
            (size.width * BYTES_PER_PIXEL).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
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
            width: size.width,
            padded_row,
        }
    }

    pub async fn pixels(self) -> Result<Vec<u8>, RenderError> {
        let (sender, receiver) = oneshot::channel();
        self.buffer
            .map_async(wgpu::MapMode::Read, .., move |result| {
                let _ = sender.send(result);
            });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| RenderError::Readback(error.to_string()))?;
        receiver
            .await
            .map_err(|error| RenderError::Readback(error.to_string()))?
            .map_err(|error| RenderError::Readback(error.to_string()))?;

        let mapped = self
            .buffer
            .get_mapped_range(..)
            .map_err(|error| RenderError::Readback(error.to_string()))?;
        let row = (self.width * BYTES_PER_PIXEL) as usize;
        Ok(mapped
            .chunks_exact(self.padded_row as usize)
            .flat_map(|padded| &padded[..row])
            .copied()
            .collect())
    }
}
