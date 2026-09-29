# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Current phase: Phase 9 — Graph Builder

Each task follows TDD: failing test first, minimum code to pass, refactor green.

1. **Repository resolution pipeline** (new prerequisite, discovered closing Phase 8)
   - Phases 5–8 built a complete set of resolver functions (symbol tables, type/field/
     signature extraction, reference/call/instantiate resolution, structural
     relationships) but nothing actually calls them together across a real repository —
     each phase closed with only unit tests exercising its own functions directly. A
     `cargo clippy -D warnings` audit while closing Phase 8 confirmed every one of them
     is `#[allow(dead_code)]`-marked dead outside `#[cfg(test)]`.
   - Build the orchestration: given a `Repository` (Phase 1) with detected languages
     (Phase 2), run each file's adapter `parse`/`extract` (Phase 4), merge symbol tables
     across files (Phase 5), run per-file type/field/signature extraction (Phase 6) and
     reference/call/instantiate resolution (Phase 7) against the *merged* repo-wide
     symbol table, then run Phase 8's structural resolvers (`EXTENDS`/`IMPLEMENTS`/
     `OVERRIDES`/`IMPORTS`/`EXPORTS`/`DEPENDS_ON`) against the full repo-wide entity set.
   - Remove the `#[allow(dead_code)]` annotations added while closing Phase 8 as each
     function becomes genuinely reachable from this pipeline.
   - Done: a multi-file fixture repository run through the pipeline produces the full,
     correct entity/edge stream (spot-checked against what the equivalent hand-assembled
     unit tests already verify piecemeal); `cargo clippy -D warnings` is clean with no
     new dead-code allows required.

2. **In-memory graph structure**
   - A `Graph` type holding every entity (syntax + symbol) and edge from task 1's
     pipeline output, with O(1) node lookup by `EntityId` and adjacency lists (outgoing/
     incoming edges per node, optionally indexed by `EdgeKind`).
   - `Graph::build(entities, edges) -> Graph` as the single construction entry point.
   - Done: a small hand-built entity/edge set produces a `Graph` whose node lookup and
     adjacency queries match the input exactly.

3. **Structural integrity checks**
   - Dangling edges: an edge whose `from`/`to` doesn't match any node in the graph.
   - Orphaned nodes: a node with no incoming or outgoing edges at all (informational,
     not necessarily invalid — e.g. an unresolved-symbol placeholder is expected to be a
     pure sink).
   - Invalid cycles: specifically self-referential structural edges that can never be
     legitimate (e.g. a type `EXTENDS`/`IMPLEMENTS` itself).
   - `Graph::build` returns a report of these alongside the graph (never silently drops
     or panics on them — consistent with the project's "represent explicitly" rule) —
     exact return shape (e.g. `(Graph, IntegrityReport)`) decided during implementation.
   - Done: one test per check category — a dangling edge, a self-`EXTENDS` cycle, and a
     legitimately orphaned unresolved-placeholder node — each detected and reported
     without the graph construction itself failing.

4. **Deterministic diffing**
   - Same input entity/edge stream (from task 1) run through `Graph::build` twice
     produces byte-identical serialized output.
   - A `Graph` serialization suitable for this comparison (doesn't need to be the final
     Phase 10 on-disk format yet — a canonical, deterministically-ordered form is
     enough) plus a `diff(a: &Graph, b: &Graph)` reporting added/removed nodes and edges
     between two graphs (used later for incremental analysis, Phase 13, but the
     mechanism is proven here).
   - Done: the same fixture repo analyzed twice produces identical serialized graphs;
     two graphs from deliberately different fixtures produce an accurate, non-empty
     diff.

---

## Planned ahead (not yet current — approved as direction, tasks finalized when picked up)

### Phase 10 — Persistent State Store
- On-disk storage format for the full graph (symbols, types, files, relationships,
  metadata) — format choice (e.g. versioned binary vs. structured file set) decided
  when this phase starts.
- Load/save round-trip with integrity verification (round-tripped graph equals the
  original per Phase 9's diffing).
- Compare/export operations: diff two stored states; export to JSON and GraphML.
- Storage versioning so a schema change in a later phase doesn't corrupt old state
  (version tag + explicit migration-or-reject policy).

### Phase 11 — Query Engine
- Symbol lookup and definition lookup against Phase 10's persisted state.
- Call graph queries: callers-of / callees-of, built on `CALLS` edges.
- Dependency and reverse-dependency analysis, built on `DEPENDS_ON`/`IMPORTS` edges.
- Implementation lookup: interface → implementers, built on `IMPLEMENTS` edges.
- Reference lookup: symbol → all reference sites, built on `REFERENCES` edges.
- Impact analysis: symbol/file change → affected code, built on graph traversal from
  the changed node.
- Repository exploration: entry points, module tree.
- All queries operate purely against persisted state — no re-parsing or re-resolving.

Phase 12 (Developer CLI) is deliberately not broken into tasks yet — to be discussed
before it's planned.
