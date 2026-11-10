# What RustyBar costs: memory, CPU, wake-ups, GPU and child processes of each running bar, over
# a few seconds of watching. The numbers come from WMI's performance classes, which have the
# same names in every language of Windows (the counters of Get-Counter don't).
#
#   scripts\measure.ps1                 # every rustybar process, 30 s
#   scripts\measure.ps1 -Id 1234 -Seconds 60

param(
    [int]$Id = 0,
    [int]$Seconds = 30
)

$ErrorActionPreference = 'Stop'

$procs = if ($Id) { @(Get-Process -Id $Id) } else { @(Get-Process -Name rustybar -ErrorAction SilentlyContinue) }
if (-not $procs) {
    Write-Output "no rustybar process"
    exit 1
}
$cores = [Environment]::ProcessorCount

function Switches($procId) {
    # Raw thread counters are running totals, by thread: two readings give the wake-ups in between
    $totals = @{}
    foreach ($t in Get-CimInstance Win32_PerfRawData_PerfProc_Thread -Filter "IDProcess = $procId") {
        $totals[[int]$t.IDThread] = [long]$t.ContextSwitchesPersec
    }
    $totals
}

function SwitchesSince($before, $now) {
    # Thread by thread: a thread that ended took its count with it (an idle worker of a thread pool
    # going away), one that started counts from zero
    $sum = 0
    foreach ($id in $now.Keys) {
        $sum += $now[$id] - $(if ($before.ContainsKey($id)) { $before[$id] } else { 0 })
    }
    $sum
}

$start = @{}
foreach ($p in $procs) {
    $start[$p.Id] = @{ Cpu = $p.TotalProcessorTime; Switches = (Switches $p.Id) }
}
$t0 = Get-Date
Start-Sleep -Seconds $Seconds
$elapsed = ((Get-Date) - $t0).TotalMilliseconds

foreach ($p in $procs) {
    $p.Refresh()
    $cpuMs = ($p.TotalProcessorTime - $start[$p.Id].Cpu).TotalMilliseconds
    $switches = SwitchesSince $start[$p.Id].Switches (Switches $p.Id)
    $perf = Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter "IDProcess = $($p.Id)"
    $children = @(Get-CimInstance Win32_Process -Filter "ParentProcessId = $($p.Id)")
    $gpuEngines = @(Get-CimInstance Win32_PerfFormattedData_GPUPerformanceCounters_GPUEngine -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like "pid_$($p.Id)_*" })
    $gpuMemory = @(Get-CimInstance Win32_PerfFormattedData_GPUPerformanceCounters_GPUProcessMemory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like "pid_$($p.Id)_*" })
    $modules = $p.Modules | ForEach-Object { $_.ModuleName.ToLower() }
    $renderer = if ($modules -contains 'd3d10warp.dll') { 'WARP (software)' }
        elseif ($modules | Where-Object { $_ -match '^(nvwgf2umx|nvldumdx|amdxx64|aticfx64|igd10iumd64|igdumdim64)' }) { 'GPU driver' }
        else { 'none loaded' }

    Write-Output ("rustybar {0}, {1} s" -f $p.Id, [math]::Round($elapsed / 1000))
    Write-Output ("  memory     {0:N1} MB private working set, {1:N1} MB working set, {2:N1} MB committed" -f `
        ($perf.WorkingSetPrivate / 1MB), ($p.WorkingSet64 / 1MB), ($p.PrivateMemorySize64 / 1MB))
    Write-Output ("  cpu        {0:N3}% of the CPU ({1:N0} ms in {2:N0} s, {3} logical cores)" -f `
        ($cpuMs / ($elapsed * $cores) * 100), $cpuMs, ($elapsed / 1000), $cores)
    Write-Output ("  wake-ups   {0} context switches, {1:N2}/s" -f $switches, ($switches / ($elapsed / 1000)))
    Write-Output ("  gpu        {0:N1}% engines, {1:N1} MB dedicated, {2:N1} MB shared; renderer: {3}" -f `
        (($gpuEngines | Measure-Object -Property UtilizationPercentage -Sum).Sum),
        ((($gpuMemory | Measure-Object -Property DedicatedUsage -Sum).Sum) / 1MB),
        ((($gpuMemory | Measure-Object -Property SharedUsage -Sum).Sum) / 1MB), $renderer)
    Write-Output ("  threads    {0}, handles {1}" -f $p.Threads.Count, $p.HandleCount)
    Write-Output ("  children   {0}{1}" -f $children.Count, $(if ($children) { ': ' + (($children | ForEach-Object { $_.Name }) -join ', ') } else { '' }))
}
