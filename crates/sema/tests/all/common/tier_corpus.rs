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
/// contest the sweep exists for). The split-arity families are pairs only.
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
                });
            }
            for &b in &Tier::ALL[i + 1..] {
                out.push(Plant {
                    name: format!("{prefix}{}{}", a.tag(), b.tag()),
                    tiers: vec![a, b],
                    form,
                    arity,
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
/// enclosing namespace and the explicit `open` are always present — so the
/// only things that vary between cases are which tiers declare the name, which
/// form reaches it, and which assembly is referenced first. A per-case probe
/// shape would let a template difference masquerade as a tier difference.
pub fn probe_source(plant: &Plant) -> String {
    format!(
        "namespace Tier.Enclosing\n\nopen Tier.Explicit\n\nmodule Probe =\n    type X = {}\n",
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
