# Test-only deadlines and retained process cleanup. Callers choose absolute deadlines once.
function Get-KtFixtureDeadline([long]$StartedTicks, [int]$Milliseconds) {
  return $StartedTicks + [long][Math]::Floor([Diagnostics.Stopwatch]::Frequency * ($Milliseconds / 1000.0))
}
function Get-KtFixtureRemainingMs([long]$DeadlineTicks, [int]$LimitMs = [int]::MaxValue, [long]$NowTicks = [Diagnostics.Stopwatch]::GetTimestamp()) {
  $left = [Math]::Floor(($DeadlineTicks - $NowTicks) * 1000.0 / [Diagnostics.Stopwatch]::Frequency)
  return [int][Math]::Max(0, [Math]::Min($LimitMs, $left))
}

# Only the Process returned by our own Start is passed here; never reacquire a root by PID.
function Stop-KtOwnedFixtureRoot($Process, [bool]$Started, [long]$DeadlineTicks) {
  $killFailed = $false; $waitFailed = $false; $observeFailed = $false; $confirmed = $false
  try {
    if ($Started) {
      try {
        if (-not $Process.HasExited) { try { $Process.Kill() } catch { $killFailed = $true } }
      } catch { $observeFailed = $true }
      try {
        $left = Get-KtFixtureRemainingMs $DeadlineTicks 2000
        $confirmed = $Process.WaitForExit($left) -and $Process.HasExited -and ([Diagnostics.Stopwatch]::GetTimestamp() -le $DeadlineTicks)
      } catch { $waitFailed = $true }
    }
    return [pscustomobject]@{ started = $Started; kill_failed = $killFailed; wait_failed = $waitFailed; observe_failed = $observeFailed; exit_confirmed = $confirmed }
  } finally { $Process.Dispose() }
}

# Adoption is read-only. Force a retained handle before comparing the original start identity.
# On mismatch or observation failure, dispose and refuse; this object is never a Kill target.
function Get-KtFixtureDescendant($Identity) {
  $process = $null; $accepted = $false
  try {
    if ([long]$Identity.pid -le 0 -or [long]$Identity.pid -gt [int]::MaxValue -or [long]$Identity.start_ticks -le 0) { return $null }
    $process = Get-Process -Id ([int]$Identity.pid) -ErrorAction Stop
    [void]$process.Handle
    if ($process.HasExited -or $process.StartTime.ToUniversalTime().Ticks -ne [long]$Identity.start_ticks) { return $null }
    $accepted = $true
    return $process
  } catch { return $null } finally {
    if (-not $accepted -and $null -ne $process) { $process.Dispose() }
  }
}

# A synchronization-only handle: the descendant must exit by itself, even on failure paths.
function Wait-KtFixtureDescendant($Process, [long]$DeadlineTicks) {
  $confirmed = $false; $waitFailed = $false
  try {
    try {
      $left = Get-KtFixtureRemainingMs $DeadlineTicks
      $confirmed = $Process.WaitForExit($left) -and $Process.HasExited -and ([Diagnostics.Stopwatch]::GetTimestamp() -le $DeadlineTicks)
    } catch { $waitFailed = $true }
    return [pscustomobject]@{ wait_failed = $waitFailed; exit_confirmed = $confirmed }
  } finally { $Process.Dispose() }
}
