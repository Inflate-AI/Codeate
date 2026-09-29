use crate::model::{EntityId, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};

/// A resolved symbol, paired with the `SyntaxEntity` id it was resolved
/// from -- later used (task 3) to emit `DEFINED_IN` edges back to the
/// symbol's declaring module.
pub struct SymbolTableEntry {
    pub symbol: SymbolEntity,
    pub syntax_id: EntityId,
}

/// The symbols resolved from one file's syntax entities.
pub struct SymbolTable {
    entries: Vec<SymbolTableEntry>,
}

impl SymbolTable {
    pub fn entries(&self) -> &[SymbolTableEntry] {
        &self.entries
    }

    pub fn symbols(&self) -> impl Iterator<Item = &SymbolEntity> {
        self.entries.iter().map(|entry| &entry.symbol)
    }

    /// The symbol resolved from the syntax entity `syntax_id`, if any.
    pub fn find_by_syntax_id(&self, syntax_id: &EntityId) -> Option<&SymbolEntity> {
        self.entries
            .iter()
            .find(|entry| &entry.syntax_id == syntax_id)
            .map(|entry| &entry.symbol)
    }

    /// The symbol with the given `EntityId`, if any.
    pub fn find_by_id(&self, id: &EntityId) -> Option<&SymbolEntity> {
        self.entries
            .iter()
            .find(|entry| &entry.symbol.id == id)
            .map(|entry| &entry.symbol)
    }
}

/// Combine multiple per-file `SymbolTable`s into one repository-level
/// table. Symbols from separate files never collide on `EntityId`, since
/// each id is derived from its originating (per-file, per-scope) syntax
/// entity id.
pub fn merge_symbol_tables(tables: Vec<SymbolTable>) -> SymbolTable {
    let mut merged = SymbolTable { entries: Vec::new() };
    for table in tables {
        merged.entries.extend(table.entries);
    }
    merged
}

/// Map a `SyntaxKind` to the `SymbolKind` it resolves to. `Statement` and
/// `Expression` don't bind a name, so they resolve to no symbol.
fn symbol_kind_for(kind: SyntaxKind) -> Option<SymbolKind> {
    match kind {
        SyntaxKind::Module | SyntaxKind::Package => Some(SymbolKind::Namespace),
        SyntaxKind::Class | SyntaxKind::Struct | SyntaxKind::Enum | SyntaxKind::Interface => {
            Some(SymbolKind::Type)
        }
        SyntaxKind::Function => Some(SymbolKind::Function),
        SyntaxKind::Method => Some(SymbolKind::Method),
        // Phase 3's closed SymbolKind enum has no dedicated variable kind;
        // variables and parameters resolve to the generic Symbol kind.
        SyntaxKind::Variable | SyntaxKind::Parameter => Some(SymbolKind::Symbol),
        SyntaxKind::Statement | SyntaxKind::Expression => None,
    }
}

/// Build a `SymbolTable` for one file's syntax entities. Each symbol's
/// `EntityId` is derived from its originating syntax entity's own id (which
/// already encodes the entity's declaring scope), so it stays distinct per
/// declaration site and deterministic across runs.
pub fn build_symbol_table(entities: &[SyntaxEntity]) -> SymbolTable {
    let mut table = SymbolTable { entries: Vec::new() };

    for entity in entities {
        let Some(kind) = symbol_kind_for(entity.kind) else {
            continue;
        };
        let symbol = SymbolEntity::new(kind, entity.id.as_str(), &entity.name);
        table.entries.push(SymbolTableEntry { symbol, syntax_id: entity.id.clone() });
    }

    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(kind: SyntaxKind, name: &str) -> SyntaxEntity {
        SyntaxEntity::new(kind, "src/lib.rs", name)
    }

    #[test]
    fn maps_each_syntax_kind_to_the_expected_symbol_kind() {
        let cases = [
            (SyntaxKind::Module, SymbolKind::Namespace),
            (SyntaxKind::Struct, SymbolKind::Type),
            (SyntaxKind::Enum, SymbolKind::Type),
            (SyntaxKind::Interface, SymbolKind::Type),
            (SyntaxKind::Function, SymbolKind::Function),
            (SyntaxKind::Method, SymbolKind::Method),
            (SyntaxKind::Variable, SymbolKind::Symbol),
            (SyntaxKind::Parameter, SymbolKind::Symbol),
        ];

        for (syntax_kind, expected_symbol_kind) in cases {
            let entities = vec![entity(syntax_kind, "thing")];
            let table = build_symbol_table(&entities);
            let symbol = table.symbols().next().expect("one symbol");
            assert_eq!(symbol.kind, expected_symbol_kind, "for {syntax_kind:?}");
        }
    }

    #[test]
    fn statements_and_expressions_resolve_to_no_symbol() {
        let entities = vec![entity(SyntaxKind::Statement, "stmt_0"), entity(SyntaxKind::Expression, "expr_0")];
        let table = build_symbol_table(&entities);
        assert_eq!(table.symbols().count(), 0);
    }

    #[test]
    fn a_symbol_is_reachable_by_its_originating_syntax_id() {
        let point = entity(SyntaxKind::Struct, "Point");
        let entities = vec![point.clone()];
        let table = build_symbol_table(&entities);

        let symbol = table.find_by_syntax_id(&point.id).expect("symbol for Point");
        assert_eq!(symbol.name, "Point");
        assert_eq!(symbol.kind, SymbolKind::Type);
    }

    #[test]
    fn symbol_carries_its_declared_name() {
        let entities = vec![entity(SyntaxKind::Function, "run")];
        let table = build_symbol_table(&entities);
        let symbol = table.symbols().next().expect("one symbol");
        assert_eq!(symbol.name, "run");
    }

    fn entity_at(location: &str, kind: SyntaxKind, name: &str) -> SyntaxEntity {
        SyntaxEntity::new(kind, location, name)
    }

    #[test]
    fn merging_tables_from_different_files_keeps_every_symbol_without_collision() {
        let a_entities = vec![entity_at("src/a.rs", SyntaxKind::Struct, "Point")];
        let b_entities = vec![entity_at("src/b.rs", SyntaxKind::Struct, "Point")];
        let table_a = build_symbol_table(&a_entities);
        let table_b = build_symbol_table(&b_entities);

        let merged = merge_symbol_tables(vec![table_a, table_b]);

        assert_eq!(merged.symbols().count(), 2);
        let symbol_a = merged.find_by_syntax_id(&a_entities[0].id).expect("a's Point");
        let symbol_b = merged.find_by_syntax_id(&b_entities[0].id).expect("b's Point");
        assert_ne!(symbol_a.id, symbol_b.id);
    }

    #[test]
    fn merged_table_finds_symbols_from_any_input_file_by_id() {
        let a_entities = vec![entity_at("src/a.rs", SyntaxKind::Function, "run")];
        let b_entities = vec![entity_at("src/b.rs", SyntaxKind::Function, "walk")];
        let table_a = build_symbol_table(&a_entities);
        let table_b = build_symbol_table(&b_entities);
        let run_id = table_a.symbols().next().unwrap().id.clone();
        let walk_id = table_b.symbols().next().unwrap().id.clone();

        let merged = merge_symbol_tables(vec![table_a, table_b]);

        assert_eq!(merged.find_by_id(&run_id).map(|s| s.name.as_str()), Some("run"));
        assert_eq!(merged.find_by_id(&walk_id).map(|s| s.name.as_str()), Some("walk"));
    }
}
