use std::time::{Duration, Instant};

use armature_render::{Point, Rect};

use crate::core::{CursorIcon, Cx};
use crate::event::{Event, Key, PointerButton, Status};

/// What a text field's widget reads to paint itself.
#[derive(Default)]
pub struct FieldState {
    /// Caret position as a char index into the value.
    pub cursor: usize,
    /// Other end of the selection, as a char index.
    pub anchor: usize,
    pub dragging: bool,
    pub hovered: bool,
    /// How far the text has slid left to keep the caret in view.
    pub scroll: f32,
    /// When the caret last moved, which restarts its blink.
    pub blink_origin: Option<Instant>,
    /// Set once the field has been laid out, so autofocus happens only once.
    mounted: bool,
    /// The value as of the last layout or edit. A different value arriving
    /// from the app means it was changed from outside, so the caret moves
    /// to the end.
    seen: Option<String>,
}

impl FieldState {
    /// Whether the blinking caret is showing at `now`, and how long until
    /// it next changes.
    pub fn blink(&self, now: Instant) -> (bool, Duration) {
        let since = self.blink_origin.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32());
        let next = 0.53 - (since % 0.53);
        ((since % 1.06) < 0.53, Duration::from_secs_f32(next.max(0.02)))
    }

    /// Slides the text so a caret at `caret_x` stays inside `width`.
    /// Returns the scroll offset.
    pub fn keep_caret_visible(&mut self, caret_x: f32, width: f32) -> f32 {
        if caret_x - self.scroll > width {
            self.scroll = caret_x - width;
        } else if caret_x < self.scroll {
            self.scroll = caret_x;
        }
        self.scroll = self.scroll.max(0.0);
        self.scroll
    }
}

/// What a text field asks of its owner.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldAction {
    /// The text was edited; this is the new value.
    Edit(String),
    /// Enter was pressed.
    Submit,
    /// Escape was pressed. Focus has already been given up.
    Cancel,
    /// Up (-1) or Down (1), for a field that hands those keys on.
    Arrow(i32),
}

/// A single-line text field: caret and selection, word jumps, editing keys,
/// clipboard and input methods. The owner holds the text; edits come back
/// as [`FieldAction::Edit`].
#[derive(Clone, Copy, Debug)]
pub struct FieldLogic<'a> {
    pub value: &'a str,
    /// A password field: shows dots and refuses to copy.
    pub secure: bool,
    /// Report Up and Down as [`FieldAction::Arrow`] instead of moving the
    /// caret to the start or end.
    pub arrows: bool,
}

/// Byte offset of the `char_idx`th character.
pub fn byte_at(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map_or(s.len(), |(i, _)| i)
}

/// Index of the character at byte offset `byte_idx`.
pub fn char_at(s: &str, byte_idx: usize) -> usize {
    s.char_indices().take_while(|(i, _)| *i < byte_idx).count()
}

/// Char index of the start of the word before `i`.
pub fn word_left(s: &str, i: usize) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let mut j = i.min(chars.len());
    while j > 0 && chars[j - 1].is_whitespace() {
        j -= 1;
    }
    while j > 0 && !chars[j - 1].is_whitespace() {
        j -= 1;
    }
    j
}

/// Char index of the end of the word after `i`.
pub fn word_right(s: &str, i: usize) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let mut j = i.min(chars.len());
    while j < chars.len() && chars[j].is_whitespace() {
        j += 1;
    }
    while j < chars.len() && !chars[j].is_whitespace() {
        j += 1;
    }
    j
}

impl FieldLogic<'_> {
    /// The text to draw: the value, or dots for a secure field.
    pub fn shown(&self) -> String {
        if self.secure { "•".repeat(self.value.chars().count()) } else { self.value.to_owned() }
    }

    fn char_count(&self) -> usize {
        self.value.chars().count()
    }

    /// Call from layout. Keeps the caret inside the text, moves it to the
    /// end when the owner replaced the value, and with `autofocus` takes
    /// focus with everything selected the first time the field appears.
    pub fn sync(&self, cx: &mut Cx, autofocus: bool) {
        let n = self.char_count();
        let st = cx.state::<FieldState>();
        if st.seen.as_deref().is_some_and(|s| s != self.value) {
            st.cursor = n;
            st.anchor = n;
        }
        st.seen = Some(self.value.to_owned());
        st.cursor = st.cursor.min(n);
        st.anchor = st.anchor.min(n);
        if autofocus && !st.mounted {
            st.cursor = n;
            st.anchor = 0;
        }
        let first = !std::mem::replace(&mut st.mounted, true);
        if autofocus && first {
            cx.request_focus();
        }
    }

    /// Replaces the selection with `insert` and returns the new value.
    fn edit(&self, cx: &mut Cx, insert: &str) -> FieldAction {
        let st = cx.state::<FieldState>();
        let (lo, hi) = (st.cursor.min(st.anchor), st.cursor.max(st.anchor));
        let mut v = self.value.to_owned();
        let (blo, bhi) = (byte_at(&v, lo), byte_at(&v, hi));
        v.replace_range(blo..bhi, insert);
        let new_cursor = lo + insert.chars().count();
        st.cursor = new_cursor;
        st.anchor = new_cursor;
        st.blink_origin = Some(Instant::now());
        st.seen = Some(v.clone());
        FieldAction::Edit(v)
    }

    fn selected_text(&self, cursor: usize, anchor: usize) -> String {
        let (lo, hi) = (cursor.min(anchor), cursor.max(anchor));
        self.value.chars().skip(lo).take(hi - lo).collect()
    }

    /// Handles an event for a field occupying `bounds`. `hit` gives the
    /// char index nearest a pointer position, which depends on how the
    /// owner draws the text.
    pub fn event(&self, cx: &mut Cx, event: &Event, bounds: Rect, hit: impl Fn(Point) -> usize) -> (Status, Option<FieldAction>) {
        match event {
            Event::PointerMoved { pos } => {
                let inside = bounds.contains(*pos);
                let dragging = {
                    let st = cx.state::<FieldState>();
                    st.hovered = inside;
                    st.dragging
                };
                if inside {
                    cx.set_cursor(CursorIcon::Text);
                }
                if dragging {
                    cx.state::<FieldState>().cursor = hit(*pos);
                    cx.request_redraw();
                }
                (Status::Ignored, None)
            }
            Event::PointerPressed { pos, button: PointerButton::Primary } => {
                if bounds.contains(*pos) {
                    let c = hit(*pos);
                    cx.request_focus();
                    let st = cx.state::<FieldState>();
                    st.cursor = c;
                    st.anchor = c;
                    st.dragging = true;
                    st.blink_origin = Some(Instant::now());
                    (Status::Captured, None)
                } else {
                    cx.release_focus();
                    (Status::Ignored, None)
                }
            }
            Event::PointerReleased { .. } => {
                cx.state::<FieldState>().dragging = false;
                (Status::Ignored, None)
            }
            Event::Ime(t) if cx.is_focused() => (Status::Captured, Some(self.edit(cx, t))),
            Event::Key(k) if k.pressed && cx.is_focused() => {
                let n = self.char_count();
                let (cursor, anchor) = {
                    let st = cx.state::<FieldState>();
                    (st.cursor, st.anchor)
                };
                let shift = k.modifiers.shift;
                let cmd = k.modifiers.command();
                let word = if cfg!(target_os = "macos") { k.modifiers.alt } else { k.modifiers.ctrl };
                let move_to = |cx: &mut Cx, c: usize| {
                    let st = cx.state::<FieldState>();
                    st.cursor = c;
                    if !shift {
                        st.anchor = c;
                    }
                    st.blink_origin = Some(Instant::now());
                    cx.request_redraw();
                };
                let mut action = None;
                match &k.key {
                    Key::Up | Key::Down if self.arrows => action = Some(FieldAction::Arrow(if k.key == Key::Up { -1 } else { 1 })),
                    Key::Left => {
                        let c = if cmd { 0 } else if word { word_left(self.value, cursor) } else if cursor != anchor && !shift { cursor.min(anchor) } else { cursor.saturating_sub(1) };
                        move_to(cx, c);
                    }
                    Key::Right => {
                        let c = if cmd { n } else if word { word_right(self.value, cursor) } else if cursor != anchor && !shift { cursor.max(anchor) } else { (cursor + 1).min(n) };
                        move_to(cx, c);
                    }
                    Key::Home | Key::Up => move_to(cx, 0),
                    Key::End | Key::Down => move_to(cx, n),
                    Key::Backspace => {
                        if cursor == anchor {
                            if cursor == 0 {
                                return (Status::Captured, None);
                            }
                            let from = if word { word_left(self.value, cursor) } else { cursor - 1 };
                            cx.state::<FieldState>().anchor = from;
                        }
                        action = Some(self.edit(cx, ""));
                    }
                    Key::Delete => {
                        if cursor == anchor {
                            if cursor >= n {
                                return (Status::Captured, None);
                            }
                            let to = if word { word_right(self.value, cursor) } else { cursor + 1 };
                            cx.state::<FieldState>().anchor = to;
                        }
                        action = Some(self.edit(cx, ""));
                    }
                    Key::Enter => action = Some(FieldAction::Submit),
                    Key::Escape => {
                        cx.release_focus();
                        cx.request_redraw();
                        action = Some(FieldAction::Cancel);
                    }
                    Key::Tab => return (Status::Ignored, None),
                    Key::Character(c) if cmd => match c.as_str() {
                        "a" => {
                            let st = cx.state::<FieldState>();
                            st.anchor = 0;
                            st.cursor = n;
                            cx.request_redraw();
                        }
                        "c" | "x" if cursor != anchor && !self.secure => {
                            cx.copy(self.selected_text(cursor, anchor));
                            if c == "x" {
                                action = Some(self.edit(cx, ""));
                            }
                        }
                        "v" => {
                            if let Some(t) = cx.clipboard().map(|t| t.replace(['\n', '\r'], " ")) {
                                action = Some(self.edit(cx, &t));
                            }
                        }
                        _ => return (Status::Ignored, None),
                    },
                    _ => {
                        let text = k.text.as_deref().filter(|t| !t.chars().any(char::is_control) && !k.modifiers.ctrl && !k.modifiers.logo);
                        match text {
                            Some(t) => action = Some(self.edit(cx, t)),
                            None => return (Status::Ignored, None),
                        }
                    }
                }
                (Status::Captured, action)
            }
            _ => (Status::Ignored, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_motion() {
        let s = "hello brave  world";
        assert_eq!(word_left(s, s.chars().count()), 13);
        assert_eq!(word_left(s, 13), 6);
        assert_eq!(word_right(s, 0), 5);
        assert_eq!(word_right(s, 5), 11);
    }

    #[test]
    fn char_byte_mapping() {
        let s = "añb";
        assert_eq!(byte_at(s, 2), 3);
        assert_eq!(char_at(s, 3), 2);
        assert_eq!(byte_at(s, 9), s.len());
    }

    #[test]
    fn a_secure_field_shows_one_dot_per_character() {
        let f = FieldLogic { value: "pässword", secure: true, arrows: false };
        assert_eq!(f.shown(), "••••••••");
        assert_eq!(FieldLogic { secure: false, ..f }.shown(), "pässword");
    }

    #[test]
    fn the_text_slides_only_as_far_as_the_caret_needs() {
        let mut st = FieldState::default();
        assert_eq!(st.keep_caret_visible(50.0, 100.0), 0.0, "fits: no scroll");
        assert_eq!(st.keep_caret_visible(180.0, 100.0), 80.0, "caret at the right edge");
        assert_eq!(st.keep_caret_visible(150.0, 100.0), 80.0, "still visible: stays put");
        assert_eq!(st.keep_caret_visible(30.0, 100.0), 30.0, "caret at the left edge");
    }

    #[test]
    fn the_caret_blinks_about_once_a_second() {
        let start = Instant::now();
        let st = FieldState { blink_origin: Some(start), ..Default::default() };
        assert!(st.blink(start).0, "on straight after moving");
        assert!(!st.blink(start + Duration::from_millis(600)).0);
        assert!(st.blink(start + Duration::from_millis(1100)).0);
        assert!(st.blink(start + Duration::from_millis(100)).1 <= Duration::from_millis(430));
    }
}
