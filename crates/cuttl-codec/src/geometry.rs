//! Pulse grid geometry (`DESIGN.md` §3a).
//!
//! ```text
//! ┌────────────────────────────────────────────────────┐
//! │ ◰        [ BEACON: id·OTI·pulse#·geom ]         ◱  │  ← finder + beacon strip
//! ├────────────────────────────────────────────────────┤
//! │ ▓ ░ ▓ ░  band 0  ▣  sym ESI·payload·RS·CRC ▣ ░ ▓ │  ← timing track both edges
//! │ ▓ ░ ▓ ░  band 1     ...          · pilots ·  ░ ▓ │  ← ▣ = alignment lattice
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
    /// Blank ring on the inner sides of each finder.
    ///
    /// Not decoration. Finder detection works by scanning for the run-length
    /// ratio 1:1:3:1:1 across a finder's centre. Without a blank ring, payload
    /// cells of the same polarity as the finder's outer ring merge with it, the
    /// outermost run measures long, and the ratio test fails. QR calls this the
    /// separator and it is load-bearing for exactly this reason.
    Separator,
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
    /// Interior registration pattern — QR's alignment pattern, 5×5 concentric
    /// squares on a lattice through the data region.
    ///
    /// Four corner finders give a homography, which is *exact* for a planar
    /// target under a pinhole camera. These exist for the two cases where that
    /// model is not the one in force:
    ///
    /// - **Rolling-shutter skew.** The sensor reads row by row over 10–30 ms,
    ///   so a handheld camera is in a slightly different pose for each row. The
    ///   pulse is still planar, but the projection is no longer a single
    ///   homography — it is a different one per row, and fitting one set of
    ///   corners to all of them leaves a residual that grows toward the middle
    ///   of the frame.
    ///
    ///   Note this is *not* the tear case. A tear is two pulses joined at a
    ///   sensor row with the grid in the same place; it is a content problem
    ///   and the beacon already detects it. Skew is a geometry problem, it
    ///   happens on every handheld frame rather than only on straddled ones,
    ///   and nothing in the stack currently corrects it.
    /// - **Lens distortion.** Barrel distortion is not projective, so no
    ///   homography can absorb it. It bites hardest close in and at the edges,
    ///   which is exactly where a phone is held to fill the frame with a dense
    ///   grid.
    ///
    /// Deliberately **5** cells, not the finder's 7. An alignment pattern is
    /// found by local search near a position the homography already predicts,
    /// so it never needs the distinctive 1:1:3:1:1 run ratio that forced the
    /// finder to 7 — and at 5 it scans as 1:1:1:1:1, the pattern
    /// [`crate::eye`] explicitly rejects. An alignment pattern therefore cannot
    /// be mistaken for a finder, which would otherwise wreck the corner fit.
    Alignment,
    /// Carries actual data.
    Payload,
}

/// Side length of an interior alignment pattern, in cells. Fixed, not a knob:
/// see [`Region::Alignment`] for why it is 5 while the finder is 7.
pub const ALIGN_SIDE: u16 = 5;

/// A named grid + palette pairing.
///
/// Lives here rather than in each front end so the CLI, the browser skin and
/// the browser eye cannot drift apart about what "m1" means — which is the same
/// argument as the codec crate itself (§4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// 64×36 mono — the M1 air-gap profile.
    #[default]
    M1,
    /// 192×108 mono — density without colour.
    M2,
    /// 96×54 eight-colour — the M3 profile.
    M3,
    /// 192×108 eight-colour — both levers at once.
    M4,
}

impl Profile {
    /// Every profile, in ascending order of both bitrate and risk. The order is
    /// the ladder a bring-up should climb, and the order the eye tries them in.
    pub const ALL: [Profile; 4] = [Profile::M1, Profile::M2, Profile::M3, Profile::M4];

    pub const fn parts(self) -> (Grid, Palette) {
        match self {
            Profile::M1 => (Grid::M1_MONO, Palette::Mono1),
            Profile::M2 => (Grid::DENSE_MONO, Palette::Mono1),
            Profile::M3 => (Grid::M3_COLOR, Palette::Color3),
            Profile::M4 => (Grid::DENSE_COLOR, Palette::Color3),
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Profile::M1 => "m1",
            Profile::M2 => "m2",
            Profile::M3 => "m3",
            Profile::M4 => "m4",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        Profile::ALL.into_iter().find(|p| p.name() == name)
    }
}

/// Which of the two duplicated beacon strips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strip {
    Top,
    Bottom,
}

/// Cell grid layout for a pulse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub cols: u16,
    pub rows: u16,
    /// Height of each of the two beacon strips.
    pub beacon_rows: u16,
    /// Side length of each corner finder square.
    ///
    /// Must be odd, and in practice must be 7: the detector scans for the run
    /// ratio 1:1:3:1:1 through a finder's centre, and only a 7-wide concentric
    /// square produces it. A 5-wide one reads 1:1:1:1:1, which is not
    /// distinctive against random payload — that was the reason the M1 grid
    /// grew from 48×27, see [`Grid::M1_MONO`].
    pub finder: u16,
    /// Blank ring on the inner sides of each finder. See [`Region::Separator`].
    pub separator: u16,
    /// Width of each of the two vertical timing tracks.
    pub timing_cols: u16,
    /// A pilot every `pilot_period` cells in both axes → ~1-in-period² density.
    pub pilot_period: u16,
    /// Spacing of the interior alignment lattice, in cells. `0` disables them.
    ///
    /// QR v40 spaces its alignment patterns about 26 modules apart across 177;
    /// 32 across 192 is the same ballpark, reached the same way — close enough
    /// that the residual between two neighbours stays small, far enough that
    /// the 25 cells each one costs stay under 2% of the grid.
    ///
    /// Zero on the sparse profile on purpose. See [`Grid::M1_MONO`].
    pub align_period: u16,
    /// Horizontal stripes the data region is cut into, each an independent
    /// fountain symbol (§3a).
    ///
    /// The point is damage granularity: a rolling-shutter tear or a glare blob
    /// ruins the rows it touches, and with one symbol per pulse that costs the
    /// whole pulse. With bands it costs the bands it crossed.
    ///
    /// It is not free, and the cost is worst exactly where pulses are small:
    /// the inner code spends `ECC_LEN` bytes *per band*, so four bands on the
    /// M1 profile would burn 30% of the pulse on ECC alone. Hence 1 here.
    ///
    /// On the colour profile the right count turned out to be **2**, which is
    /// well below what §3a guessed. Measured bytes delivered per frame shown,
    /// under `Preset::Heavy`, averaged over four seeds:
    ///
    /// | bands | 1   | 2        | 3   | 4   | 5   | 7   |
    /// |-------|-----|----------|-----|-----|-----|-----|
    /// | B/frame | 952 | **1014** | 978 | 985 | 870 | 800 |
    ///
    /// The curve rises then falls, and by seven bands it is *worse than not
    /// banding at all*. Two reasons. Per-band ECC and framing are a fixed tax
    /// that scales with the band count. And tear is partly self-mitigating:
    /// bands below the tear line hold the *next* pulse's symbols, which are
    /// perfectly valid — a torn frame loses only the band straddling the tear,
    /// so extra granularity buys less than it appears to.
    ///
    /// The stronger case for bands is glare, which ruins a fixed *region* and
    /// is not self-mitigating. The channel does not model it yet, so that case
    /// is currently unmeasured — see `banding_beats_whole_pulse_symbols_under_tear`.
    pub bands: u8,
}

impl Grid {
    /// The M1 profile: 64×36 mono.
    ///
    /// Was 48×27 through M0 steps 1–3a, while nothing had to *find* the grid.
    /// Registration is a fixed cost — four 7×7 finders plus separators is ~324
    /// cells however big the grid is — and on a 48×27 frame that is a quarter
    /// of everything. Enlarging amortises it; the original 48 was arbitrary
    /// anyway, and a camera resolving 3–4 px/cell handles 64 columns trivially.
    pub const M1_MONO: Self = Self {
        cols: 64,
        rows: 36,
        beacon_rows: 3,
        finder: 7,
        separator: 1,
        timing_cols: 1,
        pilot_period: 8,
        // No interior alignment. The data region is 62x30, so a 32-cell lattice
        // fits one or two patterns -- too few to correct anything, while 25
        // cells is 1.1% of a grid that already spends 27% of itself on
        // registration. This profile buys margin by having large cells, which
        // is the same distortion budget alignment patterns would have bought.
        align_period: 0,
        // One band: at 211 B per pulse there is not enough room to pay for a
        // second copy of the inner code. See `bands`.
        bands: 1,
    };

    /// The M3 profile: 96×54, sized for the 8-colour palette (§5, M3).
    pub const M3_COLOR: Self = Self {
        cols: 96,
        rows: 54,
        beacon_rows: 3,
        finder: 7,
        separator: 1,
        timing_cols: 1,
        pilot_period: 8,
        // 28, not the dense profiles' 32: at 32 this grid fits a single row of
        // patterns, leaving nothing to interpolate against vertically. 28 gives
        // a 3x2 lattice for 2.9% of the cells; 24 would give 4x2 for 3.9%, and
        // the extra column is not worth a further point of payload.
        align_period: 28,
        // Two. Measured, not chosen: see `bands`.
        bands: 2,
    };

    /// 192×108, mono. The density lever pulled on its own.
    ///
    /// Registration is a *fixed* cost — four 7×7 finders and their separators
    /// are 256 cells whatever the grid is — so the payload fraction climbs with
    /// size: 73% at 64×36, 83% at 96×54, **91% here**. That is why a 9× cell
    /// count buys 15× the bytes. Small grids do not merely carry less, they
    /// spend proportionally more of themselves on saying where they are.
    ///
    /// The ceiling is not cells, it is **pixels per cell at the sensor**. The
    /// density sweep put the cliff between 3 and 2 px/cell — sampling error,
    /// not detection — and measured this grid at 4 px/cell locating 100% of
    /// frames with ~10 misread cells, comfortably inside the inner code. At
    /// 4 px/cell this is 768×432 on the sending screen, and the eye decodes at
    /// 960 px wide, so the budget closes. It has never been tried on glass.
    pub const DENSE_MONO: Self = Self {
        cols: 192,
        rows: 108,
        beacon_rows: 3,
        finder: 7,
        separator: 1,
        timing_cols: 1,
        pilot_period: 8,
        align_period: 32,
        // Affordable here in a way it is not at 211 B/pulse: two copies of the
        // inner code cost under 2% of this payload. See `bands`.
        bands: 2,
    };

    /// 192×108, eight colours — the M4 profile, both levers at once.
    ///
    /// ~7.1 KB per pulse, which at the measured 20 Hz optimum is the only
    /// configuration in this file that reaches three figures in KB/s. It also
    /// stacks the two least-proven things the project has: the density above
    /// and a colour palette that has never met a real camera's white balance.
    /// Climb the ladder, do not jump to it.
    pub const DENSE_COLOR: Self = Self {
        cols: 192,
        rows: 108,
        beacon_rows: 3,
        finder: 7,
        separator: 1,
        timing_cols: 1,
        pilot_period: 8,
        align_period: 32,
        bands: 2,
    };

    pub const fn cell_count(&self) -> u32 {
        self.cols as u32 * self.rows as u32
    }

    /// Classify a cell. Saturating arithmetic throughout so a nonsensical grid
    /// yields nonsense rather than a panic; use [`Grid::validate`] to reject it.
    ///
    /// Alignment patterns are resolved last and only ever displace a [`Pilot`]
    /// or a [`Payload`] cell, so adding them can never move a finder, a beacon
    /// or a timing track — the references the eye needs *before* it can predict
    /// where an alignment pattern should be.
    ///
    /// [`Pilot`]: Region::Pilot
    /// [`Payload`]: Region::Payload
    pub fn region(&self, x: u16, y: u16) -> Region {
        let base = self.base_region(x, y);
        if matches!(base, Region::Pilot | Region::Payload) && self.alignment_centre(x, y).is_some()
        {
            return Region::Alignment;
        }
        base
    }

    /// Everything except alignment patterns, which are layered on top.
    fn base_region(&self, x: u16, y: u16) -> Region {
        let zone = self.finder + self.separator;
        let in_zone_x = x < zone || x >= self.cols.saturating_sub(zone);
        let in_zone_y = y < zone || y >= self.rows.saturating_sub(zone);
        if in_zone_x && in_zone_y {
            let in_finder_x = x < self.finder || x >= self.cols.saturating_sub(self.finder);
            let in_finder_y = y < self.finder || y >= self.rows.saturating_sub(self.finder);
            return if in_finder_x && in_finder_y {
                Region::Finder
            } else {
                Region::Separator
            };
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

    /// Can a 5×5 pattern centred here sit entirely on data cells?
    ///
    /// An alignment pattern may overwrite pilots and payload and nothing else.
    /// Clipping one against a beacon strip or a finder separator would corrupt
    /// the reference it collided with, so such a lattice point is simply
    /// dropped — the eye interpolates across the gap, exactly as QR does where
    /// its lattice would foul a finder.
    fn alignment_fits(&self, cx: u16, cy: u16) -> bool {
        let half = ALIGN_SIDE / 2;
        if cx < half || cy < half {
            return false;
        }
        let (x0, y0) = (cx - half, cy - half);
        let (x1, y1) = (cx + half, cy + half);
        if x1 >= self.cols || y1 >= self.rows {
            return false;
        }
        if y0 < self.beacon_rows || y1 >= self.rows.saturating_sub(self.beacon_rows) {
            return false;
        }
        if x0 < self.timing_cols || x1 >= self.cols.saturating_sub(self.timing_cols) {
            return false;
        }
        let zone = self.finder + self.separator;
        let near_x = x0 < zone || x1 >= self.cols.saturating_sub(zone);
        let near_y = y0 < zone || y1 >= self.rows.saturating_sub(zone);
        !(near_x && near_y)
    }

    /// The alignment centre whose pattern covers this cell, if any.
    ///
    /// The lattice is anchored half a period into the data region rather than
    /// on its edge, which keeps the outermost patterns away from the beacon
    /// strips and the timing tracks without needing a special case.
    pub fn alignment_centre(&self, x: u16, y: u16) -> Option<(u16, u16)> {
        let period = self.align_period;
        if period == 0 || period < ALIGN_SIDE {
            return None;
        }
        let half = ALIGN_SIDE / 2;
        let phase = (period / 2) as i32;
        // Only two lattice points per axis can be within half a pattern of a
        // cell, so check the pair that brackets it rather than searching.
        let candidates = |v: u16, origin: u16| -> [i32; 2] {
            let rel = v as i32 - origin as i32 - phase;
            let k = rel.div_euclid(period as i32);
            [k, k + 1]
        };
        for kx in candidates(x, self.timing_cols) {
            for ky in candidates(y, self.beacon_rows) {
                let cx = self.timing_cols as i32 + phase + kx * period as i32;
                let cy = self.beacon_rows as i32 + phase + ky * period as i32;
                if cx < 0 || cy < 0 || cx > u16::MAX as i32 || cy > u16::MAX as i32 {
                    continue;
                }
                let (cx, cy) = (cx as u16, cy as u16);
                if x.abs_diff(cx) <= half && y.abs_diff(cy) <= half && self.alignment_fits(cx, cy) {
                    return Some((cx, cy));
                }
            }
        }
        None
    }

    /// Every alignment pattern centre, in raster order, in **cell space**.
    ///
    /// These are the eye's interior reference points: it predicts each one
    /// through the corner homography, finds where it actually landed, and
    /// corrects the difference (`DESIGN.md` §3b, the same interpolate-don't-fit
    /// argument the pilots use for colour).
    pub fn alignment_centres(&self) -> Vec<(u16, u16)> {
        let period = self.align_period;
        if period == 0 || period < ALIGN_SIDE {
            return Vec::new();
        }
        let phase = period / 2;
        let mut out = Vec::new();
        let mut cy = self.beacon_rows + phase;
        while cy < self.rows {
            let mut cx = self.timing_cols + phase;
            while cx < self.cols {
                if self.alignment_fits(cx, cy) {
                    out.push((cx, cy));
                }
                cx += period;
            }
            cy += period;
        }
        out
    }

    /// Centres of alignment patterns *and* finders together — every fixed point
    /// the eye can measure in a frame. Finders come first, in the order
    /// [`Grid::finder_centres`] uses.
    pub fn registration_centres(&self) -> Vec<(f64, f64)> {
        let mut out: Vec<(f64, f64)> = self.finder_centres().to_vec();
        out.extend(
            self.alignment_centres()
                .into_iter()
                .map(|(x, y)| (x as f64 + 0.5, y as f64 + 0.5)),
        );
        out
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

    /// Row range covered by one band, as evenly as the data region divides.
    ///
    /// Bands are *row-contiguous* on purpose. A tear is a horizontal line, so
    /// only a contiguous stripe can be the unit of damage; scattering a symbol
    /// across the frame would mean every tear damaged every symbol.
    pub fn band_rows(&self, band: u8) -> core::ops::Range<u16> {
        let start = self.beacon_rows;
        let end = self.rows.saturating_sub(self.beacon_rows);
        let total = end.saturating_sub(start);
        let count = self.bands.max(1) as u16;
        if band as u16 >= count {
            return start..start;
        }
        let (base, extra) = (total / count, total % count);
        let index = band as u16;
        let lo = start + index * base + index.min(extra);
        let hi = lo + base + u16::from(index < extra);
        lo..hi.min(end)
    }

    /// Payload cells of one band, in raster order.
    ///
    /// Concatenating every band in order reproduces [`Grid::payload_coords`]
    /// exactly, which is what lets the single-band case stay identical.
    pub fn band_payload_coords(&self, band: u8) -> impl Iterator<Item = (u16, u16)> {
        let g = *self;
        let rows = g.band_rows(band);
        rows.flat_map(move |y| (0..g.cols).map(move |x| (x, y)))
            .filter(move |&(x, y)| g.region(x, y) == Region::Payload)
    }

    pub fn band_payload_cells(&self, band: u8) -> u32 {
        self.band_payload_coords(band).count() as u32
    }

    /// Whole bytes one band carries, before framing.
    pub fn band_payload_bytes(&self, band: u8, palette: Palette) -> usize {
        ((self.band_payload_cells(band) * palette.bits_per_cell()) / 8) as usize
    }

    /// The smallest band. Symbol size has to fit the tightest one, because a
    /// fountain code needs every symbol the same length.
    pub fn min_band_payload_bytes(&self, palette: Palette) -> usize {
        (0..self.bands.max(1))
            .map(|band| self.band_payload_bytes(band, palette))
            .min()
            .unwrap_or(0)
    }

    /// Cells of one beacon strip, in raster order.
    ///
    /// The two strips carry the *same* bytes. That duplication is the whole
    /// rolling-shutter tear detector (§3a): if a frame is stitched from two
    /// different pulses, the counters disagree and the frame is thrown away.
    pub fn beacon_coords(&self, strip: Strip) -> impl Iterator<Item = (u16, u16)> {
        let g = *self;
        let rows: Vec<u16> = match strip {
            Strip::Top => (0..g.beacon_rows).collect(),
            Strip::Bottom => (g.rows.saturating_sub(g.beacon_rows)..g.rows).collect(),
        };
        rows.into_iter()
            .flat_map(move |y| (0..g.cols).map(move |x| (x, y)))
            .filter(move |&(x, y)| g.region(x, y) == Region::Beacon)
    }

    /// Centres of the four finders in **cell space**, ordered top-left,
    /// top-right, bottom-left, bottom-right.
    ///
    /// Cell `(i, j)` covers `[i, i+1) × [j, j+1)`, so a centre lands on a `.5`.
    /// These are the fixed points the eye matches its detected finder centres
    /// against to solve the homography (§3a) — the whole registration story
    /// reduces to four correspondences.
    pub fn finder_centres(&self) -> [(f64, f64); 4] {
        let half = (self.finder as f64 - 1.0) / 2.0 + 0.5;
        let (right, bottom) = (self.cols as f64 - half, self.rows as f64 - half);
        [(half, half), (right, half), (half, bottom), (right, bottom)]
    }

    pub fn validate(&self) -> Result<()> {
        if self.pilot_period == 0 {
            return Err(Error::ZeroPilotPeriod);
        }
        // Odd and at least 7, so the centre scan reads 1:1:3:1:1.
        if self.finder < 7 || self.finder.is_multiple_of(2) {
            return Err(Error::BadFinder {
                finder: self.finder,
            });
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
        for grid in Profile::ALL.map(|p| p.parts().0) {
            let mut counted = 0u32;
            for region in [
                Region::Finder,
                Region::Separator,
                Region::Beacon,
                Region::Timing,
                Region::Pilot,
                Region::Alignment,
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
        for grid in Profile::ALL.map(|p| p.parts().0) {
            let finders = (0..grid.rows)
                .flat_map(|y| (0..grid.cols).map(move |x| (x, y)))
                .filter(|&(x, y)| grid.region(x, y) == Region::Finder)
                .count();
            assert_eq!(finders, 4 * (grid.finder as usize).pow(2), "{grid:?}");
        }
    }

    #[test]
    fn payload_coords_agree_with_the_count() {
        for grid in Profile::ALL.map(|p| p.parts().0) {
            assert_eq!(grid.payload_coords().count() as u32, grid.payload_cells());
        }
    }

    /// Guards the throughput claims in DESIGN.md §5. Loose bounds on purpose —
    /// this catches an accounting regression, not a tuning change.
    #[test]
    fn profile_capacities_are_in_the_expected_ballpark() {
        let m1 = Grid::M1_MONO.payload_bytes(Palette::Mono1);
        assert!(
            (180..=260).contains(&m1),
            "M1 mono payload was {m1} B/pulse"
        );

        let m3 = Grid::M3_COLOR.payload_bytes(Palette::Color3);
        assert!(
            (1400..=1900).contains(&m3),
            "M3 colour payload was {m3} B/pulse"
        );
    }

    /// Bands must tile the data region exactly: every payload cell in one band,
    /// none in two. If this drifts, symbols silently overlap or lose bytes.
    #[test]
    fn bands_tile_the_payload_exactly() {
        for grid in Profile::ALL.map(|p| p.parts().0) {
            let mut seen: Vec<(u16, u16)> = (0..grid.bands)
                .flat_map(|band| grid.band_payload_coords(band))
                .collect();
            let mut whole: Vec<(u16, u16)> = grid.payload_coords().collect();
            assert_eq!(
                seen.len(),
                whole.len(),
                "{grid:?} band cells vs payload cells"
            );
            seen.sort_unstable();
            whole.sort_unstable();
            assert_eq!(seen, whole, "{grid:?} bands do not tile the payload");
        }
    }

    #[test]
    fn band_rows_are_contiguous_and_cover_the_data_region() {
        for grid in Profile::ALL.map(|p| p.parts().0) {
            let mut next = grid.beacon_rows;
            for band in 0..grid.bands {
                let rows = grid.band_rows(band);
                assert_eq!(rows.start, next, "{grid:?} band {band} is not contiguous");
                assert!(rows.end > rows.start, "{grid:?} band {band} is empty");
                next = rows.end;
            }
            assert_eq!(
                next,
                grid.rows - grid.beacon_rows,
                "{grid:?} bands stop short"
            );
        }
    }

    /// Bands differ in size because the finder zones eat into the top and
    /// bottom of the data region. Symbol sizing has to use the smallest, so
    /// keep an eye on how lopsided it is.
    #[test]
    fn band_sizes_stay_within_a_workable_spread() {
        let grid = Grid::M3_COLOR;
        let sizes: Vec<usize> = (0..grid.bands)
            .map(|b| grid.band_payload_bytes(b, Palette::Color3))
            .collect();
        let (min, max) = (*sizes.iter().min().unwrap(), *sizes.iter().max().unwrap());
        assert!(min > 0, "a band carries nothing: {sizes:?}");
        assert!(
            max <= min * 2,
            "bands too lopsided, smallest sets the symbol size: {sizes:?}"
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
