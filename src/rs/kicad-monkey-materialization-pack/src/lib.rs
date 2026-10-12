//! Packed encoding for `kicad_monkey.materialization.a0`.
//!
//! The pack is the canonical serialized materialization. It depends only on
//! the generated contracts, so viewers (through WASM) and simulation engines
//! read it without the KiCad parser.
//!
//! Byte layout, little-endian:
//!
//! | Offset | Size | Field |
//! |---|---|---|
//! | 0 | 8 | magic `KMMATPK\0` |
//! | 8 | 2 | format version, `0` for a0 |
//! | 10 | 2 | flags, `0` |
//! | 12 | 4 | header length in bytes |
//! | 16 | header length | header: the document as JSON with `operations` empty |
//! | aligned to 8 | rest | operation columns (slice 5) |
//!
//! The header carries scope, stack, nets, components, binding, and the
//! chunk directory, so a reader can seek to the chunks it needs. The decoder
//! validates it with the contract's operation-independent rules before
//! returning it.

#![forbid(unsafe_code)]

use kicad_monkey_contracts::ValidationError;
use kicad_monkey_contracts::generated::materialization_document::MaterializationDocumentA0;
use kicad_monkey_contracts::materialization_contract::validate_materialization_header;
use std::fmt;

/// File magic.
pub const PACK_MAGIC: [u8; 8] = *b"KMMATPK\0";
/// Format version of the a0 pack.
pub const PACK_FORMAT_VERSION: u16 = 0;
/// Size of the fixed preamble before the header.
pub const PREAMBLE_BYTES: usize = 16;
/// Column sections start on this alignment.
pub const COLUMN_ALIGNMENT: usize = 8;

/// Reader ceilings. Readers reject input beyond them before allocating.
#[derive(Clone, Copy, Debug)]
pub struct PackLimits {
    pub max_header_bytes: usize,
}

impl Default for PackLimits {
    fn default() -> Self {
        Self {
            max_header_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Pack encode or decode failure.
#[derive(Debug)]
pub enum PackError {
    BadMagic,
    UnsupportedVersion(u16),
    UnsupportedFlags(u16),
    Truncated,
    HeaderTooLarge(usize),
    Header(serde_json::Error),
    /// The header decodes but breaks a contract rule.
    Invalid(ValidationError),
}

impl fmt::Display for PackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => formatter.write_str("not a materialization pack"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported pack format version {version}")
            }
            Self::UnsupportedFlags(flags) => write!(formatter, "unsupported pack flags {flags}"),
            Self::Truncated => formatter.write_str("pack is truncated"),
            Self::HeaderTooLarge(bytes) => {
                write!(formatter, "pack header of {bytes} bytes exceeds limit")
            }
            Self::Header(error) => write!(formatter, "pack header: {error}"),
            Self::Invalid(error) => write!(formatter, "pack header: {error}"),
        }
    }
}

impl std::error::Error for PackError {}

/// Decoded pack header and where its columns begin.
pub struct PackHeader {
    pub document: MaterializationDocumentA0,
    pub columns_offset: usize,
}

fn aligned(offset: usize) -> usize {
    offset.div_ceil(COLUMN_ALIGNMENT) * COLUMN_ALIGNMENT
}

/// Encode the preamble and header of `document`. Operations are not part of
/// the header; columns are appended by the slice-5 writer.
pub fn encode_header(document: &MaterializationDocumentA0) -> Result<Vec<u8>, PackError> {
    let mut header = document.clone();
    header.operations.clear();
    let json = serde_json::to_vec(&header).map_err(PackError::Header)?;
    let length = u32::try_from(json.len()).map_err(|_| PackError::HeaderTooLarge(json.len()))?;
    let mut bytes = Vec::with_capacity(aligned(PREAMBLE_BYTES + json.len()));
    bytes.extend_from_slice(&PACK_MAGIC);
    bytes.extend_from_slice(&PACK_FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(&json);
    bytes.resize(aligned(bytes.len()), 0);
    Ok(bytes)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, PackError> {
    let field = bytes.get(offset..offset + 2).ok_or(PackError::Truncated)?;
    Ok(u16::from_le_bytes([field[0], field[1]]))
}

/// Decode the preamble and header, checking magic, version, flags, and the
/// header ceiling before reading the header, then the header's contract
/// rules.
pub fn decode_header(bytes: &[u8], limits: PackLimits) -> Result<PackHeader, PackError> {
    let magic = bytes.get(..8).ok_or(PackError::Truncated)?;
    if magic != PACK_MAGIC {
        return Err(PackError::BadMagic);
    }
    let version = read_u16(bytes, 8)?;
    if version != PACK_FORMAT_VERSION {
        return Err(PackError::UnsupportedVersion(version));
    }
    let flags = read_u16(bytes, 10)?;
    if flags != 0 {
        return Err(PackError::UnsupportedFlags(flags));
    }
    let length_field = bytes.get(12..PREAMBLE_BYTES).ok_or(PackError::Truncated)?;
    let length = usize::try_from(u32::from_le_bytes([
        length_field[0],
        length_field[1],
        length_field[2],
        length_field[3],
    ]))
    .map_err(|_| PackError::Truncated)?;
    if length > limits.max_header_bytes {
        return Err(PackError::HeaderTooLarge(length));
    }
    let end = PREAMBLE_BYTES
        .checked_add(length)
        .ok_or(PackError::Truncated)?;
    let json = bytes.get(PREAMBLE_BYTES..end).ok_or(PackError::Truncated)?;
    // Value first: the workspace's arbitrary-precision numbers do not decode
    // straight from text into the contract's untagged unions.
    let value: serde_json::Value = serde_json::from_slice(json).map_err(PackError::Header)?;
    let document: MaterializationDocumentA0 =
        serde_json::from_value(value).map_err(PackError::Header)?;
    validate_materialization_header(&document).map_err(PackError::Invalid)?;
    Ok(PackHeader {
        document,
        columns_offset: aligned(end),
    })
}
