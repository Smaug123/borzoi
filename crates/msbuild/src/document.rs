//! An XML document parsed once and held with its text, so that evaluations
//! can share it: [`crate::observed::ImportDocuments`] keeps one per imported
//! file across evaluations, and the item pass replays deferred groups
//! against the document the property pass walked.

use roxmltree::Document;

use crate::observed::TextDigest;

self_cell::self_cell!(
    /// A text and the document `roxmltree` parsed from it, which borrows it.
    struct Cell {
        owner: String,
        #[covariant]
        dependent: Document,
    }
);

/// A text and the document `roxmltree` parsed from it.
///
/// `roxmltree` documents borrow the text they were parsed from; this type owns
/// both. The self-reference is `self_cell`'s, whose macro checks at compile
/// time that the document is covariant in the borrow and fixes the drop
/// order, so this crate holds no `unsafe` of its own.
pub struct ParsedDocument {
    cell: Cell,
    digest: TextDigest,
}

impl ParsedDocument {
    /// Parse `text`, whose digest is `digest`.
    pub fn parse(text: String, digest: TextDigest) -> Result<Self, roxmltree::Error> {
        debug_assert_eq!(digest, TextDigest::of(&text));
        let cell = Cell::try_new(text, |text| Document::parse(text))?;
        Ok(Self { cell, digest })
    }

    /// The document, for as long as `self` is borrowed.
    pub fn document(&self) -> &Document<'_> {
        self.cell.borrow_dependent()
    }

    /// The text the document was parsed from.
    pub fn text(&self) -> &str {
        self.cell.borrow_owner()
    }

    /// [`Self::text`]'s digest.
    pub fn digest(&self) -> TextDigest {
        self.digest
    }
}

impl std::fmt::Debug for ParsedDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParsedDocument")
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn a_parsed_document_outlives_the_string_it_was_made_from() {
        let source = String::from("<Project><PropertyGroup><A>x</A></PropertyGroup></Project>");
        let digest = TextDigest::of(&source);
        let parsed = ParsedDocument::parse(source, digest).expect("well-formed");
        let moved = Arc::new(parsed);
        let shared = Arc::clone(&moved);
        drop(moved);
        let a = shared
            .document()
            .descendants()
            .find(|n| n.has_tag_name("A"))
            .and_then(|n| n.text());
        assert_eq!(a, Some("x"));
        assert_eq!(shared.digest(), digest);
    }

    #[test]
    fn malformed_text_fails_as_roxmltree_does() {
        let source = String::from("<Project>");
        let ours =
            ParsedDocument::parse(source.clone(), TextDigest::of(&source)).expect_err("malformed");
        let theirs = Document::parse(&source).expect_err("malformed");
        assert_eq!(ours, theirs);
    }
}
