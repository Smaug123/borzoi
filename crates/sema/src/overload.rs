//! The OV-5 **applicability matcher** — the pure, side-effect-free core of
//! overload resolution (`docs/overload-resolution-plan.md` §4.2–4.3).
//!
//! Overload resolution commits a candidate `c*` from a group `G` only when the
//! group is provably complete, `c*` is provably applicable, and every *other*
//! candidate is provably *not* applicable (the keystone rule, plan §1). The two
//! "provably" directions have **opposite** soundness requirements and so are two
//! different functions here:
//!
//! - [`AssemblyEnv::must_apply`] is an **under**-approximation of FCS
//!   applicability: everything it affirms, FCS affirms. It affirms the *winner*.
//! - [`AssemblyEnv::may_apply`] is an **over**-approximation: everything it
//!   rejects, FCS rejects. Its `false` *eliminates* a losing candidate.
//! - [`arity_window`] is the argument-count half of the over-approximation: a
//!   candidate is applicable only at caller counts inside its window, so a count
//!   outside it eliminates the candidate.
//!
//! Using one approximation for both jobs is exactly the mistake the arity
//! shortcut made (plan §1); keeping them separate is the whole point of the
//! design. The functions are pure over `&MethodLike` + `&[Ty]` (the `&self`
//! borrow is read-only metadata: base chains for the subtype test, member lists
//! for the `op_Implicit` table). The engine that drives them and runs the
//! group-completeness gate is a later stage (OV-6); this stage is the matcher
//! and its property tests.

use borzoi_assembly::{Access, Member, MethodLike, ParamDefault, Parameter, Primitive, TypeRef};

use crate::assembly_env::{AssemblyEnv, EntityHandle, MemberIndex};
use crate::ty::Ty;

/// The inclusive caller-argument-count window `[min, max]` at which a candidate
/// *could* be FCS-applicable — the arity half of `may_apply`'s
/// over-approximation. `max == None` is ∞ (a trailing `[<ParamArray>]`).
///
/// Deliberately **generous** (a sound *super*set of the truly-applicable
/// counts): `min` is a lower bound and `max` an upper bound, so a caller count
/// *inside* the window never wrongly eliminates a candidate FCS finds
/// applicable. A count *outside* it is a sound elimination — no trimming, params
/// expansion, or optional/`out` omission can make a method with too few or too
/// many parameters match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArityWindow {
    /// The fewest caller args FCS could accept: the declared parameter count
    /// minus the maximal trailing run of *omittable* parameters (each optional
    /// xor `out`, plus a trailing param array which may take zero elements).
    pub min: usize,
    /// The most caller args FCS could accept: `None` (∞) when a trailing param
    /// array absorbs any surplus, else the declared parameter count.
    pub max: Option<usize>,
}

impl ArityWindow {
    /// Whether `n` caller arguments fall inside the window.
    pub fn contains(&self, n: usize) -> bool {
        n >= self.min && self.max.is_none_or(|max| n <= max)
    }
}

/// The [`ArityWindow`] of a candidate (plan §4.2, arity prong).
///
/// Walks the parameters from the end counting the maximal trailing run of
/// *omittable* ones: a trailing `[<ParamArray>]` (it can take zero elements),
/// then each preceding parameter that is optional **xor** `out`
/// ([`ParamDefault::Optional`]/[`ParamDefault::FSharpOptional`] count as
/// optional). A parameter that is *both* optional and `out`, or *neither*,
/// breaks the run — matching FCS's "one violation silently disables all
/// trimming" (§2.2). `max` is ∞ exactly when a trailing param array is present.
pub fn arity_window(method: &MethodLike) -> ArityWindow {
    let params = &method.signature.parameters;
    let has_param_array = params.last().is_some_and(|p| p.is_param_array);
    let mut omittable = 0usize;
    for (i, p) in params.iter().enumerate().rev() {
        let is_last = i + 1 == params.len();
        if (is_last && p.is_param_array) || is_omittable(p) {
            omittable += 1;
        } else {
            break;
        }
    }
    ArityWindow {
        min: params.len() - omittable,
        max: if has_param_array {
            None
        } else {
            Some(params.len())
        },
    }
}

/// Whether a parameter is trailing-trimmable: optional **xor** `out`. A param
/// that is both (the §2.2 landmine) or neither is not omittable.
fn is_omittable(p: &Parameter) -> bool {
    let optional = matches!(
        p.default,
        ParamDefault::Optional(_) | ParamDefault::FSharpOptional
    );
    optional ^ p.is_out
}

/// A type in the **closed decidable set** the type-prong reasons about (plan
/// §4.2): the sealed BCL primitives (numerics, `IntPtr`/`UIntPtr`, `Decimal`,
/// `String`, `Char`, `Boolean`) and 1-D vectors of such. Any other type — `obj`,
/// an interface, a non-sealed named type, a typar, a byref, a bounded/multidim
/// array — is *not* in the set: a position holding one cannot eliminate its
/// candidate (its conversion channels are open-ended), which is exactly why the
/// mapping functions return `None` for it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ClosedTy {
    /// A sealed primitive, by its canonical dotted path (`"System.Int32"`).
    Prim(&'static str),
    /// A 1-D vector `T[]` whose element `T` is a sealed primitive.
    Vector(&'static str),
}

/// The 16 sealed BCL primitives — `is_sealed_primitive` in `infer.rs`, restated
/// here as the closed-set membership currency (canonical dotted paths). `Object`
/// is deliberately absent: `obj` is an *open* target (everything boxes to it),
/// so it can never eliminate a position.
const SEALED_PRIMITIVES: [&str; 16] = [
    "System.SByte",
    "System.Byte",
    "System.Int16",
    "System.UInt16",
    "System.Int32",
    "System.UInt32",
    "System.Int64",
    "System.UInt64",
    "System.IntPtr",
    "System.UIntPtr",
    "System.Single",
    "System.Double",
    "System.Decimal",
    "System.String",
    "System.Char",
    "System.Boolean",
];

/// The canonical sealed-primitive name equal to `dotted`, if any.
fn sealed_canon_str(dotted: &str) -> Option<&'static str> {
    SEALED_PRIMITIVES.iter().copied().find(|&c| c == dotted)
}

/// The canonical sealed-primitive name of a dotted-path slice (`["System",
/// "Int32"]`), if it names one.
fn sealed_canon_path(path: &[String]) -> Option<&'static str> {
    sealed_canon_str(&path.join("."))
}

/// The canonical dotted path of an ECMA primitive code — including the *open*
/// `System.Object` (used by the subtype test and `op_Implicit` matching, which
/// care about `obj`), unlike the closed-set membership above. `Void` has no
/// value-carrying form and maps to `None`.
fn primitive_canon(p: Primitive) -> Option<&'static str> {
    Some(match p {
        Primitive::Void => return None,
        Primitive::Bool => "System.Boolean",
        Primitive::Char => "System.Char",
        Primitive::I1 => "System.SByte",
        Primitive::U1 => "System.Byte",
        Primitive::I2 => "System.Int16",
        Primitive::U2 => "System.UInt16",
        Primitive::I4 => "System.Int32",
        Primitive::U4 => "System.UInt32",
        Primitive::I8 => "System.Int64",
        Primitive::U8 => "System.UInt64",
        Primitive::R4 => "System.Single",
        Primitive::R8 => "System.Double",
        Primitive::IntPtr => "System.IntPtr",
        Primitive::UIntPtr => "System.UIntPtr",
        Primitive::Object => "System.Object",
        Primitive::String => "System.String",
    })
}

/// The full canonical dotted path of a named or primitive [`TypeRef`] — total
/// over the shapes `op_Implicit` signatures use, `None` for anything generic /
/// nested / array / byref. Includes `System.Object` (unlike the closed set).
fn canon_of_type_ref(ty: &TypeRef) -> Option<String> {
    match ty {
        TypeRef::Primitive(p) => primitive_canon(*p).map(str::to_owned),
        TypeRef::Named {
            namespace,
            name,
            type_args,
            segment_arities,
            ..
        } if type_args.is_empty() && segment_arities.iter().all(|&a| a == 0) => {
            Some(dotted_of(namespace, name))
        }
        _ => None,
    }
}

/// `namespace.name` (just `name` when the namespace is empty).
fn dotted_of(namespace: &[String], name: &str) -> String {
    if namespace.is_empty() {
        name.to_owned()
    } else {
        format!("{}.{name}", namespace.join("."))
    }
}

impl ClosedTy {
    /// The closed-set classification of an inference [`Ty`] (a caller argument),
    /// or `None` if it is outside the set.
    fn of_ty(ty: &Ty) -> Option<ClosedTy> {
        match ty {
            Ty::Named(path) => sealed_canon_path(path).map(ClosedTy::Prim),
            Ty::Array { elem, rank: 1 } => match elem.as_ref() {
                Ty::Named(path) => sealed_canon_path(path).map(ClosedTy::Vector),
                _ => None,
            },
            _ => None,
        }
    }

    /// The closed-set classification of a metadata [`TypeRef`] (a parameter
    /// type), or `None` if it is outside the set. A bounded/multidim array or an
    /// array of arrays is *not* in the set (only plain vectors of primitives).
    fn of_type_ref(ty: &TypeRef) -> Option<ClosedTy> {
        match ty {
            TypeRef::Primitive(p) => primitive_canon(*p)
                .and_then(sealed_canon_str)
                .map(ClosedTy::Prim),
            TypeRef::Named {
                namespace,
                name,
                type_args,
                segment_arities,
                ..
            } if type_args.is_empty() && segment_arities.iter().all(|&a| a == 0) => {
                sealed_canon_str(&dotted_of(namespace, name)).map(ClosedTy::Prim)
            }
            TypeRef::Array {
                element,
                rank: 1,
                sizes,
                lower_bounds,
            } if sizes.is_empty() && lower_bounds.is_empty() => {
                match ClosedTy::of_type_ref(&element.ty) {
                    Some(ClosedTy::Prim(c)) => Some(ClosedTy::Vector(c)),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// The built-in F#-6 widening conversions (`LanguageFeature.AdditionalType-
/// DirectedConversions`, plan §2.4(2)): `int32` widens to `int64`, `nativeint`,
/// and `float`. Only `int32` is a source. Assumed enabled (the over-approx
/// direction is unaffected if the language flag is off — §5).
fn is_builtin_widening(from: &str, to: &str) -> bool {
    from == "System.Int32" && matches!(to, "System.Int64" | "System.IntPtr" | "System.Double")
}

impl AssemblyEnv {
    /// `may_apply` — the **over-approximation** (plan §4.2). `true` whenever FCS
    /// could find `method` applicable to `args`; a `false` is a sound
    /// *elimination* of a losing candidate. `declaring` is the declaring
    /// assembly of the type the method is read from (for canonicalising
    /// `assembly: None` parameter references and resolving conversion targets).
    ///
    /// A candidate is applicable under *some* argument-alignment interpretation;
    /// `may_apply` returns `true` if **any** interpretation survives (has no
    /// position it can prove unconvertible). Two prongs eliminate:
    ///
    /// - **Arity** — a count outside the generous [`arity_window`] is a sound
    ///   elimination (no trimming, params expansion, or optional/`out` omission
    ///   can rescue it).
    /// - **Type** — at an in-window count, a *leading* argument position whose
    ///   type provably cannot convert to its parameter (see
    ///   [`AssemblyEnv::may_apply`]'s refuter). Only positions whose alignment is
    ///   the same in **every** interpretation are consulted, so an omitted
    ///   trailing optional/`out` never causes a wrong elimination: for a non-params
    ///   method the supplied args align 1:1 to the leading parameters (a *leading*
    ///   optional can never be dropped, since a later supplied argument forces it);
    ///   a params method is refuted only when its expanded **and** direct-array
    ///   interpretations both fail, and — when fewer args than the fixed
    ///   parameters are supplied (a leading optional omitted, ambiguous alignment)
    ///   — it is conservatively kept.
    pub fn may_apply(&self, method: &MethodLike, declaring: &str, args: &[Ty]) -> bool {
        let window = arity_window(method);
        let n = args.len();
        // Arity prong.
        if !window.contains(n) {
            return false;
        }
        let params = &method.signature.parameters;
        if !params.last().is_some_and(|p| p.is_param_array) {
            // Non-params: the sole interpretation aligns args[i] to params[i] for
            // i < n (any trailing optional/out run is omitted). Refute iff a
            // leading position is provably unconvertible.
            return !(0..n).any(|i| self.position_refuted(&args[i], &params[i]));
        }
        // Params method. Fewer args than the fixed parameters means a leading
        // optional/out was omitted, so which argument lands on which parameter is
        // ambiguous — conservatively keep the candidate.
        let k = params.len();
        if n + 1 < k {
            return true;
        }
        // Otherwise the candidate survives if EITHER interpretation is unrefuted:
        // the expanded form (surplus args as elements) or, at exact arity, the
        // direct-array form (the trailing array passed whole).
        let expanded_ok = !self.expanded_refuted(method, declaring, args);
        let direct_ok = n == k && !(0..n).any(|i| self.position_refuted(&args[i], &params[i]));
        expanded_ok || direct_ok
    }

    /// Whether the **expanded params** interpretation has a provably-unconvertible
    /// position: any leading fixed parameter, or any surplus argument against the
    /// trailing array's element type. Caller guarantees the trailing parameter is
    /// a param array and `args.len() + 1 >= params.len()`.
    fn expanded_refuted(&self, method: &MethodLike, _declaring: &str, args: &[Ty]) -> bool {
        let params = &method.signature.parameters;
        let k = params.len();
        for i in 0..k - 1 {
            if self.position_refuted(&args[i], &params[i]) {
                return true;
            }
        }
        // Surplus args vs the element type — only decidable when the array is a
        // plain vector of a closed element type; otherwise the surplus cannot
        // refute (an open element admits any argument).
        if let TypeRef::Array {
            element,
            rank: 1,
            sizes,
            lower_bounds,
        } = &params[k - 1].ty
            && sizes.is_empty()
            && lower_bounds.is_empty()
            && let Some(elem) = ClosedTy::of_type_ref(&element.ty)
        {
            for a in &args[k - 1..] {
                if let Some(arg) = ClosedTy::of_ty(a)
                    && self.no_conversion_channel(&arg, &elem)
                {
                    return true;
                }
            }
        }
        false
    }

    /// Whether the argument `a` at parameter `p` is provably beyond every
    /// conversion channel (both closed-set, `p` not byref). A byref or open-typed
    /// position can never refute — its channels are open-ended.
    fn position_refuted(&self, a: &Ty, p: &Parameter) -> bool {
        if p.is_byref {
            return false;
        }
        match (ClosedTy::of_ty(a), ClosedTy::of_type_ref(&p.ty)) {
            (Some(arg), Some(param)) => self.no_conversion_channel(&arg, &param),
            _ => false,
        }
    }

    /// Whether **no** conversion channel can carry a value of closed type `a`
    /// into a parameter of closed type `p` (plan §4.2, the refuter). `true`
    /// (eliminate) only when every channel is ruled out: identity/subsumption
    /// (structural inequality), the built-in widening table, and `op_Implicit`
    /// on either entity. Delegate/`Nullable`/auto-quoting channels never target a
    /// closed-set type, so they need no check.
    fn no_conversion_channel(&self, a: &ClosedTy, p: &ClosedTy) -> bool {
        if a == p {
            return false; // identity (and reflexive subsumption)
        }
        if let (ClosedTy::Prim(from), ClosedTy::Prim(to)) = (a, p) {
            if is_builtin_widening(from, to) {
                return false;
            }
            if self.op_implicit_exists(from, to) {
                return false;
            }
        }
        // Distinct closed types with no widening and no `op_Implicit` — including
        // any scalar↔vector or distinct-vector pair (arrays declare no user
        // conversions and never widen).
        true
    }

    /// Whether a static, public, non-generic `op_Implicit : from -> to` is
    /// declared on **either** the source or the target entity (plan §2.4(4)). To
    /// soundly *rule out* the channel we must have inspected both possible
    /// declaring entities *completely*; we conservatively report the channel
    /// *may* exist (`true`) — which only ever *keeps* a candidate, never a wrong
    /// elimination — whenever we cannot: if **either** entity is absent from the
    /// env, or either **dropped an `op_Implicit` into
    /// [`skipped_members`](borzoi_assembly::Entity::skipped_members)** (an
    /// undecodable conversion signature — the same "unreadable member ⇒ defer"
    /// rule the inherited-member walk uses).
    fn op_implicit_exists(&self, from: &str, to: &str) -> bool {
        match (self.lookup_canon(from), self.lookup_canon(to)) {
            (Some(source), Some(target)) => {
                self.has_skipped_op_implicit(source)
                    || self.has_skipped_op_implicit(target)
                    || self.declares_op_implicit(source, from, to)
                    || self.declares_op_implicit(target, from, to)
            }
            _ => true,
        }
    }

    /// Whether `handle` dropped a member named `op_Implicit` into its
    /// `skipped_members` — an undecodable conversion the decoded `members` list
    /// does not witness, so its absence there cannot be trusted.
    fn has_skipped_op_implicit(&self, handle: EntityHandle) -> bool {
        self.entity(handle)
            .skipped_members
            .iter()
            .any(|skipped| skipped.name == "op_Implicit")
    }

    /// Whether `handle` declares a static public non-generic
    /// `op_Implicit(from) : to`.
    fn declares_op_implicit(&self, handle: EntityHandle, from: &str, to: &str) -> bool {
        self.entity(handle).members.iter().any(|member| {
            let Member::Method(m) = member else {
                return false;
            };
            m.is_static
                && m.access == Access::Public
                && !m.is_constructor
                && m.generic_parameters.is_empty()
                && m.name == "op_Implicit"
                && m.signature.parameters.len() == 1
                && !m.signature.parameters[0].is_byref
                && canon_of_type_ref(&m.signature.parameters[0].ty).as_deref() == Some(from)
                && canon_of_type_ref(&m.signature.return_type).as_deref() == Some(to)
        })
    }

    /// Resolve a canonical dotted primitive/type name (`"System.Object"`) to its
    /// interned handle, if the env holds it.
    fn lookup_canon(&self, canon: &str) -> Option<EntityHandle> {
        let (namespace, name) = match canon.rsplit_once('.') {
            Some((ns, name)) => (
                ns.split('.').map(str::to_owned).collect::<Vec<_>>(),
                name.to_owned(),
            ),
            None => (Vec::new(), canon.to_owned()),
        };
        self.lookup_type(&namespace, &name, 0)
    }

    /// `must_apply` — the **under-approximation** (plan §4.3). `true` implies FCS
    /// applicability, so it may affirm the *winner*. Conservative by design: it
    /// affirms only the fully-supplied, ground-argument, no-TDC shape — a
    /// candidate applicable only via widening/`op_Implicit`, an omitted
    /// optional/`out`, a byref parameter, a generic method, or a constructor
    /// fails here and the call defers (all sound, all extendable later).
    ///
    /// Per argument, affirmation is structural type-equality **or** provable
    /// subsumption (`A :> P` via the receiver's base/interface closure) — never a
    /// type-directed conversion. `declaring` canonicalises `assembly: None`
    /// parameter references (see [`AssemblyEnv::may_apply`]).
    ///
    /// Applicability only: the caller (OV-6) separately bridges the return type
    /// (or records a `void` identity) at commit — a return the bridge declines
    /// defers there, not here.
    pub fn must_apply(&self, method: &MethodLike, declaring: &str, args: &[Ty]) -> bool {
        // Deferred winner shapes (§5): generic, constructor, any byref/out param.
        if !method.generic_parameters.is_empty() || method.is_constructor {
            return false;
        }
        let params = &method.signature.parameters;
        if params.iter().any(|p| p.is_byref || p.is_out) {
            return false;
        }
        // Every caller argument must be ground (no unsolved inference variable).
        if !args.iter().all(Ty::is_ground) {
            return false;
        }
        if params.last().is_some_and(|p| p.is_param_array) {
            return self.must_apply_expanded(method, declaring, args);
        }
        // Direct form: fully supplied, exact arity, every argument affirmed.
        args.len() == params.len()
            && args
                .iter()
                .zip(params)
                .all(|(a, p)| self.arg_affirms(a, &p.ty, declaring))
    }

    /// The expanded-params affirmation: leading fixed parameters affirmed 1:1 and
    /// every surplus argument affirmed against the trailing array's element type
    /// (which must itself be closed — §5). Caller guarantees the trailing
    /// parameter is a param array.
    fn must_apply_expanded(&self, method: &MethodLike, declaring: &str, args: &[Ty]) -> bool {
        let params = &method.signature.parameters;
        let k = params.len();
        if args.len() + 1 < k {
            return false; // need at least k - 1 supplied
        }
        for i in 0..k - 1 {
            if !self.arg_affirms(&args[i], &params[i].ty, declaring) {
                return false;
            }
        }
        let TypeRef::Array {
            element,
            rank: 1,
            sizes,
            lower_bounds,
        } = &params[k - 1].ty
        else {
            return false;
        };
        if !sizes.is_empty() || !lower_bounds.is_empty() {
            return false;
        }
        // A params *winner* is allowed only when its element type is in the
        // closed set (§5) — else the affirmed shape is not one v1 characterises.
        if ClosedTy::of_type_ref(&element.ty).is_none() {
            return false;
        }
        args[k - 1..]
            .iter()
            .all(|a| self.arg_affirms(a, &element.ty, declaring))
    }

    /// Whether argument `a` affirms parameter type `p`: structural type-equality
    /// or provable subsumption `a :> p`. No type-directed conversion (§4.3).
    ///
    /// A **named** argument's identity is *assembly-qualified*, but [`Ty::Named`]
    /// carries only a dotted path (no assembly — a §7 representational limit) and
    /// the env's first-wins index collapses a colliding FQN onto one handle. So a
    /// named type whose identity we cannot prove — one whose FQN two referenced
    /// assemblies declare, or whose namespace had a **dropped** type (a same-FQN
    /// sibling may have been dropped, making the "unique" count unreliable) — must
    /// not affirm ([`Self::ty_named_unprovable`]); an `A::N.X` argument must not
    /// affirm a `B::N.X` parameter. This is checked **recursively**, so it also
    /// covers a named type nested inside a `Ty::Array` (`A::N.X[]`), which
    /// [`ty_equiv`] would otherwise compare element-vs-element with no assembly
    /// identity. A sealed BCL primitive (and `System.Object`) is CLR-unique, so the
    /// common literal-argument path is unaffected (review, GPT-5.6).
    fn arg_affirms(&self, a: &Ty, p: &TypeRef, declaring: &str) -> bool {
        if self.ty_named_unprovable(a) {
            return false;
        }
        ty_equiv(a, p) || self.is_subtype(a, p, declaring)
    }

    /// Whether the argument type `a` contains (at any depth) a **named type whose
    /// identity cannot be proved** from the first-wins index — the FQN is declared
    /// by two referenced assemblies, or its namespace had a dropped type (a
    /// same-FQN sibling may have been dropped, so a lone surviving entry need not
    /// be the argument's actual type). A **sealed BCL primitive** (and
    /// `System.Object`) is CLR-unique and always provable; a `Ty::Array` recurses
    /// into its element.
    fn ty_named_unprovable(&self, a: &Ty) -> bool {
        match a {
            Ty::Named(path) => {
                if sealed_canon_path(path).is_some()
                    || path.iter().map(String::as_str).eq(["System", "Object"])
                {
                    return false;
                }
                let Some((name, namespace)) = path.split_last() else {
                    return false;
                };
                // `public_types_named` scans the *full* top-level set (not the
                // first-wins index), so a cross-assembly FQN collision shows as ≥ 2.
                self.public_types_named(namespace, name).len() > 1
                    || self.namespace_has_dropped_type(namespace)
            }
            Ty::Array { elem, .. } => self.ty_named_unprovable(elem),
            _ => false,
        }
    }

    /// Whether the argument named type `a` is a **subtype** of the parameter type
    /// `p` — `p` is a resolvable supertype of `a` in `a`'s base/interface closure
    /// ([`AssemblyEnv::super_types`]). Only named argument types resolve; an
    /// array/tuple/function argument, or a `p` that cannot be resolved to an
    /// entity (generic, nested, absent), declines (`false`, hence a deferral).
    fn is_subtype(&self, a: &Ty, p: &TypeRef, declaring: &str) -> bool {
        let Ty::Named(path) = a else {
            return false;
        };
        let Some(a_handle) = self.lookup_path(path) else {
            return false;
        };
        let Some(p_handle) = self.resolve_super_handle(p, declaring) else {
            return false;
        };
        self.super_types(a_handle).contains(&p_handle)
    }

    /// Resolve a `Ty::Named` dotted path to its interned handle.
    fn lookup_path(&self, path: &[String]) -> Option<EntityHandle> {
        let (name, namespace) = path.split_last()?;
        self.lookup_type(namespace, name, 0)
    }

    /// Resolve a supertype-position parameter [`TypeRef`] to its handle: a
    /// primitive by its canonical name (chiefly `System.Object`, the boxing
    /// target), or a non-generic named type via the assembly-aware
    /// [`AssemblyEnv::resolve_base`].
    fn resolve_super_handle(&self, p: &TypeRef, declaring: &str) -> Option<EntityHandle> {
        match p {
            TypeRef::Primitive(prim) => primitive_canon(*prim).and_then(|c| self.lookup_canon(c)),
            TypeRef::Named { .. } => self.resolve_base(p, declaring),
            _ => None,
        }
    }

    /// Select the **unique committable candidate** from a provably-complete
    /// instance-method group `group` for the ground call arguments `args`, per
    /// the plan's commit keystone (§1): commit `c*` iff `must_apply(c*)` holds
    /// **and** every *other* candidate has `may_apply` false. Since
    /// `must_apply ⟹ may_apply`, that is equivalent to — and checked here as —
    /// "exactly one candidate survives the `may_apply` over-approximation, and
    /// [`AssemblyEnv::must_apply`] affirms it". Then exactly one candidate is
    /// FCS-applicable, FCS picks it, and betterness never runs (so none of its
    /// 14 rules need modelling).
    ///
    /// Returns the winner `(level, idx, method)` — the caller bridges its return
    /// type and records its identity at commit — or `None` (defer) when zero or
    /// ≥ 2 candidates survive `may_apply`, or the lone survivor is not
    /// `must_apply`-affirmed (applicable only via a TDC channel, an omitted
    /// optional, a byref/generic/constructor shape — all §5 deferrals). Each
    /// candidate's declaring assembly name (for canonicalising `assembly: None`
    /// signature references) is read from its own declaring level, so a group
    /// spanning base levels in different assemblies is compared soundly.
    ///
    /// Pure: `may_apply`/`must_apply` are functions of ground types and
    /// metadata, so no `ena` speculation is needed here (the OV-4 snapshot API
    /// is for the later Pass-A/B and generic-inference work).
    pub(crate) fn resolve_overload<'g>(
        &self,
        group: &[(EntityHandle, MemberIndex, &'g MethodLike)],
        args: &[Ty],
    ) -> Option<(EntityHandle, MemberIndex, &'g MethodLike)> {
        let mut survivors = group
            .iter()
            .filter(|(level, _, m)| self.may_apply(m, &self.entity(*level).assembly.name, args));
        let &(level, idx, m) = survivors.next()?;
        // A second `may_apply` survivor means ≥ 2 candidates could be applicable,
        // so FCS would run betterness (which v1 does not model) — defer.
        if survivors.next().is_some() {
            return None;
        }
        // The lone survivor must be provably applicable (the winner affirmation),
        // not merely un-refuted; otherwise it is a §5 deferral shape.
        self.must_apply(m, &self.entity(level).assembly.name, args)
            .then_some((level, idx, m))
    }
}

/// Structural type-equality between an inference [`Ty`] and a metadata
/// [`TypeRef`] — the `typeEquiv` half of affirmation (§4.3). Named/primitive
/// types compare by canonical dotted path (so `Ty::Named(System.Int32)` equals
/// `TypeRef::Primitive(I4)`); a plain vector compares by rank and element.
/// Anything else (generic instantiation, byref, bounded array, tuple/function
/// argument) is *not* affirmed here — conservative, hence a deferral.
fn ty_equiv(a: &Ty, p: &TypeRef) -> bool {
    match a {
        Ty::Named(path) => named_matches(path, p),
        Ty::Array { elem, rank } => matches!(
            p,
            TypeRef::Array { element, rank: r2, sizes, lower_bounds }
                if sizes.is_empty()
                    && lower_bounds.is_empty()
                    && *rank == u32::from(*r2)
                    && ty_equiv(elem, &element.ty)
        ),
        _ => false,
    }
}

/// Whether the metadata type `p` names exactly the dotted path `path`.
fn named_matches(path: &[String], p: &TypeRef) -> bool {
    match p {
        TypeRef::Primitive(prim) => primitive_canon(*prim).is_some_and(|c| path.join(".") == c),
        TypeRef::Named {
            namespace,
            name,
            type_args,
            segment_arities,
            ..
        } if type_args.is_empty() && segment_arities.iter().all(|&a| a == 0) => {
            path.len() == namespace.len() + 1
                && path[..namespace.len()] == namespace[..]
                && &path[namespace.len()] == name
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ty::TyVid;
    use borzoi_assembly::{
        AssemblyIdentity, Entity, EntityKind, MethodSignature, Nullability, NullableType,
        TypeParameter, Variance, Version,
    };
    use proptest::prelude::*;

    /// The single assembly all synthetic entities and candidates belong to, so an
    /// `assembly: None` parameter reference resolves against it (see
    /// [`AssemblyEnv::resolve_base`]). Passed as `declaring` throughout.
    const ASM: &str = "TestAsm";

    // ---- constructors (the model has no `Default`; spell every field once) ----

    fn ident() -> AssemblyIdentity {
        AssemblyIdentity {
            name: ASM.to_owned(),
            version: Version {
                major: 0,
                minor: 0,
                build: 0,
                revision: 0,
            },
            public_key_token: None,
        }
    }

    /// A non-generic same-assembly named reference (`System.Decimal`).
    fn named(ns: &[&str], name: &str) -> TypeRef {
        TypeRef::Named {
            assembly: None,
            namespace: ns.iter().map(|s| (*s).to_owned()).collect(),
            name: name.to_owned(),
            type_args: vec![],
            segment_arities: vec![0],
        }
    }

    fn vector(elem: TypeRef) -> TypeRef {
        TypeRef::Array {
            element: Box::new(NullableType::oblivious(elem)),
            rank: 1,
            sizes: vec![],
            lower_bounds: vec![],
        }
    }

    fn base_param(
        ty: TypeRef,
        default: ParamDefault,
        is_out: bool,
        is_param_array: bool,
    ) -> Parameter {
        Parameter {
            name: None,
            ty,
            is_byref: is_out,
            is_readonly_ref: false,
            is_out,
            default,
            is_param_array,
            nullability: Nullability::Oblivious,
        }
    }

    fn param(ty: TypeRef) -> Parameter {
        base_param(ty, ParamDefault::None, false, false)
    }

    fn opt_param(ty: TypeRef) -> Parameter {
        base_param(ty, ParamDefault::Optional(None), false, false)
    }

    fn out_param(ty: TypeRef) -> Parameter {
        base_param(ty, ParamDefault::None, true, false)
    }

    fn params_param(elem: TypeRef) -> Parameter {
        base_param(vector(elem), ParamDefault::None, false, true)
    }

    fn method(name: &str, params: Vec<Parameter>, ret: TypeRef) -> MethodLike {
        MethodLike {
            name: name.to_owned(),
            access: Access::Public,
            signature: MethodSignature {
                parameters: params,
                return_type: ret,
                return_nullability: Nullability::Oblivious,
            },
            arg_group_count: Some(1),
            is_static: false,
            is_virtual: false,
            is_abstract: false,
            is_final: false,
            is_newslot: false,
            is_hide_by_sig: false,
            is_constructor: false,
            is_extension_method: false,
            augmentation: borzoi_assembly::Augmentation::No,
            module_value: None,
            is_module_value_binding: false,
            generic_parameters: vec![],
            obsolete: None,
            experimental: None,
            sets_required_members: false,
            compiler_feature_required: vec![],
            source_name: None,
            custom_attrs: vec![],
            metadata_token: 0,
            implements: Vec::new(),
            unclassified_impls: Vec::new(),
        }
    }

    fn type_parameter(name: &str) -> TypeParameter {
        TypeParameter {
            name: name.to_owned(),
            variance: Variance::Invariant,
            reference_type_constraint: false,
            value_type_constraint: false,
            default_constructor_constraint: false,
            is_unmanaged: false,
            allows_ref_struct: false,
            nullability: Nullability::Oblivious,
            type_constraints: vec![],
        }
    }

    fn entity(
        ns: &[&str],
        name: &str,
        kind: EntityKind,
        is_struct: bool,
        base: Option<TypeRef>,
        members: Vec<Member>,
    ) -> Entity {
        Entity {
            assembly: ident(),
            namespace: ns.iter().map(|s| (*s).to_owned()).collect(),
            name: name.to_owned(),
            kind,
            access: Access::Public,
            is_sealed: is_struct,
            generic_parameters: vec![],
            base_type: base,
            interfaces: vec![],
            members,
            skipped_members: vec![],
            method_def_tokens: vec![],
            nested_types: vec![],
            is_readonly: false,
            is_byref_like: false,
            is_struct,
            is_auto_open: false,
            is_require_qualified_access: false,
            is_no_equality: false,
            is_no_comparison: false,
            is_structural_equality: false,
            is_structural_comparison: false,
            is_allow_null_literal: false,
            obsolete: None,
            experimental: None,
            default_member: None,
            compiler_feature_required: vec![],
            source_name: None,
            extension_member_names: vec![],
            union_case_names: None,
            static_extension_member_names: Vec::new(),
            is_extension_container: false,
            custom_attrs: vec![],
        }
    }

    /// A `System.` value-type entity (a struct, base `ValueType`).
    fn value(name: &str, members: Vec<Member>) -> Entity {
        entity(
            &["System"],
            name,
            EntityKind::Struct,
            true,
            Some(named(&["System"], "ValueType")),
            members,
        )
    }

    fn op_implicit(from: TypeRef, to: TypeRef) -> Member {
        let mut m = method("op_Implicit", vec![param(from)], to);
        m.is_static = true;
        Member::Method(m)
    }

    /// A mini-BCL: `Object`, `ValueType`, the sealed primitives the generators
    /// draw from, `String`, and `Decimal` — optionally carrying the real
    /// `op_Implicit(int) : Decimal` conversion so the `op_Implicit` channel is
    /// exercised both present and absent.
    fn mini_bcl(decimal_conv: bool) -> AssemblyEnv {
        let decimal_members = if decimal_conv {
            vec![op_implicit(
                TypeRef::Primitive(Primitive::I4),
                named(&["System"], "Decimal"),
            )]
        } else {
            vec![]
        };
        AssemblyEnv::from_entities(vec![
            entity(
                &["System"],
                "Object",
                EntityKind::Class,
                false,
                None,
                vec![],
            ),
            entity(
                &["System"],
                "ValueType",
                EntityKind::Class,
                false,
                Some(named(&["System"], "Object")),
                vec![],
            ),
            value("Int32", vec![]),
            value("Int64", vec![]),
            value("Double", vec![]),
            value("Byte", vec![]),
            value("Char", vec![]),
            value("Boolean", vec![]),
            value("Decimal", decimal_members),
            entity(
                &["System"],
                "String",
                EntityKind::Class,
                false,
                Some(named(&["System"], "Object")),
                vec![],
            ),
        ])
    }

    fn int_ty() -> Ty {
        Ty::named("System.Int32")
    }
    fn str_ty() -> Ty {
        Ty::named("System.String")
    }

    // ---- FCS-grounded probe unit tests (the §3 catalogue verdicts) ----

    #[test]
    fn must_apply_affirms_boxing_subsumption() {
        // P5 `M(obj)`, call `M(3)`: `int :> obj`, so the winner affirms.
        let env = mini_bcl(false);
        let m = method(
            "M",
            vec![param(TypeRef::Primitive(Primitive::Object))],
            int_ty_ref(),
        );
        assert!(env.must_apply(&m, ASM, &[int_ty()]));
        assert!(env.may_apply(&m, ASM, &[int_ty()]));
    }

    #[test]
    fn may_apply_eliminates_when_no_conversion_channel() {
        // P5 `M(string)`, call `M(3)`: no channel `int -> string`, so the loser is
        // refuted — the elimination that lets `M(obj)` commit.
        let env = mini_bcl(false);
        let m = method(
            "M",
            vec![param(TypeRef::Primitive(Primitive::String))],
            int_ty_ref(),
        );
        assert!(!env.may_apply(&m, ASM, &[int_ty()]));
        assert!(!env.must_apply(&m, ASM, &[int_ty()]));
    }

    #[test]
    fn obj_loser_is_never_refuted() {
        // P5 `M("hi")`: the winner `M(string)` affirms, but the `M(obj)` loser
        // cannot be refuted (`obj` is open), so the group DEFERS — the honest
        // price of soundness.
        let env = mini_bcl(false);
        let m_obj = method(
            "M",
            vec![param(TypeRef::Primitive(Primitive::Object))],
            int_ty_ref(),
        );
        let m_str = method(
            "M",
            vec![param(TypeRef::Primitive(Primitive::String))],
            int_ty_ref(),
        );
        assert!(env.must_apply(&m_str, ASM, &[str_ty()]));
        assert!(env.may_apply(&m_obj, ASM, &[str_ty()]));
    }

    #[test]
    fn widening_candidate_stays_may_but_not_must() {
        // P1 `M(float)`, call `M(3)`: applicable to FCS *only via widening*, so
        // `may_apply` keeps it (over-approx) but `must_apply` refuses it (the call
        // defers — a widening-only winner is never affirmed).
        let env = mini_bcl(false);
        let m = method(
            "M",
            vec![param(TypeRef::Primitive(Primitive::R8))],
            int_ty_ref(),
        );
        assert!(env.may_apply(&m, ASM, &[int_ty()]));
        assert!(!env.must_apply(&m, ASM, &[int_ty()]));
    }

    #[test]
    fn arity_prong_refutes_wrong_count() {
        // P4 `s.Substring(1)`: `Substring(int, int)` is refuted by arity; the
        // 1-arg `Substring(int)` affirms.
        let env = mini_bcl(false);
        let two = method(
            "Substring",
            vec![
                param(TypeRef::Primitive(Primitive::I4)),
                param(TypeRef::Primitive(Primitive::I4)),
            ],
            str_ty_ref(),
        );
        let one = method(
            "Substring",
            vec![param(TypeRef::Primitive(Primitive::I4))],
            str_ty_ref(),
        );
        assert!(!env.may_apply(&two, ASM, &[int_ty()]));
        assert!(env.must_apply(&one, ASM, &[int_ty()]));
    }

    #[test]
    fn params_expanded_affirms_and_refutes() {
        // P2 `V(params int[]) / V(string)`.
        let env = mini_bcl(false);
        let v_pa = method(
            "V",
            vec![params_param(TypeRef::Primitive(Primitive::I4))],
            int_ty_ref(),
        );
        let v_str = method(
            "V",
            vec![param(TypeRef::Primitive(Primitive::String))],
            int_ty_ref(),
        );

        // `V(1, 2)` and `V(7)`: the expanded params winner affirms.
        assert!(env.must_apply(&v_pa, ASM, &[int_ty(), int_ty()]));
        assert!(env.must_apply(&v_pa, ASM, &[int_ty()]));
        // and the `V(string)` loser refutes (wrong arity, or no channel).
        assert!(!env.may_apply(&v_str, ASM, &[int_ty(), int_ty()]));
        assert!(!env.may_apply(&v_str, ASM, &[int_ty()]));

        // `V("x")`: `V(string)` affirms; the params candidate is refuted in BOTH
        // its interpretations (direct-array `string ≠ int[]`, expanded
        // `string -> int` no channel), so it does not block the commit.
        assert!(env.must_apply(&v_str, ASM, &[str_ty()]));
        assert!(!env.may_apply(&v_pa, ASM, &[str_ty()]));
    }

    #[test]
    fn omitted_trailing_optional_is_not_eliminated() {
        // Soundness (codex OV-5 P2): `M(int, [<Optional>] int)` called `M(1)` is
        // FCS-applicable by omitting the trailing optional — the count sits inside
        // the arity window. `may_apply` MUST keep it (over-approximation): wrongly
        // refuting an in-window candidate could let a competing `must_apply`
        // overload commit instead of deferring. The same holds for a trailing
        // `out` (folded into the tuple return).
        let env = mini_bcl(false);
        let i4 = || TypeRef::Primitive(Primitive::I4);
        let opt = method("M", vec![param(i4()), opt_param(i4())], int_ty_ref());
        assert!(env.may_apply(&opt, ASM, &[int_ty()]));
        let out = method("M", vec![param(i4()), out_param(i4())], int_ty_ref());
        assert!(env.may_apply(&out, ASM, &[int_ty()]));
        // A *leading* required position is still refuted on type grounds even when
        // a trailing optional is omitted: `M(string, [<Optional>] int)` on `M(1)`.
        let str_opt = method(
            "M",
            vec![
                param(TypeRef::Primitive(Primitive::String)),
                opt_param(i4()),
            ],
            int_ty_ref(),
        );
        assert!(!env.may_apply(&str_opt, ASM, &[int_ty()]));
    }

    #[test]
    fn op_implicit_channel_is_consulted() {
        // P10-flavoured: `M(decimal)`, call `M(3)`. With the real
        // `op_Implicit(int):Decimal` present, the channel exists ⇒ `may_apply`
        // keeps the candidate; absent, it is refuted. `must_apply` refuses it
        // either way (no `typeEquiv`/subsumption `int -> decimal`).
        let m = method(
            "M",
            vec![param(named(&["System"], "Decimal"))],
            int_ty_ref(),
        );

        let with_conv = mini_bcl(true);
        assert!(with_conv.may_apply(&m, ASM, &[int_ty()]));
        assert!(!with_conv.must_apply(&m, ASM, &[int_ty()]));

        let without_conv = mini_bcl(false);
        assert!(!without_conv.may_apply(&m, ASM, &[int_ty()]));
    }

    #[test]
    fn op_implicit_conservative_when_operator_skipped() {
        // Soundness (codex OV-5 round 2): the reader may drop an undecodable
        // `op_Implicit` into `skipped_members` rather than `members`. Scanning
        // only the decoded members would then "prove" no conversion and wrongly
        // eliminate the candidate — so a skipped `op_Implicit` on either entity
        // must conservatively KEEP it.
        use borzoi_assembly::SkippedMember;
        let mut decimal = value("Decimal", vec![]); // no *decoded* op_Implicit
        decimal.skipped_members.push(SkippedMember {
            name: "op_Implicit".to_owned(),
            reason: "test: undecodable conversion signature".to_owned(),
        });
        let env = AssemblyEnv::from_entities(vec![value("Int32", vec![]), decimal]);
        let m = method(
            "M",
            vec![param(named(&["System"], "Decimal"))],
            int_ty_ref(),
        );
        assert!(env.may_apply(&m, ASM, &[int_ty()]));
    }

    #[test]
    fn op_implicit_conservative_when_target_absent() {
        // Soundness of the absence branch: if the target entity is not in the env,
        // we cannot prove no conversion exists, so `may_apply` must KEEP the
        // candidate (never a wrong elimination). `Decimal` is absent here.
        let env = AssemblyEnv::from_entities(vec![value("Int32", vec![])]);
        let m = method(
            "M",
            vec![param(named(&["System"], "Decimal"))],
            int_ty_ref(),
        );
        assert!(env.may_apply(&m, ASM, &[int_ty()]));
    }

    #[test]
    fn generic_and_byref_winners_defer() {
        let env = mini_bcl(false);
        // A generic method never affirms in v1.
        let mut generic = method(
            "M",
            vec![param(TypeRef::Primitive(Primitive::I4))],
            int_ty_ref(),
        );
        generic.generic_parameters = vec![type_parameter("T")];
        assert!(!env.must_apply(&generic, ASM, &[int_ty()]));
        // A byref/out parameter in the winner defers.
        let byref = method(
            "M",
            vec![out_param(TypeRef::Primitive(Primitive::I4))],
            int_ty_ref(),
        );
        assert!(!env.must_apply(&byref, ASM, &[int_ty()]));
        // A constructor is not a value-receiver call.
        let mut ctor = method(
            ".ctor",
            vec![param(TypeRef::Primitive(Primitive::I4))],
            int_ty_ref(),
        );
        ctor.is_constructor = true;
        assert!(!env.must_apply(&ctor, ASM, &[int_ty()]));
    }

    #[test]
    fn arity_windows() {
        let i4 = || TypeRef::Primitive(Primitive::I4);
        // Plain: exactly the param count.
        assert_eq!(
            arity_window(&method("M", vec![param(i4())], int_ty_ref())),
            ArityWindow {
                min: 1,
                max: Some(1)
            }
        );
        // Trailing optional: min drops by one.
        assert_eq!(
            arity_window(&method(
                "M",
                vec![param(i4()), opt_param(i4())],
                int_ty_ref()
            )),
            ArityWindow {
                min: 1,
                max: Some(2)
            }
        );
        // Trailing out: omittable (folded into the tuple return).
        assert_eq!(
            arity_window(&method(
                "M",
                vec![param(i4()), out_param(i4())],
                int_ty_ref()
            )),
            ArityWindow {
                min: 1,
                max: Some(2)
            }
        );
        // Param array: max is ∞, and it may take zero elements.
        assert_eq!(
            arity_window(&method("M", vec![params_param(i4())], int_ty_ref())),
            ArityWindow { min: 0, max: None }
        );
        assert_eq!(
            arity_window(&method(
                "M",
                vec![param(i4()), params_param(i4())],
                int_ty_ref()
            )),
            ArityWindow { min: 1, max: None }
        );
        // Both optional AND out breaks the trailing run (§2.2 landmine): the
        // param is neither cleanly omittable, so min does not drop.
        let both = base_param(i4(), ParamDefault::Optional(None), true, false);
        assert_eq!(
            arity_window(&method("M", vec![param(i4()), both], int_ty_ref())),
            ArityWindow {
                min: 2,
                max: Some(2)
            }
        );
    }

    // convenience returns for the builders above
    fn int_ty_ref() -> TypeRef {
        TypeRef::Primitive(Primitive::I4)
    }
    fn str_ty_ref() -> TypeRef {
        TypeRef::Primitive(Primitive::String)
    }

    // ---- instance_method_group: the complete candidate set (§4.1) ----

    #[test]
    fn instance_method_group_surfaces_the_overload_set() {
        // A class with two same-name overloads and a distinct single method: the
        // group query surfaces *both* overloads (≥ 2 — the OV-6 engine's input),
        // where the single-candidate `instance_method` wrapper would decline.
        let object = entity(
            &["System"],
            "Object",
            EntityKind::Class,
            false,
            None,
            vec![],
        );
        let c = entity(
            &["Test"],
            "C",
            EntityKind::Class,
            false,
            Some(named(&["System"], "Object")),
            vec![
                Member::Method(method("M", vec![param(int_ty_ref())], str_ty_ref())),
                Member::Method(method(
                    "M",
                    vec![param(int_ty_ref()), param(int_ty_ref())],
                    str_ty_ref(),
                )),
                Member::Method(method("Solo", vec![], int_ty_ref())),
            ],
        );
        let env = AssemblyEnv::from_entities(vec![object, c]);
        let h = env.lookup_type(&["Test".to_owned()], "C", 0).unwrap();

        let group = env
            .instance_method_group(h, "M")
            .expect("a complete group for M");
        assert_eq!(group.len(), 2, "both M overloads are surfaced");
        assert!(
            env.instance_method(h, "M").is_none(),
            "the single-candidate wrapper declines a genuine overload set"
        );

        let solo = env
            .instance_method_group(h, "Solo")
            .expect("a complete group for Solo");
        assert_eq!(solo.len(), 1, "a single method is a group of one");
        assert!(
            env.instance_method(h, "Solo").is_some(),
            "the wrapper resolves the single candidate"
        );
    }

    // ---- resolve_overload: the commit keystone over a group (§1) ----

    #[test]
    fn resolve_overload_commits_the_unique_survivor() {
        // P4-shape group `Substring(int)` / `Substring(int, int)`, called with one
        // `int`: the 2-arg overload is arity-refuted, leaving the 1-arg overload the
        // unique `may_apply` survivor, which `must_apply` affirms — so the keystone
        // commits it. (Membership is by declared index here — `MemberIndex(0)`.)
        let env = mini_bcl(false);
        let h = env
            .lookup_type(&["System".to_owned()], "String", 0)
            .unwrap();
        let m1 = method("Substring", vec![param(int_ty_ref())], str_ty_ref());
        let m2 = method(
            "Substring",
            vec![param(int_ty_ref()), param(int_ty_ref())],
            str_ty_ref(),
        );
        let group = vec![(h, MemberIndex::new(0), &m1), (h, MemberIndex::new(1), &m2)];
        let (_, idx, _) = env
            .resolve_overload(&group, &[int_ty()])
            .expect("a unique arity-surviving candidate commits");
        assert_eq!(idx.index(), 0, "the one-argument overload is chosen");
    }

    #[test]
    fn resolve_overload_defers_with_two_survivors() {
        // P5 `M("hi")`: `M(obj)` and `M(string)` are BOTH applicable to a `string`
        // argument (`obj` is open — un-refutable), so two candidates survive
        // `may_apply` and the keystone defers (FCS would run betterness, unmodelled).
        let env = mini_bcl(false);
        let h = env
            .lookup_type(&["System".to_owned()], "String", 0)
            .unwrap();
        let m_obj = method(
            "M",
            vec![param(TypeRef::Primitive(Primitive::Object))],
            int_ty_ref(),
        );
        let m_str = method("M", vec![param(str_ty_ref())], int_ty_ref());
        let group = vec![
            (h, MemberIndex::new(0), &m_obj),
            (h, MemberIndex::new(1), &m_str),
        ];
        assert!(
            env.resolve_overload(&group, &[str_ty()]).is_none(),
            "two may_apply survivors defer (betterness would run)"
        );
    }

    #[test]
    fn resolve_overload_defers_a_survivor_without_must_apply() {
        // A lone `may_apply` survivor that is applicable *only via widening* is not
        // `must_apply`-affirmed, so the keystone defers rather than commit a
        // TDC-only winner: `M(float)` called with `int` — `may_apply` keeps it
        // (widening), `must_apply` refuses it, and no other candidate refutes.
        let env = mini_bcl(false);
        let h = env
            .lookup_type(&["System".to_owned()], "String", 0)
            .unwrap();
        let m = method(
            "M",
            vec![param(TypeRef::Primitive(Primitive::R8))],
            int_ty_ref(),
        );
        let group = vec![(h, MemberIndex::new(0), &m)];
        assert!(
            env.resolve_overload(&group, &[int_ty()]).is_none(),
            "a widening-only survivor is not affirmed, so the group defers"
        );
    }

    #[test]
    fn must_apply_declines_an_ambiguous_named_argument() {
        // OV-6 review (GPT-5.6): a bare `Ty::Named` lost its assembly, and the env's
        // first-wins index collapses a colliding FQN. When two referenced assemblies
        // both declare `N.X`, an `A::N.X` argument must not affirm a `B::N.X`
        // parameter — `must_apply` declines. A *uniquely*-named argument still
        // affirms (control).
        let object = entity(
            &["System"],
            "Object",
            EntityKind::Class,
            false,
            None,
            vec![],
        );
        let x_a = entity(
            &["N"],
            "X",
            EntityKind::Class,
            false,
            Some(named(&["System"], "Object")),
            vec![],
        );
        // A second `N.X` (a colliding FQN — here a second declaration in the env).
        let x_b = x_a.clone();
        let uniq = entity(
            &["N"],
            "Y",
            EntityKind::Class,
            false,
            Some(named(&["System"], "Object")),
            vec![],
        );
        let env = AssemblyEnv::from_entities(vec![object, x_a, x_b, uniq]);

        let m_x = method("M", vec![param(named(&["N"], "X"))], int_ty_ref());
        assert!(
            !env.must_apply(&m_x, ASM, &[Ty::named("N.X")]),
            "an ambiguous-FQN named argument cannot affirm (identity unprovable)"
        );
        let m_y = method("M", vec![param(named(&["N"], "Y"))], int_ty_ref());
        assert!(
            env.must_apply(&m_y, ASM, &[Ty::named("N.Y")]),
            "a uniquely-named argument still affirms by type-equality"
        );

        // The ambiguity guard is **recursive**: an ambiguous named type nested in a
        // `Ty::Array` (`N.X[]`) must also decline — `ty_equiv` would otherwise
        // compare element paths with no assembly identity.
        let m_x_arr = method("M", vec![param(vector(named(&["N"], "X")))], int_ty_ref());
        let arg_arr = Ty::Array {
            elem: Box::new(Ty::named("N.X")),
            rank: 1,
        };
        assert!(
            !env.must_apply(&m_x_arr, ASM, std::slice::from_ref(&arg_arr)),
            "an array of an ambiguous named type cannot affirm either"
        );
    }

    #[test]
    fn auto_open_module_defers_only_the_names_it_declares() {
        // EX-1: an auto-open surface is no longer a wholesale defer. An in-scope
        // extension competes only within its **own name's** group (probed —
        // `docs/extension-scope-enumeration-plan.md` §1), so an auto-opened module
        // that declares `Twice` defers a call to `Twice` and *nothing else*. This is
        // the whole coverage refinement: under the old presence gate, FSharp.Core's
        // implicit auto-opens made every project defer every overloaded call.
        let plain = AssemblyEnv::from_entities(vec![entity(
            &["Ext"],
            "Helpers",
            EntityKind::Class,
            false,
            None,
            vec![method("Foo", vec![], int_ty_ref())]
                .into_iter()
                .map(Member::Method)
                .collect(),
        )]);
        assert!(
            !plain.extension_named_in_scope(&[], "Twice", false),
            "a plain env with no auto-open surface contributes no extension of any name"
        );

        // A module-shaped assembly `[<AutoOpen>]` path, declaring one *instance*
        // extension (`Twice`) and one *static* extension (`Make`).
        let mut helpers = entity(&["Ext"], "Helpers", EntityKind::Module, false, None, vec![]);
        helpers.extension_member_names = vec!["Twice".to_string()];
        helpers.static_extension_member_names = vec!["Make".to_string()];
        let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![(
            std::path::PathBuf::from("test.dll"),
            vec![helpers],
            crate::AbbreviationVisibility::Modelled,
            vec!["Ext.Helpers".to_string()], // the assembly-level AutoOpen path
        )]);
        assert!(
            env.extension_named_in_scope(&[], "Twice", false),
            "the auto-opened module's instance extension defers a call of ITS name"
        );
        assert!(
            !env.extension_named_in_scope(&[], "Substring", false),
            "…and no other name — the surface's mere presence is not a reason to defer"
        );
        // The two indexes are consulted by call shape, exactly as FCS selects them:
        // a value receiver's group takes only instance extensions, a type-qualified
        // call's only static ones (EX-0).
        assert!(
            env.extension_named_in_scope(&[], "Make", true),
            "the static extension defers a STATIC call of its name"
        );
        assert!(
            !env.extension_named_in_scope(&[], "Make", false),
            "…but not an instance call of that name (a static extension is not in a \
             value receiver's group)"
        );
        assert!(
            !env.extension_named_in_scope(&[], "Twice", true),
            "…and the instance extension is not in a static call's group"
        );
    }

    #[test]
    fn unresolvable_auto_open_path_is_an_unknowable_surface() {
        // OV-6 review (GPT-5.6): a successfully-read AutoOpen path resolving to
        // neither a module nor a namespace in the env (its target may have been a
        // dropped type) leaves the extension surface unknowable, so the gate defers.
        let helpers = entity(&["Ext"], "Helpers", EntityKind::Class, false, None, vec![]);
        let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![(
            std::path::PathBuf::from("test.dll"),
            vec![helpers],
            crate::AbbreviationVisibility::Modelled,
            vec!["Nonexistent.Target".to_string()],
        )]);
        assert!(
            env.extension_named_in_scope(&[], "AnyName", false),
            "an unresolvable auto-open path leaves the surface UNKNOWABLE, so it defers \
             every name — the name-keyed gate (EX-1) narrows the *knowable* sources only"
        );
    }

    #[test]
    fn auto_open_assembly_with_no_surviving_roots_is_unknowable() {
        // OV-6 review (GPT-5.6): an assembly whose types were all dropped (no
        // surviving roots) but which declares `[<AutoOpen>]` targets leaves its
        // extension surface unknowable — nothing survives to resolve them against.
        let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![(
            std::path::PathBuf::from("empty.dll"),
            vec![],
            crate::AbbreviationVisibility::Modelled,
            vec!["Some.AutoOpen".to_string()],
        )]);
        assert!(
            env.extension_named_in_scope(&[], "AnyName", false),
            "an auto-open assembly with no surviving roots is unknowable — every name defers"
        );
    }

    // ---- property tests: the refuter is where "no channel exists" lives ----

    /// A sealed-primitive parameter type the generators draw from (plus `obj`,
    /// which is *not* closed — it exercises the open-target path).
    fn prim_pool() -> Vec<Primitive> {
        vec![
            Primitive::I4,
            Primitive::I8,
            Primitive::R8,
            Primitive::U1,
            Primitive::Char,
            Primitive::Bool,
            Primitive::String,
            Primitive::Object,
        ]
    }

    fn arb_prim() -> impl Strategy<Value = Primitive> {
        prop::sample::select(prim_pool())
    }

    /// A parameter type: a primitive, `System.Decimal` (named), or a vector.
    fn arb_param_type() -> impl Strategy<Value = TypeRef> {
        prop_oneof![
            arb_prim().prop_map(TypeRef::Primitive),
            Just(named(&["System"], "Decimal")),
            arb_prim().prop_map(|p| vector(TypeRef::Primitive(p))),
        ]
    }

    /// A ground closed argument type, plus an occasional non-ground `Var` (to
    /// exercise the `must_apply` groundness gate) and `obj`.
    fn arb_arg() -> impl Strategy<Value = Ty> {
        prop_oneof![
            8 => arb_prim().prop_map(|p| Ty::named(primitive_canon(p).unwrap_or("System.Object"))),
            2 => arb_prim().prop_map(|p| Ty::Array {
                elem: Box::new(Ty::named(primitive_canon(p).unwrap_or("System.Object"))),
                rank: 1,
            }),
            1 => (0u32..4).prop_map(|v| Ty::Var(TyVid(v))),
        ]
    }

    #[derive(Debug, Clone, Copy)]
    enum Kind {
        Plain,
        Optional,
        Out,
    }

    fn arb_kind() -> impl Strategy<Value = Kind> {
        prop_oneof![Just(Kind::Plain), Just(Kind::Optional), Just(Kind::Out)]
    }

    /// A candidate method: 0..3 parameters (each plain/optional/out), the last
    /// optionally promoted to a param array. Never generic/constructor (those are
    /// unit-tested); the point is to stress the arity + type prongs.
    fn arb_method() -> impl Strategy<Value = MethodLike> {
        (
            prop::collection::vec((arb_param_type(), arb_kind()), 0..3),
            any::<bool>(),
        )
            .prop_map(|(specs, param_array)| {
                let mut params: Vec<Parameter> = specs
                    .into_iter()
                    .map(|(ty, kind)| match kind {
                        Kind::Plain => param(ty),
                        Kind::Optional => opt_param(ty),
                        Kind::Out => out_param(ty),
                    })
                    .collect();
                if param_array && let Some(last) = params.last_mut() {
                    *last = params_param(TypeRef::Primitive(Primitive::I4));
                }
                method("M", params, TypeRef::Primitive(Primitive::I4))
            })
    }

    proptest! {
        /// The keystone relationship: `must_apply` is an *under*-approximation and
        /// `may_apply` an *over*-approximation of the SAME predicate, so
        /// everything the winner-affirmer accepts the candidate-eliminator must
        /// also keep. A violation would mean the engine could refute the very
        /// candidate it just affirmed — an outright contradiction.
        #[test]
        fn must_implies_may(m in arb_method(), args in prop::collection::vec(arb_arg(), 0..4)) {
            let env = mini_bcl(true);
            if env.must_apply(&m, ASM, &args) {
                prop_assert!(env.may_apply(&m, ASM, &args));
            }
        }

        /// Identity is a conversion channel: a single-parameter candidate with a
        /// closed primitive parameter is never eliminated when called with an
        /// argument of that exact type.
        #[test]
        fn identity_is_never_eliminated(p in arb_prim()) {
            let env = mini_bcl(true);
            let m = method("M", vec![param(TypeRef::Primitive(p))], TypeRef::Primitive(Primitive::I4));
            let arg = Ty::named(primitive_canon(p).unwrap());
            prop_assert!(env.may_apply(&m, ASM, &[arg]));
        }

        /// The built-in widenings are channels: an `int32` argument is never
        /// eliminated against an `int64` / `nativeint` / `float` parameter.
        #[test]
        fn widening_is_never_eliminated(to in prop::sample::select(vec![
            Primitive::I8, Primitive::IntPtr, Primitive::R8,
        ])) {
            let env = mini_bcl(true);
            let m = method("M", vec![param(TypeRef::Primitive(to))], TypeRef::Primitive(Primitive::I4));
            prop_assert!(env.may_apply(&m, ASM, &[int_ty()]));
        }

        /// `must_apply` only ever affirms at an in-window argument count — the
        /// arity prong can never refute a candidate the affirmer accepts.
        #[test]
        fn must_apply_is_within_the_arity_window(
            m in arb_method(),
            args in prop::collection::vec(arb_arg(), 0..4),
        ) {
            let env = mini_bcl(true);
            if env.must_apply(&m, ASM, &args) {
                prop_assert!(arity_window(&m).contains(args.len()));
            }
        }

        /// The over-approximation contract, isolated: a candidate whose every
        /// parameter is *open* (`obj`, incl. an `obj[]` param array) has no
        /// type-refutable position, so `may_apply` must reduce to pure arity-window
        /// membership — never eliminating an in-window count. Directly guards the
        /// omitted-trailing-optional soundness bug (an in-window `M(obj, ?obj)`
        /// called `M(x)` must survive).
        #[test]
        fn open_candidate_is_eliminated_iff_out_of_window(
            kinds in prop::collection::vec(arb_kind(), 0..4),
            param_array in any::<bool>(),
            args in prop::collection::vec(arb_arg(), 0..5),
        ) {
            let env = mini_bcl(true);
            let obj = || TypeRef::Primitive(Primitive::Object);
            let mut params: Vec<Parameter> = kinds
                .into_iter()
                .map(|kind| match kind {
                    Kind::Plain => param(obj()),
                    Kind::Optional => opt_param(obj()),
                    Kind::Out => out_param(obj()),
                })
                .collect();
            if param_array && let Some(last) = params.last_mut() {
                *last = params_param(obj());
            }
            let m = method("M", params, TypeRef::Primitive(Primitive::I4));
            prop_assert_eq!(
                env.may_apply(&m, ASM, &args),
                arity_window(&m).contains(args.len())
            );
        }

        /// Structural invariant of the window: `min` never exceeds the declared
        /// count, and a finite `max` is exactly the declared count.
        #[test]
        fn arity_window_is_well_formed(m in arb_method()) {
            let w = arity_window(&m);
            let n = m.signature.parameters.len();
            prop_assert!(w.min <= n);
            if let Some(max) = w.max {
                prop_assert_eq!(max, n);
                prop_assert!(w.min <= max);
            }
        }
    }
}
