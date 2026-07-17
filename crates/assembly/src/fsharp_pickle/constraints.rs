//! Phase-1 typar-constraint decoders.
//!
//! Mirrors `u_tyar_constraint` (`TypedTreePickle.fs:2331-2350`),
//! `u_tyar_constraintB` (`:2353-2359`), and `u_tyar_constraints`
//! (`:2361-2368`). The composed `u_tyar_constraints` reads a length-
//! prefixed list from the primary stream via `u_list_revi` (so that
//! `DefaultsTo` priorities descend with declaration order) and then
//! reads a length-prefixed list from the B stream via `u_listB` (so
//! that legacy assemblies without a B stream produce no extra
//! constraints).
//!
//! Tag pin (re-verified against FCS):
//!
//! Primary stream (`u_tyar_constraint`, `:2331-2350`):
//!
//! | Tag | Variant                            |
//! |-----|------------------------------------|
//! | 0   | `CoercesTo`                        |
//! | 1   | `MayResolveMember` (SRTP)          |
//! | 2   | `DefaultsTo { priority, ty }`     |
//! | 3   | `SupportsNull`                    |
//! | 4   | `IsNonNullableStruct`             |
//! | 5   | `IsReferenceType`                 |
//! | 6   | `RequiresDefaultConstructor`      |
//! | 7   | `SimpleChoice`                    |
//! | 8   | `IsEnum`                          |
//! | 9   | `IsDelegate`                      |
//! | 10  | `SupportsComparison`              |
//! | 11  | `SupportsEquality`                |
//! | 12  | `IsUnmanaged`                     |
//!
//! B-stream tail (`u_tyar_constraintB`, `:2353-2359`):
//!
//! | Tag | Variant            |
//! |-----|--------------------|
//! | 1   | `NotSupportsNull`  |
//! | 2   | `AllowsRefStruct`  |

use crate::error::ImportError;
use crate::fsharp_pickle::expr::read_expr;
use crate::fsharp_pickle::il::{read_il_method_ref, read_il_type_ref};
use crate::fsharp_pickle::model::{FSharpTyparConstraint, PickledTrait};
use crate::fsharp_pickle::osgn::PhaseOneState;
use crate::fsharp_pickle::types::{read_ty, read_tys};
use crate::fsharp_pickle::val::read_member_flags;
use crate::fsharp_pickle::vrefs::read_vref;

/// `u_trait` — `TypedTreePickle.fs:2170-2174`. Six wire fields in
/// order: `u_tys` support types, `u_string` member name,
/// `u_MemberFlags`, `u_tys` arg types, `u_option u_ty` return type,
/// `u_option u_trait_sln` solution.
///
/// The trait solution is decoded for stream alignment and dropped (it is
/// not part of the cross-CCU view we model). FCS at `:2143` notes
/// solutions exist mainly because they "can occur in optimization data";
/// a typar-constraint trait pickles `None`, but a trait in *expression*
/// position (`Expr.WitnessArg` / `TOp.TraitCall`) carries a real
/// `Some`-solution, which [`read_trait_sln`] consumes.
pub(crate) fn read_trait(state: &mut PhaseOneState<'_>) -> Result<PickledTrait, ImportError> {
    let support_tys = read_tys(state)?;
    let member_name = state.reader.read_string("u_trait member-name")?;
    let member_flags = read_member_flags(&mut state.reader)?;
    let arg_tys = read_tys(state)?;
    let return_ty = state.read_option("u_trait return-ty", read_ty)?;
    state.read_option("u_trait solution", read_trait_sln)?;
    Ok(PickledTrait {
        support_tys,
        member_name,
        member_flags,
        arg_tys,
        return_ty,
    })
}

/// `u_trait_sln` (`TypedTreePickle.fs:2144-2168`). Eight arms; decoded for
/// stream alignment only — the resolved trait solution is not part of the
/// cross-CCU view. Arms 4/5 (`FSRecdFieldSln` / `FSAnonRecdFieldSln`) need
/// `u_rfref` / `u_anonInfo`, which the signature unpickler does not yet
/// port, so they stay loud-on-unknown (no fixture has tripped one).
fn read_trait_sln(state: &mut PhaseOneState<'_>) -> Result<(), ImportError> {
    let tag = state.reader.read_byte("u_trait_sln tag")?;
    match tag {
        // ILMethSln(ty, optILTypeRef, ILMethodRef, tys, None)
        0 => {
            read_ty(state)?;
            state
                .reader
                .read_option("u_trait_sln ILMethSln ILTypeRef", read_il_type_ref)?;
            read_il_method_ref(&mut state.reader)?;
            read_tys(state)?;
            Ok(())
        }
        // FSMethSln(ty, vref, tys, None)
        1 => {
            read_ty(state)?;
            read_vref(state)?;
            read_tys(state)?;
            Ok(())
        }
        // BuiltInSln
        2 => Ok(()),
        // ClosedExprSln(expr)
        3 => {
            read_expr(state, "u_trait_sln ClosedExprSln")?;
            Ok(())
        }
        // ILMethSln(ty, optILTypeRef, ILMethodRef, tys, Some ty)
        6 => {
            read_ty(state)?;
            state
                .reader
                .read_option("u_trait_sln ILMethSln ILTypeRef", read_il_type_ref)?;
            read_il_method_ref(&mut state.reader)?;
            read_tys(state)?;
            read_ty(state)?;
            Ok(())
        }
        // FSMethSln(ty, vref, tys, Some ty)
        7 => {
            read_ty(state)?;
            read_vref(state)?;
            read_tys(state)?;
            read_ty(state)?;
            Ok(())
        }
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_trait_sln tag (arms 4/5 need u_rfref/u_anonInfo — port when FSharp.Core trips one)",
            tag: u32::from(other),
        }),
    }
}

/// `u_tyar_constraint` — `:2331-2350`. The returned closure in the FCS
/// source carries the `range0` and accepts a reverse index argument
/// from `u_list_revi` (only `DefaultsTo` uses it). We unify those: the
/// caller passes `ridx` explicitly and we plumb it straight into
/// `DefaultsTo`'s priority.
pub(crate) fn read_tyar_constraint(
    state: &mut PhaseOneState<'_>,
    ridx: u32,
) -> Result<FSharpTyparConstraint, ImportError> {
    let tag = state.reader.read_byte("u_tyar_constraint tag")?;
    match tag {
        0 => {
            let ty = read_ty(state)?;
            Ok(FSharpTyparConstraint::CoercesTo(ty))
        }
        1 => {
            let trait_ = read_trait(state)?;
            Ok(FSharpTyparConstraint::MayResolveMember(trait_))
        }
        2 => {
            let ty = read_ty(state)?;
            Ok(FSharpTyparConstraint::DefaultsTo { priority: ridx, ty })
        }
        3 => Ok(FSharpTyparConstraint::SupportsNull),
        4 => Ok(FSharpTyparConstraint::IsNonNullableStruct),
        5 => Ok(FSharpTyparConstraint::IsReferenceType),
        6 => Ok(FSharpTyparConstraint::RequiresDefaultConstructor),
        7 => {
            let tys = read_tys(state)?;
            Ok(FSharpTyparConstraint::SimpleChoice(tys))
        }
        8 => {
            let ty = read_ty(state)?;
            Ok(FSharpTyparConstraint::IsEnum(ty))
        }
        9 => {
            let a = read_ty(state)?;
            let b = read_ty(state)?;
            Ok(FSharpTyparConstraint::IsDelegate(a, b))
        }
        10 => Ok(FSharpTyparConstraint::SupportsComparison),
        11 => Ok(FSharpTyparConstraint::SupportsEquality),
        12 => Ok(FSharpTyparConstraint::IsUnmanaged),
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_tyar_constraint tag",
            tag: u32::from(other),
        }),
    }
}

/// `u_tyar_constraintB` — `:2353-2359`. The leading `u_byteB` is read
/// inside `read_list_b`, but each element body still reads from the B
/// stream, so the per-element tag we read here is also via the B
/// cursor.
pub(crate) fn read_tyar_constraint_b(
    state: &mut PhaseOneState<'_>,
) -> Result<FSharpTyparConstraint, ImportError> {
    let tag = state.reader.read_byte_b("u_tyar_constraintB tag");
    match tag {
        1 => Ok(FSharpTyparConstraint::NotSupportsNull),
        2 => Ok(FSharpTyparConstraint::AllowsRefStruct),
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_tyar_constraintB tag",
            tag: u32::from(other),
        }),
    }
}

/// `u_tyar_constraints` — `:2361-2368`. The primary list is read with
/// `u_list_revi` so that priorities descend with declaration order
/// (last gets `0`); the B-stream tail is read with `u_listB` so that
/// pre-F#9 assemblies (no B stream) produce no extra constraints. The
/// two are concatenated.
pub(crate) fn read_tyar_constraints(
    state: &mut PhaseOneState<'_>,
) -> Result<Vec<FSharpTyparConstraint>, ImportError> {
    let mut out = state.read_list_revi("u_tyar_constraints primary", read_tyar_constraint)?;
    // The B-stream tail doesn't touch OSGN tables, so we don't go
    // through the state-aware list reader. But the length still has
    // to be bounded by the B-cursor's remaining bytes before we
    // allocate — a malformed `0xFF`-marker length on a near-empty
    // B stream would otherwise drive `Vec::with_capacity` to OOM.
    // Each element body reads at least one B byte (`read_byte_b` for
    // the constraint tag), so `n` cannot exceed B's remaining length.
    let n = state.reader.read_uint32_b("u_tyar_constraints B len") as usize;
    let b_remaining = state.reader.b_remaining();
    if n > b_remaining {
        return Err(ImportError::MalformedPickleHeader {
            detail: format!(
                "u_tyar_constraints B list length {n} exceeds B-stream remaining {b_remaining}",
            ),
        });
    }
    let mut tail = Vec::with_capacity(n);
    for _ in 0..n {
        tail.push(read_tyar_constraint_b(state)?);
    }
    out.extend(tail);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsharp_pickle::model::{Nullness, PickledType};
    use crate::fsharp_pickle::reader::PickleReader;

    /// Capacities are generous: constraint fixtures embed `u_ty`
    /// payloads (typically `Var(typar_idx)`) whose decoder now
    /// bounds-checks against the typar OSGN table at decode time.
    fn empty_state<'a>(bytes: &'a [u8]) -> PhaseOneState<'a> {
        PhaseOneState::with_capacities(PickleReader::new(bytes), 256, 256, 256)
    }

    fn empty_state_dual<'a>(primary: &'a [u8], b: Option<&'a [u8]>) -> PhaseOneState<'a> {
        PhaseOneState::with_capacities(PickleReader::new_dual(primary, b), 256, 256, 256)
    }

    fn read_one(bytes: &[u8]) -> Result<FSharpTyparConstraint, ImportError> {
        let mut s = empty_state(bytes);
        let c = read_tyar_constraint(&mut s, 0)?;
        assert!(
            s.reader.is_eof(),
            "decoder should consume the whole fixture; {} bytes left",
            s.reader.remaining()
        );
        Ok(c)
    }

    fn var0() -> [u8; 2] {
        // Var(typar 0), Ambivalent (B-absent fallback).
        [4, 0]
    }

    #[test]
    fn tag_0_coerces_to() {
        let mut bytes = vec![0];
        bytes.extend(var0());
        let c = read_one(&bytes).unwrap();
        assert_eq!(
            c,
            FSharpTyparConstraint::CoercesTo(PickledType::Var {
                typar_index: 0,
                nullness: Nullness::Ambivalent,
            }),
        );
    }

    #[test]
    fn tag_1_srtp_minimal_round_trip() {
        // tag 1, then a minimal u_trait:
        //   support_tys = [] (compressed-int 0)
        //   member_name = strings[0] = "Foo"
        //   member_flags = Instance + Member (IsInstance=true, _unused=false,
        //                  IsDispatchSlot=false, IsOverride=false, IsFinal=false,
        //                  kind=Member)
        //   arg_tys = []
        //   return_ty = None
        //   solution = None
        let strings = vec!["Foo".to_string()];
        let bytes = vec![1u8, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut r = PickleReader::new(&bytes);
        let pubpaths: &[Vec<u32>] = &[];
        r.attach_tables(&strings, pubpaths);
        let mut s = PhaseOneState::with_capacities(r, 256, 256, 256);
        let c = read_tyar_constraint(&mut s, 0).unwrap();
        let FSharpTyparConstraint::MayResolveMember(t) = c else {
            panic!("expected MayResolveMember")
        };
        assert!(t.support_tys.is_empty());
        assert_eq!(t.member_name, "Foo");
        assert!(t.member_flags.is_instance);
        assert!(!t.member_flags.is_dispatch_slot);
        assert!(t.arg_tys.is_empty());
        assert!(t.return_ty.is_none());
        assert!(s.reader.is_eof());
    }

    #[test]
    fn tag_1_srtp_with_solution_decodes() {
        // Same minimal u_trait but with solution = Some(BuiltInSln). The
        // solution is decoded for alignment and dropped; the constraint still
        // surfaces as MayResolveMember. (A typar-constraint trait normally
        // pickles None, but the solution arm must round-trip so traits in
        // expression position — WitnessArg / TraitCall — decode too.)
        let strings = vec!["Bar".to_string()];
        // …, solution option = Some (1), then u_trait_sln tag 2 = BuiltInSln.
        let bytes = vec![1u8, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 2];
        let mut r = PickleReader::new(&bytes);
        let pubpaths: &[Vec<u32>] = &[];
        r.attach_tables(&strings, pubpaths);
        let mut s = PhaseOneState::with_capacities(r, 256, 256, 256);
        let FSharpTyparConstraint::MayResolveMember(t) = read_tyar_constraint(&mut s, 0).unwrap()
        else {
            panic!("expected MayResolveMember")
        };
        assert_eq!(t.member_name, "Bar");
        assert!(s.reader.is_eof());
    }

    #[test]
    fn tag_2_defaults_to_uses_ridx_as_priority() {
        let mut bytes = vec![2];
        bytes.extend(var0());
        let mut s = empty_state(&bytes);
        let c = read_tyar_constraint(&mut s, 7).unwrap();
        match c {
            FSharpTyparConstraint::DefaultsTo { priority, ty } => {
                assert_eq!(priority, 7);
                assert_eq!(
                    ty,
                    PickledType::Var {
                        typar_index: 0,
                        nullness: Nullness::Ambivalent,
                    }
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn no_payload_tags_round_trip() {
        for (tag, expected) in [
            (3, FSharpTyparConstraint::SupportsNull),
            (4, FSharpTyparConstraint::IsNonNullableStruct),
            (5, FSharpTyparConstraint::IsReferenceType),
            (6, FSharpTyparConstraint::RequiresDefaultConstructor),
            (10, FSharpTyparConstraint::SupportsComparison),
            (11, FSharpTyparConstraint::SupportsEquality),
            (12, FSharpTyparConstraint::IsUnmanaged),
        ] {
            let bytes = [tag];
            let c = read_one(&bytes).unwrap();
            assert_eq!(c, expected);
        }
    }

    #[test]
    fn tag_7_simple_choice() {
        // tag 7, then u_tys = list of 2 Var(0)s.
        let mut bytes = vec![7, 2];
        bytes.extend(var0());
        bytes.extend(var0());
        let c = read_one(&bytes).unwrap();
        match c {
            FSharpTyparConstraint::SimpleChoice(tys) => {
                assert_eq!(tys.len(), 2);
                for ty in tys {
                    assert!(matches!(
                        ty,
                        PickledType::Var {
                            typar_index: 0,
                            nullness: Nullness::Ambivalent,
                        },
                    ));
                }
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn tag_8_is_enum() {
        let mut bytes = vec![8];
        bytes.extend(var0());
        let c = read_one(&bytes).unwrap();
        assert_eq!(
            c,
            FSharpTyparConstraint::IsEnum(PickledType::Var {
                typar_index: 0,
                nullness: Nullness::Ambivalent,
            }),
        );
    }

    #[test]
    fn tag_9_is_delegate() {
        let mut bytes = vec![9];
        bytes.extend(var0());
        bytes.extend(var0());
        let c = read_one(&bytes).unwrap();
        match c {
            FSharpTyparConstraint::IsDelegate(a, b) => {
                assert!(matches!(a, PickledType::Var { .. }));
                assert!(matches!(b, PickledType::Var { .. }));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn unknown_primary_tag_errors() {
        let bytes = [13];
        let mut s = empty_state(&bytes);
        match read_tyar_constraint(&mut s, 0) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_tyar_constraint tag",
                tag: 13,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn b_stream_tag_1_not_supports_null() {
        let primary: [u8; 1] = [0];
        let bb = [1u8];
        let mut s = empty_state_dual(&primary, Some(&bb));
        let c = read_tyar_constraint_b(&mut s).unwrap();
        assert_eq!(c, FSharpTyparConstraint::NotSupportsNull);
    }

    #[test]
    fn b_stream_tag_2_allows_ref_struct() {
        let primary: [u8; 1] = [0];
        let bb = [2u8];
        let mut s = empty_state_dual(&primary, Some(&bb));
        let c = read_tyar_constraint_b(&mut s).unwrap();
        assert_eq!(c, FSharpTyparConstraint::AllowsRefStruct);
    }

    #[test]
    fn b_stream_tag_0_errors() {
        // tagB = 0 means either an explicit zero or the absent-B
        // fallback. In B-stream constraint decoding it is illegal —
        // FCS's u_tyar_constraintB ufailwiths.
        let primary: [u8; 1] = [0];
        let bb = [0u8];
        let mut s = empty_state_dual(&primary, Some(&bb));
        match read_tyar_constraint_b(&mut s) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_tyar_constraintB tag",
                tag: 0,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn read_tyar_constraints_concatenates_primary_and_b() {
        // Primary: 2 constraints (SupportsNull, IsUnmanaged).
        // B-stream: 1 constraint (NotSupportsNull).
        let primary = [2u8, 3, 12];
        let bb = [1u8, 1];
        let mut s = empty_state_dual(&primary, Some(&bb));
        let cs = read_tyar_constraints(&mut s).unwrap();
        assert_eq!(
            cs,
            vec![
                FSharpTyparConstraint::SupportsNull,
                FSharpTyparConstraint::IsUnmanaged,
                FSharpTyparConstraint::NotSupportsNull,
            ],
        );
    }

    #[test]
    fn read_tyar_constraints_b_absent_yields_only_primary() {
        let primary = [1u8, 11];
        let mut s = empty_state_dual(&primary, None);
        let cs = read_tyar_constraints(&mut s).unwrap();
        assert_eq!(cs, vec![FSharpTyparConstraint::SupportsEquality]);
    }

    #[test]
    fn read_tyar_constraints_assigns_descending_default_priorities() {
        // Three DefaultsTo constraints in declaration order. After
        // u_list_revi, the first gets priority 2 (= n-1), the last 0.
        let mut bytes = vec![3u8]; // primary list length 3
        for _ in 0..3 {
            bytes.push(2); // tag = DefaultsTo
            bytes.extend([4, 0]); // ty = Var(0), Ambivalent
        }
        let mut s = empty_state(&bytes);
        let cs = read_tyar_constraints(&mut s).unwrap();
        assert_eq!(cs.len(), 3);
        let priorities: Vec<u32> = cs
            .iter()
            .map(|c| match c {
                FSharpTyparConstraint::DefaultsTo { priority, .. } => *priority,
                other => panic!("expected DefaultsTo, got {other:?}"),
            })
            .collect();
        assert_eq!(priorities, vec![2, 1, 0]);
    }

    #[test]
    fn read_tyar_constraints_empty() {
        // Primary empty list, no B stream.
        let primary = [0u8];
        let mut s = empty_state(&primary);
        let cs = read_tyar_constraints(&mut s).unwrap();
        assert!(cs.is_empty());
    }

    #[test]
    fn read_tyar_constraints_b_length_bounded_by_b_remaining() {
        // Primary list empty. B stream length encoded via the
        // marker form (`0xFF` + 4-byte LE) as 1_000_000, but only 5
        // bytes follow the marker. Must hard-error rather than
        // `Vec::with_capacity(1_000_000)`.
        let primary = [0u8];
        let mut bb = vec![0xFFu8];
        bb.extend_from_slice(&1_000_000u32.to_le_bytes());
        bb.extend_from_slice(&[1u8, 1, 1, 1, 1]);
        let mut s = empty_state_dual(&primary, Some(&bb));
        match read_tyar_constraints(&mut s) {
            Err(ImportError::MalformedPickleHeader { detail }) => {
                assert!(detail.contains("1000000"), "detail: {detail}");
                assert!(detail.contains("B-stream"), "detail: {detail}");
            }
            other => panic!("expected MalformedPickleHeader, got {other:?}"),
        }
    }
}
