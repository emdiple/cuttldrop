//! A borrowed view of an 8-bit RGB image.
//!
//! This exists so the eye-side pipeline can live in this crate rather than in
//! `cuttl-sim`. The simulator has an `image` crate buffer, the browser has a
//! `Uint8ClampedArray` from a canvas or a `VideoFrame`, and neither can depend
//! on the other. Both are contiguous RGB bytes, so both become a [`Raster`] for
//! free — no copy, no image library, and it compiles to `wasm32`.
//!
//! Deliberately borrowed and read-only. Nothing here owns pixels; the caller
//! keeps whatever buffer it already had.

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub struct Raster<'a> {
    width: u32,
    height: u32,
    data: &'a [u8],
}

impl<'a> Raster<'a> {
    /// `data` must be exactly `width × height × 3` bytes, RGB, row-major.
    pub fn new(width: u32, height: u32, data: &'a [u8]) -> Result<Self> {
        let wanted = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(3));
        if wanted != Some(data.len()) || width == 0 || height == 0 {
            return Err(Error::RasterSize {
                width,
                height,
                len: data.len(),
            });
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let i = ((y.min(self.height - 1) * self.width + x.min(self.width - 1)) * 3) as usize;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    /// Rec.601 luma, fixed point.
    pub fn luma(&self, x: u32, y: u32) -> u8 {
        let p = self.pixel(x, y);
        ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8
    }

    /// Bilinear sample, clamped at the edges.
    ///
    /// Cell centres land on fractional pixels under any real perspective, so
    /// rounding them would throw away the sub-pixel accuracy the homography
    /// just worked out.
    ///
    /// **Coordinates are edge space**, not pixel indices: pixel `i` occupies
    /// `[i, i+1)`, so its centre is `i + 0.5`. That is the convention the
    /// homography and the finder detector both produce, and having one
    /// convention everywhere is the point — mixing them is a half-pixel error
    /// that stays invisible while cells are uniform and only shows up when a
    /// cell is one pixel wide.
    pub fn bilinear(&self, x: f64, y: f64) -> [u8; 3] {
        let x = (x - 0.5).clamp(0.0, self.width as f64 - 1.0);
        let y = (y - 0.5).clamp(0.0, self.height as f64 - 1.0);
        let (x0, y0) = (x.floor() as u32, y.floor() as u32);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (fx, fy) = (x - x0 as f64, y - y0 as f64);

        let (p00, p10) = (self.pixel(x0, y0), self.pixel(x1, y0));
        let (p01, p11) = (self.pixel(x0, y1), self.pixel(x1, y1));

        let mut out = [0u8; 3];
        for (c, value) in out.iter_mut().enumerate() {
            let top = p00[c] as f64 * (1.0 - fx) + p10[c] as f64 * fx;
            let bottom = p01[c] as f64 * (1.0 - fx) + p11[c] as f64 * fx;
            *value = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_buffers_that_do_not_match_the_dimensions() {
        assert!(Raster::new(2, 2, &[0; 11]).is_err());
        assert!(Raster::new(2, 2, &[0; 13]).is_err());
        assert!(Raster::new(0, 2, &[]).is_err());
        assert!(Raster::new(2, 2, &[0; 12]).is_ok());
    }

    /// Edge-space coordinates: pixel centres are at 0.5 and 1.5, and the
    /// boundary between them at 1.0 is the halfway blend.
    #[test]
    fn bilinear_interpolates_between_neighbours() {
        // Two pixels: black then white.
        let data = [0, 0, 0, 255, 255, 255];
        let raster = Raster::new(2, 1, &data).unwrap();
        assert_eq!(raster.bilinear(0.5, 0.5), [0, 0, 0], "centre of pixel 0");
        assert_eq!(
            raster.bilinear(1.5, 0.5),
            [255, 255, 255],
            "centre of pixel 1"
        );
        assert_eq!(raster.bilinear(1.0, 0.5), [128, 128, 128], "the boundary");
    }

    /// Sampling a one-pixel-wide cell must land exactly on that pixel and not
    /// bleed into its neighbour. This is the case the half-pixel bug broke.
    #[test]
    fn unit_wide_cells_do_not_bleed() {
        let data = [0, 0, 0, 255, 255, 255, 0, 0, 0];
        let raster = Raster::new(3, 1, &data).unwrap();
        for (index, want) in [(0u32, 0u8), (1, 255), (2, 0)] {
            let centre = index as f64 + 0.5;
            assert_eq!(raster.bilinear(centre, 0.5)[0], want, "pixel {index}");
        }
    }

    #[test]
    fn sampling_outside_the_frame_clamps_rather_than_panicking() {
        let data = [10, 20, 30, 40, 50, 60];
        let raster = Raster::new(2, 1, &data).unwrap();
        assert_eq!(raster.bilinear(-5.0, -5.0), [10, 20, 30]);
        assert_eq!(raster.bilinear(99.0, 99.0), [40, 50, 60]);
        assert_eq!(raster.pixel(99, 99), [40, 50, 60]);
    }
}
