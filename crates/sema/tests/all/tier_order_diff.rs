//! Differential sweep: **the tier ladder itself**, against FCS.
//!
//! Every other type-position differential in this crate probes one surface and
//! takes the ladder as given. That ladder — which scope wins when several make
//! the same bare name visible — has been restated three times (#181, #187, and
//! the contested arm this group's sibling commit fixes), each time because a
//! review round found a scope the previous statement had not considered. The
//! systematic answer is to stop asserting the ladder and start *measuring* it.
//!
//! The corpus ([`crate::common::tier_corpus`]) plants a distinct simple name
//! for every singleton and every unordered pair of [`Tier`]s — and, for the
//! risk families, for every tier paired with a
//! [`Risk`](crate::common::tier_corpus::Risk) — and the probe template is
//! uniform, so one case puts exactly two things in contention (two visible
//! scopes, or one visible scope and one hidden entity) with no third party to
//! confound the answer. Each case runs under **both** reference orders, because
//! FCS has no fixed manifest-surface/root tier at all: an assembly's
//! root-namespace contents and its `[<assembly: AutoOpen>]` targets both enter
//! the name environment when that assembly is imported, so reference order
//! decides.
//!
//! The whole matrix repeats per generic-arity shape
//! ([`crate::common::tier_corpus::Arity`]), because the resolver's type-position
//! lookup is arity-keyed and FCS's arity preference is a *fallback rather than
//! a filter*: the tier that wins a name is not necessarily the tier that wins
//! it when nothing holds the written arity.
//!
//! Three properties ride on the same matrix. The first is the crate's usual
//! **certain-implies-exact**: whenever we commit an entity for the probed name,
//! FCS's `(assembly, full name)` must agree exactly.
//!
//! The second separates a deferral from a **denial**, which a
//! certain-implies-exact oracle alone cannot do: `resolve_type_path` records
//! nothing at all on a genuine no-match, and for a single segment that silence
//! is not an absence of opinion but the resolver's positive claim that *no
//! shadow is possible* — a signal downstream consumers read. So a bare
//! (one-segment) plant denied while FCS binds something is a divergence in its
//! own right. Without that property a whole class of branch is invisible here:
//! the resolver's arity-fallback arm exists precisely to turn such a no-match
//! into a deferral when a manifest surface holds the written name at another
//! arity, and deleting the arm outright moves not one case of the first
//! property.
//!
//! The third is the **cost** side. Both properties above are one-sided by
//! construction: a deferral makes no claim, so no oracle built on
//! certain-implies-exact can see one, and a veto that gets *stronger* — that
//! starts declining a case it used to bind — leaves them green. Every shadow
//! risk the resolver models is paid for in declines, so [`KNOWN_DEFERRALS`]
//! records each case where we decline and FCS binds, and the channel that
//! declined. Without it the only measurement of a veto's cost is a printed
//! integer.
//!
//! That channel is the resolver's own
//! [`DeclineSite`] — which guard spoke and the ladder tier it spoke from — not
//! a label the table asserts by hand. It rides in the ratchet key, so a change
//! that keeps a case deferring while moving *which* guard declined it fails
//! here. That is exactly the shape of a reorder of the ladder, and it is the
//! measurement the three attempts at one lacked: each was priced by disabling
//! one guard at a time and re-running a whole-project differential, because the
//! aggregate could not say which model owned which share of the loss.
//!
//! [`Risk`](crate::common::tier_corpus::Risk) is the dimension that makes that
//! measurable at all. The walk's vetoes are keyed by the namespace prefix a
//! risk lives in but ranked by where that prefix sits in the walk, and until a
//! risk is planted at a namespace the walk visits *as a prefix*, none of them
//! fires here.
//!
//! All three properties are a **two-sided ratchet** ([`KNOWN_DIVERGENCES`],
//! [`WRONG_ARITY_DENIALS`], [`KNOWN_DEFERRALS`]): a case in a table must still
//! land the way it says, and a case outside them must agree. So fixing one of
//! the modelling errors they record fails this test until the entry is removed,
//! and a regression that reintroduces one fails it too. Every row states the
//! **verdict** it expects — what we said against what FCS said — and that
//! triple, not the case key, is the ratchet's identity: a recorded denial that
//! decays into a wrong-target commit keeps its case key, as does a decline that
//! hardens into a wrong commit, so a key-only ratchet would let the tables
//! satisfy each other.

use std::collections::{BTreeMap, BTreeSet};

use crate::common::tier_corpus::{self, Plant, Tier};
use crate::common::{
    ensure_tier_corpus_built, invoke_fcs_dump_with_refs, parse_fcs_uses, temp_fs_file,
};
use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, DeclineSite, ProjectItems, Resolution, SyntaxRecovery, resolve_file,
};
use rowan::TextRange;

/// Which assembly the probe references first. FCS imports references in this
/// order and the name environment is last-write-wins, so it is a *dimension*,
/// not an implementation detail.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Order {
    ContributorFirst,
    DecoyFirst,
}

impl Order {
    const ALL: [Order; 2] = [Order::ContributorFirst, Order::DecoyFirst];

    fn label(self) -> &'static str {
        match self {
            Order::ContributorFirst => "contributor-first",
            Order::DecoyFirst => "decoy-first",
        }
    }
}

/// The divergences this sweep records but does not fix, each with the scope
/// error it is a symptom of. Keyed `"<plant>/<order>"`.
///
/// Every row is independently FCS-verified with hand-built DLLs before being
/// recorded, so it states something about the compiler rather than rubber-stamps
/// whatever the suite happened to print.
const KNOWN_DIVERGENCES: &[(&str, &str, &str, &str)] = &[
    // What is left of the ladder error is the **implicit-open/root boundary**,
    // and it is not an ordering we can get right: an assembly's root contents
    // and its manifest `[<assembly: AutoOpen>]`s both enter the name environment
    // at that assembly's import (`CheckDeclarations.fs`'s `AddCcuToTcEnv`, folded
    // over the references), so *reference order* decides between two assemblies
    // and no fixed rank reproduces it. The rows below are that boundary; the
    // decoy-first twin of each agrees only because our fixed answer happens to
    // be FCS's in that order.
    //
    // The other half of the error — the implicit opens ranked above the
    // **enclosing namespace** — is fixed. The compiler settles it directly:
    // `ImplicitlyOpenOwnNamespace` ("Inside `namespace X.Y.Z` there is an
    // implicit open of `X.Y.Z`") runs after the whole `AddCcuToTcEnv` fold and
    // before the file's own `open`s, and FCS's name environment is
    // last-write-wins, so the enclosing namespace outranks every implicit open
    // and yields only to explicit source `open`s.
    //
    // Its price is a **decline**, never a wrong target, and it is measured
    // rather than argued: the whole-project differential loses 48 assembly
    // agreements on the six pinned projects (3977 → 3929) and 661 on
    // WoofWare.PawPrint (13888 → 13227), with divergences unmoved in both
    // (2026-08-02). The decline census attributes it to one missing model, not
    // to the ladder: FCS merges a project module with a same-named assembly
    // namespace **per member** (`ProjectPathShadow`, 331 of PawPrint's;
    // `ProjectTypePathShadow`, 37), so a project `module List` in the enclosing
    // namespace now preempts `Microsoft.FSharp.Collections` for the whole path
    // instead of falling through member by member. `AssemblyAutoOpenShadow` —
    // the channel a rank fix would have addressed — is **zero** on both corpora
    // before and after. That merge is the next slice, and the census sizes it.
    //
    // The one part of it the reorder could not wait for is the **self-qualifier**
    // reading, because `module List = …` inside `namespace N` is ordinary F#:
    // the enclosing-namespace reading `N.List` is the current module, which FCS
    // does not bind (FS0039). `assembly_path_records` now declines there only
    // when the assemblies hold a rooting position for the reading — the case
    // where an assembly entity really might answer it (`Calc.Zero()` inside
    // `Demo.Calc` binds the assembly's `Demo.Calc.Zero`) — and treats a reading
    // the assemblies cannot root at all as the no-match it is, so the walk
    // carries on to FSharp.Core.
    //
    // We rank the implicit-open tier above root unconditionally. FCS has no
    // such tier: the root contents of an assembly and its manifest auto-opens
    // both enter the name environment at that assembly's import, so reference
    // order decides. The decoy-first twin of this case agrees only because our
    // fixed answer happens to be FCS's in that order.
    (
        "TNsRo/contributor-first",
        "NsAuto",
        "Root",
        "implicit-open vs root is decided by reference order in FCS; our ladder fixes it",
    ),
    (
        "DNsRo/contributor-first",
        "NsAuto",
        "Root",
        "as TNsRo, reached as a dotted head: the tier error is not form-specific",
    ),
    // The arity-1 twins of the error above. It reproduces unchanged one arity
    // up, which is the evidence that it is a tier error rather than anything the
    // arity-keyed lookup introduces.
    (
        "GNsRo/contributor-first",
        "NsAuto",
        "Root",
        "as TNsRo at arity 1: the reference-order error is not arity-specific",
    ),
    (
        "HNsRo/contributor-first",
        "NsAuto",
        "Root",
        "as TNsRo, dotted head at arity 1: the error is neither form- nor arity-specific",
    ),
    // The same error reached through the `Risk` dimension, where the tier that
    // should win holds a *hidden* entity rather than a visible plant. Everything
    // above needs the name declared at two visible tiers; these need it at one,
    // with the winner behind an assembly `[<AutoOpen>]` module. So the error is
    // not an artefact of two visible declarations contending — and the wrong
    // commit here is strictly worse than the pair cases, because the veto that
    // exists to catch exactly this hidden entity never runs: the implicit-open
    // tier resolves and returns before the prefix carrying the risk is ever
    // visited.
    (
        "SNsYRo/contributor-first",
        "NsAuto",
        "Hidden@Root",
        "as TNsRo, against a hidden winner: our fixed implicit-open-over-root rank commits before \
         the root tier's auto-open module is looked at",
    ),
    (
        "VNsMRo/contributor-first",
        "NsAuto",
        "Hidden@Root",
        "as SNsYRo, reached as a dotted head",
    ),
    // The project `[<AutoOpen>]` channel reached as a *dotted head*, at the two
    // tiers our walk visits **before** the namespace the risk lives in.
    //
    // `project_shadow_at` answers per walk prefix, and a project auto-open
    // module in the probe's own namespace is found only at the
    // enclosing-namespace prefix. So a plant at the explicit or implicit open
    // tier is committed before the channel is ever consulted, while the same
    // plant at the enclosing or root tier declines correctly (the `VEnQ`/`VRoQ`
    // rows of `KNOWN_DEFERRALS`).
    //
    // That is the rank/keying conflation with the operands swapped: here it is
    // the *risk* that is under-ranked, not over-ranked. Whether a project
    // `[<AutoOpen>]` module is in scope is a fact about the **file**, not about
    // any assembly prefix — the walk's prefix loop is simply the wrong place to
    // ask it. Hoisting the question out of the loop is not a reorder either: a
    // preemptive veto asked once, before any tier, fires for every file holding
    // such a module, which is the cost the rank work exists to bound. So these
    // two wait on it: only the **explicit**-open plant is left, because the
    // enclosing namespace now out-ranks the implicit opens and so is reached
    // before an implicit-open plant can commit (the `VNsQ` rows of
    // `KNOWN_DEFERRALS`).
    (
        "VExQ/contributor-first",
        "Explicit",
        "Hidden@Project",
        "a project [<AutoOpen>] module owns this dotted head, but it lives in the enclosing \
         namespace and the plant sits at the explicit open, which our walk commits first",
    ),
    (
        "VExQ/decoy-first",
        "Explicit",
        "Hidden@Project",
        "a project [<AutoOpen>] module owns this dotted head, but it lives in the enclosing \
         namespace and the plant sits at the explicit open, which our walk commits first",
    ),
];

/// Divergences of the **second** property: cases where we deny that anything
/// could bind and FCS binds something.
///
/// Empty, and kept so that the property stays checked rather than becoming a
/// branch nothing reaches. A denial is the resolver's *claim* that no shadow is
/// possible, which a consumer reads as licence to act; the whole point of
/// separating it from a deferral is that this table can be required to stay
/// empty. Any row appearing here is a wrong claim, not a lost binding, and
/// wants a fix rather than an entry.
///
/// One error per row when there are any, enumerated rather than derived from a
/// predicate — a generated table would quietly re-fit itself around a partial
/// fix, and the ratchet's job is to make each case an individual commitment.
const WRONG_ARITY_DENIALS: &[(&str, &str)] = &[];

/// The shared reasons for [`KNOWN_DEFERRALS`], one per channel that produces
/// them. Each is stated once because every row it labels is the same modelling
/// gap reached from a different case.
mod decline {
    /// The hidden entity could bind the probe's form, but it sits at the
    /// implicit-open tier and FCS binds the *visible* plant above it. A
    /// `ShadowVeto::Vetoed` verdict has no rank of its own: it ends the whole
    /// walk from wherever the prefix carrying the risk happens to
    /// sit. Only the **root**-tier plants still reach this, since the ladder
    /// now visits the implicit opens after the enclosing namespace and before
    /// the root, and the implicit-open/root boundary is the one FCS decides by
    /// reference order rather than by rank.
    pub const RISK_BELOW_THE_PLANT: &str = "the hidden entity is real but enters scope below the tier FCS binds; the veto carries no \
         rank, so reaching its prefix first ends the walk before the winning tier is tried";

    /// Risk and plant share the root tier and FCS binds the *plant*. At root an
    /// auto-open module's type loses a bare name to the namespace's direct
    /// type — though the same module wins the *dotted* form there (`VRoMRo`)
    /// and wins the bare form at the enclosing namespace (`SEnYEn`). Measured,
    /// not derived: the three answers differ, so no single rule about
    /// auto-open modules covers them.
    pub const RISK_LOSES_AT_ROOT: &str = "the hidden entity shares the root tier with the plant and FCS binds the plant's direct \
         declaration; the veto cannot express a same-tier loss, so it declines";

    /// `auto_open_modules_in_namespace_shadow_type_named` matches a *name*
    /// among an auto-open module's public children — not a shape, and not a
    /// reading. The name is really there, so the veto is not wrong to fire; but
    /// a module cannot bind a bare type reference and a type cannot own a
    /// dotted tail, and in those two cells FCS falls straight through to the
    /// visible plant.
    pub const NAME_KEYED_SHAPE_BLIND: &str = "an assembly [<AutoOpen>] module declares this name, so the exact channel vetoes — but \
         what it declares cannot bind the probe's form, and FCS falls through to the visible tier";

    /// The project channel's twin of [`HIDDEN_WINS`], and the cell that tells a
    /// *narrowed* project veto from a switched-off one: the project
    /// `[<AutoOpen>]` module really does declare this name, and FCS binds it
    /// over every assembly tier.
    pub const PROJECT_HIDDEN_WINS: &str = "the project [<AutoOpen>] module declares this very name and FCS binds it — the deferral \
         is the right answer, and a veto narrowed by name must keep making it";

    /// A **correct** decline: FCS really does bind the hidden entity. Sema does
    /// not model an assembly `[<AutoOpen>]` module's nested types, so deferring
    /// is the right answer — the row is here so that a change which starts
    /// committing has to say so.
    pub const HIDDEN_WINS: &str = "FCS binds the hidden entity, which sema does not model — the deferral is the right \
         answer and the row exists to stop it silently becoming a commit";

    /// The dotted twin of [`WRONG_ARITY_DENIALS`](super::WRONG_ARITY_DENIALS):
    /// no tier holds the written arity, so the arity-keyed walk finds nothing,
    /// and on a *dotted* path recording nothing makes no claim at all.
    pub const ARITY_FALLBACK: &str = "no tier holds the written arity, so the arity-keyed walk finds nothing; FCS's arity \
         preference is a fallback, not a filter, so it binds a wrong-arity occupant";

    /// A module-shaped manifest auto-open surface is modelled as a retained
    /// surface rather than a walk prefix, and is deferred to rather than
    /// committed — the design, and still a decline where FCS binds.
    pub const MANIFEST_SURFACE: &str = "a module-shaped manifest auto-open is among the contenders; the resolver models it as a \
         retained surface and defers to it rather than committing";

    /// A *contested* manifest auto-open — a namespace the contributor
    /// auto-opens and the decoy also declares — is dropped from the opens
    /// entirely rather than scoped to the contributing assembly's own content,
    /// so nothing models the tier.
    pub const CONTESTED_DROPPED: &str = "a contested manifest auto-open is among the contenders; the env drops it from the opens \
         entirely rather than scoping it to the contributing assembly's content";
}

/// The sentinel `ours` of a divergence where we committed nothing at all: for a
/// single-segment name, silence is the resolver's "no shadow is possible"
/// claim, so it names a verdict just as a tier does.
const DENIED: &str = "denied";
/// The `ours` of a **decline**: we recorded a deferral, which is sound but is
/// also the entire cost of every veto — spelled with the guard that declined
/// and the ladder tier it spoke from, so the ratchet is two-sided on the reason
/// and not merely on the fact.
///
/// A reorder of the ladder is precisely a change that keeps cases deferring
/// while moving *which* guard does it. Keying on the bare word `deferred` would
/// let every such move through; keying on the cause makes each one a row that
/// has to be re-justified.
fn deferred_verdict(site: Option<DeclineSite>) -> String {
    match site {
        Some(s) => format!("deferred:{}@{}", s.cause.label(), s.tier.label()),
        // The census did not attribute this one. In this corpus that is
        // always the same thing — the walk found nothing at the written arity,
        // and on a *dotted* path recording nothing is not a claim (a bare one
        // would be [`Ours::Denied`]) — and the `J` family is the only family
        // that reaches it. `our_target` asserts the stronger half rather than
        // leaving it to the reader: a *recorded* deferral always names a
        // guard, so this can never be a threading gap in disguise.
        None => "deferred:no-claim".to_string(),
    }
}
/// The sentinel `fcs` of a divergence where FCS resolved the span to nothing.
const NOTHING: &str = "nothing";

/// Every case where we **decline** — record a deferral — and FCS binds
/// something, with the tier it binds and the channel that made us decline.
///
/// A decline makes no claim, so certain-implies-exact can never see one; that
/// is the property's whole design. But it is also the price of every shadow
/// veto, and an unratcheted count cannot tell a veto that got *weaker* from one
/// that got *stronger* — a veto that starts declining a case it used to bind
/// leaves this sweep green. Ratcheting the declines makes each one a
/// commitment, so the ranking work has a scoreboard rather than a printed
/// integer.
///
/// A decline where FCS *also* binds nothing is not here: nothing is lost, so
/// there is nothing to commit to.
///
/// Columns: the case key, the **verdict** ([`deferred_verdict`] — the guard
/// that declined and the ladder tier it spoke from, read off the resolver
/// rather than asserted by hand), the tier FCS binds, and the channel's
/// standing reason. The verdict is what makes the row two-sided on the
/// *reason*: `deferred` alone would let every guard-moving change through.
const KNOWN_DEFERRALS: &[(&str, &str, &str, &str)] = &[
    // The rank error that survives the ladder: the hidden entity sits at the
    // implicit-open tier, below the plant FCS binds, and the unranked veto ends
    // the walk from there. Only the **root**-tier plants are left — the
    // enclosing-namespace ones now out-rank the implicit opens outright — and
    // the implicit-open/root boundary is the one FCS decides by reference
    // order, which is why these two remain. A bare/dotted pair of one case. 2
    // cases.
    (
        "SRoYNs/contributor-first",
        "deferred:assembly_auto_open_shadow@implicit_open",
        "Root",
        decline::RISK_BELOW_THE_PLANT,
    ),
    (
        "VRoMNs/contributor-first",
        "deferred:assembly_auto_open_shadow@implicit_open",
        "Root",
        decline::RISK_BELOW_THE_PLANT,
    ),
    // The mirror image, and the ladder's own price: the risk now sits at the
    // enclosing namespace, which out-ranks the implicit-open plant, so the veto
    // fires *above* the tier FCS binds. In `SNsYEn`/`VNsMEn` FCS binds the
    // hidden entity and the deferral is the right answer; in `SNsMEn`/`VNsYEn`
    // what is hidden cannot bind the probe's form, so it is the shape-blindness
    // of the channel rather than its rank. 8 cases.
    (
        "SNsYEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::HIDDEN_WINS,
    ),
    (
        "SNsYEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::HIDDEN_WINS,
    ),
    (
        "VNsMEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::HIDDEN_WINS,
    ),
    (
        "VNsMEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::HIDDEN_WINS,
    ),
    (
        "SNsMEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "NsAuto",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SNsMEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "NsAuto",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VNsYEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "NsAuto",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VNsYEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "NsAuto",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    // The project channel's twin of the row above: the enclosing namespace now
    // out-ranks the implicit-open plant, so the project `[<AutoOpen>]` module —
    // which really does declare this dotted head — is consulted before the plant
    // and correctly wins. 2 cases.
    (
        "VNsQ/contributor-first",
        "deferred:project_auto_open_shadow@enclosing_namespace",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    (
        "VNsQ/decoy-first",
        "deferred:project_auto_open_shadow@enclosing_namespace",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    // Risk and plant share the root tier and FCS binds the plant. 2 cases.
    (
        "SRoYRo/contributor-first",
        "deferred:assembly_auto_open_shadow@root",
        "Root",
        decline::RISK_LOSES_AT_ROOT,
    ),
    (
        "SRoYRo/decoy-first",
        "deferred:assembly_auto_open_shadow@root",
        "Root",
        decline::RISK_LOSES_AT_ROOT,
    ),
    // The name is really declared; what wears it cannot bind the probe's
    // form. Exactly the two off-diagonal cells of the Risk x Form square,
    // in every order and at every risk tier the walk reaches before the
    // plant's own. 40 cases.
    (
        "SEnMEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Enclosing",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SEnMEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Enclosing",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SEnMEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Enclosing",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SEnMEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Enclosing",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SExMEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Explicit",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SExMEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Explicit",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SNsMEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "NsAuto",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SNsMEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "NsAuto",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SNsMNs/contributor-first",
        "deferred:assembly_auto_open_shadow@implicit_open",
        "NsAuto",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SNsMNs/decoy-first",
        "deferred:assembly_auto_open_shadow@implicit_open",
        "NsAuto",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SRoMEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Root",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SRoMEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Root",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SRoMEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Root",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SRoMEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Root",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SRoMNs/contributor-first",
        "deferred:assembly_auto_open_shadow@implicit_open",
        "Root",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SRoMNs/decoy-first",
        "deferred:assembly_auto_open_shadow@implicit_open",
        "Root",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SRoMRo/contributor-first",
        "deferred:assembly_auto_open_shadow@root",
        "Root",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "SRoMRo/decoy-first",
        "deferred:assembly_auto_open_shadow@root",
        "Root",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VEnYEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Enclosing",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VEnYEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Enclosing",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VEnYEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Enclosing",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VEnYEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Enclosing",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VExYEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Explicit",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VExYEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Explicit",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VNsYEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "NsAuto",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VNsYEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "NsAuto",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VNsYNs/contributor-first",
        "deferred:assembly_auto_open_shadow@implicit_open",
        "NsAuto",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VNsYNs/decoy-first",
        "deferred:assembly_auto_open_shadow@implicit_open",
        "NsAuto",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VRoYEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Root",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VRoYEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Root",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VRoYEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Root",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VRoYEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Root",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VRoYNs/contributor-first",
        "deferred:assembly_auto_open_shadow@implicit_open",
        "Root",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VRoYNs/decoy-first",
        "deferred:assembly_auto_open_shadow@implicit_open",
        "Root",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VRoYRo/contributor-first",
        "deferred:assembly_auto_open_shadow@root",
        "Root",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    (
        "VRoYRo/decoy-first",
        "deferred:assembly_auto_open_shadow@root",
        "Root",
        decline::NAME_KEYED_SHAPE_BLIND,
    ),
    // The hazard cell of the project channel: the project module holds the name,
    // FCS binds it, and no assembly tier gets a look in — so the plant's own
    // tier does not appear here at all. 16 cases: every bare one, and the two
    // dotted tiers our walk reaches the risk's namespace before committing
    // (the other two dotted tiers are the `V…Q` rows of `KNOWN_DIVERGENCES`).
    (
        "VEnQ/contributor-first",
        "deferred:project_auto_open_shadow@enclosing_namespace",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    (
        "VEnQ/decoy-first",
        "deferred:project_auto_open_shadow@enclosing_namespace",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    (
        "VRoQ/contributor-first",
        "deferred:project_auto_open_shadow@enclosing_namespace",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    (
        "VRoQ/decoy-first",
        "deferred:project_auto_open_shadow@enclosing_namespace",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    (
        "SCoQ/contributor-first",
        "deferred:same_file_auto_open_type@pre_walk",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    (
        "SCoQ/decoy-first",
        "deferred:same_file_auto_open_type@pre_walk",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    (
        "SEnQ/contributor-first",
        "deferred:same_file_auto_open_type@pre_walk",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    (
        "SEnQ/decoy-first",
        "deferred:same_file_auto_open_type@pre_walk",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    (
        "SExQ/contributor-first",
        "deferred:same_file_auto_open_type@pre_walk",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    (
        "SExQ/decoy-first",
        "deferred:same_file_auto_open_type@pre_walk",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    (
        "SMoQ/contributor-first",
        "deferred:same_file_auto_open_type@pre_walk",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    (
        "SMoQ/decoy-first",
        "deferred:same_file_auto_open_type@pre_walk",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    (
        "SNsQ/contributor-first",
        "deferred:same_file_auto_open_type@pre_walk",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    (
        "SNsQ/decoy-first",
        "deferred:same_file_auto_open_type@pre_walk",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    (
        "SRoQ/contributor-first",
        "deferred:same_file_auto_open_type@pre_walk",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    (
        "SRoQ/decoy-first",
        "deferred:same_file_auto_open_type@pre_walk",
        "Hidden@Project",
        decline::PROJECT_HIDDEN_WINS,
    ),
    // The two dotted `Q` cells that do *not* wrong-target: their own channel
    // declines before the project one would have been consulted, so they say
    // nothing about it either way.
    (
        "VCoQ/contributor-first",
        "deferred:project_auto_open_shadow@enclosing_namespace",
        "Hidden@Project",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoQ/decoy-first",
        "deferred:project_auto_open_shadow@enclosing_namespace",
        "Hidden@Project",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VMoQ/contributor-first",
        "deferred:project_auto_open_shadow@enclosing_namespace",
        "Hidden@Project",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoQ/decoy-first",
        "deferred:project_auto_open_shadow@enclosing_namespace",
        "Hidden@Project",
        decline::MANIFEST_SURFACE,
    ),
    // The `R` cells at the same two tiers. Their project-side declaration is
    // the shape that *cannot* bind the probe, so FCS binds the plant and the
    // project channel is silent — these decline purely on the contested /
    // manifest channels, exactly as the `Q` cells above do. At the four tiers
    // the walk visits as prefixes they commit, which is where they carry their
    // claim (the non-vacuity floor).
    (
        "SCoR/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoR/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SMoR/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoR/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VCoR/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoR/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VMoR/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoR/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    // Correct declines: FCS really does bind the hidden entity. Recorded so
    // that a change which starts committing here has to say so. 32 cases.
    (
        "SEnYEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::HIDDEN_WINS,
    ),
    (
        "SEnYEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::HIDDEN_WINS,
    ),
    (
        "SEnYEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::HIDDEN_WINS,
    ),
    (
        "SEnYEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::HIDDEN_WINS,
    ),
    (
        "SExYEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::HIDDEN_WINS,
    ),
    (
        "SExYEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::HIDDEN_WINS,
    ),
    (
        "SNsYEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::HIDDEN_WINS,
    ),
    (
        "SNsYEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::HIDDEN_WINS,
    ),
    (
        "SNsYNs/contributor-first",
        "deferred:assembly_auto_open_shadow@implicit_open",
        "Hidden@NsAuto",
        decline::HIDDEN_WINS,
    ),
    (
        "SNsYNs/decoy-first",
        "deferred:assembly_auto_open_shadow@implicit_open",
        "Hidden@NsAuto",
        decline::HIDDEN_WINS,
    ),
    (
        "SRoYEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::HIDDEN_WINS,
    ),
    (
        "SRoYEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::HIDDEN_WINS,
    ),
    (
        "SRoYEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::HIDDEN_WINS,
    ),
    (
        "SRoYEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::HIDDEN_WINS,
    ),
    (
        "SRoYNs/decoy-first",
        "deferred:assembly_auto_open_shadow@implicit_open",
        "Hidden@NsAuto",
        decline::HIDDEN_WINS,
    ),
    (
        "VEnMEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::HIDDEN_WINS,
    ),
    (
        "VEnMEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::HIDDEN_WINS,
    ),
    (
        "VEnMEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::HIDDEN_WINS,
    ),
    (
        "VEnMEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::HIDDEN_WINS,
    ),
    (
        "VExMEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::HIDDEN_WINS,
    ),
    (
        "VExMEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::HIDDEN_WINS,
    ),
    (
        "VNsMEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::HIDDEN_WINS,
    ),
    (
        "VNsMEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::HIDDEN_WINS,
    ),
    (
        "VNsMNs/contributor-first",
        "deferred:assembly_auto_open_shadow@implicit_open",
        "Hidden@NsAuto",
        decline::HIDDEN_WINS,
    ),
    (
        "VNsMNs/decoy-first",
        "deferred:assembly_auto_open_shadow@implicit_open",
        "Hidden@NsAuto",
        decline::HIDDEN_WINS,
    ),
    (
        "VRoMEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::HIDDEN_WINS,
    ),
    (
        "VRoMEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::HIDDEN_WINS,
    ),
    (
        "VRoMEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::HIDDEN_WINS,
    ),
    (
        "VRoMEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::HIDDEN_WINS,
    ),
    (
        "VRoMNs/decoy-first",
        "deferred:assembly_auto_open_shadow@implicit_open",
        "Hidden@NsAuto",
        decline::HIDDEN_WINS,
    ),
    (
        "VRoMRo/contributor-first",
        "deferred:assembly_auto_open_shadow@root",
        "Hidden@Root",
        decline::HIDDEN_WINS,
    ),
    (
        "VRoMRo/decoy-first",
        "deferred:assembly_auto_open_shadow@root",
        "Hidden@Root",
        decline::HIDDEN_WINS,
    ),
    // The arity fallback, in both forms — the `W` (bare) and `J` (dotted)
    // families. No tier holds the written arity, so the arity-keyed walk finds
    // nothing, and FCS binds a wrong-arity occupant instead. Both decline, and
    // for the same reason: FCS's arity preference is a fallback, not a filter.
    // 40 cases.
    //
    // A decline and not a denial even in the bare form, where silence would be
    // the resolver's "no shadow is possible" claim: FCS does bind here, so that
    // claim would be false.
    (
        "WEn/contributor-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Enclosing",
        decline::ARITY_FALLBACK,
    ),
    (
        "WEn/decoy-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Enclosing",
        decline::ARITY_FALLBACK,
    ),
    (
        "WEnNs/contributor-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Enclosing",
        decline::ARITY_FALLBACK,
    ),
    (
        "WEnNs/decoy-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Enclosing",
        decline::ARITY_FALLBACK,
    ),
    (
        "WEnRo/contributor-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Enclosing",
        decline::ARITY_FALLBACK,
    ),
    (
        "WEnRo/decoy-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Enclosing",
        decline::ARITY_FALLBACK,
    ),
    (
        "WEx/contributor-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Explicit",
        decline::ARITY_FALLBACK,
    ),
    (
        "WEx/decoy-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Explicit",
        decline::ARITY_FALLBACK,
    ),
    (
        "WExEn/contributor-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Explicit",
        decline::ARITY_FALLBACK,
    ),
    (
        "WExEn/decoy-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Explicit",
        decline::ARITY_FALLBACK,
    ),
    (
        "WExNs/contributor-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Explicit",
        decline::ARITY_FALLBACK,
    ),
    (
        "WExNs/decoy-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Explicit",
        decline::ARITY_FALLBACK,
    ),
    (
        "WExRo/contributor-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Explicit",
        decline::ARITY_FALLBACK,
    ),
    (
        "WExRo/decoy-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Explicit",
        decline::ARITY_FALLBACK,
    ),
    (
        "WNs/contributor-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "NsAuto",
        decline::ARITY_FALLBACK,
    ),
    (
        "WNs/decoy-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "NsAuto",
        decline::ARITY_FALLBACK,
    ),
    (
        "WNsRo/contributor-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Root",
        decline::ARITY_FALLBACK,
    ),
    (
        "WNsRo/decoy-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "NsAuto",
        decline::ARITY_FALLBACK,
    ),
    (
        "WRo/contributor-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Root",
        decline::ARITY_FALLBACK,
    ),
    (
        "WRo/decoy-first",
        "deferred:wrong_arity_occupant@whole_walk",
        "Root",
        decline::ARITY_FALLBACK,
    ),
    (
        "JEn/contributor-first",
        "deferred:no-claim",
        "Enclosing",
        decline::ARITY_FALLBACK,
    ),
    (
        "JEn/decoy-first",
        "deferred:no-claim",
        "Enclosing",
        decline::ARITY_FALLBACK,
    ),
    (
        "JEnNs/contributor-first",
        "deferred:no-claim",
        "Enclosing",
        decline::ARITY_FALLBACK,
    ),
    (
        "JEnNs/decoy-first",
        "deferred:no-claim",
        "Enclosing",
        decline::ARITY_FALLBACK,
    ),
    (
        "JEnRo/contributor-first",
        "deferred:no-claim",
        "Enclosing",
        decline::ARITY_FALLBACK,
    ),
    (
        "JEnRo/decoy-first",
        "deferred:no-claim",
        "Enclosing",
        decline::ARITY_FALLBACK,
    ),
    (
        "JEx/contributor-first",
        "deferred:no-claim",
        "Explicit",
        decline::ARITY_FALLBACK,
    ),
    (
        "JEx/decoy-first",
        "deferred:no-claim",
        "Explicit",
        decline::ARITY_FALLBACK,
    ),
    (
        "JExEn/contributor-first",
        "deferred:no-claim",
        "Explicit",
        decline::ARITY_FALLBACK,
    ),
    (
        "JExEn/decoy-first",
        "deferred:no-claim",
        "Explicit",
        decline::ARITY_FALLBACK,
    ),
    (
        "JExNs/contributor-first",
        "deferred:no-claim",
        "Explicit",
        decline::ARITY_FALLBACK,
    ),
    (
        "JExNs/decoy-first",
        "deferred:no-claim",
        "Explicit",
        decline::ARITY_FALLBACK,
    ),
    (
        "JExRo/contributor-first",
        "deferred:no-claim",
        "Explicit",
        decline::ARITY_FALLBACK,
    ),
    (
        "JExRo/decoy-first",
        "deferred:no-claim",
        "Explicit",
        decline::ARITY_FALLBACK,
    ),
    (
        "JNs/contributor-first",
        "deferred:no-claim",
        "NsAuto",
        decline::ARITY_FALLBACK,
    ),
    (
        "JNs/decoy-first",
        "deferred:no-claim",
        "NsAuto",
        decline::ARITY_FALLBACK,
    ),
    (
        "JNsRo/contributor-first",
        "deferred:no-claim",
        "Root",
        decline::ARITY_FALLBACK,
    ),
    (
        "JNsRo/decoy-first",
        "deferred:no-claim",
        "NsAuto",
        decline::ARITY_FALLBACK,
    ),
    (
        "JRo/contributor-first",
        "deferred:no-claim",
        "Root",
        decline::ARITY_FALLBACK,
    ),
    (
        "JRo/decoy-first",
        "deferred:no-claim",
        "Root",
        decline::ARITY_FALLBACK,
    ),
    // A module-shaped manifest auto-open is among the contenders. 100 cases.
    (
        "DMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "DMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "DMoRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Root",
        decline::MANIFEST_SURFACE,
    ),
    (
        "DMoRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "DNsMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "DNsMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "FEnMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "FEnMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "FExMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "FExMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "FNsMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "FNsMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "GMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "GMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "GMoRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Root",
        decline::MANIFEST_SURFACE,
    ),
    (
        "GMoRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "GNsMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "GNsMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "HMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "HMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "HMoRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Root",
        decline::MANIFEST_SURFACE,
    ),
    (
        "HMoRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "HNsMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "HNsMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "JEnMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Enclosing",
        decline::MANIFEST_SURFACE,
    ),
    (
        "JEnMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Enclosing",
        decline::MANIFEST_SURFACE,
    ),
    (
        "JExMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Explicit",
        decline::MANIFEST_SURFACE,
    ),
    (
        "JExMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Explicit",
        decline::MANIFEST_SURFACE,
    ),
    (
        "JMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "JMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "JMoRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Root",
        decline::MANIFEST_SURFACE,
    ),
    (
        "JMoRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "JNsMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "JNsMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "KEnMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "KEnMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "KExMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "KExMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "KMoRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Root",
        decline::MANIFEST_SURFACE,
    ),
    (
        "KMoRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Root",
        decline::MANIFEST_SURFACE,
    ),
    (
        "KNsMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "KNsMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "LMoRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "LMoRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "LNsMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "NsAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "LNsMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "NsAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "RMoRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "RMoRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoMEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoMEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoMEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoMEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoMNs/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoMNs/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoMRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoMRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoP/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoP/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoYEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoYEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoYEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoYEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoYNs/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoYNs/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoYRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Hidden@Root",
        decline::MANIFEST_SURFACE,
    ),
    (
        "SMoYRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "TMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "TMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "TMoRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Root",
        decline::MANIFEST_SURFACE,
    ),
    (
        "TMoRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "TNsMo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "TNsMo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoMEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoMEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoMEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoMEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoMNs/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoMNs/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoMRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Hidden@Root",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoMRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoP/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoP/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoYEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoYEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoYEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoYEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoYNs/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoYNs/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoYRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "VMoYRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "WEnMo/contributor-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Enclosing",
        decline::MANIFEST_SURFACE,
    ),
    (
        "WEnMo/decoy-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Enclosing",
        decline::MANIFEST_SURFACE,
    ),
    (
        "WExMo/contributor-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Explicit",
        decline::MANIFEST_SURFACE,
    ),
    (
        "WExMo/decoy-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Explicit",
        decline::MANIFEST_SURFACE,
    ),
    (
        "WMo/contributor-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "WMo/decoy-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "WMoRo/contributor-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Root",
        decline::MANIFEST_SURFACE,
    ),
    (
        "WMoRo/decoy-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "WNsMo/contributor-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    (
        "WNsMo/decoy-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "ModAuto",
        decline::MANIFEST_SURFACE,
    ),
    // A contested manifest auto-open is among the contenders. 120 cases.
    (
        "DCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "DCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "DCoRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Root",
        decline::CONTESTED_DROPPED,
    ),
    (
        "DCoRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "DMoCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "DMoCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "DNsCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "DNsCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "FEnCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "FEnCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "FExCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "FExCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "FMoCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "FMoCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "FNsCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "FNsCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "GCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "GCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "GCoRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Root",
        decline::CONTESTED_DROPPED,
    ),
    (
        "GCoRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "GMoCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "GMoCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "GNsCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "GNsCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "HCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "HCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "HCoRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Root",
        decline::CONTESTED_DROPPED,
    ),
    (
        "HCoRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "HMoCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "HMoCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "HNsCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "HNsCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "JCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "JCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "JCoRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Root",
        decline::CONTESTED_DROPPED,
    ),
    (
        "JCoRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "JEnCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Enclosing",
        decline::CONTESTED_DROPPED,
    ),
    (
        "JEnCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Enclosing",
        decline::CONTESTED_DROPPED,
    ),
    (
        "JExCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Explicit",
        decline::CONTESTED_DROPPED,
    ),
    (
        "JExCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Explicit",
        decline::CONTESTED_DROPPED,
    ),
    (
        "JMoCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "JMoCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "JNsCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "JNsCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "KCoRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Root",
        decline::CONTESTED_DROPPED,
    ),
    (
        "KCoRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Root",
        decline::CONTESTED_DROPPED,
    ),
    (
        "KEnCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "KEnCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "KExCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "KExCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "KMoCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "KMoCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "KNsCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "KNsCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "LCoRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "LCoRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "LMoCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::CONTESTED_DROPPED,
    ),
    (
        "LMoCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::CONTESTED_DROPPED,
    ),
    (
        "LNsCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "NsAuto",
        decline::CONTESTED_DROPPED,
    ),
    (
        "LNsCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "NsAuto",
        decline::CONTESTED_DROPPED,
    ),
    (
        "RCoRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "RCoRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "RMoCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::CONTESTED_DROPPED,
    ),
    (
        "RMoCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "ModAuto",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoMEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoMEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoMEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoMEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoMNs/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoMNs/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoMRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoMRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoP/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoP/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoYEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoYEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoYEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoYEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoYNs/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoYNs/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoYRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Hidden@Root",
        decline::CONTESTED_DROPPED,
    ),
    (
        "SCoYRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "TCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "TCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "TCoRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Root",
        decline::CONTESTED_DROPPED,
    ),
    (
        "TCoRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "TMoCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "TMoCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "TNsCo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "TNsCo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoMEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoMEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Hidden@Enclosing",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoMEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoMEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Hidden@Explicit",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoMNs/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoMNs/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoMRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Hidden@Root",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoMRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoP/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoP/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoYEn/contributor-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoYEn/decoy-first",
        "deferred:assembly_auto_open_shadow@enclosing_namespace",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoYEx/contributor-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoYEx/decoy-first",
        "deferred:assembly_auto_open_shadow@explicit_open",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoYNs/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoYNs/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoYRo/contributor-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "VCoYRo/decoy-first",
        "deferred:manifest_surface_contest@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "WCo/contributor-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "WCo/decoy-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "WCoRo/contributor-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Root",
        decline::CONTESTED_DROPPED,
    ),
    (
        "WCoRo/decoy-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "WEnCo/contributor-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Enclosing",
        decline::CONTESTED_DROPPED,
    ),
    (
        "WEnCo/decoy-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Enclosing",
        decline::CONTESTED_DROPPED,
    ),
    (
        "WExCo/contributor-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Explicit",
        decline::CONTESTED_DROPPED,
    ),
    (
        "WExCo/decoy-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Explicit",
        decline::CONTESTED_DROPPED,
    ),
    (
        "WMoCo/contributor-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "WMoCo/decoy-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "WNsCo/contributor-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
    (
        "WNsCo/decoy-first",
        "deferred:manifest_surface_arity_fallback@whole_walk",
        "Contested",
        decline::CONTESTED_DROPPED,
    ),
];

/// Every recorded divergence and decline, keyed by its **whole identity** —
/// `(case, what we said, what FCS said)` — rather than by the case alone.
///
/// The case key is not an identity: a recorded denial that decays into a
/// wrong-target commit keeps it, and so does a wrong target that starts naming
/// a different tier or that FCS stops resolving at all. Both are regressions
/// the tables would otherwise satisfy for each other (codex review). Every row
/// therefore states the verdict it expects, and a case that diverges a
/// different way is reported as *both* an unexpected divergence and a stale
/// entry. Declines share the keyspace for the same reason: a decline that
/// hardens into a wrong commit must not be able to keep its row.
fn known_records() -> BTreeMap<(String, String, String), &'static str> {
    KNOWN_DIVERGENCES
        .iter()
        .map(|&(case, ours, fcs, why)| ((case.into(), ours.into(), fcs.into()), why))
        .chain(WRONG_ARITY_DENIALS.iter().map(|&(case, fcs)| {
            (
                (case.into(), DENIED.into(), fcs.into()),
                "a recorded wrong denial",
            )
        }))
        .chain(
            KNOWN_DEFERRALS
                .iter()
                .map(|&(case, verdict, fcs, why)| ((case.into(), verdict.into(), fcs.into()), why)),
        )
        .collect()
}

/// The `(assembly, full name)` currency both oracles report in.
type Target = (String, String);

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// Name the tier a reported target came from, for a readable failure. `None`
/// when the target is not one of the plant's own declarations — which is
/// itself the interesting case, so the caller prints the raw target too.
fn tier_of(plant: &Plant, target: &Target) -> Option<Tier> {
    plant
        .tiers
        .iter()
        .copied()
        .find(|&t| plant.declaration(t) == (target.0.as_str(), target.1.clone()))
}

fn describe(plant: &Plant, target: &Target) -> String {
    if let Some(t) = tier_of(plant, target) {
        return format!("{t:?}");
    }
    // A risk plant has a second declaration nothing enumerates: the one its
    // `[<AutoOpen>]` module hides. Naming it keeps a recorded row readable —
    // and keeps "the hidden entity won" distinct from "we cannot account for
    // this target at all", which is a corpus bug rather than a ladder fact.
    if plant
        .hidden_declaration()
        .is_some_and(|d| d == (target.0.as_str(), target.1.clone()))
    {
        let tier = plant.risk.tier().expect("a hidden declaration has a tier");
        return format!("Hidden@{tier:?}");
    }
    // Its project-side twin, matched on the full name alone: the entity is
    // compiled with the probe, so its assembly is whatever FCS names the
    // throwaway compilation rather than a fixture constant.
    if plant
        .project_hidden_full_name()
        .is_some_and(|full| full == target.1)
    {
        return "Hidden@Project".to_string();
    }
    format!("<not a plant declaration: {}/{}>", target.0, target.1)
}

/// Our verdict for one probe. The three cases are genuinely different claims,
/// and collapsing the last two — as "an `Option<Target>`" does — is what hides
/// the arity-fallback branch from this sweep.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Ours {
    /// We committed this entity. Bound by certain-implies-exact.
    Entity(Target),
    /// We recorded a deferral: something may bind here, but we cannot say
    /// what. Makes no claim, and is always sound — which is exactly why it
    /// needs a table of its own ([`KNOWN_DEFERRALS`]) rather than a counter: a
    /// sound answer is still a lost binding, and nothing else here can tell one
    /// deferral from a thousand.
    ///
    /// The payload is **which guard** declined and the ladder tier it spoke
    /// from ([`ResolvedFile::decline_site`]), and it rides in the ratchet key.
    /// That is what makes the table two-sided on the *reason*: without it, a
    /// change that keeps a case deferring while moving the guard that did it —
    /// exactly what reordering the ladder does — passes unremarked. `None`
    /// where the resolver declined without naming a guard, which is itself
    /// worth seeing in the table.
    Deferred(Option<DeclineSite>),
    /// We denied that anything can bind. For a single-segment name, recording
    /// nothing is not an absence of opinion but an opinion — the resolver's
    /// "no shadow is possible" signal — and [`Resolution::Unresolved`] is the
    /// same claim made explicitly. Either is a claim FCS can contradict.
    ///
    /// Reachable only for [`tier_corpus::Form::Bare`]: `defer_shadowable_type`
    /// marks single-segment paths only, so a *dotted* deferral records nothing
    /// either and its silence says nothing at all. That distinction belongs
    /// here, at the one place a verdict is decided, so
    /// [`report_tier_ladder`] cannot print `(denied)` for a sound dotted
    /// deferral (codex review).
    Denied,
}

/// Our verdict for one probe, at the plant's span.
fn our_target(env: &AssemblyEnv, src: &str, plant: &Plant) -> Ours {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "tier probe for {} does not parse: {:?}",
        plant.name,
        parsed.errors
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("probe is an impl file");
    let rf = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let (start, end) = tier_corpus::probe_use_span(src, plant);
    // The census keys a path decline at the **whole written path**, which for a
    // dotted probe is not the leaf both oracles report the use at.
    let (path_start, path_end) = tier_corpus::probe_path_span(src, plant);
    let site = rf.decline_site(span(path_start, path_end));
    match rf.resolution_at(span(start, end)) {
        // `entity_full_name` is the currency `fcs-dump` was taught to report
        // in: nesting-aware, and named from `source_name` so a generic's
        // compiled backtick arity never reaches the comparison.
        Some(Resolution::Entity(h)) => {
            Ours::Entity((env.entity(h).assembly.name.clone(), env.entity_full_name(h)))
        }
        // A recorded deferral is a guard's doing, so it always names one.
        // Asserting it here — rather than trusting the threading — is what
        // keeps `deferred:no-claim` meaning "nothing declined this" instead of
        // silently absorbing a census gap.
        Some(Resolution::Deferred(_)) => {
            assert!(
                site.is_some(),
                "tier probe {}: recorded a deferral with no decline site",
                plant.name
            );
            Ours::Deferred(site)
        }
        // A recorded no-match is a claim only where the resolver makes one.
        Some(Resolution::Unresolved) | None if plant.form == tier_corpus::Form::Bare => {
            Ours::Denied
        }
        Some(Resolution::Unresolved) | None => Ours::Deferred(site),
        // Nothing else is reachable from this corpus, and each would be a
        // distinct bug rather than a deferral: the probe declares no type of
        // the plant's name, is resolved against an empty `ProjectItems`, and
        // binds no local — so a `Local`/`Item` verdict means the walk
        // wrong-targeted an in-file or project binder, and a `Member` one
        // means it bound a *value* for a type-position name. A catch-all arm
        // would launder all three into "no claim", which is exactly the
        // collapse this sweep exists to prevent.
        Some(res @ (Resolution::Local(_) | Resolution::Item(_) | Resolution::Member { .. })) => {
            panic!(
                "tier probe {}: type position resolved to {res:?}",
                plant.name
            )
        }
    }
}

/// FCS's verdict for one probe.
fn fcs_target(refs: &[&std::path::Path], src: &str, plant: &Plant) -> Option<Target> {
    let (start, end) = tier_corpus::probe_use_span(src, plant);
    let path = temp_fs_file("tier_order", src);
    let json = invoke_fcs_dump_with_refs("uses", &path, refs);
    let _ = std::fs::remove_file(&path);
    parse_fcs_uses(&json, src)
        .into_iter()
        .find(|u| {
            !u.is_from_definition
                && u.start <= start
                && end <= u.end
                && u.name == plant.probed_ident()
        })
        .and_then(|u| Some((u.assembly?, u.full_name?)))
}

/// Every case's `(key, ours, fcs)`, one reference order at a time so the two
/// `AssemblyEnv`s and the two `-r` orders stay in lock step.
fn observe() -> BTreeMap<String, (Ours, Option<Target>)> {
    let (contributor, decoy) = ensure_tier_corpus_built();
    let contributor_bytes = std::fs::read(contributor).expect("read tier contributor dll");
    let decoy_bytes = std::fs::read(decoy).expect("read tier decoy dll");
    let plants = tier_corpus::corpus();

    let mut out = BTreeMap::new();
    for order in Order::ALL {
        let views = match order {
            Order::ContributorFirst => [&contributor_bytes, &decoy_bytes],
            Order::DecoyFirst => [&decoy_bytes, &contributor_bytes],
        }
        .map(|b| Ecma335Assembly::parse(b).expect("parse tier fixture dll"));
        let refs: Vec<&std::path::Path> = match order {
            Order::ContributorFirst => vec![contributor, decoy],
            Order::DecoyFirst => vec![decoy, contributor],
        };
        let env = AssemblyEnv::from_views(&views).expect("build tier AssemblyEnv");

        for plant in &plants {
            let src = tier_corpus::probe_source(plant);
            let ours = our_target(&env, &src, plant);
            let fcs = fcs_target(&refs, &src, plant);
            out.insert(plant.key(order.label()), (ours, fcs));
        }
    }
    out
}

#[test]
fn tier_ladder_is_sound_against_fcs() {
    let plants: BTreeMap<String, Plant> = tier_corpus::corpus()
        .into_iter()
        .map(|p| (p.name.clone(), p))
        .collect();
    let observations = observe();

    // Unsound rows — we said something FCS contradicts — and cost rows — we
    // said nothing where FCS binds — are kept apart. They share one keyspace so
    // a case cannot hold a row of each, but they are not the same failure: an
    // unsound row is a bug to fix, a cost row is a decline to justify.
    let mut unsound: BTreeMap<(String, String, String), String> = BTreeMap::new();
    let mut cost: BTreeMap<(String, String, String), String> = BTreeMap::new();
    let mut agreed = 0usize;
    let mut silent = 0usize;
    for (key, (ours, fcs)) in &observations {
        let plant = &plants[key.split('/').next().expect("keyed <plant>/<order>")];
        match (ours, fcs) {
            // Neither side binds: our silence costs nothing, so there is
            // nothing to commit to.
            (Ours::Deferred(_) | Ours::Denied, None) => silent += 1,
            (Ours::Deferred(site), Some(f)) => {
                cost.insert(
                    (key.clone(), deferred_verdict(*site), describe(plant, f)),
                    "we decline where FCS binds".to_string(),
                );
            }
            (Ours::Denied, Some(f)) => {
                unsound.insert(
                    (key.clone(), DENIED.into(), describe(plant, f)),
                    "we deny that anything can bind — the \"no shadow is possible\" signal — \
                     but FCS binds something"
                        .to_string(),
                );
            }
            (Ours::Entity(o), Some(f)) if o == f => agreed += 1,
            (Ours::Entity(o), Some(f)) => {
                unsound.insert(
                    (key.clone(), describe(plant, o), describe(plant, f)),
                    "we bound a different tier from the one FCS binds".to_string(),
                );
            }
            (Ours::Entity(o), None) => {
                unsound.insert(
                    (key.clone(), describe(plant, o), NOTHING.into()),
                    "we bound a tier where FCS resolves the span to nothing at all".to_string(),
                );
            }
        }
    }

    // Non-vacuity, per case rather than by a count: a corpus that stopped
    // building, or a probe template that stopped resolving, would otherwise
    // pass this test by committing nothing at all. The floor is the
    // **uncontested** plants at the tiers the resolver is supposed to commit —
    // nothing contends for those names in either order, so anything but exact
    // agreement is a bug. (`TCo` and `TMo` are deliberately absent: a manifest
    // surface is never committed, only deferred to, which is the design. So is
    // every `W`/`J`: with no exact-arity occupant anywhere, a *deferral* is the
    // right answer even uncontested, so it cannot serve as a floor. The split
    // families `F`/`R`/`K`/`L` have no singletons at all.)
    //
    // The `S`/`V` entries are the risk families' floor, and they carry a second
    // job: a project `[<AutoOpen>]` module that holds nothing of the name must
    // veto nothing. Every `…P` cell the walk can reach is here — the four tiers
    // the project channel could preempt, in both forms — so a veto that went
    // back to reading the module's *presence* rather than its names would fail
    // here rather than merely re-appear in `KNOWN_DEFERRALS`. (`SCoP`/`SMoP`
    // and `VCoP`/`VMoP` are absent: the contested and manifest channels decline
    // those regardless, so they cannot floor anything.)
    //
    // The `…R` entries floor the channel's keying **by form**, which the `…P`
    // ones cannot: a `P` probe declares nothing of the plant's name on the
    // project side at all, so it commits whichever index a head consults. An
    // `R` probe declares the name in the shape that cannot bind the probe — a
    // module where a bare annotation is written, a type where a dotted head is
    // — so a bare head widened to the module index fails `S…R`, and a dotted
    // head widened to the type index fails `V…R`, and neither shows up
    // anywhere else in the sweep.
    for control in [
        "TEx", "TEn", "TNs", "TRo", //
        "DEx", "DEn", "DNs", "DRo", //
        "GEx", "GEn", "GNs", "GRo", //
        "HEx", "HEn", "HNs", "HRo", //
        "SExP", "SEnP", "SNsP", "SRoP", //
        "VExP", "VEnP", "VNsP", "VRoP", //
        "SExR", "SEnR", "SNsR", "SRoR", //
        "VExR", "VEnR", "VNsR", "VRoR",
    ] {
        for order in Order::ALL {
            let key = format!("{control}/{}", order.label());
            let (ours, fcs) = &observations[&key];
            let fcs = fcs.as_ref().unwrap_or_else(|| {
                panic!("{key}: FCS resolved nothing — corpus broken?");
            });
            assert_eq!(
                ours,
                &Ours::Entity(fcs.clone()),
                "{key}: an uncontested name must resolve, and to what FCS says",
            );
        }
    }

    let known = known_records();
    let expected: BTreeSet<&(String, String, String)> = known.keys().collect();
    let recorded: BTreeMap<&(String, String, String), &String> =
        unsound.iter().chain(cost.iter()).collect();
    let observed: BTreeSet<&(String, String, String)> = recorded.keys().copied().collect();

    let show =
        |(case, ours, fcs): &(String, String, String)| format!("{case}: ours={ours} fcs={fcs}");
    let mut new_unsound = Vec::new();
    let mut new_cost = Vec::new();
    for id in observed.difference(&expected) {
        let line = format!("  {} — {}", show(id), recorded[*id]);
        if unsound.contains_key(*id) {
            new_unsound.push(line);
        } else {
            new_cost.push(line);
        }
    }
    let stale: Vec<String> = expected
        .difference(&observed)
        .map(|id| {
            format!(
                "  {} — recorded ({}), but it now agrees or lands a different way",
                show(id),
                known[*id]
            )
        })
        .collect();

    let or_none = |lines: Vec<String>| {
        if lines.is_empty() {
            "  (none)".to_string()
        } else {
            lines.join("\n")
        }
    };
    assert!(
        new_unsound.is_empty() && new_cost.is_empty() && stale.is_empty(),
        "tier ladder disagrees with FCS.\n\
         NEW divergences (a wrong target or a wrong denial — fix, or record in \
         KNOWN_DIVERGENCES/WRONG_ARITY_DENIALS with an FCS-verified reason):\n{}\n\
         NEW declines (sound, but we lost a binding FCS makes — fix, or record in \
         KNOWN_DEFERRALS with the channel that declined):\n{}\n\
         STALE entries (remove them; the ratchet is two-sided so a fix must land with its \
         entry):\n{}\n\
         ({agreed} agreed, {silent} silent on both sides, {} cases total)",
        or_none(new_unsound),
        or_none(new_cost),
        or_none(stale),
        observations.len(),
    );
}

/// The measurement behind the ladder, printed rather than asserted: which tier
/// FCS picks for every contest, in both reference orders. Run it when a scope
/// question comes up instead of reasoning about the ladder from the comments.
///
/// `#[ignore]`d — it is a report, and its value is the output, not a verdict.
#[test]
#[ignore = "report generator; run explicitly with --ignored --nocapture"]
fn report_tier_ladder() {
    let plants: BTreeMap<String, Plant> = tier_corpus::corpus()
        .into_iter()
        .map(|p| (p.name.clone(), p))
        .collect();
    for (key, (ours, fcs)) in observe() {
        let plant = &plants[key.split('/').next().expect("keyed <plant>/<order>")];
        let show_fcs = match &fcs {
            Some(t) => describe(plant, t),
            None => "-".to_string(),
        };
        let show_ours = match &ours {
            Ours::Entity(t) => describe(plant, t),
            Ours::Deferred(site) => format!("({})", deferred_verdict(*site)),
            Ours::Denied => "(denied)".to_string(),
        };
        println!(
            "{key:<40} {:<13} contenders={:<30} fcs={:<12} ours={show_ours}",
            format!("{:?}", plant.arity),
            format!("{:?}", plant.tiers),
            show_fcs,
        );
    }
}
