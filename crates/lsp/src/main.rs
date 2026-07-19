use std::error::Error;

use borzoi::log_info;
use borzoi::server::{
    State, client_capabilities_from_initialize, server_capabilities, workspace_roots_from_init,
};
use lsp_server::Connection;
use lsp_types::InitializeParams;

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    // Held until the end of `main`; on drop it flushes buffered spans. A no-op
    // unless built with `--features otel`.
    let _telemetry = borzoi::telemetry::init();

    log_info!("starting on stdio");

    let (connection, io_threads) = Connection::stdio();

    let capabilities = serde_json::to_value(server_capabilities())?;

    let init_params = match connection.initialize(capabilities) {
        Ok(params) => params,
        Err(err) => {
            io_threads.join()?;
            return Err(err.into());
        }
    };
    let client_capabilities = client_capabilities_from_initialize(&init_params)?;
    let init_params: InitializeParams = serde_json::from_value(init_params)?;

    let mut state = State::new();
    // Opt the running server into the on-disk assembly-projection cache (a warm
    // restart then skips the parse+project of every referenced DLL). Disabled in
    // `State::new`, so tests and library embeddings stay off-disk; governed by
    // `BORZOI_LSP_CACHE_DIR` (empty = off) for read-only deployments.
    state
        .semantic
        .set_assembly_cache(borzoi::assembly_cache::AssemblyCache::from_env());
    // Roots for `workspace/diagnostic`. `rootUri` is deprecated in the protocol
    // but still the only signal some clients send, so we honour it as a
    // fallback.
    let roots = {
        #[allow(deprecated)]
        let root_uri = init_params.root_uri.as_ref();
        workspace_roots_from_init(init_params.workspace_folders.as_deref(), root_uri)
    };
    state.set_workspace_roots(roots);
    // Workspace-trust opt-in: on-demand `dotnet restore` for an assets-absent
    // project executes the project's MSBuild targets (arbitrary code), so it is
    // off unless the host sets `enableOnDemandRestore: true`. Without it, an
    // un-restored project degrades to single-file editing.
    let enable_on_demand_restore = init_params
        .initialization_options
        .as_ref()
        .and_then(|opts| opts.get("enableOnDemandRestore"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    state
        .semantic
        .set_on_demand_restore_enabled(enable_on_demand_restore);
    state.set_client_capabilities(client_capabilities);

    borzoi::server::run(connection, state)?;
    io_threads.join()?;

    log_info!("shut down cleanly");
    Ok(())
}
