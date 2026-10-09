# Windows PowerShell 5.1 regression tests for probes/win_logons.ps1.
# Get-WinEvent is shadowed. The real Security log is never read, cleared or written.
$ErrorActionPreference = 'Stop'
if ($PSVersionTable.PSVersion.Major -ne 5) { throw "expected Windows PowerShell 5.1, got $($PSVersionTable.PSVersion)" }
$Root = Split-Path -Parent $PSScriptRoot
$ProbePath = Join-Path $Root 'probes\win_logons.ps1'
$ProbeBytes = [IO.File]::ReadAllBytes($ProbePath)
if ($ProbeBytes.Length -lt 4 -or $ProbeBytes[0] -ne 0xEF -or $ProbeBytes[1] -ne 0xBB -or $ProbeBytes[2] -ne 0xBF) { throw 'win_logons.ps1 must have a UTF-8 BOM' }
foreach ($b in $ProbeBytes[3..($ProbeBytes.Length - 1)]) { if ($b -gt 0x7F) { throw 'win_logons.ps1 body must be ASCII' } }
$ProbeSource = [IO.File]::ReadAllText($ProbePath, ([Text.UTF8Encoding]::new($true)))
if ($ProbeSource.Length -gt 6000) { throw 'win_logons.ps1 exceeded the transport source budget' }

function Assert-KtEqual($Actual, $Expected, [string]$Message) {
  if ([string]$Actual -ne [string]$Expected) { throw "$Message (actual=$Actual expected=$Expected)" }
}
function Assert-KtTrue([bool]$Value, [string]$Message) { if (-not $Value) { throw $Message } }
function New-KtNoMatch {
  $e = [Exception]::new('No events were found')
  return [System.Management.Automation.ErrorRecord]::new($e, 'NoMatchingEventsFound', [System.Management.Automation.ErrorCategory]::ObjectNotFound, $null)
}
function New-KtEvent([long]$RecordId, [long]$TimeKey = -1) {
  if ($TimeKey -lt 0) { $TimeKey = $RecordId }
  $at = [DateTime]::SpecifyKind(([datetime]'2026-01-01T00:00:00').AddSeconds($TimeKey), [DateTimeKind]::Utc)
  $xml = "<Event><EventData><Data Name='TargetUserName'>fixture-user</Data><Data Name='IpAddress'>203.0.113.10</Data><Data Name='LogonType'>3</Data><Data Name='AuthenticationPackageName'>Negotiate</Data><Data Name='Status'>0xC000006D</Data><Data Name='SubStatus'>0xC000006A</Data></EventData></Event>"
  $e = [pscustomobject]@{ RecordId = $RecordId; Id = 4625; TimeCreated = $at; FixtureXml = $xml }
  $e | Add-Member -MemberType ScriptMethod -Name ToXml -Value { return $this.FixtureXml }
  return $e
}
function Get-KtEpoch($Event) { return [DateTimeOffset]::new($Event.TimeCreated).ToUnixTimeMilliseconds() }

function Invoke-KtProbe([string]$Fixture, [long]$Cursor, [long]$CursorTime) {
  $script:KtFixture = $Fixture
  $script:KtCalls = 0
  $script:KtXpaths = New-Object System.Collections.ArrayList
  function Get-WinEvent {
    [CmdletBinding()]
    param([string]$LogName, [int]$MaxEvents, [string]$FilterXPath, [switch]$Oldest)
    if ($LogName -ne 'Security') { throw 'fixture received a non-Security query' }
    $script:KtCalls++
    if ($FilterXPath) { [void]$script:KtXpaths.Add($FilterXPath) }
    $identity = $FilterXPath -match 'EventRecordID=([0-9]+)'
    switch ($script:KtFixture) {
      'reset-301' { return @(1..301 | ForEach-Object { New-KtEvent $_ }) }
      'next-one' { if ($identity) { return @(New-KtEvent 300) }; return @(New-KtEvent 301) }
      'empty-current' { $PSCmdlet.ThrowTerminatingError((New-KtNoMatch)) }
      'no-permission-identity' { throw ([System.UnauthorizedAccessException]::new('fixture denied')) }
      'no-permission-query' { if ($identity) { return @(New-KtEvent 702) }; throw ([System.UnauthorizedAccessException]::new('fixture denied')) }
      'legacy-query-denied' { throw ([System.UnauthorizedAccessException]::new('fixture denied')) }
      'clear-reuse' {
        if ($script:KtCalls -eq 1) { return @(New-KtEvent 300) }
        if ($script:KtCalls -eq 2) { return @(New-KtEvent 400 1000) }
        if ($script:KtCalls -eq 3) { return @(New-KtEvent 300 900) }
        return @(New-KtEvent 2 902)
      }
      'clear-recheck-denied' {
        if ($script:KtCalls -eq 1) { return @(New-KtEvent 300) }
        if ($script:KtCalls -eq 2) { return @(New-KtEvent 400 1000) }
        throw ([System.UnauthorizedAccessException]::new('fixture denied'))
      }
      default { throw "unknown fixture $script:KtFixture" }
    }
  }
  $text = & ([ScriptBlock]::Create($ProbeSource)) -SecCursor $Cursor -SecTime $CursorTime | Out-String
  return [pscustomobject]@{ Output = ($text | ConvertFrom-Json); Calls = $script:KtCalls; XPaths = @($script:KtXpaths) }
}

$t300 = Get-KtEpoch (New-KtEvent 300)
$t301 = Get-KtEpoch (New-KtEvent 301)
$t702 = Get-KtEpoch (New-KtEvent 702)
$t2new = Get-KtEpoch (New-KtEvent 2 902)

$reset = Invoke-KtProbe 'reset-301' 900 0
$r = $reset.Output.sources.win_security
Assert-KtEqual $r.cursor ("300:" + $t300) 'legacy cursor must reset and gain the event timestamp'
Assert-KtEqual (@($r.rows).Count) 300 'reset batch must be bounded to 300 rows'
Assert-KtEqual $reset.Calls 1 'legacy cursor must reset without trusting RecordId alone'
Assert-KtTrue ($reset.XPaths[0] -like '*259200000*') 'reset query must use the three-day window'
$uids = @($r.rows | ForEach-Object { $_.uid } | Select-Object -Unique)
Assert-KtEqual $uids.Count 300 'RecordId:EpochMs UIDs must be unique within the reset batch'
Assert-KtTrue ($r.rows[0].uid -match '^1:[0-9]+$') 'event UID must contain RecordId and epoch milliseconds'

$next = Invoke-KtProbe 'next-one' 300 $t300
$n = $next.Output.sources.win_security
Assert-KtEqual $n.cursor ("301:" + $t301) 'the next page must advance by exactly one event'
Assert-KtEqual (@($n.rows).Count) 1 'the next page must return the 301st event only'
Assert-KtEqual $next.Calls 3 'incremental read must verify cursor identity before and after the query'
Assert-KtTrue (@($next.XPaths | Where-Object { $_ -like '*EventRecordID > 300*' }).Count -eq 1) 'incremental query must use the current cursor'

$empty = Invoke-KtProbe 'empty-current' 700 (Get-KtEpoch (New-KtEvent 700))
$e = $empty.Output.sources.win_security
Assert-KtEqual $e.cursor 0 'a missing cursor identity and empty Security log must reset to zero'
Assert-KtEqual (@($e.rows).Count) 0 'an empty current Security log must return no rows'
Assert-KtEqual $empty.Calls 2 'identity miss must perform one bounded zero-cursor query'

$deniedIdentity = Invoke-KtProbe 'no-permission-identity' 701 (Get-KtEpoch (New-KtEvent 701))
$d1 = $deniedIdentity.Output.sources.win_security
Assert-KtEqual $d1.cursor ("701:" + (Get-KtEpoch (New-KtEvent 701))) 'identity permission failure must preserve the composite cursor'
Assert-KtEqual $d1.note 'no-permission' 'identity permission failure must be explicit'

$deniedQuery = Invoke-KtProbe 'no-permission-query' 702 $t702
$d2 = $deniedQuery.Output.sources.win_security
Assert-KtEqual $d2.cursor ("702:" + $t702) 'query permission failure must preserve the composite cursor'
Assert-KtEqual $d2.note 'no-permission' 'query permission failure must be explicit'
Assert-KtEqual $deniedQuery.Calls 2 'query permission fixture must pass the identity check first'

$legacyDenied = Invoke-KtProbe 'legacy-query-denied' 703 0
$ld = $legacyDenied.Output.sources.win_security
Assert-KtEqual $ld.cursor 703 'legacy reset followed by no permission must preserve the DB cursor'
Assert-KtEqual $ld.note 'no-permission' 'legacy permission failure must not be reported as success'

$race = Invoke-KtProbe 'clear-reuse' 300 $t300
$rr = $race.Output.sources.win_security
Assert-KtEqual $rr.cursor ("2:" + $t2new) 'RecordId reuse between query phases must discard the stale incremental page and restart'
Assert-KtEqual (@($rr.rows).Count) 1 'the retry must return the post-clear low RecordId event'
Assert-KtEqual $race.Calls 4 'the clear race must perform precheck, query, postcheck and one bounded retry'

$raceDenied = Invoke-KtProbe 'clear-recheck-denied' 300 $t300
$rd = $raceDenied.Output.sources.win_security
Assert-KtEqual $rd.cursor ("300:" + $t300) 'post-query identity permission failure must preserve the old cursor'
Assert-KtEqual $rd.note 'no-permission' 'post-query identity failure must not be hidden'
Assert-KtEqual (@($rd.rows).Count) 0 'an unverified incremental page must not be emitted'

Write-Output 'win_logons regression fixtures passed'
