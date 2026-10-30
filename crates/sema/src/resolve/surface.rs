//! What a declaration binds **bare** in the value and pattern namespaces —
//! one closed walk over the module-declaration grammar.
//!
//! Most declarations bind their bare names by pushing a scope entry at their
//! own position, so source position orders them against everything else for
//! free. Four routes do not, because sema models what they bind as something
//! other than a value or a case:
//!
//! - a **constructible type** takes FCS's unqualified value slot for its name
//!   (`AddTyconRefsToNameEnv`'s constructor group), and sema models no project
//!   type constructor (task #45);
//! - an **`[<AutoOpen>]` type** (or exception) folds its statics into the rest
//!   of its container, and sema enumerates none of them (task #48);
//! - a **bar-less single-case union** `type T = Nn` is spelled exactly like an
//!   abbreviation, and FCS reads it as a union whose case is `Nn` whenever no
//!   type `Nn` is in scope (`TyconCoreAbbrevThatIsReallyAUnion`);
//! - a **`rec` block** makes every declaration visible to every other, so a use
//!   written *before* a declaration binds it, which no position order expresses.
//!
//! Each of them declines from its own position instead, so a use after it can
//! no longer bind whatever was in scope before. A type already has a positional
//! account of the slot it takes — [`Resolver::type_holds_the_slot_over_the_entry`],
//! which a bare name and a dotted head share, and which a `rec` block's types
//! join at the block's start — so it pushes nothing. An `[<AutoOpen>]` type's
//! surface is open-ended (static members, an enum's fields, an exception's
//! augmentation, an abbreviation's target), so it raises the generation
//! barrier, as the fold-back already does for one inside a fragment. The other
//! two push a `Deferred` entry for every name this module says they bind.
//! The match below is exhaustive over [`ModuleDecl`] and [`TypeDefnRepr`] on
//! purpose: a declaration form added to the grammar is a compile error here,
//! rather than a route someone has to think of.

use borzoi_cst::syntax::{
    AstNode, AttributeList, ModuleDecl, SyntaxKind, SyntaxToken, Type, TypeDefn, TypeDefnRepr,
};

use super::decls::type_slot_class;
use super::model::{DeferredReason, ExportDeclKind, Resolution, SlotClass};
use super::state::{Resolver, ScopeEntry};
use super::{
    abbrev_target_single_ident, active_pat_name_of, attrs_auto_open,
    attrs_require_qualified_access, is_type_augmentation, single_ident,
};
use crate::binders::{BinderRole, PatternName, pattern_names};
use crate::def::{DefKind, id_text};

/// Which of the two bare namespaces a name occupies.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum BareNamespace {
    /// Expression position only: a value, an `extern`, an `[<AutoOpen>]`
    /// type's static.
    Value,
    /// Pattern position only: an active-pattern case.
    Pattern,
    /// Both: a union case, an exception constructor, or a value that may be a
    /// `[<Literal>]` (a constant pattern).
    Both,
}

/// The bare names a run of declarations may bind, over-approximated: a name
/// listed here that the declarations turn out not to bind costs one decline,
/// and a name they bind that is missing is a wrong answer.
#[derive(Debug, Default)]
pub(super) struct BareSurface {
    pub(super) names: Vec<(String, BareNamespace)>,
    /// The constructible types, kept apart: a type takes its name's slot
    /// without an entry, and a `Deferred` entry under it would blind a dotted
    /// head (`T.Member`) that reads the type
    /// ([`Resolver::decline_rec_block_surface`]).
    pub(super) type_names: Vec<(String, SlotClass)>,
    /// The bar-less cases' candidate names ([`barless_case_candidate`]), kept
    /// apart: each is a case only if no type of its name is in scope, which
    /// only the resolver can ask ([`Resolver::type_name_certainly_in_scope`]).
    pub(super) barless_candidates: Vec<String>,
    /// Every type these declarations define, of any kind — each one is in
    /// scope throughout a `rec` block, so a bar-less candidate naming one is
    /// an abbreviation.
    pub(super) defined_types: Vec<String>,
    /// Some declaration binds names this walk cannot list — an `[<AutoOpen>]`
    /// type or exception ([`folds_statics`]).
    pub(super) unknowable: bool,
}

impl BareSurface {
    fn add(&mut self, tok: &SyntaxToken, ns: BareNamespace) {
        self.names.push((id_text(tok).to_string(), ns));
    }
}

/// Whether a declaration's attribute lists may mark it `[<AutoOpen>]`
/// ([`Resolver::attrs_may_auto_open`]) — passed in, because only the resolver
/// knows which abbreviations are in reach.
pub(super) type MayAutoOpen<'a> = &'a dyn Fn(Vec<AttributeList>) -> bool;

/// Every bare name `decls` may bind in the scope that holds them, including
/// what an `[<AutoOpen>]` module among them folds in. The `rec`-block decline
/// reads this.
pub(super) fn decls_bare_surface(
    decls: impl Iterator<Item = ModuleDecl>,
    may_auto_open: MayAutoOpen<'_>,
) -> BareSurface {
    let mut out = BareSurface::default();
    for decl in decls {
        decl_bare_surface(&decl, &mut out, may_auto_open);
    }
    out
}

fn decl_bare_surface(decl: &ModuleDecl, out: &mut BareSurface, may_auto_open: MayAutoOpen<'_>) {
    match decl {
        ModuleDecl::Let(l) => {
            // A `[<Literal>]` value is a constant pattern too: FCS's `ePatItems`
            // holds the literals alongside the cases. Attribute *presence* is
            // the test, as for [`ScopeEntry::maybe_constant_pattern`] — a
            // `LiteralAttribute` alias is undetectable.
            let decl_attributed = l
                .syntax()
                .children()
                .any(|c| c.kind() == SyntaxKind::ATTRIBUTE_LIST);
            for binding in l.bindings() {
                let Some(pat) = binding.pat() else { continue };
                let ns = if decl_attributed || binding.attributes().next().is_some() {
                    BareNamespace::Both
                } else {
                    BareNamespace::Value
                };
                if let Some(ap) = active_pat_name_of(&pat) {
                    for case in ap.case_tokens() {
                        if case.kind() == SyntaxKind::IDENT_TOK {
                            out.add(&case, BareNamespace::Pattern);
                        }
                    }
                    continue;
                }
                // The module-level binders only: a function's parameters are
                // not bare names of the module.
                for name in pattern_names(&pat, BinderRole::Let, &Default::default()) {
                    if let PatternName::Binder(def) = name
                        && matches!(def.kind, DefKind::Value { .. })
                    {
                        out.names.push((def.key().to_string(), ns));
                    }
                }
            }
        }
        ModuleDecl::Extern(e) => {
            if let Some(name) = e.name().and_then(|n| n.idents().last()) {
                out.add(&name, BareNamespace::Value);
            }
        }
        ModuleDecl::Exception(e) => {
            if let Some(name) = e.union_case().and_then(|c| c.ident()) {
                out.add(&name, BareNamespace::Both);
            }
            if may_auto_open(e.attributes().collect()) {
                out.unknowable = true;
            }
        }
        ModuleDecl::Types(types) => {
            for defn in types.defns() {
                type_defn_bare_surface(&defn, out, may_auto_open);
            }
        }
        // An `[<AutoOpen>]` module's surface folds into this scope; a plain
        // module's members are reached qualified.
        ModuleDecl::NestedModule(nm) => {
            if may_auto_open(nm.attributes().collect()) {
                for inner in nm.decls() {
                    decl_bare_surface(&inner, out, may_auto_open);
                }
            }
        }
        // An `open` pushes its own entries at its own position, and the rest
        // bind nothing bare.
        ModuleDecl::Open(_)
        | ModuleDecl::Expr(_)
        | ModuleDecl::ModuleAbbrev(_)
        | ModuleDecl::Attributes(_)
        | ModuleDecl::HashDirective(_) => {}
    }
}

/// Every bare name one type definition may bind.
fn type_defn_bare_surface(defn: &TypeDefn, out: &mut BareSurface, may_auto_open: MayAutoOpen<'_>) {
    if is_type_augmentation(defn) {
        return;
    }
    let Some(name) = defn.long_id().and_then(single_ident) else {
        return;
    };
    out.defined_types.push(id_text(&name).to_string());
    let slot = type_slot_class(defn);
    if slot != SlotClass::Keeps {
        out.type_names.push((id_text(&name).to_string(), slot));
    }
    if let Some(case) =
        barless_case_candidate(defn).filter(|_| !attrs_require_qualified_access(defn.attributes()))
    {
        // `type Nn = Nn` is always a union.
        if id_text(&case) == id_text(&name) {
            out.add(&case, BareNamespace::Both);
        } else {
            out.barless_candidates.push(id_text(&case).to_string());
        }
    }
    match defn.repr() {
        Some(TypeDefnRepr::Union(u)) if !attrs_require_qualified_access(defn.attributes()) => {
            for case in u.cases() {
                if let Some(ident) = case.ident() {
                    out.add(&ident, BareNamespace::Both);
                }
            }
        }
        // An enum's cases are reached qualified, and the rest own no case.
        Some(
            TypeDefnRepr::Union(_)
            | TypeDefnRepr::Abbrev(_)
            | TypeDefnRepr::Record(_)
            | TypeDefnRepr::Enum(_)
            | TypeDefnRepr::ObjectModel(_)
            | TypeDefnRepr::Delegate(_)
            | TypeDefnRepr::InlineIl(_),
        )
        | None => {}
    }
    if folds_statics(defn, may_auto_open) {
        out.unknowable = true;
    }
}

/// The ident a **bar-less** type definition `type T = Nn` may declare as a
/// union case, before anyone asks whether a type `Nn` is in scope: a
/// single-segment, unapplied right-hand side (parenthesised or not), exactly
/// `TyconCoreAbbrevThatIsReallyAUnion`'s syntactic half.
pub(super) fn barless_case_candidate(defn: &TypeDefn) -> Option<SyntaxToken> {
    let Some(TypeDefnRepr::Abbrev(a)) = defn.repr() else {
        return None;
    };
    a.ty().as_ref().and_then(abbrev_target_single_ident)
}

/// Whether `defn` may be an `[<AutoOpen>]` type that folds its statics into
/// its container. A generic one folds nothing: `CanAutoOpenTyconRef` ends
/// `tcref.Typars(m) |> List.isEmpty`.
pub(super) fn folds_statics(defn: &TypeDefn, may_auto_open: MayAutoOpen<'_>) -> bool {
    defn.typar_decls().is_none() && may_auto_open(defn.attributes().collect())
}

impl Resolver<'_> {
    /// Push a decline for each of `names` into the current frame, from here on.
    ///
    /// A pattern-namespace or two-namespace name is marked
    /// [`ScopeEntry::opened_case`], so a pattern reference stops at it with no
    /// target rather than reading the `Deferred` as a non-case and scanning past
    /// it to whatever case was in scope before.
    pub(super) fn push_bare_declines(&mut self, names: Vec<(String, BareNamespace)>) {
        let generation = self.open_generation;
        let declined = || Resolution::Deferred(DeferredReason::UnboundName);
        let entries: Vec<ScopeEntry> = names
            .into_iter()
            .map(|(name, ns)| match ns {
                BareNamespace::Value => ScopeEntry::binding(name, declined(), generation),
                BareNamespace::Pattern => {
                    let mut e = ScopeEntry::opened_pattern_only(name, declined(), generation, 0);
                    e.opened_case = true;
                    e
                }
                BareNamespace::Both => {
                    let mut e = ScopeEntry::binding(name, declined(), generation);
                    e.opened_case = true;
                    e
                }
            })
            .collect();
        self.module_frame().entries.extend(entries);
    }

    /// At the start of a `rec` block's body: decline every name its
    /// declarations may bind ([`decls_bare_surface`]), and raise the barrier
    /// when that surface cannot be listed. A use written before a declaration
    /// binds it in FCS; position cannot say so, and each declaration's own
    /// entry takes the name over once the walk reaches it.
    ///
    /// A constructible type pushes no entry: it enters the slot at the block's
    /// `start` ([`Resolver::rec_block_types`]), which the slot account both bare
    /// names and dotted heads read ([`Self::type_holds_the_slot_over_the_entry`])
    /// orders against whatever holds the name. An entry would blind a dotted
    /// `T.Member` that reads the type — a `module rec` full of
    /// `Type.StaticMember` calls is ordinary F#.
    pub(super) fn decline_rec_block_surface(
        &mut self,
        decls: impl Iterator<Item = ModuleDecl>,
        start: rowan::TextSize,
    ) {
        let surface = decls_bare_surface(decls, &|attrs| self.attrs_may_auto_open(attrs));
        if surface.unknowable {
            self.open_generation += 1;
        }
        self.rec_block_types
            .push((u32::from(start), surface.type_names));
        let mut names = surface.names;
        for name in surface.barless_candidates {
            if !surface.defined_types.contains(&name) && !self.type_name_certainly_in_scope(&name) {
                names.push((name, BareNamespace::Both));
            }
        }
        self.push_bare_declines(names);
    }

    /// Whether some type named `name` is **certainly** in scope here, at any
    /// arity — FCS's `LookupTypeNameInEnvNoArity`, which decides whether a
    /// bar-less `type T = Nn` is an abbreviation. Existence, not identity:
    /// FSharp.Core's `string` is in scope as an abbreviation marker even where
    /// the type it stands for is not loaded, and that is enough.
    ///
    /// The sources are a sound subset of FCS's, each **accessible** here by
    /// construction: this file's types along the container chain, and the
    /// referenced assemblies' public types — not modules, which a type lookup
    /// does not find (codex review) — and abbreviation markers in every
    /// namespace an `open` reads — the implicit FSharp.Core opens included — in
    /// the enclosing namespace, and at the root. A project type an `open`
    /// brings in is left out: the exports do not say whether it is `private`,
    /// and an inaccessible type leaves the candidate a case (codex review). A
    /// type the subset misses can only make a bar-less candidate decline.
    pub(super) fn type_name_certainly_in_scope(&self, name: &str) -> bool {
        let in_file = (0..=self.container_path.len()).any(|k| {
            self.type_defs
                .get(&self.container_path[..k])
                .is_some_and(|types| types.contains_key(name))
        });
        let enclosing = self.enclosing_namespace().to_vec();
        let root: Vec<String> = Vec::new();
        in_file
            || self
                .imports
                .iter()
                .flat_map(|group| group.readings.iter())
                .chain([&enclosing, &root])
                .any(|ns| self.assemblies.declares_type_at_any_arity(ns, name))
    }

    /// Whether `attrs` may mark FSharp.Core's `AutoOpenAttribute`: an attribute
    /// spelled `AutoOpen`, or one named — with or without its `Attribute`
    /// suffix — like an abbreviation in reach, which FCS would chase to it
    /// (`type MyOpenAttribute = AutoOpenAttribute`, then `[<MyOpen>]`; codex
    /// review). Name-based, so it can be asked before the attributes resolve
    /// (a `rec` block's surface is read at its start): this file's
    /// abbreviations, an earlier file's types of any kind, and the referenced
    /// assemblies' abbreviation markers in every namespace in scope.
    pub(super) fn attrs_may_auto_open(&self, attrs: Vec<AttributeList>) -> bool {
        if attrs_auto_open(attrs.iter().cloned()) {
            return true;
        }
        let enclosing = self.enclosing_namespace().to_vec();
        let root: Vec<String> = Vec::new();
        attrs
            .iter()
            .flat_map(|list| list.attributes().collect::<Vec<_>>())
            .filter_map(|attr| attr.type_name())
            .any(|li| {
                let segs: Vec<String> = li.idents().map(|t| id_text(&t).to_string()).collect();
                let Some((last, qualifier)) = segs.split_last() else {
                    return true;
                };
                let names = [last.clone(), format!("{last}Attribute")];
                let written = qualifier.to_vec();
                let namespaces: Vec<&Vec<String>> = self
                    .imports
                    .iter()
                    .flat_map(|group| group.readings.iter())
                    .chain([&enclosing, &root])
                    .chain((!qualifier.is_empty()).then_some(&written))
                    .collect();
                names.iter().any(|name| {
                    self.own_abbrev_type_simple_names.contains(name)
                        || self.preceding.project_type_simple_names.contains(name)
                        || namespaces
                            .iter()
                            .any(|ns| self.assemblies.declares_abbreviation(ns, name))
                })
            })
    }

    /// A bar-less `type T = Nn` is a single-case union whose case is `Nn`
    /// unless a type `Nn` is in scope there (`TyconCoreAbbrevThatIsReallyAUnion`;
    /// `type Nn = Nn` always is one). Sema models it as an abbreviation and so
    /// interns no case, so the case declines in both namespaces from here on —
    /// unless a type of the name is provably in scope
    /// ([`Self::type_name_certainly_in_scope`]), which proves FCS reads an
    /// abbreviation. Asked once every type of the `and` group is defined and
    /// before any member body is walked: FCS establishes the group's tycons —
    /// this case included — before it checks a member, so a member written
    /// earlier in the group already sees it.
    ///
    /// The case is also recorded ([`ExportDeclKind::BarlessCase`]) so every
    /// fold of the container — the fold-back, an `open`, an implicit
    /// namespace fold, from this file or a later one — declines it by name
    /// ([`Resolver::open_module_values`]). Not as a hidden-module marker: an
    /// abbreviation whose target sema cannot resolve looks exactly like a case,
    /// and a marker would make every `open` of its container opaque.
    pub(super) fn decline_barless_case(&mut self, defn: &TypeDefn) {
        if is_type_augmentation(defn) {
            return;
        }
        let Some(case) = barless_case_candidate(defn) else {
            return;
        };
        // A `[<RequireQualifiedAccess>]` union's case is reached as `T.Nn`
        // only, so it binds nothing bare.
        if attrs_require_qualified_access(defn.attributes()) {
            return;
        }
        let self_named = defn
            .long_id()
            .and_then(single_ident)
            .is_some_and(|n| id_text(&n) == id_text(&case));
        if !self_named && self.type_name_certainly_in_scope(&id_text(&case)) {
            return;
        }
        let name = id_text(&case).to_string();
        self.push_bare_declines(vec![(name.clone(), BareNamespace::Both)]);
        self.push_export_decl(
            self.container_path.clone(),
            defn.syntax().text_range().start(),
            ExportDeclKind::BarlessCase {
                name,
                private: super::decls::type_header_is_private(defn),
            },
        );
    }

    /// A type group holding an `[<AutoOpen>]` type that folds statics
    /// ([`folds_statics`]), or an `[<AutoOpen>]` exception: its statics enter
    /// the container — and, for a group, every member body in it — under names
    /// this walk does not list, so the barrier goes up **before** the group, and
    /// everything already in scope declines past it. The group's own cases are
    /// pushed after it and stay; where one shares a rank with a static FCS keeps
    /// the later in source, which no position here records, so those decline
    /// too (`case_names`).
    pub(super) fn raise_barrier_for_auto_open_statics(&mut self, case_names: Vec<String>) {
        self.open_generation += 1;
        self.pending_static_case_declines = case_names;
    }

    /// The second half of [`Self::raise_barrier_for_auto_open_statics`], once
    /// the group's cases are in: decline them.
    pub(super) fn decline_cases_beside_auto_open_statics(&mut self) {
        let names = std::mem::take(&mut self.pending_static_case_declines);
        self.push_bare_declines(
            names
                .into_iter()
                .map(|n| (n, BareNamespace::Both))
                .collect(),
        );
    }

    /// After the fold of an explicit `open` of a module this file declares:
    /// decline what the fold could not enumerate — the module's `extern`s and
    /// active-pattern cases ([`Self::unenumerated_member_names_in`]). (Its
    /// bar-less cases the fold declines itself.) Declining last is the sound
    /// side of the ordering inside the module, exactly as for the fold-back:
    /// where the module's own value outranks one of them, the name declines
    /// rather than stands. An `[<AutoOpen>]` type or exception in the module
    /// ([`Resolver::own_auto_open_static_containers`]) raises the barrier
    /// instead.
    pub(super) fn decline_unenumerated_after_open(&mut self, module: &[String], pos: u32) {
        let whole_file = rowan::TextRange::new(0.into(), u32::MAX.into());
        let names = self.unenumerated_member_names_in(module, whole_file);
        if self
            .own_auto_open_static_containers
            .iter()
            .any(|c| c.as_slice() == module)
        {
            self.open_generation += 1;
        }
        let generation = self.open_generation;
        let declined = || Resolution::Deferred(DeferredReason::UnboundName);
        let entries: Vec<ScopeEntry> = names
            .into_iter()
            .map(|(name, ns)| {
                let mut e = match ns {
                    BareNamespace::Pattern => {
                        ScopeEntry::opened_pattern_only(name, declined(), generation, pos)
                    }
                    BareNamespace::Value | BareNamespace::Both => {
                        ScopeEntry::opened(name, declined(), generation, pos)
                    }
                };
                e.opened_case = ns != BareNamespace::Value;
                e
            })
            .collect();
        self.module_frame().entries.extend(entries);
    }
}

impl Resolver<'_> {
    /// An abbreviation enters FCS's unqualified slot exactly when the type it
    /// stands for does (`mayHaveConstruction` reads the expanded type), so
    /// once its right-hand side has resolved, take the target's slot class in
    /// place of the [`SlotClass::Unknown`] the syntax alone gives it. One
    /// level: a target that is itself an abbreviation stays `Unknown`, and so
    /// does one that did not resolve. [`Self::type_holds_the_slot_over_the_entry`]
    /// and the dotted head read the result — `type Diff = Diff'<string>` over
    /// a union keeps a same-named case standing.
    pub(super) fn chase_abbreviation_slot(&mut self, defn: &TypeDefn) {
        let Some(TypeDefnRepr::Abbrev(a)) = defn.repr() else {
            return;
        };
        let Some(name) = defn.long_id().and_then(single_ident) else {
            return;
        };
        let mut ty = a.ty();
        let head = loop {
            match ty {
                Some(Type::Paren(p)) => ty = p.inner(),
                Some(Type::App(app)) => ty = app.type_name(),
                Some(Type::LongIdent(t)) => break t.long_ident().and_then(|li| li.idents().last()),
                _ => break None,
            }
        };
        let Some(head) = head else { return };
        let class = match self.resolutions.get(&head.text_range()) {
            Some(Resolution::Local(target)) => {
                let target = *target;
                self.type_defs.iter().find_map(|(container, types)| {
                    types
                        .iter()
                        .find(|(_, id)| **id == target)
                        .and_then(|(n, _)| self.type_slot_classes.get(container)?.get(n).copied())
                })
            }
            Some(Resolution::Entity(h)) if !self.assemblies.is_abbreviation(*h) => {
                let entity = self.assemblies.entity(*h);
                Some(super::lookup::assembly_slot_class(
                    entity.kind,
                    entity.is_struct,
                ))
            }
            _ => None,
        };
        if let Some(class) = class.filter(|c| *c != SlotClass::Unknown) {
            self.type_slot_classes
                .entry(self.container_path.clone())
                .or_default()
                .insert(id_text(&name).to_string(), class);
        }
    }
}
