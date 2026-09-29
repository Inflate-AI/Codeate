use crate::model::id::EntityId;

/// The closed set of symbol-level entity kinds the canonical model
/// recognizes. Symbols are resolved names with identity and scope (Phase 5+)
/// — distinct from `SyntaxKind`, which describes AST shape. There is no
/// catch-all "other" variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    Symbol,
    Type,
    Field,
    Method,
    Function,
    Constant,
    Namespace,
    Generic,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Symbol => "Symbol",
            SymbolKind::Type => "Type",
            SymbolKind::Field => "Field",
            SymbolKind::Method => "Method",
            SymbolKind::Function => "Function",
            SymbolKind::Constant => "Constant",
            SymbolKind::Namespace => "Namespace",
            SymbolKind::Generic => "Generic",
        }
    }
}

/// A language-agnostic resolved symbol: a stable identity, its kind, and its
/// name. Symbol resolution (Phase 5+) produces these; no resolver targets
/// this model yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolEntity {
    pub id: EntityId,
    pub kind: SymbolKind,
    pub name: String,
}

impl SymbolEntity {
    pub fn new(kind: SymbolKind, location: &str, name: impl Into<String>) -> Self {
        let name = name.into();
        // Namespaced by "symbol::" so a SymbolEntity and a SyntaxEntity of
        // the same-named kind (e.g. both "Function") never collide on ID.
        let kind_tag = format!("symbol::{}", kind.as_str());
        let id = EntityId::new(&kind_tag, location, &name, None);
        Self { id, kind, name }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KINDS: [SymbolKind; 8] = [
        SymbolKind::Symbol,
        SymbolKind::Type,
        SymbolKind::Field,
        SymbolKind::Method,
        SymbolKind::Function,
        SymbolKind::Constant,
        SymbolKind::Namespace,
        SymbolKind::Generic,
    ];

    // Exhaustive match with no wildcard arm: only compiles if every
    // SymbolKind variant is handled, keeping the enum a genuinely closed set.
    fn exhaustive_name(kind: SymbolKind) -> &'static str {
        match kind {
            SymbolKind::Symbol => "Symbol",
            SymbolKind::Type => "Type",
            SymbolKind::Field => "Field",
            SymbolKind::Method => "Method",
            SymbolKind::Function => "Function",
            SymbolKind::Constant => "Constant",
            SymbolKind::Namespace => "Namespace",
            SymbolKind::Generic => "Generic",
        }
    }

    #[test]
    fn every_kind_has_a_stable_name_and_matches_as_str() {
        for kind in ALL_KINDS {
            assert_eq!(exhaustive_name(kind), kind.as_str());
        }
    }

    #[test]
    fn entities_of_the_same_kind_location_and_name_are_equal() {
        let a = SymbolEntity::new(SymbolKind::Function, "src/main.rs", "main");
        let b = SymbolEntity::new(SymbolKind::Function, "src/main.rs", "main");
        assert_eq!(a, b);
    }

    #[test]
    fn entities_of_different_kinds_are_not_equal() {
        let a = SymbolEntity::new(SymbolKind::Type, "src/main.rs", "Point");
        let b = SymbolEntity::new(SymbolKind::Symbol, "src/main.rs", "Point");
        assert_ne!(a, b);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn entity_carries_its_declared_name() {
        let entity = SymbolEntity::new(SymbolKind::Constant, "src/main.rs", "MAX_SIZE");
        assert_eq!(entity.name, "MAX_SIZE");
        assert_eq!(entity.kind, SymbolKind::Constant);
    }
}
