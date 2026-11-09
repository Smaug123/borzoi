//! Generative source-level differential test for the assembly reader.
//!
//! The per-construct diff tests (`assembly_diff.rs`) pin hand-picked
//! shapes; this harness *searches* the shape space instead. Each case:
//!
//! 1. generates a random list of F# declarations (a [`Decl`] AST that is
//!    valid-by-construction — the strategy cannot produce source that
//!    fails to compile);
//! 2. pretty-prints it to an F# source file and compiles it with the
//!    real `fsc` (`dotnet build` of a temp project under
//!    `CARGO_TARGET_TMPDIR`);
//! 3. projects the resulting DLL through our `Ecma335Assembly` reader
//!    (including the F# signature-pickle overlay) and through FCS
//!    (`fcs-dump entities`);
//! 4. asserts the two [`NormalisedAssembly`] projections are equal.
//!
//! Because the compiler is in the loop, this differentially exercises the
//! whole pipeline — ECMA-335 tables, signature blobs, the
//! `CompilationMapping` kind decoder, the F# pickle decode and its merge
//! overlays — against fresh combinations no fixture pins.
//!
//! A compile *failure* is a test failure too: it means the generator
//! escaped its valid-by-construction envelope, and the failure message
//! carries the offending source.
//!
//! Cost model: each case is one `dotnet build` (~1-2 s warm) plus one
//! `fcs-dump` run, so the default case count is deliberately small.
//! Override with `BORZOI_GENERATIVE_CASES` for a deep local sweep:
//!
//! ```text
//! BORZOI_GENERATIVE_CASES=50 cargo test -p borzoi-assembly \
//!     --test all generative_source_diff::
//! ```
//!
//! On failure, proptest shrinks the declaration list to a minimal
//! divergent program and the panic message includes the full F# source —
//! paste it into a fixture to turn any find into a pinned regression.

use std::fmt::Write as _;

use proptest::prelude::*;

use crate::common::{ArgGroupObligation, Lang, compile_generated, diff_dll};

// ============================================================================
// The declaration AST.
//
// Valid-by-construction: every reachable value renders to F# source that
// compiles. Names are assigned positionally at render time (types `T0…`,
// modules `M0…`, fields `F0…`, DU cases `C0…`), so name collisions are
// unrepresentable rather than filtered.
// ============================================================================

/// A type usable in a signature position (field, parameter, return).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ty {
    Int,
    Int64,
    String,
    Bool,
    Float,
    Char,
    Decimal,
    Option(Box<Ty>),
    List(Box<Ty>),
    Array(Box<Ty>),
    /// A two-element tuple. Fixed arity keeps the strategy simple; wider
    /// tuples add no new projection machinery (both sides render
    /// `System.Tuple`/`ValueTuple`-free F# tuples structurally).
    Pair(Box<Ty>, Box<Ty>),
    /// An anonymous record, `{| A0: t0; … |}` or its struct form. Field names
    /// are positional, so two of one record never collide.
    Anon {
        fields: Vec<Ty>,
        is_struct: bool,
    },
}

/// One `let` binding inside a generated module.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Binding {
    /// `let v<i> : ty = Unchecked.defaultof<ty>` — a module-level value,
    /// compiled to a static property.
    Value { ty: Ty },
    /// `let mutable m<i> : ty = …` — compiled to a static getter+setter
    /// property; FCS surfaces the getter's shape.
    MutableValue { ty: Ty },
    /// `let [private] f<i> (p0: t0) (p1: t1) … : ret = …` — a curried
    /// module function; IL flattens the currying into one method with N
    /// parameters. `params` may be empty: `let f<i> () : ret = …`, which
    /// exercises the synthetic-unit-parameter strip on the FCS side.
    /// `is_private` exercises the accessibility filter both normalisers
    /// apply (`accessible_from_some_fsharp_code`): a private binding must
    /// vanish from *both* projections.
    Function {
        params: Vec<Ty>,
        ret: Ty,
        is_private: bool,
    },
    /// `[<Literal>] let L<i> = <int literal>` — a static literal field on
    /// the module class, which both projectors surface.
    IntLiteral { value: i32 },
}

/// A `[<CompilationRepresentation>]` flag on an instance member, which
/// decides whether fsc compiles it as an IL instance method or as a static
/// taking the receiver first. F# declares the member instance either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompiledAs {
    /// No attribute: instance in IL, except on a null-as-true union
    /// ([`Decl::Union::null_as_true`]), which compiles it static.
    Default,
    /// `Static`: a static taking the receiver, on any type.
    Static,
    /// `Instance`: instance in IL, even on a null-as-true union.
    Instance,
}

/// One member of a generated class or union.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ClassMember {
    /// `member _.M<i> (q0: t0, q1: t1) : ret = …` (tupled — the idiomatic
    /// F# member shape, one IL parameter per element). Empty `params`
    /// renders as `member _.M<i> () : ret = …`. `is_private` exercises
    /// the accessibility filter on both sides of the diff. `compiled` is
    /// [`CompiledAs::Default`] on a static method.
    Method {
        params: Vec<Ty>,
        ret: Ty,
        is_static: bool,
        is_private: bool,
        compiled: CompiledAs,
    },
    /// `member _.Q<i> : ty = …` (or `static member Q<i>`) — a getter-only
    /// property. `compiled` as on [`ClassMember::Method`].
    Property {
        ty: Ty,
        is_static: bool,
        compiled: CompiledAs,
    },
    /// `member val [private] P<i> : ty = Unchecked.defaultof<ty> with
    /// get, set`. `is_private` as on [`ClassMember::Method`]. Classes only:
    /// a union cannot declare one.
    AutoProperty { ty: Ty, is_private: bool },
}

/// One top-level declaration in the generated namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Decl {
    /// `type T<i> = { F0: t0; mutable F1: t1; … }`, optionally `[<Struct>]`.
    Record {
        /// `(field type, is mutable)`; non-empty by construction.
        fields: Vec<(Ty, bool)>,
        is_struct: bool,
    },
    /// `type T<i> = | C0 | C1 of c1f0: t0 * c1f1: t1 | …`. Payload fields
    /// are named uniquely per case (`c<ci>f<fi>`), which keeps the
    /// `[<Struct>]` variant legal (struct DUs require case-field names to
    /// be unique across cases).
    Union {
        /// One payload-type list per case; non-empty by construction.
        cases: Vec<Vec<Ty>>,
        is_struct: bool,
        require_qualified_access: bool,
        /// `[<CompilationRepresentation(UseNullAsTrueValue)>]`. Only set on
        /// the shapes fsc accepts it on (FS1196): a reference union with
        /// exactly one nullary case and at least one carrying case.
        null_as_true: bool,
        /// Methods and getter properties; never [`ClassMember::AutoProperty`].
        members: Vec<ClassMember>,
    },
    /// `type T<i> = | E0 = 0 | E1 = 1 | …` — an int enum.
    Enum { case_count: usize },
    /// `exception T<i> of t0 * t1 * …` (or a payload-less `exception T<i>`).
    Exception { fields: Vec<Ty> },
    /// `type T<i>(…) = <members>` — a plain class with a primary
    /// constructor.
    Class {
        ctor_params: Vec<Ty>,
        members: Vec<ClassMember>,
    },
    /// `type T<i> = abstract member M0: t0 * t1 -> ret; …` — an
    /// interface. Empty `methods` renders `interface end`.
    Interface { methods: Vec<(Vec<Ty>, Ty)> },
    /// `module M<i> = <bindings>`; non-empty by construction (an empty
    /// module is legal F# but adds nothing).
    Module {
        bindings: Vec<Binding>,
        require_qualified_access: bool,
    },
    /// `[<Measure>] type T<i>` — a unit-of-measure marker type,
    /// recovered from the F# signature pickle (kind `Measure`).
    Measure,
}

// ============================================================================
// Rendering to F# source.
// ============================================================================

/// Render `ty` in F# postfix syntax. Tuples are always parenthesised so
/// they nest safely inside postfix constructors (`(int * string) list`).
fn render_ty(ty: &Ty) -> String {
    match ty {
        Ty::Int => "int".into(),
        Ty::Int64 => "int64".into(),
        Ty::String => "string".into(),
        Ty::Bool => "bool".into(),
        Ty::Float => "float".into(),
        Ty::Char => "char".into(),
        Ty::Decimal => "decimal".into(),
        Ty::Option(t) => format!("{} option", render_ty(t)),
        Ty::List(t) => format!("{} list", render_ty(t)),
        Ty::Array(t) => format!("{}[]", render_ty(t)),
        Ty::Pair(a, b) => format!("({} * {})", render_ty(a), render_ty(b)),
        // Parenthesised, like a tuple, so `struct` binds to the record alone.
        Ty::Anon { fields, is_struct } => {
            let fields = fields
                .iter()
                .enumerate()
                .map(|(i, t)| format!("A{i}: {}", render_ty(t)))
                .collect::<Vec<_>>()
                .join("; ");
            let prefix = if *is_struct { "struct " } else { "" };
            format!("({prefix}{{| {fields} |}})")
        }
    }
}

/// A total F# expression of type `ty`. `Unchecked.defaultof` works for
/// every generated type and keeps bodies trivially well-typed.
fn default_expr(ty: &Ty) -> String {
    format!("Unchecked.defaultof<{}>", render_ty(ty))
}

fn render_binding(out: &mut String, idx: usize, b: &Binding) {
    match b {
        Binding::Value { ty } => {
            let _ = writeln!(
                out,
                "    let v{idx} : {} = {}",
                render_ty(ty),
                default_expr(ty)
            );
        }
        Binding::MutableValue { ty } => {
            let _ = writeln!(
                out,
                "    let mutable m{idx} : {} = {}",
                render_ty(ty),
                default_expr(ty)
            );
        }
        Binding::Function {
            params,
            ret,
            is_private,
        } => {
            let access = if *is_private { "private " } else { "" };
            let params_src = if params.is_empty() {
                " ()".to_string()
            } else {
                params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| format!(" (p{i}: {})", render_ty(p)))
                    .collect::<String>()
            };
            let _ = writeln!(
                out,
                "    let {access}f{idx}{params_src} : {} = {}",
                render_ty(ret),
                default_expr(ret)
            );
        }
        Binding::IntLiteral { value } => {
            let _ = writeln!(out, "    [<Literal>]");
            let _ = writeln!(out, "    let L{idx} = {value}");
        }
    }
}

fn render_compiled_as(out: &mut String, compiled: CompiledAs) {
    let flag = match compiled {
        CompiledAs::Default => return,
        CompiledAs::Static => "Static",
        CompiledAs::Instance => "Instance",
    };
    let _ = writeln!(
        out,
        "    [<CompilationRepresentation(CompilationRepresentationFlags.{flag})>]"
    );
}

fn render_class_member(out: &mut String, idx: usize, m: &ClassMember) {
    match m {
        ClassMember::Method {
            params,
            ret,
            is_static,
            is_private,
            compiled,
        } => {
            render_compiled_as(out, *compiled);
            let access = if *is_private { "private " } else { "" };
            let receiver = if *is_static {
                format!("static member {access}M")
            } else {
                format!("member {access}_.M")
            };
            let params_src = if params.is_empty() {
                "()".to_string()
            } else {
                let inner = params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| format!("q{i}: {}", render_ty(p)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({inner})")
            };
            let _ = writeln!(
                out,
                "    {receiver}{idx} {params_src} : {} = {}",
                render_ty(ret),
                default_expr(ret)
            );
        }
        ClassMember::Property {
            ty,
            is_static,
            compiled,
        } => {
            render_compiled_as(out, *compiled);
            let receiver = if *is_static {
                "static member "
            } else {
                "member _."
            };
            let _ = writeln!(
                out,
                "    {receiver}Q{idx} : {} = {}",
                render_ty(ty),
                default_expr(ty)
            );
        }
        ClassMember::AutoProperty { ty, is_private } => {
            let access = if *is_private { "private " } else { "" };
            let _ = writeln!(
                out,
                "    member val {access}P{idx} : {} = {} with get, set",
                render_ty(ty),
                default_expr(ty)
            );
        }
    }
}

fn render_decl(out: &mut String, idx: usize, d: &Decl) {
    match d {
        Decl::Record { fields, is_struct } => {
            if *is_struct {
                let _ = writeln!(out, "[<Struct>]");
            }
            let fields_src = fields
                .iter()
                .enumerate()
                .map(|(i, (ty, mutable))| {
                    let m = if *mutable { "mutable " } else { "" };
                    format!("{m}F{i}: {}", render_ty(ty))
                })
                .collect::<Vec<_>>()
                .join("; ");
            let _ = writeln!(out, "type T{idx} = {{ {fields_src} }}");
        }
        Decl::Union {
            cases,
            is_struct,
            require_qualified_access,
            null_as_true,
            members,
        } => {
            if *is_struct {
                let _ = writeln!(out, "[<Struct>]");
            }
            if *require_qualified_access {
                let _ = writeln!(out, "[<RequireQualifiedAccess>]");
            }
            if *null_as_true {
                assert!(
                    !is_struct
                        && cases.iter().filter(|c| c.is_empty()).count() == 1
                        && cases.iter().any(|c| !c.is_empty()),
                    "the strategy only sets `null_as_true` on a shape fsc accepts: {cases:?}"
                );
                let _ = writeln!(
                    out,
                    "[<CompilationRepresentation(CompilationRepresentationFlags.UseNullAsTrueValue)>]"
                );
            }
            let _ = writeln!(out, "type T{idx} =");
            for (ci, case) in cases.iter().enumerate() {
                if case.is_empty() {
                    let _ = writeln!(out, "    | C{ci}");
                } else {
                    let payload = case
                        .iter()
                        .enumerate()
                        .map(|(fi, ty)| format!("c{ci}f{fi}: {}", render_ty(ty)))
                        .collect::<Vec<_>>()
                        .join(" * ");
                    let _ = writeln!(out, "    | C{ci} of {payload}");
                }
            }
            for (mi, m) in members.iter().enumerate() {
                assert!(
                    !matches!(m, ClassMember::AutoProperty { .. }),
                    "a union cannot declare an auto-property"
                );
                render_class_member(out, mi, m);
            }
        }
        Decl::Enum { case_count } => {
            let _ = writeln!(out, "type T{idx} =");
            for ci in 0..*case_count {
                let _ = writeln!(out, "    | E{ci} = {ci}");
            }
        }
        Decl::Exception { fields } => {
            if fields.is_empty() {
                let _ = writeln!(out, "exception T{idx}");
            } else {
                let payload = fields.iter().map(render_ty).collect::<Vec<_>>().join(" * ");
                let _ = writeln!(out, "exception T{idx} of {payload}");
            }
        }
        Decl::Class {
            ctor_params,
            members,
        } => {
            let ctor_src = ctor_params
                .iter()
                .enumerate()
                .map(|(i, p)| format!("p{i}: {}", render_ty(p)))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(out, "type T{idx}({ctor_src}) =");
            if members.is_empty() {
                let _ = writeln!(out, "    class end");
            } else {
                for (mi, m) in members.iter().enumerate() {
                    render_class_member(out, mi, m);
                }
            }
        }
        Decl::Interface { methods } => {
            let _ = writeln!(out, "type T{idx} =");
            if methods.is_empty() {
                let _ = writeln!(out, "    interface end");
            } else {
                for (mi, (params, ret)) in methods.iter().enumerate() {
                    let params_src = if params.is_empty() {
                        "unit".to_string()
                    } else {
                        params.iter().map(render_ty).collect::<Vec<_>>().join(" * ")
                    };
                    let _ = writeln!(
                        out,
                        "    abstract member M{mi}: {params_src} -> {}",
                        render_ty(ret)
                    );
                }
            }
        }
        Decl::Module {
            bindings,
            require_qualified_access,
        } => {
            if *require_qualified_access {
                let _ = writeln!(out, "[<RequireQualifiedAccess>]");
            }
            let _ = writeln!(out, "module M{idx} =");
            for (bi, b) in bindings.iter().enumerate() {
                render_binding(out, bi, b);
            }
        }
        Decl::Measure => {
            let _ = writeln!(out, "[<Measure>] type T{idx}");
        }
    }
}

/// Render the whole program: one namespace, declarations in order.
fn render_source(decls: &[Decl]) -> String {
    assert!(
        !decls.is_empty(),
        "strategy guarantees at least one declaration"
    );
    let mut out = String::from(
        "// Generated by crates/assembly/tests/all/generative_source_diff.rs — do not edit.\n\
         namespace Generated\n",
    );
    for (i, d) in decls.iter().enumerate() {
        out.push('\n');
        render_decl(&mut out, i, d);
    }
    out
}

// ============================================================================
// Compile + diff plumbing.
// ============================================================================

/// Compile and diff through [`diff_dll`] — or a message describing a stage
/// failure (generator escape, reader refusal, divergence), which the caller
/// turns into a test failure carrying the source.
fn diff_generated(decls: &[Decl]) -> Result<(), String> {
    let source = render_source(decls);
    let fail = |stage: &str, detail: String| {
        format!("{stage}: {detail}\n--- generated source ---\n{source}")
    };
    let dll_path = compile_generated(Lang::FSharp, &source).map_err(|e| fail("compile", e))?;
    diff_dll(&dll_path, ArgGroupObligation::ModulesCommit)
        .map(|_agreement| ())
        .map_err(|e| fail("projection diff", e))
}

// ============================================================================
// Deterministic smoke test: one program touching every AST variant.
//
// This is the always-on floor — it runs even if someone dials the
// property's case count to zero, and its fixed shape makes a failure here
// unambiguous ("a construct in the core vocabulary regressed") rather
// than "the search happened to find something".
// ============================================================================

#[test]
fn smoke_full_vocabulary_agrees() {
    let decls = vec![
        Decl::Record {
            fields: vec![
                (Ty::Int, false),
                (Ty::Option(Box::new(Ty::String)), true),
                (Ty::Pair(Box::new(Ty::Float), Box::new(Ty::Bool)), false),
                (
                    Ty::Anon {
                        fields: vec![Ty::Int, Ty::List(Box::new(Ty::String))],
                        is_struct: false,
                    },
                    false,
                ),
            ],
            is_struct: false,
        },
        Decl::Record {
            fields: vec![(Ty::Decimal, false)],
            is_struct: true,
        },
        Decl::Union {
            cases: vec![
                vec![],
                vec![Ty::Int, Ty::String],
                vec![Ty::List(Box::new(Ty::Char))],
            ],
            is_struct: false,
            require_qualified_access: true,
            null_as_true: false,
            members: vec![],
        },
        // A null-as-true union: fsc compiles its default instance members as
        // statics taking the receiver, and keeps an `Instance`-flagged one
        // instance.
        Decl::Union {
            cases: vec![vec![Ty::Int], vec![]],
            is_struct: false,
            require_qualified_access: false,
            null_as_true: true,
            members: vec![
                ClassMember::Method {
                    params: vec![Ty::String, Ty::Int],
                    ret: Ty::Bool,
                    is_static: false,
                    is_private: false,
                    compiled: CompiledAs::Default,
                },
                ClassMember::Method {
                    params: vec![],
                    ret: Ty::Int,
                    is_static: false,
                    is_private: false,
                    compiled: CompiledAs::Default,
                },
                ClassMember::Property {
                    ty: Ty::Float,
                    is_static: false,
                    compiled: CompiledAs::Default,
                },
                ClassMember::Property {
                    ty: Ty::Int,
                    is_static: false,
                    compiled: CompiledAs::Instance,
                },
                ClassMember::Method {
                    params: vec![Ty::Int],
                    ret: Ty::Int,
                    is_static: true,
                    is_private: false,
                    compiled: CompiledAs::Default,
                },
            ],
        },
        // A struct union whose `Static`-flagged method takes its receiver by
        // reference.
        Decl::Union {
            cases: vec![vec![Ty::Int]],
            is_struct: true,
            require_qualified_access: false,
            null_as_true: false,
            members: vec![ClassMember::Method {
                params: vec![Ty::Bool],
                ret: Ty::Int,
                is_static: false,
                is_private: false,
                compiled: CompiledAs::Static,
            }],
        },
        Decl::Enum { case_count: 3 },
        Decl::Exception {
            fields: vec![Ty::Int64, Ty::String],
        },
        Decl::Class {
            ctor_params: vec![Ty::Int, Ty::String],
            members: vec![
                ClassMember::Method {
                    params: vec![Ty::Bool, Ty::Array(Box::new(Ty::Int))],
                    ret: Ty::String,
                    is_static: false,
                    is_private: false,
                    compiled: CompiledAs::Default,
                },
                ClassMember::Method {
                    params: vec![],
                    ret: Ty::Int,
                    is_static: true,
                    is_private: false,
                    compiled: CompiledAs::Default,
                },
                ClassMember::Method {
                    params: vec![Ty::Int],
                    ret: Ty::Bool,
                    is_static: false,
                    is_private: true,
                    compiled: CompiledAs::Default,
                },
                ClassMember::Method {
                    params: vec![Ty::Int],
                    ret: Ty::Int,
                    is_static: false,
                    is_private: false,
                    compiled: CompiledAs::Static,
                },
                ClassMember::Property {
                    ty: Ty::String,
                    is_static: false,
                    compiled: CompiledAs::Static,
                },
                ClassMember::AutoProperty {
                    ty: Ty::Float,
                    is_private: false,
                },
                ClassMember::AutoProperty {
                    ty: Ty::String,
                    is_private: true,
                },
            ],
        },
        Decl::Interface {
            methods: vec![
                (vec![Ty::Int, Ty::String], Ty::Bool),
                (vec![], Ty::Option(Box::new(Ty::Int))),
            ],
        },
        Decl::Module {
            bindings: vec![
                Binding::Value { ty: Ty::Int },
                Binding::Value {
                    ty: Ty::Option(Box::new(Ty::Anon {
                        fields: vec![Ty::Bool],
                        is_struct: true,
                    })),
                },
                Binding::MutableValue { ty: Ty::String },
                Binding::Function {
                    params: vec![Ty::Int, Ty::Bool],
                    ret: Ty::Float,
                    is_private: false,
                },
                Binding::Function {
                    params: vec![],
                    ret: Ty::Int,
                    is_private: false,
                },
                Binding::Function {
                    params: vec![Ty::String],
                    ret: Ty::String,
                    is_private: true,
                },
                Binding::IntLiteral { value: 42 },
            ],
            require_qualified_access: false,
        },
        Decl::Measure,
    ];
    if let Err(msg) = diff_generated(&decls) {
        panic!("{msg}");
    }
}

// ============================================================================
// The property.
// ============================================================================

fn ty_strategy() -> impl Strategy<Value = Ty> {
    let leaf = prop_oneof![
        Just(Ty::Int),
        Just(Ty::Int64),
        Just(Ty::String),
        Just(Ty::Bool),
        Just(Ty::Float),
        Just(Ty::Char),
        Just(Ty::Decimal),
    ];
    // Depth 2 / ~6 nodes: enough to reach e.g. `(int * string) list option`
    // without generating signature towers that stress nothing new.
    leaf.prop_recursive(2, 6, 2, |inner| {
        prop_oneof![
            inner.clone().prop_map(|t| Ty::Option(Box::new(t))),
            inner.clone().prop_map(|t| Ty::List(Box::new(t))),
            inner.clone().prop_map(|t| Ty::Array(Box::new(t))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Ty::Pair(Box::new(a), Box::new(b))),
            (prop::collection::vec(inner, 1..3), any::<bool>())
                .prop_map(|(fields, is_struct)| Ty::Anon { fields, is_struct }),
        ]
    })
}

fn binding_strategy() -> impl Strategy<Value = Binding> {
    prop_oneof![
        ty_strategy().prop_map(|ty| Binding::Value { ty }),
        ty_strategy().prop_map(|ty| Binding::MutableValue { ty }),
        (
            prop::collection::vec(ty_strategy(), 0..3),
            ty_strategy(),
            any::<bool>()
        )
            .prop_map(|(params, ret, is_private)| Binding::Function {
                params,
                ret,
                is_private,
            }),
        any::<i32>().prop_map(|value| Binding::IntLiteral { value }),
    ]
}

/// A member's `[<CompilationRepresentation>]` flag: any of the three on an
/// instance member, none on a static one (an `Instance`-flagged static member
/// compiles to an instance method that has lost its first argument, which the
/// projection declines rather than surfaces).
fn compiled_as_strategy(is_static: bool) -> BoxedStrategy<CompiledAs> {
    if is_static {
        Just(CompiledAs::Default).boxed()
    } else {
        prop_oneof![
            Just(CompiledAs::Default),
            Just(CompiledAs::Static),
            Just(CompiledAs::Instance),
        ]
        .boxed()
    }
}

/// A method or getter property: what both a class and a union can declare.
fn method_or_property_strategy() -> impl Strategy<Value = ClassMember> {
    prop_oneof![
        (
            prop::collection::vec(ty_strategy(), 0..3),
            ty_strategy(),
            any::<bool>(),
            any::<bool>()
        )
            .prop_flat_map(|(params, ret, is_static, is_private)| {
                compiled_as_strategy(is_static).prop_map(move |compiled| ClassMember::Method {
                    params: params.clone(),
                    ret: ret.clone(),
                    is_static,
                    is_private,
                    compiled,
                })
            }),
        (ty_strategy(), any::<bool>()).prop_flat_map(|(ty, is_static)| {
            compiled_as_strategy(is_static).prop_map(move |compiled| ClassMember::Property {
                ty: ty.clone(),
                is_static,
                compiled,
            })
        }),
    ]
}

/// A struct's `Static`-flagged instance property compiles to a static whose
/// receiver is a byref *index parameter*, which the projection refuses
/// (recording the member as skipped) while FCS shows the property: a decline,
/// outside what this differential grades. `struct_static_compiled_property_is_refused`
/// in `projector_fsharp_core.rs` pins it instead.
fn without_byref_receiver_property(m: ClassMember) -> ClassMember {
    match m {
        ClassMember::Property {
            ty,
            is_static,
            compiled: CompiledAs::Static,
        } => ClassMember::Property {
            ty,
            is_static,
            compiled: CompiledAs::Default,
        },
        other => other,
    }
}

fn class_member_strategy() -> impl Strategy<Value = ClassMember> {
    prop_oneof![
        method_or_property_strategy(),
        (ty_strategy(), any::<bool>())
            .prop_map(|(ty, is_private)| ClassMember::AutoProperty { ty, is_private }),
    ]
}

fn decl_strategy() -> impl Strategy<Value = Decl> {
    prop_oneof![
        (
            prop::collection::vec((ty_strategy(), any::<bool>()), 1..4),
            any::<bool>()
        )
            .prop_map(|(fields, is_struct)| Decl::Record { fields, is_struct }),
        (
            prop::collection::vec(prop::collection::vec(ty_strategy(), 0..3), 1..4),
            any::<bool>(),
            any::<bool>(),
            prop::collection::vec(method_or_property_strategy(), 0..3)
        )
            .prop_map(|(cases, is_struct, require_qualified_access, members)| {
                Decl::Union {
                    cases,
                    is_struct,
                    require_qualified_access,
                    null_as_true: false,
                    members: if is_struct {
                        members
                            .into_iter()
                            .map(without_byref_receiver_property)
                            .collect()
                    } else {
                        members
                    },
                }
            }),
        // A null-as-true union, built in the one shape fsc accepts (FS1196):
        // carrying cases with the single nullary case inserted among them.
        (
            prop::collection::vec(prop::collection::vec(ty_strategy(), 1..3), 1..3),
            any::<prop::sample::Index>(),
            any::<bool>(),
            prop::collection::vec(method_or_property_strategy(), 0..4)
        )
            .prop_map(
                |(mut cases, nullary_at, require_qualified_access, members)| {
                    let at = nullary_at.index(cases.len() + 1);
                    cases.insert(at, vec![]);
                    Decl::Union {
                        cases,
                        is_struct: false,
                        require_qualified_access,
                        null_as_true: true,
                        members,
                    }
                }
            ),
        (1usize..5).prop_map(|case_count| Decl::Enum { case_count }),
        prop::collection::vec(ty_strategy(), 0..3).prop_map(|fields| Decl::Exception { fields }),
        (
            prop::collection::vec(ty_strategy(), 0..3),
            prop::collection::vec(class_member_strategy(), 0..4)
        )
            .prop_map(|(ctor_params, members)| Decl::Class {
                ctor_params,
                members,
            }),
        prop::collection::vec(
            (prop::collection::vec(ty_strategy(), 0..3), ty_strategy()),
            0..4
        )
        .prop_map(|methods| Decl::Interface { methods }),
        (
            prop::collection::vec(binding_strategy(), 1..5),
            any::<bool>()
        )
            .prop_map(|(bindings, require_qualified_access)| {
                Decl::Module {
                    bindings,
                    require_qualified_access,
                }
            }),
        Just(Decl::Measure),
    ]
}

/// Number of generated programs per run. Each costs a `dotnet build` +
/// `fcs-dump`, so CI runs a handful; deep sweeps override via env.
fn generative_cases() -> u32 {
    match std::env::var("BORZOI_GENERATIVE_CASES") {
        Ok(v) => v.parse().expect("BORZOI_GENERATIVE_CASES must be a u32"),
        Err(_) => 4,
    }
}

proptest! {
    // `failure_persistence: None`: integration-test binary, no `lib.rs`
    // anchor for the regression directory (same rationale as
    // `fail_loud.rs`). The shrunk `Vec<Decl>` in the failure output is
    // the reproduction artifact.
    //
    // `max_shrink_iters` is bounded because every shrink step is a
    // `dotnet build`; 200 steps ≈ a few minutes worst case, which is
    // acceptable for the payoff (a minimal divergent program).
    #![proptest_config(ProptestConfig {
        cases: generative_cases(),
        failure_persistence: None,
        max_shrink_iters: 200,
        ..ProptestConfig::default()
    })]

    /// For all valid-by-construction F# programs, our projection of the
    /// compiled assembly equals FCS's.
    #[test]
    fn generated_fsharp_projections_agree(
        decls in prop::collection::vec(decl_strategy(), 1..10),
    ) {
        if let Err(msg) = diff_generated(&decls) {
            return Err(TestCaseError::fail(msg));
        }
    }
}
