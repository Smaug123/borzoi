//! The walk's **windows** ([`Windows`]): state that holds a value only for
//! the dynamic extent of one call, and is back at its outer value when the
//! call returns.
//!
//! A window is opened by a scoped call (`self.with_binding_head(true, |r|
//! …)`), which sets it, runs the body, and restores it, so neither an early
//! return nor `?` in the body can skip the restore. The fields are private to
//! this module and the scoped calls are the only writers; everything else
//! reads them through [`Windows`]' predicates.
//!
//! Two checks run in debug builds. A scoped call asserts that the body hands
//! the window back holding the value it was opened with (a nested call
//! restores its own value before returning, so the only way to fail is a
//! write that bypassed the scoped calls). And the finished walk asserts that
//! every window is closed ([`Windows::assert_closed`]): nothing is left open
//! past the call that opened it.

use std::fmt::Debug;

use crate::hash::HashSet;

use super::model::ItemId;
use super::state::{Resolver, ScopeEntry};

/// The windows; see the [module docs](self).
#[derive(Debug, Default, PartialEq)]
pub(super) struct Windows {
    /// `true` while resolving a **binding-head** pattern — a `let` head or a
    /// lambda's curried parameters — where a shape-keyed active-pattern split
    /// may run ([`resolve_pattern_arg_as_expr`](Resolver::resolve_pattern_arg_as_expr)).
    /// In such a position an earlier curried parameter is **not yet in
    /// scope** when a later parameter pattern's active-pattern argument is
    /// resolved (FCS scopes curried parameters left to right — `let f d
    /// (DivBy d)` scopes the first `d` into the second parameter's `DivBy
    /// d`), so resolving the argument against the enclosing scope could
    /// commit to the *wrong* target (a same-named module value the earlier
    /// parameter should shadow). The argument's expression resolution is
    /// therefore **declined** here (its fabricated binder is still excluded —
    /// a sound coverage gap, never a wrong commit); a *match*-clause or
    /// general pattern position (the window is closed) resolves it normally
    /// against the enclosing scope, which is exactly FCS's rule there. Opened
    /// around the let head
    /// ([`resolve_let_head_pat_types`](Resolver::resolve_let_head_pat_types))
    /// and the lambda-parameter walk ([`pattern_locals`](Resolver::pattern_locals)
    /// with [`BinderRole::Param`](crate::BinderRole)).
    binding_head: bool,
    /// Project-global [`ItemId`]s of the value binders of the **current
    /// non-`rec` `let … and …` group**, while that group's RHSs are being
    /// resolved — items that are interned (eagerly pushed to
    /// [`Output::items`](crate::resolve::output::Output::items) by [`prepare_binding`](Resolver::prepare_binding))
    /// but **not yet in scope**, because a non-`rec` binding is not visible in
    /// its own (or its group's) RHS. Empty outside such a window and for `rec`
    /// groups (whose binders *are* in scope).
    ///
    /// [`ordinary_value_at`](Resolver::ordinary_value_at) skips these so the
    /// value-vs-type-qualifier shadow check
    /// ([`value_shadows_case`](Resolver::value_shadows_case)) does not let a
    /// binding being defined shadow its own qualified self-reference: `let
    /// Color = Lib.Container.Color.Red` resolves to the earlier file's case,
    /// not member access on the not-yet-in-scope `Color` (FCS-verified).
    /// Opened around [`resolve_rhss`](Resolver::resolve_rhss) in
    /// [`module_let`](Resolver::module_let).
    pending_items: HashSet<ItemId>,
    /// The case names (`idText`-normalised) of every active-pattern recognizer
    /// whose own RHS is *currently being resolved*, accumulating for nested
    /// recognizers. A **bare** expression use of one of these names in the
    /// recognizer's own body ([`resolve_name_use`](Resolver::resolve_name_use))
    /// is ambiguous between constructing the result case (FCS
    /// `ActivePatternCase`) and a fresh uppercase *pattern* rebinding (`match
    /// n with A -> A`, where FCS binds `A` as a fresh local) — a distinction a
    /// resolution-only pass cannot draw, since it drops uppercase provisional
    /// binders — so the use *declines* rather than commit an outer same-named
    /// value (the original AP-body-shadow bug) or the case. Only **bare**
    /// single-ident expression uses are declined: a *qualified* head (`A.X`,
    /// where `A` names a type sharing the case name) is resolved by
    /// [`resolve_long_ident`](Resolver::resolve_long_ident), which never
    /// consults this set, so it resolves normally. Empty outside a recognizer
    /// body. Opened around each binding's body by
    /// [`resolve_rhss`](Resolver::resolve_rhss) and
    /// [`resolve_local_let_rhss`](Resolver::resolve_local_let_rhss).
    ap_body_case_names: HashSet<String>,
}

impl Windows {
    /// Whether a binding-head pattern is being resolved
    /// ([`Self::binding_head`]).
    pub(super) fn in_binding_head(&self) -> bool {
        self.binding_head
    }

    /// Whether `id` is a binder of the non-`rec` group whose RHSs are being
    /// resolved ([`Self::pending_items`]).
    pub(super) fn is_pending(&self, id: ItemId) -> bool {
        self.pending_items.contains(&id)
    }

    /// Whether `name` is a case of a recognizer whose own RHS is being
    /// resolved ([`Self::ap_body_case_names`]).
    pub(super) fn is_ap_body_case(&self, name: &str) -> bool {
        self.ap_body_case_names.contains(name)
    }

    /// Assert, in debug builds, that every window is closed: what the walk
    /// holds once no scoped call is running.
    pub(super) fn assert_closed(&self) {
        debug_assert_eq!(
            *self,
            Windows::default(),
            "a window was left open past the call that opened it"
        );
    }
}

impl Resolver<'_> {
    /// Run `body` with the window `window` holding `value`, then restore the
    /// window's outer value. In debug builds, assert that the body handed the
    /// window back holding `value`.
    fn with_window<T: Clone + PartialEq + Debug, R>(
        &mut self,
        window: fn(&mut Windows) -> &mut T,
        value: T,
        body: impl FnOnce(&mut Self) -> R,
    ) -> R {
        #[cfg(debug_assertions)]
        let opened = value.clone();
        let outer = std::mem::replace(window(&mut self.windows), value);
        let out = body(self);
        let inner = std::mem::replace(window(&mut self.windows), outer);
        #[cfg(debug_assertions)]
        assert_eq!(
            inner, opened,
            "a window changed inside the call that opened it"
        );
        drop(inner);
        out
    }

    /// Run `body` with the binding-head window ([`Windows::binding_head`])
    /// open when `on`, and closed otherwise.
    pub(super) fn with_binding_head<R>(
        &mut self,
        on: bool,
        body: impl FnOnce(&mut Self) -> R,
    ) -> R {
        self.with_window(|w| &mut w.binding_head, on, body)
    }

    /// Run `body` with `pending` as the not-yet-in-scope binders
    /// ([`Windows::pending_items`]).
    pub(super) fn with_pending_items<R>(
        &mut self,
        pending: HashSet<ItemId>,
        body: impl FnOnce(&mut Self) -> R,
    ) -> R {
        self.with_window(|w| &mut w.pending_items, pending, body)
    }

    /// Run `body`, a recognizer's RHS, with each of `case_entries`' names
    /// added to the recognizer-body cases ([`Windows::ap_body_case_names`]).
    /// With no entries (an ordinary binding) the window is left as it is.
    pub(super) fn with_ap_body_cases<R>(
        &mut self,
        case_entries: &[ScopeEntry],
        body: impl FnOnce(&mut Self) -> R,
    ) -> R {
        if case_entries.is_empty() {
            return body(self);
        }
        let mut names = self.windows.ap_body_case_names.clone();
        for e in case_entries {
            names.insert(self.assemblies.names().text(e.name).to_string());
        }
        self.with_window(|w| &mut w.ap_body_case_names, names, body)
    }
}
