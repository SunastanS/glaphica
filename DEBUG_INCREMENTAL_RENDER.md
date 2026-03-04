# Incremental Repaint Debugging

## Problem Description
When using incremental repaint, only one tile is displayed after drawing. When the brush moves to affect another tile, the previous tile gets cleared from the screen.

## Added Debug Logging

### 1. Screen Blitter (`crates/app/src/screen_blitter.rs`)
- Logs all tile_keys in the root image before blitting to screen
- Shows which tiles are being drawn and their atlas addresses

### 2. Main Thread Render (`crates/app/src/main_thread.rs`)
- `process_render()`: Logs dirty tracker entries and built render commands
- `present_to_screen()`: Logs root image tile_keys state
- `process_gpu_commands()`: Logs TileSlotKeyUpdate processing and resulting shared_tree state

### 3. Engine Thread (`crates/app/src/engine_thread.rs`)
- Logs leaf image tile_keys before sync
- Logs shared_tree root image tile_keys after sync

### 4. Render Executor (`crates/gpu_runtime/src/render_executor.rs`)
- Logs RenderCmd execution details
- Shows destination tile keys and source tile keys being composited
- **Dev Assertion**: Verifies all sources have matching tile_keys length with cmd.to

## Added Tests

### 1. `test_sync_tile_keys_partial_update_preserves_untouched_tiles`
**Location**: `crates/document/src/lib.rs`

**Purpose**: Verifies that `sync_tile_keys_to_flat_tree` correctly preserves untouched tiles when performing partial updates.

**Expected Behavior**: When only tile 1 is updated, tile 0 should retain its original key from the old tree.

### 2. `test_build_render_cmds_with_partial_dirty`
**Location**: `crates/document/src/shared_tree.rs`

**Purpose**: Verifies that `build_render_cmds` correctly handles partial dirty scenarios.

**Expected Behavior**: 
- When only tile 0 is dirty, RenderCmd should have `to.len() == 1` and `from[x].tile_keys.len() == 1`
- When both tiles are dirty, RenderCmd should have `to.len() == 2` and `from[x].tile_keys.len() == 2`

## Key Analysis Points

### Data Flow During Brush Stroke

1. **Engine Thread**:
   - Generates DrawOp for affected tiles
   - Allocates new tile slots via TileSlotAllocator
   - Creates TileSlotKeyUpdate with (node_id, tile_index, new_tile_key)
   - Updates leaf image: `image.set_tile_key(tile_index, new_tile_key)`
   - Syncs to shared_tree via `sync_tile_keys_to_flat_tree`

2. **Main Thread - GPU Command Processing**:
   - Executes DrawOp on GPU (writes to atlas)
   - Marks dirty: `image_dirty_tracker.mark(node_id, tile_index)`
   - Processes TileSlotKeyUpdate (updates are already applied by engine thread)

3. **Main Thread - Render** (`process_render`):
   - Builds RenderCmds from dirty tracker
   - **Key**: Only dirty tiles are included in RenderCmd
   - Executes composite to branch cache
   - Clears dirty tracker

4. **Screen Blit** (`present_to_screen`):
   - Reads shared_tree root image
   - Iterates ALL tiles (0..tile_count)
   - Blits non-EMPTY tiles to screen

### Potential Issue Locations

Based on the code analysis, the issue could be:

1. **Tile Key Lost**: Root image's tile_key for tile 0 is being reset to EMPTY
   - Check: Screen blitter logs for tile[0] key

2. **Cache Not Updated**: Branch cache tile 0 not being composited when only tile 1 is dirty
   - This is EXPECTED behavior - cache should preserve old content
   - Check: Render executor logs for what's being composited

3. **Wrong Tree Read**: Screen blitter reading stale tree version
   - Check: Engine thread sync logs vs screen blitter logs

4. **Atlas Tile Cleared**: The physical atlas tile being cleared by ClearOp
   - Check: GPU command stream for unexpected ClearOps

## Running the Debug Build

```bash
# Build with debug logs
cargo build

# Run the application
cargo run

# Watch for logs like:
# [ENGINE] Leaf image tile_keys BEFORE sync:
# [ENGINE]   tile[0]: Some(TileKey(...))
# [ENGINE]   tile[1]: Some(TileKey(...))
# [MAIN] Shared tree root image tile_keys:
# [MAIN]   tile[0]: Some(TileKey(...))
# [MAIN]   tile[1]: Some(TileKey(...))
# [BLITTER] Root image tile_keys:
# [BLITTER]   tile[0]: key=..., origin=...
# [BLITTER]   tile[1]: key=..., origin=...
```

## Expected vs Actual Behavior

### Expected (Correct Incremental Repaint)
- Frame 1: Brush on tile 0+1 → Both tiles displayed
- Frame 2: Brush moves to tile 1 only → Both tiles still displayed (tile 0 preserved)

### Reported Bug
- Frame 1: Brush on tile 0+1 → Both tiles displayed ✓
- Frame 2: Brush moves to tile 1 only → Only tile 1 displayed, tile 0 disappears ✗

## Next Steps

1. Run the application with debug logs
2. Perform a brush stroke that crosses tile boundaries
3. Examine the logs to identify where tile 0's key is lost:
   - Is it in the leaf image after sync?
   - Is it in the shared_tree after update?
   - Is it in the root image at blit time?
4. Once the loss point is identified, trace back to find the root cause
