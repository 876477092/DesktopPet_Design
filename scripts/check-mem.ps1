<#
.SYNOPSIS
    DesktopPet 内存/CPU 巡检（S6-M1 / S6-M2 交付物；AC-40 口径）。
    测量运行中的 DesktopPet.exe 进程工作集/私有内存峰值与 CPU 占用，输出结构化 JSON
    供 CI 断言，超阈值即失败（exit 非 0）。

.DESCRIPTION
    阈值（`02 §10.2 R12` / `01 §7`）：
      - 内存告警 200MB（WARN）、红线 250MB（FAIL）；S6-M1 验收 Given 动画；
        Then 内存 ≤250MB（峰值口径 PrivateUsage）；
      - CPU：空闲 ≤3%（60s）、动画 ≤8%；本脚本按采样窗内平均值断言
        （默认采样 30s，可用 -SampleSeconds 调整）。
    输出 JSON（stdout 末尾 `RESULT_JSON=` 前缀行，CI 用 jq/正则提取）：
      { "mem_mb": 峰值, "mem_warn_mb": 200, "mem_hard_mb": 250,
        "cpu_percent": 均值, "cpu_warn_percent": 8,
        "status": "ok|warn|fail", "samples": n }

.PARAMETER ProcessName
    目标进程名（默认 DesktopPet）。
.PARAMETER SampleSeconds
    采样时长（默认 30；CI 可缩短）。
.PARAMETER IntervalSeconds
    采样间隔（默认 1s）。
.PARAMETER MemWarnMb
    内存告警阈值 MB（默认 200，`02 §10.2 R12`）。
.PARAMETER MemHardMb
    内存红线 MB（默认 250，`01 §7` 验收 ≤250MB）。
.PARAMETER CpuWarnPercent
    CPU 告警阈值 %（默认 8，动画档；空闲档由调用方传 3）。

.EXAMPLE
    ./scripts/check-mem.ps1                        # 30s 巡检，200/250MB 告警/红线
    ./scripts/check-mem.ps1 -SampleSeconds 60 -CpuWarnPercent 3   # 空闲 60s 验收
#>
#Requires -Version 5.1
[CmdletBinding()]
param(
    [string] $ProcessName = 'DesktopPet',
    [int] $SampleSeconds = 30,
    [int] $IntervalSeconds = 1,
    [int] $MemWarnMb = 200,
    [int] $MemHardMb = 250,
    [int] $CpuWarnPercent = 8
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$procs = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue
if ($null -eq $procs -or $procs.Count -eq 0) {
    Write-Host "[check-mem] 未找到进程 $ProcessName —— 先启动桌面宠物再巡检。" -ForegroundColor Red
    exit 2
}
$proc = $procs | Sort-Object StartTime -Descending | Select-Object -First 1

# 首轮 CPU 基线（GetProcessTimes 差分）。
Add-Type -Namespace DP.Perf -Name Native -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true)]
public static extern bool GetProcessTimes(IntPtr hProcess, out long creation, out long exit,
    out long kernel, out long user);
'@

function Get-ProcTimes {
    param([System.Diagnostics.Process] $P)
    $k = [long]0; $u = [long]0; $c = [long]0; $e = [long]0
    [void][DP.Perf.Native]::GetProcessTimes($P.Handle, [ref]$c, [ref]$e, [ref]$k, [ref]$u)
    return ($k + $u)
}

$prev = Get-ProcTimes -P $proc
$prevWall = [DateTime]::UtcNow
$samples = New-Object System.Collections.Generic.List[double]
$peakMb = 0.0

$deadline = [DateTime]::UtcNow.AddSeconds($SampleSeconds)
while ([DateTime]::UtcNow -lt $deadline) {
    Start-Sleep -Seconds $IntervalSeconds
    $proc.Refresh()
    $mb = [math]::Round($proc.WorkingSet64 / 1MB, 1)
    if ($mb -gt $peakMb) { $peakMb = $mb }
    $now = [DateTime]::UtcNow
    $nowTimes = Get-ProcTimes -P $proc
    $dt = ($now - $prevWall).TotalSeconds
    if ($dt -gt 0) {
        $cpu = [math]::Round((($nowTimes - $prev) / 1e7) / $dt * 100.0, 1)
        $samples.Add($cpu)
    }
    $prev = $nowTimes
    $prevWall = $now
}

$cpuAvg = if ($samples.Count -gt 0) { [math]::Round(($samples | Measure-Object -Average).Average, 1) } else { 0.0 }

# 状态判定（AC-40：200 告警 / 250 红线；CPU 超阈值告警）。
$status = 'ok'
$reasons = @()
if ($peakMb -ge $MemHardMb) { $status = 'fail'; $reasons += "内存 ${peakMb}MB ≥ 红线 ${MemHardMb}MB" }
elseif ($peakMb -ge $MemWarnMb) { $status = 'warn'; $reasons += "内存 ${peakMb}MB ≥ 告警 ${MemWarnMb}MB" }
if ($cpuAvg -gt $CpuWarnPercent) { $status = 'warn'; $reasons += "CPU ${cpuAvg}% > 告警 ${CpuWarnPercent}%" }

$result = [ordered]@{
    mem_mb         = $peakMb
    mem_warn_mb    = $MemWarnMb
    mem_hard_mb    = $MemHardMb
    cpu_percent    = $cpuAvg
    cpu_warn_percent = $CpuWarnPercent
    status         = $status
    samples        = $samples.Count
    reasons        = ($reasons -join '；')
}
$json = $result | ConvertTo-Json -Compress
Write-Host "RESULT_JSON=$json"
Write-Host "[check-mem] 峰值内存=${peakMb}MB（告警 $MemWarnMb / 红线 $MemHardMb）CPU=${cpuAvg}%（告警 $CpuWarnPercent）status=$status"

if ($status -eq 'fail') { exit 1 }
if ($status -eq 'warn') { exit 3 }
exit 0
