//! Differential test for expression-level `let … in` and `e1; e2` in
//! [`borzoi_sema::infer_file`], against both FCS typed-tree oracles: `types`
//! (every expression node) and `binder-types` (every binder, local `let`s
//! included).
//!
//! # What is compared
//!
//! Soundness (D5), in the certain-implies-exact direction: every expression type
//! and every binder type we commit, FCS reports identically at the same range.
//! Deferring is always allowed. The generated programs are well-typed by
//! construction, so FCS must report no errors on them; an error means the
//! generator is wrong, and the sweep says so rather than grading a recovered
//! tree.
//!
//! # The hazard the sweep is built around
//!
//! A local binder is not monomorphic in F#. FCS generalises an eligible local
//! (`let local = idf` is `'a -> 'a`), so one inference variable shared by every
//! use is *wrong* for it: in `(local 1, local "s")` the first use would ground
//! the variable to `int` and the second use's result would read back `int`
//! where FCS says `string`. Inference only types a local whose RHS is ground on
//! its own, before the continuation is walked, and marks the binding incomplete
//! for any other. The generator therefore binds in-file generic functions and
//! lambdas to locals and applies them at several types, so a rule that lets a
//! use ground an open local fails here rather than in review.
//!
//! # Vacuity
//!
//! Every assertion above passes on an implementation that commits nothing. The
//! sweep's floors (committed local binders, committed sequential nodes, and the
//! generator's own count of generic locals applied at two types) are what make a
//! green run evidence. They are one-sided: they fail if the sweep stops
//! measuring, not if inference gets better.

use std::collections::HashSet;

use crate::common::{
    full_bcl_env, invoke_fcs_dump, parse_fcs_binder_types_with_errors, parse_fcs_types_with_errors,
    temp_fs_file,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, Expr, ImplFile, LetOrUseExpr, Pat, SequentialExpr, SyntaxNode};
use borzoi_sema::{ProjectItems, SyntaxRecovery, infer_file, resolve_file};

/// What one checked file committed, for the curated cases' expectations and
/// the sweep's floors.
#[derive(Debug, Default, Clone, Copy)]
struct Committed {
    /// Expression types committed (every one matched FCS).
    exprs: usize,
    /// Binder types committed (every one matched FCS).
    binders: usize,
    /// Of `binders`, how many are expression-level `let` locals.
    local_binders: usize,
    /// Of `exprs`, how many sit at a whole `e1; e2` sequence.
    sequentials: usize,
    /// Errors FCS reported on the file — nonzero only where the caller allowed it.
    fcs_errors: usize,
}

impl std::ops::AddAssign for Committed {
    fn add_assign(&mut self, o: Committed) {
        self.exprs += o.exprs;
        self.binders += o.binders;
        self.local_binders += o.local_binders;
        self.sequentials += o.sequentials;
        self.fcs_errors += o.fcs_errors;
    }
}

/// The half-open byte range of `node` without its leading and trailing trivia —
/// the span FCS gives an expression.
fn trimmed_range(node: &SyntaxNode) -> Option<(usize, usize)> {
    let mut toks = node
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia() && !t.text_range().is_empty());
    let first = toks.next()?;
    let last = toks.last().unwrap_or_else(|| first.clone());
    Some((
        u32::from(first.text_range().start()) as usize,
        u32::from(last.text_range().end()) as usize,
    ))
}

/// Resolve, infer and diff `source` against both oracles. `expect_clean`
/// demands FCS report no errors (the generator's contract); curated ill-typed
/// cases pass `false`, and the comparison is then strict in the other
/// direction — a committed type must still have an FCS record, and match it.
fn check(source: &str, expect_clean: bool) -> Committed {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors: {:?}\n{source}",
        parsed.errors
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = full_bcl_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let inferred = infer_file(&file, &resolved, env);

    let path = temp_fs_file("infer_local_let", source);
    let types_json = invoke_fcs_dump("types", &path);
    let binders_json = invoke_fcs_dump("binder-types", &path);
    let _ = std::fs::remove_file(&path);
    let (fcs_types, errors) = parse_fcs_types_with_errors(&types_json, source);
    let (fcs_binders, _) = parse_fcs_binder_types_with_errors(&binders_json, source);
    if expect_clean {
        assert!(
            errors.is_empty(),
            "the generator produced a program FCS rejects: {errors:?}\n{source}"
        );
    }

    let local_binder_ranges: HashSet<(usize, usize)> = file
        .syntax()
        .descendants()
        .filter_map(LetOrUseExpr::cast)
        .flat_map(|e| e.bindings().collect::<Vec<_>>())
        .filter_map(|b| match b.pat()? {
            Pat::Named(n) => n.ident(),
            _ => None,
        })
        .map(|t| {
            (
                u32::from(t.text_range().start()) as usize,
                u32::from(t.text_range().end()) as usize,
            )
        })
        .collect();
    let sequential_ranges: HashSet<(usize, usize)> = file
        .syntax()
        .descendants()
        .filter(|n| SequentialExpr::cast(n.clone()).is_some())
        .filter_map(|n| trimmed_range(&n))
        .collect();

    let mut c = Committed {
        fcs_errors: errors.len(),
        ..Committed::default()
    };
    for (range, ty) in inferred.types() {
        let key = (
            u32::from(range.start()) as usize,
            u32::from(range.end()) as usize,
        );
        let ours = ty.render();
        let theirs = fcs_types.get(&key).unwrap_or_else(|| {
            panic!(
                "we inferred `{ours}` at {key:?} (`{}`) but FCS reports no node there\n{source}",
                &source[key.0..key.1]
            )
        });
        assert_eq!(
            &ours,
            theirs,
            "expression type mismatch at {key:?} (`{}`)\n{source}",
            &source[key.0..key.1]
        );
        c.exprs += 1;
        if sequential_ranges.contains(&key) {
            c.sequentials += 1;
        }
    }
    for (def_id, ty) in inferred.def_types() {
        let def = resolved.def(*def_id);
        let key = (
            u32::from(def.range.start()) as usize,
            u32::from(def.range.end()) as usize,
        );
        let ours = ty.render();
        let theirs = fcs_binders.get(&key).unwrap_or_else(|| {
            panic!(
                "we inferred `{ours}` for binder `{}` at {key:?} but FCS reports no binder \
                 there\n{source}",
                def.name
            )
        });
        assert_eq!(
            &ours, theirs,
            "binder type mismatch for `{}` at {key:?}\n{source}",
            def.name
        );
        c.binders += 1;
        if local_binder_ranges.contains(&key) {
            c.local_binders += 1;
        }
    }
    c
}

/// A local bound to a member access types from its RHS, and so does its use:
/// `y : int` at the binder, and the body's `y` node.
#[test]
fn a_local_types_from_its_rhs() {
    let c = check(
        "module M\nlet f (s: string) =\n    let y = s.Length\n    y\n",
        true,
    );
    assert_eq!(c.local_binders, 1, "{c:?}");
}

/// Nested inline `let … in`: both locals commit, and the tuple built from them.
#[test]
fn nested_inline_lets_commit_each_local() {
    let c = check(
        "module M\nlet k () = let a = 1 in let b = \"s\" in (a, b)\n",
        true,
    );
    assert_eq!(c.local_binders, 2, "{c:?}");
}

/// A value binding whose RHS is a `let … in` types through it, and the
/// enclosing function still generalises over a parameter the local ignores.
#[test]
fn a_ground_local_leaves_the_enclosing_function_generalisable() {
    let c = check(
        "module M\nlet v = (let a = 1 in a)\nlet f x = let y = \"s\" in (x, y)\n",
        true,
    );
    // `a`, `v`, `y`, and `f : 'a -> 'a * string`.
    assert_eq!(c.binders, 4, "{c:?}");
}

/// The generalised-local hazard. FCS types `local` as `'a -> 'a` and the two
/// applications as `int` and `string`; a shared monomorphic variable would read
/// the second back as `int`. Whatever we commit must agree, and the local
/// itself (open at its binding) must not be published.
///
/// The parameter is a modelled one on purpose: a `()` parameter would make the
/// binding incomplete on its own, and then no argument check could fire to
/// ground the local whatever rule this case exists to pin.
#[test]
fn a_generalisable_local_applied_at_two_types_is_not_monomorphised() {
    let c = check(
        "module M\nlet idf x = x\nlet g (b: bool) =\n    let local = idf\n    (local 1, local \"s\")\n",
        true,
    );
    assert_eq!(c.local_binders, 0, "{c:?}");
}

/// A statement forces an open expression to `unit`: FCS types `x` as `unit`
/// here and reports `mono x` as the error. The statement's unit relation is
/// unmodelled, so `h` must not be published as `bool -> int` through the later
/// argument check.
#[test]
fn a_statement_on_an_open_parameter_blocks_the_argument_wake() {
    let c = check(
        "module M\nlet mono (b: bool) = 1\nlet h x =\n    x\n    mono x\n",
        false,
    );
    // Only `mono : bool -> int` is committed.
    assert_eq!(c.binders, 1, "{c:?}");
}

/// A sequence types as its last statement; a non-unit statement is not
/// emitted (FCS wraps it in a synthetic `unit` node at its own range).
#[test]
fn a_sequence_types_as_its_last_statement() {
    let c = check(
        "module M\nlet q (s: string) = (s.Length; \"t\")\nlet r (s: string) =\n    s.Length\n    let z = 3\n    (z, \"u\")\n",
        true,
    );
    assert_eq!(c.sequentials, 2, "{c:?}");
    assert_eq!(c.local_binders, 1, "{c:?}");
}

/// A method call FCS rejects (here, on arity) keeps nothing inside it: no
/// expression node and no binder. A local declared in its argument is walked,
/// so it must fall under the same barrier as the argument's nodes.
#[test]
fn a_local_inside_a_rejected_method_call_is_not_published() {
    let c = check(
        "module M\nlet f (b: bool) = \"s\".ToLowerInvariant(let y = 1 in y)\n",
        false,
    );
    assert_eq!(c.local_binders, 0, "{c:?}");
}

/// A statement fixes an open parameter to `unit` *before* a later condition
/// sees it: FCS types `h` as `unit -> int` and reports the condition. The
/// dropped unit relation must also stop the condition from grounding the
/// parameter's slot, or `h` publishes as `bool -> int`.
#[test]
fn a_statement_fixes_a_parameter_before_a_later_condition() {
    let c = check(
        "module M\nlet h x =\n    x\n    if x then 1 else 2\n",
        false,
    );
    assert_eq!(c.binders, 0, "{c:?}");
}

/// A condition *before* the statement is FCS's first constraint on the
/// parameter, so the slot it grounds is right: `h : bool -> int`, with only a
/// warning on the non-unit statement.
#[test]
fn a_condition_before_the_statement_still_grounds_the_parameter() {
    let c = check(
        "module M\nlet h x =\n    let r = if x then 1 else 2\n    x\n    r\n",
        true,
    );
    assert_eq!(c.binders, 2, "`r` and `h`: {c:?}");
}

/// FCS unifies in source order: an earlier use fixes a parameter, and a later
/// condition on it is the error, not a retyping. So a condition grounds the
/// parameter only when it is the parameter's first occurrence — through a
/// statement, a local, or a plain tuple (the last predates CE-1).
#[test]
fn an_earlier_use_fixes_a_parameter_before_a_condition() {
    for body in [
        "mono x; if x then 1 else 2",
        "let y = mono x in if x then 1 else 2",
        "(mono x, if x then 1 else 2)",
    ] {
        let c = check(
            &format!("module M\nlet mono (s: string) = 1\nlet f x = {body}\n"),
            false,
        );
        // Of the declarations, only `mono : string -> int`; `f` stays silent. (A
        // local `y : int` is right, and is not what this case is about.)
        assert_eq!(c.binders - c.local_binders, 1, "{body}: {c:?}");
    }
}

/// An application FCS rejects (a non-function applied) keeps nothing inside its
/// argument. A local in the argument is walked in a check position, and a
/// `let` in a check position emits nothing.
#[test]
fn a_local_inside_a_rejected_application_is_not_published() {
    let c = check(
        "module M\nlet f = 1\nlet g (b: bool) = f (let y = 1 in y)\n",
        false,
    );
    assert_eq!(c.local_binders, 0, "{c:?}");
}

/// A ground statement leaves the binding complete, so the function around it
/// still generalises.
#[test]
fn a_ground_statement_keeps_the_function_generalisable() {
    let c = check("module M\nlet f x = (1; x)\n", true);
    // `f : 'a -> 'a`.
    assert_eq!(c.binders, 1, "{c:?}");
}

// ---------------------------------------------------------------------------
// The generative sweep.
// ---------------------------------------------------------------------------

/// SplitMix64: a deterministic stream, so a failing seed reproduces exactly.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }
}

/// The ground types the generator builds expressions at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum T {
    Int,
    Str,
    Bool,
    /// `int * string`.
    Pair,
}

const TYPES: [T; 4] = [T::Int, T::Str, T::Bool, T::Pair];

/// What an in-scope name can be used as.
#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Mono(T),
    /// `'a -> 'a`: applicable at any type.
    Generic,
    /// An unannotated parameter whose type only its uses decide. Used solely
    /// where any type is legal, so the program stays well-typed whatever FCS
    /// infers for it.
    Open,
}

#[derive(Debug, Clone)]
struct Var {
    name: String,
    kind: Kind,
}

/// The generator's state for one function: the in-scope names, a fresh-name
/// counter, and a record of which generic locals were applied at which types
/// (the hazard's coverage count).
///
/// `modelled_only` restricts the function to constructs inference models. The
/// hazard needs it: any unmodelled construct marks the whole binding
/// incomplete, no argument check fires in an incomplete binding, and so a rule
/// that lets a use ground an open local only shows up in a binding that is
/// otherwise complete.
struct Gen<'r> {
    rng: &'r mut Rng,
    env: Vec<Var>,
    next: usize,
    generic_uses: Vec<(String, T)>,
    modelled_only: bool,
    /// Use an open parameter wherever *any* type is asked for — the ill-typed
    /// family's lever: FCS fixes the parameter at its first use and reports the
    /// rest, and whatever we commit must still agree with what FCS kept.
    open_anywhere: bool,
}

impl Gen<'_> {
    fn fresh(&mut self, stem: &str) -> String {
        self.next += 1;
        format!("{stem}{}", self.next)
    }

    fn vars_of(&self, kind: &Kind) -> Vec<String> {
        self.env
            .iter()
            .filter(|v| &v.kind == kind)
            .map(|v| v.name.clone())
            .collect()
    }

    fn literal(&mut self, t: T) -> String {
        match t {
            T::Int => ["1", "2", "42"][self.rng.below(3)].to_string(),
            T::Str => ["\"s\"", "\"t\""][self.rng.below(2)].to_string(),
            T::Bool => ["true", "false"][self.rng.below(2)].to_string(),
            T::Pair => "(7, \"p\")".to_string(),
        }
    }

    /// An expression of type `t`, built to `depth`.
    fn expr(&mut self, t: T, depth: usize) -> String {
        let vars = self.vars_of(&Kind::Mono(t));
        let opens = self.vars_of(&Kind::Open);
        if self.open_anywhere && !opens.is_empty() && self.rng.chance(20) {
            return opens[self.rng.below(opens.len())].clone();
        }
        // Apply an in-scope generic local often: each application is at the
        // type asked for here, so repeated ones land at several types.
        let generics = self.vars_of(&Kind::Generic);
        if !generics.is_empty() && self.rng.chance(if self.modelled_only { 45 } else { 15 }) {
            let g = generics[self.rng.below(generics.len())].clone();
            let arg = self.expr(t, depth.saturating_sub(1));
            self.generic_uses.push((g.clone(), t));
            return format!("({g} {arg})");
        }
        if depth == 0 {
            return if !vars.is_empty() && self.rng.chance(60) {
                vars[self.rng.below(vars.len())].clone()
            } else {
                self.literal(t)
            };
        }
        match self.rng.below(10) {
            0 => self.literal(t),
            1 if !vars.is_empty() => vars[self.rng.below(vars.len())].clone(),
            2 => {
                let c = self.expr(T::Bool, depth - 1);
                let a = self.expr(t, depth - 1);
                let b = self.expr(t, depth - 1);
                format!("(if {c} then {a} else {b})")
            }
            3 => {
                // A generic application: an in-scope generic local, or the
                // top-level `idf`.
                let generics = self.vars_of(&Kind::Generic);
                let arg = self.expr(t, depth - 1);
                if !generics.is_empty() && self.rng.chance(75) {
                    let g = generics[self.rng.below(generics.len())].clone();
                    self.generic_uses.push((g.clone(), t));
                    format!("({g} {arg})")
                } else {
                    format!("(idf {arg})")
                }
            }
            4 | 5 => self.block(t, depth - 1, false),
            6 if t == T::Int => {
                let s = self.expr(T::Str, depth - 1);
                let strs = self.vars_of(&Kind::Mono(T::Str));
                if !strs.is_empty() && self.rng.chance(50) {
                    format!("{}.Length", strs[self.rng.below(strs.len())])
                } else {
                    format!("({s}).Length")
                }
            }
            7 if t == T::Int => {
                if self.rng.chance(50) {
                    format!("(mono {})", self.expr(T::Bool, depth - 1))
                } else {
                    format!("(monos {})", self.expr(T::Str, depth - 1))
                }
            }
            8 if t == T::Pair => {
                let a = self.expr(T::Int, depth - 1);
                let b = self.expr(T::Str, depth - 1);
                format!("({a}, {b})")
            }
            _ => self.expr(t, depth - 1),
        }
    }

    /// A statement: a non-unit expression (FS0020 is a warning), `ignore` of
    /// one, or an open parameter (which the statement forces to `unit`).
    fn statement(&mut self, depth: usize) -> String {
        let opens = self.vars_of(&Kind::Open);
        if self.modelled_only {
            let t = TYPES[self.rng.below(TYPES.len())];
            return self.expr(t, depth);
        }
        match self.rng.below(4) {
            0 if !opens.is_empty() => opens[self.rng.below(opens.len())].clone(),
            1 => {
                let t = TYPES[self.rng.below(TYPES.len())];
                format!("ignore ({})", self.expr(t, depth))
            }
            _ => {
                let t = TYPES[self.rng.below(TYPES.len())];
                self.expr(t, depth)
            }
        }
    }

    /// One local binding, returning its source text (`name = rhs` or a
    /// pattern form) and extending the environment with what it binds.
    fn binding(&mut self, depth: usize) -> String {
        let choice = if self.modelled_only {
            // A generic local or a plain value local: the two modelled shapes.
            [0, 7][self.rng.below(2)]
        } else {
            self.rng.below(8)
        };
        match choice {
            // A generic local, from the several sources FCS generalises.
            0 | 1 => {
                let name = self.fresh("g");
                let rhs = if self.modelled_only {
                    "idf"
                } else {
                    ["idf", "(fun z -> z)", "id"][self.rng.below(3)]
                };
                let text = format!("{name} = {rhs}");
                self.env.push(Var {
                    name,
                    kind: Kind::Generic,
                });
                text
            }
            // A local function — an unmodelled binding shape.
            2 => {
                let name = self.fresh("h");
                self.env.push(Var {
                    name: name.clone(),
                    kind: Kind::Generic,
                });
                format!("{name} w = w")
            }
            // A tuple pattern — unmodelled, binding two monomorphic names.
            3 => {
                let rhs = self.expr(T::Pair, depth);
                let a = self.fresh("a");
                let b = self.fresh("b");
                let text = format!("({a}, {b}) = {rhs}");
                self.env.push(Var {
                    name: a,
                    kind: Kind::Mono(T::Int),
                });
                self.env.push(Var {
                    name: b,
                    kind: Kind::Mono(T::Str),
                });
                text
            }
            // An alias of an open parameter: the local is open at its binding.
            4 if !self.vars_of(&Kind::Open).is_empty() => {
                let opens = self.vars_of(&Kind::Open);
                let src = opens[self.rng.below(opens.len())].clone();
                let name = self.fresh("o");
                self.env.push(Var {
                    name: name.clone(),
                    kind: Kind::Open,
                });
                format!("{name} = {src}")
            }
            // A plain value local at a ground type.
            _ => {
                let t = TYPES[self.rng.below(TYPES.len())];
                let rhs = self.expr(t, depth);
                let name = self.fresh("v");
                self.env.push(Var {
                    name: name.clone(),
                    kind: Kind::Mono(t),
                });
                format!("{name} = {rhs}")
            }
        }
    }

    /// A block — local bindings and statements, then a final expression of type
    /// `t` — rendered inline (`let … in`, `;`, parenthesised) or, at the top of
    /// a function, offside (one item per line).
    fn block(&mut self, t: T, depth: usize, offside: bool) -> String {
        let scope = self.env.len();
        let items = if self.modelled_only {
            1 + self.rng.below(3)
        } else {
            self.rng.below(4)
        };
        let mut lines: Vec<String> = Vec::new();
        for _ in 0..items {
            if self.rng.chance(60) {
                lines.push(format!("let {}", self.binding(depth)));
            } else {
                lines.push(self.statement(depth));
            }
        }
        let last = self.expr(t, depth);
        self.env.truncate(scope);
        if offside {
            let mut out = String::new();
            for l in lines {
                out.push_str("\n    ");
                out.push_str(&l);
            }
            out.push_str("\n    ");
            out.push_str(&last);
            out
        } else {
            let mut out = last;
            for l in lines.into_iter().rev() {
                out = if l.starts_with("let ") {
                    format!("{l} in {out}")
                } else {
                    format!("{l}; {out}")
                };
            }
            format!("({out})")
        }
    }
}

/// One generated file: the shared header, then `functions` top-level
/// functions over a string, a bool and (sometimes) an open parameter. Returns
/// the source and how many generic locals were applied at two distinct types
/// inside a function built from modelled constructs only — the count that says
/// the hazard was genuinely exercised.
fn generate(seed: u64, functions: usize) -> (String, usize) {
    let mut rng = Rng(seed);
    let mut src = String::from(
        "module Gen\nlet idf x = x\nlet mono (b: bool) = 1\nlet monos (t: string) = 2\n",
    );
    let mut two_type_generics = 0;
    for i in 0..functions {
        let modelled_only = rng.chance(50);
        let with_open = !modelled_only && rng.chance(60);
        let offside = rng.chance(50);
        let t = TYPES[rng.below(TYPES.len())];
        let mut g = Gen {
            rng: &mut rng,
            env: vec![
                Var {
                    name: "s".into(),
                    kind: Kind::Mono(T::Str),
                },
                Var {
                    name: "b".into(),
                    kind: Kind::Mono(T::Bool),
                },
            ],
            next: 0,
            generic_uses: Vec::new(),
            modelled_only,
            open_anywhere: false,
        };
        if with_open {
            g.env.push(Var {
                name: "p".into(),
                kind: Kind::Open,
            });
        }
        let body = g.block(t, 3, offside);
        let mut by_name: std::collections::HashMap<&str, HashSet<T>> = Default::default();
        for (name, ty) in &g.generic_uses {
            by_name.entry(name).or_default().insert(*ty);
        }
        if modelled_only {
            two_type_generics += by_name.values().filter(|s| s.len() > 1).count();
        }
        let params = if with_open {
            "(s: string) (b: bool) p"
        } else {
            "(s: string) (b: bool)"
        };
        if offside {
            src.push_str(&format!("let f{i} {params} ={body}\n"));
        } else {
            src.push_str(&format!("let f{i} {params} = {body}\n"));
        }
    }
    (src, two_type_generics)
}

/// The generative sweep: every committed expression and binder type in a few
/// hundred generated functions agrees with FCS, and the sweep genuinely
/// measured something.
#[test]
fn generated_local_lets_and_sequences_agree_with_fcs() {
    let files = crate::common::env_usize_or("BORZOI_LOCAL_LET_FILES", 40);
    let mut total = Committed::default();
    let mut two_type_generics = 0;
    for seed in 0..files as u64 {
        let (src, twos) = generate(seed, 6);
        two_type_generics += twos;
        total += check(&src, true);
    }
    eprintln!(
        "local-let sweep: {total:?}, generic locals at two types in modelled-only \
         functions: {two_type_generics}"
    );
    assert!(
        two_type_generics >= 20,
        "the generator stopped exercising the generalised-local hazard ({two_type_generics})"
    );
    assert!(
        total.local_binders >= 40,
        "the sweep committed too few local binders to be evidence: {total:?}"
    );
    assert!(
        total.sequentials >= 10,
        "the sweep committed too few sequence nodes to be evidence: {total:?}"
    );
}

/// The ill-typed family. Each function is a generated block, wrapped in one of
/// three ways: bare, as the argument of a method call FCS rejects on arity, or
/// as the argument of a non-function value applied (`k0 (…)`). Open parameters
/// are used wherever any type is asked for, so FCS fixes each at its first use
/// and reports the others. The comparison is the strict one: anything we commit,
/// FCS must have kept, with the same type.
///
/// This is where the dropped relations live. The well-typed sweep cannot see
/// them — on a program FCS accepts, a constraint we drop never contradicts one
/// we keep — so a rule that is only sound on accepted programs passes there and
/// fails here.
#[test]
fn generated_ill_typed_programs_commit_only_what_fcs_kept() {
    // Twice the well-typed sweep's files: a dropped relation needs a particular
    // conjunction of shapes (a statement, then a condition, on one parameter, in
    // an otherwise ground function), and at half this size the sample missed it.
    let files = crate::common::env_usize_or("BORZOI_LOCAL_LET_FILES", 40) * 2;
    let mut total = Committed::default();
    let mut wrapped_lets = 0usize;
    for seed in 0..files.max(1) as u64 {
        let mut rng = Rng(seed ^ 0x5eed_ba77);
        let mut src = String::from(
            "module Gen\nlet idf x = x\nlet mono (b: bool) = 1\nlet monos (t: string) = 2\nlet k0 = 1\n",
        );
        for i in 0..6 {
            let modelled_only = rng.chance(50);
            let t = TYPES[rng.below(TYPES.len())];
            let wrap = rng.below(3);
            let mut g = Gen {
                rng: &mut rng,
                env: vec![
                    Var {
                        name: "s".into(),
                        kind: Kind::Mono(T::Str),
                    },
                    Var {
                        name: "b".into(),
                        kind: Kind::Mono(T::Bool),
                    },
                    Var {
                        name: "p".into(),
                        kind: Kind::Open,
                    },
                ],
                next: 0,
                generic_uses: Vec::new(),
                modelled_only,
                open_anywhere: true,
            };
            let block = g.block(t, 3, wrap == 0 && i % 2 == 0);
            let body = match wrap {
                0 => block,
                1 => {
                    wrapped_lets += block.matches("let ").count();
                    format!(" \"r\".ToLowerInvariant({block})")
                }
                _ => {
                    wrapped_lets += block.matches("let ").count();
                    format!(" k0 ({block})")
                }
            };
            let sep = if body.starts_with('\n') || body.starts_with(' ') {
                ""
            } else {
                " "
            };
            src.push_str(&format!("let f{i} (s: string) (b: bool) p ={sep}{body}\n"));
        }
        total += check(&src, false);
    }
    eprintln!("ill-typed family: {total:?}, lets inside rejected constructs: {wrapped_lets}");
    assert!(
        total.fcs_errors >= 50,
        "the family stopped producing ill-typed programs: {total:?}"
    );
    assert!(
        wrapped_lets >= 40,
        "too few local bindings inside rejected constructs to be evidence: {wrapped_lets}"
    );
    assert!(
        total.exprs + total.binders >= 120,
        "the family committed too little to be evidence: {total:?}"
    );
}

/// Sanity for the generator itself, cheap enough to run over more seeds than
/// the sweep: every generated file parses, and the population contains the two
/// constructs under test in quantity.
#[test]
fn generated_programs_parse() {
    let (mut lets, mut seqs) = (0usize, 0usize);
    for seed in 0..256 {
        let (src, _) = generate(seed, 6);
        let parsed = parse(&src);
        assert!(parsed.errors.is_empty(), "{:?}\n{src}", parsed.errors);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        for e in file.syntax().descendants().filter_map(Expr::cast) {
            match e {
                Expr::LetOrUse(_) => lets += 1,
                Expr::Sequential(_) => seqs += 1,
                _ => {}
            }
        }
    }
    assert!(
        lets >= 500 && seqs >= 200,
        "lets: {lets}, sequences: {seqs}"
    );
}
