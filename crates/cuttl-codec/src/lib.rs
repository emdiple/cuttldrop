//! # cuttl-codec
//!
//! The single codec definition shared by the **skin** (sender) and **eye**
//! (receiver). Everything both sides must agree on lives here: pulse
//! geometry, palette, pilot layout, symbol framing, the FEC stack, and the
//! manifest format. See `DESIGN.md` §3–§4 for the rationale.
//!
//! Must compile for native targets (simulator, CLI) and `wasm32-unknown-unknown`
//! (browser). Keep browser-only and OS-only concerns out of this crate — in
//! particular, no pixels: [`pulse::Pulse`] holds cell values, and turning those
//! into pixels belongs to the caller.
//!
//! ## Status
//! - [`geometry`] — cell grid, regions, payload ordering (§3a) — **landed**
//! - [`palette`] — Mono1 now; Color3 defined but unused until M3 (§3b) — **landed**
//! - [`pulse`] — cell buffer, structure painting, bit packing (§3a) — **landed**
//! - [`stream`] — framing + CRC gate; a chunked carousel, *not yet* a fountain (§3c)
//! - `fec` — inner RS + outer RaptorQ (§1b, §3c) — M0 step 2
//! - `beacon` — ECC-heavy header in the reserved strips (§3a) — M1
//! - `manifest` — filename/size/mime/BLAKE3 as its own stream (§3c) — M2

#![forbid(unsafe_code)]

pub mod error;
pub mod geometry;
pub mod palette;
pub mod pulse;
pub mod stream;

pub use error::{Error, Result};
pub use geometry::{Grid, Region};
pub use palette::Palette;
pub use pulse::Pulse;
pub use stream::{Ingest, Reassembler};
