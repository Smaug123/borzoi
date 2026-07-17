use crate::common;

#[cfg(unix)]
#[test]
fn corpus_walk_includes_symlinked_sources_without_cycles() {
    use std::fs;
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().expect("tempdir");
    let corpus = tmp.path().join("corpus");
    let external = tmp.path().join("external");
    fs::create_dir_all(&corpus).expect("create corpus");
    fs::create_dir_all(&external).expect("create external");

    let real = corpus.join("Real.fs");
    let linked_file = corpus.join("Linked.fs");
    let linked_dir = corpus.join("linked-dir");
    let external_file = external.join("Script.fsx");
    let cycle = external.join("cycle");

    fs::write(&real, "module Real\n").expect("write real source");
    fs::write(&external_file, "printfn \"script\"\n").expect("write external source");
    symlink(&real, &linked_file).expect("create file symlink");
    symlink(&external, &linked_dir).expect("create directory symlink");
    symlink(&corpus, &cycle).expect("create cycle symlink");

    let files = common::collect_fsharp_corpus_files(&corpus).expect("walk corpus");

    assert!(
        files.contains(&real),
        "real source was not collected: {files:?}"
    );
    assert!(
        files.contains(&linked_file),
        "symlinked source file was not collected: {files:?}"
    );
    assert!(
        files.contains(&linked_dir.join("Script.fsx")),
        "source under a symlinked directory was not collected: {files:?}"
    );
    assert_eq!(
        files.len(),
        3,
        "cycle through a symlinked directory should not duplicate traversal: {files:?}"
    );
}

#[test]
fn corpus_walk_reports_read_dir_errors() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let not_dir = tmp.path().join("not-a-dir");
    std::fs::write(&not_dir, "not a directory").expect("write file");

    let err = common::collect_fsharp_corpus_files(&not_dir).expect_err("walk should fail");

    assert!(
        err.to_string().contains("read directory"),
        "unexpected walk error: {err}"
    );
}

#[test]
fn corpus_source_read_errors_are_loud() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("missing.fs");
    let err = common::read_corpus_source(&missing).expect_err("read should fail");

    assert!(
        err.to_string().contains(&missing.display().to_string()),
        "read error should include the path: {err}"
    );
    assert!(
        !err.is_non_utf8(),
        "missing files should be I/O errors, not non-UTF-8 skips"
    );
}

#[test]
fn corpus_source_non_utf8_errors_are_classified() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("codepage.fs");
    std::fs::write(&path, [0xff]).expect("write invalid UTF-8");

    let err = common::read_corpus_source(&path).expect_err("decode should fail");

    assert!(err.is_non_utf8(), "expected non-UTF-8 error, got {err}");
}
