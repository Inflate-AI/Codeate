# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Current phase: Phase 7 — Reference & Call Resolution

Resolve usages back to the symbols they refer to, building on the Phase 5 scope model
and Phase 6 type resolution (see `ROADMAP.md`). `OVERRIDES`/`IMPLEMENTS`-aware dispatch
is genuinely simple in this phase — those edges are Phase 8's own deliverable, so this
phase's `CALLS` resolution for method calls is best-effort name matching, not full
trait-dispatch resolution.

No new external dependencies proposed.

1. **Explicit unresolved-reference representation.**
   Before resolving anything, establish how "couldn't resolve this" is represented: a
   canonical placeholder `SymbolEntity` (reusing the `SymbolKind::Symbol`/`Type` generic
   kinds, no new `SymbolKind` needed) keyed by a reason tag (`unknown-local`,
   `external-path`, `unsupported-form`) plus the raw referenced name — so an edge to it
   is still a real, present edge, never a silently dropped reference.
   Done when: unit tests confirm the same (reason, name) pair always produces the same
   placeholder id, and different reasons/names produce different ids.

2. **`REFERENCES` edges.**
   Walk a function/method body's expressions for identifier usages (`syn::Expr::Path`,
   single-segment), resolve each via the Phase 5 `ScopeTree`, and emit a `REFERENCES`
   edge from the enclosing function/method symbol to the resolved symbol — or to task 1's
   placeholder (reason `unknown-local`) when lookup fails.
   Done when: unit tests cover a body referencing an in-scope variable/function
   (resolved) and one referencing an undefined name (explicit placeholder, not dropped).

3. **`CALLS` edges.**
   Resolve `syn::Expr::Call` (free function calls, via scope lookup of the callee path)
   and `syn::Expr::MethodCall` (best-effort: match the method name against any `Method`
   symbol declared in the same file, since real receiver-type dispatch needs Phase 6
   type inference this phase doesn't fully have) to callee symbols, emitting `CALLS`
   edges from the caller's function/method symbol. Calls to paths that don't resolve
   (e.g. `std::process::exit(1)`) get task 1's placeholder with reason `external-path`.
   Done when: unit tests cover a resolved free-function call, a best-effort method call
   match, and an external-path call producing an explicit placeholder.

4. **`INSTANTIATES` edges.**
   Resolve `syn::Expr::Struct` (struct literals) and tuple-struct-style `syn::Expr::Call`
   where the callee resolves to a known struct, emitting an `INSTANTIATES` edge from the
   enclosing function/method symbol to the instantiated type's symbol (the real
   scope-resolved struct symbol when in scope, else a `Named`-type symbol via Phase 6's
   `type_symbol_for`).
   Done when: unit tests cover a struct-literal instantiation and a tuple-struct
   constructor call, both resolving to the struct's `Type` symbol.

5. **Combined per-body resolution pass.**
   `resolve_body(block, fn_symbol, scope_tree, scope, symbol_table, generics) -> (Vec<SymbolEntity>, Vec<Edge>)`
   ties tasks 2-4 together into one walk over a function/method body, so a single call
   produces the full REFERENCES/CALLS/INSTANTIATES edge set for that body.
   Done when: an integration-style unit test runs a body exercising a reference, a
   resolved call, an unresolved call, and an instantiation together, and asserts the
   complete edge set.
