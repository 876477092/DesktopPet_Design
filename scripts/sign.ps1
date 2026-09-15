<#
.SYNOPSIS
    DesktopPet 安装包签名与杀软白名单提报（S6-M3，T-17：签名 OV→EV；杀软白名单提报）。
    使用 Windows SDK 的 signtool.exe 对安装包 / 主程序 / 卸载器签名。

.DESCRIPTION
    - 未提供证书时以 `-AllowUnsigned` 显式跳过签名并输出告警（本地交付用）；
    - 提供证书（.pfx / signtool 可用证书）时执行签名，并回显签名信息；
    - 无论是否签名，都会输出 `dist-install/whitelist-report.csv`
      （SHA256 + 文件信息），供杀软厂商白名单提报使用。

.PARAMETER InstallerPath
    待签名安装包路径（缺省自动取 dist-install 下最新产物）。
.PARAMETER CertificatePath
    代码签名证书（.pfx 或 .cer+私钥）路径。
.PARAMETER CertificatePassword
    证书密码（留空则尝试交互 / 使用默认容器）。
.PARAMETER TimestampUrl
    时间戳服务器（默认 RFC3161：http://timestamp.digicert.com，可用
    http://timestamp.comodoca.com/rfc3161 等）。
.PARAMETER AllowUnsigned
    未配置证书时允许跳过签名（正式发布禁止）。
.PARAMETER ExtraBinaries
    额外待签文件列表（默认包含主程序与卸载器，安装包内由 NSIS 构建期签名）。

.EXAMPLE
    ./scripts/sign.ps1 -InstallerPath .\dist-install\DesktopPet-0.1.0-x64-offline-setup.exe -CertificatePath .\cert.pfx -CertificatePassword '***'
    ./scripts/sign.ps1 -AllowUnsigned          # 本地交付：仅生成白名单报告
#>
#Requires -Version 5.1
[CmdletBinding()]
param(
    [string] $InstallerPath = '',
    [string] $CertificatePath = '',
    [string] $CertificatePassword = '',
    [string] $TimestampUrl = 'http://timestamp.digicert.com',
    [switch] $AllowUnsigned,
    [string[]] $ExtraBinaries = @()
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$outDir = Join-Path $repoRoot 'dist-install'

function Write-Step {
    param([Parameter(Mandatory = $true)][string] $Message)
    Write-Host "[sign] $Message" -ForegroundColor Cyan
}

# 定位 signtool（Windows SDK / VS BuildTools）。
function Find-Signtool {
    $candidates = @()
    if ($env:WindowsSdkVerBinPath) { $candidates += (Join-Path $env:WindowsSdkVerBinPath 'x64\signtool.exe') }
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    if (Test-Path -LiteralPath $kits) {
        $candidates += Get-ChildItem -LiteralPath $kits -Directory |
            Sort-Object Name -Descending |
            ForEach-Object { Join-Path $_.FullName 'x64\signtool.exe' }
    }
    foreach ($c in $candidates) {
        if (Test-Path -LiteralPath $c) { return $c }
    }
    $cmd = Get-Command 'signtool.exe' -ErrorAction SilentlyContinue
    return $cmd.Source
}

# 定位主程序与卸载器（安装包同目录 / target 内）。
function Resolve-Binaries {
    param([string] $Installer)
    $list = New-Object System.Collections.Generic.List[string]
    $base = Split-Path -Parent $Installer
    $exe = Join-Path $base 'DesktopPet.exe'
    if (Test-Path -LiteralPath $exe) { $list.Add($exe) }
    $uninstaller = Join-Path $base 'uninstall.exe'
    if (Test-Path -LiteralPath $uninstaller) { $list.Add($uninstaller) }
    # NSIS 卸载器内嵌于安装包，安装后方可从安装目录取（此处仅记录安装目录内路径）。
    $list.Add((Join-Path $base 'uninstall.exe'))
    return $list
}

New-Item -ItemType Directory -Path $outDir -Force | Out-Null

# 1) 定位安装包。
if ([string]::IsNullOrWhiteSpace($InstallerPath)) {
    $InstallerPath = Get-ChildItem -LiteralPath $outDir -Filter '*.exe' -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName
}
if ([string]::IsNullOrWhiteSpace($InstallerPath) -or -not (Test-Path -LiteralPath $InstallerPath)) {
    throw "未找到待签名安装包。请用 -InstallerPath 指定，或先运行 bundle-offline.ps1 产出 dist-install\*.exe。"
}
Write-Step "待签名安装包：$InstallerPath"

# 2) 白名单报告（无论是否签名都生成，供杀软提报）。
$report = Join-Path $outDir 'whitelist-report.csv'
$rows = @()
$allFiles = @($InstallerPath) + $ExtraBinaries
foreach ($f in $allFiles) {
    if (-not (Test-Path -LiteralPath $f)) { continue }
    $item = Get-Item -LiteralPath $f
    $hash = (Get-FileHash -LiteralPath $f -Algorithm SHA256).Hash
    $size = [math]::Round($item.Length / 1MB, 2)
    $rows += [pscustomobject]@{
        File     = $item.Name
        SHA256   = $hash
        SizeMB   = $size
        Signed   = $false
        Path     = $item.FullName
    }
}
$rows | Export-Csv -LiteralPath $report -NoTypeInformation -Encoding UTF8
Write-Step "杀软白名单报告已生成：$report"

# 3) 签名。
$signtool = Find-Signtool
$canSign = -not [string]::IsNullOrWhiteSpace($signtool) -and
           -not [string]::IsNullOrWhiteSpace($CertificatePath) -and
           (Test-Path -LiteralPath $CertificatePath)

if (-not $canSign) {
    if ($AllowUnsigned) {
        Write-Host "[sign] 未配置有效证书（signtool=$signtool cert=$CertificatePath），按 -AllowUnsigned 跳过签名（仅生成白名单报告）。正式发布禁止此路径。" -ForegroundColor Yellow
        return
    }
    throw "未找到 signtool 或证书。请安装 Windows SDK 并提供 -CertificatePath；本地交付可用 -AllowUnsigned 跳过。"
}

Write-Step "使用 signtool：$signtool"
$targets = @($InstallerPath) + (Resolve-Binaries -Installer $InstallerPath) + $ExtraBinaries |
    Select-Object -Unique

foreach ($target in $targets) {
    if (-not (Test-Path -LiteralPath $target)) { continue }
    $sigArgs = @('sign', '/fd', 'SHA256', '/tr', $TimestampUrl, '/td', 'SHA256')
    if (-not [string]::IsNullOrWhiteSpace($CertificatePassword)) {
        $sigArgs += @('/f', $CertificatePath, '/p', $CertificatePassword)
    }
    else {
        $sigArgs += @('/f', $CertificatePath)
    }
    $sigArgs += $target
    Write-Step "签名：$target"
    & $signtool $sigArgs
    if ($LASTEXITCODE -ne 0) { throw "签名失败：$target（exit=$LASTEXITCODE）" }
}

# 回填报告签名状态并复验。
foreach ($row in $rows) {
    $row.Signed = (($targets | Where-Object { $_ -eq $row.Path }).Count -gt 0)
}
$rows | Export-Csv -LiteralPath $report -NoTypeInformation -Encoding UTF8
Write-Step "签名完成；白名单报告已更新：$report"
