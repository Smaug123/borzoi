namespace SigHiddenUnion

/// A type-equality witness whose representation is HIDDEN by this signature:
/// consumers see only an opaque `type Teq<'a, 'b>`, with no accessible union
/// cases. The implementation (`Teq.fs`) is a single-case union, but the F#
/// compiler lowers the union representation to `TNoRepr` in the signature data
/// (`SignatureConformance`), so the pickle a cross-assembly consumer reads
/// carries no union repr — while the compiled class still bears
/// `CompilationMapping(SumType)`. This is the `TypeEquality.Teq` shape that
/// made `open`ing the namespace defer every dotted head before the projector
/// learned to seal a signature-hidden union to zero accessible cases.
type Teq<'a, 'b>
