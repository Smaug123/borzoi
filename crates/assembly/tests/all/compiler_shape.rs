//! [`compiler_shape`] accepts what compilers write: every type of the
//! reference pack, and of C# fixtures csc compiled (compiler-generated
//! display classes, backing fields, state machines and explicit
//! implementations included). A consumer that declines outside the shape
//! pays for every type this rejects, so a rejection here is a cost, and the
//! test names it.
//!
//! The converse — that hand-made metadata outside the shape is rejected —
//! is the consumers' property: the LSP's generated-metadata differential
//! requires every entry built from such metadata to decline.

use std::path::Path;

use borzoi_assembly::shape::{Unshaped, compiler_shape};
use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity};

use crate::common::{ensure_doc_ids_built, ensure_member_shapes_built, sdk_ref_pack_dir};

/// Every type of `path`, nested ones included, that is not compiler-shaped,
/// with why; and how many types there were.
fn unshaped(path: &Path) -> (Vec<(String, Unshaped)>, usize) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let view = Ecma335Assembly::parse(&bytes).unwrap_or_else(|e| panic!("parse {path:?}: {e}"));
    let Ok(entities) = view.enumerate_type_defs() else {
        return (Vec::new(), 0);
    };
    fn walk(
        e: &Entity,
        enclosing: Option<&Entity>,
        out: &mut Vec<(String, Unshaped)>,
        n: &mut usize,
    ) {
        *n += 1;
        if let Err(why) = compiler_shape(e, enclosing) {
            out.push((format!("{}.{}", e.namespace.join("."), e.name), why));
        }
        for nested in &e.nested_types {
            walk(nested, Some(e), out, n);
        }
    }
    let (mut out, mut n) = (Vec::new(), 0);
    for e in &entities {
        walk(e, None, &mut out, &mut n);
    }
    (out, n)
}

fn assert_all_shaped(label: &str, paths: &[std::path::PathBuf]) {
    let mut types = 0;
    let mut rejected = Vec::new();
    for path in paths {
        let (bad, n) = unshaped(path);
        types += n;
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        rejected.extend(
            bad.into_iter()
                .map(|(t, why)| format!("{name}: {t}: {why:?}")),
        );
    }
    assert!(types > 0, "{label}: no types read");
    assert!(
        rejected.is_empty(),
        "{label}: {} of {types} types are not compiler-shaped:\n{}",
        rejected.len(),
        rejected
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_reference_pack_is_compiler_shaped() {
    let dir = sdk_ref_pack_dir();
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read ref pack dir {dir:?}: {e}"))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "dll"))
        .collect();
    paths.sort();
    assert!(paths.len() > 100, "ref pack looks empty: {dir:?}");
    assert_all_shaped("reference pack", &paths);
}

#[test]
fn csc_output_is_compiler_shaped() {
    assert_all_shaped(
        "csc fixtures",
        &[
            ensure_member_shapes_built().to_path_buf(),
            ensure_doc_ids_built().to_path_buf(),
        ],
    );
}

/// Over the highest version of each package under `BORZOI_SHAPE_SWEEP_ROOT`
/// (a NuGet cache), its `lib/` and `ref/` assemblies not compiled by F#
/// (which a consumer reads in F#'s source view instead): how many types are
/// not compiler-shaped, by cause, with the first offending member of each.
/// A measurement, not a gate: packages ship hand-written IL too.
#[test]
#[ignore = "measurement over a local package cache"]
fn package_cache_shape_census() {
    let root = std::path::PathBuf::from(
        std::env::var("BORZOI_SHAPE_SWEEP_ROOT").expect("BORZOI_SHAPE_SWEEP_ROOT"),
    );
    let mut dlls = Vec::new();
    for package in std::fs::read_dir(&root).unwrap().flatten() {
        let Some(latest) = std::fs::read_dir(package.path())
            .into_iter()
            .flatten()
            .flatten()
            .map(|v| v.path())
            .filter(|v| v.is_dir())
            .max()
        else {
            continue;
        };
        let mut stack: Vec<_> = ["lib", "ref"].iter().map(|d| latest.join(d)).collect();
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "dll") {
                    dlls.push(p);
                }
            }
        }
    }
    dlls.sort();
    let (mut read, mut types) = (0, 0);
    let mut by_cause: std::collections::BTreeMap<String, (usize, Vec<String>)> =
        std::collections::BTreeMap::new();
    let mut by_package: std::collections::BTreeMap<(String, String), usize> =
        std::collections::BTreeMap::new();
    let mut culprits: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for path in &dlls {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(view) = Ecma335Assembly::parse(&bytes) else {
            continue;
        };
        if view.fsharp_resources().map_or(true, |r| !r.is_empty()) {
            continue;
        }
        let Ok(entities) = view.enumerate_type_defs() else {
            continue;
        };
        read += 1;
        fn walk(
            e: &Entity,
            enclosing: Option<&Entity>,
            f: &mut dyn FnMut(&Entity, Option<&Entity>),
        ) {
            f(e, enclosing);
            for n in &e.nested_types {
                walk(n, Some(e), f);
            }
        }
        for e in &entities {
            walk(e, None, &mut |e, enclosing| {
                types += 1;
                let Err(why) = compiler_shape(e, enclosing) else {
                    return;
                };
                let package = path
                    .strip_prefix(&root)
                    .ok()
                    .and_then(|p| p.iter().next())
                    .map(|c| c.to_string_lossy().into_owned())
                    .unwrap_or_default();
                *by_package.entry((format!("{why:?}"), package)).or_default() += 1;
                if matches!(why, Unshaped::MemberName | Unshaped::TypeName) {
                    let name = e
                        .members
                        .iter()
                        .find(|m| {
                            let mut one = e.clone();
                            one.members = vec![(*m).clone()];
                            compiler_shape(&one, enclosing).is_err()
                        })
                        .map(|m| match m {
                            borzoi_assembly::Member::Method(m) => m.name.clone(),
                            borzoi_assembly::Member::Field(f) => f.name.clone(),
                            borzoi_assembly::Member::Property(p) => p.name.clone(),
                            borzoi_assembly::Member::Event(e) => e.name.clone(),
                        })
                        .unwrap_or_else(|| format!("type {}", e.name));
                    *culprits.entry(name).or_default() += 1;
                }
                let slot = by_cause.entry(format!("{why:?}")).or_default();
                slot.0 += 1;
                if slot.1.len() < 30 {
                    let culprit = e
                        .members
                        .iter()
                        .find(|m| {
                            let mut one = e.clone();
                            one.members = vec![(*m).clone()];
                            compiler_shape(&one, enclosing).is_err()
                        })
                        .map(|m| format!("{m:?}").chars().take(160).collect::<String>())
                        .unwrap_or_default();
                    let file = path
                        .strip_prefix(&root)
                        .unwrap_or(path)
                        .display()
                        .to_string();
                    slot.1.push(format!(
                        "{file}: {}.{} :: {culprit}",
                        e.namespace.join("."),
                        e.name
                    ));
                }
            });
        }
    }
    eprintln!(
        "{read} non-F# assemblies of {} files, {types} types",
        dlls.len()
    );
    for (cause, (n, eg)) in &by_cause {
        eprintln!("{cause}: {n}");
        for e in eg {
            eprintln!("    {e}");
        }
    }
    eprintln!("names (types):");
    for (name, n) in &culprits {
        eprintln!("  {n} {name}");
    }
    eprintln!("by package (cause, package: types):");
    for ((cause, package), n) in &by_package {
        eprintln!("  {cause} {package}: {n}");
    }
}

/// Retyping any one accessor position of every property and event in the
/// reference pack and in csc output leaves no bindable member that was
/// compiler-shaped still shaped (`borzoi_assembly::shape_metamorphic`).
#[test]
fn a_retyped_accessor_is_never_compiler_shaped() {
    use borzoi_assembly::shape_metamorphic::{POSITIONS, retyped_accessors_are_unshaped};
    let dir = sdk_ref_pack_dir();
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "dll"))
        .collect();
    paths.sort();
    paths.push(ensure_member_shapes_built().to_path_buf());
    let mut failures = Vec::new();
    for position in POSITIONS {
        let mut retyped = 0;
        let mut findings = Vec::new();
        for path in &paths {
            let bytes = std::fs::read(path).unwrap();
            let Ok(outcome) = borzoi_assembly::shape_metamorphic::retyped_accessors_are_unshaped(
                &bytes, position,
            ) else {
                continue;
            };
            retyped += outcome.retyped;
            findings.extend(outcome.findings);
        }
        eprintln!(
            "{position:?}: {retyped} retyped, {} still shaped",
            findings.len()
        );
        assert!(retyped > 0, "{position:?}: the probe retyped nothing");
        if !findings.is_empty() {
            failures.push(format!(
                "{position:?}: {} of {retyped}, e.g.\n  {}",
                findings.len(),
                findings
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n  ")
            ));
        }
    }
    let _ = retyped_accessors_are_unshaped;
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Renaming every type, or every ordinary method, in a way a documentation
/// ID reads otherwise leaves no compiler-shaped type accepted, nor rejected
/// for another reason (`borzoi_assembly::shape_metamorphic`).
#[test]
fn a_name_an_id_reads_otherwise_is_never_compiler_shaped() {
    use borzoi_assembly::shape_metamorphic::{RENAMES, renamed_names_are_unshaped};
    let dir = sdk_ref_pack_dir();
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "dll"))
        .collect();
    paths.sort();
    paths.push(ensure_member_shapes_built().to_path_buf());
    let mut failures = Vec::new();
    for rename in RENAMES {
        let mut renamed = 0;
        let mut findings = Vec::new();
        for path in &paths {
            let bytes = std::fs::read(path).unwrap();
            let Ok(outcome) = renamed_names_are_unshaped(&bytes, rename) else {
                continue;
            };
            renamed += outcome.retyped;
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            findings.extend(outcome.findings.into_iter().map(|f| format!("{name}: {f}")));
        }
        eprintln!(
            "{rename:?}: {renamed} renamed, {} not rejected for it",
            findings.len()
        );
        assert!(renamed > 0, "{rename:?}: the probe renamed nothing");
        if !findings.is_empty() {
            failures.push(format!(
                "{rename:?}: {} of {renamed}, e.g.\n  {}",
                findings.len(),
                findings
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n  ")
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
