//! Suffix irrelevance: what inference commits inside a prefix of a binding
//! must not depend on the text after it.
//!
//! Completion asks for types while the rest of the binding is unwritten or
//! broken, so it needs inference's answer for the text before the cursor to be
//! the answer FCS would give there, whatever follows. FCS takes its
//! order-dependent decisions at a point in its checking order, and a later
//! construct can only error or add to what was decided. Inference today
//! decides at the binding's end, once it knows the whole binding is complete:
//! an argument check in the prefix fires only when nothing after it is
//! unmodelled. These properties are what #662's left-to-right stages are
//! held to.
//!
//! # The programs
//!
//! Each base program `P` is a generated function over `infer_local_let_diff`'s
//! generator (well-typed by construction):
//!
//! ```text
//! let f (s: string) (b: bool) p =
//!     item1
//!     …
//!     itemN
//!     tail
//! ```
//!
//! where an item is a local `let` or a statement and the tail an expression,
//! an application of a two-argument function (`two (a) (b)`, `pick (a) (b)`),
//! or a tuple. A **cut** is a point in the body: before an item or the tail (a
//! statement boundary), before the application's last argument (a later
//! sibling argument), or before a tuple element after the first. Cutting `P`
//! there and replacing everything after the cut with a **hole** gives `P'`,
//! byte-identical to `P` up to the cut. The holes, neither of which mentions a
//! name the prefix binds, so neither can constrain it:
//!
//! - `(Unchecked.defaultof<_>)`, which FCS gives a fresh type variable and
//!   inference does not model, so the binding becomes incomplete;
//! - `(mono "s")`, ill-typed between two ground types.
//!
//! Every variant of a base is a top-level function of the same file, its name
//! as long as the base's, so a range in the prefix is the base's range moved
//! by the distance between the two functions.
//!
//! # The properties
//!
//! - [`fcs_graded_prefix_commits_survive_a_hole`]: every expression or binder
//!   type we commit in `P`'s prefix whose FCS type on `P'` is ground, we also
//!   commit on `P'`, the same. In words: a commit FCS can make from the prefix
//!   alone must not depend, for us, on the suffix.
//! - [`prefix_commits_agree_across_holes`] (no oracle): `P'` under the two holes
//!   commits the same in the prefix — expression and binder types, member
//!   resolutions, `(expr).Member` receiver types.
//! - [`prefix_accesses_agree_across_suffixes`] (no oracle): the member
//!   resolutions and receiver types inside the prefix are the same under the
//!   original suffix and both holes. FCS decides an access at the access.
//!   (Expression types are not compared against the original suffix: it may
//!   ground a variable the prefix left open, and FCS's typed tree then shows
//!   the prefix node at the later, final type.)
//!
//! The FCS-graded form fails today, and is `#[ignore]`d until #662's stage L2
//! makes it hold. Its smallest counterexample: `let f () = (idf uu, None)`
//! commits `idf uu : int * string`, an argument check that fires only in a
//! complete binding, and `(idf uu, Unchecked.defaultof<_>)` is incomplete
//! (`Unmodelled(TYPE_APP_EXPR)`), so the check never fires and the commit FCS
//! still makes is lost. The ill-typed hole loses it too, which is why the
//! comparison across holes holds today. Both oracle-free forms hold, and gate
//! as ordinary tests.
//!
//! Each asserts that its hole does what it is there for: the
//! `Unchecked.defaultof` hole must mark the binding incomplete, or the
//! property stops testing what an incomplete suffix does to the prefix once
//! that hole is modelled.
//!
//! [`the_harness_compares_like_with_like`] is the control: a renamed copy of
//! `P` in place of `P'` must pass every property with the whole body as the
//! prefix, so a failure above is about the suffix, not about how ranges are
//! matched.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile, SyntaxKind, TextRange};
use borzoi_sema::{
    AssemblyEnv, Incomplete, InferredFile, PrecedingTypes, ProjectItems, Resolution, ResolvedFile,
    SyntaxRecovery, infer_file, resolve_file,
};

use crate::common::{GradedDumps, env_usize_or, full_bcl_env, temp_fs_file, try_invoke_fcs_dump};
use crate::infer_local_let_diff::{
    Gen as LetGen, HEADER, Kind, Rng, T, TYPES, Var, header_vars, params,
};

/// The generated files' header: `infer_local_let_diff`'s, plus the
/// two-argument functions the application tail applies.
fn header() -> String {
    format!("{HEADER}let two (x: int) (y: string) = x\nlet pick x y = x\n")
}

/// The ill-typed hole.
const ILL_TYPED: &str = "(mono \"s\")";
/// The hole FCS gives a fresh type variable.
const DEFAULT_OF: &str = "(Unchecked.defaultof<_>)";

/// What replaces the text after a cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Suffix {
    /// The base's own text: `P` itself.
    Rest,
    DefaultOf,
    IllTyped,
}

impl Suffix {
    fn hole(self) -> &'static str {
        match self {
            Suffix::Rest => unreachable!("the original suffix is not a hole"),
            Suffix::DefaultOf => DEFAULT_OF,
            Suffix::IllTyped => ILL_TYPED,
        }
    }
}

/// Where a cut is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Cut {
    /// Before item `j` (or the tail, when `j` is the number of items); at
    /// least one item precedes it.
    Statement(usize),
    /// Before the application tail's second argument.
    Argument,
    /// Before the tuple tail's element `i`, `i >= 1`.
    TupleElement(usize),
    /// After the whole body: the control's cut, whose `P'` is `P` itself.
    End,
}

impl Cut {
    fn kind(self) -> &'static str {
        match self {
            Cut::Statement(_) => "statement",
            Cut::Argument => "argument",
            Cut::TupleElement(_) => "tuple-element",
            Cut::End => "end",
        }
    }
}

#[derive(Debug, Clone)]
enum Tail {
    Expr(String),
    App {
        fun: &'static str,
        a: String,
        b: String,
    },
    Tuple(Vec<String>),
}

/// A base program: its parameters, items and tail.
#[derive(Debug, Clone)]
struct Base {
    params: String,
    items: Vec<String>,
    tail: Tail,
}

/// Every function name in a file has this many characters, so moving between
/// functions moves every range by the same amount.
fn name(i: usize) -> String {
    format!("c{i:03}")
}

impl Base {
    fn head(&self, name: &str) -> String {
        format!("let {name} {} =", self.params)
    }

    fn items_text(&self, upto: usize) -> String {
        self.items[..upto]
            .iter()
            .map(|i| format!("\n    {i}"))
            .collect()
    }

    /// The tail's text up to `cut` (all of it for a statement cut or none),
    /// and what follows the cut in it, with each later element replaced by
    /// `hole` when one is given.
    fn tail_parts(&self, cut: Cut, hole: Option<&str>) -> (String, String) {
        match (&self.tail, cut) {
            (Tail::App { fun, a, b }, Cut::Argument) => (
                format!("{fun} ({a}) "),
                hole.map_or_else(|| format!("({b})"), str::to_string),
            ),
            (Tail::Tuple(elems), Cut::TupleElement(i)) => {
                let rest: Vec<String> = elems[i..]
                    .iter()
                    .map(|e| hole.map_or_else(|| e.clone(), str::to_string))
                    .collect();
                (
                    format!("({}, ", elems[..i].join(", ")),
                    format!("{})", rest.join(", ")),
                )
            }
            (tail, _) => (String::new(), Base::tail_text(tail)),
        }
    }

    fn tail_text(tail: &Tail) -> String {
        match tail {
            Tail::Expr(e) => e.clone(),
            Tail::App { fun, a, b } => format!("{fun} ({a}) ({b})"),
            Tail::Tuple(elems) => format!("({})", elems.join(", ")),
        }
    }

    /// The function's text for `cut` and `suffix`, and the offset of the cut
    /// in it. For [`Suffix::Rest`] the text is the base's whatever the cut.
    fn render(&self, name: &str, cut: Cut, suffix: Suffix) -> (String, usize) {
        let hole = (suffix != Suffix::Rest).then(|| suffix.hole());
        let mut text = self.head(name);
        match cut {
            Cut::Statement(j) => {
                text.push_str(&self.items_text(j));
                text.push_str("\n    ");
                let at = text.len();
                match hole {
                    Some(h) => text.push_str(h),
                    None => {
                        text.push_str(
                            &self.items[j..]
                                .iter()
                                .map(|i| format!("{i}\n    "))
                                .collect::<String>(),
                        );
                        text.push_str(&Base::tail_text(&self.tail));
                    }
                }
                text.push('\n');
                (text, at)
            }
            Cut::Argument | Cut::TupleElement(_) => {
                text.push_str(&self.items_text(self.items.len()));
                text.push_str("\n    ");
                let (before, after) = self.tail_parts(cut, hole);
                text.push_str(&before);
                let at = text.len();
                text.push_str(&after);
                text.push('\n');
                (text, at)
            }
            Cut::End => {
                text.push_str(&self.items_text(self.items.len()));
                text.push_str("\n    ");
                text.push_str(&Base::tail_text(&self.tail));
                let at = text.len();
                text.push('\n');
                (text, at)
            }
        }
    }

    /// Every cut this base admits, the control's excepted.
    fn cuts(&self) -> Vec<Cut> {
        let mut cuts: Vec<Cut> = (1..=self.items.len()).map(Cut::Statement).collect();
        match &self.tail {
            Tail::App { .. } => cuts.push(Cut::Argument),
            Tail::Tuple(elems) => cuts.extend((1..elems.len()).map(Cut::TupleElement)),
            Tail::Expr(_) => {}
        }
        cuts
    }
}

/// One base program. `modelled_pct` is the chance its items use only
/// constructs inference models, so that `P` is often complete: an incomplete
/// `P` fires no argument check, and has nothing for a hole to take away.
fn gen_base(rng: &mut Rng, modelled_pct: usize, plain_pct: usize) -> Base {
    let modelled_only = rng.chance(modelled_pct);
    // Plain items: ground value locals and `unit` statements only. The
    // generator's other locals (a generic `g = idf`, a pattern over an open
    // value) and its statements of other types each mark the binding
    // incomplete, and an incomplete base has nothing for a hole to take away.
    let plain = modelled_only && rng.chance(plain_pct);
    let with_open = !plain && rng.chance(50);
    let (_, params) = params(rng, with_open, false);
    let mut env = header_vars();
    env.extend([
        Var {
            name: "s".into(),
            kind: Kind::Mono(T::Str),
        },
        Var {
            name: "b".into(),
            kind: Kind::Mono(T::Bool),
        },
    ]);
    if with_open {
        env.push(Var {
            name: "p".into(),
            kind: Kind::Open,
        });
    }
    let mut g = LetGen {
        rng,
        env,
        next: 0,
        generic_uses: Vec::new(),
        modelled_only,
        open_anywhere: false,
        under_coercion: false,
    };
    let n = 1 + g.rng.below(3);
    let mut items = Vec::new();
    for k in 0..n {
        if plain {
            // Shallow, mostly: a local whose RHS waits on an argument check is
            // open where it is bound, which marks the binding incomplete.
            let depth = usize::from(g.rng.chance(30));
            if g.rng.chance(70) {
                let t = TYPES[g.rng.below(TYPES.len())];
                let rhs = g.expr(t, depth);
                let name = format!("u{k}");
                g.env.push(Var {
                    name: name.clone(),
                    kind: Kind::Mono(t),
                });
                items.push(format!("let {name} = {rhs}"));
            } else {
                items.push(g.expr(T::Unit, depth));
            }
        } else if g.rng.chance(60) {
            items.push(format!("let {}", g.binding(2)));
        } else {
            items.push(g.statement(2));
        }
    }
    let tail = match g.rng.below(3) {
        0 => {
            let t = TYPES[g.rng.below(TYPES.len())];
            Tail::Expr(g.expr(t, 2))
        }
        1 => {
            if g.rng.chance(50) {
                Tail::App {
                    fun: "two",
                    a: g.expr(T::Int, 2),
                    b: g.expr(T::Str, 2),
                }
            } else {
                let (ta, tb) = (
                    TYPES[g.rng.below(TYPES.len())],
                    TYPES[g.rng.below(TYPES.len())],
                );
                Tail::App {
                    fun: "pick",
                    a: g.expr(ta, 2),
                    b: g.expr(tb, 2),
                }
            }
        }
        _ => {
            let k = 2 + g.rng.below(2);
            Tail::Tuple(
                (0..k)
                    .map(|_| {
                        let t = TYPES[g.rng.below(TYPES.len())];
                        g.expr(t, 2)
                    })
                    .collect(),
            )
        }
    };
    Base {
        params,
        items,
        tail,
    }
}

/// One function of a generated file.
#[derive(Debug, Clone)]
struct Fun {
    /// Its offset in the file.
    start: usize,
    text: String,
    /// The offset, within the function, of its body's first character and
    /// of its cut.
    body: usize,
    cut_at: usize,
    base: usize,
    cut: Cut,
    suffix: Suffix,
}

/// A generated file: the header, then each base followed by its variants.
struct Program {
    src: String,
    funs: Vec<Fun>,
    bases: Vec<Base>,
}

impl Program {
    /// `bases`, each with a variant per cut and hole, and the control's copy.
    fn build(bases: Vec<Base>, with_holes: bool) -> Program {
        let mut src = header();
        let mut funs = Vec::new();
        for (b, base) in bases.iter().enumerate() {
            let mut variants = vec![(Cut::End, Suffix::Rest), (Cut::End, Suffix::Rest)];
            if with_holes {
                for cut in base.cuts() {
                    for suffix in [Suffix::DefaultOf, Suffix::IllTyped] {
                        variants.push((cut, suffix));
                    }
                }
            }
            for (cut, suffix) in variants {
                let n = name(funs.len());
                let (text, cut_at) = base.render(&n, cut, suffix);
                funs.push(Fun {
                    start: src.len(),
                    body: base.head(&n).len(),
                    text: text.clone(),
                    cut_at,
                    base: b,
                    cut,
                    suffix,
                });
                src.push_str(&text);
            }
        }
        Program { src, funs, bases }
    }

    /// The base function of base `b`: the first of its two copies.
    fn base_fun(&self, b: usize) -> &Fun {
        self.funs
            .iter()
            .find(|f| f.base == b)
            .expect("every base has a function")
    }

    /// The control's copy of base `b`.
    fn copy_fun(&self, b: usize) -> &Fun {
        self.funs
            .iter()
            .filter(|f| f.base == b)
            .nth(1)
            .expect("every base has a copy")
    }
}

/// What inference committed, by range relative to a function's start.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Commits {
    types: BTreeMap<(usize, usize), String>,
    binders: BTreeMap<(usize, usize), String>,
    members: BTreeMap<(usize, usize), String>,
    receivers: BTreeMap<(usize, usize), String>,
}

/// One file's inference, with every commit by absolute range.
struct Inferred {
    all: Commits,
    /// Each function's incompleteness census entry, in function order.
    census: Vec<Vec<Incomplete>>,
}

fn span(r: TextRange) -> (usize, usize) {
    (u32::from(r.start()) as usize, u32::from(r.end()) as usize)
}

fn spell_resolution(env: &AssemblyEnv, res: Resolution) -> String {
    match res {
        Resolution::Member { parent, idx } => format!(
            "{}#{idx:?} {}",
            env.entity_full_name(parent),
            env.member_display_name(parent, idx)
        ),
        other => format!("{other:?}"),
    }
}

/// How many census entries the header's bindings take, ahead of the
/// generated functions'.
fn header_census_len() -> usize {
    static LEN: OnceLock<usize> = OnceLock::new();
    *LEN.get_or_init(|| {
        let (_, _, inferred) = infer_src(&header());
        inferred.incompleteness().len()
    })
}

fn infer_src(src: &str) -> (ImplFile, ResolvedFile, InferredFile) {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "a generated file parses cleanly: {:?}\n{src}",
        parsed.errors
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = full_bcl_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let inferred = infer_file(&file, &resolved, env, &PrecedingTypes::default());
    (file, resolved, inferred)
}

fn infer_program(program: &Program) -> Inferred {
    let env = full_bcl_env();
    let (file, resolved, inferred) = infer_src(&program.src);
    let mut all = Commits::default();
    for (r, t) in inferred.types() {
        all.types.insert(span(*r), t.render(env));
    }
    for (d, t) in inferred.def_types() {
        all.binders
            .insert(span(resolved.def(*d).range), t.render(env));
    }
    for (r, res) in inferred.member_resolutions() {
        all.members.insert(span(*r), spell_resolution(env, *res));
    }
    for node in file
        .syntax()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::DOT_GET_EXPR)
    {
        if let Some(t) = inferred.dot_get_receiver_type(node.text_range()) {
            all.receivers.insert(span(node.text_range()), t.render(env));
        }
    }
    let census = inferred.incompleteness();
    let h = header_census_len();
    assert_eq!(
        census.len(),
        h + program.funs.len(),
        "one census entry per header binding and per generated function\n{}",
        program.src
    );
    Inferred {
        all,
        census: census[h..].to_vec(),
    }
}

impl Commits {
    /// The commits of `fun` whose range lies in `[lo, hi)` of the function,
    /// keyed relative to its start.
    fn region(&self, fun: &Fun, lo: usize, hi: usize) -> Commits {
        let pick = |m: &BTreeMap<(usize, usize), String>| {
            m.iter()
                .filter(|((s, e), _)| *s >= fun.start + lo && *e <= fun.start + hi)
                .map(|((s, e), t)| ((s - fun.start, e - fun.start), t.clone()))
                .collect()
        };
        Commits {
            types: pick(&self.types),
            binders: pick(&self.binders),
            members: pick(&self.members),
            receivers: pick(&self.receivers),
        }
    }
}

/// A broken property: a commit in a prefix one side made and the other did
/// not make, or made differently.
#[derive(Debug, Clone)]
struct Violation {
    category: &'static str,
    cut: Cut,
    left: Suffix,
    right: Suffix,
    base_text: String,
    variant_text: String,
    range: (usize, usize),
    slice: String,
    left_commit: Option<String>,
    right_commit: Option<String>,
}

impl Violation {
    fn show(&self) -> String {
        format!(
            "{} at a {} cut ({:?}), {:?} vs {:?}: `{}` at {:?} committed {:?} vs {:?}\n--- base ---\n{}--- variant ---\n{}",
            self.category,
            self.cut.kind(),
            self.cut,
            self.left,
            self.right,
            self.slice,
            self.range,
            self.left_commit,
            self.right_commit,
            self.base_text,
            self.variant_text
        )
    }
}

/// Compare the commits of `left` and `right` in one table, in the region both
/// share, recording each key on which they differ.
#[allow(clippy::too_many_arguments)]
fn compare_table(
    category: &'static str,
    l: &BTreeMap<(usize, usize), String>,
    r: &BTreeMap<(usize, usize), String>,
    left: &Fun,
    right: &Fun,
    base: &Fun,
    out: &mut Vec<Violation>,
) {
    let keys: std::collections::BTreeSet<_> = l.keys().chain(r.keys()).copied().collect();
    for k in keys {
        let (a, b) = (l.get(&k), r.get(&k));
        if a != b {
            out.push(Violation {
                category,
                cut: right.cut,
                left: left.suffix,
                right: right.suffix,
                base_text: base.text.clone(),
                variant_text: right.text.clone(),
                range: k,
                slice: left.text[k.0..k.1].to_string(),
                left_commit: a.cloned(),
                right_commit: b.cloned(),
            });
        }
    }
}

/// The violations to print: the smallest programs first, a few of each
/// category and cut kind, and a count of all of them by category and cut.
fn report(label: &str, violations: &mut [Violation]) -> String {
    violations.sort_by_key(|v| (v.variant_text.len() + v.base_text.len(), v.range));
    let mut by: BTreeMap<(&str, &str, Suffix, Suffix), usize> = BTreeMap::new();
    for v in violations.iter() {
        *by.entry((v.category, v.cut.kind(), v.left, v.right))
            .or_default() += 1;
    }
    let mut out = format!("{label}: {} violations\n", violations.len());
    for ((cat, cut, l, r), n) in &by {
        out.push_str(&format!("  {cat:<10} {cut:<14} {l:?} vs {r:?}: {n}\n"));
    }
    let mut shown: HashMap<(&str, &str), usize> = HashMap::new();
    for v in violations.iter() {
        let n = shown.entry((v.category, v.cut.kind())).or_default();
        if *n < 2 {
            *n += 1;
            out.push_str(&format!("\n{}", v.show()));
        }
    }
    out
}

/// Bases per generated file.
const BASES_PER_FILE: usize = 2;

/// The bases of file `seed`, the chance of a modelled-only base itself drawn
/// per file so the sweep covers both regimes.
fn bases_for(seed: u64) -> Vec<Base> {
    let mut rng = Rng(seed ^ 0x5eed_c0de_cafe);
    let modelled_pct = [50, 75, 90][rng.below(3)];
    let plain_pct = [30, 60, 90][rng.below(3)];
    (0..BASES_PER_FILE)
        .map(|_| gen_base(&mut rng, modelled_pct, plain_pct))
        .collect()
}

/// The non-vacuity tally every property keeps: each kind of cut reached, each
/// base complete or not, and each `Unchecked.defaultof` hole marking its
/// binding incomplete.
#[derive(Debug, Default)]
struct Tally {
    cuts: BTreeMap<&'static str, usize>,
    complete_bases: usize,
    bases: usize,
    /// How many incomplete bases have each census reason.
    base_reasons: BTreeMap<String, usize>,
    /// Prefix commits a property compared.
    compared: BTreeMap<&'static str, usize>,
    /// `Unchecked.defaultof` variants whose census entry is empty.
    complete_default_holes: Vec<String>,
    /// Ill-typed variants whose census entry is empty.
    complete_ill_typed_holes: usize,
    ill_typed_holes: usize,
}

impl Tally {
    fn see(&mut self, program: &Program, inferred: &Inferred) {
        for (i, fun) in program.funs.iter().enumerate() {
            let complete = inferred.census[i].is_empty();
            match fun.suffix {
                Suffix::Rest
                    if fun.cut == Cut::End && program.base_fun(fun.base).start == fun.start =>
                {
                    self.bases += 1;
                    self.complete_bases += usize::from(complete);
                    let reasons: std::collections::BTreeSet<String> = inferred.census[i]
                        .iter()
                        .map(|r| format!("{r:?}"))
                        .collect();
                    if std::env::var_os("BORZOI_SUFFIX_SHOW_INCOMPLETE").is_some() && !complete {
                        eprintln!("incomplete {reasons:?}:\n{}", fun.text);
                    }
                    for r in reasons {
                        *self.base_reasons.entry(r).or_default() += 1;
                    }
                }
                Suffix::Rest => {}
                Suffix::DefaultOf => {
                    *self.cuts.entry(fun.cut.kind()).or_default() += 1;
                    if complete {
                        self.complete_default_holes.push(fun.text.clone());
                    }
                }
                Suffix::IllTyped => {
                    self.ill_typed_holes += 1;
                    self.complete_ill_typed_holes += usize::from(complete);
                }
            }
        }
    }

    /// The hole must mark the binding incomplete, and the sweep must reach
    /// every kind of cut and both complete and incomplete bases.
    fn assert_live(&self, min: usize) {
        assert!(
            self.complete_default_holes.is_empty(),
            "an `Unchecked.defaultof` hole left its binding complete, so the property no \
             longer tests what an incomplete suffix does to the prefix; pick a hole \
             inference does not model:\n{}",
            self.complete_default_holes.join("\n")
        );
        for kind in ["statement", "argument", "tuple-element"] {
            let n = self.cuts.get(kind).copied().unwrap_or(0);
            assert!(n >= min, "only {n} {kind} cuts: {self:?}");
        }
        assert!(
            self.complete_bases >= min && self.bases - self.complete_bases >= min,
            "the bases should be both complete and incomplete in quantity: {self:?}"
        );
    }
}

/// The FCS-graded form over `files` generated files: its violations and tally.
fn fcs_graded(files: usize, with_holes: bool) -> (Vec<Violation>, Tally, usize) {
    let mut violations = Vec::new();
    let mut tally = Tally::default();
    let mut uncheckable = 0;
    for seed in 0..files as u64 {
        let program = Program::build(bases_for(seed), with_holes);
        let inferred = infer_program(&program);
        tally.see(&program, &inferred);
        let path = temp_fs_file("infer_suffix", &program.src);
        let jsons = try_invoke_fcs_dump("types", &path)
            .and_then(|t| try_invoke_fcs_dump("binder-types", &path).map(|b| (t, b)));
        let _ = std::fs::remove_file(&path);
        let Ok((types_json, binders_json)) = jsons else {
            uncheckable += 1;
            continue;
        };
        let file = ImplFile::cast(parse(&program.src).root);
        let Some(dumps) =
            GradedDumps::read(&types_json, &binders_json, &program.src, file.as_ref())
        else {
            uncheckable += 1;
            continue;
        };
        // The bases and their copies are well-typed: an error on one of their
        // lines means the generator is wrong.
        let line_of = |offset: usize| program.src[..offset].matches('\n').count() as u32 + 1;
        for fun in program.funs.iter().filter(|f| f.suffix == Suffix::Rest) {
            let (l0, l1) = (line_of(fun.start), line_of(fun.start + fun.text.len() - 1));
            let errors: Vec<_> = dumps
                .errors
                .iter()
                .filter(|e| (l0..=l1).contains(&e.line))
                .collect();
            assert!(
                errors.is_empty(),
                "the generator produced a function FCS rejects: {errors:?}\n{}",
                fun.text
            );
        }
        for (b, _) in program.bases.iter().enumerate() {
            let base = program.base_fun(b);
            let targets: Vec<&Fun> = program
                .funs
                .iter()
                .filter(|f| f.base == b && f.start != base.start)
                .collect();
            for target in targets {
                let (lo, hi) = (base.body, target.cut_at);
                let ours_p = inferred.all.region(base, lo, hi);
                let ours_v = inferred.all.region(target, lo, hi);
                for (category, p, v, fcs) in [
                    ("type", &ours_p.types, &ours_v.types, &dumps.types),
                    ("binder", &ours_p.binders, &ours_v.binders, &dumps.binders),
                ] {
                    for (&(s, e), ty) in p {
                        let at = (target.start + s, target.start + e);
                        if dumps.is_ungraded(at) {
                            continue;
                        }
                        let Some(theirs) = fcs.get(&at) else {
                            continue;
                        };
                        if theirs.contains('\'') {
                            continue;
                        }
                        *tally.compared.entry(category).or_default() += 1;
                        if v.get(&(s, e)) != Some(ty) {
                            violations.push(Violation {
                                category,
                                cut: target.cut,
                                left: Suffix::Rest,
                                right: target.suffix,
                                base_text: base.text.clone(),
                                variant_text: target.text.clone(),
                                range: (s, e),
                                slice: base.text[s..e].to_string(),
                                left_commit: Some(ty.clone()),
                                right_commit: v.get(&(s, e)).cloned(),
                            });
                        }
                    }
                }
            }
        }
    }
    (violations, tally, uncheckable)
}

/// How many files the FCS-graded form checks.
fn fcs_files() -> usize {
    env_usize_or("BORZOI_SUFFIX_FILES", 40)
}

/// A commit FCS can determine from the prefix alone does not depend, for us,
/// on the suffix.
///
/// Fails on main: either hole switches off the argument checks of the whole
/// binding, prefix included (the `Unchecked.defaultof` hole by marking it
/// incomplete), and so the prefix commits that a complete binding's checks
/// ground disappear (see the module docs for the smallest case). Stage L2 of
/// the plan for #662 (argument checks decided at the argument, gated on a
/// prefix-completeness flag) turns it into a gate.
#[test]
#[ignore = "fails on main; becomes a gate at #662's stage L2"]
fn fcs_graded_prefix_commits_survive_a_hole() {
    let (mut violations, tally, uncheckable) = fcs_graded(fcs_files(), true);
    eprintln!("suffix irrelevance (FCS-graded): {tally:?}, uncheckable files: {uncheckable}");
    assert!(
        uncheckable * 4 <= fcs_files(),
        "FCS could not check {uncheckable} files"
    );
    tally.assert_live(10);
    assert!(
        tally.compared.values().sum::<usize>() >= 200,
        "too few prefix commits had a ground FCS type to be evidence: {tally:?}"
    );
    assert!(
        violations.is_empty(),
        "{}",
        report("FCS-graded", &mut violations)
    );
}

/// The oracle-free forms over `files` generated files: violations of
/// [`prefix_commits_agree_across_holes`] and of
/// [`prefix_accesses_agree_across_suffixes`], and the tally.
fn oracle_free(files: usize, with_holes: bool) -> (Vec<Violation>, Vec<Violation>, Tally) {
    let (mut holes, mut accesses) = (Vec::new(), Vec::new());
    let mut tally = Tally::default();
    for seed in 0..files as u64 {
        let program = Program::build(bases_for(seed ^ 0xf0f0), with_holes);
        let inferred = infer_program(&program);
        tally.see(&program, &inferred);
        for (b, _) in program.bases.iter().enumerate() {
            let base = program.base_fun(b);
            // The control: the base's copy against the base, over the whole
            // body.
            let copy = program.copy_fun(b);
            let (lo, hi) = (base.body, base.cut_at);
            let (p, c) = (
                inferred.all.region(base, lo, hi),
                inferred.all.region(copy, lo, hi),
            );
            for (cat, l, r) in [
                ("type", &p.types, &c.types),
                ("binder", &p.binders, &c.binders),
                ("member", &p.members, &c.members),
                ("receiver", &p.receivers, &c.receivers),
            ] {
                *tally.compared.entry("control").or_default() += l.len();
                compare_table(cat, l, r, base, copy, base, &mut holes);
            }
            let mut by_cut: BTreeMap<Cut, Vec<&Fun>> = BTreeMap::new();
            for f in program
                .funs
                .iter()
                .filter(|f| f.base == b && f.suffix != Suffix::Rest)
            {
                by_cut.entry(f.cut).or_default().push(f);
            }
            for (_, variants) in by_cut {
                let [default_of, ill_typed] = variants[..] else {
                    panic!("two holes per cut")
                };
                let (lo, hi) = (base.body, default_of.cut_at);
                let d = inferred.all.region(default_of, lo, hi);
                let i = inferred.all.region(ill_typed, lo, hi);
                let p = inferred.all.region(base, lo, hi);
                for (cat, l, r) in [
                    ("type", &d.types, &i.types),
                    ("binder", &d.binders, &i.binders),
                    ("member", &d.members, &i.members),
                    ("receiver", &d.receivers, &i.receivers),
                ] {
                    *tally.compared.entry(cat).or_default() += l.len().max(r.len());
                    compare_table(cat, l, r, default_of, ill_typed, base, &mut holes);
                }
                for hole in [default_of, ill_typed] {
                    let h = inferred.all.region(hole, lo, hi);
                    for (cat, l, r) in [
                        ("member", &p.members, &h.members),
                        ("receiver", &p.receivers, &h.receivers),
                    ] {
                        *tally.compared.entry("access").or_default() += l.len().max(r.len());
                        compare_table(cat, l, r, base, hole, base, &mut accesses);
                    }
                }
            }
        }
    }
    (holes, accesses, tally)
}

/// How many files the oracle-free forms check.
fn oracle_free_files() -> usize {
    env_usize_or("BORZOI_SUFFIX_FILES", 40) * 5
}

/// The two holes, neither of which constrains the prefix, leave the same
/// commits in it: expression and binder types, member resolutions and
/// receiver types.
///
/// It holds today because both holes switch off the prefix's argument checks
/// alike; #662's stage L2, which decides each check at its argument, must
/// keep it, now because neither does.
#[test]
fn prefix_commits_agree_across_holes() {
    let (mut holes, _, tally) = oracle_free(oracle_free_files(), true);
    eprintln!("suffix irrelevance across holes: {tally:?}");
    tally.assert_live(50);
    assert!(
        tally.compared.get("type").copied().unwrap_or(0) >= 500,
        "too few prefix types compared to be evidence: {tally:?}"
    );
    assert!(holes.is_empty(), "{}", report("across holes", &mut holes));
}

/// The member resolutions and receiver types inside the prefix are the same
/// under the original suffix and under either hole: FCS decides an access at
/// the access.
///
/// It holds today on these programs, which are well-typed, so every access
/// FCS resolves has a receiver already known at the access. #662's stage L3,
/// which decides each access at the access and records its receiver there,
/// must keep it.
#[test]
fn prefix_accesses_agree_across_suffixes() {
    let (_, mut accesses, tally) = oracle_free(oracle_free_files(), true);
    eprintln!("suffix irrelevance of accesses: {tally:?}");
    tally.assert_live(50);
    assert!(
        tally.compared.get("access").copied().unwrap_or(0) >= 100,
        "too few prefix accesses compared to be evidence: {tally:?}"
    );
    assert!(accesses.is_empty(), "{}", report("accesses", &mut accesses));
}

/// The control: with a renamed copy of each base in place of a cut variant,
/// every property holds over the whole body, FCS-graded and not. A failure of
/// the properties above is then about the suffix, not about how ranges are
/// matched between functions.
#[test]
fn the_harness_compares_like_with_like() {
    let files = env_usize_or("BORZOI_SUFFIX_FILES", 40) / 4;
    let (mut graded, tally, uncheckable) = fcs_graded(files.max(4), false);
    assert_eq!(uncheckable, 0, "FCS checks every control file");
    assert!(
        graded.is_empty(),
        "{}",
        report("control, FCS-graded", &mut graded)
    );
    assert!(
        tally.compared.values().sum::<usize>() >= 50,
        "the control compared too few commits to be evidence: {tally:?}"
    );
    let (mut holes, accesses, tally) = oracle_free(files.max(4) * 4, false);
    assert!(holes.is_empty(), "{}", report("control", &mut holes));
    assert!(accesses.is_empty());
    assert!(
        tally.compared.get("control").copied().unwrap_or(0) >= 100,
        "the control compared too few commits to be evidence: {tally:?}"
    );
}

/// The generated programs parse, and a cut variant's text is its base's up to
/// the cut.
#[test]
fn every_variant_shares_its_base_text_up_to_the_cut() {
    for seed in 0..64 {
        let program = Program::build(bases_for(seed), true);
        let parsed = parse(&program.src);
        assert!(
            parsed.errors.is_empty(),
            "{:?}\n{}",
            parsed.errors,
            program.src
        );
        for fun in &program.funs {
            let base = program.base_fun(fun.base);
            assert_eq!(
                base.text[base.body..fun.cut_at],
                fun.text[fun.body..fun.cut_at],
                "{:?}\n{}\n{}",
                fun.cut,
                base.text,
                fun.text
            );
            assert_eq!(base.body, fun.body);
        }
    }
}
