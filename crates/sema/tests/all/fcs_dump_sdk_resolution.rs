//! The project oracles' shared script resolution against the per-directory one
//! it stands in for.
//!
//! FCS runs `dotnet --version` once for each directory a script sits in, to
//! honour any `global.json` above it. Each grid cell gets its own temporary
//! directory, so fcs-dump resolves a project's script options in a shared
//! ancestor instead (`sdkResolutionDir`, `projectScriptOptions`). That is
//! sound only if the shared resolution equals the one made in the first
//! file's own directory. This test checks that equality over layouts that
//! each break one part of the walk's rule: a `global.json` beside the file and
//! one above it, a directory link whose target sits under a `global.json` its
//! lexical parents never see, and an `#r` resolved relative to the script.
//!
//! The pinned `global.json` names an SDK no machine has, so `dotnet --version`
//! fails there and FCS warns (FS3384). That warning is the observable: a walk
//! that went past the `global.json` would resolve where nothing warns. The
//! test pins the warning per row, so a host where the pin stops being
//! observable fails instead of passing vacuously.
//!
//! Unix only: the link row needs `std::os::unix::fs::symlink`.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::common::{ensure_contributor_fixture_built, invoke_fcs_dump_script_options};

#[derive(Deserialize, PartialEq, Debug)]
#[serde(rename_all = "PascalCase")]
struct Side {
    other_options: Vec<String>,
    diagnostics: Vec<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct Answer {
    resolved_as: PathBuf,
    shared: Side,
    direct: Side,
}

/// A `global.json` pinning an SDK version no machine has.
const UNSATISFIABLE_PIN: &str = r#"{ "sdk": { "version": "1.0.0", "rollForward": "disable" } }"#;

const MODULE: &str = "module P\nlet s = 1\n";

/// Where a row expects the shared resolution to have run.
enum Expect {
    /// In some ancestor of the file's directory, not the directory itself.
    SharedAncestor,
    /// In this directory.
    In(PathBuf),
    /// As the file itself.
    Own,
}

struct Row {
    label: &'static str,
    file: PathBuf,
    expect: Expect,
    /// Whether FCS warns that it could not determine the SDK.
    warns: bool,
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("create dir");
    std::fs::write(path, text).expect("write file");
}

fn layouts(root: &Path) -> Vec<Row> {
    let plain = root.join("plain/P.fs");
    write(&plain, MODULE);

    let pinned = root.join("pinned");
    write(&pinned.join("global.json"), UNSATISFIABLE_PIN);
    write(&pinned.join("P.fs"), MODULE);

    let pinned_above = root.join("pinned_above");
    write(&pinned_above.join("global.json"), UNSATISFIABLE_PIN);
    write(&pinned_above.join("cell/P.fs"), MODULE);

    // `links/cell` is a link to `target/real`: its real path sits under
    // `target/global.json`, which no lexical parent of `links/cell` holds.
    let target = root.join("target");
    write(&target.join("global.json"), UNSATISFIABLE_PIN);
    write(&target.join("real/P.fs"), MODULE);
    std::fs::create_dir_all(root.join("links")).expect("create links");
    std::os::unix::fs::symlink(target.join("real"), root.join("links/cell")).expect("symlink");

    // An `#r` resolved against the script's own directory.
    let hash_r = root.join("hash_r");
    let fixture = ensure_contributor_fixture_built();
    std::fs::create_dir_all(&hash_r).expect("create hash_r");
    std::fs::copy(fixture, hash_r.join("Fixture.dll")).expect("copy fixture");
    write(&hash_r.join("P.fsx"), "#r \"Fixture.dll\"\nlet h = 1\n");

    vec![
        Row {
            label: "no global.json anywhere near",
            file: plain,
            expect: Expect::SharedAncestor,
            warns: false,
        },
        Row {
            label: "global.json beside the file",
            file: pinned.join("P.fs"),
            expect: Expect::In(pinned.clone()),
            warns: true,
        },
        Row {
            label: "global.json in the parent",
            file: pinned_above.join("cell/P.fs"),
            expect: Expect::In(pinned_above.clone()),
            warns: true,
        },
        Row {
            label: "a link whose target sits under a global.json",
            file: root.join("links/cell/P.fs"),
            expect: Expect::In(root.join("links/cell")),
            warns: true,
        },
        Row {
            label: "an #r relative to the script",
            file: hash_r.join("P.fsx"),
            expect: Expect::Own,
            warns: false,
        },
    ]
}

#[test]
fn shared_script_resolution_is_the_per_directory_one() {
    let root =
        std::env::temp_dir().join(format!("borzoi_sema_sdk_resolution_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let rows = layouts(&root);
    let paths: Vec<PathBuf> = rows.iter().map(|r| r.file.clone()).collect();
    let out = invoke_fcs_dump_script_options(&paths);
    let _ = std::fs::remove_dir_all(&root);

    let answers: Vec<Answer> = out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect();
    assert_eq!(answers.len(), rows.len(), "one answer per path:\n{out}");

    let mut failures = Vec::new();
    for (row, answer) in rows.iter().zip(&answers) {
        if answer.shared != answer.direct {
            failures.push(format!(
                "{}: the shared resolution differs\n  shared: {:?}\n  direct: {:?}",
                row.label, answer.shared, answer.direct
            ));
        }
        let dir = row.file.parent().expect("a parent");
        let name = row.file.file_name().expect("a file name");
        let where_ok = match &row.expect {
            Expect::SharedAncestor => {
                answer.resolved_as.file_name() == Some(name)
                    && dir.starts_with(answer.resolved_as.parent().expect("a parent"))
                    && answer.resolved_as.parent() != Some(dir)
            }
            Expect::In(d) => answer.resolved_as == d.join(name),
            Expect::Own => answer.resolved_as == row.file,
        };
        if !where_ok {
            failures.push(format!(
                "{}: resolved as {}",
                row.label,
                answer.resolved_as.display()
            ));
        }
        let warned = answer
            .direct
            .diagnostics
            .iter()
            .any(|d| d == "Warning FS3384");
        if warned != row.warns {
            failures.push(format!(
                "{}: expected {} FS3384, got {:?}",
                row.label,
                if row.warns { "a" } else { "no" },
                answer.direct.diagnostics
            ));
        }
    }
    let hash_r = rows
        .iter()
        .zip(&answers)
        .find(|(r, _)| matches!(r.expect, Expect::Own))
        .expect("the #r row");
    if !hash_r
        .1
        .direct
        .other_options
        .iter()
        .any(|o| o.starts_with("-r:") && o.ends_with("Fixture.dll"))
    {
        failures.push(format!(
            "the #r row no longer references its DLL, so it tests nothing: {:?}",
            hash_r.1.direct.other_options
        ));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
