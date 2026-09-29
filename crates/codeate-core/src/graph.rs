use std::collections::HashMap;

use crate::hash::hash_bytes;
use crate::model::{Edge, EdgeKind, EntityId, SymbolEntity, SyntaxEntity};

/// A single graph node: either a Phase 3 syntax entity or a resolved symbol
/// entity. The two have distinct Rust types (different `kind` enums), so the
/// graph unifies them behind this one node type rather than forcing callers
/// to juggle two parallel entity lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Syntax(SyntaxEntity),
    Symbol(SymbolEntity),
}

impl Node {
    pub fn id(&self) -> &EntityId {
        match self {
            Node::Syntax(entity) => &entity.id,
            Node::Symbol(entity) => &entity.id,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Node::Syntax(entity) => &entity.name,
            Node::Symbol(entity) => &entity.name,
        }
    }
}

/// The in-memory Code Graph: every node (syntax + symbol) and edge from a
/// `pipeline::AnalysisResult`, indexed for O(1) node lookup and adjacency
/// queries. The only way to build one is `Graph::build`, which also runs the
/// structural integrity checks (task 3) so a caller can never end up with an
/// un-audited graph.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    nodes: HashMap<EntityId, Node>,
    outgoing: HashMap<EntityId, Vec<Edge>>,
    incoming: HashMap<EntityId, Vec<Edge>>,
}

impl Graph {
    /// Assemble a `Graph` from the accumulated entity/edge stream (a
    /// `pipeline::AnalysisResult`'s fields, typically), returning both the
    /// graph and its integrity report. Never panics or drops input on a
    /// detected problem -- a dangling edge or invalid cycle is still indexed
    /// (its endpoints and adjacency lists are recorded as given) and simply
    /// also reported, so nothing is silently discarded.
    pub fn build(
        syntax_entities: Vec<SyntaxEntity>,
        symbols: Vec<SymbolEntity>,
        edges: Vec<Edge>,
    ) -> (Graph, IntegrityReport) {
        let mut nodes = HashMap::new();
        for entity in syntax_entities {
            nodes.insert(entity.id.clone(), Node::Syntax(entity));
        }
        for symbol in symbols {
            nodes.insert(symbol.id.clone(), Node::Symbol(symbol));
        }

        let mut outgoing: HashMap<EntityId, Vec<Edge>> = HashMap::new();
        let mut incoming: HashMap<EntityId, Vec<Edge>> = HashMap::new();
        let mut dangling_edges = Vec::new();
        let mut invalid_cycles = Vec::new();

        for edge in edges {
            if !nodes.contains_key(&edge.from) || !nodes.contains_key(&edge.to) {
                dangling_edges.push(edge.clone());
            }
            if edge.from == edge.to && is_structurally_irreflexive(edge.kind) {
                invalid_cycles.push(edge.clone());
            }
            outgoing
                .entry(edge.from.clone())
                .or_default()
                .push(edge.clone());
            incoming.entry(edge.to.clone()).or_default().push(edge);
        }

        let orphaned_nodes = nodes
            .keys()
            .filter(|id| !outgoing.contains_key(*id) && !incoming.contains_key(*id))
            .cloned()
            .collect();

        let graph = Graph {
            nodes,
            outgoing,
            incoming,
        };
        let report = IntegrityReport {
            dangling_edges,
            orphaned_nodes,
            invalid_cycles,
        };

        (graph, report)
    }

    pub fn node(&self, id: &EntityId) -> Option<&Node> {
        self.nodes.get(id)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn outgoing(&self, id: &EntityId) -> &[Edge] {
        self.outgoing.get(id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn incoming(&self, id: &EntityId) -> &[Edge] {
        self.incoming.get(id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn outgoing_of_kind<'a>(
        &'a self,
        id: &EntityId,
        kind: EdgeKind,
    ) -> impl Iterator<Item = &'a Edge> {
        self.outgoing(id)
            .iter()
            .filter(move |edge| edge.kind == kind)
    }

    pub fn incoming_of_kind<'a>(
        &'a self,
        id: &EntityId,
        kind: EdgeKind,
    ) -> impl Iterator<Item = &'a Edge> {
        self.incoming(id)
            .iter()
            .filter(move |edge| edge.kind == kind)
    }

    /// A canonical, deterministically-ordered textual form of this graph:
    /// every node id and every edge, sorted, so two graphs built from the
    /// same input produce byte-identical output regardless of the
    /// `HashMap`-driven construction order.
    pub fn canonical_form(&self) -> String {
        let mut node_lines: Vec<String> = self
            .nodes
            .values()
            .map(|node| {
                let kind = match node {
                    Node::Syntax(entity) => format!("syntax:{}", entity.kind.as_str()),
                    Node::Symbol(entity) => format!("symbol:{}", entity.kind.as_str()),
                };
                format!("{}\t{}\t{}", node.id().as_str(), kind, node.name())
            })
            .collect();
        node_lines.sort();

        let mut edge_lines: Vec<String> = self
            .outgoing
            .values()
            .flatten()
            .map(|edge| {
                format!(
                    "{}\t{}\t{}",
                    edge.from.as_str(),
                    edge.kind.as_str(),
                    edge.to.as_str()
                )
            })
            .collect();
        edge_lines.sort();

        format!(
            "NODES\n{}\nEDGES\n{}",
            node_lines.join("\n"),
            edge_lines.join("\n")
        )
    }

    /// A compact, deterministic fingerprint of `canonical_form()` (SHA-256
    /// hex digest via the Phase 1 `hash_bytes` primitive) -- cheaper to
    /// compare/store than the full canonical form when only "did this graph
    /// change at all" matters.
    pub fn fingerprint(&self) -> String {
        hash_bytes(self.canonical_form().as_bytes())
    }
}

/// Structural relationship kinds where a self-loop (`edge.from == edge.to`)
/// can never be legitimate -- a type can't extend, implement, or override
/// itself. Kinds like `CONTAINS`/`REFERENCES` are deliberately excluded:
/// this is about specific invalid *structural* cycles, not cycle detection
/// in general (a recursive function referencing/calling itself is normal).
fn is_structurally_irreflexive(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Extends | EdgeKind::Implements | EdgeKind::Overrides
    )
}

/// Structural problems `Graph::build` found while assembling a graph,
/// reported rather than silently dropped or panicked on (mirrors the
/// project's "represent explicitly" rule for unresolved references).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntegrityReport {
    /// An edge whose `from` or `to` doesn't match any node in the graph.
    pub dangling_edges: Vec<Edge>,
    /// A node with no incoming or outgoing edges at all. Not necessarily
    /// invalid -- an `unresolved_symbol` placeholder is expected to be a
    /// pure sink -- just surfaced for inspection.
    pub orphaned_nodes: Vec<EntityId>,
    /// A self-referential structural edge (a type `EXTENDS`/`IMPLEMENTS`/
    /// `OVERRIDES` itself) that can never be legitimate.
    pub invalid_cycles: Vec<Edge>,
}

impl IntegrityReport {
    pub fn is_clean(&self) -> bool {
        self.dangling_edges.is_empty() && self.invalid_cycles.is_empty()
    }
}

/// The difference between two graphs: nodes/edges present in `b` but not
/// `a` ("added"), and present in `a` but not `b` ("removed"). Comparison is
/// by value (`Node`/`Edge` equality), not by identity, so the same logical
/// entity/edge appearing in both graphs is never reported as changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphDiff {
    pub added_nodes: Vec<EntityId>,
    pub removed_nodes: Vec<EntityId>,
    pub added_edges: Vec<Edge>,
    pub removed_edges: Vec<Edge>,
}

impl GraphDiff {
    pub fn is_empty(&self) -> bool {
        self.added_nodes.is_empty()
            && self.removed_nodes.is_empty()
            && self.added_edges.is_empty()
            && self.removed_edges.is_empty()
    }
}

/// Diff two graphs. Used by Phase 13's incremental analysis to find the
/// blast radius of a change; proven here against Phase 9's own determinism
/// guarantee (same input twice -> empty diff).
pub fn diff(a: &Graph, b: &Graph) -> GraphDiff {
    let mut added_nodes: Vec<EntityId> = b
        .nodes
        .keys()
        .filter(|id| !a.nodes.contains_key(*id))
        .cloned()
        .collect();
    let mut removed_nodes: Vec<EntityId> = a
        .nodes
        .keys()
        .filter(|id| !b.nodes.contains_key(*id))
        .cloned()
        .collect();
    added_nodes.sort_by(|x, y| x.as_str().cmp(y.as_str()));
    removed_nodes.sort_by(|x, y| x.as_str().cmp(y.as_str()));

    let a_edges: Vec<&Edge> = a.outgoing.values().flatten().collect();
    let b_edges: Vec<&Edge> = b.outgoing.values().flatten().collect();

    let mut added_edges: Vec<Edge> = b_edges
        .iter()
        .filter(|edge| !a_edges.contains(edge))
        .map(|edge| (*edge).clone())
        .collect();
    let mut removed_edges: Vec<Edge> = a_edges
        .iter()
        .filter(|edge| !b_edges.contains(edge))
        .map(|edge| (*edge).clone())
        .collect();
    added_edges.sort_by(|x, y| edge_sort_key(x).cmp(&edge_sort_key(y)));
    removed_edges.sort_by(|x, y| edge_sort_key(x).cmp(&edge_sort_key(y)));

    GraphDiff {
        added_nodes,
        removed_nodes,
        added_edges,
        removed_edges,
    }
}

fn edge_sort_key(edge: &Edge) -> (String, &'static str, String) {
    (
        edge.from.as_str().to_string(),
        edge.kind.as_str(),
        edge.to.as_str().to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SymbolKind, SyntaxKind};

    fn syntax(kind: SyntaxKind, name: &str) -> SyntaxEntity {
        SyntaxEntity::new(kind, "src/lib.rs", name)
    }

    fn symbol(kind: SymbolKind, name: &str) -> SymbolEntity {
        SymbolEntity::new(kind, "src/lib.rs", name)
    }

    #[test]
    fn node_lookup_and_adjacency_match_the_input_exactly() {
        let module = syntax(SyntaxKind::Module, "src/lib.rs");
        let point = syntax(SyntaxKind::Struct, "Point");
        let edge = Edge::new(module.id.clone(), point.id.clone(), EdgeKind::Contains);

        let (graph, report) = Graph::build(
            vec![module.clone(), point.clone()],
            vec![],
            vec![edge.clone()],
        );

        assert_eq!(graph.node_count(), 2);
        assert_eq!(graph.node(&module.id), Some(&Node::Syntax(module.clone())));
        assert_eq!(graph.node(&point.id), Some(&Node::Syntax(point.clone())));
        assert_eq!(graph.outgoing(&module.id), std::slice::from_ref(&edge));
        assert_eq!(graph.incoming(&point.id), &[edge]);
        assert!(graph.outgoing(&point.id).is_empty());
        assert!(report.is_clean());
    }

    #[test]
    fn outgoing_of_kind_filters_to_just_that_edge_kind() {
        let a = syntax(SyntaxKind::Module, "a");
        let b = syntax(SyntaxKind::Struct, "b");
        let contains = Edge::new(a.id.clone(), b.id.clone(), EdgeKind::Contains);
        let declares = Edge::new(a.id.clone(), b.id.clone(), EdgeKind::Declares);

        let (graph, _report) =
            Graph::build(vec![a.clone(), b], vec![], vec![contains.clone(), declares]);

        let contains_only: Vec<&Edge> = graph.outgoing_of_kind(&a.id, EdgeKind::Contains).collect();
        assert_eq!(contains_only, vec![&contains]);
    }

    #[test]
    fn a_dangling_edge_is_reported_but_still_indexed() {
        let a = syntax(SyntaxKind::Module, "a");
        let missing = crate::model::EntityId::new("Struct", "src/lib.rs", "Ghost", None);
        let edge = Edge::new(a.id.clone(), missing.clone(), EdgeKind::Contains);

        let (graph, report) = Graph::build(vec![a.clone()], vec![], vec![edge.clone()]);

        assert_eq!(report.dangling_edges, vec![edge.clone()]);
        assert!(!report.is_clean());
        // Still indexed -- the caller can inspect what's there, not just be
        // told it's broken.
        assert_eq!(graph.outgoing(&a.id), &[edge]);
    }

    #[test]
    fn a_self_extends_edge_is_reported_as_an_invalid_cycle() {
        let t = syntax(SyntaxKind::Interface, "Weird");
        let edge = Edge::new(t.id.clone(), t.id.clone(), EdgeKind::Extends);

        let (_graph, report) = Graph::build(vec![t], vec![], vec![edge.clone()]);

        assert_eq!(report.invalid_cycles, vec![edge]);
        assert!(!report.is_clean());
    }

    #[test]
    fn a_self_referencing_call_is_not_an_invalid_cycle() {
        let f = syntax(SyntaxKind::Function, "recurse");
        let edge = Edge::new(f.id.clone(), f.id.clone(), EdgeKind::Calls);

        let (_graph, report) = Graph::build(vec![f], vec![], vec![edge]);

        assert!(report.invalid_cycles.is_empty());
        assert!(report.is_clean());
    }

    #[test]
    fn an_unresolved_placeholder_with_no_edges_is_a_legitimate_orphan() {
        let placeholder = symbol(SymbolKind::Symbol, "external-crate:serde");

        let (_graph, report) = Graph::build(vec![], vec![placeholder.clone()], vec![]);

        assert_eq!(report.orphaned_nodes, vec![placeholder.id]);
        // Orphaned isn't itself a failure -- only dangling edges/invalid
        // cycles are.
        assert!(report.is_clean());
    }

    #[test]
    fn a_node_with_any_edge_is_not_orphaned() {
        let a = syntax(SyntaxKind::Module, "a");
        let b = syntax(SyntaxKind::Struct, "b");
        let edge = Edge::new(a.id.clone(), b.id.clone(), EdgeKind::Contains);

        let (_graph, report) = Graph::build(vec![a, b], vec![], vec![edge]);

        assert!(report.orphaned_nodes.is_empty());
    }

    #[test]
    fn building_the_same_input_twice_produces_an_identical_fingerprint() {
        let a = syntax(SyntaxKind::Module, "a");
        let b = syntax(SyntaxKind::Struct, "b");
        let edge = Edge::new(a.id.clone(), b.id.clone(), EdgeKind::Contains);

        let (graph1, _) = Graph::build(vec![a.clone(), b.clone()], vec![], vec![edge.clone()]);
        let (graph2, _) = Graph::build(vec![a, b], vec![], vec![edge]);

        assert_eq!(graph1.canonical_form(), graph2.canonical_form());
        assert_eq!(graph1.fingerprint(), graph2.fingerprint());
    }

    #[test]
    fn two_graphs_from_different_fixtures_produce_a_different_fingerprint() {
        let a = syntax(SyntaxKind::Module, "a");
        let b = syntax(SyntaxKind::Struct, "b");
        let c = syntax(SyntaxKind::Struct, "c");
        let edge_ab = Edge::new(a.id.clone(), b.id.clone(), EdgeKind::Contains);
        let edge_ac = Edge::new(a.id.clone(), c.id.clone(), EdgeKind::Contains);

        let (graph1, _) = Graph::build(vec![a.clone(), b], vec![], vec![edge_ab]);
        let (graph2, _) = Graph::build(vec![a, c], vec![], vec![edge_ac]);

        assert_ne!(graph1.fingerprint(), graph2.fingerprint());
    }

    #[test]
    fn diffing_two_builds_of_the_same_input_is_empty() {
        let a = syntax(SyntaxKind::Module, "a");
        let b = syntax(SyntaxKind::Struct, "b");
        let edge = Edge::new(a.id.clone(), b.id.clone(), EdgeKind::Contains);

        let (graph1, _) = Graph::build(vec![a.clone(), b.clone()], vec![], vec![edge.clone()]);
        let (graph2, _) = Graph::build(vec![a, b], vec![], vec![edge]);

        assert!(diff(&graph1, &graph2).is_empty());
    }

    #[test]
    fn diffing_two_different_fixtures_reports_the_added_and_removed_node_and_edge() {
        let a = syntax(SyntaxKind::Module, "a");
        let b = syntax(SyntaxKind::Struct, "b");
        let c = syntax(SyntaxKind::Struct, "c");
        let edge_ab = Edge::new(a.id.clone(), b.id.clone(), EdgeKind::Contains);
        let edge_ac = Edge::new(a.id.clone(), c.id.clone(), EdgeKind::Contains);

        let (graph1, _) = Graph::build(vec![a.clone(), b.clone()], vec![], vec![edge_ab.clone()]);
        let (graph2, _) = Graph::build(vec![a, c.clone()], vec![], vec![edge_ac.clone()]);

        let d = diff(&graph1, &graph2);
        assert_eq!(d.added_nodes, vec![c.id.clone()]);
        assert_eq!(d.removed_nodes, vec![b.id.clone()]);
        assert_eq!(d.added_edges, vec![edge_ac]);
        assert_eq!(d.removed_edges, vec![edge_ab]);
    }
}
