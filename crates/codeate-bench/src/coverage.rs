//! Computes per-kind observed counts from a cold run's `StoredGraph`, so a
//! language's coverage/depth table reports real numbers for Codeate's row
//! the same way it does for every competitor tool's row (their counts come
//! from their own `status --json`/`graph.json` output, not a hand-read list
//! of possible kind names) -- Codeate's own row shouldn't be the one
//! exception that looks unmeasured.

use std::collections::BTreeMap;

use codeate_core::StoredGraph;

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct CoverageSummary {
    /// `SyntaxKind::as_str()` -> observed count. Only kinds that occur at
    /// least once are present -- an absent kind means zero, matching how
    /// the competitor tools' own rows only list kinds they actually
    /// produced, not their full possible vocabulary.
    pub syntax_kinds: BTreeMap<String, usize>,
    pub symbol_kinds: BTreeMap<String, usize>,
    pub edge_kinds: BTreeMap<String, usize>,
}

pub fn summarize(graph: &StoredGraph) -> CoverageSummary {
    let mut summary = CoverageSummary::default();

    for entity in &graph.syntax_entities {
        *summary
            .syntax_kinds
            .entry(entity.kind.as_str().to_string())
            .or_insert(0) += 1;
    }
    for symbol in &graph.symbols {
        *summary
            .symbol_kinds
            .entry(symbol.kind.as_str().to_string())
            .or_insert(0) += 1;
    }
    for edge in &graph.edges {
        *summary
            .edge_kinds
            .entry(edge.kind.as_str().to_string())
            .or_insert(0) += 1;
    }

    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeate_core::{Edge, EdgeKind, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};

    fn stored_graph(
        entities: Vec<SyntaxEntity>,
        symbols: Vec<SymbolEntity>,
        edges: Vec<Edge>,
    ) -> StoredGraph {
        StoredGraph {
            version: codeate_core::CURRENT_VERSION,
            syntax_entities: entities,
            symbols,
            edges,
            files: Vec::new(),
            owner_file: Default::default(),
        }
    }

    #[test]
    fn an_empty_graph_summarizes_to_empty_maps() {
        let graph = stored_graph(vec![], vec![], vec![]);
        let summary = summarize(&graph);
        assert!(summary.syntax_kinds.is_empty());
        assert!(summary.symbol_kinds.is_empty());
        assert!(summary.edge_kinds.is_empty());
    }

    #[test]
    fn counts_syntax_entities_by_kind() {
        let graph = stored_graph(
            vec![
                SyntaxEntity::new(SyntaxKind::Function, "a.rs", "foo"),
                SyntaxEntity::new(SyntaxKind::Function, "a.rs", "bar"),
                SyntaxEntity::new(SyntaxKind::Struct, "a.rs", "Baz"),
            ],
            vec![],
            vec![],
        );
        let summary = summarize(&graph);
        assert_eq!(summary.syntax_kinds.get("Function"), Some(&2));
        assert_eq!(summary.syntax_kinds.get("Struct"), Some(&1));
        assert_eq!(summary.syntax_kinds.len(), 2);
    }

    #[test]
    fn a_kind_with_zero_occurrences_is_absent_not_zero() {
        let graph = stored_graph(
            vec![SyntaxEntity::new(SyntaxKind::Function, "a.rs", "foo")],
            vec![],
            vec![],
        );
        let summary = summarize(&graph);
        assert_eq!(summary.syntax_kinds.get("Struct"), None);
    }

    #[test]
    fn counts_symbols_and_edges_by_kind() {
        let func = SyntaxEntity::new(SyntaxKind::Function, "a.rs", "foo");
        let symbol = SymbolEntity::new(SymbolKind::Function, "a.rs", "foo");
        let edge_a = Edge::new(func.id.clone(), symbol.id.clone(), EdgeKind::DefinedIn);
        let edge_b = Edge::new(func.id.clone(), symbol.id.clone(), EdgeKind::Calls);
        let graph = stored_graph(vec![func], vec![symbol], vec![edge_a, edge_b]);

        let summary = summarize(&graph);
        assert_eq!(summary.symbol_kinds.get("Function"), Some(&1));
        assert_eq!(summary.edge_kinds.get("DEFINED_IN"), Some(&1));
        assert_eq!(summary.edge_kinds.get("CALLS"), Some(&1));
    }
}
