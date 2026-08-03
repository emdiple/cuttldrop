//! The eye side: find the pulse in a frame, then read its cells (§3a, §9).
//!
//! This lives in the codec crate rather than the simulator because the browser
//! needs exactly the same code. Everything here works on a borrowed
//! [`Raster`] — no image library, no allocation of pixel buffers — so it
//! compiles to `wasm32` unchanged.
//!
//! This is the step HCCB died on. Its triangular cells were dense but poor for
//! robust corner localisation, and the format never recovered from it. So the
//! approach here is deliberately unoriginal: QR's concentric-square finders,
//! located by the same 1:1:3:1:1 run-length scan QR uses, then four
//! correspondences into a homography.
//!
//! ```text
//! scan a line through a finder's centre:
//!
//!   ██  ░░  ██████  ░░  ██        runs:  1 : 1 : 3 : 1 : 1
//!   ring gap  core  gap ring
//! ```
//!
//! The ratio is what makes it distinctive — and it is why the finder must be
//! 7 cells wide. A 5-wide concentric square scans as 1:1:1:1:1, which random
//! payload produces constantly.
//!
//! Two structural priors keep false positives down, both legitimate because we
//! designed the thing we are looking for: a candidate must be confirmed by both
//! a horizontal and a vertical scan, and it must lie in the outer part of the
//! frame, because that is where corners are.

use crate::error::{Error, Result};
use crate::geom::Homography;
use crate::geometry::Grid;
use crate::palette::Palette;
use crate::pulse::Pulse;
use crate::raster::Raster;

/// Fraction of the frame, measured from each edge, in which a finder centre is
/// allowed to sit. Generous: it only has to exclude the middle.
const CORNER_BAND: f64 = 0.40;

/// How far a run-length may stray from its ideal share of the 1:1:3:1:1 pattern.
const RATIO_TOLERANCE: f64 = 0.5;

/// Locate the four finders and solve for the cell-space → image-space
/// transform. `None` when fewer than four corners are confidently found, which
/// the caller must treat as an unreadable frame — an erasure, not an error.
pub fn locate(raster: &Raster, grid: Grid) -> Option<Homography> {
    let (w, h) = (raster.width(), raster.height());
    let luma = luma_of(raster);
    let threshold = otsu(&luma);

    let mut horizontal = Vec::new();
    for y in 0..h {
        let row: Vec<bool> = (0..w)
            .map(|x| luma[(y * w + x) as usize] > threshold)
            .collect();
        for centre in scan(&row) {
            // +0.5: pixel `y` spans [y, y+1), same convention as cell centres.
            horizontal.push((centre, y as f64 + 0.5));
        }
    }

    let mut vertical = Vec::new();
    for x in 0..w {
        let col: Vec<bool> = (0..h)
            .map(|y| luma[(y * w + x) as usize] > threshold)
            .collect();
        for centre in scan(&col) {
            vertical.push((x as f64 + 0.5, centre));
        }
    }

    // A finder shows up in both sweeps; payload noise rarely does.
    let tolerance = (w.min(h) as f64 * 0.02).max(2.0);
    let across = cluster(&horizontal, tolerance);
    let down = cluster(&vertical, tolerance);

    let mut centres: Vec<(f64, f64)> = across
        .iter()
        .filter(|&&(hx, hy)| {
            down.iter()
                .any(|&(vx, vy)| (hx - vx).hypot(hy - vy) < tolerance * 3.0)
        })
        .copied()
        .filter(|&(x, y)| in_corner_band(x, y, w as f64, h as f64))
        .collect();

    if centres.len() < 4 {
        return None;
    }
    // More than four means noise survived; keep the four most extreme, which
    // are the ones actually at the corners.
    centres = pick_corners(&centres)?;

    Homography::from_correspondences(
        grid.finder_centres(),
        [centres[0], centres[1], centres[2], centres[3]],
    )
}

fn in_corner_band(x: f64, y: f64, w: f64, h: f64) -> bool {
    let near_x = x < w * CORNER_BAND || x > w * (1.0 - CORNER_BAND);
    let near_y = y < h * CORNER_BAND || y > h * (1.0 - CORNER_BAND);
    near_x && near_y
}

/// Order four points as top-left, top-right, bottom-left, bottom-right — the
/// order [`Grid::finder_centres`] uses.
fn pick_corners(points: &[(f64, f64)]) -> Option<Vec<(f64, f64)>> {
    let pick = |f: &dyn Fn(&(f64, f64)) -> f64| -> Option<(f64, f64)> {
        points.iter().copied().min_by(|a, b| {
            f(a).partial_cmp(&f(b))
                .unwrap_or(core::cmp::Ordering::Equal)
        })
    };
    let tl = pick(&|p| p.0 + p.1)?;
    let br = pick(&|p| -(p.0 + p.1))?;
    let tr = pick(&|p| -(p.0 - p.1))?;
    let bl = pick(&|p| p.0 - p.1)?;

    let corners = vec![tl, tr, bl, br];
    // Four *distinct* points, or the "quad" is degenerate and the homography
    // would be meaningless.
    for i in 0..corners.len() {
        for j in i + 1..corners.len() {
            if (corners[i].0 - corners[j].0).hypot(corners[i].1 - corners[j].1) < 1.0 {
                return None;
            }
        }
    }
    Some(corners)
}

/// Centres of every 1:1:3:1:1 run pattern along one scan line.
fn scan(line: &[bool]) -> Vec<f64> {
    let mut runs: Vec<(bool, usize, usize)> = Vec::new();
    let mut start = 0usize;
    for i in 1..=line.len() {
        if i == line.len() || line[i] != line[start] {
            runs.push((line[start], start, i - start));
            start = i;
        }
    }

    let mut centres = Vec::new();
    for window in runs.windows(5) {
        // The pattern is light-dark-light-dark-light: outer ring, gap, core,
        // gap, ring. Runs alternate by construction, so checking the first is
        // enough to fix the polarity.
        if !window[0].0 {
            continue;
        }
        let lengths: [f64; 5] = [
            window[0].2 as f64,
            window[1].2 as f64,
            window[2].2 as f64,
            window[3].2 as f64,
            window[4].2 as f64,
        ];
        let unit = lengths.iter().sum::<f64>() / 7.0;
        if unit < 1.0 {
            continue;
        }
        let expected = [1.0, 1.0, 3.0, 1.0, 1.0];
        if lengths
            .iter()
            .zip(expected.iter())
            .any(|(&len, &want)| (len / unit - want).abs() > RATIO_TOLERANCE * want)
        {
            continue;
        }
        centres.push(window[2].1 as f64 + window[2].2 as f64 / 2.0);
    }
    centres
}

/// Merge nearby hits into one point each, dropping singletons as noise.
///
/// **Single linkage** — a point joins a cluster if it is close to *any* member,
/// not to the running centroid. That distinction is not academic: a finder
/// produces a contiguous line of hits, one per scan line through its core, and
/// comparing against a lagging centroid splits that line into several clusters
/// whose centres sit a whole cell away from the truth. Which then biases the
/// homography, which then misreads every cell in the pulse.
fn cluster(points: &[(f64, f64)], tolerance: f64) -> Vec<(f64, f64)> {
    let mut clusters: Vec<Vec<(f64, f64)>> = Vec::new();

    for &point in points {
        let mut touching: Vec<usize> = clusters
            .iter()
            .enumerate()
            .filter(|(_, members)| {
                members
                    .iter()
                    .any(|m| (m.0 - point.0).hypot(m.1 - point.1) <= tolerance)
            })
            .map(|(i, _)| i)
            .collect();

        let Some(first) = touching.first().copied() else {
            clusters.push(vec![point]);
            continue;
        };
        clusters[first].push(point);
        // A new point can bridge two clusters; fold them together. Indices are
        // ascending and `first` is the smallest, so removing from the back is
        // safe.
        touching.remove(0);
        for &index in touching.iter().rev() {
            let merged = clusters.remove(index);
            clusters[first].extend(merged);
        }
    }

    clusters
        .into_iter()
        .filter(|members| members.len() >= 2)
        .map(|members| {
            let n = members.len() as f64;
            (
                members.iter().map(|p| p.0).sum::<f64>() / n,
                members.iter().map(|p| p.1).sum::<f64>() / n,
            )
        })
        .collect()
}

fn luma_of(raster: &Raster) -> Vec<u8> {
    let (w, h) = (raster.width(), raster.height());
    let mut out = Vec::with_capacity((w as usize) * (h as usize));
    for y in 0..h {
        for x in 0..w {
            out.push(raster.luma(x, y));
        }
    }
    out
}

/// Sample every cell centre through a known cell-space → image-space transform.
pub fn sample_with(
    raster: &Raster,
    grid: Grid,
    palette: Palette,
    transform: &Homography,
) -> Result<Pulse> {
    sample_corrected(raster, grid, palette, transform, &[])
}

/// One interior reference point: where an alignment pattern should be in cell
/// space, and where the frame says it actually is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    /// Cell-space centre. Fixed by the grid; the skin and eye agree on it.
    pub cell: (f64, f64),
    /// Image-space centre as measured. The difference between this and
    /// `transform.apply(cell)` is the residual the homography could not model.
    pub image: (f64, f64),
}

/// Minimum separation, in luma, between the cells an alignment pattern says
/// should be light and the ones it says should be dark. Below this the window
/// holds no pattern worth trusting — glare, blur, or the prediction was simply
/// too far out — and the anchor is dropped rather than guessed at.
const ANCHOR_CONTRAST: f64 = 18.0;

/// Locate the interior alignment patterns, starting from the corner homography.
///
/// This is a *local* search around a position the homography already predicts,
/// which is why an alignment pattern needs no distinctive run-length ratio and
/// why this cannot suffer the candidate explosion a global scan can. Patterns
/// that cannot be found confidently are simply absent from the result; the
/// correction interpolates across them.
pub fn find_anchors(raster: &Raster, grid: Grid, transform: &Homography) -> Vec<Anchor> {
    let half = crate::geometry::ALIGN_SIDE as i32 / 2;
    let mut anchors = Vec::new();

    for (cx, cy) in grid.alignment_centres() {
        let (fx, fy) = (cx as f64 + 0.5, cy as f64 + 0.5);
        let predicted = transform.apply(fx, fy);
        // Local basis: what one cell step looks like in image space *here*.
        // Taken per pattern rather than globally because under perspective a
        // cell is a different size at each end of the frame.
        let right = transform.apply(fx + 1.0, fy);
        let down = transform.apply(fx, fy + 1.0);
        let ex = (right.0 - predicted.0, right.1 - predicted.1);
        let ey = (down.0 - predicted.0, down.1 - predicted.1);
        let cell_px = (ex.0.hypot(ex.1) + ey.0.hypot(ey.1)) / 2.0;
        // Below ~2 px/cell the pattern is not resolvable and any peak found
        // would be noise. That is also where the payload has already failed.
        if !cell_px.is_finite() || cell_px < 2.0 {
            continue;
        }

        // Score a candidate centre by how cleanly the 25 cells split into the
        // light ring plus core and the dark gap. A contrast score needs no
        // threshold, so vignette and ambient lift cannot bias it.
        let score = |ox: f64, oy: f64| -> f64 {
            let (mut on, mut off, mut on_n, mut off_n) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
            for j in -half..=half {
                for i in -half..=half {
                    let px = ox + ex.0 * i as f64 + ey.0 * j as f64;
                    let py = oy + ex.1 * i as f64 + ey.1 * j as f64;
                    let ring = i.abs().max(j.abs());
                    let luma = f64::from(luma_at(raster, px, py));
                    // Same concentric rule the skin paints with: outer ring and
                    // core light, the gap between them dark.
                    if ring == half || ring + 2 <= half {
                        on += luma;
                        on_n += 1.0;
                    } else {
                        off += luma;
                        off_n += 1.0;
                    }
                }
            }
            on / on_n.max(1.0) - off / off_n.max(1.0)
        };

        // Coarse pass on whole pixels, then a sub-pixel pass around the winner.
        // The radius is under a cell: the corner fit is wrong by residuals, not
        // by whole cells, and a wider search would only invite a false peak.
        let radius = (cell_px * 0.75).round().clamp(2.0, 10.0) as i32;
        let mut best = (score(predicted.0, predicted.1), predicted);
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                let candidate = (predicted.0 + dx as f64, predicted.1 + dy as f64);
                let s = score(candidate.0, candidate.1);
                if s > best.0 {
                    best = (s, candidate);
                }
            }
        }
        for step in 1..=4 {
            let d = 0.5f64.powi(step);
            let mut improved = best;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let candidate = (best.1.0 + dx as f64 * d, best.1.1 + dy as f64 * d);
                    let s = score(candidate.0, candidate.1);
                    if s > improved.0 {
                        improved = (s, candidate);
                    }
                }
            }
            best = improved;
        }

        if best.0 >= ANCHOR_CONTRAST {
            anchors.push(Anchor {
                cell: (fx, fy),
                image: best.1,
            });
        }
    }
    anchors
}

fn luma_at(raster: &Raster, x: f64, y: f64) -> u8 {
    let [r, g, b] = raster.bilinear(x, y);
    // Same weights as Raster::luma, applied to an interpolated sample.
    ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000) as u8
}

/// Sample every cell, correcting the homography by the measured anchors.
///
/// The correction is an inverse-distance-weighted interpolation of the anchor
/// residuals, evaluated in *cell* space. That is the same choice §3b makes for
/// colour and for the same reason: the error being corrected is spatially
/// varying, so a single global fit is the wrong shape for it. With no anchors
/// this is exactly [`sample_with`].
pub fn sample_corrected(
    raster: &Raster,
    grid: Grid,
    palette: Palette,
    transform: &Homography,
    anchors: &[Anchor],
) -> Result<Pulse> {
    let mut pulse = Pulse::new(grid, palette)?;

    // Precompute each anchor's residual once: measured minus predicted.
    let residuals: Vec<Residual> = anchors
        .iter()
        .map(|a| {
            let p = transform.apply(a.cell.0, a.cell.1);
            (a.cell, (a.image.0 - p.0, a.image.1 - p.1))
        })
        .collect();

    for y in 0..grid.rows {
        for x in 0..grid.cols {
            let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
            let (mut ix, mut iy) = transform.apply(fx, fy);
            let (dx, dy) = residual_at(&residuals, fx, fy);
            ix += dx;
            iy += dy;
            pulse.set_cell(x, y, palette.from_rgb(raster.bilinear(ix, iy)))?;
        }
    }
    Ok(pulse)
}

/// An anchor's cell-space position paired with the image-space error the
/// homography made there.
type Residual = ((f64, f64), (f64, f64));

/// Inverse-distance-weighted residual at a point in cell space.
///
/// Weight is `1/d²`, which makes the interpolant reproduce each anchor exactly
/// at its own centre and decay quickly enough that a distant anchor on the far
/// side of the frame cannot drag a local correction around.
fn residual_at(residuals: &[Residual], x: f64, y: f64) -> (f64, f64) {
    if residuals.is_empty() {
        return (0.0, 0.0);
    }
    let (mut sx, mut sy, mut sw) = (0.0f64, 0.0f64, 0.0f64);
    for &((ax, ay), (rx, ry)) in residuals {
        let d2 = (x - ax).powi(2) + (y - ay).powi(2);
        if d2 < 1e-6 {
            return (rx, ry);
        }
        let w = 1.0 / d2;
        sx += rx * w;
        sy += ry * w;
        sw += w;
    }
    (sx / sw, sy / sw)
}

/// The eye's real entry point: locate the pulse, then read it.
///
/// A frame whose finders cannot be found is an *erasure* — count it and move
/// on, exactly as for a CRC reject (§1b). The skin is looping; there will be
/// another frame.
pub fn read(raster: &Raster, grid: Grid, palette: Palette) -> Result<Pulse> {
    let transform = locate(raster, grid).ok_or(Error::NotLocated)?;
    let anchors = find_anchors(raster, grid, &transform);
    sample_corrected(raster, grid, palette, &transform, &anchors)
}

/// Read a frame that is known to be axis-aligned and to fill the raster exactly.
///
/// A shortcut for testing the codec without the optics in the way — wrong the
/// moment there is any perspective, which is why [`read`] is the real path.
pub fn sample_aligned(raster: &Raster, grid: Grid, palette: Palette) -> Result<Pulse> {
    let (w, h) = (raster.width(), raster.height());
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
    sample_with(raster, grid, palette, &Homography::scale(cell_px as f64))
}

/// Otsu's method. A fixed threshold would be wrong under vignette and ambient
/// lift, both of which the channel models and a real room supplies.
fn otsu(luma: &[u8]) -> u8 {
    let mut histogram = [0u32; 256];
    for &value in luma {
        histogram[value as usize] += 1;
    }
    let total = luma.len() as f64;
    let sum: f64 = histogram
        .iter()
        .enumerate()
        .map(|(level, &count)| level as f64 * count as f64)
        .sum();

    let (mut sum_below, mut weight_below, mut best, mut threshold) = (0.0, 0.0, -1.0, 0u8);
    for (level, &count) in histogram.iter().enumerate() {
        weight_below += count as f64;
        if weight_below == 0.0 {
            continue;
        }
        let weight_above = total - weight_below;
        if weight_above == 0.0 {
            break;
        }
        sum_below += level as f64 * count as f64;
        let mean_below = sum_below / weight_below;
        let mean_above = (sum - sum_below) / weight_above;
        let variance = weight_below * weight_above * (mean_below - mean_above).powi(2);
        if variance > best {
            best = variance;
            threshold = level as u8;
        }
    }
    threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs_to_line(spec: &[(bool, usize)]) -> Vec<bool> {
        spec.iter()
            .flat_map(|&(value, len)| std::iter::repeat_n(value, len))
            .collect()
    }

    #[test]
    fn finds_an_ideal_finder_scan() {
        // 1:1:3:1:1 at four pixels per cell, padded with dark either side.
        let line = runs_to_line(&[
            (false, 20),
            (true, 4),
            (false, 4),
            (true, 12),
            (false, 4),
            (true, 4),
            (false, 20),
        ]);
        let centres = scan(&line);
        assert_eq!(centres.len(), 1);
        assert!((centres[0] - 34.0).abs() < 0.5, "got {centres:?}");
    }

    /// The exact reason the finder had to grow from 5 cells to 7.
    #[test]
    fn rejects_the_five_wide_pattern_that_forced_the_redesign() {
        let line = runs_to_line(&[
            (false, 20),
            (true, 4),
            (false, 4),
            (true, 4), // a 5-wide finder's core: 1:1:1:1:1, not 1:1:3:1:1
            (false, 4),
            (true, 4),
            (false, 20),
        ]);
        assert!(scan(&line).is_empty());
    }

    #[test]
    fn ignores_inverted_polarity() {
        let line = runs_to_line(&[
            (true, 20),
            (false, 4),
            (true, 4),
            (false, 12),
            (true, 4),
            (false, 4),
            (true, 20),
        ]);
        assert!(scan(&line).is_empty());
    }

    #[test]
    fn clustering_drops_lone_hits() {
        let points = [(10.0, 10.0), (10.5, 10.2), (200.0, 5.0)];
        let clustered = cluster(&points, 3.0);
        assert_eq!(clustered.len(), 1);
        assert!((clustered[0].0 - 10.25).abs() < 0.1);
    }

    #[test]
    fn corners_come_back_in_grid_order() {
        let points = [(90.0, 88.0), (11.0, 9.0), (92.0, 10.0), (9.0, 91.0)];
        let corners = pick_corners(&points).unwrap();
        assert_eq!(corners[0], (11.0, 9.0), "top-left");
        assert_eq!(corners[1], (92.0, 10.0), "top-right");
        assert_eq!(corners[2], (9.0, 91.0), "bottom-left");
        assert_eq!(corners[3], (90.0, 88.0), "bottom-right");
    }
}
