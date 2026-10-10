# katala-tune Windows probe. Read-only: prints one JSON line to stdout.
# PowerShell 5.1. Keep this file ASCII only (PS 5.1 reads BOM-less UTF-8 as the ANSI code page).
# Collects no secrets, environment variables, command lines or file contents
# (except the memory/processors keys of ~/.wslconfig).
# -NoNetwork skips the network and security section (nodes.json "network": false).
param([switch]$NoNetwork)
$ErrorActionPreference = 'SilentlyContinue'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$t0 = Get-Date

function R1($x, $d = 1) { if ($null -eq $x) { return $null } return [math]::Round([double]$x, $d) }
function RNum($x, $d = 1) {
  if ($null -eq $x) { return $null }
  $s = ([string]$x).Trim()
  if (-not $s -or $s -match "^(N/A|Not Supported|\[N/A\])$") { return $null }
  $v = 0.0
  if (-not [double]::TryParse($s, [Globalization.NumberStyles]::Float, [Globalization.CultureInfo]::InvariantCulture, [ref]$v)) { return $null }
  if ([double]::IsNaN($v) -or [double]::IsInfinity($v)) { return $null }
  return [math]::Round($v, $d)
}
# Short reason for a failed read. "no-permission" when the account may not read it (no admin rights are requested).
function NsErr($e) {
  $x = $e.Exception
  if ($x -is [System.UnauthorizedAccessException] -or $x.HResult -eq -2147024891 -or [string]$x.NativeErrorCode -eq 'AccessDenied') { return 'no-permission' }
  $m = [string]$x.Message
  if ($m.Length -gt 160) { $m = $m.Substring(0, 160) }
  return $m
}

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
$cpuEffectiveMhz = $null
if ($perfInfo -and $null -ne $perfInfo.ProcessorFrequency -and $null -ne $perfInfo.PercentProcessorPerformance) {
  $f = RNum $perfInfo.ProcessorFrequency
  $p = RNum $perfInfo.PercentProcessorPerformance
  if ($null -ne $f -and $null -ne $p) { $cpuEffectiveMhz = RNum ($f * $p / 100) }
}

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
  $q = @(& nvidia-smi --query-gpu=uuid,name,utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw,power.limit,power.min_limit,power.max_limit,clocks.current.graphics,clocks.current.sm,clocks.current.memory,pstate,clocks_throttle_reasons.active,driver_version --format=csv,noheader,nounits 2>$null)
  $full = $q.Count -gt 0
  if (-not $full) { $q = @(& nvidia-smi --query-gpu=name,utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw,power.limit,clocks_throttle_reasons.active,driver_version --format=csv,noheader,nounits 2>$null) }
  foreach ($line in $q) {
    $f = $line -split ',\s*'
    if ($full -and $f.Count -ge 16) {
      $gpus += [pscustomobject]@{ uuid = $f[0]; name = $f[1]; util = (RNum $f[2]); mem_used_mb = (RNum $f[3] 0); mem_total_mb = (RNum $f[4] 0); temp_c = (RNum $f[5] 0); power_w = (RNum $f[6]); power_limit_w = (RNum $f[7]); power_min_w = (RNum $f[8]); power_max_w = (RNum $f[9]); clocks_graphics_mhz = (RNum $f[10] 0); clocks_sm_mhz = (RNum $f[11] 0); clocks_memory_mhz = (RNum $f[12] 0); pstate = $(if ($f[13] -match "^(N/A|Not Supported|\[N/A\])$") { $null } else { $f[13] }); throttle = $f[14]; driver = $f[15]; source = "nvidia-smi"; available = $true }
    } elseif (-not $full -and $f.Count -ge 9) {
      $gpus += [pscustomobject]@{ uuid = $null; name = $f[0]; util = (RNum $f[1]); mem_used_mb = (RNum $f[2] 0); mem_total_mb = (RNum $f[3] 0); temp_c = (RNum $f[4] 0); power_w = (RNum $f[5]); power_limit_w = (RNum $f[6]); power_min_w = $null; power_max_w = $null; clocks_graphics_mhz = $null; clocks_sm_mhz = $null; clocks_memory_mhz = $null; pstate = $null; throttle = $f[7]; driver = $f[8]; source = "nvidia-smi-legacy-query"; available = $true }
    }
  }
}

$cpuPowerW = $null; $cpuPowerReason = "LibreHardwareMonitor CPU Package unavailable"
try {
  $lhm = @(Get-CimInstance -Namespace root/LibreHardwareMonitor -ClassName Sensor -ErrorAction Stop | Where-Object { $_.SensorType -eq "Power" -and $_.Name -match "CPU Package" })
  if ($lhm.Count -gt 0) {
    $cpuPowerW = RNum $lhm[0].Value
    if ($null -ne $cpuPowerW) { $cpuPowerReason = $null }
  }
} catch {}

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
# A failed read is kept apart from "no tasks" (the persistence record must not count a failure as removals)
$allTasks = @(); $allTasksErr = $null
try { $allTasks = @(Get-ScheduledTask -ErrorAction Stop | Where-Object { $_.TaskPath -notlike '\Microsoft\*' }) } catch { $allTasksErr = NsErr $_ }
$jobs = foreach ($t in @($allTasks | Select-Object -First 150)) {
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
$allSvc = @(); $allSvcErr = $null
try { $allSvc = @(Get-CimInstance Win32_Service -OperationTimeoutSec 20 -ErrorAction Stop) } catch { $allSvcErr = NsErr $_ }
$services = foreach ($s in @($allSvc | Where-Object { $_.PathName -and $_.PathName -notmatch '(?i)\\windows\\' } | Select-Object -First 120)) {
  $exe = $null; if ($s.PathName -match '^"?([^"]+?\.exe)') { $exe = [IO.Path]::GetFileName($Matches[1]) }
  [pscustomobject]@{ name = $s.Name; display = $s.DisplayName; state = ([string]$s.State).ToLower(); start = ([string]$s.StartMode).ToLower(); program = $exe }
}

$svc = foreach ($n in 'SysMain', 'WSearch', 'DiagTrack') { $s = Get-Service $n; if ($s) { [pscustomobject]@{ name = $n; status = [string]$s.Status; start = [string]$s.StartType } } }
$startup = @(Get-CimInstance Win32_StartupCommand | Select-Object -ExpandProperty Name)

# Network and security (docs/observability.md, section 6). Read-only: connection metadata (process, address, port), the state
# of the OS protections and the names of auto-start entries. No packet contents, no command lines (only the executable file name).
# Every query has its own timeout and the section has a time budget; what cannot be read is reported in errors.
function ExeName($cmd) {
  $s = ([string]$cmd).Trim()
  if (-not $s) { return $null }
  if ($s -match '^"([^"]+)"') { $s = $Matches[1] }
  elseif ($s -match '(?i)^(.+?\.(exe|com|bat|cmd|vbs|js|ps1|dll))(\s|,|$)') { $s = $Matches[1] }
  else { $s = ($s -split '\s+')[0] }
  try { return [IO.Path]::GetFileName($s) } catch { return $null }
}
$netsec = $null
if (-not $NoNetwork) {
  # Run the complete section in a private child process. If a Windows provider does not return,
  # the parent can terminate only this child and keep the parts already flushed to stdout.
  $nsWorker = {
  param($payload)
  $ErrorActionPreference = 'SilentlyContinue'
  $mp = $payload.mp
  $allTasks = @($payload.allTasks)
  $allTasksErr = [string]$payload.allTasksErr; if (-not $allTasksErr) { $allTasksErr = $null }
  $allSvc = @($payload.allSvc)
  $allSvcErr = [string]$payload.allSvcErr; if (-not $allSvcErr) { $allSvcErr = $null }
  function NsErr($e) {
    $x = $e.Exception
    if ($x -is [System.UnauthorizedAccessException] -or $x.HResult -eq -2147024891 -or [string]$x.NativeErrorCode -eq 'AccessDenied') { return 'no-permission' }
    $m = [string]$x.Message
    if ($m.Length -gt 160) { $m = $m.Substring(0, 160) }
    return $m
  }
  function EpochMs($d) { if ($d -and $d.Year -gt 2000) { return [DateTimeOffset]::new($d.ToUniversalTime()).ToUnixTimeMilliseconds() } return $null }
  function ExeName($cmd) {
    $s = ([string]$cmd).Trim()
    if (-not $s) { return $null }
    if ($s -match '^"([^"]+)"') { $s = $Matches[1] }
    elseif ($s -match '(?i)^(.+?\.(exe|com|bat|cmd|vbs|js|ps1|dll))(\s|,|$)') { $s = $Matches[1] }
    else { $s = ($s -split '\s+')[0] }
    try { return [IO.Path]::GetFileName($s) } catch { return $null }
  }
  $nsWatch = [Diagnostics.Stopwatch]::StartNew()
  $nsBudgetMs = 15000
  $nsCpu0 = (Get-Process -Id $PID).TotalProcessorTime.TotalMilliseconds
  $ns = [ordered]@{ v = 1; listen = @(); outbound = @(); defense = [ordered]@{}; persist = @(); errors = [ordered]@{}; parts_ms = [ordered]@{} }
  $nsParts = [ordered]@{
    # Listening TCP/UDP endpoints and established TCP connections (process, remote address, remote port), one sample.
    # MSFT_NetTCPConnection.State: 2 = Listen, 5 = Established. tcp_states is kept to check that mapping.
    listen = {
      $map = @{}
      foreach ($p in @(Get-Process)) { $map[[int]$p.Id] = $p.ProcessName }
      $map[0] = 'Idle'; $map[4] = 'System'
      $tcp = @(Get-CimInstance -Namespace root/StandardCimv2 -ClassName MSFT_NetTCPConnection -OperationTimeoutSec 8 -ErrorAction Stop)
      $udp = @(Get-CimInstance -Namespace root/StandardCimv2 -ClassName MSFT_NetUDPEndpoint -OperationTimeoutSec 8 -ErrorAction Stop)
      $lst = New-Object System.Collections.ArrayList
      $conn = @{}
      $states = @{}
      $lports = @{}
      foreach ($c in $tcp) {
        $st = [string]$c.State
        $states[$st] = 1 + [int]$states[$st]
        if ($st -eq '2' -or $st -eq 'Listen') {
          $o = [int]$c.OwningProcess
          $pn = $map[$o]; if (-not $pn) { $pn = '?' }
          [void]$lst.Add([pscustomobject]@{ proto = 'tcp'; addr = [string]$c.LocalAddress; port = [int]$c.LocalPort; pid = $o; proc = $pn })
          $lports[[int]$c.LocalPort] = 1
        }
      }
      # Outbound only: a connection whose local port is a listening port was accepted from outside (its remote port changes every time)
      foreach ($c in $tcp) {
        $st = [string]$c.State
        if (($st -eq '5' -or $st -eq 'Established') -and [int]$c.RemotePort -gt 0 -and -not $lports.ContainsKey([int]$c.LocalPort)) {
          $ra = [string]$c.RemoteAddress
          $pn = $map[[int]$c.OwningProcess]; if (-not $pn) { $pn = '?' }
          if ($ra -notmatch '^(127\.|::1$|::ffff:127\.)') { $k = $pn + "`t" + $ra + "`t" + [int]$c.RemotePort; $conn[$k] = 1 + [int]$conn[$k] }
        }
      }
      foreach ($u in $udp) {
        $o = [int]$u.OwningProcess
        $pn = $map[$o]; if (-not $pn) { $pn = '?' }
        [void]$lst.Add([pscustomobject]@{ proto = 'udp'; addr = [string]$u.LocalAddress; port = [int]$u.LocalPort; pid = $o; proc = $pn })
      }
      $ns.listen = @($lst | Select-Object -First 600)
      $ns.outbound = @($conn.Keys | ForEach-Object { $f = $_ -split "`t"; [pscustomobject]@{ proc = $f[0]; addr = $f[1]; port = [int]$f[2]; n = $conn[$_] } } | Sort-Object n -Descending | Select-Object -First 400)
      $ns.tcp_states = $states
    }
    defender = {
      $m = $mp
      if (-not $m) { $m = Get-MpComputerStatus -ErrorAction Stop }
      $ns.defense.defender = [ordered]@{ realtime = $m.RealTimeProtectionEnabled; antivirus = $m.AntivirusEnabled; service = $m.AMServiceEnabled; mode = [string]$m.AMRunningMode
        sig_age_days = $m.AntivirusSignatureAge; sig_at = $m.AntivirusSignatureLastUpdatedMs; tamper = $m.IsTamperProtected }
    }
    # Antivirus products registered with Windows Security (another product may be the active one while Defender is passive)
    av = {
      $ns.defense.av = @(Get-CimInstance -Namespace root/SecurityCenter2 -ClassName AntiVirusProduct -OperationTimeoutSec 5 -ErrorAction Stop | ForEach-Object {
        $h = '{0:X6}' -f [int]$_.productState
        $h = $h.Substring($h.Length - 6)
        [pscustomobject]@{ name = [string]$_.displayName; enabled = ($h.Substring(2, 2) -eq '10' -or $h.Substring(2, 2) -eq '11'); uptodate = ($h.Substring(4, 2) -eq '00') }
      })
    }
    firewall = {
      $ns.defense.firewall = @(Get-NetFirewallProfile -PolicyStore ActiveStore -ErrorAction Stop | ForEach-Object {
        $en = [string]$_.Enabled
        [pscustomobject]@{ name = [string]$_.Name; enabled = $(if ($en -eq 'True') { $true } elseif ($en -eq 'False') { $false } else { $null }) }
      })
      $ns.defense.active = @(@(Get-NetConnectionProfile -ErrorAction SilentlyContinue) | ForEach-Object { $c = [string]$_.NetworkCategory; if ($c -eq 'DomainAuthenticated') { 'Domain' } else { $c } } | Select-Object -Unique)
    }
    detections = {
      $since = (Get-Date).AddDays(-30)
      $d = @(Get-CimInstance -Namespace root/Microsoft/Windows/Defender -ClassName MSFT_MpThreatDetection -OperationTimeoutSec 5 -ErrorAction Stop)
      $recent = @($d | Where-Object { $_.InitialDetectionTime -and $_.InitialDetectionTime -ge $since })
      $ns.defense.detections_30d = $recent.Count
      # ThreatStatusID 2 cleaned, 3 quarantined, 4 removed, 6 blocked = handled. Anything else (or no status) counts as open
      $ns.defense.detections_open_30d = @($recent | Where-Object { @(2, 3, 4, 6) -notcontains [int]$_.ThreatStatusID -or $null -eq $_.ThreatStatusID }).Count
    }
    # Whether the logon records (Security log 4624/4625, read by win_logs.ps1) are readable without admin rights
    security_log = {
      try { [void](Get-WinEvent -LogName Security -MaxEvents 1 -ErrorAction Stop); $ns.defense.security_log = 'ok' }
      catch {
        if ((NsErr $_) -eq 'no-permission') { $ns.defense.security_log = 'no-permission' }
        elseif ([string]$_.FullyQualifiedErrorId -like 'NoMatchingEventsFound*') { $ns.defense.security_log = 'ok' }
        else { throw }
      }
    }
    # Auto-start entries: tasks outside \Microsoft\, services (per-user suffix folded), Run keys, Startup folders. Names only.
    tasks = {
      if ($allTasksErr) { throw $allTasksErr }
      $ns.persist += @($allTasks | ForEach-Object { [pscustomobject]@{ kind = 'schtask'; key = [string]$_.TaskPath + [string]$_.TaskName; program = (ExeName $_.Execute) } })
    }
    services = {
      if ($allSvcErr) { throw $allSvcErr }
      $ns.persist += @($allSvc | ForEach-Object { [pscustomobject]@{ kind = 'service'; key = ([string]$_.Name -replace '_[0-9a-fA-F]{4,8}$', '_*'); program = (ExeName $_.PathName) } })
    }
    run = {
      $keys = [ordered]@{ 'HKCU' = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'; 'HKLM' = 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Run'; 'HKLM32' = 'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run' }
      foreach ($h in @($keys.Keys)) {
        if (-not (Test-Path -LiteralPath $keys[$h])) { continue }
        $k = Get-Item -LiteralPath $keys[$h] -ErrorAction Stop
        foreach ($v in @($k.GetValueNames())) { if ($v) { $ns.persist += [pscustomobject]@{ kind = 'run'; key = $h + '\' + $v; program = (ExeName $k.GetValue($v)) } } }
      }
    }
    startup = {
      $dirs = [ordered]@{ 'user' = [Environment]::GetFolderPath('Startup'); 'common' = [Environment]::GetFolderPath('CommonStartup') }
      foreach ($s in @($dirs.Keys)) {
        $p = $dirs[$s]
        if (-not $p -or -not (Test-Path -LiteralPath $p)) { continue }
        foreach ($f in @(Get-ChildItem -LiteralPath $p -File -Force -ErrorAction Stop)) { if ($f.Name -ne 'desktop.ini') { $ns.persist += [pscustomobject]@{ kind = 'startup'; key = $s + '\' + $f.Name; program = $null } } }
      }
    }
  }
  foreach ($name in @($nsParts.Keys)) {
    if ($nsWatch.ElapsedMilliseconds -ge $nsBudgetMs) { $ns.errors[$name] = 'skipped: time budget' }
    else {
    $s0 = $nsWatch.ElapsedMilliseconds
    try { & $nsParts[$name] } catch { $ns.errors[$name] = (NsErr $_) }
    $ns.parts_ms[$name] = [int]($nsWatch.ElapsedMilliseconds - $s0)
    }
    $ns.elapsed_ms = [int]$nsWatch.ElapsedMilliseconds
    $ns.cpu_ms = [int]((Get-Process -Id $PID).TotalProcessorTime.TotalMilliseconds - $nsCpu0)
    $json = $ns | ConvertTo-Json -Compress -Depth 8
    [Console]::Out.WriteLine([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($json)))
    [Console]::Out.Flush()
  }
  }

  $child = $null
  $outerWatch = [Diagnostics.Stopwatch]::StartNew()
  try {
    # Send only fields the worker uses. In particular, task arguments and service command lines are not copied to its output.
    $nsMp = $null
    if ($mp) {
      $nsMp = [pscustomobject]@{ RealTimeProtectionEnabled = $mp.RealTimeProtectionEnabled; AntivirusEnabled = $mp.AntivirusEnabled; AMServiceEnabled = $mp.AMServiceEnabled
        AMRunningMode = [string]$mp.AMRunningMode; AntivirusSignatureAge = $mp.AntivirusSignatureAge; AntivirusSignatureLastUpdatedMs = (EpochMs $mp.AntivirusSignatureLastUpdated); IsTamperProtected = $mp.IsTamperProtected }
    }
    $nsTasks = @($allTasks | ForEach-Object { $a = @($_.Actions)[0]; [pscustomobject]@{ TaskPath = [string]$_.TaskPath; TaskName = [string]$_.TaskName; Execute = $(if ($a) { [string]$a.Execute } else { $null }) } })
    $nsServices = @($allSvc | ForEach-Object { [pscustomobject]@{ Name = [string]$_.Name; PathName = [string]$_.PathName } })
    $payload = [ordered]@{ mp = $nsMp; allTasks = $nsTasks; allTasksErr = $allTasksErr; allSvc = $nsServices; allSvcErr = $allSvcErr } | ConvertTo-Json -Compress -Depth 8
    $workerText = $nsWorker.ToString()
    $wireText = [string]$workerText.Length + "`n" + $workerText + $payload
    $wire = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($wireText))
    # The fixed bootstrap reads the worker and payload from private stdin; neither is placed on the command line or disk.
    $bootstrap = '$ErrorActionPreference=''Stop'';$b=[Console]::In.ReadToEnd();$r=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($b));$i=$r.IndexOf("`n");$n=[int]$r.Substring(0,$i);$s=$r.Substring($i+1,$n);$p=$r.Substring($i+1+$n)|ConvertFrom-Json;& ([ScriptBlock]::Create($s)) $p'
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($bootstrap))
    $psi = New-Object Diagnostics.ProcessStartInfo
    $psi.FileName = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
    $psi.Arguments = '-NoLogo -NoProfile -NonInteractive -EncodedCommand ' + $encoded
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $child = New-Object Diagnostics.Process
    $child.StartInfo = $psi
    if (-not $child.Start()) { throw 'netsec worker did not start' }
    $outTask = $child.StandardOutput.ReadToEndAsync()
    $errTask = $child.StandardError.ReadToEndAsync()
    $writeTask = $child.StandardInput.WriteAsync($wire)
    if (-not $writeTask.Wait(2000)) { throw 'netsec worker stdin timeout' }
    $child.StandardInput.Close()
    $runLeft = [Math]::Max(0, 17000 - [int]$outerWatch.ElapsedMilliseconds)
    $timedOut = -not $child.WaitForExit($runLeft)
    if ($timedOut) {
      $child.Kill()
      $killLeft = [Math]::Max(0, 19500 - [int]$outerWatch.ElapsedMilliseconds)
      if (-not $child.WaitForExit($killLeft)) { throw 'netsec worker did not exit after kill' }
    }
    $outText = $outTask.Result
    $errText = $errTask.Result
    $workerFailed = $child.ExitCode -ne 0
    foreach ($line in @($outText -split "`r?`n")) {
      if (-not $line.Trim()) { continue }
      try {
        $json = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($line.Trim()))
        $candidate = $json | ConvertFrom-Json -ErrorAction Stop
        if ($candidate.v -eq 1) { $netsec = $candidate }
      } catch {}
    }
    if (-not $netsec) {
      $reason = $(if ($timedOut) { 'timeout: 20s' } elseif ($errText.Trim()) { $errText.Trim() } else { 'worker returned no data' })
      $netsec = [ordered]@{ v = 1; listen = @(); outbound = @(); defense = [ordered]@{}; persist = @(); errors = [ordered]@{}; parts_ms = [ordered]@{}; elapsed_ms = 0; cpu_ms = $null }
      foreach ($name in 'listen', 'defender', 'av', 'firewall', 'detections', 'security_log', 'tasks', 'services', 'run', 'startup') { $netsec.errors[$name] = $reason }
    } elseif ($timedOut -or $workerFailed) {
      $missingReason = $(if ($timedOut) { 'timeout: 20s' } elseif ($errText.Trim()) { $errText.Trim() } else { 'worker exited before all parts' })
      if ($missingReason.Length -gt 160) { $missingReason = $missingReason.Substring(0, 160) }
      foreach ($name in 'listen', 'defender', 'av', 'firewall', 'detections', 'security_log', 'tasks', 'services', 'run', 'startup') {
        $partDone = $netsec.parts_ms.PSObject.Properties.Name -contains $name
        $hasError = $netsec.errors.PSObject.Properties.Name -contains $name
        if (-not $partDone -and -not $hasError) { $netsec.errors | Add-Member -NotePropertyName $name -NotePropertyValue $missingReason }
      }
    }
  } catch {
    $reason = NsErr $_
    $netsec = [ordered]@{ v = 1; listen = @(); outbound = @(); defense = [ordered]@{}; persist = @(); errors = [ordered]@{ listen = $reason; defender = $reason; av = $reason; firewall = $reason; detections = $reason; security_log = $reason; tasks = $reason; services = $reason; run = $reason; startup = $reason }; parts_ms = [ordered]@{}; elapsed_ms = 0; cpu_ms = $null }
  } finally {
    if ($child) {
      if (-not $child.HasExited) {
        try {
          $child.Kill()
          $killLeft = [Math]::Max(0, 19500 - [int]$outerWatch.ElapsedMilliseconds)
          [void]$child.WaitForExit($killLeft)
        } catch {}
      }
      $child.Dispose()
    }
  }
  $netsec.elapsed_ms = [int]$outerWatch.ElapsedMilliseconds
}

$result = [ordered]@{
  probe = 'windows'; probe_version = 2
  host = [ordered]@{ hostname = $env:COMPUTERNAME; os = "$($os.Caption) $($os.BuildNumber)"; cpu = $cpu.Name.Trim(); cores = $ncpu; max_mhz = $cpu.MaxClockSpeed; uptime_h = (R1 (((Get-Date) - $os.LastBootUpTime).TotalHours)) }
  cpu_busy = (R1 ((([double]$perf1.PercentProcessorTime) + ([double]$perf2.PercentProcessorTime)) / 2))
  cpu_perf_pct = (R1 $perfInfo.PercentProcessorPerformance)
  cpu_clock = [ordered]@{ nominal_mhz = $cpu.MaxClockSpeed; effective_mhz = $cpuEffectiveMhz; source = "Win32_PerfFormattedData_Counters_ProcessorInformation.ProcessorFrequency * PercentProcessorPerformance / 100"; availability = $(if ($null -ne $cpuEffectiveMhz) { "available" } else { "unavailable" }) }
  memory = [ordered]@{ total_gb = (R1 ($totalMb / 1024)); free_gb = (R1 ($os.FreePhysicalMemory / 1MB)); available_pct = (R1 ($os.FreePhysicalMemory / $os.TotalVisibleMemorySize * 100)); commit_pct = $commitPct; pagefile_alloc_mb = $pfa['AllocatedBaseSize']; pagefile_used_mb = $pfa['CurrentUsage']; pagefile_peak_mb = $pfa['PeakUsage'] }
  processes = [ordered]@{
    count = @($list).Count
    top_cpu = @($list | Sort-Object cpu -Descending | Select-Object -First 15)
    top_mem = @($list | Sort-Object mem_mb -Descending | Select-Object -First 15)
    apps = @($groups | Sort-Object mem_mb -Descending | Select-Object -First 25)
    apps_cpu = @($groups | Sort-Object cpu -Descending | Select-Object -First 10)
    agent_processes = $agents
  }
  power = [ordered]@{ plan_guid = $guid; plan_name = $planName; plans = @($plans); package_w = $cpuPowerW; package_source = $(if ($null -ne $cpuPowerW) { "LibreHardwareMonitor.Sensor" } else { $null }); package_availability = $(if ($null -ne $cpuPowerW) { "available" } else { "unavailable" }); package_reason = $cpuPowerReason; soc_w = $null; soc_source = $null; low_power_mode = $null; on_battery = $null }
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
if ($netsec) { $result.netsec = $netsec }
$result.elapsed_s = R1 (((Get-Date) - $t0).TotalSeconds)
$result | ConvertTo-Json -Depth 8 -Compress
