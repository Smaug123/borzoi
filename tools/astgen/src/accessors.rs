//! Reading the union's accessors out of `crates/cst/src/syntax/union.rs` with
//! `syn` (`docs/ast-versioning-plan.md` D4).
//!
//! An accessor is a `pub fn (&self) -> T` in an inherent `impl` of a node
//! newtype. Its body stays hand-written; astgen reads only the signature and
//! classifies `T` into the [`Shape`] set. The reading is total: every item in
//! the file is either understood or refused, so a public accessor can be
//! missed only by refusing the file. In particular the file declares no public
//! types (astgen's tables own them), no modules and no macros, either of which
//! could hide one.

use crate::shape::{Names, Shape, classify};

/// One public accessor: `owner::name(&self) -> shape`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Accessor {
    /// The node newtype whose inherent `impl` declares it.
    pub owner: String,
    /// The method name.
    pub name: String,
    /// What it returns.
    pub shape: Shape,
}

/// The repo-relative path [`read_accessors`] is given the text of.
pub const UNION_PATH: &str = "crates/cst/src/syntax/union.rs";

/// The text of [`UNION_PATH`], read at astgen's build, so a regeneration after
/// an accessor change sees the change.
pub const UNION_SOURCE: &str = include_str!("../../../crates/cst/src/syntax/union.rs");

fn is_pub(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Public(_))
}

/// Why the signature of `owner::name` is not an accessor's, if it is not.
fn check_signature(sig: &syn::Signature) -> Result<&syn::Type, String> {
    if sig.constness.is_some()
        || sig.asyncness.is_some()
        || sig.unsafety.is_some()
        || sig.abi.is_some()
        || sig.variadic.is_some()
    {
        return Err("a qualified fn (`const`, `async`, `unsafe`, `extern`)".to_string());
    }
    if !sig.generics.params.is_empty() || sig.generics.where_clause.is_some() {
        return Err("generic".to_string());
    }
    let receiver_only = sig.inputs.len() == 1
        && sig.receiver().is_some_and(|r| {
            r.reference.as_ref().is_some_and(|(_, lt)| lt.is_none())
                && r.mutability.is_none()
                && r.colon_token.is_none()
        });
    if !receiver_only {
        return Err("its parameters are not exactly `&self`".to_string());
    }
    match &sig.output {
        syn::ReturnType::Type(_, ty) => Ok(ty),
        syn::ReturnType::Default => Err("it returns `()`".to_string()),
    }
}

/// The owner of an inherent `impl`, when it is a plain type name.
fn impl_owner(imp: &syn::ItemImpl) -> Option<String> {
    let syn::Type::Path(p) = &*imp.self_ty else {
        return None;
    };
    if p.qself.is_some() || p.path.segments.len() != 1 || !imp.generics.params.is_empty() {
        return None;
    }
    let segment = &p.path.segments[0];
    matches!(segment.arguments, syn::PathArguments::None).then(|| segment.ident.to_string())
}

/// Every public accessor in `source` (the text of [`UNION_PATH`]), in source
/// order, or every reason the file cannot be read that way, each naming the
/// item or accessor at fault.
pub fn read_accessors(source: &str, names: &Names) -> Result<Vec<Accessor>, Vec<String>> {
    let file = syn::parse_file(source).map_err(|e| vec![format!("{UNION_PATH}: {e}")])?;
    let mut accessors = Vec::new();
    let mut errors = Vec::new();
    for item in &file.items {
        match item {
            syn::Item::Use(_) => {}
            syn::Item::Fn(f) if !is_pub(&f.vis) => {}
            syn::Item::Fn(f) => errors.push(format!(
                "`{}` is a public free function; an accessor is a method of a node",
                f.sig.ident
            )),
            syn::Item::Const(c) if !is_pub(&c.vis) => {}
            syn::Item::Static(s) if !is_pub(&s.vis) => {}
            syn::Item::Struct(s) if !is_pub(&s.vis) => {}
            syn::Item::Enum(e) if !is_pub(&e.vis) => {}
            syn::Item::Type(t) if !is_pub(&t.vis) => {}
            syn::Item::Struct(syn::ItemStruct { ident, .. })
            | syn::Item::Enum(syn::ItemEnum { ident, .. })
            | syn::Item::Type(syn::ItemType { ident, .. })
            | syn::Item::Const(syn::ItemConst { ident, .. })
            | syn::Item::Static(syn::ItemStatic { ident, .. }) => errors.push(format!(
                "`{ident}` is a public item declared by hand; the facade's types are \
                 declared in astgen's tables (`tools/astgen/src/lib.rs`)"
            )),
            syn::Item::Impl(imp) => read_impl(imp, names, &mut accessors, &mut errors),
            syn::Item::Macro(m) => errors.push(format!(
                "a macro at item level (`{}!`) could declare anything; astgen cannot \
                 read through it",
                m.mac
                    .path
                    .segments
                    .last()
                    .map_or_else(String::new, |s| s.ident.to_string())
            )),
            syn::Item::Mod(m) => errors.push(format!(
                "module `{}`: astgen reads the file's top level only",
                m.ident
            )),
            _ => errors.push(
                "an item astgen does not read (a trait, union, `extern` block, …)".to_string(),
            ),
        }
    }
    if errors.is_empty() {
        Ok(accessors)
    } else {
        Err(errors)
    }
}

fn read_impl(
    imp: &syn::ItemImpl,
    names: &Names,
    accessors: &mut Vec<Accessor>,
    errors: &mut Vec<String>,
) {
    let Some(owner) = impl_owner(imp) else {
        errors.push("an `impl` whose type is not a plain type name".to_string());
        return;
    };
    if imp.trait_.is_some() {
        errors.push(format!(
            "a trait `impl` for `{owner}`: astgen reads inherent accessors only"
        ));
        return;
    }
    if !names.nodes.contains(&owner) {
        errors.push(format!(
            "`impl {owner}`: accessors belong to node newtypes, and `{owner}` is not one \
             in astgen's tables"
        ));
        return;
    }
    for item in &imp.items {
        match item {
            syn::ImplItem::Fn(f) if !is_pub(&f.vis) => {}
            syn::ImplItem::Fn(f) => {
                let name = f.sig.ident.to_string();
                let shape = check_signature(&f.sig)
                    .and_then(|ty| classify(ty, names).map_err(|why| format!("it returns {why}")));
                match shape {
                    Ok(shape) => accessors.push(Accessor {
                        owner: owner.clone(),
                        name,
                        shape,
                    }),
                    Err(why) => errors.push(format!(
                        "accessor `{owner}::{name}` has a signature outside astgen's shape \
                         set (`tools/astgen/src/shape.rs`): {why}"
                    )),
                }
            }
            syn::ImplItem::Const(c) if !is_pub(&c.vis) => {}
            _ => errors.push(format!(
                "`impl {owner}` holds an item other than a fn or a private const"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Names {
        Names {
            nodes: ["ParenExpr", "LongIdent"].map(String::from).into(),
            enums: ["Expr"].map(String::from).into(),
            values: Default::default(),
        }
    }

    #[test]
    fn accessors_are_read_in_source_order() {
        let src = r#"
            use super::SyntaxNode;
            fn helper() {}
            impl ParenExpr {
                pub fn inner(&self) -> Option<Expr> { todo!() }
                fn private(&self) -> String { todo!() }
                pub(crate) fn crate_only(&self, x: u8) {}
                pub fn is_struct(&self) -> bool { todo!() }
            }
        "#;
        let read = read_accessors(src, &names()).unwrap();
        let got: Vec<String> = read
            .iter()
            .map(|a| format!("{}::{} {}", a.owner, a.name, a.shape.render()))
            .collect();
        assert_eq!(
            got,
            [
                "ParenExpr::inner option enum Expr",
                "ParenExpr::is_struct bool"
            ]
        );
    }

    #[track_caller]
    fn refused(src: &str, needle: &str) {
        let errors = read_accessors(src, &names()).expect_err("the source was accepted");
        assert!(
            errors.iter().any(|e| e.contains(needle)),
            "no error mentions {needle:?}: {errors:#?}"
        );
    }

    #[test]
    fn whatever_could_hide_an_accessor_is_refused() {
        refused(
            "impl ParenExpr { pub fn text(&self) -> String { todo!() } }",
            "ParenExpr::text",
        );
        refused(
            "impl ParenExpr { pub fn nth(&self, n: usize) -> Option<Expr> { todo!() } }",
            "ParenExpr::nth",
        );
        refused(
            "impl ParenExpr { pub fn make() -> Option<Expr> { todo!() } }",
            "ParenExpr::make",
        );
        refused(
            "impl ParenExpr { pub fn go(&mut self) -> bool { todo!() } }",
            "ParenExpr::go",
        );
        refused(
            "impl ParenExpr { pub fn go<T>(&self) -> bool { todo!() } }",
            "ParenExpr::go",
        );
        refused("impl ParenExpr { pub fn go(&self) {} }", "ParenExpr::go");
        refused(
            "impl Unknown { pub fn go(&self) -> bool { todo!() } }",
            "Unknown",
        );
        refused("impl Expr { pub fn go(&self) -> bool { todo!() } }", "Expr");
        refused(
            "impl<T> Wrapper<T> { pub fn go(&self) -> bool { todo!() } }",
            "not a plain type name",
        );
        refused(
            "impl Clone for ParenExpr { fn clone(&self) -> Self { todo!() } }",
            "trait",
        );
        refused("pub struct Extra(SyntaxNode);", "Extra");
        refused("pub enum Extra { A }", "Extra");
        refused("ast_node!(Extra, EXTRA);", "ast_node");
        refused("mod inner { }", "inner");
        refused("pub fn free() -> bool { true }", "free");
        refused("impl ParenExpr { pub const X: u8 = 0; }", "other than a fn");
    }
}
