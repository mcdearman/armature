use armature_render::{Point, Size};

use crate::core::{Cx, DrawCx, Element, EventCx, Length, Limits, Node, Widget};
use crate::event::{Event, Status};

/// Converts a child's messages into the parent's message type.
pub struct Map<M, N> {
    child: Element<M>,
    f: Box<dyn Fn(M) -> N>,
}

impl<M: 'static, N> Map<M, N> {
    pub fn new(child: Element<M>, f: impl Fn(M) -> N + 'static) -> Self {
        Self { child, f: Box::new(f) }
    }
}

impl<M: 'static, N: 'static> Widget<N> for Map<M, N> {
    fn width(&self) -> Length {
        self.child.width()
    }

    fn height(&self) -> Length {
        self.child.height()
    }

    fn visit(&mut self, f: &mut dyn FnMut(&mut dyn Node)) {
        f(&mut self.child);
    }

    fn layout(&mut self, cx: &mut Cx, limits: Limits) -> Size {
        let s = self.child.layout(cx, limits);
        self.child.set_position(Point::ZERO);
        s
    }

    fn draw(&self, cx: &mut DrawCx) {
        self.child.draw(cx);
    }

    fn event(&mut self, cx: &mut EventCx<N>, event: &Event) -> Status {
        let mut inner = Vec::new();
        let status = {
            let mut ecx = EventCx { cx: cx.cx.reborrow(cx.cx.id, cx.cx.bounds), messages: &mut inner };
            self.child.event(&mut ecx, event)
        };
        for m in inner {
            cx.emit((self.f)(m));
        }
        status
    }
}
