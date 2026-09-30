use std::collections::HashSet;

use tree_sitter::{Node, Parser, Tree};

use crate::adapter::LanguageAdapter;
use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};
use crate::resolve::unresolved_symbol;
use crate::symbols::{ScopeTree, SymbolTable};
use crate::types::TypeRef;

/// TypeScript's predefined types (https://www.typescriptlang.org/docs/handbook/2/everyday-types.html),
/// treated as `TypeRef::Primitive` the same way Go's/Rust's/Python's own
/// built-in lists are. `tree-sitter-typescript` surfaces every one of these
/// as a single `predefined_type` node whose own text is the keyword, so
/// `extract_type` doesn't need this list to *classify* a `predefined_type`
/// (it always is one) -- it's kept for parity with the other adapters'
/// documented primitive sets and any future non-`predefined_type` primitive
/// spellings.
const PRIMITIVE_TYPES: &[&str] = &[
    "string", "number", "boolean", "any", "void", "null", "undefined", "symbol", "never",
    "unknown", "object", "bigint",
];

/// The parsed form a `TypeScriptAdapter` produces: `tree-sitter`'s `Tree`
/// paired with the source text it was parsed from, mirroring `GoAst` --
/// `tree-sitter` nodes are spans into the original source, not a
/// self-contained typed tree.
pub struct TypeScriptAst {
    tree: Tree,
    source: String,
}

/// The TypeScript language adapter: parses TypeScript source (the plain
/// `.ts` grammar -- JSX/TSX embedding is out of scope for this sub-phase,
/// per Phase 17.2's task list) via `tree-sitter`/`tree-sitter-typescript`
/// and maps it onto the canonical syntax model. Structured the same way as
/// `GoAdapter`, the most recent `tree-sitter`-based adapter in this
/// codebase.
pub struct TypeScriptAdapter;

impl LanguageAdapter for TypeScriptAdapter {
    type Ast = TypeScriptAst;

    fn parse(&self, source: &str) -> Result<Self::Ast> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .map_err(|err| Error::Message(format!("failed to load TypeScript grammar: {err}")))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::Message("failed to parse TypeScript source".to_string()))?;
        Ok(TypeScriptAst {
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
        // `SyntaxEntity`s `sites` carries are the exact same (same
        // `EntityId`) ones `extract` already handed the pipeline -- matches
        // `GoAdapter::resolve_file`'s own re-derivation.
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
            let (body_symbols, body_edges) =
                resolve_body(*body, &ast.source, &fn_symbol, &scope_tree, scope, symbol_table);
            symbols.extend(body_symbols);
            edges.extend(body_edges);
        }

        edges.extend(resolve_heritage(
            &sites,
            all_entities,
            edges_so_far,
            symbol_table,
            &ast.source,
        ));

        let (import_symbols, import_edges) =
            resolve_imports(&sites.imports, &ast.source, module_symbol);
        symbols.extend(import_symbols);
        edges.extend(import_edges);

        edges.extend(resolve_exports(
            &sites,
            &file_entities,
            module_symbol,
            symbol_table,
        ));

        (symbols, edges)
    }
}

/// Sites `extract_items` collects that `resolve_file` needs the raw tree
/// for but that have no place in `extract`'s plain `(Vec<SyntaxEntity>,
/// Vec<Edge>)`. Mirrors `GoAdapter`'s own `ResolutionSites`, extended with
/// `classes`/`interfaces` (for `EXTENDS`/`IMPLEMENTS` heritage resolution)
/// and `exported_names` (TypeScript's `export` is an explicit keyword,
/// unlike Go's uppercase-name convention, so exports have to be tracked as
/// they're encountered rather than inferred after the fact).
#[derive(Default)]
struct ResolutionSites<'a> {
    bodies: Vec<(SyntaxEntity, Node<'a>)>,
    imports: Vec<Node<'a>>,
    classes: Vec<(SyntaxEntity, Node<'a>)>,
    interfaces: Vec<(SyntaxEntity, Node<'a>)>,
    exported_names: Vec<String>,
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

/// Walk a `program`'s (or, recursively, an `export_statement`'s wrapped)
/// direct children. Unlike Go, TypeScript classes/methods don't need a
/// two-pass walk -- a class's methods are always nested directly inside its
/// own `class_body`, never declared free-standing the way a Go method with
/// a receiver is -- so a single top-down pass is enough.
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
    for child in root.named_children(&mut cursor) {
        extract_item(child, src, parent, scope, entities, edges, sites, false);
    }
}

/// Extract one item-level node. `exported` is set when this node is being
/// visited because it's the `declaration` field of an `export_statement` --
/// threaded through so the produced entity's name lands in
/// `sites.exported_names`.
#[allow(clippy::too_many_arguments)]
fn extract_item<'a>(
    node: Node<'a>,
    src: &'a str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
    exported: bool,
) {
    match node.kind() {
        "class_declaration" => extract_class(node, src, parent, scope, entities, edges, sites, exported),
        "interface_declaration" => {
            extract_interface(node, src, parent, scope, entities, edges, sites, exported)
        }
        "function_declaration" => {
            extract_function(node, src, parent, scope, entities, edges, sites, exported)
        }
        "lexical_declaration" | "variable_declaration" => {
            for name_node in declarator_names(node) {
                let entity = SyntaxEntity::new(SyntaxKind::Variable, scope, text(name_node, src));
                if exported {
                    sites.exported_names.push(entity.name.clone());
                }
                contain(parent, entity, entities, edges);
            }
        }
        "import_statement" => sites.imports.push(node),
        "export_statement" => {
            if let Some(declaration) = node.child_by_field_name("declaration") {
                extract_item(declaration, src, parent, scope, entities, edges, sites, true);
            } else if let Some(export_clause) = find_child(node, "export_clause") {
                let mut clause_cursor = export_clause.walk();
                for specifier in export_clause.named_children(&mut clause_cursor) {
                    if specifier.kind() != "export_specifier" {
                        continue;
                    }
                    if let Some(name_node) = specifier.child_by_field_name("name") {
                        sites.exported_names.push(text(name_node, src).to_string());
                    }
                }
            } else if let Some(value) = node.child_by_field_name("value") {
                // `export default <expr>`: an identifier default-exports an
                // already-declared entity; anything else (an inline
                // expression/anonymous function) has no declared name to
                // attach an EXPORTS edge to -- a documented gap, not
                // guessed at.
                if value.kind() == "identifier" {
                    sites.exported_names.push(text(value, src).to_string());
                }
            }
        }
        _ => {}
    }
}

fn find_child<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == kind);
    found
}

/// A `class_declaration`: its own `Class` entity, its methods (nested
/// `method_definition`s under `class_body`, becoming `Method` entities
/// declared under the class, matching the Python adapter's own class/method
/// nesting), and its heritage clauses (recorded in `sites.classes` for
/// `resolve_heritage`, since `EXTENDS`/`IMPLEMENTS` resolution needs
/// repository-wide symbol lookup that isn't available at extract time).
#[allow(clippy::too_many_arguments)]
fn extract_class<'a>(
    node: Node<'a>,
    src: &'a str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
    exported: bool,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let name = text(name_node, src);
    let entity = SyntaxEntity::new(SyntaxKind::Class, scope, name);
    let class_scope = child_scope(scope, name);
    declare(parent, entity.clone(), entities, edges);
    if exported {
        sites.exported_names.push(entity.name.clone());
    }

    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for member in body.named_children(&mut cursor) {
            if member.kind() != "method_definition" {
                // Fields, index signatures, static blocks: no clean mapping
                // onto the canonical model's Method/Variable shape -- a
                // documented gap, same posture as Go's un-extracted struct
                // fields.
                continue;
            }
            extract_method(member, src, &entity, &class_scope, entities, edges, sites);
        }
    }

    sites.classes.push((entity, node));
}

/// A `method_definition` inside a `class_body`: its own `Method` entity,
/// parameters, and body.
fn extract_method<'a>(
    node: Node<'a>,
    src: &'a str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let name = text(name_node, src);
    let entity = SyntaxEntity::new(SyntaxKind::Method, owner_scope, name);
    let method_scope = child_scope(owner_scope, name);
    declare(owner, entity.clone(), entities, edges);

    if let Some(params) = node.child_by_field_name("parameters") {
        extract_parameters(params, src, &entity, &method_scope, entities, edges);
    }
    if let Some(body) = node.child_by_field_name("body") {
        extract_body(body, src, &entity, &method_scope, entities, edges);
        sites.bodies.push((entity, body));
    }
}

/// An `interface_declaration`: its own `Interface` entity, with its method
/// signatures (`method_signature` nodes in `interface_body`) declared as
/// `Method` entities under it -- mirrors Go's `method_elem` handling, the
/// same shape structural-interface-satisfaction (`resolve_heritage`) needs.
/// Heritage (`interface B extends A`) is recorded in `sites.interfaces` for
/// the same repository-wide reason `extract_class` defers class heritage.
#[allow(clippy::too_many_arguments)]
fn extract_interface<'a>(
    node: Node<'a>,
    src: &'a str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
    exported: bool,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let name = text(name_node, src);
    let entity = SyntaxEntity::new(SyntaxKind::Interface, scope, name);
    let iface_scope = child_scope(scope, name);
    declare(parent, entity.clone(), entities, edges);
    if exported {
        sites.exported_names.push(entity.name.clone());
    }

    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for member in body.named_children(&mut cursor) {
            if member.kind() != "method_signature" {
                // property_signature/call_signature/index_signature/
                // construct_signature: no clean Method/Variable mapping --
                // a documented gap, same as struct fields elsewhere.
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

    sites.interfaces.push((entity, node));
}

/// A top-level function declaration: its own entity, parameters, and body.
#[allow(clippy::too_many_arguments)]
fn extract_function<'a>(
    node: Node<'a>,
    src: &'a str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
    exported: bool,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let name = text(name_node, src);
    let entity = SyntaxEntity::new(SyntaxKind::Function, scope, name);
    let fn_scope = child_scope(scope, name);
    declare(parent, entity.clone(), entities, edges);
    if exported {
        sites.exported_names.push(entity.name.clone());
    }

    if let Some(params) = node.child_by_field_name("parameters") {
        extract_parameters(params, src, &entity, &fn_scope, entities, edges);
    }
    if let Some(body) = node.child_by_field_name("body") {
        extract_body(body, src, &entity, &fn_scope, entities, edges);
        sites.bodies.push((entity, body));
    }
}

/// A `formal_parameters`' `required_parameter`/`optional_parameter`s become
/// `Parameter` entities. The bound identifier lives under the `pattern`
/// field for a plain `x: T` parameter (`pattern`'s subtypes include
/// `identifier`) or the `name` field for a rest parameter (`...rest`,
/// `rest_pattern`) -- checked in that order. A destructured parameter
/// (`{a, b}: T`) binds no single name -- a documented gap, not guessed at,
/// matching Go's own unnamed-parameter gap.
fn extract_parameters(
    params: Node,
    src: &str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = params.walk();
    for param in params.named_children(&mut cursor) {
        if param.kind() != "required_parameter" && param.kind() != "optional_parameter" {
            continue;
        }
        let bound = param
            .child_by_field_name("pattern")
            .or_else(|| param.child_by_field_name("name"));
        let Some(bound) = bound else { continue };
        let name_node = match bound.kind() {
            "identifier" => Some(bound),
            "rest_pattern" => bound.named_child(0).filter(|c| c.kind() == "identifier"),
            _ => None,
        };
        let Some(name_node) = name_node else { continue };
        let entity = SyntaxEntity::new(SyntaxKind::Parameter, scope, text(name_node, src));
        contain(parent, entity, entities, edges);
    }
}

/// Walk a function/method body's direct statements (a `statement_block`'s
/// named children -- unlike Go's `block`, there's no intermediate
/// `statement_list` wrapper): a `let`/`const`/`var` declaration becomes
/// `Variable` entities; everything else becomes a generic `Statement`
/// entity. A nested `function`/`class`/`interface` declaration inside a
/// body isn't extracted -- a documented gap, same as the other adapters not
/// walking into nested item definitions.
fn extract_body(
    block: Node,
    src: &str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = block.walk();
    for (index, stmt) in block.named_children(&mut cursor).enumerate() {
        match stmt.kind() {
            "lexical_declaration" | "variable_declaration" => {
                for name_node in declarator_names(stmt) {
                    let entity = SyntaxEntity::new(
                        SyntaxKind::Variable,
                        &format!("{scope}#{index}"),
                        text(name_node, src),
                    );
                    contain(parent, entity, entities, edges);
                }
            }
            "function_declaration" | "class_declaration" | "interface_declaration" => {}
            _ => {
                let entity =
                    SyntaxEntity::new(SyntaxKind::Statement, scope, format!("stmt_{index}"));
                contain(parent, entity, entities, edges);
            }
        }
    }
}

/// Every `identifier`/`array_pattern`/`object_pattern` bound by a
/// `variable_declarator` directly under a `lexical_declaration`/
/// `variable_declaration`. Destructuring patterns (`array_pattern`/
/// `object_pattern`) don't resolve to a single name -- only a plain
/// `identifier` binding is extracted, a documented gap matching
/// `extract_parameters`'s own.
fn declarator_names(node: Node) -> Vec<Node> {
    let mut names = Vec::new();
    let mut cursor = node.walk();
    for declarator in node.named_children(&mut cursor) {
        if declarator.kind() != "variable_declarator" {
            continue;
        }
        if let Some(name_node) = declarator.child_by_field_name("name") {
            if name_node.kind() == "identifier" {
                names.push(name_node);
            }
        }
    }
    names
}

/// `REFERENCES`/`CALLS`/`INSTANTIATES` edges from one function/method body.
/// Mirrors `GoAdapter`'s own `resolve_body`: a bare identifier read resolves
/// via `scope_tree`; a bare-name call resolves the same way; a method call
/// (`obj.method()`) resolves by best-effort name match against any `Method`
/// symbol in the file; a `new Type()` whose constructor name resolves in
/// scope to a `Type` symbol is an `INSTANTIATES` site. Anything unresolved
/// gets an explicit placeholder symbol, never silently dropped.
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
    let mut news: Vec<Node<'a>> = Vec::new();

    walk_block_statements(
        body,
        &mut |n| names.push(n),
        &mut |n| calls.push(n),
        &mut |n| news.push(n),
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
                    .cloned();
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
            "member_expression" => {
                let Some(property) = func.child_by_field_name("property") else {
                    continue;
                };
                let ident = text(property, src);
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

    for new_node in &news {
        let Some(constructor) = new_node.child_by_field_name("constructor") else {
            continue;
        };
        if constructor.kind() != "identifier" {
            continue;
        }
        let ident = text(constructor, src);
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
    on_new: &mut impl FnMut(Node<'a>),
) {
    match node.kind() {
        "identifier" => on_name(node),
        "call_expression" => {
            on_call(node);
            if let Some(func) = node.child_by_field_name("function") {
                walk_expr_sites(func, on_name, on_call, on_new);
            }
            if let Some(args) = node.child_by_field_name("arguments") {
                let mut cursor = args.walk();
                for arg in args.named_children(&mut cursor) {
                    walk_expr_sites(arg, on_name, on_call, on_new);
                }
            }
        }
        "member_expression" => {
            if let Some(object) = node.child_by_field_name("object") {
                walk_expr_sites(object, on_name, on_call, on_new);
            }
        }
        "new_expression" => {
            on_new(node);
            if let Some(args) = node.child_by_field_name("arguments") {
                let mut cursor = args.walk();
                for arg in args.named_children(&mut cursor) {
                    walk_expr_sites(arg, on_name, on_call, on_new);
                }
            }
        }
        "binary_expression" => {
            if let Some(l) = node.child_by_field_name("left") {
                walk_expr_sites(l, on_name, on_call, on_new);
            }
            if let Some(r) = node.child_by_field_name("right") {
                walk_expr_sites(r, on_name, on_call, on_new);
            }
        }
        "unary_expression" => {
            if let Some(operand) = node.child_by_field_name("argument") {
                walk_expr_sites(operand, on_name, on_call, on_new);
            }
        }
        "parenthesized_expression" => {
            if let Some(inner) = node.named_child(0) {
                walk_expr_sites(inner, on_name, on_call, on_new);
            }
        }
        "subscript_expression" => {
            if let Some(object) = node.child_by_field_name("object") {
                walk_expr_sites(object, on_name, on_call, on_new);
            }
            if let Some(index) = node.child_by_field_name("index") {
                walk_expr_sites(index, on_name, on_call, on_new);
            }
        }
        "assignment_expression" => {
            if let Some(right) = node.child_by_field_name("right") {
                walk_expr_sites(right, on_name, on_call, on_new);
            }
        }
        _ => {}
    }
}

fn walk_stmt_sites<'a>(
    node: Node<'a>,
    on_name: &mut impl FnMut(Node<'a>),
    on_call: &mut impl FnMut(Node<'a>),
    on_new: &mut impl FnMut(Node<'a>),
) {
    match node.kind() {
        "expression_statement" => {
            if let Some(inner) = node.named_child(0) {
                walk_expr_sites(inner, on_name, on_call, on_new);
            }
        }
        "return_statement" => {
            if let Some(inner) = node.named_child(0) {
                walk_expr_sites(inner, on_name, on_call, on_new);
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            let mut cursor = node.walk();
            for declarator in node.named_children(&mut cursor) {
                if declarator.kind() != "variable_declarator" {
                    continue;
                }
                if let Some(value) = declarator.child_by_field_name("value") {
                    walk_expr_sites(value, on_name, on_call, on_new);
                }
            }
        }
        "if_statement" => {
            if let Some(cond) = node.child_by_field_name("condition") {
                walk_expr_sites(cond, on_name, on_call, on_new);
            }
            if let Some(consequence) = node.child_by_field_name("consequence") {
                if consequence.kind() == "statement_block" {
                    walk_block_statements(consequence, on_name, on_call, on_new);
                } else {
                    walk_stmt_sites(consequence, on_name, on_call, on_new);
                }
            }
            if let Some(alternative) = node.child_by_field_name("alternative") {
                if alternative.kind() == "statement_block" {
                    walk_block_statements(alternative, on_name, on_call, on_new);
                } else {
                    walk_stmt_sites(alternative, on_name, on_call, on_new);
                }
            }
        }
        "for_statement" => {
            if let Some(body) = node.child_by_field_name("body") {
                if body.kind() == "statement_block" {
                    walk_block_statements(body, on_name, on_call, on_new);
                } else {
                    walk_stmt_sites(body, on_name, on_call, on_new);
                }
            }
        }
        "statement_block" => walk_block_statements(node, on_name, on_call, on_new),
        _ => {}
    }
}

fn walk_block_statements<'a>(
    block: Node<'a>,
    on_name: &mut impl FnMut(Node<'a>),
    on_call: &mut impl FnMut(Node<'a>),
    on_new: &mut impl FnMut(Node<'a>),
) {
    let mut cursor = block.walk();
    for stmt in block.named_children(&mut cursor) {
        walk_stmt_sites(stmt, on_name, on_call, on_new);
    }
}

/// `EXTENDS`/`IMPLEMENTS` edges from every class/interface declared in
/// *this* file's `sites`. TypeScript, unlike Go, has explicit
/// `extends`/`implements` keywords -- those are honored directly by name
/// lookup. But a class can also satisfy an interface purely structurally
/// (no `implements` clause at all), the same way a Go struct does -- so
/// when a class has no explicit `implements` clause, every interface in the
/// repository whose full method set the class's own methods are a superset
/// of still gets an `IMPLEMENTS` edge. Both forms coexist, per Phase 17.2's
/// task list.
fn resolve_heritage(
    sites: &ResolutionSites,
    all_entities: &[SyntaxEntity],
    edges_so_far: &[Edge],
    symbol_table: &SymbolTable,
    src: &str,
) -> Vec<Edge> {
    let mut edges = Vec::new();

    let interfaces: Vec<&SyntaxEntity> = all_entities
        .iter()
        .filter(|e| e.kind == SyntaxKind::Interface)
        .collect();
    let classes: Vec<&SyntaxEntity> = all_entities
        .iter()
        .filter(|e| e.kind == SyntaxKind::Class)
        .collect();

    for (class_entity, node) in &sites.classes {
        let Some(class_symbol) = symbol_table.find_by_syntax_id(&class_entity.id) else {
            continue;
        };

        let heritage = find_child(*node, "class_heritage");
        let mut has_explicit_implements = false;

        if let Some(heritage) = heritage {
            let mut cursor = heritage.walk();
            for clause in heritage.children(&mut cursor) {
                match clause.kind() {
                    "extends_clause" => {
                        if let Some(value) = clause.child_by_field_name("value") {
                            let ident = base_identifier_text(value, src);
                            if let Some(target) = classes.iter().find(|c| c.name == ident) {
                                if let Some(target_symbol) =
                                    symbol_table.find_by_syntax_id(&target.id)
                                {
                                    edges.push(Edge::new(
                                        class_symbol.id.clone(),
                                        target_symbol.id.clone(),
                                        EdgeKind::Extends,
                                    ));
                                }
                            }
                        }
                    }
                    "implements_clause" => {
                        has_explicit_implements = true;
                        let mut type_cursor = clause.walk();
                        for type_node in clause.named_children(&mut type_cursor) {
                            let ident = base_identifier_text(type_node, src);
                            if let Some(target) = interfaces.iter().find(|i| i.name == ident) {
                                if let Some(target_symbol) =
                                    symbol_table.find_by_syntax_id(&target.id)
                                {
                                    edges.push(Edge::new(
                                        class_symbol.id.clone(),
                                        target_symbol.id.clone(),
                                        EdgeKind::Implements,
                                    ));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if has_explicit_implements {
            continue;
        }

        let class_methods: HashSet<&str> = declared_children(edges_so_far, &class_entity.id)
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

            if required_methods.is_empty() || !required_methods.is_subset(&class_methods) {
                continue;
            }

            if let Some(interface_symbol) = symbol_table.find_by_syntax_id(&interface_entity.id) {
                edges.push(Edge::new(
                    class_symbol.id.clone(),
                    interface_symbol.id.clone(),
                    EdgeKind::Implements,
                ));
            }
        }
    }

    for (interface_entity, node) in &sites.interfaces {
        let Some(interface_symbol) = symbol_table.find_by_syntax_id(&interface_entity.id) else {
            continue;
        };
        let Some(extends_clause) = find_child(*node, "extends_type_clause") else {
            continue;
        };
        let mut cursor = extends_clause.walk();
        for type_node in extends_clause.named_children(&mut cursor) {
            let ident = base_identifier_text(type_node, src);
            if let Some(target) = interfaces.iter().find(|i| i.name == ident) {
                if let Some(target_symbol) = symbol_table.find_by_syntax_id(&target.id) {
                    edges.push(Edge::new(
                        interface_symbol.id.clone(),
                        target_symbol.id.clone(),
                        EdgeKind::Extends,
                    ));
                }
            }
        }
    }

    edges
}

/// The base identifier text of a heritage clause's value: `Foo` for a bare
/// `identifier`, or the `name` field's text for a `generic_type` (`Foo<T>`)
/// -- so `extends Base<T>`/`implements Shape<T>` still resolve `Base`/
/// `Shape` by name, ignoring the type arguments (Codeate doesn't yet
/// specialize generic instantiations at resolution time, a documented gap
/// matching the other adapters' own generic-parameter gaps).
fn base_identifier_text<'a>(node: Node<'a>, src: &'a str) -> &'a str {
    match node.kind() {
        "generic_type" => node
            .child_by_field_name("name")
            .map(|n| text(n, src))
            .unwrap_or_else(|| text(node, src)),
        _ => text(node, src),
    }
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

/// Resolve a file's `import_statement`s into `IMPORTS` edges from
/// `module_symbol`. A relative specifier (`./shape`, `../lib/shape`) is
/// resolvable against this repository's own files in principle, but doing
/// so needs path resolution against the repository root that isn't
/// available at this adapter's boundary -- out of scope for this
/// sub-phase, matching Go's own import-path gap. Every import source
/// resolves to an explicit `external-module` placeholder instead of being
/// guessed at.
fn resolve_imports(
    imports: &[Node],
    src: &str,
    module_symbol: &SymbolEntity,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for import_stmt in imports {
        let Some(source_node) = import_stmt.child_by_field_name("source") else {
            continue;
        };
        let source = text(source_node, src).trim_matches(|c| c == '"' || c == '\'');
        let placeholder = unresolved_symbol("external-module", source);
        symbols.push(placeholder.clone());
        edges.push(Edge::new(
            module_symbol.id.clone(),
            placeholder.id,
            EdgeKind::Imports,
        ));
    }

    (symbols, edges)
}

/// Resolve `sites.exported_names` (populated as `export`-keyword
/// declarations/clauses were encountered during extraction) into `EXPORTS`
/// edges from `module_symbol`. Unlike Go's uppercase-name convention,
/// TypeScript's export surface is the `export` keyword itself, so this
/// matches by name against this file's own top-level declared entities
/// rather than inferring from casing.
fn resolve_exports(
    sites: &ResolutionSites,
    file_entities: &[SyntaxEntity],
    module_symbol: &SymbolEntity,
    symbol_table: &SymbolTable,
) -> Vec<Edge> {
    let mut edges = Vec::new();
    for name in &sites.exported_names {
        let Some(entity) = file_entities.iter().find(|e| {
            e.id != file_entities[0].id
                && &e.name == name
                && matches!(
                    e.kind,
                    SyntaxKind::Function | SyntaxKind::Class | SyntaxKind::Interface | SyntaxKind::Variable
                )
        }) else {
            continue;
        };
        if let Some(symbol) = symbol_table.find_by_syntax_id(&entity.id) {
            let edge = Edge::new(module_symbol.id.clone(), symbol.id.clone(), EdgeKind::Exports);
            if !edges.contains(&edge) {
                edges.push(edge);
            }
        }
    }
    edges
}

/// Map a TypeScript type expression to a `TypeRef`. Not yet wired into
/// `extract`/`resolve_file` -- `ACCEPTS`/`RETURNS` type binding isn't
/// called from the pipeline, mirroring Go's own `extract_type` gap (tested
/// directly, deliberately not yet wired in either). `union_type`/
/// `intersection_type` map onto the canonical model's `TypeRef::Union`/
/// `TypeRef::Intersection` -- added to `types.rs` by this sub-phase, since
/// neither existed before TypeScript needed them (a genuine canonical-model
/// gap, not a workaround folded into this adapter).
#[allow(dead_code)]
fn extract_type(node: Node, src: &str) -> TypeRef {
    match node.kind() {
        "predefined_type" => {
            let name = text(node, src);
            TypeRef::Primitive(name.to_string())
        }
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
        "generic_type" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| text(n, src).to_string())
                .unwrap_or_else(|| text(node, src).to_string());
            let type_args = node
                .child_by_field_name("type_arguments")
                .map(|args| {
                    let mut cursor = args.walk();
                    args.named_children(&mut cursor)
                        .map(|arg| extract_type(arg, src))
                        .collect()
                })
                .unwrap_or_default();
            TypeRef::Named { name, type_args }
        }
        "union_type" => {
            let mut cursor = node.walk();
            let members = node
                .named_children(&mut cursor)
                .map(|member| extract_type(member, src))
                .collect();
            TypeRef::Union(members)
        }
        "intersection_type" => {
            let mut cursor = node.walk();
            let members = node
                .named_children(&mut cursor)
                .map(|member| extract_type(member, src))
                .collect();
            TypeRef::Intersection(members)
        }
        "type_annotation" => node
            .named_child(0)
            .map(|inner| extract_type(inner, src))
            .unwrap_or_else(|| TypeRef::Unresolved(text(node, src).to_string())),
        _ => TypeRef::Unresolved(text(node, src).to_string()),
    }
}

/// `ACCEPTS`/`RETURNS` edges for a function/method signature. Not yet wired
/// into `extract`/`resolve_file`, matching `extract_type`'s own gap above.
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
        for param in params.named_children(&mut cursor) {
            if param.kind() != "required_parameter" && param.kind() != "optional_parameter" {
                continue;
            }
            let Some(type_annotation) = param.child_by_field_name("type") else {
                continue;
            };
            let type_ref = extract_type(type_annotation, src);
            let type_symbol = type_symbol_for_ref(&type_ref);
            edges.push(Edge::new(
                fn_symbol.id.clone(),
                type_symbol.id.clone(),
                EdgeKind::Accepts,
            ));
            symbols.push(type_symbol);
        }
    }

    match node.child_by_field_name("return_type") {
        Some(return_type) => {
            let type_ref = extract_type(return_type, src);
            let type_symbol = type_symbol_for_ref(&type_ref);
            edges.push(Edge::new(
                fn_symbol.id.clone(),
                type_symbol.id.clone(),
                EdgeKind::Returns,
            ));
            symbols.push(type_symbol);
        }
        None => {
            let type_symbol = type_symbol_for_ref(&TypeRef::Primitive("void".to_string()));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
        let adapter = TypeScriptAdapter;
        let ast = adapter.parse(source).expect("valid typescript parses");
        adapter.extract(&ast, "module.ts")
    }

    #[test]
    fn parses_valid_typescript_source() {
        let adapter = TypeScriptAdapter;
        assert!(adapter.parse("function main(): void {}\n").is_ok());
    }

    #[test]
    fn extract_always_returns_the_file_module_first() {
        let (entities, _) = extract("const x = 1;\n");
        assert_eq!(entities[0].kind, SyntaxKind::Module);
        assert_eq!(entities[0].name, "module.ts");
    }

    #[test]
    fn extracts_a_top_level_function_with_its_parameters() {
        let (entities, edges) = extract("function add(a: number, b: number): number {\n  return a + b;\n}\n");

        let module = &entities[0];
        let function = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "add")
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
    fn extracts_a_class_and_a_method_declared_on_it() {
        let source = "class Circle {\n  radius: number;\n  area(): number {\n    return this.radius;\n  }\n}\n";
        let (entities, edges) = extract(source);

        let module = &entities[0];
        let circle = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Circle")
            .expect("Class entity");
        assert!(edges.contains(&Edge::new(
            module.id.clone(),
            circle.id.clone(),
            EdgeKind::Declares
        )));

        let area = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "area")
            .expect("Method entity");
        assert!(
            edges.contains(&Edge::new(circle.id.clone(), area.id.clone(), EdgeKind::Declares)),
            "area should be declared under Circle, not the module"
        );
    }

    #[test]
    fn extracts_an_interface_with_its_method_signatures() {
        let source = "interface Shape {\n  area(): number;\n}\n";
        let (entities, edges) = extract(source);

        let shape = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Interface && e.name == "Shape")
            .expect("Interface entity");
        let area = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "area")
            .expect("Method signature entity");
        assert!(edges.contains(&Edge::new(shape.id.clone(), area.id.clone(), EdgeKind::Declares)));
    }

    #[test]
    fn extracts_a_lexical_declaration_in_a_function_body_as_a_variable() {
        let (entities, edges) = extract("function run(): void {\n  const total = 1;\n}\n");

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
    fn extracts_a_top_level_const_declaration() {
        let (entities, _) = extract("const counter: number = 0;\n");
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Variable && e.name == "counter"));
    }

    #[test]
    fn a_non_declaration_statement_becomes_a_generic_statement_entity() {
        let (entities, _) = extract("function run(): void {\n  console.log(1);\n}\n");
        assert!(entities.iter().any(|e| e.kind == SyntaxKind::Statement));
    }

    #[test]
    fn a_parameter_with_a_rest_pattern_is_extracted_by_its_bound_name() {
        let (entities, _) = extract("function collect(...items: number[]): void {}\n");
        let param_names: Vec<&str> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Parameter)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(param_names, vec!["items"]);
    }

    #[test]
    fn an_exported_function_is_extracted_the_same_as_an_unexported_one() {
        let (entities, edges) = extract("export function greet(): void {}\n");
        let module = &entities[0];
        let function = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "greet")
            .expect("Function entity");
        assert!(edges.contains(&Edge::new(
            module.id.clone(),
            function.id.clone(),
            EdgeKind::Declares
        )));
    }

    // -- type extraction (not yet wired into extract/resolve_file) --

    fn parse_source(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn find_matching<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        let mut cursor = tree.root_node().walk();
        let found = tree.root_node().named_children(&mut cursor).find(|c| {
            c.kind() == "function_declaration"
                && c.child_by_field_name("name")
                    .is_some_and(|n| text(n, src) == name)
        });
        found.unwrap_or_else(|| panic!("no function named {name}"))
    }

    fn return_type_of<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        find_matching(tree, src, name)
            .child_by_field_name("return_type")
            .unwrap()
            .named_child(0)
            .unwrap()
    }

    #[test]
    fn extracts_a_primitive_type() {
        let full = "function f(): number { return 0; }\n";
        let tree = parse_source(full);
        let node = return_type_of(&tree, full, "f");
        assert_eq!(extract_type(node, full), TypeRef::Primitive("number".to_string()));
    }

    #[test]
    fn extracts_a_user_defined_named_type() {
        let full = "function f(): Circle { return new Circle(); }\n";
        let tree = parse_source(full);
        let node = return_type_of(&tree, full, "f");
        assert_eq!(
            extract_type(node, full),
            TypeRef::Named {
                name: "Circle".to_string(),
                type_args: Vec::new(),
            }
        );
    }

    #[test]
    fn extracts_a_generic_named_type_with_its_type_argument() {
        let full = "function f(): Array<number> { return []; }\n";
        let tree = parse_source(full);
        let node = return_type_of(&tree, full, "f");
        assert_eq!(
            extract_type(node, full),
            TypeRef::Named {
                name: "Array".to_string(),
                type_args: vec![TypeRef::Primitive("number".to_string())],
            }
        );
    }

    #[test]
    fn extracts_a_union_type() {
        let full = "function f(): string | number { return 0; }\n";
        let tree = parse_source(full);
        let node = return_type_of(&tree, full, "f");
        assert_eq!(
            extract_type(node, full),
            TypeRef::Union(vec![
                TypeRef::Primitive("string".to_string()),
                TypeRef::Primitive("number".to_string()),
            ])
        );
    }

    #[test]
    fn extracts_an_intersection_type() {
        let full = "function f(): A & B { return x; }\n";
        let tree = parse_source(full);
        let node = return_type_of(&tree, full, "f");
        assert_eq!(
            extract_type(node, full),
            TypeRef::Intersection(vec![
                TypeRef::Named {
                    name: "A".to_string(),
                    type_args: Vec::new(),
                },
                TypeRef::Named {
                    name: "B".to_string(),
                    type_args: Vec::new(),
                },
            ])
        );
    }

    #[test]
    fn binds_a_return_type_via_returns() {
        let full = "function make(): Circle { return new Circle(); }\n";
        let tree = parse_source(full);
        let node = find_matching(&tree, full, "make");
        let fn_symbol = SymbolEntity::new(SymbolKind::Function, "module.ts", "make");

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
    fn a_signature_with_no_return_type_binds_returns_to_void() {
        let full = "function run() {}\n";
        let tree = parse_source(full);
        let node = find_matching(&tree, full, "run");
        let fn_symbol = SymbolEntity::new(SymbolKind::Function, "module.ts", "run");

        let (_symbols, edges) = extract_signature_types(node, full, &fn_symbol);

        let void_type = crate::types::type_symbol_for(&TypeRef::Primitive("void".to_string()));
        assert!(edges.contains(&Edge::new(
            fn_symbol.id.clone(),
            void_type.id.clone(),
            EdgeKind::Returns
        )));
    }

    #[test]
    fn binds_parameter_types_via_accepts() {
        let full = "function add(a: number, b: number): number { return a + b; }\n";
        let tree = parse_source(full);
        let node = find_matching(&tree, full, "add");
        let fn_symbol = SymbolEntity::new(SymbolKind::Function, "module.ts", "add");

        let (_symbols, edges) = extract_signature_types(node, full, &fn_symbol);

        let number_type = crate::types::type_symbol_for(&TypeRef::Primitive("number".to_string()));
        let accepts_count = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Accepts && e.to == number_type.id)
            .count();
        assert_eq!(accepts_count, 2, "both a and b are number");
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
        let adapter = TypeScriptAdapter;
        let ast = adapter.parse(source).expect("valid typescript parses");
        let (entities, edges) = adapter.extract(&ast, "module.ts");
        let module = entities[0].clone();
        let fn_entity = entities
            .iter()
            .find(|e| {
                (e.kind == SyntaxKind::Function || e.kind == SyntaxKind::Method) && e.name == fn_name
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
        find_matching(tree, src, name).child_by_field_name("body").unwrap()
    }

    #[test]
    fn resolves_a_reference_to_an_in_scope_variable() {
        let source = "function run(): void {\n  const value = 1;\n  console.log(value);\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) = resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

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
        let source = "function run(): void {\n  console.log(missing);\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (symbols, edges) =
            resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        assert!(symbols.iter().any(|s| s.name.starts_with("unknown-local")));
        assert!(edges
            .iter()
            .any(|e| e.kind == EdgeKind::References && e.from == r.fn_symbol.id));
    }

    #[test]
    fn resolves_a_free_function_call() {
        let source = "function helper(): void {}\n\nfunction run(): void {\n  helper();\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) = resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

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
    fn resolves_a_method_call_by_best_effort_name_match() {
        let source = "class Shape {\n  area(): void {}\n}\n\nfunction run(shape: Shape): void {\n  shape.area();\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) = resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        let area = r
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "area")
            .unwrap();
        let area_symbol = r.symbol_table.find_by_syntax_id(&area.id).unwrap();
        assert!(edges.contains(&Edge::new(
            r.fn_symbol.id.clone(),
            area_symbol.id.clone(),
            EdgeKind::Calls
        )));
    }

    #[test]
    fn resolves_a_new_expression_instantiation() {
        let source = "class Circle {}\n\nfunction run(): void {\n  const c = new Circle();\n  console.log(c);\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) = resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        let circle = r
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Circle")
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
        let source = "function run(): void {\n  foo()();\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (symbols, edges) =
            resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        assert!(symbols.iter().any(|s| s.name.starts_with("unsupported-form")));
        assert!(edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.from == r.fn_symbol.id));
    }

    #[test]
    fn a_class_implementing_every_interface_method_structurally_gets_an_implements_edge() {
        let source =
            "interface Shape {\n  area(): number;\n}\n\nclass Circle {\n  area(): number {\n    return 0;\n  }\n}\n";
        let adapter = TypeScriptAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, edges) = adapter.extract(&ast, "module.ts");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "module.ts",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );

        let implements_edges =
            resolve_heritage(&sites, &entities, &edges, &symbol_table, &ast.source);

        let circle = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Circle")
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
    fn a_class_missing_a_required_method_gets_no_structural_implements_edge() {
        let source = "interface Shape {\n  area(): number;\n  perimeter(): number;\n}\n\nclass Circle {\n  area(): number {\n    return 0;\n  }\n}\n";
        let adapter = TypeScriptAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, edges) = adapter.extract(&ast, "module.ts");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "module.ts",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );

        let implements_edges =
            resolve_heritage(&sites, &entities, &edges, &symbol_table, &ast.source);
        assert!(!implements_edges
            .iter()
            .any(|e| e.kind == EdgeKind::Implements));
    }

    #[test]
    fn an_explicit_implements_clause_produces_an_implements_edge() {
        let source =
            "interface Shape {\n  area(): number;\n}\n\nclass Circle implements Shape {\n  area(): number {\n    return 0;\n  }\n}\n";
        let adapter = TypeScriptAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, edges) = adapter.extract(&ast, "module.ts");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "module.ts",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );

        let implements_edges =
            resolve_heritage(&sites, &entities, &edges, &symbol_table, &ast.source);

        let circle = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Circle")
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
    fn a_class_extending_another_class_gets_an_extends_edge() {
        let source = "class Base {}\n\nclass Derived extends Base {}\n";
        let adapter = TypeScriptAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, edges) = adapter.extract(&ast, "module.ts");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "module.ts",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );

        let heritage_edges = resolve_heritage(&sites, &entities, &edges, &symbol_table, &ast.source);

        let base = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Base")
            .unwrap();
        let derived = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Derived")
            .unwrap();
        let base_symbol = symbol_table.find_by_syntax_id(&base.id).unwrap();
        let derived_symbol = symbol_table.find_by_syntax_id(&derived.id).unwrap();
        assert!(heritage_edges.contains(&Edge::new(
            derived_symbol.id.clone(),
            base_symbol.id.clone(),
            EdgeKind::Extends
        )));
    }

    #[test]
    fn resolves_imports_to_an_explicit_external_module_placeholder() {
        let source = "import { Shape } from \"./shape\";\nimport fs from \"fs\";\n\nfunction run(): void {\n  console.log(Shape, fs);\n}\n";
        let adapter = TypeScriptAdapter;
        let ast = adapter.parse(source).unwrap();
        let file_module = SyntaxEntity::new(SyntaxKind::Module, "module.ts", "module.ts");
        let mut file_entities = vec![file_module.clone()];
        let mut file_edges = Vec::new();
        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &file_module,
            "module.ts",
            &mut file_entities,
            &mut file_edges,
            &mut sites,
        );
        let module_symbol = SymbolEntity::new(SymbolKind::Namespace, "module.ts", "module.ts");

        let (symbols, edges) = resolve_imports(&sites.imports, &ast.source, &module_symbol);

        assert_eq!(symbols.len(), 2);
        assert!(symbols.iter().any(|s| s.name == "external-module:./shape"));
        assert!(symbols.iter().any(|s| s.name == "external-module:fs"));
        assert_eq!(
            edges.iter().filter(|e| e.kind == EdgeKind::Imports).count(),
            2
        );
    }

    #[test]
    fn exports_an_explicitly_exported_declaration_but_not_an_unexported_one() {
        let source = "export function pub(): void {}\nfunction priv(): void {}\n";
        let adapter = TypeScriptAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "module.ts");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table.find_by_syntax_id(&entities[0].id).unwrap().clone();
        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "module.ts",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );

        let export_edges = resolve_exports(&sites, &entities, &module_symbol, &symbol_table);

        let public = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "pub")
            .unwrap();
        let public_symbol = symbol_table.find_by_syntax_id(&public.id).unwrap();
        assert_eq!(export_edges.len(), 1);
        assert!(export_edges.contains(&Edge::new(
            module_symbol.id.clone(),
            public_symbol.id.clone(),
            EdgeKind::Exports
        )));
    }

    #[test]
    fn a_named_export_clause_also_produces_an_exports_edge() {
        let source = "function pub(): void {}\nexport { pub };\n";
        let adapter = TypeScriptAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "module.ts");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table.find_by_syntax_id(&entities[0].id).unwrap().clone();
        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "module.ts",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );

        let export_edges = resolve_exports(&sites, &entities, &module_symbol, &symbol_table);
        assert_eq!(export_edges.len(), 1);
    }
}
