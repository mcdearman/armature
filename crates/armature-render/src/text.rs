use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Weight, Wrap};

use crate::geometry::{Point, Size};

/// Which bundled typeface to use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FontFamily {
    #[default]
    Sans,
    Mono,
    Icons,
}

/// Everything that affects how a string is shaped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    /// Font size in logical pixels.
    pub size: f32,
    pub weight: u16,
    pub family: FontFamily,
    /// Line height as a multiple of `size`.
    pub line_height: f32,
    /// Extra space between letters in logical pixels.
    pub letter_spacing: f32,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self { size: 14.0, weight: 500, family: FontFamily::Sans, line_height: 1.4, letter_spacing: 0.0 }
    }
}

/// The typefaces a renderer draws text with. The system's own fonts are
/// always available as a fallback, for emoji, CJK and so on.
#[derive(Clone, Debug, Default)]
pub struct Fonts {
    /// Font files to load.
    pub data: Vec<std::borrow::Cow<'static, [u8]>>,
    /// Family name for [`FontFamily::Sans`]. Empty uses the system's.
    pub sans: String,
    /// Family name for [`FontFamily::Mono`]. Empty uses the system's.
    pub mono: String,
    /// Family name for [`FontFamily::Icons`]. Empty uses the sans family.
    pub icons: String,
}

impl Fonts {
    /// No fonts of its own: text is drawn with whatever the system has.
    pub fn system() -> Self {
        Self::default()
    }
}

/// Shaped text, cheap to clone.
#[derive(Clone)]
pub struct TextLayout {
    pub(crate) buffer: Rc<Buffer>,
    size: Size,
    line_height: f32,
}

impl std::fmt::Debug for TextLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextLayout").field("size", &self.size).finish()
    }
}

impl TextLayout {
    /// The bounding size of the laid-out text.
    pub fn size(&self) -> Size {
        self.size
    }

    pub fn line_height(&self) -> f32 {
        self.line_height
    }

    /// The byte index in the source string closest to `p` (relative to the
    /// layout's top-left corner).
    pub fn hit(&self, p: Point) -> usize {
        match self.buffer.hit(p.x, p.y) {
            Some(c) => {
                // Single-paragraph layouts only; add line offsets for multi-line text.
                let mut offset = 0;
                for (i, line) in self.buffer.lines.iter().enumerate() {
                    if i == c.line {
                        return offset + c.index;
                    }
                    offset += line.text().len() + 1;
                }
                offset
            }
            None => 0,
        }
    }

    /// The caret position (top-left, relative to the layout) before byte `index`.
    pub fn caret(&self, index: usize) -> Point {
        let mut last = Point::ZERO;
        for run in self.buffer.layout_runs() {
            for g in run.glyphs {
                if g.start >= index {
                    return Point::new(g.x, run.line_top);
                }
                last = Point::new(g.x + g.w, run.line_top);
            }
            if run.glyphs.is_empty() {
                last = Point::new(0.0, run.line_top);
            }
        }
        last
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    text: String,
    /// Byte length and RGBA of each coloured span; empty for plain text.
    spans: Vec<(usize, [u8; 4])>,
    size: u32,
    weight: u16,
    family: FontFamily,
    line_height: u32,
    spacing: u32,
    max_width: Option<u32>,
}

struct Entry {
    layout: TextLayout,
    last_used: u64,
}

/// Owns the font database and caches shaped text between frames.
pub struct TextSystem {
    pub(crate) fonts: FontSystem,
    names: Fonts,
    cache: HashMap<u64, Vec<(Key, Entry)>>,
    frame: u64,
}

impl TextSystem {
    /// Loads `names.data` plus system fonts for fallback (emoji, CJK, ...).
    pub fn new(mut names: Fonts) -> Self {
        let mut fonts = FontSystem::new();
        let db = fonts.db_mut();
        for data in std::mem::take(&mut names.data) {
            db.load_font_data(data.into_owned());
        }
        if !names.sans.is_empty() {
            db.set_sans_serif_family(names.sans.clone());
        }
        if !names.mono.is_empty() {
            db.set_monospace_family(names.mono.clone());
        }
        Self { fonts, names, cache: HashMap::new(), frame: 0 }
    }

    /// Shapes `text`, wrapping at `max_width` when given.
    pub fn layout(&mut self, text: &str, style: &TextStyle, max_width: Option<f32>) -> TextLayout {
        self.layout_inner(text, &[], style, max_width)
    }

    /// Shapes coloured spans as one run of text, for syntax highlighting.
    /// Spans with no colour of their own use the colour passed when drawing.
    pub fn layout_spans(&mut self, spans: &[(&str, Option<crate::Color>)], style: &TextStyle, max_width: Option<f32>) -> TextLayout {
        let text: String = spans.iter().map(|(t, _)| *t).collect();
        let colors: Vec<(usize, [u8; 4])> = spans.iter().map(|(t, c)| (t.len(), c.map_or([0; 4], |c| c.to_rgba8()))).collect();
        self.layout_inner(&text, &colors, style, max_width)
    }

    fn layout_inner(&mut self, text: &str, spans: &[(usize, [u8; 4])], style: &TextStyle, max_width: Option<f32>) -> TextLayout {
        let key = Key {
            text: text.to_owned(),
            spans: spans.to_vec(),
            size: style.size.to_bits(),
            weight: style.weight,
            family: style.family,
            line_height: style.line_height.to_bits(),
            spacing: style.letter_spacing.to_bits(),
            max_width: max_width.filter(|w| w.is_finite()).map(|w| w.to_bits()),
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h);
        let hash = h.finish();
        let frame = self.frame;
        if let Some(bucket) = self.cache.get_mut(&hash)
            && let Some((_, e)) = bucket.iter_mut().find(|(k, _)| *k == key) {
                e.last_used = frame;
                return e.layout.clone();
            }

        let line_height = (style.size * style.line_height).round();
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(style.size, line_height));
        let family = match style.family {
            FontFamily::Sans => Family::SansSerif,
            FontFamily::Mono => Family::Monospace,
            FontFamily::Icons if self.names.icons.is_empty() => Family::SansSerif,
            FontFamily::Icons => Family::Name(&self.names.icons),
        };
        let attrs = Attrs::new()
            .family(family)
            .weight(Weight(style.weight))
            .letter_spacing(style.letter_spacing / style.size.max(1.0));
        let width = max_width.filter(|w| w.is_finite());
        buffer.set_wrap(if width.is_some() { Wrap::WordOrGlyph } else { Wrap::None });
        buffer.set_size(width, None);
        if spans.is_empty() {
            buffer.set_text(text, &attrs, Shaping::Advanced, None);
        } else {
            let mut at = 0;
            let rich = spans.iter().map(|(len, rgba)| {
                let t = &text[at..at + len];
                at += len;
                let a = if rgba[3] == 0 { attrs.clone() } else { attrs.clone().color(glyphon::Color::rgba(rgba[0], rgba[1], rgba[2], rgba[3])) };
                (t, a)
            });
            buffer.set_rich_text(rich, &attrs, Shaping::Advanced, None);
        }
        buffer.shape_until_scroll(&mut self.fonts, false);

        let mut w: f32 = 0.0;
        let mut lines = 0;
        for run in buffer.layout_runs() {
            // Trailing letter spacing is not part of the visible width.
            w = w.max(run.line_w - if run.glyphs.is_empty() { 0.0 } else { style.letter_spacing });
            lines += 1;
        }
        let lines = lines.max(1);
        let layout = TextLayout {
            buffer: Rc::new(buffer),
            size: Size::new(w.ceil().max(0.0), line_height * lines as f32),
            line_height,
        };
        self.cache.entry(hash).or_default().push((key, Entry { layout: layout.clone(), last_used: frame }));
        layout
    }

    /// Drops cached layouts that went unused for a while.
    pub fn end_frame(&mut self) {
        self.frame += 1;
        let frame = self.frame;
        self.cache.retain(|_, bucket| {
            bucket.retain(|(_, e)| frame - e.last_used < 120);
            !bucket.is_empty()
        });
    }
}
