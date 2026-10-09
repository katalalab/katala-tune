# katala-tune Windows log collector. Read-only, prints one JSON line. PS 5.1, ASCII only (BOM).
# -SysCursor/-AppCursor <RecordId> -NeonCursor <byte offset>. Sources: win_system (Critical/Error + hardware warnings),
# win_application (Critical/Error), neonmonitor (%APPDATA%\NeonMonitor\guard.log).
param([long]$SysCursor = 0, [long]$AppCursor = 0, [long]$NeonCursor = 0)
$ErrorActionPreference = 'SilentlyContinue'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$MaxRows = 300
$WarnProviders = '^(Microsoft-Windows-WHEA-Logger|Display|nvlddmkm|disk|Ntfs|stornvme|storahci|Microsoft-Windows-Kernel-Power|Microsoft-Windows-Resource-Exhaustion-Detector|Microsoft-Windows-Kernel-Boot|volmgr|BugCheck)$'
function EpochMs($d) { return [DateTimeOffset]::new($d.ToUniversalTime()).ToUnixTimeMilliseconds() }

function Norm($m) {
  $s = $m.ToLower() -replace '\{?[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}\}?', '<g>' -replace '0x[0-9a-f]+', '<h>' -replace '[a-z]:\\[^\s"'']+', '<p>'
  $s = $s -replace '"[^"]*"|''[^'']*''', '<q>' -replace '[0-9]+(\.[0-9]+)?', '<n>' -replace '\s+', ' '
  return $s.Trim()
}

function Read-EventLog($logName, [long]$cursor, [bool]$withWarnings) {
  $lv = if ($withWarnings) { '(Level=1 or Level=2 or Level=3)' } else { '(Level=1 or Level=2)' }
  $cond = if ($cursor -gt 0) { "EventRecordID > $cursor" } else { 'TimeCreated[timediff(@SystemTime) <= 259200000]' }
  $xpath = "*[System[$lv and $cond]]"
  $ev = @(Get-WinEvent -LogName $logName -FilterXPath $xpath -MaxEvents 2000 -ErrorAction SilentlyContinue)
  if ($withWarnings) { $ev = @($ev | Where-Object { $_.Level -ne 3 -or $_.ProviderName -match $WarnProviders }) }
  $newest = $cursor
  foreach ($e in $ev) { if ($e.RecordId -gt $newest) { $newest = $e.RecordId } }
  $g = @{}
  $o = @()
  foreach ($e in @($ev | Sort-Object RecordId)) {
    $msg = $e.Message
    if (-not $msg) { $msg = "(no message) " + (($e.Properties | ForEach-Object { $_.Value }) -join ' ') }
    if ($msg.Length -gt 1000) { $msg = $msg.Substring(0, 1000) }
    $ts = EpochMs $e.TimeCreated
    $k = "$($e.ProviderName)|$($e.Id)|" + (Norm $msg)
    if ($g[$k]) { $g[$k].count++; $g[$k].ts = $ts; continue }
    $lvl = switch ($e.Level) { 1 { 'critical' } 2 { 'error' } 3 { 'warn' } default { 'info' } }
    $r = [pscustomobject]@{ uid = [string]$e.RecordId; ts = $ts; level = $lvl; provider = $e.ProviderName; event_id = [string]$e.Id; message = $msg; count = 1 }
    $g[$k] = $r
    $o += $r
  }
  $o = @($o | Sort-Object ts)
  $keep = @($o | Select-Object -Last $MaxRows)
  $cut = $o.Count - $keep.Count
  $dropped = 0
  if ($cut -gt 0) { $dropped = ($o | Select-Object -First $cut | Measure-Object -Property count -Sum).Sum }
  return [ordered]@{ cursor = [string]$newest; rows = $keep; dropped = $dropped; events = $ev.Count }
}

function Read-Neon([long]$offset) {
  $dir = Join-Path $env:APPDATA 'NeonMonitor'
  $f = Join-Path $dir 'guard.log'
  $meta = [ordered]@{ installed = (Test-Path $dir); guard_mode = $null }
  $ini = Join-Path $dir 'settings.ini'
  if (Test-Path $ini) {
    foreach ($line in Get-Content $ini) {
      if ($line -match '^guard_mode=(\d+)') { $meta.guard_mode = [int]$Matches[1] }
    }
  }
  if (-not (Test-Path $f)) { return [ordered]@{ cursor = [string]$offset; rows = @(); dropped = 0; meta = $meta; note = $(if ($meta.installed) { 'no-guard-log' } else { 'not-installed' }) } }
  $len = (Get-Item $f).Length
  if ($len -lt $offset) { $offset = 0 }
  $max = 262144
  $fs = [System.IO.File]::Open($f, 'Open', 'Read', 'ReadWrite')
  try {
    [void]$fs.Seek($offset, 'Begin')
    $n = [int][math]::Min($max, $len - $offset)
    $buf = New-Object byte[] $n
    $read = $fs.Read($buf, 0, $n)
  } finally { $fs.Close() }
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

$result = [ordered]@{
  probe = 'win_logs'
  sources = [ordered]@{
    win_system = (Read-EventLog 'System' $SysCursor $true)
    win_application = (Read-EventLog 'Application' $AppCursor $false)
    neonmonitor = (Read-Neon $NeonCursor)
  }
}
$result | ConvertTo-Json -Depth 6 -Compress
