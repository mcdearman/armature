//! The text model behind [`TextEditor`](super::TextEditor).

mod vim;

pub use vim::{BlockSelection, ClipboardNeed, Mode, Scroll, VimRequest, VimStatus, VimView};

use std::fmt;

/// A position in a document. `col` is a byte offset into the line and always
/// falls on a character boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// Where a cursor movement goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    /// The first non-blank character, or column 0 if already there.
    LineStart,
    LineEnd,
    DocStart,
    DocEnd,
    /// Up by this many lines.
    PageUp(usize),
    /// Down by this many lines.
    PageDown(usize),
}

/// An edit or cursor change. Editors send these to the application, which
/// applies them with [`Document::apply`].
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Replace the selection with text (typing or paste).
    Insert(String),
    /// New line, keeping the current indentation.
    Enter,
    Backspace,
    Delete,
    Move { motion: Motion, select: bool },
    Click { pos: Pos, select: bool },
    Drag(Pos),
    SelectWord(Pos),
    SelectAll,
    /// Drop the selection, keeping the cursor.
    Collapse,
    Indent,
    Outdent,
    Undo,
    Redo,
    /// A key press for Vim mode. Ignored unless Vim mode is on.
    Key(crate::event::KeyEvent),
    /// The system clipboard's text, sent before a Vim command that reads
    /// the `+` register.
    Clipboard(String),
    /// The lines on screen, so Vim can scroll by pages and use `H M L`.
    Viewport { top: usize, lines: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditKind {
    None,
    Typing,
    Other,
}

#[derive(Clone)]
struct Snapshot {
    lines: Vec<String>,
    cursor: Pos,
    anchor: Pos,
}

/// Editable text with a cursor, selection and undo history.
#[derive(Clone)]
pub struct Document {
    lines: Vec<String>,
    cursor: Pos,
    anchor: Pos,
    goal: Option<usize>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: EditKind,
    revision: u64,
    /// While set, edits join the current undo step (a Vim command or insert session).
    grouped: bool,
    vim: Option<Box<vim::Vim>>,
}

/// Spaces per indentation level. Tabs are expanded to this many spaces.
pub const INDENT: usize = 4;

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n").replace('\t', &" ".repeat(INDENT))
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

impl fmt::Debug for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Document").field("lines", &self.lines.len()).field("cursor", &self.cursor).field("revision", &self.revision).finish()
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new("")
    }
}

impl Document {
    pub fn new(text: &str) -> Self {
        let lines: Vec<String> = normalize(text).split('\n').map(str::to_owned).collect();
        Self { lines, cursor: Pos::default(), anchor: Pos::default(), goal: None, undo: vec![], redo: vec![], last_edit: EditKind::None, revision: 0, grouped: false, vim: None }
    }

    /// Turns Vim-style modal editing on or off. It starts in Normal mode.
    pub fn set_vim(&mut self, on: bool) {
        if on == self.vim.is_some() {
            return;
        }
        self.grouped = false;
        self.vim = on.then(|| Box::new(vim::Vim::default()));
        self.anchor = self.cursor;
        if on {
            self.cursor = vim::normal_clamp(&self.lines, self.cursor);
            self.anchor = self.cursor;
        }
    }

    /// Vim mode, pending keys, command line and messages, when Vim is on.
    pub fn vim(&self) -> Option<VimStatus> {
        self.vim.as_ref().map(|v| v.status())
    }

    /// Commands such as `:w` and `:q` that the application should carry out.
    pub fn take_vim_requests(&mut self) -> Vec<VimRequest> {
        self.vim.as_mut().map(|v| v.take_requests()).unwrap_or_default()
    }

    /// The range to highlight as selected. The flag is true for whole-line
    /// selections (Vim's Visual Line mode).
    pub fn display_selection(&self) -> Option<(Pos, Pos, bool)> {
        match &self.vim {
            Some(v) => v.display_selection(self),
            None => self.selection().map(|(a, b)| (a, b, false)),
        }
    }

    /// Vim state the editor widget needs: block selections, clipboard
    /// traffic, scroll requests and the last known viewport.
    pub fn vim_view(&self) -> Option<VimView> {
        self.vim.as_ref().map(|v| v.view(self))
    }

    /// Whether the caret should be drawn as a block (Vim Normal and Visual modes).
    pub fn block_caret(&self) -> bool {
        self.vim.as_ref().is_some_and(|v| v.block_caret())
    }

    /// The whole text, lines joined with `\n`.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn cursor(&self) -> Pos {
        self.cursor
    }

    pub fn anchor(&self) -> Pos {
        self.anchor
    }

    /// Increases with every change to the text, including undo and redo.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The selected range, start first, or `None` when nothing is selected.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        (self.cursor != self.anchor).then(|| (self.cursor.min(self.anchor), self.cursor.max(self.anchor)))
    }

    /// The text shown as selected. In Vim's Visual modes this includes the
    /// character under the cursor, or whole lines.
    pub fn selected_text(&self) -> String {
        if let Some(b) = self.vim_view().and_then(|v| v.block) {
            return b.text(&self.lines);
        }
        match self.display_selection() {
            Some((a, b, true)) => self.lines[a.line..=b.line].join("\n") + "\n",
            Some((a, b, false)) => text_between(&self.lines, a, b),
            None => String::new(),
        }
    }

    /// Applies an action. Returns true when the text changed.
    pub fn apply(&mut self, action: Action) -> bool {
        if let Some(mut vim) = self.vim.take() {
            let before = self.revision;
            let handled = vim.intercept(self, &action);
            self.vim = Some(vim);
            if handled {
                return self.revision != before;
            }
        }
        self.apply_plain(action)
    }

    /// Applies an action without Vim's interpretation.
    fn apply_plain(&mut self, action: Action) -> bool {
        match action {
            Action::Key(_) | Action::Clipboard(_) | Action::Viewport { .. } => false,
            Action::Insert(t) => {
                let t = normalize(&t);
                let typing = t.chars().count() == 1 && !t.chars().any(char::is_whitespace);
                self.begin(if typing { EditKind::Typing } else { EditKind::Other });
                self.insert(&t);
                true
            }
            Action::Enter => {
                self.begin(EditKind::Other);
                self.delete_selection();
                let line = &self.lines[self.cursor.line];
                let before = &line[..self.cursor.col];
                let after = &line[self.cursor.col..];
                let base: String = before.chars().take_while(|c| *c == ' ').collect();
                let opens = before.trim_end().ends_with(['{', '(', '[']);
                let closes = after.trim_start().starts_with(['}', ')', ']']);
                let inner = if opens { format!("{base}{}", " ".repeat(INDENT)) } else { base.clone() };
                if opens && closes {
                    // Put the closing bracket on its own line.
                    self.insert(&format!("\n{inner}\n{base}"));
                    let target = Pos::new(self.cursor.line - 1, inner.len());
                    self.set_cursor(target, false);
                } else {
                    self.insert(&format!("\n{inner}"));
                }
                true
            }
            Action::Backspace => {
                if self.selection().is_some() {
                    self.begin(EditKind::Other);
                    self.delete_selection();
                    return true;
                }
                let Pos { line, col } = self.cursor;
                if col == 0 && line == 0 {
                    return false;
                }
                self.begin(EditKind::Other);
                if col == 0 {
                    let cur = self.lines.remove(line);
                    let prev = &mut self.lines[line - 1];
                    let at = prev.len();
                    prev.push_str(&cur);
                    self.set_cursor(Pos::new(line - 1, at), false);
                } else {
                    let before = &self.lines[line][..col];
                    // In leading indentation, remove back to the previous indent stop.
                    let from = if before.chars().all(|c| c == ' ') {
                        let stop = (col - 1) / INDENT * INDENT;
                        stop.min(col - 1)
                    } else {
                        prev_boundary(&self.lines[line], col)
                    };
                    self.lines[line].replace_range(from..col, "");
                    self.set_cursor(Pos::new(line, from), false);
                }
                true
            }
            Action::Delete => {
                if self.selection().is_some() {
                    self.begin(EditKind::Other);
                    self.delete_selection();
                    return true;
                }
                let Pos { line, col } = self.cursor;
                if col >= self.lines[line].len() {
                    if line + 1 >= self.lines.len() {
                        return false;
                    }
                    self.begin(EditKind::Other);
                    let next = self.lines.remove(line + 1);
                    self.lines[line].push_str(&next);
                } else {
                    self.begin(EditKind::Other);
                    let to = next_boundary(&self.lines[line], col);
                    self.lines[line].replace_range(col..to, "");
                }
                true
            }
            Action::Indent => {
                self.begin(EditKind::Other);
                match self.selection() {
                    Some((a, b)) if a.line != b.line => {
                        let pad = " ".repeat(INDENT);
                        for l in a.line..=b.line {
                            if !(l == b.line && b.col == 0) {
                                self.lines[l].insert_str(0, &pad);
                            }
                        }
                        let shift = |p: Pos| if p.line == b.line && b.col == 0 { p } else { Pos::new(p.line, p.col + INDENT) };
                        self.cursor = shift(self.cursor);
                        self.anchor = shift(self.anchor);
                    }
                    _ => {
                        let chars = self.lines[self.cursor.line][..self.cursor.col].chars().count();
                        let n = INDENT - chars % INDENT;
                        self.insert(&" ".repeat(n));
                    }
                }
                true
            }
            Action::Outdent => {
                let (a, b) = self.selection().unwrap_or((self.cursor, self.cursor));
                let last = if b.line > a.line && b.col == 0 { b.line - 1 } else { b.line };
                let removable: Vec<usize> = (a.line..=last).map(|l| self.lines[l].chars().take(INDENT).take_while(|c| *c == ' ').count()).collect();
                if removable.iter().all(|n| *n == 0) {
                    return false;
                }
                self.begin(EditKind::Other);
                for (i, l) in (a.line..=last).enumerate() {
                    self.lines[l].replace_range(..removable[i], "");
                }
                let fix = |p: Pos| {
                    if p.line >= a.line && p.line <= last { Pos::new(p.line, p.col.saturating_sub(removable[p.line - a.line])) } else { p }
                };
                self.cursor = fix(self.cursor);
                self.anchor = fix(self.anchor);
                true
            }
            Action::Undo => self.step(true),
            Action::Redo => self.step(false),
            Action::Move { motion, select } => {
                self.last_edit = EditKind::None;
                let target = match (motion, self.selection(), select) {
                    (Motion::Left, Some((a, _)), false) => a,
                    (Motion::Right, Some((_, b)), false) => b,
                    _ => self.moved(motion),
                };
                let keep_goal = matches!(motion, Motion::Up | Motion::Down | Motion::PageUp(_) | Motion::PageDown(_));
                if !keep_goal {
                    self.goal = None;
                }
                self.set_cursor(target, select);
                false
            }
            Action::Click { pos, select } => {
                self.last_edit = EditKind::None;
                self.goal = None;
                let p = self.clamp(pos);
                self.set_cursor(p, select);
                false
            }
            Action::Drag(pos) => {
                self.goal = None;
                self.cursor = self.clamp(pos);
                false
            }
            Action::SelectWord(pos) => {
                let p = self.clamp(pos);
                let line = &self.lines[p.line];
                let at = line[p.col..].chars().next().or_else(|| line[..p.col].chars().last());
                let Some(c) = at else { return false };
                let k = class(c);
                let mut start = p.col;
                while let Some(ch) = line[..start].chars().last() {
                    if class(ch) != k {
                        break;
                    }
                    start -= ch.len_utf8();
                }
                let mut end = p.col;
                while let Some(ch) = line[end..].chars().next() {
                    if class(ch) != k {
                        break;
                    }
                    end += ch.len_utf8();
                }
                self.anchor = Pos::new(p.line, start);
                self.cursor = Pos::new(p.line, end);
                false
            }
            Action::SelectAll => {
                self.anchor = Pos::default();
                let last = self.lines.len() - 1;
                self.cursor = Pos::new(last, self.lines[last].len());
                false
            }
            Action::Collapse => {
                self.anchor = self.cursor;
                false
            }
        }
    }

    fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() - 1);
        let text = &self.lines[line];
        let mut col = p.col.min(text.len());
        while !text.is_char_boundary(col) {
            col -= 1;
        }
        Pos::new(line, col)
    }

    fn set_cursor(&mut self, p: Pos, select: bool) {
        self.cursor = p;
        if !select {
            self.anchor = p;
        }
    }

    fn begin(&mut self, kind: EditKind) {
        if self.grouped {
            self.redo.clear();
            self.last_edit = kind;
            self.goal = None;
            self.revision += 1;
            return;
        }
        let coalesce = kind == EditKind::Typing && self.last_edit == EditKind::Typing && self.selection().is_none();
        if !coalesce {
            self.undo.push(Snapshot { lines: self.lines.clone(), cursor: self.cursor, anchor: self.anchor });
            if self.undo.len() > 500 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_edit = kind;
        self.goal = None;
        self.revision += 1;
    }

    fn step(&mut self, undo: bool) -> bool {
        let (from, to) = if undo { (&mut self.undo, &mut self.redo) } else { (&mut self.redo, &mut self.undo) };
        let Some(snap) = from.pop() else { return false };
        to.push(Snapshot { lines: std::mem::take(&mut self.lines), cursor: self.cursor, anchor: self.anchor });
        self.lines = snap.lines;
        self.cursor = snap.cursor;
        self.anchor = snap.anchor;
        self.last_edit = EditKind::None;
        self.revision += 1;
        true
    }

    fn delete_selection(&mut self) {
        let Some((a, b)) = self.selection() else { return };
        let tail = self.lines[b.line][b.col..].to_owned();
        self.lines[a.line].truncate(a.col);
        self.lines[a.line].push_str(&tail);
        self.lines.drain(a.line + 1..=b.line);
        self.set_cursor(a, false);
    }

    fn insert(&mut self, text: &str) {
        self.delete_selection();
        let Pos { line, col } = self.cursor;
        let tail = self.lines[line].split_off(col);
        let mut parts = text.split('\n');
        self.lines[line].push_str(parts.next().unwrap_or(""));
        let mut at = line;
        for part in parts {
            at += 1;
            self.lines.insert(at, part.to_owned());
        }
        let end_col = self.lines[at].len();
        self.lines[at].push_str(&tail);
        self.set_cursor(Pos::new(at, end_col), false);
    }

    fn moved(&mut self, motion: Motion) -> Pos {
        let Pos { line, col } = self.cursor;
        let text = &self.lines[line];
        let last = self.lines.len() - 1;
        let vertical = |doc: &mut Document, target: usize| {
            let goal = *doc.goal.get_or_insert_with(|| doc.lines[line][..col].chars().count());
            let t = &doc.lines[target];
            Pos::new(target, t.char_indices().nth(goal).map_or(t.len(), |(i, _)| i))
        };
        match motion {
            Motion::Left if col > 0 => Pos::new(line, prev_boundary(text, col)),
            Motion::Left if line > 0 => Pos::new(line - 1, self.lines[line - 1].len()),
            Motion::Left => self.cursor,
            Motion::Right if col < text.len() => Pos::new(line, next_boundary(text, col)),
            Motion::Right if line < last => Pos::new(line + 1, 0),
            Motion::Right => self.cursor,
            Motion::Up if line > 0 => vertical(self, line - 1),
            Motion::Up => Pos::new(0, 0),
            Motion::Down if line < last => vertical(self, line + 1),
            Motion::Down => Pos::new(last, self.lines[last].len()),
            Motion::PageUp(n) => vertical(self, line.saturating_sub(n.max(1))),
            Motion::PageDown(n) => vertical(self, (line + n.max(1)).min(last)),
            Motion::LineStart => {
                let first = text.len() - text.trim_start().len();
                Pos::new(line, if col == first { 0 } else { first })
            }
            Motion::LineEnd => Pos::new(line, text.len()),
            Motion::DocStart => Pos::new(0, 0),
            Motion::DocEnd => Pos::new(last, self.lines[last].len()),
            Motion::WordLeft => {
                if col == 0 {
                    return if line > 0 { Pos::new(line - 1, self.lines[line - 1].len()) } else { self.cursor };
                }
                let mut i = col;
                while let Some(c) = text[..i].chars().last().filter(|c| class(*c) == Class::Space) {
                    i -= c.len_utf8();
                }
                if let Some(k) = text[..i].chars().last().map(class) {
                    while let Some(c) = text[..i].chars().last().filter(|c| class(*c) == k) {
                        i -= c.len_utf8();
                    }
                }
                Pos::new(line, i)
            }
            Motion::WordRight => {
                if col >= text.len() {
                    return if line < last { Pos::new(line + 1, 0) } else { self.cursor };
                }
                let mut i = col;
                while let Some(c) = text[i..].chars().next().filter(|c| class(*c) == Class::Space) {
                    i += c.len_utf8();
                }
                if let Some(k) = text[i..].chars().next().map(class) {
                    while let Some(c) = text[i..].chars().next().filter(|c| class(*c) == k) {
                        i += c.len_utf8();
                    }
                }
                Pos::new(line, i)
            }
        }
    }
}

/// The text from `a` up to `b`, with lines joined by newlines.
pub fn text_between(lines: &[String], a: Pos, b: Pos) -> String {
    if a.line == b.line {
        return lines[a.line][a.col..b.col].to_owned();
    }
    let mut out = lines[a.line][a.col..].to_owned();
    for l in &lines[a.line + 1..b.line] {
        out.push('\n');
        out.push_str(l);
    }
    out.push('\n');
    out.push_str(&lines[b.line][..b.col]);
    out
}

fn prev_boundary(s: &str, i: usize) -> usize {
    s[..i].char_indices().last().map_or(0, |(j, _)| j)
}

fn next_boundary(s: &str, i: usize) -> usize {
    s[i..].chars().next().map_or(i, |c| i + c.len_utf8())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(d: &mut Document, line: usize, col: usize) {
        d.apply(Action::Click { pos: Pos::new(line, col), select: false });
    }

    fn typing(d: &mut Document, s: &str) {
        for c in s.chars() {
            d.apply(Action::Insert(c.to_string()));
        }
    }

    #[test]
    fn typing_and_undo_groups_words() {
        let mut d = Document::new("");
        typing(&mut d, "let x");
        assert_eq!(d.text(), "let x");
        d.apply(Action::Undo);
        assert_eq!(d.text(), "let ");
        d.apply(Action::Undo);
        assert_eq!(d.text(), "let");
        d.apply(Action::Undo);
        assert_eq!(d.text(), "");
        d.apply(Action::Redo);
        assert_eq!(d.text(), "let");
    }

    #[test]
    fn enter_keeps_and_adds_indentation() {
        let mut d = Document::new("fn main() {}");
        at(&mut d, 0, 11);
        d.apply(Action::Enter);
        assert_eq!(d.text(), "fn main() {\n    \n}");
        assert_eq!(d.cursor(), Pos::new(1, 4));
        typing(&mut d, "x;");
        d.apply(Action::Enter);
        assert_eq!(d.lines()[2], "    ");
    }

    #[test]
    fn backspace_joins_lines_and_removes_indent_stops() {
        let mut d = Document::new("a\n        b");
        at(&mut d, 1, 8);
        d.apply(Action::Backspace);
        assert_eq!(d.lines()[1], "    b");
        at(&mut d, 1, 0);
        d.apply(Action::Backspace);
        assert_eq!(d.text(), "a    b");
        assert_eq!(d.cursor(), Pos::new(0, 1));
    }

    #[test]
    fn selection_replace_and_copy() {
        let mut d = Document::new("hello brave\nnew world");
        at(&mut d, 0, 6);
        d.apply(Action::Click { pos: Pos::new(1, 3), select: true });
        assert_eq!(d.selected_text(), "brave\nnew");
        d.apply(Action::Insert("big".into()));
        assert_eq!(d.text(), "hello big world");
    }

    #[test]
    fn word_motions_and_select_word() {
        let mut d = Document::new("let value_2 = foo(bar);");
        at(&mut d, 0, 0);
        d.apply(Action::Move { motion: Motion::WordRight, select: false });
        assert_eq!(d.cursor().col, 3);
        d.apply(Action::Move { motion: Motion::WordRight, select: false });
        assert_eq!(d.cursor().col, 11);
        d.apply(Action::Move { motion: Motion::WordLeft, select: false });
        assert_eq!(d.cursor().col, 4);
        d.apply(Action::SelectWord(Pos::new(0, 15)));
        assert_eq!(d.selected_text(), "foo");
    }

    #[test]
    fn indent_and_outdent_lines() {
        let mut d = Document::new("a\nb\nc");
        at(&mut d, 0, 0);
        d.apply(Action::Click { pos: Pos::new(1, 1), select: true });
        d.apply(Action::Indent);
        assert_eq!(d.text(), "    a\n    b\nc");
        d.apply(Action::Outdent);
        assert_eq!(d.text(), "a\nb\nc");
    }

    #[test]
    fn vertical_motion_keeps_goal_column() {
        let mut d = Document::new("abcdef\nab\nabcdef");
        at(&mut d, 0, 5);
        d.apply(Action::Move { motion: Motion::Down, select: false });
        assert_eq!(d.cursor(), Pos::new(1, 2));
        d.apply(Action::Move { motion: Motion::Down, select: false });
        assert_eq!(d.cursor(), Pos::new(2, 5));
    }

    #[test]
    fn tabs_and_crlf_are_normalized() {
        let d = Document::new("a\r\n\tb");
        assert_eq!(d.lines(), &["a".to_string(), "    b".to_string()]);
    }

    #[test]
    fn multibyte_characters_are_safe() {
        let mut d = Document::new("héllo");
        at(&mut d, 0, 3);
        d.apply(Action::Backspace);
        assert_eq!(d.text(), "hllo");
        d.apply(Action::Click { pos: Pos::new(0, 99), select: false });
        assert_eq!(d.cursor(), Pos::new(0, 4));
    }
}
