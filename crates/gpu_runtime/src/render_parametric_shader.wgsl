struct ParametricParams {
    tile_origin: vec2<f32>,
    opacity: f32,
    _padding: f32,
};

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: ParametricParams;

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    let tile_xy = input.position - params.tile_origin + vec2<f32>(f32(1u), f32(1u));
    let ndc_x = tile_xy.x / f32(66u) * 2.0 - 1.0;
    let ndc_y = 1.0 - tile_xy.y / f32(66u) * 2.0;
    var output: VertexOutput;
    output.clip_position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    output.color = input.color;
    return output;
}

@fragment
fn fs_normal(input: VertexOutput) -> @location(0) vec4<f32> {
    let alpha = clamp(input.color.a * params.opacity, 0.0, 1.0);
    if (alpha <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(input.color.rgb * params.opacity, alpha);
}

@fragment
fn fs_multiply(input: VertexOutput) -> @location(0) vec4<f32> {
    let alpha = clamp(input.color.a * params.opacity, 0.0, 1.0);
    if (input.color.a <= 0.0 || alpha <= 0.0) {
        return vec4<f32>(0.0);
    }
    let unpremul_rgb = clamp(input.color.rgb / input.color.a, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(unpremul_rgb * alpha, alpha);
}
