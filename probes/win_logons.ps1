# Read-only Windows Security log collector. PowerShell 5.1, ASCII with BOM.
# Params: RecordId and its TimeCreated epoch ms. Events: 4625 and remote/network 4624 types 3, 8, 10.
# Access needs admin or Event Log Readers. note=no-permission keeps the old cursor and records a source error.
# Kept separate from win_logs.ps1 for the 8191-byte transport limit. Skipped when network=false.
param([long]$SecCursor = 0, [long]$SecTime = 0)
$ErrorActionPreference = 'SilentlyContinue'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$MaxRows = 300
# DateTimeOffset avoids the PS 5.1 local-time parse of the Unix epoch.
function EpochMs($d) { return [DateTimeOffset]::new($d.ToUniversalTime()).ToUnixTimeMilliseconds() }

function LogonAddr($a) {
  $s = ([string]$a).Trim()
  if (-not $s -or $s -eq '-') { return 'local' }
  if ($s -like '::ffff:*') { $s = $s.Substring(7) }
  return $s
}

function Check-Cursor([long]$cursor, [long]$cursorTime) {
  try {
    $found = @(Get-WinEvent -LogName Security -FilterXPath "*[System[EventRecordID=$cursor]]" -MaxEvents 1 -ErrorAction Stop)
  }
  catch {
    $x = $_.Exception
    if ($x -is [System.UnauthorizedAccessException] -or $x.HResult -eq -2147024891) { return [ordered]@{ note = 'no-permission' } }
    if ([string]$_.FullyQualifiedErrorId -like 'NoMatchingEventsFound*') { return [ordered]@{ reset = $true } }
    $m = [string]$x.Message; if ($m.Length -gt 200) { $m = $m.Substring(0, 200) }
    return [ordered]@{ error = $m }
  }
  return [ordered]@{ reset = ($found.Count -eq 0 -or (EpochMs $found[0].TimeCreated) -ne $cursorTime) }
}

function Read-Logons([long]$cursor, [long]$cursorTime, [string]$savedCursor) {
  if ($cursor -gt 0) {
    if ($cursorTime -le 0) { return Read-Logons 0 0 $savedCursor }
    $check = Check-Cursor $cursor $cursorTime
    if ($check.note) { return [ordered]@{ cursor = $savedCursor; rows = @(); dropped = 0; note = $check.note } }
    if ($check.error) { return [ordered]@{ error = $check.error } }
    if ($check.reset) { return Read-Logons 0 0 $savedCursor }
  }
  $cond = if ($cursor -gt 0) { "EventRecordID > $cursor" } else { 'TimeCreated[timediff(@SystemTime) <= 259200000]' }
  $q = "*[System[$cond] and (System[EventID=4625] or (System[EventID=4624] and EventData[Data[@Name='LogonType']='3' or Data[@Name='LogonType']='8' or Data[@Name='LogonType']='10']))]"
  try { $ev = @(Get-WinEvent -LogName Security -FilterXPath $q -Oldest -MaxEvents ($MaxRows + 1) -ErrorAction Stop) }
  catch {
    $x = $_.Exception
    if ($x -is [System.UnauthorizedAccessException] -or $x.HResult -eq -2147024891) { return [ordered]@{ cursor = $savedCursor; rows = @(); dropped = 0; note = 'no-permission' } }
    if ([string]$_.FullyQualifiedErrorId -notlike 'NoMatchingEventsFound*') { $m = [string]$x.Message; if ($m.Length -gt 200) { $m = $m.Substring(0, 200) }; return [ordered]@{ error = $m } }
    $ev = @()
  }
  if ($cursor -gt 0) {
    $check = Check-Cursor $cursor $cursorTime
    if ($check.note) { return [ordered]@{ cursor = $savedCursor; rows = @(); dropped = 0; note = $check.note } }
    if ($check.error) { return [ordered]@{ error = $check.error } }
    if ($check.reset) { return Read-Logons 0 0 $savedCursor }
  }
  $batch = @($ev | Select-Object -First $MaxRows)
  $newCursor = if ($batch.Count -gt 0) { [string]$batch[-1].RecordId + ':' + [string](EpochMs $batch[-1].TimeCreated) } elseif ($cursor -gt 0) { [string]$cursor + ':' + [string]$cursorTime } else { '0' }
  $rows = New-Object System.Collections.ArrayList
  foreach ($e in $batch) {
    $d = @{}
    foreach ($n in @(([xml]$e.ToXml()).Event.EventData.Data)) { $d[[string]$n.Name] = [string]$n.'#text' }
    $user = [string]$d['TargetUserName']
    $addr = LogonAddr $d['IpAddress']
    if ($e.Id -eq 4624) {
      if ($user -like '*$' -or $user -eq 'ANONYMOUS LOGON' -or $addr -eq 'local' -or $addr -eq '127.0.0.1' -or $addr -eq '::1') { continue }
      $msg = 'logon ok: type ' + $d['LogonType'] + ', account ' + $user + ', from ' + $addr + ', ' + $d['AuthenticationPackageName']
      $lvl = 'info'
    } else {
      $msg = 'logon failed: type ' + $d['LogonType'] + ', account ' + $user + ', from ' + $addr + ', status ' + $d['Status'] + '/' + $d['SubStatus']
      $lvl = 'warn'
    }
    $stamp = EpochMs $e.TimeCreated
    [void]$rows.Add([pscustomobject]@{ uid = ([string]$e.RecordId + ":" + [string]$stamp); ts = $stamp; level = $lvl; provider = $addr; event_id = [string]$e.Id; message = $msg })
  }
  return [ordered]@{ cursor = $newCursor; rows = @($rows); dropped = 0 }
}

$savedCursor = if ($SecCursor -gt 0 -and $SecTime -gt 0) { [string]$SecCursor + ':' + [string]$SecTime } else { [string]$SecCursor }
$result = [ordered]@{ probe = 'win_logons'; sources = [ordered]@{ win_security = (Read-Logons $SecCursor $SecTime $savedCursor) } }
$result | ConvertTo-Json -Depth 6 -Compress
