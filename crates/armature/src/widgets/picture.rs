use armature_render::{Image, Rect, Size};

use crate::core::{Cx, DrawCx, Length, Limits, Widget};

/// How a picture fills its box when the shapes differ.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fit {
    /// Show the whole picture, leaving space on two sides. The default.
    #[default]
    Contain,
    /// Fill the box, cropping the picture's edges.
    Cover,
    /// Stretch the picture to the box.
    Fill,
}

/// Where to draw a `image`-sized picture in `bounds`, and which part of it,
/// as left, top, right and bottom fractions.
pub fn fit_rect(fit: Fit, image: Size, bounds: Rect) -> (Rect, [f32; 4]) {
    if image.w <= 0.0 || image.h <= 0.0 || bounds.w <= 0.0 || bounds.h <= 0.0 {
        return (bounds, [0.0, 0.0, 1.0, 1.0]);
    }
    match fit {
        Fit::Fill => (bounds, [0.0, 0.0, 1.0, 1.0]),
        Fit::Contain => {
            let scale = (bounds.w / image.w).min(bounds.h / image.h);
            let (w, h) = (image.w * scale, image.h * scale);
            (Rect::new(bounds.x + (bounds.w - w) * 0.5, bounds.y + (bounds.h - h) * 0.5, w, h), [0.0, 0.0, 1.0, 1.0])
        }
        Fit::Cover => {
            // The fraction of the picture that fits, centred.
            let scale = (bounds.w / image.w).max(bounds.h / image.h);
            let (u, v) = (bounds.w / (image.w * scale), bounds.h / (image.h * scale));
            (bounds, [(1.0 - u) * 0.5, (1.0 - v) * 0.5, (1.0 + u) * 0.5, (1.0 + v) * 0.5])
        }
    }
}

/// Shows an [`Image`]. Within a layer pictures sit above shapes and below
/// text, so a label can go over one; to draw shapes over a picture, put
/// them later in a [`Stack`](super::Stack).
pub struct Picture {
    image: Image,
    fit: Fit,
    width: Length,
    height: Length,
}

impl Picture {
    pub fn new(image: &Image) -> Self {
        Self { image: image.clone(), fit: Fit::Contain, width: Length::Shrink, height: Length::Shrink }
    }

    pub fn fit(mut self, fit: Fit) -> Self {
        self.fit = fit;
        self
    }

    pub fn width(mut self, w: impl Into<Length>) -> Self {
        self.width = w.into();
        self
    }

    pub fn height(mut self, h: impl Into<Length>) -> Self {
        self.height = h.into();
        self
    }
}

/// Shorthand for [`Picture::new`].
pub fn picture(image: &Image) -> Picture {
    Picture::new(image)
}

impl<M> Widget<M> for Picture {
    fn width(&self) -> Length {
        self.width
    }

    fn height(&self) -> Length {
        self.height
    }

    fn layout(&mut self, _cx: &mut Cx, limits: Limits) -> Size {
        let natural = Size::new(self.image.width() as f32, self.image.height() as f32);
        let l = limits.constrain(self.width, self.height);
        // Shrink to the picture's own size, scaled down to fit the space.
        let scale = (l.max.w / natural.w.max(1.0)).min(l.max.h / natural.h.max(1.0)).min(1.0);
        l.resolve(Size::new(natural.w * scale, natural.h * scale))
    }

    fn draw(&self, cx: &mut DrawCx) {
        let b = cx.bounds();
        let (rect, uv) = fit_rect(self.fit, Size::new(self.image.width() as f32, self.image.height() as f32), b);
        cx.scene.push_clip(b);
        cx.scene.image_part(&self.image, rect, uv, 1.0);
        cx.scene.pop_clip();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_wide_and_tall_pictures() {
        let b = Rect::new(0.0, 0.0, 200.0, 100.0);
        // A square picture is centred with space left and right.
        assert_eq!(fit_rect(Fit::Contain, Size::new(50.0, 50.0), b).0, Rect::new(50.0, 0.0, 100.0, 100.0));
        // Covering the box shows the middle half of its height.
        assert_eq!(fit_rect(Fit::Cover, Size::new(50.0, 50.0), b), (b, [0.0, 0.25, 1.0, 0.75]));
        assert_eq!(fit_rect(Fit::Fill, Size::new(50.0, 50.0), b), (b, [0.0, 0.0, 1.0, 1.0]));
    }
}
