// A MODULE at the exact name the main fixture exports as a UNION
// (`Cases.Union.Target`). A module is transparent to a constructor-namespace
// head lookup, so with only this DLL loaded nothing binds `Target.Carrier`;
// with both loaded, FCS binds whichever the reference order puts last. See the
// `.fsproj` comment for why the two share a manifest identity.
namespace Cases.Union

module Target =
    let helper () = 1
