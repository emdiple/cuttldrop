//! Browser bindings over `cuttl-codec` (`DESIGN.md` §4).
//!
//! Two objects, matching the vocabulary: a [`Skin`] that turns a file into
//! pulses to paint, and an [`Eye`] that turns camera frames back into the file.
//!
//! ## What is deliberately *not* here
//!
//! Anything the browser already does better. No canvas, no `getUserMedia`, no
//! pacing, no UI — that is all TypeScript. This crate is the codec boundary and
//! nothing else, which keeps the surface small enough to be obviously correct.
//!
//! Also absent: the optical channel. Warp, blur and tear are simulation, and
//! simulation belongs in `cuttl-sim` where a real camera would only get in the
//! way. The browser has an actual camera.
//!
//! ## Zero-copy on the hot path
//!
//! [`Eye::ingest`] takes RGBA exactly as `ImageData.data` provides it and
//! borrows it as a `Raster`. No conversion pass over a multi-megabyte frame,
//! and no allocation per capture beyond what the decoder itself needs.

use cuttl_codec::{Grid, Ingest, Palette, Profile, Pulse, Raster, Receiver, eye, stream};
use wasm_bindgen::prelude::*;

/// What happened to one captured frame.
///
/// Every variant except `Completed` is routine — a looping skin produces far
/// more frames than the transfer needs. Only a *rising* rate of `Torn` or
/// `Unlocatable` means the human should do something (§1e).
#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Absorbed; more symbols still needed.
    Accepted,
    /// Absorbed, and the file is now complete.
    Completed,
    /// A symbol already held, or a frame arriving after completion.
    Duplicate,
    /// Failed the CRC gate, or belongs to another transfer.
    Rejected,
    /// The beacon strips disagreed — rolling-shutter tear. Hold steadier, or
    /// slow the pulse rate.
    Torn,
    /// No four finders found. Usually framing: move closer, or hold still.
    Unlocatable,
}

fn profile_of(name: &str) -> Result<Profile, String> {
    Profile::parse(name).ok_or_else(|| format!("unknown profile {name:?}"))
}

/// The sending side: holds a file's worth of pulses, ready to paint.
#[wasm_bindgen]
pub struct Skin {
    pulses: Vec<Pulse>,
    grid: Grid,
}

#[wasm_bindgen]
impl Skin {
    /// Encode a file into a looping pulse sequence.
    ///
    /// `overhead` is repair symbols per source symbol. The skin loops forever,
    /// so this only bounds how long the loop is before it repeats — but a
    /// longer loop means a receiver that missed a frame waits less time for a
    /// *different* one rather than the same one again.
    #[wasm_bindgen(constructor)]
    pub fn new(
        object: &[u8],
        profile: &str,
        stream_id: u32,
        overhead: f32,
    ) -> Result<Skin, JsValue> {
        Self::create(object, profile, stream_id, overhead).map_err(|e| JsValue::from_str(&e))
    }

    fn create(object: &[u8], profile: &str, stream_id: u32, overhead: f32) -> Result<Skin, String> {
        let (grid, palette) = profile_of(profile)?.parts();
        let pulses = stream::encode(object, grid, palette, stream_id, overhead)
            .map_err(|e| e.to_string())?;
        Ok(Self { pulses, grid })
    }

    #[wasm_bindgen(getter)]
    pub fn cols(&self) -> u32 {
        self.grid.cols as u32
    }

    #[wasm_bindgen(getter)]
    pub fn rows(&self) -> u32 {
        self.grid.rows as u32
    }

    #[wasm_bindgen(getter, js_name = pulseCount)]
    pub fn pulse_count(&self) -> usize {
        self.pulses.len()
    }

    /// One pulse as RGBA, at *grid* resolution — `cols × rows`, not screen size.
    ///
    /// The caller blits this into an `ImageData` and scales it up with
    /// smoothing disabled. Upscaling is the browser's job and it does it on the
    /// GPU; sending full-screen pixels across the WASM boundary instead would
    /// be thousands of times more data for an identical picture.
    #[wasm_bindgen(js_name = pulseRgba)]
    pub fn pulse_rgba(&self, index: usize) -> Vec<u8> {
        let mut out = vec![255u8; (self.grid.cols as usize) * (self.grid.rows as usize) * 4];
        if self.pulses.is_empty() {
            return out;
        }
        // Wraps, because the skin loops forever.
        let pulse = &self.pulses[index % self.pulses.len()];
        for y in 0..self.grid.rows {
            for x in 0..self.grid.cols {
                let rgb = pulse.rgb(x, y).expect("cell within grid bounds");
                let i = (y as usize * self.grid.cols as usize + x as usize) * 4;
                out[i..i + 3].copy_from_slice(&rgb);
            }
        }
        out
    }
}

/// The receiving side: absorbs camera frames until the file falls out.
#[wasm_bindgen]
pub struct Eye {
    grid: Grid,
    palette: Palette,
    receiver: Receiver,
    unlocatable: u32,
}

#[wasm_bindgen]
impl Eye {
    #[wasm_bindgen(constructor)]
    pub fn new(profile: &str) -> Result<Eye, JsValue> {
        Self::create(profile).map_err(|e| JsValue::from_str(&e))
    }

    fn create(profile: &str) -> Result<Eye, String> {
        let (grid, palette) = profile_of(profile)?.parts();
        Ok(Self {
            grid,
            palette,
            receiver: Receiver::new(),
            unlocatable: 0,
        })
    }

    /// Feed one captured frame, RGBA, as `ImageData.data` provides it.
    pub fn ingest(&mut self, rgba: &[u8], width: u32, height: u32) -> Outcome {
        let Ok(raster) = Raster::new_rgba(width, height, rgba) else {
            self.unlocatable += 1;
            return Outcome::Unlocatable;
        };
        let Ok(pulse) = eye::read(&raster, self.grid, self.palette) else {
            self.unlocatable += 1;
            return Outcome::Unlocatable;
        };
        match self.receiver.ingest(&pulse) {
            Ingest::Accepted => Outcome::Accepted,
            Ingest::Completed => Outcome::Completed,
            Ingest::Duplicate => Outcome::Duplicate,
            Ingest::Rejected => Outcome::Rejected,
            Ingest::Torn => Outcome::Torn,
        }
    }

    /// Symbols absorbed so far. Honest and monotonic — not a guessed percentage.
    #[wasm_bindgen(getter)]
    pub fn symbols(&self) -> u32 {
        self.receiver.progress().0
    }

    /// Symbols needed at minimum. Zero until the first frame is understood.
    #[wasm_bindgen(getter)]
    pub fn needed(&self) -> u32 {
        self.receiver.progress().1
    }

    #[wasm_bindgen(getter)]
    pub fn torn(&self) -> u32 {
        self.receiver.torn()
    }

    #[wasm_bindgen(getter)]
    pub fn rejected(&self) -> u32 {
        self.receiver.rejected()
    }

    #[wasm_bindgen(getter)]
    pub fn unlocatable(&self) -> u32 {
        self.unlocatable
    }

    #[wasm_bindgen(getter, js_name = isComplete)]
    pub fn is_complete(&self) -> bool {
        self.receiver.is_complete()
    }

    /// The reconstructed file, or `undefined` if it is not ready.
    ///
    /// Verified against the object CRC before it is handed back — an unverified
    /// file is never returned (§3f).
    #[wasm_bindgen(js_name = takeObject)]
    pub fn take_object(&self) -> Option<Vec<u8>> {
        self.receiver.finish().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shim must not reshape anything: a file through `Skin` and straight
    /// back through `Eye` is the same file. Runs natively — no browser needed.
    #[test]
    fn skin_to_eye_roundtrips_without_a_browser() {
        let object: Vec<u8> = (0..9000u32).map(|i| (i * 37) as u8).collect();
        let skin = Skin::create(&object, "m1", 5, 0.5).unwrap();
        let mut eye = Eye::create("m1").unwrap();

        let (w, h) = (skin.cols(), skin.rows());
        for index in 0..skin.pulse_count() {
            let rgba = skin.pulse_rgba(index);
            if eye.ingest(&rgba, w, h) == Outcome::Completed {
                break;
            }
        }
        assert!(eye.is_complete());
        assert_eq!(eye.take_object().unwrap(), object);
    }

    #[test]
    fn unknown_profiles_are_rejected() {
        assert!(Skin::create(&[1, 2, 3], "m9", 1, 0.0).is_err());
        assert!(Eye::create("").is_err());
    }

    #[test]
    fn pulse_rgba_is_grid_sized_and_opaque() {
        let skin = Skin::create(&[7u8; 500], "m1", 1, 0.0).unwrap();
        let rgba = skin.pulse_rgba(0);
        assert_eq!(rgba.len() as u32, skin.cols() * skin.rows() * 4);
        assert!(rgba.chunks_exact(4).all(|px| px[3] == 255));
    }

    /// Indexing wraps, because the skin loops forever.
    #[test]
    fn pulse_index_wraps() {
        let skin = Skin::create(&[1u8; 300], "m1", 1, 0.0).unwrap();
        let count = skin.pulse_count();
        assert_eq!(skin.pulse_rgba(0), skin.pulse_rgba(count));
    }
}
