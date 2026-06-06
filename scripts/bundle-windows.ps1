#Requires -Version 5.1
<#
.SYNOPSIS
  Build a distributable Windows package (.zip) for Nyx.

.DESCRIPTION
  The Windows counterpart to scripts/bundle-mac.sh. The app's fonts/icons are
  embedded in the binary via rust-embed, so the package only needs the
  executable (plus LICENSE/README for good manners).

  No external tooling required beyond Rust + the MSVC toolchain (the zip is
  built with the in-box Compress-Archive cmdlet).

  Code signing is intentionally NOT done here. The resulting exe runs locally;
  to ship it widely without a SmartScreen warning you'd add `signtool` with an
  Authenticode certificate afterwards.

  NOTE: this requires the compile-level Windows changes (cfg-gated keyring
  backend + per-target GPUI features) to be in place before `cargo build` will
  succeed on Windows. Until then this script will fail at the build step.

.PARAMETER Target
  The Rust target triple to build for. Defaults to the host's MSVC target.

.EXAMPLE
  scripts/bundle-windows.ps1

.OUTPUTS
  target/windows/Nyx-<version>-windows-x64.zip
#>
[CmdletBinding()]
param(
    [string]$Target = 'x86_64-pc-windows-msvc'
)

$ErrorActionPreference = 'Stop'

# --- Resolve repo root (script lives in <root>/scripts) ----------------------
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location $Root

# --- Config ------------------------------------------------------------------
$AppName = 'Nyx'
$BinName = 'nyx'        # the [[bin]] name in crates/nyx/Cargo.toml -> nyx.exe

# Version from the workspace Cargo.toml ([workspace.package] version = "x.y.z").
$Version = (Select-String -Path 'Cargo.toml' -Pattern '^version\s*=\s*"([^"]+)"' |
            Select-Object -First 1).Matches.Groups[1].Value
if (-not $Version) { $Version = '0.0.0' }

$OutDir  = Join-Path 'target' 'windows'
$Stage   = Join-Path $OutDir "$AppName-$Version"
$ZipPath = Join-Path $OutDir "$AppName-$Version-windows-x64.zip"

Write-Host "==> Building $AppName $Version (release, $Target)"

# --- 1. Compile the release binary -------------------------------------------
rustup target add $Target | Out-Null
cargo build --release -p $BinName --target $Target
if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }

$BinSrc = Join-Path 'target' (Join-Path $Target (Join-Path 'release' "$BinName.exe"))
if (-not (Test-Path $BinSrc)) { throw "binary not found: $BinSrc" }

# --- 2. Stage the package ----------------------------------------------------
Write-Host "==> Staging $Stage"
if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Force -Path $Stage | Out-Null

Copy-Item $BinSrc (Join-Path $Stage "$AppName.exe")
foreach ($extra in 'LICENSE', 'README.md') {
    if (Test-Path $extra) { Copy-Item $extra $Stage }
}

# --- 3. Build the .zip -------------------------------------------------------
Write-Host "==> Creating $ZipPath"
if (Test-Path $ZipPath) { Remove-Item -Force $ZipPath }
Compress-Archive -Path (Join-Path $Stage '*') -DestinationPath $ZipPath

Write-Host ""
Write-Host "Done."
Write-Host "  Exe: $(Join-Path $Stage "$AppName.exe")"
Write-Host "  Zip: $ZipPath"
