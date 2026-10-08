//! Does the parser differential compare everything the consumers read?
//!
//! The normalised AST (`normalised_ast/`) is the currency the FCS differential
//! compares in, and it deliberately elides detail. That is only sound for
//! detail no consumer reads: a typed-AST accessor that `borzoi-sema` or the LSP
//! calls, and that the projection never reads, is a parser output that nothing
//! grades against FCS. This module computes both sides of that question.
//!
//! * **The accessors.** Every public `&self` method of an inherent `impl` in
//!   `crates/cst/src/syntax/mod.rs`, read with `syn`. Each must open with
//!   `accessor!("Type::method")` (checked by [`accessor_universe`]), which under
//!   the crate's `accessor-trace` feature logs the call.
//! * **What the consumers read.** Every method call, path expression and
//!   macro-argument call in `crates/sema/src` and `crates/lsp/src`, also read
//!   with `syn` ([`consumer_reads`]). `syn` has no types, so a call is matched
//!   to accessors by method name, narrowed only by which AST types the
//!   consumers can hold a value of: a type is *reachable* when a consumer
//!   names it, when a consumed accessor's return type mentions it, or when a
//!   consumer matches the dispatch-enum variant that carries it
//!   (`Expr::App(app)` reaches `AppExpr`). `T::m` is consumed when `T` is
//!   reachable and a consumer calls a method named `m` (or uses the path
//!   `T::m`). That over-approximates the consumers, which is the safe
//!   direction: it can demand more of the projection than is needed, never
//!   less, short of a value obtained without any of those (a generic helper
//!   that infers `T` from a non-accessor signature).
//! * **What the projection reads.** `parser_corpus_diff` runs
//!   `normalise_parse` and the range audit under
//!   `borzoi_cst::syntax::accessor_trace::record` and keeps the reads of the
//!   files whose trees and audited ranges both match FCS (and of the exact
//!   recovered trees, and of [`FIXTURES`]). An accessor counts as compared only
//!   if the projection read it, directly, on a file where everything it read
//!   agreed with FCS. The set is pinned exactly in
//!   `tests/manifests/accessor_coverage.txt`, so the check below needs neither
//!   the corpus nor FCS.
//!
//! [`assert_projection_covers_consumers`] then requires every consumed accessor
//! to be read by the projection, or to be in [`NOT_PROJECTED`] with the reason
//! it cannot be. Both directions of that list are checked, so it cannot go
//! stale.
//!
//! The limits, stated so nobody mistakes this for more than it is:
//!
//! * It sees the typed facade only. A consumer that walks raw `SyntaxNode`s
//!   (`.syntax().children()`, a `SyntaxKind` match) bypasses every accessor and
//!   is invisible here.
//! * "Read" is "called". An accessor whose result the projection reads only in
//!   part (the text of a returned token but not its range, say) counts as
//!   read. Ranges get their own relation in `range_audit`.
//! * Coverage is by the pinned corpus: an accessor that only per-construct tests
//!   reach, and no matching corpus file does, is reported as unread.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use syn::visit::Visit;

/// Accessors the consumers read that the projection does not, with the reason
/// FCS's tree has nothing to compare them with.
pub const NOT_PROJECTED: &[(&str, &str)] = &[
    (
        "InlineIlExpr::args",
        "FSharp.Core's inline IL `(# … #)`: the normaliser does not model \
         SynExpr.LibraryOnlyILAssembly, whose IL FCS boxes as an opaque object, so \
         no file holding one reaches the comparison",
    ),
    (
        "InlineIlExpr::types",
        "FSharp.Core's inline IL `(# … #)`; see InlineIlExpr::args",
    ),
];

/// Constructs the consumers read that no matching corpus file holds: each is
/// graded against FCS like a corpus file ([`fixture_reads`]), and must read
/// the accessor it names.
pub const FIXTURES: &[(&str, &str)] = &[
    ("DotMissingExpr::receiver", "let x = (f x).\nlet y = 2\n"),
    (
        "LibraryOnlyFieldGetExpr::object",
        "let f cons = cons.( :: ).0\n",
    ),
];

/// Grade each of [`FIXTURES`] against FCS: the parse verdicts agree and the
/// normalised trees are equal (and, when both are clean, the audited ranges
/// too). Returns everything the projection read on them.
pub fn fixture_reads() -> BTreeSet<&'static str> {
    use std::io::Write as _;

    use borzoi_cst::parser::parse;
    use borzoi_cst::syntax::accessor_trace::record;

    use super::normalised_ast::{normalise_fcs_dump, normalise_parse};
    use super::{ast_ranges_match, fcs_ast_batch, fcs_parse_had_errors};

    let mut all = BTreeSet::new();
    for (accessor, source) in FIXTURES {
        let mut tmp = tempfile::NamedTempFile::with_suffix(".fs").expect("create tempfile");
        tmp.write_all(source.as_bytes()).expect("write fixture");
        let json = fcs_ast_batch(tmp.path());
        let ours = parse(source);
        assert_eq!(
            ours.errors.is_empty(),
            !fcs_parse_had_errors(&json),
            "accessor fixture {source:?}: the parse verdicts differ"
        );
        let (rust, mut reads) = record(|| normalise_parse(&ours));
        assert_eq!(
            rust,
            normalise_fcs_dump(&json),
            "accessor fixture {source:?}: the trees differ"
        );
        if ours.errors.is_empty() {
            let (ranges, range_reads) = record(|| ast_ranges_match(&ours, &json, source));
            ranges.unwrap_or_else(|e| panic!("accessor fixture {source:?}: {e}"));
            reads.extend(range_reads);
        }
        assert!(
            reads.contains(accessor),
            "accessor fixture {source:?} does not read {accessor}"
        );
        all.extend(reads);
    }
    all
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root exists")
}

/// The typed AST's public surface, as the coverage check needs it.
pub struct Facade {
    /// Every public accessor, `"Type::method"`, with the AST types its return
    /// type mentions.
    pub accessors: BTreeMap<String, BTreeSet<String>>,
    /// Every AST type name: those with accessors, and the dispatch enums.
    type_names: BTreeSet<String>,
    /// `(enum, variant)` to the AST types its payload mentions.
    variants: BTreeMap<(String, String), BTreeSet<String>>,
}

fn syntax_sources() -> Vec<PathBuf> {
    let dir = repo_root().join("crates/cst/src/syntax");
    let mut files = Vec::new();
    rust_files(&dir, &mut files);
    files
}

fn idents_in(tokens: TokenStream, out: &mut BTreeSet<String>) {
    for tt in tokens {
        match tt {
            TokenTree::Ident(id) => {
                out.insert(id.to_string());
            }
            TokenTree::Group(g) => idents_in(g.stream(), out),
            _ => {}
        }
    }
}

/// Read the typed AST's surface. Panics, naming them, if any public accessor
/// does not open with its own `accessor!("Type::method")`.
pub fn facade() -> Facade {
    let mut accessors = BTreeMap::new();
    let mut type_names = BTreeSet::new();
    let mut variant_idents: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    let mut untraced = Vec::new();
    for path in syntax_sources() {
        let text = std::fs::read_to_string(&path).expect("read a syntax source");
        let file = syn::parse_file(&text)
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
        let accessor_file = path.ends_with("syntax/mod.rs");
        for item in &file.items {
            match item {
                syn::Item::Enum(e) => {
                    type_names.insert(e.ident.to_string());
                    for v in &e.variants {
                        let mut mentioned = BTreeSet::new();
                        idents_in(v.fields.to_token_stream(), &mut mentioned);
                        variant_idents
                            .entry((e.ident.to_string(), v.ident.to_string()))
                            .or_default()
                            .extend(mentioned);
                    }
                }
                syn::Item::Struct(st) => {
                    type_names.insert(st.ident.to_string());
                }
                syn::Item::Impl(imp) if accessor_file && imp.trait_.is_none() => {
                    let syn::Type::Path(ty) = &*imp.self_ty else {
                        continue;
                    };
                    let ty = ty
                        .path
                        .segments
                        .last()
                        .expect("an impl names its type")
                        .ident
                        .to_string();
                    type_names.insert(ty.clone());
                    for impl_item in &imp.items {
                        let syn::ImplItem::Fn(f) = impl_item else {
                            continue;
                        };
                        if !matches!(f.vis, syn::Visibility::Public(_))
                            || f.sig.receiver().is_none()
                        {
                            continue;
                        }
                        let name = format!("{ty}::{}", f.sig.ident);
                        if !first_stmt_traces(&f.block, &name) {
                            untraced.push(name);
                            continue;
                        }
                        let mut mentioned = BTreeSet::new();
                        idents_in(f.sig.output.to_token_stream(), &mut mentioned);
                        accessors.insert(name, mentioned);
                    }
                }
                _ => {}
            }
        }
    }
    // `ast_node!(Name, KIND)` declares the hand-written node types.
    for path in syntax_sources() {
        let text = std::fs::read_to_string(&path).expect("read a syntax source");
        let file = syn::parse_file(&text).expect("already parsed");
        for item in &file.items {
            if let syn::Item::Macro(m) = item
                && m.mac.path.is_ident("ast_node")
                && let Some(TokenTree::Ident(name)) = m.mac.tokens.clone().into_iter().next()
            {
                type_names.insert(name.to_string());
            }
        }
    }
    assert!(
        untraced.is_empty(),
        "these public accessors do not open with their own `accessor!(\"Type::method\")`, \
         so the accessor-coverage check cannot see whether anything compares them: \
         {untraced:#?}",
    );
    let only_types = |idents: BTreeSet<String>| -> BTreeSet<String> {
        idents
            .into_iter()
            .filter(|i| type_names.contains(i))
            .collect()
    };
    let accessors = accessors
        .into_iter()
        .map(|(a, mentioned)| (a, only_types(mentioned)))
        .collect();
    let variants = variant_idents
        .into_iter()
        .map(|(k, mentioned)| (k, only_types(mentioned)))
        .collect();
    Facade {
        accessors,
        type_names,
        variants,
    }
}

/// Every public accessor of the typed AST, as `"Type::method"`.
pub fn accessor_universe() -> BTreeSet<String> {
    facade().accessors.into_keys().collect()
}

fn first_stmt_traces(block: &syn::Block, name: &str) -> bool {
    let Some(syn::Stmt::Macro(m)) = block.stmts.first() else {
        return false;
    };
    if !m.mac.path.is_ident("accessor") {
        return false;
    }
    let Ok(lit) = m.mac.parse_body::<syn::LitStr>() else {
        return false;
    };
    lit.value() == name
}

/// Method names called, `Type::method` paths used, and identifiers named,
/// across some sources.
#[derive(Default)]
struct Calls {
    methods: BTreeSet<String>,
    paths: BTreeSet<(String, String)>,
    idents: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for Calls {
    fn visit_ident(&mut self, id: &'ast proc_macro2::Ident) {
        self.idents.insert(id.to_string());
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        let segs: Vec<String> = p.segments.iter().map(|s| s.ident.to_string()).collect();
        if let [.., ty, method] = segs.as_slice() {
            self.paths.insert((ty.clone(), method.clone()));
        }
        syn::visit::visit_path(self, p);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.methods.insert(call.method.to_string());
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.scan_tokens(mac.tokens.clone());
        syn::visit::visit_macro(self, mac);
    }
}

impl Calls {
    /// A macro's arguments are tokens to `syn`, not expressions, so a call in a
    /// `format!` or `matches!` would be missed by the visitor: read `. ident (`
    /// and `ident :: ident` off the token stream instead.
    fn scan_tokens(&mut self, tokens: TokenStream) {
        let tts: Vec<TokenTree> = tokens.into_iter().collect();
        for (i, tt) in tts.iter().enumerate() {
            match tt {
                TokenTree::Group(g) => self.scan_tokens(g.stream()),
                TokenTree::Punct(p) if p.as_char() == '.' => {
                    if let (Some(TokenTree::Ident(id)), Some(TokenTree::Group(g))) =
                        (tts.get(i + 1), tts.get(i + 2))
                        && g.delimiter() == proc_macro2::Delimiter::Parenthesis
                    {
                        self.methods.insert(id.to_string());
                    }
                }
                TokenTree::Ident(ty) => {
                    self.idents.insert(ty.to_string());
                    if let (
                        Some(TokenTree::Punct(a)),
                        Some(TokenTree::Punct(b)),
                        Some(TokenTree::Ident(method)),
                    ) = (tts.get(i + 1), tts.get(i + 2), tts.get(i + 3))
                        && a.as_char() == ':'
                        && b.as_char() == ':'
                    {
                        self.paths.insert((ty.to_string(), method.to_string()));
                    }
                }
                _ => {}
            }
        }
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.expect("directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The accessors of `facade` that the consumers' sources read, each with one
/// consumer file that reads it, for the failure message.
pub fn consumer_reads(facade: &Facade) -> BTreeMap<String, String> {
    let root = repo_root();
    let mut per_file: Vec<(String, Calls)> = Vec::new();
    for dir in ["crates/sema/src", "crates/lsp/src"] {
        let mut files = Vec::new();
        rust_files(&root.join(dir), &mut files);
        assert!(!files.is_empty(), "no Rust sources under {dir}");
        for path in files {
            let text = std::fs::read_to_string(&path).expect("read a consumer source");
            let file = syn::parse_file(&text)
                .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
            let mut calls = Calls::default();
            calls.visit_file(&file);
            let shown = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .display()
                .to_string();
            per_file.push((shown, calls));
        }
    }
    // The AST types a consumer can hold, to a fixpoint: named ones, those a
    // matched dispatch-enum variant carries, and those a consumed accessor
    // returns.
    let mut reachable: BTreeSet<String> = per_file
        .iter()
        .flat_map(|(_, c)| c.idents.iter())
        .filter(|i| facade.type_names.contains(*i))
        .cloned()
        .collect();
    let mut reads: BTreeMap<String, String> = BTreeMap::new();
    loop {
        let before = (reachable.len(), reads.len());
        for ((e, v), payload) in &facade.variants {
            if reachable.contains(e)
                && per_file
                    .iter()
                    .any(|(_, c)| c.paths.contains(&(e.clone(), v.clone())))
            {
                reachable.extend(payload.iter().cloned());
            }
        }
        for (accessor, returns) in &facade.accessors {
            let (ty, method) = accessor.split_once("::").expect("Type::method");
            if !reachable.contains(ty) {
                continue;
            }
            let reader = per_file.iter().find(|(_, c)| {
                c.methods.contains(method)
                    || c.paths.contains(&(ty.to_string(), method.to_string()))
            });
            if let Some((file, _)) = reader {
                reads
                    .entry(accessor.clone())
                    .or_insert_with(|| file.clone());
                reachable.extend(returns.iter().cloned());
            }
        }
        if (reachable.len(), reads.len()) == before {
            break;
        }
    }
    reads
}

/// The accessors the projection compares with FCS, as `parser_corpus_diff`
/// last pinned them.
pub fn pinned_projection() -> BTreeSet<String> {
    let path = super::corpus_manifest::manifest_path("accessor_coverage");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    borzoi_oracle_harness::manifest::Manifest::parse(&text)
        .unwrap_or_else(|(line, e)| panic!("{} line {line}: {e}", path.display()))
        .entries()
        .iter()
        .cloned()
        .collect()
}

/// Require every accessor the consumers read to be in `projected` (what the
/// projection read on FCS-matching corpus files), or in [`NOT_PROJECTED`].
pub fn assert_projection_covers_consumers(projected: &BTreeSet<String>) {
    let facade = facade();
    let consumed = consumer_reads(&facade);
    let universe = &facade.accessors;
    let exempt: BTreeMap<&str, &str> = NOT_PROJECTED.iter().copied().collect();
    assert_eq!(
        exempt.len(),
        NOT_PROJECTED.len(),
        "NOT_PROJECTED lists an accessor twice"
    );
    let unread: Vec<String> = consumed
        .iter()
        .filter(|(a, _)| !projected.contains(a.as_str()) && !exempt.contains_key(a.as_str()))
        .map(|(a, file)| format!("{a} (read in {file})"))
        .collect();
    let stale: Vec<&str> = exempt
        .keys()
        .copied()
        .filter(|a| {
            projected.contains(*a) || !consumed.contains_key(*a) || !universe.contains_key(*a)
        })
        .collect();
    eprintln!(
        "accessor coverage: {} accessors, {} read by consumers, {} read by the projection on \
         matching files, {} exempt",
        universe.len(),
        consumed.len(),
        projected.len(),
        exempt.len(),
    );
    assert!(
        unread.is_empty(),
        "{} accessors that sema or the LSP read are never compared with FCS: the projection \
         does not read them on any corpus file that matches. Project them \
         (`normalised_ast/`), or list them in NOT_PROJECTED with the reason FCS has \
         nothing to compare them with:\n  {}",
        unread.len(),
        unread.join("\n  "),
    );
    assert!(
        stale.is_empty(),
        "NOT_PROJECTED entries that are now projected, no longer read by a consumer, or no \
         longer accessors: {stale:?}",
    );
}
