//! `u_expr` — FCS's expression-tree pickler, as reached from attribute
//! arguments.
//!
//! The signature pickle carries `u_expr` payloads inside attribute-argument
//! position (`u_attrib_expr` at `TypedTreePickle.fs:3232-3244`), and FCS's
//! pickler writes the *full* original expression there (`p_attrib_expr` at
//! `:2878-2889` only normalises a literal `Expr.Val` to `Expr.Const`). So
//! e.g. `[<AttributeUsage(AttributeTargets.Method ||| AttributeTargets.Property)>]`
//! pickles the inline `(|||)` operator as `App(Lambda(…, Op(ILAsm[or], …)))`,
//! not a folded constant. These exprs are written inline with no byte-length
//! framing, so the decoder must walk the whole tree to stay aligned —
//! decoding attribute arguments cannot be skipped.
//!
//! ## Alignment-only, with two structured shapes kept
//!
//! Nothing in the cross-CCU view consumes attribute-argument *values* (the
//! measure / extension overlays read tycon kinds and `ValFlags`, never
//! attribute-arg expressions). So most arms are decoded purely for their wire
//! structure — and for the *side effects* of the osgn-publishing sub-decoders
//! they reach (`u_Val` links a val stamp, `u_tyar_specs` links typars) — then
//! collapsed to [`PickledExpr::Other`]. Two shapes a *literal* attribute
//! argument actually takes are kept structured for inspectability:
//! `Const`/`Val`/`App` (a `typeof<T>` argument pickles as
//! `App(Val(typeof), [T], [])`, head `App` = tag 6, callee `Val` = tag 1),
//! and the two `Expr.Op`s `CheckAttribArgExpr` admits literally —
//! `TOp.Array` (`[<Attr([| … |])>]`) and `TOp.Coerce` (a literal passed to an
//! `obj`-typed parameter). `read_op` decodes any operator (and its payload);
//! only those two keep a value.
//!
//! ## Still loud-on-unknown (D6.5)
//!
//! Wire shapes no FSharp.Core (or fixture) pickle has reached are left as
//! hard errors so the first occurrence pinpoints itself rather than
//! corrupting the stream: `Expr.Match` (tag 9) and `Expr.Obj` (tag 10) — which
//! pull in the decision-tree / object-expression sub-decoders
//! (`u_dtree`/`u_target`, `u_method`/`u_intf`); the `u_op` operators that
//! carry a record-field ref / `LValueOp` / `ILCall` / byte blob / anon-record
//! info (tags 4/5/17/18/22/25/26/31/32); the payload-bearing `u_ILInstr`
//! opcodes (only the no-argument arithmetic/bitwise set is ported); and the
//! `u_trait_sln` arms needing `u_rfref`/`u_anonInfo` (arms 4/5). The
//! `u_lazy`-framed `u_modul_typ` length check
//! ([`MalformedPickleLazyFrame`](crate::ImportError::MalformedPickleLazyFrame))
//! is the backstop catching any drift a new arm introduces inside a module
//! body.

use crate::error::ImportError;
use crate::fsharp_pickle::access::read_dummy_range;
use crate::fsharp_pickle::constraints::read_trait;
use crate::fsharp_pickle::consts::read_const;
use crate::fsharp_pickle::model::PickledExpr;
use crate::fsharp_pickle::osgn::PhaseOneState;
use crate::fsharp_pickle::typar::read_tyar_specs;
use crate::fsharp_pickle::types::{read_tcref, read_ty, read_tys, read_ucref};
use crate::fsharp_pickle::val::read_val;
use crate::fsharp_pickle::vrefs::read_vref;

/// `u_vrefFlags` (`TypedTreePickle.fs:3269-3276`). A tag byte selecting a
/// `ValUseFlag`; only tag 3 (`PossibleConstrainedCall`) carries a
/// payload (an extra `u_ty`). The flag drives typechecking and has no
/// cross-CCU consumer, so — like the dropped `u_dummy_range` — we decode
/// it purely to keep the stream aligned and discard the result.
fn read_vref_flags(state: &mut PhaseOneState<'_>) -> Result<(), ImportError> {
    let tag = state.reader.read_byte("u_vrefFlags tag")?;
    match tag {
        0 | 1 | 2 | 4 => Ok(()),
        3 => {
            // PossibleConstrainedCall(ty): decode and discard.
            read_ty(state)?;
            Ok(())
        }
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_vrefFlags tag",
            tag: u32::from(other),
        }),
    }
}

/// Decode one `u_expr` (`TypedTreePickle.fs:3795`). Keeps a structured value
/// for the literal attribute-argument shapes and collapses the rest to
/// [`PickledExpr::Other`] after consuming their wire structure; see the
/// module docs for the coverage and the loud-on-unknown set.
///
/// Raises `UnsupportedPickleExpr` for an unported arm (`Match`/`Obj`). The
/// `context` string is threaded through to the error — and through every
/// recursive sub-expression — so the originating attribute position appears
/// in the diagnostic (e.g. "u_attrib_expr orig").
///
/// Depth-guarded: several arms (`Sequential`, `App`, …) self-recurse
/// after consuming very few bytes, so a malformed stream drives stack
/// frames near-linearly in its byte length without the guard.
pub(crate) fn read_expr(
    state: &mut PhaseOneState<'_>,
    context: &'static str,
) -> Result<PickledExpr, ImportError> {
    state.reader.enter_recursion("u_expr")?;
    let result = read_expr_body(state, context);
    state.reader.exit_recursion();
    result
}

fn read_expr_body(
    state: &mut PhaseOneState<'_>,
    context: &'static str,
) -> Result<PickledExpr, ImportError> {
    let tag = state.reader.read_byte("u_expr tag")?;
    match tag {
        // Expr.Const(c, _, ty)
        0 => {
            let value = read_const(&mut state.reader)?;
            read_dummy_range(&mut state.reader)?;
            let ty = read_ty(state)?;
            Ok(PickledExpr::Const { value, ty })
        }
        // Expr.Val(vref, _flags, _range)
        1 => {
            let vref = read_vref(state)?;
            read_vref_flags(state)?;
            read_dummy_range(&mut state.reader)?;
            Ok(PickledExpr::Val(vref))
        }
        // Expr.Op(op, opTys, args, _range). `read_op` consumes the operator
        // (and any operator-specific payload); `TOp.Array`/`TOp.Coerce` keep a
        // structured value (the two ops that reach a *literal* attribute
        // argument), and every other operator collapses to `Other` — the
        // operands still recurse so the stream stays aligned.
        2 => {
            let op = read_op(state)?;
            // opTys: decoded for alignment, dropped.
            read_tys(state)?;
            let mut args = state.read_array("u_Exprs (Op arg)", |s| read_expr(s, context))?;
            read_dummy_range(&mut state.reader)?;
            match op {
                OpKind::Array => Ok(PickledExpr::Array { elements: args }),
                OpKind::Coerce => {
                    // FCS always pickles `TOp.Coerce` with exactly one operand
                    // (`Expr.Op(TOp.Coerce, _, [arg], _)`).
                    if args.len() != 1 {
                        return Err(ImportError::UnsupportedPickleTag {
                            context: "u_op TOp.Coerce operand count (expected 1)",
                            tag: args.len() as u32,
                        });
                    }
                    Ok(PickledExpr::Coerce {
                        arg: Box::new(args.pop().expect("len checked == 1")),
                    })
                }
                OpKind::Other => Ok(PickledExpr::Other { tag: 2 }),
            }
        }
        // Expr.Sequential(e1, e2, dir, _)
        3 => {
            read_expr(state, context)?;
            read_expr(state, context)?;
            // FCS maps the flag to `NormalSeq` (0) / `ThenDoSeq` (1) and
            // rejects anything else as `specialSeqFlag`; mirror that so a
            // corrupted flag fails loud rather than decoding as `Other`.
            let dir = state.reader.read_uint32("u_expr Sequential dir")?;
            if dir > 1 {
                return Err(ImportError::UnsupportedPickleTag {
                    context: "u_expr Sequential special-sequence flag (expected 0 or 1)",
                    tag: dir,
                });
            }
            read_dummy_range(&mut state.reader)?;
            Ok(PickledExpr::Other { tag: 3 })
        }
        // Expr.Lambda(_, baseValOpt, ctorThisValOpt, vals, body, _, _ty).
        // The `u_Val`s publish into the val OSGN table, so they must be
        // decoded, not skipped.
        4 => {
            state.read_option("u_expr Lambda baseVal", read_val)?;
            state.read_option("u_expr Lambda ctorThisVal", read_val)?;
            state.read_array("u_expr Lambda vals", read_val)?;
            read_expr(state, context)?;
            read_dummy_range(&mut state.reader)?;
            read_ty(state)?;
            Ok(PickledExpr::Other { tag: 4 })
        }
        // Expr.TyLambda(_, tps, body, _, _ty). `u_tyar_specs` publishes typars.
        5 => {
            read_tyar_specs(state)?;
            read_expr(state, context)?;
            read_dummy_range(&mut state.reader)?;
            read_ty(state)?;
            Ok(PickledExpr::Other { tag: 5 })
        }
        // Expr.App(func, _funcTy, tyArgs, args, _range)
        6 => {
            let func = read_expr(state, context)?;
            // The function-value type carries no information the cross-CCU
            // view consumes; decode it for alignment and drop it.
            read_ty(state)?;
            let ty_args = read_tys(state)?;
            let args = state.read_array("u_Exprs (App arg)", |s| read_expr(s, context))?;
            read_dummy_range(&mut state.reader)?;
            Ok(PickledExpr::App {
                func: Box::new(func),
                ty_args,
                args,
            })
        }
        // Expr.LetRec(binds, body, _, _)
        7 => {
            read_binds(state, context)?;
            read_expr(state, context)?;
            read_dummy_range(&mut state.reader)?;
            Ok(PickledExpr::Other { tag: 7 })
        }
        // Expr.Let(bind, body, _, _)
        8 => {
            read_bind(state, context)?;
            read_expr(state, context)?;
            read_dummy_range(&mut state.reader)?;
            Ok(PickledExpr::Other { tag: 8 })
        }
        // Expr.StaticOptimization(constraints, e1, e2, _)
        11 => {
            state.read_array(
                "u_expr StaticOptimization constraints",
                read_static_optimization_constraint,
            )?;
            read_expr(state, context)?;
            read_expr(state, context)?;
            read_dummy_range(&mut state.reader)?;
            Ok(PickledExpr::Other { tag: 11 })
        }
        // Expr.TyChoose(tps, body, _). `u_tyar_specs` publishes typars.
        12 => {
            read_tyar_specs(state)?;
            read_expr(state, context)?;
            read_dummy_range(&mut state.reader)?;
            Ok(PickledExpr::Other { tag: 12 })
        }
        // Expr.Quote(e, _ref, _, _, _ty)
        13 => {
            read_expr(state, context)?;
            read_dummy_range(&mut state.reader)?;
            read_ty(state)?;
            Ok(PickledExpr::Other { tag: 13 })
        }
        // Expr.WitnessArg(traitInfo, _)
        14 => {
            read_trait(state)?;
            read_dummy_range(&mut state.reader)?;
            Ok(PickledExpr::Other { tag: 14 })
        }
        // Tags 9 (`Match`) and 10 (`Obj`) pull in the decision-tree /
        // object-expression sub-decoders (`u_dtree`/`u_target`,
        // `u_method`/`u_intf`) and have not been observed in
        // attribute-argument position; loud-on-unknown (D6.5) so the first
        // one to appear pinpoints itself.
        other => Err(ImportError::UnsupportedPickleExpr {
            context,
            tag: u32::from(other),
        }),
    }
}

/// Which `TOp` a `u_op` decoded to, reduced to what the attribute-argument
/// view distinguishes: `Array` and `Coerce` keep a structured
/// [`PickledExpr`], everything else collapses to [`PickledExpr::Other`].
enum OpKind {
    Array,
    Coerce,
    Other,
}

/// `u_op` (`TypedTreePickle.fs:3630-3729`). Reads the operator tag and
/// consumes any operator-specific payload so the enclosing `Expr.Op`'s
/// `opTys`/`args`/range follow at the right offset. The decoded operator
/// itself is dropped (nothing in the cross-CCU view inspects it) beyond the
/// `Array`/`Coerce` discriminator.
///
/// The operators that carry a record-field ref (`ValFieldGet/Set/Addr` —
/// tags 4/5/25), an `LValueOp` (17), an `ILCall` (18), a byte/uint16 blob
/// (22/26), or anon-record info (31/32) are not yet ported: none has been
/// observed in attribute-argument position, so they stay loud-on-unknown.
fn read_op(state: &mut PhaseOneState<'_>) -> Result<OpKind, ImportError> {
    let tag = state.reader.read_byte("u_op tag")?;
    Ok(match tag {
        // UnionCase / UnionCaseProof — `u_ucref`.
        0 | 14 => {
            read_ucref(state)?;
            OpKind::Other
        }
        // ExnConstr / Recd / UnionCaseTagGet — `u_tcref`.
        1 | 3 | 6 => {
            read_tcref(state)?;
            OpKind::Other
        }
        // Tuple (ref) / RefAddrGet / While / TryWith / TryFinally / Reraise /
        // Tuple (struct) — no payload.
        2 | 13 | 20 | 23 | 24 | 27 | 29 => OpKind::Other,
        // UnionCaseFieldGet / Set / GetAddr — `u_ucref` + `u_int`.
        7 | 8 | 28 => {
            read_ucref(state)?;
            state.reader.read_uint32("u_op union-case field index")?;
            OpKind::Other
        }
        // ExnFieldGet / Set — `u_tcref` + `u_int`.
        9 | 10 => {
            read_tcref(state)?;
            state.reader.read_uint32("u_op exn field index")?;
            OpKind::Other
        }
        // TupleFieldGet (ref / struct) — `u_int`.
        11 | 30 => {
            state.reader.read_uint32("u_op tuple field index")?;
            OpKind::Other
        }
        // ILAsm(instrs, asmTys) — the inline-IL intrinsic behind FSharp.Core's
        // operators (`(# "or" … #)` for `(|||)`, …).
        12 => {
            state.read_array("u_op ILAsm instrs", read_il_instr)?;
            read_tys(state)?;
            OpKind::Other
        }
        // Coerce / Array — the two operators that keep a structured value.
        15 => OpKind::Coerce,
        19 => OpKind::Array,
        // TraitCall — `u_trait`.
        16 => {
            read_trait(state)?;
            OpKind::Other
        }
        // IntegerForLoop — direction `u_int`. FCS accepts only 0
        // (FSharpForLoopUp), 1 (CSharpForLoopUp), 2 (FSharpForLoopDown) and
        // fails otherwise; mirror that.
        21 => {
            let dir = state.reader.read_uint32("u_op IntegerForLoop direction")?;
            if dir > 2 {
                return Err(ImportError::UnsupportedPickleTag {
                    context: "u_op IntegerForLoop direction (expected 0, 1, or 2)",
                    tag: dir,
                });
            }
            OpKind::Other
        }
        other => {
            return Err(ImportError::UnsupportedPickleTag {
                context: "u_op tag (extend when FSharp.Core trips a new operator)",
                tag: u32::from(other),
            });
        }
    })
}

/// `u_ILInstr` (`TypedTreePickle.fs:1853`). FCS dispatches on the opcode byte
/// through `decode_tab` (`:1804`): the no-argument arithmetic / bitwise /
/// comparison instructions consume *only* the opcode byte, while the
/// payload-bearing ones (`decoders`, `:1707`) read a method-spec / field-spec
/// / IL-type / string / shape afterwards.
///
/// Inline FSharp.Core operators (`(|||)`, `(&&&)`, `(~~~)`, the arithmetic
/// ops) expand to the no-argument set, which is what reaches attribute-
/// argument position. We decode exactly that set; a payload-bearing opcode is
/// loud-on-unknown so the first one to appear pinpoints itself (it would also
/// require porting the IL-operand sub-decoders).
fn read_il_instr(state: &mut PhaseOneState<'_>) -> Result<(), ImportError> {
    let op = state.reader.read_byte("u_ILInstr opcode")?;
    match op {
        // nop, ldnull; add..not (5..19); throw(30); ldlen(40); ckfinite(42);
        // *_ovf (44..49); ceq..clt_un (50..54); localloc(56); rethrow(57);
        // initblk(64); cpblk(66) — all no-argument (`simple_instrs`).
        0 | 2 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15 | 16 | 17 | 18 | 19 | 30 | 40
        | 42 | 44 | 45 | 46 | 47 | 48 | 49 | 50 | 51 | 52 | 53 | 54 | 56 | 57 | 64 | 66 => Ok(()),
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_ILInstr opcode (no-argument subset; extend when a payload opcode appears)",
            tag: u32::from(other),
        }),
    }
}

/// `u_bind` (`TypedTreePickle.fs:3501`). `u_tup2 u_Val u_expr` — the `u_Val`
/// publishes into the val OSGN table. The decoded binding is dropped.
fn read_bind(state: &mut PhaseOneState<'_>, context: &'static str) -> Result<(), ImportError> {
    read_val(state)?;
    read_expr(state, context)?;
    Ok(())
}

/// `u_binds` (`:3959`). `u_List u_bind`.
fn read_binds(state: &mut PhaseOneState<'_>, context: &'static str) -> Result<(), ImportError> {
    state.read_array("u_binds element", |s| read_bind(s, context))?;
    Ok(())
}

/// `u_static_optimization_constraint` (`:3918-3924`). Tag 0 →
/// `TTyconEqualsTycon(ty, ty)`, tag 1 → `TTyconIsStruct(ty)`.
fn read_static_optimization_constraint(state: &mut PhaseOneState<'_>) -> Result<(), ImportError> {
    let tag = state
        .reader
        .read_byte("u_static_optimization_constraint tag")?;
    match tag {
        0 => {
            read_ty(state)?;
            read_ty(state)?;
            Ok(())
        }
        1 => {
            read_ty(state)?;
            Ok(())
        }
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_static_optimization_constraint tag",
            tag: u32::from(other),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsharp_pickle::model::{Nullness, PickledConst, PickledType, PickledVRef};
    use crate::fsharp_pickle::reader::PickleReader;

    fn make_state<'a>(bytes: &'a [u8], strings: &'a [String]) -> PhaseOneState<'a> {
        make_state_nvals(bytes, strings, 0)
    }

    fn make_state_nvals<'a>(
        bytes: &'a [u8],
        strings: &'a [String],
        nvals: usize,
    ) -> PhaseOneState<'a> {
        let mut r = PickleReader::new(bytes);
        let pubpaths: &'a [Vec<u32>] = &[];
        r.attach_tables(strings, pubpaths);
        PhaseOneState::with_capacities(r, 0, 0, nvals)
    }

    #[test]
    fn const_expr_with_bool_and_simple_type() {
        let strings: Vec<String> = vec![];
        // u_expr tag 0, u_const Bool(true), then u_ty AppSimple(simpletyp_idx=2)
        // with B-stream absent so nullness = Ambivalent.
        //
        // u_ty AppSimple wire form: tag 1, then `u_byteB` (absent → 0 →
        // Ambivalent), then `u_simpletyp` (compressed-int index).
        let bytes = [
            0u8, // u_expr tag = Const
            0u8, 1u8, // u_const Bool(true): tag 0, body 1
            // u_dummy_range reads zero bytes
            1u8, // u_ty tag = AppSimple
            2u8, // u_simpletyp index 2
        ];
        let mut s = make_state(&bytes, &strings);
        let e = read_expr(&mut s, "test ctx").unwrap();
        assert_eq!(
            e,
            PickledExpr::Const {
                value: PickledConst::Bool(true),
                ty: PickledType::AppSimple {
                    simpletyp_index: 2,
                    nullness: Nullness::Ambivalent,
                },
            },
        );
        assert!(s.reader.is_eof());
    }

    #[test]
    fn const_expr_with_string() {
        let strings = vec!["hello".to_string()];
        let bytes = [
            0u8, // u_expr tag = Const
            14u8, 0u8, // u_const String, index 0 → "hello"
            // dummy_range: no bytes
            1u8, // u_ty tag = AppSimple
            0u8, // simpletyp idx 0
        ];
        let mut s = make_state(&bytes, &strings);
        let e = read_expr(&mut s, "test ctx").unwrap();
        match e {
            PickledExpr::Const {
                value: PickledConst::String(s),
                ..
            } => assert_eq!(s, "hello"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn val_expr_decodes_local_vref() {
        // tag 1 = Expr.Val; u_vref Local(stamp 0); vrefFlags NormalValUse;
        // empty dummy range. Previously tag 1 hard-errored.
        let strings: Vec<String> = vec![];
        let bytes = [
            1u8, // u_expr tag = Val
            0u8, // u_vref tag = Local
            0u8, // u_vref Local stamp index 0
            0u8, // u_vrefFlags = NormalValUse
                 // u_dummy_range: no bytes
        ];
        let mut s = make_state_nvals(&bytes, &strings, 1);
        let e = read_expr(&mut s, "test ctx").unwrap();
        assert_eq!(e, PickledExpr::Val(PickledVRef::Local(0)));
        assert!(s.reader.is_eof());
    }

    #[test]
    fn app_expr_decodes_typeof_shape() {
        // The `typeof<int>` shape: App(Val(typeof), funcTy, [tyArg], [], _).
        // Self-contained bytes — the callee is a Local vref standing in for
        // the `typeof` intrinsic, the single type argument an AppSimple.
        let strings: Vec<String> = vec![];
        let bytes = [
            6u8, // u_expr tag = App
            // func = Val(Local 0):
            1u8, 0u8, 0u8, 0u8, // Val tag, vref Local, stamp 0, vrefFlags 0
            // funcTy (decoded + dropped): AppSimple idx 0
            1u8, 0u8, // tyArgs = u_list u_ty, length 1:
            1u8, // length 1
            1u8, 2u8, // tyArgs[0] = AppSimple idx 2
            // args = u_list u_expr, length 0:
            0u8,
            // u_dummy_range: no bytes
        ];
        let mut s = make_state_nvals(&bytes, &strings, 1);
        let e = read_expr(&mut s, "test ctx").unwrap();
        assert_eq!(
            e,
            PickledExpr::App {
                func: Box::new(PickledExpr::Val(PickledVRef::Local(0))),
                ty_args: vec![PickledType::AppSimple {
                    simpletyp_index: 2,
                    nullness: Nullness::Ambivalent,
                }],
                args: vec![],
            },
        );
        assert!(s.reader.is_eof());
    }

    #[test]
    fn expr_tag_witness_arg_decodes() {
        // tag 14 = Expr.WitnessArg(traitInfo, _). The trait is decoded for
        // alignment and collapses to `Other { tag: 14 }`.
        let strings = vec!["m".to_string()];
        let bytes = [
            14u8, // u_expr tag = WitnessArg
            // u_trait: support_tys(0), member_name idx 0, member_flags(6),
            // arg_tys(0), return_ty None, solution None.
            0, // support_tys len 0
            0, // member_name idx 0 = "m"
            0, 0, 0, 0, 0, 0, // member_flags (5 bools + kind)
            0, // arg_tys len 0
            0, // return_ty option None
            0, // solution option None
               // WitnessArg u_dummy_range: no bytes
        ];
        let mut s = make_state(&bytes, &strings);
        let e = read_expr(&mut s, "test").unwrap();
        assert_eq!(e, PickledExpr::Other { tag: 14 });
        assert!(s.reader.is_eof());
    }

    #[test]
    fn array_expr_empty() {
        // `[||]`: tag 2 (Expr.Op), u_op tag 19 (TOp.Array), empty u_tys,
        // empty u_Exprs. The empty array still pickles as an Op (verified
        // against a real `[<Attr([||])>]` fixture).
        let strings: Vec<String> = vec![];
        let bytes = [
            2u8,  // u_expr tag = Op
            19u8, // u_op tag = Array
            0u8,  // u_tys: list length 0
            0u8,  // u_Exprs: list length 0
                  // u_dummy_range: no bytes
        ];
        let mut s = make_state(&bytes, &strings);
        let e = read_expr(&mut s, "test ctx").unwrap();
        assert_eq!(e, PickledExpr::Array { elements: vec![] });
        assert!(s.reader.is_eof());
    }

    #[test]
    fn array_expr_of_int_consts() {
        // `[| 1; 2 |]`: tag 2, op 19, element type list [AppSimple 0],
        // then two `Expr.Const(Int32)` elements.
        let strings: Vec<String> = vec![];
        let bytes = [
            2u8,  // u_expr tag = Op
            19u8, // u_op tag = Array
            // u_tys = [AppSimple idx 0]
            1u8, // list length 1
            1u8, 0u8, // AppSimple idx 0
            // u_Exprs = [Const(Int32 1); Const(Int32 2)]
            2u8, // list length 2
            // element 0: Const(Int32 1) : AppSimple 0
            0u8, 5u8, 1u8, 1u8, 0u8, // tag Const, u_const Int32 tag, body 1, u_ty AppSimple 0
            // element 1: Const(Int32 2) : AppSimple 0
            0u8, 5u8, 2u8, 1u8, 0u8,
            // u_dummy_range: no bytes
        ];
        let mut s = make_state(&bytes, &strings);
        let e = read_expr(&mut s, "test ctx").unwrap();
        assert_eq!(
            e,
            PickledExpr::Array {
                elements: vec![
                    PickledExpr::Const {
                        value: PickledConst::Int32(1),
                        ty: PickledType::AppSimple {
                            simpletyp_index: 0,
                            nullness: Nullness::Ambivalent,
                        },
                    },
                    PickledExpr::Const {
                        value: PickledConst::Int32(2),
                        ty: PickledType::AppSimple {
                            simpletyp_index: 0,
                            nullness: Nullness::Ambivalent,
                        },
                    },
                ],
            },
        );
        assert!(s.reader.is_eof());
    }

    #[test]
    fn array_expr_of_typeof_apps() {
        // `[| typeof<int> |]`: tag 2, op 19, element type list, then one
        // `App(Val(typeof), [int], [])` element — the recursive case that
        // nests a tag-6 `App` inside the array.
        let strings: Vec<String> = vec![];
        let bytes = [
            2u8,  // u_expr tag = Op
            19u8, // u_op tag = Array
            // u_tys = [AppSimple idx 0]  (the `System.Type[]` element type)
            1u8, 1u8, 0u8,
            // u_Exprs = [ App(Val(Local 0), funcTy, [AppSimple 2], []) ]
            1u8, // list length 1
            // element 0: App
            6u8, // u_expr tag = App
            // func = Val(Local 0)
            1u8, 0u8, 0u8, 0u8, // Val tag, vref Local, stamp 0, vrefFlags 0
            // funcTy (decoded + dropped): AppSimple idx 0
            1u8, 0u8, // tyArgs = [AppSimple idx 2]
            1u8, 1u8, 2u8, // args = []
            0u8,
            // u_dummy_range (App): no bytes
            // u_dummy_range (Array): no bytes
        ];
        let mut s = make_state_nvals(&bytes, &strings, 1);
        let e = read_expr(&mut s, "test ctx").unwrap();
        assert_eq!(
            e,
            PickledExpr::Array {
                elements: vec![PickledExpr::App {
                    func: Box::new(PickledExpr::Val(PickledVRef::Local(0))),
                    ty_args: vec![PickledType::AppSimple {
                        simpletyp_index: 2,
                        nullness: Nullness::Ambivalent,
                    }],
                    args: vec![],
                }],
            },
        );
        assert!(s.reader.is_eof());
    }

    #[test]
    fn coerce_expr_wraps_single_operand() {
        // `Coerce(Const(String "x"))`: tag 2, u_op tag 15 (TOp.Coerce),
        // coercion target type list [AppSimple 0], then one element —
        // a `Const(String)` operand. Models a literal passed to an
        // `obj`-typed attribute parameter.
        let strings = vec!["x".to_string()];
        let bytes = [
            2u8,  // u_expr tag = Op
            15u8, // u_op tag = Coerce
            // u_tys = [AppSimple idx 0]  (the `obj` coercion target)
            1u8, 1u8, 0u8, // u_Exprs = [Const(String "x") : AppSimple 0]
            1u8, // list length 1
            0u8, 14u8, 0u8, 1u8,
            0u8, // Const tag, String tag, idx 0, u_ty AppSimple 0
                 // u_dummy_range (Coerce): no bytes
        ];
        let mut s = make_state(&bytes, &strings);
        let e = read_expr(&mut s, "test ctx").unwrap();
        assert_eq!(
            e,
            PickledExpr::Coerce {
                arg: Box::new(PickledExpr::Const {
                    value: PickledConst::String("x".to_string()),
                    ty: PickledType::AppSimple {
                        simpletyp_index: 0,
                        nullness: Nullness::Ambivalent,
                    },
                }),
            },
        );
        assert!(s.reader.is_eof());
    }

    #[test]
    fn coerce_expr_wrong_operand_count_errors() {
        // `TOp.Coerce` with two operands is a wire shape FCS never emits
        // (`CheckAttribArgExpr` matches `[arg]`); it must raise the loud
        // `UnsupportedPickleTag` rather than silently dropping an operand.
        let strings: Vec<String> = vec![];
        let bytes = [
            2u8, 15u8, // Op, u_op Coerce
            0u8,  // u_tys: length 0
            // u_Exprs: length 2, two Unit consts (tag 0, u_const Unit tag 15,
            // u_ty AppSimple 0)
            2u8, 0u8, 15u8, 1u8, 0u8, 0u8, 15u8, 1u8, 0u8,
        ];
        let mut s = make_state(&bytes, &strings);
        match read_expr(&mut s, "test") {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_op TOp.Coerce operand count (expected 1)",
                tag: 2,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn op_ilasm_decodes() {
        // tag 2 (Expr.Op) with u_op tag 12 (TOp.ILAsm). The inline-IL op
        // behind FSharp.Core's operators: one no-argument instruction (`or`,
        // opcode 13), empty asmTys/opTys/args. Collapses to `Other { tag: 2 }`.
        let strings: Vec<String> = vec![];
        let bytes = [
            2u8,  // u_expr tag = Op
            12u8, // u_op tag = ILAsm
            1u8,  // ILAsm instrs: list len 1
            13u8, // u_ILInstr opcode 13 = `or` (no argument)
            0u8,  // ILAsm asmTys: len 0
            0u8,  // Op opTys: len 0
            0u8,  // Op args (u_Exprs): len 0
                  // Op u_dummy_range: no bytes
        ];
        let mut s = make_state(&bytes, &strings);
        let e = read_expr(&mut s, "test").unwrap();
        assert_eq!(e, PickledExpr::Other { tag: 2 });
        assert!(s.reader.is_eof());
    }

    #[test]
    fn sequential_bad_direction_errors() {
        // Expr.Sequential with two Const operands and a special-sequence flag
        // of 2 — FCS rejects anything but 0/1, so we must too.
        let strings: Vec<String> = vec![];
        let minimal_const = [0u8, 0u8, 0u8, 1u8, 0u8]; // Const Bool(false) : AppSimple(0)
        let mut bytes = vec![3u8]; // u_expr tag = Sequential
        bytes.extend_from_slice(&minimal_const); // e1
        bytes.extend_from_slice(&minimal_const); // e2
        bytes.push(2u8); // dir = 2 (invalid)
        let mut s = make_state(&bytes, &strings);
        match read_expr(&mut s, "test") {
            Err(ImportError::UnsupportedPickleTag { context, tag: 2 }) => {
                assert!(context.contains("Sequential"), "context: {context}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn integer_for_loop_bad_direction_errors() {
        // Expr.Op(TOp.IntegerForLoop, …) with direction 3 — FCS accepts only
        // 0/1/2.
        let strings: Vec<String> = vec![];
        let bytes = [2u8, 21u8, 3u8]; // Op, u_op tag 21, direction 3 (invalid)
        let mut s = make_state(&bytes, &strings);
        match read_expr(&mut s, "test") {
            Err(ImportError::UnsupportedPickleTag { context, tag: 3 }) => {
                assert!(context.contains("IntegerForLoop"), "context: {context}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn op_genuinely_unsupported_tag_errors() {
        // A u_op tag that is still not ported (4 = TOp.ValFieldSet, which
        // needs `u_rfref`) must stay loud-on-unknown.
        let strings: Vec<String> = vec![];
        let bytes = [2u8, 4u8]; // Op, then u_op tag 4 (ValFieldSet)
        let mut s = make_state(&bytes, &strings);
        match read_expr(&mut s, "test") {
            Err(ImportError::UnsupportedPickleTag { context, tag: 4 }) => {
                assert!(context.starts_with("u_op tag"), "context: {context}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn expr_lambda_decodes_and_links_vals() {
        // tag 4 = Expr.Lambda. Its `u_Val`s publish into the val OSGN table,
        // so the arm must decode (not skip) them; the lambda collapses to
        // `Other { tag: 4 }`.
        let strings = vec!["p".to_string(), "".to_string()];
        // A minimal u_ValData body (mirrors val.rs::val_data_minimal_round_trip):
        // logical_name idx 0, no compiled_name, no ranges, ty AppSimple(0),
        // flags 0, no member_info, no attribs, no repr_info, xmldoc_sig idx 1,
        // public access, ParentNone, no literal, no xmldoc.
        let val_body = [
            0u8, // logical_name idx 0 = "p"
            0u8, // compiled_name None
            0u8, // ranges None
            1u8, 0u8, // ty AppSimple(0)
            0u8, 0u8, // flags 0
            0u8, // member_info None
            0u8, // attribs len 0
            0u8, // repr_info None
            1u8, // xmldoc_sig idx 1 = ""
            0u8, // access None
            0u8, // parent ParentNone
            0u8, // literal None
            0u8, // used_space1 None
        ];
        let mut bytes = vec![
            4u8, // u_expr tag = Lambda
            0u8, // baseVal option None
            0u8, // ctorThisVal option None
            1u8, // vals: list len 1
            0u8, // u_Val osgn index 0
        ];
        bytes.extend_from_slice(&val_body); // the val at stamp 0
        bytes.extend_from_slice(&[
            0u8, // body: u_expr — Const
            // (Const body:) u_const Bool(false) tag 0 body 0, then u_ty AppSimple(0)
            0u8, 0u8, // u_const Bool(false)
            1u8, 0u8, // u_ty AppSimple(0)
            // Lambda u_dummy_range: no bytes
            1u8, 0u8, // result ty: u_ty AppSimple(0)
        ]);
        let mut s = make_state_nvals(&bytes, &strings, 1);
        let e = read_expr(&mut s, "test").unwrap();
        assert_eq!(e, PickledExpr::Other { tag: 4 });
        assert!(s.ivals.get(0).is_ok(), "lambda val stamp 0 must be linked");
        assert!(s.reader.is_eof());
    }
}
