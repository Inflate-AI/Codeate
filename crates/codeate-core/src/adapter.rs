use std::any::Any;

use crate::error::Result;
use crate::model::{Edge, SymbolEntity, SyntaxEntity};
use crate::symbols::SymbolTable;

/// The interface every language adapter implements: parse source text into a
/// language-specific AST, extract that AST into the canonical model, then
/// resolve it against repository-wide context (the merged entity list and
/// symbol table) into symbols and edges. Everything past parsing/extraction
/// -- scope-based reference/call resolution, structural relationships
/// (`EXTENDS`/`IMPLEMENTS`/`OVERRIDES`/`IMPORTS`/`EXPORTS`), whatever else a
/// given language needs -- is this adapter's own business; the pipeline
/// (`pipeline::run_pipeline`) only ever calls `parse`, `extract`, and
/// `resolve_file`, never anything language-specific.
pub trait LanguageAdapter {
    /// The language-specific parsed AST this adapter produces.
    type Ast;

    /// Parse `source` into this adapter's AST representation.
    fn parse(&self, source: &str) -> Result<Self::Ast>;

    /// Extract canonical syntax entities and their `CONTAINS`/`DECLARES`
    /// edges from a parsed AST. `location` identifies the source file the
    /// AST came from (used to derive stable `EntityId`s).
    ///
    /// Contract: the first entity in the returned `Vec` must be this file's
    /// own root `Module` entity. `pipeline::run_pipeline` relies on this to
    /// find each file's module without assuming anything else about a given
    /// language's structure.
    fn extract(&self, ast: &Self::Ast, location: &str) -> (Vec<SyntaxEntity>, Vec<Edge>);

    /// Resolve `ast` (the same file `extract` was already called on) against
    /// repository-wide context into every symbol/edge Phases 5-8 produce for
    /// it: declared-name resolution beyond `extract`'s own entities,
    /// reference/call/instantiate resolution, and structural relationships.
    ///
    /// - `all_entities`/`symbol_table` are repository-wide (every file's
    ///   extraction merged), so resolution can target declarations in other
    ///   files.
    /// - `edges_so_far` are every `CONTAINS`/`DECLARES`/`DEFINED_IN` edge
    ///   produced so far, repository-wide (for scope-tree construction and
    ///   any other structural lookup a resolver needs).
    /// - `module_symbol`/`file_module` are this file's own module symbol and
    ///   syntax entity.
    ///
    /// Defaults to producing nothing, so an adapter that only implements
    /// parsing/extraction (Phase 4's shape) still satisfies the trait.
    #[allow(clippy::too_many_arguments)]
    fn resolve_file(
        &self,
        _ast: &Self::Ast,
        _location: &str,
        _all_entities: &[SyntaxEntity],
        _symbol_table: &SymbolTable,
        _edges_so_far: &[Edge],
        _module_symbol: &SymbolEntity,
        _file_module: &SyntaxEntity,
    ) -> (Vec<SymbolEntity>, Vec<Edge>) {
        (Vec::new(), Vec::new())
    }
}

/// An object-safe view of `LanguageAdapter`, for `pipeline::run_pipeline` to
/// dispatch across multiple concrete adapter types (Rust, Python, ...) by
/// `LanguageId` without needing to know each one's associated `Ast` type at
/// the call site. A blanket impl derives this for every `LanguageAdapter`
/// automatically -- adapter authors only ever implement `LanguageAdapter`
/// itself.
///
/// The parsed AST crosses this boundary as `Box<dyn Any>`/`&dyn Any`,
/// downcast back to `T::Ast` inside the blanket impl. The downcast can only
/// fail if `parse_dyn`'s result is fed to a different adapter's
/// `extract_dyn`/`resolve_file_dyn` than produced it, which `run_pipeline`
/// never does (it always re-looks-up the same adapter by the file's
/// `LanguageId`).
pub trait DynLanguageAdapter {
    fn parse_dyn(&self, source: &str) -> Result<Box<dyn Any>>;
    fn extract_dyn(&self, ast: &dyn Any, location: &str) -> (Vec<SyntaxEntity>, Vec<Edge>);
    #[allow(clippy::too_many_arguments)]
    fn resolve_file_dyn(
        &self,
        ast: &dyn Any,
        location: &str,
        all_entities: &[SyntaxEntity],
        symbol_table: &SymbolTable,
        edges_so_far: &[Edge],
        module_symbol: &SymbolEntity,
        file_module: &SyntaxEntity,
    ) -> (Vec<SymbolEntity>, Vec<Edge>);
}

impl<T> DynLanguageAdapter for T
where
    T: LanguageAdapter,
    T::Ast: 'static,
{
    fn parse_dyn(&self, source: &str) -> Result<Box<dyn Any>> {
        Ok(Box::new(self.parse(source)?))
    }

    fn extract_dyn(&self, ast: &dyn Any, location: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
        let ast = ast
            .downcast_ref::<T::Ast>()
            .expect("extract_dyn called with an ast from a different adapter");
        self.extract(ast, location)
    }

    fn resolve_file_dyn(
        &self,
        ast: &dyn Any,
        location: &str,
        all_entities: &[SyntaxEntity],
        symbol_table: &SymbolTable,
        edges_so_far: &[Edge],
        module_symbol: &SymbolEntity,
        file_module: &SyntaxEntity,
    ) -> (Vec<SymbolEntity>, Vec<Edge>) {
        let ast = ast
            .downcast_ref::<T::Ast>()
            .expect("resolve_file_dyn called with an ast from a different adapter");
        self.resolve_file(
            ast,
            location,
            all_entities,
            symbol_table,
            edges_so_far,
            module_symbol,
            file_module,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SymbolKind, SyntaxKind};

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
    fn resolve_file_defaults_to_empty_without_being_overridden() {
        let adapter = NoOpAdapter;
        let module = SyntaxEntity::new(SyntaxKind::Module, "src/lib.rs", "src/lib.rs");
        let module_symbol = SymbolEntity::new(SymbolKind::Namespace, "src/lib.rs", "src/lib.rs");
        let symbol_table = crate::symbols::build_symbol_table(&[]);

        let (symbols, edges) = adapter.resolve_file(
            &(),
            "src/lib.rs",
            &[],
            &symbol_table,
            &[],
            &module_symbol,
            &module,
        );

        assert!(symbols.is_empty());
        assert!(edges.is_empty());
    }

    #[test]
    fn dyn_adapter_dispatch_round_trips_through_parse_extract_and_resolve_file() {
        let adapter = NoOpAdapter;
        let dyn_adapter: &dyn DynLanguageAdapter = &adapter;

        let ast = dyn_adapter.parse_dyn("irrelevant source").expect("parse");
        let (entities, edges) = dyn_adapter.extract_dyn(ast.as_ref(), "src/lib.rs");
        assert!(entities.is_empty());
        assert!(edges.is_empty());

        let module = SyntaxEntity::new(SyntaxKind::Module, "src/lib.rs", "src/lib.rs");
        let module_symbol = SymbolEntity::new(SymbolKind::Namespace, "src/lib.rs", "src/lib.rs");
        let symbol_table = crate::symbols::build_symbol_table(&[]);
        let (symbols, edges) = dyn_adapter.resolve_file_dyn(
            ast.as_ref(),
            "src/lib.rs",
            &[],
            &symbol_table,
            &[],
            &module_symbol,
            &module,
        );
        assert!(symbols.is_empty());
        assert!(edges.is_empty());
    }
}
