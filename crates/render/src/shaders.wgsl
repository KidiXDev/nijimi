@vertex
fn fullscreen_vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4f {
    let corner = vec2f(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4f(corner * 2.0 - 1.0, 0.0, 1.0);
}

// Document passes
@group(0) @binding(0) var source: texture_2d<f32>;

@fragment
fn premultiply_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let texel = textureLoad(source, vec2i(position.xy), 0);
    return vec4f(texel.rgb * texel.a, texel.a);
}

@fragment
fn unpremultiply_fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let texel = textureLoad(source, vec2i(position.xy), 0);
    return vec4f(texel.rgb / max(texel.a, 1e-5), texel.a);
}

// Dabs
struct DabVertex {
    @builtin(position) position: vec4f,
    @location(0) offset: vec2f,
    @location(1) @interpolate(flat) inner: f32,
    @location(2) @interpolate(flat) color: vec4f,
}

@vertex
fn dab_vertex(
    @builtin(vertex_index) index: u32,
    @location(0) center: vec2f,
    @location(1) half_size: vec2f,
    @location(2) inner: f32,
    @location(3) color: vec4f,
) -> DabVertex {
    let offset = vec2f(f32(index & 1u), f32(index >> 1u)) * 2.0 - 1.0;
    return DabVertex(vec4f(center + offset * half_size, 0.0, 1.0), offset, inner, color);
}

@fragment
fn dab_fragment(in: DabVertex) -> @location(0) vec4f {
    return in.color * (1.0 - smoothstep(in.inner, 1.0, length(in.offset)));
}

// Present
struct View {
    pan: vec2f,
    zoom: f32,
    _padding: f32,
}

@group(0) @binding(1) var<uniform> view: View;
@group(0) @binding(2) var linear_sampler: sampler;

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
        texel = textureSampleLevel(source, linear_sampler, point / document_size, 0.0);
    }
    let cell = vec2i(floor(position.xy / CHECKER_SIZE));
    let checker = select(0.8, 1.0, ((cell.x + cell.y) & 1) == 0);
    return vec4f(texel.rgb + checker * (1.0 - texel.a), 1.0);
}
