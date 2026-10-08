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
//! - the inherited symbol is the `cref`'s, when there is one, resolved by
//!   documentation-comment ID across the whole compilation; otherwise
//!   [`candidate`]'s (base member, implemented interface member, base type);
//! - the nodes are those an XPath selects from the inherited entry: the
//!   `path` attribute's, or one built from the `<inheritdoc>`'s ancestry, so a
//!   top-level `<inheritdoc/>` takes every child but `<overloads>` and one
//!   inside `<summary>` takes the inherited summary's content ([`xpath`](super::xpath));
//! - in the inherited entry, a `<typeparamref>` naming a type parameter of the
//!   inherited symbol's containing types becomes `<see cref="…"/>` of the type
//!   argument it is reached with, when that argument has a documentation ID
//!   (`class D : B<int>` inherits "a `T:System.Int32`", not "a `T`");
//! - a symbol already being expanded on the current path is a cycle.
//!
//! So an entry with its own `<summary>` and a top-level `<inheritdoc/>` shows
//! *both* summaries, exactly as Roslyn's expansion holds them.
//!
//! Every way the expansion could fail to be exactly Roslyn's declines — the
//! entry keeps its `<inheritdoc>`, which renders as the honest marker — with a
//! typed [`Decline`]: no candidate or an undecidable one, a `cref` that does not
//! name exactly one importable symbol, an inherited entry that is missing or
//! that Roslyn would read differently, a path outside the modelled XPath, a
//! type argument whose documentation ID cannot be settled, a cycle, too many
//! hops, or a result too large. One decline anywhere declines the whole entry:
//! a partly expanded entry would hold a marker where Roslyn shows text, and
//! the comparison against Roslyn is per entry.
//!
//! Where Roslyn itself expands to nothing — no candidate, a cycle, an
//! inherited symbol with no documentation, an XPath it cannot evaluate — this
//! module declines too rather than delete the element: an `<inheritdoc>` whose
//! inherited text is not shown is marked, never silently dropped.

use std::sync::Arc;

use borzoi_assembly::doc_id::type_enc;
use borzoi_assembly::{Access, Member, Primitive, TypeRef};
use borzoi_sema::{AssemblyEnv, EntityHandle, IlTypeDefinition};

use super::candidate::{
    Candidate, Instantiation, NoCandidate, Reached, Undecidable, candidate, il_faithful,
    inherits_automatically, type_parameter_names,
};
use super::key::{DocIdIndex, DocTarget, type_name};
use super::lookup::{DocLookup, DocSources, Located, xml_path_for};
use super::tree::{DocElement, DocNode, MAX_DEPTH, name_is};
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
    /// A bare `<inheritdoc/>` whose symbol Roslyn gives no candidate.
    NoCandidate(NoCandidate),
    /// A bare `<inheritdoc/>` whose candidate the env cannot settle.
    Undecidable(Undecidable),
    /// A `cref` that does not name exactly one symbol Roslyn would bind.
    Cref(CrefMiss),
    /// The inherited symbol's documentation could not be read.
    InheritedDoc(Box<DocLookup>),
    /// The inherited symbol's entry is one Roslyn's documentation provider
    /// reads differently (nested, unlisted, namespaced file).
    ReadDifferently,
    /// A `path` outside the modelled XPath.
    Path(Unsupported),
    /// A namespaced element or attribute, which Roslyn matches by qualified
    /// name.
    Namespaced,
    /// A `<typeparamref>` whose replacement cannot be settled.
    TypeParamRef(TypeArgMiss),
    /// A symbol reached again on its own expansion path (Roslyn leaves the
    /// element in place).
    Cycle,
    /// A chain longer than [`MAX_HOPS`], or more than [`MAX_RESOLUTIONS`]
    /// resolutions in all.
    TooManyHops,
    /// A result deeper than the renderer's bound, or larger than
    /// [`MAX_NODES`].
    TooLarge,
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
    /// symbol found may not be the only one.
    DroppedNearby,
    /// The member's signature names a type no loaded assembly provides: to
    /// Roslyn it is an error type, which no documentation ID binds to.
    UnboundSignature,
}

/// Why a type argument's documentation ID cannot be settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeArgMiss {
    /// `System.Object`, which may be `dynamic` (whose ID is none).
    MaybeDynamic,
    /// The argument's type does not bind.
    Unbound,
    /// A type constructed from its own type parameters somewhere other than
    /// where the rendering of that case is modelled.
    SelfConstructed,
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
    };
    let result = if entry.file.read_as_roslyn_reads(&entry.key) {
        walk.expand_entry(&Reached::definition(env, start), &entry.member)
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
/// expanded too.
pub fn is_inheritdoc(element: &DocElement) -> bool {
    !element.namespaced && name_is(&element.name, "inheritdoc")
}

/// What identifies one symbol on a path, as Roslyn's `visitedSymbols` set
/// does: the definition and the type arguments it is reached with.
type VisitKey = (DocTarget, EntityHandle, Vec<String>);

fn visit_key(at: &Reached) -> VisitKey {
    (
        at.target,
        at.inst.context,
        at.inst.args.iter().map(type_enc).collect(),
    )
}

struct Walk<'a> {
    sources: &'a mut DocSources,
    env: &'a Arc<AssemblyEnv>,
    index: Option<Arc<DocIdIndex>>,
    visited: Vec<VisitKey>,
    hops: usize,
    nodes: usize,
}

impl Walk<'_> {
    /// `root` (an entry of `at`) with every `<inheritdoc>` rewritten.
    fn expand_entry(&mut self, at: &Reached, root: &DocElement) -> Result<DocElement, Decline> {
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
        at: &Reached,
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
        at: &Reached,
        element: &DocElement,
        ancestry: &[String],
    ) -> Result<Vec<DocNode>, Decline> {
        // Roslyn computes the candidate even beside a `cref`, and for a
        // constructor it dereferences the base type unchecked — so a
        // constructor of a type without one abandons the whole expansion.
        if let DocTarget::Member { parent, idx } = at.target
            && matches!(self.env.member_at(parent, idx), Member::Method(m) if m.is_constructor)
            && self.env.entity(parent).base_type.is_none()
        {
            return Err(Decline::Undecidable(Undecidable::RoslynThrows));
        }
        let target = match element.attribute("cref") {
            Some(cref) => self.cref(cref)?,
            None => match candidate(self.env, at) {
                Candidate::Found(target) => target,
                Candidate::None(why) => return Err(Decline::NoCandidate(why)),
                Candidate::Undecidable(why) => return Err(Decline::Undecidable(why)),
            },
        };
        let key = visit_key(&target);
        if self.visited.contains(&key) {
            return Err(Decline::Cycle);
        }
        self.hops += 1;
        if self.visited.len() >= MAX_HOPS || self.hops > MAX_RESOLUTIONS {
            return Err(Decline::TooManyHops);
        }
        self.visited.push(key);
        let selected = self.inherit(&target, element, ancestry);
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
        if self.nodes > MAX_NODES {
            return Err(Decline::TooLarge);
        }
        Ok(selected)
    }

    /// The selection from `target`'s expanded entry.
    fn inherit(
        &mut self,
        target: &Reached,
        element: &DocElement,
        ancestry: &[String],
    ) -> Result<Vec<DocNode>, Decline> {
        let mut inherited = match self.sources.locate(self.env, target.target) {
            Ok(located) => {
                if !located.file.read_as_roslyn_reads(&located.key) {
                    return Err(Decline::ReadDifferently);
                }
                self.expand_entry(target, &located.member)?
            }
            // Roslyn reads no documentation for the symbol — its file has no
            // entry, or there is no file — and an undocumented override or
            // interface implementation inherits as if its entry were a bare
            // `<inheritdoc/>`. Anything else inherits nothing, which this
            // module marks rather than deletes.
            Err(miss @ (DocLookup::NoEntry { .. } | DocLookup::NoXmlFile(_))) => {
                // "No entry" here must be "no entry" to Roslyn too.
                if let DocLookup::NoEntry { key } = &miss
                    && let Some(dll) = self.env.assembly_path(target.target.owner())
                    && !self
                        .sources
                        .files
                        .load(&xml_path_for(dll))
                        .is_ok_and(|file| file.read_as_roslyn_reads(key))
                {
                    return Err(Decline::ReadDifferently);
                }
                match inherits_automatically(self.env, target) {
                    Ok(true) => {
                        let automatic = DocElement::new(
                            "doc",
                            Vec::new(),
                            vec![DocNode::Element(DocElement::new(
                                "inheritdoc",
                                Vec::new(),
                                Vec::new(),
                            ))],
                        );
                        self.expand_entry(target, &automatic)?
                    }
                    Ok(false) => return Err(Decline::InheritedDoc(Box::new(miss))),
                    Err(why) => return Err(Decline::Undecidable(why)),
                }
            }
            Err(miss) => return Err(Decline::InheritedDoc(Box::new(miss))),
        };
        let names = all_type_parameter_names(self.env, target.target);
        rewrite_type_param_refs(self.env, &target.inst, &names, &mut inherited)?;
        let path = match element.attribute("path") {
            Some(path) if !path.is_empty() => authored_path(path),
            _ => {
                let ancestry: Vec<&str> = ancestry.iter().map(String::as_str).collect();
                default_path(&ancestry)
            }
        }
        .map_err(Decline::Path)?;
        Ok(path.select(&inherited))
    }

    /// The symbol a `cref` names, as Roslyn's
    /// `DocumentationCommentId.GetFirstSymbolForDeclarationId` binds it over
    /// the compilation: exactly one importable symbol with that ID.
    fn cref(&mut self, cref: &str) -> Result<Reached, Decline> {
        let env = self.env;
        let index = match &self.index {
            Some(index) => index.clone(),
            None => {
                let index = self.sources.indices.index(env);
                self.index = Some(index.clone());
                index
            }
        };
        let all = index.targets(cref);
        let imported: Vec<DocTarget> = all
            .iter()
            .copied()
            .filter(|t| roslyn_imports(env, *t))
            .collect();
        let target = match imported.as_slice() {
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
        if let DocTarget::Member { parent, idx } = target {
            let member = env.member_at(parent, idx);
            if matches!(member, Member::Method(m) if m.module_value.is_some()) {
                return Err(Decline::Cref(CrefMiss::NotIl));
            }
            let name = super::candidate::member_name(member);
            if env
                .entity(parent)
                .skipped_members
                .iter()
                .any(|s| s.name == name)
            {
                return Err(Decline::Cref(CrefMiss::DroppedNearby));
            }
        }
        if env.dropped_a_type_beside(owner) {
            return Err(Decline::Cref(CrefMiss::DroppedNearby));
        }
        // Roslyn binds a member ID by matching its parameter types against
        // symbols; a parameter type from an assembly the compilation lacks is
        // an error type no ID matches, where the string comparison here would
        // still match.
        if let DocTarget::Member { parent, idx } = target
            && !signature_types(env.member_at(parent, idx))
                .iter()
                .all(|t| binds_throughout(env, parent, t))
        {
            return Err(Decline::Cref(CrefMiss::UnboundSignature));
        }
        Ok(Reached::definition(env, target))
    }
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

/// Whether every named type in `ty` (written in `from`'s metadata) binds.
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
        TypeRef::Ptr(None) | TypeRef::Primitive(_) | TypeRef::Var { .. } => true,
    }
}

/// Whether Roslyn's PE importer (`MetadataImportOptions.Public`, as for any
/// referenced assembly) imports `target`: every type; a field unless private
/// or internal; a method if virtual, or else unless private or internal; a
/// property or event if some accessor is imported.
pub fn roslyn_imports(env: &AssemblyEnv, target: DocTarget) -> bool {
    let DocTarget::Member { parent, idx } = target else {
        return true;
    };
    let visible = |access: Access| !matches!(access, Access::Private | Access::Internal);
    match env.member_at(parent, idx) {
        Member::Field(f) => visible(f.access),
        Member::Method(m) => {
            m.is_virtual
                || visible(m.access)
                || (m.is_static && (!m.implements.is_empty() || !m.unclassified_impls.is_empty()))
        }
        Member::Property(p) => visible(p.access) || p.accessor_slots.iter().any(|s| s.is_virtual),
        Member::Event(e) => visible(e.access) || e.accessor_slots.iter().any(|s| s.is_virtual),
    }
}

/// Roslyn's `GetAllTypeParameters` of a target's original definition: its
/// containing types' parameters, outermost first, then a method's own.
fn all_type_parameter_names(env: &AssemblyEnv, target: DocTarget) -> Vec<String> {
    let mut names = type_parameter_names(env, target.owner()).unwrap_or_default();
    if let DocTarget::Member { parent, idx } = target
        && let Member::Method(m) = env.member_at(parent, idx)
    {
        names.extend(m.generic_parameters.iter().map(|p| p.name.clone()));
    }
    names
}

/// Rewrite, in `root`, each `<typeparamref name="T"/>` naming a containing
/// type's parameter of the inherited symbol to `<see cref="…"/>` of the type
/// argument `inst` gives it, when that argument has a documentation ID that
/// is not an error ID (`!:`) — Roslyn's `RewriteInheritdocElement`.
fn rewrite_type_param_refs(
    env: &AssemblyEnv,
    inst: &Instantiation,
    names: &[String],
    root: &mut DocElement,
) -> Result<(), Decline> {
    for child in &mut root.children {
        let DocNode::Element(e) = child else {
            continue;
        };
        if e.name == "typeparamref"
            && let Some(name) = e.attribute("name")
        {
            let replacement = match names.iter().position(|n| n == name) {
                Some(index) if index < inst.args.len() => {
                    type_argument_id(env, inst, &inst.args[index]).map_err(Decline::TypeParamRef)?
                }
                _ => None,
            };
            if let Some(id) = replacement {
                *child = DocNode::Element(DocElement::new(
                    "see",
                    vec![("cref".to_string(), id)],
                    Vec::new(),
                ));
                continue;
            }
        }
        rewrite_type_param_refs(env, inst, names, e)?;
    }
    Ok(())
}

/// The documentation ID Roslyn's `GetDocumentationCommentId` gives a type
/// argument, or `None` where it gives none or an error ID (a type parameter,
/// an array, a pointer).
fn type_argument_id(
    env: &AssemblyEnv,
    inst: &Instantiation,
    arg: &TypeRef,
) -> Result<Option<String>, TypeArgMiss> {
    match arg {
        TypeRef::Var { .. } | TypeRef::Array { .. } | TypeRef::Ptr(_) | TypeRef::ByRef { .. } => {
            Ok(None)
        }
        // `dynamic` is `object` plus an attribute the model does not keep, and
        // has no ID of its own.
        TypeRef::Primitive(Primitive::Object) => Err(TypeArgMiss::MaybeDynamic),
        TypeRef::Primitive(_) => Ok(Some(format!("T:{}", type_enc(arg)))),
        TypeRef::Named { type_args, .. } => {
            let def = match env.il_type_definition(inst.context, arg) {
                IlTypeDefinition::Resolved(def) => def,
                _ => return Err(TypeArgMiss::Unbound),
            };
            // Roslyn renders a type constructed from its own type parameters
            // as its definition (`` T:P.E`1 ``), and the model can tell only
            // at the top level, for the context type or an encloser of it.
            let own = |args: &[borzoi_assembly::NullableType]| {
                !args.is_empty()
                    && args.iter().enumerate().all(|(i, a)| {
                        matches!(a.ty, TypeRef::Var { index, is_method: false } if usize::from(index) == i)
                    })
            };
            let enclosing = env
                .enclosing_chain_from_root(inst.context)
                .unwrap_or_default();
            if own(type_args) && enclosing.contains(&def) {
                if env.entity(def).generic_parameters.len() == type_args.len()
                    && let Ok(name) = type_name(env, def)
                {
                    return Ok(Some(name.type_id()));
                }
                return Err(TypeArgMiss::SelfConstructed);
            }
            // The same rendering applies inside the braces, where it is not
            // modelled: decline if any nested argument may take it.
            if self_constructed_inside(env, inst.context, &enclosing, type_args, &own)? {
                return Err(TypeArgMiss::SelfConstructed);
            }
            Ok(Some(format!("T:{}", type_enc(arg))))
        }
    }
}

/// Whether some type nested in `args` is constructed from the context's
/// type parameters `0..n`, in order, *and* is the context or one of its
/// enclosers — a type Roslyn renders as its own definition.
fn self_constructed_inside(
    env: &AssemblyEnv,
    context: EntityHandle,
    enclosing: &[EntityHandle],
    args: &[borzoi_assembly::NullableType],
    own: &impl Fn(&[borzoi_assembly::NullableType]) -> bool,
) -> Result<bool, TypeArgMiss> {
    for a in args {
        match &a.ty {
            TypeRef::Named { type_args, .. } => {
                if own(type_args) {
                    match env.il_type_definition(context, &a.ty) {
                        IlTypeDefinition::Resolved(def) if !enclosing.contains(&def) => {}
                        IlTypeDefinition::Resolved(_) => return Ok(true),
                        _ => return Err(TypeArgMiss::Unbound),
                    }
                }
                if self_constructed_inside(env, context, enclosing, type_args, own)? {
                    return Ok(true);
                }
            }
            TypeRef::Array { element, .. }
                if self_constructed_inside(
                    env,
                    context,
                    enclosing,
                    std::slice::from_ref(element),
                    own,
                )? =>
            {
                return Ok(true);
            }
            _ => {}
        }
    }
    Ok(false)
}
