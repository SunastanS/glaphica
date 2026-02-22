use std::convert::Infallible;
use std::sync::Arc;

use crate::atlas::{TileAtlasFormat, TileAtlasUsage, TilePayloadKind};
use crate::{TILE_SIZE, TileIngestError};

pub trait TileFormatSpec {
    const PAYLOAD_KIND: TilePayloadKind;
    const FORMAT: TileAtlasFormat;
}

pub trait TilePayloadSpec {
    type UploadPayload;
}

pub trait TileUploadFormatSpec:
    TileFormatSpec + TilePayloadSpec<UploadPayload = Arc<[u8]>>
{
    fn validate_ingest_contract(usage: TileAtlasUsage) -> Result<(), TileIngestError>;

    fn validate_upload_bytes(bytes: &[u8]) -> Result<(), TileIngestError>;
}

#[derive(Debug)]
pub struct Rgba8Spec;
#[derive(Debug)]
pub struct Rgba8SrgbSpec;
#[derive(Debug)]
pub struct Bgra8Spec;
#[derive(Debug)]
pub struct Bgra8SrgbSpec;
#[derive(Debug)]
pub struct R32FloatSpec;
#[derive(Debug)]
pub struct R8UintSpec;

impl TileFormatSpec for Rgba8Spec {
    const PAYLOAD_KIND: TilePayloadKind = TilePayloadKind::Rgba8;
    const FORMAT: TileAtlasFormat = TileAtlasFormat::Rgba8Unorm;
}

impl TilePayloadSpec for Rgba8Spec {
    type UploadPayload = Arc<[u8]>;
}

impl TileUploadFormatSpec for Rgba8Spec {
    fn validate_ingest_contract(usage: TileAtlasUsage) -> Result<(), TileIngestError> {
        if !usage.contains_copy_dst() {
            return Err(TileIngestError::MissingCopyDstUsage);
        }
        Ok(())
    }

    fn validate_upload_bytes(bytes: &[u8]) -> Result<(), TileIngestError> {
        if bytes.len() != rgba8_tile_len() {
            return Err(TileIngestError::BufferLengthMismatch);
        }
        Ok(())
    }
}

impl TileFormatSpec for Rgba8SrgbSpec {
    const PAYLOAD_KIND: TilePayloadKind = TilePayloadKind::Rgba8;
    const FORMAT: TileAtlasFormat = TileAtlasFormat::Rgba8UnormSrgb;
}

impl TilePayloadSpec for Rgba8SrgbSpec {
    type UploadPayload = Arc<[u8]>;
}

impl TileUploadFormatSpec for Rgba8SrgbSpec {
    fn validate_ingest_contract(usage: TileAtlasUsage) -> Result<(), TileIngestError> {
        Rgba8Spec::validate_ingest_contract(usage)
    }

    fn validate_upload_bytes(bytes: &[u8]) -> Result<(), TileIngestError> {
        Rgba8Spec::validate_upload_bytes(bytes)
    }
}

impl TileFormatSpec for R32FloatSpec {
    const PAYLOAD_KIND: TilePayloadKind = TilePayloadKind::R32Float;
    const FORMAT: TileAtlasFormat = TileAtlasFormat::R32Float;
}

impl TileFormatSpec for Bgra8Spec {
    const PAYLOAD_KIND: TilePayloadKind = TilePayloadKind::Rgba8;
    const FORMAT: TileAtlasFormat = TileAtlasFormat::Bgra8Unorm;
}

impl TilePayloadSpec for Bgra8Spec {
    type UploadPayload = Arc<[u8]>;
}

impl TileUploadFormatSpec for Bgra8Spec {
    fn validate_ingest_contract(usage: TileAtlasUsage) -> Result<(), TileIngestError> {
        Rgba8Spec::validate_ingest_contract(usage)
    }

    fn validate_upload_bytes(bytes: &[u8]) -> Result<(), TileIngestError> {
        Rgba8Spec::validate_upload_bytes(bytes)
    }
}

impl TileFormatSpec for Bgra8SrgbSpec {
    const PAYLOAD_KIND: TilePayloadKind = TilePayloadKind::Rgba8;
    const FORMAT: TileAtlasFormat = TileAtlasFormat::Bgra8UnormSrgb;
}

impl TilePayloadSpec for Bgra8SrgbSpec {
    type UploadPayload = Arc<[u8]>;
}

impl TileUploadFormatSpec for Bgra8SrgbSpec {
    fn validate_ingest_contract(usage: TileAtlasUsage) -> Result<(), TileIngestError> {
        Rgba8Spec::validate_ingest_contract(usage)
    }

    fn validate_upload_bytes(bytes: &[u8]) -> Result<(), TileIngestError> {
        Rgba8Spec::validate_upload_bytes(bytes)
    }
}

impl TilePayloadSpec for R32FloatSpec {
    type UploadPayload = Infallible;
}

impl TileFormatSpec for R8UintSpec {
    const PAYLOAD_KIND: TilePayloadKind = TilePayloadKind::R8Uint;
    const FORMAT: TileAtlasFormat = TileAtlasFormat::R8Uint;
}

impl TilePayloadSpec for R8UintSpec {
    type UploadPayload = Infallible;
}

pub(crate) fn rgba8_tile_len() -> usize {
    (TILE_SIZE as usize) * (TILE_SIZE as usize) * 4
}
