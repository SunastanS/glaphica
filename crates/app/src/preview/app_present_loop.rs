use std::time::Duration;

use renderer::RenderTarget2d;

use crate::{AppPresentError, ScreenPresentCacheError, SurfaceRuntime, present_root_tiles};

use super::{
    MAX_PENDING_BRUSH_INPUTS_PER_FRAME, PreviewRuntimeError, PreviewState,
    app_bootstrap::{env_flag, env_millis},
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
    dirty_tile_count: usize,
}

impl PreviewState {
    pub(super) fn redraw(&mut self) -> Result<(), PreviewRuntimeError> {
        if self.perf_trace.puffin_enabled {
            puffin::GlobalProfiler::lock().new_frame();
        }
        puffin::profile_function!();

        let frame_started = std::time::Instant::now();
        let mut perf = PreviewFramePerf::default();
        let Some(runtime) = self.runtime.as_mut() else {
            return Ok(());
        };

        let process_inputs_started = std::time::Instant::now();
        {
            puffin::profile_scope!("process_pending_brush_input_gpu");
            runtime.process_pending_brush_input_gpu(
                &self.image_backend,
                &mut self.tile_renderer,
                &self.gpu.device,
                &self.gpu.queue,
                MAX_PENDING_BRUSH_INPUTS_PER_FRAME,
            )?;
        }
        perf.process_inputs = process_inputs_started.elapsed();

        let dirty_tile_indices = runtime.frame_scheduler_mut().take_scheduled_tile_indices();
        perf.dirty_tile_count = dirty_tile_indices.len();
        if !dirty_tile_indices.is_empty() {
            let update_cache_started = std::time::Instant::now();
            {
                puffin::profile_scope!("update_screen_cache");
                let screen_cache_view = self
                    .screen_cache
                    .texture()
                    .create_layer_view(0)
                    .map_err(ScreenPresentCacheError::from)?;
                let screen_cache_target = RenderTarget2d {
                    view: &screen_cache_view,
                    format: self.screen_cache.texture().format,
                    width: self.screen_cache.texture().width,
                    height: self.screen_cache.texture().height,
                };
                if dirty_tile_indices.len() == self.full_tile_indices.len() {
                    self.tile_renderer.clear_render_target(
                        &self.gpu.device,
                        &self.gpu.queue,
                        screen_cache_target,
                        DEFAULT_BACKGROUND_COLOR,
                    );
                }
                present_root_tiles(
                    runtime.session().doc(),
                    runtime.session().doc_renderer(),
                    &mut self.tile_renderer,
                    &self.gpu.device,
                    &self.gpu.queue,
                    runtime.view(),
                    screen_cache_target,
                    &dirty_tile_indices,
                )?;
            }
            perf.update_cache = update_cache_started.elapsed();
        }

        let acquire_frame_started = std::time::Instant::now();
        let frame = {
            puffin::profile_scope!("surface_acquire_frame");
            self.surface.acquire_frame().map_err(|error| {
                AppPresentError::DocRenderer(
                    gla_doc_renderer::GlaDocRendererError::RenderExecution(
                        gla_doc_renderer::RenderExecutionError::new(error.to_string()),
                    ),
                )
            })?
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
            let result = {
                puffin::profile_scope!("present_surface");
                self.tile_renderer.present_texture_2d(
                    &self.gpu.device,
                    &self.gpu.queue,
                    self.screen_cache.texture(),
                    target,
                )
            };
            perf.present_surface = present_surface_started.elapsed();
            result.map_err(AppPresentError::from)
        };

        match result {
            Ok(()) => {
                runtime.frame_scheduler_mut().reset_redraw_request();
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
            perf.dirty_tile_count,
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
