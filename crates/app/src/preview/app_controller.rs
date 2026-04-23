use glaphica_core::{RadianVec2, ScreenVec2};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::Key;

use super::{
    MAX_PENDING_BRUSH_INPUTS_PER_FRAME, PreviewEventAction, PreviewRuntimeError, PreviewState,
};

impl PreviewState {
    pub(super) fn handle_window_event(
        &mut self,
        event: WindowEvent,
    ) -> Result<PreviewEventAction, PreviewRuntimeError> {
        let ui_response = self.ui.on_window_event(&self.window, &event);
        let ui_event_consumed = ui_response.consumed;
        let ui_requested_repaint = ui_response.repaint;

        match event {
            WindowEvent::CloseRequested => Ok(PreviewEventAction::Shutdown),
            WindowEvent::Resized(size) => {
                self.surface
                    .resize(&self.gpu.device, size.width.max(1), size.height.max(1));
                self.screen_cache.resize(
                    &self.gpu.device,
                    self.surface.format(),
                    self.surface.width(),
                    self.surface.height(),
                )?;
                self.update_view(self.surface.width(), self.surface.height())?;
                if let Some(runtime) = self.runtime.as_mut() {
                    runtime
                        .frame_scheduler_mut()
                        .schedule_tile_indices(&self.full_tile_indices);
                }
                Ok(merge_redraw_request(
                    PreviewEventAction::RequestRedraw,
                    ui_requested_repaint,
                ))
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
                Ok(merge_redraw_request(
                    PreviewEventAction::None,
                    ui_requested_repaint,
                ))
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = Some(ScreenVec2::new(position.x as f32, position.y as f32));
                if self.stroke_active && !ui_event_consumed {
                    self.push_cursor_input();
                    return Ok(PreviewEventAction::RequestRedraw);
                }
                Ok(merge_redraw_request(
                    PreviewEventAction::None,
                    ui_requested_repaint,
                ))
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    if ui_event_consumed {
                        return Ok(merge_redraw_request(
                            PreviewEventAction::None,
                            ui_requested_repaint,
                        ));
                    }
                    if !self.stroke_active
                        && let Some(runtime) = self.runtime.as_mut()
                    {
                        runtime.begin_active_tool_stroke()?;
                        self.stroke_active = true;
                        self.push_cursor_input();
                    }
                    Ok(PreviewEventAction::RequestRedraw)
                }
                ElementState::Released => {
                    if self.stroke_active {
                        if let Some(runtime) = self.runtime.as_mut() {
                            runtime.end_active_tool_stroke_gpu(
                                &self.image_backend,
                                &mut self.tile_renderer,
                                &self.gpu.device,
                                &self.gpu.queue,
                                MAX_PENDING_BRUSH_INPUTS_PER_FRAME,
                            )?;
                        }
                        self.stroke_active = false;
                    }
                    Ok(PreviewEventAction::RequestRedraw)
                }
            },
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state,
                        logical_key,
                        repeat,
                        ..
                    },
                ..
            } => {
                if ui_event_consumed {
                    return Ok(merge_redraw_request(
                        PreviewEventAction::None,
                        ui_requested_repaint,
                    ));
                }
                if state == ElementState::Pressed
                    && !repeat
                    && self.modifiers.control_key()
                    && let Key::Character(value) = logical_key
                    && value.eq_ignore_ascii_case("z")
                {
                    if let Some(runtime) = self.runtime.as_mut() {
                        runtime.undo_last_stroke_gpu(
                            &self.image_backend,
                            &mut self.tile_renderer,
                            &self.gpu.device,
                            &self.gpu.queue,
                        )?;
                    }
                    return Ok(PreviewEventAction::RequestRedraw);
                }
                Ok(merge_redraw_request(
                    PreviewEventAction::None,
                    ui_requested_repaint,
                ))
            }
            WindowEvent::RedrawRequested => {
                self.redraw()?;
                Ok(PreviewEventAction::None)
            }
            _ => Ok(merge_redraw_request(
                PreviewEventAction::None,
                ui_requested_repaint,
            )),
        }
    }

    fn update_view(&mut self, width: u32, height: u32) -> Result<(), PreviewRuntimeError> {
        let Some(runtime) = self.runtime.as_mut() else {
            return Ok(());
        };
        let layout = runtime.session().doc().layout();
        *runtime.view_mut() =
            super::app_bootstrap::fitted_view(layout.size_x(), layout.size_y(), width, height)?;
        Ok(())
    }

    fn push_cursor_input(&mut self) {
        let Some(position) = self.cursor_position else {
            return;
        };
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        runtime.push_screen_input(
            self.elapsed_time_ns(),
            position,
            1.0,
            RadianVec2::new(0.0, 0.0),
            0.0,
        );
    }

    fn elapsed_time_ns(&self) -> u64 {
        let nanos = self.started_at.elapsed().as_nanos();
        nanos.min(u128::from(u64::MAX)) as u64
    }

    pub(super) fn shutdown(&mut self) {
        self.stroke_active = false;
        if let Some(runtime) = self.runtime.take()
            && let Err(error) = runtime.shutdown()
        {
            eprintln!("preview shutdown failed: {error}");
        }
    }
}

fn merge_redraw_request(
    action: PreviewEventAction,
    ui_requested_repaint: bool,
) -> PreviewEventAction {
    if ui_requested_repaint && matches!(action, PreviewEventAction::None) {
        return PreviewEventAction::RequestRedraw;
    }
    action
}
