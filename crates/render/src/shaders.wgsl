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
fn tile_vertex(
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

@fragment
fn import_mask_fragment(in: TileVertex) -> @location(0) vec4f {
    let pixel = vec2i(in.position.xy) + in.origin - image_origin.xy;
    return vec4f(0.0, 0.0, 0.0, textureLoad(source, pixel, 0).r);
}

struct Resample {
    x: vec2f,
    y: vec2f,
    offset: vec2f,
    origin: vec2i,
    size: vec2i,
}

@group(0) @binding(1) var<uniform> resample: Resample;

@fragment
fn resample_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let point = vec2f(dot(resample.x, position.xy), dot(resample.y, position.xy)) + resample.offset;
    let pixel = vec2i(floor(point)) - resample.origin;
    if any(pixel < vec2i(0)) || any(pixel >= resample.size) {
        discard;
    }
    return textureLoad(source, pixel, 0);
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

fn dab_corner(index: u32) -> vec2f {
    return vec2f(f32(index & 1u), f32(index >> 1u)) * 2.0 - 1.0;
}

fn dab_position(center: vec2f, offset: vec2f, radius: f32) -> vec4f {
    let pixel = (center + offset * radius) / f32(TILE_SIZE);
    return vec4f(pixel.x * 2.0 - 1.0, 1.0 - pixel.y * 2.0, 0.0, 1.0);
}

@vertex
fn dab_vertex(
    @builtin(vertex_index) index: u32,
    @location(0) center: vec2f,
    @location(1) radius: f32,
    @location(2) inner: f32,
    @location(3) flow: f32,
) -> DabVertex {
    let offset = dab_corner(index);
    return DabVertex(dab_position(center, offset, radius), offset, inner, flow);
}

fn dab_coverage(offset: vec2f, inner: f32, flow: f32) -> f32 {
    return flow * (1.0 - smoothstep(inner, 1.0, length(offset)));
}

@fragment
fn dab_fragment(in: DabVertex) -> @location(0) vec4f {
    return vec4f(dab_coverage(in.offset, in.inner, in.flow));
}

// Stroke
struct Paint {
    color: vec3f,
    opacity: f32,
    selection: vec2i,
    canvas: vec2i,
    unselected: f32,
}

@group(0) @binding(1) var coverage: texture_2d<f32>;
@group(0) @binding(2) var<uniform> paint: Paint;
@group(0) @binding(3) var selection: texture_2d<f32>;
@group(0) @binding(4) var replacement: texture_2d<f32>;

fn selected(canvas_pixel: vec2i) -> f32 {
    let pixel = canvas_pixel - paint.selection;
    if any(pixel < vec2i(0)) || any(pixel >= vec2i(textureDimensions(selection))) {
        return paint.unselected;
    }
    return textureLoad(selection, pixel, 0).r;
}

fn stroke_weight(in: TileVertex) -> f32 {
    let pixel = vec2i(in.position.xy);
    return textureLoad(coverage, pixel, 0).r * paint.opacity * selected(pixel + in.origin);
}

fn over(top: vec4f, bottom: vec4f) -> vec4f {
    return top + bottom * (1.0 - top.a);
}

@fragment
fn stroke_fragment(in: TileVertex) -> @location(0) vec4f {
    let alpha = stroke_weight(in);
    return over(vec4f(paint.color * alpha, alpha), textureLoad(source, vec2i(in.position.xy), 0));
}

@fragment
fn erase_fragment(in: TileVertex) -> @location(0) vec4f {
    return textureLoad(source, vec2i(in.position.xy), 0) * (1.0 - stroke_weight(in));
}

@fragment
fn mask_stroke_fragment(in: TileVertex) -> @location(0) vec4f {
    let before = textureLoad(source, vec2i(in.position.xy), 0).a;
    return vec4f(0.0, 0.0, 0.0, mix(before, paint.color.r, stroke_weight(in)));
}

@fragment
fn clone_stroke_fragment(in: TileVertex) -> @location(0) vec4f {
    let pixel = vec2i(in.position.xy);
    let copied = textureLoad(replacement, pixel, 0) * stroke_weight(in);
    return over(copied, textureLoad(source, pixel, 0));
}

@fragment
fn replace_stroke_fragment(in: TileVertex) -> @location(0) vec4f {
    let pixel = vec2i(in.position.xy);
    let before = textureLoad(source, pixel, 0);
    return mix(before, textureLoad(replacement, pixel, 0), stroke_weight(in));
}

// Selected pixels
@fragment
fn lift_fragment(in: TileVertex) -> @location(0) vec4f {
    let pixel = vec2i(in.position.xy);
    return textureLoad(source, pixel, 0) * selected(pixel + in.origin);
}

@fragment
fn drop_fragment(in: TileVertex) -> @location(0) vec4f {
    let pixel = vec2i(in.position.xy);
    let left_behind = textureLoad(source, pixel, 0) * (1.0 - selected(pixel + in.origin));
    return over(textureLoad(replacement, pixel, 0), left_behind);
}

// Smudge
struct SmudgeVertex {
    @builtin(position) position: vec4f,
    @location(0) offset: vec2f,
    @location(1) @interpolate(flat) inner: f32,
    @location(2) @interpolate(flat) flow: f32,
    @location(3) @interpolate(flat) origin: vec2i,
    @location(4) @interpolate(flat) delta: vec2i,
}

@vertex
fn smudge_vertex(
    @builtin(vertex_index) index: u32,
    @location(0) center: vec2f,
    @location(1) radius: f32,
    @location(2) inner: f32,
    @location(3) flow: f32,
    @location(4) origin: vec2i,
    @location(5) delta: vec2i,
) -> SmudgeVertex {
    let offset = dab_corner(index);
    let position = dab_position(center, offset, radius);
    return SmudgeVertex(position, offset, inner, flow, origin, delta);
}

@fragment
fn smudge_fragment(in: SmudgeVertex) -> @location(0) vec4f {
    let pixel = vec2i(in.position.xy);
    let canvas_pixel = pixel + in.origin;
    let under = textureLoad(source, pixel, 0);
    let dragged_from = canvas_pixel - in.delta;
    if any(dragged_from < vec2i(0)) || any(dragged_from >= paint.canvas) {
        return under;
    }
    let weight = dab_coverage(in.offset, in.inner, in.flow) * paint.opacity * selected(canvas_pixel);
    return mix(under, textureLoad(replacement, pixel, 0), weight);
}

// Heal
struct Heal {
    origin: vec2i,
    extent: vec2i,
    scale: i32,
}

@group(0) @binding(2) var heal_third: texture_2d<f32>;
@group(0) @binding(3) var heal_sampler: sampler;
@group(0) @binding(4) var<uniform> heal: Heal;

const UNCOVERED = 0.002;

@fragment
fn heal_base_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let cell = vec2i(position.xy) * heal.scale - heal.origin;
    var total = vec4f(0.0);
    for (var y = 0; y < heal.scale; y += 1) {
        for (var x = 0; x < heal.scale; x += 1) {
            let pixel = cell + vec2i(x, y);
            let before = textureLoad(source, pixel, 0);
            let copied = textureLoad(heal_third, pixel, 0);
            if before.a > 0.0 && copied.a > 0.0 && textureLoad(coverage, pixel, 0).r < UNCOVERED {
                total += vec4f(straight_color(before) - straight_color(copied), 1.0);
            }
        }
    }
    return vec4f(total.rgb / max(total.a, 1.0), min(total.a, 1.0));
}

@fragment
fn heal_pull_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let corner = vec2i(position.xy) * 2;
    let size = vec2i(textureDimensions(source));
    var total = vec4f(0.0);
    for (var index = 0; index < 4; index += 1) {
        let pixel = corner + vec2i(index & 1, index >> 1u);
        if all(pixel < size) {
            let known = textureLoad(source, pixel, 0);
            total += vec4f(known.rgb * known.a, known.a);
        }
    }
    return vec4f(total.rgb / max(total.a, 1.0), min(total.a, 1.0));
}

@fragment
fn heal_push_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let known = textureLoad(source, vec2i(position.xy), 0);
    let point = position.xy / (2.0 * vec2f(textureDimensions(coverage)));
    let guess = textureSampleLevel(coverage, heal_sampler, point, 0.0);
    return select(vec4f(guess.rgb, 0.0), known, known.a > 0.0);
}

@fragment
fn heal_relax_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let pixel = vec2i(position.xy);
    let known = textureLoad(source, pixel, 0);
    if known.a > 0.0 {
        return known;
    }
    let last = vec2i(textureDimensions(source)) - 1;
    let around = textureLoad(coverage, clamp(pixel + vec2i(1, 0), vec2i(0), last), 0)
        + textureLoad(coverage, clamp(pixel - vec2i(1, 0), vec2i(0), last), 0)
        + textureLoad(coverage, clamp(pixel + vec2i(0, 1), vec2i(0), last), 0)
        + textureLoad(coverage, clamp(pixel - vec2i(0, 1), vec2i(0), last), 0);
    return vec4f(around.rgb * 0.25, 0.0);
}

@fragment
fn heal_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let pixel = vec2i(position.xy);
    let before = textureLoad(source, pixel, 0);
    let copied = textureLoad(coverage, pixel, 0);
    if before.a == 0.0 || copied.a == 0.0 {
        return before;
    }
    let point = (vec2f(heal.origin) + position.xy) / vec2f(heal.extent);
    let difference = textureSampleLevel(heal_third, heal_sampler, point, 0.0).rgb;
    let color = clamp(straight_color(copied) + difference, vec3f(0.0), vec3f(1.0));
    return vec4f(color * before.a, before.a);
}

@fragment
fn apply_mask_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let pixel = vec2i(position.xy);
    return textureLoad(source, pixel, 0) * textureLoad(coverage, pixel, 0).a;
}

@fragment
fn copy_tile_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    return textureLoad(source, vec2i(position.xy) - export_origin.xy, 0);
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
    has_selection: f32,
    selection: vec2i,
}

@group(0) @binding(2) var<uniform> view: View;

const WORKSPACE_COLOR = vec3f(0.09, 0.1, 0.12);
const CHECKER_SIZE = 8.0;

const DASH_LENGTH = 4;

fn is_selected(point: vec2f) -> bool {
    let pixel = vec2i(floor(point)) - view.selection;
    if any(pixel < vec2i(0)) || any(pixel >= vec2i(textureDimensions(selection))) {
        return false;
    }
    return textureLoad(selection, pixel, 0).r >= 0.5;
}

fn is_outline(point: vec2f) -> bool {
    let step = vec2f(1.0 / view.zoom, 0.0);
    return is_selected(point) && !(is_selected(point + step.xy) && is_selected(point - step.xy)
        && is_selected(point + step.yx) && is_selected(point - step.yx));
}

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
    if view.has_selection > 0.0 && is_outline(point) {
        let along = vec2i(position.xy);
        let dash = ((along.x + along.y) / DASH_LENGTH) & 1;
        return vec4f(vec3f(f32(dash)), 1.0);
    }
    let cell = vec2i(floor(position.xy / CHECKER_SIZE));
    let checker = select(0.8, 1.0, ((cell.x + cell.y) & 1) == 0);
    return vec4f(texel.rgb + checker * (1.0 - texel.a), 1.0);
}

// Adjustment layers read the premultiplied backdrop of their own stack.
struct Grade {
    origin: vec2i,
    size: vec2i,
    op: u32,
    seed: u32,
    opacity: f32,
    blend: f32,
    params: array<vec4f, 3>,
}

@group(0) @binding(0) var neighbor_0: texture_2d<f32>;
@group(0) @binding(1) var neighbor_1: texture_2d<f32>;
@group(0) @binding(2) var neighbor_2: texture_2d<f32>;
@group(0) @binding(3) var neighbor_3: texture_2d<f32>;
@group(0) @binding(4) var neighbor_4: texture_2d<f32>;
@group(0) @binding(5) var neighbor_5: texture_2d<f32>;
@group(0) @binding(6) var neighbor_6: texture_2d<f32>;
@group(0) @binding(7) var neighbor_7: texture_2d<f32>;
@group(0) @binding(8) var neighbor_8: texture_2d<f32>;
@group(0) @binding(9) var grade_original: texture_2d<f32>;
@group(0) @binding(10) var grade_mask: texture_2d<f32>;
@group(0) @binding(11) var<uniform> grade: Grade;
@group(0) @binding(12) var<storage, read> grade_table: array<vec4f>;

fn neighborhood(pixel: vec2i) -> vec4f {
    let canvas_pixel = clamp(pixel + grade.origin, vec2i(0), grade.size - 1);
    let coord = canvas_pixel / TILE_SIZE - grade.origin / TILE_SIZE;
    let local = canvas_pixel % TILE_SIZE;
    let index = (coord.y + 1) * 3 + coord.x + 1;
    switch index {
        case 0: { return textureLoad(neighbor_0, local, 0); }
        case 1: { return textureLoad(neighbor_1, local, 0); }
        case 2: { return textureLoad(neighbor_2, local, 0); }
        case 3: { return textureLoad(neighbor_3, local, 0); }
        case 5: { return textureLoad(neighbor_5, local, 0); }
        case 6: { return textureLoad(neighbor_6, local, 0); }
        case 7: { return textureLoad(neighbor_7, local, 0); }
        case 8: { return textureLoad(neighbor_8, local, 0); }
        default: { return textureLoad(neighbor_4, local, 0); }
    }
}

fn srgb_to_linear(color: vec3f) -> vec3f {
    return select(pow((color + 0.055) / 1.055, vec3f(2.4)), color / 12.92, color <= vec3f(0.04045));
}

fn linear_to_srgb(color: vec3f) -> vec3f {
    let rgb = clamp(color, vec3f(0.0), vec3f(1.0));
    return select(1.055 * pow(rgb, vec3f(1.0 / 2.4)) - 0.055, rgb * 12.92, rgb <= vec3f(0.0031308));
}

fn linear_premul(pixel: vec4f) -> vec4f {
    return vec4f(srgb_to_linear(straight_color(pixel)) * pixel.a, pixel.a);
}

fn curve(value: f32) -> f32 {
    var lo = 0u;
    var hi = u32(grade.params[0].x) - 1u;
    while hi - lo > 1u {
        let mid = (lo + hi) / 2u;
        if grade_table[mid].x <= value {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let a = grade_table[lo].xy;
    let b = grade_table[hi].xy;
    return mix(a.y, b.y, (value - a.x) / max(b.x - a.x, 1e-8));
}

fn hsl_grade(rgb: vec3f, params: vec3f) -> vec3f {
    let low = min(rgb.r, min(rgb.g, rgb.b));
    let high = max(rgb.r, max(rgb.g, rgb.b));
    let chroma = high - low;
    var hue = 0.0;
    var sat = 0.0;
    var light = (high + low) * 0.5;
    if chroma > 1e-6 {
        if high == rgb.r {
            hue = (rgb.g - rgb.b) / chroma;
        } else if high == rgb.g {
            hue = (rgb.b - rgb.r) / chroma + 2.0;
        } else {
            hue = (rgb.r - rgb.g) / chroma + 4.0;
        }
        sat = chroma / max(1.0 - abs(2.0 * light - 1.0), 1e-6);
    }
    hue = fract(hue / 6.0 + params.x);
    sat = clamp(sat * (1.0 + params.y), 0.0, 1.0);
    light = mix(light, select(0.0, 1.0, params.z >= 0.0), abs(params.z));
    let wheel = clamp(abs(fract(hue + vec3f(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0) - 1.0, vec3f(0.0), vec3f(1.0));
    return light + (wheel - 0.5) * (1.0 - abs(2.0 * light - 1.0)) * sat;
}

fn cube_index(p: vec3u, n: u32) -> u32 {
    return p.x + n * (p.y + n * p.z);
}

fn lookup(rgb: vec3f) -> vec3f {
    let low = grade.params[0].xyz;
    let high = grade.params[1].xyz;
    let n = u32(grade.params[0].w);
    let p = clamp((rgb - low) / (high - low), vec3f(0.0), vec3f(1.0)) * f32(n - 1u);
    let base = min(vec3u(p), vec3u(n - 2u));
    let f = p - vec3f(base);
    if grade.op == 7u {
        return vec3f(
            mix(grade_table[base.r].r, grade_table[base.r + 1u].r, f.r),
            mix(grade_table[base.g].g, grade_table[base.g + 1u].g, f.g),
            mix(grade_table[base.b].b, grade_table[base.b + 1u].b, f.b),
        );
    }
    var a = 0u;
    var b = 1u;
    var c = 2u;
    if f[a] < f[b] { let t = a; a = b; b = t; }
    if f[b] < f[c] { let t = b; b = c; c = t; }
    if f[a] < f[b] { let t = a; a = b; b = t; }
    var point = base;
    let v0 = grade_table[cube_index(point, n)].rgb;
    point[a] += 1u;
    let v1 = grade_table[cube_index(point, n)].rgb;
    point[b] += 1u;
    let v2 = grade_table[cube_index(point, n)].rgb;
    point[c] += 1u;
    let v3 = grade_table[cube_index(point, n)].rgb;
    return v0 + f[a] * (v1 - v0) + f[b] * (v2 - v1) + f[c] * (v3 - v2);
}

fn grade_noise(pixel: vec2i) -> f32 {
    let p = vec2u(pixel + grade.origin);
    var hash = (p.x * 0x8da6b343u) ^ (p.y * 0xd8163841u) ^ grade.seed;
    hash = (hash ^ (hash >> 16u)) * 0x7feb352du;
    hash = (hash ^ (hash >> 15u)) * 0x846ca68bu;
    return f32((hash ^ (hash >> 16u)) & 0xffffu) / 65535.0 - 0.5;
}

fn point_grade(rgb: vec3f, pixel: vec2i) -> vec3f {
    let p = grade.params[0];
    switch grade.op {
        case 1u: { return (rgb - 0.18) * p.y + 0.18 + p.x; }
        case 2u: {
            let input = clamp((rgb - p.x) / (p.y - p.x), vec3f(0.0), vec3f(1.0));
            return mix(vec3f(p.w), vec3f(grade.params[1].x), pow(input, vec3f(p.z)));
        }
        case 3u: { return vec3f(curve(rgb.r), curve(rgb.g), curve(rgb.b)); }
        case 4u: { return hsl_grade(rgb, p.xyz); }
        case 5u: {
            let luma = dot(rgb, vec3f(0.2126, 0.7152, 0.0722));
            let shadows = (1.0 - luma) * (1.0 - luma);
            let highlights = luma * luma;
            return rgb + (p.xyz * shadows + grade.params[1].xyz * (1.0 - shadows - highlights)
                + grade.params[2].xyz * highlights) * 0.5;
        }
        case 6u: { return rgb * p.x; }
        case 9u: {
            var around = vec3f(0.0);
            for (var i = 0; i < 4; i += 1) {
                let step = array<vec2i, 4>(vec2i(-1, 0), vec2i(1, 0), vec2i(0, -1), vec2i(0, 1))[i];
                let tap = neighborhood(pixel + step);
                around += mix(rgb, srgb_to_linear(straight_color(tap)), tap.a);
            }
            return rgb + p.x * (rgb - around * 0.25);
        }
        case 12u: {
            let center = (vec2f(pixel + grade.origin) + 0.5) / vec2f(grade.size) * 2.0 - 1.0;
            return rgb * (1.0 - p.x * smoothstep(p.y, p.y + p.z, length(center) * 0.70710678));
        }
        case 13u: { return rgb + vec3f(grade_noise(pixel) * p.x * 0.25); }
        default: { return rgb; }
    }
}

fn gaussian(pixel: vec2i, horizontal: bool) -> vec4f {
    let radius = i32(grade.params[0].x);
    var total = vec4f(0.0);
    for (var i = -radius; i <= radius; i += 1) {
        if horizontal {
            total += linear_premul(neighborhood(pixel + vec2i(i, 0))) * grade_table[abs(i)].x;
        } else {
            total += neighborhood(pixel + vec2i(0, i)) * grade_table[abs(i)].x;
        }
    }
    return total;
}

@fragment
fn grade_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let pixel = vec2i(position.xy);
    if any(pixel + grade.origin >= grade.size) {
        return vec4f(0.0);
    }
    if grade.op == 10u {
        return gaussian(pixel, true);
    }
    let before = textureLoad(grade_original, pixel, 0);
    let opacity = grade.opacity * textureLoad(grade_mask, pixel, 0).a;
    if opacity == 0.0 {
        return before;
    }
    let base = straight_color(before);
    var after: vec4f;
    if grade.op == 11u {
        let blurred = gaussian(pixel, false);
        after = vec4f(linear_to_srgb(straight_color(blurred)) * blurred.a, blurred.a);
    } else {
        if before.a == 0.0 {
            return before;
        }
        var adjusted: vec3f;
        if grade.op == 7u || grade.op == 8u {
            adjusted = clamp(lookup(base), vec3f(0.0), vec3f(1.0));
        } else {
            adjusted = linear_to_srgb(point_grade(srgb_to_linear(base), pixel));
        }
        after = vec4f(adjusted * before.a, before.a);
    }
    if i32(grade.blend) != NORMAL && before.a > 0.0 && after.a > 0.0 {
        after = vec4f(blend_colors(i32(grade.blend), base, straight_color(after)) * after.a, after.a);
    }
    return mix(before, after, opacity);
}
