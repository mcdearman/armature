use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use std::any::Any;
use std::rc::Rc;

use armature_render::{Color, Fonts, Point, Rect, Size, TextStyle};

use crate::core::Element;
use crate::event::KeyEvent;

/// An application, in the Elm style: state, messages that change it,
/// and a view that describes the interface for the current state.
pub trait App: 'static {
    type Message: Clone + 'static;

    /// Shown in the title bar and task switcher.
    fn title(&self) -> String {
        String::new()
    }

    /// Applies a message to the application state.
    fn update(&mut self, message: Self::Message);

    /// Describes the interface for the current state.
    fn view(&self) -> Element<Self::Message>;

    /// How the window looks, and whatever a toolkit's widgets read to paint
    /// themselves. `system` is the desktop's light or dark preference.
    /// Asked before every frame, so it can follow app state.
    fn style(&self, _system: Scheme) -> Style {
        Style::default()
    }

    /// The typefaces text is drawn with. Read once at startup.
    fn fonts(&self) -> Fonts {
        Fonts::system()
    }

    /// Wraps the view in whatever the window itself needs drawn: its
    /// background and, with [`Decorations::Custom`], a title bar. The
    /// default draws nothing around the view.
    fn frame(&self, view: Element<Self::Message>, _chrome: Chrome) -> Element<Self::Message> {
        view
    }

    /// Key presses no widget handled, for app-wide shortcuts such as save.
    fn on_key(&self, _key: &KeyEvent) -> Option<Self::Message> {
        None
    }

    /// Called once before the first frame. Keep the proxy to send messages
    /// from other threads, such as a pty reader or a file watcher.
    fn start(&mut self, _proxy: Proxy<Self::Message>) {}

    /// Timers that send messages periodically.
    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        vec![]
    }

    /// How the window presents itself right now: shown or hidden, on top,
    /// see-through. Read after every update, so it can follow app state.
    fn window_state(&self) -> WindowState {
        WindowState::default()
    }

    /// Called when the user asks to close the window. Return a message to
    /// handle it yourself, for example by hiding the window; `None` closes
    /// the window and ends the app.
    fn on_close(&self) -> Option<Self::Message> {
        None
    }

    /// Called when the window moves or changes size, with where it is on
    /// the screen.
    fn on_window_geometry(&self, _geometry: WindowGeometry) -> Option<Self::Message> {
        None
    }

    /// Called when the window gains or loses keyboard focus, as when the
    /// user clicks another app. A pop-up can use this to dismiss itself.
    fn on_window_focus(&self, _focused: bool) -> Option<Self::Message> {
        None
    }

    /// Return true to end the app. Checked after every update.
    fn should_exit(&self) -> bool {
        false
    }

    /// Initial window settings. Read once at startup.
    fn window(&self) -> WindowSettings {
        WindowSettings::default()
    }
}

/// Sends messages to a running app from any thread and wakes its event loop.
pub struct Proxy<M> {
    pub(crate) tx: Sender<M>,
    pub(crate) wake: Arc<dyn Fn() + Send + Sync>,
}

impl<M> Clone for Proxy<M> {
    fn clone(&self) -> Self {
        Self { tx: self.tx.clone(), wake: self.wake.clone() }
    }
}

impl<M> Proxy<M> {
    /// Queues `message` for `App::update`. Returns false once the app has exited.
    pub fn send(&self, message: M) -> bool {
        let ok = self.tx.send(message).is_ok();
        if ok {
            (self.wake)();
        }
        ok
    }
}

/// Sends `message` every `period`.
#[derive(Clone, Debug)]
pub struct Subscription<M> {
    pub period: Duration,
    pub message: M,
}

impl<M> Subscription<M> {
    pub fn every(period: Duration, message: M) -> Self {
        Self { period, message }
    }
}

/// How a window presents itself. See [`App::window_state`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowState {
    /// Hidden windows keep running, for example to wait for a global shortcut.
    pub visible: bool,
    /// Stay above other apps' windows.
    pub always_on_top: bool,
    /// Draw no window background or title bar. Only what the view paints
    /// shows; the rest of the window is see-through.
    pub bare: bool,
    /// Resize the window's content area to this whenever the value changes.
    pub size: Option<Size>,
    /// Move the window's top-left corner here, in logical screen pixels,
    /// whenever the value changes. Ignored where the compositor places
    /// windows itself, as on Wayland.
    pub position: Option<Point>,
    /// Leave the window out of screenshots and screen recordings, where the
    /// platform can.
    pub hidden_from_capture: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self { visible: true, always_on_top: false, bare: false, size: None, position: None, hidden_from_capture: false }
    }
}

/// Where a window is. See [`App::on_window_geometry`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowGeometry {
    /// The window's content area on the screen, in logical pixels.
    /// Compositors that hide window positions, such as Wayland ones, report
    /// the origin as zero.
    pub frame: Rect,
    /// The screen the window is on, in the same coordinates.
    pub screen: Rect,
    /// Physical pixels per logical pixel.
    pub scale: f32,
}

/// Who draws the title bar and window border.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Decorations {
    /// The app draws the title bar, window controls and rounded corners in
    /// [`App::frame`], so windows look the same on every platform. The
    /// default.
    #[default]
    Custom,
    /// The platform's native title bar.
    System,
}

/// The desktop's light or dark preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Scheme {
    #[default]
    Light,
    Dark,
}

/// What [`App::frame`] should draw around the view.
#[derive(Clone, Debug, PartialEq)]
pub struct Chrome {
    pub title: String,
    /// Draw a title bar with window controls.
    pub title_bar: bool,
    /// Round the window's corners. False when maximized.
    pub rounded: bool,
    /// Paint the window background. False for a bare, see-through window.
    pub background: bool,
}

/// How a window looks. The framework reads the few things it needs itself;
/// a toolkit attaches its own theme with [`Style::new`] and its widgets read
/// it back with [`Cx::style`](crate::Cx::style).
#[derive(Clone)]
pub struct Style {
    content: Color,
    text: TextStyle,
    window_radius: f32,
    backdrop_blur: Option<f32>,
    value: Option<Rc<dyn Any>>,
    same: fn(&dyn Any, &dyn Any) -> bool,
}

impl Default for Style {
    fn default() -> Self {
        Self { content: Color::BLACK, text: TextStyle::default(), window_radius: 0.0, backdrop_blur: None, value: None, same: |_, _| true }
    }
}

impl Style {
    /// A style carrying `theme` for widgets to read.
    pub fn new<T: PartialEq + 'static>(theme: T) -> Self {
        let same = |a: &dyn Any, b: &dyn Any| matches!((a.downcast_ref::<T>(), b.downcast_ref::<T>()), (Some(a), Some(b)) if a == b);
        Self { value: Some(Rc::new(theme)), same, ..Self::default() }
    }

    /// Colour for text and icons that do not set their own.
    pub fn content(mut self, c: Color) -> Self {
        self.content = c;
        self
    }

    /// The text style for plain strings used as elements.
    pub fn text(mut self, t: TextStyle) -> Self {
        self.text = t;
        self
    }

    /// Corner radius of a window with [`Decorations::Custom`], which the
    /// platform's window shape and backdrop follow.
    pub fn window_radius(mut self, r: f32) -> Self {
        self.window_radius = r;
        self
    }

    /// Blurs what is behind the window by this much, where the platform
    /// can. `None` is an ordinary opaque window.
    pub fn backdrop_blur(mut self, blur: Option<f32>) -> Self {
        self.backdrop_blur = blur;
        self
    }

    /// The theme given to [`Style::new`], if it is a `T`.
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.value.as_deref().and_then(|v| v.downcast_ref())
    }

    pub(crate) fn content_color(&self) -> Color {
        self.content
    }

    pub(crate) fn text_style(&self) -> TextStyle {
        self.text
    }

    pub(crate) fn radius(&self) -> f32 {
        self.window_radius
    }

    pub(crate) fn blur(&self) -> Option<f32> {
        self.backdrop_blur
    }
}

impl PartialEq for Style {
    fn eq(&self, other: &Self) -> bool {
        let values = match (&self.value, &other.value) {
            (Some(a), Some(b)) => (self.same)(a.as_ref(), b.as_ref()),
            (None, None) => true,
            _ => false,
        };
        values && self.content == other.content && self.text == other.text && self.window_radius == other.window_radius && self.backdrop_blur == other.backdrop_blur
    }
}

#[derive(Clone, Debug)]
pub struct WindowSettings {
    /// Initial content size in logical pixels.
    pub size: Size,
    pub min_size: Option<Size>,
    pub decorations: Decorations,
    pub resizable: bool,
    /// Identifies the app to the desktop, such as `org.example.Files`. On Linux
    /// this is the Wayland app ID and X11 class, which must match the name
    /// of the app's `.desktop` file for docks and menus to show its icon.
    pub app_id: Option<String>,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self { size: Size::new(960.0, 640.0), min_size: Some(Size::new(360.0, 240.0)), decorations: Decorations::Custom, resizable: true, app_id: None }
    }
}
