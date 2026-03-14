struct RenderParams {
    src_layer: u32,
    src_x: u32,
    src_y: u32,
    origin_layer: u32,
    origin_x: u32,
    origin_y: u32,
    has_origin: u32,
    has_tint: u32,
    tint_r: f32,
    tint_g: f32,
    tint_b: f32,
    opacity: f32,
}

const THICKNESS_ALPHA_BOOST: f32 = 1.05;

fn full_opacity_thickness() -> f32 {
    return -log(1.0 - 1.0 / THICKNESS_ALPHA_BOOST);
}

@group(0) @binding(0) var src_texture: texture_2d_array<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var<uniform> params: RenderParams;
@group(0) @binding(3) var origin_texture: texture_2d_array<f32>;

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
    let thickness = max(color.a, 0.0);
    var alpha = min(THICKNESS_ALPHA_BOOST * (1.0 - exp(-thickness)), 1.0);
    if (thickness >= full_opacity_thickness()) {
        alpha = 1.0;
    }
    alpha = alpha * clamp(params.opacity, 0.0, 1.0);
    if (params.has_tint != 0u) {
        let tint = vec3<f32>(params.tint_r, params.tint_g, params.tint_b);
        return vec4<f32>(tint * alpha, alpha);
    }
    return vec4<f32>(color.rgb * alpha, alpha);
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
    if (params.has_tint != 0u) {
        let tint = vec3<f32>(params.tint_r, params.tint_g, params.tint_b);
        return vec4<f32>(tint, alpha);
    }
    return vec4<f32>(unpremul_rgb, alpha);
}

@fragment
fn fs_image_normal(input: VertexOutput) -> @location(0) vec4<f32> {
    let local = vec2<i32>(
        i32(input.position.x) % 64,
        i32(input.position.y) % 64,
    );
    let texel = vec2<i32>(
        i32(params.src_x) + local.x,
        i32(params.src_y) + local.y,
    );
    let color = textureLoad(src_texture, texel, i32(params.src_layer), 0);
    let alpha = clamp(color.a * params.opacity, 0.0, 1.0);
    if (params.has_tint != 0u) {
        let tint = vec3<f32>(params.tint_r, params.tint_g, params.tint_b);
        return vec4<f32>(tint * alpha, alpha);
    }
    return vec4<f32>(color.rgb * params.opacity, alpha);
}

@fragment
fn fs_image_multiply(input: VertexOutput) -> @location(0) vec4<f32> {
    let local = vec2<i32>(
        i32(input.position.x) % 64,
        i32(input.position.y) % 64,
    );
    let texel = vec2<i32>(
        i32(params.src_x) + local.x,
        i32(params.src_y) + local.y,
    );
    let color = textureLoad(src_texture, texel, i32(params.src_layer), 0);
    let alpha = clamp(color.a * params.opacity, 0.0, 1.0);
    if (color.a <= 0.0 || alpha <= 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    let unpremul_rgb = clamp(color.rgb / color.a, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(unpremul_rgb, alpha);
}

@fragment
fn fs_erase(input: VertexOutput) -> @location(0) vec4<f32> {
    let local = vec2<i32>(
        i32(input.position.x) % 64,
        i32(input.position.y) % 64,
    );
    let mask_texel = vec2<i32>(
        i32(params.src_x) + local.x,
        i32(params.src_y) + local.y,
    );
    let mask = textureLoad(src_texture, mask_texel, i32(params.src_layer), 0);
    let erase_alpha = clamp(mask.a * params.opacity, 0.0, 1.0);
    if (params.has_origin == 0u) {
        return vec4<f32>(0.0);
    }
    let origin_texel = vec2<i32>(
        i32(params.origin_x) + local.x,
        i32(params.origin_y) + local.y,
    );
    let origin = textureLoad(origin_texture, origin_texel, i32(params.origin_layer), 0);
    let out_a = max(origin.a - erase_alpha, 0.0);
    if (origin.a <= 0.0 || out_a <= 0.0) {
        return vec4<f32>(0.0);
    }
    let scale = out_a / origin.a;
    return vec4<f32>(origin.rgb * scale, out_a);
}
