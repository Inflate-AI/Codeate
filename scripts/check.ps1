#!/usr/bin/env pwsh
# Local stand-in for CI: format check, lint, tests. Run before closing any phase.
$ErrorActionPreference = "Stop"

Write-Host "==> cargo fmt --check"
cargo fmt --all -- --check

Write-Host "==> cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings

Write-Host "==> cargo test"
cargo test --workspace

Write-Host "All checks passed."
