//! Chroma cell palettes (`DESIGN.md` §3b).
//!
//! The colour palette is the eight corners of the RGB cube — R, G and B treated
//! as three independent binary subchannels, 3 bits per chroma cell. This is the
//! same eight colours as JAB Code's 8-colour mode (CMY + RGB + K + W), reached
//! from the opposite direction: additive rather than subtractive.
//!
//! Deliberately **no 6-bit / 64-colour mode**. That needs four amplitude levels
//! per channel, and amplitude is the most fragile dimension we have (§1c).

/// How many bits each chroma cell carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Palette {
    /// 1 bit per cell, black/white. The M1 palette — robust, and the baseline
    /// the M3 colour A/B toggle is measured against.
    #[default]
    Mono1,
    /// 3 bits per cell, RGB cube corners. Lands at M3.
    Color3,
}

impl Palette {
    pub const fn bits_per_cell(self) -> u32 {
        match self {
            Palette::Mono1 => 1,
            Palette::Color3 => 3,
        }
    }

    /// Number of distinct cell values: 2 for `Mono1`, 8 for `Color3`.
    pub const fn levels(self) -> u8 {
        1u8 << self.bits_per_cell()
    }

    /// The value rendered as "white" — all subchannels on.
    pub const fn max_value(self) -> u8 {
        self.levels() - 1
    }

    /// The cell value carrying a single bit.
    ///
    /// The beacon uses this rather than the full palette because it has to be
    /// readable *before* colour calibration is possible — the eye needs the
    /// stream id and pulse counter to interpret anything, and it has not fitted
    /// the pilots yet. One bit per cell is the only thing that works with no
    /// calibration at all (§3b).
    pub const fn bit_value(self, bit: bool) -> u8 {
        if bit { self.max_value() } else { 0 }
    }

    /// Read a cell back as a single bit.
    ///
    /// In colour mode this is a majority vote across the three subchannels, so
    /// a beacon cell survives one channel being misread entirely — free
    /// redundancy that falls out of not using the palette's full alphabet.
    pub const fn to_bit(self, value: u8) -> bool {
        match self {
            Palette::Mono1 => value != 0,
            Palette::Color3 => value.count_ones() >= 2,
        }
    }

    /// Cell value → sRGB. Fully saturated corners only; see module docs.
    pub const fn to_rgb(self, value: u8) -> [u8; 3] {
        match self {
            Palette::Mono1 => {
                let l = if value & 1 != 0 { 255 } else { 0 };
                [l, l, l]
            }
            Palette::Color3 => [
                if value & 0b001 != 0 { 255 } else { 0 },
                if value & 0b010 != 0 { 255 } else { 0 },
                if value & 0b100 != 0 { 255 } else { 0 },
            ],
        }
    }

    /// sRGB → cell value.
    ///
    /// **M0 placeholder.** This is a fixed 50% threshold per channel with no
    /// calibration whatsoever. The real classifier (§3b) fits a per-region
    /// colour transform from the pilots, equalises cross-module interference,
    /// and only then classifies — optionally with QDA. None of that exists yet
    /// and none of it is needed while the channel is lossless.
    pub const fn from_rgb(self, rgb: [u8; 3]) -> u8 {
        match self {
            Palette::Mono1 => {
                // Rec.601 luma, fixed point: 0.299/0.587/0.114 scaled by 256.
                let luma = (rgb[0] as u32 * 77 + rgb[1] as u32 * 150 + rgb[2] as u32 * 29) >> 8;
                (luma >= 128) as u8
            }
            Palette::Color3 => {
                ((rgb[0] >= 128) as u8)
                    | (((rgb[1] >= 128) as u8) << 1)
                    | (((rgb[2] >= 128) as u8) << 2)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_match_bit_depth() {
        assert_eq!(Palette::Mono1.levels(), 2);
        assert_eq!(Palette::Color3.levels(), 8);
        assert_eq!(Palette::Mono1.max_value(), 1);
        assert_eq!(Palette::Color3.max_value(), 7);
    }

    #[test]
    fn bits_roundtrip_through_cell_values() {
        for palette in [Palette::Mono1, Palette::Color3] {
            for bit in [false, true] {
                assert_eq!(
                    palette.to_bit(palette.bit_value(bit)),
                    bit,
                    "{palette:?} {bit}"
                );
            }
        }
    }

    /// A beacon cell must survive one colour subchannel being misread.
    #[test]
    fn colour_beacon_bits_tolerate_one_bad_channel() {
        for bit in [false, true] {
            let value = Palette::Color3.bit_value(bit);
            for channel in 0..3 {
                let damaged = value ^ (1 << channel);
                assert_eq!(
                    Palette::Color3.to_bit(damaged),
                    bit,
                    "bit {bit}, channel {channel}"
                );
            }
        }
    }

    #[test]
    fn rgb_roundtrips_for_every_value() {
        for palette in [Palette::Mono1, Palette::Color3] {
            for value in 0..palette.levels() {
                let rgb = palette.to_rgb(value);
                assert_eq!(
                    palette.from_rgb(rgb),
                    value,
                    "{palette:?} value {value} via {rgb:?}"
                );
            }
        }
    }

    /// The 8 colours must be exactly the RGB cube corners — i.e. JAB Code's
    /// 8-colour set (§3b). If this ever fails, the palette has drifted off the
    /// corners and colour separation has silently degraded.
    #[test]
    fn colour_palette_is_the_rgb_cube_corners() {
        let mut seen: Vec<[u8; 3]> = (0..8).map(|v| Palette::Color3.to_rgb(v)).collect();
        seen.sort_unstable();
        let mut expected = vec![
            [0, 0, 0],       // black
            [255, 0, 0],     // red
            [0, 255, 0],     // green
            [0, 0, 255],     // blue
            [0, 255, 255],   // cyan   = G+B
            [255, 0, 255],   // magenta= R+B
            [255, 255, 0],   // yellow = R+G
            [255, 255, 255], // white
        ];
        expected.sort_unstable();
        assert_eq!(seen, expected);
    }
}
