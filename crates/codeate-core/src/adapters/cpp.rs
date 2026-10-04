use tree_sitter::{Node, Parser, Tree};

use crate::adapter::LanguageAdapter;
use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};
use crate::resolve::unresolved_symbol;
use crate::symbols::{ScopeTree, SymbolTable};
use crate::types::TypeRef;

/// C++'s built-in primitive types, same posture as C's own `PRIMITIVE_TYPES`
/// list (see `c.rs`) -- `sized_type_specifier` combinations (`unsigned
/// long`, ...) aren't classified here, same documented gap.
const PRIMITIVE_TYPES: &[&str] = &[
    "bool", "char", "int", "float", "double", "void", "size_t", "ssize_t", "wchar_t", "char8_t",
    "char16_t", "char32_t",
];

/// The parsed form a `CppAdapter` produces: mirrors `CAst`'s tree+source
/// pairing.
pub struct CppAst {
    tree: Tree,
    source: String,
}

/// The C++ language adapter: parses C++ source via `tree-sitter`/
/// `tree-sitter-cpp` and maps it onto the canonical syntax model.
///
/// Structurally, this is `c.rs` (the same base grammar family) extended with
/// C++'s OOP layer: `class`/`struct` with single and multiple inheritance
/// (every base in a `base_class_clause` becomes an `EXTENDS` edge -- the
/// canonical model has no "private inheritance" concept, so the access
/// specifier on each base, read directly off the grammar's own
/// `access_specifier` child node, is deliberately not encoded, matching the
/// sub-phase's own scoping note), templates (type parameters only, same
/// not-yet-wired-into-the-pipeline posture Go/Rust/Java/C# already carry for
/// their own generics), namespaces (mapped onto nested `Module` entities,
/// like C#'s own namespace handling), operator overloading (a normal
/// `Method` named after the operator's own grammar text, e.g.
/// `"operator+"`, not a special kind), and virtual/pure-virtual methods.
///
/// `OVERRIDES` is resolved only from an explicit `override` specifier (the
/// grammar's own `virtual_specifier` node) against a same-named method on
/// one of the class's declared bases -- an unmarked virtual method that
/// happens to share a signature with a base method is a documented gap, not
/// guessed at (full override resolution needs a signature comparison this
/// sub-phase doesn't attempt, the same posture Rust's own `resolve_overrides`
/// takes for trait-impl methods it can't find a trait-side counterpart for).
///
/// `#include` resolution is the same documented gap C's adapter already
/// carries: the preprocessor pass `tree-sitter-cpp` doesn't run. A method
/// defined out-of-line (`Foo::bar() { ... }`, its declarator a
/// `qualified_identifier` rather than a plain name) is a second documented
/// gap specific to this adapter -- the member entity comes only from the
/// in-class declaration; the out-of-line body is not attached to it and not
/// separately modeled (modeling it as a second, unrelated `Function` would
/// misrepresent the graph more than omitting it).
pub struct CppAdapter;

impl LanguageAdapter for CppAdapter {
    type Ast = CppAst;

    fn parse(&self, source: &str) -> Result<Self::Ast> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .map_err(|err| Error::Message(format!("failed to load C++ grammar: {err}")))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::Message("failed to parse C++ source".to_string()))?;
        Ok(CppAst {
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

        edges.extend(resolve_structural(
            &sites,
            all_entities,
            edges_so_far,
            symbol_table,
            &mut symbols,
        ));

        edges.extend(resolve_exports(
            &file_entities,
            &file_edges,
            module_symbol,
            symbol_table,
            &sites.static_names,
        ));

        (symbols, edges)
    }
}

/// Sites `extract_items` collects for `resolve_file`. Mirrors C's own
/// `ResolutionSites` (bodies, `static_names`), extended with Java-style
/// explicit-keyword structural sites for `EXTENDS`/`OVERRIDES`.
#[derive(Default)]
struct ResolutionSites<'a> {
    bodies: Vec<(SyntaxEntity, Node<'a>)>,
    /// Names of top-level functions/variables declared `static` -- internal
    /// linkage, same proxy C's adapter uses for `EXPORTS`.
    static_names: std::collections::HashSet<String>,
    /// A class/struct's own entity paired with every base type name listed
    /// in its `base_class_clause`, access specifier deliberately discarded.
    bases: Vec<(SyntaxEntity, String)>,
    /// A method entity paired with its own name and its owning class
    /// entity's base names, recorded only when the method carries an
    /// explicit `override` specifier.
    overrides: Vec<(SyntaxEntity, String, Vec<String>)>,
}

fn text<'a>(node: Node<'_>, src: &'a str) -> &'a str {
    node.utf8_text(src.as_bytes()).unwrap_or("")
}

fn child_scope(scope: &str, name: &str) -> String {
    format!("{scope}::{name}")
}

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

/// A declarator resolved down to a bare name, or recognized as a function
/// signature (named or operator/destructor) -- mirrors C's own
/// `classify_declarator`/`declarator_identifier`, extended with
/// `reference_declarator` (C++ references) and the name-shapes a C++
/// declarator's innermost identifier can take (`field_identifier` for class
/// members, `operator_name` for operator overloads, `destructor_name` for
/// destructors) alongside C's plain `identifier`.
fn declarator_identifier(node: Node) -> Option<Node> {
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" | "operator_name"
        | "destructor_name" => Some(node),
        "pointer_declarator"
        | "array_declarator"
        | "reference_declarator"
        | "attributed_declarator" => node
            .child_by_field_name("declarator")
            .or_else(|| node.named_child(0))
            .and_then(declarator_identifier),
        "init_declarator" => node
            .child_by_field_name("declarator")
            .and_then(declarator_identifier),
        "parenthesized_declarator" => node.named_child(0).and_then(declarator_identifier),
        _ => None,
    }
}

/// A `function_declarator`'s own name identifier and `parameter_list`,
/// walking down through any pointer/reference/array return-type wrapping.
/// Returns `None` (not a function signature) for a `qualified_identifier`
/// declarator (`Foo::bar`) -- that's an out-of-line definition, a documented
/// gap handled by the caller, not here.
fn function_signature<'a>(node: Node<'a>) -> Option<(Node<'a>, Node<'a>)> {
    match node.kind() {
        "function_declarator" => {
            let inner = node.child_by_field_name("declarator")?;
            let params = node.child_by_field_name("parameters")?;
            let name = declarator_identifier(inner)?;
            Some((name, params))
        }
        "pointer_declarator"
        | "array_declarator"
        | "reference_declarator"
        | "attributed_declarator" => node
            .child_by_field_name("declarator")
            .and_then(function_signature),
        "parenthesized_declarator" => node.named_child(0).and_then(function_signature),
        _ => None,
    }
}

/// Whether a `function_declarator` wraps a `qualified_identifier`
/// declarator (`Foo::bar`) -- the out-of-line-definition shape this adapter
/// deliberately doesn't model, see the module doc comment.
fn is_out_of_line_definition(declarator: Node) -> bool {
    match declarator.kind() {
        "function_declarator" => declarator
            .child_by_field_name("declarator")
            .map(|d| d.kind() == "qualified_identifier")
            .unwrap_or(false),
        "pointer_declarator"
        | "array_declarator"
        | "reference_declarator"
        | "attributed_declarator" => declarator
            .child_by_field_name("declarator")
            .map(is_out_of_line_definition)
            .unwrap_or(false),
        "parenthesized_declarator" => declarator
            .named_child(0)
            .map(is_out_of_line_definition)
            .unwrap_or(false),
        _ => false,
    }
}

/// Whether `declarator` (a `field_declaration`/`function_definition`'s own
/// top-level declarator) is function-shaped, i.e. resolves via
/// `function_signature`.
fn is_function_declarator(declarator: Node) -> bool {
    function_signature(declarator).is_some()
}

/// A declarator's bare field/variable name, unwrapping pointer/reference/
/// array/init wrapping -- used for non-function `field_declaration`s
/// (members) and `declaration`s (globals/locals).
fn declarator_name(node: Node) -> Option<Node> {
    declarator_identifier(node)
}

fn has_static_storage(node: Node, src: &str) -> bool {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(|c| c.kind() == "storage_class_specifier" && text(c, src) == "static");
    found
}

/// A type expression's base declared name: `type_identifier` as-is,
/// `qualified_identifier`'s final `name` segment (`shapes::Circle` ->
/// `Circle`), and `template_type`'s own `name` field (`Box<T>` -> `Box`).
/// Mirrors Java's own `type_base_name`.
fn type_base_name(node: Node, src: &str) -> String {
    match node.kind() {
        "type_identifier" => text(node, src).to_string(),
        "qualified_identifier" => node
            .child_by_field_name("name")
            .map(|n| type_base_name(n, src))
            .unwrap_or_else(|| text(node, src).to_string()),
        "template_type" => node
            .child_by_field_name("name")
            .map(|n| text(n, src).to_string())
            .unwrap_or_else(|| text(node, src).to_string()),
        _ => text(node, src).to_string(),
    }
}

/// Every base type node (`type_identifier`/`qualified_identifier`/
/// `template_type`) listed in a `base_class_clause`, skipping its
/// `access_specifier` children entirely -- the deliberate access-specifier
/// omission documented on `CppAdapter`.
fn base_class_types<'a>(clause: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = clause.walk();
    clause
        .named_children(&mut cursor)
        .filter(|c| c.kind() != "access_specifier")
        .collect()
}

/// Walk a `translation_unit`/`namespace_definition` body's direct children.
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
            "class_specifier" => extract_class_or_struct(
                child,
                src,
                parent,
                scope,
                SyntaxKind::Class,
                entities,
                edges,
                sites,
            ),
            "struct_specifier" => extract_class_or_struct(
                child,
                src,
                parent,
                scope,
                SyntaxKind::Struct,
                entities,
                edges,
                sites,
            ),
            "enum_specifier" => extract_enum(child, src, parent, scope, entities, edges),
            "namespace_definition" => {
                extract_namespace(child, src, parent, scope, entities, edges, sites)
            }
            "template_declaration" => {
                extract_template(child, src, parent, scope, entities, edges, sites)
            }
            "function_definition" => {
                extract_free_function(child, src, parent, scope, entities, edges, sites)
            }
            "declaration" => {
                extract_top_level_declaration(child, src, parent, scope, entities, edges, sites)
            }
            // `preproc_include` and other preprocessor directives: same
            // documented gap as C's adapter (see the module doc comment).
            _ => {}
        }
    }
}

/// A `template_declaration`'s inner declaration, extracted the same way its
/// un-templated form would be -- the template parameter list itself isn't
/// wired into extraction, the same not-yet-wired posture
/// `extract_generic_params` below documents (tested directly).
fn extract_template<'a>(
    node: Node<'a>,
    src: &'a str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_specifier" => extract_class_or_struct(
                child,
                src,
                parent,
                scope,
                SyntaxKind::Class,
                entities,
                edges,
                sites,
            ),
            "struct_specifier" => extract_class_or_struct(
                child,
                src,
                parent,
                scope,
                SyntaxKind::Struct,
                entities,
                edges,
                sites,
            ),
            "function_definition" => {
                extract_free_function(child, src, parent, scope, entities, edges, sites)
            }
            "declaration" => {
                extract_top_level_declaration(child, src, parent, scope, entities, edges, sites)
            }
            _ => {}
        }
    }
}

/// A `namespace_definition` becomes a nested `Module` entity (an anonymous
/// namespace, which has no `name` field, recurses directly into the parent
/// instead -- its members behave as if declared in the enclosing scope,
/// the same posture C's adapter takes toward untracked internal linkage).
fn extract_namespace<'a>(
    node: Node<'a>,
    src: &'a str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    match node.child_by_field_name("name") {
        Some(name_node) => {
            let name = text(name_node, src);
            let entity = SyntaxEntity::new(SyntaxKind::Module, scope, name);
            let ns_scope = child_scope(scope, name);
            declare(parent, entity.clone(), entities, edges);
            extract_items(body, src, &entity, &ns_scope, entities, edges, sites);
        }
        None => {
            extract_items(body, src, parent, scope, entities, edges, sites);
        }
    }
}

/// A `class_specifier`/`struct_specifier` with a body: its own `Class`/
/// `Struct` entity, `base_class_clause` bases recorded for
/// `resolve_structural`, and its member list.
#[allow(clippy::too_many_arguments)]
fn extract_class_or_struct<'a>(
    node: Node<'a>,
    src: &'a str,
    parent: &SyntaxEntity,
    scope: &str,
    kind: SyntaxKind,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    if node.child_by_field_name("body").is_none() {
        // A forward declaration (`class Foo;`) with no body -- nothing to
        // extract, same posture as C's own forward-declared-struct handling.
        return;
    }
    let name = text(name_node, src);
    let entity = SyntaxEntity::new(kind, scope, name);
    let type_scope = child_scope(scope, name);
    declare(parent, entity.clone(), entities, edges);

    // `base_class_clause` is a plain named child, not a fielded one, in
    // `tree-sitter-cpp`'s grammar -- found by kind, same as C's own
    // `storage_class_specifier` lookup.
    let mut base_names = Vec::new();
    let mut cursor = node.walk();
    if let Some(clause) = node
        .children(&mut cursor)
        .find(|c| c.kind() == "base_class_clause")
    {
        for base_node in base_class_types(clause) {
            let base_name = type_base_name(base_node, src);
            sites.bases.push((entity.clone(), base_name.clone()));
            base_names.push(base_name);
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for member in body.children(&mut cursor) {
            extract_member(
                member,
                src,
                &entity,
                &type_scope,
                &base_names,
                entities,
                edges,
                sites,
            );
        }
    }
}

/// A class/struct body's direct members: fields, methods (in-class bodies
/// and signature-only declarations alike -- both become `Method` entities,
/// matching Java's own interface-method-signature treatment), nested types,
/// and access-specifier labels (skipped, access deliberately not modeled).
#[allow(clippy::too_many_arguments)]
fn extract_member<'a>(
    node: Node<'a>,
    src: &'a str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    base_names: &[String],
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    match node.kind() {
        "access_specifier" => {}
        "field_declaration" => {
            let Some(declarator) = node.child_by_field_name("declarator") else {
                return;
            };
            if is_function_declarator(declarator) {
                extract_method_signature(
                    node,
                    declarator,
                    src,
                    owner,
                    owner_scope,
                    base_names,
                    entities,
                    edges,
                    sites,
                );
            } else if let Some(name_node) = declarator_name(declarator) {
                let entity =
                    SyntaxEntity::new(SyntaxKind::Variable, owner_scope, text(name_node, src));
                contain(owner, entity, entities, edges);
            }
        }
        "function_definition" => {
            extract_method_with_body(
                node,
                src,
                owner,
                owner_scope,
                base_names,
                entities,
                edges,
                sites,
            );
        }
        "class_specifier" => extract_class_or_struct(
            node,
            src,
            owner,
            owner_scope,
            SyntaxKind::Class,
            entities,
            edges,
            sites,
        ),
        "struct_specifier" => extract_class_or_struct(
            node,
            src,
            owner,
            owner_scope,
            SyntaxKind::Struct,
            entities,
            edges,
            sites,
        ),
        "enum_specifier" => extract_enum(node, src, owner, owner_scope, entities, edges),
        "template_declaration" => {
            extract_template(node, src, owner, owner_scope, entities, edges, sites)
        }
        _ => {}
    }
}

/// A `field_declaration` whose declarator is function-shaped (a method
/// signature with no in-class body -- either a plain prototype, defined
/// out-of-line elsewhere, or a pure-virtual declaration ending in `= 0`):
/// becomes a `Method` entity with no body, same as Java's abstract/interface
/// method signatures.
#[allow(clippy::too_many_arguments)]
fn extract_method_signature<'a>(
    field_decl: Node<'a>,
    declarator: Node<'a>,
    src: &'a str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    base_names: &[String],
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let Some((name_node, params)) = function_signature(declarator) else {
        return;
    };
    let name = text(name_node, src).to_string();
    let entity = SyntaxEntity::new(SyntaxKind::Method, owner_scope, &name);
    let method_scope = child_scope(owner_scope, &name);
    declare(owner, entity.clone(), entities, edges);
    extract_parameters(params, src, &entity, &method_scope, entities, edges);

    if has_override_specifier(field_decl, src) && !base_names.is_empty() {
        sites.overrides.push((entity, name, base_names.to_vec()));
    }
}

/// A `function_definition` member (an in-class method body, or a
/// constructor/destructor): becomes a `Method` entity with its body
/// extracted and registered for `resolve_body`.
#[allow(clippy::too_many_arguments)]
fn extract_method_with_body<'a>(
    node: Node<'a>,
    src: &'a str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    base_names: &[String],
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let Some(declarator) = node.child_by_field_name("declarator") else {
        return;
    };
    // An out-of-line definition at namespace/global scope is handled by
    // `extract_free_function` (and documented there as not modeled); this
    // function only ever sees an in-class `function_definition`, whose
    // declarator is never a `qualified_identifier`.
    let Some((name_node, params)) = function_signature(declarator) else {
        return;
    };
    let name = text(name_node, src).to_string();
    let entity = SyntaxEntity::new(SyntaxKind::Method, owner_scope, &name);
    let method_scope = child_scope(owner_scope, &name);
    declare(owner, entity.clone(), entities, edges);
    extract_parameters(params, src, &entity, &method_scope, entities, edges);

    if has_override_specifier(declarator, src) && !base_names.is_empty() {
        sites
            .overrides
            .push((entity.clone(), name, base_names.to_vec()));
    }

    if let Some(body) = node.child_by_field_name("body") {
        extract_body(body, src, &entity, &method_scope, entities, edges);
        sites.bodies.push((entity, body));
    }
}

/// Whether `declarator` (a `function_declarator`, possibly wrapped) carries
/// an explicit `override` specifier -- `tree-sitter-cpp`'s own
/// `virtual_specifier` node, covering both `override` and `final` (only
/// `override`'s text is treated as the `OVERRIDES` signal; `final` alone,
/// with no `override`, produces no edge -- a documented narrowing, not a
/// guess).
fn has_override_specifier(node: Node, src: &str) -> bool {
    fn find_function_declarator(node: Node) -> Option<Node> {
        match node.kind() {
            "function_declarator" => Some(node),
            "pointer_declarator"
            | "array_declarator"
            | "reference_declarator"
            | "attributed_declarator" => node
                .child_by_field_name("declarator")
                .and_then(find_function_declarator),
            "parenthesized_declarator" => node.named_child(0).and_then(find_function_declarator),
            "field_declaration" | "function_definition" => node
                .child_by_field_name("declarator")
                .and_then(find_function_declarator),
            _ => None,
        }
    }
    let Some(declarator) = find_function_declarator(node) else {
        return false;
    };
    let mut cursor = declarator.walk();
    let found = declarator
        .children(&mut cursor)
        .any(|c| c.kind() == "virtual_specifier" && text(c, src) == "override");
    found
}

/// A named `enum_specifier` with a body (plain `enum` and `enum class`
/// alike) becomes an `Enum` entity. Enumerator constants aren't extracted,
/// same documented gap as C's adapter.
fn extract_enum(
    node: Node,
    src: &str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    if node.child_by_field_name("body").is_none() {
        return;
    }
    let entity = SyntaxEntity::new(SyntaxKind::Enum, scope, text(name_node, src));
    declare(parent, entity, entities, edges);
}

/// A top-level/namespace-scope `function_definition`: a free `Function`
/// entity when its declarator names a plain identifier or operator; an
/// out-of-line method definition (`Foo::bar() {}`, a `qualified_identifier`
/// declarator) is skipped, the documented gap on `CppAdapter`'s own doc
/// comment.
fn extract_free_function<'a>(
    node: Node<'a>,
    src: &'a str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let Some(declarator) = node.child_by_field_name("declarator") else {
        return;
    };
    if is_out_of_line_definition(declarator) {
        return;
    }
    let Some((name_node, params)) = function_signature(declarator) else {
        return;
    };
    let name = text(name_node, src).to_string();
    if has_static_storage(node, src) {
        sites.static_names.insert(name.clone());
    }

    let entity = SyntaxEntity::new(SyntaxKind::Function, scope, &name);
    let fn_scope = child_scope(scope, &name);
    declare(parent, entity.clone(), entities, edges);
    extract_parameters(params, src, &entity, &fn_scope, entities, edges);

    if let Some(body) = node.child_by_field_name("body") {
        extract_body(body, src, &entity, &fn_scope, entities, edges);
        sites.bodies.push((entity, body));
    }
}

/// A `parameter_list`'s `parameter_declaration`/`optional_parameter_declaration`
/// nodes become `Parameter` entities. An unnamed parameter and the bare
/// `void` marker bind no name -- skipped, not guessed at.
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
        if !matches!(
            param.kind(),
            "parameter_declaration" | "optional_parameter_declaration"
        ) {
            continue;
        }
        let Some(declarator) = param.child_by_field_name("declarator") else {
            continue;
        };
        let Some(name_node) = declarator_name(declarator) else {
            continue;
        };
        let entity = SyntaxEntity::new(SyntaxKind::Parameter, scope, text(name_node, src));
        contain(parent, entity, entities, edges);
    }
}

/// A top-level/namespace-scope `declaration`: a function prototype (no
/// entity, same as C) or one or more global variable declarators, each a
/// `Variable` entity.
fn extract_top_level_declaration<'a>(
    node: Node<'a>,
    src: &'a str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let is_static = has_static_storage(node, src);
    let mut cursor = node.walk();
    for declarator in node.children_by_field_name("declarator", &mut cursor) {
        if is_function_declarator(declarator) {
            continue;
        }
        if let Some(name_node) = declarator_name(declarator) {
            let name = text(name_node, src).to_string();
            if is_static {
                sites.static_names.insert(name.clone());
            }
            let entity = SyntaxEntity::new(SyntaxKind::Variable, scope, name);
            contain(parent, entity, entities, edges);
        }
    }
}

/// Walk a function/method body's direct statements -- a `declaration`
/// becomes `Variable` entities; everything else a generic `Statement`
/// entity. Mirrors C's own `extract_body`.
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
            "declaration" => {
                let mut decl_cursor = stmt.walk();
                for declarator in stmt.children_by_field_name("declarator", &mut decl_cursor) {
                    if let Some(name_node) = declarator_name(declarator) {
                        let entity = SyntaxEntity::new(
                            SyntaxKind::Variable,
                            &format!("{scope}#{index}"),
                            text(name_node, src),
                        );
                        contain(parent, entity, entities, edges);
                    }
                }
            }
            "function_definition" | "class_specifier" | "struct_specifier" | "enum_specifier" => {}
            _ => {
                let entity =
                    SyntaxEntity::new(SyntaxKind::Statement, scope, format!("stmt_{index}"));
                contain(parent, entity, entities, edges);
            }
        }
    }
}

/// `REFERENCES`/`CALLS`/`INSTANTIATES` edges from one function/method body.
/// Mirrors Java's own `resolve_body`: a bare identifier read resolves via
/// `scope_tree`; a bare-name call the same way; a member-call
/// (`obj.method()`/`obj->method()`) resolves by best-effort name match
/// against any `Method` symbol; `new Type(...)` whose type resolves in
/// scope to a `Type` symbol is an `INSTANTIATES` site, otherwise an explicit
/// out-of-scope `Type` placeholder, same as Java's. Anything unresolved gets
/// an explicit placeholder symbol, never silently dropped.
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
        walk_stmt(
            stmt,
            &mut |n| names.push(n),
            &mut |n| calls.push(n),
            &mut |n| news.push(n),
        );
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
        let Some(func) = call_node.child_by_field_name("function") else {
            continue;
        };
        let target = match func.kind() {
            "identifier" => scope_tree
                .lookup(scope, text(func, src))
                .and_then(|id| symbol_table.find_by_syntax_id(id))
                .cloned()
                .or_else(|| {
                    // The scope tree only covers this file's own entities,
                    // so a call to a function declared in another file
                    // never resolves through it -- fall back to a
                    // repo-wide best-effort name match against
                    // `symbol_table`, mirroring the `field_expression`
                    // branch below, which has always searched repo-wide.
                    symbol_table
                        .symbols()
                        .find(|s| s.kind == SymbolKind::Function && s.name == text(func, src))
                        .cloned()
                })
                .unwrap_or_else(|| {
                    let placeholder = unresolved_symbol("unknown-local", text(func, src));
                    symbols.push(placeholder.clone());
                    placeholder
                }),
            "field_expression" => {
                let ident = func
                    .child_by_field_name("field")
                    .map(|n| text(n, src))
                    .unwrap_or("");
                symbol_table
                    .symbols()
                    .find(|s| s.kind == SymbolKind::Method && s.name == ident)
                    .cloned()
                    .unwrap_or_else(|| {
                        let placeholder = unresolved_symbol("unknown-method", ident);
                        symbols.push(placeholder.clone());
                        placeholder
                    })
            }
            "qualified_identifier" => {
                let ident = func
                    .child_by_field_name("name")
                    .map(|n| text(n, src))
                    .unwrap_or("");
                scope_tree
                    .lookup(scope, ident)
                    .and_then(|id| symbol_table.find_by_syntax_id(id))
                    .cloned()
                    .unwrap_or_else(|| {
                        let placeholder = unresolved_symbol("unknown-qualified", ident);
                        symbols.push(placeholder.clone());
                        placeholder
                    })
            }
            _ => {
                let placeholder =
                    unresolved_symbol("unsupported-form", &format!("{:?}", func.kind()));
                symbols.push(placeholder.clone());
                placeholder
            }
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
        "call_expression" => {
            on_call(node);
            match node.child_by_field_name("function") {
                Some(func) if func.kind() == "identifier" => on_name(func),
                Some(func) => walk_expr(func, on_name, on_call, on_new),
                None => {}
            }
            if let Some(args) = node.child_by_field_name("arguments") {
                let mut cursor = args.walk();
                for arg in args.named_children(&mut cursor) {
                    walk_expr(arg, on_name, on_call, on_new);
                }
            }
        }
        "new_expression" => {
            on_new(node);
            if let Some(args) = node.child_by_field_name("arguments") {
                let mut cursor = args.walk();
                for arg in args.named_children(&mut cursor) {
                    walk_expr(arg, on_name, on_call, on_new);
                }
            }
        }
        "field_expression" => {
            if let Some(argument) = node.child_by_field_name("argument") {
                walk_expr(argument, on_name, on_call, on_new);
            }
        }
        "assignment_expression" => {
            if let Some(l) = node.child_by_field_name("left") {
                walk_expr(l, on_name, on_call, on_new);
            }
            if let Some(r) = node.child_by_field_name("right") {
                walk_expr(r, on_name, on_call, on_new);
            }
        }
        "binary_expression" => {
            if let Some(l) = node.child_by_field_name("left") {
                walk_expr(l, on_name, on_call, on_new);
            }
            if let Some(r) = node.child_by_field_name("right") {
                walk_expr(r, on_name, on_call, on_new);
            }
        }
        "unary_expression" | "update_expression" => {
            if let Some(operand) = node.child_by_field_name("argument") {
                walk_expr(operand, on_name, on_call, on_new);
            }
        }
        "parenthesized_expression" => {
            if let Some(inner) = node.named_child(0) {
                walk_expr(inner, on_name, on_call, on_new);
            }
        }
        "pointer_expression" => {
            if let Some(operand) = node.named_child(0) {
                walk_expr(operand, on_name, on_call, on_new);
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
        "return_statement" => {
            if let Some(inner) = node.named_child(0) {
                walk_expr(inner, on_name, on_call, on_new);
            }
        }
        "declaration" => {
            let mut cursor = node.walk();
            for declarator in node.children_by_field_name("declarator", &mut cursor) {
                if declarator.kind() == "init_declarator" {
                    if let Some(value) = declarator.child_by_field_name("value") {
                        walk_expr(value, on_name, on_call, on_new);
                    }
                }
            }
        }
        "if_statement" => {
            if let Some(cond) = node.child_by_field_name("condition") {
                walk_expr(cond, on_name, on_call, on_new);
            }
            if let Some(consequence) = node.child_by_field_name("consequence") {
                walk_stmt(consequence, on_name, on_call, on_new);
            }
            if let Some(alternative) = node.child_by_field_name("alternative") {
                walk_stmt(alternative, on_name, on_call, on_new);
            }
        }
        "while_statement" => {
            if let Some(cond) = node.child_by_field_name("condition") {
                walk_expr(cond, on_name, on_call, on_new);
            }
            if let Some(body) = node.child_by_field_name("body") {
                walk_stmt(body, on_name, on_call, on_new);
            }
        }
        "for_statement" => {
            if let Some(body) = node.child_by_field_name("body") {
                walk_stmt(body, on_name, on_call, on_new);
            }
        }
        "compound_statement" => walk_block_statements(node, on_name, on_call, on_new),
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
        walk_stmt(stmt, on_name, on_call, on_new);
    }
}

fn find_entity_by_kinds_name<'a>(
    entities: &'a [SyntaxEntity],
    kinds: &[SyntaxKind],
    name: &str,
) -> Option<&'a SyntaxEntity> {
    entities
        .iter()
        .find(|e| kinds.contains(&e.kind) && e.name == name)
}

/// Resolve `sites.bases` into `EXTENDS` edges (searched repository-wide, so
/// a base declared in another file still resolves) and `sites.overrides`
/// into `OVERRIDES` edges against a same-named method declared on one of
/// the class's own bases. An unresolved base/override target gets an
/// explicit placeholder symbol, never silently dropped.
fn resolve_structural(
    sites: &ResolutionSites,
    all_entities: &[SyntaxEntity],
    edges_so_far: &[Edge],
    symbol_table: &SymbolTable,
    symbols: &mut Vec<SymbolEntity>,
) -> Vec<Edge> {
    let mut edges = Vec::new();

    for (entity, base_name) in &sites.bases {
        let Some(owner_symbol) = symbol_table.find_by_syntax_id(&entity.id) else {
            continue;
        };
        let target = find_entity_by_kinds_name(
            all_entities,
            &[SyntaxKind::Class, SyntaxKind::Struct],
            base_name,
        )
        .and_then(|e| symbol_table.find_by_syntax_id(&e.id))
        .cloned()
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

    for (method_entity, method_name, base_names) in &sites.overrides {
        let Some(method_symbol) = symbol_table.find_by_syntax_id(&method_entity.id) else {
            continue;
        };
        let mut resolved = None;
        for base_name in base_names {
            let Some(base_entity) = find_entity_by_kinds_name(
                all_entities,
                &[SyntaxKind::Class, SyntaxKind::Struct],
                base_name,
            ) else {
                continue;
            };
            let base_method = edges_so_far
                .iter()
                .filter(|e| e.kind == EdgeKind::Declares && e.from == base_entity.id)
                .filter_map(|e| all_entities.iter().find(|ent| ent.id == e.to))
                .find(|ent| ent.kind == SyntaxKind::Method && ent.name == *method_name);
            if let Some(base_method) = base_method {
                if let Some(symbol) = symbol_table.find_by_syntax_id(&base_method.id) {
                    resolved = Some(symbol.clone());
                    break;
                }
            }
        }
        let target = resolved.unwrap_or_else(|| {
            let placeholder = unresolved_symbol(
                "external-method",
                &format!("{}::{}", base_names.join("|"), method_name),
            );
            symbols.push(placeholder.clone());
            placeholder
        });
        edges.push(Edge::new(
            method_symbol.id.clone(),
            target.id,
            EdgeKind::Overrides,
        ));
    }

    edges
}

/// Resolve a file's top-level/namespace-scope functions/variables into
/// `EXPORTS` edges, using `static`/default linkage exactly as C's own
/// `resolve_exports` does. Classes/structs/enums carry no linkage
/// specifier in C++ -- always exported when declared.
fn resolve_exports(
    file_entities: &[SyntaxEntity],
    file_edges: &[Edge],
    module_symbol: &SymbolEntity,
    symbol_table: &SymbolTable,
    static_names: &std::collections::HashSet<String>,
) -> Vec<Edge> {
    const EXPORTABLE_KINDS: [SyntaxKind; 5] = [
        SyntaxKind::Function,
        SyntaxKind::Variable,
        SyntaxKind::Class,
        SyntaxKind::Struct,
        SyntaxKind::Enum,
    ];

    let mut edges = Vec::new();
    let module_id = &file_entities[0].id;
    for entity_id in file_edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Declares && &e.from == module_id)
        .map(|e| &e.to)
    {
        let Some(entity) = file_entities.iter().find(|e| &e.id == entity_id) else {
            continue;
        };
        if !EXPORTABLE_KINDS.contains(&entity.kind) {
            continue;
        }
        if static_names.contains(&entity.name) {
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

/// Map a C++ type expression to a `TypeRef`. Not yet wired into
/// `extract`/`resolve_file` -- same not-yet-wired `ACCEPTS`/`RETURNS` gap
/// every other adapter's own `extract_type` carries (tested directly).
#[allow(dead_code)]
fn extract_type(type_node: Node, declarator: Option<Node>, src: &str) -> TypeRef {
    let mut declarator = declarator;
    let mut pointers = 0usize;
    loop {
        match declarator.map(|d| d.kind()) {
            Some("pointer_declarator") | Some("reference_declarator") => {
                pointers += 1;
                declarator = declarator.and_then(|d| {
                    d.child_by_field_name("declarator")
                        .or_else(|| d.named_child(0))
                });
            }
            Some("parenthesized_declarator") => {
                declarator = declarator.and_then(|d| d.named_child(0));
            }
            _ => break,
        }
    }

    let base = match type_node.kind() {
        "primitive_type" => {
            let name = text(type_node, src);
            if PRIMITIVE_TYPES.contains(&name) {
                TypeRef::Primitive(name.to_string())
            } else {
                TypeRef::Unresolved(name.to_string())
            }
        }
        "type_identifier" => TypeRef::Named {
            name: text(type_node, src).to_string(),
            type_args: Vec::new(),
        },
        "template_type" => {
            let name = type_node
                .child_by_field_name("name")
                .map(|n| text(n, src).to_string())
                .unwrap_or_else(|| text(type_node, src).to_string());
            let type_args = type_node
                .child_by_field_name("arguments")
                .map(|args| {
                    let mut cursor = args.walk();
                    args.named_children(&mut cursor)
                        .map(|arg| extract_type(arg, None, src))
                        .collect()
                })
                .unwrap_or_default();
            TypeRef::Named { name, type_args }
        }
        "qualified_identifier" => TypeRef::Named {
            name: type_base_name(type_node, src),
            type_args: Vec::new(),
        },
        _ => TypeRef::Unresolved(text(type_node, src).to_string()),
    };

    let mut result = base;
    for _ in 0..pointers {
        result = TypeRef::Reference {
            mutable: true,
            inner: Box::new(result),
        };
    }
    result
}

/// A `template_declaration`'s own `template_parameter_list` becomes
/// `Generic` symbols declared under `owner_symbol` -- not yet wired into
/// `extract`/`resolve_file`, matching Go/Rust/Java/C#'s own
/// `extract_generic_params` gap.
#[allow(dead_code)]
fn extract_generic_params(
    node: Node,
    src: &str,
    owner_symbol: &SymbolEntity,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    let Some(type_params) = node.child_by_field_name("parameters") else {
        return (symbols, edges);
    };

    let mut cursor = type_params.walk();
    for param in type_params.children(&mut cursor) {
        if !matches!(
            param.kind(),
            "type_parameter_declaration" | "variadic_type_parameter_declaration"
        ) {
            continue;
        }
        let Some(name_node) = param.named_child(0) else {
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
        let adapter = CppAdapter;
        let ast = adapter.parse(source).expect("valid C++ parses");
        adapter.extract(&ast, "module.cpp")
    }

    fn parse_source(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn parses_valid_cpp_source() {
        let adapter = CppAdapter;
        assert!(adapter.parse("int main() { return 0; }\n").is_ok());
    }

    #[test]
    fn extract_always_returns_the_file_module_first() {
        let (entities, _) = extract("int x;\n");
        assert_eq!(entities[0].kind, SyntaxKind::Module);
        assert_eq!(entities[0].name, "module.cpp");
    }

    #[test]
    fn extracts_a_free_function_with_its_parameters() {
        let (entities, edges) = extract("int add(int a, int b) {\n  return a + b;\n}\n");
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
    fn extracts_a_global_variable() {
        let (entities, _) = extract("int global_count;\n");
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Variable && e.name == "global_count"));
    }

    #[test]
    fn extracts_a_struct_with_a_field() {
        let (entities, edges) = extract("struct Point {\n  double x;\n  double y;\n};\n");
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
        let field = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Variable && e.name == "x")
            .expect("field entity");
        assert!(edges.contains(&Edge::new(
            point.id.clone(),
            field.id.clone(),
            EdgeKind::Contains
        )));
    }

    #[test]
    fn extracts_a_class_with_a_method_and_its_parameters() {
        let (entities, edges) = extract(
            "class Calculator {\npublic:\n  int add(int a, int b) {\n    return a + b;\n  }\n};\n",
        );
        let class = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Calculator")
            .expect("Class entity");
        let method = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "add")
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
    fn extracts_single_inheritance() {
        let source = "class Animal {};\nclass Dog : public Animal {};\n";
        let (entities, _) = extract(source);
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Class && e.name == "Animal"));
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Class && e.name == "Dog"));
    }

    #[test]
    fn extracts_multiple_inheritance_bases() {
        let source =
            "class Base1 {};\nclass Base2 {};\nclass Derived : public Base1, private Base2 {};\n";
        let adapter = CppAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "module.cpp");
        let derived = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Derived")
            .unwrap();
        let mut sites = ResolutionSites::default();
        let mut e2 = vec![entities[0].clone()];
        let mut ed2 = Vec::new();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "module.cpp",
            &mut e2,
            &mut ed2,
            &mut sites,
        );
        let base_names: Vec<&str> = sites
            .bases
            .iter()
            .filter(|(e, _)| e.id == derived.id)
            .map(|(_, n)| n.as_str())
            .collect();
        assert_eq!(base_names, vec!["Base1", "Base2"]);
    }

    #[test]
    fn extracts_a_pure_virtual_method_signature_with_no_body() {
        let (entities, _) =
            extract("class Shape {\npublic:\n  virtual double area() const = 0;\n};\n");
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Method && e.name == "area"));
    }

    #[test]
    fn extracts_an_operator_overload_as_a_method_named_after_the_operator() {
        let source = "class Vec2 {\npublic:\n  Vec2 operator+(const Vec2& other) const { return Vec2(); }\n};\n";
        let (entities, _) = extract(source);
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Method && e.name == "operator+"));
    }

    #[test]
    fn extracts_a_namespace_as_a_nested_module() {
        let source = "namespace shapes {\n  class Circle {};\n}\n";
        let (entities, edges) = extract(source);
        let module = &entities[0];
        let ns = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Module && e.name == "shapes")
            .expect("nested Module entity for the namespace");
        assert!(edges.contains(&Edge::new(
            module.id.clone(),
            ns.id.clone(),
            EdgeKind::Declares
        )));
        let circle = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Circle")
            .expect("Class entity inside the namespace");
        assert!(edges.contains(&Edge::new(
            ns.id.clone(),
            circle.id.clone(),
            EdgeKind::Declares
        )));
    }

    #[test]
    fn extracts_a_template_class() {
        let source = "template<typename T>\nclass Box {\npublic:\n  T get() { return value; }\nprivate:\n  T value;\n};\n";
        let (entities, _) = extract(source);
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Class && e.name == "Box"));
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Method && e.name == "get"));
    }

    #[test]
    fn extracts_a_template_function() {
        let source = "template<typename T>\nT identity(T x) { return x; }\n";
        let (entities, _) = extract(source);
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Function && e.name == "identity"));
    }

    #[test]
    fn an_out_of_line_method_definition_produces_no_duplicate_function_entity() {
        let source = "class Foo {\npublic:\n  void bar();\n};\nvoid Foo::bar() { }\n";
        let (entities, _) = extract(source);
        // The in-class prototype is still a Method entity...
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Method && e.name == "bar"));
        // ...but the out-of-line definition is not modeled as a free Function
        // (the documented gap on `CppAdapter`).
        assert!(!entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Function && e.name == "bar"));
    }

    #[test]
    fn a_local_variable_declaration_is_extracted_as_a_variable() {
        let (entities, edges) = extract("void run() {\n  int total = 1;\n  total = 2;\n}\n");
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
    }

    #[test]
    fn a_non_declaration_statement_becomes_a_generic_statement_entity() {
        let (entities, _) = extract("void run() {\n  helper();\n}\nvoid helper() {}\n");
        assert!(entities.iter().any(|e| e.kind == SyntaxKind::Statement));
    }

    // -- type extraction (not yet wired into extract/resolve_file) --

    fn find_top_level<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        let mut cursor = tree.root_node().walk();
        let found = tree.root_node().children(&mut cursor).find(|c| {
            c.kind() == "function_definition"
                && c.child_by_field_name("declarator")
                    .and_then(function_signature)
                    .map(|(n, _)| text(n, src) == name)
                    .unwrap_or(false)
        });
        found.unwrap_or_else(|| panic!("no function named {name}"))
    }

    #[test]
    fn extracts_a_primitive_type() {
        let full = "int f() { return 0; }\n";
        let tree = parse_source(full);
        let node = find_top_level(&tree, full, "f");
        let type_node = node.child_by_field_name("type").unwrap();
        assert_eq!(
            extract_type(type_node, None, full),
            TypeRef::Primitive("int".to_string())
        );
    }

    #[test]
    fn extracts_a_named_class_type() {
        let full = "Circle f() { Circle c; return c; }\n";
        let tree = parse_source(full);
        let node = find_top_level(&tree, full, "f");
        let type_node = node.child_by_field_name("type").unwrap();
        assert_eq!(
            extract_type(type_node, None, full),
            TypeRef::Named {
                name: "Circle".to_string(),
                type_args: Vec::new(),
            }
        );
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
        let adapter = CppAdapter;
        let ast = adapter.parse(source).expect("valid C++ parses");
        let (entities, edges) = adapter.extract(&ast, "module.cpp");
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
        let mut cursor = tree.root_node().walk();
        let node = tree
            .root_node()
            .children(&mut cursor)
            .find(|c| {
                c.kind() == "function_definition"
                    && c.child_by_field_name("declarator")
                        .and_then(function_signature)
                        .map(|(n, _)| text(n, src) == name)
                        .unwrap_or(false)
            })
            .unwrap_or_else(|| panic!("no function named {name}"));
        node.child_by_field_name("body").unwrap()
    }

    #[test]
    fn resolves_a_reference_to_an_in_scope_variable() {
        let source = "void run() {\n  int value = 1;\n  value = 2;\n}\n";
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
    fn resolves_a_free_function_call() {
        let source = "void helper() {}\n\nvoid run() {\n  helper();\n}\n";
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
        // another file fell straight to an `unknown-local` placeholder.
        use crate::symbols::build_symbol_table;

        let adapter = CppAdapter;
        let helper_ast = adapter.parse("void helper() {}\n").unwrap();
        let (helper_entities, helper_edges) = adapter.extract(&helper_ast, "helper.cpp");
        let caller_ast = adapter.parse("void run() {\n  helper();\n}\n").unwrap();
        let (caller_entities, caller_edges) = adapter.extract(&caller_ast, "run.cpp");

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
            "run.cpp",
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
    fn resolves_a_new_expression_as_instantiation() {
        let source = "class Circle {};\n\nvoid run() {\n  Circle* c = new Circle();\n}\n";
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
    fn a_reference_to_an_undefined_name_gets_an_explicit_placeholder() {
        let source = "void run() {\n  missing = 1;\n}\n";
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
    fn resolves_single_inheritance_as_an_extends_edge() {
        let source = "class Animal {};\nclass Dog : public Animal {};\n";
        let adapter = CppAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, edges) = adapter.extract(&ast, "module.cpp");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table
            .find_by_syntax_id(&entities[0].id)
            .unwrap()
            .clone();
        let file_module = entities[0].clone();

        let (_, resolved_edges) = adapter.resolve_file(
            &ast,
            "module.cpp",
            &entities,
            &symbol_table,
            &edges,
            &module_symbol,
            &file_module,
        );

        let animal = entities.iter().find(|e| e.name == "Animal").unwrap();
        let dog = entities.iter().find(|e| e.name == "Dog").unwrap();
        let animal_symbol = symbol_table.find_by_syntax_id(&animal.id).unwrap();
        let dog_symbol = symbol_table.find_by_syntax_id(&dog.id).unwrap();
        assert!(resolved_edges.contains(&Edge::new(
            dog_symbol.id.clone(),
            animal_symbol.id.clone(),
            EdgeKind::Extends
        )));
    }

    #[test]
    fn resolves_an_override_to_the_base_classs_virtual_method() {
        let source = "class Shape {\npublic:\n  virtual double area() const = 0;\n};\nclass Circle : public Shape {\npublic:\n  double area() const override { return 1.0; }\n};\n";
        let adapter = CppAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, edges) = adapter.extract(&ast, "module.cpp");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table
            .find_by_syntax_id(&entities[0].id)
            .unwrap()
            .clone();
        let file_module = entities[0].clone();

        let (_, resolved_edges) = adapter.resolve_file(
            &ast,
            "module.cpp",
            &entities,
            &symbol_table,
            &edges,
            &module_symbol,
            &file_module,
        );

        let shape = entities.iter().find(|e| e.name == "Shape").unwrap();
        let circle = entities.iter().find(|e| e.name == "Circle").unwrap();
        let shape_area = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Declares && e.from == shape.id)
            .filter_map(|e| entities.iter().find(|ent| ent.id == e.to))
            .find(|e| e.kind == SyntaxKind::Method && e.name == "area")
            .unwrap();
        let circle_area = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Declares && e.from == circle.id)
            .filter_map(|e| entities.iter().find(|ent| ent.id == e.to))
            .find(|e| e.kind == SyntaxKind::Method && e.name == "area")
            .unwrap();
        let shape_symbol = symbol_table.find_by_syntax_id(&shape_area.id).unwrap();
        let circle_symbol = symbol_table.find_by_syntax_id(&circle_area.id).unwrap();
        assert!(resolved_edges.contains(&Edge::new(
            circle_symbol.id.clone(),
            shape_symbol.id.clone(),
            EdgeKind::Overrides
        )));
    }

    #[test]
    fn exports_a_default_linkage_function_but_not_a_static_one() {
        let source = "int public_fn() { return 0; }\nstatic int hidden_fn() { return 0; }\n";
        let adapter = CppAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, edges) = adapter.extract(&ast, "module.cpp");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table
            .find_by_syntax_id(&entities[0].id)
            .unwrap()
            .clone();
        let file_module = entities[0].clone();

        let (_, resolved_edges) = adapter.resolve_file(
            &ast,
            "module.cpp",
            &entities,
            &symbol_table,
            &edges,
            &module_symbol,
            &file_module,
        );

        let public_fn = entities.iter().find(|e| e.name == "public_fn").unwrap();
        let public_symbol = symbol_table.find_by_syntax_id(&public_fn.id).unwrap();
        let export_edges: Vec<_> = resolved_edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Exports)
            .collect();
        assert_eq!(export_edges.len(), 1);
        assert!(export_edges.contains(&&Edge::new(
            module_symbol.id.clone(),
            public_symbol.id.clone(),
            EdgeKind::Exports
        )));
    }
}
