use std::rc::Rc;

use nijimi_core::{TILE_BYTES, TILE_SIZE};

pub(crate) const TILE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub(crate) const COVERAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;

pub type TileRef = Rc<Tile>;

#[derive(Debug)]
pub struct Tile {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Tile {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tile"),
            size: wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Self { texture, view }
    }

    pub(crate) fn with_pixels(device: &wgpu::Device, queue: &wgpu::Queue, pixels: &[u8]) -> Self {
        let tile = Self::new(device, TILE_FORMAT);
        queue.write_texture(
            tile.texture.as_image_copy(),
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(TILE_BYTES as u32 / TILE_SIZE),
                rows_per_image: None,
            },
            tile.texture.size(),
        );
        tile
    }

    pub(crate) fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub(crate) fn view(&self) -> &wgpu::TextureView {
        &self.view
    }
}

impl Drop for Tile {
    fn drop(&mut self) {
        self.texture.destroy();
    }
}
