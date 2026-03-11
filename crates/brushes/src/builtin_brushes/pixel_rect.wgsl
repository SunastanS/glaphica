struct DrawInput {
    center_local_x: f32,
    center_local_y: f32,
    radius_px: f32,
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
    _pad0: f32,
}

@group(0) @binding(0) var<storage, read> draw_input: DrawInput;
@group(0) @binding(1) var<uniform> params: ShaderParams;

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
    // Brush input is encoded in image-tile coordinates (62x62), while the atlas tile
    // includes a 1px gutter on each side (64x64). Map atlas pixel space to image-local.
    let tile_local_x = pos.x - f32(params.tile_origin_x) - 1.0;
    let tile_local_y = pos.y - f32(params.tile_origin_y) - 1.0;
    
    let center_x = draw_input.center_local_x;
    let center_y = draw_input.center_local_y;
    let half_size = draw_input.radius_px;
    
    let dx = tile_local_x - center_x;
    let dy = tile_local_y - center_y;
    let tint = vec3<f32>(params.tint_r, params.tint_g, params.tint_b);
    
    if (abs(dx) <= half_size && abs(dy) <= half_size) {
        if (params.erase != 0u) {
            return vec4<f32>(0.0);
        }
        return vec4<f32>(tint, 1.0);
    }
    discard;
}
