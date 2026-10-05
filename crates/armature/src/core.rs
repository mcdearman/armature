use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use armature_render::{Color, Rect, Scene, Size, TextSystem, Point};

use crate::event::{Event, Status};

/// Stable identity of a widget across view rebuilds, derived from its
/// position in the tree (or an explicit key) and its type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct WidgetId(pub u64);

impl WidgetId {
    pub(crate) fn child(self, discriminant: u64, ty: TypeId) -> WidgetId {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.0.hash(&mut h);
        discriminant.hash(&mut h);
        ty.hash(&mut h);
        WidgetId(h.finish())
    }
}

/// How a widget sizes itself along one axis.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Length {
    /// As small as the content allows.
    #[default]
    Shrink,
    /// Take all available space.
    Fill,
    /// Take a share of the available space relative to other `Portion`s.
    Portion(u16),
    /// Exactly this many logical pixels.
    Fixed(f32),
}

impl Length {
    pub fn is_fill(&self) -> bool {
        matches!(self, Length::Fill | Length::Portion(_))
    }

    pub fn portion(&self) -> u16 {
        match self {
            Length::Fill => 1,
            Length::Portion(p) => *p,
            _ => 0,
        }
    }
}

impl From<f32> for Length {
    fn from(v: f32) -> Self {
        Length::Fixed(v)
    }
}

impl From<i32> for Length {
    fn from(v: i32) -> Self {
        Length::Fixed(v as f32)
    }
}

/// Alignment along an axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
    /// Children fill the cross axis.
    Stretch,
}

impl Align {
    /// How far from the start to place content that leaves `free` space.
    pub fn offset(self, free: f32) -> f32 {
        match self {
            Align::Start | Align::Stretch => 0.0,
            Align::Center => (free * 0.5).max(0.0).round(),
            Align::End => free.max(0.0),
        }
    }
}

/// Space inside a widget's edges.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Padding {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Padding {
    pub const ZERO: Padding = Padding { top: 0.0, right: 0.0, bottom: 0.0, left: 0.0 };

    pub const fn all(v: f32) -> Self {
        Self { top: v, right: v, bottom: v, left: v }
    }

    pub const fn xy(x: f32, y: f32) -> Self {
        Self { top: y, right: x, bottom: y, left: x }
    }

    pub fn horizontal(&self) -> f32 {
        self.left + self.right
    }

    pub fn vertical(&self) -> f32 {
        self.top + self.bottom
    }
}

impl From<f32> for Padding {
    fn from(v: f32) -> Self {
        Padding::all(v)
    }
}

impl From<i32> for Padding {
    fn from(v: i32) -> Self {
        Padding::all(v as f32)
    }
}

impl From<[f32; 2]> for Padding {
    fn from([x, y]: [f32; 2]) -> Self {
        Padding::xy(x, y)
    }
}

impl From<[f32; 4]> for Padding {
    fn from([top, right, bottom, left]: [f32; 4]) -> Self {
        Padding { top, right, bottom, left }
    }
}

/// Minimum and maximum size a widget may take.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub min: Size,
    pub max: Size,
}

impl Limits {
    pub fn new(min: Size, max: Size) -> Self {
        Self { min, max }
    }

    pub fn loose(max: Size) -> Self {
        Self { min: Size::ZERO, max }
    }

    pub fn tight(s: Size) -> Self {
        Self { min: s, max: s }
    }

    /// Removes padding from both bounds.
    pub fn shrink(&self, p: Padding) -> Limits {
        let sub = |v: f32, d: f32| if v.is_finite() { (v - d).max(0.0) } else { v };
        Limits {
            min: Size::new(sub(self.min.w, p.horizontal()), sub(self.min.h, p.vertical())),
            max: Size::new(sub(self.max.w, p.horizontal()), sub(self.max.h, p.vertical())),
        }
    }

    /// Applies a widget's own width and height preferences.
    pub fn constrain(&self, width: Length, height: Length) -> Limits {
        let axis = |len: Length, min: f32, max: f32| match len {
            Length::Fixed(v) => {
                let v = v.clamp(min, max.max(min));
                (v, v)
            }
            Length::Fill | Length::Portion(_) if max.is_finite() => (max, max),
            _ => (min, max),
        };
        let (wmin, wmax) = axis(width, self.min.w, self.max.w);
        let (hmin, hmax) = axis(height, self.min.h, self.max.h);
        Limits { min: Size::new(wmin, hmin), max: Size::new(wmax, hmax) }
    }

    /// The final size for content of size `content` under these limits.
    pub fn resolve(&self, content: Size) -> Size {
        let clamp = |v: f32, lo: f32, hi: f32| v.max(lo).min(hi.max(lo));
        Size::new(clamp(content.w, self.min.w, self.max.w), clamp(content.h, self.min.h, self.max.h))
    }

    pub fn with_max_width(mut self, w: f32) -> Limits {
        self.max.w = self.max.w.min(w);
        self.min.w = self.min.w.min(self.max.w);
        self
    }
}

/// A requested change to the window, raised by widgets such as the title bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowRequest {
    /// Start a system drag of these files, so they can be dropped on other
    /// apps. Raise it while handling the pointer movement that began the
    /// drag. Only macOS supports it so far.
    DragFiles(Vec<std::path::PathBuf>),
    Drag,
    Resize(ResizeEdge),
    Minimize,
    ToggleMaximize,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeEdge {
    North,
    South,
    East,
    West,
    NorthEast,
    NorthWest,
    SouthEast,
    SouthWest,
}

/// Mouse cursor shapes a widget can ask for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorIcon {
    #[default]
    Default,
    Pointer,
    Text,
    Grab,
    Grabbing,
    Resize(ResizeEdge),
}

/// Per-widget state that survives view rebuilds.
#[derive(Default)]
pub(crate) struct StateStore {
    map: HashMap<WidgetId, Box<dyn Any>>,
    seen: HashSet<WidgetId>,
}

impl StateStore {
    fn get<T: Default + 'static>(&mut self, id: WidgetId) -> &mut T {
        self.seen.insert(id);
        let entry = self.map.entry(id).or_insert_with(|| Box::new(T::default()));
        if !entry.is::<T>() {
            *entry = Box::new(T::default());
        }
        entry.downcast_mut::<T>().expect("state type checked above")
    }

    /// Forgets state for widgets that are no longer in the tree.
    pub(crate) fn sweep(&mut self, live: &HashSet<WidgetId>) {
        self.seen.clear();
        self.map.retain(|id, _| live.contains(id));
    }
}

/// Everything shared by the layout, draw and event passes.
pub(crate) struct Shared<'a> {
    pub text: &'a mut TextSystem,
    pub style: crate::app::Style,
    pub scheme: crate::app::Scheme,
    pub states: &'a mut StateStore,
    pub runtime: &'a mut RuntimeState,
}

/// Runtime bookkeeping kept between frames.
#[derive(Default)]
pub(crate) struct RuntimeState {
    pub now: Option<Instant>,
    pub focus: Option<WidgetId>,
    pub focus_visible: bool,
    pub focus_chain: Vec<WidgetId>,
    pub animating: bool,
    pub redraw: bool,
    pub relayout: bool,
    pub wake_at: Option<Instant>,
    pub cursor: CursorIcon,
    pub window_requests: Vec<WindowRequest>,
    pub pointer: Option<Point>,
    pub clipboard_out: Option<String>,
    pub clipboard_in: Option<String>,
    pub content_color: Option<Color>,
    pub window_focused: bool,
    pub modifiers: crate::event::Modifiers,
    /// Reads the system clipboard on demand. Set by the windowing shell.
    pub clipboard_reader: Option<Box<dyn FnMut() -> Option<String>>>,
    /// Messages sent outside the event pass, as the app's message type.
    pub deferred: Vec<Box<dyn std::any::Any>>,
}

/// Context handed to every widget method.
pub struct Cx<'a, 'b> {
    pub(crate) shared: &'a mut Shared<'b>,
    pub(crate) id: WidgetId,
    pub(crate) bounds: Rect,
}

impl<'a, 'b> Cx<'a, 'b> {
    pub fn id(&self) -> WidgetId {
        self.id
    }

    /// This widget's rectangle in window coordinates. Valid after layout.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// The theme the app attached with [`Style::new`](crate::Style::new),
    /// if it is a `T`. Toolkits wrap this in a typed accessor.
    pub fn style<T: 'static>(&self) -> Option<&T> {
        self.shared.style.get()
    }

    /// The style's default text style, which [`Label`](crate::widgets::Label) uses.
    pub fn text_style(&self) -> armature_render::TextStyle {
        self.shared.style.text_style()
    }

    /// The desktop's light or dark preference.
    pub fn scheme(&self) -> crate::app::Scheme {
        self.shared.scheme
    }

    pub fn text(&mut self) -> &mut TextSystem {
        self.shared.text
    }

    /// This widget's persistent state, created with `Default` on first use.
    pub fn state<T: Default + 'static>(&mut self) -> &mut T {
        self.shared.states.get::<T>(self.id)
    }

    pub fn now(&self) -> Instant {
        self.shared.runtime.now.unwrap_or_else(Instant::now)
    }

    /// Keep drawing frames; call while an animation is running.
    pub fn request_animation(&mut self) {
        self.shared.runtime.animating = true;
    }

    /// Redraw once more (for example after a hover change).
    pub fn request_redraw(&mut self) {
        self.shared.runtime.redraw = true;
    }

    /// Re-run layout before the next frame (for example after scrolling).
    pub fn request_layout(&mut self) {
        self.shared.runtime.relayout = true;
        self.shared.runtime.redraw = true;
    }

    /// Redraw after `delay` (for example to blink a caret).
    pub fn request_redraw_after(&mut self, delay: Duration) {
        let at = self.now() + delay;
        let w = &mut self.shared.runtime.wake_at;
        *w = Some(w.map_or(at, |x| x.min(at)));
    }

    pub fn is_focused(&self) -> bool {
        self.shared.runtime.focus == Some(self.id) && self.shared.runtime.window_focused
    }

    /// Whether focus should be drawn (it moved by keyboard, not by click).
    pub fn focus_visible(&self) -> bool {
        self.is_focused() && self.shared.runtime.focus_visible
    }

    pub fn request_focus(&mut self) {
        self.shared.runtime.focus = Some(self.id);
        self.shared.runtime.focus_visible = false;
        self.shared.runtime.redraw = true;
    }

    pub fn release_focus(&mut self) {
        if self.shared.runtime.focus == Some(self.id) {
            self.shared.runtime.focus = None;
        }
    }

    pub fn set_cursor(&mut self, c: CursorIcon) {
        self.shared.runtime.cursor = c;
    }

    pub fn window_request(&mut self, r: WindowRequest) {
        self.shared.runtime.window_requests.push(r);
    }

    /// Modifier keys currently held, for example to extend a selection on shift-click.
    pub fn modifiers(&self) -> crate::event::Modifiers {
        self.shared.runtime.modifiers
    }

    /// Latest pointer position in window coordinates.
    pub fn pointer(&self) -> Option<Point> {
        self.shared.runtime.pointer
    }

    pub fn is_hovered(&self) -> bool {
        self.pointer().is_some_and(|p| self.bounds.contains(p))
    }

    /// Sends a message to the app after the current pass, for example when
    /// layout finds that a widget's size changed. `M` must be the app's
    /// message type; anything else is dropped.
    pub fn defer<M: 'static>(&mut self, message: M) {
        self.shared.runtime.deferred.push(Box::new(message));
        self.shared.runtime.redraw = true;
    }

    /// Put text on the system clipboard.
    pub fn copy(&mut self, text: String) {
        self.shared.runtime.clipboard_out = Some(text);
    }

    /// Reads the system clipboard now. Slower than [`clipboard`](Self::clipboard),
    /// so call it only when the text is needed.
    pub fn read_clipboard(&mut self) -> Option<String> {
        if let Some(t) = &self.shared.runtime.clipboard_in {
            return Some(t.clone());
        }
        self.shared.runtime.clipboard_reader.as_mut().and_then(|r| r())
    }

    /// Text from the system clipboard, read when a paste shortcut arrives.
    pub fn clipboard(&self) -> Option<&str> {
        self.shared.runtime.clipboard_in.as_deref()
    }

    /// Colour for text and icons that do not set their own, which follows
    /// the surface they sit on (for example accent text on a pressed button).
    pub fn content_color(&self) -> Color {
        self.shared.runtime.content_color.unwrap_or_else(|| self.shared.style.content_color())
    }

    pub(crate) fn reborrow<'c>(&'c mut self, id: WidgetId, bounds: Rect) -> Cx<'c, 'b> {
        Cx { shared: &mut *self.shared, id, bounds }
    }
}

/// Context for drawing.
pub struct DrawCx<'a, 'b> {
    pub cx: Cx<'a, 'b>,
    pub scene: &'a mut Scene,
}

impl<'a, 'b> std::ops::Deref for DrawCx<'a, 'b> {
    type Target = Cx<'a, 'b>;
    fn deref(&self) -> &Self::Target {
        &self.cx
    }
}

impl<'a, 'b> std::ops::DerefMut for DrawCx<'a, 'b> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.cx
    }
}

impl<'a, 'b> DrawCx<'a, 'b> {
    /// Draws children with `color` as the inherited content colour.
    pub fn with_content_color(&mut self, color: Color, f: impl FnOnce(&mut DrawCx<'_, 'b>)) {
        let prev = self.cx.shared.runtime.content_color.replace(color);
        f(self);
        self.cx.shared.runtime.content_color = prev;
    }
}

/// Context for event handling. Collects messages for the application.
pub struct EventCx<'a, 'b, M> {
    pub cx: Cx<'a, 'b>,
    pub(crate) messages: &'a mut Vec<M>,
}

impl<'a, 'b, M> std::ops::Deref for EventCx<'a, 'b, M> {
    type Target = Cx<'a, 'b>;
    fn deref(&self) -> &Self::Target {
        &self.cx
    }
}

impl<'a, 'b, M> std::ops::DerefMut for EventCx<'a, 'b, M> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.cx
    }
}

impl<'a, 'b, M> EventCx<'a, 'b, M> {
    /// Sends a message to the application's `update`.
    pub fn emit(&mut self, m: M) {
        self.messages.push(m);
        self.cx.shared.runtime.redraw = true;
    }
}

/// A node in the user interface. Implement this to make custom widgets.
pub trait Widget<M> {
    /// Computes this widget's size within `limits` and lays out children.
    fn layout(&mut self, cx: &mut Cx, limits: Limits) -> Size;

    fn draw(&self, cx: &mut DrawCx);

    fn event(&mut self, _cx: &mut EventCx<M>, _event: &Event) -> Status {
        Status::Ignored
    }

    /// Direct children, used for identity and positioning.
    fn children_mut(&mut self) -> &mut [Element<M>] {
        &mut []
    }

    /// Visits every direct child, whatever its message type. Override only
    /// when children are not all in `children_mut` (see `Map`).
    fn visit(&mut self, f: &mut dyn FnMut(&mut dyn Node)) {
        for c in self.children_mut() {
            f(c);
        }
    }

    /// Whether Tab can move keyboard focus here.
    fn focusable(&self) -> bool {
        false
    }

    /// Preferred width, used by rows and columns to share space.
    fn width(&self) -> Length {
        Length::Shrink
    }

    /// Preferred height, used by rows and columns to share space.
    fn height(&self) -> Length {
        Length::Shrink
    }
}

/// A widget plus the bookkeeping that places it in the tree.
pub struct Element<M> {
    widget: Box<dyn Widget<M>>,
    type_id: TypeId,
    key: Option<u64>,
    id: WidgetId,
    rel: Point,
    bounds: Rect,
}

impl<M: 'static> Element<M> {
    pub fn new<W: Widget<M> + 'static>(widget: W) -> Self {
        Self {
            widget: Box::new(widget),
            type_id: TypeId::of::<W>(),
            key: None,
            id: WidgetId::default(),
            rel: Point::ZERO,
            bounds: Rect::ZERO,
        }
    }

    /// Gives this element an explicit identity so its state follows it
    /// when siblings are inserted or removed.
    pub fn key(mut self, key: impl Hash) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h);
        self.key = Some(h.finish());
        self
    }

    /// Converts this element's messages with `f`.
    pub fn map<N: 'static>(self, f: impl Fn(M) -> N + 'static) -> Element<N> {
        Element::new(crate::widgets::map::Map::new(self, f))
    }
}

impl<M> Element<M> {
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    pub fn width(&self) -> Length {
        self.widget.width()
    }

    pub fn height(&self) -> Length {
        self.widget.height()
    }


    /// Lays out this element. Call [`set_position`](Self::set_position) afterwards.
    pub fn layout(&mut self, cx: &mut Cx, limits: Limits) -> Size {
        let mut child = cx.reborrow(self.id, Rect::ZERO);
        let size = self.widget.layout(&mut child, limits);
        self.bounds.w = size.w;
        self.bounds.h = size.h;
        size
    }

    /// Sets this element's position relative to its parent's origin.
    pub fn set_position(&mut self, p: Point) {
        self.rel = p;
    }


    pub fn draw(&self, cx: &mut DrawCx) {
        let mut child = DrawCx { cx: cx.cx.reborrow(self.id, self.bounds), scene: &mut *cx.scene };
        self.widget.draw(&mut child);
    }

    pub fn event(&mut self, cx: &mut EventCx<M>, event: &Event) -> Status {
        let mut child = EventCx { cx: cx.cx.reborrow(self.id, self.bounds), messages: &mut *cx.messages };
        self.widget.event(&mut child, event)
    }

    /// Offers `event` to children from topmost to bottom, stopping when one
    /// captures a press, wheel or key event.
    pub fn event_children(children: &mut [Element<M>], cx: &mut EventCx<M>, event: &Event) -> Status {
        let stoppable = matches!(event, Event::PointerPressed { .. } | Event::Wheel { .. } | Event::Pinch { .. } | Event::FilesDropped { .. } | Event::Key(_) | Event::Ime(_));
        let mut status = Status::Ignored;
        for c in children.iter_mut().rev() {
            status = status.merge(c.event(cx, event));
            if stoppable && status == Status::Captured {
                break;
            }
        }
        status
    }
}

/// Tree operations that do not depend on an element's message type.
/// Output of the identity pass.
#[derive(Default)]
pub struct IdPass {
    /// Focusable widgets in tree order, for Tab navigation.
    pub focus_chain: Vec<WidgetId>,
    /// Every widget in the tree.
    pub live: HashSet<WidgetId>,
}

pub trait Node {
    /// Assigns identities to this subtree.
    fn assign_ids(&mut self, parent: WidgetId, index: usize, pass: &mut IdPass);
    /// Resolves window-space bounds for this subtree.
    fn place(&mut self, parent_origin: Point);
}

impl<M> Node for Element<M> {
    fn assign_ids(&mut self, parent: WidgetId, index: usize, pass: &mut IdPass) {
        self.id = parent.child(self.key.unwrap_or(index as u64 ^ 0x9e37_79b9_7f4a_7c15), self.type_id);
        if self.widget.focusable() {
            pass.focus_chain.push(self.id);
        }
        pass.live.insert(self.id);
        let id = self.id;
        let mut i = 0;
        self.widget.visit(&mut |c| {
            c.assign_ids(id, i, pass);
            i += 1;
        });
    }

    fn place(&mut self, parent_origin: Point) {
        self.bounds.x = parent_origin.x + self.rel.x;
        self.bounds.y = parent_origin.y + self.rel.y;
        let origin = self.bounds.origin();
        self.widget.visit(&mut |c| c.place(origin));
    }
}

/// Lets `&str`, `String` and widgets be used wherever an element is expected.
impl<M: 'static> From<&str> for Element<M> {
    fn from(s: &str) -> Self {
        Element::new(crate::widgets::Label::new(s))
    }
}

impl<M: 'static> From<String> for Element<M> {
    fn from(s: String) -> Self {
        Element::new(crate::widgets::Label::new(s))
    }
}
