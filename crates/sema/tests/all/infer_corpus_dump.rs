//! Everything inference publishes about the whole corpus, pinned file by file:
//! the oracle a refactor of inference is held to.
//!
//! A change that means to leave inference's output as it was (moving state
//! between structs, unifying an equality where it is generated rather than at
//! the binding's end) cannot be checked against FCS alone: FCS grades what we
//! commit, so it misses a commit that moved from one sound answer to another,
//! or a decline that became a different decline. What it can be checked
//! against is the output itself, byte for byte, before and after.
//!
//! # What is dumped
//!
//! Every `.fs` file under `BORZOI_CORPUS` is inferred twice:
//!
//! - **alone**, as [`infer_corpus_diff`](super::infer_corpus_diff) infers it:
//!   [`resolve_file`] and [`infer_named_file`] with no preceding file, against
//!   the same env (FSharp.Core and the whole reference pack) and under FCS's
//!   script symbols;
//! - **in its directory**, every `.fsi` and `.fs` of the directory folded as
//!   one project ([`resolve_project_files`], [`infer_project`]), in path order
//!   with each signature before its implementation. That order is not a real
//!   Compile order, but it is a fixed one, under which a file reads what the
//!   files before it published, a signature screens its implementation, and a
//!   repeated module identity is rejected: the cross-file channels a
//!   single-file inference never exercises.
//!
//! Files our parser recovers from are dumped too: inference's declines on a
//! recovered declaration are output like any other.
//!
//! Each inference is spelled ([`dump_file`]) as its expression types, binder
//! types, member resolutions, `(expr).Member` receiver types, incompleteness
//! census, type-budget verdict and deepest expression walk, each type through
//! [`spelled_debug`] and each table sorted by range. [`InferredFile::type_walks`]
//! is left out: how large a type walk was is a measurement of how inference
//! computed, which a refactor may legitimately change. A file's dump in its
//! directory adds what the fold published for each value it exports
//! ([`dump_exports`]), the type a later file's use instantiates or why there
//! is none, which a later file's dump shows only where it uses the value.
//!
//! # The manifest
//!
//! The full dump is about 12 MB, too large to review as a manifest, so the
//! manifest pins its digest per file: one keyed line per corpus file,
//! `<path>\talone <sha1-prefix>/<n> dir <sha1-prefix>/<n>`, where `n` counts
//! the file's commits (types, binder types, member resolutions and receiver
//! types) so a moved line says which way it moved; a state stands in for a
//! digest where there is no dump (`unreadable`, or `none` for a slot the fold
//! left uninferred). Any byte of a file's dump that changes moves its line;
//! the line names the file, and the dump itself says what moved:
//!
//! ```text
//! BORZOI_INFER_DUMP_OUT=/tmp/base.txt nix develop -c cargo test -p borzoi-sema --test all infer_corpus_dump:: -- --ignored
//! # … again on the other commit, to /tmp/head.txt, then
//! diff /tmp/base.txt /tmp/head.txt
//! ```
//!
//! `BORZOI_INFER_DUMP_OUT` writes the whole dump, every file's text under a
//! `== <path> alone` or `== <path> dir` header, in key order.
//!
//! A movement that is intended is acknowledged by regenerating:
//!
//! ```text
//! BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-sema --test all infer_corpus_dump:: -- --ignored
//! ```
//!
//! `#[ignore]`d: it resolves and infers the whole corpus, twice.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use borzoi_cst::parser::parse_with_symbols;
use borzoi_cst::syntax::{AstNode, ImplFile, SigFile, SyntaxKind, SyntaxNode};
use borzoi_oracle_harness::manifest::Manifest;
use borzoi_oracle_harness::panic_silence::silence_panics_here;
use borzoi_oracle_harness::par_map::par_map;
use borzoi_oracle_harness::stack_probe::on_stack;
use borzoi_sema::test_support::spelled_debug;
use borzoi_sema::{
    AssemblyEnv, InferredFile, InferredProject, PrecedingTypes, ProjectFile, ProjectItems,
    Resolution, ResolvedFile, SourceFile, SyntaxRecovery, WALK_STACK_BYTES, infer_named_file,
    infer_project, qualified_names, resolve_file, resolve_project_files,
};
use sha1::{Digest, Sha1};

use crate::common::corpus_manifest::{check_manifest, corpus_relative, regenerate_ignored};

/// The conditional-compilation symbols every file is parsed under: FCS's
/// script check's, as `infer_corpus_diff` parses.
const SYMBOLS: [&str; 2] = ["INTERACTIVE", "EDITING"];

/// How many hex digits of each dump's SHA-1 a manifest line keeps: enough
/// that a change to a file's dump cannot plausibly leave its line in place.
const DIGEST_HEX: usize = 16;

/// `ty` through [`spelled_debug`].
fn spell(env: &AssemblyEnv, ty: borzoi_sema::Ty) -> String {
    spelled_debug(env, ty)
}

/// A member resolution, spelled by the member's declaring entity and index
/// rather than by its ids, which name the env's tables.
fn spell_resolution(env: &AssemblyEnv, res: Resolution) -> String {
    match res {
        Resolution::Member { parent, idx } => format!(
            "member {}/{}#{idx:?} {}",
            env.entity(parent).assembly.name,
            env.entity_full_name(parent),
            env.member_display_name(parent, idx)
        ),
        other => format!("{other:?}"),
    }
}

/// One inference, spelled: every table sorted by range, so the text depends
/// only on what was inferred.
///
/// `root` is the file's tree, which supplies the `(expr).Member` accesses
/// whose receiver types [`InferredFile::dot_get_receiver_type`] is asked for.
pub fn dump_file(
    env: &AssemblyEnv,
    root: &SyntaxNode,
    resolved: &ResolvedFile,
    inferred: &InferredFile,
) -> (String, usize) {
    use std::fmt::Write;
    let at = |r: borzoi_cst::syntax::TextRange| (u32::from(r.start()), u32::from(r.end()));
    let mut out = String::new();
    let mut commits = 0;
    writeln!(out, "budget {:?}", inferred.type_budget_exceeded()).unwrap();
    writeln!(out, "deepest {}", inferred.deepest_expression_walk()).unwrap();
    writeln!(out, "incompleteness").unwrap();
    for (i, reasons) in inferred.incompleteness().iter().enumerate() {
        writeln!(out, "  {i} {reasons:?}").unwrap();
    }
    let mut types: Vec<_> = inferred
        .types()
        .iter()
        .map(|(r, t)| (at(*r), spell(env, *t)))
        .collect();
    types.sort();
    writeln!(out, "types").unwrap();
    for ((s, e), t) in &types {
        writeln!(out, "  {s}-{e} {t}").unwrap();
    }
    let mut defs: Vec<_> = inferred
        .def_types()
        .iter()
        .map(|(d, t)| {
            let def = resolved.def(*d);
            (at(def.range), format!("{:?} {}", def.kind, spell(env, *t)))
        })
        .collect();
    defs.sort();
    writeln!(out, "defs").unwrap();
    for ((s, e), t) in &defs {
        writeln!(out, "  {s}-{e} {t}").unwrap();
    }
    let mut members: Vec<_> = inferred
        .member_resolutions()
        .iter()
        .map(|(r, res)| (at(*r), spell_resolution(env, *res)))
        .collect();
    members.sort();
    writeln!(out, "members").unwrap();
    for ((s, e), m) in &members {
        writeln!(out, "  {s}-{e} {m}").unwrap();
    }
    let mut receivers: Vec<_> = root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::DOT_GET_EXPR)
        .filter_map(|n| {
            let range = n.text_range();
            inferred
                .dot_get_receiver_type(range)
                .map(|t| (at(range), spell(env, *t)))
        })
        .collect();
    receivers.sort();
    receivers.dedup();
    writeln!(out, "receivers").unwrap();
    for ((s, e), t) in &receivers {
        writeln!(out, "  {s}-{e} {t}").unwrap();
    }
    commits += types.len() + defs.len() + members.len() + receivers.len();
    (out, commits)
}

/// What the fold published for each value `resolved` exports, in source order:
/// the type a later file's use instantiates, or why there is none. Hover and
/// completion read it across files, and a change to it shows in a later
/// file's dump only where some later file happens to use the value.
fn dump_exports(env: &AssemblyEnv, resolved: &ResolvedFile, inferred: &InferredProject) -> String {
    use std::fmt::Write;
    let mut out = "exports\n".to_string();
    // Named by path, not by binder: a screened implementation's export may
    // name its signature's binder, in another file's arena.
    for item in resolved.exports().iter() {
        let published = match inferred.item_type(item.id()) {
            Ok(ty) => spell(env, *ty),
            Err(gap) => format!("declined {gap:?}"),
        };
        writeln!(
            out,
            "  {:?} {}",
            item.qualified_path(env)
                .unwrap_or_else(|| vec![item.name(env).to_string()]),
            published
        )
        .unwrap();
    }
    out
}

/// A file's dump in one of its two inferences, or why it has none.
#[derive(Debug, Clone)]
enum Dumped {
    Text {
        text: String,
        commits: usize,
    },
    /// Resolution or inference panicked.
    Panicked,
    /// The file is not UTF-8 (the corpus holds codepage and UTF-16 tests),
    /// so nothing reads it; a directory folds without it.
    Unreadable,
    /// The fold left no inference at this file's slot: its identity repeats
    /// an earlier file's.
    NoInference,
}

impl Dumped {
    fn line(&self) -> String {
        match self {
            Dumped::Text { text, commits } => {
                let digest = Sha1::digest(text.as_bytes());
                let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
                format!("{}/{commits}", &hex[..DIGEST_HEX])
            }
            Dumped::Panicked => "panicked".to_string(),
            Dumped::Unreadable => "unreadable".to_string(),
            Dumped::NoInference => "none".to_string(),
        }
    }

    fn text(&self) -> &str {
        match self {
            Dumped::Text { text, .. } => text,
            Dumped::Panicked => "panicked\n",
            Dumped::Unreadable => "unreadable\n",
            Dumped::NoInference => "none\n",
        }
    }
}

/// `path`'s text without a leading byte-order mark, which FCS strips, or
/// `None` when it is not UTF-8.
fn read_source(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let raw = String::from_utf8(bytes).ok()?;
    Some(raw.strip_prefix('\u{feff}').unwrap_or(&raw).to_string())
}

fn symbols() -> std::collections::HashSet<String> {
    SYMBOLS.iter().map(|s| s.to_string()).collect()
}

/// Parse `src` as the file at `path` says: a signature for `.fsi`.
fn parse_source(path: &Path, src: &str) -> (SourceFile, SyntaxRecovery) {
    let parsed = if path.extension().is_some_and(|e| e == "fsi") {
        borzoi_cst::parser::parse_sig_with_symbols(src, &symbols())
    } else {
        parse_with_symbols(src, &symbols())
    };
    let recovery = SyntaxRecovery::of(&parsed);
    let file = if path.extension().is_some_and(|e| e == "fsi") {
        SourceFile::Sig(SigFile::cast(parsed.root).expect("sig root"))
    } else {
        SourceFile::Impl(ImplFile::cast(parsed.root).expect("impl root"))
    };
    (file, recovery)
}

/// Infer `path` alone and dump it.
fn dump_alone(env: &'static AssemblyEnv, path: PathBuf) -> Dumped {
    on_stack(WALK_STACK_BYTES, move || {
        let _silence = silence_panics_here();
        let Some(src) = read_source(&path) else {
            return Dumped::Unreadable;
        };
        catch_unwind(AssertUnwindSafe(|| {
            let (file, recovery) = parse_source(&path, &src);
            let SourceFile::Impl(file) = file else {
                unreachable!("an implementation file")
            };
            let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
            let name = qualified_names(
                &[SourceFile::Impl(file.clone())],
                std::slice::from_ref(&path),
            )
            .pop()
            .expect("one name per file");
            let inferred =
                infer_named_file(&file, &resolved, env, &PrecedingTypes::default(), &name);
            let (text, commits) = dump_file(env, file.syntax(), &resolved, &inferred);
            Dumped::Text { text, commits }
        }))
        .unwrap_or(Dumped::Panicked)
    })
}

/// Fold `paths` (one directory, in [`directory_order`]) as one project and
/// dump each implementation file.
fn dump_directory(env: &'static AssemblyEnv, paths: Vec<PathBuf>) -> Vec<(PathBuf, Dumped)> {
    on_stack(WALK_STACK_BYTES, move || {
        let _silence = silence_panics_here();
        let (mut sources, mut unreadable) = (Vec::new(), Vec::new());
        for p in paths {
            match read_source(&p) {
                Some(src) => sources.push((p, src)),
                None => unreadable.push(p),
            }
        }
        let paths: Vec<PathBuf> = sources.iter().map(|(p, _)| p.clone()).collect();
        let impls: Vec<PathBuf> = paths
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e == "fs"))
            .cloned()
            .collect();
        let folded = catch_unwind(AssertUnwindSafe(|| {
            let parsed: Vec<(SourceFile, SyntaxRecovery)> = sources
                .iter()
                .map(|(p, src)| parse_source(p, src))
                .collect();
            let trees: Vec<SourceFile> = parsed.iter().map(|(f, _)| f.clone()).collect();
            let files: Vec<ProjectFile> = parsed
                .into_iter()
                .zip(qualified_names(&trees, &paths))
                .map(|((file, recovery), qnof)| ProjectFile::new(file, qnof, recovery))
                .collect();
            let resolved = resolve_project_files(&files, env);
            let inferred = infer_project(&files, &resolved, env);
            paths
                .iter()
                .enumerate()
                .filter(|(_, p)| p.extension().is_some_and(|e| e == "fs"))
                .map(|(i, p)| {
                    let dumped = match (&trees[i], inferred.file(i)) {
                        (SourceFile::Impl(file), Some(inf)) => {
                            let (mut text, commits) =
                                dump_file(env, file.syntax(), resolved.file(i), inf);
                            text.push_str(&dump_exports(env, resolved.file(i), &inferred));
                            Dumped::Text { text, commits }
                        }
                        _ => Dumped::NoInference,
                    };
                    (p.clone(), dumped)
                })
                .collect::<Vec<_>>()
        }));
        let mut dumped =
            folded.unwrap_or_else(|_| impls.into_iter().map(|p| (p, Dumped::Panicked)).collect());
        dumped.extend(
            unreadable
                .into_iter()
                .filter(|p| p.extension().is_some_and(|e| e == "fs"))
                .map(|p| (p, Dumped::Unreadable)),
        );
        dumped
    })
}

/// A directory's sources in the order the dump folds them: by path, a
/// signature before the implementation of the same name.
fn directory_order(paths: &mut [PathBuf]) {
    paths.sort_by(|a, b| {
        let key = |p: &PathBuf| {
            (
                p.with_extension(""),
                p.extension().is_none_or(|e| e != "fsi"),
            )
        };
        key(a).cmp(&key(b))
    });
}

/// Every `.fs` and `.fsi` under `dir`, by directory, skipping build and VCS
/// output and symlinks, as the corpus sweeps do.
fn collect_sources(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<PathBuf>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            if !matches!(
                path.file_name().and_then(|s| s.to_str()),
                Some(".git" | "target" | "artifacts" | "bin" | "obj")
            ) {
                collect_sources(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "fs" || e == "fsi") {
            out.entry(dir.to_path_buf()).or_default().push(path);
        }
    }
}

#[test]
#[ignore = "resolves and infers the whole corpus twice; run with --ignored under nix develop"]
fn inference_over_the_corpus_is_pinned() {
    let Some(root) = std::env::var_os("BORZOI_CORPUS") else {
        eprintln!("BORZOI_CORPUS unset; skipping the inference dump.");
        return;
    };
    let root = PathBuf::from(root);
    let env = super::infer_corpus_diff::ref_pack_env();
    let mut dirs = BTreeMap::new();
    collect_sources(&root, &mut dirs);
    let dirs: Vec<Vec<PathBuf>> = dirs
        .into_values()
        .map(|mut paths| {
            directory_order(&mut paths);
            paths
        })
        .collect();
    let impls: Vec<PathBuf> = dirs
        .iter()
        .flatten()
        .filter(|p| p.extension().is_some_and(|e| e == "fs"))
        .cloned()
        .collect();
    assert!(!impls.is_empty(), "no .fs files under {}", root.display());
    eprintln!(
        "infer-dump: {} .fs files in {} directories",
        impls.len(),
        dirs.len()
    );

    let alone = par_map(impls.len(), |i| dump_alone(env, impls[i].clone()));
    let in_dir: BTreeMap<PathBuf, Dumped> =
        par_map(dirs.len(), |i| dump_directory(env, dirs[i].clone()))
            .into_iter()
            .flatten()
            .collect();

    let mut by_key: BTreeMap<String, (&Dumped, &Dumped)> = BTreeMap::new();
    for (path, alone) in impls.iter().zip(&alone) {
        let dir = in_dir
            .get(path)
            .expect("every file is folded with its directory");
        let previous = by_key.insert(corpus_relative(&root, path), (alone, dir));
        assert!(
            previous.is_none(),
            "two files share the key of {}",
            path.display()
        );
    }
    let (mut panicked, mut commits) = (Vec::new(), (0usize, 0usize));
    for (key, (alone, dir)) in &by_key {
        for (pass, d) in [("alone", alone), ("dir", dir)] {
            match d {
                Dumped::Panicked => panicked.push(format!("{key} ({pass})")),
                Dumped::Text { commits: n, .. } if pass == "alone" => commits.0 += n,
                Dumped::Text { commits: n, .. } => commits.1 += n,
                Dumped::NoInference | Dumped::Unreadable => {}
            }
        }
    }
    eprintln!(
        "infer-dump: {} commits alone, {} in directories; {} panicked",
        commits.0,
        commits.1,
        panicked.len()
    );
    if let Some(out) = std::env::var_os("BORZOI_INFER_DUMP_OUT") {
        use std::io::Write;
        let mut w = std::io::BufWriter::new(
            std::fs::File::create(&out).unwrap_or_else(|e| panic!("create {out:?}: {e}")),
        );
        for (key, (alone, dir)) in &by_key {
            write!(
                w,
                "== {key} alone\n{}== {key} dir\n{}",
                alone.text(),
                dir.text()
            )
            .expect("write the dump");
        }
        w.flush().expect("write the dump");
        eprintln!("infer-dump: wrote {out:?}");
    }
    // Resolution and inference must not panic on any file, recovered or not.
    // Regenerating the manifest cannot bless one.
    assert!(
        panicked.is_empty(),
        "resolve/infer panicked on corpus files:\n{}",
        panicked.join("\n")
    );
    let manifest = Manifest::from_keyed(by_key.iter().map(|(key, (alone, dir))| {
        (
            key.clone(),
            format!("alone {} dir {}", alone.line(), dir.line()),
        )
    }))
    .unwrap_or_else(|e| panic!("manifest entry: {e}"));
    check_manifest(
        "infer_corpus_dump",
        &manifest,
        &regenerate_ignored("infer_corpus_dump"),
    );
}

/// The dump spells every table it pins: a fixture that commits into each of
/// them shows up in each, so no section of the manifest's digest is silently
/// empty.
#[test]
fn every_section_of_the_dump_is_live() {
    let src = "module M\n\
               let f (s: string) = (s).Length\n\
               let g (s: string) = s.ToUpperInvariant()\n\
               let h x = match x with Some y -> y | None -> 0\n";
    let env = crate::common::full_bcl_env();
    let parsed = parse_with_symbols(src, &symbols());
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl root");
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let inferred = borzoi_sema::infer_file(&file, &resolved, env, &PrecedingTypes::default());
    let (text, commits) = dump_file(env, file.syntax(), &resolved, &inferred);
    let section = |name: &str| -> Vec<&str> {
        text.lines()
            .skip_while(|l| *l != name)
            .skip(1)
            .take_while(|l| l.starts_with("  "))
            .collect()
    };
    for name in ["incompleteness", "types", "defs", "members", "receivers"] {
        assert!(
            !section(name).is_empty(),
            "the fixture puts nothing in the dump's `{name}` section:\n{text}"
        );
    }
    assert!(commits > 0, "{text}");
    assert!(
        section("receivers")
            .iter()
            .any(|l| l.contains("\"System\", \"String\"")),
        "the receiver of `(s).Length` is a string:\n{text}"
    );
}

/// What a fold publishes for a file's exports is in its dump, a decline with
/// its reason: making `f` not `inline` changes nothing else the file dumps.
#[test]
fn exports_are_dumped_with_their_declines() {
    let dump = |src: &str| {
        let env = crate::common::full_bcl_env();
        let path = PathBuf::from("/p/M.fs");
        let (file, recovery) = parse_source(&path, src);
        let files = vec![ProjectFile::new(
            file.clone(),
            qualified_names(std::slice::from_ref(&file), std::slice::from_ref(&path))
                .pop()
                .expect("one name"),
            recovery,
        )];
        let resolved = resolve_project_files(&files, env);
        let inferred = infer_project(&files, &resolved, env);
        dump_exports(env, resolved.file(0), &inferred)
    };
    let inline = dump("module M\nlet a = 1\nlet inline f x = x\n");
    let plain = dump("module M\nlet a = 1\nlet        f x = x\n");
    assert!(
        inline.contains("[\"M\", \"a\"] Named { path: [\"System\", \"Int32\"]"),
        "{inline}"
    );
    assert!(
        inline.contains("[\"M\", \"f\"] declined Inline"),
        "{inline}"
    );
    assert!(plain.contains("[\"M\", \"f\"] Fun"), "{plain}");
}
