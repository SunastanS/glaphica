struct RenderParams {
    src_layer: u32,
    src_x: u32,
    src_y: u32,
    opacity: f32,
}

@group(0) @binding(0) var src_texture: texture_2d_array;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var<uniform> params: RenderParams;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var output: VertexOutput;
    let x = f32(vertex_index & 1u) * 2.0 - 1.0;
    let y = f32(vertex_index >> 1u) * 2.0 - 1.0;
    output.position = vec4<f32>(x, -y, 0.0, 1.0);
    output.uv = vec2<f32>((x + 1.0) * 0.5, (y + 1.0) * 0.5);
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel_size = 1.0 / f32(64u);
    let uv = vec2<f32>(
        f32(params.src_x) * texel_size + input.uv.x * texel_size,
        f32(params.src_y) * texel_size + input.uv.y * texel_size,
    );
    let color = textureSample(src_texture, src_sampler, uv, params.src_layer);
    return vec4<f32>(color.rgb * params.opacity, color.a * params.opacity);
}