use armature_render::{Point, Size};

use crate::core::{Align, Cx, DrawCx, Element, EventCx, Length, Limits, Widget};
use crate::event::{Event, Status};

/// Children drawn on top of each other, later ones above.
pub struct Stack<M> {
    children: Vec<Element<M>>,
    width: Length,
    height: Length,
    align_x: Align,
    align_y: Align,
}

impl<M: 'static> Stack<M> {
    pub fn new() -> Self {
        Self { children: vec![], width: Length::Shrink, height: Length::Shrink, align_x: Align::Start, align_y: Align::Start }
    }

    pub fn push(mut self, child: impl Into<Element<M>>) -> Self {
        self.children.push(child.into());
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

    pub fn align_x(mut self, a: Align) -> Self {
        self.align_x = a;
        self
    }

    pub fn align_y(mut self, a: Align) -> Self {
        self.align_y = a;
        self
    }
}

impl<M: 'static> Default for Stack<M> {
    fn default() -> Self {
        Self::new()
    }
}

/// An empty stack. Add layers with `push`.
pub fn stack<M: 'static>() -> Stack<M> {
    Stack::new()
}

impl<M: 'static> Widget<M> for Stack<M> {
    fn width(&self) -> Length {
        self.width
    }

    fn height(&self) -> Length {
        self.height
    }

    fn children_mut(&mut self) -> &mut [Element<M>] {
        &mut self.children
    }

    fn layout(&mut self, cx: &mut Cx, limits: Limits) -> Size {
        let limits = limits.constrain(self.width, self.height);
        let mut sizes = vec![];
        let mut content = Size::ZERO;
        for c in &mut self.children {
            let s = c.layout(cx, Limits::loose(limits.max));
            content = Size::new(content.w.max(s.w), content.h.max(s.h));
            sizes.push(s);
        }
        let size = limits.resolve(content);
        for (c, s) in self.children.iter_mut().zip(sizes) {
            c.set_position(Point::new(self.align_x.offset(size.w - s.w), self.align_y.offset(size.h - s.h)));
        }
        size
    }

    fn draw(&self, cx: &mut DrawCx) {
        for (i, c) in self.children.iter().enumerate() {
            if i > 0 {
                cx.scene.push_layer();
            }
            c.draw(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx<M>, event: &Event) -> Status {
        Element::event_children(&mut self.children, cx, event)
    }
}
