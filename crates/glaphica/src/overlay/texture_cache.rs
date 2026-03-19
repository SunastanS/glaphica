use std::collections::HashMap;

use app::LayerPreviewBitmap;
use egui::Context;
use glaphica_core::NodeId;

pub struct LayerTextureCache {
    textures: HashMap<NodeId, egui::TextureHandle>,
}

impl LayerTextureCache {
    pub fn new() -> Self {
        Self {
            textures: HashMap::new(),
        }
    }

    pub fn get(&self, node_id: NodeId) -> Option<&egui::TextureHandle> {
        self.textures.get(&node_id)
    }

    pub fn update(&mut self, ctx: &Context, updates: Vec<LayerPreviewBitmap>) {
        for preview in updates {
            let texture = ctx.load_texture(
                format!("layer-preview-{}", preview.node_id.0),
                egui::ColorImage::from_rgba_unmultiplied(
                    [preview.width as usize, preview.height as usize],
                    &preview.pixels,
                ),
                egui::TextureOptions::NEAREST,
            );
            self.textures.insert(preview.node_id, texture);
        }
    }

    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&NodeId) -> bool,
    {
        self.textures.retain(|node_id, _| f(node_id));
    }

    pub fn ids(&self) -> &HashMap<NodeId, egui::TextureHandle> {
        &self.textures
    }
}
