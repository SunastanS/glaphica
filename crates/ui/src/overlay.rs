use brush::round::RoundBrushSettings;
use egui::{Button, Context, RichText, SidePanel, TopBottomPanel};
use egui_winit::{EventResponse, State};
use gla_document::{GlaNodeId, GlaNodeKind};
use glaphica_core::BlendMode;
use ui_components::{labeled_f32_slider, panel_frame, rgb_color_picker};
use winit::{event::WindowEvent, event_loop::ActiveEventLoop, window::Window};

use crate::theme::{UiTheme, apply_theme};

const BRUSH_PANEL_MIN_WIDTH: f32 = 220.0;
const BRUSH_PANEL_MAX_WIDTH: f32 = 320.0;
const DEFAULT_BRUSH_PANEL_WIDTH: f32 = 268.0;
const LAYER_PANEL_MIN_WIDTH: f32 = 220.0;
const LAYER_PANEL_MAX_WIDTH: f32 = 340.0;
const DEFAULT_LAYER_PANEL_WIDTH: f32 = 280.0;

pub struct AppUi {
    ctx: Context,
    state: State,
    theme: UiTheme,
    round_brush_settings: RoundBrushSettings,
    left_panel_width: f32,
    right_panel_width: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    UndoRequested,
    CreateLayerRequested,
    CreateGroupRequested,
    DeleteActiveLayerRequested,
    ActiveLayerChanged(GlaNodeId),
    LayerOpacityChanged(GlaNodeId, f32),
    LayerBlendModeChanged(GlaNodeId, BlendMode),
    RoundBrushSettingsChanged(RoundBrushSettings),
}

#[derive(Debug, Clone, PartialEq)]
pub struct UiLayerItem {
    pub id: GlaNodeId,
    pub kind: GlaNodeKind,
    pub depth: usize,
    pub active: bool,
    pub opacity: f32,
    pub blend_mode: BlendMode,
}

pub struct UiPaintOutput {
    pub textures_delta: egui::TexturesDelta,
    pub clipped_primitives: Vec<egui::ClippedPrimitive>,
    pub pixels_per_point: f32,
    pub actions: Vec<UiAction>,
}

impl AppUi {
    pub fn new(
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

    pub fn on_window_event(&mut self, window: &Window, event: &WindowEvent) -> EventResponse {
        self.state.on_window_event(window, event)
    }

    pub fn paint(
        &mut self,
        window: &Window,
        document_size: [u32; 2],
        layers: &[UiLayerItem],
        stroke_active: bool,
    ) -> UiPaintOutput {
        let raw_input = self.state.take_egui_input(window);
        let theme = self.theme;
        let left_panel_width = &mut self.left_panel_width;
        let right_panel_width = &mut self.right_panel_width;
        let round_brush_settings = &mut self.round_brush_settings;
        let mut actions = Vec::new();

        let full_output = self.ctx.run(raw_input, |ctx| {
            render_top_bar(ctx, theme, &mut actions);
            render_layer_panel(ctx, theme, left_panel_width, layers, &mut actions);
            render_status_bar(ctx, theme, document_size, stroke_active);
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

fn render_top_bar(ctx: &Context, theme: UiTheme, actions: &mut Vec<UiAction>) {
    TopBottomPanel::top("glaphica-ui-top-bar")
        .frame(panel_frame(theme.panel_fill, theme.panel_border))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("glaphica").strong());
                ui.label(
                    RichText::new("Round Brush")
                        .color(theme.subdued_text)
                        .small(),
                );
                ui.separator();
                if ui.button("Undo").clicked() {
                    actions.push(UiAction::UndoRequested);
                }
                if ui.button("New Layer").clicked() {
                    actions.push(UiAction::CreateLayerRequested);
                }
                if ui.button("Delete").clicked() {
                    actions.push(UiAction::DeleteActiveLayerRequested);
                }
            });
        });
}

fn render_status_bar(ctx: &Context, theme: UiTheme, document_size: [u32; 2], stroke_active: bool) {
    TopBottomPanel::bottom("glaphica-ui-status-bar")
        .frame(panel_frame(theme.panel_fill, theme.panel_border))
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
                ui.label(RichText::new("Ctrl+Z Undo").color(theme.subdued_text));
            });
        });
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
        .frame(panel_frame(theme.panel_fill, theme.panel_border))
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
                    render_layer_row(ui, theme, item, actions);
                }
            });
        });

    *left_panel_width = panel.response.rect.width();
}

fn render_layer_row(
    ui: &mut egui::Ui,
    theme: UiTheme,
    item: &UiLayerItem,
    actions: &mut Vec<UiAction>,
) {
    let indent = 14.0 * item.depth as f32;
    ui.horizontal(|ui| {
        ui.add_space(indent);
        let label = layer_label(item);
        let selected = ui
            .add_sized(
                [96.0, 24.0],
                Button::new(label).selected(item.active),
            )
            .on_hover_text(format!("{:?}", item.id));
        if selected.clicked() {
            actions.push(UiAction::ActiveLayerChanged(item.id));
        }

        let mut opacity_percent = item.opacity * 100.0;
        let opacity_response = ui.add_sized(
            [82.0, 18.0],
            egui::Slider::new(&mut opacity_percent, 0.0..=100.0)
                .show_value(false)
                .text("%"),
        );
        if opacity_response.changed() {
            actions.push(UiAction::LayerOpacityChanged(
                item.id,
                (opacity_percent / 100.0).clamp(0.0, 1.0),
            ));
        }

        let mut blend_mode = item.blend_mode;
        egui::ComboBox::from_id_salt(("blend", item.id))
            .selected_text(blend_mode_label(blend_mode))
            .width(76.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut blend_mode, BlendMode::Normal, "Normal");
                ui.selectable_value(&mut blend_mode, BlendMode::Multiply, "Multiply");
            });
        if blend_mode != item.blend_mode {
            actions.push(UiAction::LayerBlendModeChanged(item.id, blend_mode));
        }
    });
    let _ = theme;
}

fn layer_label(item: &UiLayerItem) -> String {
    let prefix = match item.kind {
        GlaNodeKind::Root => "Root",
        GlaNodeKind::Branch => "Group",
        GlaNodeKind::Leaf => "Layer",
    };
    format!("{prefix} {}", slotmap_key_suffix(item.id))
}

fn blend_mode_label(blend_mode: BlendMode) -> &'static str {
    match blend_mode {
        BlendMode::Normal => "Normal",
        BlendMode::Multiply => "Multiply",
    }
}

fn slotmap_key_suffix(id: GlaNodeId) -> String {
    let debug = format!("{id:?}");
    debug
        .strip_prefix("GlaNodeId(")
        .and_then(|value| value.strip_suffix(')'))
        .unwrap_or(&debug)
        .to_owned()
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
        .frame(panel_frame(theme.panel_fill, theme.panel_border))
        .show(ctx, |ui| {
            ui.heading("Brush");
            ui.label(
                RichText::new("Round brush settings are applied immediately.")
                    .color(theme.subdued_text)
                    .small(),
            );
            ui.separator();

            let mut changed = false;
            changed |= rgb_color_picker(ui, "Tint", &mut round_brush_settings.tint);
            ui.separator();
            changed |= labeled_f32_slider(
                ui,
                "Radius",
                &mut round_brush_settings.base_radius_px,
                1.0..=256.0,
            );
            changed |= labeled_f32_slider(
                ui,
                "Hardness",
                &mut round_brush_settings.base_hardness,
                0.0..=1.0,
            );
            changed |=
                labeled_f32_slider(ui, "Flow", &mut round_brush_settings.base_flow, 0.0..=1.0);
            changed |= labeled_f32_slider(
                ui,
                "Opacity",
                &mut round_brush_settings.base_opacity,
                0.0..=1.0,
            );
            changed |= labeled_f32_slider(
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
