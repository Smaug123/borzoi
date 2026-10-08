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
    /// inner), or by a `<member>` outside any `<members>` (Roslyn indexes every
    /// `<member>`, last one wins). See [`Self::read_as_roslyn_reads`].
    roslyn_divergent: HashSet<String>,
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

impl DocFile {
    /// Decode a documentation file's bytes: UTF-8 (with or without a BOM), or
    /// UTF-16 with a BOM. Nothing else is guessed at.
    pub fn decode(bytes: &[u8]) -> Result<String, DocFileError> {
        if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
            return String::from_utf8(rest.to_vec()).map_err(|_| DocFileError::Encoding);
        }
        let utf16 = |rest: &[u8], from: fn([u8; 2]) -> u16| {
            if !rest.len().is_multiple_of(2) {
                return Err(DocFileError::Encoding);
            }
            let units: Vec<u16> = rest
                .chunks_exact(2)
                .map(|pair| from([pair[0], pair[1]]))
                .collect();
            String::from_utf16(&units).map_err(|_| DocFileError::Encoding)
        };
        if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
            return utf16(rest, u16::from_le_bytes);
        }
        if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
            return utf16(rest, u16::from_be_bytes);
        }
        String::from_utf8(bytes.to_vec()).map_err(|_| DocFileError::Encoding)
    }

    /// Parse and index a documentation file's text.
    pub fn parse(text: impl Into<Arc<str>>) -> Result<DocFile, DocFileError> {
        let text: Arc<str> = text.into();
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
        // name under `<members>`, nested ones included — and re-parses each
        // entry on its own, losing a namespace declared outside it. Any
        // `member` element outside the overlap, or in a namespace, marks its
        // key as read differently.
        let mut roslyn_divergent: HashSet<String> = HashSet::new();
        for member in root
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "member")
        {
            let indexed_here = member.tag_name().namespace().is_none()
                && member.ancestors().any(|a| a.has_tag_name("members"));
            let nested = member
                .ancestors()
                .skip(1)
                .any(|a| a.is_element() && a.tag_name().name() == "member");
            if (!indexed_here || nested)
                && let Some(key) = member.attribute("name")
            {
                roslyn_divergent.insert(key.to_string());
            }
        }
        // Entries are the `<member>` elements under a `<members>` element
        // anywhere in the `<doc>` — including one nested inside another entry
        // (seen in NuGet packages), which documents its own key. The outer
        // entry's range still covers it; the renderer does not show a nested
        // `<member>` as the outer symbol's documentation.
        let members = doc_element
            .descendants()
            .filter(|n| n.has_tag_name("member"))
            .filter(|m| m.ancestors().any(|a| a.has_tag_name("members")));
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
        Ok(DocFile {
            text,
            assembly,
            entries,
            roslyn_divergent,
        })
    }

    /// Whether Roslyn's `XmlDocumentationProvider` reads the entry for `key`
    /// exactly as [`Self::entry`] does — including, for a key this index has
    /// no entry for, whether Roslyn also has none. It indexes every element
    /// named `member` (last one wins) and reads each outer one whole, so the
    /// two agree unless the key also sits on a nested `<member>`, one outside
    /// `<members>`, or a namespaced one. `<inheritdoc>` expansion reproduces
    /// Roslyn, so it reads — and reads the absence of — only entries for which
    /// this holds. (A namespaced element *inside* an entry is the expansion's
    /// own concern: it declines on one.)
    pub fn read_as_roslyn_reads(&self, key: &str) -> bool {
        !self.roslyn_divergent.contains(key)
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
