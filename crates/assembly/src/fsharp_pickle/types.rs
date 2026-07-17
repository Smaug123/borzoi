//! Phase-1 type decoder.
//!
//! Mirrors `u_ty` (`TypedTreePickle.fs:2497-2579`) and the per-`TType`
//! helpers it composes: `u_tys` (`:2007`), `u_tcref` (`:1942`),
//! `u_ucref` (`:1950`), `u_tpref` (`:1958`), `u_simpletyp` (`:915`).
//!
//! From 6b4 onward, `read_ty` threads `&mut PhaseOneState` because
//! `TType_forall` (tag 5) writes into the typar OSGN table via
//! `u_tyar_specs`. The non-Forall variants still need only the reader,
//! but expose a state-shaped signature so callers don't have to pick
//! between two near-identical entry points.
//!
//! ### Tag-value pin (re-verified against FCS source)
//!
//! `u_ty` dispatches on a 10-tag set; this module's `read_ty` follows
//! the order at `:2500-2579`:
//!
//! | Tag | FCS variant                          | Status in 6b4  |
//! |-----|--------------------------------------|----------------|
//! | 0   | `TType_tuple(tupInfoRef, _)`         | supported      |
//! | 1   | `TType_app` from simpletyp table     | supported      |
//! | 2   | `TType_app(tcref, args, _)`          | supported      |
//! | 3   | `TType_fun(d, r, _)`                 | supported      |
//! | 4   | `TType_var(tpref, _)`                | supported      |
//! | 5   | `TType_forall(tps, ty)`              | supported      |
//! | 6   | `TType_measure m`                    | supported      |
//! | 7   | `TType_ucase(uc, args)`              | supported      |
//! | 8   | `TType_tuple(tupInfoStruct, _)`      | supported      |
//! | 9   | `TType_anon(anonInfo, args)`         | **deferred** (no MiniLibFs fixture trips it) |
//!
//! The four B-stream tags per nullness-bearing variant project as the
//! plan describes: tagB 0 (B-absent fallback) and the canonical
//! "ambivalent" tag (11 / 14 / 17 / 20) both yield `Nullness::Ambivalent`;
//! the other two values yield `WithNull` / `WithoutNull`.

use crate::error::ImportError;
use crate::fsharp_pickle::measure::read_measure_expr;
use crate::fsharp_pickle::model::{
    Nullness, PickledTcRef, PickledType, PickledUCaseRef, TupleKind,
};
use crate::fsharp_pickle::osgn::PhaseOneState;
use crate::fsharp_pickle::reader::PickleReader;
use crate::fsharp_pickle::typar::read_tyar_specs;

/// Project a B-stream tag for `TType_app`-from-simpletyp (tag 1) onto a
/// `Nullness`. The "0" value here is the B-absent fallback; the others
/// come straight from the canonical assignments at `:2509-2521`.
fn nullness_from_b_app_simple(tag_b: u8, context: &'static str) -> Result<Nullness, ImportError> {
    match tag_b {
        0 | 11 => Ok(Nullness::Ambivalent),
        9 => Ok(Nullness::WithNull),
        10 => Ok(Nullness::WithoutNull),
        _ => Err(ImportError::UnsupportedPickleTag {
            context,
            tag: u32::from(tag_b),
        }),
    }
}

/// `TType_app(tcref, args, _)` — `:2528-2533`.
fn nullness_from_b_app(tag_b: u8, context: &'static str) -> Result<Nullness, ImportError> {
    match tag_b {
        0 | 14 => Ok(Nullness::Ambivalent),
        12 => Ok(Nullness::WithNull),
        13 => Ok(Nullness::WithoutNull),
        _ => Err(ImportError::UnsupportedPickleTag {
            context,
            tag: u32::from(tag_b),
        }),
    }
}

/// `TType_fun(d, r, _)` — `:2539-2544`.
fn nullness_from_b_fun(tag_b: u8, context: &'static str) -> Result<Nullness, ImportError> {
    match tag_b {
        0 | 17 => Ok(Nullness::Ambivalent),
        15 => Ok(Nullness::WithNull),
        16 => Ok(Nullness::WithoutNull),
        _ => Err(ImportError::UnsupportedPickleTag {
            context,
            tag: u32::from(tag_b),
        }),
    }
}

/// `TType_var(tpref, _)` — `:2549-2554`.
fn nullness_from_b_var(tag_b: u8, context: &'static str) -> Result<Nullness, ImportError> {
    match tag_b {
        0 | 20 => Ok(Nullness::Ambivalent),
        18 => Ok(Nullness::WithNull),
        19 => Ok(Nullness::WithoutNull),
        _ => Err(ImportError::UnsupportedPickleTag {
            context,
            tag: u32::from(tag_b),
        }),
    }
}

/// Read an entity reference. `:1942-1948`.
///
/// Both branches are decoded eagerly and bounds-checked at decode time:
/// `ERefLocal` (tag 0) checks its stamp against the entity OSGN table —
/// matching FCS's `u_osgn_ref` (`TypedTreePickle.fs:596-602`), which
/// rejects out-of-range stamps before the linked body is reached — and
/// `ERefNonLocal` (tag 1) checks its index against the phase-2 `nlerefs`
/// table, so a `PickledTcRef::NonLocal` always dereferences safely.
pub(crate) fn read_tcref(state: &mut PhaseOneState<'_>) -> Result<PickledTcRef, ImportError> {
    let tag = state.reader.read_byte("u_tcref tag")?;
    match tag {
        0 => {
            let stamp = state
                .itycons
                .read_ref(&mut state.reader, "u_tcref entity-osgn index")?;
            Ok(PickledTcRef::Local(stamp))
        }
        1 => {
            let idx = state.reader.read_uint32("u_tcref nleref-index")?;
            if idx as usize >= state.nlerefs_len {
                return Err(ImportError::DanglingPickleRef {
                    kind: "nleref",
                    index: idx,
                });
            }
            Ok(PickledTcRef::NonLocal(idx))
        }
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_tcref tag",
            tag: u32::from(other),
        }),
    }
}

/// `u_ucref = u_tup2 u_tcref u_string` (`:1950-1952`). `u_string`
/// itself indexes into the strings table (`:831`), so the encoded
/// payload is `(tcref, string_index)`.
pub(crate) fn read_ucref(state: &mut PhaseOneState<'_>) -> Result<PickledUCaseRef, ImportError> {
    let tcref = read_tcref(state)?;
    let case_name_index = state.reader.read_uint32("u_ucref case-name string-index")?;
    Ok(PickledUCaseRef {
        tcref,
        case_name_index,
    })
}

/// `u_simpletyp = lookup_uniq isimpletys (u_int st)` (`:915`). The
/// stream-encoded value is the simpletys-table index, surfaced raw; the
/// caller (`read_ty` tag 1, the only one) bounds-checks it against the
/// phase-2 table before storing it.
pub(crate) fn read_simpletyp_index(reader: &mut PickleReader<'_>) -> Result<u32, ImportError> {
    reader.read_uint32("u_simpletyp index")
}

/// `u_tpref = u_local_item_ref itypars st` (`:1958`). FCS's
/// `u_osgn_ref` (`:596-602`) bounds-checks the stamp against the
/// table's preallocated capacity at decode time; we mirror that so a
/// malformed signature surfaces `OsgnIndexOutOfRange` here rather than
/// leaving an invalid index in `PickledType::Var`.
pub(crate) fn read_tpref(state: &mut PhaseOneState<'_>) -> Result<u32, ImportError> {
    state
        .itypars
        .read_ref(&mut state.reader, "u_tpref typar-osgn index")
}

/// `u_tys = u_list u_ty` (`:2007`).
pub(crate) fn read_tys(state: &mut PhaseOneState<'_>) -> Result<Vec<PickledType>, ImportError> {
    state.read_array("u_tys element", read_ty)
}

/// `u_ty` — `:2497-2579`. See module doc for the per-tag breakdown.
///
/// Takes `&mut PhaseOneState` because tag 5 (`Forall`) recurses through
/// `u_tyar_specs`, which writes into the typar OSGN table. The
/// non-Forall variants only touch the reader, accessed via
/// `state.reader.X()` (`PhaseOneState`'s fields are `pub(crate)` so the
/// borrow checker can see disjoint field borrows when a tag both reads
/// from the reader and recurses through OSGN-touching paths).
///
/// Depth-guarded: tags 3 (`Fun`) and 5 (`Forall`) self-recurse after
/// consuming a single primary byte, so a malformed run of tag bytes
/// drives one stack frame per byte without the guard.
pub(crate) fn read_ty(state: &mut PhaseOneState<'_>) -> Result<PickledType, ImportError> {
    state.reader.enter_recursion("u_ty")?;
    let result = read_ty_body(state);
    state.reader.exit_recursion();
    result
}

fn read_ty_body(state: &mut PhaseOneState<'_>) -> Result<PickledType, ImportError> {
    let tag = state.reader.read_byte("u_ty tag")?;
    match tag {
        0 => {
            let elems = read_tys(state)?;
            Ok(PickledType::Tuple {
                kind: TupleKind::Reference,
                elems,
            })
        }
        1 => {
            let tag_b = state.reader.read_byte_b("u_ty 1/B");
            let simpletyp_index = read_simpletyp_index(&mut state.reader)?;
            // Bounds-check against the phase-2 table at decode time
            // (matching OSGN stamps and `ERefNonLocal`): a stored
            // `AppSimple` index must always dereference safely.
            if simpletyp_index as usize >= state.simpletys_len {
                return Err(ImportError::DanglingPickleRef {
                    kind: "simpletyp",
                    index: simpletyp_index,
                });
            }
            let nullness = nullness_from_b_app_simple(tag_b, "u_ty - 1/B")?;
            Ok(PickledType::AppSimple {
                simpletyp_index,
                nullness,
            })
        }
        2 => {
            let tag_b = state.reader.read_byte_b("u_ty 2/B");
            let tcref = read_tcref(state)?;
            let args = read_tys(state)?;
            let nullness = nullness_from_b_app(tag_b, "u_ty - 2/B")?;
            Ok(PickledType::App {
                tcref,
                args,
                nullness,
            })
        }
        3 => {
            let tag_b = state.reader.read_byte_b("u_ty 3/B");
            let domain = Box::new(read_ty(state)?);
            let range = Box::new(read_ty(state)?);
            let nullness = nullness_from_b_fun(tag_b, "u_ty - 3/B")?;
            Ok(PickledType::Fun {
                domain,
                range,
                nullness,
            })
        }
        4 => {
            let tag_b = state.reader.read_byte_b("u_ty 4/B");
            let typar_index = read_tpref(state)?;
            let nullness = nullness_from_b_var(tag_b, "u_ty - 4/B")?;
            Ok(PickledType::Var {
                typar_index,
                nullness,
            })
        }
        5 => {
            // `TType_forall(tps, ty)` — `:2545`. `u_tyar_specs` writes
            // each freshly-decoded typar into the typar OSGN table; the
            // body's `TType_var` references resolve back through the
            // same table at projection time.
            let typars = read_tyar_specs(state)?;
            let body = Box::new(read_ty(state)?);
            Ok(PickledType::Forall { typars, body })
        }
        6 => {
            let m = read_measure_expr(state)?;
            Ok(PickledType::Measure(m))
        }
        7 => {
            let ucref = read_ucref(state)?;
            let args = read_tys(state)?;
            Ok(PickledType::UCase { ucref, args })
        }
        8 => {
            let elems = read_tys(state)?;
            Ok(PickledType::Tuple {
                kind: TupleKind::Struct,
                elems,
            })
        }
        9 => Err(ImportError::UnsupportedPickleTag {
            context: "u_ty tag 9 (Anon — no MiniLibFs fixture exercises it)",
            tag: 9,
        }),
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_ty tag",
            tag: u32::from(other),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsharp_pickle::model::Measure;

    /// Helper: build a one-byte literal compressed-int encoding.
    fn b(v: u8) -> u8 {
        v
    }

    /// Helper: encode `u_array` length n followed by raw payload bytes.
    fn arr(n: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![n];
        out.extend_from_slice(payload);
        out
    }

    /// Helper: encode a non-local tcref (tag 1 then nleref-index 0).
    fn nleref_tcref(nleref_idx: u8) -> Vec<u8> {
        vec![1, nleref_idx]
    }

    /// Build a phase-1 state with capacities sufficient for every
    /// synthetic fixture in this module's tests (highest hand-coded
    /// stamp is around 11). 256 leaves headroom without making it
    /// look like a meaningful number.
    fn empty_state<'a>(bytes: &'a [u8]) -> PhaseOneState<'a> {
        PhaseOneState::with_capacities(PickleReader::new(bytes), 256, 256, 256)
    }

    fn empty_state_dual<'a>(primary: &'a [u8], b: &'a [u8]) -> PhaseOneState<'a> {
        PhaseOneState::with_capacities(PickleReader::new_dual(primary, Some(b)), 256, 256, 256)
    }

    fn read_one(bytes: &[u8]) -> Result<PickledType, ImportError> {
        let mut s = empty_state(bytes);
        let t = read_ty(&mut s)?;
        assert!(
            s.reader.is_eof(),
            "decoder should consume the whole fixture; {} bytes left",
            s.reader.remaining()
        );
        Ok(t)
    }

    fn read_one_dual(primary: &[u8], b: &[u8]) -> Result<PickledType, ImportError> {
        let mut s = empty_state_dual(primary, b);
        let t = read_ty(&mut s)?;
        assert!(
            s.reader.is_eof(),
            "decoder should consume the whole primary; {} bytes left",
            s.reader.remaining()
        );
        Ok(t)
    }

    #[test]
    fn tag_0_tuple_reference_empty() {
        // tag 0, then u_tys = u_list = length 0.
        let bytes = [b(0), b(0)];
        let t = read_one(&bytes).unwrap();
        assert_eq!(
            t,
            PickledType::Tuple {
                kind: TupleKind::Reference,
                elems: vec![],
            }
        );
    }

    #[test]
    fn tag_0_tuple_reference_with_elements() {
        // Two reference-tuple elements, each a Var{typar_index=5, ambivalent} via tag 4 with B-absent.
        let inner = [b(4), b(5)]; // primary bytes for Var(5)
        let mut bytes = vec![b(0), b(2)];
        bytes.extend_from_slice(&inner);
        bytes.extend_from_slice(&inner);
        let t = read_one(&bytes).unwrap();
        match t {
            PickledType::Tuple { kind, elems } => {
                assert_eq!(kind, TupleKind::Reference);
                assert_eq!(elems.len(), 2);
                for e in elems {
                    assert_eq!(
                        e,
                        PickledType::Var {
                            typar_index: 5,
                            nullness: Nullness::Ambivalent,
                        }
                    );
                }
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
    }

    #[test]
    fn tag_1_simpletyp_index_validated_against_table() {
        // Primary: tag 1, simpletyp index = 3 — but only 2 simpletys entries.
        let primary = [b(1), b(3)];
        let mut s = empty_state(&primary);
        s.simpletys_len = 2;
        match read_ty(&mut s) {
            Err(ImportError::DanglingPickleRef {
                kind: "simpletyp",
                index: 3,
            }) => {}
            other => panic!("expected DanglingPickleRef for simpletyp, got {other:?}"),
        }
    }

    #[test]
    fn nonlocal_tcref_index_validated_against_nlerefs_table() {
        // tcref tag 1 (NonLocal), nleref index = 5 — but only 2 nlerefs.
        let bytes = nleref_tcref(5);
        let mut s = empty_state(&bytes);
        s.nlerefs_len = 2;
        match read_tcref(&mut s) {
            Err(ImportError::DanglingPickleRef {
                kind: "nleref",
                index: 5,
            }) => {}
            other => panic!("expected DanglingPickleRef for nleref, got {other:?}"),
        }
    }

    #[test]
    fn tag_1_app_simple_b_absent_yields_ambivalent() {
        // Primary: tag 1, simpletyp index = 3.
        let primary = [b(1), b(3)];
        let t = read_one(&primary).unwrap();
        assert_eq!(
            t,
            PickledType::AppSimple {
                simpletyp_index: 3,
                nullness: Nullness::Ambivalent,
            }
        );
    }

    #[test]
    fn tag_1_app_simple_b_nullness_values() {
        let primary = [b(1), b(3)];
        // tagB = 9 -> WithNull
        let t = read_one_dual(&primary, &[9]).unwrap();
        assert_eq!(
            t,
            PickledType::AppSimple {
                simpletyp_index: 3,
                nullness: Nullness::WithNull,
            }
        );
        // tagB = 10 -> WithoutNull
        let t = read_one_dual(&primary, &[10]).unwrap();
        assert_eq!(
            t,
            PickledType::AppSimple {
                simpletyp_index: 3,
                nullness: Nullness::WithoutNull,
            }
        );
        // tagB = 11 -> Ambivalent (canonical)
        let t = read_one_dual(&primary, &[11]).unwrap();
        assert_eq!(
            t,
            PickledType::AppSimple {
                simpletyp_index: 3,
                nullness: Nullness::Ambivalent,
            }
        );
    }

    #[test]
    fn tag_1_app_simple_rejects_unknown_b_tag() {
        let primary = [b(1), b(3)];
        match read_one_dual(&primary, &[42]) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_ty - 1/B",
                tag: 42,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn tag_2_app_full_decodes() {
        // tag 2, B-stream absent (Ambivalent), tcref = NonLocal(7), args = [Var(2)]
        let inner_var = [b(4), b(2)];
        let mut bytes = vec![b(2)];
        bytes.extend(nleref_tcref(7));
        bytes.extend(arr(1, &inner_var));
        let t = read_one(&bytes).unwrap();
        assert_eq!(
            t,
            PickledType::App {
                tcref: PickledTcRef::NonLocal(7),
                args: vec![PickledType::Var {
                    typar_index: 2,
                    nullness: Nullness::Ambivalent
                }],
                nullness: Nullness::Ambivalent,
            }
        );

        // tagB = 12 -> WithNull
        let t = read_one_dual(&bytes, &[12]).unwrap();
        match t {
            PickledType::App { nullness, .. } => assert_eq!(nullness, Nullness::WithNull),
            other => panic!("expected App, got {other:?}"),
        }
        // tagB = 13 -> WithoutNull
        let t = read_one_dual(&bytes, &[13]).unwrap();
        match t {
            PickledType::App { nullness, .. } => assert_eq!(nullness, Nullness::WithoutNull),
            other => panic!("expected App, got {other:?}"),
        }
        // tagB = 14 -> Ambivalent (canonical)
        let t = read_one_dual(&bytes, &[14]).unwrap();
        match t {
            PickledType::App { nullness, .. } => assert_eq!(nullness, Nullness::Ambivalent),
            other => panic!("expected App, got {other:?}"),
        }
    }

    #[test]
    fn tag_3_fun_b_tags() {
        // tag 3, then two trivial Var(0)s. B-absent → Ambivalent.
        let inner_var = [b(4), b(0)];
        let mut primary = vec![b(3)];
        primary.extend_from_slice(&inner_var);
        primary.extend_from_slice(&inner_var);
        let t = read_one(&primary).unwrap();
        assert!(matches!(
            t,
            PickledType::Fun {
                nullness: Nullness::Ambivalent,
                ..
            }
        ));

        for (tag_b, expected) in [
            (15, Nullness::WithNull),
            (16, Nullness::WithoutNull),
            (17, Nullness::Ambivalent),
        ] {
            let t = read_one_dual(&primary, &[tag_b]).unwrap();
            match t {
                PickledType::Fun { nullness, .. } => assert_eq!(nullness, expected),
                other => panic!("expected Fun, got {other:?}"),
            }
        }
    }

    #[test]
    fn tag_4_var_b_tags() {
        let primary = [b(4), b(11)];
        let t = read_one(&primary).unwrap();
        assert_eq!(
            t,
            PickledType::Var {
                typar_index: 11,
                nullness: Nullness::Ambivalent,
            }
        );
        for (tag_b, expected) in [
            (18u8, Nullness::WithNull),
            (19, Nullness::WithoutNull),
            (20, Nullness::Ambivalent),
        ] {
            let t = read_one_dual(&primary, &[tag_b]).unwrap();
            match t {
                PickledType::Var { nullness, .. } => assert_eq!(nullness, expected),
                other => panic!("expected Var, got {other:?}"),
            }
        }
    }

    #[test]
    fn tag_5_forall_decodes_with_typar_osgn() {
        // tag 5, then u_tyar_specs = u_list of one u_tyar_spec.
        //
        // u_tyar_spec wire: compressed-int idx, then u_tyar_spec_data
        // body. The body is `u_ident, u_attribs, u_int64, u_tyar_constraints, u_xmldoc`
        // (`TypedTreePickle.fs:2389-2411`); we craft the smallest one:
        // ident = ("", range0), no attribs, flags 0, no constraints,
        // empty xmldoc.
        //
        // u_ident = u_string idx 0, u_range (file idx 0, start/end positions).
        // We need a strings table with at least one entry (the typar name).
        let strings = vec!["T".to_string()];
        let pubpaths: Vec<Vec<u32>> = vec![];

        let mut body = vec![
            b(5), // u_ty tag = Forall
            b(1), // u_tyar_specs list length = 1
            b(0), // u_tyar_spec osgn index = 0
            // u_tyar_spec_data:
            b(0), // u_ident: u_string idx 0
            b(0),
            b(0),
            b(0),
            b(0),
            b(0), // u_range: file 0, start(0,0), end(0,0) — 5 compressed-int zeros
            b(0), // u_attribs: length 0
            b(0),
            b(0), // u_int64 flags = 0 (two compressed-int zeros)
            b(0), // u_tyar_constraints primary list length = 0
            b(0), // u_xmldoc: length 0
        ];
        // Body of TType_forall: a Var(0) referencing the typar we just declared.
        body.extend_from_slice(&[b(4), b(0)]);

        let mut r = PickleReader::new(&body);
        r.attach_tables(&strings, &pubpaths);
        let mut s = PhaseOneState::with_capacities(r, 0, 1, 0);
        let t = read_ty(&mut s).unwrap();
        match t {
            PickledType::Forall { typars, body } => {
                assert_eq!(typars, vec![0]);
                assert_eq!(
                    *body,
                    PickledType::Var {
                        typar_index: 0,
                        nullness: Nullness::Ambivalent,
                    }
                );
            }
            other => panic!("expected Forall, got {other:?}"),
        }
        assert!(s.reader.is_eof());
    }

    #[test]
    fn tag_6_measure_one() {
        // tag 6, then Measure.One = tag 4 in the measure stream.
        let bytes = [b(6), b(4)];
        let t = read_one(&bytes).unwrap();
        assert_eq!(t, PickledType::Measure(Measure::One));
    }

    #[test]
    fn tag_7_ucase_decodes() {
        // tag 7, tcref = NonLocal(1), case name idx = 4, args = []
        let bytes = [b(7), 1, b(1), b(4), b(0)];
        let t = read_one(&bytes).unwrap();
        assert_eq!(
            t,
            PickledType::UCase {
                ucref: PickledUCaseRef {
                    tcref: PickledTcRef::NonLocal(1),
                    case_name_index: 4,
                },
                args: vec![],
            }
        );
    }

    #[test]
    fn tag_8_tuple_struct() {
        let bytes = [b(8), b(0)];
        let t = read_one(&bytes).unwrap();
        assert_eq!(
            t,
            PickledType::Tuple {
                kind: TupleKind::Struct,
                elems: vec![],
            }
        );
    }

    #[test]
    fn tag_9_anon_is_deferred() {
        let bytes = [b(9)];
        let mut s = empty_state(&bytes);
        match read_ty(&mut s) {
            Err(ImportError::UnsupportedPickleTag { context, tag: 9 }) => {
                assert!(
                    context.contains("Anon"),
                    "context should name Anon: {context}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn unknown_top_level_tag_errors() {
        let bytes = [b(42)];
        let mut s = empty_state(&bytes);
        match read_ty(&mut s) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_ty tag",
                tag: 42,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn u_tcref_local_decodes_stamp() {
        // tag 0 then compressed-int 7 → Local(7). The entity OSGN
        // table size 16 leaves stamp 7 in-range; resolution happens at
        // projection time, but bounds-checking is eager.
        let bytes = [b(0), b(7)];
        let mut s = PhaseOneState::with_capacities(PickleReader::new(&bytes), 16, 0, 0);
        assert_eq!(read_tcref(&mut s).unwrap(), PickledTcRef::Local(7));
        assert!(s.reader.is_eof());
    }

    #[test]
    fn u_tcref_local_out_of_range_errors() {
        // tag 0 then compressed-int 7, but the entity OSGN table has
        // only 5 slots — eager bounds-check catches it.
        let bytes = [b(0), b(7)];
        let mut s = PhaseOneState::with_capacities(PickleReader::new(&bytes), 5, 0, 0);
        match read_tcref(&mut s) {
            Err(ImportError::OsgnIndexOutOfRange {
                kind: "tycons",
                index: 7,
                max: 5,
            }) => {}
            other => panic!("expected OsgnIndexOutOfRange for tycons, got {other:?}"),
        }
    }

    #[test]
    fn u_tpref_out_of_range_errors() {
        // tpref idx 3 against a typar OSGN table of size 2.
        let bytes = [b(3)];
        let mut s = PhaseOneState::with_capacities(PickleReader::new(&bytes), 0, 2, 0);
        match read_tpref(&mut s) {
            Err(ImportError::OsgnIndexOutOfRange {
                kind: "typars",
                index: 3,
                max: 2,
            }) => {}
            other => panic!("expected OsgnIndexOutOfRange for typars, got {other:?}"),
        }
    }

    #[test]
    fn u_tcref_rejects_unknown_tag() {
        let bytes = [b(7)];
        let mut s = PhaseOneState::with_capacities(PickleReader::new(&bytes), 0, 0, 0);
        match read_tcref(&mut s) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_tcref tag",
                tag: 7,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
