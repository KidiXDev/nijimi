use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use futures_channel::mpsc::UnboundedReceiver;
use js_sys::{Function, Promise, Uint8Array};
use nijimi_core::{
    Brush, BrushDynamics, BrushPreset, CropRatio, Event, LayerId, PixelRect, Point, StrokePoint,
    Viewport,
};
use nijimi_render::{Engine, EngineError, Readback};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{future_to_promise, spawn_local};
use web_sys::{HtmlCanvasElement, console};
use wgpu::CurrentSurfaceTexture;

use crate::bridge::{EditorState, FillOptions, ImportOptions, from_js, to_js};

type Listeners = Rc<RefCell<Vec<(u32, Function)>>>;

#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
}

struct Session {
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    max_surface_size: u32,
    engine: Engine,
    viewport: Viewport,
    brush: Brush,
    dynamics: BrushDynamics,
    erase: bool,
}

#[wasm_bindgen]
pub struct Editor {
    session: Rc<RefCell<Session>>,
    listeners: Listeners,
    next_listener: u32,
    latest_load: Rc<Cell<u32>>,
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

        let (engine, events) = Engine::new(device, queue, surface_config.format, (width, height))?;
        let mut session = Session {
            surface,
            surface_config,
            max_surface_size,
            engine,
            viewport: Viewport::default(),
            brush: Brush::default(),
            dynamics: BrushDynamics::default(),
            erase: false,
        };
        session.fit();
        let listeners = Listeners::default();
        spawn_local(deliver_events(events, listeners.clone()));
        Ok(Editor {
            session: Rc::new(RefCell::new(session)),
            listeners,
            next_listener: 0,
            latest_load: Rc::default(),
        })
    }

    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.session.borrow().engine.document().size().0
    }

    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.session.borrow().engine.document().size().1
    }

    #[wasm_bindgen(getter)]
    pub fn zoom(&self) -> f32 {
        self.session.borrow().viewport.zoom()
    }

    #[wasm_bindgen(getter, js_name = isDeviceLost)]
    pub fn is_device_lost(&self) -> bool {
        self.session.borrow().engine.is_device_lost()
    }

    #[wasm_bindgen(unchecked_return_type = "EditorState")]
    pub fn state(&self) -> Result<JsValue, JsValue> {
        let session = self.session.borrow();
        to_js(&EditorState::new(
            session.engine.document(),
            session.engine.committed_document(),
        ))
    }

    pub fn subscribe(
        &mut self,
        #[wasm_bindgen(unchecked_param_type = "(event: EditorEvent) => void")] listener: Function,
    ) -> u32 {
        let id = self.next_listener;
        self.next_listener += 1;
        self.listeners.borrow_mut().push((id, listener));
        id
    }

    pub fn unsubscribe(&mut self, id: u32) {
        self.listeners
            .borrow_mut()
            .retain(|(listener_id, _)| *listener_id != id);
    }

    pub fn dispatch(
        &mut self,
        #[wasm_bindgen(unchecked_param_type = "Command")] command: JsValue,
    ) -> Result<Option<LayerId>, JsValue> {
        let command = from_js(&command)?;
        let mut session = self.session.borrow_mut();
        Ok(session.engine.dispatch(command).map_err(JsError::from)?)
    }

    pub fn preview(
        &mut self,
        #[wasm_bindgen(unchecked_param_type = "PreviewCommand")] command: JsValue,
    ) -> Result<(), JsValue> {
        self.session
            .borrow_mut()
            .engine
            .preview(from_js(&command)?)
            .map_err(|error| JsError::from(error).into())
    }

    #[wasm_bindgen(js_name = commitPreview)]
    pub fn commit_preview(&mut self) -> Result<(), JsError> {
        Ok(self.session.borrow_mut().engine.commit_preview()?)
    }

    #[wasm_bindgen(js_name = cancelPreview)]
    pub fn cancel_preview(&mut self) {
        self.session.borrow_mut().engine.cancel_preview();
    }

    #[wasm_bindgen(js_name = cropRect, unchecked_return_type = "PixelRect")]
    pub fn crop_rect(
        &self,
        #[wasm_bindgen(unchecked_param_type = "CropRatio")] preset: JsValue,
        #[wasm_bindgen(unchecked_optional_param_type = "PixelRect")] bounds: Option<JsValue>,
    ) -> Result<JsValue, JsValue> {
        let size = self.session.borrow().engine.committed_document().size();
        let bounds = optional_rect(bounds)?.unwrap_or_else(|| PixelRect::full(size));
        let preset: CropRatio = from_js(&preset)?;
        if !bounds.is_inside(size) {
            return Err(JsError::from(EngineError::InvalidRegion).into());
        }
        let rect = bounds
            .with_ratio(preset)
            .ok_or_else(|| JsError::new("The crop ratio cannot fit inside these bounds."))?;
        to_js(&rect)
    }

    #[wasm_bindgen(js_name = importRgba)]
    pub fn import_rgba(
        &mut self,
        width: u32,
        height: u32,
        rgba: &[u8],
        #[wasm_bindgen(unchecked_optional_param_type = "ImportOptions")] options: Option<JsValue>,
    ) -> Result<LayerId, JsValue> {
        let options: ImportOptions = options
            .as_ref()
            .map(from_js)
            .transpose()?
            .unwrap_or_default();
        let place = options.place();
        let mut session = self.session.borrow_mut();
        Ok(session
            .engine
            .import_rgba(
                (width, height),
                rgba,
                (options.x, options.y),
                options.name,
                place,
            )
            .map_err(JsError::from)?)
    }

    #[wasm_bindgen(js_name = setHistoryLimit)]
    pub fn set_history_limit(&mut self, bytes: f64) {
        self.session
            .borrow_mut()
            .engine
            .set_history_limit(bytes as u64);
    }

    #[wasm_bindgen(js_name = loadRgba)]
    pub fn load_rgba(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<(), JsError> {
        let mut session = self.session.borrow_mut();
        session.engine.load_rgba(width, height, rgba)?;
        session.fit();
        self.begin_load();
        Ok(())
    }

    #[wasm_bindgen(js_name = saveProject, unchecked_return_type = "Promise<Uint8Array>")]
    pub fn save_project(&self) -> Promise {
        let snapshot = self.session.borrow().engine.save_project();
        future_to_promise(async move {
            let snapshot = snapshot.map_err(JsError::from)?;
            let file = snapshot.encode().await.map_err(JsError::from)?;
            Ok(Uint8Array::from(file.as_slice()).into())
        })
    }

    #[wasm_bindgen(js_name = loadProject, unchecked_return_type = "Promise<void>")]
    pub fn load_project(&self, file: Vec<u8>) -> Promise {
        let loader = self.session.borrow().engine.load_project(file);
        let session = self.session.clone();
        let latest_load = self.latest_load.clone();
        let this_load = self.begin_load();
        future_to_promise(async move {
            let loader = loader.map_err(JsError::from)?;
            let project = loader.load().await.map_err(JsError::from)?;
            if latest_load.get() != this_load {
                return Err(JsError::new("Another document was opened first.").into());
            }
            let mut session = session.borrow_mut();
            session.engine.open(project);
            session.fit();
            Ok(JsValue::UNDEFINED)
        })
    }

    #[wasm_bindgen(js_name = exportRgba, unchecked_return_type = "Promise<Uint8Array>")]
    pub fn export_rgba(&mut self) -> Promise {
        export_pixels(self.session.borrow_mut().engine.read_rgba(), false)
    }

    #[wasm_bindgen(js_name = exportRegion, unchecked_return_type = "Promise<Uint8Array>")]
    pub fn export_region(
        &mut self,
        #[wasm_bindgen(unchecked_param_type = "PixelRect")] rect: JsValue,
    ) -> Promise {
        let readback = from_js(&rect).and_then(|rect| {
            self.session
                .borrow_mut()
                .engine
                .read_region(Some(rect))
                .map_err(|error| JsError::from(error).into())
        });
        export_pixels_js(readback, false)
    }

    #[wasm_bindgen(js_name = exportLayer, unchecked_return_type = "Promise<Uint8Array>")]
    pub fn export_layer(
        &mut self,
        id: LayerId,
        #[wasm_bindgen(unchecked_optional_param_type = "PixelRect")] rect: Option<JsValue>,
    ) -> Promise {
        self.export_layer_pixels(id, rect, false)
    }

    #[wasm_bindgen(js_name = exportLayerMask, unchecked_return_type = "Promise<Uint8Array>")]
    pub fn export_layer_mask(
        &mut self,
        id: LayerId,
        #[wasm_bindgen(unchecked_optional_param_type = "PixelRect")] rect: Option<JsValue>,
    ) -> Promise {
        self.export_layer_pixels(id, rect, true)
    }

    fn export_layer_pixels(&mut self, id: LayerId, rect: Option<JsValue>, alpha: bool) -> Promise {
        let readback = optional_rect(rect).and_then(|rect| {
            self.session
                .borrow_mut()
                .engine
                .read_layer(id, rect)
                .map_err(|error| JsError::from(error).into())
        });
        export_pixels_js(readback, alpha)
    }

    #[wasm_bindgen(js_name = pickColor, unchecked_return_type = "Promise<Uint8Array>")]
    pub fn pick_color(&mut self, x: u32, y: u32) -> Promise {
        let rect = PixelRect {
            x,
            y,
            width: 1,
            height: 1,
        };
        export_pixels(
            self.session.borrow_mut().engine.read_region(Some(rect)),
            false,
        )
    }

    #[wasm_bindgen(unchecked_return_type = "Promise<void>")]
    pub fn fill(
        &mut self,
        #[wasm_bindgen(unchecked_param_type = "FillOptions")] options: JsValue,
    ) -> Promise {
        let fill = from_js::<FillOptions>(&options).and_then(|options| {
            self.session
                .borrow_mut()
                .engine
                .fill(
                    options.id,
                    (options.x, options.y),
                    options.color,
                    options.tolerance,
                )
                .map_err(|error| JsError::from(error).into())
        });
        let session = Rc::downgrade(&self.session);
        future_to_promise(async move {
            let result = fill?.prepare().await.map_err(JsError::from)?;
            let session = session
                .upgrade()
                .ok_or_else(|| JsError::new("The editor was closed."))?;
            session
                .borrow_mut()
                .engine
                .commit_fill(result)
                .map_err(JsError::from)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    #[wasm_bindgen(js_name = documentPoint, unchecked_return_type = "Point")]
    pub fn document_point(&self, x: f32, y: f32) -> Result<JsValue, JsValue> {
        to_js(&self.session.borrow().viewport.to_document(Point::new(x, y)))
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let mut session = self.session.borrow_mut();
        let limit = session.max_surface_size;
        session.surface_config.width = width.clamp(1, limit);
        session.surface_config.height = height.clamp(1, limit);
        session.configure_surface();
    }

    pub fn fit(&mut self) {
        self.session.borrow_mut().fit();
    }

    #[wasm_bindgen(js_name = panBy)]
    pub fn pan_by(&mut self, dx: f32, dy: f32) {
        self.session.borrow_mut().viewport.pan_by(dx, dy);
    }

    #[wasm_bindgen(js_name = zoomAt)]
    pub fn zoom_at(&mut self, x: f32, y: f32, factor: f32) {
        self.session
            .borrow_mut()
            .viewport
            .zoom_at(Point::new(x, y), factor);
    }

    #[wasm_bindgen(js_name = setBrush)]
    pub fn set_brush(
        &mut self,
        #[wasm_bindgen(unchecked_param_type = "Brush")] brush: JsValue,
    ) -> Result<(), JsValue> {
        let brush: Brush = from_js(&brush)?;
        if !brush.is_valid() {
            return Err(JsError::from(EngineError::InvalidBrush).into());
        }
        self.session.borrow_mut().brush = brush;
        Ok(())
    }

    #[wasm_bindgen(js_name = brushPreset, unchecked_return_type = "Brush")]
    pub fn brush_preset(
        #[wasm_bindgen(unchecked_param_type = "BrushPreset")] preset: JsValue,
    ) -> Result<JsValue, JsValue> {
        to_js(&from_js::<BrushPreset>(&preset)?.brush())
    }

    #[wasm_bindgen(js_name = setBrushDynamics)]
    pub fn set_brush_dynamics(
        &mut self,
        #[wasm_bindgen(unchecked_param_type = "BrushDynamics")] dynamics: JsValue,
    ) -> Result<(), JsValue> {
        let dynamics: BrushDynamics = from_js(&dynamics)?;
        if !dynamics.is_valid() {
            return Err(JsError::from(EngineError::InvalidBrush).into());
        }
        self.session.borrow_mut().dynamics = dynamics;
        Ok(())
    }

    #[wasm_bindgen(js_name = setEraser)]
    pub fn set_eraser(&mut self, erase: bool) {
        self.session.borrow_mut().erase = erase;
    }

    #[wasm_bindgen(js_name = beginStroke)]
    pub fn begin_stroke(&mut self, x: f32, y: f32, pressure: f32) -> Result<(), JsError> {
        let mut session = self.session.borrow_mut();
        let (brush, dynamics, erase) = (session.brush, session.dynamics, session.erase);
        let point = session.stroke_point(x, y, pressure);
        Ok(session
            .engine
            .begin_brush_stroke(brush, dynamics, erase, point)?)
    }

    #[wasm_bindgen(js_name = extendStroke)]
    pub fn extend_stroke(&mut self, x: f32, y: f32, pressure: f32) {
        let mut session = self.session.borrow_mut();
        let point = session.stroke_point(x, y, pressure);
        session.engine.extend_stroke(point);
    }

    #[wasm_bindgen(js_name = endStroke)]
    pub fn end_stroke(&mut self) -> Result<(), JsError> {
        Ok(self.session.borrow_mut().engine.end_stroke()?)
    }

    #[wasm_bindgen(js_name = cancelStroke)]
    pub fn cancel_stroke(&mut self) {
        self.session.borrow_mut().engine.cancel_stroke();
    }

    pub fn render(&mut self) -> Result<(), JsError> {
        self.session.borrow_mut().render()
    }

    fn begin_load(&self) -> u32 {
        let load = self.latest_load.get().wrapping_add(1);
        self.latest_load.set(load);
        load
    }
}

impl Session {
    fn configure_surface(&self) {
        self.surface
            .configure(self.engine.device(), &self.surface_config);
    }

    fn fit(&mut self) {
        self.viewport.fit(
            self.engine.document().size(),
            (self.surface_config.width, self.surface_config.height),
        );
    }

    fn render(&mut self) -> Result<(), JsError> {
        let frame = match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) | CurrentSurfaceTexture::Suboptimal(frame) => {
                frame
            }
            CurrentSurfaceTexture::Outdated => {
                self.configure_surface();
                return Ok(());
            }
            CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => return Ok(()),
            CurrentSurfaceTexture::Lost | CurrentSurfaceTexture::Validation => {
                return Err(JsError::new("The canvas surface was lost."));
            }
        };
        let view = frame.texture.create_view(&Default::default());
        self.engine.present(&view, &self.viewport)?;
        self.engine.queue().present(frame);
        Ok(())
    }

    fn stroke_point(&self, x: f32, y: f32, pressure: f32) -> StrokePoint {
        StrokePoint {
            position: self.viewport.to_document(Point::new(x, y)),
            pressure,
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.engine.device().destroy();
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        self.listeners.borrow_mut().clear();
    }
}

fn optional_rect(rect: Option<JsValue>) -> Result<Option<PixelRect>, JsValue> {
    rect.as_ref().map(from_js).transpose()
}

fn export_pixels(readback: Result<Readback, EngineError>, alpha: bool) -> Promise {
    export_pixels_js(readback.map_err(|error| JsError::from(error).into()), alpha)
}

fn export_pixels_js(readback: Result<Readback, JsValue>, alpha: bool) -> Promise {
    future_to_promise(async move {
        let mut pixels = readback?.pixels().await.map_err(JsError::from)?;
        if alpha {
            for index in 0..pixels.len() / 4 {
                pixels[index] = pixels[index * 4 + 3];
            }
            pixels.truncate(pixels.len() / 4);
        }
        Ok(Uint8Array::from(pixels.as_slice()).into())
    })
}

async fn deliver_events(mut events: UnboundedReceiver<Event>, listeners: Listeners) {
    while let Ok(event) = events.recv().await {
        let callbacks: Vec<_> = listeners
            .borrow()
            .iter()
            .map(|(_, listener)| listener.clone())
            .collect();
        for callback in callbacks {
            let delivered =
                to_js(&event).and_then(|event| callback.call1(&JsValue::UNDEFINED, &event));
            if let Err(error) = delivered {
                console::error_1(&error);
            }
        }
    }
}
