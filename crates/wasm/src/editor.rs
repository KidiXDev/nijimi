use js_sys::{Promise, Uint8Array};
use nijimi_core::{Brush, Dab, Point, Stroke, StrokePoint, Viewport};
use nijimi_render::Renderer;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;
use web_sys::HtmlCanvasElement;
use wgpu::CurrentSurfaceTexture;

#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
}

#[wasm_bindgen]
pub struct Editor {
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    max_surface_size: u32,
    renderer: Renderer,
    viewport: Viewport,
    brush: Brush,
    stroke: Option<Stroke>,
    pending_dabs: Vec<Dab>,
}

#[wasm_bindgen]
impl Editor {
    pub async fn create(
        canvas: HtmlCanvasElement,
        width: u32,
        height: u32,
    ) -> Result<Editor, JsError> {
        let (canvas_width, canvas_height) = (canvas.width().max(1), canvas.height().max(1));
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(wgpu::SurfaceTarget::Canvas(canvas))?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await?;

        let max_surface_size = device.limits().max_texture_dimension_2d;
        let mut surface_config = surface
            .get_default_config(
                &adapter,
                canvas_width.min(max_surface_size),
                canvas_height.min(max_surface_size),
            )
            .ok_or_else(|| JsError::new("The GPU adapter cannot draw to this canvas."))?;
        surface_config.format = surface_config.format.remove_srgb_suffix();
        surface.configure(&device, &surface_config);

        let renderer = Renderer::new(device, queue, surface_config.format, (width, height))?;
        let mut editor = Editor {
            surface,
            surface_config,
            max_surface_size,
            renderer,
            viewport: Viewport::default(),
            brush: Brush::default(),
            stroke: None,
            pending_dabs: Vec::new(),
        };
        editor.fit();
        Ok(editor)
    }

    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.renderer.document_size().0
    }

    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.renderer.document_size().1
    }

    #[wasm_bindgen(getter)]
    pub fn zoom(&self) -> f32 {
        self.viewport.zoom()
    }

    #[wasm_bindgen(js_name = loadRgba)]
    pub fn load_rgba(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<(), JsError> {
        self.renderer.load_rgba(width, height, rgba)?;
        self.stroke = None;
        self.pending_dabs.clear();
        self.fit();
        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.surface_config.width = width.clamp(1, self.max_surface_size);
        self.surface_config.height = height.clamp(1, self.max_surface_size);
        self.surface
            .configure(self.renderer.device(), &self.surface_config);
    }

    pub fn fit(&mut self) {
        self.viewport.fit(
            self.renderer.document_size(),
            (self.surface_config.width, self.surface_config.height),
        );
    }

    #[wasm_bindgen(js_name = panBy)]
    pub fn pan_by(&mut self, dx: f32, dy: f32) {
        self.viewport.pan_by(dx, dy);
    }

    #[wasm_bindgen(js_name = zoomAt)]
    pub fn zoom_at(&mut self, x: f32, y: f32, factor: f32) {
        self.viewport.zoom_at(Point::new(x, y), factor);
    }

    #[wasm_bindgen(js_name = setBrush)]
    pub fn set_brush(
        &mut self,
        diameter: f32,
        hardness: f32,
        flow: f32,
        red: f32,
        green: f32,
        blue: f32,
    ) {
        self.brush = Brush {
            diameter,
            hardness,
            flow,
            color: [red, green, blue],
        };
    }

    #[wasm_bindgen(js_name = beginStroke)]
    pub fn begin_stroke(&mut self, x: f32, y: f32, pressure: f32) {
        let (stroke, dab) = Stroke::begin(self.brush, self.stroke_point(x, y, pressure));
        self.stroke = Some(stroke);
        self.pending_dabs.push(dab);
    }

    #[wasm_bindgen(js_name = extendStroke)]
    pub fn extend_stroke(&mut self, x: f32, y: f32, pressure: f32) {
        let point = self.stroke_point(x, y, pressure);
        if let Some(stroke) = &mut self.stroke {
            stroke.extend(point, &mut self.pending_dabs);
        }
    }

    #[wasm_bindgen(js_name = endStroke)]
    pub fn end_stroke(&mut self) {
        self.stroke = None;
    }

    pub fn render(&mut self) -> Result<(), JsError> {
        self.flush_dabs();
        let frame = match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) | CurrentSurfaceTexture::Suboptimal(frame) => {
                frame
            }
            CurrentSurfaceTexture::Outdated => {
                self.surface
                    .configure(self.renderer.device(), &self.surface_config);
                return Ok(());
            }
            CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => return Ok(()),
            CurrentSurfaceTexture::Lost | CurrentSurfaceTexture::Validation => {
                return Err(JsError::new("The canvas surface was lost."));
            }
        };
        self.renderer.present(
            &frame.texture.create_view(&Default::default()),
            &self.viewport,
        );
        self.renderer.queue().present(frame);
        Ok(())
    }

    #[wasm_bindgen(js_name = exportRgba, unchecked_return_type = "Promise<Uint8Array>")]
    pub fn export_rgba(&mut self) -> Promise {
        self.flush_dabs();
        let readback = self.renderer.read_rgba();
        future_to_promise(async move {
            let pixels = readback.pixels().await.map_err(JsError::from)?;
            Ok(Uint8Array::from(pixels.as_slice()).into())
        })
    }

    fn flush_dabs(&mut self) {
        self.renderer.paint(&self.pending_dabs);
        self.pending_dabs.clear();
    }

    fn stroke_point(&self, x: f32, y: f32, pressure: f32) -> StrokePoint {
        StrokePoint {
            position: self.viewport.to_document(Point::new(x, y)),
            pressure,
        }
    }
}
