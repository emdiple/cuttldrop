//! The pulse header carried in the reserved strips (`DESIGN.md` §3a).
//!
//! Four bytes, repeated an odd number of times and majority-voted, painted into
//! the top strip and again into the bottom. It is the only field the eye can
//! read with no prior knowledge at all: one bit per cell, no palette alphabet,
//! no colour calibration, no CRC gate, no fountain config.
//!
//! ## Why only four bytes
//!
//! §1d originally put the RaptorQ config and grid geometry here too, ~16 bytes.
//! The M0 arithmetic killed that: a strip is 3 rows minus the finder zones, so
//! ~144 cells on the M1 grid, and heavy repetition leaves room for 4 bytes.
//! Everything else moved into the payload where the fountain protects it. What
//! stayed is exactly what must be readable *before* anything else works.
//!
//! ## What the duplication buys
//!
//! A rolling-shutter capture stitches the top of one pulse to the bottom of the
//! next. Duplicating the counter turns that into a visible disagreement.
//!
//! It is worth being precise about what this is *for*, because it is not
//! correctness: a stitched frame's payload matches neither pulse and fails the
//! CRC gate anyway. The beacon earns its place by being **cheap and early** —
//! it rejects a torn frame before Reed–Solomon and RaptorQ do any work — and by
//! being **diagnosable**: "your frames are tearing" is exactly the kind of hint
//! the human back channel needs to act on (§1e), and a CRC failure cannot tell
//! you that.

use crate::error::Result;
use crate::geometry::Strip;
use crate::pulse::Pulse;

/// Payload bytes: stream id, then a 24-bit pulse counter.
pub const BEACON_LEN: usize = 4;
const BEACON_BITS: usize = BEACON_LEN * 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Beacon {
    pub stream_id: u8,
    /// Wraps at 2^24. Only *differences* matter, so wrapping is harmless.
    pub counter: u32,
}

impl Beacon {
    fn to_bytes(self) -> [u8; BEACON_LEN] {
        let c = self.counter.to_le_bytes();
        [self.stream_id, c[0], c[1], c[2]]
    }

    fn from_bytes(bytes: [u8; BEACON_LEN]) -> Self {
        Self {
            stream_id: bytes[0],
            counter: u32::from_le_bytes([bytes[1], bytes[2], bytes[3], 0]),
        }
    }
}

/// How many whole copies of the beacon fit in one strip.
///
/// Forced odd so a majority vote can never tie. Repetition rather than a real
/// code because the field is tiny and the decoder must be trivial — this runs
/// before we know anything about the stream.
pub fn repetitions(pulse: &Pulse) -> usize {
    let cells = pulse.grid().beacon_coords(Strip::Top).count();
    let whole = cells / BEACON_BITS;
    if whole.is_multiple_of(2) {
        whole.saturating_sub(1)
    } else {
        whole
    }
}

/// Paint the beacon into both strips.
pub fn write(pulse: &mut Pulse, beacon: Beacon) -> Result<()> {
    let copies = repetitions(pulse);
    if copies == 0 {
        return Ok(());
    }
    let bytes = beacon.to_bytes();
    let palette = pulse.palette();
    let grid = pulse.grid();

    for strip in [Strip::Top, Strip::Bottom] {
        for (index, (x, y)) in grid.beacon_coords(strip).enumerate() {
            let bit = index % BEACON_BITS;
            let value = if index / BEACON_BITS < copies {
                palette.bit_value(bytes[bit / 8] >> (bit % 8) & 1 == 1)
            } else {
                0 // slack beyond the last whole copy
            };
            pulse.set_cell(x, y, value)?;
        }
    }
    Ok(())
}

/// Recover one strip's beacon by majority vote. `None` if the strip is too
/// small to hold even one copy.
pub fn read(pulse: &Pulse, strip: Strip) -> Option<Beacon> {
    let copies = repetitions(pulse);
    if copies == 0 {
        return None;
    }
    let palette = pulse.palette();
    let mut votes = [0i32; BEACON_BITS];

    for (index, (x, y)) in pulse.grid().beacon_coords(strip).enumerate() {
        if index / BEACON_BITS >= copies {
            break;
        }
        let value = pulse.cell(x, y).ok()?;
        votes[index % BEACON_BITS] += if palette.to_bit(value) { 1 } else { -1 };
    }

    let mut bytes = [0u8; BEACON_LEN];
    for (bit, &vote) in votes.iter().enumerate() {
        if vote > 0 {
            bytes[bit / 8] |= 1 << (bit % 8);
        }
    }
    Some(Beacon::from_bytes(bytes))
}

/// Whether the two strips agree — i.e. the frame came from a single pulse.
///
/// A disagreement means rolling-shutter tear. `None` when either strip is
/// unreadable, which the caller should also treat as unusable.
pub fn is_intact(pulse: &Pulse) -> Option<bool> {
    let top = read(pulse, Strip::Top)?;
    let bottom = read(pulse, Strip::Bottom)?;
    Some(top == bottom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Grid;
    use crate::palette::Palette;

    const PROFILES: [(Grid, Palette); 2] = [
        (Grid::M1_MONO, Palette::Mono1),
        (Grid::M3_COLOR, Palette::Color3),
    ];

    fn stamped(grid: Grid, palette: Palette, beacon: Beacon) -> Pulse {
        let mut pulse = Pulse::new(grid, palette).unwrap();
        write(&mut pulse, beacon).unwrap();
        pulse
    }

    #[test]
    fn every_profile_fits_at_least_three_copies() {
        for (grid, palette) in PROFILES {
            let pulse = Pulse::new(grid, palette).unwrap();
            let copies = repetitions(&pulse);
            assert!(copies >= 3, "{grid:?} fits only {copies} beacon copies");
            assert!(
                !copies.is_multiple_of(2),
                "{copies} copies would allow a tied vote"
            );
        }
    }

    #[test]
    fn roundtrips_in_both_strips() {
        let beacon = Beacon {
            stream_id: 0xA7,
            counter: 0x0B_CD_EF,
        };
        for (grid, palette) in PROFILES {
            let pulse = stamped(grid, palette, beacon);
            assert_eq!(read(&pulse, Strip::Top), Some(beacon), "{grid:?} top");
            assert_eq!(read(&pulse, Strip::Bottom), Some(beacon), "{grid:?} bottom");
            assert_eq!(is_intact(&pulse), Some(true));
        }
    }

    #[test]
    fn writing_the_beacon_leaves_the_payload_alone() {
        let grid = Grid::M1_MONO;
        let mut pulse = Pulse::new(grid, Palette::Mono1).unwrap();
        pulse.write_payload(&vec![0x5A; pulse.capacity()]).unwrap();
        let before = pulse.read_payload();

        write(
            &mut pulse,
            Beacon {
                stream_id: 3,
                counter: 9,
            },
        )
        .unwrap();
        assert_eq!(pulse.read_payload(), before);
    }

    /// Majority vote must ride out damage up to half the copies.
    #[test]
    fn survives_a_damaged_copy() {
        let beacon = Beacon {
            stream_id: 1,
            counter: 77,
        };
        let grid = Grid::M1_MONO;
        let mut pulse = stamped(grid, Palette::Mono1, beacon);

        // Wipe the whole first copy in the top strip.
        let victims: Vec<_> = grid.beacon_coords(Strip::Top).take(BEACON_BITS).collect();
        for (x, y) in victims {
            pulse.set_cell(x, y, 0).unwrap();
        }
        assert_eq!(read(&pulse, Strip::Top), Some(beacon));
    }

    /// The tear detector: strips carrying different counters must disagree.
    #[test]
    fn mismatched_strips_read_as_torn() {
        let grid = Grid::M1_MONO;
        let mut pulse = stamped(
            grid,
            Palette::Mono1,
            Beacon {
                stream_id: 1,
                counter: 100,
            },
        );

        // Repaint only the bottom strip, as a rolling-shutter tear would.
        let later = Beacon {
            stream_id: 1,
            counter: 101,
        };
        let bytes = [later.stream_id, 101, 0, 0];
        let copies = repetitions(&pulse);
        let coords: Vec<_> = grid.beacon_coords(Strip::Bottom).collect();
        for (index, (x, y)) in coords.into_iter().enumerate() {
            if index / BEACON_BITS >= copies {
                break;
            }
            let bit = index % BEACON_BITS;
            let on = bytes[bit / 8] >> (bit % 8) & 1 == 1;
            pulse.set_cell(x, y, Palette::Mono1.bit_value(on)).unwrap();
        }

        assert_eq!(read(&pulse, Strip::Bottom), Some(later));
        assert_eq!(is_intact(&pulse), Some(false));
    }
}
