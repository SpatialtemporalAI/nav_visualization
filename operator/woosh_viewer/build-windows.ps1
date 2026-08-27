param(
    [switch]$CleanBuildCache,
    [switch]$KeepBuildCache
)

$ErrorActionPreference = "Stop"
$stage = $null
if ($CleanBuildCache -and $KeepBuildCache) {
    throw "Use either -CleanBuildCache or -KeepBuildCache, not both."
}

Push-Location $PSScriptRoot
try {
    $expectedRust = "1.95.0"
    $actualRust = rustc --version
    if (-not $actualRust.Contains($expectedRust)) {
        throw "Rust $expectedRust is required, got: $actualRust"
    }

    $env:CARGO_REGISTRIES_CRATES_IO_PROTOCOL = "sparse"
    $env:CARGO_NET_RETRY = "10"
    Write-Host "Downloading Rust dependencies..."
    cargo fetch --locked

    Write-Host "Compiling the native Viewer and integrated data service..."
    cargo build --release --locked

    $distRoot = Join-Path $PSScriptRoot "dist"
    $dist = Join-Path $distRoot "windows-x64"
    $archive = Join-Path $distRoot "woosh-viewer-windows-x64.zip"
    $checksum = "$archive.sha256"
    $stage = Join-Path $PSScriptRoot ("target\package-windows-x64-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force -Path $stage, $distRoot | Out-Null

    Copy-Item "$PSScriptRoot\target\release\woosh-viewer.exe" $stage -Force
    Copy-Item "$PSScriptRoot\woosh-viewer.example.toml" (Join-Path $stage "woosh-viewer.toml") -Force

    Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $archive -CompressionLevel Optimal -Force
    $archiveHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash.ToLowerInvariant()
    "$archiveHash  $([IO.Path]::GetFileName($archive))" | Set-Content -LiteralPath $checksum -Encoding ascii

    if (Test-Path -LiteralPath $dist) {
        $resolvedDist = [IO.Path]::GetFullPath($dist)
        $resolvedDistRoot = [IO.Path]::GetFullPath($distRoot)
        if (-not $resolvedDist.StartsWith($resolvedDistRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to replace a package directory outside dist: $resolvedDist"
        }
        Remove-Item -LiteralPath $resolvedDist -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $dist | Out-Null
    Copy-Item (Join-Path $stage "*") $dist -Recurse -Force

    $exeSize = (Get-Item (Join-Path $dist "woosh-viewer.exe")).Length / 1MB
    $zipSize = (Get-Item $archive).Length / 1MB
    Write-Host ("Native Windows package: {0} ({1:N1} MiB executable)" -f $dist, $exeSize)
    Write-Host ("Portable ZIP: {0} ({1:N1} MiB)" -f $archive, $zipSize)
    Write-Host ("SHA-256: {0}" -f $checksum)
    Write-Host "The package contains only woosh-viewer.exe and woosh-viewer.toml."

    if ($CleanBuildCache) {
        cargo clean
        Write-Host "Rust build cache removed because -CleanBuildCache was specified."
    }
    else {
        Write-Host "Rust build cache retained for incremental builds."
    }
}
finally {
    if ($stage -and (Test-Path -LiteralPath $stage)) {
        $targetRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "target"))
        $resolvedStage = [IO.Path]::GetFullPath($stage)
        if (-not $resolvedStage.StartsWith($targetRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to remove package staging path outside target: $resolvedStage"
        }
        Remove-Item -LiteralPath $resolvedStage -Recurse -Force
    }
    Pop-Location
}
