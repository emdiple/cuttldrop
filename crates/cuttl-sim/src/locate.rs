//! Finding the pulse in a camera frame (`DESIGN.md` §3a, §9).
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

use crate::geom::Homography;
use cuttl_codec::Grid;
use image::RgbImage;

/// Fraction of the frame, measured from each edge, in which a finder centre is
/// allowed to sit. Generous: it only has to exclude the middle.
const CORNER_BAND: f64 = 0.40;

/// How far a run-length may stray from its ideal share of the 1:1:3:1:1 pattern.
const RATIO_TOLERANCE: f64 = 0.5;

/// Locate the four finders and solve for the cell-space → image-space
/// transform. `None` when fewer than four corners are confidently found, which
/// the caller must treat as an unreadable frame — an erasure, not an error.
pub fn locate(image: &RgbImage, grid: Grid) -> Option<Homography> {
    let (w, h) = image.dimensions();
    let luma = luma_of(image);
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

fn luma_of(image: &RgbImage) -> Vec<u8> {
    image
        .pixels()
        .map(|p| ((p.0[0] as u32 * 77 + p.0[1] as u32 * 150 + p.0[2] as u32 * 29) >> 8) as u8)
        .collect()
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
