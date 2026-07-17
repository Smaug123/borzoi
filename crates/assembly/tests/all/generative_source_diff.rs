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
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;

use borzoi_assembly::test_support::{normalise_entities, parse_fcs_dump};
use borzoi_assembly::{Ecma335Assembly, EcmaView};
use proptest::prelude::*;

use crate::common::{dotnet_build_captured, invoke_fcs_dump};

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
    /// `[<Literal>] let L<i> = <int literal>` — both projectors filter
    /// literals out of the member surface; generating them checks the
    /// *agreement* on that filtering.
    IntLiteral { value: i32 },
}

/// One member of a generated class.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ClassMember {
    /// `member _.M<i> (q0: t0, q1: t1) : ret = …` (tupled — the idiomatic
    /// F# member shape, one IL parameter per element). Empty `params`
    /// renders as `member _.M<i> () : ret = …`. `is_private` exercises
    /// the accessibility filter on both sides of the diff.
    Method {
        params: Vec<Ty>,
        ret: Ty,
        is_static: bool,
        is_private: bool,
    },
    /// `member val [private] P<i> : ty = Unchecked.defaultof<ty> with
    /// get, set`. `is_private` as on [`ClassMember::Method`].
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

fn render_class_member(out: &mut String, idx: usize, m: &ClassMember) {
    match m {
        ClassMember::Method {
            params,
            ret,
            is_static,
            is_private,
        } => {
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
        } => {
            if *is_struct {
                let _ = writeln!(out, "[<Struct>]");
            }
            if *require_qualified_access {
                let _ = writeln!(out, "[<RequireQualifiedAccess>]");
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

/// The project file every generated source compiles under. Mirrors the
/// MiniLibFs fixture's shape (same TFM, deterministic, no docs/PDB) so a
/// generated build behaves exactly like the pinned fixtures.
const FSPROJ: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <AssemblyName>Generated</AssemblyName>
    <RootNamespace>Generated</RootNamespace>
    <Deterministic>true</Deterministic>
    <GenerateDocumentationFile>false</GenerateDocumentationFile>
    <DebugType>none</DebugType>
    <DebugSymbols>false</DebugSymbols>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Library.fs" />
  </ItemGroup>
</Project>
"#;

/// Compile `source` to a DLL in a content-addressed directory under
/// `CARGO_TARGET_TMPDIR`, returning the DLL path. Re-running the same
/// source (e.g. across proptest shrink steps that revisit a candidate)
/// hits the cache. The cache key is a hash of the source; on the
/// (theoretical) collision the stored `Library.fs` differs from `source`
/// and we rebuild over the top, so a collision costs time, not
/// correctness.
fn compile_generated(source: &str) -> Result<PathBuf, String> {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("generative-source-diff")
        .join(format!("{:016x}-{}", hasher.finish(), source.len()));
    let dll = dir
        .join("bin")
        .join("Release")
        .join("net10.0")
        .join("Generated.dll");
    let lib = dir.join("Library.fs");
    let cached = dll.is_file()
        && std::fs::read_to_string(&lib)
            .map(|prev| prev == source)
            .unwrap_or(false);
    if cached {
        return Ok(dll);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    std::fs::write(dir.join("Generated.fsproj"), FSPROJ)
        .map_err(|e| format!("write fsproj: {e}"))?;
    std::fs::write(&lib, source).map_err(|e| format!("write Library.fs: {e}"))?;
    dotnet_build_captured(&dir)?;
    if !dll.is_file() {
        return Err(format!(
            "dotnet build succeeded but {} was not produced",
            dll.display()
        ));
    }
    Ok(dll)
}

/// Compile, project through both readers, and return
/// `(rust_normalised, fcs_normalised, source)` — or a message describing
/// a stage failure (generator escape or reader refusal), which the
/// caller turns into a test failure carrying the source.
fn diff_generated(decls: &[Decl]) -> Result<(), String> {
    let source = render_source(decls);
    let fail = |stage: &str, detail: String| {
        format!("{stage}: {detail}\n--- generated source ---\n{source}")
    };

    let dll_path = compile_generated(&source).map_err(|e| fail("compile", e))?;
    let dll_bytes =
        std::fs::read(&dll_path).map_err(|e| fail("read dll", format!("{e} ({dll_path:?})")))?;

    let view = Ecma335Assembly::parse(&dll_bytes)
        .map_err(|e| fail("Ecma335Assembly::parse", format!("{e:?}")))?;
    let rust_entities = view
        .enumerate_type_defs()
        .map_err(|e| fail("enumerate_type_defs", format!("{e:?}")))?;
    let rust_norm = normalise_entities(&view.identity().name, &rust_entities);

    let fcs_json = invoke_fcs_dump("entities", &dll_path);
    let fcs_norm = parse_fcs_dump(&fcs_json);

    if rust_norm == fcs_norm {
        Ok(())
    } else {
        Err(fail(
            "projection diff",
            format!(
                "normalised assemblies diverge.\nrust ({} entities): {rust_norm:#?}\nfcs  ({} entities): {fcs_norm:#?}",
                rust_norm.entities.len(),
                fcs_norm.entities.len(),
            ),
        ))
    }
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
                },
                ClassMember::Method {
                    params: vec![],
                    ret: Ty::Int,
                    is_static: true,
                    is_private: false,
                },
                ClassMember::Method {
                    params: vec![Ty::Int],
                    ret: Ty::Bool,
                    is_static: false,
                    is_private: true,
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
            (inner.clone(), inner).prop_map(|(a, b)| Ty::Pair(Box::new(a), Box::new(b))),
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

fn class_member_strategy() -> impl Strategy<Value = ClassMember> {
    prop_oneof![
        (
            prop::collection::vec(ty_strategy(), 0..3),
            ty_strategy(),
            any::<bool>(),
            any::<bool>()
        )
            .prop_map(|(params, ret, is_static, is_private)| ClassMember::Method {
                params,
                ret,
                is_static,
                is_private,
            }),
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
            any::<bool>()
        )
            .prop_map(|(cases, is_struct, require_qualified_access)| {
                Decl::Union {
                    cases,
                    is_struct,
                    require_qualified_access,
                }
            }),
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
