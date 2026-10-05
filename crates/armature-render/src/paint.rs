use crate::color::Color;

/// A drop shadow or inner shadow, in CSS `box-shadow` terms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    pub offset: (f32, f32),
    /// Blur radius (CSS semantics: the Gaussian sigma is half of this).
    pub blur: f32,
    pub spread: f32,
    pub color: Color,
    pub inset: bool,
}

/// How to paint a box: its fill, border and shadows, and the colour for
/// whatever is drawn on top of it. See [`Scene::paint`](crate::Scene::paint).
#[derive(Clone, Debug, PartialEq)]
pub struct Paint {
    pub fill: Color,
    /// Border width and colour.
    pub border: Option<(f32, Color)>,
    pub shadows: Vec<Shadow>,
    /// Colour for content (text, icons) drawn on this surface.
    pub content: Color,
}
