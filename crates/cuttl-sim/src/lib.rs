//! # cuttl-sim
//!
//! Synthetic optical channel: renders pulses from `cuttl-codec`, distorts
//! them the way a real screen→camera path would, and feeds them back to the
//! decoder — thousands of frames per second, no browser, no camera.
//!
//! This is the M0 deliverable and the highest-leverage piece of the project
//! (`DESIGN.md` §5, M0): every optical bug from M1 onward should be asked
//! "does the simulator reproduce it?" before touching a camera.
//!
//! ## Distortion stages (lands in M0)
//! perspective warp → defocus/motion blur → sensor noise → colour crosstalk
//! (3×3 mixing + offset) → exposure blend of adjacent pulses → rolling-shutter
//! tear → frame drops.
//!
//! From M2 this is joined by the **capture corpus** (`DESIGN.md` §5, M2):
//! recorded real camera frames replayed through the decoder in CI. The
//! simulator tests what we thought of; the corpus tests what we didn't.

#![forbid(unsafe_code)]
