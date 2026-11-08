//! The xml-doc oracle's answer to a request depends on that request alone.
//!
//! The resident `xmldoc-batch` child serves the whole suite in sequence, so
//! any state one request leaves behind moves the answers to later ones,
//! whichever test sent them. The property: a probe project answered after
//! each of a set of earlier requests gets byte for byte the answer a fresh
//! child gives it.
//!
//! The earlier requests are the ways a project's location can move script
//! resolution: an `.fsx` whose `#r` names a DLL beside it, a project under a
//! `global.json`, and a plain project. The probe names a type in that DLL, so
//! it resolves only where the reference leaked: a fresh child reports it
//! unbound. That `#r` is the row that discriminates on every host. The pin
//! names an SDK no machine has, and FCS then falls back to the newest one,
//! so the `global.json` row moves the answer only where that differs from
//! the SDK the probe's own directory selects.

use std::path::{Path, PathBuf};

use crate::common::{ensure_fsharp_compiler_service_dll, fcs_dump_batch_child};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("create dir");
    std::fs::write(path, text).expect("write file");
}

fn request(paths: &[&Path]) -> String {
    serde_json::json!({
        "paths": paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "refs": [],
    })
    .to_string()
}

#[test]
fn an_answer_does_not_depend_on_earlier_requests() {
    let root = tempfile::TempDir::new().expect("temp dir");
    let root = root.path();
    let dll = ensure_fsharp_compiler_service_dll();

    let hash_r = root.join("hash_r");
    std::fs::create_dir_all(&hash_r).expect("create hash_r");
    std::fs::copy(&dll, hash_r.join("FSharp.Compiler.Service.dll")).expect("copy dll");
    write(
        &hash_r.join("A.fsx"),
        "#r \"FSharp.Compiler.Service.dll\"\nlet a = 1\n",
    );
    let pinned = root.join("pinned");
    write(
        &pinned.join("global.json"),
        r#"{ "sdk": { "version": "1.0.0", "rollForward": "disable" } }"#,
    );
    write(&pinned.join("A.fs"), "module A\nlet a = 1\n");
    let plain = root.join("plain/A.fs");
    write(&plain, "module A\nlet a = 1\n");

    let probe = root.join("probe/B.fs");
    write(
        &probe,
        "module B\n/// Doc.\nlet z = FSharp.Compiler.Text.Range.Zero\n",
    );
    let probe_request = request(&[&probe]);

    let fresh = fcs_dump_batch_child("xmldoc-batch").request(&probe_request);
    assert!(
        fresh.contains("\"ErrorNumber\":39"),
        "a fresh child must report the probe's type unbound, or the leak is unobservable: {fresh}"
    );

    let earlier: [(&str, PathBuf); 3] = [
        ("an .fsx with a relative #r", hash_r.join("A.fsx")),
        ("a project under a global.json", pinned.join("A.fs")),
        ("a plain project", plain),
    ];
    let mut child = fcs_dump_batch_child("xmldoc-batch");
    let mut failures = Vec::new();
    for (label, path) in &earlier {
        let _ = child.request(&request(&[path]));
        let after = child.request(&probe_request);
        if after != fresh {
            failures.push(format!("after {label}:\n  got   {after}\n  fresh {fresh}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
