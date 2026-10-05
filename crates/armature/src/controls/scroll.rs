use armature_render::Rect;

use crate::core::Cx;
use crate::event::{Event, PointerButton, Status};

/// What a scrolling widget reads to place and paint its content.
#[derive(Default)]
pub struct ScrollState {
    /// How far the content has moved up, in logical pixels.
    pub offset: f32,
    /// A thumb drag in progress: where the pointer started, and the offset then.
    pub drag: Option<(f32, f32)>,
    pub hovered: bool,
}

/// What a scrolling widget should do with a pointer event once
/// [`ScrollLogic::pointer`] has seen it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScrollStep {
    /// Handled here. Return this status without telling the content.
    Done(Status),
    /// Pass the event on to the content.
    Pass,
    /// The pointer is outside the viewport. Tell the content it has gone,
    /// so scrolled-away children do not react to it.
    PassAway,
}

/// Vertical scrolling of content taller than its viewport: wheel, thumb
/// dragging and keeping the offset in range.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScrollLogic {
    /// Height of the content.
    pub content: f32,
}

impl ScrollLogic {
    /// The furthest the content can scroll in a viewport this tall.
    pub fn max_offset(&self, viewport: f32) -> f32 {
        (self.content - viewport).max(0.0)
    }

    /// Holds the stored offset in range, for layout after the content or
    /// viewport changed size. Returns the offset.
    pub fn clamp(&self, cx: &mut Cx, viewport: f32) -> f32 {
        let max = self.max_offset(viewport);
        let st = cx.state::<ScrollState>();
        st.offset = st.offset.clamp(0.0, max);
        st.offset
    }

    /// Where a scrollbar thumb `width` wide sits, `inset` from the right
    /// edge of `bounds`, and never shorter than `min_length`. `None` when
    /// everything fits.
    pub fn thumb(&self, bounds: Rect, offset: f32, width: f32, inset: f32, min_length: f32) -> Option<Rect> {
        if self.content <= bounds.h + 0.5 {
            return None;
        }
        let h = (bounds.h * bounds.h / self.content).max(min_length);
        let y = bounds.y + (bounds.h - h) * (offset / self.max_offset(bounds.h));
        Some(Rect::new(bounds.right() - width - inset, y, width, h))
    }

    /// Scrolls by a wheel movement. Returns false if there is nowhere to
    /// scroll, so the event can go to whatever is behind.
    pub fn wheel(&self, cx: &mut Cx, delta: f32, viewport: f32) -> bool {
        let max = self.max_offset(viewport);
        if max <= 0.0 {
            return false;
        }
        let st = cx.state::<ScrollState>();
        let new = (st.offset + delta).clamp(0.0, max);
        if new != st.offset {
            st.offset = new;
            cx.request_layout();
        }
        true
    }

    /// Handles pointer events for a viewport at `bounds` whose scrollbar
    /// thumb, if any, is drawn at `thumb`.
    pub fn pointer(&self, cx: &mut Cx, event: &Event, bounds: Rect, thumb: Option<Rect>) -> ScrollStep {
        let max = self.max_offset(bounds.h);
        match event {
            Event::PointerPressed { pos, button: PointerButton::Primary } => {
                if let Some(t) = thumb
                    && t.inset(-4.0).contains(*pos)
                {
                    let st = cx.state::<ScrollState>();
                    st.drag = Some((pos.y, st.offset));
                    return ScrollStep::Done(Status::Captured);
                }
                if !bounds.contains(*pos) {
                    return ScrollStep::Done(Status::Ignored);
                }
                ScrollStep::Pass
            }
            Event::PointerMoved { pos } => {
                let hovered = bounds.contains(*pos);
                let (changed, drag) = {
                    let st = cx.state::<ScrollState>();
                    let changed = st.hovered != hovered;
                    st.hovered = hovered;
                    (changed, st.drag)
                };
                if changed {
                    cx.request_redraw();
                }
                if let Some((y0, off0)) = drag {
                    let track = bounds.h - thumb.map_or(bounds.h, |t| t.h);
                    if track > 0.0 {
                        cx.state::<ScrollState>().offset = (off0 + (pos.y - y0) * max / track).clamp(0.0, max);
                        cx.request_layout();
                    }
                    return ScrollStep::Done(Status::Captured);
                }
                if hovered { ScrollStep::Pass } else { ScrollStep::PassAway }
            }
            Event::PointerReleased { .. } => {
                cx.state::<ScrollState>().drag = None;
                ScrollStep::Pass
            }
            _ => ScrollStep::Pass,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_is_no_thumb_when_everything_fits() {
        let view = Rect::new(0.0, 0.0, 100.0, 200.0);
        assert_eq!(ScrollLogic { content: 200.0 }.thumb(view, 0.0, 6.0, 3.0, 28.0), None);
        assert_eq!(ScrollLogic { content: 150.0 }.max_offset(200.0), 0.0);
    }

    #[test]
    fn the_thumb_is_in_proportion_and_travels_the_whole_track() {
        let view = Rect::new(10.0, 20.0, 100.0, 200.0);
        let s = ScrollLogic { content: 800.0 };
        assert_eq!(s.max_offset(200.0), 600.0);
        let top = s.thumb(view, 0.0, 6.0, 3.0, 28.0).unwrap();
        assert_eq!((top.x, top.y, top.w, top.h), (101.0, 20.0, 6.0, 50.0), "a quarter of the view, at the right edge");
        let end = s.thumb(view, 600.0, 6.0, 3.0, 28.0).unwrap();
        assert_eq!(end.y + end.h, 220.0, "at the bottom when scrolled to the end");
        // Very long content still gets a thumb big enough to grab.
        assert_eq!(ScrollLogic { content: 100_000.0 }.thumb(view, 0.0, 6.0, 3.0, 28.0).unwrap().h, 28.0);
    }
}
