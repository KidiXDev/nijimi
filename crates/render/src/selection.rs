use std::{rc::Rc, sync::Arc};

use nijimi_core::{PixelRect, Selection, SelectionMode};
use wgpu::util::DeviceExt;

use crate::{
    EngineError,
    fill::region,
    gpu::{Gpu, texture_descriptor},
    readback::Readback,
};

pub(crate) struct SelectionView {
    selection: Option<Arc<Selection>>,
    view: wgpu::TextureView,
}

impl SelectionView {
    pub fn new(gpu: &Gpu, selection: Option<&Arc<Selection>>) -> Self {
        let (size, alpha) = match selection {
            Some(selection) => (selection.bounds().size(), selection.alpha()),
            None => ((1, 1), &[u8::MAX][..]),
        };
        let texture = gpu.device.create_texture_with_data(
            &gpu.queue,
            &texture_descriptor(
                "selection",
                size,
                wgpu::TextureFormat::R8Unorm,
                wgpu::TextureUsages::TEXTURE_BINDING,
            ),
            wgpu::util::TextureDataOrder::LayerMajor,
            alpha,
        );
        Self {
            selection: selection.cloned(),
            view: texture.create_view(&Default::default()),
        }
    }

    pub fn shows(&self, selection: Option<&Arc<Selection>>) -> bool {
        match (&self.selection, selection) {
            (Some(shown), Some(selection)) => Arc::ptr_eq(shown, selection),
            (shown, selection) => shown.is_none() && selection.is_none(),
        }
    }

    pub fn selection(&self) -> Option<&Arc<Selection>> {
        self.selection.as_ref()
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn origin(&self) -> [i32; 2] {
        self.selection.as_ref().map_or([0; 2], |selection| {
            let PixelRect { x, y, .. } = selection.bounds();
            [x as i32, y as i32]
        })
    }
}

pub(crate) enum SelectionSource {
    Alpha,
    Wand {
        seed: (u32, u32),
        tolerance: f32,
        contiguous: bool,
    },
}

pub struct SelectionJob {
    pub(crate) gpu: Rc<Gpu>,
    pub(crate) revision: u64,
    pub(crate) size: (u32, u32),
    pub(crate) mode: SelectionMode,
    pub(crate) source: SelectionSource,
    pub(crate) readback: Readback,
}

pub struct Selected {
    pub(crate) gpu: Rc<Gpu>,
    pub(crate) revision: u64,
    pub(crate) mode: SelectionMode,
    pub(crate) selection: Option<Selection>,
}

impl SelectionJob {
    pub async fn prepare(self) -> Result<Selected, EngineError> {
        let Self {
            gpu,
            revision,
            size,
            mode,
            source,
            readback,
        } = self;
        let data = readback.pixels().await?;
        let (pixels, _) = data.as_chunks::<4>();
        let pixel = |x: u32, y: u32| pixels[y as usize * size.0 as usize + x as usize];
        let bounds = PixelRect::full(size);
        let selection = match source {
            SelectionSource::Alpha => Selection::from_fn(bounds, |x, y| pixel(x, y)[3]),
            SelectionSource::Wand {
                seed,
                tolerance,
                contiguous,
            } => {
                let reference = pixel(seed.0, seed.1);
                let tolerance = (tolerance * 255.0).round() as u8;
                let matched = region(size, seed, contiguous, |x, y| {
                    pixel(x, y)
                        .into_iter()
                        .zip(reference)
                        .all(|(channel, reference)| channel.abs_diff(reference) <= tolerance)
                });
                Selection::from_fn(
                    bounds,
                    |x, y| {
                        if matched.contains(x, y) { u8::MAX } else { 0 }
                    },
                )
            }
        };
        Ok(Selected {
            gpu,
            revision,
            mode,
            selection,
        })
    }
}
