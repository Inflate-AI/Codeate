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
    [string]$BenchBin = "./target/release/codeate-bench.exe"
)

$ErrorActionPreference = "Stop"

function Measure-ProcessRun {
    # Times a process AND samples its peak working set in one pass --
    # Windows doesn't expose peak RSS after exit, so it must be polled while
    # the process is alive, and polling in a second separate run would just
    # measure a different run's time.
    param([string]$FilePath, [string[]]$ArgumentList)

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $proc = Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -PassThru -NoNewWindow
    $peakBytes = 0
    while (-not $proc.HasExited) {
        try {
            $proc.Refresh()
            if ($proc.PeakWorkingSet64 -gt $peakBytes) {
                $peakBytes = $proc.PeakWorkingSet64
            }
        } catch {
            # Process may have exited between the HasExited check and Refresh.
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
if ($editTarget) {
    Add-Content -Path $editTarget.FullName -Value "`n"
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
