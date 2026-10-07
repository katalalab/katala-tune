# katala-tune Windows log collector. Read-only: prints one JSON line.
# PowerShell 5.1, ASCII only (BOM). Usage: -SysCursor <RecordId> -AppCursor <RecordId> -NeonCursor <byte offset>
# Sources:
#   win_system       System log: Critical/Error, plus warnings from hardware/driver/memory providers
#   win_application  Application log: Critical/Error (app crashes and hangs)
#   neonmonitor      %APPDATA%\NeonMonitor\guard.log (memory guard events), read from the last byte offset
#   win_security     Security log: failed logons (4625) and successful network / remote logons (4624, types 3, 8, 10).
#                    Only time, logon type, account, source address and status. Reading it needs admin rights or the
#                    Event Log Readers group; without them nothing is read and note = 'no-permission' (not an error).
#                    -NoNetwork (nodes.json "network": false) skips it.
param([long]$SysCursor = 0, [long]$AppCursor = 0, [long]$NeonCursor = 0, [long]$SecCursor = 0, [switch]$NoNetwork)
$ErrorActionPreference = 'SilentlyContinue'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$MaxRows = 300
$WarnProviders = '^(Microsoft-Windows-WHEA-Logger|Display|nvlddmkm|disk|Ntfs|stornvme|storahci|Microsoft-Windows-Kernel-Power|Microsoft-Windows-Resource-Exhaustion-Detector|Microsoft-Windows-Kernel-Boot|volmgr|BugCheck)$'
# NOTE: [DateTime]'1970-01-01T00:00:00Z' is parsed as LOCAL time in PS 5.1 (off by the UTC offset). Use DateTimeOffset.
function EpochMs($d) { return [DateTimeOffset]::new($d.ToUniversalTime()).ToUnixTimeMilliseconds() }

function Read-EventLog($logName, [long]$cursor, [bool]$withWarnings) {
  $lv = if ($withWarnings) { '(Level=1 or Level=2 or Level=3)' } else { '(Level=1 or Level=2)' }
  # First run: last 3 days. Later: everything after the last RecordId.
  $cond = if ($cursor -gt 0) { "EventRecordID > $cursor" } else { 'TimeCreated[timediff(@SystemTime) <= 259200000]' }
  $xpath = "*[System[$lv and $cond]]"
  $ev = @(Get-WinEvent -LogName $logName -FilterXPath $xpath -MaxEvents 2000 -ErrorAction SilentlyContinue)
  if ($withWarnings) { $ev = @($ev | Where-Object { $_.Level -ne 3 -or $_.ProviderName -match $WarnProviders }) }
  $newest = $cursor
  foreach ($e in $ev) { if ($e.RecordId -gt $newest) { $newest = $e.RecordId } }
  $sorted = @($ev | Sort-Object RecordId)
  $dropped = [math]::Max(0, $sorted.Count - $MaxRows)
  $keep = @($sorted | Select-Object -Last $MaxRows)
  $rows = foreach ($e in $keep) {
    $msg = $e.Message
    if (-not $msg) { $msg = "(no message) " + (($e.Properties | ForEach-Object { $_.Value }) -join ' ') }
    $lvl = switch ($e.Level) { 1 { 'critical' } 2 { 'error' } 3 { 'warn' } default { 'info' } }
    [pscustomobject]@{
      uid = [string]$e.RecordId
      ts = (EpochMs $e.TimeCreated)
      level = $lvl; provider = $e.ProviderName; event_id = [string]$e.Id
      message = if ($msg.Length -gt 1000) { $msg.Substring(0, 1000) } else { $msg }
    }
  }
  return [ordered]@{ cursor = [string]$newest; rows = @($rows); dropped = $dropped }
}

function Read-Neon([long]$offset) {
  $dir = Join-Path $env:APPDATA 'NeonMonitor'
  $f = Join-Path $dir 'guard.log'
  $meta = [ordered]@{ installed = (Test-Path $dir); guard_mode = $null; threshold = $null }
  $ini = Join-Path $dir 'settings.ini'
  if (Test-Path $ini) {
    foreach ($line in Get-Content $ini) {
      if ($line -match '^guard_mode=(\d+)') { $meta.guard_mode = [int]$Matches[1] }
      if ($line -match '^guard_threshold=(\d+)') { $meta.threshold = [int]$Matches[1] }
    }
  }
  if (-not (Test-Path $f)) { return [ordered]@{ cursor = [string]$offset; rows = @(); dropped = 0; meta = $meta; note = 'guard.log not found' } }
  $len = (Get-Item $f).Length
  if ($len -lt $offset) { $offset = 0 }  # rotated or truncated
  $max = 262144
  $fs = [System.IO.File]::Open($f, 'Open', 'Read', 'ReadWrite')
  try {
    [void]$fs.Seek($offset, 'Begin')
    $n = [int][math]::Min($max, $len - $offset)
    $buf = New-Object byte[] $n
    $read = $fs.Read($buf, 0, $n)
  } finally { $fs.Close() }
  # Only complete lines; a partial last line is read next time.
  $last = [Array]::LastIndexOf($buf, [byte]10, $read - 1)
  if ($last -lt 0) { return [ordered]@{ cursor = [string]$offset; rows = @(); dropped = 0; meta = $meta } }
  $text = [System.Text.Encoding]::UTF8.GetString($buf, 0, $last + 1)
  $mtime = (EpochMs (Get-Item $f).LastWriteTime)
  $rows = @()
  $pos = $offset
  foreach ($line in ($text -split "`n")) {
    $byteLen = [System.Text.Encoding]::UTF8.GetByteCount($line) + 1
    $l = $line.TrimEnd("`r")
    if ($l.Trim()) {
      $ts = $mtime
      if ($l -match '^\[?(\d{4}[-/]\d{2}[-/]\d{2}[ T]\d{2}:\d{2}:\d{2})') {
        $d = [DateTime]::MinValue
        if ([DateTime]::TryParse($Matches[1].Replace('/', '-'), [ref]$d)) { $ts = (EpochMs $d) }
      }
      $lvl = 'info'
      if ($l -match '(?i)kill|terminat|\u5F37\u5236\u7D42\u4E86') { $lvl = 'warn' }
      if ($l -match '(?i)error|fail|exception|\u5931\u6557') { $lvl = 'error' }
      $rows += [pscustomobject]@{ uid = [string]$pos; ts = $ts; level = $lvl; provider = 'NeonMonitor'; event_id = $null; message = $(if ($l.Length -gt 1000) { $l.Substring(0, 1000) } else { $l }) }
    }
    $pos += $byteLen
  }
  return [ordered]@{ cursor = [string]($offset + $last + 1); rows = @($rows); dropped = 0; meta = $meta }
}

function LogonAddr($a) {
  $s = ([string]$a).Trim()
  if (-not $s -or $s -eq '-') { return 'local' }
  if ($s -like '::ffff:*') { $s = $s.Substring(7) }
  return $s
}

# One row per logon event. provider = source address ('local' when none), so the per-source counts come from the log tables.
function Read-Logons([long]$cursor) {
  $cond = if ($cursor -gt 0) { "EventRecordID > $cursor" } else { 'TimeCreated[timediff(@SystemTime) <= 259200000]' }
  $queries = @(
    "*[System[EventID=4625 and $cond]]",
    "*[System[EventID=4624 and $cond] and EventData[Data[@Name='LogonType']='3' or Data[@Name='LogonType']='8' or Data[@Name='LogonType']='10']]"
  )
  $ev = @()
  foreach ($q in $queries) {
    try { $ev += @(Get-WinEvent -LogName Security -FilterXPath $q -MaxEvents 2000 -ErrorAction Stop) }
    catch {
      $x = $_.Exception
      if ($x -is [System.UnauthorizedAccessException] -or $x.HResult -eq -2147024891) { return [ordered]@{ cursor = [string]$cursor; rows = @(); dropped = 0; note = 'no-permission' } }
      if ([string]$_.FullyQualifiedErrorId -notlike 'NoMatchingEventsFound*') {
        $m = [string]$x.Message
        if ($m.Length -gt 200) { $m = $m.Substring(0, 200) }
        return [ordered]@{ error = $m }
      }
    }
  }
  $newest = $cursor
  foreach ($e in $ev) { if ($e.RecordId -gt $newest) { $newest = $e.RecordId } }
  $rows = New-Object System.Collections.ArrayList
  foreach ($e in @($ev | Sort-Object RecordId)) {
    $d = @{}
    foreach ($n in @(([xml]$e.ToXml()).Event.EventData.Data)) { $d[[string]$n.Name] = [string]$n.'#text' }
    $user = [string]$d['TargetUserName']
    $addr = LogonAddr $d['IpAddress']
    if ($e.Id -eq 4624) {
      # machine accounts, anonymous and loopback sign-ins are local plumbing, not remote logons
      if ($user -like '*$' -or $user -eq 'ANONYMOUS LOGON' -or $addr -eq 'local' -or $addr -eq '127.0.0.1' -or $addr -eq '::1') { continue }
      $msg = 'logon ok: type ' + $d['LogonType'] + ', account ' + $user + ', from ' + $addr + ', ' + $d['AuthenticationPackageName']
      $lvl = 'info'
    } else {
      $msg = 'logon failed: type ' + $d['LogonType'] + ', account ' + $user + ', from ' + $addr + ', status ' + $d['Status'] + '/' + $d['SubStatus']
      $lvl = 'warn'
    }
    [void]$rows.Add([pscustomobject]@{ uid = [string]$e.RecordId; ts = (EpochMs $e.TimeCreated); level = $lvl; provider = $addr; event_id = [string]$e.Id; message = $msg })
  }
  $dropped = [math]::Max(0, $rows.Count - $MaxRows)
  return [ordered]@{ cursor = [string]$newest; rows = @($rows | Select-Object -Last $MaxRows); dropped = $dropped }
}

$result = [ordered]@{
  probe = 'win_logs'
  sources = [ordered]@{
    win_system = (Read-EventLog 'System' $SysCursor $true)
    win_application = (Read-EventLog 'Application' $AppCursor $false)
    neonmonitor = (Read-Neon $NeonCursor)
  }
}
if (-not $NoNetwork) { $result.sources.win_security = (Read-Logons $SecCursor) }
$result | ConvertTo-Json -Depth 6 -Compress
