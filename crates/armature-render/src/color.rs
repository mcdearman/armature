/// A colour in the sRGB colour space with straight (non-premultiplied) alpha.
///
/// The renderer blends in sRGB space, the same way browsers do, so these values are
/// what you see on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0.0, 0.0, 0.0, 0.0);
    pub const BLACK: Color = Color::rgb(0.0, 0.0, 0.0);
    pub const WHITE: Color = Color::rgb(1.0, 1.0, 1.0);

    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// Builds a colour from a `0xRRGGBB` literal.
    pub const fn hex(rgb: u32) -> Self {
        Self::rgb(
            ((rgb >> 16) & 0xff) as f32 / 255.0,
            ((rgb >> 8) & 0xff) as f32 / 255.0,
            (rgb & 0xff) as f32 / 255.0,
        )
    }

    pub const fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    /// Mixes `self` toward `other` by `t` (0 keeps `self`), like CSS `color-mix(in srgb)`.
    pub fn mix(self, other: Color, t: f32) -> Self {
        let l = |a: f32, b: f32| a + (b - a) * t;
        Self {
            r: l(self.r, other.r),
            g: l(self.g, other.g),
            b: l(self.b, other.b),
            a: l(self.a, other.a),
        }
    }

    /// Relative luminance per WCAG 2.x.
    pub fn luminance(self) -> f32 {
        fn lin(c: f32) -> f32 {
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        }
        0.2126 * lin(self.r) + 0.7152 * lin(self.g) + 0.0722 * lin(self.b)
    }

    /// WCAG contrast ratio between two opaque colours (1.0 to 21.0).
    pub fn contrast(self, other: Color) -> f32 {
        let (a, b) = (self.luminance(), other.luminance());
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }

    pub fn to_rgba8(self) -> [u8; 4] {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        [q(self.r), q(self.g), q(self.b), q(self.a)]
    }

    /// Premultiplied components, ready for the GPU.
    pub fn premultiplied(self) -> [f32; 4] {
        [self.r * self.a, self.g * self.a, self.b * self.a, self.a]
    }
}

impl Default for Color {
    fn default() -> Self {
        Color::TRANSPARENT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_reads_red_green_blue_in_order() {
        let c = Color::hex(0x336699);
        assert_eq!(c.to_rgba8(), [0x33, 0x66, 0x99, 255]);
        assert_eq!(Color::hex(0xff0000), Color::rgb(1.0, 0.0, 0.0));
    }

    #[test]
    fn mix_moves_every_channel_including_alpha() {
        let a = Color::rgba(0.0, 0.2, 1.0, 0.0);
        let b = Color::rgba(1.0, 0.6, 0.0, 1.0);
        assert_eq!(a.mix(b, 0.0), a);
        assert_eq!(a.mix(b, 1.0), b);
        let half = a.mix(b, 0.5);
        for (got, want) in [(half.r, 0.5), (half.g, 0.4), (half.b, 0.5), (half.a, 0.5)] {
            assert!((got - want).abs() < 1e-6);
        }
    }

    #[test]
    fn contrast_follows_wcag() {
        assert!((Color::BLACK.contrast(Color::WHITE) - 21.0).abs() < 1e-3);
        assert_eq!(Color::WHITE.contrast(Color::BLACK), Color::BLACK.contrast(Color::WHITE), "order does not matter");
        assert!((Color::hex(0x808080).contrast(Color::hex(0x808080)) - 1.0).abs() < 1e-6);
        // A reference pair: #767676 on white is the lightest grey that passes AA.
        let c = Color::hex(0x767676).contrast(Color::WHITE);
        assert!((c - 4.54).abs() < 0.01, "{c}");
    }

    #[test]
    fn bytes_round_and_clamp() {
        assert_eq!(Color::rgba(2.0, -1.0, 0.5, 0.25).to_rgba8(), [255, 0, 128, 64]);
    }

    #[test]
    fn premultiplying_scales_colour_by_alpha() {
        assert_eq!(Color::rgba(1.0, 0.5, 0.0, 0.5).premultiplied(), [0.5, 0.25, 0.0, 0.5]);
        assert_eq!(Color::default(), Color::TRANSPARENT);
        assert_eq!(Color::WHITE.with_alpha(0.3).a, 0.3);
    }
}
