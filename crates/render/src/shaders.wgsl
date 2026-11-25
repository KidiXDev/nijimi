fn fullscreen_corner(index: u32) -> vec4f {
    let corner = vec2f(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4f(corner * 2.0 - 1.0, 0.0, 1.0);
}

@vertex
fn fullscreen_vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4f {
    return fullscreen_corner(index);
}

@group(0) @binding(0) var source: texture_2d<f32>;

// Import
const TILE_SIZE = 256;

struct TileVertex {
    @builtin(position) position: vec4f,
    @location(0) @interpolate(flat) origin: vec2i,
}

@vertex
fn import_vertex(
    @builtin(vertex_index) index: u32,
    @builtin(instance_index) packed_coord: u32,
) -> TileVertex {
    let coord = vec2u(packed_coord & 0xffffu, packed_coord >> 16u);
    return TileVertex(fullscreen_corner(index),vec2i(coord) * TILE_SIZE);
}

@group(0) @binding(1) var<uniform> image_origin: vec4i;

@fragment
fn import_fragment(in: TileVertex) -> @location(0) vec4f {
    let pixel = vec2i(in.position.xy) + in.origin - image_origin.xy;
    if any(pixel < vec2i(0)) || any(pixel >= vec2i(textureDimensions(source))) {
        return vec4f(0.0);
    }
    let texel = textureLoad(source, pixel, 0);
    return vec4f(texel.rgb * texel.a, texel.a);
}

struct Resample {
    x: vec2f,
    y: vec2f,
    offset: vec2f,
    size: vec2f,
}

@group(0) @binding(1) var<uniform> resample: Resample;

@fragment
fn resample_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let pixel = vec2f(dot(resample.x, position.xy), dot(resample.y, position.xy)) + resample.offset;
    if any(pixel < vec2f(0.0)) || any(pixel >= resample.size) {
        discard;
    }
    return textureLoad(source, vec2i(pixel), 0);
}

// Export
@group(0) @binding(1) var<uniform> export_origin: vec4i;

@fragment
fn export_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let texel = textureLoad(source, vec2i(position.xy) + export_origin.xy, 0);
    return vec4f(texel.rgb / max(texel.a, 1e-5), texel.a);
}

// Dabs
struct DabVertex {
    @builtin(position) position: vec4f,
    @location(0) offset: vec2f,
    @location(1) @interpolate(flat) inner: f32,
    @location(2) @interpolate(flat) flow: f32,
}

@vertex
fn dab_vertex(
    @builtin(vertex_index) index: u32,
    @location(0) center: vec2f,
    @location(1) radius: f32,
    @location(2) inner: f32,
    @location(3) flow: f32,
) -> DabVertex {
    let offset = vec2f(f32(index & 1u), f32(index >> 1u)) * 2.0 - 1.0;
    let pixel = (center + offset * radius) / f32(TILE_SIZE);
    let position = vec2f(pixel.x * 2.0 - 1.0, 1.0 - pixel.y * 2.0);
    return DabVertex(vec4f(position, 0.0, 1.0), offset, inner, flow);
}

@fragment
fn dab_fragment(in: DabVertex) -> @location(0) vec4f {
    return vec4f(in.flow * (1.0 - smoothstep(in.inner, 1.0, length(in.offset))));
}

// Stroke
struct Paint {
    color: vec3f,
    opacity: f32,
}

@group(0) @binding(1) var coverage: texture_2d<f32>;
@group(0) @binding(2) var<uniform> paint: Paint;

@fragment
fn stroke_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let pixel = vec2i(position.xy);
    let alpha = textureLoad(coverage, pixel, 0).r * paint.opacity;
    return vec4f(paint.color * alpha, alpha) + textureLoad(source, pixel, 0) * (1.0 - alpha);
}

@fragment
fn erase_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let pixel = vec2i(position.xy);
    let alpha = textureLoad(coverage, pixel, 0).r * paint.opacity;
    return textureLoad(source, pixel, 0) * (1.0 - alpha);
}

// Layer compositing
struct Layers {
    origin: vec2i,
    appearance: array<vec4f, 8>,
}

@group(0) @binding(1) var layer_0: texture_2d<f32>;
@group(0) @binding(2) var layer_1: texture_2d<f32>;
@group(0) @binding(3) var layer_2: texture_2d<f32>;
@group(0) @binding(4) var layer_3: texture_2d<f32>;
@group(0) @binding(5) var layer_4: texture_2d<f32>;
@group(0) @binding(6) var layer_5: texture_2d<f32>;
@group(0) @binding(7) var layer_6: texture_2d<f32>;
@group(0) @binding(8) var layer_7: texture_2d<f32>;
@group(0) @binding(9) var<uniform> layers: Layers;

const NORMAL = 0;
const MULTIPLY = 1;
const SCREEN = 2;
const OVERLAY = 3;
const SOFT_LIGHT = 4;
const ADD = 5;
const DARKEN = 6;
const LIGHTEN = 7;

fn hard_light(top: vec3f, bottom: vec3f) -> vec3f {
    let doubled = top * 2.0;
    let screened = bottom + doubled - 1.0 - bottom * (doubled - 1.0);
    return select(screened, bottom * doubled, top <= vec3f(0.5));
}

fn soft_light(bottom: vec3f, top: vec3f) -> vec3f {
    let curve = select(
        sqrt(bottom),
        ((16.0 * bottom - 12.0) * bottom + 4.0) * bottom,
        bottom <= vec3f(0.25),
    );
    let darker = bottom - (1.0 - 2.0 * top) * bottom * (1.0 - bottom);
    let lighter = bottom + (2.0 * top - 1.0) * (curve - bottom);
    return select(lighter, darker, top <= vec3f(0.5));
}

fn blend_colors(mode: i32, bottom: vec3f, top: vec3f) -> vec3f {
    switch mode {
        case MULTIPLY: { return bottom * top; }
        case SCREEN: { return bottom + top - bottom * top; }
        case OVERLAY: { return hard_light(bottom, top); }
        case SOFT_LIGHT: { return soft_light(bottom, top); }
        case ADD: { return min(bottom + top, vec3f(1.0)); }
        case DARKEN: { return min(bottom, top); }
        case LIGHTEN: { return max(bottom, top); }
        default: { return top; }
    }
}

fn straight_color(texel: vec4f) -> vec3f {
    return clamp(texel.rgb / max(texel.a, 1e-5), vec3f(0.0), vec3f(1.0));
}

fn blend_layer(bottom: vec4f, texel: vec4f, appearance: vec4f) -> vec4f {
    let top = texel * appearance.x;
    let mode = i32(appearance.y);
    if top.a <= 0.0 {
        return bottom;
    }
    if mode == NORMAL {
        return top + bottom * (1.0 - top.a);
    }
    let mixed = blend_colors(mode, straight_color(bottom), straight_color(top));
    let color = top.rgb * (1.0 - bottom.a) + bottom.rgb * (1.0 - top.a) + mixed * top.a * bottom.a;
    return vec4f(color, top.a + bottom.a * (1.0 - top.a));
}

@fragment
fn composite_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let pixel = vec2i(position.xy) - layers.origin;
    var result = textureLoad(source, pixel, 0);
    result = blend_layer(result, textureLoad(layer_0, pixel, 0), layers.appearance[0]);
    result = blend_layer(result, textureLoad(layer_1, pixel, 0), layers.appearance[1]);
    result = blend_layer(result, textureLoad(layer_2, pixel, 0), layers.appearance[2]);
    result = blend_layer(result, textureLoad(layer_3, pixel, 0), layers.appearance[3]);
    result = blend_layer(result, textureLoad(layer_4, pixel, 0), layers.appearance[4]);
    result = blend_layer(result, textureLoad(layer_5, pixel, 0), layers.appearance[5]);
    result = blend_layer(result, textureLoad(layer_6, pixel, 0), layers.appearance[6]);
    result = blend_layer(result, textureLoad(layer_7, pixel, 0), layers.appearance[7]);
    return result;
}

// Mipmaps
@group(0) @binding(1) var linear_sampler: sampler;

@fragment
fn mipmap_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let target_size = max(textureDimensions(source) / 2u, vec2u(1u));
    return textureSampleLevel(source, linear_sampler, position.xy / vec2f(target_size), 0.0);
}

// Present
struct View {
    pan: vec2f,
    zoom: f32,
    _padding: f32,
}

@group(0) @binding(2) var<uniform> view: View;

const WORKSPACE_COLOR = vec3f(0.09, 0.1, 0.12);
const CHECKER_SIZE = 8.0;

@fragment
fn present_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let document_size = vec2f(textureDimensions(source));
    let point = (position.xy - view.pan) / view.zoom;
    if any(point < vec2f(0.0)) || any(point >= document_size) {
        return vec4f(WORKSPACE_COLOR, 1.0);
    }
    var texel: vec4f;
    if view.zoom >= 1.0 {
        texel = textureLoad(source, vec2i(point), 0);
    } else {
        texel = textureSampleLevel(
            source,
            linear_sampler,
            point / document_size,
            -log2(view.zoom),
        );
    }
    let cell = vec2i(floor(position.xy / CHECKER_SIZE));
    let checker = select(0.8, 1.0, ((cell.x + cell.y) & 1) == 0);
    return vec4f(texel.rgb + checker * (1.0 - texel.a), 1.0);
}
