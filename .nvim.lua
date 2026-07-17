-- Project-local Neovim setup for borzoi.
--
-- Usage:
--   1. Build the server: `nix build` (produces ./result/bin/borzoi).
--   2. Open an .fs/.fsi/.fsx file with `nvim` from this directory.
--
-- Neovim auto-loads this file when 'exrc' is enabled in your global config
-- and you've trusted it (`:trust` on the prompt). If you don't want 'exrc'
-- on globally, run `:luafile .nvim.lua` after launching nvim here.

local root = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h")
local bin = root .. "/result/bin/borzoi"

vim.filetype.add({
  extension = {
    fs = "fsharp",
    fsi = "fsharp",
    fsx = "fsharp",
  },
})

local function start_lsp(buf)
  if vim.fn.executable(bin) == 0 then
    vim.notify(
      "borzoi: binary not found at " .. bin .. " (run `nix build`)",
      vim.log.levels.WARN
    )
    return
  end
  vim.lsp.start({
    name = "borzoi",
    cmd = { bin },
    root_dir = root,
    -- Neovim debounces the `didChange` notification by `debounce_text_changes`
    -- ms (default 150) before the server even sees an edit, and the semantic-
    -- token refresh only fires after that lands. borzoi folds a keystroke in
    -- ~40-75ms, so most of the *felt* highlighting lag is this client-side
    -- debounce, not the server. Shrink it; the server handles the extra folds
    -- comfortably. Tune to taste (0 disables debouncing entirely).
    flags = { debounce_text_changes = 20 },
  }, { bufnr = buf })
end

-- Make borzoi the *sole* highlighter for F# buffers. Vim's built-in
-- regex syntax and tree-sitter are not LSP clients, so the stop_others loop
-- below doesn't touch them; and setting filetype=fsharp is exactly what turns
-- them on. Both colour identifiers by spelling (e.g. a DU case named `String`
-- gets painted as the built-in type), which the LSP deliberately won't do — its
-- semantic tokens cover only the lexically-unambiguous categories (keywords,
-- comments, strings, numbers, operators) and leave every identifier to fall
-- through. Turning the grammars off trades a sparser buffer for a consistent
-- one with no spelling-based mis-colouring.
local function disable_builtin_highlight(buf)
  vim.bo[buf].syntax = "OFF"
  pcall(vim.treesitter.stop, buf)
end

vim.api.nvim_create_autocmd("FileType", {
  pattern = "fsharp",
  callback = function(args)
    start_lsp(args.buf)
    -- Defer so we run after any other plugin's FileType handler has attached
    -- tree-sitter or set 'syntax' (Neovim's own syntaxset augroup sets
    -- syntax=<ft> on FileType too); otherwise a later handler re-enables what
    -- we just turned off.
    vim.schedule(function() disable_builtin_highlight(args.buf) end)
  end,
})

-- Stop any other LSP server (e.g. fsautocomplete, started by a global
-- lspconfig setup) on F# buffers so only borzoi is active here.
-- Fully stops the client rather than just detaching it from the buffer:
-- a detached-but-running client gets re-attached by lspconfig.
local get_clients = vim.lsp.get_clients or vim.lsp.get_active_clients

local function stop_others(buf)
  for _, client in pairs(get_clients({ bufnr = buf })) do
    if client.name ~= "borzoi" then
      vim.lsp.stop_client(client.id, true)
    end
  end
end

vim.api.nvim_create_autocmd("LspAttach", {
  callback = function(args)
    if vim.bo[args.buf].filetype == "fsharp" then
      -- Defer so we don't stop a client mid-attach.
      vim.schedule(function() stop_others(args.buf) end)
      -- The real highlighting lag is Neovim's *semantic-token* engine, not
      -- `debounce_text_changes`: it debounces token requests 200ms after you
      -- stop typing (runtime/lua/vim/lsp/semantic_tokens.lua — STHighlighter
      -- .debounce, default 200, reset on every buffer change). borzoi folds a
      -- keystroke in ~55ms, so that 200ms client-side wait dominates the felt
      -- de-highlight delay. Drop it on the auto-started highlighter.
      --
      -- Safe to make aggressive: `Client:request` flushes pending didChange
      -- before every request (client.lua — "so that the server doesn't operate
      -- on a stale state"), so the token request always resolves fresh text; a
      -- short debounce can never highlight a stale buffer. And because every
      -- keystroke resets the timer, continuous typing sends *no* requests — one
      -- fires ~20ms after you pause. `enable()` exposes no debounce knob, so we
      -- reach the (test-exported) highlighter directly; deferred so the LSP
      -- capability auto-start has created it.
      vim.defer_fn(function()
        local ok, sth = pcall(function()
          return vim.lsp.semantic_tokens.__STHighlighter
        end)
        local hl = ok and sth and sth.active and sth.active[args.buf]
        if hl then
          hl.debounce = 20
        end
      end, 100)
    end
  end,
})

-- Attach to F# buffers that were already loaded before this file was sourced
-- (e.g. via `:luafile`, or if exrc fires after BufRead). FileType won't re-fire
-- on a buffer whose filetype is already `fsharp`, so call start_lsp directly.
-- Also sweep any servers that attached before our LspAttach autocmd existed.
for _, buf in ipairs(vim.api.nvim_list_bufs()) do
  if vim.api.nvim_buf_is_loaded(buf) and vim.bo[buf].filetype == "fsharp" then
    stop_others(buf)
    start_lsp(buf)
    disable_builtin_highlight(buf)
  end
end
