pub mod driver_bridge;

use brush_execution::BrushExecutionMergeFeedback;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use document::{Document, DocumentMergeError, MergeDirtyHint, TileCoordinate, TileReplacement};
use render_protocol::{
    BlendMode, BrushControlAck, BrushControlCommand, BrushRenderCommand, BufferTileCoordinate,
    ImageHandle, ReceiptTerminalState, RenderOp, RenderStepSupportMatrix, StrokeExecutionReceiptId,
    Viewport,
};
use renderer::{
    BrushControlError, BrushRenderEnqueueError, FrameGpuTimingReport, MergeAckError,
    MergeCompletionNotice, MergeFinalizeError, MergePollError, MergeSubmitError, PresentError,
    RenderDataResolver, Renderer, ViewOpSender,
};
use tiles::{
    MergeAuditRecord, MergePlanRequest, MergePlanTileOp, TileAddress, TileAtlasStore, TileKey,
    TileMergeCompletionNoticeId, TileMergeEngine, TileMergeError, TilesBusinessResult,
};
use view::ViewTransform;
use winit::dpi::PhysicalSize;
use winit::window::Window;

const DEFAULT_DOCUMENT_WIDTH: u32 = 1280;
const DEFAULT_DOCUMENT_HEIGHT: u32 = 720;

struct DocumentRenderDataResolver {
    document: Arc<RwLock<Document>>,
    atlas_store: Arc<TileAtlasStore>,
}

impl RenderDataResolver for DocumentRenderDataResolver {
    fn document_size(&self) -> (u32, u32) {
        let document = self
            .document
            .read()
            .unwrap_or_else(|_| panic!("document read lock poisoned"));
        (document.size_x(), document.size_y())
    }

    fn visit_image_tiles(
        &self,
        image_handle: ImageHandle,
        visitor: &mut dyn FnMut(u32, u32, TileKey),
    ) {
        let document = self
            .document
            .read()
            .unwrap_or_else(|_| panic!("document read lock poisoned"));
        let Some(image) = document.image(image_handle) else {
            return;
        };

        for (tile_x, tile_y, tile_key) in image.iter_tiles() {
            visitor(tile_x, tile_y, *tile_key);
        }
    }

    fn visit_image_tiles_for_coords(
        &self,
        image_handle: ImageHandle,
        tile_coords: &[(u32, u32)],
        visitor: &mut dyn FnMut(u32, u32, TileKey),
    ) {
        let document = self
            .document
            .read()
            .unwrap_or_else(|_| panic!("document read lock poisoned"));
        let Some(image) = document.image(image_handle) else {
            return;
        };

        for (tile_x, tile_y) in tile_coords {
            let tile_key = image
                .get_tile(*tile_x, *tile_y)
                .unwrap_or_else(|error| panic!("tile coordinate lookup failed: {error:?}"));
            let Some(tile_key) = tile_key else {
                continue;
            };
            visitor(*tile_x, *tile_y, *tile_key);
        }
    }

    fn resolve_tile_address(&self, tile_key: TileKey) -> Option<TileAddress> {
        self.atlas_store.resolve(tile_key)
    }
}

pub struct GpuState {
    renderer: Renderer,
    view_sender: ViewOpSender,
    atlas_store: Arc<TileAtlasStore>,
    tile_merge_engine: TileMergeEngine<Arc<TileAtlasStore>>,
    document: Arc<RwLock<Document>>,
    view_transform: ViewTransform,
    surface_size: PhysicalSize<u32>,
    next_frame_id: u64,
    brush_execution_feedback_queue: VecDeque<BrushExecutionMergeFeedback>,
    brush_buffer_tile_keys: HashMap<u64, HashMap<BufferTileCoordinate, TileKey>>,
    gc_evicted_batches_total: u64,
    gc_evicted_keys_total: u64,
    stroke_gc_capability: HashMap<u64, StrokeGcCapability>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrokeGcCapability {
    Retained,
    Evicted,
}

#[derive(Debug)]
pub enum MergeBridgeError {
    RendererPoll(MergePollError),
    RendererAck(MergeAckError),
    RendererSubmit(MergeSubmitError),
    RendererFinalize(MergeFinalizeError),
    Tiles(TileMergeError),
    Document(DocumentMergeError),
    MissingRendererNotice {
        receipt_id: StrokeExecutionReceiptId,
        notice_id: TileMergeCompletionNoticeId,
    },
}

impl GpuState {
    fn enqueue_stroke_merge_submission(&mut self, stroke_session_id: u64, layer_id: u64) {
        let stroke_tile_keys = self
            .brush_buffer_tile_keys
            .get(&stroke_session_id)
            .unwrap_or_else(|| {
                panic!(
                    "merge requested for stroke {} without buffer tile mapping",
                    stroke_session_id
                )
            });
        let document = self
            .document
            .read()
            .unwrap_or_else(|_| panic!("document read lock poisoned"));
        let mut tile_ops = Vec::with_capacity(stroke_tile_keys.len());
        for (index, (tile_coordinate, stroke_buffer_key)) in stroke_tile_keys.iter().enumerate() {
            if tile_coordinate.tile_x < 0 || tile_coordinate.tile_y < 0 {
                continue;
            }
            let tile_x = u32::try_from(tile_coordinate.tile_x)
                .expect("positive brush tile x must convert to u32");
            let tile_y = u32::try_from(tile_coordinate.tile_y)
                .expect("positive brush tile y must convert to u32");
            let existing_layer_key = document.leaf_tile_key_at(layer_id, tile_x, tile_y);
            tile_ops.push(MergePlanTileOp {
                tile_x,
                tile_y,
                existing_layer_key,
                stroke_buffer_key: *stroke_buffer_key,
                blend_mode: BlendMode::Normal,
                opacity: 1.0,
                op_trace_id: u64::try_from(index).expect("merge op index exceeds u64"),
            });
        }
        drop(document);
        if tile_ops.is_empty() {
            self.brush_execution_feedback_queue
                .push_back(BrushExecutionMergeFeedback::MergeApplied { stroke_session_id });
            return;
        }
        let request = MergePlanRequest {
            stroke_session_id,
            tx_token: stroke_session_id,
            program_revision: None,
            layer_id,
            tile_ops,
        };
        let submission = self
            .tile_merge_engine
            .submit_merge_plan(request)
            .unwrap_or_else(|error| panic!("submit merge plan failed: {error:?}"));
        self.renderer
            .enqueue_planned_merge(
                submission.renderer_submit_payload.receipt,
                submission.renderer_submit_payload.gpu_merge_ops,
                submission.renderer_submit_payload.meta,
            )
            .unwrap_or_else(|error| panic!("enqueue planned merge failed: {error:?}"));
    }

    pub async fn new(window: Arc<Window>, startup_image_path: Option<PathBuf>) -> Self {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = instance
            .create_surface(window.clone())
            .expect("create wgpu surface");

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("request wgpu adapter");

        let limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: None,
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("request wgpu device");

        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let mut size = window.inner_size();
        size.width = size.width.max(1);
        size.height = size.height.max(1);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width,
            height: size.height,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        let (atlas_store, tile_atlas) = TileAtlasStore::new(
            &device,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
        )
        .expect("create tile atlas store");
        let atlas_store = Arc::new(atlas_store);

        let document = create_startup_document(&atlas_store, startup_image_path.as_deref());
        let initial_snapshot = document.render_tree_snapshot(document.revision());
        initial_snapshot
            .validate_executable(&RenderStepSupportMatrix::current_executable_semantics())
            .unwrap_or_else(|error| {
                panic!(
                    "initial render steps include unsupported feature at step {}: {:?}",
                    error.step_index, error.reason
                )
            });
        let document = Arc::new(RwLock::new(document));

        let render_data_resolver = Box::new(DocumentRenderDataResolver {
            document: Arc::clone(&document),
            atlas_store: Arc::clone(&atlas_store),
        });

        let tile_merge_engine = TileMergeEngine::new(Arc::clone(&atlas_store));

        let (renderer, view_sender) = Renderer::new(
            device,
            queue,
            surface,
            config,
            tile_atlas,
            render_data_resolver,
        );

        let view_transform = ViewTransform::default();
        push_view_state(&view_sender, &view_transform, size);
        view_sender
            .send(RenderOp::BindRenderTree(initial_snapshot))
            .expect("send initial render steps");

        Self {
            renderer,
            view_sender,
            atlas_store,
            tile_merge_engine,
            document,
            view_transform,
            surface_size: size,
            next_frame_id: 0,
            brush_execution_feedback_queue: VecDeque::new(),
            brush_buffer_tile_keys: HashMap::new(),
            gc_evicted_batches_total: 0,
            gc_evicted_keys_total: 0,
            stroke_gc_capability: HashMap::new(),
        }
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        let width = new_size.width.max(1);
        let height = new_size.height.max(1);
        if self.surface_size.width == width && self.surface_size.height == height {
            return;
        }

        self.surface_size = PhysicalSize::new(width, height);
        self.renderer.resize(width, height);
        push_view_state(&self.view_sender, &self.view_transform, self.surface_size);
    }

    pub fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        self.renderer.drain_view_ops();

        let frame_id = self.next_frame_id;
        self.next_frame_id = self
            .next_frame_id
            .checked_add(1)
            .expect("frame id overflow");

        match self.renderer.present_frame(frame_id) {
            Ok(()) => Ok(()),
            Err(PresentError::Surface(error)) => Err(error),
            Err(PresentError::TileDrain(error)) => {
                panic!("tile atlas drain failed during present: {error}")
            }
        }
    }

    pub fn set_brush_command_quota(&self, max_commands: u32) {
        self.view_sender
            .send(RenderOp::SetBrushCommandQuota { max_commands })
            .expect("send brush command quota");
    }

    pub fn take_latest_gpu_timing_report(&mut self) -> Option<FrameGpuTimingReport> {
        self.renderer.take_latest_gpu_timing_report()
    }

    pub fn apply_brush_control_command(
        &mut self,
        command: BrushControlCommand,
    ) -> Result<BrushControlAck, BrushControlError> {
        self.renderer.apply_brush_control_command(command)
    }

    pub fn enqueue_brush_render_command(
        &mut self,
        command: BrushRenderCommand,
    ) -> Result<(), BrushRenderEnqueueError> {
        match command {
            BrushRenderCommand::AllocateBufferTiles(allocate) => {
                let stroke_tile_keys = self
                    .brush_buffer_tile_keys
                    .entry(allocate.stroke_session_id)
                    .or_default();
                for tile_coordinate in allocate.tiles {
                    if stroke_tile_keys.contains_key(&tile_coordinate) {
                        continue;
                    }
                    let tile_key = self.atlas_store.allocate().unwrap_or_else(|error| {
                        panic!(
                            "failed to allocate brush buffer tile for stroke {} at ({}, {}): {error}",
                            allocate.stroke_session_id, tile_coordinate.tile_x, tile_coordinate.tile_y
                        )
                    });
                    stroke_tile_keys.insert(tile_coordinate, tile_key);
                }
                self.drain_tile_gc_evictions();
                Ok(())
            }
            BrushRenderCommand::ReleaseBufferTiles(release) => {
                let stroke_tile_keys = self
                    .brush_buffer_tile_keys
                    .get_mut(&release.stroke_session_id)
                    .unwrap_or_else(|| {
                        panic!(
                            "release requested for unknown stroke {}",
                            release.stroke_session_id
                        )
                    });
                for tile_coordinate in release.tiles {
                    let tile_key = stroke_tile_keys
                        .remove(&tile_coordinate)
                        .unwrap_or_else(|| {
                            panic!(
                                "release requested for missing tile mapping: stroke {} at ({}, {})",
                                release.stroke_session_id,
                                tile_coordinate.tile_x,
                                tile_coordinate.tile_y
                            )
                        });
                    let released = self.atlas_store.release(tile_key);
                    if !released {
                        panic!(
                            "failed to release brush buffer tile for stroke {} at ({}, {})",
                            release.stroke_session_id,
                            tile_coordinate.tile_x,
                            tile_coordinate.tile_y
                        );
                    }
                }
                if stroke_tile_keys.is_empty() {
                    self.brush_buffer_tile_keys
                        .remove(&release.stroke_session_id);
                }
                Ok(())
            }
            BrushRenderCommand::MergeBuffer(merge) => {
                self.enqueue_stroke_merge_submission(
                    merge.stroke_session_id,
                    merge.target_layer_id,
                );
                self.renderer
                    .enqueue_brush_render_command(BrushRenderCommand::MergeBuffer(merge))
            }
            other => self.renderer.enqueue_brush_render_command(other),
        }
    }

    pub fn pending_brush_dab_count(&self) -> u64 {
        self.renderer.pending_brush_dab_count()
    }

    pub fn process_renderer_merge_completions(
        &mut self,
        frame_id: u64,
    ) -> Result<(), MergeBridgeError> {
        self.renderer
            .submit_pending_merges(frame_id, u32::MAX)
            .map_err(MergeBridgeError::RendererSubmit)?;
        let renderer_notices = self
            .renderer
            .poll_completion_notices(frame_id)
            .map_err(MergeBridgeError::RendererPoll)?;

        let mut renderer_notice_by_key = HashMap::new();
        for renderer_notice in renderer_notices {
            let notice_id = notice_id_from_renderer(&renderer_notice);
            let notice_key = (notice_id, renderer_notice.receipt_id);
            self.tile_merge_engine
                .on_renderer_completion_signal(
                    renderer_notice.receipt_id,
                    renderer_notice.audit_meta,
                    renderer_notice.result.clone(),
                )
                .map_err(MergeBridgeError::Tiles)?;
            let previous = renderer_notice_by_key.insert(notice_key, renderer_notice);
            assert!(
                previous.is_none(),
                "renderer poll yielded duplicate merge notice key"
            );
        }

        let completion_notices = self.tile_merge_engine.poll_submission_results();
        for notice in completion_notices {
            let notice_key = (notice.notice_id, notice.receipt_id);
            let renderer_notice = renderer_notice_by_key.remove(&notice_key).ok_or(
                MergeBridgeError::MissingRendererNotice {
                    receipt_id: notice.receipt_id,
                    notice_id: notice.notice_id,
                },
            )?;

            self.renderer
                .ack_merge_result(renderer_notice)
                .map_err(MergeBridgeError::RendererAck)?;
            self.tile_merge_engine
                .ack_merge_result(notice.receipt_id, notice.notice_id)
                .map_err(MergeBridgeError::Tiles)?;
        }

        let business_results = self.tile_merge_engine.drain_business_results();
        self.apply_tiles_business_results(&business_results)?;
        self.drain_tile_gc_evictions();
        Ok(())
    }

    pub fn drain_brush_execution_merge_feedbacks(&mut self) -> Vec<BrushExecutionMergeFeedback> {
        self.brush_execution_feedback_queue.drain(..).collect()
    }

    pub fn finalize_merge_receipt(
        &mut self,
        receipt_id: StrokeExecutionReceiptId,
    ) -> Result<(), MergeBridgeError> {
        self.renderer
            .ack_receipt_terminal_state(receipt_id, ReceiptTerminalState::Finalized)
            .map_err(MergeBridgeError::RendererFinalize)?;
        self.tile_merge_engine
            .finalize_receipt(receipt_id)
            .map_err(MergeBridgeError::Tiles)
    }

    pub fn abort_merge_receipt(
        &mut self,
        receipt_id: StrokeExecutionReceiptId,
    ) -> Result<(), MergeBridgeError> {
        self.renderer
            .ack_receipt_terminal_state(receipt_id, ReceiptTerminalState::Aborted)
            .map_err(MergeBridgeError::RendererFinalize)?;
        self.tile_merge_engine
            .abort_receipt(receipt_id)
            .map_err(MergeBridgeError::Tiles)
    }

    pub fn query_merge_audit_record(
        &self,
        receipt_id: StrokeExecutionReceiptId,
    ) -> Result<MergeAuditRecord, MergeBridgeError> {
        self.tile_merge_engine
            .query_merge_audit_record(receipt_id)
            .map_err(MergeBridgeError::Tiles)
    }

    pub fn pan_canvas(&mut self, delta_x: f32, delta_y: f32) {
        self.view_transform
            .pan_by(delta_x, delta_y)
            .unwrap_or_else(|error| panic!("pan canvas failed: {error:?}"));
        push_view_state(&self.view_sender, &self.view_transform, self.surface_size);
    }

    pub fn rotate_canvas(&mut self, delta_radians: f32) {
        self.view_transform
            .rotate_by(delta_radians)
            .unwrap_or_else(|error| panic!("rotate canvas failed: {error:?}"));
        push_view_state(&self.view_sender, &self.view_transform, self.surface_size);
    }

    pub fn zoom_canvas_about_viewport_point(
        &mut self,
        zoom_factor: f32,
        viewport_x: f32,
        viewport_y: f32,
    ) {
        self.view_transform
            .zoom_about_point(zoom_factor, viewport_x, viewport_y)
            .unwrap_or_else(|error| panic!("zoom canvas failed: {error:?}"));
        push_view_state(&self.view_sender, &self.view_transform, self.surface_size);
    }

    fn apply_tiles_business_results(
        &mut self,
        business_results: &[TilesBusinessResult],
    ) -> Result<(), MergeBridgeError> {
        for result in business_results {
            match result {
                TilesBusinessResult::CanFinalize {
                    receipt_id,
                    stroke_session_id,
                    layer_id,
                    new_key_mappings,
                    ..
                } => {
                    let document_apply_result: Result<(), MergeBridgeError> = (|| {
                        let mut document = self
                            .document
                            .write()
                            .unwrap_or_else(|_| panic!("document write lock poisoned"));
                        let expected_revision = document.revision();
                        document
                            .begin_merge(*layer_id, *stroke_session_id, expected_revision)
                            .map_err(MergeBridgeError::Document)?;
                        let replacements: Vec<TileReplacement> = new_key_mappings
                            .iter()
                            .map(|mapping| TileReplacement {
                                tile_x: mapping.tile_x,
                                tile_y: mapping.tile_y,
                                new_key: mapping.new_key,
                            })
                            .collect();
                        let dirty_tiles: Vec<TileCoordinate> = replacements
                            .iter()
                            .map(|replacement| TileCoordinate {
                                tile_x: replacement.tile_x,
                                tile_y: replacement.tile_y,
                            })
                            .collect();
                        document
                            .commit_tile_replacements(*layer_id, &replacements)
                            .map_err(MergeBridgeError::Document)?;
                        let summary = document
                            .finalize_merge(
                                *layer_id,
                                *stroke_session_id,
                                MergeDirtyHint::Tiles(dirty_tiles),
                            )
                            .map_err(MergeBridgeError::Document)?;
                        if document.take_render_tree_cache_dirty() {
                            let render_tree = document.render_tree_snapshot(summary.revision);
                            self.view_sender
                                .send(RenderOp::BindRenderTree(render_tree))
                                .expect("send updated render tree after merge");
                        }
                        let _dirty_layers = document.take_dirty_layers();
                        Ok(())
                    })(
                    );
                    if let Err(error) = document_apply_result {
                        self.brush_execution_feedback_queue.push_back(
                            BrushExecutionMergeFeedback::MergeFailed {
                                stroke_session_id: *stroke_session_id,
                                message: format!("document merge apply failed: {error:?}"),
                            },
                        );
                        return Err(error);
                    }
                    if let Err(error) = self.finalize_merge_receipt(*receipt_id) {
                        self.brush_execution_feedback_queue.push_back(
                            BrushExecutionMergeFeedback::MergeFailed {
                                stroke_session_id: *stroke_session_id,
                                message: format!("finalize merge receipt failed: {error:?}"),
                            },
                        );
                        return Err(error);
                    }
                    self.mark_stroke_gc_retained(*stroke_session_id);
                    self.brush_execution_feedback_queue.push_back(
                        BrushExecutionMergeFeedback::MergeApplied {
                            stroke_session_id: *stroke_session_id,
                        },
                    );
                }
                TilesBusinessResult::RequiresAbort {
                    receipt_id,
                    stroke_session_id,
                    layer_id,
                    ..
                } => {
                    let document_abort_result: Result<(), MergeBridgeError> = (|| {
                        let mut document = self
                            .document
                            .write()
                            .unwrap_or_else(|_| panic!("document write lock poisoned"));
                        if document.has_active_merge(*layer_id, *stroke_session_id) {
                            document
                                .abort_merge(*layer_id, *stroke_session_id)
                                .map_err(MergeBridgeError::Document)?;
                        }
                        Ok(())
                    })(
                    );
                    if let Err(error) = document_abort_result {
                        self.brush_execution_feedback_queue.push_back(
                            BrushExecutionMergeFeedback::MergeFailed {
                                stroke_session_id: *stroke_session_id,
                                message: format!("document merge abort failed: {error:?}"),
                            },
                        );
                        return Err(error);
                    }
                    if let Err(error) = self.abort_merge_receipt(*receipt_id) {
                        self.brush_execution_feedback_queue.push_back(
                            BrushExecutionMergeFeedback::MergeFailed {
                                stroke_session_id: *stroke_session_id,
                                message: format!("abort merge receipt failed: {error:?}"),
                            },
                        );
                        return Err(error);
                    }
                    self.brush_execution_feedback_queue.push_back(
                        BrushExecutionMergeFeedback::MergeFailed {
                            stroke_session_id: *stroke_session_id,
                            message: "merge requires abort".to_owned(),
                        },
                    );
                }
            }
        }
        Ok(())
    }

    fn drain_tile_gc_evictions(&mut self) {
        let evicted_batches = self.atlas_store.drain_evicted_retain_batches();
        for evicted_batch in evicted_batches {
            self.apply_gc_evicted_batch(evicted_batch.retain_id, evicted_batch.keys.len());
        }
    }

    fn mark_stroke_gc_retained(&mut self, stroke_session_id: u64) {
        self.stroke_gc_capability
            .insert(stroke_session_id, StrokeGcCapability::Retained);
    }

    fn apply_gc_evicted_batch(&mut self, retain_id: u64, key_count: usize) {
        apply_gc_evicted_batch_state(
            &mut self.gc_evicted_batches_total,
            &mut self.gc_evicted_keys_total,
            &mut self.stroke_gc_capability,
            retain_id,
            key_count,
        );
        eprintln!(
            "tiles gc evicted retain batch: retain_id={} key_count={} total_batches={} total_keys={}",
            retain_id, key_count, self.gc_evicted_batches_total, self.gc_evicted_keys_total
        );
    }
}

fn create_startup_document(
    atlas_store: &TileAtlasStore,
    startup_image_path: Option<&Path>,
) -> Document {
    let Some(startup_image_path) = startup_image_path else {
        return Document::new(DEFAULT_DOCUMENT_WIDTH, DEFAULT_DOCUMENT_HEIGHT);
    };

    let decoded = image::ImageReader::open(startup_image_path)
        .unwrap_or_else(|error| {
            panic!(
                "failed to open startup image at {}: {error}",
                startup_image_path.display()
            )
        })
        .decode()
        .unwrap_or_else(|error| {
            panic!(
                "failed to decode startup image at {}: {error}",
                startup_image_path.display()
            )
        })
        .to_rgba8();

    let size_x = decoded.width();
    let size_y = decoded.height();
    let image_bytes = decoded.into_raw();

    let image = atlas_store
        .ingest_image_rgba8_strided(size_x, size_y, &image_bytes, size_x * 4)
        .unwrap_or_else(|error| {
            panic!(
                "failed to ingest startup image into tile atlas at {}: {error:?}",
                startup_image_path.display()
            )
        });

    let mut document = Document::new(size_x, size_y);
    let _layer_id = document.new_layer_root_with_image(image, BlendMode::Normal);
    document
}

fn push_view_state(
    view_sender: &ViewOpSender,
    view_transform: &ViewTransform,
    size: PhysicalSize<u32>,
) {
    view_sender
        .send(RenderOp::SetViewport(Viewport {
            origin_x: 0,
            origin_y: 0,
            width: size.width,
            height: size.height,
        }))
        .expect("send viewport");

    let matrix = view_transform
        .to_clip_matrix4x4(size.width as f32, size.height as f32)
        .expect("build clip matrix");
    view_sender
        .send(RenderOp::SetViewTransform { matrix })
        .expect("send view transform");
}

fn notice_id_from_renderer(notice: &MergeCompletionNotice) -> TileMergeCompletionNoticeId {
    TileMergeCompletionNoticeId {
        renderer_submission_id: notice.audit_meta.renderer_submission_id,
        frame_id: notice.audit_meta.frame_id,
        op_trace_id: notice.audit_meta.op_trace_id,
    }
}

fn apply_gc_evicted_batch_state(
    gc_evicted_batches_total: &mut u64,
    gc_evicted_keys_total: &mut u64,
    stroke_gc_capability: &mut HashMap<u64, StrokeGcCapability>,
    retain_id: u64,
    key_count: usize,
) {
    *gc_evicted_batches_total = gc_evicted_batches_total
        .checked_add(1)
        .expect("gc evicted batch counter overflow");
    *gc_evicted_keys_total = gc_evicted_keys_total
        .checked_add(u64::try_from(key_count).expect("gc key count exceeds u64"))
        .expect("gc evicted key counter overflow");
    stroke_gc_capability.insert(retain_id, StrokeGcCapability::Evicted);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_device_queue() -> (wgpu::Device, wgpu::Queue) {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends: wgpu::Backends::all(),
                ..Default::default()
            });
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: true,
                })
                .await
                .expect("request test adapter");
            adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("glaphica tests"),
                    required_features: wgpu::Features::empty(),
                    required_limits: adapter.limits(),
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                    memory_hints: wgpu::MemoryHints::Performance,
                    trace: wgpu::Trace::Off,
                })
                .await
                .expect("request test device")
        })
    }

    fn read_tile_rgba8(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        atlas_layout: tiles::TileAtlasLayout,
        address: TileAddress,
    ) -> Vec<u8> {
        let buffer_size = (tiles::TILE_SIZE as u64) * (tiles::TILE_SIZE as u64) * 4;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica.tests.readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glaphica.tests.readback"),
        });
        let (origin_x, origin_y) = address.atlas_content_origin_pixels_in(atlas_layout);
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: origin_x,
                    y: origin_y,
                    z: address.atlas_layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(tiles::TILE_SIZE * 4),
                    rows_per_image: Some(tiles::TILE_SIZE),
                },
            },
            wgpu::Extent3d {
                width: tiles::TILE_SIZE,
                height: tiles::TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).expect("map callback send");
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll");
        receiver
            .recv()
            .expect("map callback recv")
            .expect("map tile readback");
        let tile = slice.get_mapped_range().to_vec();
        buffer.unmap();
        tile
    }

    #[test]
    fn image_from_tests_resources_round_trips_through_document_and_gpu_atlas() {
        let image_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/resources/document_import_e2e.png");
        let decoded = image::ImageReader::open(&image_path)
            .expect("open e2e source image")
            .decode()
            .expect("decode e2e source image")
            .to_rgba8();
        let size_x = decoded.width();
        let size_y = decoded.height();
        let source_bytes = decoded.into_raw();

        let (device, queue) = create_device_queue();
        let (atlas_store, atlas_gpu) = tiles::TileAtlasStore::new(
            &device,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
        )
        .expect("create tile atlas store");

        let virtual_image = atlas_store
            .ingest_image_rgba8_strided(size_x, size_y, &source_bytes, size_x * 4)
            .expect("ingest source image into tile atlas");
        atlas_gpu
            .drain_and_execute(&queue)
            .expect("flush tile uploads to gpu atlas");
        let atlas_layout = atlas_gpu.layout();

        let mut document = Document::new(size_x, size_y);
        let _layer_id = document.new_layer_root_with_image(virtual_image, BlendMode::Normal);
        let snapshot = document.render_tree_snapshot(1);
        let image_handle = find_first_leaf_image_handle(snapshot.root.as_ref())
            .expect("snapshot should contain a leaf image");
        let document_image = document
            .image(image_handle)
            .expect("snapshot leaf image handle should resolve");

        let rendered_bytes = document_image
            .export_rgba8(|tile_key| {
                let address = atlas_store.resolve(*tile_key)?;
                Some(read_tile_rgba8(
                    &device,
                    &queue,
                    atlas_gpu.texture(),
                    atlas_layout,
                    address,
                ))
            })
            .expect("export rendered image from document tiles");

        assert_eq!(rendered_bytes, source_bytes);
    }

    #[test]
    fn apply_gc_evicted_batch_state_updates_counters_and_marks_evicted() {
        let mut gc_evicted_batches_total = 0u64;
        let mut gc_evicted_keys_total = 0u64;
        let mut stroke_gc_capability = HashMap::new();
        stroke_gc_capability.insert(42, StrokeGcCapability::Retained);

        apply_gc_evicted_batch_state(
            &mut gc_evicted_batches_total,
            &mut gc_evicted_keys_total,
            &mut stroke_gc_capability,
            42,
            3,
        );
        apply_gc_evicted_batch_state(
            &mut gc_evicted_batches_total,
            &mut gc_evicted_keys_total,
            &mut stroke_gc_capability,
            42,
            2,
        );

        assert_eq!(gc_evicted_batches_total, 2);
        assert_eq!(gc_evicted_keys_total, 5);
        assert_eq!(
            stroke_gc_capability.get(&42),
            Some(&StrokeGcCapability::Evicted)
        );
    }

    #[test]
    fn apply_gc_evicted_batch_state_keeps_empty_batch_accounting() {
        let mut gc_evicted_batches_total = 0u64;
        let mut gc_evicted_keys_total = 0u64;
        let mut stroke_gc_capability = HashMap::new();

        apply_gc_evicted_batch_state(
            &mut gc_evicted_batches_total,
            &mut gc_evicted_keys_total,
            &mut stroke_gc_capability,
            100,
            0,
        );

        assert_eq!(gc_evicted_batches_total, 1);
        assert_eq!(gc_evicted_keys_total, 0);
        assert_eq!(
            stroke_gc_capability.get(&100),
            Some(&StrokeGcCapability::Evicted)
        );
    }

    fn find_first_leaf_image_handle(
        node: &render_protocol::RenderNodeSnapshot,
    ) -> Option<ImageHandle> {
        match node {
            render_protocol::RenderNodeSnapshot::Leaf { image_handle, .. } => Some(*image_handle),
            render_protocol::RenderNodeSnapshot::Group { children, .. } => {
                children.iter().find_map(find_first_leaf_image_handle)
            }
        }
    }
}
