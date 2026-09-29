# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Current phase: Phase 6 — Type Resolution

Resolve static type information for symbols, fields, parameters, and return values,
building on the Phase 4/5 Rust adapter and symbol table (see `ROADMAP.md`).

New dependencies proposed: `quote` + `proc-macro2` (both already pulled in transitively
by `syn`) — used to deterministically stringify a `syn::Type` we can't otherwise resolve,
for the explicit "unresolved/external" fallback.

1. **`TypeRef` model and canonical type symbols.**
   A `TypeRef` enum: `Primitive(name)`, `Named { name, type_args }` (a user-defined type,
   possibly generic), `Reference { mutable, inner }`, `Generic(name)` (a type parameter),
   `Unresolved(raw)` (anything we can't classify, keeping its raw source text rather than
   dropping it). A `type_symbol_for(&TypeRef) -> SymbolEntity` gives every `TypeRef` —
   including primitives and unresolved ones — a stable `SymbolKind::Type` symbol with a
   deterministic id, so it always has something concrete to bind edges to.
   Done when: unit tests cover construction/equality for each variant and confirm
   `type_symbol_for` is deterministic (same `TypeRef` → same id) and namespaces
   primitive/unresolved ids apart from user-defined-type ids of the same name.

2. **Rust type-expression extraction.**
   `extract_type(&syn::Type) -> TypeRef`: `syn::Type::Path` → `Primitive`/`Named`/
   `Generic` (primitives recognized by name: `i32`, `bool`, `String`, ...; a single
   uppercase-ish unbound identifier heuristic isn't required — generics are recognized
   via the enclosing signature's declared type parameters, threaded in from task 5),
   `syn::Type::Reference` → `Reference`, everything else (tuples, arrays, pointers, ...)
   → `Unresolved` with the type's raw token text via `quote`.
   Done when: unit tests cover a primitive, a user-defined named type, a generic
   reference (`&T`), and an unresolved fallback (e.g. a tuple type).

3. **Field extraction and type binding.**
   Rust struct fields have no `SyntaxKind` counterpart (Phase 3's syntax model has no
   `Field` kind — only the symbol side does), so extract them directly as
   `SymbolKind::Field` symbols from `syn::ItemStruct`, `DECLARES`-edged from the struct's
   own `Type` symbol, each `DEPENDS_ON`-edged to its `type_symbol_for(extract_type(..))`.
   Done when: unit tests confirm a struct with several fields produces one `Field` symbol
   per field with the right `DECLARES`/`DEPENDS_ON` edges and type.

4. **Parameter and return type binding.**
   For each `Function`/`Method` symbol (Phase 5), emit an `ACCEPTS` edge from the
   function/method symbol to each parameter's type symbol, and a `RETURNS` edge to its
   return type symbol (defaulting to the unit type `()` when the signature has none).
   Done when: unit tests confirm a function with multiple typed parameters and a return
   type produces the expected `ACCEPTS`/`RETURNS` edges, and a function with no return
   type gets a `RETURNS` edge to the unit type.

5. **Generic parameters and explicit unresolved/external types.**
   Extract a function/struct's declared generic parameters (`<T: Bound>`) as
   `SymbolKind::Generic` symbols, `DECLARES`-edged from their owning function/struct
   symbol — and thread them into task 2's extraction so a matching type usage resolves to
   `TypeRef::Generic` rather than an unrelated `Named`. Confirm genuinely unresolvable
   types (complex/external forms) surface as an explicit `Unresolved` type symbol rather
   than being silently dropped.
   Done when: unit tests cover a generic function (`fn identity<T>(x: T) -> T`) producing
   a `Generic` symbol correctly referenced by its parameter/return edges, and an
   unresolvable type producing a present, explicitly-marked `Unresolved` type symbol.
