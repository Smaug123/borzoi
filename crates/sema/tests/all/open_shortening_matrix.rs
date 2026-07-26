//! The **open-shortening matrix**: the dimension the other fold grids leave
//! out — the `open` path *itself* needing to be shortened, through an
//! `[<AutoOpen>]` module that some earlier (or implicit) open brought into
//! scope. (Mechanics live in [`crate::common::fold_matrix`]; this module owns
//! the grid.)
//!
//! ## What it covers
//!
//! FCS's `AddModuleOrNamespaceRefsToNameEnv` enters **every** nested module of
//! an opened container into `eModulesAndNamespaces` under its short name, and
//! then recurses into the `[<AutoOpen>]` ones — so opening a container also
//! makes its auto-open modules' *submodules* openable by their short names.
//! That is why `open Checked` works in a file that opens nothing: FSharp.Core's
//! `Microsoft.FSharp.Core` is implicitly opened, its `[<AutoOpen>]` module
//! `Operators` is folded, and `Operators.Checked` therefore answers to the bare
//! name.
//!
//! The failure this grid guards is a **wrong target**, not a missed one: every
//! positive cell's probe also exists in the enclosing auto-open module, so a
//! shortening we fail to find leaves the *enclosing* value standing rather than
//! producing nothing. That is exactly how the unchecked `Operators.int64` used
//! to win over `Operators.Checked.int64` in four `WoofWare.PawPrint` files.
//!
//! Channels: an explicit namespace prefix, a module prefix, and a transitive
//! two-level chain. Two contests pin the *ranking*, which is where a flattened
//! prefix list goes wrong quietly: an auto-open-derived prefix versus the opened
//! container's own direct submodule, and two assemblies' auto-open modules in
//! one namespace. Negative controls pin that the recursion runs through
//! `[<AutoOpen>]` and public accessibility only.
//!
//! **Project** auto-open modules lend no shortening prefix, so there are no
//! project cells: FCS auto-opens one *fragment*, not the merged module, and
//! deciding which nested modules belong to the attributed fragment needs the
//! declaring file of each — see `Resolver::auto_open_shortening_prefixes`.
//!
//! The **implicit** prefix — the `open Checked` shape itself — cannot be cast
//! as a cell here: FCS applies an assembly-level `[<AutoOpen>]` inside the
//! declaring assembly only, so the fixture's own `[<AutoOpen>]` module in
//! `Microsoft.FSharp.Core` is not implicitly opened at all (fcs-dump-verified:
//! bare `plainCore` is FS0039, `open Microsoft.FSharp.Core` then `plainCore`
//! resolves). That channel is pinned against the real article instead, by
//! `resolve_fsharp_core::open_checked_binds_the_checked_conversions`.

use crate::common::fold_matrix::{Cell, Position, run_matrix};

const CELLS: &[Cell] = &[
    // ---- explicit namespace prefix ----
    Cell {
        decls: &[],
        label: "explicit-ns / control: the enclosing auto-open value",
        body: &["open Demo.Auto"],
        probe: "extraShortenTarget",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "explicit-ns / plain submodule of the namespace's auto-open module",
        body: &["open Demo.Auto", "open ExtraShorten"],
        probe: "extraShortenTarget",
        position: Position::Expr,
    },
    // ---- transitive: two auto-open levels down ----
    Cell {
        decls: &[],
        label: "transitive / plain submodule two auto-open levels down",
        body: &["open Demo.Auto", "open DeepShorten"],
        probe: "deepShortenValue",
        position: Position::Expr,
    },
    // ---- precedence: auto-open-derived vs the container's own submodule ----
    Cell {
        decls: &[],
        label: "precedence / auto-open-derived prefix out-ranks the direct submodule",
        body: &["open Demo.Auto", "open ShortenContest"],
        probe: "contestValue",
        position: Position::Expr,
    },
    // ---- module prefix (the opened container is a module, not a namespace) ----
    Cell {
        decls: &[],
        label: "module-prefix / control: the enclosing auto-open submodule's value",
        body: &["open Demo.MOpen.AutoSub"],
        probe: "mShortenTarget",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "module-prefix / plain submodule of the opened module's auto-open submodule",
        body: &["open Demo.MOpen.AutoSub", "open InnerShorten"],
        probe: "mShortenTarget",
        position: Position::Expr,
    },
    // ---- negative controls ----
    Cell {
        // The closure recurses through `[<AutoOpen>]` modules only: a plain
        // module nested in a plain module answers to no short name.
        decls: &[],
        label: "negative / a plain module inside a plain module is no prefix",
        body: &["open Demo.Auto", "open ClosedDeeper"],
        probe: "closedDeeperValue",
        position: Position::Expr,
    },
    Cell {
        // An `internal` auto-open module is inaccessible cross-assembly, so it
        // contributes neither contents nor a shortening prefix.
        decls: &[],
        label: "negative / an internal auto-open module is no prefix",
        body: &["open Demo.Auto", "open InternalDeeper"],
        probe: "internalDeeperValue",
        position: Position::Expr,
    },
    // ---- two auto-open roots contesting the short name ----
    Cell {
        // Sibling roots in one namespace, contributed by DIFFERENT assemblies:
        // FCS folds `AsmAutoA` then `AsmAutoB` in reference order and the later
        // fold wins. Unlike the same-FQN merge (`Demo.ModuleOpen.Shared`, which
        // defers because two surfaces of one group collide), these are two
        // distinct paths, so the winner is decided by their order in the
        // namespace's auto-open index — which is the order the assemblies were
        // handed to `AssemblyEnv::from_views`, i.e. the same reference order
        // FCS ranks by. This cell is what holds that correspondence.
        decls: &[],
        label: "cross-assembly / two assemblies' auto-open modules nest the same short name",
        body: &["open Demo.TwoAsm", "open AsmPick"],
        probe: "asmPickValue",
        position: Position::Expr,
    },
];

/// Cells where FCS resolves the probe but we do not — each must remain
/// *exactly* "we name nothing while FCS resolves" (see the harness ratchet).
const KNOWN_GAPS: &[(&str, &str)] = &[];

#[test]
fn open_shortening_matches_fcs_on_every_cell() {
    run_matrix(CELLS, KNOWN_GAPS, &[], "open_shortening_matrix");
}
