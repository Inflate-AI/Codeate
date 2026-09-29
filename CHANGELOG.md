# Changelog

All notable changes to Codeate are documented here, per closed phase.
Format loosely follows [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

Nothing closed yet.

## [Phase 0] — Project Foundations

### Added

- Cargo workspace with two crates: `codeate-core` (lib) and `codeate-cli` (binary
  `codeate`, depends on `codeate-core`).
- Base error type in `codeate-core` (`error::Error` via `thiserror`, `Result<T>`
  alias) with `Io` and `Message` variants — built TDD, 5 unit tests written first
  (red) then made to pass (green): Display formatting, `From<io::Error>` conversion,
  `std::error::Error` impl, Debug output.
- Local check script (`scripts/check.ps1`) running `cargo fmt --check` →
  `cargo clippy -- -D warnings` → `cargo test` in sequence, as the local stand-in
  for CI.
- `codeate` CLI binary skeleton (via `clap`): `codeate --version` and
  `codeate --help` — built TDD, 3 integration tests in `codeate-cli/tests/cli.rs`
  written first against a no-op `main()` (2 red), then made to pass by wiring in
  `clap::Parser` (green). No subcommands yet.

### Fixed

- Repaired a corrupted local `rustup` stable toolchain (missing manifest) by
  uninstalling and reinstalling it; now on rustc/cargo/clippy/rustfmt 1.98.1.

### Verified

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` all pass (8 tests total) via `scripts/check.ps1`.
