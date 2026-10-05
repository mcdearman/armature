//! Vim-style modal editing for [`Document`].
//!
//! Modes: Normal, Insert, Visual, Visual Line and Visual Block. Counts,
//! registers (named, numbered, clipboard), the `d c y > < gu gU g~`
//! operators with motions and text objects, macros, marks, `.` repeat,
//! regex search, and Ex commands including `:s`. Keys arrive as
//! [`Action::Key`] and are parsed here.

use std::collections::HashMap;

use regex::{Regex, RegexBuilder};

use super::{Action, Document, EditKind, INDENT, Motion as DocMotion, Pos, next_boundary, normalize, prev_boundary, text_between};
use crate::event::{Key, KeyEvent};

/// The current Vim mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
    VisualBlock,
}

impl Mode {
    /// The label Vim shows for this mode.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
            Mode::VisualBlock => "V-BLOCK",
        }
    }

    pub fn is_visual(self) -> bool {
        matches!(self, Mode::Visual | Mode::VisualLine | Mode::VisualBlock)
    }
}

/// Ex commands the application should carry out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VimRequest {
    /// `:w`
    Write,
    /// `:q`, or `:q!` / `ZQ` with `force` to discard unsaved changes.
    Quit { force: bool },
    /// `:wq`, `:x`, `ZZ`
    WriteQuit,
}

/// What to show in a status bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VimStatus {
    pub mode: Mode,
    /// Keys typed so far for an unfinished command, such as `"a2d`.
    pub pending: String,
    /// The command line while typing `:`, `/` or `?`, including the prefix.
    pub command_line: Option<String>,
    /// The latest message, such as "Pattern not found: foo".
    pub message: Option<String>,
    /// The register a macro is being recorded into.
    pub recording: Option<char>,
}

/// A scroll the editor should perform (`zz`, `Ctrl-e`, `Ctrl-d`...).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scroll {
    /// Put the cursor line in the middle of the view.
    Center,
    Top,
    Bottom,
    /// Scroll by this many lines (positive moves the text up).
    Lines(isize),
}

/// When the editor should read the system clipboard before sending a key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClipboardNeed {
    #[default]
    None,
    /// Before the next key, whatever it is (`"+` is pending).
    Always,
    /// Before `p` or `P` (`clipboard=unnamedplus`).
    OnPaste,
}

/// A Visual Block selection in character columns (inclusive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockSelection {
    pub first_line: usize,
    pub last_line: usize,
    pub first_col: usize,
    pub last_col: usize,
    /// `$` was pressed: every line is selected to its end.
    pub to_end: bool,
}

impl BlockSelection {
    /// Byte range of the block on `line`, or `None` if the line is too short.
    pub fn bytes(&self, line: &str) -> Option<(usize, usize)> {
        let chars = line.chars().count();
        if chars <= self.first_col {
            return None;
        }
        let a = byte_at(line, self.first_col);
        let b = if self.to_end { line.len() } else { byte_at(line, self.last_col + 1) };
        Some((a, b))
    }

    /// The selected text, one line per row.
    pub fn text(&self, lines: &[String]) -> String {
        (self.first_line..=self.last_line)
            .map(|l| self.bytes(&lines[l]).map_or(String::new(), |(a, b)| lines[l][a..b].to_owned()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Vim state the editor widget needs to draw and to talk to the system.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VimView {
    pub block: Option<BlockSelection>,
    /// Text to put on the system clipboard, with a serial number so each
    /// copy happens once.
    pub clipboard: Option<(u64, String)>,
    pub clipboard_need: ClipboardNeed,
    /// A scroll to perform, with a serial number.
    pub scroll: Option<(u64, Scroll)>,
    /// The viewport Vim last heard about: first line and line count.
    pub viewport: (usize, usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VKey {
    Char(char),
    Ctrl(char),
    Esc,
    Enter,
    Backspace,
    Delete,
    Tab,
    BackTab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

const SPECIAL: [VKey; 9] = [VKey::BackTab, VKey::Left, VKey::Right, VKey::Up, VKey::Down, VKey::Home, VKey::End, VKey::PageUp, VKey::PageDown];

impl VKey {
    fn from_event(k: &KeyEvent) -> Option<VKey> {
        let m = k.modifiers;
        Some(match &k.key {
            Key::Escape => VKey::Esc,
            Key::Enter => VKey::Enter,
            Key::Backspace => VKey::Backspace,
            Key::Delete => VKey::Delete,
            Key::Tab if m.shift => VKey::BackTab,
            Key::Tab => VKey::Tab,
            Key::Left => VKey::Left,
            Key::Right => VKey::Right,
            Key::Up => VKey::Up,
            Key::Down => VKey::Down,
            Key::Home => VKey::Home,
            Key::End => VKey::End,
            Key::PageUp => VKey::PageUp,
            Key::PageDown => VKey::PageDown,
            Key::Space => VKey::Char(' '),
            Key::Character(c) if m.ctrl => {
                let c = c.chars().next()?.to_ascii_lowercase();
                if c == '[' || c == 'c' { VKey::Esc } else { VKey::Ctrl(c) }
            }
            Key::Character(c) => VKey::Char(k.text.as_deref().and_then(|t| t.chars().next()).or_else(|| c.chars().next())?),
            Key::Other => VKey::Char(k.text.as_deref()?.chars().next()?),
        })
    }

    /// Registers store macros as text, the way Vim does: control keys
    /// become control characters, other special keys private-use characters.
    fn to_char(self) -> char {
        match self {
            VKey::Char(c) => c,
            VKey::Ctrl(c) if c.is_ascii_lowercase() => ((c as u8) & 0x1f) as char,
            VKey::Ctrl(c) => c,
            VKey::Esc => '\x1b',
            VKey::Enter => '\r',
            VKey::Backspace => '\x08',
            VKey::Tab => '\t',
            VKey::Delete => '\x7f',
            k => char::from_u32(0xE000 + SPECIAL.iter().position(|s| *s == k).unwrap_or(0) as u32).unwrap_or(' '),
        }
    }

    fn from_char(c: char) -> VKey {
        match c {
            '\x1b' => VKey::Esc,
            '\r' | '\n' => VKey::Enter,
            '\x08' => VKey::Backspace,
            '\t' => VKey::Tab,
            '\x7f' => VKey::Delete,
            c if (c as u32) < 0x20 => VKey::Ctrl(((c as u8) | 0x60) as char),
            c if (0xE000..0xE000 + SPECIAL.len() as u32).contains(&(c as u32)) => SPECIAL[(c as u32 - 0xE000) as usize],
            c => VKey::Char(c),
        }
    }

    fn describe(self) -> String {
        match self {
            VKey::Char(c) => c.to_string(),
            VKey::Ctrl(c) => format!("^{}", c.to_ascii_uppercase()),
            _ => String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Exclusive,
    Inclusive,
    Linewise,
}

enum Parsed {
    More,
    Fail,
    Done,
}

enum Target {
    More,
    Fail,
    To(Pos, Kind),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum RegKind {
    #[default]
    Char,
    Line,
    Block,
}

#[derive(Clone, Debug, Default)]
struct Register {
    text: String,
    kind: RegKind,
}

#[derive(Clone, Copy, Debug)]
struct VisualSize {
    mode: Mode,
    lines: usize,
    cols: usize,
    dollar: bool,
}

#[derive(Clone, Copy, Debug)]
struct BlockInsert {
    first_line: usize,
    last_line: usize,
    col: usize,
    append: bool,
    to_end: bool,
}

#[derive(Clone, Default)]
struct InsertState {
    count: usize,
    keys: Vec<VKey>,
    /// Started with `o` (true) or `O` (false); a count repeats the line.
    open_line: Option<bool>,
    block: Option<BlockInsert>,
    register_pending: bool,
}

#[derive(Clone, Default)]
pub(crate) struct Vim {
    mode: Mode,
    keys: Vec<VKey>,
    registers: HashMap<char, Register>,
    last_change: Vec<VKey>,
    last_visual: Option<VisualSize>,
    recording: Option<Vec<VKey>>,
    dot_depth: usize,
    macro_depth: usize,
    aborted: bool,
    changed: bool,
    find: Option<(char, char)>,
    search: Option<(String, bool)>,
    ignorecase: bool,
    smartcase: bool,
    unnamedplus: bool,
    cmdline: Option<(char, String)>,
    last_ex: Option<String>,
    message: Option<String>,
    requests: Vec<VimRequest>,
    marks: HashMap<char, Pos>,
    macro_rec: Option<(char, Vec<VKey>)>,
    last_macro: Option<char>,
    insert: InsertState,
    block_anchor_col: usize,
    block_dollar: bool,
    last_visual_sel: Option<(Mode, Pos, Pos)>,
    clipboard_out: Option<(u64, String)>,
    scroll: Option<(u64, Scroll)>,
    serial: u64,
    view_top: usize,
    view_lines: usize,
}

// ---------------------------------------------------------------------------
// Text helpers. A position with `col == line.len()` stands for the newline.

fn ch(lines: &[String], p: Pos) -> char {
    lines[p.line][p.col..].chars().next().unwrap_or('\n')
}

fn next(lines: &[String], p: Pos) -> Option<Pos> {
    if p.col < lines[p.line].len() {
        Some(Pos::new(p.line, next_boundary(&lines[p.line], p.col)))
    } else if p.line + 1 < lines.len() {
        Some(Pos::new(p.line + 1, 0))
    } else {
        None
    }
}

fn prev(lines: &[String], p: Pos) -> Option<Pos> {
    if p.col > 0 {
        Some(Pos::new(p.line, prev_boundary(&lines[p.line], p.col)))
    } else if p.line > 0 {
        Some(Pos::new(p.line - 1, lines[p.line - 1].len()))
    } else {
        None
    }
}

fn last_col(line: &str) -> usize {
    if line.is_empty() { 0 } else { prev_boundary(line, line.len()) }
}

fn first_non_blank(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Byte offset of character column `col`, or the line length past the end.
fn byte_at(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
}

fn char_col(line: &str, byte: usize) -> usize {
    line[..byte.min(line.len())].chars().count()
}

/// Normal mode keeps the cursor on a character, never past the last one.
pub(crate) fn normal_clamp(lines: &[String], p: Pos) -> Pos {
    let line = p.line.min(lines.len() - 1);
    let text = &lines[line];
    let mut col = p.col.min(last_col(text));
    while !text.is_char_boundary(col) {
        col -= 1;
    }
    Pos::new(line, col)
}

fn class(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        0
    } else if big || c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

fn is_empty_line(lines: &[String], p: Pos) -> bool {
    p.col == 0 && lines[p.line].is_empty()
}

fn word_forward(lines: &[String], p: Pos, big: bool) -> Pos {
    let start = class(ch(lines, p), big);
    let mut q = p;
    if start != 0 {
        loop {
            match next(lines, q) {
                Some(n) => {
                    q = n;
                    if class(ch(lines, q), big) != start {
                        break;
                    }
                }
                None => return Pos::new(q.line, lines[q.line].len()),
            }
        }
    }
    loop {
        if q != p && is_empty_line(lines, q) {
            return q;
        }
        if class(ch(lines, q), big) != 0 {
            return q;
        }
        match next(lines, q) {
            Some(n) => q = n,
            None => return Pos::new(q.line, lines[q.line].len()),
        }
    }
}

fn word_end(lines: &[String], p: Pos, big: bool) -> Pos {
    let Some(mut q) = next(lines, p) else { return p };
    while class(ch(lines, q), big) == 0 {
        match next(lines, q) {
            Some(n) => q = n,
            None => return p,
        }
    }
    let k = class(ch(lines, q), big);
    while let Some(n) = next(lines, q) {
        if class(ch(lines, n), big) != k {
            break;
        }
        q = n;
    }
    q
}

fn word_back(lines: &[String], p: Pos, big: bool) -> Pos {
    let Some(mut q) = prev(lines, p) else { return p };
    while class(ch(lines, q), big) == 0 && !is_empty_line(lines, q) {
        match prev(lines, q) {
            Some(n) => q = n,
            None => return q,
        }
    }
    if is_empty_line(lines, q) {
        return q;
    }
    let k = class(ch(lines, q), big);
    while let Some(n) = prev(lines, q) {
        if n.line != q.line || class(ch(lines, n), big) != k {
            break;
        }
        q = n;
    }
    q
}

const PAIRS: [(char, char); 4] = [('(', ')'), ('[', ']'), ('{', '}'), ('<', '>')];

fn match_bracket(lines: &[String], p: Pos) -> Option<Pos> {
    let c = ch(lines, p);
    let (open, close, forward) = PAIRS.iter().find_map(|&(o, cl)| {
        if c == o {
            Some((o, cl, true))
        } else if c == cl {
            Some((o, cl, false))
        } else {
            None
        }
    })?;
    let mut depth = 0i32;
    let mut q = p;
    loop {
        let x = ch(lines, q);
        if x == open {
            depth += if forward { 1 } else { -1 };
        } else if x == close {
            depth += if forward { -1 } else { 1 };
        }
        if depth == 0 {
            return Some(q);
        }
        q = if forward { next(lines, q)? } else { prev(lines, q)? };
    }
}

/// The innermost `open`/`close` pair around `p`, as the positions of the brackets.
fn enclosing(lines: &[String], p: Pos, open: char, close: char) -> Option<(Pos, Pos)> {
    let mut depth = 0;
    let mut q = p;
    let start = loop {
        let c = ch(lines, q);
        if c == open {
            if depth == 0 {
                break q;
            }
            depth -= 1;
        } else if c == close && q != p {
            depth += 1;
        }
        q = prev(lines, q)?;
    };
    let end = match_bracket(lines, start)?;
    (end >= p).then_some((start, end))
}

fn count(ks: &[VKey], i: &mut usize) -> Option<usize> {
    let mut n: Option<usize> = None;
    while let Some(VKey::Char(c)) = ks.get(*i) {
        match c.to_digit(10) {
            Some(d) if d > 0 || n.is_some() => {
                n = Some(n.unwrap_or(0).saturating_mul(10).saturating_add(d as usize).min(100_000));
                *i += 1;
            }
            _ => break,
        }
    }
    n
}

fn swap_case(c: char) -> String {
    if c.is_lowercase() { c.to_uppercase().collect() } else { c.to_lowercase().collect() }
}

fn case_op(op: char) -> fn(char) -> String {
    match op {
        'u' => |c| c.to_lowercase().collect(),
        'U' => |c| c.to_uppercase().collect(),
        _ => swap_case,
    }
}

fn valid_register(c: char) -> bool {
    c.is_ascii_alphanumeric() || "\"-_+*/.:".contains(c)
}

/// `f t F T` within a line. With `repeat`, `t`/`T` skip an adjacent match
/// so `;` keeps moving.
fn find_char(line: &str, col: usize, f: char, c: char, n: usize, repeat: bool) -> Option<usize> {
    let mut at = col;
    for k in 0..n {
        match f {
            'f' | 't' => {
                let mut from = next_boundary(line, at);
                if f == 't' && (k > 0 || repeat) {
                    from = next_boundary(line, from);
                }
                let off = line.get(from..)?.find(c)?;
                at = from + off;
            }
            _ => {
                let mut upto = at;
                if f == 'T' && (k > 0 || repeat) {
                    upto = prev_boundary(line, upto);
                }
                at = line.get(..upto)?.rfind(c)?;
            }
        }
    }
    Some(match f {
        't' => prev_boundary(line, at),
        'T' => next_boundary(line, at),
        _ => at,
    })
}

/// The range (start inclusive, end exclusive) of text object `obj` around
/// `p`, and whether it covers whole lines.
fn text_object(lines: &[String], p: Pos, around: bool, obj: char) -> Option<(Pos, Pos, bool)> {
    match obj {
        'w' | 'W' => {
            let big = obj == 'W';
            let line = &lines[p.line];
            if line.is_empty() {
                return Some((p, p, false));
            }
            let k = class(ch(lines, p), big);
            let mut a = p.col;
            while let Some(c) = line[..a].chars().last() {
                if class(c, big) != k {
                    break;
                }
                a -= c.len_utf8();
            }
            let mut b = p.col;
            while let Some(c) = line[b..].chars().next() {
                if class(c, big) != k {
                    break;
                }
                b += c.len_utf8();
            }
            if around {
                let trailing = line[b..].len() - line[b..].trim_start().len();
                if trailing > 0 && k != 0 {
                    b += trailing;
                } else {
                    let leading = line[..a].len() - line[..a].trim_end().len();
                    a -= leading;
                }
            }
            Some((Pos::new(p.line, a), Pos::new(p.line, b), false))
        }
        '"' | '\'' | '`' => {
            let line = &lines[p.line];
            let quotes: Vec<usize> = line.char_indices().filter(|(i, c)| *c == obj && (*i == 0 || !line[..*i].ends_with('\\'))).map(|(i, _)| i).collect();
            let pair = quotes.chunks_exact(2).find(|q| q[0] <= p.col && p.col <= q[1]).or_else(|| quotes.chunks_exact(2).find(|q| q[0] > p.col))?;
            let (open, close) = (pair[0], pair[1]);
            Some(if around {
                (Pos::new(p.line, open), Pos::new(p.line, close + 1), false)
            } else {
                (Pos::new(p.line, open + 1), Pos::new(p.line, close), false)
            })
        }
        _ => {
            let (open, close) = match obj {
                '(' | ')' | 'b' => ('(', ')'),
                '[' | ']' => ('[', ']'),
                '{' | '}' | 'B' => ('{', '}'),
                '<' | '>' => ('<', '>'),
                _ => return None,
            };
            let (s, e) = enclosing(lines, p, open, close)?;
            if around {
                Some((s, next(lines, e).unwrap_or(e), false))
            } else {
                let a = next(lines, s)?;
                // `i{` on a block covers the lines between `{` and `}`.
                if a.col == lines[a.line].len() && a.line < e.line && lines[e.line][..e.col].trim().is_empty() {
                    if e.line == a.line + 1 {
                        return Some((Pos::new(e.line, 0), Pos::new(e.line, 0), false));
                    }
                    return Some((Pos::new(a.line + 1, 0), Pos::new(e.line - 1, lines[e.line - 1].len()), true));
                }
                Some((a, e, false))
            }
        }
    }
}

/// Converts Vim pattern extras to Rust regex syntax: `\<` and `\>` become
/// word boundaries, `\c` and `\C` force case (in)sensitivity.
fn compile(pattern: &str, ignorecase: bool, smartcase: bool) -> Result<Regex, String> {
    let mut ci = ignorecase && !(smartcase && pattern.chars().any(char::is_uppercase));
    let mut out = String::new();
    let mut it = pattern.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('<' | '>') => out.push_str(r"\b"),
            Some('c') => ci = true,
            Some('C') => ci = false,
            Some(n) => {
                out.push('\\');
                out.push(n);
            }
            None => out.push_str(r"\\"),
        }
    }
    RegexBuilder::new(&out).case_insensitive(ci).build().map_err(|_| format!("Invalid pattern: {pattern}"))
}

/// Converts a Vim replacement (`&`, `\1`, `\n`) to a regex template.
fn replacement(rep: &str) -> String {
    let mut out = String::new();
    let mut it = rep.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                Some(d @ '0'..='9') => out.push_str(&format!("${{{d}}}")),
                Some('n' | 'r') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('$') => out.push_str("$$"),
                Some(o) => out.push(o),
                None => out.push('\\'),
            },
            '&' => out.push_str("${0}"),
            '$' => out.push_str("$$"),
            o => out.push(o),
        }
    }
    out
}

/// Splits `s` on unescaped `delim`, unescaping `\delim`.
fn split_delim(s: &str, delim: char) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' && it.peek() == Some(&delim) {
            parts.last_mut().unwrap().push(delim);
            it.next();
        } else if c == delim {
            parts.push(String::new());
        } else {
            parts.last_mut().unwrap().push(c);
        }
    }
    parts
}

fn is_jump(k: VKey) -> bool {
    matches!(k, VKey::Char('G' | '%' | '{' | '}' | 'n' | 'N' | '*' | '#' | '`' | '\'' | 'H' | 'M' | 'L')) || matches!(k, VKey::Char('g'))
}

// ---------------------------------------------------------------------------

impl Vim {
    pub(crate) fn status(&self) -> VimStatus {
        VimStatus {
            mode: self.mode,
            pending: self.keys.iter().map(|k| k.describe()).collect(),
            command_line: self.cmdline.as_ref().map(|(p, s)| format!("{p}{s}")),
            message: self.message.clone(),
            recording: self.macro_rec.as_ref().map(|(r, _)| *r),
        }
    }

    pub(crate) fn view(&self, doc: &Document) -> VimView {
        VimView {
            block: self.block(doc),
            clipboard: self.clipboard_out.clone(),
            clipboard_need: self.clipboard_need(),
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

    fn clipboard_need(&self) -> ClipboardNeed {
        if self.cmdline.is_some() {
            return ClipboardNeed::None;
        }
        if self.mode == Mode::Insert {
            return if self.insert.register_pending { ClipboardNeed::Always } else { ClipboardNeed::None };
        }
        match self.keys.as_slice() {
            [VKey::Char('"'), VKey::Char('+' | '*'), ..] => ClipboardNeed::Always,
            [VKey::Char('@')] => ClipboardNeed::Always,
            _ if self.unnamedplus => ClipboardNeed::OnPaste,
            _ => ClipboardNeed::None,
        }
    }

    fn block(&self, doc: &Document) -> Option<BlockSelection> {
        if self.mode != Mode::VisualBlock {
            return None;
        }
        let cursor_col = doc.goal.unwrap_or_else(|| char_col(&doc.lines[doc.cursor.line], doc.cursor.col));
        Some(BlockSelection {
            first_line: doc.anchor.line.min(doc.cursor.line),
            last_line: doc.anchor.line.max(doc.cursor.line),
            first_col: self.block_anchor_col.min(cursor_col),
            last_col: self.block_anchor_col.max(cursor_col),
            to_end: self.block_dollar,
        })
    }

    fn visual_range(&self, doc: &Document) -> (Pos, Pos) {
        let (a, b) = (doc.anchor.min(doc.cursor), doc.anchor.max(doc.cursor));
        match self.mode {
            Mode::VisualLine => (Pos::new(a.line, 0), Pos::new(b.line, doc.lines[b.line].len())),
            _ => (a, Pos::new(b.line, next_boundary(&doc.lines[b.line], b.col))),
        }
    }

    pub(crate) fn display_selection(&self, doc: &Document) -> Option<(Pos, Pos, bool)> {
        match self.mode {
            Mode::Visual | Mode::VisualLine => {
                let (a, b) = self.visual_range(doc);
                Some((a, b, self.mode == Mode::VisualLine))
            }
            _ => None,
        }
    }

    fn visual_size(&self, doc: &Document) -> VisualSize {
        let (a, b) = (doc.anchor.min(doc.cursor), doc.anchor.max(doc.cursor));
        let lines = b.line - a.line;
        let cols = match self.mode {
            Mode::VisualBlock => self.block(doc).map_or(1, |bl| bl.last_col - bl.first_col + 1),
            Mode::Visual if lines == 0 => doc.lines[a.line][a.col..next_boundary(&doc.lines[a.line], b.col)].chars().count(),
            Mode::Visual => char_col(&doc.lines[b.line], b.col),
            _ => 0,
        };
        VisualSize { mode: self.mode, lines, cols, dollar: self.block_dollar }
    }

    /// Selects a region the size of `vs` starting at the cursor (for `.`).
    fn reselect(&mut self, doc: &mut Document, vs: VisualSize) {
        let cur = doc.cursor;
        let l1 = (cur.line + vs.lines).min(doc.lines.len() - 1);
        doc.anchor = cur;
        let line = &doc.lines[l1];
        match vs.mode {
            Mode::Visual => {
                let c = if vs.lines == 0 { char_col(line, cur.col) + vs.cols.saturating_sub(1) } else { vs.cols };
                doc.cursor = normal_clamp(&doc.lines, Pos::new(l1, byte_at(line, c)));
            }
            Mode::VisualLine => doc.cursor = Pos::new(l1, 0),
            _ => {
                let c0 = char_col(&doc.lines[cur.line], cur.col);
                self.block_anchor_col = c0;
                self.block_dollar = vs.dollar;
                doc.cursor = normal_clamp(&doc.lines, Pos::new(l1, byte_at(line, c0 + vs.cols - 1)));
                doc.goal = Some(c0 + vs.cols - 1);
            }
        }
        self.mode = vs.mode;
    }

    fn enter_visual(&mut self, doc: &mut Document, mode: Mode) {
        if !self.mode.is_visual() {
            doc.anchor = doc.cursor;
        }
        if mode == Mode::VisualBlock {
            self.block_anchor_col = char_col(&doc.lines[doc.anchor.line], doc.anchor.col);
            self.block_dollar = false;
        }
        self.mode = mode;
    }

    /// Leaves a Visual mode, remembering the selection for `gv`, `'<` and `'>`.
    fn exit_visual(&mut self, doc: &mut Document) {
        if !self.mode.is_visual() {
            return;
        }
        let (a, b) = (doc.anchor.min(doc.cursor), doc.anchor.max(doc.cursor));
        self.marks.insert('<', a);
        self.marks.insert('>', b);
        self.last_visual_sel = Some((self.mode, doc.anchor, doc.cursor));
        self.mode = Mode::Normal;
        doc.anchor = doc.cursor;
    }

    fn request_scroll(&mut self, s: Scroll) {
        self.serial += 1;
        self.scroll = Some((self.serial, s));
    }

    // --- Registers ------------------------------------------------------------

    fn get_register(&self, name: Option<char>) -> Register {
        let key = match name {
            None if self.unnamedplus && self.registers.contains_key(&'+') => '+',
            None => '"',
            Some('*') => '+',
            Some(c) => c.to_ascii_lowercase(),
        };
        match key {
            '/' => Register { text: self.search.as_ref().map(|s| s.0.clone()).unwrap_or_default(), kind: RegKind::Char },
            ':' => Register { text: self.last_ex.clone().unwrap_or_default(), kind: RegKind::Char },
            k => self.registers.get(&k).cloned().unwrap_or_default(),
        }
    }

    /// Stores yanked or deleted text following Vim's register rules.
    fn store(&mut self, name: Option<char>, reg: Register, yank: bool) {
        match name {
            Some('_') => return,
            Some(c @ 'A'..='Z') => {
                let lower = c.to_ascii_lowercase();
                let old = self.registers.remove(&lower).unwrap_or_default();
                let joined = if old.text.is_empty() {
                    reg.clone()
                } else if old.kind == RegKind::Line || reg.kind == RegKind::Line {
                    Register { text: format!("{}\n{}", old.text, reg.text), kind: RegKind::Line }
                } else {
                    Register { text: old.text + &reg.text, kind: old.kind }
                };
                self.registers.insert(lower, joined.clone());
                self.registers.insert('"', joined);
                return;
            }
            Some(c @ ('+' | '*')) => {
                self.set_clipboard(&reg);
                self.registers.insert('+', reg.clone());
                let _ = c;
            }
            Some(c) if c.is_ascii_alphanumeric() || c == '-' => {
                self.registers.insert(c, reg.clone());
            }
            _ => {
                if yank {
                    self.registers.insert('0', reg.clone());
                } else if reg.kind == RegKind::Line || reg.text.contains('\n') {
                    for i in (1..9).rev() {
                        if let Some(r) = self.registers.get(&char::from_digit(i, 10).unwrap()).cloned() {
                            self.registers.insert(char::from_digit(i + 1, 10).unwrap(), r);
                        }
                    }
                    self.registers.insert('1', reg.clone());
                } else {
                    self.registers.insert('-', reg.clone());
                }
                if self.unnamedplus {
                    self.set_clipboard(&reg);
                    self.registers.insert('+', reg.clone());
                }
            }
        }
        self.registers.insert('"', reg);
    }

    fn set_clipboard(&mut self, reg: &Register) {
        let text = if reg.kind == RegKind::Line { format!("{}\n", reg.text) } else { reg.text.clone() };
        self.serial += 1;
        self.clipboard_out = Some((self.serial, text));
    }

    fn register_listing(&self) -> String {
        let order = "\"0123456789abcdefghijklmnopqrstuvwxyz-.+";
        let mut parts = vec![];
        for c in order.chars() {
            if let Some(r) = self.registers.get(&c) {
                if r.text.is_empty() {
                    continue;
                }
                let mut t: String = r.text.chars().take(18).collect::<String>().replace('\n', "^J");
                if r.text.chars().count() > 18 {
                    t.push('…');
                }
                parts.push(format!("\"{c} {t}"));
            }
        }
        if parts.is_empty() { "No registers set".into() } else { parts.join("   ") }
    }

    fn shift_marks(&mut self, from_line: usize, delta: isize) {
        for p in self.marks.values_mut() {
            if p.line > from_line {
                p.line = (p.line as isize + delta).max(from_line as isize) as usize;
            }
        }
    }

    // --- Entry points ---------------------------------------------------------

    /// Handles actions while Vim is on. Returns false to let the document
    /// apply the action itself.
    pub(crate) fn intercept(&mut self, doc: &mut Document, action: &Action) -> bool {
        match action {
            Action::Key(k) => {
                if let Some(key) = VKey::from_event(k) {
                    self.handle(doc, key, true);
                }
                true
            }
            Action::Clipboard(t) => {
                let t = normalize(t);
                let reg = match t.strip_suffix('\n') {
                    Some(line) => Register { text: line.to_owned(), kind: RegKind::Line },
                    None => Register { text: t, kind: RegKind::Char },
                };
                self.registers.insert('+', reg);
                true
            }
            Action::Viewport { top, lines } => {
                self.view_top = *top;
                self.view_lines = *lines;
                true
            }
            Action::Click { pos, select } => {
                self.keys.clear();
                doc.goal = None;
                let p = doc.clamp(*pos);
                if *select {
                    if self.mode == Mode::Normal {
                        self.enter_visual(doc, Mode::Visual);
                    }
                    doc.cursor = p;
                } else {
                    self.exit_visual(doc);
                    doc.cursor = p;
                    doc.anchor = p;
                }
                self.settle(doc);
                true
            }
            Action::Drag(pos) => {
                if self.mode == Mode::Insert {
                    self.leave_insert(doc);
                }
                if self.mode == Mode::Normal {
                    self.mode = Mode::Visual;
                }
                doc.cursor = normal_clamp(&doc.lines, doc.clamp(*pos));
                true
            }
            Action::SelectWord(pos) => {
                doc.apply_plain(Action::SelectWord(*pos));
                if doc.cursor != doc.anchor {
                    doc.cursor = Pos::new(doc.cursor.line, prev_boundary(&doc.lines[doc.cursor.line], doc.cursor.col));
                }
                if self.mode == Mode::Insert {
                    self.leave_insert(doc);
                }
                self.mode = Mode::Visual;
                true
            }
            Action::SelectAll => {
                if self.mode == Mode::Insert {
                    self.leave_insert(doc);
                }
                doc.anchor = Pos::default();
                let last = doc.lines.len() - 1;
                doc.cursor = normal_clamp(&doc.lines, Pos::new(last, doc.lines[last].len()));
                self.mode = Mode::Visual;
                true
            }
            Action::Insert(t) if self.mode != Mode::Insert => {
                // A system paste outside Insert mode replaces the selection or goes before the cursor.
                self.edit(doc);
                if self.mode.is_visual() {
                    self.delete_visual(doc, Some('_'));
                }
                doc.insert(&normalize(t));
                self.settle(doc);
                true
            }
            Action::Backspace | Action::Delete if self.mode.is_visual() => {
                // System cut of a Visual selection.
                self.delete_visual(doc, None);
                self.settle(doc);
                true
            }
            Action::Undo | Action::Redo if self.mode != Mode::Insert => {
                self.handle(doc, if *action == Action::Undo { VKey::Char('u') } else { VKey::Ctrl('r') }, true);
                true
            }
            _ => false,
        }
    }

    /// Starts an undo step for this command if one is not already open.
    fn edit(&mut self, doc: &mut Document) {
        if !doc.grouped {
            doc.begin(EditKind::Other);
            doc.grouped = true;
        }
        self.changed = true;
    }

    fn settle(&mut self, doc: &mut Document) {
        if self.mode != Mode::Insert {
            doc.grouped = false;
            doc.cursor = normal_clamp(&doc.lines, doc.cursor);
            if self.mode == Mode::Normal {
                doc.anchor = doc.cursor;
            }
        }
    }

    fn leave_insert(&mut self, doc: &mut Document) {
        self.mode = Mode::Normal;
        doc.grouped = false;
        self.insert = InsertState::default();
        if let Some(mut r) = self.recording.take() {
            r.push(VKey::Esc);
            self.last_change = r;
        }
    }

    /// `typed` is false for keys replayed by `.` or a macro.
    fn handle(&mut self, doc: &mut Document, key: VKey, typed: bool) {
        if typed {
            self.message = None;
            if let Some((_, rec)) = self.macro_rec.as_mut() {
                rec.push(key);
            }
        }
        let before = doc.lines.len();
        let start_line = doc.cursor.line.min(doc.anchor.line);
        self.dispatch(doc, key);
        let delta = doc.lines.len() as isize - before as isize;
        if delta != 0 {
            self.shift_marks(start_line, delta);
        }
    }

    fn dispatch(&mut self, doc: &mut Document, key: VKey) {
        if self.cmdline.is_some() {
            self.command_line_key(doc, key);
            return;
        }
        if self.mode == Mode::Insert {
            if let (Some(r), 0) = (self.recording.as_mut(), self.dot_depth) {
                r.push(key);
            }
            self.insert_key(doc, key);
            return;
        }
        self.keys.push(key);
        self.changed = false;
        let keys = self.keys.clone();
        let visual = self.mode.is_visual().then(|| self.visual_size(doc));
        match self.command(doc, &keys) {
            Parsed::More => return,
            Parsed::Fail => {
                if self.macro_depth > 0 {
                    self.aborted = true;
                }
            }
            Parsed::Done => {
                let cmd = keys.iter().copied().find(|k| !matches!(k, VKey::Char('0'..='9')));
                let repeatable = !matches!(cmd, Some(VKey::Char('.' | 'u' | '@' | 'q')) | Some(VKey::Ctrl('r')));
                if self.changed && self.dot_depth == 0 && repeatable {
                    self.last_visual = visual;
                    if self.mode == Mode::Insert {
                        self.recording = Some(keys.clone());
                    } else {
                        self.last_change = keys.clone();
                    }
                }
            }
        }
        self.keys.clear();
        self.settle(doc);
    }

    // --- Insert mode ------------------------------------------------------------

    fn insert_key(&mut self, doc: &mut Document, key: VKey) {
        if self.insert.register_pending {
            self.insert.register_pending = false;
            if let VKey::Char(c) = key {
                let reg = self.get_register(Some(c));
                let text = if reg.kind == RegKind::Line { format!("{}\n", reg.text) } else { reg.text };
                if !text.is_empty() {
                    self.edit(doc);
                    doc.apply_plain(Action::Insert(text));
                }
                self.insert.keys.push(VKey::Ctrl('r'));
                self.insert.keys.push(key);
            }
            return;
        }
        match key {
            VKey::Esc => self.finish_insert(doc),
            VKey::Ctrl('r') => self.insert.register_pending = true,
            k => {
                self.insert.keys.push(k);
                self.insert_edit(doc, k);
            }
        }
    }

    fn insert_edit(&mut self, doc: &mut Document, key: VKey) {
        match key {
            VKey::Char(c) => {
                self.edit(doc);
                doc.apply_plain(Action::Insert(c.to_string()));
            }
            VKey::Enter => {
                self.edit(doc);
                doc.apply_plain(Action::Enter);
            }
            VKey::Backspace | VKey::Ctrl('h') => {
                self.edit(doc);
                doc.apply_plain(Action::Backspace);
            }
            VKey::Delete => {
                self.edit(doc);
                doc.apply_plain(Action::Delete);
            }
            VKey::Tab => {
                self.edit(doc);
                doc.apply_plain(Action::Indent);
            }
            VKey::BackTab => {
                self.edit(doc);
                doc.apply_plain(Action::Outdent);
            }
            VKey::Ctrl('w') => {
                self.edit(doc);
                doc.apply_plain(Action::Move { motion: DocMotion::WordLeft, select: true });
                doc.apply_plain(Action::Backspace);
            }
            VKey::Ctrl('u') => {
                self.edit(doc);
                doc.anchor = Pos::new(doc.cursor.line, 0);
                doc.delete_selection();
            }
            VKey::Left | VKey::Right | VKey::Up | VKey::Down | VKey::Home | VKey::End | VKey::PageUp | VKey::PageDown => {
                let motion = match key {
                    VKey::Left => DocMotion::Left,
                    VKey::Right => DocMotion::Right,
                    VKey::Up => DocMotion::Up,
                    VKey::Down => DocMotion::Down,
                    VKey::Home => DocMotion::LineStart,
                    VKey::End => DocMotion::LineEnd,
                    VKey::PageUp => DocMotion::PageUp(self.view_lines.max(20)),
                    _ => DocMotion::PageDown(self.view_lines.max(20)),
                };
                doc.apply_plain(Action::Move { motion, select: false });
            }
            _ => {}
        }
    }

    fn finish_insert(&mut self, doc: &mut Document) {
        let state = std::mem::take(&mut self.insert);
        // A count repeats what was typed (`3ix<Esc>`, `2oline<Esc>`).
        for _ in 1..state.count.max(1) {
            if let Some(below) = state.open_line {
                let c = doc.cursor;
                let at = if below { c.line + 1 } else { c.line };
                let indent: String = doc.lines[c.line].chars().take_while(|c| *c == ' ').collect();
                doc.lines.insert(at, indent.clone());
                doc.cursor = Pos::new(at, indent.len());
                doc.anchor = doc.cursor;
            }
            for &k in &state.keys {
                self.insert_edit(doc, k);
            }
        }
        // Visual Block `I`, `A` and `c` copy the first line's insert to the others.
        if let Some(bi) = state.block {
            let cur = doc.cursor;
            if cur.line == bi.first_line {
                let line = doc.lines[bi.first_line].clone();
                let typed: String = self.typed_text(&state.keys);
                // With `$A` each line was appended at its own end; the typed text ends at the cursor.
                let start = if bi.to_end { cur.col.saturating_sub(typed.len()) } else { byte_at(&line, bi.col) };
                if cur.col >= start && !typed.contains('\n') && !typed.is_empty() {
                    for l in bi.first_line + 1..=bi.last_line.min(doc.lines.len() - 1) {
                        let chars = doc.lines[l].chars().count();
                        let at = if bi.to_end {
                            doc.lines[l].len()
                        } else if chars < bi.col {
                            if !bi.append {
                                continue;
                            }
                            doc.lines[l].push_str(&" ".repeat(bi.col - chars));
                            doc.lines[l].len()
                        } else {
                            byte_at(&doc.lines[l], bi.col)
                        };
                        doc.lines[l].insert_str(at, &typed);
                    }
                }
            }
        }
        let typed = self.typed_text(&state.keys);
        if !typed.is_empty() {
            self.registers.insert('.', Register { text: typed, kind: RegKind::Char });
        }
        self.marks.insert('.', doc.cursor);
        self.recording_insert_done(doc);
    }

    fn recording_insert_done(&mut self, doc: &mut Document) {
        self.mode = Mode::Normal;
        doc.grouped = false;
        if let Some(mut r) = self.recording.take() {
            r.push(VKey::Esc);
            self.last_change = r;
        }
        let c = doc.cursor;
        if c.col > 0 {
            doc.cursor = Pos::new(c.line, prev_boundary(&doc.lines[c.line], c.col));
        }
        doc.goal = None;
        self.settle(doc);
    }

    fn typed_text(&self, keys: &[VKey]) -> String {
        let mut s = String::new();
        for k in keys {
            match k {
                VKey::Char(c) => s.push(*c),
                VKey::Enter => s.push('\n'),
                VKey::Backspace => {
                    s.pop();
                }
                _ => {}
            }
        }
        s
    }

    fn start_insert(&mut self, doc: &mut Document, at: Pos, count: usize) -> Parsed {
        doc.cursor = at;
        doc.anchor = at;
        self.mode = Mode::Insert;
        self.insert = InsertState { count, ..Default::default() };
        self.changed = true;
        Parsed::Done
    }

    // --- Command line -----------------------------------------------------------

    fn command_line_key(&mut self, doc: &mut Document, key: VKey) {
        let Some((prefix, text)) = self.cmdline.as_mut() else { return };
        match key {
            VKey::Esc => self.cmdline = None,
            VKey::Backspace => {
                if text.pop().is_none() {
                    self.cmdline = None;
                }
            }
            VKey::Char(c) => text.push(c),
            VKey::Ctrl('u') => text.clear(),
            VKey::Enter => {
                let (prefix, text) = (*prefix, text.clone());
                self.cmdline = None;
                if prefix == ':' {
                    let cmd = text.trim().to_owned();
                    if !cmd.is_empty() {
                        self.last_ex = Some(cmd.clone());
                    }
                    self.ex(doc, &cmd);
                } else {
                    let pattern = if text.is_empty() { self.search.as_ref().map(|s| s.0.clone()).unwrap_or_default() } else { text };
                    if !pattern.is_empty() {
                        self.search = Some((pattern, prefix == '/'));
                        let cur = doc.cursor;
                        if let Some(p) = self.find_match(doc, cur, true) {
                            self.marks.insert('\'', cur);
                            doc.cursor = p;
                        }
                    }
                }
                self.settle(doc);
            }
            _ => {}
        }
    }

    fn parse_address(&self, doc: &Document, s: &str, i: &mut usize) -> Result<Option<usize>, String> {
        let b = s.as_bytes();
        let last = doc.lines.len() as isize - 1;
        let cur = doc.cursor.line as isize;
        let mut base: Option<isize> = None;
        match b.get(*i) {
            Some(b'.') => {
                base = Some(cur);
                *i += 1;
            }
            Some(b'$') => {
                base = Some(last);
                *i += 1;
            }
            Some(b'\'') => {
                let m = s[*i + 1..].chars().next().ok_or("E20: Mark not set")?;
                let p = self.marks.get(&m).ok_or("E20: Mark not set")?;
                base = Some(p.line as isize);
                *i += 1 + m.len_utf8();
            }
            Some(d) if d.is_ascii_digit() => {
                let start = *i;
                while b.get(*i).is_some_and(u8::is_ascii_digit) {
                    *i += 1;
                }
                base = Some(s[start..*i].parse::<isize>().unwrap_or(1) - 1);
            }
            _ => {}
        }
        while let Some(&c) = b.get(*i) {
            if c != b'+' && c != b'-' {
                break;
            }
            *i += 1;
            let start = *i;
            while b.get(*i).is_some_and(u8::is_ascii_digit) {
                *i += 1;
            }
            let n: isize = if start == *i { 1 } else { s[start..*i].parse().unwrap_or(1) };
            base = Some(base.unwrap_or(cur) + if c == b'+' { n } else { -n });
        }
        Ok(base.map(|l| l.clamp(0, last) as usize))
    }

    fn parse_range(&self, doc: &Document, s: &str) -> Result<(Option<(usize, usize)>, usize), String> {
        if s.starts_with('%') {
            return Ok((Some((0, doc.lines.len() - 1)), 1));
        }
        let mut i = 0;
        let a = self.parse_address(doc, s, &mut i)?;
        if s.as_bytes().get(i) == Some(&b',') {
            i += 1;
            let b = self.parse_address(doc, s, &mut i)?.unwrap_or(doc.cursor.line);
            let a = a.unwrap_or(doc.cursor.line);
            return Ok((Some((a.min(b), a.max(b))), i));
        }
        Ok((a.map(|a| (a, a)), i))
    }

    fn ex(&mut self, doc: &mut Document, input: &str) {
        let (range, rest) = match self.parse_range(doc, input) {
            Ok(r) => r,
            Err(e) => {
                self.message = Some(e);
                return;
            }
        };
        let cmd = input[rest..].trim();
        let cur = doc.cursor.line;
        let (first, last) = range.unwrap_or((cur, cur));
        let word = cmd.split(|c: char| !c.is_ascii_alphabetic()).next().unwrap_or("");
        let arg = cmd[word.len()..].trim_start();
        match (word, cmd) {
            ("", "") => {
                if range.is_some() {
                    self.marks.insert('\'', doc.cursor);
                    doc.cursor = Pos::new(last, first_non_blank(&doc.lines[last]));
                }
            }
            ("s" | "substitute", _) if !cmd[word.len()..].starts_with(|c: char| c.is_alphanumeric() || c.is_whitespace()) => {
                if let Err(e) = self.substitute(doc, (first, last), &cmd[word.len()..]) {
                    self.message = Some(e);
                }
            }
            ("w" | "write", _) => self.requests.push(VimRequest::Write),
            ("q" | "quit", _) => self.requests.push(VimRequest::Quit { force: cmd.ends_with('!') }),
            ("wq" | "x" | "xit" | "exit", _) => self.requests.push(VimRequest::WriteQuit),
            ("d" | "delete", _) => {
                let reg = arg.chars().next();
                self.operate(doc, 'd', Pos::new(first, 0), Pos::new(last, doc.lines[last].len()), true, reg);
            }
            ("y" | "yank", _) => {
                let reg = arg.chars().next();
                self.operate(doc, 'y', Pos::new(first, 0), Pos::new(last, doc.lines[last].len()), true, reg);
            }
            ("j" | "join", _) => {
                doc.cursor = Pos::new(first, 0);
                self.join(doc, (last - first + 1).max(2), true);
            }
            ("", c) if c.starts_with('>') || c.starts_with('<') => {
                let op = c.chars().next().unwrap();
                for _ in 0..c.chars().take_while(|x| *x == op).count() {
                    self.operate(doc, op, Pos::new(first, 0), Pos::new(last, 0), true, None);
                }
            }
            ("noh" | "nohlsearch", _) => {}
            ("set" | "se", _) => self.set_option(arg),
            ("reg" | "registers" | "di" | "display", _) => self.message = Some(self.register_listing()),
            ("marks", _) => {
                let mut marks: Vec<_> = self.marks.iter().filter(|(k, _)| k.is_ascii_lowercase() || "'<>.".contains(**k)).collect();
                marks.sort();
                self.message = Some(if marks.is_empty() {
                    "No marks set".into()
                } else {
                    marks.iter().map(|(k, p)| format!("{k} {}:{}", p.line + 1, p.col)).collect::<Vec<_>>().join("   ")
                });
            }
            _ => self.message = Some(format!("Not an editor command: {cmd}")),
        }
    }

    fn set_option(&mut self, arg: &str) {
        for opt in arg.split_whitespace() {
            match opt {
                "ic" | "ignorecase" => self.ignorecase = true,
                "noic" | "noignorecase" => self.ignorecase = false,
                "scs" | "smartcase" => self.smartcase = true,
                "noscs" | "nosmartcase" => self.smartcase = false,
                "clipboard=unnamedplus" | "clipboard=unnamed" | "cb=unnamedplus" | "cb=unnamed" => self.unnamedplus = true,
                "clipboard=" | "cb=" => self.unnamedplus = false,
                other => {
                    self.message = Some(format!("Unknown option: {other}"));
                    return;
                }
            }
        }
    }

    /// `:s/pattern/replacement/flags` over `range`.
    fn substitute(&mut self, doc: &mut Document, range: (usize, usize), args: &str) -> Result<(), String> {
        let mut chars = args.chars();
        let delim = chars.next().ok_or("E35: No previous regular expression")?;
        let parts = split_delim(chars.as_str(), delim);
        let pattern = match parts.first().map(String::as_str) {
            Some("") | None => self.search.as_ref().map(|s| s.0.clone()).ok_or("E35: No previous regular expression")?,
            Some(p) => p.to_owned(),
        };
        let template = replacement(parts.get(1).map(String::as_str).unwrap_or(""));
        let flags = parts.get(2).map(String::as_str).unwrap_or("");
        let global = flags.contains('g');
        let ignorecase = if flags.contains('I') { false } else { flags.contains('i') || self.ignorecase };
        let re = compile(&pattern, ignorecase, self.smartcase && !flags.contains('i'))?;
        self.search = Some((pattern.clone(), true));

        let (mut l, mut end) = range;
        let (mut subs, mut lines, mut last_line) = (0, 0, None);
        while l <= end && l < doc.lines.len() {
            let line = doc.lines[l].clone();
            let n = if global { re.find_iter(&line).count() } else { usize::from(re.is_match(&line)) };
            if n == 0 {
                l += 1;
                continue;
            }
            if subs == 0 {
                self.edit(doc);
            }
            let new = if global { re.replace_all(&line, template.as_str()) } else { re.replace(&line, template.as_str()) };
            let parts: Vec<String> = new.split('\n').map(str::to_owned).collect();
            let added = parts.len() - 1;
            doc.lines.splice(l..=l, parts);
            subs += n;
            lines += 1;
            last_line = Some(l + added);
            end += added;
            l += added + 1;
        }
        let Some(last_line) = last_line else { return Err(format!("Pattern not found: {pattern}")) };
        self.marks.insert('\'', doc.cursor);
        doc.cursor = Pos::new(last_line, first_non_blank(&doc.lines[last_line]));
        if subs > 1 {
            self.message = Some(format!("{subs} substitution{} on {lines} line{}", if subs == 1 { "" } else { "s" }, if lines == 1 { "" } else { "s" }));
        }
        Ok(())
    }

    /// Finds the next match of the last search. `same` searches in the
    /// original direction; false reverses it (`N`).
    fn find_match(&mut self, doc: &Document, from: Pos, same: bool) -> Option<Pos> {
        let (pattern, fwd) = self.search.clone()?;
        let forward = fwd == same;
        let re = match compile(&pattern, self.ignorecase, self.smartcase) {
            Ok(r) => r,
            Err(e) => {
                self.message = Some(e);
                return None;
            }
        };
        let n = doc.lines.len();
        let hits = |l: usize| -> Vec<usize> { re.find_iter(&doc.lines[l]).map(|m| m.start()).collect() };
        let same_line = hits(from.line);
        let first = if forward { same_line.iter().copied().find(|&c| c > from.col) } else { same_line.iter().copied().rfind(|&c| c < from.col) };
        if let Some(c) = first {
            return Some(Pos::new(from.line, c));
        }
        for step in 1..=n {
            let l = if forward { (from.line + step) % n } else { (from.line + n * 2 - step) % n };
            let h = hits(l);
            let found = if forward { h.first() } else { h.last() };
            if let Some(&c) = found {
                let wrapped = if forward { l <= from.line } else { l >= from.line };
                if wrapped {
                    self.message = Some(if forward { "search hit BOTTOM, continuing at TOP" } else { "search hit TOP, continuing at BOTTOM" }.into());
                }
                return Some(Pos::new(l, c));
            }
        }
        self.message = Some(format!("Pattern not found: {pattern}"));
        None
    }

    // --- Normal and Visual commands -------------------------------------------

    fn command(&mut self, doc: &mut Document, ks: &[VKey]) -> Parsed {
        let mut i = 0;
        // Register prefix: "x
        let mut reg: Option<char> = None;
        if ks.first() == Some(&VKey::Char('"')) {
            match ks.get(1) {
                None => return Parsed::More,
                Some(VKey::Char(r)) if valid_register(*r) => {
                    reg = Some(*r);
                    i = 2;
                }
                _ => return Parsed::Fail,
            }
        }
        let c1 = count(ks, &mut i);
        let Some(&k) = ks.get(i) else { return Parsed::More };
        i += 1;
        let n = c1.unwrap_or(1);
        let visual = self.mode.is_visual();
        let cur = doc.cursor;

        if visual
            && let Some(done) = self.visual_command(doc, ks, i, k, n, reg) {
                return done;
            }

        match k {
            VKey::Esc => {
                self.exit_visual(doc);
                Parsed::Done
            }
            VKey::Char(':') => {
                let text = if visual {
                    self.exit_visual(doc);
                    "'<,'>".to_owned()
                } else if let Some(c) = c1 {
                    if c == 1 { ".".to_owned() } else { format!(".,.+{}", c - 1) }
                } else {
                    String::new()
                };
                self.cmdline = Some((':', text));
                Parsed::Done
            }
            VKey::Char(c @ ('/' | '?')) => {
                self.cmdline = Some((c, String::new()));
                Parsed::Done
            }
            VKey::Char('q') => {
                if let Some((r, mut keys)) = self.macro_rec.take() {
                    keys.pop(); // the `q` that stopped recording
                    let text: String = keys.iter().map(|k| k.to_char()).collect();
                    let store = if r.is_ascii_uppercase() { r } else { r.to_ascii_lowercase() };
                    self.store(Some(store), Register { text, kind: RegKind::Char }, true);
                    self.message = None;
                    return Parsed::Done;
                }
                match ks.get(i) {
                    None => Parsed::More,
                    Some(VKey::Char(r)) if r.is_ascii_alphanumeric() || *r == '"' => {
                        self.macro_rec = Some((*r, vec![]));
                        self.message = Some(format!("recording @{r}"));
                        Parsed::Done
                    }
                    _ => Parsed::Fail,
                }
            }
            VKey::Char('@') => {
                let r = match ks.get(i) {
                    None => return Parsed::More,
                    Some(VKey::Char('@')) => match self.last_macro {
                        Some(r) => r,
                        None => return Parsed::Fail,
                    },
                    Some(VKey::Char(r)) if valid_register(*r) => *r,
                    _ => return Parsed::Fail,
                };
                if r == ':' {
                    let Some(cmd) = self.last_ex.clone() else { return Parsed::Fail };
                    for _ in 0..n {
                        self.ex(doc, &cmd);
                    }
                    return Parsed::Done;
                }
                if self.macro_depth > 40 {
                    self.message = Some("Macro nested too deeply".into());
                    return Parsed::Fail;
                }
                self.last_macro = Some(r);
                let keys: Vec<VKey> = {
                    let reg = self.get_register(Some(r));
                    let text = if reg.kind == RegKind::Line { format!("{}\n", reg.text) } else { reg.text };
                    text.chars().map(VKey::from_char).collect()
                };
                self.keys.clear();
                self.macro_depth += 1;
                'run: for _ in 0..n {
                    for &k in &keys {
                        self.handle(doc, k, false);
                        if self.aborted {
                            break 'run;
                        }
                    }
                }
                self.macro_depth -= 1;
                if self.macro_depth == 0 {
                    self.aborted = false;
                }
                Parsed::Done
            }
            VKey::Char('m') => match ks.get(i) {
                None => Parsed::More,
                Some(VKey::Char(m)) if m.is_ascii_alphabetic() || "'`<>".contains(*m) => {
                    let m = if *m == '`' { '\'' } else { *m };
                    self.marks.insert(m, cur);
                    Parsed::Done
                }
                _ => Parsed::Fail,
            },
            VKey::Char(op @ ('d' | 'c' | 'y' | '>' | '<')) => self.operator(doc, ks, i, op, c1, reg),
            VKey::Char('g') => match ks.get(i) {
                None => Parsed::More,
                Some(VKey::Char(op @ ('u' | 'U' | '~'))) => {
                    // `guu`, `gUU`, `g~~` and `gugu` act on lines.
                    if matches!(ks.get(i + 1), Some(VKey::Char(x)) if *x == *op) || ks.get(i + 1..i + 3) == Some(&[VKey::Char('g'), VKey::Char(*op)]) {
                        let last = (cur.line + n - 1).min(doc.lines.len() - 1);
                        self.operate(doc, *op, Pos::new(cur.line, 0), Pos::new(last, doc.lines[last].len()), true, reg);
                        return Parsed::Done;
                    }
                    self.operator(doc, ks, i + 1, *op, c1, reg)
                }
                Some(VKey::Char('v')) => {
                    let Some((mode, a, c)) = self.last_visual_sel else { return Parsed::Fail };
                    doc.anchor = normal_clamp(&doc.lines, a);
                    doc.cursor = normal_clamp(&doc.lines, c);
                    if mode == Mode::VisualBlock {
                        self.block_anchor_col = char_col(&doc.lines[doc.anchor.line], doc.anchor.col);
                    }
                    self.mode = mode;
                    Parsed::Done
                }
                Some(VKey::Char('J')) => {
                    self.join(doc, n.max(2), false);
                    Parsed::Done
                }
                Some(VKey::Char('i')) => {
                    let p = self.marks.get(&'.').copied().map(|p| doc.clamp(p)).unwrap_or(cur);
                    self.start_insert(doc, p, n)
                }
                _ => self.motion_command(doc, ks, i - 1, n, c1.is_some(), visual),
            },
            VKey::Char('x') | VKey::Delete => {
                let line = &doc.lines[cur.line];
                if line.is_empty() {
                    return Parsed::Done;
                }
                let mut b = cur.col;
                for _ in 0..n {
                    b = next_boundary(line, b);
                }
                self.operate(doc, 'd', cur, Pos::new(cur.line, b), false, reg);
                Parsed::Done
            }
            VKey::Char('X') => {
                let line = &doc.lines[cur.line];
                let mut a = cur.col;
                for _ in 0..n {
                    a = prev_boundary(line, a);
                }
                if a < cur.col {
                    self.operate(doc, 'd', Pos::new(cur.line, a), cur, false, reg);
                }
                Parsed::Done
            }
            VKey::Char('D') => {
                self.operate(doc, 'd', cur, Pos::new(cur.line, doc.lines[cur.line].len()), false, reg);
                Parsed::Done
            }
            VKey::Char('C') => {
                self.operate(doc, 'c', cur, Pos::new(cur.line, doc.lines[cur.line].len()), false, reg);
                Parsed::Done
            }
            VKey::Char('s') => {
                let b = if doc.lines[cur.line].is_empty() { cur.col } else { next_boundary(&doc.lines[cur.line], cur.col) };
                self.operate(doc, 'c', cur, Pos::new(cur.line, b), false, reg);
                Parsed::Done
            }
            VKey::Char('S') => {
                let last = (cur.line + n - 1).min(doc.lines.len() - 1);
                self.operate(doc, 'c', Pos::new(cur.line, 0), Pos::new(last, doc.lines[last].len()), true, reg);
                Parsed::Done
            }
            VKey::Char('Y') => {
                let last = (cur.line + n - 1).min(doc.lines.len() - 1);
                self.operate(doc, 'y', Pos::new(cur.line, 0), Pos::new(last, doc.lines[last].len()), true, reg);
                Parsed::Done
            }
            VKey::Char(p @ ('p' | 'P')) => {
                self.put(doc, p == 'p', n, reg);
                Parsed::Done
            }
            VKey::Char('J') => {
                self.join(doc, n.max(2), true);
                Parsed::Done
            }
            VKey::Char('r') => {
                let Some(&r) = ks.get(i) else { return Parsed::More };
                let VKey::Char(r) = r else { return Parsed::Fail };
                let line = &doc.lines[cur.line];
                let mut b = cur.col;
                for _ in 0..n {
                    if b >= line.len() {
                        return Parsed::Fail;
                    }
                    b = next_boundary(line, b);
                }
                self.edit(doc);
                let count = doc.lines[cur.line][cur.col..b].chars().count();
                doc.lines[cur.line].replace_range(cur.col..b, &r.to_string().repeat(count));
                doc.cursor = Pos::new(cur.line, cur.col + r.len_utf8() * (count - 1));
                Parsed::Done
            }
            VKey::Char('~') => {
                let line = doc.lines[cur.line].clone();
                if line.is_empty() {
                    return Parsed::Done;
                }
                let mut b = cur.col;
                for _ in 0..n {
                    b = next_boundary(&line, b);
                }
                self.edit(doc);
                let swapped: String = line[cur.col..b].chars().map(swap_case).collect();
                doc.lines[cur.line].replace_range(cur.col..b, &swapped);
                doc.cursor = Pos::new(cur.line, (cur.col + swapped.len()).min(doc.lines[cur.line].len()));
                Parsed::Done
            }
            VKey::Char('u') => {
                let before = doc.lines.clone();
                for _ in 0..n {
                    doc.grouped = false;
                    if !doc.step(true) {
                        self.message = Some("Already at oldest change".into());
                        break;
                    }
                }
                Self::cursor_to_change(doc, &before);
                doc.anchor = doc.cursor;
                Parsed::Done
            }
            VKey::Ctrl('r') => {
                let before = doc.lines.clone();
                for _ in 0..n {
                    doc.grouped = false;
                    if !doc.step(false) {
                        self.message = Some("Already at newest change".into());
                        break;
                    }
                }
                Self::cursor_to_change(doc, &before);
                doc.anchor = doc.cursor;
                Parsed::Done
            }
            VKey::Char('.') => {
                let mut keys = self.last_change.clone();
                if keys.is_empty() {
                    return Parsed::Done;
                }
                if let Some(vs) = self.last_visual {
                    self.reselect(doc, vs);
                } else if let Some(c) = c1 {
                    // A count replaces the original one.
                    let skip = if keys.first() == Some(&VKey::Char('"')) { 2 } else { 0 };
                    let mut j = skip;
                    count(&keys, &mut j);
                    keys.splice(skip..j, c.to_string().chars().map(VKey::Char));
                }
                self.keys.clear();
                self.dot_depth += 1;
                for k in keys {
                    self.handle(doc, k, false);
                }
                self.dot_depth -= 1;
                Parsed::Done
            }
            VKey::Char('i') => self.start_insert(doc, cur, n),
            VKey::Char('a') => {
                let line = &doc.lines[cur.line];
                let col = if line.is_empty() { 0 } else { next_boundary(line, cur.col) };
                self.start_insert(doc, Pos::new(cur.line, col), n)
            }
            VKey::Char('I') => self.start_insert(doc, Pos::new(cur.line, first_non_blank(&doc.lines[cur.line])), n),
            VKey::Char('A') => self.start_insert(doc, Pos::new(cur.line, doc.lines[cur.line].len()), n),
            VKey::Char(o @ ('o' | 'O')) => {
                self.edit(doc);
                let line = &doc.lines[cur.line];
                let mut indent: String = line.chars().take_while(|c| *c == ' ').collect();
                if o == 'o' && line.trim_end().ends_with(['{', '(', '[']) {
                    indent.push_str(&" ".repeat(INDENT));
                }
                let at = if o == 'o' { cur.line + 1 } else { cur.line };
                doc.lines.insert(at, indent.clone());
                self.start_insert(doc, Pos::new(at, indent.len()), n);
                self.insert.open_line = Some(o == 'o');
                Parsed::Done
            }
            VKey::Char('v') | VKey::Char('V') | VKey::Ctrl('v') => {
                let mode = match k {
                    VKey::Char('v') => Mode::Visual,
                    VKey::Char('V') => Mode::VisualLine,
                    _ => Mode::VisualBlock,
                };
                if self.mode == mode {
                    self.exit_visual(doc);
                } else {
                    self.enter_visual(doc, mode);
                }
                Parsed::Done
            }
            VKey::Char('Z') => match ks.get(i) {
                None => Parsed::More,
                Some(VKey::Char('Z')) => {
                    self.requests.push(VimRequest::WriteQuit);
                    Parsed::Done
                }
                Some(VKey::Char('Q')) => {
                    self.requests.push(VimRequest::Quit { force: true });
                    Parsed::Done
                }
                _ => Parsed::Fail,
            },
            VKey::Char('z') => match ks.get(i) {
                None => Parsed::More,
                Some(VKey::Char('z' | '.')) => {
                    self.request_scroll(Scroll::Center);
                    Parsed::Done
                }
                Some(VKey::Char('t') | VKey::Enter) => {
                    self.request_scroll(Scroll::Top);
                    Parsed::Done
                }
                Some(VKey::Char('b' | '-')) => {
                    self.request_scroll(Scroll::Bottom);
                    Parsed::Done
                }
                _ => Parsed::Fail,
            },
            VKey::Ctrl('d' | 'u' | 'f' | 'b') | VKey::PageDown | VKey::PageUp => {
                let c = match k {
                    VKey::PageDown => 'f',
                    VKey::PageUp => 'b',
                    VKey::Ctrl(c) => c,
                    _ => 'f',
                };
                let lines = if self.view_lines == 0 { 30 } else { self.view_lines };
                let step = if c == 'd' || c == 'u' { (lines / 2).max(1) } else { lines.saturating_sub(2).max(1) } * if matches!(c, 'f' | 'b') { n } else { 1 };
                let last = doc.lines.len() - 1;
                let down = c == 'd' || c == 'f';
                let line = if down { (cur.line + step).min(last) } else { cur.line.saturating_sub(step) };
                if line == cur.line {
                    return Parsed::Fail;
                }
                self.request_scroll(Scroll::Lines(if down { step as isize } else { -(step as isize) }));
                let goal = *doc.goal.get_or_insert_with(|| char_col(&doc.lines[cur.line], cur.col));
                doc.cursor = Pos::new(line, byte_at(&doc.lines[line], goal));
                if !visual {
                    doc.anchor = doc.cursor;
                }
                Parsed::Done
            }
            VKey::Ctrl(c @ ('e' | 'y')) => {
                let delta = if c == 'e' { n as isize } else { -(n as isize) };
                self.request_scroll(Scroll::Lines(delta));
                if self.view_lines > 0 {
                    let top = (self.view_top as isize + delta).max(0) as usize;
                    let bottom = top + self.view_lines - 1;
                    let line = cur.line.clamp(top.min(doc.lines.len() - 1), bottom.min(doc.lines.len() - 1));
                    if line != cur.line {
                        doc.cursor = Pos::new(line, byte_at(&doc.lines[line], char_col(&doc.lines[cur.line], cur.col)));
                        if !visual {
                            doc.anchor = doc.cursor;
                        }
                    }
                }
                Parsed::Done
            }
            _ => self.motion_command(doc, ks, i - 1, n, c1.is_some(), visual),
        }
    }

    /// A plain motion: moves the cursor (and records jumps).
    fn motion_command(&mut self, doc: &mut Document, ks: &[VKey], at: usize, n: usize, counted: bool, visual: bool) -> Parsed {
        let cur = doc.cursor;
        let key = ks[at];
        match self.motion(doc, ks, at, n, counted) {
            Target::More => Parsed::More,
            Target::Fail => Parsed::Fail,
            Target::To(p, _) => {
                if is_jump(key) {
                    self.marks.insert('\'', cur);
                }
                if self.mode == Mode::VisualBlock {
                    let vertical = matches!(key, VKey::Char('j' | 'k') | VKey::Up | VKey::Down);
                    if !vertical {
                        self.block_dollar = matches!(key, VKey::Char('$') | VKey::End);
                        doc.goal = None;
                    }
                }
                doc.cursor = p;
                if !visual {
                    doc.anchor = p;
                }
                Parsed::Done
            }
        }
    }

    /// `op` followed by a motion or text object, starting at `ks[i]`.
    fn operator(&mut self, doc: &mut Document, ks: &[VKey], mut i: usize, op: char, c1: Option<usize>, reg: Option<char>) -> Parsed {
        let cur = doc.cursor;
        let c2 = count(ks, &mut i);
        let Some(&m) = ks.get(i) else { return Parsed::More };
        let n = c1.unwrap_or(1) * c2.unwrap_or(1);
        if m == VKey::Char(op) || (op == 'y' && m == VKey::Char('y')) {
            let last = (cur.line + n - 1).min(doc.lines.len() - 1);
            self.operate(doc, op, Pos::new(cur.line, 0), Pos::new(last, doc.lines[last].len()), true, reg);
            return Parsed::Done;
        }
        if let VKey::Char(ia @ ('i' | 'a')) = m {
            let Some(&VKey::Char(obj)) = ks.get(i + 1) else { return if ks.len() > i + 1 { Parsed::Fail } else { Parsed::More } };
            return match text_object(&doc.lines, cur, ia == 'a', obj) {
                Some((a, b, linewise)) => {
                    self.operate(doc, op, a, b, linewise, reg);
                    Parsed::Done
                }
                None => Parsed::Fail,
            };
        }
        // `cw` changes to the end of the word, like `ce`.
        let m = match m {
            VKey::Char('w') if op == 'c' && class(ch(&doc.lines, cur), false) != 0 => VKey::Char('e'),
            VKey::Char('W') if op == 'c' && class(ch(&doc.lines, cur), true) != 0 => VKey::Char('E'),
            m => m,
        };
        let mut rest = ks[i..].to_vec();
        rest[0] = m;
        match self.motion(doc, &rest, 0, n, c1.or(c2).is_some()) {
            Target::More => Parsed::More,
            Target::Fail => Parsed::Fail,
            Target::To(t, kind) => {
                let (a, mut b) = (cur.min(t), cur.max(t));
                match kind {
                    Kind::Linewise => {
                        self.operate(doc, op, Pos::new(a.line, 0), Pos::new(b.line, doc.lines[b.line].len()), true, reg);
                        return Parsed::Done;
                    }
                    Kind::Inclusive => b = next(&doc.lines, b).filter(|q| q.line == b.line).unwrap_or(Pos::new(b.line, doc.lines[b.line].len())),
                    Kind::Exclusive => {
                        // An exclusive motion that ends at the start of a line stops at the end of the previous one.
                        if b.col == 0 && b.line > a.line && matches!(rest[0], VKey::Char('w' | 'W')) {
                            b = Pos::new(b.line - 1, doc.lines[b.line - 1].len());
                        }
                    }
                }
                self.operate(doc, op, a, b, false, reg);
                Parsed::Done
            }
        }
    }

    /// Commands that behave differently on a Visual selection. Returns
    /// `None` for keys that act as in Normal mode (motions, `v`, `:`...).
    fn visual_command(&mut self, doc: &mut Document, ks: &[VKey], i: usize, k: VKey, n: usize, reg: Option<char>) -> Option<Parsed> {
        if self.mode == Mode::VisualBlock {
            return self.block_command(doc, ks, i, k, n, reg);
        }
        let (a, b) = self.visual_range(doc);
        let linewise = self.mode == Mode::VisualLine;
        let op = match k {
            VKey::Char('d' | 'x') | VKey::Delete => 'd',
            VKey::Char('c' | 's') => 'c',
            VKey::Char('y') => 'y',
            VKey::Char('>') => '>',
            VKey::Char('<') => '<',
            VKey::Char('~') => '~',
            VKey::Char('u') => 'u',
            VKey::Char('U') => 'U',
            VKey::Char('D' | 'X' | 'Y' | 'C' | 'S' | 'R') => {
                let op = match k {
                    VKey::Char('Y') => 'y',
                    VKey::Char('C' | 'S' | 'R') => 'c',
                    _ => 'd',
                };
                self.exit_visual(doc);
                self.operate(doc, op, Pos::new(a.line, 0), Pos::new(b.line, doc.lines[b.line].len()), true, reg);
                return Some(Parsed::Done);
            }
            VKey::Char('o') => {
                std::mem::swap(&mut doc.anchor, &mut doc.cursor);
                return Some(Parsed::Done);
            }
            VKey::Char('J') => {
                self.exit_visual(doc);
                doc.cursor = Pos::new(a.line, 0);
                self.join(doc, (b.line - a.line + 1).max(2), true);
                return Some(Parsed::Done);
            }
            VKey::Char('p' | 'P') => {
                let put = self.get_register(reg);
                self.exit_visual(doc);
                self.delete_range(doc, a, b, linewise, false, None);
                if linewise && put.kind != RegKind::Line {
                    doc.lines.insert(doc.cursor.line, String::new());
                    doc.cursor = Pos::new(doc.cursor.line, 0);
                }
                let saved = self.registers.get(&'"').cloned();
                self.registers.insert('\u{0}', put);
                self.put(doc, false, n, Some('\u{0}'));
                self.registers.remove(&'\u{0}');
                if let Some(s) = saved {
                    self.registers.insert('"', s);
                }
                return Some(Parsed::Done);
            }
            VKey::Char(ia @ ('i' | 'a')) => {
                let Some(&VKey::Char(obj)) = ks.get(i) else { return Some(if ks.len() > i { Parsed::Fail } else { Parsed::More }) };
                if let Some((s, e, lw)) = text_object(&doc.lines, doc.cursor, ia == 'a', obj) {
                    doc.anchor = s;
                    doc.cursor = if lw { e } else { prev(&doc.lines, e).unwrap_or(e) };
                    self.mode = if lw { Mode::VisualLine } else { Mode::Visual };
                }
                return Some(Parsed::Done);
            }
            _ => return None,
        };
        self.exit_visual(doc);
        self.operate(doc, op, a, b, linewise, reg);
        Some(Parsed::Done)
    }

    fn block_command(&mut self, doc: &mut Document, ks: &[VKey], i: usize, k: VKey, n: usize, reg: Option<char>) -> Option<Parsed> {
        let bl = self.block(doc)?;
        let lines = bl.first_line..=bl.last_line.min(doc.lines.len() - 1);
        match k {
            VKey::Char('d' | 'x' | 'y' | 'c' | 's') | VKey::Delete => {
                let text = bl.text(&doc.lines);
                self.store(reg, Register { text, kind: RegKind::Block }, k == VKey::Char('y'));
                self.exit_visual(doc);
                if k != VKey::Char('y') {
                    self.edit(doc);
                    for l in lines.clone() {
                        if let Some((a, b)) = bl.bytes(&doc.lines[l]) {
                            doc.lines[l].replace_range(a..b, "");
                        }
                    }
                }
                let at = Pos::new(bl.first_line, byte_at(&doc.lines[bl.first_line], bl.first_col));
                doc.cursor = at;
                if matches!(k, VKey::Char('c' | 's')) {
                    self.start_insert(doc, at, 1);
                    self.insert.block = Some(BlockInsert { first_line: bl.first_line, last_line: bl.last_line, col: bl.first_col, append: false, to_end: false });
                }
                Some(Parsed::Done)
            }
            VKey::Char(ia @ ('I' | 'A')) => {
                self.exit_visual(doc);
                let append = ia == 'A';
                let col = if append { bl.last_col + 1 } else { bl.first_col };
                self.edit(doc);
                let line = &mut doc.lines[bl.first_line];
                let chars = line.chars().count();
                let at = if append && bl.to_end {
                    line.len()
                } else {
                    if chars < col {
                        line.push_str(&" ".repeat(col - chars));
                    }
                    byte_at(line, col)
                };
                self.start_insert(doc, Pos::new(bl.first_line, at), 1);
                self.insert.block = Some(BlockInsert { first_line: bl.first_line, last_line: bl.last_line, col, append, to_end: bl.to_end && append });
                Some(Parsed::Done)
            }
            VKey::Char('r') => {
                let Some(&r) = ks.get(i) else { return Some(Parsed::More) };
                let VKey::Char(r) = r else { return Some(Parsed::Fail) };
                self.edit(doc);
                for l in lines {
                    if let Some((a, b)) = bl.bytes(&doc.lines[l]) {
                        let count = doc.lines[l][a..b].chars().count();
                        doc.lines[l].replace_range(a..b, &r.to_string().repeat(count));
                    }
                }
                self.exit_visual(doc);
                doc.cursor = Pos::new(bl.first_line, byte_at(&doc.lines[bl.first_line], bl.first_col));
                Some(Parsed::Done)
            }
            VKey::Char(op @ ('~' | 'u' | 'U')) => {
                self.edit(doc);
                let f = case_op(op);
                for l in lines {
                    if let Some((a, b)) = bl.bytes(&doc.lines[l]) {
                        let changed: String = doc.lines[l][a..b].chars().map(f).collect();
                        doc.lines[l].replace_range(a..b, &changed);
                    }
                }
                self.exit_visual(doc);
                doc.cursor = Pos::new(bl.first_line, byte_at(&doc.lines[bl.first_line], bl.first_col));
                Some(Parsed::Done)
            }
            VKey::Char(op @ ('>' | '<')) => {
                self.exit_visual(doc);
                for _ in 0..n {
                    self.operate(doc, op, Pos::new(bl.first_line, 0), Pos::new(bl.last_line, 0), true, None);
                }
                Some(Parsed::Done)
            }
            VKey::Char('o') => {
                let cursor_col = doc.goal.unwrap_or_else(|| char_col(&doc.lines[doc.cursor.line], doc.cursor.col));
                std::mem::swap(&mut doc.anchor, &mut doc.cursor);
                doc.goal = Some(self.block_anchor_col);
                self.block_anchor_col = cursor_col;
                Some(Parsed::Done)
            }
            VKey::Char('p' | 'P') => {
                let put = self.get_register(reg);
                self.handle(doc, VKey::Char('d'), false);
                self.registers.insert('\u{0}', put);
                self.put(doc, false, n, Some('\u{0}'));
                self.registers.remove(&'\u{0}');
                Some(Parsed::Done)
            }
            VKey::Char('J') => {
                self.exit_visual(doc);
                doc.cursor = Pos::new(bl.first_line, 0);
                self.join(doc, (bl.last_line - bl.first_line + 1).max(2), true);
                Some(Parsed::Done)
            }
            _ => None,
        }
    }

    /// After undo or redo, put the cursor on the first changed character, as Vim does.
    fn cursor_to_change(doc: &mut Document, before: &[String]) {
        let after = &doc.lines;
        let line = (0..after.len().max(before.len())).find(|&l| before.get(l) != after.get(l));
        if let Some(l) = line {
            let l = l.min(after.len() - 1);
            let col = match before.get(l) {
                Some(old) => after[l].char_indices().zip(old.chars()).find(|((_, a), b)| a != b).map_or(old.len().min(after[l].len()), |((i, _), _)| i),
                None => 0,
            };
            doc.cursor = normal_clamp(after, Pos::new(l, col));
        }
    }

    fn join(&mut self, doc: &mut Document, lines: usize, spaces: bool) {
        self.edit(doc);
        for _ in 0..lines - 1 {
            let l = doc.cursor.line;
            if l + 1 >= doc.lines.len() {
                break;
            }
            let next_line = doc.lines.remove(l + 1);
            let rest = if spaces { next_line.trim_start() } else { next_line.as_str() };
            let line = &mut doc.lines[l];
            if spaces {
                let trimmed = line.trim_end().len();
                line.truncate(trimmed);
            }
            let at = line.len();
            if spaces && !line.is_empty() && !rest.is_empty() && !rest.starts_with(')') {
                line.push(' ');
            }
            line.push_str(rest);
            doc.cursor = Pos::new(l, at);
        }
    }

    /// Applies operator `op` to `a..b` (exclusive; whole lines when `linewise`).
    fn operate(&mut self, doc: &mut Document, op: char, a: Pos, b: Pos, linewise: bool, reg: Option<char>) {
        match op {
            'y' => {
                self.yank(doc, a, b, linewise, reg);
                doc.cursor = if linewise { Pos::new(a.line, doc.cursor.col) } else { a };
                if linewise && b.line > a.line + 1 {
                    self.message = Some(format!("{} lines yanked", b.line - a.line + 1));
                }
            }
            'd' => self.delete_range(doc, a, b, linewise, false, reg),
            'c' => {
                self.delete_range(doc, a, b, linewise, true, reg);
                let at = doc.cursor;
                self.start_insert(doc, at, 1);
            }
            '>' | '<' => {
                self.edit(doc);
                for l in a.line..=b.line {
                    let line = &mut doc.lines[l];
                    if op == '>' {
                        if !line.is_empty() {
                            line.insert_str(0, &" ".repeat(INDENT));
                        }
                    } else {
                        let n = line.chars().take(INDENT).take_while(|c| *c == ' ').count();
                        line.replace_range(..n, "");
                    }
                }
                doc.cursor = Pos::new(a.line, first_non_blank(&doc.lines[a.line]));
            }
            '~' | 'u' | 'U' => {
                self.edit(doc);
                let f = case_op(op);
                let (a, b) = if linewise { (Pos::new(a.line, 0), Pos::new(b.line, doc.lines[b.line].len())) } else { (a, b) };
                for l in a.line..=b.line {
                    let line = &doc.lines[l];
                    let from = if l == a.line { a.col } else { 0 };
                    let to = if l == b.line { b.col } else { line.len() };
                    let changed: String = line[from..to].chars().map(f).collect();
                    doc.lines[l].replace_range(from..to, &changed);
                }
                doc.cursor = a;
            }
            _ => {}
        }
        self.marks.insert('.', doc.cursor);
    }

    fn yank(&mut self, doc: &Document, a: Pos, b: Pos, linewise: bool, reg: Option<char>) {
        let r = if linewise {
            Register { text: doc.lines[a.line..=b.line].join("\n"), kind: RegKind::Line }
        } else {
            Register { text: text_between(&doc.lines, a, b), kind: RegKind::Char }
        };
        self.store(reg, r, true);
    }

    fn delete_visual(&mut self, doc: &mut Document, reg: Option<char>) {
        if self.mode == Mode::VisualBlock {
            let keys = [VKey::Char('d')];
            let _ = self.block_command(doc, &keys, 1, VKey::Char('d'), 1, reg);
            return;
        }
        let (a, b) = self.visual_range(doc);
        let linewise = self.mode == Mode::VisualLine;
        self.exit_visual(doc);
        self.delete_range(doc, a, b, linewise, false, reg);
    }

    /// Deletes `a..b` into a register. With `keep_line`, a linewise delete
    /// leaves one line with the first line's indentation (for `cc`).
    fn delete_range(&mut self, doc: &mut Document, a: Pos, b: Pos, linewise: bool, keep_line: bool, reg: Option<char>) {
        if a == b && !linewise {
            return;
        }
        let r = if linewise {
            Register { text: doc.lines[a.line..=b.line].join("\n"), kind: RegKind::Line }
        } else {
            Register { text: text_between(&doc.lines, a, b), kind: RegKind::Char }
        };
        self.store(reg, r, false);
        self.edit(doc);
        if linewise {
            if keep_line {
                let indent: String = doc.lines[a.line].chars().take_while(|c| *c == ' ').collect();
                doc.lines.drain(a.line + 1..=b.line);
                doc.lines[a.line] = indent.clone();
                doc.cursor = Pos::new(a.line, indent.len());
            } else {
                doc.lines.drain(a.line..=b.line);
                if doc.lines.is_empty() {
                    doc.lines.push(String::new());
                }
                let line = a.line.min(doc.lines.len() - 1);
                doc.cursor = Pos::new(line, first_non_blank(&doc.lines[line]));
            }
            if b.line > a.line + 1 {
                self.message = Some(format!("{} fewer lines", b.line - a.line + 1));
            }
        } else {
            doc.anchor = a;
            doc.cursor = b;
            doc.delete_selection();
        }
        doc.anchor = doc.cursor;
    }

    fn put(&mut self, doc: &mut Document, after: bool, n: usize, reg: Option<char>) {
        let r = self.get_register(reg);
        if r.text.is_empty() && r.kind != RegKind::Line {
            return;
        }
        self.edit(doc);
        let cur = doc.cursor;
        match r.kind {
            RegKind::Line => {
                let piece: Vec<String> = r.text.split('\n').map(str::to_owned).collect();
                let lines: Vec<String> = std::iter::repeat_n(piece, n).flatten().collect();
                let at = if after { cur.line + 1 } else { cur.line };
                doc.lines.splice(at..at, lines);
                doc.cursor = Pos::new(at, first_non_blank(&doc.lines[at]));
            }
            RegKind::Char => {
                let text = r.text.repeat(n);
                let line = &doc.lines[cur.line];
                let at = if after && !line.is_empty() { next_boundary(line, cur.col) } else { cur.col };
                doc.cursor = Pos::new(cur.line, at);
                doc.anchor = doc.cursor;
                doc.insert(&text);
                doc.cursor = prev(&doc.lines, doc.cursor).unwrap_or(doc.cursor);
            }
            RegKind::Block => {
                let line = &doc.lines[cur.line];
                let col = char_col(line, cur.col) + usize::from(after && !line.is_empty());
                for (k, piece) in r.text.split('\n').enumerate() {
                    let l = cur.line + k;
                    if l >= doc.lines.len() {
                        doc.lines.push(String::new());
                    }
                    let chars = doc.lines[l].chars().count();
                    if chars < col {
                        doc.lines[l].push_str(&" ".repeat(col - chars));
                    }
                    let at = byte_at(&doc.lines[l], col);
                    doc.lines[l].insert_str(at, &piece.repeat(n));
                }
                doc.cursor = Pos::new(cur.line, byte_at(&doc.lines[cur.line], col));
            }
        }
        doc.anchor = doc.cursor;
        self.marks.insert('.', doc.cursor);
    }

    /// Parses a motion starting at `ks[i]`, repeated `n` times.
    fn motion(&mut self, doc: &mut Document, ks: &[VKey], i: usize, n: usize, counted: bool) -> Target {
        let cur = doc.cursor;
        let last = doc.lines.len() - 1;
        let Some(&k) = ks.get(i) else { return Target::More };
        let repeat = |lines: &[String], f: &dyn Fn(&[String], Pos) -> Pos| {
            let mut p = cur;
            for _ in 0..n {
                p = f(lines, p);
            }
            p
        };
        let vertical = |doc: &mut Document, line: usize| {
            let goal = *doc.goal.get_or_insert_with(|| char_col(&doc.lines[cur.line], cur.col));
            Pos::new(line, byte_at(&doc.lines[line], goal))
        };
        let keep_goal = matches!(k, VKey::Char('j' | 'k') | VKey::Up | VKey::Down | VKey::Ctrl('n' | 'p'));
        if !keep_goal {
            doc.goal = None;
        }
        match k {
            VKey::Char('h') | VKey::Left | VKey::Backspace => {
                let p = repeat(&doc.lines, &|l, p| Pos::new(p.line, prev_boundary(&l[p.line], p.col)));
                Target::To(Pos::new(cur.line, p.col), Kind::Exclusive)
            }
            VKey::Char('l') | VKey::Right | VKey::Char(' ') => {
                let len = doc.lines[cur.line].len();
                Target::To(repeat(&doc.lines, &|l, p| Pos::new(p.line, next_boundary(&l[p.line], p.col).min(len))), Kind::Exclusive)
            }
            VKey::Char('j') | VKey::Down | VKey::Ctrl('n') | VKey::Enter | VKey::Char('+') => {
                let line = (cur.line + n).min(last);
                let p = if matches!(k, VKey::Enter | VKey::Char('+')) { Pos::new(line, first_non_blank(&doc.lines[line])) } else { vertical(doc, line) };
                Target::To(p, Kind::Linewise)
            }
            VKey::Char('k') | VKey::Up | VKey::Ctrl('p') => Target::To(vertical(doc, cur.line.saturating_sub(n)), Kind::Linewise),
            VKey::Char('-') => {
                let line = cur.line.saturating_sub(n);
                Target::To(Pos::new(line, first_non_blank(&doc.lines[line])), Kind::Linewise)
            }
            VKey::Char('w') => Target::To(repeat(&doc.lines, &|l, p| word_forward(l, p, false)), Kind::Exclusive),
            VKey::Char('W') => Target::To(repeat(&doc.lines, &|l, p| word_forward(l, p, true)), Kind::Exclusive),
            VKey::Char('b') => Target::To(repeat(&doc.lines, &|l, p| word_back(l, p, false)), Kind::Exclusive),
            VKey::Char('B') => Target::To(repeat(&doc.lines, &|l, p| word_back(l, p, true)), Kind::Exclusive),
            VKey::Char('e') => Target::To(repeat(&doc.lines, &|l, p| word_end(l, p, false)), Kind::Inclusive),
            VKey::Char('E') => Target::To(repeat(&doc.lines, &|l, p| word_end(l, p, true)), Kind::Inclusive),
            VKey::Char('0') | VKey::Home => Target::To(Pos::new(cur.line, 0), Kind::Exclusive),
            VKey::Char('^') => Target::To(Pos::new(cur.line, first_non_blank(&doc.lines[cur.line])), Kind::Exclusive),
            VKey::Char('$') | VKey::End => {
                let line = (cur.line + n - 1).min(last);
                Target::To(Pos::new(line, last_col(&doc.lines[line])), Kind::Inclusive)
            }
            VKey::Char('G') => {
                let line = if counted { (n - 1).min(last) } else { last };
                Target::To(Pos::new(line, first_non_blank(&doc.lines[line])), Kind::Linewise)
            }
            VKey::Char(c @ ('H' | 'M' | 'L')) => {
                let (top, lines) = if self.view_lines == 0 { (cur.line, 1) } else { (self.view_top, self.view_lines) };
                let bottom = (top + lines - 1).min(last);
                let line = match c {
                    'H' => (top + n - 1).min(bottom),
                    'L' => bottom.saturating_sub(n - 1).max(top),
                    _ => (top + bottom) / 2,
                };
                Target::To(Pos::new(line, first_non_blank(&doc.lines[line])), Kind::Linewise)
            }
            VKey::Char('g') => match ks.get(i + 1) {
                None => Target::More,
                Some(VKey::Char('g')) => {
                    let line = if counted { (n - 1).min(last) } else { 0 };
                    Target::To(Pos::new(line, first_non_blank(&doc.lines[line])), Kind::Linewise)
                }
                Some(VKey::Char('e')) => {
                    let lines = &doc.lines;
                    let p = word_back(lines, cur, false);
                    let q = prev(lines, p).map(|q| if class(ch(lines, q), false) == 0 { prev(lines, q).unwrap_or(q) } else { q }).unwrap_or(p);
                    Target::To(q, Kind::Inclusive)
                }
                Some(VKey::Char('_')) => {
                    let line = (cur.line + n - 1).min(last);
                    let text = &doc.lines[line];
                    Target::To(Pos::new(line, last_col(text.trim_end())), Kind::Inclusive)
                }
                _ => Target::Fail,
            },
            VKey::Char(m @ ('`' | '\'')) => {
                let Some(&VKey::Char(name)) = ks.get(i + 1) else { return if ks.len() > i + 1 { Target::Fail } else { Target::More } };
                let name = if name == '`' { '\'' } else { name };
                let Some(p) = self.marks.get(&name).copied() else {
                    self.message = Some("E20: Mark not set".into());
                    return Target::Fail;
                };
                let p = doc.clamp(p);
                if m == '\'' {
                    Target::To(Pos::new(p.line, first_non_blank(&doc.lines[p.line])), Kind::Linewise)
                } else {
                    Target::To(p, Kind::Exclusive)
                }
            }
            VKey::Char(f @ ('f' | 't' | 'F' | 'T')) => {
                let Some(&c) = ks.get(i + 1) else { return Target::More };
                let VKey::Char(c) = c else { return Target::Fail };
                self.find = Some((f, c));
                match find_char(&doc.lines[cur.line], cur.col, f, c, n, false) {
                    Some(col) => Target::To(Pos::new(cur.line, col), if f.is_ascii_lowercase() { Kind::Inclusive } else { Kind::Exclusive }),
                    None => Target::Fail,
                }
            }
            VKey::Char(r @ (';' | ',')) => {
                let Some((f, c)) = self.find else { return Target::Fail };
                let f = if r == ',' {
                    match f {
                        'f' => 'F',
                        'F' => 'f',
                        't' => 'T',
                        _ => 't',
                    }
                } else {
                    f
                };
                match find_char(&doc.lines[cur.line], cur.col, f, c, n, true) {
                    Some(col) => Target::To(Pos::new(cur.line, col), if f.is_ascii_lowercase() { Kind::Inclusive } else { Kind::Exclusive }),
                    None => Target::Fail,
                }
            }
            VKey::Char('%') => {
                let lines = &doc.lines;
                let line = &lines[cur.line];
                let start = line[cur.col..].char_indices().find(|(_, c)| PAIRS.iter().any(|(o, cl)| c == o || c == cl)).map(|(i, _)| cur.col + i);
                match start.and_then(|col| match_bracket(lines, Pos::new(cur.line, col))) {
                    Some(p) => Target::To(p, Kind::Inclusive),
                    None => Target::Fail,
                }
            }
            VKey::Char('}') => {
                let lines = &doc.lines;
                let mut l = cur.line;
                for _ in 0..n {
                    l += 1;
                    while l < last && !lines[l].trim().is_empty() {
                        l += 1;
                    }
                    l = l.min(last);
                }
                Target::To(Pos::new(l, if l == last { last_col(&lines[l]) } else { 0 }), Kind::Exclusive)
            }
            VKey::Char('{') => {
                let lines = &doc.lines;
                let mut l = cur.line;
                for _ in 0..n {
                    l = l.saturating_sub(1);
                    while l > 0 && !lines[l].trim().is_empty() {
                        l -= 1;
                    }
                }
                Target::To(Pos::new(l, 0), Kind::Exclusive)
            }
            VKey::Char(s @ ('*' | '#')) => {
                let (a, b) = match text_object(&doc.lines, cur, false, 'w') {
                    Some((a, b, _)) if class(ch(&doc.lines, cur), false) == 1 => (a, b),
                    _ => return Target::Fail,
                };
                let word = text_between(&doc.lines, a, b);
                self.search = Some((format!(r"\<{}\>", regex::escape(&word)), s == '*'));
                let from = if s == '*' { cur } else { a };
                match self.find_match(doc, from, true) {
                    Some(p) => Target::To(p, Kind::Exclusive),
                    None => Target::Fail,
                }
            }
            VKey::Char(s @ ('n' | 'N')) => {
                let mut p = cur;
                for _ in 0..n {
                    match self.find_match(doc, p, s == 'n') {
                        Some(q) => p = q,
                        None => return Target::Fail,
                    }
                }
                Target::To(p, Kind::Exclusive)
            }
            _ => Target::Fail,
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Modifiers;

    /// Feeds keys to a document. `<Esc>`, `<CR>`, `<BS>`, `<Tab>` and
    /// `<C-x>` name special keys.
    fn feed(doc: &mut Document, keys: &str) {
        let mut rest = keys;
        while !rest.is_empty() {
            let (key, text, ctrl, len) = if let Some(end) = rest.strip_prefix('<').and_then(|r| r.find('>')) {
                let name = &rest[1..=end];
                let len = end + 2;
                match name {
                    "Esc" => (Key::Escape, None, false, len),
                    "CR" => (Key::Enter, None, false, len),
                    "BS" => (Key::Backspace, None, false, len),
                    "Tab" => (Key::Tab, None, false, len),
                    n if n.starts_with("C-") => (Key::Character(n[2..].to_string()), None, true, len),
                    _ => (Key::Character("<".into()), Some("<".to_string()), false, 1),
                }
            } else {
                let c = rest.chars().next().unwrap();
                let key = if c == ' ' { Key::Space } else { Key::Character(c.to_lowercase().to_string()) };
                (key, Some(c.to_string()), false, c.len_utf8())
            };
            rest = &rest[len..];
            let modifiers = Modifiers { ctrl, ..Default::default() };
            doc.apply(Action::Key(KeyEvent { key, pressed: true, repeat: false, modifiers, text }));
        }
    }

    fn vim(text: &str) -> Document {
        let mut d = Document::new(text);
        d.set_vim(true);
        d
    }

    fn mode(d: &Document) -> Mode {
        d.vim().unwrap().mode
    }

    fn message(d: &Document) -> String {
        d.vim().unwrap().message.unwrap_or_default()
    }

    #[test]
    fn motions() {
        let mut d = vim("let value = foo(bar);\n\nfn next() {}");
        feed(&mut d, "w");
        assert_eq!(d.cursor(), Pos::new(0, 4));
        feed(&mut d, "e");
        assert_eq!(d.cursor(), Pos::new(0, 8));
        feed(&mut d, "$");
        assert_eq!(d.cursor(), Pos::new(0, 20));
        feed(&mut d, "0fb");
        assert_eq!(d.cursor(), Pos::new(0, 16));
        feed(&mut d, "0f(%");
        assert_eq!(d.cursor(), Pos::new(0, 19));
        feed(&mut d, "G");
        assert_eq!(d.cursor(), Pos::new(2, 0));
        feed(&mut d, "gg}");
        assert_eq!(d.cursor(), Pos::new(1, 0));
        feed(&mut d, "3G");
        assert_eq!(d.cursor().line, 2);
        feed(&mut d, "b");
        assert_eq!(d.cursor(), Pos::new(1, 0), "b stops on an empty line");
    }

    #[test]
    fn delete_change_yank_put() {
        let mut d = vim("one two three\nfour");
        feed(&mut d, "dw");
        assert_eq!(d.lines()[0], "two three");
        feed(&mut d, "cwTWO<Esc>");
        assert_eq!(d.lines()[0], "TWO three");
        assert_eq!(mode(&d), Mode::Normal);
        feed(&mut d, "ddp");
        assert_eq!(d.text(), "four\nTWO three");
        feed(&mut d, "yyP");
        assert_eq!(d.text(), "four\nTWO three\nTWO three");
        feed(&mut d, "gg3x");
        assert_eq!(d.lines()[0], "r");
        feed(&mut d, "u");
        assert_eq!(d.lines()[0], "four");
    }

    #[test]
    fn counts_and_dot_repeat() {
        let mut d = vim("a\nb\nc\nd\ne");
        feed(&mut d, "2dd");
        assert_eq!(d.text(), "c\nd\ne");
        feed(&mut d, ".");
        assert_eq!(d.text(), "e");
        let mut d = vim("x1 x2 x3");
        feed(&mut d, "ciwy<Esc>w.w.");
        assert_eq!(d.text(), "y y y");
        let mut d = vim("abc");
        feed(&mut d, "Ahi<Esc>.");
        assert_eq!(d.text(), "abchihi");
    }

    #[test]
    fn dot_takes_a_new_count_and_repeats_visual_changes() {
        let mut d = vim("abcdefgh");
        feed(&mut d, "2x3.");
        assert_eq!(d.text(), "fgh");
        let mut d = vim("one\ntwo\nthree\nfour");
        feed(&mut d, "Vj>j.");
        assert_eq!(d.text(), "    one\n        two\n    three\nfour");
        let mut d = vim("aaaa bbbb");
        feed(&mut d, "vlUw.");
        assert_eq!(d.text(), "AAaa BBbb");
    }

    #[test]
    fn counted_insert() {
        let mut d = vim("");
        feed(&mut d, "3ia<Esc>");
        assert_eq!(d.text(), "aaa");
        let mut d = vim("x");
        feed(&mut d, "2oy<Esc>");
        assert_eq!(d.text(), "x\ny\ny");
    }

    #[test]
    fn insert_session_is_one_undo_step() {
        let mut d = vim("fn main() {");
        feed(&mut d, "A<CR>let x = 1;<CR>x<Esc>");
        assert_eq!(d.text(), "fn main() {\n    let x = 1;\n    x");
        feed(&mut d, "u");
        assert_eq!(d.text(), "fn main() {");
        feed(&mut d, "<C-r>");
        assert_eq!(d.lines().len(), 3);
    }

    #[test]
    fn text_objects() {
        let mut d = vim(r#"call(a, "b c", [d])"#);
        feed(&mut d, "fbdi\"");
        assert_eq!(d.text(), r#"call(a, "", [d])"#);
        feed(&mut d, "0f(di(");
        assert_eq!(d.text(), "call()");
        let mut d = vim("fn f() {\n    body();\n}");
        feed(&mut d, "jdi{");
        assert_eq!(d.text(), "fn f() {\n}");
        let mut d = vim("one two three");
        feed(&mut d, "wdaw");
        assert_eq!(d.text(), "one three");
    }

    #[test]
    fn visual_modes() {
        let mut d = vim("alpha beta\ngamma");
        feed(&mut d, "vey");
        assert_eq!(mode(&d), Mode::Normal);
        feed(&mut d, "$p");
        assert_eq!(d.lines()[0], "alpha betaalpha");
        feed(&mut d, "Vjd");
        assert_eq!(d.text(), "");
        let mut d = vim("keep\ndrop\nkeep");
        feed(&mut d, "jVd");
        assert_eq!(d.text(), "keep\nkeep");
        let mut d = vim("a\nb");
        feed(&mut d, "Vj>");
        assert_eq!(d.text(), "    a\n    b");
    }

    #[test]
    fn misc_commands() {
        let mut d = vim("hello\n    world");
        feed(&mut d, "J");
        assert_eq!(d.text(), "hello world");
        feed(&mut d, "0rj~");
        assert_eq!(d.text(), "Jello world");
        feed(&mut d, "oend<Esc>");
        assert_eq!(d.text(), "Jello world\nend");
        feed(&mut d, ">>");
        assert_eq!(d.lines()[1], "    end");
        feed(&mut d, "gUiw");
        assert_eq!(d.lines()[1], "    END");
        feed(&mut d, "kgJ");
        assert_eq!(d.text(), "Jello world    END");
    }

    #[test]
    fn search_and_command_line() {
        let mut d = vim("foo bar\nbaz foo\nfoo");
        feed(&mut d, "/foo<CR>");
        assert_eq!(d.cursor(), Pos::new(1, 4));
        feed(&mut d, "n");
        assert_eq!(d.cursor(), Pos::new(2, 0));
        feed(&mut d, "n");
        assert_eq!(d.cursor(), Pos::new(0, 0));
        assert!(message(&d).contains("BOTTOM"));
        feed(&mut d, "N");
        assert_eq!(d.cursor(), Pos::new(2, 0));
        feed(&mut d, ":2<CR>");
        assert_eq!(d.cursor().line, 1);
        feed(&mut d, ":wq<CR>");
        assert_eq!(d.take_vim_requests(), vec![VimRequest::WriteQuit]);
        feed(&mut d, ":q!<CR>");
        assert_eq!(d.take_vim_requests(), vec![VimRequest::Quit { force: true }]);
        feed(&mut d, ":nope<CR>");
        assert!(message(&d).contains("Not an editor command"));
        feed(&mut d, "/zzz<CR>");
        assert!(message(&d).contains("Pattern not found"));
    }

    #[test]
    fn regex_search() {
        let mut d = vim("let a1 = 1;\nlet b22 = 22;\nnothing");
        feed(&mut d, "/[a-z]\\d{2}<CR>");
        assert_eq!(d.cursor(), Pos::new(1, 4));
        feed(&mut d, "gg/\\<22\\><CR>");
        assert_eq!(d.cursor(), Pos::new(1, 10));
        feed(&mut d, "gg/NOTHING\\c<CR>");
        assert_eq!(d.cursor(), Pos::new(2, 0));
        feed(&mut d, "gg*");
        assert_eq!(d.cursor(), Pos::new(1, 0));
    }

    #[test]
    fn substitute() {
        let mut d = vim("foo foo\nbar foo\nfoo");
        feed(&mut d, ":s/foo/X/<CR>");
        assert_eq!(d.text(), "X foo\nbar foo\nfoo");
        feed(&mut d, ":%s/foo/[&]/g<CR>");
        assert_eq!(d.text(), "X [foo]\nbar [foo]\n[foo]");
        assert!(message(&d).contains("3 substitutions on 3 lines"));
        feed(&mut d, "u");
        assert_eq!(d.text(), "X foo\nbar foo\nfoo");
        feed(&mut d, ":2,3s/(\\w+) foo/\\1\\nqux/<CR>");
        assert_eq!(d.text(), "X foo\nbar\nqux\nfoo");
        feed(&mut d, "ggVj:s/o/0/g<CR>");
        assert_eq!(d.text(), "X f00\nbar\nqux\nfoo");
        feed(&mut d, ":%s/zzz/y/<CR>");
        assert!(message(&d).contains("Pattern not found"));
        feed(&mut d, ":$d<CR>");
        assert_eq!(d.text(), "X f00\nbar\nqux");
    }

    #[test]
    fn registers() {
        let mut d = vim("alpha\nbeta\ngamma");
        feed(&mut d, "\"ayyj\"byy");
        feed(&mut d, "G\"ap\"bp");
        assert_eq!(d.text(), "alpha\nbeta\ngamma\nalpha\nbeta");
        feed(&mut d, "gg\"Ayy");
        feed(&mut d, "G\"ap");
        assert_eq!(d.lines()[5..], ["alpha".to_string(), "alpha".to_string()]);
        // Deletes go to "1 and shift; yanks go to "0.
        let mut d = vim("one\ntwo\nthree");
        feed(&mut d, "yyjddjdd");
        feed(&mut d, "\"0p");
        assert_eq!(d.text(), "one\none");
        feed(&mut d, "\"2p");
        assert_eq!(d.lines().last().unwrap(), "two");
        // The black hole register keeps the unnamed register.
        let mut d = vim("keep drop");
        feed(&mut d, "yiww\"_dw0P");
        assert_eq!(d.text(), "keepkeep ");
    }

    #[test]
    fn clipboard_register() {
        let mut d = vim("hello world");
        feed(&mut d, "\"+yiw");
        let (serial, text) = d.vim_view().unwrap().clipboard.unwrap();
        assert_eq!(text, "hello");
        d.apply(Action::Clipboard("from outside".into()));
        feed(&mut d, "$\"+p");
        assert_eq!(d.text(), "hello worldfrom outside");
        feed(&mut d, ":set clipboard=unnamedplus<CR>yy");
        let (serial2, text) = d.vim_view().unwrap().clipboard.unwrap();
        assert!(serial2 > serial);
        assert_eq!(text, "hello worldfrom outside\n");
        assert_eq!(d.vim_view().unwrap().clipboard_need, ClipboardNeed::OnPaste);
    }

    #[test]
    fn insert_mode_register_paste() {
        let mut d = vim("word");
        feed(&mut d, "yiwA <C-r>\"<Esc>");
        assert_eq!(d.text(), "word word");
    }

    #[test]
    fn macros() {
        let mut d = vim("1\n2\n3\n4");
        feed(&mut d, "qaA!<Esc>jq");
        assert_eq!(d.text(), "1!\n2\n3\n4");
        assert_eq!(d.vim().unwrap().recording, None);
        feed(&mut d, "@a");
        assert_eq!(d.text(), "1!\n2!\n3\n4");
        feed(&mut d, "2@@");
        assert_eq!(d.text(), "1!\n2!\n3!\n4!");
        // A failing motion stops the macro.
        let mut d = vim("a b\nc");
        feed(&mut d, "qqdwq");
        feed(&mut d, "5@q");
        assert_eq!(d.text(), "\nc");
        // Macros are registers: yanked text runs as keys.
        let mut d = vim("x\nhello");
        feed(&mut d, "\"ayiwj0@a");
        assert_eq!(d.text(), "x\nello");
    }

    #[test]
    fn marks() {
        let mut d = vim("a\nb\nc\nd");
        feed(&mut d, "jmxG'x");
        assert_eq!(d.cursor().line, 1);
        feed(&mut d, "''");
        assert_eq!(d.cursor().line, 3);
        feed(&mut d, "ggdd'x");
        assert_eq!(d.cursor().line, 0, "marks follow deleted lines");
        feed(&mut d, "d'x");
        assert_eq!(d.text(), "c\nd");
        let mut d = vim("abc def");
        feed(&mut d, "wmmd`m");
        assert_eq!(d.text(), "abc def", "`m is exclusive: nothing between cursor and mark");
        feed(&mut d, "0d`m");
        assert_eq!(d.text(), "def");
    }

    #[test]
    fn visual_block() {
        let mut d = vim("abcd\nefgh\nijkl");
        feed(&mut d, "l<C-v>jld");
        assert_eq!(d.text(), "ad\neh\nijkl");
        feed(&mut d, "u0<C-v>jjI# <Esc>");
        assert_eq!(d.text(), "# abcd\n# efgh\n# ijkl");
        let mut d = vim("ab\ncd");
        feed(&mut d, "<C-v>j$A;<Esc>");
        assert_eq!(d.text(), "ab;\ncd;");
        let mut d = vim("1234\n5678");
        feed(&mut d, "l<C-v>jlyGo<Esc>p");
        assert_eq!(d.text(), "1234\n5678\n23\n67");
        let mut d = vim("aaa\nbbb");
        feed(&mut d, "<C-v>jlrx");
        assert_eq!(d.text(), "xxa\nxxb");
        let mut d = vim("one\ntwo");
        feed(&mut d, "<C-v>jcX<Esc>");
        assert_eq!(d.text(), "Xne\nXwo");
    }

    #[test]
    fn scrolling_uses_the_viewport() {
        let text: String = (0..100).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        let mut d = vim(&text);
        d.apply(Action::Viewport { top: 0, lines: 20 });
        feed(&mut d, "<C-d>");
        assert_eq!(d.cursor().line, 10);
        assert_eq!(d.vim_view().unwrap().scroll.unwrap().1, Scroll::Lines(10));
        feed(&mut d, "L");
        assert_eq!(d.cursor().line, 19);
        feed(&mut d, "zz");
        assert_eq!(d.vim_view().unwrap().scroll.unwrap().1, Scroll::Center);
        feed(&mut d, "<C-u>");
        assert_eq!(d.cursor().line, 9);
    }

    #[test]
    fn normal_mode_keeps_cursor_on_a_character() {
        let mut d = vim("abc");
        feed(&mut d, "$l");
        assert_eq!(d.cursor(), Pos::new(0, 2));
        feed(&mut d, "a!<Esc>");
        assert_eq!(d.text(), "abc!");
        assert_eq!(d.cursor(), Pos::new(0, 3));
    }

    #[test]
    fn macro_text_round_trips_special_keys() {
        for k in [VKey::Esc, VKey::Enter, VKey::Ctrl('r'), VKey::Left, VKey::PageDown, VKey::Char('é')] {
            assert_eq!(VKey::from_char(k.to_char()), k);
        }
    }
}
