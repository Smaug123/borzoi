//! Smoke test for the `tools/fcs-dump` F# binary.
//!
//! Shells out to `dotnet run --project tools/fcs-dump` against a tiny F#
//! source file, then parses the JSON envelope. Verifies the F# bridge is
//! reachable, builds, and emits a stable shape for the upcoming parser
//! differential tests to compare against.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
//!
//! Set `BORZOI_FCS_DUMP=/path/to/fcs-dump` to point at a pre-built
//! self-contained binary instead of invoking `dotnet run` (much faster on
//! repeated invocations). The first invocation in a fresh checkout will
//! restore NuGet packages — that can take a couple of minutes.

use std::io::Write;
use std::path::Path;

use serde_json::Value;
use tempfile::NamedTempFile;

use crate::common::{invoke_fcs_dump, project_dir};

#[test]
fn fcs_dump_emits_parse_tree_for_a_trivial_source() {
    let mut src = NamedTempFile::with_suffix(".fs").expect("create temp .fs file");
    writeln!(src, "let x = 1 + 2").expect("write temp source");
    let path = src.path();

    let json = run_fcs_dump(path);
    let v: Value = serde_json::from_str(&json).expect("fcs-dump output is JSON");

    let obj = v.as_object().expect("top-level is an object");
    assert!(
        obj.contains_key("ParseTree"),
        "missing ParseTree in dump: {obj:?}"
    );
    assert!(
        obj.contains_key("Diagnostics"),
        "missing Diagnostics in dump"
    );
    assert_eq!(
        obj.get("ParseHadErrors"),
        Some(&Value::Bool(false)),
        "trivial source should parse without errors: {v:#}"
    );

    let parse_tree = &obj["ParseTree"];
    assert_eq!(
        parse_tree["Case"], "ImplFile",
        "expected ImplFile (not signature): {parse_tree:#}"
    );
}

/// Regression for codex review #1: System.Text.Json's default `MaxDepth =
/// 64` aborts mid-serialise on most real F# files. Dumping the bridge tool's
/// own `Program.fs` produces an AST deeper than 64 — if MaxDepth ever drops
/// back to the default, this test will fail with `JsonException: A possible
/// object cycle was detected` long before the trivial test does.
#[test]
fn fcs_dump_handles_a_non_trivial_source() {
    let project = project_dir();
    let path = project.join("Program.fs");
    assert!(path.is_file(), "expected {} to exist", path.display());

    let json = run_fcs_dump(&path);
    let v: Value = serde_json::from_str(&json).expect("fcs-dump output is JSON");

    assert_eq!(v["ParseHadErrors"], Value::Bool(false), "{v:#}");
    assert_eq!(v["ParseTree"]["Case"], "ImplFile");
}

/// Regression for codex review #2: FCS's `PreXmlDoc(pos, XmlDocCollector)`
/// case wraps a collector with private fields, so auto-serialisation
/// silently drops XML doc content. The dedicated converter must materialise
/// the lines via `.ToXmlDoc(false, None)`. Asserts the dump JSON literally
/// contains the doc text.
#[test]
fn fcs_dump_preserves_xml_doc_content() {
    let mut src = NamedTempFile::with_suffix(".fs").expect("create temp .fs file");
    writeln!(src, "/// A documented function.").expect("write");
    writeln!(src, "/// Returns one.").expect("write");
    writeln!(src, "let one () = 1").expect("write");

    let json = run_fcs_dump(src.path());
    let v: Value = serde_json::from_str(&json).expect("fcs-dump output is JSON");

    // PreXmlDoc lines should appear somewhere in the tree as an array of
    // strings under a "Lines" key. Walk the tree until we find them.
    let lines = find_xml_doc_lines(&v).unwrap_or_else(|| panic!("no XML doc lines in dump: {v:#}"));
    assert_eq!(
        lines,
        vec![" A documented function.", " Returns one."],
        "XML doc content lost"
    );
}

fn find_xml_doc_lines(v: &Value) -> Option<Vec<String>> {
    if let Some(obj) = v.as_object()
        && let Some(lines) = obj.get("Lines").and_then(|l| l.as_array())
        && obj.contains_key("IsEmpty")
        && !lines.is_empty()
    {
        return Some(
            lines
                .iter()
                .filter_map(|l| l.as_str())
                .map(String::from)
                .collect(),
        );
    }
    match v {
        Value::Object(m) => m.values().find_map(find_xml_doc_lines),
        Value::Array(arr) => arr.iter().find_map(find_xml_doc_lines),
        _ => None,
    }
}

fn run_fcs_dump(source: &Path) -> String {
    invoke_fcs_dump("ast", source)
}
