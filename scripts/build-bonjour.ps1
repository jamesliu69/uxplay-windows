<# Prepares the x64 or ARM64 Bonjour SDK required by uxplay-windows.
   Downloads, patches, and builds dnssd.dll and mDNSResponder when needed. #>
[CmdletBinding()]
param(
    [string]$ProjectRoot,

    [ValidateSet("x64", "arm64")]
    [string]$Architecture = "x64",

    [switch]$Force
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

# 以 .NET API 計算 SHA256，避免 Windows PowerShell 在 PSModulePath 被外部覆寫的環境下載入不到內建模組 Get-FileHash。
function Get-FileSha256Hex {
    param([string]$Path)

    $sha256 = [Security.Cryptography.SHA256]::Create()
    $stream = [IO.File]::OpenRead($Path)
    try {
        return ([BitConverter]::ToString($sha256.ComputeHash($stream))).Replace("-", "").ToLowerInvariant()
    }
    finally {
        $stream.Dispose()
        $sha256.Dispose()
    }
}

if (-not $ProjectRoot) {
    $ProjectRoot = Split-Path -Parent $PSScriptRoot
}
$ProjectRoot = (Resolve-Path -LiteralPath $ProjectRoot).Path

$msbuildPlatform = if ($Architecture -eq "arm64") { "ARM64" } else { "x64" }
$sourcePlatformDirectory = if ($Architecture -eq "arm64") { "arm64" } else { "x64" }
$sdk = Join-Path $ProjectRoot "Bonjour SDK"
$required = @(
    (Join-Path $sdk "Include\dns_sd.h"),
    (Join-Path $sdk "Lib\x64\dnssd.lib"),
    (Join-Path $sdk "Bin\$Architecture\dnssd.dll"),
    (Join-Path $sdk "Bin\$Architecture\mDNSResponder.exe"),
    (Join-Path $sdk "build-info.json")
)
if ($Architecture -eq "arm64") {
    $required += (Join-Path $sdk "Lib\arm64\dnssd.lib")
}

$cacheValid = @($required | Where-Object { -not (Test-Path $_) }).Count -eq 0
if ($cacheValid) {
    try {
        $buildInfo = Get-Content `
            -LiteralPath (Join-Path $sdk "build-info.json") `
            -Raw |
            ConvertFrom-Json
        $cacheValid = $buildInfo.architecture -eq $Architecture
    }
    catch {
        $cacheValid = $false
    }
}

if (-not $Force -and $cacheValid) {
    Write-Host "Bonjour SDK $Architecture is already available."
    return
}

$msbuildCommand = Get-Command msbuild.exe -ErrorAction SilentlyContinue
$msbuild = if ($msbuildCommand) {
    $msbuildCommand.Source
} else {
    $null
}
$vswhere = Join-Path ${env:ProgramFiles(x86)} `
    "Microsoft Visual Studio\Installer\vswhere.exe"
if (-not $msbuild -and (Test-Path -LiteralPath $vswhere)) {
    $msbuild = & $vswhere `
        -latest `
        -products * `
        -requires Microsoft.Component.MSBuild `
        -find "MSBuild\**\Bin\MSBuild.exe" |
        Select-Object -First 1
}

if (-not $msbuild) {
    $candidates = Get-ChildItem `
        -Path "$env:ProgramFiles\Microsoft Visual Studio\2022\*\MSBuild\Current\Bin\MSBuild.exe" `
        -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($candidates) {
        $msbuild = $candidates.FullName
    }
}

if (-not $msbuild) {
    throw (
        "Bonjour SDK is missing and MSBuild was not found. " +
        "Install Visual Studio Build Tools with the C++ workload, then retry."
    )
}

# mDNSResponder 的 vcxproj 預設要求 v143（VS2022）工具集；機器只裝新版 VS 時會報 MSB8020。
# 從 MSBuild.exe 所在的 VS 實例根目錄扫描實際安裝的工具集，用命令列屬性覆寫；
# 有 v143 時仍優先用 v143，否則取編號最新的版本。
function Get-VcPlatformToolset {
    param([string]$MsBuildPath, [string]$Platform)

    $current = $MsBuildPath
    while ($current) {
        $instanceRoot = Split-Path -Parent $current
        $vcRoot = Join-Path $instanceRoot "MSBuild\Microsoft\VC"
        if (Test-Path -LiteralPath $vcRoot) {
            $toolsets = Get-ChildItem -Path $vcRoot -Directory -ErrorAction SilentlyContinue |
                ForEach-Object {
                    Get-ChildItem `
                        -Path (Join-Path $_.FullName "Platforms\$Platform\PlatformToolsets") `
                        -Directory -ErrorAction SilentlyContinue
                } |
                ForEach-Object { $_.Name } |
                Where-Object { $_ -match '^v\d+$' } |
                Select-Object -Unique
            if ($toolsets) {
                if ($toolsets -contains 'v143') { return 'v143' }
                return ($toolsets |
                    Sort-Object -Property { [int]$_.Substring(1) } -Descending |
                    Select-Object -First 1)
            }
            return $null
        }
        $current = $instanceRoot
    }
    return $null
}

$vcToolset = Get-VcPlatformToolset -MsBuildPath $msbuild -Platform $msbuildPlatform
$toolsetArgs = @()
if ($vcToolset) {
    Write-Host "Building Bonjour with VC platform toolset $vcToolset."
    $toolsetArgs = @("/p:PlatformToolset=$vcToolset")
}
else {
    Write-Warning "Unable to detect an installed VC platform toolset; using the vcxproj default."
}

$architectureOutDir = Join-Path $ProjectRoot "out\$Architecture"
$sourceDir = Join-Path $architectureOutDir "bonjour-source"
if (Test-Path -LiteralPath $sourceDir) {
    $resolvedSource = [IO.Path]::GetFullPath($sourceDir)
    $expectedRoot = [IO.Path]::GetFullPath($architectureOutDir)
    if (-not $resolvedSource.StartsWith(
        $expectedRoot,
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw "Refusing to remove unexpected Bonjour source path: $resolvedSource"
    }
    Remove-Item -LiteralPath $resolvedSource -Recurse -Force
}
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $sourceDir) |
    Out-Null

& git clone `
    --branch rel/mDNSResponder-2881 `
    --depth 1 `
    --single-branch `
    https://github.com/apple-oss-distributions/mDNSResponder.git `
    $sourceDir
if ($LASTEXITCODE -ne 0) {
    throw "Unable to clone mDNSResponder."
}

& git -C $sourceDir apply `
    (Join-Path $ProjectRoot "mdnsresponder-patches\2881.patch")
if ($LASTEXITCODE -ne 0) {
    throw "Unable to apply the mDNSResponder patch."
}

# mDNSResponder 原始碼在字串字面值內含 UTF-8 彎引號；非英文語系的 Windows 上 MSVC 會以
# 本機字碼頁（如繁中 CP950）解讀而吞掉引號（C2001）。藉 CL 環境變數附加 /utf-8，
# 僅作用於這兩次建置，結束後還原，不影響同一程序內後續的建置步驟。
$previousClEnvironment = $env:CL
$env:CL = ("$previousClEnvironment /utf-8").Trim()
try {
    & $msbuild `
        (Join-Path $sourceDir "mDNSWindows\DLL\dnssd.vcxproj") `
        /m /t:Build /p:Configuration=Release /p:Platform=$msbuildPlatform @toolsetArgs
    if ($LASTEXITCODE -ne 0) {
        throw "dnssd.dll build failed."
    }

    & $msbuild `
        (Join-Path $sourceDir "mDNSWindows\SystemService\mDNSResponder.vcxproj") `
        /m /t:Build /p:Configuration=Release /p:Platform=$msbuildPlatform @toolsetArgs
    if ($LASTEXITCODE -ne 0) {
        throw "mDNSResponder.exe build failed."
    }
}
finally {
    if ($null -ne $previousClEnvironment) {
        $env:CL = $previousClEnvironment
    }
    else {
        Remove-Item Env:CL -ErrorAction SilentlyContinue
    }
}

New-Item -ItemType Directory -Force -Path (Join-Path $sdk "Include") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $sdk "Lib\x64") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $sdk "Bin\$Architecture") |
    Out-Null
if ($Architecture -eq "arm64") {
    New-Item -ItemType Directory -Force -Path (Join-Path $sdk "Lib\arm64") |
        Out-Null
}

Copy-Item `
    (Join-Path $sourceDir "mDNSShared\dns_sd.h") `
    (Join-Path $sdk "Include\dns_sd.h") `
    -Force
Copy-Item `
    (Join-Path $sourceDir "mDNSWindows\DLL\$sourcePlatformDirectory\Release\dnssd.lib") `
    (Join-Path $sdk "Lib\x64\dnssd.lib") `
    -Force
if ($Architecture -eq "arm64") {
    # libuxplay currently looks under Lib\x64 on Windows. Keep the native
    # location as well as the compatibility alias until it is configurable.
    Copy-Item `
        (Join-Path $sourceDir "mDNSWindows\DLL\$sourcePlatformDirectory\Release\dnssd.lib") `
        (Join-Path $sdk "Lib\arm64\dnssd.lib") `
        -Force
}
Copy-Item `
    (Join-Path $sourceDir "mDNSWindows\DLL\$sourcePlatformDirectory\Release\dnssd.dll") `
    (Join-Path $sdk "Bin\$Architecture\dnssd.dll") `
    -Force
Copy-Item `
    (Join-Path $sourceDir "mDNSWindows\SystemService\$sourcePlatformDirectory\Release\mDNSResponder.exe") `
    (Join-Path $sdk "Bin\$Architecture\mDNSResponder.exe") `
    -Force

$stamp = [ordered]@{
    source = "apple-oss-distributions/mDNSResponder"
    tag = "rel/mDNSResponder-2881"
    architecture = $Architecture
    patchSha256 = (
        Get-FileSha256Hex (Join-Path $ProjectRoot "mdnsresponder-patches\2881.patch")
    )
}
$stamp | ConvertTo-Json |
    Set-Content (Join-Path $sdk "build-info.json") -Encoding utf8

Write-Host "Bonjour SDK $Architecture built successfully."
