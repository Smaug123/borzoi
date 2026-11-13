//! Generated multi-file projects and the edit scripts run over them: the
//! scenarios the incremental folds' differentials share
//! (`resolve_incremental_diff.rs`, `infer_incremental_diff.rs`).
//!
//! A project is a manipulable model ([`File`], [`Decl`], [`Body`]) rendered
//! to F# source. Every choice is read off a number tape ([`generate`]), so
//! proptest's shrinking of the tape simplifies a scenario without ever
//! producing unparseable source.
//!
//! The edits cover two kinds of change, which the incremental folds must
//! tell apart: ones that change a file's exports (an added or removed
//! declaration, an attribute, an `[<AutoOpen>]` module), and ones that leave
//! the exports as they were while changing what a later file reads through
//! them — a body whose type changes (`1` to `"s"` to `fun q -> q`), a union
//! case gaining a payload, a record made generic.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};

// ============================================================================
// Parse helper
// ============================================================================

/// Parse a generated source file to an [`ImplFile`], asserting it is parse-clean
/// — the generator only emits well-formed F#, so a parse error is a generator
/// bug, not a case we want to fold.
pub fn impl_file(src: &str) -> ImplFile {
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "generator emitted un-parseable source:\n{src}\nerrors: {:?}",
        p.errors
    );
    ImplFile::cast(p.root).expect("impl file")
}

// ============================================================================
// Project model — a manipulable AST rendered to F# source
// ============================================================================

/// The right-hand side of a value/function binding.
#[derive(Clone, PartialEq)]
pub enum Body {
    /// A literal — `1`. Edits bump it to perturb a body without touching exports.
    Lit(u32),
    /// A cross-file reference `M{file}.{name}` to an earlier file's value. May
    /// dangle after an edit removes the target's export; both folds see the same
    /// dangling reference, so the equality still holds — and the incremental fold
    /// must recompute the referrer, which is the point.
    Ref { file: u32, name: String },
    /// `(let z = {n} in z)` — a body that binds a local, so editing between it
    /// and a literal renumbers every def after it in the file, the types
    /// declared later included, while leaving the exports as they were.
    LetIn(u32),
    /// A body of another type, by [`OTHER_SHAPES`] index: editing between it
    /// and a literal changes the type the binder publishes while leaving the
    /// exports as they were.
    Other(u32),
    /// `M{file}.A{id}` — a case of an earlier file's union, typed by what
    /// that file publishes for the case.
    Case { file: u32, id: u32 },
    /// `px.F{id}` in a function whose parameter is annotated `R{id}`: a field
    /// read of an earlier file's record, typed by what that file publishes
    /// for the record. Anywhere else it is a bare `px` (a function) or `0`.
    Field,
}

/// The bodies [`Body::Other`] renders: each a value of its own type, a
/// generic one among them.
pub const OTHER_SHAPES: &[&str] = &[
    "\"s\"",
    "true",
    "1.0",
    "(1, \"a\")",
    "fun (q: int) -> q",
    "fun q -> q",
    "None",
];

#[derive(Clone, PartialEq)]
pub enum Decl {
    /// `let {name} = {body}`, optionally attributed (`[<System.Obsolete>]`). The
    /// attribute flips the file's extension-source signal *without* changing its
    /// exports — the case that needs the incremental fold's separate
    /// augmentation/attribute check, not just `same_export_contribution`.
    Val {
        name: String,
        attr: bool,
        body: Body,
    },
    /// `let {name} px = {body}` — a curried function export; `param` annotates
    /// `px` with an earlier file's record `R{param}`, a type-position use of an
    /// earlier file's type.
    Func {
        name: String,
        param: Option<u32>,
        body: Body,
    },
    /// `type R{id} = { F{id} : int }` — a record type, declared at namespace
    /// level after the module in a namespace file, so the defs of every body
    /// before it number ahead of it. Edits make it generic or `private`, which
    /// leaves its exported path as it was and changes what a later file's use
    /// of it resolves to.
    Record {
        id: u32,
        generic: bool,
        private: bool,
    },
    /// `type U{id} = A{id} | B{id}` — a union type plus its two constructor
    /// cases, exercising the type / qualified-case export channels. With
    /// `payload`, `A{id}` carries an `int`: the case exports are the same,
    /// what a later use of `A{id}` is typed as is not.
    Union { id: u32, payload: bool },
}

#[derive(Clone, PartialEq)]
pub struct File {
    pub uid: u32,
    /// Whether the file declares a nested `[<AutoOpen>] module Aut{uid}` —
    /// contributes both an exportable auto-open path and the extension signal.
    pub auto_open: bool,
    /// Whether the file is `namespace Lib` holding `module M{uid}`, rather than
    /// a top-level `module M{uid}`: a later namespace file sees its records
    /// bare, through the enclosing namespace.
    pub namespace: bool,
    pub decls: Vec<Decl>,
}

impl File {
    /// The value/function names this file exports, in order — the pool an edit's
    /// cross-file reference may target.
    fn value_names(&self) -> Vec<String> {
        self.decls
            .iter()
            .filter_map(|d| match d {
                Decl::Val { name, .. } | Decl::Func { name, .. } => Some(name.clone()),
                Decl::Union { .. } | Decl::Record { .. } => None,
            })
            .collect()
    }

    fn render(&self) -> String {
        let (mut out, pad) = if self.namespace {
            (
                format!(
                    "namespace Lib\nmodule M{} =\n    let anchor = 0\n",
                    self.uid
                ),
                "    ",
            )
        } else {
            (format!("module M{}\n", self.uid), "")
        };
        let mut records = String::new();
        for decl in &self.decls {
            match decl {
                Decl::Val { name, attr, body } => {
                    if *attr {
                        out.push_str(&format!("{pad}[<System.Obsolete>]\n"));
                    }
                    out.push_str(&format!("{pad}let {name} = {}\n", render_body(body, None)));
                }
                Decl::Func { name, param, body } => {
                    let px = match param {
                        Some(id) => format!("(px : R{id})"),
                        None => "px".to_string(),
                    };
                    out.push_str(&format!(
                        "{pad}let {name} {px} = {}\n",
                        render_body(body, Some(*param))
                    ));
                }
                Decl::Union { id, payload } => {
                    let of = if *payload { " of int" } else { "" };
                    out.push_str(&format!("{pad}type U{id} = A{id}{of} | B{id}\n"));
                }
                Decl::Record {
                    id,
                    generic,
                    private,
                } => {
                    let access = if *private { "private " } else { "" };
                    let record = if *generic {
                        format!("type {access}R{id}<'a> = {{ F{id} : 'a }}\n")
                    } else {
                        format!("type {access}R{id} = {{ F{id} : int }}\n")
                    };
                    if self.namespace {
                        records.push_str(&record);
                    } else {
                        out.push_str(&record);
                    }
                }
            }
        }
        if self.auto_open {
            out.push_str(&format!(
                "{pad}[<AutoOpen>]\n{pad}module Aut{} =\n{pad}    let auto = 1\n",
                self.uid
            ));
        }
        out.push_str(&records);
        out
    }

    /// The unions this file declares, the pool a later file's case use may
    /// name.
    fn union_ids(&self) -> Vec<u32> {
        self.decls
            .iter()
            .filter_map(|d| match d {
                Decl::Union { id, .. } => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// The records this file declares, the pool a later file's annotation may
    /// name.
    fn record_ids(&self) -> Vec<u32> {
        self.decls
            .iter()
            .filter_map(|d| match d {
                Decl::Record { id, .. } => Some(*id),
                _ => None,
            })
            .collect()
    }
}

/// Render a binding's body. `func` is the parameter annotation of the
/// function the body belongs to, `None` for a value.
fn render_body(body: &Body, func: Option<Option<u32>>) -> String {
    match body {
        Body::Lit(n) => n.to_string(),
        Body::Ref { file, name } => format!("M{file}.{name}"),
        Body::LetIn(n) => format!("(let z = {n} in z)"),
        Body::Other(n) => OTHER_SHAPES[*n as usize % OTHER_SHAPES.len()].to_string(),
        Body::Case { file, id } => format!("M{file}.A{id}"),
        Body::Field => match func {
            Some(Some(id)) => format!("px.F{id}"),
            Some(None) => "px".to_string(),
            None => "0".to_string(),
        },
    }
}

pub fn render_all(files: &[File]) -> Vec<String> {
    files.iter().map(File::render).collect()
}

// ============================================================================
// Tape-driven generator
// ============================================================================

/// A deterministic reader over a random number tape — every choice is derived
/// from it, so shrinking the tape (proptest) monotonically simplifies the
/// generated scenario without ever producing garbage.
struct Tape {
    nums: Vec<u32>,
    pos: usize,
}

impl Tape {
    fn next(&mut self) -> u32 {
        let v = self.nums.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        v
    }
    fn choice(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { self.next() as usize % n }
    }
    fn between(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.choice(hi - lo + 1)
    }
    fn flip(&mut self) -> bool {
        self.next().is_multiple_of(2)
    }
}

/// Fresh, globally-unique names so an added declaration is always a distinct
/// export and no within-file collision muddies the exercised channels.
struct Names {
    value: u32,
    union: u32,
}

impl Names {
    fn value(&mut self) -> String {
        let n = self.value;
        self.value += 1;
        format!("v{n}")
    }
    fn func(&mut self) -> String {
        let n = self.value;
        self.value += 1;
        format!("f{n}")
    }
    fn union(&mut self) -> u32 {
        let n = self.union;
        self.union += 1;
        n
    }
}

/// A generated scenario: an initial project and the source snapshots after each
/// edit. Every snapshot has the same file count and order (the generator holds
/// the file set fixed — the LSP only calls the incremental fold on text-sync,
/// which never changes the Compile order).
pub struct Scenario {
    /// The initial project, as a model a property can edit further.
    pub initial: Vec<File>,
    pub snapshots: Vec<Vec<String>>,
}

pub fn generate(nums: Vec<u32>) -> Scenario {
    let mut t = Tape { nums, pos: 0 };
    let mut names = Names { value: 0, union: 0 };
    let n_files = t.between(2, 4);

    let mut files: Vec<File> = Vec::with_capacity(n_files);
    for uid in 0..n_files {
        let file = initial_file(&mut t, &mut names, uid as u32, &files);
        files.push(file);
    }

    let initial = files.clone();
    let mut snapshots = vec![render_all(&files)];
    let n_edits = t.between(1, 6);
    for _ in 0..n_edits {
        apply_edit(&mut t, &mut names, &mut files);
        snapshots.push(render_all(&files));
    }
    Scenario { initial, snapshots }
}

fn initial_file(t: &mut Tape, names: &mut Names, uid: u32, earlier: &[File]) -> File {
    let auto_open = t.flip();
    let namespace = t.flip();
    let n_decls = t.between(0, 3);
    let mut decls = Vec::with_capacity(n_decls);
    for _ in 0..n_decls {
        decls.push(gen_decl(t, names, earlier));
    }
    File {
        uid,
        auto_open,
        namespace,
        decls,
    }
}

fn gen_decl(t: &mut Tape, names: &mut Names, earlier: &[File]) -> Decl {
    match t.choice(4) {
        0 => Decl::Val {
            name: names.value(),
            attr: t.flip(),
            body: gen_body(t, earlier),
        },
        1 => Decl::Func {
            name: names.func(),
            param: gen_param(t, earlier),
            body: gen_body(t, earlier),
        },
        2 => Decl::Record {
            id: names.union(),
            generic: false,
            private: false,
        },
        _ => Decl::Union {
            id: names.union(),
            payload: t.flip(),
        },
    }
}

/// A function parameter's annotation: an earlier file's record, when there is
/// one and the tape says so.
fn gen_param(t: &mut Tape, earlier: &[File]) -> Option<u32> {
    let records: Vec<u32> = earlier.iter().flat_map(File::record_ids).collect();
    (!records.is_empty() && t.flip()).then(|| records[t.choice(records.len())])
}

/// A body: a literal, a value of another type, a field read, or — when an
/// earlier file exports a value or declares a union — a cross-file reference
/// to one (so a later edit to that file's exports, or to the type it publishes
/// for one, changes what this file reads, the situation suffix-reuse must get
/// right).
fn gen_body(t: &mut Tape, earlier: &[File]) -> Body {
    let targets: Vec<(u32, String)> = earlier
        .iter()
        .flat_map(|f| f.value_names().into_iter().map(move |n| (f.uid, n)))
        .collect();
    let cases: Vec<(u32, u32)> = earlier
        .iter()
        .flat_map(|f| f.union_ids().into_iter().map(move |id| (f.uid, id)))
        .collect();
    match t.choice(6) {
        0 | 1 if !targets.is_empty() => {
            let (file, name) = targets[t.choice(targets.len())].clone();
            Body::Ref { file, name }
        }
        2 if !cases.is_empty() => {
            let (file, id) = cases[t.choice(cases.len())];
            Body::Case { file, id }
        }
        3 => Body::Other(t.choice(OTHER_SHAPES.len()) as u32),
        4 => Body::Field,
        _ => Body::Lit(t.choice(4) as u32),
    }
}

fn apply_edit(t: &mut Tape, names: &mut Names, files: &mut [File]) {
    let fi = t.choice(files.len());
    match t.choice(12) {
        // Replace a body: exports unchanged, resolutions change — the
        // resolver's suffix must stay reusable — and the type the binder
        // publishes may change, which a later file's inference reads.
        0 => {
            let (earlier, rest) = files.split_at_mut(fi);
            let f = &mut rest[0];
            if !f.decls.is_empty() {
                let di = t.choice(f.decls.len());
                match &mut f.decls[di] {
                    Decl::Val { body, .. } | Decl::Func { body, .. } => {
                        let n = t.choice(9) as u32;
                        *body = match t.choice(4) {
                            0 => Body::Lit(n),
                            1 => Body::LetIn(n),
                            2 => Body::Other(n),
                            _ => gen_body(t, earlier),
                        };
                    }
                    Decl::Union { .. } | Decl::Record { .. } => {}
                }
            }
        }
        // Append a new value export: changes the file's exports (count + names),
        // which must invalidate every later file's reuse.
        1 => {
            files[fi].decls.push(Decl::Val {
                name: names.value(),
                attr: false,
                body: Body::Lit(1),
            });
        }
        // Remove a declaration: may drop an export a later file references.
        2 => {
            let f = &mut files[fi];
            if !f.decls.is_empty() {
                let di = t.choice(f.decls.len());
                f.decls.remove(di);
            }
        }
        // Toggle an attribute on a value: flips the extension-source signal while
        // leaving exports byte-identical.
        3 => {
            let f = &mut files[fi];
            let val_positions: Vec<usize> = f
                .decls
                .iter()
                .enumerate()
                .filter(|(_, d)| matches!(d, Decl::Val { .. }))
                .map(|(i, _)| i)
                .collect();
            if !val_positions.is_empty() {
                let di = val_positions[t.choice(val_positions.len())];
                if let Decl::Val { attr, .. } = &mut f.decls[di] {
                    *attr = !*attr;
                }
            }
        }
        // Toggle the nested auto-open module: changes both the exportable
        // auto-open paths and the extension signal.
        4 => files[fi].auto_open = !files[fi].auto_open,
        // Add a record type: a new type later namespace files may name.
        6 => files[fi].decls.push(Decl::Record {
            id: names.union(),
            generic: false,
            private: false,
        }),
        // Annotate a function's parameter with an earlier file's record, or
        // drop the annotation.
        7 => {
            let (earlier, rest) = files.split_at_mut(fi);
            let f = &mut rest[0];
            let funcs: Vec<usize> = f
                .decls
                .iter()
                .enumerate()
                .filter(|(_, d)| matches!(d, Decl::Func { .. }))
                .map(|(i, _)| i)
                .collect();
            if !funcs.is_empty() {
                let di = funcs[t.choice(funcs.len())];
                let new_param = gen_param(t, earlier);
                if let Decl::Func { param, .. } = &mut f.decls[di] {
                    *param = new_param;
                }
            }
        }
        // Make a record generic or `private`, or undo it: its exported path is
        // unchanged, what a later use of it names is not.
        8 | 9 => {
            let toggle_generic = t.flip();
            let f = &mut files[fi];
            let records: Vec<usize> = f
                .decls
                .iter()
                .enumerate()
                .filter(|(_, d)| matches!(d, Decl::Record { .. }))
                .map(|(i, _)| i)
                .collect();
            if !records.is_empty() {
                let di = records[t.choice(records.len())];
                if let Decl::Record {
                    generic, private, ..
                } = &mut f.decls[di]
                {
                    if toggle_generic {
                        *generic = !*generic;
                    } else {
                        *private = !*private;
                    }
                }
            }
        }
        // Swap two adjacent declarations: the same declarations, in another
        // order.
        10 => {
            let f = &mut files[fi];
            if f.decls.len() >= 2 {
                let di = t.choice(f.decls.len() - 1);
                f.decls.swap(di, di + 1);
            }
        }
        // Give a union's first case a payload, or take it away: the same case
        // exports, a different type for a later use of the case.
        11 => {
            let f = &mut files[fi];
            let unions: Vec<usize> = f
                .decls
                .iter()
                .enumerate()
                .filter(|(_, d)| matches!(d, Decl::Union { .. }))
                .map(|(i, _)| i)
                .collect();
            if !unions.is_empty() {
                let di = unions[t.choice(unions.len())];
                if let Decl::Union { payload, .. } = &mut f.decls[di] {
                    *payload = !*payload;
                }
            }
        }
        // Add a union type: new type + case exports.
        _ => files[fi].decls.push(Decl::Union {
            id: names.union(),
            payload: false,
        }),
    }
}

/// A deterministic reader over the same generator, so a batch run can assert the
/// property is not vacuous: the generated edits must actually drive the reuse
/// machinery (unchanged-prefix *and* export-neutral-suffix reuse), else
/// `incremental ≡ batch` would hold trivially and prove nothing about reuse.
pub fn seeded_tape(seed: u64) -> Vec<u32> {
    // A small splitmix-style PRNG — deterministic (no `rand`), decent spread.
    let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    (0..64)
        .map(|_| {
            s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = s;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ((z ^ (z >> 31)) & 0xFFFF_FFFF) as u32
        })
        .collect()
}
