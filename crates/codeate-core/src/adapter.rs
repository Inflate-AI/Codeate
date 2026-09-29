use crate::error::Result;
use crate::model::{Edge, SymbolEntity, SyntaxEntity};

/// The interface every language adapter implements: parse source text into a
/// language-specific AST, then extract that AST into the canonical model.
///
/// Later phases add resolver stages (symbol/type/reference resolution) on
/// top of the same parsed AST; their methods default to returning empty so
/// this trait's shape doesn't need to change again as those phases land —
/// an adapter overrides only the stages it currently supports.
pub trait LanguageAdapter {
    /// The language-specific parsed AST this adapter produces.
    type Ast;

    /// Parse `source` into this adapter's AST representation.
    fn parse(&self, source: &str) -> Result<Self::Ast>;

    /// Extract canonical syntax entities and their `CONTAINS`/`DECLARES`
    /// edges from a parsed AST. `location` identifies the source file the
    /// AST came from (used to derive stable `EntityId`s).
    fn extract(&self, ast: &Self::Ast, location: &str) -> (Vec<SyntaxEntity>, Vec<Edge>);

    /// Resolve declared names to symbol entities (Phase 5). Unimplemented by
    /// default.
    fn resolve_symbols(&self, _ast: &Self::Ast, _location: &str) -> Vec<SymbolEntity> {
        Vec::new()
    }

    /// Resolve identifier usages and call sites to `REFERENCES`/`CALLS`
    /// edges (Phase 7). Unimplemented by default.
    fn resolve_references(&self, _ast: &Self::Ast, _location: &str) -> Vec<Edge> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoOpAdapter;

    impl LanguageAdapter for NoOpAdapter {
        type Ast = ();

        fn parse(&self, _source: &str) -> Result<Self::Ast> {
            Ok(())
        }

        fn extract(&self, _ast: &Self::Ast, _location: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
            (Vec::new(), Vec::new())
        }
    }

    #[test]
    fn parse_and_extract_round_trip_on_a_minimal_adapter() {
        let adapter = NoOpAdapter;
        adapter.parse("irrelevant source").expect("parse succeeds");
        let (entities, edges) = adapter.extract(&(), "src/lib.rs");

        assert!(entities.is_empty());
        assert!(edges.is_empty());
    }

    #[test]
    fn resolver_stubs_default_to_empty_without_being_overridden() {
        let adapter = NoOpAdapter;
        adapter.parse("irrelevant source").expect("parse succeeds");

        assert!(adapter.resolve_symbols(&(), "src/lib.rs").is_empty());
        assert!(adapter.resolve_references(&(), "src/lib.rs").is_empty());
    }
}
