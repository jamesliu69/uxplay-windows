#Requires -Version 5.1
<#
.SYNOPSIS
  Stage the UxPlayRs rewrite bundle (Rust engine + WPF GUI).
#>
[CmdletBinding()]
param(
  [ValidateSet('x64', 'arm64')]
  [string] $Architecture = 'x64'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$PackagingDir = $PSScriptRoot
$RewriteDir = Split-Path $PackagingDir -Parent
$RepoRoot = Split-Path $RewriteDir -Parent
$EngineDir = Join-Path $RewriteDir 'engine'
$GuiProject = Join-Path $RewriteDir 'gui\UxPlayRs.Gui\UxPlayRs.Gui.csproj'
$OutDir = Join-Path $RewriteDir "out\$Architecture\bundle"
$PublishDir = Join-Path $RewriteDir "out\$Architecture\gui-publish"

$Rid = if ($Architecture -eq 'arm64') { 'win-arm64' } else { 'win-x64' }

Write-Host "Staging UxPlayRs bundle for $Architecture ($Rid)..."

& cargo build --release -p airplayd --manifest-path (Join-Path $EngineDir 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE." }

$EngineExe = Join-Path $EngineDir 'target\release\airplayd.exe'
if (-not (Test-Path $EngineExe)) { throw "Missing engine binary: $EngineExe" }

& dotnet publish $GuiProject -c Release -r $Rid --self-contained true -o $PublishDir
if ($LASTEXITCODE -ne 0) { throw "dotnet publish failed with exit code $LASTEXITCODE." }

$GuiExe = Join-Path $PublishDir 'UxPlayRs.Gui.exe'
if (-not (Test-Path $GuiExe)) { throw "Missing GUI binary: $GuiExe" }

if (Test-Path $OutDir) { Remove-Item $OutDir -Recurse -Force }
New-Item -ItemType Directory -Path $OutDir -Force | Out-Null

Copy-Item (Join-Path $PublishDir '*') $OutDir -Recurse -Force
Copy-Item $EngineExe (Join-Path $OutDir 'airplayd.exe') -Force

$FinalGui = Join-Path $OutDir 'UxPlayRs.Gui.exe'
$FinalEngine = Join-Path $OutDir 'airplayd.exe'
if (-not (Test-Path $FinalGui)) { throw "Staged bundle missing UxPlayRs.Gui.exe." }
if (-not (Test-Path $FinalEngine)) { throw "Staged bundle missing airplayd.exe." }

$Sha = 'unknown'
try { $Sha = (git -C $RepoRoot rev-parse --short HEAD 2>$null).Trim() } catch { }
if (-not $Sha) { $Sha = 'unknown' }
$Date = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
"sha=$Sha`ndate=$Date`narch=$Architecture`nrid=$Rid`n" | Set-Content (Join-Path $OutDir 'VERSION.txt') -Encoding Ascii

Write-Host "Bundle staged at $OutDir"
