//! `let`-binding processing: interning, scheduling, and RHS resolution.

use std::collections::HashSet;

use borzoi_cst::syntax::{AstNode, Binding, Expr, LetDecl, LetOrUseExpr, SyntaxKind};

use crate::binders::{BinderRole, binders};
use crate::def::{DefId, DefKind};
use crate::diagnostics::{SemaDiagnostic, SemaDiagnosticKind};

use super::model::{ExportedItem, ItemId, Resolution};
use super::state::{Frame, Resolver, ScopeEntry};
use super::types::head_typar_decls;
use super::{active_pat_name_of, active_pattern_param_arity, id_text};

/// The interned binders of one top-level binding, before they are made visible.
pub(super) struct PreparedBinding {
    /// Head value binders (exported items), already interned and self-resolved.
    item_entries: Vec<ScopeEntry>,
    /// Curried-argument binders (parameter locals), already interned and
    /// self-resolved; visible only in this binding's RHS.
    param_entries: Vec<ScopeEntry>,
    /// Active-pattern *case* entries (each pointing at the recognizer), if the
    /// head is an active pattern. Empty otherwise. Added to the module frame on
    /// the same `rec` schedule as `item_entries` — before the RHSs for a `rec`
    /// group (so a `rec` recognizer's body sees its own cases as patterns), after
    /// them otherwise — and they persist for later decls.
    eager_entries: Vec<ScopeEntry>,
    /// This binding's generic parameters (`let f<'T> …`), interned once and keyed
    /// by bare `idText` name. Empty for a non-generic binding. Activated as a
    /// typar frame around the head annotations (in `prepare_binding`) and again
    /// around the RHS (in `resolve_rhss`), so a `'T` in either position resolves.
    typar_frame: Vec<(String, DefId)>,
    rhs: Option<Expr>,
}

/// One prepared block-/class-`let` binding, ready for RHS resolution: its
/// curried-parameter entries, its active-pattern case entries, its
/// generic-parameter (typar) frame (interned once, re-activated around the RHS —
/// mirrors [`PreparedBinding::typar_frame`]), and its RHS expression.
type PreparedLocalBinding = (
    Vec<ScopeEntry>,
    Vec<ScopeEntry>,
    Vec<(String, DefId)>,
    Option<Expr>,
);

impl<'a> Resolver<'a> {
    /// Process a top-level `let` / `let rec [… and …]` group. The head value
    /// binders become exported [`ItemId`]s; the curried-argument binders become
    /// [`Resolution::Local`] parameters visible only in that binding's RHS.
    ///
    /// `let rec`: every binding's items are in scope for *every* RHS in the
    /// group, so they are added to the module frame before any RHS is resolved.
    /// `let` (non-rec): no binding's items are in scope for any RHS in the
    /// group, so the RHSs are resolved first and the items added afterwards.
    /// Either way the items persist in the module frame for later decls.
    pub(super) fn module_let(&mut self, let_decl: &LetDecl) {
        let prepared: Vec<PreparedBinding> = let_decl
            .bindings()
            .map(|b| self.prepare_binding(&b))
            .collect();

        // A *module-level* active pattern (a binding with case entries) is a
        // value-namespace member an `open` of this module brings into pattern
        // scope but `open_module_values` does not enumerate, so opening this module
        // may shadow earlier opens (see [`Self::modules_with_hidden_values`]).
        if prepared.iter().any(|p| !p.eager_entries.is_empty()) {
            self.note_hidden_value_module(self.container_path.clone());
        }

        // Active-pattern cases follow the same `rec` discipline as ordinary value
        // binders (added alongside [`Self::add_items`]): a `rec` recognizer's
        // cases are in scope for every RHS in the group, a non-`rec` one's are
        // not — FCS resolves a case used as a *pattern* in the recognizer's own
        // non-`rec` body as a fresh variable, not the case. Either way they
        // persist in the module frame for later decls.
        let is_rec = let_decl.is_rec();
        if is_rec {
            self.add_eager_entries(&prepared);
            self.add_items(&prepared);
            self.resolve_rhss(&prepared);
        } else {
            // A non-`rec` group's binders are not in scope in any of its RHSs, yet
            // `prepare_binding` has already eagerly interned them into `self.items`.
            // Mark them *pending* so the value-shadow check ignores them while the
            // RHSs resolve — a binding's own qualified self-reference must reach the
            // earlier definition, not the not-yet-in-scope binder (Gap B).
            let pending: HashSet<ItemId> = prepared
                .iter()
                .flat_map(|p| p.item_entries.iter())
                .filter_map(|e| match e.resolution {
                    Resolution::Item(id) => Some(id),
                    _ => None,
                })
                .collect();
            let saved = std::mem::replace(&mut self.pending_items, pending);
            self.resolve_rhss(&prepared);
            self.pending_items = saved;
            self.add_eager_entries(&prepared);
            self.add_items(&prepared);
        }
    }

    /// Create the binders of one top-level binding's head pattern: head values
    /// become interned items (with their self-resolution recorded), curried
    /// arguments become interned parameter locals (likewise). Nothing is pushed
    /// into a scope yet — the caller controls visibility timing.
    pub(super) fn prepare_binding(&mut self, binding: &Binding) -> PreparedBinding {
        // Attribute *presence* on the binding — `let [<Literal>] x` (leading
        // `ATTRIBUTE_LIST` children of the `BINDING`) or the pre-`let` form
        // `[<Literal>] let x` (leading children of the enclosing `LET_DECL`).
        // A multi-`and` decl's pre-`let` run over-approximates onto every
        // binding of the group — FCS attaches it to the first only, but the
        // flag only ever widens a pattern-position *defer*, never a commit.
        let attributed = binding.attributes().next().is_some()
            || binding.syntax().parent().is_some_and(|p| {
                p.kind() == SyntaxKind::LET_DECL
                    && p.children().any(|c| c.kind() == SyntaxKind::ATTRIBUTE_LIST)
            });
        let mut item_entries = Vec::new();
        let mut param_entries = Vec::new();
        let mut eager_entries = Vec::new();
        // A generic binding head (`let f<'T> (x: 'T) : 'T = …`) declares typars
        // scoped to this binding's annotations *and* its RHS. Intern them once
        // and activate the frame around the head-pattern / return-type resolution
        // below; `resolve_rhss` re-activates the stored frame around the RHS.
        let typar_frame = binding
            .pat()
            .as_ref()
            .and_then(head_typar_decls)
            .map(|d| self.intern_typars(&d))
            .unwrap_or_default();
        let pushed_typars = !typar_frame.is_empty();
        if pushed_typars {
            self.typar_scopes.push(typar_frame.clone());
        }
        if let Some(head) = binding.pat() {
            // Type annotations in the head (`let f (x : T) = …`) name types, not
            // value binders, so they are resolved separately from the binders.
            // The *let-head* variant: an applied single-segment `LongIdent` here is
            // the function being defined (`let DivBy x = x`), never an
            // active-pattern use, so its args are not split (which would drop a
            // genuine parameter).
            self.resolve_let_head_pat_types(&head);
            // An active-pattern head (`let (|Even|Odd|) … = …`) is not an ordinary
            // binder: intern the recognizer + case tokens and collect the case
            // *entries* (each pointing at the recognizer). These are "eager" — in
            // scope for every RHS in the group (a total active pattern constructs
            // its own cases in its body) and for later decls — so the caller adds
            // them before resolving any RHS. The curried args, if any, still bind
            // as parameters through the ordinary binders walk below (the
            // active-pattern head itself contributes no binder there).
            if let Some(apn) = active_pat_name_of(&head) {
                let arity = active_pattern_param_arity(&head);
                // A `let private (|Even|Odd|)` scopes its cross-file case handle to
                // its container, exactly as a `let private` value / `private` case
                // does (`export_access_root_len`).
                let is_private = super::decls::header_is_private(binding.syntax());
                eager_entries = self.define_active_pattern(&apn, true, arity, is_private);
            }
            for def in binders(&head, BinderRole::Let) {
                // An active-pattern *parameter* argument (`let (Scale divisor) =
                // …`): the shape-keyed split (run by `resolve_pat_types` above) has
                // resolved it as an expression and excluded its fabricated binder
                // range. Skip it — before the `provisional` branch — leaving no
                // recorded self-resolution and no scope entry.
                if self.excluded_param_ranges.contains(&def.range) {
                    continue;
                }
                // Provisional maybe-var head (`None` in `let (x, None) = …`,
                // `let f None = …`): a constructor-shaped head naming a known
                // union case in scope is a case *reference*, so resolve it;
                // otherwise decline (drop) — a genuine maybe-var head stays
                // unrecorded (a coverage gap, never wrong). See the module-level
                // "Provisional pattern heads" note.
                if def.provisional {
                    if let Some(res) = self.case_reference(&def.name) {
                        self.record(def.range, res);
                    }
                    continue;
                }
                let name = id_text(&def.name).to_string();
                let range = def.range;
                let is_value = matches!(def.kind, DefKind::Value { .. });
                let id = self.intern(def);
                if is_value {
                    // Project-global handle: this file's items occupy the
                    // contiguous range starting at `item_base`.
                    let item_idx = self.items.len();
                    let item_id = ItemId::new(self.item_base as usize + item_idx);
                    let qualified = self.qualified_export_path(&name);
                    self.items.push(ExportedItem {
                        name: name.clone(),
                        qualified,
                        id: item_id,
                        def: id,
                        case: None,
                        // Own `let private` modifier narrows to the value's own
                        // container; otherwise it inherits any `private` enclosing
                        // module (see `export_access_root_len` / `access_floor`).
                        // This keeps the collapse recovery from committing an
                        // inaccessible value to an outside `open`.
                        access_root_len: self.export_access_root_len(
                            super::decls::header_is_private(binding.syntax()),
                        ),
                        attributed,
                    });
                    // The export-decl-list twin of the `ExportedItem` push: an
                    // ordinary `let` value (no case, no type-qualified path). Its
                    // `value_exports` row derives from the referenced item.
                    let mut decl_path = self.container_path.clone();
                    decl_path.push(name.clone());
                    self.push_export_decl(
                        decl_path,
                        range.start(),
                        super::model::ExportDeclKind::Item {
                            item: Some(item_idx),
                            type_qualified: None,
                        },
                    );
                    let res = Resolution::Item(item_id);
                    self.record(range, res);
                    let mut entry = ScopeEntry::binding(name, res, self.open_generation);
                    // A maybe-literal module-level value contests the pattern
                    // namespace as a constant pattern (see
                    // [`ScopeEntry::maybe_constant_pattern`]).
                    entry.maybe_constant_pattern = attributed;
                    item_entries.push(entry);
                } else {
                    // A curried argument of a function binding — a parameter.
                    let res = Resolution::Local(id);
                    self.record(range, res);
                    param_entries.push(ScopeEntry::binding(name, res, self.open_generation));
                }
            }
        }
        // The return-type annotation (`let x : T = …`) is a type use too.
        if let Some(ret) = binding.return_type() {
            self.resolve_type(&ret);
        }
        if pushed_typars {
            self.typar_scopes.pop();
        }
        PreparedBinding {
            item_entries,
            param_entries,
            eager_entries,
            typar_frame,
            rhs: binding.expr(),
        }
    }

    /// Resolve an expression-level (block) `let`/`use` group — the plain
    /// (`IsBang = false`) form of `SynExpr.LetOrUse`. The same scoping as
    /// [`Self::module_let`], but the head-value binders are *locals*
    /// ([`Resolution::Local`]), interior to the enclosing expression rather than
    /// exported items: each binding's head value scopes the `LetOrUse` body, and
    /// its curried parameters scope only that binding's RHS. `let rec` puts the
    /// value binders in scope for every RHS (and the body); a plain `let`
    /// resolves the RHSs first (no binder of the group visible to any RHS).
    ///
    /// Uses `BinderRole::Let` (not the bang form's `BinderRole::Pattern`): the
    /// head is a function-binding head, so `let f a = …` binds `f` as a value
    /// and `a` as a parameter — `Pattern` would mis-read `f` as a constructor
    /// reference and never bind it.
    pub(super) fn resolve_local_let(&mut self, e: &LetOrUseExpr) {
        // `use rec` is FCS's `FS0821` (`tcBindingCannotBeUseAndRec`). It is a
        // *semantic* error FCS raises during type-checking, but a syntactically
        // decidable one — both keywords are present on the group — so it is
        // always sound to report here without any inference (see
        // [`SemaDiagnostic`]). `is_rec()` is only ever true for the plain
        // form, so the bang binder (`use!`) never trips this. Reported once per
        // group, anchored at the `use` keyword (FCS spans the whole expression).
        if e.is_use()
            && e.is_rec()
            && let Some(kw) = e.keyword()
        {
            self.diagnostics.push(SemaDiagnostic {
                range: kw.text_range(),
                kind: SemaDiagnosticKind::UseAndRec,
            });
        }

        // Per binding: the head-value binders (scope the body) and the curried
        // parameter binders (scope that binding's RHS). Each binder is interned
        // and records its own self-resolution here, regardless of visibility
        // timing — exactly as `prepare_binding` does for the module-level form.
        let (value_entries, per_binding) = self.prepare_local_bindings(e.bindings());

        // `let rec`: value binders (and active-pattern cases) visible to every RHS
        // *and* the body, so push the frame before resolving RHSs. Plain `let`:
        // RHSs resolve first (the group's binders not yet in scope — so a case
        // used as a *pattern* in a non-`rec` recognizer's own body is a fresh
        // variable, not the case, matching FCS), then the frame for the body.
        // Either way exactly one value-binder frame is left on the stack for the
        // body, popped below. The `if`/`else` (not two sequential `if`s) moves
        // `value_entries` down exactly one path.
        if e.is_rec() {
            self.scopes.push(Frame {
                entries: value_entries,
            });
            self.resolve_local_let_rhss(&per_binding);
        } else {
            self.resolve_local_let_rhss(&per_binding);
            self.scopes.push(Frame {
                entries: value_entries,
            });
        }
        if let Some(body) = e.body() {
            self.resolve_expr(&body);
        }
        self.scopes.pop();
    }

    /// Intern the binders of a `let`/`let rec` group (block-`let` or a type's
    /// class-level `let` fields) — the shared core of [`Self::resolve_local_let`]
    /// and [`Self::resolve_class_let`](super::Resolver::resolve_class_let).
    ///
    /// Returns `(value_entries, per_binding)`: the head-value binders (which the
    /// caller makes visible to the body / rest of the class) and, per binding,
    /// its curried-parameter entries, active-pattern case entries, and RHS. Each
    /// binder is interned and self-resolved here; the caller controls the `rec`
    /// visibility timing (push the value frame before or after
    /// [`Self::resolve_local_let_rhss`]).
    pub(super) fn prepare_local_bindings(
        &mut self,
        bindings: impl Iterator<Item = Binding>,
    ) -> (Vec<ScopeEntry>, Vec<PreparedLocalBinding>) {
        let mut value_entries: Vec<ScopeEntry> = Vec::new();
        let mut per_binding: Vec<PreparedLocalBinding> = Vec::new();
        for b in bindings {
            let mut params = Vec::new();
            let mut ap_cases = Vec::new();
            // This binding's generic parameters (`let f<'T> (x: 'T) = …`),
            // interned once and activated around the head annotations here, then
            // re-activated around the RHS in `resolve_local_let_rhss` — exactly as
            // the module-level `prepare_binding`. Pushing it also *shadows* an
            // enclosing same-named typar to a collision (see `lookup_typar`), so a
            // nested generic `let` never silently binds the outer parameter.
            let typar_frame = b
                .pat()
                .as_ref()
                .and_then(head_typar_decls)
                .map(|d| self.intern_typars(&d))
                .unwrap_or_default();
            let pushed_typars = !typar_frame.is_empty();
            if pushed_typars {
                self.typar_scopes.push(typar_frame.clone());
            }
            if let Some(head) = b.pat() {
                // Annotations in the head (`let (x : T) = …`) are type uses. The
                // let-head variant suppresses the AP split for a direct
                // function-binding head (`let DivBy x = x`) — see
                // [`Self::resolve_let_head_pat_types`].
                self.resolve_let_head_pat_types(&head);
                // A local active-pattern head: intern its recognizer + cases and
                // collect the case entries among the *value* binders, so they
                // follow the same `rec` timing (in scope for the RHS only when
                // `rec`; for the body either way). The curried args still bind as
                // parameters through the ordinary binders walk below. The cases are
                // also kept per-binding (`ap_cases`) so the recognizer's own body
                // sees them as expression constructors (see [`resolve_local_let_rhss`]).
                if let Some(apn) = active_pat_name_of(&head) {
                    let arity = active_pattern_param_arity(&head);
                    // A *local* active pattern is not a module member — it exports
                    // no cross-file handle, so its `private`-ness is irrelevant.
                    ap_cases = self.define_active_pattern(&apn, false, arity, false);
                    value_entries.extend(ap_cases.iter().cloned());
                }
                for def in binders(&head, BinderRole::Let) {
                    // An active-pattern *parameter* argument: the shape-keyed split
                    // (run by `resolve_pat_types` above) has resolved it as an
                    // expression and excluded its fabricated binder range. Skip it —
                    // before the `provisional` branch — as in `prepare_binding`.
                    if self.excluded_param_ranges.contains(&def.range) {
                        continue;
                    }
                    // Provisional maybe-var head (`None` in `let f None = …`):
                    // resolve a known union-case reference, else decline (drop),
                    // as in `prepare_binding`.
                    if def.provisional {
                        if let Some(res) = self.case_reference(&def.name) {
                            self.record(def.range, res);
                        }
                        continue;
                    }
                    let name = id_text(&def.name).to_string();
                    let range = def.range;
                    let is_value = matches!(def.kind, DefKind::Value { .. });
                    let id = self.intern(def);
                    let res = Resolution::Local(id);
                    self.record(range, res);
                    let entry = ScopeEntry::binding(name, res, self.open_generation);
                    if is_value {
                        value_entries.push(entry);
                    } else {
                        params.push(entry);
                    }
                }
            }
            if let Some(ret) = b.return_type() {
                self.resolve_type(&ret);
            }
            if pushed_typars {
                self.typar_scopes.pop();
            }
            per_binding.push((params, ap_cases, typar_frame, b.expr()));
        }
        (value_entries, per_binding)
    }

    /// Resolve each local-let binding's RHS with that binding's curried
    /// parameters in scope (and nothing else of the group, unless a `let rec`
    /// frame is already pushed by the caller). Mirrors [`Self::resolve_rhss`].
    pub(super) fn resolve_local_let_rhss(&mut self, per_binding: &[PreparedLocalBinding]) {
        for (params, ap_cases, typar_frame, rhs) in per_binding {
            // Re-activate this binding's generic parameters so a `'T` annotation
            // inside the RHS resolves (and shadows an enclosing same-named typar).
            let pushed_typars = !typar_frame.is_empty();
            if pushed_typars {
                self.typar_scopes.push(typar_frame.clone());
            }
            self.scopes.push(Frame {
                entries: params.clone(),
            });
            // Mark the binding's own case names for its RHS (mirrors the
            // module-level [`resolve_rhss`]): a bare body use of the recognizer's
            // own case name declines rather than commit an outer value or the case.
            let restore = self.enter_ap_body(ap_cases);
            if let Some(rhs) = rhs {
                self.resolve_expr(rhs);
            }
            self.ap_body_case_names = restore;
            self.scopes.pop();
            if pushed_typars {
                self.typar_scopes.pop();
            }
        }
    }

    pub(super) fn add_items(&mut self, prepared: &[PreparedBinding]) {
        for p in prepared {
            for e in &p.item_entries {
                // Record the value in the container's declared-name view at
                // *scope-entry* time — for a non-`rec` group this is after the RHSs
                // resolve, so a binding's own RHS does not see the not-yet-in-scope
                // binder (`let Pal = Pal.Color.Red` must resolve `Pal` cross-file, not
                // stop on the same-file binder); for a `rec` group it is before, as the
                // binder is in scope in its own RHS. Mirrors the scope-frame timing.
                self.mark_decl(&e.name).value = true;
                self.module_frame().entries.push(e.clone());
            }
        }
    }

    /// Add the active-pattern *case* entries of the group to the current module
    /// frame, alongside [`Self::add_items`] and on the same `rec` schedule (see
    /// [`Self::module_let`]): before the RHSs for a `rec` group, after them
    /// otherwise. Either way the cases persist in the module frame for later
    /// decls in the container — like union cases, at the definition's source
    /// position.
    pub(super) fn add_eager_entries(&mut self, prepared: &[PreparedBinding]) {
        for p in prepared {
            for e in &p.eager_entries {
                self.module_frame().entries.push(e.clone());
            }
        }
    }

    pub(super) fn resolve_rhss(&mut self, prepared: &[PreparedBinding]) {
        for p in prepared {
            // Re-activate this binding's generic parameters (interned in
            // `prepare_binding`) so a `'T` annotation *inside* the RHS resolves.
            let pushed_typars = !p.typar_frame.is_empty();
            if pushed_typars {
                self.typar_scopes.push(p.typar_frame.clone());
            }
            self.scopes.push(Frame {
                entries: p.param_entries.clone(),
            });
            // A recognizer's own case name used *bare* in its own body is ambiguous
            // — a result-case construction (`let (|A|B|) x = … then A …`, FCS
            // `ActivePatternCase`) or a fresh uppercase pattern rebinding (`match n
            // with A -> A`, FCS a fresh local) — and a resolution-only pass cannot
            // tell them apart. Mark those names for the RHS so
            // [`resolve_name_use`](Self::resolve_name_use) declines a bare use
            // rather than commit an outer same-named value (the AP-body-shadow bug)
            // or the case. Only bare uses are affected; a qualified head is untouched.
            let restore = self.enter_ap_body(&p.eager_entries);
            if let Some(rhs) = &p.rhs {
                self.resolve_expr(rhs);
            }
            self.ap_body_case_names = restore;
            self.scopes.pop();
            if pushed_typars {
                self.typar_scopes.pop();
            }
        }
    }

    /// Add each entry's case name to [`Self::ap_body_case_names`] for the duration
    /// of a recognizer's RHS, returning the previous set to restore afterward
    /// (accumulating for nested recognizers). A no-op when `case_entries` is empty
    /// (an ordinary, non-active-pattern binding).
    fn enter_ap_body(&mut self, case_entries: &[ScopeEntry]) -> HashSet<String> {
        let restore = self.ap_body_case_names.clone();
        for e in case_entries {
            self.ap_body_case_names.insert(id_text(&e.name).to_string());
        }
        restore
    }

    /// The frame top-level bindings of the *current* module are added to. At
    /// decl-processing time the scope stack is exactly the chain of enclosing
    /// module frames (every transient frame — lambda params, match clauses,
    /// let-RHS params — is pushed and popped within a single expression
    /// resolution), so the innermost is the current module's frame: the base
    /// frame for a top-level module, or the frame [`Self::nested_module`] pushed
    /// for a `module M = …` body. Adding to the innermost (not the base) keeps a
    /// nested module's bindings out of the enclosing scope.
    pub(super) fn module_frame(&mut self) -> &mut Frame {
        self.scopes
            .last_mut()
            .expect("module frame pushed in resolve_file")
    }
}
