<# 建置 UxPlayRs（rewrite）的單一入口腳本。
   預設只建置 WPF GUI（UxPlayRs.Gui.exe）；
   加 -Bundle 會重編 Rust 引擎（airplayd）並組出完整可執行資料夾；
   再加 -Zip 額外產生可攜式壓縮包。 #>
[CmdletBinding()]
param(
    [ValidateSet("x64", "arm64")]
    [string]$Architecture = "x64",

    # 重編 airplayd 引擎並組出完整 bundle（rewrite\out\<arch>\bundle）
    [switch]$Bundle,

    # 搭配 -Bundle：額外產生 uxplay-rs-<arch>.zip
    [switch]$Zip,

    # bundle 模式：靜態版 ffmpeg.exe 路徑；未提供時使用 PATH 上的 ffmpeg
    [string]$FFmpegPath,

    # ffmpeg 授權說明檔；未提供時找 ffmpeg 同目錄的 FFmpeg-LICENSE.txt 或 LICENSE.txt
    [string]$FFmpegLicensePath,

    # ffmpeg 來源說明檔；未提供時找 ffmpeg 同目錄的 FFmpeg-SOURCE.txt 或 SOURCE.txt
    [string]$FFmpegSourcePath
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$projectRoot = $PSScriptRoot
$guiProject = Join-Path $projectRoot "rewrite\gui\UxPlayRs.Gui\UxPlayRs.Gui.csproj"
$packagingDir = Join-Path $projectRoot "rewrite\packaging"
$guiExe = Join-Path $projectRoot "rewrite\gui\UxPlayRs.Gui\bin\Release\net8.0-windows\UxPlayRs.Gui.exe"

function Assert-CommandAvailable {
    param([string]$Name, [string]$InstallHint)

    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "找不到 $Name。$InstallHint"
    }
}

# GUI 執行中會鎖住建置輸出檔，先確認沒有執行個體。
function Assert-GuiNotRunning {
    $processes = Get-Process -Name "UxPlayRs.Gui" -ErrorAction SilentlyContinue
    if ($processes) {
        throw (
            "UxPlayRs.Gui 正在執行中（PID $($processes.Id -join ', ')）。" +
            "請從系統匣右鍵選單 Exit 後再建置。"
        )
    }
}

# 依 ffmpeg 同目錄的慣用檔名解析授權／來源說明檔；都沒有時給出可行動的錯誤提示。
function Resolve-FFmpegMetadataPath {
    param(
        [string]$BasePath,
        [string]$Current,
        [string[]]$Names,
        [string]$Label,
        [string]$OptionName
    )

    if ($Current -and (Test-Path -LiteralPath $Current -PathType Leaf)) {
        return $Current
    }
    foreach ($name in $Names) {
        $candidate = Join-Path (Split-Path -Parent $BasePath) $name
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return $candidate
        }
    }
    throw (
        "ffmpeg 同目錄缺少${Label}說明檔（需要 $($Names -join ' 或 ')）。" +
        "winget 安裝的 Gyan 版不含這些檔案；請改用附帶授權檔的靜態版（如 BtbN release），" +
        "或以 -$OptionName 參數自行指定。"
    )
}

function Build-Gui {
    Assert-CommandAvailable -Name "dotnet" `
        -InstallHint "請安裝 .NET 8 SDK：https://dotnet.microsoft.com/download"
    Assert-GuiNotRunning

    & dotnet build $guiProject -c Release -nologo -v minimal
    if ($LASTEXITCODE -ne 0) {
        throw "GUI 建置失敗（exit $LASTEXITCODE）。"
    }
    Write-Host ""
    Write-Host "GUI 建置完成：$guiExe"
}

function Invoke-Bundle {
    Assert-CommandAvailable -Name "dotnet" `
        -InstallHint "請安裝 .NET 8 SDK：https://dotnet.microsoft.com/download"
    Assert-CommandAvailable -Name "cargo" `
        -InstallHint "請安裝 Rust（MSVC 工具鏈）：https://rustup.rs"
    Assert-GuiNotRunning

    $ffmpeg = $FFmpegPath
    if (-not $ffmpeg) {
        $command = Get-Command ffmpeg -CommandType Application -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($command) {
            $ffmpeg = $command.Source
        }
    }
    if (-not $ffmpeg -or -not (Test-Path -LiteralPath $ffmpeg -PathType Leaf)) {
        throw "找不到 ffmpeg.exe。請提供 -FFmpegPath 指向靜態版 ffmpeg。"
    }
    $licensePath = Resolve-FFmpegMetadataPath `
        -BasePath $ffmpeg -Current $FFmpegLicensePath `
        -Names @("FFmpeg-LICENSE.txt", "LICENSE.txt") `
        -Label "授權" -OptionName "FFmpegLicensePath"
    $sourcePath = Resolve-FFmpegMetadataPath `
        -BasePath $ffmpeg -Current $FFmpegSourcePath `
        -Names @("FFmpeg-SOURCE.txt", "SOURCE.txt") `
        -Label "來源" -OptionName "FFmpegSourcePath"

    Write-Host "建置引擎與 bundle（cargo + dotnet publish）..."
    & (Join-Path $packagingDir "stage.ps1") `
        -Architecture $Architecture `
        -FFmpegPath $ffmpeg `
        -FFmpegLicensePath $licensePath `
        -FFmpegSourcePath $sourcePath
    if (-not $?) {
        throw "stage.ps1 執行失敗。"
    }
    if ($Zip) {
        & (Join-Path $packagingDir "zip.ps1") -Architecture $Architecture
        if (-not $?) {
            throw "zip.ps1 執行失敗。"
        }
    }
    $bundleDir = Join-Path $projectRoot "rewrite\out\$Architecture\bundle"
    Write-Host ""
    Write-Host "bundle 完成：$(Join-Path $bundleDir 'UxPlayRs.Gui.exe')"
    if (-not $Zip) {
        Write-Host "要產生壓縮包請加 -Zip 再跑一次（會直接重用已完成的 bundle）。"
    }
}

if ($Bundle) {
    Invoke-Bundle
}
else {
    Build-Gui
}
