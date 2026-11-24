use nijimi_core::{Dab, Point, Viewport};
use nijimi_render::{RenderError, Renderer, wgpu};
use pollster::block_on;

const WIDTH: u32 = 70;
const HEIGHT: u32 = 50;

fn renderer(size: (u32, u32)) -> Renderer {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter =
        block_on(instance.request_adapter(&Default::default())).expect("tests need a GPU adapter");
    let (device, queue) =
        block_on(adapter.request_device(&Default::default())).expect("device request failed");
    Renderer::new(device, queue, wgpu::TextureFormat::Rgba8Unorm, size).expect("valid size")
}

fn pixels(renderer: &Renderer) -> Vec<u8> {
    block_on(renderer.read_rgba().pixels()).expect("readback failed")
}

fn pixel(rgba: &[u8], x: u32, y: u32) -> [u8; 4] {
    let start = ((y * WIDTH + x) * 4) as usize;
    rgba[start..start + 4].try_into().expect("four channels")
}

fn red_dab(hardness: f32, alpha: f32) -> Dab {
    Dab {
        center: Point::new(20.5, 15.5),
        radius: 10.0,
        hardness,
        color: [1.0, 0.0, 0.0],
        alpha,
    }
}

#[test]
fn imported_pixels_survive_a_round_trip() {
    let mut renderer = renderer((WIDTH, HEIGHT));
    let image: Vec<u8> = (0..WIDTH * HEIGHT)
        .flat_map(|i| [(i % 256) as u8, (i / 14) as u8, 200, 255])
        .collect();
    renderer.load_rgba(WIDTH, HEIGHT, &image).expect("load");
    assert_eq!(pixels(&renderer), image);
}

#[test]
fn translucent_color_survives_a_round_trip() {
    let mut renderer = renderer((WIDTH, HEIGHT));
    let image = [200, 100, 50, 128].repeat((WIDTH * HEIGHT) as usize);
    renderer.load_rgba(WIDTH, HEIGHT, &image).expect("load");
    for (actual, expected) in pixels(&renderer).iter().zip(&image) {
        assert!(actual.abs_diff(*expected) <= 1);
    }
}

#[test]
fn a_hard_dab_lands_where_it_was_placed() {
    let renderer = renderer((WIDTH, HEIGHT));
    renderer.paint(&[red_dab(1.0, 1.0)]);
    let rgba = pixels(&renderer);
    assert_eq!(pixel(&rgba, 20, 15), [255, 0, 0, 255]);
    assert_eq!(pixel(&rgba, 28, 15), [255, 0, 0, 255]);
    assert_eq!(pixel(&rgba, 20, 34)[3], 0);
    assert_eq!(pixel(&rgba, 32, 15)[3], 0);
}

#[test]
fn a_soft_dab_fades_towards_its_edge() {
    let renderer = renderer((WIDTH, HEIGHT));
    renderer.paint(&[red_dab(0.0, 1.0)]);
    let rgba = pixels(&renderer);
    let alphas: Vec<_> = (20..=30).map(|x| pixel(&rgba, x, 15)[3]).collect();
    assert_eq!(alphas[0], 255);
    assert!(
        alphas.windows(2).all(|pair| pair[0] > pair[1]),
        "{alphas:?}"
    );
}

#[test]
fn translucent_dabs_blend_over_the_image() {
    let mut renderer = renderer((WIDTH, HEIGHT));
    let white = [255; 4].repeat((WIDTH * HEIGHT) as usize);
    renderer.load_rgba(WIDTH, HEIGHT, &white).expect("load");
    renderer.paint(&[red_dab(1.0, 0.5)]);
    let [red, green, blue, alpha] = pixel(&pixels(&renderer), 20, 15);
    assert_eq!((red, alpha), (255, 255));
    assert!(green.abs_diff(128) <= 1 && blue.abs_diff(128) <= 1);
}

#[test]
fn present_applies_the_viewport() {
    let renderer = renderer((WIDTH, HEIGHT));
    renderer.paint(&[red_dab(1.0, 1.0)]);
    let target = renderer.device().create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let mut viewport = Viewport::default();
    viewport.zoom_at(Point::new(20.5, 15.5), 4.0);
    renderer.present(&target.create_view(&Default::default()), &viewport);
    let rgba = block_on(renderer.read_texture(&target).pixels()).expect("readback failed");
    assert_eq!(pixel(&rgba, 20, 15), [255, 0, 0, 255]);
    assert_eq!(pixel(&rgba, 50, 15), [255, 0, 0, 255]);
    assert_ne!(pixel(&rgba, 65, 15), [255, 0, 0, 255]);
}

#[test]
fn invalid_images_are_rejected() {
    let mut renderer = renderer((WIDTH, HEIGHT));
    assert!(matches!(
        renderer.load_rgba(0, 10, &[]),
        Err(RenderError::EmptyImage)
    ));
    assert!(matches!(
        renderer.load_rgba(u32::MAX, 1, &[]),
        Err(RenderError::ImageTooLarge { .. })
    ));
    assert!(matches!(
        renderer.load_rgba(2, 2, &[0; 15]),
        Err(RenderError::PixelCount {
            expected: 16,
            actual: 15
        })
    ));
    assert_eq!(renderer.document_size(), (WIDTH, HEIGHT));
}
