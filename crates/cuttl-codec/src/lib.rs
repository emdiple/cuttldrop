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
//! - [`fountain`] — outer RaptorQ erasure code (§1a, §3c) — **landed**
//! - [`stream`] — framing + CRC gate over the fountain (§3c) — **landed**
//! - [`fec`] — *inner* Reed–Solomon, below the CRC gate (§1b) — **landed**
//! - [`beacon`] — repetition-coded header in the reserved strips (§3a) — **landed**
//! - [`geom`] / [`eye`] — homography, finder detection, cell sampling (§3a) —
//!   **landed**, and deliberately here rather than in the simulator so the
//!   browser runs the same code (§4)
//! - [`manifest`] — filename/mime/BLAKE3, interleaved into the stream (§3c) —
//!   **landed**; the hash is the mandatory object check (§3f)
//!
//! The two FEC layers are not interchangeable and the distinction is the one
//! most worth keeping straight: the fountain repairs **erasures** (whole lost
//! or rejected pulses), the inner code repairs **errors** (a few misread cells
//! within an otherwise good pulse), and the CRC gate between them turns the
//! second kind into the first.

#![forbid(unsafe_code)]

pub mod beacon;
pub mod error;
pub mod eye;
pub mod fec;
pub mod fountain;
pub mod geom;
pub mod geometry;
pub mod manifest;
pub mod palette;
pub mod pulse;
pub mod raster;
pub mod stream;

pub use beacon::Beacon;
pub use error::{Error, Result};
pub use geom::Homography;
pub use geometry::{Grid, Profile, Region, Strip};
pub use manifest::Manifest;
pub use palette::Palette;
pub use pulse::Pulse;
pub use raster::Raster;
pub use stream::{Ingest, Receiver};
