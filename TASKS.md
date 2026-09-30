# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Current phase: Phase 17 — Additional Language Adapters

See `ROADMAP.md` Phase 17 for the full required set and closing condition (every
language/format supported by Graphify + Code Review Graph, union researched
2026-09-30). This phase is a queue of 32 sub-phases, each planned/approved/implemented/
closed independently, same shape as Phase 15.

### Sub-phase queue

**Tier 1 — mainstream general-purpose**
- [ ] 17.1 Go
- [ ] 17.2 TypeScript
- [ ] 17.3 JavaScript
- [ ] 17.4 Java
- [ ] 17.5 C#
- [ ] 17.6 Kotlin
- [ ] 17.7 C
- [ ] 17.8 C++
- [ ] 17.9 Ruby
- [ ] 17.10 PHP
- [ ] 17.11 Swift
- [ ] 17.12 Scala
- [ ] 17.13 Dart

**Tier 2 — scripting / data / infra**
- [ ] 17.14 SQL
- [ ] 17.15 Shell (bash/sh)
- [ ] 17.16 PowerShell
- [ ] 17.17 Perl
- [ ] 17.18 Lua
- [ ] 17.19 R
- [ ] 17.20 Julia

**Tier 3 — enterprise / legacy / niche**
- [ ] 17.21 VB.NET
- [ ] 17.22 Objective-C
- [ ] 17.23 Elixir
- [ ] 17.24 Solidity
- [ ] 17.25 Zig
- [ ] 17.26 Verilog
- [ ] 17.27 SystemVerilog
- [ ] 17.28 Nix
- [ ] 17.29 GDScript
- [ ] 17.30 ReScript
- [ ] 17.31 Luau

**Tier 4 — template/config formats (depend on a Tier 1/2 adapter)**
- [ ] 17.32 Vue SFC (needs TypeScript, 17.2)
- [ ] 17.33 Svelte SFC (needs TypeScript, 17.2)
- [ ] 17.34 Astro (needs TypeScript, 17.2)
- [ ] 17.35 Blade templates (needs PHP, 17.10)
- [ ] 17.36 Ansible YAML
- [ ] 17.37 Terraform/OpenTofu (`.tf`)
- [ ] 17.38 Jupyter/Databricks notebooks (`.ipynb`)
- [ ] 17.39 Spring Boot config (needs Java, 17.4)
- [ ] 17.40 Perl XS (`.xs`, needs Perl, 17.17)

---

### Sub-phase 17.1 — Go (up next, pending approval)

*Same shape as Phase 15: prove the pattern generalizes to a third, C-family-syntax
language via `tree-sitter` rather than a hand-rolled or `syn`-style parser — the first
tree-sitter-based adapter, which the rest of the queue reuses.*

1. **Parser integration.** Add `tree-sitter` + `tree-sitter-go` to the Go adapter crate.
   Confirm parser produces a walkable AST for a representative fixture (packages,
   structs, interfaces, functions, methods, goroutines-as-statements, multiple return
   values).
   - Failing test: parse a minimal fixture, assert non-empty AST root — scaffolding,
     no behavior to TDD beyond "it parses."
2. **AST extractor → canonical syntax entities.** Map Go constructs onto Phase 3's
   model: `File` → package clause, `Struct` (Go structs), `Interface` (Go interfaces),
   `Function`/`Method` (methods keyed by receiver), `Parameter` (incl. multiple returns
   — canonical-model gap check: does `Function`/`Method` support multiple return
   `Type`s today? If not, fix Phase 3's model, don't work around it here),
   `Variable`, top-level `Statement`/`Expression`.
   - Golden-file tests per construct, mirroring Phase 4/15's fixture-based approach.
3. **Symbol resolution.** Wire Go into Phase 5's `build_symbol_table`/`ScopeTree`
   (expected language-agnostic and unchanged, per Phase 15's finding) — package-level
   scope, function/block scope, Go's implicit `:=` short variable declarations handled
   deterministically (shadowing rules match Go's actual scoping, not Rust's/Python's).
4. **Type resolution.** Primitives, structs, interfaces, generics (Go 1.18+ type
   parameters), and Go's implicit interface satisfaction (a type implements an
   interface by having the right methods, no `implements` keyword) — this is the one
   place Go's semantics diverge most from Rust/Python; needs explicit test coverage.
5. **Reference & call resolution.** `REFERENCES`, `CALLS` edges; method calls resolved
   through both explicit receivers and embedded-struct promotion (Go's structural
   "inheritance").
6. **Structural relationships.** `IMPLEMENTS` for implicit interface satisfaction
   (derived, not declared — mark these edges as inferred vs. explicit if the model
   distinguishes that), `IMPORTS`/`EXPORTS`/`DEPENDS_ON` from Go's import paths and
   capitalization-based exported-identifier convention.
7. **Cross-adapter test.** Extend Phase 15's Rust+Python cross-adapter fixture set with
   a Go fixture repo; identical query behavior for equivalent constructs across all
   three languages.

Tiers 2-4 and the remaining Tier 1 languages are not broken into concrete tasks yet —
each sub-phase gets its own planning pass (parser crate selection, canonical-model gap
check, task breakdown) when it's picked up, per the workflow in `CLAUDE.md`.
