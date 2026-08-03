//! The inner error-correcting code (`DESIGN.md` §1b).
//!
//! This is the layer the fountain cannot replace. RaptorQ repairs **erasures** —
//! whole pulses that never arrived or were rejected. It is helpless against
//! **errors**: a handful of misread cells inside an otherwise good pulse. Left
//! alone, one bad cell out of ~900 fails the CRC and costs the entire symbol,
//! and no amount of fountain overhead helps, because the loss rate is 100%.
//!
//! Reed–Solomon over GF(2⁸) mops those up so the CRC gate above only sees
//! pulses that are either genuinely fine or genuinely beyond repair.
//!
//! ## Crate note
//!
//! An earlier draft recorded `reed-solomon-32` as the provisional choice. That
//! was wrong: despite the name it works in **GF(2⁵)** with 31-*symbol* blocks
//! and 5-bit symbols, so using it would mean repacking every byte and running
//! ~90 blocks for one colour pulse. `reed-solomon` 0.2 is the GF(2⁸) original
//! it was forked from — 255-byte blocks, byte symbols, no_std.
//!
//! ## Interleaving
//!
//! Blur makes cell errors *spatially* clustered, and neighbouring cells are
//! neighbouring bytes in the coded stream, so errors arrive in bursts. Coded
//! bytes are therefore interleaved across blocks: consecutive bytes on the wire
//! belong to different blocks, so a burst is spread thin instead of exhausting
//! one block's correction budget. With a single block (the M1 profile) this is
//! a no-op; it matters for the multi-block colour profile.

use crate::error::{Error, Result};
use reed_solomon::{Decoder, Encoder};

/// ECC bytes per block. Corrects `ECC_LEN / 2` byte errors per block.
///
/// Sized against measurement, not taste — see `cuttl-sim`'s channel presets for
/// the error rates this has to absorb.
pub const ECC_LEN: usize = 16;

/// GF(2⁸) Reed–Solomon blocks cannot exceed 255 bytes including ECC. The crate
/// asserts on this internally, so [`Layout`] must never produce a longer block.
const MAX_BLOCK: usize = 255;

/// How a pulse's byte capacity is divided into Reed–Solomon blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    blocks: usize,
    /// Bytes per block, ECC included.
    block_len: usize,
    ecc_len: usize,
}

impl Layout {
    pub fn new(capacity: usize, ecc_len: usize) -> Result<Self> {
        let blocks = capacity.div_ceil(MAX_BLOCK).max(1);
        let block_len = capacity / blocks;
        if ecc_len == 0 || block_len <= ecc_len {
            return Err(Error::NoRoomForEcc { capacity, ecc_len });
        }
        Ok(Self {
            blocks,
            block_len,
            ecc_len,
        })
    }

    pub fn for_capacity(capacity: usize) -> Result<Self> {
        Self::new(capacity, ECC_LEN)
    }

    pub fn blocks(&self) -> usize {
        self.blocks
    }

    pub fn ecc_len(&self) -> usize {
        self.ecc_len
    }

    /// Usable bytes after ECC.
    pub fn data_len(&self) -> usize {
        self.blocks * (self.block_len - self.ecc_len)
    }

    /// Bytes actually written to cells. Up to `blocks - 1` bytes of the pulse go
    /// unused so every block is the same length, which keeps interleaving exact.
    pub fn coded_len(&self) -> usize {
        self.blocks * self.block_len
    }

    /// Byte errors correctable per block.
    pub fn correctable_per_block(&self) -> usize {
        self.ecc_len / 2
    }
}

/// Add ECC and interleave. Output is exactly [`Layout::coded_len`] bytes; short
/// input is zero-padded.
pub fn encode(data: &[u8], layout: Layout) -> Result<Vec<u8>> {
    let capacity = layout.data_len();
    if data.len() > capacity {
        return Err(Error::PayloadTooLarge {
            len: data.len(),
            capacity,
        });
    }
    let per_block = layout.block_len - layout.ecc_len;
    let encoder = Encoder::new(layout.ecc_len);
    let mut out = vec![0u8; layout.coded_len()];
    let mut block = vec![0u8; per_block];

    for b in 0..layout.blocks {
        let start = b * per_block;
        let end = (start + per_block).min(data.len());
        block.iter_mut().for_each(|byte| *byte = 0);
        if start < end {
            block[..end - start].copy_from_slice(&data[start..end]);
        }
        let coded = encoder.encode(&block);
        for (i, byte) in coded.iter().enumerate().take(layout.block_len) {
            out[i * layout.blocks + b] = *byte;
        }
    }
    Ok(out)
}

/// De-interleave and correct. `None` means at least one block was beyond
/// repair — which the caller must treat as an erasure, never an error (§1b).
pub fn decode(coded: &[u8], layout: Layout) -> Option<Vec<u8>> {
    if coded.len() < layout.coded_len() {
        return None;
    }
    let decoder = Decoder::new(layout.ecc_len);
    let mut data = Vec::with_capacity(layout.data_len());
    let mut block = vec![0u8; layout.block_len];

    for b in 0..layout.blocks {
        for (i, byte) in block.iter_mut().enumerate() {
            *byte = coded[i * layout.blocks + b];
        }
        let corrected = decoder.correct(&block, None).ok()?;
        data.extend_from_slice(corrected.data());
    }
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two live profiles: M1 mono (114 B) and M3 colour (1629 B).
    const CAPACITIES: [usize; 2] = [114, 1629];

    #[test]
    fn layouts_never_exceed_the_crate_block_limit() {
        // The crate asserts internally rather than returning an error, so an
        // over-long block is a panic, not a test failure. Keep this honest.
        for capacity in [1usize, 64, 114, 255, 256, 1629, 8192, 65_000] {
            if let Ok(layout) = Layout::for_capacity(capacity) {
                assert!(layout.coded_len() / layout.blocks() <= MAX_BLOCK);
                assert!(layout.coded_len() <= capacity);
                assert!(layout.data_len() < layout.coded_len());
            }
        }
    }

    #[test]
    fn tiny_capacities_are_rejected_not_panicked() {
        assert!(Layout::for_capacity(ECC_LEN).is_err());
        assert!(Layout::new(100, 0).is_err());
    }

    #[test]
    fn roundtrips_without_errors() {
        for capacity in CAPACITIES {
            let layout = Layout::for_capacity(capacity).unwrap();
            let data: Vec<u8> = (0..layout.data_len()).map(|i| (i * 7) as u8).collect();
            let coded = encode(&data, layout).unwrap();
            assert_eq!(coded.len(), layout.coded_len());
            assert_eq!(decode(&coded, layout).unwrap(), data);
        }
    }

    /// The whole point: corrupt bytes must come back correct.
    #[test]
    fn corrects_up_to_the_budget() {
        for capacity in CAPACITIES {
            let layout = Layout::for_capacity(capacity).unwrap();
            let budget = layout.correctable_per_block() * layout.blocks();
            let data: Vec<u8> = (0..layout.data_len()).map(|i| (i * 13) as u8).collect();
            let mut coded = encode(&data, layout).unwrap();

            // Interleaving means a contiguous run spreads across blocks, so a
            // burst of `budget` bytes is exactly what the code should absorb.
            for byte in coded.iter_mut().take(budget) {
                *byte ^= 0xFF;
            }
            assert_eq!(
                decode(&coded, layout).unwrap(),
                data,
                "capacity {capacity} failed to correct a burst of {budget}"
            );
        }
    }

    /// Past the budget it must fail, not silently return wrong bytes. A wrong
    /// answer here would sail through to the fountain decoder and poison the
    /// object, which is exactly what the CRC gate above exists to prevent.
    #[test]
    fn beyond_the_budget_it_fails_rather_than_lying() {
        let layout = Layout::for_capacity(114).unwrap();
        let data: Vec<u8> = (0..layout.data_len()).map(|i| (i * 3) as u8).collect();
        let mut coded = encode(&data, layout).unwrap();
        for byte in coded.iter_mut().take(layout.correctable_per_block() * 3) {
            *byte ^= 0xA5;
        }
        match decode(&coded, layout) {
            None => {}
            Some(out) => assert_ne!(out, data, "corruption was silently accepted as correct"),
        }
    }

    #[test]
    fn interleaving_spreads_bursts_across_blocks() {
        let layout = Layout::for_capacity(1629).unwrap();
        assert!(
            layout.blocks() > 1,
            "need a multi-block profile to test this"
        );

        let data: Vec<u8> = (0..layout.data_len()).map(|i| (i * 29) as u8).collect();
        let mut coded = encode(&data, layout).unwrap();

        // A burst as long as one whole block. Without interleaving this would
        // destroy that block outright; with it, each block loses a few bytes.
        for byte in coded.iter_mut().take(layout.blocks() * 4) {
            *byte ^= 0x5A;
        }
        assert_eq!(decode(&coded, layout).unwrap(), data);
    }
}
