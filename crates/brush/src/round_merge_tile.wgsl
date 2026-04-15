struct MergeUniforms {
    origin_origin: vec2u,
    origin_layer: u32,
    _pad0: u32,
    intermediate_origin: vec2u,
    intermediate_layer: u32,
    _pad1: u32,
    tint: vec3f,
    _pad2: f32,
};

@group(0) @binding(0) var atlas_texture: texture_2d_array<f32>;
@group(0) @binding(1) var<uniform> uniforms: MergeUniforms;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(positions[vertex_index], 0.0, 1.0);
}

@fragment
fn fs_merge_tile(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let pixel = vec2u(pos.xy);
    let base = textureLoad(
        atlas_texture,
        vec2i(uniforms.origin_origin + pixel),
        i32(uniforms.origin_layer),
        0
    );
    let intermediate = textureLoad(
        atlas_texture,
        vec2i(uniforms.intermediate_origin + pixel),
        i32(uniforms.intermediate_layer),
        0
    );
    let effective_alpha = 1.0 - exp(-max(intermediate.a, 0.0));
    let out_alpha = base.a + (1.0 - base.a) * effective_alpha;
    let out_rgb = mix(base.rgb, uniforms.tint, effective_alpha);
    return vec4f(out_rgb, out_alpha);
}
