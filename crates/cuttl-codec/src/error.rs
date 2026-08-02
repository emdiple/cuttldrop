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

    #[error(
        "pulse capacity of {capacity} B cannot hold a {header} B header, a {symbol_id} B symbol id and a symbol"
    )]
    NoRoomForSymbol {
        capacity: usize,
        header: usize,
        symbol_id: usize,
    },

    #[error("malformed fountain configuration in pulse header")]
    BadConfig,

    #[error("fountain has not converged: {have} symbols absorbed, at least {need} needed")]
    NotConverged { have: u32, need: u32 },

    #[error("no pulse passed the CRC gate")]
    Empty,

    #[error("object CRC mismatch after reassembly (expected {expected:#010x}, got {got:#010x})")]
    ObjectCrc { expected: u32, got: u32 },

    #[error("pulses disagree about the object: {field}")]
    Inconsistent { field: &'static str },
}

pub type Result<T> = core::result::Result<T, Error>;
