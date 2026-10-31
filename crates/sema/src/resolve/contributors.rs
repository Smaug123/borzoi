//! **Contributors the walk does not model** (`docs/unmodelled-contributors.md`).
//!
//! The walk models a binding by pushing a scope entry where F# adds it to the
//! name environment, and F#'s environment is latest-write-wins. Some routes add
//! names where the walk pushes no entry of its own: an `[<AutoOpen>]` module
//! or type folding back into its enclosing scope, an earlier file's root-level
//! one, an `open` of a module whose `[<AutoOpen>]` children it does not fold, a
//! `rec` block's later binders, a constructible type taking the unqualified
//! slot, an `extern` prototype, a static the assembly projection drops. Each is
//! answered by one primitive: at the position F# adds the contributor's names,
//! [`Resolver::push_contribution`] pushes a **mark** per name it may add
//! ([`ScopeEntry::contributor_mark`]). A mark declines, and shadows every
//! earlier entry of its name by position exactly as the contributor's own
//! binding would, so a use the contributor could take finds the mark and
//! declines instead of committing the entry it hid. A contributor whose names
//! are spelled nowhere in the source (an `[<AutoOpen>]` abbreviation's or base
//! class's statics) stales every earlier entry instead, through the open
//! generation.
//!
//! The name sets are closed over-approximations read off the syntax: every
//! name such a contributor adds is *declared* in it — a `let` binder, a union
//! or exception case, an active-pattern case, an `extern`, a type name, a
//! member — so collecting the declarations cannot miss one, and a name it
//! declares but does not in fact bring into bare scope costs a decline.

use std::collections::{BTreeSet, HashSet};

use borzoi_cst::syntax::{
    ActivePatName, AstNode, AttributeList, ExceptionDefnDecl, Expr, ModuleDecl, ModuleOrNamespace,
    ModuleOrNamespaceKind, NestedModuleDecl, SigFile, SyntaxKind, SyntaxNode, Type, TypeDefn,
    TypeDefnRepr, TypeDefnsDecl, UnionCase,
};

use super::decls::type_slot_class;
use super::model::SlotClass;
use super::model::{ProjectItems, ResolvedFile};
use super::state::{MarkReach, ScopeEntry};
use super::{
    Resolver, abbrev_target_single_ident, attrs_auto_open, is_type_augmentation,
    type_defn_renames_core_attribute,
};
use crate::assembly_env::{AssemblyEnv, CustomOperationNames};
use crate::binders::{BinderRole, PatternName, pattern_names};
use crate::def::{DefKind, id_text};

/// The bare names one contributor may add to the environment: a closed
/// over-approximation of them, or `unlisted` when they are spelled nowhere in
/// the source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Contribution {
    /// Cases and values that may be literals: they contest every position.
    pub(super) names: BTreeSet<String>,
    /// Values that cannot be literals: they contest expression position only
    /// ([`MarkReach::Expression`]).
    pub(super) values: BTreeSet<String>,
    pub(super) unlisted: bool,
}

impl Contribution {
    /// A contribution of the one non-literal value `name`.
    pub(super) fn of_value(name: String) -> Self {
        Contribution {
            values: std::iter::once(name).collect(),
            ..Contribution::default()
        }
    }

    pub(super) fn merge(&mut self, other: &Contribution) {
        self.names.extend(other.names.iter().cloned());
        self.values.extend(other.values.iter().cloned());
        self.unlisted |= other.unlisted;
    }

    pub(super) fn is_empty(&self) -> bool {
        self.names.is_empty() && self.values.is_empty() && !self.unlisted
    }
}

/// The active `rec` block's contributions ([`Resolver::rec_contribution`]):
/// everything it declares, and the part its nested modules declare.
#[derive(Debug, Clone, Default)]
pub(super) struct RecBlock {
    pub(super) all: Contribution,
    pub(super) modules: Contribution,
}

/// What one file's declarations contribute to later files through routes the
/// walk does not model. Part of the file's export contribution
/// ([`ResolvedFile::same_export_contribution`](super::model::ResolvedFile)).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct FileContributions {
    /// What the file adds to the root scope of every later file
    /// (`AddLocalRootModuleOrNamespace`): its root-level `[<AutoOpen>]`
    /// modules, and the declarations of its `namespace global` blocks.
    pub(super) root: Contribution,
    /// For a project module, what an `open` of it adds beyond the values the
    /// walk folds ([`Resolver::open_module_values`]): its `[<AutoOpen>]`
    /// children, at any depth through `[<AutoOpen>]` ones.
    pub(super) module_opens: Vec<(Vec<String>, Contribution)>,
    /// For a namespace, what an `open` of it adds that the namespace fold does
    /// not model: the `[<AutoOpen>]` children whose attribute is not spelled
    /// `AutoOpen`, and the `[<AutoOpen>]` types.
    pub(super) namespace_opens: Vec<(Vec<String>, Contribution)>,
    /// The names the custom operations the file's type augmentations declare
    /// may take ([`super::custom_operations::file_custom_operations`]): an
    /// extension of any builder in any later computation expression.
    pub(super) custom_operations: CustomOperationNames,
    /// Whether the file declares a type spelled like
    /// `CustomOperationAttribute` or an abbreviation of it, after which any
    /// attribute may be one.
    pub(super) declares_custom_operation_alias: bool,
}

/// Every name anything declared in `node` can bear: each identifier written
/// outside an expression, a type annotation or an attribute, with the
/// accessor names a property or event of that name brings (`get_P`, `set_P`,
/// `add_E`, `remove_E`), and the compiled names of operator-named union cases
/// (`op_Nil`, `op_ColonColon`), which are spelled with no identifier at all.
fn declared_identifiers(node: &SyntaxNode) -> BTreeSet<String> {
    let idents: Vec<String> = node
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::IDENT_TOK)
        .filter(|tok| {
            !tok.parent_ancestors().take_while(|n| n != node).any(|n| {
                Expr::can_cast(n.kind())
                    || Type::can_cast(n.kind())
                    || AttributeList::can_cast(n.kind())
            })
        })
        .map(|tok| id_text(&tok).to_string())
        .collect();
    let mut names: BTreeSet<String> = idents
        .iter()
        .flat_map(|name| {
            ["", "get_", "set_", "add_", "remove_"]
                .into_iter()
                .map(move |prefix| format!("{prefix}{name}"))
        })
        .collect();
    names.extend(operator_case_names(node));
    names
}

/// The compiled names of the operator-named union cases declared in `node`.
fn operator_case_names(node: &SyntaxNode) -> impl Iterator<Item = String> + '_ {
    node.descendants()
        .filter_map(UnionCase::cast)
        .filter_map(|case| case.operator_name().map(|(name, _)| name.to_string()))
}

impl Resolver<'_> {
    /// Whether written attribute names can be taken at their word for
    /// `[<AutoOpen>]` and `[<RequireQualifiedAccess>]`: neither this file nor
    /// an earlier one renames or impersonates either, no referenced assembly
    /// certainly does, and none of `spellings` is an assembly abbreviation
    /// whose target was not decoded. FCS honours an attribute through an
    /// abbreviation (`type AO = AutoOpenAttribute`, then `[<AO>]` — it warns),
    /// so a spelling proves nothing once one may exist.
    pub(super) fn attribute_spellings_trusted<'s>(
        &self,
        mut spellings: impl Iterator<Item = &'s str>,
    ) -> bool {
        let aliases = self.assemblies.core_attribute_aliases();
        !self.declares_core_attribute_alias
            && !self.preceding.core_attribute_alias_declared
            && !aliases.certain
            && spellings.all(|s| !aliases.possible.contains(s))
    }

    /// Whether attribute lists *may* mark their target with the core
    /// attribute spelled `core` (`AutoOpen` or `RequireQualifiedAccess`):
    /// spelled so, or carrying any attribute while written names cannot be
    /// trusted ([`Self::attribute_spellings_trusted`]).
    fn may_carry_core_attribute(
        &self,
        attrs: impl Iterator<Item = AttributeList>,
        core: &str,
    ) -> bool {
        let spellings: Vec<String> = attrs
            .flat_map(|list| list.attributes().collect::<Vec<_>>())
            .filter_map(|attr| attr.type_name())
            .filter_map(|li| li.idents().last())
            .map(|seg| id_text(&seg).to_string())
            .collect();
        let long = format!("{core}Attribute");
        spellings.iter().any(|s| *s == core || *s == long)
            || (!spellings.is_empty()
                && !self.attribute_spellings_trusted(spellings.iter().map(String::as_str)))
    }

    /// Whether attribute lists *may* mark their target `[<AutoOpen>]`.
    pub(super) fn may_auto_open(&self, attrs: impl Iterator<Item = AttributeList>) -> bool {
        self.may_carry_core_attribute(attrs, "AutoOpen")
    }

    /// Whether attribute lists *may* mark their target
    /// `[<RequireQualifiedAccess>]`.
    pub(super) fn may_require_qualified_access(
        &self,
        attrs: impl Iterator<Item = AttributeList>,
    ) -> bool {
        self.may_carry_core_attribute(attrs, "RequireQualifiedAccess")
    }

    /// What a block of module declarations puts in bare scope when the block
    /// is opened: its `let` binders, its types' names and union cases, its
    /// exceptions, `extern`s and active-pattern cases, and — recursively — its
    /// `[<AutoOpen>]` modules' and types' contributions. A plain nested
    /// module contributes nothing: its contents stay qualified.
    pub(super) fn decls_contribution(
        &self,
        decls: impl Iterator<Item = ModuleDecl>,
    ) -> Contribution {
        let mut out = Contribution::default();
        for decl in decls {
            match decl {
                ModuleDecl::Let(let_decl) => {
                    // Only an attributed `let` can be a `[<Literal>]`, the one
                    // value that contests the pattern table.
                    let attributed = let_decl.syntax().descendants().any(|n| {
                        AttributeList::can_cast(n.kind())
                            && !n
                                .ancestors()
                                .take_while(|a| a != let_decl.syntax())
                                .any(|a| Expr::can_cast(a.kind()))
                    });
                    let mut ap_cases: Vec<String> = Vec::new();
                    let binders = if attributed {
                        &mut out.names
                    } else {
                        &mut out.values
                    };
                    for binding in let_decl.bindings() {
                        let Some(head) = binding.pat() else {
                            // No head to read the binder from: anything the
                            // binding spells might be one.
                            binders.extend(declared_identifiers(binding.syntax()));
                            continue;
                        };
                        for name in pattern_names(&head, BinderRole::Let, &HashSet::new()) {
                            if let PatternName::Binder(def) = name
                                && def.kind != DefKind::Parameter
                            {
                                binders.insert(def.key().to_string());
                            }
                        }
                        // An active pattern's cases are pattern constructors.
                        for ap in head.syntax().descendants().filter_map(ActivePatName::cast) {
                            ap_cases.extend(ap.case_tokens().map(|t| id_text(&t).to_string()));
                        }
                    }
                    out.names.extend(ap_cases);
                }
                ModuleDecl::Types(types) => {
                    for defn in types.defns() {
                        out.merge(&self.type_names(&defn));
                        if self.may_auto_open(defn.attributes()) && defn.typar_decls().is_none() {
                            out.merge(&type_contribution(&defn));
                        }
                        // An intrinsic augmentation's statics join its type's
                        // surface, which an `[<AutoOpen>]` type folds.
                        if is_type_augmentation(&defn) {
                            out.values.extend(declared_identifiers(defn.syntax()));
                        }
                    }
                }
                ModuleDecl::Exception(exn) => {
                    if let Some(name) = exn.union_case().and_then(|c| c.ident()) {
                        out.names.insert(id_text(&name).to_string());
                    }
                    // An exception is a type: `[<AutoOpen>]` folds its statics.
                    if self.may_auto_open(exn.attributes()) {
                        out.values.extend(declared_identifiers(exn.syntax()));
                    }
                }
                ModuleDecl::Extern(ext) => {
                    if let Some(name) = ext.name().and_then(|n| n.idents().last()) {
                        out.values.insert(id_text(&name).to_string());
                    }
                }
                ModuleDecl::NestedModule(nm) => {
                    if self.may_auto_open(nm.attributes()) {
                        out.merge(&self.decls_contribution(nm.decls()));
                    }
                }
                ModuleDecl::Expr(_)
                | ModuleDecl::Open(_)
                | ModuleDecl::ModuleAbbrev(_)
                | ModuleDecl::Attributes(_)
                | ModuleDecl::HashDirective(_) => {}
            }
        }
        out
    }

    /// The names a type definition itself puts in bare scope: its own name,
    /// if it is constructible and so takes the unqualified slot, and its union
    /// cases. The name contests a dotted head too: the head reads types
    /// through the walk's type tables, which hold no contributor's.
    fn type_names(&self, defn: &TypeDefn) -> Contribution {
        let mut out = Contribution::default();
        if type_slot_class(defn) != SlotClass::Keeps
            && let Some(name) = defn.long_id().and_then(|li| li.idents().last())
        {
            out.values.insert(id_text(&name).to_string());
        }
        for case in defn.syntax().descendants().filter_map(UnionCase::cast) {
            if let Some(name) = case.ident() {
                out.names.insert(id_text(&name).to_string());
            }
        }
        out.names.extend(operator_case_names(defn.syntax()));
        // `type T = Nn` is a single-case union when `Nn` names no type in
        // scope (`TyconCoreAbbrevThatIsReallyAUnion`), which the syntax
        // cannot decide.
        if let Some(TypeDefnRepr::Abbrev(abbrev)) = defn.repr()
            && let Some(case) = abbrev.ty().as_ref().and_then(abbrev_target_single_ident)
        {
            out.names.insert(id_text(&case).to_string());
        }
        out
    }

    /// Push `contribution`'s marks into the current frame at `pos`
    /// (module docs). An unlisted contribution stales every earlier entry
    /// first; the named marks then follow at the new generation, so each still
    /// names its own cause.
    pub(super) fn push_contribution(&mut self, contribution: &Contribution, pos: u32) {
        if contribution.unlisted {
            self.open_generation += 1;
        }
        let generation = self.open_generation;
        let marks: Vec<ScopeEntry> = contribution
            .values
            .iter()
            .map(|name| (name, MarkReach::Expression))
            .chain(
                contribution
                    .names
                    .iter()
                    .map(|name| (name, MarkReach::Both)),
            )
            .map(|(name, reach)| ScopeEntry::contributor_mark(name.clone(), reach, generation, pos))
            .collect();
        self.module_frame().entries.extend(marks);
    }
}

/// What an `[<AutoOpen>]` type puts in bare scope: its statics
/// (`AddStaticContentOfTypeToNameEnv` — its own, not a base class's).
/// Unlisted when they need not be spelled in it: an abbreviation's are its
/// target's, and a delegate's or inline-IL type's are not source at all.
pub(super) fn type_contribution(defn: &TypeDefn) -> Contribution {
    let spelled = matches!(
        defn.repr(),
        Some(
            TypeDefnRepr::Record(_)
                | TypeDefnRepr::Union(_)
                | TypeDefnRepr::Enum(_)
                | TypeDefnRepr::ObjectModel(_)
        )
    );
    if spelled {
        Contribution {
            names: declared_identifiers(defn.syntax()),
            ..Contribution::default()
        }
    } else {
        Contribution {
            unlisted: true,
            ..Contribution::default()
        }
    }
}

impl Resolver<'_> {
    /// Whether the current container is a namespace (rather than a module):
    /// its path holds no module segment.
    fn container_is_namespace(&self) -> bool {
        self.container_path.len() == self.namespace_depth
    }

    /// Record `contribution` as what an `open` of the current container adds
    /// beyond what the walk folds for it. A module's `open` folds only its own
    /// values, so every `[<AutoOpen>]` child is recorded; a namespace's fold
    /// models the children spelled `[<AutoOpen>]` (`spelled`), so only the
    /// rest are. The root namespace's are this file's root contribution,
    /// recorded once for the whole block.
    fn record_open_contribution(&mut self, contribution: &Contribution, spelled: bool) {
        if self.anonymous_root || self.container_path.is_empty() || contribution.is_empty() {
            return;
        }
        let into = if !self.container_is_namespace() {
            &mut self.contributions.module_opens
        } else if !spelled {
            &mut self.contributions.namespace_opens
        } else {
            return;
        };
        match into.iter_mut().find(|(p, _)| *p == self.container_path) {
            Some((_, c)) => c.merge(contribution),
            None => into.push((self.container_path.clone(), contribution.clone())),
        }
    }

    /// An `[<AutoOpen>]` nested module, just closed: its contents fold into
    /// the rest of its container, and into an `open` of the container.
    pub(super) fn auto_open_module_contribution(&mut self, nm: &NestedModuleDecl) {
        if !self.may_auto_open(nm.attributes()) {
            return;
        }
        let contribution = self.decls_contribution(nm.decls());
        let end = u32::from(nm.syntax().text_range().end());
        self.push_contribution(&contribution, end);
        self.record_open_contribution(&contribution, attrs_auto_open(nm.attributes()));
    }

    /// A type-definition group, before its walk: an `[<AutoOpen>]` type's
    /// statics enter the container (`AddLocalTycons`, ahead of the members).
    /// A constructible type's own name takes the unqualified slot too, which
    /// the walk models by position ([`Resolver::bare_value_evicted`]).
    pub(super) fn type_group_contributions(&mut self, types: &TypeDefnsDecl) {
        let pos = u32::from(types.syntax().text_range().start());
        for defn in types.defns() {
            let name = defn
                .long_id()
                .and_then(|li| li.idents().last())
                .map(|t| id_text(&t).to_string());
            if self.may_auto_open(defn.attributes()) && defn.typar_decls().is_none() {
                let contribution = type_contribution(&defn);
                self.push_contribution(&contribution, pos);
                self.record_open_contribution(&contribution, false);
                if let Some(name) = name {
                    let mut path = self.container_path.clone();
                    path.push(name);
                    self.auto_open_type_paths.insert(path);
                }
            } else if is_type_augmentation(&defn)
                && let Some(name) = name
            {
                // An intrinsic augmentation of an `[<AutoOpen>]` type extends
                // the statics that type folds into its container.
                let mut path = self.container_path.clone();
                path.push(name);
                if self.auto_open_type_paths.contains(&path) {
                    let contribution = Contribution {
                        values: declared_identifiers(defn.syntax()),
                        ..Contribution::default()
                    };
                    self.push_contribution(&contribution, pos);
                    self.record_open_contribution(&contribution, false);
                }
            }
        }
    }

    /// An `[<AutoOpen>]` exception, before its walk: an exception is a type,
    /// and its statics enter the container as an `[<AutoOpen>]` type's do.
    pub(super) fn exception_contribution(&mut self, exn: &ExceptionDefnDecl) {
        if !self.may_auto_open(exn.attributes()) {
            return;
        }
        let contribution = Contribution {
            values: declared_identifiers(exn.syntax()),
            ..Contribution::default()
        };
        self.push_contribution(&contribution, u32::from(exn.syntax().text_range().start()));
        self.record_open_contribution(&contribution, false);
    }

    /// A union whose cases the walk just put in bare scope, because its
    /// attributes are not spelled `[<RequireQualifiedAccess>]`: if they may
    /// still be that attribute under another name, FCS may hold the cases
    /// qualified-only, and a bare use then binds whatever they would shadow.
    /// Neither reading can be committed, here or through an `open` of the
    /// container.
    pub(super) fn unproven_bare_cases(&mut self, defn: &TypeDefn) {
        if !self.may_require_qualified_access(defn.attributes()) {
            return;
        }
        let cases = Contribution {
            names: defn
                .syntax()
                .descendants()
                .filter_map(UnionCase::cast)
                .filter_map(|c| c.ident())
                .map(|t| id_text(&t).to_string())
                .chain(operator_case_names(defn.syntax()))
                .collect(),
            ..Contribution::default()
        };
        let pos = u32::from(defn.syntax().text_range().end());
        self.push_contribution(&cases, pos);
        self.record_open_contribution(&cases, false);
    }

    /// Entering a `rec` block's module: everything it declares is in scope
    /// from here (`TcMutRecDefns_Phase1` adds the block's binders before
    /// checking any of it).
    pub(super) fn enter_rec_block(&mut self, decls: impl Iterator<Item = ModuleDecl>, pos: u32) {
        let decls: Vec<ModuleDecl> = decls.collect();
        let all = self.decls_contribution(decls.iter().cloned());
        let modules = self.decls_contribution(
            decls
                .into_iter()
                .filter(|d| matches!(d, ModuleDecl::NestedModule(_))),
        );
        self.push_contribution(&all, pos);
        let mut active = self.rec_contribution.take().unwrap_or_default();
        active.all.merge(&all);
        active.modules.merge(&modules);
        self.rec_contribution = Some(active);
    }

    /// After an `open` in a `rec` block: FCS adds the block's opens first and
    /// every binder after them, so the block's binders outrank it.
    pub(super) fn reassert_rec_block(&mut self, pos: u32) {
        if let Some(block) = &self.rec_contribution {
            // The names, not the unlisted part: entering the block already
            // staled everything the open could have hidden behind it.
            let named = Contribution {
                unlisted: false,
                ..block.all.clone()
            };
            self.push_contribution(&named, pos);
        }
    }

    /// After a type or exception definition in a `rec` block: FCS adds the
    /// block's modules — their `[<AutoOpen>]` contents with them — after its
    /// types and exceptions, so those contents outrank the cases just pushed.
    pub(super) fn reassert_rec_modules(&mut self, pos: u32) {
        if let Some(block) = &self.rec_contribution {
            let named = Contribution {
                unlisted: false,
                ..block.modules.clone()
            };
            self.push_contribution(&named, pos);
        }
    }

    /// After an `open` of project module `path`: its `[<AutoOpen>]` children,
    /// which [`Self::open_module_values`] does not fold.
    pub(super) fn module_open_contributions(&mut self, path: &[String], pos: u32) {
        let mut contribution = Contribution::default();
        for (p, c) in &self.contributions.module_opens {
            if p == path {
                contribution.merge(c);
            }
        }
        if let Some(c) = self.preceding.module_open_contributions.get(path) {
            contribution.merge(c);
        }
        self.push_contribution(&contribution, pos);
    }

    /// After an `open` (or the implicit open) of project namespace `path`:
    /// what its fold does not model.
    pub(super) fn namespace_open_contributions(&mut self, path: &[String], pos: u32) {
        let mut contribution = Contribution::default();
        for (p, c) in &self.contributions.namespace_opens {
            if p == path {
                contribution.merge(c);
            }
        }
        if let Some(c) = self.preceding.namespace_open_contributions.get(path) {
            contribution.merge(c);
        }
        self.push_contribution(&contribution, pos);
    }

    /// A top-level block, after the implicit auto-opens are seeded and before
    /// its declarations: what earlier files put in its root scope
    /// (`AddLocalRootModuleOrNamespace`), then its own namespace's implicit
    /// open with what that open adds unmodelled — run here so the two land in
    /// FCS's order — then, for a `rec` block, its binders. Also records what
    /// this block puts in later files' root scope.
    pub(super) fn block_start_contributions(&mut self, module: &ModuleOrNamespace) {
        let root = self.preceding.root_contribution.clone();
        self.push_contribution(&root, 0);
        self.open_own_enclosing_namespace();
        let enclosing = self.enclosing_namespace().to_vec();
        if !enclosing.is_empty() {
            self.namespace_open_contributions(&enclosing, 0);
        }
        match module.kind() {
            ModuleOrNamespaceKind::GlobalNamespace => {
                let contribution = self.decls_contribution(module.decls());
                self.contributions.root.merge(&contribution);
            }
            ModuleOrNamespaceKind::NamedModule if self.may_auto_open(module.attributes()) => {
                let contribution = self.decls_contribution(module.decls());
                if self.container_path.len() == 1 {
                    self.contributions.root.merge(&contribution);
                } else if !attrs_auto_open(module.attributes()) {
                    let ns = self.container_path[..self.container_path.len() - 1].to_vec();
                    match self
                        .contributions
                        .namespace_opens
                        .iter_mut()
                        .find(|(p, _)| *p == ns)
                    {
                        Some((_, c)) => c.merge(&contribution),
                        None => self.contributions.namespace_opens.push((ns, contribution)),
                    }
                }
            }
            _ => {}
        }
        self.rec_contribution = None;
        if module.is_rec() {
            let pos = u32::from(module.syntax().text_range().start());
            self.enter_rec_block(module.decls(), pos);
        }
    }
}

/// What a signature file contributes to later files through routes the walk
/// does not model, written into its slot's `rf`. A signature's attributes are
/// the ones later files see, so a top-level `[<AutoOpen>] module X` it
/// declares opens into every later file's root
/// (`AddLocalRootModuleOrNamespace`) whatever its implementation says; so do
/// the declarations of a `namespace global` block. Whether an attribute may be
/// `[<AutoOpen>]` is decided as for an implementation
/// ([`Resolver::attribute_spellings_trusted`]).
pub(super) fn signature_contributions(
    sig: &SigFile,
    preceding: &ProjectItems,
    assemblies: &AssemblyEnv,
    rf: &mut ResolvedFile,
) {
    let renames = sig
        .syntax()
        .descendants()
        .filter_map(TypeDefn::cast)
        .any(|defn| type_defn_renames_core_attribute(&defn));
    rf.declares_core_attribute_alias = renames;
    let aliases = assemblies.core_attribute_aliases();
    let may_auto_open = |attrs: &mut dyn Iterator<Item = AttributeList>| {
        let spellings: Vec<String> = attrs
            .flat_map(|list| list.attributes().collect::<Vec<_>>())
            .filter_map(|attr| attr.type_name())
            .filter_map(|li| li.idents().last())
            .map(|seg| id_text(&seg).to_string())
            .collect();
        let trusted = !renames
            && !preceding.core_attribute_alias_declared
            && !aliases.certain
            && spellings.iter().all(|s| !aliases.possible.contains(s));
        spellings
            .iter()
            .any(|s| s == "AutoOpen" || s == "AutoOpenAttribute")
            || (!spellings.is_empty() && !trusted)
    };
    // A signature's attributes are the ones later files see, so the custom
    // operations it declares count as an implementation's would
    // (`super::custom_operations::signature_custom_operations`).
    let custom_operation_alias =
        super::custom_operations::signature_renames_custom_operation(sig.syntax());
    rf.contributions.declares_custom_operation_alias = custom_operation_alias;
    let co_aliases = assemblies.custom_operation_aliases();
    let co_trusted = !custom_operation_alias
        && !preceding.custom_operation_alias_declared
        && !co_aliases.certain;
    rf.contributions.custom_operations =
        super::custom_operations::signature_custom_operations(sig.syntax(), &|s| {
            super::custom_operations::CUSTOM_OPERATION_SPELLINGS.contains(&s)
                || !co_trusted
                || co_aliases.possible.contains(s)
        });
    for module in sig.modules() {
        let root_module = matches!(module.kind(), ModuleOrNamespaceKind::NamedModule)
            && module.long_id().is_some_and(|li| li.idents().count() == 1)
            && may_auto_open(&mut module.attributes());
        if root_module || matches!(module.kind(), ModuleOrNamespaceKind::GlobalNamespace) {
            let names = declared_identifiers(module.syntax());
            rf.contributions.root.names.extend(names);
        }
    }
}
