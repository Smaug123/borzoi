//! What the parser had to recover from, carried far enough down that a
//! consumer can refuse to speak about a declaration it did not fully parse.
//!
//! Parser recovery **drops** text it cannot consume rather than representing
//! it, so a malformed declaration's surviving children look well-formed. There
//! is no tree-local test that sees this. Five were tried and measured against
//! FCS before this module existed, and each is refuted by a real spelling:
//!
//! | proxy | refuted by |
//! |---|---|
//! | an `ERROR` **node** in the subtree | the marker is a token, not a node |
//! | an `ERROR` **token** in the subtree | zero-width ones are ordinary structure — a *clean* `KeyValuePair<int, string>` holds one |
//! | the construct's own punctuation | `let v : …KeyValuePair<int, string>. = …` leaves the application's `<`, `,`, `>` intact |
//! | overlap with the construct's range | the junk sits *beside* it: `APP_TYPE@8..60`, junk at `60..61` |
//! | the construct's range equalling its slot's | `BINDING_RETURN_INFO@5..60` and `APP_TYPE@8..60` share an end offset |
//!
//! The parser's reported error spans are the only thing that knows, and this is
//! what carries them.

use std::sync::Arc;

use std::collections::HashSet;

use borzoi_cst::parser::{FileKind, Parse};
use borzoi_cst::syntax::{SyntaxKind, SyntaxNode};
use rowan::{TextRange, TextSize};

/// The parse errors reported for a file — or the fact that whoever built this
/// did not keep them.
///
/// The distinction is the point of the type. "No errors" and "we never looked"
/// are the same value in a `Vec<TextRange>`, and a consumer reading the empty
/// vec as *proof of a clean parse* commits a type for an annotation nobody
/// wrote. This crate has published three wrong answers from exactly that
/// conflation in a neighbouring model (union cases, typar constraints, entity
/// source names), so the two readings are separate variants here and the type
/// deliberately has no `Default`.
///
/// [`Unretained`](Self::Unretained) is the *safe* value: it proves nothing, so
/// every declaration is suspect and every annotation under it declines. A
/// caller that forgets therefore loses coverage rather than becoming wrong —
/// but there is deliberately no convenience constructor for it, because the
/// path of least resistance is how the conflation gets reintroduced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxRecovery {
    /// Exactly the spans [`Parse::errors`] reported. Empty **proves** the file
    /// parsed cleanly.
    Reported(Arc<[TextRange]>),
    /// The parse's errors were not kept, so no declaration in this file can be
    /// shown to have parsed cleanly.
    Unretained,
}

impl SyntaxRecovery {
    /// The blessed constructor: take the spans straight off the parse that
    /// produced the tree being resolved.
    ///
    /// Warnings are not errors and are not read — a warning means the parser
    /// understood the text and disapproved, which is not the failure mode this
    /// type exists for.
    pub fn of(parse: &Parse) -> Self {
        SyntaxRecovery::Reported(
            parse
                .errors
                .iter()
                .map(|e| {
                    // `TextRange::new` panics on an inverted span, and this runs
                    // inside a language server on whatever the user is halfway
                    // through typing — so the range is *normalised* rather than
                    // trusted. Only the start is read, so an inverted or
                    // saturated span still lands in the right declaration.
                    let start = TextSize::new(u32::try_from(e.span.start).unwrap_or(u32::MAX));
                    let end = TextSize::new(u32::try_from(e.span.end).unwrap_or(u32::MAX));
                    TextRange::new(start.min(end), start.max(end))
                })
                .collect::<Vec<_>>()
                .into(),
        )
    }

    /// The reading to use when the language version the tree was parsed
    /// against is a **guess** rather than a known fact — an untrusted
    /// `LangVersion` provenance, or a file with no project at all.
    ///
    /// A version-gated legality check reports a feature error and parses the
    /// construct anyway, so a guess landing on the wrong side of a feature
    /// threshold turns a rejected file into a clean-looking one *from the same
    /// tree*: `#elif` is an error at F# 10 and silent at preview, and the
    /// nested-type FS0058 fires at F# 10 and is silent below. Reading the
    /// guess's empty error list as proof would license exactly the commitment
    /// this type exists to withhold.
    ///
    /// So the source is asked whether its diagnostics depend on the version at
    /// all — by comparing whole diagnostic sets across the version ladder, which
    /// costs two extra parses and is why this is not the default constructor.
    /// Almost no file's diagnostics do depend on it, which is what makes this a
    /// per-file question rather than a blanket
    /// [`Unretained`](Self::Unretained) for every untrusted project: untrusted
    /// is the *normal* state, since every real SDK project's conditional
    /// `LangVersion` default taints provenance.
    pub fn of_guessed_version(
        parse: &Parse,
        source: &str,
        symbols: &HashSet<String>,
        file_kind: FileKind,
    ) -> Self {
        if borzoi_cst::parser::diagnostics_are_version_invariant(source, symbols, file_kind) {
            SyntaxRecovery::of(parse)
        } else {
            SyntaxRecovery::Unretained
        }
    }

    /// The reading for a path that resolves names but never infers types.
    ///
    /// [`declaration_is_intact`](Self::declaration_is_intact) has exactly one
    /// consumer — the annotation gate in `infer` — so a handler that stops at
    /// resolution reads this value not at all, and asking
    /// [`of_guessed_version`](Self::of_guessed_version) there would buy its two
    /// extra parses per request for an answer nobody looks at.
    ///
    /// [`Unretained`](Self::Unretained) rather than [`of`](Self::of) because the
    /// two differ only if inference is later added to such a path, and there the
    /// directions are not symmetric: this one declines every annotation, costing
    /// results, while `of` would hand inference a reading taken at a *guessed*
    /// version — which is the wrong answer this type exists to prevent.
    pub const fn without_inference() -> Self {
        SyntaxRecovery::Unretained
    }

    /// Whether the declaration enclosing `node` parsed with no recovery — the
    /// question a consumer must answer *yes* to before committing anything it
    /// read out of that declaration's syntax.
    ///
    /// The region checked is the enclosing declaration's *recovery extent* (see
    /// `declaration_extent`), not its node range: recovery flushes what it
    /// could not parse *out* of the declaration it broke out of, so the range
    /// alone misses precisely the junk that makes the declaration
    /// untrustworthy.
    pub fn declaration_is_intact(&self, node: &SyntaxNode) -> bool {
        let spans = match self {
            SyntaxRecovery::Unretained => return false,
            // The overwhelmingly common case, and the reason this is affordable
            // to ask per annotation node: a file that parsed clean answers
            // without touching the tree at all.
            SyntaxRecovery::Reported(spans) if spans.is_empty() => return true,
            SyntaxRecovery::Reported(spans) => spans,
        };
        let extent = declaration_extent(node);
        // An error is *this* declaration's when it starts anywhere in the
        // extent — **closed** at the top, because the boundary offset is
        // genuinely ambiguous and the ambiguity resolves towards declining.
        //
        // Recovery does not always flush junk as loose tokens. It can also
        // hoist it into a fresh sibling *node*, which stops the extent dead at
        // exactly the offset the error is reported at:
        //
        // ```text
        // let v : System.String( = failwith ""
        //   LET_DECL@9..30            // `let v : System.String`
        //   ERROR@30..30 ""
        //   EXPR_DECL@30..45          // `( = failwith ""` — the junk, as a node
        // ```
        //
        // with the error at `30..31`. Read half-open, that error belongs to the
        // `EXPR_DECL` and the binding looks intact; read closed, the binding
        // declines, which is what FCS's FS0010 says it should. The same offset
        // also carries an end-of-file diagnostic when the junk runs to the end
        // of the source.
        //
        // The price is the mirror case — a following declaration whose *own*
        // first token errors also condemns its predecessor. That is a decline,
        // never a commitment, and it is narrow: an error at a declaration's
        // opening offset is overwhelmingly this same hoisted-junk shape rather
        // than an independent failure.
        !spans
            .iter()
            .any(|s| extent.start() <= s.start() && s.start() <= extent.end())
    }
}

/// The text a recovery inside `node`'s enclosing declaration can occupy: the
/// declaration's own range, extended forward over the loose tokens that follow
/// it up to the next sibling **node**.
///
/// The extension is what makes the check work at all. Tree construction here is
/// append-only — a finished node is never grown retroactively — so text the
/// parser gives up on after a declaration has closed cannot become part of it.
/// It lands as flat sibling tokens instead:
///
/// ```text
/// LET_DECL@0..60                 // `let v : …KeyValuePair<int, string>`
///   …
/// ERROR@60..61 "."               // the junk, a sibling
/// ERROR@62..63 "="
/// EXPR_DECL@64..75               // recovery restarts here
/// ```
///
/// so the extent is `0..64` and the errors at `60..61` and `62..63` fall in it.
///
/// The enclosing declaration is found by climbing to the last ancestor before a
/// declaration-*list* container. That is coarse in one direction on purpose: a
/// binding nested inside an expression is judged with its whole top-level
/// declaration, and `skip_stray_type_continuation`
/// (`borzoi_cst`'s `parser/decls_recover.rs`) can flush an entire malformed
/// `and`-continuation as one flat token run behind a perfectly clean preceding
/// declaration, which this then blames on that declaration. Both cost
/// coverage; neither can make a decline into a commitment.
fn declaration_extent(node: &SyntaxNode) -> TextRange {
    let decl = enclosing_declaration(node);
    let mut end = decl.text_range().end();
    let mut next = decl.next_sibling_or_token();
    while let Some(sibling) = next {
        match sibling {
            rowan::NodeOrToken::Node(_) => break,
            rowan::NodeOrToken::Token(t) => {
                end = t.text_range().end();
                next = t.next_sibling_or_token();
            }
        }
    }
    TextRange::new(decl.text_range().start(), end)
}

/// The declaration `node` sits in: the highest ancestor that is still a
/// *member* of a declaration list rather than the list itself.
///
/// Falls back to the file root when no container is found, which is the
/// conservative direction — a whole-file extent declines more, never less.
fn enclosing_declaration(node: &SyntaxNode) -> SyntaxNode {
    let mut current = node.clone();
    while let Some(parent) = current.parent() {
        if holds_a_declaration_list(parent.kind()) {
            return current;
        }
        current = parent;
    }
    current
}

/// Whether a node of this kind holds a *list* of declarations or members, each
/// of which is separately trustworthy.
///
/// Anything absent from this set merely makes the extent coarser (the climb
/// continues past it), so a kind added to the grammar later costs coverage
/// rather than soundness.
fn holds_a_declaration_list(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::IMPL_FILE
            | SyntaxKind::SIG_FILE
            | SyntaxKind::MODULE_OR_NAMESPACE
            | SyntaxKind::NESTED_MODULE_DECL
            | SyntaxKind::TYPE_DEFNS
            | SyntaxKind::OBJECT_MODEL_REPR
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use borzoi_cst::parser::parse;
    use borzoi_cst::syntax::AstNode;
    use borzoi_cst::syntax::ImplFile;

    /// The `Type` node of the first annotation in `source`.
    fn annotation(root: &SyntaxNode) -> SyntaxNode {
        root.descendants()
            .find(|n| n.kind() == SyntaxKind::BINDING_RETURN_INFO)
            .expect("the source annotates a binding")
            .children()
            .next()
            .expect("the annotation slot holds a type")
    }

    /// The headline case: junk flushed *outside* the declaration still condemns
    /// it. Without the forward extension this returns `true` and the annotation
    /// commits.
    #[test]
    fn junk_beside_a_declaration_condemns_it() {
        let source = "module M\nlet v : System.String. = failwith \"\"\n";
        let parsed = parse(source);
        assert!(
            !parsed.errors.is_empty(),
            "the spelling must not parse clean"
        );
        let root = ImplFile::cast(parsed.root.clone()).expect("impl file");
        let recovery = SyntaxRecovery::of(&parse(source));
        assert!(!recovery.declaration_is_intact(&annotation(root.syntax())));
    }

    /// A clean declaration beside a broken one keeps its own verdict — the
    /// check is per-declaration, not per-file, which is what lets the feature
    /// survive an editor mid-keystroke.
    #[test]
    fn a_neighbouring_declaration_is_judged_separately() {
        let source = "module M\nlet a : System.String = failwith \"\"\nlet b : System.String. = failwith \"\"\n";
        let parsed = parse(source);
        assert!(
            !parsed.errors.is_empty(),
            "the spelling must not parse clean"
        );
        let recovery = SyntaxRecovery::of(&parsed);
        let root = ImplFile::cast(parsed.root.clone()).expect("impl file");
        let slots: Vec<_> = root
            .syntax()
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::BINDING_RETURN_INFO)
            .map(|n| n.children().next().expect("a type"))
            .collect();
        assert_eq!(slots.len(), 2, "one annotation per binding");
        assert!(
            recovery.declaration_is_intact(&slots[0]),
            "`a` parsed cleanly and must keep its annotation"
        );
        assert!(
            !recovery.declaration_is_intact(&slots[1]),
            "`b` did not parse cleanly"
        );
    }

    /// A clean file proves itself, and does so without consulting the tree.
    #[test]
    fn a_clean_parse_is_intact() {
        let parsed = parse("module M\nlet v : System.String = failwith \"\"\n");
        assert!(parsed.errors.is_empty());
        let root = ImplFile::cast(parsed.root.clone()).expect("impl file");
        assert!(SyntaxRecovery::of(&parsed).declaration_is_intact(&annotation(root.syntax())));
    }

    /// `Unretained` proves nothing, so it condemns a declaration that in fact
    /// parsed perfectly. This is the safe direction and the reason the variant
    /// exists.
    #[test]
    fn unretained_condemns_even_a_clean_declaration() {
        let parsed = parse("module M\nlet v : System.String = failwith \"\"\n");
        assert!(parsed.errors.is_empty());
        let root = ImplFile::cast(parsed.root.clone()).expect("impl file");
        assert!(!SyntaxRecovery::Unretained.declaration_is_intact(&annotation(root.syntax())));
    }

    /// A member's annotation is judged against its own member, not the whole
    /// type: `OBJECT_MODEL_REPR` is a declaration list.
    #[test]
    fn a_member_is_judged_separately_from_its_sibling() {
        let source = "module M\ntype T =\n  member this.A : System.String = failwith \"\"\n  member this.B : System.String. = failwith \"\"\n";
        let parsed = parse(source);
        assert!(
            !parsed.errors.is_empty(),
            "the spelling must not parse clean"
        );
        let recovery = SyntaxRecovery::of(&parsed);
        let root = ImplFile::cast(parsed.root.clone()).expect("impl file");
        let slots: Vec<_> = root
            .syntax()
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::BINDING_RETURN_INFO)
            .map(|n| n.children().next().expect("a type"))
            .collect();
        assert_eq!(slots.len(), 2, "one annotation per member");
        assert!(recovery.declaration_is_intact(&slots[0]));
        assert!(!recovery.declaration_is_intact(&slots[1]));
    }
}
