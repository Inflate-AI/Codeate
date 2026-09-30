use std::collections::HashMap;

use crate::model::{Edge, EdgeKind, EntityId, SyntaxEntity, SyntaxKind};

/// A single lexical scope: the entity that owns it (a module, function, or
/// method), its own name bindings, and a link to its parent scope.
struct Scope {
    #[allow(dead_code)]
    owner: EntityId,
    parent: Option<usize>,
    /// Name -> bindings declared in this scope, in declaration order. The
    /// last entry is the most recently declared (used for same-scope
    /// redeclaration lookup in task 5).
    bindings: HashMap<String, Vec<EntityId>>,
}

/// The set of nested lexical scopes built from a `SyntaxEntity`/`Edge`
/// `CONTAINS` tree. `Module`, `Function`, and `Method` entities each open a
/// new scope; every other contained entity is a name binding in its
/// immediately enclosing scope.
pub struct ScopeTree {
    scopes: Vec<Scope>,
    root: usize,
}

impl ScopeTree {
    /// Build a scope tree rooted at `root_entity`, using `entities`/`edges`
    /// (typically a `LanguageAdapter::extract` result) to find each scope's
    /// contained declarations.
    pub fn build(
        root_entity: &SyntaxEntity,
        entities: &[SyntaxEntity],
        edges: &[Edge],
    ) -> ScopeTree {
        let mut tree = ScopeTree {
            scopes: Vec::new(),
            root: 0,
        };
        let root_idx = tree.push_scope(root_entity.id.clone(), None);
        tree.root = root_idx;
        tree.populate(root_idx, root_entity, entities, edges);
        tree
    }

    fn push_scope(&mut self, owner: EntityId, parent: Option<usize>) -> usize {
        self.scopes.push(Scope {
            owner,
            parent,
            bindings: HashMap::new(),
        });
        self.scopes.len() - 1
    }

    fn populate(
        &mut self,
        scope_idx: usize,
        owner_entity: &SyntaxEntity,
        entities: &[SyntaxEntity],
        edges: &[Edge],
    ) {
        for edge in edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Contains && e.from == owner_entity.id)
        {
            let Some(child) = entities.iter().find(|e| e.id == edge.to) else {
                continue;
            };

            match child.kind {
                SyntaxKind::Module | SyntaxKind::Function | SyntaxKind::Method => {
                    self.bind(scope_idx, &child.name, child.id.clone());
                    let child_idx = self.push_scope(child.id.clone(), Some(scope_idx));
                    self.populate(child_idx, child, entities, edges);
                }
                SyntaxKind::Class
                | SyntaxKind::Struct
                | SyntaxKind::Enum
                | SyntaxKind::Interface => {
                    self.bind(scope_idx, &child.name, child.id.clone());
                    // Not a lexical scope boundary itself (only Module/
                    // Function/Method are) -- recurse in the SAME scope so
                    // a nested method (Python classes nest their methods
                    // directly in the AST, unlike Rust's separate `impl`
                    // blocks, which are already children of the enclosing
                    // module) still gets its own scope registered, and
                    // resolves names against the enclosing scope rather
                    // than a nonexistent "class scope" (matching real
                    // Python semantics: a method body's name lookup skips
                    // right past its own class's body).
                    self.populate(scope_idx, child, entities, edges);
                }
                SyntaxKind::Variable | SyntaxKind::Parameter => {
                    self.bind(scope_idx, &child.name, child.id.clone());
                }
                // Phase 14's repository-level kinds never appear in an
                // adapter's AST-derived CONTAINS tree (manifest.rs builds
                // their own entities/edges directly), so they bind nothing
                // here, same as Package/Statement/Expression.
                SyntaxKind::Package
                | SyntaxKind::Statement
                | SyntaxKind::Expression
                | SyntaxKind::Dependency
                | SyntaxKind::BuildTarget
                | SyntaxKind::Configuration
                | SyntaxKind::Test
                | SyntaxKind::EntryPoint
                | SyntaxKind::ApiEndpoint
                | SyntaxKind::GeneratedCode => {}
            }
        }
    }

    fn bind(&mut self, scope_idx: usize, name: &str, id: EntityId) {
        self.scopes[scope_idx]
            .bindings
            .entry(name.to_string())
            .or_default()
            .push(id);
    }

    pub fn root(&self) -> usize {
        self.root
    }

    /// The entity that owns `scope`.
    pub fn owner(&self, scope: usize) -> &EntityId {
        &self.scopes[scope].owner
    }

    /// The scope opened by `entity_id`, if it opened one (i.e. it was a
    /// `Module`/`Function`/`Method`).
    pub fn scope_of(&self, entity_id: &EntityId) -> Option<usize> {
        self.scopes.iter().position(|s| &s.owner == entity_id)
    }

    /// Resolve `name` starting at `scope`, walking up through parent scopes
    /// until a binding is found -- an inner scope's binding shadows an
    /// outer scope's same-named binding. Returns the most recently declared
    /// binding for `name` within the first scope where it's found.
    pub fn lookup(&self, scope: usize, name: &str) -> Option<&EntityId> {
        let current = &self.scopes[scope];
        if let Some(bound) = current.bindings.get(name).and_then(|ids| ids.last()) {
            return Some(bound);
        }
        current.parent.and_then(|parent| self.lookup(parent, name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declare(parent: &SyntaxEntity, entity: SyntaxEntity, edges: &mut Vec<Edge>) -> SyntaxEntity {
        edges.push(Edge::new(
            parent.id.clone(),
            entity.id.clone(),
            EdgeKind::Contains,
        ));
        entity
    }

    #[test]
    fn looks_up_a_binding_declared_directly_in_the_root_scope() {
        let module = SyntaxEntity::new(SyntaxKind::Module, "src/lib.rs", "src/lib.rs");
        let mut edges = Vec::new();
        let point = declare(
            &module,
            SyntaxEntity::new(SyntaxKind::Struct, "src/lib.rs", "Point"),
            &mut edges,
        );
        let entities = vec![module.clone(), point.clone()];

        let tree = ScopeTree::build(&module, &entities, &edges);

        assert_eq!(tree.lookup(tree.root(), "Point"), Some(&point.id));
        assert_eq!(tree.lookup(tree.root(), "Missing"), None);
    }

    #[test]
    fn a_nested_function_scope_can_see_its_own_bindings() {
        let module = SyntaxEntity::new(SyntaxKind::Module, "src/lib.rs", "src/lib.rs");
        let mut edges = Vec::new();
        let run = declare(
            &module,
            SyntaxEntity::new(SyntaxKind::Function, "src/lib.rs", "run"),
            &mut edges,
        );
        let count = declare(
            &run,
            SyntaxEntity::new(SyntaxKind::Variable, "src/lib.rs::run", "count"),
            &mut edges,
        );
        let entities = vec![module.clone(), run.clone(), count.clone()];

        let tree = ScopeTree::build(&module, &entities, &edges);
        let run_scope = tree.scope_of(&run.id).expect("run opens a scope");

        assert_eq!(tree.lookup(run_scope, "count"), Some(&count.id));
        // Not visible from the module (outer) scope.
        assert_eq!(tree.lookup(tree.root(), "count"), None);
    }

    #[test]
    fn an_inner_scope_binding_shadows_an_outer_scope_binding_of_the_same_name() {
        let module = SyntaxEntity::new(SyntaxKind::Module, "src/lib.rs", "src/lib.rs");
        let mut edges = Vec::new();
        let outer_value = declare(
            &module,
            SyntaxEntity::new(SyntaxKind::Struct, "src/lib.rs", "Value"),
            &mut edges,
        );
        let run = declare(
            &module,
            SyntaxEntity::new(SyntaxKind::Function, "src/lib.rs", "run"),
            &mut edges,
        );
        let inner_value = declare(
            &run,
            SyntaxEntity::new(SyntaxKind::Variable, "src/lib.rs::run", "Value"),
            &mut edges,
        );
        let entities = vec![
            module.clone(),
            outer_value.clone(),
            run.clone(),
            inner_value.clone(),
        ];

        let tree = ScopeTree::build(&module, &entities, &edges);
        let run_scope = tree.scope_of(&run.id).expect("run opens a scope");

        assert_eq!(tree.lookup(run_scope, "Value"), Some(&inner_value.id));
        assert_eq!(tree.lookup(tree.root(), "Value"), Some(&outer_value.id));
    }

    /// A method nested directly under a `Class` in the `CONTAINS` tree
    /// (Python's own shape, unlike Rust's separate `impl` blocks) must
    /// still get its own scope, and that scope's *parent* must be the
    /// enclosing module -- not the class, which isn't a lexical scope
    /// boundary (a method body's name lookup skips right past its own
    /// class, matching real Python semantics). A prior bug here meant
    /// `Class`/`Struct`/`Enum`/`Interface` only bound a name and never
    /// recursed into their own `CONTAINS` children at all, so a nested
    /// method's body was silently never resolved.
    #[test]
    fn a_method_nested_under_a_class_opens_a_scope_whose_parent_is_the_module() {
        let module = SyntaxEntity::new(SyntaxKind::Module, "module.py", "module.py");
        let mut edges = Vec::new();
        let helper = declare(
            &module,
            SyntaxEntity::new(SyntaxKind::Function, "module.py", "helper"),
            &mut edges,
        );
        let circle = declare(
            &module,
            SyntaxEntity::new(SyntaxKind::Class, "module.py", "Circle"),
            &mut edges,
        );
        let area = declare(
            &circle,
            SyntaxEntity::new(SyntaxKind::Method, "module.py::Circle", "area"),
            &mut edges,
        );
        let entities = vec![module.clone(), helper.clone(), circle.clone(), area.clone()];

        let tree = ScopeTree::build(&module, &entities, &edges);

        let area_scope = tree
            .scope_of(&area.id)
            .expect("area gets its own scope despite being nested under a Class");
        // helper isn't bound in area's own scope, but is visible by walking
        // up to the module (root) scope -- proving area_scope's parent is
        // the module, not some intermediate "class scope".
        assert_eq!(tree.lookup(area_scope, "helper"), Some(&helper.id));
    }
}
