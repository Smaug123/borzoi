//! Differential test: our [`SemanticClass`] classification of in-file names
//! against FCS's symbol kinds as the oracle.
//!
//! The property is *certain-implies-agree*, the same shape as the MSBuild /
//! NuGet differentials: for every name occurrence where **we commit** a
//! classification ([`ResolvedFile::classification_at`] returns `Some`), FCS's
//! symbol kind at that occurrence must be *compatible* with it. A decline
//! (`None`) makes no claim and is never held against us — which is exactly what
//! lets a name-resolution-only classifier stay sound in the face of FCS's
//! richer, type-directed classification: we simply say nothing where resolution
//! is not enough.
//!
//! [`committed_classifications`] enforces this in **both directions**: it walks
//! FCS's reported ranges (checking our classification at each), *and* walks our
//! own committed occurrences (checking each against the FCS symbol ending where
//! it does — so a *qualified* whole-span commitment, whose tail is where FCS
//! reports the member/case, is validated too). The one asymmetry is deliberate:
//! FCS is not a *total* oracle — it drops uses in unresolved contexts (`let f a =
//! f a`, non-`rec`, has an unbound body `f`, so FCS reports no body use even
//! though our resolver resolves the param) — so a range FCS reports **nothing**
//! at is a tolerated omission, never a failure; we fail only on an actual
//! *incompatible* FCS symbol at one of our commitments.
//!
//! The oracle is [`fcs-dump uses-census-batch`](crate::common::invoke_fcs_dump_census),
//! which type-checks each snippet *in isolation* against the SDK refs and reports,
//! per occurrence, the `FSharpSymbol` kind (`Class` + the `Is*` flags on
//! [`CensusUse`]). Referenced-assembly names (`printfn`, operators, `System.*`,
//! FSharp.Core's `Some`/`[]`) resolve out-of-file for FCS and we decline them
//! (no [`AssemblyEnv`] here), so this slice exercises the in-file,
//! [`DefKind`]-derived classification — the part a single file can settle.
//!
//! ## Compatibility, not equality
//!
//! The two taxonomies are not identical, and where they legitimately differ we
//! widen the acceptable set rather than assert a false equality (see
//! [`fcs_compatible`]):
//!
//! - **function vs value.** Our [`SemanticClass::Value`] is *syntactic* (a `let`
//!   head with no parameter patterns), so `let g = fun x -> x` is a `Value` even
//!   though FCS types it as a function. We therefore never assert
//!   value-vs-function on `Value`; only `Function` (which we reach solely from a
//!   binding with syntactic parameters, where FCS *must* agree) asserts it.
//! - **parameter vs local.** FCS surfaces ordinary values, function parameters,
//!   and `match` locals all as local *values* (`Mfv`, non-member), so we accept
//!   that for [`SemanticClass::Value`], [`SemanticClass::Parameter`], and
//!   [`SemanticClass::PatternLocal`] and never assert *which* of the three. But so
//!   that this widening cannot mask resolving to the *wrong binder*, those local
//!   classes additionally require our committed binder to start where FCS's
//!   declaration does ([`decl_range_agrees`]) — kind conflation, not declaration
//!   conflation.
//!
//! Run just this group:
//!
//! ```text
//! cargo test -p borzoi-sema --test all classify_diff:: -- --nocapture
//! ```

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, SemanticClass, resolve_file};
use rowan::TextRange;

use crate::common::{CensusUse, LineIndex, invoke_fcs_dump_census, parse_census_jsonl};

/// Snippets in the resolved parser subset that pair each classifiable in-file
/// name with a use, one per category the in-file classifier can commit.
const CORPUS: &[&str] = &[
    // Value: `y`'s binder and the use of `x`.
    "let x = 1\nlet y = x\n",
    // Function + Parameter: `f` (a curried-function binding) and its parameter
    // `a`. This is *not* `rec`, so FCS reports only the two definitions — the body
    // `f a` has an unbound `f` and FCS drops its uses (our resolver still resolves
    // the param `a`, which the converse check tolerates as an FCS omission). The
    // `rec` companion below exercises the body uses FCS *does* report.
    "let f a = f a\n",
    "let rec fac n = fac n\n",
    // Parameter + PatternLocal: `q` is a parameter, `w` a match-clause local.
    "let m q = match q with w -> w\n",
    // Type + UnionCase: `T` a type, `A` a union case used as a constructor.
    "type T = A | B\nlet u = A\n",
    // Type + EnumCase: `Color` a type, `Red` an enum case reached qualified.
    "type Color = Red = 0 | Green = 1\nlet c = Color.Red\n",
    // ExceptionCase: `E` an exception constructor used to construct.
    "exception E of int\nlet r = E 1\n",
    // ActivePattern: the recognizer `(|Even|Odd|)`.
    "let (|Even|Odd|) n = if n % 2 = 0 then Even else Odd\n",
    // Member: a static member of an in-file type, reached qualified. The member
    // *use* `Color2.Red` is recorded at the whole dotted span (the member's own
    // definition is not self-recorded), so it exercises the qualified path.
    "module Demo\ntype Color2() =\n    static member Red = 2\nlet y = Color2.Red\n",
    // Self-referential single-case union: FCS reinterprets `type Ap = Ap` as a
    // single-case *union* (RHS `Ap` is a `UnionCase`, not a type reference —
    // `TyconCoreAbbrevThatIsReallyAUnion`, `id.idText = unionCaseName.idText`).
    // We parse it as an abbreviation, so the RHS must not be committed as a
    // `Type`; declining keeps the soundness contract.
    "type Ap = Ap\nlet z = Ap\n",
    // Active-pattern cases that shadow earlier values: inside the recognizer's
    // own body a case use (`USome x`, `UNone`) must NOT commit the outer
    // `let USome` / `let UNone` values (FCS reports an `ActivePatternCase` there).
    // A resolution-only pass cannot tell that construction apart from a fresh
    // uppercase pattern rebinding, so it declines — the point is that it never
    // commits `Function`/`Value`.
    "let UNone = 1\nlet USome x = x + 1\n\
     let (|UNone|USome|) x = if x > 0 then USome x else UNone\n",
    // Shape-keyed split of parameterized active-pattern arguments (Stage 2 of
    // `docs/parameterized-active-pattern-args-plan.md`). Each row's parameter
    // argument classifies compatibly with FCS's outer-value symbol — the round-3
    // decl-range check fails on these before the split lands (a fabricated
    // pattern-local instead of the outer value). Only FCS-legal shapes: every
    // snippet is `dotnet fsi`-verified. `Lt threshold` (FS0722-illegal) is NOT
    // here — it would fail the gate as an erroring source, not a divergence.
    //
    // `DivBy divisor` — partial single-case, arity 1, k = 1 = paramCount: the
    // lone arg `divisor` is a *parameter* (the unit-payload branch), resolving to
    // the outer value, no result binder.
    "let divisor = 3\n\
     let (|DivBy|_|) d n = if n % d = 0 then Some () else None\n\
     let h n = match n with DivBy divisor -> 1 | _ -> 0\n",
    // `DivBy divisor q` — k = 2 = paramCount + 1: `divisor` a parameter (outer
    // value), `q` the result sub-pattern (a fresh binder).
    "let divisor = 3\n\
     let (|DivBy|_|) d n = if n % d = 0 then Some (n / d) else None\n\
     let h n = match n with DivBy divisor q -> q | _ -> 0\n",
    // `DivBy divisor (Parse v)` — the result is a nested applied active-pattern
    // head; `divisor` a parameter (outer value), `v` binds via the inner split.
    "let divisor = 3\n\
     let (|Parse|_|) s = if s = 0 then None else Some s\n\
     let (|DivBy|_|) d n = if n % d = 0 then Some (n / d) else None\n\
     let h n = match n with DivBy divisor (Parse v) -> v | _ -> 0\n",
    // `Scale factor v` — total single-case, arity 1, k = 2 (`frontAndBack`):
    // `factor` a parameter (outer value), `v` the result binder.
    "let factor = 2\n\
     let (|Scale|) k x = k * x\n\
     let s n = match n with Scale factor v -> v\n",
    // `Scale g` — total single-case, k = 1: `frontAndBack` (arity NEVER consulted)
    // makes the lone arg `g` the *result*, binding at itself (the partially-applied
    // recognizer) and shadowing the outer `let g`, NOT a parameter. Pins the
    // frontAndBack branch — the original positional draft's regression.
    "let g = 0\n\
     let (|Scale|) k x = k * x\n\
     let s n = match n with Scale g -> g 5\n",
    // `Eq A` — a nullary uppercase parameter argument that names a same-file union
    // case. The CST models `A` as a `Pat::LongIdent`, so the parameter helper must
    // resolve it through expression-namespace lookup (→ the union case), not decline
    // it. FCS reports `A` here as the `UnionCase`.
    "type T = A | B\n\
     let (|Eq|_|) (t: T) (x: T) = if t = x then Some() else None\n\
     let classify x = match x with Eq A -> 1 | _ -> 0\n",
];

/// Write `source` to a uniquely-named temp `.fs` file (parallel-safe) and return
/// the path. Left on disk for the duration of the `fcs-dump` child.
fn temp_fs_file(source: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "borzoi_sema_classify_diff_{}_{n}.fs",
        std::process::id()
    ));
    let mut f = std::fs::File::create(&path).expect("create temp .fs");
    f.write_all(source.as_bytes()).expect("write temp .fs");
    path
}

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// The whole corpus's FCS census, computed **once** in a single batched
/// `uses-census-batch` invocation — all snippets go to one FCS process, which is
/// the batch oracle's whole point. Keyed by source, so the soundness gate and
/// every per-category coverage case share it rather than re-launching FCS (~19
/// .NET startups down to one).
fn corpus_census() -> &'static HashMap<&'static str, Vec<CensusUse>> {
    static CENSUS: OnceLock<HashMap<&'static str, Vec<CensusUse>>> = OnceLock::new();
    CENSUS.get_or_init(|| {
        // One temp file per snippet, all censused together in one process.
        let files: Vec<(PathBuf, &'static str)> =
            CORPUS.iter().map(|&s| (temp_fs_file(s), s)).collect();
        let paths: Vec<PathBuf> = files.iter().map(|(p, _)| p.clone()).collect();
        let json = invoke_fcs_dump_census(&paths);
        let parsed = parse_census_jsonl(&json);
        for (p, _) in &files {
            let _ = std::fs::remove_file(p);
        }
        // Associate each census line back to its snippet by file name (unique per
        // temp file), robust to any path normalisation FCS applies.
        let mut by_source = HashMap::new();
        for fc in parsed {
            let name = Path::new(&fc.path).file_name();
            let &(_, source) = files
                .iter()
                .find(|(p, _)| p.file_name() == name)
                .unwrap_or_else(|| panic!("census path {:?} is not a corpus snippet", fc.path));
            assert!(fc.ok, "FCS failed to type-check snippet {source:?}");
            by_source.insert(source, fc.uses);
        }
        assert_eq!(
            by_source.len(),
            CORPUS.len(),
            "every corpus snippet must get exactly one census line"
        );
        by_source
    })
}

/// FCS's per-occurrence symbol kinds for `source` (which must be a [`CORPUS`]
/// snippet), from the shared single-process census.
fn fcs_census(source: &str) -> &'static [CensusUse] {
    corpus_census()
        .get(source)
        .unwrap_or_else(|| panic!("{source:?} is not in CORPUS; add it there"))
        .as_slice()
}

/// Is FCS's symbol kind at an occurrence compatible with the [`SemanticClass`]
/// we committed there? Where the two taxonomies legitimately diverge we widen
/// the acceptable set rather than assert a false equality (see the module docs).
fn fcs_compatible(class: SemanticClass, u: &CensusUse) -> bool {
    let c = u.class.as_str();
    match class {
        // We only reach `Function` from a binding with syntactic parameters, so
        // FCS *must* see a curried function (`CurriedParameterGroups.Count > 0`).
        SemanticClass::Function => c == "Mfv" && u.is_function,
        // A term-level value binding: any non-member `Mfv`. Deliberately does
        // not assert value-vs-function (our function-ness is syntactic).
        SemanticClass::Value => c == "Mfv" && !u.is_member,
        // FCS reports function/lambda parameters and match locals as local
        // values (`Mfv`, non-member), occasionally as `FSharpParameter`.
        SemanticClass::Parameter | SemanticClass::PatternLocal => {
            (c == "Mfv" && !u.is_member) || c == "Parameter"
        }
        // A type name is an entity that is neither a namespace nor a module.
        SemanticClass::Type => c == "Entity" && !u.is_namespace && !u.is_module,
        SemanticClass::UnionCase => c == "UnionCase",
        // Exception constructors — the acceptable set is pinned empirically by
        // `dump_fcs_ground_truth`; widened to whatever FCS actually reports.
        SemanticClass::ExceptionCase => {
            c == "UnionCase" || c == "Entity" || (c == "Mfv" && u.is_constructor)
        }
        SemanticClass::ActivePattern => {
            c == "ActivePatternCase" || (c == "Mfv" && u.is_active_pattern)
        }
        SemanticClass::EnumCase => c == "Field",
        // A member of unspecified flavour: FCS sees a member `Mfv`.
        SemanticClass::Member => c == "Mfv" && u.is_member,
        // Cross-assembly classes: never produced by this single-file, env-less
        // differential (no `AssemblyEnv`), so committing one would be a bug —
        // fail loudly. They are covered by `resolve_assembly`'s classifier test.
        SemanticClass::Module
        | SemanticClass::Method
        | SemanticClass::Property
        | SemanticClass::Event => false,
    }
}

/// The **local** classes — [`SemanticClass::Value`], [`SemanticClass::Parameter`],
/// [`SemanticClass::PatternLocal`] — all surface to FCS as the same non-member
/// `Mfv` symbol kind (FCS does not distinguish a value from a parameter from a
/// match local), so [`fcs_compatible`] alone accepts any of them against any such
/// record — and would stay green even if we resolved the occurrence to the *wrong
/// binder*. Tighten those branches with a declaration check: require our committed
/// binder to **start where FCS's declaration does**. A fabricated binder — e.g.
/// the pre-existing parameterized-active-pattern argument gap, which binds `DivBy
/// divisor`'s `divisor` as a fresh local while FCS resolves it to an outer value —
/// then fails, even though its kind is superficially compatible. Non-local classes
/// carry a discriminating kind of their own and are not tightened. `true` when FCS
/// reports no declaration range (nothing to compare) or the occurrence is not a
/// local class.
fn decl_range_agrees(
    class: SemanticClass,
    u: &CensusUse,
    our_def_start: Option<usize>,
    idx: &LineIndex,
) -> bool {
    match class {
        SemanticClass::Value | SemanticClass::Parameter | SemanticClass::PatternLocal => {
            match (our_def_start, u.decl_range_bytes(idx)) {
                (Some(ours), Some((fcs_start, _))) => ours == fcs_start,
                _ => true,
            }
        }
        _ => true,
    }
}

/// Resolve `source`, and for every FCS occurrence where **we commit** a
/// classification, assert it is compatible with FCS's symbol kind. Returns the
/// committed `(source-text, class)` pairs so callers can assert coverage.
fn committed_classifications(source: &str) -> Vec<(String, SemanticClass)> {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors (outside the subset?): {source:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let rf = resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default());

    let idx = LineIndex::new(source);
    // Group FCS's uses by byte range. FCS can report *several* symbols at one
    // range — a type name and its implicit constructor share the `Color2` token
    // (`Entity` + a ctor `Mfv`) — so our single commitment need only agree with
    // *one* of them; requiring agreement with all would be unsound the other way.
    let mut by_range: HashMap<(usize, usize), Vec<&CensusUse>> = HashMap::new();
    for u in fcs_census(source) {
        let range = u.use_range_bytes(&idx);
        by_range.entry(range).or_default().push(u);
    }

    let mut committed = Vec::new();
    for ((s, e), uses) in &by_range {
        // Skip the implicit anonymous-module symbol (zero-width range).
        if s == e {
            continue;
        }
        let Some(class) = rf.classification_at(span(*s, *e)) else {
            continue; // we decline — no claim, so nothing to check.
        };
        // Our committed binder's start offset (for the local-class declaration
        // check — see [`decl_range_agrees`]). Always present for an in-file local.
        let our_def_start = rf
            .resolution_at(span(*s, *e))
            .and_then(|r| rf.resolved_def(r))
            .map(|d| usize::from(d.range.start()));
        let text = &source[*s..*e];
        assert!(
            uses.iter()
                .copied()
                .any(|u| fcs_compatible(class, u)
                    && decl_range_agrees(class, u, our_def_start, &idx)),
            "we classified {text:?} at {:?} as {class:?}, but no FCS symbol there agrees \
             (in kind and declaration site); FCS reports {:?}; {source:?}",
            span(*s, *e),
            uses.iter()
                .map(|u| format!(
                    "class={} member={} prop={} val={} fn={} ns={} mod={} ctor={} ap={}",
                    u.class,
                    u.is_member,
                    u.is_property,
                    u.is_value,
                    u.is_function,
                    u.is_namespace,
                    u.is_module,
                    u.is_constructor,
                    u.is_active_pattern,
                ))
                .collect::<Vec<_>>(),
        );
        committed.push((text.to_string(), class));
    }

    // Converse direction: check the classifications WE emit, not only the ranges
    // FCS reports — otherwise a commitment at a range the FCS-driven loop above
    // never visits (a *qualified* whole-span occurrence, whose tail is where FCS
    // reports the member/case) is unchecked. Reconcile ranges by END offset, like
    // [`ResolvedFile::token_classifier`]: the whole-span occurrence ends at its
    // tail segment — exactly where FCS reports the tail symbol — and a bare name
    // matches exactly.
    //
    // **Omission is tolerated, disagreement is not.** FCS is not a total oracle: a
    // range it reports *nothing* at does not make our commitment there wrong — it
    // drops uses in unresolved contexts (`let f a = f a`: the non-`rec` body `f`
    // is unbound, so FCS reports no body use, though our resolver still resolves
    // the param `a`). So we fail only when FCS *does* report symbol(s) ending
    // where our occurrence does and **none** is compatible — a genuine
    // disagreement — never on absence.
    let fcs_ranges: Vec<(usize, usize, &CensusUse)> = fcs_census(source)
        .iter()
        .map(|u| {
            let (us, ue) = u.use_range_bytes(&idx);
            (us, ue, u)
        })
        .collect();
    for occ in rf.resolutions().keys() {
        let (os, oe) = (usize::from(occ.start()), usize::from(occ.end()));
        if os == oe {
            continue; // zero-width (e.g. an implicit module) — nothing to check.
        }
        let Some(class) = rf.classification_at(*occ) else {
            continue; // not a commitment (cross-file / deferred / unresolved).
        };
        let our_def_start = rf
            .resolution_at(*occ)
            .and_then(|r| rf.resolved_def(r))
            .map(|d| usize::from(d.range.start()));
        // FCS symbols ending exactly where our occurrence does, contained in it.
        let ending_here: Vec<&CensusUse> = fcs_ranges
            .iter()
            .filter(|&&(us, ue, _)| ue == oe && us >= os)
            .map(|&(_, _, u)| u)
            .collect();
        if ending_here.is_empty() {
            continue; // FCS omission — absence is not disagreement.
        }
        assert!(
            ending_here
                .iter()
                .any(|u| fcs_compatible(class, u)
                    && decl_range_agrees(class, u, our_def_start, &idx)),
            "we classified {:?} at {os}..{oe} as {class:?}, but every FCS symbol ending there \
             disagrees (in kind or declaration site); {source:?}",
            &source[os..oe],
        );
    }
    committed
}

/// Soundness gate: over the whole curated corpus, every commitment agrees with
/// FCS, and every snippet commits at least once (so the loop is never a silent
/// no-op that proves nothing).
#[test]
fn classification_agrees_with_fcs_over_the_corpus() {
    for source in CORPUS {
        let committed = committed_classifications(source);
        assert!(
            !committed.is_empty(),
            "no classification committed for {source:?} — the differential proves nothing"
        );
    }
}

/// Coverage: **every** in-file [`SemanticClass`] a [`DefKind`] can produce is
/// actually committed (and committed correctly) somewhere in the corpus.
/// Guards against a regression to decline-everything — which the soundness gate
/// alone would pass vacuously — *and* against a whole category silently
/// dropping out. Exhaustive on the enum: adding a `SemanticClass` variant that a
/// `DefKind` maps to without giving it a case here should be a visible omission.
#[test]
fn commits_each_in_file_category() {
    fn has(source: &str, text: &str, class: SemanticClass) -> bool {
        committed_classifications(source)
            .iter()
            .any(|(t, c)| t == text && *c == class)
    }
    // Assert each expectation, and record which class it covers, so the match
    // below is the single checklist of covered variants.
    let cover = |source: &str, text: &str, class: SemanticClass| {
        assert!(
            has(source, text, class),
            "{text:?} in {source:?} should classify as {class:?}"
        );
        class
    };

    for class in [
        SemanticClass::Function,
        SemanticClass::Value,
        SemanticClass::Parameter,
        SemanticClass::PatternLocal,
        SemanticClass::Type,
        SemanticClass::UnionCase,
        SemanticClass::ExceptionCase,
        SemanticClass::ActivePattern,
        SemanticClass::EnumCase,
        SemanticClass::Member,
    ] {
        let covered = match class {
            SemanticClass::Function => cover("let f a = f a\n", "f", class),
            SemanticClass::Value => cover("let x = 1\nlet y = x\n", "x", class),
            SemanticClass::Parameter => cover("let f a = f a\n", "a", class),
            SemanticClass::PatternLocal => cover("let m q = match q with w -> w\n", "w", class),
            SemanticClass::Type => cover("type T = A | B\nlet u = A\n", "T", class),
            SemanticClass::UnionCase => cover("type T = A | B\nlet u = A\n", "A", class),
            SemanticClass::ExceptionCase => cover("exception E of int\nlet r = E 1\n", "E", class),
            SemanticClass::ActivePattern => cover(
                "let (|Even|Odd|) n = if n % 2 = 0 then Even else Odd\n",
                "Even",
                class,
            ),
            SemanticClass::EnumCase => cover(
                "type Color = Red = 0 | Green = 1\nlet c = Color.Red\n",
                "Red",
                class,
            ),
            SemanticClass::Member => cover(
                "module Demo\ntype Color2() =\n    static member Red = 2\nlet y = Color2.Red\n",
                "Color2.Red",
                class,
            ),
            // Cross-assembly classes are not in-file; the loop never yields them
            // (they're covered by `resolve_assembly`'s classifier test).
            SemanticClass::Module
            | SemanticClass::Method
            | SemanticClass::Property
            | SemanticClass::Event => unreachable!("not an in-file class"),
        };
        assert_eq!(covered, class);
    }
}

/// A qualified reference (`Color.Red`, `Color2.Red`) records its resolution
/// under the whole dotted range, so the **tail** token (`Red`) has no exact key
/// — enum cases are require-qualified and member definitions are not
/// self-recorded. The token-oriented [`ResolvedFile::token_classifier`] must
/// still classify the tail (from the qualified occurrence ending there) while
/// the head keeps its own class; an exact [`ResolvedFile::classification_at`]
/// on the tail token declines.
#[test]
fn token_classifier_resolves_qualified_tails() {
    let cases = [
        (
            "type Color = Red = 0 | Green = 1\nlet c = Color.Red\n",
            "Color",
            SemanticClass::Type,
            SemanticClass::EnumCase,
        ),
        (
            "module Demo\ntype Color2() =\n    static member Red = 2\nlet y = Color2.Red\n",
            "Color2",
            SemanticClass::Type,
            SemanticClass::Member,
        ),
    ];
    for (source, head, head_class, tail_class) in cases {
        let parsed = parse(source);
        assert!(parsed.errors.is_empty(), "parse errors in {source:?}");
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let rf = resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default());
        let classify = rf.token_classifier();

        // The qualified use is `<head>.Red`; locate that occurrence.
        let dotted = format!("{head}.Red");
        let base = source.rfind(&dotted).expect("the qualified use");
        let head_range = span(base, base + head.len());
        let tail_off = base + head.len() + 1; // skip `<head>.`
        let tail_range = span(tail_off, tail_off + "Red".len());

        // The tail token has no exact key of its own …
        assert_eq!(
            rf.classification_at(tail_range),
            None,
            "the tail token should have no *exact* occurrence in {source:?}"
        );
        // … but the token-oriented classifier finds it via the qualified path.
        assert_eq!(
            classify(tail_range),
            Some(tail_class),
            "tail `Red` in {source:?}"
        );
        // The head keeps its own class.
        assert_eq!(
            classify(head_range),
            Some(head_class),
            "head {head:?} in {source:?}"
        );
    }
}

/// Regression (soundness): a self-referential single-case union `type Ap = Ap`
/// is reinterpreted by FCS as a *union* — the RHS `Ap` is a `UnionCase`, not a
/// type reference (`TyconCoreAbbrevThatIsReallyAUnion`, the `id = unionCaseName`
/// branch). Our parser models it as an abbreviation, so the RHS resolves to the
/// type being defined; committing `Type` there would violate the
/// certain-implies-agree contract. The RHS occurrence must therefore decline (or
/// commit `UnionCase`), while the *type name* (the LHS) still commits `Type`.
#[test]
fn self_referential_single_case_union_rhs_is_not_committed_as_type() {
    let source = "type Ap = Ap\nlet z = Ap\n";
    let parsed = parse(source);
    assert!(parsed.errors.is_empty(), "parse errors in {source:?}");
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let rf = resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default());

    // `type Ap = Ap`: the LHS name at 5..7, the RHS at 10..12.
    let lhs = span(5, 7);
    let rhs = span(10, 12);
    assert_eq!(&source[5..7], "Ap");
    assert_eq!(&source[10..12], "Ap");

    // The type name still commits `Type` (FCS reports it as an `Entity`).
    assert_eq!(
        rf.classification_at(lhs),
        Some(SemanticClass::Type),
        "the type-name LHS should classify as Type"
    );
    // The RHS is a union case for FCS — we must not commit `Type` there.
    assert!(
        matches!(
            rf.classification_at(rhs),
            None | Some(SemanticClass::UnionCase)
        ),
        "the self-referential single-case-union RHS must decline (or be a UnionCase), \
         not commit {:?}",
        rf.classification_at(rhs)
    );
}

/// Regression (soundness): inside a total active pattern's own body, using one of
/// its cases (`then USome x`, `else UNone`) is — in FCS — an **active-pattern
/// case**, shadowing an earlier same-named `let` value. The classifier must not
/// commit the shadowed `Function`/`Value` there; it declines (a bare case name in
/// the body is ambiguous with a fresh uppercase pattern rebinding).
#[test]
fn active_pattern_body_case_does_not_commit_shadowed_value() {
    let source = "let UNone = 1\nlet USome x = x + 1\n\
                  let (|UNone|USome|) x = if x > 0 then USome x else UNone\n";
    let parsed = parse(source);
    assert!(parsed.errors.is_empty(), "parse errors in {source:?}");
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let rf = resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default());

    let usome_body = {
        let base = source.find("then USome").expect("the case construction") + "then ".len();
        span(base, base + "USome".len())
    };
    let unone_body = {
        let base = source.find("else UNone").expect("the case construction") + "else ".len();
        span(base, base + "UNone".len())
    };
    assert_eq!(
        &source[usome_body.start().into()..usome_body.end().into()],
        "USome"
    );
    assert_eq!(
        &source[unone_body.start().into()..unone_body.end().into()],
        "UNone"
    );

    for (occ, name) in [(usome_body, "USome"), (unone_body, "UNone")] {
        assert!(
            matches!(
                rf.classification_at(occ),
                None | Some(SemanticClass::ActivePattern)
            ),
            "the case construction {name:?} in the recognizer body must not commit a \
             shadowed value; got {:?}",
            rf.classification_at(occ)
        );
    }
}

/// Ground-truth dump (ignored): print each occurrence's FCS symbol kind
/// alongside our resolution, to pin [`fcs_compatible`] against reality rather
/// than memory. Run with:
///
/// ```text
/// cargo test -p borzoi-sema --test all classify_diff::dump_fcs_ground_truth -- --ignored --nocapture
/// ```
#[test]
#[ignore = "diagnostic dump, not a gate"]
fn dump_fcs_ground_truth() {
    for source in CORPUS {
        println!("\n=== {source:?} ===");
        let parsed = parse(source);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let rf = resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default());
        let idx = LineIndex::new(source);
        for u in fcs_census(source) {
            let (s, e) = u.use_range_bytes(&idx);
            if s == e {
                continue;
            }
            let text = &source[s..e];
            let res = rf.resolution_at(span(s, e));
            let kind = res.and_then(|r| rf.resolved_def(r)).map(|d| d.kind);
            let ours = rf.classification_at(span(s, e));
            println!(
                "  {text:<10} fcs[class={:<16} def?={} member={} prop={} val={} fn={} ns={} mod={} ctor={} ap={}] \
                 ours[res={res:?} kind={kind:?} class={ours:?}]",
                u.class,
                u.is_from_definition,
                u.is_member,
                u.is_property,
                u.is_value,
                u.is_function,
                u.is_namespace,
                u.is_module,
                u.is_constructor,
                u.is_active_pattern,
            );
        }
    }
}
