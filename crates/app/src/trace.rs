use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use brushes::{BrushConfigValue, UnitIntervalPoint};
use document::{LayerMoveTarget, NewLayerKind, UiBlendMode};
use glaphica_core::{
    BlendMode, BrushId, CanvasVec2, EpochId, ImageTileBinding, ImageTileKey, InputDeviceKind,
    MappedCursor, NodeId, RadianVec2, RenderTreeGeneration, StrokeId, TileKey,
};
use gpu_runtime::FrameBatchPerfStats;
use serde::{Deserialize, Serialize};
use thread_protocol::{
    ClearOp, CompositeOp, CopyOp, DrawFrameMergePolicy, DrawOp, DrawStrokeCtx, GpuCmdFrameMergeTag,
    GpuCmdMsg, InputControlEvent, InputControlOp, InputRingSample, RefImage, RenderTreeUpdatedMsg,
    TileSlotKeyUpdateMsg, WriteKind, WriteOp,
};

use crate::AppControl;

const TRACE_VERSION: u32 = 2;

#[derive(Debug)]
pub enum TraceIoError {
    Io(std::io::Error),
    Json(serde_json::Error),
    UnsupportedVersion(u32),
}

impl Display for TraceIoError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "trace io error: {error}"),
            Self::Json(error) => write!(f, "trace json error: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported trace version: {version}")
            }
        }
    }
}

impl std::error::Error for TraceIoError {}

impl From<std::io::Error> for TraceIoError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for TraceIoError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceInputFile {
    pub version: u32,
    pub frames: Vec<TraceInputFrame>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceOutputFile {
    pub version: u32,
    pub frames: Vec<TraceOutputFrame>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceInputFrame {
    pub controls: Vec<TraceAppControl>,
    pub samples: Vec<TraceInputSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceOutputFrame {
    pub commands: Vec<TraceGpuCmd>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tile_timeline: Option<TraceTileTimeline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submit_render: Option<TraceSubmitRender>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_tile_events: Vec<TraceRuntimeTileEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draw_compaction: Option<TraceDrawCompaction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceTileTimeline {
    pub updated_tile_indices: Vec<usize>,
    pub drawn_tile_indices: Vec<usize>,
    pub missing_updated_tile_indices: Vec<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSubmitRender {
    pub render_cmd_count: usize,
    pub render_dst_tile_count: usize,
    pub render_tile_indices: Vec<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceRuntimeTileEvent {
    pub stage: TraceRuntimeTileStage,
    pub tile_indices: Vec<usize>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TraceRuntimeTileStage {
    ApplyVisibleUpdates,
    ProcessRenderComposite,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceDrawCompaction {
    pub pre_compact_draw_tile_indices: Vec<usize>,
    pub post_compact_draw_tile_indices: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TraceAppControl {
    StrokeBoundary {
        node_id: u64,
        begin: bool,
    },
    SelectNode {
        node_id: u64,
    },
    CreateLayerAboveActive {
        kind: TraceNewLayerKind,
    },
    CreateGroupAboveActive,
    DeleteNode {
        node_id: u64,
    },
    MoveNode {
        node_id: u64,
        target_parent_id: u64,
        target_index: usize,
    },
    SetNodeVisibility {
        node_id: u64,
        visible: bool,
    },
    SetNodeOpacity {
        node_id: u64,
        opacity: f32,
    },
    SetNodeBlendMode {
        node_id: u64,
        blend_mode: TraceUiBlendMode,
    },
    SetActiveBrush {
        brush_id: u64,
    },
    SetActiveBrushColorRgb {
        rgb: [f32; 3],
    },
    SetActiveBrushErase {
        erase: bool,
    },
    UpdateBrushConfig {
        brush_id: u64,
        values: Vec<TraceBrushConfigValue>,
    },
    MoveActiveNodeUp,
    MoveActiveNodeDown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TraceBrushConfigValue {
    ScalarF32(f32),
    UnitIntervalCurve(Vec<TraceUnitIntervalPoint>),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TraceUnitIntervalPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TraceUiBlendMode {
    Normal,
    Multiply,
    Penetrate,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TraceNewLayerKind {
    Raster,
    SolidColor,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TraceTileKey {
    pub backend: u8,
    pub generation: u32,
    pub slot: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TraceInputDeviceKind {
    Pen,
    Cursor,
    Finger { index: u32 },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TraceInputSample {
    pub epoch: u32,
    pub time_ns: u64,
    pub device: TraceInputDeviceKind,
    pub cursor_x: f32,
    pub cursor_y: f32,
    pub tilt_x: f32,
    pub tilt_y: f32,
    pub pressure: f32,
    pub twist: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TraceGpuCmd {
    ExpandAtlasBackend(TraceExpandAtlasBackendMsg),
    DrawOp(TraceDrawOp),
    CopyOp(TraceCopyOp),
    WriteOp(TraceWriteOp),
    CompositeOp(TraceCompositeOp),
    ClearOp(TraceClearOp),
    RenderTreeUpdated(TraceRenderTreeUpdatedMsg),
    TileSlotKeyUpdate(TraceTileSlotKeyUpdateMsg),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TraceExpandAtlasBackendMsg {
    pub src_backend_id: u8,
    pub dst_backend_id: u8,
    pub src_layout: TraceAtlasLayout,
    pub dst_layout: TraceAtlasLayout,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TraceAtlasLayout {
    Tiny8,
    Small11,
    Medium14,
    Large17,
    Huge20,
}

impl From<glaphica_core::AtlasLayout> for TraceAtlasLayout {
    fn from(value: glaphica_core::AtlasLayout) -> Self {
        match value {
            glaphica_core::AtlasLayout::Tiny8 => Self::Tiny8,
            glaphica_core::AtlasLayout::Small11 => Self::Small11,
            glaphica_core::AtlasLayout::Medium14 => Self::Medium14,
            glaphica_core::AtlasLayout::Large17 => Self::Large17,
            glaphica_core::AtlasLayout::Huge20 => Self::Huge20,
        }
    }
}

impl From<TraceAtlasLayout> for glaphica_core::AtlasLayout {
    fn from(value: TraceAtlasLayout) -> Self {
        match value {
            TraceAtlasLayout::Tiny8 => Self::Tiny8,
            TraceAtlasLayout::Small11 => Self::Small11,
            TraceAtlasLayout::Medium14 => Self::Medium14,
            TraceAtlasLayout::Large17 => Self::Large17,
            TraceAtlasLayout::Huge20 => Self::Huge20,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceDrawOp {
    pub node_id: Option<u64>,
    #[serde(default)]
    pub image_tile: TraceImageTileKey,
    #[serde(default)]
    pub stroke_id: u64,
    pub tile_key: TraceTileKey,
    pub blend_mode: Option<TraceDrawBlendMode>,
    pub frame_merge: Option<TraceDrawFrameMergePolicy>,
    #[serde(default = "trace_empty_tile_key")]
    pub origin_tile_key: TraceTileKey,
    pub ref_image_tile_key: Option<TraceTileKey>,
    pub input: Vec<f32>,
    pub rgb: Option<[f32; 3]>,
    #[serde(default)]
    pub erase: bool,
    pub brush_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TraceDrawBlendMode {
    Alpha,
    Additive,
    Replace,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TraceDrawFrameMergePolicy {
    None,
    KeepLastInFrameByNodeTileBrush,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TraceGpuCmdFrameMergeTag {
    None,
    KeepFirstInFrameByDstTile,
    KeepLastInFrameByDstTile,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TraceCopyOp {
    pub src_tile_key: TraceTileKey,
    pub dst_tile_key: TraceTileKey,
    #[serde(default = "trace_gpu_cmd_frame_merge_none")]
    pub frame_merge: TraceGpuCmdFrameMergeTag,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TraceWriteOp {
    pub src_tile_key: TraceTileKey,
    #[serde(default)]
    pub node_id: u64,
    #[serde(default)]
    pub image_tile: TraceImageTileKey,
    pub dst_tile_key: TraceTileKey,
    #[serde(default = "trace_gpu_cmd_frame_merge_none")]
    pub frame_merge: TraceGpuCmdFrameMergeTag,
    #[serde(default = "trace_write_blend_mode_normal")]
    pub blend_mode: TraceWriteBlendMode,
    #[serde(default = "trace_write_opacity_one")]
    pub opacity: f32,
    #[serde(default = "trace_write_rgb_red")]
    pub rgb: Option<[f32; 3]>,
    #[serde(default)]
    pub origin_tile_key: Option<TraceTileKey>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TraceCompositeOp {
    pub base_tile_key: TraceTileKey,
    pub overlay_tile_key: TraceTileKey,
    pub dst_tile_key: TraceTileKey,
    #[serde(default = "trace_composite_blend_mode_normal")]
    pub blend_mode: TraceCompositeBlendMode,
    #[serde(default = "trace_write_opacity_one")]
    pub opacity: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TraceWriteBlendMode {
    Normal,
    Erase,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TraceCompositeBlendMode {
    Normal,
    Multiply,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TraceClearOp {
    pub tile_key: TraceTileKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceRenderTreeUpdatedMsg {
    pub generation: u64,
    pub dirty_render_caches: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceTileSlotKeyUpdateMsg {
    pub updates: Vec<TraceImageTileBinding>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TraceImageTileKey {
    #[serde(default)]
    pub image_id: u64,
    #[serde(default)]
    pub tile_index: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TraceImageTileBinding {
    pub image_tile: TraceImageTileKey,
    pub tile_key: TraceTileKey,
}

#[derive(Debug, Default)]
pub struct TraceRecorder {
    input_frames: Vec<TraceInputFrame>,
    output_frames: Vec<TraceOutputFrame>,
}

impl TraceRecorder {
    pub fn record_input_frame(
        &mut self,
        controls: &[InputControlEvent<AppControl>],
        samples: &[InputRingSample],
    ) {
        if controls.is_empty() && samples.is_empty() {
            return;
        }
        let mut trace_controls = Vec::with_capacity(controls.len());
        for control in controls {
            let InputControlEvent::Control(control) = control;
            if let Some(serialized) = control.to_serialized() {
                trace_controls.push(serialized);
            } else {
                eprintln!("trace record skipped an unsupported control event");
            }
        }

        let mut trace_samples = Vec::with_capacity(samples.len());
        for sample in samples {
            trace_samples.push(TraceInputSample::from(*sample));
        }

        self.input_frames.push(TraceInputFrame {
            controls: trace_controls,
            samples: trace_samples,
        });
    }

    pub fn record_output_frame(
        &mut self,
        commands: &[GpuCmdMsg],
        submit_stats: Option<&FrameBatchPerfStats>,
        runtime_tile_events: Vec<TraceRuntimeTileEvent>,
        draw_compaction: Option<TraceDrawCompaction>,
    ) {
        if commands.is_empty() && runtime_tile_events.is_empty() {
            return;
        }
        let mut updated = BTreeSet::new();
        let mut drawn = BTreeSet::new();
        for command in commands {
            match command {
                GpuCmdMsg::TileSlotKeyUpdate(msg) => {
                    for binding in &msg.updates {
                        updated.insert(binding.image_tile.tile_index);
                    }
                }
                GpuCmdMsg::DrawOp(draw_op) => {
                    drawn.insert(draw_op.image_tile.tile_index);
                }
                _ => {}
            }
        }
        let tile_timeline = if updated.is_empty() && drawn.is_empty() {
            None
        } else {
            let updated_tile_indices = updated.iter().copied().collect::<Vec<_>>();
            let drawn_tile_indices = drawn.iter().copied().collect::<Vec<_>>();
            let missing_updated_tile_indices =
                updated.difference(&drawn).copied().collect::<Vec<_>>();
            Some(TraceTileTimeline {
                updated_tile_indices,
                drawn_tile_indices,
                missing_updated_tile_indices,
            })
        };
        let mut trace_commands = Vec::with_capacity(commands.len());
        for command in commands {
            trace_commands.push(TraceGpuCmd::from(command.clone()));
        }
        self.output_frames.push(TraceOutputFrame {
            commands: trace_commands,
            tile_timeline,
            submit_render: submit_stats.map(|stats| TraceSubmitRender {
                render_cmd_count: stats.render_cmd_count,
                render_dst_tile_count: stats.render_dst_tile_count,
                render_tile_indices: stats.render_tile_indices.clone(),
            }),
            runtime_tile_events,
            draw_compaction,
        });
    }

    pub fn save_input_file(&self, input_path: &Path) -> Result<(), TraceIoError> {
        save_json_file(
            input_path,
            &TraceInputFile {
                version: TRACE_VERSION,
                frames: self.input_frames.clone(),
            },
        )
    }

    pub fn save_output_file(&self, output_path: &Path) -> Result<(), TraceIoError> {
        save_json_file(
            output_path,
            &TraceOutputFile {
                version: TRACE_VERSION,
                frames: self.output_frames.clone(),
            },
        )
    }

    pub fn load_input_file(path: &Path) -> Result<TraceInputFile, TraceIoError> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let trace_file: TraceInputFile = serde_json::from_reader(reader)?;
        if trace_file.version != TRACE_VERSION {
            return Err(TraceIoError::UnsupportedVersion(trace_file.version));
        }
        Ok(trace_file)
    }
}

impl TraceInputFrame {
    pub fn to_runtime(&self) -> (Vec<InputControlEvent<AppControl>>, Vec<InputRingSample>) {
        let mut controls = Vec::with_capacity(self.controls.len());
        for control in &self.controls {
            if let Some(runtime_control) = AppControl::from_serialized(control.clone()) {
                controls.push(InputControlEvent::Control(runtime_control));
            } else {
                eprintln!("trace replay skipped an unsupported serialized control event");
            }
        }

        let mut samples = Vec::with_capacity(self.samples.len());
        for sample in &self.samples {
            samples.push(InputRingSample::from(*sample));
        }

        (controls, samples)
    }
}

fn save_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), TraceIoError> {
    if let Some(parent_dir) = path.parent() {
        if !parent_dir.as_os_str().is_empty() {
            std::fs::create_dir_all(parent_dir)?;
        }
    }
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, value)?;
    Ok(())
}

fn trace_empty_tile_key() -> TraceTileKey {
    TraceTileKey::from(TileKey::EMPTY)
}

fn trace_draw_blend_mode_alpha() -> TraceDrawBlendMode {
    TraceDrawBlendMode::Alpha
}

fn trace_draw_frame_merge_none() -> TraceDrawFrameMergePolicy {
    TraceDrawFrameMergePolicy::None
}

fn trace_gpu_cmd_frame_merge_none() -> TraceGpuCmdFrameMergeTag {
    TraceGpuCmdFrameMergeTag::None
}

fn trace_write_blend_mode_normal() -> TraceWriteBlendMode {
    TraceWriteBlendMode::Normal
}

fn trace_composite_blend_mode_normal() -> TraceCompositeBlendMode {
    TraceCompositeBlendMode::Normal
}

fn trace_write_opacity_one() -> f32 {
    1.0
}

fn trace_rgb_red() -> [f32; 3] {
    [1.0, 0.0, 0.0]
}

fn trace_write_rgb_red() -> Option<[f32; 3]> {
    Some(trace_rgb_red())
}

impl From<UnitIntervalPoint> for TraceUnitIntervalPoint {
    fn from(value: UnitIntervalPoint) -> Self {
        Self {
            x: value.x,
            y: value.y,
        }
    }
}

impl From<TraceUnitIntervalPoint> for UnitIntervalPoint {
    fn from(value: TraceUnitIntervalPoint) -> Self {
        Self::new(value.x, value.y)
    }
}

impl From<BrushConfigValue> for TraceBrushConfigValue {
    fn from(value: BrushConfigValue) -> Self {
        match value {
            BrushConfigValue::ScalarF32(v) => Self::ScalarF32(v),
            BrushConfigValue::UnitIntervalCurve(points) => Self::UnitIntervalCurve(
                points
                    .into_iter()
                    .map(TraceUnitIntervalPoint::from)
                    .collect(),
            ),
        }
    }
}

impl From<TraceBrushConfigValue> for BrushConfigValue {
    fn from(value: TraceBrushConfigValue) -> Self {
        match value {
            TraceBrushConfigValue::ScalarF32(v) => Self::ScalarF32(v),
            TraceBrushConfigValue::UnitIntervalCurve(points) => {
                Self::UnitIntervalCurve(points.into_iter().map(UnitIntervalPoint::from).collect())
            }
        }
    }
}

impl From<AppControl> for TraceAppControl {
    fn from(value: AppControl) -> Self {
        match value {
            AppControl::StrokeBoundary { node_id, begin } => Self::StrokeBoundary {
                node_id: node_id.0,
                begin,
            },
            AppControl::SelectNode { node_id } => Self::SelectNode { node_id: node_id.0 },
            AppControl::CreateLayerAboveActive { kind } => Self::CreateLayerAboveActive {
                kind: match kind {
                    NewLayerKind::Raster => TraceNewLayerKind::Raster,
                    NewLayerKind::SolidColor { .. } => TraceNewLayerKind::SolidColor,
                },
            },
            AppControl::CreateGroupAboveActive => Self::CreateGroupAboveActive,
            AppControl::DeleteNode { node_id } => Self::DeleteNode { node_id: node_id.0 },
            AppControl::MoveNode { node_id, target } => Self::MoveNode {
                node_id: node_id.0,
                target_parent_id: target.parent_id.0,
                target_index: target.index,
            },
            AppControl::SetNodeVisibility { node_id, visible } => Self::SetNodeVisibility {
                node_id: node_id.0,
                visible,
            },
            AppControl::SetNodeOpacity { node_id, opacity } => Self::SetNodeOpacity {
                node_id: node_id.0,
                opacity,
            },
            AppControl::SetNodeBlendMode {
                node_id,
                blend_mode,
            } => Self::SetNodeBlendMode {
                node_id: node_id.0,
                blend_mode: match blend_mode {
                    UiBlendMode::Normal => TraceUiBlendMode::Normal,
                    UiBlendMode::Multiply => TraceUiBlendMode::Multiply,
                    UiBlendMode::Penetrate => TraceUiBlendMode::Penetrate,
                },
            },
            AppControl::SetActiveBrush { brush_id } => Self::SetActiveBrush {
                brush_id: brush_id.0,
            },
            AppControl::SetActiveBrushColorRgb { rgb } => Self::SetActiveBrushColorRgb { rgb },
            AppControl::SetActiveBrushErase { erase } => Self::SetActiveBrushErase { erase },
            AppControl::UpdateBrushConfig { brush_id, values } => Self::UpdateBrushConfig {
                brush_id: brush_id.0,
                values: values
                    .into_iter()
                    .map(TraceBrushConfigValue::from)
                    .collect(),
            },
            AppControl::MoveActiveNodeUp => Self::MoveActiveNodeUp,
            AppControl::MoveActiveNodeDown => Self::MoveActiveNodeDown,
        }
    }
}

impl From<TraceAppControl> for AppControl {
    fn from(value: TraceAppControl) -> Self {
        match value {
            TraceAppControl::StrokeBoundary { node_id, begin } => Self::StrokeBoundary {
                node_id: NodeId(node_id),
                begin,
            },
            TraceAppControl::SelectNode { node_id } => Self::SelectNode {
                node_id: NodeId(node_id),
            },
            TraceAppControl::CreateLayerAboveActive { kind } => Self::CreateLayerAboveActive {
                kind: match kind {
                    TraceNewLayerKind::Raster => NewLayerKind::Raster,
                    TraceNewLayerKind::SolidColor => NewLayerKind::SolidColor {
                        color: [1.0, 1.0, 1.0, 1.0],
                    },
                },
            },
            TraceAppControl::CreateGroupAboveActive => Self::CreateGroupAboveActive,
            TraceAppControl::DeleteNode { node_id } => Self::DeleteNode {
                node_id: NodeId(node_id),
            },
            TraceAppControl::MoveNode {
                node_id,
                target_parent_id,
                target_index,
            } => Self::MoveNode {
                node_id: NodeId(node_id),
                target: LayerMoveTarget {
                    parent_id: NodeId(target_parent_id),
                    index: target_index,
                },
            },
            TraceAppControl::SetNodeVisibility { node_id, visible } => Self::SetNodeVisibility {
                node_id: NodeId(node_id),
                visible,
            },
            TraceAppControl::SetNodeOpacity { node_id, opacity } => Self::SetNodeOpacity {
                node_id: NodeId(node_id),
                opacity,
            },
            TraceAppControl::SetNodeBlendMode {
                node_id,
                blend_mode,
            } => Self::SetNodeBlendMode {
                node_id: NodeId(node_id),
                blend_mode: match blend_mode {
                    TraceUiBlendMode::Normal => UiBlendMode::Normal,
                    TraceUiBlendMode::Multiply => UiBlendMode::Multiply,
                    TraceUiBlendMode::Penetrate => UiBlendMode::Penetrate,
                },
            },
            TraceAppControl::SetActiveBrush { brush_id } => Self::SetActiveBrush {
                brush_id: BrushId(brush_id),
            },
            TraceAppControl::SetActiveBrushColorRgb { rgb } => Self::SetActiveBrushColorRgb { rgb },
            TraceAppControl::SetActiveBrushErase { erase } => Self::SetActiveBrushErase { erase },
            TraceAppControl::UpdateBrushConfig { brush_id, values } => Self::UpdateBrushConfig {
                brush_id: BrushId(brush_id),
                values: values.into_iter().map(BrushConfigValue::from).collect(),
            },
            TraceAppControl::MoveActiveNodeUp => Self::MoveActiveNodeUp,
            TraceAppControl::MoveActiveNodeDown => Self::MoveActiveNodeDown,
        }
    }
}

impl From<TileKey> for TraceTileKey {
    fn from(value: TileKey) -> Self {
        Self {
            backend: value.backend_index(),
            generation: value.generation_index(),
            slot: value.slot_index(),
        }
    }
}

impl From<TraceTileKey> for TileKey {
    fn from(value: TraceTileKey) -> Self {
        TileKey::from_parts(value.backend, value.generation, value.slot)
    }
}

impl From<InputRingSample> for TraceInputSample {
    fn from(value: InputRingSample) -> Self {
        let device = match value.device {
            InputDeviceKind::Pen => TraceInputDeviceKind::Pen,
            InputDeviceKind::Cursor => TraceInputDeviceKind::Cursor,
            InputDeviceKind::Finger(index) => TraceInputDeviceKind::Finger { index },
        };
        Self {
            epoch: value.epoch.0,
            time_ns: value.time_ns,
            device,
            cursor_x: value.cursor.cursor.x,
            cursor_y: value.cursor.cursor.y,
            tilt_x: value.cursor.tilt.x,
            tilt_y: value.cursor.tilt.y,
            pressure: value.cursor.pressure,
            twist: value.cursor.twist,
        }
    }
}

impl From<TraceInputSample> for InputRingSample {
    fn from(value: TraceInputSample) -> Self {
        let device = match value.device {
            TraceInputDeviceKind::Pen => InputDeviceKind::Pen,
            TraceInputDeviceKind::Cursor => InputDeviceKind::Cursor,
            TraceInputDeviceKind::Finger { index } => InputDeviceKind::Finger(index),
        };
        Self {
            epoch: EpochId(value.epoch),
            time_ns: value.time_ns,
            device,
            cursor: MappedCursor {
                cursor: CanvasVec2::new(value.cursor_x, value.cursor_y),
                tilt: RadianVec2::new(value.tilt_x, value.tilt_y),
                pressure: value.pressure,
                twist: value.twist,
            },
        }
    }
}

impl From<GpuCmdMsg> for TraceGpuCmd {
    fn from(value: GpuCmdMsg) -> Self {
        match value {
            GpuCmdMsg::ExpandAtlasBackend(msg) => Self::ExpandAtlasBackend(TraceExpandAtlasBackendMsg {
                src_backend_id: msg.src_backend_id,
                dst_backend_id: msg.dst_backend_id,
                src_layout: msg.src_layout.into(),
                dst_layout: msg.dst_layout.into(),
            }),
            GpuCmdMsg::DrawOp(draw_op) => Self::DrawOp(TraceDrawOp {
                node_id: draw_op.image_tile.image_id.node_id().map(|id| id.0),
                image_tile: draw_op.image_tile.into(),
                stroke_id: draw_op.stroke_id.0,
                tile_key: draw_op.tile_key.into(),
                origin_tile_key: draw_op.origin_tile.into(),
                ref_image_tile_key: draw_op.ref_image.map(|ref_image| ref_image.tile_key.into()),
                input: draw_op.input,
                blend_mode: draw_op.stroke_ctx.map(|ctx| match ctx.blend_mode {
                    BlendMode::Alpha => TraceDrawBlendMode::Alpha,
                    BlendMode::Additive => TraceDrawBlendMode::Additive,
                    BlendMode::Replace => TraceDrawBlendMode::Replace,
                    _ => {
                        debug_assert!(DrawOp::supports_blend_mode(ctx.blend_mode));
                        unreachable!("trace draw only supports draw-op blend mode subset");
                    }
                }),
                frame_merge: draw_op.stroke_ctx.map(|ctx| match ctx.frame_merge {
                    DrawFrameMergePolicy::None => TraceDrawFrameMergePolicy::None,
                    DrawFrameMergePolicy::KeepLastInFrameByNodeTileBrush => {
                        TraceDrawFrameMergePolicy::KeepLastInFrameByNodeTileBrush
                    }
                }),
                rgb: draw_op.stroke_ctx.map(|ctx| ctx.rgb),
                erase: false,
                brush_id: draw_op.stroke_ctx.map(|ctx| ctx.brush_id.0),
            }),
            GpuCmdMsg::CopyOp(copy_op) => Self::CopyOp(TraceCopyOp {
                src_tile_key: copy_op.src_tile_key.into(),
                dst_tile_key: copy_op.dst_tile_key.into(),
                frame_merge: match copy_op.frame_merge {
                    GpuCmdFrameMergeTag::None => TraceGpuCmdFrameMergeTag::None,
                    GpuCmdFrameMergeTag::KeepFirstInFrameByDstTile => {
                        TraceGpuCmdFrameMergeTag::KeepFirstInFrameByDstTile
                    }
                    GpuCmdFrameMergeTag::KeepLastInFrameByDstTile => {
                        TraceGpuCmdFrameMergeTag::KeepLastInFrameByDstTile
                    }
                },
            }),
            GpuCmdMsg::WriteOp(write_op) => Self::WriteOp(TraceWriteOp {
                src_tile_key: write_op.src_tile_key.into(),
                node_id: write_op
                    .image_tile
                    .image_id
                    .node_id()
                    .map(|id| id.0)
                    .unwrap_or(0),
                image_tile: write_op.image_tile.into(),
                dst_tile_key: write_op.dst_tile_key.into(),
                frame_merge: match write_op.frame_merge {
                    GpuCmdFrameMergeTag::None => TraceGpuCmdFrameMergeTag::None,
                    GpuCmdFrameMergeTag::KeepFirstInFrameByDstTile => {
                        TraceGpuCmdFrameMergeTag::KeepFirstInFrameByDstTile
                    }
                    GpuCmdFrameMergeTag::KeepLastInFrameByDstTile => {
                        TraceGpuCmdFrameMergeTag::KeepLastInFrameByDstTile
                    }
                },
                blend_mode: match write_op.kind {
                    WriteKind::Paint => TraceWriteBlendMode::Normal,
                    WriteKind::Erase { .. } => TraceWriteBlendMode::Erase,
                },
                opacity: write_op.opacity,
                rgb: write_op.rgb,
                origin_tile_key: match write_op.kind {
                    WriteKind::Paint => None,
                    WriteKind::Erase { origin_tile_key } => Some(origin_tile_key.into()),
                },
            }),
            GpuCmdMsg::CompositeOp(composite_op) => Self::CompositeOp(TraceCompositeOp {
                base_tile_key: composite_op.base_tile_key.into(),
                overlay_tile_key: composite_op.overlay_tile_key.into(),
                dst_tile_key: composite_op.dst_tile_key.into(),
                blend_mode: match composite_op.blend_mode {
                    BlendMode::Normal => TraceCompositeBlendMode::Normal,
                    BlendMode::Multiply => TraceCompositeBlendMode::Multiply,
                    _ => {
                        debug_assert!(CompositeOp::supports_blend_mode(composite_op.blend_mode));
                        unreachable!(
                            "trace composite only supports composite-op blend mode subset"
                        );
                    }
                },
                opacity: composite_op.opacity,
            }),
            GpuCmdMsg::ClearOp(clear_op) => Self::ClearOp(TraceClearOp {
                tile_key: clear_op.tile_key.into(),
            }),
            GpuCmdMsg::RenderTreeUpdated(message) => {
                Self::RenderTreeUpdated(TraceRenderTreeUpdatedMsg {
                    generation: message.generation.0,
                    dirty_render_caches: message
                        .dirty_render_caches
                        .into_iter()
                        .map(|node_id| node_id.0)
                        .collect(),
                })
            }
            GpuCmdMsg::TileSlotKeyUpdate(message) => {
                Self::TileSlotKeyUpdate(TraceTileSlotKeyUpdateMsg {
                    updates: message
                        .updates
                        .into_iter()
                        .map(|binding| TraceImageTileBinding {
                            image_tile: binding.image_tile.into(),
                            tile_key: binding.tile_key.into(),
                        })
                        .collect(),
                })
            }
        }
    }
}

impl From<TraceGpuCmd> for GpuCmdMsg {
    fn from(value: TraceGpuCmd) -> Self {
        match value {
            TraceGpuCmd::ExpandAtlasBackend(msg) => {
                Self::ExpandAtlasBackend(thread_protocol::ExpandAtlasBackendMsg {
                    src_backend_id: msg.src_backend_id,
                    dst_backend_id: msg.dst_backend_id,
                    src_layout: msg.src_layout.into(),
                    dst_layout: msg.dst_layout.into(),
                })
            }
            TraceGpuCmd::DrawOp(draw_op) => Self::DrawOp(DrawOp {
                stroke_id: StrokeId(draw_op.stroke_id),
                stroke_ctx: match (
                    draw_op.node_id,
                    draw_op.blend_mode,
                    draw_op.frame_merge,
                    draw_op.rgb,
                    draw_op.brush_id,
                ) {
                    (
                        Some(_node_id),
                        Some(blend_mode),
                        Some(frame_merge),
                        Some(rgb),
                        Some(brush_id),
                    ) => Some(DrawStrokeCtx {
                        blend_mode: match blend_mode {
                            TraceDrawBlendMode::Alpha => BlendMode::Alpha,
                            TraceDrawBlendMode::Additive => BlendMode::Additive,
                            TraceDrawBlendMode::Replace => BlendMode::Replace,
                        },
                        frame_merge: match frame_merge {
                            TraceDrawFrameMergePolicy::None => DrawFrameMergePolicy::None,
                            TraceDrawFrameMergePolicy::KeepLastInFrameByNodeTileBrush => {
                                DrawFrameMergePolicy::KeepLastInFrameByNodeTileBrush
                            }
                        },
                        rgb,
                        brush_id: BrushId(brush_id),
                    }),
                    (None, None, None, None, None) => None,
                    _ => {
                        debug_assert!(
                            false,
                            "trace draw op stroke ctx fields must be fully present or fully absent"
                        );
                        None
                    }
                },
                image_tile: draw_op.image_tile.into(),
                tile_key: draw_op.tile_key.into(),
                origin_tile: draw_op.origin_tile_key.into(),
                ref_image: draw_op.ref_image_tile_key.map(|tile_key| RefImage {
                    tile_key: tile_key.into(),
                }),
                input: draw_op.input,
            }),
            TraceGpuCmd::CopyOp(copy_op) => Self::CopyOp(CopyOp {
                src_tile_key: copy_op.src_tile_key.into(),
                dst_tile_key: copy_op.dst_tile_key.into(),
                frame_merge: match copy_op.frame_merge {
                    TraceGpuCmdFrameMergeTag::None => GpuCmdFrameMergeTag::None,
                    TraceGpuCmdFrameMergeTag::KeepFirstInFrameByDstTile => {
                        GpuCmdFrameMergeTag::KeepFirstInFrameByDstTile
                    }
                    TraceGpuCmdFrameMergeTag::KeepLastInFrameByDstTile => {
                        GpuCmdFrameMergeTag::KeepLastInFrameByDstTile
                    }
                },
            }),
            TraceGpuCmd::WriteOp(write_op) => Self::WriteOp(WriteOp {
                src_tile_key: write_op.src_tile_key.into(),
                image_tile: write_op.image_tile.into(),
                dst_tile_key: write_op.dst_tile_key.into(),
                frame_merge: match write_op.frame_merge {
                    TraceGpuCmdFrameMergeTag::None => GpuCmdFrameMergeTag::None,
                    TraceGpuCmdFrameMergeTag::KeepFirstInFrameByDstTile => {
                        GpuCmdFrameMergeTag::KeepFirstInFrameByDstTile
                    }
                    TraceGpuCmdFrameMergeTag::KeepLastInFrameByDstTile => {
                        GpuCmdFrameMergeTag::KeepLastInFrameByDstTile
                    }
                },
                blend_mode: BlendMode::Normal,
                kind: match write_op.blend_mode {
                    TraceWriteBlendMode::Normal => WriteKind::Paint,
                    TraceWriteBlendMode::Erase => WriteKind::Erase {
                        origin_tile_key: write_op
                            .origin_tile_key
                            .map(Into::into)
                            .unwrap_or(TileKey::EMPTY),
                    },
                },
                opacity: write_op.opacity,
                rgb: write_op.rgb,
            }),
            TraceGpuCmd::CompositeOp(composite_op) => Self::CompositeOp(CompositeOp {
                base_tile_key: composite_op.base_tile_key.into(),
                overlay_tile_key: composite_op.overlay_tile_key.into(),
                dst_tile_key: composite_op.dst_tile_key.into(),
                blend_mode: match composite_op.blend_mode {
                    TraceCompositeBlendMode::Normal => BlendMode::Normal,
                    TraceCompositeBlendMode::Multiply => BlendMode::Multiply,
                },
                opacity: composite_op.opacity,
            }),
            TraceGpuCmd::ClearOp(clear_op) => Self::ClearOp(ClearOp {
                tile_key: clear_op.tile_key.into(),
            }),
            TraceGpuCmd::RenderTreeUpdated(message) => {
                Self::RenderTreeUpdated(RenderTreeUpdatedMsg {
                    generation: RenderTreeGeneration(message.generation),
                    dirty_render_caches: message
                        .dirty_render_caches
                        .into_iter()
                        .map(NodeId)
                        .collect(),
                })
            }
            TraceGpuCmd::TileSlotKeyUpdate(message) => {
                Self::TileSlotKeyUpdate(TileSlotKeyUpdateMsg {
                    updates: message
                        .updates
                        .into_iter()
                        .map(|binding| ImageTileBinding {
                            image_tile: binding.image_tile.into(),
                            tile_key: TileKey::from(binding.tile_key),
                        })
                        .collect(),
                })
            }
        }
    }
}

impl From<ImageTileKey> for TraceImageTileKey {
    fn from(value: ImageTileKey) -> Self {
        Self {
            image_id: value.image_id.0,
            tile_index: value.tile_index,
        }
    }
}

impl From<TraceImageTileKey> for ImageTileKey {
    fn from(value: TraceImageTileKey) -> Self {
        ImageTileKey::new(glaphica_core::ImageId(value.image_id), value.tile_index)
    }
}

#[cfg(test)]
mod tests {
    use super::{TraceAppControl, TraceBrushConfigValue};
    use crate::AppControl;
    use brushes::{BrushConfigValue, UnitIntervalPoint};
    use glaphica_core::{BrushId, NodeId};

    #[test]
    fn brush_config_control_roundtrip() {
        let control = AppControl::UpdateBrushConfig {
            brush_id: BrushId(7),
            values: vec![
                BrushConfigValue::ScalarF32(0.42),
                BrushConfigValue::UnitIntervalCurve(vec![
                    UnitIntervalPoint::new(0.0, 0.1),
                    UnitIntervalPoint::new(1.0, 0.9),
                ]),
            ],
        };
        let trace = TraceAppControl::from(control.clone());
        let replay = AppControl::from(trace);
        assert_eq!(replay, control);
    }

    #[test]
    fn active_brush_control_roundtrip() {
        let trace = TraceAppControl::SetActiveBrushColorRgb {
            rgb: [0.2, 0.3, 0.4],
        };
        let control = AppControl::from(trace.clone());
        let back = TraceAppControl::from(control);
        assert_eq!(back, trace);

        let trace = TraceAppControl::UpdateBrushConfig {
            brush_id: 3,
            values: vec![TraceBrushConfigValue::ScalarF32(1.0)],
        };
        let control = AppControl::from(trace.clone());
        let back = TraceAppControl::from(control);
        assert_eq!(back, trace);
    }

    #[test]
    fn delete_node_control_roundtrip() {
        let control = AppControl::DeleteNode {
            node_id: NodeId(11),
        };
        let trace = TraceAppControl::from(control.clone());
        let replay = AppControl::from(trace);
        assert_eq!(replay, control);
    }
}
