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
//! - [`render`] / [`sample`] — the identity path — **landed**
//! - [`channel`] — photometric distortion: crosstalk, gain/offset, vignette,
//!   blur, noise — **landed**
//! - geometric and temporal distortion — perspective warp, rolling-shutter
//!   tear, exposure blend between consecutive pulses — **M0 step 3**
//!
//! The split is not arbitrary. Photometric distortion leaves the grid where it
//! was, so the eye can keep reading cell centres. Everything in step 3 moves
//! the grid, which means the eye has to *locate* it — finder detection and a
//! homography — and that is a large enough change to be worth isolating from
//! the fountain layer landing at the same time.
//!
//! [`sample`] therefore still assumes an axis-aligned grid that exactly fills
//! the image. That assumption is where the homography will go.
//!
//! From M2 this is joined by the **capture corpus** (`DESIGN.md` §5, M2):
//! recorded real camera frames replayed through the decoder in CI. The
//! simulator tests what we thought of; the corpus tests what we didn't.

#![forbid(unsafe_code)]

pub mod channel;

pub use channel::{Channel, Preset};

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
    use cuttl_codec::Receiver;
    use cuttl_codec::stream;
    use proptest::prelude::*;
    use rand::{RngExt, SeedableRng, rngs::StdRng};

    const M1: (Grid, Palette) = (Grid::M1_MONO, Palette::Mono1);
    /// 3 px/cell — the realistic lower bound for a camera that can still
    /// resolve cells (§2), and 7× less blur work per frame than the render
    /// default. Tests should run at the hard end of the envelope anyway.
    const CELL_PX: u32 = 3;

    /// Run an object through the full path with a given channel and frame loss,
    /// returning the reconstruction attempt plus how many pulses were rejected.
    fn transfer(
        data: &[u8],
        overhead: f32,
        preset: Preset,
        loss: f64,
        seed: u64,
    ) -> (cuttl_codec::Result<Vec<u8>>, u32, usize) {
        let (grid, palette) = M1;
        let channel = Channel::preset(preset);
        let pulses = stream::encode(data, grid, palette, 42, overhead).unwrap();
        let mut rng = StdRng::seed_from_u64(seed);
        let mut rx = Receiver::new();
        let mut delivered = 0;

        for pulse in &pulses {
            if rng.random::<f64>() < loss {
                continue;
            }
            delivered += 1;
            let image = channel::apply(&render(pulse, CELL_PX), &channel, CELL_PX, &mut rng);
            if let Ok(sampled) = sample(&image, grid, palette) {
                rx.ingest(&sampled);
            }
        }
        (rx.finish(), rx.rejected(), delivered)
    }

    #[test]
    fn odd_image_dimensions_are_rejected() {
        let image = RgbImage::new(100, 100);
        assert!(matches!(
            sample(&image, Grid::M1_MONO, Palette::Mono1),
            Err(Error::Dimensions { .. })
        ));
    }

    /// The M0 headline: 60% of pulses thrown away, object still exact.
    #[test]
    fn survives_sixty_percent_frame_loss() {
        let data: Vec<u8> = (0..16384u32).map(|i| (i ^ (i >> 5)) as u8).collect();
        let (out, _, delivered) = transfer(&data, 2.0, Preset::None, 0.6, 1);
        assert_eq!(out.unwrap(), data);
        assert!(delivered < 1500, "loss was not actually applied");
    }

    /// Heavy photometric distortion *and* loss together.
    ///
    /// Note this is not a hard test for mono: at 1 bit/cell the decision margin
    /// is enormous, and heavy costs zero pulses. It bites in colour — see
    /// `colour_is_more_fragile_than_mono`. The work here is done by the loss.
    #[test]
    fn survives_heavy_distortion_with_loss() {
        let data: Vec<u8> = (0..8192u32).map(|i| (i * 31) as u8).collect();
        let (out, _, _) = transfer(&data, 2.0, Preset::Heavy, 0.3, 2);
        assert_eq!(out.unwrap(), data);
    }

    /// Fraction of cells misread after a round trip through the channel.
    fn cell_error_rate(palette: Palette, preset: Preset, seed: u64) -> f64 {
        // Same grid for both palettes, so only the bit depth differs.
        let grid = Grid::M3_COLOR;
        let mut pulse = Pulse::new(grid, palette).unwrap();
        let data: Vec<u8> = (0..pulse.capacity())
            .map(|i| (i * i * 7 + i * 13) as u8)
            .collect();
        pulse.write_payload(&data).unwrap();

        let image = channel::apply(
            &render(&pulse, CELL_PX),
            &Channel::preset(preset),
            CELL_PX,
            &mut StdRng::seed_from_u64(seed),
        );
        let got = sample(&image, grid, palette).unwrap();
        let wrong = pulse
            .cells()
            .iter()
            .zip(got.cells())
            .filter(|(a, b)| a != b)
            .count();
        wrong as f64 / pulse.cells().len() as f64
    }

    /// §1c as a measurement: 3 bits/cell is meaningfully more fragile than 1,
    /// because the decision margin between palette entries is smaller. This is
    /// why colour is the *third* lever and why M3 gates it behind an A/B
    /// toggle rather than assuming a 3× win.
    ///
    /// Measured at `Brutal`, not `Heavy`, for a mundane reason: at `Heavy` the
    /// cell error rate is around 3e-6, so a single pulse of ~5k cells sees zero
    /// errors and the comparison is pure noise. `Brutal` puts both palettes
    /// somewhere the difference is real.
    #[test]
    fn colour_is_more_fragile_than_mono() {
        let mean = |palette| {
            (0..4)
                .map(|seed| cell_error_rate(palette, Preset::Brutal, seed))
                .sum::<f64>()
                / 4.0
        };
        let (mono, colour) = (mean(Palette::Mono1), mean(Palette::Color3));
        assert!(
            colour > mono,
            "colour {colour:.4} was not worse than mono {mono:.4}"
        );
    }

    /// Without repair symbols, loss must fail cleanly — never return wrong
    /// bytes. This is the test that would catch the fountain silently doing
    /// nothing.
    #[test]
    fn zero_overhead_with_loss_fails_rather_than_lying() {
        let data = vec![0xC3u8; 8192];
        let (out, _, _) = transfer(&data, 0.0, Preset::None, 0.3, 3);
        assert!(out.is_err());
    }

    /// Measured motivation for the inner Reed–Solomon code (§1b).
    ///
    /// Past roughly 0.45 cell widths of blur, nearly every pulse contains at
    /// least one misread cell. With no inner code one bad cell costs the whole
    /// symbol, so rejection goes to ~100% and no amount of fountain overhead
    /// helps — the fountain repairs erasures, and this is an *error* problem.
    ///
    /// **When the inner code lands, this test should start failing.** That is
    /// the point: the failure is the measurement of what RS bought us.
    #[test]
    fn brutal_is_currently_unsurvivable() {
        let data = vec![0x77u8; 8192];
        let (out, rejected, delivered) = transfer(&data, 2.0, Preset::Brutal, 0.0, 4);
        assert!(
            out.is_err(),
            "brutal now survives — has the inner code landed?"
        );
        assert!(
            rejected as f64 > delivered as f64 * 0.9,
            "expected near-total rejection, got {rejected}/{delivered}"
        );
    }

    proptest! {
        /// Render → sample is the identity when the channel is clean. Every
        /// distortion stage is measured as a departure from this.
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
    }

    proptest! {
        // Each case pushes dozens of frames through blur and noise, so this
        // runs far fewer cases than the default. Breadth here comes from the
        // fixed-seed tests above, not from case count.
        #![proptest_config(ProptestConfig::with_cases(12))]

        /// Object → pulses → images → channel → cells → object.
        #[test]
        fn object_survives_the_optical_path(
            data in prop::collection::vec(any::<u8>(), 0..2048),
        ) {
            let (out, _, _) = transfer(&data, 2.0, Preset::Light, 0.2, 99);
            prop_assert_eq!(out.unwrap(), data);
        }
    }
}
