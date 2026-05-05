use std::time::Duration;

use brush::round::RoundBrushSettings;
use gla_document::{GlaDoc, GlaDocError, GlaNodeId};
use renderer::RenderTarget2d;
use ui::{UiAction, UiLayerItem, UiTraceMode, UiTraceStatus};

use crate::{
    AppPresentError, AppRuntimeError, EditorSessionError, ScreenPresentTileError, SurfaceRuntime,
    present_root_tiles,
};

use super::{
    MAX_PENDING_BRUSH_INPUTS_PER_FRAME, PreviewRuntimeError, PreviewState,
    app_bootstrap::{env_flag, env_millis},
    trace::{
        PreviewTraceBlendMode, PreviewTraceEvent, PreviewTraceMode, PreviewTraceRoundBrushSettings,
        PreviewTraceUiAction,
    },
};

const DEFAULT_BACKGROUND_COLOR: wgpu::Color = wgpu::Color {
    r: 0.12,
    g: 0.12,
    b: 0.12,
    a: 1.0,
};

#[derive(Debug, Clone, Copy)]
pub(super) struct PreviewPerfTraceConfig {
    puffin_enabled: bool,
    stderr_enabled: bool,
    http_enabled: bool,
    slow_threshold: Duration,
}

impl PreviewPerfTraceConfig {
    pub(super) fn from_env() -> Self {
        let puffin_enabled = env_flag("GLAPHICA_PREVIEW_PERF_TRACE");
        let stderr_enabled = env_flag("GLAPHICA_PREVIEW_PERF_TRACE_STDERR");
        let http_enabled = env_flag("GLAPHICA_PREVIEW_PERF_TRACE_HTTP");
        let slow_threshold = env_millis("GLAPHICA_PREVIEW_PERF_TRACE_SLOW_MS")
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_millis(8));
        Self {
            puffin_enabled,
            stderr_enabled,
            http_enabled,
            slow_threshold,
        }
    }

    pub(super) fn puffin_enabled(self) -> bool {
        self.puffin_enabled || self.http_enabled
    }

    pub(super) fn http_enabled(self) -> bool {
        self.http_enabled
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct PreviewFramePerf {
    process_inputs: Duration,
    update_cache: Duration,
    acquire_frame: Duration,
    present_surface: Duration,
    dirty_slot_count: usize,
}

impl PreviewState {
    pub(super) fn redraw(&mut self) -> Result<(), PreviewRuntimeError> {
        if self.perf_trace.puffin_enabled {
            puffin::GlobalProfiler::lock().new_frame();
        }
        puffin::profile_function!();

        let frame_started = std::time::Instant::now();
        let mut perf = PreviewFramePerf::default();

        let process_inputs_started = std::time::Instant::now();
        {
            let Some(runtime) = self.runtime.as_mut() else {
                return Ok(());
            };
            puffin::profile_scope!("process_pending_brush_input_gpu");
            runtime.process_pending_brush_input_gpu(
                &mut self.tile_renderer,
                &self.gpu.device,
                &self.gpu.queue,
                MAX_PENDING_BRUSH_INPUTS_PER_FRAME,
            )?;
        }
        perf.process_inputs = process_inputs_started.elapsed();

        let document_size = self
            .runtime
            .as_ref()
            .map(|runtime| {
                let layout = runtime.session().doc().layout();
                [layout.size_x(), layout.size_y()]
            })
            .unwrap_or([0, 0]);
        let layers = self
            .runtime
            .as_ref()
            .map(|runtime| collect_ui_layers(runtime.session().doc()))
            .transpose()
            .map_err(|error| {
                PreviewRuntimeError::Runtime(AppRuntimeError::Session(
                    EditorSessionError::Document(error),
                ))
            })?
            .unwrap_or_default();
        let ui_output = self.ui.paint(
            &self.window,
            document_size,
            &layers,
            self.stroke_active,
            &ui_trace_status(&self.trace.ui_state()),
        );
        self.apply_ui_actions(&layers, &ui_output.actions, true)?;
        self.ui_renderer.upload_textures(
            &self.gpu.device,
            &self.gpu.queue,
            &ui_output.textures_delta,
        );
        self.ui_renderer.upload_meshes(
            &self.gpu.device,
            &self.gpu.queue,
            &ui_output.clipped_primitives,
        );

        let dirty_tile_indices = self
            .runtime
            .as_mut()
            .map(|runtime| runtime.frame_scheduler_mut().take_scheduled_tile_indices())
            .unwrap_or_default();
        perf.dirty_slot_count = dirty_tile_indices.len();
        if !dirty_tile_indices.is_empty() {
            let update_cache_started = std::time::Instant::now();
            {
                puffin::profile_scope!("update_screen_presence");
                let screen_present_view = self
                    .screen_present
                    .texture()
                    .create_layer_view(0)
                    .map_err(ScreenPresentTileError::from)?;
                let screen_present_target = RenderTarget2d {
                    view: &screen_present_view,
                    format: self.screen_present.texture().format,
                    width: self.screen_present.texture().width,
                    height: self.screen_present.texture().height,
                };
                self.tile_renderer.clear_render_target(
                    &self.gpu.device,
                    &self.gpu.queue,
                    screen_present_target,
                    DEFAULT_BACKGROUND_COLOR,
                );
                let Some(runtime) = self.runtime.as_ref() else {
                    return Ok(());
                };
                present_root_tiles(
                    runtime.session().doc(),
                    runtime.session().doc_renderer(),
                    &mut self.tile_renderer,
                    &self.gpu.device,
                    &self.gpu.queue,
                    runtime.view(),
                    screen_present_target,
                    &self.full_tile_indices,
                )?;
            }
            perf.update_cache = update_cache_started.elapsed();
        }

        let acquire_frame_started = std::time::Instant::now();
        let frame = {
            puffin::profile_scope!("surface_acquire_frame");
            self.surface
                .acquire_frame()
                .map_err(AppPresentError::Surface)?
        };
        perf.acquire_frame = acquire_frame_started.elapsed();

        let result = {
            let target = RenderTarget2d {
                view: &frame.view,
                format: self.surface.format(),
                width: self.surface.width(),
                height: self.surface.height(),
            };
            let present_surface_started = std::time::Instant::now();
            let result: Result<(), AppPresentError> = (|| {
                puffin::profile_scope!("present_surface");
                self.tile_renderer.present_texture_2d(
                    &self.gpu.device,
                    &self.gpu.queue,
                    self.screen_present.texture(),
                    target,
                )?;
                let mut encoder =
                    self.gpu
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("glaphica-ui-overlay-encoder"),
                        });
                self.ui_renderer.render(
                    &self.gpu.queue,
                    &mut encoder,
                    &frame.view,
                    [self.surface.width(), self.surface.height()],
                    ui_output.pixels_per_point,
                );
                self.gpu.queue.submit(Some(encoder.finish()));
                Ok::<(), AppPresentError>(())
            })();
            perf.present_surface = present_surface_started.elapsed();
            result
        };

        match result {
            Ok(()) => {
                if let Some(runtime) = self.runtime.as_mut() {
                    runtime.frame_scheduler_mut().reset_redraw_request();
                }
                SurfaceRuntime::present(frame);
                self.trace_frame_perf(frame_started.elapsed(), &perf);
                Ok(())
            }
            Err(error) => {
                drop(frame);
                Err(error.into())
            }
        }
    }

    pub(super) fn apply_ui_actions(
        &mut self,
        layers: &[UiLayerItem],
        actions: &[UiAction],
        record: bool,
    ) -> Result<(), PreviewRuntimeError> {
        for action in actions {
            if record && let Some(event) = trace_action_for_ui_action(layers, action) {
                self.trace.record(PreviewTraceEvent::Ui(event));
            }
            match action {
                UiAction::StartRecordingRequested => {
                    self.trace.start_recording(&self.trace_default_path);
                }
                UiAction::StopRecordingRequested => {
                    if let Err(error) = self.trace.stop_recording() {
                        eprintln!("preview trace save failed: {error}");
                    }
                }
                UiAction::ReplayRequested => {
                    if let Err(error) = self.trace.load_replay(&self.trace_default_path) {
                        eprintln!("preview trace load failed: {error}");
                    }
                }
                UiAction::UndoRequested => {
                    if let Some(runtime) = self.runtime.as_mut() {
                        runtime.undo_last_stroke_gpu(
                            &mut self.tile_renderer,
                            &self.gpu.device,
                            &self.gpu.queue,
                        )?;
                    }
                }
                UiAction::CreateLayerRequested => {
                    self.stroke_active = false;
                    if let Some(runtime) = self.runtime.as_mut() {
                        runtime.create_layer_above_active_gpu(
                            &mut self.tile_renderer,
                            &self.gpu.device,
                            &self.gpu.queue,
                        )?;
                    }
                }
                UiAction::CreateGroupRequested => {
                    self.stroke_active = false;
                    if let Some(runtime) = self.runtime.as_mut() {
                        runtime.create_group_above_active_gpu(
                            &mut self.tile_renderer,
                            &self.gpu.device,
                            &self.gpu.queue,
                        )?;
                    }
                }
                UiAction::DeleteActiveLayerRequested => {
                    self.stroke_active = false;
                    if let Some(runtime) = self.runtime.as_mut() {
                        runtime.delete_active_layer_gpu(
                            &mut self.tile_renderer,
                            &self.gpu.device,
                            &self.gpu.queue,
                        )?;
                    }
                }
                UiAction::ActiveLayerChanged(node_id) => {
                    self.stroke_active = false;
                    if let Some(runtime) = self.runtime.as_mut() {
                        runtime.set_active_layer(*node_id)?;
                    }
                }
                UiAction::LayerOpacityChanged(node_id, opacity) => {
                    self.stroke_active = false;
                    if let Some(runtime) = self.runtime.as_mut() {
                        runtime.set_layer_opacity_gpu(
                            *node_id,
                            *opacity,
                            &mut self.tile_renderer,
                            &self.gpu.device,
                            &self.gpu.queue,
                        )?;
                    }
                }
                UiAction::LayerBlendModeChanged(node_id, blend_mode) => {
                    self.stroke_active = false;
                    if let Some(runtime) = self.runtime.as_mut() {
                        runtime.set_layer_blend_mode_gpu(
                            *node_id,
                            *blend_mode,
                            &mut self.tile_renderer,
                            &self.gpu.device,
                            &self.gpu.queue,
                        )?;
                    }
                }
                UiAction::RoundBrushSettingsChanged(settings) => {
                    self.stroke_active = false;
                    if let Some(runtime) = self.runtime.as_mut() {
                        runtime.set_round_brush_settings(settings.clone())?;
                    }
                }
            }
        }
        Ok(())
    }

    fn trace_frame_perf(&mut self, total: Duration, perf: &PreviewFramePerf) {
        if !self.perf_trace.stderr_enabled || total < self.perf_trace.slow_threshold {
            return;
        }
        let stages = [
            ("process_inputs", perf.process_inputs),
            ("update_cache", perf.update_cache),
            ("acquire_frame", perf.acquire_frame),
            ("present_surface", perf.present_surface),
        ];
        let Some((bottleneck, bottleneck_duration)) =
            stages.iter().max_by_key(|(_, duration)| *duration)
        else {
            return;
        };
        self.perf_frame_seq += 1;
        eprintln!(
            "[PERF][preview][frame={}] total_ms={:.3} bottleneck={} ({:.3}ms) dirty_tiles={} stages_ms={{process_inputs:{:.3}, update_cache:{:.3}, acquire_frame:{:.3}, present_surface:{:.3}}}",
            self.perf_frame_seq,
            duration_ms(total),
            bottleneck,
            duration_ms(*bottleneck_duration),
            perf.dirty_slot_count,
            duration_ms(perf.process_inputs),
            duration_ms(perf.update_cache),
            duration_ms(perf.acquire_frame),
            duration_ms(perf.present_surface),
        );
    }
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn collect_ui_layers(doc: &GlaDoc) -> Result<Vec<UiLayerItem>, GlaDocError> {
    let mut output = Vec::new();
    collect_ui_layer_subtree(doc, doc.root_id(), 0, &mut output)?;
    Ok(output)
}

fn collect_ui_layer_subtree(
    doc: &GlaDoc,
    node_id: GlaNodeId,
    depth: usize,
    output: &mut Vec<UiLayerItem>,
) -> Result<(), GlaDocError> {
    let node = doc.node(node_id)?;
    output.push(UiLayerItem {
        id: node_id,
        kind: node.kind(),
        depth,
        active: node_id == doc.active_layer_id(),
        opacity: node.opacity(),
        blend_mode: node.blend_mode(),
    });

    if let Some(children) = node.children() {
        for &child_id in children.iter().rev() {
            collect_ui_layer_subtree(doc, child_id, depth + 1, output)?;
        }
    }
    Ok(())
}

impl PreviewState {
    pub(super) fn process_replay_event(&mut self) -> Result<(), PreviewRuntimeError> {
        let Some(event) = self.trace.next_replay_event() else {
            return Ok(());
        };
        match event {
            PreviewTraceEvent::Ui(action) => self.apply_trace_ui_action(action)?,
            PreviewTraceEvent::BeginStroke => {
                if let Some(runtime) = self.runtime.as_mut()
                    && runtime.active_layer_is_paintable()
                {
                    runtime.begin_active_tool_stroke()?;
                    self.stroke_active = true;
                }
            }
            PreviewTraceEvent::StrokeSample(input) => {
                if let Some(runtime) = self.runtime.as_ref() {
                    runtime.push_canvas_input(input.into());
                }
            }
            PreviewTraceEvent::EndStroke => {
                if self.stroke_active {
                    if let Some(runtime) = self.runtime.as_mut() {
                        runtime.end_active_tool_stroke_gpu(
                            &mut self.tile_renderer,
                            &self.gpu.device,
                            &self.gpu.queue,
                            MAX_PENDING_BRUSH_INPUTS_PER_FRAME,
                        )?;
                    }
                    self.stroke_active = false;
                }
            }
        }
        if let Some(runtime) = self.runtime.as_mut() {
            runtime.frame_scheduler_mut().request_redraw();
        }
        Ok(())
    }

    fn apply_trace_ui_action(
        &mut self,
        action: PreviewTraceUiAction,
    ) -> Result<(), PreviewRuntimeError> {
        let layers = self
            .runtime
            .as_ref()
            .map(|runtime| collect_ui_layers(runtime.session().doc()))
            .transpose()
            .map_err(|error| {
                PreviewRuntimeError::Runtime(AppRuntimeError::Session(
                    EditorSessionError::Document(error),
                ))
            })?
            .unwrap_or_default();
        let ui_action = match action {
            PreviewTraceUiAction::Undo => UiAction::UndoRequested,
            PreviewTraceUiAction::CreateLayer => UiAction::CreateLayerRequested,
            PreviewTraceUiAction::CreateGroup => UiAction::CreateGroupRequested,
            PreviewTraceUiAction::DeleteActiveLayer => UiAction::DeleteActiveLayerRequested,
            PreviewTraceUiAction::SelectLayer { visible_index } => {
                let Some(layer) = layers.get(visible_index) else {
                    return Ok(());
                };
                UiAction::ActiveLayerChanged(layer.id)
            }
            PreviewTraceUiAction::SetLayerOpacity {
                visible_index,
                opacity,
            } => {
                let Some(layer) = layers.get(visible_index) else {
                    return Ok(());
                };
                UiAction::LayerOpacityChanged(layer.id, opacity)
            }
            PreviewTraceUiAction::SetLayerBlendMode {
                visible_index,
                blend_mode,
            } => {
                let Some(layer) = layers.get(visible_index) else {
                    return Ok(());
                };
                UiAction::LayerBlendModeChanged(layer.id, blend_mode.into())
            }
            PreviewTraceUiAction::SetRoundBrushSettings(settings) => {
                let mut brush_settings = RoundBrushSettings::default();
                settings.apply_to(&mut brush_settings);
                UiAction::RoundBrushSettingsChanged(brush_settings)
            }
        };
        self.apply_ui_actions(&layers, &[ui_action], false)
    }
}

fn trace_action_for_ui_action(
    layers: &[UiLayerItem],
    action: &UiAction,
) -> Option<PreviewTraceUiAction> {
    match action {
        UiAction::UndoRequested => Some(PreviewTraceUiAction::Undo),
        UiAction::CreateLayerRequested => Some(PreviewTraceUiAction::CreateLayer),
        UiAction::CreateGroupRequested => Some(PreviewTraceUiAction::CreateGroup),
        UiAction::DeleteActiveLayerRequested => Some(PreviewTraceUiAction::DeleteActiveLayer),
        UiAction::ActiveLayerChanged(node_id) => visible_layer_index(layers, *node_id)
            .map(|visible_index| PreviewTraceUiAction::SelectLayer { visible_index }),
        UiAction::LayerOpacityChanged(node_id, opacity) => visible_layer_index(layers, *node_id)
            .map(|visible_index| PreviewTraceUiAction::SetLayerOpacity {
                visible_index,
                opacity: *opacity,
            }),
        UiAction::LayerBlendModeChanged(node_id, blend_mode) => {
            visible_layer_index(layers, *node_id).map(|visible_index| {
                PreviewTraceUiAction::SetLayerBlendMode {
                    visible_index,
                    blend_mode: PreviewTraceBlendMode::from(*blend_mode),
                }
            })
        }
        UiAction::RoundBrushSettingsChanged(settings) => {
            Some(PreviewTraceUiAction::SetRoundBrushSettings(
                PreviewTraceRoundBrushSettings::from(settings.clone()),
            ))
        }
        UiAction::StartRecordingRequested
        | UiAction::StopRecordingRequested
        | UiAction::ReplayRequested => None,
    }
}

fn visible_layer_index(layers: &[UiLayerItem], node_id: GlaNodeId) -> Option<usize> {
    layers.iter().position(|layer| layer.id == node_id)
}

fn ui_trace_status(status: &super::trace::PreviewTraceUiState) -> UiTraceStatus {
    UiTraceStatus {
        mode: match status.mode {
            PreviewTraceMode::Idle => UiTraceMode::Idle,
            PreviewTraceMode::Recording => UiTraceMode::Recording,
            PreviewTraceMode::Replaying => UiTraceMode::Replaying,
            PreviewTraceMode::ReplayDone => UiTraceMode::ReplayDone,
        },
        event_count: status.event_count,
        replay_index: status.replay_index,
        path: status.path.clone(),
    }
}
