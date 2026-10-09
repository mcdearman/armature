//! An interface drawn over something else: a game's frame, say, by a host
//! that owns the window and shares its input. What such a host needs is to
//! be told which events the interface took, and to draw the interface
//! without wiping out what is already there.
//!
//! Needs a GPU adapter.

use armature::testing::Harness;
use armature::{App, Color, Cx, DrawCx, Element, Event, EventCx, Key, KeyEvent, Limits, Modifiers, Point, PointerButton, Rect, Size, Status, Widget};

/// A panel in the corner with one button on it, and nothing anywhere else.
const PANEL: Rect = Rect { x: 10.0, y: 10.0, w: 120.0, h: 60.0 };
const BUTTON: Rect = Rect { x: 20.0, y: 20.0, w: 50.0, h: 24.0 };

struct Overlay;

impl Widget<Msg> for Overlay {
    fn layout(&mut self, _cx: &mut Cx, limits: Limits) -> Size {
        limits.max
    }

    fn draw(&self, cx: &mut DrawCx) {
        cx.scene.shadow(PANEL, 8.0, &armature::Shadow { color: Color::BLACK.with_alpha(0.4), blur: 30.0, offset: (0.0, 10.0), spread: 0.0, inset: false });
        cx.scene.fill(PANEL, 8.0, Color::hex(0x202020).with_alpha(0.8), None);
        cx.scene.fill(BUTTON, 4.0, Color::hex(0x3f5bc4), None);
        // Something drawn that cannot be seen is not there to be clicked.
        cx.scene.fill(Rect::new(200.0, 100.0, 50.0, 50.0), 0.0, Color::TRANSPARENT, None);
    }

    fn event(&mut self, cx: &mut EventCx<Msg>, event: &Event) -> Status {
        match event {
            Event::PointerPressed { pos, button: PointerButton::Primary } if BUTTON.contains(*pos) => {
                cx.emit(Msg::Pressed);
                Status::Captured
            }
            _ => Status::Ignored,
        }
    }
}

#[derive(Clone, Debug)]
enum Msg {
    Pressed,
    Toggled,
}

#[derive(Default)]
struct Hud {
    pressed: u32,
    toggled: u32,
}

impl App for Hud {
    type Message = Msg;

    fn update(&mut self, m: Msg) {
        match m {
            Msg::Pressed => self.pressed += 1,
            Msg::Toggled => self.toggled += 1,
        }
    }

    fn view(&self) -> Element<Msg> {
        Element::new(Overlay)
    }

    fn on_key(&self, key: &KeyEvent) -> Option<Msg> {
        (key.key == Key::F(1)).then_some(Msg::Toggled)
    }
}

fn press(at: Point) -> Event {
    Event::PointerPressed { pos: at, button: PointerButton::Primary }
}

fn key(key: Key) -> Event {
    Event::Key(KeyEvent { key, pressed: true, repeat: false, modifiers: Modifiers::default(), text: None })
}

#[test]
fn a_host_is_told_which_events_the_interface_took() {
    let mut h = Harness::new(Hud::default(), Size::new(320.0, 200.0)).expect("a GPU adapter is required for these tests");
    // Before anything is drawn there is nothing to be on.
    assert_eq!(h.event(press(Point::new(100.0, 50.0))), Status::Ignored);
    h.render(1.0);

    // On the button: the widget's own. On the panel beside it: no widget
    // wants it, but it is not for what is behind the panel either.
    assert_eq!((h.event(press(Point::new(30.0, 30.0))), h.app().pressed), (Status::Captured, 1));
    assert_eq!((h.event(press(Point::new(100.0, 50.0))), h.app().pressed), (Status::Captured, 1));
    assert_eq!(h.event(Event::PointerReleased { pos: Point::new(100.0, 50.0), button: PointerButton::Primary }), Status::Captured);
    assert_eq!(h.event(Event::Wheel { pos: Point::new(100.0, 50.0), delta: Point::new(0.0, -10.0) }), Status::Captured);
    // Off the panel, in its shadow, and where something was drawn that cannot be seen: the host's.
    for off in [Point::new(250.0, 150.0), Point::new(70.0, 78.0), Point::new(220.0, 120.0)] {
        assert_eq!(h.event(press(off)), Status::Ignored, "{off:?}");
        assert_eq!(h.event(Event::Wheel { pos: off, delta: Point::new(0.0, -10.0) }), Status::Ignored);
    }
    assert_eq!(h.app().pressed, 1);
    // Moving the pointer is nobody's in particular, wherever it is.
    assert_eq!(h.event(Event::PointerMoved { pos: Point::new(30.0, 30.0) }), Status::Ignored);

    // A key the app acts on is taken; one it has no use for is the host's,
    // and so is Tab where there is nothing to move between.
    assert_eq!((h.event(key(Key::F(1))), h.app().toggled), (Status::Captured, 1));
    assert_eq!(h.event(key(Key::Character("w".into()))), Status::Ignored);
    assert_eq!(h.event(key(Key::Tab)), Status::Ignored);
}
