struct ViewUniform {
    matrix: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> view: ViewUniform;

struct TileInstance {
    document_x: f32,
    document_y: f32,
    atlas_layer: f32,
    tile_index: u32,
    _padding0: u32,
};

@group(0) @binding(1) var<storage, read> tiles: array<TileInstance>;

@group(1) @binding(0) var tile_atlas: texture_2d_array<f32>;
@group(1) @binding(1) var tile_sampler: sampler;

struct TileTextureManager {
    atlas_width: f32,
    atlas_height: f32,
    tiles_per_row: u32,
    _padding0: u32,
};

@group(1) @binding(2) var<uniform> tile_texture_manager: TileTextureManager;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) atlas_layer: u32,
    @location(2) @interpolate(flat) tile_index: u32,
};

const TILE_SIZE: f32 = 256.0;
const TILE_GUTTER: f32 = 1.0;
const TILE_STRIDE: f32 = TILE_SIZE + TILE_GUTTER * 2.0;

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    let tile = tiles[instance_index];

    let quad_x = array<f32, 6>(0.0, 1.0, 0.0, 1.0, 1.0, 0.0);
    let quad_y = array<f32, 6>(0.0, 0.0, 1.0, 1.0, 0.0, 1.0);
    let local_x = quad_x[vertex_index];
    let local_y = quad_y[vertex_index];

    let tile_index_x = floor(tile.document_x / TILE_SIZE + 0.5);
    let tile_index_y = floor(tile.document_y / TILE_SIZE + 0.5);
    let slot_origin_x = tile_index_x * TILE_STRIDE;
    let slot_origin_y = tile_index_y * TILE_STRIDE;
    let document_pos = vec4<f32>(
        slot_origin_x + local_x * TILE_STRIDE,
        slot_origin_y + local_y * TILE_STRIDE,
        0.0,
        1.0,
    );
    let screen_pos = view.matrix * document_pos;

    var output: VertexOutput;
    output.position = screen_pos;
    output.uv = vec2<f32>(local_x, local_y);
    output.atlas_layer = u32(tile.atlas_layer);
    output.tile_index = tile.tile_index;
    return output;
}

fn tile_slot_uv_origin(tile_index: u32) -> vec2<f32> {
    let tile_x = tile_index % tile_texture_manager.tiles_per_row;
    let tile_y = tile_index / tile_texture_manager.tiles_per_row;
    let slot_origin = vec2<f32>(f32(tile_x) * TILE_STRIDE, f32(tile_y) * TILE_STRIDE);
    return slot_origin
        / vec2<f32>(tile_texture_manager.atlas_width, tile_texture_manager.atlas_height);
}

fn sample_tile_slot(tile_layer: i32, tile_index: u32, slot_uv: vec2<f32>) -> vec4<f32> {
    let atlas_uv = tile_slot_uv_origin(tile_index)
        + slot_uv * (vec2<f32>(TILE_STRIDE, TILE_STRIDE)
            / vec2<f32>(tile_texture_manager.atlas_width, tile_texture_manager.atlas_height));
    return textureSampleLevel(tile_atlas, tile_sampler, atlas_uv, tile_layer, 0.0);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return sample_tile_slot(i32(input.atlas_layer), input.tile_index, input.uv);
}
