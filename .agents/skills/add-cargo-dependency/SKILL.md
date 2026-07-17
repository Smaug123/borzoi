---
name: add-cargo-dependency
description: How to add a new crates.io dependency to this repo. The `nix develop` devshell pins cargo offline against a vendored registry, so a naive `cargo add` / build fails with a "crates-io is replaced with non-remote-registry source dir" error. Use whenever adding a dependency to any crate's Cargo.toml.
---

# Adding a crates.io dependency

This repo's `nix develop` devshell pins cargo to an **offline vendored
registry**: it exports `CARGO_NET_OFFLINE=true` plus a `CARGO_HOME` (under
`~/.cache/borzoi/cargo/<vendor-hash>`) whose `config.toml`
source-replaces `[source.crates-io]` with a read-only Nix store directory
(`flake.nix`'s `cargoVendorDir`). A dependency not already in `Cargo.lock`
therefore cannot be fetched in the normal devshell — `cargo search` / `cargo add`
/ a build that needs it fail with:

> crates-io is replaced with non-remote-registry source dir /nix/store/…-vendor-cargo-deps…

## Procedure

1. **Edit `Cargo.toml`** of the target crate to add the dependency (pick a
   version; prefer default features unless you need more).

2. **Resolve it online with a *dedicated scratch* `CARGO_HOME`**, leaving the
   devshell's vendored home untouched (reverting is then just "stop using the
   scratch home" — nothing to clean up):

   ```sh
   SCRATCH="$(pwd)/.scratch-cargo-home"   # or anywhere writable outside the repo
   nix develop --command bash -c \
     "export CARGO_HOME='$SCRATCH'; export CARGO_NET_OFFLINE=false; cargo check -p <crate>"
   ```

3. **Use `cargo check` (or build), NOT `cargo generate-lockfile`.** The latter
   re-locks the *entire* graph "to latest compatible versions", churning dozens
   of unrelated crates. `cargo check` against the existing `Cargo.lock` adds
   only the new crate (plus any genuinely-new transitive deps), giving a minimal
   diff. **Verify:**

   ```sh
   git diff --no-ext-diff Cargo.lock     # should show only the new crate(s)
   ```

   If it churned, `git checkout -- Cargo.lock` and redo with `cargo check`.

4. **No manual vendoring step.** The flake re-vendors from `Cargo.lock`
   automatically on the next `nix develop` (the vendor dir is keyed by the
   lockfile), and CI builds the same closure. Just commit the `Cargo.toml` +
   `Cargo.lock` change.

## While developing

Reuse the same scratch `CARGO_HOME` + `CARGO_NET_OFFLINE=false` for every later
cargo command in the work session (`test`, `clippy`, `doc`). After the first
fetch the cache is warm, so they behave as offline-cache hits — but staying
online lets cargo fall back to crates.io if the scratch cache is missing
something. Do **not** overwrite the devshell's real vendored `CARGO_HOME`; keep
it pristine so the offline workflow keeps working for everyone else.
