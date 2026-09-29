#!/usr/bin/env pwsh
# Local stand-in for CI: format check, lint, tests. Run before closing any phase.
#
# $ErrorActionPreference = "Stop" only catches PowerShell/cmdlet errors, NOT a
# native command (cargo) returning a nonzero exit code -- so each step's exit
# code must be checked explicitly, or a failing step silently falls through to
# "All checks passed."
$ErrorActionPreference = "Stop"

# The toolchain lives on D:\, not the default C:\Users\<user>\.cargo/.rustup
# (see CLAUDE.md's "Toolchain location" section) -- an agent's tool shell can
# inherit a stale environment that doesn't see the user-level CARGO_HOME/
# RUSTUP_HOME/PATH, so set them explicitly here rather than relying on that.
if (Test-Path "D:\cargo\bin") {
    $env:CARGO_HOME = "D:\cargo"
    $env:RUSTUP_HOME = "D:\rustup"
    $env:PATH = "D:\cargo\bin;$env:PATH"
}

function Invoke-Step {
    param([string]$Name, [scriptblock]$Command)
    Write-Host "==> $Name"
    & $Command
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: $Name (exit code $LASTEXITCODE)" -ForegroundColor Red
        exit $LASTEXITCODE
    }
}

Invoke-Step "cargo fmt --check" { cargo fmt --all -- --check }
Invoke-Step "cargo clippy" { cargo clippy --workspace --all-targets -- -D warnings }
Invoke-Step "cargo test" { cargo test --workspace }

Write-Host "All checks passed."
