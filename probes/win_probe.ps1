# katala-tune Windows probe. Read-only: prints one JSON line to stdout.
# PowerShell 5.1. Keep this file ASCII only (PS 5.1 reads BOM-less UTF-8 as the ANSI code page).
# Collects no secrets, environment variables, command lines or file contents
# (except the memory/processors keys of ~/.wslconfig).
$ErrorActionPreference = 'SilentlyContinue'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$t0 = Get-Date

function R1($x, $d = 1) { if ($null -eq $x) { return $null } return [math]::Round([double]$x, $d) }

$os = Get-CimInstance Win32_OperatingSystem
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
$ncpu = [int]$env:NUMBER_OF_PROCESSORS
$totalMb = [double]$os.TotalVisibleMemorySize / 1024

# CPU per process: two samples of total processor time.
$s1 = @{}
foreach ($p in Get-Process) { if ($p.CPU -ne $null) { $s1[$p.Id] = $p.CPU } }
$perf1 = Get-CimInstance Win32_PerfFormattedData_PerfOS_Processor -Filter "Name='_Total'"
$sw = [Diagnostics.Stopwatch]::StartNew()
Start-Sleep -Milliseconds 1500
$procs = Get-Process
$elapsed = $sw.Elapsed.TotalSeconds
$perf2 = Get-CimInstance Win32_PerfFormattedData_PerfOS_Processor -Filter "Name='_Total'"
$perfInfo = Get-CimInstance Win32_PerfFormattedData_Counters_ProcessorInformation -Filter "Name='_Total'"

$list = foreach ($p in $procs) {
  $c = 0
  if ($p.CPU -ne $null -and $s1.ContainsKey($p.Id)) { $c = ($p.CPU - $s1[$p.Id]) / $elapsed / $ncpu * 100 }
  # avg = lifetime average, percent of one core (null when StartTime is not readable)
  $avg = $null
  if ($p.CPU -ne $null -and $p.StartTime) { $life = ((Get-Date) - $p.StartTime).TotalSeconds; if ($life -gt 60) { $avg = $p.CPU / $life * 100 } }
  # start: UTC start time, used to confirm the same process right before a kill (PID reuse guard)
  $st = $null; if ($p.StartTime) { $st = $p.StartTime.ToUniversalTime().ToString('yyyyMMddHHmmssfff') }
  [pscustomobject]@{ pid = $p.Id; name = $p.ProcessName; cpu = (R1 $c); avg_core = (R1 $avg); mem_mb = [int]($p.WorkingSet64 / 1MB); start = $st }
}
$groups = $list | Group-Object name | ForEach-Object {
  [pscustomobject]@{ app = $_.Name; cpu = (R1 (($_.Group | Measure-Object cpu -Sum).Sum)); mem_mb = [int](($_.Group | Measure-Object mem_mb -Sum).Sum); count = $_.Count }
}
$agents = @($list | Where-Object { $_.name -match '^(claude|codex|opencode|cursor-agent|agy|gemini)' }).Count

# Memory
$commitPct = $null
if ($os.TotalVirtualMemorySize -gt 0) { $commitPct = R1 ((1 - $os.FreeVirtualMemory / $os.TotalVirtualMemorySize) * 100) }
$pf = Get-CimInstance Win32_PageFileUsage | Measure-Object -Property AllocatedBaseSize, CurrentUsage, PeakUsage -Sum
$pfa = @{}; foreach ($m in $pf) { $pfa[$m.Property] = $m.Sum }

# Power plan
$scheme = (powercfg /getactivescheme) -join ' '
$guid = $null; $planName = $null
if ($scheme -match '([0-9a-fA-F-]{36})\s*\((.+)\)') { $guid = $Matches[1]; $planName = $Matches[2] }

$plans = @()
foreach ($line in (powercfg /list)) { if ($line -match '([0-9a-fA-F-]{36})') { $plans += $Matches[1].ToLower() } }

# Defender
$mp = Get-MpComputerStatus
$excl = $null
$pref = Get-MpPreference
if ($pref -and $pref.ExclusionPath -and ($pref.ExclusionPath -notmatch 'N/A')) { $excl = @($pref.ExclusionPath).Count }

# GPU
$gpus = @()
$smi = Get-Command nvidia-smi -ErrorAction SilentlyContinue
if ($smi) {
  $q = & nvidia-smi --query-gpu=name,utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw,power.limit,clocks_throttle_reasons.active,driver_version --format=csv,noheader,nounits
  foreach ($line in $q) {
    $f = $line -split ',\s*'
    if ($f.Count -ge 9) {
      $gpus += [pscustomobject]@{ name = $f[0]; util = (R1 $f[1]); mem_used_mb = (R1 $f[2] 0); mem_total_mb = (R1 $f[3] 0); temp_c = (R1 $f[4] 0); power_w = (R1 $f[5]); power_limit_w = (R1 $f[6]); throttle = $f[7]; driver = $f[8] }
    }
  }
}

# Disks
$disks = foreach ($d in Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=3') {
  if ($d.Size -gt 0) { [pscustomobject]@{ mount = $d.DeviceID; total_gb = [int]($d.Size / 1GB); free_gb = [int]($d.FreeSpace / 1GB); free_pct = (R1 ($d.FreeSpace / $d.Size * 100)) } }
}

# WSL limits (only two keys are read)
$wsl = [ordered]@{ config = $false; memory = $null; processors = $null }
$wcfg = Join-Path $env:USERPROFILE '.wslconfig'
if (Test-Path $wcfg) {
  $wsl.config = $true
  foreach ($line in Get-Content $wcfg) {
    if ($line -match '^\s*memory\s*=\s*(\S+)') { $wsl.memory = $Matches[1] }
    if ($line -match '^\s*processors\s*=\s*(\S+)') { $wsl.processors = $Matches[1] }
  }
}

# Stability: unexpected shutdowns / bugchecks in the last 7 days
$since = (Get-Date).AddDays(-7)
$ev = Get-WinEvent -FilterHashtable @{ LogName = 'System'; Id = 41, 1001, 6008; StartTime = $since } -ErrorAction SilentlyContinue
$stab = [ordered]@{ kernel_power_41 = @($ev | Where-Object Id -eq 41).Count; bugcheck_1001 = @($ev | Where-Object { $_.Id -eq 1001 -and $_.ProviderName -match 'WER-SystemErrorReporting|BugCheck' }).Count; unexpected_6008 = @($ev | Where-Object Id -eq 6008).Count }

# Scheduled tasks outside \Microsoft\ (path, schedule, state, last result). Only the executable name of the action, no arguments.
# NOTE: [DateTime]'1970-01-01T00:00:00Z' is parsed as LOCAL time in PS 5.1 (off by the UTC offset). Use DateTimeOffset.
function EpochMs($d) { if ($d -and $d.Year -gt 2000) { return [DateTimeOffset]::new($d.ToUniversalTime()).ToUnixTimeMilliseconds() } return $null }
$jobs = foreach ($t in @(Get-ScheduledTask | Where-Object { $_.TaskPath -notlike '\Microsoft\*' } | Select-Object -First 150)) {
  $i = $t | Get-ScheduledTaskInfo -ErrorAction SilentlyContinue
  $trig = @($t.Triggers | ForEach-Object {
    $k = ($_.CimClass.CimClassName -replace '^MSFT_Task', '' -replace 'Trigger$', '')
    if ($_.Repetition -and $_.Repetition.Interval) { $k += ' every ' + $_.Repetition.Interval }
    $k }) -join ', '
  $exe = $null; $a = @($t.Actions)[0]; if ($a -and $a.Execute) { $exe = [IO.Path]::GetFileName($a.Execute.Trim('"')) }
  [pscustomobject]@{ kind = 'schtask'; id = $t.TaskPath + $t.TaskName; name = $t.TaskName; path = $t.TaskPath; state = ([string]$t.State).ToLower()
    last_result = $(if ($i) { [long]$i.LastTaskResult } else { $null }); last_run = (EpochMs $i.LastRunTime); next_run = (EpochMs $i.NextRunTime)
    schedule = $(if ($trig) { $trig } else { 'manual' }); program = $exe; author = $t.Author }
}

# Non-Windows services (path outside C:\Windows), for feature checks
$services = foreach ($s in @(Get-CimInstance Win32_Service | Where-Object { $_.PathName -and $_.PathName -notmatch '(?i)\\windows\\' } | Select-Object -First 120)) {
  $exe = $null; if ($s.PathName -match '^"?([^"]+?\.exe)') { $exe = [IO.Path]::GetFileName($Matches[1]) }
  [pscustomobject]@{ name = $s.Name; display = $s.DisplayName; state = ([string]$s.State).ToLower(); start = ([string]$s.StartMode).ToLower(); program = $exe }
}

$svc = foreach ($n in 'SysMain', 'WSearch', 'DiagTrack') { $s = Get-Service $n; if ($s) { [pscustomobject]@{ name = $n; status = [string]$s.Status; start = [string]$s.StartType } } }
$startup = @(Get-CimInstance Win32_StartupCommand | Select-Object -ExpandProperty Name)

$result = [ordered]@{
  probe = 'windows'; probe_version = 2
  host = [ordered]@{ hostname = $env:COMPUTERNAME; os = "$($os.Caption) $($os.BuildNumber)"; cpu = $cpu.Name.Trim(); cores = $ncpu; max_mhz = $cpu.MaxClockSpeed; uptime_h = (R1 (((Get-Date) - $os.LastBootUpTime).TotalHours)) }
  cpu_busy = (R1 ((([double]$perf1.PercentProcessorTime) + ([double]$perf2.PercentProcessorTime)) / 2))
  cpu_perf_pct = (R1 $perfInfo.PercentProcessorPerformance)
  memory = [ordered]@{ total_gb = (R1 ($totalMb / 1024)); free_gb = (R1 ($os.FreePhysicalMemory / 1MB)); available_pct = (R1 ($os.FreePhysicalMemory / $os.TotalVisibleMemorySize * 100)); commit_pct = $commitPct; pagefile_alloc_mb = $pfa['AllocatedBaseSize']; pagefile_used_mb = $pfa['CurrentUsage']; pagefile_peak_mb = $pfa['PeakUsage'] }
  processes = [ordered]@{
    count = @($list).Count
    top_cpu = @($list | Sort-Object cpu -Descending | Select-Object -First 15)
    top_mem = @($list | Sort-Object mem_mb -Descending | Select-Object -First 15)
    apps = @($groups | Sort-Object mem_mb -Descending | Select-Object -First 25)
    apps_cpu = @($groups | Sort-Object cpu -Descending | Select-Object -First 10)
    agent_processes = $agents
  }
  power = [ordered]@{ plan_guid = $guid; plan_name = $planName; plans = @($plans) }
  defender = [ordered]@{ realtime = $mp.RealTimeProtectionEnabled; exclusions = $excl }
  gpus = @($gpus)
  disk = @($disks)
  wsl = $wsl
  stability_7d = $stab
  services = @($svc)
  startup_items = $startup
  jobs = @($jobs)
  third_party_services = @($services)
}
$result.elapsed_s = R1 (((Get-Date) - $t0).TotalSeconds)
$result | ConvertTo-Json -Depth 6 -Compress
