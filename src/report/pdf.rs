//! Deterministic PDF rendering of the canonical report markdown.
//!
//! The PDF is a presentation twin of `REPORT.md`, produced with no model
//! call, no clock, and no randomness: the same markdown always yields the
//! same bytes, so the artifact can be re-derived and compared forever.
//! It is deliberately dependency-light — a minimal hand-written PDF 1.4
//! writer over three of the standard 14 fonts (Helvetica, Helvetica-Bold,
//! Courier), whose metrics every conforming reader ships, so nothing is
//! embedded and the output stays small and portable.
//!
//! # Safety with untrusted text
//!
//! Report bodies contain model-generated text. All of it lands inside PDF
//! string literals: the delimiter set (`\`, `(`, `)`) is escaped, control
//! characters are dropped, and characters outside WinAnsi degrade to `?`.
//! Text can therefore never inject PDF operators, whatever it contains.
//!
//! # What is interpreted
//!
//! Exactly the markdown the report template emits: `#`–`####` headings,
//! `-`/`*` bullets (two spaces per nesting level), `>` quotes, fenced code
//! blocks, `---` rules, paragraphs, and inline `**bold**` and `` `code` ``
//! runs. Anything else renders as plain text. Layout is fixed: A4, 72 pt
//! margins, greedy word wrap against the font width tables, and a
//! hard character split for words wider than a whole line.

// --- fixed page geometry (points) ---
const PAGE_W: f32 = 595.0;
const PAGE_H: f32 = 842.0;
const MARGIN: f32 = 72.0;
const TEXT_W: f32 = PAGE_W - 2.0 * MARGIN;
const FOOTER_Y: f32 = 40.0;

/// Render the report markdown into a complete, self-contained PDF.
///
/// Pure and infallible: every input, including hostile or non-WinAnsi
/// text, renders to a valid document.
pub fn render_report_pdf(markdown: &str) -> Vec<u8> {
    let blocks = parse(markdown);
    let mut pages = layout(&blocks);
    add_footers(&mut pages);
    assemble(&pages)
}

// --- markdown model ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Body,
    Bold,
    Code,
}

#[derive(Debug, Clone)]
struct Span {
    style: Style,
    text: String,
}

#[derive(Debug)]
enum Block {
    Heading { level: u8, spans: Vec<Span> },
    Paragraph { spans: Vec<Span> },
    Bullet { depth: u8, spans: Vec<Span> },
    Quote { spans: Vec<Span> },
    CodeLine(String),
    Rule,
}

/// Line-based markdown parse, covering the constructs the report template
/// emits. Unterminated fences or markers degrade to plain text; nothing
/// rejects.
fn parse(markdown: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut quote: Vec<String> = Vec::new();
    let mut in_code = false;

    let flush_paragraph = |blocks: &mut Vec<Block>, buf: &mut Vec<String>| {
        if !buf.is_empty() {
            blocks.push(Block::Paragraph {
                spans: inline_spans(&buf.join(" ")),
            });
            buf.clear();
        }
    };
    let flush_quote = |blocks: &mut Vec<Block>, buf: &mut Vec<String>| {
        if !buf.is_empty() {
            blocks.push(Block::Quote {
                spans: inline_spans(&buf.join(" ")),
            });
            buf.clear();
        }
    };

    for raw in markdown.lines() {
        let line = raw.trim_end();
        if in_code {
            if line.trim() == "```" {
                in_code = false;
            } else {
                blocks.push(Block::CodeLine(raw.replace('\t', "    ")));
            }
            continue;
        }

        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            flush_paragraph(&mut blocks, &mut paragraph);
            flush_quote(&mut blocks, &mut quote);
            in_code = true;
            continue;
        }
        if line.is_empty() {
            flush_paragraph(&mut blocks, &mut paragraph);
            flush_quote(&mut blocks, &mut quote);
            continue;
        }
        if let Some((level, text)) = heading_of(trimmed) {
            flush_paragraph(&mut blocks, &mut paragraph);
            flush_quote(&mut blocks, &mut quote);
            blocks.push(Block::Heading {
                level,
                spans: inline_spans(text),
            });
            continue;
        }
        if trimmed == "---" || trimmed == "***" {
            flush_paragraph(&mut blocks, &mut paragraph);
            flush_quote(&mut blocks, &mut quote);
            blocks.push(Block::Rule);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('>') {
            flush_paragraph(&mut blocks, &mut paragraph);
            quote.push(rest.trim_start().to_string());
            continue;
        }
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            flush_paragraph(&mut blocks, &mut paragraph);
            flush_quote(&mut blocks, &mut quote);
            let indent = line.len() - trimmed.len();
            blocks.push(Block::Bullet {
                depth: u8::try_from(indent / 2).unwrap_or(4).min(4),
                spans: inline_spans(rest),
            });
            continue;
        }
        flush_quote(&mut blocks, &mut quote);
        paragraph.push(trimmed.to_string());
    }
    flush_paragraph(&mut blocks, &mut paragraph);
    flush_quote(&mut blocks, &mut quote);
    blocks
}

/// `## heading` → `(2, "heading")`; levels past four share one style.
fn heading_of(line: &str) -> Option<(u8, &str)> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = line[hashes..].strip_prefix(' ')?;
    Some((u8::try_from(hashes).unwrap_or(6), rest))
}

/// Split inline text on `**bold**` and `` `code` `` markers. Markers
/// toggle; an unbalanced marker simply styles the rest of the line, which
/// is cosmetic and cannot corrupt output.
fn inline_spans(text: &str) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut bold = false;
    let mut code = false;

    let push = |spans: &mut Vec<Span>, buf: &mut String, bold: bool, code: bool| {
        if buf.is_empty() {
            return;
        }
        let style = if code {
            Style::Code
        } else if bold {
            Style::Bold
        } else {
            Style::Body
        };
        spans.push(Span {
            style,
            text: std::mem::take(buf),
        });
    };

    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '*' && chars.peek() == Some(&'*') && !code {
            chars.next();
            push(&mut spans, &mut buf, bold, code);
            bold = !bold;
        } else if c == '`' {
            push(&mut spans, &mut buf, bold, code);
            code = !code;
        } else {
            buf.push(c);
        }
    }
    push(&mut spans, &mut buf, bold, code);
    spans
}

// --- fonts and metrics ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Font {
    Helvetica,
    HelveticaBold,
    Courier,
}

impl Font {
    fn resource(self) -> &'static str {
        match self {
            Font::Helvetica => "/F1",
            Font::HelveticaBold => "/F2",
            Font::Courier => "/F3",
        }
    }
}

/// Which concrete font a styled run uses, given the block's base font
/// (bold for headings, regular otherwise).
fn font_for(style: Style, base: Font) -> Font {
    match style {
        Style::Code => Font::Courier,
        Style::Bold => Font::HelveticaBold,
        Style::Body => base,
    }
}

/// Helvetica AFM widths for `0x20..=0x7E`, in 1/1000 em.
#[rustfmt::skip]
const HELVETICA_WIDTHS: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278,
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556,
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778,
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556,
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556,
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

/// Helvetica-Bold AFM widths for `0x20..=0x7E`, in 1/1000 em.
#[rustfmt::skip]
const HELVETICA_BOLD_WIDTHS: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278,
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611,
    975, 722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778,
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 333, 278, 333, 584, 556,
    333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889, 611, 611,
    611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
];

/// Glyph advance in 1/1000 em for the WinAnsi byte this character encodes
/// to. Extended-range widths are approximated (Helvetica average); wrap
/// points shift slightly but can never overflow the page by much more
/// than one glyph.
fn glyph_width(font: Font, c: char) -> u16 {
    if font == Font::Courier {
        return 600;
    }
    let byte = winansi_byte(c).unwrap_or(b'?');
    let table = match font {
        Font::Helvetica => &HELVETICA_WIDTHS,
        Font::HelveticaBold => &HELVETICA_BOLD_WIDTHS,
        Font::Courier => unreachable!("handled above"),
    };
    match byte {
        0x20..=0x7E => table[usize::from(byte - 0x20)],
        _ => 556,
    }
}

fn text_width(text: &str, font: Font, size: f32) -> f32 {
    let units: u32 = text.chars().map(|c| u32::from(glyph_width(font, c))).sum();
    units as f32 * size / 1000.0
}

/// Encode a character as its WinAnsi (CP1252) byte, or `None` when the
/// character has no WinAnsi form.
fn winansi_byte(c: char) -> Option<u8> {
    let u = c as u32;
    match u {
        0x20..=0x7E | 0xA0..=0xFF => u8::try_from(u).ok(),
        _ => match c {
            '\u{20AC}' => Some(0x80), // €
            '\u{201A}' => Some(0x82),
            '\u{0192}' => Some(0x83),
            '\u{201E}' => Some(0x84),
            '\u{2026}' => Some(0x85), // …
            '\u{2020}' => Some(0x86),
            '\u{2021}' => Some(0x87),
            '\u{02C6}' => Some(0x88),
            '\u{2030}' => Some(0x89),
            '\u{0160}' => Some(0x8A),
            '\u{2039}' => Some(0x8B),
            '\u{0152}' => Some(0x8C),
            '\u{017D}' => Some(0x8E),
            '\u{2018}' => Some(0x91), // '
            '\u{2019}' => Some(0x92), // '
            '\u{201C}' => Some(0x93), // "
            '\u{201D}' => Some(0x94), // "
            '\u{2022}' => Some(0x95), // •
            '\u{2013}' => Some(0x96), // –
            '\u{2014}' => Some(0x97), // —
            '\u{02DC}' => Some(0x98),
            '\u{2122}' => Some(0x99), // ™
            '\u{0161}' => Some(0x9A),
            '\u{203A}' => Some(0x9B),
            '\u{0153}' => Some(0x9C),
            '\u{017E}' => Some(0x9E),
            '\u{0178}' => Some(0x9F),
            _ => None,
        },
    }
}

// --- layout ---

/// One same-font run of text within a line.
#[derive(Debug, Clone)]
struct Frag {
    font: Font,
    text: String,
}

/// A word is an unbreakable (except by hard split) sequence of same- or
/// mixed-font fragments with no spaces inside.
type Word = Vec<Frag>;

#[derive(Debug)]
enum Item {
    Text {
        x: f32,
        y: f32,
        size: f32,
        frags: Vec<Frag>,
    },
    Rule {
        y: f32,
    },
}

/// Accumulates positioned items page by page, breaking when a line no
/// longer fits above the bottom margin.
struct Pager {
    pages: Vec<Vec<Item>>,
    current: Vec<Item>,
    y: f32,
}

impl Pager {
    fn new() -> Self {
        Self {
            pages: Vec::new(),
            current: Vec::new(),
            y: PAGE_H - MARGIN,
        }
    }

    /// Vertical gap before a block; swallowed at the top of a page.
    fn space_before(&mut self, gap: f32) {
        if !self.current.is_empty() {
            self.y -= gap;
        }
    }

    /// Reserve one line of `leading`, breaking the page first if needed,
    /// and return the line's baseline y.
    fn take_line(&mut self, leading: f32) -> f32 {
        if self.y - leading < MARGIN {
            self.pages.push(std::mem::take(&mut self.current));
            self.y = PAGE_H - MARGIN;
        }
        self.y -= leading;
        self.y
    }

    /// Start a fresh page when a short block plus the first line following it
    /// would otherwise be orphaned. Oversized blocks paginate normally rather
    /// than creating a leading empty page.
    fn keep_together(&mut self, height: f32) {
        let page_capacity = PAGE_H - 2.0 * MARGIN;
        if !self.current.is_empty() && height <= page_capacity && self.y - height < MARGIN {
            self.pages.push(std::mem::take(&mut self.current));
            self.y = PAGE_H - MARGIN;
        }
    }

    fn finish(mut self) -> Vec<Vec<Item>> {
        self.pages.push(self.current);
        self.pages
    }
}

/// Per-block typography: base font, size, leading, space before/after,
/// and left indent.
struct BlockStyle {
    base: Font,
    size: f32,
    leading: f32,
    before: f32,
    after: f32,
    indent: f32,
}

fn block_style(block: &Block) -> BlockStyle {
    match block {
        Block::Heading { level, .. } => match level {
            1 => style(Font::HelveticaBold, 18.0, 23.0, 4.0, 10.0, 0.0),
            2 => style(Font::HelveticaBold, 13.5, 18.0, 14.0, 7.0, 0.0),
            3 => style(Font::HelveticaBold, 11.5, 16.0, 10.0, 5.0, 0.0),
            _ => style(Font::HelveticaBold, 10.5, 15.0, 8.0, 4.0, 0.0),
        },
        Block::Paragraph { .. } => style(Font::Helvetica, 10.0, 14.0, 0.0, 6.0, 0.0),
        Block::Bullet { depth, .. } => style(
            Font::Helvetica,
            10.0,
            14.0,
            0.0,
            3.0,
            14.0 * f32::from(*depth),
        ),
        Block::Quote { .. } => style(Font::Helvetica, 10.0, 14.0, 2.0, 8.0, 16.0),
        Block::CodeLine(_) => style(Font::Courier, 9.0, 11.5, 0.0, 0.0, 12.0),
        Block::Rule => style(Font::Helvetica, 10.0, 14.0, 0.0, 0.0, 0.0),
    }
}

fn style(base: Font, size: f32, leading: f32, before: f32, after: f32, indent: f32) -> BlockStyle {
    BlockStyle {
        base,
        size,
        leading,
        before,
        after,
        indent,
    }
}

fn layout(blocks: &[Block]) -> Vec<Vec<Item>> {
    let mut pager = Pager::new();
    for (index, block) in blocks.iter().enumerate() {
        let st = block_style(block);
        match block {
            Block::Heading { spans, .. } => {
                // Keep a heading (including wrapped heading lines) with at
                // least one line of the block after it whenever possible.
                let avail = (TEXT_W - st.indent).max(st.size);
                let heading_lines =
                    wrap_words(&split_words(spans, st.base), avail, st.base, st.size).len();
                let next_line = blocks
                    .get(index + 1)
                    .map(block_style)
                    .map_or(0.0, |next| next.before + next.leading);
                pager.keep_together(
                    st.before + heading_lines as f32 * st.leading + st.after + next_line,
                );
                flow_spans(&mut pager, spans, &st, None);
            }
            Block::Paragraph { spans } | Block::Quote { spans } => {
                flow_spans(&mut pager, spans, &st, None);
            }
            Block::Bullet { spans, .. } => {
                let hang = text_width("\u{2022} ", Font::Helvetica, st.size);
                flow_spans(&mut pager, spans, &st, Some(hang));
            }
            Block::CodeLine(text) => {
                flow_code_line(&mut pager, text, &st);
            }
            Block::Rule => {
                pager.space_before(6.0);
                let y = pager.take_line(10.0) + 4.0;
                pager.current.push(Item::Rule { y });
                pager.y -= 4.0;
            }
        }
    }
    pager.finish()
}

/// Wrap and place one text block; `hang` carries the bullet hanging
/// indent (text column offset) when the block is a bullet.
fn flow_spans(pager: &mut Pager, spans: &[Span], st: &BlockStyle, hang: Option<f32>) {
    pager.space_before(st.before);
    let text_x = MARGIN + st.indent + hang.unwrap_or(0.0);
    let avail = (TEXT_W - st.indent - hang.unwrap_or(0.0)).max(st.size);
    let lines = wrap_words(&split_words(spans, st.base), avail, st.base, st.size);

    for (i, words) in lines.iter().enumerate() {
        let y = pager.take_line(st.leading);
        if i == 0
            && let Some(_hang) = hang
        {
            pager.current.push(Item::Text {
                x: MARGIN + st.indent,
                y,
                size: st.size,
                frags: vec![Frag {
                    font: Font::Helvetica,
                    text: "\u{2022} ".into(),
                }],
            });
        }
        pager.current.push(Item::Text {
            x: text_x,
            y,
            size: st.size,
            frags: join_words(words, st.base),
        });
    }
    pager.y -= st.after;
}

/// Code lines keep their internal spacing: no word wrap, only a hard
/// character split against the Courier advance width.
fn flow_code_line(pager: &mut Pager, text: &str, st: &BlockStyle) {
    let avail = (TEXT_W - st.indent).max(st.size);
    let word: Word = vec![Frag {
        font: Font::Courier,
        text: text.to_string(),
    }];
    let chunks = if word_width(&word, st.size) <= avail {
        vec![word]
    } else {
        split_word(&word, avail, st.size)
    };
    for chunk in chunks {
        let y = pager.take_line(st.leading);
        pager.current.push(Item::Text {
            x: MARGIN + st.indent,
            y,
            size: st.size,
            frags: chunk,
        });
    }
}

/// Split styled spans into words at spaces, keeping per-fragment fonts.
/// A style change mid-word must not introduce a break.
fn split_words(spans: &[Span], base: Font) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    let mut word: Word = Vec::new();
    for span in spans {
        let font = font_for(span.style, base);
        let mut frag = String::new();
        for c in span.text.chars() {
            if c == ' ' {
                if !frag.is_empty() {
                    word.push(Frag {
                        font,
                        text: std::mem::take(&mut frag),
                    });
                }
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            } else if !c.is_control() {
                frag.push(c);
            }
        }
        if !frag.is_empty() {
            word.push(Frag { font, text: frag });
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn word_width(word: &Word, size: f32) -> f32 {
    word.iter().map(|f| text_width(&f.text, f.font, size)).sum()
}

/// Greedy wrap into lines of at most `avail` points. A word wider than a
/// whole empty line is hard-split by characters so nothing ever draws
/// outside the margins.
fn wrap_words(words: &[Word], avail: f32, base: Font, size: f32) -> Vec<Vec<Word>> {
    let space = text_width(" ", base, size);
    let mut lines: Vec<Vec<Word>> = Vec::new();
    let mut current: Vec<Word> = Vec::new();
    let mut used = 0.0f32;

    for word in words {
        let w = word_width(word, size);
        let sep = if current.is_empty() { 0.0 } else { space };
        if used + sep + w <= avail {
            current.push(word.clone());
            used += sep + w;
        } else if w <= avail {
            lines.push(std::mem::take(&mut current));
            current.push(word.clone());
            used = w;
        } else {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let mut chunks = split_word(word, avail, size);
            // The last chunk stays open so following words can join it.
            if let Some(last) = chunks.pop() {
                for chunk in chunks {
                    lines.push(vec![chunk]);
                }
                used = word_width(&last, size);
                current.push(last);
            }
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// Hard-split an oversized word into chunks no wider than `avail`,
/// always making progress (at least one character per chunk).
fn split_word(word: &Word, avail: f32, size: f32) -> Vec<Word> {
    let mut chunks: Vec<Word> = Vec::new();
    let mut chunk: Word = Vec::new();
    let mut used = 0.0f32;
    for frag in word {
        let mut text = String::new();
        for c in frag.text.chars() {
            let w = f32::from(glyph_width(frag.font, c)) * size / 1000.0;
            if used + w > avail && used > 0.0 {
                if !text.is_empty() {
                    chunk.push(Frag {
                        font: frag.font,
                        text: std::mem::take(&mut text),
                    });
                }
                chunks.push(std::mem::take(&mut chunk));
                used = 0.0;
            }
            text.push(c);
            used += w;
        }
        if !text.is_empty() {
            chunk.push(Frag {
                font: frag.font,
                text,
            });
        }
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

/// Merge a wrapped line's words back into same-font fragments with
/// single spaces between words (spaces take the base font).
fn join_words(words: &[Word], base: Font) -> Vec<Frag> {
    let mut frags: Vec<Frag> = Vec::new();
    let push = |font: Font, text: &str, frags: &mut Vec<Frag>| {
        if let Some(last) = frags.last_mut()
            && last.font == font
        {
            last.text.push_str(text);
        } else {
            frags.push(Frag {
                font,
                text: text.to_string(),
            });
        }
    };
    for (i, word) in words.iter().enumerate() {
        if i > 0 {
            push(base, " ", &mut frags);
        }
        for frag in word {
            push(frag.font, &frag.text, &mut frags);
        }
    }
    frags
}

/// Centered `page N of M` on every page; placed after layout so the total
/// is known.
fn add_footers(pages: &mut [Vec<Item>]) {
    let total = pages.len();
    for (i, page) in pages.iter_mut().enumerate() {
        let text = format!("page {} of {}", i + 1, total);
        let width = text_width(&text, Font::Helvetica, 8.0);
        page.push(Item::Text {
            x: (PAGE_W - width) / 2.0,
            y: FOOTER_Y,
            size: 8.0,
            frags: vec![Frag {
                font: Font::Helvetica,
                text,
            }],
        });
    }
}

// --- PDF emission ---

/// Escape and WinAnsi-encode one fragment into a PDF string literal body.
fn encode_literal(text: &str, out: &mut Vec<u8>) {
    for c in text.chars() {
        let b = winansi_byte(c).unwrap_or(b'?');
        if b < 0x20 {
            continue;
        }
        if matches!(b, b'(' | b')' | b'\\') {
            out.push(b'\\');
        }
        out.push(b);
    }
}

/// One page's content stream: rules first (outside the text object),
/// then every text line with explicit positioning.
fn page_stream(items: &[Item]) -> Vec<u8> {
    let mut s: Vec<u8> = Vec::new();
    for item in items {
        if let Item::Rule { y } = item {
            s.extend_from_slice(
                format!(
                    "0.5 w 0.6 G {:.2} {y:.2} m {:.2} {y:.2} l S\n",
                    MARGIN,
                    PAGE_W - MARGIN
                )
                .as_bytes(),
            );
        }
    }
    s.extend_from_slice(b"BT\n");
    for item in items {
        let Item::Text { x, y, size, frags } = item else {
            continue;
        };
        s.extend_from_slice(format!("1 0 0 1 {x:.2} {y:.2} Tm\n").as_bytes());
        for frag in frags {
            s.extend_from_slice(format!("{} {size:.2} Tf (", frag.font.resource()).as_bytes());
            encode_literal(&frag.text, &mut s);
            s.extend_from_slice(b") Tj\n");
        }
    }
    s.extend_from_slice(b"ET\n");
    s
}

/// Assemble the document: fixed object order, uncompressed streams, a
/// correct xref table, and no `/Info`, `/ID`, or timestamp anywhere —
/// that is what makes the bytes reproducible.
fn assemble(pages: &[Vec<Item>]) -> Vec<u8> {
    // Object ids: 1 catalog, 2 page tree, 3..=5 fonts, then per page i:
    // 6+2i page, 7+2i contents.
    let page_object_count = pages.len() * 2;
    let total_objects = 5 + page_object_count;
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets: Vec<usize> = Vec::with_capacity(total_objects);

    let begin = |id: usize, out: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        debug_assert_eq!(offsets.len() + 1, id);
        offsets.push(out.len());
        out.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
    };

    begin(1, &mut out, &mut offsets);
    out.extend_from_slice(b"<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    begin(2, &mut out, &mut offsets);
    let kids: Vec<String> = (0..pages.len())
        .map(|i| format!("{} 0 R", 6 + 2 * i))
        .collect();
    out.extend_from_slice(
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>\nendobj\n",
            kids.join(" "),
            pages.len()
        )
        .as_bytes(),
    );

    for (id, name) in [(3, "Helvetica"), (4, "Helvetica-Bold"), (5, "Courier")] {
        begin(id, &mut out, &mut offsets);
        out.extend_from_slice(
            format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /{name} \
                 /Encoding /WinAnsiEncoding >>\nendobj\n"
            )
            .as_bytes(),
        );
    }

    for (i, page) in pages.iter().enumerate() {
        let page_id = 6 + 2 * i;
        let contents_id = page_id + 1;

        begin(page_id, &mut out, &mut offsets);
        out.extend_from_slice(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {PAGE_W:.0} {PAGE_H:.0}] \
                 /Resources << /Font << /F1 3 0 R /F2 4 0 R /F3 5 0 R >> >> \
                 /Contents {contents_id} 0 R >>\nendobj\n"
            )
            .as_bytes(),
        );

        let stream = page_stream(page);
        begin(contents_id, &mut out, &mut offsets);
        out.extend_from_slice(format!("<< /Length {} >>\nstream\n", stream.len()).as_bytes());
        out.extend_from_slice(&stream);
        out.extend_from_slice(b"endstream\nendobj\n");
    }

    let xref_at = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", total_objects + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            total_objects + 1
        )
        .as_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Vec<Span> {
        vec![Span {
            style: Style::Body,
            text: text.into(),
        }]
    }

    #[test]
    fn inline_markers_toggle_bold_and_code() {
        let spans = inline_spans("a **b** `c`");
        let shape: Vec<(Style, &str)> = spans.iter().map(|s| (s.style, s.text.as_str())).collect();
        assert_eq!(
            shape,
            vec![
                (Style::Body, "a "),
                (Style::Bold, "b"),
                (Style::Body, " "),
                (Style::Code, "c"),
            ]
        );
    }

    #[test]
    fn a_style_change_mid_word_does_not_split_the_word() {
        let spans = inline_spans("pre**bold**post");
        let words = split_words(&spans, Font::Helvetica);
        assert_eq!(words.len(), 1, "one word: {words:?}");
        assert_eq!(words[0].len(), 3, "three fragments: {words:?}");
    }

    #[test]
    fn wrapping_never_exceeds_the_available_width() {
        let words = split_words(&plain(&"word ".repeat(200)), Font::Helvetica);
        let lines = wrap_words(&words, 200.0, Font::Helvetica, 10.0);
        for line in &lines {
            let w: f32 = line.iter().map(|wd| word_width(wd, 10.0)).sum::<f32>()
                + (line.len().saturating_sub(1)) as f32 * text_width(" ", Font::Helvetica, 10.0);
            assert!(w <= 200.0, "line of {w} pt exceeds 200 pt");
        }
    }

    #[test]
    fn an_oversized_word_is_split_with_progress_on_every_chunk() {
        let words = split_words(&plain(&"x".repeat(500)), Font::Helvetica);
        let lines = wrap_words(&words, 100.0, Font::Helvetica, 10.0);
        assert!(lines.len() > 10, "500 glyphs at 5 pt each need many lines");
        let glyphs: usize = lines
            .iter()
            .flatten()
            .flat_map(|w| w.iter())
            .map(|f| f.text.chars().count())
            .sum();
        assert_eq!(glyphs, 500, "no character is lost by the split");
    }

    #[test]
    fn a_heading_is_never_stranded_as_the_last_line_of_a_page() {
        // Sweep fill depths so the heading crosses the page boundary at
        // several of them; the keep-together rule must hold at every depth.
        for filler in 0..80 {
            let mut md = String::from("# Top\n\n");
            for i in 0..filler {
                md.push_str(&format!("Filler paragraph number {i}.\n\n"));
            }
            md.push_str("## Keeper\n\nfollower line\n");
            let pages = layout(&parse(&md));
            let orphaned = pages.iter().any(|page| {
                page.last().is_some_and(|item| {
                    matches!(item, Item::Text { frags, .. }
                        if frags.iter().any(|f| f.text.contains("Keeper")))
                })
            });
            assert!(
                !orphaned,
                "heading orphaned with {filler} filler paragraphs"
            );
        }
    }

    #[test]
    fn a_heading_at_the_top_of_the_document_does_not_force_a_blank_page() {
        let pages = layout(&parse("## Keeper\n\nfollower\n"));
        assert_eq!(pages.len(), 1);
    }

    #[test]
    fn an_oversized_heading_flows_across_pages_instead_of_breaking_early() {
        // A heading taller than a page cannot be kept together with its
        // follower; it must start in place and paginate normally.
        let md = format!("intro paragraph\n\n## {}\n", "word ".repeat(1500));
        let pages = layout(&parse(&md));
        assert!(pages.len() >= 2, "a page-high heading spans pages");
        assert!(
            pages[0].len() > 1,
            "the heading starts beside the intro rather than forcing a break"
        );
    }

    #[test]
    fn literals_escape_the_pdf_delimiter_set() {
        let mut out = Vec::new();
        encode_literal(r"a(b)c\d", &mut out);
        assert_eq!(out, br"a\(b\)c\\d".to_vec());
    }

    #[test]
    fn non_winansi_characters_encode_as_question_marks() {
        let mut out = Vec::new();
        encode_literal("日x🚀", &mut out);
        assert_eq!(out, b"?x?".to_vec());
        assert_eq!(winansi_byte('\u{20AC}'), Some(0x80), "euro sign is CP1252");
        assert_eq!(winansi_byte('é'), Some(0xE9), "latin-1 range maps through");
    }
}
