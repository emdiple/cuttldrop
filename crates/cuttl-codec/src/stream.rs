//! Object → packets → object, over the fountain layer.
//!
//! A packet carries one fountain symbol. There is no packet index and no
//! total: a rateless stream has neither. The eye absorbs symbols in any
//! order, from any subset, and stops when the object falls out (§3c).
//!
//! ## Layout
//!
//! ```text
//! packet:  MAGIC ver flags stream_id config hash │ symbol or manifest │ crc32
//! ```
//!
//! Every packet repeats the stream header — packets travel as standard QR
//! symbols, any one of which can be the first a late-arriving eye decodes.
//!
//! ## The manifest
//!
//! Through the head of the loop, every [`MANIFEST_PERIOD`]-th packet donates
//! its symbol slot to the [`Manifest`] — name, mime, and the BLAKE3 hash of
//! the object — flagged in the header and protected by the same CRC as any
//! symbol; past the head the cadence relaxes to every [`MANIFEST_STEADY`]-th.
//! The eye can say *"receiving cuttlefish.pdf — 2.4 MB"* within a second of
//! looking, and nothing is ever handed back until the reconstruction matches
//! the manifest's hash (§3c, §3f). The header repeats the hash's first four
//! bytes in every packet, binding symbols and manifest to one another.
//!
//! ## The CRC gate
//!
//! The most important check here is the per-packet CRC. A fountain decoder
//! assumes every symbol it receives is correct or absent; one silently corrupt
//! symbol propagates through the XOR graph and poisons the whole object. The
//! gate turns an *error* into an *erasure*, the one thing the fountain layer
//! can repair (§1b). A rejected packet is never an `Err` — it is routine.
//!
//! ## Order of operations
//!
//! ```text
//! skin:  header ‖ symbol  →  QR writer  →  screen
//! eye:   camera  →  QR decode  →  CRC gate  →  fountain  →  restore  →  BLAKE3
//! ```
//!
//! QR's own ECC corrects a symbol or its decoder rejects it wholesale, the
//! CRC gate converts whatever survived into an erasure, and only then does
//! anything reach the fountain — which repairs erasures and nothing else. The
//! BLAKE3 check at the very end is the only statement about the *file*;
//! everything before it is about bytes.

use crate::error::{Error, Result};
use crate::fountain::{CONFIG_LEN, Fountain, RaptorQ, RaptorQSink, SYMBOL_ID_LEN, Sink};
use crate::manifest::{Compression as ObjectCompression, Manifest};
use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use std::borrow::Cow;
use std::collections::HashSet;
use std::io::{Read, Write};

const MAGIC: [u8; 2] = *b"CD";
const VERSION: u8 = 4;

/// The packet's payload is the manifest, not a fountain symbol.
const FLAG_MANIFEST: u8 = 1;

/// Dense manifest period through the head of the loop: packets 0, 8, 16 and
/// 24 donate their symbol slots to the manifest. An eye watching from the
/// start — the common case for a human-coordinated transfer — learns the
/// filename within the first second, and a small object never leaves the
/// burst, keeping its manifests exactly this dense throughout (§3c).
pub const MANIFEST_PERIOD: usize = 8;

/// Steady manifest period past the head burst. A late joiner mid-loop waits
/// at most this many frames to learn what it is receiving, while a long
/// stream spends ~4% of its slots on the manifest instead of 12.5% — the
/// difference goes straight into goodput.
pub const MANIFEST_STEADY: usize = 24;

/// Last index of the dense head burst.
const MANIFEST_BURST_END: usize = 24;

// `manifest_slots_before` counts the two schedules separately and needs them
// to nest evenly.
const _: () = assert!(MANIFEST_BURST_END.is_multiple_of(MANIFEST_PERIOD));
const _: () = assert!(MANIFEST_BURST_END.is_multiple_of(MANIFEST_STEADY));
const _: () = assert!(MANIFEST_STEADY.is_multiple_of(MANIFEST_PERIOD));

/// Does packet `index` donate its symbol slot to the manifest?
const fn is_manifest_slot(index: usize) -> bool {
    index.is_multiple_of(MANIFEST_PERIOD)
        && (index <= MANIFEST_BURST_END || index.is_multiple_of(MANIFEST_STEADY))
}

/// Manifest slots among indices `0..index` — the offset between a packet
/// index and the fountain symbol ordinal it carries.
const fn manifest_slots_before(index: usize) -> usize {
    let burst_total = MANIFEST_BURST_END / MANIFEST_PERIOD + 1;
    let dense = index.div_ceil(MANIFEST_PERIOD);
    let dense = if dense > burst_total {
        burst_total
    } else {
        dense
    };
    // Multiples of MANIFEST_STEADY inside the burst are already counted above.
    let in_burst = MANIFEST_BURST_END / MANIFEST_STEADY + 1;
    let steady = index.div_ceil(MANIFEST_STEADY).saturating_sub(in_burst);
    dense + steady
}

/// Standard-QR density rungs for the transport.
///
/// The number is the RaptorQ payload, not the complete QR byte payload. Every
/// packet also carries a 24 B stream header, a 4 B RaptorQ id and a 4 B CRC.
/// Each value is therefore the largest eight-byte-aligned symbol that fits the
/// named QR version at L-level ECC when encoded in byte mode with mask 4:
///
/// ```text
/// QR v27-L: 1465 QR bytes - 32 B framing -> 1432 B symbol
/// QR v35-L: 2303 QR bytes - 32 B framing -> 2264 B symbol
/// QR v40-L: 2953 QR bytes - 32 B framing -> 2920 B symbol
/// ```
///
/// Alignment is deliberately applied here rather than relying on RaptorQ to
/// round down invisibly. That keeps the QR writer's fixed dimensions a hard
/// invariant: no packet can make it silently select a larger matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceProfile {
    V27,
    V35,
    V40,
}

impl ReferenceProfile {
    pub const ALL: [Self; 3] = [Self::V27, Self::V35, Self::V40];

    pub const fn name(self) -> &'static str {
        match self {
            Self::V27 => "qr27",
            Self::V35 => "qr35",
            Self::V40 => "qr40",
        }
    }

    pub const fn version(self) -> u8 {
        match self {
            Self::V27 => 27,
            Self::V35 => 35,
            Self::V40 => 40,
        }
    }

    pub const fn symbol_capacity(self) -> u16 {
        match self {
            Self::V27 => 1432,
            Self::V35 => 2264,
            Self::V40 => 2920,
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        Self::ALL.into_iter().find(|profile| profile.name() == name)
    }
}

/// Stream header bytes, carried by every packet.
pub const STREAM_HEADER_LEN: usize = 24;

/// Fields every packet of a stream repeats verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamHeader {
    pub stream_id: u32,
    /// Fountain configuration; the RFC 6330 OTI for RaptorQ. Carries the exact
    /// object length, which is why the manifest does not.
    pub config: [u8; CONFIG_LEN],
    /// First four bytes of the object's BLAKE3 hash. Binds every packet to the
    /// manifest that can verify it; the full hash rides in the manifest.
    pub hash_head: [u8; 4],
}

impl StreamHeader {
    fn write_into(&self, buf: &mut [u8], flags: u8) {
        buf[0..2].copy_from_slice(&MAGIC);
        buf[2] = VERSION;
        buf[3] = flags;
        buf[4..8].copy_from_slice(&self.stream_id.to_le_bytes());
        buf[8..20].copy_from_slice(&self.config);
        buf[20..24].copy_from_slice(&self.hash_head);
    }

    fn parse(bytes: &[u8]) -> Option<(Self, u8)> {
        if bytes.len() < STREAM_HEADER_LEN || bytes[0..2] != MAGIC || bytes[2] != VERSION {
            return None;
        }
        Some((
            Self {
                stream_id: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
                config: bytes[8..20].try_into().ok()?,
                hash_head: bytes[20..24].try_into().ok()?,
            },
            bytes[3],
        ))
    }
}

fn crc(bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

/// What one packet carries on the wire.
enum Framed<'a> {
    Symbol(&'a [u8]),
    Manifest(&'a [u8]),
}

/// Frame one complete packet: stream header, the payload, then a CRC over both.
///
/// There is no padding and no inner ECC layer: QR's own L-level ECC corrects a
/// symbol or its decoder rejects it wholesale. The CRC remains because it is
/// the firewall before RaptorQ.
fn frame_packet(header: &StreamHeader, payload: Framed<'_>) -> Vec<u8> {
    let flags = match payload {
        Framed::Manifest(_) => FLAG_MANIFEST,
        Framed::Symbol(_) => 0,
    };
    let bytes = match payload {
        Framed::Symbol(bytes) | Framed::Manifest(bytes) => bytes,
    };
    let mut buf = vec![0u8; STREAM_HEADER_LEN + bytes.len() + 4];
    header.write_into(&mut buf[..STREAM_HEADER_LEN], flags);
    buf[STREAM_HEADER_LEN..STREAM_HEADER_LEN + bytes.len()].copy_from_slice(bytes);
    let end = STREAM_HEADER_LEN + bytes.len();
    let checksum = crc(&buf[..end]).to_le_bytes();
    buf[end..].copy_from_slice(&checksum);
    buf
}

/// What one packet turned out to carry.
enum Payload<'a> {
    Symbol(&'a [u8]),
    Manifest(Manifest),
}

fn check_crc(bytes: &[u8], end: usize) -> Option<()> {
    let expected = u32::from_le_bytes(bytes.get(end..end + 4)?.try_into().ok()?);
    (crc(&bytes[..end]) == expected).then_some(())
}

/// Parse an exact, header-bearing packet, verifying the CRC.
///
/// `None` means the packet is unusable — an erasure, never an error.
fn parse_packet(bytes: &[u8]) -> Option<(StreamHeader, Payload<'_>)> {
    let (header, flags) = StreamHeader::parse(bytes)?;
    let at = STREAM_HEADER_LEN;
    if flags & FLAG_MANIFEST != 0 {
        let (manifest, used) = Manifest::parse(bytes.get(at..)?)?;
        (bytes.len() == at + used + 4).then_some(())?;
        check_crc(bytes, at + used)?;
        return Some((header, Payload::Manifest(manifest)));
    }

    // Symbol length comes from the fountain config, not the wire, because
    // RaptorQ picks a symbol size that may be smaller than the space offered.
    let symbol_len = RaptorQSink::probe(&header.config)?;
    let end = at
        + if symbol_len == 0 {
            0
        } else {
            SYMBOL_ID_LEN + symbol_len
        };
    let symbol = bytes.get(at..end)?;
    (bytes.len() == end + 4).then_some(())?;
    check_crc(bytes, end)?;
    Some((header, Payload::Symbol(symbol)))
}

/// Prepared skin-side packet stream. The expensive RaptorQ intermediate-symbol
/// solve happens once; packets are framed individually when the display asks
/// for them, so startup and memory stay proportional to the file.
///
/// The name is historical — this began as the *reference* transport beside a
/// custom optical raster, and the browser still calls the mode QR Reference.
pub struct ReferenceEncoder {
    fountain: RaptorQ,
    manifest: Vec<u8>,
    header: StreamHeader,
    symbols: u32,
    packets: usize,
}

impl ReferenceEncoder {
    /// Encode a file as a looping packet stream at a fixed density rung.
    ///
    /// `name` and `mime` ride in the manifest so the far end can display and
    /// save the file as itself. `overhead` is repair symbols per source
    /// symbol: the skin loops forever, so this only bounds how long the loop
    /// is before it repeats — but a longer loop means a receiver that missed
    /// a frame waits less time for a *different* one rather than the same one
    /// again.
    pub fn new(
        object: &[u8],
        name: &str,
        mime: &str,
        profile: ReferenceProfile,
        stream_id: u32,
        overhead: f32,
    ) -> Result<Self> {
        let symbol_capacity = profile.symbol_capacity();
        let (encoded, compression) = prepare_object(object, mime)?;
        let fountain = RaptorQ::new(&encoded, symbol_capacity)?;
        let manifest = Manifest::describe_encoded(name, mime, object, compression);
        let hash_head: [u8; 4] = manifest.hash[..4].try_into().expect("hash has 32 bytes");
        let manifest = manifest.to_bytes();
        if manifest.len() > symbol_capacity as usize {
            return Err(Error::PayloadTooLarge {
                len: manifest.len(),
                capacity: symbol_capacity as usize,
            });
        }

        let source = fountain.source_symbols();
        let repair = (source as f32 * overhead.max(0.0)).ceil() as u32;
        // An empty object still needs one empty symbol to make the sink
        // produce the empty object; packet 0 itself carries the manifest.
        let symbols = if source == 0 { 1 } else { source + repair };
        let mut packets = 0usize;
        let mut slots = 0usize;
        while slots < symbols as usize || packets == 0 {
            if !is_manifest_slot(packets) {
                slots += 1;
            }
            packets += 1;
        }

        let header = StreamHeader {
            stream_id,
            config: fountain.config(),
            hash_head,
        };
        Ok(Self {
            fountain,
            manifest,
            header,
            symbols,
            packets,
        })
    }

    pub fn packet_count(&self) -> usize {
        self.packets
    }

    /// One complete QR payload, wrapping around as long as the skin sends.
    pub fn packet(&self, index: usize) -> Vec<u8> {
        let index = index % self.packets;
        if is_manifest_slot(index) {
            return frame_packet(&self.header, Framed::Manifest(&self.manifest));
        }
        let ordinal = index - manifest_slots_before(index);
        debug_assert!(ordinal < self.symbols as usize);
        let symbol = self.fountain.symbol(ordinal as u32);
        frame_packet(&self.header, Framed::Symbol(&symbol))
    }
}

/// Compress only when the media type is plausibly compressible and the result
/// wins by enough to pay for format metadata. This mirrors Decimen's useful
/// policy, but compression remains inside Cuttldrop's authenticated manifest.
fn prepare_object<'a>(object: &'a [u8], mime: &str) -> Result<(Cow<'a, [u8]>, ObjectCompression)> {
    if object.len() < 768 || is_precompressed(mime) {
        return Ok((Cow::Borrowed(object), ObjectCompression::None));
    }
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(object)
        .map_err(|error| Error::Compression(error.to_string()))?;
    let compressed = encoder
        .finish()
        .map_err(|error| Error::Compression(error.to_string()))?;
    if compressed.len() + 64 < object.len() {
        Ok((Cow::Owned(compressed), ObjectCompression::Deflate))
    } else {
        Ok((Cow::Borrowed(object), ObjectCompression::None))
    }
}

fn is_precompressed(mime: &str) -> bool {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    mime.starts_with("video/")
        || matches!(
            mime.as_str(),
            "application/gzip"
                | "application/java-archive"
                | "application/vnd.rar"
                | "application/x-7z-compressed"
                | "application/x-rar-compressed"
                | "application/zip"
                | "application/zstd"
        )
        || (mime.starts_with("image/")
            && !matches!(
                mime.as_str(),
                "image/bmp" | "image/svg+xml" | "image/tiff" | "image/x-icon"
            ))
        || (mime.starts_with("audio/")
            && !matches!(mime.as_str(), "audio/wav" | "audio/x-wav" | "audio/aiff"))
}

fn restore_object(manifest: &Manifest, encoded: &[u8]) -> Result<Vec<u8>> {
    let expected = usize::try_from(manifest.original_len)
        .map_err(|_| Error::Compression("declared length does not fit this device".into()))?;
    let object = match manifest.compression {
        ObjectCompression::None => encoded.to_vec(),
        ObjectCompression::Deflate => {
            let decoder = DeflateDecoder::new(encoded);
            let mut limited = decoder.take(manifest.original_len.saturating_add(1));
            // The manifest crossed an untrusted optical channel. Its CRC and
            // hash protect correctness, not resource use; never reserve an
            // attacker-declared multi-gigabyte length before decompression has
            // produced those bytes.
            let mut out = Vec::with_capacity(expected.min(8 * 1024 * 1024));
            limited
                .read_to_end(&mut out)
                .map_err(|error| Error::Compression(error.to_string()))?;
            out
        }
    };
    if object.len() != expected {
        return Err(Error::Compression(format!(
            "declared {expected} B, restored {} B",
            object.len()
        )));
    }
    Ok(object)
}

/// What one packet amounted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingest {
    /// A new symbol (or the manifest) absorbed.
    Accepted,
    /// Absorbed, and the object came out — reconstructed *and* manifest in
    /// hand, so [`Receiver::finish`] can verify it.
    Completed,
    /// A symbol already held, or a packet arriving after completion.
    Duplicate,
    /// Failed the CRC gate, or belongs to another transfer.
    Rejected,
}

/// Absorbs packets until the object falls out.
pub struct Receiver {
    stream_id: Option<u32>,
    stream_config: Option<[u8; CONFIG_LEN]>,
    hash_head: [u8; 4],
    /// A different, CRC-valid stream seen once. Two consecutive headers are
    /// required before abandoning the active transfer, so one fantastically
    /// unlikely CRC false-positive cannot erase useful progress.
    candidate: Option<(StreamHeader, u8)>,
    sink: Option<RaptorQSink>,
    manifest: Option<Manifest>,
    seen: HashSet<[u8; SYMBOL_ID_LEN]>,
    accepted: u32,
    rejected: u32,
    object: Option<Vec<u8>>,
}

impl Default for Receiver {
    fn default() -> Self {
        Self::new()
    }
}

impl Receiver {
    pub fn new() -> Self {
        Self {
            stream_id: None,
            stream_config: None,
            hash_head: [0; 4],
            candidate: None,
            sink: None,
            manifest: None,
            seen: HashSet::new(),
            accepted: 0,
            rejected: 0,
            object: None,
        }
    }

    /// Absorb one decoded packet.
    ///
    /// The QR decoder has already located, sampled and applied its own ECC, so
    /// this begins at the CRC gate: nothing unverified reaches the fountain.
    pub fn ingest_packet(&mut self, bytes: &[u8]) -> Ingest {
        if self.is_complete() {
            return Ingest::Duplicate;
        }
        let Some((header, payload)) = parse_packet(bytes) else {
            self.rejected += 1;
            return Ingest::Rejected;
        };
        if !self.adopt(header) {
            self.rejected += 1;
            return Ingest::Rejected;
        }

        match payload {
            Payload::Manifest(manifest) => {
                // The header's hash head binds packets to their manifest; a
                // manifest that disagrees belongs to some other transfer.
                if manifest.hash[..4] != self.hash_head {
                    self.rejected += 1;
                    return Ingest::Rejected;
                }
                if self.manifest.is_some() {
                    return Ingest::Duplicate;
                }
                self.manifest = Some(manifest);
            }
            Payload::Symbol(symbol) => {
                if self.object.is_some() {
                    return Ingest::Duplicate;
                }
                if symbol.len() >= SYMBOL_ID_LEN {
                    let id: [u8; SYMBOL_ID_LEN] =
                        symbol[..SYMBOL_ID_LEN].try_into().expect("checked length");
                    if !self.seen.insert(id) {
                        return Ingest::Duplicate;
                    }
                }
                let Some(sink) = self.sink.as_mut() else {
                    self.rejected += 1;
                    return Ingest::Rejected;
                };
                self.accepted += 1;
                if let Some(object) = sink.absorb(symbol) {
                    self.object = Some(object);
                }
            }
        }
        if self.is_complete() {
            Ingest::Completed
        } else {
            Ingest::Accepted
        }
    }

    /// Follow a stream, changing over only after two consecutive CRC-valid
    /// headers agree on the replacement.
    ///
    /// A receiver locked onto the first stream forever would reject every
    /// packet after the skin selects a new file, until the page reloaded. One
    /// foreign packet is still ignored; seeing the same complete header twice
    /// is the deliberate handover signal. The CRC gate has already accepted
    /// the packet before this method runs.
    fn adopt(&mut self, header: StreamHeader) -> bool {
        let current = self.stream_id == Some(header.stream_id)
            && self.stream_config == Some(header.config)
            && self.hash_head == header.hash_head;
        if current {
            self.candidate = None;
            return true;
        }
        if self.stream_id.is_none() {
            return self.start_stream(header);
        }

        let sightings = match self.candidate {
            Some((candidate, count)) if candidate == header => count.saturating_add(1),
            _ => 1,
        };
        self.candidate = Some((header, sightings));
        sightings >= 2 && self.start_stream(header)
    }

    /// Replace all state that belongs to one transfer. Diagnostic counters
    /// reset too: after handover, the UI must describe the new stream rather
    /// than carrying the old stream's failures forward.
    fn start_stream(&mut self, header: StreamHeader) -> bool {
        let Ok(sink) = RaptorQSink::new(&header.config) else {
            return false;
        };
        self.stream_id = Some(header.stream_id);
        self.stream_config = Some(header.config);
        self.hash_head = header.hash_head;
        self.candidate = None;
        self.sink = Some(sink);
        self.manifest = None;
        self.seen.clear();
        self.accepted = 0;
        self.rejected = 0;
        self.object = None;
        true
    }

    /// Symbols absorbed versus the minimum needed. Honest and monotonic — not a
    /// fabricated "percent decoded" (§3c). The numerator can exceed the
    /// denominator: RaptorQ needs a small overhead above K.
    pub fn progress(&self) -> (u32, u32) {
        (
            self.accepted,
            self.sink.as_ref().map_or(0, |s| s.source_symbols()),
        )
    }

    /// The manifest, from the first manifest packet onward — typically long
    /// before the object converges, which is the point (§3c): the eye can say
    /// what it is receiving a second in.
    pub fn manifest(&self) -> Option<&Manifest> {
        self.manifest.as_ref()
    }

    /// Exact object length, known as soon as any packet is understood — the
    /// fountain config carries it.
    pub fn expected_len(&self) -> Option<u64> {
        self.manifest
            .as_ref()
            .map(|manifest| manifest.original_len)
            .or_else(|| self.sink.as_ref().map(|sink| sink.transfer_length()))
    }

    /// Bytes of the object each accepted symbol stands for, once the config is
    /// known. Multiplied by [`Receiver::progress`]'s numerator this is the only
    /// honest way to state a *rate* mid-transfer: symbols are not bytes of the
    /// file until the fountain converges, but each one is worth this many.
    pub fn symbol_len(&self) -> Option<usize> {
        self.sink.as_ref().map(|sink| sink.symbol_len())
    }

    /// Packets dropped by the CRC gate.
    pub fn rejected(&self) -> u32 {
        self.rejected
    }

    /// Object reconstructed *and* manifest in hand — everything
    /// [`Receiver::finish`] needs to verify and hand the file back.
    pub fn is_complete(&self) -> bool {
        self.object.is_some() && self.manifest.is_some()
    }

    /// The reconstructed object, verified against the manifest's BLAKE3 hash.
    /// Fails loudly rather than returning unverified bytes (§3f).
    pub fn finish(&self) -> Result<Vec<u8>> {
        if self.stream_id.is_none() {
            return Err(Error::Empty);
        }
        let Some(encoded) = &self.object else {
            let (have, need) = self.progress();
            return Err(Error::NotConverged { have, need });
        };
        let Some(manifest) = &self.manifest else {
            return Err(Error::NoManifest);
        };
        let object = restore_object(manifest, encoded)?;
        if blake3::hash(&object).as_bytes() != &manifest.hash {
            return Err(Error::ObjectHash);
        }
        Ok(object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn encode(object: &[u8], stream_id: u32, overhead: f32) -> ReferenceEncoder {
        ReferenceEncoder::new(object, "", "", ReferenceProfile::V27, stream_id, overhead).unwrap()
    }

    fn absorb_all(skin: &ReferenceEncoder) -> Receiver {
        let mut rx = Receiver::new();
        for index in 0..skin.packet_count() {
            rx.ingest_packet(&skin.packet(index));
        }
        rx
    }

    #[test]
    fn empty_object_roundtrips() {
        let skin = encode(&[], 1, 0.0);
        assert_eq!(absorb_all(&skin).finish().unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn every_profile_roundtrips_the_verified_stream() {
        let object: Vec<u8> = (0..18_000u32).map(|n| (n * 31) as u8).collect();
        for profile in ReferenceProfile::ALL {
            let skin = ReferenceEncoder::new(
                &object,
                "reference.bin",
                "application/test",
                profile,
                19,
                0.5,
            )
            .unwrap();
            let mut eye = Receiver::new();
            for index in 0..skin.packet_count() {
                if eye.ingest_packet(&skin.packet(index)) == Ingest::Completed {
                    break;
                }
            }
            assert_eq!(
                eye.manifest().map(|manifest| manifest.name.as_str()),
                Some("reference.bin"),
                "{}",
                profile.name()
            );
            assert_eq!(eye.finish().unwrap(), object, "{}", profile.name());
        }
    }

    /// The manifest names the file long before the object converges (§3c).
    #[test]
    fn manifest_arrives_first_and_names_the_object() {
        let object = vec![0xABu8; 20_000];
        let skin = ReferenceEncoder::new(
            &object,
            "cuttlefish.pdf",
            "application/pdf",
            ReferenceProfile::V27,
            7,
            0.5,
        )
        .unwrap();
        let mut rx = Receiver::new();
        assert_eq!(rx.ingest_packet(&skin.packet(0)), Ingest::Accepted);

        let manifest = rx.manifest().expect("packet 0 carries the manifest");
        assert_eq!(manifest.name, "cuttlefish.pdf");
        assert_eq!(manifest.mime, "application/pdf");
        assert_eq!(rx.expected_len(), Some(object.len() as u64));
        assert!(!rx.is_complete());
    }

    /// Completion is gated on the manifest: an object that cannot be verified
    /// is never handed back (§3f), however completely it reconstructed.
    #[test]
    fn finish_requires_the_manifest() {
        let object = vec![0x3Cu8; 8_000];
        let skin = encode(&object, 2, 1.0);
        let mut rx = Receiver::new();
        for index in 0..skin.packet_count() {
            if is_manifest_slot(index) {
                continue; // withhold every manifest packet
            }
            rx.ingest_packet(&skin.packet(index));
        }
        assert!(!rx.is_complete(), "complete without a manifest");
        assert!(matches!(rx.finish(), Err(Error::NoManifest)));

        // The next manifest packet is all that was missing.
        assert_eq!(rx.ingest_packet(&skin.packet(0)), Ingest::Completed);
        assert_eq!(rx.finish().unwrap(), object);
    }

    /// The cadence helpers must agree with each other exactly: `packet`
    /// subtracts `manifest_slots_before` to find a symbol's ordinal, and an
    /// off-by-one anywhere silently hands the eye the wrong symbol under the
    /// right index. Walk the whole schedule and check the count at every
    /// step.
    #[test]
    fn manifest_cadence_counts_agree() {
        let mut seen = 0usize;
        for index in 0..10_000 {
            assert_eq!(
                manifest_slots_before(index),
                seen,
                "count drifted at {index}"
            );
            if is_manifest_slot(index) {
                seen += 1;
            }
        }
        // Dense head, sparse steady state.
        assert!(is_manifest_slot(0) && is_manifest_slot(8) && is_manifest_slot(24));
        assert!(!is_manifest_slot(32) && !is_manifest_slot(40));
        assert!(is_manifest_slot(48) && is_manifest_slot(72));
    }

    /// With no repair symbols, loss must fail cleanly rather than return wrong
    /// bytes. Packet 1, not packet 0: the first packet carries the manifest,
    /// and what this test needs to lose is a *symbol*.
    #[test]
    fn zero_overhead_plus_loss_fails_loudly() {
        let object = vec![3u8; 6000];
        let skin = encode(&object, 1, 0.0);
        let mut rx = Receiver::new();
        for index in 0..skin.packet_count() {
            if index == 1 {
                continue;
            }
            rx.ingest_packet(&skin.packet(index));
        }
        assert!(matches!(rx.finish(), Err(Error::NotConverged { .. })));
    }

    #[test]
    fn duplicates_are_recognised_not_double_counted() {
        let skin = encode(&[3u8; 300], 1, 0.0);
        let mut rx = Receiver::new();
        // Packet 0 is the manifest copy; its first arrival is news, its second
        // is not. Packet 1 carries the only symbol a 300 B object needs at
        // this rung, so it completes the transfer — and repeats are still
        // recognised, not double counted.
        assert_eq!(rx.ingest_packet(&skin.packet(0)), Ingest::Accepted);
        assert_eq!(rx.ingest_packet(&skin.packet(0)), Ingest::Duplicate);
        assert_eq!(rx.ingest_packet(&skin.packet(1)), Ingest::Completed);
        assert_eq!(rx.ingest_packet(&skin.packet(1)), Ingest::Duplicate);
        assert_eq!(rx.progress().0, 1);
    }

    /// A corrupted packet is an erasure at the CRC gate, never an error — and
    /// corruption must never reach the object.
    #[test]
    fn corruption_hits_the_crc_gate() {
        let object = vec![0x5Au8; 4000];
        let skin = encode(&object, 1, 1.0);
        let mut rx = Receiver::new();
        for index in 0..skin.packet_count() {
            let mut packet = skin.packet(index);
            let flip = STREAM_HEADER_LEN + 5 + (index % 16);
            packet[flip] ^= 0x40;
            assert_eq!(rx.ingest_packet(&packet), Ingest::Rejected);
        }
        assert_eq!(rx.rejected(), skin.packet_count() as u32);
        assert!(!rx.is_complete());
        assert!(rx.finish().is_err());
    }

    #[test]
    fn a_foreign_stream_in_view_is_ignored() {
        // This test is about handover, not compression. Mark the synthetic
        // bytes as already compressed so they still require several symbols.
        let ours = ReferenceEncoder::new(
            &[1u8; 4000],
            "",
            "application/zip",
            ReferenceProfile::V27,
            111,
            0.0,
        )
        .unwrap();
        let theirs = ReferenceEncoder::new(
            &[2u8; 4000],
            "",
            "application/zip",
            ReferenceProfile::V27,
            222,
            0.0,
        )
        .unwrap();
        let mut rx = Receiver::new();
        assert_eq!(rx.ingest_packet(&ours.packet(0)), Ingest::Accepted);
        assert_eq!(rx.ingest_packet(&theirs.packet(0)), Ingest::Rejected);
        // Returning to the active stream cancels the one-packet candidate.
        assert_eq!(rx.ingest_packet(&ours.packet(1)), Ingest::Accepted);
        assert_eq!(rx.ingest_packet(&theirs.packet(0)), Ingest::Rejected);
        assert_eq!(rx.ingest_packet(&ours.packet(2)), Ingest::Accepted);
    }

    #[test]
    fn two_valid_headers_move_the_eye_to_a_new_stream() {
        let old = ReferenceEncoder::new(
            &[1u8; 4000],
            "",
            "application/zip",
            ReferenceProfile::V27,
            111,
            0.5,
        )
        .unwrap();
        let object = vec![2u8; 4000];
        let new = ReferenceEncoder::new(
            &object,
            "",
            "application/zip",
            ReferenceProfile::V27,
            222,
            0.5,
        )
        .unwrap();
        let mut rx = Receiver::new();

        assert_eq!(rx.ingest_packet(&old.packet(0)), Ingest::Accepted);
        assert_eq!(rx.ingest_packet(&old.packet(1)), Ingest::Accepted);
        assert_eq!(rx.progress().0, 1);

        // One packet could be a foreign screen briefly crossing the camera.
        assert_eq!(rx.ingest_packet(&new.packet(0)), Ingest::Rejected);
        assert_eq!(rx.progress().0, 1);
        // The second CRC-valid header is intent. It resets the old progress and
        // absorbs this packet as the first contribution to the new stream.
        assert_eq!(rx.ingest_packet(&new.packet(0)), Ingest::Accepted);
        assert_eq!(rx.progress().0, 0); // packet 0 is the manifest
        assert_eq!(
            rx.manifest().unwrap().hash,
            *blake3::hash(&object).as_bytes()
        );

        for index in 0..new.packet_count() {
            rx.ingest_packet(&new.packet(index));
            if rx.is_complete() {
                break;
            }
        }
        assert_eq!(rx.finish().unwrap(), object);
    }

    #[test]
    fn useful_compression_shortens_the_stream_and_restores_the_original() {
        let object = "chromatophore pulse\n".repeat(5000).into_bytes();
        let compressed = ReferenceEncoder::new(
            &object,
            "notes.txt",
            "text/plain",
            ReferenceProfile::V27,
            7,
            0.2,
        )
        .unwrap();
        let raw = ReferenceEncoder::new(
            &object,
            "notes.txt",
            "application/zip",
            ReferenceProfile::V27,
            8,
            0.2,
        )
        .unwrap();

        assert!(compressed.packet_count() < raw.packet_count());
        let rx = absorb_all(&compressed);
        assert_eq!(
            rx.manifest().unwrap().compression,
            ObjectCompression::Deflate
        );
        assert_eq!(rx.expected_len(), Some(object.len() as u64));
        assert_eq!(rx.finish().unwrap(), object);
    }

    proptest! {
        #[test]
        fn object_roundtrips(data in prop::collection::vec(any::<u8>(), 0..4096)) {
            let skin = encode(&data, 1, 0.2);
            prop_assert_eq!(absorb_all(&skin).finish().unwrap(), data);
        }

        /// Names and mime types survive the trip exactly, whatever they are.
        #[test]
        fn manifest_metadata_roundtrips(
            name in "[a-zA-Z0-9._ -]{0,40}",
            mime in "[a-z]{0,10}(/[a-z0-9.+-]{1,15})?",
        ) {
            let skin = ReferenceEncoder::new(
                &[7u8; 600], &name, &mime, ReferenceProfile::V27, 6, 0.0,
            ).unwrap();
            let rx = absorb_all(&skin);
            let manifest = rx.manifest().expect("manifest always travels");
            prop_assert_eq!(&manifest.name, &name);
            prop_assert_eq!(&manifest.mime, &mime);
            rx.finish().unwrap();
        }
    }
}
