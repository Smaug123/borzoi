//! A pre-parse bound on XML element nesting.
//!
//! `roxmltree`'s tokenizer recurses once per nested element
//! (`parse_element` → `parse_content` → `parse_element`), with no limit of its
//! own, so a file nesting a few hundred thousand elements overflows the stack
//! and aborts the process — the LSP, here, over a `.xml` that any NuGet package
//! can ship. [`parse_bounded`] scans the text for its nesting depth first and
//! only hands it to `roxmltree` when that depth is within a bound.
//!
//! The scan must never *under*-estimate the depth `roxmltree` would recurse to;
//! over-estimating merely refuses a file. It follows the same lexical grammar
//! for the constructs that decide nesting — start/end/empty-element tags,
//! comments, CDATA, processing instructions, quoted attribute values — and
//! counts every `<` that `roxmltree` would treat as an element start, whether or
//! not the document later turns out malformed.

/// The deepest element nesting [`parse_bounded`] accepts.
///
/// The deepest shipped doc file found nests 10 levels (`doc` → `members` →
/// `member` → `remarks` → `list` → `item` → `description` → …; measured over
/// the .NET 10 reference pack, FSharp.Core, and 1,154 NuGet packages). The
/// ceiling is the stack: an unoptimised `roxmltree` spends ~16 KiB of stack per
/// level and overflows a 2 MiB test thread at ~128, an optimised one ~3,200
/// levels. 32 is three times the deepest real file and a quarter of the
/// tightest ceiling.
pub const MAX_FILE_DEPTH: usize = 32;

/// Why [`parse_bounded`] produced no document.
#[derive(Debug)]
pub enum BoundedParseError {
    /// The text nests elements deeper than the bound.
    TooDeep,
    Xml(roxmltree::Error),
}

impl std::fmt::Display for BoundedParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BoundedParseError::TooDeep => {
                write!(f, "elements nested deeper than {MAX_FILE_DEPTH}")
            }
            BoundedParseError::Xml(e) => e.fmt(f),
        }
    }
}

/// Parse `text` with `roxmltree`, refusing it up front if its element nesting
/// exceeds [`MAX_FILE_DEPTH`].
pub fn parse_bounded(text: &str) -> Result<roxmltree::Document<'_>, BoundedParseError> {
    if max_nesting(text) > MAX_FILE_DEPTH {
        return Err(BoundedParseError::TooDeep);
    }
    roxmltree::Document::parse(text).map_err(BoundedParseError::Xml)
}

/// An upper bound on the element nesting depth `roxmltree` reaches on `text`:
/// exact for well-formed XML, never lower for anything else.
pub fn max_nesting(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut depth: usize = 0;
    let mut max: usize = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &bytes[i..];
        if rest.starts_with(b"<!--") {
            i = skip_past(bytes, i + 4, b"-->");
        } else if rest.starts_with(b"<![CDATA[") {
            i = skip_past(bytes, i + 9, b"]]>");
        } else if rest.starts_with(b"<?") {
            i = skip_past(bytes, i + 2, b"?>");
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            i = skip_past(bytes, i + 2, b">");
        } else if rest.starts_with(b"<!") {
            // A DOCTYPE (refused by `roxmltree`'s default options) or junk:
            // nothing that opens an element. Its internal subset may contain
            // `>`, but stopping early only makes later `<` count, which can
            // only raise the estimate.
            i = skip_past(bytes, i + 2, b">");
        } else {
            // A start tag. Every one counts as an open, before knowing whether
            // it is self-closing: `roxmltree` recurses only for an open tag, so
            // this is an over-estimate by at most one level for `<a/>`.
            depth += 1;
            max = max.max(depth);
            let (end, self_closing) = scan_start_tag(bytes, i + 1);
            if self_closing {
                depth -= 1;
            }
            i = end;
        }
    }
    max
}

/// The index just past the first occurrence of `needle` at or after `from`, or
/// the end of the input when there is none.
fn skip_past(bytes: &[u8], from: usize, needle: &[u8]) -> usize {
    bytes
        .get(from..)
        .and_then(|tail| tail.windows(needle.len()).position(|w| w == needle))
        .map_or(bytes.len(), |p| from + p + needle.len())
}

/// Scan a start tag's remainder from just after its `<`: returns the index past
/// its closing `>` and whether it was `/>`. Quoted attribute values may contain
/// `>` and `/`; a `<` inside one is illegal XML, so the tag ends there — the
/// caller resumes at that `<`, which can only raise the estimate.
fn scan_start_tag(bytes: &[u8], mut i: usize) -> (usize, bool) {
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) if b == b'<' => return (i, false),
            Some(_) => {}
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'>' => {
                    let self_closing = i > 0 && bytes[i - 1] == b'/';
                    return (i + 1, self_closing);
                }
                b'<' => return (i, false),
                _ => {}
            },
        }
        i += 1;
    }
    (i, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A generated well-formed XML element tree, printed with the lexical
    /// constructs that could fool a depth scanner: attribute values holding
    /// `>` and `/>`, comments and CDATA holding tag-like text, processing
    /// instructions, and self-closing elements.
    #[derive(Debug, Clone)]
    enum Node {
        Element {
            attr: Option<String>,
            children: Vec<Node>,
        },
        Text(String),
        Comment(String),
        Cdata(String),
        Pi(String),
    }

    fn tricky_text() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                Just("<a>"),
                Just("</a>"),
                Just("<a/>"),
                Just("/>"),
                Just(">"),
                Just("--"),
                Just("]]"),
                Just("?"),
                Just("\""),
                Just("'"),
                Just("x"),
            ],
            0..6,
        )
        .prop_map(|parts| parts.concat())
    }

    fn node() -> impl Strategy<Value = Node> {
        let leaf = prop_oneof![
            "[a-z ]{0,4}".prop_map(Node::Text),
            tricky_text().prop_map(Node::Comment),
            tricky_text().prop_map(Node::Cdata),
            tricky_text().prop_map(Node::Pi),
            proptest::option::of(tricky_text()).prop_map(|attr| Node::Element {
                attr,
                children: Vec::new(),
            }),
        ];
        leaf.prop_recursive(12, 64, 4, |inner| {
            (
                proptest::option::of(tricky_text()),
                proptest::collection::vec(inner, 0..4),
            )
                .prop_map(|(attr, children)| Node::Element { attr, children })
        })
    }

    fn print(node: &Node, out: &mut String) {
        match node {
            Node::Element { attr, children } => {
                out.push_str("<e");
                if let Some(value) = attr {
                    // Attribute values may not contain `<` or the quote; `&lt;`
                    // stands in for the former.
                    let value = value.replace('<', "&lt;").replace('"', "&quot;");
                    out.push_str(&format!(" v=\"{value}\""));
                }
                if children.is_empty() {
                    out.push_str("/>");
                } else {
                    out.push('>');
                    for child in children {
                        print(child, out);
                    }
                    out.push_str("</e>");
                }
            }
            Node::Text(t) => out.push_str(t),
            Node::Comment(t) => {
                // A comment may not contain `--` or end in `-`.
                out.push_str(&format!("<!--{}-->", t.replace('-', "_")));
            }
            Node::Cdata(t) => out.push_str(&format!("<![CDATA[{}]]>", t.replace("]]", "]_"))),
            Node::Pi(t) => out.push_str(&format!("<?pi {}?>", t.replace('?', "_"))),
        }
    }

    /// The depth `roxmltree` actually recurses to: the number of *open*
    /// (non-empty) elements on the deepest path, plus one for an empty
    /// element at the bottom (the scanner counts every start tag).
    fn true_depth(node: roxmltree::Node<'_, '_>) -> usize {
        node.children()
            .filter(|c| c.is_element())
            .map(|c| 1 + true_depth(c))
            .max()
            .unwrap_or(0)
    }

    proptest! {
        /// On well-formed input the scan is exact, however the tag-like text is
        /// hidden in attributes, comments, CDATA, and processing instructions.
        #[test]
        fn the_scan_is_exact_on_well_formed_xml(children in proptest::collection::vec(node(), 0..4)) {
            let root = Node::Element { attr: None, children };
            let mut text = String::new();
            print(&root, &mut text);
            let doc = roxmltree::Document::parse(&text)
                .unwrap_or_else(|e| panic!("generated XML must be well-formed ({e}):\n{text}"));
            prop_assert_eq!(max_nesting(&text), 1 + true_depth(doc.root_element()), "{}", text);
        }

        /// Lexical noise spliced in anywhere: whenever `roxmltree` still accepts
        /// the result, the scan still matches it exactly. (Where it rejects the
        /// text, it stops at the first error, having opened no more elements
        /// than the start tags the scan has already counted by then.)
        #[test]
        fn the_scan_tracks_roxmltree_through_lexical_noise(
            children in proptest::collection::vec(node(), 0..4),
            splices in proptest::collection::vec(
                (any::<proptest::sample::Index>(), prop_oneof![
                    Just("<"), Just(">"), Just("/"), Just("\""), Just("'"), Just("!"),
                    Just("-"), Just("["), Just("]"), Just("?"), Just("<e>"), Just("</e>"),
                    Just("<e/>"), Just("<!--"), Just("-->"), Just("<![CDATA["), Just("]]>"),
                ]),
                1..4,
            ),
        ) {
            let root = Node::Element { attr: None, children };
            let mut text = String::new();
            print(&root, &mut text);
            for (at, noise) in splices {
                let at = at.index(text.len() + 1);
                text.insert_str(at, noise);
            }
            if let Ok(doc) = roxmltree::Document::parse(&text) {
                prop_assert_eq!(max_nesting(&text), 1 + true_depth(doc.root_element()), "{}", text);
            }
        }
    }

    #[test]
    fn a_deep_document_is_refused_before_roxmltree_sees_it() {
        let depth = 1_000_000;
        let text = format!("{}{}", "<i>".repeat(depth), "</i>".repeat(depth));
        assert!(matches!(
            parse_bounded(&text),
            Err(BoundedParseError::TooDeep)
        ));
        let ok = format!(
            "{}{}",
            "<i>".repeat(MAX_FILE_DEPTH),
            "</i>".repeat(MAX_FILE_DEPTH)
        );
        assert!(parse_bounded(&ok).is_ok());
    }
}
