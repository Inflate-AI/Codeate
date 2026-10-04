#!/usr/bin/env pwsh
# Sets up an isolated, throwaway Python environment for Phase 18.1's
# comparison tools (Code Review Graph, Graphify, Graphifyy) -- fully
# decoupled from this repo's Rust toolchain (CARGO_HOME/RUSTUP_HOME on
# D:\) and never referenced from crates/. Re-run per session; nothing here
# is meant to persist.
param(
    [string]$VenvPath = (Join-Path $env:TEMP "codeate-bench-venv")
)

$ErrorActionPreference = "Stop"

function Resolve-Python {
    # Windows' `python` often resolves to the Microsoft Store app-execution
    # alias stub rather than a real interpreter -- prefer `python3` if it
    # works, fall back to `python` otherwise.
    foreach ($candidate in @("python3", "python")) {
        $cmd = Get-Command $candidate -ErrorAction SilentlyContinue
        if ($cmd) {
            $version = & $candidate --version 2>&1
            if ($LASTEXITCODE -eq 0) {
                return $candidate
            }
        }
    }
    throw "No working Python interpreter found (tried python3, python)."
}

$python = Resolve-Python
Write-Host "Using interpreter: $python ($(& $python --version))"

if (-not (Test-Path $VenvPath)) {
    Write-Host "Creating venv at $VenvPath"
    & $python -m venv $VenvPath
} else {
    Write-Host "Reusing existing venv at $VenvPath"
}

$venvPip = Join-Path $VenvPath "Scripts\pip.exe"
$venvPython = Join-Path $VenvPath "Scripts\python.exe"

Write-Host "Installing code-review-graph via pip"
& $venvPip install --quiet code-review-graph
if ($LASTEXITCODE -ne 0) {
    Write-Host "FAILED: pip install code-review-graph" -ForegroundColor Red
    exit 1
}

$uv = Get-Command uv -ErrorAction SilentlyContinue
if ($uv) {
    Write-Host "Installing graphify / graphifyy via uv tool install"
    & uv tool install graphify
    & uv tool install graphifyy
} else {
    Write-Host "uv not found; trying pipx"
    $pipx = Get-Command pipx -ErrorAction SilentlyContinue
    if (-not $pipx) {
        Write-Host "FAILED: neither uv nor pipx is available to install graphify/graphifyy" -ForegroundColor Red
        exit 1
    }
    & pipx install graphify
    & pipx install graphifyy
}

Write-Host "`n--- Installed versions ---"
& $venvPip show code-review-graph 2>&1 | Select-String "^Name:|^Version:"

Write-Host "`n--- Tool --help output (for confirming invocation syntax) ---"
foreach ($tool in @("code-review-graph", "graphify", "graphifyy")) {
    Write-Host "`n### $tool --help ###"
    $toolCmd = Join-Path $VenvPath "Scripts\$tool.exe"
    if (Test-Path $toolCmd) {
        & $toolCmd --help 2>&1
    } else {
        # graphify/graphifyy installed via `uv tool install` live in uv's own
        # tool-install location, not this venv -- try invoking by name.
        & $tool --help 2>&1
    }
}

Write-Host "`nVenv ready at $VenvPath"
