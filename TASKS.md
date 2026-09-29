# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Current phase: Phase 11 — Query Engine

Each task follows TDD: failing test first, minimum code to pass, refactor green.

Queries operate against an in-memory `Graph` (built from a loaded `StoredGraph`, per
Phase 10) — "purely against persisted state" means no re-parsing/re-resolving, not that
each query call re-reads from disk.

1. **Symbol lookup and definition lookup**
   - Look up a symbol by name (repository-wide) and by exact `EntityId`.
   - "Definition lookup": given a symbol, its declaring syntax entity (via `DEFINED_IN`).
   - Done: a fixture graph with several same-named symbols in different files/scopes
     returns all of them by name, and exactly one by id; a definition lookup returns the
     declaring module/file.

2. **Call graph queries**
   - Callers-of(symbol): every symbol with a `CALLS` edge into it.
   - Callees-of(symbol): every symbol that `symbol` has a `CALLS` edge into.
   - Done: a small call chain (`a` calls `b` calls `c`) resolves callers-of(`b`) = `{a}`
     and callees-of(`b`) = `{c}`.

3. **Dependency / reverse-dependency analysis**
   - Dependencies-of(file or module symbol): everything reachable via outgoing
     `DEPENDS_ON`/`IMPORTS` edges.
   - Dependents-of(file or module symbol): everything reachable via incoming
     `DEPENDS_ON`/`IMPORTS` edges (the reverse).
   - Done: a 3-file fixture (A imports B, B imports C) gives dependencies-of(A) ⊇ {B, C}
     (transitive) and dependents-of(C) ⊇ {A, B}.

4. **Implementation lookup**
   - Given an interface/trait symbol, every symbol with an `IMPLEMENTS` edge into it.
   - Done: two structs implementing the same trait both appear; a struct implementing an
     unrelated trait does not.

5. **Reference lookup**
   - Given a symbol, every syntax site with a `REFERENCES` edge into it.
   - Done: a symbol referenced from three call sites returns all three; an unreferenced
     symbol returns an empty result (not an error).

6. **Impact analysis**
   - Given a changed symbol or file, the transitive closure of everything that depends on
     it: reverse-walk `CALLS`/`REFERENCES`/`IMPLEMENTS`/`EXTENDS`/`OVERRIDES`/
     `DEPENDS_ON`/`IMPORTS` edges from the changed node outward.
   - Done: changing a function three calls deep in a chain reports every caller up the
     chain, not just the immediate one; a leaf symbol with no dependents reports an empty
     impact set.

7. **Repository exploration**
   - Entry points: functions/methods named `main`, plus (best-effort, Rust-specific for
     now) anything Cargo would treat as a binary/library root — exact heuristic decided
     during implementation, but never silently empty without saying why.
   - Module tree: the `CONTAINS` hierarchy of `Module` syntax entities, exposed as a
     simple nested structure.
   - Done: a fixture with a `main` function and nested modules returns both the entry
     point and a module tree matching the fixture's actual nesting.

---

## Planned ahead (not yet current — approved as direction, tasks finalized when picked up)

Phase 12 (Developer CLI) is deliberately not broken into tasks yet — to be discussed
before it's planned.
