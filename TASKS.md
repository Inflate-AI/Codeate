# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Current phase: Phase 0 — Project Foundations

1. **Cargo workspace scaffold** — root workspace `Cargo.toml` with members
   `crates/codeate-core` (lib) and `crates/codeate-cli` (bin, produces the `codeate`
   binary), `codeate-cli` depending on `codeate-core`. Pure scaffolding, no behavior —
   no test.
2. **Base error type in `codeate-core`** — a workspace-wide `Error`/`Result` type
   (via `thiserror`) that later phases build on. TDD: test display formatting and
   `From` conversions before implementing.
3. **Local check script** — a script (`scripts/check.sh` / `.ps1`) running
   `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test` in sequence, as the
   local stand-in for CI. Pure scaffolding, no test.
4. **`codeate` CLI binary skeleton** — `codeate --version` and `codeate --help` (via
   `clap`) with no subcommands yet. TDD: integration test asserting exit code 0 and
   expected output substrings for both flags, written before wiring `clap` up.

**Done when:** `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check` all
pass at the workspace root, and `codeate --version` / `codeate --help` run correctly
from the built binary.
