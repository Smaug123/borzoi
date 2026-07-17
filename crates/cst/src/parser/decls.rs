//! Module-structure productions: file / namespace / module headers, nested-
//! module bodies, and the shared module-decl body loop. Type definitions live
//! in [`super::decls_type`] / [`super::decls_repr`] / [`super::decls_member`],
//! signature declarations in [`super::decls_sig`], and `let` / attribute
//! productions in [`super::decls_binding`].

use super::*;

/// Which scope the shared [`Parser::parse_module_decls`] loop is running in.
/// Determines how a body-level `OBLOCKEND` virtual is treated: at
/// [`File`](BodyScope::File) scope the loop runs to EOF and stray
/// `OBLOCKEND`s are inter-decl scaffolding; at [`Nested`](BodyScope::Nested)
/// scope (a `module X = <block>` body, phase 8.4) the body-closing
/// `OBLOCKEND` terminates the loop and is handed back to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BodyScope {
    /// The file body (the implicit `AnonModule` or a named-module / namespace
    /// body). Runs to `peek() == None`.
    File,
    /// A nested `module X = <block>` body. Terminates at the body-closing
    /// `OBLOCKEND` (left for [`Parser::parse_nested_module_decl`] to consume).
    Nested,
}

impl<'src> Parser<'src> {
    /// Phase 1 entry point. The whole file is wrapped in an `IMPL_FILE` node
    /// holding one or more `MODULE_OR_NAMESPACE`s — FCS's
    /// `ParsedImplFileInput.contents`. A header-less file is a single implicit
    /// `AnonModule`; a whole-file `module Foo` (phase 8.2) is a single
    /// `NamedModule`; one or more `namespace N` blocks (phase 8.3) are one
    /// `DeclaredNamespace` / `GlobalNamespace` each. Each segment's decls are
    /// the body parsed by the shared [`Self::parse_module_decls`] loop, which
    /// hands control back at the next `namespace` header.
    pub(super) fn parse_impl_file(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::IMPL_FILE));

        // File-form mixing: a leading *whole-file* `module M` header (no `=`)
        // followed by a top-level `namespace` is invalid — a file is either
        // module-headed or namespaced, never both. FCS bails the whole file to a
        // single empty `AnonModule` (dropping the module header, its body, *and*
        // the namespace). The test has two halves: the file *starts* with a
        // whole-file module header (`raw_leading_whole_file_module_head`, which
        // also covers the attributed `[<A>]⏎module M` form), and a top-level
        // `namespace` *actually follows* — measured by the loop producing a 2nd
        // segment (`segment_count`), so a stray `namespace` token in an expression
        // / interpolation fill never counts. When both hold, the segment loop's
        // output is wrapped (below) in one outer `MODULE_OR_NAMESPACE`: the inner
        // segments nest inside it, so `modules()` / `kind()` / `decls()` see only
        // the empty outer (projecting as the empty `AnonModule`), every token
        // staying lossless. (`module M = X⏎namespace N` has a trailing `=` — a
        // nested-decl prefix — so it stays the FS0222 path; `module M` alone
        // produces a single segment and stays a valid `NamedModule`.)
        let leading_module_head = self.raw_leading_whole_file_module_head();
        let file_cp = self.builder.checkpoint();
        let file_span = self
            .peek()
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| self.source.len()..self.source.len());
        let mut segment_count = 0usize;

        // Phase 8.3 — a file is a sequence of `SynModuleOrNamespace`s: one
        // implicit `AnonModule` / whole-file `NamedModule`, or *one or more*
        // `namespace` blocks. Each iteration emits one `MODULE_OR_NAMESPACE`;
        // the body loop hands control back at the next `namespace` header
        // (LexFilter segments adjacent namespaces with an `OBLOCKSEP`, and
        // `namespace` — unlike the swallowed `module` — is a real filtered
        // token). The first iteration always runs (even an empty file is one
        // empty `AnonModule`).
        loop {
            segment_count += 1;
            // Open the segment under a checkpoint so its node kind can be chosen
            // *after* the body is parsed: a normal segment is a
            // `MODULE_OR_NAMESPACE`, but a non-empty anonymous prefix before a
            // `namespace` is the FS0222 illegal case wrapped in `ERROR` (below).
            let cp = self.builder.checkpoint();
            // The segment's first-token span — the FS0222 diagnostic anchor.
            let prefix_span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());

            // An optional `module Foo` / `namespace N` header (phase 8.2),
            // emitted as direct children ahead of the body. A no-op
            // (AnonModule) when no header is present; returns whether one was
            // parsed so the body loop won't claim a *second* whole-file header
            // (10.7e).
            let header_present = self.parse_optional_file_header();

            // Body decls. At file scope the shared loop runs to the next
            // `namespace` boundary or EOF; nested-module bodies (phase 8.4)
            // pass `BodyScope::Nested` to terminate at their `OBLOCKEND`.
            let (seen_decl, seen_non_hash_decl, header_parsed) =
                self.parse_module_decls(BodyScope::File, header_present, false);

            // Another segment iff the loop stopped at a `namespace` header.
            let more = matches!(self.next_non_trivia_raw_at_pos(), Some(Token::Namespace));
            if !more {
                // Last segment — flush remaining raw tokens (trailing trivia,
                // plus anything LexFilter swallowed after the final filtered
                // token) *inside* this segment so the `IMPL_FILE` root stays a
                // clean wrapper.
                self.drain_raw_up_to(usize::MAX);
            }

            // FCS FS0222: "Only '#' compiler directives may occur prior to the
            // first 'namespace' declaration." A non-empty *anonymous* prefix
            // (`open System⏎namespace N`) before a `namespace` is illegal — FCS
            // drops the leading decls and keeps only the namespace(s). A prefix
            // containing only `#` compiler directives is legal but is still not a
            // separate anonymous module (`#I "/tmp"⏎namespace N` yields only the
            // namespace). Wrap either prefix in an `ERROR` node (not
            // `MODULE_OR_NAMESPACE`) so it stays lossless but is not projected;
            // flag only the non-`#` form.
            //
            // A *module-header* prefix (`module M⏎namespace N`) is the distinct
            // file-form-mixing case, handled by the outer wrap after this loop
            // (FCS drops *both* the module and the namespace into one empty
            // `AnonModule`, not the FS0222 namespace-kept shape). The
            // `!header_parsed` guard keeps such a header out of this branch.
            let hash_only_prefix = more && !header_parsed && seen_decl && !seen_non_hash_decl;
            let illegal_prefix = more && !header_parsed && seen_non_hash_decl;
            let kind = if illegal_prefix || hash_only_prefix {
                SyntaxKind::ERROR
            } else {
                SyntaxKind::MODULE_OR_NAMESPACE
            };
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(kind));
            if illegal_prefix {
                self.errors.push(ParseError {
                    message: "only '#' compiler directives may occur prior to the first \
                              'namespace' declaration"
                        .to_string(),
                    span: prefix_span,
                });
            }
            self.builder.finish_node(); // segment (MODULE_OR_NAMESPACE or ERROR)
            if !more {
                break;
            }
        }

        // File-form mixing (see the top of this fn): a whole-file module header
        // and a real top-level namespace (≥2 segments). Wrap all the parsed
        // segments in one outer `MODULE_OR_NAMESPACE`. The inner segments become
        // its (non-projected) children, so the file projects as a single empty
        // `AnonModule` — matching FCS — and the error is flagged.
        if leading_module_head && segment_count >= 2 {
            self.builder.start_node_at(
                file_cp,
                FSharpLang::kind_to_raw(SyntaxKind::MODULE_OR_NAMESPACE),
            );
            self.errors.push(ParseError {
                message: "a 'namespace' may not follow a whole-file 'module' header".to_string(),
                span: file_span,
            });
            self.builder.finish_node(); // outer empty AnonModule
        }

        self.builder.finish_node(); // IMPL_FILE
    }

    /// Phase 10.11 entry point for a signature file (`.fsi`) — FCS's
    /// `ParsedSigFileInput.contents`. Mirrors [`Self::parse_impl_file`]: the file
    /// is one or more `SynModuleOrNamespaceSig`s (an implicit `AnonModule`, a
    /// whole-file `module Foo`, or one-or-more `namespace N` blocks), each a
    /// `MODULE_OR_NAMESPACE` node — the *same* node kind as the impl side, since
    /// the header machinery ([`Self::parse_optional_file_header`]) is shared and
    /// the projection reads identical fields. Only the root kind (`SIG_FILE`) and
    /// the body (type-only *specifications*, [`Self::parse_sig_module_decls`])
    /// differ. Sig declarations themselves land in phases 10.12–10.15; this slice
    /// parses the file/segment skeleton with empty bodies.
    pub(super) fn parse_sig_file(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::SIG_FILE));
        // File-form mixing (see `parse_impl_file`): a leading whole-file
        // `module M` header plus a real top-level namespace (≥2 segments) bails
        // the whole file to one empty `AnonModule`.
        let leading_module_head = self.raw_leading_whole_file_module_head();
        let file_cp = self.builder.checkpoint();
        let file_span = self
            .peek()
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| self.source.len()..self.source.len());
        let mut segment_count = 0usize;
        loop {
            segment_count += 1;
            // Checkpoint so the segment's node kind is chosen after its body is
            // parsed (mirrors `parse_impl_file`): `MODULE_OR_NAMESPACE` normally,
            // or `ERROR` for an FS0222 illegal anonymous prefix before a namespace.
            let cp = self.builder.checkpoint();
            let prefix_span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            // The header is identical to the impl side (`module Foo` /
            // `namespace N`); reuse phase 8's machinery.
            let header_present = self.parse_optional_file_header();
            // Body specifications. The loop hands control back at the next
            // `namespace` header (file segmentation). `header_present` seeds the
            // `header_parsed` latch so a second (attributed) whole-file `module`
            // head after an existing header is *not* claimed (matching the impl
            // path / FCS).
            let (seen_decl, seen_non_hash_decl, header_parsed) =
                self.parse_sig_module_decls(BodyScope::File, header_present, false);
            let more = matches!(self.next_non_trivia_raw_at_pos(), Some(Token::Namespace));
            if !more {
                self.drain_raw_up_to(usize::MAX);
            }
            // FS0222 — see `parse_impl_file`: a hash-only anonymous prefix
            // before a `namespace` is legal but not projected as a module; a
            // non-hash anonymous prefix (`open System⏎namespace N`) is illegal
            // and gets flagged.
            let hash_only_prefix = more && !header_parsed && seen_decl && !seen_non_hash_decl;
            let illegal_prefix = more && !header_parsed && seen_non_hash_decl;
            let kind = if illegal_prefix || hash_only_prefix {
                SyntaxKind::ERROR
            } else {
                SyntaxKind::MODULE_OR_NAMESPACE
            };
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(kind));
            if illegal_prefix {
                self.errors.push(ParseError {
                    message: "only '#' compiler directives may occur prior to the first \
                              'namespace' declaration"
                        .to_string(),
                    span: prefix_span,
                });
            }
            self.builder.finish_node(); // segment (MODULE_OR_NAMESPACE or ERROR)
            if !more {
                break;
            }
        }
        // File-form mixing — wrap all segments in one outer (empty) `AnonModule`.
        if leading_module_head && segment_count >= 2 {
            self.builder.start_node_at(
                file_cp,
                FSharpLang::kind_to_raw(SyntaxKind::MODULE_OR_NAMESPACE),
            );
            self.errors.push(ParseError {
                message: "a 'namespace' may not follow a whole-file 'module' header".to_string(),
                span: file_span,
            });
            self.builder.finish_node(); // outer empty AnonModule
        }
        self.builder.finish_node(); // SIG_FILE
    }

    /// The signature-file body loop (phase 10.11) — the `.fsi` counterpart of
    /// [`Self::parse_module_decls`] at file scope. It dispatches the modelled
    /// `SynModuleSigDecl` forms — `open`/`open type` (10.13a), nested
    /// `module`/abbreviation (10.13b), and `val` (10.12a) — consumes the
    /// inter-segment layout virtuals, and returns at the next `namespace` header
    /// or EOF, keeping the file/segment skeleton lossless. The one header form it
    /// must claim is the *leading-attributed* whole-file `module` header
    /// (`[<AutoOpen>]⏎module M`): a leading `[<` hides the swallowed `module` from
    /// [`Self::parse_optional_file_header`] (the first raw token is `[<`), exactly
    /// as on the impl side (phase 10.7e). A not-yet-modelled specification token
    /// (type / exception sigs, 10.14/10.15) is flagged and bumped as an `ERROR`
    /// so the loop always makes progress.
    /// `scope` mirrors the impl loop: [`BodyScope::File`] runs to the next
    /// `namespace` header or EOF; [`BodyScope::Nested`] (a `module X = <block>`
    /// signature body, phase 10.13b) terminates at the body-closing `OBLOCKEND`.
    ///
    /// Returns `(seen_decl, seen_non_hash_decl, header_parsed)` — see
    /// [`Self::parse_module_decls`]; [`Self::parse_sig_file`] uses them for the
    /// FS0222 illegal-prefix check.
    ///
    /// `begin_delimited` marks a verbose-syntax `module X = begin … end`
    /// signature body — see [`Self::parse_module_decls`] for the terminator
    /// semantics (here the body holds specifications, but the `begin`/`end`
    /// framing is identical).
    fn parse_sig_module_decls(
        &mut self,
        scope: BodyScope,
        header_present: bool,
        begin_delimited: bool,
    ) -> (bool, bool, bool) {
        // Whether this segment already has a header — seeded from
        // `parse_optional_file_header` (a plain `module`/`namespace` header) and
        // latched once the leading-attributed whole-file form is claimed inline.
        // A whole-file header is only valid as the segment's leading construct, so
        // a second `module` head after an existing header falls to the deferred
        // arm (matching the impl path / FCS).
        let mut header_parsed = header_present;
        // Whether any body/spec token has been consumed. A whole-file header is
        // only valid as the segment's *leading* construct, so once body content
        // has begun a later `[<A>] module M` must stay in the body/error path —
        // not be claimed retroactively as the header (the impl loop's `seen_decl`
        // gate).
        let mut seen_decl = false;
        // Whether any parsed prefix content is *not* a `#` compiler directive.
        // A hash-only prefix before a file-scope `namespace` is legal and
        // dropped from the projected module list; any other non-empty anonymous
        // prefix is FS0222.
        let mut seen_non_hash_decl = false;
        while let Some((res, span)) = self.peek().cloned() {
            // A verbose-syntax `module X = begin … end` signature body ends at the
            // real `end` token, before the body-closing OBLOCKEND — mirror the
            // impl loop. `parse_nested_module_body` consumes the `end`/OBLOCKEND.
            if begin_delimited && matches!(&res, Ok(FilteredToken::Raw(Token::End))) {
                return (seen_decl, seen_non_hash_decl, header_parsed);
            }
            // A `namespace` header ends the current segment — but only at *file*
            // scope (the file loop opens a fresh `MODULE_OR_NAMESPACE` for the
            // next one). In a nested module body an (indented) `namespace` is not
            // a segment boundary — it stays part of the body and falls to the
            // error arm (FCS rejects a namespace there), rather than escaping to
            // the outer loop. Mirrors `parse_module_decls`. A non-empty anonymous
            // prefix before a file-scope namespace is the FS0222 illegal case
            // unless the prefix is hash-directive-only; the caller distinguishes
            // those via the returned `(seen_decl, seen_non_hash_decl,
            // header_parsed)`.
            if scope == BodyScope::File && matches!(&res, Ok(FilteredToken::Raw(Token::Namespace)))
            {
                return (seen_decl, seen_non_hash_decl, header_parsed);
            }
            // Nested `module M = …` / `module M = LongId` abbreviation (phase
            // 10.13b) — `SynModuleSigDecl.NestedModule` / `.ModuleAbbrev`. The
            // `module` keyword is swallowed by LexFilter, so it is detected on the
            // *raw* stream: `raw_module_head_eq() == Some(true)` is a `module`
            // head with a trailing `=` (a whole-file no-`=` header is claimed by
            // `parse_optional_file_header` / the leading-attr branch instead). The
            // `peek()`-is-a-real-token gate mirrors the impl loop: a preceding
            // layout virtual is consumed by the virtual arm first, so the swallowed
            // `module` is reached on a real token (the name). Shares the impl
            // `parse_nested_module_decl_at` with `sig = true`.
            if matches!(&res, Ok(FilteredToken::Raw(_))) && self.raw_module_head_eq() == Some(true)
            {
                self.parse_nested_module_decl_at(None, true);
                seen_decl = true;
                seen_non_hash_decl = true;
                continue;
            }
            // `open` / `open type` (phase 10.13a) — `SynModuleSigDecl.Open`,
            // structurally identical to the impl-side `SynModuleDecl.Open`, so the
            // impl `open` parser is reused verbatim. `open` is a real filtered
            // token (it opens no swallowing LexFilter context). Body content has
            // begun, so a later `module` head is no longer the file header.
            if matches!(&res, Ok(FilteredToken::Raw(Token::Open))) {
                self.parse_open_decl();
                seen_decl = true;
                seen_non_hash_decl = true;
                continue;
            }
            // A `#`-directive (`#I "/tmp"`, `#load "a.fsi"`) — FCS's
            // `SynModuleSigDecl.HashDirective`. Like the implementation-file
            // form, it has a natural end and is permitted before a file-scope
            // `namespace` without tripping FS0222; the file loop drops such a
            // hash-only prefix from the projected module list.
            if matches!(&res, Ok(FilteredToken::Raw(Token::Hash))) {
                self.parse_hash_directive();
                seen_decl = true;
                continue;
            }
            // `val x : <type>` (phase 10.12a) — `SynModuleSigDecl.Val`. `val` is a
            // real filtered token (it opens no swallowing LexFilter context). Body
            // content has begun, so a later `module` head is no longer the file
            // header. Reached in both file and nested-module bodies, so this also
            // closes the 10.13b nested-body `val` limitation.
            if matches!(&res, Ok(FilteredToken::Raw(Token::Val))) {
                self.parse_val_sig_decl();
                seen_decl = true;
                seen_non_hash_decl = true;
                continue;
            }
            // `exception E [of …] [= path]` (phase 10.15) —
            // `SynModuleSigDecl.Exception`. `exception` is a real filtered token
            // (it opens no swallowing LexFilter context), and `exconCore` is shared
            // with the impl exception, so the impl `EXCEPTION_DEFN` node is reused.
            // The `with member` augmentation (member sigs) is a later slice.
            if matches!(&res, Ok(FilteredToken::Raw(Token::Exception))) {
                self.parse_sig_exception_defn();
                seen_decl = true;
                seen_non_hash_decl = true;
                continue;
            }
            // A swallowed `type` opens a type signature (10.14) — parsed by
            // `parse_sig_type_defn`, which consumes the `type … (and …)*` group
            // (the `and`-chain loop lives there, like the impl `parse_type_defn`).
            // Body shapes not yet modelled (a `delegate of …`, a trailing
            // `with`/bare-member augmentation on a structural repr) record a "not
            // yet supported" diagnostic and skip the body *inside* the definition —
            // so a nested spec (`type C =`⏎`  val x : int`) is never promoted to a
            // top-level `SynModuleSigDecl.Val` (a phantom export). Detected on the
            // raw stream like the swallowed `module` head; the filtered peek is
            // the type name, so it would otherwise fall to the generic error arm
            // token-by-token and leak the body.
            if matches!(&res, Ok(FilteredToken::Raw(_))) && self.raw_leading_type_defn() {
                self.parse_sig_type_defn();
                seen_decl = true;
                seen_non_hash_decl = true;
                continue;
            }
            // A *stray* top-level `and` (slice 5). A valid `and`-continuation of a
            // `type … and …` group is consumed by `parse_sig_type_defn`'s chain
            // loop and never reaches here; an `and` that *does* reach the module
            // loop is therefore malformed — a continuation after a non-type decl
            // (`val x`⏎`and B = …`), which FCS rejects and drops. Skip the `and`
            // plus its (type-definition-shaped) header + body as one ERROR so a
            // nested member spec (`and B =`⏎`  val q`) stays contained rather than
            // leaking as a phantom top-level `SynModuleSigDecl.Val`. `and` is a
            // real filtered token (not swallowed), unlike a leading `type`.
            if matches!(&res, Ok(FilteredToken::Raw(Token::And))) {
                self.skip_stray_type_continuation();
                seen_decl = true;
                seen_non_hash_decl = true;
                continue;
            }
            // Top separators between specifications — `;` / `;;` (FCS's
            // `topSeparators` / `opt_seps` after a `moduleSpfn`, `pars.fsy:567`).
            // Inert *between* decls (they carry no `SynModuleSigDecl`), so emit
            // them as their tokens without erroring. Gated on `seen_decl`: a
            // *leading* separator (before any decl) is an FCS error, so it falls
            // to the error arm below — mirroring the impl loop's `;;` handling.
            if seen_decl && matches!(&res, Ok(FilteredToken::Raw(Token::Semi))) {
                self.bump_into(SyntaxKind::SEMI_TOK);
                continue;
            }
            if seen_decl && matches!(&res, Ok(FilteredToken::Raw(Token::SemiSemi))) {
                self.bump_into(SyntaxKind::SEMISEMI_TOK);
                continue;
            }
            // A leading `[<…>]` attribute run. Parse it under a checkpoint, skip
            // the offside `BlockSep` before the swallowed `module` (preserving the
            // keyword via `bump_layout_virtual`), then dispatch on what follows:
            // * an attributed *nested* module / abbreviation (`[<A>] module M =
            //   …`, head `Some(true)`) → wrap the attrs into the
            //   `NESTED_MODULE_DECL` via `cp` (10.7d × 10.13b);
            // * an attributed *whole-file* header (`[<AutoOpen>]⏎module M`, head
            //   `Some(false)`) — valid only as the segment's leading construct —
            //   attrs become direct `MODULE_OR_NAMESPACE` children (FCS's
            //   `SynModuleOrNamespaceSig.attribs`, field 5; 10.7e; `cp` unused);
            // * anything else (an attributed `val`/`type` sig — later slices, or a
            //   misplaced header) → flagged.
            if matches!(&res, Ok(FilteredToken::Raw(Token::LBrackLess))) {
                let cp = self.builder.checkpoint();
                self.parse_attribute_lists();
                let module_start = self
                    .next_non_trivia_raw_at_pos_with_span()
                    .map(|(_, s)| s.start)
                    .unwrap_or(usize::MAX);
                while matches!(
                    self.peek(),
                    Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), sp))
                        if sp.start <= module_start
                ) {
                    self.bump_layout_virtual();
                }
                // End-of-scope guard (mirrors the impl loop): if the attribute run
                // is the last item in this (nested) body, the cursor now sits on
                // the body-closing `OBLOCKEND` / EOF. `raw_module_head_eq()` scans
                // the *raw* stream and would cross that boundary to an *outer*
                // sibling `module B = …`, reparenting it inside this body. Only
                // claim a module head when a real token is still present here; a
                // dangling end-of-body attribute run falls to the error arm.
                let head = if matches!(
                    self.peek(),
                    None | Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _))
                ) {
                    None
                } else {
                    self.raw_module_head_eq()
                };
                match head {
                    Some(true) => {
                        self.parse_nested_module_decl_at(Some(cp), true);
                        seen_decl = true;
                        seen_non_hash_decl = true;
                    }
                    Some(false) if scope == BodyScope::File && !header_parsed && !seen_decl => {
                        // A whole-file `[<A>]⏎module M` header — only meaningful at
                        // *file* scope. In a nested body a no-`=` `module` head is
                        // not a header (FCS rejects it); the `scope` guard keeps it
                        // out of this arm so it falls to the error path.
                        self.parse_named_module_header();
                        header_parsed = true;
                    }
                    // An attributed `val` signature (`[<Literal>] val x : int`,
                    // FCS's `SynValSig.attributes`). Route through the shared
                    // `val` parser with the checkpoint so the attribute lists
                    // become leading `VAL_DECL` children.
                    _ if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Val)), _))) => {
                        self.parse_val_sig_decl_at(Some(cp));
                        seen_decl = true;
                        seen_non_hash_decl = true;
                    }
                    // An attributed `type` signature (`[<Sealed>] type T`, FCS's
                    // `SynComponentInfo.attributes`). The `type` keyword is
                    // swallowed by LexFilter, so detect it on the raw stream
                    // (like the unattributed `type` arm) and thread the
                    // checkpoint so the attrs land on the first `TYPE_DEFN`.
                    // Members of the type body remain a later slice.
                    _ if matches!(self.peek(), Some((Ok(FilteredToken::Raw(_)), _)))
                        && self.raw_leading_type_defn() =>
                    {
                        self.parse_sig_type_defn_at(Some(cp));
                        seen_decl = true;
                        seen_non_hash_decl = true;
                    }
                    // An attributed `exception` signature (`[<A>] exception E`,
                    // FCS's `SynExceptionDefnRepr.attributes`). `exception` is a
                    // real filtered token; thread the checkpoint so the attrs land
                    // as leading `EXCEPTION_DEFN` children.
                    _ if matches!(
                        self.peek(),
                        Some((Ok(FilteredToken::Raw(Token::Exception)), _))
                    ) =>
                    {
                        self.parse_sig_exception_defn_at(Some(cp));
                        seen_decl = true;
                        seen_non_hash_decl = true;
                    }
                    _ => {
                        self.errors.push(ParseError {
                            message:
                                "attributes on this signature specification are a later phase-10 \
                                 slice"
                                    .to_string(),
                            span,
                        });
                        seen_decl = true;
                        seen_non_hash_decl = true;
                    }
                }
                continue;
            }
            // Body-closing `OBLOCKEND` in a nested sig-module body (phase 10.13b),
            // mirroring the impl loop: at `BodyScope::Nested` a `BlockEnd` *not*
            // trailed by a `DeclEnd` is the body close — hand it back to
            // `parse_nested_module_body`.
            if scope == BodyScope::Nested
                && matches!(&res, Ok(FilteredToken::Virtual(Virtual::BlockEnd)))
                && !matches!(
                    self.next_non_trivia_filtered_after_pos(),
                    Some(FilteredToken::Virtual(Virtual::DeclEnd))
                )
            {
                return (seen_decl, seen_non_hash_decl, header_parsed);
            }
            // Layout virtuals (the empty body's `OBLOCKBEGIN`/`OBLOCKEND`/
            // `OBLOCKSEP`) are LexFilter scaffolding — emit as zero-width
            // placeholders, preserving any swallowed keyword.
            if let Ok(FilteredToken::Virtual(_)) = &res {
                self.bump_layout_virtual();
                continue;
            }
            // Anything else is a specification Phase 10.11 doesn't parse yet (or a
            // surfaced lex error). Flag and bump so the loop terminates; body
            // content has begun, so a later `module` head is no longer the file
            // header.
            let message = match &res {
                Err(e) => format!("lex error: {e:?}"),
                _ => "unexpected token".to_string(),
            };
            self.errors.push(ParseError { message, span });
            self.bump_into(SyntaxKind::ERROR);
            seen_decl = true;
            seen_non_hash_decl = true;
        }
        (seen_decl, seen_non_hash_decl, header_parsed)
    }

    /// Phase 8.2 — detect and parse an optional file-level `module`/
    /// `namespace` header into the already-open `MODULE_OR_NAMESPACE` node.
    ///
    /// The two keywords reach the parser differently. `namespace` flows
    /// through the filtered stream as a real `Token::Namespace`
    /// (`lexfilter/mod.rs:2892`); `module` is *swallowed* (it pushes a
    /// transient `CtxtModuleHead`, `lexfilter/mod.rs:2921`) and is only
    /// visible on the raw stream — so both are detected by peeking the raw
    /// stream. A leading `module` is treated as a whole-file `NamedModule`
    /// header **only** when no `=` follows the name (a `module Foo = …` is a
    /// *nested* module decl / abbreviation — phases 8.4/8.5 — left for the
    /// body loop to handle).
    /// Returns `true` iff a header was parsed (so the body loop knows a
    /// `module`/`namespace` header already exists in this segment — see the
    /// whole-file attributed-header branch in [`Self::parse_module_decls`], which
    /// must *not* claim a second header). A leading `[<…>]` hides the swallowed
    /// `module` here (the first raw is `[<`), so the attributed whole-file header
    /// is parsed by the body loop instead, with this returning `false`.
    pub(super) fn parse_optional_file_header(&mut self) -> bool {
        match self.next_non_trivia_raw_at_pos() {
            Some(Token::Namespace) => {
                self.parse_namespace_header();
                true
            }
            Some(Token::Module) if self.raw_leading_named_module() => {
                self.parse_named_module_header();
                true
            }
            _ => false,
        }
    }

    /// The shared module/namespace body loop — FCS's `moduleDefns`
    /// (`pars.fsy:1274`). Refactored out of [`Self::parse_impl_file`] so the
    /// top-level body, named-module bodies, namespace bodies, and
    /// nested-module bodies all share one decl dispatch. The `scope` selects
    /// the terminator: [`BodyScope::File`] runs to `peek() == None` or the next
    /// `namespace` header (phase 8.3, where the file-level loop starts a fresh
    /// `MODULE_OR_NAMESPACE`); [`BodyScope::Nested`] stops at the body-closing
    /// `OBLOCKEND` (phase 8.4).
    ///
    /// `header_present` is whether [`Self::parse_optional_file_header`] already
    /// claimed a `module`/`namespace` header for this segment — it seeds the
    /// `header_parsed` latch that gates the whole-file attributed-header branch
    /// (10.7e), so a no-`=` `module` head appearing *after* an existing header
    /// (`module Foo⏎[<A>]⏎module Bar`) stays a deferred error rather than a
    /// spurious second header. Nested bodies pass `false`.
    /// Returns `(seen_decl, seen_non_hash_decl, header_parsed)`: whether any
    /// declaration was parsed, whether any such declaration was not a `#`
    /// compiler directive, and whether this segment ended up with a
    /// `module`/`namespace` header (either the seeded `header_present` or one
    /// claimed inline by the 10.7e branch). [`Self::parse_impl_file`] uses these
    /// to drop hash-only prefixes before a namespace without an FS0222 error, and
    /// to flag non-hash anonymous prefixes.
    ///
    /// `begin_delimited` marks a verbose-syntax `module X = begin … end` body
    /// (`wrappedNamedModuleDefn`, `pars.fsy:1478`): the body decls sit inside both
    /// the `OBLOCKBEGIN` block and the real `begin`/`end` pair, so the loop must
    /// stop at the raw `end` (which precedes the body-closing `OBLOCKEND`),
    /// handing the `end`/`OBLOCKEND` back to [`Self::parse_nested_module_body`].
    /// Every other caller passes `false`.
    pub(super) fn parse_module_decls(
        &mut self,
        scope: BodyScope,
        header_present: bool,
        begin_delimited: bool,
    ) -> (bool, bool, bool) {
        // Tracks whether the previous decl ended without a `Virtual::BlockSep`
        // separator. While `needs_sep` is true, any raw expr-starter on the
        // same logical row is a syntax error rather than a new decl — the
        // decl parser stopped because the token can't continue the previous
        // expression (e.g. `a & b`: AMP isn't infix and isn't an app-arg
        // continuation, so `parse_app_expr` returns early). Treating that
        // dangling raw as a new top-level decl would diverge from FCS, which
        // emits "Unexpected symbol" error 10 and drops the trailing tokens.
        let mut needs_sep = false;
        // Whether at least one declaration has been parsed in this body. Gates
        // the top-level `;;` separator: `topSeparators` only follows a
        // `moduleDefnOrDirective` (`pars.fsy:1281`), so a *leading* `;;` (before
        // any decl) is an error in FCS ("Unexpected symbol ';;'"), whereas a
        // post-decl `;;` is an inert separator. Unlike `needs_sep` (which a
        // BlockSep/virtual resets between decls) this latches once set.
        let mut seen_decl = false;
        // Whether any parsed prefix content is *not* a `#` compiler directive.
        // A hash-only prefix before a file-scope `namespace` is legal and
        // dropped from the projected module list; any other non-empty anonymous
        // prefix is FS0222.
        let mut seen_non_hash_decl = false;
        // Whether this segment already has a `module`/`namespace` header — seeded
        // from `parse_optional_file_header`, latched `true` once the whole-file
        // attributed-header branch (10.7e) claims one. Gates that branch so only
        // the file's *leading* `module Foo` header (no `=`) is claimed.
        let mut header_parsed = header_present;
        // Incremental offside-block nesting depth at this body's top level,
        // maintained by [`Self::advance_block_depth`] (stepped once per iteration
        // from `depth_pos`). It gates the single-`;` top separator: a `;` only
        // separates decls at depth 0; deeper it is still inside the preceding
        // decl's open block (a type definition's `CtxtTypeDefns`, whose block a
        // single `;` does not close). Stepping incrementally keeps that gate
        // linear over the body rather than rescanning the prefix per `;`.
        //
        // For a nested body the caller (`parse_nested_module_decl`) has already
        // consumed the body's opening `OBLOCKBEGIN`, so depth 0 is the body's top
        // level. For a whole-file `module`/`namespace` *header* the body opener is
        // still pending and is consumed by the virtual arm below; the
        // `!seen_decl` re-base there resets `depth`/`depth_pos` past that leading
        // scaffolding so the body top level still reads as depth 0.
        let mut depth = 0i32;
        let mut depth_pos = self.pos;
        while let Some((res, span)) = self.peek().cloned() {
            self.advance_block_depth(&mut depth, &mut depth_pos);
            // A verbose-syntax `module X = begin … end` body ends at the real
            // `end` token (the body decls sit *inside* both the OBLOCKBEGIN block
            // and the `begin`/`end` pair, so `end` arrives before the body-closing
            // OBLOCKEND). Hand it back to `parse_nested_module_body`, which
            // consumes the `end` and then the OBLOCKEND. Gated on `begin_delimited`
            // so a stray `end` in an ordinary body still errors below.
            if begin_delimited && matches!(&res, Ok(FilteredToken::Raw(Token::End))) {
                return (seen_decl, seen_non_hash_decl, header_parsed);
            }
            // A `namespace` header at file scope ends the current segment — the
            // file-level loop in `parse_impl_file` starts a fresh
            // `MODULE_OR_NAMESPACE` for it (phase 8.3). `namespace` is a real
            // filtered token (it opens no swallowing LexFilter context), so it
            // surfaces here directly; the body's trailing `OBLOCKEND`/`ODECLEND`/
            // `OBLOCKSEP` virtuals were already consumed as strays above, so the
            // current segment owns them. (Only at file scope: a `namespace`
            // cannot nest inside a module body.)
            if scope == BodyScope::File && matches!(&res, Ok(FilteredToken::Raw(Token::Namespace)))
            {
                return (seen_decl, seen_non_hash_decl, header_parsed);
            }
            // `Virtual::Let` opens a top-level `let` binding (LexFilter's
            // rewrite of the raw `Token::Let`). Detected separately from the
            // expression-starter path because the binding production owns
            // distinct tokens (IDENT_TOK, EQUALS_TOK) and ends at the RHS's
            // ODECLEND rather than the expression boundary.
            //
            // A *raw* `Token::Let`/`Token::Use` reaches the loop head only when a
            // top separator left an inline `let` in decl position — `open X;
            // let y = 1` / `a; let y = 1`. A single `;` (unlike `;;`) does not
            // short-circuit the offside rule, so LexFilter does not rewrite that
            // inline keyword to `Virtual::Let`; FCS still parses it as a
            // `topSeparator`-separated `defnBindings`, so we route it to the same
            // `parse_module_let` (which accepts the raw keyword). A `let … in …`
            // with a body directly after the `in` becomes a `SynModuleDecl.Expr`
            // (`SynExpr.LetOrUse`); a `let` *before* the `;` swallows the `;` into
            // its RHS block, so the raw keyword never surfaces here.
            //
            // The raw form is gated on `depth == 0` — a top separator only sits at
            // the body's top level. At positive depth a raw `let`/`use` is still
            // inside a preceding decl's unclosed block (e.g. after a rejected `;`
            // in an open type body, `type T =⏎ | A⏎ ; let x = 1`, where an
            // `OBLOCKSEP` cleared `needs_sep`); promoting it to a module `LET_DECL`
            // there would leak it out of the malformed body. `Virtual::Let` is the
            // offside module binding, which only surfaces here at depth 0 anyway.
            if matches!(&res, Ok(FilteredToken::Virtual(Virtual::Let)))
                || (depth == 0 && matches!(&res, Ok(FilteredToken::Raw(Token::Let | Token::Use))))
            {
                if needs_sep {
                    self.errors.push(ParseError {
                        message: "unexpected token".to_string(),
                        span,
                    });
                    self.bump_into(SyntaxKind::ERROR);
                    continue;
                }
                self.parse_module_let();
                // A trailing `;` inside the binding's `typedSequentialExpr` RHS
                // (`let x = a;`) is absorbed by the RHS block, so it never reaches
                // the loop's `Semi` arm. The following decl is still separated the
                // normal way: a newline-separated offside decl (`let x = a;⏎
                // let y = b`) is preceded by the binding's `OBLOCKEND`/`OBLOCKSEP`
                // virtuals, which the layout-virtual arm consumes and which clear
                // `needs_sep` before the next decl. So this stays `true` (matching
                // every other decl arm); a *same-line* non-expression follower
                // (`let x = a; open X`) is correctly rejected — the RHS block
                // swallowed it and no separator cleared `needs_sep`.
                //
                // `parse_module_let` also handles the `let … in …` forms: a body
                // directly after the `in` yields a `SynModuleDecl.Expr(LetOrUse)`,
                // while an `in` followed by a dedented sibling stays a flat
                // `SynModuleDecl.Let` with the `in` claimed as a bare terminator.
                needs_sep = true;
                seen_decl = true;
                seen_non_hash_decl = true;
                continue;
            }
            // `open` / `open type` begins a `SynModuleDecl.Open`. It opens no
            // LexFilter context, so it arrives as a plain `Token::Open` in the
            // filtered stream; detect it before the expression-starter path
            // since `open` is not an expression starter.
            if let Ok(FilteredToken::Raw(Token::Open)) = &res {
                if needs_sep {
                    self.errors.push(ParseError {
                        message: "unexpected token".to_string(),
                        span,
                    });
                    self.bump_into(SyntaxKind::ERROR);
                    continue;
                }
                self.parse_open_decl();
                needs_sep = true;
                seen_decl = true;
                seen_non_hash_decl = true;
                continue;
            }
            // A swallowed `module X = …` introduces a nested module (phase
            // 8.4) or a module abbreviation (phase 8.5). `module` pushes a
            // transient `CtxtModuleHead` and is swallowed by LexFilter — it
            // reaches neither the filtered stream nor a virtual — so, like the
            // file-header path (`parse_optional_file_header`), it is detected
            // by peeking the *raw* stream. `raw_module_head_eq() == Some(true)`
            // confirms a `module`-headed name followed by `=` (the
            // whole-file-vs-nested switch); it must precede the
            // expression-starter arm below because the nested module's name
            // (the first *filtered* token, the `module` keyword being
            // swallowed) is itself an expression starter.
            //
            // The `peek()`-is-a-real-token gate matters: any layout virtual
            // (`OBLOCKBEGIN` opening the enclosing body, `OBLOCKSEP` before a
            // sibling) that *precedes* the swallowed `module` is consumed first
            // by the stray-virtual arm below — which preserves the swallowed
            // keyword via [`Self::bump_layout_virtual`] — so by the time we
            // claim the module the cursor sits on the head's first *real*
            // token (a `rec`/access modifier, which pass through, or the name).
            if matches!(self.peek(), Some((Ok(FilteredToken::Raw(_)), _)))
                && self.raw_module_head_eq() == Some(true)
            {
                if needs_sep {
                    self.errors.push(ParseError {
                        message: "unexpected token".to_string(),
                        span,
                    });
                    self.bump_into(SyntaxKind::ERROR);
                    continue;
                }
                self.parse_nested_module_decl();
                needs_sep = true;
                seen_decl = true;
                seen_non_hash_decl = true;
                continue;
            }
            // A swallowed bare `type` introduces a type definition (phase 9).
            // `type` pushes a transient `CtxtTypeDefns` and is swallowed by
            // LexFilter (it reaches neither the filtered stream nor a virtual),
            // exactly like `module` — so it is detected by peeking the *raw*
            // stream. This arm must precede the expression-starter arm below
            // because the type's name (the first *filtered* token, the `type`
            // keyword being swallowed) is itself an expression starter. The
            // `peek()`-is-a-real-token gate matches the nested-module arm: a
            // layout virtual preceding the swallowed `type` is consumed first
            // by the stray-virtual arm, which preserves the keyword via
            // [`Self::bump_layout_virtual`]. (`open type T` is handled earlier
            // by the `open` arm; there the next raw is `Token::Open`, not
            // `Token::Type`, so this gate does not fire on it.)
            if matches!(self.peek(), Some((Ok(FilteredToken::Raw(_)), _)))
                && self.raw_leading_type_defn()
            {
                if needs_sep {
                    self.errors.push(ParseError {
                        message: "unexpected token".to_string(),
                        span,
                    });
                    self.bump_into(SyntaxKind::ERROR);
                    continue;
                }
                self.parse_type_defn();
                needs_sep = true;
                seen_decl = true;
                seen_non_hash_decl = true;
                continue;
            }
            // A plain raw `exception` introduces an exception definition (phase
            // 9.15a). Unlike `type`/`module`, `exception` is *not* swallowed by
            // LexFilter — it opens the silent `CtxtException` but passes the
            // keyword through — so it surfaces here as a real filtered
            // `Token::Exception`, detected like `open`.
            if let Ok(FilteredToken::Raw(Token::Exception)) = &res {
                if needs_sep {
                    self.errors.push(ParseError {
                        message: "unexpected token".to_string(),
                        span,
                    });
                    self.bump_into(SyntaxKind::ERROR);
                    continue;
                }
                self.parse_exception_defn();
                needs_sep = true;
                seen_decl = true;
                seen_non_hash_decl = true;
                continue;
            }
            // A plain raw `extern` introduces a DllImport prototype (FCS's
            // `cPrototype`, `pars.fsy:3186`). `extern` opens no LexFilter context,
            // so it surfaces here as a real filtered `Token::Extern`, detected like
            // `exception`.
            if let Ok(FilteredToken::Raw(Token::Extern)) = &res {
                if needs_sep {
                    self.errors.push(ParseError {
                        message: "unexpected token".to_string(),
                        span,
                    });
                    self.bump_into(SyntaxKind::ERROR);
                    continue;
                }
                self.parse_extern_decl_at(None);
                needs_sep = true;
                seen_decl = true;
                seen_non_hash_decl = true;
                continue;
            }
            // A `#`-directive (`#I "/tmp"`, `#load "a.fs"`) — FCS's
            // `SynModuleDecl.HashDirective`. `#` reaches the filtered stream as a
            // plain raw `Token::Hash` (LexFilter opens no context for it).
            if let Ok(FilteredToken::Raw(Token::Hash)) = &res {
                if needs_sep {
                    self.errors.push(ParseError {
                        message: "unexpected token".to_string(),
                        span,
                    });
                    self.bump_into(SyntaxKind::ERROR);
                    continue;
                }
                self.parse_hash_directive();
                // A `#`-directive's args have a natural end, so FCS lets the next
                // declaration follow *without* a separator (`#I "/tmp" let x = 1`,
                // `#I "/tmp" #load "a.fs"`). Leave `needs_sep = false` so the loop
                // dispatches the following construct directly.
                needs_sep = false;
                seen_decl = true;
                continue;
            }
            // A leading `[<` opens one or more attribute lists. Phase 10.5
            // carries them on the `let`-binding (FCS's `SynBinding.attributes`,
            // `opt_attributes opt_access defnBindings`, `pars.fsy:1308`): the
            // attribute lists become leading children of the binding's
            // `LET_DECL`. Other carriers — standalone `SynModuleDecl.Attributes`,
            // type/member attributes — are later phase-10 slices, so a leading
            // `[<` *not* followed by a `let`/`use` is flagged rather than
            // silently mis-parsed.
            if let Ok(FilteredToken::Raw(Token::LBrackLess)) = &res {
                if needs_sep {
                    self.errors.push(ParseError {
                        message: "unexpected token".to_string(),
                        span,
                    });
                    self.bump_into(SyntaxKind::ERROR);
                    continue;
                }
                // Keep leading trivia a sibling of the eventual `LET_DECL`.
                self.drain_raw_up_to(span.start);
                let cp = self.builder.checkpoint();
                self.parse_attribute_lists();
                // Decide the carrier on the *raw* stream — robust to the layout
                // virtuals / `OLET` relabel LexFilter shows after `>]` (the
                // `let` arrives as a raw `Token::Let` on the same line, or as a
                // `Virtual::Let` after a `BlockSep` on the next line; either way
                // the next non-trivia raw is `Token::Let`/`Use`).
                if matches!(
                    self.next_non_trivia_raw_at_pos(),
                    Some(Token::Let | Token::Use)
                ) {
                    // Skip an inter-line `BlockSep` so the filtered cursor lands
                    // on the `let` (for the classifier below and `parse_let_decl_at`).
                    while matches!(
                        self.peek(),
                        Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
                    ) {
                        self.bump_into(SyntaxKind::ERROR);
                    }
                    if self.module_let_is_inline_in_expr() {
                        // Attributed let-in *expression* (`[<A>] let a = 0 in ()`):
                        // an attribute list cannot attach to a `let`-expression, so
                        // FCS floats it into a standalone `SynModuleDecl.Attributes`
                        // and parses `let … in body` as a separate
                        // `SynModuleDecl.Expr(LetOrUse)`. Emit the parsed attribute
                        // lists (under `cp`) as their own `ATTRIBUTES_DECL`, then
                        // `continue` so the loop re-dispatches the `let` fresh
                        // (non-attributed) into `parse_module_let`, which builds the
                        // `EXPR_DECL`. `needs_sep = false` lets the `let` begin its
                        // own decl even on the same line. (Mirrors the no-valid-
                        // carrier attribute path below.)
                        self.builder.start_node_at(
                            cp,
                            FSharpLang::kind_to_raw(SyntaxKind::ATTRIBUTES_DECL),
                        );
                        self.builder.finish_node();
                        needs_sep = false;
                        seen_decl = true;
                        seen_non_hash_decl = true;
                        continue;
                    }
                    // Flat/plain attributed `let` (no inline `in` body): wrap the
                    // attribute lists + the binding in one `LET_DECL` via the
                    // checkpoint, so the attrs are leading children of the binding.
                    self.parse_let_decl_at(Some(cp), SyntaxKind::LET_DECL);
                    needs_sep = true;
                    seen_decl = true;
                    seen_non_hash_decl = true;
                } else if self.raw_leading_type_defn() {
                    // Type-header carrier (phase 10.7a): `[<…>] type T = …`. The
                    // attribute lists attach to the type definition's
                    // `SynComponentInfo`. Skip a `BlockSep` sitting *between the
                    // attribute list and the `type` keyword* — the offside
                    // `[<A>]⏎type T` form (the common `[<Struct>]`-on-its-own-line
                    // shape) — via `bump_layout_virtual` (**not**
                    // `bump_into(ERROR)`: the swallowed `type` shares the virtual's
                    // span, so a plain `bump_into` would drain it). The skip is
                    // gated on the virtual *preceding* the `type` keyword: a
                    // `BlockSep` *after* `type` is the `[<A>] type⏎T` form (an
                    // offside *name*), which FCS rejects as an incomplete type
                    // name — so leave it in place for the name parse to error on,
                    // rather than silently accepting it. `parse_type_defn_at` then
                    // recovers the keyword from the raw stream and wraps the
                    // attribute lists into the first `TYPE_DEFN` via the checkpoint.
                    let type_start = self
                        .next_non_trivia_raw_at_pos_with_span()
                        .map(|(_, s)| s.start)
                        .unwrap_or(usize::MAX);
                    while matches!(
                        self.peek(),
                        Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), span))
                            if span.start <= type_start
                    ) {
                        self.bump_layout_virtual();
                    }
                    self.parse_type_defn_at(Some(cp));
                    needs_sep = true;
                    seen_decl = true;
                    seen_non_hash_decl = true;
                } else if !matches!(
                    self.peek(),
                    None | Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _))
                ) && matches!(self.next_non_trivia_raw_at_pos(), Some(Token::Exception))
                {
                    // Exception-header carrier (phase 10.7m): `[<A>]⏎exception E`.
                    // The attribute lists attach to `SynExceptionDefnRepr.attributes`
                    // (FCS's leading `$1`). Unlike `type`/`module`, `exception` is
                    // *not* swallowed by LexFilter — it surfaces as a real filtered
                    // `Token::Exception` — so the inter-line `[<A>]⏎exception` form's
                    // `BlockSep` (between `>]` and the keyword) is skipped with a
                    // plain `bump_into(ERROR)` (mirroring the `let`/`use` carrier),
                    // *not* `bump_layout_virtual` (there is no swallowed keyword
                    // riding the virtual's span to preserve). `parse_exception_defn_at`
                    // then bumps the real keyword and wraps the attribute lists into
                    // the `EXCEPTION_DEFN` via the checkpoint. The filtered-peek-is-
                    // not-end-of-scope guard mirrors the nested-module carrier: the
                    // raw lookahead would otherwise cross a body-closing `OBLOCKEND`
                    // and wrongly claim an *outer* scope's `exception` (a trailing
                    // `[<…>]` at a nested body's end is a standalone `Attributes`
                    // decl, handled by the gate below).
                    while matches!(
                        self.peek(),
                        Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
                    ) {
                        self.bump_into(SyntaxKind::ERROR);
                    }
                    self.parse_exception_defn_at(Some(cp));
                    needs_sep = true;
                    seen_decl = true;
                    seen_non_hash_decl = true;
                } else if !matches!(
                    self.peek(),
                    None | Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _))
                ) && matches!(self.next_non_trivia_raw_at_pos(), Some(Token::Extern))
                {
                    // Extern-prototype carrier: `[<DllImport(…)>]⏎extern int puts(…)`.
                    // The attribute lists attach to the lowered `SynBinding.attributes`
                    // (FCS's `SynModuleDecl.Let`). Like `exception`, `extern` is not
                    // swallowed by LexFilter — it surfaces as a real filtered
                    // `Token::Extern` — so the inter-line `[<A>]⏎extern` form's
                    // `BlockSep` (between `>]` and the keyword) is skipped with a
                    // plain `bump_into(ERROR)` (no swallowed keyword riding the
                    // virtual's span). `parse_extern_decl_at` then bumps the real
                    // keyword and wraps the attribute lists into the `EXTERN_DECL` via
                    // the checkpoint. The filtered-peek-is-not-end-of-scope guard
                    // mirrors the exception carrier.
                    while matches!(
                        self.peek(),
                        Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
                    ) {
                        self.bump_into(SyntaxKind::ERROR);
                    }
                    self.parse_extern_decl_at(Some(cp));
                    needs_sep = true;
                    seen_decl = true;
                    seen_non_hash_decl = true;
                } else if !matches!(
                    self.peek(),
                    None | Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _))
                ) && self.raw_module_head_eq() == Some(true)
                {
                    // Nested module-header carrier (phase 10.7d):
                    // `[<AutoOpen>]⏎module Inner = …`. The attribute lists attach
                    // to the nested module's `SynComponentInfo.attributes` (FCS
                    // field 0). The filtered-peek-is-not-end-of-scope guard
                    // mirrors the standalone gate below: `raw_module_head_eq()`
                    // peeks the *raw* stream, which would cross a body-closing
                    // `OBLOCKEND` and wrongly claim the *outer* scope's `module`
                    // (an `[<assembly: …>]` at the end of a nested body followed by
                    // a sibling `module B` is a standalone `Attributes` decl, not a
                    // carrier for `B`). Skip a `BlockSep` sitting *between the
                    // attribute list's `>]` and the swallowed `module` keyword* —
                    // the offside `[<A>]⏎module Inner` form — via `bump_layout_virtual`
                    // (**not** `bump_into(ERROR)`: the swallowed `module` shares
                    // the virtual's span, so a plain bump would drain it). Mirrors
                    // the type-header carrier above; gated on the virtual
                    // *preceding* the `module` keyword. `parse_nested_module_decl_at`
                    // then recovers the keyword from the raw stream and wraps the
                    // attribute lists into the `NESTED_MODULE_DECL` via `cp`.
                    let module_start = self
                        .next_non_trivia_raw_at_pos_with_span()
                        .map(|(_, s)| s.start)
                        .unwrap_or(usize::MAX);
                    while matches!(
                        self.peek(),
                        Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), span))
                            if span.start <= module_start
                    ) {
                        self.bump_layout_virtual();
                    }
                    self.parse_nested_module_decl_at(Some(cp), false);
                    needs_sep = true;
                    seen_decl = true;
                    seen_non_hash_decl = true;
                } else if scope == BodyScope::File
                    && !header_parsed
                    && !seen_decl
                    && self.raw_module_head_eq() == Some(false)
                {
                    // Whole-file module-header carrier (phase 10.7e):
                    // `[<AutoOpen>]⏎module Foo` (a `module` head with *no* `=`,
                    // `raw_module_head_eq() == Some(false)`). The attribute lists
                    // were parsed (under `cp`) as direct children of the open
                    // `MODULE_OR_NAMESPACE` — FCS's `SynModuleOrNamespace.attribs`
                    // — so there is *no* wrapper decl: `cp` goes unused and the
                    // header is parsed inline ahead of the body. Gated on file
                    // scope + *no header yet* (`!header_parsed`) + *no decl yet*
                    // (`!seen_decl`): a whole-file header is only valid as the
                    // file's leading construct. A no-`=` `module` head appearing
                    // after an existing header (`module Foo⏎[<A>]⏎module Bar`, or a
                    // `namespace N⏎[<A>]⏎module M`) or after a decl is malformed and
                    // stays in the deferred `else`. The header is normally claimed
                    // by `parse_optional_file_header`, but a leading `[<…>]` hides
                    // the swallowed `module` from it (the first raw token is `[<`),
                    // so the dispatch handles it. Skip the offside `BlockSep` before
                    // the swallowed `module` (mirroring the nested/type carriers).
                    let module_start = self
                        .next_non_trivia_raw_at_pos_with_span()
                        .map(|(_, s)| s.start)
                        .unwrap_or(usize::MAX);
                    while matches!(
                        self.peek(),
                        Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), span))
                            if span.start <= module_start
                    ) {
                        self.bump_layout_virtual();
                    }
                    self.parse_named_module_header();
                    header_parsed = true;
                    needs_sep = false;
                } else if matches!(
                    self.peek(),
                    None | Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _))
                ) || (begin_delimited
                    && matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::End)), _))))
                    || (self.raw_module_head_eq().is_none()
                        && self.next_non_trivia_raw_at_pos().is_some_and(|t| {
                            // An expression decl (`opt_attributes declExpr`) — or `do`,
                            // the canonical `[<assembly: …>]⏎ do ()` idiom (the `do`
                            // expression itself is the separate unimplemented top-level
                            // `do` slice, but the `Attributes` decl must still be
                            // emitted ahead of it). The `declExpr`-but-not-`minusExpr`
                            // starters kept out of the global `raw_starts_minus_expr`
                            // are admitted explicitly: the from-end prefix `^`
                            // (`[<Obsolete>]⏎ ^1` → attrs + `IndexFromEnd`; `^` is also
                            // read on the raw stream in type/measure context as a head
                            // typar, hence not global), and the open-lower range
                            // operators `..` / `..^` (`[<Obsolete>]⏎ ..^1` → attrs +
                            // `IndexRange`).
                            raw_starts_minus_expr(t)
                                || matches!(
                                    t,
                                    Token::Do | Token::Op("^") | Token::DotDot | Token::DotDotHat
                                )
                        }))
                {
                    // Standalone `SynModuleDecl.Attributes` (phase 10.7) — FCS's
                    // `opt_attributes declExpr`: a leading `[<…>]` not attached to
                    // a carrier, followed by an **expression** declaration
                    // (`[<assembly: Foo>] ignore 0`), a `do`, or end of scope. End
                    // of scope is the *filtered* peek being EOF or a body-closing
                    // `OBLOCKEND` — a raw lookahead would cross that boundary and
                    // wrongly classify the *outer* scope's next decl (e.g. a
                    // standalone attr at the end of a nested `module A`, followed by
                    // a sibling `module B`). A verbose `begin … end` body closes on
                    // the real `end` *before* its `OBLOCKEND` (the top-of-loop
                    // terminator), so when `begin_delimited` that `end` is the
                    // end-of-scope sentinel here too — keeping a trailing standalone
                    // attribute (`module A = begin [<assembly: Foo>] end`) an
                    // `ATTRIBUTES_DECL`, matching FCS and the ordinary-body path.
                    // Otherwise gate positively on an
                    // expression-starter so attrs before a non-expression construct
                    // fall to the deferred-error arm below; a swallowed `module`
                    // head shows its *name* (an expr-starter ident) as the next raw,
                    // so exclude it via `raw_module_head_eq`. Wrap the parsed
                    // attribute lists (under `cp`) in an `ATTRIBUTES_DECL`; leave
                    // `needs_sep = false` so the following expression — even on the
                    // same line — begins its own decl.
                    self.builder
                        .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::ATTRIBUTES_DECL));
                    self.builder.finish_node();
                    needs_sep = false;
                    seen_decl = true;
                    seen_non_hash_decl = true;
                } else {
                    // A still-deferred or invalid carrier follows — a `namespace`
                    // header (FCS error 530 rejects attributes there), an
                    // `open`/other keyword-led construct that FCS rejects after
                    // attributes, or a malformed whole-file `module M` head
                    // appearing where it is not valid (mid-file or nested; the
                    // valid file-leading form is handled above, and the nested
                    // `module M = …` head before it). (The `exception` carrier is
                    // handled by the 10.7m branch above.)
                    // Flag it and leave the parsed `ATTRIBUTE_LIST`s as siblings so
                    // the following construct parses on its own.
                    let follow_span = self
                        .next_non_trivia_raw_at_pos_with_span()
                        .map(|(_, s)| s)
                        .unwrap_or_else(|| self.source.len()..self.source.len());
                    self.errors.push(ParseError {
                        message: "attributes on this carrier are a later phase-10.7 slice"
                            .to_string(),
                        span: follow_span,
                    });
                    needs_sep = false;
                    // Body content (an attribute prefix) has begun, so a following
                    // `namespace` makes this the FS0222 illegal-prefix case
                    // (`[<AutoOpen>]⏎namespace N`) rather than a fresh header —
                    // mark it seen so `parse_impl_file` drops the prefix.
                    seen_decl = true;
                    seen_non_hash_decl = true;
                }
                continue;
            }
            // A top-level `;;` is a declaration *separator*
            // (`topSeparator: SEMICOLON_SEMICOLON`, `pars.fsy:6967`), not an
            // expression — it carries no `SynModuleDecl`. `;;` passes through
            // LexFilter as a raw `Token::SemiSemi` (its only role there is to
            // short-circuit the offside rule, `LexFilter.fs:1806`). After a
            // decl it is inert: emit it as a `SEMISEMI_TOK` and clear
            // `needs_sep` so the following decl parses cleanly — crucially for
            // `open`/`type`, whose decls leave no `BlockEnd` virtual to reset
            // it, so without this the trailing `let` would cascade into
            // spurious errors. A *leading* `;;` (no decl yet) is not a valid
            // separator — `topSeparators` only follows a `moduleDefnOrDirective`
            // (`pars.fsy:1281`), and FCS rejects it ("Unexpected symbol ';;'")
            // — so it falls through to the generic error path below.
            if seen_decl && matches!(&res, Ok(FilteredToken::Raw(Token::SemiSemi))) {
                self.bump_into(SyntaxKind::SEMISEMI_TOK);
                needs_sep = false;
                continue;
            }
            // A single `;` is also a top-level decl separator
            // (`topSeparator: SEMICOLON`, `pars.fsy:6967`) — the same inert role
            // as `;;`. It reaches the loop after a non-block decl that does not
            // absorb it (`open`/`exception`/a module abbreviation/an expression
            // decl, which stop at the `;`); emit a `SEMI_TOK` and clear
            // `needs_sep`. A `let` binding's `typedSequentialExpr` RHS *does*
            // absorb a trailing `;` (so `let x = a; b` is one `Sequential` decl)
            // — handled by the `needs_sep` reset on the let arm.
            //
            // The depth gate is essential. Unlike `;;`, a single `;` does not
            // short-circuit the offside rule (FCS's `isSemiSemi`,
            // `LexFilter.fs:1806`), so it does not close a preceding *block*
            // decl's offside block: after `type T = int`, the type's
            // `OBLOCKEND` lands at end-of-line, leaving the `;` *inside*
            // `CtxtTypeDefns` (`depth > 0`). FCS rejects such a `;`
            // (`type T = int; open System` is an error), so we must not treat it
            // as a clean separator — at positive depth it falls through to the
            // generic error path. A *leading* `;` (no decl yet) is likewise an
            // FCS error, so it too falls through (like a leading `;;`). The
            // `matches!` precedes the depth read only for clarity — `depth` is
            // already current (stepped at the top of the loop), so the order is
            // free.
            if seen_decl && matches!(&res, Ok(FilteredToken::Raw(Token::Semi))) && depth == 0 {
                self.bump_into(SyntaxKind::SEMI_TOK);
                needs_sep = false;
                continue;
            }
            if self.peek_is_expr_start() {
                if needs_sep {
                    // Same-line continuation that couldn't be absorbed by
                    // the prior decl. Record an error and bump as ERROR;
                    // keep `needs_sep` true so any further dangling raws on
                    // this row also error rather than starting fresh decls.
                    self.errors.push(ParseError {
                        message: "unexpected token".to_string(),
                        span,
                    });
                    self.bump_into(SyntaxKind::ERROR);
                    continue;
                }
                self.parse_module_decl();
                needs_sep = true;
                seen_decl = true;
                seen_non_hash_decl = true;
                continue;
            }

            // Virtual tokens between decls (BlockSep, BlockBegin, …) are
            // LexFilter scaffolding Phase 1 doesn't model semantically; emit
            // them into the green tree as zero-width ERROR placeholders
            // without generating a user-visible parse error. A BlockSep also
            // resets `needs_sep` — the offside separator is the canonical
            // decl boundary, so the next expr-starter is allowed to begin a
            // fresh decl.
            if let Ok(FilteredToken::Virtual(v)) = &res {
                // In a nested-module body the body-closing `OBLOCKEND`
                // terminates the loop, handed back to
                // [`Self::parse_nested_module_decl`]. It is distinguished from
                // a binding's own RHS-block `OBLOCKEND` — which the binding
                // parser defers to us (`drain_let_rhs_block`) — by what
                // follows: a binding terminator is the pair
                // `OBLOCKEND·ODECLEND`, whereas the body close is an
                // `OBLOCKEND` *not* trailed by an `ODECLEND` (it is followed by
                // an `OBLOCKSEP`, the enclosing body's `OBLOCKEND`, a fresh
                // decl, or EOF). Ground-truthed against the filtered stream for
                // single-decl, multi-decl, expr-body, open-body, empty-body,
                // and doubly-nested bodies.
                if scope == BodyScope::Nested
                    && *v == Virtual::BlockEnd
                    && !matches!(
                        self.next_non_trivia_filtered_after_pos(),
                        Some(FilteredToken::Virtual(Virtual::DeclEnd))
                    )
                {
                    return (seen_decl, seen_non_hash_decl, header_parsed);
                }
                self.bump_layout_virtual();
                needs_sep = false;
                // Leading layout scaffolding before the first decl — for a
                // whole-file `module`/`namespace` header this includes the body's
                // opening `OBLOCKBEGIN`. Re-base the single-`;` depth past it so
                // the body's top level reads as depth 0 (matching a nested body,
                // whose opener the caller already consumed). Once a decl is seen
                // the depth latches: a later inter-decl `OBLOCKEND` must still
                // register as a change against this baseline.
                if !seen_decl {
                    depth = 0;
                    depth_pos = self.pos;
                }
                continue;
            }

            // Anything else: either a real-token construct Phase 1 doesn't
            // parse, or a raw `Err(LexError)` that LexFilter surfaced. Push
            // a ParseError, distinguishing lex failures so consumers see the
            // specific cause (e.g. `unterminated string`) rather than the
            // generic "unexpected token". `bump_into` advances `raw_pos`
            // past the underlying raw, so the `drain_raw_up_to` `Err` arm
            // never sees this case — we have to record the lex error here.
            let message = match &res {
                Err(e) => format!("lex error: {e:?}"),
                _ => "unexpected token".to_string(),
            };
            self.errors.push(ParseError { message, span });
            self.bump_into(SyntaxKind::ERROR);
            seen_decl = true;
            seen_non_hash_decl = true;
        }
        (seen_decl, seen_non_hash_decl, header_parsed)
    }

    /// Phase 8.2 — parse a `namespace Foo.Bar` / `namespace global` /
    /// `namespace rec A.B` header into the open `MODULE_OR_NAMESPACE`.
    /// FCS's `namespaceIntro` is `NAMESPACE opt_rec path` (`pars.fsy:556`);
    /// `namespace` reaches us as a real filtered `Token::Namespace`.
    ///
    /// A bare `global` target (`namespace global`, with no following `.`)
    /// is emitted as a [`SyntaxKind::GLOBAL_TOK`] rather than a path, so the
    /// header projects to `GlobalNamespace` with an empty `longId` (FCS's
    /// post-parse pass at `ParseAndCheckInputs.fs:154-164`). Everything else
    /// — including a dotted `global.Foo` head (a documented gap; see the
    /// phase-8 sub-plan) — flows through [`Self::parse_long_ident_path`].
    pub(super) fn parse_namespace_header(&mut self) {
        // `namespace` is a real filtered token; claim it as NAMESPACE_TOK.
        self.bump_into(SyntaxKind::NAMESPACE_TOK);

        // Optional `rec` (FCS's `opt_rec`). LexFilter passes it through raw.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Rec)), _))) {
            self.bump_into(SyntaxKind::REC_TOK);
        }

        // `namespace global` (bare, not `global.Foo`) ⇒ GlobalNamespace.
        let bare_global = match self.next_non_trivia_raw_at_pos_with_span() {
            Some((Token::Global, span)) => {
                !matches!(self.next_non_trivia_raw_after(span.end), Some(Token::Dot))
            }
            _ => false,
        };
        if bare_global {
            self.bump_into(SyntaxKind::GLOBAL_TOK);
        } else {
            self.parse_long_ident_path("namespace");
        }
    }

    /// Phase 8.2 — parse a whole-file `module Foo` / `module Foo.Bar.Baz` /
    /// `module rec Foo` / `module internal Foo` header into the open
    /// `MODULE_OR_NAMESPACE`. FCS's `moduleIntro` is `moduleKeyword
    /// opt_attributes opt_access opt_rec path` (`pars.fsy:536`); attributes
    /// (`module [<…>] Foo`) are deferred to phase 10.
    ///
    /// The raw `module` keyword is *swallowed* by LexFilter, so — like
    /// [`Self::parse_open_decl`]'s recovery of the swallowed `type` — we
    /// claim it from the raw stream and emit it as [`SyntaxKind::MODULE_TOK`].
    /// Caller (`parse_optional_file_header`) has verified via
    /// [`Self::raw_leading_named_module`] that this is a no-`=` named module.
    pub(super) fn parse_named_module_header(&mut self) {
        let (kw, module_span) = self
            .next_non_trivia_raw_at_pos_with_span()
            .expect("caller verified a leading swallowed `module`");
        debug_assert!(
            matches!(kw, Token::Module),
            "parse_named_module_header invoked without a leading raw `module`",
        );
        // Drain leading trivia outside the keyword, then claim the swallowed
        // `module` directly from the raw stream (it never reached the filtered
        // stream, so `bump_into` would mark it ERROR — mirror
        // `parse_let_head_and_bindings`'s raw-`let` claim).
        self.drain_raw_up_to(module_span.start);
        self.emit_text(SyntaxKind::MODULE_TOK, module_span.clone());
        self.raw_pos += 1;

        // After-keyword attributes (phase 10.7k): `module [<A>] Foo` — FCS's
        // `moduleKeyword opt_attributes …`. They become direct children of the
        // open `MODULE_OR_NAMESPACE`, between `MODULE_TOK` and the name, and share
        // `SynModuleOrNamespace.attribs` with any leading `[<A>] module …` (10.7e).
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LBrackLess)), _))
        ) {
            self.parse_attribute_lists();
        }

        // Optional access (`internal`/`private`/`public`) and `rec`. FCS
        // fixes the order as `opt_access opt_rec`; be lenient and consume any
        // run of the two (both are passed through raw by LexFilter) — the
        // normaliser reads only `rec` and elides access, so order is
        // immaterial to the projection.
        loop {
            match self.peek() {
                Some((Ok(FilteredToken::Raw(Token::Rec)), _)) => {
                    self.bump_into(SyntaxKind::REC_TOK);
                }
                Some((
                    Ok(FilteredToken::Raw(Token::Internal | Token::Private | Token::Public)),
                    _,
                )) => {
                    self.bump_into(SyntaxKind::ACCESS_TOK);
                }
                _ => break,
            }
        }

        // The module name (NamedModule `longId`).
        self.parse_long_ident_path("module");
    }

    /// `true` iff the file *starts* with a whole-file `module M` header (no `=`),
    /// possibly preceded by a leading `[<…>]` attribute run (`[<AutoOpen>]⏎module
    /// M`, phase 10.7e). Used by the file loops to detect file-form mixing (a
    /// whole-file `module` header coexisting with a top-level `namespace`). Unlike
    /// [`Self::raw_module_head_eq`] this skips a *leading* attribute run, so it
    /// must stay separate (that method's callers rely on a leading `[<` bailing).
    /// The "a namespace actually follows" half of the mixing test is the file
    /// loop's real segment count, not a raw scan — so a stray `namespace` token in
    /// an expression / interpolation fill does not count.
    fn raw_leading_whole_file_module_head(&self) -> bool {
        let mut it = self
            .raw_tokens
            .iter()
            .map_while(|(res, _)| res.as_ref().ok())
            .filter_map(|tt| match tt {
                TriviaToken::Lexed(t) => Some(t),
                _ => None,
            })
            .filter(|t| trivia_kind(t).is_none())
            .peekable();
        // Skip a *leading* attribute run (`[<…>]` before `module`).
        while matches!(it.peek(), Some(Token::LBrackLess)) {
            it.next();
            for t in it.by_ref() {
                if matches!(t, Token::GreaterRBrack) {
                    break;
                }
            }
        }
        if !matches!(it.next(), Some(Token::Module)) {
            return false;
        }
        // After-keyword attrs, then opt_access / opt_rec (mirror raw_module_head_eq).
        while matches!(it.peek(), Some(Token::LBrackLess)) {
            it.next();
            for t in it.by_ref() {
                if matches!(t, Token::GreaterRBrack) {
                    break;
                }
            }
        }
        while matches!(
            it.peek(),
            Some(Token::Internal | Token::Private | Token::Public | Token::Rec)
        ) {
            it.next();
        }
        // First name segment (required; `module global` is not claimable).
        if !matches!(it.next(), Some(Token::Ident(_) | Token::QuotedIdent(_))) {
            return false;
        }
        while matches!(it.peek(), Some(Token::Dot)) {
            it.next();
            if !matches!(it.next(), Some(Token::Ident(_) | Token::QuotedIdent(_))) {
                break;
            }
        }
        // A whole-file header has *no* trailing `=` (that would be a nested module
        // / abbreviation — the FS0222 nested-decl-prefix path, not file mixing).
        !matches!(it.peek(), Some(Token::Equals))
    }

    /// Scan the *raw* stream from the cursor for a `module` head —
    /// `MODULE opt(access|rec)* Ident (DOT Ident)*` — and report whether a
    /// `=` follows the name. Returns `Some(true)` for a nested module /
    /// abbreviation (`module Foo = …`, phases 8.4/8.5), `Some(false)` for a
    /// whole-file `NamedModule` header (`module Foo`, phase 8.2), and `None`
    /// when there is no claimable module head (no leading `module`, or a
    /// `global`-headed module). Read-only; a lex error stops the scan and is
    /// treated as "no trailing `=`".
    ///
    /// A leading `global` is deliberately NOT accepted (returns `None`):
    /// unlike a *namespace* (where `global` is the global-namespace marker), a
    /// *module* may not be named `global`, and FCS emits **no**
    /// `SynModuleOrNamespace` for either spelling (both verified against
    /// `fcs-dump ast`):
    ///   * `module global`      → FS0244 "Invalid module or namespace name"
    ///     (`ParseAndCheckInputs.fs:129`: a sole `global` longId on a module is
    ///     rejected post-parse).
    ///   * `module global.Foo`  → FS0010 "Unexpected symbol '.'". FCS's
    ///     `CtxtModuleHead` scanner *terminates the head at `global`*, so the
    ///     parser sees `module <global>` followed by a stray `.Foo` body
    ///     statement — and the sole-`global` head then also trips FS0244.
    ///
    /// Returning `None` leaves the construct unclaimed so it errors out,
    /// matching FCS's zero-module result (pinned by
    /// `module_global_head_is_not_a_named_module`). Accepting `global` as a
    /// head would synthesise a bogus `NamedModule [global]` — a divergence.
    fn raw_module_head_eq(&self) -> Option<bool> {
        // Non-trivia raw tokens from `raw_pos`, stopping at the first lex
        // error (a `module` header can't usefully span one).
        let mut it = self
            .raw_tokens
            .iter()
            .skip(self.raw_pos)
            .map_while(|(res, _)| res.as_ref().ok())
            .filter_map(|tt| match tt {
                TriviaToken::Lexed(t) => Some(t),
                // Directive / inactive-code markers are trivia — skip past
                // them, like whitespace, when scanning the module header.
                _ => None,
            })
            .filter(|t| trivia_kind(t).is_none())
            .peekable();

        if !matches!(it.next(), Some(Token::Module)) {
            return None;
        }
        // After-keyword attributes (phase 10.7k): `module [<A>] …` — FCS's
        // `moduleKeyword opt_attributes …`. They are part of the head, so skip the
        // `[<…>]` run(s) before the name scan / `=` disambiguation (the first `>]`
        // closes each list — adequate for this lookahead; the real attribute parse
        // runs in `parse_named_module_header` / `parse_nested_module_decl_at`). A
        // *leading* `[<A>] module …` (10.7d/e) never reaches here — its first raw
        // token is `[<`, not `module`, so the `Module` check above already bailed.
        while matches!(it.peek(), Some(Token::LBrackLess)) {
            it.next(); // `[<`
            for t in it.by_ref() {
                if matches!(t, Token::GreaterRBrack) {
                    break;
                }
            }
        }
        // opt_access / opt_rec, in either order.
        while matches!(
            it.peek(),
            Some(Token::Internal | Token::Private | Token::Public | Token::Rec)
        ) {
            it.next();
        }
        // First name segment (required); a `global`-headed module is not
        // claimable (see the doc comment above).
        if !matches!(it.next(), Some(Token::Ident(_) | Token::QuotedIdent(_))) {
            return None;
        }
        // Dotted continuation `(. ident)*`.
        while matches!(it.peek(), Some(Token::Dot)) {
            it.next(); // `.`
            if !matches!(it.next(), Some(Token::Ident(_) | Token::QuotedIdent(_))) {
                break;
            }
        }
        Some(matches!(it.peek(), Some(Token::Equals)))
    }

    /// Phase 8.2 — `true` iff the leading swallowed `module` introduces a
    /// whole-file `NamedModule` (`module Foo`, no `=`), rather than a nested
    /// `module Foo = …` decl (phase 8.4) or abbreviation (phase 8.5). Thin
    /// wrapper over [`Self::raw_module_head_eq`]: a claimable module head with
    /// no trailing `=`.
    pub(super) fn raw_leading_named_module(&self) -> bool {
        self.raw_module_head_eq() == Some(false)
    }

    /// `true` iff a swallowed bare `type` keyword (a type definition, phase 9)
    /// sits at the raw cursor. Like a swallowed `module`, `type` pushes a
    /// transient LexFilter context and never reaches the filtered stream, so it
    /// is only visible on the raw stream. `open type T` is *not* matched here:
    /// `open` flows through the filtered stream and is claimed by the earlier
    /// `open` arm, so when this is consulted the next raw is `Token::Open`, not
    /// `Token::Type`.
    pub(super) fn raw_leading_type_defn(&self) -> bool {
        matches!(self.next_non_trivia_raw_at_pos(), Some(Token::Type))
    }

    /// Consume one layout virtual (`OBLOCKBEGIN` / `OBLOCKSEP` / a non-
    /// terminating `OBLOCKEND` / `ODECLEND`) at the cursor, emitting it as a
    /// zero-width `ERROR` placeholder in the tree.
    ///
    /// Differs from a plain `bump_into(ERROR)` only when a swallowed `module`
    /// keyword (a nested module / abbreviation, phases 8.4/8.5, *or* a whole-file
    /// `module Foo` header, phase 8.2 / 10.7e) or a swallowed `type` keyword (a
    /// type definition, phase 9) immediately follows: `bump_into` drains raw up to
    /// the *next filtered token* (the module's / type's name), which would consume
    /// the swallowed keyword sitting in between (it shares the virtual's byte
    /// span). Here we instead advance only `pos` and drain *no* raw, so the
    /// keyword survives for [`Self::raw_module_head_eq`] /
    /// [`Self::parse_nested_module_decl`] / [`Self::parse_named_module_header`] (or
    /// [`Self::raw_leading_type_defn`] / [`Self::parse_type_defn`]) to claim — the
    /// keyword's leading trivia is drained later by that production's
    /// `drain_raw_up_to(<keyword>_span.start)`. The module check is
    /// [`Self::raw_module_head_eq`]`.is_some()` (either `Some(true)` nested-with-`=`
    /// or `Some(false)` whole-file head): both swallow `module`, so both must be
    /// preserved.
    fn bump_layout_virtual(&mut self) {
        if self.raw_module_head_eq().is_some() || self.raw_leading_type_defn() {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
            self.pos += 1;
        } else {
            self.bump_into(SyntaxKind::ERROR);
        }
    }

    /// Phase 8.4 — parse a nested `module X = <block>` declaration into a
    /// [`SyntaxKind::NESTED_MODULE_DECL`] (`SynModuleDecl.NestedModule`,
    /// `pars.fsy:1305`). The caller has verified, via
    /// [`Self::raw_module_head_eq`] returning `Some(true)`, that a swallowed
    /// `module` head with a trailing `=` sits at the cursor.
    pub(super) fn parse_nested_module_decl(&mut self) {
        self.parse_nested_module_decl_at(None, false);
    }

    /// Parse a nested `module X = <block>` / abbreviation. With `cp = None` this
    /// is the plain form; with `cp = Some(checkpoint)` the caller has already
    /// emitted one or more leading `ATTRIBUTE_LIST`s (phase 10.7d) after the
    /// checkpoint, and this wraps them — together with the decl — so the
    /// attributes become leading children of the `NESTED_MODULE_DECL` (FCS
    /// attaches a nested module-header attribute to its
    /// `SynComponentInfo.attributes`). Mirrors [`Self::parse_type_defn_at`]. (An
    /// attributed module *abbreviation* — `[<A>] module X = Bar` — is not a valid
    /// F# form: FCS emits error 535 "Ignoring attributes on module abbreviation"
    /// and drops the decl, so here it is forced to an `ERROR` node, not a
    /// `MODULE_ABBREV_DECL`.)
    ///
    /// `sig` selects the *signature*-file shape (phase 10.13b): the body is a
    /// `SynModuleSigDecl` block ([`Self::parse_sig_module_decls`]), and `module
    /// rec` is rejected outright — FCS emits "Invalid use of 'rec' keyword" and
    /// drops the whole decl in a `.fsi` (unlike `.fs`, where `module rec X` is
    /// valid). The node kinds (`NESTED_MODULE_DECL` / `MODULE_ABBREV_DECL`) are
    /// shared with the impl side — the sig variants reuse the same nodes.
    pub(super) fn parse_nested_module_decl_at(
        &mut self,
        outer_cp: Option<rowan::Checkpoint>,
        sig: bool,
    ) {
        // `outer_cp` is the attributed entry (the dispatch parses ≥1 attribute
        // list before it), so it doubles as the "has header attributes" flag —
        // captured here before it is consumed into `cp` below.
        let attributed = outer_cp.is_some();
        let (kw, module_span) = self
            .next_non_trivia_raw_at_pos_with_span()
            .expect("caller verified a swallowed `module`");
        debug_assert!(
            matches!(kw, Token::Module),
            "parse_nested_module_decl invoked without a swallowed raw `module`",
        );
        // Keep leading trivia a sibling of the decl node (mirror
        // `parse_module_decl` / `parse_let_decl_at`), then open a checkpoint so
        // the wrapper kind — `NESTED_MODULE_DECL` vs `MODULE_ABBREV_DECL` — is
        // chosen *after* the body shape is known (FCS's `namedModuleDefnBlock`
        // disambiguation is post-parse, `pars.fsy:1427`). With `outer_cp` the
        // attribute lists already sit between that checkpoint and here, so reuse
        // it (the trivia between `>]` and `module` is then drained *inside* the
        // node, after the attrs). Claim the swallowed `module` directly from the
        // raw stream (it never reached the filtered stream, so `bump_into` would
        // mark it ERROR).
        self.drain_raw_up_to(module_span.start);
        let cp = outer_cp.unwrap_or_else(|| self.builder.checkpoint());
        self.emit_text(SyntaxKind::MODULE_TOK, module_span.clone());
        self.raw_pos += 1;

        // After-keyword attributes (phase 10.7k): `module [<A>] M = …` — FCS's
        // `moduleKeyword opt_attributes …`. They sit between `MODULE_TOK` and the
        // name (inside the node opened at `cp`) and share
        // `SynComponentInfo.attributes` with any leading `[<A>] module …` (10.7d).
        // Like a leading attribute, they make an *abbreviation* illegal (FCS
        // "Ignoring attributes on module abbreviation"), so fold them into the
        // `attributed` flag below.
        let after_kw_attrs = matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LBrackLess)), _))
        );
        if after_kw_attrs {
            self.parse_attribute_lists();
        }
        let attributed = attributed || after_kw_attrs;

        // Optional access / `rec`, in either order (same lenient run as
        // `parse_named_module_header`). A module *abbreviation* admits neither
        // (FCS: "Invalid use of 'rec' keyword" / "accessibility … not allowed on
        // module abbreviation"), so remember whether we saw them.
        let mut has_rec = false;
        let mut has_access = false;
        loop {
            match self.peek() {
                Some((Ok(FilteredToken::Raw(Token::Rec)), _)) => {
                    self.bump_into(SyntaxKind::REC_TOK);
                    has_rec = true;
                }
                Some((
                    Ok(FilteredToken::Raw(Token::Internal | Token::Private | Token::Public)),
                    _,
                )) => {
                    self.bump_into(SyntaxKind::ACCESS_TOK);
                    has_access = true;
                }
                _ => break,
            }
        }

        // The module name. For a nested module this is `SynComponentInfo.longId`
        // (may be dotted); for an abbreviation it is the single `ident` LHS — a
        // dotted LHS (`module X.Y = Z`) is an error.
        let name_span = self
            .peek()
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| self.source.len()..self.source.len());
        let lhs_segments = self.parse_long_ident_path("module");

        // `= OBLOCKBEGIN <body> OBLOCKEND`. Returns whether the body was a
        // module abbreviation (a single bare long-ident); the RHS is parsed as a
        // `LONG_IDENT` (FCS's `longId`) in that case.
        let is_abbrev = self.parse_nested_module_body(sig);

        // Pick the decl node kind. A real nested module → `NESTED_MODULE_DECL`.
        // A valid abbreviation (`module X = LongId`, simple LHS, no `rec`, no
        // access) → `MODULE_ABBREV_DECL`. An *invalid* abbreviation → an `ERROR`
        // node (not cast by `ModuleDecl`) + a diagnostic, matching FCS, which
        // emits a diagnostic and **no** decl for these forms.
        let kind = if sig && has_rec {
            // In a signature file `module rec` is rejected outright (FCS "Invalid
            // use of 'rec' keyword"), and the whole decl is dropped — whether the
            // body is a nested module or an abbreviation. Force an `ERROR` node
            // (not cast by `SigDecl`) so the projection drops it, matching FCS.
            self.errors.push(ParseError {
                message: "`rec` is not allowed on a module in a signature file".to_string(),
                span: name_span,
            });
            SyntaxKind::ERROR
        } else if !is_abbrev && lhs_segments > 1 {
            // A *nested* module head must be a simple name: `module A.B = <body>`
            // is rejected by FCS in both impl ("A module abbreviation must be a
            // simple name…") and signature ("A module name must be a simple
            // name, not a path") files, which drop the decl to an empty module.
            // Force an `ERROR` node (not cast) so the projection drops it too.
            // (The dotted *abbreviation* form — `module A.B = LongId` — is caught
            // by the `lhs_segments > 1` abbrev arm below.)
            self.errors.push(ParseError {
                message: "a nested module name must be a simple name, not a path".to_string(),
                span: name_span,
            });
            SyntaxKind::ERROR
        } else if !is_abbrev {
            SyntaxKind::NESTED_MODULE_DECL
        } else if attributed {
            // FCS error 535 "Ignoring attributes on module abbreviation"
            // (Severity Error): attributes are not a valid form on a module
            // abbreviation, and FCS drops the decl entirely. Force an `ERROR`
            // node (not cast by `ModuleDecl`, so the attribute lists + the abbrev
            // vanish from the projection — matching FCS's dropped decl) + a
            // diagnostic. Checked *before* the simple-abbrev case so an
            // attributed `[<A>] module M = N` errors rather than emitting a clean
            // `MODULE_ABBREV_DECL`.
            self.errors.push(ParseError {
                message: "attributes are not allowed on a module abbreviation".to_string(),
                span: name_span,
            });
            SyntaxKind::ERROR
        } else if lhs_segments <= 1 && !has_rec && !has_access {
            SyntaxKind::MODULE_ABBREV_DECL
        } else {
            let message = if lhs_segments > 1 {
                "a module abbreviation must be a simple name, not a path"
            } else if has_rec {
                "`rec` is not allowed on a module abbreviation"
            } else {
                "an accessibility modifier is not allowed on a module abbreviation"
            };
            self.errors.push(ParseError {
                message: message.to_string(),
                span: name_span,
            });
            SyntaxKind::ERROR
        };
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(kind));
        self.builder.finish_node();
    }

    /// Parse `= OBLOCKBEGIN <module decls> OBLOCKEND` after a nested module's
    /// name. The body reuses [`Self::parse_module_decls`] in
    /// [`BodyScope::Nested`] so it accepts every module-level decl (including
    /// further nested modules); the loop returns at the body-closing
    /// `OBLOCKEND`, which we then consume as a zero-width `ERROR` placeholder.
    ///
    /// Returns `true` iff the body is a *module abbreviation* (a single bare
    /// long-ident — see [`Self::body_is_module_abbrev`]), which the caller uses
    /// to pick the decl's node kind.
    ///
    /// `sig` (phase 10.13b) selects a *signature*-file body
    /// ([`Self::parse_sig_module_decls`] in [`BodyScope::Nested`]) instead of the
    /// impl body loop. The abbreviation RHS (a bare `longId`) is identical for
    /// both, so only the non-abbrev block branch differs.
    fn parse_nested_module_body(&mut self, sig: bool) -> bool {
        // `=`.
        match self.peek().cloned() {
            Some((Ok(FilteredToken::Raw(Token::Equals)), _)) => {
                self.bump_into(SyntaxKind::EQUALS_TOK);
            }
            other => {
                let span = other
                    .map(|(_, s)| s)
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected `=` in nested module declaration".to_string(),
                    span,
                });
                return false;
            }
        }

        // Opening `OBLOCKBEGIN` — consume as a zero-width ERROR (mirror
        // `parse_let_equals_rhs`). Absent only on malformed input; nothing to
        // parse then. Uses [`Self::bump_layout_virtual`] so that when the body
        // begins with *another* nested module (`module A =\n module B = …`),
        // the `OBLOCKBEGIN`'s drain doesn't eat the swallowed `module` keyword
        // that shares its byte span.
        if !matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _))
        ) {
            return false;
        }
        self.bump_layout_virtual();

        // Verbose-syntax body `module X = begin … end` (`wrappedNamedModuleDefn`,
        // `pars.fsy:1478`): after the `OBLOCKBEGIN` comes a real `begin`, then the
        // body decls, a real `end`, then the body-closing `OBLOCKEND`. FCS drops
        // the `begin`/`end` from the AST (it returns just the decl list), so they
        // ride as marker tokens (`BEGIN_TOK`/`END_TOK`) — the same surface
        // treatment as the explicit `class … end` repr. A `begin`-led body is
        // never a module abbreviation, so it bypasses `body_is_module_abbrev`.
        let begin_delimited =
            matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Begin)), _)));
        if begin_delimited {
            self.bump_into(SyntaxKind::BEGIN_TOK);
        }

        // A body that is exactly a bare long-ident is a *module abbreviation*
        // (`module X = LongId`), not a nested module — FCS's
        // `namedModuleDefnBlock` disambiguation (`pars.fsy:1427`), resolved
        // purely on body shape, regardless of layout. The RHS is FCS's `longId`
        // (an `Ident list`, not an expression), so we parse it as a bare
        // `LONG_IDENT` (8.5) rather than threading it through the decl loop; the
        // caller tags the decl `MODULE_ABBREV_DECL` (or an `ERROR` node for an
        // invalid abbreviation). Otherwise the body is the nested module's
        // decls, parsed by the shared loop. A `begin … end` body is never an
        // abbreviation (FCS routes it through `wrappedNamedModuleDefn`).
        let is_abbrev = !begin_delimited && self.body_is_module_abbrev();
        if is_abbrev {
            self.parse_long_ident_path("module abbreviation");
        } else if sig {
            // Signature nested-module body — the sig decl loop, terminating at
            // the body-closing `OBLOCKEND` (phase 10.13b), or — when
            // `begin_delimited` — at the verbose `end`.
            self.parse_sig_module_decls(BodyScope::Nested, false, begin_delimited);
        } else {
            // Nested-module body — the shared loop, terminating at the
            // body-closing `OBLOCKEND` (or the verbose `end`). A nested body has
            // no file-level `module`/`namespace` header, so `header_present` is
            // `false`.
            self.parse_module_decls(BodyScope::Nested, false, begin_delimited);
        }

        // An abbreviation body that did not close at the long-ident
        // (`module M = N; …`): the single `;` is FCS's `topSeparator` but does
        // not short-circuit the offside rule, so the body's `OBLOCKEND` is still
        // pending. FCS would scope the `;` and the following decls to the
        // *enclosing* module; we do not model that, so drain the trailing content
        // up to (not including) the body-closing `OBLOCKEND` as recovery — depth-
        // balanced so any blocks the trailing content opens are consumed too.
        // Leaving them for the outer loop instead would leak `M`'s `OBLOCKEND`,
        // which at `BodyScope::Nested` terminates an *enclosing* module and
        // reparents its later decls. One error keeps the failure loud.
        if is_abbrev
            && !matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _)) | None
            )
        {
            if let Some((_, span)) = self.peek() {
                let span = span.clone();
                self.errors.push(ParseError {
                    message: "unexpected token after module abbreviation".to_string(),
                    span,
                });
            }
            let mut nested = 0i32;
            loop {
                match self.peek() {
                    Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _)) if nested == 0 => {
                        break;
                    }
                    Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _)) => nested += 1,
                    Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _)) => nested -= 1,
                    Some(_) => {}
                    None => break,
                }
                self.bump_into(SyntaxKind::ERROR);
            }
        }

        // The verbose-syntax closing `end` (a real filtered token), sitting
        // between the body decls and the body-closing `OBLOCKEND`. A missing
        // `end` is FCS's `BEGIN … recover` (`parsUnmatchedBeginOrStruct`); record
        // a clean error and fall through to the `OBLOCKEND` consumption.
        if begin_delimited {
            if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::End)), _))) {
                self.bump_into(SyntaxKind::END_TOK);
            } else {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected `end` to close the `begin … end` module body".to_string(),
                    span,
                });
            }
        }

        // Consume the body-closing `OBLOCKEND` as a zero-width ERROR
        // placeholder advancing only `pos` (the `parse_if_body` discipline):
        // the raw cursor stays put so trailing trivia / a LexFilter-swallowed
        // close stays in the enclosing scope.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _))
        ) {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
            self.pos += 1;
        }

        is_abbrev
    }

    /// `true` iff the nested-module body — positioned just after its opening
    /// `OBLOCKBEGIN` — is exactly a bare long-ident, FCS's
    /// `namedModuleDefnBlock` module-abbreviation case (`pars.fsy:1427`: a body
    /// of one `SynModuleDecl.Expr(LongOrSingleIdent(false, path, None, _))`
    /// projects to `ModuleAbbrev`, regardless of layout). Read-only
    /// filtered-stream scan: `Ident (DOT Ident)*` immediately followed by the
    /// body-closing `OBLOCKEND`. Anything else (an application argument, an
    /// operator, a leading `let`/`open`/virtual, …) is a nested module. A
    /// leading `global` is accepted as a path head (`module M = global.System`),
    /// mirroring [`Self::parse_long_ident_path`].
    ///
    /// **Swallowed-keyword guard:** LexFilter removes a leading `type` / `module`
    /// from the *filtered* stream (pushing a transient context), so a one-decl
    /// body like `module M =`⏎`  type T` (a signature opaque type) would otherwise
    /// look like the bare longident `T` (the type's name) and be misclassified as
    /// `module M = T`. An abbreviation's RHS begins with a real identifier, never
    /// a swallowed keyword — so the *raw* stream's first significant token must be
    /// an identifier head, else this is a decl body (handled by the body loop),
    /// not an abbreviation.
    ///
    /// A single `;` between the long-ident and the body `OBLOCKEND`
    /// (`module M = N; open X`) does not close the block, so the path is not
    /// immediately followed by `OBLOCKEND`; FCS still classifies the body as a
    /// `ModuleAbbrev` (the `;` is a `topSeparator`), so this returns `true` too.
    /// We do not model the following sibling decls — a fully FCS-faithful
    /// `module M = N; open X` needs the abbreviation body's block extent reworked
    /// (the `;` and `open` would be the *enclosing* scope's siblings). Instead
    /// [`Self::parse_nested_module_body`] drains the trailing content and the
    /// body's `OBLOCKEND` as recovery, so the divergence is loud and contained
    /// *inside* `M`, not a silent reparent and not a leaked `OBLOCKEND` that
    /// terminates an enclosing module (see the `single-semicolon` test module). A
    /// `;;`, by contrast, *does* close the block, so `module M = N;;` reaches the
    /// `OBLOCKEND` arm and the `;;` is left as the outer scope's separator.
    fn body_is_module_abbrev(&self) -> bool {
        if !matches!(
            self.next_non_trivia_raw_at_pos(),
            Some(Token::Ident(_) | Token::QuotedIdent(_) | Token::Global)
        ) {
            return false;
        }
        let mut it = self
            .filtered_tokens
            .iter()
            .skip(self.pos)
            .filter_map(|(res, _)| res.as_ref().ok())
            .filter(|ft| !matches!(ft, FilteredToken::Raw(t) if trivia_kind(t).is_some()));

        // First path segment must be an identifier (or the `global` head).
        if !matches!(
            it.next(),
            Some(FilteredToken::Raw(
                Token::Ident(_) | Token::QuotedIdent(_) | Token::Global
            ))
        ) {
            return false;
        }
        loop {
            match it.next() {
                // Path complete and the body closes → abbreviation. A `;;`
                // short-circuits the offside rule, so its body-closing
                // `OBLOCKEND` lands here *before* the `;;` — `module M = N;;`
                // and `module M = N;; open X` reach this arm unchanged.
                Some(FilteredToken::Virtual(Virtual::BlockEnd)) => return true,
                // Path complete and a single `;` follows, still inside the body
                // block (`module M = N; …`): FCS classifies the body as a
                // `ModuleAbbrev` (the `;` is a `topSeparator`), so this is an
                // abbreviation too. The block does not close at the `;`, so the
                // trailing content (`; …`) and the body's `OBLOCKEND` are drained
                // as recovery by [`Self::parse_nested_module_body`] before it
                // returns — keeping the failure loud and *inside* `M` rather than
                // leaking `M`'s `OBLOCKEND` to terminate an enclosing module.
                Some(FilteredToken::Raw(Token::Semi)) => return true,
                // `. ident` continuation.
                Some(FilteredToken::Raw(Token::Dot)) => {
                    if !matches!(
                        it.next(),
                        Some(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_)))
                    ) {
                        return false;
                    }
                }
                // Application argument, operator, HPA virtual, … → nested.
                _ => return false,
            }
        }
    }
}
