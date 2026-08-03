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

use cuttl_codec::{Grid, Ingest, Profile, Pulse, Raster, Receiver, eye, stream};
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
    /// `name` and `mime` ride in the manifest so the far end can display and
    /// save the file as itself; pass what the `File` object says. `overhead` is
    /// repair symbols per source symbol. The skin loops forever, so this only
    /// bounds how long the loop is before it repeats — but a longer loop means
    /// a receiver that missed a frame waits less time for a *different* one
    /// rather than the same one again.
    #[wasm_bindgen(constructor)]
    pub fn new(
        object: &[u8],
        name: &str,
        mime: &str,
        profile: &str,
        stream_id: u32,
        overhead: f32,
    ) -> Result<Skin, JsValue> {
        Self::create(object, name, mime, profile, stream_id, overhead)
            .map_err(|e| JsValue::from_str(&e))
    }

    fn create(
        object: &[u8],
        name: &str,
        mime: &str,
        profile: &str,
        stream_id: u32,
        overhead: f32,
    ) -> Result<Skin, String> {
        let (grid, palette) = profile_of(profile)?.parts();
        let pulses = stream::encode_named(object, name, mime, grid, palette, stream_id, overhead)
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
    /// The profile in force, or `None` while still being worked out.
    locked: Option<Profile>,
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
        // `"auto"` means no profile is fixed yet: `locked` stays `None` and the
        // first understood frame decides. Anything else pins one immediately.
        let locked = match profile {
            "auto" => None,
            name => Some(profile_of(name)?),
        };
        Ok(Self {
            locked,
            receiver: Receiver::new(),
            unlocatable: 0,
        })
    }

    /// Feed one captured frame, RGBA, as `ImageData.data` provides it.
    ///
    /// With no profile locked, every known grid is tried in turn and the first
    /// one that yields a pulse the receiver *accepts* is locked in for the rest
    /// of the stream. This is deliberately trial decoding rather than a field
    /// in the header: the header lives in cells, and cells cannot be sampled
    /// until the grid is already known. Reading the grid's dimensions off the
    /// finder geometry would break that circularity properly and is the better
    /// answer eventually; trying four grids costs a few milliseconds once and
    /// spares the human from setting a menu identically on two devices.
    pub fn ingest(&mut self, rgba: &[u8], width: u32, height: u32) -> Outcome {
        let Ok(raster) = Raster::new_rgba(width, height, rgba) else {
            self.unlocatable += 1;
            return Outcome::Unlocatable;
        };

        if self.locked.is_none() {
            match self.detect(&raster) {
                Some(outcome) => return outcome,
                None => {
                    self.unlocatable += 1;
                    return Outcome::Unlocatable;
                }
            }
        }

        let (grid, palette) = self.locked.unwrap_or_default().parts();
        let Ok(pulse) = eye::read(&raster, grid, palette) else {
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

    /// Try every profile against one frame; lock the first that *accepts*.
    ///
    /// Acceptance, not merely "read without error", is the test. A dense grid
    /// sampled at the wrong pitch still finds four finders often enough to
    /// produce cells; what it cannot do is produce cells whose CRC passes. The
    /// gate that already exists to keep corrupt symbols out of the fountain is
    /// exactly the right oracle here, so nothing new has to be trusted.
    ///
    /// Returns `None` if no profile got anywhere, leaving the eye unlocked to
    /// try again on the next frame.
    fn detect(&mut self, raster: &Raster<'_>) -> Option<Outcome> {
        for profile in Profile::ALL {
            let (grid, palette) = profile.parts();
            let Ok(pulse) = eye::read(raster, grid, palette) else {
                continue;
            };
            let ingest = self.receiver.ingest(&pulse);
            if matches!(ingest, Ingest::Accepted | Ingest::Completed) {
                self.locked = Some(profile);
                return Some(match ingest {
                    Ingest::Completed => Outcome::Completed,
                    _ => Outcome::Accepted,
                });
            }
        }
        None
    }

    /// The profile in force, or `undefined` while the eye is still searching.
    #[wasm_bindgen(getter)]
    pub fn profile(&self) -> Option<String> {
        self.locked.map(|p| p.name().to_string())
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

    /// Filename from the manifest, already sanitised for a download attribute —
    /// or `undefined` before the first manifest pulse. Arrives within
    /// `MANIFEST_PERIOD` pulses of looking, usually long before the file (§3c).
    #[wasm_bindgen(getter, js_name = fileName)]
    pub fn file_name(&self) -> Option<String> {
        self.receiver.manifest().map(|m| m.safe_name())
    }

    /// Mime type from the manifest; empty if the sender did not know,
    /// `undefined` before the first manifest pulse.
    #[wasm_bindgen(getter, js_name = fileMime)]
    pub fn file_mime(&self) -> Option<String> {
        self.receiver.manifest().map(|m| m.mime.clone())
    }

    /// Exact incoming file size in bytes, or `undefined` until the first pulse
    /// is understood. An `f64` because JS numbers are doubles; RFC 6330 caps
    /// transfers far below 2^53, so the value is always exact.
    #[wasm_bindgen(getter, js_name = expectedBytes)]
    pub fn expected_bytes(&self) -> Option<f64> {
        self.receiver.expected_len().map(|n| n as f64)
    }

    /// Object bytes each accepted symbol is worth, or `undefined` until the
    /// first pulse is understood.
    ///
    /// The eye's goodput readout is `symbols × symbolBytes ÷ elapsed`. Without
    /// this the page would have to divide `expectedBytes` by `needed` and hope
    /// the padding rounded its way; RaptorQ's symbol size is a fact, so it is
    /// reported as one.
    #[wasm_bindgen(getter, js_name = symbolBytes)]
    pub fn symbol_bytes(&self) -> Option<u32> {
        self.receiver.symbol_len().map(|n| n as u32)
    }

    #[wasm_bindgen(getter, js_name = isComplete)]
    pub fn is_complete(&self) -> bool {
        self.receiver.is_complete()
    }

    /// The reconstructed file, or `undefined` if it is not ready.
    ///
    /// Verified against the manifest's BLAKE3 hash before it is handed back —
    /// an unverified file is never returned (§3f).
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
        let skin =
            Skin::create(&object, "ink.bin", "application/octet-stream", "m1", 5, 0.5).unwrap();
        let mut eye = Eye::create("m1").unwrap();

        let (w, h) = (skin.cols(), skin.rows());
        for index in 0..skin.pulse_count() {
            let rgba = skin.pulse_rgba(index);
            let outcome = eye.ingest(&rgba, w, h);
            if index == 0 {
                // Pulse 0 carries the manifest: the eye knows what it is
                // receiving before it has received anything.
                assert_eq!(eye.file_name().as_deref(), Some("ink.bin"));
                assert_eq!(eye.expected_bytes(), Some(object.len() as f64));
                assert!(!eye.is_complete());
            }
            if outcome == Outcome::Completed {
                break;
            }
        }
        assert!(eye.is_complete());
        assert_eq!(eye.take_object().unwrap(), object);
        assert_eq!(eye.file_mime().as_deref(), Some("application/octet-stream"));
    }

    /// The eye works out the profile for itself, for every profile there is.
    ///
    /// This is what keeps a density change from being a two-device chore: the
    /// skin picks, the eye follows. The assertion that it locks onto the *same*
    /// profile matters more than that it completes — a dense grid misread as a
    /// sparse one could in principle limp along, and it must not.
    #[test]
    fn the_eye_locks_onto_whichever_profile_the_skin_chose() {
        let object: Vec<u8> = (0..40000u32).map(|i| (i * 91 + i / 7) as u8).collect();
        for profile in Profile::ALL {
            let name = profile.name();
            let skin = Skin::create(&object, "auto.bin", "", name, 5, 0.5).unwrap();
            let mut eye = Eye::create("auto").unwrap();
            assert_eq!(eye.profile(), None, "{name}: locked before seeing anything");

            let (w, h) = (skin.cols(), skin.rows());
            for index in 0..skin.pulse_count() {
                if eye.ingest(&skin.pulse_rgba(index), w, h) == Outcome::Completed {
                    break;
                }
            }
            assert_eq!(eye.profile().as_deref(), Some(name), "{name}: locked wrong");
            assert_eq!(eye.take_object().unwrap(), object, "{name}: bad object");
        }
    }

    #[test]
    fn unknown_profiles_are_rejected() {
        assert!(Skin::create(&[1, 2, 3], "f", "", "m9", 1, 0.0).is_err());
        assert!(Eye::create("").is_err());
    }

    #[test]
    fn pulse_rgba_is_grid_sized_and_opaque() {
        let skin = Skin::create(&[7u8; 500], "f", "", "m1", 1, 0.0).unwrap();
        let rgba = skin.pulse_rgba(0);
        assert_eq!(rgba.len() as u32, skin.cols() * skin.rows() * 4);
        assert!(rgba.chunks_exact(4).all(|px| px[3] == 255));
    }

    /// Indexing wraps, because the skin loops forever.
    #[test]
    fn pulse_index_wraps() {
        let skin = Skin::create(&[1u8; 300], "f", "", "m1", 1, 0.0).unwrap();
        let count = skin.pulse_count();
        assert_eq!(skin.pulse_rgba(0), skin.pulse_rgba(count));
    }
}
