use futures_channel::mpsc::UnboundedReceiver;
use nijimi_core::{
    BlendMode, Brush, Command, DocumentError, Event, LayerId, Point, ProjectError, StrokePoint,
    TileCoord, Viewport,
};
use nijimi_render::{Engine, EngineError, wgpu};
use pollster::block_on;

const SIZE: (u32, u32) = (600, 300);
const FIRST_LAYER: LayerId = 1;

struct Editor {
    engine: Engine,
    events: UnboundedReceiver<Event>,
}

impl Editor {
    fn new(size: (u32, u32)) -> Self {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = block_on(instance.request_adapter(&Default::default()))
            .expect("tests need a GPU adapter");
        let (device, queue) =
            block_on(adapter.request_device(&Default::default())).expect("device request failed");
        let (engine, events) =
            Engine::new(device, queue, wgpu::TextureFormat::Rgba8Unorm, size).expect("valid size");
        Self { engine, events }
    }

    fn dispatch(&mut self, command: Command) -> Option<LayerId> {
        self.engine.dispatch(command).expect("command")
    }

    fn add_layer(&mut self) -> LayerId {
        self.dispatch(Command::AddLayer {
            name: None,
            parent: None,
            index: Some(usize::MAX),
        })
        .expect("layer id")
    }

    fn paint(&mut self, layer: LayerId, brush: Brush, points: &[(f32, f32)]) {
        self.dispatch(Command::PaintStroke {
            layer,
            brush,
            points: points.iter().map(|(x, y)| at(*x, *y)).collect(),
        });
    }

    fn fill(&mut self, layer: LayerId, [red, green, blue, alpha]: [f32; 4]) {
        let brush = Brush {
            diameter: 4000.0,
            opacity: alpha,
            color: [red, green, blue],
            ..hard_brush()
        };
        self.paint(layer, brush, &[(300.0, 150.0)]);
    }

    fn pixels(&mut self) -> Vec<u8> {
        let readback = self.engine.read_rgba().expect("export");
        block_on(readback.pixels()).expect("readback")
    }

    fn pixel(&mut self, x: u32, y: u32) -> [u8; 4] {
        pixel_in(&self.pixels(), self.engine.document().size().0, x, y)
    }

    fn present(&mut self, size: (u32, u32), viewport: &Viewport) -> Vec<u8> {
        let target = self
            .engine
            .device()
            .create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
        self.engine
            .present(&target.create_view(&Default::default()), viewport)
            .expect("present");
        block_on(self.engine.read_texture(&target).pixels()).expect("readback")
    }

    fn allocated_tiles(&self, layer: LayerId) -> Vec<(u32, u32)> {
        let layer = self.engine.document().layer(layer).expect("layer");
        let tiles = layer.tiles().expect("raster layer");
        tiles.keys().copied().map(Into::into).collect()
    }

    fn save(&self) -> Vec<u8> {
        let snapshot = self.engine.save_project().expect("snapshot");
        block_on(snapshot.encode()).expect("save")
    }

    fn load(&mut self, file: Vec<u8>) -> Result<(), EngineError> {
        let project = block_on(self.engine.load_project(file)?.load())?;
        self.engine.open(project);
        Ok(())
    }

    fn take_events(&mut self) -> Vec<Event> {
        std::iter::from_fn(|| self.events.try_recv().ok()).collect()
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        let errors: Vec<_> = self
            .take_events()
            .into_iter()
            .filter(|event| matches!(event, Event::GpuError { .. }))
            .collect();
        if !std::thread::panicking() {
            assert!(errors.is_empty(), "{errors:?}");
        }
    }
}

fn pixel_in(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let start = ((y * width + x) * 4) as usize;
    rgba[start..start + 4].try_into().expect("four channels")
}

fn at(x: f32, y: f32) -> StrokePoint {
    StrokePoint {
        position: Point::new(x, y),
        pressure: 1.0,
    }
}

fn hard_brush() -> Brush {
    Brush {
        diameter: 20.0,
        hardness: 1.0,
        flow: 1.0,
        opacity: 1.0,
        color: [1.0, 0.0, 0.0],
    }
}

fn test_image((width, height): (u32, u32)) -> Vec<u8> {
    (0..height)
        .flat_map(|y| (0..width).flat_map(move |x| [x as u8, y as u8, (x / 3 + y) as u8, 255]))
        .collect()
}

fn assert_same(actual: &[u8], expected: &[u8]) {
    assert_near(actual, expected, 0);
}

fn assert_near(actual: &[u8], expected: &[u8], tolerance: u8) {
    assert_eq!(actual.len(), expected.len());
    for (index, (a, b)) in actual.iter().zip(expected).enumerate() {
        assert!(a.abs_diff(*b) <= tolerance, "byte {index}: {a} != {b}");
    }
}

#[test]
fn imported_pixels_survive_a_round_trip() {
    let mut editor = Editor::new((10, 10));
    for size in [(70, 50), (600, 300), (256, 512), (257, 1)] {
        let image = test_image(size);
        editor
            .engine
            .load_rgba(size.0, size.1, &image)
            .expect("load");
        assert_same(&editor.pixels(), &image);
    }
}

#[test]
fn translucent_color_survives_a_round_trip() {
    let mut editor = Editor::new(SIZE);
    let image = [200, 100, 50, 128].repeat((SIZE.0 * SIZE.1) as usize);
    editor
        .engine
        .load_rgba(SIZE.0, SIZE.1, &image)
        .expect("load");
    assert_near(&editor.pixels(), &image, 1);
}

#[test]
fn empty_tiles_are_not_allocated() {
    let mut editor = Editor::new(SIZE);
    assert!(editor.allocated_tiles(FIRST_LAYER).is_empty());
    editor.paint(FIRST_LAYER, hard_brush(), &[(300.0, 280.0)]);
    assert_eq!(editor.allocated_tiles(FIRST_LAYER), [(1, 1)]);

    let mut image = vec![0; (SIZE.0 * SIZE.1 * 4) as usize];
    image[3] = 255;
    editor
        .engine
        .load_rgba(SIZE.0, SIZE.1, &image)
        .expect("load");
    assert_eq!(editor.allocated_tiles(FIRST_LAYER), [(0, 0)]);
    assert_same(&editor.pixels(), &image);
}

#[test]
fn a_hard_dab_lands_where_it_was_placed() {
    let mut editor = Editor::new(SIZE);
    editor.paint(FIRST_LAYER, hard_brush(), &[(20.5, 15.5)]);
    let rgba = editor.pixels();
    assert_eq!(pixel_in(&rgba, SIZE.0, 20, 15), [255, 0, 0, 255]);
    assert_eq!(pixel_in(&rgba, SIZE.0, 28, 15), [255, 0, 0, 255]);
    assert_eq!(pixel_in(&rgba, SIZE.0, 20, 34)[3], 0);
    assert_eq!(pixel_in(&rgba, SIZE.0, 32, 15)[3], 0);
}

#[test]
fn a_soft_dab_fades_towards_its_edge() {
    let mut editor = Editor::new(SIZE);
    let brush = Brush {
        hardness: 0.0,
        ..hard_brush()
    };
    editor.paint(FIRST_LAYER, brush, &[(20.5, 15.5)]);
    let rgba = editor.pixels();
    let alphas: Vec<_> = (20..=30)
        .map(|x| pixel_in(&rgba, SIZE.0, x, 15)[3])
        .collect();
    assert_eq!(alphas[0], 255);
    assert!(
        alphas.windows(2).all(|pair| pair[0] > pair[1]),
        "{alphas:?}"
    );
}

#[test]
fn painting_is_the_same_across_tile_borders() {
    let brush = Brush {
        diameter: 60.0,
        hardness: 0.2,
        flow: 0.4,
        opacity: 0.8,
        color: [0.2, 0.6, 0.9],
    };
    let stroke = [(0.25, 0.5), (30.0, 20.0), (10.0, 35.0)];
    let mut crops = Vec::new();
    for (left, top) in [(100, 100), (240, 230)] {
        let mut editor = Editor::new(SIZE);
        let points = stroke.map(|(x, y)| (x + left as f32, y + top as f32));
        editor.paint(FIRST_LAYER, brush, &points);
        let rgba = editor.pixels();
        let crop: Vec<_> = (top - 32..top + 68)
            .flat_map(|y| (left - 32..left + 68).map(move |x| (x, y)))
            .flat_map(|(x, y)| pixel_in(&rgba, SIZE.0, x, y))
            .collect();
        crops.push(crop);
    }
    assert!(crops[0].iter().any(|byte| *byte > 0));
    assert_near(&crops[1], &crops[0], 2);
}

#[test]
fn painting_stays_inside_the_document() {
    let mut editor = Editor::new((300, 100));
    for x in [-50.0, -9.0, 150.0, 299.0, 400.0] {
        editor.paint(FIRST_LAYER, hard_brush(), &[(x, 50.0), (x, 110.0)]);
    }
    assert_eq!(editor.allocated_tiles(FIRST_LAYER), [(0, 0), (1, 0)]);
    assert_eq!(editor.pixel(299, 50), [255, 0, 0, 255]);
    editor.paint(FIRST_LAYER, hard_brush(), &[(1000.0, 1000.0)]);
    assert_eq!(editor.allocated_tiles(FIRST_LAYER).len(), 2);
}

#[test]
fn stroke_opacity_does_not_build_up_where_a_stroke_overlaps_itself() {
    let mut editor = Editor::new(SIZE);
    let brush = Brush {
        opacity: 0.5,
        ..hard_brush()
    };
    let scribble = [(230.0, 50.0), (290.0, 50.0), (230.0, 50.0), (290.0, 50.0)];
    editor.paint(FIRST_LAYER, brush, &scribble);
    assert_near(&editor.pixel(260, 50), &[255, 0, 0, 128], 1);
    editor.paint(FIRST_LAYER, brush, &scribble);
    assert_near(&editor.pixel(260, 50), &[255, 0, 0, 191], 1);
}

#[test]
fn flow_builds_up_within_a_stroke_up_to_its_opacity() {
    let alpha_after = |points: &[(f32, f32)]| {
        let mut editor = Editor::new(SIZE);
        let brush = Brush {
            flow: 0.1,
            opacity: 0.6,
            ..hard_brush()
        };
        editor.paint(FIRST_LAYER, brush, points);
        editor.pixel(50, 50)[3]
    };
    let dot = alpha_after(&[(50.0, 50.0)]);
    let line = alpha_after(&[(50.0, 50.0), (58.0, 50.0)]);
    let scribble = alpha_after(&[(50.0, 50.0), (58.0, 50.0)].repeat(40));
    assert_near(&[dot], &[15], 1);
    assert!(dot < line && line < scribble, "{dot} {line} {scribble}");
    assert_near(&[scribble], &[153], 1);
}

#[test]
fn a_stroke_in_progress_is_shown_and_can_be_cancelled() {
    let mut editor = Editor::new(SIZE);
    editor.fill(FIRST_LAYER, [0.0, 0.0, 1.0, 1.0]);
    let before = editor.pixels();
    editor
        .engine
        .begin_stroke(hard_brush(), at(250.0, 100.0))
        .expect("stroke");
    assert_eq!(editor.pixel(250, 100), [255, 0, 0, 255]);
    editor.engine.extend_stroke(at(270.0, 100.0));
    assert_eq!(editor.pixel(270, 100), [255, 0, 0, 255]);
    editor.engine.cancel_stroke();
    assert_same(&editor.pixels(), &before);

    editor
        .engine
        .begin_stroke(hard_brush(), at(250.0, 100.0))
        .expect("stroke");
    editor.engine.extend_stroke(at(270.0, 100.0));
    editor.engine.end_stroke().expect("end");
    assert_eq!(editor.pixel(270, 100), [255, 0, 0, 255]);
    editor.dispatch(Command::Undo);
    assert_same(&editor.pixels(), &before);
}

#[test]
fn a_command_finishes_the_stroke_in_progress() {
    let mut editor = Editor::new(SIZE);
    editor
        .engine
        .begin_stroke(hard_brush(), at(50.0, 50.0))
        .expect("stroke");
    editor.dispatch(Command::Undo);
    assert_eq!(editor.pixel(50, 50), [0; 4]);
    editor.dispatch(Command::Redo);
    assert_eq!(editor.pixel(50, 50), [255, 0, 0, 255]);
}

fn soft_light(bottom: f32, top: f32) -> f32 {
    if top <= 0.5 {
        return bottom - (1.0 - 2.0 * top) * bottom * (1.0 - bottom);
    }
    let curve = if bottom <= 0.25 {
        ((16.0 * bottom - 12.0) * bottom + 4.0) * bottom
    } else {
        bottom.sqrt()
    };
    bottom + (2.0 * top - 1.0) * (curve - bottom)
}

fn blend_channel(mode: BlendMode, bottom: f32, top: f32) -> f32 {
    match mode {
        BlendMode::Normal => top,
        BlendMode::Multiply => bottom * top,
        BlendMode::Screen => 1.0 - (1.0 - bottom) * (1.0 - top),
        BlendMode::Overlay if bottom <= 0.5 => 2.0 * bottom * top,
        BlendMode::Overlay => 1.0 - 2.0 * (1.0 - bottom) * (1.0 - top),
        BlendMode::SoftLight => soft_light(bottom, top),
        BlendMode::Add => (bottom + top).min(1.0),
        BlendMode::Darken => bottom.min(top),
        BlendMode::Lighten => bottom.max(top),
    }
}

fn composite(mode: BlendMode, bottom: [f32; 4], top: [f32; 4]) -> [f32; 4] {
    let [.., bottom_alpha] = bottom;
    let [.., top_alpha] = top;
    let alpha = top_alpha + bottom_alpha * (1.0 - top_alpha);
    let channel = |index: usize| {
        let mixed = blend_channel(mode, bottom[index], top[index]);
        let source = (1.0 - bottom_alpha) * top[index] + bottom_alpha * mixed;
        (top_alpha * source + (1.0 - top_alpha) * bottom_alpha * bottom[index]) / alpha
    };
    [channel(0), channel(1), channel(2), alpha]
}

fn to_bytes(color: [f32; 4]) -> [u8; 4] {
    color.map(|channel| (channel * 255.0).round() as u8)
}

#[test]
fn blend_modes_match_their_reference_formulas() {
    let colors = [
        [0.8, 0.3, 0.1, 1.0],
        [0.1, 0.6, 0.9, 1.0],
        [0.45, 0.2, 0.7, 0.6],
        [0.9, 0.95, 0.05, 0.4],
    ];
    let mut editor = Editor::new((40, 40));
    for mode in BlendMode::ALL {
        for bottom in colors {
            for top in colors {
                editor
                    .engine
                    .load_rgba(40, 40, &[0; 40 * 40 * 4])
                    .expect("load");
                editor.fill(FIRST_LAYER, bottom);
                let layer = editor.add_layer();
                editor.fill(layer, top);
                editor.dispatch(Command::SetLayerBlendMode {
                    id: layer,
                    blend_mode: mode,
                });
                let expected = to_bytes(composite(mode, bottom, top));
                assert_near(&editor.pixel(20, 20), &expected, 1);
            }
        }
    }
}

#[test]
fn many_layers_composite_in_order() {
    let mut editor = Editor::new(SIZE);
    let mut expected = [0.2, 0.4, 0.6, 0.9];
    editor.fill(FIRST_LAYER, expected);
    for index in 0..20 {
        let color = [index as f32 / 20.0, 0.9 - index as f32 / 30.0, 0.3, 0.35];
        let mode = BlendMode::ALL[index % BlendMode::ALL.len()];
        let layer = editor.add_layer();
        editor.fill(layer, color);
        editor.dispatch(Command::SetLayerBlendMode {
            id: layer,
            blend_mode: mode,
        });
        expected = composite(mode, expected, color);
    }
    assert_near(&editor.pixel(100, 100), &to_bytes(expected), 2);
    assert_near(&editor.pixel(599, 299), &to_bytes(expected), 2);
}

#[test]
fn layer_opacity_and_visibility_apply() {
    let mut editor = Editor::new(SIZE);
    editor.fill(FIRST_LAYER, [0.0, 0.0, 1.0, 1.0]);
    let layer = editor.add_layer();
    editor.fill(layer, [1.0, 0.0, 0.0, 1.0]);
    editor.dispatch(Command::SetLayerOpacity {
        id: layer,
        opacity: 0.25,
    });
    assert_near(&editor.pixel(300, 10), &[64, 0, 191, 255], 1);
    editor.dispatch(Command::SetLayerVisible {
        id: layer,
        visible: false,
    });
    assert_eq!(editor.pixel(300, 10), [0, 0, 255, 255]);
}

#[test]
fn groups_composite_in_isolation() {
    let mut editor = Editor::new(SIZE);
    let base = [0.9, 0.5, 0.2, 1.0];
    editor.fill(FIRST_LAYER, base);
    let group = editor
        .dispatch(Command::AddGroup {
            name: None,
            parent: None,
            index: None,
        })
        .expect("group");
    let inner = editor
        .dispatch(Command::AddGroup {
            name: None,
            parent: Some(Some(group)),
            index: None,
        })
        .expect("group");
    let mut expected = [0.0, 0.0, 0.0, 0.0];
    for index in 0..10 {
        let color = [0.1 * index as f32, 0.5, 0.8, 0.5];
        let layer = editor
            .dispatch(Command::AddLayer {
                name: None,
                parent: Some(Some(inner)),
                index: None,
            })
            .expect("layer");
        editor.fill(layer, color);
        expected = composite(BlendMode::Normal, expected, color);
    }
    editor.dispatch(Command::SetLayerOpacity {
        id: group,
        opacity: 0.5,
    });
    editor.dispatch(Command::SetLayerBlendMode {
        id: group,
        blend_mode: BlendMode::Multiply,
    });
    expected[3] *= 0.5;
    let expected = composite(BlendMode::Multiply, base, expected);
    assert_near(&editor.pixel(300, 280), &to_bytes(expected), 2);
}

fn assert_pixels_undoable(name: &str, change: impl FnOnce(&mut Editor, LayerId, LayerId)) {
    let mut editor = Editor::new(SIZE);
    editor.fill(FIRST_LAYER, [0.9, 0.8, 0.1, 0.7]);
    let group = editor
        .dispatch(Command::AddGroup {
            name: None,
            parent: None,
            index: None,
        })
        .expect("group");
    let child = editor
        .dispatch(Command::AddLayer {
            name: None,
            parent: Some(Some(group)),
            index: None,
        })
        .expect("layer");
    editor.paint(
        child,
        Brush {
            diameter: 90.0,
            color: [0.1, 0.5, 0.9],
            ..hard_brush()
        },
        &[(200.0, 200.0), (400.0, 280.0)],
    );
    let layers = editor.engine.document().layers().to_vec();
    let active = editor.engine.document().active();
    let before = editor.pixels();

    change(&mut editor, group, child);
    let changed_layers = editor.engine.document().layers().to_vec();
    let after = editor.pixels();

    editor.dispatch(Command::Undo);
    assert!(editor.pixels() == before, "{name}: undo changed pixels");
    let document = editor.engine.document();
    assert_eq!(document.active(), active, "{name}: undo changed selection");
    assert!(same_layers(document.layers(), &layers), "{name}: undo");

    editor.dispatch(Command::Redo);
    assert!(editor.pixels() == after, "{name}: redo changed pixels");
    let document = editor.engine.document();
    assert!(same_layers(document.layers(), &changed_layers), "{name}");
}

fn same_layers(
    a: &[nijimi_core::Layer<nijimi_render::TileRef>],
    b: &[nijimi_core::Layer<nijimi_render::TileRef>],
) -> bool {
    let identity = |layers: &[nijimi_core::Layer<nijimi_render::TileRef>]| {
        layers
            .iter()
            .map(|layer| layer.map_tiles(&mut |_, _, tile| std::rc::Rc::as_ptr(tile)))
            .collect::<Vec<_>>()
    };
    identity(a) == identity(b)
}

#[test]
fn every_command_round_trips_through_undo_and_redo() {
    assert_pixels_undoable("add layer", |editor, _, _| {
        editor.add_layer();
    });
    assert_pixels_undoable("add group", |editor, group, _| {
        editor.dispatch(Command::AddGroup {
            name: Some("New".into()),
            parent: Some(Some(group)),
            index: Some(0),
        });
    });
    assert_pixels_undoable("delete layer", |editor, _, _| {
        editor.dispatch(Command::DeleteLayer { id: FIRST_LAYER });
    });
    assert_pixels_undoable("delete group", |editor, group, _| {
        editor.dispatch(Command::DeleteLayer { id: group });
    });
    assert_pixels_undoable("move layer", |editor, _, child| {
        editor.dispatch(Command::MoveLayer {
            id: child,
            parent: None,
            index: 0,
        });
    });
    assert_pixels_undoable("rename layer", |editor, _, child| {
        editor.dispatch(Command::RenameLayer {
            id: child,
            name: "Renamed".into(),
        });
    });
    assert_pixels_undoable("set visible", |editor, group, _| {
        editor.dispatch(Command::SetLayerVisible {
            id: group,
            visible: false,
        });
    });
    assert_pixels_undoable("set opacity", |editor, _, child| {
        editor.dispatch(Command::SetLayerOpacity {
            id: child,
            opacity: 0.3,
        });
    });
    assert_pixels_undoable("set blend mode", |editor, group, _| {
        editor.dispatch(Command::SetLayerBlendMode {
            id: group,
            blend_mode: BlendMode::Overlay,
        });
    });
    assert_pixels_undoable("paint stroke", |editor, _, child| {
        editor.paint(child, hard_brush(), &[(20.0, 20.0), (580.0, 290.0)]);
    });
    assert_pixels_undoable("select layer and paint", |editor, _, child| {
        editor.paint(child, hard_brush(), &[(20.0, 20.0)]);
        editor.dispatch(Command::SelectLayer { id: child });
    });
}

#[test]
fn undo_keeps_only_changed_tiles() {
    let mut editor = Editor::new((2048, 2048));
    editor.fill(FIRST_LAYER, [1.0, 1.0, 1.0, 1.0]);
    let before = editor.engine.document().history_bytes();
    editor.paint(FIRST_LAYER, hard_brush(), &[(100.0, 100.0)]);
    let added = editor.engine.document().history_bytes() - before;
    assert!(added < 2 * nijimi_core::TILE_BYTES as u64, "{added}");
}

#[test]
fn invalid_commands_are_rejected() {
    let mut editor = Editor::new(SIZE);
    let mut stroke = |brush, points: &[StrokePoint], layer| {
        editor.engine.dispatch(Command::PaintStroke {
            layer,
            brush,
            points: points.to_vec(),
        })
    };
    let invalid_brush = Brush {
        diameter: f32::INFINITY,
        ..hard_brush()
    };
    assert!(matches!(
        stroke(invalid_brush, &[at(0.0, 0.0)], FIRST_LAYER),
        Err(EngineError::InvalidBrush)
    ));
    for points in [&[][..], &[at(f32::NAN, 0.0)], &[at(0.0, 0.0), at(1e9, 0.0)]] {
        assert!(matches!(
            stroke(hard_brush(), points, FIRST_LAYER),
            Err(EngineError::InvalidStroke)
        ));
    }
    assert!(matches!(
        stroke(hard_brush(), &[at(0.0, 0.0)], 99),
        Err(EngineError::Document(DocumentError::LayerNotFound(99)))
    ));
    editor.dispatch(Command::DeleteLayer { id: FIRST_LAYER });
    assert!(matches!(
        editor.engine.begin_stroke(hard_brush(), at(0.0, 0.0)),
        Err(EngineError::NoActiveLayer)
    ));
    assert_eq!(editor.pixel(0, 0), [0; 4]);
}

#[test]
fn present_applies_the_viewport() {
    let size = (70, 50);
    let mut editor = Editor::new(size);
    editor.paint(FIRST_LAYER, hard_brush(), &[(20.5, 15.5)]);
    let mut viewport = Viewport::default();
    viewport.zoom_at(Point::new(20.5, 15.5), 4.0);
    let rgba = editor.present(size, &viewport);
    assert_eq!(pixel_in(&rgba, size.0, 20, 15), [255, 0, 0, 255]);
    assert_eq!(pixel_in(&rgba, size.0, 50, 15), [255, 0, 0, 255]);
    assert_ne!(pixel_in(&rgba, size.0, 65, 15), [255, 0, 0, 255]);
}

#[test]
fn zoomed_out_views_average_fine_detail() {
    let size = (1024, 1024);
    let mut editor = Editor::new(size);
    let checkerboard: Vec<u8> = (0..size.1)
        .flat_map(|y| (0..size.0).map(move |x| ((x ^ y) & 1) as u8 * 255))
        .flat_map(|value| [value, value, value, 255])
        .collect();
    editor
        .engine
        .load_rgba(size.0, size.1, &checkerboard)
        .expect("load");
    for screen in [512, 300, 128, 50] {
        let mut viewport = Viewport::default();
        viewport.fit(size, (screen, screen));
        let rgba = editor.present((screen, screen), &viewport);
        for pixel in rgba.chunks_exact(4) {
            assert_near(pixel, &[128, 128, 128, 255], 2);
        }
    }
}

#[test]
fn zoomed_out_views_have_no_seams_at_tile_borders() {
    let size = (1024, 512);
    let mut editor = Editor::new(size);
    let ramp: Vec<u8> = (0..size.1)
        .flat_map(|_| (0..size.0).map(|x| (x / 4) as u8))
        .flat_map(|value| [value, value, value, 255])
        .collect();
    editor
        .engine
        .load_rgba(size.0, size.1, &ramp)
        .expect("load");
    for (zoom_out, max_step) in [(1.3, 1), (2.0, 1), (3.7, 1), (8.0, 3)] {
        let screen = (
            (size.0 as f32 / zoom_out) as u32,
            (size.1 as f32 / zoom_out) as u32,
        );
        let mut viewport = Viewport::default();
        viewport.zoom_at(Point::new(0.0, 0.0), 1.0 / zoom_out);
        let rgba = editor.present(screen, &viewport);
        for row in rgba.chunks_exact(screen.0 as usize * 4) {
            for pair in row.as_chunks::<4>().0.windows(2) {
                let step = i32::from(pair[1][0]) - i32::from(pair[0][0]);
                assert!((0..=max_step).contains(&step), "{zoom_out}: {pair:?}");
            }
        }
    }
}

#[test]
fn partial_updates_match_a_full_redraw() {
    let size = (1001, 517);
    let mut editor = Editor::new(size);
    editor
        .engine
        .load_rgba(size.0, size.1, &test_image(size))
        .expect("load");
    let layer = editor.add_layer();
    let mut viewports = Vec::new();
    for screen in [(500, 258), (211, 109), (60, 31)] {
        let mut viewport = Viewport::default();
        viewport.fit(size, screen);
        viewports.push((screen, viewport));
    }
    editor.present(viewports[0].0, &viewports[0].1);
    editor.paint(layer, hard_brush(), &[(250.0, 250.0), (800.0, 516.0)]);
    editor.present(viewports[0].0, &viewports[0].1);
    editor.dispatch(Command::SetLayerOpacity {
        id: layer,
        opacity: 0.7,
    });
    let mut redrawn = Editor::new(size);
    redrawn.load(editor.save()).expect("load");
    for (screen, viewport) in viewports {
        assert_same(
            &editor.present(screen, &viewport),
            &redrawn.present(screen, &viewport),
        );
    }
}

fn sample_project() -> Editor {
    let mut editor = Editor::new((1100, 700));
    editor
        .engine
        .load_rgba(1100, 700, &test_image((1100, 700)))
        .expect("load");
    let group = editor
        .dispatch(Command::AddGroup {
            name: Some("Group ✿".into()),
            parent: None,
            index: None,
        })
        .expect("group");
    let layer = editor
        .dispatch(Command::AddLayer {
            name: None,
            parent: Some(Some(group)),
            index: None,
        })
        .expect("layer");
    let brush = Brush {
        diameter: 80.0,
        hardness: 0.1,
        flow: 0.3,
        opacity: 0.7,
        color: [0.3, 0.9, 0.5],
    };
    editor.paint(layer, brush, &[(10.0, 10.0), (900.0, 650.0)]);
    editor.dispatch(Command::SetLayerBlendMode {
        id: group,
        blend_mode: BlendMode::SoftLight,
    });
    editor.dispatch(Command::SetLayerOpacity {
        id: layer,
        opacity: 0.6,
    });
    editor
}

#[test]
fn a_saved_project_reopens_with_identical_pixels() {
    let mut original = sample_project();
    let file = original.save();
    let mut reopened = Editor::new((10, 10));
    reopened.load(file.clone()).expect("load");
    assert_eq!(reopened.take_events(), [Event::DocumentReplaced]);
    assert_same(&reopened.pixels(), &original.pixels());
    assert_eq!(
        reopened.engine.document().active(),
        original.engine.document().active()
    );
    assert_same(&reopened.save(), &file);
}

#[test]
fn a_save_captures_the_document_when_it_started() {
    let mut editor = sample_project();
    let expected = editor.save();
    let snapshot = editor.engine.save_project().expect("snapshot");
    editor.fill(FIRST_LAYER, [1.0, 0.0, 0.0, 1.0]);
    editor
        .engine
        .begin_stroke(hard_brush(), at(50.0, 50.0))
        .expect("stroke");
    editor.pixels();
    assert_same(&block_on(snapshot.encode()).expect("save"), &expected);
}

#[test]
fn a_failed_load_keeps_the_open_document() {
    let mut editor = sample_project();
    let before = editor.pixels();
    let layers = editor.engine.document().layers().to_vec();
    let file = editor.save();
    let mut truncated = file.clone();
    truncated.truncate(file.len() / 2);
    let mut damaged = file.clone();
    for byte in &mut damaged[file.len() / 4..file.len() / 4 + 4096] {
        *byte ^= 0x55;
    }
    for broken in [Vec::new(), truncated, damaged] {
        assert!(editor.load(broken).is_err());
    }
    let too_large = nijimi_core::ProjectWriter::default()
        .finish::<()>((1_000_000, 10), &[], None)
        .expect("project");
    assert!(matches!(
        editor.load(too_large),
        Err(EngineError::ImageTooLarge { .. })
    ));
    assert!(matches!(
        editor.load(b"not a project".to_vec()),
        Err(EngineError::Project(ProjectError::NotAProject))
    ));
    assert!(same_layers(editor.engine.document().layers(), &layers));
    assert_same(&editor.pixels(), &before);
    editor.dispatch(Command::Undo);
}

#[test]
fn invalid_images_are_rejected() {
    let mut editor = Editor::new((70, 50));
    let engine = &mut editor.engine;
    assert!(matches!(
        engine.load_rgba(0, 10, &[]),
        Err(EngineError::EmptyImage)
    ));
    assert!(matches!(
        engine.load_rgba(u32::MAX, 1, &[]),
        Err(EngineError::ImageTooLarge { .. })
    ));
    assert!(matches!(
        engine.load_rgba(2, 2, &[0; 15]),
        Err(EngineError::PixelCount {
            expected: 16,
            actual: 15
        })
    ));
    assert_eq!(engine.document().size(), (70, 50));
}

#[test]
fn events_are_published_in_order() {
    let mut editor = Editor::new(SIZE);
    editor.paint(FIRST_LAYER, hard_brush(), &[(50.0, 50.0)]);
    let layer = editor.add_layer();
    assert_eq!(
        editor.take_events(),
        [
            Event::PixelsChanged { id: FIRST_LAYER },
            Event::HistoryChanged {
                can_undo: true,
                can_redo: false
            },
            Event::LayerAdded { id: layer },
            Event::ActiveLayerChanged { id: Some(layer) },
        ]
    );
}

#[test]
fn a_lost_device_is_reported() {
    let mut editor = sample_project();
    let snapshot = editor.engine.save_project().expect("snapshot");
    editor.take_events();
    editor.engine.device().destroy();
    let _ = editor
        .engine
        .device()
        .poll(wgpu::PollType::wait_indefinitely());
    assert!(editor.engine.is_device_lost());
    assert!(matches!(
        editor.take_events()[..],
        [Event::DeviceLost { .. }]
    ));
    assert!(block_on(snapshot.encode()).is_err());
    assert!(matches!(
        editor.engine.dispatch(Command::Undo),
        Err(EngineError::DeviceLost)
    ));
    assert!(matches!(
        editor.engine.read_rgba(),
        Err(EngineError::DeviceLost)
    ));
    assert!(matches!(
        editor.engine.save_project().err(),
        Some(EngineError::DeviceLost)
    ));
}

#[test]
fn painting_leaves_other_tiles_alone() {
    let mut editor = Editor::new(SIZE);
    let image = test_image(SIZE);
    editor
        .engine
        .load_rgba(SIZE.0, SIZE.1, &image)
        .expect("load");
    let tile = |editor: &Editor| {
        let document = editor.engine.document();
        let tiles = document.layers()[0].tiles().expect("raster").clone();
        tiles[&TileCoord::new(2, 0)].clone()
    };
    let before = tile(&editor);
    editor.paint(FIRST_LAYER, hard_brush(), &[(50.0, 50.0)]);
    assert!(std::rc::Rc::ptr_eq(&before, &tile(&editor)));
    assert_eq!(editor.pixel(599, 10), pixel_in(&image, SIZE.0, 599, 10));
}

fn fraction(seed: f32) -> f32 {
    let value = (seed * 12.9898).sin() * 43758.547;
    value - value.floor()
}

#[test]
fn incremental_compositing_matches_a_reopened_document() {
    let mut editor = Editor::new((1500, 900));
    let mut layers = vec![FIRST_LAYER];
    layers.extend((1..12).map(|_| editor.add_layer()));
    for (index, layer) in layers.into_iter().enumerate() {
        editor.dispatch(Command::SetLayerBlendMode {
            id: layer,
            blend_mode: BlendMode::ALL[index % BlendMode::ALL.len()],
        });
        for stroke in 0..3 {
            let seed = (index * 10 + stroke) as f32;
            let brush = Brush {
                diameter: 300.0,
                hardness: 0.3,
                flow: 0.6,
                opacity: 0.7,
                color: [1.0, 2.0, 3.0].map(|offset| fraction(seed + offset)),
            };
            let point = |offset: f32| {
                let x = fraction(seed + offset) * 1500.0;
                (x, fraction(seed + offset + 0.5) * 900.0)
            };
            editor.paint(layer, brush, &[point(0.1), point(0.2)]);
        }
    }
    editor.dispatch(Command::Undo);
    let mut reopened = Editor::new((10, 10));
    reopened.load(editor.save()).expect("load");
    assert_same(&editor.pixels(), &reopened.pixels());
}
