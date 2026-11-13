# Measures release binary size, cold start, idle memory and idle CPU.
# Usage: powershell -File scripts/measure.ps1 [-NoBuild] [-Warmup <seconds before idle measurement>]
param([switch]$NoBuild, [int]$Warmup = 30)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
if (-not $NoBuild) { cargo build --release | Out-Null }
$exe = Resolve-Path 'target/release/beardl.exe'
$size = (Get-Item $exe).Length

# Isolated profile so measurements do not depend on the user's library.
$env:BEARDL_HOME = Join-Path $env:TEMP 'beardl-measure'

$log = Join-Path $env:TEMP 'beardl-startup.log'
$env:BEARDL_STARTUP_LOG = $log
$times = @()
for ($i = 0; $i -lt 6; $i++) {
    Remove-Item $log -ErrorAction SilentlyContinue
    $t0 = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    Start-Process -FilePath $exe -ArgumentList '--bench-exit' -Wait
    $frames = @(Get-Content $log | Where-Object { $_ -match ' frame$' })
    $painted = [int64]($frames[-1].Split(' ')[0])
    if ($i -gt 0) { $times += ($painted - $t0) }
}
Remove-Item Env:BEARDL_STARTUP_LOG
$cold = ($times | Sort-Object)[[int][math]::Floor($times.Count / 2)]

$p = Start-Process -FilePath $exe -PassThru
Start-Sleep -Seconds $Warmup
$p.Refresh()
$cpu0 = $p.TotalProcessorTime.TotalMilliseconds
Start-Sleep -Seconds 10
$p.Refresh()
$cpu1 = $p.TotalProcessorTime.TotalMilliseconds
$priv = $p.PrivateMemorySize64
$ws = $p.WorkingSet64
Stop-Process -Id $p.Id -Force

'size_bytes={0} size_mib={1:N2}' -f $size, ($size / 1MB)
'cold_start_ms={0:N0} (median of 5)' -f $cold
'idle_private_mib={0:N1} idle_working_set_mib={1:N1}' -f ($priv / 1MB), ($ws / 1MB)
'idle_cpu_ms_per_10s={0:N0}' -f ($cpu1 - $cpu0)
