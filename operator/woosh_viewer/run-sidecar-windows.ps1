param(
    [string]$RobotIp = "192.168.123.161",
    [int]$RobotPort = 8008,
    [int]$ControlPort = 8010,
    [int]$RerunPort = 9876,
    [switch]$SkipSync
)

$ErrorActionPreference = "Stop"
$packagedRoot = Join-Path $PSScriptRoot "sidecar"
if (Test-Path (Join-Path $packagedRoot "rerun_bridge\pyproject.toml")) {
    $repoRoot = $packagedRoot
}
else {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
}
$projectDir = Join-Path $repoRoot "rerun_bridge"
$sidecarScript = Join-Path $repoRoot "src\run_rerun_sidecar.py"
if (-not (Test-Path $sidecarScript)) {
    throw "The integrated sidecar is incomplete: $sidecarScript"
}

$historyDir = Join-Path $env:LOCALAPPDATA "Woosh\rerun-history"
New-Item -ItemType Directory -Force -Path $historyDir | Out-Null
$packagedPython = Join-Path $packagedRoot "python\python.exe"
$sidecarArgs = @(
    $sidecarScript,
    "--upstream", "http://${RobotIp}:${RobotPort}",
    "--control-port", $ControlPort,
    "--rerun-port", $RerunPort,
    "--history-dir", $historyDir
)

if (Test-Path $packagedPython) {
    $env:PYTHONDONTWRITEBYTECODE = "1"
    & $packagedPython @sidecarArgs
    exit $LASTEXITCODE
}

$uvPath = (Get-Command uv -ErrorAction Stop).Source
if (-not $SkipSync) {
    & $uvPath sync --project $projectDir --extra sidecar --locked
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to prepare the locked remote sidecar environment."
    }
}

& $uvPath run --project $projectDir --extra sidecar --locked --no-sync python @sidecarArgs
exit $LASTEXITCODE
