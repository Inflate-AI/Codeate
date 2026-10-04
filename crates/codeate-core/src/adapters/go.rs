use std::collections::HashSet;

use tree_sitter::{Node, Parser, Tree};

use crate::adapter::LanguageAdapter;
use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};
use crate::resolve::unresolved_symbol;
use crate::symbols::{ScopeTree, SymbolTable};
use crate::types::TypeRef;

/// Go's predeclared basic types (https://go.dev/ref/spec#Types), treated as
/// `TypeRef::Primitive` the same way Rust's `i32`/`bool`/... are. `error`
/// and `any` are technically interface types, not basic types, but for
/// `extract_type`'s purposes (matching the Rust/Python adapters' own
/// primitive lists) they're common enough return/parameter types to classify
/// explicitly rather than falling through to `Named`.
const PRIMITIVE_TYPES: &[&str] = &[
    "bool",
    "string",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
    "byte",
    "rune",
    "float32",
    "float64",
    "complex64",
    "complex128",
    "error",
    "any",
];

/// The parsed form a `GoAdapter` produces: `tree-sitter`'s `Tree` paired with
/// the source text it was parsed from. Unlike `syn`/`rustpython-parser`,
/// `tree-sitter` nodes are spans into the original source rather than a
/// self-contained typed tree, so every node text lookup needs the source
/// bytes alongside the tree -- every later `tree-sitter`-based adapter in
/// Phase 17's queue needs this same pairing, not just Go's.
pub struct GoAst {
    tree: Tree,
    source: String,
}

/// The Go language adapter: parses Go source via `tree-sitter`/
/// `tree-sitter-go` and maps it onto the canonical syntax model. The first
/// `tree-sitter`-based adapter in this codebase (Rust used `syn`, Python
/// used `rustpython-parser`, both of which hand back typed, self-contained
/// ASTs) -- the rest of Phase 17's queue reuses this shape.
pub struct GoAdapter;

impl LanguageAdapter for GoAdapter {
    type Ast = GoAst;

    fn parse(&self, source: &str) -> Result<Self::Ast> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .map_err(|err| Error::Message(format!("failed to load Go grammar: {err}")))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::Message("failed to parse Go source".to_string()))?;
        Ok(GoAst {
            tree,
            source: source.to_string(),
        })
    }

    fn extract(&self, ast: &Self::Ast, location: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
        let mut entities = Vec::new();
        let mut edges = Vec::new();

        let file_module = SyntaxEntity::new(SyntaxKind::Module, location, location);
        entities.push(file_module.clone());

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &file_module,
            location,
            &mut entities,
            &mut edges,
            &mut sites,
        );

        (entities, edges)
    }

    fn resolve_file(
        &self,
        ast: &Self::Ast,
        location: &str,
        all_entities: &[SyntaxEntity],
        symbol_table: &SymbolTable,
        edges_so_far: &[Edge],
        module_symbol: &SymbolEntity,
        file_module: &SyntaxEntity,
    ) -> (Vec<SymbolEntity>, Vec<Edge>) {
        // Re-derive this file's own entities/edges/sites in one pass, so the
        // `SyntaxEntity`s `sites.bodies` carries are the exact same
        // (kind/scope/name, hence same `EntityId`) ones `extract` already
        // handed the pipeline -- `symbol_table.find_by_syntax_id` below
        // depends on that match.
        let mut file_entities = vec![file_module.clone()];
        let mut file_edges = Vec::new();
        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            file_module,
            location,
            &mut file_entities,
            &mut file_edges,
            &mut sites,
        );
        let scope_tree = ScopeTree::build(file_module, &file_entities, edges_so_far);

        let mut symbols = Vec::new();
        let mut edges = Vec::new();

        for (fn_entity, body) in &sites.bodies {
            let Some(fn_symbol) = symbol_table.find_by_syntax_id(&fn_entity.id).cloned() else {
                continue;
            };
            let Some(scope) = scope_tree.scope_of(&fn_entity.id) else {
                continue;
            };
            let (body_symbols, body_edges) = resolve_body(
                *body,
                &ast.source,
                &fn_symbol,
                &scope_tree,
                scope,
                symbol_table,
            );
            symbols.extend(body_symbols);
            edges.extend(body_edges);
        }

        edges.extend(resolve_implements(
            &file_entities,
            all_entities,
            edges_so_far,
            symbol_table,
        ));

        let (import_symbols, import_edges) =
            resolve_imports(&sites.imports, &ast.source, module_symbol);
        symbols.extend(import_symbols);
        edges.extend(import_edges);

        edges.extend(resolve_exports(
            &file_entities,
            &file_edges,
            module_symbol,
            symbol_table,
        ));

        (symbols, edges)
    }
}

/// Sites `extract_items` collects that `resolve_file` needs the raw tree for
/// but that have no place in `extract`'s plain `(Vec<SyntaxEntity>,
/// Vec<Edge>)`: a function/method's own entity paired with its body block,
/// and every `import_spec`. Mirrors the Rust/Python adapters' own
/// `ResolutionSites`.
#[derive(Default)]
struct ResolutionSites<'a> {
    bodies: Vec<(SyntaxEntity, Node<'a>)>,
    imports: Vec<Node<'a>>,
}

fn text<'a>(node: Node<'_>, src: &'a str) -> &'a str {
    node.utf8_text(src.as_bytes()).unwrap_or("")
}

fn child_scope(scope: &str, name: &str) -> String {
    format!("{scope}::{name}")
}

/// Record `parent CONTAINS child` and `parent DECLARES child`, then push
/// `child` into `entities`.
fn declare(
    parent: &SyntaxEntity,
    child: SyntaxEntity,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    edges.push(Edge::new(
        parent.id.clone(),
        child.id.clone(),
        EdgeKind::Contains,
    ));
    edges.push(Edge::new(
        parent.id.clone(),
        child.id.clone(),
        EdgeKind::Declares,
    ));
    entities.push(child);
}

/// Record `parent CONTAINS child` only (parameters, variables, statements),
/// then push `child` into `entities`.
fn contain(
    parent: &SyntaxEntity,
    child: SyntaxEntity,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    edges.push(Edge::new(
        parent.id.clone(),
        child.id.clone(),
        EdgeKind::Contains,
    ));
    entities.push(child);
}

/// Walk a `source_file`'s direct children. Two passes: type declarations
/// (`struct`/`interface`) are extracted first, so a `method_declaration`
/// appearing earlier in the file than its receiver's `type_declaration` (Go
/// has no declaration-order requirement, unlike a strictly top-down walk)
/// still finds its owning `Struct` entity already present. Also populates
/// `sites` (function/method bodies, import specs) for `resolve_file`, in the
/// same walk that builds `entities`/`edges` -- so the entities `sites`
/// carries are identical (by `EntityId`) to the ones this walk pushes into
/// `entities`, not a separately reconstructed guess.
fn extract_items<'a>(
    root: Node<'a>,
    src: &'a str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "type_declaration" {
            extract_type_declaration(child, src, parent, scope, entities, edges);
        }
    }

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                extract_function(child, src, parent, scope, entities, edges, sites)
            }
            "method_declaration" => {
                extract_method(child, src, parent, scope, entities, edges, sites)
            }
            "var_declaration" | "const_declaration" => {
                for name_node in spec_names(child) {
                    let entity =
                        SyntaxEntity::new(SyntaxKind::Variable, scope, text(name_node, src));
                    contain(parent, entity, entities, edges);
                }
            }
            "import_declaration" => {
                let mut import_cursor = child.walk();
                for descendant in child.children(&mut import_cursor) {
                    collect_import_specs(descendant, &mut sites.imports);
                }
            }
            _ => {}
        }
    }
}

/// A `type_declaration` (`type Foo struct {...}`, or a parenthesized group
/// of several `type_spec`s) becomes a `Struct` entity for a struct type, or
/// an `Interface` entity (with its method signatures declared as `Method`
/// entities under it, mirroring the Rust adapter's own trait-method
/// declaration) for an interface type. A defined type over anything else
/// (`type MyInt int`, `type Handler func()`) has no clean mapping onto the
/// canonical model's Struct/Interface/Enum shape -- a documented gap, not
/// guessed at, same posture as the Rust/Python adapters' own item-vs-body
/// gaps.
fn extract_type_declaration(
    node: Node,
    src: &str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = node.walk();
    for spec in node.children(&mut cursor) {
        if spec.kind() != "type_spec" {
            continue;
        }
        let Some(name_node) = spec.child_by_field_name("name") else {
            continue;
        };
        let name = text(name_node, src);
        let Some(type_node) = spec.child_by_field_name("type") else {
            continue;
        };

        match type_node.kind() {
            "struct_type" => {
                let entity = SyntaxEntity::new(SyntaxKind::Struct, scope, name);
                declare(parent, entity, entities, edges);
                // Struct fields aren't extracted as entities -- the same
                // documented, not-yet-wired-in gap as the Rust/Python
                // adapters' own `extract_struct_fields`.
            }
            "interface_type" => {
                let entity = SyntaxEntity::new(SyntaxKind::Interface, scope, name);
                let iface_scope = child_scope(scope, name);
                declare(parent, entity.clone(), entities, edges);

                let mut member_cursor = type_node.walk();
                for member in type_node.children(&mut member_cursor) {
                    if member.kind() != "method_elem" {
                        continue;
                    }
                    let Some(method_name) = member.child_by_field_name("name") else {
                        continue;
                    };
                    let method_entity =
                        SyntaxEntity::new(SyntaxKind::Method, &iface_scope, text(method_name, src));
                    declare(&entity, method_entity, entities, edges);
                }
            }
            _ => {}
        }
    }
}

/// A top-level function declaration: its own entity, parameters, and body.
fn extract_function<'a>(
    node: Node<'a>,
    src: &'a str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let name = text(name_node, src);
    let entity = SyntaxEntity::new(SyntaxKind::Function, scope, name);
    let fn_scope = child_scope(scope, name);
    declare(parent, entity.clone(), entities, edges);

    if let Some(params) = node.child_by_field_name("parameters") {
        extract_parameters(params, src, &entity, &fn_scope, entities, edges);
    }
    if let Some(body) = node.child_by_field_name("body") {
        extract_body(body, src, &entity, &fn_scope, entities, edges);
        sites.bodies.push((entity, body));
    }
}

/// A method declaration (`func (c *Circle) Area() float64 {...}`): parented
/// under its receiver's `Struct` entity when one was already extracted in
/// this same file (see `extract_items`'s two-pass ordering), matching the
/// Python adapter's own nesting of methods directly under their class. A
/// receiver type declared in a different file of the same package (Go
/// allows this; Codeate doesn't yet resolve types across files at extract
/// time) falls back to being declared directly under the file module -- a
/// documented gap, not guessed at.
fn extract_method<'a>(
    node: Node<'a>,
    src: &'a str,
    module: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let name = text(name_node, src);

    let receiver_type_name = node
        .child_by_field_name("receiver")
        .and_then(|receiver_list| first_parameter_declaration(receiver_list))
        .and_then(|param| param.child_by_field_name("type"))
        .map(|type_node| receiver_type_name(type_node, src));

    let owning_struct = receiver_type_name
        .as_deref()
        .and_then(|rt| {
            entities
                .iter()
                .find(|e| e.kind == SyntaxKind::Struct && e.name == rt)
        })
        .cloned();

    let (owner, owner_scope) = match owning_struct {
        Some(struct_entity) => {
            let s = child_scope(scope, &struct_entity.name);
            (struct_entity, s)
        }
        None => (module.clone(), scope.to_string()),
    };

    let entity = SyntaxEntity::new(SyntaxKind::Method, &owner_scope, name);
    let method_scope = child_scope(&owner_scope, name);
    declare(&owner, entity.clone(), entities, edges);

    if let Some(params) = node.child_by_field_name("parameters") {
        extract_parameters(params, src, &entity, &method_scope, entities, edges);
    }
    if let Some(body) = node.child_by_field_name("body") {
        extract_body(body, src, &entity, &method_scope, entities, edges);
        sites.bodies.push((entity, body));
    }
}

fn first_parameter_declaration(list: Node) -> Option<Node> {
    let mut cursor = list.walk();
    let found = list
        .children(&mut cursor)
        .find(|c| c.kind() == "parameter_declaration");
    found
}

/// A receiver's declared type name, unwrapping `*T` (`pointer_type`) to `T`
/// the same as a plain value receiver `T`.
fn receiver_type_name(node: Node, src: &str) -> String {
    match node.kind() {
        "pointer_type" => node
            .named_child(0)
            .map(|c| text(c, src).to_string())
            .unwrap_or_default(),
        _ => text(node, src).to_string(),
    }
}

/// A `parameter_list`'s `parameter_declaration`s become `Parameter`
/// entities. A single declaration can bind several comma-joined names to one
/// type (`func f(a, b int)`), exposed by `tree-sitter-go` as repeated
/// `"name"` fields on the same node -- each becomes its own `Parameter`. An
/// unnamed parameter (common in interface method signatures and bare return
/// types) binds nothing -- a documented gap, not guessed at.
fn extract_parameters(
    params: Node,
    src: &str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = params.walk();
    for param in params.children(&mut cursor) {
        if param.kind() != "parameter_declaration"
            && param.kind() != "variadic_parameter_declaration"
        {
            continue;
        }
        let mut name_cursor = param.walk();
        for name_node in param.children_by_field_name("name", &mut name_cursor) {
            let entity = SyntaxEntity::new(SyntaxKind::Parameter, scope, text(name_node, src));
            contain(parent, entity, entities, edges);
        }
    }
}

/// Walk a function/method body's direct statements (its `block`'s
/// `statement_list`, not nested into deeper control flow, matching the
/// Rust/Python adapters' own scope): a short/`var`/`const` declaration
/// becomes `Variable` entities; everything else becomes a generic
/// `Statement` entity. A nested `func`/`type` declaration inside a body
/// isn't extracted -- a documented gap, same as the other adapters not
/// walking into nested item definitions.
fn extract_body(
    block: Node,
    src: &str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    let Some(stmt_list) = block
        .named_child(0)
        .filter(|n| n.kind() == "statement_list")
    else {
        return;
    };

    let mut cursor = stmt_list.walk();
    for (index, stmt) in stmt_list.children(&mut cursor).enumerate() {
        match stmt.kind() {
            "short_var_declaration" => {
                if let Some(left) = stmt.child_by_field_name("left") {
                    let mut left_cursor = left.walk();
                    for name_node in left.named_children(&mut left_cursor) {
                        if name_node.kind() != "identifier" || text(name_node, src) == "_" {
                            continue;
                        }
                        let entity = SyntaxEntity::new(
                            SyntaxKind::Variable,
                            &format!("{scope}#{index}"),
                            text(name_node, src),
                        );
                        contain(parent, entity, entities, edges);
                    }
                }
            }
            "var_declaration" | "const_declaration" => {
                for name_node in spec_names(stmt) {
                    let entity = SyntaxEntity::new(
                        SyntaxKind::Variable,
                        &format!("{scope}#{index}"),
                        text(name_node, src),
                    );
                    contain(parent, entity, entities, edges);
                }
            }
            "function_declaration" | "method_declaration" | "type_declaration" => {}
            _ => {
                let entity =
                    SyntaxEntity::new(SyntaxKind::Statement, scope, format!("stmt_{index}"));
                contain(parent, entity, entities, edges);
            }
        }
    }
}

/// Every `"name"` field bound by any `var_spec`/`const_spec` directly or
/// indirectly (parenthesized-group form: `var (\n a = 1\n b = 2\n)`) under
/// `node`.
fn spec_names(node: Node) -> Vec<Node> {
    let mut names = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "var_spec" | "const_spec" => {
                let mut name_cursor = child.walk();
                names.extend(child.children_by_field_name("name", &mut name_cursor));
            }
            "var_spec_list" | "const_spec_list" => names.extend(spec_names(child)),
            _ => {}
        }
    }
    names
}

/// Recursively collect every `import_spec` under `node` (an
/// `import_declaration`'s direct child: either a single `import_spec`, or an
/// `import_spec_list` for the parenthesized `import (...)` group form).
fn collect_import_specs<'a>(node: Node<'a>, imports: &mut Vec<Node<'a>>) {
    match node.kind() {
        "import_spec" => imports.push(node),
        "import_spec_list" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_import_specs(child, imports);
            }
        }
        _ => {}
    }
}

/// `REFERENCES`/`CALLS`/`INSTANTIATES` edges from one function/method body.
/// A bare identifier read resolves via `scope_tree`; a bare-name call
/// (`helper()`) resolves the same way; a method call (`obj.Method()`)
/// resolves by best-effort name match against any `Method` symbol in the
/// file (full receiver-type dispatch needs cross-file struct/interface
/// knowledge, out of scope here, same as the Rust/Python adapters' own
/// method-call resolution); a composite literal (`Circle{}`) whose type
/// resolves in scope to a `Type` symbol is an `INSTANTIATES` site. Anything
/// unresolved gets an explicit placeholder symbol, never silently dropped.
fn resolve_body<'a>(
    body: Node<'a>,
    src: &str,
    fn_symbol: &SymbolEntity,
    scope_tree: &ScopeTree,
    scope: usize,
    symbol_table: &SymbolTable,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut names: Vec<Node<'a>> = Vec::new();
    let mut calls: Vec<Node<'a>> = Vec::new();
    let mut composites: Vec<Node<'a>> = Vec::new();

    walk_block_statements(
        body,
        &mut |n| names.push(n),
        &mut |n| calls.push(n),
        &mut |n| composites.push(n),
    );

    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for name_node in &names {
        let ident = text(*name_node, src);
        let resolved = scope_tree
            .lookup(scope, ident)
            .and_then(|id| symbol_table.find_by_syntax_id(id))
            .cloned();
        let target = resolved.unwrap_or_else(|| {
            let placeholder = unresolved_symbol("unknown-local", ident);
            symbols.push(placeholder.clone());
            placeholder
        });
        edges.push(Edge::new(
            fn_symbol.id.clone(),
            target.id.clone(),
            EdgeKind::References,
        ));
    }

    for call_node in &calls {
        let Some(func) = call_node.child_by_field_name("function") else {
            continue;
        };
        match func.kind() {
            "identifier" => {
                let ident = text(func, src);
                let resolved = scope_tree
                    .lookup(scope, ident)
                    .and_then(|id| symbol_table.find_by_syntax_id(id))
                    .cloned()
                    .or_else(|| {
                        // The scope tree only covers this file's own
                        // entities, so a call to a function declared in
                        // another file of the same package never resolves
                        // through it -- fall back to a repo-wide
                        // best-effort name match against `symbol_table`,
                        // mirroring the `selector_expression` branch below,
                        // which has always searched repo-wide.
                        symbol_table
                            .symbols()
                            .find(|s| s.kind == SymbolKind::Function && s.name == ident)
                            .cloned()
                    });
                let target = resolved.unwrap_or_else(|| {
                    let placeholder = unresolved_symbol("unknown-local", ident);
                    symbols.push(placeholder.clone());
                    placeholder
                });
                edges.push(Edge::new(
                    fn_symbol.id.clone(),
                    target.id.clone(),
                    EdgeKind::Calls,
                ));
            }
            "selector_expression" => {
                let Some(field) = func.child_by_field_name("field") else {
                    continue;
                };
                let ident = text(field, src);
                let matched = symbol_table
                    .symbols()
                    .find(|s| s.kind == SymbolKind::Method && s.name == ident)
                    .cloned();
                let target = matched.unwrap_or_else(|| {
                    let placeholder = unresolved_symbol("unknown-method", ident);
                    symbols.push(placeholder.clone());
                    placeholder
                });
                edges.push(Edge::new(
                    fn_symbol.id.clone(),
                    target.id.clone(),
                    EdgeKind::Calls,
                ));
            }
            other => {
                let placeholder = unresolved_symbol("unsupported-form", &format!("{other:?}"));
                symbols.push(placeholder.clone());
                edges.push(Edge::new(
                    fn_symbol.id.clone(),
                    placeholder.id.clone(),
                    EdgeKind::Calls,
                ));
            }
        }
    }

    for composite_node in &composites {
        let Some(type_node) = composite_node.child_by_field_name("type") else {
            continue;
        };
        if type_node.kind() != "type_identifier" {
            continue;
        }
        let ident = text(type_node, src);
        if let Some(symbol) = scope_tree
            .lookup(scope, ident)
            .and_then(|id| symbol_table.find_by_syntax_id(id))
        {
            if symbol.kind == SymbolKind::Type {
                edges.push(Edge::new(
                    fn_symbol.id.clone(),
                    symbol.id.clone(),
                    EdgeKind::Instantiates,
                ));
            }
        }
    }

    (symbols, edges)
}

fn walk_expr_sites<'a>(
    node: Node<'a>,
    on_name: &mut impl FnMut(Node<'a>),
    on_call: &mut impl FnMut(Node<'a>),
    on_composite: &mut impl FnMut(Node<'a>),
) {
    match node.kind() {
        "identifier" => on_name(node),
        "call_expression" => {
            on_call(node);
            if let Some(func) = node.child_by_field_name("function") {
                walk_expr_sites(func, on_name, on_call, on_composite);
            }
            if let Some(args) = node.child_by_field_name("arguments") {
                let mut cursor = args.walk();
                for arg in args.named_children(&mut cursor) {
                    walk_expr_sites(arg, on_name, on_call, on_composite);
                }
            }
        }
        "selector_expression" => {
            if let Some(operand) = node.child_by_field_name("operand") {
                walk_expr_sites(operand, on_name, on_call, on_composite);
            }
        }
        "composite_literal" => on_composite(node),
        "binary_expression" => {
            if let Some(l) = node.child_by_field_name("left") {
                walk_expr_sites(l, on_name, on_call, on_composite);
            }
            if let Some(r) = node.child_by_field_name("right") {
                walk_expr_sites(r, on_name, on_call, on_composite);
            }
        }
        "unary_expression" => {
            if let Some(operand) = node.child_by_field_name("operand") {
                walk_expr_sites(operand, on_name, on_call, on_composite);
            }
        }
        "parenthesized_expression" => {
            if let Some(inner) = node.named_child(0) {
                walk_expr_sites(inner, on_name, on_call, on_composite);
            }
        }
        "index_expression" => {
            if let Some(operand) = node.child_by_field_name("operand") {
                walk_expr_sites(operand, on_name, on_call, on_composite);
            }
            if let Some(index) = node.child_by_field_name("index") {
                walk_expr_sites(index, on_name, on_call, on_composite);
            }
        }
        _ => {}
    }
}

fn walk_stmt_sites<'a>(
    node: Node<'a>,
    on_name: &mut impl FnMut(Node<'a>),
    on_call: &mut impl FnMut(Node<'a>),
    on_composite: &mut impl FnMut(Node<'a>),
) {
    match node.kind() {
        "expression_statement" => {
            if let Some(inner) = node.named_child(0) {
                walk_expr_sites(inner, on_name, on_call, on_composite);
            }
        }
        "return_statement" => {
            if let Some(list) = node.named_child(0) {
                let mut cursor = list.walk();
                for expr in list.named_children(&mut cursor) {
                    walk_expr_sites(expr, on_name, on_call, on_composite);
                }
            }
        }
        "short_var_declaration" | "assignment_statement" => {
            if let Some(right) = node.child_by_field_name("right") {
                let mut cursor = right.walk();
                for expr in right.named_children(&mut cursor) {
                    walk_expr_sites(expr, on_name, on_call, on_composite);
                }
            }
        }
        "if_statement" => {
            if let Some(cond) = node.child_by_field_name("condition") {
                walk_expr_sites(cond, on_name, on_call, on_composite);
            }
            if let Some(consequence) = node.child_by_field_name("consequence") {
                walk_block_statements(consequence, on_name, on_call, on_composite);
            }
            if let Some(alternative) = node.child_by_field_name("alternative") {
                if alternative.kind() == "block" {
                    walk_block_statements(alternative, on_name, on_call, on_composite);
                } else {
                    walk_stmt_sites(alternative, on_name, on_call, on_composite);
                }
            }
        }
        "for_statement" => {
            if let Some(body) = node.child_by_field_name("body") {
                walk_block_statements(body, on_name, on_call, on_composite);
            }
        }
        "block" => walk_block_statements(node, on_name, on_call, on_composite),
        _ => {}
    }
}

fn walk_block_statements<'a>(
    block: Node<'a>,
    on_name: &mut impl FnMut(Node<'a>),
    on_call: &mut impl FnMut(Node<'a>),
    on_composite: &mut impl FnMut(Node<'a>),
) {
    let Some(stmt_list) = block
        .named_child(0)
        .filter(|n| n.kind() == "statement_list")
    else {
        return;
    };
    let mut cursor = stmt_list.walk();
    for stmt in stmt_list.children(&mut cursor) {
        walk_stmt_sites(stmt, on_name, on_call, on_composite);
    }
}

/// `IMPLEMENTS` edges from every `Struct` declared in *this* file to every
/// `Interface` in the repository (`all_entities`, so a cross-file interface
/// is still found) whose full method set the struct's own declared methods
/// are a superset of -- Go has no `implements` keyword; a type satisfies an
/// interface purely by having the right methods. Scoped to this file's own
/// structs (not `all_entities`' structs) so the edge is only produced once,
/// from the file that actually declares the struct, rather than once per
/// file in the repository.
fn resolve_implements(
    file_entities: &[SyntaxEntity],
    all_entities: &[SyntaxEntity],
    edges_so_far: &[Edge],
    symbol_table: &SymbolTable,
) -> Vec<Edge> {
    let mut edges = Vec::new();

    let structs = file_entities
        .iter()
        .filter(|e| e.kind == SyntaxKind::Struct);
    let interfaces: Vec<&SyntaxEntity> = all_entities
        .iter()
        .filter(|e| e.kind == SyntaxKind::Interface)
        .collect();

    for struct_entity in structs {
        let struct_methods: HashSet<&str> = declared_children(edges_so_far, &struct_entity.id)
            .filter_map(|id| all_entities.iter().find(|e| &e.id == id))
            .filter(|e| e.kind == SyntaxKind::Method)
            .map(|e| e.name.as_str())
            .collect();

        for interface_entity in &interfaces {
            let required_methods: HashSet<&str> =
                declared_children(edges_so_far, &interface_entity.id)
                    .filter_map(|id| all_entities.iter().find(|e| &e.id == id))
                    .filter(|e| e.kind == SyntaxKind::Method)
                    .map(|e| e.name.as_str())
                    .collect();

            if required_methods.is_empty() || !required_methods.is_subset(&struct_methods) {
                continue;
            }

            let (Some(struct_symbol), Some(interface_symbol)) = (
                symbol_table.find_by_syntax_id(&struct_entity.id),
                symbol_table.find_by_syntax_id(&interface_entity.id),
            ) else {
                continue;
            };
            edges.push(Edge::new(
                struct_symbol.id.clone(),
                interface_symbol.id.clone(),
                EdgeKind::Implements,
            ));
        }
    }

    edges
}

fn declared_children<'a>(
    edges: &'a [Edge],
    parent: &'a crate::model::EntityId,
) -> impl Iterator<Item = &'a crate::model::EntityId> {
    edges
        .iter()
        .filter(move |e| e.kind == EdgeKind::Declares && &e.from == parent)
        .map(|e| &e.to)
}

/// Resolve a file's `import_spec`s into `IMPORTS` edges from `module_symbol`.
/// Go's import paths (`"fmt"`, `"myrepo/internal/shapes"`) don't carry
/// enough information at this stage to distinguish a standard-library/
/// third-party package from another package within this same repository
/// without resolving them against `go.mod`'s module path -- out of scope for
/// this sub-phase, so every import resolves to an explicit
/// `external-package` placeholder rather than guessed at. A named import
/// alias (`m "math"`) doesn't change this -- the path, not the alias, is
/// what's placeheld.
fn resolve_imports(
    imports: &[Node],
    src: &str,
    module_symbol: &SymbolEntity,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for spec in imports {
        let Some(path_node) = spec.child_by_field_name("path") else {
            continue;
        };
        let path = text(path_node, src).trim_matches('"');
        let placeholder = unresolved_symbol("external-package", path);
        symbols.push(placeholder.clone());
        edges.push(Edge::new(
            module_symbol.id.clone(),
            placeholder.id,
            EdgeKind::Imports,
        ));
    }

    (symbols, edges)
}

/// Resolve a file's top-level exported declarations into `EXPORTS` edges
/// from `module_symbol`: Go's actual public-API convention -- an identifier
/// starting with an uppercase letter is exported, no `pub`/`__all__` keyword
/// or convention to inspect instead.
fn resolve_exports(
    file_entities: &[SyntaxEntity],
    file_edges: &[Edge],
    module_symbol: &SymbolEntity,
    symbol_table: &SymbolTable,
) -> Vec<Edge> {
    const EXPORTABLE_KINDS: [SyntaxKind; 3] = [
        SyntaxKind::Function,
        SyntaxKind::Struct,
        SyntaxKind::Interface,
    ];

    let mut edges = Vec::new();
    for entity_id in declared_children(file_edges, &module_symbol_owner(file_entities)) {
        let Some(entity) = file_entities.iter().find(|e| &e.id == entity_id) else {
            continue;
        };
        if !EXPORTABLE_KINDS.contains(&entity.kind) {
            continue;
        }
        if !entity.name.starts_with(|c: char| c.is_uppercase()) {
            continue;
        }
        if let Some(symbol) = symbol_table.find_by_syntax_id(&entity.id) {
            edges.push(Edge::new(
                module_symbol.id.clone(),
                symbol.id.clone(),
                EdgeKind::Exports,
            ));
        }
    }
    edges
}

fn module_symbol_owner(file_entities: &[SyntaxEntity]) -> crate::model::EntityId {
    file_entities[0].id.clone()
}

/// Map a Go type expression to a `TypeRef`. Not yet wired into
/// `extract`/`resolve_file` -- `ACCEPTS`/`RETURNS` type binding isn't called
/// from the pipeline, mirroring the Rust adapter's own
/// `extract_signature_types`/`extract_struct_fields` gap (tested directly,
/// deliberately not yet wired in either) and the Python adapter's own
/// `extract_type`.
#[allow(dead_code)]
fn extract_type(node: Node, src: &str) -> TypeRef {
    match node.kind() {
        "type_identifier" => {
            let name = text(node, src);
            if PRIMITIVE_TYPES.contains(&name) {
                TypeRef::Primitive(name.to_string())
            } else {
                TypeRef::Named {
                    name: name.to_string(),
                    type_args: Vec::new(),
                }
            }
        }
        "qualified_type" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| text(n, src).to_string())
                .unwrap_or_else(|| text(node, src).to_string());
            TypeRef::Named {
                name,
                type_args: Vec::new(),
            }
        }
        "pointer_type" => {
            let inner = node
                .named_child(0)
                .map(|c| extract_type(c, src))
                .unwrap_or_else(|| TypeRef::Unresolved(text(node, src).to_string()));
            TypeRef::Reference {
                mutable: false,
                inner: Box::new(inner),
            }
        }
        _ => TypeRef::Unresolved(text(node, src).to_string()),
    }
}

/// `ACCEPTS`/`RETURNS` edges for a function/method signature. Go allows
/// multiple return types (`func Divide(a, b int) (int, error)`) -- the
/// canonical model places no limit on how many `RETURNS` edges a function
/// symbol has, so this simply emits one per declared return type; no
/// canonical-model change was needed. Not yet wired into
/// `extract`/`resolve_file`, matching `extract_type`'s own gap above.
#[allow(dead_code)]
fn extract_signature_types(
    node: Node,
    src: &str,
    fn_symbol: &SymbolEntity,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    if let Some(params) = node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for param in params.children(&mut cursor) {
            if param.kind() != "parameter_declaration" {
                continue;
            }
            let Some(type_node) = param.child_by_field_name("type") else {
                continue;
            };
            let name_count = {
                let mut name_cursor = param.walk();
                param
                    .children_by_field_name("name", &mut name_cursor)
                    .count()
                    .max(1)
            };
            let type_ref = extract_type(type_node, src);
            let type_symbol = type_symbol_for_ref(&type_ref);
            for _ in 0..name_count {
                edges.push(Edge::new(
                    fn_symbol.id.clone(),
                    type_symbol.id.clone(),
                    EdgeKind::Accepts,
                ));
            }
            symbols.push(type_symbol);
        }
    }

    match node.child_by_field_name("result") {
        Some(result) if result.kind() == "parameter_list" => {
            let mut cursor = result.walk();
            for param in result.children(&mut cursor) {
                if param.kind() != "parameter_declaration" {
                    continue;
                }
                let Some(type_node) = param.child_by_field_name("type") else {
                    continue;
                };
                let type_ref = extract_type(type_node, src);
                let type_symbol = type_symbol_for_ref(&type_ref);
                edges.push(Edge::new(
                    fn_symbol.id.clone(),
                    type_symbol.id.clone(),
                    EdgeKind::Returns,
                ));
                symbols.push(type_symbol);
            }
        }
        Some(single) => {
            let type_ref = extract_type(single, src);
            let type_symbol = type_symbol_for_ref(&type_ref);
            edges.push(Edge::new(
                fn_symbol.id.clone(),
                type_symbol.id.clone(),
                EdgeKind::Returns,
            ));
            symbols.push(type_symbol);
        }
        None => {
            let type_symbol = type_symbol_for_ref(&TypeRef::Primitive("()".to_string()));
            edges.push(Edge::new(
                fn_symbol.id.clone(),
                type_symbol.id.clone(),
                EdgeKind::Returns,
            ));
            symbols.push(type_symbol);
        }
    }

    (symbols, edges)
}

#[allow(dead_code)]
fn type_symbol_for_ref(type_ref: &TypeRef) -> SymbolEntity {
    crate::types::type_symbol_for(type_ref)
}

/// A function's own type-parameter list (`func Identity[T any](x T) T`)
/// becomes `Generic` symbols declared under the function. Not yet wired into
/// `extract`/`resolve_file` (threading these names into `extract_type` so a
/// matching usage resolves to `TypeRef::Generic` rather than an unrelated
/// `Named` is future work), matching the Rust adapter's own
/// `extract_generic_params` gap.
#[allow(dead_code)]
fn extract_generic_params(
    node: Node,
    src: &str,
    owner_symbol: &SymbolEntity,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    let Some(type_params) = node.child_by_field_name("type_parameters") else {
        return (symbols, edges);
    };

    let mut cursor = type_params.walk();
    for param in type_params.children(&mut cursor) {
        if param.kind() != "type_parameter_declaration" {
            continue;
        }
        let mut name_cursor = param.walk();
        for name_node in param.children_by_field_name("name", &mut name_cursor) {
            let symbol = SymbolEntity::new(
                SymbolKind::Generic,
                owner_symbol.id.as_str(),
                text(name_node, src),
            );
            edges.push(Edge::new(
                owner_symbol.id.clone(),
                symbol.id.clone(),
                EdgeKind::Declares,
            ));
            symbols.push(symbol);
        }
    }

    (symbols, edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
        let adapter = GoAdapter;
        let ast = adapter.parse(source).expect("valid go parses");
        adapter.extract(&ast, "module.go")
    }

    #[test]
    fn parses_valid_go_source() {
        let adapter = GoAdapter;
        assert!(adapter.parse("package main\n\nfunc main() {}\n").is_ok());
    }

    #[test]
    fn extract_always_returns_the_file_module_first() {
        let (entities, _) = extract("package main\n");
        assert_eq!(entities[0].kind, SyntaxKind::Module);
        assert_eq!(entities[0].name, "module.go");
    }

    #[test]
    fn extracts_a_top_level_function_with_its_parameters() {
        let (entities, edges) =
            extract("package main\n\nfunc Add(a int, b int) int {\n\treturn a + b\n}\n");

        let module = &entities[0];
        let function = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "Add")
            .expect("Function entity");
        assert!(edges.contains(&Edge::new(
            module.id.clone(),
            function.id.clone(),
            EdgeKind::Declares
        )));

        let param_names: Vec<&str> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Parameter)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(param_names, vec!["a", "b"]);
    }

    #[test]
    fn a_parameter_declaration_with_comma_joined_names_produces_one_parameter_each() {
        let (entities, _) =
            extract("package main\n\nfunc Add(a, b int) int {\n\treturn a + b\n}\n");

        let param_names: Vec<&str> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Parameter)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(param_names, vec!["a", "b"]);
    }

    #[test]
    fn extracts_a_struct_and_a_method_declared_on_it() {
        let source = "package main\n\ntype Circle struct {\n\tRadius float64\n}\n\nfunc (c *Circle) Area() float64 {\n\treturn c.Radius\n}\n";
        let (entities, edges) = extract(source);

        let module = &entities[0];
        let circle = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Struct && e.name == "Circle")
            .expect("Struct entity");
        assert!(edges.contains(&Edge::new(
            module.id.clone(),
            circle.id.clone(),
            EdgeKind::Declares
        )));

        let area = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "Area")
            .expect("Method entity");
        assert!(
            edges.contains(&Edge::new(
                circle.id.clone(),
                area.id.clone(),
                EdgeKind::Declares
            )),
            "Area should be declared under Circle, not the module"
        );
    }

    #[test]
    fn a_method_on_an_unknown_receiver_falls_back_to_the_module() {
        let source = "package main\n\nfunc (c *Missing) Area() float64 {\n\treturn 0\n}\n";
        let (entities, edges) = extract(source);

        let module = &entities[0];
        let area = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "Area")
            .expect("Method entity");
        assert!(edges.contains(&Edge::new(
            module.id.clone(),
            area.id.clone(),
            EdgeKind::Declares
        )));
    }

    #[test]
    fn extracts_an_interface_with_its_method_signatures() {
        let source = "package main\n\ntype Shape interface {\n\tArea() float64\n}\n";
        let (entities, edges) = extract(source);

        let shape = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Interface && e.name == "Shape")
            .expect("Interface entity");
        let area = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "Area")
            .expect("Method signature entity");
        assert!(edges.contains(&Edge::new(
            shape.id.clone(),
            area.id.clone(),
            EdgeKind::Declares
        )));
    }

    #[test]
    fn extracts_a_short_var_declaration_in_a_function_body_as_a_variable() {
        let (entities, edges) =
            extract("package main\n\nfunc run() {\n\ttotal := 1\n\t_ = total\n}\n");

        let function = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "run")
            .expect("Function entity");
        let variable = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Variable && e.name == "total")
            .expect("Variable entity");
        assert!(edges.contains(&Edge::new(
            function.id.clone(),
            variable.id.clone(),
            EdgeKind::Contains
        )));
        assert!(!edges.contains(&Edge::new(
            function.id.clone(),
            variable.id.clone(),
            EdgeKind::Declares
        )));
    }

    #[test]
    fn extracts_a_package_level_var_declaration() {
        let (entities, _) = extract("package main\n\nvar Counter int\n");
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Variable && e.name == "Counter"));
    }

    #[test]
    fn a_non_declaration_statement_becomes_a_generic_statement_entity() {
        let (entities, _) = extract("package main\n\nfunc run() {\n\tprintln(1)\n}\n");
        assert!(entities.iter().any(|e| e.kind == SyntaxKind::Statement));
    }

    #[test]
    fn same_named_method_on_different_receivers_gets_distinct_ids() {
        let source = "package main\n\ntype A struct{}\ntype B struct{}\nfunc (a *A) Run() {}\nfunc (b *B) Run() {}\n";
        let (entities, _) = extract(source);

        let methods: Vec<&SyntaxEntity> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Method && e.name == "Run")
            .collect();
        assert_eq!(methods.len(), 2);
        assert_ne!(methods[0].id, methods[1].id);
    }

    // -- type extraction (not yet wired into extract/resolve_file) --

    fn parse_source(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn result_type_of<'a>(tree: &'a Tree, src: &'a str) -> Node<'a> {
        find_matching(tree, src, "f")
            .child_by_field_name("result")
            .unwrap()
    }

    #[test]
    fn extracts_a_primitive_type() {
        let full = "package main\n\nfunc f() int { return 0 }\n";
        let tree = parse_source(full);
        let node = result_type_of(&tree, full);
        assert_eq!(
            extract_type(node, full),
            TypeRef::Primitive("int".to_string())
        );
    }

    #[test]
    fn extracts_a_user_defined_named_type() {
        let full = "package main\n\nfunc f() Circle { return Circle{} }\n";
        let tree = parse_source(full);
        let node = result_type_of(&tree, full);
        assert_eq!(
            extract_type(node, full),
            TypeRef::Named {
                name: "Circle".to_string(),
                type_args: Vec::new(),
            }
        );
    }

    #[test]
    fn extracts_a_pointer_type_as_a_reference() {
        let full = "package main\n\nfunc f() *Circle { return nil }\n";
        let tree = parse_source(full);
        let node = result_type_of(&tree, full);
        assert_eq!(
            extract_type(node, full),
            TypeRef::Reference {
                mutable: false,
                inner: Box::new(TypeRef::Named {
                    name: "Circle".to_string(),
                    type_args: Vec::new(),
                }),
            }
        );
    }

    #[test]
    fn binds_a_single_return_type_via_returns() {
        let full = "package main\n\nfunc make() Circle { return Circle{} }\n";
        let tree = parse_source(full);
        let node = find_matching(&tree, full, "make");
        let fn_symbol = SymbolEntity::new(SymbolKind::Function, "module.go", "make");

        let (_symbols, edges) = extract_signature_types(node, full, &fn_symbol);

        let circle_type = crate::types::type_symbol_for(&TypeRef::Named {
            name: "Circle".to_string(),
            type_args: Vec::new(),
        });
        assert!(edges.contains(&Edge::new(
            fn_symbol.id.clone(),
            circle_type.id.clone(),
            EdgeKind::Returns
        )));
    }

    #[test]
    fn binds_multiple_return_types_via_repeated_returns_edges() {
        let full = "package main\n\nfunc divide() (int, error) { return 0, nil }\n";
        let tree = parse_source(full);
        let node = find_matching(&tree, full, "divide");
        let fn_symbol = SymbolEntity::new(SymbolKind::Function, "module.go", "divide");

        let (_symbols, edges) = extract_signature_types(node, full, &fn_symbol);

        let returns_count = edges.iter().filter(|e| e.kind == EdgeKind::Returns).count();
        assert_eq!(
            returns_count, 2,
            "a (int, error) result should bind two RETURNS edges"
        );

        let int_type = crate::types::type_symbol_for(&TypeRef::Primitive("int".to_string()));
        let error_type = crate::types::type_symbol_for(&TypeRef::Primitive("error".to_string()));
        assert!(edges.contains(&Edge::new(
            fn_symbol.id.clone(),
            int_type.id.clone(),
            EdgeKind::Returns
        )));
        assert!(edges.contains(&Edge::new(
            fn_symbol.id.clone(),
            error_type.id.clone(),
            EdgeKind::Returns
        )));
    }

    #[test]
    fn a_signature_with_no_result_binds_returns_to_unit() {
        let full = "package main\n\nfunc run() {}\n";
        let tree = parse_source(full);
        let node = find_matching(&tree, full, "run");
        let fn_symbol = SymbolEntity::new(SymbolKind::Function, "module.go", "run");

        let (_symbols, edges) = extract_signature_types(node, full, &fn_symbol);

        let unit_type = crate::types::type_symbol_for(&TypeRef::Primitive("()".to_string()));
        assert!(edges.contains(&Edge::new(
            fn_symbol.id.clone(),
            unit_type.id.clone(),
            EdgeKind::Returns
        )));
    }

    #[test]
    fn binds_parameter_types_via_accepts() {
        let full = "package main\n\nfunc add(a, b int) int { return a + b }\n";
        let tree = parse_source(full);
        let node = find_matching(&tree, full, "add");
        let fn_symbol = SymbolEntity::new(SymbolKind::Function, "module.go", "add");

        let (_symbols, edges) = extract_signature_types(node, full, &fn_symbol);

        let int_type = crate::types::type_symbol_for(&TypeRef::Primitive("int".to_string()));
        let accepts_count = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Accepts && e.to == int_type.id)
            .count();
        assert_eq!(accepts_count, 2, "both a and b are int");
    }

    #[test]
    fn extracts_a_generic_type_parameter() {
        let full = "package main\n\nfunc Identity[T any](x T) T { return x }\n";
        let tree = parse_source(full);
        let node = find_matching(&tree, full, "Identity");
        let fn_symbol = SymbolEntity::new(SymbolKind::Function, "module.go", "Identity");

        let (symbols, edges) = extract_generic_params(node, full, &fn_symbol);

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].kind, SymbolKind::Generic);
        assert_eq!(symbols[0].name, "T");
        assert!(edges.contains(&Edge::new(
            fn_symbol.id.clone(),
            symbols[0].id.clone(),
            EdgeKind::Declares
        )));
    }

    fn find_matching<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        let mut cursor = tree.root_node().walk();
        let found = tree.root_node().children(&mut cursor).find(|c| {
            (c.kind() == "function_declaration" || c.kind() == "method_declaration")
                && c.child_by_field_name("name")
                    .is_some_and(|n| text(n, src) == name)
        });
        found.unwrap_or_else(|| panic!("no function/method named {name}"))
    }

    // -- symbol/reference/call/instantiate/structural resolution --

    struct Resolved {
        entities: Vec<SyntaxEntity>,
        scope_tree: ScopeTree,
        scope: usize,
        symbol_table: SymbolTable,
        fn_symbol: SymbolEntity,
    }

    fn setup(source: &str, fn_name: &str) -> (Tree, Resolved) {
        let adapter = GoAdapter;
        let ast = adapter.parse(source).expect("valid go parses");
        let (entities, edges) = adapter.extract(&ast, "module.go");
        let module = entities[0].clone();
        let fn_entity = entities
            .iter()
            .find(|e| {
                (e.kind == SyntaxKind::Function || e.kind == SyntaxKind::Method)
                    && e.name == fn_name
            })
            .unwrap_or_else(|| panic!("no Function/Method entity named {fn_name}"))
            .clone();
        let scope_tree = ScopeTree::build(&module, &entities, &edges);
        let scope = scope_tree
            .scope_of(&fn_entity.id)
            .expect("function opens a scope");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let fn_symbol = symbol_table
            .find_by_syntax_id(&fn_entity.id)
            .expect("function symbol")
            .clone();
        (
            ast.tree,
            Resolved {
                entities,
                scope_tree,
                scope,
                symbol_table,
                fn_symbol,
            },
        )
    }

    fn body_of<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        find_matching(tree, src, name)
            .child_by_field_name("body")
            .unwrap()
    }

    #[test]
    fn resolves_a_reference_to_an_in_scope_variable() {
        let source = "package main\n\nfunc run() {\n\tvalue := 1\n\t_ = value\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) = resolve_body(
            body,
            source,
            &r.fn_symbol,
            &r.scope_tree,
            r.scope,
            &r.symbol_table,
        );

        let value_var = r
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Variable)
            .unwrap();
        let value_symbol = r.symbol_table.find_by_syntax_id(&value_var.id).unwrap();
        assert!(edges.contains(&Edge::new(
            r.fn_symbol.id.clone(),
            value_symbol.id.clone(),
            EdgeKind::References
        )));
    }

    #[test]
    fn a_reference_to_an_undefined_name_gets_an_explicit_placeholder() {
        let source = "package main\n\nfunc run() {\n\t_ = missing\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (symbols, edges) = resolve_body(
            body,
            source,
            &r.fn_symbol,
            &r.scope_tree,
            r.scope,
            &r.symbol_table,
        );

        assert!(symbols.iter().any(|s| s.name.starts_with("unknown-local")));
        assert!(edges
            .iter()
            .any(|e| e.kind == EdgeKind::References && e.from == r.fn_symbol.id));
    }

    #[test]
    fn resolves_a_free_function_call() {
        let source = "package main\n\nfunc helper() {}\n\nfunc run() {\n\thelper()\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) = resolve_body(
            body,
            source,
            &r.fn_symbol,
            &r.scope_tree,
            r.scope,
            &r.symbol_table,
        );

        let helper = r
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "helper")
            .unwrap();
        let helper_symbol = r.symbol_table.find_by_syntax_id(&helper.id).unwrap();
        assert!(edges.contains(&Edge::new(
            r.fn_symbol.id.clone(),
            helper_symbol.id.clone(),
            EdgeKind::Calls
        )));
    }

    #[test]
    fn a_free_call_to_a_function_declared_in_another_file_resolves_via_repo_wide_fallback() {
        // Regression test for the cross-file CALLS gap found via Phase 18.1's
        // real-world benchmarking: the scope tree only ever covered the
        // calling file's own entities, so a call to a function declared in
        // another file of the same package fell straight to an
        // `unknown-local` placeholder.
        use crate::symbols::build_symbol_table;

        let adapter = GoAdapter;
        let helper_ast = adapter.parse("package main\n\nfunc helper() {}\n").unwrap();
        let (helper_entities, helper_edges) = adapter.extract(&helper_ast, "helper.go");
        let caller_ast = adapter
            .parse("package main\n\nfunc run() {\n\thelper()\n}\n")
            .unwrap();
        let (caller_entities, caller_edges) = adapter.extract(&caller_ast, "run.go");

        let mut all_entities = helper_entities.clone();
        all_entities.extend(caller_entities.clone());
        let mut all_edges = helper_edges;
        all_edges.extend(caller_edges);
        let symbol_table = build_symbol_table(&all_entities);

        let caller_module = caller_entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Module)
            .unwrap();
        let module_symbol = symbol_table.find_by_syntax_id(&caller_module.id).unwrap();

        let (_symbols, edges) = adapter.resolve_file(
            &caller_ast,
            "run.go",
            &all_entities,
            &symbol_table,
            &all_edges,
            module_symbol,
            caller_module,
        );

        let helper = helper_entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "helper")
            .unwrap();
        let helper_symbol = symbol_table.find_by_syntax_id(&helper.id).unwrap();
        let run = caller_entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "run")
            .unwrap();
        let run_symbol = symbol_table.find_by_syntax_id(&run.id).unwrap();

        assert!(
            edges.contains(&Edge::new(
                run_symbol.id.clone(),
                helper_symbol.id.clone(),
                EdgeKind::Calls
            )),
            "expected a real CALLS edge to helper's resolved symbol, not an unresolved placeholder: {edges:?}"
        );
    }

    #[test]
    fn resolves_a_method_call_by_best_effort_name_match() {
        let source = "package main\n\ntype Shape struct{}\nfunc (s *Shape) Area() {}\n\nfunc run(shape Shape) {\n\tshape.Area()\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) = resolve_body(
            body,
            source,
            &r.fn_symbol,
            &r.scope_tree,
            r.scope,
            &r.symbol_table,
        );

        let area = r
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "Area")
            .unwrap();
        let area_symbol = r.symbol_table.find_by_syntax_id(&area.id).unwrap();
        assert!(edges.contains(&Edge::new(
            r.fn_symbol.id.clone(),
            area_symbol.id.clone(),
            EdgeKind::Calls
        )));
    }

    #[test]
    fn resolves_a_composite_literal_instantiation() {
        let source =
            "package main\n\ntype Circle struct{}\n\nfunc run() {\n\tc := Circle{}\n\t_ = c\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) = resolve_body(
            body,
            source,
            &r.fn_symbol,
            &r.scope_tree,
            r.scope,
            &r.symbol_table,
        );

        let circle = r
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Struct && e.name == "Circle")
            .unwrap();
        let circle_symbol = r.symbol_table.find_by_syntax_id(&circle.id).unwrap();
        assert!(edges.contains(&Edge::new(
            r.fn_symbol.id.clone(),
            circle_symbol.id.clone(),
            EdgeKind::Instantiates
        )));
    }

    #[test]
    fn a_call_to_an_unsupported_callee_form_gets_an_explicit_placeholder() {
        let source = "package main\n\nfunc run() {\n\tfoo()()\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (symbols, edges) = resolve_body(
            body,
            source,
            &r.fn_symbol,
            &r.scope_tree,
            r.scope,
            &r.symbol_table,
        );

        assert!(symbols
            .iter()
            .any(|s| s.name.starts_with("unsupported-form")));
        assert!(edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.from == r.fn_symbol.id));
    }

    #[test]
    fn a_struct_implementing_every_interface_method_gets_an_implements_edge() {
        let source = "package main\n\ntype Shape interface {\n\tArea() float64\n}\n\ntype Circle struct{}\nfunc (c *Circle) Area() float64 { return 0 }\n";
        let adapter = GoAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, edges) = adapter.extract(&ast, "module.go");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let implements_edges = resolve_implements(&entities, &entities, &edges, &symbol_table);

        let circle = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Struct && e.name == "Circle")
            .unwrap();
        let shape = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Interface && e.name == "Shape")
            .unwrap();
        let circle_symbol = symbol_table.find_by_syntax_id(&circle.id).unwrap();
        let shape_symbol = symbol_table.find_by_syntax_id(&shape.id).unwrap();
        assert!(implements_edges.contains(&Edge::new(
            circle_symbol.id.clone(),
            shape_symbol.id.clone(),
            EdgeKind::Implements
        )));
    }

    #[test]
    fn a_struct_missing_a_required_method_gets_no_implements_edge() {
        let source = "package main\n\ntype Shape interface {\n\tArea() float64\n\tPerimeter() float64\n}\n\ntype Circle struct{}\nfunc (c *Circle) Area() float64 { return 0 }\n";
        let adapter = GoAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, edges) = adapter.extract(&ast, "module.go");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let implements_edges = resolve_implements(&entities, &entities, &edges, &symbol_table);
        assert!(implements_edges.is_empty());
    }

    #[test]
    fn resolves_imports_to_an_explicit_external_package_placeholder() {
        let source = "package main\n\nimport (\n\t\"fmt\"\n\tm \"math\"\n)\n\nfunc run() {\n\t_ = fmt.Sprint\n\t_ = m.Pi\n}\n";
        let adapter = GoAdapter;
        let ast = adapter.parse(source).unwrap();
        let file_module = SyntaxEntity::new(SyntaxKind::Module, "module.go", "module.go");
        let mut file_entities = vec![file_module.clone()];
        let mut file_edges = Vec::new();
        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &file_module,
            "module.go",
            &mut file_entities,
            &mut file_edges,
            &mut sites,
        );
        let module_symbol = SymbolEntity::new(SymbolKind::Namespace, "module.go", "module.go");

        let (symbols, edges) = resolve_imports(&sites.imports, &ast.source, &module_symbol);

        assert_eq!(symbols.len(), 2);
        assert!(symbols.iter().any(|s| s.name == "external-package:fmt"));
        assert!(symbols.iter().any(|s| s.name == "external-package:math"));
        assert_eq!(
            edges.iter().filter(|e| e.kind == EdgeKind::Imports).count(),
            2
        );
    }

    #[test]
    fn exports_an_uppercase_top_level_declaration_but_not_a_lowercase_one() {
        let source = "package main\n\nfunc Public() {}\nfunc private() {}\n";
        let adapter = GoAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, edges) = adapter.extract(&ast, "module.go");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table
            .find_by_syntax_id(&entities[0].id)
            .unwrap()
            .clone();

        let export_edges = resolve_exports(&entities, &edges, &module_symbol, &symbol_table);

        let public = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "Public")
            .unwrap();
        let public_symbol = symbol_table.find_by_syntax_id(&public.id).unwrap();
        assert_eq!(export_edges.len(), 1);
        assert!(export_edges.contains(&Edge::new(
            module_symbol.id.clone(),
            public_symbol.id.clone(),
            EdgeKind::Exports
        )));
    }
}
