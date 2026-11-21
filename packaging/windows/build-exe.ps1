<#
.SYNOPSIS
Builds the release blink.exe into dist/.

.DESCRIPTION
Builds blink-app for x86_64-pc-windows-msvc and copies blink.exe to dist/ at
the repository root, where build-msi.ps1 picks it up. Fails if the executable
links the C runtime dynamically, which happens when a RUSTFLAGS environment
variable replaces the static CRT flag in .cargo/config.toml.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
Set-StrictMode -Version Latest

$root = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$manifest = Join-Path $root 'Cargo.toml'
$target = 'x86_64-pc-windows-msvc'

cargo build --release --locked --target $target -p blink-app --manifest-path $manifest

$targetDir = (cargo metadata --format-version 1 --no-deps --locked --manifest-path $manifest | ConvertFrom-Json).target_directory
$built = Join-Path $targetDir "$target/release/blink.exe"

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$dumpbin = & $vswhere -latest -products * -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe' | Select-Object -First 1
if (-not $dumpbin) {
    throw 'dumpbin.exe not found; install the Visual Studio C++ build tools.'
}
$dynamicCrt = & $dumpbin /nologo /dependents $built | Select-String -Pattern '^\s*(vcruntime|msvcp|ucrtbase|api-ms-win-crt-)'
if ($dynamicCrt) {
    $names = ($dynamicCrt | ForEach-Object { $_.Line.Trim() }) -join ', '
    throw "blink.exe links the C runtime dynamically ($names). Unset RUSTFLAGS so .cargo/config.toml applies."
}

$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force -Path $dist | Out-Null
$exe = Join-Path $dist 'blink.exe'
Copy-Item -Path $built -Destination $exe -Force
Write-Output $exe
