param([switch]$SkipTests)

$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw '此脚本仅用于 Windows 构建。' }
Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    $package = Get-Content -Raw Cargo.toml
    $version = [regex]::Match($package, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
    if (-not $version) { throw '无法读取版本号。' }
    $hostInfo = rustc -vV
    if ($LASTEXITCODE -ne 0 -or -not ($hostInfo -match 'host: x86_64-pc-windows-msvc')) {
        throw '请使用 x86_64-pc-windows-msvc Rust 工具链。'
    }
    & (Join-Path $PSScriptRoot 'test-package.ps1')
    cargo fmt --check
    if ($LASTEXITCODE -ne 0) { throw '格式检查失败。' }
    if (-not $SkipTests) {
        cargo test --release --locked
        if ($LASTEXITCODE -ne 0) { throw '测试失败。' }
    }
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Windows 构建失败。' }
    $releaseName = "Pointory-$version-Windows-x64"
    $releaseDirectory = Join-Path (Get-Location) "dist/$releaseName"
    New-Item -ItemType Directory -Force -Path $releaseDirectory | Out-Null
    Copy-Item -LiteralPath target/release/pointory.exe -Destination (Join-Path $releaseDirectory 'Pointory.exe') -Force
    Copy-Item -LiteralPath README.md,CHANGELOG.md -Destination $releaseDirectory -Force
    $archive = Join-Path (Get-Location) "dist/$releaseName.zip"
    . (Join-Path $PSScriptRoot 'package-windows.ps1')
    New-PointoryArchive -SourceDirectory $releaseDirectory -ArchivePath $archive -ReleaseName $releaseName
    $hash = Get-FileHash -Algorithm SHA256 -LiteralPath $archive
    "$($hash.Hash.ToLowerInvariant())  $releaseName.zip" | Set-Content -Encoding utf8NoBOM -LiteralPath "dist/$releaseName.sha256"
    Write-Output $archive
}
finally { Pop-Location }
