use std::collections::HashSet;

use crate::adapter::LanguageAdapter;
use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};
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
    fn a_signature_with_no_return_type_binds_returns_to_unit() {
        let (sig, fn_symbol) = parse_fn("fn run() {}");

        let (_symbols, edges) = extract_signature_types(&sig, &fn_symbol, &HashSet::new());

        let unit_type = type_symbol_for(&TypeRef::Primitive("()".to_string()));
        assert!(edges.contains(&Edge::new(fn_symbol.id.clone(), unit_type.id.clone(), EdgeKind::Returns)));
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
