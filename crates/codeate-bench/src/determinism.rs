//! Diffs two `StoredGraph` snapshots produced from independent `codeate
//! analyze --full` runs over the same fixture, to confirm Codeate's
//! determinism guarantee holds on real-world repos, not just the small
//! fixtures Phase 9's own tests already cover.

use std::collections::HashSet;

use codeate_core::{EntityId, StoredGraph};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeterminismReport {
    pub syntax_only_in_first: Vec<EntityId>,
    pub syntax_only_in_second: Vec<EntityId>,
    pub symbols_only_in_first: Vec<EntityId>,
    pub symbols_only_in_second: Vec<EntityId>,
    pub edges_only_in_first: usize,
    pub edges_only_in_second: usize,
}

impl DeterminismReport {
    pub fn is_clean(&self) -> bool {
        self.syntax_only_in_first.is_empty()
            && self.syntax_only_in_second.is_empty()
            && self.symbols_only_in_first.is_empty()
            && self.symbols_only_in_second.is_empty()
            && self.edges_only_in_first == 0
            && self.edges_only_in_second == 0
    }
}

/// Compares two independently-produced `StoredGraph`s entity-set-wise and
/// edge-count-wise, reporting exactly which ids diverge rather than a single
/// pass/fail bit -- a divergence here is a determinism bug worth a specific
/// repro, not just a red flag.
pub fn diff(first: &StoredGraph, second: &StoredGraph) -> DeterminismReport {
    let first_syntax: HashSet<&EntityId> = first.syntax_entities.iter().map(|e| &e.id).collect();
    let second_syntax: HashSet<&EntityId> = second.syntax_entities.iter().map(|e| &e.id).collect();
    let first_symbols: HashSet<&EntityId> = first.symbols.iter().map(|s| &s.id).collect();
    let second_symbols: HashSet<&EntityId> = second.symbols.iter().map(|s| &s.id).collect();
    let edge_key = |e: &codeate_core::Edge| (e.from.clone(), e.to.clone(), e.kind);
    let first_edges: HashSet<_> = first.edges.iter().map(edge_key).collect();
    let second_edges: HashSet<_> = second.edges.iter().map(edge_key).collect();

    DeterminismReport {
        syntax_only_in_first: first_syntax
            .difference(&second_syntax)
            .map(|id| (*id).clone())
            .collect(),
        syntax_only_in_second: second_syntax
            .difference(&first_syntax)
            .map(|id| (*id).clone())
            .collect(),
        symbols_only_in_first: first_symbols
            .difference(&second_symbols)
            .map(|id| (*id).clone())
            .collect(),
        symbols_only_in_second: second_symbols
            .difference(&first_symbols)
            .map(|id| (*id).clone())
            .collect(),
        edges_only_in_first: first_edges.difference(&second_edges).count(),
        edges_only_in_second: second_edges.difference(&first_edges).count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeate_core::{Edge, EdgeKind, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};

    fn graph_with(
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
    fn identical_graphs_diff_clean() {
        let entity = SyntaxEntity::new(SyntaxKind::Function, "a.rs", "foo");
        let symbol = SymbolEntity::new(SymbolKind::Function, "a.rs", "foo");
        let edge = Edge::new(entity.id.clone(), symbol.id.clone(), EdgeKind::DefinedIn);

        let first = graph_with(
            vec![entity.clone()],
            vec![symbol.clone()],
            vec![edge.clone()],
        );
        let second = graph_with(vec![entity], vec![symbol], vec![edge]);

        let report = diff(&first, &second);
        assert!(report.is_clean());
    }

    #[test]
    fn a_syntax_entity_present_in_only_one_run_is_reported() {
        let shared = SyntaxEntity::new(SyntaxKind::Function, "a.rs", "foo");
        let extra = SyntaxEntity::new(SyntaxKind::Function, "a.rs", "bar");

        let first = graph_with(vec![shared.clone(), extra.clone()], vec![], vec![]);
        let second = graph_with(vec![shared], vec![], vec![]);

        let report = diff(&first, &second);
        assert!(!report.is_clean());
        assert_eq!(report.syntax_only_in_first, vec![extra.id]);
        assert!(report.syntax_only_in_second.is_empty());
    }

    #[test]
    fn an_edge_present_in_only_one_run_is_counted() {
        let entity = SyntaxEntity::new(SyntaxKind::Function, "a.rs", "foo");
        let symbol = SymbolEntity::new(SymbolKind::Function, "a.rs", "foo");
        let edge = Edge::new(entity.id.clone(), symbol.id.clone(), EdgeKind::DefinedIn);

        let first = graph_with(vec![entity], vec![symbol], vec![edge]);
        let second = graph_with(vec![], vec![], vec![]);

        let report = diff(&first, &second);
        assert_eq!(report.edges_only_in_first, 1);
        assert_eq!(report.edges_only_in_second, 0);
    }
}
