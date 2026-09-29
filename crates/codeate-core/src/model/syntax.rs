use crate::model::id::EntityId;

/// The closed set of syntax-level entity kinds the canonical model
/// recognizes. Every language adapter maps its AST onto exactly these
/// kinds — there is no catch-all "other" variant, so an unmapped
/// construct is a model gap to fix here, not something to smuggle through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyntaxKind {
    Module,
    Package,
    Class,
    Interface,
    Struct,
    Enum,
    Function,
    Method,
    Variable,
    Parameter,
    Statement,
    Expression,
}

impl SyntaxKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SyntaxKind::Module => "Module",
            SyntaxKind::Package => "Package",
            SyntaxKind::Class => "Class",
            SyntaxKind::Interface => "Interface",
            SyntaxKind::Struct => "Struct",
            SyntaxKind::Enum => "Enum",
            SyntaxKind::Function => "Function",
            SyntaxKind::Method => "Method",
            SyntaxKind::Variable => "Variable",
            SyntaxKind::Parameter => "Parameter",
            SyntaxKind::Statement => "Statement",
            SyntaxKind::Expression => "Expression",
        }
    }
}

/// A language-agnostic syntax node: a stable identity, its kind, and its
/// declared name. Language adapters (Phase 4+) produce these; no adapter
/// targets this model yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxEntity {
    pub id: EntityId,
    pub kind: SyntaxKind,
    pub name: String,
}

impl SyntaxEntity {
    pub fn new(kind: SyntaxKind, location: &str, name: impl Into<String>) -> Self {
        let name = name.into();
        // Namespaced by "syntax::" so a SyntaxEntity and a SymbolEntity of
        // the same-named kind (e.g. both "Function") never collide on ID.
        let kind_tag = format!("syntax::{}", kind.as_str());
        let id = EntityId::new(&kind_tag, location, &name, None);
        Self { id, kind, name }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KINDS: [SyntaxKind; 12] = [
        SyntaxKind::Module,
        SyntaxKind::Package,
        SyntaxKind::Class,
        SyntaxKind::Interface,
        SyntaxKind::Struct,
        SyntaxKind::Enum,
        SyntaxKind::Function,
        SyntaxKind::Method,
        SyntaxKind::Variable,
        SyntaxKind::Parameter,
        SyntaxKind::Statement,
        SyntaxKind::Expression,
    ];

    // Exhaustive match with no wildcard arm: this only compiles if every
    // SyntaxKind variant is handled, so adding a variant without updating
    // this test is a compile error -- the enum stays a genuinely closed set.
    fn exhaustive_name(kind: SyntaxKind) -> &'static str {
        match kind {
            SyntaxKind::Module => "Module",
            SyntaxKind::Package => "Package",
            SyntaxKind::Class => "Class",
            SyntaxKind::Interface => "Interface",
            SyntaxKind::Struct => "Struct",
            SyntaxKind::Enum => "Enum",
            SyntaxKind::Function => "Function",
            SyntaxKind::Method => "Method",
            SyntaxKind::Variable => "Variable",
            SyntaxKind::Parameter => "Parameter",
            SyntaxKind::Statement => "Statement",
            SyntaxKind::Expression => "Expression",
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
        let a = SyntaxEntity::new(SyntaxKind::Function, "src/main.rs", "main");
        let b = SyntaxEntity::new(SyntaxKind::Function, "src/main.rs", "main");
        assert_eq!(a, b);
    }

    #[test]
    fn entities_of_different_kinds_are_not_equal() {
        let a = SyntaxEntity::new(SyntaxKind::Struct, "src/main.rs", "Point");
        let b = SyntaxEntity::new(SyntaxKind::Class, "src/main.rs", "Point");
        assert_ne!(a, b);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn entity_carries_its_declared_name() {
        let entity = SyntaxEntity::new(SyntaxKind::Variable, "src/main.rs", "count");
        assert_eq!(entity.name, "count");
        assert_eq!(entity.kind, SyntaxKind::Variable);
    }
}
