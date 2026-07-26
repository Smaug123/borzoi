//! The **companion corpus**: the F# source for the `SemaCompanionFixture`
//! assembly `companion_head_diff` probes, generated from the dimension enums so
//! the F# universe and the Rust side's matrix cannot drift apart (the
//! `tier_corpus` pattern).
//!
//! One `(namespace, simple name)` in a referenced assembly can hold **several**
//! entities at once: a type and its companion module (`type TypeInfo<'a,'b>`
//! beside `[<RequireQualifiedAccess>] module TypeInfo`, which the compiler emits
//! as `TypeInfoModule` but F# source still spells `TypeInfo`), and types at
//! several generic arities. A dotted path's *head* therefore does not name a
//! candidate — it names a **candidate set** — and which member of that set FCS
//! binds depends on where the *tail* lives. That question is orthogonal to the
//! tier ladder [`tier_corpus`](super::tier_corpus) measures: the ladder says
//! which namespace wins, this corpus says which of a namespace's same-named
//! entities does.
//!
//! It is a real wrong answer, not a hypothetical: on `WoofWare.PawPrint`'s main
//! library `TypeInfo.NominallyEqual` (a static member of the record) and
//! `MethodBody.Il` (a case of the union) bound `System.Reflection.TypeInfo` /
//! `System.Reflection.MethodBody` from an `open System.Reflection`, because the
//! reading in the file's *own* namespace picked the companion module out of the
//! candidate set, found no such member on it, and ceded the path to the
//! higher-priority open's partial reading.
//!
//! ## The dimensions
//!
//! - [`Holder`] — which entities the plant's namespace declares under the name.
//! - [`Arity`] — the type's declared generic arity. The probe **always** writes
//!   the name bare, so a generic plant is reached at an arity nothing declares;
//!   FCS infers it (`FS1125`) rather than failing, and an arity-keyed lookup
//!   that filters instead of preferring never finds the type at all.
//! - [`Tail`] — which of the two candidates declares the probed leaf. `Both`
//!   is the tie-break cell (fsi-measured: the **module** wins), `Neither` the
//!   cell that must not commit anything.
//! - [`TypeShape`] — what the *type* contributes the leaf as: a static method, a
//!   static property, a union case with a field, or a nullary union case. Each
//!   compiles to different metadata (a method; a property; a `New…` factory plus
//!   a nested carrier; a static property), and a lookup keyed on one surface is
//!   blind to the others. A fifth shape, a constructible class, exists for the
//!   terminal position, which has no leaf to contribute.
//! - [`Position`] — expression, pattern, or **terminal**. A pattern head is a
//!   lookup in F#'s *constructor* namespace, where a module value cannot compete
//!   at all; a terminal head asks for no leaf, so every candidate trivially
//!   supplies the path and only the candidate order decides — which is why a
//!   module must not own one (FCS wants a value there, and a module is not one).
//! - `decoy` — whether an explicitly `open`ed namespace declares a same-named
//!   type that does **not** hold the leaf. This is the tier interaction the
//!   PawPrint sites hit: the decoy outranks the plant, so only a tail-sensitive
//!   walk reaches the plant.

/// What the plant's namespace declares under the plant's simple name.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Holder {
    /// A type alone.
    TypeOnly,
    /// A module alone.
    ModuleOnly,
    /// A type **and** its companion module — the shape whose candidate set has
    /// two members at one name.
    Both,
}

impl Holder {
    pub const ALL: [Holder; 3] = [Holder::TypeOnly, Holder::ModuleOnly, Holder::Both];

    fn tag(self) -> &'static str {
        match self {
            Holder::TypeOnly => "T",
            Holder::ModuleOnly => "M",
            Holder::Both => "B",
        }
    }

    fn has_type(self) -> bool {
        matches!(self, Holder::TypeOnly | Holder::Both)
    }

    fn has_module(self) -> bool {
        matches!(self, Holder::ModuleOnly | Holder::Both)
    }
}

/// The type's declared generic arity. The module is always non-generic, and the
/// probe always writes the head bare.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Arity {
    /// Non-generic: the written (bare) arity is the declared one.
    Mono,
    /// One type parameter, so **nothing** at the name holds the written arity.
    Generic,
}

impl Arity {
    pub const ALL: [Arity; 2] = [Arity::Mono, Arity::Generic];

    fn tag(self) -> &'static str {
        match self {
            Arity::Mono => "0",
            Arity::Generic => "1",
        }
    }

    fn params(self) -> &'static str {
        match self {
            Arity::Mono => "",
            Arity::Generic => "<'T>",
        }
    }

    /// The type a field of the plant's type carries — the type parameter when
    /// there is one, so a generic plant does not declare an unused parameter.
    fn payload(self) -> &'static str {
        match self {
            Arity::Mono => "int",
            Arity::Generic => "'T",
        }
    }
}

/// Which candidate declares the probed leaf.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Tail {
    /// The type only.
    Type,
    /// The companion module only.
    Module,
    /// Both — the tie-break.
    Both,
    /// Neither: the name is occupied but the leaf is genuinely absent, so no
    /// reading here owns the path.
    Neither,
}

impl Tail {
    pub const ALL: [Tail; 4] = [Tail::Type, Tail::Module, Tail::Both, Tail::Neither];

    fn tag(self) -> &'static str {
        match self {
            Tail::Type => "T",
            Tail::Module => "M",
            Tail::Both => "B",
            Tail::Neither => "N",
        }
    }

    fn on_type(self) -> bool {
        matches!(self, Tail::Type | Tail::Both)
    }

    fn on_module(self) -> bool {
        matches!(self, Tail::Module | Tail::Both)
    }
}

/// What the *type* contributes the probed leaf as. Each compiles to a different
/// metadata surface, so a resolver that reads one is blind to the others.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum TypeShape {
    /// A record with a static **method** — `WoofWare.PawPrint.TypeInfo`'s shape
    /// (`TypeInfo.NominallyEqual`).
    StaticMethod,
    /// A record with a static **property**. A parameterless `static member` is a
    /// property, not a method, and the two are separate metadata tables.
    StaticProp,
    /// A union case carrying a field, compiled as a `New…` factory method plus a
    /// nested carrier type — `MethodBody.Il`'s shape. Every union here declares a
    /// **second** case, because fsc emits no nested carrier for a single-case
    /// union: that shape's case can only ever be deferred to, which would confound
    /// "we found the case" with "we could name it".
    FieldCase,
    /// A nullary union case, compiled as a static property —
    /// `MethodBody.Abstract`'s shape.
    NullaryCase,
    /// A class with a primary constructor, carrying nothing the probe asks for.
    /// [`Position::Terminal`]'s shape and only its: a terminal head must name a
    /// *value* for FCS to report anything there at all, and a record or union
    /// written bare is `FS0800` with no symbol — measured, not assumed.
    Ctor,
}

impl TypeShape {
    pub const ALL: [TypeShape; 5] = [
        TypeShape::StaticMethod,
        TypeShape::StaticProp,
        TypeShape::FieldCase,
        TypeShape::NullaryCase,
        TypeShape::Ctor,
    ];

    fn tag(self) -> &'static str {
        match self {
            TypeShape::StaticMethod => "S",
            TypeShape::StaticProp => "R",
            TypeShape::FieldCase => "F",
            TypeShape::NullaryCase => "U",
            TypeShape::Ctor => "C",
        }
    }

    /// Whether this shape puts the leaf in F#'s *constructor* namespace, the
    /// only one a pattern head searches.
    fn is_case(self) -> bool {
        matches!(self, TypeShape::FieldCase | TypeShape::NullaryCase)
    }
}

/// Where the probe writes the path.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Position {
    /// `let probeResult = <head>.Tail`.
    Expr,
    /// `match x with | <head>.Tail … -> 1 | _ -> 0`.
    Pattern,
    /// `let probeResult = Companion.Enclosing.<head>` — the head **terminates**
    /// the path, fully qualified so it is still the assembly walk and not a bare
    /// name. No tail is walked, so every candidate at the name trivially
    /// "supplies" it and the choice falls entirely to the candidate order: the
    /// shape where preferring a module is wrong, since FCS's expression-position
    /// lookup wants a value and a module is not one (codex review).
    Terminal,
}

impl Position {
    pub const ALL: [Position; 3] = [Position::Expr, Position::Pattern, Position::Terminal];

    fn tag(self) -> &'static str {
        match self {
            Position::Expr => "E",
            Position::Pattern => "P",
            Position::Terminal => "T",
        }
    }
}

/// The namespace the plants — and the probe file — live in: the plant is reached
/// through the probe's own **enclosing namespace**, the tier the decoy's
/// explicit `open` outranks.
pub const PLANT_NS: &str = "Companion.Enclosing";
/// The namespace holding the same-named decoys, which a decoy cell `open`s.
pub const DECOY_NS: &str = "Companion.Decoy";
/// The leaf every probe asks for.
pub const TAIL: &str = "Tail";
/// The fixture's `<AssemblyName>`.
pub const FIXTURE_ASM: &str = "SemaCompanionFixture";

/// One cell: a `(namespace, name)` holding one of the [`Holder`] shapes, probed
/// through `<name>.Tail`.
#[derive(Clone, Debug)]
pub struct Plant {
    pub name: String,
    pub holder: Holder,
    pub arity: Arity,
    pub tail: Tail,
    pub shape: TypeShape,
    pub position: Position,
    pub decoy: bool,
}

impl Plant {
    /// Whether this combination is expressible in F# at all. The invalid ones
    /// are excluded rather than silently generated: a fixture that does not
    /// compile fails every cell, and a cell whose probe is nonsense measures the
    /// error recovery rather than the lookup.
    fn valid(&self) -> bool {
        // A leaf can only sit where the holder declares something.
        if self.tail.on_type() && !self.holder.has_type() {
            return false;
        }
        if self.tail.on_module() && !self.holder.has_module() {
            return false;
        }
        // With no type there is no generic arity to vary; pin it so the cell is
        // not generated twice. The shape is likewise unobservable, and is pinned
        // per position below.
        if !self.holder.has_type() && self.arity != Arity::Mono {
            return false;
        }
        // The constructible class exists for the terminal position and only for
        // it: it offers no leaf, and every other position asks for one.
        if (self.shape == TypeShape::Ctor) != (self.position == Position::Terminal) {
            return false;
        }
        if !self.holder.has_type()
            && !matches!(self.shape, TypeShape::StaticMethod | TypeShape::Ctor)
        {
            return false;
        }
        // A pattern head searches the constructor namespace: only a union case
        // can answer, so a pattern cell needs the type to contribute the leaf as
        // one. (A module value in pattern position is not a pattern at all —
        // that is an F# error, not a resolution question.)
        if self.position == Position::Pattern && !(self.shape.is_case() && self.tail.on_type()) {
            return false;
        }
        // A terminal head asks for no leaf, so where the leaf lives cannot vary
        // it; pin one value rather than generate the same cell four times. The
        // decoy, the holder and the arity still vary, which is the question.
        if self.position == Position::Terminal && self.tail != Tail::Neither {
            return false;
        }
        true
    }

    /// The plant's key in the sweep's tables.
    pub fn key(&self) -> &str {
        &self.name
    }

    /// Whether the probe's path type-checks. A [`Tail::Neither`] cell exists to
    /// pin that *nothing* owns the path, and F# rejects it — FCS then reports no
    /// symbol at the head either, so such a cell can hold no expectation about
    /// which candidate the head names. Deriving that from the corpus, rather than
    /// listing the cells, keeps the sweep from silencing a genuine
    /// "we commit where FCS binds nothing".
    pub fn path_type_checks(&self) -> bool {
        match self.position {
            // A terminal head is a *value* only when a constructible type
            // answers it: `Ns.C` on a class is its constructor, while a module
            // is not a value and a record/union written bare is `FS0800`. FCS
            // reports no symbol at all in those cases (measured with
            // `fcs-dump uses`), so the cell can hold no expectation there.
            Position::Terminal => self.holder.has_type(),
            _ => self.tail != Tail::Neither,
        }
    }

    /// The type declaration for this plant, empty when it has no type.
    fn type_decl(&self) -> String {
        if !self.holder.has_type() {
            return String::new();
        }
        let name = &self.name;
        let params = self.arity.params();
        let payload = self.arity.payload();
        match self.shape {
            TypeShape::StaticMethod => {
                let member = if self.tail.on_type() { TAIL } else { "Other" };
                format!(
                    "type {name}{params} =\n    {{ Payload : {payload} }}\n\
                     \n    static member {member} (arg : int) = \"type\"\n"
                )
            }
            TypeShape::StaticProp => {
                let member = if self.tail.on_type() { TAIL } else { "Other" };
                format!(
                    "type {name}{params} =\n    {{ Payload : {payload} }}\n\
                     \n    static member {member} = \"type\"\n"
                )
            }
            TypeShape::FieldCase => {
                let case = if self.tail.on_type() { TAIL } else { "Other" };
                format!("type {name}{params} =\n    | {case} of {payload}\n    | Carrier of int\n")
            }
            TypeShape::NullaryCase => {
                let case = if self.tail.on_type() { TAIL } else { "Other" };
                format!("type {name}{params} =\n    | {case}\n    | Carrier of {payload}\n")
            }
            TypeShape::Ctor => format!("type {name}{params}() =\n    member _.Which = \"type\"\n"),
        }
    }

    /// The companion-module declaration for this plant, empty when it has none.
    /// It follows the type, which is the order F# accepts for a companion pair.
    fn module_decl(&self) -> String {
        if !self.holder.has_module() {
            return String::new();
        }
        let name = &self.name;
        let value = if self.tail.on_module() { TAIL } else { "Other" };
        // F# lets a module share a **generic** type's name outright, but a
        // *non-generic* pair needs the explicit `ModuleSuffix` representation —
        // the shape `List`/`Option` take in FSharp.Core. Both compile to a
        // `…Module` IL name the source still spells bare, and only asking for it
        // where F# requires it keeps the generic pair's metadata exactly
        // `WoofWare.PawPrint.TypeInfo`'s.
        let suffix = if self.holder == Holder::Both && self.arity == Arity::Mono {
            "[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]\n"
        } else {
            ""
        };
        format!("{suffix}module {name} =\n    let {value} = \"module\"\n")
    }

    /// The decoy declaration — a same-named class in [`DECOY_NS`] holding
    /// nothing the probe asks for. Empty for a non-decoy cell.
    fn decoy_decl(&self) -> String {
        if !self.decoy {
            return String::new();
        }
        format!("type {}() =\n    member _.Decoy = 0\n", self.name)
    }

    /// The path the probe writes. A terminal cell writes the plant
    /// **fully qualified** and stops there: it must stay a dotted path (the
    /// assembly walk's subject) rather than become a bare name, which a
    /// different resolver answers.
    pub fn probe_path(&self) -> String {
        match self.position {
            Position::Terminal => format!("{PLANT_NS}.{}", self.name),
            _ => format!("{}.{TAIL}", self.name),
        }
    }

    /// The probe source for this cell.
    pub fn probe_source(&self) -> String {
        let open = if self.decoy {
            format!("open {DECOY_NS}\n\n")
        } else {
            String::new()
        };
        let path = self.probe_path();
        let body = match self.position {
            Position::Expr | Position::Terminal => {
                format!("module Probe =\n    let probeResult = {path}\n")
            }
            Position::Pattern => {
                // The case's arity decides whether the pattern takes an
                // argument; a mismatch is an F# error rather than a lookup.
                let arg = if self.shape == TypeShape::FieldCase {
                    " _"
                } else {
                    ""
                };
                format!(
                    "module Probe =\n    let probeFn x =\n        match x with\n        | {path}{arg} -> 1\n        | _ -> 0\n"
                )
            }
        };
        format!("namespace {PLANT_NS}\n\n{open}{body}")
    }

    /// The byte range of the **head** segment inside [`Self::probe_source`] —
    /// the span the candidate-set choice is observable at, and the one the
    /// PawPrint divergences were reported at.
    pub fn head_span(&self, src: &str) -> (usize, usize) {
        let path = self.probe_path();
        let at = src.rfind(&path).expect("probe writes the path");
        // The head is the plant's own segment, which a terminal path puts
        // *last* rather than first.
        match self.position {
            Position::Terminal => (at + path.len() - self.name.len(), at + path.len()),
            _ => (at, at + self.name.len()),
        }
    }

    /// The byte range of the **whole path**, where a resolved leaf is recorded.
    pub fn path_span(&self, src: &str) -> (usize, usize) {
        let path = self.probe_path();
        let at = src.rfind(&path).expect("probe writes the path");
        (at, at + path.len())
    }
}

/// Every valid cell, in a deterministic order.
pub fn corpus() -> Vec<Plant> {
    let mut out = Vec::new();
    for holder in Holder::ALL {
        for arity in Arity::ALL {
            for tail in Tail::ALL {
                for shape in TypeShape::ALL {
                    for position in Position::ALL {
                        for decoy in [false, true] {
                            let name = format!(
                                "C{}{}{}{}{}{}",
                                holder.tag(),
                                arity.tag(),
                                tail.tag(),
                                shape.tag(),
                                position.tag(),
                                if decoy { "D" } else { "X" },
                            );
                            let plant = Plant {
                                name,
                                holder,
                                arity,
                                tail,
                                shape,
                                position,
                                decoy,
                            };
                            if plant.valid() {
                                out.push(plant);
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// `Generated.fs` for `tests/fixtures/companion_env`.
pub fn fixture_source(plants: &[Plant]) -> String {
    let mut out = String::from(
        "// GENERATED by crates/sema/tests/all/common/companion_corpus.rs — do not edit.\n",
    );
    out.push_str(&format!("\nnamespace {PLANT_NS}\n\n"));
    for plant in plants {
        out.push_str(&plant.type_decl());
        out.push('\n');
        out.push_str(&plant.module_decl());
        out.push('\n');
    }
    out.push_str(&format!("\nnamespace {DECOY_NS}\n\n"));
    for plant in plants {
        out.push_str(&plant.decoy_decl());
    }
    // A namespace F# will not accept as empty: every cell may be non-decoy.
    out.push_str("type DecoyAnchor() =\n    member _.Anchor = 0\n");
    out
}
