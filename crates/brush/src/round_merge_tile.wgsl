struct MergeUniforms {
    origin_origin: vec2u,
    origin_layer: u32,
    _pad0: u32,
    intermediate_origin: vec2u,
    intermediate_layer: u32,
    _pad1: u32,
};

struct MergePayload {
    tint_and_opacity: vec4f,
    coverage_params: vec4f,
}

@group(0) @binding(0) var origin_texture: texture_2d_array<f32>;
@group(0) @binding(1) var intermediate_texture: texture_2d_array<f32>;
@group(0) @binding(2) var<uniform> uniforms: MergeUniforms;
@group(0) @binding(3) var<storage, read> payload: MergePayload;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(positions[vertex_index], 0.0, 1.0);
}

fn coverage_for_source(source_value: f32) -> f32 {
    let source = max(source_value, 0.0);
    if (source <= 0.0) {
        return 0.0;
    }

    let hardness = clamp(payload.coverage_params.y, 0.0, 1.0);
    if (hardness >= 1.0) {
        return 1.0;
    }

    let hardness_threshold_source = payload.coverage_params.x;
    if (hardness_threshold_source <= 0.0) {
        return 0.0;
    }

    return clamp(source / hardness_threshold_source, 0.0, 1.0);
}

@fragment
fn fs_merge_tile(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let pixel = vec2u(pos.xy);
    let base = textureLoad(
        origin_texture,
        vec2i(uniforms.origin_origin + pixel),
        i32(uniforms.origin_layer),
        0
    );
    let intermediate = textureLoad(
        intermediate_texture,
        vec2i(uniforms.intermediate_origin + pixel),
        i32(uniforms.intermediate_layer),
        0
    );
    let stroke_opacity = clamp(payload.tint_and_opacity.a, 0.0, 1.0);
    let coverage_raw = coverage_for_source(intermediate.r);
    let coverage = coverage_raw * coverage_raw * (3.0 - 2.0 * coverage_raw);
    let effective_alpha = stroke_opacity * coverage;
    let out_alpha = base.a + (1.0 - base.a) * effective_alpha;
    let out_rgb = mix(base.rgb, payload.tint_and_opacity.rgb, effective_alpha);
    return vec4f(out_rgb, out_alpha);
}
