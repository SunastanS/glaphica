struct ViewUniform {
    matrix: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> view: ViewUniform;

struct TileInstance {
    // tile position in document pixel coordinates
    document_x: f32,
    document_y: f32,
    // tile location in atlas
    atlas_layer: f32,
    atlas_u: f32,
    atlas_v: f32,
};

@group(0) @binding(1) var<storage, read> tiles: array<TileInstance>;

@group(1) @binding(0) var tile_atlas: texture_2d_array<f32>;
@group(1) @binding(1) var tile_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) atlas_layer: u32,
    @location(2) atlas_uv_origin: vec2<f32>,
};

const TILE_SIZE: f32 = 256.0;

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    let tile = tiles[instance_index];

    // quad: 2 triangles from 6 vertices
    let quad_x = array<f32, 6>(0.0, 1.0, 0.0, 1.0, 1.0, 0.0);
    let quad_y = array<f32, 6>(0.0, 0.0, 1.0, 1.0, 0.0, 1.0);
    let local_x = quad_x[vertex_index];
    let local_y = quad_y[vertex_index];

    let document_pos = vec4<f32>(
        tile.document_x + local_x * TILE_SIZE,
        tile.document_y + local_y * TILE_SIZE,
        0.0,
        1.0,
    );
    let screen_pos = view.matrix * document_pos;

    var output: VertexOutput;
    output.position = screen_pos;
    output.uv = vec2<f32>(local_x, local_y);
    output.atlas_layer = u32(tile.atlas_layer);
    output.atlas_uv_origin = vec2<f32>(tile.atlas_u, tile.atlas_v);
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let atlas_uv = input.atlas_uv_origin + input.uv * (TILE_SIZE / f32(textureDimensions(tile_atlas).x));
    return textureSampleLevel(tile_atlas, tile_sampler, atlas_uv, i32(input.atlas_layer), 0.0);
}
