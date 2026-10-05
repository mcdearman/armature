use std::path::PathBuf;

use armature_render::{Point, Size};

use crate::core::{Cx, DrawCx, Element, EventCx, Length, Limits, Widget, WindowRequest};
use crate::event::{Event, PointerButton, Status};

/// How far the pointer moves with the button down before it is a drag.
const DRAG_DISTANCE: f32 = 6.0;

#[derive(Default)]
struct AreaState {
    /// Where the primary button went down, while it is still down here.
    pressed: Option<Point>,
}

/// Wraps a widget to hear about pointer actions the widget itself does not
/// handle: the right click that opens a context menu, files dropped on it,
/// and dragging it away as files.
pub struct MouseArea<M> {
    child: [Element<M>; 1],
    on_secondary: Option<Box<dyn Fn(Point) -> M>>,
    on_drop: Option<Box<dyn Fn(Vec<PathBuf>) -> M>>,
    drag_files: Vec<PathBuf>,
}

impl<M: 'static> MouseArea<M> {
    pub fn new(child: impl Into<Element<M>>) -> Self {
        Self { child: [child.into()], on_secondary: None, on_drop: None, drag_files: vec![] }
    }

    /// Called with the pointer's position in the window when the secondary
    /// button (right click, or Control-click on macOS) is pressed inside.
    pub fn on_secondary_press(mut self, f: impl Fn(Point) -> M + 'static) -> Self {
        self.on_secondary = Some(Box::new(f));
        self
    }

    /// Called with the files let go over this area, whether they were
    /// dragged from another app or from this one. Of nested areas, the
    /// innermost one under the pointer gets the drop.
    pub fn on_drop(mut self, f: impl Fn(Vec<PathBuf>) -> M + 'static) -> Self {
        self.on_drop = Some(Box::new(f));
        self
    }

    /// Dragging this area drags these files, to drop on another area or
    /// another app. A plain click still reaches the widget inside.
    pub fn drag_files(mut self, paths: Vec<PathBuf>) -> Self {
        self.drag_files = paths;
        self
    }
}

/// Shorthand for [`MouseArea::new`].
pub fn mouse_area<M: 'static>(child: impl Into<Element<M>>) -> MouseArea<M> {
    MouseArea::new(child)
}

impl<M: 'static> Widget<M> for MouseArea<M> {
    fn width(&self) -> Length {
        self.child[0].width()
    }

    fn height(&self) -> Length {
        self.child[0].height()
    }

    fn children_mut(&mut self) -> &mut [Element<M>] {
        &mut self.child
    }

    fn layout(&mut self, cx: &mut Cx, limits: Limits) -> Size {
        let size = self.child[0].layout(cx, limits);
        self.child[0].set_position(Point::ZERO);
        size
    }

    fn draw(&self, cx: &mut DrawCx) {
        self.child[0].draw(cx);
    }

    fn event(&mut self, cx: &mut EventCx<M>, event: &Event) -> Status {
        let bounds = cx.bounds();
        // Drags are watched whatever the child does with the press, since a
        // button inside takes the press for itself.
        if !self.drag_files.is_empty() {
            match event {
                Event::PointerPressed { pos, button: PointerButton::Primary } if bounds.contains(*pos) => cx.state::<AreaState>().pressed = Some(*pos),
                Event::PointerReleased { button: PointerButton::Primary, .. } | Event::PointerLeft => cx.state::<AreaState>().pressed = None,
                Event::PointerMoved { pos } => {
                    let st = cx.state::<AreaState>();
                    if let Some(from) = st.pressed
                        && (pos.x - from.x).hypot(pos.y - from.y) >= DRAG_DISTANCE
                    {
                        st.pressed = None;
                        cx.window_request(WindowRequest::DragFiles(self.drag_files.clone()));
                    }
                }
                _ => {}
            }
        }
        // The child goes first, so an area inside this one answers for itself.
        if self.child[0].event(cx, event) == Status::Captured {
            return Status::Captured;
        }
        match event {
            Event::PointerPressed { pos, button } if bounds.contains(*pos) => {
                // Control-click is the Mac's other way to right-click.
                let secondary = *button == PointerButton::Secondary || (cfg!(target_os = "macos") && *button == PointerButton::Primary && cx.modifiers().ctrl);
                if let (true, Some(f)) = (secondary, &self.on_secondary) {
                    cx.emit(f(*pos));
                    return Status::Captured;
                }
                Status::Ignored
            }
            Event::FilesDropped { pos, paths } if bounds.contains(*pos) => match &self.on_drop {
                Some(f) => {
                    cx.emit(f(paths.clone()));
                    Status::Captured
                }
                None => Status::Ignored,
            },
            _ => Status::Ignored,
        }
    }
}
