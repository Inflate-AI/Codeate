use tree_sitter::{Node, Parser, Tree};

use crate::adapter::LanguageAdapter;
use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};
use crate::resolve::unresolved_symbol;
use crate::symbols::{ScopeTree, SymbolTable};
use crate::types::TypeRef;

/// C's built-in primitive types (`_type_specifier`'s `primitive_type`
/// alternative in `tree-sitter-c`'s grammar), treated as `TypeRef::Primitive`
/// the same way Go's/Rust's basic-type lists are. `sized_type_specifier`
/// combinations (`unsigned long`, `short int`, ...) aren't classified here --
/// `extract_type` only inspects the single `type` field tree-sitter hands
/// back, and a `sized_type_specifier` node is a documented gap (falls
/// through to `Unresolved`), same posture as every other adapter's
/// not-yet-modeled shapes.
const PRIMITIVE_TYPES: &[&str] = &[
    "bool", "char", "int", "float", "double", "void", "size_t", "ssize_t", "ptrdiff_t",
    "intptr_t", "uintptr_t",
];

/// The parsed form a `CAdapter` produces: mirrors `GoAst`'s tree+source
/// pairing (`tree-sitter` nodes are spans into the source, not a
/// self-contained typed tree).
pub struct CAst {
    tree: Tree,
    source: String,
}

/// The C language adapter: parses C source via `tree-sitter`/`tree-sitter-c`
/// and maps it onto the canonical syntax model.
///
/// C is structurally the most different adapter in this codebase so far: it
/// has no classes, interfaces, or inheritance of any kind -- so this adapter
/// never produces `EXTENDS`/`IMPLEMENTS` edges. That is a correct absence,
/// not an unfinished corner: C's type system has nothing for those edges to
/// represent. Likewise there is no method/receiver concept -- every
/// function is free-standing, so this adapter has no `Method` extraction at
/// all, only `Function`.
///
/// C also has no real module/import system. `#include` is the closest
/// analog to an import, but resolving it requires running the C
/// preprocessor, which `tree-sitter-c`'s grammar deliberately does not do
/// (a `#include` is parsed as an opaque `preproc_include` directive, not a
/// resolved dependency). Full `#include` resolution is out of scope for
/// this sub-phase and is not attempted here -- this adapter produces no
/// `IMPORTS` edges. `EXPORTS` is approximated instead from linkage: a
/// top-level function or global variable declared `static` has internal
/// linkage (not exported, in the sense of being visible to other
/// translation units); anything else (`extern`, or no storage-class
/// specifier, which defaults to external linkage for file-scope
/// declarations) is treated as exported.
pub struct CAdapter;

impl LanguageAdapter for CAdapter {
    type Ast = CAst;

    fn parse(&self, source: &str) -> Result<Self::Ast> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .map_err(|err| Error::Message(format!("failed to load C grammar: {err}")))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::Message("failed to parse C source".to_string()))?;
        Ok(CAst {
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
        _all_entities: &[SyntaxEntity],
        symbol_table: &SymbolTable,
        edges_so_far: &[Edge],
        module_symbol: &SymbolEntity,
        file_module: &SyntaxEntity,
    ) -> (Vec<SymbolEntity>, Vec<Edge>) {
        // Re-derive this file's own entities/edges/sites, mirroring the Go
        // adapter's own `resolve_file` -- `sites.bodies`' `SyntaxEntity`s
        // must be the exact same (by `EntityId`) ones `extract` already
        // handed the pipeline.
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

        // No EXTENDS/IMPLEMENTS resolution: C has no inheritance/interface
        // construct for those edges to represent (see the module doc
        // comment above). No IMPORTS resolution either: `#include`
        // resolution needs the preprocessor, out of scope here.

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

/// Sites `extract_items` collects that `resolve_file` needs the raw tree
/// for, plus linkage information `resolve_exports` needs. Mirrors the Go
/// adapter's `ResolutionSites`, with `static_names` added for C's
/// `static`/`extern` EXPORTS proxy.
#[derive(Default)]
struct ResolutionSites<'a> {
    bodies: Vec<(SyntaxEntity, Node<'a>)>,
    /// Names of top-level functions/variables declared `static` in this
    /// file -- internal linkage, so `resolve_exports` excludes them.
    static_names: std::collections::HashSet<String>,
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
    edges.push(Edge::new(parent.id.clone(), child.id.clone(), EdgeKind::Contains));
    edges.push(Edge::new(parent.id.clone(), child.id.clone(), EdgeKind::Declares));
    entities.push(child);
}

fn contain(
    parent: &SyntaxEntity,
    child: SyntaxEntity,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    edges.push(Edge::new(parent.id.clone(), child.id.clone(), EdgeKind::Contains));
    entities.push(child);
}

/// A declarator resolved down to either a plain name, or recognized as a
/// function prototype (a `function_declarator` whose own declarator is a
/// bare identifier -- `int add(int, int);`, no body). Everything else --
/// plain variables, pointer variables, array variables, and function-pointer
/// variables (`void (*cb)(int);`, whose declarator is a `function_declarator`
/// wrapping a *parenthesized* identifier rather than a bare one) -- resolves
/// to `Name`.
enum Declared<'a> {
    Name(Node<'a>),
    Prototype,
}

fn classify_declarator(node: Node) -> Option<Declared> {
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" => Some(Declared::Name(node)),
        "pointer_declarator" | "array_declarator" | "attributed_declarator" => {
            node.child_by_field_name("declarator")
                .and_then(classify_declarator)
        }
        "init_declarator" => node
            .child_by_field_name("declarator")
            .and_then(classify_declarator),
        "parenthesized_declarator" => node.named_child(0).and_then(classify_declarator),
        "function_declarator" => {
            let inner = node.child_by_field_name("declarator")?;
            if inner.kind() == "identifier" {
                Some(Declared::Prototype)
            } else {
                classify_declarator(inner)
            }
        }
        _ => None,
    }
}

/// The identifier a declarator ultimately names, regardless of pointer/
/// array/parenthesized wrapping -- used where a bare name is all that's
/// needed (parameters, a function definition's own declarator) and
/// prototype-vs-variable classification doesn't matter.
fn declarator_identifier(node: Node) -> Option<Node> {
    match classify_declarator(node) {
        Some(Declared::Name(n)) => Some(n),
        _ => None,
    }
}

/// A `function_definition`'s top-level declarator, walked down through any
/// pointer/array wrapping (`int *make(void) {...}`) to the actual
/// `function_declarator`, returning its name identifier and `parameter_list`
/// node.
fn function_signature<'a>(node: Node<'a>) -> Option<(Node<'a>, Node<'a>)> {
    match node.kind() {
        "function_declarator" => {
            let inner = node.child_by_field_name("declarator")?;
            let params = node.child_by_field_name("parameters")?;
            let name = declarator_identifier(inner)?;
            Some((name, params))
        }
        "pointer_declarator" | "array_declarator" | "attributed_declarator" => {
            node.child_by_field_name("declarator")
                .and_then(function_signature)
        }
        "parenthesized_declarator" => node.named_child(0).and_then(function_signature),
        _ => None,
    }
}

/// Whether `node` (a `function_definition`/`declaration`) carries a `static`
/// `storage_class_specifier` direct child.
fn has_static_storage(node: Node, src: &str) -> bool {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(|c| c.kind() == "storage_class_specifier" && text(c, src) == "static");
    found
}

/// Walk a `translation_unit`'s direct children. Two passes, mirroring the
/// Go adapter: struct/union/enum type declarations are extracted first (so
/// declaration order in the file -- C has none of Go's package-wide
/// declaration-order independence to begin with, but a `typedef` can still
/// appear after code that would want to reference it in a fuller
/// implementation) doesn't matter for entity presence, then
/// functions/global variables are extracted with those type entities
/// already available.
fn extract_items<'a>(
    root: Node<'a>,
    src: &'a str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    sites: &mut ResolutionSites<'a>,
) {
    let mut seen_type_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        match child.kind() {
            "struct_specifier" => {
                extract_struct_or_union(child, src, parent, scope, entities, edges, &mut seen_type_names);
            }
            "union_specifier" => {
                extract_struct_or_union(child, src, parent, scope, entities, edges, &mut seen_type_names);
            }
            "enum_specifier" => {
                extract_enum(child, src, parent, scope, entities, edges, &mut seen_type_names);
            }
            "type_definition" => {
                extract_typedef(child, src, parent, scope, entities, edges, &mut seen_type_names);
            }
            _ => {}
        }
    }

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        match child.kind() {
            "function_definition" => {
                extract_function(child, src, parent, scope, entities, edges, sites);
            }
            "declaration" => {
                extract_top_level_declaration(child, src, parent, scope, entities, edges, sites);
            }
            // `preproc_include` and other preprocessor directives are
            // deliberately not modeled -- see the module doc comment on
            // `CAdapter`: resolving them needs the preprocessor pass,
            // which `tree-sitter-c` does not run.
            _ => {}
        }
    }
}

/// A named `struct_specifier`/`union_specifier` with a body becomes a
/// `Struct` entity. The canonical model has no dedicated `Union` syntax
/// kind (see `crate::model::SyntaxKind`) -- a C `union` is structurally
/// closest to a `Struct` (a named aggregate type with member fields), so it
/// is mapped onto `SyntaxKind::Struct` too, same as the Go/Rust adapters'
/// own choice to not distinguish struct-like shapes any finer than the
/// canonical model already does. Struct/union fields aren't extracted as
/// entities, the same documented, not-yet-wired-in gap the Go/Rust adapters
/// already carry for their own field extraction.
fn extract_struct_or_union(
    node: Node,
    src: &str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    seen: &mut std::collections::HashSet<String>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    if node.child_by_field_name("body").is_none() {
        return;
    }
    let name = text(name_node, src).to_string();
    if !seen.insert(name.clone()) {
        return;
    }
    let entity = SyntaxEntity::new(SyntaxKind::Struct, scope, name);
    declare(parent, entity, entities, edges);
}

/// A named `enum_specifier` with a body becomes an `Enum` entity. Individual
/// enumerator constants aren't extracted as entities -- the same documented
/// gap as struct/union fields above.
fn extract_enum(
    node: Node,
    src: &str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    seen: &mut std::collections::HashSet<String>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    if node.child_by_field_name("body").is_none() {
        return;
    }
    let name = text(name_node, src).to_string();
    if !seen.insert(name.clone()) {
        return;
    }
    let entity = SyntaxEntity::new(SyntaxKind::Enum, scope, name);
    declare(parent, entity, entities, edges);
}

/// A `typedef` becomes a `Struct`/`Enum` entity named after the typedef
/// alias when its underlying type is an (optionally anonymous)
/// struct/union/enum -- `typedef struct { ... } Circle;` and
/// `typedef struct Circle { ... } Circle;` both produce one `Struct` entity
/// named `Circle`, deduplicated against a same-named tagged struct already
/// extracted by `extract_struct_or_union`. A typedef over anything else
/// (`typedef int MyInt;`, `typedef void (*Handler)(int);`) has no clean
/// Struct/Interface/Enum mapping in the canonical model -- a documented
/// gap, not guessed at, mirroring the Go adapter's own defined-type gap.
fn extract_typedef(
    node: Node,
    src: &str,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    seen: &mut std::collections::HashSet<String>,
) {
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    let Some(declarator) = node.child_by_field_name("declarator") else {
        return;
    };
    let Some(alias_name) = declarator_identifier(declarator).map(|n| text(n, src).to_string())
    else {
        return;
    };

    let kind = match type_node.kind() {
        "struct_specifier" | "union_specifier" => SyntaxKind::Struct,
        "enum_specifier" => SyntaxKind::Enum,
        _ => return,
    };

    if !seen.insert(alias_name.clone()) {
        return;
    }
    let entity = SyntaxEntity::new(kind, scope, alias_name);
    declare(parent, entity, entities, edges);
}

/// A top-level function definition (has a body -- see
/// `extract_top_level_declaration` for prototype-only declarations, which
/// this adapter does not turn into entities).
fn extract_function<'a>(
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

/// A `parameter_list`'s `parameter_declaration`s become `Parameter`
/// entities. An unnamed parameter (`int f(int)`, common in prototypes and
/// occasionally in definitions) and the bare `void` no-parameters marker
/// both bind no name -- skipped, not guessed at.
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
        if param.kind() != "parameter_declaration" {
            continue;
        }
        let Some(declarator) = param.child_by_field_name("declarator") else {
            continue;
        };
        let Some(name_node) = declarator_identifier(declarator) else {
            continue;
        };
        let entity = SyntaxEntity::new(SyntaxKind::Parameter, scope, text(name_node, src));
        contain(parent, entity, entities, edges);
    }
}

/// A top-level `declaration`: either a function prototype (no body --
/// `int add(int a, int b);`), which this adapter does not model as an
/// entity (nothing new to say about it beyond the eventual definition, and
/// unlike Go's exported-vs-unexported convention, C has no separate
/// "interface" surface a prototype-only declaration would represent), or one
/// or more global variable declarators (`int a, *b, c[3];`), each of which
/// becomes a `Variable` entity.
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
        match classify_declarator(declarator) {
            Some(Declared::Name(name_node)) => {
                let name = text(name_node, src).to_string();
                if is_static {
                    sites.static_names.insert(name.clone());
                }
                let entity = SyntaxEntity::new(SyntaxKind::Variable, scope, name);
                contain(parent, entity, entities, edges);
            }
            Some(Declared::Prototype) | None => {}
        }
    }
}

/// Walk a function body's direct statements (its `compound_statement`'s
/// named children, not nested into deeper control flow -- matching the
/// Go/Rust adapters' own scope): a `declaration` becomes `Variable`
/// entities; everything else becomes a generic `Statement` entity.
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
                    if let Some(Declared::Name(name_node)) = classify_declarator(declarator) {
                        let entity = SyntaxEntity::new(
                            SyntaxKind::Variable,
                            &format!("{scope}#{index}"),
                            text(name_node, src),
                        );
                        contain(parent, entity, entities, edges);
                    }
                }
            }
            "function_definition" | "struct_specifier" | "union_specifier"
            | "enum_specifier" | "type_definition" => {}
            _ => {
                let entity =
                    SyntaxEntity::new(SyntaxKind::Statement, scope, format!("stmt_{index}"));
                contain(parent, entity, entities, edges);
            }
        }
    }
}

/// `REFERENCES`/`CALLS`/`INSTANTIATES` edges from one function body. A bare
/// identifier read resolves via `scope_tree`; a bare-name call (`helper()`)
/// resolves the same way; a local variable declared with a struct/union-typed
/// initializer list (`struct Circle c = { 1.0 };`) -- the closest C gets to
/// "constructing" an aggregate, since C has no constructor-call syntax -- is
/// an `INSTANTIATES` site when its declared type resolves in scope to a
/// `Type` symbol. Anything unresolved gets an explicit placeholder symbol,
/// never silently dropped.
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
    let mut instantiations: Vec<Node<'a>> = Vec::new();

    walk_block_statements(body, &mut |n| names.push(n), &mut |n| calls.push(n), &mut |n| {
        instantiations.push(n)
    });

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
        if func.kind() == "identifier" {
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
        } else {
            let placeholder = unresolved_symbol("unsupported-form", &format!("{:?}", func.kind()));
            symbols.push(placeholder.clone());
            edges.push(Edge::new(fn_symbol.id.clone(), placeholder.id.clone(), EdgeKind::Calls));
        }
    }

    for decl_node in &instantiations {
        let Some(type_node) = decl_node.child_by_field_name("type") else {
            continue;
        };
        let type_name = match type_node.kind() {
            "type_identifier" => text(type_node, src),
            "struct_specifier" | "union_specifier" => {
                let Some(name_node) = type_node.child_by_field_name("name") else {
                    continue;
                };
                text(name_node, src)
            }
            _ => continue,
        };
        if let Some(symbol) = scope_tree
            .lookup(scope, type_name)
            .and_then(|id| symbol_table.find_by_syntax_id(id))
        {
            if symbol.kind == SymbolKind::Type {
                edges.push(Edge::new(fn_symbol.id.clone(), symbol.id.clone(), EdgeKind::Instantiates));
            }
        }
    }

    (symbols, edges)
}

fn walk_expr_sites<'a>(
    node: Node<'a>,
    on_name: &mut impl FnMut(Node<'a>),
    on_call: &mut impl FnMut(Node<'a>),
) {
    match node.kind() {
        "identifier" => on_name(node),
        "call_expression" => {
            on_call(node);
            if let Some(func) = node.child_by_field_name("function") {
                walk_expr_sites(func, on_name, on_call);
            }
            if let Some(args) = node.child_by_field_name("arguments") {
                let mut cursor = args.walk();
                for arg in args.named_children(&mut cursor) {
                    walk_expr_sites(arg, on_name, on_call);
                }
            }
        }
        "field_expression" => {
            if let Some(operand) = node.child_by_field_name("argument") {
                walk_expr_sites(operand, on_name, on_call);
            }
        }
        "assignment_expression" => {
            if let Some(l) = node.child_by_field_name("left") {
                walk_expr_sites(l, on_name, on_call);
            }
            if let Some(r) = node.child_by_field_name("right") {
                walk_expr_sites(r, on_name, on_call);
            }
        }
        "binary_expression" => {
            if let Some(l) = node.child_by_field_name("left") {
                walk_expr_sites(l, on_name, on_call);
            }
            if let Some(r) = node.child_by_field_name("right") {
                walk_expr_sites(r, on_name, on_call);
            }
        }
        "unary_expression" | "update_expression" => {
            if let Some(operand) = node.child_by_field_name("argument") {
                walk_expr_sites(operand, on_name, on_call);
            }
        }
        "parenthesized_expression" => {
            if let Some(inner) = node.named_child(0) {
                walk_expr_sites(inner, on_name, on_call);
            }
        }
        "pointer_expression" => {
            if let Some(operand) = node.named_child(0) {
                walk_expr_sites(operand, on_name, on_call);
            }
        }
        _ => {}
    }
}

fn walk_stmt_sites<'a>(
    node: Node<'a>,
    on_name: &mut impl FnMut(Node<'a>),
    on_call: &mut impl FnMut(Node<'a>),
    on_instantiate: &mut impl FnMut(Node<'a>),
) {
    match node.kind() {
        "expression_statement" => {
            if let Some(inner) = node.named_child(0) {
                walk_expr_sites(inner, on_name, on_call);
            }
        }
        "return_statement" => {
            if let Some(inner) = node.named_child(0) {
                walk_expr_sites(inner, on_name, on_call);
            }
        }
        "declaration" => {
            on_instantiate(node);
            let mut cursor = node.walk();
            for declarator in node.children_by_field_name("declarator", &mut cursor) {
                if declarator.kind() == "init_declarator" {
                    if let Some(value) = declarator.child_by_field_name("value") {
                        walk_expr_sites(value, on_name, on_call);
                    }
                }
            }
        }
        "if_statement" => {
            if let Some(cond) = node.child_by_field_name("condition") {
                walk_expr_sites(cond, on_name, on_call);
            }
            if let Some(consequence) = node.child_by_field_name("consequence") {
                walk_stmt_sites(consequence, on_name, on_call, on_instantiate);
            }
            if let Some(alternative) = node.child_by_field_name("alternative") {
                if let Some(inner) = alternative.named_child(0) {
                    walk_stmt_sites(inner, on_name, on_call, on_instantiate);
                }
            }
        }
        "while_statement" => {
            if let Some(cond) = node.child_by_field_name("condition") {
                walk_expr_sites(cond, on_name, on_call);
            }
            if let Some(body) = node.child_by_field_name("body") {
                walk_stmt_sites(body, on_name, on_call, on_instantiate);
            }
        }
        "for_statement" => {
            if let Some(body) = node.child_by_field_name("body") {
                walk_stmt_sites(body, on_name, on_call, on_instantiate);
            }
        }
        "compound_statement" => walk_block_statements(node, on_name, on_call, on_instantiate),
        _ => {}
    }
}

fn walk_block_statements<'a>(
    block: Node<'a>,
    on_name: &mut impl FnMut(Node<'a>),
    on_call: &mut impl FnMut(Node<'a>),
    on_instantiate: &mut impl FnMut(Node<'a>),
) {
    let mut cursor = block.walk();
    for stmt in block.named_children(&mut cursor) {
        walk_stmt_sites(stmt, on_name, on_call, on_instantiate);
    }
}

/// Resolve a file's top-level functions/global variables into `EXPORTS`
/// edges from `module_symbol`, using `static`/`extern`/default linkage as a
/// rough proxy for "part of this translation unit's public surface":
/// `static` is internal linkage (excluded), anything else is treated as
/// exported. This is a proxy, not a precise match for C's actual visibility
/// rules (a non-`static` symbol with no matching header declaration is
/// still technically link-visible but not part of any documented API) but
/// it's the only linkage signal available at the syntax level, the same
/// posture Go's adapter takes with its uppercase-identifier EXPORTS proxy.
fn resolve_exports(
    file_entities: &[SyntaxEntity],
    file_edges: &[Edge],
    module_symbol: &SymbolEntity,
    symbol_table: &SymbolTable,
    static_names: &std::collections::HashSet<String>,
) -> Vec<Edge> {
    const EXPORTABLE_KINDS: [SyntaxKind; 2] = [SyntaxKind::Function, SyntaxKind::Variable];

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
            edges.push(Edge::new(module_symbol.id.clone(), symbol.id.clone(), EdgeKind::Exports));
        }
    }
    edges
}

/// Map a C type expression (a `type` field node, plus the accompanying
/// declarator for pointer-depth/function-pointer detection) to a `TypeRef`.
/// Not yet wired into `extract`/`resolve_file` -- `ACCEPTS`/`RETURNS` type
/// binding isn't called from the pipeline, mirroring the Go adapter's own
/// `extract_type` gap (tested directly, deliberately not yet wired in).
///
/// Function pointers (`void (*cb)(int)`) have no clean `TypeRef` fit: the
/// canonical model's `Reference` variant models a single level of
/// indirection to a *value* type, not "pointer to a callable with this
/// signature," and there is no `TypeRef::Function`/`Callable` variant to
/// carry a parameter/return-type list. Rather than force a lossy
/// approximation, a function-pointer declarator resolves to
/// `TypeRef::Unresolved` carrying the raw declarator text -- a documented
/// canonical-model gap, not a guessed-at shape.
#[allow(dead_code)]
fn extract_type(type_node: Node, declarator: Option<Node>, src: &str) -> TypeRef {
    let mut declarator = declarator;
    let mut pointers = 0usize;
    loop {
        match declarator.map(|d| d.kind()) {
            Some("pointer_declarator") => {
                pointers += 1;
                declarator = declarator.and_then(|d| d.child_by_field_name("declarator"));
            }
            Some("parenthesized_declarator") => {
                declarator = declarator.and_then(|d| d.named_child(0));
            }
            Some("function_declarator") => {
                // A `function_declarator` reached while walking a return
                // type's own declarator chain is usually the function's
                // *own* signature boundary (`int *f(void)`: the pointer
                // belongs to the return type, the function_declarator is
                // just `f`'s parameter list) -- recognized by its inner
                // declarator being a bare identifier, in which case this
                // isn't a function-pointer type at all and pointer-counting
                // simply stops here. Only when the inner declarator is
                // something else (typically a `parenthesized_declarator`,
                // `void (*cb)(int)`'s shape) is this genuinely a function
                // pointer, with no clean `TypeRef` fit -- see this
                // function's doc comment.
                let inner = declarator.and_then(|d| d.child_by_field_name("declarator"));
                if inner.map(|n| n.kind()) == Some("identifier") {
                    break;
                }
                return TypeRef::Unresolved(format!(
                    "function pointer: {}",
                    text(declarator.expect("checked Some above"), src)
                ));
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
        "struct_specifier" | "union_specifier" | "enum_specifier" => {
            let name = type_node
                .child_by_field_name("name")
                .map(|n| text(n, src).to_string())
                .unwrap_or_else(|| "anonymous".to_string());
            TypeRef::Named {
                name,
                type_args: Vec::new(),
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
        let adapter = CAdapter;
        let ast = adapter.parse(source).expect("valid C parses");
        adapter.extract(&ast, "module.c")
    }

    #[test]
    fn parses_valid_c_source() {
        let adapter = CAdapter;
        assert!(adapter.parse("int main(void) { return 0; }\n").is_ok());
    }

    #[test]
    fn extract_always_returns_the_file_module_first() {
        let (entities, _) = extract("int x;\n");
        assert_eq!(entities[0].kind, SyntaxKind::Module);
        assert_eq!(entities[0].name, "module.c");
    }

    #[test]
    fn extracts_a_top_level_function_with_its_parameters() {
        let (entities, edges) = extract("int add(int a, int b) {\n  return a + b;\n}\n");

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
    fn a_void_only_parameter_list_produces_no_parameters() {
        let (entities, _) = extract("int helper(void) {\n  return 0;\n}\n");
        assert!(!entities.iter().any(|e| e.kind == SyntaxKind::Parameter));
    }

    #[test]
    fn extracts_a_struct_with_a_field_declaration() {
        let (entities, edges) = extract("struct Circle {\n  double radius;\n};\n");
        let module = &entities[0];
        let circle = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Struct && e.name == "Circle")
            .expect("Struct entity");
        assert!(edges.contains(&Edge::new(module.id.clone(), circle.id.clone(), EdgeKind::Declares)));
    }

    #[test]
    fn extracts_a_union_as_a_struct_kind_entity() {
        let (entities, _) = extract("union U {\n  int i;\n  float f;\n};\n");
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Struct && e.name == "U"));
    }

    #[test]
    fn extracts_an_enum() {
        let (entities, _) = extract("enum Color { RED, GREEN, BLUE };\n");
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Enum && e.name == "Color"));
    }

    #[test]
    fn extracts_an_anonymous_typedef_struct_by_its_alias_name() {
        let (entities, _) = extract("typedef struct {\n  double radius;\n} Circle;\n");
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Struct && e.name == "Circle"));
    }

    #[test]
    fn a_tagged_typedef_struct_does_not_produce_a_duplicate_entity() {
        let (entities, _) = extract("typedef struct Circle {\n  double radius;\n} Circle;\n");
        let structs: Vec<&SyntaxEntity> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Struct && e.name == "Circle")
            .collect();
        assert_eq!(structs.len(), 1);
    }

    #[test]
    fn extracts_a_global_variable() {
        let (entities, _) = extract("int global_count;\n");
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Variable && e.name == "global_count"));
    }

    #[test]
    fn a_function_prototype_produces_no_entity() {
        let (entities, _) = extract("int add(int a, int b);\n");
        assert!(!entities.iter().any(|e| e.kind == SyntaxKind::Function));
        assert!(!entities.iter().any(|e| e.kind == SyntaxKind::Variable));
    }

    #[test]
    fn a_function_pointer_variable_is_extracted_by_name() {
        let (entities, _) = extract("void (*callback)(int);\n");
        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Variable && e.name == "callback"));
    }

    #[test]
    fn a_pointer_returning_prototype_produces_no_entity() {
        let (entities, _) = extract("int *make_ptr(void);\n");
        assert!(!entities.iter().any(|e| e.kind == SyntaxKind::Function));
        assert!(!entities.iter().any(|e| e.kind == SyntaxKind::Variable));
    }

    #[test]
    fn extracts_a_local_variable_declaration_as_a_variable() {
        let (entities, edges) =
            extract("void run(void) {\n  int total = 1;\n  total = 2;\n}\n");
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
    fn a_non_declaration_statement_becomes_a_generic_statement_entity() {
        let (entities, _) = extract("void run(void) {\n  helper();\n}\nvoid helper(void) {}\n");
        assert!(entities.iter().any(|e| e.kind == SyntaxKind::Statement));
    }

    // -- type extraction (not yet wired into extract/resolve_file) --

    fn parse_source(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_c::LANGUAGE.into()).unwrap();
        parser.parse(source, None).unwrap()
    }

    fn find_top_level<'a>(tree: &'a Tree, src: &'a str, kind: &str, name: &str) -> Node<'a> {
        let mut cursor = tree.root_node().walk();
        let found = tree.root_node().children(&mut cursor).find(|c| {
            c.kind() == kind
                && function_signature(c.child_by_field_name("declarator").unwrap_or(*c))
                    .map(|(n, _)| text(n, src) == name)
                    .unwrap_or(false)
        });
        found.unwrap_or_else(|| panic!("no {kind} named {name}"))
    }

    #[test]
    fn extracts_a_primitive_type() {
        let full = "int f(void) { return 0; }\n";
        let tree = parse_source(full);
        let node = find_top_level(&tree, full, "function_definition", "f");
        let type_node = node.child_by_field_name("type").unwrap();
        assert_eq!(
            extract_type(type_node, None, full),
            TypeRef::Primitive("int".to_string())
        );
    }

    #[test]
    fn extracts_a_named_struct_type() {
        let full = "struct Circle f(void) { struct Circle c; return c; }\n";
        let tree = parse_source(full);
        let node = find_top_level(&tree, full, "function_definition", "f");
        let type_node = node.child_by_field_name("type").unwrap();
        assert_eq!(
            extract_type(type_node, None, full),
            TypeRef::Named {
                name: "Circle".to_string(),
                type_args: Vec::new(),
            }
        );
    }

    #[test]
    fn extracts_a_pointer_type_as_a_reference() {
        let full = "int *f(void) { return 0; }\n";
        let tree = parse_source(full);
        let mut cursor = tree.root_node().walk();
        let node = tree
            .root_node()
            .children(&mut cursor)
            .find(|c| c.kind() == "function_definition")
            .unwrap();
        let type_node = node.child_by_field_name("type").unwrap();
        let declarator = node.child_by_field_name("declarator").unwrap();
        assert_eq!(
            extract_type(type_node, Some(declarator), full),
            TypeRef::Reference {
                mutable: true,
                inner: Box::new(TypeRef::Primitive("int".to_string())),
            }
        );
    }

    #[test]
    fn a_function_pointer_type_is_unresolved_not_guessed_at() {
        let full = "void (*callback)(int);\n";
        let tree = parse_source(full);
        let mut cursor = tree.root_node().walk();
        let decl = tree
            .root_node()
            .children(&mut cursor)
            .find(|c| c.kind() == "declaration")
            .unwrap();
        let type_node = decl.child_by_field_name("type").unwrap();
        let declarator = decl.child_by_field_name("declarator").unwrap();
        match extract_type(type_node, Some(declarator), full) {
            TypeRef::Unresolved(_) => {}
            other => panic!("expected Unresolved for a function pointer, got {other:?}"),
        }
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
        let adapter = CAdapter;
        let ast = adapter.parse(source).expect("valid C parses");
        let (entities, edges) = adapter.extract(&ast, "module.c");
        let module = entities[0].clone();
        let fn_entity = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == fn_name)
            .unwrap_or_else(|| panic!("no Function entity named {fn_name}"))
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
        let source = "void run(void) {\n  int value = 1;\n  value = 2;\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) = resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        let value_var = r.entities.iter().find(|e| e.kind == SyntaxKind::Variable).unwrap();
        let value_symbol = r.symbol_table.find_by_syntax_id(&value_var.id).unwrap();
        assert!(edges.contains(&Edge::new(r.fn_symbol.id.clone(), value_symbol.id.clone(), EdgeKind::References)));
    }

    #[test]
    fn a_reference_to_an_undefined_name_gets_an_explicit_placeholder() {
        let source = "void run(void) {\n  missing = 1;\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (symbols, edges) =
            resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        assert!(symbols.iter().any(|s| s.name.starts_with("unknown-local")));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::References && e.from == r.fn_symbol.id));
    }

    #[test]
    fn resolves_a_free_function_call() {
        let source = "void helper(void) {}\n\nvoid run(void) {\n  helper();\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) = resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        let helper = r
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "helper")
            .unwrap();
        let helper_symbol = r.symbol_table.find_by_syntax_id(&helper.id).unwrap();
        assert!(edges.contains(&Edge::new(r.fn_symbol.id.clone(), helper_symbol.id.clone(), EdgeKind::Calls)));
    }

    #[test]
    fn resolves_a_struct_typed_local_declaration_as_instantiation() {
        let source =
            "struct Circle {\n  double radius;\n};\n\nvoid run(void) {\n  struct Circle c = { 1.0 };\n}\n";
        let (tree, r) = setup(source, "run");
        let body = body_of(&tree, source, "run");

        let (_, edges) = resolve_body(body, source, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        let circle = r
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Struct && e.name == "Circle")
            .unwrap();
        let circle_symbol = r.symbol_table.find_by_syntax_id(&circle.id).unwrap();
        assert!(edges.contains(&Edge::new(r.fn_symbol.id.clone(), circle_symbol.id.clone(), EdgeKind::Instantiates)));
    }

    #[test]
    fn exports_a_default_linkage_function_but_not_a_static_one() {
        let source = "int public_fn(void) { return 0; }\nstatic int hidden_fn(void) { return 0; }\n";
        let adapter = CAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _edges) = adapter.extract(&ast, "module.c");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table.find_by_syntax_id(&entities[0].id).unwrap().clone();

        let mut sites = ResolutionSites::default();
        let mut file_entities = vec![entities[0].clone()];
        let mut file_edges = Vec::new();
        extract_items(
            ast.tree.root_node(),
            &ast.source,
            &entities[0],
            "module.c",
            &mut file_entities,
            &mut file_edges,
            &mut sites,
        );

        let export_edges =
            resolve_exports(&file_entities, &file_edges, &module_symbol, &symbol_table, &sites.static_names);

        let public_fn = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Function && e.name == "public_fn")
            .unwrap();
        let public_symbol = symbol_table.find_by_syntax_id(&public_fn.id).unwrap();
        assert_eq!(export_edges.len(), 1);
        assert!(export_edges.contains(&Edge::new(module_symbol.id.clone(), public_symbol.id.clone(), EdgeKind::Exports)));
    }

    #[test]
    fn produces_no_extends_or_implements_edges() {
        // C has no classes/interfaces/inheritance -- see the module doc
        // comment. This confirms the *absence* is deliberate, not merely
        // untested: even a fixture with a struct and functions that could
        // plausibly be mistaken for OOP shapes never produces EXTENDS or
        // IMPLEMENTS.
        let source =
            "struct Shape {\n  double area;\n};\n\ndouble compute(struct Shape s) {\n  return s.area;\n}\n";
        let adapter = CAdapter;
        let ast = adapter.parse(source).unwrap();
        let (_, edges) = adapter.extract(&ast, "module.c");
        assert!(!edges.iter().any(|e| e.kind == EdgeKind::Extends));
        assert!(!edges.iter().any(|e| e.kind == EdgeKind::Implements));
    }
}
