use std::collections::HashSet;

use crate::adapter::LanguageAdapter;
use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};
use crate::resolve::unresolved_symbol;
use crate::symbols::{ScopeTree, SymbolTable};
use crate::types::{type_symbol_for, TypeRef};

const PRIMITIVE_TYPES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
    "f32", "f64", "bool", "char", "str", "String",
];

/// The Rust language adapter: parses Rust source via `syn` and maps it onto
/// the canonical syntax model.
pub struct RustAdapter;

impl LanguageAdapter for RustAdapter {
    type Ast = syn::File;

    fn parse(&self, source: &str) -> Result<Self::Ast> {
        syn::parse_file(source)
            .map_err(|err| Error::Message(format!("failed to parse Rust source: {err}")))
    }

    fn extract(&self, ast: &Self::Ast, location: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
        let mut entities = Vec::new();
        let mut edges = Vec::new();

        let file_module = SyntaxEntity::new(SyntaxKind::Module, location, location);
        entities.push(file_module.clone());

        extract_items(&ast.items, &file_module, location, &mut entities, &mut edges);

        (entities, edges)
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
    edges.push(Edge::new(parent.id.clone(), child.id.clone(), EdgeKind::Contains));
    edges.push(Edge::new(parent.id.clone(), child.id.clone(), EdgeKind::Declares));
    entities.push(child);
}

/// Record `parent CONTAINS child` only (used for parameters, which are
/// contained by their function/method but not "declared" the way top-level
/// items are), then push `child` into `entities`.
fn contain(
    parent: &SyntaxEntity,
    child: SyntaxEntity,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    edges.push(Edge::new(parent.id.clone(), child.id.clone(), EdgeKind::Contains));
    entities.push(child);
}

fn child_scope(scope: &str, name: &str) -> String {
    format!("{scope}::{name}")
}

fn extract_items(
    items: &[syn::Item],
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    for item in items {
        match item {
            syn::Item::Mod(item_mod) => {
                let name = item_mod.ident.to_string();
                let module = SyntaxEntity::new(SyntaxKind::Module, scope, &name);
                let module_scope = child_scope(scope, &name);
                if let Some((_, content)) = &item_mod.content {
                    extract_items(content, &module, &module_scope, entities, edges);
                }
                declare(parent, module, entities, edges);
            }
            syn::Item::Struct(item_struct) => {
                let entity =
                    SyntaxEntity::new(SyntaxKind::Struct, scope, item_struct.ident.to_string());
                declare(parent, entity, entities, edges);
            }
            syn::Item::Enum(item_enum) => {
                let entity =
                    SyntaxEntity::new(SyntaxKind::Enum, scope, item_enum.ident.to_string());
                declare(parent, entity, entities, edges);
            }
            syn::Item::Trait(item_trait) => {
                let entity =
                    SyntaxEntity::new(SyntaxKind::Interface, scope, item_trait.ident.to_string());
                declare(parent, entity, entities, edges);
            }
            syn::Item::Fn(item_fn) => {
                let name = item_fn.sig.ident.to_string();
                let entity = SyntaxEntity::new(SyntaxKind::Function, scope, &name);
                let fn_scope = child_scope(scope, &name);
                extract_params(&item_fn.sig, &entity, &fn_scope, entities, edges);
                extract_body(&item_fn.block, &entity, &fn_scope, entities, edges);
                declare(parent, entity, entities, edges);
            }
            syn::Item::Impl(item_impl) => {
                for impl_item in &item_impl.items {
                    if let syn::ImplItem::Fn(method) = impl_item {
                        let name = method.sig.ident.to_string();
                        let entity = SyntaxEntity::new(SyntaxKind::Method, scope, &name);
                        let method_scope = child_scope(scope, &name);
                        extract_params(&method.sig, &entity, &method_scope, entities, edges);
                        extract_body(&method.block, &entity, &method_scope, entities, edges);
                        declare(parent, entity, entities, edges);
                    }
                }
            }
            _ => {}
        }
    }
}

fn extract_params(
    sig: &syn::Signature,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    for input in &sig.inputs {
        if let syn::FnArg::Typed(typed) = input {
            if let syn::Pat::Ident(pat_ident) = typed.pat.as_ref() {
                let entity =
                    SyntaxEntity::new(SyntaxKind::Parameter, scope, pat_ident.ident.to_string());
                contain(parent, entity, entities, edges);
            }
        }
    }
}

/// Extract `let` bindings as `Variable` entities and top-level body
/// statements/expressions as `Statement`/`Expression` entities, all
/// contained by `parent` (the enclosing function/method). Nested items
/// (e.g. a fn defined inside a fn body) are out of scope for this phase.
fn extract_body(
    block: &syn::Block,
    parent: &SyntaxEntity,
    scope: &str,
    entities: &mut Vec<SyntaxEntity>,
    edges: &mut Vec<Edge>,
) {
    for (index, stmt) in block.stmts.iter().enumerate() {
        match stmt {
            syn::Stmt::Local(local) => {
                if let syn::Pat::Ident(pat_ident) = &local.pat {
                    // Location includes the statement index so redeclaring
                    // the same name in the same scope (`let x = 1; let x =
                    // 2;`) produces two distinct entities/ids, not a
                    // collision.
                    let entity = SyntaxEntity::new(
                        SyntaxKind::Variable,
                        &format!("{scope}#{index}"),
                        pat_ident.ident.to_string(),
                    );
                    contain(parent, entity, entities, edges);
                }
            }
            syn::Stmt::Expr(_, Some(_semicolon)) => {
                let entity = SyntaxEntity::new(SyntaxKind::Statement, scope, format!("stmt_{index}"));
                contain(parent, entity, entities, edges);
            }
            syn::Stmt::Expr(_, None) => {
                let entity = SyntaxEntity::new(SyntaxKind::Expression, scope, format!("expr_{index}"));
                contain(parent, entity, entities, edges);
            }
            syn::Stmt::Item(_) | syn::Stmt::Macro(_) => {}
        }
    }
}

/// Extract a `TypeRef` from a parsed Rust type expression. `generics` names
/// the type parameters in scope (from the enclosing function/struct's
/// generic parameter list, task 5) -- a bare identifier matching one of
/// them resolves to `TypeRef::Generic` rather than `TypeRef::Named`.
/// Anything not handled (tuples, arrays, pointers, ...) becomes
/// `TypeRef::Unresolved` with its raw source text, never silently dropped.
fn extract_type(ty: &syn::Type, generics: &HashSet<String>) -> TypeRef {
    match ty {
        syn::Type::Path(type_path) if type_path.qself.is_none() => {
            let Some(segment) = type_path.path.segments.last() else {
                return unresolved_type(ty);
            };
            let name = segment.ident.to_string();
            let type_args = extract_generic_args(&segment.arguments, generics);

            if type_args.is_empty() && generics.contains(&name) {
                TypeRef::Generic(name)
            } else if type_args.is_empty() && PRIMITIVE_TYPES.contains(&name.as_str()) {
                TypeRef::Primitive(name)
            } else {
                TypeRef::Named { name, type_args }
            }
        }
        syn::Type::Reference(type_reference) => TypeRef::Reference {
            mutable: type_reference.mutability.is_some(),
            inner: Box::new(extract_type(&type_reference.elem, generics)),
        },
        other => unresolved_type(other),
    }
}

fn unresolved_type(ty: &syn::Type) -> TypeRef {
    TypeRef::Unresolved(quote::quote!(#ty).to_string())
}

fn extract_generic_args(args: &syn::PathArguments, generics: &HashSet<String>) -> Vec<TypeRef> {
    match args {
        syn::PathArguments::AngleBracketed(angle) => angle
            .args
            .iter()
            .filter_map(|arg| match arg {
                syn::GenericArgument::Type(ty) => Some(extract_type(ty, generics)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Extract `struct_item`'s named fields as `SymbolKind::Field` symbols.
/// Rust struct fields have no `SyntaxKind` counterpart in the Phase 3
/// canonical model (only the symbol side has a `Field` kind), so they're
/// extracted directly here rather than through `extract_items`.
///
/// Each field symbol is `DECLARES`-edged from `struct_symbol` (the
/// struct's own `SymbolKind::Type` symbol) and `DEPENDS_ON`-edged to its
/// resolved type's symbol -- both the field and type symbols are returned
/// alongside those edges.
fn extract_struct_fields(
    struct_item: &syn::ItemStruct,
    struct_symbol: &SymbolEntity,
    generics: &HashSet<String>,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    let syn::Fields::Named(named) = &struct_item.fields else {
        return (symbols, edges);
    };

    for field in &named.named {
        let Some(ident) = &field.ident else { continue };

        let field_symbol =
            SymbolEntity::new(SymbolKind::Field, struct_symbol.id.as_str(), ident.to_string());
        edges.push(Edge::new(struct_symbol.id.clone(), field_symbol.id.clone(), EdgeKind::Declares));

        let type_ref = extract_type(&field.ty, generics);
        let type_symbol = type_symbol_for(&type_ref);
        edges.push(Edge::new(field_symbol.id.clone(), type_symbol.id.clone(), EdgeKind::DependsOn));

        symbols.push(field_symbol);
        symbols.push(type_symbol);
    }

    (symbols, edges)
}

/// Extract `sig`'s parameter and return types, `ACCEPTS`/`RETURNS`-edged
/// from `fn_symbol` (the function/method's own `SymbolKind::Function`/
/// `Method` symbol). A signature with no declared return type binds
/// `RETURNS` to the unit type `()`.
fn extract_signature_types(
    sig: &syn::Signature,
    fn_symbol: &SymbolEntity,
    generics: &HashSet<String>,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for input in &sig.inputs {
        if let syn::FnArg::Typed(typed) = input {
            let type_ref = extract_type(&typed.ty, generics);
            let type_symbol = type_symbol_for(&type_ref);
            edges.push(Edge::new(fn_symbol.id.clone(), type_symbol.id.clone(), EdgeKind::Accepts));
            symbols.push(type_symbol);
        }
    }

    let return_type = match &sig.output {
        syn::ReturnType::Default => TypeRef::Primitive("()".to_string()),
        syn::ReturnType::Type(_, ty) => extract_type(ty, generics),
    };
    let return_symbol = type_symbol_for(&return_type);
    edges.push(Edge::new(fn_symbol.id.clone(), return_symbol.id.clone(), EdgeKind::Returns));
    symbols.push(return_symbol);

    (symbols, edges)
}

/// Extract `generics`' declared type parameters as `SymbolKind::Generic`
/// symbols, `DECLARES`-edged from `owner_symbol` (the function/struct that
/// declares them). Returns the symbols/edges plus the set of declared
/// names, to thread into `extract_type`/`extract_signature_types`/
/// `extract_struct_fields` so a matching usage resolves to
/// `TypeRef::Generic` rather than an unrelated `Named`.
fn extract_generic_params(
    generics: &syn::Generics,
    owner_symbol: &SymbolEntity,
) -> (Vec<SymbolEntity>, Vec<Edge>, HashSet<String>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();
    let mut names = HashSet::new();

    for param in &generics.params {
        if let syn::GenericParam::Type(type_param) = param {
            let name = type_param.ident.to_string();
            let symbol = SymbolEntity::new(SymbolKind::Generic, owner_symbol.id.as_str(), &name);
            edges.push(Edge::new(owner_symbol.id.clone(), symbol.id.clone(), EdgeKind::Declares));
            names.insert(name);
            symbols.push(symbol);
        }
    }

    (symbols, edges, names)
}

/// Walk `expr`'s common sub-expression forms, calling `on_path` for every
/// `syn::Expr::Path` leaf encountered. Not an exhaustive `syn::Expr` walk --
/// unhandled forms (closures, macros, ranges, ...) simply aren't descended
/// into, which is a deliberate scope limit for this phase rather than a
/// silent-drop of something we claimed to resolve.
fn walk_expr(expr: &syn::Expr, on_path: &mut impl FnMut(&syn::ExprPath)) {
    match expr {
        syn::Expr::Path(path) => on_path(path),
        syn::Expr::Binary(b) => {
            walk_expr(&b.left, on_path);
            walk_expr(&b.right, on_path);
        }
        syn::Expr::Unary(u) => walk_expr(&u.expr, on_path),
        syn::Expr::Paren(p) => walk_expr(&p.expr, on_path),
        syn::Expr::Group(g) => walk_expr(&g.expr, on_path),
        syn::Expr::Call(c) => {
            walk_expr(&c.func, on_path);
            for arg in &c.args {
                walk_expr(arg, on_path);
            }
        }
        syn::Expr::MethodCall(m) => {
            walk_expr(&m.receiver, on_path);
            for arg in &m.args {
                walk_expr(arg, on_path);
            }
        }
        syn::Expr::Field(f) => walk_expr(&f.base, on_path),
        syn::Expr::Reference(r) => walk_expr(&r.expr, on_path),
        syn::Expr::Return(r) => {
            if let Some(inner) = &r.expr {
                walk_expr(inner, on_path);
            }
        }
        syn::Expr::Assign(a) => {
            walk_expr(&a.left, on_path);
            walk_expr(&a.right, on_path);
        }
        syn::Expr::If(i) => {
            walk_expr(&i.cond, on_path);
            for stmt in &i.then_branch.stmts {
                walk_stmt(stmt, on_path);
            }
            if let Some((_, else_expr)) = &i.else_branch {
                walk_expr(else_expr, on_path);
            }
        }
        syn::Expr::Block(b) => {
            for stmt in &b.block.stmts {
                walk_stmt(stmt, on_path);
            }
        }
        syn::Expr::Struct(s) => {
            for field in &s.fields {
                walk_expr(&field.expr, on_path);
            }
        }
        _ => {}
    }
}

fn walk_stmt(stmt: &syn::Stmt, on_path: &mut impl FnMut(&syn::ExprPath)) {
    match stmt {
        syn::Stmt::Local(local) => {
            if let Some(init) = &local.init {
                walk_expr(&init.expr, on_path);
            }
        }
        syn::Stmt::Expr(expr, _) => walk_expr(expr, on_path),
        syn::Stmt::Item(_) | syn::Stmt::Macro(_) => {}
    }
}

/// Resolve every single-segment identifier usage in `block` back to its
/// declaring symbol via `scope_tree`, emitting a `REFERENCES` edge from
/// `fn_symbol` to the resolved symbol -- or to an explicit
/// `unresolved_symbol("unknown-local", ..)` placeholder when the name isn't
/// found in scope. Multi-segment paths (`std::process::exit`) are left to
/// call resolution (task 3).
fn resolve_references(
    block: &syn::Block,
    fn_symbol: &SymbolEntity,
    scope_tree: &ScopeTree,
    scope: usize,
    symbol_table: &SymbolTable,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for stmt in &block.stmts {
        walk_stmt(stmt, &mut |path: &syn::ExprPath| {
            if path.path.segments.len() != 1 {
                return;
            }
            let name = path.path.segments[0].ident.to_string();

            let resolved_symbol = scope_tree
                .lookup(scope, &name)
                .and_then(|syntax_id| symbol_table.find_by_syntax_id(syntax_id))
                .cloned();

            let target = match resolved_symbol {
                Some(symbol) => symbol,
                None => {
                    let placeholder = unresolved_symbol("unknown-local", &name);
                    symbols.push(placeholder.clone());
                    placeholder
                }
            };
            edges.push(Edge::new(fn_symbol.id.clone(), target.id.clone(), EdgeKind::References));
        });
    }

    (symbols, edges)
}

enum CallSite<'a> {
    Free(&'a syn::ExprCall),
    Method(&'a syn::ExprMethodCall),
}

/// Like `walk_expr`, but surfaces `syn::Expr::Call`/`MethodCall` nodes
/// (still recursing into their receiver/args to find nested calls), rather
/// than `syn::Expr::Path` leaves. Same deliberate non-exhaustive scope as
/// `walk_expr`.
fn walk_calls<'a>(expr: &'a syn::Expr, on_call: &mut impl FnMut(CallSite<'a>)) {
    match expr {
        syn::Expr::Call(c) => {
            on_call(CallSite::Free(c));
            for arg in &c.args {
                walk_calls(arg, on_call);
            }
        }
        syn::Expr::MethodCall(m) => {
            on_call(CallSite::Method(m));
            walk_calls(&m.receiver, on_call);
            for arg in &m.args {
                walk_calls(arg, on_call);
            }
        }
        syn::Expr::Binary(b) => {
            walk_calls(&b.left, on_call);
            walk_calls(&b.right, on_call);
        }
        syn::Expr::Unary(u) => walk_calls(&u.expr, on_call),
        syn::Expr::Paren(p) => walk_calls(&p.expr, on_call),
        syn::Expr::Group(g) => walk_calls(&g.expr, on_call),
        syn::Expr::Field(f) => walk_calls(&f.base, on_call),
        syn::Expr::Reference(r) => walk_calls(&r.expr, on_call),
        syn::Expr::Return(r) => {
            if let Some(inner) = &r.expr {
                walk_calls(inner, on_call);
            }
        }
        syn::Expr::Assign(a) => {
            walk_calls(&a.left, on_call);
            walk_calls(&a.right, on_call);
        }
        syn::Expr::If(i) => {
            walk_calls(&i.cond, on_call);
            for stmt in &i.then_branch.stmts {
                walk_calls_stmt(stmt, on_call);
            }
            if let Some((_, else_expr)) = &i.else_branch {
                walk_calls(else_expr, on_call);
            }
        }
        syn::Expr::Block(b) => {
            for stmt in &b.block.stmts {
                walk_calls_stmt(stmt, on_call);
            }
        }
        syn::Expr::Struct(s) => {
            for field in &s.fields {
                walk_calls(&field.expr, on_call);
            }
        }
        _ => {}
    }
}

fn walk_calls_stmt<'a>(stmt: &'a syn::Stmt, on_call: &mut impl FnMut(CallSite<'a>)) {
    match stmt {
        syn::Stmt::Local(local) => {
            if let Some(init) = &local.init {
                walk_calls(&init.expr, on_call);
            }
        }
        syn::Stmt::Expr(expr, _) => walk_calls(expr, on_call),
        syn::Stmt::Item(_) | syn::Stmt::Macro(_) => {}
    }
}

/// Resolve call sites in `block` to callee symbols, emitting `CALLS` edges
/// from `fn_symbol`. Free-function calls (`foo(..)`) resolve via
/// `scope_tree`; a multi-segment callee path (`std::process::exit(..)`)
/// gets an explicit `"external-path"` placeholder. Method calls
/// (`x.foo(..)`) resolve by best-effort name match against any `Method`
/// symbol in `symbol_table` -- real receiver-type dispatch is out of scope
/// for this phase (see Phase 8's `OVERRIDES`/`IMPLEMENTS`); an unmatched
/// method name gets an explicit `"unknown-method"` placeholder.
fn resolve_calls(
    block: &syn::Block,
    fn_symbol: &SymbolEntity,
    scope_tree: &ScopeTree,
    scope: usize,
    symbol_table: &SymbolTable,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for stmt in &block.stmts {
        walk_calls_stmt(stmt, &mut |call: CallSite| match call {
            CallSite::Free(call_expr) => {
                let syn::Expr::Path(path) = call_expr.func.as_ref() else {
                    let raw = quote::quote!(#call_expr).to_string();
                    let placeholder = unresolved_symbol("unsupported-form", &raw);
                    symbols.push(placeholder.clone());
                    edges.push(Edge::new(fn_symbol.id.clone(), placeholder.id.clone(), EdgeKind::Calls));
                    return;
                };

                if path.path.segments.len() != 1 {
                    let raw = quote::quote!(#path).to_string();
                    let placeholder = unresolved_symbol("external-path", &raw);
                    symbols.push(placeholder.clone());
                    edges.push(Edge::new(fn_symbol.id.clone(), placeholder.id.clone(), EdgeKind::Calls));
                    return;
                }

                let name = path.path.segments[0].ident.to_string();
                let resolved = scope_tree
                    .lookup(scope, &name)
                    .and_then(|syntax_id| symbol_table.find_by_syntax_id(syntax_id))
                    .cloned();
                let target = resolved.unwrap_or_else(|| {
                    let placeholder = unresolved_symbol("unknown-local", &name);
                    symbols.push(placeholder.clone());
                    placeholder
                });
                edges.push(Edge::new(fn_symbol.id.clone(), target.id.clone(), EdgeKind::Calls));
            }
            CallSite::Method(method_call) => {
                let name = method_call.method.to_string();
                let matched = symbol_table
                    .symbols()
                    .find(|s| s.kind == SymbolKind::Method && s.name == name)
                    .cloned();
                let target = matched.unwrap_or_else(|| {
                    let placeholder = unresolved_symbol("unknown-method", &name);
                    symbols.push(placeholder.clone());
                    placeholder
                });
                edges.push(Edge::new(fn_symbol.id.clone(), target.id.clone(), EdgeKind::Calls));
            }
        });
    }

    (symbols, edges)
}

enum InstantiateSite<'a> {
    StructLiteral(&'a syn::ExprStruct),
    Call(&'a syn::ExprCall),
}

/// Like `walk_calls`, but surfaces struct-literal expressions (`Point { .. }`)
/// alongside call expressions (candidate tuple-struct constructors).
fn walk_instantiate_sites<'a>(expr: &'a syn::Expr, on_site: &mut impl FnMut(InstantiateSite<'a>)) {
    match expr {
        syn::Expr::Struct(s) => {
            on_site(InstantiateSite::StructLiteral(s));
            for field in &s.fields {
                walk_instantiate_sites(&field.expr, on_site);
            }
        }
        syn::Expr::Call(c) => {
            on_site(InstantiateSite::Call(c));
            for arg in &c.args {
                walk_instantiate_sites(arg, on_site);
            }
        }
        syn::Expr::MethodCall(m) => {
            walk_instantiate_sites(&m.receiver, on_site);
            for arg in &m.args {
                walk_instantiate_sites(arg, on_site);
            }
        }
        syn::Expr::Binary(b) => {
            walk_instantiate_sites(&b.left, on_site);
            walk_instantiate_sites(&b.right, on_site);
        }
        syn::Expr::Unary(u) => walk_instantiate_sites(&u.expr, on_site),
        syn::Expr::Paren(p) => walk_instantiate_sites(&p.expr, on_site),
        syn::Expr::Group(g) => walk_instantiate_sites(&g.expr, on_site),
        syn::Expr::Field(f) => walk_instantiate_sites(&f.base, on_site),
        syn::Expr::Reference(r) => walk_instantiate_sites(&r.expr, on_site),
        syn::Expr::Return(r) => {
            if let Some(inner) = &r.expr {
                walk_instantiate_sites(inner, on_site);
            }
        }
        syn::Expr::Assign(a) => {
            walk_instantiate_sites(&a.left, on_site);
            walk_instantiate_sites(&a.right, on_site);
        }
        syn::Expr::If(i) => {
            walk_instantiate_sites(&i.cond, on_site);
            for stmt in &i.then_branch.stmts {
                walk_instantiate_stmt(stmt, on_site);
            }
            if let Some((_, else_expr)) = &i.else_branch {
                walk_instantiate_sites(else_expr, on_site);
            }
        }
        syn::Expr::Block(b) => {
            for stmt in &b.block.stmts {
                walk_instantiate_stmt(stmt, on_site);
            }
        }
        _ => {}
    }
}

fn walk_instantiate_stmt<'a>(stmt: &'a syn::Stmt, on_site: &mut impl FnMut(InstantiateSite<'a>)) {
    match stmt {
        syn::Stmt::Local(local) => {
            if let Some(init) = &local.init {
                walk_instantiate_sites(&init.expr, on_site);
            }
        }
        syn::Stmt::Expr(expr, _) => walk_instantiate_sites(expr, on_site),
        syn::Stmt::Item(_) | syn::Stmt::Macro(_) => {}
    }
}

/// Resolve object-creation sites in `block` to `INSTANTIATES` edges from
/// `fn_symbol`. Struct literals (`Point { .. }`) always produce an edge --
/// to the real scope-resolved struct's `Type` symbol when in scope, else a
/// `Named`-type symbol (Phase 6's `type_symbol_for`) standing in for an
/// external/unresolved struct. Calls (`Meters(1.0)`) are only treated as a
/// tuple-struct constructor when the callee resolves in scope to a `Type`
/// symbol -- an unrecognized call is left to `resolve_calls` rather than
/// guessed at here.
fn resolve_instantiates(
    block: &syn::Block,
    fn_symbol: &SymbolEntity,
    scope_tree: &ScopeTree,
    scope: usize,
    symbol_table: &SymbolTable,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    for stmt in &block.stmts {
        walk_instantiate_stmt(stmt, &mut |site| match site {
            InstantiateSite::StructLiteral(struct_expr) => {
                let Some(name) = struct_expr.path.segments.last().map(|seg| seg.ident.to_string()) else {
                    return;
                };
                let resolved = scope_tree
                    .lookup(scope, &name)
                    .and_then(|syntax_id| symbol_table.find_by_syntax_id(syntax_id))
                    .cloned();
                let target = resolved.unwrap_or_else(|| {
                    let type_symbol =
                        type_symbol_for(&TypeRef::Named { name: name.clone(), type_args: vec![] });
                    symbols.push(type_symbol.clone());
                    type_symbol
                });
                edges.push(Edge::new(fn_symbol.id.clone(), target.id.clone(), EdgeKind::Instantiates));
            }
            InstantiateSite::Call(call_expr) => {
                let syn::Expr::Path(path) = call_expr.func.as_ref() else {
                    return;
                };
                let Some(name) = path.path.segments.last().map(|seg| seg.ident.to_string()) else {
                    return;
                };
                let Some(symbol) = scope_tree
                    .lookup(scope, &name)
                    .and_then(|syntax_id| symbol_table.find_by_syntax_id(syntax_id))
                else {
                    return;
                };
                if symbol.kind == SymbolKind::Type {
                    edges.push(Edge::new(fn_symbol.id.clone(), symbol.id.clone(), EdgeKind::Instantiates));
                }
            }
        });
    }

    (symbols, edges)
}

/// Run reference, call, and instantiation resolution (tasks 2-4) over one
/// function/method body in a single pass, combining their symbols/edges.
fn resolve_body(
    block: &syn::Block,
    fn_symbol: &SymbolEntity,
    scope_tree: &ScopeTree,
    scope: usize,
    symbol_table: &SymbolTable,
) -> (Vec<SymbolEntity>, Vec<Edge>) {
    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    let (ref_symbols, ref_edges) = resolve_references(block, fn_symbol, scope_tree, scope, symbol_table);
    symbols.extend(ref_symbols);
    edges.extend(ref_edges);

    let (call_symbols, call_edges) = resolve_calls(block, fn_symbol, scope_tree, scope, symbol_table);
    symbols.extend(call_symbols);
    edges.extend(call_edges);

    let (inst_symbols, inst_edges) =
        resolve_instantiates(block, fn_symbol, scope_tree, scope, symbol_table);
    symbols.extend(inst_symbols);
    edges.extend(inst_edges);

    (symbols, edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
        let adapter = RustAdapter;
        let ast = adapter.parse(source).expect("valid Rust source");
        adapter.extract(&ast, "src/lib.rs")
    }

    fn find<'a>(entities: &'a [SyntaxEntity], kind: SyntaxKind, name: &str) -> &'a SyntaxEntity {
        entities
            .iter()
            .find(|e| e.kind == kind && e.name == name)
            .unwrap_or_else(|| panic!("no {kind:?} entity named {name}"))
    }

    #[test]
    fn parses_valid_rust_source() {
        let adapter = RustAdapter;
        let result = adapter.parse("fn main() {}");
        assert!(result.is_ok());
    }

    #[test]
    fn parsing_invalid_syntax_is_a_clear_error() {
        let adapter = RustAdapter;
        let result = adapter.parse("fn main( {{{ not rust");

        let err = result.expect_err("invalid syntax should error");
        assert!(matches!(err, Error::Message(_)));
        assert!(err.to_string().contains("failed to parse Rust source"));
    }

    #[test]
    fn extracts_a_struct_declared_by_the_file_module() {
        let (entities, edges) = extract("struct Point { x: i32, y: i32 }");

        let module = find(&entities, SyntaxKind::Module, "src/lib.rs");
        let point = find(&entities, SyntaxKind::Struct, "Point");

        assert!(edges.contains(&Edge::new(module.id.clone(), point.id.clone(), EdgeKind::Contains)));
        assert!(edges.contains(&Edge::new(module.id.clone(), point.id.clone(), EdgeKind::Declares)));
    }

    #[test]
    fn extracts_an_enum() {
        let (entities, _edges) = extract("enum Color { Red, Green, Blue }");
        find(&entities, SyntaxKind::Enum, "Color");
    }

    #[test]
    fn extracts_a_trait_as_an_interface() {
        let (entities, _edges) = extract("trait Shape { fn area(&self) -> f64; }");
        find(&entities, SyntaxKind::Interface, "Shape");
    }

    #[test]
    fn extracts_a_top_level_function_with_its_parameters() {
        let (entities, edges) = extract("fn add(a: i32, b: i32) -> i32 { a + b }");

        let function = find(&entities, SyntaxKind::Function, "add");
        let a = find(&entities, SyntaxKind::Parameter, "a");
        let b = find(&entities, SyntaxKind::Parameter, "b");

        assert!(edges.contains(&Edge::new(function.id.clone(), a.id.clone(), EdgeKind::Contains)));
        assert!(edges.contains(&Edge::new(function.id.clone(), b.id.clone(), EdgeKind::Contains)));
        // Parameters are contained by their function, but not separately
        // "declared" by it the way top-level items are.
        assert!(!edges.contains(&Edge::new(function.id.clone(), a.id.clone(), EdgeKind::Declares)));
    }

    #[test]
    fn extracts_an_impl_method_excluding_self_from_parameters() {
        let source = "struct Counter; impl Counter { fn increment(&mut self, by: i32) {} }";
        let (entities, _edges) = extract(source);

        let method = find(&entities, SyntaxKind::Method, "increment");
        assert_eq!(method.kind, SyntaxKind::Method);
        find(&entities, SyntaxKind::Parameter, "by");
        assert!(!entities.iter().any(|e| e.kind == SyntaxKind::Parameter && e.name == "self"));
    }

    #[test]
    fn extracts_a_nested_module_and_scopes_its_declarations_to_it() {
        let source = "mod shapes { struct Circle { radius: f64 } }";
        let (entities, edges) = extract(source);

        let file_module = find(&entities, SyntaxKind::Module, "src/lib.rs");
        let shapes = find(&entities, SyntaxKind::Module, "shapes");
        let circle = find(&entities, SyntaxKind::Struct, "Circle");

        assert!(edges.contains(&Edge::new(file_module.id.clone(), shapes.id.clone(), EdgeKind::Contains)));
        assert!(edges.contains(&Edge::new(shapes.id.clone(), circle.id.clone(), EdgeKind::Contains)));
        assert!(!edges.contains(&Edge::new(file_module.id.clone(), circle.id.clone(), EdgeKind::Contains)));
    }

    #[test]
    fn extracts_let_bindings_as_variables_contained_by_the_function() {
        let (entities, edges) = extract("fn run() { let count = 0; let total = count + 1; }");

        let function = find(&entities, SyntaxKind::Function, "run");
        let count = find(&entities, SyntaxKind::Variable, "count");
        let total = find(&entities, SyntaxKind::Variable, "total");

        assert!(edges.contains(&Edge::new(function.id.clone(), count.id.clone(), EdgeKind::Contains)));
        assert!(edges.contains(&Edge::new(function.id.clone(), total.id.clone(), EdgeKind::Contains)));
    }

    #[test]
    fn extracts_a_trailing_statement_and_a_final_tail_expression() {
        let (entities, edges) = extract("fn run() { let x = 1; x + 1; x }");

        let function = find(&entities, SyntaxKind::Function, "run");
        let statement = find(&entities, SyntaxKind::Statement, "stmt_1");
        let tail_expr = find(&entities, SyntaxKind::Expression, "expr_2");

        assert!(edges.contains(&Edge::new(function.id.clone(), statement.id.clone(), EdgeKind::Contains)));
        assert!(edges.contains(&Edge::new(function.id.clone(), tail_expr.id.clone(), EdgeKind::Contains)));
    }

    #[test]
    fn method_bodies_are_extracted_too() {
        let source = "struct Counter; impl Counter { fn increment(&mut self) { let step = 1; } }";
        let (entities, _edges) = extract(source);

        find(&entities, SyntaxKind::Method, "increment");
        find(&entities, SyntaxKind::Variable, "step");
    }

    fn parse_type(source: &str) -> syn::Type {
        syn::parse_str(source).expect("valid Rust type")
    }

    #[test]
    fn extracts_a_primitive_type() {
        let ty = parse_type("i32");
        assert_eq!(extract_type(&ty, &HashSet::new()), TypeRef::Primitive("i32".to_string()));
    }

    #[test]
    fn extracts_a_user_defined_named_type() {
        let ty = parse_type("Point");
        assert_eq!(
            extract_type(&ty, &HashSet::new()),
            TypeRef::Named { name: "Point".to_string(), type_args: vec![] }
        );
    }

    #[test]
    fn extracts_a_generic_named_type_with_its_type_args() {
        let ty = parse_type("Vec<i32>");
        assert_eq!(
            extract_type(&ty, &HashSet::new()),
            TypeRef::Named {
                name: "Vec".to_string(),
                type_args: vec![TypeRef::Primitive("i32".to_string())],
            }
        );
    }

    #[test]
    fn extracts_a_reference_type() {
        let ty = parse_type("&mut str");
        assert_eq!(
            extract_type(&ty, &HashSet::new()),
            TypeRef::Reference {
                mutable: true,
                inner: Box::new(TypeRef::Primitive("str".to_string())),
            }
        );
    }

    #[test]
    fn a_bare_identifier_matching_a_declared_generic_resolves_to_generic() {
        let ty = parse_type("T");
        let generics: HashSet<String> = ["T".to_string()].into_iter().collect();
        assert_eq!(extract_type(&ty, &generics), TypeRef::Generic("T".to_string()));
    }

    #[test]
    fn a_tuple_type_is_unresolved_but_keeps_its_raw_text() {
        let ty = parse_type("(i32, String)");
        let result = extract_type(&ty, &HashSet::new());
        let TypeRef::Unresolved(raw) = result else {
            panic!("expected Unresolved, got something else");
        };
        assert!(raw.contains("i32"));
        assert!(raw.contains("String"));
    }

    fn parse_struct(source: &str) -> (syn::ItemStruct, SymbolEntity) {
        let file: syn::File = syn::parse_str(source).expect("valid Rust source");
        let item_struct = file
            .items
            .into_iter()
            .find_map(|item| match item {
                syn::Item::Struct(s) => Some(s),
                _ => None,
            })
            .expect("source has a struct");
        let syntax_entity = SyntaxEntity::new(SyntaxKind::Struct, "src/lib.rs", item_struct.ident.to_string());
        let struct_symbol =
            SymbolEntity::new(SymbolKind::Type, syntax_entity.id.as_str(), &syntax_entity.name);
        (item_struct, struct_symbol)
    }

    #[test]
    fn extracts_one_field_symbol_per_named_field() {
        let (item_struct, struct_symbol) = parse_struct("struct Point { x: i32, y: i32 }");

        let (symbols, edges) = extract_struct_fields(&item_struct, &struct_symbol, &HashSet::new());

        let field_symbols: Vec<&SymbolEntity> =
            symbols.iter().filter(|s| s.kind == SymbolKind::Field).collect();
        assert_eq!(field_symbols.len(), 2);
        assert!(field_symbols.iter().any(|s| s.name == "x"));
        assert!(field_symbols.iter().any(|s| s.name == "y"));

        for field in &field_symbols {
            assert!(edges.contains(&Edge::new(
                struct_symbol.id.clone(),
                field.id.clone(),
                EdgeKind::Declares
            )));
        }
    }

    #[test]
    fn each_field_depends_on_its_resolved_type() {
        let (item_struct, struct_symbol) = parse_struct("struct Point { x: i32 }");

        let (symbols, edges) = extract_struct_fields(&item_struct, &struct_symbol, &HashSet::new());

        let x_field = symbols.iter().find(|s| s.kind == SymbolKind::Field && s.name == "x").unwrap();
        let i32_type = type_symbol_for(&TypeRef::Primitive("i32".to_string()));

        assert!(edges.contains(&Edge::new(x_field.id.clone(), i32_type.id.clone(), EdgeKind::DependsOn)));
        assert!(symbols.iter().any(|s| s.id == i32_type.id));
    }

    fn parse_fn(source: &str) -> (syn::Signature, SymbolEntity) {
        let file: syn::File = syn::parse_str(source).expect("valid Rust source");
        let item_fn = file
            .items
            .into_iter()
            .find_map(|item| match item {
                syn::Item::Fn(f) => Some(f),
                _ => None,
            })
            .expect("source has a fn");
        let syntax_entity = SyntaxEntity::new(SyntaxKind::Function, "src/lib.rs", item_fn.sig.ident.to_string());
        let fn_symbol =
            SymbolEntity::new(SymbolKind::Function, syntax_entity.id.as_str(), &syntax_entity.name);
        (item_fn.sig, fn_symbol)
    }

    #[test]
    fn binds_each_parameter_type_via_accepts() {
        let (sig, fn_symbol) = parse_fn("fn add(a: i32, b: i32) -> i32 { a + b }");

        let (symbols, edges) = extract_signature_types(&sig, &fn_symbol, &HashSet::new());

        let i32_type = type_symbol_for(&TypeRef::Primitive("i32".to_string()));
        let accepts_count = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Accepts && e.from == fn_symbol.id && e.to == i32_type.id)
            .count();
        // Both `a` and `b` are i32, so the same type symbol is ACCEPTS-edged twice.
        assert_eq!(accepts_count, 2);
        assert!(symbols.iter().any(|s| s.id == i32_type.id));
    }

    #[test]
    fn binds_the_return_type_via_returns() {
        let (sig, fn_symbol) = parse_fn("fn make() -> Point { Point }");

        let (_symbols, edges) = extract_signature_types(&sig, &fn_symbol, &HashSet::new());

        let point_type = type_symbol_for(&TypeRef::Named { name: "Point".to_string(), type_args: vec![] });
        assert!(edges.contains(&Edge::new(fn_symbol.id.clone(), point_type.id.clone(), EdgeKind::Returns)));
    }

    #[test]
    fn extracts_a_generic_parameter_and_threads_it_into_signature_type_binding() {
        let (sig, fn_symbol) = parse_fn("fn identity<T>(x: T) -> T { x }");
        let file: syn::File = syn::parse_str("fn identity<T>(x: T) -> T { x }").unwrap();
        let syn::Item::Fn(item_fn) = &file.items[0] else { unreachable!() };

        let (generic_symbols, generic_edges, names) =
            extract_generic_params(&item_fn.sig.generics, &fn_symbol);

        assert_eq!(generic_symbols.len(), 1);
        let t_symbol = &generic_symbols[0];
        assert_eq!(t_symbol.kind, SymbolKind::Generic);
        assert_eq!(t_symbol.name, "T");
        assert!(generic_edges.contains(&Edge::new(fn_symbol.id.clone(), t_symbol.id.clone(), EdgeKind::Declares)));

        let (_type_symbols, sig_edges) = extract_signature_types(&sig, &fn_symbol, &names);
        let generic_type = type_symbol_for(&TypeRef::Generic("T".to_string()));

        assert!(sig_edges.contains(&Edge::new(fn_symbol.id.clone(), generic_type.id.clone(), EdgeKind::Accepts)));
        assert!(sig_edges.contains(&Edge::new(fn_symbol.id.clone(), generic_type.id.clone(), EdgeKind::Returns)));
    }

    #[test]
    fn an_unresolvable_type_still_produces_an_explicit_unresolved_type_symbol() {
        let (sig, fn_symbol) = parse_fn("fn describe() -> dyn std::fmt::Debug { unimplemented!() }");

        let (symbols, edges) = extract_signature_types(&sig, &fn_symbol, &HashSet::new());

        let returns_edge = edges.iter().find(|e| e.kind == EdgeKind::Returns).expect("a RETURNS edge");
        let return_symbol = symbols.iter().find(|s| s.id == returns_edge.to).expect("return type symbol present");
        // Not silently dropped: it's a real, present Type symbol, just not
        // one of the classified variants -- an explicit Unresolved.
        assert_eq!(return_symbol.kind, SymbolKind::Type);
        assert!(!return_symbol.name.is_empty());
    }

    #[test]
    fn a_signature_with_no_return_type_binds_returns_to_unit() {
        let (sig, fn_symbol) = parse_fn("fn run() {}");

        let (_symbols, edges) = extract_signature_types(&sig, &fn_symbol, &HashSet::new());

        let unit_type = type_symbol_for(&TypeRef::Primitive("()".to_string()));
        assert!(edges.contains(&Edge::new(fn_symbol.id.clone(), unit_type.id.clone(), EdgeKind::Returns)));
    }

    fn parse_fn_block(source: &str) -> syn::Block {
        let file: syn::File = syn::parse_str(source).expect("valid Rust source");
        let syn::Item::Fn(item_fn) = &file.items[0] else {
            panic!("source's first item is not a fn");
        };
        *item_fn.block.clone()
    }

    #[test]
    fn resolves_a_reference_to_an_in_scope_variable() {
        use crate::symbols::build_symbol_table;

        let source = "fn run() { let count = 0; count }";
        let (entities, edges) = extract(source);
        let module = find(&entities, SyntaxKind::Module, "src/lib.rs");
        let run_syntax = find(&entities, SyntaxKind::Function, "run");
        let count_syntax = find(&entities, SyntaxKind::Variable, "count");

        let scope_tree = ScopeTree::build(module, &entities, &edges);
        let symbol_table = build_symbol_table(&entities);
        let run_scope = scope_tree.scope_of(&run_syntax.id).unwrap();
        let run_symbol = symbol_table.find_by_syntax_id(&run_syntax.id).unwrap().clone();
        let count_symbol = symbol_table.find_by_syntax_id(&count_syntax.id).unwrap().clone();

        let block = parse_fn_block(source);
        let (_symbols, ref_edges) =
            resolve_references(&block, &run_symbol, &scope_tree, run_scope, &symbol_table);

        assert!(ref_edges.contains(&Edge::new(
            run_symbol.id.clone(),
            count_symbol.id.clone(),
            EdgeKind::References
        )));
    }

    #[test]
    fn a_reference_to_an_undefined_name_gets_an_explicit_placeholder() {
        use crate::symbols::build_symbol_table;

        let source = "fn run() { missing_name }";
        let (entities, edges) = extract(source);
        let module = find(&entities, SyntaxKind::Module, "src/lib.rs");
        let run_syntax = find(&entities, SyntaxKind::Function, "run");

        let scope_tree = ScopeTree::build(module, &entities, &edges);
        let symbol_table = build_symbol_table(&entities);
        let run_scope = scope_tree.scope_of(&run_syntax.id).unwrap();
        let run_symbol = symbol_table.find_by_syntax_id(&run_syntax.id).unwrap().clone();

        let block = parse_fn_block(source);
        let (symbols, ref_edges) =
            resolve_references(&block, &run_symbol, &scope_tree, run_scope, &symbol_table);

        let placeholder = symbols.first().expect("an unresolved placeholder was created");
        assert_eq!(placeholder.name, "unknown-local:missing_name");
        assert!(ref_edges.contains(&Edge::new(
            run_symbol.id.clone(),
            placeholder.id.clone(),
            EdgeKind::References
        )));
    }

    #[test]
    fn resolves_a_free_function_call() {
        use crate::symbols::build_symbol_table;

        let source = "fn helper() {} fn run() { helper() }";
        let (entities, edges) = extract(source);
        let module = find(&entities, SyntaxKind::Module, "src/lib.rs");
        let run_syntax = find(&entities, SyntaxKind::Function, "run");
        let helper_syntax = find(&entities, SyntaxKind::Function, "helper");

        let scope_tree = ScopeTree::build(module, &entities, &edges);
        let symbol_table = build_symbol_table(&entities);
        let run_scope = scope_tree.scope_of(&run_syntax.id).unwrap();
        let run_symbol = symbol_table.find_by_syntax_id(&run_syntax.id).unwrap().clone();
        let helper_symbol = symbol_table.find_by_syntax_id(&helper_syntax.id).unwrap().clone();

        let block = parse_fn_block("fn run() { helper() }");
        let (_symbols, call_edges) =
            resolve_calls(&block, &run_symbol, &scope_tree, run_scope, &symbol_table);

        assert!(call_edges.contains(&Edge::new(
            run_symbol.id.clone(),
            helper_symbol.id.clone(),
            EdgeKind::Calls
        )));
    }

    #[test]
    fn resolves_a_method_call_by_best_effort_name_match() {
        use crate::symbols::build_symbol_table;

        let source = "struct Counter; impl Counter { fn increment(&mut self) {} } fn run(c: Counter) {}";
        let (entities, edges) = extract(source);
        let module = find(&entities, SyntaxKind::Module, "src/lib.rs");
        let run_syntax = find(&entities, SyntaxKind::Function, "run");
        let increment_syntax = find(&entities, SyntaxKind::Method, "increment");

        let scope_tree = ScopeTree::build(module, &entities, &edges);
        let symbol_table = build_symbol_table(&entities);
        let run_scope = scope_tree.scope_of(&run_syntax.id).unwrap();
        let run_symbol = symbol_table.find_by_syntax_id(&run_syntax.id).unwrap().clone();
        let increment_symbol = symbol_table.find_by_syntax_id(&increment_syntax.id).unwrap().clone();

        let block = parse_fn_block("fn run(mut c: Counter) { c.increment() }");
        let (_symbols, call_edges) =
            resolve_calls(&block, &run_symbol, &scope_tree, run_scope, &symbol_table);

        assert!(call_edges.contains(&Edge::new(
            run_symbol.id.clone(),
            increment_symbol.id.clone(),
            EdgeKind::Calls
        )));
    }

    #[test]
    fn a_call_to_an_external_path_gets_an_explicit_placeholder() {
        use crate::symbols::build_symbol_table;

        let source = "fn run() {}";
        let (entities, edges) = extract(source);
        let module = find(&entities, SyntaxKind::Module, "src/lib.rs");
        let run_syntax = find(&entities, SyntaxKind::Function, "run");

        let scope_tree = ScopeTree::build(module, &entities, &edges);
        let symbol_table = build_symbol_table(&entities);
        let run_scope = scope_tree.scope_of(&run_syntax.id).unwrap();
        let run_symbol = symbol_table.find_by_syntax_id(&run_syntax.id).unwrap().clone();

        let block = parse_fn_block("fn run() { std::process::exit(1) }");
        let (symbols, call_edges) =
            resolve_calls(&block, &run_symbol, &scope_tree, run_scope, &symbol_table);

        let placeholder = symbols.first().expect("an unresolved placeholder was created");
        assert!(placeholder.name.starts_with("external-path:"));
        assert!(call_edges.contains(&Edge::new(
            run_symbol.id.clone(),
            placeholder.id.clone(),
            EdgeKind::Calls
        )));
    }

    #[test]
    fn resolves_a_struct_literal_instantiation() {
        use crate::symbols::build_symbol_table;

        let source = "struct Point { x: i32, y: i32 } fn run() {}";
        let (entities, edges) = extract(source);
        let module = find(&entities, SyntaxKind::Module, "src/lib.rs");
        let run_syntax = find(&entities, SyntaxKind::Function, "run");
        let point_syntax = find(&entities, SyntaxKind::Struct, "Point");

        let scope_tree = ScopeTree::build(module, &entities, &edges);
        let symbol_table = build_symbol_table(&entities);
        let run_scope = scope_tree.scope_of(&run_syntax.id).unwrap();
        let run_symbol = symbol_table.find_by_syntax_id(&run_syntax.id).unwrap().clone();
        let point_symbol = symbol_table.find_by_syntax_id(&point_syntax.id).unwrap().clone();

        let block = parse_fn_block("fn run() { Point { x: 1, y: 2 } }");
        let (_symbols, inst_edges) =
            resolve_instantiates(&block, &run_symbol, &scope_tree, run_scope, &symbol_table);

        assert!(inst_edges.contains(&Edge::new(
            run_symbol.id.clone(),
            point_symbol.id.clone(),
            EdgeKind::Instantiates
        )));
    }

    #[test]
    fn resolves_a_tuple_struct_constructor_call() {
        use crate::symbols::build_symbol_table;

        let source = "struct Meters(f64); fn run() {}";
        let (entities, edges) = extract(source);
        let module = find(&entities, SyntaxKind::Module, "src/lib.rs");
        let run_syntax = find(&entities, SyntaxKind::Function, "run");
        let meters_syntax = find(&entities, SyntaxKind::Struct, "Meters");

        let scope_tree = ScopeTree::build(module, &entities, &edges);
        let symbol_table = build_symbol_table(&entities);
        let run_scope = scope_tree.scope_of(&run_syntax.id).unwrap();
        let run_symbol = symbol_table.find_by_syntax_id(&run_syntax.id).unwrap().clone();
        let meters_symbol = symbol_table.find_by_syntax_id(&meters_syntax.id).unwrap().clone();

        let block = parse_fn_block("fn run() { Meters(1.0) }");
        let (_symbols, inst_edges) =
            resolve_instantiates(&block, &run_symbol, &scope_tree, run_scope, &symbol_table);

        assert!(inst_edges.contains(&Edge::new(
            run_symbol.id.clone(),
            meters_symbol.id.clone(),
            EdgeKind::Instantiates
        )));
    }

    #[test]
    fn resolve_body_produces_the_full_reference_call_and_instantiate_edge_set() {
        use crate::symbols::build_symbol_table;

        let decls_source = "struct Point { x: i32, y: i32 } fn helper() {} fn run() {}";
        let (entities, edges) = extract(decls_source);
        let module = find(&entities, SyntaxKind::Module, "src/lib.rs");
        let run_syntax = find(&entities, SyntaxKind::Function, "run");
        let helper_syntax = find(&entities, SyntaxKind::Function, "helper");
        let point_syntax = find(&entities, SyntaxKind::Struct, "Point");

        let scope_tree = ScopeTree::build(module, &entities, &edges);
        let symbol_table = build_symbol_table(&entities);
        let run_scope = scope_tree.scope_of(&run_syntax.id).unwrap();
        let run_symbol = symbol_table.find_by_syntax_id(&run_syntax.id).unwrap().clone();
        let helper_symbol = symbol_table.find_by_syntax_id(&helper_syntax.id).unwrap().clone();
        let point_symbol = symbol_table.find_by_syntax_id(&point_syntax.id).unwrap().clone();

        let body_source = "fn run() { \
            let p = Point { x: 1, y: 2 }; \
            helper(); \
            std::process::exit(0); \
            p \
        }";
        let block = parse_fn_block(body_source);

        let (symbols, body_edges) =
            resolve_body(&block, &run_symbol, &scope_tree, run_scope, &symbol_table);

        // CALLS: helper() resolves; std::process::exit(0) gets a placeholder.
        assert!(body_edges.contains(&Edge::new(run_symbol.id.clone(), helper_symbol.id.clone(), EdgeKind::Calls)));
        let external_placeholder = symbols
            .iter()
            .find(|s| s.name.starts_with("external-path:"))
            .expect("external call placeholder present");
        assert!(body_edges.contains(&Edge::new(
            run_symbol.id.clone(),
            external_placeholder.id.clone(),
            EdgeKind::Calls
        )));

        // REFERENCES: calling helper() also references it by name.
        assert!(body_edges.contains(&Edge::new(
            run_symbol.id.clone(),
            helper_symbol.id.clone(),
            EdgeKind::References
        )));

        // INSTANTIATES: the struct literal resolves to Point's Type symbol.
        assert!(body_edges.contains(&Edge::new(
            run_symbol.id.clone(),
            point_symbol.id.clone(),
            EdgeKind::Instantiates
        )));
    }

    #[test]
    fn redeclaring_the_same_name_in_the_same_scope_produces_distinct_variables() {
        let (entities, _edges) = extract("fn run() { let x = 1; let x = 2; }");

        let xs: Vec<&SyntaxEntity> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Variable && e.name == "x")
            .collect();

        assert_eq!(xs.len(), 2);
        assert_ne!(xs[0].id, xs[1].id);
    }

    #[test]
    fn same_named_struct_in_different_modules_gets_distinct_ids() {
        let source = "mod a { struct Point; } mod b { struct Point; }";
        let (entities, _edges) = extract(source);

        let points: Vec<&SyntaxEntity> = entities
            .iter()
            .filter(|e| e.kind == SyntaxKind::Struct && e.name == "Point")
            .collect();

        assert_eq!(points.len(), 2);
        assert_ne!(points[0].id, points[1].id);
    }
}
