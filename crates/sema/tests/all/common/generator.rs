//! A generator of random *well-scoped* F# programs within the current parser
//! subset, used by the resolver tests.
//!
//! A [`generate`] call interprets a tape of random numbers deterministically
//! into a program in which every reference targets the latest in-scope binder
//! of a chosen name. It returns the rendered source plus, by construction, the
//! exact binder each reference must resolve to — so it is its own oracle for
//! the scoping *model* (used FCS-free by `resolve_scoping.rs`) and a source of
//! random inputs for the FCS differential (`resolve_diff.rs`).
//!
//! Because the interpreter always produces a valid, scope-correct program for
//! *any* tape, shrinking the tape (proptest) or varying a seed never yields
//! garbage.

use std::collections::HashMap;

use rowan::TextRange;

/// The product of generation: source text and the resolutions it must induce.
pub struct Generated {
    pub src: String,
    /// Binder uid → its defining source range.
    pub binder_ranges: HashMap<usize, TextRange>,
    /// Each reference's (source range, target binder uid).
    pub refs: Vec<(TextRange, usize)>,
}

/// Interpret `nums` into a well-scoped program.
pub fn generate(nums: Vec<u32>) -> Generated {
    let mut g = Gen {
        tape: Tape { nums, pos: 0 },
        next_uid: 0,
    };
    let program = g.program();
    let mut render = Render::default();
    render.program(&program);
    Generated {
        src: render.out,
        binder_ranges: render.binder_ranges,
        refs: render.refs,
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
    Ref { name: String, target: usize },
    Paren(Box<GExpr>),
    Tuple(Vec<GExpr>),
    App(Box<GExpr>, Box<GExpr>),
    If(Box<GExpr>, Box<GExpr>, Box<GExpr>),
    Fun { param: Binder, body: Box<GExpr> },
}

enum GHead {
    Value(Binder),
    Func(Binder, Vec<Binder>),
}

struct GBinding {
    rec: bool,
    head: GHead,
    rhs: GExpr,
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
}

const MAX_DEPTH: usize = 3;

impl Gen {
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

    fn program(&mut self) -> Vec<GBinding> {
        let n = self.tape.between(1, 6);
        let mut top: Vec<Binder> = Vec::new();
        let mut prog = Vec::new();
        for _ in 0..n {
            let rec = self.tape.flip();
            let is_func = self.tape.flip();
            // Sometimes reuse an existing top-level name to exercise shadowing.
            let head = if !top.is_empty() && self.tape.flip() {
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
            prog.push(GBinding {
                rec,
                head: ghead,
                rhs,
            });
            top.push(head);
        }
        prog
    }

    fn expr(&mut self, scope: &[Binder], depth: usize) -> GExpr {
        let forms = if depth >= MAX_DEPTH { 2 } else { 7 };
        match self.tape.choice(forms) {
            0 => GExpr::Lit(self.tape.choice(10) as u32),
            1 => self.reference(scope),
            2 => GExpr::Paren(Box::new(self.expr(scope, depth + 1))),
            3 => {
                let k = self.tape.between(2, 3);
                GExpr::Tuple((0..k).map(|_| self.expr(scope, depth + 1)).collect())
            }
            4 => GExpr::App(
                Box::new(self.expr(scope, depth + 1)),
                Box::new(self.expr(scope, depth + 1)),
            ),
            5 => GExpr::If(
                Box::new(self.expr(scope, depth + 1)),
                Box::new(self.expr(scope, depth + 1)),
                Box::new(self.expr(scope, depth + 1)),
            ),
            6 => {
                // A lambda whose parameter may shadow an in-scope name.
                let param = if !scope.is_empty() && self.tape.flip() {
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
            _ => unreachable!(),
        }
    }

    /// A reference to a random in-scope name, recording the *latest* binder of
    /// that name (the one position-ordered shadowing must pick).
    fn reference(&mut self, scope: &[Binder]) -> GExpr {
        if scope.is_empty() {
            return GExpr::Lit(0);
        }
        let name = scope[self.tape.choice(scope.len())].name.clone();
        let target = scope.iter().rev().find(|b| b.name == name).unwrap().uid;
        GExpr::Ref { name, target }
    }
}

/// Render a program to F# source, recording every binder's range (by uid) and
/// every reference's (range, target-uid).
#[derive(Default)]
struct Render {
    out: String,
    binder_ranges: HashMap<usize, TextRange>,
    refs: Vec<(TextRange, usize)>,
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
    fn emit_ref(&mut self, name: &str, target: usize) {
        let start = self.out.len();
        self.out.push_str(name);
        let end = self.out.len();
        self.refs.push((Self::span(start, end), target));
    }

    fn program(&mut self, prog: &[GBinding]) {
        for b in prog {
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
            }
            self.out.push_str(" = ");
            self.expr(&b.rhs);
            self.out.push('\n');
        }
    }

    /// Render a sub-expression in a position where a compound form needs
    /// parenthesising (application argument, `if` sub-expression, tuple
    /// element).
    fn atom(&mut self, e: &GExpr) {
        let atomic = matches!(
            e,
            GExpr::Lit(_) | GExpr::Ref { .. } | GExpr::Paren(_) | GExpr::Tuple(_)
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
            GExpr::Ref { name, target } => self.emit_ref(name, *target),
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
        }
    }
}
