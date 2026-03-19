use std::time::{SystemTime, UNIX_EPOCH};

use glaphica_core::{CanvasVec2, InputDeviceKind, MappedCursor, RadianVec2};
use thread_protocol::InputRingSample;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use crate::desktop_app::DesktopApp;

pub fn current_time_ns() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as u64,
        Err(_) => 0,
    }
}

pub fn scroll_delta_lines(delta: &MouseScrollDelta) -> f32 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => *y,
        MouseScrollDelta::PixelDelta(position) => position.y as f32 / 40.0,
    }
}

pub enum MouseInputResult {
    None,
    StrokeBegan,
    StrokeEnded,
    PanStarted,
    PanEnded,
}

pub fn handle_window_event(
    app: &mut DesktopApp,
    event: &WindowEvent,
    ui_event_consumed: bool,
) -> (MouseInputResult, bool) {
    match event {
        WindowEvent::MouseInput { button, state, .. } => {
            if ui_event_consumed {
                return handle_mouse_input_ui_consumed(app, button, state);
            }
            handle_mouse_input(app, button, state)
        }
        WindowEvent::CursorMoved { position, .. } => {
            if app.is_replay_mode() {
                return (MouseInputResult::None, false);
            }
            let current_position = (position.x as f32, position.y as f32);
            app.cursor_position = Some(current_position);
            if ui_event_consumed {
                return (MouseInputResult::None, false);
            }
            let needs_redraw = handle_cursor_moved(app, current_position);
            (MouseInputResult::None, needs_redraw)
        }
        WindowEvent::MouseWheel { delta, .. } => {
            if app.is_replay_mode() || ui_event_consumed {
                return (MouseInputResult::None, false);
            }
            let needs_redraw = handle_mouse_wheel(app, delta);
            (MouseInputResult::None, needs_redraw)
        }
        WindowEvent::ModifiersChanged(modifiers) => {
            app.ctrl_pressed = modifiers.state().control_key();
            app.shift_pressed = modifiers.state().shift_key();
            (MouseInputResult::None, false)
        }
        _ => (MouseInputResult::None, false),
    }
}

fn handle_mouse_input_ui_consumed(
    app: &mut DesktopApp,
    button: &MouseButton,
    state: &ElementState,
) -> (MouseInputResult, bool) {
    match (button, state) {
        (MouseButton::Left, ElementState::Released) if app.stroke_active => {
            app.stroke_active = false;
            if let Some(integration) = &mut app.integration {
                integration.end_stroke();
            }
            if let Some(overlay) = &mut app.overlay {
                overlay.mark_document_dirty();
            }
            (MouseInputResult::StrokeEnded, true)
        }
        (MouseButton::Middle, ElementState::Released) => {
            app.middle_pan_active = false;
            app.middle_pan_last_position = None;
            (MouseInputResult::PanEnded, true)
        }
        _ => (MouseInputResult::None, false),
    }
}

fn handle_mouse_input(
    app: &mut DesktopApp,
    button: &MouseButton,
    state: &ElementState,
) -> (MouseInputResult, bool) {
    if app.is_replay_mode() {
        return (MouseInputResult::None, false);
    }
    match button {
        MouseButton::Left => match state {
            ElementState::Pressed => {
                app.stroke_active = false;
                if let Some(integration) = &mut app.integration {
                    if integration.active_paint_node().is_some() {
                        app.stroke_active = true;
                        return (MouseInputResult::StrokeBegan, false);
                    }
                }
                (MouseInputResult::None, false)
            }
            ElementState::Released => {
                let stroke_was_active = app.stroke_active;
                app.stroke_active = false;
                if let Some(integration) = &mut app.integration {
                    integration.end_stroke();
                }
                if stroke_was_active {
                    if let Some(overlay) = &mut app.overlay {
                        overlay.mark_document_dirty();
                    }
                }
                (MouseInputResult::None, false)
            }
        },
        MouseButton::Middle => match state {
            ElementState::Pressed => {
                app.middle_pan_active = true;
                app.middle_pan_last_position = app.cursor_position;
                (MouseInputResult::PanStarted, true)
            }
            ElementState::Released => {
                app.middle_pan_active = false;
                app.middle_pan_last_position = None;
                (MouseInputResult::PanEnded, true)
            }
        },
        _ => (MouseInputResult::None, false),
    }
}

fn handle_cursor_moved(app: &mut DesktopApp, current_position: (f32, f32)) -> bool {
    let mut needs_redraw = false;

    if app.middle_pan_active {
        if let Some((last_x, last_y)) = app.middle_pan_last_position {
            let dx = current_position.0 - last_x;
            let dy = current_position.1 - last_y;
            if let Some(integration) = &mut app.integration {
                integration.pan_view(dx, dy);
            }
            needs_redraw = true;
        }
        app.middle_pan_last_position = Some(current_position);
    }

    if app.stroke_active {
        if let Some(integration) = &mut app.integration {
            let (doc_x, doc_y) =
                integration.map_screen_to_document(current_position.0, current_position.1);
            let sample = InputRingSample {
                epoch: app.epoch,
                time_ns: current_time_ns(),
                device: InputDeviceKind::Cursor,
                cursor: MappedCursor {
                    cursor: CanvasVec2::new(doc_x, doc_y),
                    tilt: RadianVec2::new(0.0, 0.0),
                    pressure: 1.0,
                    twist: 0.0,
                },
            };
            integration.push_input_sample(sample);
        }
    }

    needs_redraw
}

fn handle_mouse_wheel(app: &mut DesktopApp, delta: &MouseScrollDelta) -> bool {
    let scroll = scroll_delta_lines(delta);
    if scroll.abs() <= f32::EPSILON {
        return false;
    }

    let (center_x, center_y) = match app.cursor_position {
        Some(position) => position,
        None => match &app.window {
            Some(window) => {
                let size = window.inner_size();
                (size.width as f32 * 0.5, size.height as f32 * 0.5)
            }
            None => return false,
        },
    };

    if let Some(integration) = &mut app.integration {
        if app.ctrl_pressed {
            integration.rotate_view(scroll * 0.05, center_x, center_y);
        } else {
            integration.zoom_view((scroll * 0.12).exp(), center_x, center_y);
        }
    }

    true
}
