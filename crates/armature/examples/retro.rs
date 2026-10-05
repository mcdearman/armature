//! A second look, built on the framework alone: square corners, hard
//! outlines and solid offset shadows, nothing like the Neo toolkit. It shows
//! what a toolkit has to supply: a theme, a few controls that read it, and
//! the window's background.
//!
//! `cargo run -p armature --example retro`

use armature::widgets::{column, row};
use armature::{App, Chrome, Color, CursorIcon, Cx, DrawCx, Element, Event, EventCx, FontFamily, Key, Limits, Point, PointerButton, Rect, Scheme, Size, Status, Style, TextLayout, TextStyle, Widget};

/// The whole theme: three colours.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Retro {
    pub paper: Color,
    pub ink: Color,
    /// Fill for things that can be pressed.
    pub pop: Color,
}

impl Retro {
    pub fn for_scheme(scheme: Scheme) -> Self {
        match scheme {
            Scheme::Light => Self { paper: Color::hex(0xfff4d6), ink: Color::hex(0x111111), pop: Color::hex(0xffd23f) },
            Scheme::Dark => Self { paper: Color::hex(0x10241a), ink: Color::hex(0x7dffb0), pop: Color::hex(0x1f6f46) },
        }
    }

    /// The theme the app attached to the window's style.
    fn of(cx: &Cx) -> Self {
        cx.style::<Retro>().copied().unwrap_or_else(|| Self::for_scheme(cx.scheme()))
    }

    pub fn style(self) -> Style {
        Style::new(self).content(self.ink).text(LABEL)
    }
}

const LABEL: TextStyle = TextStyle { size: 18.0, weight: 700, family: FontFamily::Mono, line_height: 1.3, letter_spacing: 0.0 };
/// How far a button's shadow sits from it, and how far it sinks when pressed.
pub const DEPTH: f32 = 4.0;
const OUTLINE: f32 = 2.0;

/// A button with a hard outline that sinks onto its shadow when pressed.
pub struct PushButton<M> {
    label: String,
    on_press: M,
    text: Option<TextLayout>,
}

#[derive(Default)]
struct Held(bool);

/// A [`PushButton`] that sends `on_press`.
pub fn push_button<M: Clone + 'static>(label: impl Into<String>, on_press: M) -> Element<M> {
    Element::new(PushButton { label: label.into(), on_press, text: None })
}

impl<M: Clone + 'static> Widget<M> for PushButton<M> {
    fn layout(&mut self, cx: &mut Cx, limits: Limits) -> Size {
        let text = cx.text().layout(&self.label, &LABEL, None);
        let size = limits.resolve(Size::new(text.size().w + 36.0 + DEPTH, text.size().h + 20.0 + DEPTH));
        self.text = Some(text);
        size
    }

    fn focusable(&self) -> bool {
        true
    }

    fn draw(&self, cx: &mut DrawCx) {
        let theme = Retro::of(cx);
        let b = cx.bounds();
        let sink = if cx.state::<Held>().0 { DEPTH } else { 0.0 };
        let face = Rect::new(b.x + sink, b.y + sink, b.w - DEPTH, b.h - DEPTH);
        cx.scene.fill(Rect::new(b.x + DEPTH, b.y + DEPTH, face.w, face.h), 0.0, theme.ink, None);
        let outline = if cx.focus_visible() { OUTLINE * 2.0 } else { OUTLINE };
        cx.scene.fill(face, 0.0, theme.pop, Some((outline, theme.ink)));
        if let Some(text) = &self.text {
            cx.scene.text(text, Point::new(face.x + (face.w - text.size().w) * 0.5, face.y + (face.h - text.size().h) * 0.5), theme.ink);
        }
    }

    fn event(&mut self, cx: &mut EventCx<M>, event: &Event) -> Status {
        let b = cx.bounds();
        match event {
            Event::PointerMoved { pos } if b.contains(*pos) => {
                cx.set_cursor(CursorIcon::Pointer);
                Status::Ignored
            }
            Event::PointerPressed { pos, button: PointerButton::Primary } if b.contains(*pos) => {
                cx.state::<Held>().0 = true;
                cx.request_redraw();
                Status::Captured
            }
            Event::PointerReleased { pos, button: PointerButton::Primary } => {
                if std::mem::take(&mut cx.state::<Held>().0) {
                    cx.request_redraw();
                    if b.contains(*pos) {
                        cx.emit(self.on_press.clone());
                    }
                }
                Status::Ignored
            }
            Event::Key(k) if k.pressed && cx.is_focused() && matches!(k.key, Key::Enter | Key::Space) => {
                cx.emit(self.on_press.clone());
                Status::Captured
            }
            _ => Status::Ignored,
        }
    }
}

/// Paints the window's paper behind the view.
struct Paper<M>([Element<M>; 1]);

impl<M: 'static> Widget<M> for Paper<M> {
    fn children_mut(&mut self) -> &mut [Element<M>] {
        &mut self.0
    }

    fn layout(&mut self, cx: &mut Cx, limits: Limits) -> Size {
        self.0[0].layout(cx, Limits::loose(limits.max));
        self.0[0].set_position(Point::ZERO);
        limits.max
    }

    fn draw(&self, cx: &mut DrawCx) {
        let theme = Retro::of(cx);
        let b = cx.bounds();
        cx.scene.fill(b, 0.0, theme.paper, None);
        self.0[0].draw(cx);
    }

    fn event(&mut self, cx: &mut EventCx<M>, event: &Event) -> Status {
        self.0[0].event(cx, event)
    }
}

#[derive(Default)]
pub struct Counter {
    pub count: i32,
}

#[derive(Clone, Debug)]
pub enum Msg {
    Less,
    More,
}

impl App for Counter {
    type Message = Msg;

    fn title(&self) -> String {
        "Retro".into()
    }

    fn window(&self) -> armature::WindowSettings {
        armature::WindowSettings { size: Size::new(320.0, 180.0), decorations: armature::Decorations::System, ..Default::default() }
    }

    fn update(&mut self, m: Msg) {
        match m {
            Msg::Less => self.count -= 1,
            Msg::More => self.count += 1,
        }
    }

    fn style(&self, system: Scheme) -> Style {
        Retro::for_scheme(system).style()
    }

    fn frame(&self, view: Element<Msg>, _chrome: Chrome) -> Element<Msg> {
        Element::new(Paper([view]))
    }

    // 24px padding; the count on the first line, the buttons 16px below it.
    fn view(&self) -> Element<Msg> {
        column()
            .padding(24.0)
            .spacing(16.0)
            .push(format!("COUNT {}", self.count))
            .push(row().spacing(16.0).push(push_button("LESS", Msg::Less)).push(push_button("MORE", Msg::More)))
            .into()
    }
}

#[allow(dead_code)]
fn main() -> Result<(), armature::Error> {
    armature::run(Counter::default())
}
