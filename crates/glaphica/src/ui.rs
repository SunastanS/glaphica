use crate::{
    DocumentBlendMode, DocumentLayerTreeError, DocumentNodeId, DocumentNodeKind, DocumentWorkspace,
    RoundBrushSettings,
};

#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    UndoRequested,
    StartRecordingRequested,
    StopRecordingRequested,
    ReplayRequested,
    CreateLayerRequested,
    CreateGroupRequested,
    DeleteActiveNodeRequested,
    ActiveNodeChanged(DocumentNodeId),
    NodeOpacityChanged(DocumentNodeId, f32),
    NodeBlendModeChanged(DocumentNodeId, DocumentBlendMode),
    RoundBrushSettingsChanged(RoundBrushSettings),
}

#[derive(Debug, Clone, PartialEq)]
pub struct UiLayerItem {
    pub id: DocumentNodeId,
    pub kind: DocumentNodeKind,
    pub depth: usize,
    pub active: bool,
    pub opacity: f32,
    pub blend_mode: DocumentBlendMode,
    pub paintable: bool,
}

pub fn collect_ui_layers(
    workspace: &DocumentWorkspace,
) -> Result<Vec<UiLayerItem>, DocumentLayerTreeError> {
    let mut output = Vec::new();
    collect_ui_layer_subtree(workspace, workspace.layer_tree().root_id(), 0, &mut output)?;
    Ok(output)
}

pub fn visible_layer_index(layers: &[UiLayerItem], node_id: DocumentNodeId) -> Option<usize> {
    layers.iter().position(|layer| layer.id == node_id)
}

fn collect_ui_layer_subtree(
    workspace: &DocumentWorkspace,
    node_id: DocumentNodeId,
    depth: usize,
    output: &mut Vec<UiLayerItem>,
) -> Result<(), DocumentLayerTreeError> {
    let node = workspace.layer_tree().node(node_id)?;
    output.push(UiLayerItem {
        id: node_id,
        kind: node.kind(),
        depth,
        active: node_id == workspace.layer_tree().active_node_id(),
        opacity: node.opacity(),
        blend_mode: node.blend_mode(),
        paintable: node.kind() == DocumentNodeKind::Root || node.kind() == DocumentNodeKind::Layer,
    });

    if let Some(children) = node.children() {
        for &child_id in children.iter().rev() {
            collect_ui_layer_subtree(workspace, child_id, depth + 1, output)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{collect_ui_layers, visible_layer_index};
    use crate::{DocumentNodeKind, DocumentWorkspace};

    #[test]
    fn collect_ui_layers_matches_visible_tree_order() {
        let mut workspace = DocumentWorkspace::blank(320, 240).unwrap();
        let root = workspace.layer_tree().root_id();
        let first = workspace.append_layer(root).unwrap();
        let group = workspace.append_group(root).unwrap();
        let nested = workspace.append_layer(group).unwrap();

        let layers = collect_ui_layers(&workspace).unwrap();

        assert_eq!(
            layers.iter().map(|layer| layer.id).collect::<Vec<_>>(),
            vec![root, group, nested, first]
        );
        assert_eq!(layers[0].kind, DocumentNodeKind::Root);
        assert_eq!(layers[0].depth, 0);
        assert!(layers[0].paintable);
        assert_eq!(layers[1].kind, DocumentNodeKind::Group);
        assert_eq!(layers[1].depth, 1);
        assert!(!layers[1].paintable);
        assert_eq!(layers[2].kind, DocumentNodeKind::Layer);
        assert_eq!(layers[2].depth, 2);
        assert!(layers[2].active);
        assert!(layers[2].paintable);
        assert_eq!(visible_layer_index(&layers, nested), Some(2));
    }
}
