# Borzoi: an F# LSP

*Slop status: 100% vibe-coded. Written primarily by Claude Opus 4.6-4.8, Fable 5, and GPT-5.5 and 5.6 Sol. Abandon hope, all ye who enter here.*

This is an F# LSP written in Rust.

All my life I have unsuccessfully fought the urge to rewrite the world; I have finally got round to the F# language itself.
The *real*, official and supported, answer to "how do we make a fast F# LSP" is the ongoing work in the F# compiler's own repo to create a proper tree-sitter grammar.

## Status

Not ready for use.

Differential testing against the F# compiler's own source indicates that we correctly parse almost all of the compiler, and we correctly perform enough of MSBuild to know what is compiling where and to list all the package dependencies.
But the type-checker is barely begun, and NuGet restore is only partially implemented.

## Trying it out

If you *do* try it out (I repeat that it is extremely incomplete), consider seeing how it performs in Neovim.
`nix build .#otel` gives you an OpenTelemetry-enabled build in `result/` (set `OTEL_EXPORTER_OTLP_ENDPOINT` or default to `http://localhost:4318`), and then `:luafile .nvim.lua` from within Neovim will turn it on with a few Neovim settings intended to give you a pretty direct experience of how fast and complete the LSP is.
The "hover" action will generally tell you why the LSP doesn't understand a given thing yet.

You should, once, manually `dotnet restore` in the project you're accessing.
`dotnet restore` executes arbitrary code (whyyyy Microsoft whyyyyy), so by default we don't do that for you.
Moreover, NuGet is *astonishingly* complex, and becomes ever more so SDK-by-SDK; most recently they've moved a bunch of the SDK's NuGet restore logic out into runtime C# in MSBuild `Target`s, which means we are getting further and further away from being able to do that ourselves.
Anyway, once NuGet has produced its `obj/project.assets.json` file, we consume it without further restores needed.

Consider setting `BORZOI_LSP_CACHE_DIR="$HOME/.cache/borzoi"`.
If set, this will speed several things up substantially, most notably the parsing of assemblies your project depends on.

## Speed

The primary design goal is speed, which is why I'm not using .NET and the F# Compiler Service.

### Parser

I performed an initial test, before any performance optimisation, parsing the entire `dotnet/fsharp` repo (that is, converting every `.fs`, `.fsi`, and `.fsx` file into an untyped syntax tree), a corpus of 6344 valid files, totalling 42.3MB.
One parser is Borzoi's, one is build from FCS, both in release mode.
I timed the parse loop only, reading files into memory and building FCS's parse options once up front before timing; I disabled FCS's parse cache.

From a cold start (fresh process): Borzoi takes about 2s (of which 1.7s is parsing).
FCS takes about 8s (this includes warming up the JIT): Borzoi hasn't quite finished parsing the world before the .NET runtime and `FSharpChecker` have been instantiated, but it's not that far off.

From a warm start (the .NET JIT is warm): Borzoi takes 1.7s extremely consistently, while FCS takes 5.2s.

That is, without any particular attention paid to optimisation, Borzoi parses from scratch in about 1/3 the time FCS does (1/4 to 1/5th from a cold start), while doing more work (it preserves trivia while FCS does not).

Borzoi is *not* anywhere near fast enough that a from-scratch cold re-parse beats FCS's built-in parse cache for a file that FCS has already seen and cached.

## Correctness

### Parser

We have a differential harness for comparison against FSharp.Compiler.Service.
It lives in `crates/cst/tests/all`: it sweeps an entire corpus (`parser_corpus_diff.rs`) verifying that Borzoi's `rowan` AST and FCS's `ParsedInput` project to the same common representation expressed in our `normalised_ast/model.rs`.

There are some known divergences in certain extremely rare F# constructs; these are tracked by a report you can generate with `fcs_divergence.rs`.
The divergences are bugs I wish to fix.

## License

Licensed under either of

 * Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

This project is a derivative work of the [F# compiler](https://github.com/dotnet/fsharp),
which is used under the MIT license; that upstream copyright notice is reproduced
in [LICENCE_fsharp.md](LICENCE_fsharp.md).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
