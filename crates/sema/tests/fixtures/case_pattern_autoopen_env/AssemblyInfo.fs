module Cases.Retained.AssemblyInfo

// The manifest auto-open: fsc records this on the assembly, and every consumer
// referencing the DLL gets `Cases.Retained.Auto`'s contents in bare scope.
[<assembly: AutoOpen("Cases.Retained.Auto")>]
do ()
