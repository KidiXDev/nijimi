use std::{
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use futures_channel::mpsc::UnboundedSender;
use nijimi_core::Event;
use wgpu::ShaderStages;

use crate::{
    EngineError,
    composite::LAYERS_PER_PASS,
    grade::{LutCache, make_table},
    stroke::DabInstance,
    tile::{COVERAGE_FORMAT, TILE_FORMAT, Tile, TileRef},
};

pub(crate) const IMAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

pub(crate) struct Layouts {
    pub image: wgpu::BindGroupLayout,
    pub resample: wgpu::BindGroupLayout,
    pub export: wgpu::BindGroupLayout,
    pub stroke: wgpu::BindGroupLayout,
    pub apply_mask: wgpu::BindGroupLayout,
    pub grade: wgpu::BindGroupLayout,
    pub copy_tile: wgpu::BindGroupLayout,
    pub composite: wgpu::BindGroupLayout,
    pub mipmap: wgpu::BindGroupLayout,
    pub present: wgpu::BindGroupLayout,
}

pub(crate) struct Pipelines {
    pub import: wgpu::RenderPipeline,
    pub import_mask: wgpu::RenderPipeline,
    pub erase: wgpu::RenderPipeline,
    pub resample: wgpu::RenderPipeline,
    pub export: wgpu::RenderPipeline,
    pub dab: wgpu::RenderPipeline,
    pub stroke: wgpu::RenderPipeline,
    pub mask_stroke: wgpu::RenderPipeline,
    pub apply_mask: wgpu::RenderPipeline,
    pub grade: wgpu::RenderPipeline,
    pub copy_tile: wgpu::RenderPipeline,
    pub composite: wgpu::RenderPipeline,
    pub mipmap: wgpu::RenderPipeline,
    pub present: wgpu::RenderPipeline,
}

pub(crate) struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub layouts: Layouts,
    pub pipelines: Pipelines,
    pub sampler: wgpu::Sampler,
    pub empty_tile: TileRef,
    revealed_tile: TileRef,
    pub luts: LutCache,
    pub empty_grade_table: wgpu::Buffer,
    lost: Arc<AtomicBool>,
}

impl Gpu {
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface_format: wgpu::TextureFormat,
        events: &UnboundedSender<Event>,
    ) -> Self {
        let lost = Arc::new(AtomicBool::new(false));
        report_device_loss(&device, lost.clone(), events.clone());
        report_gpu_errors(&device, events.clone());

        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders.wgsl"));
        let layouts = create_layouts(&device);
        let pipelines = create_pipelines(&device, &shader, &layouts, surface_format);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let empty_tile = Rc::new(Tile::new(&device, TILE_FORMAT));
        let revealed_tile = Rc::new(Tile::new(&device, TILE_FORMAT));
        let mut encoder = device.create_command_encoder(&Default::default());
        drop(begin_pass(&mut encoder, revealed_tile.view(), REVEALED));
        queue.submit([encoder.finish()]);
        let empty_grade_table = make_table(&device, &[[0.0; 4]]);
        Self {
            device,
            queue,
            layouts,
            pipelines,
            sampler,
            empty_tile,
            revealed_tile,
            luts: LutCache::default(),
            empty_grade_table,
            lost,
        }
    }

    pub fn check_device(&self) -> Result<(), EngineError> {
        if self.lost.load(Ordering::Relaxed) {
            return Err(EngineError::DeviceLost);
        }
        Ok(())
    }

    pub fn check_size(&self, (width, height): (u32, u32)) -> Result<(), EngineError> {
        let limit = self.device.limits().max_texture_dimension_2d;
        if width == 0 || height == 0 {
            return Err(EngineError::EmptyImage);
        }
        if width > limit || height > limit {
            return Err(EngineError::ImageTooLarge { limit });
        }
        Ok(())
    }

    pub fn mask_default(&self, reveal: bool) -> &TileRef {
        if reveal {
            &self.revealed_tile
        } else {
            &self.empty_tile
        }
    }

    pub fn new_tile(&self) -> Tile {
        Tile::new(&self.device, TILE_FORMAT)
    }

    pub fn submit<T>(&self, record: impl FnOnce(&mut wgpu::CommandEncoder) -> T) -> T {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let result = record(&mut encoder);
        self.queue.submit([encoder.finish()]);
        result
    }

    pub fn bind(
        &self,
        layout: &wgpu::BindGroupLayout,
        resources: &[wgpu::BindingResource<'_>],
    ) -> wgpu::BindGroup {
        let entries: Vec<_> = (0..)
            .zip(resources)
            .map(|(binding, resource)| wgpu::BindGroupEntry {
                binding,
                resource: resource.clone(),
            })
            .collect();
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout,
            entries: &entries,
        })
    }

    pub async fn catch_errors<T>(&self, work: impl FnOnce() -> T) -> Result<T, EngineError> {
        let memory = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let result = work();
        let (validation, memory) = (validation.pop(), memory.pop());
        if let Some(error) = validation.await.or(memory.await) {
            return Err(error.into());
        }
        self.check_device()?;
        Ok(result)
    }
}

fn report_device_loss(
    device: &wgpu::Device,
    lost: Arc<AtomicBool>,
    events: UnboundedSender<Event>,
) {
    device.set_device_lost_callback(move |_, message| {
        lost.store(true, Ordering::Relaxed);
        let _ = events.unbounded_send(Event::DeviceLost { message });
    });
}

fn report_gpu_errors(device: &wgpu::Device, events: UnboundedSender<Event>) {
    device.on_uncaptured_error(Arc::new(move |error: wgpu::Error| {
        let _ = events.unbounded_send(Event::GpuError {
            message: error.to_string(),
        });
    }));
}

pub(crate) fn begin_pass<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    load: wgpu::LoadOp<wgpu::Color>,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load,
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    })
}

pub(crate) const REVEALED: wgpu::LoadOp<wgpu::Color> = wgpu::LoadOp::Clear(wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
});

pub(crate) const CLEAR: wgpu::LoadOp<wgpu::Color> = wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT);

pub(crate) fn draw_fullscreen(
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
    bindings: &wgpu::BindGroup,
) {
    let mut pass = begin_pass(encoder, target, CLEAR);
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bindings, &[]);
    pass.draw(0..3, 0..1);
}

pub(crate) fn texture_descriptor(
    label: &str,
    (width, height): (u32, u32),
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::TextureDescriptor<'_> {
    wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    }
}

fn create_layouts(device: &wgpu::Device) -> Layouts {
    let layout = |label, types: &[wgpu::BindingType]| {
        let entries: Vec<_> = (0..)
            .zip(types)
            .map(|(binding, ty)| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: ShaderStages::FRAGMENT,
                ty: *ty,
                count: None,
            })
            .collect();
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &entries,
        })
    };
    let texture = wgpu::BindingType::Texture {
        sample_type: wgpu::TextureSampleType::Float { filterable: true },
        view_dimension: wgpu::TextureViewDimension::D2,
        multisampled: false,
    };
    let sampler = wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering);
    let uniform = |has_dynamic_offset| wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Uniform,
        has_dynamic_offset,
        min_binding_size: None,
    };

    let mut composite = vec![texture; 1 + LAYERS_PER_PASS];
    composite.push(uniform(true));
    let mut grade = vec![texture; 11];
    grade.extend([
        uniform(false),
        wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
    ]);
    Layouts {
        grade: layout("grade", &grade),
        apply_mask: layout("apply mask", &[texture, texture]),
        copy_tile: layout("copy tile", &[texture, uniform(false)]),
        image: layout("image", &[texture, uniform(false)]),
        resample: layout("resample", &[texture, uniform(false)]),
        export: layout("export", &[texture, uniform(false)]),
        stroke: layout("stroke", &[texture, texture, uniform(false)]),
        composite: layout("composite", &composite),
        mipmap: layout("mipmap", &[texture, sampler]),
        present: layout("present", &[texture, sampler, uniform(false)]),
    }
}

fn create_pipelines(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layouts: &Layouts,
    surface_format: wgpu::TextureFormat,
) -> Pipelines {
    let fullscreen = |fragment, target, layout| PipelineSpec {
        vertex: "fullscreen_vertex",
        fragment,
        target,
        layout: Some(layout),
        buffers: &[],
        blend: None,
        topology: wgpu::PrimitiveTopology::TriangleList,
    };
    let build = |spec: PipelineSpec<'_>| spec.build(device, shader);
    let accumulate = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrc,
        operation: wgpu::BlendOperation::Add,
    };
    Pipelines {
        import: build(PipelineSpec {
            vertex: "import_vertex",
            ..fullscreen("import_fragment", TILE_FORMAT, &layouts.image)
        }),
        import_mask: build(PipelineSpec {
            vertex: "import_vertex",
            ..fullscreen("import_mask_fragment", TILE_FORMAT, &layouts.image)
        }),
        export: build(fullscreen("export_fragment", IMAGE_FORMAT, &layouts.export)),
        resample: build(fullscreen(
            "resample_fragment",
            TILE_FORMAT,
            &layouts.resample,
        )),
        erase: build(fullscreen("erase_fragment", TILE_FORMAT, &layouts.stroke)),
        dab: build(PipelineSpec {
            vertex: "dab_vertex",
            fragment: "dab_fragment",
            target: COVERAGE_FORMAT,
            layout: None,
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<DabInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &DabInstance::ATTRIBUTES,
            })],
            blend: Some(wgpu::BlendState {
                color: accumulate,
                alpha: accumulate,
            }),
            topology: wgpu::PrimitiveTopology::TriangleStrip,
        }),
        stroke: build(fullscreen("stroke_fragment", TILE_FORMAT, &layouts.stroke)),
        mask_stroke: build(fullscreen(
            "mask_stroke_fragment",
            TILE_FORMAT,
            &layouts.stroke,
        )),
        apply_mask: build(fullscreen(
            "apply_mask_fragment",
            TILE_FORMAT,
            &layouts.apply_mask,
        )),
        grade: build(fullscreen("grade_fragment", TILE_FORMAT, &layouts.grade)),
        copy_tile: build(fullscreen(
            "copy_tile_fragment",
            TILE_FORMAT,
            &layouts.copy_tile,
        )),
        composite: build(fullscreen(
            "composite_fragment",
            TILE_FORMAT,
            &layouts.composite,
        )),
        mipmap: build(fullscreen("mipmap_fragment", TILE_FORMAT, &layouts.mipmap)),
        present: build(fullscreen(
            "present_fragment",
            surface_format,
            &layouts.present,
        )),
    }
}

struct PipelineSpec<'a> {
    vertex: &'a str,
    fragment: &'a str,
    target: wgpu::TextureFormat,
    layout: Option<&'a wgpu::BindGroupLayout>,
    buffers: &'a [Option<wgpu::VertexBufferLayout<'a>>],
    blend: Option<wgpu::BlendState>,
    topology: wgpu::PrimitiveTopology,
}

impl PipelineSpec<'_> {
    fn build(self, device: &wgpu::Device, shader: &wgpu::ShaderModule) -> wgpu::RenderPipeline {
        let layouts: Vec<_> = self.layout.into_iter().map(Some).collect();
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(self.fragment),
            bind_group_layouts: &layouts,
            immediate_size: 0,
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(self.fragment),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some(self.vertex),
                compilation_options: Default::default(),
                buffers: self.buffers,
            },
            primitive: wgpu::PrimitiveState {
                topology: self.topology,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some(self.fragment),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: self.target,
                    blend: self.blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }
}
