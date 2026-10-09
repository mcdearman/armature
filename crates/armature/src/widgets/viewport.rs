use armature_render::{Color, Image, Point, Rect, Size};

use crate::core::{Cx, CursorIcon, DrawCx, EventCx, Length, Limits, Widget, WindowRequest};
use crate::event::{Event, Key, KeyEvent, PointerButton, Status};

/// What happens in a [`Viewport`] that has the keyboard, for whatever is
/// shown in it. Positions are from the viewport's own top left corner, in
/// logical pixels; multiply by the scale it last reported for physical ones.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewportEvent {
    Moved(Point),
    Pressed(Point, PointerButton),
    Released(Point, PointerButton),
    Wheel(Point, Point),
    /// How far the pointer moved while it is held: see [`Viewport::capture`].
    Motion(Point),
    Key(KeyEvent),
    /// The viewport took the keyboard, or gave it up.
    Focused(bool),
    /// The pointer is held, or has been let go: by the app, by Escape, or
    /// by the window losing the keyboard.
    Captured(bool),
    /// Every key and button is to be taken as let go: the viewport has
    /// lost the keyboard or the pointer and will not hear of them rising.
    AllReleased,
}

type Resized<M> = Box<dyn Fn(Rect, f32) -> M>;
type Input<M> = Box<dyn Fn(ViewportEvent) -> M>;

/// A place in a window where something else's picture is shown and its
/// input taken: a running game, drawn by the app into a texture of its own
/// (see `Image::from_texture` and `App::graphics`).
///
/// It says how large it is, so that the app can draw at that size; takes
/// the keyboard when clicked; and while it has it passes on what the
/// pointer and the keys do. Clicking elsewhere, or Tab, gives the keyboard
/// up. It paints nothing of its own but the picture, stretched to fill it.
pub struct Viewport<M> {
    image: Option<Image>,
    width: Length,
    height: Length,
    on_resize: Option<Resized<M>>,
    on_input: Option<Input<M>>,
    playing: bool,
    capture: bool,
}

/// What a viewport remembers between frames.
#[derive(Default)]
struct State {
    /// The place and scale last reported.
    told: Option<(Rect, f32)>,
    focused: bool,
    /// The pointer is held for it.
    holding: bool,
}

impl<M> Viewport<M> {
    /// A viewport showing `image`, or nothing yet.
    pub fn new(image: Option<&Image>) -> Self {
        Self { image: image.cloned(), width: Length::Fill, height: Length::Fill, on_resize: None, on_input: None, playing: false, capture: false }
    }

    pub fn width(mut self, w: impl Into<Length>) -> Self {
        self.width = w.into();
        self
    }

    pub fn height(mut self, h: impl Into<Length>) -> Self {
        self.height = h.into();
        self
    }

    /// Told where the viewport is in the window, in logical pixels, and
    /// the screen's scale, at first and whenever either changes. What is
    /// shown should be drawn `width × scale` by `height × scale` pixels.
    pub fn on_resize(mut self, f: impl Fn(Rect, f32) -> M + 'static) -> Self {
        self.on_resize = Some(Box::new(f));
        self
    }

    /// Told what the pointer and the keys do while the viewport has the
    /// keyboard, and when it takes it and gives it up.
    pub fn on_input(mut self, f: impl Fn(ViewportEvent) -> M + 'static) -> Self {
        self.on_input = Some(Box::new(f));
        self
    }

    /// Draw a new frame every time the screen does, as for a game that is
    /// running. Otherwise one is drawn only when something changes.
    pub fn playing(mut self, playing: bool) -> Self {
        self.playing = playing;
        self
    }

    /// Hold the pointer in the window and hide it while the viewport has
    /// the keyboard, for looking around: its movement then comes as
    /// [`ViewportEvent::Motion`]. Escape lets it go, as does the window
    /// losing the keyboard; a click in the viewport takes it again.
    pub fn capture(mut self, capture: bool) -> Self {
        self.capture = capture;
        self
    }
}

/// Shorthand for [`Viewport::new`].
pub fn viewport<M>(image: Option<&Image>) -> Viewport<M> {
    Viewport::new(image)
}

impl<M> Viewport<M> {
    fn say(&self, cx: &mut EventCx<M>, event: ViewportEvent) {
        if let Some(f) = &self.on_input {
            cx.emit(f(event));
        }
    }

    /// Holds the pointer or lets it go, and says so.
    fn hold(&self, cx: &mut EventCx<M>, hold: bool) {
        if cx.state::<State>().holding != hold {
            cx.state::<State>().holding = hold;
            cx.window_request(WindowRequest::CapturePointer(hold));
            self.say(cx, ViewportEvent::Captured(hold));
            if !hold {
                self.say(cx, ViewportEvent::AllReleased);
            }
        }
    }

    /// Gives the keyboard up, letting go of everything.
    fn leave(&self, cx: &mut EventCx<M>) {
        if !cx.state::<State>().focused {
            return;
        }
        cx.state::<State>().focused = false;
        if cx.state::<State>().holding {
            cx.state::<State>().holding = false;
            cx.window_request(WindowRequest::CapturePointer(false));
            self.say(cx, ViewportEvent::Captured(false));
        }
        self.say(cx, ViewportEvent::AllReleased);
        self.say(cx, ViewportEvent::Focused(false));
    }
}

impl<M: 'static> Widget<M> for Viewport<M> {
    fn width(&self) -> Length {
        self.width
    }

    fn height(&self) -> Length {
        self.height
    }

    fn focusable(&self) -> bool {
        true
    }

    fn layout(&mut self, _cx: &mut Cx, limits: Limits) -> Size {
        // As large as it is let be: what is shown is drawn to fit it.
        let wanted = self.image.as_ref().map_or(Size::new(320.0, 200.0), |i| Size::new(i.width() as f32, i.height() as f32));
        limits.constrain(self.width, self.height).resolve(wanted)
    }

    fn draw(&self, cx: &mut DrawCx) {
        let (bounds, scale) = (cx.bounds(), cx.scale());
        if cx.state::<State>().told != Some((bounds, scale)) {
            cx.state::<State>().told = Some((bounds, scale));
            if let Some(f) = &self.on_resize {
                cx.defer(f(bounds, scale));
            }
        }
        match &self.image {
            Some(image) => {
                cx.scene.push_clip(bounds);
                cx.scene.image(image, bounds);
                cx.scene.pop_clip();
            }
            // Something to be on, and to click, before there is a picture.
            None => cx.scene.fill(bounds, 0.0, Color::BLACK, None),
        }
        if self.playing {
            cx.request_animation();
        }
    }

    fn event(&mut self, cx: &mut EventCx<M>, event: &Event) -> Status {
        let bounds = cx.bounds();
        let within = |p: Point| Point::new(p.x - bounds.x, p.y - bounds.y);
        // The keyboard went elsewhere, by Tab or another widget taking it.
        if cx.state::<State>().focused && !cx.is_focused() && !matches!(event, Event::WindowFocus(_)) {
            self.leave(cx);
        }
        let (focused, holding) = (cx.state::<State>().focused, cx.state::<State>().holding);
        match event {
            Event::PointerPressed { pos, button } if bounds.contains(*pos) => {
                if !focused {
                    cx.request_focus();
                    cx.state::<State>().focused = true;
                    self.say(cx, ViewportEvent::Focused(true));
                }
                if self.capture {
                    self.hold(cx, true);
                }
                self.say(cx, ViewportEvent::Pressed(within(*pos), *button));
                Status::Captured
            }
            // A press anywhere else is someone else's, and the keyboard goes with it.
            Event::PointerPressed { .. } if focused && !holding => {
                cx.release_focus();
                self.leave(cx);
                Status::Ignored
            }
            _ if !focused => Status::Ignored,
            // Held, the pointer is nowhere: a press is the viewport's wherever it lands.
            Event::PointerPressed { pos, button } => {
                self.say(cx, ViewportEvent::Pressed(within(*pos), *button));
                Status::Captured
            }
            Event::PointerReleased { pos, button } => {
                self.say(cx, ViewportEvent::Released(within(*pos), *button));
                Status::Captured
            }
            Event::PointerMoved { pos } if holding || bounds.contains(*pos) => {
                cx.set_cursor(CursorIcon::Default);
                if !holding {
                    self.say(cx, ViewportEvent::Moved(within(*pos)));
                }
                Status::Ignored
            }
            Event::PointerMotion { delta } if holding => {
                self.say(cx, ViewportEvent::Motion(*delta));
                Status::Captured
            }
            Event::Wheel { pos, delta } if holding || bounds.contains(*pos) => {
                self.say(cx, ViewportEvent::Wheel(within(*pos), *delta));
                Status::Captured
            }
            // Escape lets the pointer go, and is not passed on then; with
            // nothing held it is a key like any other.
            Event::Key(k) if holding && k.pressed && k.key == Key::Escape => {
                self.hold(cx, false);
                Status::Captured
            }
            // Tab is left for moving on from the viewport, unless the pointer is held.
            Event::Key(k) if k.key == Key::Tab && !holding => Status::Ignored,
            Event::Key(k) => {
                self.say(cx, ViewportEvent::Key(k.clone()));
                Status::Captured
            }
            Event::PointerCaptureLost => {
                self.hold(cx, false);
                Status::Ignored
            }
            // The window itself lost the keyboard: nothing more will be heard of what was down.
            Event::WindowFocus(false) => {
                if holding {
                    self.hold(cx, false);
                } else {
                    self.say(cx, ViewportEvent::AllReleased);
                }
                Status::Ignored
            }
            _ => Status::Ignored,
        }
    }
}
