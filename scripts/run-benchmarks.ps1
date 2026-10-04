#!/usr/bin/env pwsh
# Runs Phase 18.1's Codeate benchmark for one language's fixture (already
# fetched via fetch-benchmark-fixtures.ps1) and writes a run-record JSON file
# codeate-bench metrics format later renders into docs/benchmarks.md's
# tables. Competitor-tool (code-review-graph, graphify) runs are driven
# separately per their own CLI shapes -- see docs/benchmarks.md methodology.
param(
    [Parameter(Mandatory = $true)]
    [string]$Language,
    [string]$FixturesRoot = "benches/fixtures",
    [string]$ResultsRoot = "benches/results",
    [int]$Runs = 5,
    [string]$CodeateBin = "./target/release/codeate.exe",
    [string]$BenchBin = "./target/release/codeate-bench.exe",
    [string]$VenvPath = (Join-Path $env:TEMP "codeate-bench-venv"),
    [switch]$SkipCompetitors
)

$ErrorActionPreference = "Stop"

function Get-DescendantProcessIds {
    # code-review-graph and graphify both fork worker subprocesses for
    # parallel AST extraction -- a tool's own top-level process can report a
    # tiny PeakWorkingSet64 while its children do the actual (memory-heavy)
    # work. Walks Win32_Process's ParentProcessId to find every live
    # descendant, not just the one process we spawned directly.
    param([int]$RootId)
    $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId
    $frontier = @($RootId)
    $descendants = @()
    while ($frontier.Count -gt 0) {
        $children = $all | Where-Object { $_.ParentProcessId -in $frontier } | Select-Object -ExpandProperty ProcessId
        $descendants += $children
        $frontier = $children
    }
    return $descendants
}

function Measure-ProcessRun {
    # Times a process AND samples peak working set across it AND every live
    # descendant process in one pass -- Windows doesn't expose peak RSS
    # after exit, so it must be polled while processes are alive, and
    # polling in a second separate run would just measure a different run's
    # time (and might not even see the same subprocess tree).
    param([string]$FilePath, [string[]]$ArgumentList)

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $proc = Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -PassThru -NoNewWindow
    $peakBytes = 0
    while (-not $proc.HasExited) {
        $currentPeak = 0
        try {
            $proc.Refresh()
            $currentPeak = $proc.PeakWorkingSet64
        } catch {
            # Process may have exited between the HasExited check and Refresh.
        }
        try {
            foreach ($childId in (Get-DescendantProcessIds -RootId $proc.Id)) {
                try {
                    $child = Get-Process -Id $childId -ErrorAction Stop
                    $currentPeak += $child.PeakWorkingSet64
                } catch {
                    # Child may have already exited between enumeration and Get-Process.
                }
            }
        } catch {
            # A transient CIM/WMI failure enumerating descendants shouldn't
            # discard the main process's own already-read peak above.
        }
        if ($currentPeak -gt $peakBytes) {
            $peakBytes = $currentPeak
        }
        Start-Sleep -Milliseconds 50
    }
    $proc.WaitForExit()
    $sw.Stop()
    return @{
        ExitCode      = $proc.ExitCode
        PeakBytes     = $peakBytes
        ElapsedSeconds = $sw.Elapsed.TotalSeconds
    }
}

$fixturePath = Join-Path $FixturesRoot $Language
if (-not (Test-Path $fixturePath)) {
    Write-Host "FAILED: no fixture at $fixturePath -- run fetch-benchmark-fixtures.ps1 -Language $Language first" -ForegroundColor Red
    exit 1
}

$resultsDir = Join-Path $ResultsRoot $Language
New-Item -ItemType Directory -Force -Path $resultsDir | Out-Null

Write-Host "=== Codeate: $Language ==="

$coldTimes = @()
$coldPeaks = @()
$graphOut = Join-Path $resultsDir "codeate-graph.json"

for ($i = 1; $i -le $Runs; $i++) {
    if (Test-Path $graphOut) { Remove-Item -Force $graphOut }
    $run = Measure-ProcessRun -FilePath $CodeateBin -ArgumentList @("analyze", $fixturePath, "--out", $graphOut, "--full")
    if ($run.ExitCode -ne 0) {
        Write-Host "FAILED: codeate analyze exited $($run.ExitCode) on cold run $i" -ForegroundColor Red
        exit 1
    }
    $coldTimes += $run.ElapsedSeconds
    $coldPeaks += $run.PeakBytes
    Write-Host "  cold run ${i}: $([math]::Round($run.ElapsedSeconds, 2))s, peak $([math]::Round($run.PeakBytes / 1MB, 1))MB"
}

# Determinism: two independent --full runs, diffed byte-for-byte via
# codeate-bench's StoredGraph comparison.
$detA = Join-Path $resultsDir "determinism-a.json"
$detB = Join-Path $resultsDir "determinism-b.json"
& $CodeateBin analyze $fixturePath --out $detA --full | Out-Null
& $CodeateBin analyze $fixturePath --out $detB --full | Out-Null
& $BenchBin determinism diff $detA $detB
$deterministic = ($LASTEXITCODE -eq 0)
Write-Host "  determinism: $(if ($deterministic) { 'clean' } else { 'DIVERGED' })"

# Incremental: one deterministic no-op edit (append a blank line to the
# first source file found) followed by a non-`--full` analyze, which
# codeate-cli's own existing incremental path (crates/codeate-cli/src/main.rs)
# takes whenever an existing graph is already present at --out.
if (Test-Path $graphOut) { Remove-Item -Force $graphOut }
& $CodeateBin analyze $fixturePath --out $graphOut --full | Out-Null

$editTarget = Get-ChildItem -Path $fixturePath -Recurse -File |
    Where-Object { $_.Extension -in @(".rs", ".py", ".go", ".ts", ".js", ".java", ".cs", ".kt", ".c", ".cpp", ".rb", ".dart") } |
    Select-Object -First 1
$editCommit = $null
if ($editTarget) {
    Add-Content -Path $editTarget.FullName -Value "`n"
    # Commit the edit so code-review-graph's git-diff-based `update` (below)
    # sees a change too, not just Codeate's own mtime/hash-based detection.
    Push-Location $fixturePath
    git add -A 2>$null
    git commit --quiet -m "Incremental benchmark edit"
    $editCommit = (git rev-parse HEAD).Trim()
    Pop-Location
} else {
    Write-Host "WARNING: no source file found to make the incremental no-op edit against" -ForegroundColor Yellow
}

$incrementalRun = Measure-ProcessRun -FilePath $CodeateBin -ArgumentList @("analyze", $fixturePath, "--out", $graphOut)
if ($incrementalRun.ExitCode -ne 0) {
    Write-Host "FAILED: codeate analyze (incremental) exited $($incrementalRun.ExitCode)" -ForegroundColor Red
    exit 1
}
Write-Host "  incremental: $([math]::Round($incrementalRun.ElapsedSeconds, 2))s"

$sortedCold = $coldTimes | Sort-Object
$coldMedian = $sortedCold[[math]::Floor(($sortedCold.Count - 1) / 2)]
$peakMax = ($coldPeaks | Measure-Object -Maximum).Maximum

$result = @{
    language            = $Language
    tool                = "codeate"
    cold_seconds        = $coldMedian
    incremental_seconds = $incrementalRun.ElapsedSeconds
    peak_memory_mb       = [math]::Round($peakMax / 1MB, 1)
    deterministic       = $deterministic
}
$result | ConvertTo-Json | Set-Content -Path (Join-Path $resultsDir "codeate.json")

Write-Host "`nCodeate results for $Language written to $resultsDir/codeate.json"

if ($SkipCompetitors) {
    exit 0
}

# --- Code Review Graph -------------------------------------------------
# `update --base <commit>` is git-diff-based, so the pre-edit commit (before
# $editCommit above) is its "base". The fixture's fetch-time commit is its
# very first commit, so HEAD~1 at this point is that base.
Write-Host "`n=== Code Review Graph: $Language ==="

$crgBin = Join-Path $VenvPath "Scripts\code-review-graph.exe"
if (-not (Test-Path $crgBin)) {
    Write-Host "SKIPPING: code-review-graph not found at $crgBin -- run setup-benchmark-venv.ps1 first" -ForegroundColor Yellow
} else {
    $crgDataDir = Join-Path $resultsDir "code-review-graph-data"

    $crgColdTimes = @()
    $crgColdPeaks = @()
    for ($i = 1; $i -le $Runs; $i++) {
        if (Test-Path $crgDataDir) { Remove-Item -Recurse -Force $crgDataDir }
        $run = Measure-ProcessRun -FilePath $crgBin -ArgumentList @("build", "--repo", $fixturePath, "--data-dir", $crgDataDir, "-q")
        if ($run.ExitCode -ne 0) {
            Write-Host "FAILED: code-review-graph build exited $($run.ExitCode) on cold run ${i}" -ForegroundColor Red
            break
        }
        $crgColdTimes += $run.ElapsedSeconds
        $crgColdPeaks += $run.PeakBytes
        Write-Host "  cold run ${i}: $([math]::Round($run.ElapsedSeconds, 2))s, peak $([math]::Round($run.PeakBytes / 1MB, 1))MB"
    }

    if ($crgColdTimes.Count -gt 0) {
        # Determinism: two independent cold builds, compared by node/edge
        # COUNT via `status --json` -- not a raw file hash. Neither
        # competitor tool guarantees stable serialization ordering the way
        # Codeate's StoredGraph comparison does (byte-identical JSON/DB
        # output isn't something either tool claims), so a count-stability
        # check is the fair, like-for-like bar: same semantic content, not
        # same bytes.
        if (Test-Path $crgDataDir) { Remove-Item -Recurse -Force $crgDataDir }
        & $crgBin build --repo $fixturePath --data-dir $crgDataDir -q | Out-Null
        $statusA = & $crgBin status --repo $fixturePath --data-dir $crgDataDir --json | ConvertFrom-Json
        if (Test-Path $crgDataDir) { Remove-Item -Recurse -Force $crgDataDir }
        & $crgBin build --repo $fixturePath --data-dir $crgDataDir -q | Out-Null
        $statusB = & $crgBin status --repo $fixturePath --data-dir $crgDataDir --json | ConvertFrom-Json
        $crgDeterministic = ($statusA.nodes -eq $statusB.nodes) -and ($statusA.edges -eq $statusB.edges)
        Write-Host "  determinism: $(if ($crgDeterministic) { 'clean' } else { "DIVERGED (run A: $($statusA.nodes) nodes/$($statusA.edges) edges, run B: $($statusB.nodes) nodes/$($statusB.edges) edges)" })"

        # Incremental: re-use the same $editCommit from Codeate's own
        # incremental step above, via --base pointing at its parent.
        & $crgBin build --repo $fixturePath --data-dir $crgDataDir -q | Out-Null
        $crgIncrementalRun = Measure-ProcessRun -FilePath $crgBin -ArgumentList @("update", "--repo", $fixturePath, "--data-dir", $crgDataDir, "-q")
        Write-Host "  incremental: $([math]::Round($crgIncrementalRun.ElapsedSeconds, 2))s"

        $crgSorted = $crgColdTimes | Sort-Object
        $crgMedian = $crgSorted[[math]::Floor(($crgSorted.Count - 1) / 2)]
        $crgPeakMax = ($crgColdPeaks | Measure-Object -Maximum).Maximum

        $crgResult = @{
            language            = $Language
            tool                = "code-review-graph"
            cold_seconds        = $crgMedian
            incremental_seconds = $crgIncrementalRun.ElapsedSeconds
            peak_memory_mb       = [math]::Round($crgPeakMax / 1MB, 1)
            deterministic       = $crgDeterministic
        }
        $crgResult | ConvertTo-Json | Set-Content -Path (Join-Path $resultsDir "code-review-graph.json")
        Write-Host "Code Review Graph results for $Language written to $resultsDir/code-review-graph.json"
    }
}

# --- Graphify -------------------------------------------------------------
Write-Host "`n=== Graphify: $Language ==="

$graphifyBin = Get-Command graphify -ErrorAction SilentlyContinue
if (-not $graphifyBin) {
    Write-Host "SKIPPING: graphify not found on PATH -- run setup-benchmark-venv.ps1 first (uv tool install graphifyy)" -ForegroundColor Yellow
} else {
    $graphifyOutDir = Join-Path $fixturePath "graphify-out"

    $graphifyColdTimes = @()
    $graphifyColdPeaks = @()
    for ($i = 1; $i -le $Runs; $i++) {
        if (Test-Path $graphifyOutDir) { Remove-Item -Recurse -Force $graphifyOutDir }
        $run = Measure-ProcessRun -FilePath $graphifyBin.Source -ArgumentList @("update", $fixturePath, "--no-cluster")
        if ($run.ExitCode -ne 0) {
            Write-Host "FAILED: graphify update exited $($run.ExitCode) on cold run ${i}" -ForegroundColor Red
            break
        }
        $graphifyColdTimes += $run.ElapsedSeconds
        $graphifyColdPeaks += $run.PeakBytes
        Write-Host "  cold run ${i}: $([math]::Round($run.ElapsedSeconds, 2))s, peak $([math]::Round($run.PeakBytes / 1MB, 1))MB"
    }

    if ($graphifyColdTimes.Count -gt 0) {
        # Determinism via node/edge COUNT (same rationale as Code Review
        # Graph's check above) rather than a raw file hash -- graphify's
        # graph.json is a plain JSON node-link structure with no documented
        # serialization-order guarantee.
        if (Test-Path $graphifyOutDir) { Remove-Item -Recurse -Force $graphifyOutDir }
        & $graphifyBin.Source update $fixturePath --no-cluster | Out-Null
        $graphA = Get-Content (Join-Path $graphifyOutDir "graph.json") -Raw | ConvertFrom-Json
        & $graphifyBin.Source update $fixturePath --no-cluster --force | Out-Null
        $graphB = Get-Content (Join-Path $graphifyOutDir "graph.json") -Raw | ConvertFrom-Json
        $graphifyDeterministic = ($graphA.nodes.Count -eq $graphB.nodes.Count) -and ($graphA.links.Count -eq $graphB.links.Count)
        Write-Host "  determinism: $(if ($graphifyDeterministic) { 'clean' } else { "DIVERGED (run A: $($graphA.nodes.Count) nodes/$($graphA.links.Count) links, run B: $($graphB.nodes.Count) nodes/$($graphB.links.Count) links)" })"

        # Incremental: graphify's own `update` re-extracts only changed
        # files by its own file-hash tracking (no --full-equivalent flag),
        # so a second `update` after the no-op edit already made above is
        # its incremental step.
        $graphifyIncrementalRun = Measure-ProcessRun -FilePath $graphifyBin.Source -ArgumentList @("update", $fixturePath, "--no-cluster")
        Write-Host "  incremental: $([math]::Round($graphifyIncrementalRun.ElapsedSeconds, 2))s"

        $graphifySorted = $graphifyColdTimes | Sort-Object
        $graphifyMedian = $graphifySorted[[math]::Floor(($graphifySorted.Count - 1) / 2)]
        $graphifyPeakMax = ($graphifyColdPeaks | Measure-Object -Maximum).Maximum

        $graphifyResult = @{
            language            = $Language
            tool                = "graphify"
            cold_seconds        = $graphifyMedian
            incremental_seconds = $graphifyIncrementalRun.ElapsedSeconds
            peak_memory_mb       = [math]::Round($graphifyPeakMax / 1MB, 1)
            deterministic       = $graphifyDeterministic
        }
        $graphifyResult | ConvertTo-Json | Set-Content -Path (Join-Path $resultsDir "graphify.json")
        Write-Host "Graphify results for $Language written to $resultsDir/graphify.json"
    }
}
