//! `textDocument/completion` — member (`recv.`) completion.
//!
//! Stage 3.3b: when the cursor sits **after a dot** in a member access
//! (`s.`, mid-`s.Le`, `"hi".Le`), offer the **receiver type's public instance
//! members** — fields, non-indexer readable properties, *and* methods. A
//! completion list is a set of genuinely-callable candidates, not a type
//! assertion, so methods belong even though inference (Stage 3.3a) only *types*
//! data members. The receiver type comes from inference (the receiver's
//! inferred/binder type); its [`borzoi_sema::Ty::Named`] head is resolved
//! through the project's [`AssemblyEnv`] to the entity's member list.
//!
//! **Soundness (D5: silence over noise).** A completion list requires a
//! **ground** receiver type. An open / deferred receiver (an unannotated
//! parameter, a receiver inference could not type) offers **nothing** — never a
//! guess. Only *public instance* members are offered; static members (`Empty`)
//! need a type-qualified path, not a value receiver, so they are excluded, and
//! a `private get` / write-only property is excluded (unreadable). No base-class
//! walk yet — exact-entity members only (as Stage 3.3a).
//!
//! **Parse-shape coverage** (probed against the CST — see
//! `docs/sema-phase3-impl-plan.md` §3.3b). The two *partial-member* shapes parse
//! cleanly and are fully covered: `s.Le` is a `LONG_IDENT_EXPR`
//! (`[IDENT s, DOT, IDENT Le]`) and `"hi".Le` a `DOT_GET_EXPR` (receiver
//! expression + member path). The *trailing-dot* `s.` also recovers as a
//! `LONG_IDENT_EXPR` (`[IDENT s, DOT]`) whose receiver token is still present, so
//! it is covered via the receiver binder's type. The literal trailing-dot `"hi".`
//! is **scoped out**: under error recovery its `DOT_GET_EXPR` collapses — the
//! receiver becomes a bare sibling `CONST_EXPR` and the `DOT` an orphan — so no
//! member-access node survives to anchor the receiver (a parser-side follow-up,
//! not hacked here).

use borzoi_cst::syntax::{
    AstNode, DotGetExpr, Expr, ImplFile, LongIdentExpr, SyntaxKind, SyntaxNode, SyntaxToken,
};
use borzoi_sema::{AssemblyEnv, InferredFile, ResolvedFile, Ty, infer_file};
use lsp_types::{CompletionItem, CompletionParams, CompletionResponse, Position};
use rowan::TextSize;

use crate::paths::{lexically_normalize, paths_equal};
use crate::position::position_to_offset;
use crate::server::State;

/// Run the completion handler. Returns member candidates for a `recv.`
/// member-access position whose receiver has a **ground** inferred type resolving
/// to a referenced-assembly entity; `None` for every other position (D5: silence
/// over noise). Project-only in v1 — member completion needs the project's
/// `AssemblyEnv` to enumerate members, which an orphan buffer has no way to build.
pub fn handle(state: &mut State, params: CompletionParams) -> Option<CompletionResponse> {
    let pos = params.text_document_position.position;
    let uri = params.text_document_position.text_document.uri.clone();
    let text = state.docs.get(&uri).cloned()?;
    let names = member_candidates(state, &uri, &text, pos)?;
    if names.is_empty() {
        return None;
    }
    // No `kind` icon is claimed: the candidate set mixes fields, properties, and
    // methods, and we do not carry the per-member kind here (a later enrichment —
    // `member_kind_for` — could set it). A wrong icon is worse than none.
    let items = names
        .into_iter()
        .map(|name| CompletionItem {
            label: name,
            ..CompletionItem::default()
        })
        .collect();
    Some(CompletionResponse::Array(items))
}

/// The public instance member names offered at `pos`, or `None` when the position
/// is not a completable `recv.` member access against a ground receiver type in an
/// evaluated project.
fn member_candidates(
    state: &mut State,
    uri: &lsp_types::Url,
    text: &str,
    pos: Position,
) -> Option<Vec<String>> {
    let path = uri.to_file_path().ok()?;
    let project = state.workspace.owning_project(&path)?;
    let State {
        semantic,
        workspace,
        docs,
        ..
    } = state;
    // Size the fold to this file's Compile index *first*, so we fold only the
    // prefix up to it: F# is order-sensitive, so the receiver's type and every
    // in-scope member candidate are declared at an index `<= file_idx` — the
    // suffix fold can't add a candidate this file can see.
    let parses = semantic
        .parses_for_project(&project, workspace, docs)?
        .clone();
    let file_idx = parses
        .paths
        .iter()
        .position(|p| paths_equal(&lexically_normalize(p), &lexically_normalize(&path)))?;
    let resolved = semantic.resolved_prefix_for(&project, file_idx, workspace, docs)?;
    let file = resolved.file(file_idx);
    // Completion needs the file's implementation tree; a `.fsi` Compile slot
    // is inert in Stage 1, so decline (no member completion on signatures).
    let impl_file = parses.files[file_idx].file.as_impl()?;

    let byte = position_to_offset(text, pos);
    // Find the receiver expression whose member is being completed at `byte`.
    let receiver = receiver_at(impl_file, byte)?;

    // Inference gives the receiver's ground type. Its `AssemblyEnv` is the same
    // input the resolver/inference use; caching amortises the DLL reads.
    let dotnet_root = workspace.dotnet_root_for_project(&project);
    let target_framework = workspace.served_tfm_for_project(&project);
    let env = semantic.assembly_env_for_project(
        &project,
        dotnet_root.as_deref(),
        &target_framework,
        workspace,
    );
    let inferred = {
        let _span = tracing::info_span!("infer_file").entered();
        infer_file(impl_file, file, &env)
    };

    let recv_ty = receiver_type(&receiver, file, &inferred)?;
    // A ground `Ty::Named` receiver resolves to an entity; anything else (open,
    // an array / tuple / function head, a generic named type) offers nothing.
    let names = members_of(&env, &recv_ty)?;
    Some(names.into_iter().map(str::to_string).collect())
}

/// The receiver expression of a member access at `byte` — the `recv` in
/// `recv.<member-being-typed>`. Handles the three covered parse shapes; `None`
/// for every other position (not a member access, an unsupported receiver, or the
/// scoped-out literal trailing-dot). See the module docs for the shape survey.
fn receiver_at(file: &ImplFile, byte: usize) -> Option<Receiver> {
    let byte = TextSize::try_from(byte).ok()?;
    let root = file.syntax();
    // The token just left of the cursor anchors the position: completion fires
    // when it is the dot itself (`s.`) or a member ident following a dot
    // (`s.Le`). `token_at_offset` yields both sides at a boundary; prefer the one
    // that ends *at* the cursor (the token we are completing after).
    //
    // A **trivia** left token means the cursor is not adjacent to any code. The
    // only completable position reached *through* trivia is a trailing dot
    // (`s. |` — the dot is still the member-access edit point); an ident behind
    // the trivia (`let n = s.Length |`, or the next line) is a **completed**
    // access the cursor has left, and offering the receiver's members there is
    // noise, not help (a codex round-3 finding) — decline (silence over noise).
    let raw_left = raw_token_left_of(root, byte)?;
    let left = if raw_left.kind().is_trivia() {
        let tok = prev_non_trivia(&raw_left)?;
        if tok.kind() != SyntaxKind::DOT_TOK {
            return None;
        }
        tok
    } else {
        raw_left
    };
    match left.kind() {
        // Trailing dot (`s.`) or partial member (`s.Le`): the anchoring dot is
        // `left` itself, or the dot immediately preceding the member ident.
        SyntaxKind::DOT_TOK => receiver_before_dot(&left),
        SyntaxKind::IDENT_TOK => {
            // The member ident being typed — its immediately-preceding non-trivia
            // sibling in the path must be a dot, else this is a bare name (not a
            // member access) and completion does not fire here.
            let dot = prev_non_trivia(&left)?;
            if dot.kind() != SyntaxKind::DOT_TOK {
                return None;
            }
            receiver_before_dot(&dot)
        }
        _ => None,
    }
}

/// A member-access receiver, in one of the two shapes that survive parsing
/// (after transparent parentheses are peeled — see [`receiver_of_expr`]).
enum Receiver {
    /// A value-binder receiver: the head ident of a `LONG_IDENT_EXPR`
    /// (`s` in `s.Le` / `s.`) or a paren-peeled ident receiver (`(s).Le`). Its
    /// type is read from the binder's `def_type`.
    IdentHead(SyntaxToken),
    /// Any other receiver *expression* of a `DOT_GET_EXPR` (`"hi"` in `"hi".Le`):
    /// its type is read from the inferred expression-type map at `range` — the
    /// range inference keys the expression's emission under (the literal *token*
    /// for a `Const`, the node range otherwise).
    Expr(rowan::TextRange),
}

/// The receiver anchored by a member-access `dot` token. The dot always lives
/// inside a `LONG_IDENT`; that path's parent distinguishes the two shapes:
///
/// - parent `LONG_IDENT_EXPR` (`s.Le` / `s.`): the receiver is the path's **head
///   ident** (`s`), a value binder.
/// - parent `DOT_GET_EXPR` (`"hi".Le`): the receiver is the `DOT_GET_EXPR`'s inner
///   **expression** (`"hi"`).
///
/// In **both** shapes `dot` must be the path's **first** dot, so the receiver is
/// exactly the head ident / inner expression. A completion after a *later* dot
/// (`s.A.`, `"hi".Length.`) has a multi-segment receiver (`s.A`, `"hi".Length`)
/// whose type this handler does not chain through, so it **declines** rather than
/// wrongly offer the head's members — the silence-over-wrong-list rule (D5).
///
/// `None` if the dot is not inside either shape (or the shape is malformed).
fn receiver_before_dot(dot: &SyntaxToken) -> Option<Receiver> {
    let long_ident = dot.parent()?;
    if long_ident.kind() != SyntaxKind::LONG_IDENT {
        return None;
    }
    // Only the *first* dot of the path anchors a head receiver; a later dot has a
    // multi-segment receiver we cannot type here.
    let first_dot = long_ident
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::DOT_TOK)?;
    if first_dot != *dot {
        return None;
    }
    let path_parent = long_ident.parent()?;
    match path_parent.kind() {
        SyntaxKind::LONG_IDENT_EXPR => {
            let long_ident_expr = LongIdentExpr::cast(path_parent)?;
            let head = long_ident_expr
                .long_ident()?
                .syntax()
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .find(|t| t.kind() == SyntaxKind::IDENT_TOK)?;
            Some(Receiver::IdentHead(head))
        }
        SyntaxKind::DOT_GET_EXPR => {
            let dot_get = DotGetExpr::cast(path_parent)?;
            receiver_of_expr(dot_get.expr()?)
        }
        _ => None,
    }
}

/// Classify a `DOT_GET_EXPR` receiver *expression*, peeling transparent
/// **parentheses** first — inference records a paren's type on its *inner*
/// expression (parens carry no node of their own), so looking up the `Paren`
/// node's range would wrongly decline `("hi").Le` / `(s).Le` (a codex round-2
/// finding). A peeled **ident** receiver is a value binder (typed via
/// `def_type`, like a `LONG_IDENT_EXPR` head); a **literal** is keyed by its
/// token range (inference's emission key); any other expression by its node
/// range. `None` on a malformed shape (an empty paren recovery hole, an
/// ident/const with no token).
fn receiver_of_expr(mut expr: Expr) -> Option<Receiver> {
    loop {
        match expr {
            Expr::Paren(p) => expr = p.inner()?,
            Expr::Ident(ident) => return Some(Receiver::IdentHead(ident.ident()?)),
            Expr::Const(c) => return Some(Receiver::Expr(c.literal()?.text_range())),
            other => return Some(Receiver::Expr(other.syntax().text_range())),
        }
    }
}

/// The receiver's **ground** inferred type, or `None` (D5 silence): an open /
/// deferred receiver offers no completions.
///
/// - An `IdentHead` receiver resolves to an in-file binder; its type is the
///   binder's `def_type` (a value/parameter typed by inference).
/// - An `Expr` receiver's type is the inferred expression type at its key range.
fn receiver_type(receiver: &Receiver, file: &ResolvedFile, inferred: &InferredFile) -> Option<Ty> {
    match receiver {
        Receiver::IdentHead(head) => {
            let res = file.resolution_at(head.text_range())?;
            let def = file.resolved_def_id(res)?;
            inferred.def_type(def).cloned()
        }
        Receiver::Expr(range) => inferred.type_at(*range).cloned(),
    }
}

/// The public instance member names of a ground `Ty::Named` receiver, resolved
/// through `env`. `None` (offer nothing) for a non-`Named` head, a generic /
/// nested named type we cannot resolve to an arity-0 entity, or an entity absent
/// from the env. Duplicates (overloaded method names) are already deduplicated by
/// [`AssemblyEnv::public_instance_member_names`].
fn members_of<'e>(env: &'e AssemblyEnv, ty: &Ty) -> Option<Vec<&'e str>> {
    let Ty::Named(path) = ty else {
        return None;
    };
    let (type_name, namespace) = path.split_last()?;
    // Arity 0 — a non-generic receiver, exactly Stage 3.3a's `HasMember` lookup.
    let handle = env.lookup_type(namespace, type_name, 0)?;
    Some(env.public_instance_member_names(handle))
}

/// The **raw** token ending at or containing `byte`, preferring the one whose
/// range ends exactly at the cursor (the token we completed after). May be
/// trivia — the caller ([`receiver_at`]) decides whether trivia is skippable
/// (only toward a trailing dot). `None` at the very start of the file.
fn raw_token_left_of(root: &SyntaxNode, byte: TextSize) -> Option<SyntaxToken> {
    // `token_at_offset` returns up to two tokens at a boundary. Take the token to
    // the *left* (ending at `byte`); if the cursor is inside a token, that token.
    match root.token_at_offset(byte) {
        rowan::TokenAtOffset::None => None,
        rowan::TokenAtOffset::Single(t) => Some(t),
        rowan::TokenAtOffset::Between(left, _right) => Some(left),
    }
}

/// The nearest preceding non-trivia token in the whole tree, if any.
fn prev_non_trivia(tok: &SyntaxToken) -> Option<SyntaxToken> {
    let mut cur = tok.prev_token();
    while let Some(t) = cur {
        if !t.kind().is_trivia() {
            return Some(t);
        }
        cur = t.prev_token();
    }
    None
}
