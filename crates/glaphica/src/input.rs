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

pub fn handle_window_event(
    app: &mut DesktopApp,
    event: &WindowEvent,
    ui_event_consumed: bool,
) -> bool {
    match event {
        WindowEvent::MouseInput { button, state, .. } => {
            if ui_event_consumed {
                return handle_mouse_input_ui_consumed(app, button, state);
            }
            handle_mouse_input(app, button, state)
        }
        WindowEvent::CursorMoved { position, .. } => {
            if app.is_replay_mode() {
                return false;
            }
            let current_position = (position.x as f32, position.y as f32);
            app.cursor_position = Some(current_position);
            if ui_event_consumed {
                return false;
            }
            handle_cursor_moved(app, current_position)
        }
        WindowEvent::MouseWheel { delta, .. } => {
            if app.is_replay_mode() || ui_event_consumed {
                return false;
            }
            handle_mouse_wheel(app, delta)
        }
        WindowEvent::ModifiersChanged(modifiers) => {
            app.ctrl_pressed = modifiers.state().control_key();
            app.shift_pressed = modifiers.state().shift_key();
            false
        }
        _ => false,
    }
}

fn handle_mouse_input_ui_consumed(
    app: &mut DesktopApp,
    button: &MouseButton,
    state: &ElementState,
) -> bool {
    match (button, state) {
        (MouseButton::Left, ElementState::Released) if app.stroke_active => {
            app.stroke_active = false;
            if let Some(integration) = &mut app.integration {
                integration.end_stroke();
            }
            true
        }
        (MouseButton::Middle, ElementState::Released) => {
            app.middle_pan_active = false;
            app.middle_pan_last_position = None;
            true
        }
        _ => false,
    }
}

fn handle_mouse_input(app: &mut DesktopApp, button: &MouseButton, state: &ElementState) -> bool {
    match button {
        MouseButton::Left => match state {
            ElementState::Pressed => {
                app.stroke_active = false;
                if let Some(integration) = &mut app.integration {
                    if let Some(node_id) = integration.active_paint_node() {
                        integration.begin_stroke(node_id);
                        app.stroke_active = true;
                    }
                }
                true
            }
            ElementState::Released => {
                let stroke_was_active = app.stroke_active;
                app.stroke_active = false;
                if let Some(integration) = &mut app.integration {
                    integration.end_stroke();
                }
                stroke_was_active
            }
        },
        MouseButton::Middle => match state {
            ElementState::Pressed => {
                app.middle_pan_active = true;
                app.middle_pan_last_position = app.cursor_position;
                true
            }
            ElementState::Released => {
                app.middle_pan_active = false;
                app.middle_pan_last_position = None;
                true
            }
        },
        _ => false,
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
