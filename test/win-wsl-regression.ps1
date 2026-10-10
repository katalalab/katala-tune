# Windows PowerShell 5.1 regression tests for the WSL observation in probes/win_probe.ps1.
# wsl.exe is never started: Invoke-KtWsl (the only function that launches it) is replaced by a fake that records every call.
# Checks that a stopped distro is never entered (no -d, no --exec) and is reported as stopped_unobserved.
param([string]$ProbeText = $null)
$ErrorActionPreference = 'Stop'
if ($PSVersionTable.PSVersion.Major -ne 5) { throw "expected Windows PowerShell 5.1, got $($PSVersionTable.PSVersion)" }
if (-not $ProbeText) {
  $Root = Split-Path -Parent $PSScriptRoot
  $ProbePath = Join-Path $Root 'probes\win_probe.ps1'
  $ProbeBytes = [IO.File]::ReadAllBytes($ProbePath)
  if ($ProbeBytes.Length -lt 4 -or $ProbeBytes[0] -ne 0xEF -or $ProbeBytes[1] -ne 0xBB -or $ProbeBytes[2] -ne 0xBF) { throw 'win_probe.ps1 must have a UTF-8 BOM' }
  foreach ($b in $ProbeBytes[3..($ProbeBytes.Length - 1)]) { if ($b -gt 0x7F) { throw 'win_probe.ps1 body must be ASCII' } }
  $ProbeText = [IO.File]::ReadAllText($ProbePath, ([Text.UTF8Encoding]::new($true)))
}
function Assert-KtEqual($Actual, $Expected, [string]$Message) {
  if ([string]$Actual -ne [string]$Expected) { throw "$Message (actual=$Actual expected=$Expected)" }
}
function Assert-KtTrue([bool]$Value, [string]$Message) { if (-not $Value) { throw $Message } }

$a = $ProbeText.IndexOf('# --- WSL observation (read-only) begin ---')
$b = $ProbeText.IndexOf('# --- WSL observation (read-only) end ---')
Assert-KtTrue ($a -ge 0 -and $b -gt $a) 'WSL observation markers are missing'
$region = $ProbeText.Substring($a, $b - $a)

# The region must not contain anything that changes WSL (comments are ignored).
$code = (($region -split "`n") | Where-Object { $_.TrimStart() -notmatch '^#' }) -join "`n"
foreach ($bad in @('--shutdown', '--terminate', '--unregister', '--import', '--export', '--install', '--update', '--set-', '--mount', '--unmount', 'Stop-Service', 'Start-Service', 'Restart-Service')) {
  Assert-KtTrue ($code.IndexOf($bad, [StringComparison]::OrdinalIgnoreCase) -lt 0) "region contains $bad"
}
# wsl.exe is launched only by Invoke-KtWsl, and only -l / -d <running distro> --exec are ever passed.
Assert-KtEqual ([regex]::Matches($code, 'ProcessStartInfo').Count) 1 'only one place may build a process'
Assert-KtEqual ([regex]::Matches($code, '--exec').Count) 1 'only one --exec call is expected'

function R1($x, $d = 1) { if ($null -eq $x) { return $null } return [math]::Round([double]$x, $d) }
Invoke-Expression $region

$script:Calls = New-Object System.Collections.ArrayList
$script:Installed = $true
$script:Script = @{}
function Test-KtWslInstalled { return $script:Installed }
function Invoke-KtWsl([string[]]$WslArgs, [string]$InputText = $null, [int]$TimeoutMs = 8000, [bool]$Utf16 = $true) {
  [void]$script:Calls.Add(($WslArgs -join ' '))
  $key = $WslArgs -join ' '
  if ($script:Script.ContainsKey($key)) { return $script:Script[$key] }
  return [pscustomobject]@{ ok = $false; code = 1; out = ''; err = 'exit 1' }
}
function New-KtRes([string]$Out, $Err = $null) { return [pscustomobject]@{ ok = ($null -eq $Err); code = $(if ($Err) { 1 } else { 0 }); out = $Out; err = $Err } }
function Get-KtEnteredNames { return @($script:Calls | Where-Object { $_ -like '-d *' } | ForEach-Object { ($_ -split ' ')[1] }) }

$inner = "@@mem`nMemTotal:       32870000 kB`nMemAvailable:   16000000 kB`nSwapTotal:       8388608 kB`nSwapFree:       8388000 kB`n@@psi`nsome avg10=1.25 avg60=0.50 avg300=0.10 total=1234`nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0`n@@load`n0.52 0.40 0.30 1/200 999`n@@ps`n  100  2048000  3.5 python3`n  101  1024000  1.0 node`n@@df`n/dev/sdc 1055762868 123456789 875000000 13% /`n@@end`n"

# 1. wsl.exe not installed: nothing is run
$script:Installed = $false
$script:Calls.Clear()
$r = Get-KtWslObservation
Assert-KtEqual $r.installed $false 'not installed'
Assert-KtEqual $r.distros.Count 0 'not installed: no distros'
Assert-KtEqual $script:Calls.Count 0 'not installed: wsl.exe must not be called'
$script:Installed = $true

# 2. running + stopped + internal distro: only the running one is entered
$script:Calls.Clear()
$script:Script = @{
  '-l -q' = (New-KtRes "Ubuntu`r`nOld`r`ndocker-desktop`r`n")
  '-l -v' = (New-KtRes "  NAME              STATE           VERSION`r`n* Ubuntu            Running         2`r`n  Old               Stopped         2`r`n  docker-desktop    Running         2`r`n")
  '-l --running -q' = (New-KtRes "Ubuntu`r`ndocker-desktop`r`n")
  '-d Ubuntu --exec /bin/sh -s' = (New-KtRes $inner)
}
$r = Get-KtWslObservation
Assert-KtEqual $r.listed $true 'listed'
Assert-KtEqual $r.distros.Count 3 'three distros'
$entered = Get-KtEnteredNames
Assert-KtEqual ($entered -join ',') 'Ubuntu' 'only the running distro may be entered'
Assert-KtTrue ($script:Calls -notcontains '-d Old --exec /bin/sh -s') 'the stopped distro must not be entered'
$u = $r.distros | Where-Object { $_.name -eq 'Ubuntu' }
Assert-KtEqual $u.state 'running' 'Ubuntu state'
Assert-KtEqual $u.observed $true 'Ubuntu observed'
Assert-KtEqual $u.default $true 'Ubuntu default'
Assert-KtEqual $u.version 2 'Ubuntu version'
Assert-KtEqual $u.inside.mem_total_mb 32100 'mem_total_mb'
Assert-KtEqual $u.inside.mem_available_mb 15625 'mem_available_mb'
Assert-KtEqual $u.inside.psi_some_avg10 1.25 'psi some'
Assert-KtEqual $u.inside.load1 0.52 'load1'
Assert-KtEqual $u.inside.top.Count 2 'top processes'
Assert-KtEqual $u.inside.top[0].name 'python3' 'top process name'
Assert-KtEqual $u.inside.top[0].mem_mb 2000 'top process mem'
Assert-KtEqual $u.inside.disk[0].mount '/' 'disk mount'
$o = $r.distros | Where-Object { $_.name -eq 'Old' }
Assert-KtEqual $o.state 'stopped_unobserved' 'Old state'
Assert-KtEqual $o.observed $false 'Old observed'
Assert-KtTrue ($null -eq $o.inside) 'Old must have no inside data'
$d = $r.distros | Where-Object { $_.name -eq 'docker-desktop' }
Assert-KtEqual $d.reason 'internal-distro-skipped' 'internal distro is skipped'
Assert-KtEqual $d.observed $false 'internal distro observed'
$json = $r | ConvertTo-Json -Depth 8 -Compress
Assert-KtTrue ($json.Length -gt 100) 'observation must serialize'

# 3. the running list cannot be read: nothing is entered, every state is unknown
$script:Calls.Clear()
$script:Script['-l --running -q'] = (New-KtRes '' 'timeout')
$r = Get-KtWslObservation
Assert-KtEqual (Get-KtEnteredNames).Count 0 'running list failed: nothing may be entered'
foreach ($x in $r.distros) { Assert-KtEqual $x.state 'unknown' "state of $($x.name)"; Assert-KtEqual $x.reason 'running-list-failed' "reason of $($x.name)" }

# 4. nothing is running (localized message, non-zero exit): every distro is stopped_unobserved
$script:Calls.Clear()
$script:Script['-l --running -q'] = (New-KtRes "There are no running distributions.`r`n" 'exit 1')
$r = Get-KtWslObservation
Assert-KtEqual (Get-KtEnteredNames).Count 0 'none running: nothing may be entered'
foreach ($x in $r.distros) { Assert-KtEqual $x.state 'stopped_unobserved' "state of $($x.name)" }

# 5. a running distro whose command times out is reported with a reason, not as empty or healthy
$script:Calls.Clear()
$script:Script['-l --running -q'] = (New-KtRes "Ubuntu`r`n")
$script:Script['-d Ubuntu --exec /bin/sh -s'] = (New-KtRes '' 'timeout')
$r = Get-KtWslObservation
$u = $r.distros | Where-Object { $_.name -eq 'Ubuntu' }
Assert-KtEqual $u.state 'running' 'timeout: state'
Assert-KtEqual $u.observed $false 'timeout: observed'
Assert-KtEqual $u.reason 'timeout' 'timeout: reason'
Assert-KtTrue ($null -eq $u.inside) 'timeout: no inside data'
Assert-KtEqual (Get-KtEnteredNames).Count 1 'timeout: one entry only'

# 6. no registered distro (localized message): listed, no distros, nothing entered
$script:Calls.Clear()
$script:Script = @{ '-l -q' = (New-KtRes "Windows Subsystem for Linux has no installed distributions.`r`n" 'exit 1') }
$r = Get-KtWslObservation
Assert-KtEqual $r.listed $true 'no distros: listed'
Assert-KtEqual $r.distros.Count 0 'no distros: empty'
Assert-KtEqual $r.ignored_lines 1 'no distros: message is counted, not used as a name'
Assert-KtEqual (Get-KtEnteredNames).Count 0 'no distros: nothing may be entered'

# 7. the list itself fails: reported as an error, nothing is entered
$script:Calls.Clear()
$script:Script = @{ '-l -q' = (New-KtRes '' 'timeout') }
$r = Get-KtWslObservation
Assert-KtEqual $r.listed $false 'list failed: not listed'
Assert-KtEqual $r.list_error 'timeout' 'list failed: reason'
Assert-KtEqual $script:Calls.Count 1 'list failed: no further calls'

Write-Output 'win-wsl-regression: ok'
