use tree_sitter::{Node, Parser, Tree};

use crate::adapter::LanguageAdapter;
use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};
use crate::resolve::unresolved_symbol;
use crate::symbols::{ScopeTree, SymbolTable};
use crate::types::TypeRef;

/// C#'s predefined-type tokens (ECMA-334 §8.3.5 "Simple Types" plus `object`,
/// `string`, `void`, and the native-sized `nint`/`nuint`), treated as
/// `TypeRef::Primitive` the same way Java's `PRIMITIVE_TYPES` treats its own
/// `void`-included set. `object`/`string` are technically reference types in
/// C#'s type system, but `tree-sitter-c-sharp`'s grammar tokenizes them
/// alongside the true value types as a single `predefined_type` rule with no
/// further distinction -- kept as `Primitive` here for the same reason
/// Java's `void` is: it's the token the grammar actually hands back, and
/// splitting `object`/`string` out into `Named` would need semantic
/// knowledge this adapter doesn't have at this node.
const PRIMITIVE_TYPES: &[&str] = &[
    "bool", "byte", "char", "decimal", "double", "float", "int", "long", "object", "sbyte",
    "short", "string", "uint", "ulong", "ushort", "nint", "nuint", "void",
];

/// The parsed form a `CSharpAdapter` produces: `tree-sitter`'s `Tree` paired
/// with the source text it was parsed from, the same shape every other
/// `tree-sitter`-backed adapter (`JavaAst`, `GoAst`, ...) uses.
pub struct CSharpAst {
    tree: Tree,
    source: String,
}

/// The C# language adapter: parses C# source via `tree-sitter`/
/// `tree-sitter-c-sharp` and maps it onto the canonical syntax model.
/// Closest structurally to `JavaAdapter` -- explicit `class`/`struct`/
/// `interface`/`enum` keywords, no Go-style structural inference -- but C#
/// uses a single `: Base, IInterface` base-list for a class's superclass
/// *and* its implemented interfaces together (unlike Java's separate
/// `extends`/`implements` keywords), so `resolve_structural` below has to
/// disambiguate the first base-list entry itself; see its own doc comment.
pub struct CSharpAdapter;

impl LanguageAdapter for CSharpAdapter {
    type Ast = CSharpAst;

    fn parse(&self, source: &str) -> Result<Self::Ast> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
            .map_err(|err| Error::Message(format!("failed to load C# grammar: {err}")))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::Message("failed to parse C# source".to_string()))?;
        Ok(CSharpAst {
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
        // Re-derive this file's own entities/edges/sites in one pass -- same
        // approach as every other `tree-sitter`-backed adapter's
        // `resolve_file`, so `sites.bodies` carries the exact same
        // `SyntaxEntity`s (same kind/scope/name, hence same `EntityId`)
        // `extract` already handed the pipeline.
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

        edges.extend(resolve_structural(
            &sites,
            all_entities,
            symbol_table,
            &mut symbols,
        ));

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

/// Sites `extract_items`/`extract_member` collect that `resolve_file` needs
/// the raw tree for, or that describe a structural relationship read
/// directly off an explicit C# base-list/`using`/modifier at extraction
/// time.
#[derive(Default)]
struct ResolutionSites<'a> {
    /// A method/constructor's own entity paired with its body node -- either
    /// a `block` or an `arrow_expression_clause` (expression-bodied member).
    bodies: Vec<(SyntaxEntity, Node<'a>)>,
    /// Every `using_directive`.
    imports: Vec<Node<'a>>,
    /// A class/record's own entity paired with its base-list's *first*
    /// entry, the only one that can possibly name a superclass -- C#'s
    /// single base-list doesn't distinguish "extends" from "implements"
    /// syntactically, so `resolve_structural` decides Extends vs. Implements
    /// for these, not extraction.
    class_first_bases: Vec<(SyntaxEntity, String)>,
    /// A class/struct/record/enum's own entity paired with one
    /// unambiguously-implemented interface name: every base-list entry
    /// after the first (classes/records) or every entry (structs, which
    /// cannot extend a class or another struct).
    implemented_interfaces: Vec<(SyntaxEntity, String)>,
    /// An interface's own entity paired with one of its base-list's
    /// superinterfaces (interface-to-interface inheritance is unambiguous:
    /// C# interfaces can only list other interfaces there).
    extended_interfaces: Vec<(SyntaxEntity, String)>,
    /// Every class/struct/interface/enum/record entity declared with an
    /// explicit `public` modifier -- `SyntaxEntity` carries no visibility
    /// field, so `resolve_exports` needs this recorded at extraction time.
    public_types: Vec<crate::model::EntityId>,
}

/// Whether `node` (a `class_declaration`/`struct_declaration`/
/// `interface_declaration`/`enum_declaration`/`record_declaration`) carries
/// an explicit `public` modifier. Unlike Java's grammar, which wraps its
/// modifiers in a `modifiers` node, `tree-sitter-c-sharp` exposes each
/// `modifier` as a direct, unwrapped child.
fn has_public_modifier(node: Node, src: &str) -> bool {
    let mut cursor = node.walk();
    let result = node
        .children(&mut cursor)
        .any(|c| c.kind() == "modifier" && text(c, src) == "public");
    result
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

/// Record `parent CONTAINS child` only (fields/properties, parameters,
/// local variables, statements), then push `child` into `entities`.
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

/// A type expression's base declared name, ignoring generic type arguments
/// (`List<string>` -> `List`), qualification (`System.Collections.List` ->
/// `List`), and nullability (`string?` -> `string`) -- what a
/// `SyntaxEntity`'s own `name` field holds for the class/interface/struct it
/// names, so it can be looked up by exact match the same way Java's
/// `type_base_name` does.
fn type_base_name(node: Node, src: &str) -> String {
    match node.kind() {
        "identifier" => text(node, src).to_string(),
        "generic_name" => node
            .named_child(0)
            .map(|c| type_base_name(c, src))
            .unwrap_or_else(|| text(node, src).to_string()),
        "qualified_name" => node
            .child_by_field_name("name")
            .map(|c| type_base_name(c, src))
            .unwrap_or_else(|| text(node, src).to_string()),
        "nullable_type" => node
            .child_by_field_name("type")
            .map(|c| type_base_name(c, src))
            .unwrap_or_else(|| text(node, src).to_string()),
        _ => text(node, src).to_string(),
    }
}

/// `node`'s `base_list` child (`: Base, IInterface`), if any -- unlike
/// Java's fielded `superclass`/`interfaces`, C#'s grammar exposes it as an
/// unfielded child of the declaration node (found by kind), shared by
/// `class_declaration`/`struct_declaration`/`interface_declaration`/
/// `enum_declaration`/`record_declaration` alike.
fn find_base_list(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    let result = node.children(&mut cursor).find(|c| c.kind() == "base_list");
    result
}

/// `base_list`'s individual type nodes, in source order. `base_list`'s own
/// rule is `seq(':', commaSep1(seq($.type, optional($.argument_list))))` --
/// an optional `argument_list` (a record's primary-constructor base call,
/// e.g. `: Base(x, y)`) can follow a type, so `argument_list` children are
/// filtered out here rather than mistaken for an extra base type.
fn base_list_types<'a>(base_list: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = base_list.walk();
    base_list
        .named_children(&mut cursor)
        .filter(|c| c.kind() != "argument_list")
        .collect()
}

/// Whether `name` looks like a conventionally-named C# interface (an `I`
/// followed by an uppercase letter, e.g. `IDisposable`, `IShape`) -- the
/// fallback `resolve_structural` uses to classify a class's first base-list
/// entry as Extends vs. Implements when it doesn't resolve to a known entity
/// in this repository. A heuristic, not a language rule: nothing stops a
/// class from being named `IThing`, or an interface from being named
/// `Shape` -- documented as approximate, not attempted to be made exact.
fn looks_like_interface_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('I')) && chars.next().is_some_and(|c| c.is_uppercase())
}

/// `node`'s own `variable_declaration` child (a `field_declaration` or
/// `local_declaration_statement` each wrap exactly one, unfielded, found by
/// kind).
fn variable_declaration_of<'a>(node: Node<'a>) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let result = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "variable_declaration");
    result
}

/// Every `variable_declarator`'s `"name"` field directly under
/// `variable_declaration` (possibly repeated for `int a, b;`). A tuple
/// destructuring declarator (`var (a, b) = ...`) binds no single name here
/// -- a documented gap, not guessed at.
fn declarator_names<'a>(var_decl: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = var_decl.walk();
    var_decl
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "variable_declarator")
        .filter_map(|d| d.child_by_field_name("name"))
        .collect()
}

/// A `variable_declarator`'s initializer expression (the `= expr` part), if
/// any. Unlike Java's fielded `"value"`, C#'s grammar leaves it unfielded --
/// it's the declarator's last named child, as long as that isn't the `name`
/// field itself (no initializer) or a `bracketed_argument_list` (an indexer
/// declarator, rare and not modeled further here).
fn declarator_value<'a>(declarator: Node<'a>) -> Option<Node<'a>> {
    let name_node = declarator.child_by_field_name("name");
    let count = declarator.named_child_count();
    if count == 0 {
        return None;
    }
    let last = declarator.named_child(count - 1)?;
    if name_node.is_some_and(|n| n.id() == last.id()) {
        return None;
    }
    if last.kind() == "bracketed_argument_list" {
        return None;
    }
    Some(last)
}

/// Walk a `compilation_unit`/namespace body's direct children, dispatching
/// top-level and nested declarations. Tracks a mutable "current namespace"
/// parent/scope because C# 10's file-scoped `namespace Foo;` form (unlike
/// the block form `namespace Foo { ... }`) applies to every sibling that
/// follows it in the same file, not to a nested body -- there is no single
/// child node to recurse into for that case.
fn extract_items<'a>(
    root: Node<'a>,
    src: &'a str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let mut current_parent = parent.clone();
    let mut current_scope = scope.to_string();

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        match child.kind() {
            "using_directive" => sites.imports.push(child),
            "namespace_declaration" => {
                let Some(name_node) = child.child_by_field_name("name") else {
                    continue;
                };
                let name = text(name_node, src);
                // Namespaces map onto `Module`, the same canonical kind this
                // adapter uses for the file itself -- a namespace is a named
                // scope containing type declarations, structurally closer to
                // a nested module than to Java's package (which gets no
                // entity of its own, only a naming convention).
                let entity = SyntaxEntity::new(SyntaxKind::Module, &current_scope, name);
                let ns_scope = child_scope(&current_scope, name);
                declare(&current_parent, entity.clone(), entities, edges);
                if let Some(body) = child.child_by_field_name("body") {
                    extract_items(body, src, &entity, &ns_scope, entities, edges, sites);
                }
            }
            "file_scoped_namespace_declaration" => {
                let Some(name_node) = child.child_by_field_name("name") else {
                    continue;
                };
                let name = text(name_node, src);
                let entity = SyntaxEntity::new(SyntaxKind::Module, &current_scope, name);
                let ns_scope = child_scope(&current_scope, name);
                declare(&current_parent, entity.clone(), entities, edges);
                current_parent = entity;
                current_scope = ns_scope;
            }
            // `global_statement` (top-level statements, C# 9+ minimal
            // programs) has no declared name to attach a Phase 3 entity to
            // under this adapter's shape -- a documented gap, matching
            // Java's own undeclared `static_initializer` handling.
            "global_statement" => {}
            _ => extract_member(
                child,
                src,
                &current_parent,
                &current_scope,
                entities,
                edges,
                sites,
            ),
        }
    }
}

/// A class/struct/interface/enum/record body's direct member declarations,
/// or a top-level declaration reached via `extract_items`. Shared by both
/// call sites so they don't drift out of sync, the same way Java's
/// `extract_member` is shared by `class_body`/`interface_body`.
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
        "field_declaration" => {
            if let Some(var_decl) = variable_declaration_of(node) {
                for declarator in declarator_names(var_decl) {
                    let entity =
                        SyntaxEntity::new(SyntaxKind::Variable, owner_scope, text(declarator, src));
                    contain(owner, entity, entities, edges);
                }
            }
        }
        "property_declaration" => {
            // Canonical-model choice, made once and documented rather than
            // re-litigated per property: a property is modeled as a `Field`
            // (`SyntaxKind::Variable`), the same shape a plain field gets,
            // rather than as a Method pair for its `get`/`set` accessors.
            // The accessor bodies themselves aren't decomposed into
            // Statement/Variable entities or resolved for calls -- a
            // documented gap, consistent with this adapter also not
            // decomposing an expression-bodied method's arrow body.
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let entity = SyntaxEntity::new(SyntaxKind::Variable, owner_scope, text(name_node, src));
            contain(owner, entity, entities, edges);
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
            // An interface method signature (`;` instead of a body) has
            // nothing further to extract/resolve.
            if let Some(body) = node.child_by_field_name("body") {
                if body.kind() == "block" {
                    extract_body(body, src, &entity, &method_scope, entities, edges);
                }
                // An expression-bodied member (`=> expr;`) isn't decomposed
                // into Statement/Variable entities here -- still recorded
                // for reference/call resolution below.
                sites.bodies.push((entity, body));
            }
        }
        "constructor_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let name = text(name_node, src);
            // Modeled as a `Method`, same as every other member function --
            // the canonical model has no separate constructor kind, matching
            // Java's own constructor handling.
            let entity = SyntaxEntity::new(SyntaxKind::Method, owner_scope, name);
            let method_scope = child_scope(owner_scope, name);
            declare(owner, entity.clone(), entities, edges);

            if let Some(params) = node.child_by_field_name("parameters") {
                extract_parameters(params, src, &entity, &method_scope, entities, edges);
            }
            if let Some(body) = node.child_by_field_name("body") {
                if body.kind() == "block" {
                    extract_body(body, src, &entity, &method_scope, entities, edges);
                }
                sites.bodies.push((entity, body));
            }
        }
        "class_declaration" => extract_class(node, src, owner, owner_scope, entities, edges, sites),
        "struct_declaration" => {
            extract_struct(node, src, owner, owner_scope, entities, edges, sites)
        }
        // A `record`/`record class`/`record struct` maps onto the same
        // `Class` entity an ordinary class gets -- the canonical model has
        // no separate record kind, and a record's compiler-synthesized
        // members (positional-parameter properties, `Equals`/`GetHashCode`/
        // `ToString`/`Deconstruct`, `with`-expression support) aren't
        // modeled, the same posture Java's adapter takes toward enum
        // constants' auto-generated `values()`/`valueOf()`.
        "record_declaration" => {
            extract_class(node, src, owner, owner_scope, entities, edges, sites)
        }
        "interface_declaration" => {
            extract_interface(node, src, owner, owner_scope, entities, edges, sites)
        }
        "enum_declaration" => extract_enum(node, src, owner, owner_scope, entities, edges, sites),
        // An indexer, event, delegate, operator/conversion-operator, or
        // destructor declaration has no clean Phase 3 entity to become under
        // this adapter's shape -- a documented gap, not guessed at (an
        // indexer's `this[...]` binds no name; an operator's name is a
        // symbol, not an identifier).
        "indexer_declaration"
        | "event_declaration"
        | "event_field_declaration"
        | "delegate_declaration"
        | "operator_declaration"
        | "conversion_operator_declaration"
        | "destructor_declaration"
        | ";" => {}
        _ => {}
    }
}

/// A `class_declaration`: its own `Class` entity, its base-list's structural
/// relations (recorded in `sites` for `resolve_structural`, read directly
/// off the base list rather than inferred), and its body's members.
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
    if has_public_modifier(node, src) {
        sites.public_types.push(entity.id.clone());
    }
    declare(parent, entity.clone(), entities, edges);

    if let Some(base_list) = find_base_list(node) {
        for (index, type_node) in base_list_types(base_list).into_iter().enumerate() {
            let type_name = type_base_name(type_node, src);
            if index == 0 {
                sites.class_first_bases.push((entity.clone(), type_name));
            } else {
                sites
                    .implemented_interfaces
                    .push((entity.clone(), type_name));
            }
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for member in body.children(&mut cursor) {
            extract_member(member, src, &entity, &class_scope, entities, edges, sites);
        }
    }
}

/// A `struct_declaration`: its own `Struct` entity and its body's members. A
/// struct's base list can only ever list interfaces (C# forbids a struct
/// extending a class or another struct), so unlike `extract_class` every
/// base-list entry here is unambiguously `Implements` -- no
/// `class_first_bases` disambiguation needed.
fn extract_struct<'a>(
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
    let entity = SyntaxEntity::new(SyntaxKind::Struct, scope, name);
    let struct_scope = child_scope(scope, name);
    if has_public_modifier(node, src) {
        sites.public_types.push(entity.id.clone());
    }
    declare(parent, entity.clone(), entities, edges);

    if let Some(base_list) = find_base_list(node) {
        for type_node in base_list_types(base_list) {
            sites
                .implemented_interfaces
                .push((entity.clone(), type_base_name(type_node, src)));
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for member in body.children(&mut cursor) {
            extract_member(member, src, &entity, &struct_scope, entities, edges, sites);
        }
    }
}

/// An `interface_declaration`: its own `Interface` entity, `Extends`
/// relations to its superinterfaces (unambiguous -- an interface's base list
/// can only name other interfaces), and its body's members.
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
    if has_public_modifier(node, src) {
        sites.public_types.push(entity.id.clone());
    }
    declare(parent, entity.clone(), entities, edges);

    if let Some(base_list) = find_base_list(node) {
        for type_node in base_list_types(base_list) {
            sites
                .extended_interfaces
                .push((entity.clone(), type_base_name(type_node, src)));
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for member in body.children(&mut cursor) {
            extract_member(member, src, &entity, &iface_scope, entities, edges, sites);
        }
    }
}

/// An `enum_declaration`: its own `Enum` entity and its constants (modeled
/// as `Variable` entities, matching Java's enum-constant handling). An
/// enum's own `base_list`, if present, names its *underlying integral type*
/// (`enum Color : byte`) -- a completely different meaning from a class's or
/// interface's base list -- so it is deliberately not read into
/// `class_first_bases`/`implemented_interfaces` here: a correct absence, not
/// a gap, the same posture the C adapter takes toward EXTENDS/IMPLEMENTS
/// never existing for a language with no inheritance at all.
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
    if has_public_modifier(node, src) {
        sites.public_types.push(entity.id.clone());
    }
    declare(parent, entity.clone(), entities, edges);

    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() != "enum_member_declaration" {
            continue;
        }
        let Some(const_name) = child.child_by_field_name("name") else {
            continue;
        };
        let const_entity =
            SyntaxEntity::new(SyntaxKind::Variable, &enum_scope, text(const_name, src));
        contain(&entity, const_entity, entities, edges);
    }
}

/// A `parameter_list`'s individual `parameter` nodes become `Parameter`
/// entities. A `params T[] name` parameter is a special case:
/// `tree-sitter-c-sharp`'s `_parameter_array` rule is hidden (leading `_`,
/// unaliased), so its matched tokens -- including the `name`-fielded
/// identifier -- surface as direct, unwrapped children of `parameter_list`
/// rather than nested inside a `parameter` node; the bare `"identifier"`
/// branch below catches that case.
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
            "parameter" => {
                if let Some(name_node) = param.child_by_field_name("name") {
                    let entity =
                        SyntaxEntity::new(SyntaxKind::Parameter, scope, text(name_node, src));
                    contain(parent, entity, entities, edges);
                }
            }
            "identifier" => {
                let entity = SyntaxEntity::new(SyntaxKind::Parameter, scope, text(param, src));
                contain(parent, entity, entities, edges);
            }
            _ => {}
        }
    }
}

/// Walk a method/constructor body's direct statements (a `block`'s
/// `repeat($.statement)`, all named): a `local_declaration_statement`
/// becomes `Variable` entities; everything else becomes a generic
/// `Statement` entity. A local type/local-function declaration inside a
/// body isn't extracted -- a documented gap, same as Java's own
/// `extract_body` not walking into nested item definitions.
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
            "local_declaration_statement" => {
                if let Some(var_decl) = variable_declaration_of(stmt) {
                    for name_node in declarator_names(var_decl) {
                        let entity = SyntaxEntity::new(
                            SyntaxKind::Variable,
                            &format!("{scope}#{index}"),
                            text(name_node, src),
                        );
                        contain(parent, entity, entities, edges);
                    }
                }
            }
            "class_declaration"
            | "struct_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "local_function_statement" => {}
            _ => {
                let entity =
                    SyntaxEntity::new(SyntaxKind::Statement, scope, format!("stmt_{index}"));
                contain(parent, entity, entities, edges);
            }
        }
    }
}

/// An `argument_list`'s individual `argument` nodes' own expressions --
/// `argument`'s rule is `seq(optional(name ':'), optional(ref/out/in),
/// choice(expression, declaration_expression))`, none of it fielded, so the
/// expression is simply an `argument`'s last named child.
fn argument_expressions<'a>(args: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = args.walk();
    args.named_children(&mut cursor)
        .filter(|c| c.kind() == "argument")
        .filter_map(|arg| {
            let count = arg.named_child_count();
            if count == 0 {
                None
            } else {
                arg.named_child(count - 1)
            }
        })
        .collect()
}

/// `REFERENCES`/`CALLS`/`INSTANTIATES` edges from one method/constructor/
/// expression-bodied-member body. Mirrors Java's `resolve_body` shape: a
/// bare identifier read resolves via `scope_tree`; a bare-name call
/// (`Helper()`) resolves the same way; a receiver-qualified call
/// (`obj.Method()`) resolves by best-effort name match against any `Method`
/// symbol in the file; `new Type(...)` whose type resolves in scope to a
/// `Type` symbol is an `INSTANTIATES` site. Anything unresolved gets an
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

    match body.kind() {
        "block" => {
            let mut cursor = body.walk();
            for stmt in body.named_children(&mut cursor) {
                walk_stmt(
                    stmt,
                    &mut |n| names.push(n),
                    &mut |n| calls.push(n),
                    &mut |n| news.push(n),
                );
            }
        }
        "arrow_expression_clause" => {
            if let Some(expr) = body.named_child(0) {
                walk_expr(
                    expr,
                    &mut |n| names.push(n),
                    &mut |n| calls.push(n),
                    &mut |n| news.push(n),
                );
            }
        }
        _ => {}
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
        edges.push(Edge::new(
            member_symbol.id.clone(),
            target.id.clone(),
            EdgeKind::References,
        ));
    }

    for call_node in &calls {
        let Some(function) = call_node.child_by_field_name("function") else {
            continue;
        };
        let (ident, qualified) = match function.kind() {
            "identifier" => (text(function, src), false),
            "generic_name" => (
                function
                    .named_child(0)
                    .map(|n| text(n, src))
                    .unwrap_or_else(|| text(function, src)),
                false,
            ),
            "member_access_expression" => (
                function
                    .child_by_field_name("name")
                    .map(|n| text(n, src))
                    .unwrap_or(""),
                true,
            ),
            _ => (text(function, src), true),
        };
        if ident.is_empty() {
            continue;
        }
        let target = if qualified {
            // Receiver-qualified: best-effort name match, same posture as
            // Java's own `method_invocation` handling.
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
                .or_else(|| {
                    // The scope tree only covers this file's own entities,
                    // so a call to a method declared in another file never
                    // resolves through it -- fall back to a repo-wide
                    // best-effort name match against `symbol_table`,
                    // mirroring the receiver-qualified branch above, which
                    // has always searched repo-wide.
                    symbol_table
                        .symbols()
                        .find(|s| s.kind == SymbolKind::Method && s.name == ident)
                        .cloned()
                })
                .unwrap_or_else(|| {
                    let placeholder = unresolved_symbol("unknown-local", ident);
                    symbols.push(placeholder.clone());
                    placeholder
                })
        };
        edges.push(Edge::new(
            member_symbol.id.clone(),
            target.id.clone(),
            EdgeKind::Calls,
        ));
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
        let type_symbol = crate::types::type_symbol_for(&TypeRef::Named {
            name: ident,
            type_args: Vec::new(),
        });
        symbols.push(type_symbol.clone());
        edges.push(Edge::new(
            member_symbol.id.clone(),
            type_symbol.id,
            EdgeKind::Instantiates,
        ));
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
        "invocation_expression" => {
            on_call(node);
            if let Some(function) = node.child_by_field_name("function") {
                match function.kind() {
                    // A bare-name call (`Helper()`) is also a name usage of
                    // `Helper` itself, matching Java's own bare-call
                    // handling -- without this, a body whose only expression
                    // is a bare call produces a CALLS edge but no REFERENCES
                    // edge for the same name.
                    "identifier" => on_name(function),
                    _ => walk_expr(function, on_name, on_call, on_new),
                }
            }
            if let Some(args) = node.child_by_field_name("arguments") {
                for arg_expr in argument_expressions(args) {
                    walk_expr(arg_expr, on_name, on_call, on_new);
                }
            }
        }
        "object_creation_expression" => {
            on_new(node);
            if let Some(args) = node.child_by_field_name("arguments") {
                for arg_expr in argument_expressions(args) {
                    walk_expr(arg_expr, on_name, on_call, on_new);
                }
            }
        }
        "member_access_expression" => {
            if let Some(receiver) = node.child_by_field_name("expression") {
                walk_expr(receiver, on_name, on_call, on_new);
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
        "prefix_unary_expression" | "postfix_unary_expression" => {
            if let Some(operand) = node.named_child(0) {
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
        "element_access_expression" => {
            if let Some(expr) = node.child_by_field_name("expression") {
                walk_expr(expr, on_name, on_call, on_new);
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
        "local_declaration_statement" => {
            if let Some(var_decl) = variable_declaration_of(node) {
                let mut cursor = var_decl.walk();
                for declarator in var_decl.named_children(&mut cursor) {
                    if declarator.kind() != "variable_declarator" {
                        continue;
                    }
                    if let Some(value) = declarator_value(declarator) {
                        walk_expr(value, on_name, on_call, on_new);
                    }
                }
            }
        }
        "return_statement" | "throw_statement" | "yield_statement" => {
            if let Some(expr) = node.named_child(0) {
                walk_expr(expr, on_name, on_call, on_new);
            }
        }
        "if_statement" => {
            if let Some(condition) = node.child_by_field_name("condition") {
                walk_expr(condition, on_name, on_call, on_new);
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
                walk_expr(condition, on_name, on_call, on_new);
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
            if let Some(init) = node.child_by_field_name("initializer") {
                if init.kind() == "variable_declaration" {
                    let mut cursor = init.walk();
                    for declarator in init.named_children(&mut cursor) {
                        if declarator.kind() != "variable_declarator" {
                            continue;
                        }
                        if let Some(value) = declarator_value(declarator) {
                            walk_expr(value, on_name, on_call, on_new);
                        }
                    }
                } else {
                    walk_expr(init, on_name, on_call, on_new);
                }
            }
            if let Some(body) = node.child_by_field_name("body") {
                walk_stmt(body, on_name, on_call, on_new);
            }
        }
        "foreach_statement" => {
            if let Some(right) = node.child_by_field_name("right") {
                walk_expr(right, on_name, on_call, on_new);
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
        "block" => {
            let mut cursor = node.walk();
            for stmt in node.named_children(&mut cursor) {
                walk_stmt(stmt, on_name, on_call, on_new);
            }
        }
        // try/switch/using-statement/local functions and other less common
        // control flow are a documented gap here, matching Java's own
        // limited `walk_stmt` coverage -- not silently miscounted, simply
        // not descended into.
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

/// This repository's own kind for a type named `name`, if it declares one --
/// used by `resolve_structural` to decide whether a class's first base-list
/// entry is a superclass (`Extends`) or an interface (`Implements`).
fn find_type_entity_kind(all_entities: &[SyntaxEntity], name: &str) -> Option<SyntaxKind> {
    all_entities
        .iter()
        .find(|e| {
            matches!(
                e.kind,
                SyntaxKind::Class | SyntaxKind::Struct | SyntaxKind::Interface
            ) && e.name == name
        })
        .map(|e| e.kind)
}

/// Resolve `sites`' base-list relations into `EXTENDS`/`IMPLEMENTS` edges,
/// searched repository-wide (`all_entities`) so a superclass/superinterface
/// declared in a different file still resolves -- matching Java's own
/// cross-file `resolve_structural` reach.
///
/// The interesting case is `class_first_bases`: C#'s single `: Base,
/// IInterface` base-list syntax doesn't distinguish a superclass from an
/// implemented interface the way Java's separate `extends`/`implements`
/// keywords do, so only the *first* entry can possibly be a superclass (C#
/// requires it to come first when present), and even then only if it
/// actually is one. This is resolved here, not at extraction time: look up
/// the name repository-wide and classify by what it actually resolves to
/// (`Interface` -> Implements, `Class`/`Struct` -> Extends); when it
/// resolves to nothing in this repository (an external/BCL type, e.g.
/// `Exception` or `IDisposable`), fall back to `looks_like_interface_name`'s
/// naming-convention heuristic. This is genuinely approximate for external
/// types with unconventional names -- documented as a heuristic, not
/// silently assumed correct.
fn resolve_structural(
    sites: &ResolutionSites,
    all_entities: &[SyntaxEntity],
    symbol_table: &SymbolTable,
    symbols: &mut Vec<SymbolEntity>,
) -> Vec<Edge> {
    let mut edges = Vec::new();

    for (owner_entity, base_name) in &sites.class_first_bases {
        let Some(owner_symbol) = symbol_table.find_by_syntax_id(&owner_entity.id) else {
            continue;
        };
        match find_type_entity_kind(all_entities, base_name) {
            Some(SyntaxKind::Interface) => {
                let target = find_type_symbol(
                    all_entities,
                    symbol_table,
                    &[SyntaxKind::Interface],
                    base_name,
                )
                .unwrap_or_else(|| {
                    let placeholder = unresolved_symbol("external-interface", base_name);
                    symbols.push(placeholder.clone());
                    placeholder
                });
                edges.push(Edge::new(
                    owner_symbol.id.clone(),
                    target.id,
                    EdgeKind::Implements,
                ));
            }
            Some(SyntaxKind::Class) | Some(SyntaxKind::Struct) => {
                let target = find_type_symbol(
                    all_entities,
                    symbol_table,
                    &[SyntaxKind::Class, SyntaxKind::Struct],
                    base_name,
                )
                .unwrap_or_else(|| {
                    let placeholder = unresolved_symbol("external-class", base_name);
                    symbols.push(placeholder.clone());
                    placeholder
                });
                edges.push(Edge::new(
                    owner_symbol.id.clone(),
                    target.id,
                    EdgeKind::Extends,
                ));
            }
            _ => {
                if looks_like_interface_name(base_name) {
                    let placeholder = unresolved_symbol("external-interface", base_name);
                    symbols.push(placeholder.clone());
                    edges.push(Edge::new(
                        owner_symbol.id.clone(),
                        placeholder.id,
                        EdgeKind::Implements,
                    ));
                } else {
                    let placeholder = unresolved_symbol("external-class", base_name);
                    symbols.push(placeholder.clone());
                    edges.push(Edge::new(
                        owner_symbol.id.clone(),
                        placeholder.id,
                        EdgeKind::Extends,
                    ));
                }
            }
        }
    }

    for (entity, interface_name) in &sites.implemented_interfaces {
        let Some(owner_symbol) = symbol_table.find_by_syntax_id(&entity.id) else {
            continue;
        };
        let target = find_type_symbol(
            all_entities,
            symbol_table,
            &[SyntaxKind::Interface],
            interface_name,
        )
        .unwrap_or_else(|| {
            let placeholder = unresolved_symbol("external-interface", interface_name);
            symbols.push(placeholder.clone());
            placeholder
        });
        edges.push(Edge::new(
            owner_symbol.id.clone(),
            target.id,
            EdgeKind::Implements,
        ));
    }

    for (iface_entity, super_name) in &sites.extended_interfaces {
        let Some(iface_symbol) = symbol_table.find_by_syntax_id(&iface_entity.id) else {
            continue;
        };
        let target = find_type_symbol(
            all_entities,
            symbol_table,
            &[SyntaxKind::Interface],
            super_name,
        )
        .unwrap_or_else(|| {
            let placeholder = unresolved_symbol("external-interface", super_name);
            symbols.push(placeholder.clone());
            placeholder
        });
        edges.push(Edge::new(
            iface_symbol.id.clone(),
            target.id,
            EdgeKind::Extends,
        ));
    }

    edges
}

/// Resolve a file's `using_directive`s into `IMPORTS` edges from
/// `module_symbol`. Neither a namespace `using` (`using System.Collections;`)
/// nor a `using static`/alias form tells us, without resolving it against
/// this repository's actual namespace layout, whether it names a BCL/
/// third-party namespace or another file in this repository -- out of scope
/// here, so (matching Java's own `resolve_imports` posture) every `using`
/// resolves to an explicit `external-namespace` placeholder rather than
/// guessed at. Keyed by the directive's last named child's text, which for
/// the ordinary form is the dotted name itself, and for the alias form
/// (`using Sys = System;`) is the aliased target type -- the more useful of
/// the two names to record either way.
fn resolve_imports(
    imports: &[Node],
    src: &str,
    module_symbol: &SymbolEntity,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for using_node in imports {
        let mut cursor = using_node.walk();
        let named: Vec<Node> = using_node.named_children(&mut cursor).collect();
        let Some(last) = named.last() else {
            continue;
        };
        let path = text(*last, src);
        if path.is_empty() {
            continue;
        }
        let placeholder = unresolved_symbol("external-namespace", path);
        symbols.push(placeholder.clone());
        edges.push(Edge::new(
            module_symbol.id.clone(),
            placeholder.id,
            EdgeKind::Imports,
        ));
    }

    (symbols, edges)
}

/// Resolve a file's top-level `public` type declarations into `EXPORTS`
/// edges from `module_symbol`: C#'s actual public-API convention -- an
/// explicit `public` modifier (recorded in `public_types` at extraction
/// time, since `SyntaxEntity` carries no visibility field of its own). An
/// `internal` (the C# default for a top-level type with no modifier) or
/// otherwise-restricted declaration is not exported.
fn resolve_exports(
    file_entities: &[SyntaxEntity],
    file_edges: &[Edge],
    public_types: &[crate::model::EntityId],
    module_symbol: &SymbolEntity,
    symbol_table: &SymbolTable,
) -> Vec<Edge> {
    const EXPORTABLE_KINDS: [SyntaxKind; 4] = [
        SyntaxKind::Class,
        SyntaxKind::Struct,
        SyntaxKind::Interface,
        SyntaxKind::Enum,
    ];

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
            edges.push(Edge::new(
                module_symbol.id.clone(),
                symbol.id.clone(),
                EdgeKind::Exports,
            ));
        }
    }
    edges
}

/// `node`'s own `type_parameter_list` -- fielded as `"type_parameters"` on
/// `interface_declaration`/`method_declaration`, but unfielded (found by
/// kind, among a `repeat(choice(type_parameter_list, parameter_list,
/// base_list))`) on `class_declaration`/`struct_declaration`/
/// `record_declaration`.
fn find_type_parameter_list(node: Node) -> Option<Node> {
    node.child_by_field_name("type_parameters").or_else(|| {
        let mut cursor = node.walk();
        let result = node
            .children(&mut cursor)
            .find(|c| c.kind() == "type_parameter_list");
        result
    })
}

/// Map a C# type expression to a `TypeRef`. Not yet wired into
/// `extract`/`resolve_file` -- `ACCEPTS`/`RETURNS` type binding isn't called
/// from the pipeline by any adapter yet, mirroring Java's own
/// `extract_type` gap (tested directly, deliberately not yet wired in
/// either).
///
/// Nullable reference/value types (`string?`, `int?`) fit `TypeRef` as-is by
/// unwrapping to the inner type -- nullability itself isn't modeled (no
/// "this may be null" flag anywhere on `TypeRef`), a smaller version of the
/// same "the canonical model doesn't capture every language nuance" posture
/// as the generic-bound gap below.
///
/// Canonical-model gap, same one Java's `extract_type` already documented:
/// `TypeRef::Generic` carries only a type parameter's name, with nowhere to
/// record a `where T : IComparable<T>` constraint. Not fixed here,
/// consistent with that existing precedent.
#[allow(dead_code)]
fn extract_type(node: Node, src: &str) -> TypeRef {
    match node.kind() {
        "predefined_type" => {
            let name = text(node, src);
            debug_assert!(
                PRIMITIVE_TYPES.contains(&name),
                "grammar's predefined_type token {name:?} not in PRIMITIVE_TYPES"
            );
            TypeRef::Primitive(name.to_string())
        }
        "identifier" => TypeRef::Named {
            name: text(node, src).to_string(),
            type_args: Vec::new(),
        },
        "generic_name" => {
            let name = node
                .named_child(0)
                .map(|c| type_base_name(c, src))
                .unwrap_or_else(|| text(node, src).to_string());
            let type_args = node
                .named_child(1)
                .filter(|n| n.kind() == "type_argument_list")
                .map(|args| {
                    let mut cursor = args.walk();
                    args.named_children(&mut cursor)
                        .map(|arg| extract_type(arg, src))
                        .collect()
                })
                .unwrap_or_default();
            TypeRef::Named { name, type_args }
        }
        "qualified_name" => TypeRef::Named {
            name: type_base_name(node, src),
            type_args: Vec::new(),
        },
        "nullable_type" => node
            .child_by_field_name("type")
            .map(|c| extract_type(c, src))
            .unwrap_or_else(|| TypeRef::Unresolved(text(node, src).to_string())),
        "array_type" => {
            let inner = node
                .child_by_field_name("type")
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

/// A class/interface/struct/method's own type-parameter list (`<T> where T
/// : IComparable<T>`) becomes `Generic` symbols declared under it -- the
/// bound itself is read but discarded, the canonical-model gap documented
/// on `extract_type` above. Not yet wired into `extract`/`resolve_file`,
/// matching Java's own `extract_generic_params` gap.
#[allow(dead_code)]
fn extract_generic_params(
    node: Node,
    src: &str,
    owner_symbol: &SymbolEntity,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    let Some(type_params) = find_type_parameter_list(node) else {
        return (symbols, edges);
    };

    let mut cursor = type_params.walk();
    for param in type_params.children(&mut cursor) {
        if param.kind() != "type_parameter" {
            continue;
        }
        let Some(name_node) = param.child_by_field_name("name") else {
            continue;
        };
        let name = text(name_node, src);
        let symbol = SymbolEntity::new(SymbolKind::Generic, owner_symbol.id.as_str(), name);
        edges.push(Edge::new(
            owner_symbol.id.clone(),
            symbol.id.clone(),
            EdgeKind::Declares,
        ));
        symbols.push(symbol);
    }

    (symbols, edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
        let adapter = CSharpAdapter;
        let ast = adapter.parse(source).expect("valid c# parses");
        adapter.extract(&ast, "Module.cs")
    }

    fn parse_source(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn parses_valid_csharp_source() {
        let adapter = CSharpAdapter;
        assert!(adapter
            .parse("public class Program { public static void Main() {} }")
            .is_ok());
    }

    #[test]
    fn parsing_invalid_syntax_is_a_clear_error() {
        // tree-sitter is error-tolerant by design and always returns *some*
        // tree, so "invalid" C# doesn't surface as an `Err` here -- the same
        // documented difference from `syn`'s hand-rolled Rust parser that
        // Java's own equivalent test notes.
        let adapter = CSharpAdapter;
        assert!(adapter.parse("class {{{ not csharp").is_ok());
    }

    #[test]
    fn extract_always_returns_the_file_module_first() {
        let (entities, _) = extract("");
        assert_eq!(entities[0].kind, SyntaxKind::Module);
        assert_eq!(entities[0].name, "Module.cs");
    }

    #[test]
    fn extracts_a_top_level_class_with_a_method_and_its_parameters() {
        let (entities, edges) = extract(
            "public class Calculator {\n  public int Add(int a, int b) {\n    return a + b;\n  }\n}\n",
        );

        let module = &entities[0];
        let class = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Calculator")
            .expect("Class entity");
        assert!(edges.contains(&Edge::new(
            module.id.clone(),
            class.id.clone(),
            EdgeKind::Declares
        )));

        let method = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "Add")
            .expect("Method entity");
        assert!(edges.contains(&Edge::new(
            class.id.clone(),
            method.id.clone(),
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
        assert!(edges.contains(&Edge::new(
            class.id.clone(),
            ctor.id.clone(),
            EdgeKind::Declares
        )));
    }

    #[test]
    fn extracts_an_interface_with_a_method_signature_and_no_body() {
        let (entities, _edges) = extract("interface IShape {\n  double Area();\n}\n");
        entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Interface && e.name == "IShape")
            .expect("Interface entity");
        entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "Area")
            .expect("Method signature entity, even with no body");
    }

    #[test]
    fn extracts_a_struct() {
        let (entities, edges) = extract("struct Point {\n  public int X;\n}\n");
        let module = &entities[0];
        let point = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Struct && e.name == "Point")
            .expect("Struct entity");
        assert!(edges.contains(&Edge::new(
            module.id.clone(),
            point.id.clone(),
            EdgeKind::Declares
        )));
    }

    #[test]
    fn extracts_a_record_as_a_class() {
        let (entities, _) = extract("public record Person(string Name, int Age);\n");
        entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Person")
            .expect("record modeled as a Class entity");
    }

    #[test]
    fn extracts_an_enum_with_its_constants() {
        let (entities, edges) = extract("enum Color {\n  Red, Green, Blue\n}\n");
        let color = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Enum && e.name == "Color")
            .unwrap();
        let red = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Variable && e.name == "Red")
            .expect("Red constant entity");
        assert!(edges.contains(&Edge::new(
            color.id.clone(),
            red.id.clone(),
            EdgeKind::Contains
        )));
    }

    #[test]
    fn extracts_a_nested_class_scoped_under_its_enclosing_class() {
        let source = "class Outer {\n  class Inner {\n    void Run() {}\n  }\n}\n";
        let (entities, edges) = extract(source);
        let outer = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Outer")
            .unwrap();
        let inner = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Inner")
            .unwrap();
        assert!(edges.contains(&Edge::new(
            outer.id.clone(),
            inner.id.clone(),
            EdgeKind::Declares
        )));
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
        assert!(edges.contains(&Edge::new(
            class.id.clone(),
            field.id.clone(),
            EdgeKind::Contains
        )));
        assert!(!edges.contains(&Edge::new(
            class.id.clone(),
            field.id.clone(),
            EdgeKind::Declares
        )));
    }

    #[test]
    fn extracts_a_property_declaration_as_a_field() {
        let (entities, edges) = extract("class Box {\n  public int Size { get; set; }\n}\n");
        let class = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class)
            .unwrap();
        let property = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Variable && e.name == "Size")
            .expect("property entity modeled as a Field");
        assert!(edges.contains(&Edge::new(
            class.id.clone(),
            property.id.clone(),
            EdgeKind::Contains
        )));
    }

    #[test]
    fn extracts_a_local_variable_declaration_in_a_method_body() {
        let (entities, edges) = extract("class Run {\n  void Go() {\n    int total = 1;\n  }\n}\n");
        let method = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "Go")
            .unwrap();
        let variable = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Variable && e.name == "total")
            .expect("Variable entity");
        assert!(edges.contains(&Edge::new(
            method.id.clone(),
            variable.id.clone(),
            EdgeKind::Contains
        )));
        assert!(!edges.contains(&Edge::new(
            method.id.clone(),
            variable.id.clone(),
            EdgeKind::Declares
        )));
    }

    #[test]
    fn a_non_declaration_statement_becomes_a_generic_statement_entity() {
        let (entities, _) =
            extract("class Run {\n  void Go() {\n    Console.WriteLine(1);\n  }\n}\n");
        assert!(entities.iter().any(|e| e.kind == SyntaxKind::Statement));
    }

    #[test]
    fn same_named_method_on_different_classes_gets_distinct_ids() {
        let source = "class A {\n  void Run() {}\n}\nclass B {\n  void Run() {}\n}\n";
        let (entities, _) = extract(source);
        let methods: Vec<&SyntaxEntity> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Method && e.name == "Run")
            .collect();
        assert_eq!(methods.len(), 2);
        assert_ne!(methods[0].id, methods[1].id);
    }

    #[test]
    fn a_namespace_declaration_becomes_a_nested_module() {
        let source = "namespace Shapes {\n  class Circle {}\n}\n";
        let (entities, edges) = extract(source);
        let file_module = &entities[0];
        let ns_module = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Module && e.name == "Shapes")
            .expect("namespace entity");
        assert!(edges.contains(&Edge::new(
            file_module.id.clone(),
            ns_module.id.clone(),
            EdgeKind::Declares
        )));
        let circle = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Circle")
            .unwrap();
        assert!(edges.contains(&Edge::new(
            ns_module.id.clone(),
            circle.id.clone(),
            EdgeKind::Declares
        )));
    }

    #[test]
    fn a_file_scoped_namespace_scopes_every_following_declaration() {
        let source = "namespace Shapes;\n\nclass Circle {}\nclass Square {}\n";
        let (entities, edges) = extract(source);
        let ns_module = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Module && e.name == "Shapes")
            .expect("file-scoped namespace entity");
        let circle = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Circle")
            .unwrap();
        let square = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Square")
            .unwrap();
        assert!(edges.contains(&Edge::new(
            ns_module.id.clone(),
            circle.id.clone(),
            EdgeKind::Declares
        )));
        assert!(edges.contains(&Edge::new(
            ns_module.id.clone(),
            square.id.clone(),
            EdgeKind::Declares
        )));
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
        let full = "class C {\n  int F() { return 0; }\n}\n";
        let tree = parse_source(full);
        let node = method_named(&tree, full, "F")
            .child_by_field_name("returns")
            .unwrap();
        assert_eq!(
            extract_type(node, full),
            TypeRef::Primitive("int".to_string())
        );
    }

    #[test]
    fn extracts_a_user_defined_named_type() {
        let full = "class C {\n  Circle F() { return null; }\n}\n";
        let tree = parse_source(full);
        let node = method_named(&tree, full, "F")
            .child_by_field_name("returns")
            .unwrap();
        assert_eq!(
            extract_type(node, full),
            TypeRef::Named {
                name: "Circle".to_string(),
                type_args: Vec::new()
            }
        );
    }

    #[test]
    fn extracts_a_generic_named_type_with_its_type_argument() {
        let full = "class C {\n  List<string> F() { return null; }\n}\n";
        let tree = parse_source(full);
        let node = method_named(&tree, full, "F")
            .child_by_field_name("returns")
            .unwrap();
        assert_eq!(
            extract_type(node, full),
            TypeRef::Named {
                name: "List".to_string(),
                type_args: vec![TypeRef::Primitive("string".to_string())],
            }
        );
    }

    #[test]
    fn extracts_a_nullable_type_by_unwrapping_to_its_inner_type() {
        let full = "class C {\n  string? F() { return null; }\n}\n";
        let tree = parse_source(full);
        let node = method_named(&tree, full, "F")
            .child_by_field_name("returns")
            .unwrap();
        assert_eq!(
            extract_type(node, full),
            TypeRef::Primitive("string".to_string())
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
        let adapter = CSharpAdapter;
        let ast = adapter.parse(source).expect("valid c# parses");
        let (entities, edges) = adapter.extract(&ast, "Module.cs");
        let module = entities[0].clone();
        let member_entity = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == member_name)
            .unwrap_or_else(|| panic!("no Method entity named {member_name}"))
            .clone();
        let scope_tree = ScopeTree::build(&module, &entities, &edges);
        let scope = scope_tree
            .scope_of(&member_entity.id)
            .expect("method opens a scope");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let member_symbol = symbol_table
            .find_by_syntax_id(&member_entity.id)
            .expect("method symbol")
            .clone();
        (
            ast.tree,
            Resolved {
                entities,
                scope_tree,
                scope,
                symbol_table,
                member_symbol,
            },
        )
    }

    fn body_of<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        method_named(tree, src, name)
            .child_by_field_name("body")
            .unwrap()
    }

    #[test]
    fn resolves_a_reference_to_an_in_scope_local_variable() {
        let source = "class C {\n  void Run() {\n    int value = 1;\n    Console.WriteLine(value);\n  }\n}\n";
        let (tree, r) = setup(source, "Run");
        let body = body_of(&tree, source, "Run");

        let (_, edges) = resolve_body(
            body,
            source,
            &r.member_symbol,
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
            r.member_symbol.id.clone(),
            value_symbol.id.clone(),
            EdgeKind::References
        )));
    }

    #[test]
    fn a_reference_to_an_undefined_name_gets_an_explicit_placeholder() {
        let source = "class C {\n  void Run() {\n    missing.ToString();\n  }\n}\n";
        let (tree, r) = setup(source, "Run");
        let body = body_of(&tree, source, "Run");

        let (symbols, _edges) = resolve_body(
            body,
            source,
            &r.member_symbol,
            &r.scope_tree,
            r.scope,
            &r.symbol_table,
        );

        assert!(symbols.iter().any(|s| s.name.starts_with("unknown-local")));
    }

    #[test]
    fn resolves_a_free_function_style_call() {
        let source = "class C {\n  void Helper() {}\n  void Run() {\n    Helper();\n  }\n}\n";
        let (tree, r) = setup(source, "Run");
        let body = body_of(&tree, source, "Run");

        let (_, edges) = resolve_body(
            body,
            source,
            &r.member_symbol,
            &r.scope_tree,
            r.scope,
            &r.symbol_table,
        );

        let helper = r
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "Helper")
            .unwrap();
        let helper_symbol = r.symbol_table.find_by_syntax_id(&helper.id).unwrap();
        assert!(edges.contains(&Edge::new(
            r.member_symbol.id.clone(),
            helper_symbol.id.clone(),
            EdgeKind::Calls
        )));
    }

    #[test]
    fn a_free_call_to_a_method_declared_in_another_file_resolves_via_repo_wide_fallback() {
        // Regression test for the cross-file CALLS gap found via Phase 18.1's
        // real-world benchmarking: the scope tree only ever covered the
        // calling file's own entities, so a bare (no-receiver) call to a
        // method declared in another file fell straight to an
        // `unknown-local` placeholder.
        use crate::symbols::build_symbol_table;

        let adapter = CSharpAdapter;
        let helper_ast = adapter
            .parse("class Helper {\n  void Helper_() {}\n}\n")
            .unwrap();
        let (helper_entities, helper_edges) = adapter.extract(&helper_ast, "Helper.cs");
        let caller_ast = adapter
            .parse("class Run {\n  void Go() {\n    Helper_();\n  }\n}\n")
            .unwrap();
        let (caller_entities, caller_edges) = adapter.extract(&caller_ast, "Run.cs");

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
            "Run.cs",
            &all_entities,
            &symbol_table,
            &all_edges,
            module_symbol,
            caller_module,
        );

        let helper = helper_entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "Helper_")
            .unwrap();
        let helper_symbol = symbol_table.find_by_syntax_id(&helper.id).unwrap();
        let go = caller_entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "Go")
            .unwrap();
        let go_symbol = symbol_table.find_by_syntax_id(&go.id).unwrap();

        assert!(
            edges.contains(&Edge::new(
                go_symbol.id.clone(),
                helper_symbol.id.clone(),
                EdgeKind::Calls
            )),
            "expected a real CALLS edge to helper's resolved symbol, not an unresolved placeholder: {edges:?}"
        );
    }

    #[test]
    fn resolves_a_receiver_qualified_call_by_best_effort_name_match() {
        let source =
            "class Shape {\n  double Area() { return 0; }\n}\nclass Run {\n  void Go(Shape shape) {\n    shape.Area();\n  }\n}\n";
        let (tree, r) = setup(source, "Go");
        let body = body_of(&tree, source, "Go");

        let (_, edges) = resolve_body(
            body,
            source,
            &r.member_symbol,
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
            r.member_symbol.id.clone(),
            area_symbol.id.clone(),
            EdgeKind::Calls
        )));
    }

    #[test]
    fn resolves_an_object_creation_instantiation_for_an_in_scope_type() {
        let source =
            "class Circle {}\nclass Run {\n  void Go() {\n    Circle c = new Circle();\n  }\n}\n";
        let (tree, r) = setup(source, "Go");
        let body = body_of(&tree, source, "Go");

        let (_, edges) = resolve_body(
            body,
            source,
            &r.member_symbol,
            &r.scope_tree,
            r.scope,
            &r.symbol_table,
        );

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
        let source = "class Run {\n  void Go() {\n    var o = new object();\n  }\n}\n";
        let (tree, r) = setup(source, "Go");
        let body = body_of(&tree, source, "Go");

        let (symbols, edges) = resolve_body(
            body,
            source,
            &r.member_symbol,
            &r.scope_tree,
            r.scope,
            &r.symbol_table,
        );

        assert!(symbols.iter().any(|s| s.name == "object"));
        assert!(edges
            .iter()
            .any(|e| e.kind == EdgeKind::Instantiates && e.from == r.member_symbol.id));
    }

    #[test]
    fn resolves_an_expression_bodied_method_call() {
        let source = "class C {\n  void Helper() {}\n  void Run() => Helper();\n}\n";
        let (tree, r) = setup(source, "Run");
        let body = body_of(&tree, source, "Run");
        assert_eq!(body.kind(), "arrow_expression_clause");

        let (_, edges) = resolve_body(
            body,
            source,
            &r.member_symbol,
            &r.scope_tree,
            r.scope,
            &r.symbol_table,
        );

        let helper = r
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "Helper")
            .unwrap();
        let helper_symbol = r.symbol_table.find_by_syntax_id(&helper.id).unwrap();
        assert!(edges.contains(&Edge::new(
            r.member_symbol.id.clone(),
            helper_symbol.id.clone(),
            EdgeKind::Calls
        )));
    }

    fn extract_with_sites(
        source: &str,
    ) -> (Vec<SyntaxEntity>, SymbolTable, ResolutionSites<'static>) {
        // Leaks the parsed tree so `ResolutionSites`' borrowed nodes can
        // outlive this helper -- test-only, mirroring the shape Java's own
        // structural-resolution tests use via a `Tree` kept alive in the
        // caller; here it's simplest to leak since the tree itself isn't
        // otherwise needed by the caller.
        let adapter = CSharpAdapter;
        let ast = adapter.parse(source).unwrap();
        let ast: &'static CSharpAst = Box::leak(Box::new(ast));
        let (entities, _edges) = adapter.extract(ast, "Module.cs");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "Module.cs",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );
        (entities, symbol_table, sites)
    }

    #[test]
    fn a_class_implementing_an_interface_gets_an_explicit_implements_edge() {
        let source = "interface IShape {\n  double Area();\n}\nclass Circle : IShape {\n  public double Area() { return 0; }\n}\n";
        let (entities, symbol_table, sites) = extract_with_sites(source);
        let mut symbols = Vec::new();
        let edges = resolve_structural(&sites, &entities, &symbol_table, &mut symbols);

        let circle = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Circle")
            .unwrap();
        let shape = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Interface && e.name == "IShape")
            .unwrap();
        let circle_symbol = symbol_table.find_by_syntax_id(&circle.id).unwrap();
        let shape_symbol = symbol_table.find_by_syntax_id(&shape.id).unwrap();
        assert!(edges.contains(&Edge::new(
            circle_symbol.id.clone(),
            shape_symbol.id.clone(),
            EdgeKind::Implements
        )));
    }

    #[test]
    fn a_class_extending_a_class_gets_an_explicit_extends_edge() {
        let source = "class Animal {}\nclass Dog : Animal {}\n";
        let (entities, symbol_table, sites) = extract_with_sites(source);
        let mut symbols = Vec::new();
        let edges = resolve_structural(&sites, &entities, &symbol_table, &mut symbols);

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
        assert!(edges.contains(&Edge::new(
            dog_symbol.id.clone(),
            animal_symbol.id.clone(),
            EdgeKind::Extends
        )));
    }

    #[test]
    fn a_class_with_a_base_class_and_an_interface_gets_both_edges() {
        let source =
            "interface IShape {\n  double Area();\n}\nclass Animal {}\nclass Dog : Animal, IShape {\n  public double Area() { return 0; }\n}\n";
        let (entities, symbol_table, sites) = extract_with_sites(source);
        let mut symbols = Vec::new();
        let edges = resolve_structural(&sites, &entities, &symbol_table, &mut symbols);

        let dog = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Dog")
            .unwrap();
        let animal = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Animal")
            .unwrap();
        let shape = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Interface && e.name == "IShape")
            .unwrap();
        let dog_symbol = symbol_table.find_by_syntax_id(&dog.id).unwrap();
        let animal_symbol = symbol_table.find_by_syntax_id(&animal.id).unwrap();
        let shape_symbol = symbol_table.find_by_syntax_id(&shape.id).unwrap();
        assert!(edges.contains(&Edge::new(
            dog_symbol.id.clone(),
            animal_symbol.id.clone(),
            EdgeKind::Extends
        )));
        assert!(edges.contains(&Edge::new(
            dog_symbol.id.clone(),
            shape_symbol.id.clone(),
            EdgeKind::Implements
        )));
    }

    #[test]
    fn a_struct_implementing_an_interface_gets_an_implements_edge_not_extends() {
        let source = "interface IShape {\n  double Area();\n}\nstruct Circle : IShape {\n  public double Area() { return 0; }\n}\n";
        let (entities, symbol_table, sites) = extract_with_sites(source);
        let mut symbols = Vec::new();
        let edges = resolve_structural(&sites, &entities, &symbol_table, &mut symbols);

        let circle = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Struct && e.name == "Circle")
            .unwrap();
        let shape = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Interface && e.name == "IShape")
            .unwrap();
        let circle_symbol = symbol_table.find_by_syntax_id(&circle.id).unwrap();
        let shape_symbol = symbol_table.find_by_syntax_id(&shape.id).unwrap();
        assert!(edges.contains(&Edge::new(
            circle_symbol.id.clone(),
            shape_symbol.id.clone(),
            EdgeKind::Implements
        )));
        assert!(!edges.iter().any(|e| e.kind == EdgeKind::Extends));
    }

    #[test]
    fn an_interface_extending_another_interface_gets_an_extends_edge() {
        let source = "interface IBase {}\ninterface IDerived : IBase {}\n";
        let (entities, symbol_table, sites) = extract_with_sites(source);
        let mut symbols = Vec::new();
        let edges = resolve_structural(&sites, &entities, &symbol_table, &mut symbols);

        let derived = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Interface && e.name == "IDerived")
            .unwrap();
        let base = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Interface && e.name == "IBase")
            .unwrap();
        let derived_symbol = symbol_table.find_by_syntax_id(&derived.id).unwrap();
        let base_symbol = symbol_table.find_by_syntax_id(&base.id).unwrap();
        assert!(edges.contains(&Edge::new(
            derived_symbol.id.clone(),
            base_symbol.id.clone(),
            EdgeKind::Extends
        )));
    }

    #[test]
    fn a_class_extending_an_unresolved_external_type_falls_back_to_naming_convention() {
        let source = "class Widget : IDisposable {\n  public void Dispose() {}\n}\n";
        let (entities, symbol_table, sites) = extract_with_sites(source);
        let mut symbols = Vec::new();
        let edges = resolve_structural(&sites, &entities, &symbol_table, &mut symbols);

        assert!(symbols
            .iter()
            .any(|s| s.name.starts_with("external-interface:IDisposable")));
        assert!(edges.iter().all(|e| e.kind == EdgeKind::Implements));
    }

    #[test]
    fn a_class_extending_an_unresolved_non_interface_named_type_defaults_to_extends() {
        let source = "class Widget : Exception {}\n";
        let (entities, symbol_table, sites) = extract_with_sites(source);
        let mut symbols = Vec::new();
        let edges = resolve_structural(&sites, &entities, &symbol_table, &mut symbols);

        assert!(symbols
            .iter()
            .any(|s| s.name.starts_with("external-class:Exception")));
        assert!(edges.iter().all(|e| e.kind == EdgeKind::Extends));
    }

    #[test]
    fn resolves_using_directives_to_an_explicit_external_namespace_placeholder() {
        let source = "using System;\nusing System.Collections.Generic;\n\nclass C {}\n";
        let adapter = CSharpAdapter;
        let ast = adapter.parse(source).unwrap();
        let module_symbol = SymbolEntity::new(SymbolKind::Namespace, "Module.cs", "Module.cs");

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &SyntaxEntity::new(SyntaxKind::Module, "Module.cs", "Module.cs"),
            "Module.cs",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );

        let (symbols, edges) = resolve_imports(&sites.imports, &ast.source, &module_symbol);

        assert_eq!(symbols.len(), 2);
        assert!(symbols
            .iter()
            .any(|s| s.name == "external-namespace:System"));
        assert!(symbols
            .iter()
            .any(|s| s.name == "external-namespace:System.Collections.Generic"));
        assert_eq!(
            edges.iter().filter(|e| e.kind == EdgeKind::Imports).count(),
            2
        );
    }

    #[test]
    fn exports_a_public_top_level_class_but_not_an_internal_one() {
        let source = "public class Public {}\nclass Internal {}\n";
        let adapter = CSharpAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, edges) = adapter.extract(&ast, "Module.cs");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table
            .find_by_syntax_id(&entities[0].id)
            .unwrap()
            .clone();

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "Module.cs",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );

        let export_edges = resolve_exports(
            &entities,
            &edges,
            &sites.public_types,
            &module_symbol,
            &symbol_table,
        );

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
