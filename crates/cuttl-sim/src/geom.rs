//! Projective geometry for registration (`DESIGN.md` §3a).
//!
//! Perspective is the *easy* part of the eye's job, and it is worth being clear
//! why: a pulse is planar and a camera is very nearly a pinhole, so the mapping
//! from cell space to image space is exactly a homography — 8 degrees of
//! freedom, fully determined by four point correspondences. Not an
//! approximation that degrades with angle; exact.
//!
//! What this does *not* cover is rolling-shutter skew, which is a per-row,
//! time-varying transform and so cannot be a single homography at all. That is
//! why tear gets its own detection mechanism (duplicated beacons) rather than
//! being folded in here.

/// A 3×3 projective transform, row-major, normalised so `m[8] == 1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Homography([f64; 9]);

impl Homography {
    /// Uniform scale — the axis-aligned case, where cell space maps to image
    /// space by nothing more than `cell_px`.
    pub fn scale(factor: f64) -> Self {
        Self([factor, 0.0, 0.0, 0.0, factor, 0.0, 0.0, 0.0, 1.0])
    }

    /// Solve for the transform taking each `src` point to the matching `dst`.
    ///
    /// Direct linear transform: each correspondence contributes two equations,
    /// four give the eight unknowns exactly. `None` if the points are
    /// degenerate — three collinear, say — which is the honest answer when the
    /// detector has found four things that cannot be a quad.
    pub fn from_correspondences(src: [(f64, f64); 4], dst: [(f64, f64); 4]) -> Option<Self> {
        let mut a = [[0.0f64; 9]; 8];
        for i in 0..4 {
            let (x, y) = src[i];
            let (u, v) = dst[i];
            a[i * 2] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
            a[i * 2 + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
        }
        let solution = solve8(&mut a)?;
        let mut m = [0.0; 9];
        m[..8].copy_from_slice(&solution);
        m[8] = 1.0;
        Some(Self(m))
    }

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let m = &self.0;
        let w = m[6] * x + m[7] * y + m[8];
        if w.abs() < f64::EPSILON {
            return (f64::NAN, f64::NAN);
        }
        (
            (m[0] * x + m[1] * y + m[2]) / w,
            (m[3] * x + m[4] * y + m[5]) / w,
        )
    }

    pub fn inverse(&self) -> Option<Self> {
        let m = &self.0;
        let c = [
            m[4] * m[8] - m[5] * m[7],
            m[2] * m[7] - m[1] * m[8],
            m[1] * m[5] - m[2] * m[4],
            m[5] * m[6] - m[3] * m[8],
            m[0] * m[8] - m[2] * m[6],
            m[2] * m[3] - m[0] * m[5],
            m[3] * m[7] - m[4] * m[6],
            m[1] * m[6] - m[0] * m[7],
            m[0] * m[4] - m[1] * m[3],
        ];
        let det = m[0] * c[0] + m[1] * c[3] + m[2] * c[6];
        if det.abs() < 1e-12 || c[8].abs() < 1e-12 {
            return None;
        }
        // Renormalise so m[8] == 1 again; scale is irrelevant projectively.
        let mut out = [0.0; 9];
        for i in 0..9 {
            out[i] = c[i] / c[8];
        }
        Some(Self(out))
    }
}

/// Gaussian elimination with partial pivoting on an 8×9 augmented matrix.
fn solve8(a: &mut [[f64; 9]; 8]) -> Option<[f64; 8]> {
    for col in 0..8 {
        let pivot = (col..8).max_by(|&i, &j| {
            a[i][col]
                .abs()
                .partial_cmp(&a[j][col].abs())
                .unwrap_or(core::cmp::Ordering::Equal)
        })?;
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        let diag = a[col][col];
        for value in a[col][col..].iter_mut() {
            *value /= diag;
        }
        let pivot_row = a[col];
        for (row, equation) in a.iter_mut().enumerate() {
            if row == col {
                continue;
            }
            let factor = equation[col];
            if factor == 0.0 {
                continue;
            }
            for (k, value) in equation.iter_mut().enumerate().skip(col) {
                *value -= factor * pivot_row[k];
            }
        }
    }
    let mut out = [0.0; 8];
    for (i, value) in out.iter_mut().enumerate() {
        *value = a[i][8];
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQUARE: [(f64, f64); 4] = [(0.0, 0.0), (10.0, 0.0), (0.0, 10.0), (10.0, 10.0)];

    fn close(a: (f64, f64), b: (f64, f64), tol: f64) -> bool {
        (a.0 - b.0).abs() < tol && (a.1 - b.1).abs() < tol
    }

    #[test]
    fn recovers_a_known_scale() {
        let dst = SQUARE.map(|(x, y)| (x * 3.0, y * 3.0));
        let h = Homography::from_correspondences(SQUARE, dst).unwrap();
        assert!(close(h.apply(5.0, 5.0), (15.0, 15.0), 1e-9));
    }

    /// The property that matters: any quad, exactly hit at all four corners.
    #[test]
    fn maps_corners_exactly_for_a_perspective_quad() {
        let dst = [(12.0, 7.0), (98.0, 21.0), (5.0, 88.0), (110.0, 103.0)];
        let h = Homography::from_correspondences(SQUARE, dst).unwrap();
        for (src, want) in SQUARE.iter().zip(dst.iter()) {
            assert!(
                close(h.apply(src.0, src.1), *want, 1e-6),
                "corner {src:?} landed at {:?}, wanted {want:?}",
                h.apply(src.0, src.1)
            );
        }
    }

    #[test]
    fn inverse_undoes_the_forward_map() {
        let dst = [(12.0, 7.0), (98.0, 21.0), (5.0, 88.0), (110.0, 103.0)];
        let h = Homography::from_correspondences(SQUARE, dst).unwrap();
        let inv = h.inverse().unwrap();
        for (x, y) in [(0.0, 0.0), (3.3, 7.1), (10.0, 10.0), (5.5, 2.2)] {
            let (u, v) = h.apply(x, y);
            assert!(close(inv.apply(u, v), (x, y), 1e-6));
        }
    }

    #[test]
    fn degenerate_points_are_rejected_not_fudged() {
        let collinear = [(0.0, 0.0), (1.0, 1.0), (2.0, 2.0), (3.0, 3.0)];
        assert!(Homography::from_correspondences(SQUARE, collinear).is_none());
    }
}
