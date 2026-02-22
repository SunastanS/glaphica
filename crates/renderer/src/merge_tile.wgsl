struct MergeUniform {
    base_slot_origin_uv: vec2<f32>,
    stroke_slot_origin_uv: vec2<f32>,
    slot_uv_size: vec2<f32>,
    base_layer: f32,
    stroke_layer: f32,
    has_base: f32,
    opacity: f32,
    blend_mode: u32,
    _padding0: u32,
    _padding1: u32,
    _padding2: u32,
};

@group(0) @binding(0) var tile_atlas: texture_2d_array<f32>;
@group(0) @binding(1) var tile_sampler: sampler;
@group(0) @binding(2) var<uniform> merge_uniform: MergeUniform;

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(3.0, 1.0),
    );
    var out: VsOut;
    let pos = positions[vertex_index];
    out.clip_position = vec4<f32>(pos, 0.0, 1.0);
    out.uv = pos * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return out;
}

fn blend_normal(base: vec4<f32>, stroke: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(
        stroke.rgb * stroke.a + base.rgb * (1.0 - stroke.a),
        stroke.a + base.a * (1.0 - stroke.a),
    );
}

fn blend_multiply(base: vec4<f32>, stroke: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(
        stroke.rgb * base.rgb + base.rgb * (1.0 - stroke.a),
        stroke.a + base.a * (1.0 - stroke.a),
    );
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    let base_uv = merge_uniform.base_slot_origin_uv + input.uv * merge_uniform.slot_uv_size;
    let stroke_uv = merge_uniform.stroke_slot_origin_uv + input.uv * merge_uniform.slot_uv_size;
    let base = select(
        vec4<f32>(0.0, 0.0, 0.0, 0.0),
        textureSampleLevel(
            tile_atlas,
            tile_sampler,
            base_uv,
            i32(merge_uniform.base_layer),
            0.0,
        ),
        merge_uniform.has_base > 0.5,
    );
    let stroke_sample = textureSampleLevel(
        tile_atlas,
        tile_sampler,
        stroke_uv,
        i32(merge_uniform.stroke_layer),
        0.0,
    );
    let stroke = vec4<f32>(stroke_sample.rgb, stroke_sample.a * merge_uniform.opacity);

    if merge_uniform.blend_mode == 1u {
        return blend_multiply(base, stroke);
    }
    return blend_normal(base, stroke);
}
