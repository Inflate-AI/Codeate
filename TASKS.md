# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Current phase: Phase 18 — 1.0.0 Beta Hardening & Release

See `ROADMAP.md` Phase 18. Moved up ahead of Phase 17's remaining language adapters by
explicit user request on 2026-09-30 — Phase 17 is paused (not closed) after 17.1 (Go)
and resumes once this phase closes. Draft task breakdown below, **pending approval**
before implementation starts (per `CLAUDE.md` workflow step 2).

### Proposed sub-areas (each independently testable, same closing discipline as a
language sub-phase: planned → TDD → Happenings → closed with its own commits)

- [ ] 18.1 **Performance benchmarking.** Pick/vendor 1-2 real-world repos per
  supported language (Rust, Python, Go) at meaningful scale. Benchmark harness
  (`criterion` or hand-rolled timing in the CLI) covering ingest, full analyze,
  incremental analyze, and representative queries. Record baseline numbers
  committed to the repo so future phases can catch regressions.
- [ ] 18.2 **Correctness/robustness hardening.** Malformed/edge-case input handling
  across all 3 adapters (truncated files, syntax errors, encoding issues, huge
  single files, deeply nested structures). Property-based or fuzz testing
  (`proptest`/`cargo-fuzz`) where it fits the determinism guarantee — same fuzzed
  input must always produce the same output, crash-free.
- [ ] 18.3 **Documentation.** Architecture doc (how ingestion → detection → adapters
  → resolution → graph → store → query fit together), an adapter-authoring guide
  (concrete enough that Phase 17's resumed language work can follow it without
  reverse-engineering existing adapters), query API reference (Phase 11/16
  surface), CLI usage docs.
- [ ] 18.4 **Packaging/distribution.** crates.io publish readiness (crate
  metadata: description, license, repository links, keywords for
  `codeate-core`/`codeate-cli`/`codeate-api`), binary release process (target
  platforms, build/release workflow).
- [ ] 18.5 **Dependency/security audit.** `cargo audit` (or equivalent) across the
  workspace; resolve or explicitly accept-and-document any findings.
- [ ] 18.6 **CI.** Automate `cargo test` + `cargo clippy -- -D warnings` on every
  push/PR, covering all 3 language adapters; extends Phase 0's local check script
  rather than replacing it.
- [ ] 18.7 **Versioning policy.** Formal SemVer policy for the canonical model,
  storage format, and `codeate-api`'s public surface (extends Phase 16's policy);
  explicit statement of what "1.0.0 beta" commits to vs. what can still change
  before a stable 1.0.0.

Each sub-area gets its own concrete task list (mirroring 17.1's shape) once you confirm
this breakdown and its ordering — flag now if any should be reordered, merged, dropped,
or if scope within one is off before I start planning 18.1 in detail.

---

## Deferred: Phase 17 — Additional Language Adapters (paused after 17.1)

Full required set and rationale in `ROADMAP.md` Phase 17. Resumes after Phase 18 closes.
Tier 4 (template/config formats) has been dropped from scope entirely per user decision
on 2026-09-30.

**Tier 1 — mainstream general-purpose** (12 remaining)
- [x] 17.1 Go
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
