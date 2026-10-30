# katala-tune Windows probe. Prints one JSON line; opt-in WSL observation may activate a distro in a stop race.
# PowerShell 5.1. Keep this file ASCII only (PS 5.1 reads BOM-less UTF-8 as the ANSI code page).
# Collects no secrets, environment variables, command lines or file contents
# (except the memory/processors keys of ~/.wslconfig, and the opt-in WSL observation below:
# /proc memory figures, process names and disk usage inside distros reported as running;
# command lines of interpreters are read only to build a short key and are never output).
# -NoNetwork skips the network and security section (nodes.json "network": false).
# -ObserveWsl enables WSL distro enumeration and inside observation (nodes.json "wsl": true).
param([switch]$NoNetwork, [switch]$ObserveWsl)
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
# Read both pipes concurrently. A root exit without EOF, or either stream over
# its limit, discards the capture. The outer native Job owns descendant cleanup.
function Read-KtProcessOutput($Process, $Clock, [int]$DeadlineMs, [string]$InputText = '', $Encoding = [Text.Encoding]::UTF8, [int]$StdoutLimit = 1048576, [int]$StderrLimit = 65536) {
  $ErrorActionPreference = 'Stop'
  $stdout = New-Object IO.MemoryStream
  $stderr = New-Object IO.MemoryStream
  try {
    $outBuffer = New-Object byte[] 4096; $errBuffer = New-Object byte[] 4096
    $outRead = $Process.StandardOutput.BaseStream.ReadAsync($outBuffer, 0, $outBuffer.Length)
    $errRead = $Process.StandardError.BaseStream.ReadAsync($errBuffer, 0, $errBuffer.Length)
    $write = $Process.StandardInput.WriteAsync($InputText)
    $left = [Math]::Max(0, $DeadlineMs - [int]$Clock.ElapsedMilliseconds)
    if ($left -le 0 -or -not $write.Wait([Math]::Min(2000, $left))) { throw 'worker stdin timeout' }
    $Process.StandardInput.Close()
    $outDone = $false; $errDone = $false
    while (-not ($outDone -and $errDone)) {
      $left = [Math]::Max(0, $DeadlineMs - [int]$Clock.ElapsedMilliseconds)
      if ($left -le 0) { throw 'worker output timeout or missing EOF' }
      $pending = New-Object Collections.Generic.List[Threading.Tasks.Task]
      if (-not $outDone) { $pending.Add($outRead) }
      if (-not $errDone) { $pending.Add($errRead) }
      if ([Threading.Tasks.Task]::WaitAny([Threading.Tasks.Task[]]$pending.ToArray(), $left) -lt 0) { throw 'worker output timeout or missing EOF' }
      if (-not $outDone -and $outRead.IsCompleted) {
        $count = $outRead.Result
        if ($count -eq 0) { $outDone = $true }
        elseif ($stdout.Length + $count -gt $StdoutLimit) { throw 'worker stdout exceeded limit' }
        else { $stdout.Write($outBuffer, 0, $count); $outRead = $Process.StandardOutput.BaseStream.ReadAsync($outBuffer, 0, $outBuffer.Length) }
      }
      if (-not $errDone -and $errRead.IsCompleted) {
        $count = $errRead.Result
        if ($count -eq 0) { $errDone = $true }
        elseif ($stderr.Length + $count -gt $StderrLimit) { throw 'worker stderr exceeded limit' }
        else { $stderr.Write($errBuffer, 0, $count); $errRead = $Process.StandardError.BaseStream.ReadAsync($errBuffer, 0, $errBuffer.Length) }
      }
    }
    $left = [Math]::Max(0, $DeadlineMs - [int]$Clock.ElapsedMilliseconds)
    if ($left -le 0 -or -not $Process.WaitForExit($left)) { throw 'worker root exit timeout' }
    return [pscustomobject]@{ out = $Encoding.GetString($stdout.ToArray()); err = $Encoding.GetString($stderr.ToArray()); code = $Process.ExitCode }
  } finally { $stdout.Dispose(); $stderr.Dispose() }
}

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

# CPU snapshots pair one cumulative read with its own monotonic timestamp.
# Process objects expose live properties; never defer the second read until after CIM.
function ReadCpuSample($p) {
  $ErrorActionPreference = "Stop"
  try {
    $start = $p.StartTime
    if ($null -eq $start) { return $null }
    # Identity must survive a failed CPU getter: actions use the start guard.
    $seconds = $null; $ticks = $null
    try {
      $seconds = $p.TotalProcessorTime.TotalSeconds
      $ticks = [Diagnostics.Stopwatch]::GetTimestamp()
    } catch { $seconds = $null; $ticks = $null }
    if ($null -eq $seconds) { $ticks = $null }
    return [pscustomobject]@{ pid = $p.Id; start = $start.ToUniversalTime().Ticks; cpu = $seconds; ticks = $ticks
      start_time = $start; life = ((Get-Date) - $start).TotalSeconds }
  } catch { return $null }
}
function ProcessCpuPercent($before, $after, $cores, $frequency) {
  if ($null -eq $before -or $null -eq $after -or $cores -le 0 -or $frequency -le 0) { return $null }
  foreach ($sample in @($before, $after)) {
    if ($null -eq $sample.pid -or $null -eq $sample.start -or $null -eq $sample.cpu -or $null -eq $sample.ticks) { return $null }
  }
  if ($before.pid -ne $after.pid -or $before.start -ne $after.start) { return $null }
  foreach ($value in @($before.cpu, $after.cpu, $before.ticks, $after.ticks, $cores, $frequency)) {
    if ([double]::IsNaN($value) -or [double]::IsInfinity($value)) { return $null }
  }
  $elapsed = ($after.ticks - $before.ticks) / $frequency
  $delta = $after.cpu - $before.cpu
  if ($delta -lt 0 -or $elapsed -le 0) { return $null }
  $percent = $delta / $elapsed / $cores * 100
  foreach ($value in @($delta, $elapsed, $percent)) {
    if ([double]::IsNaN($value) -or [double]::IsInfinity($value)) { return $null }
  }
  return $percent
}
function HostCpuSample($first, $second) {
  $values = @(@($first, $second) | ForEach-Object { $value = RNum $_ 6; if ($null -ne $value) { $value } })
  if ($values.Count -eq 0) { return [pscustomobject]@{ value = $null; availability = "unavailable"; samples = 0 } }
  return [pscustomobject]@{ value = ($values | Measure-Object -Average).Average
    availability = $(if ($values.Count -eq 2) { "available" } else { "partial" }); samples = $values.Count }
}
$s1 = @{}
foreach ($p in Get-Process) { $s1[$p.Id] = ReadCpuSample $p }
$perf1 = Get-CimInstance Win32_PerfFormattedData_PerfOS_Processor -Filter "Name='_Total'"
Start-Sleep -Milliseconds 1500
$procs = Get-Process
$s2 = @{}
foreach ($p in $procs) { $s2[$p.Id] = ReadCpuSample $p }
$perf2 = Get-CimInstance Win32_PerfFormattedData_PerfOS_Processor -Filter "Name='_Total'"
$hostCpu = HostCpuSample $perf1.PercentProcessorTime $perf2.PercentProcessorTime
$perfInfo = Get-CimInstance Win32_PerfFormattedData_Counters_ProcessorInformation -Filter "Name='_Total'"
$cpuEffectiveMhz = $null
if ($perfInfo -and $null -ne $perfInfo.ProcessorFrequency -and $null -ne $perfInfo.PercentProcessorPerformance) {
  $f = RNum $perfInfo.ProcessorFrequency
  $p = RNum $perfInfo.PercentProcessorPerformance
  if ($null -ne $f -and $null -ne $p) { $cpuEffectiveMhz = RNum ($f * $p / 100) }
}

$list = foreach ($p in $procs) {
  $sample = $s2[$p.Id]
  $c = ProcessCpuPercent $s1[$p.Id] $sample $ncpu ([Diagnostics.Stopwatch]::Frequency)
  # avg = lifetime average, percent of one core (null when the snapshot is unreadable)
  $avg = $null
  if ($null -ne $sample -and $null -ne $sample.cpu -and $sample.life -gt 60) { $avg = $sample.cpu / $sample.life * 100 }
  # start: UTC start time, used to confirm the same process right before a kill (PID reuse guard)
  $st = $null; if ($null -ne $sample) { $st = $sample.start_time.ToUniversalTime().ToString("yyyyMMddHHmmssfff") }
  [pscustomobject]@{ pid = $p.Id; name = $p.ProcessName; cpu = (R1 $c); avg_core = (R1 $avg); mem_mb = $(if ($null -ne $p.WorkingSet64) { RNum ($p.WorkingSet64 / 1MB) } else { $null }); start = $st; cpu_seconds = $(if ($null -ne $sample) { RNum $sample.cpu 3 } else { $null }); private_mb = $(if ($null -ne $p.PrivateMemorySize64) { RNum ($p.PrivateMemorySize64 / 1MB) } else { $null }); handle_count = $(if ($null -ne $p.HandleCount) { [int]$p.HandleCount } else { $null }); thread_count = $(if ($null -ne $p.Threads) { [int]$p.Threads.Count } else { $null }) }
}
$groups = $list | Group-Object name | ForEach-Object {
  $groupCpu = $null
  if (@($_.Group | Where-Object { $null -eq $_.cpu }).Count -eq 0) { $groupCpu = ($_.Group | Measure-Object cpu -Sum).Sum }
  [pscustomobject]@{ app = $_.Name; cpu = (R1 $groupCpu); mem_mb = [int](($_.Group | Measure-Object mem_mb -Sum).Sum); count = $_.Count }
}
$agents = @($list | Where-Object { $_.name -match '^(claude|codex|opencode|cursor-agent|agy|gemini)' }).Count

# Process multiplication and trees (issues 38 and 30). Read-only. Command lines are read only for interpreters (node, python, git ...)
# to build a short key (exe:script name); the command line itself is never output. Owner: session 0 (services and SSH logons) = system, otherwise user.
$mxInterp = '^(node|nodejs|bun|deno|python3?(\.\d+)?|py|uv|uvx|npx|bunx|npm|pnpm|ruby|java|javaw|git)$'
$mxGeneric = @('index', 'main', 'cli', 'server', 'bin', 'run', 'app', 'start', 'dist', 'build', 'lib', 'src', 'out', 'node_modules', '__main__')
$mxSkip = @{ npm = @('exec', 'run', 'x'); pnpm = @('exec', 'run', 'dlx'); uv = @('tool', 'run'); bun = @('run', 'x') }
# Options are interpreter-specific: Python -I takes no value, but Ruby -I takes a path.
$mxValueless = @{
  python = @('-q', '-u', '-B', '-E', '-s', '-S', '-I', '-O', '-OO', '-i')
  node = @('--no-warnings', '--no-deprecation', '--trace-warnings', '--enable-source-maps', '--inspect', '-i')
  npx = @('-y', '--yes', '-q', '--quiet', '--silent'); bunx = @('-y', '--yes')
  npm = @('-y', '--yes', '-q', '--quiet', '--silent', '-s'); pnpm = @('--silent', '-s')
  uv = @('-q', '--quiet'); uvx = @('-q', '--quiet')
}
$mxLaunchers = @('explorer', 'services', 'wininit', 'winlogon', 'svchost', 'sihost', 'csrss', 'smss', 'userinit', 'System', 'Idle', 'sshd')
# Label = the script file name; for a generic name (index.js ...) only the package directly under node_modules.
# Arbitrary parent directories (project or customer names) are never used.
function MxLabel([string]$path) {
  $parts = @($path -split '[\\/]+' | Where-Object { $_ })
  if ($parts.Count -eq 0) { return $null }
  $ok = '^[A-Za-z0-9][A-Za-z0-9_.@+-]{0,47}$'
  $n = $parts[$parts.Count - 1] -replace '\.([cm]?[jt]s|py|jar)$', ''
  if (-not ($mxGeneric -contains $n) -and ($n -cmatch $ok)) { return $n }
  for ($j = $parts.Count - 2; $j -ge 0; $j--) {
    if ($parts[$j] -ieq 'node_modules' -and $j + 1 -lt $parts.Count - 1) {
      $pkg = $parts[$j + 1]
      if ($pkg.StartsWith('@') -and $j + 2 -lt $parts.Count - 1) { $pkg = $parts[$j + 2] }
      if (-not ($mxGeneric -contains $pkg) -and ($pkg -cmatch $ok)) { return $pkg }
      return $null
    }
  }
  return $null
}
function MxKey([string]$exe, [string[]]$tokens) {
  if ($exe -notmatch $mxInterp) { return $exe.Substring(0, [Math]::Min(48, $exe.Length)) }
  $exe = $exe.ToLowerInvariant()
  $family = $exe; if ($exe -like 'python*') { $family = 'python' } elseif ($exe -eq 'nodejs') { $family = 'node' }
  $label = $null
  $i = 0
  $options = $true
  $launcher = $true
  while ($i -lt $tokens.Count) {
    $t = [string]$tokens[$i]
    if ($options -and $t -ceq '--') { $options = $false; $i++; continue }
    if ($options -and $t -ceq '-m' -and $family -eq 'python' -and $i + 1 -lt $tokens.Count) {
      if ($tokens[$i + 1] -cmatch '^[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)*$') { $label = MxLabel (($tokens[$i + 1] -split '\.')[0]) }
      break
    }
    if ($options -and $t -ceq '-c' -and $exe -like 'python*') { break }
    if ($options -and (($exe -eq 'git' -and ($t -ceq '-c' -or $t -ceq '-C')) -or ($family -eq 'node' -and ($t -ceq '-r' -or $t -ceq '--require')))) { $i += 2; continue }
    if ($options -and $t.StartsWith('-')) {
      if ($t.Contains('=') -or ($mxValueless[$family] -ccontains $t)) { $i++; continue }
      break
    }
    if ($options -and $launcher -and ($mxSkip[$exe] -ccontains $t)) {
      if ($exe -eq 'uv' -and $t -ceq 'tool') {
        if ($i + 1 -ge $tokens.Count -or $tokens[$i + 1] -cne 'run') { break }
        $i++
      }
      $i++; $launcher = $false; continue
    }
    if ($exe -eq 'git' -and $t -cne 'fsmonitor--daemon') { break }
    $label = MxLabel ($t -replace '(?<=.)@[^@/\\]*$', '')
    break
  }
  if ($label) { return ($exe + ':' + $label) }
  return $exe
}
$mxGroups = @(); $mxTrees = @(); $mxAvail = 'unavailable'; $mxReason = $null
try {
  $mx = @{}
  $mxCim = @(Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId, Name, WorkingSetSize, SessionId, CreationDate -OperationTimeoutSec 4 -ErrorAction Stop)
  if ($mxCim.Count -eq 0) { throw 'no-data' }
  # Command lines are queried only for interpreter executables (other processes' arguments are never read)
  $mxCmd = @{}
  $mxNames = @($mxCim | ForEach-Object { [string]$_.Name } | Where-Object { ($_ -replace '\.exe$', '') -match $mxInterp -and $_ -match '^[A-Za-z0-9._-]+$' } | Sort-Object -Unique)
  if ($mxNames.Count -gt 0) {
    $flt = ($mxNames | ForEach-Object { "Name = '$_'" }) -join ' OR '
    foreach ($c in @(Get-CimInstance Win32_Process -Filter $flt -Property ProcessId, CommandLine -OperationTimeoutSec 4 -ErrorAction Stop)) { if ($c.CommandLine) { $mxCmd[[int]$c.ProcessId] = [string]$c.CommandLine } }
  }
  foreach ($w in $mxCim) {
    $nm = ([string]$w.Name) -replace '\.exe$', ''
    $tok = @()
    if ($nm -match $mxInterp -and $mxCmd.ContainsKey([int]$w.ProcessId)) {
      $tok = @([regex]::Matches($mxCmd[[int]$w.ProcessId], '"([^"]*)"|(\S+)') | ForEach-Object { if ($_.Groups[1].Success) { $_.Groups[1].Value } else { $_.Groups[2].Value } } | Select-Object -Skip 1)
    }
    $mx[[int]$w.ProcessId] = @{ pid = [int]$w.ProcessId; ppid = [int]$w.ParentProcessId; name = $nm; kb = ([double]$w.WorkingSetSize / 1KB); sys = ([int]$w.SessionId -eq 0); born = $w.CreationDate; key = (MxKey $nm $tok) }
  }
  $kids = @{}
  foreach ($r in $mx.Values) {
    $p = $mx[$r.ppid]
    if ($p -and $r.born -and $p.born -and $p.born -gt $r.born) { $p = $null }
    $r.parent = $p
    $r.orphan = (($r.ppid -le 0) -or (-not $p))
    if ($p) { $kids[$p.pid] = 1 + [int]$kids[$p.pid] }
  }
  $gh = @{}
  $th = @{}
  foreach ($r in $mx.Values) {
    $g = $gh[$r.key]
    if (-not $g) { $g = @{ key = $r.key; count = 0; kb = 0.0; orphans = 0; orphan_kb = 0.0; sys = 0; usr = 0; parents = @{} }; $gh[$r.key] = $g }
    $g.count++; $g.kb += $r.kb
    if ($r.sys) { $g.sys++ } else { $g.usr++ }
    if ($r.orphan) { $g.orphans++; $g.orphan_kb += $r.kb }
    if ($r.parent) { $pn = ([string]$r.parent.name); $g.parents[$pn] = 1 + [int]$g.parents[$pn] }
    $x = $r; $seen = @{}
    while (-not $seen.ContainsKey($x.pid)) {
      $seen[$x.pid] = 1
      $p = $x.parent
      if ((-not $p) -or $p.pid -le 4 -or ($mxLaunchers -contains $p.name)) { break }
      $x = $p
    }
    $t = $th[$x.pid]
    if (-not $t) { $t = @{ root = $x.name; pid = $x.pid; sys = $x.sys; count = 0; kb = 0.0; children = [int]$kids[$x.pid] }; $th[$x.pid] = $t }
    $t.count++; $t.kb += $r.kb
  }
  $mxGroups = @($gh.Values | Where-Object { $_.count -ge 3 -or ($_.orphans -ge 2 -and $_.key -match 'mcp') } | ForEach-Object {
    $top = $null
    if ($_.parents.Count -gt 0) { $top = ($_.parents.GetEnumerator() | Sort-Object @{ e = 'Value'; Descending = $true }, @{ e = 'Name'; Descending = $false } | Select-Object -First 1).Key }
    $own = 'mixed'; if ($_.sys -eq 0) { $own = 'user' } elseif ($_.usr -eq 0) { $own = 'system' }
    [pscustomobject]@{ key = $_.key; count = $_.count; mem_mb = [int][Math]::Round($_.kb / 1024); orphans = $_.orphans; orphan_mb = [int][Math]::Round($_.orphan_kb / 1024); parent = $top; owner = $own }
  } | Sort-Object @{ e = 'mem_mb'; Descending = $true }, @{ e = 'count'; Descending = $true }, key | Select-Object -First 20)
  $mxTrees = @($th.Values | Where-Object { $_.count -ge 2 } | ForEach-Object {
    $own = 'user'; if ($_.sys) { $own = 'system' }
    [pscustomobject]@{ root = $_.root; pid = $_.pid; owner = $own; count = $_.count; children = $_.children; mem_mb = [int][Math]::Round($_.kb / 1024) }
  } | Sort-Object @{ e = 'mem_mb'; Descending = $true }, pid | Select-Object -First 10)
  $mxAvail = 'available'
} catch {
  $mxReason = NsErr $_
}
# Keep $mx for the following GPU compute PID and parent/child check, then clear it.
$mxCim = $null; $mxCmd = $null; $gh = $null; $th = $null; $kids = $null

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

# GPU: a single bounded query per analysis; no daemon, setters or process-name query.
# BEGIN GPU TELEMETRY
function InvokeGpuBounded($exe, $arguments) {
  $p = New-Object Diagnostics.Process
  $p.StartInfo = New-Object Diagnostics.ProcessStartInfo
  $p.StartInfo.FileName = $exe
  $p.StartInfo.Arguments = $arguments
  $p.StartInfo.UseShellExecute = $false
  $p.StartInfo.CreateNoWindow = $true
  $p.StartInfo.RedirectStandardOutput = $true
  $p.StartInfo.RedirectStandardError = $true
  $started = $false; $cleanupAttempted = $false; $result = $null
  $cleanup = {
    param($process, $clock)
    try {
      try { if (-not $process.HasExited) { $process.Kill() } } catch { }
      $left = [Math]::Max(0, 4000 - [int]$clock.ElapsedMilliseconds)
      $ack = $process.WaitForExit([Math]::Min(1000, $left))
      return ($ack -and $process.HasExited)
    } catch { return $false }
  }
  try {
    [void]$p.Start()
    $started = $true
    $clock = [Diagnostics.Stopwatch]::StartNew(); $stdout = New-Object IO.MemoryStream; $stderr = New-Object IO.MemoryStream
    $outBuffer = New-Object byte[] 4096; $errBuffer = New-Object byte[] 4096
    $outTask = $p.StandardOutput.BaseStream.ReadAsync($outBuffer, 0, $outBuffer.Length)
    $errTask = $p.StandardError.BaseStream.ReadAsync($errBuffer, 0, $errBuffer.Length)
    $outEof = $false; $errEof = $false; $reason = $null
    while (-not ($p.HasExited -and $outEof -and $errEof)) {
      $left = 4000 - [int]$clock.ElapsedMilliseconds
      if ($left -le 0) { $reason = "query-timeout"; break }
      $pending = @(); if (-not $outEof) { $pending += $outTask }; if (-not $errEof) { $pending += $errTask }
      # Pipe EOF alone is never success: a descendant may have closed both
      # streams while the root nvidia-smi process remains alive.
      if ($pending.Count -eq 0) {
        if (-not $p.WaitForExit($left)) { $reason = "query-timeout"; break }
        continue
      }
      if ([Threading.Tasks.Task]::WaitAny([Threading.Tasks.Task[]]$pending, $left) -lt 0) { $reason = "query-timeout"; break }
      if (-not $outEof -and $outTask.IsCompleted) {
        $n = $outTask.Result
        if ($n -eq 0) { $outEof = $true } elseif ($stdout.Length + $n -gt 65536) { $reason = "output-limit"; break } else { $stdout.Write($outBuffer, 0, $n); $outTask = $p.StandardOutput.BaseStream.ReadAsync($outBuffer, 0, $outBuffer.Length) }
      }
      if (-not $errEof -and $errTask.IsCompleted) {
        $n = $errTask.Result
        if ($n -eq 0) { $errEof = $true } elseif ($stderr.Length + $n -gt 65536) { $reason = "output-limit"; break } else { $stderr.Write($errBuffer, 0, $n); $errTask = $p.StandardError.BaseStream.ReadAsync($errBuffer, 0, $errBuffer.Length) }
      }
    }
    if ($reason) {
      $cleanupAttempted = $true
      if (-not (& $cleanup $p $clock)) { $reason = "cleanup-unverified" }
      $result = @{ text = ""; reason = $reason }
    } elseif ($p.ExitCode -ne 0) {
      $result = @{ text = ""; reason = "query-failed" }
    } else {
      $result = @{ text = [Text.Encoding]::UTF8.GetString($stdout.ToArray()); reason = $null }
    }
  } catch {
    $result = @{ text = ""; reason = "query-unavailable" }
  } finally {
    if ($started -and -not $cleanupAttempted -and -not $p.HasExited) {
      $cleanupAttempted = $true
      if (-not (& $cleanup $p $clock)) { $result = @{ text = ""; reason = "cleanup-unverified" } }
    }
    $p.Dispose()
  }
  return $result
}
function InvokeGpuQuery($exe, $fields) {
  $result = InvokeGpuBounded $exe "--query-gpu=$fields --format=csv,noheader,nounits"
  if ($result.reason) { return @{ rows = @(); reason = $result.reason } }
  return @{ rows = @($result.text -split "`r?`n" | Where-Object { $_.Trim() } | Select-Object -First 16); reason = $null }
}
function InvokeGpuComputeQuery($exe) {
  $result = InvokeGpuBounded $exe "--query-compute-apps=pid,used_gpu_memory --format=csv,noheader,nounits"
  if ($result.reason) { return @{ rows = @(); reason = $result.reason } }
  # nvidia-smi uses this text (rather than a blank CSV stream) for a successful empty result.
  $rows = @($result.text -split "`r?`n" | Where-Object { $_.Trim() -and $_.Trim() -ne "No running processes found" })
  if ($rows.Count -gt 16) { return @{ rows = @(); reason = "row-limit" } }
  return @{ rows = $rows; reason = $null }
}
function GpuMemory($g, $at) {
  foreach ($key in @("mem_used_mb", "mem_total_mb", "mem_free_mb", "util", "temp_c", "power_w")) { if ($null -ne $g[$key] -and $g[$key] -lt 0) { $g[$key] = $null } }
  if ($null -ne $g.mem_total_mb -and $g.mem_total_mb -le 0) { $g.mem_total_mb = $null }
  if ($null -ne $g.util -and $g.util -gt 100) { $g.util = $null }
  if ($null -ne $g.mem_total_mb -and $null -ne $g.mem_used_mb -and $g.mem_used_mb -gt $g.mem_total_mb) { $g.mem_used_mb = $null }
  if ($null -ne $g.mem_total_mb -and $null -ne $g.mem_free_mb -and $g.mem_free_mb -gt $g.mem_total_mb) { $g.mem_free_mb = $null }
  $g.sensor_reason = "null sensor values mean unavailable/unsupported driver fields, never zero"
  $g.memory_kind = "dedicated"; $g.memory_unit = "MiB"; $g.sampled_at = $at
  $valid = $null -ne $g.mem_used_mb -and $null -ne $g.mem_total_mb -and $g.mem_total_mb -gt 0 -and $g.mem_used_mb -ge 0 -and $g.mem_used_mb -le $g.mem_total_mb
  $g.mem_used_pct = $(if ($valid) { RNum ($g.mem_used_mb / $g.mem_total_mb * 100) } else { $null })
  $freeValid = $null -ne $g.mem_free_mb -and $g.mem_free_mb -ge 0 -and $g.mem_free_mb -le $g.mem_total_mb
  $g.memory_status = $(if ($valid -and $freeValid) { "available" } else { "partial" })
  $g.memory_reason = $(if ($valid -and $freeValid) { $null } else { "driver returned unavailable or invalid memory fields" })
  $g.process_memory_reason = "not collected; WDDM per-process memory may be N/A; compute-apps covers CUDA only"
  return [pscustomobject]$g
}
$gpus = @()
$gpuCompute = [ordered]@{ availability = "unavailable"; apps = @(); reason = "nvidia-smi-not-found" }
$gpuAt = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$smi = Get-Command nvidia-smi -ErrorAction SilentlyContinue
$gpuReason = "nvidia-smi-not-found; other GPU vendors unsupported (AdapterRAM is not a reliable VRAM capacity)"
if ($smi) {
  $query = InvokeGpuQuery $smi.Source "index,name,utilization.gpu,memory.used,memory.total,memory.free,temperature.gpu,power.draw,power.limit,power.min_limit,power.max_limit,clocks.current.graphics,clocks.current.sm,clocks.current.memory,pstate,driver_version,uuid,clocks_throttle_reasons.active"
  $gpuReason = $query.reason
  $memoryOnly = $false
  if ($query.reason -eq "query-failed") {
    $query = InvokeGpuQuery $smi.Source "index,name,memory.used,memory.total,memory.free"
    $gpuReason = $query.reason; $memoryOnly = $true
  }
  foreach ($line in $query.rows) {
    $f = $line -split ",\s*"
    if ($memoryOnly -and $f.Count -eq 5) { $f = @($f[0], $f[1], $null, $f[2], $f[3], $f[4], $null, $null, $null, $null, $null, $null, $null, $null, $null, $null, $null, $null) }
    if ($f.Count -ne 18) { $gpuReason = "invalid-query-row"; continue }
    $g = @{ uuid = $f[16]; throttle = $f[17]; adapter_index = (RNum $f[0] 0); name = $f[1]; util = (RNum $f[2]); mem_used_mb = (RNum $f[3] 0); mem_total_mb = (RNum $f[4] 0); mem_free_mb = (RNum $f[5] 0); temp_c = (RNum $f[6]); power_w = (RNum $f[7]); power_limit_w = (RNum $f[8]); power_min_w = (RNum $f[9]); power_max_w = (RNum $f[10]); clocks_graphics_mhz = (RNum $f[11]); clocks_sm_mhz = (RNum $f[12]); clocks_memory_mhz = (RNum $f[13]); pstate = $(if ($f[14] -match "^(N/A|Not Supported|\[N/A\])$") { $null } else { $f[14] }); driver = $f[15]; source = $(if ($memoryOnly) { "nvidia-smi memory-only fallback" } else { "nvidia-smi" }); available = $true }
    $gpus += GpuMemory $g $gpuAt
  }
  $compute = if ($query.reason) { @{ rows = @(); reason = $query.reason } } else { InvokeGpuComputeQuery $smi.Source }
  if ($compute.reason) {
    $gpuCompute.availability = "unavailable"; $gpuCompute.reason = $compute.reason
  } else {
    $apps = @(); $valid = $true
    foreach ($line in $compute.rows) {
      $f = $line -split ",\s*"
      if ($f.Count -ne 2 -or $f[0] -notmatch "^[0-9]+$") { $valid = $false; break }
      [int]$computePid = 0
      if (-not [int]::TryParse($f[0], [ref]$computePid) -or $computePid -le 4) { $valid = $false; break }
      # Keep N/A as null rather than zero; a PID still means active compute work.
      $used = $null
      if ($f[1] -notmatch "^(N/A|Not Supported|\[N/A\])$") { $used = RNum $f[1] 0; if ($null -eq $used -or $used -lt 0) { $valid = $false; break } }
      $name = $null; $mxRow = $mx[$computePid]
      if ($mxRow -and $mxRow.name -match "^[A-Za-z0-9_.+-]{1,80}$") { $name = $mxRow.name }
      $apps += [pscustomobject]@{ pid = $computePid; name = $name; used_memory_mb = $used }
    }
    if ($valid) { $gpuCompute.availability = "available"; $gpuCompute.apps = @($apps); $gpuCompute.reason = $null }
    else { $gpuCompute.availability = "unavailable"; $gpuCompute.reason = "invalid-compute-row" }
  }
} else {
  try {
    $adapters = @(Get-CimInstance Win32_VideoController -Property Name,PNPDeviceID -OperationTimeoutSec 4 -ErrorAction Stop)
    $nvidiaAdapters = @($adapters | Where-Object { $_.Name -match "(?i)NVIDIA" -or $_.PNPDeviceID -match "(?i)VEN_10DE" })
    if ($adapters.Count -gt 0 -and $adapters.Count -le 32 -and $nvidiaAdapters.Count -eq 0) { $gpuCompute.availability = "absent"; $gpuCompute.reason = "verified-no-nvidia-adapter" }
    else { $gpuCompute.reason = "nvidia-smi-not-found; adapter-identity-unavailable" }
  } catch { $gpuCompute.reason = "nvidia-smi-not-found; adapter-query-unavailable" }
}
if ($gpus.Count -eq 0) {
  $gpus = @([pscustomobject]@{ adapter_index = $null; name = "GPU"; source = "nvidia-smi"; available = $false; memory_kind = "unknown"; memory_unit = "MiB"; memory_status = $(if ($smi) { "unavailable" } else { "unsupported" }); memory_reason = $gpuReason; sampled_at = $gpuAt; mem_total_mb = $null; mem_used_mb = $null; mem_free_mb = $null; mem_used_pct = $null; util = $null; temp_c = $null; power_w = $null; power_limit_w = $null; process_memory_reason = "not collected" })
}
$gpuPids = @($gpuCompute.apps | ForEach-Object { [int]$_.pid })
function Test-KtGpuRelated([int]$TargetPid, [int[]]$GpuPids, $Map) {
  if ($GpuPids -contains $TargetPid) { return $true }
  $ancestors = @{}; $cur = $Map[$TargetPid]
  for ($i = 0; $i -lt 64 -and $cur -and $cur.pid -gt 0 -and -not $ancestors.ContainsKey($cur.pid); $i++) {
    $ancestors[$cur.pid] = $true
    $parent = $Map[$cur.ppid]
    if (-not $parent -or -not $cur.born -or -not $parent.born -or $parent.born -gt $cur.born) { break }
    $cur = $parent
  }
  foreach ($gpuPid in $GpuPids) {
    $cur = $Map[$gpuPid]
    for ($i = 0; $i -lt 64 -and $cur -and $cur.pid -gt 0; $i++) {
      if ($ancestors.ContainsKey($cur.pid)) { return $true }
      $parent = $Map[$cur.ppid]
      if (-not $parent -or -not $cur.born -or -not $parent.born -or $parent.born -gt $cur.born) { break }
      $cur = $parent
    }
  }
  return $false
}
foreach ($p in $list) {
  $row = $mx[[int]$p.pid]
  if ($row) { $p | Add-Member -NotePropertyName ppid -NotePropertyValue $row.ppid -Force }
  if ($gpuCompute.availability -eq "available" -and (Test-KtGpuRelated ([int]$p.pid) $gpuPids $mx)) { $p | Add-Member -NotePropertyName gpu_compute -NotePropertyValue $true -Force }
}
$mx = $null
# END GPU TELEMETRY

$cpuPowerW = $null; $cpuPowerSource = $null; $cpuPowerReason = "LibreHardwareMonitor CPU Package unavailable"
try {
  $lhm = @(Get-CimInstance -Namespace root/LibreHardwareMonitor -ClassName Sensor -ErrorAction Stop | Where-Object { $_.SensorType -eq "Power" -and $_.Name -match "CPU Package" })
  if ($lhm.Count -gt 0) {
    $cpuPowerW = RNum $lhm[0].Value
    if ($null -ne $cpuPowerW) { $cpuPowerSource = "LibreHardwareMonitor.Sensor"; $cpuPowerReason = $null }
  }
} catch {}
if ($null -eq $cpuPowerW) {
  try {
    $samples = @(Get-Counter "\Energy Meter(*)\Power" -ErrorAction Stop).CounterSamples | Where-Object { $_.InstanceName -match "(?i)^rapl_package\d+_pkg$" }
    $packages = @($samples | Group-Object InstanceName)
    $watts = @()
    foreach ($package in $packages) {
      $sample = @($package.Group | Where-Object { $_.Status -eq 0 -and $null -ne (RNum $_.CookedValue) } | Select-Object -First 1)
      if ($sample.Count -ne 1) { $watts = @(); break }
      $watts += (RNum $sample[0].CookedValue) / 1000
    }
    if ($packages.Count -gt 0 -and $watts.Count -eq $packages.Count) {
      $cpuPowerW = RNum (($watts | Measure-Object -Sum).Sum)
      if ($null -ne $cpuPowerW) { $cpuPowerSource = "Energy Meter RAPL package average (mW / 1000)"; $cpuPowerReason = $null }
    }
  } catch {}
}

# Disks
$disks = foreach ($d in Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=3') {
  if ($d.Size -gt 0) { [pscustomobject]@{ mount = $d.DeviceID; total_gb = [int]($d.Size / 1GB); free_gb = [int]($d.FreeSpace / 1GB); free_pct = (R1 ($d.FreeSpace / $d.Size * 100)) } }
}

# WSL limits (only two keys are read)
$wsl = [ordered]@{ config = $false; memory = $null; processors = $null; observation_enabled = [bool]$ObserveWsl }
$wcfg = Join-Path $env:USERPROFILE '.wslconfig'
if (Test-Path $wcfg) {
  $wsl.config = $true
  foreach ($line in Get-Content $wcfg) {
    if ($line -match '^\s*memory\s*=\s*(\S+)') { $wsl.memory = $Matches[1] }
    if ($line -match '^\s*processors\s*=\s*(\S+)') { $wsl.processors = $Matches[1] }
  }
}

# --- WSL observation (read-only) begin ---
# Lists the registered distros with wsl.exe -l (this never starts a distro). Only a distro that is already Running gets one
# fixed read-only command (/proc and ps/df). A stopped distro is reported as stopped_unobserved and is never touched:
# no -d, no --exec, nothing runs inside it. If the running list cannot be read, no distro is touched either.
# Invoke-KtWsl is the only place that starts wsl.exe, so a regression test can replace it with a fake.
$KtWslInnerScript = @'
LC_ALL=C
export LC_ALL
echo @@mem
grep -E "^(MemTotal|MemAvailable|SwapTotal|SwapFree):" /proc/meminfo 2>/dev/null
echo "@@status mem $?"
echo @@psi
cat /proc/pressure/memory 2>/dev/null
echo "@@status psi $?"
echo @@load
cat /proc/loadavg 2>/dev/null
echo "@@status load $?"
echo @@ps
{ ps -eo pid=,rss=,pcpu=,comm= --sort=-rss 2>/dev/null; echo "@@status ps $?"; } | awk "NR <= 8 || /^@@status ps /"
echo @@df
df -P -k / 2>/dev/null
echo "@@status df $?"
echo @@end
'@
function Test-KtWslInstalled { return (Test-Path (Join-Path $env:SystemRoot 'System32\wsl.exe')) }
function Invoke-KtWsl([string[]]$WslArgs, [string]$InputText = $null, [int]$TimeoutMs = 8000, [bool]$Utf16 = $true) {
  $res = [pscustomobject]@{ ok = $false; code = $null; out = ''; err = $null }
  $p = $null; $clock = [Diagnostics.Stopwatch]::StartNew()
  try {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = Join-Path $env:SystemRoot 'System32\wsl.exe'
    $psi.Arguments = ($WslArgs -join ' ')
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.RedirectStandardInput = $true
    $enc = if ($Utf16) { [System.Text.Encoding]::Unicode } else { New-Object System.Text.UTF8Encoding($false) }
    $p = New-Object System.Diagnostics.Process
    $p.StartInfo = $psi
    [void]$p.Start()
    $capture = Read-KtProcessOutput $p $clock $TimeoutMs $InputText $enc
    $res.code = $capture.code
    $res.out = $capture.out
    $res.ok = ($capture.code -eq 0)
    if (-not $res.ok) { $res.err = 'exit ' + $capture.code }
  } catch {
    $res.err = $(if ($_.Exception.Message -match 'timeout') { 'timeout' } elseif ($_.Exception.Message -match 'exceeded limit') { 'output-limit' } else { 'failed' })
  } finally {
    if ($p) {
      try {
        if (-not $p.HasExited) { $p.Kill(); if (-not $p.WaitForExit(1000)) { $res.ok = $false; $res.out = ''; $res.err = 'cleanup-unverified' } }
      } catch { $res.ok = $false; $res.out = ''; $res.err = 'cleanup-unverified' }
      $p.Dispose()
    }
  }
  return $res
}
# wsl.exe -l -q prints one name per line (UTF-16). Lines that are not plain distro names (localized messages) are counted, not used.
function ConvertTo-KtWslNames([string]$Text) {
  $names = @()
  $ignored = 0
  foreach ($line in ($Text -split "`n")) {
    $t = ($line -replace "[\x00\uFEFF\r]", '').Trim()
    if (-not $t) { continue }
    if ($t -match '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$') { $names += $t } else { $ignored++ }
  }
  return [pscustomobject]@{ names = @($names); ignored = $ignored }
}
function ConvertFrom-KtWslVerbose([string]$Text) {
  $map = @{}
  foreach ($line in ($Text -split "`n")) {
    $t = ($line -replace "[\x00\uFEFF\r]", '')
    if ($t -match '^\s*(\*)?\s*([A-Za-z0-9][A-Za-z0-9._-]{0,63})\s+\S+\s+([0-9])\s*$') {
      $map[$Matches[2]] = [pscustomobject]@{ default = [bool]$Matches[1]; version = [int]$Matches[3] }
    }
  }
  return $map
}
function Get-KtWslVhdxGb([string]$Name) {
  try {
    $key = Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Lxss' -ErrorAction Stop | Where-Object { (Get-ItemProperty $_.PSPath).DistributionName -eq $Name } | Select-Object -First 1
    if (-not $key) { return $null }
    $base = [string](Get-ItemProperty $key.PSPath).BasePath
    $base = $base -replace '^\\\\\?\\', ''
    $f = Join-Path $base 'ext4.vhdx'
    if (Test-Path $f) { return (R1 ((Get-Item $f).Length / 1GB)) }
  } catch { }
  return $null
}
function ConvertFrom-KtWslInside([string]$Text) {
  $sec = ''
  $kv = @{}
  $psi = @{}
  $load = $null
  $top = @()
  $disk = @()
  $status = @{}
  $invalid = @{}
  foreach ($line in ($Text -split "`n")) {
    $t = $line.TrimEnd("`r")
    if ($t -match '^@@status (mem|psi|load|ps|df) ([0-9]+)$') { $status[$Matches[1]] = $Matches[2]; continue }
    if ($t -match '^@@(\w+)$') { $sec = $Matches[1]; continue }
    if ($sec -eq 'mem' -and $t -match '^(\w+):\s+(\d+)\s*kB') { $kv[$Matches[1]] = [double]$Matches[2] }
    elseif ($sec -eq 'psi' -and $t -match '^(some|full)\s.*avg10=([0-9]+(?:\.[0-9]+)?)\s') { $psi[$Matches[1]] = [double]::Parse($Matches[2], [Globalization.CultureInfo]::InvariantCulture) }
    elseif ($sec -eq 'load' -and $t -match '^([0-9]+(?:\.[0-9]+)?)\s') { $load = [double]::Parse($Matches[1], [Globalization.CultureInfo]::InvariantCulture) }
    elseif ($sec -eq 'ps' -and $t -match '^\s*(\d+)\s+(\d+)\s+([0-9]+(?:\.[0-9]+)?)\s+(\S.*)$') {
      $top += [pscustomobject]@{ pid = [int]$Matches[1]; name = $Matches[4].Trim(); mem_mb = [int]([double]$Matches[2] / 1024); cpu = [double]::Parse($Matches[3], [Globalization.CultureInfo]::InvariantCulture) }
    }
    elseif ($sec -eq 'df' -and $t -match '^\S+\s+(\d+)\s+(\d+)\s+(\d+)\s+\d+%\s+(\S+)$') {
      $tot = [double]$Matches[1]
      if ($tot -gt 0 -and [double]$Matches[3] -le $tot) { $disk += [pscustomobject]@{ mount = $Matches[4]; total_gb = (R1 ($tot / 1048576)); free_gb = (R1 ([double]$Matches[3] / 1048576)); free_pct = (R1 ([double]$Matches[3] / $tot * 100)) } } else { $invalid['df'] = $true }
    }
    elseif ($t.Trim() -and $sec -in @('psi', 'load', 'ps', 'df') -and -not ($sec -eq 'df' -and $t -match '^Filesystem\s')) { $invalid[$sec] = $true }
  }
  if ($status['mem'] -ne '0' -or -not $kv.ContainsKey('MemTotal') -or -not $kv.ContainsKey('MemAvailable')) { return $null }
  if ($kv['MemTotal'] -le 0 -or $kv['MemAvailable'] -gt $kv['MemTotal']) { return $null }
  $errors = [ordered]@{}
  foreach ($pair in @(@('psi', 'psi'), @('load', 'load'), @('ps', 'top'), @('df', 'disk'))) {
    $section = $pair[0]; $key = $pair[1]
    if (-not $status.ContainsKey($section)) { $errors[$key] = 'missing-status' }
    elseif ($status[$section] -ne '0') { $errors[$key] = 'command-failed' }
    elseif ($invalid[$section]) { $errors[$key] = 'invalid-data' }
  }
  if (-not $errors.Contains('psi') -and (-not $psi.ContainsKey('some') -or -not $psi.ContainsKey('full') -or $psi['some'] -gt 100 -or $psi['full'] -gt 100)) { $errors['psi'] = 'invalid-data' }
  if (-not $errors.Contains('load') -and $null -eq $load) { $errors['load'] = 'invalid-data' }
  if (-not $errors.Contains('disk') -and $disk.Count -eq 0) { $errors['disk'] = 'invalid-data' }
  if ($errors.Contains('psi')) { $psi = @{} }
  if ($errors.Contains('load')) { $load = $null }
  if ($errors.Contains('top')) { $top = @() }
  if ($errors.Contains('disk')) { $disk = @() }
  $swapTotal = $null; $swapUsed = $null
  if ($kv.ContainsKey('SwapTotal') -and $kv.ContainsKey('SwapFree') -and $kv['SwapFree'] -le $kv['SwapTotal']) {
    $swapTotal = [int]($kv['SwapTotal'] / 1024); $swapUsed = [int](($kv['SwapTotal'] - $kv['SwapFree']) / 1024)
  } else { $errors['swap'] = 'invalid-data' }
  return [ordered]@{
    mem_total_mb = [int]($kv['MemTotal'] / 1024)
    mem_available_mb = [int]($kv['MemAvailable'] / 1024)
    swap_total_mb = $swapTotal
    swap_used_mb = $swapUsed
    psi_some_avg10 = $psi['some']
    psi_full_avg10 = $psi['full']
    load1 = $load
    top = @($top)
    disk = @($disk)
    errors = $errors
  }
}
function Get-KtWslObservation([bool]$Enabled = $false) {
  $r = [ordered]@{ enabled = $Enabled; installed = $false; listed = $false; list_error = $null; ignored_lines = 0; distros = @() }
  if (-not $Enabled) { return $r }
  if (-not (Test-KtWslInstalled)) { return $r }
  $r.installed = $true
  $all = Invoke-KtWsl @('-l', '-q')
  $parsed = ConvertTo-KtWslNames $all.out
  $r.ignored_lines = $parsed.ignored
  # Non-zero exit (including partial stdout) is not a reliable inventory.
  if (-not $all.ok) { $r.list_error = [string]$all.err; return $r }
  if ($parsed.names.Count -eq 0) { $r.listed = $true; return $r }
  $r.listed = $true
  $verbose = ConvertFrom-KtWslVerbose ((Invoke-KtWsl @('-l', '-v')).out)
  $run = Invoke-KtWsl @('-l', '--running', '-q')
  $runKnown = $run.ok
  $runNames = if ($runKnown) { @((ConvertTo-KtWslNames $run.out).names) } else { @() }
  $observed = 0
  foreach ($name in $parsed.names) {
    $v = $verbose[$name]
    $d = [ordered]@{ name = $name; default = $(if ($v) { $v.default } else { $false }); version = $(if ($v) { $v.version } else { $null }); state = 'unknown'; observed = $false; reason = $null; vhdx_gb = (Get-KtWslVhdxGb $name); inside = $null }
    if (-not $runKnown) { $d.reason = 'running-list-failed' }
    elseif ($runNames -notcontains $name) { $d.state = 'stopped_unobserved'; $d.reason = 'not-running' }
    elseif ($name -like 'docker-desktop*') { $d.state = 'running'; $d.reason = 'internal-distro-skipped' }
    elseif ($observed -ge 6) { $d.state = 'running'; $d.reason = 'observation-limit' }
    else {
      # Earlier detail commands can take seconds. Do not enter from a stale running list.
      $freshRun = Invoke-KtWsl @('-l', '--running', '-q')
      if (-not $freshRun.ok) { $d.reason = 'running-list-failed' }
      elseif (@((ConvertTo-KtWslNames $freshRun.out).names) -notcontains $name) { $d.state = 'stopped_unobserved'; $d.reason = 'not-running' }
      else {
        $d.state = 'running'
        $observed++
        $o = Invoke-KtWsl @('-d', $name, '--exec', '/bin/sh', '-s') ($KtWslInnerScript -replace "`r`n", "`n") 10000 $false
        $inside = if ($o.ok) { ConvertFrom-KtWslInside $o.out } else { $null }
        if ($inside) { $d.observed = $true; $d.inside = $inside } else { $d.reason = $(if ($o.err) { [string]$o.err } else { 'no-data' }) }
      }
    }
    $r.distros += [pscustomobject]$d
  }
  return $r
}
# --- WSL observation (read-only) end ---
$wslObs = Get-KtWslObservation -Enabled:$ObserveWsl
$wsl.installed = $wslObs.installed
$wsl.listed = $wslObs.listed
$wsl.list_error = $wslObs.list_error
$wsl.ignored_lines = $wslObs.ignored_lines
$wsl.distros = @($wslObs.distros)

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
    $capture = Read-KtProcessOutput $child $outerWatch 17000 $wire ([Text.Encoding]::UTF8)
    $timedOut = $false
    $outText = $capture.out
    $errText = $capture.err
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
  cpu_busy = (R1 $hostCpu.value)
  cpu_busy_availability = $hostCpu.availability
  cpu_busy_samples = $hostCpu.samples
  cpu_perf_pct = (R1 $perfInfo.PercentProcessorPerformance)
  cpu_clock = [ordered]@{ nominal_mhz = $cpu.MaxClockSpeed; effective_mhz = $cpuEffectiveMhz; source = "Win32_PerfFormattedData_Counters_ProcessorInformation.ProcessorFrequency * PercentProcessorPerformance / 100"; availability = $(if ($null -ne $cpuEffectiveMhz) { "available" } else { "unavailable" }) }
  memory = [ordered]@{ total_gb = (R1 ($totalMb / 1024)); free_gb = (R1 ($os.FreePhysicalMemory / 1MB)); available_pct = (R1 ($os.FreePhysicalMemory / $os.TotalVisibleMemorySize * 100)); commit_pct = $commitPct; pagefile_alloc_mb = $pfa['AllocatedBaseSize']; pagefile_used_mb = $pfa['CurrentUsage']; pagefile_peak_mb = $pfa['PeakUsage'] }
  processes = [ordered]@{
    count = @($list).Count
    threads = $(if (@($list | Where-Object { $null -eq $_.thread_count }).Count -eq 0) { ($list | Measure-Object thread_count -Sum).Sum } else { $null })
    handles = $(if (@($list | Where-Object { $null -eq $_.handle_count }).Count -eq 0) { ($list | Measure-Object handle_count -Sum).Sum } else { $null })
    source = "Get-Process (existing two-point CPU sample)"
    sampled_at = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    memory_unit = "MiB"; cpu_time_unit = "seconds (cumulative)"
    details_reason = "I/O rates unavailable in this provider; private bytes are committed private memory, not private working set"
    top_cpu = @($list | Sort-Object cpu -Descending | Select-Object -First 15)
    top_mem = @($list | Sort-Object mem_mb -Descending | Select-Object -First 15)
    apps = @($groups | Sort-Object mem_mb -Descending | Select-Object -First 25)
    apps_cpu = @($groups | Sort-Object cpu -Descending | Select-Object -First 10)
    agent_processes = $agents
    groups = @($mxGroups)
    trees = @($mxTrees)
    groups_availability = $mxAvail
    groups_reason = $mxReason
  }
  power = [ordered]@{ plan_guid = $guid; plan_name = $planName; plans = @($plans); package_w = $cpuPowerW; package_source = $cpuPowerSource; package_availability = $(if ($null -ne $cpuPowerW) { "available" } else { "unavailable" }); package_reason = $cpuPowerReason; soc_w = $null; soc_source = $null; low_power_mode = $null; on_battery = $null }
  defender = [ordered]@{ realtime = $mp.RealTimeProtectionEnabled; exclusions = $excl }
  gpus = @($gpus)
  gpu_compute = [pscustomobject]$gpuCompute
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
