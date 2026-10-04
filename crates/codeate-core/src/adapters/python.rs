use rustpython_parser::{ast, Parse};

use crate::adapter::LanguageAdapter;
use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};
use crate::resolve::unresolved_symbol;
use crate::symbols::{ScopeTree, SymbolTable};
use crate::types::TypeRef;

/// Python's built-in types, treated as `TypeRef::Primitive` the same way
/// Rust's `i32`/`bool`/... are. `None` here represents `NoneType` (a
/// PEP 484 return/parameter annotation of `None`, not the *absence* of an
/// annotation -- an unannotated value is a separate, explicit case, not
/// silently folded into this).
///
/// Not yet wired into `extract`/`resolve_file` -- parameter/return type
/// binding (`ACCEPTS`/`RETURNS`) isn't called from the pipeline, mirroring
/// the Rust adapter's own `extract_signature_types`/`extract_struct_fields`
/// gap (tested directly, deliberately not yet wired in either).
#[allow(dead_code)]
const PRIMITIVE_TYPES: &[&str] = &["int", "float", "bool", "str", "bytes", "complex", "None"];

/// The Python language adapter: parses Python source via `rustpython-parser`
/// and maps it onto the canonical syntax model. Phase 4 shape only (parsing
/// + extraction); Phase 15's later tasks add resolution.
pub struct PythonAdapter;

impl LanguageAdapter for PythonAdapter {
    type Ast = ast::Suite;

    fn parse(&self, source: &str) -> Result<Self::Ast> {
        ast::Suite::parse(source, "<module>")
            .map_err(|err| Error::Message(format!("failed to parse Python source: {err}")))
    }

    fn extract(&self, ast: &Self::Ast, location: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
        let mut entities = Vec::new();
        let mut edges = Vec::new();

        let file_module = SyntaxEntity::new(SyntaxKind::Module, location, location);
        entities.push(file_module.clone());

        extract_items(
            ast,
            &file_module,
            location,
            &mut entities,
            &mut edges,
            false,
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
        // Re-derive this file's own entities (for scope-tree construction,
        // matching the Rust adapter's own `resolve_file`) -- `extract`
        // already returned them to the caller, only the raw `ast` crosses
        // this boundary.
        let (file_entities, _file_edges) = self.extract(ast, location);
        let scope_tree = ScopeTree::build(file_module, &file_entities, edges_so_far);

        let mut sites = ResolutionSites::default();
        collect_sites(ast, location, false, &mut sites);

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
                resolve_body(body, &fn_symbol, &scope_tree, scope, symbol_table);
            symbols.extend(body_symbols);
            edges.extend(body_edges);
        }

        for class_stmt in &sites.classes {
            let (extends_symbols, extends_edges) =
                resolve_extends(class_stmt, all_entities, symbol_table);
            symbols.extend(extends_symbols);
            edges.extend(extends_edges);
        }

        let (import_symbols, import_edges) =
            resolve_imports(ast, module_symbol, all_entities, symbol_table);
        symbols.extend(import_symbols);
        edges.extend(import_edges);

        edges.extend(resolve_exports(
            ast,
            module_symbol,
            all_entities,
            symbol_table,
        ));

        (symbols, edges)
    }
}

/// Sites `collect_sites` visits that `resolve_file` needs the raw AST for
/// (a function/method's own body statements, a class's base list) but that
/// have no place in `extract`'s plain `(Vec<SyntaxEntity>, Vec<Edge>)`.
/// Mirrors the Rust adapter's own `ResolutionSites`, and the same
/// traversal `extract_items` uses (top-level and class bodies only --
/// nested `def`/`class` inside a function body are not visited, matching
/// `extract_body`'s own documented gap).
#[derive(Default)]
struct ResolutionSites<'a> {
    bodies: Vec<(SyntaxEntity, &'a [ast::Stmt])>,
    classes: Vec<&'a ast::StmtClassDef>,
}

fn collect_sites<'a>(
    stmts: &'a [ast::Stmt],
    scope: &str,
    in_class: bool,
    sites: &mut ResolutionSites<'a>,
) {
    for stmt in stmts {
        match stmt {
            ast::Stmt::FunctionDef(f) => {
                let kind = if in_class {
                    SyntaxKind::Method
                } else {
                    SyntaxKind::Function
                };
                let entity = SyntaxEntity::new(kind, scope, f.name.as_str());
                sites.bodies.push((entity, f.body.as_slice()));
            }
            ast::Stmt::AsyncFunctionDef(f) => {
                let kind = if in_class {
                    SyntaxKind::Method
                } else {
                    SyntaxKind::Function
                };
                let entity = SyntaxEntity::new(kind, scope, f.name.as_str());
                sites.bodies.push((entity, f.body.as_slice()));
            }
            ast::Stmt::ClassDef(c) => {
                sites.classes.push(c);
                let class_scope = child_scope(scope, c.name.as_str());
                collect_sites(&c.body, &class_scope, true, sites);
            }
            _ => {}
        }
    }
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

/// Record `parent CONTAINS child` only (parameters, variables, statements --
/// contained by their function/method but not top-level declarations), then
/// push `child` into `entities`.
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

fn child_scope(scope: &str, name: &str) -> String {
    format!("{scope}::{name}")
}

/// Map a PEP 484-style annotation expression to a `TypeRef`. Mirrors the
/// Rust adapter's own `extract_type`: a bare name is `Primitive` (if it's
/// one of Python's built-ins) or `Named` otherwise; `Foo[Bar, Baz]`
/// (`typing.List[int]`, `dict[str, int]`, ...) is `Named` with `type_args`
/// from the subscript; anything else keeps its `Debug` form as `Unresolved`
/// rather than being dropped (`rustpython-ast` has no source-text unparser
/// the way `syn`+`quote` gives the Rust adapter, so this is coarser than
/// Rust's raw-source-text `Unresolved`, but still explicit and inspectable).
#[allow(dead_code)]
fn extract_type(expr: &ast::Expr) -> TypeRef {
    match expr {
        ast::Expr::Name(name) => {
            let id = name.id.as_str();
            if PRIMITIVE_TYPES.contains(&id) {
                TypeRef::Primitive(id.to_string())
            } else {
                TypeRef::Named {
                    name: id.to_string(),
                    type_args: Vec::new(),
                }
            }
        }
        ast::Expr::Constant(constant) if matches!(constant.value, ast::Constant::None) => {
            TypeRef::Primitive("None".to_string())
        }
        ast::Expr::Attribute(attr) => TypeRef::Named {
            name: attr.attr.as_str().to_string(),
            type_args: Vec::new(),
        },
        ast::Expr::Subscript(subscript) => {
            let name = match subscript.value.as_ref() {
                ast::Expr::Name(name) => name.id.as_str().to_string(),
                ast::Expr::Attribute(attr) => attr.attr.as_str().to_string(),
                other => return TypeRef::Unresolved(format!("{other:?}")),
            };
            let type_args = match subscript.slice.as_ref() {
                ast::Expr::Tuple(tuple) => tuple.elts.iter().map(extract_type).collect(),
                other => vec![extract_type(other)],
            };
            TypeRef::Named { name, type_args }
        }
        other => TypeRef::Unresolved(format!("{other:?}")),
    }
}

/// Walk item-level statements (`class`/`def` -- a Python module or class
/// body): `class` becomes an Interface-less `Class` entity (Python has no
/// separate interface construct; an ABC is still just a `Class`), whose
/// nested `def`s become `Method` entities; a top-level `def` becomes a
/// `Function` entity. Everything else at this level (module-level
/// assignments/imports/statements) is not extracted here -- only
/// `extract_body` (function/method bodies) captures statement-level detail,
/// matching the Rust adapter's own item-vs-body split.
fn extract_items(
    stmts: &[ast::Stmt],
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    in_class: bool,
) {
    for stmt in stmts {
        match stmt {
            ast::Stmt::FunctionDef(f) => extract_function(
                f.name.as_str(),
                &f.args,
                &f.body,
                parent,
                scope,
                entities,
                edges,
                in_class,
            ),
            ast::Stmt::AsyncFunctionDef(f) => extract_function(
                f.name.as_str(),
                &f.args,
                &f.body,
                parent,
                scope,
                entities,
                edges,
                in_class,
            ),
            ast::Stmt::ClassDef(c) => {
                let entity = SyntaxEntity::new(SyntaxKind::Class, scope, c.name.as_str());
                let class_scope = child_scope(scope, c.name.as_str());
                declare(parent, entity.clone(), entities, edges);
                extract_items(&c.body, &entity, &class_scope, entities, edges, true);
            }
            _ => {}
        }
    }
}

/// A function/method declaration: its own entity, parameters (positional-only
/// and positional-or-keyword only -- `*args`/`**kwargs`/keyword-only params
/// are a documented gap, left for a later task rather than guessed at), and
/// its body. `in_class` methods have their first parameter (`self`/`cls`,
/// conventional, not enforced by the language) excluded, matching the Rust
/// adapter's own `self`-exclusion for impl methods.
#[allow(clippy::too_many_arguments)]
fn extract_function(
    name: &str,
    args: &ast::Arguments,
    body: &[ast::Stmt],
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
    in_class: bool,
) {
    let kind = if in_class {
        SyntaxKind::Method
    } else {
        SyntaxKind::Function
    };
    let entity = SyntaxEntity::new(kind, scope, name);
    let fn_scope = child_scope(scope, name);
    declare(parent, entity.clone(), entities, edges);

    let params: Vec<&ast::ArgWithDefault> =
        args.posonlyargs.iter().chain(args.args.iter()).collect();
    for (index, param) in params.iter().enumerate() {
        if in_class && index == 0 {
            continue;
        }
        let param_entity =
            SyntaxEntity::new(SyntaxKind::Parameter, &fn_scope, param.def.arg.as_str());
        contain(&entity, param_entity, entities, edges);
    }

    extract_body(body, &entity, &fn_scope, entities, edges);
}

/// Walk a function/method body's direct statements (not nested into
/// if/for/while/... blocks, matching the Rust adapter's own scope): a simple
/// `name = ...`/`name: T = ...` assignment becomes a `Variable` entity;
/// everything else becomes a generic `Statement` entity. A nested `def`/
/// `class` inside a body is not extracted (a documented gap, same as the
/// Rust adapter not walking into nested item definitions).
fn extract_body(
    stmts: &[ast::Stmt],
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    for (index, stmt) in stmts.iter().enumerate() {
        match stmt {
            ast::Stmt::Assign(assign) => {
                for target in &assign.targets {
                    if let ast::Expr::Name(name) = target {
                        let entity = SyntaxEntity::new(
                            SyntaxKind::Variable,
                            &format!("{scope}#{index}"),
                            name.id.as_str(),
                        );
                        contain(parent, entity, entities, edges);
                    }
                }
            }
            ast::Stmt::AnnAssign(assign) => {
                if let ast::Expr::Name(name) = assign.target.as_ref() {
                    let entity = SyntaxEntity::new(
                        SyntaxKind::Variable,
                        &format!("{scope}#{index}"),
                        name.id.as_str(),
                    );
                    contain(parent, entity, entities, edges);
                }
            }
            ast::Stmt::FunctionDef(_) | ast::Stmt::AsyncFunctionDef(_) | ast::Stmt::ClassDef(_) => {
            }
            _ => {
                let entity =
                    SyntaxEntity::new(SyntaxKind::Statement, scope, format!("stmt_{index}"));
                contain(parent, entity, entities, edges);
            }
        }
    }
}

/// Walk `expr`'s common sub-expression forms, invoking `on_name` at every
/// `Name` read (`Load` context -- an assignment target's `Store`/`Del`
/// context is a binding site, not a usage, so it's not a reference) and
/// `on_call` at every call site (still recursing into the call's own
/// `func`/`args`, so `helper()` fires both a reference to `helper` and a
/// call to it -- mirrors the Rust adapter's own `walk_expr`, which does the
/// same for `syn::Expr::Call`). Rarer expression forms (lambdas,
/// comprehensions, f-strings, slices, `yield`) are not walked into -- a
/// documented gap, not a silent behavior difference from what's tested.
fn walk_expr_sites<'a>(
    expr: &'a ast::Expr,
    on_name: &mut impl FnMut(&'a ast::ExprName),
    on_call: &mut impl FnMut(&'a ast::ExprCall),
) {
    match expr {
        ast::Expr::Name(name) if matches!(name.ctx, ast::ExprContext::Load) => on_name(name),
        ast::Expr::Name(_) => {}
        ast::Expr::Call(call) => {
            on_call(call);
            walk_expr_sites(&call.func, on_name, on_call);
            for arg in &call.args {
                walk_expr_sites(arg, on_name, on_call);
            }
        }
        ast::Expr::Attribute(attr) => walk_expr_sites(&attr.value, on_name, on_call),
        ast::Expr::BinOp(b) => {
            walk_expr_sites(&b.left, on_name, on_call);
            walk_expr_sites(&b.right, on_name, on_call);
        }
        ast::Expr::UnaryOp(u) => walk_expr_sites(&u.operand, on_name, on_call),
        ast::Expr::BoolOp(b) => {
            for value in &b.values {
                walk_expr_sites(value, on_name, on_call);
            }
        }
        ast::Expr::Compare(c) => {
            walk_expr_sites(&c.left, on_name, on_call);
            for comparator in &c.comparators {
                walk_expr_sites(comparator, on_name, on_call);
            }
        }
        ast::Expr::IfExp(i) => {
            walk_expr_sites(&i.test, on_name, on_call);
            walk_expr_sites(&i.body, on_name, on_call);
            walk_expr_sites(&i.orelse, on_name, on_call);
        }
        ast::Expr::List(l) => {
            for elt in &l.elts {
                walk_expr_sites(elt, on_name, on_call);
            }
        }
        ast::Expr::Tuple(t) => {
            for elt in &t.elts {
                walk_expr_sites(elt, on_name, on_call);
            }
        }
        ast::Expr::Set(s) => {
            for elt in &s.elts {
                walk_expr_sites(elt, on_name, on_call);
            }
        }
        ast::Expr::Dict(d) => {
            for key in d.keys.iter().flatten() {
                walk_expr_sites(key, on_name, on_call);
            }
            for value in &d.values {
                walk_expr_sites(value, on_name, on_call);
            }
        }
        ast::Expr::Starred(s) => walk_expr_sites(&s.value, on_name, on_call),
        ast::Expr::Await(a) => walk_expr_sites(&a.value, on_name, on_call),
        ast::Expr::Subscript(s) => {
            walk_expr_sites(&s.value, on_name, on_call);
            walk_expr_sites(&s.slice, on_name, on_call);
        }
        ast::Expr::NamedExpr(n) => walk_expr_sites(&n.value, on_name, on_call),
        _ => {}
    }
}

/// Like `walk_expr_sites`, but over a function/method body's direct
/// statements (not nested into deeper control flow beyond common
/// if/while/for/with, matching the Rust adapter's own scope).
fn walk_stmt_sites<'a>(
    stmt: &'a ast::Stmt,
    on_name: &mut impl FnMut(&'a ast::ExprName),
    on_call: &mut impl FnMut(&'a ast::ExprCall),
) {
    match stmt {
        ast::Stmt::Expr(e) => walk_expr_sites(&e.value, on_name, on_call),
        ast::Stmt::Return(r) => {
            if let Some(value) = &r.value {
                walk_expr_sites(value, on_name, on_call);
            }
        }
        ast::Stmt::Assign(a) => walk_expr_sites(&a.value, on_name, on_call),
        ast::Stmt::AnnAssign(a) => {
            if let Some(value) = &a.value {
                walk_expr_sites(value, on_name, on_call);
            }
        }
        ast::Stmt::AugAssign(a) => walk_expr_sites(&a.value, on_name, on_call),
        ast::Stmt::If(i) => {
            walk_expr_sites(&i.test, on_name, on_call);
            for stmt in &i.body {
                walk_stmt_sites(stmt, on_name, on_call);
            }
            for stmt in &i.orelse {
                walk_stmt_sites(stmt, on_name, on_call);
            }
        }
        ast::Stmt::While(w) => {
            walk_expr_sites(&w.test, on_name, on_call);
            for stmt in &w.body {
                walk_stmt_sites(stmt, on_name, on_call);
            }
            for stmt in &w.orelse {
                walk_stmt_sites(stmt, on_name, on_call);
            }
        }
        ast::Stmt::For(f) => {
            walk_expr_sites(&f.iter, on_name, on_call);
            for stmt in &f.body {
                walk_stmt_sites(stmt, on_name, on_call);
            }
            for stmt in &f.orelse {
                walk_stmt_sites(stmt, on_name, on_call);
            }
        }
        ast::Stmt::With(w) => {
            for item in &w.items {
                walk_expr_sites(&item.context_expr, on_name, on_call);
            }
            for stmt in &w.body {
                walk_stmt_sites(stmt, on_name, on_call);
            }
        }
        _ => {}
    }
}

/// `REFERENCES` edges: every `Load`-context `Name` in `stmts` resolved via
/// `scope_tree`; an undefined name gets an explicit `unknown-local`
/// placeholder (never silently dropped).
fn resolve_references(
    stmts: &[ast::Stmt],
    fn_symbol: &SymbolEntity,
    scope_tree: &ScopeTree,
    scope: usize,
    symbol_table: &SymbolTable,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for stmt in stmts {
        walk_stmt_sites(
            stmt,
            &mut |name: &ast::ExprName| {
                let ident = name.id.as_str();
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
            },
            &mut |_: &ast::ExprCall| {},
        );
    }

    (symbols, edges)
}

/// `CALLS` edges: a bare-name call (`helper()`) resolves via `scope_tree`; a
/// method call (`obj.method()`) resolves by best-effort name match against
/// any `Method` symbol in the file (full receiver-type dispatch needs a
/// structural-relationships stage, out of scope here, same as the Rust
/// adapter); any other callee form gets an explicit `unsupported-form`
/// placeholder.
fn resolve_calls(
    stmts: &[ast::Stmt],
    fn_symbol: &SymbolEntity,
    scope_tree: &ScopeTree,
    scope: usize,
    symbol_table: &SymbolTable,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for stmt in stmts {
        walk_stmt_sites(
            stmt,
            &mut |_: &ast::ExprName| {},
            &mut |call: &ast::ExprCall| match call.func.as_ref() {
                ast::Expr::Name(name) => {
                    let ident = name.id.as_str();
                    let resolved = scope_tree
                        .lookup(scope, ident)
                        .and_then(|id| symbol_table.find_by_syntax_id(id))
                        .cloned()
                        .or_else(|| {
                            // The scope tree only covers this file's own
                            // entities, so a call to a function declared in
                            // another module never resolves through it --
                            // fall back to a repo-wide best-effort name
                            // match against `symbol_table`, mirroring the
                            // `Attribute` branch below, which has always
                            // searched repo-wide.
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
                ast::Expr::Attribute(attr) => {
                    let ident = attr.attr.as_str();
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
            },
        );
    }

    (symbols, edges)
}

/// `INSTANTIATES` edges: a bare-name call whose callee resolves in scope to
/// a `Type` symbol (a class) -- an unrecognized/unresolved call is left to
/// `resolve_calls` rather than guessed at here, matching the Rust adapter's
/// own tuple-struct-constructor rule.
fn resolve_instantiates(
    stmts: &[ast::Stmt],
    fn_symbol: &SymbolEntity,
    scope_tree: &ScopeTree,
    scope: usize,
    symbol_table: &SymbolTable,
) -> Vec<Edge> {
    let mut edges = Vec::new();

    for stmt in stmts {
        walk_stmt_sites(
            stmt,
            &mut |_: &ast::ExprName| {},
            &mut |call: &ast::ExprCall| {
                let ast::Expr::Name(name) = call.func.as_ref() else {
                    return;
                };
                let ident = name.id.as_str();
                let Some(symbol) = scope_tree
                    .lookup(scope, ident)
                    .and_then(|id| symbol_table.find_by_syntax_id(id))
                else {
                    return;
                };
                if symbol.kind == SymbolKind::Type {
                    edges.push(Edge::new(
                        fn_symbol.id.clone(),
                        symbol.id.clone(),
                        EdgeKind::Instantiates,
                    ));
                }
            },
        );
    }

    edges
}

/// Run reference, call, and instantiation resolution over one function/
/// method body in a single pass, combining their symbols/edges.
fn resolve_body(
    stmts: &[ast::Stmt],
    fn_symbol: &SymbolEntity,
    scope_tree: &ScopeTree,
    scope: usize,
    symbol_table: &SymbolTable,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    let (ref_symbols, ref_edges) =
        resolve_references(stmts, fn_symbol, scope_tree, scope, symbol_table);
    symbols.extend(ref_symbols);
    edges.extend(ref_edges);

    let (call_symbols, call_edges) =
        resolve_calls(stmts, fn_symbol, scope_tree, scope, symbol_table);
    symbols.extend(call_symbols);
    edges.extend(call_edges);

    edges.extend(resolve_instantiates(
        stmts,
        fn_symbol,
        scope_tree,
        scope,
        symbol_table,
    ));

    (symbols, edges)
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

/// `EXTENDS` edges from `class_stmt`'s own `Class` symbol to each of its
/// base classes (Python has no separate interface/trait construct, so a
/// base is always another `Class`, unlike Rust's `IMPLEMENTS`-to-an-
/// `Interface`). A base that isn't a bare name (a `Subscript` for
/// `Generic[T]`, a keyword like `metaclass=...`) is skipped -- a documented
/// gap, not guessed at. A base outside this repository (e.g. a builtin like
/// `Exception`) resolves to an explicit `external-class` placeholder.
fn resolve_extends(
    class_stmt: &ast::StmtClassDef,
    entities: &[SyntaxEntity],
    symbol_table: &SymbolTable,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    let Some(class_symbol) =
        find_entity_by_kinds_name(entities, &[SyntaxKind::Class], class_stmt.name.as_str())
            .and_then(|e| symbol_table.find_by_syntax_id(&e.id))
            .cloned()
    else {
        return (symbols, edges);
    };

    for base in &class_stmt.bases {
        let ast::Expr::Name(base_name) = base else {
            continue;
        };
        let name = base_name.id.as_str();
        let base_symbol = find_entity_by_kinds_name(entities, &[SyntaxKind::Class], name)
            .and_then(|e| symbol_table.find_by_syntax_id(&e.id))
            .cloned()
            .unwrap_or_else(|| {
                let placeholder = unresolved_symbol("external-class", name);
                symbols.push(placeholder.clone());
                placeholder
            });
        edges.push(Edge::new(
            class_symbol.id.clone(),
            base_symbol.id,
            EdgeKind::Extends,
        ));
    }

    (symbols, edges)
}

/// Resolve a module's top-level `import`/`from ... import ...` statements
/// into `IMPORTS` edges from `module_symbol`. Python has no `crate::`/
/// `self::`-style marker distinguishing a repository-local import from an
/// external one the way Rust's `use` paths do, so `from X import name`
/// (any `X`, relative or absolute) is resolved by searching this
/// repository's own declared `Class`/`Function` entities for `name` first;
/// only when nothing matches does it fall back to an explicit
/// `external-module` placeholder. A bare `import X` (importing the module
/// object itself, not a specific declared name) always resolves to an
/// `external-module` placeholder -- Codeate doesn't yet model "a module as
/// an importable value" distinctly from the `Module` entity itself, a
/// documented gap.
fn resolve_imports(
    stmts: &[ast::Stmt],
    module_symbol: &SymbolEntity,
    entities: &[SyntaxEntity],
    symbol_table: &SymbolTable,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    const IMPORTABLE_KINDS: [SyntaxKind; 2] = [SyntaxKind::Class, SyntaxKind::Function];
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for stmt in stmts {
        match stmt {
            ast::Stmt::Import(import) => {
                for alias in &import.names {
                    let placeholder = unresolved_symbol("external-module", alias.name.as_str());
                    symbols.push(placeholder.clone());
                    edges.push(Edge::new(
                        module_symbol.id.clone(),
                        placeholder.id,
                        EdgeKind::Imports,
                    ));
                }
            }
            ast::Stmt::ImportFrom(import_from) => {
                let module_path = import_from
                    .module
                    .as_ref()
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default();
                for alias in &import_from.names {
                    let name = alias.name.as_str();
                    let joined = if module_path.is_empty() {
                        name.to_string()
                    } else {
                        format!("{module_path}.{name}")
                    };
                    if name == "*" {
                        let placeholder = unresolved_symbol("glob-import", &joined);
                        symbols.push(placeholder.clone());
                        edges.push(Edge::new(
                            module_symbol.id.clone(),
                            placeholder.id,
                            EdgeKind::Imports,
                        ));
                        continue;
                    }
                    let target = find_entity_by_kinds_name(entities, &IMPORTABLE_KINDS, name)
                        .and_then(|e| symbol_table.find_by_syntax_id(&e.id))
                        .cloned()
                        .unwrap_or_else(|| {
                            let placeholder = unresolved_symbol("external-module", &joined);
                            symbols.push(placeholder.clone());
                            placeholder
                        });
                    edges.push(Edge::new(
                        module_symbol.id.clone(),
                        target.id,
                        EdgeKind::Imports,
                    ));
                }
            }
            _ => {}
        }
    }

    (symbols, edges)
}

/// Resolve a module's implicitly-exported top-level `class`/`def`s into
/// `EXPORTS` edges from `module_symbol`: if the module defines `__all__ =
/// [...]`, exactly the named items are exported; otherwise every top-level
/// item whose name doesn't start with `_` is (Python's actual public-API
/// convention -- there's no `pub` keyword to inspect the way Rust has).
fn resolve_exports(
    stmts: &[ast::Stmt],
    module_symbol: &SymbolEntity,
    entities: &[SyntaxEntity],
    symbol_table: &SymbolTable,
) -> Vec<Edge> {
    let mut edges = Vec::new();

    let all_list: Option<Vec<String>> = stmts.iter().find_map(|stmt| {
        let ast::Stmt::Assign(assign) = stmt else {
            return None;
        };
        let is_all = assign
            .targets
            .iter()
            .any(|target| matches!(target, ast::Expr::Name(n) if n.id.as_str() == "__all__"));
        if !is_all {
            return None;
        }
        let elts = match assign.value.as_ref() {
            ast::Expr::List(list) => &list.elts,
            ast::Expr::Tuple(tuple) => &tuple.elts,
            _ => return None,
        };
        Some(
            elts.iter()
                .filter_map(|elt| match elt {
                    ast::Expr::Constant(c) => match &c.value {
                        ast::Constant::Str(s) => Some(s.clone()),
                        _ => None,
                    },
                    _ => None,
                })
                .collect(),
        )
    });

    for stmt in stmts {
        let (kind, name) = match stmt {
            ast::Stmt::FunctionDef(f) => (SyntaxKind::Function, f.name.as_str().to_string()),
            ast::Stmt::AsyncFunctionDef(f) => (SyntaxKind::Function, f.name.as_str().to_string()),
            ast::Stmt::ClassDef(c) => (SyntaxKind::Class, c.name.as_str().to_string()),
            _ => continue,
        };

        let exported = match &all_list {
            Some(names) => names.contains(&name),
            None => !name.starts_with('_'),
        };
        if !exported {
            continue;
        }

        if let Some(symbol) = find_entity_by_kinds_name(entities, &[kind], &name)
            .and_then(|e| symbol_table.find_by_syntax_id(&e.id))
        {
            edges.push(Edge::new(
                module_symbol.id.clone(),
                symbol.id.clone(),
                EdgeKind::Exports,
            ));
        }
    }

    edges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
        let adapter = PythonAdapter;
        let ast = adapter.parse(source).expect("valid python parses");
        adapter.extract(&ast, "module.py")
    }

    fn parse_annotation(source: &str) -> ast::Expr {
        ast::Expr::parse(source, "<annotation>").expect("valid annotation expression")
    }

    #[test]
    fn parses_valid_python_source() {
        let adapter = PythonAdapter;
        assert!(adapter.parse("def run():\n    pass\n").is_ok());
    }

    #[test]
    fn parsing_invalid_syntax_is_a_clear_error() {
        let adapter = PythonAdapter;
        let err = adapter
            .parse("def run(:\n")
            .expect_err("invalid syntax should error");
        assert!(err.to_string().contains("Python"));
    }

    #[test]
    fn extract_always_returns_the_file_module_first() {
        let (entities, _) = extract("x = 1\n");
        assert_eq!(entities[0].kind, SyntaxKind::Module);
        assert_eq!(entities[0].name, "module.py");
    }

    #[test]
    fn extracts_a_top_level_function_with_its_parameters() {
        let (entities, edges) = extract("def add(a, b):\n    return a + b\n");

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
    fn extracts_a_class_with_a_method_excluding_self() {
        let (entities, edges) =
            extract("class Circle:\n    def area(self, factor):\n        return factor\n");

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

        let param_names: Vec<&str> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Parameter)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(param_names, vec!["factor"], "self should be excluded");
    }

    #[test]
    fn extracts_assignments_in_a_function_body_as_variables() {
        let (entities, edges) = extract("def run():\n    total = 1\n    return total\n");

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
        // Variables are contained, not declared (matching the Rust adapter).
        assert!(!edges.contains(&Edge::new(
            function.id.clone(),
            variable.id.clone(),
            EdgeKind::Declares
        )));
    }

    #[test]
    fn extracts_an_annotated_assignment_as_a_variable() {
        let (entities, _) = extract("def run():\n    total: int = 1\n    return total\n");

        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Variable && e.name == "total"));
    }

    #[test]
    fn a_non_assignment_statement_becomes_a_generic_statement_entity() {
        let (entities, _) = extract("def run():\n    print(1)\n");

        assert!(entities.iter().any(|e| e.kind == SyntaxKind::Statement));
    }

    #[test]
    fn async_functions_are_extracted_like_regular_functions() {
        let (entities, _) = extract("async def run():\n    pass\n");

        assert!(entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Function && e.name == "run"));
    }

    #[test]
    fn same_named_function_in_a_class_and_at_module_level_gets_distinct_ids() {
        let (entities, _) =
            extract("def run():\n    pass\n\nclass Runner:\n    def run(self):\n        pass\n");

        let functions: Vec<&SyntaxEntity> = entities
            .iter()
            .filter(|e| {
                e.name == "run" && (e.kind == SyntaxKind::Function || e.kind == SyntaxKind::Method)
            })
            .collect();
        assert_eq!(functions.len(), 2);
        assert_ne!(functions[0].id, functions[1].id);
    }

    #[test]
    fn extracts_a_primitive_type() {
        assert_eq!(
            extract_type(&parse_annotation("int")),
            TypeRef::Primitive("int".to_string())
        );
        assert_eq!(
            extract_type(&parse_annotation("None")),
            TypeRef::Primitive("None".to_string())
        );
    }

    #[test]
    fn extracts_a_user_defined_named_type() {
        assert_eq!(
            extract_type(&parse_annotation("Circle")),
            TypeRef::Named {
                name: "Circle".to_string(),
                type_args: Vec::new(),
            }
        );
    }

    #[test]
    fn extracts_a_generic_named_type_with_its_type_args() {
        assert_eq!(
            extract_type(&parse_annotation("List[int]")),
            TypeRef::Named {
                name: "List".to_string(),
                type_args: vec![TypeRef::Primitive("int".to_string())],
            }
        );
    }

    #[test]
    fn extracts_a_generic_type_with_multiple_type_args_from_a_tuple_subscript() {
        assert_eq!(
            extract_type(&parse_annotation("Dict[str, int]")),
            TypeRef::Named {
                name: "Dict".to_string(),
                type_args: vec![
                    TypeRef::Primitive("str".to_string()),
                    TypeRef::Primitive("int".to_string()),
                ],
            }
        );
    }

    #[test]
    fn extracts_a_dotted_attribute_type_by_its_final_segment() {
        assert_eq!(
            extract_type(&parse_annotation("typing.Optional")),
            TypeRef::Named {
                name: "Optional".to_string(),
                type_args: Vec::new(),
            }
        );
    }

    #[test]
    fn an_unresolvable_annotation_still_produces_an_explicit_unresolved_type() {
        // `int | str` (PEP 604 union syntax) isn't mapped to a structured
        // TypeRef -- it still gets an explicit, present Unresolved rather
        // than being silently dropped.
        assert!(matches!(
            extract_type(&parse_annotation("int | str")),
            TypeRef::Unresolved(_)
        ));
    }

    fn body_of<'a>(ast: &'a ast::Suite, name: &str) -> &'a [ast::Stmt] {
        for stmt in ast {
            match stmt {
                ast::Stmt::FunctionDef(f) if f.name.as_str() == name => return &f.body,
                ast::Stmt::ClassDef(c) => {
                    for inner in &c.body {
                        if let ast::Stmt::FunctionDef(f) = inner {
                            if f.name.as_str() == name {
                                return &f.body;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        panic!("no function named {name}");
    }

    /// Common setup for the reference/call/instantiate tests below: parse,
    /// extract, build a scope tree + symbol table, and resolve `fn_name`'s
    /// own symbol -- everything a resolver needs except the body itself
    /// (fetched per test via `body_of`, since which function's body is
    /// under test varies).
    struct Resolved {
        entities: Vec<SyntaxEntity>,
        scope_tree: ScopeTree,
        scope: usize,
        symbol_table: SymbolTable,
        fn_symbol: SymbolEntity,
    }

    fn setup(source: &str, fn_name: &str) -> (ast::Suite, Resolved) {
        let adapter = PythonAdapter;
        let ast = adapter.parse(source).expect("valid python parses");
        let (entities, edges) = adapter.extract(&ast, "module.py");
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
            ast,
            Resolved {
                entities,
                scope_tree,
                scope,
                symbol_table,
                fn_symbol,
            },
        )
    }

    #[test]
    fn resolves_a_reference_to_an_in_scope_variable() {
        let source = "def run():\n    value = 1\n    value\n";
        let (ast, r) = setup(source, "run");
        let body = body_of(&ast, "run");

        let (_, edges) =
            resolve_references(body, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

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
        let source = "def run():\n    missing\n";
        let (ast, r) = setup(source, "run");
        let body = body_of(&ast, "run");

        let (symbols, edges) =
            resolve_references(body, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        assert!(symbols.iter().any(|s| s.name.starts_with("unknown-local")));
        assert!(edges
            .iter()
            .any(|e| e.kind == EdgeKind::References && e.from == r.fn_symbol.id));
    }

    #[test]
    fn resolves_a_free_function_call() {
        let source = "def helper():\n    pass\n\n\ndef run():\n    helper()\n";
        let (ast, r) = setup(source, "run");
        let body = body_of(&ast, "run");

        let (_, edges) = resolve_calls(body, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

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
        // another module fell straight to an `unknown-local` placeholder.
        use crate::symbols::build_symbol_table;

        let adapter = PythonAdapter;
        let helper_ast = adapter.parse("def helper():\n    pass\n").unwrap();
        let (helper_entities, helper_edges) = adapter.extract(&helper_ast, "helper.py");
        let caller_ast = adapter.parse("def run():\n    helper()\n").unwrap();
        let (caller_entities, caller_edges) = adapter.extract(&caller_ast, "run.py");

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
            "run.py",
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
    fn a_call_to_an_unsupported_callee_form_gets_an_explicit_placeholder() {
        let source = "def run():\n    foo()()\n";
        let (ast, r) = setup(source, "run");
        let body = body_of(&ast, "run");

        let (symbols, edges) =
            resolve_calls(body, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        assert!(symbols
            .iter()
            .any(|s| s.name.starts_with("unsupported-form")));
        assert!(edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.from == r.fn_symbol.id));
    }

    #[test]
    fn resolves_a_method_call_by_best_effort_name_match() {
        let source =
            "class Shape:\n    def area(self):\n        pass\n\n\ndef run(shape):\n    shape.area()\n";
        let (ast, r) = setup(source, "run");
        let body = body_of(&ast, "run");

        let (_, edges) = resolve_calls(body, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

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
    fn resolves_a_class_instantiation_call() {
        let source = "class Circle:\n    pass\n\n\ndef run():\n    Circle()\n";
        let (ast, r) = setup(source, "run");
        let body = body_of(&ast, "run");

        let edges =
            resolve_instantiates(body, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

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
    fn resolve_body_produces_the_full_reference_call_and_instantiate_edge_set() {
        let source = "class Circle:\n    pass\n\n\ndef helper():\n    pass\n\n\ndef run():\n    value = Circle()\n    helper()\n    missing\n";
        let (ast, r) = setup(source, "run");
        let body = body_of(&ast, "run");

        let (symbols, edges) =
            resolve_body(body, &r.fn_symbol, &r.scope_tree, r.scope, &r.symbol_table);

        assert!(edges.iter().any(|e| e.kind == EdgeKind::References));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Calls));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Instantiates));
        assert!(symbols.iter().any(|s| s.name.starts_with("unknown-local")));
    }

    fn class_def<'a>(ast: &'a ast::Suite, name: &str) -> &'a ast::StmtClassDef {
        ast.iter()
            .find_map(|stmt| match stmt {
                ast::Stmt::ClassDef(c) if c.name.as_str() == name => Some(c),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no class named {name}"))
    }

    #[test]
    fn resolves_extends_from_a_local_base_class() {
        let source = "class Shape:\n    pass\n\n\nclass Circle(Shape):\n    pass\n";
        let adapter = PythonAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _) = adapter.extract(&ast, "module.py");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let (_, edges) = resolve_extends(class_def(&ast, "Circle"), &entities, &symbol_table);

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
        let source = "class Custom(Exception):\n    pass\n";
        let adapter = PythonAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _) = adapter.extract(&ast, "module.py");
        let symbol_table = crate::symbols::build_symbol_table(&entities);

        let (symbols, edges) = resolve_extends(class_def(&ast, "Custom"), &entities, &symbol_table);

        assert!(symbols.iter().any(|s| s.name.starts_with("external-class")));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Extends));
    }

    #[test]
    fn resolves_a_from_import_against_a_locally_declared_item() {
        let source = "class Shape:\n    pass\n";
        let adapter = PythonAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _) = adapter.extract(&ast, "module.py");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table
            .find_by_syntax_id(&entities[0].id)
            .unwrap()
            .clone();

        let import_source = "from shapes import Shape\n";
        let import_ast = adapter.parse(import_source).unwrap();
        let (_, edges) = resolve_imports(&import_ast, &module_symbol, &entities, &symbol_table);

        let shape = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class)
            .unwrap();
        let shape_symbol = symbol_table.find_by_syntax_id(&shape.id).unwrap();
        assert!(edges.contains(&Edge::new(
            module_symbol.id.clone(),
            shape_symbol.id.clone(),
            EdgeKind::Imports
        )));
    }

    #[test]
    fn an_unresolvable_from_import_gets_an_explicit_placeholder() {
        let adapter = PythonAdapter;
        let ast = adapter.parse("x = 1\n").unwrap();
        let (entities, _) = adapter.extract(&ast, "module.py");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table
            .find_by_syntax_id(&entities[0].id)
            .unwrap()
            .clone();

        let import_ast = adapter.parse("from numpy import array\n").unwrap();
        let (symbols, edges) =
            resolve_imports(&import_ast, &module_symbol, &entities, &symbol_table);

        assert!(symbols
            .iter()
            .any(|s| s.name.starts_with("external-module")));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Imports));
    }

    #[test]
    fn a_glob_import_gets_an_explicit_placeholder() {
        let adapter = PythonAdapter;
        let ast = adapter.parse("x = 1\n").unwrap();
        let (entities, _) = adapter.extract(&ast, "module.py");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table
            .find_by_syntax_id(&entities[0].id)
            .unwrap()
            .clone();

        let import_ast = adapter.parse("from shapes import *\n").unwrap();
        let (symbols, edges) =
            resolve_imports(&import_ast, &module_symbol, &entities, &symbol_table);

        assert!(symbols.iter().any(|s| s.name.starts_with("glob-import")));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Imports));
    }

    #[test]
    fn a_bare_module_import_gets_an_explicit_placeholder() {
        let adapter = PythonAdapter;
        let ast = adapter.parse("x = 1\n").unwrap();
        let (entities, _) = adapter.extract(&ast, "module.py");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table
            .find_by_syntax_id(&entities[0].id)
            .unwrap()
            .clone();

        let import_ast = adapter.parse("import os\n").unwrap();
        let (symbols, edges) =
            resolve_imports(&import_ast, &module_symbol, &entities, &symbol_table);

        assert!(symbols
            .iter()
            .any(|s| s.name.starts_with("external-module")));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Imports));
    }

    #[test]
    fn exports_a_top_level_item_not_starting_with_underscore() {
        let source = "class Shape:\n    pass\n\n\ndef _helper():\n    pass\n";
        let adapter = PythonAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _) = adapter.extract(&ast, "module.py");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table
            .find_by_syntax_id(&entities[0].id)
            .unwrap()
            .clone();

        let edges = resolve_exports(&ast, &module_symbol, &entities, &symbol_table);

        let shape = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class)
            .unwrap();
        let shape_symbol = symbol_table.find_by_syntax_id(&shape.id).unwrap();
        assert!(edges.contains(&Edge::new(
            module_symbol.id.clone(),
            shape_symbol.id.clone(),
            EdgeKind::Exports
        )));
        assert_eq!(
            edges.len(),
            1,
            "the underscore-prefixed helper isn't exported"
        );
    }

    #[test]
    fn all_dunder_restricts_exports_to_exactly_the_named_items() {
        let source =
            "__all__ = [\"Shape\"]\n\n\nclass Shape:\n    pass\n\n\nclass Internal:\n    pass\n";
        let adapter = PythonAdapter;
        let ast = adapter.parse(source).unwrap();
        let (entities, _) = adapter.extract(&ast, "module.py");
        let symbol_table = crate::symbols::build_symbol_table(&entities);
        let module_symbol = symbol_table
            .find_by_syntax_id(&entities[0].id)
            .unwrap()
            .clone();

        let edges = resolve_exports(&ast, &module_symbol, &entities, &symbol_table);

        let shape = entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Class && e.name == "Shape")
            .unwrap();
        let shape_symbol = symbol_table.find_by_syntax_id(&shape.id).unwrap();
        assert_eq!(edges.len(), 1);
        assert!(edges.contains(&Edge::new(
            module_symbol.id.clone(),
            shape_symbol.id.clone(),
            EdgeKind::Exports
        )));
    }
}
