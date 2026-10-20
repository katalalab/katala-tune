# katala-tune live sampler (Windows). Read-only: prints one JSON line per interval to stdout until the reader goes away.
# PowerShell 5.1. Keep this file ASCII only (PS 5.1 reads BOM-less UTF-8 as the ANSI code page).
# Reads performance counters in-process (.NET PerformanceCounterCategory with English names, which also works on
# localized Windows): no WMI polling. GPU uses one read-only nvidia-smi query at most every five seconds,
# bounded to four seconds and tied to this sampler. Collects numbers and, for top processes, PID and executable name only
# (no command lines, environment or file contents).
# Stops with -WatchStdin when stdin closes (local run: the app keeps our stdin open) or when the ssh session we run
# under ends (remote run), when stdout breaks, after -MaxSeconds ({"type":"end","reason":"max_age"}),
# or after -Count samples ({"type":"end","reason":"count"}).
# Output: the first line is {"type":"hello",...}, then {"type":"s",...} (see probes/live_mac.py for the fields).
#   cpu / cores: percent of the whole machine. procs.cpu: percent of one core. mem.commit_pct: commit charge / limit.
# The loop avoids PowerShell function calls and pipelines (they cost milliseconds each in PS 5.1) to stay light.
param([double]$Interval = 1, [double]$ProcEvery = 5, [double]$MaxSeconds = 900, [int]$Count = 0, [switch]$WatchStdin)
$ErrorActionPreference = 'SilentlyContinue'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$inv = [Globalization.CultureInfo]::InvariantCulture
$out = [Console]::Out
$errs = New-Object System.Collections.Generic.List[string]
if ($Interval -lt 0.2) { $Interval = 0.2 }
if ($Interval -gt 10) { $Interval = 10 }
if ($ProcEvery -lt 1) { $ProcEvery = 1 }
$TOP = 8
$CS = [System.Diagnostics.CounterSample]

# JSON number (culture independent), or null for anything that is not a finite number
function N($x, [int]$d = 1) {
  if ($null -eq $x) { return 'null' }
  $v = 0.0
  if ($x -is [string]) { if (-not [double]::TryParse($x, [Globalization.NumberStyles]::Float, $inv, [ref]$v)) { return 'null' } }
  else { try { $v = [double]$x } catch { return 'null' } }
  if ([double]::IsNaN($v) -or [double]::IsInfinity($v)) { return 'null' }
  return ([math]::Round($v, $d)).ToString('R', $inv)
}
# JSON string (process and GPU names)
function J($s) { if ($null -eq $s) { return 'null' } return '"' + (([string]$s) -replace '[\\"]', '\$&' -replace '[\x00-\x1f]', ' ') + '"' }
function Emit([string]$s) { try { $out.WriteLine($s); $out.Flush(); return $true } catch { return $false } }

function OpenCat([string]$name) {
  try { $c = New-Object System.Diagnostics.PerformanceCounterCategory($name); [void]$c.ReadCategory(); return $c }
  catch { $errs.Add(($name + ': ' + $_.Exception.Message)); return $null }
}
function ReadCat($cat) { if ($null -eq $cat) { return $null } try { return $cat.ReadCategory() } catch { return $null } }
# instance name -> CounterSample
function Samples($data, [string]$counter) {
  $h = @{}
  if ($null -eq $data) { return $h }
  $col = $data[$counter]
  if ($null -eq $col) { return $h }
  foreach ($i in $col.Values) { $h[$i.InstanceName] = $i.Sample }
  return $h
}

# Network: physical adapters only (virtual ones carry the same traffic twice).
$NET_SKIP = '(?i)loopback|isatap|teredo|6to4|virtual|hyper-v|vethernet|tailscale|wireguard|wintun|vpn|tap-|npcap|wan miniport|bluetooth|vmware|virtualbox|kernel debug'

$cpuCat = OpenCat 'Processor'
$memCat = OpenCat 'Memory'
$diskCat = OpenCat 'PhysicalDisk'
$netCat = OpenCat 'Network Interface'
$procCat = OpenCat 'Process'

$totalBytes = $null
try { $totalBytes = [double](Get-CimInstance Win32_OperatingSystem).TotalVisibleMemorySize * 1024 } catch { }
if (-not $totalBytes) { $errs.Add('memory: total unknown') }

# CPU package power: Windows Energy Meter exposes RAPL package instances as interval-average mW.
# Open the counters once; the first NextValue() is only a warmup and is never emitted as a measurement.
$energyCounters = New-Object System.Collections.Generic.List[object]
try {
  $energyCat = New-Object System.Diagnostics.PerformanceCounterCategory('Energy Meter')
  foreach ($instance in $energyCat.GetInstanceNames()) {
    if ($instance -match '(?i)^rapl_package\d+_pkg$') {
      $counter = New-Object System.Diagnostics.PerformanceCounter('Energy Meter', 'Power', $instance, $true)
      [void]$counter.NextValue()
      $energyCounters.Add($counter)
    }
  }
} catch { $energyCounters.Clear() }
$energyWarm = $energyCounters.Count -gt 0

# ---- GPU: one bounded read-only query at most every 5 seconds, no resident dmon ----
$smi = $null
$sc = Get-Command nvidia-smi -ErrorAction SilentlyContinue
if ($sc) { $smi = $sc.Source }
$gpuSampleClock = [Diagnostics.Stopwatch]::StartNew()
$gpuEveryMs = [math]::Max(5000, $ProcEvery * 1000)
$gpuMaxAgeMs = $gpuEveryMs + 4500
$gpuQuery = $null; $gpuTask = $null; $gpuErrorTask = $null
$gpuFallback = $false; $gpuStarted = -$gpuEveryMs; $gpuReceived = $null; $gpuRows = "[]"; $gpuReason = "nvidia-smi-not-found; other vendors unsupported"
function ParseGpuRows($text, $at) {
  $rows = New-Object System.Collections.Generic.List[string]
  foreach ($line in @($text -split "`r?`n" | Select-Object -First 16)) {
    $f = $line -split ",\s*"
    $memoryOnly = $f.Count -eq 5
    if ($memoryOnly) { $f = @($f[0], $f[1], $null, $f[2], $f[3], $f[4], $null, $null, $null, $null, $null, $null, $null, $null, $null, $null, $null, $null) }
    if ($f.Count -ne 18) { continue }
    $used = N $f[3] 0; $total = N $f[4] 0; $free = N $f[5] 0
    $pct = "null"; $status = "partial"; $reason = "driver returned unavailable or invalid memory fields"
    if ($used -ne "null" -and $total -ne "null" -and [double]$total -gt 0 -and [double]$used -ge 0 -and [double]$used -le [double]$total) { $pct = N ([double]$used / [double]$total * 100) }
    if ($pct -ne "null" -and $free -ne "null" -and [double]$free -ge 0 -and [double]$free -le [double]$total) { $status = "available"; $reason = $null }
    $rows.Add('{"adapter_index":' + (N $f[0] 0) + ',"name":' + (J $f[1]) + ',"util":' + (N $f[2]) + ',"mem_used_mb":' + $used + ',"mem_total_mb":' + $total + ',"mem_free_mb":' + $free + ',"mem_used_pct":' + $pct + ',"temp_c":' + (N $f[6]) + ',"power_w":' + (N $f[7]) + ',"clocks_graphics_mhz":' + (N $f[8]) + ',"clocks_memory_mhz":' + (N $f[9]) + ',"uuid":' + (J $f[10]) + ',"pstate":' + (J $f[11]) + ',"power_limit_w":' + (N $f[12]) + ',"power_min_w":' + (N $f[13]) + ',"power_max_w":' + (N $f[14]) + ',"clocks_sm_mhz":' + (N $f[15]) + ',"driver":' + (J $f[16]) + ',"memory_kind":"dedicated","memory_status":' + (J $status) + ',"memory_reason":' + (J $reason) + ',"sampled_at":' + $at + ',"source":"nvidia-smi query (MiB)","available":true}')
  }
  return "[" + ($rows -join ",") + "]"
}
function StopGpuQuery {
  if ($script:gpuQuery) { try { if (-not $script:gpuQuery.HasExited) { $script:gpuQuery.Kill() }; $script:gpuQuery.Dispose() } catch { } }
  $script:gpuQuery = $null; $script:gpuTask = $null; $script:gpuErrorTask = $null
}
function StartGpuQuery($memoryOnly) {
  $script:gpuStarted = $script:gpuSampleClock.Elapsed.TotalMilliseconds
  $psi = New-Object Diagnostics.ProcessStartInfo($smi, "--query-gpu=index,name,utilization.gpu,memory.used,memory.total,memory.free,temperature.gpu,power.draw,clocks.current.graphics,clocks.current.memory,uuid,pstate,power.limit,power.min_limit,power.max_limit,clocks.current.sm,driver_version,clocks_throttle_reasons.active --format=csv,noheader,nounits")
  if ($memoryOnly) { $psi.Arguments = "--query-gpu=index,name,memory.used,memory.total,memory.free --format=csv,noheader,nounits" }
  $psi.UseShellExecute = $false; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true; $psi.CreateNoWindow = $true
  try {
    $script:gpuQuery = [Diagnostics.Process]::Start($psi)
    $script:gpuTask = $script:gpuQuery.StandardOutput.ReadToEndAsync(); $script:gpuErrorTask = $script:gpuQuery.StandardError.ReadToEndAsync()
  } catch { StopGpuQuery; $script:gpuRows = "[]"; $script:gpuReason = "query-unavailable" }
}
function PollGpuQuery {
  $ms = $script:gpuSampleClock.Elapsed.TotalMilliseconds
  if ($script:gpuQuery) {
    if ($script:gpuQuery.HasExited) {
      if (-not $script:gpuTask.IsCompleted -and $ms - $script:gpuStarted -lt 4000) { return }
      if ($script:gpuQuery.ExitCode -eq 0 -and $script:gpuTask.IsCompleted -and $script:gpuTask.Result.Length -le 65536) {
        $script:gpuRows = ParseGpuRows $script:gpuTask.Result ([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())
        $script:gpuReceived = $ms; $script:gpuReason = "invalid-query-row"
      } elseif (-not $script:gpuFallback -and $script:gpuQuery.ExitCode -ne 0) {
        StopGpuQuery; $script:gpuFallback = $true; StartGpuQuery $true; return
      } else { $script:gpuRows = "[]"; $script:gpuReason = "query-failed" }
      StopGpuQuery
    } elseif ($ms - $script:gpuStarted -ge 4000) { StopGpuQuery; $script:gpuRows = "[]"; $script:gpuReason = "query-timeout" }
    return
  }
  if (-not $smi -or $ms - $script:gpuStarted -lt $gpuEveryMs) { return }
  $script:gpuFallback = $false; StartGpuQuery $false
}
function GpuJson {
  if ($null -ne $gpuReceived -and $gpuSampleClock.Elapsed.TotalMilliseconds - $gpuReceived -le $gpuMaxAgeMs -and $gpuRows -ne "[]") { return $gpuRows }
  return '[{"name":"GPU","memory_kind":"unknown","memory_status":' + (J $(if ($smi) { "unavailable" } else { "unsupported" })) + ',"memory_reason":' + (J $gpuReason) + ',"source":"nvidia-smi query","available":false}]'
}

# ---- Top processes from the Process counters: CPU of every process (also protected ones) without opening handles ----
$procPrev = @{}
function TopJson($order, $names, $pids, $cpus, $mems, [int]$n) {
  $rows = New-Object System.Collections.Generic.List[string]
  for ($j = $order.Length - 1; $j -ge 0 -and $rows.Count -lt $n; $j--) {
    $i = $order[$j]
    $c = 'null'; if ($cpus[$i] -ge 0) { $c = ([math]::Round($cpus[$i], 1)).ToString('R', $inv) }
    $rows.Add('{"pid":' + $pids[$i] + ',"name":' + (J ($names[$i] -replace '#\d+$', '')) + ',"cpu":' + $c + ',"mem_mb":' + ([math]::Round($mems[$i] / 1MB)).ToString($inv) + '}')
  }
  return ($rows -join ',')
}
function Procs {
  $d = ReadCat $procCat
  if ($null -eq $d) { return $null }
  $cpuCol = $d['% Processor Time']; $idCol = $d['ID Process']; $wsCol = $d['Working Set']
  if ($null -eq $cpuCol -or $null -eq $idCol) { return $null }
  $n = $idCol.Count
  $names = New-Object string[] $n; $pids = New-Object int[] $n; $cpus = New-Object double[] $n; $mems = New-Object double[] $n
  $cur = @{}
  $k = 0
  foreach ($x in $idCol.Values) {
    $inst = $x.InstanceName
    $id = [int]$x.RawValue
    if ($inst -eq '_Total' -or $id -eq 0) { continue }
    $s = $cpuCol[$inst].Sample
    $cur[$id] = $s
    $p = $procPrev[$id]
    $c = -1.0
    if ($null -ne $p) { try { $c = [double]$CS::Calculate($p, $s) } catch { } }
    $names[$k] = $inst; $pids[$k] = $id; $cpus[$k] = $c
    if ($null -ne $wsCol) { $mems[$k] = [double]$wsCol[$inst].RawValue }
    $k++
  }
  $script:procPrev = $cur
  # sort indexes by cpu and by memory with .NET (Sort-Object is slow in PS 5.1)
  $byCpu = New-Object int[] $k; $byMem = New-Object int[] $k
  for ($i = 0; $i -lt $k; $i++) { $byCpu[$i] = $i; $byMem[$i] = $i }
  $kc = New-Object double[] $k; $km = New-Object double[] $k
  [Array]::Copy($cpus, $kc, $k); [Array]::Copy($mems, $km, $k)
  [Array]::Sort($kc, $byCpu); [Array]::Sort($km, $byMem)
  return ('{"count":' + $k + ',"top_cpu":[' + (TopJson $byCpu $names $pids $cpus $mems $TOP) + '],"top_mem":[' + (TopJson $byMem $names $pids $cpus $mems $TOP) + ']}')
}

$has = [ordered]@{ cpu = ($null -ne $cpuCat); mem = ($null -ne $memCat); disk = ($null -ne $diskCat); net = ($null -ne $netCat); procs = ($null -ne $procCat); gpu = [bool]$smi; power = ($energyCounters.Count -gt 0) }
$sessionEpochMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$hello = [ordered]@{ type = 'hello'; v = 1; os = 'windows'; cores = [Environment]::ProcessorCount; interval = $Interval; procs_every = $ProcEvery
  mem_total_gb = $(if ($totalBytes) { [math]::Round($totalBytes / 1GB, 1) } else { $null }); session_epoch_ms = $sessionEpochMs; has = $has; errors = @($errs) }
if (-not (Emit ($hello | ConvertTo-Json -Compress -Depth 3))) { exit 0 }

$me = [Diagnostics.Process]::GetCurrentProcess()
$sw = [Diagnostics.Stopwatch]::StartNew()
$cpuPrev = Samples (ReadCat $cpuCat) '% Processor Time'
# core instances are "0".."N-1" (and "_Total"); sort once
$coreKeys = @($cpuPrev.Keys | Where-Object { $_ -match '^\d+$' } | Sort-Object { [int]$_ })
$dPrev = ReadCat $diskCat
$drPrev = Samples $dPrev 'Disk Read Bytes/sec'; $dwPrev = Samples $dPrev 'Disk Write Bytes/sec'
$nPrev = ReadCat $netCat
$rxPrev = Samples $nPrev 'Bytes Received/sec'; $txPrev = Samples $nPrev 'Bytes Sent/sec'
$netKeys = @($rxPrev.Keys | Where-Object { $_ -notmatch $NET_SKIP })
[void](Procs)
PollGpuQuery
# -WatchStdin: an async read on stdin completes with 0 bytes when the app goes away (local run).
# Over ssh that is not enough: when the session ends, Git Bash keeps copies of our stdin/stdout pipes, so neither EOF
# nor a write error arrives (seen on a real machine). So we also watch the ssh session process (sshd-session / sshd)
# that we run under, found once by walking up the parent processes, and stop when it has exited.
$stdinTask = $null
$session = $null; $sessionId = 0
if ($WatchStdin) {
  try { $stdinStream = [Console]::OpenStandardInput(); $stdinBuf = New-Object byte[] 256; $stdinTask = $stdinStream.ReadAsync($stdinBuf, 0, 256) } catch { $stdinTask = $null }
  try {
    $tree = @{}
    foreach ($p in @(Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId, Name)) { $tree[[int]$p.ProcessId] = $p }
    $cur = $tree[[int]$PID]
    for ($i = 0; $i -lt 16 -and $null -ne $cur; $i++) {
      $par = $tree[[int]$cur.ParentProcessId]
      if ($null -eq $par -or [int]$par.ProcessId -eq [int]$cur.ProcessId) { break }
      if ([string]$par.Name -match '^sshd') { $sessionId = [int]$par.ProcessId; break }
      $cur = $par
    }
    $tree = $null
    # keep a handle open (Process.Handle) so that HasExited cannot be fooled by a reused process id
    if ($sessionId) { try { $session = [Diagnostics.Process]::GetProcessById($sessionId); [void]$session.Handle; [void]$session.HasExited } catch { $session = $null } }
  } catch { $sessionId = 0 }
}
# true when the ssh session we run under has ended (handle check; by id when the handle cannot be opened)
function SessionGone {
  if ($null -ne $script:session) { try { return $script:session.HasExited } catch { $script:session = $null } }
  if ($script:sessionId) { try { [void][Diagnostics.Process]::GetProcessById($script:sessionId); return $false } catch { return $true } }
  return $false
}
$nextProcs = $Interval * 1000
$next = 0.0
$seq = 0
$sampleErrors = 0
$cores = New-Object System.Collections.Generic.List[string]
try {
  while ($true) {
    $next += $Interval * 1000
    $wait = $next - $sw.Elapsed.TotalMilliseconds
    if ($wait -gt 0) { [Threading.Thread]::Sleep([int]$wait) }
    elseif ($wait -lt -2 * $Interval * 1000) { $next = $sw.Elapsed.TotalMilliseconds }
    if ($null -ne $stdinTask -and $stdinTask.IsCompleted) {
      $got = 0
      try { $got = $stdinTask.Result } catch { }
      if ($got -le 0) { break }
      $stdinTask = $stdinStream.ReadAsync($stdinBuf, 0, 256)
    }
    if ($sessionId -and (SessionGone)) { break }
    $now = $sw.Elapsed.TotalMilliseconds
    if ($now -ge $MaxSeconds * 1000) { [void](Emit '{"type":"end","reason":"max_age"}'); break }
    $seq++
    # inside try, any error would end the script: skip this sample instead (and tell stderr a few times)
    $line = $null
    try {

    # CPU: whole machine and per core
    $total = 'null'
    $cores.Clear()
    $d = $null; try { $d = $cpuCat.ReadCategory() } catch { }
    if ($null -ne $d) {
      $col = $d['% Processor Time']
      $s = $col['_Total'].Sample; $p = $cpuPrev['_Total']
      if ($null -ne $p) { $total = ([math]::Round([math]::Min(100.0, [math]::Max(0.0, [double]$CS::Calculate($p, $s))), 1)).ToString('R', $inv) }
      $cpuPrev['_Total'] = $s
      foreach ($c in $coreKeys) {
        $s = $col[$c].Sample; $p = $cpuPrev[$c]
        if ($null -ne $p) { $cores.Add(([math]::Round([math]::Min(100.0, [math]::Max(0.0, [double]$CS::Calculate($p, $s))))).ToString($inv)) } else { $cores.Add('null') }
        $cpuPrev[$c] = $s
      }
    }

    # Memory: used = 1 - available / visible total (same as the probe); commit = committed / limit
    $usedPct = 'null'; $commitPct = 'null'; $commitGb = 'null'; $limitGb = 'null'
    $m = $null; try { $m = $memCat.ReadCategory() } catch { }
    if ($null -ne $m) {
      # single-instance category: take the only value
      $avail = [double]@($m['Available Bytes'].Values)[0].RawValue
      $committed = [double]@($m['Committed Bytes'].Values)[0].RawValue
      $limit = [double]@($m['Commit Limit'].Values)[0].RawValue
      if ($totalBytes) { $usedPct = ([math]::Round((1 - $avail / $totalBytes) * 100, 1)).ToString('R', $inv) }
      if ($limit -gt 0) { $commitPct = ([math]::Round($committed / $limit * 100, 1)).ToString('R', $inv); $limitGb = ([math]::Round($limit / 1GB, 2)).ToString('R', $inv) }
      $commitGb = ([math]::Round($committed / 1GB, 2)).ToString('R', $inv)
    }

    $line = '{"type":"s","t":' + [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() + ',"seq":' + $seq + ',"cpu":' + $total + ',"cores":[' + ($cores -join ',') + ']'
    $line += ',"mem":{"used_pct":' + $usedPct + ',"commit_pct":' + $commitPct + ',"commit_gb":' + $commitGb + ',"commit_limit_gb":' + $limitGb + '}'

    # Energy Meter Power is mW. Require every package counter to be finite and nonnegative before summing.
    $packageW = 'null'; $powerAvailable = 'false'
    if ($energyCounters.Count -gt 0) {
      if ($energyWarm) { $energyWarm = $false }
      else {
        $sumMw = 0.0; $validPower = $true
        foreach ($counter in $energyCounters) {
          $value = $null; try { $value = [double]$counter.NextValue() } catch { }
          if ($null -eq $value -or [double]::IsNaN($value) -or [double]::IsInfinity($value) -or $value -lt 0) { $validPower = $false; break }
          $sumMw += $value
        }
        if ($validPower) { $packageW = ([math]::Round($sumMw / 1000, 1)).ToString('R', $inv); $powerAvailable = 'true' }
      }
      $line += ',"power":{"package_w":' + $packageW + ',"package_source":"Energy Meter RAPL package average (mW / 1000)","available":' + $powerAvailable + '}'
    }

    # Disk: all physical disks
    $d = $null; try { $d = $diskCat.ReadCategory() } catch { }
    if ($null -ne $d) {
      $r = $d['Disk Read Bytes/sec']['_Total'].Sample; $w = $d['Disk Write Bytes/sec']['_Total'].Sample
      $rv = 'null'; $wv = 'null'
      if ($null -ne $drPrev['_Total']) { $rv = ([math]::Round([double]$CS::Calculate($drPrev['_Total'], $r))).ToString($inv); $wv = ([math]::Round([double]$CS::Calculate($dwPrev['_Total'], $w))).ToString($inv) }
      $drPrev['_Total'] = $r; $dwPrev['_Total'] = $w
      $line += ',"disk":{"read_bps":' + $rv + ',"write_bps":' + $wv + '}'
    }

    # Network: physical adapters (the list of adapters is refreshed with the processes)
    $nd = $null; try { $nd = $netCat.ReadCategory() } catch { }
    if ($null -ne $nd) {
      $rc = $nd['Bytes Received/sec']; $tc = $nd['Bytes Sent/sec']
      $sumRx = 0.0; $sumTx = 0.0
      foreach ($k in $netKeys) {
        $a = $rc[$k]; $b = $tc[$k]
        if ($null -eq $a -or $null -eq $b) { continue }
        if ($null -ne $rxPrev[$k]) { $sumRx += [double]$CS::Calculate($rxPrev[$k], $a.Sample); $sumTx += [double]$CS::Calculate($txPrev[$k], $b.Sample) }
        $rxPrev[$k] = $a.Sample; $txPrev[$k] = $b.Sample
      }
      $line += ',"net":{"rx_bps":' + ([math]::Round($sumRx)).ToString($inv) + ',"tx_bps":' + ([math]::Round($sumTx)).ToString($inv) + '}'
    }

    PollGpuQuery
    $line += ',"gpu":' + (GpuJson)
    if ($procCat -and $now -ge $nextProcs) {
      $nextProcs = $now + $ProcEvery * 1000
      $p = Procs
      if ($p) { $line += ',"procs":' + $p }
      if ($null -ne $nd) { $netKeys = @($nd['Bytes Received/sec'].Keys | Where-Object { $_ -notmatch $NET_SKIP }) }
      # this sampler's own load (cpu seconds so far, working set); the reader turns the delta into a percent
      $me.Refresh()
      $line += ',"self":{"cpu_s":' + ([math]::Round($me.TotalProcessorTime.TotalSeconds, 3)).ToString('R', $inv) + ',"rss_mb":' + ([math]::Round($me.WorkingSet64 / 1MB, 1)).ToString('R', $inv) + '}'
    }
    $line += '}'
    }
    catch {
      $line = $null
      if ($script:sampleErrors++ -lt 3) { try { [Console]::Error.WriteLine(('sample ' + $seq + ': ' + $_.Exception.Message + ' @' + $_.InvocationInfo.ScriptLineNumber)) } catch { } }
    }
    if ($null -ne $line -and -not (Emit $line)) { break }
    if ($Count -gt 0 -and $seq -ge $Count) { [void](Emit '{"type":"end","reason":"count"}'); break }
  }
}
finally { StopGpuQuery }
