// Fixture for the PDB arm of the fail-loud robustness harness
// (`tests/all/fail_loud.rs`). Deliberately tiny — the harness mutates every
// byte of the produced DLL and of its extracted PDB blob, so size is
// cost — but with enough real code that the PDB carries documents,
// sequence points, and (via EmbedAllSources) an embedded-source blob.

namespace MiniLibPdb

module Sample =
    let add (x: int) (y: int) = x + y

    let describe (n: int) : string =
        if n > 0 then "positive"
        elif n < 0 then "negative"
        else "zero"
