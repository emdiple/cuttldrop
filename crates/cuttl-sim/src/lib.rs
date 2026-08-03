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
//! ## Where the eye actually lives
//!
//! Not here. Finder detection, the homography and cell sampling are in
//! `cuttl_codec::eye`, because the browser needs the identical code and cannot
//! depend on the `image` crate. What remains here is the adapter: an
//! `RgbImage` is contiguous RGB bytes, so it becomes a `Raster` for free.
//!
//! From M2 this is joined by the **capture corpus** (`DESIGN.md` §5, M2):
//! recorded real camera frames replayed through the decoder in CI. The
//! simulator tests what we thought of; the corpus tests what we didn't.

#![forbid(unsafe_code)]

pub mod channel;

pub use channel::{Channel, Preset};

use cuttl_codec::{Grid, Palette, Pulse, Raster, Result, eye};
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

/// Borrow an `image` buffer as a codec [`Raster`] — no copy.
pub fn raster_of(image: &RgbImage) -> Result<Raster<'_>> {
    Raster::new(image.width(), image.height(), image.as_raw())
}

/// Locate the pulse in a frame and read it. See `cuttl_codec::eye::read`.
pub fn read(image: &RgbImage, grid: Grid, palette: Palette) -> Result<Pulse> {
    eye::read(&raster_of(image)?, grid, palette)
}

/// Read a frame known to be axis-aligned and exactly grid-sized.
pub fn sample(image: &RgbImage, grid: Grid, palette: Palette) -> Result<Pulse> {
    eye::sample_aligned(&raster_of(image)?, grid, palette)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuttl_codec::{Error, Receiver, stream};
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
        /// Frames whose beacon strips disagreed — rolling-shutter tear.
        torn: u32,
        /// Frames that survived the loss stage and were handed to the eye.
        delivered: usize,
    }

    impl Transfer {
        /// Frames that produced nothing usable, however they failed.
        fn wasted(&self) -> f64 {
            (self.unlocatable as f64 + self.rejected as f64 + self.torn as f64)
                / self.delivered.max(1) as f64
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

        for (index, pulse) in pulses.iter().enumerate() {
            if rng.random::<f64>() < loss {
                continue;
            }
            delivered += 1;
            // Every capture straddles two pulses in flight: the skin loops, so
            // the frame after the last is the first again.
            let next = &pulses[(index + 1) % pulses.len()];
            let image = channel::capture(
                &render(pulse, CELL_PX),
                &render(next, CELL_PX),
                &channel,
                CELL_PX,
                &mut rng,
            );
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
            torn: rx.torn(),
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
        let run = transfer(&data, 3.0, Preset::Heavy, 0.3, 2);
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

    /// Tear is *detected*, not merely survived.
    ///
    /// A stitched frame would fail the CRC gate regardless, so this is not what
    /// makes the transfer correct. What the beacon adds is knowing *why* a frame
    /// was dropped — which is the difference between a mystery and a `SLOW DOWN`
    /// hint the human can act on (§1e, §3a).
    #[test]
    fn tearing_is_detected_by_the_beacon_and_survived() {
        let data = vec![0x11u8; 4096];
        let run = transfer(&data, 3.0, Preset::Heavy, 0.0, 12);
        assert!(run.torn > 0, "no tears detected at Heavy");
        assert_eq!(run.object.unwrap(), data, "tears should be survivable");
        // Deliberately no assertion on the *rate*: the receiver short-circuits
        // to `Duplicate` once the object is complete, so it stops inspecting
        // beacons long before the pulse list runs out. The rate is pinned by
        // `a_stitched_frame_reads_as_torn` instead, which does not depend on
        // when decoding happens to finish.
    }

    /// The tear detector on its own, with no other distortion in the way.
    ///
    /// Not every stitch is detectable: if the tear line lands inside a beacon
    /// strip, or in the dark surround outside the pulse, both strips still come
    /// from one pulse and the frame reads as intact. Those are caught by the CRC
    /// gate instead — the beacon is the cheap early-out, not the guarantee.
    #[test]
    fn a_stitched_frame_reads_as_torn() {
        let (grid, palette) = M1;
        let torn_only = Channel {
            tear: 1.0,
            ..Channel::preset(Preset::None)
        };
        let pulses = stream::encode(&vec![0x42u8; 4096], grid, palette, 9, 0.0).unwrap();

        let mut detected = 0;
        let trials = 20;
        for seed in 0..trials {
            let mut rng = StdRng::seed_from_u64(seed);
            let image = channel::capture(
                &render(&pulses[3], CELL_PX),
                &render(&pulses[4], CELL_PX),
                &torn_only,
                CELL_PX,
                &mut rng,
            );
            let sampled =
                read(&image, grid, palette).expect("finders are identical in every pulse");
            if cuttl_codec::Receiver::new().ingest(&sampled) == cuttl_codec::Ingest::Torn {
                detected += 1;
            }
        }
        assert!(
            detected >= trials * 3 / 4,
            "only {detected} of {trials} stitched frames read as torn"
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

    /// Bands are a bet: they cost goodput (more inner ECC, more framing, and the
    /// smallest band sets the symbol size) and buy damage granularity. Whether
    /// that nets out is a measurement, not an opinion.
    ///
    /// Measured in bytes delivered per frame *shown*, which is the only figure
    /// that matters to someone holding a phone.
    #[test]
    fn banding_beats_whole_pulse_symbols_under_tear() {
        let object: Vec<u8> = (0..40_000u32).map(|i| (i * 13) as u8).collect();
        let palette = Palette::Color3;
        let heavy = Channel::preset(Preset::Heavy);

        let delivered_per_frame = |bands: u8, seed: u64| -> f64 {
            let grid = Grid {
                bands,
                ..Grid::M3_COLOR
            };
            let pulses = stream::encode(&object, grid, palette, 1, 3.0).unwrap();
            let mut rng = StdRng::seed_from_u64(seed);
            let mut rx = Receiver::new();
            let mut frames = 0usize;
            for (index, pulse) in pulses.iter().enumerate() {
                frames += 1;
                let next = &pulses[(index + 1) % pulses.len()];
                let image = channel::capture(
                    &render(pulse, CELL_PX),
                    &render(next, CELL_PX),
                    &heavy,
                    CELL_PX,
                    &mut rng,
                );
                if let Ok(sampled) = read(&image, grid, palette) {
                    rx.ingest(&sampled);
                }
                if rx.is_complete() {
                    break;
                }
            }
            assert!(rx.is_complete(), "{bands}-band run never completed");
            object.len() as f64 / frames as f64
        };

        // Averaged: a single seed swings by 30% and would make this flaky.
        let mean = |bands: u8| {
            (0..3)
                .map(|seed| delivered_per_frame(bands, 21 + seed))
                .sum::<f64>()
                / 3.0
        };
        let whole = mean(1);
        let banded = mean(Grid::M3_COLOR.bands);
        assert!(
            banded > whole,
            "banding lost: {banded:.0} vs {whole:.0} B/frame — reconsider `Grid::bands`"
        );
    }

    /// One simulated transfer through the timed shutter model ([`channel::capture_timed`]):
    /// the skin flips pulses at `pulse_hz`, a fixed 30 fps camera with phone
    /// shutter timing watches, and tear/blend happen when the physics says so.
    /// Returns (simulated seconds to completion, torn, rejected), or `None` if
    /// the deadline passes first.
    fn timed_transfer(
        object: &[u8],
        grid: Grid,
        palette: Palette,
        pulse_hz: f64,
        deadline: f64,
        seed: u64,
    ) -> Option<(f64, u32, u32)> {
        const CAPTURE_HZ: f64 = 30.0;
        /// Camera timestamp jitter, ± seconds. Without it, ideal clocks at
        /// integer pulse:capture ratios phase-lock, and one unlucky phase makes
        /// *every* capture torn forever — measured, 30 Hz pulses never
        /// completed against a 30 fps camera. Real clocks drift; ±2 ms turns
        /// the pathology into the statistical cost it actually is.
        const JITTER: f64 = 0.002;
        // Tear and blend zeroed: in the timed model they are outcomes, not
        // parameters, and leaving the coin flips on would double-count them.
        let channel = Channel {
            tear: 0.0,
            blend: 0.0,
            ..Channel::preset(Preset::Heavy)
        };
        let shutter = channel::Shutter::PHONE;
        let pulses = stream::encode(object, grid, palette, 88, 3.0).unwrap();
        let frames: Vec<RgbImage> = pulses.iter().map(|p| render(p, CELL_PX)).collect();
        let period = 1.0 / pulse_hz;

        let mut rng = StdRng::seed_from_u64(seed);
        let mut rx = Receiver::new();
        let start: f64 = shutter.exposure + rng.random_range(0.0..period);
        let mut n = 0u64;
        loop {
            let t = start + n as f64 / CAPTURE_HZ + rng.random_range(-JITTER..JITTER);
            n += 1;
            if t > deadline {
                return None;
            }
            // The pulses whose screen time overlaps this capture's window,
            // folded through the loop — the skin repeats forever.
            let first = ((t - shutter.exposure) / period).floor() as i64;
            let last = ((t + shutter.readout) / period).floor() as i64;
            let window: Vec<&RgbImage> = (first..=last)
                .map(|i| &frames[i.rem_euclid(frames.len() as i64) as usize])
                .collect();
            let phase = t - first as f64 * period;
            let frame = channel::capture_timed(
                &window, period, phase, &shutter, &channel, CELL_PX, &mut rng,
            );
            if let Ok(pulse) = read(&frame, grid, palette) {
                rx.ingest(&pulse);
            }
            if rx.is_complete() {
                return Some((t, rx.torn(), rx.rejected()));
            }
        }
    }

    /// A flip landing mid-readout reads as torn with nothing but timing at
    /// work — no `tear` probability anywhere in sight.
    #[test]
    fn a_mid_readout_flip_reads_as_torn_in_the_timed_model() {
        let (grid, palette) = M1;
        let pulses = stream::encode(&vec![0x42u8; 4096], grid, palette, 9, 0.0).unwrap();
        let frames = [render(&pulses[3], CELL_PX), render(&pulses[4], CELL_PX)];
        let shutter = channel::Shutter {
            readout: 0.015,
            exposure: 0.0,
        };
        // Pulse period 20 ms; rows read over [10 ms, 25 ms], flip at 20 ms —
        // two thirds of the way down the frame.
        let mut rng = StdRng::seed_from_u64(5);
        let frame = channel::capture_timed(
            &[&frames[0], &frames[1]],
            0.020,
            0.010,
            &shutter,
            &Channel::preset(Preset::None),
            CELL_PX,
            &mut rng,
        );
        let sampled = read(&frame, grid, palette).expect("finders are shared by both pulses");
        assert_eq!(
            cuttl_codec::Receiver::new().ingest(&sampled),
            cuttl_codec::Ingest::Torn
        );
    }

    /// Pulse-rate sweep, the decimen question: how fast should the skin strobe?
    /// A measurement tool, not CI — run with:
    /// `cargo test -p cuttl-sim --release rate_sweep -- --ignored --nocapture`
    ///
    /// Measured (mono, 30 fps camera, `Shutter::PHONE`, heavy channel, ±2 ms
    /// camera jitter, mean of 3 seeds):
    ///
    /// | pulse Hz | 5 | 10 | 15 | **20** | 24 | 30 | 45 | 60 |
    /// |---|---|---|---|---|---|---|---|---|
    /// | KB/s | 0.69 | 1.37 | 2.06 | **2.74** | 2.29 | starved | 1.62 | 0.38 |
    ///
    /// Three findings. (1) Goodput is linear in pulse rate up to **20 Hz**, the
    /// measured optimum — twice the old default, and exactly §3d's original
    /// guess. (2) Pulse rates at integer ratios of the capture rate are a trap:
    /// at 30:30 the phase relationship is frozen, one unlucky draw makes every
    /// capture torn, and the transfer starves *forever* — jitter alone does not
    /// walk it out. decimen's field notes ("reduce to 24–30 on 60 Hz screens")
    /// are this same cliff seen from the other side. (3) Past ~43 Hz the 23 ms
    /// shutter window is wider than the pulse period, so no capture can be
    /// clean; mono survives only on blend-dominated rows at ~10× the cost.
    #[test]
    #[ignore = "measurement tool; findings pinned by faster_strobing_* and phase_lock_*"]
    fn rate_sweep_prints_goodput_by_pulse_rate() {
        let object: Vec<u8> = (0..12_000u32).map(|i| (i * 29) as u8).collect();
        println!("pulse_hz    KB/s  (per seed: s to complete, torn, rejected)");
        for &hz in &[5.0, 10.0, 15.0, 20.0, 24.0, 30.0, 45.0, 60.0] {
            let mut detail = String::new();
            let mut rates = Vec::new();
            for seed in 0..3u64 {
                match timed_transfer(&object, M1.0, M1.1, hz, 120.0, 100 + seed) {
                    Some((secs, torn, rejected)) => {
                        rates.push(object.len() as f64 / secs / 1024.0);
                        detail += &format!("  [{secs:5.1}s t{torn:<3} r{rejected:<3}]");
                    }
                    None => detail += "  [did not finish]",
                }
            }
            let mean = rates.iter().sum::<f64>() / rates.len().max(1) as f64;
            println!("{hz:>8}  {mean:>6.2}{detail}");
        }
    }

    /// The rate sweep's headline, pinned: 20 pulses/s beats the old 10 Hz
    /// default handily. If this fails, either the shutter model or the codec
    /// regressed in a way that changes the recommended operating point — and
    /// the skin's default rate slider is set from this measurement.
    #[test]
    fn faster_strobing_beats_the_default_rate() {
        let object: Vec<u8> = (0..6_000u32).map(|i| (i * 41) as u8).collect();
        for seed in [201, 202] {
            let (slow, ..) =
                timed_transfer(&object, M1.0, M1.1, 10.0, 60.0, seed).expect("10 Hz must complete");
            let (fast, ..) =
                timed_transfer(&object, M1.0, M1.1, 20.0, 60.0, seed).expect("20 Hz must complete");
            assert!(
                fast < slow,
                "seed {seed}: 20 Hz took {fast:.1}s, 10 Hz took {slow:.1}s"
            );
        }
    }

    /// The clock-lock hazard, pinned: when the pulse clock sits at an integer
    /// ratio of the capture clock, their phase relationship freezes, and one
    /// unlucky draw makes every capture straddle a flip — *forever*. Camera
    /// timestamp jitter of ±2 ms cannot walk out of it; only a frequency
    /// offset could, and ideal clocks have none. At 30:30 the sweep starved on
    /// all three seeds; at 60:30 one lucky-phase seed flew and the rest
    /// crawled. This lottery is why the skin's rate control must keep clear of
    /// the capture rate and its multiples — decimen's "reduce to 24–30 on
    /// 60 Hz screens" field note is the same cliff seen from the other side.
    #[test]
    fn phase_lock_at_the_capture_rate_can_starve_a_transfer() {
        let object = vec![0x6Bu8; 4_000];
        // Sanity: the same object at the recommended rate completes in ~1.5 s.
        let (base, ..) =
            timed_transfer(&object, M1.0, M1.1, 20.0, 60.0, 100).expect("20 Hz must complete");
        assert!(base < 15.0);
        // Same seed, pulse rate == capture rate: the phase draw is bad, and no
        // amount of extra time helps — 15 s is a 10× allowance already.
        assert!(
            timed_transfer(&object, M1.0, M1.1, 30.0, 15.0, 100).is_none(),
            "a locked bad phase should starve; if this completes, the model gained clock drift"
        );
    }

    /// One point of the density sweep: locate rate and misread cells per
    /// located frame, single frames through the heavy channel (warp included —
    /// registration error is exactly what kills density).
    fn density_point(cols: u16, rows: u16, cell_px: u32, seeds: u64) -> (f64, f64) {
        let grid = Grid {
            cols,
            rows,
            ..Grid::M3_COLOR
        };
        let palette = Palette::Color3;
        let mut located = 0u32;
        let mut errors = 0usize;
        for seed in 0..seeds {
            let mut pulse = Pulse::new(grid, palette).unwrap();
            let data: Vec<u8> = (0..pulse.capacity())
                .map(|i| (i * 31 + seed as usize * 7) as u8)
                .collect();
            pulse.write_payload(&data).unwrap();
            let image = channel::apply(
                &render(&pulse, cell_px),
                &Channel::preset(Preset::Heavy),
                cell_px,
                &mut StdRng::seed_from_u64(400 + seed),
            );
            if let Ok(got) = read(&image, grid, palette) {
                located += 1;
                errors += pulse
                    .cells()
                    .iter()
                    .zip(got.cells())
                    .filter(|(a, b)| a != b)
                    .count();
            }
        }
        (
            located as f64 / seeds as f64,
            errors as f64 / located.max(1) as f64,
        )
    }

    /// Density sweep, the other decimen question: how many cells can a frame
    /// carry before our detector gives up? Run with:
    /// `cargo test -p cuttl-sim --release density_sweep -- --ignored --nocapture`
    ///
    /// Measured (colour, heavy channel with warp, 6 seeds; locate % / misread
    /// cells per located frame):
    ///
    /// | grid | 4 px/cell | 3 px/cell | 2 px/cell |
    /// |---|---|---|---|
    /// | 96×54 | 100% / 1.7 | 100% / 18 | 67% / 160 |
    /// | 128×72 | 100% / 3.3 | 100% / 29 | 83% / 300 |
    /// | 160×90 | 100% / 11 | 83% / 44 | 100% / 742 |
    /// | 192×108 | 100% / 10 | 100% / 58 | 100% / 837 |
    ///
    /// The cliff sits between 3 and 2 px/cell and is driven by *sampling*
    /// error, not detection — at 2 px/cell the finders are often still found
    /// while the payload is garbage. At 4 px/cell even 192×108 (4× the M3 cell
    /// count, 768×432 sensor px — inside the eye's 960 px working width) reads
    /// with ~10 misread cells, comfortably within the inner-RS budget. The M4
    /// grid target is therefore evidence, not aspiration.
    #[test]
    #[ignore = "measurement tool; the finding is pinned by the_m4_grid_is_reachable_in_sim"]
    fn density_sweep_prints_detection_cliff() {
        println!("   grid  px/cell   image_px  locate  err_cells/frame");
        for &(cols, rows) in &[(96u16, 54u16), (128, 72), (160, 90), (192, 108)] {
            for &cell_px in &[4u32, 3, 2] {
                let (locate, errs) = density_point(cols, rows, cell_px, 6);
                println!(
                    "{cols:>3}×{rows:<3} {cell_px:>7}  {:>4}×{:<4}  {:>5.0}%  {errs:>10.1}",
                    cols as u32 * cell_px,
                    rows as u32 * cell_px,
                    locate * 100.0
                );
            }
        }
    }

    /// The density sweep's headline, pinned: the M4 grid (192×108, 4× the M3
    /// cell count) locates every frame and misreads few enough cells for the
    /// inner code, while 2 px/cell is confirmed past the cliff. If the first
    /// half fails, the detector regressed; if the second half fails, the
    /// channel got too easy and the sweep needs re-running.
    #[test]
    fn the_m4_grid_is_reachable_in_sim() {
        let (locate, errors) = density_point(192, 108, 4, 4);
        assert!(
            locate >= 0.99,
            "192×108 @ 4 px/cell located only {:.0}%",
            locate * 100.0
        );
        assert!(
            errors < 25.0,
            "192×108 @ 4 px/cell misread {errors:.1} cells"
        );

        // Past the cliff means unusable, and the yardstick is the inner code:
        // a colour pulse can absorb ~64 spread byte errors at the very best,
        // so anything near that count in *cells* is far beyond repair.
        let (_, past_cliff) = density_point(96, 54, 2, 4);
        assert!(
            past_cliff > 64.0,
            "2 px/cell should be far past the inner-code budget, saw {past_cliff:.1}"
        );
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
