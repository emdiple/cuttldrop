//! Pulse grid geometry (`DESIGN.md` §3a).
//!
//! ```text
//! ┌────────────────────────────────────────────────────┐
//! │ ◰        [ BEACON: id·OTI·pulse#·geom ]         ◱  │  ← finder + beacon strip
//! ├────────────────────────────────────────────────────┤
//! │ ▓ ░ ▓ ░  band 0   sym ESI·payload·RS·CRC   ░ ▓ ░ ▓ │  ← timing track both edges
//! │ ▓ ░ ▓ ░  band 1   ...          · pilots ·  ░ ▓ ░ ▓ │
//! ├────────────────────────────────────────────────────┤
//! │ ◲        [ BEACON: id·OTI·pulse#·geom ]         ◳  │  ← repeated, for tear detect
//! └────────────────────────────────────────────────────┘
//! ```
//!
//! Bands are not modelled yet — they land at M2, per the agreed decision that
//! M1 uses whole-pulse symbols. The beacon strips *are* reserved from the
//! start so that cell accounting never shifts under us, but nothing is
//! written into them yet (see [`crate::pulse`]).

use crate::error::{Error, Result};
use crate::palette::Palette;

/// What a given cell is for. Every cell in a pulse belongs to exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Region {
    /// Corner registration pattern — concentric squares, QR-style. Deliberately
    /// boring geometry: HCCB died of alignment fragility, not of colour (§9).
    Finder,
    /// Top and bottom header strip. Duplicated so a pulse-counter mismatch
    /// between the two detects rolling-shutter tear (§3a).
    Beacon,
    /// Alternating track down both vertical edges; validates the homography
    /// and gives a cells-per-camera-pixel estimate for the distance hint.
    Timing,
    /// Known-value cell used to fit the colour transform. Distributed rather
    /// than gathered in a corner block, because glare is spatially varying —
    /// a deliberate divergence from JAB Code (§3b).
    Pilot,
    /// Carries actual data.
    Payload,
}

/// Cell grid layout for a pulse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub cols: u16,
    pub rows: u16,
    /// Height of each of the two beacon strips.
    pub beacon_rows: u16,
    /// Side length of each corner finder square. Odd values give a centre cell.
    pub finder: u16,
    /// Width of each of the two vertical timing tracks.
    pub timing_cols: u16,
    /// A pilot every `pilot_period` cells in both axes → ~1-in-period² density.
    pub pilot_period: u16,
}

impl Grid {
    /// The M1 profile: 48×27 mono. Pathetic bitrate on purpose — this is the
    /// grid that has to cross a real air gap first (§5, M1).
    pub const M1_MONO: Self = Self {
        cols: 48,
        rows: 27,
        beacon_rows: 3,
        finder: 5,
        timing_cols: 1,
        pilot_period: 8,
    };

    /// The M3 profile: 96×54, sized for the 8-colour palette (§5, M3).
    pub const M3_COLOR: Self = Self {
        cols: 96,
        rows: 54,
        beacon_rows: 3,
        finder: 7,
        timing_cols: 1,
        pilot_period: 8,
    };

    pub const fn cell_count(&self) -> u32 {
        self.cols as u32 * self.rows as u32
    }

    /// Classify a cell. Saturating arithmetic throughout so a nonsensical grid
    /// yields nonsense rather than a panic; use [`Grid::validate`] to reject it.
    pub fn region(&self, x: u16, y: u16) -> Region {
        let in_finder_x = x < self.finder || x >= self.cols.saturating_sub(self.finder);
        let in_finder_y = y < self.finder || y >= self.rows.saturating_sub(self.finder);
        if in_finder_x && in_finder_y {
            return Region::Finder;
        }
        if y < self.beacon_rows || y >= self.rows.saturating_sub(self.beacon_rows) {
            return Region::Beacon;
        }
        if x < self.timing_cols || x >= self.cols.saturating_sub(self.timing_cols) {
            return Region::Timing;
        }
        if self.pilot_period == 0 {
            return Region::Payload;
        }
        let dx = x - self.timing_cols;
        let dy = y - self.beacon_rows;
        if dx.is_multiple_of(self.pilot_period) && dy.is_multiple_of(self.pilot_period) {
            Region::Pilot
        } else {
            Region::Payload
        }
    }

    /// Payload cells in raster order. This ordering *is* the wire format — skin
    /// and eye must walk it identically, which is exactly why it lives in the
    /// shared codec crate rather than in either end (§4).
    pub fn payload_coords(&self) -> impl Iterator<Item = (u16, u16)> {
        let g = *self;
        (0..g.rows)
            .flat_map(move |y| (0..g.cols).map(move |x| (x, y)))
            .filter(move |&(x, y)| g.region(x, y) == Region::Payload)
    }

    pub fn payload_cells(&self) -> u32 {
        self.payload_coords().count() as u32
    }

    pub fn payload_bits(&self, palette: Palette) -> u32 {
        self.payload_cells() * palette.bits_per_cell()
    }

    /// Whole bytes carried by one pulse, before any framing overhead.
    pub fn payload_bytes(&self, palette: Palette) -> usize {
        (self.payload_bits(palette) / 8) as usize
    }

    pub fn validate(&self) -> Result<()> {
        if self.pilot_period == 0 {
            return Err(Error::ZeroPilotPeriod);
        }
        if self.payload_cells() == 0 {
            return Err(Error::GridTooSmall {
                cols: self.cols,
                rows: self.rows,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every cell must belong to exactly one region — no overlap, no gaps.
    /// If this fails, payload accounting is silently wrong somewhere.
    #[test]
    fn regions_partition_the_grid() {
        for grid in [Grid::M1_MONO, Grid::M3_COLOR] {
            let mut counted = 0u32;
            for region in [
                Region::Finder,
                Region::Beacon,
                Region::Timing,
                Region::Pilot,
                Region::Payload,
            ] {
                counted += (0..grid.rows)
                    .flat_map(|y| (0..grid.cols).map(move |x| (x, y)))
                    .filter(|&(x, y)| grid.region(x, y) == region)
                    .count() as u32;
            }
            assert_eq!(counted, grid.cell_count(), "{grid:?}");
        }
    }

    #[test]
    fn four_finders_of_the_declared_size() {
        for grid in [Grid::M1_MONO, Grid::M3_COLOR] {
            let finders = (0..grid.rows)
                .flat_map(|y| (0..grid.cols).map(move |x| (x, y)))
                .filter(|&(x, y)| grid.region(x, y) == Region::Finder)
                .count();
            assert_eq!(finders, 4 * (grid.finder as usize).pow(2), "{grid:?}");
        }
    }

    #[test]
    fn payload_coords_agree_with_the_count() {
        for grid in [Grid::M1_MONO, Grid::M3_COLOR] {
            assert_eq!(grid.payload_coords().count() as u32, grid.payload_cells());
        }
    }

    /// Guards the throughput claims in DESIGN.md §5. Loose bounds on purpose —
    /// this catches an accounting regression, not a tuning change.
    #[test]
    fn profile_capacities_are_in_the_expected_ballpark() {
        let m1 = Grid::M1_MONO.payload_bytes(Palette::Mono1);
        assert!((90..=140).contains(&m1), "M1 mono payload was {m1} B/pulse");

        let m3 = Grid::M3_COLOR.payload_bytes(Palette::Color3);
        assert!(
            (1400..=1900).contains(&m3),
            "M3 colour payload was {m3} B/pulse"
        );
    }

    #[test]
    fn degenerate_grids_are_rejected_not_panicked() {
        let tiny = Grid {
            cols: 4,
            rows: 4,
            ..Grid::M1_MONO
        };
        assert!(matches!(tiny.validate(), Err(Error::GridTooSmall { .. })));

        let no_pilots = Grid {
            pilot_period: 0,
            ..Grid::M1_MONO
        };
        assert_eq!(no_pilots.validate(), Err(Error::ZeroPilotPeriod));
    }
}
