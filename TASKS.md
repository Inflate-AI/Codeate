# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Current phase: Phase 4 — First Language Adapter: Parsing & AST Extraction

Prove the adapter architecture end-to-end with one real language (Rust), mapping its AST
into the Phase 3 canonical syntax entities (see `ROADMAP.md`).

New dependency proposed: `syn` (with `full` + `parsing` features) — the standard Rust
source-to-AST parsing crate, used standalone via `syn::parse_file` (no proc-macro context
needed).

1. **`LanguageAdapter` trait.**
   Define the trait every language adapter implements: a `parse` method (source text →
   an opaque parsed-AST associated type) and an `extract` method (parsed AST + file
   location → `(Vec<SyntaxEntity>, Vec<Edge>)`), plus stub methods for the resolvers
   later phases add (symbol/type/reference resolution) that default to returning empty —
   so the trait shape doesn't need to change again as those phases land.
   Done when: unit tests exercise the trait via a trivial no-op test adapter, confirming
   the default stub methods return empty without requiring an override.

2. **Rust parser (`RustAdapter::parse`).**
   Wire in `syn`; `RustAdapter::parse(source: &str) -> Result<syn::File>` via
   `syn::parse_file`, converting syntax errors into a `codeate_core::Error` rather than
   panicking.
   Done when: unit tests cover parsing valid Rust source and a clear error on invalid
   syntax.

3. **AST extraction: declarations.**
   `RustAdapter::extract` walks the parsed `syn::File` and produces canonical
   `SyntaxEntity`s for: the file's own `Module`, inline `mod` declarations, `struct`s,
   `enum`s, `trait`s (mapped to `Interface`), top-level `fn`s (`Function`), and `impl`
   block methods (`Method`) with their `Parameter`s — plus `CONTAINS` edges (module
   contains its declarations) and `DECLARES` edges (module declares each item).
   Done when: unit tests on small hand-written source snippets confirm the right
   entities and edges are produced for each declaration kind.

4. **AST extraction: variables, statements, expressions.**
   Extend `extract` to walk function/method bodies, producing `Variable` entities for
   `let` bindings and `Statement`/`Expression` entities for top-level body statements,
   linked to their containing function/method via `CONTAINS`.
   Done when: unit tests confirm a function body with `let` bindings and a few
   statements/expressions produces the expected entity/edge set.

5. **Golden-file tests.**
   Fixture Rust source files (`crates/codeate-core/tests/fixtures/rust_adapter/`) paired
   with an expected canonical syntax tree (entities + edges), asserted via integration
   tests — covering a file that exercises every declaration kind from tasks 3–4 together.
   Done when: these tests pass and would catch a regression in any single extraction
   rule.
