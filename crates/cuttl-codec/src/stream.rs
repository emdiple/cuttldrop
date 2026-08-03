//! Object → pulses → object, over the fountain layer.
//!
//! A pulse carries one fountain symbol *per band*. There is no pulse index and
//! no total: a rateless stream has neither. The eye absorbs symbols in any
//! order, from any subset, and stops when the object falls out (§3c).
//!
//! ## Layout
//!
//! ```text
//! band 0:  MAGIC ver stream_id config object_crc │ symbol │ crc32
//! band 1:                                        │ symbol │ crc32
//! band n:                                        │ symbol │ crc32
//! ```
//!
//! Only band 0 carries the stream header, because those fields are identical in
//! every pulse of a stream — the eye needs them once and then never again. Every
//! band carries its own CRC, and that is what makes bands independent: damage
//! confined to one band's rows costs one symbol, not the pulse (§3a).
//!
//! Losing band 0 to a tear therefore costs a symbol but not the transfer; the
//! header arrives again on the next pulse, and the one after that.
//!
//! ## The CRC gate
//!
//! The most important check here is the per-band CRC. A fountain decoder assumes
//! every symbol it receives is correct or absent; one silently corrupt symbol
//! propagates through the XOR graph and poisons the whole object. The gate turns
//! an *error* into an *erasure*, the one thing the fountain layer can repair
//! (§1b). A rejected band is never an `Err` — it is routine.
//!
//! ## Order of operations
//!
//! ```text
//! skin:  header ‖ symbol  →  + inner ECC  →  band cells
//! eye:   band cells  →  inner correct  →  CRC gate  →  fountain
//! ```
//!
//! The inner code repairs sparse cell errors, the CRC gate converts whatever
//! survived into an erasure, and only then does anything reach the fountain —
//! which repairs erasures and nothing else.

use crate::beacon::{self, Beacon};
use crate::error::{Error, Result};
use crate::fec;
use crate::fountain::{CONFIG_LEN, Fountain, RaptorQ, RaptorQSink, SYMBOL_ID_LEN, Sink};
use crate::geometry::Grid;
use crate::palette::Palette;
use crate::pulse::Pulse;
use std::collections::HashSet;

const MAGIC: [u8; 2] = *b"CD";
const VERSION: u8 = 2;

/// Stream header bytes, carried by band 0 only.
pub const STREAM_HEADER_LEN: usize = 23;
/// Symbol id plus the trailing CRC, on every band.
const BAND_OVERHEAD: usize = SYMBOL_ID_LEN + 4;

/// Fields every pulse of a stream repeats verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamHeader {
    pub stream_id: u32,
    /// Fountain configuration; the RFC 6330 OTI for RaptorQ.
    pub config: [u8; CONFIG_LEN],
    /// CRC-32 of the whole object, checked after reconstruction. BLAKE3 replaces
    /// this when the manifest stream lands (§3c).
    pub object_crc: u32,
}

impl StreamHeader {
    fn write_into(&self, buf: &mut [u8]) {
        buf[0..2].copy_from_slice(&MAGIC);
        buf[2] = VERSION;
        buf[3..7].copy_from_slice(&self.stream_id.to_le_bytes());
        buf[7..19].copy_from_slice(&self.config);
        buf[19..23].copy_from_slice(&self.object_crc.to_le_bytes());
    }

    fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < STREAM_HEADER_LEN || bytes[0..2] != MAGIC || bytes[2] != VERSION {
            return None;
        }
        Some(Self {
            stream_id: u32::from_le_bytes(bytes[3..7].try_into().ok()?),
            config: bytes[7..19].try_into().ok()?,
            object_crc: u32::from_le_bytes(bytes[19..23].try_into().ok()?),
        })
    }
}

fn crc(bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

/// How one band's cells split between inner ECC and everything else.
pub fn band_layout(grid: Grid, palette: Palette, band: u8) -> Result<fec::Layout> {
    fec::Layout::for_capacity(grid.band_payload_bytes(band, palette))
}

/// Bytes reserved before the symbol in a given band.
const fn reserved_in(band: u8) -> usize {
    if band == 0 {
        STREAM_HEADER_LEN + BAND_OVERHEAD
    } else {
        BAND_OVERHEAD
    }
}

/// Largest fountain symbol every band can carry.
///
/// The *smallest* band sets this, because a fountain code needs one symbol size
/// for the whole object. Band 0 is usually the binding one: it pays for the
/// stream header on top of everything else.
pub fn symbol_capacity(grid: Grid, palette: Palette) -> Result<u16> {
    let mut smallest = usize::MAX;
    for band in 0..grid.bands.max(1) {
        let data = band_layout(grid, palette, band)?.data_len();
        let room = data
            .checked_sub(reserved_in(band))
            .filter(|&n| n > 0)
            .ok_or(Error::NoRoomForSymbol {
                capacity: data,
                header: reserved_in(band),
                symbol_id: SYMBOL_ID_LEN,
            })?;
        smallest = smallest.min(room);
    }
    Ok(smallest.min(u16::MAX as usize) as u16)
}

/// Frame one band: optional stream header, the symbol, then a CRC over both.
fn frame_band(band: u8, header: &StreamHeader, symbol: &[u8], data_len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; data_len];
    let mut at = 0;
    if band == 0 {
        header.write_into(&mut buf[..STREAM_HEADER_LEN]);
        at = STREAM_HEADER_LEN;
    }
    buf[at..at + symbol.len()].copy_from_slice(symbol);
    let end = at + symbol.len();
    let checksum = crc(&buf[..end]);
    buf[end..end + 4].copy_from_slice(&checksum.to_le_bytes());
    buf
}

/// Split a band's decoded bytes back into header and symbol, verifying the CRC.
///
/// `symbol_len` comes from the fountain config, not the wire, because RaptorQ
/// picks a symbol size that may be smaller than the space offered. `None` means
/// the band is unusable — an erasure, never an error.
fn parse_band(band: u8, bytes: &[u8], symbol_len: usize) -> Option<(Option<StreamHeader>, &[u8])> {
    let header = if band == 0 {
        Some(StreamHeader::parse(bytes)?)
    } else {
        None
    };
    let at = if band == 0 { STREAM_HEADER_LEN } else { 0 };
    let end = at
        + if symbol_len == 0 {
            0
        } else {
            SYMBOL_ID_LEN + symbol_len
        };

    let symbol = bytes.get(at..end)?;
    let expected = u32::from_le_bytes(bytes.get(end..end + 4)?.try_into().ok()?);
    if crc(&bytes[..end]) != expected {
        return None;
    }
    Some((header, symbol))
}

/// Encode an object as a batch of pulses, `grid.bands` symbols at a time.
///
/// `overhead` is the ratio of repair symbols to source symbols. It exists only
/// because a directory of PNGs has to stand in for a skin that loops forever;
/// at `0.0` the output is exactly the source symbols and survives no loss.
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

    let header = StreamHeader {
        stream_id,
        config: fountain.config(),
        object_crc: crc(object),
    };

    let bands = grid.bands.max(1) as usize;
    let symbols = fountain.symbols(overhead);
    let mut pulses = Vec::with_capacity(symbols.len().div_ceil(bands));

    for (index, chunk) in symbols.chunks(bands).enumerate() {
        let mut pulse = Pulse::new(grid, palette)?;
        for (band, symbol) in chunk.iter().enumerate() {
            let band = band as u8;
            let data_len = band_layout(grid, palette, band)?.data_len();
            let framed = frame_band(band, &header, symbol, data_len);
            pulse.write_band(
                band,
                &fec::encode(&framed, band_layout(grid, palette, band)?)?,
            )?;
        }
        // A short final chunk leaves its remaining bands blank; they simply fail
        // their CRC at the far end, which is an erasure like any other.
        beacon::write(
            &mut pulse,
            Beacon {
                stream_id: stream_id as u8,
                counter: index as u32,
            },
        )?;
        pulses.push(pulse);
    }
    Ok(pulses)
}

/// What a whole pulse amounted to. With bands, this is a summary: one frame can
/// contribute several symbols, and they need not all fare the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingest {
    /// At least one new symbol absorbed.
    Accepted,
    /// Absorbed, and the object came out.
    Completed,
    /// Every usable band was a symbol already held.
    Duplicate,
    /// No band survived its CRC.
    Rejected,
    /// Beacon strips disagreed and nothing could be salvaged.
    Torn,
}

/// Absorbs pulses until the object falls out.
pub struct Receiver {
    stream_id: Option<u32>,
    object_crc: u32,
    sink: Option<RaptorQSink>,
    seen: HashSet<[u8; SYMBOL_ID_LEN]>,
    accepted: u32,
    rejected: u32,
    torn: u32,
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
            torn: 0,
            object: None,
        }
    }

    pub fn ingest(&mut self, pulse: &Pulse) -> Ingest {
        if self.object.is_some() {
            return Ingest::Duplicate;
        }
        let grid = pulse.grid();
        let palette = pulse.palette();
        let bands = grid.bands.max(1);

        // A stitched frame is worth reporting either way, but with bands it is
        // no longer worth discarding: the tear ruins the bands it crossed and
        // leaves the rest perfectly readable. Only a single-band pulse has
        // nothing left to salvage.
        let torn = beacon::is_intact(pulse) == Some(false);
        if torn {
            self.torn += 1;
            if bands == 1 {
                return Ingest::Torn;
            }
        }

        let mut fresh = 0;
        let mut usable = 0;
        for band in 0..bands {
            let Ok(layout) = band_layout(grid, palette, band) else {
                continue;
            };
            let Some(bytes) = fec::decode(&pulse.read_band(band), layout) else {
                self.rejected += 1;
                continue;
            };

            // Until a config is locked, only band 0 can be interpreted: every
            // other band's symbol length is unknown.
            let symbol_len = match &self.sink {
                Some(sink) => sink.symbol_len(),
                None if band == 0 => match StreamHeader::parse(&bytes)
                    .and_then(|header| RaptorQSink::probe(&header.config))
                {
                    Some(len) => len,
                    None => {
                        self.rejected += 1;
                        continue;
                    }
                },
                None => continue,
            };

            let Some((header, symbol)) = parse_band(band, &bytes, symbol_len) else {
                self.rejected += 1;
                continue;
            };
            if let Some(header) = header
                && !self.adopt(header)
            {
                self.rejected += 1;
                continue;
            }
            usable += 1;

            if symbol.len() >= SYMBOL_ID_LEN {
                let id: [u8; SYMBOL_ID_LEN] =
                    symbol[..SYMBOL_ID_LEN].try_into().expect("checked length");
                if !self.seen.insert(id) {
                    continue;
                }
            }
            fresh += 1;
            self.accepted += 1;

            let Some(sink) = self.sink.as_mut() else {
                continue;
            };
            if let Some(object) = sink.absorb(symbol) {
                self.object = Some(object);
                return Ingest::Completed;
            }
        }

        match (fresh, usable, torn) {
            (0, 0, true) => Ingest::Torn,
            (0, 0, false) => Ingest::Rejected,
            (0, _, _) => Ingest::Duplicate,
            _ => Ingest::Accepted,
        }
    }

    /// Lock onto the first stream seen; reject anything that disagrees.
    fn adopt(&mut self, header: StreamHeader) -> bool {
        match self.stream_id {
            Some(id) => id == header.stream_id && self.object_crc == header.object_crc,
            None => {
                let Ok(sink) = RaptorQSink::new(&header.config) else {
                    return false;
                };
                self.stream_id = Some(header.stream_id);
                self.object_crc = header.object_crc;
                self.sink = Some(sink);
                true
            }
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

    /// Bands dropped by the CRC gate. Counts *bands*, not pulses.
    pub fn rejected(&self) -> u32 {
        self.rejected
    }

    /// Frames whose beacon strips disagreed. A rising count is the signal
    /// behind a `SLOW DOWN` hint to the human (§1e).
    pub fn torn(&self) -> u32 {
        self.torn
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
        let got = crc(object);
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
    const M3: (Grid, Palette) = (Grid::M3_COLOR, Palette::Color3);

    fn absorb_all(pulses: &[Pulse]) -> Receiver {
        let mut rx = Receiver::new();
        for pulse in pulses {
            rx.ingest(pulse);
        }
        rx
    }

    /// Flip `count` payload cells inside one band, spread so each lands in a
    /// different byte and costs the inner code a full correction.
    fn damage_band(pulse: &Pulse, band: u8, count: usize) -> Pulse {
        let mut pulse = pulse.clone();
        let coords: Vec<_> = pulse.grid().band_payload_coords(band).collect();
        for i in 0..count {
            let (x, y) = coords[(i * 8 + 24) % coords.len()];
            let flipped = pulse.cell(x, y).unwrap() ^ 1;
            pulse.set_cell(x, y, flipped).unwrap();
        }
        pulse
    }

    #[test]
    fn empty_object_roundtrips() {
        let pulses = encode(&[], M1.0, M1.1, 1, 0.0).unwrap();
        assert_eq!(absorb_all(&pulses).finish().unwrap(), Vec::<u8>::new());
    }

    /// The point of bands: damage one stripe and the others still deliver.
    #[test]
    fn damage_to_one_band_spares_the_others() {
        let (grid, palette) = M3;
        assert!(grid.bands > 1, "this test needs a banded profile");

        let object: Vec<u8> = (0..40_000u32).map(|i| (i ^ (i >> 4)) as u8).collect();
        // Overhead 3.0 because wrecking one of two bands throws away half the
        // symbols; the fountain has to make that up out of what is left.
        let pulses = encode(&object, grid, palette, 3, 3.0).unwrap();

        // Wreck the last band of every pulse, far past inner-code repair.
        // Note the budget is per *block* and a colour band spans several, so
        // this has to clear `correctable × blocks` — an earlier version of this
        // test landed exactly on that total and the band was quietly repaired.
        let victim = grid.bands - 1;
        let layout = band_layout(grid, palette, victim).unwrap();
        let ruin = layout.correctable_per_block() * layout.blocks() * 3;
        let mangled: Vec<Pulse> = pulses
            .iter()
            .map(|p| damage_band(p, victim, ruin))
            .collect();

        let rx = absorb_all(&mangled);
        assert_eq!(
            rx.finish().unwrap(),
            object,
            "one ruined band should not cost the transfer"
        );
        assert!(
            rx.rejected() > 0,
            "the ruined band should have been rejected"
        );
    }

    /// Losing band 0 costs its symbol and the stream header, but the header
    /// arrives again on the next pulse — so the transfer still completes.
    #[test]
    fn losing_the_header_band_is_survivable() {
        let (grid, palette) = M3;
        let object: Vec<u8> = (0..30_000u32).map(|i| (i * 11) as u8).collect();
        let pulses = encode(&object, grid, palette, 4, 2.0).unwrap();
        let budget = band_layout(grid, palette, 0)
            .unwrap()
            .correctable_per_block();

        // Every third pulse loses band 0 entirely.
        let mangled: Vec<Pulse> = pulses
            .iter()
            .enumerate()
            .map(|(i, p)| {
                if i % 3 == 0 {
                    damage_band(p, 0, budget * 4)
                } else {
                    p.clone()
                }
            })
            .collect();
        assert_eq!(absorb_all(&mangled).finish().unwrap(), object);
    }

    /// With no repair symbols, loss must fail cleanly rather than return wrong
    /// bytes.
    #[test]
    fn zero_overhead_plus_loss_fails_loudly() {
        let object = vec![3u8; 6000];
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

    /// A few cell errors are repaired by the inner code (§1b).
    #[test]
    fn a_few_cell_errors_are_repaired() {
        let pulses = encode(&[9u8; 2000], M1.0, M1.1, 1, 0.5).unwrap();
        let budget = band_layout(M1.0, M1.1, 0).unwrap().correctable_per_block();
        let mut rx = Receiver::new();
        assert_eq!(
            rx.ingest(&damage_band(&pulses[0], 0, budget)),
            Ingest::Accepted
        );
        assert_eq!(rx.rejected(), 0);
    }

    /// Past that budget the CRC gate takes over.
    #[test]
    fn errors_past_the_inner_budget_hit_the_crc_gate() {
        let pulses = encode(&[9u8; 2000], M1.0, M1.1, 1, 0.5).unwrap();
        let budget = band_layout(M1.0, M1.1, 0).unwrap().correctable_per_block();
        let mut rx = Receiver::new();
        assert_eq!(
            rx.ingest(&damage_band(&pulses[0], 0, budget * 4)),
            Ingest::Rejected
        );
        assert_eq!(rx.rejected(), 1);
    }

    /// Corruption must never reach the object.
    #[test]
    fn corruption_never_reaches_the_object() {
        let object = vec![0x5Au8; 4000];
        let budget = band_layout(M1.0, M1.1, 0).unwrap().correctable_per_block();
        let pulses = encode(&object, M1.0, M1.1, 1, 1.0).unwrap();
        let mangled: Vec<Pulse> = pulses
            .iter()
            .map(|p| damage_band(p, 0, budget * 4))
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
        fn object_roundtrips_in_colour(data in prop::collection::vec(any::<u8>(), 0..20000)) {
            let pulses = encode(&data, M3.0, M3.1, 1, 0.2).unwrap();
            prop_assert_eq!(absorb_all(&pulses).finish().unwrap(), data);
        }
    }
}
