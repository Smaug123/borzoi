//! FS0892 (`tcModuleRequiresQualifiedAccess`): an `open` that names a module
//! marked `[<RequireQualifiedAccess>]`.
//!
//! FCS resolves the opened path to a *set* of modules and namespaces — every
//! entity in scope under the head, walked through the rest — and reports the
//! error once for each module in that set carrying FSharp.Core's attribute
//! (`TcOpenModuleOrNamespaceDecl`), at the range of the path, and then opens
//! them all anyway. So the diagnostic needs only one certain member of the
//! set, not the whole set: we report it when some route FCS takes
//! ([`CertainReach`](super::state::CertainReach)) reaches a module that
//! certainly carries the attribute, and stay silent otherwise.
//!
//! What "certainly carries the attribute" means differs by where the module
//! lives:
//!
//! - a **project** module ([`Resolver::module_certainly_require_qualified_access`])
//!   is decided from its written attribute, which is FSharp.Core's only while
//!   nothing can rename or impersonate it
//!   ([`Resolver::attribute_spellings_trusted`]) and FSharp.Core declares it;
//!   it must also be accessible from every `open` in the project, so a
//!   `private` module or one nested in a `private` module is never recorded;
//! - a **referenced** module counts only when its assembly's F# signature is
//!   authoritative ([`AssemblyEnv::is_authoritative_module`](crate::AssemblyEnv::is_authoritative_module)):
//!   FCS reads a module through that pickle, which also carries the attribute
//!   verdict ([`AssemblyEnv::is_require_qualified_access`](crate::AssemblyEnv::is_require_qualified_access)),
//!   and imports a module of any other assembly as a plain type.
//!
//! Silent wherever the file failed to parse (FCS reports no type-checking
//! error there) and inside a `rec` scope, whose opens FCS processes ahead of
//! the declarations they follow.

use borzoi_cst::syntax::{AttributeList, SyntaxToken};
use rowan::TextRange;

use super::id_text;
use super::state::{Resolver, TierCandidate};
use crate::diagnostics::{SemaDiagnostic, SemaDiagnosticKind};

/// The namespace FSharp.Core declares `RequireQualifiedAccessAttribute` in.
const CORE_NAMESPACE: [&str; 3] = ["Microsoft", "FSharp", "Core"];

impl Resolver<'_> {
    /// Whether a module declared now, with `attrs`, certainly carries
    /// FSharp.Core's `[<RequireQualifiedAccess>]` and can be opened from
    /// anywhere in the project. `private` is whether the module or any module
    /// enclosing it is `private`: FCS's reading of such a module's
    /// accessibility at an `open` is not modelled (a `private` module in a
    /// namespace is openable from another file of that namespace, FCS-graded).
    ///
    /// The attribute must be written with nothing that could make FCS read it
    /// as anything else: exactly `RequireQualifiedAccess`, whose two lookup
    /// names (`RequireQualifiedAccessAttribute`, then itself) are both names
    /// the trust rule guards — the long spelling is looked up first as
    /// `RequireQualifiedAccessAttributeAttribute`, which nothing guards; a
    /// single segment (`Foo.RequireQualifiedAccess` may name no type at all);
    /// no target (`[<assembly: …>]` decorates something else); and no argument
    /// (no constructor takes one).
    pub(super) fn module_certainly_require_qualified_access(
        &self,
        attrs: impl Iterator<Item = AttributeList>,
        private: bool,
    ) -> bool {
        if private || self.recursive_module_active || !self.recovery.proves_clean_parse() {
            return false;
        }
        let spellings: Vec<String> = attrs
            .flat_map(|list| list.attributes().collect::<Vec<_>>())
            .filter(|attr| attr.target().is_none() && attr.arg().is_none())
            .filter_map(|attr| {
                let name = attr.type_name()?;
                let mut idents = name.idents();
                let only = idents.next()?;
                idents.next().is_none().then(|| id_text(&only).to_string())
            })
            .filter(|s| s == "RequireQualifiedAccess")
            .collect();
        let core: Vec<String> = CORE_NAMESPACE.iter().map(|s| s.to_string()).collect();
        !spellings.is_empty()
            && self
                .assemblies
                .lookup_type(&core, "RequireQualifiedAccessAttribute", 0)
                .is_some()
            && self.attribute_spellings_trusted(spellings.iter().map(String::as_str))
    }

    /// Whether `path`, or a path it runs through, names more than one project
    /// container, so that the path cannot say which an `open` reaches: a
    /// module declared there twice (here twice, here and in an earlier file,
    /// or in two earlier files — FCS reports the duplicate), or a module and a
    /// namespace both (`namespace N.A` beside `module A` in `namespace N`: an
    /// `open M` inside the module does not reach the namespace's `M`,
    /// FCS-graded).
    fn module_path_is_ambiguous(&self, path: &[String]) -> bool {
        (1..=path.len()).any(|k| {
            let p = &path[..k];
            let module = self.declared_module_paths.contains(p)
                || self.preceding.is_exact_project_module(p)
                || self.preceding.is_real_nested_module(p);
            self.duplicated_module_paths.contains(p)
                || self.preceding.is_duplicated_module(p)
                || (module && self.is_project_namespace_path(p))
        })
    }

    /// Record a module this file declares at `path` (a header's or a nested
    /// module's full path), noting it as duplicated when this file or an
    /// earlier one already declared one there.
    pub(super) fn note_module_declared(&mut self, path: &[String]) {
        let earlier = self.preceding.is_exact_project_module(path)
            || self.preceding.is_real_nested_module(path);
        if !self.declared_module_paths.insert(path.to_vec()) || earlier {
            self.duplicated_module_paths.insert(path.to_vec());
        }
    }

    /// Report FS0892 on a plain `open` whose path is written as `idents`, when
    /// one of its tier `candidates` certainly names a module carrying the
    /// attribute. The range is the written path's, `global.` included, as
    /// FCS's is.
    pub(super) fn report_open_of_rqa_module(
        &mut self,
        idents: &[SyntaxToken],
        candidates: &[TierCandidate],
    ) {
        if self.recursive_module_active || !self.recovery.proves_clean_parse() {
            return;
        }
        let (Some(first), Some(last)) = (idents.first(), idents.last()) else {
            return;
        };
        let names_rqa_module = candidates.iter().any(|c| {
            !self.module_path_is_ambiguous(&c.full)
                && ((c.reach.this_file && self.rqa_module_paths.contains(&c.full))
                    || (c.reach.earlier_files && self.preceding.is_certain_rqa_module(&c.full))
                    || self.opened_assembly_modules(&c.full).into_iter().any(|h| {
                        let reached = c.reach.assembly
                            || c.implicit_namespace
                                .as_ref()
                                .is_some_and(|ns| self.assemblies.implicitly_opened_in(ns, h));
                        reached
                            && self.assemblies.is_authoritative_module(h)
                            && self.assemblies.is_require_qualified_access(h)
                    }))
        });
        if names_rqa_module {
            self.diagnostics.push(SemaDiagnostic {
                range: TextRange::new(first.text_range().start(), last.text_range().end()),
                kind: SemaDiagnosticKind::OpenOfRequireQualifiedAccessModule,
            });
        }
    }
}
