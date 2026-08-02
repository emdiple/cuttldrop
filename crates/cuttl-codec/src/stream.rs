//! Object → pulses → object.
//!
//! **This is M0 scaffolding, not the shipping transport.** It splits an object
//! into fixed chunks, one per pulse, and reassembles them by index. That is a
//! carousel, not a fountain: it needs *every* pulse, so it cannot survive loss.
//!
//! The outer RaptorQ layer (`DESIGN.md` §1a, §3c) replaces the chunking here,
//! at which point loss stops mattering and `--loss 0.6` becomes decodable. The
//! inner Reed–Solomon layer (§1b) then slots in below the CRC. What is already
//! correct and will *not* change is the CRC gate: a pulse that fails its CRC is
//! dropped rather than repaired, converting an error into an erasure, because
//! feeding a corrupt symbol to a fountain decoder poisons the whole object.

use crate::error::{Error, Result};
use crate::geometry::Grid;
use crate::palette::Palette;
use crate::pulse::Pulse;
use std::collections::BTreeMap;

const MAGIC: [u8; 2] = *b"CD";
const VERSION: u8 = 0;

/// Framing header, prepended to every pulse's payload.
///
/// Temporary home: per §3a this data belongs in the beacon strip, ECC-protected
/// and duplicated top and bottom. Doing the arithmetic on the M1 grid shows the
/// beacon holds only ~4 bytes, so the full header cannot live there — see the
/// note in `DESIGN.md` §3a about what the beacon should actually carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PulseHeader {
    pub stream_id: u32,
    pub pulse_index: u32,
    pub total_pulses: u32,
    pub object_len: u32,
    pub object_crc: u32,
    pub payload_len: u16,
}

/// Header bytes, including the trailing CRC.
pub const HEADER_LEN: usize = 29;
const CRC_OFFSET: usize = 25;

impl PulseHeader {
    fn write_into(&self, buf: &mut [u8], payload: &[u8]) {
        buf[0..2].copy_from_slice(&MAGIC);
        buf[2] = VERSION;
        buf[3..7].copy_from_slice(&self.stream_id.to_le_bytes());
        buf[7..11].copy_from_slice(&self.pulse_index.to_le_bytes());
        buf[11..15].copy_from_slice(&self.total_pulses.to_le_bytes());
        buf[15..19].copy_from_slice(&self.object_len.to_le_bytes());
        buf[19..23].copy_from_slice(&self.object_crc.to_le_bytes());
        buf[23..25].copy_from_slice(&self.payload_len.to_le_bytes());
        let crc = crc(&buf[..CRC_OFFSET], payload);
        buf[CRC_OFFSET..HEADER_LEN].copy_from_slice(&crc.to_le_bytes());
    }

    /// Parse and verify. `None` means "not a valid pulse" — magic, version or
    /// CRC failed — which the caller must treat as an erasure, never an error.
    fn parse(bytes: &[u8]) -> Option<(Self, &[u8])> {
        if bytes.len() < HEADER_LEN || bytes[0..2] != MAGIC || bytes[2] != VERSION {
            return None;
        }
        let header = Self {
            stream_id: u32::from_le_bytes(bytes[3..7].try_into().ok()?),
            pulse_index: u32::from_le_bytes(bytes[7..11].try_into().ok()?),
            total_pulses: u32::from_le_bytes(bytes[11..15].try_into().ok()?),
            object_len: u32::from_le_bytes(bytes[15..19].try_into().ok()?),
            object_crc: u32::from_le_bytes(bytes[19..23].try_into().ok()?),
            payload_len: u16::from_le_bytes(bytes[23..25].try_into().ok()?),
        };
        let body = bytes.get(HEADER_LEN..)?;
        let payload = body.get(..header.payload_len as usize)?;
        let expected = u32::from_le_bytes(bytes[CRC_OFFSET..HEADER_LEN].try_into().ok()?);
        if crc(&bytes[..CRC_OFFSET], payload) != expected {
            return None;
        }
        Some((header, payload))
    }
}

fn crc(header: &[u8], payload: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(header);
    hasher.update(payload);
    hasher.finalize()
}

/// Payload bytes per pulse once the header is subtracted.
pub fn chunk_capacity(grid: Grid, palette: Palette) -> Result<usize> {
    let capacity = grid.payload_bytes(palette);
    capacity
        .checked_sub(HEADER_LEN)
        .filter(|&n| n > 0)
        .ok_or(Error::NoRoomForHeader {
            capacity,
            header: HEADER_LEN,
        })
}

/// Split an object into a full sequence of pulses.
pub fn encode(object: &[u8], grid: Grid, palette: Palette, stream_id: u32) -> Result<Vec<Pulse>> {
    grid.validate()?;
    let chunk = chunk_capacity(grid, palette)?;
    let total = object.len().div_ceil(chunk).max(1) as u32;
    let object_crc = crc(&[], object);

    let mut buf = vec![0u8; HEADER_LEN + chunk];
    let mut pulses = Vec::with_capacity(total as usize);

    for index in 0..total {
        let start = index as usize * chunk;
        let slice = &object[start.min(object.len())..((start + chunk).min(object.len()))];

        let header = PulseHeader {
            stream_id,
            pulse_index: index,
            total_pulses: total,
            object_len: object.len() as u32,
            object_crc,
            payload_len: slice.len() as u16,
        };

        buf.iter_mut().for_each(|b| *b = 0);
        buf[HEADER_LEN..HEADER_LEN + slice.len()].copy_from_slice(slice);
        header.write_into(&mut buf, slice);

        let mut pulse = Pulse::new(grid, palette)?;
        pulse.write_payload(&buf)?;
        pulses.push(pulse);
    }
    Ok(pulses)
}

/// What happened to an ingested pulse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingest {
    Accepted,
    /// Already had this chunk. Expected and harmless — the skin loops forever.
    Duplicate,
    /// Failed the CRC gate, or belongs to a different stream. Treated as an
    /// erasure; never an error (§1b).
    Rejected,
}

/// Collects pulses until the object can be rebuilt.
#[derive(Debug, Default)]
pub struct Reassembler {
    stream_id: Option<u32>,
    total: u32,
    object_len: u32,
    object_crc: u32,
    chunks: BTreeMap<u32, Vec<u8>>,
    rejected: u32,
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ingest(&mut self, pulse: &Pulse) -> Ingest {
        let bytes = pulse.read_payload();
        let Some((header, payload)) = PulseHeader::parse(&bytes) else {
            self.rejected += 1;
            return Ingest::Rejected;
        };

        match self.stream_id {
            // Lock onto the first stream we see and ignore any other in view.
            Some(id) if id != header.stream_id => {
                self.rejected += 1;
                return Ingest::Rejected;
            }
            Some(_) => {}
            None => {
                self.stream_id = Some(header.stream_id);
                self.total = header.total_pulses;
                self.object_len = header.object_len;
                self.object_crc = header.object_crc;
            }
        }

        if header.total_pulses != self.total
            || header.object_len != self.object_len
            || header.object_crc != self.object_crc
            || header.pulse_index >= self.total
        {
            self.rejected += 1;
            return Ingest::Rejected;
        }

        if self.chunks.contains_key(&header.pulse_index) {
            return Ingest::Duplicate;
        }
        self.chunks.insert(header.pulse_index, payload.to_vec());
        Ingest::Accepted
    }

    /// Chunks held versus chunks needed. This is the honest progress number —
    /// monotonic, and not a fabricated "percent decoded" (§3c).
    pub fn progress(&self) -> (u32, u32) {
        (self.chunks.len() as u32, self.total)
    }

    pub fn rejected(&self) -> u32 {
        self.rejected
    }

    pub fn is_complete(&self) -> bool {
        self.stream_id.is_some() && self.chunks.len() as u32 == self.total
    }

    /// Rebuild the object, verifying its CRC. Fails loudly rather than
    /// returning unverified bytes (§3f).
    pub fn finish(&self) -> Result<Vec<u8>> {
        if self.stream_id.is_none() {
            return Err(Error::Empty);
        }
        if !self.is_complete() {
            return Err(Error::Incomplete {
                missing: self.total - self.chunks.len() as u32,
                total: self.total,
            });
        }
        let mut object = Vec::with_capacity(self.object_len as usize);
        for chunk in self.chunks.values() {
            object.extend_from_slice(chunk);
        }
        object.truncate(self.object_len as usize);

        let got = crc(&[], &object);
        if got != self.object_crc {
            return Err(Error::ObjectCrc {
                expected: self.object_crc,
                got,
            });
        }
        Ok(object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn roundtrip(object: &[u8], grid: Grid, palette: Palette) -> Result<Vec<u8>> {
        let pulses = encode(object, grid, palette, 0x5EED)?;
        let mut rx = Reassembler::new();
        for pulse in &pulses {
            rx.ingest(pulse);
        }
        rx.finish()
    }

    #[test]
    fn empty_object_roundtrips() {
        assert_eq!(
            roundtrip(&[], Grid::M1_MONO, Palette::Mono1).unwrap(),
            Vec::<u8>::new()
        );
    }

    #[test]
    fn incomplete_stream_refuses_to_produce_bytes() {
        let pulses = encode(&[7u8; 400], Grid::M1_MONO, Palette::Mono1, 1).unwrap();
        let mut rx = Reassembler::new();
        for pulse in pulses.iter().skip(1) {
            rx.ingest(pulse);
        }
        assert!(matches!(rx.finish(), Err(Error::Incomplete { .. })));
    }

    #[test]
    fn duplicates_are_recognised_not_double_counted() {
        let pulses = encode(&[3u8; 300], Grid::M1_MONO, Palette::Mono1, 1).unwrap();
        let mut rx = Reassembler::new();
        assert_eq!(rx.ingest(&pulses[0]), Ingest::Accepted);
        assert_eq!(rx.ingest(&pulses[0]), Ingest::Duplicate);
        assert_eq!(rx.progress().0, 1);
    }

    /// The CRC gate: a single flipped payload cell must cause the pulse to be
    /// dropped, never silently accepted (§1b).
    #[test]
    fn corrupted_pulse_is_rejected_by_the_crc_gate() {
        let pulses = encode(&[9u8; 200], Grid::M1_MONO, Palette::Mono1, 1).unwrap();
        let mut corrupt = pulses[0].clone();
        let (x, y) = Grid::M1_MONO.payload_coords().nth(64).unwrap();
        let flipped = corrupt.cell(x, y).unwrap() ^ 1;
        corrupt.set_cell(x, y, flipped).unwrap();

        let mut rx = Reassembler::new();
        assert_eq!(rx.ingest(&corrupt), Ingest::Rejected);
        assert_eq!(rx.rejected(), 1);
    }

    #[test]
    fn a_foreign_stream_in_view_is_ignored() {
        let ours = encode(&[1u8; 200], Grid::M1_MONO, Palette::Mono1, 111).unwrap();
        let theirs = encode(&[2u8; 200], Grid::M1_MONO, Palette::Mono1, 222).unwrap();
        let mut rx = Reassembler::new();
        assert_eq!(rx.ingest(&ours[0]), Ingest::Accepted);
        assert_eq!(rx.ingest(&theirs[0]), Ingest::Rejected);
    }

    proptest! {
        /// The M0 observable, in miniature: any object, byte-identical back.
        #[test]
        fn object_roundtrips(data in prop::collection::vec(any::<u8>(), 0..4096)) {
            prop_assert_eq!(roundtrip(&data, Grid::M1_MONO, Palette::Mono1).unwrap(), data);
        }

        #[test]
        fn object_roundtrips_in_colour(data in prop::collection::vec(any::<u8>(), 0..8192)) {
            prop_assert_eq!(roundtrip(&data, Grid::M3_COLOR, Palette::Color3).unwrap(), data);
        }
    }
}
