use std::{
    cell::RefCell,
    sync::{Arc, Weak},
};

use bytemuck::{Pod, Zeroable};
use nijimi_core::{Adjustment, CubeLut, LutKind, TileCoord, Tiles};
use wgpu::{BindingResource::TextureView, util::DeviceExt};

use crate::{gpu::Gpu, tile::TileRef};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniform {
    origin: [i32; 2],
    size: [i32; 2],
    op: u32,
    seed: u32,
    opacity: f32,
    blend: f32,
    params: [[f32; 4]; 3],
}

#[derive(Default)]
pub(crate) struct LutCache(RefCell<Vec<(Weak<CubeLut>, wgpu::Buffer)>>);

impl LutCache {
    pub fn prune(&self) {
        self.0
            .borrow_mut()
            .retain(|(table, _)| table.strong_count() > 0);
    }

    fn buffer(&self, gpu: &Gpu, table: &Arc<CubeLut>) -> wgpu::Buffer {
        let mut entries = self.0.borrow_mut();
        let key = Arc::downgrade(table);
        if let Some((_, buffer)) = entries.iter().find(|(existing, _)| existing.ptr_eq(&key)) {
            return buffer.clone();
        }
        let rows: Vec<_> = table
            .samples
            .iter()
            .map(|&[r, g, b]| [r, g, b, 0.0])
            .collect();
        let buffer = make_table(&gpu.device, &rows);
        entries.push((key, buffer.clone()));
        buffer
    }
}

pub(crate) struct Grade {
    uniform: Uniform,
    table: wgpu::Buffer,
}

impl Grade {
    pub fn new(
        gpu: &Gpu,
        adjustment: &Adjustment,
        horizontal: bool,
        opacity: f32,
        blend: f32,
    ) -> Self {
        let mut uniform = Uniform::zeroed();
        uniform.opacity = opacity;
        uniform.blend = blend;
        let mut rows = Vec::new();
        match adjustment {
            Adjustment::BrightnessContrast {
                brightness,
                contrast,
            } => {
                uniform.op = 1;
                uniform.params[0] = [*brightness, (3.0 * contrast).exp2(), 0.0, 0.0];
            }
            Adjustment::Levels {
                black,
                white,
                gamma,
                output_black,
                output_white,
            } => {
                uniform.op = 2;
                uniform.params[0] = [*black, *white, gamma.recip(), *output_black];
                uniform.params[1][0] = *output_white;
            }
            Adjustment::Curves { points } => {
                uniform.op = 3;
                uniform.params[0][0] = points.len() as f32;
                rows.extend(points.iter().map(|point| [point.x, point.y, 0.0, 0.0]));
            }
            Adjustment::Hsl {
                hue,
                saturation,
                lightness,
            } => {
                uniform.op = 4;
                uniform.params[0] = [hue / 360.0, *saturation, *lightness, 0.0];
            }
            Adjustment::ColorBalance {
                shadows,
                midtones,
                highlights,
            } => {
                uniform.op = 5;
                uniform.params = [*shadows, *midtones, *highlights].map(|[r, g, b]| [r, g, b, 0.0]);
            }
            Adjustment::Exposure { stops } => {
                uniform.op = 6;
                uniform.params[0][0] = stops.exp2();
            }
            Adjustment::Lut { table } => {
                uniform.op = match table.info.kind {
                    LutKind::OneDimensional => 7,
                    LutKind::ThreeDimensional => 8,
                };
                let [r, g, b] = table.info.domain_min;
                uniform.params[0] = [r, g, b, table.info.size as f32];
                let [r, g, b] = table.info.domain_max;
                uniform.params[1] = [r, g, b, 0.0];
                return Self {
                    uniform,
                    table: gpu.luts.buffer(gpu, table),
                };
            }
            Adjustment::Sharpen { amount } => {
                uniform.op = 9;
                uniform.params[0][0] = *amount;
            }
            Adjustment::Blur { radius } => {
                uniform.op = if horizontal { 10 } else { 11 };
                uniform.params[0][0] = *radius as f32;
                let sigma = (*radius as f32 / 2.0).max(0.5);
                rows.extend(
                    (0..=*radius)
                        .map(|i| [(-0.5 * (i as f32 / sigma).powi(2)).exp(), 0.0, 0.0, 0.0]),
                );
                let total = 2.0 * rows.iter().map(|row| row[0]).sum::<f32>() - rows[0][0];
                for row in &mut rows {
                    row[0] /= total;
                }
            }
            Adjustment::Vignette {
                amount,
                radius,
                feather,
            } => {
                uniform.op = 12;
                uniform.params[0] = [*amount, *radius, *feather, 0.0];
            }
            Adjustment::Grain { amount, seed } => {
                uniform.op = 13;
                uniform.params[0][0] = *amount;
                uniform.seed = *seed;
            }
        }
        let table = if rows.is_empty() {
            gpu.empty_grade_table.clone()
        } else {
            make_table(&gpu.device, &rows)
        };
        Self { uniform, table }
    }

    pub fn bindings(
        &self,
        gpu: &Gpu,
        size: (u32, u32),
        coord: TileCoord,
        neighborhood: &[&TileRef; 9],
        original: &TileRef,
        mask: &TileRef,
    ) -> wgpu::BindGroup {
        let mut uniform = self.uniform;
        let (x, y) = coord.origin();
        uniform.origin = [x as i32, y as i32];
        uniform.size = [size.0 as i32, size.1 as i32];
        let buffer = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("grade"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let mut resources: Vec<_> = neighborhood
            .iter()
            .map(|tile| TextureView(tile.view()))
            .collect();
        resources.extend([
            TextureView(original.view()),
            TextureView(mask.view()),
            buffer.as_entire_binding(),
            self.table.as_entire_binding(),
        ]);
        gpu.bind(&gpu.layouts.grade, &resources)
    }
}

pub(crate) fn neighbors<'a>(
    gpu: &'a Gpu,
    coord: TileCoord,
    tiles: &'a Tiles<TileRef>,
    spatial: bool,
) -> [&'a TileRef; 9] {
    let mut neighborhood = [&gpu.empty_tile; 9];
    neighborhood[4] = tiles.get(&coord).unwrap_or(&gpu.empty_tile);
    if spatial {
        for (index, tile) in neighborhood.iter_mut().enumerate() {
            let dx = (index % 3) as i32 - 1;
            let dy = (index / 3) as i32 - 1;
            if let (Some(x), Some(y)) = (
                coord.x.checked_add_signed(dx),
                coord.y.checked_add_signed(dy),
            ) && let Some(source) = tiles.get(&TileCoord::new(x, y))
            {
                *tile = source;
            }
        }
    }
    neighborhood
}

pub(crate) fn make_table(device: &wgpu::Device, rows: &[[f32; 4]]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("grade table"),
        contents: bytemuck::cast_slice(rows),
        usage: wgpu::BufferUsages::STORAGE,
    })
}
