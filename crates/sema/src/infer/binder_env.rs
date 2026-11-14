//! [`BinderEnv`], in a module of its own so that its fields are reached only
//! through its methods.

use super::ModuleBinder;
use crate::def::DefId;
use crate::hash::{HashMap, HashSet};
use crate::ty::{Ty, TyVid};

/// What the walk knows of each in-file binder it met: its variable, its
/// scheme once generalised, whether it is withheld, and for a parameter
/// its slot, its annotation and its function. FCS's `TcEnv`'s
/// accumulating part — the types of the values its name environment
/// reaches. Held apart from [`super::Solver`], whose variables it holds,
/// so that the solver knows no [`DefId`].
///
/// The methods only add: no entry is removed, and no map's entry is
/// overwritten, which each writer asserts. What the types cannot hold is
/// the order, and that is broken in one place. A binder's entries are
/// written in its own binding, so in document order, except its
/// variable: [`Self::bind_var`] is called at the binder's first
/// reference ([`super::Gen::def_var`]), and for a binder whose
/// declaration the walk does not visit (a `let` FCS drops in a
/// namespace, or a binding construct that is not a `let`) that reference
/// is a later binding's use.
pub(super) struct BinderEnv {
    /// The inference variable standing for each referenced in-file binder.
    def_vars: HashMap<DefId, TyVid>,
    /// Generalised binders' schemes, keyed by `DefId` (Stage 3.2c-2c). The scheme
    /// body has open variables replaced by [`TyKind::Param`](crate::ty::TyKind::Param)s
    /// (numbered by first appearance); each use
    /// [instantiates](super::Solver::instantiate) it with fresh variables.
    /// Computed at a function binding's finalisation, consulted by a later
    /// binding's use — so it is filled sequentially, in document order.
    def_schemes: HashMap<DefId, Ty>,
    /// The **private function-type slot** variable for each simple named
    /// parameter of a `let`-function head ([`super::Gen::param_var`]): the parameter's
    /// type *inside* the function's [`TyKind::Fun`](crate::ty::TyKind::Fun), and the only variable
    /// [`super::Gen::constrain_bool`] grounds when that parameter is used as a
    /// condition. It is deliberately **separate** from the parameter binder's
    /// [`def_var`](super::Gen::def_var) — which its expression uses and `def_type` read
    /// off — so the condition-derived `bool` flows *only* into the function type,
    /// never into a standalone parameter read-off where an unmodelled constraint
    /// (a use-site ascription, an annotation) could make it wrong on ill-typed
    /// input (D5). Keyed by the parameter's `DefId`, so a condition referencing
    /// that parameter finds its slot. On a *complete* binding the slot is
    /// reunified with the binder var ([`super::Gen::slot_binder_reunify`]).
    param_slots: HashMap<DefId, TyVid>,
    /// Each parameter of a function whose type [`super::Gen::function_type`] built,
    /// with that function: a parameter's type is published only where its
    /// function's is ([`super::Gen::finish`]).
    param_owner: HashMap<DefId, DefId>,
    /// Every parameter `DefId` collected during [`super::Gen::param_var`].
    /// [`super::Gen::finish`] publishes each from its annotation or its slot in a
    /// published function type, never from its binder variable.
    param_defs: HashSet<DefId>,
    /// Each annotated parameter's annotation: what FCS knows of the parameter
    /// at every use in its body.
    param_annotations: HashMap<DefId, Ty>,
    /// Functions withheld for a rigid variable the body fixed
    /// ([`super::Incomplete::RigidTypeVariable`]): never published, and a use of one
    /// is not typed.
    withheld_defs: HashSet<DefId>,
    /// Every binder a module-level `let` binds ([`super::Gen::own_binders`]): the
    /// binders [`super::Gen::finish`] decides a [`super::ModuleValue`] for.
    module_binders: HashMap<DefId, ModuleBinder>,
    /// The module-level binders whose binding was walked for its type
    /// ([`super::Gen::type_let_decl`]), whether or not it found one.
    walked_binders: HashSet<DefId>,
}

impl BinderEnv {
    pub(super) fn new() -> Self {
        BinderEnv {
            def_vars: HashMap::default(),
            def_schemes: HashMap::default(),
            param_slots: HashMap::default(),
            param_owner: HashMap::default(),
            param_defs: HashSet::default(),
            param_annotations: HashMap::default(),
            withheld_defs: HashSet::default(),
            module_binders: HashMap::default(),
            walked_binders: HashSet::default(),
        }
    }

    /// `def`'s variable, if it has one yet.
    pub(super) fn var(&self, def: DefId) -> Option<TyVid> {
        self.def_vars.get(&def).copied()
    }

    /// Give `def`, which has none, the variable `v`.
    pub(super) fn bind_var(&mut self, def: DefId, v: TyVid) {
        let previous = self.def_vars.insert(def, v);
        debug_assert!(previous.is_none(), "{def:?} already has a variable");
    }

    /// Every binder's variable, for [`super::Gen::finish`].
    pub(super) fn vars(&self) -> impl Iterator<Item = (DefId, TyVid)> + '_ {
        self.def_vars.iter().map(|(&def, &v)| (def, v))
    }

    /// `def`'s scheme, if it was generalised.
    pub(super) fn scheme(&self, def: DefId) -> Option<Ty> {
        self.def_schemes.get(&def).copied()
    }

    /// Record that `def` generalised to `scheme`.
    pub(super) fn generalised(&mut self, def: DefId, scheme: Ty) {
        let previous = self.def_schemes.insert(def, scheme);
        debug_assert!(previous.is_none(), "{def:?} generalised twice");
    }

    /// Register `def` as a function parameter whose slot is `slot`.
    pub(super) fn register_param(&mut self, def: DefId, slot: TyVid) {
        let previous = self.param_slots.insert(def, slot);
        debug_assert!(previous.is_none(), "{def:?} registered twice");
        self.param_defs.insert(def);
    }

    /// The slot of `def`, if it is a registered parameter.
    pub(super) fn param_slot(&self, def: DefId) -> Option<TyVid> {
        self.param_slots.get(&def).copied()
    }

    /// Every registered parameter.
    pub(super) fn params(&self) -> impl Iterator<Item = DefId> + '_ {
        self.param_defs.iter().copied()
    }

    /// Whether `def` is a registered parameter.
    pub(super) fn is_param(&self, def: DefId) -> bool {
        self.param_defs.contains(&def)
    }

    /// Record that the parameter `param` is one of `f_def`'s.
    pub(super) fn own_param(&mut self, param: DefId, f_def: DefId) {
        let previous = self.param_owner.insert(param, f_def);
        debug_assert!(previous.is_none(), "{param:?} owned twice");
    }

    /// The function `param` is a parameter of, if its type was built.
    pub(super) fn param_owner(&self, param: DefId) -> Option<DefId> {
        self.param_owner.get(&param).copied()
    }

    /// Record the parameter `def`'s annotation, `ty`.
    pub(super) fn annotate_param(&mut self, def: DefId, ty: Ty) {
        let previous = self.param_annotations.insert(def, ty);
        debug_assert!(previous.is_none(), "{def:?} annotated twice");
    }

    /// The parameter `def`'s annotation, if it has one.
    pub(super) fn param_annotation(&self, def: DefId) -> Option<Ty> {
        self.param_annotations.get(&def).copied()
    }

    /// Withhold every binder of `defs`.
    pub(super) fn withhold(&mut self, defs: impl IntoIterator<Item = DefId>) {
        self.withheld_defs.extend(defs);
    }

    /// Whether `def` is withheld.
    pub(super) fn is_withheld(&self, def: DefId) -> bool {
        self.withheld_defs.contains(&def)
    }

    /// Record that `def` is bound by a module-level `let`, as `binder`.
    /// A pattern may name one binder twice (`let (x | x) = …`, whose
    /// alternatives share their binders), and then records it twice,
    /// alike.
    pub(super) fn module_binder(&mut self, def: DefId, binder: ModuleBinder) {
        let previous = self.module_binders.insert(def, binder);
        debug_assert!(
            previous.is_none_or(|p| p == binder),
            "{def:?} bound twice, differently"
        );
    }

    /// Every module-level binder.
    pub(super) fn module_binders(&self) -> impl Iterator<Item = (DefId, ModuleBinder)> + '_ {
        self.module_binders.iter().map(|(&def, &b)| (def, b))
    }

    /// Record that the binding of every binder of `defs` was walked.
    pub(super) fn walked(&mut self, defs: impl IntoIterator<Item = DefId>) {
        self.walked_binders.extend(defs);
    }

    /// Whether `def`'s binding was walked.
    pub(super) fn was_walked(&self, def: DefId) -> bool {
        self.walked_binders.contains(&def)
    }
}
