//! Trait abstracting over the ECMA-335 reader.
//!
//! This was originally the seam for swapping ECMA-335 backends. Today there is
//! one production implementor, [`crate::Ecma335Assembly`], and the trait earns its
//! keep as a *test seam* and generic bound: the LSP's per-DLL degradation
//! tests drive its enumeration path with fake views that error or panic on
//! demand (`crates/lsp/src/semantic.rs`), and `sema`'s
//! `AssemblyEnv::from_views` is generic over it for the same reason. A second
//! production backend remains possible but is not the justification.
//!
//! Methods are deliberately coarse-grained (enumerate types, enumerate
//! resources) so a fake — or a hypothetical other backend — implements them
//! without exposing row IDs.

use crate::ImportError;
use crate::model::{AssemblyIdentity, AssemblyProjectionSkips, Entity};

/// Implemented by [`crate::Ecma335Assembly`] (over the in-crate reader's owned
/// `Image`). Consumers query an assembly entirely through this trait.
pub trait EcmaView {
    /// Identity of the loaded assembly (name, version, public key token).
    fn identity(&self) -> &AssemblyIdentity;

    /// References to other assemblies this one names. Used by the
    /// resolver to follow `TypeRef`s out across the assembly graph.
    fn assembly_refs(&self) -> Vec<AssemblyIdentity>;

    /// All non-synthetic top-level types in this assembly, together with the
    /// assembly-level projection degradations. Per "bound uncertainty", a type
    /// whose shape cannot be decoded is skipped — its `(name, reason)` lands in
    /// [`AssemblyProjectionSkips::dropped_types`] — rather than sinking the
    /// enumeration. Per-*member* drops are recorded on each entity's
    /// [`Entity::skipped_members`] instead. Nested types appear inside their
    /// encloser's `nested_types`, not as top-level entries.
    ///
    /// This is the required core method so that a trait-only consumer (the
    /// LSP's generic enumeration path) can report what degraded; a backend
    /// cannot forget to surface the records.
    fn enumerate_type_defs_with_skips(
        &self,
    ) -> Result<(Vec<Entity>, AssemblyProjectionSkips), ImportError>;

    /// Sugar over [`Self::enumerate_type_defs_with_skips`] for callers that
    /// have decided they don't need assembly-level projection degradation
    /// records (fixture-driven tests, mostly — a fixture with an undecodable
    /// type is a test bug).
    /// Runtime consumers that report diagnostics should call the with-skips
    /// form; this is the *one* place those records are deliberately discarded.
    fn enumerate_type_defs(&self) -> Result<Vec<Entity>, ImportError> {
        Ok(self.enumerate_type_defs_with_skips()?.0)
    }

    /// The dotted paths named by the assembly-level
    /// `[<assembly: AutoOpen("path")>]` custom attributes on the manifest, in
    /// manifest order — the analogue of FCS's `GetAutoOpenAttributes`
    /// (`CompilerImports.fs`). These are the namespaces/modules the F#
    /// compiler implicitly opens in every file that references the assembly
    /// (FSharp.Core's list is what makes `printfn` reachable — there is no
    /// hardcoded implicit-open list in FCS). Order matters: FCS applies the
    /// opens in this order, which decides shadowing among them.
    ///
    /// The paths are returned verbatim; a path may name a **namespace** or a
    /// **module** (`Microsoft.FSharp.Core.LanguagePrimitives.IntrinsicOperators`),
    /// and classifying which is the consumer's job. Mirroring FCS's
    /// `TryFindAutoOpenAttr`: only the single-string-argument constructor
    /// contributes a path; the no-argument (TypeDef-level) form and any other
    /// shape contribute nothing (FCS warns and skips — a malformed AutoOpen
    /// must not sink the assembly, it only costs an implicit open, which can
    /// only *reduce* what resolves).
    fn assembly_auto_opens(&self) -> Result<Vec<String>, ImportError>;

    /// All managed resources whose name begins with `FSharp`. Returning
    /// every match (not just those the importer can decode) lets the
    /// projector decide what to do — including raising the
    /// `UnknownFSharpResource` error from D5 for prefixes we haven't
    /// ported.
    ///
    /// Backends must implement this explicitly: a default `Ok(vec![])`
    /// would silently mask the existence of F# data on every backend
    /// that hadn't been ported, which is the opposite of what D5 wants.
    /// A backend that genuinely cannot enumerate resources is welcome
    /// to return `Ok(vec![])` from its own body — the contract is "fail
    /// loud on unknown F# resources", not "must produce non-empty".
    fn fsharp_resources(&self) -> Result<Vec<FSharpResource>, ImportError>;
}

/// A managed resource carrying F# compiler data. Returned by
/// [`EcmaView::fsharp_resources`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FSharpResource {
    /// The full resource name as stored in the manifest, e.g.
    /// `FSharpSignatureCompressedData.MyLib`.
    pub name: String,
    pub kind: ResourceKind,
    /// Already-decompressed payload for the `Compressed*` variants; raw
    /// for the others. Implementors are responsible for the deflate step
    /// so callers can treat both shapes identically.
    pub payload: Vec<u8>,
}

/// Classifies an F# resource by the prefix on its manifest name. The set
/// mirrors `PrettyNaming.fs` in the F# compiler verbatim, per D8 — see
/// `FSharp{Signature,Optimization}{Data,DataB,CompressedData,CompressedDataB}ResourceName`
/// (plus the FSharp.Core-only `FSharp{Signature,Optimization}Info.`) at
/// `dotnet/fsharp/src/Compiler/SyntaxTree/PrettyNaming.fs:1116`-`:1141`.
///
/// The `*B` variants ("secondary" in older F# parlance) carry a sibling
/// stream emitted alongside the primary pickle when the assembly's
/// signature splits into two parts (large signatures, certain language
/// features). The `*Info.` variants are reserved for FSharp.Core itself —
/// the compiler bootstraps from those rather than the public
/// `FSharpSignatureData.` prefix to avoid a loop where reading
/// FSharp.Core needs FSharp.Core's own pickle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// `FSharpSignatureData.<name>` — uncompressed signature pickle.
    SignatureData,
    /// `FSharpSignatureDataB.<name>` — secondary uncompressed signature
    /// stream alongside the primary `SignatureData`.
    SignatureDataB,
    /// `FSharpSignatureCompressedData.<name>` — deflate-compressed pickle
    /// (F# ≥ 4.7).
    SignatureCompressedData,
    /// `FSharpSignatureCompressedDataB.<name>` — deflate-compressed
    /// secondary signature stream.
    SignatureCompressedDataB,
    /// `FSharpSignatureInfo.<name>` — FSharp.Core-only uncompressed
    /// signature pickle (the runtime library's own data uses this prefix
    /// so reading it doesn't loop through the public `SignatureData.`
    /// detection).
    SignatureDataFSharpCore,
    /// `FSharpOptimizationData.<name>` — uncompressed optimisation pickle
    /// (inline bodies, specialisation hints).
    OptimizationData,
    /// `FSharpOptimizationDataB.<name>` — secondary uncompressed
    /// optimisation stream.
    OptimizationDataB,
    /// `FSharpOptimizationCompressedData.<name>` — deflate-compressed
    /// optimisation pickle.
    OptimizationCompressedData,
    /// `FSharpOptimizationCompressedDataB.<name>` — deflate-compressed
    /// secondary optimisation stream.
    OptimizationCompressedDataB,
    /// `FSharpOptimizationInfo.<name>` — FSharp.Core-only uncompressed
    /// optimisation pickle.
    OptimizationDataFSharpCore,
}
