struct ApplyUniforms {
    source_origin: vec2u,
    source_layer: u32,
    _pad0: u32,
    center_local: vec2f,
    radius_px: f32,
    hardness: f32,
    opacity: f32,
    _pad1: vec3u,
};

@group(0) @binding(0) var atlas_texture: texture_2d_array<f32>;
@group(0) @binding(1) var<uniform> uniforms: ApplyUniforms;

const PI: f32 = 3.141592653589793;
const SOFT_KERNEL_SCALE_N2: f32 = 8.0 / (3.0 * PI);
const HARD_BRUSH_THICKNESS: f32 = 4.0;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(positions[vertex_index], 0.0, 1.0);
}

fn hardness_response(hardness: f32) -> f32 {
    let h = clamp(hardness, 0.0, 1.0);
    return h * h * h * h * h * h;
}

fn round_kernel(radius: f32, hardness: f32, dist: f32) -> f32 {
    if (radius <= 0.0 || dist >= radius) {
        return 0.0;
    }

    let normalized_dist = dist / radius;
    let radial_falloff = max(1.0 - normalized_dist * normalized_dist, 0.0);
    let hard_t = hardness_response(hardness);
    let exponent = mix(1.5, 0.0, hard_t);
    let scale = mix(SOFT_KERNEL_SCALE_N2 / radius, 1.0, hard_t);
    return scale * pow(radial_falloff, exponent);
}

fn thickness_gain(hardness: f32) -> f32 {
    let hard_t = hardness_response(hardness);
    return mix(1.0, HARD_BRUSH_THICKNESS, hard_t);
}

@fragment
fn fs_apply_dab(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let pixel = vec2u(pos.xy);
    let source = textureLoad(
        atlas_texture,
        vec2i(uniforms.source_origin + pixel),
        i32(uniforms.source_layer),
        0
    );
    let tile_local = vec2f(pos.xy) - vec2f(1.0, 1.0);
    let radius = max(uniforms.radius_px, 0.0);
    let opacity = max(uniforms.opacity, 0.0);
    let dist = distance(tile_local, uniforms.center_local);
    let added =
        round_kernel(radius, uniforms.hardness, dist) * opacity * thickness_gain(uniforms.hardness);
    let next_alpha = max(source.a + added, 0.0);
    return vec4f(0.0, 0.0, 0.0, next_alpha);
}
