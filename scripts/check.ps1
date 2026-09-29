#!/usr/bin/env pwsh
# Local stand-in for CI: format check, lint, tests. Run before closing any phase.
#
# $ErrorActionPreference = "Stop" only catches PowerShell/cmdlet errors, NOT a
# native command (cargo) returning a nonzero exit code -- so each step's exit
# code must be checked explicitly, or a failing step silently falls through to
# "All checks passed."
$ErrorActionPreference = "Stop"

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
