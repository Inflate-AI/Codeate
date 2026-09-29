# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Current phase: Phase 3 — Canonical Code Model

Define the language-agnostic data model all future adapters produce and all future
consumers read (see `ROADMAP.md`). This phase is schema + invariants only — no parser
targets it yet, so tasks are pure data-model TDD (construction, equality, ID stability),
not integration tests against real source.

No new external dependencies proposed.

1. **Stable, language-independent entity IDs.**
   An `EntityId` type, deterministically derived from an entity's kind + declaring
   location (e.g. file path) + name (+ disambiguator for overloads/duplicates), using the
   existing `hash_bytes` primitive from Phase 1. Same logical entity → same ID on every
   run; different entities → different IDs.
   Done when: unit tests confirm determinism (same inputs → same ID), distinctness
   (different inputs → different IDs), and a stable string representation.

2. **Syntax entity kinds.**
   A closed `SyntaxKind` enum covering exactly: `Module`, `Package`, `Class`, `Interface`,
   `Struct`, `Enum`, `Function`, `Method`, `Variable`, `Parameter`, `Statement`,
   `Expression`. A `SyntaxEntity` struct `{ id: EntityId, kind: SyntaxKind, name: String }`
   carrying the common shape every syntax node needs, independent of any one language's
   AST.
   Done when: unit tests cover construction/equality for each kind and confirm the enum
   has no "other"/catch-all escape hatch.

3. **Symbol entity kinds.**
   A closed `SymbolKind` enum covering exactly: `Symbol`, `Type`, `Field`, `Method`,
   `Function`, `Constant`, `Namespace`, `Generic`. A `SymbolEntity` struct
   `{ id: EntityId, kind: SymbolKind, name: String }`.
   Done when: unit tests mirror task 2's coverage for the symbol-side kinds.

4. **Closed relationship edge enum.**
   An `EdgeKind` enum covering exactly the 14 README-specified relationship types:
   `Contains`, `Imports`, `Exports`, `Declares`, `References`, `Calls`, `Extends`,
   `Implements`, `Overrides`, `Instantiates`, `Returns`, `Accepts`, `DependsOn`,
   `DefinedIn`. An `Edge` struct `{ from: EntityId, to: EntityId, kind: EdgeKind }`.
   Done when: unit tests cover construction/equality and confirm all 14 kinds round-trip
   (e.g. through a stable string/serialization form used later by the state store).

5. **Cross-cutting invariant tests.**
   Integration-style unit tests (still no parser involved) that build small hand-authored
   sets of entities/edges and assert the model's invariants hold: ID stability across
   independently-constructed-but-logically-identical entities, edges only ever carry one
   of the 14 closed `EdgeKind`s, and syntax vs. symbol entities remain distinguishable
   (e.g. a `SyntaxEntity` for a `Function` and a `SymbolEntity` for the same-named
   `Function` get different `EntityId`s unless explicitly linked).
   Done when: these tests pass and document the model's guarantees for Phase 4 onward.
