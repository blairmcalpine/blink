<#
.SYNOPSIS
Builds dist/blink-<version>-x86_64.msi from dist/blink.exe.

.DESCRIPTION
Run build-exe.ps1 first. The version comes from the blink-app package. Needs
WiX Toolset 5 and its Util extension:

    dotnet tool install --global wix --version 5.0.2
    wix extension add --global WixToolset.Util.wixext/5.0.2

Warnings, including those from the ICE validation that follows the build,
are errors, except ICE91.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
Set-StrictMode -Version Latest

$root = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$dist = Join-Path $root 'dist'
$exe = Join-Path $dist 'blink.exe'
if (-not (Test-Path $exe)) {
    throw "$exe not found; run build-exe.ps1 first."
}

$wixVersion = wix --version
if ($wixVersion -notmatch '^5\.') {
    throw "WiX Toolset 5 is required, found $wixVersion."
}

$metadata = cargo metadata --format-version 1 --no-deps --locked --manifest-path (Join-Path $root 'Cargo.toml') | ConvertFrom-Json
$version = ($metadata.packages | Where-Object name -EQ 'blink-app').version
# Windows Installer reads ProductVersion as major.minor.build, with major and
# minor below 256 and build below 65536, and has no pre-release suffix.
if ($version -notmatch '^(\d+)\.(\d+)\.(\d+)$' -or [int]$Matches[1] -gt 255 -or [int]$Matches[2] -gt 255 -or [int]$Matches[3] -gt 65535) {
    throw "blink-app version $version cannot be an MSI ProductVersion."
}

$msi = Join-Path $dist "blink-$version-x86_64.msi"
# ICE91 warns that per-user files are not copied to every profile in a
# per-machine install; Scope="perUser" means there never is one.
wix build (Join-Path $PSScriptRoot 'blink.wxs') -arch x64 -ext WixToolset.Util.wixext `
    -d "Version=$version" -d "BlinkExe=$exe" -d "RepoRoot=$root" -wx -o $msi
wix msi validate -sice ICE91 -wx $msi
Write-Output $msi
