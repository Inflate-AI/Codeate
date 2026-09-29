use std::collections::{HashSet, VecDeque};

use crate::graph::{Graph, Node};
use crate::model::{EdgeKind, EntityId, SymbolEntity, SyntaxEntity, SyntaxKind};

/// A nested view of a repository's `Module` syntax entities, following
/// `CONTAINS` edges between them (task 7). Built once by
/// `QueryEngine::module_tree` rather than walked lazily, since a repository's
/// module count is small enough that materializing it is simpler than a
/// streaming API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleTree {
    pub module: SyntaxEntity,
    pub children: Vec<ModuleTree>,
}

/// The deterministic query surface over an already-built `Graph` (typically
/// loaded from a Phase 10 `StoredGraph`). Every query here reads the graph's
/// existing edges -- no re-parsing, no re-resolving, no inference.
pub struct QueryEngine<'g> {
    graph: &'g Graph,
}

impl<'g> QueryEngine<'g> {
    pub fn new(graph: &'g Graph) -> Self {
        Self { graph }
    }

    // -- Task 1: symbol lookup and definition lookup --------------------

    /// Every symbol in the graph with exactly this name (there may be
    /// several -- same-named symbols in different files/scopes get distinct
    /// `EntityId`s, per Phase 5, so all of them are legitimate matches).
    pub fn symbols_named(&self, name: &str) -> Vec<&'g SymbolEntity> {
        self.graph
            .node_ids()
            .filter_map(|id| match self.graph.node(id) {
                Some(Node::Symbol(symbol)) if symbol.name == name => Some(symbol),
                _ => None,
            })
            .collect()
    }

    pub fn symbol_by_id(&self, id: &EntityId) -> Option<&'g SymbolEntity> {
        match self.graph.node(id) {
            Some(Node::Symbol(symbol)) => Some(symbol),
            _ => None,
        }
    }

    /// The syntax entity that declares `symbol_id`, following its
    /// `DEFINED_IN` edge (Phase 5). `None` if the symbol has no `DEFINED_IN`
    /// edge (e.g. an `unresolved_symbol` placeholder, or the file's own root
    /// module, which nothing declares).
    pub fn definition_of(&self, symbol_id: &EntityId) -> Option<&'g SyntaxEntity> {
        let edge = self
            .graph
            .outgoing_of_kind(symbol_id, EdgeKind::DefinedIn)
            .next()?;
        match self.graph.node(&edge.to) {
            Some(Node::Syntax(entity)) => Some(entity),
            _ => None,
        }
    }

    // -- Task 2: call graph queries --------------------------------------

    /// Every symbol with a `CALLS` edge into `callee_id`.
    pub fn callers_of(&self, callee_id: &EntityId) -> Vec<&'g SymbolEntity> {
        self.symbols_at_incoming(callee_id, EdgeKind::Calls)
    }

    /// Every symbol `caller_id` has a `CALLS` edge into.
    pub fn callees_of(&self, caller_id: &EntityId) -> Vec<&'g SymbolEntity> {
        self.symbols_at_outgoing(caller_id, EdgeKind::Calls)
    }

    // -- Task 3: dependency / reverse-dependency analysis ----------------

    /// Everything transitively reachable from `id` via outgoing
    /// `DEPENDS_ON`/`IMPORTS` edges.
    pub fn dependencies_of(&self, id: &EntityId) -> Vec<EntityId> {
        self.transitive_closure(
            id,
            &[EdgeKind::DependsOn, EdgeKind::Imports],
            Direction::Outgoing,
        )
    }

    /// Everything that transitively depends on `id` via incoming
    /// `DEPENDS_ON`/`IMPORTS` edges.
    pub fn dependents_of(&self, id: &EntityId) -> Vec<EntityId> {
        self.transitive_closure(
            id,
            &[EdgeKind::DependsOn, EdgeKind::Imports],
            Direction::Incoming,
        )
    }

    // -- Task 4: implementation lookup -----------------------------------

    /// Every symbol with an `IMPLEMENTS` edge into `trait_id`.
    pub fn implementers_of(&self, trait_id: &EntityId) -> Vec<&'g SymbolEntity> {
        self.symbols_at_incoming(trait_id, EdgeKind::Implements)
    }

    // -- Task 5: reference lookup -----------------------------------------

    /// Every symbol with a `REFERENCES` edge into `symbol_id` -- the
    /// enclosing function/method that referenced it (Phase 7 attributes a
    /// reference to its containing symbol, not a fine-grained expression
    /// site). Empty, not an error, when nothing references it.
    pub fn references_to(&self, symbol_id: &EntityId) -> Vec<&'g SymbolEntity> {
        self.symbols_at_incoming(symbol_id, EdgeKind::References)
    }

    // -- Task 6: impact analysis ------------------------------------------

    /// The transitive closure of everything that depends on `id`: a reverse
    /// walk over every edge kind that represents "the source needs the
    /// target to keep working" (`CALLS`, `REFERENCES`, `IMPLEMENTS`,
    /// `EXTENDS`, `OVERRIDES`, `DEPENDS_ON`, `IMPORTS`). A leaf symbol with
    /// no dependents returns an empty set, not an error.
    pub fn impact_of(&self, id: &EntityId) -> Vec<EntityId> {
        const IMPACT_KINDS: [EdgeKind; 7] = [
            EdgeKind::Calls,
            EdgeKind::References,
            EdgeKind::Implements,
            EdgeKind::Extends,
            EdgeKind::Overrides,
            EdgeKind::DependsOn,
            EdgeKind::Imports,
        ];
        self.transitive_closure(id, &IMPACT_KINDS, Direction::Incoming)
    }

    // -- Task 7: repository exploration -----------------------------------

    /// Functions/methods named `main` -- the only entry-point heuristic this
    /// phase implements. Never silently empty without a reason: an empty
    /// result means no `main` was found, which the caller can distinguish
    /// from "didn't look" by the fact this always runs over the whole graph.
    pub fn entry_points(&self) -> Vec<&'g SyntaxEntity> {
        self.graph
            .node_ids()
            .filter_map(|id| match self.graph.node(id) {
                Some(Node::Syntax(entity))
                    if matches!(entity.kind, SyntaxKind::Function | SyntaxKind::Method)
                        && entity.name == "main" =>
                {
                    Some(entity)
                }
                _ => None,
            })
            .collect()
    }

    /// The `CONTAINS` hierarchy of `Module` syntax entities, rooted at every
    /// module with no incoming `CONTAINS` edge from another module (i.e.
    /// every file's own top-level module).
    pub fn module_tree(&self) -> Vec<ModuleTree> {
        let mut roots = Vec::new();
        for id in self.graph.node_ids() {
            let Some(Node::Syntax(entity)) = self.graph.node(id) else {
                continue;
            };
            if entity.kind != SyntaxKind::Module {
                continue;
            }
            let has_module_parent = self
                .graph
                .incoming_of_kind(id, EdgeKind::Contains)
                .any(|edge| matches!(self.graph.node(&edge.from), Some(Node::Syntax(parent)) if parent.kind == SyntaxKind::Module));
            if !has_module_parent {
                roots.push(self.build_module_tree(entity));
            }
        }
        roots
    }

    fn build_module_tree(&self, module: &SyntaxEntity) -> ModuleTree {
        let children = self
            .graph
            .outgoing_of_kind(&module.id, EdgeKind::Contains)
            .filter_map(|edge| match self.graph.node(&edge.to) {
                Some(Node::Syntax(child)) if child.kind == SyntaxKind::Module => {
                    Some(self.build_module_tree(child))
                }
                _ => None,
            })
            .collect();
        ModuleTree {
            module: module.clone(),
            children,
        }
    }

    // -- shared helpers -----------------------------------------------------

    fn symbols_at_incoming(&self, id: &EntityId, kind: EdgeKind) -> Vec<&'g SymbolEntity> {
        self.graph
            .incoming_of_kind(id, kind)
            .filter_map(|edge| self.symbol_by_id(&edge.from))
            .collect()
    }

    fn symbols_at_outgoing(&self, id: &EntityId, kind: EdgeKind) -> Vec<&'g SymbolEntity> {
        self.graph
            .outgoing_of_kind(id, kind)
            .filter_map(|edge| self.symbol_by_id(&edge.to))
            .collect()
    }

    fn transitive_closure(
        &self,
        start: &EntityId,
        kinds: &[EdgeKind],
        direction: Direction,
    ) -> Vec<EntityId> {
        let mut visited: HashSet<EntityId> = HashSet::new();
        let mut queue: VecDeque<EntityId> = VecDeque::new();
        queue.push_back(start.clone());

        let mut result = Vec::new();
        while let Some(current) = queue.pop_front() {
            let edges: Vec<&crate::model::Edge> = kinds
                .iter()
                .flat_map(|kind| match direction {
                    Direction::Outgoing => self
                        .graph
                        .outgoing_of_kind(&current, *kind)
                        .collect::<Vec<_>>(),
                    Direction::Incoming => self
                        .graph
                        .incoming_of_kind(&current, *kind)
                        .collect::<Vec<_>>(),
                })
                .collect();

            for edge in edges {
                let next = match direction {
                    Direction::Outgoing => edge.to.clone(),
                    Direction::Incoming => edge.from.clone(),
                };
                if visited.insert(next.clone()) {
                    result.push(next.clone());
                    queue.push_back(next);
                }
            }
        }

        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Outgoing,
    Incoming,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;
    use crate::model::{Edge, SymbolKind};

    fn syntax(kind: SyntaxKind, name: &str) -> SyntaxEntity {
        SyntaxEntity::new(kind, "src/lib.rs", name)
    }

    fn symbol(kind: SymbolKind, name: &str) -> SymbolEntity {
        SymbolEntity::new(kind, "src/lib.rs", name)
    }

    #[test]
    fn symbols_named_finds_every_same_named_symbol_and_by_id_finds_exactly_one() {
        let a = SymbolEntity::new(SymbolKind::Type, "src/a.rs", "Point");
        let b = SymbolEntity::new(SymbolKind::Type, "src/b.rs", "Point");
        assert_ne!(a.id, b.id, "distinct files must produce distinct ids");

        let (graph, _) = Graph::build(vec![], vec![a.clone(), b.clone()], vec![]);
        let engine = QueryEngine::new(&graph);

        let found = engine.symbols_named("Point");
        assert_eq!(found.len(), 2);

        assert_eq!(engine.symbol_by_id(&a.id), Some(&a));
    }

    #[test]
    fn definition_of_follows_the_defined_in_edge() {
        let module = syntax(SyntaxKind::Module, "src/lib.rs");
        let point_symbol = symbol(SymbolKind::Type, "Point");
        let edge = Edge::new(
            point_symbol.id.clone(),
            module.id.clone(),
            EdgeKind::DefinedIn,
        );

        let (graph, _) = Graph::build(vec![module.clone()], vec![point_symbol.clone()], vec![edge]);
        let engine = QueryEngine::new(&graph);

        assert_eq!(engine.definition_of(&point_symbol.id), Some(&module));
    }

    #[test]
    fn a_symbol_with_no_defined_in_edge_has_no_definition() {
        let placeholder = symbol(SymbolKind::Symbol, "external-crate:serde");
        let (graph, _) = Graph::build(vec![], vec![placeholder.clone()], vec![]);
        let engine = QueryEngine::new(&graph);

        assert_eq!(engine.definition_of(&placeholder.id), None);
    }

    #[test]
    fn callers_and_callees_reflect_a_call_chain() {
        let a = symbol(SymbolKind::Function, "a");
        let b = symbol(SymbolKind::Function, "b");
        let c = symbol(SymbolKind::Function, "c");
        let edges = vec![
            Edge::new(a.id.clone(), b.id.clone(), EdgeKind::Calls),
            Edge::new(b.id.clone(), c.id.clone(), EdgeKind::Calls),
        ];

        let (graph, _) = Graph::build(vec![], vec![a.clone(), b.clone(), c.clone()], edges);
        let engine = QueryEngine::new(&graph);

        assert_eq!(engine.callers_of(&b.id), vec![&a]);
        assert_eq!(engine.callees_of(&b.id), vec![&c]);
    }

    #[test]
    fn dependencies_and_dependents_are_transitive_across_a_three_file_chain() {
        let a = symbol(SymbolKind::Namespace, "a");
        let b = symbol(SymbolKind::Namespace, "b");
        let c = symbol(SymbolKind::Namespace, "c");
        let edges = vec![
            Edge::new(a.id.clone(), b.id.clone(), EdgeKind::Imports),
            Edge::new(b.id.clone(), c.id.clone(), EdgeKind::Imports),
        ];

        let (graph, _) = Graph::build(vec![], vec![a.clone(), b.clone(), c.clone()], edges);
        let engine = QueryEngine::new(&graph);

        let deps_of_a = engine.dependencies_of(&a.id);
        assert!(deps_of_a.contains(&b.id));
        assert!(deps_of_a.contains(&c.id));

        let dependents_of_c = engine.dependents_of(&c.id);
        assert!(dependents_of_c.contains(&a.id));
        assert!(dependents_of_c.contains(&b.id));
    }

    #[test]
    fn implementers_of_finds_every_implementing_symbol_but_not_unrelated_ones() {
        let shape = symbol(SymbolKind::Type, "Shape");
        let circle = symbol(SymbolKind::Type, "Circle");
        let square = symbol(SymbolKind::Type, "Square");
        let unrelated_trait = symbol(SymbolKind::Type, "Debug");
        let triangle = symbol(SymbolKind::Type, "Triangle");
        let edges = vec![
            Edge::new(circle.id.clone(), shape.id.clone(), EdgeKind::Implements),
            Edge::new(square.id.clone(), shape.id.clone(), EdgeKind::Implements),
            Edge::new(
                triangle.id.clone(),
                unrelated_trait.id.clone(),
                EdgeKind::Implements,
            ),
        ];

        let (graph, _) = Graph::build(
            vec![],
            vec![
                shape.clone(),
                circle.clone(),
                square.clone(),
                unrelated_trait,
                triangle,
            ],
            edges,
        );
        let engine = QueryEngine::new(&graph);

        let mut implementers = engine.implementers_of(&shape.id);
        implementers.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(implementers, vec![&circle, &square]);
    }

    #[test]
    fn references_to_finds_every_referencing_symbol_and_is_empty_when_unreferenced() {
        let target = symbol(SymbolKind::Symbol, "count");
        let f1 = symbol(SymbolKind::Function, "f1");
        let f2 = symbol(SymbolKind::Function, "f2");
        let f3 = symbol(SymbolKind::Function, "f3");
        let edges = vec![
            Edge::new(f1.id.clone(), target.id.clone(), EdgeKind::References),
            Edge::new(f2.id.clone(), target.id.clone(), EdgeKind::References),
            Edge::new(f3.id.clone(), target.id.clone(), EdgeKind::References),
        ];

        let (graph, _) = Graph::build(vec![], vec![target.clone(), f1, f2, f3], edges);
        let engine = QueryEngine::new(&graph);

        assert_eq!(engine.references_to(&target.id).len(), 3);

        let unreferenced = symbol(SymbolKind::Symbol, "unused");
        let (graph2, _) = Graph::build(vec![], vec![unreferenced.clone()], vec![]);
        let engine2 = QueryEngine::new(&graph2);
        assert!(engine2.references_to(&unreferenced.id).is_empty());
    }

    #[test]
    fn impact_of_reports_every_caller_up_a_three_deep_chain_and_none_for_a_leaf() {
        let a = symbol(SymbolKind::Function, "a");
        let b = symbol(SymbolKind::Function, "b");
        let c = symbol(SymbolKind::Function, "c");
        let d = symbol(SymbolKind::Function, "d");
        // a -> b -> c -> d (a calls b calls c calls d)
        let edges = vec![
            Edge::new(a.id.clone(), b.id.clone(), EdgeKind::Calls),
            Edge::new(b.id.clone(), c.id.clone(), EdgeKind::Calls),
            Edge::new(c.id.clone(), d.id.clone(), EdgeKind::Calls),
        ];

        let (graph, _) = Graph::build(
            vec![],
            vec![a.clone(), b.clone(), c.clone(), d.clone()],
            edges,
        );
        let engine = QueryEngine::new(&graph);

        // Changing d impacts c, b, and a (the whole chain up).
        let impact = engine.impact_of(&d.id);
        assert!(impact.contains(&c.id));
        assert!(impact.contains(&b.id));
        assert!(impact.contains(&a.id));

        assert!(engine.impact_of(&a.id).is_empty());
    }

    #[test]
    fn entry_points_finds_main_and_module_tree_matches_nesting() {
        let file_module = syntax(SyntaxKind::Module, "src/lib.rs");
        let geometry = SyntaxEntity::new(SyntaxKind::Module, "src/lib.rs::geometry", "geometry");
        let main_fn = SyntaxEntity::new(SyntaxKind::Function, "src/lib.rs", "main");
        let edges = vec![
            Edge::new(
                file_module.id.clone(),
                geometry.id.clone(),
                EdgeKind::Contains,
            ),
            Edge::new(
                file_module.id.clone(),
                main_fn.id.clone(),
                EdgeKind::Contains,
            ),
        ];

        let (graph, _) = Graph::build(
            vec![file_module.clone(), geometry.clone(), main_fn.clone()],
            vec![],
            edges,
        );
        let engine = QueryEngine::new(&graph);

        assert_eq!(engine.entry_points(), vec![&main_fn]);

        let tree = engine.module_tree();
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].module, file_module);
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].module, geometry);
    }
}
