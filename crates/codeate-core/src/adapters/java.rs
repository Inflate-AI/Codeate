use tree_sitter::{Node, Parser, Tree};

use crate::adapter::LanguageAdapter;
use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};
use crate::resolve::unresolved_symbol;
use crate::symbols::{ScopeTree, SymbolTable};
use crate::types::TypeRef;

/// Java's primitive types (JLS §4.2), treated as `TypeRef::Primitive` the
/// same way Go's predeclared basic types and Rust's `i32`/`bool`/... are.
/// `void` is included since it's the common "no return value" spelling a
/// method signature actually uses, mirroring Go's own `"()"` unit-type
/// convention (see `extract_signature_types`, not yet wired in here either).
const PRIMITIVE_TYPES: &[&str] = &[
    "byte", "short", "int", "long", "char", "float", "double", "boolean", "void",
];

/// The parsed form a `JavaAdapter` produces: `tree-sitter`'s `Tree` paired
/// with the source text it was parsed from, the same shape `GoAst` uses --
/// `tree-sitter` nodes are spans into the original source rather than a
/// self-contained typed tree, so every node text lookup needs the source
/// bytes alongside the tree.
pub struct JavaAst {
    tree: Tree,
    source: String,
}

/// The Java language adapter: parses Java source via `tree-sitter`/
/// `tree-sitter-java` and maps it onto the canonical syntax model. Unlike
/// Go's implicit structural interface satisfaction, Java has explicit
/// `extends`/`implements` keywords -- `resolve_structural` below reads them
/// directly rather than inferring anything, closer to the Rust adapter's own
/// explicit `impl Trait for Type` handling than to Go's method-set
/// comparison.
pub struct JavaAdapter;

impl LanguageAdapter for JavaAdapter {
    type Ast = JavaAst;

    fn parse(&self, source: &str) -> Result<Self::Ast> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .map_err(|err| Error::Message(format!("failed to load Java grammar: {err}")))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::Message("failed to parse Java source".to_string()))?;
        Ok(JavaAst {
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
        // handed the pipeline -- mirrors Go's own `resolve_file` shape.
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

        for (member_entity, body) in &sites.bodies {
            let Some(member_symbol) = symbol_table.find_by_syntax_id(&member_entity.id).cloned()
            else {
                continue;
            };
            let Some(scope) = scope_tree.scope_of(&member_entity.id) else {
                continue;
            };
            let (body_symbols, body_edges) = resolve_body(
                *body,
                &ast.source,
                &member_symbol,
                &scope_tree,
                scope,
                symbol_table,
            );
            symbols.extend(body_symbols);
            edges.extend(body_edges);
        }

        edges.extend(resolve_structural(&sites, all_entities, symbol_table, &mut symbols));

        let (import_symbols, import_edges) =
            resolve_imports(&sites.imports, &ast.source, module_symbol);
        symbols.extend(import_symbols);
        edges.extend(import_edges);

        edges.extend(resolve_exports(
            &file_entities,
            &file_edges,
            &sites.public_types,
            module_symbol,
            symbol_table,
        ));

        (symbols, edges)
    }
}

/// Sites `extract_items` collects that `resolve_file` needs the raw tree
/// for, or that describe a structural relationship read directly off an
/// explicit Java keyword at extraction time (no later structural-inference
/// pass needed, unlike Go's `resolve_implements`).
#[derive(Default)]
struct ResolutionSites<'a> {
    /// A method/constructor's own entity paired with its body block.
    bodies: Vec<(SyntaxEntity, Node<'a>)>,
    /// Every `import_declaration`.
    imports: Vec<Node<'a>>,
    /// A class's own entity paired with its single `extends` superclass
    /// name, read directly from the `extends` keyword.
    superclasses: Vec<(SyntaxEntity, String)>,
    /// A class/enum's own entity paired with one `implements`-listed
    /// interface name (one entry per interface).
    implemented_interfaces: Vec<(SyntaxEntity, String)>,
    /// An interface's own entity paired with one `extends`-listed
    /// superinterface name (an interface can extend several, unlike a
    /// class's single superclass).
    extended_interfaces: Vec<(SyntaxEntity, String)>,
    /// Every class/interface/enum entity declared with an explicit `public`
    /// modifier -- `SyntaxEntity` itself carries no visibility field, so
    /// `resolve_exports` needs this recorded at extraction time, read
    /// directly off the modifier the same way `superclasses`/
    /// `implemented_interfaces` read `extends`/`implements`.
    public_types: Vec<crate::model::EntityId>,
}

/// Whether `node` (a `class_declaration`/`interface_declaration`/
/// `enum_declaration`) carries an explicit `public` modifier. `modifiers`
/// isn't a fielded child in the grammar, so it's found by kind; `'public'`
/// itself is an anonymous token, present as an unnamed child of that node.
fn has_public_modifier(node: Node) -> bool {
    let mut cursor = node.walk();
    let Some(modifiers) = node.children(&mut cursor).find(|c| c.kind() == "modifiers") else {
        return false;
    };
    let mut mod_cursor = modifiers.walk();
    let is_public = modifiers.children(&mut mod_cursor).any(|c| c.kind() == "public");
    is_public
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

/// Record `parent CONTAINS child` only (fields, parameters, local
/// variables, statements), then push `child` into `entities`.
fn contain(
    parent: &SyntaxEntity,
    child: SyntaxEntity,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    edges.push(Edge::new(parent.id.clone(), child.id.clone(), EdgeKind::Contains));
    entities.push(child);
}

/// A type expression's base declared name, ignoring generic type arguments
/// (`List<String>` -> `List`) and any qualifying scope beyond the final
/// segment (`java.util.List` -> `List`) -- what a `SyntaxEntity`'s own
/// `name` field holds for the class/interface it names, so it can be looked
/// up by exact match the same way Rust's `base_type_name` does.
fn type_base_name(node: Node, src: &str) -> String {
    match node.kind() {
        "type_identifier" => text(node, src).to_string(),
        "generic_type" => node
            .named_child(0)
            .map(|c| type_base_name(c, src))
            .unwrap_or_else(|| text(node, src).to_string()),
        "scoped_type_identifier" => node
            .named_child(node.named_child_count().saturating_sub(1))
            .map(|c| type_base_name(c, src))
            .unwrap_or_else(|| text(node, src).to_string()),
        "annotated_type" => node
            .named_child(node.named_child_count().saturating_sub(1))
            .map(|c| type_base_name(c, src))
            .unwrap_or_else(|| text(node, src).to_string()),
        _ => text(node, src).to_string(),
    }
}

/// Walk a `program`/class-body-like node's direct children, dispatching
/// top-level and nested declarations. `import_declaration`/
/// `package_declaration` only appear at the top level (`program`), but a
/// single dispatcher handles both cases -- unhandled kinds at either level
/// simply fall through, same as every other adapter's documented gaps.
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
        match child.kind() {
            "class_declaration" => extract_class(child, src, parent, scope, entities, edges, sites),
            "interface_declaration" => {
                extract_interface(child, src, parent, scope, entities, edges, sites)
            }
            "enum_declaration" => extract_enum(child, src, parent, scope, entities, edges, sites),
            "method_declaration" => {
                // A top-level method (Java 21+ "implicitly declared classes"
                // preview, or simply unusual input) has no enclosing class
                // to nest under -- modeled as a free `Function`, mirroring
                // Go/Rust's own top-level function handling.
                extract_member(child, src, parent, scope, entities, edges, sites)
            }
            "import_declaration" => sites.imports.push(child),
            // `package_declaration` carries no Phase 3 syntax counterpart of
            // its own (this adapter, like Go's per-file module, keeps one
            // `Module` entity per file rather than per package directory) --
            // a documented gap, not guessed at.
            _ => {}
        }
    }
}

/// A class/interface/enum body's direct member declarations: fields,
/// methods, constructors, and nested type declarations. Shared by
/// `class_body`, `interface_body`'s member loop, and `enum_body_declarations`
/// so the three don't drift out of sync.
fn extract_member<'a>(
    node: Node<'a>,
    src: &'a str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    match node.kind() {
        "field_declaration" | "constant_declaration" => {
            for declarator in declarator_names(node) {
                let entity = SyntaxEntity::new(SyntaxKind::Variable, owner_scope, text(declarator, src));
                contain(owner, entity, entities, edges);
            }
        }
        "method_declaration" => {
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
            // An abstract/interface method declaration has no body (`;`
            // instead of a block) -- nothing to extract/resolve for it.
            if let Some(body) = node.child_by_field_name("body") {
                extract_body(body, src, &entity, &method_scope, entities, edges);
                sites.bodies.push((entity, body));
            }
        }
        "constructor_declaration" | "compact_constructor_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let name = text(name_node, src);
            // Modeled as a `Method` (same as every other member function) --
            // the canonical model has no separate constructor kind, matching
            // Rust's own `impl` methods (including `new`) having no special
            // treatment either.
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
        "class_declaration" => extract_class(node, src, owner, owner_scope, entities, edges, sites),
        "interface_declaration" => {
            extract_interface(node, src, owner, owner_scope, entities, edges, sites)
        }
        "enum_declaration" => extract_enum(node, src, owner, owner_scope, entities, edges, sites),
        // A static/instance initializer block (`static { ... }`, `{ ... }`)
        // has no declared name to attach entities to under the canonical
        // model's Phase 3 shape -- a documented gap, not guessed at.
        "static_initializer" | "block" | ";" => {}
        _ => {}
    }
}

/// A `class_declaration`: its own `Class` entity, `extends`/`implements`
/// relations (recorded in `sites` for `resolve_structural`, read directly
/// from the explicit keywords rather than inferred), and its body's members.
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
    if has_public_modifier(node) {
        sites.public_types.push(entity.id.clone());
    }
    declare(parent, entity.clone(), entities, edges);

    if let Some(superclass) = node.child_by_field_name("superclass") {
        if let Some(type_node) = superclass.named_child(0) {
            sites
                .superclasses
                .push((entity.clone(), type_base_name(type_node, src)));
        }
    }
    if let Some(interfaces) = node.child_by_field_name("interfaces") {
        for type_node in super_interfaces_types(interfaces) {
            sites
                .implemented_interfaces
                .push((entity.clone(), type_base_name(type_node, src)));
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for member in body.children(&mut cursor) {
            extract_member(member, src, &entity, &class_scope, entities, edges, sites);
        }
    }
}

/// An `interface_declaration`: its own `Interface` entity, `extends`
/// relations to its superinterfaces (`extends_interfaces` isn't a fielded
/// child in `tree-sitter-java`'s grammar, so it's found by kind), and its
/// body's members (method signatures, constants, nested types).
fn extract_interface<'a>(
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
    let entity = SyntaxEntity::new(SyntaxKind::Interface, scope, name);
    let iface_scope = child_scope(scope, name);
    if has_public_modifier(node) {
        sites.public_types.push(entity.id.clone());
    }
    declare(parent, entity.clone(), entities, edges);

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "extends_interfaces" {
            continue;
        }
        if let Some(type_list) = child.named_child(0) {
            let mut list_cursor = type_list.walk();
            for type_node in type_list.named_children(&mut list_cursor) {
                sites
                    .extended_interfaces
                    .push((entity.clone(), type_base_name(type_node, src)));
            }
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for member in body.children(&mut cursor) {
            extract_member(member, src, &entity, &iface_scope, entities, edges, sites);
        }
    }
}

/// An `enum_declaration`: its own `Enum` entity, `implements` relations
/// (an enum can implement interfaces but never extends a class), its
/// constants (modeled as `Variable` entities -- the canonical model has no
/// dedicated enum-constant kind), and any member declarations after the
/// constant list (`enum_body_declarations`).
fn extract_enum<'a>(
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
    let entity = SyntaxEntity::new(SyntaxKind::Enum, scope, name);
    let enum_scope = child_scope(scope, name);
    if has_public_modifier(node) {
        sites.public_types.push(entity.id.clone());
    }
    declare(parent, entity.clone(), entities, edges);

    if let Some(interfaces) = node.child_by_field_name("interfaces") {
        for type_node in super_interfaces_types(interfaces) {
            sites
                .implemented_interfaces
                .push((entity.clone(), type_base_name(type_node, src)));
        }
    }

    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            "enum_constant" => {
                let Some(const_name) = child.child_by_field_name("name") else {
                    continue;
                };
                let const_entity =
                    SyntaxEntity::new(SyntaxKind::Variable, &enum_scope, text(const_name, src));
                contain(&entity, const_entity, entities, edges);
            }
            "enum_body_declarations" => {
                let mut decl_cursor = child.walk();
                for member in child.children(&mut decl_cursor) {
                    extract_member(member, src, &entity, &enum_scope, entities, edges, sites);
                }
            }
            _ => {}
        }
    }
}

/// `implements TypeList`'s individual type nodes: `super_interfaces`' only
/// named child is a `type_list`, whose own named children are the listed
/// types directly (`type_list` isn't a fielded wrapper in the grammar).
fn super_interfaces_types(super_interfaces: Node) -> Vec<Node> {
    let Some(type_list) = super_interfaces.named_child(0) else {
        return Vec::new();
    };
    let mut cursor = type_list.walk();
    type_list.named_children(&mut cursor).collect()
}

/// A `formal_parameters`' individual `formal_parameter`/`spread_parameter`
/// (varargs) nodes become `Parameter` entities. A `receiver_parameter`
/// (explicit `this` parameter, used only to annotate an outer-class
/// reference in a nested class) binds no name -- a documented gap, not
/// guessed at.
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
        match param.kind() {
            "formal_parameter" => {
                if let Some(name_node) = param.child_by_field_name("name") {
                    let entity =
                        SyntaxEntity::new(SyntaxKind::Parameter, scope, text(name_node, src));
                    contain(parent, entity, entities, edges);
                }
            }
            "spread_parameter" => {
                let mut spread_cursor = param.walk();
                let declarator = param
                    .children(&mut spread_cursor)
                    .find(|c| c.kind() == "variable_declarator");
                if let Some(declarator) = declarator {
                    if let Some(name_node) = declarator.child_by_field_name("name") {
                        let entity =
                            SyntaxEntity::new(SyntaxKind::Parameter, scope, text(name_node, src));
                        contain(parent, entity, entities, edges);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Every `variable_declarator`'s `"name"` field directly under `node`
/// (a `field_declaration`/`constant_declaration`/`local_variable_declaration`,
/// each exposing its declarators via the `"declarator"` field, possibly
/// repeated for `int a, b;`).
fn declarator_names(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children_by_field_name("declarator", &mut cursor)
        .filter_map(|d| d.child_by_field_name("name"))
        .collect()
}

/// Walk a method/constructor body's direct statements (a `block`'s named
/// children -- Java's `block` node holds its braces as anonymous tokens, so
/// `named_children` already excludes them, unlike Go's separate
/// `statement_list` wrapper): a `local_variable_declaration` becomes
/// `Variable` entities; everything else becomes a generic `Statement`
/// entity. A local class/interface/enum/method declaration inside a body
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
    let mut cursor = block.walk();
    for (index, stmt) in block.named_children(&mut cursor).enumerate() {
        match stmt.kind() {
            "local_variable_declaration" => {
                for name_node in declarator_names(stmt) {
                    let entity = SyntaxEntity::new(
                        SyntaxKind::Variable,
                        &format!("{scope}#{index}"),
                        text(name_node, src),
                    );
                    contain(parent, entity, entities, edges);
                }
            }
            "class_declaration" | "interface_declaration" | "enum_declaration"
            | "method_declaration" => {}
            _ => {
                let entity =
                    SyntaxEntity::new(SyntaxKind::Statement, scope, format!("stmt_{index}"));
                contain(parent, entity, entities, edges);
            }
        }
    }
}

/// `REFERENCES`/`CALLS`/`INSTANTIATES` edges from one method/constructor
/// body. A bare identifier read resolves via `scope_tree`; a bare-name call
/// (`helper()`) resolves the same way; a receiver-qualified call
/// (`obj.method()`) resolves by best-effort name match against any `Method`
/// symbol in the file (real receiver-type dispatch needs cross-file
/// class/interface knowledge, out of scope here, matching Go/Rust's own
/// method-call resolution); `new Type(...)` whose type resolves in scope to
/// a `Type` symbol is an `INSTANTIATES` site. Anything unresolved gets an
/// explicit placeholder symbol, never silently dropped.
fn resolve_body<'a>(
    body: Node<'a>,
    src: &str,
    member_symbol: &SymbolEntity,
    scope_tree: &ScopeTree,
    scope: usize,
    symbol_table: &SymbolTable,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut names: Vec<Node<'a>> = Vec::new();
    let mut calls: Vec<Node<'a>> = Vec::new();
    let mut news: Vec<Node<'a>> = Vec::new();

    let mut cursor = body.walk();
    for stmt in body.named_children(&mut cursor) {
        walk_stmt(stmt, &mut |n| names.push(n), &mut |n| calls.push(n), &mut |n| news.push(n));
    }

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
        edges.push(Edge::new(member_symbol.id.clone(), target.id.clone(), EdgeKind::References));
    }

    for call_node in &calls {
        let Some(name_node) = call_node.child_by_field_name("name") else {
            continue;
        };
        let ident = text(name_node, src);
        let target = if call_node.child_by_field_name("object").is_some() {
            // Receiver-qualified: best-effort name match, same posture as
            // Go's `selector_expression`/Rust's `MethodCall` handling.
            symbol_table
                .symbols()
                .find(|s| s.kind == SymbolKind::Method && s.name == ident)
                .cloned()
                .unwrap_or_else(|| {
                    let placeholder = unresolved_symbol("unknown-method", ident);
                    symbols.push(placeholder.clone());
                    placeholder
                })
        } else {
            scope_tree
                .lookup(scope, ident)
                .and_then(|id| symbol_table.find_by_syntax_id(id))
                .cloned()
                .unwrap_or_else(|| {
                    let placeholder = unresolved_symbol("unknown-local", ident);
                    symbols.push(placeholder.clone());
                    placeholder
                })
        };
        edges.push(Edge::new(member_symbol.id.clone(), target.id.clone(), EdgeKind::Calls));
    }

    for new_node in &news {
        let Some(type_node) = new_node.child_by_field_name("type") else {
            continue;
        };
        let ident = type_base_name(type_node, src);
        if let Some(symbol) = scope_tree
            .lookup(scope, &ident)
            .and_then(|id| symbol_table.find_by_syntax_id(id))
        {
            if symbol.kind == SymbolKind::Type {
                edges.push(Edge::new(
                    member_symbol.id.clone(),
                    symbol.id.clone(),
                    EdgeKind::Instantiates,
                ));
                continue;
            }
        }
        // Not in scope (an external/library type) -- still an explicit
        // `INSTANTIATES` site, targeting a `Named`-type placeholder symbol
        // the same way Rust's `resolve_instantiates` treats an out-of-scope
        // struct literal.
        let type_symbol = crate::types::type_symbol_for(&TypeRef::Named {
            name: ident,
            type_args: Vec::new(),
        });
        symbols.push(type_symbol.clone());
        edges.push(Edge::new(member_symbol.id.clone(), type_symbol.id, EdgeKind::Instantiates));
    }

    (symbols, edges)
}

fn walk_expr<'a>(
    node: Node<'a>,
    on_name: &mut impl FnMut(Node<'a>),
    on_call: &mut impl FnMut(Node<'a>),
    on_new: &mut impl FnMut(Node<'a>),
) {
    match node.kind() {
        "identifier" => on_name(node),
        "method_invocation" => {
            on_call(node);
            match node.child_by_field_name("object") {
                Some(object) => walk_expr(object, on_name, on_call, on_new),
                // A bare-name call (`helper()`) is also a name usage of
                // `helper` itself, matching Go's own `call_expression`
                // handling (which walks its `function` field too) --
                // without this, a body whose only expression is a bare call
                // produces a CALLS edge but no REFERENCES edge for the same
                // name, an inconsistency Go doesn't have.
                None => {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        on_name(name_node);
                    }
                }
            }
            if let Some(args) = node.child_by_field_name("arguments") {
                let mut cursor = args.walk();
                for arg in args.named_children(&mut cursor) {
                    walk_expr(arg, on_name, on_call, on_new);
                }
            }
        }
        "object_creation_expression" => {
            on_new(node);
            if let Some(args) = node.child_by_field_name("arguments") {
                let mut cursor = args.walk();
                for arg in args.named_children(&mut cursor) {
                    walk_expr(arg, on_name, on_call, on_new);
                }
            }
        }
        "field_access" => {
            if let Some(object) = node.child_by_field_name("object") {
                walk_expr(object, on_name, on_call, on_new);
            }
        }
        "assignment_expression" => {
            if let Some(left) = node.child_by_field_name("left") {
                walk_expr(left, on_name, on_call, on_new);
            }
            if let Some(right) = node.child_by_field_name("right") {
                walk_expr(right, on_name, on_call, on_new);
            }
        }
        "binary_expression" => {
            if let Some(left) = node.child_by_field_name("left") {
                walk_expr(left, on_name, on_call, on_new);
            }
            if let Some(right) = node.child_by_field_name("right") {
                walk_expr(right, on_name, on_call, on_new);
            }
        }
        "unary_expression" => {
            if let Some(operand) = node.child_by_field_name("operand") {
                walk_expr(operand, on_name, on_call, on_new);
            }
        }
        "cast_expression" => {
            if let Some(value) = node.child_by_field_name("value") {
                walk_expr(value, on_name, on_call, on_new);
            }
        }
        "parenthesized_expression" => {
            if let Some(inner) = node.named_child(0) {
                walk_expr(inner, on_name, on_call, on_new);
            }
        }
        "array_access" => {
            if let Some(array) = node.child_by_field_name("array") {
                walk_expr(array, on_name, on_call, on_new);
            }
            if let Some(index) = node.child_by_field_name("index") {
                walk_expr(index, on_name, on_call, on_new);
            }
        }
        _ => {}
    }
}

fn walk_stmt<'a>(
    node: Node<'a>,
    on_name: &mut impl FnMut(Node<'a>),
    on_call: &mut impl FnMut(Node<'a>),
    on_new: &mut impl FnMut(Node<'a>),
) {
    match node.kind() {
        "expression_statement" => {
            if let Some(inner) = node.named_child(0) {
                walk_expr(inner, on_name, on_call, on_new);
            }
        }
        "local_variable_declaration" => {
            let mut cursor = node.walk();
            for declarator in node.children_by_field_name("declarator", &mut cursor) {
                if let Some(value) = declarator.child_by_field_name("value") {
                    walk_expr(value, on_name, on_call, on_new);
                }
            }
        }
        "return_statement" | "throw_statement" | "assert_statement" | "yield_statement" => {
            if let Some(expr) = node.named_child(0) {
                walk_expr(expr, on_name, on_call, on_new);
            }
        }
        "if_statement" => {
            if let Some(condition) = node.child_by_field_name("condition") {
                if let Some(inner) = condition.named_child(0) {
                    walk_expr(inner, on_name, on_call, on_new);
                }
            }
            if let Some(consequence) = node.child_by_field_name("consequence") {
                walk_stmt(consequence, on_name, on_call, on_new);
            }
            if let Some(alternative) = node.child_by_field_name("alternative") {
                walk_stmt(alternative, on_name, on_call, on_new);
            }
        }
        "while_statement" => {
            if let Some(condition) = node.child_by_field_name("condition") {
                if let Some(inner) = condition.named_child(0) {
                    walk_expr(inner, on_name, on_call, on_new);
                }
            }
            if let Some(body) = node.child_by_field_name("body") {
                walk_stmt(body, on_name, on_call, on_new);
            }
        }
        "do_statement" => {
            if let Some(body) = node.child_by_field_name("body") {
                walk_stmt(body, on_name, on_call, on_new);
            }
        }
        "for_statement" => {
            if let Some(init) = node.child_by_field_name("init") {
                walk_stmt(init, on_name, on_call, on_new);
            }
            if let Some(body) = node.child_by_field_name("body") {
                walk_stmt(body, on_name, on_call, on_new);
            }
        }
        "enhanced_for_statement" => {
            if let Some(value) = node.child_by_field_name("value") {
                walk_expr(value, on_name, on_call, on_new);
            }
            if let Some(body) = node.child_by_field_name("body") {
                walk_stmt(body, on_name, on_call, on_new);
            }
        }
        "labeled_statement" => {
            if let Some(inner) = node.named_child(node.named_child_count().saturating_sub(1)) {
                walk_stmt(inner, on_name, on_call, on_new);
            }
        }
        "synchronized_statement" => {
            if let Some(body) = node.named_child(node.named_child_count().saturating_sub(1)) {
                if body.kind() == "block" {
                    walk_stmt(body, on_name, on_call, on_new);
                }
            }
        }
        "block" => {
            let mut cursor = node.walk();
            for stmt in node.named_children(&mut cursor) {
                walk_stmt(stmt, on_name, on_call, on_new);
            }
        }
        // try/switch/local classes and other less common control flow are a
        // documented gap here, matching Go's own limited `walk_stmt_sites`
        // coverage (`for`/`if`/`block` only) -- not silently miscounted,
        // simply not descended into.
        _ => {}
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

/// Resolve `sites`' explicit `extends`/`implements` relations into
/// `EXTENDS`/`IMPLEMENTS` edges, searched repository-wide (`all_entities`)
/// so a superclass/superinterface declared in a different file still
/// resolves -- matching Go's own cross-file `resolve_implements` reach, but
/// reading the relation directly off the keyword rather than inferring it
/// from a method-set comparison. A name that resolves to nothing in the
/// repository (an external/JDK type, e.g. `extends RuntimeException`) gets
/// an explicit placeholder symbol, never silently dropped.
fn resolve_structural(
    sites: &ResolutionSites,
    all_entities: &[SyntaxEntity],
    symbol_table: &SymbolTable,
    symbols: &mut Vec<SymbolEntity>,
) -> Vec<Edge> {
    let mut edges = Vec::new();

    for (class_entity, superclass_name) in &sites.superclasses {
        let Some(class_symbol) = symbol_table.find_by_syntax_id(&class_entity.id) else {
            continue;
        };
        let target = find_type_symbol(all_entities, symbol_table, &[SyntaxKind::Class], superclass_name)
            .unwrap_or_else(|| {
                let placeholder = unresolved_symbol("external-class", superclass_name);
                symbols.push(placeholder.clone());
                placeholder
            });
        edges.push(Edge::new(class_symbol.id.clone(), target.id, EdgeKind::Extends));
    }

    for (entity, interface_name) in &sites.implemented_interfaces {
        let Some(owner_symbol) = symbol_table.find_by_syntax_id(&entity.id) else {
            continue;
        };
        let target =
            find_type_symbol(all_entities, symbol_table, &[SyntaxKind::Interface], interface_name)
                .unwrap_or_else(|| {
                    let placeholder = unresolved_symbol("external-interface", interface_name);
                    symbols.push(placeholder.clone());
                    placeholder
                });
        edges.push(Edge::new(owner_symbol.id.clone(), target.id, EdgeKind::Implements));
    }

    for (iface_entity, super_name) in &sites.extended_interfaces {
        let Some(iface_symbol) = symbol_table.find_by_syntax_id(&iface_entity.id) else {
            continue;
        };
        let target = find_type_symbol(all_entities, symbol_table, &[SyntaxKind::Interface], super_name)
            .unwrap_or_else(|| {
                let placeholder = unresolved_symbol("external-interface", super_name);
                symbols.push(placeholder.clone());
                placeholder
            });
        edges.push(Edge::new(iface_symbol.id.clone(), target.id, EdgeKind::Extends));
    }

    edges
}

fn find_type_symbol(
    all_entities: &[SyntaxEntity],
    symbol_table: &SymbolTable,
    kinds: &[SyntaxKind],
    name: &str,
) -> Option<SymbolEntity> {
    all_entities
        .iter()
        .find(|e| kinds.contains(&e.kind) && e.name == name)
        .and_then(|e| symbol_table.find_by_syntax_id(&e.id))
        .cloned()
}

/// Resolve a file's `import_declaration`s into `IMPORTS` edges from
/// `module_symbol`. A Java import path (`java.util.List`,
/// `com.example.shapes.Shape`) doesn't tell us, without resolving it against
/// this repository's actual package layout, whether it names a JDK/
/// third-party type or another file in this same repository -- out of scope
/// for this sub-phase, so (matching Go's own `resolve_imports` posture)
/// every import resolves to an explicit `external-package` placeholder
/// rather than guessed at. A static import (`import static Foo.bar`) and a
/// wildcard import (`import a.b.*`) both resolve the same way, keyed by
/// their full raw path.
fn resolve_imports(
    imports: &[Node],
    src: &str,
    module_symbol: &SymbolEntity,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for import_node in imports {
        // `import_declaration`'s `_name` (and optional `.*`) children are
        // unfielded -- collect every named child's text and join it back
        // into the dotted path, wildcard included.
        let mut cursor = import_node.walk();
        let path: Vec<String> = import_node
            .named_children(&mut cursor)
            .filter(|c| c.kind() != "static")
            .map(|c| text(c, src).to_string())
            .collect();
        if path.is_empty() {
            continue;
        }
        let joined = path.join(".");
        let placeholder = unresolved_symbol("external-package", &joined);
        symbols.push(placeholder.clone());
        edges.push(Edge::new(module_symbol.id.clone(), placeholder.id, EdgeKind::Imports));
    }

    (symbols, edges)
}

/// Resolve a file's top-level `public` type declarations into `EXPORTS`
/// edges from `module_symbol`: Java's actual public-API convention -- an
/// explicit `public` modifier (recorded in `public_types` at extraction
/// time, since `SyntaxEntity` carries no visibility field of its own),
/// unlike Go's uppercase-identifier convention. A package-private (no
/// `public` modifier) top-level declaration is not exported.
fn resolve_exports(
    file_entities: &[SyntaxEntity],
    file_edges: &[Edge],
    public_types: &[crate::model::EntityId],
    module_symbol: &SymbolEntity,
    symbol_table: &SymbolTable,
) -> Vec<Edge> {
    const EXPORTABLE_KINDS: [SyntaxKind; 3] =
        [SyntaxKind::Class, SyntaxKind::Interface, SyntaxKind::Enum];

    let mut edges = Vec::new();
    let module_id = &file_entities[0].id;
    for entity_id in declared_children(file_edges, module_id) {
        let Some(entity) = file_entities.iter().find(|e| &e.id == entity_id) else {
            continue;
        };
        if !EXPORTABLE_KINDS.contains(&entity.kind) {
            continue;
        }
        if !public_types.contains(entity_id) {
            continue;
        }
        if let Some(symbol) = symbol_table.find_by_syntax_id(&entity.id) {
            edges.push(Edge::new(module_symbol.id.clone(), symbol.id.clone(), EdgeKind::Exports));
        }
    }
    edges
}

/// Map a Java type expression to a `TypeRef`. Not yet wired into
/// `extract`/`resolve_file` -- `ACCEPTS`/`RETURNS` type binding isn't called
/// from the pipeline, mirroring the Go/Rust adapters' own
/// `extract_signature_types`/`extract_struct_fields` gap (tested directly,
/// deliberately not yet wired in either).
///
/// Canonical-model gap, found while implementing this: `TypeRef::Generic`
/// carries only a type parameter's name, with nowhere to record a bound
/// (`<T extends Comparable<T>>`) -- the same gap Go's own
/// `extract_generic_params` and Rust's `extract_generic_params` already
/// leave open (both capture only the parameter name too). Not fixed here,
/// consistent with that existing precedent -- `extract_generic_params`
/// below captures the name only, same posture as Go/Rust.
#[allow(dead_code)]
fn extract_type(node: Node, src: &str) -> TypeRef {
    match node.kind() {
        "type_identifier" | "void_type" | "integral_type" | "floating_point_type"
        | "boolean_type" => {
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
                .named_child(0)
                .map(|c| type_base_name(c, src))
                .unwrap_or_else(|| text(node, src).to_string());
            let type_args = node
                .named_child(1)
                .filter(|n| n.kind() == "type_arguments")
                .map(|args| {
                    let mut cursor = args.walk();
                    args.named_children(&mut cursor)
                        .map(|arg| extract_type(arg, src))
                        .collect()
                })
                .unwrap_or_default();
            TypeRef::Named { name, type_args }
        }
        "scoped_type_identifier" => TypeRef::Named {
            name: type_base_name(node, src),
            type_args: Vec::new(),
        },
        "array_type" => {
            let inner = node
                .child_by_field_name("element")
                .map(|c| extract_type(c, src))
                .unwrap_or_else(|| TypeRef::Unresolved(text(node, src).to_string()));
            TypeRef::Reference {
                mutable: true,
                inner: Box::new(inner),
            }
        }
        _ => TypeRef::Unresolved(text(node, src).to_string()),
    }
}

/// A method/constructor's own type-parameter list (`<T extends
/// Comparable<T>>`) becomes `Generic` symbols declared under it -- the bound
/// itself (`extends Comparable<T>`) is read but discarded, the
/// canonical-model gap documented on `extract_type` above. Not yet wired
/// into `extract`/`resolve_file`, matching Go/Rust's own
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
        if param.kind() != "type_parameter" {
            continue;
        }
        let mut param_cursor = param.walk();
        let Some(name_node) = param
            .named_children(&mut param_cursor)
            .find(|c| c.kind() == "type_identifier")
        else {
            continue;
        };
        let name = text(name_node, src);
        let symbol = SymbolEntity::new(SymbolKind::Generic, owner_symbol.id.as_str(), name);
        edges.push(Edge::new(owner_symbol.id.clone(), symbol.id.clone(), EdgeKind::Declares));
        symbols.push(symbol);
    }

    (symbols, edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
        let adapter = JavaAdapter;
        let ast = adapter.parse(source).expect("valid java parses");
        adapter.extract(&ast, "Module.java")
    }

    fn parse_source(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_java::LANGUAGE.into()).unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn parses_valid_java_source() {
        let adapter = JavaAdapter;
        assert!(adapter
            .parse("public class Main { public static void main(String[] args) {} }")
            .is_ok());
    }

    #[test]
    fn parsing_invalid_syntax_is_a_clear_error() {
        // tree-sitter is error-tolerant by design and always returns *some*
        // tree, so "invalid" Java doesn't surface as an `Err` here the way
        // `syn`'s hand-rolled parser does for Rust -- there is nothing
        // further to assert for this adapter's `parse`, a documented
        // difference from the Rust adapter's own equivalent test.
        let adapter = JavaAdapter;
        assert!(adapter.parse("class {{{ not java").is_ok());
    }

    #[test]
    fn extract_always_returns_the_file_module_first() {
        let (entities, _) = extract("");
        assert_eq!(entities[0].kind, SyntaxKind::Module);
        assert_eq!(entities[0].name, "Module.java");
    }

    #[test]
    fn extracts_a_top_level_class_with_a_method_and_its_parameters() {
        let (entities, edges) =
            extract("public class Calculator {\n  public int add(int a, int b) {\n    return a + b;\n  }\n}\n");

        let module = &entities[0];
        let class = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Calculator")
            .expect("Class entity");
        assert!(edges.contains(&Edge::new(module.id.clone(), class.id.clone(), EdgeKind::Declares)));

        let method = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "add")
            .expect("Method entity");
        assert!(edges.contains(&Edge::new(class.id.clone(), method.id.clone(), EdgeKind::Declares)));

        let param_names: Vec<&str> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Parameter)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(param_names, vec!["a", "b"]);
    }

    #[test]
    fn extracts_a_constructor_as_a_method() {
        let (entities, edges) = extract("class Point {\n  Point(int x) {}\n}\n");
        let class = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Point")
            .unwrap();
        let ctor = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "Point")
            .expect("constructor modeled as a Method");
        assert!(edges.contains(&Edge::new(class.id.clone(), ctor.id.clone(), EdgeKind::Declares)));
    }

    #[test]
    fn extracts_an_interface_with_a_method_signature_and_no_body() {
        let (entities, _edges) = extract("interface Shape {\n  double area();\n}\n");
        entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Interface && e.name == "Shape")
            .expect("Interface entity");
        entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "area")
            .expect("Method signature entity, even with no body");
    }

    #[test]
    fn extracts_an_enum_with_its_constants() {
        let (entities, edges) = extract("enum Color {\n  RED, GREEN, BLUE\n}\n");
        let color = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Enum && e.name == "Color")
            .expect("Enum entity");
        let red = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Variable && e.name == "RED")
            .expect("RED constant entity");
        assert!(edges.contains(&Edge::new(color.id.clone(), red.id.clone(), EdgeKind::Contains)));
    }

    #[test]
    fn extracts_a_nested_class_scoped_under_its_enclosing_class() {
        let source = "class Outer {\n  class Inner {\n    void run() {}\n  }\n}\n";
        let (entities, edges) = extract(source);
        let outer = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Outer")
            .unwrap();
        let inner = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Inner")
            .unwrap();
        assert!(edges.contains(&Edge::new(outer.id.clone(), inner.id.clone(), EdgeKind::Declares)));
    }

    #[test]
    fn extracts_a_field_declaration_as_a_variable() {
        let (entities, edges) = extract("class Box {\n  private int size;\n}\n");
        let class = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class)
            .unwrap();
        let field = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Variable && e.name == "size")
            .expect("field entity");
        assert!(edges.contains(&Edge::new(class.id.clone(), field.id.clone(), EdgeKind::Contains)));
        assert!(!edges.contains(&Edge::new(class.id.clone(), field.id.clone(), EdgeKind::Declares)));
    }

    #[test]
    fn extracts_a_local_variable_declaration_in_a_method_body() {
        let (entities, edges) =
            extract("class Run {\n  void run() {\n    int total = 1;\n  }\n}\n");
        let method = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "run")
            .unwrap();
        let variable = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Variable && e.name == "total")
            .expect("Variable entity");
        assert!(edges.contains(&Edge::new(method.id.clone(), variable.id.clone(), EdgeKind::Contains)));
        assert!(!edges.contains(&Edge::new(method.id.clone(), variable.id.clone(), EdgeKind::Declares)));
    }

    #[test]
    fn a_non_declaration_statement_becomes_a_generic_statement_entity() {
        let (entities, _) =
            extract("class Run {\n  void run() {\n    System.out.println(1);\n  }\n}\n");
        assert!(entities.iter().any(|e| e.kind == SyntaxKind::Statement));
    }

    #[test]
    fn same_named_method_on_different_classes_gets_distinct_ids() {
        let source = "class A {\n  void run() {}\n}\nclass B {\n  void run() {}\n}\n";
        let (entities, _) = extract(source);
        let methods: Vec<&SyntaxEntity> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Method && e.name == "run")
            .collect();
        assert_eq!(methods.len(), 2);
        assert_ne!(methods[0].id, methods[1].id);
    }

    // -- type extraction (not yet wired into extract/resolve_file) --

    fn method_named<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        fn find<'a>(node: Node<'a>, src: &'a str, name: &str) -> Option<Node<'a>> {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "method_declaration"
                    && child
                        .child_by_field_name("name")
                        .is_some_and(|n| text(n, src) == name)
                {
                    return Some(child);
                }
                if let Some(found) = find(child, src, name) {
                    return Some(found);
                }
            }
            None
        }
        find(tree.root_node(), src, name).unwrap_or_else(|| panic!("no method named {name}"))
    }

    #[test]
    fn extracts_a_primitive_return_type() {
        let full = "class C {\n  int f() { return 0; }\n}\n";
        let tree = parse_source(full);
        let node = method_named(&tree, full, "f").child_by_field_name("type").unwrap();
        assert_eq!(extract_type(node, full), TypeRef::Primitive("int".to_string()));
    }

    #[test]
    fn extracts_a_user_defined_named_type() {
        let full = "class C {\n  Circle f() { return null; }\n}\n";
        let tree = parse_source(full);
        let node = method_named(&tree, full, "f").child_by_field_name("type").unwrap();
        assert_eq!(
            extract_type(node, full),
            TypeRef::Named { name: "Circle".to_string(), type_args: Vec::new() }
        );
    }

    #[test]
    fn extracts_a_generic_named_type_with_its_type_argument() {
        let full = "class C {\n  java.util.List<String> f() { return null; }\n}\n";
        let tree = parse_source(full);
        let node = method_named(&tree, full, "f").child_by_field_name("type").unwrap();
        assert_eq!(
            extract_type(node, full),
            TypeRef::Named {
                name: "List".to_string(),
                type_args: vec![TypeRef::Named { name: "String".to_string(), type_args: Vec::new() }],
            }
        );
    }

    // -- symbol/reference/call/instantiate/structural resolution --

    struct Resolved {
        entities: Vec<SyntaxEntity>,
        scope_tree: ScopeTree,
        scope: usize,
        symbol_table: SymbolTable,
        member_symbol: SymbolEntity,
    }

    fn setup(source: &str, member_name: &str) -> (Tree, Resolved) {
        let adapter = JavaAdapter;
        let ast = adapter.parse(source).expect("valid java parses");
        let (entities, edges) = adapter.extract(&ast, "Module.java");
        let module = entities[0].clone();
        let member_entity = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == member_name)
            .unwrap_or_else(|| panic!("no Method entity named {member_name}"))
            .clone();
        let scope_tree = ScopeTree::build(&module, &entities, &edges);
        let scope = scope_tree.scope_of(&member_entity.id).expect("method opens a scope");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let member_symbol = symbol_table
            .find_by_syntax_id(&member_entity.id)
            .expect("method symbol")
            .clone();
        (
            ast.tree,
            Resolved { entities, scope_tree, scope, symbol_table, member_symbol },
        )
    }

    fn body_of<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        method_named(tree, src, name).child_by_field_name("body").unwrap()
    }

    #[test]
    fn resolves_a_reference_to_an_in_scope_local_variable() {
        let source = "class C {\n  void run() {\n    int value = 1;\n    System.out.println(value);\n  }\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) =
            resolve_body(body, source, &r.member_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        let value_var = r.entities.iter().find(|e| e.kind == SyntaxKind::Variable).unwrap();
        let value_symbol = r.symbol_table.find_by_syntax_id(&value_var.id).unwrap();
        assert!(edges.contains(&Edge::new(
            r.member_symbol.id.clone(),
            value_symbol.id.clone(),
            EdgeKind::References
        )));
    }

    #[test]
    fn a_reference_to_an_undefined_name_gets_an_explicit_placeholder() {
        let source = "class C {\n  void run() {\n    missing.toString();\n  }\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (symbols, _edges) =
            resolve_body(body, source, &r.member_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        assert!(symbols.iter().any(|s| s.name.starts_with("unknown-local")));
    }

    #[test]
    fn resolves_a_free_function_style_call() {
        let source = "class C {\n  void helper() {}\n  void run() {\n    helper();\n  }\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) =
            resolve_body(body, source, &r.member_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        let helper = r
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "helper")
            .unwrap();
        let helper_symbol = r.symbol_table.find_by_syntax_id(&helper.id).unwrap();
        assert!(edges.contains(&Edge::new(
            r.member_symbol.id.clone(),
            helper_symbol.id.clone(),
            EdgeKind::Calls
        )));
    }

    #[test]
    fn resolves_a_receiver_qualified_call_by_best_effort_name_match() {
        let source =
            "class Shape {\n  double area() { return 0; }\n}\nclass Run {\n  void run(Shape shape) {\n    shape.area();\n  }\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) =
            resolve_body(body, source, &r.member_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        let area = r
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "area")
            .unwrap();
        let area_symbol = r.symbol_table.find_by_syntax_id(&area.id).unwrap();
        assert!(edges.contains(&Edge::new(
            r.member_symbol.id.clone(),
            area_symbol.id.clone(),
            EdgeKind::Calls
        )));
    }

    #[test]
    fn resolves_an_object_creation_instantiation_for_an_in_scope_type() {
        let source = "class Circle {}\nclass Run {\n  void run() {\n    Circle c = new Circle();\n  }\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) =
            resolve_body(body, source, &r.member_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        let circle = r
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Circle")
            .unwrap();
        let circle_symbol = r.symbol_table.find_by_syntax_id(&circle.id).unwrap();
        assert!(edges.contains(&Edge::new(
            r.member_symbol.id.clone(),
            circle_symbol.id.clone(),
            EdgeKind::Instantiates
        )));
    }

    #[test]
    fn resolves_an_object_creation_for_an_external_type_to_a_placeholder() {
        let source = "class Run {\n  void run() {\n    Object o = new Object();\n  }\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (symbols, edges) =
            resolve_body(body, source, &r.member_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        assert!(symbols.iter().any(|s| s.name == "Object"));
        assert!(edges
            .iter()
            .any(|e| e.kind == EdgeKind::Instantiates && e.from == r.member_symbol.id));
    }

    #[test]
    fn a_class_implementing_an_interface_gets_an_explicit_implements_edge() {
        let source = "interface Shape {\n  double area();\n}\nclass Circle implements Shape {\n  public double area() { return 0; }\n}\n";
        let adapter = JavaAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "Module.java");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "Module.java",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );
        let mut symbols = Vec::new();
        let implements_edges = resolve_structural(&sites, &entities, &symbol_table, &mut symbols);

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
    fn a_class_extending_a_class_gets_an_explicit_extends_edge() {
        let source = "class Animal {}\nclass Dog extends Animal {}\n";
        let adapter = JavaAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "Module.java");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "Module.java",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );
        let mut symbols = Vec::new();
        let extends_edges = resolve_structural(&sites, &entities, &symbol_table, &mut symbols);

        let dog = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Dog")
            .unwrap();
        let animal = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Animal")
            .unwrap();
        let dog_symbol = symbol_table.find_by_syntax_id(&dog.id).unwrap();
        let animal_symbol = symbol_table.find_by_syntax_id(&animal.id).unwrap();
        assert!(extends_edges.contains(&Edge::new(
            dog_symbol.id.clone(),
            animal_symbol.id.clone(),
            EdgeKind::Extends
        )));
    }

    #[test]
    fn an_interface_extending_an_external_interface_gets_a_placeholder() {
        let source = "interface MyList extends java.util.List {}\n";
        let adapter = JavaAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "Module.java");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "Module.java",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );
        let mut symbols = Vec::new();
        let extends_edges = resolve_structural(&sites, &entities, &symbol_table, &mut symbols);

        assert!(symbols.iter().any(|s| s.name.starts_with("external-interface:List")));
        assert_eq!(extends_edges.len(), 1);
    }

    #[test]
    fn resolves_imports_to_an_explicit_external_package_placeholder() {
        let source = "import java.util.List;\nimport static java.lang.Math.PI;\n\nclass C {}\n";
        let adapter = JavaAdapter;
        let ast = adapter.parse(source).unwrap();
        let module_symbol = SymbolEntity::new(SymbolKind::Namespace, "Module.java", "Module.java");

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &SyntaxEntity::new(SyntaxKind::Module, "Module.java", "Module.java"),
            "Module.java",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );

        let (symbols, edges) = resolve_imports(&sites.imports, &ast.source, &module_symbol);

        assert_eq!(symbols.len(), 2);
        assert!(symbols.iter().any(|s| s.name == "external-package:java.util.List"));
        assert!(symbols.iter().any(|s| s.name == "external-package:java.lang.Math.PI"));
        assert_eq!(edges.iter().filter(|e| e.kind == EdgeKind::Imports).count(), 2);
    }

    #[test]
    fn exports_a_public_top_level_class_but_not_a_package_private_one() {
        let source = "public class Public {}\nclass PackagePrivate {}\n";
        let adapter = JavaAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, edges) = adapter.extract(&ast, "Module.java");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table.find_by_syntax_id(&entities[0].id).unwrap().clone();

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "Module.java",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );

        let export_edges =
            resolve_exports(&entities, &edges, &sites.public_types, &module_symbol, &symbol_table);

        let public = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Public")
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
