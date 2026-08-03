//! Synthetic optical channel — the photometric half (`DESIGN.md` §5, M0).
//!
//! What a screen→camera path does to a pulse, in the order it happens:
//!
//! 1. **Colour crosstalk** — display primaries and camera Bayer filters do not
//!    align, so a "pure red" cell produces real green and blue response. This
//!    is the distortion the pilot cells exist to measure and invert (§3b).
//! 2. **Gain and offset** — locked-but-wrong white balance, plus ambient light
//!    lifting the black level.
//! 3. **Vignette** — radial falloff. Deliberately *spatially varying*, because
//!    that is precisely why a single global colour transform is not enough and
//!    the pilots have to be distributed (§3b).
//! 4. **Blur** — defocus and lens MTF. Measured in *cell widths*, not pixels,
//!    because that is the physically meaningful unit: it is the mechanism
//!    behind cross-module colour interference (§1c).
//! 5. **Noise** — sensor shot and read noise.
//!
//! Before all of those comes **perspective warp** — the one geometric stage,
//! applied first because it decides *where* cells are rather than what colour
//! they read as. Warping an already-blurred frame would blur it twice.
//!
//! ## The temporal half
//!
//! Two models, at different fidelities:
//!
//! - [`capture`] — tear and exposure blend as *probabilities* (`Channel::tear`,
//!   `Channel::blend`). Cheap, and right for tests that need "some frames are
//!   torn" without caring why.
//! - [`capture_timed`] — tear and blend *emerge* from [`Shutter`] physics: each
//!   sensor row integrates whatever pulses were on screen during its exposure,
//!   rows are read top to bottom over the readout time. This is what makes
//!   **pulse rate** a sweepable variable: raise it and straddles happen more
//!   often because the maths says so, not because a knob was turned.
//!
//! Frame loss is not modelled here — that is the caller dropping whole pulses,
//! which needs no image processing.

use cuttl_codec::{Homography, Raster};
use image::{Rgb, RgbImage};
use rand::{Rng, RngExt};

/// Photometric distortion parameters. All zero is a perfect channel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Channel {
    /// Cross-channel mixing, 0.0 = none, 1.0 = fully mixed.
    pub crosstalk: f32,
    /// Per-channel multiplier — white balance error.
    pub gain: [f32; 3],
    /// Per-channel additive offset in units of full scale — ambient lift.
    pub offset: [f32; 3],
    /// Radial falloff at the corners, 0.0 = flat.
    pub vignette: f32,
    /// Gaussian blur sigma, in cell widths.
    pub blur_cells: f32,
    /// Gaussian noise sigma, in units of full scale.
    pub noise: f32,
    /// Perspective warp: each corner is pulled inward by a random fraction of
    /// the frame, up to this much, on top of a fixed 5% inset.
    ///
    /// This is what forces the eye to *locate* the grid rather than assume it.
    /// Unlike every other field here it changes where cells are, not what
    /// colour they read as.
    pub warp: f32,
    /// Rolling-shutter **motion skew**: peak sideways displacement, as a
    /// fraction of frame width, of the middle rows relative to the edges.
    ///
    /// The sensor reads row by row over 10–33 ms, so a handheld camera holds a
    /// slightly different pose for each row. Constant velocity would produce a
    /// shear, which is affine and therefore absorbed by the homography for
    /// free — so this models the part that is *not*: a smooth bulge, standing
    /// in for the acceleration and rotation any real hand supplies.
    ///
    /// This is a distinct failure from [`Channel::tear`]. Tear is two pulses
    /// stitched with the grid in the same place — a content problem the beacon
    /// detects. Skew moves the grid itself, happens on every handheld frame
    /// rather than only on straddled ones, and nothing detects it at all: it
    /// simply shows up as misread cells.
    pub skew: f32,
    /// Radial lens distortion, as a fraction of the half-diagonal displaced at
    /// the frame corners. Positive is barrel.
    ///
    /// Not projective, so no homography can absorb any of it. Worst close in
    /// and at the edges, which is where a phone is held to fill its frame with
    /// a dense grid.
    pub barrel: f32,
    /// Probability that a capture is stitched from two pulses by the rolling
    /// shutter. Only meaningful through [`capture`].
    pub tear: f32,
    /// Probability that the exposure straddles a pulse flip and integrates both.
    pub blend: f32,
}

/// Named severities, so the CLI and tests agree on what "heavy" means.
///
/// [`Preset::Brutal`] is deliberately past what the current stack can survive.
/// Because blur is measured in cell widths, its effect is scale-invariant: at
/// `blur_cells` around 0.45 a cell centre picks up roughly half its value from
/// its neighbours, so *almost every pulse* contains at least one misread cell.
/// With no inner code, one bad cell costs the whole symbol, and the transfer
/// collapses to nothing — measured, 100% rejection.
///
/// That is the empirical argument for the inner Reed–Solomon layer (§1b), and
/// `brutal_stays_unsurvivable_even_with_rs` pins it, and records why the
/// inner code did not rescue it: ~33 misread cells per pulse is far past what
/// any affordable ECC can absorb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    None,
    Light,
    Heavy,
    Brutal,
}

impl Channel {
    pub const fn preset(preset: Preset) -> Self {
        match preset {
            Preset::None => Self {
                crosstalk: 0.0,
                gain: [1.0, 1.0, 1.0],
                offset: [0.0, 0.0, 0.0],
                vignette: 0.0,
                blur_cells: 0.0,
                noise: 0.0,
                warp: 0.0,
                // Presets leave these at zero on purpose: every measurement in the
                // repo predates them, and folding a new distortion into "heavy" would
                // silently move numbers that are cited as evidence elsewhere. Set them
                // explicitly, as `alignment_patterns_earn_their_cells` does.
                skew: 0.0,
                barrel: 0.0,
                tear: 0.0,
                blend: 0.0,
            },
            // A good phone, held reasonably still, in a well-lit room.
            Preset::Light => Self {
                crosstalk: 0.10,
                gain: [1.00, 0.98, 1.03],
                offset: [0.01, 0.00, -0.01],
                vignette: 0.15,
                blur_cells: 0.12,
                noise: 0.015,
                warp: 0.03,
                skew: 0.0,
                barrel: 0.0,
                tear: 0.10,
                blend: 0.05,
            },
            // A cheap camera, slightly soft, room lights on, screen at an
            // angle.
            //
            // Measured: this costs mono *nothing* — 1 bit/cell has a huge
            // decision margin, black against white with the threshold halfway
            // between. It costs colour ~1.5% of pulses. That asymmetry is §1c's
            // claim that colour accuracy is a real but third-order lever,
            // showing up as a number rather than an assertion.
            Preset::Heavy => Self {
                crosstalk: 0.30,
                gain: [1.00, 0.92, 1.12],
                offset: [0.04, 0.00, -0.03],
                vignette: 0.30,
                blur_cells: 0.25,
                noise: 0.035,
                warp: 0.08,
                skew: 0.0,
                barrel: 0.0,
                tear: 0.30,
                blend: 0.15,
            },
            // Past the cliff. See the enum docs — this is where the missing
            // inner code stops being a theoretical concern.
            Preset::Brutal => Self {
                crosstalk: 0.35,
                gain: [1.00, 0.90, 1.15],
                offset: [0.05, 0.00, -0.04],
                vignette: 0.35,
                blur_cells: 0.45,
                noise: 0.06,
                warp: 0.12,
                skew: 0.0,
                barrel: 0.0,
                tear: 0.55,
                blend: 0.35,
            },
        }
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::preset(Preset::None)
    }
}

/// Plausible display-primary → camera-filter mixing, applied at `crosstalk` = 1.
/// Rows sum to 1 so overall brightness is preserved and only *separation* is lost.
const MIXING: [[f32; 3]; 3] = [[0.72, 0.20, 0.08], [0.15, 0.70, 0.15], [0.08, 0.22, 0.70]];

/// Push one frame through the channel. `cell_px` sets the scale for `blur_cells`.
pub fn apply(image: &RgbImage, channel: &Channel, cell_px: u32, rng: &mut impl Rng) -> RgbImage {
    if channel.is_identity() {
        return image.clone();
    }
    let transform = warp_transform(image.dimensions(), channel.warp, rng);
    let warped = warp_with(image, transform.as_ref(), Lens::of(channel));
    photometric(&warped, channel, cell_px, rng)
}

/// Capture two consecutive pulses as a single frame.
///
/// This is the temporal half of the channel, and it is the bottleneck the whole
/// design bends around (§2). Two things happen when a capture does not line up
/// with a pulse:
///
/// - **Rolling-shutter tear** — the sensor reads row by row over 10–33 ms, so a
///   mid-readout flip stitches the top of one pulse to the bottom of the next.
/// - **Exposure straddle** — a long exposure spanning the flip integrates both,
///   giving a ghosted blend of the two.
///
/// Note the ordering, which is a correctness point rather than a preference:
/// the tear line is horizontal in **sensor** space, not screen space, so both
/// pulses are warped *first* and stitched afterwards. Compositing before the
/// warp would bend the tear line along with the image, which no rolling shutter
/// does. Both frames share one transform — the camera does not move between the
/// first sensor row and the last.
pub fn capture(
    current: &RgbImage,
    next: &RgbImage,
    channel: &Channel,
    cell_px: u32,
    rng: &mut impl Rng,
) -> RgbImage {
    if channel.is_identity() {
        return current.clone();
    }
    let transform = warp_transform(current.dimensions(), channel.warp, rng);
    let a = warp_with(current, transform.as_ref(), Lens::of(channel));
    let b = warp_with(next, transform.as_ref(), Lens::of(channel));
    photometric(&composite(&a, &b, channel, rng), channel, cell_px, rng)
}

/// Camera timing: what the sensor is doing while the skin flips pulses.
///
/// A row read at instant `t` has integrated the light of `[t - exposure, t]`;
/// the first row is read at the capture instant and the last `readout` seconds
/// later. Everything [`capture`] models with coin flips follows from these two
/// numbers and the pulse period.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shutter {
    /// Seconds from the first sensor row being read to the last.
    pub readout: f64,
    /// Seconds each row integrates light before it is read.
    pub exposure: f64,
}

impl Shutter {
    /// A mid-range phone filming a screen indoors: ~15 ms rolling readout,
    /// ~8 ms exposure (screens are bright; auto-exposure keeps it short).
    pub const PHONE: Self = Self {
        readout: 0.015,
        exposure: 0.008,
    };
}

/// Capture one frame from a skin flipping pulses every `pulse_period` seconds.
///
/// `pulses[i]` is on screen during `[i·period, (i+1)·period)`; `phase` is the
/// instant the first sensor row is read, in the same clock. The caller supplies
/// enough consecutive pulses that the whole window `[phase - exposure,
/// phase + readout]` is covered — indices are clamped, so a short slice merely
/// freezes the ends rather than panicking.
///
/// Tear and blend are not parameters here; they *happen* when the window spans
/// a flip. `Channel::tear` and `Channel::blend` are ignored, everything
/// photometric still applies, and — as in [`capture`] — one warp is shared by
/// every contributing pulse, because the camera does not move between the first
/// sensor row and the last.
pub fn capture_timed(
    pulses: &[&RgbImage],
    pulse_period: f64,
    phase: f64,
    shutter: &Shutter,
    channel: &Channel,
    cell_px: u32,
    rng: &mut impl Rng,
) -> RgbImage {
    assert!(!pulses.is_empty() && pulse_period > 0.0);
    let (w, h) = pulses[0].dimensions();
    let transform = warp_transform((w, h), channel.warp, rng);
    // Warped lazily: most windows touch one or two pulses.
    let mut warped: Vec<Option<RgbImage>> = (0..pulses.len()).map(|_| None).collect();

    let mut out = RgbImage::new(w, h);
    for y in 0..h {
        let row_at = if h > 1 {
            phase + (y as f64 / (h - 1) as f64) * shutter.readout
        } else {
            phase
        };
        let (from, to) = (row_at - shutter.exposure, row_at);

        // Which pulses this row saw, and for how long.
        let mut weights: Vec<(usize, f64)> = Vec::new();
        let first = (from / pulse_period).floor().max(0.0) as usize;
        let last = ((to / pulse_period).floor().max(0.0) as usize).min(pulses.len() - 1);
        for i in first.min(pulses.len() - 1)..=last {
            let (lo, hi) = (i as f64 * pulse_period, (i + 1) as f64 * pulse_period);
            let weight = if shutter.exposure > 0.0 {
                (to.min(hi) - from.max(lo)).max(0.0)
            } else if (lo..hi).contains(&row_at) {
                1.0
            } else {
                0.0
            };
            if weight > 0.0 {
                weights.push((i, weight));
            }
        }
        for &(i, _) in &weights {
            if warped[i].is_none() {
                warped[i] = Some(warp_with(pulses[i], transform.as_ref(), Lens::of(channel)));
            }
        }

        let total: f64 = weights.iter().map(|(_, weight)| weight).sum();
        for x in 0..w {
            let mut acc = [0f64; 3];
            for &(i, weight) in &weights {
                let px = warped[i].as_ref().expect("warped above").get_pixel(x, y).0;
                for (c, sum) in acc.iter_mut().enumerate() {
                    *sum += weight * px[c] as f64;
                }
            }
            let mut px = [0u8; 3];
            if total > 0.0 {
                for (c, value) in px.iter_mut().enumerate() {
                    *value = (acc[c] / total).round().clamp(0.0, 255.0) as u8;
                }
            }
            out.put_pixel(x, y, Rgb(px));
        }
    }
    // Timing always applies; the photometric stages are skipped for a perfect
    // camera exactly as `apply` skips them, so a clean window is bit-exact.
    if channel.is_identity() {
        return out;
    }
    photometric(&out, channel, cell_px, rng)
}

/// Stitch or blend two already-warped frames.
fn composite(a: &RgbImage, b: &RgbImage, channel: &Channel, rng: &mut impl Rng) -> RgbImage {
    let (w, h) = a.dimensions();
    if h > 2 && rng.random::<f32>() < channel.tear {
        let line = rng.random_range(1..h - 1);
        let mut out = a.clone();
        for y in line..h {
            for x in 0..w {
                out.put_pixel(x, y, *b.get_pixel(x, y));
            }
        }
        return out;
    }
    if rng.random::<f32>() < channel.blend {
        let alpha = rng.random_range(0.25f32..0.75);
        let mut out = RgbImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (p, q) = (a.get_pixel(x, y).0, b.get_pixel(x, y).0);
                let mut px = [0u8; 3];
                for (c, value) in px.iter_mut().enumerate() {
                    *value = (p[c] as f32 * alpha + q[c] as f32 * (1.0 - alpha)) as u8;
                }
                out.put_pixel(x, y, Rgb(px));
            }
        }
        return out;
    }
    a.clone()
}

fn photometric(image: &RgbImage, channel: &Channel, cell_px: u32, rng: &mut impl Rng) -> RgbImage {
    let (w, h) = image.dimensions();
    let mut buf = vec![0f32; (w * h * 3) as usize];

    // Stages 1-3 are per-pixel and independent, so they fold into one pass.
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let max_r2 = cx * cx + cy * cy;
    for y in 0..h {
        for x in 0..w {
            let px = image.get_pixel(x, y).0;
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let vignette = 1.0 - channel.vignette * ((dx * dx + dy * dy) / max_r2);

            for out in 0..3 {
                let mixed: f32 = (0..3)
                    .map(|inp| {
                        let identity = if inp == out { 1.0 } else { 0.0 };
                        let weight = identity + channel.crosstalk * (MIXING[out][inp] - identity);
                        weight * px[inp] as f32 / 255.0
                    })
                    .sum();
                let value = (mixed * channel.gain[out] + channel.offset[out]) * vignette;
                buf[((y * w + x) * 3 + out as u32) as usize] = value;
            }
        }
    }

    let sigma = channel.blur_cells * cell_px as f32;
    if sigma > 0.0 {
        blur(&mut buf, w, h, sigma);
    }

    let mut out = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut px = [0u8; 3];
            for c in 0..3 {
                let mut value = buf[((y * w + x) * 3 + c as u32) as usize];
                if channel.noise > 0.0 {
                    value += normal(rng) * channel.noise;
                }
                px[c] = (value * 255.0).clamp(0.0, 255.0) as u8;
            }
            out.put_pixel(x, y, Rgb(px));
        }
    }
    out
}

/// Choose a projection of the frame onto a quad inside itself — a screen
/// photographed at an angle, from somewhere that is not dead centre.
///
/// Returns the *inverse* map, because destination pixels are sampled backwards
/// through it. That is the standard direction: it gives every output pixel a
/// value, where forward-mapping leaves holes.
fn warp_transform((w, h): (u32, u32), amount: f32, rng: &mut impl Rng) -> Option<Homography> {
    if amount <= 0.0 {
        return None;
    }
    let (fw, fh) = (w as f64, h as f64);
    let span = fw.min(fh);
    let inset = 0.05 * span;
    let mut jitter = || inset + rng.random_range(0.0..amount as f64) * span;

    let source = [(0.0, 0.0), (fw, 0.0), (0.0, fh), (fw, fh)];
    let target = [
        (jitter(), jitter()),
        (fw - jitter(), jitter()),
        (jitter(), fh - jitter()),
        (fw - jitter(), fh - jitter()),
    ];
    Homography::from_correspondences(source, target).and_then(|forward| forward.inverse())
}

/// Resample a frame through an inverse transform, leaving a dark surround.
fn warp_with(image: &RgbImage, inverse: Option<&Homography>, lens: Lens) -> RgbImage {
    if inverse.is_none() && lens.is_identity() {
        return image.clone();
    }
    let (w, h) = image.dimensions();
    let (fw, fh) = (w as f64, h as f64);
    let Ok(source) = Raster::new(w, h, image.as_raw()) else {
        return image.clone();
    };
    let mut out = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            // Composed into one resample rather than two passes: resampling
            // twice would blur the frame more than either stage models, and
            // blur is the variable half this file's measurements turn on.
            let (dx, dy) = lens.unbend(x as f64 + 0.5, y as f64 + 0.5, fw, fh);
            let (sx, sy) = match inverse {
                Some(inverse) => inverse.apply(dx, dy),
                None => (dx, dy),
            };
            if sx < 0.0 || sy < 0.0 || sx >= fw || sy >= fh {
                continue; // dark surround
            }
            out.put_pixel(x, y, Rgb(source.bilinear(sx, sy)));
        }
    }
    out
}

/// The non-projective part of the geometry: everything a homography cannot
/// absorb. See [`Channel::skew`] and [`Channel::barrel`].
#[derive(Debug, Clone, Copy)]
struct Lens {
    skew: f64,
    barrel: f64,
}

impl Lens {
    fn of(channel: &Channel) -> Self {
        Self {
            skew: channel.skew as f64,
            barrel: channel.barrel as f64,
        }
    }

    fn is_identity(&self) -> bool {
        self.skew == 0.0 && self.barrel == 0.0
    }

    /// Map an output pixel back to where the undistorted image had it.
    fn unbend(&self, x: f64, y: f64, w: f64, h: f64) -> (f64, f64) {
        if self.is_identity() {
            return (x, y);
        }
        let (cx, cy) = (w / 2.0, h / 2.0);
        let (mut px, mut py) = (x, y);

        if self.barrel != 0.0 {
            // r' = r(1 + k·(r/rmax)²) about the frame centre.
            let (ux, uy) = (px - cx, py - cy);
            let rmax = cx.hypot(cy);
            let r = ux.hypot(uy) / rmax;
            let k = 1.0 + self.barrel * r * r;
            px = cx + ux * k;
            py = cy + uy * k;
        }
        if self.skew != 0.0 {
            // A half-period sine down the frame: zero at the first and last
            // sensor row, peak in the middle. Quadratic-or-higher in `y`, so
            // unlike a shear it survives being fitted by four corners.
            px += self.skew * w * (core::f64::consts::PI * (py / h)).sin();
        }
        (px, py)
    }
}

/// Separable Gaussian, edges clamped.
fn blur(buf: &mut [f32], w: u32, h: u32, sigma: f32) {
    let radius = (3.0 * sigma).ceil() as i32;
    let kernel: Vec<f32> = {
        let raw: Vec<f32> = (-radius..=radius)
            .map(|i| (-((i * i) as f32) / (2.0 * sigma * sigma)).exp())
            .collect();
        let sum: f32 = raw.iter().sum();
        raw.into_iter().map(|v| v / sum).collect()
    };

    let mut tmp = vec![0f32; buf.len()];
    let sample = |buf: &[f32], x: i32, y: i32, c: u32| -> f32 {
        let x = x.clamp(0, w as i32 - 1) as u32;
        let y = y.clamp(0, h as i32 - 1) as u32;
        buf[((y * w + x) * 3 + c) as usize]
    };

    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let mut acc = 0.0;
                for (k, weight) in kernel.iter().enumerate() {
                    acc += weight * sample(buf, x as i32 + k as i32 - radius, y as i32, c);
                }
                tmp[((y * w + x) * 3 + c) as usize] = acc;
            }
        }
    }
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let mut acc = 0.0;
                for (k, weight) in kernel.iter().enumerate() {
                    acc += weight * sample(&tmp, x as i32, y as i32 + k as i32 - radius, c);
                }
                buf[((y * w + x) * 3 + c) as usize] = acc;
            }
        }
    }
}

/// Standard normal via Box–Muller. Avoids a dependency for five lines of maths.
fn normal(rng: &mut impl Rng) -> f32 {
    let u1: f32 = rng.random_range(f32::MIN_POSITIVE..1.0);
    let u2: f32 = rng.random_range(0.0..1.0);
    (-2.0f32 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn checkerboard(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| {
            if (x / 8 + y / 8) % 2 == 0 {
                Rgb([255, 255, 255])
            } else {
                Rgb([0, 0, 0])
            }
        })
    }

    #[test]
    fn none_preset_is_a_true_identity() {
        let image = checkerboard(64, 64);
        let mut rng = StdRng::seed_from_u64(1);
        let out = apply(&image, &Channel::preset(Preset::None), 8, &mut rng);
        assert_eq!(out, image);
    }

    #[test]
    fn distortion_is_deterministic_for_a_given_seed() {
        let image = checkerboard(64, 64);
        let channel = Channel::preset(Preset::Heavy);
        let a = apply(&image, &channel, 8, &mut StdRng::seed_from_u64(42));
        let b = apply(&image, &channel, 8, &mut StdRng::seed_from_u64(42));
        assert_eq!(a, b, "same seed must give the same frame, or CI is flaky");
    }

    /// The presets must be ordered by how much damage they do. If this ever
    /// fails, a preset has drifted and every test that depends on one is
    /// quietly measuring something else.
    #[test]
    fn presets_are_monotonically_worse() {
        let image = checkerboard(64, 64);
        let severities: Vec<f64> = [Preset::None, Preset::Light, Preset::Heavy, Preset::Brutal]
            .iter()
            .map(|&preset| {
                let out = apply(
                    &image,
                    &Channel::preset(preset),
                    8,
                    &mut StdRng::seed_from_u64(7),
                );
                mean_drift(&image, &out)
            })
            .collect();
        for pair in severities.windows(2) {
            assert!(pair[1] > pair[0], "presets not ordered: {severities:?}");
        }
    }

    /// Heavy must actually be heavy. If this ever passes trivially, the preset
    /// has drifted and the loss tests stop proving anything.
    #[test]
    fn heavy_visibly_degrades_the_image() {
        let image = checkerboard(64, 64);
        let mut rng = StdRng::seed_from_u64(7);
        let out = apply(&image, &Channel::preset(Preset::Heavy), 8, &mut rng);

        let drift = mean_drift(&image, &out);
        assert!(drift > 20.0, "mean per-channel drift was only {drift:.1}");
    }

    /// A window that never crosses a flip reproduces its pulse bit-exactly.
    #[test]
    fn timed_window_inside_one_pulse_is_clean() {
        let a = checkerboard(64, 64);
        let b = RgbImage::from_pixel(64, 64, Rgb([255, 0, 0]));
        let shutter = Shutter {
            readout: 0.002,
            exposure: 0.001,
        };
        let mut rng = StdRng::seed_from_u64(3);
        // Window [0.004, 0.007] sits inside pulse 0's [0, 0.1).
        let out = capture_timed(
            &[&a, &b],
            0.1,
            0.005,
            &shutter,
            &Channel::preset(Preset::None),
            8,
            &mut rng,
        );
        assert_eq!(out, a);
    }

    /// A flip inside the readout stitches the frame at the corresponding rows:
    /// top rows read before the flip see pulse 0, bottom rows see pulse 1.
    #[test]
    fn timed_flip_mid_readout_stitches_by_row() {
        let a = RgbImage::from_pixel(16, 16, Rgb([255, 255, 255]));
        let b = RgbImage::from_pixel(16, 16, Rgb([0, 0, 0]));
        let shutter = Shutter {
            readout: 0.010,
            exposure: 0.0,
        };
        let mut rng = StdRng::seed_from_u64(3);
        // Rows read over [0.015, 0.025]; the flip is at 0.020 — halfway down.
        let out = capture_timed(
            &[&a, &b],
            0.020,
            0.015,
            &shutter,
            &Channel::preset(Preset::None),
            8,
            &mut rng,
        );
        assert_eq!(out.get_pixel(8, 0).0, [255, 255, 255], "top is pulse 0");
        assert_eq!(out.get_pixel(8, 15).0, [0, 0, 0], "bottom is pulse 1");
        let boundary = (0..16)
            .find(|&y| out.get_pixel(8, y).0 == [0, 0, 0])
            .unwrap();
        assert!(
            (6..=9).contains(&boundary),
            "tear line at row {boundary}, expected near the middle"
        );
    }

    /// Mean absolute per-channel difference, 0–255.
    fn mean_drift(a: &RgbImage, b: &RgbImage) -> f64 {
        a.pixels()
            .zip(b.pixels())
            .map(|(p, q)| {
                (0..3)
                    .map(|c| (p.0[c] as f64 - q.0[c] as f64).abs())
                    .sum::<f64>()
            })
            .sum::<f64>()
            / (a.pixels().len() * 3) as f64
    }
}
