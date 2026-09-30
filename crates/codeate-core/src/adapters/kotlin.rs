use tree_sitter::{Node, Parser, Tree};

use crate::adapter::LanguageAdapter;
use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};
use crate::resolve::unresolved_symbol;
use crate::symbols::{ScopeTree, SymbolTable};
use crate::types::TypeRef;

/// Kotlin's built-in value types that play the same syntactic role Java's
/// keyword primitives (`int`, `boolean`, ...) do, even though Kotlin has no
/// separate primitive-type grammar production for them -- `Int`, `Boolean`,
/// etc. parse identically to any other user type name (`user_type`), so
/// this list is a name-based heuristic rather than a keyword-based fact the
/// way Java's `PRIMITIVE_TYPES` is. `String`/`Any`/`Nothing`/`Array` are
/// deliberately excluded -- they're ordinary reference types in Kotlin, not
/// the closest analogue to a JVM primitive.
const PRIMITIVE_TYPES: &[&str] = &[
    "Int", "Long", "Short", "Byte", "Double", "Float", "Boolean", "Char", "Unit",
];

/// The parsed form a `KotlinAdapter` produces: `tree-sitter`'s `Tree` paired
/// with the source text it was parsed from, the same shape every other
/// `tree-sitter`-backed adapter (Go/Java/TypeScript/JavaScript) uses.
pub struct KotlinAst {
    tree: Tree,
    source: String,
}

/// The Kotlin language adapter: parses Kotlin source via `tree-sitter`/
/// `tree-sitter-kotlin-ng` and maps it onto the canonical syntax model.
///
/// Parser crate note: `TASKS.md`'s pre-batch research named `tree-sitter-kotlin`
/// (fwcd's grammar, last published for `tree-sitter` `>= 0.21, < 0.23`) as the
/// parser crate. That crate's Rust binding returns a `tree_sitter::Language`
/// from its *own* pinned `tree-sitter` dependency, which does not type-check
/// against this workspace's `tree-sitter 0.25` (`Parser::set_language` expects
/// the 0.25 crate's `Language`/`Into<Language>`, not one from a different
/// semver-incompatible `tree-sitter` version) -- it does not compile in this
/// workspace. `tree-sitter-kotlin-ng` (the actively maintained grammar used by
/// `tree-sitter-grammars`) instead exposes a `LANGUAGE: LanguageFn` constant
/// via the version-agnostic `tree-sitter-language` bridge crate, the exact
/// pattern `tree_sitter_java::LANGUAGE.into()` already uses here -- so this
/// adapter uses `tree-sitter-kotlin-ng` instead, a deliberate substitution for
/// the originally-named crate, not a drift from the plan.
///
/// Like Java/C#, Kotlin has explicit `class`/`interface`/`: SuperType`
/// syntax, so `resolve_structural` below reads relations directly off the
/// grammar rather than inferring them the way Go's structural
/// interface-satisfaction does. Kotlin's grammar makes no syntactic
/// distinction between a class's `extends`-like relation and an interface's
/// `implements`-like relation -- both are just a `: SuperType` entry in the
/// same `delegation_specifiers` list -- so this adapter tells them apart the
/// way the language itself does: a list entry that's a constructor call
/// (`: Base()`) is a superclass, a bare type name (`: Interface`) is an
/// implemented interface, mirroring how a Kotlin compiler itself
/// disambiguates the two.
pub struct KotlinAdapter;

impl LanguageAdapter for KotlinAdapter {
    type Ast = KotlinAst;

    fn parse(&self, source: &str) -> Result<Self::Ast> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
            .map_err(|err| Error::Message(format!("failed to load Kotlin grammar: {err}")))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::Message("failed to parse Kotlin source".to_string()))?;
        Ok(KotlinAst {
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
            &sites.non_public_types,
            module_symbol,
            symbol_table,
        ));

        (symbols, edges)
    }
}

/// Sites `extract_items`/`extract_member` collect that `resolve_file` needs
/// the raw tree for, or that describe a structural relationship read
/// directly off explicit Kotlin syntax (no later structural-inference pass
/// needed, mirroring Java's `ResolutionSites`).
#[derive(Default)]
struct ResolutionSites<'a> {
    /// A function/method/constructor's own entity paired with its body node
    /// -- a `block` for a block-bodied function, or the bare expression node
    /// itself for an expression-bodied one (`fun f() = expr`); `resolve_body`
    /// handles both shapes.
    bodies: Vec<(SyntaxEntity, Node<'a>)>,
    /// Every `import` node.
    imports: Vec<Node<'a>>,
    /// A class/object's own entity paired with one superclass name, read
    /// from a `delegation_specifiers` entry that's a constructor call.
    superclasses: Vec<(SyntaxEntity, String)>,
    /// A class/object's own entity paired with one implemented-interface
    /// name, read from a `delegation_specifiers` entry that's a bare type
    /// (or an explicit-delegation `by` clause -- the delegation mechanics
    /// themselves aren't modeled, just the "implements" relationship).
    implemented_interfaces: Vec<(SyntaxEntity, String)>,
    /// An interface's own entity paired with one superinterface name --
    /// Kotlin's grammar doesn't distinguish an interface's `: SuperType`
    /// list from a class's, so this is populated by the same
    /// `delegation_specifiers` walk, keyed off whether the owning entity is
    /// `SyntaxKind::Interface`.
    extended_interfaces: Vec<(SyntaxEntity, String)>,
    /// Every class/interface/object/enum entity declared with an explicit
    /// `private`/`internal`/`protected` visibility modifier -- Kotlin
    /// defaults to public unless marked otherwise (the opposite convention
    /// from Java's explicit-`public` requirement), so `resolve_exports`
    /// treats everything NOT in this list as exported, rather than
    /// requiring membership the way Java's `public_types` does.
    non_public_types: Vec<crate::model::EntityId>,
}

fn text<'a>(node: Node<'_>, src: &'a str) -> &'a str {
    node.utf8_text(src.as_bytes()).unwrap_or("")
}

fn child_scope(scope: &str, name: &str) -> String {
    format!("{scope}::{name}")
}

/// Record `parent CONTAINS child` and `parent DECLARES child`, then push
/// `child` into `entities` -- classes/interfaces/objects/functions.
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

/// Record `parent CONTAINS child` only -- properties, parameters, local
/// variables, statements.
fn contain(
    parent: &SyntaxEntity,
    child: SyntaxEntity,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    edges.push(Edge::new(parent.id.clone(), child.id.clone(), EdgeKind::Contains));
    entities.push(child);
}

/// Whether `node` (a `class_declaration`) is an `interface` rather than a
/// `class` -- both keywords surface as a direct (unnamed-position but
/// still-kinded) child token of the node itself, since Kotlin's grammar
/// spells the choice as `choice('class', seq(optional('fun'), 'interface'))`
/// rather than a fielded distinction.
fn is_interface_decl(node: Node) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|c| c.kind() == "interface");
    found
}

/// The visibility modifier text (`"private"`/`"internal"`/`"protected"`) an
/// entity's `modifiers` child explicitly carries, if any -- `None` means no
/// explicit modifier was written, which under Kotlin's own default-public
/// convention means public (see `resolve_exports`).
fn explicit_visibility(node: Node, src: &str) -> Option<String> {
    let mut cursor = node.walk();
    let modifiers = node.children(&mut cursor).find(|c| c.kind() == "modifiers")?;
    let mut mod_cursor = modifiers.walk();
    let found = modifiers
        .children(&mut mod_cursor)
        .find(|c| c.kind() == "visibility_modifier")
        .map(|c| text(c, src).to_string());
    found
}

/// A `user_type`/`nullable_type`/... type expression's base declared name:
/// `user_type`'s last dotted segment's identifier, ignoring type arguments
/// (`List<String>` -> `List`) and nullability (`Shape?` -> `Shape`) --
/// mirrors Java's `type_base_name`.
fn type_base_name(node: Node, src: &str) -> String {
    match node.kind() {
        "user_type" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .filter(|c| c.kind() != "type_arguments")
                .last()
                .map(|last| type_base_name(last, src))
                .unwrap_or_else(|| text(node, src).to_string())
        }
        "nullable_type" | "non_nullable_type" | "parenthesized_type" => node
            .named_child(0)
            .map(|c| type_base_name(c, src))
            .unwrap_or_else(|| text(node, src).to_string()),
        "identifier" => text(node, src).to_string(),
        _ => text(node, src).to_string(),
    }
}

/// A `class_declaration`/`object_declaration`'s `delegation_specifiers`
/// list, if present -- `None` when the node has no `: SuperType` clause.
fn delegation_specifiers(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == "delegation_specifiers");
    found
}

/// One `delegation_specifier` entry's base type name, and whether it's a
/// superclass constructor call (`Base()`) rather than a bare
/// interface/explicit-delegation type.
fn delegation_entry(entry: Node, src: &str) -> Option<(String, bool)> {
    let mut cursor = entry.walk();
    for child in entry.named_children(&mut cursor) {
        match child.kind() {
            "constructor_invocation" => {
                let type_node = child.named_child(0)?;
                return Some((type_base_name(type_node, src), true));
            }
            "explicit_delegation" => {
                let type_node = child.named_child(0)?;
                return Some((type_base_name(type_node, src), false));
            }
            "user_type" | "nullable_type" | "non_nullable_type" | "parenthesized_type" => {
                return Some((type_base_name(child, src), false));
            }
            _ => {}
        }
    }
    None
}

/// Walk `class_declaration`/`object_declaration`/`companion_object`'s
/// `delegation_specifiers`, recording each entry into `sites.superclasses`
/// (constructor-call entries) or `sites.implemented_interfaces` /
/// `sites.extended_interfaces` (bare-type entries), depending on whether
/// `owner` is itself an `Interface`.
fn record_delegations<'a>(node: Node<'a>, src: &str, owner: &SyntaxEntity, sites: &mut ResolutionSites<'a>) {
    let Some(list) = delegation_specifiers(node) else {
        return;
    };
    let mut cursor = list.walk();
    for entry in list.named_children(&mut cursor) {
        let Some((name, is_call)) = delegation_entry(entry, src) else {
            continue;
        };
        if owner.kind == SyntaxKind::Interface {
            sites.extended_interfaces.push((owner.clone(), name));
        } else if is_call {
            sites.superclasses.push((owner.clone(), name));
        } else {
            sites.implemented_interfaces.push((owner.clone(), name));
        }
    }
}

/// Walk a `source_file`'s direct children, dispatching top-level
/// declarations. `import`/`package_header` are handled here directly;
/// everything else that can also nest inside a class body is delegated to
/// `extract_member` so the two don't drift out of sync.
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
            "import" => sites.imports.push(child),
            // `package_header` carries no Phase 3 syntax counterpart of its
            // own, same posture as Java's `package_declaration` -- one
            // `Module` entity per file rather than per package, a
            // documented gap, not guessed at.
            "package_header" => {}
            _ => extract_member(child, src, parent, scope, entities, edges, sites),
        }
    }
}

/// A class/object/companion body's (or the top level's) direct member
/// declarations. Shared by `extract_items` (top level) and `extract_class`'s
/// own body loop so top-level and nested declarations extract identically.
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
        "class_declaration" => extract_class(node, src, owner, owner_scope, entities, edges, sites),
        "object_declaration" => {
            extract_object(node, src, owner, owner_scope, entities, edges, sites)
        }
        "companion_object" => {
            extract_companion(node, src, owner, owner_scope, entities, edges, sites)
        }
        "function_declaration" => {
            let is_method = owner.kind != SyntaxKind::Module;
            extract_function(node, src, owner, owner_scope, is_method, entities, edges, sites);
        }
        "property_declaration" => {
            extract_property(node, src, owner, owner_scope, entities, edges);
        }
        "secondary_constructor" => {
            extract_secondary_constructor(node, src, owner, owner_scope, entities, edges, sites);
        }
        // A `type_alias` introduces no new declaration of its own kind
        // under the canonical model, and an `init { ... }` anonymous
        // initializer block has no declared name to attach entities to --
        // both documented gaps, matching Java's `static_initializer` gap.
        "type_alias" | "anonymous_initializer" | ";" => {}
        _ => {}
    }
}

/// A `class_declaration`: its own `Class`/`Interface`/`Enum` entity
/// (Kotlin's grammar uses one node kind for all three -- see
/// `is_interface_decl` and the `enum_class_body` check below), its primary
/// constructor (modeled as a `Method` named after the class, same posture as
/// Java's constructor-as-Method), `: SuperType` relations (recorded in
/// `sites`, read directly rather than inferred), and its body's members.
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

    let body = {
        let mut cursor = node.walk();
        let found = node
            .children(&mut cursor)
            .find(|c| c.kind() == "class_body" || c.kind() == "enum_class_body");
        found
    };
    let is_enum = body.is_some_and(|b| b.kind() == "enum_class_body");
    let kind = if is_enum {
        SyntaxKind::Enum
    } else if is_interface_decl(node) {
        SyntaxKind::Interface
    } else {
        SyntaxKind::Class
    };

    let entity = SyntaxEntity::new(kind, scope, name);
    let class_scope = child_scope(scope, name);
    if let Some(visibility) = explicit_visibility(node, src) {
        if visibility != "public" {
            sites.non_public_types.push(entity.id.clone());
        }
    }
    declare(parent, entity.clone(), entities, edges);

    record_delegations(node, src, &entity, sites);

    let primary_ctor = {
        let mut cursor = node.walk();
        let found = node.children(&mut cursor).find(|c| c.kind() == "primary_constructor");
        found
    };
    if let Some(ctor) = primary_ctor {
        extract_primary_constructor(ctor, src, &entity, &class_scope, entities, edges, sites);
    }

    let Some(body) = body else { return };
    match body.kind() {
        "class_body" => {
            let mut cursor = body.walk();
            for member in body.children(&mut cursor) {
                extract_member(member, src, &entity, &class_scope, entities, edges, sites);
            }
        }
        "enum_class_body" => {
            let mut cursor = body.walk();
            for child in body.children(&mut cursor) {
                if child.kind() == "enum_entry" {
                    let mut ec = child.walk();
                    let const_name_node = {
                        let found = child.named_children(&mut ec).find(|c| c.kind() == "identifier");
                        found
                    };
                    if let Some(const_name) = const_name_node {
                        let const_entity =
                            SyntaxEntity::new(SyntaxKind::Variable, &class_scope, text(const_name, src));
                        contain(&entity, const_entity, entities, edges);
                    }
                } else {
                    extract_member(child, src, &entity, &class_scope, entities, edges, sites);
                }
            }
        }
        _ => {}
    }
}

/// An `object_declaration` (a Kotlin singleton): modeled as a plain `Class`
/// entity -- the canonical model has no dedicated singleton/object kind, and
/// an `object`'s only structural difference from a regular class (exactly
/// one implicit instance, no constructor parameters) has no bearing on
/// `Class`'s own shape -- a documented modeling choice, not an oversight.
fn extract_object<'a>(
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
    let obj_scope = child_scope(scope, name);
    if let Some(visibility) = explicit_visibility(node, src) {
        if visibility != "public" {
            sites.non_public_types.push(entity.id.clone());
        }
    }
    declare(parent, entity.clone(), entities, edges);
    record_delegations(node, src, &entity, sites);

    let class_body = {
        let mut cursor = node.walk();
        let found = node.children(&mut cursor).find(|c| c.kind() == "class_body");
        found
    };
    if let Some(body) = class_body {
        let mut body_cursor = body.walk();
        for member in body.children(&mut body_cursor) {
            extract_member(member, src, &entity, &obj_scope, entities, edges, sites);
        }
    }
}

/// A `companion_object`: modeled the same way as a regular `object`, nested
/// under its owning class. Kotlin allows an unnamed companion (defaulting to
/// the name `Companion`) -- handled explicitly here, since
/// `child_by_field_name("name")` returns `None` for that case rather than
/// something to fall through on silently.
fn extract_companion<'a>(
    node: Node<'a>,
    src: &'a str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let name = node
        .child_by_field_name("name")
        .map(|n| text(n, src).to_string())
        .unwrap_or_else(|| "Companion".to_string());
    let entity = SyntaxEntity::new(SyntaxKind::Class, scope, &name);
    let companion_scope = child_scope(scope, &name);
    declare(parent, entity.clone(), entities, edges);
    record_delegations(node, src, &entity, sites);

    let class_body = {
        let mut cursor = node.walk();
        let found = node.children(&mut cursor).find(|c| c.kind() == "class_body");
        found
    };
    if let Some(body) = class_body {
        let mut body_cursor = body.walk();
        for member in body.children(&mut body_cursor) {
            extract_member(member, src, &entity, &companion_scope, entities, edges, sites);
        }
    }
}

/// A primary constructor's `class_parameters`: each becomes a `Parameter`
/// under a synthesized `Method` entity named after the owning class (same
/// posture as Java's constructor-as-Method). A parameter explicitly marked
/// `val`/`var` is *also* a class property under Kotlin's constructor-property
/// shorthand -- recorded as a second, separate `Variable` entity directly on
/// the class, alongside (not instead of) the `Parameter` entity, since both
/// facts (it's a constructor parameter, and it's a property) are true at
/// once.
fn extract_primary_constructor<'a>(
    ctor: Node<'a>,
    src: &'a str,
    class_entity: &SyntaxEntity,
    class_scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let ctor_entity = SyntaxEntity::new(SyntaxKind::Method, class_scope, class_entity.name.clone());
    let ctor_scope = child_scope(class_scope, &class_entity.name);
    declare(class_entity, ctor_entity.clone(), entities, edges);

    let mut cursor = ctor.walk();
    let Some(params) = ctor.children(&mut cursor).find(|c| c.kind() == "class_parameters") else {
        sites.bodies.push((ctor_entity, ctor));
        return;
    };
    let mut p_cursor = params.walk();
    for param in params.children(&mut p_cursor) {
        if param.kind() != "class_parameter" {
            continue;
        }
        let mut ident_cursor = param.walk();
        let Some(name_node) = param.named_children(&mut ident_cursor).find(|c| c.kind() == "identifier")
        else {
            continue;
        };
        let pname = text(name_node, src);
        let param_entity = SyntaxEntity::new(SyntaxKind::Parameter, &ctor_scope, pname);
        contain(&ctor_entity, param_entity, entities, edges);

        let mut is_property_cursor = param.walk();
        let is_property = param
            .children(&mut is_property_cursor)
            .any(|c| c.kind() == "val" || c.kind() == "var");
        if is_property {
            let property_entity = SyntaxEntity::new(SyntaxKind::Variable, class_scope, pname);
            contain(class_entity, property_entity, entities, edges);
        }
    }
    // No body node to resolve for a primary constructor (parameter default
    // values aside, out of scope -- matching Java's own field-initializer
    // gap).
    let _ = &sites;
}

/// A `secondary_constructor`: modeled as a `Method` named after the owning
/// class, same as the primary constructor and Java's own constructors.
fn extract_secondary_constructor<'a>(
    node: Node<'a>,
    src: &'a str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let entity = SyntaxEntity::new(SyntaxKind::Method, owner_scope, owner.name.clone());
    let method_scope = child_scope(owner_scope, &owner.name);
    declare(owner, entity.clone(), entities, edges);

    let mut cursor = node.walk();
    if let Some(params) = node.children(&mut cursor).find(|c| c.kind() == "function_value_parameters") {
        extract_parameters(params, src, &entity, &method_scope, entities, edges);
    }
    let block_body = {
        let mut body_cursor = node.walk();
        let found = node.children(&mut body_cursor).find(|c| c.kind() == "block");
        found
    };
    if let Some(body) = block_body {
        extract_body(body, src, &entity, &method_scope, entities, edges);
        sites.bodies.push((entity, body));
    }
}

/// A `function_declaration`: a top-level `Function` or a member `Method`
/// (`is_method` distinguishes the two, since Kotlin uses the same node kind
/// for both).
#[allow(clippy::too_many_arguments)]
fn extract_function<'a>(
    node: Node<'a>,
    src: &'a str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    is_method: bool,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let name = text(name_node, src);
    let kind = if is_method { SyntaxKind::Method } else { SyntaxKind::Function };
    let entity = SyntaxEntity::new(kind, owner_scope, name);
    let fn_scope = child_scope(owner_scope, name);
    declare(owner, entity.clone(), entities, edges);

    let mut cursor = node.walk();
    if let Some(params) = node.children(&mut cursor).find(|c| c.kind() == "function_value_parameters") {
        extract_parameters(params, src, &entity, &fn_scope, entities, edges);
    }

    // `function_body` wraps either a `block` (`{ ... }`) or a bare
    // expression (`= expr`) -- an abstract/interface method with no body at
    // all (`;` or nothing) simply has no `function_body` child, nothing
    // further to extract/resolve.
    let mut body_cursor = node.walk();
    let Some(function_body) = node.children(&mut body_cursor).find(|c| c.kind() == "function_body") else {
        return;
    };
    let mut inner_cursor = function_body.walk();
    let Some(body_node) = function_body.named_children(&mut inner_cursor).next() else {
        return;
    };
    if body_node.kind() == "block" {
        extract_body(body_node, src, &entity, &fn_scope, entities, edges);
    } else {
        // Expression-bodied function -- one generic `Statement` entity for
        // the expression, so an expression-bodied and block-bodied function
        // aren't silently treated differently at the entity level.
        let stmt_entity = SyntaxEntity::new(SyntaxKind::Statement, &fn_scope, "stmt_0");
        contain(&entity, stmt_entity, entities, edges);
    }
    sites.bodies.push((entity, body_node));
}

/// A `property_declaration` (`val`/`var`, at top level, in a class body, or
/// -- via `extract_body` -- as a local declaration): becomes one `Variable`
/// entity per declared name. Destructuring (`val (a, b) = pair`) is a
/// documented gap -- `multi_variable_declaration` isn't walked, matching the
/// posture every other adapter takes toward patterns it doesn't extract
/// names from individually.
fn extract_property(
    node: Node,
    src: &str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = node.walk();
    let Some(var_decl) = node.named_children(&mut cursor).find(|c| c.kind() == "variable_declaration")
    else {
        return;
    };
    let mut ident_cursor = var_decl.walk();
    let Some(name_node) = var_decl.named_children(&mut ident_cursor).find(|c| c.kind() == "identifier")
    else {
        return;
    };
    let entity = SyntaxEntity::new(SyntaxKind::Variable, owner_scope, text(name_node, src));
    contain(owner, entity, entities, edges);
}

/// A `function_value_parameters`' individual `parameter` nodes become
/// `Parameter` entities.
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
        if param.kind() != "parameter" {
            continue;
        }
        if let Some(name_node) = param.named_child(0) {
            let entity = SyntaxEntity::new(SyntaxKind::Parameter, scope, text(name_node, src));
            contain(parent, entity, entities, edges);
        }
    }
}

/// A `block`'s direct statement children: a `property_declaration` becomes
/// `Variable` entities; everything else becomes a generic `Statement`
/// entity. A local class/object/function declaration inside a body isn't
/// extracted -- a documented gap, same as Java not walking into nested item
/// definitions.
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
            "property_declaration" => {
                extract_property(stmt, src, parent, &format!("{scope}#{index}"), entities, edges);
            }
            "class_declaration" | "object_declaration" | "function_declaration" => {}
            _ => {
                let entity =
                    SyntaxEntity::new(SyntaxKind::Statement, scope, format!("stmt_{index}"));
                contain(parent, entity, entities, edges);
            }
        }
    }
}

/// `REFERENCES`/`CALLS`/`INSTANTIATES` edges from one function/method/
/// constructor body. `body` is either a `block` (walked statement by
/// statement) or a bare expression (an expression-bodied function) --
/// mirrors Java's `resolve_body`, adapted to Kotlin's `identifier`/
/// `call_expression`/`navigation_expression` node kinds. A bare identifier
/// resolves via `scope_tree`; a bare-name call (`helper()`) resolves the
/// same way; a receiver-qualified call (`obj.method()`, via
/// `navigation_expression`) resolves by best-effort name match against any
/// `Method` symbol in the file (real receiver-type dispatch needs
/// cross-file class knowledge, out of scope here, matching Java/Go/Rust's
/// own posture); a call whose callee resolves in scope to a `Type` symbol
/// (Kotlin has no `new` keyword -- `Circle()` IS the instantiation syntax)
/// is an `INSTANTIATES` site. Anything unresolved gets an explicit
/// placeholder symbol, never silently dropped.
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

    if body.kind() == "block" {
        let mut cursor = body.walk();
        for stmt in body.named_children(&mut cursor) {
            walk_stmt(stmt, &mut |n| names.push(n), &mut |n| calls.push(n));
        }
    } else {
        walk_expr(body, &mut |n| names.push(n), &mut |n| calls.push(n));
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
        let Some(callee) = call_node.named_child(0) else {
            continue;
        };
        let (ident, is_qualified) = match callee.kind() {
            "navigation_expression" => {
                // `navigation_expression`'s named children are `[object,
                // member]`, both potentially `identifier` nodes (`shape.area`
                // -> `[identifier "shape", identifier "area"]`) -- the member
                // name is the *last* one, not the first.
                let mut cursor = callee.walk();
                let Some(member) = callee
                    .named_children(&mut cursor)
                    .filter(|c| c.kind() == "identifier")
                    .last()
                else {
                    continue;
                };
                (text(member, src), true)
            }
            "identifier" => (text(callee, src), false),
            _ => continue,
        };

        let resolved_in_scope = scope_tree
            .lookup(scope, ident)
            .and_then(|id| symbol_table.find_by_syntax_id(id))
            .cloned();

        if let Some(symbol) = &resolved_in_scope {
            if symbol.kind == SymbolKind::Type {
                // `Circle()` -- Kotlin's constructor-call-as-instantiation
                // syntax, no `new` keyword.
                edges.push(Edge::new(
                    member_symbol.id.clone(),
                    symbol.id.clone(),
                    EdgeKind::Instantiates,
                ));
                continue;
            }
        }

        let target = if is_qualified {
            resolved_in_scope.unwrap_or_else(|| {
                symbol_table
                    .symbols()
                    .find(|s| s.kind == SymbolKind::Method && s.name == ident)
                    .cloned()
                    .unwrap_or_else(|| {
                        let placeholder = unresolved_symbol("unknown-method", ident);
                        symbols.push(placeholder.clone());
                        placeholder
                    })
            })
        } else {
            resolved_in_scope.unwrap_or_else(|| {
                let placeholder = unresolved_symbol("unknown-local", ident);
                symbols.push(placeholder.clone());
                placeholder
            })
        };
        edges.push(Edge::new(member_symbol.id.clone(), target.id.clone(), EdgeKind::Calls));
    }

    (symbols, edges)
}

fn walk_expr<'a>(node: Node<'a>, on_name: &mut impl FnMut(Node<'a>), on_call: &mut impl FnMut(Node<'a>)) {
    match node.kind() {
        "identifier" => on_name(node),
        "call_expression" => {
            on_call(node);
            if let Some(callee) = node.named_child(0) {
                match callee.kind() {
                    "identifier" => on_name(callee),
                    "navigation_expression" => walk_expr(callee, on_name, on_call),
                    _ => {}
                }
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "value_arguments" {
                    let mut arg_cursor = child.walk();
                    for arg in child.named_children(&mut arg_cursor) {
                        walk_expr(arg, on_name, on_call);
                    }
                }
            }
        }
        "navigation_expression" => {
            if let Some(object) = node.named_child(0) {
                walk_expr(object, on_name, on_call);
            }
        }
        // A `value_argument` wraps a call argument's actual expression (plus
        // an optional `name =` prefix for a named argument) -- walk its last
        // named child, the expression itself, so a bare-identifier argument
        // (`println(value)`) is still recorded as a name usage the way a
        // top-level bare identifier is.
        "value_argument" => {
            let mut cursor = node.walk();
            if let Some(expr) = node.named_children(&mut cursor).last() {
                walk_expr(expr, on_name, on_call);
            }
        }
        "assignment" => {
            if let Some(left) = node.child_by_field_name("left") {
                walk_expr(left, on_name, on_call);
            }
            if let Some(right) = node.child_by_field_name("right") {
                walk_expr(right, on_name, on_call);
            }
        }
        "binary_expression" | "in_expression" | "is_expression" | "range_expression" => {
            if let Some(left) = node.child_by_field_name("left") {
                walk_expr(left, on_name, on_call);
            }
            if let Some(right) = node.child_by_field_name("right") {
                walk_expr(right, on_name, on_call);
            } else {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    walk_expr(child, on_name, on_call);
                }
            }
        }
        "unary_expression" => {
            if let Some(argument) = node.child_by_field_name("argument") {
                walk_expr(argument, on_name, on_call);
            }
        }
        "as_expression" => {
            if let Some(left) = node.child_by_field_name("left") {
                walk_expr(left, on_name, on_call);
            }
        }
        "return_expression" | "throw_expression" | "spread_expression" => {
            if let Some(inner) = node.named_child(0) {
                walk_expr(inner, on_name, on_call);
            }
        }
        "if_expression" => {
            if let Some(condition) = node.child_by_field_name("condition") {
                walk_expr(condition, on_name, on_call);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "block" {
                    walk_stmt(child, on_name, on_call);
                }
            }
        }
        "index_expression" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                walk_expr(child, on_name, on_call);
            }
        }
        // `when` expressions/statements are a documented gap here, matching
        // the batch guidance: structural extraction only, no exhaustiveness
        // checking and no per-branch reference/call walking.
        _ => {}
    }
}

fn walk_stmt<'a>(node: Node<'a>, on_name: &mut impl FnMut(Node<'a>), on_call: &mut impl FnMut(Node<'a>)) {
    match node.kind() {
        "property_declaration" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() != "variable_declaration" && child.kind() != "type" {
                    walk_expr(child, on_name, on_call);
                }
            }
        }
        "for_statement" => {
            if let Some(iterable) = node.named_child(node.named_child_count().saturating_sub(1)) {
                walk_expr(iterable, on_name, on_call);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "block" {
                    walk_stmt(child, on_name, on_call);
                }
            }
        }
        "while_statement" | "do_while_statement" => {
            if let Some(condition) = node.child_by_field_name("condition") {
                walk_expr(condition, on_name, on_call);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "block" {
                    walk_stmt(child, on_name, on_call);
                }
            }
        }
        "block" => {
            let mut cursor = node.walk();
            for stmt in node.named_children(&mut cursor) {
                walk_stmt(stmt, on_name, on_call);
            }
        }
        // try/when/local classes and other less common control flow are a
        // documented gap here, matching Java's own limited `walk_stmt`
        // coverage.
        _ => walk_expr(node, on_name, on_call),
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

/// Resolve `sites`' explicit `: SuperType` relations into `EXTENDS`/
/// `IMPLEMENTS` edges, searched repository-wide (`all_entities`) so a
/// superclass/superinterface declared in a different file still resolves --
/// mirrors Java's `resolve_structural`. A name that resolves to nothing in
/// the repository (an external/stdlib type, e.g. `: RuntimeException`) gets
/// an explicit placeholder symbol, never silently dropped.
fn resolve_structural(
    sites: &ResolutionSites,
    all_entities: &[SyntaxEntity],
    symbol_table: &SymbolTable,
    symbols: &mut Vec<SymbolEntity>,
) -> Vec<Edge> {
    let mut edges = Vec::new();

    for (entity, superclass_name) in &sites.superclasses {
        let Some(owner_symbol) = symbol_table.find_by_syntax_id(&entity.id) else {
            continue;
        };
        let target = find_type_symbol(
            all_entities,
            symbol_table,
            &[SyntaxKind::Class, SyntaxKind::Enum],
            superclass_name,
        )
        .unwrap_or_else(|| {
            let placeholder = unresolved_symbol("external-class", superclass_name);
            symbols.push(placeholder.clone());
            placeholder
        });
        edges.push(Edge::new(owner_symbol.id.clone(), target.id, EdgeKind::Extends));
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

    for (entity, super_name) in &sites.extended_interfaces {
        let Some(owner_symbol) = symbol_table.find_by_syntax_id(&entity.id) else {
            continue;
        };
        let target = find_type_symbol(all_entities, symbol_table, &[SyntaxKind::Interface], super_name)
            .unwrap_or_else(|| {
                let placeholder = unresolved_symbol("external-interface", super_name);
                symbols.push(placeholder.clone());
                placeholder
            });
        edges.push(Edge::new(owner_symbol.id.clone(), target.id, EdgeKind::Extends));
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

/// Resolve a file's `import` nodes into `IMPORTS` edges from
/// `module_symbol`. A Kotlin import path (`kotlin.collections.List`,
/// `com.example.shapes.Shape`) doesn't tell us, without resolving it against
/// this repository's actual package layout, whether it names a stdlib/
/// third-party type or another file in this same repository -- out of scope
/// for this sub-phase, matching Java/Go's own posture, so every import
/// resolves to an explicit `external-package` placeholder. A wildcard import
/// (`import a.b.*`) and an aliased import (`import a.B as C`) both resolve
/// the same way, keyed by their dotted path.
fn resolve_imports(
    imports: &[Node],
    src: &str,
    module_symbol: &SymbolEntity,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for import_node in imports {
        let mut cursor = import_node.walk();
        let Some(qualified) = import_node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "qualified_identifier")
        else {
            continue;
        };
        let joined = text(qualified, src).to_string();
        let placeholder = unresolved_symbol("external-package", &joined);
        symbols.push(placeholder.clone());
        edges.push(Edge::new(module_symbol.id.clone(), placeholder.id, EdgeKind::Imports));
    }

    (symbols, edges)
}

/// Resolve a file's top-level declarations into `EXPORTS` edges from
/// `module_symbol`: Kotlin's actual public-API convention -- public unless
/// explicitly marked `private`/`internal`/`protected` (recorded in
/// `non_public_types` at extraction time, since `SyntaxEntity` carries no
/// visibility field of its own) -- the opposite default from Java's
/// explicit-`public`-required convention.
fn resolve_exports(
    file_entities: &[SyntaxEntity],
    file_edges: &[Edge],
    non_public_types: &[crate::model::EntityId],
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
        if non_public_types.contains(entity_id) {
            continue;
        }
        if let Some(symbol) = symbol_table.find_by_syntax_id(&entity.id) {
            edges.push(Edge::new(module_symbol.id.clone(), symbol.id.clone(), EdgeKind::Exports));
        }
    }
    edges
}

/// Map a Kotlin type expression to a `TypeRef`. Not yet wired into
/// `extract`/`resolve_file` -- `ACCEPTS`/`RETURNS` type binding isn't called
/// from the pipeline, mirroring every other adapter's own
/// `extract_signature_types` gap (tested directly, deliberately not yet
/// wired in either).
///
/// Canonical-model gap, found while implementing this: Kotlin's `T?`
/// nullable types have no representation in `TypeRef` -- neither `Union`
/// (a nullable type isn't "one of several alternative types" the way
/// TypeScript's `A | B` is; it's a single type plus an orthogonal
/// nullability flag) nor `Reference` (that models indirection/mutability,
/// not nullability) fits. Per this batch's guidance, this is documented as
/// a gap rather than forced into an existing shape: `extract_type` below
/// unwraps a `nullable_type` to its inner type and drops the `?`,
/// consistent with Java/Go/Rust's own precedent of documenting a gap on
/// `extract_type` rather than inventing a fake structure for it.
#[allow(dead_code)]
fn extract_type(node: Node, src: &str) -> TypeRef {
    match node.kind() {
        "user_type" => {
            let mut cursor = node.walk();
            let segments: Vec<Node> = node
                .named_children(&mut cursor)
                .filter(|c| c.kind() != "type_arguments")
                .collect();
            let Some(last) = segments.last() else {
                return TypeRef::Unresolved(text(node, src).to_string());
            };
            let name = type_base_name(*last, src);
            let type_args: Vec<TypeRef> = {
                let mut arg_cursor = node.walk();
                let found: Vec<TypeRef> = node
                    .named_children(&mut arg_cursor)
                    .find(|c| c.kind() == "type_arguments")
                    .map(|args| {
                        let mut ac = args.walk();
                        args.named_children(&mut ac)
                            .filter_map(|proj| proj.named_child(0))
                            .map(|t| extract_type(t, src))
                            .collect()
                    })
                    .unwrap_or_default();
                found
            };
            if PRIMITIVE_TYPES.contains(&name.as_str()) && type_args.is_empty() {
                TypeRef::Primitive(name)
            } else {
                TypeRef::Named { name, type_args }
            }
        }
        "nullable_type" => node
            .named_child(0)
            .map(|inner| extract_type(inner, src))
            .unwrap_or_else(|| TypeRef::Unresolved(text(node, src).to_string())),
        "parenthesized_type" => node
            .named_child(0)
            .map(|inner| extract_type(inner, src))
            .unwrap_or_else(|| TypeRef::Unresolved(text(node, src).to_string())),
        _ => TypeRef::Unresolved(text(node, src).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
        let adapter = KotlinAdapter;
        let ast = adapter.parse(source).expect("valid kotlin parses");
        adapter.extract(&ast, "Module.kt")
    }

    fn parse_source(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_kotlin_ng::LANGUAGE.into()).unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn parses_valid_kotlin_source() {
        let adapter = KotlinAdapter;
        assert!(adapter.parse("fun main() {}\n").is_ok());
    }

    #[test]
    fn parsing_invalid_syntax_is_a_clear_error() {
        // tree-sitter is error-tolerant and always returns *some* tree, so
        // "invalid" Kotlin doesn't surface as an `Err` here -- matching
        // every other tree-sitter-backed adapter's documented posture.
        let adapter = KotlinAdapter;
        assert!(adapter.parse("class {{{ not kotlin").is_ok());
    }

    #[test]
    fn extract_always_returns_the_file_module_first() {
        let (entities, _) = extract("");
        assert_eq!(entities[0].kind, SyntaxKind::Module);
        assert_eq!(entities[0].name, "Module.kt");
    }

    #[test]
    fn extracts_a_top_level_class_with_a_method_and_its_parameters() {
        let (entities, edges) = extract(
            "class Calculator {\n  fun add(a: Int, b: Int): Int {\n    return a + b\n  }\n}\n",
        );

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
    fn extracts_a_primary_constructor_as_a_method_and_its_val_params_as_properties() {
        let (entities, edges) = extract("class Point(val x: Int, val y: Int)\n");
        let class = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Point")
            .unwrap();
        let ctor = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "Point")
            .expect("primary constructor modeled as a Method");
        assert!(edges.contains(&Edge::new(class.id.clone(), ctor.id.clone(), EdgeKind::Declares)));

        let properties: Vec<&SyntaxEntity> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Variable)
            .collect();
        assert_eq!(properties.len(), 2);
        assert!(properties.iter().any(|p| p.name == "x"));
        assert!(properties.iter().any(|p| p.name == "y"));

        let params: Vec<&SyntaxEntity> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Parameter)
            .collect();
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn extracts_an_interface_with_a_method_signature_and_no_body() {
        let (entities, _edges) = extract("interface Shape {\n  fun area(): Double\n}\n");
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
        let (entities, edges) = extract("enum class Color {\n  RED, GREEN, BLUE\n}\n");
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
    fn extracts_a_data_class_as_a_plain_class() {
        let (entities, _) = extract("data class Point(val x: Int, val y: Int)\n");
        entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Point")
            .expect("data class modeled as a plain Class -- no synthesized equals/hashCode/copy");
    }

    #[test]
    fn extracts_a_nested_class_scoped_under_its_enclosing_class() {
        let source = "class Outer {\n  class Inner {\n    fun run() {}\n  }\n}\n";
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
    fn extracts_a_property_declaration_as_a_variable() {
        let (entities, edges) = extract("class Box {\n  private var size: Int = 0\n}\n");
        let class = entities.iter().find(|e| e.kind == SyntaxKind::Class).unwrap();
        let field = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Variable && e.name == "size")
            .expect("property entity");
        assert!(edges.contains(&Edge::new(class.id.clone(), field.id.clone(), EdgeKind::Contains)));
        assert!(!edges.contains(&Edge::new(class.id.clone(), field.id.clone(), EdgeKind::Declares)));
    }

    #[test]
    fn extracts_a_companion_object_with_a_default_name() {
        let (entities, edges) =
            extract("class Factory {\n  companion object {\n    fun create() {}\n  }\n}\n");
        let class = entities.iter().find(|e| e.kind == SyntaxKind::Class && e.name == "Factory").unwrap();
        let companion = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Companion")
            .expect("companion object modeled as a nested Class named Companion");
        assert!(edges.contains(&Edge::new(class.id.clone(), companion.id.clone(), EdgeKind::Declares)));
        entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "create")
            .expect("companion's own member still extracted");
    }

    #[test]
    fn a_non_declaration_statement_becomes_a_generic_statement_entity() {
        let (entities, _) = extract("class Run {\n  fun run() {\n    println(1)\n  }\n}\n");
        assert!(entities.iter().any(|e| e.kind == SyntaxKind::Statement));
    }

    #[test]
    fn an_expression_bodied_function_still_gets_a_statement_entity() {
        let (entities, _) = extract("fun square(x: Int) = x * x\n");
        let function = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "square")
            .unwrap();
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Statement && e.name.starts_with("stmt")));
        let _ = function;
    }

    #[test]
    fn same_named_method_on_different_classes_gets_distinct_ids() {
        let source = "class A {\n  fun run() {}\n}\nclass B {\n  fun run() {}\n}\n";
        let (entities, _) = extract(source);
        let methods: Vec<&SyntaxEntity> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Method && e.name == "run")
            .collect();
        assert_eq!(methods.len(), 2);
        assert_ne!(methods[0].id, methods[1].id);
    }

    // -- type extraction (not yet wired into extract/resolve_file) --

    fn function_named<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        fn find<'a>(node: Node<'a>, src: &'a str, name: &str) -> Option<Node<'a>> {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "function_declaration"
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
        find(tree.root_node(), src, name).unwrap_or_else(|| panic!("no function named {name}"))
    }

    fn return_type_of<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        let func = function_named(tree, src, name);
        let mut cursor = func.walk();
        let found = func
            .children(&mut cursor)
            .find(|c| c.kind() == "user_type" || c.kind() == "nullable_type")
            .expect("return type node");
        found
    }

    #[test]
    fn extracts_a_primitive_return_type() {
        let full = "class C {\n  fun f(): Int { return 0 }\n}\n";
        let tree = parse_source(full);
        let node = return_type_of(&tree, full, "f");
        assert_eq!(extract_type(node, full), TypeRef::Primitive("Int".to_string()));
    }

    #[test]
    fn extracts_a_user_defined_named_type() {
        let full = "class C {\n  fun f(): Circle { return Circle() }\n}\n";
        let tree = parse_source(full);
        let node = return_type_of(&tree, full, "f");
        assert_eq!(
            extract_type(node, full),
            TypeRef::Named { name: "Circle".to_string(), type_args: Vec::new() }
        );
    }

    #[test]
    fn extracts_a_generic_named_type_with_its_type_argument() {
        let full = "class C {\n  fun f(): List<String> { return listOf() }\n}\n";
        let tree = parse_source(full);
        let node = return_type_of(&tree, full, "f");
        assert_eq!(
            extract_type(node, full),
            TypeRef::Named {
                name: "List".to_string(),
                type_args: vec![TypeRef::Named { name: "String".to_string(), type_args: Vec::new() }],
            }
        );
    }

    #[test]
    fn nullable_type_unwraps_to_its_inner_type_dropping_the_question_mark() {
        let full = "class C {\n  fun f(): Circle? { return null }\n}\n";
        let tree = parse_source(full);
        let node = return_type_of(&tree, full, "f");
        assert_eq!(
            extract_type(node, full),
            TypeRef::Named { name: "Circle".to_string(), type_args: Vec::new() }
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
        let adapter = KotlinAdapter;
        let ast = adapter.parse(source).expect("valid kotlin parses");
        let (entities, edges) = adapter.extract(&ast, "Module.kt");
        let module = entities[0].clone();
        let member_entity = entities
            .iter()
            .find(|e| (e.kind == SyntaxKind::Method || e.kind == SyntaxKind::Function) && e.name == member_name)
            .unwrap_or_else(|| panic!("no Method/Function entity named {member_name}"))
            .clone();
        let scope_tree = ScopeTree::build(&module, &entities, &edges);
        let scope = scope_tree.scope_of(&member_entity.id).expect("function opens a scope");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let member_symbol = symbol_table
            .find_by_syntax_id(&member_entity.id)
            .expect("function symbol")
            .clone();
        (
            ast.tree,
            Resolved { entities, scope_tree, scope, symbol_table, member_symbol },
        )
    }

    fn body_of<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        let func = function_named(tree, src, name);
        let mut cursor = func.walk();
        let function_body = func
            .children(&mut cursor)
            .find(|c| c.kind() == "function_body")
            .expect("function body");
        let mut inner = function_body.walk();
        let found = function_body.named_children(&mut inner).next().expect("body node");
        found
    }

    #[test]
    fn resolves_a_reference_to_an_in_scope_local_variable() {
        let source = "class C {\n  fun run() {\n    val value = 1\n    println(value)\n  }\n}\n";
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
        let source = "class C {\n  fun run() {\n    missing.toString()\n  }\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (symbols, _edges) =
            resolve_body(body, source, &r.member_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        assert!(symbols.iter().any(|s| s.name.starts_with("unknown-local")));
    }

    #[test]
    fn resolves_a_free_function_style_call() {
        let source = "class C {\n  fun helper() {}\n  fun run() {\n    helper()\n  }\n}\n";
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
            "class Shape {\n  fun area(): Double { return 0.0 }\n}\nclass Run {\n  fun run(shape: Shape) {\n    shape.area()\n  }\n}\n";
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
    fn resolves_a_constructor_call_instantiation_for_an_in_scope_type() {
        let source = "class Circle\nclass Run {\n  fun run() {\n    val c = Circle()\n  }\n}\n";
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
    fn a_class_implementing_an_interface_gets_an_explicit_implements_edge() {
        let source =
            "interface Shape {\n  fun area(): Double\n}\nclass Circle : Shape {\n  override fun area(): Double { return 0.0 }\n}\n";
        let adapter = KotlinAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "Module.kt");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "Module.kt",
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
    fn a_class_extending_a_class_via_a_constructor_call_gets_an_explicit_extends_edge() {
        let source = "open class Animal\nclass Dog : Animal()\n";
        let adapter = KotlinAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "Module.kt");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "Module.kt",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );
        let mut symbols = Vec::new();
        let extends_edges = resolve_structural(&sites, &entities, &symbol_table, &mut symbols);

        let dog = entities.iter().find(|e| e.kind == SyntaxKind::Class && e.name == "Dog").unwrap();
        let animal = entities.iter().find(|e| e.kind == SyntaxKind::Class && e.name == "Animal").unwrap();
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
        let source = "interface MyList : kotlin.collections.List\n";
        let adapter = KotlinAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "Module.kt");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "Module.kt",
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
        let source = "import kotlin.collections.List\nimport kotlin.math.*\n\nclass C\n";
        let adapter = KotlinAdapter;
        let ast = adapter.parse(source).unwrap();
        let module_symbol = SymbolEntity::new(SymbolKind::Namespace, "Module.kt", "Module.kt");

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &SyntaxEntity::new(SyntaxKind::Module, "Module.kt", "Module.kt"),
            "Module.kt",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );

        let (symbols, edges) = resolve_imports(&sites.imports, &ast.source, &module_symbol);

        assert_eq!(symbols.len(), 2);
        assert!(symbols.iter().any(|s| s.name == "external-package:kotlin.collections.List"));
        assert!(symbols.iter().any(|s| s.name == "external-package:kotlin.math"));
        assert_eq!(edges.iter().filter(|e| e.kind == EdgeKind::Imports).count(), 2);
    }

    #[test]
    fn exports_a_public_top_level_class_but_not_a_private_one() {
        let source = "class Public\nprivate class Hidden\n";
        let adapter = KotlinAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, edges) = adapter.extract(&ast, "Module.kt");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table.find_by_syntax_id(&entities[0].id).unwrap().clone();

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "Module.kt",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );

        let export_edges =
            resolve_exports(&entities, &edges, &sites.non_public_types, &module_symbol, &symbol_table);

        let public = entities.iter().find(|e| e.kind == SyntaxKind::Class && e.name == "Public").unwrap();
        let public_symbol = symbol_table.find_by_syntax_id(&public.id).unwrap();
        assert_eq!(export_edges.len(), 1);
        assert!(export_edges.contains(&Edge::new(
            module_symbol.id.clone(),
            public_symbol.id.clone(),
            EdgeKind::Exports
        )));
    }
}
