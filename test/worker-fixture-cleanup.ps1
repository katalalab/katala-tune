# Test-only retained root cleanup. No PID lookup or descendant termination.
function Stop-KtOwnedFixtureRoot($Process, [bool]$Started, [long]$DeadlineTicks) {
  try {
    if ($Started -and -not $Process.HasExited) {
      $Process.Kill()
      if (-not $Process.WaitForExit(2000)) { throw 'owned fixture exit unconfirmed' }
    }
    return [pscustomobject]@{ started = $Started; kill_failed = $false; wait_failed = $false; observe_failed = $false; exit_confirmed = $Started }
  } finally { $Process.Dispose() }
}
