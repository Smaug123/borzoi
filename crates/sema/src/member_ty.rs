//! The `TypeRef → Ty` bridge for member-access typing (Stage 3.3a).
//!
//! When a suspended [`HasMember`](crate::infer) constraint wakes — its receiver
//! resolves to a concrete [`Ty::Named`] whose entity has a single unambiguous
//! public instance field / non-indexer property named as accessed — the member's
//! signature ([`TypeRef`], the assembly crate's owned type model) must be brought
//! into this crate's [`Ty`]. That is a **partial** function: v1 models only the
//! non-generic corner (`no speculative generality`), so a member whose type this
//! bridge cannot render returns `None` and the access defers (D5: silence, never a
//! guess).
//!
//! What converts:
//! - [`TypeRef::Primitive`] → the canonical BCL [`Ty::Named`] (`I4` ⇒
//!   `System.Int32`), the same FQN convention [`Ty::render`](crate::Ty::render)
//!   emits, so a bridged type compares against the FCS oracle by string equality.
//! - A **non-generic, non-nested** [`TypeRef::Named`] (empty `type_args`, a single
//!   arity-`0` segment, so a plain dotted `Namespace.Name`) → [`Ty::Named`]. A
//!   generic instantiation or an open/nested generic is deferred — [`Ty::Named`]
//!   has no generic-argument list yet (that arrives with a later slice).
//! - A **plain vector** [`TypeRef::Array`] (`rank == 1`, empty `sizes` /
//!   `lower_bounds`) whose element converts → [`Ty::Array`] of rank 1. A
//!   multidimensional or bounded array is deferred (the element convention is
//!   still the same, but v1 keeps the array corner minimal).
//!
//! Everything else — a type variable, a pointer, a byref, a generic
//! instantiation, a multidim array — returns `None`. Nullability
//! ([`Nullability`](borzoi_assembly::Nullability)) is ignored: the receiver's
//! nullable-reference annotation does not change the F# type this stage displays.

use borzoi_assembly::{Primitive, TypeRef};

use crate::ty::Ty;

/// Convert a member-signature [`TypeRef`] to a [`Ty`], or `None` when the shape
/// is one this stage does not model (so the access defers, D5). Total over
/// [`TypeRef`]; the unmodelled arms are explicit `None`s. See the module docs for
/// the modelled corner.
pub(crate) fn type_ref_to_ty(ty: &TypeRef) -> Option<Ty> {
    match ty {
        // A primitive maps to its canonical BCL FQN — the same convention
        // `Ty::render` / the FCS oracle emit.
        TypeRef::Primitive(p) => Some(primitive_to_ty(*p)),
        // A non-generic, non-nested named type: empty `type_args`, and a single
        // segment that introduces no generics (`segment_arities == [0]`, i.e. the
        // name has no `/` and declares arity 0). A generic instantiation (non-empty
        // `type_args`), an *open* generic (non-zero arity with empty args), or a
        // nested type (multiple segments) all defer — `Ty::Named` carries no
        // generic-argument list yet.
        TypeRef::Named {
            namespace,
            name,
            type_args,
            segment_arities,
            ..
        } => {
            if !type_args.is_empty() {
                return None;
            }
            // A well-formed non-generic top-level type has exactly one segment of
            // arity 0. `name.contains('/')` is a defensive nested check: the
            // projector `/`-joins nested segments into `name`, so a nested type has
            // a `/` even if `segment_arities` is corrupt.
            if segment_arities.iter().any(|&a| a != 0) || name.contains('/') {
                return None;
            }
            let mut path: Vec<String> = namespace.clone();
            path.push(name.clone());
            Some(Ty::Named(path))
        }
        // A plain vector (`T[]`): rank 1, no explicit sizes or lower bounds, and a
        // convertible element. A multidimensional (`rank != 1`) or bounded
        // (non-empty `sizes` / `lower_bounds`) array defers.
        TypeRef::Array {
            element,
            rank,
            sizes,
            lower_bounds,
        } => {
            if *rank != 1 || !sizes.is_empty() || !lower_bounds.is_empty() {
                return None;
            }
            let elem = type_ref_to_ty(&element.ty)?;
            Some(Ty::Array {
                elem: Box::new(elem),
                rank: 1,
            })
        }
        // A type variable, a pointer, or a byref is not a member type this stage
        // renders.
        TypeRef::Var { .. } | TypeRef::Ptr(_) | TypeRef::ByRef { .. } => None,
    }
}

/// The canonical BCL [`Ty::Named`] for an ECMA-335 primitive — the same
/// `(namespace, name)` mapping the assembly crate's display layer uses, kept in
/// this crate so the bridge does not depend on a private helper there. `Void`
/// maps to `System.Void` for totality; a real member type is never `Void`.
fn primitive_to_ty(p: Primitive) -> Ty {
    let path = match p {
        Primitive::Void => "System.Void",
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
    };
    Ty::named(path)
}

#[cfg(test)]
mod tests {
    use borzoi_assembly::{NullableType, Primitive, TypeRef};

    use super::type_ref_to_ty;
    use crate::ty::Ty;

    fn named(ns: &[&str], name: &str) -> TypeRef {
        TypeRef::Named {
            assembly: None,
            namespace: ns.iter().map(|s| s.to_string()).collect(),
            name: name.to_string(),
            type_args: vec![],
            segment_arities: vec![0],
        }
    }

    #[test]
    fn primitives_map_to_canonical_fqns() {
        for (p, want) in [
            (Primitive::I4, "System.Int32"),
            (Primitive::String, "System.String"),
            (Primitive::Bool, "System.Boolean"),
            (Primitive::Char, "System.Char"),
            (Primitive::R8, "System.Double"),
            (Primitive::U4, "System.UInt32"),
        ] {
            assert_eq!(
                type_ref_to_ty(&TypeRef::Primitive(p)),
                Some(Ty::named(want)),
                "{p:?}"
            );
        }
    }

    #[test]
    fn non_generic_named_maps_to_dotted_path() {
        assert_eq!(
            type_ref_to_ty(&named(&["System"], "Guid")),
            Some(Ty::named("System.Guid"))
        );
        // No namespace → a bare name.
        assert_eq!(
            type_ref_to_ty(&named(&[], "Thing")),
            Some(Ty::Named(vec!["Thing".to_string()]))
        );
    }

    #[test]
    fn generic_named_defers() {
        // A generic instantiation (`List<int>`): non-empty type_args.
        let generic = TypeRef::Named {
            assembly: None,
            namespace: vec!["System".to_string(), "Collections".to_string()],
            name: "List`1".to_string(),
            type_args: vec![NullableType::oblivious(TypeRef::Primitive(Primitive::I4))],
            segment_arities: vec![1],
        };
        assert_eq!(type_ref_to_ty(&generic), None);

        // An *open* generic: arity 1, empty args (still deferred — non-zero arity).
        let open = TypeRef::Named {
            assembly: None,
            namespace: vec!["System".to_string()],
            name: "Nullable`1".to_string(),
            type_args: vec![],
            segment_arities: vec![1],
        };
        assert_eq!(type_ref_to_ty(&open), None);
    }

    #[test]
    fn nested_named_defers() {
        // A nested type (`Outer/Inner`): two segments in the `/`-joined name.
        let nested = TypeRef::Named {
            assembly: None,
            namespace: vec!["Ns".to_string()],
            name: "Outer/Inner".to_string(),
            type_args: vec![],
            segment_arities: vec![0, 0],
        };
        assert_eq!(type_ref_to_ty(&nested), None);
    }

    #[test]
    fn plain_vector_array_maps_and_multidim_defers() {
        let vector = TypeRef::Array {
            element: Box::new(NullableType::oblivious(TypeRef::Primitive(Primitive::U1))),
            rank: 1,
            sizes: vec![],
            lower_bounds: vec![],
        };
        assert_eq!(
            type_ref_to_ty(&vector),
            Some(Ty::Array {
                elem: Box::new(Ty::named("System.Byte")),
                rank: 1,
            })
        );

        // A rank-2 array defers.
        let md = TypeRef::Array {
            element: Box::new(NullableType::oblivious(TypeRef::Primitive(Primitive::I4))),
            rank: 2,
            sizes: vec![],
            lower_bounds: vec![],
        };
        assert_eq!(type_ref_to_ty(&md), None);

        // A bounded vector (non-empty sizes) defers.
        let bounded = TypeRef::Array {
            element: Box::new(NullableType::oblivious(TypeRef::Primitive(Primitive::I4))),
            rank: 1,
            sizes: vec![4],
            lower_bounds: vec![],
        };
        assert_eq!(type_ref_to_ty(&bounded), None);
    }

    #[test]
    fn nullability_is_ignored() {
        // An `Annotated` (`string?`) element still bridges to `System.String`.
        let annotated = TypeRef::Array {
            element: Box::new(NullableType {
                ty: TypeRef::Primitive(Primitive::String),
                nullability: borzoi_assembly::Nullability::Annotated,
            }),
            rank: 1,
            sizes: vec![],
            lower_bounds: vec![],
        };
        assert_eq!(
            type_ref_to_ty(&annotated),
            Some(Ty::Array {
                elem: Box::new(Ty::named("System.String")),
                rank: 1,
            })
        );
    }

    #[test]
    fn pointer_and_byref_defer() {
        assert_eq!(
            type_ref_to_ty(&TypeRef::Ptr(Some(Box::new(TypeRef::Primitive(
                Primitive::I4
            ))))),
            None
        );
        assert_eq!(
            type_ref_to_ty(&TypeRef::ByRef {
                inner: Box::new(TypeRef::Primitive(Primitive::I4)),
                readonly: false
            }),
            None
        );
        assert_eq!(
            type_ref_to_ty(&TypeRef::Var {
                index: 0,
                is_method: false
            }),
            None
        );
    }
}
