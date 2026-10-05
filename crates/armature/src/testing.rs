//! Drive an app without a window: send input, inspect state, save screenshots.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use armature_render::{Point, Renderer, Size};

use crate::app::{App, Scheme};
use crate::event::{Event, Key, KeyEvent, Modifiers, PointerButton};
use crate::runtime::Ui;

/// Runs an [`App`] headlessly on the GPU.
pub struct Harness<A: App> {
    ui: Ui<A>,
    renderer: Renderer,
    size: Size,
    clock: Instant,
    clipboard: Rc<RefCell<Option<String>>>,
}

impl<A: App> Harness<A> {
    /// Creates a harness with a window of `size` logical pixels.
    pub fn new(app: A, size: Size) -> Result<Self, String> {
        let renderer = Renderer::headless(app.fonts())?;
        let clipboard = Rc::new(RefCell::new(None));
        let mut ui = Ui::new(app, size, Scheme::Light);
        let reader = clipboard.clone();
        ui.set_clipboard_reader(Box::new(move || reader.borrow().clone()));
        ui.start(std::sync::Arc::new(|| {}));
        Ok(Self { ui, renderer, size, clock: Instant::now(), clipboard })
    }

    pub fn app(&self) -> &A {
        self.ui.app()
    }

    pub fn app_mut(&mut self) -> &mut A {
        self.ui.app_mut()
    }

    pub fn set_system_scheme(&mut self, s: Scheme) {
        self.ui.set_system_scheme(s);
    }

    pub fn resize(&mut self, size: Size) {
        self.size = size;
        self.ui.resize(size);
    }

    pub fn event(&mut self, e: Event) {
        self.ui.event(self.renderer.text(), e);
        self.ui.refresh(self.renderer.text());
        if let Some(t) = self.ui.take_clipboard() {
            *self.clipboard.borrow_mut() = Some(t);
        }
    }

    /// The simulated system clipboard: what widgets last copied, or what
    /// [`set_clipboard`](Self::set_clipboard) put there.
    pub fn clipboard(&self) -> Option<String> {
        self.clipboard.borrow().clone()
    }

    /// What widgets asked the window to do since the last call, such as
    /// starting a drag.
    pub fn take_window_requests(&mut self) -> Vec<crate::WindowRequest> {
        self.ui.take_window_requests()
    }

    /// The pointer shape the widgets asked for at the last pointer event.
    pub fn cursor(&self) -> crate::CursorIcon {
        self.ui.cursor()
    }

    /// The style the app chose for the current system scheme.
    pub fn style(&self) -> &crate::Style {
        self.ui.style()
    }

    pub fn title(&self) -> String {
        self.ui.title()
    }

    pub fn window_state(&self) -> crate::WindowState {
        self.ui.window_state()
    }

    /// True once the app has asked to end.
    pub fn should_exit(&self) -> bool {
        self.ui.should_exit()
    }

    /// Asks to close the window, as the close button does. Returns true if
    /// the window would close, false if the app handled it instead.
    pub fn close(&mut self) -> bool {
        let closes = self.ui.close_requested();
        self.ui.refresh(self.renderer.text());
        closes
    }

    /// Tells the app the window gained or lost keyboard focus.
    pub fn set_window_focused(&mut self, focused: bool) {
        self.event(Event::WindowFocus(focused));
    }

    /// The modifier keys held during the pointer events that follow.
    pub fn set_modifiers(&mut self, m: Modifiers) {
        self.ui.set_modifiers(m);
    }

    pub fn set_clipboard(&mut self, text: &str) {
        *self.clipboard.borrow_mut() = Some(text.to_owned());
    }

    pub fn move_to(&mut self, p: Point) {
        self.event(Event::PointerMoved { pos: p });
    }

    /// Moves to `p`, then presses and releases the primary button.
    pub fn click(&mut self, p: Point) {
        self.move_to(p);
        self.event(Event::PointerPressed { pos: p, button: PointerButton::Primary });
        self.event(Event::PointerReleased { pos: p, button: PointerButton::Primary });
    }

    pub fn key(&mut self, key: Key, modifiers: Modifiers) {
        let text = match &key {
            Key::Character(c) if !modifiers.command() => Some(c.clone()),
            Key::Space => Some(" ".into()),
            _ => None,
        };
        for pressed in [true, false] {
            self.event(Event::Key(KeyEvent { key: key.clone(), pressed, repeat: false, modifiers, text: text.clone() }));
        }
    }

    /// Presses the paste shortcut with `text` on the clipboard.
    pub fn paste(&mut self, text: &str, command: Modifiers) {
        self.ui.set_clipboard(Some(text.to_owned()));
        self.event(Event::Key(KeyEvent { key: Key::Character("v".into()), pressed: true, repeat: false, modifiers: command, text: None }));
    }

    /// Types `text` one character at a time.
    pub fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let key = if c == ' ' { Key::Space } else { Key::Character(c.to_string()) };
            self.event(Event::Key(KeyEvent { key, pressed: true, repeat: false, modifiers: Modifiers::default(), text: Some(c.to_string()) }));
        }
    }

    /// Advances the clock, firing timers and finishing animations.
    pub fn advance(&mut self, d: Duration) {
        self.clock += d;
        self.ui.tick(self.clock);
        let _ = self.ui.draw(self.renderer.text(), self.clock);
    }

    /// Renders the current state and returns straight-alpha RGBA pixels.
    pub fn render(&mut self, scale: f32) -> Vec<u8> {
        // Two frames so that state transitions settle at their resting values.
        let _ = self.ui.draw(self.renderer.text(), self.clock);
        self.clock += Duration::from_secs(1);
        let scene = self.ui.draw(self.renderer.text(), self.clock);
        let (w, h) = ((self.size.w * scale) as u32, (self.size.h * scale) as u32);
        self.renderer.render_to_rgba(&scene, w, h, scale)
    }

    /// Renders the current state to a PNG file.
    pub fn save_png(&mut self, path: impl AsRef<Path>, scale: f32) -> std::io::Result<()> {
        let _ = self.ui.draw(self.renderer.text(), self.clock);
        self.clock += Duration::from_secs(1);
        let scene = self.ui.draw(self.renderer.text(), self.clock);
        let (w, h) = ((self.size.w * scale) as u32, (self.size.h * scale) as u32);
        self.renderer.save_png(&scene, w, h, scale, path)
    }
}
