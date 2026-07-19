//! Module/namespace declaration walking and export construction.

use borzoi_cst::syntax::{
    AstNode, ModuleDecl, NestedModuleDecl, SyntaxToken, TypeDefn, TypeDefnRepr,
};

use crate::assembly_env::{OpenFoldSurface, OpenFoldTarget};
use crate::def::DefId;

use super::model::{
    CaseKind, ExportDeclKind, ExportedItem, ItemId, OpenOpacity, OpenTrace, Resolution, SlotClass,
};
use super::state::{AutoOpenTypeShadow, Frame, OpenGroup, OpenInterpretation, Resolver};
use super::{
    attrs_auto_open, attrs_mark_struct, attrs_require_qualified_access, id_text,
    is_type_augmentation, single_ident, type_long_ident_path,
};

/// Whether `defn` carries a **type-header** `private` modifier (`type private
/// Color` — the `ACCESS_TOK` *before* the name's `LONG_IDENT`). FCS does not
/// import a private type at an `open` from outside its declaration group
/// (probe M20r, codex round 4), so [`Resolver::export_type_path`]
/// downgrades its slot class to [`SlotClass::Keeps`]; within its own
/// container the type is fully visible and still evicts (probe M20s), so
/// [`Resolver::define_type`]'s class is untouched. The *after-name* slot
/// (`type C private = …`, an `ACCESS_TOK` after the `LONG_IDENT`) is
/// FCS-discarded — the type stays public — and must not count; a ctor's or
/// repr's modifier nests inside its own node and never appears as a direct
/// child here. `internal` types stay visible within the project (one
/// assembly), so only `private` downgrades.
fn type_header_is_private(defn: &TypeDefn) -> bool {
    header_is_private(defn.syntax())
}

/// Whether a module header (`module private Auto = …` / `namespace private
/// …` — a [`NestedModuleDecl`] or top-level `ModuleOrNamespace`) carries a
/// `private` accessibility modifier before its name — the one
/// stop-at-`LONG_IDENT` scan, which [`type_header_is_private`] delegates to. F# does not bring
/// a `private` module into scope for another file's `open` of its namespace
/// (found by review, round 5, on `docs/completed/r2-annotation-typing-plan.md`), so an
/// `[<AutoOpen>]` module's `[<AutoOpen>]`-driven type-shadow signal must not
/// cross that file boundary when this is `true`; `internal` stays
/// project-visible (one assembly), so only `private` counts here too.
pub(super) fn header_is_private(node: &borzoi_cst::syntax::SyntaxNode) -> bool {
    use borzoi_cst::syntax::SyntaxKind;
    for el in node.children_with_tokens() {
        match el {
            rowan::NodeOrToken::Node(n) if n.kind() == SyntaxKind::LONG_IDENT => {
                return false;
            }
            rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::ACCESS_TOK => {
                return t.text() == "private";
            }
            _ => {}
        }
    }
    false
}

/// Merge one auto-open type-shadow contribution into the scope's set.
/// Same-name contributions merge **monotonically soundly** (codex round 3: a
/// later `module private` sibling's depth-pinned entry must not evict an
/// earlier public one): the minimum visible depth keeps the name shadowing
/// wherever ANY contribution is visible, and the maximum import position
/// makes the positional contest against in-file types as conservative as the
/// latest introduction — either can only widen the defer, never mis-bind.
fn merge_auto_open_shadow(
    map: &mut std::collections::HashMap<String, AutoOpenTypeShadow>,
    name: String,
    new: AutoOpenTypeShadow,
) {
    map.entry(name)
        .and_modify(|e| {
            e.min_depth = e.min_depth.min(new.min_depth);
            e.import_pos = e.import_pos.max(new.import_pos);
        })
        .or_insert(new);
}

/// Classify whether `defn`'s name enters FCS's unqualified slot (see
/// [`SlotClass`]): `[<Struct>]` forces a struct type regardless of repr
/// (probe M20m); enums are structs (M20b); an object model is class/struct
/// when explicitly kinded or carrying a primary constructor (M20a), an
/// explicit interface never enters (M20o), and an unspecified kind is
/// inference-dependent; unions/records never enter (M20k/M20l); everything
/// else — abbreviations (target-chased, M20n), delegates (langversion-gated),
/// bodyless/IL reprs — is statically undecidable.
fn type_slot_class(defn: &TypeDefn) -> SlotClass {
    match defn.repr() {
        Some(TypeDefnRepr::Enum(_)) => SlotClass::Evicts,
        Some(TypeDefnRepr::Union(_) | TypeDefnRepr::Record(_)) => {
            // A genuine `[<Struct>]` union/record IS a struct type and evicts
            // (probe M20m) — but the marker is matched textually, and a
            // CUSTOM attribute named `Struct` would be mistaken for it while
            // FCS keeps the type ordinary (codex round 7). Sema cannot
            // resolve attribute types, so a `Struct`-marked union/record is
            // Unknown — defer in contest, never a wrong target either way
            // (the genuine-marker resolve is the availability price).
            if attrs_mark_struct(defn.attributes()) {
                SlotClass::Unknown
            } else {
                SlotClass::Keeps
            }
        }
        Some(TypeDefnRepr::ObjectModel(om)) => {
            if om.is_interface() {
                SlotClass::Keeps
            } else if om.is_class() || om.is_struct() || defn.implicit_ctor().is_some() {
                SlotClass::Evicts
            } else {
                SlotClass::Unknown
            }
        }
        _ => SlotClass::Unknown,
    }
}

impl<'a> Resolver<'a> {
    /// The qualified path a later file uses to reach this export
    /// (`["Shared", "foo"]`), or `None` in an anonymous module — whose members
    /// F# reaches only through the file's implicit (filename-derived) module,
    /// which we do not model, so they are not cross-file referenceable here.
    pub(super) fn qualified_export_path(&self, name: &str) -> Option<Vec<String>> {
        self.module_path.as_ref().map(|path| {
            let mut full = path.clone();
            full.push(name.to_string());
            full
        })
    }

    /// Export a value-namespace **constructor case** (a non-qualified union case
    /// or an `exception` constructor) for cross-file resolution: it interns an
    /// [`ExportedItem`] under the case's value-namespace path (`Container.Case` —
    /// the type segment is elided, matching F#'s shortcut), so a later file's
    /// `open Container; Case` ([`open_module_values`](Self::open_module_values)) and
    /// `Container.Case` ([`resolve_long_ident`](Self::resolve_long_ident)) resolve
    /// it. Returns the new [`ItemId`] (the caller records the decl and frame entry
    /// as that [`Resolution::Item`], so the case has the *same* identity everywhere
    /// — declaration, same-file uses, and the cross-file open — which keeps
    /// find-references / rename intact, exactly as a top-level `let` does).
    ///
    /// The path is built from [`container_path`](Self::container_path), not
    /// [`module_path`](Self::module_path) (which is `None` for a `namespace`): a
    /// union/exception case **can** be declared directly under a namespace
    /// (`namespace Lib; type Color = Red | …`), and F# exposes its constructors
    /// from that namespace. `None` only for an **anonymous-root** file (no real
    /// `namespace`/`module` header — its members are reachable cross-file only via
    /// the unmodeled filename module), so the caller keeps the conservative
    /// hidden-module marking there.
    /// The [`ExportedItem::access_root_len`] for an export declared at the
    /// current walk position, given whether it (a value) or its type (a case /
    /// exception) carries its own `private` modifier. An own `private` restricts
    /// the export to its container `self.container_path` (length
    /// `container_path.len()`); otherwise it inherits [`Self::access_floor`] (a
    /// `private` enclosing module). The own container is always the deeper
    /// boundary when present (`access_floor` is an ancestor prefix), so it wins
    /// whenever `own_private`; `None` (public) when neither restricts.
    pub(super) fn export_access_root_len(&self, own_private: bool) -> Option<usize> {
        debug_assert!(
            self.access_floor
                .is_none_or(|f| f <= self.container_path.len()),
            "access_floor must be a prefix length of the current container_path"
        );
        if own_private {
            Some(self.container_path.len())
        } else {
            self.access_floor
        }
    }

    pub(super) fn export_case(
        &mut self,
        name: &str,
        def: DefId,
        type_is_private: bool,
        kind: CaseKind,
    ) -> Option<ItemId> {
        let mut qualified = self.container_path.clone();
        qualified.push(name.to_string());
        let pos = self.defs[def.index()].range.start();
        if self.anonymous_root {
            // No cross-file `ExportedItem`, but the case is still recorded (with
            // `item: None`) so its container derives into `modules_with_hidden_values`
            // — the anonymous-root union/exception hidden-value marker (plan
            // pitfall 1). Only `export_case` (non-RQA union / exception) marks the
            // container hidden here; `export_require_qualified_case` (RQA / enum)
            // does not, so it records nothing under an anonymous root.
            self.push_export_decl(
                qualified,
                pos,
                ExportDeclKind::Item {
                    item: None,
                    type_qualified: None,
                },
            );
            return None;
        }
        let item_idx = self.items.len();
        let item_id = ItemId::new(self.item_base as usize + item_idx);
        self.items.push(ExportedItem {
            name: name.to_string(),
            qualified: Some(qualified.clone()),
            id: item_id,
            def: super::model::ExportDef::Own(def),
            case: Some(kind),
            // A union/exception case inherits its *type*'s accessibility (a case
            // of a `private` type is scoped to the type's container) and any
            // enclosing `private` module (oracle-pinned D3/D5).
            access_root_len: self.export_access_root_len(type_is_private),
            attributed: false,
        });
        self.push_export_decl(
            qualified,
            pos,
            ExportDeclKind::Item {
                item: Some(item_idx),
                // A non-RQA union case's type-qualified path is threaded next by
                // `export_type_qualified_case`; an `exception` ctor has none.
                type_qualified: None,
            },
        );
        Some(item_id)
    }

    /// Attach a case's **type-qualified** export path (`[container.., type, case]`)
    /// to the case's trailing `Item` [`ExportDecl`], so a later file's
    /// `Lib.Color.Red` resolves it ([`ProjectItems::type_qualified_cases`]).
    /// Skipped in an anonymous-root file, which has no real cross-file container
    /// path (like [`Self::export_case`]) and pushed no `Item { item: Some(_) }`
    /// decl to attach to. Called immediately after the case's `Item` decl at both
    /// producer sites (a non-RQA union case, and [`Self::export_require_qualified_case`]),
    /// so that decl is always the trailing one.
    pub(super) fn export_type_qualified_case(&mut self, type_name: &str, case_name: &str) {
        if self.anonymous_root {
            return;
        }
        let mut path = self.container_path.clone();
        path.push(type_name.to_string());
        path.push(case_name.to_string());
        self.set_last_decl_type_qualified(path);
    }

    /// Record a type definition's **qualified path** (`[container.., name]`) for
    /// the cross-file type index ([`ProjectItems::type_paths`]).
    /// `cases_enumerable` is `true` when every case the type owns is in the
    /// type-qualified case index — any genuine non-abbreviation repr (a
    /// union/enum's cases are all exported alongside it; other reprs own none) —
    /// so a later file can prove case *absence* on it. An abbreviation's cases
    /// live on its target, which sema does not chase cross-file, so it passes
    /// `false` (present, but case-opaque). Skipped in an anonymous-root file (no
    /// real cross-file container path), like [`Self::export_case`].
    pub(super) fn export_type_path(&mut self, name: &str, cases_enumerable: bool, slot: SlotClass) {
        if self.anonymous_root {
            return;
        }
        let mut path = self.container_path.clone();
        path.push(name.to_string());
        self.type_path_exports.push((path, cases_enumerable, slot));
    }

    /// Give a **require-qualified** case (an enum case, or an
    /// `[<RequireQualifiedAccess>]` union case) a cross-file handle so
    /// `Lib.Color.Red` resolves to it, register its type-qualified path, and return
    /// the handle. Unlike [`Self::export_case`] the item carries **no** `qualified`
    /// value path — a require-qualified case is not in the value namespace (no bare /
    /// `Mod.Case` access), so it must not enter [`ProjectItems::by_qualified_path`];
    /// only the type-qualified index reaches it. The caller uses the returned
    /// [`ItemId`] as the case's resolution **everywhere** (declaration and same-file
    /// `Color.Red`), so it has one identity for find-references / rename, just like a
    /// non-RQA union case. `None` in an anonymous-root file (no cross-file handle),
    /// where the case stays a [`Resolution::Local`].
    pub(super) fn export_require_qualified_case(
        &mut self,
        name: &str,
        def: DefId,
        type_name: &str,
        type_is_private: bool,
        kind: CaseKind,
    ) -> Option<ItemId> {
        if self.anonymous_root {
            // No cross-file handle and — unlike `export_case` — no hidden-value
            // marker: an RQA/enum case is not in the value namespace, so an `open`
            // of the (anonymous, unmodelled) container brings no bare name.
            return None;
        }
        let item_idx = self.items.len();
        let item_id = ItemId::new(self.item_base as usize + item_idx);
        self.items.push(ExportedItem {
            name: name.to_string(),
            qualified: None,
            id: item_id,
            def: super::model::ExportDef::Own(def),
            case: Some(kind),
            // RQA case: `qualified: None`, so it never enters the open-fold value
            // namespace; the access-root is recorded for consistency (it is
            // reached only through the type-qualified index).
            access_root_len: self.export_access_root_len(type_is_private),
            attributed: false,
        });
        // The export-decl-list twin: a case `Item` decl (no value path). Its
        // type-qualified path is threaded by `export_type_qualified_case` below,
        // which sets it on this just-pushed decl.
        let pos = self.defs[def.index()].range.start();
        self.push_export_decl(
            {
                let mut p = self.container_path.clone();
                p.push(name.to_string());
                p
            },
            pos,
            ExportDeclKind::Item {
                item: Some(item_idx),
                type_qualified: None,
            },
        );
        self.export_type_qualified_case(type_name, name);
        Some(item_id)
    }

    pub(super) fn module_decl(&mut self, decl: &ModuleDecl) {
        // SOUNDNESS TRIPWIRE: this match is exhaustive on purpose. When the CST
        // grows a nested-module / project-type declaration (parser 8.3–8.4),
        // adding the arm here is mandatory — and so is registering whatever
        // paths it *provides* in `by_qualified_path`. `assembly_path_records`
        // falls through a project module *proper*-prefix to the assembly,
        // trusting that index to enumerate everything a module provides; a
        // nested member left out of it would resolve to a colliding assembly
        // type instead. `resolve_project_assembly_diff.rs` is the FCS guard, but
        // its corpus is hand-written, so add a colliding nested-module case there.
        match decl {
            ModuleDecl::Expr(e) => {
                if let Some(expr) = e.expr() {
                    self.resolve_expr(&expr);
                }
            }
            ModuleDecl::NestedModule(nm) => self.nested_module(nm),
            ModuleDecl::ModuleAbbrev(a) => {
                // A module abbreviation `module X = Bar.Baz` (parser 8.5) aliases
                // `X` to the module `Bar.Baz`. We resolve the RHS to its canonical
                // in-project module path (with the same precedence as an `open`
                // path) and record `X` → that target ([`Self::module_aliases`]), so
                // `open X`, a chained `open X; open Sub`, and `X`'s hidden-ness all
                // canonicalise through to the target. `X` is also a
                // project-introduced *name* recorded in the shadow sets (like a
                // nested module) so a reference rooted at it does not fall through
                // to a colliding referenced-assembly member.
                if let Some(ident) = a.ident() {
                    let segs: Vec<String> = ident
                        .idents()
                        .map(|t| id_text(t.text()).to_string())
                        .collect();
                    // `X` is a module-like name in this container, so it shadows a
                    // same-named enclosing type for member access (`X.foo`).
                    if let Some(first) = segs.first() {
                        self.note_module_like_name(first);
                        // Mark `X` as an *alias* (not a `Module`): the target may be
                        // cross-file, so the type-qualifier head walk must leave
                        // `X.Color.Red` to the alias-aware cross-file path — but a
                        // nearer alias still shadows an outer real module of the same
                        // name, so it stops the walk.
                        self.mark_decl(first).alias = true;
                    }
                    // Resolve the RHS target the same way an `open` path is resolved
                    // (relative to the enclosing namespace / prior opens, `global.`
                    // rooting it). Done *before* `record_project_name_shadow` below,
                    // so `module X = X` cannot resolve to itself.
                    let target = a.long_id().and_then(|li| {
                        let idents: Vec<SyntaxToken> = li.idents().collect();
                        let mut rhs: Vec<String> = idents
                            .iter()
                            .map(|t| id_text(t.text()).to_string())
                            .collect();
                        let rooted = idents.first().is_some_and(|t| t.text() == "global");
                        if rooted {
                            rhs.remove(0);
                        }
                        self.resolved_project_module(&rhs, rooted)
                    });
                    // The alias is cross-file-resolvable only with a real root (an
                    // anonymous-root alias is not in `nested_module_exports`, so an
                    // `open X` of it falls to the conservative path regardless).
                    if !self.anonymous_root && !segs.is_empty() {
                        let mut alias_path = self.container_path.clone();
                        alias_path.extend(segs.iter().cloned());
                        // Always mark `X` hidden. This is the only marker exported
                        // cross-file (`module_aliases` is same-file), so a *later*
                        // file's `open X` stays conservative — it shadows earlier
                        // opens rather than leaving a stale earlier-opened value
                        // (we do not yet follow an alias declared in an earlier
                        // file). Same-file, the mapping below canonicalises `X` →
                        // `Target` *before* any hidden-check, so this marker is not
                        // consulted there.
                        self.note_hidden_value_module(alias_path.clone());
                        // Resolvable in-project target: record the mapping so
                        // same-file resolution canonicalises `X` → `Target`. An
                        // unresolvable target (an assembly module) records no
                        // mapping — the hidden marker above keeps it conservative.
                        if let Some(target) = target {
                            self.module_aliases.insert(alias_path, target);
                        }
                    }
                    // The export-decl-list twin: one `ModuleAbbrev` decl whose
                    // `path` (container + `X`) is both its nested-module shadow and
                    // its hidden-value path (both derived only when non-anonymous).
                    if !segs.is_empty() {
                        let mut path = self.container_path.clone();
                        path.extend(segs.iter().cloned());
                        let pos = ident
                            .idents()
                            .next()
                            .map(|t| t.text_range().start())
                            .unwrap_or_else(|| a.syntax().text_range().start());
                        self.push_export_decl(path, pos, ExportDeclKind::ModuleAbbrev);
                    }
                    self.record_project_name_shadow(segs);
                }
            }
            ModuleDecl::Types(types) => {
                // A type definition (parser phase 9) introduces a project type
                // name (`type T = …`). Two things happen, in order:
                //
                // 1. The *name* is recorded in the shadow sets so a reference
                //    rooted at it (`Demo.T`) defers rather than falling through
                //    to a colliding referenced-assembly type — the same
                //    `assembly_path_records` soundness tripwire the nested-module
                //    and module-abbreviation arms guard. (This is unconditional,
                //    including for augmentations, exactly as before.)
                //
                // 2. A genuine new type definition is *interned* as a
                //    first-class [`DefKind::Type`] binder and entered in
                //    [`Self::type_defs`], so a same-file type-name use resolves
                //    to it (intra-file go-to-definition on types). An
                //    augmentation (`type T with member …`) or a qualified head
                //    (`type A.B with …`) names an *existing* type, so it is not
                //    re-interned — its head is a use the type-checker resolves,
                //    not a definition.
                //
                // Every name in an `and`-group is interned (step 2) before any
                // of their right-hand sides is resolved (step 3), so the group
                // is mutually recursive (`type R1 = { x : R2 } and R2 = …`).
                let defns: Vec<TypeDefn> = types.defns().collect();
                for defn in &defns {
                    // A genuine single-ident, non-augmentation definition carries a
                    // full `Type` decl below (at the `export_type_path` site); an
                    // augmentation (`type A.B with …`) or a dotted head records only
                    // the conflated nested-module shadow, so it gets a `Type` decl
                    // with `info: None` here.
                    let genuine = !is_type_augmentation(defn)
                        && defn.long_id().and_then(single_ident).is_some();
                    if let Some(li) = defn.long_id() {
                        let segs: Vec<String> =
                            li.idents().map(|t| id_text(t.text()).to_string()).collect();
                        self.record_project_name_shadow(segs.clone());
                        // A nameless recovered type (`type = int`, `type exception`)
                        // has empty `segs`, which `record_project_name_shadow` skips;
                        // the shadow decl must skip it too, or folding would add the
                        // *container* to `nested_module_paths` and spuriously defer
                        // later-file assembly references rooted there (codex fuzz find).
                        if !genuine && !segs.is_empty() {
                            let mut path = self.container_path.clone();
                            path.extend(segs);
                            let pos = li
                                .idents()
                                .next()
                                .map(|t| t.text_range().start())
                                .unwrap_or_else(|| defn.syntax().text_range().start());
                            self.push_export_decl(
                                path,
                                pos,
                                ExportDeclKind::Type {
                                    info: None,
                                    auto_open: false,
                                },
                            );
                        }
                    }
                    if !is_type_augmentation(defn)
                        && let Some(name) = defn.long_id().and_then(single_ident)
                    {
                        // `[<AutoOpen>]` on a TYPE (not just a module) is real F#:
                        // its public static members enter bare scope wherever the
                        // enclosing namespace/module is opened, exactly like an
                        // explicit `open type` (codex review round 5,
                        // fcs-dump-verified — `[<AutoOpen>] type T = static member
                        // Clash = …` in `namespace X` makes `open X; Clash` bind
                        // `X.T.Clash`). Sema does not enumerate a project type's
                        // members at all (no project-side `open_type_statics`
                        // equivalent exists), so those names are invisible to
                        // every enumeration this fold does — `open_module_values`,
                        // `direct_project_type_contestants`,
                        // `direct_value_names_at`. Marking the container hidden
                        // (the same signal an active pattern/anonymous-root case
                        // already gives) is the same "decline the whole thing
                        // rather than enumerate a blacklist" move
                        // `docs/assembly-module-open-plan.md` §4b/4c already made
                        // for the identical assembly-side hazard: it raises the
                        // barrier everywhere `module_has_hidden_values` is
                        // consulted, so a colliding assembly value correctly
                        // defers instead of committing (§7's `demote_module_half`
                        // wiring reads it via `project_ns_hidden` below).
                        let type_auto_open = attrs_auto_open(defn.attributes());
                        if type_auto_open {
                            self.note_hidden_value_module(self.container_path.clone());
                        }
                        let slot = type_slot_class(defn);
                        // The type's access-root (own `private` plus any enclosing
                        // `private` module) — a same-file module-qualified `A.Foo.Red`
                        // from an inaccessible site does not resolve the type's
                        // case/member.
                        let type_access_root =
                            self.export_access_root_len(type_header_is_private(defn));
                        self.define_type(&name, slot, type_access_root);
                        // The cross-file type index: any genuine non-abbreviation
                        // repr's cases are fully indexed below (a union/enum's by
                        // `define_union_cases` / `define_enum_cases`; other reprs
                        // own none), so case absence is provable cross-file. An
                        // abbreviation's cases live on its unchased target, and a
                        // bodyless / inline-IL repr stays conservative — the type
                        // is indexed as present but case-opaque.
                        let cases_enumerable = matches!(
                            defn.repr(),
                            Some(
                                TypeDefnRepr::Union(_)
                                    | TypeDefnRepr::Enum(_)
                                    | TypeDefnRepr::Record(_)
                                    | TypeDefnRepr::ObjectModel(_)
                                    | TypeDefnRepr::Delegate(_)
                            )
                        );
                        // A `private` type is invisible to an `open` from
                        // outside its declaration group (probe M20r), so the
                        // cross-file export provably keeps the slot; the
                        // in-container class (`define_type` above) is
                        // untouched — the type still evicts locally (M20s).
                        let export_slot = if type_header_is_private(defn) {
                            SlotClass::Keeps
                        } else {
                            slot
                        };
                        self.export_type_path(id_text(name.text()), cases_enumerable, export_slot);
                        // The export-decl-list twin: a genuine `Type` decl feeding
                        // `type_paths` (keyed by its own path), the nested-module
                        // shadow set, and — when `[<AutoOpen>]` — its container's
                        // hidden-value marker.
                        {
                            let mut path = self.container_path.clone();
                            path.push(id_text(name.text()).to_string());
                            self.push_export_decl(
                                path,
                                name.text_range().start(),
                                ExportDeclKind::Type {
                                    info: Some((cases_enumerable, export_slot)),
                                    auto_open: type_auto_open,
                                },
                            );
                        }
                        // Last-wins on redefinition (mirrors `type_defs`): drop any
                        // prior cases filed at this type name before re-populating,
                        // so a re-`type`d name — or a non-union/enum redefinition
                        // (`type Color = int`) — leaves no stale case for
                        // `Color.Red` to combine with the new type id. Done once
                        // here, before either populator, since both
                        // `define_union_cases` and `define_enum_cases` run for the
                        // same defn (only one matches the repr) and must not clobber
                        // the other's just-added cases.
                        if let Some(by_type) = self.type_cases.get_mut(&self.container_path) {
                            by_type.remove(id_text(name.text()));
                        }
                        // A union's cases enter the container-scoped case index
                        // here, so the whole group's cases are visible before any
                        // RHS (step 3) and to every later decl in this container
                        // (`let c = Red`). They also populate [`Self::type_cases`]
                        // so a qualified `Color.Red` resolves (for unions and enums
                        // uniformly); `[<RequireQualifiedAccess>]` keeps the cases
                        // out of the *value* frame — reachable then only as `T.Case`.
                        let require_qualified = attrs_require_qualified_access(defn.attributes());
                        self.define_union_cases(defn, &name, require_qualified);
                        // An enum's cases (`Color = Red = 0 | …`) populate the same
                        // [`Self::type_cases`] index — never the value frame, so a
                        // bare `Red` stays unresolved while `Color.Red` resolves.
                        self.define_enum_cases(defn, &name);
                        // The type's members (object-model + trailing) enter the
                        // in-file member index, powering the qualified static-
                        // member emit (`Color.Red`, probes M1/M2a — see
                        // `docs/project-type-member-plan.md`).
                        self.define_type_members(defn, &name);
                    }
                }
                // 3. Now every name in the group is in scope, resolve the uses:
                //    an augmentation's head (`type T with …`) is a *use* of an
                //    existing type — resolve it against the table (a genuine
                //    definition's head is the defining occurrence, already
                //    self-recorded by `define_type`) — and the type uses inside
                //    each definition's right-hand side.
                for defn in &defns {
                    if is_type_augmentation(defn) {
                        // EX-3 §2(a): the augmentation's member *names* join
                        // the extension gate's name sets, so the gate defers
                        // exactly those calls instead of the whole file.
                        // Before the head guard below — the names need no
                        // head, and a head-less (parser-degraded) augmentation
                        // must not silently skip the gate.
                        self.collect_augmentation_extension_names(defn);
                    }
                    if is_type_augmentation(defn)
                        && let Some(li) = defn.long_id()
                    {
                        let segs: Vec<SyntaxToken> = li.idents().collect();
                        // An augmentation head (`type T with …`) resolves in-file
                        // only: an in-file `T` resolves via `lookup_type_def`
                        // (arity-agnostic). It must *not* fall through to the
                        // arity-keyed assembly lookup — the augmented type's typars
                        // aren't on this `long_id`, so a generic assembly target
                        // (`type Demo.Pair<'T> with …`) would mis-key to arity 0 and
                        // resolve a wrong same-named entity (D5).
                        self.resolve_in_file_type_path(&segs);
                        // The augmentation's members join the in-file member
                        // index — visible only from here on (FCS FS0039 before
                        // the augmentation, probe M4a) — or, when the head is
                        // not a type of this container, suppress member
                        // emission for the name file-wide.
                        self.index_augmentation_members(defn);
                    }
                    // The type header's generic parameters (`type Foo<'T>`) are in
                    // scope throughout its body: the abbreviation/record/union RHS
                    // resolved by `resolve_type_defn`, and every member signature
                    // and body reached by `resolve_type_member_bodies`. (An
                    // augmentation carries no `<…>` on its head, so this pushes
                    // nothing there.)
                    let pushed_typars = self.enter_typars(defn.typar_decls());
                    self.resolve_type_defn(defn);
                    // Descend into the type's member bodies (self-id, params,
                    // ctor params, class fields) — the value-resolution slice
                    // that `resolve_type_defn` (type uses only) does not cover.
                    self.resolve_type_member_bodies(defn);
                    self.leave_typars(pushed_typars);
                }
            }
            ModuleDecl::Exception(exn) => {
                // An exception definition (parser phase 9.15) introduces a
                // project-level name `E` — both an exception *type* and a
                // value-namespace *constructor*. Two things happen:
                //
                // 1. The *name* is recorded in the shadow sets so a reference
                //    rooted at it (`E.Member`) defers rather than falling through
                //    to a colliding referenced-assembly member (the same
                //    `assembly_path_records` soundness tripwire the type and
                //    nested-module arms guard).
                //
                // 2. The constructor is interned as a value binder and added to
                //    the current container's value frame at its source position
                //    ([`Self::define_exception_case`]), so an unqualified use
                //    resolves: `E "x"` / `raise (E x)` (an expression
                //    constructor, via `lookup`) and `try … with E m -> …` (a
                //    pattern head, via `case_reference`). This is the single-
                //    constructor analogue of [`Self::define_union_cases`]; an
                //    exception is never `[<RequireQualifiedAccess>]`, so the
                //    constructor is always added. The abbreviation form
                //    (`exception Alias = E`) likewise interns its new name
                //    `Alias` from the same `union_case` slot.
                //
                // The exception *type* (in the disjoint type namespace) is not
                // interned yet; the payload-field types are uses left for later
                // slices. The abbreviation *target* (`= E`) is resolved as an
                // ordinary value-name use (see below) — *before* the alias is
                // bound, so the alias never shadows its own target.
                if let Some(name) = exn.union_case().and_then(|c| c.ident()) {
                    self.record_project_name_shadow(vec![id_text(name.text()).to_string()]);
                    // The export-decl-list twin of the tycon-side name shadow (the
                    // value-namespace ctor is a separate `Item` decl from
                    // `define_exception_case` → `export_case` below).
                    {
                        let mut path = self.container_path.clone();
                        path.push(id_text(name.text()).to_string());
                        self.push_export_decl(
                            path,
                            name.text_range().start(),
                            ExportDeclKind::ExceptionTycon,
                        );
                    }
                    // An exception abbreviation target (`exception Alias = E`) is
                    // resolved through the ordinary **value namespace with
                    // latest-wins shadowing** — not a type/exception-only lookup.
                    // Proof (F# compiler): `exception E of string; let E = 0;
                    // exception Alias = E` reports FS0921 "Not an exception",
                    // which only fires if the *value* `E` shadowed the earlier
                    // exception during name resolution (a type-namespace lookup
                    // would have found the exception and not errored). So a
                    // single-segment target is just a value-name use: it points
                    // at whatever the name resolves to — the in-file exception
                    // when none shadows it, or a later shadowing value (F# then
                    // type-errors, a Phase-4 concern, but the *name* still
                    // resolves to the value). A multi-segment / assembly target
                    // (`exception E = System.Exception`) is filtered out by
                    // `single_ident` and left deferred until assembly resolution
                    // covers it. FCS's symbol-use dump does not emit the target
                    // as a use, so this is a navigation bonus over the
                    // differential oracle (never checked by the corpus); direct
                    // tests pin it.
                    if let Some(target) = exn.abbrev_path().and_then(single_ident) {
                        self.resolve_name_use(&target);
                    }
                    self.define_exception_case(&name, header_is_private(exn.syntax()));
                }
            }
            ModuleDecl::Let(let_decl) => self.module_let(let_decl),
            // A `#`-directive (`#I "/tmp"`, `#load …`) is a compiler directive: it
            // introduces no names and binds nothing, so name resolution ignores it.
            ModuleDecl::HashDirective(_) => {}
            ModuleDecl::Extern(ext) => {
                // An `extern` DllImport prototype (parser's extern slice, FCS's
                // `cPrototype`) introduces a module-level *value* name — the
                // function. Record it in the shadow sets so a reference rooted at
                // it (`ExternFn.Member`) defers rather than falling through to a
                // colliding referenced-assembly member (the same
                // `assembly_path_records` soundness tripwire the `let` / exception /
                // nested-module arms guard). Interning the binder as a *usable*
                // value — so an unqualified `externFn 0` resolves the call — is a
                // later slice; until then such a use falls through to the
                // conservative (unresolved) path, which is sound.
                let ext_name: Vec<String> = ext
                    .name()
                    .map(|name| {
                        name.idents()
                            .map(|t| id_text(t.text()).to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                if !ext_name.is_empty() {
                    self.record_project_name_shadow(ext_name.clone());
                }
                // An `extern` introduces a value-namespace name we do NOT intern
                // (interning it is a later slice), so it is invisible to the
                // per-name provenance the namespace-straddle fold reads. Mark the
                // container hidden so a straddle whose fold reaches an extern-bearing
                // auto-open submodule DEFERS rather than trusting an understated
                // `value_slot` and committing a wrong direct-tier winner (codex
                // review of the straddle slice). A sound over-defer: `extern` is
                // rare (P/Invoke), and every other unenumerable value producer
                // (union cases / exception ctors / active patterns / aliases)
                // already marks its module hidden.
                self.note_hidden_value_module(self.container_path.clone());
                // The export-decl-list twin: `path` = the container (its
                // hidden-value path, recorded unconditionally as above); `name`
                // carries the function segments (empty for a nameless recovery
                // node), and `path + name` is the nested-module shadow, derived
                // only when `name` is non-empty.
                self.push_export_decl(
                    self.container_path.clone(),
                    ext.syntax().text_range().start(),
                    ExportDeclKind::Extern { name: ext_name },
                );
            }
            ModuleDecl::Open(open) => {
                // Resolution-explain trace (see `ResolutionTrace`): snapshot the
                // three opaque-open flags *before* this open, and capture its
                // range / path / kind, so the record pushed at the end of this
                // arm names which flags this open flipped false→true. The flags
                // are write-only within this arm and monotone within a block, so
                // a `now && !before` diff is exactly this open's contribution.
                let trace_range = open.syntax().text_range();
                let trace_is_type = open.is_type();
                let trace_path: Vec<String> = if trace_is_type {
                    open.ty()
                        .and_then(|t| type_long_ident_path(&t))
                        .unwrap_or_default()
                } else if let Some(li) = open.long_ident() {
                    // Drop a leading `global` root qualifier (matching the raw
                    // keyword, not an escaped ``global`` ident) so the traced
                    // path is the namespace/module actually opened — the same
                    // normalisation the plain-open branch below applies.
                    let idents: Vec<SyntaxToken> = li.idents().collect();
                    let rooted = idents.first().is_some_and(|t| t.text() == "global");
                    idents
                        .iter()
                        .skip(usize::from(rooted))
                        .map(|t| id_text(t.text()).to_string())
                        .collect()
                } else {
                    Vec::new()
                };
                let trace_before = (
                    self.opaque_value_open,
                    self.opaque_dotted_open,
                    self.unmodelled_open_active,
                );
                // The generation is a fourth deferral mechanism: an open that
                // raises the barrier stales earlier entries, deferring a later
                // head through one even when the three flags stay false. Snapshot
                // it too. Monotone within the arm (only ever bumped), so a strict
                // increase is this open's own barrier.
                let trace_before_gen = self.open_generation;
                // A fifth: a fully-enumerated open can import a name that is
                // ITSELF `Deferred` (a cross-assembly duplicate, say), setting no
                // flag and bumping no generation — yet it is the source of that
                // deferred name. Opened entries all land in `module_frame()`
                // (`scopes.last()`), and this arm pushes only its own (it never
                // enters a nested module), so a new `Deferred` entry after it is
                // this open's import.
                let trace_before_entries = self.scopes.last().map_or(0, |f| f.entries.len());
                // A sixth: a namespace open contributes a **reading** / shortening
                // prefix — a qualified-path precedence entry — while setting no flag
                // and raising no barrier. It usually *resolves* names, but it can
                // re-own a later dotted head against a lower open's reading (`open
                // Low; open High; M.Mangled`), deferring when the higher reading owns
                // the path with an uncertain member. Both `imports` (the `OpenGroup`)
                // and `open_shortening_prefixes` grow only within this arm (it never
                // enters a nested module), so a strict length increase in either is
                // this open's own reading contribution.
                let trace_before_imports = self.imports.len();
                let trace_before_shortening = self.open_shortening_prefixes.len();
                // Classify the open. `open <namespace>` brings the namespace's
                // *types* into scope — record the prefix so later qualified
                // references retry under it (modelled), no unqualified values.
                //
                // `open type T` brings the type's *members* into scope. Modelled
                // when its target resolves to a public assembly type that is not
                // project-shadowed: one *opened* value entry per distinct public
                // static name is pushed into the current frame
                // ([`Self::open_type_statics`]) so a bare name resolves against
                // them through the ordinary latest-wins [`Self::lookup`], and
                // `unmodelled_open_active` is set so a *qualified* path through its
                // (unmodelled) nested types still defers. Otherwise (an in-project /
                // project-shadowed / exotic target whose statics we cannot
                // enumerate) it sets `opaque_value_open` so bare-name resolution
                // stays conservative.
                //
                // A plain `open M` of an *in-project module* brings M's direct
                // `let` values into unqualified scope: one *opened* entry per value
                // is pushed into the current frame ([`Self::open_module_values`]) so
                // a bare name resolves through the latest-wins `lookup`, and
                // `opaque_dotted_open` is set so a *dotted* head through M's
                // (unmodelled) submodules/types stays conservative.
                //
                // A *plain* `open <assembly module/class>` does **not** import a
                // class's statics in F# (only `open type` does), and an *assembly*
                // module's values are not modelled here, so it sets
                // `opaque_value_open` (plus `unmodelled_open_active` for qualified
                // paths) rather than resolving anything from it.
                // EX-3 §2(d): every `open` of any kind bumps the latest-open
                // position — F# is latest-wins across bindings and opens alike,
                // so an in-file attribute-type commit whose definition precedes
                // any open must defer (the open could supply the candidate at
                // higher priority; see `attribute_candidate`). Monotone: never
                // restored on nested-module exit, which only over-defers.
                self.latest_open_pos = self
                    .latest_open_pos
                    .max(open.syntax().text_range().start().into());
                if open.is_type() {
                    // The opened type is the `ty()` child (a `Type`), not
                    // `long_ident()`. (Our parser does not accept a `global`-
                    // qualified `open type global.Demo.Calc`, so unlike the plain-
                    // open branch below there is no leading `global` to strip.)
                    // `opened_type_target` resolves it through the active opens.
                    match open
                        .ty()
                        .and_then(|t| type_long_ident_path(&t))
                        .and_then(|path| self.opened_type_target(&path))
                        // An abbreviation *marker*: FCS opens the
                        // abbreviation's TARGET type's statics (`open type
                        // Lib.S` where `type S = System.String` opens
                        // `String`'s statics), so a chase-able marker opens
                        // its terminal. An unchaseable one still cannot be
                        // enumerated — pushing the marker's (empty) statics
                        // would let an earlier open's same-named value win
                        // where FCS binds a target static, so it routes to
                        // the opaque branch (codex review on the marker PR).
                        .and_then(|h| {
                            if self.assemblies.is_abbreviation(h) {
                                // The reference-order collision guard: when
                                // two loaded DLLs export the alias's rooting
                                // FQN, FCS picks by reference order — which
                                // sema does not model — so opening the
                                // first-indexed pick's target statics could
                                // import the wrong DLL's surface. Opaque
                                // instead (codex round 3).
                                if self.assemblies.alias_rooting_collides_across_dlls(h) {
                                    return None;
                                }
                                self.assemblies.resolve_abbreviation_target(h)
                            } else {
                                Some(h)
                            }
                        }) {
                        // The target resolves to a public assembly type: push one
                        // source-ordered *opened* entry per distinct public static
                        // name, so a bare name resolves against the type's statics
                        // through the ordinary latest-wins `lookup`.
                        Some(handle) => {
                            let pos = u32::from(open.syntax().text_range().start());
                            self.open_type_statics(handle, pos, true)
                        }
                        // An in-project / project-shadowed / exotic target whose
                        // statics we cannot enumerate (or an abbreviation marker,
                        // filtered above): nothing is modelled, so bare-name
                        // resolution stays conservative.
                        None => {
                            self.opaque_value_open = true;
                            self.open_generation += 1;
                        }
                    }
                    self.unmodelled_open_active = true;
                    // EX-2: `open type T` brings T's static content into unqualified
                    // scope, not an instance/static extension into any *method group*
                    // — but a name-keyed claim about that surface would have to
                    // enumerate the opened type's members, which we do not do here.
                    // Defer wholesale; the coverage cost is a rare `open type`.
                    self.open_extension_unknowable = true;
                } else if let Some(li) = open.long_ident() {
                    let idents: Vec<SyntaxToken> = li.idents().collect();
                    let mut path: Vec<String> = idents
                        .iter()
                        .map(|t| id_text(t.text()).to_string())
                        .collect();
                    // `open global.Demo` ≡ `open Demo` *from the root*: `global` is
                    // F#'s root-namespace qualifier, surfaced by the parser as a
                    // leading ident. Strip it (matching the raw keyword, not an
                    // escaped ``global`` identifier). `open global` alone strips to
                    // empty — the root namespace is already the direct case. A
                    // `global`-qualified path is *fully rooted*: it must not be
                    // shortened through the enclosing namespace or a prior open, so
                    // `open global.Root` in `namespace N` opens the root `Root`, not
                    // `N.Root`.
                    let rooted = idents.first().is_some_and(|t| t.text() == "global");
                    if rooted {
                        path.remove(0);
                    }
                    // `open <path>` opens **every** entity its path names — these are
                    // not mutually exclusive (FCS): project modules and namespace
                    // readings (relative *and* as-written root, project and/or
                    // assembly) and/or an assembly type. One walk
                    // ([`Self::open_interpretations`]) resolves them all into a
                    // single **proximity-ordered** list (highest priority first);
                    // they are applied lowest-priority-first below, so a
                    // higher-priority one wins via latest-wins `lookup` /
                    // `case_reference` / shortening — precedence is the path's
                    // relativeness, never the module-vs-reading category.
                    let interps = if path.is_empty() {
                        Vec::new()
                    } else {
                        self.open_interpretations(&path, rooted)
                    };
                    // **Veto through an incomplete prefix** (review round 9). An earlier
                    // `open Parent` whose surface is not provably complete may have
                    // brought in a nested module `Parent.Sub` that projection *dropped* —
                    // invisible to us, but bound by FCS at a higher priority than any
                    // root `Sub`. So if this open names nothing under such a prefix, the
                    // lower-tier interpretations we *can* see must not be applied as
                    // definite targets: go opaque instead (defer, never a wrong target).
                    // A `global.`-rooted open cannot be shortened through *any* prefix
                    // (tier 3 only), so an incomplete prefix cannot hide a higher-priority
                    // reading of it — the veto must not fire, or opening an incomplete
                    // module would make every later rooted open opaque (round 10).
                    //
                    // Otherwise the veto fires on the mere *presence* of a prefix that
                    // could hide a nested module: seeing a namespace or a project module
                    // at `prefix.path` does **not** disprove a hidden assembly module
                    // there, because FCS merges same-path entities across assemblies and
                    // would bind the hidden one's values at this higher priority (round
                    // 10). We cannot prove absence, so we decline to name a target.
                    let vetoed_by_incomplete_prefix =
                        !rooted && !path.is_empty() && !self.incomplete_open_prefixes.is_empty();
                    if vetoed_by_incomplete_prefix {
                        self.opaque_value_open = true;
                        self.open_generation += 1;
                        self.unmodelled_open_active = true;
                    }
                    // **A dropped TypeDef can BE the module this open names**
                    // (codex round 23). When projection dropped the only entity
                    // at a tier candidate's path, the open resolves to NO
                    // interpretation there — no group exists to carry the
                    // conservatism — yet FCS opens the real module, whose
                    // exports may shadow any earlier name. Scan the SAME tier
                    // candidates the interpretation walk scans (one enumeration,
                    // round 16's lesson); a candidate that carries a dropped
                    // split AND no interpretation goes blanket-opaque, exactly
                    // like the incomplete-prefix veto. Candidates that did
                    // yield an interpretation are handled per-group below.
                    let names_uncovered_dropped_path = !path.is_empty()
                        && self
                            .open_tier_candidates(&path, rooted)
                            .iter()
                            .any(|(full, _)| {
                                self.assemblies
                                    .any_split_of_a_module_path_has_a_dropped_type(full)
                                    && !interps.iter().any(|i| match i {
                                        OpenInterpretation::Module(p)
                                        | OpenInterpretation::AssemblyModule(p)
                                        | OpenInterpretation::Reading(p) => p == full,
                                    })
                            });
                    if names_uncovered_dropped_path {
                        self.opaque_value_open = true;
                        self.open_generation += 1;
                        self.unmodelled_open_active = true;
                    }
                    let project_resolved = interps.iter().any(|i| match i {
                        OpenInterpretation::Module(_) => true,
                        OpenInterpretation::AssemblyModule(_) => false,
                        OpenInterpretation::Reading(r) => self.is_project_namespace_path(r),
                    });
                    // Whether an **enumerable** project module was opened — its
                    // direct values enter the frame below, so the blunt opaque
                    // fallback is not needed. A resolved *namespace* does **not**
                    // count: a namespace enumerates no values, so an unenumerable
                    // module sharing the same path must still shadow earlier opens.
                    // **Project** modules only. An assembly module at the same path
                    // enumerates *its* values, but says nothing about an unenumerable
                    // project module of that name (an anonymous-root `module X` in a
                    // headerless file, which `is_project_module_path` cannot see): FCS
                    // binds the local `X.Foo`, so the project-opaque fallback must still
                    // fire and defer. Counting the assembly module here suppressed it
                    // and resolved `Foo` into the referenced assembly — a wrong target
                    // (review, Slice A round 4).
                    let module_opened = interps
                        .iter()
                        .any(|i| matches!(i, OpenInterpretation::Module(_)));

                    // Which readings apply — all of which feed
                    // [`Resolver::imports`] (the assembly precedence walk —
                    // [`Self::resolve_assembly_path_tiered`] tries the relative
                    // before the root within this open and prefers this whole open
                    // over earlier ones, latest-open-wins):
                    //
                    // * the raw path is a project **module** — it shadows the
                    //   as-written assembly interpretation entirely, and
                    //   assembly-only readings are suppressed with it; the
                    //   *project*-namespace readings still apply (FCS opens both).
                    // * the raw path is an **assembly type** (an unmodelled open) —
                    //   its statics own bare-name space conservatively and the
                    //   assembly-namespace readings are suppressed with it.
                    // * otherwise — a namespace open: every reading applies.
                    //
                    // In the first two cases the surviving *project*-namespace
                    // readings bind no assembly path themselves, but a name they
                    // shadow must veto a lower-tier assembly binding
                    // (`ProjectShadowed` → defer) at its true priority — so the
                    // (filtered) group always enters `imports`. (For a project
                    // module that veto is currently redundant — the module
                    // interpretation sets `opaque_dotted_open` over the same
                    // scope, deferring every dotted head — but the walker's view
                    // must stay complete for the day that blanket is refined;
                    // `project_module_open_does_not_leak_a_root_assembly_type`
                    // pins the soundness either way.)
                    let raw_project_module = !path.is_empty() && self.is_project_module_path(&path);
                    let mut unmodelled = false;
                    if !path.is_empty()
                        && !raw_project_module
                        && let Some(handle) = self.opened_assembly_type(&path)
                    {
                        // An unenumerable assembly module/type. A bare name might
                        // be one of its names we cannot model, so it must shadow
                        // earlier opens (else a stale earlier `open`'s value
                        // resolves — wrong). When the open ALSO resolved a project
                        // module/namespace, shadow earlier opens with the precise
                        // **generation barrier** so the open's *own* modelled names
                        // (project cases/values, at the new generation) still
                        // resolve — `opaque_value_open` would blanket-suppress
                        // them. With no project interpretation it is the simpler
                        // equivalent (no modelled names to preserve).
                        //
                        // An abbreviation *marker* is opaque like a module: a
                        // suffixed module companion (`type Foo = string` +
                        // `module Foo`, compiled `FooModule`) loses its
                        // source-name index slot to the marker, so the handle
                        // being a marker may mean FCS opens the companion
                        // module's (unmodelled) values here (codex review,
                        // round 2, on the marker PR).
                        // An **enumerable assembly module** at this path is not this
                        // branch's business: it has its own tiered interpretation
                        // (`OpenInterpretation::AssemblyModule`, applied below), which
                        // imports its values at their true priority. A plain `open`
                        // opens that module — **never** the bare type (that needs
                        // `open type`) — so a same-named *type* companion at the path
                        // (`type Tagged` beside the suffixed `module Tagged`, compiled
                        // `TaggedModule`) must not make the open unmodelled: it is the
                        // module that opens, and its values are modelled.
                        //
                        // The predicate is therefore "an enumerable module exists
                        // here" — the exact condition `open_interpretations` uses to
                        // emit the `AssemblyModule` tier — **not** "the type-preferring
                        // `opened_assembly_type` handle *is* that module". The latter
                        // reads the companion *type* (the first-wins `by_type` index
                        // returns it over the source-named module), so at a collision
                        // it wrongly deemed the open non-enumerable, set
                        // `unmodelled_open_active`, and suppressed every later
                        // *relative* reading — including the implicit
                        // `Microsoft.FSharp.Collections` that makes a bare `Seq.toList`
                        // resolve. When the module is genuinely unenumerable (its
                        // source-name slot lost to an abbreviation marker, or its
                        // contents dropped) `opened_assembly_module` is `None`, so the
                        // conservative path below still fires.
                        // A `[<RequireQualifiedAccess>]` module imports nothing at all
                        // (FCS errors, FS0892), so it needs no conservatism either:
                        // nothing came in to shadow with.
                        let enumerable = self.opened_assembly_module(&path).is_some();
                        if !enumerable {
                            if self.assemblies.is_module(handle)
                                || self.assemblies.is_abbreviation(handle)
                            {
                                if project_resolved {
                                    self.open_generation += 1;
                                } else {
                                    self.opaque_value_open = true;
                                    self.open_generation += 1;
                                }
                            }
                            self.unmodelled_open_active = true;
                            unmodelled = true;
                        }
                    }
                    let project_readings_only = raw_project_module || unmodelled;
                    let readings: Vec<Vec<String>> = interps
                        .iter()
                        .filter_map(|i| match i {
                            OpenInterpretation::Reading(r)
                                if !project_readings_only || self.is_project_namespace_path(r) =>
                            {
                                Some(r.clone())
                            }
                            _ => None,
                        })
                        .collect();
                    if !readings.is_empty() {
                        self.imports.push(OpenGroup { readings });
                    }

                    // **Project-opaque** fallback: an anonymous-root project module
                    // (a path *under* a project module whose values we cannot
                    // enumerate) — only when no *enumerable* module was opened above
                    // (`open M` that resolved a relative module already enumerated it).
                    // Gated on `module_opened`, **not** `project_resolved`: when the
                    // open also resolved a namespace at the same path, that namespace
                    // brings no values, so the unenumerable module's values still must
                    // shadow earlier opens (FCS opens both; else a stale earlier
                    // `open`'s value resolves — wrong; deferring is correct).
                    if !path.is_empty() && !module_opened && self.open_imports_project_values(&path)
                    {
                        self.opaque_value_open = true;
                        self.open_generation += 1;
                    }

                    // EX-2 (`docs/extension-scope-enumeration-plan.md`): classify this
                    // open's *extension* surface for the overload-absence gate, keyed
                    // by name only when we can enumerate that surface. The gate treats
                    // an opened assembly namespace exactly as it treats an enclosing
                    // one — the same `namespace_extension_names` query — so an open
                    // that resolves **entirely** to assembly-namespace readings, with
                    // no residual uncertainty, contributes its readings by name. Every
                    // other target defers wholesale: a project module/namespace (its
                    // extension content is EX-3, and is anyway caught by the file's
                    // preceding/own project-source triggers), an assembly module
                    // (`OpenInterpretation::AssemblyModule` — a plain `open` of one, or
                    // the plain-open-of-a-class `unmodelled` case), or an open whose
                    // path is uncertain (an incomplete prefix or a dropped-type split
                    // could hide an `[<Extension>]` of any name; an unresolved path
                    // might be a namespace we simply failed to model). `open global`
                    // (empty path) opens the always-in-scope root — a no-op here.
                    //
                    // A **dropped type at any split of an opened reading** is unknowable
                    // even when a namespace reading survives at the exact path (codex
                    // P1): `open A.B.C` with a dropped TypeDef at `A.B` may be opening a
                    // same-FQN *module* `A.B.C` FCS merges into scope, whose extensions
                    // are invisible — and `extension_named_in_scope` queries only the
                    // exact opened namespace (`A.B.C`), where the marker (recorded under
                    // its enclosing namespace `A.B`) does not sit. The value-side fold
                    // guards the same hazard with the same `any_split_…` check.
                    let clean_assembly_namespace_open = !path.is_empty()
                        && !interps.is_empty()
                        && !vetoed_by_incomplete_prefix
                        && !names_uncovered_dropped_path
                        && !unmodelled
                        && !self.open_imports_project_values(&path)
                        && interps.iter().all(|i| {
                            matches!(i, OpenInterpretation::Reading(r)
                                if !self.is_project_namespace_path(r)
                                    && !self.assemblies.any_split_of_a_module_path_has_a_dropped_type(r))
                        });
                    if clean_assembly_namespace_open {
                        for interp in &interps {
                            if let OpenInterpretation::Reading(r) = interp {
                                self.open_extension_namespaces.push(r.clone());
                            }
                        }
                    } else if !path.is_empty() {
                        self.open_extension_unknowable = true;
                    }

                    // Apply the interpretations **lowest priority first** (the list
                    // is highest-first): the shortening prefixes, the opened names,
                    // and `lookup` / `case_reference` are all consumed latest-wins,
                    // so a more-proximate interpretation's names out-rank a
                    // less-proximate one's — whichever kind each is (FCS: a tier-1
                    // namespace reading beats a tier-2 module; a relative reading's
                    // assembly auto-open value beats a root project case).
                    //
                    // They are applied as **groups, one per path**: FCS's
                    // environment maps a head to the LIST of every same-named
                    // module/namespace (all tiers, all assemblies) and an `open`
                    // folds them ALL (`AddModuleOrNamespaceRefsContentsToNameEnv`,
                    // a foldBack — the most proximate folds last and wins), so the
                    // halves sharing one path — project module, assembly module(s),
                    // namespace reading — form ONE fold unit. The group's
                    // barrier/demotion decisions span exactly its halves (rounds
                    // 15-17), and a group with name-unknown residue bumps the
                    // generation AS IT IS APPLIED, staling every entry folded
                    // before it — a lower-priority group's entries included. That
                    // per-group bump is what closed round 19: the old single
                    // pre-loop bump stamped a lower-tier project module's values
                    // with the barrier's own generation, so a higher hidden
                    // assembly module never shadowed them.
                    let mut group_paths: Vec<Vec<String>> = Vec::new();
                    for interp in &interps {
                        let p = match interp {
                            OpenInterpretation::Module(p)
                            | OpenInterpretation::AssemblyModule(p)
                            | OpenInterpretation::Reading(p) => p,
                        };
                        if !group_paths.iter().any(|g| g == p) {
                            group_paths.push(p.clone());
                        }
                    }
                    let pos: u32 = li.syntax().text_range().start().into();
                    for gp in group_paths.iter().rev() {
                        let has_project_module = interps
                            .iter()
                            .any(|i| matches!(i, OpenInterpretation::Module(p) if p == gp));
                        let has_assembly_module = interps
                            .iter()
                            .any(|i| matches!(i, OpenInterpretation::AssemblyModule(p) if p == gp));
                        let has_reading = interps
                            .iter()
                            .any(|i| matches!(i, OpenInterpretation::Reading(p) if p == gp));

                        let project_ns = self.is_project_namespace_path(gp);

                        // The assembly-module half's complete-or-opaque surfaces —
                        // one per referenced assembly exposing the FQN (FCS merges
                        // them, review round 5) — and the group's residue verdict,
                        // decided BEFORE any half is applied so the barrier
                        // outranks earlier groups and earlier opens only, never a
                        // half of its own group (round 15).
                        let handles = if has_assembly_module {
                            self.opened_assembly_modules(gp)
                        } else {
                            Vec::new()
                        };
                        let mut surfaces: Vec<OpenFoldSurface> = handles
                            .iter()
                            .map(|&h| self.assemblies.open_fold_surface(h))
                            .collect();
                        // The **assembly namespace half** joins the fold as one more
                        // surface (`docs/assembly-module-open-plan.md`, "the namespace
                        // half joins the fold"): its direct tycon tier (exceptions,
                        // non-RQA union cases) and its `[<AutoOpen>]` submodules'
                        // contents. Gated exactly as the old `open_auto_open_modules_in`
                        // was (`!(project_readings_only && !project_ns)`) — a pure
                        // assembly-namespace reading suppressed by a project-module /
                        // unmodelled-type open must not contribute. Folding it here is
                        // what lets the cross-kind demote below drop its `has_namespace`
                        // arm: a name the namespace half supplies now collides per-name
                        // with the module half inside the fold writer, and its
                        // name-unknown residue feeds the group verdict, instead of the
                        // whole module half deferring wholesale.
                        let assembly_ns_applies = has_reading
                            && self.assemblies.has_namespace(gp)
                            && !(project_readings_only && !project_ns);
                        if assembly_ns_applies {
                            surfaces.extend(self.assemblies.open_namespace_fold_surfaces(gp));
                        }
                        // The **project namespace half**'s own constructible type names
                        // join the fold as a contestant-only surface (codex review of
                        // §7's machinery slice, `docs/assembly-module-open-plan.md`): a
                        // project type at this FQN takes FCS's unqualified constructor
                        // slot exactly like an assembly namespace's constructible types
                        // do, so it can evict a same-named *value* from a DIFFERENT
                        // surface (a colocated assembly module) — `collisions()` in
                        // `open_assembly_module_fold` demotes the colliding name once
                        // it sees both. Not entries (sema does not model project type
                        // members), so the contested name itself still defers — sound,
                        // just unavailable, like the assembly-side analogue.
                        if project_ns {
                            let contestants = self.project_namespace_contestant_names(gp);
                            if !contestants.is_empty() {
                                surfaces.push(OpenFoldSurface {
                                    contestant_names: contestants,
                                    ..Default::default()
                                });
                            }
                        }
                        // Name-unknown residue of the fold group — full tier: a
                        // surface that cannot list all its names, or a dropped
                        // type at ANY split of the path, which may be a same-FQN
                        // module half we cannot see at all (round 16 — the check
                        // must span the same splits as the lookup).
                        // Ungated on the module half existing: a dropped type at
                        // a split can be a same-FQN module half of a READING-only
                        // or project-only group too (codex round 23) — its hidden
                        // contents shadow earlier opens and contest the group's
                        // assembly-side names either way.
                        let path_dropped = self
                            .assemblies
                            .any_split_of_a_module_path_has_a_dropped_type(gp);
                        // The **project namespace half**'s own name-unknown residue —
                        // an `[<AutoOpen>]` type (or any other construct
                        // `open_project_namespace_values` cannot enumerate the names
                        // of) directly in `gp` or one of its `[<AutoOpen>]`
                        // submodules (codex review round 5, fcs-dump-verified: sema
                        // has no project-side `open_type_statics` equivalent, so
                        // such a type's statics are invisible to every enumeration
                        // this fold does). Folds into `full_residue` exactly like an
                        // assembly surface's own residue does — a colliding
                        // assembly value must defer, not stay wrongly definite,
                        // when the project half might supply a name we cannot see.
                        let project_ns_hidden =
                            project_ns && self.namespace_fold_has_hidden_values(gp);
                        let full_residue =
                            surfaces.iter().any(|s| s.residue) || path_dropped || project_ns_hidden;
                        // Tycon-tier-confined residue (a case-nameless union):
                        // hidden names that FCS folds *before* the vals. They
                        // shadow earlier opens (barrier) and contest the group's
                        // own case entries, but never its vals (round 10) — in
                        // ONE surface. Across a merge (module half + namespace
                        // half, or two assemblies) the tiers interleave in
                        // reference order, so with more than one surface it
                        // escalates to the full demote.
                        let below_vals = surfaces.iter().any(|s| s.residue_below_vals);
                        // "The group hides names" — what the dotted-head blanket keys on.
                        let module_half_hides_names = full_residue || below_vals;
                        // A cross-kind path where the FQN is ALSO a **project**
                        // namespace needs no blanket demote here (§7's "machinery"
                        // slice): unlike two assemblies (unknowable reference order,
                        // `collisions()`'s reason to defer), the project half's fold
                        // position relative to every assembly half is FIXED — it is
                        // pushed strictly after this group's assembly fold, below
                        // (`open_project_namespace_values`, Q14: the project's own
                        // fragment always folds last) — so a name it supplies simply
                        // out-ranks the module half's by **position**, whether or not
                        // the module half's own entry stays definite. Demoting the
                        // module half's entry to `Deferred` would not even help: the
                        // project push shadows it either way. What the project half
                        // still needs is the generation **barrier** when it may bring
                        // names it cannot enumerate (`namespace_fold_has_hidden_values`,
                        // symmetric with the project-*module*-half bump below) — that
                        // is a property of the project half alone, not a reason to
                        // demote the assembly module half's own unique names.
                        let demote_module_half = full_residue || (below_vals && surfaces.len() > 1);
                        // Restore the barrier the deleted `has_namespace` arm gave a
                        // **cross-kind** open (codex round 4). A namespace half's
                        // constructor-slot **type** name enters FCS's unqualified slot
                        // and **evicts** a same-named value from an EARLIER open — even
                        // when nothing in this group supplies that name, so no collision
                        // entry is emitted for it. The type is not a fold entry (it
                        // takes its slot via the eviction/type channel, which also
                        // serves qualified `Type.Member`), so the only lever left is the
                        // generation barrier. Coarser than FCS — it stales every earlier
                        // opened value AND local, not just the colliding name — which is
                        // sound for bare names (they defer) and needs the qualified
                        // channels' per-head `head_entry_staled` veto to be sound for
                        // compound ones (codex round 10). Strictly narrower than the
                        // blanket cross-kind demote
                        // it replaces (which bumped for *any* namespace half). Gated on the
                        // **module half**: a pure namespace open needs no barrier — its
                        // own entries shadow by position, and the head-slot eviction
                        // machinery already handles a local value vs a namespace type,
                        // so bumping there would stale that local and mis-resolve the
                        // eviction probes.
                        let cross_kind_ns_type = has_assembly_module
                            && surfaces.iter().any(|s| !s.contestant_names.is_empty());
                        // The barrier: ANY unseen or unordered name in the group, or a
                        // cross-kind namespace type that evicts an earlier value, must
                        // shadow everything folded before this group.
                        //
                        // A risen barrier stales every earlier name — an earlier open's
                        // value AND a local binding. A *dotted head* through such a
                        // staled entry (`X.Zero` after `let X = …`) must then DEFER,
                        // not fall through to a qualified path an earlier open can
                        // still see (a referenced assembly's `X.Zero`): FCS binds the
                        // local the bump staled. Every barrier arm gets that for free
                        // from the per-head `head_entry_staled` veto in the qualified
                        // channels (codex round 10 — the cross-kind-type arm used to
                        // bump without any dotted guard and rerouted an unrelated
                        // local's dotted head to the assembly).
                        if (!surfaces.is_empty() || path_dropped)
                            && (demote_module_half || below_vals || cross_kind_ns_type)
                        {
                            self.open_generation += 1;
                        }
                        // The project-module half's OWN hidden-values barrier
                        // (Q14's "may bring names we cannot enumerate" bump).
                        // A same-file RESOLVABLE alias is canonicalised to its
                        // target at tier 0, and THAT group carries the open's
                        // real semantics — values and hidden-value conservatism
                        // alike. The alias's own path exports no values and its
                        // blanket hidden marker exists for *later files* (which
                        // cannot follow the alias), so consulting it here would
                        // double-bump and stale names FCS resolves (the marker's
                        // declaration site documents exactly this split).
                        //
                        // The bump's position splits on marker provenance
                        // (`docs/fsi-signature-restriction-plan.md`, the
                        // open-fold slice). When every hidden marker for `gp`
                        // is **sig-screened** — no marker from this file's own
                        // decls, none from an unscreened earlier file — the
                        // names the barrier fears are bounded by the signature
                        // text, and THE FOLD's per-name screen demotion below
                        // (`sig_screened_open_name`) already defers exactly
                        // those among this open's own assembly entries. Bump
                        // BEFORE the fold, so those entries carry the fresh
                        // generation and the ones the signature provably cannot
                        // expose fall through to the assembly as FCS does
                        // (probed: a sig-`private` or sig-hidden name after
                        // `open` of the signatured module binds the colliding
                        // assembly member, diagnostics-clean). An opaque marker
                        // (an active pattern, an alias, … in an unscreened
                        // fragment) keeps the bump after the fold: the hidden
                        // name could shadow this open's own assembly entries
                        // and no screen demotes it per-name.
                        let resolved_alias = self.module_aliases.contains_key(gp.as_slice());
                        let project_module_bump = has_project_module
                            && !resolved_alias
                            && self.module_has_hidden_values(gp);
                        let bump_covered_by_screen = project_module_bump
                            && !self.modules_with_hidden_values.contains(gp.as_slice())
                            && !self.preceding.opaque_hidden_value_module(gp);
                        if bump_covered_by_screen {
                            self.open_generation += 1;
                        }
                        // A group that hides names we cannot LIST needs more than the
                        // per-head staleness veto: the hidden name could itself be a
                        // dotted HEAD with no earlier entry to go stale (`X.Red` where
                        // the residue conceals a value `X` — codex round 7), which no
                        // per-head test can see. Dotted heads stay blanket-vetoed for
                        // exactly the hidden-name arms. The KNOWN-names arm alone
                        // (`cross_kind_ns_type` — every name it contests is an entry
                        // or a contestant) hides nothing, so the group's own names —
                        // its namespace half's types included — keep their dotted
                        // resolution.
                        if module_half_hides_names {
                            self.opaque_dotted_open = true;
                        }

                        // -- The namespace reading's prefixes (the reading is a chain
                        // base for later opens — the shortening prefix — and a head
                        // candidate for the same-file module-qualified classifier,
                        // [`Resolver::explicit_open_prefixes`]). Its *contents* are
                        // folded above (assembly half) and below (project cases). A
                        // module alias produces no reading, so it stays out (codex
                        // round 1).
                        if has_reading {
                            self.assembly_open_prefixes.push((pos, gp.clone()));
                            if !(project_readings_only && !project_ns) {
                                self.open_shortening_prefixes.push(gp.clone());
                                self.explicit_open_prefixes.push((pos, gp.clone()));
                            }
                        }

                        // -- THE FOLD (`docs/assembly-module-open-plan.md`, "the
                        // fold"): the assembly module half(s) and the assembly
                        // namespace half. Push every name each surface lists — vals,
                        // union cases, exception constructors, active-pattern tags,
                        // nested type names, auto-open submodule contents — in FCS's
                        // fold order, demoted to `Deferred` when the group's fold
                        // order is not decidable (`demote_module_half`). Cross-tier
                        // contests are ordered by the group sequence, cross-surface
                        // collisions (two assemblies, or the module vs the namespace
                        // half) demote per-name inside the writer, and hidden
                        // tycon-tier names are ENTRIES rather than a reason to defer
                        // the vals.
                        if !surfaces.is_empty() {
                            // Stage-1 signature screen
                            // (`docs/fsi-signature-restriction-plan.md`): a
                            // bare name this open would commit to an assembly
                            // member, at a path a signatured project module
                            // *may* expose, must defer instead — FCS binds
                            // the `.fsi` (probe: bare `shown` after `open
                            // ProbeNs.Shared` with a colliding `RefLib` → the
                            // `.fsi`). The entry is demoted to `Opaque` (in
                            // scope, shadowing by position, naming nothing)
                            // rather than removed, so an earlier open's
                            // same-named value cannot wrongly win. Names the
                            // signature provably cannot expose keep their
                            // assembly target (probe: bare `bar` → the
                            // assembly). Runs on the *complete* surface list
                            // — the namespace half's auto-open contents
                            // included.
                            let implicit_screened =
                                self.preceding.implicit_module_open_screened(gp);
                            for surface in &mut surfaces {
                                for entry in &mut surface.entries {
                                    if implicit_screened
                                        || self.preceding.sig_screened_open_name(gp, &entry.name)
                                    {
                                        entry.target = OpenFoldTarget::Opaque;
                                    }
                                }
                            }
                            self.open_assembly_module_fold(
                                surfaces,
                                pos,
                                demote_module_half,
                                below_vals,
                            );
                        }
                        // -- The assembly *module* half's dotted-head bookkeeping —
                        // keyed on the module handles, not the namespace surface.
                        if !handles.is_empty() {
                            self.open_shortening_prefixes.push(gp.clone());
                            // Only a prefix that could hide a whole nested *module*
                            // (a dropped type, an unknowable pickle) can make a
                            // later `open Sub` name something we cannot see
                            // (round 10 — the veto is expensive; keep it to what
                            // actually earns it). The dropped-type ask spans every
                            // split of the path (round 16).
                            if handles
                                .iter()
                                .any(|&h| self.assemblies.module_may_hide_nested_modules(h))
                                || self
                                    .assemblies
                                    .any_split_of_a_module_path_has_a_dropped_type(gp)
                            {
                                self.incomplete_open_prefixes.push(gp.clone());
                            }
                            // A *dotted head* through this module (`open M` then
                            // `Sub.f`) is not modelled yet (Slice B of the plan
                            // gives the walk a module-rooted prefix). Conservative
                            // while such an open is in scope — but only when the
                            // module could actually seed one: it has an
                            // **accessible** nested member — a *public* nested
                            // module/type a cross-assembly dotted head could root
                            // at (`open M` sees only public members). A childless
                            // module seeds nothing, and blanketing it would
                            // suppress the merged *namespace* half of the same path
                            // (Q9).
                            //
                            // The accessibility filter is load-bearing: an F#
                            // module's `let` values compile to (often dozens of)
                            // **non-public** compiler-generated closure classes that
                            // surface as `children`, yet none can be a
                            // cross-assembly dotted-head prefix. Counting them made
                            // opening ANY closure-backed module defer every later
                            // dotted head — `open Fantomas.FCS.Text.Range` (whose
                            // `Range` module is all closure classes, no public nested
                            // member) killed a bare `Seq.toList` two lines down. This
                            // is the same accessible-child fix the R2 primitive-alias
                            // shadow already made (`resolve_fsharp_core.rs`). (A
                            // residue-bearing surface — an undecodable member could
                            // be the head — is covered by the hidden-name blanket
                            // above; `module_may_hide_nested_modules` feeds the
                            // `incomplete_open_prefixes` veto separately.)
                            if handles.iter().any(|&h| {
                                self.assemblies
                                    .children(h)
                                    .iter()
                                    .any(|&c| self.assemblies.is_public(c))
                            }) {
                                self.opaque_dotted_open = true;
                            }
                        }

                        // -- The project namespace half's direct cases/exceptions,
                        // and (recursively) its `[<AutoOpen>]` submodules' contents.
                        // Pushed AFTER the assembly halves so that on a name shared
                        // with either assembly half the project entry wins by
                        // position (FCS folds the project fragment last — Q14).
                        // `open_project_namespace_values` raises the barrier itself,
                        // per hidden child, at the exact point the child's
                        // unenumerable names could shadow — not upfront here for the
                        // whole recursive tree (codex review: an upfront bump would
                        // stamp `gp`'s own direct entries with the bumped
                        // generation too, so a later hidden grandchild could never
                        // stale them).
                        //
                        // **Skipped for a literal self-open of the CURRENT enclosing
                        // namespace** (codex review round 4, fcs-dump-verified): an
                        // `[<AutoOpen>]` submodule's values are already visible to the
                        // rest of its OWN enclosing namespace's scope from the
                        // submodule's own declaration site — `namespace N` /
                        // `[<AutoOpen>] module A = let x = 1` / `module Client = let y
                        // = x` (no `open` at all) resolves `x` to `A.x`. An explicit
                        // `open N` written INSIDE that same namespace is therefore a
                        // redundant self-open, and re-running this recursive fold at
                        // the OPEN's (later) text position would wrongly re-introduce
                        // `A`'s values there, overriding a local binding declared
                        // between the namespace's start and the open that FCS's real
                        // (start-of-block) fold position does not reach.
                        //
                        // Gated on `path` (the `open`'s own AS-WRITTEN text, `global.`
                        // stripped) equalling the enclosing namespace, not merely on
                        // `gp`: `explicit_ancestor_open_lets_a_later_open_bind_the_current_namespace`
                        // (resolve_cross_file_cases.rs) writes `open Outer; open
                        // Inner` inside `namespace Outer.Inner` — the SECOND open's
                        // `written` is just `Inner`, reaching `Outer.Inner` only via
                        // the FIRST open's explicit prefix (tier 1), and FCS treats
                        // that as a genuine, intentional reference, not a self-open
                        // (the self/ancestor skip that rejects `Inner` as a *relative*
                        // tier-2 candidate for its own last segment is a SEPARATE,
                        // pre-existing mechanism, scoped to the implicit/enclosing
                        // tier only). Requiring `path` itself to spell out the
                        // enclosing namespace catches only the direct, as-written
                        // self-reference; `gp` still must match too, so an unrelated
                        // group sharing this open's `path` (an explicit prefix
                        // reaching some OTHER namespace that happens to also be
                        // spelled the same as this file's enclosing one from a
                        // DIFFERENT earlier prefix) is not skipped by accident.
                        if has_reading
                            && project_ns
                            && !(path.as_slice() == self.enclosing_namespace()
                                && gp.as_slice() == self.enclosing_namespace())
                        {
                            self.open_project_namespace_values(gp, pos);
                        }

                        // -- The project module half (highest: FCS folds the
                        // project's own fragment last, so it wins collisions —
                        // Q14). Bring its direct values into scope. If it may
                        // bring value-space names we cannot enumerate (an alias,
                        // or union cases / exception constructors / active
                        // patterns we do not export), bump the open generation
                        // *before* enumerating its `let`s so an earlier open's
                        // same-named value is shadowed (FCS: the latest open
                        // wins). It may also hold submodules/types we do not
                        // model, so dotted heads through it stay conservative
                        // (`opaque_dotted_open`).
                        if has_project_module {
                            // The opaque-marker arm of the provenance split
                            // above: hidden names no screen bounds must stale
                            // this open's own assembly entries too.
                            if project_module_bump && !bump_covered_by_screen {
                                self.open_generation += 1;
                            }
                            // An explicit `open M` brings *every* direct member of
                            // M (every fragment, every file) into scope, so no
                            // per-fragment restriction — that is only for a fragment
                            // reached implicitly by opening its enclosing namespace.
                            self.open_module_values(gp, pos, None);
                            self.module_open_prefixes.push((pos, gp.clone()));
                            self.open_shortening_prefixes.push(gp.clone());
                            self.opaque_dotted_open = true;
                        }
                    }
                }
                // Resolution-explain trace: record which opaque-open flags THIS
                // open flipped false→true (snapshot at the arm's start). Both the
                // `open type` and plain-`open` branches fall through to here.
                self.trace_opens.push(OpenTrace {
                    range: trace_range,
                    path: trace_path,
                    is_type: trace_is_type,
                    opacity: OpenOpacity {
                        opaque_value: self.opaque_value_open && !trace_before.0,
                        opaque_dotted: self.opaque_dotted_open && !trace_before.1,
                        unmodelled: self.unmodelled_open_active && !trace_before.2,
                        staled_earlier: self.open_generation > trace_before_gen,
                        imported_deferred: self.scopes.last().is_some_and(|f| {
                            f.entries
                                .get(trace_before_entries..)
                                .unwrap_or(&[])
                                .iter()
                                .any(|e| matches!(e.resolution, Resolution::Deferred(_)))
                        }),
                        added_reading: self.imports.len() > trace_before_imports
                            || self.open_shortening_prefixes.len() > trace_before_shortening,
                    },
                });
            }
            ModuleDecl::Attributes(_) => {
                // A standalone `[<assembly: …>]` (parser phase 10.7) introduces no
                // names and binds no references — a no-op for name resolution. The
                // attribute argument expressions are constants/paths the checker
                // resolves; sema does not model them. (The attribute *type* itself
                // is resolved below, like every declaration's.)
            }
        }
        // EX-3 §2(d): resolve this declaration's attribute types at the scope
        // now in force — AFTER the dispatch above, so a self-referential
        // attribute on a type definition (`[<Foo>] type FooAttribute…` — FCS
        // checks a tycon's attributes after entering it, and an `and`-group
        // enters every name first) sees the type(s) it decorates. A nested
        // module contributes only its *header* lists here — post-dispatch its
        // open state is restored, which is FCS's env for them — while its body
        // attributes were already resolved inside, at the nested scope, by the
        // recursion's own `module_decl` calls. Every other declaration kind
        // cannot contain a nested module, so its whole subtree shares this
        // scope (type resolution does not see expression-level binders).
        match decl {
            ModuleDecl::NestedModule(nm) => self.resolve_attribute_lists(nm.attributes()),
            other => self.resolve_attributes_under(other.syntax()),
        }
    }

    /// Resolve a nested `module M = …` (parser phase 8.4) by descending into its
    /// body with its own lexical scope. Its top-level bindings become exports
    /// qualified by the full path (`["Demo", "Calc", "x"]`), so a cross-file
    /// `Demo.Calc.x` resolves; within the body, sibling references resolve
    /// unqualified, and a binding the body introduces is *not* leaked into the
    /// enclosing scope — the body frame is popped before the enclosing walk
    /// resumes.
    ///
    /// The module name is still recorded in the shadow sets
    /// ([`Self::record_project_name_shadow`]): the cross-file value index holds
    /// only the module's *values*, so a reference rooted at it that names a
    /// non-value member (a nested type, a deeper module not yet exported) must
    /// still defer rather than fall through to a colliding referenced-assembly
    /// member — the `assembly_path_records` soundness tripwire. An exported
    /// value resolves first (via `lookup_qualified_path`) before that check runs,
    /// so the shadow never suppresses a resolution it should make.
    pub(super) fn nested_module(&mut self, nm: &NestedModuleDecl) {
        let Some(li) = nm.long_id() else {
            return;
        };
        let segs: Vec<String> = li.idents().map(|t| id_text(t.text()).to_string()).collect();
        if segs.is_empty() {
            return;
        }
        // Record the shadow against the *enclosing* container (before the path is
        // extended below), exactly as the deferral arm did. The module's own name
        // (first segment) is module-like in this container, so it shadows a
        // same-named enclosing type for member access (`Calc.x`).
        if let Some(first) = segs.first() {
            self.note_module_like_name(first);
            // A **real** nested module (not an alias) is a same-file container, so
            // record it in the declared-name view as a `Module` head. Aliases
            // (`module P = …`) are *not* marked — they are definitively cross-file
            // resolvable (via the alias tier of `cross_file_type_case` /
            // `qualified_value_in`), so a `P.Color.Red` head must stay a `Miss`, not a
            // same-file `DeferStop`.
            let decl = self.mark_decl(first);
            decl.module = true;
            // The head environment is source-ordered latest-wins across module
            // declarations and opens (see [`DeclKinds::module_pos`]); a later
            // redeclaration keeps the later position (latest wins).
            decl.module_pos = Some(nm.syntax().text_range().start().into());
        }
        self.record_project_name_shadow(segs.clone());
        let mut qualified = self.container_path.clone();
        qualified.extend(segs.iter().cloned());
        let nm_auto_open = attrs_auto_open(nm.attributes());
        let nm_private = header_is_private(nm.syntax());
        // The module-only cross-file index ([`ProjectItems::real_nested_modules`]):
        // unlike the name-shadow set just recorded (which types, exceptions,
        // abbreviations, and `extern`s share), this answers "is there a genuine
        // module at this path?" for a later file's open-target classification.
        // The same real-root guard as the shadow's cross-file half.
        if !self.anonymous_root {
            if nm_auto_open {
                self.record_auto_open_module(qualified.clone(), nm_private);
            }
            self.real_nested_module_exports.push(qualified.clone());
        }
        // The export-decl-list twin: one nested-module decl (`header: false`)
        // carrying its `[<AutoOpen>]`/`private` bits. Every derivation off it
        // filters `!anonymous_root`, so an anonymous-root nested module records an
        // inert decl (pitfall 1) rather than a spurious cross-file entry.
        self.push_export_decl(
            qualified,
            nm.syntax().text_range().start(),
            ExportDeclKind::Module {
                header: false,
                auto_open: nm_auto_open,
                private: nm_private,
            },
        );

        // Save every piece of module-scoped resolver *state* the body may mutate,
        // so an `open`, a nested type/module, or an `open type` inside the body
        // does not leak into the enclosing scope (F# scopes all of these to the
        // module). `imports`, `open_shortening_prefixes`, `open_generation`,
        // `unmodelled_open_active`, `opaque_value_open`, and `opaque_dotted_open`
        // steer name resolution directly — a leaked `open Demo` would
        // *mis-resolve* a later enclosing sibling through an import not in scope —
        // so restoring them is a soundness requirement, not just hygiene;
        // `nested_module_locals` restoration keeps an inner nested name from
        // over-deferring an outer reference. The *opened* scope entries an
        // `open type` pushes need no save/restore: they live in the body frame,
        // which is popped and discarded below, so they vanish with it.
        // (File-level accumulators — `items`, `defs`, `resolutions`,
        // `nested_module_exports` — are the file's contribution and persist.)
        let saved_module_path = self.module_path.clone();
        let saved_container_path = self.container_path.clone();
        let saved_imports = self.imports.clone();
        let saved_open_shortening_prefixes = self.open_shortening_prefixes.clone();
        let saved_incomplete_open_prefixes = self.incomplete_open_prefixes.clone();
        let saved_explicit_open_prefixes = self.explicit_open_prefixes.clone();
        let saved_module_open_prefixes = self.module_open_prefixes.clone();
        let saved_assembly_open_prefixes = self.assembly_open_prefixes.clone();
        let saved_open_generation = self.open_generation;
        let saved_pattern_suppressed_case_ids = self.pattern_suppressed_case_ids.clone();
        let saved_unmodelled_open = self.unmodelled_open_active;
        let saved_opaque_value_open = self.opaque_value_open;
        let saved_opaque_dotted_open = self.opaque_dotted_open;
        let saved_recursive_module = self.recursive_module_active;
        let saved_auto_open_type_shadow_names = self.auto_open_type_shadow_names.clone();
        let saved_nested_locals = self.nested_module_locals.clone();
        let saved_access_floor = self.access_floor;

        // The nested module's full path = enclosing container + its own name(s)
        // (`module A.B = …` contributes both), the container for the body's own
        // nested modules / types (`container_path`). It is the value-export
        // prefix (`module_path`) for every *real* root — including `namespace
        // global`, whose nested `module Calc`'s value is bare-cross-file
        // referenceable as `Calc.x`. Only an **anonymous** file's nested module
        // (under the unmodeled implicit filename module — `Calc.x` would really
        // be `<FileName>.Calc.x`) suppresses the export, leaving
        // `module_path = None` to match the anonymous top-level module's own
        // no-cross-file-export invariant. Intra-file resolution is unaffected
        // either way (it goes through the scope frame, not `module_path`).
        let mut path = self.container_path.clone();
        path.extend(segs);
        self.module_path = (!self.anonymous_root).then(|| path.clone());
        self.container_path = path;
        // A `module private Sub` scopes its contents to its parent (this module's
        // container); stacked private modules take the deepest floor (see
        // [`Resolver::access_floor`]). A non-private module inherits the enclosing
        // floor unchanged.
        if header_is_private(nm.syntax()) {
            let parent_len = self.container_path.len().saturating_sub(1);
            self.access_floor = Some(saved_access_floor.map_or(parent_len, |f| f.max(parent_len)));
        }
        self.recursive_module_active = saved_recursive_module || nm.is_rec();
        // Entering a nested `module rec` from a non-rec scope starts a fresh
        // rec block: pre-scan ITS nested-module names. An enclosing rec block
        // already scanned this subtree (the outer collection recurses), so
        // its superset stays in place.
        let saved_rec_module_names = if !saved_recursive_module && nm.is_rec() {
            let mut names = std::collections::HashSet::new();
            super::collect_nested_module_names(nm.decls(), &mut names);
            Some(std::mem::replace(&mut self.rec_module_names, names))
        } else {
            None
        };

        // A fresh frame for the body: bindings live here (visible to the body,
        // and qualified to later cross-file references) and are dropped from
        // lexical scope when the frame is popped, so they never leak unqualified
        // into the enclosing module. The frame is the innermost, so
        // [`Self::module_frame`] targets it for this module's exports.
        self.scopes.push(Frame::default());
        for decl in nm.decls() {
            self.module_decl(&decl);
        }
        self.scopes.pop();

        self.module_path = saved_module_path;
        self.container_path = saved_container_path;
        self.access_floor = saved_access_floor;
        self.imports = saved_imports;
        self.explicit_open_prefixes = saved_explicit_open_prefixes;
        self.module_open_prefixes = saved_module_open_prefixes;
        self.assembly_open_prefixes = saved_assembly_open_prefixes;
        self.open_shortening_prefixes = saved_open_shortening_prefixes;
        self.incomplete_open_prefixes = saved_incomplete_open_prefixes;
        self.open_generation = saved_open_generation;
        self.pattern_suppressed_case_ids = saved_pattern_suppressed_case_ids;
        self.unmodelled_open_active = saved_unmodelled_open;
        self.opaque_value_open = saved_opaque_value_open;
        self.opaque_dotted_open = saved_opaque_dotted_open;
        self.recursive_module_active = saved_recursive_module;
        if let Some(saved) = saved_rec_module_names {
            self.rec_module_names = saved;
        }
        // An `[<AutoOpen>]` nested module opens into the remainder of its
        // container's scope: propagate the names grown inside the body (an
        // auto-open descendant's names flow outward through exactly this
        // branch — the FCS-recursive chain) plus this module's own direct
        // **public** type names (`type private T` is visible within the
        // module only — codex round 2). Accessibility is depth-bounded, not
        // boolean: a name contributed through a `module private` auto-open
        // module is visible no shallower than that module's container, so
        // each entry carries its minimum visible depth, filtered here on
        // every hop outward. A plain module's imports stay inside it.
        if attrs_auto_open(nm.attributes()) {
            let parent_depth = self.container_path.len();
            let own_private = header_is_private(nm.syntax());
            let import_pos = u32::from(nm.syntax().text_range().start());
            let grown = std::mem::replace(
                &mut self.auto_open_type_shadow_names,
                saved_auto_open_type_shadow_names,
            );
            for (name, entry) in grown {
                // Entries the parent scope already had pass through untouched;
                // this subtree's contributions are depth-filtered (invisible
                // at the parent → dropped) and — through a private module —
                // pinned to the parent's depth.
                let inherited = self.auto_open_type_shadow_names.get(&name) == Some(&entry);
                if inherited {
                    continue;
                }
                if entry.min_depth > parent_depth {
                    continue;
                }
                let min_depth = if own_private {
                    entry.min_depth.max(parent_depth)
                } else {
                    entry.min_depth
                };
                merge_auto_open_shadow(
                    &mut self.auto_open_type_shadow_names,
                    name,
                    AutoOpenTypeShadow {
                        import_pos: entry.import_pos,
                        min_depth,
                    },
                );
            }
            // The module's own direct public types, read from its syntax (the
            // same header scan `define_type` fed from).
            for decl in nm.decls() {
                if let ModuleDecl::Types(types) = decl {
                    for defn in types.defns() {
                        if is_type_augmentation(&defn) || type_header_is_private(&defn) {
                            continue;
                        }
                        if let Some(name) = defn.long_id().and_then(single_ident) {
                            merge_auto_open_shadow(
                                &mut self.auto_open_type_shadow_names,
                                id_text(name.text()).to_string(),
                                AutoOpenTypeShadow {
                                    import_pos,
                                    min_depth: if own_private { parent_depth } else { 0 },
                                },
                            );
                        }
                    }
                }
            }
        } else {
            self.auto_open_type_shadow_names = saved_auto_open_type_shadow_names;
        }
        self.nested_module_locals = saved_nested_locals;
    }

    /// Record a project-introduced *name* — a nested module
    /// ([`ModuleDecl::NestedModule`]), a module-abbreviation alias
    /// ([`ModuleDecl::ModuleAbbrev`]), or a type definition
    /// ([`ModuleDecl::Types`]) — in the shadow sets, so a reference rooted at it
    /// defers in [`Self::assembly_path_records`] rather than falling through to a
    /// colliding referenced-assembly member. Sema does not yet model what these
    /// names *provide* (members / fields / aliased modules), so deferring is the
    /// sound under-resolution.
    ///
    /// Two shadows, recorded under different conditions:
    /// - **Same-file** (`nested_module_locals`): always, for a reference written
    ///   relative to the enclosing module (`Calc.Answer` for a local `Calc`).
    /// - **Cross-file** (`nested_module_exports`): only when the file has a *real*
    ///   root (a `namespace`/`module` header, including `namespace global`),
    ///   qualified by `container_path` — so a nested `Calc` under `namespace Demo`
    ///   exports `Demo.Calc`. In an **anonymous** (header-less) file the names are
    ///   visible cross-file only under the filename-derived module, which this
    ///   resolver does not model; recording a *bare* `Demo.Calc` there would
    ///   wrongly shadow an unrelated module's path in a later file (over-deferring
    ///   a valid assembly resolution), so no cross-file shadow is recorded.
    ///
    /// The guard is [`Self::anonymous_root`], **not** `container_path.is_empty()`:
    /// the two diverge for a nested module under an anonymous file (where
    /// `container_path` is `["Calc"]` but the path is still filename-relative) and
    /// for `namespace global` (empty `container_path` but a real root). It is the
    /// same predicate that gates the value export in [`Self::nested_module`], so a
    /// name is cross-file shadowed exactly when it is cross-file exportable.
    pub(super) fn record_project_name_shadow(&mut self, segs: Vec<String>) {
        if segs.is_empty() {
            return;
        }
        if !self.anonymous_root {
            let mut qualified = self.container_path.clone();
            qualified.extend(segs.iter().cloned());
            self.nested_module_exports.push(qualified);
        }
        self.nested_module_locals.push(segs);
    }

    /// Note `name` as a *module-like* declaration (nested module / abbreviation)
    /// in the current container, so member access through it shadows a same-named
    /// enclosing type (see [`Self::module_like_names`]).
    pub(super) fn note_module_like_name(&mut self, name: &str) {
        self.module_like_names
            .entry(self.container_path.clone())
            .or_default()
            .insert(id_text(name).to_string());
    }

    /// The [`DeclKinds`](super::state::DeclKinds) slot for `name` in the current
    /// container ([`Self::container_decls`]), inserting an empty one if absent. The
    /// single mutation point for the per-container declared-name view: each definition
    /// site sets the relevant namespace flag (`mark_decl(name).value = true`, …).
    pub(super) fn mark_decl(&mut self, name: &str) -> &mut super::state::DeclKinds {
        self.container_decls
            .entry(self.container_path.clone())
            .or_default()
            .entry(id_text(name).to_string())
            .or_default()
    }
}
