//! Widgets that arrange or capture and paint nothing of their own. Most
//! have a lower-case shorthand constructor.

mod flex;
mod label;
pub(crate) mod map;
mod mouse_area;
mod picture;
mod space;
mod stack;

pub use flex::{column, row, Column, Flex, Justify, Row};
pub use label::{label, Label};
pub use map::Map;
pub use mouse_area::{mouse_area, MouseArea};
pub use picture::{fit_rect, picture, Fit, Picture};
pub use space::Space;
pub use stack::{stack, Stack};

use crate::core::Element;

impl<M: 'static> From<Space> for Element<M> {
    fn from(w: Space) -> Self {
        Element::new(w)
    }
}

impl<M: 'static> From<Label> for Element<M> {
    fn from(w: Label) -> Self {
        Element::new(w)
    }
}

impl<M: 'static> From<Picture> for Element<M> {
    fn from(w: Picture) -> Self {
        Element::new(w)
    }
}

impl<M: 'static> From<MouseArea<M>> for Element<M> {
    fn from(w: MouseArea<M>) -> Self {
        Element::new(w)
    }
}

impl<M: 'static> From<Flex<M>> for Element<M> {
    fn from(w: Flex<M>) -> Self {
        Element::new(w)
    }
}

impl<M: 'static> From<Stack<M>> for Element<M> {
    fn from(w: Stack<M>) -> Self {
        Element::new(w)
    }
}
