//! The **tier corpus**: the F# sources for the two fixture assemblies
//! `tier_order_diff` probes, generated from [`Tier`] so the F# universe and
//! the Rust side's matrix cannot drift apart (the `overload_corpus` pattern).
//!
//! F# makes a bare name visible from several scopes at once, and the resolver
//! ranks them. Each [`Tier`] is one such scope, and the corpus plants a
//! *distinct* name for every singleton and every unordered pair of them — so
//! one probe puts exactly two scopes in contention and FCS's answer names the
//! winner with no third party to confound it.
//!
//! **Pairs, and not larger subsets, because that was measured.** Extending
//! [`corpus`] to unordered triples is a three-line change, and it was run over
//! the six families whose per-tier arity does not itself depend on the subset
//! planted — `T`/`D`/`G`/`W`/`H`/`J`, i.e. every one but the four split
//! families, where a triple is a different experiment from its pairs rather
//! than a composition of them. FCS picked the pairwise champion in **240 of
//! 240** three-way contests, in both reference orders. Its ladder is a total
//! order on these scopes, so a triple is determined by its pairs and buys only
//! cases. Should a scope ever be added whose ranking is *contextual*, that
//! measurement is what stops being true; re-run it by extending the loop right
//! below, and re-derive the count whenever [`FAMILIES`] changes.
//!
//! [`Form`] is the second, orthogonal dimension: a bare name and a *dotted
//! head* reach a scope through different channels. An opened namespace
//! contributes its child namespaces as dotted heads, which are not entities in
//! it at all, so a surface query written only against its members answers `no`
//! for a name FCS resolves right through it.
//!
//! [`Arity`] is the third. FCS keys a type-position lookup on the written
//! generic arity, but as a *preference*, not a filter: with no exact-arity
//! occupant anywhere it falls back to a wrong-arity one and reports the use
//! (with an arity error). A tier ladder measured only at arity 0 says nothing
//! about either half of that — whether the arity comparison happens at all,
//! or which tier the fallback reaches — so each arity shape re-runs the whole
//! tier matrix.
//!
//! [`Risk`] is the fourth. The tiers above are what the resolver can *see*; a
//! shadow risk is what it cannot. `resolve_assembly_path_over` vetoes a reading
//! when something at that namespace prefix might hide a same-named entity, and
//! the veto ends the whole walk — so a risk's cost is decided by where the
//! prefix carrying it sits, not by where the hidden entity actually enters
//! scope. Nothing measures that without a risk planted at a namespace the walk
//! visits as a *prefix*: [`Tier::ModAuto`] plants an assembly `[<AutoOpen>]`
//! module, but at `Tier.ModAuto.Ops`, which is never a prefix. So each risk
//! plant hides an entity behind one of those channels at a chosen tier and asks
//! FCS which wins.
//!
//! Two assemblies, not one, because [`Tier::Root`] must be able to move: an
//! assembly's root-namespace contents and its `[<assembly: AutoOpen>]` targets
//! both enter the name environment when *that assembly* is imported, so which
//! of the two wins is a question about reference order. Splitting them lets
//! the sweep reference the same pair of DLLs in both orders. The decoy's
//! second job is to re-declare `Tier.Contested`, which is what makes the
//! contributor's auto-open of it *contested*.

/// One scope a name can be visible from without the probe naming it in an
/// `open`. The sweep's first dimension.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Tier {
    /// A namespace the probe explicitly `open`s.
    Explicit,
    /// The probe's own enclosing namespace.
    Enclosing,
    /// A **namespace-shaped** `[<assembly: AutoOpen("Tier.NsAuto")>]`, declared
    /// by the contributor alone — an *implicit open* in our model.
    NsAuto,
    /// A **module-shaped** `[<assembly: AutoOpen("Tier.ModAuto.Ops")>]`. FCS
    /// opens the module like a source `open`; our env keeps it out of the
    /// prefix walk and models it as a retained surface.
    ModAuto,
    /// A namespace-shaped `[<assembly: AutoOpen("Tier.Contested")>]` whose
    /// namespace the *decoy* also declares. FCS still applies it, scoped to
    /// the contributing assembly's own content; our env drops it from the
    /// opens entirely.
    Contested,
    /// The root (`namespace global`) of the **decoy** assembly.
    Root,
}

impl Tier {
    pub const ALL: [Tier; 6] = [
        Tier::Explicit,
        Tier::Enclosing,
        Tier::NsAuto,
        Tier::ModAuto,
        Tier::Contested,
        Tier::Root,
    ];

    /// The two-letter tag that names this tier inside a planted name.
    pub fn tag(self) -> &'static str {
        match self {
            Tier::Explicit => "Ex",
            Tier::Enclosing => "En",
            Tier::NsAuto => "Ns",
            Tier::ModAuto => "Mo",
            Tier::Contested => "Co",
            Tier::Root => "Ro",
        }
    }

    /// Where a plant for this tier is declared, as FCS spells the full name's
    /// prefix (empty for the root).
    pub fn namespace(self) -> &'static str {
        match self {
            Tier::Explicit => "Tier.Explicit",
            Tier::Enclosing => "Tier.Enclosing",
            Tier::NsAuto => "Tier.NsAuto",
            Tier::ModAuto => "Tier.ModAuto.Ops",
            Tier::Contested => "Tier.Contested",
            Tier::Root => "",
        }
    }

    /// Whether this tier's plants live in the decoy assembly rather than the
    /// contributor's.
    pub fn in_decoy(self) -> bool {
        matches!(self, Tier::Root)
    }
}

/// How the probe reaches the planted name. The sweep's second dimension:
/// the two forms travel different channels into bare scope, and a surface
/// query can model one and be blind to the other.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Form {
    /// The plant is a **type**, written bare: `type X = TExEn`.
    Bare,
    /// The plant is a **container** — a child namespace, or a nested module
    /// under the module-shaped target — holding a `Marker` type, written as a
    /// dotted head: `type X = DExEn.Marker`. A child namespace is not a member
    /// of the namespace that contains it, so nothing that enumerates members
    /// sees this channel.
    DottedHead,
}

/// The generic-arity shape of a plant: what arity each declaring tier declares
/// it at, and what arity the probe writes. The sweep's third dimension.
///
/// Orthogonal to [`Form`], because the arity belongs to the **leaf**, not to
/// the head: a [`Form::DottedHead`] container is a namespace or a module and so
/// cannot be generic, but the [`MARKER`] type it holds can, and
/// `assembly_type_path_core` keys the *final* segment on the written arity
/// while every segment above it is looked up at arity 0. Probing the dotted
/// channel only at arity 0 would leave that split — head-tier precedence versus
/// final-segment arity — unmeasured (codex review).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Arity {
    /// Non-generic at every declaring tier, probed bare. Every tier holds the
    /// written arity.
    Mono,
    /// Generic at every declaring tier, probed applied (`X<int>`). [`Mono`]
    /// repeated one arity up — the only family that exercises an arity
    /// *comparison* rather than `0 == 0`, on either oracle.
    ///
    /// [`Mono`]: Arity::Mono
    Generic,
    /// Generic at every declaring tier, probed **bare**. No tier holds the
    /// written arity, so FCS has nothing to prefer and falls back to a
    /// generic — with an arity error, and the use still reported. This is the
    /// family the resolver's arity-fallback branch exists for, and the one
    /// that says *which* tier the fallback reaches.
    Fallback,
    /// Generic at the first declaring tier (in [`Tier::ALL`] order),
    /// non-generic at the rest, probed bare: exactly one tier holds the
    /// written arity, and it is not the first.
    GenericFirst,
    /// The mirror of [`GenericFirst`]: non-generic at the first declaring
    /// tier, generic at the rest. Together the two put the exact-arity
    /// occupant on each side of every tier pair, which is what turns "arity
    /// preference is a fallback, not a filter" from a claim into a
    /// measurement.
    ///
    /// [`GenericFirst`]: Arity::GenericFirst
    GenericRest,
}

impl Arity {
    /// Whether this shape needs two declaring tiers to say anything: the
    /// split families are defined by which side of a contest the exact-arity
    /// occupant sits on, and a singleton has no sides.
    fn needs_two_tiers(self) -> bool {
        matches!(self, Arity::GenericFirst | Arity::GenericRest)
    }
}

/// A same-named entity hidden behind one of the coarse channels the tier walk
/// vetoes on. The sweep's fourth dimension.
///
/// Crossed with [`Form`] this is a 2×2 of *distinct claims*, not a repetition:
/// each channel is keyed on a name, and whether the thing wearing that name can
/// actually bind the probe depends on the form.
///
/// |                | [`HiddenType`]                       | [`HiddenModule`]                      |
/// |----------------|--------------------------------------|---------------------------------------|
/// | [`Bare`]       | a real shadow                        | a module cannot bind a bare type      |
/// | [`DottedHead`] | a type cannot own the dotted tail    | a real shadow                         |
///
/// The two "cannot bind" cells are where a name-keyed veto over-defers: the
/// metadata records the name, FCS falls through to the visible plant, and
/// nothing but this sweep says so.
///
/// The three project variants are a square of their own, over what the module
/// *holds*:
///
/// |                | holds nothing of the name | holds it as a **type** | holds it as a **module** |
/// |----------------|---------------------------|------------------------|--------------------------|
/// | [`Bare`]       | `…P`, must commit         | `…Q`, must decline     | `…R`, must commit        |
/// | [`DottedHead`] | `…P`, must commit         | `…R`, must commit      | `…Q`, must decline       |
///
/// The `P` column is the keying claim: a veto that reads the module's mere
/// presence answers the same there as in `Q`, and a veto that reads its names
/// cannot. The `Q`/`R` pair is the *form* claim, and it is the only reason the
/// bare and dotted heads may consult different indices — `R` is exactly the
/// shape that wears the name but cannot bind the probe, so a head widened to
/// the other index fails there and nowhere else.
///
/// [`HiddenType`]: Risk::HiddenType
/// [`HiddenModule`]: Risk::HiddenModule
/// [`Bare`]: Form::Bare
/// [`DottedHead`]: Form::DottedHead
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Risk {
    /// Nothing hidden: the plant's own tiers are its only occupants.
    None,
    /// An assembly `[<AutoOpen>] module Hidden` declared *inside* the tier's
    /// namespace, holding a **type** of the plant's name — the exact,
    /// name-keyed channel `auto_open_modules_in_namespace_shadow_type_named`
    /// reads.
    HiddenType(Tier),
    /// The same module, holding a **module** of the plant's name (which in turn
    /// holds a [`MARKER`]), so it can own a dotted tail. The same channel: the
    /// predicate counts public children by name and does not distinguish the
    /// two shapes.
    HiddenModule(Tier),
    /// A project `[<AutoOpen>]` module in the probe's own enclosing namespace
    /// holding **nothing of the plant's name** — the cost cell of the project
    /// channel: whatever declines here is a binding lost for no hazard at all,
    /// so every one of these must commit.
    ProjectAutoOpen,
    /// The same module, **holding the plant's name** — a type for a
    /// [`Form::Bare`] plant, a module holding a [`MARKER`] for a
    /// [`Form::DottedHead`] one. The hazard cell: FCS binds the project entity
    /// over every assembly tier, so a bare plant must decline and a dotted one
    /// (which today does not) is a wrong target.
    ///
    /// Which *mechanism* declines a bare cell is not pinned here and cannot be:
    /// a probe is one file, so the file-global auto-open name set reaches these
    /// uses as readily as the namespace-keyed
    /// `project_shadow_at` does. The cross-file case that separates
    /// them lives in `resolve_fsharp_abbrev`.
    ProjectAutoOpenHiding,
    /// The same module holding the plant's name in the shape that **cannot**
    /// bind the probe's form: a module for a [`Form::Bare`] plant, a type for a
    /// [`Form::DottedHead`] one. FCS falls through to the visible plant in both
    /// (fsc-verified — a record `type Demo` in an auto-open module leaves
    /// `Demo.CasePat.Shape` compiling against the referenced namespace), so
    /// every one of these must commit.
    ///
    /// This is the off-diagonal of the project square, and it is what makes the
    /// channel's keying *by form* a measured property rather than a documented
    /// one: widening the bare head to the module index fails the `S…R` cells,
    /// and widening the dotted head to the type index fails the `V…R` cells.
    /// Without them the only guard is a single hand-written case.
    ProjectAutoOpenHidingWrongShape,
}

impl Risk {
    /// The tier whose namespace carries the risk. `None` when nothing is
    /// hidden; [`Tier::Enclosing`] for the project channel, which can only sit
    /// in the probe's own namespace.
    pub fn tier(self) -> Option<Tier> {
        match self {
            Risk::None => None,
            Risk::HiddenType(t) | Risk::HiddenModule(t) => Some(t),
            Risk::ProjectAutoOpen
            | Risk::ProjectAutoOpenHiding
            | Risk::ProjectAutoOpenHidingWrongShape => Some(Tier::Enclosing),
        }
    }

    /// The suffix that names this risk inside a planted name.
    fn tag(self) -> String {
        match self {
            Risk::None => String::new(),
            Risk::HiddenType(t) => format!("Y{}", t.tag()),
            Risk::HiddenModule(t) => format!("M{}", t.tag()),
            Risk::ProjectAutoOpen => "P".to_string(),
            Risk::ProjectAutoOpenHiding => "Q".to_string(),
            Risk::ProjectAutoOpenHidingWrongShape => "R".to_string(),
        }
    }
}

/// The tiers a risk may be planted at: those whose namespace the walk visits as
/// a *prefix*, which is the only place the channel under test can fire.
/// [`Tier::ModAuto`]'s target is a module, so its namespace is never a prefix;
/// and every [`Tier::Contested`] case already declines for a reason of its own,
/// so hiding something there would buy 24 rows that say nothing about the veto.
const RISK_TIERS: [Tier; 4] = [Tier::Explicit, Tier::Enclosing, Tier::NsAuto, Tier::Root];

/// The `[<AutoOpen>]` module a risk hides its entity in — one per namespace,
/// holding every hidden entity planted there.
const HIDDEN: &str = "Hidden";

/// The type every [`Form::DottedHead`] container holds — the probe's leaf, and
/// the use both oracles are asked about.
pub const MARKER: &str = "Marker";

/// Every `(form, arity)` family and the letter its plant names start with.
/// Each plants the whole tier matrix independently, so a name never carries
/// two shapes.
const FAMILIES: [(Form, Arity, char); 10] = [
    (Form::Bare, Arity::Mono, 'T'),
    (Form::DottedHead, Arity::Mono, 'D'),
    (Form::Bare, Arity::Generic, 'G'),
    (Form::Bare, Arity::Fallback, 'W'),
    (Form::Bare, Arity::GenericFirst, 'F'),
    (Form::Bare, Arity::GenericRest, 'R'),
    // The dotted channel at every arity shape the bare one runs: the arity is
    // the leaf's, so the head's tier contest and the leaf's arity keying are
    // exercised together.
    (Form::DottedHead, Arity::Generic, 'H'),
    (Form::DottedHead, Arity::Fallback, 'J'),
    (Form::DottedHead, Arity::GenericFirst, 'K'),
    (Form::DottedHead, Arity::GenericRest, 'L'),
];

/// The two families the [`Risk`] dimension runs, and the letter their plant
/// names start with. Both are [`Arity::Mono`] and their contenders are
/// singletons: the tier-versus-tier contest is what [`FAMILIES`] measures, and
/// `D`/`G`/`H` already established that a tier error is neither form- nor
/// arity-specific — so a risk plant puts exactly one *visible* tier against one
/// *hidden* entity, with nothing else in play.
const RISK_FAMILIES: [(Form, char); 2] = [(Form::Bare, 'S'), (Form::DottedHead, 'V')];

/// The `<AssemblyName>` of the contributor fixture.
pub const CONTRIBUTOR_ASM: &str = "SemaTierFixture";
/// The `<AssemblyName>` of the decoy fixture.
pub const DECOY_ASM: &str = "SemaTierDecoyFixture";

/// One planted name, the tiers that declare it, and how it is reached.
#[derive(Clone, Debug)]
pub struct Plant {
    pub name: String,
    pub tiers: Vec<Tier>,
    pub form: Form,
    pub arity: Arity,
    pub risk: Risk,
}

impl Plant {
    /// The generic arity this plant is declared at, at `tier`. Derived from
    /// [`Plant::arity`] rather than stored per tier, so there is no parallel
    /// vector to keep in step with `tiers`.
    pub fn declared_arity(&self, tier: Tier) -> usize {
        let is_first = self.tiers.first() == Some(&tier);
        match self.arity {
            Arity::Mono => 0,
            Arity::Generic | Arity::Fallback => 1,
            Arity::GenericFirst => usize::from(is_first),
            Arity::GenericRest => usize::from(!is_first),
        }
    }

    /// The generic arity the probe writes. Only [`Arity::Generic`] applies
    /// type arguments; every other shape is probed bare, which is where the
    /// fallback question lives.
    pub fn written_arity(&self) -> usize {
        match self.arity {
            Arity::Generic => 1,
            _ => 0,
        }
    }

    /// The `(assembly, full name)` FCS reports when `tier` is the winner — the
    /// sweep's expectation once the oracle has named a tier.
    pub fn declaration(&self, tier: Tier) -> (&'static str, String) {
        let asm = if tier.in_decoy() {
            DECOY_ASM
        } else {
            CONTRIBUTOR_ASM
        };
        let ns = tier.namespace();
        let mut full = if ns.is_empty() {
            self.name.clone()
        } else {
            format!("{ns}.{}", self.name)
        };
        if self.form == Form::DottedHead {
            full.push('.');
            full.push_str(MARKER);
        }
        (asm, full)
    }

    /// The `(assembly, full name)` FCS reports when the plant's [`Risk`] wins —
    /// `None` when nothing hidden *can* win, which is the claim the two
    /// "cannot bind" cells of the [`Risk`] × [`Form`] square make: a module
    /// cannot bind a bare type reference, a type cannot own a dotted tail, and
    /// the project channel hides nothing of this name at all. So a `None` here
    /// says the visible plant is the only thing FCS can bind, and any decline
    /// against it is pure cost.
    pub fn hidden_declaration(&self) -> Option<(&'static str, String)> {
        let tier = match (self.risk, self.form) {
            (Risk::HiddenType(t), Form::Bare) | (Risk::HiddenModule(t), Form::DottedHead) => t,
            _ => return None,
        };
        let asm = if tier.in_decoy() {
            DECOY_ASM
        } else {
            CONTRIBUTOR_ASM
        };
        let ns = tier.namespace();
        let mut full = if ns.is_empty() {
            format!("{HIDDEN}.{}", self.name)
        } else {
            format!("{ns}.{HIDDEN}.{}", self.name)
        };
        if self.form == Form::DottedHead {
            full.push('.');
            full.push_str(MARKER);
        }
        Some((asm, full))
    }

    /// The **full name** FCS reports when a [`Risk::ProjectAutoOpenHiding`]
    /// plant's project-side declaration wins — the probe file's own
    /// `[<AutoOpen>] module Hidden`.
    ///
    /// A full name and not an `(assembly, full name)` pair like
    /// [`Plant::declaration`]: this entity is compiled as part of the probe, so
    /// its assembly is whatever FCS names the throwaway compilation, which is
    /// not a fixture constant. Nothing collides — a plant has an assembly-side
    /// hidden declaration or a project-side one, never both.
    pub fn project_hidden_full_name(&self) -> Option<String> {
        if self.risk != Risk::ProjectAutoOpenHiding {
            return None;
        }
        let mut full = format!("{}.{HIDDEN}.{}", Tier::Enclosing.namespace(), self.name);
        if self.form == Form::DottedHead {
            full.push('.');
            full.push_str(MARKER);
        }
        Some(full)
    }

    /// The type expression the probe writes.
    pub fn probe_expr(&self) -> String {
        let head = match self.form {
            Form::Bare => self.name.clone(),
            Form::DottedHead => format!("{}.{MARKER}", self.name),
        };
        match self.written_arity() {
            0 => head,
            n => format!("{head}<{}>", vec!["int"; n].join(", ")),
        }
    }

    /// The identifier whose use both oracles are compared at — the leaf, which
    /// is the segment that names a *type* in either form.
    pub fn probed_ident(&self) -> &str {
        match self.form {
            Form::Bare => &self.name,
            Form::DottedHead => MARKER,
        }
    }

    /// The key this plant's case is recorded under, per reference order.
    pub fn key(&self, order: &str) -> String {
        format!("{}/{order}", self.name)
    }
}

/// Every plant: for each [`FAMILIES`] entry, one per tier (a control — nothing
/// contends, so both sides must simply find it) and one per unordered pair (the
/// contest the sweep exists for); then, for each [`RISK_FAMILIES`] entry, one
/// per (tier, [`Risk`]) — the visible-versus-hidden contest. The split-arity
/// families are pairs only.
pub fn corpus() -> Vec<Plant> {
    let mut out = Vec::new();
    for (form, arity, prefix) in FAMILIES {
        for (i, &a) in Tier::ALL.iter().enumerate() {
            if !arity.needs_two_tiers() {
                out.push(Plant {
                    name: format!("{prefix}{}", a.tag()),
                    tiers: vec![a],
                    form,
                    arity,
                    risk: Risk::None,
                });
            }
            for &b in &Tier::ALL[i + 1..] {
                out.push(Plant {
                    name: format!("{prefix}{}{}", a.tag(), b.tag()),
                    tiers: vec![a, b],
                    form,
                    arity,
                    risk: Risk::None,
                });
            }
        }
    }
    for (form, prefix) in RISK_FAMILIES {
        for &tier in &Tier::ALL {
            for risk in RISK_TIERS
                .iter()
                .flat_map(|&t| [Risk::HiddenType(t), Risk::HiddenModule(t)])
                .chain([
                    Risk::ProjectAutoOpen,
                    Risk::ProjectAutoOpenHiding,
                    Risk::ProjectAutoOpenHidingWrongShape,
                ])
            {
                out.push(Plant {
                    name: format!("{prefix}{}{}", tier.tag(), risk.tag()),
                    tiers: vec![tier],
                    form,
                    arity: Arity::Mono,
                    risk,
                });
            }
        }
    }
    out
}

/// Every plant `tier` declares, in corpus order.
fn plants_at(plants: &[Plant], tier: Tier, form: Form) -> Vec<&Plant> {
    plants
        .iter()
        .filter(|p| p.form == form && p.tiers.contains(&tier))
        .collect()
}

/// The generic parameter list a declaration at `arity` carries, empty at 0.
fn generic_params(arity: usize) -> String {
    if arity == 0 {
        return String::new();
    }
    let params: Vec<String> = (0..arity).map(|i| format!("'T{i}")).collect();
    format!("<{}>", params.join(", "))
}

/// The [`Form::Bare`] declarations for one tier, at `indent`. Each plant is
/// declared at *its own* arity for this tier, which is what makes a split
/// family split.
fn bare_decls(plants: &[Plant], tier: Tier, indent: &str) -> String {
    let mut out = String::new();
    for p in plants_at(plants, tier, Form::Bare) {
        out.push_str(&format!(
            "{indent}type {}{}() =\n{indent}    member _.Tier = \"{tier:?}\"\n",
            p.name,
            generic_params(p.declared_arity(tier)),
        ));
    }
    out
}

/// The `[<AutoOpen>]` module holding every entity hidden at `tier`, to be
/// emitted inside that tier's `namespace` block. Empty when no plant hides
/// anything there — F# has no empty module.
///
/// The hidden entity's *shape* follows the [`Risk`] and not the plant's
/// [`Form`], which is what makes the square's two "cannot bind" cells: a
/// [`Risk::HiddenType`] plant reached as a dotted head finds a type where it
/// needs a container, and a [`Risk::HiddenModule`] plant reached bare finds a
/// module where it needs a type.
fn hidden_auto_open_module(plants: &[Plant], tier: Tier) -> String {
    let mut body = String::new();
    for p in plants {
        match p.risk {
            Risk::HiddenType(t) if t == tier => body.push_str(&format!(
                "    type {}() =\n        member _.Tier = \"hidden\"\n",
                p.name
            )),
            Risk::HiddenModule(t) if t == tier => body.push_str(&format!(
                "    module {} =\n        type {MARKER}() =\n            member _.Tier = \"hidden\"\n",
                p.name
            )),
            _ => {}
        }
    }
    if body.is_empty() {
        return String::new();
    }
    format!("\n[<AutoOpen>]\nmodule {HIDDEN} =\n{body}")
}

/// The [`Form::DottedHead`] containers for one tier, as **child namespaces**
/// of `parent`. Written as sibling `namespace` declarations because that is
/// the channel under test: a child namespace is reachable as a dotted head
/// through an `open` of its parent without being a member of it.
fn child_namespace_containers(plants: &[Plant], tier: Tier, parent: &str) -> String {
    let mut out = String::new();
    for p in plants_at(plants, tier, Form::DottedHead) {
        let ns = if parent.is_empty() {
            p.name.clone()
        } else {
            format!("{parent}.{}", p.name)
        };
        out.push_str(&format!(
            "\nnamespace {ns}\n\ntype {MARKER}{}() =\n    member _.Tier = \"{tier:?}\"\n",
            generic_params(p.declared_arity(tier)),
        ));
    }
    out
}

/// `Generated.fs` for `tests/fixtures/tier_env` — the contributor.
///
/// The three `[<assembly: AutoOpen>]` attributes ride at the end of the last
/// namespace: F# accepts an assembly-attributed `do` binding there, and it is
/// where the checked-in `autoopen_env` fixture puts its own.
pub fn contributor_source(plants: &[Plant]) -> String {
    let mut out = String::from(
        "// GENERATED by crates/sema/tests/all/common/tier_corpus.rs — do not edit.\n",
    );
    for tier in [Tier::Explicit, Tier::Enclosing, Tier::NsAuto] {
        out.push_str(&format!("\nnamespace {}\n\n", tier.namespace()));
        out.push_str(&bare_decls(plants, tier, ""));
        // Before the child namespaces: each of those opens a `namespace` block
        // of its own, which ends this one.
        out.push_str(&hidden_auto_open_module(plants, tier));
        out.push_str(&child_namespace_containers(plants, tier, tier.namespace()));
    }
    // The module-shaped target: a namespace holding the auto-opened module.
    // Its dotted heads are *nested modules*, not child namespaces — a module
    // has no child namespaces, and a nested module is the shape an `open` of
    // it makes a bare dotted head.
    out.push_str("\nnamespace Tier.ModAuto\n\nmodule Ops =\n");
    out.push_str(&bare_decls(plants, Tier::ModAuto, "    "));
    for p in plants_at(plants, Tier::ModAuto, Form::DottedHead) {
        out.push_str(&format!(
            "\n    module {} =\n        type {MARKER}{}() =\n            member _.Tier = \"ModAuto\"\n",
            p.name,
            generic_params(p.declared_arity(Tier::ModAuto)),
        ));
    }
    out.push_str("\nnamespace Tier.Contested\n\n");
    out.push_str(&bare_decls(plants, Tier::Contested, ""));
    out.push_str(&child_namespace_containers(
        plants,
        Tier::Contested,
        "Tier.Contested",
    ));
    out.push_str(
        "\nnamespace Tier.Contested\n\n\
         [<assembly: AutoOpen(\"Tier.NsAuto\")>]\n\
         [<assembly: AutoOpen(\"Tier.ModAuto.Ops\")>]\n\
         [<assembly: AutoOpen(\"Tier.Contested\")>]\n\
         do ()\n",
    );
    out
}

/// `Generated.fs` for `tests/fixtures/tier_decoy_env` — the root plants, plus
/// the re-declaration of `Tier.Contested` that makes the contributor's
/// auto-open of it contested.
pub fn decoy_source(plants: &[Plant]) -> String {
    let mut out = String::from(
        "// GENERATED by crates/sema/tests/all/common/tier_corpus.rs — do not edit.\n\
         \nnamespace global\n\n",
    );
    out.push_str(&bare_decls(plants, Tier::Root, ""));
    out.push_str(&hidden_auto_open_module(plants, Tier::Root));
    out.push_str(&child_namespace_containers(plants, Tier::Root, ""));
    // Declares the contested namespace and nothing the contributor plants:
    // only the contributor's content enters scope through the auto-open, so a
    // same-named decoy here would test something else entirely.
    out.push_str(
        "\nnamespace Tier.Contested\n\n\
         type DecoyOnlyMarker() =\n    member _.Tier = \"decoy\"\n",
    );
    out
}

/// The probe for one plant. Deliberately **uniform** across the corpus — the
/// enclosing namespace and the explicit `open` are always present — so the only
/// things that vary between cases are which tiers declare the name, which form
/// reaches it, which assembly is referenced first, and the plant's [`Risk`]. A
/// per-case probe shape would let a template difference masquerade as a tier
/// difference.
///
/// The two project risks are the ones that have to live here rather than in a
/// fixture assembly, because they are *project* declarations. They differ only
/// in what the module holds — a name no plant wears
/// ([`Risk::ProjectAutoOpen`]) versus this plant's own
/// ([`Risk::ProjectAutoOpenHiding`]) — which is the pair that separates a veto
/// keyed on the names a project actually declares from one keyed on a module's
/// mere presence: the first must commit, the second must not.
pub fn probe_source(plant: &Plant) -> String {
    let hidden_body = match plant.risk {
        Risk::ProjectAutoOpen => {
            Some("    type ProjectHidden() =\n        member _.Tier = \"project\"\n".to_string())
        }
        Risk::ProjectAutoOpenHiding => Some(match plant.form {
            Form::Bare => format!(
                "    type {}() =\n        member _.Tier = \"project\"\n",
                plant.name
            ),
            Form::DottedHead => format!(
                "    module {} =\n        type {MARKER}() =\n            member _.Tier = \"project\"\n",
                plant.name
            ),
        }),
        // The shape that cannot bind the probe's form: a module where the probe
        // writes a bare type annotation, a type where it writes a dotted head.
        Risk::ProjectAutoOpenHidingWrongShape => Some(match plant.form {
            Form::Bare => format!(
                "    module {} =\n        type {MARKER}() =\n            member _.Tier = \"project\"\n",
                plant.name
            ),
            Form::DottedHead => format!(
                "    type {}() =\n        member _.Tier = \"project\"\n",
                plant.name
            ),
        }),
        Risk::None | Risk::HiddenType(_) | Risk::HiddenModule(_) => None,
    };
    let project_risk = hidden_body
        .map(|body| format!("[<AutoOpen>]\nmodule {HIDDEN} =\n{body}\n"))
        .unwrap_or_default();
    format!(
        "namespace Tier.Enclosing\n\nopen Tier.Explicit\n\n{project_risk}module Probe =\n    type X = {}\n",
        plant.probe_expr()
    )
}

/// The byte range of [`Plant::probed_ident`] inside [`probe_source`] — the span
/// both oracles are asked about.
pub fn probe_use_span(src: &str, plant: &Plant) -> (usize, usize) {
    let ident = plant.probed_ident();
    let start = src.rfind(ident).expect("probe template names the plant");
    (start, start + ident.len())
}
