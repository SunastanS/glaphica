use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use glaphica_core::{
    BrushId, CanvasVec2, EpochId, InputDeviceKind, MappedCursor, NodeId, RadianVec2,
    RenderTreeGeneration, StrokeId, TileKey,
};
use serde::{Deserialize, Serialize};
use thread_protocol::{
    ClearOp, CompositeOp, CopyOp, DrawBlendMode, DrawFrameMergePolicy, DrawOp, GpuCmdFrameMergeTag,
    GpuCmdMsg, InputControlEvent, InputRingSample, RefImage, RenderTreeUpdatedMsg,
    TileSlotKeyUpdateMsg, WriteBlendMode, WriteOp,
};

use crate::StrokeControl;

const TRACE_VERSION: u32 = 1;

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
    pub controls: Vec<TraceStrokeControl>,
    pub samples: Vec<TraceInputSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceOutputFrame {
    pub commands: Vec<TraceGpuCmd>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TraceStrokeControl {
    pub node_id: u64,
    pub begin: bool,
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
    DrawOp(TraceDrawOp),
    CopyOp(TraceCopyOp),
    WriteOp(TraceWriteOp),
    CompositeOp(TraceCompositeOp),
    ClearOp(TraceClearOp),
    RenderTreeUpdated(TraceRenderTreeUpdatedMsg),
    TileSlotKeyUpdate(TraceTileSlotKeyUpdateMsg),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceDrawOp {
    pub node_id: u64,
    #[serde(default)]
    pub stroke_id: u64,
    pub tile_index: usize,
    pub tile_key: TraceTileKey,
    #[serde(default = "trace_draw_blend_mode_alpha")]
    pub blend_mode: TraceDrawBlendMode,
    #[serde(default = "trace_draw_frame_merge_none")]
    pub frame_merge: TraceDrawFrameMergePolicy,
    #[serde(default = "trace_empty_tile_key")]
    pub origin_tile_key: TraceTileKey,
    pub ref_image_tile_key: Option<TraceTileKey>,
    pub input: Vec<f32>,
    #[serde(default = "trace_rgb_red")]
    pub rgb: [f32; 3],
    #[serde(default)]
    pub erase: bool,
    pub brush_id: u64,
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
    #[serde(default = "trace_write_blend_mode_normal")]
    pub blend_mode: TraceWriteBlendMode,
    #[serde(default = "trace_write_opacity_one")]
    pub opacity: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TraceWriteBlendMode {
    Normal,
    Erase,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TraceClearOp {
    pub tile_key: TraceTileKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceRenderTreeUpdatedMsg {
    pub generation: u64,
    pub dirty_branch_caches: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceTileSlotKeyUpdateMsg {
    pub updates: Vec<(u64, usize, TraceTileKey)>,
}

#[derive(Debug, Default)]
pub struct TraceRecorder {
    input_frames: Vec<TraceInputFrame>,
    output_frames: Vec<TraceOutputFrame>,
}

impl TraceRecorder {
    pub fn record_input_frame(
        &mut self,
        controls: &[InputControlEvent<StrokeControl>],
        samples: &[InputRingSample],
    ) {
        if controls.is_empty() && samples.is_empty() {
            return;
        }
        let mut trace_controls = Vec::with_capacity(controls.len());
        for control in controls {
            let InputControlEvent::Control(control) = control;
            trace_controls.push(TraceStrokeControl {
                node_id: control.node_id.0,
                begin: control.begin,
            });
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

    pub fn record_output_frame(&mut self, commands: &[GpuCmdMsg]) {
        if commands.is_empty() {
            return;
        }
        let mut trace_commands = Vec::with_capacity(commands.len());
        for command in commands {
            trace_commands.push(TraceGpuCmd::from(command.clone()));
        }
        self.output_frames.push(TraceOutputFrame {
            commands: trace_commands,
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
    pub fn to_runtime(&self) -> (Vec<InputControlEvent<StrokeControl>>, Vec<InputRingSample>) {
        let mut controls = Vec::with_capacity(self.controls.len());
        for control in &self.controls {
            controls.push(InputControlEvent::Control(StrokeControl {
                node_id: NodeId(control.node_id),
                begin: control.begin,
            }));
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

fn trace_write_opacity_one() -> f32 {
    1.0
}

fn trace_rgb_red() -> [f32; 3] {
    [1.0, 0.0, 0.0]
}

fn trace_write_rgb_red() -> Option<[f32; 3]> {
    Some(trace_rgb_red())
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
            GpuCmdMsg::DrawOp(draw_op) => Self::DrawOp(TraceDrawOp {
                node_id: draw_op.node_id.0,
                stroke_id: draw_op.stroke_id.0,
                tile_index: draw_op.tile_index,
                tile_key: TraceTileKey::from(draw_op.tile_key),
                blend_mode: match draw_op.blend_mode {
                    DrawBlendMode::Alpha => TraceDrawBlendMode::Alpha,
                    DrawBlendMode::Additive => TraceDrawBlendMode::Additive,
                    DrawBlendMode::Replace => TraceDrawBlendMode::Replace,
                },
                frame_merge: match draw_op.frame_merge {
                    DrawFrameMergePolicy::None => TraceDrawFrameMergePolicy::None,
                    DrawFrameMergePolicy::KeepLastInFrameByNodeTileBrush => {
                        TraceDrawFrameMergePolicy::KeepLastInFrameByNodeTileBrush
                    }
                },
                origin_tile_key: TraceTileKey::from(draw_op.origin_tile),
                ref_image_tile_key: draw_op.ref_image.map(|ref_image| ref_image.tile_key.into()),
                input: draw_op.input,
                rgb: draw_op.rgb,
                erase: draw_op.erase,
                brush_id: draw_op.brush_id.0,
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
                blend_mode: match write_op.blend_mode {
                    WriteBlendMode::Normal => TraceWriteBlendMode::Normal,
                    WriteBlendMode::Erase => TraceWriteBlendMode::Erase,
                },
                opacity: write_op.opacity,
                rgb: write_op.rgb,
                origin_tile_key: write_op.origin_tile_key.map(Into::into),
            }),
            GpuCmdMsg::CompositeOp(composite_op) => Self::CompositeOp(TraceCompositeOp {
                base_tile_key: composite_op.base_tile_key.into(),
                overlay_tile_key: composite_op.overlay_tile_key.into(),
                dst_tile_key: composite_op.dst_tile_key.into(),
                blend_mode: match composite_op.blend_mode {
                    WriteBlendMode::Normal => TraceWriteBlendMode::Normal,
                    WriteBlendMode::Erase => TraceWriteBlendMode::Erase,
                },
                opacity: composite_op.opacity,
            }),
            GpuCmdMsg::ClearOp(clear_op) => Self::ClearOp(TraceClearOp {
                tile_key: clear_op.tile_key.into(),
            }),
            GpuCmdMsg::RenderTreeUpdated(message) => {
                Self::RenderTreeUpdated(TraceRenderTreeUpdatedMsg {
                    generation: message.generation.0,
                    dirty_branch_caches: message
                        .dirty_branch_caches
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
                        .map(|(node_id, tile_index, tile_key)| {
                            (node_id.0, tile_index, tile_key.into())
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
            TraceGpuCmd::DrawOp(draw_op) => Self::DrawOp(DrawOp {
                node_id: NodeId(draw_op.node_id),
                stroke_id: StrokeId(draw_op.stroke_id),
                tile_index: draw_op.tile_index,
                tile_key: draw_op.tile_key.into(),
                blend_mode: match draw_op.blend_mode {
                    TraceDrawBlendMode::Alpha => DrawBlendMode::Alpha,
                    TraceDrawBlendMode::Additive => DrawBlendMode::Additive,
                    TraceDrawBlendMode::Replace => DrawBlendMode::Replace,
                },
                frame_merge: match draw_op.frame_merge {
                    TraceDrawFrameMergePolicy::None => DrawFrameMergePolicy::None,
                    TraceDrawFrameMergePolicy::KeepLastInFrameByNodeTileBrush => {
                        DrawFrameMergePolicy::KeepLastInFrameByNodeTileBrush
                    }
                },
                origin_tile: draw_op.origin_tile_key.into(),
                ref_image: draw_op.ref_image_tile_key.map(|tile_key| RefImage {
                    tile_key: tile_key.into(),
                }),
                input: draw_op.input,
                rgb: draw_op.rgb,
                erase: draw_op.erase,
                brush_id: BrushId(draw_op.brush_id),
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
                blend_mode: match write_op.blend_mode {
                    TraceWriteBlendMode::Normal => WriteBlendMode::Normal,
                    TraceWriteBlendMode::Erase => WriteBlendMode::Erase,
                },
                opacity: write_op.opacity,
                rgb: write_op.rgb,
                origin_tile_key: write_op.origin_tile_key.map(Into::into),
            }),
            TraceGpuCmd::CompositeOp(composite_op) => Self::CompositeOp(CompositeOp {
                base_tile_key: composite_op.base_tile_key.into(),
                overlay_tile_key: composite_op.overlay_tile_key.into(),
                dst_tile_key: composite_op.dst_tile_key.into(),
                blend_mode: match composite_op.blend_mode {
                    TraceWriteBlendMode::Normal => WriteBlendMode::Normal,
                    TraceWriteBlendMode::Erase => WriteBlendMode::Erase,
                },
                opacity: composite_op.opacity,
            }),
            TraceGpuCmd::ClearOp(clear_op) => Self::ClearOp(ClearOp {
                tile_key: clear_op.tile_key.into(),
            }),
            TraceGpuCmd::RenderTreeUpdated(message) => {
                Self::RenderTreeUpdated(RenderTreeUpdatedMsg {
                    generation: RenderTreeGeneration(message.generation),
                    dirty_branch_caches: message
                        .dirty_branch_caches
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
                        .map(|(node_id, tile_index, tile_key)| {
                            (NodeId(node_id), tile_index, TileKey::from(tile_key))
                        })
                        .collect(),
                })
            }
        }
    }
}
