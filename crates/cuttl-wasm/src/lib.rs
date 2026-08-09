//! Browser bindings over `cuttl-codec`.
//!
//! Two objects, matching the vocabulary: a [`ReferenceSkin`] that turns a file
//! into QR packets to paint, and a [`ReferenceEye`] that turns decoded QR
//! payloads back into the file.
//!
//! ## What is deliberately *not* here
//!
//! Anything the browser already does better. No canvas, no `getUserMedia`, no
//! QR rasterization, no ZXing, no pacing, no UI — that is all TypeScript. This
//! crate is the transport boundary and nothing else, which keeps the surface
//! small enough to be obviously correct: packets out, packets in, and the
//! fountain/manifest/BLAKE3 state machine between them.

use cuttl_codec::{Ingest, Receiver, stream};
use wasm_bindgen::prelude::*;

/// What happened to one captured frame.
///
/// Every variant except `Completed` is routine — a looping skin produces far
/// more frames than the transfer needs. Only a *rising* rate of `Unlocatable`
/// means the human should do something.
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
    /// No QR symbol decoded from this camera frame. Usually framing: move
    /// closer, or hold still.
    Unlocatable,
}

fn reference_profile_of(name: &str) -> Result<stream::ReferenceProfile, String> {
    stream::ReferenceProfile::parse(name)
        .ok_or_else(|| format!("unknown QR reference profile {name:?}"))
}

fn outcome(ingest: Ingest) -> Outcome {
    match ingest {
        Ingest::Accepted => Outcome::Accepted,
        Ingest::Completed => Outcome::Completed,
        Ingest::Duplicate => Outcome::Duplicate,
        Ingest::Rejected => Outcome::Rejected,
    }
}

/// The skin-side packet producer.
///
/// It intentionally has no raster methods. The browser turns each packet into
/// a standards-compliant QR matrix — one per black-and-white frame, or three
/// per RGB-multiplexed frame; every transport detail above that matrix
/// remains Cuttldrop's own RaptorQ/manifest/BLAKE3 stream.
#[wasm_bindgen]
pub struct ReferenceSkin {
    stream: stream::ReferenceEncoder,
    profile: stream::ReferenceProfile,
}

#[wasm_bindgen]
impl ReferenceSkin {
    #[wasm_bindgen(constructor)]
    pub fn new(
        object: &[u8],
        name: &str,
        mime: &str,
        profile: &str,
        stream_id: u32,
        overhead: f32,
    ) -> Result<ReferenceSkin, JsValue> {
        Self::create(object, name, mime, profile, stream_id, overhead)
            .map_err(|error| JsValue::from_str(&error))
    }

    #[wasm_bindgen(getter)]
    pub fn profile(&self) -> String {
        self.profile.name().to_string()
    }

    /// The JsValue-free constructor body, so native tests can exercise the
    /// failure path — JsValue cannot even be *created* off wasm32.
    fn create(
        object: &[u8],
        name: &str,
        mime: &str,
        profile: &str,
        stream_id: u32,
        overhead: f32,
    ) -> Result<ReferenceSkin, String> {
        let profile = reference_profile_of(profile)?;
        stream::ReferenceEncoder::new(object, name, mime, profile, stream_id, overhead)
            .map(|stream| Self { stream, profile })
            .map_err(|error| error.to_string())
    }

    #[wasm_bindgen(getter, js_name = qrVersion)]
    pub fn qr_version(&self) -> u8 {
        self.profile.version()
    }

    #[wasm_bindgen(getter, js_name = packetCount)]
    pub fn packet_count(&self) -> usize {
        self.stream.packet_count()
    }

    #[wasm_bindgen(js_name = packet)]
    pub fn packet(&self, index: usize) -> Vec<u8> {
        self.stream.packet(index)
    }
}

/// The eye-side packet sink.
#[wasm_bindgen]
pub struct ReferenceEye {
    receiver: Receiver,
    unlocatable: u32,
}

impl Default for ReferenceEye {
    fn default() -> Self {
        Self {
            receiver: Receiver::new(),
            unlocatable: 0,
        }
    }
}

#[wasm_bindgen]
impl ReferenceEye {
    #[wasm_bindgen(constructor)]
    pub fn new() -> ReferenceEye {
        Self::default()
    }

    /// Feed bytes returned by a standards-compliant QR decoder.
    pub fn ingest(&mut self, packet: &[u8]) -> Outcome {
        outcome(self.receiver.ingest_packet(packet))
    }

    /// QR detection found no valid symbol in this camera frame.
    pub fn miss(&mut self) {
        self.unlocatable += 1;
    }

    /// Symbols absorbed so far. Honest and monotonic — not a guessed percentage.
    #[wasm_bindgen(getter)]
    pub fn symbols(&self) -> u32 {
        self.receiver.progress().0
    }

    /// Symbols needed at minimum. Zero until the first packet is understood.
    #[wasm_bindgen(getter)]
    pub fn needed(&self) -> u32 {
        self.receiver.progress().1
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
    /// or `undefined` before the first manifest packet. Arrives within
    /// `MANIFEST_PERIOD` packets of looking, usually long before the file.
    #[wasm_bindgen(getter, js_name = fileName)]
    pub fn file_name(&self) -> Option<String> {
        self.receiver
            .manifest()
            .map(|manifest| manifest.safe_name())
    }

    /// Mime type from the manifest; empty if the sender did not know,
    /// `undefined` before the first manifest packet.
    #[wasm_bindgen(getter, js_name = fileMime)]
    pub fn file_mime(&self) -> Option<String> {
        self.receiver
            .manifest()
            .map(|manifest| manifest.mime.clone())
    }

    /// Exact incoming file size in bytes, or `undefined` until the first
    /// packet is understood. An `f64` because JS numbers are doubles; RFC 6330
    /// caps transfers far below 2^53, so the value is always exact.
    #[wasm_bindgen(getter, js_name = expectedBytes)]
    pub fn expected_bytes(&self) -> Option<f64> {
        self.receiver.expected_len().map(|len| len as f64)
    }

    /// Object bytes each accepted symbol is worth, or `undefined` until the
    /// first packet is understood.
    ///
    /// The eye's goodput readout is `symbols × symbolBytes ÷ elapsed`. Without
    /// this the page would have to divide `expectedBytes` by `needed` and hope
    /// the padding rounded its way; RaptorQ's symbol size is a fact, so it is
    /// reported as one.
    #[wasm_bindgen(getter, js_name = symbolBytes)]
    pub fn symbol_bytes(&self) -> Option<u32> {
        self.receiver.symbol_len().map(|len| len as u32)
    }

    #[wasm_bindgen(getter, js_name = isComplete)]
    pub fn is_complete(&self) -> bool {
        self.receiver.is_complete()
    }

    /// The reconstructed file, or `undefined` if it is not ready.
    ///
    /// Verified against the manifest's BLAKE3 hash before it is handed back —
    /// an unverified file is never returned.
    #[wasm_bindgen(js_name = takeObject)]
    pub fn take_object(&self) -> Option<Vec<u8>> {
        self.receiver.finish().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shim must not reshape anything: a file through `ReferenceSkin` and
    /// straight back through `ReferenceEye` is the same file, at every rung.
    /// Runs natively — no browser needed.
    #[test]
    fn qr_reference_skin_and_eye_share_the_verified_stream() {
        let object: Vec<u8> = (0..11_000u32).map(|n| (n * 13) as u8).collect();
        for profile in stream::ReferenceProfile::ALL {
            let skin = ReferenceSkin::new(
                &object,
                "reference.bin",
                "application/test",
                profile.name(),
                41,
                0.5,
            )
            .unwrap();
            let mut eye = ReferenceEye::new();
            for index in 0..skin.packet_count() {
                let out = eye.ingest(&skin.packet(index));
                if index == 0 {
                    // Packet 0 carries the manifest: the eye knows what it is
                    // receiving before it has received anything.
                    assert_eq!(eye.file_name().as_deref(), Some("reference.bin"));
                    assert_eq!(eye.expected_bytes(), Some(object.len() as f64));
                    assert!(!eye.is_complete());
                }
                if out == Outcome::Completed {
                    break;
                }
            }
            assert_eq!(skin.profile(), profile.name());
            assert_eq!(skin.qr_version(), profile.version());
            assert!(eye.is_complete());
            assert_eq!(eye.file_mime().as_deref(), Some("application/test"));
            assert_eq!(eye.take_object().unwrap(), object);
        }
    }

    #[test]
    fn unknown_profiles_are_rejected() {
        assert!(ReferenceSkin::create(&[1, 2, 3], "f", "", "m1", 1, 0.0).is_err());
        assert!(ReferenceSkin::create(&[1, 2, 3], "f", "", "qr41", 1, 0.0).is_err());
    }

    /// Indexing wraps, because the skin loops forever.
    #[test]
    fn packet_index_wraps() {
        let skin = ReferenceSkin::new(&[1u8; 300], "f", "", "qr27", 1, 0.0).unwrap();
        let count = skin.packet_count();
        assert_eq!(skin.packet(0), skin.packet(count));
    }

    /// Rejected packets are counted, and garbage never breaks the stream.
    #[test]
    fn garbage_is_rejected_not_fatal() {
        let object = vec![9u8; 5000];
        let skin = ReferenceSkin::new(&object, "f", "", "qr27", 3, 0.5).unwrap();
        let mut eye = ReferenceEye::new();
        assert_eq!(eye.ingest(&[0u8; 40]), Outcome::Rejected);
        for index in 0..skin.packet_count() {
            if eye.ingest(&skin.packet(index)) == Outcome::Completed {
                break;
            }
        }
        assert_eq!(eye.rejected(), 0, "counters reset when the stream starts");
        assert_eq!(eye.take_object().unwrap(), object);
    }
}
