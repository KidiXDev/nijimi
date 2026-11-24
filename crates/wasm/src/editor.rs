use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use futures_channel::mpsc::UnboundedReceiver;
use js_sys::{Function, Promise, Uint8Array};
use nijimi_core::{Brush, Event, LayerId, Point, StrokePoint, Viewport};
use nijimi_render::Engine;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{future_to_promise, spawn_local};
use web_sys::{HtmlCanvasElement, console};
use wgpu::CurrentSurfaceTexture;

use crate::bridge::{EditorState, from_js, to_js};

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
        to_js(&EditorState::new(self.session.borrow().engine.document()))
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
        let readback = self.session.borrow_mut().engine.read_rgba();
        future_to_promise(async move {
            let readback = readback.map_err(JsError::from)?;
            let pixels = readback.pixels().await.map_err(JsError::from)?;
            Ok(Uint8Array::from(pixels.as_slice()).into())
        })
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
        self.session.borrow_mut().brush = from_js(&brush)?;
        Ok(())
    }

    #[wasm_bindgen(js_name = beginStroke)]
    pub fn begin_stroke(&mut self, x: f32, y: f32, pressure: f32) -> Result<(), JsError> {
        let mut session = self.session.borrow_mut();
        let (brush, point) = (session.brush, session.stroke_point(x, y, pressure));
        Ok(session.engine.begin_stroke(brush, point)?)
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
