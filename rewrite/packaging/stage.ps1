#Requires -Version 5.1
<#
.SYNOPSIS
  Stage the UxPlayRs rewrite bundle (Rust engine + WPF GUI).
#>
[CmdletBinding()]
param(
  [ValidateSet('x64', 'arm64')]
  [string] $Architecture = 'x64',
  [string] $FFmpegPath,
  [string] $FFmpegLicensePath,
  [string] $FFmpegSourcePath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Get-PeInfo {
  param([Parameter(Mandatory = $true)][string] $Path)

  $stream = [IO.File]::OpenRead($Path)
  $reader = [IO.BinaryReader]::new($stream)
  try {
    if ($reader.ReadUInt16() -ne 0x5a4d) { throw "Not a Windows PE binary: $Path" }
    $stream.Position = 0x3c
    $peOffset = $reader.ReadUInt32()
    $stream.Position = $peOffset
    if ($reader.ReadUInt32() -ne 0x00004550) { throw "Invalid PE signature: $Path" }
    $machine = $reader.ReadUInt16()
    $sectionCount = $reader.ReadUInt16()
    $stream.Position = $peOffset + 20
    $optionalSize = $reader.ReadUInt16()
    $optionalOffset = $peOffset + 24
    $stream.Position = $optionalOffset
    $magic = $reader.ReadUInt16()
    $directoryOffset = switch ($magic) {
      0x10b { 96 }
      0x20b { 112 }
      default { throw "Unsupported PE optional header: $Path" }
    }
    $stream.Position = $optionalOffset + $directoryOffset + 8
    $importRva = $reader.ReadUInt32()
    $sections = @()
    for ($index = 0; $index -lt $sectionCount; $index++) {
      $stream.Position = $optionalOffset + $optionalSize + ($index * 40) + 8
      $virtualSize = $reader.ReadUInt32()
      $virtualAddress = $reader.ReadUInt32()
      $rawSize = $reader.ReadUInt32()
      $rawOffset = $reader.ReadUInt32()
      $sections += [pscustomobject]@{
        Address = $virtualAddress
        Size = [Math]::Max($virtualSize, $rawSize)
        Offset = $rawOffset
      }
    }

    function Convert-PeRva {
      param([uint32] $Rva)
      foreach ($section in $sections) {
        if ($Rva -ge $section.Address -and $Rva -lt ($section.Address + $section.Size)) {
          return [long]($section.Offset + $Rva - $section.Address)
        }
      }
      throw "Invalid PE import address in $Path"
    }

    $imports = @()
    if ($importRva -ne 0) {
      $importOffset = Convert-PeRva $importRva
      for ($index = 0; $index -lt 4096; $index++) {
        $stream.Position = $importOffset + ($index * 20) + 12
        $nameRva = $reader.ReadUInt32()
        if ($nameRva -eq 0) { break }
        $stream.Position = Convert-PeRva $nameRva
        $nameBytes = [Collections.Generic.List[byte]]::new()
        do {
          $character = $reader.ReadByte()
          if ($character -ne 0) { $nameBytes.Add($character) }
          if ($nameBytes.Count -gt 260) { throw "Invalid PE import name in $Path" }
        } while ($character -ne 0)
        $imports += [Text.Encoding]::ASCII.GetString($nameBytes.ToArray())
      }
      if ($index -eq 4096) { throw "Unterminated PE import table in $Path" }
    }
    return [pscustomobject]@{ Machine = $machine; Imports = $imports }
  } finally {
    $reader.Dispose()
    $stream.Dispose()
  }
}

function Assert-PeArchitecture {
  param([string] $Path, [int] $ExpectedMachine)
  $info = Get-PeInfo $Path
  if ($info.Machine -ne $ExpectedMachine) {
    throw ('Wrong architecture for {0}: PE machine 0x{1:X4}, expected 0x{2:X4}.' -f $Path, $info.Machine, $ExpectedMachine)
  }
  return $info
}

function Assert-StaticFFmpegImports {
  param([string[]] $Imports)

  # These DLLs belong to Windows. A redistributable found in a developer's
  # System32 directory must not make an otherwise non-static build pass.
  $osDlls = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
  foreach ($name in @(
    'advapi32.dll', 'avicap32.dll', 'avrt.dll', 'bcrypt.dll', 'bcryptprimitives.dll',
    'cfgmgr32.dll', 'comctl32.dll', 'comdlg32.dll', 'crypt32.dll', 'cryptbase.dll', 'cryptsp.dll',
    'd2d1.dll', 'd3d9.dll', 'd3d11.dll', 'd3d12.dll', 'dcomp.dll', 'ddraw.dll', 'dnsapi.dll',
    'dsound.dll', 'dwrite.dll', 'dwmapi.dll', 'dxgi.dll', 'dxva2.dll', 'gdi32.dll', 'gdi32full.dll',
    'imm32.dll', 'iphlpapi.dll', 'kernel32.dll', 'kernelbase.dll', 'ksuser.dll',
    'mf.dll', 'mfplat.dll', 'mfreadwrite.dll', 'mfuuid.dll', 'msacm32.dll', 'msimg32.dll',
    'msvcrt.dll', 'msvcp_win.dll', 'ncrypt.dll', 'netapi32.dll', 'normaliz.dll', 'ntdll.dll',
    'ole32.dll', 'oleaut32.dll', 'pdh.dll', 'powrprof.dll', 'propsys.dll', 'psapi.dll',
    'rpcrt4.dll', 'secur32.dll', 'setupapi.dll', 'shcore.dll', 'shell32.dll', 'shlwapi.dll',
    'ucrtbase.dll', 'user32.dll', 'userenv.dll', 'usp10.dll', 'version.dll',
    'winhttp.dll', 'wininet.dll', 'winmm.dll', 'winnsi.dll', 'winspool.drv', 'wintrust.dll',
    'wldap32.dll', 'ws2_32.dll', 'wtsapi32.dll'
  )) { $null = $osDlls.Add($name) }

  foreach ($import in $Imports) {
    if ($import -match '^(avcodec|avutil|avformat|avfilter|avdevice|swscale|swresample|postproc)(-\d+)?\.dll$') {
      throw "FFmpeg must be a static build; shared FFmpeg library required: $import"
    }
    if ($import -match '^(vcruntime|msvcp|msvcr|concrt|vcomp|mfc|mfcm|atl)\d.*\.dll$' -or
        $import -match '^(libgcc_s|libstdc\+\+|libwinpthread|libgomp|libomp|libssp|clang_rt).*\.dll$') {
      throw "FFmpeg must be a static build; redistributable compiler runtime required: $import"
    }
    if ($import -match '^(api-ms-win-|ext-ms-win-)') { continue }
    if (-not $osDlls.Contains($import)) {
      throw "FFmpeg must be a static build; import is not an allowed Windows OS DLL: $import"
    }
  }
}

function Resolve-FFmpegMetadata {
  param([string] $Path, [string[]] $Names, [string] $Kind)
  if (-not $Path) {
    foreach ($name in $Names) {
      $candidate = Join-Path (Split-Path $FFmpegPath -Parent) $name
      if (Test-Path -LiteralPath $candidate -PathType Leaf) { $Path = $candidate; break }
    }
  }
  if (-not $Path -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    throw "Missing FFmpeg $Kind file. Supply -FFmpeg${Kind}Path for this exact FFmpeg build."
  }
  $resolved = (Resolve-Path -LiteralPath $Path).Path
  if ((Get-Item -LiteralPath $resolved).Length -eq 0) { throw "Empty FFmpeg $Kind file: $resolved" }
  return $resolved
}

$PackagingDir = $PSScriptRoot
$RewriteDir = Split-Path $PackagingDir -Parent
$RepoRoot = Split-Path $RewriteDir -Parent
$EngineDir = Join-Path $RewriteDir 'engine'
$GuiProject = Join-Path $RewriteDir 'gui\UxPlayRs.Gui\UxPlayRs.Gui.csproj'
$OutDir = Join-Path $RewriteDir "out\$Architecture\bundle"
$PublishDir = Join-Path $RewriteDir "out\$Architecture\gui-publish"

$Rid = if ($Architecture -eq 'arm64') { 'win-arm64' } else { 'win-x64' }
$RustTarget = if ($Architecture -eq 'arm64') { 'aarch64-pc-windows-msvc' } else { 'x86_64-pc-windows-msvc' }
$ExpectedMachine = if ($Architecture -eq 'arm64') { 0xaa64 } else { 0x8664 }
$EngineTargetDir = Join-Path $EngineDir 'target'

if (-not $FFmpegPath) {
  $command = Get-Command ffmpeg -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($command) { $FFmpegPath = $command.Source }
}
if (-not $FFmpegPath -or -not (Test-Path -LiteralPath $FFmpegPath -PathType Leaf)) {
  throw 'Missing FFmpeg executable. Supply -FFmpegPath or put ffmpeg.exe on PATH.'
}
$FFmpegPath = (Resolve-Path -LiteralPath $FFmpegPath).Path
$FFmpegLicensePath = Resolve-FFmpegMetadata $FFmpegLicensePath @('FFmpeg-LICENSE.txt', 'LICENSE.txt') 'License'
$FFmpegSourcePath = Resolve-FFmpegMetadata $FFmpegSourcePath @('FFmpeg-SOURCE.txt', 'SOURCE.txt') 'Source'
$ffmpegInfo = Assert-PeArchitecture $FFmpegPath $ExpectedMachine
Assert-StaticFFmpegImports $ffmpegInfo.Imports
$FFmpegVersion = @(& $FFmpegPath -hide_banner -version 2>&1)
if ($LASTEXITCODE -ne 0) { throw "FFmpeg -version failed with exit code $LASTEXITCODE." }
if (($FFmpegVersion -join "`n") -match '(?m)^configuration:.*--enable-shared(?:\s|$)') {
  throw 'FFmpeg must be a static build (--enable-shared is unsupported).'
}
$decoders = @(& $FFmpegPath -hide_banner -decoders 2>&1)
if ($LASTEXITCODE -ne 0 -or -not (($decoders -join "`n") -match '(?m)^\s*V\S{5}\s+h264\s')) {
  throw 'FFmpeg is missing its native h264 decoder.'
}
$encoders = @(& $FFmpegPath -hide_banner -encoders 2>&1)
if ($LASTEXITCODE -ne 0 -or -not (($encoders -join "`n") -match '(?m)^\s*V\S{5}\s+ppm\s')) {
  throw 'FFmpeg is missing the ppm encoder.'
}
$FFmpegHash = (Get-FileHash -LiteralPath $FFmpegPath -Algorithm SHA256).Hash.ToLowerInvariant()

Write-Host "Staging UxPlayRs bundle for $Architecture ($Rid)..."

& cargo build --release --locked -p airplayd --target $RustTarget --target-dir $EngineTargetDir --manifest-path (Join-Path $EngineDir 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE." }

$EngineExe = Join-Path $EngineTargetDir "$RustTarget\release\airplayd.exe"
if (-not (Test-Path -LiteralPath $EngineExe -PathType Leaf)) { throw "Missing engine binary: $EngineExe" }
$null = Assert-PeArchitecture $EngineExe $ExpectedMachine

& dotnet publish $GuiProject -c Release -r $Rid --self-contained true -o $PublishDir
if ($LASTEXITCODE -ne 0) { throw "dotnet publish failed with exit code $LASTEXITCODE." }

$GuiExe = Join-Path $PublishDir 'UxPlayRs.Gui.exe'
if (-not (Test-Path $GuiExe)) { throw "Missing GUI binary: $GuiExe" }

$OutDir = [IO.Path]::GetFullPath($OutDir)
$outRoot = [IO.Path]::GetFullPath((Join-Path $RewriteDir 'out')) + [IO.Path]::DirectorySeparatorChar
if (-not $OutDir.StartsWith($outRoot, [StringComparison]::OrdinalIgnoreCase)) { throw "Unsafe bundle directory: $OutDir" }
if (Test-Path -LiteralPath $OutDir) { Remove-Item -LiteralPath $OutDir -Recurse -Force }
New-Item -ItemType Directory -Path $OutDir -Force | Out-Null

Copy-Item (Join-Path $PublishDir '*') $OutDir -Recurse -Force
Copy-Item $EngineExe (Join-Path $OutDir 'airplayd.exe') -Force
Copy-Item -LiteralPath $FFmpegPath -Destination (Join-Path $OutDir 'ffmpeg.exe') -Force
Copy-Item -LiteralPath $FFmpegLicensePath -Destination (Join-Path $OutDir 'FFmpeg-LICENSE.txt') -Force
Copy-Item -LiteralPath $FFmpegSourcePath -Destination (Join-Path $OutDir 'FFmpeg-SOURCE.txt') -Force
@("sha256=$FFmpegHash", "arch=$Architecture") + $FFmpegVersion |
  Set-Content -LiteralPath (Join-Path $OutDir 'FFmpeg-VERSION.txt') -Encoding utf8
$stagedFFmpeg = Join-Path $OutDir 'ffmpeg.exe'
if ((Get-FileHash -LiteralPath $stagedFFmpeg -Algorithm SHA256).Hash -ne $FFmpegHash) {
  throw 'Staged FFmpeg does not match the validated source executable.'
}
& $stagedFFmpeg -hide_banner -version | Out-Null
if ($LASTEXITCODE -ne 0) { throw "Staged FFmpeg cannot run (exit code $LASTEXITCODE)." }

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
