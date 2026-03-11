struct DrawInput {
    center_local_x: f32,
    center_local_y: f32,
    radius_px: f32,
    hardness: f32,
    opacity: f32,
    stage: f32,
}

struct DrawInputs {
    values: array<DrawInput>,
}

struct ShaderParams {
    input_len: u32,
    tile_origin_x: u32,
    tile_origin_y: u32,
    tile_layer: u32,
    tile_size_x: u32,
    tile_size_y: u32,
    src_tile_origin_x: u32,
    src_tile_origin_y: u32,
    src_tile_layer: u32,
    cache_tile_origin_x: u32,
    cache_tile_origin_y: u32,
    cache_tile_layer: u32,
    has_cache_tile: u32,
    erase: u32,
    tint_r: f32,
    tint_g: f32,
    tint_b: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<storage, read> draw_input: DrawInputs;
@group(0) @binding(1) var<uniform> params: ShaderParams;
@group(1) @binding(0) var source_atlas: texture_2d_array<f32>;
@group(1) @binding(1) var cache_atlas: texture_2d_array<f32>;
@group(1) @binding(2) var atlas_sampler: sampler;

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
    let xy = positions[vertex_index];
    return vec4<f32>(xy, 0.0, 1.0);
}

fn round_brush_soft_kernel(radius: f32, dist: f32) -> f32 {
    if (radius <= 0.0 || dist >= radius) {
        return 0.0;
    }

    let normalized_dist = dist / radius;
    let radial_falloff = max(1.0 - normalized_dist * normalized_dist, 0.0);
    return SOFT_KERNEL_SCALE_N2 * pow(radial_falloff, 1.5) / radius;
}

fn round_brush_hardness_response(hardness: f32) -> f32 {
    let h = clamp(hardness, 0.0, 1.0);
    return h * h * h * h * h * h;
}

fn round_brush_kernel(radius: f32, hardness: f32, dist: f32) -> f32 {
    if (radius <= 0.0 || dist >= radius) {
        return 0.0;
    }

    let normalized_dist = dist / radius;
    let radial_falloff = max(1.0 - normalized_dist * normalized_dist, 0.0);
    let hard_t = round_brush_hardness_response(hardness);
    let exponent = mix(1.5, 0.0, hard_t);
    let scale = mix(SOFT_KERNEL_SCALE_N2 / radius, 1.0, hard_t);
    return scale * pow(radial_falloff, exponent);
}

fn round_brush_thickness_gain(hardness: f32) -> f32 {
    let hard_t = round_brush_hardness_response(hardness);
    return mix(1.0, HARD_BRUSH_THICKNESS, hard_t);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    // Brush input uses image-tile coordinates (62x62), atlas tile is 64x64 with 1px gutter.
    let tile_local_x = pos.x - f32(params.tile_origin_x) - 1.0;
    let tile_local_y = pos.y - f32(params.tile_origin_y) - 1.0;
    let input_count = params.input_len / 6u;

    var thickness = 0.0;
    var stage = 0.0;
    for (var index = 0u; index < input_count; index = index + 1u) {
        let dab = draw_input.values[index];
        stage = dab.stage;
        let center = vec2<f32>(dab.center_local_x, dab.center_local_y);
        let pixel = vec2<f32>(tile_local_x, tile_local_y);
        let radius = max(dab.radius_px, 0.0);
        let hardness = clamp(dab.hardness, 0.0, 1.0);
        let opacity = max(dab.opacity, 0.0);
        if (radius <= 0.0 || opacity <= 0.0) {
            continue;
        }

        let dist = distance(pixel, center);
        if (dist > radius) {
            continue;
        }

        let kernel = round_brush_kernel(radius, hardness, dist);
        let thickness_gain = round_brush_thickness_gain(hardness);
        thickness = thickness + kernel * opacity * thickness_gain;
    }

    if (stage < 0.5) {
        if (thickness <= 0.0) {
            discard;
        }
        return vec4<f32>(0.0, 0.0, 0.0, thickness);
    }

    let source_texel = vec2<i32>(
        i32(pos.x) - i32(params.tile_origin_x) + i32(params.src_tile_origin_x),
        i32(pos.y) - i32(params.tile_origin_y) + i32(params.src_tile_origin_y),
    );
    let source_sample = textureLoad(
        source_atlas,
        source_texel,
        i32(params.src_tile_layer),
        0,
    );
    let accum_thickness = max(source_sample.a, 0.0);
    let effective_alpha = 1.0 - exp(-accum_thickness * max(thickness, 0.0));

    var origin = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    if (params.has_cache_tile != 0u) {
        let cache_texel = vec2<i32>(
            i32(pos.x) - i32(params.tile_origin_x) + i32(params.cache_tile_origin_x),
            i32(pos.y) - i32(params.tile_origin_y) + i32(params.cache_tile_origin_y),
        );
        origin = textureLoad(
            cache_atlas,
            cache_texel,
            i32(params.cache_tile_layer),
            0,
        );
    }

    let tint = vec3<f32>(params.tint_r, params.tint_g, params.tint_b);
    let out_alpha = origin.a + (1.0 - origin.a) * effective_alpha;
    let out_rgb = mix(origin.rgb, tint, effective_alpha);
    return vec4<f32>(out_rgb, out_alpha);
}
