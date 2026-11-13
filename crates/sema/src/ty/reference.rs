//! The string form of a [`Ty`]: every named type's path spelled as its
//! segments, and the two renderers over that form
//! (`docs/sema-interning-plan.md`, Stage 7).
//!
//! It is the representation [`Ty`]'s paths were before they were ids, kept as
//! the reference the interned form is held to: [`SpelledTy::of`] and
//! [`SpelledTy::to_ty`] convert between the two through a table, and
//! [`render`] and [`render_fsharp`] are the renderers as they read the
//! segments, so a test can ask that rendering a [`Ty`] through its ids gives
//! the same text.

use super::{FSHARP_CORE_RENAMED, Ty, TyVid, typar_name};
use crate::names::NameTable;

/// A [`Ty`] whose named types carry their paths as segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpelledTy {
    Named {
        path: Vec<String>,
        args: Vec<SpelledTy>,
    },
    Project {
        path: Vec<String>,
        args: Vec<SpelledTy>,
    },
    Array {
        elem: Box<SpelledTy>,
        rank: u32,
    },
    Tuple(Vec<SpelledTy>),
    Fun {
        arg: Box<SpelledTy>,
        ret: Box<SpelledTy>,
    },
    Param(u32),
    Var(TyVid),
}

impl SpelledTy {
    /// `ty` with each path spelled by `names`.
    pub(crate) fn of(names: &NameTable, ty: &Ty) -> SpelledTy {
        let _ = names;
        let all = |args: &[Ty]| args.iter().map(|a| SpelledTy::of(names, a)).collect();
        match ty {
            Ty::Named { path, args } => SpelledTy::Named {
                path: path.clone(),
                args: all(args),
            },
            Ty::Project { path, args } => SpelledTy::Project {
                path: path.clone(),
                args: all(args),
            },
            Ty::Array { elem, rank } => SpelledTy::Array {
                elem: Box::new(SpelledTy::of(names, elem)),
                rank: *rank,
            },
            Ty::Tuple(elems) => SpelledTy::Tuple(all(elems)),
            Ty::Fun { arg, ret } => SpelledTy::Fun {
                arg: Box::new(SpelledTy::of(names, arg)),
                ret: Box::new(SpelledTy::of(names, ret)),
            },
            Ty::Param(i) => SpelledTy::Param(*i),
            Ty::Var(v) => SpelledTy::Var(*v),
        }
    }

    /// This type with each path interned in `names`.
    pub(crate) fn to_ty(&self, names: &NameTable) -> Ty {
        let _ = names;
        let all = |args: &[SpelledTy]| args.iter().map(|a| a.to_ty(names)).collect();
        match self {
            SpelledTy::Named { path, args } => Ty::Named {
                path: path.clone(),
                args: all(args),
            },
            SpelledTy::Project { path, args } => Ty::Project {
                path: path.clone(),
                args: all(args),
            },
            SpelledTy::Array { elem, rank } => Ty::Array {
                elem: Box::new(elem.to_ty(names)),
                rank: *rank,
            },
            SpelledTy::Tuple(elems) => Ty::Tuple(all(elems)),
            SpelledTy::Fun { arg, ret } => Ty::Fun {
                arg: Box::new(arg.to_ty(names)),
                ret: Box::new(ret.to_ty(names)),
            },
            SpelledTy::Param(i) => Ty::Param(*i),
            SpelledTy::Var(v) => Ty::Var(*v),
        }
    }
}

/// [`Ty::render`] over the string form.
pub(crate) fn render(ty: &SpelledTy) -> String {
    match ty {
        SpelledTy::Named { path, args } | SpelledTy::Project { path, args } => {
            render_named(&path.join("."), args, render)
        }
        SpelledTy::Array { elem, rank } => render_array(elem, *rank, render),
        SpelledTy::Tuple(elems) => render_tuple(elems, render),
        SpelledTy::Fun { arg, ret } => render_fun(arg, ret, render),
        SpelledTy::Param(i) => typar_name(*i),
        SpelledTy::Var(_) => "'_".to_owned(),
    }
}

/// [`Ty::render_fsharp`] over the string form.
pub(crate) fn render_fsharp(ty: &SpelledTy) -> String {
    match ty {
        SpelledTy::Named { path, args } => {
            let (namespace, name) = match path.split_last() {
                Some((name, ns)) => (ns.join("."), name.as_str()),
                None => return String::new(),
            };
            let unit = (namespace == "Microsoft.FSharp.Core" && name == "Unit").then_some("unit");
            let renamed = (!args.is_empty())
                .then(|| {
                    FSHARP_CORE_RENAMED
                        .iter()
                        .find(|(ns, compiled, _, _)| *ns == namespace && *compiled == name)
                        .map(|&(_, _, _, display)| display)
                })
                .flatten();
            let head = unit
                .or(renamed)
                .or_else(|| borzoi_assembly::fsharp_alias(&namespace, name))
                .map(str::to_owned)
                .unwrap_or_else(|| borzoi_assembly::join_quoted(path.iter().map(String::as_str)));
            render_named(&head, args, render_fsharp)
        }
        SpelledTy::Project { path, args } => render_named(
            &borzoi_assembly::join_quoted(path.iter().map(String::as_str)),
            args,
            render_fsharp,
        ),
        SpelledTy::Array { elem, rank } => render_array(elem, *rank, render_fsharp),
        SpelledTy::Tuple(elems) => render_tuple(elems, render_fsharp),
        SpelledTy::Fun { arg, ret } => render_fun(arg, ret, render_fsharp),
        SpelledTy::Param(i) => typar_name(*i),
        SpelledTy::Var(_) => "'_".to_owned(),
    }
}

fn render_named(head: &str, args: &[SpelledTy], render_arg: fn(&SpelledTy) -> String) -> String {
    if args.is_empty() {
        return head.to_owned();
    }
    let args: Vec<String> = args.iter().map(render_arg).collect();
    format!("{head}<{}>", args.join(", "))
}

fn bracketed(ty: &SpelledTy, render_elem: fn(&SpelledTy) -> String) -> String {
    match ty {
        SpelledTy::Tuple(_) | SpelledTy::Fun { .. } => format!("({})", render_elem(ty)),
        _ => render_elem(ty),
    }
}

fn render_array(elem: &SpelledTy, rank: u32, render_elem: fn(&SpelledTy) -> String) -> String {
    let commas = ",".repeat((rank as usize).saturating_sub(1));
    format!("{}[{commas}]", bracketed(elem, render_elem))
}

fn render_tuple(elems: &[SpelledTy], render_elem: fn(&SpelledTy) -> String) -> String {
    elems
        .iter()
        .map(|e| bracketed(e, render_elem))
        .collect::<Vec<_>>()
        .join(" * ")
}

fn render_fun(arg: &SpelledTy, ret: &SpelledTy, render_elem: fn(&SpelledTy) -> String) -> String {
    let arg = match arg {
        SpelledTy::Fun { .. } => format!("({})", render_elem(arg)),
        _ => render_elem(arg),
    };
    format!("{arg} -> {}", render_elem(ret))
}
