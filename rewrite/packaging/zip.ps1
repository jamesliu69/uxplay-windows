#Requires -Version 5.1
<#
.SYNOPSIS
  Zip a staged UxPlayRs rewrite bundle.
#>
[CmdletBinding()]
param(
  [ValidateSet('x64', 'arm64')]
  [string] $Architecture = 'x64'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RewriteDir = Split-Path $PSScriptRoot -Parent
$BundleDir = Join-Path $RewriteDir "out\$Architecture\bundle"
$ArtifactsDir = Join-Path $RewriteDir "out\$Architecture\artifacts"
$ZipPath = Join-Path $ArtifactsDir "uxplay-rs-$Architecture.zip"

if (-not (Test-Path (Join-Path $BundleDir 'UxPlayRs.Gui.exe'))) { throw "Missing GUI exe in bundle: $BundleDir" }
if (-not (Test-Path (Join-Path $BundleDir 'airplayd.exe'))) { throw "Missing engine exe in bundle: $BundleDir" }

New-Item -ItemType Directory -Path $ArtifactsDir -Force | Out-Null
if (Test-Path $ZipPath) { Remove-Item $ZipPath -Force }
Compress-Archive -Path (Join-Path $BundleDir '*') -DestinationPath $ZipPath -Force
Write-Host "Wrote $ZipPath"
