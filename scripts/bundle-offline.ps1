<#
.SYNOPSIS
    制作 DesktopPet 安装包：离线标准包（默认，内嵌 WebView2 离线运行时）与精简包（可选，依赖系统 WebView2）。
    对应 03 台账 S6-M3 卡（T-17）。

.DESCRIPTION
    离线标准包（-Offline，默认）：
      - `tauri build` 按 tauri.conf.json 的 `webviewInstallMode=offlineInstaller` 打包，
        构建期自动下载并缓存 Microsoft 官方 WebView2 离线安装器（微软官方链接，
        仅构建期联网一次，安装全程零联网），产物 ≈155MB。
    精简包（-Lite）：
      - 以 `TAURI_CONFIG` 环境变量覆盖为 `webviewInstallMode=skip`（依赖系统 WebView2，
        Win10/11 内置），产物 ≈25MB；安装器经 installer.nsh 安装前检测 WebView2 注册表。
    产物输出到 `src-tauri/target/release/bundle/nsis/`，随后自动复制到 `dist-install/`。

.PARAMETER Lite
    构建精简包（跳过内嵌 WebView2 运行时）。
.PARAMETER Config
    cargo 构建配置（默认 release；调试包请显式传 debug）。
.PARAMETER SkipSign
    构建完成后跳过签名（默认：未配置证书信息时自动跳过并告警）。

.EXAMPLE
    ./scripts/bundle-offline.ps1                  # 离线标准包
    ./scripts/bundle-offline.ps1 -Lite            # 精简包
    ./scripts/bundle-offline.ps1 -Sign -CertificatePath .\cert.pfx -CertificatePassword xxx
#>
#Requires -Version 5.1
[CmdletBinding()]
param(
    [switch] $Lite,
    [ValidateSet('release', 'debug')]
    [string] $Config = 'release',
    [switch] $Sign,
    [string] $CertificatePath = '',
    [string] $CertificatePassword = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$confPath = Join-Path $repoRoot 'src-tauri\tauri.conf.json'

function Write-Step {
    param([Parameter(Mandatory = $true)][string] $Message)
    Write-Host "[bundle-offline] $Message" -ForegroundColor Cyan
}

function Assert-Command {
    param([Parameter(Mandatory = $true)][string] $Name)
    if ($null -eq (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "未找到命令：$Name。请先安装后重试。"
    }
}

function Import-MsvcEnvironment {
    $vcvars = $env:VCVARS64
    if ([string]::IsNullOrWhiteSpace($vcvars)) {
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
        if (Test-Path -LiteralPath $vswhere) {
            $installPath = & $vswhere -latest -products * `
                -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
                -property installationPath 2>$null | Select-Object -First 1
            if (-not [string]::IsNullOrWhiteSpace($installPath)) {
                $candidate = Join-Path $installPath 'VC\Auxiliary\Build\vcvars64.bat'
                if (Test-Path -LiteralPath $candidate) { $vcvars = $candidate }
            }
        }
    }
    if ([string]::IsNullOrWhiteSpace($vcvars) -or -not (Test-Path -LiteralPath $vcvars)) {
        Write-Host "[bundle-offline] 未找到 vcvars64.bat，跳过 MSVC 环境导入（link 报错时请设置 `$env:VCVARS64）" -ForegroundColor Yellow
        return
    }
    Write-Step "导入 MSVC 环境：$vcvars"
    $dump = & cmd.exe /c "`"$vcvars`" >nul 2>&1 && set"
    foreach ($line in $dump) {
        if ($line -match '^(?<name>[^=]+)=(?<value>.*)$') {
            Set-Item -Path "Env:$($Matches.name)" -Value $Matches.value
        }
    }
}

Push-Location -LiteralPath $repoRoot
try {
    Assert-Command -Name 'node'
    $runner = if ($null -ne (Get-Command 'pnpm' -ErrorAction SilentlyContinue)) { @('pnpm', 'exec') }
              else { @('npx', '--no-install') }
    Import-MsvcEnvironment

    # 校验配置文件存在（C1 不硬编码盘符）。
    if (-not (Test-Path -LiteralPath $confPath)) {
        throw "未找到 tauri.conf.json：$confPath"
    }

    # 精简包：TAURI_CONFIG 深度合并覆盖 webviewInstallMode=skip（离线包保持 conf 的 offlineInstaller）。
    $env:TAURI_CONFIG = ''
    if ($Lite) {
        Write-Step '精简包模式：TAURI_CONFIG 覆盖 webviewInstallMode=skip（依赖系统 WebView2）'
        $env:TAURI_CONFIG = '{"bundle":{"windows":{"webviewInstallMode":{"type":"skip"}}}}'
    }
    else {
        Write-Step '离线标准包模式：内嵌 WebView2 离线运行时（offlineInstaller，构建期自动下载缓存）'
    }

    # 执行构建（内部先跑 `pnpm exec vite build`）。
    $args = @('tauri', 'build')
    if ($Config -eq 'debug') { $args += '--debug' }
    Write-Step "执行：$($runner -join ' ') $($args -join ' ')"
    & $runner[0] $runner[1] $args
    if ($LASTEXITCODE -ne 0) { throw "tauri build 失败（exit=$LASTEXITCODE）" }

    # 定位产物（NSIS 安装包）。
    $arch = if ($env:PROCESSOR_ARCHITECTURE -match 'ARM64') { 'aarch64' } else { 'x64' }
    $nsisDir = Join-Path $repoRoot "src-tauri\target\$Config\bundle\nsis"
    $setup = Get-ChildItem -LiteralPath $nsisDir -Filter '*-setup.exe' | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if ($null -eq $setup) { throw "未找到 NSIS 安装包产物：$nsisDir" }

    $outDir = Join-Path $repoRoot 'dist-install'
    New-Item -ItemType Directory -Path $outDir -Force | Out-Null
    $suffix = if ($Lite) { 'lite-setup' } else { 'offline-setup' }
    $dest = Join-Path $outDir ("DesktopPet-0.1.0-$arch-$suffix.exe")
    Copy-Item -LiteralPath $setup.FullName -Destination $dest -Force
    Write-Step "安装包已产出：$dest（$([math]::Round($setup.Length / 1MB, 1)) MB）"

    # 计算哈希（供签名/杀软白名单提报）。
    $hash = (Get-FileHash -LiteralPath $dest -Algorithm SHA256).Hash
    Write-Step "SHA256：$hash"

    # 签名（可选；未配置证书时自动跳过——本地交付/内测不阻塞）。
    if ($Sign) {
        $signArgs = @('-InstallerPath', $dest)
        if ($CertificatePath) {
            $signArgs += @('-CertificatePath', $CertificatePath)
            if ($CertificatePassword) { $signArgs += @('-CertificatePassword', $CertificatePassword) }
        }
        & (Join-Path $PSScriptRoot 'sign.ps1') @signArgs
        if ($LASTEXITCODE -ne 0) { throw '签名失败（见 sign.ps1 输出）' }
    }
    else {
        Write-Host "[bundle-offline] 未请求签名（-Sign），跳过（正式分发请先配置 OV/EV 证书）。" -ForegroundColor Yellow
    }

    Write-Step '打包完成。'
}
finally {
    Remove-Item Env:TAURI_CONFIG -ErrorAction SilentlyContinue
    Pop-Location
}
