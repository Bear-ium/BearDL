# Development: screenshot the UI (build with `cargo build --features dev-shots`).
# Usage: powershell -File scripts/shot.ps1 -Out shot.png [-Page library] [-Url <url>] [-Delay 4] [-HomeDir <dir>]
param(
    [Parameter(Mandatory = $true)][string]$Out,
    [string]$Page = '',
    [string]$Url = '',
    [double]$Delay = 4,
    [string]$HomeDir = (Join-Path $env:TEMP 'beardl-measure'),
    [switch]$Select
)
Set-Location (Split-Path $PSScriptRoot -Parent)
$env:BEARDL_HOME = $HomeDir
$env:BEARDL_SHOT = $Out
$env:BEARDL_SHOT_DELAY = "$Delay"
$env:BEARDL_SHOT_PAGE = $Page
$env:BEARDL_SHOT_URL = $Url
if ($Select) { $env:BEARDL_SHOT_SELECT = '1' } else { $env:BEARDL_SHOT_SELECT = '' }
$p = Start-Process -FilePath 'target/debug/beardl.exe' -PassThru
if (-not $p.WaitForExit([int](($Delay + 30) * 1000))) {
    Stop-Process -Id $p.Id -Force
    'timed out'
    exit 1
}
if (Test-Path $Out) { "saved $Out" } else { 'no screenshot'; exit 1 }
