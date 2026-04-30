struct ApplyUniforms {
    tile_origin_x: u32,
    tile_origin_y: u32,
    source_origin_x: u32,
    source_origin_y: u32,
    source_layer: u32,
    _pad0: u32,
};

struct ApplyPayload {
    center_local_x: f32,
    center_local_y: f32,
    radius_px: f32,
    flow: f32,
}

@group(0) @binding(2) var<uniform> uniforms: ApplyUniforms;
@group(0) @binding(3) var<storage, read> payload: ApplyPayload;

const ROUND_DAB_KERNEL_A: f32 = 2.0;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(positions[vertex_index], 0.0, 1.0);
}

fn round_kernel(radius: f32, pixel_distance: f32) -> f32 {
    if (radius <= 0.0 || pixel_distance >= radius) {
        return 0.0;
    }

    let r = pixel_distance / radius;
    let r2 = r * r;
    return pow(1.0 - r2, ROUND_DAB_KERNEL_A);
}

@fragment
fn fs_apply_dab_linear_add(@builtin(position) pos: vec4f) -> @location(0) f32 {
    let tile_origin = vec2u(uniforms.tile_origin_x, uniforms.tile_origin_y);
    let tile_local = vec2f(pos.xy) - vec2f(tile_origin) - vec2f(1.0, 1.0);
    let radius = max(payload.radius_px, 0.0);
    let flow = max(payload.flow, 0.0);
    let center_local = vec2f(payload.center_local_x, payload.center_local_y);
    let pixel_distance = distance(tile_local, center_local);
    return round_kernel(radius, pixel_distance) * flow;
}
