pub mod assembly_cache;
pub mod csharp_sidecar;
pub mod cst_panic_safe;
pub mod diagnostics;
pub mod fsproj_diagnostics;
mod glob;
pub mod glob_resolver;
pub mod goto_source;
pub mod handlers;
pub mod logging;
pub mod paths;
pub mod position;
pub mod project_assets;
pub mod project_graph;
pub mod publish;
pub mod pull;
pub mod restore;
pub mod sdk_discovery;
pub mod semantic;
pub mod server;
pub mod sidecar_manager;
pub mod spawn;
pub mod telemetry;
pub mod workspace;

/// The `$(Configuration)` the LSP serves everything under
/// (`docs/fsproj-tfm-selection-plan.md` E4). One policy value, four
/// consumers that must agree — the evaluator's global seed
/// ([`workspace`]), the C# sidecar's build configuration and the
/// Debug-first preference when locating F# project-reference output DLLs
/// ([`semantic`]), and the `.fsproj`-buffer diagnostics seed
/// ([`fsproj_diagnostics`]) — or a project's defines and its referenced
/// assemblies drift apart. Hard-coded to `Debug`, the editor-flow default
/// (FCS's `FSharpProjectOptions` does the same); an LSP initialisation
/// option may surface it later.
pub const BUILD_CONFIGURATION: &str = "Debug";
