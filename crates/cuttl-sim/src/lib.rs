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
//! - [`render`] — paint a pulse — **landed**
//! - [`channel`] — perspective warp, then crosstalk, gain/offset, vignette,
//!   blur, noise — **landed**
//! - [`geom`] / [`locate`] — finder detection and the homography, so the eye
//!   *locates* the grid instead of being told where it is — **landed**
//! - temporal distortion — rolling-shutter tear, exposure blend between
//!   consecutive pulses — **next**, and both need the beacon first
//!
//! ## Two ways to read a frame
//!
//! [`read`] is the real one: find the finders, solve the homography, sample
//! through it. [`sample`] is the axis-aligned shortcut, valid only when the
//! grid exactly fills the image — useful for testing the codec without the
//! optics in the way, and wrong the moment there is any perspective.
//!
//! From M2 this is joined by the **capture corpus** (`DESIGN.md` §5, M2):
//! recorded real camera frames replayed through the decoder in CI. The
//! simulator tests what we thought of; the corpus tests what we didn't.

#![forbid(unsafe_code)]

pub mod channel;
pub mod geom;
pub mod locate;

pub use channel::{Channel, Preset};
pub use geom::Homography;
pub use locate::locate;

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

/// Sample every cell centre through a known cell-space → image-space transform.
///
/// Bilinear rather than nearest-neighbour: cell centres land on fractional
/// pixels under any real perspective, and rounding them throws away the
/// sub-pixel accuracy the homography just worked out.
pub fn sample_with(
    image: &RgbImage,
    grid: Grid,
    palette: Palette,
    transform: &Homography,
) -> Result<Pulse> {
    let mut pulse = Pulse::new(grid, palette)?;
    for y in 0..grid.rows {
        for x in 0..grid.cols {
            let (ix, iy) = transform.apply(x as f64 + 0.5, y as f64 + 0.5);
            let rgb = channel::bilinear(image, ix, iy);
            pulse.set_cell(x, y, palette.from_rgb(rgb))?;
        }
    }
    Ok(pulse)
}

/// The eye's real entry point: locate the pulse, then read it.
///
/// A frame whose finders cannot be found is an *erasure* — the caller should
/// count it and move on, exactly as it would for a CRC reject (§1b). The skin
/// is looping; there will be another frame.
pub fn read(image: &RgbImage, grid: Grid, palette: Palette) -> Result<Pulse> {
    let transform = locate(image, grid).ok_or(Error::NotLocated)?;
    sample_with(image, grid, palette, &transform)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuttl_codec::Receiver;
    use cuttl_codec::stream;
    use proptest::prelude::*;
    use rand::{RngExt, SeedableRng, rngs::StdRng};

    const M1: (Grid, Palette) = (Grid::M1_MONO, Palette::Mono1);
    /// 4 px/cell — near the realistic lower bound for a camera that can still
    /// resolve cells (§2), and far less blur work per frame than the render
    /// default. Detection needs a little more than the codec does: a finder's
    /// runs are only `cell_px` wide, and the ratio test has to measure them
    /// through blur.
    const CELL_PX: u32 = 4;

    /// Outcome of pushing an object through the whole path.
    struct Transfer {
        object: cuttl_codec::Result<Vec<u8>>,
        /// Frames the eye could not find four finders in.
        unlocatable: usize,
        /// Frames it read but the CRC gate dropped.
        rejected: u32,
        /// Frames that survived the loss stage and were handed to the eye.
        delivered: usize,
    }

    impl Transfer {
        /// Frames that produced nothing usable, however they failed.
        fn wasted(&self) -> f64 {
            (self.unlocatable as f64 + self.rejected as f64) / self.delivered.max(1) as f64
        }
    }

    /// Run an object through the full path with a given channel and frame loss.
    fn transfer(data: &[u8], overhead: f32, preset: Preset, loss: f64, seed: u64) -> Transfer {
        let (grid, palette) = M1;
        let channel = Channel::preset(preset);
        let pulses = stream::encode(data, grid, palette, 42, overhead).unwrap();
        let mut rng = StdRng::seed_from_u64(seed);
        let mut rx = Receiver::new();
        let mut delivered = 0;
        let mut unlocatable = 0;

        for pulse in &pulses {
            if rng.random::<f64>() < loss {
                continue;
            }
            delivered += 1;
            let image = channel::apply(&render(pulse, CELL_PX), &channel, CELL_PX, &mut rng);
            // `read`, not `sample`: the presets warp now, so the eye has to
            // find the grid. A frame it cannot locate is just an erasure.
            match read(&image, grid, palette) {
                Ok(sampled) => {
                    rx.ingest(&sampled);
                }
                Err(_) => unlocatable += 1,
            }
        }
        Transfer {
            object: rx.finish(),
            unlocatable,
            rejected: rx.rejected(),
            delivered,
        }
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
        let run = transfer(&data, 2.0, Preset::None, 0.6, 1);
        assert_eq!(run.object.unwrap(), data);
        assert!(run.delivered < 700, "loss was not actually applied");
    }

    /// Heavy photometric distortion *and* loss together.
    ///
    /// Note this is not a hard test for mono: at 1 bit/cell the decision margin
    /// is enormous, and heavy costs zero pulses. It bites in colour — see
    /// `colour_is_more_fragile_than_mono`. The work here is done by the loss.
    #[test]
    fn survives_heavy_distortion_with_loss() {
        let data: Vec<u8> = (0..8192u32).map(|i| (i * 31) as u8).collect();
        let run = transfer(&data, 2.0, Preset::Heavy, 0.3, 2);
        assert_eq!(run.object.unwrap(), data);
    }

    /// The preset with warp removed, so a measurement can isolate photometric
    /// damage from registration error.
    fn photometric_only(preset: Preset) -> Channel {
        Channel {
            warp: 0.0,
            ..Channel::preset(preset)
        }
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
            &photometric_only(preset),
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
        assert!(transfer(&data, 0.0, Preset::None, 0.3, 3).object.is_err());
    }

    /// Mean cells misread per pulse, over `runs` seeds.
    fn mean_cell_errors(grid: Grid, palette: Palette, preset: Preset, runs: u64) -> f64 {
        let total: usize = (0..runs)
            .map(|seed| {
                let mut pulse = Pulse::new(grid, palette).unwrap();
                let data: Vec<u8> = (0..pulse.capacity())
                    .map(|i| (i * 7 + seed as usize * 3) as u8)
                    .collect();
                pulse.write_payload(&data).unwrap();
                let image = channel::apply(
                    &render(&pulse, CELL_PX),
                    &photometric_only(preset),
                    CELL_PX,
                    &mut StdRng::seed_from_u64(seed),
                );
                let got = sample(&image, grid, palette).unwrap();
                pulse
                    .cells()
                    .iter()
                    .zip(got.cells())
                    .filter(|(a, b)| a != b)
                    .count()
            })
            .sum();
        total as f64 / runs as f64
    }

    /// The measurement the inner code was sized against, pinned so it cannot
    /// drift silently. At 3 px/cell, mean cells misread per pulse:
    ///
    /// | preset | m1 mono | m3 colour |
    /// |--------|---------|-----------|
    /// | Light  | 0.0     | 0.0       |
    /// | Heavy  | 0.0     | 0.375     |
    /// | Brutal | 107.9   | 888.6     |
    ///
    /// Measured with warp removed, so this isolates *photometric* damage from
    /// registration error — the two fail in different ways and mixing them
    /// would make the number meaningless.
    ///
    /// There is still no middle ground: below the cliff, photometric distortion
    /// costs mono nothing and colour a third of a cell; above it, the count is
    /// far past what any affordable ECC could absorb. So the inner code is
    /// still not earning its keep against *this* axis. Where it does earn it is
    /// sub-cell sampling error under perspective — which now exists, and is
    /// what `survives_heavy_distortion_with_loss` exercises.
    #[test]
    fn channel_error_budget_is_what_ecc_was_sized_against() {
        for (grid, palette) in [M1, (Grid::M3_COLOR, Palette::Color3)] {
            for preset in [Preset::Light, Preset::Heavy] {
                let errors = mean_cell_errors(grid, palette, preset, 8);
                assert!(errors < 1.0, "{palette:?} at {preset:?} was {errors}");
            }
            let brutal = mean_cell_errors(grid, palette, Preset::Brutal, 8);
            assert!(brutal > 20.0, "{palette:?} at Brutal was only {brutal}");
        }
    }

    /// Brutal is past the cliff, and the inner code does **not** rescue it.
    ///
    /// This corrects an earlier prediction of mine. When `Preset::Brutal` was
    /// added, this test was written expecting it to start failing once
    /// Reed–Solomon landed. RS has landed and it still passes. Measurement
    /// says why: brutal produces ~108 misread cells per mono pulse, so
    /// correcting it would need more ECC than the pulse has bytes. Warp makes
    /// it worse still — most brutal frames now fail to locate at all, which is
    /// why this asserts on total waste rather than on CRC rejections alone.
    #[test]
    fn brutal_stays_unsurvivable_even_with_rs() {
        let data = vec![0x77u8; 8192];
        let run = transfer(&data, 2.0, Preset::Brutal, 0.0, 4);
        assert!(run.object.is_err(), "brutal now survives — what changed?");
        assert!(
            run.wasted() > 0.9,
            "expected near-total waste, got {:.2} ({} unlocatable + {} rejected of {})",
            run.wasted(),
            run.unlocatable,
            run.rejected,
            run.delivered
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
            prop_assert_eq!(transfer(&data, 2.0, Preset::Light, 0.2, 99).object.unwrap(), data);
        }
    }
}
