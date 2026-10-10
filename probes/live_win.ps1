# katala-tune live sampler (Windows). Read-only: prints one JSON line per interval to stdout until the reader goes away.
# PowerShell 5.1. Keep this file ASCII only (PS 5.1 reads BOM-less UTF-8 as the ANSI code page).
# Reads performance counters in-process (.NET PerformanceCounterCategory with English names, which also works on
# localized Windows): no WMI polling. GPU (only when nvidia-smi exists) comes from one `nvidia-smi dmon` child that
# ends by itself after 120 samples (so it cannot outlive this sampler for long even if this process is killed) and is
# restarted while we run. Collects numbers and, for top processes, PID and executable name only
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

# ---- GPU (nvidia-smi dmon: sm = utilization.gpu, fb = memory used MB) ----
$smi = $null
$sc = Get-Command nvidia-smi -ErrorAction SilentlyContinue
if ($sc) { $smi = $sc.Source }
$gpuName = @{}; $gpuTotal = @{}; $gpuNow = @{}
$dmon = $null; $dmonTask = $null; $dmonCols = $null
if ($smi) {
  foreach ($line in @(& $smi --query-gpu=index,name,memory.total --format=csv,noheader,nounits 2>$null)) {
    $f = ([string]$line) -split ',\s*'
    if ($f.Count -ge 3) { $gpuName[[int]$f[0]] = $f[1]; $gpuTotal[[int]$f[0]] = $f[2] }
  }
}
function StartDmon {
  $psi = New-Object Diagnostics.ProcessStartInfo($smi, ('dmon -s pucm -d ' + [int][math]::Max(1, [math]::Round($Interval)) + ' -c 120'))
  $psi.UseShellExecute = $false
  $psi.RedirectStandardOutput = $true
  $psi.CreateNoWindow = $true
  try { $script:dmon = [Diagnostics.Process]::Start($psi); $script:dmonTask = $script:dmon.StandardOutput.ReadLineAsync() }
  catch { $script:dmon = $null; $script:dmonTask = $null }
}
# Read the lines dmon has written so far, without waiting.
function PollDmon {
  if ($null -eq $script:dmon) { StartDmon; return }
  while ($null -ne $script:dmonTask -and $script:dmonTask.IsCompleted) {
    $line = $null
    try { $line = $script:dmonTask.Result } catch { }
    if ($null -eq $line) {
      # dmon ended (sample count reached): start a new one on the next poll
      try { $script:dmon.Dispose() } catch { }
      $script:dmon = $null; $script:dmonTask = $null
      return
    }
    if ($line -match '^#\s*gpu') { $script:dmonCols = @($line.TrimStart('#').Trim() -split '\s+') }
    elseif ($line -notmatch '^#' -and $script:dmonCols) {
      $f = @($line.Trim() -split '\s+')
      $gi = [array]::IndexOf($script:dmonCols, 'gpu'); $si = [array]::IndexOf($script:dmonCols, 'sm'); $fi = [array]::IndexOf($script:dmonCols, 'fb')
      if ($gi -ge 0 -and $f.Count -eq $script:dmonCols.Count) {
        $u = $null; if ($si -ge 0) { $u = $f[$si] }
        $m = $null; if ($fi -ge 0) { $m = $f[$fi] }
        $pi = [array]::IndexOf($script:dmonCols, 'pwr'); $ti = [array]::IndexOf($script:dmonCols, 'gtemp')
        $gci = [array]::IndexOf($script:dmonCols, 'gclk'); if ($gci -lt 0) { $gci = [array]::IndexOf($script:dmonCols, 'pclk') }; $sci = [array]::IndexOf($script:dmonCols, 'smclk'); $mci = [array]::IndexOf($script:dmonCols, 'mclk')
        $script:gpuNow[[int]$f[$gi]] = [ordered]@{
          util = $u; mem = $m
          power = $(if ($pi -ge 0) { $f[$pi] } else { $null }); temp = $(if ($ti -ge 0) { $f[$ti] } else { $null })
          graphics = $(if ($gci -ge 0) { $f[$gci] } else { $null }); sm_clock = $(if ($sci -ge 0) { $f[$sci] } else { $null }); memory_clock = $(if ($mci -ge 0) { $f[$mci] } else { $null })
        }
      }
    }
    $script:dmonTask = $script:dmon.StandardOutput.ReadLineAsync()
  }
}
function StopDmon { if ($script:dmon) { try { if (-not $script:dmon.HasExited) { $script:dmon.Kill() } } catch { } } }
function GpuJson {
  $rows = New-Object System.Collections.Generic.List[string]
  foreach ($k in $gpuNow.Keys) {
    $v = $gpuNow[$k]
    $rows.Add('{"name":' + (J $gpuName[$k]) + ',"util":' + (N $v.util 0) + ',"mem_used_mb":' + (N $v.mem 0) + ',"mem_total_mb":' + (N $gpuTotal[$k] 0) + ',"power_w":' + (N $v.power) + ',"temp_c":' + (N $v.temp 0) + ',"clocks_graphics_mhz":' + (N $v.graphics 0) + ',"clocks_sm_mhz":' + (N $v.sm_clock 0) + ',"clocks_memory_mhz":' + (N $v.memory_clock 0) + ',"source":"nvidia-smi dmon","available":true}')
  }
  return ('[' + ($rows -join ',') + ']')
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

$has = [ordered]@{ cpu = ($null -ne $cpuCat); mem = ($null -ne $memCat); disk = ($null -ne $diskCat); net = ($null -ne $netCat); procs = ($null -ne $procCat); gpu = [bool]$smi }
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
if ($smi) { PollDmon }
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

    if ($smi) {
      PollDmon
      if ($gpuNow.Count -gt 0) { $line += ',"gpu":' + (GpuJson) }
    }
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
finally { StopDmon }
