<#
.SYNOPSIS
    启动桌面宠物开发模式：vite dev server + Tauri 两个声明式窗口（pet / settings）。

.DESCRIPTION
    对应任务 T-01（S1-M1）。会依次：
      1. 校验 Node / pnpm 可用性（pnpm 缺失时尝试用 corepack 就地启用）；
      2. 按需导入 MSVC 构建环境变量（通过 $env:VCVARS64 或 vswhere 自动探测，
         脚本内不硬编码任何盘符路径，遵循 C1）；
      3. 首次运行自动执行依赖安装；
      4. 调用 `tauri dev`。

.EXAMPLE
    ./scripts/dev.ps1
#>
#Requires -Version 5.1
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot

function Write-Step {
    param([Parameter(Mandatory = $true)][string] $Message)
    Write-Host "[dev] $Message" -ForegroundColor Cyan
}

function Import-MsvcEnvironment {
    <#
    .SYNOPSIS
        导入 MSVC x64 构建环境（link.exe 等），失败仅告警不中断。
    #>
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
        Write-Host "[dev] 未找到 vcvars64.bat，跳过 MSVC 环境导入（如 link 阶段报错请设置 `$env:VCVARS64）" -ForegroundColor Yellow
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

function Assert-Command {
    param([Parameter(Mandatory = $true)][string] $Name)
    if ($null -eq (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "未找到命令：$Name。请先安装后重试。"
    }
}

Push-Location -LiteralPath $repoRoot
try {
    Write-Step "工程根目录：$repoRoot"
    Assert-Command -Name 'node'

    if ($null -eq (Get-Command 'pnpm' -ErrorAction SilentlyContinue)) {
        Write-Step '未检测到 pnpm，尝试用 corepack 启用…'
        & corepack enable pnpm
        Assert-Command -Name 'pnpm'
    }

    if (-not (Test-Path -LiteralPath (Join-Path $repoRoot 'node_modules'))) {
        Write-Step '首次运行：安装前端依赖…'
        & pnpm install
        if ($LASTEXITCODE -ne 0) { throw "pnpm install 失败（exit=$LASTEXITCODE）" }
    }

    Import-MsvcEnvironment

    Write-Step '启动 tauri dev（pet / settings 双窗口）…'
    # 用 pnpm run 而非 pnpm exec：部分环境下 pnpm exec 会破坏 node_modules/.bin 垫片
    & pnpm run tauri:dev
    if ($LASTEXITCODE -ne 0) { throw "tauri dev 失败（exit=$LASTEXITCODE）" }
}
finally {
    Pop-Location
}
