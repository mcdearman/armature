//! A GPU renderer for interfaces. It knows nothing about any one look: the
//! caller supplies every colour, shadow and typeface.
//!
//! Widgets record drawing commands into a [`Scene`] in logical pixels; the
//! [`Renderer`] turns a scene into pixels with wgpu. Every shape is a single
//! instanced quad evaluated with a signed distance function, so rounded
//! corners, borders and Gaussian shadows are exact at any scale.
//! Glass surfaces use a dual Kawase blur of everything beneath them.

mod color;
mod geometry;
mod image;
mod paint;
mod renderer;
mod scene;
mod text;

pub use color::Color;
pub use geometry::{Corners, Point, Rect, Size};
pub use renderer::{Renderer, SurfaceTarget, CANVAS_FORMAT};
pub use image::Image;
pub use paint::{Paint, Shadow};
pub use scene::Scene;
pub use text::{FontFamily, Fonts, TextLayout, TextStyle, TextSystem};

pub use wgpu;
