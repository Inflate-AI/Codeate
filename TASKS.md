# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Current phase: Phase 5 — Symbol Resolution

Resolve declared names to `Symbol` entities with stable identity and scope, building on
the Phase 4 Rust adapter's syntax entities/edges (see `ROADMAP.md`).

No new external dependencies proposed.

1. **Scope model.**
   A `Scope` type representing nested lexical scopes (module/function/block), built by
   walking a `SyntaxEntity`/`Edge` set's `CONTAINS` tree from Phase 4. Supports name
   lookup that respects nesting — an inner scope's binding shadows an outer scope's
   same-named binding.
   Done when: unit tests cover building a scope tree from a small hand-built
   entity/edge set and looking up names both directly and through shadowing.

2. **Per-file symbol table construction.**
   `build_symbol_table(entities, edges) -> SymbolTable` walks one file's syntax entities
   and produces a `SymbolEntity` (Phase 3 model) for each named declaration, bound into
   its `Scope`. `SyntaxKind` → `SymbolKind` mapping: `Module`→`Namespace`,
   `Struct`/`Enum`/`Interface`→`Type`, `Function`→`Function`, `Method`→`Method`,
   `Variable`/`Parameter`→`Symbol` (no dedicated symbol-side kind for these — Phase 3's
   closed `SymbolKind` enum doesn't have one, so they map to the generic `Symbol` kind).
   Done when: unit tests confirm each syntax kind produces the right symbol kind and is
   reachable via the scope's lookup.

3. **`DEFINED_IN` edges.**
   For every resolved symbol, emit a `DEFINED_IN` edge from the symbol to the syntax
   entity (module) that declares it.
   Done when: unit tests confirm every symbol in a built table has exactly one
   `DEFINED_IN` edge to its declaring module.

4. **Repository-scope merge.**
   `merge_symbol_tables(tables: Vec<SymbolTable>) -> SymbolTable` combines multiple
   per-file tables into one repository-level table, keyed by `EntityId`.
   Done when: unit tests confirm symbols from separate files merge without collision
   (distinct files already produce distinct IDs per Phase 4's location-scoped ID scheme),
   and querying the merged table finds symbols from any input file.

5. **Deterministic shadowing/redeclaration/duplicate handling.**
   Define and test the model's behavior for: a name shadowed in a nested scope (inner
   binding wins on lookup, outer binding still exists in the table), and the same name
   declared twice in the *same* scope (deterministic policy: both symbols exist in the
   table with distinct `EntityId`s since each carries its own AST-derived disambiguator,
   and lookup deterministically resolves to the most-recently-declared one — no random or
   platform-dependent tie-breaking).
   Done when: unit tests cover both scenarios and pin down the documented policy.
