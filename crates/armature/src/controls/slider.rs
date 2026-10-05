use std::ops::RangeInclusive;

use armature_render::Rect;

use crate::core::{CursorIcon, Cx};
use crate::event::{Event, Key, PointerButton, Status};

/// What a slider's widget reads to paint itself.
#[derive(Default)]
pub struct SliderState {
    pub dragging: bool,
    pub hovered: bool,
}

/// What an event did to a slider.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SliderChange {
    /// The new value, if it changed.
    pub value: Option<f32>,
    /// The user let go of the thumb.
    pub released: bool,
}

/// Picking a number from a range by dragging along a track, or with the
/// arrow, page, Home and End keys.
#[derive(Clone, Debug)]
pub struct SliderLogic {
    pub range: RangeInclusive<f32>,
    pub value: f32,
    /// Snap to multiples of this, which is also the arrow-key increment.
    /// Without one, an arrow key moves a hundredth of the range.
    pub step: Option<f32>,
}

impl SliderLogic {
    pub fn new(range: RangeInclusive<f32>, value: f32) -> Self {
        Self { range, value, step: None }
    }

    /// `v` held inside the range and snapped to the step.
    pub fn snap(&self, v: f32) -> f32 {
        let (lo, hi) = (*self.range.start(), *self.range.end());
        let v = v.clamp(lo, hi);
        match self.step {
            Some(s) if s > 0.0 => (lo + ((v - lo) / s).round() * s).clamp(lo, hi),
            _ => v,
        }
    }

    /// Where the value sits in the range, from 0 to 1.
    pub fn fraction(&self) -> f32 {
        let (lo, hi) = (*self.range.start(), *self.range.end());
        if hi > lo { ((self.value - lo) / (hi - lo)).clamp(0.0, 1.0) } else { 0.0 }
    }

    /// The value for a pointer at `x`, when the thumb's centre travels
    /// `travel` pixels starting at `start`. Not snapped.
    pub fn value_at(&self, x: f32, start: f32, travel: f32) -> f32 {
        let t = ((x - start) / travel.max(1.0)).clamp(0.0, 1.0);
        *self.range.start() + t * (*self.range.end() - *self.range.start())
    }

    /// The value a key press moves to, or `None` for keys a slider ignores.
    pub fn key(&self, key: &Key) -> Option<f32> {
        let span = *self.range.end() - *self.range.start();
        let step = self.step.unwrap_or(span / 100.0);
        let v = match key {
            Key::Left | Key::Down => self.value - step,
            Key::Right | Key::Up => self.value + step,
            Key::PageDown => self.value - span / 10.0,
            Key::PageUp => self.value + span / 10.0,
            Key::Home => *self.range.start(),
            Key::End => *self.range.end(),
            _ => return None,
        };
        Some(self.snap(v))
    }

    /// Handles an event for a slider occupying `bounds`, whose thumb centre
    /// travels `travel` pixels starting at x = `start`. Updates the value
    /// and the [`SliderState`] kept for this widget.
    pub fn event(&mut self, cx: &mut Cx, event: &Event, bounds: Rect, start: f32, travel: f32) -> (Status, SliderChange) {
        let mut change = SliderChange::default();
        let mut set = |logic: &mut Self, v: f32| {
            if v != logic.value {
                logic.value = v;
                change.value = Some(v);
            }
        };
        let status = match event {
            Event::PointerMoved { pos } => {
                let inside = bounds.contains(*pos);
                let st = cx.state::<SliderState>();
                let dragging = st.dragging;
                if st.hovered != inside {
                    st.hovered = inside;
                    cx.request_redraw();
                }
                if inside || dragging {
                    cx.set_cursor(if dragging { CursorIcon::Grabbing } else { CursorIcon::Pointer });
                }
                if dragging {
                    let v = self.snap(self.value_at(pos.x, start, travel));
                    set(self, v);
                }
                Status::Ignored
            }
            Event::PointerPressed { pos, button: PointerButton::Primary } if bounds.contains(*pos) => {
                cx.state::<SliderState>().dragging = true;
                cx.request_focus();
                let v = self.snap(self.value_at(pos.x, start, travel));
                set(self, v);
                Status::Captured
            }
            Event::PointerReleased { .. } => {
                if std::mem::take(&mut cx.state::<SliderState>().dragging) {
                    cx.request_redraw();
                    change.released = true;
                }
                Status::Ignored
            }
            Event::PointerLeft => {
                cx.state::<SliderState>().hovered = false;
                Status::Ignored
            }
            Event::Key(k) if k.pressed && cx.is_focused() => match self.key(&k.key) {
                Some(v) => {
                    set(self, v);
                    Status::Captured
                }
                None => Status::Ignored,
            },
            _ => Status::Ignored,
        };
        (status, change)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping_rounds_to_the_step_and_stays_in_range() {
        let s = SliderLogic { step: Some(10.0), ..SliderLogic::new(0.0..=100.0, 0.0) };
        assert_eq!(s.snap(44.0), 40.0);
        assert_eq!(s.snap(45.0), 50.0);
        assert_eq!(s.snap(-20.0), 0.0);
        assert_eq!(s.snap(999.0), 100.0);
        // A range that does not start at zero snaps from its own start.
        let s = SliderLogic { step: Some(2.0), ..SliderLogic::new(1.0..=9.0, 1.0) };
        assert_eq!(s.snap(4.2), 5.0);
        assert_eq!(SliderLogic::new(0.0..=1.0, 0.0).snap(0.37), 0.37, "no step, no snapping");
    }

    #[test]
    fn the_pointer_maps_onto_the_track() {
        let s = SliderLogic::new(0.0..=100.0, 0.0);
        assert_eq!(s.value_at(10.0, 10.0, 200.0), 0.0);
        assert_eq!(s.value_at(110.0, 10.0, 200.0), 50.0);
        assert_eq!(s.value_at(500.0, 10.0, 200.0), 100.0, "clamped past the end");
        assert_eq!(s.value_at(-50.0, 10.0, 200.0), 0.0);
    }

    #[test]
    fn the_fraction_is_safe_for_an_empty_range() {
        assert_eq!(SliderLogic::new(0.0..=10.0, 2.5).fraction(), 0.25);
        assert_eq!(SliderLogic::new(5.0..=5.0, 5.0).fraction(), 0.0);
    }

    #[test]
    fn keys_step_page_and_jump() {
        let s = SliderLogic::new(0.0..=200.0, 100.0);
        assert_eq!(s.key(&Key::Right), Some(102.0), "a hundredth of the range without a step");
        assert_eq!(s.key(&Key::PageDown), Some(80.0));
        assert_eq!(s.key(&Key::Home), Some(0.0));
        assert_eq!(s.key(&Key::End), Some(200.0));
        assert_eq!(s.key(&Key::Enter), None);
    }
}
