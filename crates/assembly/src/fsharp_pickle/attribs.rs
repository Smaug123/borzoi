//! `u_attribs` and friends — pickled F# attribute applications.
//!
//! ### FCS source map
//!
//! - `u_attribs`     — `TypedTreePickle.fs:3964`: `u_list u_attrib`.
//! - `u_attrib`      — `:3232-3236`: `u_tup6 u_tcref u_attribkind
//!   (u_list u_attrib_expr) (u_list u_attrib_arg) u_bool u_dummy_range`.
//! - `u_attribkind`  — `:3224-3230`: tag byte; 0 → `ILAttrib(u_ILMethodRef)`,
//!   1 → `FSAttrib(u_vref)`.
//! - `u_attrib_expr` — `:3238-3240`: `u_tup2 u_expr u_expr` (orig +
//!   constant-evaluated forms).
//! - `u_attrib_arg`  — `:3242-3244`: `u_tup4 u_string u_ty u_bool
//!   u_attrib_expr`.
//!
//! The two `u_expr` reads inside `u_attrib_expr` each route through
//! [`read_expr`], which walks the full expression tree FCS pickles for an
//! attribute argument — keeping a structured value for the literal shapes
//! (`Const`/`Val`/`App`/`Array`/`Coerce`) and decoding the rest for
//! alignment; the genuinely-unported shapes hard-error per D6.5 with a
//! context string naming the consuming arm.

use crate::error::ImportError;
use crate::fsharp_pickle::access::read_dummy_range;
use crate::fsharp_pickle::expr::read_expr;
use crate::fsharp_pickle::il::read_il_method_ref;
use crate::fsharp_pickle::model::{
    PickledAttribExpr, PickledAttribKind, PickledAttribNamedArg, PickledAttribute,
};
use crate::fsharp_pickle::osgn::PhaseOneState;
use crate::fsharp_pickle::types::{read_tcref, read_ty};
use crate::fsharp_pickle::vrefs::read_vref;

/// `u_attrib_expr` (`TypedTreePickle.fs:3238-3240`). Two `u_expr`
/// reads back-to-back: the original expression as it appeared in
/// source and the constant-evaluated form. FCS pickles both because
/// pre-evaluation can lose information (e.g. `Expr.Val literalRef`
/// → `Expr.Const literalValue`); the signature pickler normalises
/// `orig` to `Const` for literal-`Val` references at
/// `p_attrib_expr:2878-2889`, so literal arguments arrive as `Const`,
/// while a `typeof<T>` argument arrives as `App(Val(typeof), …)`.
pub(crate) fn read_attrib_expr(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledAttribExpr, ImportError> {
    let orig = read_expr(state, "u_attrib_expr orig")?;
    let evaluated = read_expr(state, "u_attrib_expr evaluated")?;
    Ok(PickledAttribExpr { orig, evaluated })
}

/// `u_attrib_arg` (`:3242-3244`). 4-tuple: argument name, declared
/// type, `isField` discriminator, and the value as an `AttribExpr`
/// pair.
pub(crate) fn read_attrib_arg(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledAttribNamedArg, ImportError> {
    let name = state.reader.read_string("u_attrib_arg name")?;
    let ty = read_ty(state)?;
    let is_field = state.reader.read_bool("u_attrib_arg isField")?;
    let value = read_attrib_expr(state)?;
    Ok(PickledAttribNamedArg {
        name,
        ty,
        is_field,
        value,
    })
}

/// `u_attribkind` (`:3224-3230`). Tag 0 → `ILAttrib` carrying a CLR
/// method ref; tag 1 → `FSAttrib` carrying an F# `vref`.
pub(crate) fn read_attribkind(
    state: &mut PhaseOneState<'_>,
) -> Result<PickledAttribKind, ImportError> {
    let tag = state.reader.read_byte("u_attribkind tag")?;
    match tag {
        0 => Ok(PickledAttribKind::ILAttrib(Box::new(read_il_method_ref(
            &mut state.reader,
        )?))),
        1 => Ok(PickledAttribKind::FSAttrib(read_vref(state)?)),
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_attribkind tag",
            tag: u32::from(other),
        }),
    }
}

/// `u_attrib` (`:3232-3236`). Reads six wire fields:
/// 1. `u_tcref` — the attribute class's entity ref.
/// 2. `u_attribkind` — IL vs F# discriminator.
/// 3. `u_list u_attrib_expr` — positional arguments.
/// 4. `u_list u_attrib_arg` — named arguments.
/// 5. `u_bool` — `appliedToGetterOrSetter`.
/// 6. `u_dummy_range` — zero bytes (FCS records `range0`).
///
/// FCS's 7-tuple `Attrib(a, b, c, d, e, None, f)` slot in position 6
/// is `AttributeTargets`, which is intentionally not preserved by
/// the pickler (`:3236`); we drop it.
pub(crate) fn read_attrib(state: &mut PhaseOneState<'_>) -> Result<PickledAttribute, ImportError> {
    let tcref = read_tcref(state)?;
    let kind = read_attribkind(state)?;
    let args_unnamed = state.read_array("u_attrib args_unnamed element", read_attrib_expr)?;
    let args_named = state.read_array("u_attrib args_named element", read_attrib_arg)?;
    let applied_to_getter_or_setter = state.reader.read_bool("u_attrib appliedToGetterOrSetter")?;
    read_dummy_range(&mut state.reader)?;
    Ok(PickledAttribute {
        tcref,
        kind,
        args_unnamed,
        args_named,
        applied_to_getter_or_setter,
    })
}

/// `u_attribs` (`TypedTreePickle.fs:3964`). `u_list u_attrib`.
pub(crate) fn read_attribs(
    state: &mut PhaseOneState<'_>,
) -> Result<Vec<PickledAttribute>, ImportError> {
    state.read_array("u_attribs element", read_attrib)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsharp_pickle::model::{
        Nullness, PickledConst, PickledExpr, PickledILType, PickledTcRef, PickledType, PickledVRef,
    };
    use crate::fsharp_pickle::reader::PickleReader;

    fn make_state<'a>(bytes: &'a [u8], strings: &'a [String]) -> PhaseOneState<'a> {
        let mut r = PickleReader::new(bytes);
        let pubpaths: &'a [Vec<u32>] = &[];
        r.attach_tables(strings, pubpaths);
        PhaseOneState::with_capacities(r, 0, 0, 0)
    }

    /// Build the wire bytes for a minimal `u_attrib_expr`: tag 0
    /// (`Const`) + `u_const Bool(true)` + `u_dummy_range` (no bytes)
    /// + `u_ty AppSimple(simpletyp_idx=0, ambivalent)`.
    fn enc_const_expr_bool_true_app_simple_zero() -> Vec<u8> {
        vec![
            0u8, // u_expr tag = Const
            0u8, 1u8, // u_const Bool(true): tag 0, body 1
            1u8, // u_ty tag = AppSimple
            0u8, // simpletyp idx 0
        ]
    }

    #[test]
    fn attribkind_il_attrib_round_trip() {
        // tag 0 + minimal u_ILMethodRef:
        //   parent: scope=Local, enclosing=[], name="A"
        //   call_conv: Instance+Default
        //   generic_arity=0, name=".ctor", args=[], ret=Void.
        let strings = vec!["A".to_string(), ".ctor".to_string()];
        let mut bytes = vec![
            0u8, // u_attribkind tag = ILAttrib
        ];
        // parent ILTypeRef
        bytes.push(0u8); // scope Local
        bytes.push(0u8); // enclosing list len 0
        bytes.push(0u8); // name idx 0 = "A"
        // call_conv (Instance + Default)
        bytes.push(0u8);
        bytes.push(0u8);
        // generic_arity = 0
        bytes.push(0u8);
        // name = ".ctor" idx 1
        bytes.push(1u8);
        // arg_types: empty
        bytes.push(0u8);
        // return_type: Void
        bytes.push(0u8);
        let mut s = make_state(&bytes, &strings);
        let k = read_attribkind(&mut s).unwrap();
        match k {
            PickledAttribKind::ILAttrib(mref) => {
                assert_eq!(mref.parent.name, "A");
                assert_eq!(mref.name, ".ctor");
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(s.reader.is_eof());
    }

    #[test]
    fn attribkind_fs_attrib_round_trip() {
        // tag 1 + u_vref tag 1 + minimal u_nonlocal_val_ref:
        //   tcref = NonLocal(0), no MemberParentMangledName,
        //   is_override = false, logical_name idx 0 = "foo",
        //   total_arg_count = 0, partial_type = None.
        let strings = vec!["foo".to_string()];
        let bytes = vec![
            1u8, // u_attribkind tag = FSAttrib
            1u8, // u_vref tag = NonLocal
            1u8, 0u8, // u_tcref tag = NonLocal, nleref idx 0
            0u8, // option None (mangled-parent-name)
            0u8, // bool false (is_override)
            0u8, // string idx 0 = "foo"
            0u8, // total_arg_count = 0
            0u8, // option None (partial_type)
        ];
        let mut s = make_state(&bytes, &strings);
        let k = read_attribkind(&mut s).unwrap();
        match k {
            PickledAttribKind::FSAttrib(PickledVRef::NonLocal(nl)) => {
                assert_eq!(nl.logical_name, "foo");
                assert_eq!(nl.enclosing_entity, PickledTcRef::NonLocal(0));
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(s.reader.is_eof());
    }

    #[test]
    fn attribkind_unknown_tag_errors() {
        let strings: Vec<String> = vec![];
        let bytes = vec![5u8];
        let mut s = make_state(&bytes, &strings);
        match read_attribkind(&mut s) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_attribkind tag",
                tag: 5,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn attrib_expr_pair_round_trips() {
        // Two back-to-back const exprs.
        let strings: Vec<String> = vec![];
        let mut bytes = enc_const_expr_bool_true_app_simple_zero();
        bytes.extend(enc_const_expr_bool_true_app_simple_zero());
        let mut s = make_state(&bytes, &strings);
        let e = read_attrib_expr(&mut s).unwrap();
        assert_eq!(
            e.orig,
            PickledExpr::Const {
                value: PickledConst::Bool(true),
                ty: PickledType::AppSimple {
                    simpletyp_index: 0,
                    nullness: Nullness::Ambivalent,
                },
            }
        );
        assert_eq!(e.evaluated, e.orig);
        assert!(s.reader.is_eof());
    }

    #[test]
    fn attrib_arg_round_trip() {
        // name = "Message", ty = AppSimple(0), is_field = false,
        // value.orig/evaluated = Const(String("hello"), AppSimple(0)).
        let strings = vec!["Message".to_string(), "hello".to_string()];
        let mut bytes = vec![
            0u8, // name idx 0 = "Message"
            1u8, // u_ty tag = AppSimple
            0u8, // simpletyp idx 0
            0u8, // is_field = false
        ];
        // value: AttribExpr = (orig, evaluated) — each is u_expr Const
        // String "hello" : AppSimple(0).
        for _ in 0..2 {
            bytes.push(0u8); // u_expr tag = Const
            bytes.push(14u8); // u_const String tag
            bytes.push(1u8); // string idx 1 = "hello"
            bytes.push(1u8); // u_ty tag = AppSimple
            bytes.push(0u8); // simpletyp idx 0
        }
        let mut s = make_state(&bytes, &strings);
        let a = read_attrib_arg(&mut s).unwrap();
        assert_eq!(a.name, "Message");
        assert!(!a.is_field);
        match a.value.orig {
            PickledExpr::Const {
                value: PickledConst::String(s),
                ..
            } => assert_eq!(s, "hello"),
            other => panic!("unexpected: {other:?}"),
        }
        assert!(s.reader.is_eof());
    }

    #[test]
    fn attrib_end_to_end_il_attrib_no_args() {
        // Build a parameterless IL attribute application:
        //   [<MyAttr>] applied to a non-getter/setter site, ctor =
        //   MyAttr..ctor() with no parameters and a `void` return.
        let strings = vec!["MyAttr".to_string(), ".ctor".to_string()];
        let bytes = vec![
            1u8, 0u8, // u_tcref NonLocal nleref idx 0
            0u8, // u_attribkind ILAttrib (tag 0) — payload is u_ILMethodRef
            // u_ILMethodRef parent: u_ILTypeRef
            0u8, // scope: ILScopeRef::Local (tag 0)
            0u8, // enclosing namespace u_strings list len 0
            0u8, // type name idx 0 = "MyAttr"
            // u_ILCallConv
            0u8, // has_this: Instance (tag 0)
            0u8, // basic: Default (tag 0)
            0u8, // generic_arity = 0
            1u8, // method name idx 1 = ".ctor"
            0u8, // arg_types u_ILTypes list len 0
            0u8, // return type: u_ILType tag 0 = Void
            // u_attrib trailing fields
            0u8, // args_unnamed list len 0
            0u8, // args_named list len 0
            0u8, // applied_to_getter_or_setter = false
                 // u_dummy_range reads 0 bytes
        ];
        let mut s = make_state(&bytes, &strings);
        let a = read_attrib(&mut s).unwrap();
        assert_eq!(a.tcref, PickledTcRef::NonLocal(0));
        assert!(a.args_unnamed.is_empty());
        assert!(a.args_named.is_empty());
        assert!(!a.applied_to_getter_or_setter);
        match a.kind {
            PickledAttribKind::ILAttrib(mref) => {
                assert_eq!(mref.parent.name, "MyAttr");
                assert_eq!(mref.name, ".ctor");
                assert!(mref.arg_types.is_empty());
                assert_eq!(mref.return_type, PickledILType::Void);
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(s.reader.is_eof());
    }

    #[test]
    fn attribs_empty_list() {
        let strings: Vec<String> = vec![];
        let bytes = vec![0u8];
        let mut s = make_state(&bytes, &strings);
        assert!(read_attribs(&mut s).unwrap().is_empty());
    }

    #[test]
    fn attribs_one_element() {
        // u_list of one u_attrib (matching the no-args case above).
        let strings = vec!["MyAttr".to_string(), ".ctor".to_string()];
        let mut bytes = vec![1u8]; // list length 1
        // tcref NonLocal idx 0
        bytes.push(1u8);
        bytes.push(0u8);
        // attribkind ILAttrib
        bytes.push(0u8);
        // ILMethodRef minimal
        bytes.push(0u8); // scope Local
        bytes.push(0u8); // enclosing len 0
        bytes.push(0u8); // parent name idx 0
        bytes.push(0u8); // has_this Instance
        bytes.push(0u8); // basic Default
        bytes.push(0u8); // generic_arity 0
        bytes.push(1u8); // name idx 1
        bytes.push(0u8); // arg_types empty
        bytes.push(0u8); // return Void
        bytes.push(0u8); // args_unnamed len 0
        bytes.push(0u8); // args_named len 0
        bytes.push(0u8); // applied_to_getter_or_setter false
        let mut s = make_state(&bytes, &strings);
        let v = read_attribs(&mut s).unwrap();
        assert_eq!(v.len(), 1);
        assert!(s.reader.is_eof());
    }
}
