use tree_sitter::{Node, Parser, Tree};

use crate::adapter::LanguageAdapter;
use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};
use crate::resolve::unresolved_symbol;
use crate::symbols::{ScopeTree, SymbolTable};

/// The parsed form a `JavaScriptAdapter` produces: `tree-sitter`'s `Tree`
/// paired with the source text it was parsed from, mirroring `GoAst` (see
/// `adapters::go`'s own doc comment on why `tree-sitter`-based adapters need
/// this pairing, unlike `syn`/`rustpython-parser`'s self-contained typed
/// trees).
pub struct JavaScriptAst {
    tree: Tree,
    source: String,
}

/// The JavaScript language adapter: parses JavaScript source via
/// `tree-sitter`/`tree-sitter-javascript` and maps it onto the canonical
/// syntax model.
///
/// JavaScript is dynamically typed -- there is no annotation syntax to
/// extract a `TypeRef` from at all (unlike Go's/Python's optional/inferred
/// static typing, which at least has *some* surface syntax to read). So,
/// unlike `adapters::go`/`adapters::python`, this adapter deliberately has no
/// `extract_type`/`extract_signature_types`: there is nothing honest to
/// extract, and fabricating `TypeRef::Unresolved` placeholders for every
/// value in the program would add noise, not information, to the canonical
/// model. `ACCEPTS`/`RETURNS` edges are simply never produced by this
/// adapter -- a documented, deliberate no-op, not an oversight.
///
/// Prototypal inheritance in general (arbitrary `Object.setPrototypeOf`/
/// `Object.create` chains, function-based "classes" via
/// `Constructor.prototype.method = ...`) is out of scope: only the ES6
/// `class ... extends ...` keyword form is resolved into an `EXTENDS` edge.
/// JavaScript has no `interface`/`implements` keyword (that's TypeScript-
/// only), so this adapter never produces `IMPLEMENTS` edges.
pub struct JavaScriptAdapter;

impl LanguageAdapter for JavaScriptAdapter {
    type Ast = JavaScriptAst;

    fn parse(&self, source: &str) -> Result<Self::Ast> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_javascript::LANGUAGE.into())
            .map_err(|err| Error::Message(format!("failed to load JavaScript grammar: {err}")))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::Message("failed to parse JavaScript source".to_string()))?;
        Ok(JavaScriptAst {
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
        // `EntityId`) ones `extract` already handed the pipeline -- mirrors
        // `adapters::go::resolve_file`'s own comment on why this matters for
        // `symbol_table.find_by_syntax_id` below.
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

        for class_node in &sites.classes {
            let (extends_symbols, extends_edges) =
                resolve_extends(*class_node, &ast.source, all_entities, symbol_table);
            symbols.extend(extends_symbols);
            edges.extend(extends_edges);
        }

        let (import_symbols, import_edges) =
            resolve_imports(&sites, &ast.source, module_symbol, all_entities, symbol_table);
        symbols.extend(import_symbols);
        edges.extend(import_edges);

        let (export_symbols, export_edges) = resolve_exports(
            &sites,
            &ast.source,
            module_symbol,
            &file_entities,
            symbol_table,
        );
        symbols.extend(export_symbols);
        edges.extend(export_edges);

        (symbols, edges)
    }
}

/// Sites `extract_items` collects that `resolve_file` needs the raw tree
/// for but that have no place in `extract`'s plain `(Vec<SyntaxEntity>,
/// Vec<Edge>)`: a function/method's own entity paired with its body block, a
/// class declaration (for `EXTENDS`), every ES module `import_statement`,
/// every `export_statement`, and every CommonJS `module.exports`/
/// `exports.x` assignment. Mirrors `adapters::go::ResolutionSites`.
#[derive(Default)]
struct ResolutionSites<'a> {
    bodies: Vec<(SyntaxEntity, Node<'a>)>,
    classes: Vec<Node<'a>>,
    imports: Vec<Node<'a>>,
    exports: Vec<Node<'a>>,
    commonjs_exports: Vec<Node<'a>>,
    /// Each top-level `const/let/var <name> = require("<path>")` (or
    /// destructured `const { a, b } = require("<path>")`) declarator,
    /// paired with the string node holding `require`'s path argument.
    requires: Vec<(Node<'a>, Node<'a>)>,
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
    edges.push(Edge::new(parent.id.clone(), child.id.clone(), EdgeKind::Contains));
    edges.push(Edge::new(parent.id.clone(), child.id.clone(), EdgeKind::Declares));
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
    edges.push(Edge::new(parent.id.clone(), child.id.clone(), EdgeKind::Contains));
    entities.push(child);
}

/// The `class_heritage` child of a `class_declaration`, if any -- unlike
/// most fields in this grammar, `class_heritage` isn't exposed via
/// `child_by_field_name` (the grammar wraps it in `optional(...)` without a
/// `field()`), so it's found by scanning direct children instead.
fn class_heritage(class_node: Node) -> Option<Node> {
    let mut cursor = class_node.walk();
    let found = class_node
        .children(&mut cursor)
        .find(|c| c.kind() == "class_heritage");
    found
}

/// Walk a `program`'s (or, recursively, nothing deeper -- JavaScript has no
/// Go-style declaration-order concern since methods always nest directly
/// inside their `class_body`, not standalone with a receiver) direct
/// children, extracting top-level declarations and populating `sites` for
/// `resolve_file` in the same pass, so the entities `sites` carries are
/// identical (by `EntityId`) to the ones pushed into `entities`.
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
        extract_top_level_statement(child, src, parent, scope, entities, edges, sites);
    }
}

/// Extract one direct child of `program` (or of an `export_statement`'s own
/// inlined declaration -- `export function foo() {}` both declares `foo`
/// and exports it, so the declaration side is handled by recursing back
/// through this same function).
#[allow(clippy::too_many_arguments)]
fn extract_top_level_statement<'a>(
    node: Node<'a>,
    src: &'a str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    match node.kind() {
        "class_declaration" => extract_class(node, src, parent, scope, entities, edges, sites),
        "function_declaration" | "generator_function_declaration" => {
            extract_function(node, src, parent, scope, entities, edges, sites)
        }
        "lexical_declaration" | "variable_declaration" => {
            extract_top_level_variables(node, src, parent, scope, entities, edges);
            collect_require_sites(node, src, sites);
        }
        "import_statement" => sites.imports.push(node),
        "export_statement" => {
            sites.exports.push(node);
            if let Some(declaration) = node.child_by_field_name("declaration") {
                extract_top_level_statement(
                    declaration,
                    src,
                    parent,
                    scope,
                    entities,
                    edges,
                    sites,
                );
            }
        }
        "expression_statement" => {
            if let Some(assignment) = commonjs_export_assignment(node, src) {
                sites.commonjs_exports.push(assignment);
            }
        }
        _ => {}
    }
}

/// A CommonJS `module.exports = ...` / `module.exports.x = ...` /
/// `exports.x = ...` assignment at the top of an `expression_statement`, if
/// `node` is one. Returns the `assignment_expression` node itself.
fn commonjs_export_assignment<'a>(node: Node<'a>, src: &str) -> Option<Node<'a>> {
    let assignment = node.named_child(0).filter(|n| n.kind() == "assignment_expression")?;
    let left = assignment.child_by_field_name("left")?;
    if is_commonjs_export_target(left, src) {
        Some(assignment)
    } else {
        None
    }
}

fn is_identifier_named(node: Node, name: &str, src: &str) -> bool {
    node.kind() == "identifier" && text(node, src) == name
}

/// `module.exports` exactly: a member expression whose object is the bare
/// identifier `module` and whose property is `exports`.
fn is_module_exports(node: Node, src: &str) -> bool {
    node.kind() == "member_expression"
        && node.child_by_field_name("object").is_some_and(|o| is_identifier_named(o, "module", src))
        && node
            .child_by_field_name("property")
            .is_some_and(|p| text(p, src) == "exports")
}

/// Whether `node` (an assignment's left-hand side) is `module.exports`,
/// `module.exports.<name>`, or `exports.<name>`.
fn is_commonjs_export_target(node: Node, src: &str) -> bool {
    if is_module_exports(node, src) {
        return true;
    }
    if node.kind() != "member_expression" {
        return false;
    }
    let Some(object) = node.child_by_field_name("object") else {
        return false;
    };
    // `exports.<name> = ...`
    if is_identifier_named(object, "exports", src) {
        return true;
    }
    // `module.exports.<name> = ...`
    is_module_exports(object, src)
}

fn extract_top_level_variables(
    node: Node,
    src: &str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    for name_node in declarator_names(node) {
        let entity = SyntaxEntity::new(SyntaxKind::Variable, scope, text(name_node, src));
        contain(parent, entity, entities, edges);
    }
}

/// Every `variable_declarator`'s plain `identifier` name directly under a
/// `lexical_declaration`/`variable_declaration` node. A destructuring
/// pattern (`const { a, b } = ...`) doesn't bind a single name here -- a
/// documented gap, not guessed at (mirrors Go's unnamed-parameter gap and
/// Python's assignment-target gap).
fn declarator_names(node: Node) -> Vec<Node> {
    let mut names = Vec::new();
    let mut cursor = node.walk();
    for declarator in node.children(&mut cursor) {
        if declarator.kind() != "variable_declarator" {
            continue;
        }
        if let Some(name) = declarator.child_by_field_name("name") {
            if name.kind() == "identifier" {
                names.push(name);
            }
        }
    }
    names
}

/// Record each `require("<path>")` call at the top level of a
/// `lexical_declaration`/`variable_declaration`'s own declarator values
/// (`const shape = require("./shape")`, `const { a, b } = require("./x")`)
/// into `sites.requires`, paired with the require call's string argument.
/// The declared binding itself is already extracted as an ordinary
/// `Variable` entity by `extract_top_level_variables` -- this only records
/// the extra information `resolve_imports` needs to also treat it as an
/// import site.
fn collect_require_sites<'a>(node: Node<'a>, src: &str, sites: &mut ResolutionSites<'a>) {
    let mut cursor = node.walk();
    for declarator in node.children(&mut cursor) {
        if declarator.kind() != "variable_declarator" {
            continue;
        }
        let Some(value) = declarator.child_by_field_name("value") else {
            continue;
        };
        if let Some(path_node) = require_call_path(value, src) {
            sites.requires.push((declarator, path_node));
        }
    }
}

/// If `node` is a `require("<path>")` call expression (exactly one string
/// argument, callee named literally `require`), the `string` node holding
/// its path argument.
fn require_call_path<'a>(node: Node<'a>, src: &str) -> Option<Node<'a>> {
    if node.kind() != "call_expression" {
        return None;
    }
    let func = node.child_by_field_name("function")?;
    if func.kind() != "identifier" || text(func, src) != "require" {
        return None;
    }
    let args = node.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let mut named = args.named_children(&mut cursor);
    let first = named.next()?;
    if named.next().is_some() {
        return None; // more than one argument -- not a plain require(path)
    }
    if first.kind() != "string" {
        return None;
    }
    Some(first)
}

/// An ES6 `class` declaration becomes a `Class` entity; its `method_definition`
/// members become `Method` entities declared under it (computed method names
/// -- `[Symbol.iterator]() {}` -- and class fields are a documented gap, not
/// extracted).
#[allow(clippy::too_many_arguments)]
fn extract_class<'a>(
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
    let entity = SyntaxEntity::new(SyntaxKind::Class, scope, name);
    let class_scope = child_scope(scope, name);
    declare(parent, entity.clone(), entities, edges);
    sites.classes.push(node);

    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    for member in body.children(&mut cursor) {
        if member.kind() != "method_definition" {
            continue;
        }
        let Some(method_name_node) = member.child_by_field_name("name") else {
            continue;
        };
        if method_name_node.kind() != "property_identifier" {
            continue;
        }
        let method_name = text(method_name_node, src);
        let method_entity = SyntaxEntity::new(SyntaxKind::Method, &class_scope, method_name);
        let method_scope = child_scope(&class_scope, method_name);
        declare(&entity, method_entity.clone(), entities, edges);

        if let Some(params) = member.child_by_field_name("parameters") {
            extract_parameters(params, src, &method_entity, &method_scope, entities, edges);
        }
        if let Some(method_body) = member.child_by_field_name("body") {
            extract_body(method_body, src, &method_entity, &method_scope, entities, edges);
            sites.bodies.push((method_entity, method_body));
        }
    }
}

/// A top-level `function`/`function*` declaration: its own entity,
/// parameters, and body.
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

/// A `formal_parameters`'s plain-identifier and defaulted-identifier
/// (`function f(a, b = 1)`) parameters become `Parameter` entities. A rest
/// parameter (`...rest`) and a destructuring parameter (`{a, b}`/`[a, b]`)
/// don't bind a single name -- a documented gap, matching Go's unnamed-
/// parameter gap and Python's `*args`/`**kwargs` gap.
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
        let name_node = match param.kind() {
            "identifier" => Some(param),
            "assignment_pattern" => param
                .child_by_field_name("left")
                .filter(|n| n.kind() == "identifier"),
            _ => None,
        };
        let Some(name_node) = name_node else {
            continue;
        };
        let entity = SyntaxEntity::new(SyntaxKind::Parameter, scope, text(name_node, src));
        contain(parent, entity, entities, edges);
    }
}

/// Walk a function/method body's (a `statement_block`) direct statements: a
/// `let`/`const`/`var` declaration becomes `Variable` entities; everything
/// else becomes a generic `Statement` entity. A nested `function`/`class`
/// declaration inside a body isn't extracted -- a documented gap, matching
/// Go's/Python's own item-vs-body split. An arrow function assigned to a
/// `const`/`let` (`const f = () => {}`, an extremely common JS pattern) is
/// likewise not extracted as its own `Function` entity -- the binding itself
/// still becomes a `Variable`, but its arrow-function body is not walked
/// into. A documented, deliberate scope limit for this sub-phase, not an
/// oversight.
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
            "function_declaration"
            | "generator_function_declaration"
            | "class_declaration" => {}
            _ => {
                let entity = SyntaxEntity::new(SyntaxKind::Statement, scope, format!("stmt_{index}"));
                contain(parent, entity, entities, edges);
            }
        }
    }
}

/// `REFERENCES`/`CALLS`/`INSTANTIATES` edges from one function/method body.
/// A bare identifier read resolves via `scope_tree`; a bare-name call
/// (`helper()`) resolves the same way; a method call (`obj.method()`)
/// resolves by best-effort name match against any `Method` symbol in the
/// file (full receiver-type dispatch needs cross-file class knowledge, out
/// of scope here, same as Go's/Python's own method-call resolution); a
/// `new X()` whose callee resolves in scope to a `Class` symbol is an
/// `INSTANTIATES` site. Anything unresolved gets an explicit placeholder
/// symbol, never silently dropped.
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
        edges.push(Edge::new(fn_symbol.id.clone(), target.id.clone(), EdgeKind::References));
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
                edges.push(Edge::new(fn_symbol.id.clone(), target.id.clone(), EdgeKind::Calls));
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
                edges.push(Edge::new(fn_symbol.id.clone(), target.id.clone(), EdgeKind::Calls));
            }
            other => {
                let placeholder = unresolved_symbol("unsupported-form", &format!("{other:?}"));
                symbols.push(placeholder.clone());
                edges.push(Edge::new(fn_symbol.id.clone(), placeholder.id.clone(), EdgeKind::Calls));
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
        "new_expression" => {
            on_new(node);
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
        "subscript_expression" => {
            if let Some(object) = node.child_by_field_name("object") {
                walk_expr_sites(object, on_name, on_call, on_new);
            }
            if let Some(index) = node.child_by_field_name("index") {
                walk_expr_sites(index, on_name, on_call, on_new);
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
        "assignment_expression" => {
            // Only the right-hand side is a "read" -- the left-hand side is
            // a binding/write target, mirroring Go's/Python's own
            // assignment handling (right side only).
            if let Some(right) = node.child_by_field_name("right") {
                walk_expr_sites(right, on_name, on_call, on_new);
            }
        }
        "parenthesized_expression" => {
            if let Some(inner) = node.named_child(0) {
                walk_expr_sites(inner, on_name, on_call, on_new);
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
            if let Some(value) = node.named_child(0) {
                walk_expr_sites(value, on_name, on_call, on_new);
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            let mut cursor = node.walk();
            for declarator in node.children(&mut cursor) {
                if declarator.kind() != "variable_declarator" {
                    continue;
                }
                if let Some(value) = declarator.child_by_field_name("value") {
                    walk_expr_sites(value, on_name, on_call, on_new);
                }
            }
        }
        "if_statement" => {
            if let Some(condition) = node.child_by_field_name("condition") {
                if let Some(inner) = condition.named_child(0) {
                    walk_expr_sites(inner, on_name, on_call, on_new);
                }
            }
            if let Some(consequence) = node.child_by_field_name("consequence") {
                walk_stmt_sites(consequence, on_name, on_call, on_new);
            }
            if let Some(alternative) = node.child_by_field_name("alternative") {
                // `alternative` is an `else_clause` wrapping the actual
                // statement (which may itself be another `if_statement` for
                // an `else if` chain).
                if let Some(inner) = alternative.named_child(0) {
                    walk_stmt_sites(inner, on_name, on_call, on_new);
                }
            }
        }
        "for_statement" | "for_in_statement" | "while_statement" | "do_statement" => {
            if let Some(body) = node.child_by_field_name("body") {
                walk_stmt_sites(body, on_name, on_call, on_new);
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

/// `EXTENDS` edges from an ES6 `class ... extends ...` declaration's own
/// `Class` symbol to its superclass. Only the common case -- a bare
/// identifier superclass (`class Circle extends Shape`) -- is resolved; a
/// computed/expression superclass (`class extends mixin(Base)`) is a
/// documented gap, not guessed at. A superclass outside this repository
/// (e.g. a built-in like `Error`) resolves to an explicit `external-class`
/// placeholder, matching `adapters::python::resolve_extends`.
fn resolve_extends(
    class_node: Node,
    src: &str,
    entities: &[SyntaxEntity],
    symbol_table: &SymbolTable,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    let Some(name_node) = class_node.child_by_field_name("name") else {
        return (symbols, edges);
    };
    let class_name = text(name_node, src);
    let Some(class_symbol) = entities
        .iter()
        .find(|e| e.kind == SyntaxKind::Class && e.name == class_name)
        .and_then(|e| symbol_table.find_by_syntax_id(&e.id))
        .cloned()
    else {
        return (symbols, edges);
    };

    let Some(heritage) = class_heritage(class_node) else {
        return (symbols, edges);
    };
    let Some(superclass) = heritage.named_child(0) else {
        return (symbols, edges);
    };
    if superclass.kind() != "identifier" {
        return (symbols, edges);
    }
    let name = text(superclass, src);
    let base_symbol = entities
        .iter()
        .find(|e| e.kind == SyntaxKind::Class && e.name == name)
        .and_then(|e| symbol_table.find_by_syntax_id(&e.id))
        .cloned()
        .unwrap_or_else(|| {
            let placeholder = unresolved_symbol("external-class", name);
            symbols.push(placeholder.clone());
            placeholder
        });
    edges.push(Edge::new(class_symbol.id.clone(), base_symbol.id, EdgeKind::Extends));

    (symbols, edges)
}

fn find_entity_by_kinds_name<'a>(
    entities: &'a [SyntaxEntity],
    kinds: &[SyntaxKind],
    name: &str,
) -> Option<&'a SyntaxEntity> {
    entities.iter().find(|e| kinds.contains(&e.kind) && e.name == name)
}

const IMPORTABLE_KINDS: [SyntaxKind; 3] =
    [SyntaxKind::Class, SyntaxKind::Function, SyntaxKind::Variable];

/// Resolve every ES module `import_statement` and CommonJS `require(...)`
/// call into `IMPORTS` edges from `module_symbol`. Neither JS import path
/// (`"./shape"`, `"lodash"`) nor `require`'s path argument distinguishes a
/// repository-local module from an external package on its own without
/// resolving relative paths against the filesystem -- out of scope for this
/// sub-phase (mirrors Go's own `resolve_imports` gap). Instead, each
/// imported binding's *name* is searched repository-wide against
/// `all_entities` first (matching `adapters::python::resolve_imports`'s own
/// name-based best-effort search); only when nothing matches does it fall
/// back to an explicit `external-module` placeholder.
fn resolve_imports(
    sites: &ResolutionSites,
    src: &str,
    module_symbol: &SymbolEntity,
    all_entities: &[SyntaxEntity],
    symbol_table: &SymbolTable,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for import in &sites.imports {
        let source_path = import
            .child_by_field_name("source")
            .map(|n| text(n, src).trim_matches('"').trim_matches('\'').to_string())
            .unwrap_or_default();

        let Some(clause) = find_import_clause(*import) else {
            // A bare side-effect import (`import "./setup"`): nothing to
            // bind a name to, so it resolves to an explicit placeholder for
            // the module path itself.
            let placeholder = unresolved_symbol("external-module", &source_path);
            symbols.push(placeholder.clone());
            edges.push(Edge::new(module_symbol.id.clone(), placeholder.id, EdgeKind::Imports));
            continue;
        };

        for name in import_clause_names(clause, src) {
            bind_import(
                &name,
                &source_path,
                module_symbol,
                all_entities,
                symbol_table,
                &mut symbols,
                &mut edges,
            );
        }
    }

    for (declarator, path_node) in &sites.requires {
        let source_path = text(*path_node, src).trim_matches('"').trim_matches('\'').to_string();
        for name in require_binding_names(*declarator, src) {
            bind_import(
                &name,
                &source_path,
                module_symbol,
                all_entities,
                symbol_table,
                &mut symbols,
                &mut edges,
            );
        }
    }

    (symbols, edges)
}

/// The bound name(s) of a `require(...)` declarator: `const shape =
/// require(...)` binds `shape`; `const { a, b } = require(...)` binds `a`
/// and `b` (each destructured property's shorthand/keyed identifier -- a
/// renamed/computed property is a documented gap, matching
/// `commonjs_export_names`'s own object-literal handling). An array-
/// destructured require (`const [a] = require(...)`, unusual but legal) is
/// also a documented gap.
fn require_binding_names(declarator: Node, src: &str) -> Vec<String> {
    let Some(name) = declarator.child_by_field_name("name") else {
        return Vec::new();
    };
    match name.kind() {
        "identifier" => vec![text(name, src).to_string()],
        "object_pattern" => {
            let mut names = Vec::new();
            let mut cursor = name.walk();
            for property in name.children(&mut cursor) {
                match property.kind() {
                    "shorthand_property_identifier_pattern" => {
                        names.push(text(property, src).to_string())
                    }
                    "pair_pattern" => {
                        if let Some(value) = property.child_by_field_name("value") {
                            if value.kind() == "identifier" {
                                names.push(text(value, src).to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
            names
        }
        _ => Vec::new(),
    }
}

fn bind_import(
    name: &str,
    source_path: &str,
    module_symbol: &SymbolEntity,
    all_entities: &[SyntaxEntity],
    symbol_table: &SymbolTable,
    symbols: &mut Vec<SymbolEntity>,
    edges: &mut Vec<Edge>,
) {
    let target = find_entity_by_kinds_name(all_entities, &IMPORTABLE_KINDS, name)
        .and_then(|e| symbol_table.find_by_syntax_id(&e.id))
        .cloned()
        .unwrap_or_else(|| {
            let placeholder = unresolved_symbol(
                "external-module",
                &format!("{source_path}::{name}"),
            );
            symbols.push(placeholder.clone());
            placeholder
        });
    edges.push(Edge::new(module_symbol.id.clone(), target.id, EdgeKind::Imports));
}

/// The `import_clause` wrapper node under an `import_statement`, if it has
/// one (a bare side-effect import, `import "./setup"`, has none -- it's
/// just `'import' $.string`, no clause at all).
fn find_import_clause(import_statement: Node) -> Option<Node> {
    let mut cursor = import_statement.walk();
    let found = import_statement
        .children(&mut cursor)
        .find(|c| c.kind() == "import_clause");
    found
}

/// Every bound name an `import_clause` introduces: a default import
/// (`import Foo from ...`) binds `Foo`; `import { A, B as C } from ...`
/// binds `A` and `B` (the *original* exported name, not the local alias --
/// matching `adapters::python::resolve_imports`'s own use of the original
/// name for its best-effort search); `import Foo, { A } from ...` binds
/// both `Foo` and `A` (the default and named forms are siblings within the
/// same `import_clause`, not mutually exclusive). A namespace import
/// (`import * as ns from ...`) binds nothing individually (`ns` isn't
/// itself a repository declaration) -- a documented gap, resolved as a
/// single `external-module` placeholder instead by the caller.
fn import_clause_names(clause: Node, src: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = clause.walk();
    for child in clause.children(&mut cursor) {
        match child.kind() {
            "identifier" => names.push(text(child, src).to_string()),
            "named_imports" => {
                let mut spec_cursor = child.walk();
                for specifier in child.children(&mut spec_cursor) {
                    if specifier.kind() != "import_specifier" {
                        continue;
                    }
                    if let Some(name) = specifier.child_by_field_name("name") {
                        names.push(text(name, src).to_string());
                    }
                }
            }
            "namespace_import" => {}
            _ => {}
        }
    }
    names
}

/// Resolve every ES module `export`/CommonJS `module.exports`/`exports.x`
/// site into `EXPORTS` edges from `module_symbol`.
fn resolve_exports(
    sites: &ResolutionSites,
    src: &str,
    module_symbol: &SymbolEntity,
    entities: &[SyntaxEntity],
    symbol_table: &SymbolTable,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    const EXPORTABLE_KINDS: [SyntaxKind; 3] =
        [SyntaxKind::Class, SyntaxKind::Function, SyntaxKind::Variable];

    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for export_node in &sites.exports {
        for name in export_statement_names(*export_node, src) {
            if let Some(symbol) = find_entity_by_kinds_name(entities, &EXPORTABLE_KINDS, &name)
                .and_then(|e| symbol_table.find_by_syntax_id(&e.id))
            {
                edges.push(Edge::new(module_symbol.id.clone(), symbol.id.clone(), EdgeKind::Exports));
            } else {
                let placeholder = unresolved_symbol("export-target", &name);
                symbols.push(placeholder.clone());
                edges.push(Edge::new(module_symbol.id.clone(), placeholder.id, EdgeKind::Exports));
            }
        }
    }

    for assignment in &sites.commonjs_exports {
        for name in commonjs_export_names(*assignment, src) {
            if let Some(symbol) = find_entity_by_kinds_name(entities, &EXPORTABLE_KINDS, &name)
                .and_then(|e| symbol_table.find_by_syntax_id(&e.id))
            {
                edges.push(Edge::new(module_symbol.id.clone(), symbol.id.clone(), EdgeKind::Exports));
            } else {
                let placeholder = unresolved_symbol("export-target", &name);
                symbols.push(placeholder.clone());
                edges.push(Edge::new(module_symbol.id.clone(), placeholder.id, EdgeKind::Exports));
            }
        }
    }

    (symbols, edges)
}

/// Every declared name an `export_statement` makes public: an inline
/// declaration (`export function foo() {}`, `export class Foo {}`, `export
/// const x = 1`) exports the name(s) it declares; `export default X` (an
/// identifier reference) exports `X`; `export default class Foo {}`/
/// `export default function foo() {}` exports the declared name; `export {
/// a, b as c }` exports `a` and `b` (the original local name, matching
/// `bind_import`'s own original-name convention -- `as` renames the public
/// name, not the local declaration this searches for). A re-export (`export
/// { a } from "./other"`) is treated the same way -- resolving to the
/// re-exported *local* declaration is out of scope, a documented gap.
fn export_statement_names(node: Node, src: &str) -> Vec<String> {
    let mut names = Vec::new();

    if let Some(declaration) = node.child_by_field_name("declaration") {
        names.extend(declared_names(declaration, src));
        return names;
    }

    if let Some(value) = node.child_by_field_name("value") {
        if value.kind() == "identifier" {
            names.push(text(value, src).to_string());
        }
        return names;
    }

    let mut cursor = node.walk();
    for clause in node.children(&mut cursor) {
        if clause.kind() != "export_clause" {
            continue;
        }
        let mut spec_cursor = clause.walk();
        for specifier in clause.children(&mut spec_cursor) {
            if specifier.kind() != "export_specifier" {
                continue;
            }
            if let Some(name) = specifier.child_by_field_name("name") {
                if name.kind() == "identifier" {
                    names.push(text(name, src).to_string());
                }
            }
        }
    }

    names
}

fn declared_names(declaration: Node, src: &str) -> Vec<String> {
    match declaration.kind() {
        "function_declaration" | "generator_function_declaration" | "class_declaration" => {
            declaration
                .child_by_field_name("name")
                .map(|n| vec![text(n, src).to_string()])
                .unwrap_or_default()
        }
        "lexical_declaration" | "variable_declaration" => declarator_names(declaration)
            .into_iter()
            .map(|n| text(n, src).to_string())
            .collect(),
        _ => Vec::new(),
    }
}

/// The exported name(s) a CommonJS assignment (`module.exports.foo = ...`,
/// `exports.foo = ...`, `module.exports = LocalName`,
/// `module.exports = { a, b }`) makes public. `module.exports = <object
/// literal>` extracts each shorthand-or-keyed property whose value is a
/// plain identifier; a computed/spread property is a documented gap. A
/// direct `module.exports = SomeExpression` that isn't an identifier or
/// object literal is left unresolved (no name to export under) -- also a
/// documented gap, not guessed at.
fn commonjs_export_names(assignment: Node, src: &str) -> Vec<String> {
    let Some(left) = assignment.child_by_field_name("left") else {
        return Vec::new();
    };
    let Some(right) = assignment.child_by_field_name("right") else {
        return Vec::new();
    };

    // `module.exports.foo = value` / `exports.foo = value`: the property
    // name itself is the exported name (regardless of what `value` is).
    if left.kind() == "member_expression" {
        if let Some(object) = left.child_by_field_name("object") {
            let is_bare_exports_dot =
                object.kind() == "identifier" && text(object, src) == "exports";
            let is_module_exports_dot = object.kind() == "member_expression"
                && object
                    .child_by_field_name("object")
                    .is_some_and(|o| o.kind() == "identifier" && text(o, src) == "module")
                && object
                    .child_by_field_name("property")
                    .is_some_and(|p| text(p, src) == "exports");
            if is_bare_exports_dot || is_module_exports_dot {
                if let Some(property) = left.child_by_field_name("property") {
                    return vec![text(property, src).to_string()];
                }
            }
        }
    }

    // `module.exports = <value>`: only reachable when `left` is exactly
    // `module.exports` (checked by the caller via `is_commonjs_export_target`
    // finding no further nesting) -- so this branch handles the whole-module
    // assignment form.
    match right.kind() {
        "identifier" => vec![text(right, src).to_string()],
        "object" => {
            let mut names = Vec::new();
            let mut cursor = right.walk();
            for property in right.children(&mut cursor) {
                match property.kind() {
                    "shorthand_property_identifier" => names.push(text(property, src).to_string()),
                    "pair" => {
                        if let Some(value) = property.child_by_field_name("value") {
                            if value.kind() == "identifier" {
                                names.push(text(value, src).to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
            names
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
        let adapter = JavaScriptAdapter;
        let ast = adapter.parse(source).expect("valid javascript parses");
        adapter.extract(&ast, "module.js")
    }

    #[test]
    fn parses_valid_javascript_source() {
        let adapter = JavaScriptAdapter;
        assert!(adapter.parse("function main() {}\n").is_ok());
    }

    #[test]
    fn extract_always_returns_the_file_module_first() {
        let (entities, _) = extract("const x = 1;\n");
        assert_eq!(entities[0].kind, SyntaxKind::Module);
        assert_eq!(entities[0].name, "module.js");
    }

    #[test]
    fn extracts_a_top_level_function_with_its_parameters() {
        let (entities, edges) = extract("function add(a, b) {\n  return a + b;\n}\n");

        let module = &entities[0];
        let function = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "add")
            .expect("Function entity");
        assert!(edges.contains(&Edge::new(module.id.clone(), function.id.clone(), EdgeKind::Declares)));

        let param_names: Vec<&str> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Parameter)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(param_names, vec!["a", "b"]);
    }

    #[test]
    fn a_default_parameter_still_produces_a_named_parameter() {
        let (entities, _) = extract("function greet(name = \"world\") {}\n");
        let param_names: Vec<&str> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Parameter)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(param_names, vec!["name"]);
    }

    #[test]
    fn extracts_a_class_and_its_methods() {
        let source = "class Circle {\n  area() {\n    return 0;\n  }\n}\n";
        let (entities, edges) = extract(source);

        let module = &entities[0];
        let circle = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Circle")
            .expect("Class entity");
        assert!(edges.contains(&Edge::new(module.id.clone(), circle.id.clone(), EdgeKind::Declares)));

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
    fn extracts_a_let_declaration_in_a_function_body_as_a_variable() {
        let (entities, edges) = extract("function run() {\n  let total = 1;\n  return total;\n}\n");

        let function = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "run")
            .expect("Function entity");
        let variable = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Variable && e.name == "total")
            .expect("Variable entity");
        assert!(edges.contains(&Edge::new(function.id.clone(), variable.id.clone(), EdgeKind::Contains)));
        assert!(!edges.contains(&Edge::new(function.id.clone(), variable.id.clone(), EdgeKind::Declares)));
    }

    #[test]
    fn extracts_a_top_level_const_declaration() {
        let (entities, _) = extract("const Counter = 0;\n");
        assert!(entities.iter().any(|e| e.kind == SyntaxKind::Variable && e.name == "Counter"));
    }

    #[test]
    fn var_and_let_both_produce_variable_entities_despite_different_scoping_rules() {
        // `var` is function-scoped, `let`/`const` are block-scoped -- a real
        // semantic divergence this adapter doesn't model at the ScopeTree
        // level (ScopeTree only opens new scopes at Module/Function/Method
        // boundaries, not at every block), documented here rather than
        // silently assumed identical. Both still produce a `Variable`
        // syntax entity either way, which is what this test checks.
        let (entities, _) = extract("function run() {\n  var a = 1;\n  let b = 2;\n}\n");
        assert!(entities.iter().any(|e| e.kind == SyntaxKind::Variable && e.name == "a"));
        assert!(entities.iter().any(|e| e.kind == SyntaxKind::Variable && e.name == "b"));
    }

    #[test]
    fn a_non_declaration_statement_becomes_a_generic_statement_entity() {
        let (entities, _) = extract("function run() {\n  console.log(1);\n}\n");
        assert!(entities.iter().any(|e| e.kind == SyntaxKind::Statement));
    }

    #[test]
    fn same_named_method_on_different_classes_gets_distinct_ids() {
        let source = "class A {\n  run() {}\n}\nclass B {\n  run() {}\n}\n";
        let (entities, _) = extract(source);

        let methods: Vec<&SyntaxEntity> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Method && e.name == "run")
            .collect();
        assert_eq!(methods.len(), 2);
        assert_ne!(methods[0].id, methods[1].id);
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
        let adapter = JavaScriptAdapter;
        let ast = adapter.parse(source).expect("valid javascript parses");
        let (entities, edges) = adapter.extract(&ast, "module.js");
        let module = entities[0].clone();
        let fn_entity = entities
            .iter()
            .find(|e| (e.kind == SyntaxKind::Function || e.kind == SyntaxKind::Method) && e.name == fn_name)
            .unwrap_or_else(|| panic!("no Function/Method entity named {fn_name}"))
            .clone();
        let scope_tree = ScopeTree::build(&module, &entities, &edges);
        let scope = scope_tree.scope_of(&fn_entity.id).expect("function opens a scope");
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

    fn find_matching<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        fn search<'a>(node: Node<'a>, src: &'a str, name: &str) -> Option<Node<'a>> {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(child.kind(), "function_declaration" | "method_definition") {
                    let matches_name = child
                        .child_by_field_name("name")
                        .is_some_and(|n| text(n, src) == name);
                    if matches_name {
                        return Some(child);
                    }
                }
                if let Some(found) = search(child, src, name) {
                    return Some(found);
                }
            }
            None
        }
        search(tree.root_node(), src, name).unwrap_or_else(|| panic!("no function/method named {name}"))
    }

    fn body_of<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        find_matching(tree, src, name).child_by_field_name("body").unwrap()
    }

    #[test]
    fn resolves_a_reference_to_an_in_scope_variable() {
        let source = "function run() {\n  let value = 1;\n  return value;\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) = resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        let value_var = r.entities.iter().find(|e| e.kind == SyntaxKind::Variable).unwrap();
        let value_symbol = r.symbol_table.find_by_syntax_id(&value_var.id).unwrap();
        assert!(edges.contains(&Edge::new(
            r.fn_symbol.id.clone(),
            value_symbol.id.clone(),
            EdgeKind::References
        )));
    }

    #[test]
    fn a_reference_to_an_undefined_name_gets_an_explicit_placeholder() {
        let source = "function run() {\n  return missing;\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (symbols, edges) =
            resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        assert!(symbols.iter().any(|s| s.name.starts_with("unknown-local")));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::References && e.from == r.fn_symbol.id));
    }

    #[test]
    fn resolves_a_free_function_call() {
        let source = "function helper() {}\n\nfunction run() {\n  helper();\n}\n";
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
        let source =
            "class Shape {\n  area() {}\n}\n\nfunction run(shape) {\n  shape.area();\n}\n";
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
        let source = "class Circle {}\n\nfunction run() {\n  const c = new Circle();\n  return c;\n}\n";
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
        let source = "function run() {\n  foo()();\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (symbols, edges) =
            resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        assert!(symbols.iter().any(|s| s.name.starts_with("unsupported-form")));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Calls && e.from == r.fn_symbol.id));
    }

    #[test]
    fn a_class_extending_a_local_base_gets_an_extends_edge() {
        let source = "class Shape {\n  area() {}\n}\n\nclass Circle extends Shape {\n}\n";
        let adapter = JavaScriptAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _) = adapter.extract(&ast, "module.js");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let circle_node = find_class(&ast.tree, source, "Circle");
        let (_, edges) = resolve_extends(circle_node, source, &entities, &symbol_table);

        let circle = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Circle")
            .unwrap();
        let shape = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Shape")
            .unwrap();
        let circle_symbol = symbol_table.find_by_syntax_id(&circle.id).unwrap();
        let shape_symbol = symbol_table.find_by_syntax_id(&shape.id).unwrap();
        assert!(edges.contains(&Edge::new(
            circle_symbol.id.clone(),
            shape_symbol.id.clone(),
            EdgeKind::Extends
        )));
    }

    #[test]
    fn extending_an_external_base_class_gets_an_explicit_placeholder() {
        let source = "class Custom extends Error {\n}\n";
        let adapter = JavaScriptAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _) = adapter.extract(&ast, "module.js");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let custom_node = find_class(&ast.tree, source, "Custom");
        let (symbols, edges) = resolve_extends(custom_node, source, &entities, &symbol_table);

        assert!(symbols.iter().any(|s| s.name.starts_with("external-class")));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Extends));
    }

    fn find_class<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        let mut cursor = tree.root_node().walk();
        let found = tree.root_node().children(&mut cursor).find(|c| {
            c.kind() == "class_declaration"
                && c.child_by_field_name("name").is_some_and(|n| text(n, src) == name)
        });
        found.unwrap_or_else(|| panic!("no class named {name}"))
    }

    #[test]
    fn resolves_a_named_es_module_import_against_a_locally_declared_item() {
        let source = "class Shape {\n}\n";
        let adapter = JavaScriptAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _) = adapter.extract(&ast, "shape.js");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table.find_by_syntax_id(&entities[0].id).unwrap().clone();

        let import_source = "import { Shape } from \"./shape\";\n";
        let import_ast = adapter.parse(import_source).unwrap();
        let mut sites = ResolutionSites::default();
        let import_module = SyntaxEntity::new(SyntaxKind::Module, "circle.js", "circle.js");
        let mut import_entities = vec![import_module.clone()];
        let mut import_edges = Vec::new();
        extract_items(
            import_ast.tree.root_node(),
            &import_ast.source,
            &import_module,
            "circle.js",
            &mut import_entities,
            &mut import_edges,
            &mut sites,
        );

        let (_, edges) =
            resolve_imports(&sites, &import_ast.source, &module_symbol, &entities, &symbol_table);

        let shape = entities.iter().find(|e| e.kind == SyntaxKind::Class).unwrap();
        let shape_symbol = symbol_table.find_by_syntax_id(&shape.id).unwrap();
        assert!(edges.contains(&Edge::new(module_symbol.id.clone(), shape_symbol.id.clone(), EdgeKind::Imports)));
    }

    #[test]
    fn an_unresolvable_named_import_gets_an_explicit_placeholder() {
        let adapter = JavaScriptAdapter;
        let module = SyntaxEntity::new(SyntaxKind::Module, "module.js", "module.js");
        let symbol_table = crate::symbols::build_symbol_table(std::slice::from_ref(&module));
        let module_symbol = symbol_table.find_by_syntax_id(&module.id).unwrap().clone();

        let import_ast = adapter.parse("import { Missing } from \"lodash\";\n").unwrap();
        let mut sites = ResolutionSites::default();
        let mut entities = vec![module.clone()];
        let mut edges = Vec::new();
        extract_items(
            import_ast.tree.root_node(),
            &import_ast.source,
            &module,
            "module.js",
            &mut entities,
            &mut edges,
            &mut sites,
        );

        let (symbols, edges) =
            resolve_imports(&sites, &import_ast.source, &module_symbol, &[], &symbol_table);

        assert!(symbols.iter().any(|s| s.name.starts_with("external-module")));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Imports));
    }

    #[test]
    fn a_default_import_binds_by_its_local_name() {
        let adapter = JavaScriptAdapter;
        let module = SyntaxEntity::new(SyntaxKind::Module, "module.js", "module.js");
        let symbol_table = crate::symbols::build_symbol_table(std::slice::from_ref(&module));
        let module_symbol = symbol_table.find_by_syntax_id(&module.id).unwrap().clone();

        let import_ast = adapter.parse("import React from \"react\";\n").unwrap();
        let mut sites = ResolutionSites::default();
        let mut entities = vec![module.clone()];
        let mut edges = Vec::new();
        extract_items(
            import_ast.tree.root_node(),
            &import_ast.source,
            &module,
            "module.js",
            &mut entities,
            &mut edges,
            &mut sites,
        );

        let (symbols, edges) =
            resolve_imports(&sites, &import_ast.source, &module_symbol, &[], &symbol_table);

        assert!(symbols.iter().any(|s| s.name.contains("react::React")));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Imports));
    }

    #[test]
    fn a_commonjs_require_binds_its_local_name_and_produces_an_imports_edge() {
        let source = "class Shape {\n}\n";
        let adapter = JavaScriptAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _) = adapter.extract(&ast, "shape.js");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table.find_by_syntax_id(&entities[0].id).unwrap().clone();

        let require_source = "const Shape = require(\"./shape\");\n";
        let require_ast = adapter.parse(require_source).unwrap();
        let (require_entities, _) = adapter.extract(&require_ast, "circle.js");
        // The `require(...)` call site still also produces an ordinary
        // top-level `Variable` entity for its binding -- `require` isn't
        // treated as anything other than a normal call expression by
        // `extract`, only additionally recorded as an import site.
        assert!(require_entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Variable && e.name == "Shape"));

        let mut sites = ResolutionSites::default();
        let require_module = require_entities[0].clone();
        let mut dummy_entities = vec![require_module.clone()];
        let mut dummy_edges = Vec::new();
        extract_items(
            require_ast.tree.root_node(),
            &require_ast.source,
            &require_module,
            "circle.js",
            &mut dummy_entities,
            &mut dummy_edges,
            &mut sites,
        );

        let (_, edges) =
            resolve_imports(&sites, &require_ast.source, &module_symbol, &entities, &symbol_table);

        let shape = entities.iter().find(|e| e.kind == SyntaxKind::Class).unwrap();
        let shape_symbol = symbol_table.find_by_syntax_id(&shape.id).unwrap();
        assert!(edges.contains(&Edge::new(module_symbol.id.clone(), shape_symbol.id.clone(), EdgeKind::Imports)));
    }

    #[test]
    fn a_destructured_require_binds_each_property_name() {
        let adapter = JavaScriptAdapter;
        let module = SyntaxEntity::new(SyntaxKind::Module, "module.js", "module.js");
        let symbol_table = crate::symbols::build_symbol_table(std::slice::from_ref(&module));
        let module_symbol = symbol_table.find_by_syntax_id(&module.id).unwrap().clone();

        let require_ast = adapter
            .parse("const { readFile, writeFile } = require(\"fs\");\n")
            .unwrap();
        let mut sites = ResolutionSites::default();
        let mut entities = vec![module.clone()];
        let mut edges = Vec::new();
        extract_items(
            require_ast.tree.root_node(),
            &require_ast.source,
            &module,
            "module.js",
            &mut entities,
            &mut edges,
            &mut sites,
        );

        let (symbols, edges) =
            resolve_imports(&sites, &require_ast.source, &module_symbol, &[], &symbol_table);

        assert!(symbols.iter().any(|s| s.name.contains("fs::readFile")));
        assert!(symbols.iter().any(|s| s.name.contains("fs::writeFile")));
        assert_eq!(edges.iter().filter(|e| e.kind == EdgeKind::Imports).count(), 2);
    }

    #[test]
    fn exports_a_top_level_function_declared_with_export() {
        let source = "export function publicFn() {}\nfunction privateFn() {}\n";
        let adapter = JavaScriptAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _) = adapter.extract(&ast, "module.js");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table.find_by_syntax_id(&entities[0].id).unwrap().clone();

        let mut sites = ResolutionSites::default();
        let mut dummy_entities = vec![entities[0].clone()];
        let mut dummy_edges = Vec::new();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "module.js",
            &mut dummy_entities,
            &mut dummy_edges,
            &mut sites,
        );

        let (_, edges) = resolve_exports(&sites, &ast.source, &module_symbol, &entities, &symbol_table);

        let public_fn = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "publicFn")
            .unwrap();
        let public_symbol = symbol_table.find_by_syntax_id(&public_fn.id).unwrap();
        assert!(edges.contains(&Edge::new(module_symbol.id.clone(), public_symbol.id.clone(), EdgeKind::Exports)));
        assert_eq!(edges.len(), 1, "privateFn (not exported) shouldn't produce an EXPORTS edge");
    }

    #[test]
    fn exports_a_commonjs_module_exports_property_assignment() {
        let source = "function helper() {}\nmodule.exports.helper = helper;\n";
        let adapter = JavaScriptAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _) = adapter.extract(&ast, "module.js");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table.find_by_syntax_id(&entities[0].id).unwrap().clone();

        let mut sites = ResolutionSites::default();
        let mut dummy_entities = vec![entities[0].clone()];
        let mut dummy_edges = Vec::new();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "module.js",
            &mut dummy_entities,
            &mut dummy_edges,
            &mut sites,
        );

        let (_, edges) = resolve_exports(&sites, &ast.source, &module_symbol, &entities, &symbol_table);

        let helper = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "helper")
            .unwrap();
        let helper_symbol = symbol_table.find_by_syntax_id(&helper.id).unwrap();
        assert!(edges.contains(&Edge::new(module_symbol.id.clone(), helper_symbol.id.clone(), EdgeKind::Exports)));
    }

    #[test]
    fn exports_a_bare_exports_dot_property_assignment() {
        let source = "function helper() {}\nexports.helper = helper;\n";
        let adapter = JavaScriptAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _) = adapter.extract(&ast, "module.js");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table.find_by_syntax_id(&entities[0].id).unwrap().clone();

        let mut sites = ResolutionSites::default();
        let mut dummy_entities = vec![entities[0].clone()];
        let mut dummy_edges = Vec::new();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "module.js",
            &mut dummy_entities,
            &mut dummy_edges,
            &mut sites,
        );

        let (_, edges) = resolve_exports(&sites, &ast.source, &module_symbol, &entities, &symbol_table);

        let helper = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "helper")
            .unwrap();
        let helper_symbol = symbol_table.find_by_syntax_id(&helper.id).unwrap();
        assert!(edges.contains(&Edge::new(module_symbol.id.clone(), helper_symbol.id.clone(), EdgeKind::Exports)));
    }
}
