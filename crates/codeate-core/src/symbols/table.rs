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
}
