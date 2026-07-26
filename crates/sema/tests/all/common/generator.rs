//! A generator of random *well-scoped* F# programs within the current parser
//! subset, used by the resolver tests.
//!
//! A [`generate`] call interprets a tape of random numbers deterministically
//! into a program together with, by construction, the exact binder every name
//! occurrence must resolve to — so it is its own oracle for the scoping *model*
//! (used FCS-free by `resolve_scoping.rs`) and a source of random inputs for
//! the FCS differential (`resolve_diff.rs`).
//!
//! The programs are **name**-correct, not type-correct: an application may
//! apply an integer and a `match` may mix cases of different unions. Name
//! resolution is what is under test, and FCS reports the same symbol uses
//! either way — which the FCS differential over generated programs confirms
//! empirically.
//!
//! Because the interpreter always produces a valid, scope-correct program for
//! *any* tape, shrinking the tape (proptest) or varying a seed never yields
//! garbage.
//!
//! # What a name occurrence can be
//!
//! Every occurrence is either a *binder* (recorded in
//! [`Generated::binder_ranges`]) or a *reference* to one ([`Generated::refs`]),
//! and references carry a [`RefKind`] so a test can assert a tape actually
//! exercised the construct it claims to cover. The interesting kinds live in
//! pattern position:
//!
//! * [`RefKind::UnionCase`] / [`RefKind::ActivePattern`] — a pattern *head* is
//!   a reference to a declaration, never a binder.
//! * [`RefKind::ActivePatternArgument`] — a parameterised recognizer's argument
//!   is written inside the pattern but *evaluated* in the enclosing scope, so
//!   it references whatever was in scope at the `match`.
//! * [`RefKind::OrAlias`] — an or-pattern binds each name **once**, at its
//!   first alternative; every later alternative's spelling of that name is a
//!   reference to that one binder, as is the clause body's use.
//!
//! # The grammar
//!
//! Declarations: unions (`type T = | C | D of int`) and total single-case
//! active patterns, parameterised or not. Bindings: `let` values, curried
//! functions, `let rec`, and tuple deconstructions (`let (a, b) = …`).
//! Expressions: literals, references, parens, tuples, application,
//! `if`/`then`/`else`, lambdas, case construction, `match` and `function`.
//! Patterns: wildcard, literal, binder, `as`, tuple, union-case and
//! active-pattern heads, and or-patterns — the heads nesting inside one another
//! to [`MAX_PATTERN_DEPTH`].
//!
//! What it deliberately does **not** generate, and why — each of these would
//! make the differential report a disagreement that is not a resolver fault:
//!
//! * **Patterned lambda parameters** (`fun (A v) -> …`). FCS synthesises an
//!   `_arg1` symbol spanning the whole pattern, which sema does not model.
//! * **A *named* active-pattern argument in a binding head** (`let (Ap v w) =
//!   …`). Sema declines the value use there on purpose — see [`PatPos`].
//! * **Multi-case active patterns** (`(|Even|Odd|)`). Their body must construct
//!   a case, and an expression use of a case is `FS0039`, which sema declines.
//!
//! And what is simply not modelled yet: record / list / array / type-test
//! patterns, local (in-expression) `let` deconstructions, type definitions with
//! members, and nested modules.

use std::collections::HashMap;

use rowan::TextRange;

/// What a generated name occurrence refers to. Recorded so tests can assert
/// that a tape exercised a construct rather than silently skipping it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefKind {
    /// A value, parameter or pattern-local used in expression position.
    Value,
    /// A union case, as a constructor expression or a pattern head.
    UnionCase,
    /// An active-pattern case token used as a pattern head. It resolves to the
    /// *recognizer* (the whole `|Name|` span), not to the case token in the
    /// definition.
    ActivePattern,
    /// The argument of a parameterised active-pattern application: written in
    /// pattern position, evaluated in the enclosing scope.
    ActivePatternArgument,
    /// A later or-pattern alternative's spelling of a name the first
    /// alternative binds.
    OrAlias,
}

/// Declared through a macro so [`Form::ALL`] cannot drift from the enum: a new
/// variant is automatically in the list the coverage test sweeps, and so must
/// actually be emitted or that test fails.
macro_rules! forms {
    ($($(#[$m:meta])* $v:ident,)*) => {
        /// A syntactic form the interpreter can emit. Counted per generation so
        /// a test can assert the tape *reached* each one — a construct that
        /// silently stopped being generated would otherwise leave every
        /// property green and proving nothing about it.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Form {
            $($(#[$m])* $v,)*
        }
        impl Form {
            /// Every form. Kept in step with the enum by construction.
            pub const ALL: &'static [Form] = &[$(Form::$v),*];
        }
    };
}

forms! {
    /// `type T = | C | D of int`.
    UnionDecl,
    /// A union case carrying a payload (`D of int`), which alone can head an
    /// applied pattern.
    UnionCasePayload,
    /// `let (|Ap|) n = n`.
    ActivePatternDecl,
    /// `let (|Ap|) k n = n` — the parameterised form, whose application has an
    /// argument as well as a payload.
    ActivePatternDeclWithParam,
    /// `let v = …`.
    LetValue,
    /// `let f a b = …`.
    LetFunction,
    /// `let rec …`.
    LetRec,
    /// `let (a, b) = …` — the pattern walk in its `let` role.
    LetDeconstruction,
    /// A top-level binding reusing an earlier top-level name.
    ShadowingTopLevelBinding,
    /// A lambda parameter reusing an in-scope name.
    ShadowingLambdaParam,
    /// A pattern binder reusing an in-scope name.
    ShadowingPatternBinder,
    /// An integer literal.
    ExprLiteral,
    /// A name use.
    ExprRef,
    /// `(e)`.
    ExprParen,
    /// `(a, b)`.
    ExprTuple,
    /// `f x`.
    ExprApp,
    /// `if c then a else b`.
    ExprIf,
    /// `fun p -> …`.
    ExprLambda,
    /// `match e with …`.
    ExprMatch,
    /// `function | …`.
    ExprFunction,
    /// A union-case constructor in expression position.
    ExprCaseConstruction,
    /// `_`.
    PatWildcard,
    /// A literal pattern.
    PatLiteral,
    /// A binding occurrence.
    PatBinder,
    /// `(p) as w`.
    PatAs,
    /// `(p, q)` — an or-pattern alternative binding two names.
    PatTuple,
    /// A nullary case head, which binds nothing.
    PatNullaryCase,
    /// An applied union-case head.
    PatCaseHead,
    /// An active-pattern head.
    PatActivePatternHead,
    /// A *named* argument to a parameterised recognizer.
    PatActivePatternArgument,
    /// `p | q` — alternation.
    PatOr,
    /// A head nested inside another (`C (Ap x)`), the shape whose
    /// canonicalisation has to re-point an inner alias.
    PatNestedHead,
    /// A later alternative's re-spelling of a bound name.
    PatOrAlias,
    /// `when e`.
    ClauseGuard,
}

/// One generated name reference and the binder it must resolve to.
#[derive(Clone, Debug)]
pub struct GeneratedRef {
    pub range: TextRange,
    /// The uid of the binder in [`Generated::binder_ranges`] this must resolve
    /// to.
    pub target: usize,
    pub kind: RefKind,
}

/// The product of generation: source text and the resolutions it must induce.
pub struct Generated {
    pub src: String,
    /// Binder uid → its defining source range.
    pub binder_ranges: HashMap<usize, TextRange>,
    /// Every reference occurrence and the binder it must resolve to.
    pub refs: Vec<GeneratedRef>,
    /// How many times each syntactic form was emitted.
    pub forms: HashMap<Form, usize>,
}

/// A deterministic tape of `len` numbers for `seed`, so a test can name the
/// programs it ran without carrying a corpus of literals.
///
/// An arithmetic tape (`seed + i*k`) is *not* good enough: the interpreter
/// picks forms with `n % k`, so a stride sharing a factor with `k` makes a
/// whole construct unreachable for every seed. This is an xorshift32, whose
/// low bits vary.
pub fn seed_tape(seed: u32, len: usize) -> Vec<u32> {
    let mut state = seed.wrapping_mul(2_654_435_761) ^ 0x9E37_79B9;
    state |= 1; // xorshift32 is stuck at zero.
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        })
        .collect()
}

/// Interpret `nums` into a well-scoped program.
pub fn generate(nums: Vec<u32>) -> Generated {
    let mut g = Gen {
        tape: Tape { nums, pos: 0 },
        next_uid: 0,
        unions: Vec::new(),
        aps: Vec::new(),
        forms: HashMap::new(),
    };
    let program = g.program();
    let mut render = Render::default();
    render.program(&program);
    Generated {
        src: render.out,
        binder_ranges: render.binder_ranges,
        refs: render.refs,
        forms: g.forms,
    }
}

/// A binder occurrence: a globally-unique id and its source name (names repeat
/// when shadowing is generated).
#[derive(Clone)]
struct Binder {
    uid: usize,
    name: String,
}

#[derive(Clone)]
enum GExpr {
    Lit(u32),
    Ref {
        name: String,
        target: usize,
    },
    /// A union-case constructor, applied when the case carries a payload.
    Case {
        name: String,
        target: usize,
        arg: Option<Box<GExpr>>,
    },
    Paren(Box<GExpr>),
    Tuple(Vec<GExpr>),
    App(Box<GExpr>, Box<GExpr>),
    If(Box<GExpr>, Box<GExpr>, Box<GExpr>),
    Fun {
        param: Binder,
        body: Box<GExpr>,
    },
    Match {
        scrutinee: Box<GExpr>,
        clauses: Vec<GClause>,
    },
    /// `function | p -> …`: the same clause scoping with the scrutinee left
    /// implicit.
    Function {
        clauses: Vec<GClause>,
    },
}

#[derive(Clone)]
struct GClause {
    pat: GPat,
    guard: Option<GExpr>,
    body: GExpr,
}

#[derive(Clone)]
enum GPat {
    Wild,
    Lit(u32),
    /// The declaring occurrence of a name.
    Bind(Binder),
    /// A later or-pattern alternative's spelling of an already-bound name.
    Alias {
        name: String,
        target: usize,
    },
    Tuple(Vec<GPat>),
    Case {
        name: String,
        target: usize,
        arg: Option<Box<GPat>>,
    },
    /// A total single-case active-pattern application. `arg` is present exactly
    /// when the recognizer takes a parameter, and is an *expression* in the
    /// enclosing scope.
    Ap {
        name: String,
        target: usize,
        arg: Option<GExpr>,
        payload: Box<GPat>,
    },
    As {
        inner: Box<GPat>,
        binder: Binder,
    },
    /// Two or three alternatives binding the same names, the first declaring.
    Or(Vec<GPat>),
}

enum GHead {
    Value(Binder),
    Func(Binder, Vec<Binder>),
    /// `let (a, b) = …`: a tuple pattern whose leaves are module-level values.
    /// Reached by *descending* past the `let` head, where a `LongIdent` reads as
    /// a constructor reference rather than the function-binding form.
    Deconstruct(GPat),
}

struct GBinding {
    rec: bool,
    head: GHead,
    rhs: GExpr,
}

/// `type T = C0 | C1 of int`.
struct GUnion {
    name: Binder,
    cases: Vec<GCase>,
}

struct GCase {
    binder: Binder,
    payload: bool,
}

/// `let (|Ap0|) n = n`, or `let (|Ap0|) k n = n` when parameterised. Total and
/// single-case, so its body is the matched value rather than a case
/// construction (an expression use of a case is `FS0039`, which sema declines
/// to resolve — see `resolve_diff.rs`'s corpus note).
struct GAp {
    /// Uid and *bare* name; the declaration's recorded range spans `|Name|`.
    binder: Binder,
    param: Option<Binder>,
    matched: Binder,
}

struct GProgram {
    unions: Vec<GUnion>,
    aps: Vec<GAp>,
    bindings: Vec<GBinding>,
}

struct Tape {
    nums: Vec<u32>,
    pos: usize,
}

impl Tape {
    fn next_num(&mut self) -> u32 {
        let v = self.nums.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        v
    }
    fn choice(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            self.next_num() as usize % n
        }
    }
    fn flip(&mut self) -> bool {
        self.next_num().is_multiple_of(2)
    }
    fn between(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.choice(hi - lo + 1)
    }
}

struct Gen {
    tape: Tape,
    next_uid: usize,
    unions: Vec<GUnion>,
    aps: Vec<GAp>,
    forms: HashMap<Form, usize>,
}

const MAX_DEPTH: usize = 3;

/// How deep a pattern head may nest inside another (`C (A x | B x)`).
const MAX_PATTERN_DEPTH: usize = 2;

/// Where a pattern sits, which decides whether a parameterised active pattern's
/// argument may be a *name*.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PatPos {
    /// A `match` / `function` clause. An active-pattern argument resolves
    /// against the enclosing scope, exactly as FCS does.
    Clause,
    /// A binding head (`let (Ap k w, z) = …`). Sema deliberately **declines** to
    /// resolve an active-pattern argument's *value* use here — in a curried
    /// head an earlier parameter that should shadow the name is not yet in
    /// scope, so committing would risk the wrong target
    /// (`Resolver::decline_binding_head_param_exprs`). That is a stated coverage
    /// gap, not a defect, so the generator makes no claim about such a use: it
    /// only ever writes a *literal* argument here.
    BindingHead,
}

impl Gen {
    fn form(&mut self, f: Form) {
        *self.forms.entry(f).or_default() += 1;
    }

    fn binder(&mut self, name: String) -> Binder {
        let uid = self.next_uid;
        self.next_uid += 1;
        Binder { uid, name }
    }
    fn fresh_value(&mut self) -> Binder {
        let n = self.next_uid;
        self.binder(format!("v{n}"))
    }
    fn fresh_param(&mut self) -> Binder {
        let n = self.next_uid;
        self.binder(format!("p{n}"))
    }
    fn fresh_local(&mut self) -> Binder {
        let n = self.next_uid;
        self.binder(format!("w{n}"))
    }
    fn fresh_type(&mut self) -> Binder {
        let n = self.next_uid;
        self.binder(format!("T{n}"))
    }
    fn fresh_case(&mut self) -> Binder {
        let n = self.next_uid;
        self.binder(format!("C{n}"))
    }
    fn fresh_ap(&mut self) -> Binder {
        let n = self.next_uid;
        self.binder(format!("Ap{n}"))
    }

    fn program(&mut self) -> GProgram {
        for _ in 0..self.tape.between(0, 2) {
            self.form(Form::UnionDecl);
            let name = self.fresh_type();
            let n_cases = self.tape.between(1, 3);
            let cases = (0..n_cases)
                .map(|_| {
                    let binder = self.fresh_case();
                    let payload = self.tape.flip();
                    if payload {
                        self.form(Form::UnionCasePayload);
                    }
                    GCase { binder, payload }
                })
                .collect();
            self.unions.push(GUnion { name, cases });
        }
        for _ in 0..self.tape.between(0, 2) {
            self.form(Form::ActivePatternDecl);
            let binder = self.fresh_ap();
            let param = if self.tape.flip() {
                self.form(Form::ActivePatternDeclWithParam);
                Some(self.fresh_param())
            } else {
                None
            };
            let matched = self.fresh_param();
            self.aps.push(GAp {
                binder,
                param,
                matched,
            });
        }

        let n = self.tape.between(1, 6);
        let mut top: Vec<Binder> = Vec::new();
        let mut bindings = Vec::new();
        for _ in 0..n {
            // A deconstructing binding — `let (a, b) = e` — is not a `rec`
            // form, and reaches the pattern walk in the `let` role rather than
            // the parameter/match one.
            if self.tape.choice(4) == 0 {
                self.form(Form::LetDeconstruction);
                let (pat, binders) = self.let_deconstruction(&top);
                let rhs = self.expr(&top, 0);
                bindings.push(GBinding {
                    rec: false,
                    head: GHead::Deconstruct(pat),
                    rhs,
                });
                top.extend(binders);
                continue;
            }
            let rec = self.tape.flip();
            let is_func = self.tape.flip();
            self.form(if is_func {
                Form::LetFunction
            } else {
                Form::LetValue
            });
            if rec {
                self.form(Form::LetRec);
            }
            // Sometimes reuse an existing top-level name to exercise shadowing.
            let head = if !top.is_empty() && self.tape.flip() {
                self.form(Form::ShadowingTopLevelBinding);
                let name = top[self.tape.choice(top.len())].name.clone();
                self.binder(name)
            } else {
                self.fresh_value()
            };
            let params: Vec<Binder> = if is_func {
                let np = self.tape.between(1, 2);
                (0..np).map(|_| self.fresh_param()).collect()
            } else {
                Vec::new()
            };
            // RHS scope: prior top-level binders, plus (if rec) this binder,
            // plus the parameters — in shadowing order (later = more recent).
            let mut scope = top.clone();
            if rec {
                scope.push(head.clone());
            }
            scope.extend(params.iter().cloned());
            let rhs = self.expr(&scope, 0);
            let ghead = if is_func {
                GHead::Func(head.clone(), params)
            } else {
                GHead::Value(head.clone())
            };
            bindings.push(GBinding {
                rec,
                head: ghead,
                rhs,
            });
            top.push(head);
        }
        GProgram {
            unions: std::mem::take(&mut self.unions),
            aps: std::mem::take(&mut self.aps),
            bindings,
        }
    }

    fn expr(&mut self, scope: &[Binder], depth: usize) -> GExpr {
        let forms = if depth >= MAX_DEPTH { 2 } else { 10 };
        match self.tape.choice(forms) {
            0 => {
                self.form(Form::ExprLiteral);
                GExpr::Lit(self.tape.choice(10) as u32)
            }
            1 => self.reference(scope),
            2 => {
                self.form(Form::ExprParen);
                GExpr::Paren(Box::new(self.expr(scope, depth + 1)))
            }
            3 => {
                self.form(Form::ExprTuple);
                let k = self.tape.between(2, 3);
                GExpr::Tuple((0..k).map(|_| self.expr(scope, depth + 1)).collect())
            }
            4 => {
                self.form(Form::ExprApp);
                GExpr::App(
                    Box::new(self.expr(scope, depth + 1)),
                    Box::new(self.expr(scope, depth + 1)),
                )
            }
            5 => {
                self.form(Form::ExprIf);
                GExpr::If(
                    Box::new(self.expr(scope, depth + 1)),
                    Box::new(self.expr(scope, depth + 1)),
                    Box::new(self.expr(scope, depth + 1)),
                )
            }
            6 => {
                // A lambda whose parameter may shadow an in-scope name. The
                // parameter is always a bare identifier: FCS synthesises an
                // `_arg1` symbol over a *non-simple* lambda pattern, which sema
                // does not model, so patterned lambdas would inject an
                // unmodelled FCS use into the differential.
                self.form(Form::ExprLambda);
                let param = if !scope.is_empty() && self.tape.flip() {
                    self.form(Form::ShadowingLambdaParam);
                    let name = scope[self.tape.choice(scope.len())].name.clone();
                    self.binder(name)
                } else {
                    self.fresh_param()
                };
                let mut inner = scope.to_vec();
                inner.push(param.clone());
                let body = Box::new(self.expr(&inner, depth + 1));
                GExpr::Fun { param, body }
            }
            7 => self.match_expr(scope, depth),
            8 => self.case_ctor(scope, depth),
            9 => {
                self.form(Form::ExprFunction);
                GExpr::Function {
                    clauses: self.clauses(scope, depth),
                }
            }
            _ => unreachable!(),
        }
    }

    /// A reference to a random in-scope name, recording the *latest* binder of
    /// that name (the one position-ordered shadowing must pick).
    fn reference(&mut self, scope: &[Binder]) -> GExpr {
        if scope.is_empty() {
            return GExpr::Lit(0);
        }
        self.form(Form::ExprRef);
        let name = scope[self.tape.choice(scope.len())].name.clone();
        let target = scope.iter().rev().find(|b| b.name == name).unwrap().uid;
        GExpr::Ref { name, target }
    }

    /// A union-case constructor expression, applied when the case has a
    /// payload. Falls back to a literal when the tape declared no unions.
    fn case_ctor(&mut self, scope: &[Binder], depth: usize) -> GExpr {
        let Some((name, target, payload)) = self.pick_case(|_| true) else {
            return GExpr::Lit(0);
        };
        self.form(Form::ExprCaseConstruction);
        let arg = payload.then(|| Box::new(self.expr(scope, depth + 1)));
        GExpr::Case { name, target, arg }
    }

    /// Pick a case satisfying `want` (by payload-ness), as
    /// `(name, uid, has_payload)`.
    fn pick_case(&mut self, want: impl Fn(bool) -> bool) -> Option<(String, usize, bool)> {
        let candidates: Vec<(String, usize, bool)> = self
            .unions
            .iter()
            .flat_map(|u| u.cases.iter())
            .filter(|c| want(c.payload))
            .map(|c| (c.binder.name.clone(), c.binder.uid, c.payload))
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let i = self.tape.choice(candidates.len());
        Some(candidates[i].clone())
    }

    /// Pick a recognizer, as `(name, uid, takes_a_parameter)`.
    fn pick_ap(&mut self) -> Option<(String, usize, bool)> {
        if self.aps.is_empty() {
            return None;
        }
        let i = self.tape.choice(self.aps.len());
        let ap = &self.aps[i];
        Some((ap.binder.name.clone(), ap.binder.uid, ap.param.is_some()))
    }

    fn match_expr(&mut self, scope: &[Binder], depth: usize) -> GExpr {
        self.form(Form::ExprMatch);
        let scrutinee = Box::new(self.expr(scope, depth + 1));
        let clauses = self.clauses(scope, depth);
        GExpr::Match { scrutinee, clauses }
    }

    fn clauses(&mut self, scope: &[Binder], depth: usize) -> Vec<GClause> {
        let n = self.tape.between(1, 3);
        (0..n).map(|_| self.clause(scope, depth)).collect()
    }

    fn clause(&mut self, scope: &[Binder], depth: usize) -> GClause {
        let (pat, binders) = self.clause_pat(scope);
        let mut inner = scope.to_vec();
        inner.extend(binders);
        let guard = self.tape.flip().then(|| {
            self.form(Form::ClauseGuard);
            self.expr(&inner, depth + 1)
        });
        let body = self.expr(&inner, depth + 1);
        GClause { pat, guard, body }
    }

    /// A clause pattern and the binders it introduces, in scope order.
    fn clause_pat(&mut self, scope: &[Binder]) -> (GPat, Vec<Binder>) {
        let (pat, binders) = match self.tape.choice(5) {
            0 => {
                self.form(Form::PatWildcard);
                (GPat::Wild, Vec::new())
            }
            1 => {
                self.form(Form::PatLiteral);
                (GPat::Lit(self.tape.choice(10) as u32), Vec::new())
            }
            // A nullary case head binds nothing, so the whole clause body sees
            // only the enclosing scope.
            2 => match self.pick_case(|payload| !payload) {
                Some((name, target, _)) => {
                    self.form(Form::PatNullaryCase);
                    (
                        GPat::Case {
                            name,
                            target,
                            arg: None,
                        },
                        Vec::new(),
                    )
                }
                None => {
                    self.form(Form::PatWildcard);
                    (GPat::Wild, Vec::new())
                }
            },
            3 => {
                let b = self.pattern_binder(scope, &[]);
                let mut first = true;
                (
                    self.carrier(scope, &b, &mut first, 0, PatPos::Clause),
                    vec![b],
                )
            }
            4 => self.or_pat(scope),
            _ => unreachable!(),
        };
        // `<pat> as w` names the whole match, adding one more binder.
        if self.tape.flip() {
            self.form(Form::PatAs);
            let mut taken: Vec<String> = binders.iter().map(|b| b.name.clone()).collect();
            let w = self.pattern_binder(scope, &taken);
            taken.push(w.name.clone());
            let mut all = binders;
            all.push(w.clone());
            (
                GPat::As {
                    inner: Box::new(pat),
                    binder: w,
                },
                all,
            )
        } else {
            (pat, binders)
        }
    }

    /// An or-pattern: two or three alternatives binding the same names, in the
    /// same order. Only the first alternative *declares*; the rest spell the
    /// same names as aliases of it, which is what F# means by an or-pattern and
    /// what FCS reports.
    fn or_pat(&mut self, scope: &[Binder]) -> (GPat, Vec<Binder>) {
        let k = self.tape.between(1, 2);
        let mut binders: Vec<Binder> = Vec::new();
        for _ in 0..k {
            let taken: Vec<String> = binders.iter().map(|b| b.name.clone()).collect();
            let b = self.pattern_binder(scope, &taken);
            binders.push(b);
        }
        // One flag per name, threaded across *all* alternatives: whichever
        // spelling comes first in source order is the binder, whatever nesting
        // it sits in, and every later one aliases it.
        let mut first: Vec<bool> = vec![true; binders.len()];
        let n_alts = self.tape.between(2, 3);
        let mut alts = Vec::new();
        for _ in 0..n_alts {
            let mut parts = Vec::new();
            for i in 0..binders.len() {
                let b = binders[i].clone();
                let mut seen = first[i];
                parts.push(self.carrier(scope, &b, &mut seen, 0, PatPos::Clause));
                first[i] = seen;
            }
            alts.push(if parts.len() == 1 {
                parts.pop().unwrap()
            } else {
                self.form(Form::PatTuple);
                GPat::Tuple(parts)
            });
        }
        self.form(Form::PatOr);
        (GPat::Or(alts), binders)
    }

    /// A pattern that spells the one name `b` exactly once, under a
    /// randomly-chosen head: bare, a payload case, an active-pattern
    /// application, or a *nested* alternation of any of those. The head is a
    /// *reference*, so alternatives of an or-pattern can differ structurally
    /// while binding the same name.
    ///
    /// `first` is the "no spelling of this name has been emitted yet" flag: the
    /// first one binds and the rest alias it, so a nested `(A x | B x) | (C x |
    /// D x)` still has exactly one declaration — the identity F# and FCS give
    /// it, and the shape that makes the outer canonicalisation re-point the
    /// inner aliases.
    fn carrier(
        &mut self,
        scope: &[Binder],
        b: &Binder,
        first: &mut bool,
        depth: usize,
        pos: PatPos,
    ) -> GPat {
        if depth >= MAX_PATTERN_DEPTH {
            return self.leaf(b, first);
        }
        if depth > 0 {
            self.form(Form::PatNestedHead);
        }
        match self.tape.choice(4) {
            0 => self.leaf(b, first),
            1 => match self.pick_case(|payload| payload) {
                Some((name, target, _)) => {
                    self.form(Form::PatCaseHead);
                    GPat::Case {
                        name,
                        target,
                        arg: Some(Box::new(self.carrier(scope, b, first, depth + 1, pos))),
                    }
                }
                None => self.leaf(b, first),
            },
            2 => match self.pick_ap() {
                Some((name, target, parameterised)) => {
                    self.form(Form::PatActivePatternHead);
                    // The recognizer's argument is an expression evaluated in
                    // the *enclosing* scope — the clause's own binders are not
                    // visible to it. Kept to an atom so it needs no parens in
                    // pattern position.
                    let arg = parameterised.then(|| {
                        if pos == PatPos::BindingHead || self.tape.flip() {
                            GExpr::Lit(self.tape.choice(10) as u32)
                        } else {
                            self.form(Form::PatActivePatternArgument);
                            self.reference(scope)
                        }
                    });
                    let payload = Box::new(self.carrier(scope, b, first, depth + 1, pos));
                    GPat::Ap {
                        name,
                        target,
                        arg,
                        payload,
                    }
                }
                None => self.leaf(b, first),
            },
            3 => {
                self.form(Form::PatOr);
                let n_alts = self.tape.between(2, 3);
                let alts = (0..n_alts)
                    .map(|_| self.carrier(scope, b, first, depth + 1, pos))
                    .collect();
                GPat::Or(alts)
            }
            _ => unreachable!(),
        }
    }

    /// The name itself: a binder the first time it is spelled anywhere in the
    /// pattern, an alias of that binder every later time.
    fn leaf(&mut self, b: &Binder, first: &mut bool) -> GPat {
        if *first {
            *first = false;
            self.form(Form::PatBinder);
            GPat::Bind(b.clone())
        } else {
            self.form(Form::PatOrAlias);
            GPat::Alias {
                name: b.name.clone(),
                target: b.uid,
            }
        }
    }

    /// A tuple pattern for the head of a module-level `let`, and the names it
    /// binds. Two or three elements, so the head always descends (a bare
    /// `let C x = …` at the direct head is the *function-binding* form, which
    /// binds `C`, not a constructor pattern).
    fn let_deconstruction(&mut self, scope: &[Binder]) -> (GPat, Vec<Binder>) {
        let k = self.tape.between(2, 3);
        self.form(Form::PatTuple);
        let mut binders = Vec::new();
        let mut parts = Vec::new();
        for _ in 0..k {
            let taken: Vec<String> = binders.iter().map(|b: &Binder| b.name.clone()).collect();
            let b = self.pattern_binder(scope, &taken);
            let mut first = true;
            parts.push(self.carrier(scope, &b, &mut first, 0, PatPos::BindingHead));
            binders.push(b);
        }
        (GPat::Tuple(parts), binders)
    }

    /// A binder for a pattern position, sometimes reusing an in-scope name to
    /// exercise shadowing — but never a name already bound by the same
    /// pattern, which F# rejects as a duplicate binding.
    fn pattern_binder(&mut self, scope: &[Binder], taken: &[String]) -> Binder {
        if !scope.is_empty() && self.tape.flip() {
            let name = scope[self.tape.choice(scope.len())].name.clone();
            if !taken.contains(&name) {
                self.form(Form::ShadowingPatternBinder);
                return self.binder(name);
            }
        }
        self.fresh_local()
    }
}

/// Render a program to F# source, recording every binder's range (by uid) and
/// every reference's range, target uid and kind.
#[derive(Default)]
struct Render {
    out: String,
    binder_ranges: HashMap<usize, TextRange>,
    refs: Vec<GeneratedRef>,
}

impl Render {
    fn span(start: usize, end: usize) -> TextRange {
        TextRange::new(
            u32::try_from(start).unwrap().into(),
            u32::try_from(end).unwrap().into(),
        )
    }
    fn emit_binder(&mut self, b: &Binder) {
        let start = self.out.len();
        self.out.push_str(&b.name);
        let end = self.out.len();
        self.binder_ranges.insert(b.uid, Self::span(start, end));
    }
    /// A recognizer declares over its whole `|Name|` span (parens excluded) —
    /// which is also where FCS points every *use* of one of its cases.
    fn emit_ap_binder(&mut self, b: &Binder) {
        let start = self.out.len();
        self.out.push('|');
        self.out.push_str(&b.name);
        self.out.push('|');
        let end = self.out.len();
        self.binder_ranges.insert(b.uid, Self::span(start, end));
    }
    fn emit_ref(&mut self, name: &str, target: usize, kind: RefKind) {
        let start = self.out.len();
        self.out.push_str(name);
        let end = self.out.len();
        self.refs.push(GeneratedRef {
            range: Self::span(start, end),
            target,
            kind,
        });
    }

    fn program(&mut self, prog: &GProgram) {
        for u in &prog.unions {
            self.out.push_str("type ");
            self.emit_binder(&u.name);
            self.out.push_str(" =");
            for c in &u.cases {
                // Always a *leading* bar: `type T = C` without one is a type
                // abbreviation naming the type `C`, not a one-case union.
                self.out.push_str(" | ");
                self.emit_binder(&c.binder);
                if c.payload {
                    self.out.push_str(" of int");
                }
            }
            self.out.push('\n');
        }
        for ap in &prog.aps {
            self.out.push_str("let (");
            self.emit_ap_binder(&ap.binder);
            self.out.push(')');
            if let Some(p) = &ap.param {
                self.out.push(' ');
                self.emit_binder(p);
            }
            self.out.push(' ');
            self.emit_binder(&ap.matched);
            self.out.push_str(" = ");
            self.emit_ref(&ap.matched.name, ap.matched.uid, RefKind::Value);
            self.out.push('\n');
        }
        for b in &prog.bindings {
            self.out.push_str("let ");
            if b.rec {
                self.out.push_str("rec ");
            }
            match &b.head {
                GHead::Value(h) => self.emit_binder(h),
                GHead::Func(h, params) => {
                    self.emit_binder(h);
                    for p in params {
                        self.out.push(' ');
                        self.emit_binder(p);
                    }
                }
                GHead::Deconstruct(pat) => self.pat(pat),
            }
            self.out.push_str(" = ");
            self.expr(&b.rhs);
            self.out.push('\n');
        }
    }

    /// Render a sub-expression in a position where a compound form needs
    /// parenthesising (application argument, `if` sub-expression, tuple
    /// element, `match` scrutinee, guard, clause body).
    fn atom(&mut self, e: &GExpr) {
        let atomic = matches!(
            e,
            GExpr::Lit(_)
                | GExpr::Ref { .. }
                | GExpr::Paren(_)
                | GExpr::Tuple(_)
                | GExpr::Case { arg: None, .. }
        );
        if atomic {
            self.expr(e);
        } else {
            self.out.push('(');
            self.expr(e);
            self.out.push(')');
        }
    }

    fn expr(&mut self, e: &GExpr) {
        match e {
            GExpr::Lit(n) => self.out.push_str(&n.to_string()),
            GExpr::Ref { name, target } => self.emit_ref(name, *target, RefKind::Value),
            GExpr::Case { name, target, arg } => {
                self.emit_ref(name, *target, RefKind::UnionCase);
                if let Some(a) = arg {
                    self.out.push(' ');
                    self.atom(a);
                }
            }
            GExpr::Paren(inner) => {
                self.out.push('(');
                self.expr(inner);
                self.out.push(')');
            }
            GExpr::Tuple(els) => {
                self.out.push('(');
                for (i, el) in els.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    // Elements use `atom`: a right-extending `fun`/`if` element
                    // would otherwise swallow the following comma and elements
                    // (`(fun x -> a, b)` parses as `(fun x -> (a, b))`),
                    // diverging from the generated scoping.
                    self.atom(el);
                }
                self.out.push(')');
            }
            GExpr::App(f, a) => {
                self.atom(f);
                self.out.push(' ');
                self.atom(a);
            }
            GExpr::If(c, t, e2) => {
                self.out.push_str("if ");
                self.atom(c);
                self.out.push_str(" then ");
                self.atom(t);
                self.out.push_str(" else ");
                self.atom(e2);
            }
            GExpr::Fun { param, body } => {
                self.out.push_str("fun ");
                self.emit_binder(param);
                self.out.push_str(" -> ");
                self.expr(body);
            }
            GExpr::Match { scrutinee, clauses } => {
                self.out.push_str("match ");
                self.atom(scrutinee);
                self.out.push_str(" with");
                self.clauses(clauses);
            }
            GExpr::Function { clauses } => {
                self.out.push_str("function");
                self.clauses(clauses);
            }
        }
    }

    fn clauses(&mut self, clauses: &[GClause]) {
        for c in clauses {
            self.out.push_str(" | ");
            self.pat(&c.pat);
            if let Some(g) = &c.guard {
                self.out.push_str(" when ");
                self.atom(g);
            }
            // The body is an `atom`: a right-extending `match`/`fun`/`if` body
            // would otherwise swallow the following `| …` clauses.
            self.out.push_str(" -> ");
            self.atom(&c.body);
        }
    }

    fn pat(&mut self, p: &GPat) {
        match p {
            GPat::Wild => self.out.push('_'),
            GPat::Lit(n) => self.out.push_str(&n.to_string()),
            GPat::Bind(b) => self.emit_binder(b),
            GPat::Alias { name, target } => self.emit_ref(name, *target, RefKind::OrAlias),
            GPat::Tuple(els) => {
                self.out.push('(');
                for (i, el) in els.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    // `,` binds tighter than `|`, so a bare alternation element
                    // would swallow the rest of the tuple.
                    if matches!(el, GPat::Or(_)) {
                        self.out.push('(');
                        self.pat(el);
                        self.out.push(')');
                    } else {
                        self.pat(el);
                    }
                }
                self.out.push(')');
            }
            GPat::Case { name, target, arg } => {
                self.emit_ref(name, *target, RefKind::UnionCase);
                if let Some(a) = arg {
                    self.out.push(' ');
                    self.pat_atom(a);
                }
            }
            GPat::Ap {
                name,
                target,
                arg,
                payload,
            } => {
                self.emit_ref(name, *target, RefKind::ActivePattern);
                if let Some(a) = arg {
                    self.out.push(' ');
                    match a {
                        GExpr::Ref { name, target } => {
                            self.emit_ref(name, *target, RefKind::ActivePatternArgument)
                        }
                        other => self.expr(other),
                    }
                }
                self.out.push(' ');
                self.pat_atom(payload);
            }
            GPat::As { inner, binder } => {
                // Self-parenthesising: `as` binds looser than `,` and tighter
                // than `|`, so both the inner pattern and the whole `as` need
                // brackets wherever they sit.
                self.out.push('(');
                self.pat_atom(inner);
                self.out.push_str(" as ");
                self.emit_binder(binder);
                self.out.push(')');
            }
            GPat::Or(alts) => {
                for (i, a) in alts.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(" | ");
                    }
                    self.pat(a);
                }
            }
        }
    }

    /// A sub-pattern in an argument position, parenthesised when it is an
    /// applied or alternated form.
    fn pat_atom(&mut self, p: &GPat) {
        let atomic = matches!(
            p,
            GPat::Wild
                | GPat::Lit(_)
                | GPat::Bind(_)
                | GPat::Alias { .. }
                | GPat::Tuple(_)
                | GPat::As { .. }
                | GPat::Case { arg: None, .. }
        );
        if atomic {
            self.pat(p);
        } else {
            self.out.push('(');
            self.pat(p);
            self.out.push(')');
        }
    }
}
