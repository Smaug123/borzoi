//! Stage OV-9 — the **generator + corpus differential**
//! (`docs/overload-resolution-plan.md` §6, OV-9): the automated landmine
//! detector for overload resolution, and the coverage measurement that decides
//! whether OV-8 (betterness) is worth building.
//!
//! [`common::overload_corpus`] generates one universe in two views — a **C#
//! assembly** of overload sets (every unordered pair of parameter types from the
//! closed set ∪ `obj` ∪ a base/derived pair ∪ arrays, plus the optional /
//! `params` / `out` / split-arity / cross-level / override / generic shapes) and
//! an **F# call-site matrix** over them, one call per line. The assembly is
//! referenced by *both* FCS (`BORZOI_FCS_EXTRA_REFS`) and our
//! [`AssemblyEnv`], so the two sides see the same method groups, and every call
//! site is a case.
//!
//! # The property, in both directions
//!
//! The plan's §1 keystone needs *two* approximations with opposite soundness
//! requirements, and the arity shortcut died by using one test for both. So the
//! differential asserts both, per call site:
//!
//! 1. **We commit ⇒ FCS chose the same overload.** Our recorded
//!    `Resolution::Member` must match the OV-1 oracle's chosen candidate *by
//!    signature* (§3.1: never by `Kind`, never by declaring entity — an override
//!    can retarget it), and our published type must equal its return type. If we
//!    commit where FCS has no call node at all (an *error* — FCS resolved
//!    nothing), that is a failure too.
//! 2. **FCS finds a candidate applicable ⇒ [`AssemblyEnv::may_apply`] does not
//!    refute it.** The over-approximation contract (§4.2), checked against the
//!    real oracle rather than against our own reasoning: whatever FCS chose must
//!    survive our refuter. This is the direction the arity shortcut got wrong
//!    (it *over*-rejected), and it is the one no amount of unit-testing the
//!    matcher against itself can establish.
//!
//! Direction 1 alone is satisfiable by an engine that defers everything, so the
//! differential also floors the commit count ([`MIN_COMMITS`]) — a corpus that
//! stops committing is a regression, not a pass.
//!
//! Every published expression type is *additionally* checked against the `types`
//! oracle at its exact range (the D5 soundness net the OV-6/OV-7 differentials
//! use), which catches over-claiming anywhere in the file, not just at call
//! nodes.
//!
//! # Coverage (OV-9(b))
//!
//! [`coverage_report`] (`#[ignore]`d — it is a measurement, not a gate) prints
//! the commit rate over the matrix and a **defer-reason histogram**, splitting
//! the deferrals into the ones betterness would recover (≥ 2 candidates survive
//! `may_apply`, so FCS runs its 14-rule ladder and we cannot) from the ones it
//! would not (a unique survivor that `must_apply` declines to affirm — the
//! type-directed-conversion / omitted-optional affirmation gap). That split is
//! the number that decides whether OV-8 is worth building.
//!
//! ```text
//! nix develop -c cargo test -p borzoi-sema --test all overload_corpus_diff:: -- --ignored --nocapture
//! ```

use std::collections::HashMap;

use crate::common::overload_corpus::{Corpus, Site, corpus};
use crate::common::{
    FcsCall, ensure_overload_corpus_built, ensure_system_runtime_dll, invoke_fcs_dump_with_refs,
    parse_fcs_overloads_with_errors, parse_fcs_types, temp_fs_file,
};
use borzoi_assembly::{Ecma335Assembly, Member, MethodLike, Primitive, TypeRef};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, EntityHandle, InferredFile, ProjectItems, Resolution, Ty, arity_window,
    infer_file, resolve_file,
};

/// Floor on the number of call sites we commit. Guards against the differential
/// going vacuous: "we deferred" satisfies the property trivially, so a corpus
/// that stops committing would silently pass. Measured 2026-07-12 (380); raise it
/// when a stage lands (OV-8 will), never lower it without a stated reason.
const MIN_COMMITS: usize = 350;

/// Floor on the commits made on a **genuine overload set** — a group of ≥ 2
/// candidates, where the commit went through the §1 keystone (unique `may_apply`
/// survivor, affirmed by `must_apply`) rather than through FCS's
/// single-candidate arity shortcut. Without this the headline count above could
/// stay green while the keystone itself — the thing this whole plan exists to
/// get right — was exercised by nothing. Measured 2026-07-12 (250).
const MIN_OVERLOAD_SET_COMMITS: usize = 200;

/// Everything one run of the corpus produces: the generated universe, our
/// inference of it, and FCS's two oracles over the *same* file.
///
/// Everything the per-site loops need is **indexed by line** up front. The
/// corpus is thousands of call sites and thousands of resolutions, so a
/// per-site scan (or a per-lookup `line_of` that rescans the source prefix)
/// turns the differential quadratic in file size — measured at ten CPU-minutes
/// before this was indexed, against ~150 ms for the inference it is checking.
struct Run {
    corpus: Corpus,
    env: AssemblyEnv,
    /// Byte offset of the start of each line (`lines[0]` is line 1).
    line_starts: Vec<usize>,
    inferred: InferredFile,
    /// Binder name (`r17`, `o`, `xs`, …) → the type we inferred for it.
    def_types: HashMap<String, Ty>,
    /// The method **we** committed on each line (there is at most one call per
    /// line by construction), cloned out of the env so the borrow ends here.
    our_calls: HashMap<usize, (EntityHandle, MethodLike)>,
    /// FCS's chosen overload at each invocation node, keyed by (line, method).
    fcs_calls: HashMap<(usize, String), FcsCall>,
    /// The lines FCS reported an **error** on. A call node on such a line was
    /// *elaborated*, not necessarily *resolved* (see [`common::FcsError`]) — the
    /// single-`IsCandidate` shortcut (§2.2) names the lone arity-surviving
    /// candidate without any applicability test — so no claim about FCS's
    /// applicability judgment may be made there.
    fcs_error_lines: std::collections::HashSet<usize>,
    /// FCS's inferred type at each expression range (the D5 soundness net).
    fcs_types: HashMap<(usize, usize), String>,
}

/// Generate the corpus, compile it, and run both sides over it. One `dotnet
/// build` and two `fcs-dump` invocations for the whole matrix.
fn run() -> Run {
    let corpus = corpus();
    let dll = ensure_overload_corpus_built(&corpus.csharp);
    let system_runtime = ensure_system_runtime_dll();

    // Our side: an `AssemblyEnv` over the real BCL *and* the corpus assembly. No
    // FSharp.Core — its implicit auto-opens are an extension surface, which
    // OV-6's absence gate (rightly) treats as "an extension member of this name
    // might exist" and defers on. FCS always references FSharp.Core, but its
    // extensions on BCL types are exotic (`AsyncRead`, `GetReverseIndex`, … —
    // plan §6.1(c)) and cannot collide with the corpus's `M`/`I`.
    let bcl_bytes = std::fs::read(&system_runtime).expect("read System.Runtime.dll");
    let corpus_bytes = std::fs::read(dll).expect("read OverloadCorpus.dll");
    let bcl = Ecma335Assembly::parse(&bcl_bytes).expect("parse System.Runtime.dll");
    let ovc = Ecma335Assembly::parse(&corpus_bytes).expect("parse OverloadCorpus.dll");
    let env = AssemblyEnv::from_views(&[bcl, ovc]).expect("build AssemblyEnv");

    let parsed = parse(&corpus.fsharp);
    assert!(
        parsed.errors.is_empty(),
        "the generated corpus must parse cleanly: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    let def_types = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.clone()))
        .collect();

    let line_starts = line_starts(&corpus.fsharp);
    let our_calls = inferred
        .member_resolutions()
        .iter()
        .filter_map(|(range, res)| {
            let Resolution::Member { parent, idx } = res else {
                return None;
            };
            let Member::Method(m) = env.member_at(*parent, *idx) else {
                return None;
            };
            let line = line_at(&line_starts, usize::from(range.start()));
            Some((line, (*parent, m.clone())))
        })
        .collect();

    // FCS's side: the same file, the same reference.
    let path = temp_fs_file("ov9_corpus", &corpus.fsharp);
    let overloads_json = invoke_fcs_dump_with_refs("overloads", &path, &[dll]);
    let types_json = invoke_fcs_dump_with_refs("types", &path, &[dll]);
    let _ = std::fs::remove_file(&path);

    let (calls, errors) = parse_fcs_overloads_with_errors(&overloads_json, &corpus.fsharp);
    let fcs_calls = calls
        .into_iter()
        .map(|c| ((line_at(&line_starts, c.start), c.name.clone()), c))
        .collect();
    let fcs_error_lines = errors.iter().map(|e| e.line as usize).collect();
    let fcs_types = parse_fcs_types(&types_json, &corpus.fsharp);

    Run {
        corpus,
        env,
        line_starts,
        inferred,
        def_types,
        our_calls,
        fcs_calls,
        fcs_error_lines,
        fcs_types,
    }
}

/// Byte offset of the start of every line in `src`.
fn line_starts(src: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        src.bytes()
            .enumerate()
            .filter(|&(_, b)| b == b'\n')
            .map(|(i, _)| i + 1),
    );
    starts
}

/// The 1-based line containing byte offset `at` — a binary search over
/// [`line_starts`], so a per-site lookup is O(log n) rather than O(offset).
fn line_at(starts: &[usize], at: usize) -> usize {
    match starts.binary_search(&at) {
        Ok(i) => i + 1,
        Err(i) => i,
    }
}

impl Run {
    /// The member FCS's typed tree **names** at `site` — an *elaboration*, which
    /// is a strictly weaker fact than a resolution (see [`Self::fcs_resolution`]):
    /// the single-`IsCandidate` shortcut and error recovery both put a `Call`
    /// node here for calls FCS rejected. `None` means FCS elaborated no call at
    /// all, and then we must have deferred.
    fn fcs_choice(&self, site: &Site) -> Option<&FcsCall> {
        self.fcs_calls.get(&(site.line, site.method.to_owned()))
    }

    /// The member *we* committed at `site` (recorded at the method-name range,
    /// which lies on the site's line), or `None` when we deferred.
    fn our_choice(&self, site: &Site) -> Option<&(EntityHandle, MethodLike)> {
        self.our_calls.get(&site.line)
    }

    /// The 1-based line containing byte offset `at`.
    fn line(&self, at: usize) -> usize {
        line_at(&self.line_starts, at)
    }

    /// The overload FCS chose at `site` **on a call it actually resolved** — no
    /// error on the line. Only here is "FCS chose `c`" evidence that FCS found
    /// `c` *applicable*; elsewhere the node may be the single-`IsCandidate`
    /// shortcut's un-type-checked elaboration, or error recovery.
    fn fcs_resolution(&self, site: &Site) -> Option<&FcsCall> {
        if self.fcs_error_lines.contains(&site.line) {
            return None;
        }
        self.fcs_choice(site)
    }

    /// The type we published for the site's binder (`let r<line> = …`), or `None`
    /// when the type deferred (an identity-only commit — a `void` or unbridgeable
    /// return; the corpus has none, but the engine's contract allows it).
    fn our_type(&self, site: &Site) -> Option<&Ty> {
        self.def_types.get(&format!("r{}", site.line))
    }

    /// The receiver entity a site's call resolves against — the corpus type
    /// itself, whether the call is static (`OvCorpus.T.M`) or instance
    /// (`c.I` on a receiver of type `T`).
    fn receiver(&self, site: &Site) -> Option<EntityHandle> {
        let (ns, name) = site.declaring.split_once('.').expect("qualified name");
        self.env.lookup_type(&[ns.to_owned()], name, 0)
    }

    /// The candidate group our engine would see at `site` (the OV-6/OV-7
    /// provably-complete group), or `None` when it declines to build one.
    fn group(&self, site: &Site) -> Option<Vec<(EntityHandle, MethodLike)>> {
        let handle = self.receiver(site)?;
        let group = if site.is_static {
            self.env.static_method_group(handle, site.method)
        } else {
            self.env.instance_method_group(handle, site.method)
        }?;
        // Clone out so the borrow of `self.env` ends here (callers pass the
        // members back into `&self.env` methods).
        Some(group.into_iter().map(|(h, _, m)| (h, m.clone())).collect())
    }
}

// ============================================================================
// Signature rendering — the comparison currency (plan §3.1: compare by
// *signature*, never by declaring entity, which an override can retarget)
// ============================================================================

/// Canonically render a metadata [`TypeRef`] the way the `overloads` oracle's
/// `renderTypeCanonical` renders FCS's — `System.Int32`, `OvCorpus.BaseTy`,
/// `System.Int32[]`, `System.Int32&`. Anything outside the shapes the corpus
/// declares (a generic parameter, a pointer) renders to a marker that can never
/// compare equal, so an unexpected shape fails loudly rather than silently
/// matching.
fn render_type_ref(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Primitive(p) => primitive_name(*p).to_owned(),
        TypeRef::Named {
            namespace,
            name,
            type_args,
            ..
        } if type_args.is_empty() => {
            if namespace.is_empty() {
                name.clone()
            } else {
                format!("{}.{name}", namespace.join("."))
            }
        }
        TypeRef::Array { element, rank, .. } if *rank == 1 => {
            format!("{}[]", render_type_ref(&element.ty))
        }
        TypeRef::ByRef { inner, .. } => format!("{}&", render_type_ref(inner)),
        other => format!("<unrendered:{other:?}>"),
    }
}

fn primitive_name(p: Primitive) -> &'static str {
    match p {
        Primitive::Void => "System.Void",
        Primitive::Bool => "System.Boolean",
        Primitive::Char => "System.Char",
        Primitive::I1 => "System.SByte",
        Primitive::U1 => "System.Byte",
        Primitive::I2 => "System.Int16",
        Primitive::U2 => "System.UInt16",
        Primitive::I4 => "System.Int32",
        Primitive::U4 => "System.UInt32",
        Primitive::I8 => "System.Int64",
        Primitive::U8 => "System.UInt64",
        Primitive::R4 => "System.Single",
        Primitive::R8 => "System.Double",
        Primitive::IntPtr => "System.IntPtr",
        Primitive::UIntPtr => "System.UIntPtr",
        Primitive::Object => "System.Object",
        Primitive::String => "System.String",
    }
}

/// A method's parameter list in the oracle's currency: one canonical type per
/// parameter, byref-marked (`System.Int32&`) exactly as FCS renders an `out`.
fn our_params(m: &MethodLike) -> Vec<String> {
    m.signature
        .parameters
        .iter()
        .map(|p| {
            let base = render_type_ref(&p.ty);
            if p.is_byref || p.is_out {
                format!("{base}&")
            } else {
                base
            }
        })
        .collect()
}

// ============================================================================
// The generator's own invariant (FCS-free, instant)
// ============================================================================

/// Everything in the differential is keyed by **line number** — our member
/// resolution, our binder type, FCS's chosen overload. If the generator's
/// recorded `Site::line` ever drifted from the line it actually emitted, every
/// per-site comparison would silently compare *different call sites*, and the
/// sweep would go quietly meaningless rather than fail. So pin the bookkeeping
/// itself, with no oracle and no fixture build in the way.
#[test]
fn generated_sites_sit_on_the_lines_they_claim() {
    let c = corpus();
    let lines: Vec<&str> = c.fsharp.lines().collect();
    assert!(!c.sites.is_empty(), "the generator produced no call sites");
    for site in &c.sites {
        assert_eq!(
            lines.get(site.line - 1).copied(),
            Some(site.text.as_str()),
            "site {} claims line {} but that line reads {:?}",
            site.text,
            site.line,
            lines.get(site.line - 1),
        );
        // The binder name the differential reads our type back from.
        assert!(
            site.text.starts_with(&format!("let r{} = ", site.line)),
            "a call site must bind `r<line>`: {}",
            site.text,
        );
    }
    // Each line carries at most one call site (the per-line key must be unique).
    let mut seen = std::collections::HashSet::new();
    for site in &c.sites {
        assert!(
            seen.insert(site.line),
            "two call sites on line {}: the per-line key is not unique",
            site.line,
        );
    }
}

// ============================================================================
// The differential
// ============================================================================

#[test]
fn our_commit_agrees_with_fcs_or_we_deferred() {
    let run = run();

    // The generated prelude must actually type: every corpus receiver and every
    // factory-produced argument value comes from a single-candidate static call,
    // and if those deferred, every call site would defer on a non-ground argument
    // and the whole differential would go vacuous in a way `MIN_COMMITS` alone
    // would not localise.
    for (binder, expected) in [
        ("o", "System.Object"),
        ("b", "OvCorpus.BaseTy"),
        ("d", "OvCorpus.DerivedTy"),
        ("xs", "System.Int32[]"),
    ] {
        assert_eq!(
            run.def_types.get(binder).map(Ty::render).as_deref(),
            Some(expected),
            "the corpus prelude binder `{binder}` must ground (it feeds every argument position)"
        );
    }

    // ── The D5 soundness net: every type we published, FCS agrees with. ───────
    for (range, ty) in run.inferred.types() {
        let key = (usize::from(range.start()), usize::from(range.end()));
        let fcs = run.fcs_types.get(&key).unwrap_or_else(|| {
            panic!(
                "we typed {key:?} as {} but FCS has no node there (line {}: {:?})",
                ty.render(),
                run.line(key.0),
                source_line(&run.corpus.fsharp, key.0)
            )
        });
        assert_eq!(
            &ty.render(),
            fcs,
            "type mismatch at line {}: {:?}",
            run.line(key.0),
            source_line(&run.corpus.fsharp, key.0)
        );
    }

    // Violations are *collected*, not thrown at the first sighting: this is a
    // sweep, and one run should report the whole minefield (a single `panic!`
    // would hide every landmine but the first, and each re-run costs a full FCS
    // type-check of the corpus).
    let mut violations: Vec<String> = Vec::new();
    let mut commits = 0usize;
    let mut overload_set_commits = 0usize;
    let mut fcs_resolved = 0usize;
    let mut may_apply_checked = 0usize;

    for site in &run.corpus.sites {
        let ours = run.our_choice(site);
        let theirs = run.fcs_choice(site);

        // ── Direction 1: we commit ⇒ FCS chose the same overload. ─────────────
        if let Some((_, m)) = ours {
            commits += 1;
            if run.group(site).is_some_and(|g| g.len() >= 2) {
                overload_set_commits += 1;
            }
            match theirs {
                None => violations.push(format!(
                    "COMMITTED WHERE FCS RESOLVED NOTHING (FCS errors here, so we must defer)\n    \
                     {}\n    ours: {}({})",
                    site.text,
                    m.name,
                    our_params(m).join(", "),
                )),
                Some(fcs) if our_params(m) != fcs.flat_params() => violations.push(format!(
                    "COMMITTED A DIFFERENT OVERLOAD THAN FCS CHOSE (compared by signature, §3.1)\n    \
                     {}\n    ours: {}({})\n    FCS:  {} [{}]",
                    site.text,
                    m.name,
                    our_params(m).join(", "),
                    fcs.xml_doc_sig,
                    fcs.declaring_type,
                )),
                Some(fcs) => {
                    // The published type must be the chosen overload's return type.
                    if let Some(ty) = run.our_type(site)
                        && ty.render() != fcs.ret
                    {
                        violations.push(format!(
                            "PUBLISHED THE WRONG RETURN TYPE\n    {}\n    ours: {}\n    FCS:  {}",
                            site.text,
                            ty.render(),
                            fcs.ret,
                        ));
                    }
                }
            }
        }

        // ── Direction 2: FCS's chosen candidate must survive `may_apply`. ─────
        //
        // The over-approximation contract (§4.2): everything FCS finds
        // applicable, our refuter must NOT eliminate. This is the direction the
        // abandoned arity shortcut violated, and it is checkable here *against
        // the real oracle* because FCS names the candidate it chose.
        //
        // **Only on a cleanly-resolved site.** A call node on an error line was
        // elaborated without an applicability test (§2.2's single-`IsCandidate`
        // shortcut: a sole `M(int)` called `M("x")` still names `M(int)`), so
        // refuting *that* candidate is correct, not a contract violation — the
        // engine mirrors the same shortcut and commits it, which direction 1
        // checks. Restricting to error-free sites is what makes this an
        // applicability claim rather than an elaboration claim.
        let Some(fcs) = run.fcs_resolution(site) else {
            continue;
        };
        fcs_resolved += 1;
        let (Some(group), Some(handle)) = (run.group(site), run.receiver(site)) else {
            // Our group construction declined (a shape we cannot enumerate); there
            // is no candidate of ours to check FCS's choice against.
            continue;
        };
        let declaring = run.env.entity(handle).assembly.name.clone();
        // Locate FCS's chosen candidate in *our* group, by signature. A miss is
        // legitimate only for a shape outside our rendering (a generic candidate,
        // whose parameters FCS renders as typars — plan §7).
        let Some((_, chosen)) = group
            .iter()
            .find(|(_, m)| our_params(m) == fcs.flat_params())
        else {
            continue;
        };
        let args = arg_types(site);
        // **Direct unit syntax is candidate-dependent** (§2.2, and the OV-7
        // review-1 probe): FCS reads `M()` as *zero* arguments when the candidate
        // admits arity 0, but as **one** (ill-typed) `unit` argument otherwise —
        // and the single-candidate shortcut elaborates the member either way
        // (`String.IsNullOrEmpty()` ⇒ `Boolean`). So at a unit site FCS did not
        // necessarily match the argument list we hand `may_apply`, and refuting a
        // 1-parameter candidate at arity 0 is *correct*. The claim that survives
        // both readings is the disjunction.
        let survives = run.env.may_apply(chosen, &declaring, &args)
            || (args.is_empty() && arity_window(chosen).contains(1));
        if !survives {
            violations.push(format!(
                "`may_apply` REFUTED THE CANDIDATE FCS CHOSE — the over-approximation contract \
                 (§4.2) is broken: everything FCS affirms, we must not eliminate.\n    {}\n    \
                 FCS chose: {}({})\n    args: {:?}",
                site.text,
                chosen.name,
                our_params(chosen).join(", "),
                args.iter().map(Ty::render).collect::<Vec<_>>(),
            ));
        }
        may_apply_checked += 1;
    }

    println!(
        "OV-9 corpus: {} call sites, {fcs_resolved} FCS-resolved, {may_apply_checked} \
         may_apply-checked, {commits} commits ({overload_set_commits} on genuine overload \
         sets), {} violations",
        run.corpus.sites.len(),
        violations.len(),
    );
    assert!(
        violations.is_empty(),
        "{} OV-9 violations over {} call sites:\n\n{}",
        violations.len(),
        run.corpus.sites.len(),
        violations.join("\n\n"),
    );
    assert!(
        commits >= MIN_COMMITS,
        "the differential has gone vacuous: only {commits} commits over {} call sites \
         (floor {MIN_COMMITS}). \"We deferred\" satisfies the property trivially, so a \
         collapse in coverage is a regression, not a pass.",
        run.corpus.sites.len(),
    );
    assert!(
        overload_set_commits >= MIN_OVERLOAD_SET_COMMITS,
        "only {overload_set_commits} commits went through the §1 keystone on a genuine \
         (≥ 2 candidate) overload set (floor {MIN_OVERLOAD_SET_COMMITS}); the rest rode FCS's \
         single-candidate arity shortcut, which does not exercise the matcher at all.",
    );
}

/// The inference types of a site's arguments, in order — the matcher's input.
/// Derived from the generator's own argument-shape table, so it cannot drift
/// from the source text it emitted.
fn arg_types(site: &Site) -> Vec<Ty> {
    site.arg_types
        .iter()
        .map(|canon| match canon.strip_suffix("[]") {
            Some(elem) => Ty::Array {
                elem: Box::new(Ty::named(elem)),
                rank: 1,
            },
            None => Ty::named(canon),
        })
        .collect()
}

/// The source line containing byte offset `at` (for failure messages).
fn source_line(src: &str, at: usize) -> &str {
    let start = src[..at].rfind('\n').map_or(0, |i| i + 1);
    let end = src[start..].find('\n').map_or(src.len(), |i| start + i);
    &src[start..end]
}

// ============================================================================
// OV-9(b) — the coverage measurement
// ============================================================================

/// Why a call site did not commit. Computed from the *public* matcher surface
/// (`instance_method_group` / `static_method_group` / `may_apply` /
/// `must_apply`), so the report reads the same primitives the engine does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Outcome {
    /// We committed, and FCS agrees (the differential proves the agreement).
    Committed,
    /// FCS did not cleanly resolve this call (it errored — no applicable
    /// candidate, an ambiguity, or an argument-type mismatch its
    /// single-`IsCandidate` shortcut elaborated anyway). Deferring is *correct
    /// agreement*, not a coverage loss, so these sit outside the denominator.
    FcsErrored,
    /// Our group construction declined: an incomplete base chain, an `Object`
    /// cap, a skipped member, a kind clash.
    GroupIncomplete,
    /// Some candidate might be curried (OV-6.1) — never fires for the C# corpus,
    /// but counted so its absence is *observed* rather than assumed.
    PossiblyCurried,
    /// ≥ 2 candidates survive `may_apply`, so FCS runs its 14-rule betterness
    /// ladder and we cannot. **This is OV-8's addressable market.**
    Betterness,
    /// A unique candidate survives `may_apply` but `must_apply` will not affirm
    /// it: it is applicable only through a type-directed conversion (widening /
    /// `op_Implicit`), an omitted optional, a `params` expansion we cannot
    /// affirm, or a generic/byref shape. **OV-8 does NOT recover these** — they
    /// need the affirmation side (§4.3) extended.
    Unaffirmable,
    /// The matcher *would* commit (a unique survivor, affirmed), but a gate above
    /// it declined: a byref/`out`, generic or constructor signature on the
    /// single-candidate path (§5), or an argument that never grounded.
    GateDeclined,
    /// No candidate survives `may_apply`, yet FCS resolved the call. The refuter
    /// over-rejected — a soundness violation the differential above fails on.
    NoSurvivor,
}

#[test]
#[ignore = "coverage measurement (OV-9(b)); run with --ignored --nocapture"]
fn coverage_report() {
    let run = run();
    let mut tally: HashMap<Outcome, usize> = HashMap::new();
    // What the betterness bucket is *made of*, so the OV-8 decision is informed
    // by shape and not just by count.
    let mut betterness_by_shape: HashMap<String, usize> = HashMap::new();
    // Commits on lines FCS *errored* on — FCS's single-`IsCandidate` shortcut
    // (§2.2), which the engine deliberately mirrors: the member is elaborated
    // (and hover/go-to-def work) though the call is ill-typed. Real coverage, but
    // outside the "resolution" denominator, so counted on its own.
    let mut commits_on_error_lines = 0usize;

    for site in &run.corpus.sites {
        let outcome = classify(&run, site);
        *tally.entry(outcome).or_default() += 1;
        if outcome == Outcome::FcsErrored && run.our_choice(site).is_some() {
            commits_on_error_lines += 1;
        }
        if outcome == Outcome::Betterness {
            *betterness_by_shape
                .entry(format!("{} @ {}", site.declaring, site.arg_tag))
                .or_default() += 1;
        }
    }

    let total = run.corpus.sites.len();
    let errored = tally.get(&Outcome::FcsErrored).copied().unwrap_or(0);
    let resolvable = total - errored;
    println!("\n=== OV-9(b) overload-engine coverage over the generated matrix ===");
    println!("call sites:                     {total}");
    println!(
        "FCS did not cleanly resolve:    {errored}  (we agree by deferring; \
         {commits_on_error_lines} of them we DO elaborate, mirroring FCS's \
         single-IsCandidate shortcut)"
    );
    println!("FCS cleanly resolved:           {resolvable}  <- the coverage denominator\n");
    let mut rows: Vec<_> = tally
        .iter()
        .filter(|(o, _)| **o != Outcome::FcsErrored)
        .collect();
    rows.sort();
    for (outcome, n) in rows {
        let pct = 100.0 * *n as f64 / resolvable.max(1) as f64;
        println!("  {outcome:<16?} {n:>5}  ({pct:5.1} %)");
    }
    let committed = tally.get(&Outcome::Committed).copied().unwrap_or(0);
    let betterness = tally.get(&Outcome::Betterness).copied().unwrap_or(0);
    let unaffirmable = tally.get(&Outcome::Unaffirmable).copied().unwrap_or(0);
    println!(
        "\ncommit rate: {:.1} % of the calls FCS cleanly resolves",
        100.0 * committed as f64 / resolvable.max(1) as f64
    );
    println!(
        "OV-8 (betterness) addresses at most {betterness} sites ({:.1} %) — the groups where \u{2265} 2 \
         candidates survive `may_apply`, so FCS runs its ladder and we cannot.\nThe affirmation \
         gap (`must_apply` will not affirm a lone survivor) accounts for {unaffirmable} ({:.1} %); \
         OV-8 does NOT recover those — they need §4.3 extended (type-directed conversions, \
         omitted optionals).",
        100.0 * betterness as f64 / resolvable.max(1) as f64,
        100.0 * unaffirmable as f64 / resolvable.max(1) as f64,
    );

    let mut shapes: Vec<_> = betterness_by_shape.into_iter().collect();
    shapes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    println!("\nbetterness-blocked sites, by argument shape:");
    let mut by_arg: HashMap<&str, usize> = HashMap::new();
    for (shape, n) in &shapes {
        let arg = shape.split(" @ ").nth(1).unwrap_or("?");
        *by_arg.entry(arg).or_default() += n;
    }
    let mut args: Vec<_> = by_arg.into_iter().collect();
    args.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    for (arg, n) in args {
        println!("  {n:>4}  {arg}");
    }
}

/// Classify one call site for the coverage report. **FCS's status comes first**:
/// a site FCS did not cleanly resolve is outside the coverage question entirely
/// (whatever we did there, we cannot be said to have "covered" a resolution FCS
/// never made — and where we *did* commit on such a line, it is FCS's own
/// single-`IsCandidate` shortcut, whose agreement direction 1 already checks).
fn classify(run: &Run, site: &Site) -> Outcome {
    if run.fcs_resolution(site).is_none() {
        return Outcome::FcsErrored;
    }
    if run.our_choice(site).is_some() {
        return Outcome::Committed;
    }
    let (Some(group), Some(handle)) = (run.group(site), run.receiver(site)) else {
        return Outcome::GroupIncomplete;
    };
    if group
        .iter()
        .any(|(_, m)| m.signature.parameters.len() >= 2 && m.arg_group_count != Some(1))
    {
        return Outcome::PossiblyCurried;
    }
    let declaring = run.env.entity(handle).assembly.name.clone();
    let args = arg_types(site);
    let survivors: Vec<_> = group
        .iter()
        .filter(|(_, m)| run.env.may_apply(m, &declaring, &args))
        .collect();
    match survivors.as_slice() {
        [] => Outcome::NoSurvivor,
        [(_, m)] if run.env.must_apply(m, &declaring, &args) => Outcome::GateDeclined,
        [_] => Outcome::Unaffirmable,
        _ => Outcome::Betterness,
    }
}
