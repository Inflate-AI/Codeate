use crate::model::{SymbolEntity, SymbolKind};

/// A resolved type expression: language-agnostic, but produced by a
/// language adapter's type extraction (Phase 6 task 2 for Rust).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    /// A built-in primitive, e.g. `i32`, `bool`.
    Primitive(String),
    /// A user-defined type, possibly generic (`Vec<i32>`, `Point`).
    Named {
        name: String,
        type_args: Vec<TypeRef>,
    },
    /// A reference to another type (`&T`, `&mut T`).
    Reference { mutable: bool, inner: Box<TypeRef> },
    /// A generic type parameter in scope (`T`).
    Generic(String),
    /// Anything that couldn't be classified. Keeps the raw source text
    /// rather than dropping the type silently.
    Unresolved(String),
}

impl TypeRef {
    /// A canonical, structure-sensitive key: two `TypeRef`s that are
    /// structurally equal always produce the same key, and structurally
    /// different ones (including across variants, even with the same
    /// display name) always differ.
    fn key(&self) -> String {
        match self {
            TypeRef::Primitive(name) => format!("primitive:{name}"),
            TypeRef::Named { name, type_args } => {
                let args = type_args
                    .iter()
                    .map(TypeRef::key)
                    .collect::<Vec<_>>()
                    .join(",");
                format!("named:{name}<{args}>")
            }
            TypeRef::Reference { mutable, inner } => {
                format!("ref{}:{}", if *mutable { "_mut" } else { "" }, inner.key())
            }
            TypeRef::Generic(name) => format!("generic:{name}"),
            TypeRef::Unresolved(raw) => format!("unresolved:{raw}"),
        }
    }

    /// A human-readable rendering, e.g. `Vec<i32>`, `&mut T`.
    pub fn display_name(&self) -> String {
        match self {
            TypeRef::Primitive(name) | TypeRef::Generic(name) => name.clone(),
            TypeRef::Named { name, type_args } if type_args.is_empty() => name.clone(),
            TypeRef::Named { name, type_args } => {
                let args = type_args
                    .iter()
                    .map(TypeRef::display_name)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{name}<{args}>")
            }
            TypeRef::Reference { mutable, inner } => {
                format!(
                    "&{}{}",
                    if *mutable { "mut " } else { "" },
                    inner.display_name()
                )
            }
            TypeRef::Unresolved(raw) => raw.clone(),
        }
    }
}

/// The canonical `SymbolKind::Type` symbol for a `TypeRef`. Every `TypeRef`
/// -- including primitives and unresolved ones -- gets a stable,
/// deterministic symbol, so there's always something concrete to bind
/// `ACCEPTS`/`RETURNS`/`DEPENDS_ON` edges to.
pub fn type_symbol_for(type_ref: &TypeRef) -> SymbolEntity {
    SymbolEntity::new(SymbolKind::Type, &type_ref.key(), type_ref.display_name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_type_ref_produces_the_same_symbol_id() {
        let a = TypeRef::Primitive("i32".to_string());
        let b = TypeRef::Primitive("i32".to_string());
        assert_eq!(type_symbol_for(&a).id, type_symbol_for(&b).id);
    }

    #[test]
    fn different_type_refs_produce_different_symbol_ids() {
        let a = TypeRef::Primitive("i32".to_string());
        let b = TypeRef::Primitive("i64".to_string());
        assert_ne!(type_symbol_for(&a).id, type_symbol_for(&b).id);
    }

    #[test]
    fn primitive_and_named_of_the_same_display_name_get_distinct_ids() {
        let primitive = TypeRef::Primitive("Meters".to_string());
        let named = TypeRef::Named {
            name: "Meters".to_string(),
            type_args: vec![],
        };
        assert_ne!(type_symbol_for(&primitive).id, type_symbol_for(&named).id);
    }

    #[test]
    fn nested_generic_args_affect_the_id() {
        let vec_of_i32 = TypeRef::Named {
            name: "Vec".to_string(),
            type_args: vec![TypeRef::Primitive("i32".to_string())],
        };
        let vec_of_i64 = TypeRef::Named {
            name: "Vec".to_string(),
            type_args: vec![TypeRef::Primitive("i64".to_string())],
        };
        assert_ne!(
            type_symbol_for(&vec_of_i32).id,
            type_symbol_for(&vec_of_i64).id
        );
        assert_eq!(type_symbol_for(&vec_of_i32).name, "Vec<i32>");
    }

    #[test]
    fn reference_mutability_affects_the_id_and_display_name() {
        let shared = TypeRef::Reference {
            mutable: false,
            inner: Box::new(TypeRef::Generic("T".to_string())),
        };
        let exclusive = TypeRef::Reference {
            mutable: true,
            inner: Box::new(TypeRef::Generic("T".to_string())),
        };
        assert_ne!(type_symbol_for(&shared).id, type_symbol_for(&exclusive).id);
        assert_eq!(type_symbol_for(&shared).name, "&T");
        assert_eq!(type_symbol_for(&exclusive).name, "&mut T");
    }

    #[test]
    fn unresolved_types_keep_their_raw_text_and_get_a_symbol() {
        let unresolved = TypeRef::Unresolved("(i32, String)".to_string());
        let symbol = type_symbol_for(&unresolved);
        assert_eq!(symbol.name, "(i32, String)");
        assert_eq!(symbol.kind, SymbolKind::Type);
    }
}
