# katala-tune Windows logon collector (network and security, docs/observability.md section 6). Read-only: prints one JSON line.
# PowerShell 5.1, ASCII only (BOM). Usage: -SecCursor <RecordId>
#   win_security  Security log: failed logons (4625) and successful network / remote logons (4624, types 3, 8, 10).
#                 Only time, logon type, account, source address and status. Reading it needs admin rights or the
#                 Event Log Readers group; without them nothing is read and note = 'no-permission' (not an error).
# Separate from win_logs.ps1 because the remote transport passes the script on the command line (8191 bytes at most).
# Not run for nodes with "network": false in nodes.json.
param([long]$SecCursor = 0)
$ErrorActionPreference = 'SilentlyContinue'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$MaxRows = 300
# NOTE: [DateTime]'1970-01-01T00:00:00Z' is parsed as LOCAL time in PS 5.1 (off by the UTC offset). Use DateTimeOffset.
function EpochMs($d) { return [DateTimeOffset]::new($d.ToUniversalTime()).ToUnixTimeMilliseconds() }

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

$result = [ordered]@{ probe = 'win_logons'; sources = [ordered]@{ win_security = (Read-Logons $SecCursor) } }
$result | ConvertTo-Json -Depth 6 -Compress
