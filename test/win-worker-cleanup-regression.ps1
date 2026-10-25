# Pure PS5.1 fakes: no real processes, sleeps, host probes, or PID-based mutations.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'worker-fixture-cleanup.ps1')
$stage = 'root-race'
function Assert-KtEqual($Actual, $Expected) {
  if ($Actual -cne $Expected) { throw 'fixture assertion failed' }
}
function New-KtFakeProcess([string]$Mode) {
  $fake = [pscustomobject]@{ state = @{ mode = $Mode; exited = ($Mode -eq 'exited'); observed = 0; kills = 0; waits = 0; disposed = 0; wait_ms = -1 } }
  $fake | Add-Member ScriptProperty HasExited {
    $this.state.observed++
    if ($this.state.mode -eq 'not-started') { throw 'not started' }
    return $this.state.exited
  }
  $fake | Add-Member ScriptMethod Kill {
    $this.state.kills++
    if ($this.state.mode -eq 'race') { $this.state.exited = $true; throw [ComponentModel.Win32Exception]::new(5) }
    if ($this.state.mode -eq 'denied') { throw [ComponentModel.Win32Exception]::new(5) }
    if ($this.state.mode -ne 'no-exit' -and $this.state.mode -ne 'inconsistent') { $this.state.exited = $true }
  }
  $fake | Add-Member ScriptMethod WaitForExit {
    param([int]$Milliseconds)
    $this.state.waits++; $this.state.wait_ms = $Milliseconds
    if ($this.state.mode -eq 'wait-throws') { throw 'wait failed' }
    if ($this.state.mode -eq 'inconsistent') { return $true }
    return $this.state.exited
  }
  $fake | Add-Member ScriptMethod Dispose { $this.state.disposed++ }
  return $fake
}
try {
  $deadline = [Diagnostics.Stopwatch]::GetTimestamp() + [Diagnostics.Stopwatch]::Frequency * 2
  $fake = New-KtFakeProcess 'race'
  $result = Stop-KtOwnedFixtureRoot $fake $true $deadline
  Assert-KtEqual $result.exit_confirmed $true
  Assert-KtEqual $result.kill_failed $true
  Assert-KtEqual $fake.state.kills 1
  Assert-KtEqual $fake.state.waits 1
  Assert-KtEqual $fake.state.disposed 1
  foreach ($mode in 'denied','no-exit','wait-throws','inconsistent','exited','normal','not-started') {
    $stage = $mode
    $fake = New-KtFakeProcess $mode
    $started = $mode -ne 'not-started'
    $deadline = [Diagnostics.Stopwatch]::GetTimestamp() + [Diagnostics.Stopwatch]::Frequency * 2
    $result = Stop-KtOwnedFixtureRoot $fake $started $deadline
    Assert-KtEqual $result.exit_confirmed ($mode -in 'exited','normal')
    Assert-KtEqual $result.kill_failed ($mode -eq 'denied')
    Assert-KtEqual $result.wait_failed ($mode -eq 'wait-throws')
    Assert-KtEqual $fake.state.kills $(if ($mode -in 'exited','not-started') { 0 } else { 1 })
    Assert-KtEqual $fake.state.waits $(if ($started) { 1 } else { 0 })
    Assert-KtEqual $fake.state.disposed 1
    if ($started -and ($fake.state.wait_ms -lt 0 -or $fake.state.wait_ms -gt 2000)) { throw 'unbounded wait' }
    if (-not $started) { Assert-KtEqual $fake.state.observed 0 }
  }
  Write-Output 'worker-cleanup-fakes: PASS'
} catch {
  [Console]::Error.WriteLine(('worker-cleanup mode=fake stage=' + $stage + ' failed=True'))
  exit 1
}
