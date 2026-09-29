use crate::model::{Edge, EdgeKind};
use crate::symbols::table::SymbolTable;

/// Emit a `DEFINED_IN` edge from each symbol in `table` to the syntax
/// module that declares it -- found by following the `DECLARES` edge that
/// points at the symbol's originating syntax entity. A symbol whose syntax
/// entity has no incoming `DECLARES` edge (the file's own root module, which
/// nothing else declares) gets none.
pub fn defined_in_edges(table: &SymbolTable, syntax_edges: &[Edge]) -> Vec<Edge> {
    let mut result = Vec::new();

    for entry in table.entries() {
        let declaring_module = syntax_edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Declares && edge.to == entry.syntax_id);

        if let Some(declares_edge) = declaring_module {
            result.push(Edge::new(
                entry.symbol.id.clone(),
                declares_edge.from.clone(),
                EdgeKind::DefinedIn,
            ));
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SyntaxEntity, SyntaxKind};
    use crate::symbols::table::build_symbol_table;

    #[test]
    fn emits_defined_in_from_a_symbol_to_its_declaring_module() {
        let module = SyntaxEntity::new(SyntaxKind::Module, "src/lib.rs", "src/lib.rs");
        let point = SyntaxEntity::new(SyntaxKind::Struct, "src/lib.rs", "Point");
        let syntax_edges = vec![
            Edge::new(module.id.clone(), point.id.clone(), EdgeKind::Contains),
            Edge::new(module.id.clone(), point.id.clone(), EdgeKind::Declares),
        ];
        let entities = vec![module.clone(), point.clone()];
        let table = build_symbol_table(&entities);

        let defined_in = defined_in_edges(&table, &syntax_edges);

        let point_symbol = table.find_by_syntax_id(&point.id).unwrap();
        assert!(defined_in.contains(&Edge::new(
            point_symbol.id.clone(),
            module.id.clone(),
            EdgeKind::DefinedIn
        )));
    }

    #[test]
    fn a_symbol_with_no_declaring_module_gets_no_defined_in_edge() {
        let module = SyntaxEntity::new(SyntaxKind::Module, "src/lib.rs", "src/lib.rs");
        let entities = vec![module.clone()];
        let table = build_symbol_table(&entities);

        let defined_in = defined_in_edges(&table, &[]);

        assert!(defined_in.is_empty());
    }

    #[test]
    fn every_declared_symbol_gets_exactly_one_defined_in_edge() {
        let module = SyntaxEntity::new(SyntaxKind::Module, "src/lib.rs", "src/lib.rs");
        let point = SyntaxEntity::new(SyntaxKind::Struct, "src/lib.rs", "Point");
        let shape = SyntaxEntity::new(SyntaxKind::Enum, "src/lib.rs", "Shape");
        let syntax_edges = vec![
            Edge::new(module.id.clone(), point.id.clone(), EdgeKind::Declares),
            Edge::new(module.id.clone(), shape.id.clone(), EdgeKind::Declares),
        ];
        let entities = vec![module.clone(), point.clone(), shape.clone()];
        let table = build_symbol_table(&entities);

        let defined_in = defined_in_edges(&table, &syntax_edges);

        for declared in [&point, &shape] {
            let symbol = table.find_by_syntax_id(&declared.id).unwrap();
            let count = defined_in.iter().filter(|e| e.from == symbol.id).count();
            assert_eq!(
                count, 1,
                "expected exactly one DEFINED_IN edge for {}",
                declared.name
            );
        }
    }
}
