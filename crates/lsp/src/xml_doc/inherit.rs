//! `<inheritdoc>` expansion, reproducing Roslyn's IDE — what Visual Studio
//! and the C# language server show on hover
//! (`ISymbolExtensions.GetDocumentationComment(…, expandInheritdoc: true)` in
//! Microsoft.CodeAnalysis.Workspaces).
//!
//! Roslyn rewrites the documentation *tree*; it does not merge sections. Each
//! `<inheritdoc>` element (unnamespaced, at any depth) is replaced, in place,
//! by nodes selected from the inherited symbol's own — recursively expanded —
//! documentation:
//!
//! - the inherited symbol is the `cref`'s, resolved by documentation-comment
//!   ID across the whole compilation. Without a `cref`, Roslyn inherits from
//!   an implicit candidate (base member, implemented interface member, base
//!   constructor, base type), which is not modelled here: such an element
//!   declines;
//! - the nodes are those an XPath selects from the inherited entry: the
//!   `path` attribute's, or one built from the `<inheritdoc>`'s ancestry, so a
//!   top-level `<inheritdoc/>` takes every child but `<overloads>` and one
//!   inside `<summary>` takes the inherited summary's content ([`xpath`](super::xpath));
//! - a `cref` names a definition, whose type parameters stand for themselves,
//!   so the inherited entry's `<typeparamref>`s are kept as they are: Roslyn
//!   rewrites one only to a type argument with a documentation ID, and a type
//!   parameter's is an error ID;
//! - a symbol already being expanded on the current path is a cycle.
//!
//! So an entry with its own `<summary>` and a top-level `<inheritdoc/>` shows
//! *both* summaries, exactly as Roslyn's expansion holds them.
//!
//! Every way the expansion could fail to be exactly Roslyn's declines — the
//! entry keeps its `<inheritdoc>`, which renders as the honest marker — with a
//! typed [`Decline`]: an implicit candidate, a `cref` that does not name
//! exactly one importable symbol, an inherited entry that is missing or that
//! Roslyn would read differently, a path outside the modelled XPath, a cycle,
//! too many hops, or a result too large. One decline anywhere declines the
//! whole entry: a partly expanded entry would hold a marker where Roslyn shows
//! text, and the comparison against Roslyn is per entry.
//!
//! Where Roslyn itself expands to nothing — a cycle, an inherited symbol with
//! no documentation, an XPath it cannot evaluate or that selects nothing to
//! show — this module declines too rather than delete the element: an
//! `<inheritdoc>` whose inherited text is not shown is marked, never silently
//! dropped.

use std::sync::Arc;

use borzoi_assembly::doc_id::type_enc;
use borzoi_assembly::{Access, EntityKind, Member, Primitive, TypeRef};
use borzoi_sema::{AssemblyEnv, EntityHandle, IlTypeDefinition};

use super::key::{DocIdIndex, DocTarget, type_name};
use super::lookup::{DocLookup, DocSources, Located};
use super::tree::{DocElement, DocNode, MAX_DEPTH, Namespacing, name_is};
use super::xpath::{Unsupported, authored_path, default_path};

/// An entry, expanded or not, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    /// The entry to render: the expanded one, or the original when there was
    /// nothing to expand or the expansion declined.
    pub member: DocElement,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The entry has no `<inheritdoc>`.
    NoInheritdoc,
    /// Every `<inheritdoc>` was expanded, exactly as Roslyn expands it.
    Inherited,
    /// The entry is shown unexpanded.
    Declined(Decline),
}

/// Why an entry is shown unexpanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decline {
    /// An `<inheritdoc>` without a `cref`, which inherits from an implicit
    /// candidate the expansion does not model.
    ImplicitCandidate,
    /// Roslyn's own rule throws here — on a constructor of a type with no
    /// base type, whose base it dereferences unchecked, even beside a `cref`
    /// — and the exception abandons the whole expansion.
    RoslynThrows,
    /// A `cref` that does not name exactly one symbol Roslyn would bind.
    Cref(CrefMiss),
    /// The inherited symbol's documentation could not be read. Where Roslyn
    /// reads none, an undocumented override or interface implementation
    /// inherits from its implicit candidate, which is not modelled.
    InheritedDoc(Box<DocLookup>),
    /// The inherited symbol's entry is one Roslyn's documentation provider
    /// reads differently (nested, unlisted, namespaced file).
    ReadDifferently,
    /// A `path` outside the modelled XPath.
    Path(Unsupported),
    /// A namespaced element or attribute, which Roslyn matches by qualified
    /// name.
    Namespaced,
    /// A symbol reached again on its own expansion path (Roslyn leaves the
    /// element in place).
    Cycle,
    /// A chain longer than [`MAX_HOPS`], or more than [`MAX_RESOLUTIONS`]
    /// resolutions in all.
    TooManyHops,
    /// A result deeper than the renderer's bound, or larger than
    /// [`MAX_NODES`] or [`MAX_BYTES`].
    TooLarge,
    /// The path selects nothing to show from the inherited entry (no node,
    /// or only whitespace): Roslyn removes the element, leaving nothing in
    /// its place.
    NothingSelected,
}

/// Why a `cref` does not name one symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrefMiss {
    /// No symbol of the env has that documentation ID.
    NotFound,
    /// Only symbols Roslyn does not import (private, internal) have it.
    NotImported,
    /// Several importable symbols have it.
    Ambiguous,
    /// The symbol is projected in F#'s source view, whose IDs are not
    /// Roslyn's.
    NotIl,
    /// A type or member of that name was dropped while projecting, so the
    /// symbol found may not be the only one. For a type, any dropped type at
    /// all: an ID does not say which namespace and nesting split it names.
    DroppedNearby,
    /// The ID's path, or a signature type's, is not spelled in the ID
    /// alphabet, or the bound symbol's raw names do not round-trip through
    /// it: a compiler-generated member name (an F# witness `M$W`, which the
    /// projection may omit without a record), a delimiter of the ID syntax in
    /// a name (`Odd,ame`, a literal `Odd#ame`, which Roslyn's declaration-ID
    /// lookup reads otherwise), or a constructed explicit implementation's
    /// arguments (`` F#I{System#Int32}#M ``, the ID the compiler writes but
    /// one the lookup does not bind).
    UnspellableName,
    /// A type the member's signature spells has an ID more than one type
    /// carries: Roslyn's declaration-ID lookup resolves the spelling over the
    /// whole compilation, where it is ambiguous, whichever one the member's
    /// metadata names.
    AmbiguousSignatureType,
    /// The member's signature names a type no loaded assembly provides: to
    /// Roslyn it is an error type, which no documentation ID binds to.
    UnboundSignature,
    /// The ID may be a property's or event's accessor's, which Roslyn binds
    /// as a method of its own and the model has no target for.
    MaybeAccessor,
    /// Some referenced DLL could not be read at all, and may declare a symbol
    /// of the ID too.
    IncompleteAssemblySet,
    /// A symbol of the ID that Roslyn imports only through
    /// `InternalsVisibleTo` (not modelled) may be the one it binds.
    MaybeImportedByFriendship,
    /// A prefix of the ID is a namespace an assembly read in F#'s source view
    /// occupies: that view does not show every metadata type (a union's
    /// nested `Tags` class), so a symbol it hides may carry the ID too.
    FSharpNamespace,
    /// Some loaded type is a No-PIA local type (`[TypeIdentifier]`), which
    /// Roslyn's importer hides: its ID, or a type it encloses, names nothing
    /// to Roslyn while the index has it.
    NoPiaLocalType,
    /// Two loaded DLLs share a simple name: Roslyn's compilation keeps one of
    /// two DLLs of one identity, and which one is not modelled, so the symbol
    /// found may be in the one it drops.
    DuplicateAssemblyName,
}

/// A longer chain of inheritance (one `<inheritdoc>` inheriting another's)
/// than any real documentation takes.
pub const MAX_HOPS: usize = 32;

/// More `<inheritdoc>` resolutions, over a whole expansion, than any real
/// entry needs.
pub const MAX_RESOLUTIONS: usize = 256;

/// More nodes than any real expanded entry holds; a fan-out of `<inheritdoc>`
/// elements over a deep chain would otherwise grow exponentially.
pub const MAX_NODES: usize = 20_000;

/// More text than any real expanded entry holds, in bytes of text, element
/// and attribute names and attribute values: a text run is one node however
/// long, so [`MAX_NODES`] alone does not bound what an expansion produces.
pub const MAX_BYTES: usize = 1 << 20;

/// Expand the `<inheritdoc>` elements of `start`'s entry.
pub fn expand(
    sources: &mut DocSources,
    env: &Arc<AssemblyEnv>,
    start: DocTarget,
    entry: Located,
) -> Expansion {
    if !has_inheritdoc(&entry.member) {
        return Expansion {
            member: entry.member,
            outcome: Outcome::NoInheritdoc,
        };
    }
    let mut walk = Walk {
        sources,
        env,
        index: None,
        visited: Vec::new(),
        hops: 0,
        nodes: 0,
        bytes: 0,
    };
    let result = if entry.file.read_as_roslyn_reads(&entry.key) {
        walk.expand_entry(start, &entry.member)
    } else {
        Err(Decline::ReadDifferently)
    };
    match result {
        Ok(expanded) => Expansion {
            member: expanded.normalized(),
            outcome: Outcome::Inherited,
        },
        Err(why) => Expansion {
            member: entry.member,
            outcome: Outcome::Declined(why),
        },
    }
}

/// Whether the tree holds an element Roslyn expands.
fn has_inheritdoc(element: &DocElement) -> bool {
    element.children.iter().any(|c| match c {
        DocNode::Element(e) => is_inheritdoc(e) || has_inheritdoc(e),
        DocNode::Text(_) => false,
    })
}

/// Roslyn's test: an unnamespaced element named `inheritdoc` in any case —
/// C# compares documentation element names `OrdinalIgnoreCase`
/// (`DocumentationCommentXmlNames.ElementEquals`), so `<InheritDoc/>` is
/// expanded too. A namespaced *attribute* does not change the element's name:
/// `<inheritdoc xml:lang="en"/>` is expanded.
pub fn is_inheritdoc(element: &DocElement) -> bool {
    element.namespaced != Namespacing::Element && name_is(&element.name, "inheritdoc")
}

struct Walk<'a> {
    sources: &'a mut DocSources,
    env: &'a Arc<AssemblyEnv>,
    index: Option<Arc<DocIdIndex>>,
    /// The symbols being expanded on the current path, as Roslyn's
    /// `visitedSymbols`: each a definition, named by a `cref`.
    visited: Vec<DocTarget>,
    hops: usize,
    nodes: usize,
    bytes: usize,
}

impl Walk<'_> {
    /// `root` (an entry of `at`) with every `<inheritdoc>` rewritten.
    fn expand_entry(&mut self, at: DocTarget, root: &DocElement) -> Result<DocElement, Decline> {
        if root.any_namespaced() {
            return Err(Decline::Namespaced);
        }
        let mut ancestry = vec![root.name.clone()];
        let children = self.rewrite_children(at, root, &mut ancestry)?;
        Ok(DocElement {
            children,
            ..root.clone()
        })
    }

    fn rewrite_children(
        &mut self,
        at: DocTarget,
        parent: &DocElement,
        ancestry: &mut Vec<String>,
    ) -> Result<Vec<DocNode>, Decline> {
        let mut out = Vec::with_capacity(parent.children.len());
        for child in &parent.children {
            match child {
                DocNode::Text(_) => out.push(child.clone()),
                DocNode::Element(e) if is_inheritdoc(e) => {
                    out.extend(self.inheritdoc(at, e, ancestry)?);
                }
                DocNode::Element(e) => {
                    ancestry.push(e.name.clone());
                    let children = self.rewrite_children(at, e, ancestry);
                    ancestry.pop();
                    out.push(DocNode::Element(DocElement {
                        children: children?,
                        ..e.clone()
                    }));
                }
            }
        }
        Ok(out)
    }

    /// The nodes one `<inheritdoc>` element of `at`'s entry is replaced by.
    fn inheritdoc(
        &mut self,
        at: DocTarget,
        element: &DocElement,
        ancestry: &[String],
    ) -> Result<Vec<DocNode>, Decline> {
        // Roslyn computes the implicit candidate even beside a `cref`, and for
        // a constructor it dereferences the base type unchecked — so a
        // constructor of a type without one abandons the whole expansion.
        if let DocTarget::Member { parent, idx } = at
            && matches!(self.env.member_at(parent, idx), Member::Method(m) if m.is_constructor)
            && self.env.entity(parent).base_type.is_none()
        {
            return Err(Decline::RoslynThrows);
        }
        let target = match element.attribute("cref") {
            Some(cref) => self.cref(cref)?,
            None => return Err(Decline::ImplicitCandidate),
        };
        if self.visited.contains(&target) {
            return Err(Decline::Cycle);
        }
        self.hops += 1;
        if self.visited.len() >= MAX_HOPS || self.hops > MAX_RESOLUTIONS {
            return Err(Decline::TooManyHops);
        }
        self.visited.push(target);
        let selected = self.inherit(target, element, ancestry);
        self.visited.pop();
        let selected = selected?;
        let depth = selected
            .iter()
            .map(|n| match n {
                DocNode::Element(e) => 1 + e.depth(),
                DocNode::Text(_) => 0,
            })
            .max()
            .unwrap_or(0);
        // The nodes take the `<inheritdoc>`'s place, below its ancestry.
        if ancestry.len() + depth > MAX_DEPTH {
            return Err(Decline::TooLarge);
        }
        self.nodes += selected
            .iter()
            .map(|n| match n {
                DocNode::Element(e) => e.node_count(),
                DocNode::Text(_) => 1,
            })
            .sum::<usize>();
        self.bytes += selected.iter().map(byte_size).sum::<usize>();
        if self.nodes > MAX_NODES || self.bytes > MAX_BYTES {
            return Err(Decline::TooLarge);
        }
        Ok(selected)
    }

    /// The selection from `target`'s expanded entry.
    fn inherit(
        &mut self,
        target: DocTarget,
        element: &DocElement,
        ancestry: &[String],
    ) -> Result<Vec<DocNode>, Decline> {
        // Where Roslyn reads no documentation for the symbol, an undocumented
        // override or interface implementation inherits from its implicit
        // candidate, which is not modelled, and anything else inherits
        // nothing, which this module marks rather than deletes: either way,
        // a miss declines.
        let located = self
            .sources
            .locate(self.env, target)
            .map_err(|miss| Decline::InheritedDoc(Box::new(miss)))?;
        if !located.file.read_as_roslyn_reads(&located.key) {
            return Err(Decline::ReadDifferently);
        }
        let inherited = self.expand_entry(target, &located.member)?;
        let path = match element.attribute("path") {
            Some(path) if !path.is_empty() => authored_path(path),
            _ => {
                let ancestry: Vec<&str> = ancestry.iter().map(String::as_str).collect();
                default_path(&ancestry)
            }
        }
        .map_err(Decline::Path)?;
        let selected = path.select(&inherited).ok_or(Decline::Namespaced)?;
        let shows_something = selected.iter().any(|n| match n {
            DocNode::Element(_) => true,
            DocNode::Text(t) => !t.trim().is_empty(),
        });
        if !shows_something {
            return Err(Decline::NothingSelected);
        }
        Ok(selected)
    }

    /// The symbol a `cref` names, as Roslyn's
    /// `DocumentationCommentId.GetFirstSymbolForDeclarationId` binds it over
    /// the compilation: exactly one importable symbol with that ID.
    fn cref(&mut self, cref: &str) -> Result<DocTarget, Decline> {
        let env = self.env;
        if env.identities_incomplete() {
            return Err(Decline::Cref(CrefMiss::IncompleteAssemblySet));
        }
        if env.dropped_any_type() {
            return Err(Decline::Cref(CrefMiss::DroppedNearby));
        }
        if !env.assembly_names_unique() {
            return Err(Decline::Cref(CrefMiss::DuplicateAssemblyName));
        }
        if !spelled_in_the_id_alphabet(cref) {
            return Err(Decline::Cref(CrefMiss::UnspellableName));
        }
        let index = match &self.index {
            Some(index) => index.clone(),
            None => {
                let index = self.sources.indices.index(env);
                self.index = Some(index.clone());
                index
            }
        };
        if index.under_an_fsharp_prefix(cref) {
            return Err(Decline::Cref(CrefMiss::FSharpNamespace));
        }
        if index.has_no_pia_local_type() {
            return Err(Decline::Cref(CrefMiss::NoPiaLocalType));
        }
        if index.may_name_an_accessor(cref) {
            return Err(Decline::Cref(CrefMiss::MaybeAccessor));
        }
        let all = index.targets(cref);
        let imported: Vec<DocTarget> = all
            .iter()
            .copied()
            .filter(|t| roslyn_imports(env, *t))
            .collect();
        let target = match imported.as_slice() {
            [_] if imported.len() < all.len() => {
                return Err(Decline::Cref(CrefMiss::MaybeImportedByFriendship));
            }
            [only] => *only,
            [] if all.is_empty() => return Err(Decline::Cref(CrefMiss::NotFound)),
            [] => return Err(Decline::Cref(CrefMiss::NotImported)),
            _ => return Err(Decline::Cref(CrefMiss::Ambiguous)),
        };
        let owner = target.owner();
        let chain = env.enclosing_chain_from_root(owner).unwrap_or_default();
        if chain.is_empty() || !chain.iter().all(|&h| il_faithful(env, h)) {
            return Err(Decline::Cref(CrefMiss::NotIl));
        }
        let raw_member = match target {
            DocTarget::Member { parent, idx } => Some(match env.member_at(parent, idx) {
                Member::Method(m) => m.name.as_str(),
                Member::Property(p) => p.name.as_str(),
                Member::Event(e) => e.name.as_str(),
                Member::Field(f) => f.name.as_str(),
            }),
            DocTarget::Entity(_) => None,
        };
        if !raw_names_round_trip(env, &chain, raw_member) {
            return Err(Decline::Cref(CrefMiss::UnspellableName));
        }
        // Every type of the declaring type's ID must be read as IL: a type in
        // F#'s source view keeps metadata members (a module value's getter)
        // out of the index, so one of them may carry the ID too.
        if let DocTarget::Member { parent, .. } = target {
            let siblings = match type_name(env, parent) {
                Ok(name) => index.targets(&name.type_id()).to_vec(),
                Err(_) => return Err(Decline::Cref(CrefMiss::NotIl)),
            };
            if !siblings.iter().all(|s| {
                env.enclosing_chain_from_root(s.owner())
                    .is_some_and(|chain| chain.iter().all(|&h| il_faithful(env, h)))
            }) {
                return Err(Decline::Cref(CrefMiss::NotIl));
            }
        }
        if let DocTarget::Member { parent, idx } = target {
            let member = env.member_at(parent, idx);
            if matches!(member, Member::Method(m) if m.module_value.is_some()) {
                return Err(Decline::Cref(CrefMiss::NotIl));
            }
            // A member dropped from any type of the parent's ID — in this
            // assembly or another — may carry the ID too, under a name its
            // record does not spell (a dropped property's accessor), so any
            // drop there declines.
            let parents: Vec<DocTarget> = match type_name(env, parent) {
                Ok(name) => index.targets(&name.type_id()).to_vec(),
                Err(_) => return Err(Decline::Cref(CrefMiss::DroppedNearby)),
            };
            if parents
                .iter()
                .any(|p| !env.entity(p.owner()).skipped_members.is_empty())
            {
                return Err(Decline::Cref(CrefMiss::DroppedNearby));
            }
        }
        // Roslyn binds a member ID by matching its parameter types against
        // symbols; a parameter type from an assembly the compilation lacks is
        // an error type no ID matches, where the string comparison here would
        // still match.
        if let DocTarget::Member { parent, idx } = target {
            let types = signature_types(env.member_at(parent, idx));
            if !types.iter().all(|t| binds_throughout(env, parent, t)) {
                return Err(Decline::Cref(CrefMiss::UnboundSignature));
            }
            if !types
                .iter()
                .all(|t| spelled_uniquely(env, &index, parent, t))
            {
                return Err(Decline::Cref(CrefMiss::AmbiguousSignatureType));
            }
        }
        Ok(target)
    }
}

/// The bytes of text, element and attribute names, and attribute values in
/// `node`, which [`MAX_BYTES`] bounds.
fn byte_size(node: &DocNode) -> usize {
    match node {
        DocNode::Text(t) => t.len(),
        DocNode::Element(e) => {
            e.name.len()
                + e.attributes
                    .iter()
                    .map(|(k, v)| k.len() + v.len())
                    .sum::<usize>()
                + e.children.iter().map(byte_size).sum::<usize>()
        }
    }
}

/// Whether `cref` is an ID (`X:path`, then any parameters or a conversion's
/// `~`) whose path is spelled in the ID alphabet: its type segments of
/// [`is_type_id_char`]s, its member name (a member ID's last segment) of
/// [`is_member_id_char`]s.
fn spelled_in_the_id_alphabet(cref: &str) -> bool {
    let Some((kind, rest)) = cref.split_once(':') else {
        return false;
    };
    let path = rest.split(['(', '~']).next().unwrap_or(rest);
    let (types, member) = match kind {
        "T" => (path, None),
        "M" | "P" | "E" | "F" => match path.rsplit_once('.') {
            Some((types, member)) => (types, Some(member)),
            None => return false,
        },
        _ => return false,
    };
    types
        .split('.')
        .all(|t| !t.is_empty() && t.chars().all(is_type_id_char))
        && member.is_none_or(|m| !m.is_empty() && m.chars().all(is_member_id_char))
}

/// The characters a type's segment of an ID may hold: letters, digits, `_`,
/// an arity's `` ` ``, and the `<`, `>` and `$` of a compiler-named type (a
/// C# extension block's grouping type, `` <G>$F1C9… ``).
fn is_type_id_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '`' | '<' | '>' | '$')
}

/// The characters a member's name in an ID may hold: letters, digits, `_`, a
/// generic arity's `` ` ``, and the `#` an escaped `.` becomes (`#ctor`,
/// `N#I#M`).
fn is_member_id_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '`' | '#')
}

/// Whether a symbol's raw metadata names round-trip through its ID: no type
/// of `chain` (its enclosers and itself) is named with a character the ID
/// gives another meaning (`#`, `.`, `,`, `` ` ``), nor `member` with a
/// literal `#` or `` ` `` (an ID's `#` reads back as `.`, its `` ` `` as an
/// arity).
fn raw_names_round_trip(env: &AssemblyEnv, chain: &[EntityHandle], member: Option<&str>) -> bool {
    chain
        .iter()
        .all(|&h| !env.entity(h).name.contains(['#', '.', ',', '`']))
        && member.is_none_or(|m| !m.contains(['#', '`']))
}

/// Whether `handle` is read here as the IL a metadata consumer reads: an
/// assembly without an authoritative F# signature, and an IL type kind.
pub fn il_faithful(env: &AssemblyEnv, handle: EntityHandle) -> bool {
    !env.has_authoritative_fsharp_signature(handle)
        && matches!(
            env.entity(handle).kind,
            EntityKind::Class
                | EntityKind::Struct
                | EntityKind::Interface
                | EntityKind::Enum
                | EntityKind::Delegate
        )
}

/// The types a member's documentation ID spells: its parameter types, and a
/// conversion operator's return type.
fn signature_types(member: &Member) -> Vec<&TypeRef> {
    match member {
        Member::Method(m) => {
            let mut types: Vec<&TypeRef> = m.signature.parameters.iter().map(|p| &p.ty).collect();
            if m.name.starts_with("op_") && m.name.ends_with("plicit") {
                types.push(&m.signature.return_type);
            }
            types
        }
        Member::Property(p) => p.parameters.iter().map(|ip| &ip.ty.ty).collect(),
        Member::Field(_) | Member::Event(_) => Vec::new(),
    }
}

/// Whether every named type in `ty` (written in `from`'s metadata) binds, and
/// every primitive ([`AssemblyEnv::primitive_binds`]).
fn binds_throughout(env: &AssemblyEnv, from: EntityHandle, ty: &TypeRef) -> bool {
    match ty {
        TypeRef::Named { type_args, .. } => {
            matches!(
                env.il_type_definition(from, ty),
                IlTypeDefinition::Resolved(_)
            ) && type_args.iter().all(|a| binds_throughout(env, from, &a.ty))
        }
        TypeRef::Array { element, .. } => binds_throughout(env, from, &element.ty),
        TypeRef::Ptr(Some(inner)) | TypeRef::ByRef { inner, .. } => {
            binds_throughout(env, from, inner)
        }
        TypeRef::Primitive(p) => env.primitive_binds(*p),
        TypeRef::Ptr(None) => env.primitive_binds(Primitive::Void),
        TypeRef::Var { .. } => true,
    }
}

/// Whether every named type and primitive in `ty` (written in `from`'s
/// metadata, and binding throughout: [`binds_throughout`]) has an ID that no
/// other type of the env carries, so that the spelling in a documentation ID
/// names it alone.
fn spelled_uniquely(
    env: &AssemblyEnv,
    index: &DocIdIndex,
    from: EntityHandle,
    ty: &TypeRef,
) -> bool {
    let sole = |id: String| index.targets(&id).len() == 1;
    match ty {
        TypeRef::Named { type_args, .. } => {
            let IlTypeDefinition::Resolved(def) = env.il_type_definition(from, ty) else {
                return false;
            };
            type_name(env, def).is_ok_and(|name| {
                let id = name.type_id();
                name.full()
                    .split('.')
                    .all(|t| t.chars().all(is_type_id_char))
                    && !index.under_an_fsharp_prefix(&id)
                    && sole(id)
            }) && type_args
                .iter()
                .all(|a| spelled_uniquely(env, index, from, &a.ty))
        }
        TypeRef::Array { element, .. } => spelled_uniquely(env, index, from, &element.ty),
        TypeRef::Ptr(Some(inner)) | TypeRef::ByRef { inner, .. } => {
            spelled_uniquely(env, index, from, inner)
        }
        TypeRef::Primitive(_) => sole(format!("T:{}", type_enc(ty))),
        TypeRef::Ptr(None) => sole("T:System.Void".to_string()),
        TypeRef::Var { .. } => true,
    }
}

/// Whether Roslyn's PE importer (`MetadataImportOptions.Public`, as for any
/// referenced assembly) imports `target`: every type; a field unless private
/// or internal; a method if virtual, a static explicit interface
/// implementation, or else unless private or internal; a property or event
/// if some accessor is imported.
pub fn roslyn_imports(env: &AssemblyEnv, target: DocTarget) -> bool {
    let DocTarget::Member { parent, idx } = target else {
        return true;
    };
    let visible = |access: Access| !matches!(access, Access::Private | Access::Internal);
    match env.member_at(parent, idx) {
        // An enum's backing field is no member of the enum to Roslyn.
        Member::Field(f) if f.name == "value__" && env.entity(parent).kind == EntityKind::Enum => {
            false
        }
        Member::Field(f) => visible(f.access),
        // `ShouldImportMethod` skips a vtable gap whatever its access.
        Member::Method(m) if m.name.starts_with("_VtblGap") => false,
        Member::Method(m) => {
            m.is_virtual
                || visible(m.access)
                || (m.is_static && (!m.implements.is_empty() || !m.unclassified_impls.is_empty()))
        }
        Member::Property(p) => {
            visible(p.access)
                || p.accessor_slots.iter().any(|s| s.is_virtual)
                || (p.is_static && (!p.implements.is_empty() || !p.unclassified_impls.is_empty()))
        }
        Member::Event(e) => {
            visible(e.access)
                || e.accessor_slots.iter().any(|s| s.is_virtual)
                || (e.is_static && (!e.implements.is_empty() || !e.unclassified_impls.is_empty()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::spelled_in_the_id_alphabet;

    #[test]
    fn an_id_is_spelled_in_the_id_alphabet_or_declines() {
        for (cref, expected) in [
            ("T:F.C", true),
            ("T:F.G`1", true),
            ("M:F.C.M(System.Int32)", true),
            ("M:F.C.M``1(``0)", true),
            ("M:F.C.#ctor", true),
            ("M:F.A.F#I#M", true),
            (
                "M:F.A.M(System.Collections.Generic.List{System.Int32})",
                true,
            ),
            ("M:F.C.op_Implicit(F.C)~System.Int32", true),
            ("M:F.C.Ünïcödé", true),
            ("M:N.E.<G>$F1C97.M(System.Int32)", true),
            ("T:N.E.<G>$F1C97", true),
            ("M:F.C.M$W", false),
            ("P:F.C.P$X", false),
            ("M:F.U.Odd,ame", false),
            ("M:F.A.F#I{System#Int32}#M(System.Int32)", false),
            ("M:F.C.<M>b__0", false),
            ("M:F.C.M,N", false),
            ("!:F.C", false),
            ("漢", false),
            ("", false),
        ] {
            assert_eq!(spelled_in_the_id_alphabet(cref), expected, "{cref}");
        }
    }
}
