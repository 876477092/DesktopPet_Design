<#
.SYNOPSIS
    构建桌面宠物发布包：vite build（双入口） + tauri build → 产出 DesktopPet.exe 与 NSIS 安装包。

.DESCRIPTION
    对应任务 T-01（S1-M1）。会依次：
      1. 校验 Node / pnpm；
      2. 按需导入 MSVC 构建环境（通过 $env:VCVARS64 或 vswhere 自动探测，遵循 C1）；
      3. 执行 `tauri build`（内部先跑 `pnpm exec vite build`）。

.PARAMETER Debug
    以 debug 配置构建（产物更快、体积更大）。

.EXAMPLE
    ./scripts/build.ps1
#>
#Requires -Version 5.1
[CmdletBinding()]
param(
    [switch] $Debug
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot

function Write-Step {
    param([Parameter(Mandatory = $true)][string] $Message)
    Write-Host "[build] $Message" -ForegroundColor Cyan
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
        Write-Host "[build] 未找到 vcvars64.bat，跳过 MSVC 环境导入（如 link 阶段报错请设置 `$env:VCVARS64）" -ForegroundColor Yellow
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
    Write-Step "工程根目录：$repoRoot"
    Assert-Command -Name 'node'

    if ($null -ne (Get-Command 'pnpm' -ErrorAction SilentlyContinue)) {
        $runner = @('pnpm', 'exec')
    }
    else {
        Write-Host "[build] 未检测到 pnpm，回退使用 npx。" -ForegroundColor Yellow
        $runner = @('npx', '--no-install')
    }

    Import-MsvcEnvironment

    $args = @('tauri', 'build')
    if ($Debug) { $args += '--debug' }

    Write-Step "执行：$($runner -join ' ') $($args -join ' ')"
    & $runner[0] $runner[1] $args
    if ($LASTEXITCODE -ne 0) { throw "tauri build 失败（exit=$LASTEXITCODE）" }

    Write-Step '构建完成。'
}
finally {
    Pop-Location
}
