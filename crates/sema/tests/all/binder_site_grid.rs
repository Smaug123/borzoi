//! Binder completeness over every binding construct of F#.
//!
//! [`CONSTRUCTS`] is a closed table: each row is one place F# introduces a name
//! — a position in FCS's `SynPat` / `SynSimplePats` / `SynExpr` /
//! `SynMemberDefn` trees, or a declaration form — written as a small program
//! that binds the name and uses it in its scope, after a module-level decoy of
//! the same name that a walk which missed the binder would commit to.
//!
//! Every row is checked against FCS, and every row's expectation is pinned:
//!
//! - **FCS accepts the cell**, so its answers are not error recovery;
//! - **FCS reports the binder**: a definition record at the marked range, or
//!   the row says FCS is [silent](Status::FcsSilent) there. This is the
//!   absolute half: a construct the oracle does not report is a construct
//!   binder completeness cannot see, and the row is where that is written down;
//! - **the planted uses are in the binder's scope**: FCS binds each to it;
//! - **our walk knows the binder** exactly as the row's [`Status`] says —
//!   interned or declined ([opaque](Status::Opaque)) — and a row whose status
//!   moves, in either direction, fails until the table says so;
//! - **no commit behind it**: at a planted use we commit to the binder or
//!   decline, never to the decoy;
//! - **the cell is otherwise complete**: every other binder FCS reports in it
//!   is one the walk knows, so a row cannot hide a second construct.
//!
//! A row is FCS-graded, so its expectation cannot be backwards: what the table
//! asserts about the oracle (silent or not, in scope or not) is checked against
//! the oracle, and what it asserts about us is checked against us.

use std::collections::BTreeMap;
use std::path::PathBuf;

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile, SyntaxNode};
use borzoi_sema::test_support::binder_completeness::{
    BinderVerdict, OracleUse, UNSCOPED_BINDER_LEAVES, binder_site, binder_verdict, is_unscoped,
    unmodelled_binders,
};
use borzoi_sema::{
    AssemblyEnv, ProjectItems, Resolution, ResolvedFile, SyntaxRecovery, resolve_file,
    resolve_project_files,
};
use rowan::TextRange;

use crate::common::{
    census_oracle_binders, census_oracle_uses, full_bcl_env, invoke_fcs_dump_census,
    invoke_fcs_dump_project_with_refs, parse_census_jsonl, parse_fcs_uses_project, temp_fs_file,
    temp_fs_tree, unmodelled_project_binder_failures,
};
use crate::resolve_signatures::project;

/// What the walk is pinned to know about a row's binder. There is no
/// "unmodelled" status: a construct the walk does not know about fails the
/// grid until it is interned or declined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// A def of ours spans the binder.
    Interned,
    /// The walk records the binder and declines its uses.
    Opaque,
    /// FCS reports no definition record for the binder, so binder completeness
    /// cannot see this construct. The reason names what the oracle does
    /// instead.
    FcsSilent(&'static str),
    /// FCS reports a definition, but no bare lookup reaches the name: the
    /// row's planted uses are bound elsewhere, which the grid checks, and the
    /// binder's leaf is on
    /// [`UNSCOPED_BINDER_LEAVES`](borzoi_sema::test_support::binder_completeness::UNSCOPED_BINDER_LEAVES).
    NotInScope,
}

/// One binding construct.
struct Construct {
    id: &'static str,
    /// Where FCS's syntax tree holds the binder.
    fcs: &'static str,
    /// The program after the prelude. `«n»` marks the binder, `‹n›` a use FCS
    /// must bind to it, and `USE` a use in expression position, which the
    /// grid writes both bare (`‹n›`) and as the head of a dotted access
    /// (`‹n›.GetHashCode()`): the two read different scope channels.
    template: &'static str,
    status: Status,
}

/// The decoys every cell begins with: a module-level value of each name a row
/// binds, which any walk that missed the row's binder would find instead.
const PRELUDE: &str = "module M\nlet x = \"outer\"\nlet X = \"outer\"\n";

const CONSTRUCTS: &[Construct] = &[
    // ---- `let` heads and function parameters (SynBinding headPat) ----
    Construct {
        id: "let_module_value",
        fcs: "SynBinding / SynPat.Named",
        template: "module Inner =\n    let «x» = 1\n    do ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "let_mutable",
        fcs: "SynBinding(isMutable) / SynPat.Named",
        template: "module Inner =\n    let mutable «x» = 1\n    do ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "let_tuple_head",
        fcs: "SynBinding / SynPat.Tuple",
        template: "module Inner =\n    let («x», _y) = (1, 2)\n    do ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "let_function_param",
        fcs: "SynPat.LongIdent argPats / SynPat.Named",
        template: "let f «x» = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "let_typed_param",
        fcs: "SynPat.Paren / SynPat.Typed",
        template: "let f («x»: int) = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "let_tuple_param",
        fcs: "SynPat.Paren / SynPat.Tuple",
        template: "let f («x»: int, y: int) = ignore y; ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "let_uppercase_param",
        fcs: "SynPat.LongIdent (mkSynPatMaybeVar)",
        template: "let f («X»: int) = ignore USE\n",
        status: Status::Opaque,
    },
    Construct {
        id: "let_as_param",
        fcs: "SynPat.As",
        template: "let f ((a: int) as «x») = ignore a; ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "let_rec_self",
        fcs: "SynBinding (rec) / SynPat.LongIdent",
        template: "module Inner =\n    let rec «x» (n: int) : unit = if n > 0 then ignore ‹x›\n",
        status: Status::Interned,
    },
    Construct {
        id: "let_rec_forward",
        fcs: "SynModuleDecl.Let(isRec) second binding",
        template: "module Inner =\n    let rec f () : unit = ignore ‹x›\n    and «x» () = ()\n",
        status: Status::Interned,
    },
    Construct {
        id: "local_let",
        fcs: "SynExpr.LetOrUse / SynPat.Named",
        template: "let f () =\n    let «x» = 1\n    ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "local_let_param",
        fcs: "SynExpr.LetOrUse / SynPat.LongIdent argPats",
        template: "let f () =\n    let g «x» = ignore USE\n    g 1\n",
        status: Status::Interned,
    },
    Construct {
        id: "local_let_rec_forward",
        fcs: "SynExpr.LetOrUse(isRec) second binding",
        template: "let f () =\n    let rec g () : unit = ignore ‹x›\n    and «x» () = ()\n    g ()\n",
        status: Status::Interned,
    },
    Construct {
        id: "local_use",
        fcs: "SynExpr.LetOrUse(isUse)",
        template: "let f () =\n    use «x» = { new System.IDisposable with member _.Dispose() = () }\n    ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "fixed",
        fcs: "SynExpr.Fixed under SynExpr.LetOrUse(isUse)",
        template: "let f (a: int[]) =\n    use «x» = fixed a\n    ignore ‹x›\n",
        status: Status::Interned,
    },
    // ---- lambdas (SynExpr.Lambda / SynSimplePats) ----
    Construct {
        id: "lambda_param",
        fcs: "SynExpr.Lambda / SynSimplePat.Id",
        template: "let f = fun «x» -> ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "lambda_typed_param",
        fcs: "SynExpr.Lambda / SynSimplePat.Typed",
        template: "let f = fun («x»: int) -> ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "lambda_tuple_param",
        fcs: "SynExpr.Lambda / SynSimplePats tuple",
        template: "let f = fun («x»: int, y: int) -> ignore y; ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "lambda_pattern_param",
        fcs: "SynExpr.Lambda parsedData pattern (synthesized _arg)",
        template: "let f = fun (Some «x») -> ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "lambda_curried_second",
        fcs: "SynExpr.Lambda nested",
        template: "let f = fun (_a: int) «x» -> ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "quoted_lambda",
        fcs: "SynExpr.Quote / SynExpr.Lambda",
        template: "let q = <@ fun («x»: int) -> ignore USE @>\n",
        status: Status::Interned,
    },
    Construct {
        id: "delegate_lambda",
        fcs: "SynExpr.Lambda as a delegate argument",
        template: "let d = System.Action<int>(fun «x» -> ignore USE)\n",
        status: Status::Interned,
    },
    // ---- match clauses (SynMatchClause / SynPat) ----
    Construct {
        id: "function_clause",
        fcs: "SynExpr.MatchLambda / SynMatchClause",
        template: "let f : int -> unit = function «x» -> ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "match_var",
        fcs: "SynExpr.Match / SynPat.Named",
        template: "let f (o: int) = match o with «x» -> ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "match_uppercase_var",
        fcs: "SynPat.LongIdent (maybe-var) in a clause",
        template: "let f (o: int) =\n    match o with\n    | «X» -> ignore USE\n",
        status: Status::Opaque,
    },
    Construct {
        id: "match_case_field",
        fcs: "SynPat.LongIdent argPats",
        template: "let f (o: int option) =\n    match o with\n    | Some «x» -> ignore USE\n    | None -> ()\n",
        status: Status::Interned,
    },
    Construct {
        id: "match_guard",
        fcs: "SynMatchClause whenExpr",
        template: "let f (o: int) =\n    match o with\n    | «x» when (ignore USE; true) -> ()\n    | _ -> ()\n",
        status: Status::Interned,
    },
    Construct {
        id: "match_as",
        fcs: "SynPat.As",
        template: "let f (o: int * int) =\n    match o with\n    | (_, _) as «x» -> ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "match_or",
        fcs: "SynPat.Or",
        template: "let f (o: int * int) =\n    match o with\n    | («x», 0) | (_, x) -> ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "match_cons",
        fcs: "SynPat.ListCons",
        template: "let f (o: int list) =\n    match o with\n    | «x» :: _ -> ignore USE\n    | [] -> ()\n",
        status: Status::Interned,
    },
    Construct {
        id: "match_list",
        fcs: "SynPat.ArrayOrList (list)",
        template: "let f (o: int list) =\n    match o with\n    | [«x»] -> ignore USE\n    | _ -> ()\n",
        status: Status::Interned,
    },
    Construct {
        id: "match_array",
        fcs: "SynPat.ArrayOrList (array)",
        template: "let f (o: int[]) =\n    match o with\n    | [|«x»|] -> ignore USE\n    | _ -> ()\n",
        status: Status::Interned,
    },
    Construct {
        id: "match_record",
        fcs: "SynPat.Record",
        template: "type R = { F: int }\nlet f (r: R) =\n    match r with\n    | { F = «x» } -> ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "match_named_field",
        fcs: "SynArgPats.NamePatPairs",
        template: "type U = C of f: int\nlet f (u: U) =\n    match u with\n    | C(f = «x») -> ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "match_type_test_as",
        fcs: "SynPat.As(SynPat.IsInst)",
        template: "let f (o: obj) =\n    match o with\n    | :? string as «x» -> ignore USE\n    | _ -> ()\n",
        status: Status::Interned,
    },
    Construct {
        id: "match_struct_tuple",
        fcs: "SynPat.Tuple(isStruct)",
        template: "let f (o: struct (int * int)) =\n    match o with\n    | struct («x», _) -> ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "try_with",
        fcs: "SynExpr.TryWith / SynMatchClause",
        template: "let f () =\n    try ()\n    with «x» -> ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "try_with_type_test",
        fcs: "SynExpr.TryWith / SynPat.As(IsInst)",
        template: "let f () =\n    try ()\n    with :? System.ArgumentException as «x» -> ignore USE\n",
        status: Status::Interned,
    },
    // ---- `_`-prefixed names, which FCS reports differently ----
    Construct {
        id: "underscore_let_param",
        fcs: "SynPat.Named `_x`",
        template: "let f «_x» = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "underscore_local_let",
        fcs: "SynExpr.LetOrUse / SynPat.Named `_x`",
        template: "let f () =\n    let «_x» = 1\n    ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "underscore_lambda_param",
        fcs: "SynSimplePat.Id `_x`",
        template: "let f = fun «_x» -> ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "underscore_match_var",
        fcs: "SynMatchClause / SynPat.Named `_x`",
        template: "let f (o: int) = match o with «_x» -> ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "underscore_or_pattern",
        fcs: "SynPat.Or binding `_n` in both alternatives",
        template: "let f (o: int * int) =\n    match o with\n    | («_n», 0) | (_, _n) -> ignore ‹_n›\n",
        status: Status::Interned,
    },
    // ---- loops ----
    Construct {
        id: "for_in",
        fcs: "SynExpr.ForEach",
        template: "let f () =\n    for «x» in [1] do\n        ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "for_in_tuple",
        fcs: "SynExpr.ForEach / SynPat.Tuple",
        template: "let f () =\n    for («x», _) in [(1, 2)] do\n        ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "for_to",
        fcs: "SynExpr.For",
        template: "let f () =\n    for «x» = 0 to 1 do\n        ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "for_downto",
        fcs: "SynExpr.For(down)",
        template: "let f () =\n    for «x» = 1 downto 0 do\n        ignore USE\n",
        status: Status::Interned,
    },
    // ---- computation expressions ----
    Construct {
        id: "seq_for",
        fcs: "SynExpr.ForEach in a sequence expression",
        template: "let s =\n    seq {\n        for «x» in [1] do\n            ignore USE\n            yield 1\n    }\n",
        status: Status::Interned,
    },
    Construct {
        id: "ce_let_bang",
        fcs: "SynExpr.LetOrUseBang",
        template: "let a =\n    async {\n        let! «x» = async { return 1 }\n        ignore USE\n    }\n",
        status: Status::Interned,
    },
    Construct {
        id: "ce_use_bang",
        fcs: "SynExpr.LetOrUseBang(isUse)",
        template: "let a =\n    async {\n        use! «x» = async { return { new System.IDisposable with member _.Dispose() = () } }\n        ignore USE\n    }\n",
        status: Status::Interned,
    },
    Construct {
        id: "ce_and_bang",
        fcs: "SynExpr.LetOrUseBang andBangs",
        template: "type B() =\n    member _.Bind(m: 'a, f: 'a -> 'b) = f m\n    member _.Return(v: 'a) = v\n    member _.MergeSources(a: 'a, b: 'b) = (a, b)\nlet b = B()\nlet v =\n    b {\n        let! _p = 1\n        and! «x» = 2\n        return (ignore USE)\n    }\n",
        status: Status::Interned,
    },
    Construct {
        id: "ce_let",
        fcs: "SynExpr.LetOrUse inside a computation expression",
        template: "let a =\n    async {\n        let «x» = 1\n        ignore USE\n    }\n",
        status: Status::Interned,
    },
    Construct {
        id: "ce_match_bang",
        fcs: "SynExpr.MatchBang",
        template: "let a =\n    async {\n        match! async { return 1 } with\n        | «x» -> ignore USE\n    }\n",
        status: Status::Interned,
    },
    Construct {
        id: "ce_for",
        fcs: "SynExpr.ForEach in a computation expression",
        template: "let a =\n    async {\n        for «x» in [1] do\n            ignore USE\n    }\n",
        status: Status::Interned,
    },
    Construct {
        id: "query_for",
        fcs: "SynExpr.ForEach in a query",
        template: "let q =\n    query {\n        for «x» in [1] do\n        select (ignore USE)\n    }\n",
        status: Status::Interned,
    },
    Construct {
        id: "query_let",
        fcs: "SynExpr.LetOrUse in a query",
        template: "let q =\n    query {\n        for a in [1] do\n        let «x» = a\n        select (ignore USE)\n    }\n",
        status: Status::Interned,
    },
    Construct {
        id: "query_join",
        fcs: "custom operation `join` (SynExpr.JoinIn)",
        template: "let q =\n    query {\n        for a in [1] do\n        join «x» in [1] on (a = x)\n        select (ignore USE)\n    }\n",
        status: Status::Opaque,
    },
    Construct {
        id: "query_join_tuple",
        fcs: "custom operation `join` over a tuple pattern",
        template: "let q =\n    query {\n        for a in [1] do\n        join («x», _y) in [(1, 2)] on (a = x)\n        select (ignore USE)\n    }\n",
        status: Status::Opaque,
    },
    Construct {
        id: "query_group_join_into",
        fcs: "custom operation `groupJoin … into`",
        template: "let q =\n    query {\n        for a in [1] do\n        groupJoin b in [1] on (a = b) into «x»\n        select (ignore USE)\n    }\n",
        status: Status::Opaque,
    },
    Construct {
        id: "query_left_outer_join_into",
        fcs: "custom operation `leftOuterJoin … into`",
        template: "let q =\n    query {\n        for a in [1] do\n        leftOuterJoin b in [1] on (a = b) into «x»\n        select (ignore USE)\n    }\n",
        status: Status::Opaque,
    },
    Construct {
        id: "query_group_by_into",
        fcs: "custom operation `groupBy … into`",
        template: "let q =\n    query {\n        for a in [1] do\n        groupBy a into «x»\n        select (ignore USE)\n    }\n",
        status: Status::Opaque,
    },
    // ---- active patterns ----
    Construct {
        id: "active_pattern_param",
        fcs: "SynBinding / SynPat.LongIdent(active pattern) argPats",
        template: "let (|P|) «x» = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "active_pattern_case",
        fcs: "SynPat.LongIdent(active pattern name)",
        template: "let (|«A»|B|) (n: int) = if n = 0 then A else B\n",
        status: Status::Interned,
    },
    // ---- type definitions ----
    Construct {
        id: "union_case",
        fcs: "SynUnionCase",
        template: "type U =\n    | «A»\n    | B\nlet g = ‹A›\n",
        status: Status::Interned,
    },
    Construct {
        id: "single_case_union_abbreviation_shape",
        fcs: "SynTypeDefnSimpleRepr.TypeAbbrev read as a union case",
        template: "type T = «X»\nlet g = ‹X›\n",
        status: Status::Opaque,
    },
    Construct {
        id: "exception_constructor",
        fcs: "SynExceptionDefn",
        template: "exception «E» of int\nlet g = ‹E› 1\n",
        status: Status::Interned,
    },
    Construct {
        id: "type_parameter_on_let",
        fcs: "SynValTyparDecls",
        template: "let f<«'T»> (y: ‹'T›) = y\n",
        status: Status::Interned,
    },
    Construct {
        id: "type_parameter_on_type",
        fcs: "SynComponentInfo typarDecls",
        template: "type C<«'T»>() =\n    member _.M(y: ‹'T›) = y\n",
        status: Status::Interned,
    },
    Construct {
        id: "type_parameter_on_type_single_prefix",
        fcs: "SynComponentInfo typarDecls (SinglePrefix)",
        template: "type «'T» C() =\n    member _.M(y: ‹'T›) = y\n",
        status: Status::Interned,
    },
    Construct {
        id: "type_parameter_on_type_prefix_list",
        fcs: "SynComponentInfo typarDecls (PrefixList)",
        template: "type ('U, «'T») C() =\n    member _.M(y: ‹'T›, z: 'U) = y\n",
        status: Status::Interned,
    },
    Construct {
        id: "type_parameter_on_member",
        fcs: "SynValTyparDecls on a member",
        template: "type C() =\n    member _.M<«'T»>(y: ‹'T›) = y\n",
        status: Status::Interned,
    },
    Construct {
        id: "type_parameter_implicit",
        fcs: "SynType.Var first occurrence",
        template: "let f (y: «'a») = (y: ‹'a›)\n",
        status: Status::Opaque,
    },
    // ---- members (SynMemberDefn / SynBinding) ----
    Construct {
        id: "member_param",
        fcs: "SynMemberDefn.Member / SynPat.Paren",
        template: "type C() =\n    member _.M(«x»: int) = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "member_curried_param",
        fcs: "SynMemberDefn.Member / SynPat.Named",
        template: "type C() =\n    member _.M «x» = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "member_self",
        fcs: "SynMemberDefn.Member self identifier",
        template: "type C() =\n    member «x».M() = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "member_optional_param",
        fcs: "SynPat.OptionalVal",
        template: "type C() =\n    member _.M(?«x»: int) = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "member_param_array",
        fcs: "SynPat.Attrib([<ParamArray>])",
        template: "type C() =\n    member _.M([<System.ParamArray>] «x»: int[]) = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "static_member_param",
        fcs: "SynMemberDefn.Member(static)",
        template: "type C() =\n    static member M(«x»: int) = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "property_setter_param",
        fcs: "SynMemberDefn.GetSetMember setter",
        template: "type C() =\n    member _.P\n        with set («x»: int) = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "indexer_getter_param",
        fcs: "SynMemberDefn.GetSetMember getter",
        template: "type C() =\n    member _.Item\n        with get («x»: int) = ignore USE; 0\n",
        status: Status::Interned,
    },
    Construct {
        id: "primary_ctor_param",
        fcs: "SynMemberDefn.ImplicitCtor / SynSimplePats",
        template: "type C(«x»: int) =\n    member _.M = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "primary_ctor_uppercase_param",
        fcs: "SynMemberDefn.ImplicitCtor / SynSimplePats (uppercase)",
        template: "type C(«X»: int) =\n    member _.M = ignore USE\n",
        status: Status::Opaque,
    },
    Construct {
        id: "primary_ctor_case_spelled_param",
        fcs: "SynSimplePats from SimplePatsOfPat: a name spelled like a union case",
        template: "type C(«Some»: int) =\n    member _.M = 1\n",
        status: Status::Opaque,
    },
    Construct {
        id: "primary_ctor_optional_param",
        fcs: "SynSimplePat.Id(isOptional)",
        template: "type C(?«x»: int) =\n    member _.M = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "primary_ctor_as_param",
        fcs: "SimplePatsOfPat over SynPat.As",
        template: "type C(_ as «x») =\n    member _.M = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "primary_ctor_attributed_param",
        fcs: "SynSimplePat.Attrib",
        template: "type C([<System.ParamArray>] «x»: int[]) =\n    member _.M = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "primary_ctor_tuple_second_param",
        fcs: "SynSimplePats tuple, second element",
        template: "type C(a: int, «x»: int) =\n    member _.M = (ignore a; ignore USE)\n",
        status: Status::Interned,
    },
    Construct {
        id: "primary_ctor_self",
        fcs: "SynMemberDefn.ImplicitCtor selfIdentifier",
        template: "type C() as «x» =\n    member _.M = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "class_let",
        fcs: "SynMemberDefn.LetBindings",
        template: "type C() =\n    let «x» = 1\n    member _.M = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "class_let_param",
        fcs: "SynMemberDefn.LetBindings / SynPat.LongIdent argPats",
        template: "type C() =\n    let g «x» = ignore USE\n    member _.M = g 1\n",
        status: Status::Interned,
    },
    Construct {
        id: "class_static_let",
        fcs: "SynMemberDefn.LetBindings(isStatic)",
        template: "type C() =\n    static let «x» = 1\n    static member M = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "explicit_ctor_param",
        fcs: "SynMemberDefn.Member(new) / SynPat.Paren",
        template: "type C =\n    val F: int\n    new («x»: int) = { F = 0 } then ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "explicit_ctor_self",
        fcs: "SynMemberDefn.Member(new) as-self",
        template: "type C =\n    val F: int\n    new () as «x» = { F = 0 } then ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "interface_impl_param",
        fcs: "SynMemberDefn.Interface members",
        template: "type C() =\n    interface System.IComparable with\n        member _.CompareTo(«x») = ignore USE; 0\n",
        status: Status::Interned,
    },
    Construct {
        id: "object_expression_param",
        fcs: "SynExpr.ObjExpr bindings",
        template: "let o =\n    { new System.Object() with\n        member _.Equals(«x»: obj) = ignore USE; false }\n",
        status: Status::Interned,
    },
    Construct {
        id: "object_expression_self",
        fcs: "SynExpr.ObjExpr self identifier",
        template: "let o =\n    { new System.Object() with\n        member «x».ToString() = ignore USE; \"\" }\n",
        status: Status::Interned,
    },
    Construct {
        id: "class_inherit_base",
        fcs: "SynMemberDefn.Inherit base value, declared at the whole `inherit` member",
        template: "type B() =\n    member _.M() = 1\ntype C() =\n    «inherit B()»\n    member _.N() = ‹base›.M()\n",
        status: Status::Opaque,
    },
    Construct {
        id: "object_expression_base",
        fcs: "SynExpr.ObjExpr base value, declared at the base type",
        template: "let o =\n    { new «System.Object»() with\n        override _.ToString() = ‹base›.ToString() }\n",
        status: Status::Opaque,
    },
    Construct {
        id: "augmentation_member_param",
        fcs: "SynMemberDefn.Member of an intrinsic augmentation the walk proves the scope of",
        template: "type T() = class end\ntype T with\n    member _.M(«x»: int) = ignore USE\n",
        status: Status::Interned,
    },
    Construct {
        id: "augmentation_member_param_unproven",
        fcs: "SynMemberDefn.Member of an intrinsic augmentation of a generic type, outside the proven subset",
        template: "type T<'a>() = class end\ntype T<'a> with\n    member _.M(«x»: int) = ignore USE\n",
        status: Status::Opaque,
    },
    Construct {
        id: "extension_member_param",
        fcs: "SynMemberDefn.Member of an optional extension",
        template: "type System.String with\n    member _.M(«x»: int) = ignore USE\n",
        status: Status::Opaque,
    },
    Construct {
        id: "exception_member_param",
        fcs: "SynExceptionDefn members",
        template: "exception E of int with\n    member _.M(«x»: int) = ignore USE\n",
        status: Status::Opaque,
    },
    Construct {
        id: "attribute_argument_type_parameter",
        fcs: "SynType.Var inside an attribute argument's SynExpr.TypeApp",
        template: "type A(t: System.Type) =\n    inherit System.Attribute()\n[<A(typeof<«'a» list>)>]\nlet f () = ()\n",
        status: Status::Opaque,
    },
    // ---- declarations without a body scope ----
    Construct {
        id: "extern_name",
        fcs: "SynBinding of an extern declaration",
        template: "module Inner =\n    [<System.Runtime.InteropServices.DllImport(\"x\")>]\n    extern int «X»(int y)\n    let g = ‹X› 1\n",
        status: Status::Opaque,
    },
    Construct {
        id: "extern_param",
        fcs: "SynPat of an extern argument",
        template: "[<System.Runtime.InteropServices.DllImport(\"x\")>]\nextern int f(int «x»)\n",
        status: Status::Opaque,
    },
    Construct {
        id: "abstract_slot_arg_name",
        fcs: "SynValSig argument names",
        template: "type I =\n    abstract M: «x»: int -> unit\n",
        status: Status::FcsSilent(
            "an abstract slot's argument name is a parameter name in a signature; FCS binds \
             no value for it, and there is no body for one to be in scope in",
        ),
    },
    Construct {
        id: "auto_property",
        fcs: "SynMemberDefn.AutoProperty",
        template: "type C() =\n    member val «X» = 1 with get, set\n    member _.M = ‹X›\n",
        status: Status::NotInScope,
    },
];

/// One program the grid checks: a row, written with one spelling of its
/// `USE`.
struct Cell {
    row: &'static Construct,
    variant: &'static str,
    src: String,
    binder: TextRange,
    binder_name: String,
    uses: Vec<TextRange>,
}

fn cells() -> Vec<Cell> {
    let mut out = Vec::new();
    for row in CONSTRUCTS {
        let variants: &[(&str, &str)] = if row.template.contains("USE") {
            &[("bare", "‹N›"), ("dotted", "(‹N›.GetHashCode())")]
        } else {
            &[("as-written", "")]
        };
        for (variant, spelling) in variants {
            out.push(cell(row, variant, spelling));
        }
    }
    out
}

/// Strip `row`'s markers, recording the binder's range and every use's.
fn cell(row: &'static Construct, variant: &'static str, use_spelling: &str) -> Cell {
    let (open, close) = (
        row.template.find('«').expect("a binder marker"),
        row.template.find('»').expect("a binder marker"),
    );
    let name = &row.template[open + '«'.len_utf8()..close];
    let text = row
        .template
        .replace("USE", &use_spelling.replace('N', name));
    let mut src = PRELUDE.to_string();
    let mut binder = None;
    let mut uses = Vec::new();
    let mut mark = None;
    for c in text.chars() {
        match c {
            '«' | '‹' => mark = Some((c, src.len())),
            '»' | '›' => {
                let (kind, start) = mark.take().expect("balanced markers");
                let range = TextRange::new(
                    u32::try_from(start).unwrap().into(),
                    u32::try_from(src.len()).unwrap().into(),
                );
                if kind == '«' {
                    binder = Some(range);
                } else {
                    uses.push(range);
                }
            }
            c => src.push(c),
        }
    }
    Cell {
        row,
        variant,
        src,
        binder: binder.expect("one binder"),
        binder_name: name.to_string(),
        uses,
    }
}

/// The environments every cell is resolved under: none, where a builder or an
/// imported name is unknown and the walk declines more, and the reference
/// assemblies, where it knows `query`'s operations and FSharp.Core's values and
/// commits more.
fn environments() -> [(&'static str, &'static AssemblyEnv); 2] {
    static EMPTY: std::sync::OnceLock<AssemblyEnv> = std::sync::OnceLock::new();
    [
        ("empty-env", EMPTY.get_or_init(AssemblyEnv::default)),
        ("bcl-env", full_bcl_env()),
    ]
}

fn resolve(src: &str, env: &AssemblyEnv) -> (SyntaxNode, ResolvedFile) {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "a cell failed to parse: {:?}\n{src}",
        parsed.errors
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let root = parsed.root.clone();
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let rf = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    (root, rf)
}

fn status_of(verdict: BinderVerdict) -> &'static str {
    match verdict {
        BinderVerdict::Interned => "Interned",
        BinderVerdict::Opaque => "Opaque",
        BinderVerdict::Unmodelled => "Unmodelled",
    }
}

fn pinned(status: Status) -> &'static str {
    match status {
        Status::Interned => "Interned",
        Status::Opaque => "Opaque",
        Status::FcsSilent(_) => "FcsSilent",
        Status::NotInScope => "NotInScope",
    }
}

#[test]
fn every_binding_construct_is_known_to_the_walk() {
    let cells = cells();
    let paths: Vec<PathBuf> = cells
        .iter()
        .map(|c| temp_fs_file(&format!("binder_{}_{}", c.row.id, c.variant), &c.src))
        .collect();
    let census = parse_census_jsonl(&invoke_fcs_dump_census(&paths));
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    assert_eq!(census.len(), cells.len(), "one census line per cell");

    let mut failures: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut observed: BTreeMap<&str, String> = BTreeMap::new();
    for (cell, file) in cells.iter().zip(&census) {
        let mut fail = |at: &str, why: String| {
            failures
                .entry(cell.row.id)
                .or_default()
                .push(format!("[{} {at}] {why}", cell.variant))
        };
        if !file.ok || file.has_check_errors {
            fail("fcs", format!("FCS rejects the cell:\n{}", cell.src));
            continue;
        }
        let binders = census_oracle_binders(file, &cell.src);
        let uses = census_oracle_uses(file, &cell.src);
        let reported = binders
            .iter()
            .any(|b| b.range == cell.binder && b.introduces_a_name());
        // What the oracle says of each planted use: whether it binds it to the
        // row's binder.
        for &u in &cell.uses {
            let bound = uses
                .iter()
                .any(|o| o.range == u && o.decl == Some(cell.binder));
            match cell.row.status {
                Status::NotInScope if bound => fail(
                    "fcs",
                    format!(
                        "FCS binds the planted use at {u:?} to the binder, which the row says no bare lookup reaches"
                    ),
                ),
                Status::NotInScope => {}
                _ if reported && !bound => fail(
                    "fcs",
                    format!(
                        "FCS does not bind the planted use at {u:?} to the binder {:?}",
                        cell.binder_name
                    ),
                ),
                _ => {}
            }
        }
        for (env_name, env) in environments() {
            let (root, rf) = resolve(&cell.src, env);
            let observed_status = if !reported {
                "FcsSilent"
            } else if is_unscoped(&root, cell.binder) {
                "NotInScope"
            } else {
                let oracle = binders
                    .iter()
                    .find(|b| b.range == cell.binder && b.introduces_a_name())
                    .expect("reported");
                status_of(binder_verdict(&rf, oracle))
            };
            observed.insert(cell.row.id, observed_status.to_string());
            if observed_status != pinned(cell.row.status) {
                fail(
                    env_name,
                    format!(
                        "pinned {:?}, observed {observed_status} (site {})",
                        cell.row.status,
                        binder_site(&root, cell.binder)
                    ),
                );
            }
            // No commit at a planted use except to the binder; a use the row
            // says the binder does not reach is graded by the oracle's answer.
            for &u in &cell.uses {
                let oracle_decl = uses.iter().find(|o| o.range == u).and_then(|o| o.decl);
                match rf.resolution_at(u) {
                    None | Some(Resolution::Deferred(_)) => {}
                    Some(res @ (Resolution::Local(_) | Resolution::Item(_)))
                        if rf.resolved_def(res).map(|d| d.range) == oracle_decl
                            && oracle_decl.is_some() => {}
                    Some(res) => fail(
                        env_name,
                        format!(
                            "the planted use at {u:?} commits to {res:?} ({:?}); FCS declares it at {oracle_decl:?}",
                            rf.resolved_def(res).map(|d| d.range)
                        ),
                    ),
                }
            }
            for w in wrong_commits(&rf, &uses) {
                fail(env_name, w);
            }
            // Every other binder in the cell.
            for b in unmodelled_binders(&rf, &root, &binders) {
                if b.range != cell.binder {
                    fail(
                        env_name,
                        format!(
                            "a second binder {:?} at {:?} ({}) is unmodelled",
                            b.name, b.range, b.site
                        ),
                    );
                }
            }
        }
    }
    let rendered: Vec<String> = failures
        .iter()
        .map(|(id, why)| {
            format!(
                "{id} (observed {}):\n  {}",
                observed.get(id).map_or("?", |s| s),
                why.join("\n  ")
            )
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} constructs disagree with the table:\n{}",
        failures.len(),
        CONSTRUCTS.len(),
        rendered.join("\n")
    );
}

/// The table and the exemption list agree: a row says no bare lookup reaches
/// its binder exactly when its binder's leaf is exempt. The grid checks each
/// such row's claim against FCS, so an exemption no row checks is one nobody
/// does.
#[test]
fn every_unscoped_leaf_has_a_row() {
    for (kind, _) in UNSCOPED_BINDER_LEAVES {
        let held = cells().iter().any(|c| {
            c.row.status == Status::NotInScope && {
                let (root, _) = resolve(&c.src, &AssemblyEnv::default());
                is_unscoped(&root, c.binder)
                    && root
                        .token_at_offset(c.binder.start())
                        .right_biased()
                        .and_then(|t| t.parent())
                        .is_some_and(|n| n.kind() == *kind)
            }
        });
        assert!(held, "no NotInScope row holds the exemption of {kind:?}");
    }
}

/// Every commit of ours at a use FCS binds in-file that names another binder.
fn wrong_commits(rf: &ResolvedFile, uses: &[OracleUse]) -> Vec<String> {
    uses.iter()
        .filter_map(|u| {
            let decl = u.decl?;
            let res = rf.resolution_at(u.range)?;
            let ours = match res {
                Resolution::Local(_) | Resolution::Item(_) => rf.resolved_def(res)?.range,
                _ => return None,
            };
            (ours != decl).then(|| {
                format!(
                    "the use at {:?} commits to the binder at {ours:?}; FCS declares it at {decl:?}",
                    u.range
                )
            })
        })
        .collect()
}

/// Programs whose binders are all in the table, but which put them in a
/// relation the rows do not: a custom operation's variable over a `for`
/// variable, an `into` hiding what came before it, a possible union case
/// beside a parameter of its name. Each is graded at every
/// use FCS reports, and must leave no binder unmodelled.
const SCOPE_CASES: &[(&str, &str)] = &[
    (
        "group_by_into_hides_earlier_variables",
        "module M\nlet x = \"outer\"\nlet q =\n    query {\n        for x in [1] do\n        groupBy x into g\n        select (g.Key, x)\n    }\n",
    ),
    (
        "group_join_into_keeps_earlier_variables",
        "module M\nlet c = \"outer\"\nlet q =\n    query {\n        for c in [1] do\n        groupJoin p in [1] on (c = p) into ps\n        select (c, ps)\n    }\n",
    ),
    (
        "join_shadows_an_earlier_let",
        "module M\nlet q =\n    query {\n        for a in [1] do\n        let x = string a\n        join x in [2] on (a = x)\n        select (x + 1)\n    }\n",
    ),
    (
        "primary_ctor_case_spelled_param_leaves_the_case_in_scope",
        "module M\ntype C(Some: int) =\n    member _.M = Some 1\n",
    ),
    (
        "let_after_into_is_its_own_binder",
        "module M\nlet y = \"outer\"\nlet q =\n    query {\n        for x in [1] do\n        groupBy x into g\n        let y = g.Key\n        select y\n    }\n",
    ),
    (
        "possible_case_leaves_a_parameter_binder",
        "module M\nmodule Types =\n    type X = class end\nmodule N =\n    open Types\n    type T = X\nopen N\nlet X = 123\nlet f (X: int) = X\n",
    ),
];

#[test]
fn binder_scopes_agree_with_fcs() {
    let paths: Vec<PathBuf> = SCOPE_CASES
        .iter()
        .map(|(id, src)| temp_fs_file(&format!("binder_scope_{id}"), src))
        .collect();
    let census = parse_census_jsonl(&invoke_fcs_dump_census(&paths));
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    let mut failures = Vec::new();
    for ((id, src), file) in SCOPE_CASES.iter().zip(&census) {
        if !file.ok || file.has_check_errors {
            failures.push(format!("{id}: FCS rejects it"));
            continue;
        }
        let uses = census_oracle_uses(file, src);
        let binders = census_oracle_binders(file, src);
        for (env_name, env) in environments() {
            let (root, rf) = resolve(src, env);
            failures.extend(
                wrong_commits(&rf, &uses)
                    .into_iter()
                    .map(|w| format!("{id} [{env_name}]: {w}")),
            );
            failures.extend(
                unmodelled_binders(&rf, &root, &binders)
                    .into_iter()
                    .map(|b| {
                        format!(
                            "{id} [{env_name}]: {:?} at {:?} ({}) is unmodelled",
                            b.name, b.range, b.site
                        )
                    }),
            );
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every row is a distinct construct.
#[test]
fn the_construct_table_has_distinct_ids() {
    let mut ids: Vec<_> = CONSTRUCTS.iter().map(|c| c.id).collect();
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), before, "duplicate construct ids");
    for c in CONSTRUCTS {
        assert!(!c.fcs.is_empty(), "{} names no FCS position", c.id);
    }
}

/// Multi-file programs whose binders reach a later file: a binder the walk
/// declines rather than interns has to be declined there too, where the later
/// file can see an outer same-named value it would otherwise commit to.
const PROJECT_SCOPE_CASES: &[(&str, Files)] = &[
    (
        "abbreviation_case_through_an_open",
        &[
            ("BinderA.fs", "module BinderA\ntype T = X\n"),
            (
                "BinderB.fs",
                "module BinderB\nlet X = \"outer\"\nopen BinderA\nlet y = X\nlet z = X.GetHashCode()\n",
            ),
        ],
    ),
    (
        "abbreviation_case_qualified",
        &[
            ("BinderA.fs", "module BinderA\ntype T = X\n"),
            ("BinderB.fs", "module BinderB\nlet y = BinderA.X\n"),
        ],
    ),
    (
        "extern_through_an_open",
        &[
            (
                "BinderA.fs",
                "module BinderA\n[<System.Runtime.InteropServices.DllImport(\"x\")>]\nextern int f(int y)\n",
            ),
            (
                "BinderB.fs",
                "module BinderB\nlet f (n: int) = n\nopen BinderA\nlet g = f 1\n",
            ),
        ],
    ),
];

#[test]
fn binders_declined_in_one_file_stay_declined_in_the_next() {
    let mut failures = Vec::new();
    for (id, files) in PROJECT_SCOPE_CASES {
        let (root, written) = temp_fs_tree(&format!("binder_project_{id}"), files);
        let paths: Vec<&std::path::Path> = written.iter().map(|(p, _)| p.as_path()).collect();
        let fcs = parse_fcs_uses_project(&invoke_fcs_dump_project_with_refs(&paths, &[]), &written);
        let _ = std::fs::remove_dir_all(&root);
        let errors: Vec<&String> = fcs.iter().flat_map(|f| &f.errors).collect();
        if !errors.is_empty() {
            failures.push(format!("{id}: FCS rejects it: {errors:?}"));
            continue;
        }
        for (env_name, env) in environments() {
            let proj = resolve_project_files(&project(files), env);
            failures.extend(
                unmodelled_project_binder_failures(id, &proj, &written, &fcs)
                    .into_iter()
                    .map(|f| format!("[{env_name}] {f}")),
            );
            for (i, (path, _)) in written.iter().enumerate() {
                let Some(file) = fcs.iter().find(|f| f.path.file_name() == path.file_name()) else {
                    continue;
                };
                for u in file.uses.iter().filter(|u| !u.is_from_definition) {
                    let Some(decl) = &u.decl else { continue };
                    let range = TextRange::new(
                        u32::try_from(u.start).unwrap().into(),
                        u32::try_from(u.end).unwrap().into(),
                    );
                    let Some(
                        res @ (Resolution::Local(_)
                        | Resolution::Item(_)
                        | Resolution::EarlierFileType { .. }),
                    ) = proj.file(i).resolution_at(range)
                    else {
                        continue;
                    };
                    let ours = match res {
                        Resolution::Item(_) | Resolution::EarlierFileType { .. } => {
                            proj.item_def(res).map(|(f, d)| {
                                (written[f].0.file_name().map(ToOwned::to_owned), d.range)
                            })
                        }
                        _ => proj
                            .file(i)
                            .resolved_def(res)
                            .map(|d| (path.file_name().map(ToOwned::to_owned), d.range)),
                    };
                    let theirs = (
                        decl.file.file_name().map(ToOwned::to_owned),
                        TextRange::new(
                            u32::try_from(decl.start).unwrap().into(),
                            u32::try_from(decl.end).unwrap().into(),
                        ),
                    );
                    if ours.as_ref() != Some(&theirs) {
                        failures.push(format!(
                            "{id} [{env_name}]: {:?} at {range:?} in {:?} commits to {ours:?}; FCS declares it at {theirs:?}",
                            u.name,
                            path.file_name()
                        ));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Programs in which declining a possible case would cost a commitment FCS
/// makes. Certain-implies-exact cannot see a decline, so each names the last
/// spelling of a name that must commit to what FCS binds it to.
/// A project's `(file name, source)` pairs, in Compile order.
type Files = &'static [(&'static str, &'static str)];

const MUST_COMMIT_CASES: &[(&str, Files, &str)] = &[
    (
        "later_value_shadows_an_abbreviation_case",
        &[(
            "BinderA.fs",
            "module BinderA\nmodule B =\n    type T = X\n    let X = 123\nopen B\nlet y = X\n",
        )],
        "X",
    ),
    (
        "later_value_shadows_an_abbreviation_case_across_files",
        &[
            ("BinderA.fs", "module BinderA\ntype T = X\nlet X = 123\n"),
            ("BinderB.fs", "module BinderB\nopen BinderA\nlet y = X\n"),
        ],
        "X",
    ),
    (
        "qualified_access_case_leaves_the_bare_name_alone",
        &[(
            "BinderA.fs",
            "module BinderA\nlet X = 123\n[<RequireQualifiedAccess>]\ntype T = X\nlet y = X\n",
        )],
        "X",
    ),
    (
        "into_applied_outside_a_custom_operation_binds_nothing",
        &[(
            "BinderA.fs",
            "module BinderA\nlet q =\n    query {\n        for x in [1] do\n        let into v = v + 1\n        select (into x, x)\n    }\n",
        )],
        "x",
    ),
    (
        "into_parameter_is_an_ordinary_binder",
        &[(
            "BinderA.fs",
            "module BinderA\nlet q =\n    query {\n        for x in [1] do\n        let into v = v + 1\n        select (into x)\n    }\n",
        )],
        "v",
    ),
];

#[test]
fn possible_cases_decline_only_what_they_can_capture() {
    let mut failures = Vec::new();
    for (id, files, name) in MUST_COMMIT_CASES {
        let (root, written) = temp_fs_tree(&format!("binder_must_{id}"), files);
        let paths: Vec<&std::path::Path> = written.iter().map(|(p, _)| p.as_path()).collect();
        let fcs = parse_fcs_uses_project(&invoke_fcs_dump_project_with_refs(&paths, &[]), &written);
        let _ = std::fs::remove_dir_all(&root);
        let errors: Vec<&String> = fcs.iter().flat_map(|f| &f.errors).collect();
        if !errors.is_empty() {
            failures.push(format!("{id}: FCS rejects it: {errors:?}"));
            continue;
        }
        let last = written.len() - 1;
        let src = &written[last].1;
        let at = src.rfind(name).expect("the probed name");
        let range = TextRange::new(
            u32::try_from(at).unwrap().into(),
            u32::try_from(at + name.len()).unwrap().into(),
        );
        let file = fcs
            .iter()
            .find(|f| f.path.file_name() == written[last].0.file_name())
            .expect("the probe file's uses");
        let decl = file
            .uses
            .iter()
            .find(|u| u.start == at && u.end == at + name.len())
            .and_then(|u| u.decl.clone())
            .unwrap_or_else(|| panic!("{id}: FCS binds the probe in-project"));
        for (env_name, env) in environments() {
            let proj = resolve_project_files(&project(files), env);
            let ours = match proj.file(last).resolution_at(range) {
                Some(res @ Resolution::Item(_)) => proj
                    .item_def(res)
                    .map(|(f, d)| (written[f].0.file_name().map(ToOwned::to_owned), d.range)),
                Some(res @ Resolution::Local(_)) => proj
                    .file(last)
                    .resolved_def(res)
                    .map(|d| (written[last].0.file_name().map(ToOwned::to_owned), d.range)),
                other => {
                    failures.push(format!("{id} [{env_name}]: the probe gave {other:?}"));
                    continue;
                }
            };
            let theirs = (
                decl.file.file_name().map(ToOwned::to_owned),
                TextRange::new(
                    u32::try_from(decl.start).unwrap().into(),
                    u32::try_from(decl.end).unwrap().into(),
                ),
            );
            if ours.as_ref() != Some(&theirs) {
                failures.push(format!(
                    "{id} [{env_name}]: committed {ours:?}, FCS {theirs:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
