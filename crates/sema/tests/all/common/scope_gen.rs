//! A generator of random, **well-typed** F# programs over a scope graph:
//! modules nested two deep, `open`s of sibling, nested and `[<AutoOpen>]`
//! modules, classes with constructor parameters, `let` fields, instance and
//! static members, and expression blocks with every binding construct sema
//! models — `let`, `let rec … and …`, `use`, `for … in`, `for … to`, `while`,
//! `try … with`, `try … finally`, `match` with or-patterns, `if`, lambdas.
//!
//! [`generate`] interprets a tape of random numbers into a program together
//! with, **by construction**, the declaration every planted name use must
//! resolve to: a binder in the file, or [`Expected::External`] for a call that
//! falls through to FSharp.Core (`id`, `abs`, `max`, `min`) because no in-file
//! binder of that name is in scope. The model of F# scoping that decides this
//! is the generator's own, so the FCS differential checks it as well as the
//! resolver: FCS must report every planted use, at its range, declared exactly
//! where the generator says.
//!
//! # Well-typed by construction
//!
//! FCS's answer about a program it rejects is error recovery, not semantics,
//! so every program must type-check cleanly. The generator keeps that true by
//! typing every binder ([`Ty`]) and producing each `int` expression from a
//! binder according to its type: an `int` is used bare, an `exn` as
//! `x.HResult`, a class instance as `x.Get(…)` or `x.Other`, a function by
//! application. A use names the *latest* visible binder of its name, so
//! shadowing across types (a handler's `x : exn` hiding an `x : int`) is
//! exercised rather than avoided.
//!
//! The model's types are written into the program too: every parameter and
//! every function and module value carries its `int` annotation. Left to
//! inference, a recursive function whose body never constrains its result is
//! generic, and a module value computed from it then violates the value
//! restriction (`FS0030`), a reject the model would not predict.
//!
//! # Scoping facts the model encodes
//!
//! Each was checked against FCS before it was relied on:
//!
//! * a module-level name may not be defined twice in one module (`FS0037`),
//!   so module-level shadowing only happens across modules and `open`s;
//! * a nested module sees its enclosing modules' earlier values; once closed
//!   its values are reachable only qualified (`M.x`) or after `open M`;
//! * a closed `[<AutoOpen>]` module's values are visible unqualified in the
//!   rest of its parent, and `open Parent` brings them too;
//! * an `open` appends the module's values after everything already in scope,
//!   so it shadows earlier bindings and later bindings shadow it;
//! * in a class, a `let` field sees the constructor parameters and earlier
//!   fields (and may shadow a parameter), an instance member sees those, its
//!   self identifier and its own parameters, and a static member sees only
//!   module values and its own parameters.
//!
//! Where an `open` would bring one name from both a module and its
//! `[<AutoOpen>]` child, which one wins is not something the model claims to
//! know, so the name is marked ambiguous and no use of it is planted until a
//! later binder shadows it.

use std::collections::HashMap;

use rowan::TextRange;

use super::generator::seed_tape;

/// What a planted use must resolve to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expected {
    /// The binder with this uid in [`Generated::binder_ranges`].
    Binder(usize),
    /// A symbol declared outside the file (FSharp.Core).
    External,
}

/// How a planted use reaches its target: the binding construct for a direct
/// use, or the route (`open`, qualification) for a module value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RefKind {
    /// A value of the current or an enclosing module, unqualified.
    ModuleValue,
    /// A sibling of a `let rec … and …` group, from inside the group.
    LetRecSibling,
    /// A module value brought in by an explicit `open`.
    OpenedValue,
    /// A value folded in by an `[<AutoOpen>]` module: in the rest of its
    /// parent, or through an `open` of one of its ancestors.
    AutoOpenedValue,
    /// `M.x` / `M.N.x`.
    QualifiedValue,
    /// A block-level `let`.
    LocalLet,
    /// A function's parameter.
    FunctionParam,
    /// A lambda's parameter.
    LambdaParam,
    /// A union-case pattern's payload binder.
    MatchBinder,
    /// A later or-pattern alternative's spelling of the first one's binder.
    OrAlias,
    /// A `for … in` loop variable.
    ForInVar,
    /// A `for … = … to` loop variable.
    ForToVar,
    /// A `try … with` handler's binder.
    HandlerBinder,
    /// A `use` binder.
    UseBinder,
    /// A class's primary-constructor parameter, from a field or member.
    CtorParam,
    /// A class `let` field, from a later field or a member.
    ClassLet,
    /// An instance member's self identifier.
    SelfIdentifier,
    /// An instance member's parameter.
    MemberParam,
    /// A static member's parameter.
    StaticMemberParam,
    /// A union case, constructed or matched.
    UnionCase,
    /// `T.S(…)`: a static member through its type.
    StaticMember,
    /// `T(…)` / `new D(…)`: the class, as its constructor, which FCS
    /// declares at the type's name.
    Construction,
    /// A call FCS resolves to FSharp.Core because no in-file binder of the
    /// name is in scope.
    External,
}

impl RefKind {
    /// Every kind, for the census.
    pub const ALL: &'static [RefKind] = &[
        RefKind::ModuleValue,
        RefKind::LetRecSibling,
        RefKind::OpenedValue,
        RefKind::AutoOpenedValue,
        RefKind::QualifiedValue,
        RefKind::LocalLet,
        RefKind::FunctionParam,
        RefKind::LambdaParam,
        RefKind::MatchBinder,
        RefKind::OrAlias,
        RefKind::ForInVar,
        RefKind::ForToVar,
        RefKind::HandlerBinder,
        RefKind::UseBinder,
        RefKind::CtorParam,
        RefKind::ClassLet,
        RefKind::SelfIdentifier,
        RefKind::MemberParam,
        RefKind::StaticMemberParam,
        RefKind::UnionCase,
        RefKind::StaticMember,
        RefKind::Construction,
        RefKind::External,
    ];
}

/// Declared through a macro so [`Form::ALL`] cannot drift from the enum.
macro_rules! forms {
    ($($(#[$m:meta])* $v:ident,)*) => {
        /// A construct the generator can emit, tallied at the point its text
        /// is written, so a count means the construct is in the output.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Form {
            $($(#[$m])* $v,)*
        }
        impl Form {
            /// Every form.
            pub const ALL: &'static [Form] = &[$(Form::$v),*];
        }
    };
}

forms! {
    /// `module M =` inside the file's module.
    NestedModule,
    /// A module nested in a nested module.
    NestedModuleDepth2,
    /// `[<AutoOpen>] module M =`.
    AutoOpenModule,
    /// `open M`.
    Open,
    /// `open M.N`.
    OpenDotted,
    /// `let x = …` at module level.
    ModuleLetValue,
    /// `let f p = …` / `let h p q = …` at module level.
    ModuleLetFunction,
    /// `let rec f … and g …` at module level.
    ModuleLetRec,
    /// `type T(p: int) = …`.
    TypeDecl,
    /// A class `let` field.
    ClassLet,
    /// `member s.Get(q: int) : int = …`.
    InstanceMember,
    /// `static member S(a: int) : int = …`.
    StaticMemberDecl,
    /// A block-level `let x = …`.
    LocalLet,
    /// A block-level `let f p = …`.
    LocalFunction,
    /// A block-level `let rec … and …`.
    LocalLetRec,
    /// `use d = new D(…)`.
    Use,
    /// `for x in [ … ] do`.
    ForIn,
    /// `for x = … to … do`.
    ForTo,
    /// `while … do`.
    While,
    /// `try … with | x -> …`.
    TryWith,
    /// `try … finally …`.
    TryFinally,
    /// `match … with` over the union.
    Match,
    /// `| A x | B x ->`.
    OrPattern,
    /// `if … then … else …`.
    IfThenElse,
    /// `(fun x -> …) arg`.
    Lambda,
    /// `M.x`.
    Qualified,
    /// `T.S(…)`.
    StaticCall,
    /// `T(…)`, `new D(…)`.
    Construction,
    /// `t.Get(…)` / `t.Other` on an instance.
    InstanceCall,
    /// A call that falls through to FSharp.Core.
    ExternalCall,
    /// A use whose name has another, hidden binder in scope.
    ShadowedUse,
    /// A use whose binder hides one of a *different* type.
    ShadowedAcrossTypes,
}

/// One planted name use and what it must resolve to.
#[derive(Clone, Debug)]
pub struct PlantedRef {
    pub range: TextRange,
    pub expected: Expected,
    pub kind: RefKind,
    /// For a use that reaches its target through an `[<AutoOpen>]` fold, the
    /// binder a resolver that ignores the fold would name instead: the latest
    /// same-named binder beneath the target that no fold brought in.
    pub fold_back_fallback: Option<usize>,
}

/// The product of generation.
pub struct Generated {
    pub src: String,
    /// Binder uid → its defining source range.
    pub binder_ranges: HashMap<usize, TextRange>,
    pub refs: Vec<PlantedRef>,
    pub forms: HashMap<Form, usize>,
}

/// Interpret `seed`'s tape into a program.
pub fn generate_seed(seed: u32) -> Generated {
    generate(seed_tape(seed, 4096))
}

/// Interpret `nums` into a well-typed program over a scope graph.
pub fn generate(nums: Vec<u32>) -> Generated {
    let mut g = Gen {
        tape: nums,
        pos: 0,
        out: String::new(),
        next_uid: 0,
        next_module: 0,
        next_type: 0,
        binder_ranges: HashMap::new(),
        refs: Vec::new(),
        forms: HashMap::new(),
        modules: Vec::new(),
        types: Vec::new(),
        prelude: Prelude::default(),
    };
    g.program();
    Generated {
        src: g.out,
        binder_ranges: g.binder_ranges,
        refs: g.refs,
        forms: g.forms,
    }
}

/// A binder's type: what an `int` expression may do with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ty {
    Int,
    Exn,
    /// An instance of a generated class (every one has `Get` and `Other`).
    Obj,
    /// An instance of the prelude's disposable `D`.
    Disp,
    /// A value of the prelude's union `U`.
    Union,
    Fun1,
    Fun2,
}

/// How the binder came to be bound — the [`RefKind`] of a direct use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Site {
    ModuleLet,
    LocalLet,
    Param,
    Lambda,
    MatchBinder,
    ForIn,
    ForTo,
    Handler,
    Use,
    CtorParam,
    ClassLet,
    SelfId,
    MemberParam,
    StaticParam,
}

/// How a scope entry was brought into scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Via {
    Direct,
    /// From inside its own `let rec` group.
    RecGroup,
    Opened,
    /// By an explicit `open` of a module, from one of its `[<AutoOpen>]`
    /// descendants.
    OpenedFolded,
    AutoOpened,
}

#[derive(Clone, Debug)]
struct Binder {
    uid: usize,
    name: String,
    ty: Ty,
    site: Site,
}

#[derive(Clone, Debug)]
enum Entry {
    Bound(Binder, Via),
    /// The name is in scope but the model does not claim which binder wins.
    Ambiguous,
}

/// The names visible at a point, in binding order: a use names the last
/// entry of its name.
#[derive(Clone, Default)]
struct Scope {
    values: Vec<(String, Entry)>,
    /// Modules visible by name (names are unique, so no shadowing).
    modules: Vec<(String, usize)>,
    /// Classes visible by name, with the uids of their type name and `S`.
    types: Vec<(String, usize)>,
}

impl Scope {
    fn lookup(&self, name: &str) -> Option<&Entry> {
        self.values
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, e)| e)
    }

    /// Whether `name` has a hidden entry besides the visible one, and whether
    /// any hidden one has a different type.
    fn shadowing(&self, name: &str, visible_ty: Ty) -> (bool, bool) {
        let mut entries = self.values.iter().rev().filter(|(n, _)| n == name);
        entries.next();
        let mut any = false;
        let mut across = false;
        for (_, e) in entries {
            any = true;
            if let Entry::Bound(b, _) = e
                && b.ty != visible_ty
            {
                across = true;
            }
        }
        (any, across)
    }

    /// The latest binder of `name` beneath the visible one that was not
    /// brought in by an `[<AutoOpen>]` fold.
    fn unfolded_beneath(&self, name: &str) -> Option<usize> {
        let mut entries = self.values.iter().rev().filter(|(n, _)| n == name);
        entries.next();
        entries.find_map(|(_, e)| match e {
            Entry::Bound(b, via) if !matches!(via, Via::AutoOpened | Via::OpenedFolded) => {
                Some(b.uid)
            }
            _ => None,
        })
    }

    fn push(&mut self, b: &Binder, via: Via) {
        self.values
            .push((b.name.clone(), Entry::Bound(b.clone(), via)));
    }
}

struct ModuleInfo {
    name: String,
    /// Its own module-level values, in definition order.
    own: Vec<Binder>,
    /// Its nested modules, in definition order.
    children: Vec<usize>,
    autoopen: bool,
    /// Classes declared directly in it.
    types: Vec<usize>,
}

struct TypeInfo {
    name: String,
    /// The type name's binder: what a construction `T(…)` names.
    type_uid: usize,
    /// The static member `S`'s binder.
    static_uid: usize,
}

#[derive(Default)]
struct Prelude {
    disposable: usize,
    cases: [usize; 3],
}

const INT_NAMES: [&str; 4] = ["x", "y", "z", "n"];
const FUN1_NAMES: [&str; 4] = ["f", "g", "id", "abs"];
const FUN2_NAMES: [&str; 3] = ["h", "max", "min"];
/// The names FSharp.Core supplies when no in-file binder is in scope.
const EXTERNAL_FUN1: [&str; 2] = ["id", "abs"];
const EXTERNAL_FUN2: [&str; 2] = ["max", "min"];

const MAX_MODULE_DEPTH: usize = 2;
const MAX_BLOCK_DEPTH: usize = 3;
const MAX_SIMPLE_DEPTH: usize = 2;

struct Gen {
    tape: Vec<u32>,
    pos: usize,
    out: String,
    next_uid: usize,
    next_module: usize,
    next_type: usize,
    binder_ranges: HashMap<usize, TextRange>,
    refs: Vec<PlantedRef>,
    forms: HashMap<Form, usize>,
    modules: Vec<ModuleInfo>,
    types: Vec<TypeInfo>,
    prelude: Prelude,
}

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

impl Gen {
    // ---- tape ----

    fn next_num(&mut self) -> u32 {
        let v = self.tape.get(self.pos).copied().unwrap_or(0);
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
    /// The tape is exhausted: generation winds down to the cheapest forms.
    fn starved(&self) -> bool {
        self.pos >= self.tape.len()
    }

    // ---- emission ----

    fn form(&mut self, f: Form) {
        *self.forms.entry(f).or_default() += 1;
    }
    fn s(&mut self, text: &str) {
        self.out.push_str(text);
    }
    fn nl(&mut self, indent: usize) {
        self.out.push('\n');
        for _ in 0..indent {
            self.out.push(' ');
        }
    }
    fn binder(&mut self, name: &str, ty: Ty, site: Site) -> Binder {
        let uid = self.next_uid;
        self.next_uid += 1;
        let start = self.out.len();
        self.s(name);
        self.binder_ranges.insert(uid, span(start, self.out.len()));
        Binder {
            uid,
            name: name.to_string(),
            ty,
            site,
        }
    }
    /// Emit `text` as a planted use of `expected`.
    fn planted(&mut self, text: &str, expected: Expected, kind: RefKind) {
        let start = self.out.len();
        self.s(text);
        self.refs.push(PlantedRef {
            range: span(start, self.out.len()),
            expected,
            kind,
            fold_back_fallback: None,
        });
    }

    /// Emit a use of `name`, which names `b`, tallying shadowing.
    fn use_binder(&mut self, scope: &Scope, b: &Binder, via: Via) {
        let (shadowed, across) = scope.shadowing(&b.name, b.ty);
        if shadowed {
            self.form(Form::ShadowedUse);
        }
        if across {
            self.form(Form::ShadowedAcrossTypes);
        }
        let kind = match via {
            Via::Opened => RefKind::OpenedValue,
            Via::OpenedFolded | Via::AutoOpened => RefKind::AutoOpenedValue,
            Via::RecGroup => RefKind::LetRecSibling,
            Via::Direct => match b.site {
                Site::ModuleLet => RefKind::ModuleValue,
                Site::LocalLet => RefKind::LocalLet,
                Site::Param => RefKind::FunctionParam,
                Site::Lambda => RefKind::LambdaParam,
                Site::MatchBinder => RefKind::MatchBinder,
                Site::ForIn => RefKind::ForInVar,
                Site::ForTo => RefKind::ForToVar,
                Site::Handler => RefKind::HandlerBinder,
                Site::Use => RefKind::UseBinder,
                Site::CtorParam => RefKind::CtorParam,
                Site::ClassLet => RefKind::ClassLet,
                Site::SelfId => RefKind::SelfIdentifier,
                Site::MemberParam => RefKind::MemberParam,
                Site::StaticParam => RefKind::StaticMemberParam,
            },
        };
        let name = b.name.clone();
        self.planted(&name, Expected::Binder(b.uid), kind);
        let fallback = match via {
            Via::AutoOpened | Via::OpenedFolded => scope.unfolded_beneath(&b.name),
            _ => None,
        };
        self.refs
            .last_mut()
            .expect("just planted")
            .fold_back_fallback = fallback;
    }

    // ---- program ----

    fn program(&mut self) {
        self.s("module Top\n\ntype ");
        let d = self.binder("D", Ty::Disp, Site::ModuleLet);
        self.prelude.disposable = d.uid;
        self.s("(v: int) =\n    member _.V = v\n    interface System.IDisposable with\n        member _.Dispose() = ()\n\ntype U =");
        for (i, case) in ["A", "B", "C"].iter().enumerate() {
            self.s("\n    | ");
            let c = self.binder(case, Ty::Union, Site::ModuleLet);
            self.prelude.cases[i] = c.uid;
            if i < 2 {
                self.s(" of int");
            }
        }
        self.s("\n");
        let root = self.new_module("Top".to_string(), false);
        let mut scope = Scope::default();
        self.decls(&mut scope, root, 0, 0);
        self.s("\n");
    }

    fn new_module(&mut self, name: String, autoopen: bool) -> usize {
        self.modules.push(ModuleInfo {
            name,
            own: Vec::new(),
            children: Vec::new(),
            autoopen,
            types: Vec::new(),
        });
        self.modules.len() - 1
    }

    /// A fresh module-level name of `pool` not yet defined in module `m`
    /// (`FS0037`), if any is left.
    fn module_name(&mut self, m: usize, pool: &[&str]) -> Option<String> {
        let free: Vec<&str> = pool
            .iter()
            .copied()
            .filter(|n| !self.modules[m].own.iter().any(|b| b.name == *n))
            .collect();
        if free.is_empty() {
            return None;
        }
        Some(free[self.choice(free.len())].to_string())
    }

    /// The declarations of module `m`, each on its own line at `indent`.
    fn decls(&mut self, scope: &mut Scope, m: usize, indent: usize, depth: usize) {
        let n = if depth == 0 {
            self.between(4, 9)
        } else {
            self.between(1, 4)
        };
        for i in 0..n {
            if i > 0 || depth == 0 {
                self.nl(indent);
            }
            if depth == 0 && i > 0 {
                // A blank line between top-level declarations.
                self.nl(indent);
            }
            self.decl(scope, m, indent, depth);
        }
    }

    fn decl(&mut self, scope: &mut Scope, m: usize, indent: usize, depth: usize) {
        let choices = if self.starved() { 1 } else { 8 };
        match self.choice(choices) {
            0 | 1 => self.module_let(scope, m, indent),
            2 => self.module_let_rec(scope, m, indent),
            3 | 4 if depth < MAX_MODULE_DEPTH => self.nested_module(scope, m, indent, depth),
            5 => self.open(scope, m, indent),
            6 => self.type_decl(scope, m, indent),
            _ => self.module_let(scope, m, indent),
        }
    }

    fn module_let(&mut self, scope: &mut Scope, m: usize, indent: usize) {
        let arity = self.choice(3);
        let pool: &[&str] = match arity {
            0 => &INT_NAMES,
            1 => &FUN1_NAMES,
            _ => &FUN2_NAMES,
        };
        let Some(name) = self.module_name(m, pool) else {
            // Every name of the pool is taken in this module.
            self.s("do ()");
            return;
        };
        self.s("let ");
        let ty = [Ty::Int, Ty::Fun1, Ty::Fun2][arity];
        let b = self.binder(&name, ty, Site::ModuleLet);
        self.form(if arity == 0 {
            Form::ModuleLetValue
        } else {
            Form::ModuleLetFunction
        });
        let mut inner = scope.clone();
        let mut params: Vec<&str> = Vec::new();
        for _ in 0..arity {
            self.s(" ");
            // Distinct, or the head binds a name twice (`FS0038`).
            let free: Vec<&str> = INT_NAMES
                .iter()
                .copied()
                .filter(|n| !params.contains(n))
                .collect();
            let pname = free[self.choice(free.len())];
            params.push(pname);
            self.s("(");
            let p = self.binder(pname, Ty::Int, Site::Param);
            self.s(": int)");
            inner.push(&p, Via::Direct);
        }
        self.s(" : int =");
        self.nl(indent + 4);
        self.block(&mut inner, indent + 4, 0);
        scope.push(&b, Via::Direct);
        self.modules[m].own.push(b);
    }

    fn module_let_rec(&mut self, scope: &mut Scope, m: usize, indent: usize) {
        let Some(first) = self.module_name(m, &FUN1_NAMES) else {
            return self.module_let(scope, m, indent);
        };
        let second = FUN1_NAMES
            .iter()
            .copied()
            .filter(|n| *n != first && !self.modules[m].own.iter().any(|b| b.name == *n))
            .nth(0)
            .map(str::to_string);
        self.form(Form::ModuleLetRec);
        let group = self.rec_group(scope, indent, 0, &first, second.as_deref(), Site::ModuleLet);
        for b in group {
            scope.push(&b, Via::Direct);
            self.modules[m].own.push(b);
        }
    }

    /// `let rec f p = … and g q = …` (the `and` arm when `second` is given),
    /// each body seeing the whole group. Returns the group's binders.
    fn rec_group(
        &mut self,
        scope: &Scope,
        indent: usize,
        depth: usize,
        first: &str,
        second: Option<&str>,
        site: Site,
    ) -> Vec<Binder> {
        // Binders are emitted in source order, but each body must see all of
        // them, so the bodies are rendered into the group's scope only after
        // every head is known: render head 1, then body 1 needs head 2, which
        // comes later in the text. Allocate head 2's uid up front instead.
        let second_uid = second.map(|_| {
            let uid = self.next_uid;
            self.next_uid += 1;
            uid
        });
        self.s("let rec ");
        let f = self.binder(first, Ty::Fun1, site);
        let g = second.map(|name| Binder {
            uid: second_uid.unwrap(),
            name: name.to_string(),
            ty: Ty::Fun1,
            site,
        });
        let mut group_scope = scope.clone();
        group_scope.push(&f, Via::RecGroup);
        if let Some(g) = &g {
            group_scope.push(g, Via::RecGroup);
        }
        self.rec_body(&group_scope, indent, depth);
        if let Some(g) = &g {
            self.nl(indent);
            self.s("and ");
            let start = self.out.len();
            self.s(&g.name);
            self.binder_ranges
                .insert(g.uid, span(start, self.out.len()));
            self.rec_body(&group_scope, indent, depth);
        }
        let mut group = vec![f];
        group.extend(g);
        group
    }

    /// ` (p: int) : int =` and an indented body, one level deeper than the
    /// group, that sees `scope` and its parameter.
    fn rec_body(&mut self, scope: &Scope, indent: usize, depth: usize) {
        self.s(" (");
        let pname = INT_NAMES[self.choice(INT_NAMES.len())];
        let p = self.binder(pname, Ty::Int, Site::Param);
        self.s(": int) : int");
        let mut inner = scope.clone();
        inner.push(&p, Via::Direct);
        self.s(" =");
        self.nl(indent + 4);
        self.block(&mut inner, indent + 4, depth + 1);
    }

    fn nested_module(&mut self, scope: &mut Scope, m: usize, indent: usize, depth: usize) {
        let autoopen = self.choice(3) == 0;
        let name = format!("M{}", self.next_module);
        self.next_module += 1;
        if autoopen {
            self.form(Form::AutoOpenModule);
            self.s("[<AutoOpen>]");
            self.nl(indent);
        }
        self.form(if depth == 0 {
            Form::NestedModule
        } else {
            Form::NestedModuleDepth2
        });
        let child = self.new_module(name.clone(), autoopen);
        self.modules[m].children.push(child);
        self.s("module ");
        self.s(&name);
        self.s(" =");
        let mut inner = scope.clone();
        self.nl(indent + 4);
        self.decls(&mut inner, child, indent + 4, depth + 1);
        // Out of the module: its values are reachable only by name, unless it
        // is auto-opened into the rest of its parent.
        scope.modules.push((name, child));
        if autoopen {
            self.bring_module_values(scope, child, Via::AutoOpened);
            for t in self.modules[child].types.clone() {
                scope.types.push((self.types[t].name.clone(), t));
            }
        }
    }

    /// Every value module `m` contributes when opened — its own and its
    /// `[<AutoOpen>]` descendants' — appended to `scope`, with a name that two
    /// of them supply marked ambiguous.
    fn bring_module_values(&self, scope: &mut Scope, m: usize, via: Via) {
        let mut supplied: Vec<(Binder, bool)> = Vec::new();
        self.collect_opened_values(m, &mut supplied);
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for (b, _) in &supplied {
            *counts.entry(b.name.as_str()).or_default() += 1;
        }
        let ambiguous: Vec<String> = counts
            .iter()
            .filter(|(_, n)| **n > 1)
            .map(|(name, _)| name.to_string())
            .collect();
        for (b, folded) in &supplied {
            let via = match via {
                Via::Opened if *folded => Via::OpenedFolded,
                other => other,
            };
            // A contested name's binders stay beneath its ambiguity marker:
            // no use is planted through the marker, but the one the module
            // supplies directly is what a fold-blind resolver names
            // ([`Scope::unfolded_beneath`]).
            if !ambiguous.contains(&b.name) || via == Via::Opened {
                scope.push(b, via);
            }
        }
        for name in ambiguous {
            scope.values.push((name, Entry::Ambiguous));
        }
    }

    /// Module `m`'s own values, then its `[<AutoOpen>]` descendants', each
    /// with whether it came through an auto-open fold.
    fn collect_opened_values(&self, m: usize, out: &mut Vec<(Binder, bool)>) {
        self.collect_opened_values_at(m, false, out);
    }

    fn collect_opened_values_at(&self, m: usize, folded: bool, out: &mut Vec<(Binder, bool)>) {
        out.extend(self.modules[m].own.iter().map(|b| (b.clone(), folded)));
        for &c in &self.modules[m].children {
            if self.modules[c].autoopen {
                self.collect_opened_values_at(c, true, out);
            }
        }
    }

    fn open(&mut self, scope: &mut Scope, m: usize, indent: usize) {
        // Any module visible by name, or a child of one (`open M.N`).
        let mut targets: Vec<(String, usize)> = scope.modules.clone();
        for (path, id) in scope.modules.clone() {
            for &c in &self.modules[id].children {
                targets.push((format!("{path}.{}", self.modules[c].name), c));
            }
        }
        if targets.is_empty() {
            return self.module_let(scope, m, indent);
        }
        let (path, target) = targets[self.choice(targets.len())].clone();
        self.form(if path.contains('.') {
            Form::OpenDotted
        } else {
            Form::Open
        });
        self.s("open ");
        self.s(&path);
        self.bring_module_values(scope, target, Via::Opened);
        for &c in &self.modules[target].children.clone() {
            scope.modules.push((self.modules[c].name.clone(), c));
        }
        for t in self.modules[target].types.clone() {
            scope.types.push((self.types[t].name.clone(), t));
        }
    }

    fn type_decl(&mut self, scope: &mut Scope, m: usize, indent: usize) {
        self.form(Form::TypeDecl);
        let tname = format!("T{}", self.next_type);
        self.next_type += 1;
        self.s("type ");
        let tb = self.binder(&tname, Ty::Obj, Site::ModuleLet);
        self.s("(");
        let pname = INT_NAMES[self.choice(INT_NAMES.len())];
        let p = self.binder(pname, Ty::Int, Site::CtorParam);
        self.s(": int) =");
        let mut class_scope = scope.clone();
        class_scope.push(&p, Via::Direct);
        let mut field_names: Vec<String> = Vec::new();
        for _ in 0..self.between(0, 2) {
            let free: Vec<&str> = INT_NAMES
                .iter()
                .copied()
                .filter(|n| !field_names.iter().any(|f| f == n))
                .collect();
            let fname = free[self.choice(free.len())];
            self.form(Form::ClassLet);
            self.nl(indent + 4);
            self.s("let ");
            let fb = self.binder(fname, Ty::Int, Site::ClassLet);
            self.s(" = ");
            self.simple(&mut class_scope.clone(), 0);
            class_scope.push(&fb, Via::Direct);
            field_names.push(fname.to_string());
        }

        // `member s.Get(q: int) : int =` — the self identifier distinct from
        // the parameter, the constructor parameter and every field, so none of
        // them is bound twice.
        self.form(Form::InstanceMember);
        self.nl(indent + 4);
        self.s("member ");
        let free: Vec<&str> = INT_NAMES
            .iter()
            .copied()
            .filter(|n| *n != pname && !field_names.iter().any(|f| f == n))
            .collect();
        let self_name = free[self.choice(free.len())];
        let me = self.binder(self_name, Ty::Obj, Site::SelfId);
        self.s(".Get(");
        let qfree: Vec<&str> = INT_NAMES
            .iter()
            .copied()
            .filter(|n| *n != self_name)
            .collect();
        let qname = qfree[self.choice(qfree.len())];
        let q = self.binder(qname, Ty::Int, Site::MemberParam);
        self.s(": int) : int =");
        let mut member_scope = class_scope.clone();
        member_scope.push(&me, Via::Direct);
        member_scope.push(&q, Via::Direct);
        self.nl(indent + 8);
        self.block(&mut member_scope, indent + 8, 1);

        self.nl(indent + 4);
        self.s("member _.Other : int = ");
        self.simple(&mut class_scope.clone(), 0);

        self.form(Form::StaticMemberDecl);
        self.nl(indent + 4);
        self.s("static member ");
        let sb = self.binder("S", Ty::Fun1, Site::ModuleLet);
        self.s("(");
        let aname = INT_NAMES[self.choice(INT_NAMES.len())];
        let a = self.binder(aname, Ty::Int, Site::StaticParam);
        self.s(": int) : int =");
        let mut static_scope = scope.clone();
        static_scope.push(&a, Via::Direct);
        self.nl(indent + 8);
        self.block(&mut static_scope, indent + 8, 1);

        let t = self.types.len();
        self.types.push(TypeInfo {
            name: tname.clone(),
            type_uid: tb.uid,
            static_uid: sb.uid,
        });
        self.modules[m].types.push(t);
        scope.types.push((tname, t));
    }

    // ---- expressions ----

    /// An `int`-valued block at `indent`, its first line already indented.
    fn block(&mut self, scope: &mut Scope, indent: usize, depth: usize) {
        let choices = if depth >= MAX_BLOCK_DEPTH || self.starved() {
            1
        } else {
            14
        };
        match self.choice(choices) {
            0 | 1 => self.simple(scope, 0),
            2 => self.local_let(scope, indent, depth),
            3 => self.local_function(scope, indent, depth),
            4 => self.local_let_rec(scope, indent, depth),
            5 => self.use_block(scope, indent, depth),
            6 => self.for_in(scope, indent, depth),
            7 => self.for_to(scope, indent, depth),
            8 => self.while_block(scope, indent, depth),
            9 => self.try_with(scope, indent, depth),
            10 => self.try_finally(scope, indent, depth),
            11 => self.match_block(scope, indent, depth),
            12 => self.if_block(scope, indent, depth),
            13 if self.flip() => self.union_let(scope, indent, depth),
            _ => self.instance_let(scope, indent, depth),
        }
    }

    /// `let x = A …`: a union-typed binder, for a later `match x with`.
    fn union_let(&mut self, scope: &mut Scope, indent: usize, depth: usize) {
        self.form(Form::LocalLet);
        self.s("let ");
        let name = INT_NAMES[self.choice(INT_NAMES.len())];
        let b = self.binder(name, Ty::Union, Site::LocalLet);
        self.s(" = ");
        self.case_construction(&mut scope.clone());
        let mut inner = scope.clone();
        inner.push(&b, Via::Direct);
        self.rest(&mut inner, indent, depth);
    }

    /// The rest of a block after a statement or binding line.
    fn rest(&mut self, scope: &mut Scope, indent: usize, depth: usize) {
        self.nl(indent);
        self.block(scope, indent, depth + 1);
    }

    fn local_let(&mut self, scope: &mut Scope, indent: usize, depth: usize) {
        self.form(Form::LocalLet);
        self.s("let ");
        let name = INT_NAMES[self.choice(INT_NAMES.len())];
        let b = self.binder(name, Ty::Int, Site::LocalLet);
        self.s(" =");
        self.nl(indent + 4);
        self.block(&mut scope.clone(), indent + 4, depth + 1);
        let mut inner = scope.clone();
        inner.push(&b, Via::Direct);
        self.rest(&mut inner, indent, depth);
    }

    fn local_function(&mut self, scope: &mut Scope, indent: usize, depth: usize) {
        self.form(Form::LocalFunction);
        self.s("let ");
        let name = FUN1_NAMES[self.choice(FUN1_NAMES.len())];
        let b = self.binder(name, Ty::Fun1, Site::LocalLet);
        self.s(" (");
        let pname = INT_NAMES[self.choice(INT_NAMES.len())];
        let p = self.binder(pname, Ty::Int, Site::Param);
        self.s(": int) : int =");
        let mut body = scope.clone();
        body.push(&p, Via::Direct);
        self.nl(indent + 4);
        self.block(&mut body, indent + 4, depth + 1);
        let mut inner = scope.clone();
        inner.push(&b, Via::Direct);
        self.rest(&mut inner, indent, depth);
    }

    fn local_let_rec(&mut self, scope: &mut Scope, indent: usize, depth: usize) {
        self.form(Form::LocalLetRec);
        let first = FUN1_NAMES[self.choice(FUN1_NAMES.len())];
        let second = if self.flip() {
            FUN1_NAMES.iter().copied().find(|n| *n != first)
        } else {
            None
        };
        let group = self.rec_group(scope, indent, depth, first, second, Site::LocalLet);
        let mut inner = scope.clone();
        for b in &group {
            inner.push(b, Via::Direct);
        }
        self.rest(&mut inner, indent, depth);
    }

    fn use_block(&mut self, scope: &mut Scope, indent: usize, depth: usize) {
        self.form(Form::Use);
        self.form(Form::Construction);
        self.s("use ");
        let name = INT_NAMES[self.choice(INT_NAMES.len())];
        let b = self.binder(name, Ty::Disp, Site::Use);
        self.s(" = new ");
        let d = self.prelude.disposable;
        self.planted("D", Expected::Binder(d), RefKind::Construction);
        self.s("(");
        self.atom(&mut scope.clone(), 1);
        self.s(")");
        let mut inner = scope.clone();
        inner.push(&b, Via::Direct);
        self.rest(&mut inner, indent, depth);
    }

    fn instance_let(&mut self, scope: &mut Scope, indent: usize, depth: usize) {
        let Some((tname, _)) = self.pick_type(scope) else {
            return self.local_let(scope, indent, depth);
        };
        self.form(Form::Construction);
        self.s("let ");
        let name = INT_NAMES[self.choice(INT_NAMES.len())];
        let b = self.binder(name, Ty::Obj, Site::LocalLet);
        self.s(" = ");
        let t = scope.types.iter().find(|(n, _)| *n == tname).unwrap().1;
        let uid = self.types[t].type_uid;
        self.planted(&tname, Expected::Binder(uid), RefKind::Construction);
        self.s("(");
        self.atom(&mut scope.clone(), 1);
        self.s(")");
        let mut inner = scope.clone();
        inner.push(&b, Via::Direct);
        self.rest(&mut inner, indent, depth);
    }

    /// `ignore (…)` — a `unit` statement over `scope`.
    fn unit_body(&mut self, scope: &mut Scope, indent: usize) {
        self.nl(indent);
        self.s("ignore (");
        self.simple(scope, 0);
        self.s(")");
    }

    fn for_in(&mut self, scope: &mut Scope, indent: usize, depth: usize) {
        self.form(Form::ForIn);
        self.s("for ");
        let name = INT_NAMES[self.choice(INT_NAMES.len())];
        let b = self.binder(name, Ty::Int, Site::ForIn);
        self.s(" in [ ");
        self.atom(&mut scope.clone(), 1);
        self.s("; ");
        self.atom(&mut scope.clone(), 1);
        self.s(" ] do");
        let mut body = scope.clone();
        body.push(&b, Via::Direct);
        self.unit_body(&mut body, indent + 4);
        self.rest(scope, indent, depth);
    }

    fn for_to(&mut self, scope: &mut Scope, indent: usize, depth: usize) {
        self.form(Form::ForTo);
        self.s("for ");
        let name = INT_NAMES[self.choice(INT_NAMES.len())];
        let b = self.binder(name, Ty::Int, Site::ForTo);
        self.s(" = ");
        self.atom(&mut scope.clone(), 1);
        self.s(" to ");
        self.atom(&mut scope.clone(), 1);
        self.s(" do");
        let mut body = scope.clone();
        body.push(&b, Via::Direct);
        self.unit_body(&mut body, indent + 4);
        self.rest(scope, indent, depth);
    }

    fn while_block(&mut self, scope: &mut Scope, indent: usize, depth: usize) {
        self.form(Form::While);
        self.s("while ");
        self.atom(&mut scope.clone(), 1);
        self.s(" > ");
        self.atom(&mut scope.clone(), 1);
        self.s(" do");
        self.unit_body(&mut scope.clone(), indent + 4);
        self.rest(scope, indent, depth);
    }

    fn try_with(&mut self, scope: &mut Scope, indent: usize, depth: usize) {
        self.form(Form::TryWith);
        self.s("try");
        self.nl(indent + 4);
        self.block(&mut scope.clone(), indent + 4, depth + 1);
        self.nl(indent);
        self.s("with");
        self.nl(indent);
        self.s("| ");
        let name = INT_NAMES[self.choice(INT_NAMES.len())];
        let b = self.binder(name, Ty::Exn, Site::Handler);
        self.s(" ->");
        let mut handler = scope.clone();
        handler.push(&b, Via::Direct);
        self.nl(indent + 4);
        self.block(&mut handler, indent + 4, depth + 1);
    }

    fn try_finally(&mut self, scope: &mut Scope, indent: usize, depth: usize) {
        self.form(Form::TryFinally);
        self.s("try");
        self.nl(indent + 4);
        self.block(&mut scope.clone(), indent + 4, depth + 1);
        self.nl(indent);
        self.s("finally");
        self.unit_body(&mut scope.clone(), indent + 4);
    }

    /// The scrutinee: a union-typed binder in scope, or a construction.
    fn union_value(&mut self, scope: &mut Scope) {
        let unions: Vec<Binder> = self.visible_of(scope, |t| t == Ty::Union);
        if !unions.is_empty() && self.flip() {
            let b = unions[self.choice(unions.len())].clone();
            let via = self.via_of(scope, &b);
            self.use_binder(scope, &b, via);
        } else {
            self.s("(");
            self.case_construction(scope);
            self.s(")");
        }
    }

    fn case_construction(&mut self, scope: &mut Scope) {
        let i = self.choice(3);
        let uid = self.prelude.cases[i];
        self.planted(
            ["A", "B", "C"][i],
            Expected::Binder(uid),
            RefKind::UnionCase,
        );
        if i < 2 {
            self.s(" ");
            self.atom(scope, 1);
        }
    }

    fn match_block(&mut self, scope: &mut Scope, indent: usize, depth: usize) {
        self.form(Form::Match);
        self.s("match ");
        self.union_value(scope);
        self.s(" with");
        self.nl(indent);
        self.s("| ");
        let name = INT_NAMES[self.choice(INT_NAMES.len())];
        let [a, b, c] = self.prelude.cases;
        self.planted("A", Expected::Binder(a), RefKind::UnionCase);
        self.s(" ");
        let first = self.binder(name, Ty::Int, Site::MatchBinder);
        if self.flip() {
            self.form(Form::OrPattern);
            self.s(" | ");
            self.planted("B", Expected::Binder(b), RefKind::UnionCase);
            self.s(" ");
            self.planted(name, Expected::Binder(first.uid), RefKind::OrAlias);
            self.s(" ->");
        } else {
            self.s(" ->");
        }
        let mut arm = scope.clone();
        arm.push(&first, Via::Direct);
        self.nl(indent + 4);
        self.block(&mut arm, indent + 4, depth + 1);
        // The remaining cases bind nothing.
        self.nl(indent);
        self.s("| ");
        self.planted("C", Expected::Binder(c), RefKind::UnionCase);
        self.s(" ->");
        self.nl(indent + 4);
        self.block(&mut scope.clone(), indent + 4, depth + 1);
        self.nl(indent);
        self.s("| _ ->");
        self.nl(indent + 4);
        self.block(&mut scope.clone(), indent + 4, depth + 1);
    }

    fn if_block(&mut self, scope: &mut Scope, indent: usize, depth: usize) {
        self.form(Form::IfThenElse);
        self.s("if ");
        self.atom(&mut scope.clone(), 1);
        self.s(" = ");
        self.atom(&mut scope.clone(), 1);
        self.s(" then");
        self.nl(indent + 4);
        self.block(&mut scope.clone(), indent + 4, depth + 1);
        self.nl(indent);
        self.s("else");
        self.nl(indent + 4);
        self.block(&mut scope.clone(), indent + 4, depth + 1);
    }

    /// A single-line `int` expression.
    fn simple(&mut self, scope: &mut Scope, depth: usize) {
        let choices = if depth >= MAX_SIMPLE_DEPTH || self.starved() {
            2
        } else {
            9
        };
        match self.choice(choices) {
            0 => self.atom(scope, depth),
            1 => self.value_use(scope, depth),
            2 => {
                self.atom(scope, depth + 1);
                self.s(" + ");
                self.atom(scope, depth + 1);
            }
            3 => self.lambda(scope, depth),
            4 => self.qualified(scope, depth),
            5 => self.static_call(scope, depth),
            6 => self.external_call(scope, depth),
            7 => self.function_call(scope, depth),
            _ => self.construct_and_get(scope, depth),
        }
    }

    /// A literal, a bare `int` use, or a parenthesised [`Self::simple`].
    fn atom(&mut self, scope: &mut Scope, depth: usize) {
        if depth >= MAX_SIMPLE_DEPTH || self.starved() {
            let ints = self.visible_of(scope, |t| t == Ty::Int);
            if !ints.is_empty() && self.flip() {
                let b = ints[self.choice(ints.len())].clone();
                let via = self.via_of(scope, &b);
                return self.use_binder(scope, &b, via);
            }
            let lit = self.choice(10).to_string();
            return self.s(&lit);
        }
        match self.choice(3) {
            0 => {
                let lit = self.choice(10).to_string();
                self.s(&lit);
            }
            1 => {
                let ints = self.visible_of(scope, |t| t == Ty::Int);
                if ints.is_empty() {
                    self.s("0");
                } else {
                    let b = ints[self.choice(ints.len())].clone();
                    let via = self.via_of(scope, &b);
                    self.use_binder(scope, &b, via);
                }
            }
            _ => {
                self.s("(");
                self.simple(scope, depth + 1);
                self.s(")");
            }
        }
    }

    /// The binders visible by their name (each the latest of its name) whose
    /// type satisfies `want`.
    fn visible_of(&self, scope: &Scope, want: impl Fn(Ty) -> bool) -> Vec<Binder> {
        let mut seen: Vec<&str> = Vec::new();
        let mut out = Vec::new();
        for (name, entry) in scope.values.iter().rev() {
            if seen.contains(&name.as_str()) {
                continue;
            }
            seen.push(name);
            if let Entry::Bound(b, _) = entry
                && want(b.ty)
            {
                out.push(b.clone());
            }
        }
        out
    }

    fn via_of(&self, scope: &Scope, b: &Binder) -> Via {
        match scope.lookup(&b.name) {
            Some(Entry::Bound(found, via)) if found.uid == b.uid => *via,
            other => panic!(
                "{} is not the visible binder of its name: {other:?}",
                b.name
            ),
        }
    }

    /// Any visible binder used as an `int`, by its type.
    fn value_use(&mut self, scope: &mut Scope, depth: usize) {
        let usable = self.visible_of(scope, |t| t != Ty::Union);
        if usable.is_empty() {
            return self.atom(scope, depth);
        }
        let b = usable[self.choice(usable.len())].clone();
        let via = self.via_of(scope, &b);
        match b.ty {
            Ty::Int => self.use_binder(scope, &b, via),
            Ty::Exn => {
                self.use_binder(scope, &b, via);
                self.s(".HResult");
            }
            Ty::Disp => {
                self.use_binder(scope, &b, via);
                self.s(".V");
            }
            Ty::Obj => {
                self.form(Form::InstanceCall);
                self.use_binder(scope, &b, via);
                if self.flip() {
                    self.s(".Other");
                } else {
                    self.s(".Get(");
                    self.atom(scope, depth + 1);
                    self.s(")");
                }
            }
            Ty::Fun1 => {
                self.use_binder(scope, &b, via);
                self.s(" ");
                self.atom(scope, depth + 1);
            }
            Ty::Fun2 => {
                self.use_binder(scope, &b, via);
                self.s(" ");
                self.atom(scope, depth + 1);
                self.s(" ");
                self.atom(scope, depth + 1);
            }
            Ty::Union => unreachable!("filtered out"),
        }
    }

    fn function_call(&mut self, scope: &mut Scope, depth: usize) {
        let funs = self.visible_of(scope, |t| matches!(t, Ty::Fun1 | Ty::Fun2));
        if funs.is_empty() {
            return self.external_call(scope, depth);
        }
        let b = funs[self.choice(funs.len())].clone();
        let via = self.via_of(scope, &b);
        self.use_binder(scope, &b, via);
        for _ in 0..if b.ty == Ty::Fun1 { 1 } else { 2 } {
            self.s(" ");
            self.atom(scope, depth + 1);
        }
    }

    /// `id …` / `max … …` where no in-file binder of the name is in scope, so
    /// FCS resolves it to FSharp.Core.
    fn external_call(&mut self, scope: &mut Scope, depth: usize) {
        let mut candidates: Vec<(&str, usize)> = Vec::new();
        for n in EXTERNAL_FUN1 {
            if scope.lookup(n).is_none() {
                candidates.push((n, 1));
            }
        }
        for n in EXTERNAL_FUN2 {
            if scope.lookup(n).is_none() {
                candidates.push((n, 2));
            }
        }
        if candidates.is_empty() {
            return self.atom(scope, depth);
        }
        let (name, arity) = candidates[self.choice(candidates.len())];
        self.form(Form::ExternalCall);
        self.planted(name, Expected::External, RefKind::External);
        for _ in 0..arity {
            self.s(" ");
            self.atom(scope, depth + 1);
        }
    }

    fn lambda(&mut self, scope: &mut Scope, depth: usize) {
        self.form(Form::Lambda);
        self.s("(fun ");
        let name = INT_NAMES[self.choice(INT_NAMES.len())];
        self.s("(");
        let b = self.binder(name, Ty::Int, Site::Lambda);
        self.s(": int) -> ");
        let mut body = scope.clone();
        body.push(&b, Via::Direct);
        self.simple(&mut body, depth + 1);
        self.s(") ");
        self.atom(scope, depth + 1);
    }

    /// `M.x` / `M.N.x`: an `int` value of a module visible by name, not
    /// contested by one of its `[<AutoOpen>]` descendants.
    fn qualified(&mut self, scope: &mut Scope, depth: usize) {
        let mut paths: Vec<(String, usize)> = scope.modules.clone();
        for (path, id) in scope.modules.clone() {
            for &c in &self.modules[id].children {
                paths.push((format!("{path}.{}", self.modules[c].name), c));
            }
        }
        let mut options: Vec<(String, Binder)> = Vec::new();
        for (path, id) in &paths {
            let mut supplied = Vec::new();
            self.collect_opened_values(*id, &mut supplied);
            for b in &self.modules[*id].own {
                let contested = supplied.iter().filter(|(s, _)| s.name == b.name).count() > 1;
                if b.ty == Ty::Int && !contested {
                    options.push((path.clone(), b.clone()));
                }
            }
        }
        if options.is_empty() {
            return self.atom(scope, depth);
        }
        let (path, b) = options[self.choice(options.len())].clone();
        self.form(Form::Qualified);
        self.planted(
            &format!("{path}.{}", b.name),
            Expected::Binder(b.uid),
            RefKind::QualifiedValue,
        );
    }

    fn pick_type(&mut self, scope: &Scope) -> Option<(String, usize)> {
        if scope.types.is_empty() {
            return None;
        }
        Some(scope.types[self.choice(scope.types.len())].clone())
    }

    /// `T.S(…)`: FCS reports the static member at the whole `T.S` span.
    fn static_call(&mut self, scope: &mut Scope, depth: usize) {
        let Some((tname, t)) = self.pick_type(scope) else {
            return self.atom(scope, depth);
        };
        self.form(Form::StaticCall);
        let uid = self.types[t].static_uid;
        self.planted(
            &format!("{tname}.S"),
            Expected::Binder(uid),
            RefKind::StaticMember,
        );
        self.s("(");
        self.atom(scope, depth + 1);
        self.s(")");
    }

    /// `(T(…)).Other` — a construction, whose head FCS reports as the
    /// constructor declared at the type's name.
    fn construct_and_get(&mut self, scope: &mut Scope, depth: usize) {
        let Some((tname, t)) = self.pick_type(scope) else {
            return self.atom(scope, depth);
        };
        self.form(Form::Construction);
        self.form(Form::InstanceCall);
        let uid = self.types[t].type_uid;
        self.s("(");
        self.planted(&tname, Expected::Binder(uid), RefKind::Construction);
        self.s("(");
        self.atom(scope, depth + 1);
        self.s(")).Other");
    }
}
