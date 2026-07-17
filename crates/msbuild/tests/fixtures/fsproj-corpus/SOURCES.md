# Vendored fsproj corpus

These are verbatim copies of fsproj files from the F# compiler repository,
used by `tests/fsproj_corpus.rs` as snapshot inputs for the
`src/fsproj` parser.

Source: https://github.com/dotnet/fsharp at commit
`a09de4dfd7e3ec402fc3d9f10c16873bb2263531` (2026-05-18).

| Fixture | Upstream path |
| ------- | ------------- |
| `AssemblyCheck/AssemblyCheck.fsproj` | `buildtools/AssemblyCheck/AssemblyCheck.fsproj` |
| `fslex/fslex.fsproj` | `buildtools/fslex/fslex.fsproj` |
| `FSharp.Core/FSharp.Core.fsproj` | `src/FSharp.Core/FSharp.Core.fsproj` |
| `FSharp.Compiler.Service/FSharp.Compiler.Service.fsproj` | `src/Compiler/FSharp.Compiler.Service.fsproj` |

To refresh from a newer upstream checkout, re-copy and then run

```
UPDATE_FSPROJ_SNAPSHOTS=1 cargo test --test fsproj_corpus
```

to regenerate the `.snap` files. Review the diff before committing.
