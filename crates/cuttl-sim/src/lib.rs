//! # cuttl-sim
//!
//! Synthetic optical channel: renders pulses from `cuttl-codec`, distorts
//! them the way a real screen→camera path would, and feeds them back to the
//! decoder — thousands of frames per second, no browser, no camera.
//!
//! This is the M0 deliverable and the highest-leverage piece of the project
//! (`DESIGN.md` §5, M0): every optical bug from M1 onward should be asked
//! "does the simulator reproduce it?" before touching a camera.
//!
//! ## Status
//! - [`render`] / [`sample`] — the identity path, no distortion — **landed**
//! - distortion — perspective warp → defocus/motion blur → sensor noise →
//!   colour crosstalk (3×3 mixing + offset) → exposure blend of adjacent
//!   pulses → rolling-shutter tear → frame drops — **M0 step 2**
//!
//! Note the ordering: [`sample`] currently assumes the grid is axis-aligned and
//! exactly fills the image, so it reads cell centres directly. Once warp exists,
//! this is where finder detection and the homography go — the eye will locate
//! the grid rather than being told where it is.
//!
//! From M2 this is joined by the **capture corpus** (`DESIGN.md` §5, M2):
//! recorded real camera frames replayed through the decoder in CI. The
//! simulator tests what we thought of; the corpus tests what we didn't.

#![forbid(unsafe_code)]

use cuttl_codec::{Error, Grid, Palette, Pulse, Result};
use image::{Rgb, RgbImage};

/// Default pixels per chroma cell when rendering.
pub const DEFAULT_CELL_PX: u32 = 8;

/// Paint a pulse at `cell_px` pixels per cell, nearest-neighbour.
///
/// Deliberately no anti-aliasing: a soft cell edge is exactly the cross-module
/// interference we are trying to *measure* later, not something to bake in here
/// (§1c). Blur belongs in the distortion stage where it can be dialled.
pub fn render(pulse: &Pulse, cell_px: u32) -> RgbImage {
    let grid = pulse.grid();
    let mut image = RgbImage::new(grid.cols as u32 * cell_px, grid.rows as u32 * cell_px);
    for y in 0..grid.rows {
        for x in 0..grid.cols {
            // Structural and payload cells alike are in range by construction.
            let rgb = pulse.rgb(x, y).expect("cell within grid bounds");
            for py in 0..cell_px {
                for px in 0..cell_px {
                    image.put_pixel(x as u32 * cell_px + px, y as u32 * cell_px + py, Rgb(rgb));
                }
            }
        }
    }
    image
}

/// Recover cell values from an image by reading cell centres.
///
/// The cell size is inferred from the image dimensions, so the eye is not told
/// the render scale — only the grid and palette, which it must already know
/// from the beacon. Non-integer scales are rejected rather than rounded.
pub fn sample(image: &RgbImage, grid: Grid, palette: Palette) -> Result<Pulse> {
    let (w, h) = image.dimensions();
    let dims_error = || Error::Dimensions {
        w,
        h,
        cols: grid.cols,
        rows: grid.rows,
    };

    let cell_px = w
        .checked_div(grid.cols as u32)
        .filter(|&n| n > 0)
        .ok_or_else(dims_error)?;
    if !w.is_multiple_of(grid.cols as u32) || h != grid.rows as u32 * cell_px {
        return Err(dims_error());
    }

    let mut pulse = Pulse::new(grid, palette)?;
    let half = cell_px / 2;
    for y in 0..grid.rows {
        for x in 0..grid.cols {
            let px = image.get_pixel(x as u32 * cell_px + half, y as u32 * cell_px + half);
            pulse.set_cell(x, y, palette.from_rgb(px.0))?;
        }
    }
    Ok(pulse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuttl_codec::stream::{self, Reassembler};
    use proptest::prelude::*;

    #[test]
    fn odd_image_dimensions_are_rejected() {
        let image = RgbImage::new(100, 100);
        assert!(matches!(
            sample(&image, Grid::M1_MONO, Palette::Mono1),
            Err(Error::Dimensions { .. })
        ));
    }

    proptest! {
        /// Render → sample must be the identity while the channel is lossless.
        /// Everything M0 step 2 adds is measured as a departure from this.
        #[test]
        fn render_sample_is_lossless(
            data in prop::collection::vec(any::<u8>(), 0..80),
            cell_px in 1u32..6,
        ) {
            let mut pulse = Pulse::new(Grid::M1_MONO, Palette::Mono1).unwrap();
            let data = &data[..data.len().min(pulse.capacity())];
            pulse.write_payload(data).unwrap();

            let sampled = sample(&render(&pulse, cell_px), Grid::M1_MONO, Palette::Mono1).unwrap();
            prop_assert_eq!(sampled.cells(), pulse.cells());
        }

        /// The full M0 step 1 path: object → pulses → images → cells → object.
        #[test]
        fn object_survives_the_optical_path(
            data in prop::collection::vec(any::<u8>(), 0..2048),
        ) {
            let (grid, palette) = (Grid::M1_MONO, Palette::Mono1);
            let pulses = stream::encode(&data, grid, palette, 42).unwrap();

            let mut rx = Reassembler::new();
            for pulse in &pulses {
                let image = render(pulse, 3);
                rx.ingest(&sample(&image, grid, palette).unwrap());
            }
            prop_assert_eq!(rx.finish().unwrap(), data);
        }
    }
}
