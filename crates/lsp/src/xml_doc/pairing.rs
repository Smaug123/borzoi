//! Which signature declaration FCS pairs an implementation declaration with
//! (`SignatureConformance` in the F# compiler) — which decides whose doc FCS
//! shows for a declaration a signature constrains.
//!
//! FCS pairs per module or namespace (`checkModuleOrNamespaceContents`), in
//! three tables and two nested rules:
//!
//! - **types and exceptions**, by name and generic arity
//!   (`TypesByMangledName`);
//! - **modules**, by name — the containers here, so a declaration's key starts
//!   with the path of modules and namespaces around it;
//! - **values and members together**, by logical name
//!   (`AllValsAndMembersByLogicalNameUncached`): one candidate on each side
//!   pairs when their member parents agree; several are told apart by linkage
//!   key (parent, argument count, and type);
//! - within a paired **union**, cases by position once the counts agree
//!   (`List.forall2 checkUnionCase`); within a paired enum, by name.
//!
//! Types are out of reach here, so a pairing is committed only where names
//! decide it: exactly one declaration on each side with the same container,
//! table, member parent and name. Anything else in the name's group — an
//! overload, a namesake under another parent, a dispatch slot — is
//! [`Pairing::Ambiguous`]. A name the signature does not declare at all is
//! hidden by it: FCS pairs it with nothing ([`Pairing::NoPartner`]).
//!
//! This is FCS's pairing on code that conforms to its signature. A signature
//! that does not conform is an FCS error, and some of those pair differently:
//! a member whose argument count and type both differ from its signature's
//! pairs with nothing, where names alone pair it.

use std::collections::HashMap;

use borzoi_cst::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use borzoi_sema::{Def, DefKind};
use rowan::NodeOrToken;

use super::source::{
    ancestors, covering, ident_text, in_head_pattern, is_self_identifier, type_arity, unticked,
};

/// A type as FCS's type table keys it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TypeKey {
    /// Backticks stripped; an augmentation's dotted target joined with `.`.
    name: String,
    arity: usize,
}

/// Which of a container's name tables a declaration is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Table {
    /// Values and members, by logical name.
    Vals,
    /// Types and exceptions, by name.
    Types,
}

/// A name group: every declaration FCS looks a name up among.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Group {
    container: Vec<String>,
    table: Table,
    name: String,
}

/// What a declaration is within its [`Group`]; two declarations pair only
/// when their slots are equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Slot {
    /// A module-level `let` or `val`.
    Value,
    /// A member (method, property) of the type named.
    Member(TypeKey),
    /// An `abstract`, `override` or `default` member: dispatch slots are
    /// paired by `checkVirtualSlots`, overrides by a linkage key that also
    /// reads `MemberIsOverride` — neither modelled, so such a declaration
    /// only ever makes its group ambiguous.
    Dispatch(TypeKey),
    /// A type definition of this arity.
    Type(usize),
    /// An `exception` definition.
    Exception,
}

/// One file's pairable declarations.
#[derive(Debug, Default)]
pub(super) struct Declarations {
    groups: HashMap<Group, Vec<(Slot, Def)>>,
    /// Each type definition's cases, in declaration order.
    cases: HashMap<(Vec<String>, TypeKey), Vec<Def>>,
    /// Every declaration's name was read; when not, a name absent from
    /// [`Self::groups`] may yet be declared.
    complete: bool,
}

/// Whom FCS pairs an implementation declaration with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Pairing {
    /// The signature declaration (a binder of the signature file).
    Paired(Def),
    /// Nothing: the signature hides it, or it is not a declaration
    /// conformance pairs (a local, a parameter, a class `let`).
    NoPartner,
    /// Names do not decide the pairing.
    Ambiguous,
    /// The declaration's surface is not keyed here.
    Unmodelled,
}

impl Declarations {
    /// The declarations of a signature or implementation file. An
    /// implementation's module-level `let`s are not collected: FCS rejects
    /// two in one module (FS0037), so a value's slot is unique there.
    pub(super) fn new(root: &SyntaxNode) -> Self {
        let mut decls = Declarations {
            complete: true,
            ..Declarations::default()
        };
        for node in root.descendants() {
            match node.kind() {
                SyntaxKind::VAL_DECL if is_module_level(&node) => decls.add_val(&node),
                SyntaxKind::TYPE_DEFN => decls.add_type(&node),
                SyntaxKind::EXCEPTION_DEFN => decls.add_exception(&node),
                _ => {}
            }
        }
        decls
    }

    fn add(&mut self, container: Vec<String>, table: Table, slot: Slot, def: Def) {
        let group = Group {
            container,
            table,
            name: unticked(&def.name),
        };
        self.groups.entry(group).or_default().push((slot, def));
    }

    fn add_val(&mut self, decl: &SyntaxNode) {
        let Some(sig) = decl.children().find(|n| n.kind() == SyntaxKind::VAL_SIG) else {
            self.complete = false;
            return;
        };
        // An active pattern's logical name (`|A|_|`) cannot collide with any
        // other declaration's, and its implementation is never paired here.
        if sig
            .children()
            .any(|n| n.kind() == SyntaxKind::ACTIVE_PAT_NAME)
        {
            return;
        }
        match (container_of(decl), first_ident(&sig)) {
            (Some(container), Some(name)) => self.add(
                container,
                Table::Vals,
                Slot::Value,
                def_at(&name, DefKind::Value { is_function: false }),
            ),
            _ => self.complete = false,
        }
    }

    fn add_type(&mut self, defn: &SyntaxNode) {
        let (Some(container), Some(key), Some(name)) =
            (container_of(defn), type_key(defn), last_name_token(defn))
        else {
            self.complete = false;
            return;
        };
        if !is_augmentation(defn) {
            self.add(
                container.clone(),
                Table::Types,
                Slot::Type(key.arity),
                def_at(&name, DefKind::Type),
            );
        }
        for member in defn.descendants().filter(|n| is_member_node(n.kind())) {
            // A constructor: `new` heads its signature, and leaves a
            // definition's head without an identifier.
            if member
                .children_with_tokens()
                .any(|e| e.kind() == SyntaxKind::NEW_TOK)
            {
                continue;
            }
            let Some(token) = member_name_token(&member) else {
                if member.kind() != SyntaxKind::MEMBER_DEFN {
                    self.complete = false;
                }
                continue;
            };
            let slot = if is_dispatch(&member) {
                Slot::Dispatch(key.clone())
            } else {
                Slot::Member(key.clone())
            };
            self.add(
                container.clone(),
                Table::Vals,
                slot,
                def_at(&token, DefKind::Member),
            );
        }
        let cases = cases_of(defn);
        let mut defs = Vec::new();
        for case in &cases {
            let Some(token) = first_ident(case) else {
                self.complete = false;
                return;
            };
            let kind = if case.kind() == SyntaxKind::ENUM_CASE {
                DefKind::EnumCase
            } else {
                DefKind::UnionCase
            };
            defs.push(def_at(&token, kind));
        }
        if !defs.is_empty() {
            self.cases.insert((container, key), defs);
        }
    }

    fn add_exception(&mut self, defn: &SyntaxNode) {
        let case = defn.children().find(|n| n.kind() == SyntaxKind::UNION_CASE);
        match (container_of(defn), case.as_ref().and_then(first_ident)) {
            (Some(container), Some(name)) => self.add(
                container,
                Table::Types,
                Slot::Exception,
                def_at(&name, DefKind::ExceptionCase),
            ),
            _ => self.complete = false,
        }
    }

    fn exact(&self, group: &Group, slot: &Slot) -> Vec<&Def> {
        self.groups
            .get(group)
            .into_iter()
            .flatten()
            .filter(|(s, _)| s == slot)
            .map(|(_, d)| d)
            .collect()
    }

    /// The pairing when nothing in `group` has the slot sought: ambiguous if
    /// the group declares the name in another slot; hidden if it does not —
    /// which is certain only when every name was read.
    fn absent(&self, group: &Group) -> Pairing {
        if self.groups.contains_key(group) {
            Pairing::Ambiguous
        } else if self.complete {
            Pairing::NoPartner
        } else {
            Pairing::Unmodelled
        }
    }
}

/// Whom FCS pairs `def` with: a binder of the implementation file whose tree
/// is `root` and whose declarations are `imp`, paired among the signature's
/// declarations `sig`.
pub(super) fn pair(
    root: &SyntaxNode,
    def: &Def,
    imp: &Declarations,
    sig: &Declarations,
) -> Pairing {
    let Some(element) = covering(root, def.range) else {
        return Pairing::Unmodelled;
    };
    match implementation_slot(&element, def) {
        ImplSlot::NotPaired => Pairing::NoPartner,
        ImplSlot::Unmodelled => Pairing::Unmodelled,
        ImplSlot::Keyed { group, slot } => {
            let partners = sig.exact(&group, &slot);
            // A module-level value's slot is unique (FS0037); anything else
            // may be overloaded or declared twice.
            let ours = if slot == Slot::Value {
                1
            } else {
                imp.exact(&group, &slot).len()
            };
            match (partners.as_slice(), ours) {
                ([partner], 1) => Pairing::Paired((*partner).clone()),
                ([], _) => sig.absent(&group),
                _ => Pairing::Ambiguous,
            }
        }
        ImplSlot::Case {
            container,
            ty,
            cases,
        } => {
            let group = Group {
                container: container.clone(),
                table: Table::Types,
                name: ty.name.clone(),
            };
            let slot = Slot::Type(ty.arity);
            match (
                sig.exact(&group, &slot).len(),
                imp.exact(&group, &slot).len(),
            ) {
                (0, _) => return sig.absent(&group),
                (1, 1) => {}
                _ => return Pairing::Ambiguous,
            }
            // A signature type without cases hides them (an abstract or
            // non-union representation): `checkTypeRepr` pairs none.
            let Some(theirs) = sig.cases.get(&(container, ty)) else {
                return if sig.complete {
                    Pairing::NoPartner
                } else {
                    Pairing::Unmodelled
                };
            };
            let names = |defs: &[Def]| defs.iter().map(|d| unticked(&d.name)).collect::<Vec<_>>();
            let ours: Vec<String> = cases.iter().map(|c| unticked(c.text())).collect();
            if names(theirs) != ours {
                return Pairing::Ambiguous;
            }
            let name = unticked(&def.name);
            match theirs.iter().position(|d| unticked(&d.name) == name) {
                Some(i) if ours.iter().filter(|n| **n == name).count() == 1 => {
                    Pairing::Paired(theirs[i].clone())
                }
                _ => Pairing::Ambiguous,
            }
        }
    }
}

/// Where an implementation binder sits among the declarations conformance
/// pairs.
enum ImplSlot {
    NotPaired,
    Unmodelled,
    Keyed {
        group: Group,
        slot: Slot,
    },
    /// A union or enum case: its type, and that type's case names in order.
    Case {
        container: Vec<String>,
        ty: TypeKey,
        cases: Vec<SyntaxToken>,
    },
}

fn implementation_slot(element: &NodeOrToken<SyntaxNode, SyntaxToken>, def: &Def) -> ImplSlot {
    let name = unticked(&def.name);
    let keyed = |node: &SyntaxNode, table, slot| match container_of(node) {
        Some(container) => ImplSlot::Keyed {
            group: Group {
                container,
                table,
                name: name.clone(),
            },
            slot,
        },
        None => ImplSlot::Unmodelled,
    };
    match def.kind {
        DefKind::Parameter
        | DefKind::PatternLocal
        | DefKind::TypeParam
        | DefKind::ActivePatternCase => ImplSlot::NotPaired,
        DefKind::ActivePattern => ImplSlot::Unmodelled,
        // `member x.M`'s `x`, `type T() as x`'s `x`: locals.
        DefKind::Value { .. } if is_self_identifier(element, def.range) => ImplSlot::NotPaired,
        DefKind::Value { .. } => {
            let Some(binding) = ancestors(element).find(|n| n.kind() == SyntaxKind::BINDING) else {
                return ImplSlot::Unmodelled;
            };
            let Some(container) = binding.parent() else {
                return ImplSlot::Unmodelled;
            };
            match container.kind() {
                SyntaxKind::LET_OR_USE_EXPR | SyntaxKind::MEMBER_LET_BINDINGS => {
                    ImplSlot::NotPaired
                }
                SyntaxKind::LET_DECL if !in_head_pattern(&binding, def.range) => {
                    ImplSlot::NotPaired
                }
                SyntaxKind::LET_DECL if is_module_level(&container) => {
                    keyed(&container, Table::Vals, Slot::Value)
                }
                _ => ImplSlot::Unmodelled,
            }
        }
        DefKind::Type => {
            let Some(defn) = ancestors(element).find(|n| n.kind() == SyntaxKind::TYPE_DEFN) else {
                return ImplSlot::Unmodelled;
            };
            match type_key(&defn) {
                Some(key) if !is_augmentation(&defn) && key.name == name => {
                    keyed(&defn, Table::Types, Slot::Type(key.arity))
                }
                _ => ImplSlot::Unmodelled,
            }
        }
        DefKind::ExceptionCase => {
            match ancestors(element).find(|n| n.kind() == SyntaxKind::EXCEPTION_DEFN) {
                Some(defn) => keyed(&defn, Table::Types, Slot::Exception),
                None => ImplSlot::Unmodelled,
            }
        }
        DefKind::UnionCase | DefKind::EnumCase => {
            let Some(defn) = ancestors(element).find(|n| n.kind() == SyntaxKind::TYPE_DEFN) else {
                return ImplSlot::Unmodelled;
            };
            let cases: Option<Vec<SyntaxToken>> = cases_of(&defn).iter().map(first_ident).collect();
            match (container_of(&defn), type_key(&defn), cases) {
                (Some(container), Some(ty), Some(cases)) => ImplSlot::Case {
                    container,
                    ty,
                    cases,
                },
                _ => ImplSlot::Unmodelled,
            }
        }
        DefKind::Member => {
            let Some(member) = ancestors(element).find(|n| is_member_node(n.kind())) else {
                return ImplSlot::Unmodelled;
            };
            // A member of the type itself: not an interface implementation's
            // nor an object expression's.
            let Some(defn) = member.parent().and_then(|p| match p.kind() {
                SyntaxKind::TYPE_DEFN => Some(p),
                SyntaxKind::OBJECT_MODEL_REPR => {
                    p.parent().filter(|d| d.kind() == SyntaxKind::TYPE_DEFN)
                }
                _ => None,
            }) else {
                return ImplSlot::Unmodelled;
            };
            match type_key(&defn) {
                Some(key)
                    if !is_dispatch(&member)
                        && member_name_token(&member)
                            .is_some_and(|t| unticked(t.text()) == name) =>
                {
                    keyed(&defn, Table::Vals, Slot::Member(key))
                }
                _ => ImplSlot::Unmodelled,
            }
        }
    }
}

/// The modules and namespaces around `node`, outermost first; `None` inside
/// an anonymous (header-less) module, whose name comes from the file's path.
fn container_of(node: &SyntaxNode) -> Option<Vec<String>> {
    let mut levels = Vec::new();
    for a in node.ancestors().skip(1) {
        match a.kind() {
            SyntaxKind::NESTED_MODULE_DECL => levels.push(segments(&a)?),
            SyntaxKind::MODULE_OR_NAMESPACE => {
                let headed = a.children_with_tokens().any(|e| {
                    matches!(e.kind(), SyntaxKind::MODULE_TOK | SyntaxKind::NAMESPACE_TOK)
                });
                if !headed {
                    return None;
                }
                levels.push(segments(&a).unwrap_or_default());
            }
            _ => {}
        }
    }
    levels.reverse();
    Some(levels.concat())
}

/// The names of a module or namespace header's `LONG_IDENT`.
fn segments(decl: &SyntaxNode) -> Option<Vec<String>> {
    let name = decl
        .children()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT)?;
    Some(
        name.children_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .filter(|t| t.kind() == SyntaxKind::IDENT_TOK)
            .map(|t| unticked(t.text()))
            .collect(),
    )
}

fn is_module_level(node: &SyntaxNode) -> bool {
    node.parent().is_some_and(|p| {
        matches!(
            p.kind(),
            SyntaxKind::NESTED_MODULE_DECL | SyntaxKind::MODULE_OR_NAMESPACE
        )
    })
}

fn is_member_node(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::MEMBER_DEFN
            | SyntaxKind::AUTO_PROPERTY
            | SyntaxKind::GET_SET_MEMBER
            | SyntaxKind::MEMBER_SIG
            | SyntaxKind::ABSTRACT_SLOT
    )
}

fn is_dispatch(member: &SyntaxNode) -> bool {
    member.kind() == SyntaxKind::ABSTRACT_SLOT
        || member.children_with_tokens().any(|e| {
            matches!(
                e.kind(),
                SyntaxKind::ABSTRACT_TOK | SyntaxKind::OVERRIDE_TOK | SyntaxKind::DEFAULT_TOK
            )
        })
        || member
            .ancestors()
            .any(|a| a.kind() == SyntaxKind::INTERFACE_IMPL)
}

/// A type augmentation (`type T with …`): members of an existing type, no
/// type of its own.
fn is_augmentation(defn: &SyntaxNode) -> bool {
    !defn
        .children_with_tokens()
        .any(|e| e.kind() == SyntaxKind::EQUALS_TOK)
        && defn
            .descendants_with_tokens()
            .any(|e| e.kind() == SyntaxKind::WITH_TOK)
}

fn type_key(defn: &SyntaxNode) -> Option<TypeKey> {
    let name = defn
        .children()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT)?;
    Some(TypeKey {
        name: ident_text(&name),
        arity: type_arity(defn),
    })
}

fn last_name_token(defn: &SyntaxNode) -> Option<SyntaxToken> {
    defn.children()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT)?
        .children_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .filter(|t| t.kind() == SyntaxKind::IDENT_TOK)
        .last()
}

/// The union or enum cases of a type definition's representation.
fn cases_of(defn: &SyntaxNode) -> Vec<SyntaxNode> {
    defn.children()
        .filter(|r| matches!(r.kind(), SyntaxKind::UNION_REPR | SyntaxKind::ENUM_REPR))
        .flat_map(|r| r.children())
        .filter(|c| matches!(c.kind(), SyntaxKind::UNION_CASE | SyntaxKind::ENUM_CASE))
        .collect()
}

fn first_ident(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
}

/// A member's name token: a signature's (or abstract slot's) `VAL_SIG` name,
/// an auto-property's own identifier, or the last identifier of a
/// definition's head.
fn member_name_token(member: &SyntaxNode) -> Option<SyntaxToken> {
    match member.kind() {
        SyntaxKind::MEMBER_SIG | SyntaxKind::ABSTRACT_SLOT => member
            .children()
            .find(|n| n.kind() == SyntaxKind::VAL_SIG)
            .and_then(|s| first_ident(&s)),
        SyntaxKind::AUTO_PROPERTY => first_ident(member),
        _ => super::source::member_name_token(member),
    }
}

fn def_at(token: &SyntaxToken, kind: DefKind) -> Def {
    Def {
        name: token.text().to_string(),
        range: token.text_range(),
        kind,
        provisional: false,
    }
}
