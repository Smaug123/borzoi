//! Guarding against a case group that is present but never run — and against
//! the single test binary quietly becoming several again.
//!
//! Each crate's cases live in one test binary (`tests/all/main.rs`) with a
//! `mod <group>;` per group. That has two sharp edges, and Cargo warns about
//! neither:
//!
//! * A `tests/all/<group>.rs` with *no* `mod` line is not a compile error — it
//!   is simply not part of the crate. The file is never compiled, its tests
//!   never run, and the suite stays green. A whole group can be added, or lost
//!   in a merge, and nothing tells you. The same holds one level down: a case
//!   file inside `lexfilter_diff/` that no `mod` line in that group's `mod.rs`
//!   mentions is equally invisible, so the check must recurse.
//! * A case group left *outside* `tests/all/` is the mirror image. Cargo
//!   auto-discovers both `tests/<group>.rs` and `tests/<group>/main.rs` as
//!   integration-test *targets*, so it compiles and runs — as its own binary,
//!   relinked on every `src/` edit, spawning its own oracle child. Nothing
//!   fails; the fold is just undone.
//!
//! Under the old one-binary-per-file layout Cargo picked up `tests/*.rs`
//! automatically, so neither failure mode existed. They are the cost of the
//! fold, so the fold should pay for it: [`assert_all_case_groups_declared`] is
//! the check, and each folded crate calls it from its own test binary.
//!
//! This is the "have the machine enforce it" answer to a hazard the alternative
//! would leave as a comment asking people to remember.
//!
//! The guard's own failure mode is a *false* declaration: anything it counts as
//! `mod`-ed but the compiler does not reopens the very hole it exists to close,
//! silently. So the scanner (`declared_mods_in`) reads declarations with rustc's
//! own lexer (`proc_macro2`) rather than by scanning text — a `mod` inside a
//! comment, any string (ordinary, byte, C, raw, with any prefix), a char literal,
//! or a `macro_rules!` body is simply not a `mod` token — and is then deliberately
//! conservative about what the tokens mean:
//!
//! * only a top-level `mod NAME;` counts — one nested inside any `()`/`[]`/`{}`
//!   group (a `macro_rules!` body, an expression, an inline `mod x { … }`) is not
//!   a file-level declaration and rustc compiles no file for it;
//! * a `mod` carrying an *outer* `#[cfg]`/`#[cfg_attr]`/`#[path]` — the attributes
//!   that gate *whether*, or *from which file*, it compiles — is recorded as
//!   *conditional* and rejected rather than trusted; benign attributes (`#[allow]`,
//!   doc comments) leave it plain;
//! * a module file is exempted from its own listing by path, not by name, since
//!   `group.rs` owning `group/` must still declare `group/group.rs`.
//!
//! What the scanner deliberately does **not** do is evaluate `cfg`. That is
//! target-specific, and a single-target text scan cannot decide it: on the host
//! it runs on, a `cfg`-enabled file *is* compiled, and a `cfg`-disabled one it
//! could only flag by re-implementing rustc's cfg logic. In particular an *inner*
//! `#![cfg(unix)]` on a case file (as `assembly_cache_*` carry, deliberately and
//! documentedly) gates the whole file off-Unix by design — that is intended, not
//! a silent omission, so the guard leaves it alone. The guard's remit is the
//! *accidental* omission — a file added without its `mod` line — plus refusing
//! the declarations it cannot vouch for; it is not a second Rust front-end.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Assert that every case group under `tests/all/` is reachable from `main.rs`,
/// and that no case group has been left outside `tests/all/` altogether.
///
/// `main_rs` is the binary's own source (pass `file!()`), resolved against
/// `manifest_dir` (pass `env!("CARGO_MANIFEST_DIR")`) since `file!()` is
/// workspace-relative.
///
/// Every `*.rs` and every subdirectory holding Rust code must have a matching
/// `mod` declaration in the module file that owns it — `main.rs` at the top,
/// `<group>/mod.rs` within a group, all the way down. And `tests/` itself must
/// hold no test target but `all/`.
///
/// # Panics
///
/// If a case group exists on disk but is not declared, or sits outside
/// `tests/all/` — naming the files, since the whole point is that nothing else
/// will tell you.
pub fn assert_all_case_groups_declared(manifest_dir: &str, main_rs: &str) {
    // `file!()` is relative to the workspace root, and the manifest dir is
    // <workspace>/crates/<crate>, so climb out to resolve it.
    let workspace = Path::new(manifest_dir)
        .parent()
        .and_then(Path::parent)
        .expect("manifest dir should sit at <workspace>/crates/<crate>");
    let main_path = workspace.join(main_rs);
    let all_dir = main_path
        .parent()
        .expect("main.rs should have a parent directory");
    let tests_dir = all_dir
        .parent()
        .expect("tests/all should sit inside tests/");

    let mut undeclared = Vec::new();
    let mut conditional = Vec::new();
    collect_undeclared(
        all_dir,
        &main_path,
        tests_dir,
        &mut undeclared,
        &mut conditional,
    );
    assert!(
        undeclared.is_empty(),
        "case group(s) under {} are not `mod`-declared in the module file that \
         owns them, so they are NEVER COMPILED OR RUN and the suite is green \
         regardless: {undeclared:?}\n\
         Add `mod <name>;` to the enclosing main.rs / mod.rs for each.",
        all_dir.display(),
    );
    assert!(
        conditional.is_empty(),
        "case group(s) under {} are declared behind an *outer attribute* the \
         guard cannot evaluate (a `#[cfg(…)]`, `#[path=…]`, …): {conditional:?}\n\
         Whether rustc compiles and runs them then depends on cfg/features, so the \
         guard cannot vouch that they run — the exact hole it exists to close. \
         Declare case groups unconditionally (`mod <name>;`); if a group genuinely \
         must be gated, teach this guard to evaluate the gate rather than trust it.",
        all_dir.display(),
    );

    let stray = stray_test_targets(tests_dir);
    assert!(
        stray.is_empty(),
        "case group(s) in {} sit outside `all/`, where Cargo auto-discovers each \
         as its own integration-test target — a second test binary, relinked on \
         every src/ change, with its own oracle child, which is exactly the cost \
         this crate's single binary exists to pay once: {stray:?}\n\
         Move each into tests/all/ and add `mod <name>;` to main.rs.",
        tests_dir.display(),
    );
}

/// A module file's `mod` declarations, split by whether the scanner can vouch
/// that rustc compiles them.
#[derive(Default, PartialEq, Eq, Debug)]
struct Decls {
    /// Unconditionally declared: `[pub[(…)]] mod NAME;` with no compilation-gating
    /// attribute.
    plain: BTreeSet<String>,
    /// Declared behind an attribute the scanner cannot evaluate — a `#[cfg(…)]`,
    /// `#[cfg_attr(…)]`, or `#[path=…]`. Whether rustc compiles these (and, for
    /// `path`, from which file) is not decidable here, so the guard refuses them
    /// rather than silently trusting the declaration; see
    /// [`assert_all_case_groups_declared`].
    conditional: BTreeSet<String>,
}

/// The `mod` declarations in one module file, however they are qualified
/// (`mod x;`, `pub mod x;`, `pub(crate) mod x;`).
fn declared_mods(module_file: &Path) -> Decls {
    let src = std::fs::read_to_string(module_file)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", module_file.display()));
    declared_mods_in(&src)
}

/// [`declared_mods`] on the source text itself.
///
/// Tokenised with rustc's own lexer (`proc_macro2`) rather than scanned as text,
/// because "is this a declaration the compiler sees" is a lexical question that a
/// text scan keeps getting subtly wrong: a `mod` inside a comment, an ordinary /
/// byte / C / raw string, a char literal, or a `macro_rules!` body is not a `mod`
/// token at all, and the lexer settles every one of those in one place. We then
/// look only at the *top-level* token stream — items nested inside any `()`/`[]`/
/// `{}` group are inside a macro/expr/inline-module body, not file-level — and:
///
/// * `[pub[(…)]] mod NAME;` with no gating attribute is a plain declaration;
/// * a `mod` carrying an outer `#[cfg]`/`#[cfg_attr]`/`#[path]` is *conditional*
///   (rustc may or may not compile it, or compiles a different file) — the guard
///   rejects those. Benign outer attributes (`#[allow]`, doc comments, which the
///   lexer turns into `#[doc=…]`) do not gate compilation, so they leave a plain
///   declaration plain;
/// * an *inner* attribute (`#![…]`) decorates the enclosing module and does not
///   attach to the item after it, so it never taints a following `mod`.
fn declared_mods_in(src: &str) -> Decls {
    let tokens: Vec<proc_macro2::TokenTree> = match src.parse::<proc_macro2::TokenStream>() {
        Ok(ts) => ts.into_iter().collect(),
        // A module file is valid Rust; if the lexer cannot tokenise it, something
        // is badly wrong and the caller wants to hear about it, not get an empty
        // (silently green) declaration set.
        Err(e) => panic!("cannot tokenise a module file: {e}"),
    };
    let mut decls = Decls::default();
    // Whether the item now beginning carries an unevaluable, compilation-gating
    // outer attribute.
    let mut gated = false;
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            // An attribute: `#` `[ … ]` (outer) or `#` `!` `[ … ]` (inner).
            proc_macro2::TokenTree::Punct(p) if p.as_char() == '#' => {
                let inner = matches!(
                    tokens.get(i + 1),
                    Some(proc_macro2::TokenTree::Punct(q)) if q.as_char() == '!'
                );
                let bracket_at = if inner { i + 2 } else { i + 1 };
                if let Some(proc_macro2::TokenTree::Group(g)) = tokens.get(bracket_at)
                    && g.delimiter() == proc_macro2::Delimiter::Bracket
                {
                    // Inner attributes decorate the enclosing module, so they never
                    // gate a following item. An outer one gates only if it is one of
                    // the compilation-affecting attributes.
                    if !inner && attr_gates_compilation(g) {
                        gated = true;
                    }
                    i = bracket_at + 1;
                    continue;
                }
                gated = false;
                i += 1;
            }
            proc_macro2::TokenTree::Ident(id) => {
                let word = id.to_string();
                if word == "pub" {
                    // Visibility keyword; skip it and any `(…)` restriction, keeping
                    // whatever attribute gates the item it introduces.
                    i += 1;
                    if let Some(proc_macro2::TokenTree::Group(g)) = tokens.get(i)
                        && g.delimiter() == proc_macro2::Delimiter::Parenthesis
                    {
                        i += 1;
                    }
                } else if word == "mod" {
                    // A file module is `mod NAME ;`; `mod NAME { … }` is inline and
                    // owns no file, so it is not a declaration we check.
                    if let Some(proc_macro2::TokenTree::Ident(name)) = tokens.get(i + 1)
                        && matches!(
                            tokens.get(i + 2),
                            Some(proc_macro2::TokenTree::Punct(p)) if p.as_char() == ';'
                        )
                    {
                        let name = name.to_string();
                        if gated {
                            decls.conditional.insert(name);
                        } else {
                            decls.plain.insert(name);
                        }
                    }
                    gated = false;
                    i += 1;
                } else {
                    // Some other item keyword (`fn`, `struct`, `use`, …) consumes the
                    // pending attribute.
                    gated = false;
                    i += 1;
                }
            }
            // Any other top-level token ends the current item's attribute reach.
            _ => {
                gated = false;
                i += 1;
            }
        }
    }
    decls
}

/// Does this outer attribute gate whether — or from which file — its item is
/// compiled? `cfg`/`cfg_attr` gate compilation; `path` redirects the module's
/// file so our `mod NAME` ↔ `NAME.rs` mapping no longer holds. Everything else
/// (`allow`, `doc`, `derive`, …) leaves the declaration plain.
fn attr_gates_compilation(bracket: &proc_macro2::Group) -> bool {
    matches!(
        bracket.stream().into_iter().next(),
        Some(proc_macro2::TokenTree::Ident(id))
            if matches!(id.to_string().as_str(), "cfg" | "cfg_attr" | "path")
    )
}

/// Walk `dir` (whose module file is `module_file`), reporting every Rust file
/// and code-bearing subdirectory it holds that `module_file` does not declare
/// into `out`, every one it declares *conditionally* into `conditional`, and
/// recursing into the ones it declares plainly. Reported paths are relative to
/// `root`.
fn collect_undeclared(
    dir: &Path,
    module_file: &Path,
    root: &Path,
    out: &mut Vec<String>,
    conditional: &mut Vec<String>,
) {
    let decls = declared_mods(module_file);
    let mod_rel = module_file
        .strip_prefix(root)
        .unwrap_or(module_file)
        .to_string_lossy()
        .into_owned();
    for name in &decls.conditional {
        conditional.push(format!("{mod_rel}: mod {name}"));
    }
    // A name is "known" — not *undeclared* — whether it is declared plainly or
    // conditionally; a conditional one is already reported above, so counting it
    // undeclared too would just double the noise for one fix.
    let known = |name: &str| decls.plain.contains(name) || decls.conditional.contains(name);

    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|e| e.expect("cannot stat a tests directory entry").path())
        .collect();
    entries.sort();

    for path in entries {
        let name = path
            .file_stem()
            .expect("a directory entry should have a name")
            .to_string_lossy()
            .into_owned();
        let rel = || {
            path.strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned()
        };

        if path.is_dir() {
            // A subdirectory is a module iff it holds Rust code (`fixtures/` is
            // data). `common` is a helper, but it is `mod`-declared like any
            // other group, so it needs no special case.
            if !holds_rust(&path) {
                continue;
            }
            let inner = path.join("mod.rs");
            let sibling = dir.join(format!("{name}.rs"));
            if inner.is_file() {
                // Recurse only into a *plainly* declared module: a conditional one
                // is already rejected, and an undeclared one is dead below anyway.
                if decls.plain.contains(&name) {
                    collect_undeclared(&path, &inner, root, out, conditional);
                } else if !known(&name) {
                    out.push(rel());
                }
            } else if sibling.is_file() {
                // Rust's other module form: `<name>.rs` next to `<name>/`. Its
                // declaration is checked by the file arm below, so only recurse.
                if decls.plain.contains(&name) {
                    collect_undeclared(&path, &sibling, root, out, conditional);
                }
            } else {
                // No module file at all: nothing can `mod` this into the crate,
                // so every case under it is dead however main.rs is written.
                out.push(rel());
            }
        } else if path.extension().is_some_and(|x| x == "rs")
            // The owning module file itself is not one of its own children —
            // but only *that* file is exempt, compared by path. Comparing by
            // name would exempt `group/group.rs` from `group.rs`'s list too, so
            // a nested case named after its group would never need declaring.
            && path != module_file
            && !known(&name)
        {
            out.push(rel());
        }
    }
}

/// Does this directory hold Rust code, at any depth? `fixtures/` holds data and
/// is not a module; a directory whose Rust sits one level down still is one.
fn holds_rust(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .any(|e| {
            let p = e.path();
            if p.is_dir() {
                holds_rust(&p)
            } else {
                p.extension().is_some_and(|x| x == "rs")
            }
        })
}

/// The test targets Cargo would auto-discover in `tests/` besides `all/`: a
/// loose `<group>.rs`, or a `<group>/main.rs` directory-form target.
fn stray_test_targets(tests_dir: &Path) -> Vec<String> {
    let mut stray: Vec<String> = std::fs::read_dir(tests_dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", tests_dir.display()))
        .map(|e| e.expect("cannot stat a tests directory entry").path())
        .filter(|p| {
            if p.is_dir() {
                p.file_name() != Some("all".as_ref()) && p.join("main.rs").is_file()
            } else {
                p.extension().is_some_and(|x| x == "rs")
            }
        })
        .map(|p| {
            p.strip_prefix(tests_dir)
                .unwrap_or(&p)
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    stray.sort();
    stray
}

#[cfg(test)]
mod tests {
    use super::{collect_undeclared, declared_mods_in, stray_test_targets};
    use proptest::prelude::*;
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A `tests/` tree: `all/main.rs` declaring `groups`, plus whatever extra
    /// files the case writes. Returns the `tests/` dir.
    fn scaffold(dir: &Path, groups: &[&str]) -> PathBuf {
        let tests = dir.join("tests");
        let all = tests.join("all");
        fs::create_dir_all(&all).expect("create tests/all");
        let decls: String = groups.iter().map(|g| format!("mod {g};\n")).collect();
        fs::write(all.join("main.rs"), decls).expect("write main.rs");
        tests
    }

    fn write(path: PathBuf, body: &str) {
        fs::create_dir_all(path.parent().expect("a parent")).expect("create parent");
        fs::write(path, body).expect("write file");
    }

    fn undeclared(tests: &Path) -> Vec<String> {
        let (mut out, _) = collect(tests);
        out.sort();
        out
    }

    fn conditional(tests: &Path) -> Vec<String> {
        let (_, mut cond) = collect(tests);
        cond.sort();
        cond
    }

    fn collect(tests: &Path) -> (Vec<String>, Vec<String>) {
        let all = tests.join("all");
        let mut out = Vec::new();
        let mut cond = Vec::new();
        collect_undeclared(&all, &all.join("main.rs"), tests, &mut out, &mut cond);
        (out, cond)
    }

    #[test]
    fn a_fully_declared_tree_is_clean() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tests = scaffold(tmp.path(), &["common", "cases"]);
        write(tests.join("all/cases.rs"), "#[test] fn t() {}");
        // A nested group, declared all the way down, with a data-only sibling.
        write(tests.join("all/common/mod.rs"), "pub mod inner;\n");
        write(tests.join("all/common/inner/mod.rs"), "mod leaf;\n");
        write(tests.join("all/common/inner/leaf.rs"), "");
        write(tests.join("fixtures/data.json"), "{}");

        assert!(undeclared(&tests).is_empty());
        assert!(stray_test_targets(&tests).is_empty());
    }

    #[test]
    fn an_undeclared_top_level_group_is_caught() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tests = scaffold(tmp.path(), &[]);
        write(tests.join("all/orphan.rs"), "#[test] fn t() { panic!() }");

        assert_eq!(undeclared(&tests), ["all/orphan.rs"]);
    }

    /// The gap a flat scan leaves: the group is declared in `main.rs`, so it
    /// looks fine from the top, but a case *inside* it is not.
    #[test]
    fn an_undeclared_case_inside_a_declared_group_is_caught() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tests = scaffold(tmp.path(), &["group"]);
        write(tests.join("all/group/mod.rs"), "mod declared;\n");
        write(tests.join("all/group/declared.rs"), "");
        write(
            tests.join("all/group/orphan.rs"),
            "#[test] fn t() { panic!() }",
        );

        assert_eq!(undeclared(&tests), ["all/group/orphan.rs"]);
    }

    /// Rust's other module form: `group.rs` beside `group/`. The cases under it
    /// are reachable, so only a missing `mod` line inside `group.rs` is a fault.
    #[test]
    fn the_sibling_file_module_form_is_followed_not_flagged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tests = scaffold(tmp.path(), &["group"]);
        write(tests.join("all/group.rs"), "mod declared;\n");
        write(tests.join("all/group/declared.rs"), "");
        write(tests.join("all/group/orphan.rs"), "");

        assert_eq!(undeclared(&tests), ["all/group/orphan.rs"]);
    }

    /// A code-bearing directory with no module file at all cannot be `mod`-ed
    /// in from anywhere, so it is dead however main.rs is written.
    #[test]
    fn a_directory_with_no_module_file_is_caught() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tests = scaffold(tmp.path(), &["group"]);
        write(
            tests.join("all/group/case.rs"),
            "#[test] fn t() { panic!() }",
        );

        assert_eq!(undeclared(&tests), ["all/group"]);
    }

    /// A declaration the compiler cannot see is not a declaration. The block
    /// form is the one a per-line scan cannot catch by construction: the `mod`
    /// line itself carries no comment marker.
    #[test]
    fn a_commented_out_declaration_does_not_count_as_one() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tests = scaffold(tmp.path(), &[]);
        fs::write(
            tests.join("all/main.rs"),
            "/*\nmod orphan;\n*/\n// mod also_orphan;\n",
        )
        .expect("write main.rs");
        write(tests.join("all/orphan.rs"), "#[test] fn t() { panic!() }");
        write(
            tests.join("all/also_orphan.rs"),
            "#[test] fn t() { panic!() }",
        );

        assert_eq!(
            undeclared(&tests),
            ["all/also_orphan.rs", "all/orphan.rs"],
            "a `mod` line inside a comment is not a declaration"
        );
    }

    /// The sibling form again, with the nested case named after its own group:
    /// `group.rs` owns `group/`, so `group/group.rs` is `group::group` and needs
    /// declaring like any other case. Exempting the owning module file by *name*
    /// rather than path would wave it through.
    #[test]
    fn a_child_module_named_after_its_group_still_needs_declaring() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tests = scaffold(tmp.path(), &["group"]);
        write(tests.join("all/group.rs"), "mod declared;\n");
        write(tests.join("all/group/declared.rs"), "");
        write(
            tests.join("all/group/group.rs"),
            "#[test] fn t() { panic!() }",
        );

        assert_eq!(undeclared(&tests), ["all/group/group.rs"]);

        // …and it is clean once declared.
        fs::write(tests.join("all/group.rs"), "mod declared;\nmod group;\n")
            .expect("rewrite group.rs");
        assert!(undeclared(&tests).is_empty());
    }

    /// How a generated snippet mentions `mod m<i>;`: as a plain declaration the
    /// compiler compiles, as one gated behind a `cfg`/`path` attribute (conditional
    /// — the guard must refuse it), or as tokens the compiler ignores entirely.
    #[derive(Debug, Clone, Copy)]
    enum Kind {
        Plain,
        Conditional,
        Hidden,
    }

    #[derive(Debug, Clone, Copy)]
    struct Item {
        kind: Kind,
        form: u8,
    }

    /// Render `Item` `i` as source. Plain forms include *benign* attributes and
    /// doc comments, which do not gate compilation and so leave the declaration
    /// plain; conditional forms carry a `cfg`/`cfg_attr`/`path` gate; hidden forms
    /// are every way we know of to write `mod {name};` where the compiler sees no
    /// declaration — the set the tokeniser must not be fooled by.
    fn render(i: usize, item: Item) -> String {
        let name = format!("m{i}");
        match item.kind {
            Kind::Plain => match item.form % 8 {
                0 => format!("mod {name};"),
                1 => format!("pub mod {name};"),
                2 => format!("pub(crate) mod {name};"),
                3 => format!("    mod {name};  "),
                4 => format!("mod {name}; // a trailing comment"),
                // Benign outer attribute: does not gate compilation.
                5 => format!("#[allow(dead_code)]\nmod {name};"),
                // A doc comment (the lexer turns it into `#[doc=…]`): still benign.
                6 => format!("/// docs for the module\npub mod {name};"),
                // Inner attribute on the line above: decorates the enclosing module.
                _ => format!("#![allow(dead_code)]\nmod {name};"),
            },
            Kind::Conditional => match item.form % 5 {
                0 => format!("#[cfg(any())]\nmod {name};"),
                1 => format!("#[cfg(any())] mod {name};"),
                2 => format!("#[cfg(feature = \"x\")]\npub mod {name};"),
                3 => format!("#[path = \"z.rs\"]\nmod {name};"),
                // A multi-line attribute the mod sits below.
                _ => format!("#[cfg_attr(\n    unix,\n    path = \"z.rs\",\n)]\nmod {name};"),
            },
            Kind::Hidden => match item.form % 10 {
                0 => format!("// mod {name};"),
                1 => format!("/* mod {name}; */"),
                2 => format!("/*\nmod {name};\n*/"),
                3 => format!("/* /* mod {name}; */ still commented */"),
                4 => format!("//! doc: enable with mod {name};"),
                5 => format!("const S: &str = \"mod {name};\";"),
                6 => format!("const S: &str = r#\"mod {name};\"#;"),
                // A raw C string — its own literal prefix, which a hand lexer missed.
                7 => format!("const S: &core::ffi::CStr = cr#\"mod {name};\"#;"),
                // Inside a macro body (never expanded here) — a nested group.
                8 => format!("macro_rules! m {{ () => {{\n    mod {name};\n}} }}"),
                // Inside a function body — also a nested group.
                _ => format!("fn f() {{\n    mod {name};\n}}"),
            },
        }
    }

    proptest! {
        /// The scanner splits declarations exactly as the compiler would: plain
        /// ones (including benign-attributed) reported plain, `cfg`/`path`-gated
        /// ones reported conditional for the guard to reject, disguised ones
        /// reported not at all — however the rest are dressed up.
        #[test]
        fn the_scanner_classifies_declarations_as_the_compiler_would(
            items in prop::collection::vec(
                (0u8..3, any::<u8>()).prop_map(|(k, form)| Item {
                    kind: [Kind::Plain, Kind::Conditional, Kind::Hidden][k as usize],
                    form,
                }),
                0..12,
            ),
        ) {
            let src: String = items
                .iter()
                .enumerate()
                .map(|(i, &item)| format!("{}\n", render(i, item)))
                .collect();
            let want = |k: Kind| -> BTreeSet<String> {
                items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| {
                        matches!(
                            (item.kind, k),
                            (Kind::Plain, Kind::Plain) | (Kind::Conditional, Kind::Conditional)
                        )
                    })
                    .map(|(i, _)| format!("m{i}"))
                    .collect()
            };
            let decls = declared_mods_in(&src);
            prop_assert_eq!(&decls.plain, &want(Kind::Plain), "plain; source:\n{}", src);
            prop_assert_eq!(&decls.conditional, &want(Kind::Conditional), "conditional; source:\n{}", src);
        }
    }

    /// Lifetimes and char literals share the `'`, and a mishandled `'"'` would
    /// once open a phantom string that swallowed the rest of the file — the real
    /// lexer settles all of this; the case stays as a regression guard.
    #[test]
    fn quotes_that_are_not_strings_do_not_swallow_the_file() {
        let src = "\
fn f<'a>(x: &'a str) -> char { '\"' }
const C: char = '\\'';
mod visible;
";
        assert_eq!(
            declared_mods_in(src).plain,
            BTreeSet::from(["visible".to_owned()])
        );
    }

    /// A benign outer attribute (or doc comment) does not gate compilation, so the
    /// `mod` it decorates is a plain declaration — not a conditional one the guard
    /// would refuse. Only `cfg`/`cfg_attr`/`path` gate.
    #[test]
    fn a_benign_attribute_leaves_the_declaration_plain() {
        let decls = declared_mods_in(
            "#[allow(dead_code)]\nmod a;\n/// docs\npub mod b;\n#[rustfmt::skip]\nmod c;\n",
        );
        assert_eq!(
            decls.plain,
            BTreeSet::from(["a".to_owned(), "b".to_owned(), "c".to_owned()])
        );
        assert!(decls.conditional.is_empty());
    }

    /// A raw C string literal (`cr#"…"#`) is one token; the `mod` inside its body
    /// is not a declaration. The hand-rolled lexer missed the `c` prefix; the real
    /// one does not.
    #[test]
    fn a_raw_c_string_hides_its_mod() {
        let decls =
            declared_mods_in("mod real;\nconst S: &core::ffi::CStr = cr#\"\" mod orphan;\"#;\n");
        assert_eq!(decls.plain, BTreeSet::from(["real".to_owned()]));
        assert!(decls.conditional.is_empty());
    }

    /// An *inner* attribute (`#![…]`) decorates the module it sits in, not the
    /// item below it, so a `mod` beneath one is plainly declared — exactly the
    /// `common/mod.rs` shape (`#![allow(dead_code)]` above `pub mod …;`).
    #[test]
    fn an_inner_attribute_does_not_gate_the_mod_below_it() {
        let decls = declared_mods_in("#![allow(dead_code)]\npub mod normalised_ast;\nmod leaf;\n");
        assert_eq!(
            decls.plain,
            BTreeSet::from(["normalised_ast".to_owned(), "leaf".to_owned()])
        );
        assert!(decls.conditional.is_empty());
    }

    /// A case group declared behind a `cfg` the guard cannot evaluate is refused,
    /// not silently trusted: rustc may or may not compile `orphan.rs`, so a green
    /// suite would not mean it ran.
    #[test]
    fn a_cfg_gated_group_is_reported_conditional_not_trusted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tests = scaffold(tmp.path(), &[]);
        // Separate-line form: the `mod` line alone looks like a plain declaration.
        fs::write(tests.join("all/main.rs"), "#[cfg(any())]\nmod orphan;\n")
            .expect("write main.rs");
        write(tests.join("all/orphan.rs"), "#[test] fn t() {}");

        assert_eq!(conditional(&tests), ["all/main.rs: mod orphan"]);
        // …and it is not *also* double-reported as undeclared.
        assert!(undeclared(&tests).is_empty());
    }

    /// A `mod NAME;` inside a `macro_rules!` body is not a file-level declaration —
    /// rustc never expands the macro here, so it compiles no `orphan.rs`. The
    /// scanner must not be fooled into treating the file as reachable.
    #[test]
    fn a_mod_inside_a_macro_body_is_not_a_declaration() {
        let decls =
            declared_mods_in("mod real;\nmacro_rules! m {\n    () => { mod orphan; };\n}\n");
        assert_eq!(decls.plain, BTreeSet::from(["real".to_owned()]));
        assert!(decls.conditional.is_empty());
    }

    /// An *inner* `#![cfg(unix)]` gates a whole case file off-Unix by design (as
    /// `assembly_cache_*` do). It is `mod`-declared and the file exists, so the
    /// guard passes — that is intended, not a silent omission: the guard reads
    /// declarations from the owning module file, never a case file's own inner
    /// attributes, and does not evaluate `cfg`.
    #[test]
    fn a_deliberately_cfg_gated_case_file_still_passes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tests = scaffold(tmp.path(), &["gated"]);
        write(
            tests.join("all/gated.rs"),
            "#![cfg(unix)]\n#[test] fn t() {}",
        );

        assert!(undeclared(&tests).is_empty());
        assert!(conditional(&tests).is_empty());
    }

    /// Both forms Cargo auto-discovers as an integration-test target — the
    /// loose file that this module's own history produced, and the directory
    /// form. `fixtures/` is neither.
    #[test]
    fn both_stray_test_target_forms_are_caught() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tests = scaffold(tmp.path(), &[]);
        write(tests.join("loose.rs"), "");
        write(tests.join("dir_form/main.rs"), "");
        write(tests.join("fixtures/data.json"), "{}");
        write(tests.join("fixtures/nested/more.json"), "{}");

        assert_eq!(stray_test_targets(&tests), ["dir_form", "loose.rs"]);
    }
}
