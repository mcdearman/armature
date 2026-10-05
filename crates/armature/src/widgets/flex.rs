use armature_render::{Point, Size};

use crate::core::{Align, Cx, DrawCx, Element, EventCx, Length, Limits, Padding, Widget};
use crate::event::{Event, Status};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

/// How leftover main-axis space is distributed when no child fills it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Justify {
    #[default]
    Start,
    Center,
    End,
    SpaceBetween,
}

/// Children in a line. Use [`Row`] or [`Column`].
pub struct Flex<M> {
    axis: Axis,
    children: Vec<Element<M>>,
    spacing: f32,
    padding: Padding,
    width: Length,
    height: Length,
    align: Align,
    justify: Justify,
}

/// Lays children out left to right.
pub type Row<M> = Flex<M>;
/// Lays children out top to bottom.
pub type Column<M> = Flex<M>;

impl<M: 'static> Flex<M> {
    fn new(axis: Axis) -> Self {
        Self {
            axis,
            children: vec![],
            spacing: 0.0,
            padding: Padding::ZERO,
            width: Length::Shrink,
            height: Length::Shrink,
            align: Align::Start,
            justify: Justify::Start,
        }
    }

    pub fn row() -> Self {
        Self::new(Axis::Horizontal)
    }

    pub fn column() -> Self {
        Self::new(Axis::Vertical)
    }

    pub fn push(mut self, child: impl Into<Element<M>>) -> Self {
        self.children.push(child.into());
        self
    }

    /// Adds `child` only when `cond` is true.
    pub fn push_if(self, cond: bool, child: impl FnOnce() -> Element<M>) -> Self {
        if cond { self.push(child()) } else { self }
    }

    pub fn extend(mut self, children: impl IntoIterator<Item = Element<M>>) -> Self {
        self.children.extend(children);
        self
    }

    pub fn spacing(mut self, s: f32) -> Self {
        self.spacing = s;
        self
    }

    pub fn padding(mut self, p: impl Into<Padding>) -> Self {
        self.padding = p.into();
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

    /// Cross-axis alignment of children.
    pub fn align(mut self, a: Align) -> Self {
        self.align = a;
        self
    }

    /// Main-axis distribution of leftover space.
    pub fn justify(mut self, j: Justify) -> Self {
        self.justify = j;
        self
    }
}

/// An empty row. Add children with `push`.
pub fn row<M: 'static>() -> Row<M> {
    Flex::row()
}

/// An empty column. Add children with `push`.
pub fn column<M: 'static>() -> Column<M> {
    Flex::column()
}

impl<M: 'static> Widget<M> for Flex<M> {
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
        let horizontal = self.axis == Axis::Horizontal;
        // (main, cross) helpers.
        let mc = |s: Size| if horizontal { (s.w, s.h) } else { (s.h, s.w) };
        let size_of = |main: f32, cross: f32| if horizontal { Size::new(main, cross) } else { Size::new(cross, main) };

        let limits = limits.constrain(self.width, self.height);
        let inner = limits.shrink(self.padding);
        let (max_main, max_cross) = mc(inner.max);
        let (_, min_cross) = mc(inner.min);
        let n = self.children.len();
        let gaps = self.spacing * n.saturating_sub(1) as f32;

        let is_fill = |c: &Element<M>| if horizontal { c.width().is_fill() } else { c.height().is_fill() };
        let portion = |c: &Element<M>| if horizontal { c.width().portion() } else { c.height().portion() };
        let stretch = self.align == Align::Stretch && max_cross.is_finite();
        let cross_min = if stretch { max_cross } else { 0.0 };

        let mut sizes = vec![Size::ZERO; n];
        let mut used = gaps;
        let mut total_portion = 0u32;
        let bounded = max_main.is_finite();

        // Pass 1: fixed and shrinking children.
        for (i, c) in self.children.iter_mut().enumerate() {
            if bounded && is_fill(c) {
                total_portion += portion(c) as u32;
                continue;
            }
            let remaining = (max_main - used).max(0.0);
            let l = Limits::new(size_of(0.0, cross_min), size_of(remaining, max_cross));
            sizes[i] = c.layout(cx, l);
            used += mc(sizes[i]).0;
        }

        // Pass 2: filling children share what is left.
        if total_portion > 0 {
            let free = (max_main - used).max(0.0);
            let mut given = 0.0;
            let fills: Vec<usize> = (0..n).filter(|&i| is_fill(&self.children[i])).collect();
            for (k, &i) in fills.iter().enumerate() {
                let c = &mut self.children[i];
                let share = if k + 1 == fills.len() {
                    free - given
                } else {
                    (free * portion(c) as f32 / total_portion as f32).floor()
                };
                given += share;
                let l = Limits::new(size_of(share, cross_min), size_of(share, max_cross));
                sizes[i] = c.layout(cx, l);
                used += mc(sizes[i]).0;
            }
        }

        let content_cross = sizes.iter().map(|s| mc(*s).1).fold(min_cross, f32::max);
        let content = size_of(used, content_cross);
        let outer = limits.resolve(Size::new(content.w + self.padding.horizontal(), content.h + self.padding.vertical()));
        let (outer_main, outer_cross) = mc(Size::new(outer.w - self.padding.horizontal(), outer.h - self.padding.vertical()));

        // Stretch children that shrank short of the final cross size.
        if self.align == Align::Stretch && !stretch {
            for (i, c) in self.children.iter_mut().enumerate() {
                if mc(sizes[i]).1 < outer_cross {
                    let m = mc(sizes[i]).0;
                    sizes[i] = c.layout(cx, Limits::tight(size_of(m, outer_cross)));
                }
            }
        }

        let free = (outer_main - used).max(0.0);
        let (mut cursor, extra_gap) = match self.justify {
            Justify::Start => (0.0, 0.0),
            Justify::Center => ((free * 0.5).round(), 0.0),
            Justify::End => (free, 0.0),
            Justify::SpaceBetween if n > 1 => (0.0, free / (n - 1) as f32),
            Justify::SpaceBetween => (0.0, 0.0),
        };
        for (i, c) in self.children.iter_mut().enumerate() {
            let (m, cr) = mc(sizes[i]);
            let off = self.align.offset(outer_cross - cr);
            let p = if horizontal {
                Point::new(self.padding.left + cursor, self.padding.top + off)
            } else {
                Point::new(self.padding.left + off, self.padding.top + cursor)
            };
            c.set_position(p);
            cursor += m + self.spacing + extra_gap;
        }
        outer
    }

    fn draw(&self, cx: &mut DrawCx) {
        for c in &self.children {
            c.draw(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx<M>, event: &Event) -> Status {
        Element::event_children(&mut self.children, cx, event)
    }
}
