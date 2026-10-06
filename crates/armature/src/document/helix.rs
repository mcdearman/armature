//! Helix-style modal editing for [`Document`]: select first, then act.
//!
//! Modes: Normal, Insert and Select. Outside Insert there is always a
//! selection at least one character wide; motions move or extend it and
//! commands act on it. Covered here: counts, `h j k l`, the word motions,
//! `f t F T`, the `g` and `z` menus, `x X % ; Alt-;`, text objects and
//! surround under `m`, `d c y p P R r ~ > < J`, undo and redo, regex search,
//! named registers, the system clipboard under Space, and `:w` and `:q`.
//!
//! There can be several selections at once. Every motion and change then
//! applies to each of them, and `C`, `s`, `S`, `Alt-s`, `,`, `Alt-,`, `(`,
//! `)` and `_` make and manage them. Selections that come to overlap merge.
//!
//! While this is on, the document's cursor and anchor are the two end
//! characters of the selection, both included, rather than the gaps between
//! characters that plain editing uses.

use std::collections::HashMap;

use super::vim::{ch, compile, enclosing, find_char, first_non_blank, match_bracket, next, prev, text_object, ClipboardNeed, Scroll, VimRequest, VimView};
use super::{normalize, prev_boundary, text_between, Action, Document, EditKind, ExtraSelection, ModeStatus, Motion, Pos, Sel, INDENT};
use crate::event::{Key, KeyEvent};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Normal,
    Insert,
    Select,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HKey {
    Char(char),
    Alt(char),
    Ctrl(char),
    Esc,
    Enter,
    Backspace,
    Delete,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

impl HKey {
    fn from_event(k: &KeyEvent) -> Option<HKey> {
        let m = k.modifiers;
        let typed = || k.text.as_deref().and_then(|t| t.chars().next());
        Some(match &k.key {
            Key::Escape => HKey::Esc,
            Key::Enter => HKey::Enter,
            Key::Backspace => HKey::Backspace,
            Key::Delete => HKey::Delete,
            Key::Tab => HKey::Tab,
            Key::Left => HKey::Left,
            Key::Right => HKey::Right,
            Key::Up => HKey::Up,
            Key::Down => HKey::Down,
            Key::Home => HKey::Home,
            Key::End => HKey::End,
            Key::PageUp => HKey::PageUp,
            Key::PageDown => HKey::PageDown,
            Key::Space => HKey::Char(' '),
            Key::Character(c) if m.ctrl => {
                let c = c.chars().next()?.to_ascii_lowercase();
                if c == '[' { HKey::Esc } else { HKey::Ctrl(c) }
            }
            Key::Character(c) if m.alt => {
                // macOS types a special character for Option plus a key;
                // map the ones Helix binds back to the key that was pressed.
                let c = c.chars().next()?;
                HKey::Alt(match c {
                    '…' => ';',
                    '∂' => 'd',
                    'ç' => 'c',
                    other => other,
                })
            }
            Key::Character(c) => HKey::Char(typed().or_else(|| c.chars().next())?),
            Key::Other => HKey::Char(typed()?),
        })
    }

    fn describe(self) -> String {
        match self {
            HKey::Char(' ') => "<space>".into(),
            HKey::Char(c) => c.to_string(),
            HKey::Alt(c) => format!("<A-{c}>"),
            HKey::Ctrl(c) => format!("<C-{c}>"),
            _ => String::new(),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct Helix {
    mode: Mode,
    /// Keys of an unfinished command, such as `m` then `i`.
    pending: Vec<HKey>,
    count: Option<usize>,
    /// The register named with `"` for the next command.
    register: Option<char>,
    /// What was yanked: one piece of text per selection, in document order.
    registers: HashMap<char, Vec<String>>,
    /// Where the selection being worked on comes among them all, which
    /// picks the piece of a register it pastes.
    rank: usize,
    search: Option<String>,
    cmdline: Option<(char, String)>,
    message: Option<String>,
    requests: Vec<VimRequest>,
    clipboard_in: Option<String>,
    clipboard_out: Option<(u64, String)>,
    scroll: Option<(u64, Scroll)>,
    serial: u64,
    view_top: usize,
    view_lines: usize,
    /// The character column `j` and `k` aim for.
    goal: Option<usize>,
}

// ---------------------------------------------------------------------------
// Text helpers. A position with `col == line.len()` stands for the newline.

fn doc_end(lines: &[String]) -> Pos {
    Pos::new(lines.len() - 1, lines[lines.len() - 1].len())
}

fn char_col(line: &str, byte: usize) -> usize {
    line[..byte.min(line.len())].chars().count()
}

fn byte_at(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
}

/// Line break, blank, word or punctuation. A long word (`W`) counts
/// punctuation as part of the word.
fn category(c: char, long: bool) -> u8 {
    if c == '\n' {
        0
    } else if c.is_whitespace() {
        1
    } else if long || c.is_alphanumeric() || c == '_' {
        2
    } else {
        3
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    NextStart,
    NextEnd,
    PrevStart,
}

fn reached(target: Target, before: char, after: char, long: bool) -> bool {
    let boundary = category(before, long) != category(after, long);
    match target {
        Target::NextStart => boundary && (after == '\n' || !after.is_whitespace()),
        Target::NextEnd | Target::PrevStart => boundary && (!before.is_whitespace() || after == '\n'),
    }
}

/// `w` and `e`: the selection, both ends included, that the motion makes
/// from a cursor on `cursor`. `None` at the end of the text.
fn word_forward(lines: &[String], cursor: Pos, target: Target, long: bool) -> Option<(Pos, Pos)> {
    let end = doc_end(lines);
    let mut anchor = cursor;
    // `head` is the gap the selection has reached; it starts after the cursor.
    let mut head = next(lines, cursor)?;
    let first = head;
    let mut before = ch(lines, cursor);
    while head != end && ch(lines, head) == '\n' {
        before = '\n';
        head = next(lines, head)?;
    }
    if before == '\n' {
        anchor = head;
    }
    let start = head;
    while head != end {
        let after = ch(lines, head);
        if reached(target, before, after, long) {
            if head == start {
                anchor = head;
            } else {
                break;
            }
        }
        before = after;
        head = next(lines, head).unwrap_or(end);
    }
    // Nothing passed over means there was nowhere to go.
    (head > first && head > anchor).then(|| (anchor, prev(lines, head).unwrap_or(anchor)))
}

/// `b`: as [`word_forward`], going back. The head comes before the anchor.
fn word_back(lines: &[String], cursor: Pos, long: bool) -> Option<(Pos, Pos)> {
    let mut anchor = next(lines, cursor).unwrap_or(cursor);
    let mut head = cursor;
    let mut before = ch(lines, cursor);
    while let Some(p) = prev(lines, head) {
        if ch(lines, p) != '\n' {
            break;
        }
        before = '\n';
        head = p;
    }
    if before == '\n' {
        anchor = head;
    }
    let start = head;
    while let Some(p) = prev(lines, head) {
        let after = ch(lines, p);
        if reached(Target::PrevStart, before, after, long) {
            if head == start {
                anchor = head;
            } else {
                break;
            }
        }
        before = after;
        head = p;
    }
    (head < cursor && head < anchor).then(|| (prev(lines, anchor).unwrap_or(head), head))
}

/// The two halves of a surrounding pair named by either of its characters.
fn pair(c: char) -> (char, char) {
    match c {
        '(' | ')' => ('(', ')'),
        '[' | ']' => ('[', ']'),
        '{' | '}' => ('{', '}'),
        '<' | '>' => ('<', '>'),
        other => (other, other),
    }
}

/// Where the pair named by `c` sits around `p`: its two characters.
fn surround(lines: &[String], p: Pos, c: char) -> Option<(Pos, Pos)> {
    let (open, close) = pair(c);
    if open != close {
        return enclosing(lines, p, open, close);
    }
    let (a, b, _) = text_object(lines, p, true, open)?;
    Some((a, prev(lines, b)?))
}

/// The run of non-blank lines around `line`, and with `around` the blank
/// lines after it.
fn paragraph(lines: &[String], line: usize, around: bool) -> (usize, usize) {
    let blank = |l: usize| lines[l].trim().is_empty();
    let kind = blank(line);
    let (mut a, mut b) = (line, line);
    while a > 0 && blank(a - 1) == kind {
        a -= 1;
    }
    while b + 1 < lines.len() && blank(b + 1) == kind {
        b += 1;
    }
    if around && !kind {
        while b + 1 < lines.len() && blank(b + 1) {
            b += 1;
        }
    }
    (a, b)
}


impl Helix {
    pub(crate) fn status(&self, doc: &Document) -> ModeStatus {
        let mut pending: String = self.count.map(|c| c.to_string()).unwrap_or_default();
        if let Some(r) = self.register {
            pending.push('"');
            pending.push(r);
        }
        pending.extend(self.pending.iter().map(|k| k.describe()));
        ModeStatus {
            label: match self.mode {
                Mode::Normal => "NOR",
                Mode::Insert => "INS",
                Mode::Select => "SEL",
            },
            insert: self.mode == Mode::Insert,
            pending,
            command_line: self.cmdline.as_ref().map(|(p, s)| match p {
                's' => format!("select:{s}"),
                'S' => format!("split:{s}"),
                _ => format!("{p}{s}"),
            }),
            message: self.message.clone(),
            recording: None,
            selections: doc.others.len() + 1,
        }
    }

    pub(crate) fn view(&self) -> VimView {
        // Space then `p`, `P` or `R` reads the system clipboard.
        let waiting_on_space = self.pending == [HKey::Char(' ')] && self.cmdline.is_none();
        VimView {
            block: None,
            clipboard: self.clipboard_out.clone(),
            clipboard_need: if waiting_on_space { ClipboardNeed::Always } else { ClipboardNeed::None },
            scroll: self.scroll,
            viewport: (self.view_top, self.view_lines),
        }
    }

    pub(crate) fn take_requests(&mut self) -> Vec<VimRequest> {
        std::mem::take(&mut self.requests)
    }

    pub(crate) fn block_caret(&self) -> bool {
        self.mode != Mode::Insert
    }

    pub(crate) fn display_selection(&self, doc: &Document) -> Option<(Pos, Pos, bool)> {
        if self.mode == Mode::Insert {
            return doc.selection().map(|(a, b)| (a, b, false));
        }
        (doc.cursor != doc.anchor).then(|| {
            let (lo, end) = span(doc);
            (lo, end, false)
        })
    }

    pub(crate) fn extras(&self, doc: &Document) -> Vec<ExtraSelection> {
        doc.others
            .iter()
            .map(|o| {
                let (lo, hi) = (o.cursor.min(o.anchor), o.cursor.max(o.anchor));
                let range = if lo == hi {
                    None
                } else if self.mode == Mode::Insert {
                    Some((lo, hi))
                } else {
                    Some((lo, next(&doc.lines, hi).unwrap_or(hi)))
                };
                ExtraSelection { cursor: o.cursor, range }
            })
            .collect()
    }

    /// Called when Helix mode is switched on.
    pub(crate) fn start(doc: &mut Document) {
        doc.cursor = doc.clamp(doc.cursor);
        doc.anchor = doc.cursor;
    }

    pub(crate) fn intercept(&mut self, doc: &mut Document, action: &Action) -> bool {
        match action {
            Action::Key(k) => {
                if let Some(key) = HKey::from_event(k) {
                    self.message = None;
                    self.key(doc, key);
                }
                true
            }
            Action::Clipboard(t) => {
                self.clipboard_in = Some(normalize(t));
                true
            }
            Action::Viewport { top, lines } => {
                self.view_top = *top;
                self.view_lines = *lines;
                true
            }
            Action::Click { pos, select } => {
                self.reset();
                doc.others.clear();
                if self.mode == Mode::Insert {
                    return false;
                }
                let p = doc.clamp(*pos);
                doc.cursor = p;
                if !*select {
                    doc.anchor = p;
                }
                true
            }
            Action::Drag(pos) => {
                if self.mode == Mode::Insert {
                    return false;
                }
                doc.cursor = doc.clamp(*pos);
                self.finish(doc);
                true
            }
            Action::SelectWord(pos) => {
                doc.others.clear();
                doc.apply_plain(Action::SelectWord(*pos));
                if self.mode != Mode::Insert {
                    from_gaps(doc);
                }
                true
            }
            Action::SelectAll => {
                if self.mode == Mode::Insert {
                    doc.others.clear();
                    return false;
                }
                self.select_all(doc);
                true
            }
            Action::Collapse => {
                self.each(doc, |_, doc| doc.anchor = doc.cursor);
                true
            }
            Action::Move { .. } => {
                doc.others.clear();
                if self.mode == Mode::Insert {
                    return false;
                }
                doc.anchor = doc.cursor;
                doc.apply_plain(action.clone());
                doc.cursor = doc.clamp(doc.cursor);
                doc.anchor = doc.cursor;
                true
            }
            // Edits from menus and shortcuts act on every selection as shown.
            Action::Insert(_) | Action::Enter | Action::Backspace | Action::Delete => {
                let normal = self.mode != Mode::Insert;
                self.each(doc, |_, doc| {
                    if normal {
                        to_gaps(doc);
                    }
                    plain(doc, action.clone());
                    if normal {
                        doc.cursor = doc.clamp(doc.cursor);
                        doc.anchor = doc.cursor;
                    }
                });
                self.finish(doc);
                true
            }
            Action::Indent | Action::Outdent => {
                if self.mode == Mode::Insert {
                    doc.others.clear();
                    return false;
                }
                self.indent(doc, 1, matches!(action, Action::Indent));
                self.finish(doc);
                true
            }
            Action::Undo | Action::Redo => {
                doc.grouped = false;
                doc.apply_plain(action.clone());
                self.finish(doc);
                true
            }
        }
    }

    fn reset(&mut self) {
        self.pending.clear();
        self.count = None;
        self.register = None;
        self.goal = None;
    }

    fn say(&mut self, m: impl Into<String>) {
        self.message = Some(m.into());
    }

    /// Runs `f` for the main selection and then for each of the others,
    /// with that selection in the document's cursor and anchor.
    fn each(&mut self, doc: &mut Document, mut f: impl FnMut(&mut Self, &mut Document)) {
        let ranks = order(doc);
        self.rank = ranks[0];
        f(self, doc);
        for k in 0..doc.others.len() {
            self.swap(doc, k);
            self.rank = ranks[k + 1];
            f(self, doc);
            self.swap(doc, k);
        }
        self.rank = ranks[0];
    }

    /// Exchanges the main selection with another, so commands written for
    /// one selection can work on any of them.
    fn swap(&mut self, doc: &mut Document, k: usize) {
        let other = doc.others[k];
        doc.others[k] = Sel { anchor: doc.anchor, cursor: doc.cursor, goal: self.goal };
        doc.anchor = other.anchor;
        doc.cursor = other.cursor;
        self.goal = other.goal;
    }

    /// Tidies up after a command: closes its undo step, keeps every
    /// selection on real positions, and merges any that now overlap.
    fn finish(&mut self, doc: &mut Document) {
        if self.mode != Mode::Insert {
            doc.grouped = false;
        }
        settle(doc);
        if doc.others.is_empty() {
            return;
        }
        let mut all: Vec<(Sel, bool)> = vec![(Sel { anchor: doc.anchor, cursor: doc.cursor, goal: self.goal }, true)];
        all.extend(doc.others.drain(..).map(|o| (o, false)));
        all.sort_by_key(|(s, _)| s.anchor.min(s.cursor));
        let mut merged: Vec<(Sel, bool)> = Vec::with_capacity(all.len());
        for (sel, main) in all {
            let (lo, hi) = (sel.anchor.min(sel.cursor), sel.anchor.max(sel.cursor));
            if let Some((last, last_main)) = merged.last_mut() {
                let (start, end) = (last.anchor.min(last.cursor), last.anchor.max(last.cursor));
                if lo <= end {
                    let end = end.max(hi);
                    *last = if last.cursor >= last.anchor { Sel { anchor: start, cursor: end, ..*last } } else { Sel { anchor: end, cursor: start, ..*last } };
                    *last_main |= main;
                    continue;
                }
            }
            merged.push((sel, main));
        }
        for (sel, main) in merged {
            if main {
                doc.anchor = sel.anchor;
                doc.cursor = sel.cursor;
                self.goal = sel.goal;
            } else {
                doc.others.push(sel);
            }
        }
    }

    /// Whether a key finishes a command that every selection carries out,
    /// as opposed to one that happens once: a prefix, a count, a prompt, or
    /// a command about the selections themselves.
    fn for_each_selection(&self, key: HKey) -> bool {
        if self.mode == Mode::Insert {
            return true;
        }
        let HKey::Char(c) = key else {
            return self.pending.is_empty() && !matches!(key, HKey::Esc | HKey::Alt('s' | 'C' | ','));
        };
        match self.pending.as_slice() {
            [] => {
                let digit = c.is_ascii_digit() && (c != '0' || self.count.is_some());
                !digit && !matches!(c, 'g' | 'm' | 'z' | ' ' | '"' | 'f' | 't' | 'F' | 'T' | 'r' | 'v' | ':' | '/' | '?' | 'u' | 'U' | 'n' | 'N' | '*' | '%' | '>' | '<' | 'y' | 'C' | 's' | 'S' | ',' | '(' | ')' | '&')
            }
            [HKey::Char('g' | 'f' | 't' | 'F' | 'T' | 'r'), ..] => true,
            [HKey::Char(' ')] => matches!(c, 'p' | 'P' | 'R'),
            [HKey::Char('m')] => c == 'm',
            [HKey::Char('m'), HKey::Char('r')] => false,
            [HKey::Char('m'), ..] => true,
            _ => false,
        }
    }

    /// Yanking takes every selection at once, so it happens before the
    /// command runs for each of them.
    fn yank_first(&mut self, doc: &Document, key: HKey) {
        if self.mode == Mode::Insert {
            return;
        }
        let to_clipboard = match (self.pending.as_slice(), key) {
            ([], HKey::Char('y' | 'd' | 'c')) => false,
            ([HKey::Char(' ')], HKey::Char('y')) => true,
            _ => return,
        };
        let mut spans: Vec<(Pos, Pos)> = std::iter::once((doc.anchor, doc.cursor)).chain(doc.others.iter().map(|o| (o.anchor, o.cursor))).map(|(a, c)| (a.min(c), a.max(c))).collect();
        spans.sort();
        let texts: Vec<String> = spans.into_iter().map(|(lo, hi)| text_between(&doc.lines, lo, next(&doc.lines, hi).unwrap_or(hi))).collect();
        if to_clipboard {
            self.serial += 1;
            self.clipboard_out = Some((self.serial, texts.join("\n")));
        } else {
            self.registers.insert(self.register.unwrap_or('"'), texts);
        }
    }

    fn key(&mut self, doc: &mut Document, key: HKey) {
        if self.cmdline.is_some() {
            self.command_key(doc, key);
            return self.finish(doc);
        }
        self.yank_first(doc, key);
        if self.for_each_selection(key) && !doc.others.is_empty() {
            // Each selection starts from the same pending keys and count.
            let before = (self.mode, self.pending.clone(), self.count, self.register);
            let mut after = before.clone();
            let mut first = true;
            self.each(doc, |this, doc| {
                (this.mode, this.pending, this.count, this.register) = before.clone();
                this.key_one(doc, key);
                if std::mem::take(&mut first) {
                    after = (this.mode, this.pending.clone(), this.count, this.register);
                }
            });
            (self.mode, self.pending, self.count, self.register) = after;
        } else {
            self.rank = order(doc)[0];
            self.key_one(doc, key);
        }
        self.finish(doc);
    }

    fn key_one(&mut self, doc: &mut Document, key: HKey) {
        if self.mode == Mode::Insert {
            return self.insert_key(doc, key);
        }
        if !self.pending.is_empty() {
            return self.pending_key(doc, key);
        }
        // Counts: digits, but a leading zero is not one.
        if let HKey::Char(c) = key
            && let Some(d) = c.to_digit(10)
            && (d != 0 || self.count.is_some())
        {
            self.count = Some(self.count.unwrap_or(0).saturating_mul(10).saturating_add(d as usize).min(100_000));
            return;
        }
        let n = self.count.unwrap_or(1);
        let extend = self.mode == Mode::Select;
        let keep_goal = matches!(key, HKey::Char('j' | 'k') | HKey::Up | HKey::Down | HKey::PageUp | HKey::PageDown | HKey::Ctrl('d' | 'u' | 'f' | 'b'));
        if !keep_goal {
            self.goal = None;
        }
        match key {
            HKey::Esc => {
                self.mode = Mode::Normal;
                self.reset();
                return;
            }
            // Prefixes wait for more keys and keep the count.
            HKey::Char('g' | 'm' | 'z' | ' ' | '"' | 'f' | 't' | 'F' | 'T' | 'r') => {
                self.pending.push(key);
                return;
            }

            HKey::Char('h') | HKey::Left => self.step(doc, n, false, extend),
            HKey::Char('l') | HKey::Right => self.step(doc, n, true, extend),
            HKey::Char('j') | HKey::Down => self.vertical(doc, n as isize, extend),
            HKey::Char('k') | HKey::Up => self.vertical(doc, -(n as isize), extend),
            HKey::Char('w') => self.word(doc, n, Some(Target::NextStart), false, extend),
            HKey::Char('W') => self.word(doc, n, Some(Target::NextStart), true, extend),
            HKey::Char('e') => self.word(doc, n, Some(Target::NextEnd), false, extend),
            HKey::Char('E') => self.word(doc, n, Some(Target::NextEnd), true, extend),
            HKey::Char('b') => self.word(doc, n, None, false, extend),
            HKey::Char('B') => self.word(doc, n, None, true, extend),
            HKey::Home => self.goto(doc, Pos::new(doc.cursor.line, 0), extend),
            HKey::End => self.goto(doc, line_last(doc, doc.cursor.line), extend),
            HKey::Char('G') => {
                let line = self.count.map_or(doc.lines.len() - 1, |c| c.saturating_sub(1).min(doc.lines.len() - 1));
                self.goto(doc, Pos::new(line, 0), extend);
            }
            HKey::PageDown | HKey::Ctrl('f') => self.vertical(doc, (self.page() * n) as isize, extend),
            HKey::PageUp | HKey::Ctrl('b') => self.vertical(doc, -((self.page() * n) as isize), extend),
            HKey::Ctrl('d') => self.vertical(doc, ((self.page() / 2).max(1) * n) as isize, extend),
            HKey::Ctrl('u') => self.vertical(doc, -(((self.page() / 2).max(1) * n) as isize), extend),

            HKey::Char('v') => {
                self.mode = if extend { Mode::Normal } else { Mode::Select };
            }
            HKey::Char(';') => doc.anchor = doc.cursor,
            HKey::Alt(';') => std::mem::swap(&mut doc.anchor, &mut doc.cursor),
            HKey::Char('%') => self.select_all(doc),
            HKey::Char('x') => self.select_lines(doc, n, true),
            HKey::Char('X') => self.select_lines(doc, 1, false),

            HKey::Char('i') => {
                let (lo, _) = sel(doc);
                self.insert_at(doc, lo);
            }
            HKey::Char('a') => {
                let (_, end) = span(doc);
                self.insert_at(doc, end);
            }
            HKey::Char('I') => {
                let line = sel(doc).0.line;
                self.insert_at(doc, Pos::new(line, first_non_blank(&doc.lines[line])));
            }
            HKey::Char('A') => {
                let line = sel(doc).1.line;
                self.insert_at(doc, Pos::new(line, doc.lines[line].len()));
            }
            HKey::Char('o') => self.open_line(doc, true),
            HKey::Char('O') => self.open_line(doc, false),

            // `d`, `c` and `y` have already yanked; the Alt forms do not.
            HKey::Char('d') | HKey::Alt('d') => self.delete(doc),
            HKey::Char('c') | HKey::Alt('c') => self.change(doc),
            HKey::Char('y') => self.say(if doc.others.is_empty() { "Yanked the selection".to_owned() } else { format!("Yanked {} selections", doc.others.len() + 1) }),
            HKey::Char('_') => trim(doc),
            HKey::Char('p') => {
                let text = self.register_text();
                self.paste(doc, text, true, n);
            }
            HKey::Char('P') => {
                let text = self.register_text();
                self.paste(doc, text, false, n);
            }
            HKey::Char('R') => {
                if let Some(text) = self.register_text() {
                    edit(doc);
                    replace(doc, &text);
                }
            }
            HKey::Char('~') => self.map_chars(doc, |c| if c.is_uppercase() { c.to_lowercase().collect() } else { c.to_uppercase().collect() }),
            HKey::Char('`') => self.map_chars(doc, |c| c.to_lowercase().collect()),
            HKey::Alt('`') => self.map_chars(doc, |c| c.to_uppercase().collect()),
            HKey::Char('>') => self.indent(doc, n, true),
            HKey::Char('<') => self.indent(doc, n, false),
            HKey::Char('J') => self.join(doc),
            HKey::Char('u') => {
                doc.grouped = false;
                for _ in 0..n {
                    doc.step(true);
                }
            }
            HKey::Char('U') => {
                doc.grouped = false;
                for _ in 0..n {
                    doc.step(false);
                }
            }

            HKey::Char(c @ ('/' | '?' | ':')) => self.cmdline = Some((c, String::new())),
            HKey::Char('n') => {
                for _ in 0..n {
                    self.find_next(doc, true);
                }
            }
            HKey::Char('N') => {
                for _ in 0..n {
                    self.find_next(doc, false);
                }
            }
            HKey::Char('*') => {
                let text = selected(doc);
                let pattern = regex::escape(text.trim_end_matches('\n'));
                self.say(format!("Search set to '{pattern}'"));
                self.search = Some(pattern);
            }

            // Making and managing selections.
            HKey::Char('C') => self.copy_to_lines(doc, n, true),
            HKey::Alt('C') => self.copy_to_lines(doc, n, false),
            HKey::Char(c @ ('s' | 'S')) => self.cmdline = Some((c, String::new())),
            HKey::Alt('s') => self.select_matches(doc, r"\n", true),
            HKey::Char(',') => doc.others.clear(),
            HKey::Alt(',') => self.drop_main(doc),
            HKey::Char(')') => self.rotate(doc, n as isize),
            HKey::Char('(') => self.rotate(doc, -(n as isize)),
            HKey::Char('&') => self.say("Aligning selections is not supported"),
            _ => {}
        }
        self.count = None;
        self.register = None;
    }

    fn page(&self) -> usize {
        self.view_lines.max(2) - 1
    }

    // --- Moving and selecting ---------------------------------------------

    /// Puts the cursor on `p`. Without `extend` the selection collapses there.
    fn goto(&mut self, doc: &mut Document, p: Pos, extend: bool) {
        doc.cursor = doc.clamp(p);
        if !extend {
            doc.anchor = doc.cursor;
        }
    }

    fn step(&mut self, doc: &mut Document, n: usize, forward: bool, extend: bool) {
        let mut p = doc.cursor;
        for _ in 0..n {
            match if forward { next(&doc.lines, p) } else { prev(&doc.lines, p) } {
                Some(q) => p = q,
                None => break,
            }
        }
        self.goto(doc, p, extend);
    }

    fn vertical(&mut self, doc: &mut Document, by: isize, extend: bool) {
        let col = *self.goal.get_or_insert_with(|| char_col(&doc.lines[doc.cursor.line], doc.cursor.col));
        let line = (doc.cursor.line as isize + by).clamp(0, doc.lines.len() as isize - 1) as usize;
        let text = &doc.lines[line];
        // A shorter line takes the cursor to its last character.
        let at = byte_at(text, col).min(line_last(doc, line).col);
        self.goto(doc, Pos::new(line, at), extend);
    }

    fn word(&mut self, doc: &mut Document, n: usize, forward: Option<Target>, long: bool, extend: bool) {
        for _ in 0..n {
            let found = match forward {
                Some(target) => word_forward(&doc.lines, doc.cursor, target, long),
                None => word_back(&doc.lines, doc.cursor, long),
            };
            let Some((anchor, head)) = found else { break };
            if !extend {
                doc.anchor = anchor;
            }
            doc.cursor = head;
        }
    }

    fn select_all(&mut self, doc: &mut Document) {
        doc.others.clear();
        doc.anchor = Pos::new(0, 0);
        doc.cursor = doc_end(&doc.lines);
    }

    /// `x` selects the lines the selection touches; pressed again, with
    /// `grow`, it takes in the next line.
    fn select_lines(&mut self, doc: &mut Document, n: usize, grow: bool) {
        let (lo, hi) = sel(doc);
        let whole = lo.col == 0 && hi.col == doc.lines[hi.line].len();
        let extra = if whole && grow { n } else { n - 1 };
        let last = (hi.line + if grow { extra } else { 0 }).min(doc.lines.len() - 1);
        doc.anchor = Pos::new(lo.line, 0);
        doc.cursor = Pos::new(last, doc.lines[last].len());
    }

    fn pending_key(&mut self, doc: &mut Document, key: HKey) {
        let extend = self.mode == Mode::Select;
        let n = self.count.unwrap_or(1);
        let first = self.pending[0];
        if key == HKey::Esc {
            self.reset();
            return;
        }
        let HKey::Char(c) = key else {
            self.reset();
            return;
        };
        match (first, self.pending.len()) {
            (HKey::Char('"'), _) => {
                self.pending.clear();
                self.register = Some(c);
                return;
            }
            (HKey::Char(f @ ('f' | 't' | 'F' | 'T')), _) => {
                let line = &doc.lines[doc.cursor.line];
                match find_char(line, doc.cursor.col, f, c, n, false) {
                    Some(col) => {
                        // The selection runs from where the cursor was.
                        if !extend {
                            doc.anchor = doc.cursor;
                        }
                        doc.cursor = Pos::new(doc.cursor.line, col);
                    }
                    None => self.say(format!("'{c}' is not on this line")),
                }
            }
            (HKey::Char('r'), _) => self.map_chars(doc, |_| c.to_string()),
            (HKey::Char('g'), _) => match c {
                'g' => {
                    let line = self.count.map_or(0, |n| n.saturating_sub(1).min(doc.lines.len() - 1));
                    self.goto(doc, Pos::new(line, 0), extend);
                }
                'e' => self.goto(doc, Pos::new(doc.lines.len() - 1, 0), extend),
                'h' => self.goto(doc, Pos::new(doc.cursor.line, 0), extend),
                'l' => self.goto(doc, line_last(doc, doc.cursor.line), extend),
                's' => self.goto(doc, Pos::new(doc.cursor.line, first_non_blank(&doc.lines[doc.cursor.line])), extend),
                't' => self.goto(doc, Pos::new(self.view_top, 0), extend),
                'c' => self.goto(doc, Pos::new(self.view_top + self.view_lines / 2, 0), extend),
                'b' => self.goto(doc, Pos::new(self.view_top + self.view_lines.saturating_sub(1), 0), extend),
                _ => {}
            },
            (HKey::Char('z'), _) => {
                let scroll = match c {
                    'z' | 'c' => Some(Scroll::Center),
                    't' => Some(Scroll::Top),
                    'b' => Some(Scroll::Bottom),
                    'j' => Some(Scroll::Lines(n as isize)),
                    'k' => Some(Scroll::Lines(-(n as isize))),
                    _ => None,
                };
                if let Some(s) = scroll {
                    self.serial += 1;
                    self.scroll = Some((self.serial, s));
                }
            }
            (HKey::Char(' '), _) => match c {
                // The yank itself happened before this, for every selection.
                'y' => self.say("Yanked the selection to the system clipboard"),
                'p' | 'P' => {
                    let text = self.clipboard_in.clone();
                    self.paste(doc, text, c == 'p', n);
                }
                'R' => {
                    if let Some(text) = self.clipboard_in.clone() {
                        edit(doc);
                        replace(doc, &text);
                    }
                }
                _ => {}
            },
            (HKey::Char('m'), 1) => match c {
                'm' => match match_bracket(&doc.lines, doc.cursor) {
                    Some(p) => self.goto(doc, p, extend),
                    None => self.say("No bracket under the cursor"),
                },
                'i' | 'a' | 's' | 'd' | 'r' => {
                    self.pending.push(key);
                    return;
                }
                _ => {}
            },
            (HKey::Char('m'), 2) => match self.pending[1] {
                HKey::Char(kind @ ('i' | 'a')) => self.select_object(doc, kind == 'a', c),
                HKey::Char('s') => {
                    let (open, close) = pair(c);
                    let text = selected(doc);
                    edit(doc);
                    let (start, end) = replace(doc, &format!("{open}{text}{close}"));
                    select_gaps(doc, start, end);
                }
                HKey::Char('d') => match surround(&doc.lines, doc.cursor, c) {
                    Some((a, b)) => {
                        edit(doc);
                        // Later first, so the earlier position stays valid.
                        remove_char(doc, b);
                        remove_char(doc, a);
                    }
                    None => self.say(format!("No surrounding '{c}'")),
                },
                HKey::Char('r') => {
                    self.pending.push(key);
                    return;
                }
                _ => {}
            },
            (HKey::Char('m'), _) => {
                // `mr`, the pair to replace, then the new one.
                let HKey::Char(from) = self.pending[2] else { return self.reset() };
                match surround(&doc.lines, doc.cursor, from) {
                    Some((a, b)) => {
                        let (open, close) = pair(c);
                        edit(doc);
                        set_char(doc, b, close);
                        set_char(doc, a, open);
                    }
                    None => self.say(format!("No surrounding '{from}'")),
                }
            }
            _ => {}
        }
        self.reset();
    }

    fn select_object(&mut self, doc: &mut Document, around: bool, obj: char) {
        if obj == 'p' {
            let (a, b) = paragraph(&doc.lines, doc.cursor.line, around);
            doc.anchor = Pos::new(a, 0);
            doc.cursor = Pos::new(b, doc.lines[b].len());
            return;
        }
        match text_object(&doc.lines, doc.cursor, around, obj) {
            Some((a, b, _)) if b > a => select_gaps(doc, a, b),
            Some(_) => {}
            None => self.say(format!("No '{obj}' around the cursor")),
        }
    }

    // --- Changing ----------------------------------------------------------

    /// The piece of the register that this selection pastes: its own if
    /// there is one for each, otherwise the last.
    fn register_text(&mut self) -> Option<String> {
        let r = self.register.unwrap_or('"');
        let text = self.registers.get(&r).and_then(|v| v.get(self.rank).or(v.last())).cloned();
        if text.is_none() {
            self.say(format!("Register '{r}' is empty"));
        }
        text
    }

    fn delete(&mut self, doc: &mut Document) {
        edit(doc);
        remove(doc);
        self.mode = Mode::Normal;
    }

    fn change(&mut self, doc: &mut Document) {
        let (lo, hi) = sel(doc);
        let whole = lo.col == 0 && hi.col == doc.lines[hi.line].len();
        edit(doc);
        if whole {
            // Changing whole lines leaves one empty line, indented like the first.
            let from = Pos::new(lo.line, first_non_blank(&doc.lines[lo.line]));
            tracked(doc, Some(from), hi, false, |doc| {
                doc.anchor = from;
                doc.cursor = hi;
                doc.delete_selection();
            });
        } else {
            remove(doc);
        }
        doc.anchor = doc.cursor;
        self.mode = Mode::Insert;
    }

    fn insert_at(&mut self, doc: &mut Document, gap: Pos) {
        doc.cursor = doc.clamp(gap);
        doc.anchor = doc.cursor;
        self.mode = Mode::Insert;
    }

    fn open_line(&mut self, doc: &mut Document, below: bool) {
        let (lo, hi) = sel(doc);
        let line = if below { hi.line } else { lo.line };
        let indent = " ".repeat(first_non_blank(&doc.lines[line]));
        edit(doc);
        let at = if below { Pos::new(line, doc.lines[line].len()) } else { Pos::new(line, 0) };
        let text = if below { format!("\n{indent}") } else { format!("{indent}\n") };
        tracked(doc, Some(at), at, false, |doc| {
            doc.cursor = at;
            doc.anchor = at;
            doc.insert(&text);
        });
        if !below {
            doc.cursor = Pos::new(line, indent.len());
            doc.anchor = doc.cursor;
        }
        self.mode = Mode::Insert;
    }

    /// Pastes after or before the selection and selects what was pasted.
    /// Text ending in a line break goes on its own lines.
    fn paste(&mut self, doc: &mut Document, text: Option<String>, after: bool, n: usize) {
        let Some(text) = text.filter(|t| !t.is_empty()) else { return };
        let (lo, hi) = sel(doc);
        let (_, end) = span(doc);
        let lines = text.ends_with('\n');
        let last_line = hi.line + 1 == doc.lines.len();
        edit(doc);
        let (at, body, skip) = if lines && after && last_line && end.line == hi.line {
            // No line below to start on: add the break in front instead.
            (doc_end(&doc.lines), format!("\n{}", text.trim_end_matches('\n')).repeat(n), true)
        } else if lines && after {
            (Pos::new(end.line.max(hi.line + usize::from(end.col != 0)), 0), text.repeat(n), false)
        } else if lines {
            (Pos::new(lo.line, 0), text.repeat(n), false)
        } else {
            (if after { end } else { lo }, text.repeat(n), false)
        };
        tracked(doc, Some(at), at, false, |doc| {
            doc.cursor = at;
            doc.anchor = at;
            doc.insert(&body);
        });
        let start = if skip { next(&doc.lines, at).unwrap_or(at) } else { at };
        let end = doc.cursor;
        select_gaps(doc, start, end);
        self.mode = Mode::Normal;
    }

    /// Replaces every character of the selection, line breaks aside.
    fn map_chars(&mut self, doc: &mut Document, f: impl Fn(char) -> String) {
        let text: String = selected(doc).chars().map(|c| if c == '\n' { "\n".to_owned() } else { f(c) }).collect();
        let backward = doc.cursor < doc.anchor;
        edit(doc);
        let (start, end) = replace(doc, &text);
        select_gaps(doc, start, end);
        if backward {
            std::mem::swap(&mut doc.anchor, &mut doc.cursor);
        }
    }

    /// Indents or outdents every line that any selection touches, once.
    fn indent(&mut self, doc: &mut Document, n: usize, deeper: bool) {
        let mut lines: Vec<usize> = std::iter::once((doc.anchor, doc.cursor)).chain(doc.others.iter().map(|o| (o.anchor, o.cursor))).flat_map(|(a, c)| a.line.min(c.line)..=a.line.max(c.line)).collect();
        lines.sort_unstable();
        lines.dedup();
        edit(doc);
        for line in lines {
            let start = Pos::new(line, 0);
            if deeper {
                if !doc.lines[line].is_empty() {
                    tracked(doc, Some(start), start, true, |doc| doc.lines[line].insert_str(0, &" ".repeat(INDENT * n)));
                }
            } else {
                let take = first_non_blank(&doc.lines[line]).min(INDENT * n);
                tracked(doc, Some(start), Pos::new(line, take), true, |doc| doc.lines[line].replace_range(..take, ""));
            }
        }
    }

    /// Joins the selected lines, or the cursor's line with the next.
    fn join(&mut self, doc: &mut Document) {
        let (lo, hi) = sel(doc);
        let joins = (hi.line - lo.line).max(1);
        if lo.line + 1 >= doc.lines.len() {
            return;
        }
        edit(doc);
        for _ in 0..joins {
            if lo.line + 1 >= doc.lines.len() {
                break;
            }
            let from = Pos::new(lo.line, doc.lines[lo.line].trim_end().len());
            let to = Pos::new(lo.line + 1, first_non_blank(&doc.lines[lo.line + 1]));
            tracked(doc, Some(from), to, true, |doc| {
                let below = doc.lines.remove(lo.line + 1);
                let line = &mut doc.lines[lo.line];
                line.truncate(from.col);
                let below = &below[to.col..];
                if !line.is_empty() && !below.is_empty() {
                    line.push(' ');
                }
                line.push_str(below);
            });
        }
    }

    // --- Several selections -------------------------------------------------

    /// `C`: gives each selection a copy on the next line long enough to
    /// hold it, `n` times over. The main selection moves to its last copy.
    fn copy_to_lines(&mut self, doc: &mut Document, n: usize, down: bool) {
        let all: Vec<Sel> = std::iter::once(Sel { anchor: doc.anchor, cursor: doc.cursor, goal: self.goal }).chain(doc.others.iter().copied()).collect();
        let mut main_copy = None;
        for (i, s) in all.iter().enumerate() {
            if s.anchor.line != s.cursor.line {
                continue;
            }
            let text = &doc.lines[s.cursor.line];
            let (anchor, cursor) = (char_col(text, s.anchor.col), char_col(text, s.cursor.col));
            let reach = anchor.max(cursor);
            let mut line = s.cursor.line;
            let mut made = 0;
            while made < n {
                if (down && line + 1 >= doc.lines.len()) || (!down && line == 0) {
                    break;
                }
                line = if down { line + 1 } else { line - 1 };
                let text = &doc.lines[line];
                if text.chars().count() > reach || reach == 0 {
                    let copy = Sel { anchor: Pos::new(line, byte_at(text, anchor)), cursor: Pos::new(line, byte_at(text, cursor)), goal: None };
                    doc.others.push(copy);
                    if i == 0 {
                        main_copy = Some(doc.others.len() - 1);
                    }
                    made += 1;
                }
            }
        }
        if let Some(k) = main_copy {
            self.swap(doc, k);
        }
    }

    /// `s` and `S`: within the selections, selects every match of a
    /// pattern, or with `split` the text between the matches.
    fn select_matches(&mut self, doc: &mut Document, pattern: &str, split: bool) {
        let re = match compile(pattern, true, true) {
            Ok(re) => re,
            Err(e) => return self.say(e),
        };
        let mut spans: Vec<(Pos, Pos)> = std::iter::once((doc.anchor, doc.cursor)).chain(doc.others.iter().map(|o| (o.anchor, o.cursor))).map(|(a, c)| (a.min(c), a.max(c))).collect();
        spans.sort();
        let mut found: Vec<(Pos, Pos)> = vec![];
        for (lo, hi) in spans {
            let text = text_between(&doc.lines, lo, next(&doc.lines, hi).unwrap_or(hi));
            // Where a byte of `text` is in the document.
            let at = |byte: usize| {
                let before = &text[..byte];
                match before.rfind('\n') {
                    Some(i) => Pos::new(lo.line + before.matches('\n').count(), byte - i - 1),
                    None => Pos::new(lo.line, lo.col + byte),
                }
            };
            let mut from = 0;
            for m in re.find_iter(&text).filter(|m| !m.is_empty()) {
                if !split {
                    found.push((at(m.start()), at(m.end())));
                } else if m.start() > from {
                    found.push((at(from), at(m.start())));
                }
                from = m.end();
            }
            if split && from < text.len() {
                found.push((at(from), at(text.len())));
            }
        }
        let Some(((start, end), rest)) = found.split_first() else {
            return self.say(if split { "Nothing left after splitting" } else { "No matches in the selection" });
        };
        select_gaps(doc, *start, *end);
        doc.others = rest.iter().map(|(a, b)| Sel { anchor: *a, cursor: prev(&doc.lines, *b).unwrap_or(*a), goal: None }).collect();
        self.mode = Mode::Normal;
        self.goal = None;
    }

    /// `Alt-,`: drops the main selection; the next one takes over.
    fn drop_main(&mut self, doc: &mut Document) {
        if doc.others.is_empty() {
            return;
        }
        let here = doc.anchor.min(doc.cursor);
        let k = doc.others.iter().position(|o| o.anchor.min(o.cursor) > here).unwrap_or(0);
        let next = doc.others.remove(k);
        doc.anchor = next.anchor;
        doc.cursor = next.cursor;
        self.goal = next.goal;
    }

    /// `)` and `(`: makes the selection `by` places along the main one.
    fn rotate(&mut self, doc: &mut Document, by: isize) {
        if doc.others.is_empty() {
            return;
        }
        let ranks = order(doc);
        let count = ranks.len() as isize;
        let target = (ranks[0] as isize + by).rem_euclid(count) as usize;
        if let Some(k) = ranks[1..].iter().position(|r| *r == target) {
            self.swap(doc, k);
        }
    }

    // --- Insert mode -------------------------------------------------------

    fn insert_key(&mut self, doc: &mut Document, key: HKey) {
        let go = |doc: &mut Document, motion: Motion| {
            doc.apply_plain(Action::Move { motion, select: false });
        };
        match key {
            HKey::Esc => {
                doc.grouped = false;
                self.mode = Mode::Normal;
                self.reset();
                settle(doc);
                doc.anchor = doc.cursor;
            }
            HKey::Char(c) => plain(doc, Action::Insert(c.to_string())),
            HKey::Enter | HKey::Ctrl('j') => plain(doc, Action::Enter),
            HKey::Backspace | HKey::Ctrl('h') => plain(doc, Action::Backspace),
            HKey::Delete | HKey::Ctrl('d') => plain(doc, Action::Delete),
            HKey::Tab => plain(doc, Action::Insert(" ".repeat(INDENT))),
            HKey::Ctrl('w') => {
                doc.apply_plain(Action::Move { motion: Motion::WordLeft, select: true });
                plain(doc, Action::Backspace);
            }
            HKey::Ctrl('u') => {
                doc.anchor = Pos::new(doc.cursor.line, 0);
                plain(doc, Action::Backspace);
            }
            HKey::Left => go(doc, Motion::Left),
            HKey::Right => go(doc, Motion::Right),
            HKey::Up => go(doc, Motion::Up),
            HKey::Down => go(doc, Motion::Down),
            HKey::Home => go(doc, Motion::LineStart),
            HKey::End => go(doc, Motion::LineEnd),
            HKey::PageUp => go(doc, Motion::PageUp(self.page())),
            HKey::PageDown => go(doc, Motion::PageDown(self.page())),
            _ => {}
        }
    }

    // --- Search and the command line ---------------------------------------

    fn command_key(&mut self, doc: &mut Document, key: HKey) {
        let Some((prefix, mut text)) = self.cmdline.take() else { return };
        match key {
            HKey::Esc => {}
            HKey::Backspace if text.is_empty() => {}
            HKey::Backspace => {
                text.pop();
                self.cmdline = Some((prefix, text));
            }
            HKey::Char(c) => {
                text.push(c);
                self.cmdline = Some((prefix, text));
            }
            HKey::Enter if prefix == ':' => self.command(doc, text.trim()),
            HKey::Enter if prefix == 's' || prefix == 'S' => {
                if !text.is_empty() {
                    self.select_matches(doc, &text, prefix == 'S');
                }
            }
            HKey::Enter => {
                if !text.is_empty() {
                    self.search = Some(text);
                }
                self.find_next(doc, prefix == '/');
            }
            _ => self.cmdline = Some((prefix, text)),
        }
    }

    fn command(&mut self, doc: &mut Document, command: &str) {
        match command {
            "" => {}
            "w" | "write" => self.requests.push(VimRequest::Write),
            "q" | "quit" => self.requests.push(VimRequest::Quit { force: false }),
            "q!" | "quit!" => self.requests.push(VimRequest::Quit { force: true }),
            "wq" | "x" | "write-quit" => self.requests.push(VimRequest::WriteQuit),
            other => match other.parse::<usize>() {
                Ok(line) => {
                    let line = line.saturating_sub(1).min(doc.lines.len() - 1);
                    self.goto(doc, Pos::new(line, 0), false);
                }
                Err(_) => self.say(format!("No such command: '{other}'")),
            },
        }
    }

    /// Selects the next or previous match of the search pattern, wrapping
    /// round the ends of the text.
    fn find_next(&mut self, doc: &mut Document, forward: bool) {
        let Some(pattern) = self.search.clone() else { return self.say("No search pattern") };
        let re = match compile(&pattern, true, true) {
            Ok(re) => re,
            Err(e) => return self.say(e),
        };
        let count = doc.lines.len();
        let (lo, _) = sel(doc);
        for i in 0..=count {
            let line = if forward { (lo.line + i) % count } else { (lo.line + count - i % count) % count };
            let matches = re.find_iter(&doc.lines[line]).filter(|m| !m.is_empty());
            // On the starting line, look past the selection first and come
            // back to what is before it only after going all the way round.
            let found = match (forward, i) {
                (true, 0) => matches.into_iter().find(|m| m.start() > lo.col),
                (true, _) if i == count => matches.into_iter().find(|m| m.start() <= lo.col),
                (true, _) => matches.into_iter().next(),
                (false, 0) => matches.filter(|m| m.start() < lo.col).last(),
                (false, _) if i == count => matches.filter(|m| m.start() >= lo.col).last(),
                (false, _) => matches.last(),
            };
            if let Some(m) = found {
                // In Select mode the match joins the selections.
                if self.mode == Mode::Select {
                    doc.others.push(Sel { anchor: doc.anchor, cursor: doc.cursor, goal: None });
                }
                doc.anchor = Pos::new(line, m.start());
                doc.cursor = Pos::new(line, prev_boundary(&doc.lines[line], m.end()));
                if i == count || (i > 0 && forward != (line > lo.line)) {
                    self.say("Wrapped around the document");
                }
                return;
            }
        }
        self.say(format!("Pattern not found: {pattern}"));
    }
}

// ---------------------------------------------------------------------------
// The selection, as characters.

/// The selection's first and last characters.
fn sel(doc: &Document) -> (Pos, Pos) {
    (doc.cursor.min(doc.anchor), doc.cursor.max(doc.anchor))
}

/// The selection as the gaps before its first character and after its last.
fn span(doc: &Document) -> (Pos, Pos) {
    let (lo, hi) = sel(doc);
    (lo, next(&doc.lines, hi).unwrap_or(hi))
}

fn selected(doc: &Document) -> String {
    let (lo, end) = span(doc);
    text_between(&doc.lines, lo, end)
}

fn line_last(doc: &Document, line: usize) -> Pos {
    let text = &doc.lines[line];
    Pos::new(line, if text.is_empty() { 0 } else { prev_boundary(text, text.len()) })
}

/// Selects the text between two gaps, cursor on its last character.
fn select_gaps(doc: &mut Document, start: Pos, end: Pos) {
    doc.anchor = start;
    doc.cursor = if end > start { prev(&doc.lines, end).unwrap_or(start) } else { start };
}

/// Switches the document's cursor and anchor to the gaps plain editing uses.
fn to_gaps(doc: &mut Document) {
    let (lo, end) = span(doc);
    doc.anchor = lo;
    doc.cursor = end;
}

/// The reverse of [`to_gaps`], after a plain action made a selection.
fn from_gaps(doc: &mut Document) {
    if let Some((a, b)) = doc.selection() {
        select_gaps(doc, a, b);
    }
}

/// Keeps every selection on real positions after an edit.
fn settle(doc: &mut Document) {
    doc.cursor = doc.clamp(doc.cursor);
    doc.anchor = doc.clamp(doc.anchor);
    for k in 0..doc.others.len() {
        let o = doc.others[k];
        doc.others[k] = Sel { anchor: doc.clamp(o.anchor), cursor: doc.clamp(o.cursor), ..o };
    }
}

/// Where each selection comes in document order: the main one first, then
/// the others as stored.
fn order(doc: &Document) -> Vec<usize> {
    let starts: Vec<Pos> = std::iter::once(doc.anchor.min(doc.cursor)).chain(doc.others.iter().map(|o| o.anchor.min(o.cursor))).collect();
    starts.iter().map(|s| starts.iter().filter(|t| *t < s).count()).collect()
}

/// Runs an edit that replaces the text between the gaps `from` and `to`,
/// then moves the other selections so each stays on the text it was on.
/// With `own`, this selection's ends move the same way. `from` may be left
/// out when the edit ends at the cursor and only it knows where it began,
/// as with Backspace.
fn tracked(doc: &mut Document, from: Option<Pos>, to: Pos, own: bool, f: impl FnOnce(&mut Document)) {
    let lines = doc.lines.len();
    let tail = doc.lines[to.line].len() - to.col;
    f(doc);
    // The text after the edit is untouched, which locates where it now ends.
    let line = (to.line + doc.lines.len()).saturating_sub(lines).min(doc.lines.len() - 1);
    let end = Pos::new(line, doc.lines[line].len().saturating_sub(tail));
    let from = from.unwrap_or(doc.cursor.min(to));
    let map = |p: Pos| {
        if p >= to {
            if p.line == to.line { Pos::new(end.line, end.col + (p.col - to.col)) } else { Pos::new(end.line + (p.line - to.line), p.col) }
        } else if p > from {
            end
        } else {
            p
        }
    };
    for o in &mut doc.others {
        o.anchor = map(o.anchor);
        o.cursor = map(o.cursor);
    }
    if own {
        doc.anchor = map(doc.anchor);
        doc.cursor = map(doc.cursor);
    }
}

/// Starts the undo step for a command, or joins the one already open: a
/// command over several selections, or a stay in Insert mode, undoes as one.
fn edit(doc: &mut Document) {
    if !doc.grouped {
        doc.begin(EditKind::Other);
        doc.grouped = true;
    }
}

/// Applies a plain editing action at this selection.
fn plain(doc: &mut Document, action: Action) {
    edit(doc);
    let (lo, hi) = (doc.cursor.min(doc.anchor), doc.cursor.max(doc.anchor));
    let point = lo == hi;
    let to = if point && action == Action::Delete { next(&doc.lines, hi).unwrap_or(hi) } else { hi };
    let from = if point && action == Action::Backspace { None } else { Some(lo) };
    tracked(doc, from, to, false, |doc| {
        doc.apply_plain(action);
    });
}

/// Deletes the selection, leaving the cursor where it started.
fn remove(doc: &mut Document) {
    let (lo, end) = span(doc);
    tracked(doc, Some(lo), end, false, |doc| {
        doc.anchor = lo;
        doc.cursor = end;
        doc.delete_selection();
        doc.anchor = doc.cursor;
    });
}

/// Replaces the selection and returns the gaps around the new text.
fn replace(doc: &mut Document, text: &str) -> (Pos, Pos) {
    let (lo, end) = span(doc);
    tracked(doc, Some(lo), end, false, |doc| {
        doc.anchor = lo;
        doc.cursor = end;
        doc.insert(text);
    });
    (lo, doc.cursor)
}

fn remove_char(doc: &mut Document, p: Pos) {
    let after = next(&doc.lines, p).unwrap_or(p);
    if p.col < doc.lines[p.line].len() {
        tracked(doc, Some(p), after, true, |doc| {
            doc.lines[p.line].remove(p.col);
        });
    }
}

fn set_char(doc: &mut Document, p: Pos, c: char) {
    let after = next(&doc.lines, p).unwrap_or(p);
    if p.col < doc.lines[p.line].len() {
        tracked(doc, Some(p), after, true, |doc| doc.lines[p.line].replace_range(p.col..after.col, &c.to_string()));
    }
}

/// `_`: shrinks the selection to leave out blank space at its ends.
fn trim(doc: &mut Document) {
    let (mut lo, mut hi) = sel(doc);
    while lo < hi && ch(&doc.lines, lo).is_whitespace() {
        lo = next(&doc.lines, lo).unwrap_or(hi);
    }
    while hi > lo && ch(&doc.lines, hi).is_whitespace() {
        hi = prev(&doc.lines, hi).unwrap_or(lo);
    }
    (doc.anchor, doc.cursor) = if doc.cursor >= doc.anchor { (lo, hi) } else { (hi, lo) };
}

#[cfg(test)]
mod tests {
    use super::super::Keymap;
    use super::*;
    use crate::event::Modifiers;

    fn doc(text: &str) -> Document {
        let mut d = Document::new(text);
        d.set_keymap(Keymap::Helix);
        d
    }

    /// Presses keys. Plain characters are typed; `<esc>`, `<ret>`, `<bs>`,
    /// `<space>`, `<A-x>` and `<C-x>` name the rest.
    fn press(d: &mut Document, keys: &str) {
        let mut rest = keys;
        while let Some(c) = rest.chars().next() {
            let mut modifiers = Modifiers::default();
            let (key, used) = match rest.strip_prefix('<').and_then(|r| r.split_once('>')) {
                Some((name, _)) if matches!(name, "esc" | "ret" | "bs" | "space") || name.starts_with("A-") || name.starts_with("C-") => {
                    let key = match name {
                        "esc" => Key::Escape,
                        "ret" => Key::Enter,
                        "bs" => Key::Backspace,
                        "space" => Key::Space,
                        _ => {
                            if name.starts_with("A-") {
                                modifiers.alt = true;
                            } else {
                                modifiers.ctrl = true;
                            }
                            Key::Character(name[2..].to_owned())
                        }
                    };
                    (key, name.len() + 2)
                }
                _ => (if c == ' ' { Key::Space } else { Key::Character(c.to_string()) }, c.len_utf8()),
            };
            let text = match &key {
                Key::Character(s) if !modifiers.ctrl && !modifiers.alt => Some(s.clone()),
                Key::Space => Some(" ".to_owned()),
                _ => None,
            };
            d.apply(Action::Key(KeyEvent { key, pressed: true, repeat: false, modifiers, text }));
            rest = &rest[used..];
        }
    }

    /// The selected text, and whether the cursor is at its end.
    fn sel_text(d: &Document) -> String {
        selected(d)
    }

    fn after(text: &str, keys: &str) -> Document {
        let mut d = doc(text);
        press(&mut d, keys);
        d
    }

    fn message(d: &Document) -> String {
        d.mode_status().unwrap().message.unwrap_or_default()
    }

    #[test]
    fn word_motions_select_what_they_pass() {
        let mut d = doc("hello world foo");
        press(&mut d, "w");
        assert_eq!(sel_text(&d), "hello ");
        press(&mut d, "w");
        assert_eq!(sel_text(&d), "world ");
        press(&mut d, "w");
        assert_eq!(sel_text(&d), "foo");
        press(&mut d, "w");
        assert_eq!(sel_text(&d), "foo", "nothing further at the end");
        press(&mut d, "b");
        assert_eq!(sel_text(&d), "foo");
        assert!(d.cursor() < d.anchor(), "going back puts the cursor at the start");
        press(&mut d, "b");
        assert_eq!(sel_text(&d), "world ");

        let mut d = doc("hello world");
        press(&mut d, "e");
        assert_eq!(sel_text(&d), "hello");
        press(&mut d, "e");
        assert_eq!(sel_text(&d), " world");
    }

    #[test]
    fn punctuation_splits_words_but_not_long_words() {
        let mut d = doc("foo.bar baz");
        press(&mut d, "w");
        assert_eq!(sel_text(&d), "foo");
        press(&mut d, "w");
        assert_eq!(sel_text(&d), ".");
        press(&mut d, "w");
        assert_eq!(sel_text(&d), "bar ");
        assert_eq!(sel_text(&after("foo.bar baz", "W")), "foo.bar ");
        assert_eq!(sel_text(&after("foo.bar baz", "E")), "foo.bar");
    }

    #[test]
    fn word_motions_cross_lines_and_handle_other_scripts() {
        let mut d = doc("one\n  two");
        press(&mut d, "w");
        assert_eq!(sel_text(&d), "one");
        press(&mut d, "w");
        assert_eq!(sel_text(&d), "  ", "the indentation of the next line");
        press(&mut d, "w");
        assert_eq!(sel_text(&d), "two");

        let mut d = doc("héllo wörld");
        press(&mut d, "w");
        assert_eq!(sel_text(&d), "héllo ");
        press(&mut d, "d");
        assert_eq!(d.text(), "wörld");
    }

    #[test]
    fn counts_repeat_motions() {
        assert_eq!(sel_text(&after("a b c d", "2w")), "b ");
        assert_eq!(after("abcdef", "3l").cursor(), Pos::new(0, 3));
        assert_eq!(after("a\nb\nc\nd", "2j").cursor().line, 2);
        assert_eq!(after("abc", "10l").cursor(), Pos::new(0, 3), "stops at the end");
    }

    #[test]
    fn plain_moves_keep_a_one_character_selection() {
        let mut d = doc("abc\nde");
        press(&mut d, "l");
        assert_eq!((d.cursor(), sel_text(&d).as_str()), (Pos::new(0, 1), "b"));
        press(&mut d, "lll");
        assert_eq!(d.cursor(), Pos::new(1, 0), "past the line break onto the next line");
        press(&mut d, "h");
        assert_eq!(sel_text(&d), "\n", "the cursor can sit on the line break");
        assert_eq!(d.display_selection(), None, "one character shows as the block caret alone");
        assert!(d.block_caret());
    }

    #[test]
    fn up_and_down_remember_their_column() {
        let mut d = doc("abcdef\nab\nabcdef");
        press(&mut d, "4l");
        press(&mut d, "j");
        assert_eq!(d.cursor(), Pos::new(1, 1), "the short line's last character");
        press(&mut d, "j");
        assert_eq!(d.cursor(), Pos::new(2, 4), "back to the column it started in");
        press(&mut d, "k");
        press(&mut d, "k");
        assert_eq!(d.cursor(), Pos::new(0, 4));
    }

    #[test]
    fn select_mode_extends_instead_of_moving() {
        let mut d = doc("hello world foo");
        press(&mut d, "v");
        assert_eq!(d.mode_status().unwrap().label, "SEL");
        press(&mut d, "ww");
        assert_eq!(sel_text(&d), "hello world ");
        press(&mut d, "l");
        assert_eq!(sel_text(&d), "hello world f");
        press(&mut d, "<esc>");
        assert_eq!(d.mode_status().unwrap().label, "NOR");
        assert_eq!(sel_text(&d), "hello world f", "leaving Select keeps the selection");
        press(&mut d, ";");
        assert_eq!(sel_text(&d), "f", "collapsed onto the cursor");
    }

    #[test]
    fn flipping_and_selecting_everything() {
        let mut d = doc("hello world");
        press(&mut d, "w");
        let (anchor, cursor) = (d.anchor(), d.cursor());
        press(&mut d, "<A-;>");
        assert_eq!((d.anchor(), d.cursor()), (cursor, anchor));
        assert_eq!(sel_text(&d), "hello ");
        // macOS sends an ellipsis for Option+semicolon.
        let mods = Modifiers { alt: true, ..Default::default() };
        d.apply(Action::Key(KeyEvent { key: Key::Character("…".into()), pressed: true, repeat: false, modifiers: mods, text: Some("…".into()) }));
        assert_eq!((d.anchor(), d.cursor()), (anchor, cursor));
        press(&mut d, "%");
        assert_eq!(sel_text(&d), "hello world");
        assert_eq!(d.display_selection(), Some((Pos::new(0, 0), Pos::new(0, 11), false)));
    }

    #[test]
    fn x_selects_lines_and_grows() {
        let mut d = doc("one\ntwo\nthree");
        press(&mut d, "lx");
        assert_eq!(sel_text(&d), "one\n");
        press(&mut d, "x");
        assert_eq!(sel_text(&d), "one\ntwo\n");
        press(&mut d, "d");
        assert_eq!(d.text(), "three");
        assert_eq!(sel_text(&after("one\ntwo\nthree", "3x")), "one\ntwo\nthree");
        // X fills out the lines without taking another.
        let mut d = doc("one\ntwo\nthree");
        press(&mut d, "lvjX");
        assert_eq!(sel_text(&d), "one\ntwo\n");
    }

    #[test]
    fn delete_yanks_and_paste_puts_lines_on_their_own_lines() {
        let mut d = doc("one\ntwo\nthree");
        press(&mut d, "xd");
        assert_eq!(d.text(), "two\nthree");
        press(&mut d, "p");
        assert_eq!(d.text(), "two\none\nthree", "after the cursor's line");
        assert_eq!(sel_text(&d), "one\n", "and what was pasted is selected");
        press(&mut d, "P");
        assert_eq!(d.text(), "two\none\none\nthree");
        // On the last line there is no line below to start on.
        let mut d = doc("one\ntwo");
        press(&mut d, "xdp");
        assert_eq!(d.text(), "two\none");
        assert_eq!(sel_text(&d), "one");
    }

    #[test]
    fn paste_within_a_line_goes_after_or_before_the_selection() {
        let mut d = doc("hello world");
        press(&mut d, "wd");
        assert_eq!(d.text(), "world");
        press(&mut d, "P");
        assert_eq!(d.text(), "hello world");
        assert_eq!(sel_text(&d), "hello ");
        press(&mut d, "2p");
        assert_eq!(d.text(), "hello hello hello world");
        // Alt-d deletes without touching what was yanked.
        let mut d = doc("ab cd");
        press(&mut d, "wy");
        press(&mut d, "w<A-d>");
        assert_eq!(d.text(), "ab ");
        press(&mut d, "P");
        assert_eq!(d.text(), "ab ab ");
    }

    #[test]
    fn replace_with_yanked_text_and_named_registers() {
        let mut d = doc("one two");
        press(&mut d, "ey");
        press(&mut d, "wwR");
        assert_eq!(d.text(), "one one");
        let mut d = doc("one two");
        press(&mut d, "e\"ay");
        press(&mut d, "wwy");
        press(&mut d, "\"aP");
        assert_eq!(d.text(), "one onetwo", "register a still holds the first yank");
        press(&mut d, "\"zp");
        assert_eq!(message(&d), "Register 'z' is empty");
    }

    #[test]
    fn insert_commands_start_in_the_right_place() {
        let typed = |text: &str, keys: &str| after(text, keys).text();
        assert_eq!(typed("abc", "liX<esc>"), "aXbc");
        assert_eq!(typed("abc", "laX<esc>"), "abXc");
        assert_eq!(typed("hello world", "wiX<esc>"), "Xhello world", "before the selection");
        assert_eq!(typed("hello world", "eaX<esc>"), "helloX world", "after the selection");
        assert_eq!(typed("  abc", "llllIX<esc>"), "  Xabc");
        assert_eq!(typed("abc", "AX<esc>"), "abcX");
        assert_eq!(typed("  one\ntwo", "oX<esc>"), "  one\n  X\ntwo");
        assert_eq!(typed("  one\ntwo", "OX<esc>"), "  X\n  one\ntwo");
        let d = after("abc", "iX<esc>");
        assert_eq!(d.mode_status().unwrap().label, "NOR");
        assert!(!d.mode_status().unwrap().insert);
    }

    #[test]
    fn a_stay_in_insert_mode_is_one_undo_step() {
        let mut d = doc("ab");
        press(&mut d, "i");
        assert!(d.mode_status().unwrap().insert);
        assert!(!d.block_caret());
        press(&mut d, "xyz<ret>q<bs><esc>");
        assert_eq!(d.text(), "xyz\nab");
        press(&mut d, "u");
        assert_eq!(d.text(), "ab");
        press(&mut d, "U");
        assert_eq!(d.text(), "xyz\nab");
    }

    #[test]
    fn change_replaces_the_selection_and_keeps_a_line_for_whole_lines() {
        assert_eq!(after("hello world", "ecbye<esc>").text(), "bye world");
        let mut d = doc("  foo\nbar");
        press(&mut d, "xcx<esc>");
        assert_eq!(d.text(), "  x\nbar", "the line and its indentation stay");
        press(&mut d, "u");
        assert_eq!(d.text(), "  foo\nbar", "the change and its typing undo together");
        press(&mut d, "p");
        assert_eq!(d.text(), "  foo\n  foo\nbar", "and what was changed was yanked");
    }

    #[test]
    fn find_and_till_select_up_to_a_character() {
        assert_eq!(sel_text(&after("a,b,c", "f,")), "a,");
        assert_eq!(sel_text(&after("a,b,c", "2f,")), "a,b,");
        assert_eq!(sel_text(&after("a,b,c", "2t,")), "a,b");
        let mut d = doc("a,b,c");
        press(&mut d, "gl");
        press(&mut d, "F,");
        assert_eq!(sel_text(&d), ",c");
        press(&mut d, "fz");
        assert_eq!(message(&d), "'z' is not on this line");
    }

    #[test]
    fn the_goto_menu() {
        let text = "  first\nsecond\n  third";
        assert_eq!(after(text, "ge").cursor(), Pos::new(2, 0));
        assert_eq!(after(text, "gegg").cursor(), Pos::new(0, 0));
        assert_eq!(after(text, "2gg").cursor(), Pos::new(1, 0));
        assert_eq!(after(text, "3G").cursor(), Pos::new(2, 0));
        assert_eq!(after(text, "G").cursor(), Pos::new(2, 0));
        assert_eq!(after(text, "gl").cursor(), Pos::new(0, 6));
        assert_eq!(after(text, "glgh").cursor(), Pos::new(0, 0));
        assert_eq!(after(text, "gs").cursor(), Pos::new(0, 2));
        assert_eq!(sel_text(&after(text, "vgl")), "  first", "in Select mode they extend");
        assert_eq!(after(text, ":2<ret>").cursor(), Pos::new(1, 0));
    }

    #[test]
    fn text_objects_and_matching_brackets() {
        let text = "call(foo bar) end";
        assert_eq!(sel_text(&after(text, "6lmi(")), "foo bar");
        assert_eq!(sel_text(&after(text, "6lma(")), "(foo bar)");
        assert_eq!(sel_text(&after(text, "6lmiw")), "foo");
        assert_eq!(sel_text(&after(text, "6lmaw")), "foo ");
        assert_eq!(after(text, "4lmm").cursor(), Pos::new(0, 12));
        assert_eq!(after(text, "4lmmmm").cursor(), Pos::new(0, 4));
        assert_eq!(sel_text(&after("say \"hi there\" now", "6lmi\"")), "hi there");
        assert_eq!(message(&after(text, "mi[")), "No '[' around the cursor");
        let d = after("a\nb\n\nc", "mip");
        assert_eq!(sel_text(&d), "a\nb\n");
        assert_eq!(sel_text(&after("a\nb\n\nc", "map")), "a\nb\n\n");
    }

    #[test]
    fn surround_add_delete_and_replace() {
        let mut d = doc("a word b");
        press(&mut d, "2lmiwms(");
        assert_eq!(d.text(), "a (word) b");
        assert_eq!(sel_text(&d), "(word)");
        press(&mut d, "mr(\"");
        assert_eq!(d.text(), "a \"word\" b");
        press(&mut d, "md\"");
        assert_eq!(d.text(), "a word b");
        press(&mut d, "md(");
        assert_eq!(message(&d), "No surrounding '('");
        press(&mut d, "u");
        assert_eq!(d.text(), "a \"word\" b", "each is its own undo step");
    }

    #[test]
    fn replacing_characters_and_changing_case() {
        assert_eq!(after("abc def", "erx").text(), "xxx def");
        assert_eq!(after("abc def", "e~").text(), "ABC def");
        assert_eq!(after("aBc", "e~").text(), "AbC");
        assert_eq!(after("ABC", "e`").text(), "abc");
        assert_eq!(after("abc", "e<A-`>").text(), "ABC");
        let d = after("ab\ncd", "%r-");
        assert_eq!(d.text(), "--\n--", "line breaks are left alone");
        assert_eq!(sel_text(&d), "--\n--");
    }

    #[test]
    fn indenting_and_joining_lines() {
        let mut d = doc("a\n\nb");
        press(&mut d, "%>");
        assert_eq!(d.text(), "    a\n\n    b", "empty lines stay empty");
        press(&mut d, "<");
        assert_eq!(d.text(), "a\n\nb");
        assert_eq!(after("  a", "2>").text(), "          a");
        assert_eq!(after("  a", "<").text(), "a", "never past the margin");
        assert_eq!(after("a\n  b\nc", "J").text(), "a b\nc");
        assert_eq!(after("a\n  b\nc", "%J").text(), "a b c");
        assert_eq!(after("only", "J").text(), "only");
    }

    #[test]
    fn search_selects_matches_and_wraps() {
        let mut d = doc("foo bar\nbaz foo");
        press(&mut d, "/foo<ret>");
        assert_eq!((d.anchor(), d.cursor()), (Pos::new(1, 4), Pos::new(1, 6)));
        assert_eq!(sel_text(&d), "foo");
        press(&mut d, "n");
        assert_eq!(d.anchor(), Pos::new(0, 0));
        assert_eq!(message(&d), "Wrapped around the document");
        press(&mut d, "N");
        assert_eq!(d.anchor(), Pos::new(1, 4));
        press(&mut d, "/b.[rz]<ret>");
        assert_eq!(sel_text(&d), "bar", "patterns are regular expressions, and the search wrapped");
        press(&mut d, "?baz<ret>");
        assert_eq!(d.anchor(), Pos::new(1, 0), "backwards from the first line wraps to the last");
        press(&mut d, "/nope<ret>");
        assert_eq!(message(&d), "Pattern not found: nope");
        press(&mut d, "/(<ret>");
        assert_eq!(message(&d), "Invalid pattern: (");
    }

    #[test]
    fn star_searches_for_the_selection() {
        let mut d = doc("a.b x a.b axb");
        press(&mut d, "W");
        press(&mut d, ";");
        press(&mut d, "ghvll*");
        assert_eq!(sel_text(&d), "a.b");
        press(&mut d, "<esc>n");
        assert_eq!(d.anchor(), Pos::new(0, 6), "the dot is matched literally, so not axb");
        press(&mut d, "n");
        assert_eq!(d.anchor(), Pos::new(0, 0));
    }

    #[test]
    fn the_command_line_asks_the_app_to_save_and_quit() {
        let mut d = doc("x");
        press(&mut d, ":w");
        assert_eq!(d.mode_status().unwrap().command_line.as_deref(), Some(":w"));
        press(&mut d, "<ret>:q!<ret>:x<ret>:quit<ret>");
        assert_eq!(d.take_vim_requests(), [VimRequest::Write, VimRequest::Quit { force: true }, VimRequest::WriteQuit, VimRequest::Quit { force: false }]);
        assert!(d.take_vim_requests().is_empty());
        press(&mut d, ":frob<ret>");
        assert_eq!(message(&d), "No such command: 'frob'");
        press(&mut d, ":wx<bs><bs><bs>");
        assert_eq!(d.mode_status().unwrap().command_line, None, "backspace on an empty line leaves it");
        assert_eq!(d.text(), "x", "and nothing was typed into the text");
    }

    #[test]
    fn space_uses_the_system_clipboard() {
        let mut d = doc("hello world");
        press(&mut d, "e<space>");
        assert_eq!(d.vim_view().unwrap().clipboard_need, ClipboardNeed::Always, "the editor reads the clipboard before the next key");
        press(&mut d, "y");
        assert_eq!(d.vim_view().unwrap().clipboard.map(|(_, t)| t).as_deref(), Some("hello"));
        assert_eq!(d.vim_view().unwrap().clipboard_need, ClipboardNeed::None);
        d.apply(Action::Clipboard("XY".into()));
        press(&mut d, "<space>p");
        assert_eq!(d.text(), "helloXY world");
        press(&mut d, "<space>P");
        assert_eq!(d.text(), "helloXYXY world");
        press(&mut d, "%<space>R");
        assert_eq!(d.text(), "XY");
        // Plain yank and paste do not touch it.
        press(&mut d, "y");
        assert_eq!(d.vim_view().unwrap().clipboard.map(|(_, t)| t).as_deref(), Some("hello"));
    }

    #[test]
    fn pending_keys_show_in_the_status_and_escape_cancels_them() {
        let mut d = doc("a (b) c");
        press(&mut d, "3\"am");
        assert_eq!(d.mode_status().unwrap().pending, "3\"am");
        press(&mut d, "<esc>");
        assert_eq!(d.mode_status().unwrap().pending, "");
        press(&mut d, "d");
        assert_eq!(d.text(), " (b) c", "the count and register did not carry over");
    }

    /// Every selection's text, in document order.
    fn all(d: &Document) -> Vec<String> {
        let mut spans: Vec<(Pos, Pos)> = std::iter::once((d.anchor, d.cursor)).chain(d.others.iter().map(|o| (o.anchor, o.cursor))).map(|(a, c)| (a.min(c), a.max(c))).collect();
        spans.sort();
        spans.into_iter().map(|(lo, hi)| text_between(&d.lines, lo, next(&d.lines, hi).unwrap_or(hi))).collect()
    }

    fn count(d: &Document) -> usize {
        d.mode_status().unwrap().selections
    }

    #[test]
    fn copying_a_cursor_down_types_on_every_line() {
        let mut d = doc("abc\nabc\nabc");
        press(&mut d, "CC");
        assert_eq!(count(&d), 3);
        assert_eq!(d.cursor(), Pos::new(2, 0), "the main selection is the newest copy");
        press(&mut d, "iX<esc>");
        assert_eq!(d.text(), "Xabc\nXabc\nXabc");
        assert_eq!(count(&d), 3, "the cursors survive leaving Insert mode");
        press(&mut d, "u");
        assert_eq!(d.text(), "abc\nabc\nabc", "typing at every cursor is one undo step");
        assert_eq!(count(&d), 3, "and undo brings the selections back with the text");
        assert_eq!(count(&after("a\nb\nc\nd", "2C")), 3);
        assert_eq!(count(&after("a\nb", "jC")), 1, "nowhere below the last line");
        assert_eq!(count(&after("a\nb\nc", "j<A-C>")), 2);
    }

    #[test]
    fn copying_skips_lines_too_short_to_hold_the_selection() {
        let d = after("abcd\nab\nabcd", "3lC");
        assert_eq!(count(&d), 2);
        assert_eq!(d.cursor(), Pos::new(2, 3));
        assert_eq!(all(&d), ["d", "d"]);
    }

    #[test]
    fn selecting_matches_inside_the_selection() {
        let mut d = doc("foo bar foo baz");
        press(&mut d, "%s");
        assert_eq!(d.mode_status().unwrap().command_line.as_deref(), Some("select:"));
        press(&mut d, "foo<ret>");
        assert_eq!(all(&d), ["foo", "foo"]);
        assert_eq!(d.extra_selections(), [ExtraSelection { cursor: Pos::new(0, 10), range: Some((Pos::new(0, 8), Pos::new(0, 11))) }]);
        press(&mut d, "cquux<esc>");
        assert_eq!(d.text(), "quux bar quux baz", "the second change lands in the right place after the first grew the line");
        let d = after("foo", "%sxyz<ret>");
        assert_eq!((count(&d), message(&d).as_str()), (1, "No matches in the selection"));
        assert_eq!(sel_text(&d), "foo", "and the selection is left alone");
    }

    #[test]
    fn splitting_the_selection() {
        let mut d = doc("a, b, c");
        press(&mut d, "%S, <ret>");
        assert_eq!(all(&d), ["a", "b", "c"]);
        press(&mut d, "~");
        assert_eq!(d.text(), "A, B, C");
        let mut d = doc("one\ntwo\nthree");
        press(&mut d, "%<A-s>");
        assert_eq!(all(&d), ["one", "two", "three"], "one selection per line, without the line breaks");
        press(&mut d, "A;<esc>");
        assert_eq!(d.text(), "one;\ntwo;\nthree;");
    }

    #[test]
    fn each_selection_moves_and_changes_on_its_own() {
        let mut d = doc("ab cd\nef gh");
        press(&mut d, "Cw");
        assert_eq!(all(&d), ["ab ", "ef "]);
        press(&mut d, "w");
        assert_eq!(all(&d), ["cd", "gh"]);
        press(&mut d, "gh");
        assert_eq!(all(&d), ["a", "e"]);
        press(&mut d, "vgl");
        assert_eq!(all(&d), ["ab cd", "ef gh"], "Select mode extends every one");
        press(&mut d, "<esc>_;");
        assert_eq!(all(&d), ["d", "h"]);
        press(&mut d, "rX");
        assert_eq!(d.text(), "ab cX\nef gX");
    }

    #[test]
    fn edits_on_one_line_keep_the_other_selections_in_step() {
        assert_eq!(after("a b c", "%s\\w<ret>cxx<esc>").text(), "xx xx xx");
        assert_eq!(after("x1 x2 x3", "%sx<ret>d").text(), "1 2 3");
        assert_eq!(after("ab cd", "%s\\w+<ret>ms(").text(), "(ab) (cd)");
        assert_eq!(after("(ab) (cd)", "%s\\w+<ret>md(").text(), "ab cd");
        assert_eq!(after("ab cd", "%s\\w+<ret>>").text(), "    ab cd", "a line is indented once however many selections are on it");
        assert_eq!(all(&after("ab cd", "%s\\w+<ret>>")), ["ab", "cd"], "and they stay on their words");
    }

    #[test]
    fn edits_that_add_or_remove_lines_keep_selections_in_step() {
        assert_eq!(after("a\nb", "CoX<esc>").text(), "a\nX\nb\nX");
        assert_eq!(after("a\nb", "COX<esc>").text(), "X\na\nX\nb");
        assert_eq!(after("a\nb\nc", "CCxd").text(), "");
        assert_eq!(after("a\nb\nc", "%<A-s>d").text(), "\n\n");
        assert_eq!(after("a\n  b\nc\n  d", "%s[ac]<ret>J").text(), "a b\nc d");
        // Typing, new lines and backspace in Insert mode.
        assert_eq!(after("ab\ncd", "CaX<ret>Y<bs>Z<esc>").text(), "aX\nZb\ncX\nZd");
        // Two cursors on one line.
        assert_eq!(after("ab", "%s.<ret>i-<esc>").text(), "-a-b");
        assert_eq!(after("ab", "%s.<ret>a-<esc>").text(), "a-b-");
    }

    #[test]
    fn yank_and_paste_pair_up_with_the_selections() {
        let mut d = doc("a b");
        press(&mut d, "%s\\w<ret>yp");
        assert_eq!(d.text(), "aa bb", "each pastes its own");
        assert_eq!(message(&after("a b", "%s\\w<ret>y")), "Yanked 2 selections");
        // One selection pasting what several yanked takes the first piece.
        let mut d = doc("a b");
        press(&mut d, "%s\\w<ret>y,P");
        assert_eq!(d.text(), "aa b");
        // More selections than pieces: the rest take the last.
        let mut d = doc("x y z");
        press(&mut d, "y%s\\w<ret>R");
        assert_eq!(d.text(), "x x x");
        let mut d = doc("ab cd");
        press(&mut d, "%s\\w+<ret><space>y");
        assert_eq!(d.vim_view().unwrap().clipboard.map(|(_, t)| t).as_deref(), Some("ab\ncd"));
    }

    #[test]
    fn selections_that_come_to_overlap_merge() {
        let mut d = doc("abc def");
        press(&mut d, "%s[ad]<ret>");
        assert_eq!(all(&d), ["a", "d"]);
        press(&mut d, "v3l");
        assert_eq!(all(&d), ["abc ", "def"], "side by side is not overlapping");
        press(&mut d, "l");
        assert_eq!(count(&d), 1, "the first now reaches into the second, so they are one");
        assert_eq!(sel_text(&d), "abc def");
        // Cursors that land on the same spot become one.
        let d = after("ab\ncd", "Cgg");
        assert_eq!(count(&d), 1);
    }

    #[test]
    fn choosing_among_selections() {
        let mut d = doc("a b c");
        press(&mut d, "%s\\w<ret>");
        assert_eq!(sel_text(&d), "a", "the first match is the main selection");
        press(&mut d, ")");
        assert_eq!(sel_text(&d), "b");
        press(&mut d, "((");
        assert_eq!(sel_text(&d), "c", "and it goes round");
        press(&mut d, "<A-,>");
        assert_eq!((all(&d), sel_text(&d)), (vec!["a".to_owned(), "b".to_owned()], "a".to_owned()));
        press(&mut d, "),");
        assert_eq!((count(&d), sel_text(&d).as_str()), (1, "b"));
        assert!(d.extra_selections().is_empty());
    }

    #[test]
    fn search_in_select_mode_adds_a_selection() {
        let mut d = doc("foo x foo");
        press(&mut d, "/foo<ret>");
        assert_eq!((count(&d), d.anchor()), (1, Pos::new(0, 6)), "in Normal mode the selection moves to the match");
        press(&mut d, "vn");
        assert_eq!(all(&d), ["foo", "foo"]);
        press(&mut d, "<esc>%");
        assert_eq!(count(&d), 1, "selecting everything is one selection again");
    }

    #[test]
    fn trimming_blank_space_from_a_selection() {
        let mut d = doc("hello world");
        press(&mut d, "w_");
        assert_eq!(sel_text(&d), "hello");
        press(&mut d, "b_");
        assert!(d.cursor() <= d.anchor(), "direction is kept");
    }

    #[test]
    fn the_pointer_and_shortcuts_with_several_selections() {
        let mut d = doc("ab\ncd");
        press(&mut d, "C");
        // A paste shortcut replaces every selection.
        d.apply(Action::Insert("X".into()));
        assert_eq!(d.text(), "Xb\nXd");
        d.apply(Action::Undo);
        assert_eq!((d.text().as_str(), count(&d)), ("ab\ncd", 2));
        d.apply(Action::Indent);
        assert_eq!(d.text(), "    ab\n    cd");
        d.apply(Action::Click { pos: Pos::new(0, 1), select: false });
        assert_eq!(count(&d), 1, "a click goes back to one selection");
        // Leaving Helix drops the extra selections.
        press(&mut d, "C");
        d.set_keymap(Keymap::Plain);
        assert!(d.extra_selections().is_empty());
    }

    #[test]
    fn the_pointer_and_shortcuts_act_on_the_selection_as_shown() {
        let mut d = doc("hello world");
        d.apply(Action::Click { pos: Pos::new(0, 6), select: false });
        assert_eq!(sel_text(&d), "w");
        d.apply(Action::Drag(Pos::new(0, 8)));
        assert_eq!(sel_text(&d), "wor");
        d.apply(Action::SelectWord(Pos::new(0, 1)));
        assert_eq!(sel_text(&d), "hello");
        assert_eq!(d.selected_text(), "hello", "what a copy shortcut would take");
        // A paste shortcut replaces all of it, last character included.
        d.apply(Action::Insert("bye".into()));
        assert_eq!(d.text(), "bye world");
        d.apply(Action::SelectAll);
        assert_eq!(sel_text(&d), "bye world");
        d.apply(Action::Undo);
        assert_eq!(d.text(), "hello world");
    }

    #[test]
    fn switching_keymaps() {
        let mut d = Document::new("hello world");
        assert_eq!((d.keymap(), d.mode_status()), (Keymap::Plain, None));
        d.set_keymap(Keymap::Helix);
        assert_eq!(d.mode_status().unwrap().label, "NOR");
        d.set_keymap(Keymap::Vim);
        assert_eq!((d.keymap(), d.mode_status().unwrap().label), (Keymap::Vim, "NORMAL"));
        assert!(d.vim().is_some());
        d.set_vim(false);
        assert_eq!(d.keymap(), Keymap::Plain);
        assert!(d.vim_view().is_none());
        // Plain keys are not interpreted.
        d.apply(Action::Key(KeyEvent { key: Key::Character("x".into()), pressed: true, repeat: false, modifiers: Modifiers::default(), text: Some("x".into()) }));
        assert_eq!(d.text(), "hello world");
    }

    #[test]
    fn scrolling_requests_and_pages() {
        let mut d = doc(&(0..100).map(|i| i.to_string()).collect::<Vec<_>>().join("\n"));
        d.apply(Action::Viewport { top: 0, lines: 21 });
        press(&mut d, "<C-d>");
        assert_eq!(d.cursor().line, 10, "half a page");
        press(&mut d, "<C-f>");
        assert_eq!(d.cursor().line, 30, "a page, less one line of overlap");
        press(&mut d, "<C-b><C-u>");
        assert_eq!(d.cursor().line, 0);
        press(&mut d, "zz");
        assert_eq!(d.vim_view().unwrap().scroll.map(|(_, s)| s), Some(Scroll::Center));
        d.apply(Action::Viewport { top: 40, lines: 21 });
        assert_eq!(after_view(&mut d, "gt"), 40);
        assert_eq!(after_view(&mut d, "gc"), 50);
        assert_eq!(after_view(&mut d, "gb"), 60);
    }

    fn after_view(d: &mut Document, keys: &str) -> usize {
        press(d, keys);
        d.cursor().line
    }
}
