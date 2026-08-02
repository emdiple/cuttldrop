//! One rendered frame: a grid of chroma cells (`DESIGN.md` §3a).
//!
//! A [`Pulse`] holds *cell values*, not pixels. Turning cells into pixels is a
//! presentation concern and belongs to whoever is doing the rendering — the
//! canvas in the browser, or `cuttl-sim` offline. Keeping pixels out of here is
//! what lets this crate compile to `wasm32` without an image stack (§4).

use crate::error::{Error, Result};
use crate::geometry::{Grid, Region};
use crate::palette::Palette;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pulse {
    grid: Grid,
    palette: Palette,
    cells: Vec<u8>,
}

impl Pulse {
    /// A pulse with all structural regions painted and an empty payload.
    pub fn new(grid: Grid, palette: Palette) -> Result<Self> {
        grid.validate()?;
        let mut pulse = Self {
            grid,
            palette,
            cells: vec![0; grid.cell_count() as usize],
        };
        pulse.paint_structure();
        Ok(pulse)
    }

    pub fn grid(&self) -> Grid {
        self.grid
    }

    pub fn palette(&self) -> Palette {
        self.palette
    }

    pub fn cells(&self) -> &[u8] {
        &self.cells
    }

    fn index(&self, x: u16, y: u16) -> usize {
        y as usize * self.grid.cols as usize + x as usize
    }

    pub fn cell(&self, x: u16, y: u16) -> Result<u8> {
        self.bounds_check(x, y)?;
        Ok(self.cells[self.index(x, y)])
    }

    pub fn set_cell(&mut self, x: u16, y: u16, value: u8) -> Result<()> {
        self.bounds_check(x, y)?;
        if value >= self.palette.levels() {
            return Err(Error::CellValue {
                value,
                levels: self.palette.levels(),
            });
        }
        let i = self.index(x, y);
        self.cells[i] = value;
        Ok(())
    }

    fn bounds_check(&self, x: u16, y: u16) -> Result<()> {
        if x >= self.grid.cols || y >= self.grid.rows {
            return Err(Error::CellOutOfBounds {
                x,
                y,
                cols: self.grid.cols,
                rows: self.grid.rows,
            });
        }
        Ok(())
    }

    /// sRGB for a cell, ready to blit.
    pub fn rgb(&self, x: u16, y: u16) -> Result<[u8; 3]> {
        Ok(self.palette.to_rgb(self.cell(x, y)?))
    }

    /// Paint finders, timing tracks and pilots. All deterministic and known to
    /// the eye, which is what makes them usable as references.
    ///
    /// The beacon strips are left blank: the beacon carries a pulse counter and
    /// stream id, which only matter once frames can be lost or torn. Until then
    /// the framing header rides in the payload — see [`crate::stream`].
    fn paint_structure(&mut self) {
        let g = self.grid;
        let on = self.palette.max_value();

        for y in 0..g.rows {
            for x in 0..g.cols {
                let value = match g.region(x, y) {
                    Region::Finder => {
                        // Concentric squares: outer ring on, next ring off,
                        // solid core. At finder = 7 this is exactly QR's.
                        let half = (g.finder - 1) / 2;
                        let lx = if x < g.finder { x } else { g.cols - 1 - x };
                        let ly = if y < g.finder { y } else { g.rows - 1 - y };
                        let ring = (lx as i32 - half as i32)
                            .abs()
                            .max((ly as i32 - half as i32).abs())
                            as u16;
                        if ring == half || ring + 2 <= half {
                            on
                        } else {
                            0
                        }
                    }
                    // Alternating along the run, giving a scale reference.
                    Region::Timing => {
                        if y % 2 == 0 {
                            on
                        } else {
                            0
                        }
                    }
                    // Cycle the full gamut across the frame so every palette
                    // level is observed somewhere in every pulse (§3b).
                    Region::Pilot => {
                        let px = (x - g.timing_cols) / g.pilot_period;
                        let py = (y - g.beacon_rows) / g.pilot_period;
                        ((px + py) % self.palette.levels() as u16) as u8
                    }
                    Region::Beacon | Region::Payload => 0,
                };
                let i = self.index(x, y);
                self.cells[i] = value;
            }
        }
    }

    /// Bytes one pulse can carry, before framing.
    pub fn capacity(&self) -> usize {
        self.grid.payload_bytes(self.palette)
    }

    /// Pack bytes into the payload cells, LSB first. Short input is zero-filled.
    pub fn write_payload(&mut self, bytes: &[u8]) -> Result<()> {
        let capacity = self.capacity();
        if bytes.len() > capacity {
            return Err(Error::PayloadTooLarge {
                len: bytes.len(),
                capacity,
            });
        }
        let bits = self.palette.bits_per_cell();
        let grid = self.grid;
        let mut bit = 0usize;
        for (x, y) in grid.payload_coords() {
            let mut value = 0u8;
            for k in 0..bits {
                let i = bit + k as usize;
                let set = i / 8 < bytes.len() && (bytes[i / 8] >> (i % 8)) & 1 == 1;
                if set {
                    value |= 1 << k;
                }
            }
            let idx = self.index(x, y);
            self.cells[idx] = value;
            bit += bits as usize;
        }
        Ok(())
    }

    /// Unpack the payload cells. Always returns [`Pulse::capacity`] bytes; the
    /// framing layer decides how many are meaningful.
    pub fn read_payload(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.capacity()];
        let bits = self.palette.bits_per_cell();
        let mut bit = 0usize;
        for (x, y) in self.grid.payload_coords() {
            let value = self.cells[self.index(x, y)];
            for k in 0..bits {
                let i = bit + k as usize;
                if i / 8 < out.len() && (value >> k) & 1 == 1 {
                    out[i / 8] |= 1 << (i % 8);
                }
            }
            bit += bits as usize;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn structural_cells_are_within_palette_range() {
        for (grid, palette) in [
            (Grid::M1_MONO, Palette::Mono1),
            (Grid::M3_COLOR, Palette::Color3),
        ] {
            let pulse = Pulse::new(grid, palette).unwrap();
            assert!(pulse.cells().iter().all(|&v| v < palette.levels()));
        }
    }

    /// Writing the payload must not disturb finders, timing or pilots — those
    /// are the eye's only references.
    #[test]
    fn payload_write_leaves_structure_untouched() {
        let grid = Grid::M1_MONO;
        let blank = Pulse::new(grid, Palette::Mono1).unwrap();
        let mut written = blank.clone();
        written
            .write_payload(&vec![0xA5; blank.capacity()])
            .unwrap();

        for y in 0..grid.rows {
            for x in 0..grid.cols {
                if grid.region(x, y) != Region::Payload {
                    assert_eq!(
                        blank.cell(x, y).unwrap(),
                        written.cell(x, y).unwrap(),
                        "structural cell ({x}, {y}) was overwritten"
                    );
                }
            }
        }
    }

    #[test]
    fn oversized_payload_is_rejected() {
        let mut pulse = Pulse::new(Grid::M1_MONO, Palette::Mono1).unwrap();
        let too_much = vec![0u8; pulse.capacity() + 1];
        assert!(matches!(
            pulse.write_payload(&too_much),
            Err(Error::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn out_of_palette_cell_value_is_rejected() {
        let mut pulse = Pulse::new(Grid::M1_MONO, Palette::Mono1).unwrap();
        assert!(matches!(
            pulse.set_cell(10, 10, 2),
            Err(Error::CellValue { .. })
        ));
    }

    proptest! {
        /// The core M0 invariant at pulse level: bytes in, same bytes out.
        #[test]
        fn payload_roundtrips(
            palette in prop_oneof![Just(Palette::Mono1), Just(Palette::Color3)],
            data in prop::collection::vec(any::<u8>(), 0..200),
        ) {
            let grid = if palette == Palette::Mono1 { Grid::M1_MONO } else { Grid::M3_COLOR };
            let mut pulse = Pulse::new(grid, palette).unwrap();
            let data = &data[..data.len().min(pulse.capacity())];
            pulse.write_payload(data).unwrap();
            prop_assert_eq!(&pulse.read_payload()[..data.len()], data);
        }
    }
}
