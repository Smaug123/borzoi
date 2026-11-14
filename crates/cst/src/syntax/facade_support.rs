//! What the generated per-version facades (`generated::facades`) share: the
//! node newtype, written once as a macro so each facade's code is one line per
//! node.

/// A facade's node newtype over the union's node of the same name, casting
/// from exactly the listed kinds (its lock's `node` lines). Its `Debug` is the
/// union node's, so a facade value and the union value it re-casts print the
/// same. `to_union` is the escape back to the union, under `unstable-union`.
macro_rules! facade_node {
    ($name:ident: $($kind:ident)|+) => {
        #[doc = concat!(
            "A `", $(stringify!($kind), " ",)+
            "node (`docs/ast-versioning-plan.md` D4)."
        )]
        #[derive(Clone, PartialEq, Eq, Hash)]
        pub struct $name(crate::syntax::union::$name);

        impl ::std::fmt::Debug for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                ::std::fmt::Debug::fmt(&self.0, f)
            }
        }

        impl crate::syntax::AstNode for $name {
            fn can_cast(kind: crate::syntax::SyntaxKind) -> bool {
                matches!(kind, $(crate::syntax::SyntaxKind::$kind)|+)
            }

            fn cast(node: crate::syntax::SyntaxNode) -> Option<Self> {
                if Self::can_cast(node.kind()) {
                    crate::syntax::union::$name::cast(node).map(Self)
                } else {
                    None
                }
            }

            fn syntax(&self) -> &crate::syntax::SyntaxNode {
                self.0.syntax()
            }
        }

        impl $name {
            #[doc = concat!(
                "This node as the union's [`", stringify!($name),
                "`](crate::syntax::", stringify!($name), ")."
            )]
            #[cfg(feature = "unstable-union")]
            pub fn to_union(&self) -> crate::syntax::union::$name {
                self.0.clone()
            }
        }
    };
}

pub(crate) use facade_node;
