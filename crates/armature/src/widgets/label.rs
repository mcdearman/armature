use armature_render::{Point, Size, TextLayout};

use crate::core::{Cx, DrawCx, Length, Limits, Widget};

/// Plain text in the style's default text style and content colour, wrapped
/// to the space it has. A `&str` or `String` used as an element becomes one.
pub struct Label {
    content: String,
    layout: Option<TextLayout>,
}

impl Label {
    pub fn new(content: impl Into<String>) -> Self {
        Self { content: content.into(), layout: None }
    }
}

/// Shorthand for [`Label::new`].
pub fn label(content: impl Into<String>) -> Label {
    Label::new(content)
}

impl<M> Widget<M> for Label {
    fn layout(&mut self, cx: &mut Cx, limits: Limits) -> Size {
        let style = cx.text_style();
        let limits = limits.constrain(Length::Shrink, Length::Shrink);
        let layout = cx.text().layout(&self.content, &style, Some(limits.max.w));
        let size = limits.resolve(layout.size());
        self.layout = Some(layout);
        size
    }

    fn draw(&self, cx: &mut DrawCx) {
        let Some(layout) = &self.layout else { return };
        let b = cx.bounds();
        let color = cx.content_color();
        cx.scene.text(layout, Point::new(b.x, b.y), color);
    }
}
