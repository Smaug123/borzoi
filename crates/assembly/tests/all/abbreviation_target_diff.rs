//! Differential oracle for the type-abbreviation *target*.
//!
//! `tools/fcs-dump` renders each `IsFSharpAbbreviation` entity's **immediate,
//! unchased, logical** target (`type IntId = int` ⇒ `Microsoft.FSharp.Core.int`,
//! never chased to `System.Int32`), which is exactly the shape the host
//! signature pickle's `type_abbrev` stores and the Rust decoder will mirror. See
//! `docs/abbreviation-target-projection-plan.md` §3.3.
//!
//! This slice (plan Stage 1) lands the oracle infrastructure and pins FCS's
//! rendering directly. The Rust decoder does not exist yet, so the
//! *certain-implies-exact* two-sided comparison — for every target **we** decode
//! `Some`, assert FCS agrees exactly; where we decline, assert nothing — arrives
//! with the decoder in Stage 2. Here the FCS side is pinned on its own, so the
//! canonical strings the decoder must reproduce are fixed before it is written.
//!
//! The abbreviation entities themselves are already covered by the whole-tree
//! `diff_assembly_minilib_fs` diff (both sides synthesise the identical name-only
//! entity), which the `IntId`/`S` fixtures now exercise; the target is elided
//! there and read through [`fcs_abbreviation_targets`] instead.

use borzoi_assembly::test_support::fcs_abbreviation_targets;

use crate::common::{ensure_minilib_fs_built, invoke_fcs_dump};

/// `fcs-dump entities` must render MiniLibFs's two nullary abbreviations by their
/// immediate logical target — the exact canonical strings the Stage 2 decoder is
/// built to reproduce.
///
/// - `type IntId = int` targets the FSharp.Core primitive alias, so the
///   immediate logical target is `Microsoft.FSharp.Core.int` — **not** chased to
///   `System.Int32` (the single-assembly reader cannot chase cross-assembly, and
///   the pickle stores the immediate form).
/// - `type S = System.String` targets a BCL type directly, which FCS surfaces by
///   its `AccessPath`+`LogicalName` FQN `System.String` (no alias to chase).
///
/// A green assertion also proves abbreviation entities no longer crash the
/// `entities` dump (the minimal-projection branch added in Stage 1's fcs-dump
/// half): the dump parses and both targets are present.
///
/// Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
#[test]
fn fcs_dump_renders_immediate_logical_targets_for_minilib_fs() {
    let dll_path = ensure_minilib_fs_built();
    let fcs_json = invoke_fcs_dump("entities", dll_path);
    let targets = fcs_abbreviation_targets(&fcs_json);

    assert_eq!(
        targets.get("MiniLibFs.IntId"),
        Some(&Some("Microsoft.FSharp.Core.int".to_string())),
        "`type IntId = int` must render the immediate FSharp.Core logical alias, \
         never the chased `System.Int32`. All abbreviation targets: {targets:#?}",
    );
    assert_eq!(
        targets.get("MiniLibFs.S"),
        Some(&Some("System.String".to_string())),
        "`type S = System.String` targets a BCL type directly, rendered by its \
         AccessPath+LogicalName FQN. All abbreviation targets: {targets:#?}",
    );
}
