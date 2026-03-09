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
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<storage, read> draw_input: DrawInputs;
@group(0) @binding(1) var<uniform> params: ShaderParams;
@group(1) @binding(0) var source_atlas: texture_2d_array<f32>;
@group(1) @binding(1) var cache_atlas: texture_2d_array<f32>;
@group(1) @binding(2) var atlas_sampler: sampler;

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

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    // Brush input uses image-tile coordinates (62x62), atlas tile is 64x64 with 1px gutter.
    let tile_local_x = pos.x - f32(params.tile_origin_x) - 1.0;
    let tile_local_y = pos.y - f32(params.tile_origin_y) - 1.0;
    let input_count = params.input_len / 6u;

    var buffer_alpha = 0.0;
    var stage = 0.0;
    // Packed same-tile dabs are resolved analytically here so the runtime can submit one draw
    // call per tile instead of one draw call per dab. For the round buffer stage, repeated red
    // alpha writes compose to 1 - product(1 - alpha_i), so order does not matter.
    for (var index = 0u; index < input_count; index = index + 1u) {
        let dab = draw_input.values[index];
        stage = dab.stage;
        let center = vec2<f32>(dab.center_local_x, dab.center_local_y);
        let pixel = vec2<f32>(tile_local_x, tile_local_y);
        let radius = max(dab.radius_px, 0.0);
        let hardness = clamp(dab.hardness, 0.0, 1.0);
        let opacity = clamp(dab.opacity, 0.0, 1.0);
        let dist = distance(pixel, center);

        let softness = max((1.0 - hardness) * radius, 0.0001);
        let inner_radius = max(radius - softness, 0.0);
        let falloff = 1.0 - smoothstep(inner_radius, radius, dist);
        let alpha = falloff * opacity;
        buffer_alpha = buffer_alpha + (1.0 - buffer_alpha) * alpha;
    }

    if (stage < 0.5) {
        if (buffer_alpha <= 0.0) {
            discard;
        }
        return vec4<f32>(1.0, 0.0, 0.0, buffer_alpha);
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
    let accum_alpha = clamp(source_sample.a, 0.0, 1.0);
    let effective_alpha = clamp(accum_alpha * buffer_alpha, 0.0, 1.0);

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

    let out_alpha = origin.a + (1.0 - origin.a) * effective_alpha;
    let out_rgb = mix(origin.rgb, vec3<f32>(1.0, 0.0, 0.0), effective_alpha);
    return vec4<f32>(out_rgb, out_alpha);
}
