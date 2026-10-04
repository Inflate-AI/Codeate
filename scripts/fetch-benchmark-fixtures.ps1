#!/usr/bin/env pwsh
# Fetches one or more Phase 18.1 benchmark fixtures: a pinned-commit tarball
# snapshot per language, from benches/fixtures-manifest.json, into
# benches/fixtures/<language>/.
#
# Deliberately NOT a `git clone` -- that would pull full repo history, which
# is the actual multi-GB risk for large real-world repos (not just their
# working tree size). A GitHub codeload tarball is a single-commit snapshot
# with no history at all.
#
# Disk safety (D:\ hit 100% full repeatedly during Phase 17's parallel
# worktree builds -- this script exists specifically not to repeat that):
# checks free space on the fixture root's drive before each fetch and
# refuses below a floor, rather than filling the disk silently.
param(
    [string]$Language,
    [string]$ManifestPath = "benches/fixtures-manifest.json",
    [string]$FixturesRoot = "benches/fixtures",
    [int]$MinFreeGb = 10,
    [switch]$Cleanup
)

$ErrorActionPreference = "Stop"

function Get-FreeGb {
    param([string]$Path)
    $drive = (Resolve-Path $Path -ErrorAction SilentlyContinue)
    if (-not $drive) {
        # Path doesn't exist yet -- resolve its parent's drive instead.
        $driveLetter = (Get-Item ".").PSDrive.Name
    } else {
        $driveLetter = $drive.Drive.Name
    }
    $psDrive = Get-PSDrive -Name $driveLetter
    return [math]::Round($psDrive.Free / 1GB, 2)
}

function Assert-FreeSpace {
    param([int]$MinGb)
    $free = Get-FreeGb -Path "."
    if ($free -lt $MinGb) {
        Write-Host "FAILED: only $free GB free, below the $MinGb GB floor -- aborting fetch to avoid a repeat of Phase 17's full-disk incidents." -ForegroundColor Red
        exit 1
    }
    Write-Host "Free space OK: $free GB (floor: $MinGb GB)"
}

function Remove-FixtureTree {
    param([string]$Path)
    if (Test-Path $Path) {
        Remove-Item -Recurse -Force -Path $Path
    }
}

function Fetch-Fixture {
    param(
        [string]$Lang,
        [string]$Repo,
        [string]$Commit,
        [string]$Subdir,
        [string[]]$IgnoreGlobs,
        [string]$FixturesRoot
    )

    $target = Join-Path $FixturesRoot $Lang
    $markerPath = Join-Path $target ".fetched-commit"

    if (Test-Path $markerPath) {
        $existing = Get-Content $markerPath -Raw | ConvertFrom-Json
        if ($existing.commit -eq $Commit) {
            Write-Host "Skipping $Lang -- already fetched at $Commit"
            return
        }
    }

    Assert-FreeSpace -MinGb $MinFreeGb
    Remove-FixtureTree -Path $target
    New-Item -ItemType Directory -Force -Path $target | Out-Null

    $tarballUrl = "https://github.com/$Repo/archive/$Commit.tar.gz"
    $tarballPath = Join-Path $target "fixture.tar.gz"

    Write-Host "Fetching $Lang from $tarballUrl"
    Invoke-WebRequest -Uri $tarballUrl -OutFile $tarballPath

    $extractDir = Join-Path $target "_extracted"
    New-Item -ItemType Directory -Force -Path $extractDir | Out-Null
    # Call Windows' own bundled tar.exe by full path -- a Git-for-Windows/MSYS
    # /usr/bin/tar earlier on PATH silently shadows it in some shells and
    # chokes on Windows-style paths.
    $winTar = Join-Path $env:SystemRoot "System32\tar.exe"
    & $winTar -xzf $tarballPath -C $extractDir
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: tar extraction for $Lang" -ForegroundColor Red
        exit 1
    }
    Remove-Item -Force -Path $tarballPath

    # GitHub codeload tarballs extract to a single top-level directory named
    # "<repo-name>-<short-or-full-sha>" -- find it rather than hardcoding.
    $extractedRoot = Get-ChildItem -Path $extractDir -Directory | Select-Object -First 1

    if ($Subdir) {
        $sourcePath = Join-Path $extractedRoot.FullName $Subdir
        if (-not (Test-Path $sourcePath)) {
            Write-Host "FAILED: subdir '$Subdir' not found in $Lang's extracted tarball" -ForegroundColor Red
            exit 1
        }
        Get-ChildItem -Path $sourcePath -Force | Move-Item -Destination $target -Force
    } else {
        Get-ChildItem -Path $extractedRoot.FullName -Force | Move-Item -Destination $target -Force
    }
    Remove-Item -Recurse -Force -Path $extractDir

    if ($IgnoreGlobs -and $IgnoreGlobs.Count -gt 0) {
        # A fixture's own deliberately-invalid test data (e.g. a
        # compiler-test fixture containing intentionally broken syntax,
        # not representative source) -- documented per-manifest-entry
        # rather than silently dropped or causing the whole analyze run to
        # abort on an unrelated file's parse error.
        $IgnoreGlobs | Set-Content -Path (Join-Path $target ".codeateignore")
    }

    # code-review-graph's `update --base <commit>` is git-diff-based, so the
    # fixture needs *some* local history to diff against for the
    # incremental-update metric. This is a single fresh commit of the
    # fetched snapshot -- not the upstream repo's real history (that
    # remains the thing this script deliberately never pulls).
    Push-Location $target
    git init --quiet
    git config core.autocrlf false
    git config user.email "bench@codeate.local"
    git config user.name "codeate-bench"
    git add -A 2>$null
    git commit --quiet -m "Fixture snapshot at $Commit"
    Pop-Location

    $marker = @{
        commit    = $Commit
        repo      = $Repo
        fetchedAt = (Get-Date -AsUTC).ToString("o")
    }
    $marker | ConvertTo-Json | Set-Content -Path $markerPath

    Write-Host "Fetched $Lang at $Commit into $target"
}

$manifest = Get-Content $ManifestPath -Raw | ConvertFrom-Json
$fixtures = $manifest.fixtures

if ($Language) {
    $fixtures = $fixtures | Where-Object { $_.language -eq $Language }
    if (-not $fixtures) {
        Write-Host "FAILED: no manifest entry for language '$Language'" -ForegroundColor Red
        exit 1
    }
}

foreach ($fixture in $fixtures) {
    if ($Cleanup) {
        $target = Join-Path $FixturesRoot $fixture.language
        Write-Host "Cleaning up $($fixture.language)"
        Remove-FixtureTree -Path $target
        continue
    }
    Fetch-Fixture -Lang $fixture.language -Repo $fixture.repo -Commit $fixture.commit -Subdir $fixture.subdir -IgnoreGlobs $fixture.ignore_globs -FixturesRoot $FixturesRoot
}
