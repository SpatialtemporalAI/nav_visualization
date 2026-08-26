param(
    [switch]$KeepBuildCache
)

$ErrorActionPreference = "Stop"

Push-Location $PSScriptRoot
try {
    $expectedRust = "1.95.0"
    $actualRust = (rustc --version)
    if (-not $actualRust.Contains($expectedRust)) {
        throw "Rust $expectedRust is required, got: $actualRust"
    }

    $env:CARGO_REGISTRIES_CRATES_IO_PROTOCOL = "sparse"
    $env:CARGO_NET_RETRY = "10"

    Write-Host "Downloading Rust dependencies (the first run can take several minutes)..."
    cargo fetch --locked --verbose

    Write-Host "Compiling woosh-viewer in release mode..."
    cargo build --release --locked

    $dist = Join-Path $PSScriptRoot "dist\windows-x64"
    New-Item -ItemType Directory -Force -Path $dist | Out-Null
    Copy-Item "$PSScriptRoot\target\release\woosh-viewer.exe" $dist -Force
    $distConfig = Join-Path $dist "woosh-viewer.toml"
    if (-not (Test-Path $distConfig)) {
        Copy-Item "$PSScriptRoot\woosh-viewer.sidecar.example.toml" $distConfig
    }

    Write-Host "Windows package created at $dist"
    if (-not $KeepBuildCache) {
        cargo clean
        Write-Host "Rust build cache removed. Use -KeepBuildCache to retain it for incremental builds."
    }
}
finally {
    Pop-Location
}
