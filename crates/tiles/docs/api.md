# 核心常量 (lib.rs:7-14)
- TILE_SIZE, TILE_GUTTER, TILE_STRIDE
- DEFAULT_MAX_LAYERS, TILES_PER_ROW, ATLAS_SIZE, TILES_PER_ATLAS

# 核心类型

## 数据结构
- TileKey - 瓦片键标识符
- TileSetId - 瓦片集ID
- TileAddress - 瓦片在atlases中的位置（atlas_layer, tile_index）
- TileAtlasLayout - atlases布局信息

## 错误类型
- TileAllocError - 瓦片分配错误
- TileSetError - 瓦片集操作错误
- TileIngestError / ImageIngestError - 瓦片摄取错误
- TileAtlasCreateError - atlases创建错误

## 虚拟图像与脏标记
- VirtualImage<K> - 虚拟大图像，由多个瓦片组成
- TileDirtyBitset - 瓦片脏标记位图
- TileImage - 带版本管理的瓦片图像
- TileDirtyQuery / DirtySinceResult - 脏查询结果

## GPU atlases 相关 (feature: atlas-gpu)
- TileAtlasConfig / GenericTileAtlasConfig - atlases配置
- TileAtlasFormat - 格式 (Rgba8Unorm, R32Float, R8Uint等)
- TileAtlasUsage - 使用标志位
- TileAtlasStore / GenericTileAtlasStore - 瓦片存储
- TileAtlasGpuArray / GenericTileAtlasGpuArray - GPU数组
- BrushBufferTileRegistry - 笔刷缓冲区瓦片注册表
- BrushBufferTileStore trait - 笔刷瓦片存储接口

## 合并回调 (merge_callback.rs)
- TileMergeCompletionCallback / TileMergeCompletionNotice
- TileMergeBatchAck / TileMergeAckFailure

## 合并提交 (feature: atlas-gpu, merge_submission.rs)
- MergeSubmission, MergePlanRequest, TileMergeEngine
- TileKeyMapping, AckOutcome, ReceiptState