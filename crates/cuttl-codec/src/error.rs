use thiserror::Error;

/// Codec errors.
///
/// Note the split: *structural* problems are `Error`s, but a pulse that simply
/// fails its CRC is **not** an error — it is an erasure, and the reassembler
/// reports it as [`crate::stream::Ingest::Rejected`]. Errors converted to
/// erasures is the whole point of the CRC gate (`DESIGN.md` §1b).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("payload of {len} B exceeds pulse capacity of {capacity} B")]
    PayloadTooLarge { len: usize, capacity: usize },

    #[error("grid {cols}×{rows} leaves no payload cells after structural regions")]
    GridTooSmall { cols: u16, rows: u16 },

    #[error("pilot period must be non-zero")]
    ZeroPilotPeriod,

    #[error("cell ({x}, {y}) is outside the {cols}×{rows} grid")]
    CellOutOfBounds {
        x: u16,
        y: u16,
        cols: u16,
        rows: u16,
    },

    #[error("cell value {value} out of range for a {levels}-level palette")]
    CellValue { value: u8, levels: u8 },

    #[error("image {w}×{h} does not match a {cols}×{rows} grid at any integer cell size")]
    Dimensions {
        w: u32,
        h: u32,
        cols: u16,
        rows: u16,
    },

    #[error("pulse capacity of {capacity} B cannot hold the {header} B header")]
    NoRoomForHeader { capacity: usize, header: usize },

    #[error("stream incomplete: {missing} of {total} pulses still missing")]
    Incomplete { missing: u32, total: u32 },

    #[error("no pulses ingested")]
    Empty,

    #[error("object CRC mismatch after reassembly (expected {expected:#010x}, got {got:#010x})")]
    ObjectCrc { expected: u32, got: u32 },

    #[error("pulses disagree about the object: {field}")]
    Inconsistent { field: &'static str },
}

pub type Result<T> = core::result::Result<T, Error>;
