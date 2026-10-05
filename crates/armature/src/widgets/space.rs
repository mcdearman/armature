use armature_render::Size;

use crate::core::{Cx, DrawCx, Length, Limits, Widget};

/// Empty space.
pub struct Space {
    width: Length,
    height: Length,
}

impl Space {
    pub fn new(width: impl Into<Length>, height: impl Into<Length>) -> Self {
        Self { width: width.into(), height: height.into() }
    }

    /// Space that grows horizontally, pushing siblings apart in a row.
    pub fn fill_x() -> Self {
        Self::new(Length::Fill, Length::Shrink)
    }

    /// Space that grows vertically, pushing siblings apart in a column.
    pub fn fill_y() -> Self {
        Self::new(Length::Shrink, Length::Fill)
    }
}

impl<M> Widget<M> for Space {
    fn width(&self) -> Length {
        self.width
    }

    fn height(&self) -> Length {
        self.height
    }

    fn layout(&mut self, _cx: &mut Cx, limits: Limits) -> Size {
        limits.constrain(self.width, self.height).resolve(Size::ZERO)
    }

    fn draw(&self, _cx: &mut DrawCx) {}
}
