//! # cuttl-codec
//!
//! The single codec definition shared by the **skin** (sender) and **eye**
//! (receiver). Everything both sides must agree on lives here: pulse
//! geometry, palette, pilot layout, symbol framing, the FEC stack, and the
//! manifest format. See `DESIGN.md` §3–§4 for the rationale.
//!
//! Must compile for native targets (simulator, CLI) and `wasm32-unknown-unknown`
//! (browser). Keep browser-only and OS-only concerns out of this crate.
//!
//! ## Module plan (lands in M0)
//! - `geometry` — cell grid, finders, timing tracks, beacon strips (§3a)
//! - `palette`  — pluggable palette; B/W in M1, 8-colour RGB corners in M3 (§3b)
//! - `framing`  — beacon field, band layout, symbol packing (§3a, §3c)
//! - `fec`      — inner RS + CRC-32 gate + outer RaptorQ (§1b, §3c)
//! - `manifest` — filename/size/mime/BLAKE3, its own fountain stream (§3c)

#![forbid(unsafe_code)]
