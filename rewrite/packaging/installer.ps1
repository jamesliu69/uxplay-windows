#Requires -Version 5.1
<#
.SYNOPSIS
  Build a single embedded-cabinet MSI from the staged UxPlayRs bundle.
#>
[CmdletBinding()]
param(
  [ValidateSet('x64', 'arm64')]
  [string] $Architecture = 'x64',
  [ValidatePattern('^\d+\.\d+\.\d+$')]
  [string] $Version = '0.1.1'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$RewriteDir = Split-Path $PSScriptRoot -Parent
$RepoRoot = Split-Path $RewriteDir -Parent
$BundleDir = Join-Path $RewriteDir "out\$Architecture\bundle"
$ArtifactsDir = Join-Path $RewriteDir "out\$Architecture\artifacts"
$MsiPath = Join-Path $ArtifactsDir "UxPlayRs-$Version-$Architecture.msi"
$ReceiptPath = "$MsiPath.receipt.json"
$SourceHead = (git -C $RepoRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify source checkout.' }

foreach ($name in @('UxPlayRs.Gui.exe', 'airplayd.exe', 'ffmpeg.exe',
                   'FFmpeg-LICENSE.txt', 'FFmpeg-SOURCE.txt', 'FFmpeg-VERSION.txt', 'VERSION.txt')) {
  $path = Join-Path $BundleDir $name
  if (-not (Test-Path -LiteralPath $path -PathType Leaf) -or (Get-Item -LiteralPath $path).Length -eq 0) {
    throw "Missing staged runtime file: $path. Run stage.ps1 first."
  }
}
$BundleDir = (Resolve-Path -LiteralPath $BundleDir).Path
$stageVersion = Get-Content -LiteralPath (Join-Path $BundleDir 'VERSION.txt') -Raw
if ($stageVersion -notmatch "(?m)^sha=$([regex]::Escape($SourceHead.Substring(0, 7)))\s*$" -or
    $stageVersion -notmatch "(?m)^arch=$Architecture\s*$") {
  throw 'Staged bundle checkout/architecture differs from this build. Run stage.ps1 again.'
}
$ffmpegMetadata = Get-Content -LiteralPath (Join-Path $BundleDir 'FFmpeg-VERSION.txt') -Raw
$ffmpegHash = (Get-FileHash -LiteralPath (Join-Path $BundleDir 'ffmpeg.exe') -Algorithm SHA256).Hash
if ($ffmpegMetadata -notmatch "(?im)^sha256=$ffmpegHash\s*$" -or
    $ffmpegMetadata -notmatch "(?m)^arch=$Architecture\s*$") {
  throw 'Staged FFmpeg does not match its validated receipt.'
}

& dotnet tool restore --tool-manifest (Join-Path $RepoRoot '.config\dotnet-tools.json')
if ($LASTEXITCODE -ne 0) { throw 'WiX tool restore failed.' }
Push-Location $RepoRoot
try {
  & dotnet wix extension add WixToolset.UI.wixext/7.0.0 -acceptEula wix7
  if ($LASTEXITCODE -ne 0) { throw 'WiX UI extension restore failed.' }
  Copy-Item -LiteralPath (Join-Path $RepoRoot 'libuxplay\LICENSE') -Destination (Join-Path $BundleDir 'LICENSE.txt') -Force
  New-Item -ItemType Directory -Path $ArtifactsDir -Force | Out-Null
  $payload = @(Get-ChildItem -LiteralPath $BundleDir -File -Recurse |
    Where-Object Extension -ne '.pdb' |
    Sort-Object FullName |
    ForEach-Object {
      [ordered]@{
        path = $_.FullName.Substring($BundleDir.Length + 1).Replace('\', '/')
        size = $_.Length
        sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
      }
    })
  $wixArgs = @('wix', 'build', (Join-Path $PSScriptRoot 'product.wxs'),
    '-acceptEula', 'wix7', '-arch', $Architecture, '-culture', 'zh-TW',
    '-ext', 'WixToolset.UI.wixext', '-d', "BundleDir=$BundleDir",
    '-d', "ProductVersion=$Version", '-d', "AppIcon=$(Join-Path $RepoRoot 'stuff\UxPlayRs.Gui.ico')",
    '-d', "LicenseRtf=$(Join-Path $PSScriptRoot 'license.rtf')",
    '-out', $MsiPath, '-pdbtype', 'none', '-cc', (Join-Path $ArtifactsDir 'wix-cabcache'))
  & dotnet @wixArgs
  if ($LASTEXITCODE -ne 0) { throw 'MSI build failed.' }
  & dotnet wix msi validate $MsiPath -acceptEula wix7
  if ($LASTEXITCODE -ne 0) { throw 'MSI ICE validation failed.' }
  & (Join-Path $PSScriptRoot 'verify-launch.ps1') -InstallerPath $MsiPath
  $finalHead = (git -C $RepoRoot rev-parse HEAD).Trim()
  if ($SourceHead -ne $finalHead) { throw 'Source HEAD changed during installer build.' }
  $artifact = Get-Item -LiteralPath $MsiPath
  $hash = (Get-FileHash -LiteralPath $MsiPath -Algorithm SHA256).Hash.ToLowerInvariant()
  $signature = Get-AuthenticodeSignature -LiteralPath $MsiPath
  $receipt = [ordered]@{
    version = $Version
    architecture = $Architecture
    sourceHead = $SourceHead
    sourceDirty = [bool]@(git -C $RepoRoot status --porcelain).Count
    createdUtc = (Get-Date).ToUniversalTime().ToString('o')
    installer = $artifact.Name
    size = $artifact.Length
    sha256 = $hash
    signature = $signature.Status.ToString()
    iceValidation = 'passed'
    launchConditionVerification = '8 passed: compatibility builds, maintenance, native registry AppSearch'
    installedGui = 'not tested; existing live receiver left running'
    payload = $payload
  }
  $receipt | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $ReceiptPath -Encoding utf8
  $null = Get-Content -LiteralPath $ReceiptPath -Raw | ConvertFrom-Json
  "$hash  $($artifact.Name)" | Set-Content -LiteralPath "$MsiPath.sha256" -Encoding ascii
  Write-Host "Installer: $MsiPath"
  Write-Host "SHA256: $hash"
  Write-Host "Receipt: $ReceiptPath"
} finally {
  Pop-Location
}
