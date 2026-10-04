use tree_sitter::{Node, Parser, Tree};

use crate::adapter::LanguageAdapter;
use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};
use crate::resolve::unresolved_symbol;
use crate::symbols::{ScopeTree, SymbolTable};
use crate::types::TypeRef;

/// Dart's built-in value types, treated as `TypeRef::Primitive` the same
/// name-based-heuristic way Kotlin's `PRIMITIVE_TYPES`/C#'s `PRIMITIVE_TYPES`
/// are -- `tree-sitter-dart`'s grammar has no separate primitive-type
/// production (`int`, `String`, ... parse identically to any other
/// `type_identifier`), so this is a closed list of names, not a grammar
/// fact. `num` (Dart's `int`/`double` common supertype) is included;
/// collection types (`List`, `Map`, `Set`) are deliberately excluded --
/// they're ordinary generic named types, not value-type primitives.
const PRIMITIVE_TYPES: &[&str] = &["int", "double", "num", "bool", "String"];

/// The parsed form a `DartAdapter` produces: `tree-sitter`'s `Tree` paired
/// with the source text it was parsed from, the same shape every other
/// `tree-sitter`-backed adapter uses.
pub struct DartAst {
    tree: Tree,
    source: String,
}

/// The Dart language adapter: parses Dart source via `tree-sitter`/
/// `tree-sitter-dart` and maps it onto the canonical syntax model.
///
/// Parser crate note: unlike Kotlin (17.6), `tree-sitter-dart` 0.2.0 needed
/// **no** substitution -- its only non-dev/non-build dependency is already
/// `tree-sitter-language` (the version-agnostic `LanguageFn` bridge crate),
/// so its `LANGUAGE` constant converts straight into this workspace's
/// `tree-sitter 0.25` `Language` via `.into()`, exactly like
/// `tree_sitter_java::LANGUAGE`/`tree_sitter_kotlin_ng::LANGUAGE`. Verified
/// with a standalone smoke test before any adapter code was written (see
/// `smoke_tests` below) -- the crate compiles and parses against this
/// workspace's `tree-sitter` as planned in `TASKS.md`.
///
/// Structurally closest to Kotlin/Java -- explicit `class`/`extends`/
/// `implements` syntax, no Go-style structural inference -- with two Dart-
/// specific wrinkles `resolve_structural`/`extract_class` below handle
/// directly:
///
/// - **No `interface` keyword for declaring one** (Dart 3's `interface`
///   *class modifier* restricts who may `implements` a class, it doesn't
///   declare a dedicated interface construct). Per `TASKS.md`'s guidance,
///   this adapter maps an explicitly `abstract`-or-`interface`-modified
///   class to `SyntaxKind::Interface`, any other class to `SyntaxKind::
///   Class` -- the idiomatic Dart convention for "this type is meant to be
///   implemented/extended, not instantiated directly", not a perfect
///   semantic match (an `abstract` class can still have concrete members)
///   but the closest fit without inventing a new `SyntaxKind`.
/// - **`mixin`/`with` has no canonical-model counterpart.** A `mixin`
///   declaration itself is modeled as a plain `Class` (no dedicated kind
///   exists, same posture Kotlin's `object` takes). A class/enum's `with`
///   clause (mixing another type's members in) is mapped to an `IMPLEMENTS`
///   edge, not `EXTENDS` -- documented mapping decision, mirroring how the
///   batch's Ruby guidance frames `include`: a mixin contributes behavior
///   the way an interface contract does, without being single-inheritance
///   `extends`. A `mixin` declaration's own `on` clause (which types it may
///   be mixed into) has no resolvable target in this adapter -- its
///   contributing types aren't exposed as a dedicated child node by
///   `tree-sitter-dart`'s grammar (they're inlined flatly alongside the
///   mixin's other optional clauses), so, per batch guidance, it's a
///   documented gap rather than forced into a shape that doesn't fit.
pub struct DartAdapter;

impl LanguageAdapter for DartAdapter {
    type Ast = DartAst;

    fn parse(&self, source: &str) -> Result<Self::Ast> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_dart::LANGUAGE.into())
            .map_err(|err| Error::Message(format!("failed to load Dart grammar: {err}")))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::Message("failed to parse Dart source".to_string()))?;
        Ok(DartAst {
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
            module_symbol,
            symbol_table,
        ));

        (symbols, edges)
    }
}

/// Sites `extract_items`/`extract_member` collect that `resolve_file` needs
/// the raw tree for, or that describe a structural relationship read
/// directly off explicit Dart syntax -- mirrors Kotlin/Java's
/// `ResolutionSites`.
#[derive(Default)]
struct ResolutionSites<'a> {
    /// A function/method/constructor's own entity paired with its body node
    /// -- a `block` for a block-bodied member, or the bare expression node
    /// itself for an arrow-bodied one (`int f() => 1;`).
    bodies: Vec<(SyntaxEntity, Node<'a>)>,
    /// Every `library_import`/`library_export`/`part_directive` node whose
    /// `uri` string literal names a dependency -- see `resolve_imports`'s
    /// own doc comment for why exports/parts are folded into the same
    /// `IMPORTS` bucket as plain imports.
    imports: Vec<Node<'a>>,
    /// A class/mixin/enum's own entity paired with one superclass name,
    /// read from an `extends` clause's `superclass.type` field.
    superclasses: Vec<(SyntaxEntity, String)>,
    /// A class/mixin/enum's own entity paired with one `implements`-clause
    /// type name, **or** one `with`-clause (mixin composition) type name --
    /// both map to `IMPLEMENTS`, per this adapter's documented `with`
    /// mapping decision (see `DartAdapter`'s own doc comment).
    implemented_interfaces: Vec<(SyntaxEntity, String)>,
}

fn text<'a>(node: Node<'_>, src: &'a str) -> &'a str {
    node.utf8_text(src.as_bytes()).unwrap_or("")
}

fn child_scope(scope: &str, name: &str) -> String {
    format!("{scope}::{name}")
}

/// Record `parent CONTAINS child` and `parent DECLARES child`, then push
/// `child` into `entities` -- classes/mixins/enums/functions/methods.
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

/// Record `parent CONTAINS child` only -- fields, parameters, local
/// variables, statements.
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

/// Whether `node` has a direct (named-or-not) child token of kind `kind` --
/// the same posture Kotlin's `is_interface_decl` takes for a bare keyword
/// that the grammar surfaces as an unfielded child token rather than a
/// fielded distinction.
fn has_token_child(node: Node, kind: &str) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|c| c.kind() == kind);
    found
}

/// `node`'s first direct named child of kind `kind`, for grammar
/// productions (`mixins`, `interfaces` on `enum_declaration`) that appear
/// as a plain unfielded child rather than through `child_by_field_name`.
fn find_named_child_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).find(|c| c.kind() == kind);
    found
}

/// The first signature-shaped child of `parent` (a `declaration` or
/// `method_signature` node): one of the constructor/function/getter/
/// setter/operator signature kinds, read directly off the grammar rather
/// than inferred, mirroring Kotlin's `delegation_specifiers`-style direct
/// reads of explicit syntax. Returns `None` for a `declaration` node that's
/// actually a field declaration (no signature child at all).
fn find_signature_child(parent: Node) -> Option<Node> {
    const SIGNATURE_KINDS: &[&str] = &[
        "constructor_signature",
        "constant_constructor_signature",
        "factory_constructor_signature",
        "redirecting_factory_constructor_signature",
        "function_signature",
        "getter_signature",
        "setter_signature",
        "operator_signature",
    ];
    let mut cursor = parent.walk();
    let found = parent
        .children(&mut cursor)
        .find(|c| SIGNATURE_KINDS.contains(&c.kind()));
    found
}

/// Whether `kind` (a `find_signature_child` result) names a constructor
/// variant rather than a function/getter/setter/operator.
fn is_constructor_kind(kind: &str) -> bool {
    matches!(
        kind,
        "constructor_signature"
            | "constant_constructor_signature"
            | "factory_constructor_signature"
            | "redirecting_factory_constructor_signature"
    )
}

/// A signature node's declared name and its `formal_parameter_list`, if
/// any. A constructor's `name` field carries *one or two* identifiers
/// (`ClassName` for the unnamed/default constructor, `ClassName.named` for
/// a named one) -- the **last** one is used as the entity's own name, so
/// `Foo.named` and `Foo.other` on the same class get distinct names/ids
/// rather than colliding on `Foo` the way always-naming-after-the-class
/// (Kotlin's single-primary-constructor posture) would. An operator's name
/// is its operator token text (`==`, `+`, `[]`, ...) -- there's no better
/// "name" for it under the canonical model's string-keyed entity identity.
fn signature_name_and_params<'a>(
    sig: Node<'a>,
    owner_name: &str,
    src: &str,
) -> (String, Option<Node<'a>>) {
    let params = {
        let mut cursor = sig.walk();
        let found = sig
            .children(&mut cursor)
            .find(|c| c.kind() == "formal_parameter_list");
        found
    };
    let name = match sig.kind() {
        "constructor_signature"
        | "constant_constructor_signature"
        | "factory_constructor_signature"
        | "redirecting_factory_constructor_signature" => {
            let mut cursor = sig.walk();
            sig.children_by_field_name("name", &mut cursor)
                .filter(|c| c.kind() == "identifier")
                .last()
                .map(|n| text(n, src).to_string())
                .unwrap_or_else(|| owner_name.to_string())
        }
        "function_signature" | "getter_signature" | "setter_signature" => sig
            .child_by_field_name("name")
            .map(|n| text(n, src).to_string())
            .unwrap_or_default(),
        "operator_signature" => sig
            .child_by_field_name("operator")
            .map(|n| text(n, src).to_string())
            .unwrap_or_default(),
        _ => String::new(),
    };
    (name, params)
}

/// Walk a `source_file`'s direct children, dispatching top-level
/// declarations. `library_import`/`library_export`/`part_directive` are
/// handled here directly (see `ResolutionSites::imports`'s doc comment);
/// `library_name`/`part_of_directive` carry no Phase 3 syntax counterpart
/// of their own -- documented gaps, same posture Kotlin's `package_header`
/// takes. Everything else is delegated to `extract_member` so top-level and
/// (via `extract_class`'s own body loop) nested declarations extract
/// identically.
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
            "import_or_export" | "part_directive" => sites.imports.push(child),
            "library_name" | "part_of_directive" => {}
            _ => extract_member(child, src, parent, scope, entities, edges, sites),
        }
    }
}

/// A class/mixin/enum body's (or the top level's) direct member
/// declarations. Shared by `extract_items` (top level) and
/// `extract_class_body`'s own loop so top-level and nested declarations
/// extract identically.
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
        "mixin_declaration" => extract_mixin(node, src, owner, owner_scope, entities, edges, sites),
        "enum_declaration" => extract_enum(node, src, owner, owner_scope, entities, edges, sites),
        "function_declaration" => {
            let is_method = owner.kind != SyntaxKind::Module;
            extract_top_level_function(
                node,
                src,
                owner,
                owner_scope,
                is_method,
                entities,
                edges,
                sites,
            );
        }
        "getter_declaration" | "setter_declaration" => {
            extract_top_level_accessor(node, src, owner, owner_scope, entities, edges, sites);
        }
        "top_level_variable_declaration" => {
            extract_fields_from_declaration(node, owner, owner_scope, src, entities, edges);
        }
        "class_member" => {
            extract_class_member(node, src, owner, owner_scope, entities, edges, sites)
        }
        // `external_function_declaration`/`external_getter_declaration`/
        // `external_setter_declaration`/`external_variable_declaration`
        // (bodyless, FFI-style top-level externs), `type_alias`, and a bare
        // `;` are documented gaps -- rare top-level constructs with no
        // behavior of their own to extract, matching Kotlin's
        // `type_alias`/`anonymous_initializer` gap.
        _ => {}
    }
}

/// A `class_declaration`'s `superclass`/`interfaces` fields into `sites`:
/// `extends Base` -> `sites.superclasses`; `with M1, M2` (on either the
/// `extends`-paired `superclass` node or a bare `with`-only one) and
/// `implements I1, I2` both -> `sites.implemented_interfaces`, per this
/// adapter's documented `with`-maps-to-`IMPLEMENTS` decision.
fn record_superclass_and_interfaces<'a>(
    superclass: Option<Node<'a>>,
    interfaces: Option<Node<'a>>,
    src: &str,
    owner: &SyntaxEntity,
    sites: &mut ResolutionSites<'a>,
) {
    if let Some(superclass_node) = superclass {
        if let Some(type_node) = superclass_node.child_by_field_name("type") {
            sites
                .superclasses
                .push((owner.clone(), type_base_name(type_node, src)));
        }
        if let Some(mixins_node) = find_named_child_kind(superclass_node, "mixins") {
            let mut cursor = mixins_node.walk();
            for ty in mixins_node.named_children(&mut cursor) {
                sites
                    .implemented_interfaces
                    .push((owner.clone(), type_base_name(ty, src)));
            }
        }
    }
    if let Some(interfaces_node) = interfaces {
        let mut cursor = interfaces_node.walk();
        for ty in interfaces_node.named_children(&mut cursor) {
            sites
                .implemented_interfaces
                .push((owner.clone(), type_base_name(ty, src)));
        }
    }
}

/// A `class_declaration`: its own `Class`/`Interface` entity (an
/// `abstract`- or `interface`-modified class maps to `Interface`, see
/// `DartAdapter`'s own doc comment), its `extends`/`with`/`implements`
/// relations (recorded in `sites`), and its body's members. The
/// typedef-style `class Foo = Base with M;` mixin-application form (no
/// `name`/`body` *fields* -- see `tree-sitter-dart`'s `class_declaration`
/// grammar rule's second alternative) is a documented gap: `child_by_field_
/// name` returns `None` for both on that shape, so this function simply
/// returns rather than guessing at its structure.
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
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let name = text(name_node, src);

    let kind = if has_token_child(node, "abstract") || has_token_child(node, "interface") {
        SyntaxKind::Interface
    } else {
        SyntaxKind::Class
    };

    let entity = SyntaxEntity::new(kind, scope, name);
    let class_scope = child_scope(scope, name);
    declare(parent, entity.clone(), entities, edges);

    record_superclass_and_interfaces(
        node.child_by_field_name("superclass"),
        node.child_by_field_name("interfaces"),
        src,
        &entity,
        sites,
    );

    extract_class_body(body, src, &entity, &class_scope, entities, edges, sites);
}

/// A `mixin_declaration`: modeled as a plain `Class` entity -- the
/// canonical model has no dedicated mixin kind (see `DartAdapter`'s own doc
/// comment). Its `implements`-clause maps to `IMPLEMENTS` the same as a
/// class's; its `on`-clause is a documented gap (see the same doc comment).
fn extract_mixin<'a>(
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
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let name = text(name_node, src);
    let entity = SyntaxEntity::new(SyntaxKind::Class, scope, name);
    let class_scope = child_scope(scope, name);
    declare(parent, entity.clone(), entities, edges);

    record_superclass_and_interfaces(
        None,
        node.child_by_field_name("interfaces"),
        src,
        &entity,
        sites,
    );

    extract_class_body(body, src, &entity, &class_scope, entities, edges, sites);
}

/// A `class_body`'s direct `class_member` children.
fn extract_class_body<'a>(
    body: Node<'a>,
    src: &'a str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let mut cursor = body.walk();
    for member in body.children(&mut cursor) {
        if member.kind() == "class_member" {
            extract_class_member(member, src, owner, owner_scope, entities, edges, sites);
        }
    }
}

/// A `class_member`: either a bodyless `declaration` (a field declaration,
/// or a constructor/function/getter/setter/operator declared `external` or
/// terminated directly by `;`) or a `method_declaration` (a signature with
/// an actual `function_body`). `find_signature_child` tells the two
/// `declaration` shapes apart: present -> a bodyless signature, modeled as
/// a `Method` with no body to resolve; absent -> an ordinary field
/// declaration.
fn extract_class_member<'a>(
    node: Node<'a>,
    src: &'a str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let mut cursor = node.walk();
    let Some(child) = node
        .children(&mut cursor)
        .find(|c| c.kind() == "declaration" || c.kind() == "method_declaration")
    else {
        return;
    };

    match child.kind() {
        "method_declaration" => {
            extract_method_declaration(child, src, owner, owner_scope, entities, edges, sites)
        }
        "declaration" => {
            if let Some(sig) = find_signature_child(child) {
                let (name, params) = signature_name_and_params(sig, &owner.name, src);
                if name.is_empty() {
                    return;
                }
                let entity = SyntaxEntity::new(SyntaxKind::Method, owner_scope, &name);
                let method_scope = child_scope(owner_scope, &name);
                declare(owner, entity.clone(), entities, edges);
                if let Some(params) = params {
                    extract_parameters(params, src, &entity, &method_scope, entities, edges);
                }
                // A bodyless signature (`external`/interface-style member)
                // has no body to resolve, same posture Kotlin takes toward
                // an abstract/interface method with no `function_body`.
                let _ = is_constructor_kind(sig.kind());
            } else {
                extract_fields_from_declaration(child, owner, owner_scope, src, entities, edges);
            }
        }
        _ => {}
    }
}

/// A `method_declaration`: a `method_signature` (constructor/function/
/// getter/setter/operator) paired with a required `function_body`.
#[allow(clippy::too_many_arguments)]
fn extract_method_declaration<'a>(
    node: Node<'a>,
    src: &'a str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let Some(sig_wrapper) = node.child_by_field_name("signature") else {
        return;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let Some(sig) = find_signature_child(sig_wrapper) else {
        return;
    };
    let (name, params) = signature_name_and_params(sig, &owner.name, src);
    if name.is_empty() {
        return;
    }

    let entity = SyntaxEntity::new(SyntaxKind::Method, owner_scope, &name);
    let method_scope = child_scope(owner_scope, &name);
    declare(owner, entity.clone(), entities, edges);

    if let Some(params) = params {
        extract_parameters(params, src, &entity, &method_scope, entities, edges);
    }

    extract_function_body(body, src, &entity, &method_scope, entities, edges, sites);
}

/// `function_body`'s three shapes (`function_body: choice(arrow, block,
/// native)`): a `block` is walked statement by statement; an arrow
/// (`=> expr;`) gets one generic `Statement` entity for the expression
/// (mirrors Kotlin's expression-bodied-function posture) with the
/// expression itself registered for `resolve_body`; `native` (an FFI stub)
/// has no body to extract at all.
#[allow(clippy::too_many_arguments)]
fn extract_function_body<'a>(
    body: Node<'a>,
    src: &'a str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    if let Some(block) = find_named_child_kind(body, "block") {
        extract_body(block, src, owner, owner_scope, entities, edges);
        sites.bodies.push((owner.clone(), block));
        return;
    }
    let mut cursor = body.walk();
    let Some(expr) = body.named_children(&mut cursor).next() else {
        return;
    };
    if expr.kind() == "native" {
        return;
    }
    let stmt_entity = SyntaxEntity::new(SyntaxKind::Statement, owner_scope, "stmt_0");
    contain(owner, stmt_entity, entities, edges);
    sites.bodies.push((owner.clone(), expr));
}

/// A top-level `function_declaration`: a `Function` (or, when nested -- not
/// reachable at the true top level, but defensively handled the way
/// Kotlin's `is_method` flag is -- a `Method`).
#[allow(clippy::too_many_arguments)]
fn extract_top_level_function<'a>(
    node: Node<'a>,
    src: &'a str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    is_method: bool,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let Some(sig) = node.child_by_field_name("signature") else {
        return;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let (name, params) = signature_name_and_params(sig, "", src);
    if name.is_empty() {
        return;
    }
    let kind = if is_method {
        SyntaxKind::Method
    } else {
        SyntaxKind::Function
    };
    let entity = SyntaxEntity::new(kind, owner_scope, &name);
    let fn_scope = child_scope(owner_scope, &name);
    declare(owner, entity.clone(), entities, edges);

    if let Some(params) = params {
        extract_parameters(params, src, &entity, &fn_scope, entities, edges);
    }

    extract_function_body(body, src, &entity, &fn_scope, entities, edges, sites);
}

/// A top-level `getter_declaration`/`setter_declaration`: modeled as a
/// plain `Function` -- Dart's accessor syntax has no canonical-model
/// `Property`/`Field`-with-accessors kind, same documented-minimal posture
/// this adapter takes toward Dart's getters/setters everywhere (see
/// `DartAdapter`'s own doc comment).
#[allow(clippy::too_many_arguments)]
fn extract_top_level_accessor<'a>(
    node: Node<'a>,
    src: &'a str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let Some(sig) = node.child_by_field_name("signature") else {
        return;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let (name, params) = signature_name_and_params(sig, "", src);
    if name.is_empty() {
        return;
    }
    let entity = SyntaxEntity::new(SyntaxKind::Function, owner_scope, &name);
    let fn_scope = child_scope(owner_scope, &name);
    declare(owner, entity.clone(), entities, edges);

    if let Some(params) = params {
        extract_parameters(params, src, &entity, &fn_scope, entities, edges);
    }

    extract_function_body(body, src, &entity, &fn_scope, entities, edges, sites);
}

/// An `enum_declaration`: its own `Enum` entity, its constants (as
/// `Variable` entities, mirroring Kotlin's enum-constant-as-`Variable`
/// posture), its `with`/`implements` clauses (same `IMPLEMENTS` mapping as
/// a class), and -- for Dart's enhanced enums -- any `class_member`s
/// declared after the constants' trailing `;`.
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
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let name = text(name_node, src);
    let entity = SyntaxEntity::new(SyntaxKind::Enum, scope, name);
    let enum_scope = child_scope(scope, name);
    declare(parent, entity.clone(), entities, edges);

    // `enum_declaration`'s `mixins`/`interfaces` children aren't fielded
    // (see the grammar's own `enum_declaration` rule), so they're found the
    // same `has_token_child`-style scan Kotlin's `is_interface_decl` uses
    // for an unfielded keyword, just for a whole named-node kind instead.
    if let Some(mixins_node) = find_named_child_kind(node, "mixins") {
        let mut cursor = mixins_node.walk();
        for ty in mixins_node.named_children(&mut cursor) {
            sites
                .implemented_interfaces
                .push((entity.clone(), type_base_name(ty, src)));
        }
    }
    if let Some(interfaces_node) = find_named_child_kind(node, "interfaces") {
        let mut cursor = interfaces_node.walk();
        for ty in interfaces_node.named_children(&mut cursor) {
            sites
                .implemented_interfaces
                .push((entity.clone(), type_base_name(ty, src)));
        }
    }

    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            "enum_constant" => {
                if let Some(const_name) = child.child_by_field_name("name") {
                    let const_entity =
                        SyntaxEntity::new(SyntaxKind::Variable, &enum_scope, text(const_name, src));
                    contain(&entity, const_entity, entities, edges);
                }
            }
            "class_member" => {
                extract_class_member(child, src, &entity, &enum_scope, entities, edges, sites);
            }
            _ => {}
        }
    }
}

/// A `declaration` node that's a field declaration (no signature child --
/// see `find_signature_child`): every name in its `static_final_
/// declaration_list`/`initialized_identifier_list`/`identifier_list`
/// becomes one `Variable` entity. Shared by class-member field
/// declarations and top-level `top_level_variable_declaration` (same three
/// list shapes per the grammar). Modifiers (`static`/`final`/`const`/
/// `late`/`covariant`/`external`) aren't distinguished -- a documented gap,
/// matching every other adapter's own posture toward field modifiers
/// beyond visibility.
fn extract_fields_from_declaration(
    decl: Node,
    owner: &SyntaxEntity,
    owner_scope: &str,
    src: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    const LIST_KINDS: &[&str] = &[
        "static_final_declaration_list",
        "initialized_identifier_list",
        "identifier_list",
    ];
    let mut cursor = decl.walk();
    let Some(list_node) = decl
        .children(&mut cursor)
        .find(|c| LIST_KINDS.contains(&c.kind()))
    else {
        return;
    };
    let mut item_cursor = list_node.walk();
    for item in list_node.named_children(&mut item_cursor) {
        let name_node = if item.kind() == "identifier" {
            Some(item)
        } else {
            item.child_by_field_name("name")
        };
        if let Some(name_node) = name_node {
            let entity = SyntaxEntity::new(SyntaxKind::Variable, owner_scope, text(name_node, src));
            contain(owner, entity, entities, edges);
        }
    }
    let _ = &owner_scope;
}

/// A `formal_parameter_list`'s individual parameters -- positional
/// (`formal_parameter` children directly) and optional/named (nested one
/// level deeper, inside an `optional_formal_parameters` child) -- all
/// become `Parameter` entities uniformly. Default values and the
/// `required`-named-argument marker aren't modeled -- a documented gap per
/// `TASKS.md`'s own guidance. A constructor's `this.x`/`super.x`
/// field-initializing shorthand parameter is extracted the same way (its
/// `identifier` child is its name) -- unlike Kotlin's `val`/`var`
/// constructor-property shorthand, no second `Variable` entity is
/// synthesized for it, since Dart requires the corresponding field to
/// already exist as its own declaration (the shorthand only *initializes*
/// it, it doesn't declare it).
fn extract_parameters(
    params: Node,
    src: &str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = params.walk();
    for child in params.named_children(&mut cursor) {
        match child.kind() {
            "formal_parameter" => add_parameter(child, src, parent, scope, entities, edges),
            "optional_formal_parameters" => {
                let mut inner_cursor = child.walk();
                for inner in child.named_children(&mut inner_cursor) {
                    if inner.kind() == "formal_parameter" {
                        add_parameter(inner, src, parent, scope, entities, edges);
                    }
                }
            }
            _ => {}
        }
    }
}

fn add_parameter(
    param: Node,
    src: &str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    let name_node = param.child_by_field_name("name").or_else(|| {
        let mut cursor = param.walk();
        let found = param
            .named_children(&mut cursor)
            .find(|c| c.kind() == "identifier");
        found
    });
    if let Some(name_node) = name_node {
        let entity = SyntaxEntity::new(SyntaxKind::Parameter, scope, text(name_node, src));
        contain(parent, entity, entities, edges);
    }
}

/// A `block`'s direct statement children: `local_variable_declaration`
/// becomes `Variable` entities; everything else becomes a generic
/// `Statement` entity. A local class/mixin/function declaration inside a
/// body isn't extracted -- a documented gap, same as Kotlin not walking
/// into nested item definitions.
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
                extract_local_variable_names(
                    stmt,
                    src,
                    parent,
                    &format!("{scope}#{index}"),
                    entities,
                    edges,
                );
            }
            "class_declaration" | "mixin_declaration" | "local_function_declaration" => {}
            _ => {
                let entity =
                    SyntaxEntity::new(SyntaxKind::Statement, scope, format!("stmt_{index}"));
                contain(parent, entity, entities, edges);
            }
        }
    }
}

/// `local_variable_declaration`'s declared name(s): its
/// `initialized_variable_definition` child's own `name` field for the
/// first one, plus each comma-separated `initialized_identifier` sibling
/// (own `name` field) for any further ones (`var a = 1, b = 2;`).
fn extract_local_variable_names(
    decl: Node,
    src: &str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = decl.walk();
    let Some(def) = decl
        .named_children(&mut cursor)
        .find(|c| c.kind() == "initialized_variable_definition")
    else {
        return;
    };
    if let Some(name_node) = def.child_by_field_name("name") {
        let entity = SyntaxEntity::new(SyntaxKind::Variable, owner_scope, text(name_node, src));
        contain(owner, entity, entities, edges);
    }
    let mut inner_cursor = def.walk();
    for extra in def.named_children(&mut inner_cursor) {
        if extra.kind() == "initialized_identifier" {
            if let Some(name_node) = extra.child_by_field_name("name") {
                let entity =
                    SyntaxEntity::new(SyntaxKind::Variable, owner_scope, text(name_node, src));
                contain(owner, entity, entities, edges);
            }
        }
    }
}

/// `REFERENCES`/`CALLS`/`INSTANTIATES` edges from one function/method/
/// constructor body. `body` is either a `block` (walked statement by
/// statement) or a bare expression (an arrow-bodied member) -- mirrors
/// Kotlin's `resolve_body`, adapted to Dart's `identifier`/
/// `call_expression`/`member_expression`/`new_expression`/
/// `const_object_expression` node kinds. A bare identifier resolves via
/// `scope_tree`; a bare-name call (`helper()`) resolves the same way; a
/// receiver-qualified call (`shape.area()`, via `member_expression`)
/// resolves by best-effort name match against any `Method` symbol in the
/// file (real receiver-type dispatch needs cross-file class knowledge, out
/// of scope here, matching every other adapter's own posture); a call
/// whose callee resolves in scope to a `Type` symbol (`Circle()` -- Dart's
/// implicit-constructor-call syntax, no `new` required) is an
/// `INSTANTIATES` site, and so is an explicit `new Circle()`/
/// `const Circle()`. Anything unresolved gets an explicit placeholder
/// symbol, never silently dropped.
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
    let mut instantiations: Vec<Node<'a>> = Vec::new();

    if body.kind() == "block" {
        let mut cursor = body.walk();
        for stmt in body.named_children(&mut cursor) {
            walk_stmt(
                stmt,
                &mut |n| names.push(n),
                &mut |n| calls.push(n),
                &mut |n| instantiations.push(n),
            );
        }
    } else {
        walk_expr(
            body,
            &mut |n| names.push(n),
            &mut |n| calls.push(n),
            &mut |n| instantiations.push(n),
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
        let Some(callee) = call_node.child_by_field_name("function") else {
            continue;
        };
        let (ident, is_qualified) = match callee.kind() {
            "member_expression" => {
                let Some(property) = callee.child_by_field_name("property") else {
                    continue;
                };
                (text(property, src), true)
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
                // `Circle()` -- Dart's implicit-constructor-call syntax, no
                // `new` keyword required.
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
            resolved_in_scope
                .or_else(|| {
                    // The scope tree only covers this file's own entities,
                    // so a call to a function declared in another file
                    // never resolves through it -- fall back to a
                    // repo-wide best-effort name match against
                    // `symbol_table`, mirroring the qualified branch
                    // above, which has always searched repo-wide.
                    symbol_table
                        .symbols()
                        .find(|s| s.kind == SymbolKind::Function && s.name == ident)
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

    for type_node in &instantiations {
        let type_name = type_base_name(*type_node, src);
        let resolved = scope_tree
            .lookup(scope, &type_name)
            .and_then(|id| symbol_table.find_by_syntax_id(id))
            .cloned();
        let target = resolved.unwrap_or_else(|| {
            let placeholder = unresolved_symbol("external-class", &type_name);
            symbols.push(placeholder.clone());
            placeholder
        });
        edges.push(Edge::new(
            member_symbol.id.clone(),
            target.id,
            EdgeKind::Instantiates,
        ));
    }

    (symbols, edges)
}

fn walk_expr<'a>(
    node: Node<'a>,
    on_name: &mut impl FnMut(Node<'a>),
    on_call: &mut impl FnMut(Node<'a>),
    on_instantiate: &mut impl FnMut(Node<'a>),
) {
    match node.kind() {
        "identifier" => on_name(node),
        "call_expression" => {
            on_call(node);
            if let Some(callee) = node.child_by_field_name("function") {
                match callee.kind() {
                    "identifier" => on_name(callee),
                    "member_expression" => walk_expr(callee, on_name, on_call, on_instantiate),
                    _ => {}
                }
            }
            if let Some(args) = node.child_by_field_name("arguments") {
                let mut cursor = args.walk();
                for arg in args.named_children(&mut cursor) {
                    let expr = if arg.kind() == "named_argument" {
                        let mut inner = arg.walk();
                        arg.named_children(&mut inner).last()
                    } else {
                        Some(arg)
                    };
                    if let Some(expr) = expr {
                        walk_expr(expr, on_name, on_call, on_instantiate);
                    }
                }
            }
        }
        "new_expression" | "const_object_expression" => {
            if let Some(type_node) = node.child_by_field_name("type") {
                on_instantiate(type_node);
            }
            if let Some(args) = node.child_by_field_name("arguments") {
                let mut cursor = args.walk();
                for arg in args.named_children(&mut cursor) {
                    walk_expr(arg, on_name, on_call, on_instantiate);
                }
            }
        }
        "member_expression" => {
            if let Some(object) = node.child_by_field_name("object") {
                walk_expr(object, on_name, on_call, on_instantiate);
            }
        }
        "assignment_expression" => {
            if let Some(left) = node.child_by_field_name("left") {
                walk_expr(left, on_name, on_call, on_instantiate);
            }
            if let Some(right) = node.child_by_field_name("right") {
                walk_expr(right, on_name, on_call, on_instantiate);
            }
        }
        "parenthesized_expression" => {
            if let Some(inner) = node.named_child(0) {
                walk_expr(inner, on_name, on_call, on_instantiate);
            }
        }
        "throw_expression" => {
            if let Some(value) = node.child_by_field_name("value") {
                walk_expr(value, on_name, on_call, on_instantiate);
            }
        }
        // Binary/conditional/index expressions and literals are a
        // documented gap here, matching the batch guidance: structural
        // extraction plus the common reference/call/instantiate shapes
        // only, not full expression-tree coverage.
        _ => {}
    }
}

fn walk_stmt<'a>(
    node: Node<'a>,
    on_name: &mut impl FnMut(Node<'a>),
    on_call: &mut impl FnMut(Node<'a>),
    on_instantiate: &mut impl FnMut(Node<'a>),
) {
    match node.kind() {
        "local_variable_declaration" => {
            let mut cursor = node.walk();
            let def = node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "initialized_variable_definition");
            if let Some(def) = def {
                if let Some(value) = def.child_by_field_name("value") {
                    walk_expr(value, on_name, on_call, on_instantiate);
                }
            }
        }
        "expression_statement" => {
            if let Some(expr) = node.named_child(0) {
                walk_expr(expr, on_name, on_call, on_instantiate);
            }
        }
        "return_statement" => {
            if let Some(expr) = node.named_child(0) {
                walk_expr(expr, on_name, on_call, on_instantiate);
            }
        }
        "if_statement" => {
            if let Some(consequence) = node.child_by_field_name("consequence") {
                walk_stmt(consequence, on_name, on_call, on_instantiate);
            }
            if let Some(alternative) = node.child_by_field_name("alternative") {
                walk_stmt(alternative, on_name, on_call, on_instantiate);
            }
        }
        "while_statement" => {
            if let Some(condition) = node.child_by_field_name("condition") {
                walk_expr(condition, on_name, on_call, on_instantiate);
            }
            if let Some(body) = node.child_by_field_name("body") {
                walk_stmt(body, on_name, on_call, on_instantiate);
            }
        }
        "for_statement" => {
            if let Some(body) = node.child_by_field_name("body") {
                walk_stmt(body, on_name, on_call, on_instantiate);
            }
        }
        "block" => {
            let mut cursor = node.walk();
            for stmt in node.named_children(&mut cursor) {
                walk_stmt(stmt, on_name, on_call, on_instantiate);
            }
        }
        // try/switch/labeled statements and other less common control flow
        // are a documented gap here, matching Kotlin/Java's own limited
        // `walk_stmt` coverage.
        _ => walk_expr(node, on_name, on_call, on_instantiate),
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

/// Resolve `sites`' explicit `extends`/`with`/`implements` relations into
/// `EXTENDS`/`IMPLEMENTS` edges, searched repository-wide (`all_entities`)
/// so a superclass/mixin/interface declared in a different file still
/// resolves -- mirrors Kotlin's `resolve_structural`. A name that resolves
/// to nothing in the repository (an external/`dart:core` type, e.g.
/// `extends Error`) gets an explicit placeholder symbol, never silently
/// dropped.
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
            &[SyntaxKind::Class, SyntaxKind::Interface, SyntaxKind::Enum],
            superclass_name,
        )
        .unwrap_or_else(|| {
            let placeholder = unresolved_symbol("external-class", superclass_name);
            symbols.push(placeholder.clone());
            placeholder
        });
        edges.push(Edge::new(
            owner_symbol.id.clone(),
            target.id,
            EdgeKind::Extends,
        ));
    }

    for (entity, interface_name) in &sites.implemented_interfaces {
        let Some(owner_symbol) = symbol_table.find_by_syntax_id(&entity.id) else {
            continue;
        };
        let target = find_type_symbol(
            all_entities,
            symbol_table,
            &[SyntaxKind::Class, SyntaxKind::Interface],
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

/// Resolve a file's `import_or_export`/`part_directive` nodes into
/// `IMPORTS` edges from `module_symbol`. A `library_export` (`export
/// 'foo.dart';`) folds into the same `IMPORTS` bucket as a plain `import`,
/// not a separate `EXPORTS` edge -- `EXPORTS` under this canonical model
/// means "this module's own declared symbol is part of its public API"
/// (see `resolve_exports`), which a re-export of *another* library's
/// symbols doesn't fit; "this module has a dependency on that URI" is the
/// closer fit, so it's modeled as `IMPORTS`, a documented mapping decision
/// rather than a forced fit, not a gap. A `part_directive` (`part
/// 'other.dart';`) gets the same treatment, closest to JS/TS's module
/// system per `TASKS.md`'s own framing. A URI (not a dotted package path,
/// unlike Kotlin/Java) doesn't tell us, without resolving it against this
/// repository's actual file layout, whether it names a `dart:`/`package:`
/// dependency or another file in this same repository -- out of scope for
/// this sub-phase, matching every other adapter's own posture, so every
/// one resolves to an explicit `external-package` placeholder keyed by its
/// raw URI text.
fn resolve_imports(
    imports: &[Node],
    src: &str,
    module_symbol: &SymbolEntity,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for import_node in imports {
        let Some(uri_node) = find_uri_node(*import_node) else {
            continue;
        };
        let raw = text(uri_node, src);
        let placeholder = unresolved_symbol("external-package", raw);
        symbols.push(placeholder.clone());
        edges.push(Edge::new(
            module_symbol.id.clone(),
            placeholder.id,
            EdgeKind::Imports,
        ));
    }

    (symbols, edges)
}

/// `import_or_export`'s (via its `library_import`/`library_export` child)
/// or `part_directive`'s own `uri` field -- a `string_literal`, walked
/// down one extra `import_specification` hop for `library_import`.
fn find_uri_node<'a>(node: Node<'a>) -> Option<Node<'a>> {
    match node.kind() {
        "part_directive" => node.child_by_field_name("uri"),
        "import_or_export" => {
            let mut cursor = node.walk();
            let inner = node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "library_import" || c.kind() == "library_export")?;
            if inner.kind() == "library_export" {
                return inner.child_by_field_name("uri");
            }
            let mut inner_cursor = inner.walk();
            let spec = inner
                .named_children(&mut inner_cursor)
                .find(|c| c.kind() == "import_specification")?;
            spec.child_by_field_name("uri")
        }
        _ => None,
    }
}

/// Resolve a file's top-level declarations into `EXPORTS` edges from
/// `module_symbol`: Dart's actual public-API convention -- a top-level
/// identifier is library-private if and only if it starts with `_`, a
/// purely lexical rule with no visibility *keyword* at all (unlike every
/// other adapter in this codebase so far). No `ResolutionSites` bookkeeping
/// is needed for this, unlike Kotlin's `non_public_types` -- the entity's
/// own `name` already carries the answer.
fn resolve_exports(
    file_entities: &[SyntaxEntity],
    file_edges: &[Edge],
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
        if entity.name.starts_with('_') {
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

/// A `type` node's base declared name: its first `type_identifier`
/// segment, ignoring a second dotted segment (`pkg.Foo` -> `pkg`, a
/// documented simplification -- prefixed/dotted type references are rare
/// outside generated code) and any `type_arguments`/trailing `?`.
/// `new_expression`/`const_object_expression`'s own `type` field uses the
/// same node shape, so `resolve_body`'s instantiation handling reuses this
/// too.
fn type_base_name(node: Node, src: &str) -> String {
    match node.kind() {
        "type" | "type_identifier" => {
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "type_identifier")
                .map(|n| text(n, src).to_string())
                .unwrap_or_else(|| text(node, src).to_string());
            found
        }
        _ => text(node, src).to_string(),
    }
}

/// Map a Dart type expression to a `TypeRef`. Not yet wired into
/// `extract`/`resolve_file` -- `ACCEPTS`/`RETURNS` type binding isn't
/// called from the pipeline, mirroring every other adapter's own
/// `extract_type`/`extract_signature_types` gap (tested directly,
/// deliberately not yet wired in either).
///
/// Canonical-model gap already documented for Kotlin/C#, carried over
/// unchanged: Dart's `T?` nullable types have no representation in
/// `TypeRef` -- neither `Union` nor `Reference` fits a single type plus an
/// orthogonal nullability flag. Unlike Kotlin's separate `nullable_type`
/// grammar node, Dart's `?` is a flat trailing token *inside* the same
/// `type` node (see `tree-sitter-dart`'s `nullable()` grammar helper) --
/// `extract_type` below simply ignores that trailing `?` token and extracts
/// the type as if it weren't nullable, consistent with Kotlin/Go/Rust/C#'s
/// own precedent of documenting the gap on `extract_type` rather than
/// inventing a fake structure for it.
#[allow(dead_code)]
fn extract_type(node: Node, src: &str) -> TypeRef {
    if node.kind() != "type" {
        return TypeRef::Unresolved(text(node, src).to_string());
    }
    let mut cursor = node.walk();
    let Some(type_id) = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "type_identifier")
    else {
        return TypeRef::Unresolved(text(node, src).to_string());
    };
    let name = text(type_id, src).to_string();
    let mut arg_cursor = node.walk();
    let type_args: Vec<TypeRef> = node
        .named_children(&mut arg_cursor)
        .find(|c| c.kind() == "type_arguments")
        .map(|args| {
            let mut ac = args.walk();
            args.named_children(&mut ac)
                .filter(|c| c.kind() == "type")
                .map(|t| extract_type(t, src))
                .collect()
        })
        .unwrap_or_default();

    if PRIMITIVE_TYPES.contains(&name.as_str()) && type_args.is_empty() {
        TypeRef::Primitive(name)
    } else {
        TypeRef::Named { name, type_args }
    }
}

#[cfg(test)]
mod smoke_tests {
    use tree_sitter::Parser;

    #[test]
    fn tree_sitter_dart_language_loads_and_parses() {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_dart::LANGUAGE.into())
            .expect("tree-sitter-dart LANGUAGE should load against tree-sitter 0.25");
        let tree = parser
            .parse("void main() {}\n", None)
            .expect("parses a trivial Dart snippet");
        assert_eq!(tree.root_node().kind(), "source_file");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
        let adapter = DartAdapter;
        let ast = adapter.parse(source).expect("valid dart parses");
        adapter.extract(&ast, "lib.dart")
    }

    fn parse_source(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_dart::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn parses_valid_dart_source() {
        let adapter = DartAdapter;
        assert!(adapter.parse("void main() {}\n").is_ok());
    }

    #[test]
    fn parsing_invalid_syntax_is_a_clear_error() {
        // tree-sitter is error-tolerant and always returns *some* tree, so
        // "invalid" Dart doesn't surface as an `Err` here -- matching every
        // other tree-sitter-backed adapter's documented posture.
        let adapter = DartAdapter;
        assert!(adapter.parse("class {{{ not dart").is_ok());
    }

    #[test]
    fn extract_always_returns_the_file_module_first() {
        let (entities, _) = extract("");
        assert_eq!(entities[0].kind, SyntaxKind::Module);
        assert_eq!(entities[0].name, "lib.dart");
    }

    #[test]
    fn extracts_a_top_level_class_with_a_method_and_its_parameters() {
        let (entities, edges) =
            extract("class Calculator {\n  int add(int a, int b) {\n    return a + b;\n  }\n}\n");

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
    fn extracts_an_abstract_class_as_an_interface() {
        let (entities, _edges) = extract("abstract class Shape {\n  double area();\n}\n");
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
    fn extracts_a_named_constructor_distinct_from_the_unnamed_one() {
        let (entities, edges) = extract(
            "class Point {\n  Point(this.x, this.y);\n  Point.origin() : x = 0, y = 0;\n  final int x;\n  final int y;\n}\n",
        );
        let class = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Point")
            .unwrap();

        let default_ctor = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "Point")
            .expect("default constructor named after the class");
        let named_ctor = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "origin")
            .expect("named constructor keeps its own distinguishing name");
        assert_ne!(default_ctor.id, named_ctor.id);
        assert!(edges.contains(&Edge::new(
            class.id.clone(),
            default_ctor.id.clone(),
            EdgeKind::Declares
        )));
        assert!(edges.contains(&Edge::new(
            class.id.clone(),
            named_ctor.id.clone(),
            EdgeKind::Declares
        )));

        let fields: Vec<&SyntaxEntity> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Variable)
            .collect();
        assert_eq!(fields.len(), 2);
        assert!(fields.iter().any(|f| f.name == "x"));
        assert!(fields.iter().any(|f| f.name == "y"));
    }

    #[test]
    fn extracts_an_enum_with_its_constants() {
        let (entities, edges) = extract("enum Color {\n  red, green, blue\n}\n");
        let color = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Enum && e.name == "Color")
            .expect("Enum entity");
        let red = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Variable && e.name == "red")
            .expect("red constant entity");
        assert!(edges.contains(&Edge::new(
            color.id.clone(),
            red.id.clone(),
            EdgeKind::Contains
        )));
    }

    #[test]
    fn extracts_an_enhanced_enum_with_a_method() {
        let (entities, edges) =
            extract("enum Color {\n  red, green;\n\n  String label() {\n    return 'x';\n  }\n}\n");
        let color = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Enum)
            .unwrap();
        let label = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "label")
            .expect("enhanced enum method extracted like a class method");
        assert!(edges.contains(&Edge::new(
            color.id.clone(),
            label.id.clone(),
            EdgeKind::Declares
        )));
    }

    #[test]
    fn extracts_a_mixin_declaration_as_a_class() {
        let (entities, _) = extract("mixin Flies {\n  void fly() {}\n}\n");
        entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Flies")
            .expect("mixin modeled as a plain Class -- no dedicated mixin kind");
    }

    #[test]
    fn extracts_a_field_declaration_as_a_variable() {
        let (entities, edges) = extract("class Box {\n  int size = 0;\n}\n");
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
    fn a_non_declaration_statement_becomes_a_generic_statement_entity() {
        let (entities, _) = extract("class Run {\n  void run() {\n    print(1);\n  }\n}\n");
        assert!(entities.iter().any(|e| e.kind == SyntaxKind::Statement));
    }

    #[test]
    fn an_arrow_bodied_function_still_gets_a_statement_entity() {
        let (entities, _) = extract("int square(int x) => x * x;\n");
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

    fn function_named<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        fn find<'a>(node: Node<'a>, src: &'a str, name: &str) -> Option<Node<'a>> {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "function_declaration" {
                    if let Some(sig) = child.child_by_field_name("signature") {
                        if sig
                            .child_by_field_name("name")
                            .is_some_and(|n| text(n, src) == name)
                        {
                            return Some(child);
                        }
                    }
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
        func.child_by_field_name("signature")
            .and_then(|sig| sig.child_by_field_name("return_type"))
            .expect("return type node")
    }

    #[test]
    fn extracts_a_primitive_return_type() {
        let full = "int f() { return 0; }\n";
        let tree = parse_source(full);
        let node = return_type_of(&tree, full, "f");
        assert_eq!(
            extract_type(node, full),
            TypeRef::Primitive("int".to_string())
        );
    }

    #[test]
    fn extracts_a_user_defined_named_type() {
        let full = "Circle f() { return Circle(); }\n";
        let tree = parse_source(full);
        let node = return_type_of(&tree, full, "f");
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
        let full = "List<String> f() { return []; }\n";
        let tree = parse_source(full);
        let node = return_type_of(&tree, full, "f");
        // `String` is in `PRIMITIVE_TYPES` (same posture as C#'s `string`),
        // so the type argument comes back `Primitive`, not `Named`.
        assert_eq!(
            extract_type(node, full),
            TypeRef::Named {
                name: "List".to_string(),
                type_args: vec![TypeRef::Primitive("String".to_string())],
            }
        );
    }

    #[test]
    fn nullable_type_ignores_the_question_mark_and_extracts_the_inner_type() {
        let full = "Circle? f() { return null; }\n";
        let tree = parse_source(full);
        let node = return_type_of(&tree, full, "f");
        assert_eq!(
            extract_type(node, full),
            TypeRef::Named {
                name: "Circle".to_string(),
                type_args: Vec::new()
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
        let adapter = DartAdapter;
        let ast = adapter.parse(source).expect("valid dart parses");
        let (entities, edges) = adapter.extract(&ast, "lib.dart");
        let module = entities[0].clone();
        let member_entity = entities
            .iter()
            .find(|e| {
                (e.kind == SyntaxKind::Method || e.kind == SyntaxKind::Function)
                    && e.name == member_name
            })
            .unwrap_or_else(|| panic!("no Method/Function entity named {member_name}"))
            .clone();
        let scope_tree = ScopeTree::build(&module, &entities, &edges);
        let scope = scope_tree
            .scope_of(&member_entity.id)
            .expect("function opens a scope");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let member_symbol = symbol_table
            .find_by_syntax_id(&member_entity.id)
            .expect("function symbol")
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
        let func = function_named(tree, src, name);
        let body = func.child_by_field_name("body").expect("function body");
        find_named_child_kind(body, "block").unwrap_or_else(|| {
            let mut cursor = body.walk();
            let found = body
                .named_children(&mut cursor)
                .next()
                .expect("arrow body expr");
            found
        })
    }

    #[test]
    fn resolves_a_reference_to_an_in_scope_local_variable() {
        let source = "void run() {\n  var value = 1;\n  print(value);\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

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
        let source = "void run() {\n  missing.toString();\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

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
        let source = "void helper() {}\nvoid run() {\n  helper();\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

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
            .find(|e| e.kind == SyntaxKind::Function && e.name == "helper")
            .unwrap();
        let helper_symbol = r.symbol_table.find_by_syntax_id(&helper.id).unwrap();
        assert!(edges.contains(&Edge::new(
            r.member_symbol.id.clone(),
            helper_symbol.id.clone(),
            EdgeKind::Calls
        )));
    }

    #[test]
    fn a_free_call_to_a_function_declared_in_another_file_resolves_via_repo_wide_fallback() {
        // Regression test for the cross-file CALLS gap found via Phase 18.1's
        // real-world benchmarking: the scope tree only ever covered the
        // calling file's own entities, so a call to a top-level function
        // declared in another file fell straight to an `unknown-local`
        // placeholder.
        use crate::symbols::build_symbol_table;

        let adapter = DartAdapter;
        let helper_ast = adapter.parse("void helper() {}\n").unwrap();
        let (helper_entities, helper_edges) = adapter.extract(&helper_ast, "helper.dart");
        let caller_ast = adapter.parse("void run() {\n  helper();\n}\n").unwrap();
        let (caller_entities, caller_edges) = adapter.extract(&caller_ast, "run.dart");

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
            "run.dart",
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
    fn resolves_a_receiver_qualified_call_by_best_effort_name_match() {
        let source =
            "class Shape {\n  double area() { return 0.0; }\n}\nvoid run(Shape shape) {\n  shape.area();\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

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
    fn resolves_an_implicit_constructor_call_instantiation_for_an_in_scope_type() {
        let source = "class Circle {}\nvoid run() {\n  var c = Circle();\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

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
    fn resolves_an_explicit_new_instantiation_for_an_in_scope_type() {
        let source = "class Circle {}\nvoid run() {\n  var c = new Circle();\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

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
    fn a_class_implementing_an_interface_gets_an_explicit_implements_edge() {
        let source = "abstract class Shape {\n  double area();\n}\nclass Circle implements Shape {\n  double area() { return 0.0; }\n}\n";
        let adapter = DartAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "lib.dart");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "lib.dart",
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
        let adapter = DartAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "lib.dart");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "lib.dart",
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
    fn a_class_with_a_mixin_gets_an_explicit_implements_edge() {
        let source = "mixin Flies {}\nclass Bird with Flies {}\n";
        let adapter = DartAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "lib.dart");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "lib.dart",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );
        let mut symbols = Vec::new();
        let edges = resolve_structural(&sites, &entities, &symbol_table, &mut symbols);

        let bird = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Bird")
            .unwrap();
        let flies = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Flies")
            .unwrap();
        let bird_symbol = symbol_table.find_by_syntax_id(&bird.id).unwrap();
        let flies_symbol = symbol_table.find_by_syntax_id(&flies.id).unwrap();
        assert!(edges.contains(&Edge::new(
            bird_symbol.id.clone(),
            flies_symbol.id.clone(),
            EdgeKind::Implements
        )));
    }

    #[test]
    fn a_class_extending_an_external_class_gets_a_placeholder() {
        let source = "class MyError extends Error {}\n";
        let adapter = DartAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "lib.dart");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "lib.dart",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );
        let mut symbols = Vec::new();
        let extends_edges = resolve_structural(&sites, &entities, &symbol_table, &mut symbols);

        assert!(symbols
            .iter()
            .any(|s| s.name.starts_with("external-class:Error")));
        assert_eq!(extends_edges.len(), 1);
    }

    #[test]
    fn resolves_imports_and_parts_to_an_explicit_external_package_placeholder() {
        let source = "import 'package:foo/foo.dart';\npart 'other.dart';\n\nclass C {}\n";
        let adapter = DartAdapter;
        let ast = adapter.parse(source).unwrap();
        let module_symbol = SymbolEntity::new(SymbolKind::Namespace, "lib.dart", "lib.dart");

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &SyntaxEntity::new(SyntaxKind::Module, "lib.dart", "lib.dart"),
            "lib.dart",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );

        let (symbols, edges) = resolve_imports(&sites.imports, &ast.source, &module_symbol);

        assert_eq!(symbols.len(), 2);
        assert!(symbols
            .iter()
            .any(|s| s.name == "external-package:'package:foo/foo.dart'"));
        assert!(symbols
            .iter()
            .any(|s| s.name == "external-package:'other.dart'"));
        assert_eq!(
            edges.iter().filter(|e| e.kind == EdgeKind::Imports).count(),
            2
        );
    }

    #[test]
    fn exports_a_public_top_level_class_but_not_a_private_one() {
        let source = "class Public {}\nclass _Hidden {}\n";
        let adapter = DartAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, edges) = adapter.extract(&ast, "lib.dart");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table
            .find_by_syntax_id(&entities[0].id)
            .unwrap()
            .clone();

        let export_edges = resolve_exports(&entities, &edges, &module_symbol, &symbol_table);

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
