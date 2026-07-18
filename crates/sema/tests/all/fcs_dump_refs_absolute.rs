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

use std::panic::{AssertUnwindSafe, catch_unwind};

use borzoi_oracle_harness::panic_silence::silence_panics_here;

use crate::common::{invoke_fcs_dump_with_refs, temp_fs_file};

#[test]
fn a_relative_ref_path_is_rejected_loudly() {
    let path = temp_fs_file("fcs_dump_refs_absolute", "let x = 1\n");

    let result = {
        // The panic is the expected outcome, not a test-harness failure — don't
        // let it print to stderr.
        let _silence = silence_panics_here();
        catch_unwind(AssertUnwindSafe(|| {
            invoke_fcs_dump_with_refs(
                "uses",
                &path,
                &[std::path::Path::new("relative/does_not_exist.dll")],
            )
        }))
    };
    // Clean up regardless of outcome: the expected (panicking) path must not
    // leak the temp source file on every green run.
    std::fs::remove_file(&path).expect("remove temp .fs");

    let payload = result.expect_err("a relative ref path must be rejected loudly");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default();
    assert!(
        message.contains("absolute"),
        "panic message should name the absolute-path requirement, got {message:?}"
    );
}
