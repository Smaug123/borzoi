//! ECMA-335 II.7.1.1 as a *metamorphic* property over the projector.
//!
//! The custom-modifier rule is a statement about two projections of the same
//! program, which makes it mechanically checkable against any real assembly
//! without a single hand-written expectation:
//!
//! > *An optional modifier may be ignored by a tool that does not understand
//! > it; a required one must be understood.* (paraphrased)
//!
//! Read as a transformation on a decoded image, that is two properties:
//!
//! * **P1 — an ignorable `modopt` is inert.** Decorate *every* node of *every*
//!   signature in the image with a `modopt` naming a type the projector has
//!   never heard of. The projection — the whole `Entity` tree and the whole
//!   skip record, byte for byte — must be unchanged.
//! * **P2 — an unrecognised `modreq` is fatal.** Do the same with a `modreq`.
//!   Every member is now unprojectable, so *no* member may survive: a survivor
//!   is a signature position that reached the model without passing through the
//!   modifier policy at all.
//!
//! Why this shape. Custom-modifier support was *first* added as a
//! `TypeSig::Modified` wrapper variant — a modifier as a node in front of the
//! type — and a wrapper variant is the one kind of enum growth the compiler
//! cannot police: every pre-existing `matches!(sig, TypeSig::ByRef(_))`
//! head-shape guard stayed well-typed while silently changing meaning, because
//! the byref it guarded against now hid one node down. Five such guards broke;
//! four were found one at a time in review, the fifth by this probe.
//!
//! The encoding has since moved the run *beside* the type (`ModifiedType`, which
//! is what ECMA-335's grammar says: `CustomMod* Type` is a prefix on a
//! **position**), so that particular bug is now unrepresentable rather than
//! merely detectable. These properties remain the check on everything the type
//! cannot state: that the *policy* — drop an unrecognised `modopt` (P1), refuse
//! an unrecognised `modreq` (P2) — is actually applied at **every** position the
//! model consumes, including ones no compiler emits.
//!
//! **The corpus is part of the property.** A real assembly only exercises the
//! guards a real compiler can provoke, and several of the projector's
//! modifier-sensitive guards are *defensive*: they refuse shapes no compiler
//! emits (a byref event type, a byref F# record field). Decorating a stock BCL
//! would leave exactly those untested — which is how two of them were shipped
//! broken and found by review. So the probe can pre-transform the image into a
//! [`HostileShape`] first, and then assert P1 over *that*: the baseline refuses
//! the shape, and the `modopt`-decorated image must refuse it identically. A
//! guard that inspects the head of a signature without peeling accepts the
//! decorated one, and P1 catches it. The transforms are model-level, so no
//! metadata emitter can bottleneck what we can forge.
//!
//! Both probes decorate the *decoded* `TypeSig` rather than re-encoding a
//! blob, so they need no metadata emitter — the corpus is whatever assemblies
//! the caller already has (the fixtures, the reference pack, the shared
//! runtime). Every decorated position is one the decoder itself can produce: a
//! `CustomMod` run is accepted at the head of any `Type` (II.23.2.12 spells one
//! out for `PTR`/`SZARRAY`; C++/CLI emits them inside generic arguments; and
//! `reader::signature` accepts one wherever a `Type` starts), so a divergence
//! here is a state some real or forged image can hand the projector, not a
//! state that exists only in the test.
//!
//! Gated behind the `test-support` feature: it reaches into the reader's
//! in-crate model, and no runtime consumer wants it.

use crate::reader::{
    CustomMod, Event, Field, GenericParam, Image, InterfaceMemberImpl, MemberRef, Method,
    MethodSig, ModifiedType, Param, Property, RetType, SigError, TypeConstraint, TypeDef,
    TypeScope, TypeSig, UnclassifiedImpl,
};
use crate::{Ecma335Assembly, EcmaView, Entity, ImportError, Member};

/// What one probe saw. `findings` is empty on success; each entry names a
/// position and the way the projection moved, so a sweep over many assemblies
/// can aggregate rather than panicking on the first one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutcome {
    /// Types kept by the *undecorated* projection (nested types included).
    pub baseline_types: usize,
    /// Members kept by the *undecorated* projection. A probe over an assembly
    /// with no members proves nothing; callers assert this is non-zero.
    pub baseline_members: usize,
    /// Refusals recorded by the **undecorated** projection: dropped types plus
    /// skipped members.
    ///
    /// For a [`HostileShape`] this is the load-bearing number, and it is an
    /// *absolute* expectation, not a metamorphic one. P1 says the two
    /// projections agree — and two projections that both wrongly *accept* a
    /// byref field agree perfectly. Invariance cannot see a guard that is gone;
    /// only "the baseline refused this" can. (It did not, once: the byref
    /// synthetic-field guard was deleted in a botched edit and every metamorphic
    /// test still passed.)
    pub baseline_refusals: usize,
    /// The decorated projection refused the *whole assembly* rather than
    /// enumerating it.
    ///
    /// Only P2 can see this, and for P2 it is a **pass**: refusing outright is
    /// the strongest possible way to not-ignore a `modreq`. It happens on
    /// F#-kinded assemblies, where the decoration also lands on the
    /// `FSharpInterfaceDataVersionAttribute` constructor — an assembly whose
    /// F#-ness marker will not decode is deliberately refused rather than
    /// mis-classified as C# (`Ecma335Assembly::fsharp_interface_data_version`),
    /// and that stance is not something a synthetic input should be allowed to
    /// erode. It does make P2 vacuous *for that assembly*: the member-level
    /// survivor check has its teeth in the C# corpus (the reference pack), which
    /// enumerates normally under the same decoration.
    pub refused_wholesale: bool,
    /// Empty iff the property held.
    pub findings: Vec<String>,
}

/// A structural mutation applied to the decoded image *before* the modifier
/// decoration, to reach the projector's defensive guards.
///
/// Each of these makes a signature into a shape the projector is supposed to
/// refuse. Refusing it is not what is being tested here — the fail-loud tests
/// own that. What is being tested is that the refusal is *stable under an
/// ignorable modifier*: a guard written as `matches!(sig, TypeSig::ByRef(_))`
/// stops firing the moment a modifier sits in front of the byref, and the
/// member it was supposed to refuse gets projected instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostileShape {
    /// The image as it came off disk.
    None,
    /// Every event's delegate type becomes `T&`. Guards `project_event`.
    ByRefEventTypes,
    /// Every field's type becomes `T&`. Guards the field projection (and, on an
    /// F#-kinded image, the byref check in the F# overlay).
    ByRefFieldTypes,
    /// Every property's type becomes `T&`. Guards `project_property` and — on an
    /// F#-kinded image, where a record/exception logical field *is* a property
    /// carrying the field flag — `property_as_synthetic_field`.
    ByRefPropertyTypes,
}

impl HostileShape {
    /// Every shape, for a caller that wants to sweep them all.
    pub const ALL: [HostileShape; 4] = [
        HostileShape::None,
        HostileShape::ByRefEventTypes,
        HostileShape::ByRefFieldTypes,
        HostileShape::ByRefPropertyTypes,
    ];
}

/// Which signature positions a probe decorates.
///
/// Saturating *everything* is the strong, cheap check — but it is also a blunt
/// one: when a modifier lands on a type's base, its interfaces *and* its
/// constraints at once, the type drops for whichever reason fires first, and a
/// hole in one of the other paths is masked. So a probe can also decorate a
/// single kind of position and pin exactly what must happen to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Every position in the image.
    Everything,
    /// Only the runs on *member* signatures: fields, properties, event types, and
    /// method returns/parameters. Nothing a *type* consumes (base, interfaces,
    /// constraints) is touched.
    ///
    /// Without this, the saturating probe's member check is largely vacuous: a
    /// decorated `extends` refuses nearly every concrete type before a single
    /// member is projected, so "no member survived" would hold even if every
    /// member-position modifier check were broken. Leaving the type header alone
    /// lets the type project, and forces each member path to refuse on its own.
    Members,
    /// Only the runs on generic-parameter constraints.
    ///
    /// This is the one path where a `modreq` is recognised *positionally* — the
    /// `unmanaged` marker, which the projector consumes rather than surfaces —
    /// so it is the one path where consuming a modifier could silently take an
    /// unrecognised one down with it. Under saturation the owning type drops
    /// anyway (its base type is decorated too) and the hole stays invisible.
    Constraints,
}

/// **P1**: decorating every signature node with an unrecognised `modopt` must
/// not move the projection at all.
///
/// The modifier names a type the projector has no policy for, which is the
/// whole point: II.7.1.1 licenses dropping exactly those.
pub fn modopt_saturation_is_inert(bytes: &[u8]) -> Result<ProbeOutcome, ImportError> {
    modopt_saturation_is_inert_on(bytes, HostileShape::None)
}

/// **P1**, over an image first mutated into `shape` — see [`HostileShape`].
///
/// The comparison is still baseline-vs-decorated *of the mutated image*, so the
/// (usually refusing) projection of the hostile shape is the expectation. This
/// needs no oracle: the image is its own control.
pub fn modopt_saturation_is_inert_on(
    bytes: &[u8],
    shape: HostileShape,
) -> Result<ProbeOutcome, ImportError> {
    probe(bytes, false, shape, Scope::Everything)
}

/// **P2**: decorating every signature node with an unrecognised `modreq` must
/// leave no member standing.
///
/// A member that survives has a signature position the projector consumed
/// without applying the modifier policy to it — the modifier was silently
/// dropped, and a `modreq` may not be. (`volatile`, a read-only ref, or a
/// C++/CLI calling-convention marker would then be invisible in the model.)
pub fn unknown_modreq_saturation_refuses(bytes: &[u8]) -> Result<ProbeOutcome, ImportError> {
    probe(bytes, true, HostileShape::None, Scope::Everything)
}

/// **P2, targeted at generic-parameter constraints**: an unrecognised `modreq`
/// on a *constraint* must refuse whatever declares it — the type, for a
/// type-level typar; the method, for a method-level one.
///
/// Nothing else is decorated, so nothing else can drop the type first. Without
/// this, a constraint path that consumes the `unmanaged` marker and discards an
/// unrecognised `modreq` alongside it passes the saturating probe untouched
/// (verified: it does).
pub fn unknown_modreq_on_constraints_refuses(bytes: &[u8]) -> Result<ProbeOutcome, ImportError> {
    probe(bytes, true, HostileShape::None, Scope::Constraints)
}

/// **P2, targeted at member signatures**: an unrecognised `modreq` on a field,
/// property, event, return, or parameter type must refuse that member — with the
/// enclosing type's own signatures left undecorated, so the type projects and
/// cannot mask the member paths.
///
/// This is where the survivor check has its teeth. Under whole-image saturation a
/// decorated `extends` refuses nearly every concrete type up front, and "no
/// member survived" would hold even if every member path silently ignored the
/// modifier.
pub fn unknown_modreq_on_members_refuses(bytes: &[u8]) -> Result<ProbeOutcome, ImportError> {
    probe(bytes, true, HostileShape::None, Scope::Members)
}

fn probe(
    bytes: &[u8],
    required: bool,
    shape: HostileShape,
    scope: Scope,
) -> Result<ProbeOutcome, ImportError> {
    let mut baseline_asm = Ecma335Assembly::parse(bytes)?;
    make_hostile(baseline_asm.image_mut(), shape);
    let (baseline, baseline_skips) = baseline_asm.enumerate_type_defs_with_skips()?;
    let baseline_types = count_types(&baseline);
    let baseline_members = count_members(&baseline);
    let baseline_refusals = baseline_skips.dropped_types.len() + count_member_skips(&baseline);

    let modifier = CustomMod {
        required,
        modifier: unrecognised_modifier(&baseline_asm),
    };
    let mut decorated_asm = baseline_asm.clone();
    decorate_image(decorated_asm.image_mut(), modifier, scope);

    let mut findings = Vec::new();
    let mut refused_wholesale = false;
    match decorated_asm.enumerate_type_defs_with_skips() {
        Err(e) => {
            refused_wholesale = true;
            // Under P1 this is a divergence — an ignorable modifier sank the
            // assembly. Under P2 it is the refusal the property demands (see
            // `ProbeOutcome::refused_wholesale`).
            if !required {
                findings.push(format!("decorated enumeration failed outright: {e}"));
            }
        }
        Ok((decorated, decorated_skips)) => {
            if required && scope == Scope::Members {
                findings.extend(surviving_members(&decorated).into_iter().map(|m| {
                    format!("member `{m}` survived an unrecognised `modreq` on its signature")
                }));
            } else if required && scope == Scope::Constraints {
                findings.extend(surviving_constraint_declarers(&baseline, &decorated));
            } else if required {
                let survivors = surviving_members(&decorated);
                findings.extend(survivors.into_iter().map(|m| {
                    format!("member `{m}` survived an unrecognised `modreq` on its signature")
                }));
                findings.extend(surviving_signature_consumers(&baseline, &decorated));
            } else {
                diff_entities(&baseline, &decorated, "", &mut findings);
                if baseline_skips != decorated_skips {
                    findings.push(format!(
                        "assembly-level skips moved under `modopt` decoration:\n  \
                         baseline:  {baseline_skips:?}\n  decorated: {decorated_skips:?}"
                    ));
                }
            }
        }
    }

    Ok(ProbeOutcome {
        baseline_types,
        baseline_members,
        baseline_refusals,
        refused_wholesale,
        findings,
    })
}

/// A `TypeScope` naming a type the projector has no modifier policy for.
///
/// The `<Module>` pseudo-type (`TypeDef` rid 1, always present) is the one
/// name every image has and no compiler ever uses as a modifier, so it is
/// unrecognised by construction — no need to search the `TypeRef` table for a
/// name that happens not to be `IsVolatile`/`InAttribute`/`IsUnmanaged`.
fn unrecognised_modifier(_asm: &Ecma335Assembly) -> TypeScope {
    TypeScope::Definition(crate::reader::TypeDefId(0))
}

// ---------------------------------------------------------------------------
// Hostile shapes
// ---------------------------------------------------------------------------

/// Rewrite the image into `shape` — see [`HostileShape`].
fn make_hostile(image: &mut Image, shape: HostileShape) {
    if shape == HostileShape::None {
        return;
    }
    for td in &mut image.type_defs {
        match shape {
            HostileShape::None => {}
            HostileShape::ByRefEventTypes => {
                for e in &mut td.events {
                    by_ref(&mut e.event_type);
                }
            }
            HostileShape::ByRefFieldTypes => {
                for f in &mut td.fields {
                    by_ref(&mut f.signature);
                }
            }
            HostileShape::ByRefPropertyTypes => {
                for p in &mut td.properties {
                    by_ref(&mut p.signature);
                }
            }
        }
    }
}

/// `T` becomes `T&`, keeping the position's own modifier run outermost. A
/// byref-to-byref is malformed, so an already-byref signature is left alone (the
/// projector refuses that for a different reason, and the probe wants the guard
/// under test, not its neighbour).
fn by_ref(sig: &mut Result<ModifiedType, SigError>) {
    let Ok(mt) = sig else { return };
    if matches!(mt.ty, TypeSig::ByRef(_)) {
        return;
    }
    let referent = std::mem::replace(&mut mt.ty, TypeSig::TypedByRef);
    mt.ty = TypeSig::ByRef(Box::new(ModifiedType::plain(referent)));
}

// ---------------------------------------------------------------------------
// Decoration
// ---------------------------------------------------------------------------

/// Wrap every `TypeSig` node reachable from the image in `modifier`.
///
/// Every struct below is destructured *exhaustively* (no `..` rest patterns, no
/// wildcard match arms). That is deliberate: a new signature-carrying field on
/// the reader model — or a new `TypeSig` variant with a child — must break this
/// file, because a position this probe cannot see is a position the properties
/// silently stop covering. The compiler is the reason this stays exhaustive.
fn decorate_image(image: &mut Image, modifier: CustomMod, scope: Scope) {
    let Image {
        assembly: _,
        assembly_attributes: _,
        references: _,
        type_defs,
        top_level: _,
        type_refs: _,
        member_refs,
        resources: _,
    } = image;
    if scope == Scope::Constraints {
        for td in type_defs {
            for gp in &mut td.generic_params {
                decorate_generic_param(gp, modifier);
            }
            for m in &mut td.methods {
                for gp in &mut m.generic_params {
                    decorate_generic_param(gp, modifier);
                }
            }
        }
        return;
    }
    if scope == Scope::Members {
        for td in type_defs {
            for m in &mut td.methods {
                if let Ok(sig) = &mut m.signature {
                    decorate_method_sig(sig, modifier);
                }
            }
            for f in &mut td.fields {
                decorate_result(&mut f.signature, modifier);
            }
            for p in &mut td.properties {
                decorate_result(&mut p.signature, modifier);
            }
            for e in &mut td.events {
                decorate_result(&mut e.event_type, modifier);
            }
        }
        return;
    }
    for td in type_defs {
        decorate_type_def(td, modifier);
    }
    for mr in member_refs {
        decorate_member_ref(mr, modifier);
    }
}

fn decorate_type_def(td: &mut TypeDef, modifier: CustomMod) {
    let TypeDef {
        name: _,
        accessibility: _,
        is_interface: _,
        is_sealed: _,
        extends,
        implements,
        generic_params,
        enclosing: _,
        nested: _,
        methods,
        fields,
        properties,
        events,
        attributes: _,
    } = td;
    if let Some(extends) = extends {
        decorate_result(extends, modifier);
    }
    for i in implements {
        decorate_result(i, modifier);
    }
    for gp in generic_params {
        decorate_generic_param(gp, modifier);
    }
    for m in methods {
        decorate_method(m, modifier);
    }
    for f in fields {
        let Field {
            name: _,
            accessibility: _,
            is_static: _,
            is_literal: _,
            is_init_only: _,
            signature,
            attributes: _,
        } = f;
        decorate_result(signature, modifier);
    }
    for p in properties {
        let Property {
            name: _,
            signature,
            getter: _,
            setter: _,
            other_accessors: _,
            attributes: _,
        } = p;
        decorate_result(signature, modifier);
    }
    for e in events {
        let Event {
            name: _,
            event_type,
            add: _,
            remove: _,
            raise: _,
            other_accessors: _,
            attributes: _,
        } = e;
        decorate_result(event_type, modifier);
    }
}

fn decorate_generic_param(gp: &mut GenericParam, modifier: CustomMod) {
    let GenericParam {
        name: _,
        variance: _,
        reference_type: _,
        value_type: _,
        default_ctor: _,
        allows_ref_struct: _,
        constraints,
        attributes: _,
    } = gp;
    for c in constraints {
        let TypeConstraint { ty, attributes: _ } = c;
        decorate_result(ty, modifier);
    }
}

fn decorate_method(m: &mut Method, modifier: CustomMod) {
    let Method {
        token: _,
        name: _,
        accessibility: _,
        is_static: _,
        is_abstract: _,
        is_virtual: _,
        is_final: _,
        is_new_slot: _,
        is_hide_by_sig: _,
        is_rt_special_name: _,
        generic_params,
        signature,
        attributes: _,
        implements,
        unclassified_impls,
    } = m;
    for gp in generic_params {
        decorate_generic_param(gp, modifier);
    }
    if let Ok(sig) = signature {
        decorate_method_sig(sig, modifier);
    }
    for i in implements {
        let InterfaceMemberImpl {
            interface,
            member: _,
            decl: _,
        } = i;
        decorate_result(interface, modifier);
    }
    for u in unclassified_impls {
        let UnclassifiedImpl { parent, member: _ } = u;
        decorate_position(parent, modifier);
    }
}

fn decorate_method_sig(sig: &mut MethodSig, modifier: CustomMod) {
    let MethodSig {
        has_this: _,
        explicit_this: _,
        calling_convention: _,
        return_type,
        return_attributes: _,
        parameters,
    } = sig;
    decorate_ret_type(return_type, modifier);
    for p in parameters {
        let Param {
            name: _,
            ty,
            is_in: _,
            is_out: _,
            optional: _,
            default_value: _,
            attributes: _,
        } = p;
        decorate_position(ty, modifier);
    }
}

fn decorate_member_ref(mr: &mut MemberRef, modifier: CustomMod) {
    let MemberRef {
        name: _,
        parent: _,
        signature,
    } = mr;
    let Ok(sig) = signature else { return };
    // `DecodedMethodSig` is `MethodSig`'s blob-only half; its fields are
    // spelled out here for the same reason.
    let crate::reader::DecodedMethodSig {
        has_this: _,
        explicit_this: _,
        calling_convention: _,
        return_type,
        param_types,
    } = sig;
    decorate_ret_type(return_type, modifier);
    for t in param_types {
        decorate_position(t, modifier);
    }
}

fn decorate_ret_type(ret: &mut RetType, modifier: CustomMod) {
    match ret {
        // `CustomMod* VOID`: the run is the position's, exactly as for any other.
        RetType::Void(mods) => mods.insert(0, modifier),
        RetType::Type(mt) => decorate_position(mt, modifier),
    }
}

fn decorate_result(sig: &mut Result<ModifiedType, SigError>, modifier: CustomMod) {
    if let Ok(mt) = sig {
        decorate_position(mt, modifier);
    }
}

/// Push `modifier` onto this position's run *and* onto every position under it.
///
/// Under the old encoding this had to wrap each node in a `Modified` layer, and
/// the whole point of the probe was that such a layer hides the node's head from
/// a guard. It cannot any more — which is exactly why this is still worth
/// running: it now checks that the *policy* (drop a `modopt`, refuse a `modreq`)
/// is applied at every position, rather than that the guards peel.
fn decorate_position(mt: &mut ModifiedType, modifier: CustomMod) {
    mt.mods.insert(0, modifier);
    match &mut mt.ty {
        TypeSig::Primitive(_)
        | TypeSig::Named { .. }
        | TypeSig::TypeVar(_)
        | TypeSig::MethodVar(_)
        | TypeSig::TypedByRef
        // `PTR VOID` has no child position to decorate; `PTR CustomMod* VOID` is
        // explicitly not modelled (see `TypeSig::Ptr`).
        | TypeSig::Ptr(None) => {}
        TypeSig::Generic {
            kind: _,
            scope: _,
            args,
        } => {
            for a in args {
                decorate_position(a, modifier);
            }
        }
        TypeSig::SzArray(inner) | TypeSig::ByRef(inner) | TypeSig::Ptr(Some(inner)) => {
            decorate_position(inner, modifier);
        }
        TypeSig::Array {
            element,
            rank: _,
            sizes: _,
            lower_bounds: _,
        } => decorate_position(element, modifier),
    }
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

fn count_types(entities: &[Entity]) -> usize {
    entities
        .iter()
        .map(|e| 1 + count_types(&e.nested_types))
        .sum()
}

/// Members the projector dropped and recorded, recursing into nested types.
fn count_member_skips(entities: &[Entity]) -> usize {
    entities
        .iter()
        .map(|e| e.skipped_members.len() + count_member_skips(&e.nested_types))
        .sum()
}

fn count_members(entities: &[Entity]) -> usize {
    entities
        .iter()
        .map(|e| e.members.len() + count_members(&e.nested_types))
        .sum()
}

/// Every member still standing after the `modreq` decoration, as
/// `Type.member`.
fn surviving_members(entities: &[Entity]) -> Vec<String> {
    let mut out = Vec::new();
    for e in entities {
        for m in &e.members {
            out.push(format!("{}.{}", fqn(e), member_name(m)));
        }
        out.extend(surviving_members(&e.nested_types));
    }
    out
}

/// P2's *type*-level half: a type consumes signatures too — its base type, its
/// interfaces, and its type parameters' constraints — so a type that survives an
/// unrecognised `modreq` on one of *those* has ignored it just as surely as a
/// surviving member has.
///
/// Only types that consumed no signature at all in the baseline (no base, no
/// interfaces, no typed constraints) may survive. This is not hypothetical
/// tidiness: the constraint path recognises the `unmanaged` marker positionally,
/// and a first cut of it consumed the marker while silently discarding an
/// unrecognised `modreq` sitting beside it in the same run. The member-level
/// check could not see that — the type's members drop for their own reasons.
fn surviving_signature_consumers(baseline: &[Entity], decorated: &[Entity]) -> Vec<String> {
    let kept: std::collections::BTreeSet<String> = flatten(decorated)
        .into_iter()
        .map(|e| type_key(&e))
        .collect();
    flatten(baseline)
        .into_iter()
        .filter(|e| kept.contains(&type_key(e)) && consumes_a_signature(e))
        .map(|e| {
            format!(
                "type `{}` survived an unrecognised `modreq` on its base type, an                  interface, or a generic constraint",
                fqn(&e)
            )
        })
        .collect()
}

/// [`Scope::Constraints`]: every declarer of a *typed* constraint must be gone.
///
/// A type-level typar's constraint refusal drops the type; a method-level one
/// drops the method. `unmanaged` counts: it is a typed constraint on the wire
/// (`System.ValueType modreq(UnmanagedType)`), and it is precisely the one the
/// projector consumes — so it is precisely the one where consuming could hide a
/// neighbour.
///
/// Identity, not name: the projected fqn strips generic arity, so
/// `FSharpEvent<T>` and `FSharpEvent<Del, Args>` share one — and an overload set
/// shares a method name. Keying on the name alone lets an *unconstrained* sibling
/// vouch for a constrained one that did drop.
fn surviving_constraint_declarers(baseline: &[Entity], decorated: &[Entity]) -> Vec<String> {
    let kept_types: std::collections::BTreeSet<String> = flatten(decorated)
        .into_iter()
        .map(|e| type_key(&e))
        .collect();
    // A member that survives constraint-only decoration is projected *identically*
    // to its baseline self (only typar constraints were touched), so its own
    // projection is an exact key — proof against overloads sharing a name.
    let kept_members: std::collections::BTreeSet<String> = flatten(decorated)
        .iter()
        .flat_map(|e| e.members.iter().map(|m| member_key(e, m)))
        .collect();

    let constrained = |p: &crate::TypeParameter| !p.type_constraints.is_empty() || p.is_unmanaged;
    let mut out = Vec::new();
    for e in flatten(baseline) {
        if e.generic_parameters.iter().any(constrained) && kept_types.contains(&type_key(&e)) {
            out.push(format!(
                "type `{}` survived an unrecognised `modreq` on a generic constraint",
                fqn(&e)
            ));
        }
        for m in &e.members {
            let Member::Method(mm) = m else { continue };
            if mm.generic_parameters.iter().any(constrained)
                && kept_members.contains(&member_key(&e, m))
            {
                out.push(format!(
                    "method `{}.{}` survived an unrecognised `modreq` on a generic constraint",
                    fqn(&e),
                    mm.name
                ));
            }
        }
    }
    out
}

/// A type's identity: the fqn the projection strips arity from, plus that arity.
fn type_key(e: &Entity) -> String {
    format!("{}`{}", fqn(e), e.generic_parameters.len())
}

/// A member's identity, *stable under the decoration being probed*.
///
/// Not its whole projection: a constraint regression that keeps the declaring
/// method but drops its constraint changes the method's projected
/// `generic_parameters`, so a `Debug`-of-everything key would no longer match its
/// baseline self — and the survivor would be scored as absent, which is the exact
/// false negative this check exists to avoid. Key on what constraint decoration
/// *cannot* touch: the name, the signature, and the typar count. (Together these
/// separate overloads, which a bare name does not.)
fn member_key(owner: &Entity, m: &Member) -> String {
    let ident = match m {
        Member::Method(mm) => format!(
            "M {} {:?} `{}",
            mm.name,
            mm.signature,
            mm.generic_parameters.len()
        ),
        Member::Field(f) => format!("F {}", f.name),
        Member::Property(p) => format!("P {}", p.name),
        Member::Event(e) => format!("E {}", e.name),
    };
    format!("{}::{ident}", type_key(owner))
}

/// Whether projecting this type had to read a signature of its own.
fn consumes_a_signature(e: &Entity) -> bool {
    e.base_type.is_some()
        || !e.interfaces.is_empty()
        || e.generic_parameters
            .iter()
            .any(|p| !p.type_constraints.is_empty())
}

fn flatten(entities: &[Entity]) -> Vec<Entity> {
    let mut out = Vec::new();
    for e in entities {
        out.push(e.clone());
        out.extend(flatten(&e.nested_types));
    }
    out
}

fn fqn(e: &Entity) -> String {
    if e.namespace.is_empty() {
        e.name.clone()
    } else {
        format!("{}.{}", e.namespace.join("."), e.name)
    }
}

fn member_name(m: &Member) -> &str {
    match m {
        Member::Method(m) => &m.name,
        Member::Field(f) => &f.name,
        Member::Property(p) => &p.name,
        Member::Event(e) => &e.name,
    }
}

/// Structural diff of two `Entity` trees, reporting the *first* divergence per
/// entity rather than a wall of `Debug`.
fn diff_entities(
    baseline: &[Entity],
    decorated: &[Entity],
    path: &str,
    findings: &mut Vec<String>,
) {
    if baseline.len() != decorated.len() {
        findings.push(format!(
            "{path}: type count moved under `modopt` decoration: {} -> {}",
            baseline.len(),
            decorated.len()
        ));
        return;
    }
    for (b, d) in baseline.iter().zip(decorated) {
        let here = format!("{path}{}", fqn(b));
        if b.members.len() != d.members.len() {
            let lost: Vec<_> = b
                .members
                .iter()
                .map(member_name)
                .filter(|n| !d.members.iter().any(|m| member_name(m) == *n))
                .collect();
            findings.push(format!(
                "{here}: member count moved under `modopt` decoration: {} -> {} (dropped: {lost:?})",
                b.members.len(),
                d.members.len()
            ));
        } else if b.members != d.members {
            for (bm, dm) in b.members.iter().zip(&d.members) {
                if bm != dm {
                    findings.push(format!(
                        "{here}.{}: member projection moved under `modopt` decoration:\n  \
                         baseline:  {bm:?}\n  decorated: {dm:?}",
                        member_name(bm)
                    ));
                }
            }
        }
        if b.skipped_members != d.skipped_members {
            findings.push(format!(
                "{here}: skipped-member records moved under `modopt` decoration:\n  \
                 baseline:  {:?}\n  decorated: {:?}",
                b.skipped_members, d.skipped_members
            ));
        }
        // Everything else about the entity: base type, interfaces, type
        // parameters, attribute-derived facts.
        if entity_shell(b) != entity_shell(d) {
            findings.push(format!(
                "{here}: type projection moved under `modopt` decoration:\n  \
                 baseline:  {:?}\n  decorated: {:?}",
                entity_shell(b),
                entity_shell(d)
            ));
        }
        diff_entities(
            &b.nested_types,
            &d.nested_types,
            &format!("{here}/"),
            findings,
        );
    }
}

/// An entity with its members and nested types blanked, so the shell (base
/// type, interfaces, type parameters, markers) can be compared without
/// re-reporting a member divergence already reported above.
fn entity_shell(e: &Entity) -> Entity {
    let mut e = e.clone();
    e.members = Vec::new();
    e.nested_types = Vec::new();
    e.skipped_members = Vec::new();
    e
}
