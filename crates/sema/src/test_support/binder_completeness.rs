//! Binder completeness: every name binder FCS defines, the walk knows about.
//!
//! The FCS differentials grade *uses*: where we commit, the oracle must agree.
//! That cannot see a binder the walk never interned unless some graded use
//! happens to sit in its scope with an outer same-named target to commit to —
//! and real code rarely writes that shape. So the class was found one review
//! round at a time: a primary-constructor parameter, a query `join` variable,
//! an `extern` parameter, each one a binder FCS introduces and our scope walk
//! did not, behind which a bare use committed to an outer target.
//!
//! This module checks the binders themselves. Every definition record FCS
//! reports for a symbol that bare lookup can find must land on a binder the
//! walk knows at that range: one it [interned](crate::ResolvedFile::defs), or
//! one it [declines](crate::ResolvedFile::opaque_binders) — a binder whose
//! uses the walk does not model, and which therefore hides every outer
//! same-named target from them. Anything else is an [`UnmodelledBinder`], and
//! the sound consequence of one is that no use of its name in its scope may
//! commit — which [`commits_behind_unmodelled_binders`] checks against the
//! oracle's own record of which uses are in that scope.

use std::fmt;

use borzoi_cst::syntax::{SyntaxKind, SyntaxNode};
use rowan::{TextRange, TextSize};

use crate::hash::HashMap;
use crate::{Resolution, ResolvedFile};

/// What sort of symbol an oracle definition record defines, restricted to the
/// sorts **bare lookup** can find: an unqualified name in expression, pattern
/// or type position.
///
/// A member, a record field and an enum case are reachable only through a
/// qualifier (a type, a receiver, a record expression's own label lookup), so
/// they are not here: an unmodelled one cannot capture a bare use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OracleBinderClass {
    /// A value that is not a member: a parameter, a pattern variable, a local
    /// or module-level `let`, a self identifier.
    Value,
    /// A union case or an exception constructor's case.
    UnionCase,
    /// One case of an active-pattern recognizer.
    ActivePatternCase,
    /// A type parameter.
    TypeParameter,
    /// A type definition: found bare in type position.
    Type,
}

impl OracleBinderClass {
    /// The class of a record in the `uses` currency: FCS's `SymbolKind` and,
    /// for a value, its `Value.Kind` (`local` / `module` / `member`).
    ///
    /// `None` for everything bare lookup cannot find, and for a value FCS could
    /// not classify. A caller must count the second kind of `None` rather than
    /// drop it ([`is_unclassified_value`]): an unread kind has to stay visible
    /// instead of vanishing into "not a binder".
    pub fn of_symbol_kind(symbol_kind: Option<&str>, value_kind: Option<&str>) -> Option<Self> {
        match symbol_kind? {
            "member" => match value_kind? {
                "local" | "module" => Some(Self::Value),
                _ => None,
            },
            "unioncase" => Some(Self::UnionCase),
            "activepatterncase" => Some(Self::ActivePatternCase),
            "genericparameter" => Some(Self::TypeParameter),
            "type" => Some(Self::Type),
            _ => None,
        }
    }
}

/// Whether a record is a value FCS did not classify as local, module-level or
/// member — the `None` of [`OracleBinderClass::of_symbol_kind`] that is a
/// failure to read rather than a verdict.
pub fn is_unclassified_value(symbol_kind: Option<&str>, value_kind: Option<&str>) -> bool {
    symbol_kind == Some("member") && !matches!(value_kind, Some("local" | "module" | "member"))
}

/// One oracle definition record, in the currency this check reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleBinder {
    /// FCS's display name for the symbol.
    pub name: String,
    /// The defining occurrence's span in the checked file.
    pub range: TextRange,
    pub class: OracleBinderClass,
    /// FCS made the value rather than the author writing it (`_arg1`, the
    /// receiver `_` of a missing self identifier). Carried, not filtered: a
    /// compiler-made binder still shadows, and its range is where the check
    /// says what construct made it.
    pub compiler_generated: bool,
    /// Where the oracle says the symbol is declared, when in the checked file.
    pub decl: Option<TextRange>,
}

impl OracleBinder {
    /// Whether this record introduces a name a use can capture.
    ///
    /// Two kinds of definition record do not:
    ///
    /// - one whose symbol is declared somewhere else. FCS flags more than
    ///   binders as definitions: an active-pattern case constructed in its own
    ///   recognizer's body (`then Even`), and the head of an augmentation
    ///   (`type Expr with`), both carry `IsFromDefinition` while the symbol's
    ///   declaration is the case in `(|Even|Odd|)` or the type itself. Such a
    ///   record re-names an existing symbol; it binds nothing new.
    /// - a binder spelled `_`, which no use can name: the wildcard self
    ///   identifier of `member _.M`, the `_` of `use _ = …`.
    pub fn introduces_a_name(&self) -> bool {
        self.decl == Some(self.range) && self.name != "_"
    }

    /// The binders an oracle record witnesses: the record itself, where it is
    /// a declaration, and the declaration a use names.
    ///
    /// The second is what keeps a missing definition record from reading as
    /// proof that no binder exists. FCS reports definitions unevenly — a type
    /// parameter's declaring occurrence comes as a use whose declaration is
    /// itself, and a later or-pattern alternative's `_n` not at all — but
    /// every use it binds to a binder names that binder's declaration. A
    /// compiler-made value's use is not followed: FCS can declare one where
    /// nothing is written (`member val X`'s backing value, declared at its
    /// initialiser).
    pub fn witnessed(self) -> impl Iterator<Item = OracleBinder> {
        let declared = (!self.compiler_generated)
            .then_some(self.decl)
            .flatten()
            .filter(|d| *d != self.range)
            .map(|d| OracleBinder {
                name: self.name.clone(),
                range: d,
                class: self.class,
                compiler_generated: false,
                decl: Some(d),
            });
        std::iter::once(self).chain(declared)
    }
}

/// The syntactic construct a binder sits in, read off our own tree: the
/// binder's innermost node, then the first two enclosing nodes that are not
/// patterns. A label for grouping a census by construct, not an identity —
/// two constructs that happen to share the label count together.
///
/// Derived rather than enumerated so that a construct no one thought of still
/// gets a bucket of its own; the closed table of constructs the generators
/// cover lives with them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BinderSite(pub String);

impl fmt::Display for BinderSite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An oracle binder the walk neither interned nor declines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmodelledBinder {
    pub name: String,
    pub range: TextRange,
    pub class: OracleBinderClass,
    pub compiler_generated: bool,
    pub site: BinderSite,
}

/// How the walk knows an oracle binder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinderVerdict {
    /// A def of ours starts and ends where the oracle's binder does.
    Interned,
    /// The walk records a binder there whose uses it declines.
    Opaque,
    Unmodelled,
}

/// What the walk knows about `binder`.
pub fn binder_verdict(rf: &ResolvedFile, binder: &OracleBinder) -> BinderVerdict {
    if rf.defs().any(|d| d.range == binder.range) {
        BinderVerdict::Interned
    } else if rf.opaque_binders().contains(&binder.range)
        || rf.in_opaque_region(binder.range)
        || ResolvedFile::declines_everywhere(&binder.name)
    {
        BinderVerdict::Opaque
    } else {
        BinderVerdict::Unmodelled
    }
}

/// The binders FCS reports a definition for whose name no bare lookup reaches,
/// keyed by the node directly holding the binder's token. A binder here is not
/// one the walk needs to know: nothing it could capture exists. Each entry is
/// held to that claim by a row of `binder_site_grid` that plants a bare use of
/// the name in the binder's would-be scope and checks FCS binds it elsewhere.
pub const UNSCOPED_BINDER_LEAVES: &[(SyntaxKind, &str)] = &[(
    SyntaxKind::AUTO_PROPERTY,
    "`member val X`: FCS reports the compiler's backing value `X` as a \
     definition, but a bare `X` in the type's body reaches the enclosing scope",
)];

/// Whether the binder at `range` is one no bare lookup reaches
/// ([`UNSCOPED_BINDER_LEAVES`]).
pub fn is_unscoped(root: &SyntaxNode, range: TextRange) -> bool {
    binder_leaf(root, range).is_some_and(|leaf| {
        UNSCOPED_BINDER_LEAVES
            .iter()
            .any(|(kind, _)| *kind == leaf.kind())
    })
}

/// The node directly holding the token the binder at `range` starts with.
fn binder_leaf(root: &SyntaxNode, range: TextRange) -> Option<SyntaxNode> {
    root.token_at_offset(range.start().min(root.text_range().end()))
        .right_biased()?
        .parent()
}

/// Every one of `oracle`'s binders the walk does not know, in source order.
/// A binder no bare lookup reaches ([`is_unscoped`]) is not one it needs to.
pub fn unmodelled_binders(
    rf: &ResolvedFile,
    root: &SyntaxNode,
    oracle: &[OracleBinder],
) -> Vec<UnmodelledBinder> {
    let mut out: Vec<UnmodelledBinder> = oracle
        .iter()
        .filter(|b| b.introduces_a_name())
        .filter(|b| binder_verdict(rf, b) == BinderVerdict::Unmodelled)
        .filter(|b| !is_unscoped(root, b.range))
        .map(|b| UnmodelledBinder {
            name: b.name.clone(),
            range: b.range,
            class: b.class,
            compiler_generated: b.compiler_generated,
            site: binder_site(root, b.range),
        })
        .collect();
    out.sort_by_key(|b| (b.range.start(), b.range.end()));
    out.dedup_by_key(|b| b.range);
    out
}

/// An oracle use of a symbol, in the currency
/// [`commits_behind_unmodelled_binders`] reads: where it is, and where the
/// oracle says its symbol is declared in the same file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OracleUse {
    pub range: TextRange,
    /// `None` for a declaration outside the checked file.
    pub decl: Option<TextRange>,
}

/// A use the oracle binds to an unmodelled binder, at which we committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitBehindUnmodelled {
    pub binder: UnmodelledBinder,
    pub use_range: TextRange,
    pub committed: Resolution,
}

/// The hard half of the check: at every use the oracle binds to an
/// [unmodelled](UnmodelledBinder) binder, we must not have committed.
///
/// The oracle's own records are the scope: a use it binds to the binder is
/// one in the binder's scope that nothing nearer shadows. A commit there
/// cannot be right — we have no def at the binder's range to name — so this is
/// a divergence whatever the commit names, reported with the binder that
/// explains it.
pub fn commits_behind_unmodelled_binders(
    rf: &ResolvedFile,
    unmodelled: &[UnmodelledBinder],
    uses: &[OracleUse],
) -> Vec<CommitBehindUnmodelled> {
    let by_range: HashMap<TextRange, &UnmodelledBinder> =
        unmodelled.iter().map(|b| (b.range, b)).collect();
    let mut out = Vec::new();
    for u in uses {
        let Some(binder) = u.decl.and_then(|d| by_range.get(&d)) else {
            continue;
        };
        if u.range == binder.range {
            continue;
        }
        if let Some(
            res @ (Resolution::Local(_)
            | Resolution::Item(_)
            | Resolution::Entity(_)
            | Resolution::Member { .. }),
        ) = rf.resolution_at(u.range)
        {
            out.push(CommitBehindUnmodelled {
                binder: (*binder).clone(),
                use_range: u.range,
                committed: res,
            });
        }
    }
    out
}

/// The [`BinderSite`] of the binder spanning `range`.
pub fn binder_site(root: &SyntaxNode, range: TextRange) -> BinderSite {
    let Some(leaf) = binder_leaf(root, range) else {
        return BinderSite("<no token>".to_string());
    };
    let mut label = format!("{:?}", leaf.kind());
    let mut outer = leaf
        .ancestors()
        .skip(1)
        .filter(|n| !is_pattern_glue(n.kind()));
    for _ in 0..2 {
        match outer.next() {
            Some(n) => {
                label.push('<');
                label.push_str(&format!("{:?}", n.kind()));
            }
            None => break,
        }
    }
    BinderSite(label)
}

/// The nodes a binder's site label looks through: pattern structure and the
/// identifier paths inside it, which say how a binder is spelled, not which
/// construct binds it.
fn is_pattern_glue(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        NAMED_PAT
            | LONG_IDENT_PAT
            | ACTIVE_PAT_NAME
            | NAME_PAT_PAIRS
            | NAME_PAT_PAIR
            | WILDCARD_PAT
            | PAREN_PAT
            | CONST_PAT
            | NULL_PAT
            | TYPED_PAT
            | TUPLE_PAT
            | AS_PAT
            | ARRAY_OR_LIST_PAT
            | RECORD_PAT
            | RECORD_PAT_FIELD
            | IS_INST_PAT
            | LIST_CONS_PAT
            | ANDS_PAT
            | OR_PAT
            | ATTRIB_PAT
            | OPTIONAL_VAL_PAT
            | QUOTE_PAT
            | LONG_IDENT
            | TYPAR_DECL
            | TYPAR_DECLS
    )
}

/// The position of `offset` as `line:col`, 1-based line and 0-based column, for
/// a report a person reads.
pub fn line_col(src: &str, offset: TextSize) -> (usize, usize) {
    let offset = usize::from(offset).min(src.len());
    let before = &src[..offset];
    let line = before.matches('\n').count() + 1;
    let col = before.rfind('\n').map_or(offset, |nl| offset - nl - 1);
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(start: u32, end: u32) -> TextRange {
        TextRange::new(start.into(), end.into())
    }

    fn record(name: &str, at: TextRange, decl: Option<TextRange>, generated: bool) -> OracleBinder {
        OracleBinder {
            name: name.to_string(),
            range: at,
            class: OracleBinderClass::Value,
            compiler_generated: generated,
            decl,
        }
    }

    #[test]
    fn only_a_record_declaring_itself_introduces_a_name() {
        assert!(record("x", range(1, 2), Some(range(1, 2)), false).introduces_a_name());
        // A re-naming of a symbol declared elsewhere: a case built in its own
        // recognizer's body, an augmentation's head.
        assert!(!record("A", range(9, 10), Some(range(1, 2)), false).introduces_a_name());
        assert!(!record("T", range(9, 10), None, false).introduces_a_name());
        // No use can name `_`.
        assert!(!record("_", range(1, 2), Some(range(1, 2)), false).introduces_a_name());
    }

    #[test]
    fn a_use_witnesses_the_binder_it_names_unless_the_compiler_made_it() {
        let declared: Vec<TextRange> = record("x", range(9, 10), Some(range(1, 2)), false)
            .witnessed()
            .filter(OracleBinder::introduces_a_name)
            .map(|b| b.range)
            .collect();
        assert_eq!(declared, vec![range(1, 2)]);
        let generated = record("X", range(9, 10), Some(range(12, 13)), true)
            .witnessed()
            .filter(OracleBinder::introduces_a_name)
            .count();
        assert_eq!(generated, 0);
    }

    #[test]
    fn bare_lookup_classes() {
        let class = OracleBinderClass::of_symbol_kind;
        assert_eq!(
            class(Some("member"), Some("local")),
            Some(OracleBinderClass::Value)
        );
        assert_eq!(
            class(Some("member"), Some("module")),
            Some(OracleBinderClass::Value)
        );
        assert_eq!(class(Some("member"), Some("member")), None);
        assert_eq!(class(Some("field"), None), None);
        assert_eq!(class(Some("module"), None), None);
        assert!(is_unclassified_value(Some("member"), None));
        assert!(!is_unclassified_value(Some("member"), Some("member")));
    }
}
