use brush::round::RoundBrushSettings;
use egui::{Context, RichText, SidePanel, TopBottomPanel};
use egui_winit::{EventResponse, State};
use ui_components::{labeled_f32_slider, panel_frame, rgb_color_picker};
use winit::{event::WindowEvent, event_loop::ActiveEventLoop, window::Window};

use crate::theme::{UiTheme, apply_theme};

const BRUSH_PANEL_MIN_WIDTH: f32 = 220.0;
const BRUSH_PANEL_MAX_WIDTH: f32 = 320.0;
const DEFAULT_BRUSH_PANEL_WIDTH: f32 = 268.0;

pub struct AppUi {
    ctx: Context,
    state: State,
    theme: UiTheme,
    round_brush_settings: RoundBrushSettings,
    right_panel_width: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    UndoRequested,
    RoundBrushSettingsChanged(RoundBrushSettings),
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
        stroke_active: bool,
    ) -> UiPaintOutput {
        let raw_input = self.state.take_egui_input(window);
        let theme = self.theme;
        let right_panel_width = &mut self.right_panel_width;
        let round_brush_settings = &mut self.round_brush_settings;
        let mut actions = Vec::new();

        let full_output = self.ctx.run(raw_input, |ctx| {
            render_top_bar(ctx, theme, &mut actions);
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
