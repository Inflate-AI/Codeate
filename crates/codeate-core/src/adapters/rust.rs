use crate::adapter::LanguageAdapter;
use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, SyntaxEntity, SyntaxKind};

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
                declare(parent, entity, entities, edges);
            }
            syn::Item::Impl(item_impl) => {
                for impl_item in &item_impl.items {
                    if let syn::ImplItem::Fn(method) = impl_item {
                        let name = method.sig.ident.to_string();
                        let entity = SyntaxEntity::new(SyntaxKind::Method, scope, &name);
                        let method_scope = child_scope(scope, &name);
                        extract_params(&method.sig, &entity, &method_scope, entities, edges);
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
