use crate::geometry::{Corners, Point, Rect};
use crate::image::{Image, ImageItem};
use crate::text::TextLayout;
use crate::color::Color;
use crate::paint::{Paint, Shadow};

/// GPU instance for one shape. Matches `Inst` in `shape.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct Instance {
    pub rect: [f32; 4],
    pub radii: [f32; 4],
    pub color: [f32; 4],
    pub color2: [f32; 4],
    pub params: [f32; 4],
    pub params2: [f32; 4],
    pub clip: [f32; 4],
}

pub(crate) mod kind {
    pub const FILL: f32 = 0.0;
    pub const SHADOW: f32 = 1.0;
    pub const INNER_SHADOW: f32 = 2.0;
    pub const ARC: f32 = 3.0;
    pub const GRADIENT: f32 = 4.0;
    pub const BACKDROP: f32 = 5.0;
    pub const SEGMENT: f32 = 7.0;
    pub const AREA: f32 = 8.0;
}

pub(crate) struct TextItem {
    pub layout: TextLayout,
    pub pos: Point,
    pub color: Color,
    pub clip: Option<Rect>,
}

/// Within a layer shapes draw first, then images, then text. A new layer starts for
/// overlays and for glass surfaces, which need everything beneath them
/// finished so it can be blurred.
#[derive(Default)]
pub(crate) struct Layer {
    pub shapes: Vec<Instance>,
    pub texts: Vec<TextItem>,
    pub images: Vec<ImageItem>,
    /// Largest backdrop blur radius requested in this layer (logical px).
    pub backdrop_blur: f32,
}

/// A frame's display list in logical pixels.
pub struct Scene {
    pub(crate) layers: Vec<Layer>,
    pub(crate) clear: Color,
    clips: Vec<Rect>,
    offsets: Vec<Point>,
}

impl Default for Scene {
    fn default() -> Self {
        Self::new(Color::TRANSPARENT)
    }
}

fn c4(c: Color) -> [f32; 4] {
    [c.r, c.g, c.b, c.a]
}

impl Scene {
    /// Starts an empty scene. `clear` fills the whole canvas first; use a
    /// transparent colour for glass windows.
    pub fn new(clear: Color) -> Self {
        Self { layers: vec![Layer::default()], clear, clips: vec![], offsets: vec![] }
    }

    fn offset(&self) -> Point {
        self.offsets.last().copied().unwrap_or(Point::ZERO)
    }

    fn clip4(&self) -> [f32; 4] {
        match self.clips.last() {
            Some(c) => [c.x, c.y, c.w, c.h],
            None => [0.0, 0.0, -1.0, -1.0],
        }
    }

    fn layer(&mut self) -> &mut Layer {
        self.layers.last_mut().expect("scene always has a layer")
    }

    fn push(&mut self, rect: Rect, radii: Corners, color: Color, color2: Color, params: [f32; 4], params2: [f32; 4]) {
        let o = self.offset();
        let r = rect.translate(o);
        let clip = self.clip4();
        self.layer().shapes.push(Instance {
            rect: [r.x, r.y, r.w, r.h],
            radii: radii.0,
            color: c4(color),
            color2: c4(color2),
            params,
            params2,
            clip,
        });
    }

    /// Starts a new layer. Everything drawn afterwards appears above
    /// everything drawn before, including text.
    pub fn push_layer(&mut self) {
        if !self.layer().shapes.is_empty() || !self.layer().texts.is_empty() || !self.layer().images.is_empty() {
            self.layers.push(Layer::default());
        }
    }

    /// Restricts drawing to `rect` (intersected with any enclosing clip).
    pub fn push_clip(&mut self, rect: Rect) {
        let r = rect.translate(self.offset());
        let r = match self.clips.last() {
            Some(c) => c.intersect(&r),
            None => r,
        };
        self.clips.push(r);
    }

    pub fn pop_clip(&mut self) {
        self.clips.pop();
    }

    /// Translates everything drawn until the matching [`pop_offset`](Self::pop_offset).
    pub fn push_offset(&mut self, d: Point) {
        let o = self.offset();
        self.offsets.push(o + d);
    }

    pub fn pop_offset(&mut self) {
        self.offsets.pop();
    }

    /// A rounded rectangle with an optional border drawn inside its edge.
    pub fn fill(&mut self, rect: Rect, radii: impl Into<Corners>, fill: Color, border: Option<(f32, Color)>) {
        let (bw, bc) = border.unwrap_or((0.0, Color::TRANSPARENT));
        self.push(rect, radii.into(), fill, bc, [kind::FILL, bw, 0.0, 0.0], [0.0; 4]);
    }

    /// A linear gradient fill from `from` (at point `a`) to `to` (at point `b`).
    pub fn gradient(&mut self, rect: Rect, radii: impl Into<Corners>, from: Color, a: Point, to: Color, b: Point) {
        let o = self.offset();
        let (a, b) = (a + o, b + o);
        self.push(rect, radii.into(), from, to, [kind::GRADIENT, 0.0, 0.0, 0.0], [a.x, a.y, b.x, b.y]);
    }

    /// A CSS-style box shadow for a rounded rectangle.
    pub fn shadow(&mut self, rect: Rect, radii: impl Into<Corners>, s: &Shadow) {
        if s.color.a <= 0.0 {
            return;
        }
        let k = if s.inset { kind::INNER_SHADOW } else { kind::SHADOW };
        // CSS blur radius is twice the Gaussian standard deviation.
        let sigma = (s.blur * 0.5).max(0.0);
        self.push(rect, radii.into(), s.color, Color::TRANSPARENT, [k, sigma, s.spread, 0.0], [s.offset.0, s.offset.1, 0.0, 0.0]);
    }

    /// Paints a box: outer shadows, fill and border, then inner shadows.
    pub fn paint(&mut self, rect: Rect, radii: impl Into<Corners>, paint: &Paint) {
        let radii = radii.into();
        for s in paint.shadows.iter().filter(|s| !s.inset) {
            self.shadow(rect, radii, s);
        }
        self.fill(rect, radii, paint.fill, paint.border);
        for s in paint.shadows.iter().filter(|s| s.inset) {
            self.shadow(rect, radii, s);
        }
    }

    /// A stroked circular arc with round caps inside the square `rect`.
    /// Angles are radians clockwise from 12 o'clock.
    pub fn arc(&mut self, rect: Rect, thickness: f32, start: f32, sweep: f32, color: Color) {
        self.push(rect, Corners::all(0.0), color, Color::TRANSPARENT, [kind::ARC, thickness, start, sweep.clamp(0.0, std::f32::consts::TAU)], [0.0; 4]);
    }

    /// A straight line with round caps.
    pub fn line(&mut self, a: Point, b: Point, thickness: f32, color: Color) {
        let o = self.offset();
        let (a, b) = (a + o, b + o);
        let clip = self.clip4();
        self.layer().shapes.push(Instance {
            rect: [0.0; 4],
            radii: [0.0; 4],
            color: c4(color),
            color2: [0.0; 4],
            params: [kind::SEGMENT, thickness, 0.0, 0.0],
            params2: [a.x, a.y, b.x, b.y],
            clip,
        });
    }

    /// A connected line through `points`.
    pub fn polyline(&mut self, points: &[Point], thickness: f32, color: Color) {
        for w in points.windows(2) {
            self.line(w[0], w[1], thickness, color);
        }
    }

    /// Fills the area between a polyline and the horizontal line `baseline`,
    /// fading from `top` at the line to `bottom` at the baseline.
    pub fn area(&mut self, points: &[Point], baseline: f32, top: Color, bottom: Color) {
        let o = self.offset();
        let clip = self.clip4();
        let base = baseline + o.y;
        let top_y = points.iter().map(|p| p.y).fold(f32::INFINITY, f32::min) + o.y;
        for w in points.windows(2) {
            let (a, b) = (w[0] + o, w[1] + o);
            if b.x <= a.x {
                continue;
            }
            self.layer().shapes.push(Instance {
                rect: [0.0; 4],
                radii: [0.0; 4],
                color: c4(top),
                color2: c4(bottom),
                params: [kind::AREA, base, top_y, 0.0],
                params2: [a.x, a.y, b.x, b.y],
                clip,
            });
        }
    }

    /// Frosted glass: blurs everything already drawn behind `rect`, then
    /// tints it. Starts a new layer so the blur sees finished content.
    pub fn backdrop(&mut self, rect: Rect, radii: impl Into<Corners>, blur: f32, tint: Color) {
        self.layers.push(Layer::default());
        self.layer().backdrop_blur = blur.max(0.0);
        self.push(rect, radii.into(), tint, Color::TRANSPARENT, [kind::BACKDROP, 0.0, 0.0, 0.0], [0.0; 4]);
    }

    /// Draws laid-out text with its top-left corner at `pos`.
    /// Draws `image` stretched over `rect`. Within a layer, images sit above
    /// shapes and below text; call [`push_layer`](Self::push_layer) afterwards
    /// to draw shapes over an image.
    pub fn image(&mut self, image: &Image, rect: Rect) {
        self.image_part(image, rect, [0.0, 0.0, 1.0, 1.0], 1.0);
    }

    /// Draws part of `image` over `rect`. `uv` is the part to show, as
    /// left, top, right and bottom fractions of the image.
    pub fn image_part(&mut self, image: &Image, rect: Rect, uv: [f32; 4], opacity: f32) {
        let rect = rect.translate(self.offset());
        let clip = self.clip4();
        self.layer().images.push(ImageItem { image: image.clone(), rect, uv, clip, opacity: opacity.clamp(0.0, 1.0), turns: 0 });
    }

    /// Draws `image` over `rect` turned clockwise by `turns` quarter turns.
    /// For one or three turns, `rect` should have the picture's height as
    /// its width.
    pub fn image_turned(&mut self, image: &Image, rect: Rect, turns: u8) {
        let rect = rect.translate(self.offset());
        let clip = self.clip4();
        self.layer().images.push(ImageItem { image: image.clone(), rect, uv: [0.0, 0.0, 1.0, 1.0], clip, opacity: 1.0, turns: turns % 4 });
    }

    pub fn text(&mut self, layout: &TextLayout, pos: Point, color: Color) {
        let pos = pos + self.offset();
        let clip = self.clips.last().copied();
        self.layer().texts.push(TextItem { layout: layout.clone(), pos, color, clip });
    }

    /// Number of shapes and text runs, for tests and diagnostics.
    pub fn stats(&self) -> (usize, usize) {
        self.layers.iter().fold((0, 0), |(s, t), l| (s + l.shapes.len(), t + l.texts.len()))
    }
}
