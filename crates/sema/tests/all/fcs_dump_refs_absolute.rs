//! Guard test for the `refs` / `BORZOI_FCS_EXTRA_REFS` channel fcs-dump uses to
//! make a fixture assembly resolvable to FCS (see `invoke_fcs_dump_with_refs`).
//!
//! A relative path there does not fail: FCS's script-reference resolution
//! silently treats it as unresolvable and produces no diagnostic, so the
//! symbol simply never resolves — indistinguishable from "FCS legitimately
//! couldn't resolve this". Every caller in this workspace already builds
//! absolute paths (`CARGO_MANIFEST_DIR`-anchored fixtures, or NuGet/dotnet
//! root-anchored resolved DLLs in `corpus-diff`), so this is a regression
//! guard: fcs-dump must reject a relative ref path loudly rather than
//! silently no-op.

use crate::common::{invoke_fcs_dump_with_refs, temp_fs_file};

#[test]
#[should_panic(expected = "absolute")]
fn a_relative_ref_path_is_rejected_loudly() {
    let path = temp_fs_file("fcs_dump_refs_absolute", "let x = 1\n");
    let _ = invoke_fcs_dump_with_refs(
        "uses",
        &path,
        &[std::path::Path::new("relative/does_not_exist.dll")],
    );
    let _ = std::fs::remove_file(&path);
}
