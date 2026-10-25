//! A small Markdown document model, and the printer that writes it.
//!
//! Documentation text is arbitrary prose an author wrote, and Markdown gives
//! meaning to a great many characters: a leading `#` is a heading, `1.` a list,
//! `*x*` emphasis, `<b>` raw HTML, `&amp;` an entity, `[a](b)` a link. Text from
//! a doc file must reach the reader as the text it is, never as structure. The
//! renderer therefore never writes Markdown directly: it builds a [`Block`]
//! tree, and [`to_markdown`] prints it, escaping what needs escaping.
//!
//! The printer's contract is checked, not argued: the property tests parse the
//! printed Markdown back with a CommonMark parser (with the GFM extensions an
//! editor's renderer is likely to enable) and require the *same tree* back —
//! every [`Inline::Text`] as text, every code span as a code span, no structure
//! gained or lost.
//!
//! Escaping is deliberately *minimal*: hover text is read raw by agents as often
//! as rendered, and `System\.String\.Join\(\)` is line noise. Only the
//! characters that can open structure where they stand are escaped — the
//! delimiters `` \ ` * _ [ ] < & | ~ `` everywhere, and the block markers
//! (`#`, `>`, `+`, `-`, `=`, `1.`) only at the start of a line.
//!
//! Printing goes through [`normalize`] first. Some trees have no faithful
//! Markdown spelling (`**` around text that starts with punctuation is not
//! emphasis in CommonMark; two adjacent code spans read as one), so `normalize`
//! rewrites them to the nearest tree that has one, keeping the text: whitespace
//! runs collapse to one space, emphasis that cannot be spelled becomes plain
//! text, adjacent code spans merge, empty blocks vanish. The round-trip property
//! is stated over the normalised tree, and `normalize` is idempotent.

/// A block-level element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Inline>),
    /// A fenced code block: `language` is the info string, `code` the verbatim
    /// lines without a trailing newline.
    CodeBlock {
        language: Option<String>,
        code: String,
    },
    /// A list whose items are each a sequence of blocks.
    List {
        ordered: bool,
        items: Vec<Vec<Block>>,
    },
}

/// An inline element. Emphasis and link text are plain text: nothing in a doc
/// comment needs more, and it keeps every inline's spelling decidable locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text(String),
    Code(String),
    Strong(String),
    Emphasis(String),
    Link { text: String, url: String },
    LineBreak,
}

/// Print `blocks` as CommonMark, after [`normalize`].
pub fn to_markdown(blocks: &[Block]) -> String {
    let blocks = normalize(blocks.to_vec());
    let mut out = String::new();
    print_blocks(&blocks, "", &mut out);
    out
}

// ---------------------------------------------------------------------------
// Normalisation
// ---------------------------------------------------------------------------

/// Rewrite `blocks` to the canonical tree [`to_markdown`] prints faithfully.
pub fn normalize(blocks: Vec<Block>) -> Vec<Block> {
    blocks.into_iter().filter_map(normalize_block).collect()
}

fn normalize_block(block: Block) -> Option<Block> {
    match block {
        Block::Paragraph(inlines) => {
            let inlines = normalize_inlines(inlines);
            (!inlines.is_empty()).then_some(Block::Paragraph(inlines))
        }
        Block::CodeBlock { language, code } => {
            let code = normalize_code(&code);
            if code.is_empty() {
                return None;
            }
            let language = language
                .map(|l| {
                    l.chars()
                        .filter(|c| c.is_ascii_alphanumeric() || "+#._-".contains(*c))
                        .collect::<String>()
                })
                .filter(|l| !l.is_empty());
            Some(Block::CodeBlock { language, code })
        }
        Block::List { ordered, items } => {
            let items: Vec<Vec<Block>> = items
                .into_iter()
                .map(normalize)
                .filter(|item| !item.is_empty())
                .collect();
            (!items.is_empty()).then_some(Block::List { ordered, items })
        }
    }
}

/// Code is verbatim except for what CommonMark itself would change: line
/// endings become `\n`, NUL becomes U+FFFD, and blank lines at either end go
/// (a reader cannot see them, and the fence would carry them inconsistently).
fn normalize_code(code: &str) -> String {
    let code = code
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\0', "\u{FFFD}");
    let lines: Vec<&str> = code.split('\n').collect();
    let blank = |l: &&str| l.chars().all(|c| c == ' ' || c == '\t');
    let Some(start) = lines.iter().position(|l| !blank(l)) else {
        return String::new();
    };
    let end = lines
        .iter()
        .rposition(|l| !blank(l))
        .map_or(start, |i| i + 1);
    lines[start..end].join("\n")
}

/// The characters treated as inter-word whitespace: XML's four, plus the
/// vertical tab and form feed CommonMark also treats as whitespace.
fn is_breaking_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{0B}' | '\u{0C}')
}

/// Collapse every whitespace run to one space, and substitute NUL as a
/// CommonMark reader does.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if is_breaking_space(c) {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(if c == '\0' { '\u{FFFD}' } else { c });
            in_space = false;
        }
    }
    out
}

/// Whether `url` can be a link destination as printed (`<url>` with `<`, `>`,
/// `\` and `&` backslash-escaped): no whitespace or control characters.
fn is_printable_url(url: &str) -> bool {
    !url.is_empty() && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Whether emphasis around `text` is spelled by `*`/`**` unambiguously: the
/// delimiters are then left- and right-flanking whatever surrounds them.
fn emphasis_spellable(text: &str) -> bool {
    let mut chars = text.chars();
    let first = chars.next();
    let last = text.chars().next_back();
    first.is_some_and(char::is_alphanumeric) && last.is_some_and(char::is_alphanumeric)
}

fn is_emphasis(inline: &Inline) -> bool {
    matches!(inline, Inline::Strong(_) | Inline::Emphasis(_))
}

/// Normalise a paragraph's inlines. Afterwards:
///
/// - text has collapsed whitespace, emphasis and link text are trimmed
///   (their edge spaces move outside, where they read the same), and code
///   keeps its whitespace bar line endings;
/// - emphasis survives only where [`emphasis_spellable`] holds and it has no
///   emphasis neighbour (adjacent delimiter runs would merge);
/// - a link whose destination cannot be printed becomes text;
/// - adjacent text merges, and adjacent code spans merge;
/// - no space at a paragraph edge or beside a line break, and no leading,
///   trailing or doubled line break.
fn normalize_inlines(inlines: Vec<Inline>) -> Vec<Inline> {
    // Step 1: per-inline cleanup, hoisting edge spaces out into text.
    let mut atoms: Vec<Inline> = Vec::new();
    let hoisted = |atoms: &mut Vec<Inline>, content: &str, make: fn(String) -> Inline| {
        let content = collapse(content);
        let trimmed = content.trim_matches(' ');
        if trimmed.is_empty() {
            atoms.push(Inline::Text(content));
            return;
        }
        if content.starts_with(' ') {
            atoms.push(Inline::Text(" ".to_string()));
        }
        atoms.push(make(trimmed.to_string()));
        if content.ends_with(' ') {
            atoms.push(Inline::Text(" ".to_string()));
        }
    };
    for inline in inlines {
        match inline {
            Inline::Text(t) => atoms.push(Inline::Text(collapse(&t))),
            Inline::LineBreak => atoms.push(Inline::LineBreak),
            // Code keeps its whitespace — `"a   b"` is not `"a b"` — except
            // line endings, which a code span reads as spaces anyway. Its edge
            // spaces stay inside too; the printer pads so they survive.
            Inline::Code(c) => {
                let c = c
                    .replace("\r\n", " ")
                    .replace(['\r', '\n'], " ")
                    .replace('\0', "\u{FFFD}");
                if c.trim_matches(' ').is_empty() {
                    atoms.push(Inline::Text(collapse(&c)));
                } else {
                    atoms.push(Inline::Code(c));
                }
            }
            Inline::Strong(c) => hoisted(&mut atoms, &c, Inline::Strong),
            Inline::Emphasis(c) => hoisted(&mut atoms, &c, Inline::Emphasis),
            Inline::Link { text, url } => {
                let url = url.replace('\0', "\u{FFFD}");
                if !is_printable_url(&url) {
                    let shown = if text.trim().is_empty() {
                        url
                    } else {
                        format!("{text} ({url})")
                    };
                    atoms.push(Inline::Text(collapse(&shown)));
                    continue;
                }
                let text = collapse(&text);
                if text.trim_matches(' ').is_empty() {
                    atoms.push(Inline::Text(text));
                    atoms.push(Inline::Link {
                        text: url.clone(),
                        url,
                    });
                } else {
                    if text.starts_with(' ') {
                        atoms.push(Inline::Text(" ".to_string()));
                    }
                    atoms.push(Inline::Link {
                        text: text.trim_matches(' ').to_string(),
                        url,
                    });
                    if text.ends_with(' ') {
                        atoms.push(Inline::Text(" ".to_string()));
                    }
                }
            }
        }
    }
    let atoms = merge_adjacent(atoms);

    // Step 2: emphasis that cannot be spelled — or that touches other
    // emphasis — is demoted to text.
    let demoted: Vec<Inline> = (0..atoms.len())
        .map(|i| match &atoms[i] {
            Inline::Strong(t) | Inline::Emphasis(t) => {
                let touches = (i > 0 && is_emphasis(&atoms[i - 1]))
                    || atoms.get(i + 1).is_some_and(is_emphasis);
                if emphasis_spellable(t) && !touches {
                    atoms[i].clone()
                } else {
                    Inline::Text(t.clone())
                }
            }
            other => other.clone(),
        })
        .collect();
    let atoms = merge_adjacent(demoted);

    // Step 3: spaces and line breaks at line edges.
    let mut out: Vec<Inline> = Vec::new();
    for atom in atoms {
        match atom {
            Inline::LineBreak => {
                trim_trailing(&mut out);
                if !out.is_empty() && !matches!(out.last(), Some(Inline::LineBreak)) {
                    out.push(Inline::LineBreak);
                }
            }
            Inline::Text(t) => {
                let at_line_start = matches!(out.last(), None | Some(Inline::LineBreak));
                let t = if at_line_start {
                    t.trim_start_matches(' ').to_string()
                } else {
                    t
                };
                if !t.is_empty() {
                    out.push(Inline::Text(t));
                }
            }
            other => out.push(other),
        }
    }
    trim_trailing(&mut out);
    out
}

/// Drop trailing spaces and line breaks.
fn trim_trailing(out: &mut Vec<Inline>) {
    loop {
        match out.last_mut() {
            Some(Inline::LineBreak) => {
                out.pop();
            }
            Some(Inline::Text(t)) => {
                let trimmed = t.trim_end_matches(' ').len();
                if trimmed == 0 {
                    out.pop();
                } else {
                    t.truncate(trimmed);
                    return;
                }
            }
            _ => return,
        }
    }
}

/// Merge adjacent text (re-collapsing the seam) and adjacent code spans, and
/// drop empty text.
fn merge_adjacent(atoms: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    for atom in atoms {
        match (out.last_mut(), atom) {
            (_, Inline::Text(t)) if t.is_empty() => {}
            // Each atom is already collapsed, so only the seam can hold a
            // doubled space — and only the seam is looked at, keeping the
            // merge linear however many atoms a paragraph has.
            (Some(Inline::Text(prev)), Inline::Text(t)) => {
                let t = if prev.ends_with(' ') {
                    t.strip_prefix(' ').unwrap_or(&t)
                } else {
                    &t
                };
                prev.push_str(t);
            }
            (Some(Inline::Code(prev)), Inline::Code(c)) => prev.push_str(&c),
            (_, atom) => out.push(atom),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Printing (of normalised trees)
// ---------------------------------------------------------------------------

/// Print `blocks`, the first line's prefix already written by the caller and
/// every later line prefixed by `indent` (a list item's continuation).
fn print_blocks(blocks: &[Block], indent: &str, out: &mut String) {
    // Two lists of one kind in a row would read as one list; alternating the
    // marker character between them (`-`, `*`, `-`, …) keeps them apart.
    let mut alternated = false;
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n");
            out.push_str(indent);
        }
        let follows_same_kind = i > 0
            && matches!(
                (&blocks[i - 1], block),
                (Block::List { ordered: a, .. }, Block::List { ordered: b, .. }) if a == b
            );
        alternated = follows_same_kind && !alternated;
        print_block(block, indent, alternated, out);
    }
}

fn print_block(block: &Block, indent: &str, alternate: bool, out: &mut String) {
    match block {
        Block::Paragraph(inlines) => print_inlines(inlines, indent, out),
        Block::CodeBlock { language, code } => {
            let fence = "`".repeat(longest_backtick_run(code).max(2) + 1);
            out.push_str(&fence);
            if let Some(language) = language {
                out.push_str(language);
            }
            for line in code.split('\n') {
                out.push('\n');
                out.push_str(indent);
                out.push_str(line);
            }
            out.push('\n');
            out.push_str(indent);
            out.push_str(&fence);
        }
        Block::List { ordered, items } => {
            for (n, item) in items.iter().enumerate() {
                if n > 0 {
                    out.push('\n');
                    out.push_str(indent);
                }
                let marker = match (ordered, alternate) {
                    (false, false) => "-".to_string(),
                    (false, true) => "*".to_string(),
                    (true, false) => format!("{}.", n + 1),
                    (true, true) => format!("{})", n + 1),
                };
                out.push_str(&marker);
                out.push(' ');
                let inner = format!("{indent}{}", " ".repeat(marker.len() + 1));
                print_blocks(item, &inner, out);
            }
        }
    }
}

fn longest_backtick_run(s: &str) -> usize {
    s.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

fn print_inlines(inlines: &[Inline], indent: &str, out: &mut String) {
    let mut at_line_start = true;
    for (i, inline) in inlines.iter().enumerate() {
        match inline {
            Inline::Text(t) => {
                let before_link = matches!(inlines.get(i + 1), Some(Inline::Link { .. }));
                escape_text(t, at_line_start, before_link, out);
            }
            Inline::Code(c) => {
                let fence = "`".repeat(longest_backtick_run(c) + 1);
                // CommonMark strips one space from each end of a code span
                // when both ends have one, so content that starts or ends with
                // a space — or a backtick, which would merge with the fence —
                // is padded on both sides.
                let pad = if c.starts_with(['`', ' ']) || c.ends_with(['`', ' ']) {
                    " "
                } else {
                    ""
                };
                out.push_str(&format!("{fence}{pad}{c}{pad}{fence}"));
            }
            Inline::Strong(t) => {
                out.push_str("**");
                escape_text(t, false, false, out);
                out.push_str("**");
            }
            Inline::Emphasis(t) => {
                out.push('*');
                escape_text(t, false, false, out);
                out.push('*');
            }
            Inline::Link { text, url } => {
                out.push('[');
                escape_text(text, false, false, out);
                out.push_str("](<");
                for c in url.chars() {
                    if matches!(c, '<' | '>' | '\\' | '&') {
                        out.push('\\');
                    }
                    out.push(c);
                }
                out.push_str(">)");
            }
            Inline::LineBreak => {
                out.push_str("\\\n");
                out.push_str(indent);
            }
        }
        at_line_start = matches!(inline, Inline::LineBreak);
    }
}

/// Write `text` so that it reads back as exactly itself.
///
/// `at_line_start`: the text begins a line, so a block marker there would open
/// a block. `before_link`: a link follows, so a final `!` would make it an image.
fn escape_text(text: &str, at_line_start: bool, before_link: bool, out: &mut String) {
    // At a line start, an ordered-list marker is a digit run then `.` or `)`;
    // the index of that punctuation, if this text starts with one.
    let list_punct = at_line_start
        .then(|| {
            let digits = text.bytes().take_while(u8::is_ascii_digit).count();
            (digits > 0 && matches!(text.as_bytes().get(digits), Some(b'.' | b')')))
                .then_some(digits)
        })
        .flatten();
    let last = text.len().saturating_sub(1);
    for (i, c) in text.char_indices() {
        let escape = match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '&' | '|' | '~' => true,
            '#' | '>' | '+' | '-' | '=' => at_line_start && i == 0,
            '.' | ')' => list_punct == Some(i),
            '!' => before_link && i == last,
            _ => false,
        };
        if escape {
            out.push('\\');
        }
        out.push(c);
    }
}

/// The test oracle: a CommonMark reader back into the model.
#[cfg(test)]
pub(crate) mod oracle {
    use super::{Block, Inline};
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

    /// Read Markdown back into the model with a CommonMark parser, failing on
    /// anything the model cannot express (a heading, a quote, raw HTML, a soft
    /// break, …) — any of which would mean text had become structure.
    pub(crate) fn parse_back(markdown: &str) -> Vec<Block> {
        let options = Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_GFM;
        let events: Vec<Event<'_>> = Parser::new_ext(markdown, options).collect();
        let mut pos = 0;
        let blocks = parse_blocks(&events, &mut pos, markdown);
        assert_eq!(pos, events.len(), "unconsumed events in:\n{markdown}");
        blocks
    }

    fn parse_blocks(events: &[Event<'_>], pos: &mut usize, md: &str) -> Vec<Block> {
        let mut blocks = Vec::new();
        while let Some(event) = events.get(*pos) {
            match event {
                Event::End(TagEnd::Item) => break,
                Event::Start(Tag::Paragraph) => {
                    *pos += 1;
                    blocks.push(Block::Paragraph(parse_inlines(
                        events,
                        pos,
                        TagEnd::Paragraph,
                        md,
                    )));
                }
                Event::Start(Tag::CodeBlock(kind)) => {
                    let language = match kind {
                        CodeBlockKind::Fenced(info) if !info.is_empty() => Some(info.to_string()),
                        CodeBlockKind::Fenced(_) => None,
                        CodeBlockKind::Indented => panic!("indented code block in:\n{md}"),
                    };
                    *pos += 1;
                    let mut code = String::new();
                    while let Some(Event::Text(t)) = events.get(*pos) {
                        code.push_str(t);
                        *pos += 1;
                    }
                    assert_eq!(events.get(*pos), Some(&Event::End(TagEnd::CodeBlock)));
                    *pos += 1;
                    let code = code.strip_suffix('\n').unwrap_or(&code).to_string();
                    blocks.push(Block::CodeBlock { language, code });
                }
                Event::Start(Tag::List(start)) => {
                    assert!(
                        matches!(start, None | Some(1)),
                        "list start {start:?}:\n{md}"
                    );
                    let ordered = start.is_some();
                    *pos += 1;
                    let mut items = Vec::new();
                    while let Some(Event::Start(Tag::Item)) = events.get(*pos) {
                        *pos += 1;
                        items.push(parse_blocks(events, pos, md));
                        assert_eq!(events.get(*pos), Some(&Event::End(TagEnd::Item)));
                        *pos += 1;
                    }
                    assert_eq!(
                        events.get(*pos),
                        Some(&Event::End(TagEnd::List(ordered))),
                        "in:\n{md}"
                    );
                    *pos += 1;
                    blocks.push(Block::List { ordered, items });
                }
                Event::End(_) => break,
                _ => {
                    // A tight list item holds inlines with no paragraph around them.
                    blocks.push(Block::Paragraph(parse_inline_run(events, pos, md)));
                }
            }
        }
        blocks
    }

    /// Inlines up to (and consuming) `end`.
    fn parse_inlines(events: &[Event<'_>], pos: &mut usize, end: TagEnd, md: &str) -> Vec<Inline> {
        let inlines = parse_inline_run(events, pos, md);
        assert_eq!(events.get(*pos), Some(&Event::End(end)), "in:\n{md}");
        *pos += 1;
        inlines
    }

    /// Inline events until the next block-level event (not consumed).
    fn parse_inline_run(events: &[Event<'_>], pos: &mut usize, md: &str) -> Vec<Inline> {
        let mut out: Vec<Inline> = Vec::new();
        let push_text = |out: &mut Vec<Inline>, t: &str| match out.last_mut() {
            Some(Inline::Text(prev)) => prev.push_str(t),
            _ => out.push(Inline::Text(t.to_string())),
        };
        let plain = |events: &[Event<'_>], pos: &mut usize, end: TagEnd| {
            let mut text = String::new();
            while let Some(Event::Text(t)) = events.get(*pos) {
                text.push_str(t);
                *pos += 1;
            }
            assert_eq!(events.get(*pos), Some(&Event::End(end)), "in:\n{md}");
            *pos += 1;
            text
        };
        while let Some(event) = events.get(*pos) {
            match event {
                Event::Text(t) => {
                    push_text(&mut out, t);
                    *pos += 1;
                }
                Event::Code(c) => {
                    out.push(Inline::Code(c.to_string()));
                    *pos += 1;
                }
                Event::HardBreak => {
                    out.push(Inline::LineBreak);
                    *pos += 1;
                }
                Event::Start(Tag::Strong) => {
                    *pos += 1;
                    out.push(Inline::Strong(plain(events, pos, TagEnd::Strong)));
                }
                Event::Start(Tag::Emphasis) => {
                    *pos += 1;
                    out.push(Inline::Emphasis(plain(events, pos, TagEnd::Emphasis)));
                }
                Event::Start(Tag::Link { dest_url, .. }) => {
                    let url = dest_url.to_string();
                    *pos += 1;
                    let text = plain(events, pos, TagEnd::Link);
                    out.push(Inline::Link { text, url });
                }
                Event::End(_)
                | Event::Start(Tag::Paragraph | Tag::List(_) | Tag::CodeBlock(_) | Tag::Item) => {
                    break;
                }
                other => panic!("unexpected Markdown structure {other:?} in:\n{md}"),
            }
        }
        out
    }

    /// [`text_of`], with each link's destination after its words: what a
    /// reader can recover, for oracles about content being lost.
    pub(crate) fn text_and_urls_of(blocks: &[Block]) -> String {
        let with_urls: Vec<Block> = blocks
            .iter()
            .map(|b| match b {
                Block::Paragraph(inlines) => Block::Paragraph(
                    inlines
                        .iter()
                        .flat_map(|i| match i {
                            Inline::Link { text, url } => vec![
                                Inline::Text(text.clone()),
                                Inline::Text(" ".to_string()),
                                Inline::Text(url.clone()),
                            ],
                            other => vec![other.clone()],
                        })
                        .collect(),
                ),
                Block::List { ordered, items } => Block::List {
                    ordered: *ordered,
                    items: items
                        .iter()
                        .map(|item| {
                            vec![Block::Paragraph(vec![Inline::Text(text_and_urls_of(item))])]
                        })
                        .collect(),
                },
                other => other.clone(),
            })
            .collect();
        text_of(&with_urls)
    }

    /// The text a reader sees, one space between blocks and for a line break.
    pub(crate) fn text_of(blocks: &[Block]) -> String {
        let mut out = String::new();
        for block in blocks {
            match block {
                Block::Paragraph(inlines) => {
                    for inline in inlines {
                        match inline {
                            Inline::Text(t)
                            | Inline::Code(t)
                            | Inline::Strong(t)
                            | Inline::Emphasis(t) => out.push_str(t),
                            Inline::Link { text, .. } => out.push_str(text),
                            Inline::LineBreak => out.push(' '),
                        }
                    }
                }
                Block::CodeBlock { code, .. } => out.push_str(code),
                Block::List { items, .. } => {
                    for item in items {
                        out.push_str(&text_of(item));
                    }
                }
            }
            out.push(' ');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::oracle::{parse_back, text_of};
    use super::*;
    use proptest::prelude::*;

    // -- generators -----------------------------------------------------------

    /// Text drawn mostly from characters that mean something to Markdown,
    /// with whitespace of every kind and some ordinary words.
    fn text() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                3 => "[a-zA-Z0-9]{1,4}",
                3 => proptest::sample::select(vec![
                    " ", "  ", "\t", "\n", "\r\n", "\u{0B}", "\u{0C}", "\u{A0}",
                ]).prop_map(str::to_string),
                4 => proptest::sample::select(
                    "\\`*_[]<>&|~#+-=.)(!:;{}\"'$%^?/@0123456789".chars().collect::<Vec<_>>()
                ).prop_map(|c| c.to_string()),
                1 => proptest::sample::select(vec![
                    "1.", "10)", "- ", "* ", "+ ", "# ", "> ", "---", "===", "```", "~~~",
                    "<b>", "</b>", "&amp;", "&#42;", "[x]", "[x](y)", "![x](y)", "<http://x>",
                    "http://example.com", "[^1]", "- [ ] ", "|a|b|", "\\\n", "\u{0}",
                    "**", "__", "é", "日本",
                ]).prop_map(str::to_string),
            ],
            0..8,
        )
        .prop_map(|parts| parts.concat())
    }

    fn url() -> impl Strategy<Value = String> {
        prop_oneof![
            3 => "https://[a-z]{1,6}\\.com/[a-zA-Z0-9_()&<>\\\\%?=#-]{0,8}",
            1 => text(),
        ]
    }

    fn inline() -> impl Strategy<Value = Inline> {
        prop_oneof![
            4 => text().prop_map(Inline::Text),
            2 => text().prop_map(Inline::Code),
            1 => text().prop_map(Inline::Strong),
            1 => text().prop_map(Inline::Emphasis),
            1 => (text(), url()).prop_map(|(text, url)| Inline::Link { text, url }),
            1 => Just(Inline::LineBreak),
        ]
    }

    fn code() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                "[a-z ]{0,6}",
                proptest::sample::select(vec![
                    "\n", "\n\n", "    ", "\t", "```", "````", "~~~", "`", "\r\n", "- x", "#",
                ])
                .prop_map(str::to_string),
            ],
            0..10,
        )
        .prop_map(|parts| parts.concat())
    }

    fn block() -> impl Strategy<Value = Block> {
        let leaf = prop_oneof![
            3 => proptest::collection::vec(inline(), 0..6).prop_map(Block::Paragraph),
            1 => (proptest::option::of("[a-zA-Z#+ `{}-]{0,6}"), code())
                .prop_map(|(language, code)| Block::CodeBlock { language, code }),
        ];
        leaf.prop_recursive(3, 24, 4, |inner| {
            (
                any::<bool>(),
                proptest::collection::vec(proptest::collection::vec(inner, 0..3), 0..12),
            )
                .prop_map(|(ordered, items)| Block::List { ordered, items })
        })
    }

    fn document() -> impl Strategy<Value = Vec<Block>> {
        proptest::collection::vec(block(), 0..5)
    }

    /// Characters of `s` that survive normalisation (non-whitespace).
    fn visible(s: &str) -> String {
        s.chars()
            .filter(|c| !is_breaking_space(*c))
            .map(|c| if c == '\0' { '\u{FFFD}' } else { c })
            .collect()
    }

    proptest! {
        /// The headline property: what the printer writes, a CommonMark parser
        /// reads back as exactly the normalised tree.
        #[test]
        fn printed_markdown_reads_back_as_the_normalised_tree(doc in document()) {
            let normalised = normalize(doc.clone());
            let printed = to_markdown(&doc);
            let read = parse_back(&printed);
            prop_assert_eq!(&read, &normalised, "printed:\n{}", printed);
        }

        #[test]
        fn normalisation_is_idempotent(doc in document()) {
            let once = normalize(doc);
            prop_assert_eq!(normalize(once.clone()), once);
        }

        /// Normalisation moves and merges text but never loses a visible
        /// character — except a link's URL, which a demoted link keeps in its
        /// text and a kept one keeps in the destination.
        #[test]
        fn normalisation_keeps_every_visible_character(
            doc in proptest::collection::vec(
                proptest::collection::vec(inline(), 0..6).prop_map(Block::Paragraph),
                0..4,
            )
        ) {
            let without_links = |blocks: &[Block]| -> Vec<Block> {
                blocks.iter().map(|b| match b {
                    Block::Paragraph(inlines) => Block::Paragraph(
                        inlines.iter().filter(|i| !matches!(i, Inline::Link { .. })).cloned().collect(),
                    ),
                    other => other.clone(),
                }).collect()
            };
            let doc = without_links(&doc);
            prop_assert_eq!(visible(&text_of(&normalize(doc.clone()))), visible(&text_of(&doc)));
        }
    }

    /// The property generator must reach the interesting regimes, or the
    /// round-trip holds vacuously: check that normalised documents keep
    /// emphasis, links, line breaks, nested lists and code in lists at a
    /// healthy rate. (Deterministic: a fixed-seed runner, so the counts move
    /// only when the strategy does. 300 documents, because generating them
    /// unoptimised costs ~7 ms each.)
    #[test]
    fn the_generator_reaches_every_construct() {
        use proptest::strategy::ValueTree;
        use proptest::test_runner::TestRunner;
        let mut runner = TestRunner::deterministic();
        let mut seen = [0usize; 6];
        fn walk(blocks: &[Block], depth: usize, seen: &mut [usize; 6]) {
            for block in blocks {
                match block {
                    Block::Paragraph(inlines) => {
                        for inline in inlines {
                            match inline {
                                Inline::Strong(_) | Inline::Emphasis(_) => seen[0] += 1,
                                Inline::Link { .. } => seen[1] += 1,
                                Inline::LineBreak => seen[2] += 1,
                                _ => {}
                            }
                        }
                    }
                    Block::CodeBlock { .. } if depth > 0 => seen[4] += 1,
                    Block::CodeBlock { .. } => {}
                    Block::List { items, .. } => {
                        if depth > 0 {
                            seen[3] += 1;
                        }
                        if items.len() >= 10 {
                            seen[5] += 1;
                        }
                        for item in items {
                            walk(item, depth + 1, seen);
                        }
                    }
                }
            }
        }
        for _ in 0..300 {
            let doc = document().new_tree(&mut runner).unwrap().current();
            walk(&normalize(doc), 0, &mut seen);
        }
        for (what, count) in [
            "emphasis",
            "link",
            "line break",
            "nested list",
            "code in list",
            "10+ item list",
        ]
        .iter()
        .zip(seen)
        {
            assert!(
                count >= 3,
                "only {count} {what} in 300 normalised documents"
            );
        }
    }

    /// Shrunk inputs once found by the round-trip property (kept as examples:
    /// a saved seed reproduces only while the strategy is byte-identical).
    #[test]
    fn found_by_the_round_trip_property() {
        let nested = |inlines: Vec<Inline>| {
            vec![Block::List {
                ordered: false,
                items: vec![vec![Block::List {
                    ordered: false,
                    items: vec![vec![Block::Paragraph(inlines)]],
                }]],
            }]
        };
        for doc in [
            nested(vec![Inline::Emphasis("* ".into())]),
            nested(vec![Inline::Text("- ".into())]),
            nested(vec![
                Inline::Text("!".into()),
                Inline::Link {
                    text: String::new(),
                    url: "https://a.com/".into(),
                },
            ]),
            nested(vec![Inline::Link {
                text: String::new(),
                url: "&amp;".into(),
            }]),
        ] {
            let printed = to_markdown(&doc);
            assert_eq!(parse_back(&printed), normalize(doc), "printed:\n{printed}");
        }
    }

    #[test]
    fn escaping_is_minimal_in_ordinary_prose() {
        let md = to_markdown(&[Block::Paragraph(vec![Inline::Text(
            "Returns the value (or null) of System.String.Join, e.g. 1 - 2.".to_string(),
        )])]);
        assert_eq!(
            md,
            "Returns the value (or null) of System.String.Join, e.g. 1 - 2."
        );
    }

    #[test]
    fn block_markers_are_escaped_only_at_line_starts() {
        let md = to_markdown(&[Block::Paragraph(vec![
            Inline::Text("# not a heading, 1. not a list".to_string()),
            Inline::LineBreak,
            Inline::Text("2) nor this - or this".to_string()),
        ])]);
        assert_eq!(
            md,
            "\\# not a heading, 1. not a list\\\n2\\) nor this - or this"
        );
    }

    #[test]
    fn unspellable_emphasis_degrades_to_text() {
        assert_eq!(
            normalize(vec![Block::Paragraph(vec![
                Inline::Strong(" bold ".to_string()),
                Inline::Strong("(paren)".to_string()),
            ])]),
            vec![Block::Paragraph(vec![
                Inline::Strong("bold".to_string()),
                Inline::Text(" (paren)".to_string()),
            ])]
        );
    }
}
