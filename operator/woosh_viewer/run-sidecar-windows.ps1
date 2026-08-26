param(
    [string]$RobotIp = "192.168.123.161",
    [int]$RobotPort = 8008,
    [int]$ControlPort = 8010,
    [int]$RerunPort = 9876,
    [switch]$SkipSync
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$projectDir = Join-Path $repoRoot "rerun_bridge"
$sidecarScript = Join-Path $repoRoot "src\run_rerun_sidecar.py"
$uv = Get-Command uv -ErrorAction Stop

if (-not $SkipSync) {
    & $uv.Source sync --project $projectDir --extra sidecar --locked
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to prepare the locked remote sidecar environment."
    }
}

& $uv.Source run `
    --project $projectDir `
    --extra sidecar `
    --locked `
    python $sidecarScript `
    --upstream "http://${RobotIp}:${RobotPort}" `
    --control-port $ControlPort `
    --rerun-port $RerunPort

exit $LASTEXITCODE
