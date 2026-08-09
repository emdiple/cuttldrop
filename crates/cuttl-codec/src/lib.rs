//! # cuttl-codec
//!
//! The single transport definition shared by the **skin** (sender) and **eye**
//! (receiver). Everything both sides must agree on lives here: packet framing,
//! the CRC gate, the RaptorQ fountain, and the manifest format.
//!
//! The optical carrier is deliberately *not* here. Packets become standard QR
//! symbols in the browser — written by `qrcode`, read by `zxing-wasm` — and
//! this crate neither knows nor cares whether a packet travelled as one
//! black-and-white symbol or as one channel of an RGB-multiplexed frame.
//!
//! Must compile for native targets (tests) and `wasm32-unknown-unknown`
//! (browser). Keep browser-only and OS-only concerns out of this crate.
//!
//! ## Status
//! - [`fountain`] — outer RaptorQ erasure code (§1a, §3c) — **landed**
//! - [`stream`] — packet framing + CRC gate over the fountain (§3c) — **landed**
//! - [`manifest`] — filename/mime/BLAKE3, interleaved into the stream (§3c) —
//!   **landed**; the hash is the mandatory object check (§3f)
//!
//! The layering is the part most worth keeping straight: QR's own ECC either
//! corrects a symbol or its decoder rejects it wholesale, the CRC gate turns
//! whatever survives into a clean accept-or-erase decision, and the fountain
//! repairs **erasures** — the one thing it can repair. Nothing unverified ever
//! reaches the decoder, and nothing unhashed is ever handed back.

#![forbid(unsafe_code)]

pub mod error;
pub mod fountain;
pub mod manifest;
pub mod stream;

pub use error::{Error, Result};
pub use manifest::Manifest;
pub use stream::{Ingest, Receiver, ReferenceEncoder, ReferenceProfile};
