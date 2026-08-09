use thiserror::Error;

/// Codec errors.
///
/// Note the split: *structural* problems are `Error`s, but a packet that simply
/// fails its CRC is **not** an error — it is an erasure, and the reassembler
/// reports it as [`crate::stream::Ingest::Rejected`]. Errors converted to
/// erasures is the whole point of the CRC gate (`DESIGN.md` §1b).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("payload of {len} B exceeds packet capacity of {capacity} B")]
    PayloadTooLarge { len: usize, capacity: usize },

    #[error("malformed fountain configuration in packet header")]
    BadConfig,

    #[error("fountain has not converged: {have} symbols absorbed, at least {need} needed")]
    NotConverged { have: u32, need: u32 },

    #[error("no packet passed the CRC gate")]
    Empty,

    #[error(
        "object reconstructed but no manifest packet has arrived yet, so it cannot be verified"
    )]
    NoManifest,

    #[error("BLAKE3 mismatch after reassembly — the reconstruction is not the file that was sent")]
    ObjectHash,

    #[error("compressed object could not be restored: {0}")]
    Compression(String),
}

pub type Result<T> = core::result::Result<T, Error>;
