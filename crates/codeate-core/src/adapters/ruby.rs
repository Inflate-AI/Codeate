use tree_sitter::{Node, Parser, Tree};

use crate::adapter::LanguageAdapter;
use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};
use crate::resolve::unresolved_symbol;
use crate::symbols::{ScopeTree, SymbolTable};

/// The parsed form a `RubyAdapter` produces: `tree-sitter`'s `Tree` paired
/// with the source text it was parsed from, the same shape every other
/// `tree-sitter`-backed adapter (Go/Java/TypeScript/JavaScript/C#/Kotlin/C)
/// uses.
pub struct RubyAst {
    tree: Tree,
    source: String,
}

/// The Ruby language adapter: parses Ruby source via `tree-sitter`/
/// `tree-sitter-ruby` and maps it onto the canonical syntax model.
///
/// Structurally this adapter is closer to the Python adapter (15) than to
/// any Tier 1 language done so far: Ruby is dynamically typed with no static
/// type syntax at all, so unlike Kotlin/C#/Java this adapter never produces
/// a resolved `TypeRef` (see the module-level note on `extract_type`'s
/// absence, below) and every reference/call resolution is name-based/
/// best-effort, carrying the same caveat Python's duck typing already does.
///
/// Modeling decisions specific to Ruby's grammar, each a deliberate choice
/// documented here rather than a silent gap:
///
/// - `class` and `module` both map onto `SyntaxKind::Class`. The canonical
///   model has no separate "namespace-only"/"mixin-only" syntax kind, and a
///   Ruby `module` is structurally just a class that can't be instantiated
///   (methods, constants, nested classes/modules all declare the same way)
///   -- the same posture Kotlin's adapter takes mapping `object` onto
///   `Class` rather than inventing a singleton-specific kind.
/// - `include`/`extend`/`prepend` (mixin composition) are read directly off
///   each class/module body's own top-level calls (mirroring how Kotlin's
///   `resolve_structural` reads `: SuperType` directly off the grammar
///   rather than inferring it) and resolved into `IMPLEMENTS` edges from the
///   including class/module to the named module -- the module is granting
///   its own methods to the includer the way an interface's contract would,
///   which is the closest existing edge kind, per this batch's guidance.
/// - `attr_accessor`/`attr_reader`/`attr_writer` calls synthesize one
///   `Variable` entity per named attribute, contained in (not declared by)
///   the owning class -- the canonical model has no dedicated `Field`
///   `SyntaxKind` at all (every other adapter's own "field" is a `Variable`
///   too, e.g. Kotlin's constructor-property shorthand), so this reuses that
///   same bucket. The *generated* reader/writer `Method`s themselves are
///   not synthesized as separate entities -- a documented gap, matching how
///   Kotlin's `data class` doesn't get synthesized `equals`/`hashCode`.
/// - Blocks/procs/lambdas (`{ ... }`, `do ... end`, `->`, `lambda`/`proc`)
///   are out of scope: a call's attached block is never walked into for
///   reference/call resolution, and `lambda`/`->` literals aren't extracted
///   as entities at all. This is a documented gap, not a guess -- blocks
///   introduce their own parameter scope and (for `do...end`) can carry
///   arbitrary statements, which would need a scope-tree extension beyond
///   what this sub-phase's guidance calls for.
/// - Instance variables (`@x`) and class variables (`@@x`) are extracted as
///   `Variable` entities exactly where Python's adapter extracts local
///   assignments: per assignment-site, inside whatever body (method/
///   top-level) they're assigned in, not hoisted to a class-level
///   declaration list the way Kotlin's constructor-property shorthand is.
///   Ruby has no syntax that *declares* an instance variable independently
///   of assigning it, so there is no single canonical declaration site to
///   hoist to -- a documented modeling choice.
/// - Open classes/monkey-patching (reopening `class Foo` in a second
///   `class Foo ... end` block) need no special handling: `extract_class`
///   runs again and declares a second `Class` entity with the same derived
///   `EntityId` (same kind/location/name) as the first -- entity/edge
///   dedup happens the same way any other redeclaration does elsewhere in
///   this codebase, not something this adapter needs to detect itself.
pub struct RubyAdapter;

impl LanguageAdapter for RubyAdapter {
    type Ast = RubyAst;

    fn parse(&self, source: &str) -> Result<Self::Ast> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_ruby::LANGUAGE.into())
            .map_err(|err| Error::Message(format!("failed to load Ruby grammar: {err}")))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::Message("failed to parse Ruby source".to_string()))?;
        Ok(RubyAst {
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

        edges.extend(resolve_structural(
            &sites,
            all_entities,
            symbol_table,
            &mut symbols,
        ));

        let (require_symbols, require_edges) =
            resolve_requires(&sites.requires, &ast.source, module_symbol);
        symbols.extend(require_symbols);
        edges.extend(require_edges);

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
/// directly off explicit Ruby syntax -- mirrors Kotlin's own
/// `ResolutionSites`.
#[derive(Default)]
struct ResolutionSites<'a> {
    /// A method/singleton-method/top-level `def`'s own entity paired with
    /// its body node -- a `body_statement` for a normal method, or the bare
    /// expression node itself for an endless method (`def square(x) = x *
    /// x`); `resolve_body` handles both shapes.
    bodies: Vec<(SyntaxEntity, Node<'a>)>,
    /// A class/module's own entity paired with its superclass name, read
    /// from `class Foo < Bar`'s own `superclass` field (modules have no
    /// such field -- Ruby itself has no "module inheritance").
    superclasses: Vec<(SyntaxEntity, String)>,
    /// A class/module's own entity paired with one mixed-in module name,
    /// read from a top-level `include`/`extend`/`prepend` call in its body.
    mixins: Vec<(SyntaxEntity, String)>,
    /// Every `require`/`require_relative` call node at any extracted scope.
    requires: Vec<Node<'a>>,
}

fn text<'a>(node: Node<'_>, src: &'a str) -> &'a str {
    node.utf8_text(src.as_bytes()).unwrap_or("")
}

fn child_scope(scope: &str, name: &str) -> String {
    format!("{scope}::{name}")
}

/// Record `parent CONTAINS child` and `parent DECLARES child`, then push
/// `child` into `entities` -- classes/modules/methods.
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

/// Record `parent CONTAINS child` only -- attributes, parameters, local/
/// instance/class variables, statements.
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

/// A `class`/`module`'s `name` field: `constant` is a bare name; a
/// `scope_resolution` (`Foo::Bar`) is named by its own `name` field (the
/// final segment), mirroring Kotlin's `type_base_name` dotted-segment
/// handling.
fn decl_name_text<'a>(node: Node<'_>, src: &'a str) -> &'a str {
    match node.kind() {
        "scope_resolution" => node
            .child_by_field_name("name")
            .map(|n| text(n, src))
            .unwrap_or_else(|| text(node, src)),
        _ => text(node, src),
    }
}

/// Walk a `program`'s or a `body_statement`'s direct children, dispatching
/// top-level declarations. `class`/`module`/`method`/`singleton_method` are
/// handled here directly; a bare `call` is inspected for
/// `include`/`extend`/`prepend` (recorded into `sites.mixins`) and
/// `require`/`require_relative` (recorded into `sites.requires`) --
/// everything else at this level is not extracted, matching Python's own
/// "only def/class matter at item level" stance.
#[allow(clippy::too_many_arguments)]
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
        extract_member(child, src, parent, scope, entities, edges, sites);
    }
}

#[allow(clippy::too_many_arguments)]
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
        "class" => extract_class(node, src, owner, owner_scope, entities, edges, sites),
        "module" => extract_module(node, src, owner, owner_scope, entities, edges, sites),
        "method" => {
            let is_method = owner.kind != SyntaxKind::Module;
            extract_method(node, src, owner, owner_scope, is_method, entities, edges, sites);
        }
        "singleton_method" => {
            extract_singleton_method(node, src, owner, owner_scope, entities, edges, sites);
        }
        "call" => extract_top_level_call(node, src, owner, owner_scope, entities, edges, sites),
        "assignment" => {
            extract_class_level_assignment(node, src, owner, owner_scope, entities, edges);
        }
        _ => {}
    }
}

/// A `call` with no receiver, inspected for the handful of call-syntax
/// "declarations" Ruby has no dedicated keyword for:
/// `attr_accessor`/`attr_reader`/`attr_writer` (synthesize `Variable`
/// entities, one per named attribute), `include`/`extend`/`prepend`
/// (recorded into `sites.mixins`), and `require`/`require_relative`
/// (recorded into `sites.requires`). Any other bare call at this level
/// (e.g. a macro-style class-body call from a DSL) is not extracted, a
/// documented gap -- there is no general way to know such a call declares
/// anything without evaluating it.
fn extract_top_level_call<'a>(
    node: Node<'a>,
    src: &'a str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    if node.child_by_field_name("receiver").is_some() {
        return;
    }
    let Some(method_node) = node.child_by_field_name("method") else {
        return;
    };
    let method_name = text(method_node, src);
    let Some(args) = node.child_by_field_name("arguments") else {
        return;
    };

    match method_name {
        "attr_accessor" | "attr_reader" | "attr_writer" => {
            let mut cursor = args.walk();
            for arg in args.named_children(&mut cursor) {
                let attr_name = symbol_or_string_text(arg, src);
                if let Some(attr_name) = attr_name {
                    let entity = SyntaxEntity::new(SyntaxKind::Variable, owner_scope, attr_name);
                    contain(owner, entity, entities, edges);
                }
            }
        }
        "include" | "extend" | "prepend" => {
            let mut cursor = args.walk();
            for arg in args.named_children(&mut cursor) {
                sites
                    .mixins
                    .push((owner.clone(), decl_name_text(arg, src).to_string()));
            }
        }
        "require" | "require_relative" => {
            sites.requires.push(node);
        }
        _ => {}
    }
}

/// A `simple_symbol` (`:name`, text `":name"`) or a plain `string`'s
/// `attr_accessor`-style argument text, with its leading `:`/quotes
/// stripped. `None` for any other argument shape (an interpolated symbol,
/// a splat, ...) -- a documented gap, not guessed at.
fn symbol_or_string_text<'a>(node: Node<'_>, src: &'a str) -> Option<&'a str> {
    match node.kind() {
        "simple_symbol" => Some(text(node, src).trim_start_matches(':')),
        "string" => {
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "string_content")
                .map(|c| text(c, src));
            found
        }
        _ => None,
    }
}

/// A class-body-level `x = value`/`@x = value`/`CONST = value` assignment:
/// becomes a `Variable` entity named after the assignment target text
/// (`@x`, `@@x`, `CONST`, ... -- sigils kept as-is so a later reference to
/// the same name resolves against the identical binding text). A
/// destructuring or non-simple left-hand side is left to the generic
/// statement handling a method body would give it -- not extracted here,
/// since this function is only reached for class/module body top-level
/// assignments (constants, class-level instance/class variables).
fn extract_class_level_assignment(
    node: Node,
    src: &str,
    owner: &SyntaxEntity,
    owner_scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    let Some(left) = node.child_by_field_name("left") else {
        return;
    };
    if !is_simple_binding_target(left) {
        return;
    }
    let entity = SyntaxEntity::new(SyntaxKind::Variable, owner_scope, text(left, src));
    contain(owner, entity, entities, edges);
}

fn is_simple_binding_target(node: Node) -> bool {
    matches!(
        node.kind(),
        "identifier" | "instance_variable" | "class_variable" | "global_variable" | "constant"
    )
}

/// A `class` declaration: its own `Class` entity (see the module-level note
/// on why `module` shares this kind), its `superclass` field (if present,
/// recorded into `sites.superclasses`), and its body's members.
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
    let name = decl_name_text(name_node, src);
    let entity = SyntaxEntity::new(SyntaxKind::Class, scope, name);
    let class_scope = child_scope(scope, name);
    declare(parent, entity.clone(), entities, edges);

    if let Some(superclass_node) = node.child_by_field_name("superclass") {
        if let Some(expr) = superclass_node.named_child(0) {
            sites
                .superclasses
                .push((entity.clone(), decl_name_text(expr, src).to_string()));
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for member in body.named_children(&mut cursor) {
            extract_member(member, src, &entity, &class_scope, entities, edges, sites);
        }
    }
}

/// A `module` declaration: modeled exactly like `extract_class`, minus the
/// (nonexistent) superclass handling.
#[allow(clippy::too_many_arguments)]
fn extract_module<'a>(
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
    let name = decl_name_text(name_node, src);
    let entity = SyntaxEntity::new(SyntaxKind::Class, scope, name);
    let module_scope = child_scope(scope, name);
    declare(parent, entity.clone(), entities, edges);

    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for member in body.named_children(&mut cursor) {
            extract_member(member, src, &entity, &module_scope, entities, edges, sites);
        }
    }
}

/// A `method` (`def name ... end`): a top-level `Function` or a class/
/// module member `Method` (`is_method` distinguishes the two, mirroring
/// Kotlin's `function_declaration` handling).
#[allow(clippy::too_many_arguments)]
fn extract_method<'a>(
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
    let kind = if is_method {
        SyntaxKind::Method
    } else {
        SyntaxKind::Function
    };
    let entity = SyntaxEntity::new(kind, owner_scope, name);
    let fn_scope = child_scope(owner_scope, name);
    declare(owner, entity.clone(), entities, edges);

    if let Some(params) = node.child_by_field_name("parameters") {
        extract_parameters(params, src, &entity, &fn_scope, entities, edges);
    }

    extract_method_body(node, src, &entity, &fn_scope, entities, edges, sites);
}

/// A `singleton_method` (`def self.name ... end` / `def obj.name ... end`):
/// modeled as a `Method` regardless of owner, same posture as every other
/// adapter's lack of a dedicated "static method" kind.
#[allow(clippy::too_many_arguments)]
fn extract_singleton_method<'a>(
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
    let fn_scope = child_scope(owner_scope, name);
    declare(owner, entity.clone(), entities, edges);

    if let Some(params) = node.child_by_field_name("parameters") {
        extract_parameters(params, src, &entity, &fn_scope, entities, edges);
    }

    extract_method_body(node, src, &entity, &fn_scope, entities, edges, sites);
}

/// Shared by `extract_method`/`extract_singleton_method`: a `body_statement`
/// body is walked statement by statement (`extract_body`); an endless
/// method's bare expression body (`def square(x) = x * x`) gets one generic
/// `Statement` entity instead, so an endless and block-bodied method aren't
/// silently treated differently at the entity level -- mirrors Kotlin's
/// expression-bodied-function handling. Either shape is pushed into
/// `sites.bodies` for `resolve_body` to walk later. A method with no body at
/// all (an abstract stub, rare in plain Ruby) extracts no further.
#[allow(clippy::too_many_arguments)]
fn extract_method_body<'a>(
    node: Node<'a>,
    src: &str,
    entity: &SyntaxEntity,
    fn_scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    if body.kind() == "body_statement" {
        extract_body(body, src, entity, fn_scope, entities, edges);
    } else {
        let stmt_entity = SyntaxEntity::new(SyntaxKind::Statement, fn_scope, "stmt_0");
        contain(entity, stmt_entity, entities, edges);
    }
    sites.bodies.push((entity.clone(), body));
}

/// A `method_parameters`/`block_parameters` list's individual parameter
/// nodes become `Parameter` entities, named after each one's own `name`
/// field. `forward_parameter` (`...`) and `destructured_parameter`
/// (`(a, b)`) are a documented gap -- not walked for individual names,
/// matching Python's own `*args`/`**kwargs` gap and Kotlin's
/// `multi_variable_declaration` gap. An anonymous splat/double-splat/block
/// parameter (bare `*`, `**`, `&` with no name) has no `name` field to read,
/// so it's skipped too -- there is no name to bind.
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
        let name_node = match param.kind() {
            "identifier" => Some(param),
            "optional_parameter" | "splat_parameter" | "hash_splat_parameter"
            | "block_parameter" | "keyword_parameter" => param.child_by_field_name("name"),
            _ => None,
        };
        let Some(name_node) = name_node else {
            continue;
        };
        let entity = SyntaxEntity::new(SyntaxKind::Parameter, scope, text(name_node, src));
        contain(parent, entity, entities, edges);
    }
}

/// A `body_statement`'s direct statement children: a simple
/// `name = value`/`@ivar = value`/`@@cvar = value` assignment becomes a
/// `Variable` entity (see the module-level note on why these aren't
/// hoisted to the class); everything else becomes a generic `Statement`
/// entity. A nested `method`/`class`/`module` inside a body is not
/// extracted -- a documented gap, matching every other adapter's own
/// posture toward nested item definitions inside a body.
fn extract_body(
    body: Node,
    src: &str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    let mut cursor = body.walk();
    for (index, stmt) in body.named_children(&mut cursor).enumerate() {
        match stmt.kind() {
            "assignment" => {
                let Some(left) = stmt.child_by_field_name("left") else {
                    continue;
                };
                if is_simple_binding_target(left) {
                    let entity =
                        SyntaxEntity::new(SyntaxKind::Variable, scope, text(left, src));
                    contain(parent, entity, entities, edges);
                } else {
                    let entity = SyntaxEntity::new(
                        SyntaxKind::Statement,
                        scope,
                        format!("stmt_{index}"),
                    );
                    contain(parent, entity, entities, edges);
                }
            }
            "method" | "singleton_method" | "class" | "module" => {}
            _ => {
                let entity =
                    SyntaxEntity::new(SyntaxKind::Statement, scope, format!("stmt_{index}"));
                contain(parent, entity, entities, edges);
            }
        }
    }
}

/// `REFERENCES`/`CALLS`/`INSTANTIATES` edges from one method/function body.
/// `body` is either a `body_statement` (walked statement by statement) or a
/// bare expression (an endless method) -- mirrors Kotlin's `resolve_body`,
/// adapted to Ruby's `identifier`/`instance_variable`/`class_variable`/
/// `call` node kinds. A bare identifier/ivar/cvar resolves via
/// `scope_tree`; a receiverless call (`helper()`/`helper(x)`) resolves the
/// same way by its method name; a receiver-qualified call
/// (`obj.method(...)`) resolves by best-effort name match against any
/// `Method` symbol in the file (real receiver-type dispatch needs static
/// types Ruby doesn't have, out of scope here, matching Python's own
/// posture); a call whose callee resolves in scope to a `Type` symbol is
/// treated as `Circle.new(...)`-style instantiation only when the method
/// name is literally `new` (Ruby's actual instantiation convention, unlike
/// Kotlin's constructor-call syntax) -- anything else resolved to a `Type`
/// symbol falls through to a plain `CALLS` edge instead. Anything
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

    if body.kind() == "body_statement" {
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
        edges.push(Edge::new(
            fn_symbol.id.clone(),
            target.id.clone(),
            EdgeKind::References,
        ));
    }

    for call_node in &calls {
        let Some(method_node) = call_node.child_by_field_name("method") else {
            continue;
        };
        if method_node.kind() != "identifier" {
            continue;
        }
        let ident = text(method_node, src);
        let is_qualified = call_node.child_by_field_name("receiver").is_some();

        let resolved_in_scope = scope_tree
            .lookup(scope, ident)
            .and_then(|id| symbol_table.find_by_syntax_id(id))
            .cloned();

        if ident == "new" {
            if let Some(receiver) = call_node.child_by_field_name("receiver") {
                let receiver_name = text(receiver, src);
                if let Some(type_symbol) = scope_tree
                    .lookup(scope, receiver_name)
                    .and_then(|id| symbol_table.find_by_syntax_id(id))
                {
                    if type_symbol.kind == SymbolKind::Type {
                        edges.push(Edge::new(
                            fn_symbol.id.clone(),
                            type_symbol.id.clone(),
                            EdgeKind::Instantiates,
                        ));
                        continue;
                    }
                }
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
        edges.push(Edge::new(
            fn_symbol.id.clone(),
            target.id.clone(),
            EdgeKind::Calls,
        ));
    }

    (symbols, edges)
}

fn walk_expr<'a>(
    node: Node<'a>,
    on_name: &mut impl FnMut(Node<'a>),
    on_call: &mut impl FnMut(Node<'a>),
) {
    match node.kind() {
        "identifier" | "instance_variable" | "class_variable" | "global_variable" => {
            on_name(node)
        }
        "call" => {
            on_call(node);
            if let Some(receiver) = node.child_by_field_name("receiver") {
                walk_expr(receiver, on_name, on_call);
            }
            if let Some(args) = node.child_by_field_name("arguments") {
                let mut cursor = args.walk();
                for arg in args.named_children(&mut cursor) {
                    walk_argument(arg, on_name, on_call);
                }
            }
            // A call's attached block (`{ ... }`/`do ... end`) is out of
            // scope -- see the module-level note on blocks/procs/lambdas.
        }
        "assignment" => {
            if let Some(right) = node.child_by_field_name("right") {
                walk_expr(right, on_name, on_call);
            }
            if let Some(left) = node.child_by_field_name("left") {
                if !is_simple_binding_target(left) {
                    walk_expr(left, on_name, on_call);
                }
            }
        }
        "operator_assignment" => {
            if let Some(right) = node.child_by_field_name("right") {
                walk_expr(right, on_name, on_call);
            }
        }
        "binary" => {
            if let Some(left) = node.child_by_field_name("left") {
                walk_expr(left, on_name, on_call);
            }
            if let Some(right) = node.child_by_field_name("right") {
                walk_expr(right, on_name, on_call);
            }
        }
        "unary" => {
            if let Some(operand) = node.child_by_field_name("operand") {
                walk_expr(operand, on_name, on_call);
            }
        }
        "conditional" => {
            if let Some(condition) = node.child_by_field_name("condition") {
                walk_expr(condition, on_name, on_call);
            }
            if let Some(consequence) = node.child_by_field_name("consequence") {
                walk_expr(consequence, on_name, on_call);
            }
            if let Some(alternative) = node.child_by_field_name("alternative") {
                walk_expr(alternative, on_name, on_call);
            }
        }
        "element_reference" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                walk_expr(child, on_name, on_call);
            }
        }
        "array" | "hash" | "argument_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                walk_argument(child, on_name, on_call);
            }
        }
        "return" | "break" | "next" => {
            if let Some(args) = node.named_child(0) {
                walk_argument(args, on_name, on_call);
            }
        }
        "parenthesized_statements" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                walk_stmt(child, on_name, on_call);
            }
        }
        // `if`/`unless`/`case`/blocks/lambdas and other control-flow or
        // scope-introducing expression forms are a documented gap here --
        // see the module-level note on blocks/procs/lambdas, and the
        // batch's own "structural extraction only" guidance for the rest.
        _ => {}
    }
}

/// An `argument_list`/`array`/`hash` entry: `pair` (a hash literal's
/// `key: value`/`key => value`) walks its value only -- the key is usually
/// a bare symbol/string literal, not a reference; `splat_argument`/
/// `hash_splat_argument`/`block_argument` walk their single wrapped
/// expression; anything else is walked as a plain expression.
fn walk_argument<'a>(
    node: Node<'a>,
    on_name: &mut impl FnMut(Node<'a>),
    on_call: &mut impl FnMut(Node<'a>),
) {
    match node.kind() {
        "pair" => {
            if let Some(value) = node.child_by_field_name("value") {
                walk_expr(value, on_name, on_call);
            }
        }
        "splat_argument" | "hash_splat_argument" | "block_argument" => {
            if let Some(inner) = node.named_child(0) {
                walk_expr(inner, on_name, on_call);
            }
        }
        _ => walk_expr(node, on_name, on_call),
    }
}

fn walk_stmt<'a>(
    node: Node<'a>,
    on_name: &mut impl FnMut(Node<'a>),
    on_call: &mut impl FnMut(Node<'a>),
) {
    match node.kind() {
        "if" | "unless" => {
            if let Some(condition) = node.child_by_field_name("condition") {
                walk_expr(condition, on_name, on_call);
            }
            if let Some(consequence) = node.child_by_field_name("consequence") {
                walk_then(consequence, on_name, on_call);
            }
        }
        "while" | "until" => {
            if let Some(condition) = node.child_by_field_name("condition") {
                walk_expr(condition, on_name, on_call);
            }
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for stmt in body.named_children(&mut cursor) {
                    walk_stmt(stmt, on_name, on_call);
                }
            }
        }
        "body_statement" => {
            let mut cursor = node.walk();
            for stmt in node.named_children(&mut cursor) {
                walk_stmt(stmt, on_name, on_call);
            }
        }
        // `case`/`for`/`begin`/`rescue`/blocks and other less common
        // control flow are a documented gap here, matching Kotlin's own
        // limited `walk_stmt` coverage.
        _ => walk_expr(node, on_name, on_call),
    }
}

/// A `then`/`else`/`elsif` clause's direct statement children.
fn walk_then<'a>(
    node: Node<'a>,
    on_name: &mut impl FnMut(Node<'a>),
    on_call: &mut impl FnMut(Node<'a>),
) {
    match node.kind() {
        "then" => {
            let mut cursor = node.walk();
            for stmt in node.named_children(&mut cursor) {
                walk_stmt(stmt, on_name, on_call);
            }
        }
        _ => walk_stmt(node, on_name, on_call),
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
    name: &str,
) -> Option<SymbolEntity> {
    all_entities
        .iter()
        .find(|e| e.kind == SyntaxKind::Class && e.name == name)
        .and_then(|e| symbol_table.find_by_syntax_id(&e.id))
        .cloned()
}

/// Resolve `sites`' explicit superclass/mixin relations into `EXTENDS`/
/// `IMPLEMENTS` edges, searched repository-wide (`all_entities`) so a
/// superclass/mixin declared in a different file still resolves -- mirrors
/// Kotlin's `resolve_structural`. A name that resolves to nothing in the
/// repository (an external/stdlib class or module, e.g. `StandardError`,
/// `Comparable`) gets an explicit placeholder symbol, never silently
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
        let target = find_type_symbol(all_entities, symbol_table, superclass_name).unwrap_or_else(
            || {
                let placeholder = unresolved_symbol("external-class", superclass_name);
                symbols.push(placeholder.clone());
                placeholder
            },
        );
        edges.push(Edge::new(
            owner_symbol.id.clone(),
            target.id,
            EdgeKind::Extends,
        ));
    }

    for (entity, mixin_name) in &sites.mixins {
        let Some(owner_symbol) = symbol_table.find_by_syntax_id(&entity.id) else {
            continue;
        };
        let target = find_type_symbol(all_entities, symbol_table, mixin_name).unwrap_or_else(|| {
            let placeholder = unresolved_symbol("external-module", mixin_name);
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

/// Resolve a file's `require`/`require_relative` calls into `IMPORTS` edges
/// from `module_symbol`. Neither form names a specific declared symbol the
/// way Python's `from X import Y` does -- both just load a file/gem path --
/// so every one resolves to an explicit `external-module` placeholder,
/// matching Python's own bare `import X` treatment.
fn resolve_requires(
    requires: &[Node],
    src: &str,
    module_symbol: &SymbolEntity,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for call_node in requires {
        let Some(args) = call_node.child_by_field_name("arguments") else {
            continue;
        };
        let Some(first_arg) = args.named_child(0) else {
            continue;
        };
        let path = match first_arg.kind() {
            "string" => {
                let mut cursor = first_arg.walk();
                let found = first_arg
                    .named_children(&mut cursor)
                    .find(|c| c.kind() == "string_content")
                    .map(|c| text(c, src).to_string());
                found.unwrap_or_else(|| text(first_arg, src).to_string())
            }
            _ => text(first_arg, src).to_string(),
        };
        let placeholder = unresolved_symbol("external-module", &path);
        symbols.push(placeholder.clone());
        edges.push(Edge::new(
            module_symbol.id.clone(),
            placeholder.id,
            EdgeKind::Imports,
        ));
    }

    (symbols, edges)
}

/// Resolve a file's top-level `class`/`module` declarations into `EXPORTS`
/// edges from `module_symbol`. Ruby has no file-level privacy keyword
/// analogous to Python's leading underscore or Kotlin's
/// `private`/`internal` modifiers -- every top-level constant (class or
/// module) is reachable from anywhere once its file is loaded -- so every
/// one is exported unconditionally, a documented convention difference
/// from Python/Kotlin's own gated `resolve_exports`, not an oversight.
fn resolve_exports(
    file_entities: &[SyntaxEntity],
    file_edges: &[Edge],
    module_symbol: &SymbolEntity,
    symbol_table: &SymbolTable,
) -> Vec<Edge> {
    let mut edges = Vec::new();
    let module_id = &file_entities[0].id;
    for entity_id in declared_children(file_edges, module_id) {
        let Some(entity) = file_entities.iter().find(|e| &e.id == entity_id) else {
            continue;
        };
        if entity.kind != SyntaxKind::Class {
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

// Ruby has no static type syntax at all in ordinary source files (RBS/
// `sig` annotations live in separate files/DSLs this adapter doesn't
// parse), so unlike every statically-typed adapter here, there is
// deliberately no `extract_type` function in this file -- every `TypeRef`
// this language could produce would be fabricated from nothing. This is
// the same correct-not-a-gap posture Python's adapter documents for its
// own untyped values; Ruby carries no annotation syntax even for the
// *optional* case Python's PEP 484 gives it, so this adapter has strictly
// less to extract, not a smaller implementation of the same thing.

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
        let adapter = RubyAdapter;
        let ast = adapter.parse(source).expect("valid ruby parses");
        adapter.extract(&ast, "module.rb")
    }

    #[test]
    fn parses_valid_ruby_source() {
        let adapter = RubyAdapter;
        assert!(adapter.parse("def run\nend\n").is_ok());
    }

    #[test]
    fn parsing_invalid_syntax_is_still_ok() {
        // tree-sitter is error-tolerant and always returns *some* tree, so
        // "invalid" Ruby doesn't surface as an `Err` here -- matching every
        // other tree-sitter-backed adapter's documented posture.
        let adapter = RubyAdapter;
        assert!(adapter.parse("def {{{ not ruby").is_ok());
    }

    #[test]
    fn extract_always_returns_the_file_module_first() {
        let (entities, _) = extract("");
        assert_eq!(entities[0].kind, SyntaxKind::Module);
        assert_eq!(entities[0].name, "module.rb");
    }

    #[test]
    fn extracts_a_top_level_method_with_its_parameters() {
        let (entities, edges) = extract("def add(a, b)\n  a + b\nend\n");

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
    fn extracts_a_class_with_a_method() {
        let (entities, edges) =
            extract("class Circle\n  def area(factor)\n    factor\n  end\nend\n");

        let module = &entities[0];
        let class = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Circle")
            .expect("Class entity");
        assert!(edges.contains(&Edge::new(
            module.id.clone(),
            class.id.clone(),
            EdgeKind::Declares
        )));

        let method = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "area")
            .expect("Method entity");
        assert!(edges.contains(&Edge::new(
            class.id.clone(),
            method.id.clone(),
            EdgeKind::Declares
        )));
    }

    #[test]
    fn extracts_a_module_as_a_class_kind_entity() {
        let (entities, edges) = extract("module Greet\n  def hello\n  end\nend\n");
        let module_file = &entities[0];
        let greet = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Greet")
            .expect("Ruby module maps onto SyntaxKind::Class");
        assert!(edges.contains(&Edge::new(
            module_file.id.clone(),
            greet.id.clone(),
            EdgeKind::Declares
        )));
    }

    #[test]
    fn extracts_a_singleton_method_as_a_method() {
        let (entities, _) = extract("class Factory\n  def self.create\n  end\nend\n");
        entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Method && e.name == "create")
            .expect("singleton method modeled as a Method");
    }

    #[test]
    fn extracts_attr_accessor_names_as_variables() {
        let (entities, edges) =
            extract("class Point\n  attr_accessor :x, :y\nend\n");
        let class = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Point")
            .unwrap();
        let fields: Vec<&SyntaxEntity> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Variable)
            .collect();
        assert_eq!(fields.len(), 2);
        assert!(fields.iter().any(|f| f.name == "x"));
        assert!(fields.iter().any(|f| f.name == "y"));
        for field in &fields {
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
    }

    #[test]
    fn extracts_attr_reader_and_attr_writer_too() {
        let (entities, _) =
            extract("class Box\n  attr_reader :size\n  attr_writer :label\nend\n");
        let names: Vec<&str> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Variable)
            .map(|e| e.name.as_str())
            .collect();
        assert!(names.contains(&"size"));
        assert!(names.contains(&"label"));
    }

    #[test]
    fn extracts_an_instance_variable_assignment_in_a_method_body_as_a_variable() {
        let (entities, _) = extract("class C\n  def run\n    @total = 1\n  end\nend\n");
        entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Variable && e.name == "@total")
            .expect("instance variable assignment extracted as a Variable");
    }

    #[test]
    fn extracts_a_local_variable_assignment_in_a_method_body() {
        let (entities, _) = extract("def run\n  total = 1\n  total\nend\n");
        entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Variable && e.name == "total")
            .expect("local variable extracted");
    }

    #[test]
    fn a_non_assignment_statement_becomes_a_generic_statement_entity() {
        let (entities, _) = extract("def run\n  puts 1\nend\n");
        assert!(entities.iter().any(|e| e.kind == SyntaxKind::Statement));
    }

    #[test]
    fn an_endless_method_still_gets_a_statement_entity() {
        let (entities, _) = extract("def square(x) = x * x\n");
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
    fn same_named_method_in_a_class_and_at_top_level_gets_distinct_ids() {
        let source = "def run\nend\n\nclass Runner\n  def run\n  end\nend\n";
        let (entities, _) = extract(source);
        let methods: Vec<&SyntaxEntity> = entities
            .iter()
            .filter(|e| {
                e.name == "run" && (e.kind == SyntaxKind::Function || e.kind == SyntaxKind::Method)
            })
            .collect();
        assert_eq!(methods.len(), 2);
        assert_ne!(methods[0].id, methods[1].id);
    }

    #[test]
    fn open_class_reopening_declares_again_with_the_same_id() {
        let source = "class Foo\n  def a\n  end\nend\n\nclass Foo\n  def b\n  end\nend\n";
        let (entities, _) = extract(source);
        let foos: Vec<&SyntaxEntity> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Class && e.name == "Foo")
            .collect();
        assert_eq!(foos.len(), 2);
        assert_eq!(foos[0].id, foos[1].id, "reopened class gets the same entity id");
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
        let adapter = RubyAdapter;
        let ast = adapter.parse(source).expect("valid ruby parses");
        let (entities, edges) = adapter.extract(&ast, "module.rb");
        let module = entities[0].clone();
        let fn_entity = entities
            .iter()
            .find(|e| {
                (e.kind == SyntaxKind::Method || e.kind == SyntaxKind::Function)
                    && e.name == fn_name
            })
            .unwrap_or_else(|| panic!("no Method/Function entity named {fn_name}"))
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

    fn method_named<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        fn find<'a>(node: Node<'a>, src: &'a str, name: &str) -> Option<Node<'a>> {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if (child.kind() == "method" || child.kind() == "singleton_method")
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

    fn body_of<'a>(tree: &'a Tree, src: &'a str, name: &str) -> Node<'a> {
        let func = method_named(tree, src, name);
        func.child_by_field_name("body").expect("method body")
    }

    #[test]
    fn resolves_a_reference_to_an_in_scope_local_variable() {
        let source = "def run\n  value = 1\n  value\nend\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) =
            resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

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
        let source = "def run\n  missing\nend\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (symbols, _edges) =
            resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        assert!(symbols.iter().any(|s| s.name.starts_with("unknown-local")));
    }

    #[test]
    fn resolves_a_free_function_style_call() {
        let source = "def helper\nend\n\ndef run\n  helper()\nend\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) =
            resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

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
    fn resolves_a_receiver_qualified_call_by_best_effort_name_match() {
        let source =
            "class Shape\n  def area\n  end\nend\n\nclass Run\n  def run(shape)\n    shape.area()\n  end\nend\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) =
            resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

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
    fn resolves_a_dot_new_instantiation_for_an_in_scope_class() {
        let source = "class Circle\nend\n\ndef run\n  c = Circle.new()\nend\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) =
            resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

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
    fn a_class_extending_another_local_class_gets_an_explicit_extends_edge() {
        let source = "class Animal\nend\nclass Dog < Animal\nend\n";
        let adapter = RubyAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "module.rb");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "module.rb",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );
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
    fn extending_an_external_class_gets_an_explicit_placeholder() {
        let source = "class Custom < StandardError\nend\n";
        let adapter = RubyAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "module.rb");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "module.rb",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );
        let mut symbols = Vec::new();
        let edges = resolve_structural(&sites, &entities, &symbol_table, &mut symbols);

        assert!(symbols.iter().any(|s| s.name.starts_with("external-class")));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Extends));
    }

    #[test]
    fn a_class_including_a_local_module_gets_an_implements_edge() {
        let source = "module Greetable\n  def hello\n  end\nend\n\nclass Person\n  include Greetable\nend\n";
        let adapter = RubyAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "module.rb");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "module.rb",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );
        let mut symbols = Vec::new();
        let edges = resolve_structural(&sites, &entities, &symbol_table, &mut symbols);

        let person = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Person")
            .unwrap();
        let greetable = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Greetable")
            .unwrap();
        let person_symbol = symbol_table.find_by_syntax_id(&person.id).unwrap();
        let greetable_symbol = symbol_table.find_by_syntax_id(&greetable.id).unwrap();
        assert!(edges.contains(&Edge::new(
            person_symbol.id.clone(),
            greetable_symbol.id.clone(),
            EdgeKind::Implements
        )));
    }

    #[test]
    fn including_an_external_module_gets_an_explicit_placeholder() {
        let source = "class Item\n  include Comparable\nend\n";
        let adapter = RubyAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "module.rb");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "module.rb",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );
        let mut symbols = Vec::new();
        let edges = resolve_structural(&sites, &entities, &symbol_table, &mut symbols);

        assert!(symbols
            .iter()
            .any(|s| s.name.starts_with("external-module:Comparable")));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Implements));
    }

    #[test]
    fn resolves_require_to_an_explicit_external_module_placeholder() {
        let source = "require 'set'\nrequire_relative 'helper'\n\nclass C\nend\n";
        let adapter = RubyAdapter;
        let ast = adapter.parse(source).unwrap();
        let module_symbol = SymbolEntity::new(SymbolKind::Namespace, "module.rb", "module.rb");

        let mut sites = ResolutionSites::default();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &SyntaxEntity::new(SyntaxKind::Module, "module.rb", "module.rb"),
            "module.rb",
            &mut Vec::new(),
            &mut Vec::new(),
            &mut sites,
        );

        let (symbols, edges) = resolve_requires(&sites.requires, &ast.source, &module_symbol);

        assert_eq!(symbols.len(), 2);
        assert!(symbols.iter().any(|s| s.name == "external-module:set"));
        assert!(symbols.iter().any(|s| s.name == "external-module:helper"));
        assert_eq!(
            edges.iter().filter(|e| e.kind == EdgeKind::Imports).count(),
            2
        );
    }

    #[test]
    fn exports_every_top_level_class_and_module_unconditionally() {
        let source = "class Public\nend\nmodule AlsoPublic\nend\n";
        let adapter = RubyAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, edges) = adapter.extract(&ast, "module.rb");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table.find_by_syntax_id(&entities[0].id).unwrap().clone();

        let export_edges = resolve_exports(&entities, &edges, &module_symbol, &symbol_table);

        assert_eq!(export_edges.len(), 2);
        assert!(export_edges.iter().all(|e| e.kind == EdgeKind::Exports));
    }
}
