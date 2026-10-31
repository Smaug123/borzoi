//! One sidecar `.xml` documentation file, indexed by documentation-comment ID.
//!
//! The file format is the one Roslyn and fsc both write (ECMA-334 §D.4):
//!
//! ```xml
//! <doc>
//!   <assembly><name>System.Console</name></assembly>
//!   <members>
//!     <member name="M:System.Console.WriteLine(System.String)">…</member>
//!   </members>
//! </doc>
//! ```
//!
//! Indexing is **exact or nothing**. A key that occurs twice with differing
//! content is [`DocEntry::Ambiguous`] rather than first-wins or last-wins, and a
//! file that does not parse, or is not shaped like a doc file, is an error for
//! the whole file — never a partial index, which a lookup could not tell apart
//! from a complete one and would read a missing entry as "documented as nothing".
//!
//! The index is built in one pass but stores only each entry's **byte range**
//! in the retained text; [`DocFile::entry`] re-parses that range on demand. The
//! largest shipped files (`System.Runtime.xml`, ~7.5 MB) hold tens of thousands
//! of entries of which a session reads a handful, and an owned tree for every one
//! of them costs several times the text. Re-parsing a range is equivalent to
//! reading the element out of the whole-document parse because the index refuses
//! a document carrying a DTD (`roxmltree`'s default), so no entity can be defined
//! outside the range; and namespaces cannot change a local name. The
//! `xml_doc_shipped_sweep` test checks the equivalence on every entry of every
//! shipped file.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use super::depth::parse_bounded;
use super::tree::{DocElement, TooDeep};

/// A parsed and indexed documentation file.
#[derive(Debug)]
pub struct DocFile {
    /// The whole file text, which every [`DocEntry::Unique`] range indexes.
    text: Arc<str>,
    /// The `<doc><assembly><name>` the file declares, trimmed; `None` when the
    /// file has no such element (it is optional in practice).
    assembly: Option<String>,
    entries: HashMap<String, DocEntry>,
    /// Keys whose entry Roslyn's `XmlDocumentationProvider` would read
    /// differently from this index: a key carried by a `<member>` nested
    /// inside another (Roslyn reads the outer one whole and never indexes the
    /// inner), or by a `<member>` outside the indexed `<doc>`'s `<members>`
    /// (Roslyn indexes every `<member>` in the document, last one wins). See
    /// [`Self::read_as_roslyn_reads`].
    roslyn_divergent: HashSet<String>,
    /// Whether Roslyn may read *every* key differently: some `member`
    /// element is namespaced, or carries a namespaced attribute, which this
    /// index's local-name matching may take for its `name` where Roslyn
    /// matches qualified names.
    roslyn_divergent_everywhere: bool,
}

/// What the index knows about one documentation-comment ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocEntry {
    /// Exactly one `<member>` element carries this ID (or several carry it with
    /// byte-identical content): its byte range in the file text.
    Unique(Range<usize>),
    /// Several `<member>` elements carry this ID with differing content. Either
    /// could be the right one, so neither is shown.
    Ambiguous,
}

/// Why a documentation file yields no index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocFileError {
    /// The bytes are neither UTF-8 nor BOM-marked UTF-16.
    Encoding,
    /// The text is not well-formed XML (or carries a DTD, which is refused).
    Malformed(String),
    /// There is no `<doc>` element — this is some other XML file that happens
    /// to sit next to a DLL.
    NotADocFile { root: String },
    /// A `<doc redirect="…">` stub: the documentation lives at another path.
    /// That indirection is a .NET Framework reference-assembly convention, and
    /// following it is not implemented.
    Redirect(String),
}

/// Why one entry of an indexed file yields no tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryError {
    /// The entry's range did not re-parse as a standalone element — e.g. it uses
    /// a namespace prefix declared on an ancestor.
    Malformed(String),
    /// The entry nests elements deeper than [`super::tree::MAX_DEPTH`].
    TooDeep,
}

/// The `<member>` elements Roslyn's `XmlDocumentationProvider` reads. It
/// reads the document node by node, and on an element named `member` that has
/// a `name` attribute it takes the element whole (`ReadOuterXml`). That leaves
/// the reader *on* the following node, which the loop's next `Read` then steps
/// past unseen. So a `<member>` directly after one read whole is never read
/// itself, although the nodes inside it are walked.
fn members_roslyn_reads(document: roxmltree::Node<'_, '_>) -> HashSet<roxmltree::NodeId> {
    fn walk(
        node: roxmltree::Node<'_, '_>,
        read: &mut HashSet<roxmltree::NodeId>,
        unseen: &mut bool,
    ) {
        for child in node.children() {
            let seen = !std::mem::take(unseen);
            if !child.is_element() {
                continue;
            }
            if seen && child.tag_name().name() == "member" && child.attribute("name").is_some() {
                read.insert(child.id());
                *unseen = true;
                continue;
            }
            walk(child, read, unseen);
            // The element's end tag, when it has content, is the node a step
            // past its last child lands on; an empty element set nothing.
            *unseen = false;
        }
    }
    let mut read = HashSet::new();
    walk(document, &mut read, &mut false);
    read
}

/// The encoding a documentation file's bytes were decoded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceEncoding {
    Utf8,
    Utf16,
}

/// Whether the whole document is in the grammar Roslyn's `XmlReader` is
/// known to read as this parser does. Outside it the reader may refuse the
/// file (its provider then reads no entry at all) or read other text, so
/// nothing in it is certified.
///
/// The grammar is an allow-list, deliberately small, over the text left once
/// the one byte-order mark the decoder recognises is gone:
///
/// - an optional XML declaration at the very start, of version 1.0, whose
///   encoding, if declared, is the one the bytes were decoded from;
/// - then elements, character data, CDATA sections and comments, with only
///   XML whitespace and comments outside the root element;
/// - every element and attribute name made of ASCII letters, digits, `_`,
///   `-` and `.` and the Latin letters every edition of XML allows
///   ([`name_char_reads_alike`]), starting with a letter or `_` — no colon,
///   so no namespace prefix, `xml:space` or `xmlns:` declaration;
/// - no `xmlns` attribute, except a signed file's: the XML-signature
///   namespace on `<Signature>` and Microsoft's timestamp namespace on
///   `<CounterSignature>` ([`SIGNATURE_NAMESPACES`]), beside the entries;
/// - references only to the five predefined entities or to characters XML
///   allows other than tab, line feed and carriage return: Roslyn's provider
///   reads an entry by writing it back out (`ReadOuterXml`), which spells
///   those literally, and re-reading normalises them (to a space in an
///   attribute value, a carriage return to a line feed in text);
/// - no processing instruction, document type declaration or other markup.
///
/// Machine-written documentation files fall inside it; the shipped-docs
/// sweep counts the ones that do not.
fn read_whole_as_roslyn(text: &str, encoding: SourceEncoding) -> bool {
    let Some(start) = declaration_end(text, encoding) else {
        return false;
    };
    let mut rest = &text[start..];
    let mut depth = 0usize;
    while !rest.is_empty() {
        let consumed = if let Some(body) = rest.strip_prefix("<!--") {
            comment_len(body).map(|n| "<!--".len() + n)
        } else if let Some(body) = rest.strip_prefix("<![CDATA[") {
            body.find("]]>")
                .filter(|&end| depth > 0 && body[..end].chars().all(is_xml_char))
                .map(|end| "<![CDATA[".len() + end + "]]>".len())
        } else if let Some(body) = rest.strip_prefix("</") {
            depth = match depth.checked_sub(1) {
                Some(d) => d,
                None => return false,
            };
            end_tag_len(body).map(|n| "</".len() + n)
        } else if let Some(body) = rest.strip_prefix('<') {
            start_tag(body).map(|(n, empty)| {
                if !empty {
                    depth += 1;
                }
                "<".len() + n
            })
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            let data = &rest[..end];
            let alike = if depth == 0 {
                data.chars().all(is_xml_whitespace)
            } else {
                !data.contains("]]>") && references_read_alike(data)
            };
            alike.then_some(end)
        };
        let Some(consumed) = consumed else {
            return false;
        };
        rest = &rest[consumed..];
    }
    depth == 0
}

/// The default namespaces [`read_whole_as_roslyn`]'s grammar allows, each on
/// its one element: those of a signed documentation file's signature.
const SIGNATURE_NAMESPACES: [(&str, &str); 2] = [
    ("Signature", "http://www.w3.org/2000/09/xmldsig#"),
    (
        "CounterSignature",
        "http://schemas.microsoft.com/xmldsig/timestamp/2003",
    ),
];

fn is_xml_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

/// XML 1.0's `Char` production.
fn is_xml_char(c: char) -> bool {
    matches!(
        u32::from(c),
        0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF
    )
}

/// The length of a comment's body and its `-->`, after the `<!--`: none for
/// a body holding `--` or ending in `-`, or holding a character XML refuses.
fn comment_len(body: &str) -> Option<usize> {
    let end = body.find("--")?;
    (body[end..].starts_with("-->") && body[..end].chars().all(is_xml_char))
        .then_some(end + "-->".len())
}

/// The length of a name of the grammar's at the start of `text`.
fn name_len(text: &str) -> Option<usize> {
    let mut chars = text.char_indices();
    let (_, first) = chars.next()?;
    if !(first == '_' || (first.is_alphabetic() && name_char_reads_alike(first))) {
        return None;
    }
    let end = chars
        .find(|&(_, c)| {
            !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.')) || !name_char_reads_alike(c)
        })
        .map_or(text.len(), |(i, _)| i);
    Some(end)
}

/// The length of an end tag after its `</`.
fn end_tag_len(body: &str) -> Option<usize> {
    let name = name_len(body)?;
    let after = body[name..].trim_start_matches(is_xml_whitespace);
    after.starts_with('>').then(|| body.len() - after.len() + 1)
}

/// The length of a start tag after its `<`, and whether it is empty (`/>`).
fn start_tag(body: &str) -> Option<(usize, bool)> {
    let name_end = name_len(body)?;
    let element = &body[..name_end];
    let mut rest = &body[name_end..];
    loop {
        let trimmed = rest.trim_start_matches(is_xml_whitespace);
        let spaced = trimmed.len() < rest.len();
        rest = trimmed;
        if let Some(after) = rest.strip_prefix("/>") {
            return Some((body.len() - after.len(), true));
        }
        if let Some(after) = rest.strip_prefix('>') {
            return Some((body.len() - after.len(), false));
        }
        if !spaced {
            return None;
        }
        let attr_end = name_len(rest)?;
        let attribute = &rest[..attr_end];
        rest = rest[attr_end..].trim_start_matches(is_xml_whitespace);
        rest = rest
            .strip_prefix('=')?
            .trim_start_matches(is_xml_whitespace);
        let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
        let (value, after) = rest[1..].split_once(quote)?;
        if value.contains('<') || !references_read_alike(value) {
            return None;
        }
        if attribute == "xmlns" && !SIGNATURE_NAMESPACES.contains(&(element, value)) {
            return None;
        }
        rest = after;
    }
}

/// Whether every character of `data` is one XML allows and every `&` starts
/// a reference to a predefined entity or to such a character, except the
/// whitespace that [`read_whole_as_roslyn`]'s round trip normalises.
fn references_read_alike(data: &str) -> bool {
    if !data.chars().all(is_xml_char) {
        return false;
    }
    let mut rest = data;
    while let Some(at) = rest.find('&') {
        let Some((reference, after)) = rest[at + 1..].split_once(';') else {
            return false;
        };
        let code = if let Some(hex) = reference.strip_prefix("#x") {
            Some(hex)
                .filter(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit()))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
        } else if let Some(dec) = reference.strip_prefix('#') {
            Some(dec)
                .filter(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
                .and_then(|d| d.parse::<u32>().ok())
        } else if matches!(reference, "lt" | "gt" | "amp" | "quot" | "apos") {
            Some(u32::from('&'))
        } else {
            None
        };
        if !code
            .and_then(char::from_u32)
            .is_some_and(|c| is_xml_char(c) && !matches!(c, '\t' | '\n' | '\r'))
        {
            return false;
        }
        rest = after;
    }
    true
}

/// The length of the document's XML declaration, 0 if it has none: `None`
/// for one outside the grammar of [`read_whole_as_roslyn`]. A processing
/// instruction whose target merely starts with `xml` (`<?xml-stylesheet …?>`)
/// is no declaration (and is outside the grammar too).
fn declaration_end(text: &str, encoding: SourceEncoding) -> Option<usize> {
    let Some(rest) = text.strip_prefix("<?xml") else {
        return Some(0);
    };
    if !rest.starts_with([' ', '\t', '\r', '\n']) {
        return Some(0);
    }
    let (body, after) = rest.split_once("?>")?;
    let end = text.len() - after.len();
    // `name = 'value'` pairs, in order, whitespace allowed around `=`.
    let mut pairs = Vec::new();
    let mut rest = body.trim_start();
    while !rest.is_empty() {
        let (name, after) = rest.split_once('=')?;
        let after = after.trim_start();
        let quote = after.chars().next().filter(|c| matches!(c, '"' | '\''))?;
        let (value, tail) = after[1..].split_once(quote)?;
        pairs.push((name.trim(), value));
        if !tail.is_empty() && !tail.starts_with([' ', '\t', '\r', '\n']) {
            return None;
        }
        rest = tail.trim_start();
    }
    let declared_encoding_matches = |value: &str| match encoding {
        SourceEncoding::Utf8 => value.eq_ignore_ascii_case("utf-8"),
        SourceEncoding::Utf16 => value.eq_ignore_ascii_case("utf-16"),
    };
    let alike = match pairs.as_slice() {
        [("version", "1.0"), rest @ ..] => match rest {
            [] => true,
            [("encoding", e)] => declared_encoding_matches(e),
            [("standalone", s)] => matches!(*s, "yes" | "no"),
            [("encoding", e), ("standalone", s)] => {
                declared_encoding_matches(e) && matches!(*s, "yes" | "no")
            }
            _ => false,
        },
        _ => false,
    };
    alike.then_some(end)
}

/// Whether `c` may appear in a name of a document in the grammar of
/// [`read_whole_as_roslyn`]: ASCII, or one of the Latin letters XML 1.0's
/// fourth edition lists as `BaseChar` (which .NET's reader implements; a
/// later edition's wider name characters it refuses).
fn name_char_reads_alike(c: char) -> bool {
    c.is_ascii()
        || matches!(
            u32::from(c),
            0xC0..=0xD6 | 0xD8..=0xF6 | 0xF8..=0x131 | 0x134..=0x13E | 0x141..=0x148 | 0x14A..=0x17E
        )
}

impl DocFile {
    /// Decode a documentation file's bytes: UTF-8 (with or without a BOM), or
    /// UTF-16 with a BOM. Nothing else is guessed at.
    pub fn decode(bytes: &[u8]) -> Result<String, DocFileError> {
        Self::decode_with_encoding(bytes).map(|(text, _)| text)
    }

    /// Decode and parse a documentation file's bytes ([`Self::decode`],
    /// [`Self::parse`]), knowing which encoding the text came from.
    pub fn load(bytes: &[u8]) -> Result<DocFile, DocFileError> {
        let (text, encoding) = Self::decode_with_encoding(bytes)?;
        Self::parse_from(text.into(), encoding)
    }

    fn decode_with_encoding(bytes: &[u8]) -> Result<(String, SourceEncoding), DocFileError> {
        let utf8 = |rest: &[u8]| {
            String::from_utf8(rest.to_vec())
                .map(|text| (text, SourceEncoding::Utf8))
                .map_err(|_| DocFileError::Encoding)
        };
        if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
            return utf8(rest);
        }
        let utf16 = |rest: &[u8], from: fn([u8; 2]) -> u16| {
            if !rest.len().is_multiple_of(2) {
                return Err(DocFileError::Encoding);
            }
            let units: Vec<u16> = rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| from(pair))
                .collect();
            String::from_utf16(&units)
                .map(|text| (text, SourceEncoding::Utf16))
                .map_err(|_| DocFileError::Encoding)
        };
        if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
            return utf16(rest, u16::from_le_bytes);
        }
        if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
            return utf16(rest, u16::from_be_bytes);
        }
        utf8(bytes)
    }

    /// Parse and index a documentation file's text.
    /// The text is taken as decoded from UTF-8 (see [`Self::load`]).
    pub fn parse(text: impl Into<Arc<str>>) -> Result<DocFile, DocFileError> {
        Self::parse_from(text.into(), SourceEncoding::Utf8)
    }

    fn parse_from(text: Arc<str>, encoding: SourceEncoding) -> Result<DocFile, DocFileError> {
        let doc = parse_bounded(&text).map_err(|e| DocFileError::Malformed(e.to_string()))?;
        let root = doc.root_element();
        // The `<doc>` element is the root, except in the files some 4.x/5.0
        // `System.*` reference packages ship, which wrap it in a `<span>`.
        let Some(doc_element) = root.descendants().find(|n| n.has_tag_name("doc")) else {
            return Err(DocFileError::NotADocFile {
                root: root.tag_name().name().to_string(),
            });
        };
        if let Some(target) = doc_element.attribute("redirect") {
            return Err(DocFileError::Redirect(target.to_string()));
        }
        let assembly = doc_element
            .children()
            .filter(|n| n.has_tag_name("assembly"))
            .flat_map(|a| a.children().filter(|n| n.has_tag_name("name")))
            .map(|n| n.text().unwrap_or("").trim().to_string())
            .next();

        let mut entries: HashMap<String, DocEntry> = HashMap::new();
        // Roslyn's provider indexes every element whose *qualified* name is
        // `member`, under `<members>` or not, except one inside another, which
        // it reads as part of the outer. This index takes `<member>`s by local
        // name under `<members>` in the one `<doc>` it selected, nested ones
        // included — and re-parses each
        // entry on its own, losing a namespace declared outside it. Any
        // `member` element outside the overlap, or in a namespace, marks its
        // key as read differently.
        //
        // Roslyn also never sees a `<member>` that its read of the previous
        // one stepped over (see [`members_roslyn_reads`]). And it matches
        // qualified names — of the element and of its `name` attribute, which
        // this index matches by local name — so a namespaced `member`, or one
        // with a namespaced attribute, anywhere marks every key, including
        // keys this index has no entry for.
        let mut roslyn_divergent: HashSet<String> = HashSet::new();
        let read_by_roslyn = members_roslyn_reads(doc.root());
        let all_members: Vec<_> = root
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "member")
            .collect();
        let any_namespaced = all_members.iter().any(|m| {
            m.tag_name().namespace().is_some() || m.attributes().any(|a| a.namespace().is_some())
        });
        // Entries are the `<member>` elements under a `<members>` element
        // anywhere in the selected `<doc>` — including one nested inside
        // another entry (seen in NuGet packages), which documents its own key.
        // The outer entry's range still covers it; the renderer does not show
        // a nested `<member>` as the outer symbol's documentation. A
        // `<member>` anywhere else in the file — under a second `<doc>`, say —
        // is outside the index but not outside Roslyn's reading.
        let members: Vec<_> = doc_element
            .descendants()
            .filter(|n| n.has_tag_name("member"))
            .filter(|m| m.ancestors().any(|a| a.has_tag_name("members")))
            .collect();
        let indexed: HashSet<roxmltree::NodeId> = members.iter().map(|m| m.id()).collect();
        for member in &all_members {
            let indexed_here =
                member.tag_name().namespace().is_none() && indexed.contains(&member.id());
            let nested = member
                .ancestors()
                .skip(1)
                .any(|a| a.is_element() && a.tag_name().name() == "member");
            let stepped_over =
                member.attribute("name").is_some() && !read_by_roslyn.contains(&member.id());
            if (any_namespaced || !indexed_here || nested || stepped_over)
                && let Some(key) = member.attribute("name")
            {
                roslyn_divergent.insert(key.to_string());
            }
        }
        for member in members {
            // A `<member>` without a usable `name` (absent, or the `name=""`
            // FSharp.Core 4.0.0.1 ships hundreds of) cannot be looked up, so it
            // does not enter the index; nor can it make another entry ambiguous.
            let Some(key) = member.attribute("name").filter(|k| !k.trim().is_empty()) else {
                continue;
            };
            let range = member.range();
            match entries.entry(key.to_string()) {
                Entry::Vacant(slot) => {
                    slot.insert(DocEntry::Unique(range));
                }
                Entry::Occupied(mut slot) => {
                    let identical = matches!(
                        slot.get(),
                        DocEntry::Unique(first) if text[first.clone()] == text[range.clone()]
                    );
                    if !identical {
                        slot.insert(DocEntry::Ambiguous);
                    }
                }
            }
        }
        let roslyn_divergent_everywhere = any_namespaced || !read_whole_as_roslyn(&text, encoding);
        Ok(DocFile {
            text,
            assembly,
            entries,
            roslyn_divergent,
            roslyn_divergent_everywhere,
        })
    }

    /// Whether Roslyn's `XmlDocumentationProvider` reads the entry for `key`
    /// exactly as [`Self::entry`] does — including, for a key this index has
    /// no entry for, whether Roslyn also has none. It indexes every element
    /// named `member` (last one wins) and reads each outer one whole, so the
    /// two agree unless the key also sits on a nested `<member>` or one
    /// outside the indexed `<doc>`'s `<members>`, or the file has a
    /// namespaced `member` (or one with a namespaced attribute) anywhere.
    /// `<inheritdoc>` expansion reproduces Roslyn, so it reads — and reads the
    /// absence of — only entries for which this holds. (A namespaced element *inside* an entry is the expansion's
    /// own concern: it declines on one.)
    pub fn read_as_roslyn_reads(&self, key: &str) -> bool {
        !self.roslyn_divergent_everywhere && !self.roslyn_divergent.contains(key)
    }

    /// Whether nothing about the file as a whole makes Roslyn read any key
    /// differently: [`Self::read_as_roslyn_reads`] then turns on the key's
    /// own `<member>`s alone.
    pub fn read_whole_as_roslyn_reads(&self) -> bool {
        !self.roslyn_divergent_everywhere
    }

    /// The assembly simple name the file declares, if it declares one.
    pub fn assembly(&self) -> Option<&str> {
        self.assembly.as_deref()
    }

    /// The index entry for `key`, if the file has one.
    pub fn entry(&self, key: &str) -> Option<&DocEntry> {
        self.entries.get(key)
    }

    /// Every key in the index, in no particular order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// The `<member>` element at `range` (a [`DocEntry::Unique`] range of this
    /// file), re-parsed as a standalone element.
    pub fn member_element(&self, range: Range<usize>) -> Result<DocElement, EntryError> {
        let fragment = self
            .text
            .get(range)
            .ok_or_else(|| EntryError::Malformed("range outside the file".to_string()))?;
        let doc = parse_bounded(fragment).map_err(|e| EntryError::Malformed(e.to_string()))?;
        DocElement::from_roxmltree(doc.root_element()).map_err(|TooDeep| EntryError::TooDeep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml_doc::tree::DocNode;

    fn file(members: &str) -> String {
        format!(
            "<?xml version=\"1.0\"?>\n<doc><assembly><name> Lib </name></assembly>\
             <members>{members}</members></doc>"
        )
    }

    fn unique_element(doc: &DocFile, key: &str) -> DocElement {
        match doc.entry(key) {
            Some(DocEntry::Unique(range)) => doc.member_element(range.clone()).unwrap(),
            other => panic!("expected a unique entry for {key}, got {other:?}"),
        }
    }

    #[test]
    fn indexes_members_by_name_and_reads_the_assembly() {
        let doc = DocFile::parse(file(
            r#"<member name="T:A"><summary>a</summary></member><member name="M:A.F"><summary>f</summary></member>"#,
        ))
        .unwrap();
        assert_eq!(doc.assembly(), Some("Lib"));
        let a = unique_element(&doc, "T:A");
        assert_eq!(a.name, "member");
        assert_eq!(a.text_content(), "a");
        assert_eq!(unique_element(&doc, "M:A.F").text_content(), "f");
        assert_eq!(doc.entry("T:B"), None);
    }

    /// Shapes from shipped files: a `<span>`-wrapped `<doc>`, a `<Signature>`
    /// beside `<members>`, `name=""` entries, and a `<member>` nested in an
    /// entry — which documents its own key, so is an entry of its own (and
    /// the outer entry's renderer does not show it).
    #[test]
    fn indexes_the_shapes_shipped_files_take() {
        let doc = DocFile::parse(
            r#"<span><doc><assembly><name>Lib</name></assembly><Signature/><members>
            <member name=""><summary>x</summary></member>
            <member name="T:A"><summary>a</summary><member name="T:Inner"><summary>i</summary></member></member>
            </members></doc></span>"#,
        )
        .unwrap();
        assert_eq!(doc.assembly(), Some("Lib"));
        let mut keys: Vec<&str> = doc.keys().collect();
        keys.sort();
        assert_eq!(keys, ["T:A", "T:Inner"]);
        assert_eq!(unique_element(&doc, "T:Inner").text_content(), "i");
    }

    /// Roslyn's provider takes a `<member>` whole and then steps over the
    /// node after it unseen: one directly after another — no whitespace
    /// between — is never indexed, though one after that is.
    #[test]
    fn an_entry_directly_after_another_is_read_differently() {
        let doc = DocFile::parse(
            r#"<doc><members><member name="T:A"><summary>a</summary></member><member name="T:B"><summary>b</summary></member><member name="T:C"/><!--c--><member name="T:D"/>
            <member name="T:E"/></members></doc>"#,
        )
        .unwrap();
        assert!(doc.read_as_roslyn_reads("T:A"));
        assert!(!doc.read_as_roslyn_reads("T:B"));
        assert!(doc.read_as_roslyn_reads("T:C"));
        // The comment is the node stepped over, so `T:D` is read.
        assert!(doc.read_as_roslyn_reads("T:D"));
        assert!(doc.read_as_roslyn_reads("T:E"));
        // A `<member>` stepped over is walked into: Roslyn reads one inside it.
        let inner = DocFile::parse(
            r#"<doc><members><member name="T:A"/><member name="T:B"> <member name="T:Inner"/></member></members></doc>"#,
        )
        .unwrap();
        assert!(!inner.read_as_roslyn_reads("T:B"));
        assert!(!inner.read_as_roslyn_reads("T:Inner"));
    }

    /// Roslyn's provider indexes every element named `member` (last wins) and
    /// reads an outer one whole, never indexing the one inside it: a key on a
    /// nested, unlisted or namespaced `<member>` is read differently there —
    /// and for a key this index lacks, Roslyn may have an entry.
    #[test]
    fn keys_roslyn_reads_differently_are_marked() {
        let doc = DocFile::parse(
            r#"<doc><members>
            <member name="T:A"><summary>a</summary><member name="T:Inner"><summary>i</summary></member></member>
            <member name="T:Plain"><summary>p</summary></member>
            </members><member name="T:Stray"/></doc>"#,
        )
        .unwrap();
        assert!(doc.read_as_roslyn_reads("T:A"));
        assert!(doc.read_as_roslyn_reads("T:Plain"));
        assert!(doc.read_as_roslyn_reads("T:Absent"));
        assert!(!doc.read_as_roslyn_reads("T:Inner"));
        assert_eq!(doc.entry("T:Stray"), None);
        assert!(!doc.read_as_roslyn_reads("T:Stray"));
        // A default namespace on `<members>` puts its entries in it: Roslyn
        // reads the namespace into every element of the entry, while this
        // index's re-parse of the entry alone never sees it.
        let defaulted = DocFile::parse(
            r#"<doc><members xmlns="urn:x"><member name="T:A"><summary/></member></members></doc>"#,
        )
        .unwrap();
        assert!(defaulted.entry("T:A").is_some());
        assert!(!defaulted.read_as_roslyn_reads("T:A"));
        // A signed file's namespaced `<Signature>` is beside the entries,
        // not in them.
        let signed = DocFile::parse(
            r#"<doc><members><member name="T:A"><summary/></member></members>
            <Signature xmlns="http://www.w3.org/2000/09/xmldsig#"><SignedInfo/></Signature></doc>"#,
        )
        .unwrap();
        assert!(signed.read_as_roslyn_reads("T:A"));
    }

    /// A namespaced attribute on a `<member>` may be taken for its `name` by
    /// this index (which matches attributes by local name) where Roslyn takes
    /// only the unqualified one: every key of such a file is read differently.
    #[test]
    fn a_namespaced_attribute_on_a_member_marks_every_key() {
        let doc = DocFile::parse(
            r#"<doc><members>
            <member xmlns:p="urn:x" p:name="T:Ignored" name="T:Real"><summary>r</summary></member>
            <member name="T:Plain"><summary>p</summary></member>
            </members></doc>"#,
        )
        .unwrap();
        for key in ["T:Real", "T:Ignored", "T:Plain", "T:Absent"] {
            assert!(!doc.read_as_roslyn_reads(key), "{key}");
        }
    }

    /// Roslyn's `XmlReader` refuses some documents this parser accepts — an
    /// XML 1.1 declaration, an `xml:space` value other than `default` or
    /// `preserve` — and then reads no entry at all. (No `xml:space` is in the
    /// certified grammar, whatever its value.)
    #[test]
    fn a_document_roslyn_refuses_marks_every_key() {
        let body =
            r#"<doc><members><member name="T:A"><summary>a</summary></member></members></doc>"#;
        let refused = [
            format!(r#"<?xml version="1.1"?>{body}"#),
            r#"<doc xml:space="sometimes"><members><member name="T:A"/></members></doc>"#
                .to_string(),
            r#"<doc xml:space="preserve"><members><member name="T:A"/></members></doc>"#
                .to_string(),
        ];
        for text in refused {
            let doc = DocFile::parse(text.as_str()).unwrap();
            assert!(doc.entry("T:A").is_some(), "{text}");
            assert!(!doc.read_as_roslyn_reads("T:A"), "{text}");
        }
        let text = format!(r#"<?xml version="1.0" encoding="utf-8"?>{body}"#);
        let doc = DocFile::parse(text.as_str()).unwrap();
        assert!(doc.read_as_roslyn_reads("T:A"), "{text}");
    }

    /// Only documents whose declaration and names fall in the subset known
    /// to read alike are certified: an encoding declaration other than the
    /// bytes', or a name outside the Latin letters, may make Roslyn's reader refuse the file
    /// or read other text. A declaration spelled with spaces is no refusal;
    /// a processing instruction that merely starts `<?xml` is no declaration,
    /// and outside the grammar.
    #[test]
    fn only_the_declarations_and_names_known_to_read_alike_are_certified() {
        let body =
            r#"<doc><members><member name="T:A"><summary>a</summary></member></members></doc>"#;
        let utf16 = |s: &str| -> Vec<u8> {
            [0xFF, 0xFE]
                .into_iter()
                .chain(s.encode_utf16().flat_map(u16::to_le_bytes))
                .collect()
        };
        let cases: Vec<(&str, Vec<u8>, bool)> = vec![
            ("no declaration", body.as_bytes().to_vec(), true),
            (
                "spaced declaration",
                format!(r#"<?xml version = "1.0" ?>{body}"#).into_bytes(),
                true,
            ),
            (
                "utf-8 declared",
                format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>{body}"#)
                    .into_bytes(),
                true,
            ),
            (
                "stylesheet PI",
                format!(r#"<?xml-stylesheet href="d.xsl"?>{body}"#).into_bytes(),
                false,
            ),
            (
                "utf-16 declared over utf-8 bytes",
                format!(r#"<?xml version="1.0" encoding="utf-16"?>{body}"#).into_bytes(),
                false,
            ),
            (
                "latin-1 declared",
                format!(r#"<?xml version="1.0" encoding="iso-8859-1"?>{body}"#).into_bytes(),
                false,
            ),
            (
                "utf-16 declared over utf-16 bytes",
                utf16(&format!(r#"<?xml version="1.0" encoding="utf-16"?>{body}"#)),
                true,
            ),
            (
                "utf-8 declared over utf-16 bytes",
                utf16(&format!(r#"<?xml version="1.0" encoding="utf-8"?>{body}"#)),
                false,
            ),
            (
                "a non-ASCII element name",
                r#"<doc><members><member name="T:A"/></members><𐀀/></doc>"#
                    .as_bytes()
                    .to_vec(),
                false,
            ),
            (
                "a Latin attribute name",
                r#"<doc><members><member name="T:A" é="x"/></members></doc>"#
                    .as_bytes()
                    .to_vec(),
                true,
            ),
            (
                "a Greek attribute name",
                r#"<doc><members><member name="T:A" λ="x"/></members></doc>"#
                    .as_bytes()
                    .to_vec(),
                false,
            ),
        ];
        for (label, bytes, certified) in cases {
            let doc = DocFile::load(&bytes).unwrap_or_else(|e| panic!("{label}: {e:?}"));
            assert!(doc.entry("T:A").is_some(), "{label}");
            assert_eq!(doc.read_as_roslyn_reads("T:A"), certified, "{label}");
        }
    }

    /// Certification is an allow-list: a document outside the grammar of
    /// [`read_whole_as_roslyn`] is not certified, whatever the reason Roslyn's
    /// reader might have to refuse it — a reserved or prefixed
    /// processing-instruction target, a prefix undeclared, a second
    /// byte-order mark — and the shapes machine-written files take are.
    #[test]
    fn only_documents_in_the_certified_grammar_are_certified() {
        let body =
            r#"<doc><members><member name="T:A"><summary>a</summary></member></members></doc>"#;
        let bom = |s: &str| -> Vec<u8> {
            [0xEF, 0xBB, 0xBF]
                .iter()
                .copied()
                .chain(s.bytes())
                .collect()
        };
        let cases: Vec<(&str, Vec<u8>, bool)> = vec![
            ("plain", body.as_bytes().to_vec(), true),
            ("one BOM", bom(body), true),
            ("two BOMs", bom(&format!("\u{feff}{body}")), false),
            ("an uppercase XML target", format!(r#"<?XML version="1.0"?>{body}"#).into_bytes(), false),
            ("a prefixed target", format!(r#"<?p:q x?>{body}"#).into_bytes(), false),
            ("a stylesheet PI", format!(r#"<?xml-stylesheet href="d.xsl"?>{body}"#).into_bytes(), false),
            ("a PI inside", r#"<doc><?pi x?><members><member name="T:A"/></members></doc>"#.as_bytes().to_vec(), false),
            ("a prefix undeclared", r#"<doc xmlns:p=""><members><member name="T:A"/></members></doc>"#.as_bytes().to_vec(), false),
            ("a prefix declared", r#"<doc xmlns:p="urn:x"><members><member name="T:A"/></members></doc>"#.as_bytes().to_vec(), false),
            ("a default namespace", r#"<doc xmlns="urn:x"><members><member name="T:A"/></members></doc>"#.as_bytes().to_vec(), false),
            ("xml:space preserve", r#"<doc xml:space="preserve"><members><member name="T:A"/></members></doc>"#.as_bytes().to_vec(), false),
            (
                "a signed file",
                r#"<doc><members><member name="T:A"/></members><Signature xmlns="http://www.w3.org/2000/09/xmldsig#"><SignedInfo/></Signature></doc>"#.as_bytes().to_vec(),
                true,
            ),
            (
                "a whitespace character reference",
                r#"<doc><members><member name="T:A"><param name="a&#9;b">a&#13;b</param></member></members></doc>"#.as_bytes().to_vec(),
                false,
            ),
            (
                "a line-feed reference in text",
                r#"<doc><members><member name="T:A">a&#10;b</member></members></doc>"#.as_bytes().to_vec(),
                false,
            ),
            (
                "a space reference",
                r#"<doc><members><member name="T:A" x="a&#32;b">a&#x20;&#160;b</member></members></doc>"#.as_bytes().to_vec(),
                true,
            ),
            (
                "a signed file with a timestamp",
                r#"<doc><members><member name="T:A"/></members><Signature xmlns="http://www.w3.org/2000/09/xmldsig#"><Object><CounterSignature ts-format="cms-timestamp-message" xmlns="http://schemas.microsoft.com/xmldsig/timestamp/2003">x</CounterSignature></Object></Signature></doc>"#.as_bytes().to_vec(),
                true,
            ),
            (
                "the timestamp namespace elsewhere",
                r#"<doc xmlns="http://schemas.microsoft.com/xmldsig/timestamp/2003"><members><member name="T:A"/></members></doc>"#.as_bytes().to_vec(),
                false,
            ),
            (
                "comments, CDATA and references",
                "<?xml version=\"1.0\"?>\r\n<!-- c -->\n<doc><members>\n<member name=\"T:A\"><code><![CDATA[a < b]]></code>&lt;&#x41;&#65;</member>\n</members></doc>\n<!-- t -->\n"
                    .as_bytes()
                    .to_vec(),
                true,
            ),
        ];
        for (label, bytes, certified) in cases {
            let doc = DocFile::load(&bytes).unwrap_or_else(|e| panic!("{label}: {e:?}"));
            assert!(doc.entry("T:A").is_some(), "{label}");
            assert_eq!(doc.read_as_roslyn_reads("T:A"), certified, "{label}");
        }
    }

    /// A nested entry colliding with a top-level one is ambiguous like any
    /// other repeated key.
    #[test]
    fn a_nested_entry_takes_part_in_the_ambiguity_rule() {
        let doc = DocFile::parse(file(
            r#"<member name="T:A"><summary>a</summary><member name="T:B"><summary>one</summary></member></member>
            <member name="T:B"><summary>two</summary></member>"#,
        ))
        .unwrap();
        assert_eq!(doc.entry("T:B"), Some(&DocEntry::Ambiguous));
        assert!(matches!(doc.entry("T:A"), Some(DocEntry::Unique(_))));
    }

    #[test]
    fn a_key_repeated_with_identical_content_stays_unique() {
        let m = r#"<member name="T:A"><summary>a</summary></member>"#;
        let doc = DocFile::parse(file(&format!("{m}{m}"))).unwrap();
        assert!(matches!(doc.entry("T:A"), Some(DocEntry::Unique(_))));
    }

    #[test]
    fn a_key_repeated_with_differing_content_is_ambiguous() {
        let doc = DocFile::parse(file(
            r#"<member name="T:A"><summary>a</summary></member><member name="T:A"><summary>b</summary></member><member name="T:A"><summary>c</summary></member>"#,
        ))
        .unwrap();
        assert_eq!(doc.entry("T:A"), Some(&DocEntry::Ambiguous));
        // Identical first, differing later: still ambiguous.
        let m = r#"<member name="T:A"><summary>a</summary></member>"#;
        let n = r#"<member name="T:A"><summary>b</summary></member>"#;
        let doc = DocFile::parse(file(&format!("{m}{m}{n}"))).unwrap();
        assert_eq!(doc.entry("T:A"), Some(&DocEntry::Ambiguous));
    }

    #[test]
    fn malformed_and_foreign_files_are_errors_not_empty_indexes() {
        assert!(matches!(
            DocFile::parse("<doc><members><member name=\"T:A\">"),
            Err(DocFileError::Malformed(_))
        ));
        assert_eq!(
            DocFile::parse("<Project/>").unwrap_err(),
            DocFileError::NotADocFile {
                root: "Project".to_string()
            }
        );
        assert_eq!(
            DocFile::parse(r#"<doc redirect="%PROGRAMFILESDIR%\x.xml"/>"#).unwrap_err(),
            DocFileError::Redirect(r"%PROGRAMFILESDIR%\x.xml".to_string())
        );
        // A DTD could define entities outside an entry's range, which would
        // break the range re-parse; it is refused for the whole file.
        assert!(matches!(
            DocFile::parse(r#"<!DOCTYPE doc [<!ENTITY e "x">]><doc/>"#),
            Err(DocFileError::Malformed(_))
        ));
    }

    #[test]
    fn decodes_utf8_with_and_without_bom_and_bom_marked_utf16() {
        let text = "<doc/>";
        assert_eq!(DocFile::decode(text.as_bytes()).unwrap(), text);
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(text.as_bytes());
        assert_eq!(DocFile::decode(&bom).unwrap(), text);
        let mut le = vec![0xFF, 0xFE];
        le.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(DocFile::decode(&le).unwrap(), text);
        let mut be = vec![0xFE, 0xFF];
        be.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(DocFile::decode(&be).unwrap(), text);
        assert_eq!(DocFile::decode(&[0xC3]), Err(DocFileError::Encoding));
    }

    #[test]
    fn entry_reparse_keeps_text_cdata_and_attributes_and_drops_comments() {
        let doc = DocFile::parse(file(
            r#"<member name="T:A"><summary>x &lt; y <!-- no --><![CDATA[<raw>]]><see cref="T:B"/></summary></member>"#,
        ))
        .unwrap();
        let member = unique_element(&doc, "T:A");
        assert_eq!(member.attribute("name"), Some("T:A"));
        let DocNode::Element(summary) = &member.children[0] else {
            panic!("summary element");
        };
        assert_eq!(summary.text_content(), "x < y <raw>");
        let see = summary
            .children
            .iter()
            .find_map(|c| match c {
                DocNode::Element(e) => Some(e),
                DocNode::Text(_) => None,
            })
            .unwrap();
        assert_eq!(see.attribute("cref"), Some("T:B"));
    }

    /// `roxmltree` recurses per nested element, so a hostile file must be
    /// refused before it is parsed at all — this one would abort the process.
    #[test]
    fn a_hostilely_deep_file_is_refused_rather_than_overflowing() {
        let depth = 1_000_000;
        let body = format!("{}{}", "<i>".repeat(depth), "</i>".repeat(depth));
        assert!(matches!(
            DocFile::parse(file(&format!(r#"<member name="T:A">{body}</member>"#))),
            Err(DocFileError::Malformed(_))
        ));
    }

    /// The bound is on the whole file, so a file whose deepest entry fits is
    /// accepted, and one level more is refused.
    #[test]
    fn the_depth_bound_is_exact_at_the_edge() {
        // `doc` → `members` → `member` is three levels.
        let fits = crate::xml_doc::depth::MAX_FILE_DEPTH - 3;
        let body = |n: usize| format!("{}{}", "<i>".repeat(n), "</i>".repeat(n));
        let doc = DocFile::parse(file(&format!(
            r#"<member name="T:A">{}</member>"#,
            body(fits)
        )))
        .unwrap();
        assert_eq!(unique_element(&doc, "T:A").text_content(), "");
        assert!(
            DocFile::parse(file(&format!(
                r#"<member name="T:A">{}</member>"#,
                body(fits + 1)
            )))
            .is_err()
        );
    }
}
