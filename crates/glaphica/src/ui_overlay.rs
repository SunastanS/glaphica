use egui::{Button, Color32, Context, RichText, SidePanel, TopBottomPanel};
use egui_winit::{EventResponse, State};
use winit::{event::WindowEvent, event_loop::ActiveEventLoop, window::Window};

use crate::{
    DocumentBlendMode, DocumentNodeId, DocumentNodeKind, RoundBrushSettings, UiAction, UiLayerItem,
    UiTraceMode, UiTraceStatus,
};

const BRUSH_PANEL_MIN_WIDTH: f32 = 220.0;
const BRUSH_PANEL_MAX_WIDTH: f32 = 320.0;
const DEFAULT_BRUSH_PANEL_WIDTH: f32 = 268.0;
const LAYER_PANEL_MIN_WIDTH: f32 = 220.0;
const LAYER_PANEL_MAX_WIDTH: f32 = 340.0;
const DEFAULT_LAYER_PANEL_WIDTH: f32 = 280.0;

pub(crate) struct AppUi {
    ctx: Context,
    state: State,
    theme: UiTheme,
    round_brush_settings: RoundBrushSettings,
    left_panel_width: f32,
    right_panel_width: f32,
}

#[allow(dead_code)]
pub(crate) struct UiPaintOutput {
    pub(crate) textures_delta: egui::TexturesDelta,
    pub(crate) clipped_primitives: Vec<egui::ClippedPrimitive>,
    pub(crate) pixels_per_point: f32,
    pub(crate) actions: Vec<UiAction>,
}

#[derive(Debug, Clone, Copy)]
struct UiTheme {
    panel_fill: Color32,
    panel_border: Color32,
    accent: Color32,
    subdued_text: Color32,
}

impl AppUi {
    pub(crate) fn new(
        event_loop: &ActiveEventLoop,
        window: &Window,
        round_brush_settings: RoundBrushSettings,
    ) -> Self {
        let ctx = Context::default();
        let state = State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            event_loop,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );
        let theme = UiTheme::default();
        apply_theme(&ctx, theme);

        Self {
            ctx,
            state,
            theme,
            round_brush_settings,
            left_panel_width: DEFAULT_LAYER_PANEL_WIDTH,
            right_panel_width: DEFAULT_BRUSH_PANEL_WIDTH,
        }
    }

    pub(crate) fn on_window_event(
        &mut self,
        window: &Window,
        event: &WindowEvent,
    ) -> EventResponse {
        self.state.on_window_event(window, event)
    }

    pub(crate) fn set_round_brush_settings(&mut self, settings: RoundBrushSettings) {
        self.round_brush_settings = settings;
    }

    pub(crate) fn paint(
        &mut self,
        window: &Window,
        document_size: [u32; 2],
        layers: &[UiLayerItem],
        stroke_active: bool,
        trace_status: &UiTraceStatus,
    ) -> UiPaintOutput {
        let raw_input = self.state.take_egui_input(window);
        let theme = self.theme;
        let left_panel_width = &mut self.left_panel_width;
        let right_panel_width = &mut self.right_panel_width;
        let round_brush_settings = &mut self.round_brush_settings;
        let mut actions = Vec::new();

        let full_output = self.ctx.run(raw_input, |ctx| {
            render_top_bar(ctx, theme, trace_status, &mut actions);
            render_layer_panel(ctx, theme, left_panel_width, layers, &mut actions);
            render_status_bar(ctx, theme, document_size, stroke_active, trace_status);
            render_brush_panel(
                ctx,
                theme,
                right_panel_width,
                round_brush_settings,
                &mut actions,
            );
        });

        self.state
            .handle_platform_output(window, full_output.platform_output);

        let clipped_primitives = self
            .ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);

        UiPaintOutput {
            textures_delta: full_output.textures_delta,
            clipped_primitives,
            pixels_per_point: full_output.pixels_per_point,
            actions,
        }
    }
}

impl Default for UiTheme {
    fn default() -> Self {
        Self {
            panel_fill: Color32::from_rgba_unmultiplied(28, 31, 36, 220),
            panel_border: Color32::from_rgb(60, 66, 74),
            accent: Color32::from_rgb(109, 181, 255),
            subdued_text: Color32::from_rgb(170, 178, 188),
        }
    }
}

fn apply_theme(ctx: &Context, theme: UiTheme) {
    let mut style: egui::Style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.window_fill = theme.panel_fill;
    style.visuals.panel_fill = theme.panel_fill;
    style.visuals.widgets.hovered.bg_fill = theme.accent.linear_multiply(0.25);
    style.visuals.widgets.active.bg_fill = theme.accent.linear_multiply(0.35);
    style.visuals.widgets.active.bg_stroke.color = theme.accent;
    style.visuals.selection.bg_fill = theme.accent.linear_multiply(0.35);
    style.visuals.faint_bg_color = theme.panel_fill;
    ctx.set_style(style);
}

fn panel_frame(theme: UiTheme) -> egui::Frame {
    egui::Frame {
        fill: theme.panel_fill,
        stroke: egui::Stroke::new(1.0, theme.panel_border),
        ..Default::default()
    }
}

fn render_top_bar(
    ctx: &Context,
    theme: UiTheme,
    trace_status: &UiTraceStatus,
    actions: &mut Vec<UiAction>,
) {
    TopBottomPanel::top("glaphica-ui-top-bar")
        .frame(panel_frame(theme))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("glaphica").strong());
                ui.separator();
                if ui.button("Undo").clicked() {
                    actions.push(UiAction::UndoRequested);
                }
                if ui.button("New Layer").clicked() {
                    actions.push(UiAction::CreateLayerRequested);
                }
                if ui.button("Delete").clicked() {
                    actions.push(UiAction::DeleteActiveNodeRequested);
                }
                ui.separator();
                match trace_status.mode {
                    UiTraceMode::Recording => {
                        if ui.button("Stop").clicked() {
                            actions.push(UiAction::StopRecordingRequested);
                        }
                    }
                    UiTraceMode::Replaying => {
                        ui.add_enabled(false, Button::new("Replaying"));
                    }
                    UiTraceMode::Idle | UiTraceMode::ReplayDone => {
                        if ui.button("Record").clicked() {
                            actions.push(UiAction::StartRecordingRequested);
                        }
                        if ui.button("Replay").clicked() {
                            actions.push(UiAction::ReplayRequested);
                        }
                    }
                }
            });
        });
}

fn render_status_bar(
    ctx: &Context,
    theme: UiTheme,
    document_size: [u32; 2],
    stroke_active: bool,
    trace_status: &UiTraceStatus,
) {
    TopBottomPanel::bottom("glaphica-ui-status-bar")
        .frame(panel_frame(theme))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{} x {} px", document_size[0], document_size[1]))
                        .color(theme.subdued_text),
                );
                ui.separator();
                ui.label(
                    RichText::new(if stroke_active { "Drawing" } else { "Idle" })
                        .color(theme.subdued_text),
                );
                ui.separator();
                ui.label(RichText::new(trace_status_text(trace_status)).color(theme.subdued_text));
            });
        });
}

fn trace_status_text(status: &UiTraceStatus) -> String {
    match status.mode {
        UiTraceMode::Idle => status
            .path
            .as_ref()
            .map(|path| format!("Trace ready: {path}"))
            .unwrap_or_else(|| "Trace idle".to_owned()),
        UiTraceMode::Recording => format!("Recording {} events", status.event_count),
        UiTraceMode::Replaying => {
            format!(
                "Replaying {}/{} events",
                status.replay_index, status.event_count
            )
        }
        UiTraceMode::ReplayDone => format!("Replay done: {} events", status.event_count),
    }
}

fn render_layer_panel(
    ctx: &Context,
    theme: UiTheme,
    left_panel_width: &mut f32,
    layers: &[UiLayerItem],
    actions: &mut Vec<UiAction>,
) {
    let panel = SidePanel::left("glaphica-ui-layer-panel")
        .resizable(true)
        .default_width(*left_panel_width)
        .min_width(LAYER_PANEL_MIN_WIDTH)
        .max_width(LAYER_PANEL_MAX_WIDTH)
        .frame(panel_frame(theme))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Layers");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("+ Group").clicked() {
                        actions.push(UiAction::CreateGroupRequested);
                    }
                    if ui.button("+ Layer").clicked() {
                        actions.push(UiAction::CreateLayerRequested);
                    }
                });
            });
            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                for item in layers {
                    render_layer_row(ui, item, actions);
                }
            });
        });

    *left_panel_width = panel.response.rect.width();
}

fn render_layer_row(ui: &mut egui::Ui, item: &UiLayerItem, actions: &mut Vec<UiAction>) {
    let indent = 14.0 * item.depth as f32;
    ui.horizontal(|ui| {
        ui.add_space(indent);
        let selected = ui
            .add_enabled(
                item.paintable,
                Button::new(layer_label(item)).selected(item.active),
            )
            .on_hover_text(format!("{:?}", item.id));
        if selected.clicked() {
            actions.push(UiAction::ActiveNodeChanged(item.id));
        }

        let mut opacity_percent = item.opacity * 100.0;
        let opacity_response = ui.add_sized(
            [82.0, 18.0],
            egui::Slider::new(&mut opacity_percent, 0.0..=100.0)
                .show_value(false)
                .text("%"),
        );
        if opacity_response.changed() {
            actions.push(UiAction::NodeOpacityChanged(
                item.id,
                (opacity_percent / 100.0).clamp(0.0, 1.0),
            ));
        }

        let mut blend_mode = item.blend_mode;
        egui::ComboBox::from_id_salt(("blend", item.id.value()))
            .selected_text(blend_mode_label(blend_mode))
            .width(86.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut blend_mode, DocumentBlendMode::Normal, "Normal");
                ui.selectable_value(&mut blend_mode, DocumentBlendMode::Overlay, "Overlay");
                ui.selectable_value(&mut blend_mode, DocumentBlendMode::Multiply, "Multiply");
                ui.selectable_value(&mut blend_mode, DocumentBlendMode::MaskAlpha, "Mask");
            });
        if blend_mode != item.blend_mode {
            actions.push(UiAction::NodeBlendModeChanged(item.id, blend_mode));
        }
    });
}

fn layer_label(item: &UiLayerItem) -> String {
    let prefix = match item.kind {
        DocumentNodeKind::Root => "Root",
        DocumentNodeKind::Group => "Group",
        DocumentNodeKind::Layer => "Layer",
    };
    format!("{prefix} {}", node_id_suffix(item.id))
}

fn node_id_suffix(id: DocumentNodeId) -> String {
    id.value().to_string()
}

fn blend_mode_label(blend_mode: DocumentBlendMode) -> &'static str {
    match blend_mode {
        DocumentBlendMode::Normal => "Normal",
        DocumentBlendMode::Overlay => "Overlay",
        DocumentBlendMode::Multiply => "Multiply",
        DocumentBlendMode::MaskAlpha => "Mask",
    }
}

fn render_brush_panel(
    ctx: &Context,
    theme: UiTheme,
    right_panel_width: &mut f32,
    round_brush_settings: &mut RoundBrushSettings,
    actions: &mut Vec<UiAction>,
) {
    let panel = SidePanel::right("glaphica-ui-brush-panel")
        .resizable(true)
        .default_width(*right_panel_width)
        .min_width(BRUSH_PANEL_MIN_WIDTH)
        .max_width(BRUSH_PANEL_MAX_WIDTH)
        .frame(panel_frame(theme))
        .show(ctx, |ui| {
            ui.heading("Brush");
            ui.separator();

            let mut changed = false;
            changed |= ui
                .color_edit_button_rgb(&mut round_brush_settings.tint)
                .changed();
            ui.separator();
            changed |= slider(
                ui,
                "Radius",
                &mut round_brush_settings.base_radius_px,
                1.0..=256.0,
            );
            changed |= slider(
                ui,
                "Hardness",
                &mut round_brush_settings.base_hardness,
                0.0..=1.0,
            );
            changed |= slider(ui, "Flow", &mut round_brush_settings.base_flow, 0.0..=1.0);
            changed |= slider(
                ui,
                "Opacity",
                &mut round_brush_settings.base_opacity,
                0.0..=1.0,
            );
            changed |= slider(
                ui,
                "Spacing",
                &mut round_brush_settings.spacing_ratio,
                0.05..=3.0,
            );

            if changed {
                actions.push(UiAction::RoundBrushSettingsChanged(
                    round_brush_settings.clone(),
                ));
            }
        });

    *right_panel_width = panel.response.rect.width();
}

fn slider(
    ui: &mut egui::Ui,
    label: &'static str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
) -> bool {
    ui.add(egui::Slider::new(value, range).text(label))
        .changed()
}
