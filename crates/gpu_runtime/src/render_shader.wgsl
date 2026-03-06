struct RenderParams {
    src_layer: u32,
    src_x: u32,
    src_y: u32,
    opacity: f32,
}

@group(0) @binding(0) var src_texture: texture_2d_array<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var<uniform> params: RenderParams;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var output: VertexOutput;
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let xy = positions[vertex_index];
    output.position = vec4<f32>(xy, 0.0, 1.0);
    return output;
}

@fragment
fn fs_normal(input: VertexOutput) -> @location(0) vec4<f32> {
    let local = vec2<i32>(
        i32(input.position.x) % 64,
        i32(input.position.y) % 64,
    );
    let texel = vec2<i32>(
        i32(params.src_x) + local.x,
        i32(params.src_y) + local.y,
    );
    let color = textureLoad(src_texture, texel, i32(params.src_layer), 0);
    let alpha = color.a * params.opacity;
    return vec4<f32>(color.rgb * params.opacity, alpha);
}

@fragment
fn fs_multiply(input: VertexOutput) -> @location(0) vec4<f32> {
    let local = vec2<i32>(
        i32(input.position.x) % 64,
        i32(input.position.y) % 64,
    );
    let texel = vec2<i32>(
        i32(params.src_x) + local.x,
        i32(params.src_y) + local.y,
    );
    let color = textureLoad(src_texture, texel, i32(params.src_layer), 0);
    let alpha = color.a * params.opacity;
    if (color.a <= 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    let unpremul_rgb = clamp(color.rgb / color.a, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(unpremul_rgb, alpha);
}
