struct CompositeParams {
    base_x: u32,
    base_y: u32,
    overlay_x: u32,
    overlay_y: u32,
    opacity: f32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var base_texture: texture_2d<f32>;
@group(0) @binding(1) var overlay_texture: texture_2d<f32>;
@group(0) @binding(2) var<uniform> params: CompositeParams;

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
fn fs_composite_normal(input: VertexOutput) -> @location(0) vec4<f32> {
    let local = vec2<i32>(
        i32(input.position.x) % 64,
        i32(input.position.y) % 64,
    );
    let base_texel = vec2<i32>(
        i32(params.base_x) + local.x,
        i32(params.base_y) + local.y,
    );
    let overlay_texel = vec2<i32>(
        i32(params.overlay_x) + local.x,
        i32(params.overlay_y) + local.y,
    );

    let base = textureLoad(base_texture, base_texel, 0);
    let overlay = textureLoad(overlay_texture, overlay_texel, 0);
    let overlay_alpha = overlay.a * params.opacity;

    let out_rgb = overlay.rgb * params.opacity + base.rgb * (1.0 - overlay_alpha);
    let out_a = overlay_alpha + base.a * (1.0 - overlay_alpha);
    return vec4<f32>(out_rgb, out_a);
}

@fragment
fn fs_composite_multiply(input: VertexOutput) -> @location(0) vec4<f32> {
    let local = vec2<i32>(
        i32(input.position.x) % 64,
        i32(input.position.y) % 64,
    );
    let base_texel = vec2<i32>(
        i32(params.base_x) + local.x,
        i32(params.base_y) + local.y,
    );
    let overlay_texel = vec2<i32>(
        i32(params.overlay_x) + local.x,
        i32(params.overlay_y) + local.y,
    );

    let base = textureLoad(base_texture, base_texel, 0);
    let overlay = textureLoad(overlay_texture, overlay_texel, 0);
    let overlay_alpha = clamp(overlay.a * params.opacity, 0.0, 1.0);
    let overlay_rgb = overlay.rgb * params.opacity;
    if (overlay.a <= 0.0 || overlay_alpha <= 0.0) {
        return base;
    }

    let overlay_unpremul = clamp(overlay.rgb / overlay.a, vec3<f32>(0.0), vec3<f32>(1.0));
    let base_unpremul = select(
        vec3<f32>(0.0),
        clamp(base.rgb / base.a, vec3<f32>(0.0), vec3<f32>(1.0)),
        base.a > 0.0,
    );
    let blend_rgb = base_unpremul * overlay_unpremul;
    let out_rgb =
        overlay_rgb * (1.0 - base.a) + base.rgb * (1.0 - overlay_alpha) + blend_rgb * (base.a * overlay_alpha);
    let out_a = overlay_alpha + base.a * (1.0 - overlay_alpha);
    return vec4<f32>(out_rgb, out_a);
}
