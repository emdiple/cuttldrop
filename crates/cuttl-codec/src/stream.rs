//! Object → pulses → object, over the fountain layer.
//!
//! Each pulse carries one fountain symbol plus the framing needed to interpret
//! it standalone. There is no pulse index and no total: a rateless stream has
//! neither. The eye absorbs symbols in any order, from any subset, and stops
//! when the object falls out (`DESIGN.md` §3c).
//!
//! ## The CRC gate
//!
//! The single most important line in this module is the CRC check in
//! [`PulseHeader::parse`]. A fountain decoder assumes every symbol it receives
//! is correct or absent; one silently corrupt symbol propagates through the XOR
//! graph and poisons the entire object. The gate converts an *error* into an
//! *erasure*, which is the one thing the fountain layer can actually repair
//! (§1b). A rejected pulse is never an `Err` — it is a normal, expected event.
//!
//! ## Still missing below this layer
//!
//! The inner Reed–Solomon code (§1b) sits *between* the cells and this header,
//! mopping up sparse cell errors so that a handful of misread cells does not
//! cost a whole symbol. Until it exists, one bad cell kills one pulse, and the
//! fountain has to carry the whole burden.

use crate::error::{Error, Result};
use crate::fountain::{CONFIG_LEN, Fountain, RaptorQ, RaptorQSink, SYMBOL_ID_LEN, Sink};
use crate::geometry::Grid;
use crate::palette::Palette;
use crate::pulse::Pulse;
use std::collections::HashSet;

const MAGIC: [u8; 2] = *b"CD";
const VERSION: u8 = 1;

/// Framing header, prepended to every pulse's payload.
///
/// Temporary home: per §3a the stream id belongs in the beacon strip, duplicated
/// top and bottom under heavy ECC. The M0 arithmetic showed the beacon holds
/// only ~4 bytes, so the fountain config cannot live there and rides here
/// instead — repeated every pulse, since the eye needs it before it can
/// interpret anything at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PulseHeader {
    pub stream_id: u32,
    /// Fountain configuration; RFC 6330 OTI for RaptorQ.
    pub config: [u8; CONFIG_LEN],
    /// CRC-32 of the whole object, checked after reconstruction. BLAKE3 replaces
    /// this when the manifest stream lands at M2 (§3c).
    pub object_crc: u32,
}

/// Header bytes, including the trailing CRC.
pub const HEADER_LEN: usize = 27;
const CRC_OFFSET: usize = 23;

impl PulseHeader {
    fn write_into(&self, buf: &mut [u8], symbol: &[u8]) {
        buf[0..2].copy_from_slice(&MAGIC);
        buf[2] = VERSION;
        buf[3..7].copy_from_slice(&self.stream_id.to_le_bytes());
        buf[7..19].copy_from_slice(&self.config);
        buf[19..23].copy_from_slice(&self.object_crc.to_le_bytes());
        let crc = crc(&buf[..CRC_OFFSET], symbol);
        buf[CRC_OFFSET..HEADER_LEN].copy_from_slice(&crc.to_le_bytes());
    }

    /// Parse and verify. `None` means "not a valid pulse" — bad magic, version
    /// or CRC — which the caller must treat as an erasure, never an error.
    ///
    /// `symbol_len` comes from the fountain config, not from the header, because
    /// RaptorQ picks a symbol size that may be smaller than the space offered.
    fn parse(bytes: &[u8], symbol_len: Option<usize>) -> Option<(Self, &[u8])> {
        if bytes.len() < HEADER_LEN || bytes[0..2] != MAGIC || bytes[2] != VERSION {
            return None;
        }
        let header = Self {
            stream_id: u32::from_le_bytes(bytes[3..7].try_into().ok()?),
            config: bytes[7..19].try_into().ok()?,
            object_crc: u32::from_le_bytes(bytes[19..23].try_into().ok()?),
        };

        // On the first pulse the eye does not yet know the symbol size, so it
        // derives it from this pulse's own config — which the CRC has not
        // vouched for yet. `probe` validates without building a decoder, so a
        // corrupt config becomes an erasure instead of a panic.
        let len = match symbol_len {
            Some(len) => len,
            None => RaptorQSink::probe(&header.config)?,
        };
        let want = if len == 0 { 0 } else { SYMBOL_ID_LEN + len };
        let symbol = bytes.get(HEADER_LEN..HEADER_LEN + want)?;

        let expected = u32::from_le_bytes(bytes[CRC_OFFSET..HEADER_LEN].try_into().ok()?);
        if crc(&bytes[..CRC_OFFSET], symbol) != expected {
            return None;
        }
        Some((header, symbol))
    }
}

fn crc(header: &[u8], payload: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(header);
    hasher.update(payload);
    hasher.finalize()
}

/// Largest fountain symbol one pulse can carry.
pub fn symbol_capacity(grid: Grid, palette: Palette) -> Result<u16> {
    let capacity = grid.payload_bytes(palette);
    capacity
        .checked_sub(HEADER_LEN + SYMBOL_ID_LEN)
        .filter(|&n| n > 0)
        .map(|n| n.min(u16::MAX as usize) as u16)
        .ok_or(Error::NoRoomForSymbol {
            capacity,
            header: HEADER_LEN,
            symbol_id: SYMBOL_ID_LEN,
        })
}

/// Encode an object as a batch of pulses.
///
/// `overhead` is the ratio of repair symbols to source symbols. It exists only
/// because a directory of PNGs has to stand in for a skin that loops forever;
/// at `0.0` the output is exactly the source symbols and survives no loss at
/// all, which is why the CLI defaults it much higher.
pub fn encode(
    object: &[u8],
    grid: Grid,
    palette: Palette,
    stream_id: u32,
    overhead: f32,
) -> Result<Vec<Pulse>> {
    grid.validate()?;
    let max_symbol = symbol_capacity(grid, palette)?;
    let fountain = RaptorQ::new(object, max_symbol)?;

    let header = PulseHeader {
        stream_id,
        config: fountain.config(),
        object_crc: crc(&[], object),
    };

    let capacity = grid.payload_bytes(palette);
    let mut buf = vec![0u8; capacity];

    fountain
        .symbols(overhead)
        .into_iter()
        .map(|symbol| {
            buf.iter_mut().for_each(|b| *b = 0);
            buf[HEADER_LEN..HEADER_LEN + symbol.len()].copy_from_slice(&symbol);
            header.write_into(&mut buf, &symbol);

            let mut pulse = Pulse::new(grid, palette)?;
            pulse.write_payload(&buf)?;
            Ok(pulse)
        })
        .collect()
}

/// What happened to an ingested pulse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingest {
    /// Absorbed; the object is not yet recoverable.
    Accepted,
    /// Absorbed, and the object came out. Further pulses are redundant.
    Completed,
    /// A symbol already held, or any pulse arriving after completion. Expected
    /// and harmless — the skin loops forever.
    Duplicate,
    /// Failed the CRC gate, or belongs to another stream. An erasure, never an
    /// error (§1b).
    Rejected,
}

/// Absorbs pulses until the object falls out.
pub struct Receiver {
    stream_id: Option<u32>,
    object_crc: u32,
    sink: Option<RaptorQSink>,
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
            object_crc: 0,
            sink: None,
            seen: HashSet::new(),
            accepted: 0,
            rejected: 0,
            object: None,
        }
    }

    pub fn ingest(&mut self, pulse: &Pulse) -> Ingest {
        if self.object.is_some() {
            return Ingest::Duplicate;
        }
        let bytes = pulse.read_payload();
        let symbol_len = self.sink.as_ref().map(|s| s.symbol_len());
        let Some((header, symbol)) = PulseHeader::parse(&bytes, symbol_len) else {
            self.rejected += 1;
            return Ingest::Rejected;
        };

        match self.stream_id {
            // Lock onto the first stream seen; ignore any other in view.
            Some(id) if id != header.stream_id => {
                self.rejected += 1;
                return Ingest::Rejected;
            }
            Some(_) => {}
            None => {
                let Ok(sink) = RaptorQSink::new(&header.config) else {
                    self.rejected += 1;
                    return Ingest::Rejected;
                };
                self.stream_id = Some(header.stream_id);
                self.object_crc = header.object_crc;
                self.sink = Some(sink);
            }
        }

        // A pulse that agrees on the stream but not on its contents is a
        // corrupt header that happened to pass CRC, or a restarted transfer.
        if header.object_crc != self.object_crc {
            self.rejected += 1;
            return Ingest::Rejected;
        }

        if symbol.len() >= SYMBOL_ID_LEN {
            let id: [u8; SYMBOL_ID_LEN] = symbol[..SYMBOL_ID_LEN].try_into().expect("checked len");
            if !self.seen.insert(id) {
                return Ingest::Duplicate;
            }
        }

        self.accepted += 1;
        let sink = self.sink.as_mut().expect("sink set above");
        match sink.absorb(symbol) {
            Some(object) => {
                self.object = Some(object);
                Ingest::Completed
            }
            None => Ingest::Accepted,
        }
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

    pub fn rejected(&self) -> u32 {
        self.rejected
    }

    pub fn is_complete(&self) -> bool {
        self.object.is_some()
    }

    /// The reconstructed object, CRC-verified. Fails loudly rather than
    /// returning unverified bytes (§3f).
    pub fn finish(&self) -> Result<Vec<u8>> {
        if self.stream_id.is_none() {
            return Err(Error::Empty);
        }
        let Some(object) = &self.object else {
            let (have, need) = self.progress();
            return Err(Error::NotConverged { have, need });
        };
        let got = crc(&[], object);
        if got != self.object_crc {
            return Err(Error::ObjectCrc {
                expected: self.object_crc,
                got,
            });
        }
        Ok(object.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const M1: (Grid, Palette) = (Grid::M1_MONO, Palette::Mono1);

    fn absorb_all(pulses: &[Pulse]) -> Receiver {
        let mut rx = Receiver::new();
        for pulse in pulses {
            rx.ingest(pulse);
        }
        rx
    }

    #[test]
    fn empty_object_roundtrips() {
        let pulses = encode(&[], M1.0, M1.1, 1, 0.0).unwrap();
        assert_eq!(absorb_all(&pulses).finish().unwrap(), Vec::<u8>::new());
    }

    /// The point of the whole fountain layer: an arbitrary subset suffices.
    #[test]
    fn decodes_after_sixty_percent_loss() {
        let object: Vec<u8> = (0..8192u32).map(|i| (i ^ (i >> 3)) as u8).collect();
        let pulses = encode(&object, M1.0, M1.1, 7, 2.0).unwrap();

        // Deterministic, unbiased-enough decimation: keep 2 of every 5.
        let kept: Vec<_> = pulses
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 5 < 2)
            .map(|(_, p)| p.clone())
            .collect();
        assert!(kept.len() * 5 <= pulses.len() * 2 + 5);

        let rx = absorb_all(&kept);
        assert_eq!(rx.finish().unwrap(), object);
    }

    /// With no repair symbols, losing one pulse must fail cleanly rather than
    /// return wrong bytes.
    #[test]
    fn zero_overhead_plus_loss_fails_loudly() {
        let object = vec![3u8; 2048];
        let pulses = encode(&object, M1.0, M1.1, 1, 0.0).unwrap();
        let rx = absorb_all(&pulses[1..]);
        assert!(matches!(rx.finish(), Err(Error::NotConverged { .. })));
    }

    #[test]
    fn duplicates_are_recognised_not_double_counted() {
        let pulses = encode(&[3u8; 300], M1.0, M1.1, 1, 0.0).unwrap();
        let mut rx = Receiver::new();
        assert_eq!(rx.ingest(&pulses[0]), Ingest::Accepted);
        assert_eq!(rx.ingest(&pulses[0]), Ingest::Duplicate);
        assert_eq!(rx.progress().0, 1);
    }

    /// The CRC gate: a single flipped payload cell must drop the pulse rather
    /// than feed a corrupt symbol to the fountain decoder (§1b).
    #[test]
    fn corrupted_pulse_is_rejected_by_the_crc_gate() {
        let pulses = encode(&[9u8; 2000], M1.0, M1.1, 1, 0.5).unwrap();
        let mut corrupt = pulses[0].clone();
        let (x, y) = M1.0.payload_coords().nth(300).unwrap();
        let flipped = corrupt.cell(x, y).unwrap() ^ 1;
        corrupt.set_cell(x, y, flipped).unwrap();

        let mut rx = Receiver::new();
        assert_eq!(rx.ingest(&corrupt), Ingest::Rejected);
        assert_eq!(rx.rejected(), 1);
    }

    /// Corruption must never reach the object. Flip one cell in every pulse and
    /// the transfer must fail to converge, not converge onto garbage.
    #[test]
    fn corruption_never_reaches_the_object() {
        let object = vec![0x5Au8; 4096];
        let pulses = encode(&object, M1.0, M1.1, 1, 1.0).unwrap();
        let mangled: Vec<Pulse> = pulses
            .iter()
            .map(|p| {
                let mut p = p.clone();
                let (x, y) = M1.0.payload_coords().nth(400).unwrap();
                let v = p.cell(x, y).unwrap() ^ 1;
                p.set_cell(x, y, v).unwrap();
                p
            })
            .collect();
        let rx = absorb_all(&mangled);
        assert!(!rx.is_complete());
        assert!(rx.finish().is_err());
    }

    #[test]
    fn a_foreign_stream_in_view_is_ignored() {
        let ours = encode(&[1u8; 2000], M1.0, M1.1, 111, 0.0).unwrap();
        let theirs = encode(&[2u8; 2000], M1.0, M1.1, 222, 0.0).unwrap();
        let mut rx = Receiver::new();
        assert_eq!(rx.ingest(&ours[0]), Ingest::Accepted);
        assert_eq!(rx.ingest(&theirs[0]), Ingest::Rejected);
    }

    proptest! {
        #[test]
        fn object_roundtrips(data in prop::collection::vec(any::<u8>(), 0..4096)) {
            let pulses = encode(&data, M1.0, M1.1, 1, 0.2).unwrap();
            prop_assert_eq!(absorb_all(&pulses).finish().unwrap(), data);
        }

        #[test]
        fn object_roundtrips_in_colour(data in prop::collection::vec(any::<u8>(), 0..8192)) {
            let pulses = encode(&data, Grid::M3_COLOR, Palette::Color3, 1, 0.2).unwrap();
            prop_assert_eq!(absorb_all(&pulses).finish().unwrap(), data);
        }
    }
}
