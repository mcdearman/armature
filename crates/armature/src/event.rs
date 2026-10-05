use crate::Point;

/// Mouse buttons (and the primary touch contact).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PointerButton {
    Primary,
    Secondary,
    Middle,
    Other(u16),
}

/// Keyboard modifier state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// Super on Linux, Command on macOS, Windows key on Windows.
    pub logo: bool,
}

impl Modifiers {
    /// The platform's shortcut modifier: Command on macOS, Ctrl elsewhere.
    pub fn command(&self) -> bool {
        if cfg!(target_os = "macos") { self.logo } else { self.ctrl }
    }
}

/// Keys that widgets react to. Everything else arrives as `Character`
/// or `Other`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Enter,
    Space,
    Tab,
    Escape,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    /// A printable key, lower-cased where the platform reports it that way.
    Character(String),
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyEvent {
    pub key: Key,
    pub pressed: bool,
    pub repeat: bool,
    pub modifiers: Modifiers,
    /// Text this key press produces, if any.
    pub text: Option<String>,
}

/// Input delivered to widgets. Positions are logical pixels in window space.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    PointerMoved { pos: Point },
    PointerPressed { pos: Point, button: PointerButton },
    PointerReleased { pos: Point, button: PointerButton },
    /// The pointer left the window.
    PointerLeft,
    /// Scroll by `delta` logical pixels (positive y scrolls content up).
    Wheel { pos: Point, delta: Point },
    /// A pinch on a trackpad or touch screen. `factor` is how much the
    /// fingers spread since the last event: above 1 zooms in.
    Pinch { pos: Point, factor: f32 },
    Key(KeyEvent),
    /// Files dragged from another app, or from this one, and let go here.
    FilesDropped { pos: Point, paths: Vec<std::path::PathBuf> },
    /// Text committed by an input method.
    Ime(String),
    /// The window gained or lost keyboard focus.
    WindowFocus(bool),
}

impl Event {
    pub fn pos(&self) -> Option<Point> {
        match self {
            Event::PointerMoved { pos }
            | Event::PointerPressed { pos, .. }
            | Event::PointerReleased { pos, .. }
            | Event::Wheel { pos, .. }
            | Event::Pinch { pos, .. }
            | Event::FilesDropped { pos, .. } => Some(*pos),
            _ => None,
        }
    }
}

/// Whether a widget consumed an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ignored,
    /// Stop offering this press, wheel or key event to other widgets.
    Captured,
}

impl Status {
    pub fn merge(self, other: Status) -> Status {
        if self == Status::Captured || other == Status::Captured { Status::Captured } else { Status::Ignored }
    }
}
