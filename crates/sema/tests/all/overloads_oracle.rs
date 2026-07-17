//! Stage OV-1 — the `overloads` oracle mode + the §3 probe regression corpus of
//! [`docs/overload-resolution-plan.md`](../../../docs/overload-resolution-plan.md).
//!
//! These tests pin **FCS's** overload-resolution semantics (the oracle's
//! behaviour), *before* any engine exists on our side — so every later OV stage
//! has a fixed target. Each `probe_*` test runs `fcs-dump overloads` on a snippet
//! reproducing one row of the §3 catalogue and asserts the overload FCS chose
//! (identified by its canonical parameter signature / `XmlDocSig` / return type).
//!
//! Following §3.1: assertions compare by **signature**, never gate on `Kind`
//! (`isOverloadedMember` undercounts), and tolerate a **missing** call node
//! (out-arg/tuple-return folding can erase it — probe P12).

use crate::common::{FcsCall, invoke_fcs_dump, parse_fcs_overloads, temp_fs_file};

/// Type-check `src` as a script and return the call nodes FCS elaborated.
fn overloads(label: &str, src: &str) -> Vec<FcsCall> {
    let path = temp_fs_file(label, src);
    let json = invoke_fcs_dump("overloads", &path);
    let _ = std::fs::remove_file(&path);
    parse_fcs_overloads(&json, src)
}

/// The call nodes named `name` (a call site can appear once; a ctor twice —
/// `System.Object.#ctor` plus the type's own).
fn calls_named<'a>(calls: &'a [FcsCall], name: &str) -> Vec<&'a FcsCall> {
    calls.iter().filter(|c| c.name == name).collect()
}

/// The single call node named `name`, panicking if absent or ambiguous.
fn one_call<'a>(calls: &'a [FcsCall], name: &str) -> &'a FcsCall {
    let matches = calls_named(calls, name);
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one call named {name:?}, got {}: {:#?}",
        matches.len(),
        calls
    );
    matches[0]
}

// ── Population filter: only genuine invocation sites are emitted ─────────────
//
// FCS reifies plain property reads (`s.Length` ⇒ `get_Length`) and module-value
// reads (`let y = x`) as `Call` nodes, but they are not overload sites, so the
// oracle drops them — while keeping an INDEXER accessor (`h.[i]` ⇒ `get_Item`),
// which carries index parameters and can be overloaded. Regression for the two
// OV-1/OV-2-review fixes.
#[test]
fn plain_getters_and_value_reads_are_dropped_indexers_kept() {
    let calls = overloads(
        "ov_filter",
        "\
type H() =
    member _.Item with get (i: int) = \"int\"
    member _.Item with get (s: string) = 1
let s = \"hi\"
let n = s.Length
let y = s
let h = H()
let a = h.[1]
let b = h.[\"x\"]
",
    );
    // Plain property read `s.Length` (get_Length) and value reads are gone.
    assert!(
        calls_named(&calls, "get_Length").is_empty(),
        "a plain property read is not an overload site: {calls:#?}"
    );
    assert!(
        calls.iter().all(|c| c.kind != "value:module"),
        "module-value reads are not calls: {calls:#?}"
    );
    // The overloaded indexer's two accesses survive (get_Item with index params).
    let items = calls_named(&calls, "get_Item");
    assert_eq!(items.len(), 2, "both indexer accesses are kept: {calls:#?}");
    assert!(
        items
            .iter()
            .any(|c| c.flat_params() == vec!["System.Int32"])
            && items
                .iter()
                .any(|c| c.flat_params() == vec!["System.String"]),
        "each indexer overload keeps its index parameter type: {items:#?}"
    );
}

// ── P1 — built-in widening (int32 → float) picks M(float) ───────────────────
#[test]
fn probe_p1_widening_picks_float_overload() {
    let calls = overloads(
        "ov_p1",
        "\
type C() =
    member _.M(x: float) = 1
    member _.M(x: string) = \"s\"
let c = C()
let r = c.M(3)
",
    );
    let m = one_call(&calls, "M");
    assert_eq!(
        m.flat_params(),
        vec!["System.Double"],
        "int 3 widens to float; FCS picks M(float), not M(string): {m:#?}"
    );
    assert!(m.xml_doc_sig.contains("M(System.Double)"), "{m:#?}");
}

// ── P2 — a params array is applicable at any trailing arity ──────────────────
#[test]
fn probe_p2_params_array_applicable_at_multiple_arities() {
    let src = "\
type C() =
    member _.V([<System.ParamArray>] xs: int[]) = 1
    member _.V(s: string) = \"s\"
let c = C()
let a = c.V(1, 2)
let b = c.V(7)
let d = c.V(\"x\")
";
    let calls = overloads("ov_p2", src);
    // Two source calls V(1,2) and V(7) resolve to the params form (int[]);
    // V("x") resolves to the string form. All three are named "V".
    let vs = calls_named(&calls, "V");
    assert_eq!(vs.len(), 3, "three V calls: {calls:#?}");
    let params_form: Vec<_> = vs
        .iter()
        .filter(|c| c.flat_params() == vec!["System.Int32[]"])
        .collect();
    let string_form: Vec<_> = vs
        .iter()
        .filter(|c| c.flat_params() == vec!["System.String"])
        .collect();
    assert_eq!(
        params_form.len(),
        2,
        "V(1,2) and V(7) take the params form: {vs:#?}"
    );
    assert_eq!(
        string_form.len(),
        1,
        "V(\"x\") takes the string form: {vs:#?}"
    );
}

// ── P3 — a trailing optional makes M(int,int=0) applicable at arity 1 ────────
#[test]
fn probe_p3_optional_makes_lower_arity_applicable() {
    let calls = overloads(
        "ov_p3",
        "\
open System.Runtime.InteropServices
type C() =
    member _.M(a: int, [<Optional; DefaultParameterValue(0)>] b: int) = 1.0
    member _.M(s: string) = \"s\"
let c = C()
let r = c.M(1)
",
    );
    let m = one_call(&calls, "M");
    assert_eq!(
        m.flat_params(),
        vec!["System.Int32", "System.Int32"],
        "M(1) resolves to the optional (int,int) overload, not M(string): {m:#?}"
    );
}

// ── P4 — a non-overloaded instance call vs an overloaded one ─────────────────
#[test]
fn probe_p4_tostring_vs_substring() {
    let calls = overloads(
        "ov_p4",
        "\
let s = \"hi\"
let a = s.ToString()
let b = s.Substring(1)
",
    );
    let ts = one_call(&calls, "ToString");
    assert_eq!(ts.ret, "System.String", "{ts:#?}");
    let sub = one_call(&calls, "Substring");
    assert_eq!(sub.ret, "System.String", "{sub:#?}");
    assert_eq!(
        sub.flat_params(),
        vec!["System.Int32"],
        "Substring(1) picks the single-int overload: {sub:#?}"
    );
}

// ── P5 — most-specific: M("hi")→string, M(3)→obj (int boxes to obj) ──────────
#[test]
fn probe_p5_obj_vs_string_most_specific() {
    let src = "\
type C() =
    member _.M(x: obj) = 1
    member _.M(x: string) = \"s\"
let c = C()
let a = c.M(\"hi\")
let b = c.M(3)
";
    let calls = overloads("ov_p5", src);
    let ms = calls_named(&calls, "M");
    assert_eq!(ms.len(), 2, "{calls:#?}");
    // Order is document order: M("hi") first, M(3) second.
    let by_start = {
        let mut v = ms.clone();
        v.sort_by_key(|c| c.start);
        v
    };
    assert_eq!(
        by_start[0].flat_params(),
        vec!["System.String"],
        "M(\"hi\") picks the more-specific string overload: {by_start:#?}"
    );
    assert_eq!(
        by_start[1].flat_params(),
        vec!["System.Object"],
        "M(3) picks obj (int boxes; no string channel): {by_start:#?}"
    );
}

// ── P6 — interface receivers: inherited-interface + Object members type ──────
#[test]
fn probe_p6_interface_receiver_members() {
    let calls = overloads(
        "ov_p6",
        "\
open System
open System.Collections.Generic
let f (e: IEnumerable<int>) = e.GetEnumerator()
let g (d: IDisposable) = d.GetHashCode()
",
    );
    let ge = one_call(&calls, "GetEnumerator");
    assert!(
        ge.ret.contains("Enumerator") || ge.ret.contains("IEnumerator"),
        "GetEnumerator on IEnumerable<int> types: {ge:#?}"
    );
    let gh = one_call(&calls, "GetHashCode");
    assert_eq!(
        gh.ret, "System.Int32",
        "Object.GetHashCode on an interface receiver: {gh:#?}"
    );
}

// ── P7 — two equally-good widenings ⇒ ambiguity ⇒ error, no committed pick ───
#[test]
fn probe_p7_ambiguous_widening_is_an_error() {
    let src = "\
type C() =
    member _.M(x: int64) = 1
    member _.M(x: float) = 2.0
let c = C()
let r = c.M(3)
";
    let calls = overloads("ov_p7", src);
    // FCS errors (both int64 and float widen from int32); the recovery node is
    // `call:function : Object`, NOT either concrete overload. So either the M
    // node is absent or it did not commit to int64/float.
    for m in calls_named(&calls, "M") {
        assert_ne!(
            m.flat_params(),
            vec!["System.Int64"],
            "an ambiguous call must not commit to the int64 overload: {m:#?}"
        );
        assert_ne!(
            m.flat_params(),
            vec!["System.Double"],
            "an ambiguous call must not commit to the float overload: {m:#?}"
        );
    }
}

// ── P8 — statics share the overload machinery ────────────────────────────────
#[test]
fn probe_p8_static_overloads() {
    let calls = overloads(
        "ov_p8",
        "\
let a = System.Math.Abs(3)
let b = System.String.Compare(\"a\", \"b\")
",
    );
    let abs = one_call(&calls, "Abs");
    assert_eq!(
        abs.ret, "System.Int32",
        "Math.Abs(3) picks the int overload: {abs:#?}"
    );
    let cmp = one_call(&calls, "Compare");
    assert_eq!(cmp.ret, "System.Int32", "{cmp:#?}");
    assert_eq!(
        cmp.flat_params(),
        vec!["System.String", "System.String"],
        "{cmp:#?}"
    );
}

// ── P9 — an in-scope extension member types the call ─────────────────────────
#[test]
fn probe_p9_extension_member_types() {
    let calls = overloads(
        "ov_p9",
        "\
module Ext =
    type System.String with
        member _.Twice() = 3.0
open Ext
let r = \"x\".Twice()
",
    );
    let tw = one_call(&calls, "Twice");
    assert_eq!(
        tw.ret, "System.Double",
        "the extension Twice() types: {tw:#?}"
    );
    assert_eq!(tw.kind, "call:extension", "{tw:#?}");
}

// ── P10 — op_Implicit reaches a decimal parameter ────────────────────────────
#[test]
fn probe_p10_op_implicit_to_decimal() {
    // M(int64)/M(string): int widens to int64.
    let widen = overloads(
        "ov_p10a",
        "\
type C() =
    member _.M(x: int64) = 1
    member _.M(x: string) = \"s\"
let c = C()
let r = c.M(3)
",
    );
    assert_eq!(
        one_call(&widen, "M").flat_params(),
        vec!["System.Int64"],
        "int widens to int64: {widen:#?}"
    );
    // M(decimal)/M(string): int reaches decimal via op_Implicit.
    let implicit = overloads(
        "ov_p10b",
        "\
type C() =
    member _.M(x: decimal) = 1
    member _.M(x: string) = \"s\"
let c = C()
let r = c.M(3)
",
    );
    assert_eq!(
        one_call(&implicit, "M").flat_params(),
        vec!["System.Decimal"],
        "int reaches decimal via op_Implicit: {implicit:#?}"
    );
}

// ── P11 — a generic candidate competes fully ─────────────────────────────────
#[test]
fn probe_p11_generic_candidate_competes() {
    let src = "\
type C() =
    member _.M(x: obj) = 1
    member _.M(x: 'T list) = 2.0
let c = C()
let a = c.M(\"hi\")
let b = c.M([1])
";
    let calls = overloads("ov_p11", src);
    let ms = {
        let mut v = calls_named(&calls, "M");
        v.sort_by_key(|c| c.start);
        v
    };
    assert_eq!(ms.len(), 2, "{calls:#?}");
    assert_eq!(
        ms[0].flat_params(),
        vec!["System.Object"],
        "M(\"hi\") picks obj: {ms:#?}"
    );
    // The list overload is generic, so its `Params` rendering is best-effort
    // (unbound typars fall back to FCS display text — see the `parse_fcs_overloads`
    // doc; canonical generic rendering is the deferred §7 "Ty generic args" work).
    // Key on `XmlDocSig`, which IS canonical/stable (`` `0 `` for the typar,
    // `FSharpList{…}`) regardless of the source typar name — the §3.1 comparison
    // currency.
    assert!(
        ms[1].xml_doc_sig.contains("FSharpList"),
        "M([1]) picks the generic list overload (by XmlDocSig): {ms:#?}"
    );
}

// ── P12 — an out-arg call surfaces WITH a byref parameter (the defer signal) ──
//
// The plan's §3 P12 row (probed 2026-07-06) said `Int32.TryParse "3"` leaves
// **no** `Call` node. Re-probing on the net10 / F# 10 toolchain (OV-1,
// 2026-07-08) shows this is **stale**: the tuple-sugar form
// `let ok, v = Int32.TryParse "3"`, the explicit `TryParse("3", &v)` form, and
// the `|> ignore` form all elaborate to a `call:static-overloaded` node whose
// signature carries a `System.Int32&` (byref/out) parameter. That byref is the
// engine's §5 defer trigger, so the useful pin is its presence — not absence.
// (The plan's "tolerate a missing node" guidance still holds as a general
// robustness rule; this row just no longer exercises it on this toolchain.)
#[test]
fn probe_p12_out_arg_call_carries_byref_param() {
    let calls = overloads(
        "ov_p12",
        "\
let ok, v = System.Int32.TryParse \"3\"
let s = \"hi\"
let b = s.StartsWith \"h\"
",
    );
    let tp = one_call(&calls, "TryParse");
    assert!(
        tp.flat_params().iter().any(|p| p.ends_with('&')),
        "TryParse's signature carries a byref out-param (the §5 defer trigger): {tp:#?}"
    );
    assert_eq!(tp.ret, "System.Boolean", "{tp:#?}");
    // StartsWith is a normal instance call ⇒ Boolean.
    let sw = one_call(&calls, "StartsWith");
    assert_eq!(sw.ret, "System.Boolean", "{sw:#?}");
}

// ── P13 — Object-inherited instance calls type ───────────────────────────────
#[test]
fn probe_p13_object_methods() {
    let calls = overloads(
        "ov_p13",
        "\
let s = \"hi\"
let a = s.ToString()
let b = s.GetHashCode()
",
    );
    assert_eq!(one_call(&calls, "ToString").ret, "System.String");
    assert_eq!(one_call(&calls, "GetHashCode").ret, "System.Int32");
}

// ── P14 — an overloaded ToString(IFormatProvider) call types ─────────────────
#[test]
fn probe_p14_tostring_with_format_provider() {
    let calls = overloads(
        "ov_p14",
        "\
open System.Globalization
let s = 3
let r = s.ToString(CultureInfo.InvariantCulture)
",
    );
    let ts = one_call(&calls, "ToString");
    assert_eq!(ts.ret, "System.String", "{ts:#?}");
    assert_eq!(ts.flat_params(), vec!["System.IFormatProvider"], "{ts:#?}");
}

// ── P15 — an applicable extension beats a less-specific intrinsic ────────────
#[test]
fn probe_p15_extension_beats_less_specific_intrinsic() {
    let calls = overloads(
        "ov_p15",
        "\
type C() =
    member _.M(x: obj) = 1
module Ext =
    type C with
        member _.M(x: string) = 2.0
open Ext
let c = C()
let r = c.M(\"hi\")
",
    );
    let m = one_call(&calls, "M");
    assert_eq!(
        m.kind, "call:extension",
        "the more-specific extension wins: {m:#?}"
    );
    assert_eq!(m.ret, "System.Double", "{m:#?}");
}

// ── P16 — named arguments resolve ────────────────────────────────────────────
#[test]
fn probe_p16_named_args_resolve() {
    let calls = overloads(
        "ov_p16",
        "\
type C() =
    member _.M(a: int, b: string) = 1.0
let c = C()
let r = c.M(a = 1, b = \"x\")
",
    );
    let m = one_call(&calls, "M");
    assert_eq!(m.ret, "System.Double", "named-arg call resolves: {m:#?}");
}

// ── P17 — an equally-specific intrinsic beats an extension (rule 10) ─────────
#[test]
fn probe_p17_intrinsic_beats_equal_extension() {
    let calls = overloads(
        "ov_p17",
        "\
type C() =
    member _.M(x: string) = 1
module Ext =
    type C with
        member _.M(x: string) = 2.0
open Ext
let c = C()
let r = c.M(\"hi\")
",
    );
    let m = one_call(&calls, "M");
    assert_ne!(
        m.kind, "call:extension",
        "the intrinsic wins on an equal match: {m:#?}"
    );
    assert_eq!(m.ret, "System.Int32", "{m:#?}");
}
