use std::{error::Error, fmt};

use bytemuck::{Pod, Zeroable};
use nijimi_core::{Dab, Viewport};
use wgpu::util::DeviceExt;

use crate::Readback;

const DOCUMENT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const IMAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

#[derive(Debug)]
pub enum RenderError {
    EmptyImage,
    ImageTooLarge { limit: u32 },
    PixelCount { expected: usize, actual: usize },
    Readback(String),
}

impl fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyImage => write!(formatter, "The image has no pixels."),
            Self::ImageTooLarge { limit } => {
                write!(formatter, "Images can be at most {limit} px per side.")
            }
            Self::PixelCount { expected, actual } => {
                write!(
                    formatter,
                    "Expected {expected} bytes of RGBA, got {actual}."
                )
            }
            Self::Readback(reason) => write!(formatter, "Could not read the image: {reason}"),
        }
    }
}

impl Error for RenderError {}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DabInstance {
    center: [f32; 2],
    half_size: [f32; 2],
    inner: f32,
    color: [f32; 4],
}

impl DabInstance {
    const ATTRIBUTES: [wgpu::VertexAttribute; 4] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32, 3 => Float32x4];

    fn new(dab: &Dab, size: wgpu::Extent3d) -> Self {
        let (width, height) = (size.width as f32, size.height as f32);
        let softest_edge = 1.0 - 1.0 / dab.radius.max(1.0);
        let alpha = dab.alpha.clamp(0.0, 1.0);
        let [red, green, blue] = dab.color.map(|channel| channel.clamp(0.0, 1.0) * alpha);
        Self {
            center: [
                dab.center.x / width * 2.0 - 1.0,
                1.0 - dab.center.y / height * 2.0,
            ],
            half_size: [dab.radius / width * 2.0, dab.radius / height * -2.0],
            inner: dab.hardness.clamp(0.0, 1.0).min(softest_edge),
            color: [red, green, blue, alpha],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ViewUniform {
    pan: [f32; 2],
    zoom: f32,
    padding: f32,
}

struct Document {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    present_bindings: wgpu::BindGroup,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    image_layout: wgpu::BindGroupLayout,
    present_layout: wgpu::BindGroupLayout,
    import_pipeline: wgpu::RenderPipeline,
    export_pipeline: wgpu::RenderPipeline,
    dab_pipeline: wgpu::RenderPipeline,
    present_pipeline: wgpu::RenderPipeline,
    view_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    document: Document,
}

impl Renderer {
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface_format: wgpu::TextureFormat,
        (width, height): (u32, u32),
    ) -> Result<Self, RenderError> {
        validate_size(&device, width, height)?;
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders.wgsl"));

        let image_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image"),
            entries: &[texture_entry()],
        });
        let present_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("present"),
            entries: &[
                texture_entry(),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let import_pipeline = PipelineSpec {
            vertex: "fullscreen_vertex",
            fragment: "premultiply_fragment",
            target: DOCUMENT_FORMAT,
            layout: Some(&image_layout),
            ..PipelineSpec::default()
        }
        .build(&device, &shader);
        let export_pipeline = PipelineSpec {
            vertex: "fullscreen_vertex",
            fragment: "unpremultiply_fragment",
            target: IMAGE_FORMAT,
            layout: Some(&image_layout),
            ..PipelineSpec::default()
        }
        .build(&device, &shader);
        let dab_pipeline = PipelineSpec {
            vertex: "dab_vertex",
            fragment: "dab_fragment",
            target: DOCUMENT_FORMAT,
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<DabInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &DabInstance::ATTRIBUTES,
            })],
            blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..PipelineSpec::default()
        }
        .build(&device, &shader);
        let present_pipeline = PipelineSpec {
            vertex: "fullscreen_vertex",
            fragment: "present_fragment",
            target: surface_format,
            layout: Some(&present_layout),
            ..PipelineSpec::default()
        }
        .build(&device, &shader);

        let view_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("view"),
            size: size_of::<ViewUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let document = create_document(
            &device,
            &present_layout,
            &view_buffer,
            &sampler,
            (width, height),
        );

        Ok(Self {
            device,
            queue,
            image_layout,
            present_layout,
            import_pipeline,
            export_pipeline,
            dab_pipeline,
            present_pipeline,
            view_buffer,
            sampler,
            document,
        })
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn document_size(&self) -> (u32, u32) {
        let size = self.document.texture.size();
        (size.width, size.height)
    }

    pub fn load_rgba(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<(), RenderError> {
        validate_size(&self.device, width, height)?;
        let expected = width as usize * height as usize * 4;
        if rgba.len() != expected {
            return Err(RenderError::PixelCount {
                expected,
                actual: rgba.len(),
            });
        }

        let image = self.device.create_texture_with_data(
            &self.queue,
            &texture_descriptor(
                "imported image",
                (width, height),
                IMAGE_FORMAT,
                wgpu::TextureUsages::TEXTURE_BINDING,
            ),
            wgpu::util::TextureDataOrder::LayerMajor,
            rgba,
        );
        self.document = create_document(
            &self.device,
            &self.present_layout,
            &self.view_buffer,
            &self.sampler,
            (width, height),
        );
        let bindings = self.image_bindings(&default_view(&image));
        self.submit(|encoder| {
            draw_fullscreen(
                encoder,
                &self.document.view,
                &self.import_pipeline,
                &bindings,
            );
        });
        Ok(())
    }

    pub fn paint(&self, dabs: &[Dab]) {
        if dabs.is_empty() {
            return;
        }
        let size = self.document.texture.size();
        let instances: Vec<_> = dabs.iter().map(|dab| DabInstance::new(dab, size)).collect();
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("dabs"),
                contents: bytemuck::cast_slice(&instances),
                usage: wgpu::BufferUsages::VERTEX,
            });
        self.submit(|encoder| {
            let mut pass = begin_pass(encoder, &self.document.view, wgpu::LoadOp::Load);
            pass.set_pipeline(&self.dab_pipeline);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..4, 0..instances.len() as u32);
        });
    }

    pub fn present(&self, target: &wgpu::TextureView, viewport: &Viewport) {
        let pan = viewport.pan();
        self.queue.write_buffer(
            &self.view_buffer,
            0,
            bytemuck::bytes_of(&ViewUniform {
                pan: [pan.x, pan.y],
                zoom: viewport.zoom(),
                padding: 0.0,
            }),
        );
        self.submit(|encoder| {
            draw_fullscreen(
                encoder,
                target,
                &self.present_pipeline,
                &self.document.present_bindings,
            );
        });
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn read_rgba(&self) -> Readback {
        let export = self.device.create_texture(&texture_descriptor(
            "export",
            self.document_size(),
            IMAGE_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        ));
        let bindings = self.image_bindings(&self.document.view);
        self.submit(|encoder| {
            draw_fullscreen(
                encoder,
                &default_view(&export),
                &self.export_pipeline,
                &bindings,
            );
            Readback::record(&self.device, encoder, &export)
        })
    }

    pub fn read_texture(&self, texture: &wgpu::Texture) -> Readback {
        self.submit(|encoder| Readback::record(&self.device, encoder, texture))
    }

    fn image_bindings(&self, view: &wgpu::TextureView) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image"),
            layout: &self.image_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            }],
        })
    }

    fn submit<T>(&self, record: impl FnOnce(&mut wgpu::CommandEncoder) -> T) -> T {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let result = record(&mut encoder);
        self.queue.submit([encoder.finish()]);
        result
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

impl Default for PipelineSpec<'_> {
    fn default() -> Self {
        Self {
            vertex: "",
            fragment: "",
            target: DOCUMENT_FORMAT,
            layout: None,
            buffers: &[],
            blend: None,
            topology: wgpu::PrimitiveTopology::TriangleList,
        }
    }
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

fn validate_size(device: &wgpu::Device, width: u32, height: u32) -> Result<(), RenderError> {
    let limit = device.limits().max_texture_dimension_2d;
    if width == 0 || height == 0 {
        return Err(RenderError::EmptyImage);
    }
    if width > limit || height > limit {
        return Err(RenderError::ImageTooLarge { limit });
    }
    Ok(())
}

fn create_document(
    device: &wgpu::Device,
    present_layout: &wgpu::BindGroupLayout,
    view_buffer: &wgpu::Buffer,
    sampler: &wgpu::Sampler,
    size: (u32, u32),
) -> Document {
    let texture = device.create_texture(&texture_descriptor(
        "document",
        size,
        DOCUMENT_FORMAT,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
    ));
    let view = default_view(&texture);
    let present_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("present"),
        layout: present_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: view_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    Document {
        texture,
        view,
        present_bindings,
    }
}

fn texture_descriptor(
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

fn texture_entry() -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn default_view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&Default::default())
}

fn begin_pass<'a>(
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

fn draw_fullscreen(
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
    bindings: &wgpu::BindGroup,
) {
    let mut pass = begin_pass(
        encoder,
        target,
        wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
    );
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bindings, &[]);
    pass.draw(0..3, 0..1);
}
