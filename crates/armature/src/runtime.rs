use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use armature_render::{Color, Point, Rect, Scene, Size, TextSystem};

use crate::app::{App, Chrome, Decorations, Proxy, Scheme, Style};
use crate::core::{CursorIcon, Cx, DrawCx, Element, EventCx, IdPass, Limits, Node, RuntimeState, Shared, StateStore, WidgetId, WindowRequest};
use crate::event::{Event, Key, Status};

/// Runs an [`App`] without a window: builds the view, lays it out, routes
/// events and draws scenes. The windowing shell and the test harness both
/// drive this.
pub struct Ui<A: App> {
    app: A,
    root: Option<Element<A::Message>>,
    states: StateStore,
    rt: RuntimeState,
    size: Size,
    system_scheme: Scheme,
    style: Style,
    decorations: Decorations,
    maximized: bool,
    needs_view: bool,
    needs_layout: bool,
    timers: Vec<(Duration, Instant)>,
    inbox: Option<Receiver<A::Message>>,
}

impl<A: App> Ui<A> {
    pub fn new(app: A, size: Size, system_scheme: Scheme) -> Self {
        let decorations = app.window().decorations;
        let style = app.style(system_scheme);
        Self {
            app,
            root: None,
            states: StateStore::default(),
            rt: RuntimeState { window_focused: true, ..Default::default() },
            size,
            system_scheme,
            style,
            decorations,
            maximized: false,
            needs_view: true,
            needs_layout: true,
            timers: vec![],
            inbox: None,
        }
    }

    /// Calls [`App::start`] with a proxy whose sends call `wake`.
    pub fn start(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        let (tx, rx) = channel();
        self.inbox = Some(rx);
        self.app.start(Proxy { tx, wake });
        self.needs_view = true;
    }

    pub fn app(&self) -> &A {
        &self.app
    }

    /// Mutable access to the application. The view is rebuilt afterwards.
    pub fn app_mut(&mut self) -> &mut A {
        self.needs_view = true;
        &mut self.app
    }

    pub fn style(&self) -> &Style {
        &self.style
    }

    /// Blur strength for the window's backdrop, if the style asks for one.
    pub fn backdrop_blur(&self) -> Option<f32> {
        self.style.blur()
    }

    /// Corner radius of the window as the app draws it: zero with system
    /// decorations or when maximized.
    pub fn window_radius(&self) -> f32 {
        if self.decorations == Decorations::Custom && !self.maximized { self.style.radius() } else { 0.0 }
    }

    pub fn title(&self) -> String {
        self.app.title()
    }

    pub fn window_state(&self) -> crate::app::WindowState {
        self.app.window_state()
    }

    pub fn should_exit(&self) -> bool {
        self.app.should_exit()
    }

    /// The user asked to close the window. Returns true if the app should end.
    pub fn close_requested(&mut self) -> bool {
        match self.app.on_close() {
            Some(m) => {
                self.app.update(m);
                self.needs_view = true;
                false
            }
            None => true,
        }
    }

    /// Tells the app where its window is on the screen.
    pub fn window_geometry(&mut self, geometry: crate::app::WindowGeometry) {
        if let Some(m) = self.app.on_window_geometry(geometry) {
            self.app.update(m);
            self.needs_view = true;
        }
    }

    pub fn resize(&mut self, size: Size) {
        if size != self.size {
            self.size = size;
            self.needs_layout = true;
        }
    }

    pub fn set_system_scheme(&mut self, s: Scheme) {
        self.system_scheme = s;
        self.refresh_style();
    }

    pub fn set_maximized(&mut self, m: bool) {
        if m != self.maximized {
            self.maximized = m;
            self.needs_view = true;
        }
    }

    pub fn set_window_focused(&mut self, f: bool) {
        self.rt.window_focused = f;
        self.rt.redraw = true;
    }

    /// Modifier keys currently held, as reported by the platform.
    pub fn set_modifiers(&mut self, m: crate::event::Modifiers) {
        self.rt.modifiers = m;
    }

    /// Lets widgets read the system clipboard on demand.
    pub fn set_clipboard_reader(&mut self, reader: Box<dyn FnMut() -> Option<String>>) {
        self.rt.clipboard_reader = Some(reader);
    }

    /// Text to hand to widgets on the next paste shortcut.
    pub fn set_clipboard(&mut self, text: Option<String>) {
        self.rt.clipboard_in = text;
    }

    pub fn take_clipboard(&mut self) -> Option<String> {
        self.rt.clipboard_out.take()
    }

    pub fn take_window_requests(&mut self) -> Vec<WindowRequest> {
        std::mem::take(&mut self.rt.window_requests)
    }

    pub fn cursor(&self) -> CursorIcon {
        self.rt.cursor
    }

    /// Whether anything changed that needs a new frame.
    pub fn needs_redraw(&self) -> bool {
        self.rt.redraw || self.rt.animating || self.needs_view || self.needs_layout
    }

    fn refresh_style(&mut self) {
        let style = self.app.style(self.system_scheme);
        if style != self.style {
            self.style = style;
            self.needs_layout = true;
            self.rt.redraw = true;
        }
    }

    fn ensure(&mut self, text: &mut TextSystem) {
        self.refresh_style();
        if self.needs_view {
            let bare = self.app.window_state().bare;
            let title_bar = self.decorations == Decorations::Custom && !bare;
            let chrome = Chrome { title: self.app.title(), title_bar, rounded: title_bar && !self.maximized, background: !bare };
            let mut root = self.app.frame(self.app.view(), chrome);
            let mut pass = IdPass::default();
            root.assign_ids(WidgetId(1), 0, &mut pass);
            self.states.sweep(&pass.live);
            self.rt.focus_chain = pass.focus_chain;
            if self.rt.focus.is_some_and(|f| !pass.live.contains(&f)) {
                self.rt.focus = None;
            }
            self.root = Some(root);
            self.needs_view = false;
            self.needs_layout = true;
        }
        if self.needs_layout || self.rt.relayout {
            self.rt.relayout = false;
            let root = self.root.as_mut().expect("view built above");
            let mut shared = Shared { text, style: self.style.clone(), scheme: self.system_scheme, states: &mut self.states, runtime: &mut self.rt };
            let mut cx = Cx { shared: &mut shared, id: WidgetId(0), bounds: Rect::ZERO };
            root.layout(&mut cx, Limits::tight(self.size));
            root.set_position(Point::ZERO);
            root.place(Point::ZERO);
            self.needs_layout = false;
        }
        self.apply_deferred();
    }

    /// Applies messages widgets sent with [`Cx::defer`](crate::Cx::defer).
    fn apply_deferred(&mut self) {
        if self.rt.deferred.is_empty() {
            return;
        }
        for m in std::mem::take(&mut self.rt.deferred) {
            if let Ok(m) = m.downcast::<A::Message>() {
                self.app.update(*m);
                self.needs_view = true;
            }
        }
    }

    /// Rebuilds and lays out the view if anything changed.
    pub fn refresh(&mut self, text: &mut TextSystem) {
        self.ensure(text);
    }

    /// Draws a frame.
    pub fn draw(&mut self, text: &mut TextSystem, now: Instant) -> Scene {
        self.ensure(text);
        self.rt.now = Some(now);
        self.rt.animating = false;
        self.rt.redraw = false;
        self.rt.wake_at = None;
        let mut scene = Scene::new(Color::TRANSPARENT);
        let root = self.root.as_ref().expect("view built in ensure");
        let mut shared = Shared { text, style: self.style.clone(), scheme: self.system_scheme, states: &mut self.states, runtime: &mut self.rt };
        let mut cx = DrawCx { cx: Cx { shared: &mut shared, id: WidgetId(0), bounds: root.bounds() }, scene: &mut scene };
        root.draw(&mut cx);
        scene
    }

    /// Routes an input event, then applies any resulting messages.
    pub fn event(&mut self, text: &mut TextSystem, event: Event) {
        self.ensure(text);
        if let Event::Key(k) = &event {
            self.rt.modifiers = k.modifiers;
        }
        match &event {
            Event::PointerLeft => self.rt.pointer = None,
            Event::PointerPressed { .. } => self.rt.focus_visible = false,
            Event::WindowFocus(f) => self.set_window_focused(*f),
            e => {
                if let Some(p) = e.pos() {
                    self.rt.pointer = Some(p);
                }
            }
        }
        if matches!(event, Event::PointerMoved { .. }) {
            self.rt.cursor = CursorIcon::Default;
        }
        let mut messages = Vec::new();
        let status = {
            let root = self.root.as_mut().expect("view built in ensure");
            let mut shared = Shared { text, style: self.style.clone(), scheme: self.system_scheme, states: &mut self.states, runtime: &mut self.rt };
            let mut cx = EventCx { cx: Cx { shared: &mut shared, id: WidgetId(0), bounds: root.bounds() }, messages: &mut messages };
            root.event(&mut cx, &event)
        };
        if let Event::Key(k) = &event
            && status == Status::Ignored
            && k.pressed
        {
            if let Some(m) = self.app.on_key(k) {
                messages.push(m);
            } else if k.key == Key::Tab {
                self.move_focus(!k.modifiers.shift);
            }
        }
        if let Event::WindowFocus(focused) = &event
            && let Some(m) = self.app.on_window_focus(*focused)
        {
            messages.push(m);
        }
        for m in messages {
            self.app.update(m);
            self.needs_view = true;
        }
        self.apply_deferred();
        if self.rt.relayout {
            self.needs_layout = true;
        }
        self.rt.clipboard_in = None;
    }

    fn move_focus(&mut self, forward: bool) {
        let chain = &self.rt.focus_chain;
        if chain.is_empty() {
            return;
        }
        let pos = self.rt.focus.and_then(|f| chain.iter().position(|&c| c == f));
        let next = match (pos, forward) {
            (None, true) => 0,
            (None, false) => chain.len() - 1,
            (Some(i), true) => (i + 1) % chain.len(),
            (Some(i), false) => (i + chain.len() - 1) % chain.len(),
        };
        self.rt.focus = Some(chain[next]);
        self.rt.focus_visible = true;
        self.rt.redraw = true;
    }

    /// Fires due timers. Returns when the runtime next needs to wake up.
    pub fn tick(&mut self, now: Instant) -> Option<Instant> {
        if let Some(rx) = &self.inbox {
            let messages: Vec<_> = rx.try_iter().collect();
            if !messages.is_empty() {
                for m in messages {
                    self.app.update(m);
                }
                self.needs_view = true;
            }
        }
        let subs = self.app.subscriptions();
        if subs.len() != self.timers.len() || subs.iter().zip(&self.timers).any(|(s, t)| s.period != t.0) {
            self.timers = subs.iter().map(|s| (s.period, now + s.period)).collect();
        }
        for (sub, timer) in subs.into_iter().zip(self.timers.iter_mut()) {
            if now >= timer.1 {
                self.app.update(sub.message);
                self.needs_view = true;
                timer.1 += sub.period;
                if timer.1 < now {
                    timer.1 = now + sub.period;
                }
            }
        }
        if self.rt.wake_at.is_some_and(|w| w <= now) {
            self.rt.wake_at = None;
            self.rt.redraw = true;
        }
        let next_timer = self.timers.iter().map(|t| t.1).min();
        match (next_timer, self.rt.wake_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}
