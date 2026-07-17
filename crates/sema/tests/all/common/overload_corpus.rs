//! Stage OV-9(a) — the **overload-corpus generator**
//! (`docs/overload-resolution-plan.md` §6, OV-9).
//!
//! The generator emits *two* sources that are two views of one universe:
//!
//! * a **C# assembly** (`OvCorpus`) declaring overload *sets* — every unordered
//!   pair of parameter types drawn from [`PTy`] (the closed decidable set ∪
//!   `obj` ∪ a user base/derived pair ∪ arrays), plus the decorated shapes the
//!   plan names as landmines: optional, `params`, `out`, split-arity, a group
//!   split across an inheritance level, an override, and a generic candidate.
//!   Each set is declared **twice** — as statics (`M`) and as instance methods
//!   (`I`) — so the OV-6 (instance) and OV-7 (static) engine paths see the same
//!   matrix;
//! * an **F# call-site matrix**: for every declared type, a call at every
//!   argument shape in [`ARGS`] (unit, each ground literal / factory value, and
//!   a few multi-argument tuples), on **one line each**.
//!
//! The consumer (`tests/all/overload_corpus_diff.rs`) compiles the C# once,
//! references it from *both* FCS (`BORZOI_FCS_EXTRA_REFS`) and our
//! `AssemblyEnv`, and asserts the OV-9 property line by line: **our commit
//! agrees with the OV-1 oracle's chosen overload, or we deferred.**
//!
//! Two constraints shape the F# rendering, and both are load-bearing:
//!
//! * **No `open`, no attribute, no augmentation.** OV-6's extension-absence gate
//!   (`ExtensionScope`) treats any of those as an extension *source* and defers
//!   the whole call — which would make the differential vacuous. So every
//!   corpus type is reached by its fully-qualified path.
//! * **Receivers come from static factories.** Our engine has no object-
//!   construction path, so an instance receiver of a corpus type is obtained
//!   from `OvCorpus.Make.New_<Type>()` — a single-candidate static call the
//!   engine commits, whose return type grounds the receiver. The same trick
//!   manufactures the ground `obj` / `BaseTy` / `DerivedTy` / `int[]` argument
//!   values that no F# literal can produce.
//!
//! Each call site sits on its own line and binds `r<line>`, so the differential
//! keys everything (our member resolution, our binder type, FCS's chosen
//! overload) by **line number** — no range bookkeeping, and a failing case is
//! already minimal: one declared type, one call, one line, printed verbatim.

#![allow(dead_code)] // the differential and the coverage report use different subsets.

/// A parameter / argument type in the generated universe.
///
/// The set is chosen to straddle the type prong's decidable frontier (plan
/// §4.2): `Int`…`ByteArr` are in the **closed decidable set** (a position
/// holding one can *refute* its candidate), while `Obj`, `BaseTy` and
/// `DerivedTy` are deliberately **outside** it (an `obj` parameter can never
/// refute — the P5 landmine — and a class parameter's conversion channels are
/// open-ended), so the matrix contains both refutable and unrefutable losers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PTy {
    Int,
    Int64,
    Double,
    Decimal,
    String,
    Char,
    Bool,
    IntArr,
    Obj,
    BaseTy,
    DerivedTy,
}

impl PTy {
    /// How the type is written in the generated C#.
    fn csharp(self) -> &'static str {
        match self {
            PTy::Int => "int",
            PTy::Int64 => "long",
            PTy::Double => "double",
            PTy::Decimal => "decimal",
            PTy::String => "string",
            PTy::Char => "char",
            PTy::Bool => "bool",
            PTy::IntArr => "int[]",
            PTy::Obj => "object",
            PTy::BaseTy => "BaseTy",
            PTy::DerivedTy => "DerivedTy",
        }
    }

    /// The suffix used in a generated type name (`Ov_Int_String`).
    fn tag(self) -> &'static str {
        match self {
            PTy::Int => "Int",
            PTy::Int64 => "Int64",
            PTy::Double => "Double",
            PTy::Decimal => "Decimal",
            PTy::String => "String",
            PTy::Char => "Char",
            PTy::Bool => "Bool",
            PTy::IntArr => "IntArr",
            PTy::Obj => "Obj",
            PTy::BaseTy => "BaseTy",
            PTy::DerivedTy => "DerivedTy",
        }
    }
}

/// Every parameter type the pair-matrix draws from — 11 types ⇒ 55 unordered
/// pairs, each becoming one two-candidate overload set.
const PARAM_TYPES: [PTy; 11] = [
    PTy::Int,
    PTy::Int64,
    PTy::Double,
    PTy::Decimal,
    PTy::String,
    PTy::Char,
    PTy::Bool,
    PTy::IntArr,
    PTy::Obj,
    PTy::BaseTy,
    PTy::DerivedTy,
];

/// One argument shape a call site is generated at: the F# argument text (already
/// parenthesised as an argument list) and a human tag for failure messages.
///
/// The single-argument shapes cover every **ground literal** our inference types
/// (`infer.rs::literal_ty`) plus the four **factory values** bound in the F#
/// prelude (`o`/`b`/`d`/`xs` — an `obj`, a `BaseTy`, a `DerivedTy` and an
/// `int[]`, none of which has literal syntax). The unit and multi-argument
/// shapes exercise the arity window (FCS's direct-unit-syntax reading, the
/// `params` expansion, and the optional/`out` trimming).
pub struct ArgShape {
    /// The F# argument list, parenthesised (`(3, "x")`).
    pub text: &'static str,
    /// A human tag for failure messages and the coverage histogram.
    pub tag: &'static str,
    /// The canonical type of each argument — what our inference *will* infer for
    /// `text`, so the matcher's input in a test can be derived from the same
    /// table that emitted the source and cannot drift from it. (The differential
    /// checks this: our published types for these very expressions are compared
    /// against FCS's at their exact ranges.)
    pub tys: &'static [&'static str],
}

const ARGS: [ArgShape; 19] = [
    ArgShape {
        text: "()",
        tag: "unit",
        tys: &[],
    },
    ArgShape {
        text: "(3)",
        tag: "int",
        tys: &["System.Int32"],
    },
    ArgShape {
        text: "(3L)",
        tag: "int64",
        tys: &["System.Int64"],
    },
    ArgShape {
        text: "(3.0)",
        tag: "float",
        tys: &["System.Double"],
    },
    ArgShape {
        text: "(3.0f)",
        tag: "single",
        tys: &["System.Single"],
    },
    ArgShape {
        text: "(3.0m)",
        tag: "decimal",
        tys: &["System.Decimal"],
    },
    ArgShape {
        text: "(3uy)",
        tag: "byte",
        tys: &["System.Byte"],
    },
    ArgShape {
        text: "(\"x\")",
        tag: "string",
        tys: &["System.String"],
    },
    ArgShape {
        text: "('c')",
        tag: "char",
        tys: &["System.Char"],
    },
    ArgShape {
        text: "(true)",
        tag: "bool",
        tys: &["System.Boolean"],
    },
    ArgShape {
        text: "(\"x\"B)",
        tag: "byte[]",
        tys: &["System.Byte[]"],
    },
    ArgShape {
        text: "(o)",
        tag: "obj",
        tys: &["System.Object"],
    },
    ArgShape {
        text: "(b)",
        tag: "BaseTy",
        tys: &["OvCorpus.BaseTy"],
    },
    ArgShape {
        text: "(d)",
        tag: "DerivedTy",
        tys: &["OvCorpus.DerivedTy"],
    },
    ArgShape {
        text: "(xs)",
        tag: "int[]",
        tys: &["System.Int32[]"],
    },
    ArgShape {
        text: "(3, 3)",
        tag: "int,int",
        tys: &["System.Int32", "System.Int32"],
    },
    ArgShape {
        text: "(3, \"x\")",
        tag: "int,string",
        tys: &["System.Int32", "System.String"],
    },
    ArgShape {
        text: "(\"x\", 3)",
        tag: "string,int",
        tys: &["System.String", "System.Int32"],
    },
    ArgShape {
        text: "(3, 3, 3)",
        tag: "int,int,int",
        tys: &["System.Int32", "System.Int32", "System.Int32"],
    },
];

/// One generated call site: the line it sits on (1-based, into
/// [`Corpus::fsharp`]), the method name it calls (`M` for the static form, `I`
/// for the instance form — the differential filters FCS's per-line records by
/// name, since an inserted `op_Implicit` conversion node shares the line), and
/// the source line itself for failure messages.
#[derive(Debug, Clone)]
pub struct Site {
    pub line: usize,
    pub method: &'static str,
    pub is_static: bool,
    /// The C# type declaring the group (`OvCorpus.Ov_Int_String`).
    pub declaring: String,
    /// The argument-shape tag.
    pub arg_tag: &'static str,
    /// The canonical type of each argument (see [`ArgShape::tys`]).
    pub arg_types: &'static [&'static str],
    /// The generated source line, verbatim — a failing case *is* this line.
    pub text: String,
}

/// The generated universe: the C# assembly source, the F# call-site matrix, and
/// the per-line site index.
pub struct Corpus {
    pub csharp: String,
    pub fsharp: String,
    pub sites: Vec<Site>,
}

/// One declared overload-set type: its name, its base type (for the shapes whose
/// group is split across an inheritance level), and its C# body (the `M` statics
/// and `I` instance methods).
struct OvType {
    name: String,
    base: Option<&'static str>,
    body: String,
}

/// Build the whole universe. Deterministic: no randomness, no seeds — the matrix
/// is *exhaustive* over its axes, which subsumes a fixed seed corpus and needs
/// no shrinking (each case is one line).
pub fn corpus() -> Corpus {
    let types = declared_types();

    // ── C# ───────────────────────────────────────────────────────────────────
    let mut cs = String::new();
    cs.push_str(
        "// GENERATED by crates/sema/tests/all/common/overload_corpus.rs — do not edit.\n\
         // The OV-9 overload matrix (docs/overload-resolution-plan.md §6).\n\
         namespace OvCorpus\n{\n\
         \x20   public class BaseTy { }\n\
         \x20   public class DerivedTy : BaseTy { }\n\n",
    );
    // The factory class: every ground value the F# side cannot write as a
    // literal, plus one `New_<T>` per corpus type (each a single-candidate static
    // our engine commits, so the receiver binder grounds).
    cs.push_str("    public static class Make\n    {\n");
    cs.push_str("        public static object Obj() => new object();\n");
    cs.push_str("        public static BaseTy Base() => new BaseTy();\n");
    cs.push_str("        public static DerivedTy Derived() => new DerivedTy();\n");
    cs.push_str("        public static int[] Ints() => new int[] { 1, 2 };\n");
    for t in &types {
        cs.push_str(&format!(
            "        public static {n} New_{n}() => new {n}();\n",
            n = t.name
        ));
    }
    cs.push_str("    }\n\n");
    for t in &types {
        let header = match t.base {
            Some(b) => format!("    public class {} : {b}\n    {{\n", t.name),
            None => format!("    public class {}\n    {{\n", t.name),
        };
        cs.push_str(&header);
        cs.push_str(&t.body);
        cs.push_str("    }\n\n");
    }
    cs.push_str("}\n");

    // ── F# ───────────────────────────────────────────────────────────────────
    //
    // No `open`, no attributes: OV-6's extension-absence gate treats either as an
    // extension source and would defer every call, making the whole differential
    // vacuous. Everything is reached fully qualified.
    let mut fs = String::new();
    let mut line = 0usize;
    let push = |fs: &mut String, line: &mut usize, text: &str| -> usize {
        fs.push_str(text);
        fs.push('\n');
        *line += 1;
        *line
    };
    push(&mut fs, &mut line, "module Gen");
    push(&mut fs, &mut line, "let o = OvCorpus.Make.Obj()");
    push(&mut fs, &mut line, "let b = OvCorpus.Make.Base()");
    push(&mut fs, &mut line, "let d = OvCorpus.Make.Derived()");
    push(&mut fs, &mut line, "let xs = OvCorpus.Make.Ints()");

    let mut sites = Vec::new();
    for t in &types {
        // The instance receiver for this type, from its static factory.
        let recv = format!("c_{}", t.name);
        push(
            &mut fs,
            &mut line,
            &format!("let {recv} = OvCorpus.Make.New_{}()", t.name),
        );
        for arg in &ARGS {
            // Static form: `OvCorpus.T.M(args)`.
            let text = format!("let r{} = OvCorpus.{}.M{}", line + 1, t.name, arg.text);
            let l = push(&mut fs, &mut line, &text);
            sites.push(Site {
                line: l,
                method: "M",
                is_static: true,
                declaring: format!("OvCorpus.{}", t.name),
                arg_tag: arg.tag,
                arg_types: arg.tys,
                text,
            });

            // Instance form: `c.I(args)`.
            let text = format!("let r{} = {recv}.I{}", line + 1, arg.text);
            let l = push(&mut fs, &mut line, &text);
            sites.push(Site {
                line: l,
                method: "I",
                is_static: false,
                declaring: format!("OvCorpus.{}", t.name),
                arg_tag: arg.tag,
                arg_types: arg.tys,
                text,
            });
        }
    }

    Corpus {
        csharp: cs,
        fsharp: fs,
        sites,
    }
}

/// A two-candidate overload set `M(p)` / `M(q)` (and its `I` instance twin),
/// returning `int` and `string` respectively so the two are distinguishable by
/// *return type* as well as by signature.
fn pair_type(p: PTy, q: PTy) -> OvType {
    let name = format!("Ov_{}_{}", p.tag(), q.tag());
    let body = format!(
        "        public static int M({p} x) => 1;\n\
         \x20       public static string M({q} x) => \"s\";\n\
         \x20       public int I({p} x) => 1;\n\
         \x20       public string I({q} x) => \"s\";\n",
        p = p.csharp(),
        q = q.csharp()
    );
    OvType {
        name,
        base: None,
        body,
    }
}

/// A hand-written shape: `name` + the body of its `M`/`I` declarations.
fn shape(name: &str, body: &str) -> OvType {
    OvType {
        name: name.to_owned(),
        base: None,
        body: body.to_owned(),
    }
}

/// A hand-written shape declared on top of `base_name` — its group is split
/// across an inheritance level.
fn derived_shape(name: &str, base_name: &'static str, body: &str) -> OvType {
    OvType {
        name: name.to_owned(),
        base: Some(base_name),
        body: body.to_owned(),
    }
}

/// Every declared type: the 55 pair sets, plus the decorated shapes the plan
/// names as landmines (§2.2, §4.2, §5).
fn declared_types() -> Vec<OvType> {
    let mut types = Vec::new();
    for (i, &p) in PARAM_TYPES.iter().enumerate() {
        for &q in &PARAM_TYPES[i + 1..] {
            types.push(pair_type(p, q));
        }
    }

    // Single candidate (FCS's arity-only shortcut, §2.2).
    types.push(shape(
        "OvSingle",
        "        public static int M(int x) => 1;\n\
         \x20       public int I(int x) => 1;\n",
    ));
    // Single candidate, zero-arity (the direct-unit-syntax reading).
    types.push(shape(
        "OvNullary",
        "        public static int M() => 1;\n\
         \x20       public int I() => 1;\n",
    ));
    // Arity-split overloads (the arity prong refutes one; P4's `Substring` shape).
    types.push(shape(
        "OvArity",
        "        public static int M(int x) => 1;\n\
         \x20       public static string M(int x, int y) => \"s\";\n\
         \x20       public int I(int x) => 1;\n\
         \x20       public string I(int x, int y) => \"s\";\n",
    ));
    // A trailing optional makes the 2-param candidate applicable at arity 1 (P3):
    // the winner needs an *omitted* optional, which `must_apply` refuses ⇒ defer.
    types.push(shape(
        "OvOptional",
        "        public static int M(int x, int y = 0) => 1;\n\
         \x20       public static string M(string s) => \"s\";\n\
         \x20       public int I(int x, int y = 0) => 1;\n\
         \x20       public string I(string s) => \"s\";\n",
    ));
    // A `params` array is applicable at any trailing arity (P2) — and a single
    // declared params method is *two* normalised FCS candidates.
    types.push(shape(
        "OvParams",
        "        public static int M(params int[] xs) => 1;\n\
         \x20       public static string M(string s) => \"s\";\n\
         \x20       public int I(params int[] xs) => 1;\n\
         \x20       public string I(string s) => \"s\";\n",
    ));
    types.push(shape(
        "OvParamsOnly",
        "        public static int M(params int[] xs) => 1;\n\
         \x20       public int I(params int[] xs) => 1;\n",
    ));
    // An `out` parameter: FCS folds an omitted `out` into a tuple return, which
    // v1 does not model ⇒ every arity must defer (§5).
    types.push(shape(
        "OvOut",
        "        public static int M(int x, out int y) { y = 0; return 1; }\n\
         \x20       public static string M(string s) => \"s\";\n\
         \x20       public int I(int x, out int y) { y = 0; return 1; }\n\
         \x20       public string I(string s) => \"s\";\n",
    ));
    // A generic candidate competes fully (P11): it is un-refutable at matching
    // arity, so the group defers whenever the arity fits.
    types.push(shape(
        "OvGeneric",
        "        public static int M<T>(T x) => 1;\n\
         \x20       public static string M(string s) => \"s\";\n\
         \x20       public int I<T>(T x) => 1;\n\
         \x20       public string I(string s) => \"s\";\n",
    ));
    // Three candidates: the type prong must refute *two* to commit.
    types.push(shape(
        "OvTriple",
        "        public static int M(int x) => 1;\n\
         \x20       public static string M(string s) => \"s\";\n\
         \x20       public static bool M(char c) => true;\n\
         \x20       public int I(int x) => 1;\n\
         \x20       public string I(string s) => \"s\";\n\
         \x20       public bool I(char c) => true;\n",
    ));

    // A group **split across an inheritance level** (§2.1: F# hides by parameter
    // signature, not by name — `Der.M(int)` does *not* hide `Base.M(string)`, so
    // both candidates stay in the group). Only a base-chain walk sees the whole
    // group; a derived-only scan would wrongly commit the derived candidate.
    types.push(shape(
        "OvSplitBase",
        "        public static int M(string s) => 1;\n\
         \x20       public int I(string s) => 1;\n",
    ));
    types.push(derived_shape(
        "OvSplitDer",
        "OvSplitBase",
        "        public static string M(int x) => \"s\";\n\
         \x20       public string I(int x) => \"s\";\n",
    ));

    // An **override** (OV-3's partial-signature dedup: the derived override and
    // the base virtual collapse to one candidate, so the call still commits).
    // `OvVirtDer` declares no static `M` at all, so its static call sites also
    // exercise the *inherited static* path (OV-7).
    types.push(shape(
        "OvVirtBase",
        "        public virtual int I(int x) => 1;\n\
         \x20       public static int M(int x) => 1;\n",
    ));
    types.push(derived_shape(
        "OvVirtDer",
        "OvVirtBase",
        "        public override int I(int x) => 2;\n",
    ));

    types
}
