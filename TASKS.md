# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Current phase: Phase 12 — Developer CLI

Design decisions locked in for this phase:
- Query commands that target one symbol take an opaque `--id <entity-id>` (from
  `codeate query symbol <name>`), never a bare name — same-named symbols in different
  files are legitimately distinct (Phase 5), and guessing among them isn't deterministic
  CLI behavior.
- `codeate analyze <path>` persists to `<path>/.codeate/graph.json` by default
  (`--out` overrides); `codeate query` reads from `./.codeate/graph.json` by default
  (`--store` overrides).

Status legend: `todo` → `in progress` → `done`

1. **CLI scaffold** — `todo`
   `clap` derive structure for `codeate analyze <path> [--out <path>]` and
   `codeate query <subcommand> [--store <path>] [--json]`, subcommands stubbed
   (parse only, no behavior yet — pure scaffolding, no test required).

2. **`codeate analyze`** — `todo`
   Wire `ingest` → `detect_languages` → `analyze_repository` → `StoredGraph::from_analysis`
   → `store::save` to `.codeate/graph.json` (creating the dir). Print a summary (file /
   entity / symbol / edge counts). Non-zero exit + clear message on I/O or analysis
   failure.

3. **Query plumbing** — `todo`
   Shared code every `query` subcommand uses: load `StoredGraph` from `--store` (clear
   error if missing/corrupt/unreadable version), `into_graph`, `QueryEngine::new`,
   human-readable vs `--json` output helpers. Exit code convention: `0` success
   (including legitimately empty results — the query engine treats empty as valid, not
   an error), `1` runtime error (bad store, I/O), `2` usage error (clap's default for
   bad args/unknown subcommand).

4. **`codeate query symbol` + `definition`** — `todo`
   `query symbol <name>` lists every matching symbol (id, kind, declaring file).
   `query definition --id <id>` prints the declaring syntax entity.

5. **`codeate query callers` / `callees`** — `todo`
   `--id <id>`, list of symbols on the other end of `CALLS` edges.

6. **`codeate query dependencies` / `dependents`** — `todo`
   `--id <id>`, transitive closure over `DEPENDS_ON`/`IMPORTS`.

7. **`codeate query implementers`** — `todo`
   `--id <id>`, symbols with an `IMPLEMENTS` edge into it.

8. **`codeate query references`** — `todo`
   `--id <id>`, symbols with a `REFERENCES` edge into it.

9. **`codeate query impact`** — `todo`
   `--id <id>`, transitive closure of everything that depends on it (Phase 11's
   impact-analysis edge set).

10. **`codeate query entry-points` / `module-tree`** — `todo`
    No `--id` needed. `entry-points` lists `main` functions/methods; `module-tree`
    prints the nested module hierarchy.

11. **End-to-end tests** — `todo`
    Fixture repo on disk → run the built `codeate` binary
    (`env!("CARGO_BIN_EXE_codeate")`, no new test-harness dependency) → `analyze` →
    each `query` subcommand → assert on stdout and exit code, for both human and
    `--json` output.
