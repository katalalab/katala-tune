# Pure CPU regression payload: parse the probe and import ONLY named helpers.
# No CIM, Get-Process, sleeps, network, benchmark, or machine state changes.
param([string]$ProbePath = (Join-Path (Split-Path -Parent $PSScriptRoot) "probes/win_probe.ps1"))
$ErrorActionPreference = "Stop"
if ($PSVersionTable.PSVersion.Major -ne 5) { throw "Windows PowerShell 5.1 is required" }
$tokens = $null; $parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($ProbePath, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count) { throw ($parseErrors | Out-String) }
foreach ($name in @("RNum", "ReadCpuSample", "ProcessCpuPercent", "HostCpuSample")) {
  $defs = @($ast.FindAll({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name }, $false))
  if ($defs.Count -ne 1) { throw "Missing or ambiguous helper: $name" }
  . ([scriptblock]::Create($defs[0].Extent.Text))
}
function AssertEqual($actual, $expected, $label) {
  if ($null -eq $expected) { if ($null -ne $actual) { throw "$label expected null, got $actual" }; return }
  if ($null -eq $actual -or $actual -ne $expected) { throw "$label expected $expected, got $actual" }
}
function Sample($seconds, $ticks, $identity = 100, $processId = 7) {
  return [pscustomobject]@{ pid = $processId; start = $identity; cpu = $seconds; ticks = $ticks }
}
# One core runs for 3.5s: 1.5s nominal sleep plus 2s provider delay.
# The old global stopwatch denominator would report 233.3% of one core.
$a = Sample 10 1000
$b = Sample 13.5 4500
AssertEqual (ProcessCpuPercent $a $b 8 1000) 12.5 "CIM delay belongs to numerator and denominator"
AssertEqual (ProcessCpuPercent $a (Sample 11.5 2500) 8 1000) 12.5 "ordinary 1.5 second interval"
AssertEqual (ProcessCpuPercent $a (Sample 17 4500) 8 1000) 25 "two cores are not clamped to one"
AssertEqual (ProcessCpuPercent $a (Sample 10 4500) 8 1000) 0 "real idle CPU"
AssertEqual (ProcessCpuPercent $a (Sample 13.5 4500 101) 8 1000) $null "reused PID"
AssertEqual (ProcessCpuPercent $a (Sample 13.5 4500 100 8) 8 1000) $null "different PID"
AssertEqual (ProcessCpuPercent $null $b 8 1000) $null "missing first read"
AssertEqual (ProcessCpuPercent $a $null 8 1000) $null "missing second read"
AssertEqual (ProcessCpuPercent (Sample $null 1000) $b 8 1000) $null "unreadable first CPU"
AssertEqual (ProcessCpuPercent $a (Sample $null 4500) 8 1000) $null "unreadable second CPU"
AssertEqual (ProcessCpuPercent $a (Sample 13.5 4500 $null) 8 1000) $null "unreadable identity"
AssertEqual (ProcessCpuPercent $a (Sample 9 4500) 8 1000) $null "negative delta"
AssertEqual (ProcessCpuPercent $a (Sample 13.5 1000) 8 1000) $null "zero elapsed"
AssertEqual (ProcessCpuPercent $a (Sample 13.5 999) 8 1000) $null "negative elapsed"
AssertEqual (ProcessCpuPercent $a $b 0 1000) $null "unknown core count"
AssertEqual (ProcessCpuPercent $a $b 8 0) $null "invalid clock frequency"
AssertEqual (ProcessCpuPercent $a (Sample 13.5 $null) 8 1000) $null "unreadable clock"

foreach ($invalid in @([double]::NaN, [double]::PositiveInfinity, [double]::NegativeInfinity)) {
  AssertEqual (ProcessCpuPercent (Sample $invalid 1000) $b 8 1000) $null "non-finite first CPU"
  AssertEqual (ProcessCpuPercent $a (Sample $invalid 4500) 8 1000) $null "non-finite second CPU"
  AssertEqual (ProcessCpuPercent $a (Sample 13.5 $invalid) 8 1000) $null "non-finite timestamp"
  AssertEqual (ProcessCpuPercent $a $b $invalid 1000) $null "non-finite cores"
  AssertEqual (ProcessCpuPercent $a $b 8 $invalid) $null "non-finite frequency"
}
AssertEqual (ProcessCpuPercent (Sample (-[double]::MaxValue) 1000) (Sample ([double]::MaxValue) 4500) 8 1000) $null "delta overflow"
AssertEqual (ProcessCpuPercent (Sample 0 0) (Sample 1 ([double]::Epsilon)) 1 1) $null "division overflow"
AssertEqual (ProcessCpuPercent (Sample 0 (-[double]::MaxValue)) (Sample 1 ([double]::MaxValue)) 1 1) $null "elapsed overflow"

# Snapshot capture uses fake property data, never a real process.
$fake = [pscustomobject]@{ Id = 7; StartTime = [datetime]::UtcNow.AddMinutes(-2); TotalProcessorTime = [timespan]::FromSeconds(10) }
$capture = ReadCpuSample $fake
AssertEqual $capture.cpu 10 "captured cumulative CPU"
AssertEqual $capture.pid 7 "captured PID"
AssertEqual $capture.start $fake.StartTime.ToUniversalTime().Ticks "captured start identity"
if ($capture.ticks -le 0) { throw "missing monotonic timestamp" }
AssertEqual (ReadCpuSample ([pscustomobject]@{ Id = 7; StartTime = $null; TotalProcessorTime = [timespan]::Zero })) $null "missing start is unknown"
$denied = [pscustomobject]@{ Id = 7; StartTime = $fake.StartTime }
$denied | Add-Member ScriptProperty TotalProcessorTime { throw "fixture access denied" }
$partial = ReadCpuSample $denied
AssertEqual $partial.cpu $null "CPU getter failure is unknown"
AssertEqual $partial.ticks $null "CPU getter failure has no sample timestamp"
AssertEqual $partial.pid 7 "CPU getter failure preserves PID"
AssertEqual $partial.start $fake.StartTime.ToUniversalTime().Ticks "CPU getter failure preserves start guard"
AssertEqual $partial.start_time $fake.StartTime "CPU getter failure preserves output start time"
AssertEqual (ProcessCpuPercent $capture $partial 8 ([Diagnostics.Stopwatch]::Frequency)) $null "partial identity is not CPU availability"

foreach ($case in @(
  @{ a = 20; b = 40; value = 30; availability = "available"; samples = 2 },
  @{ a = $null; b = 40; value = 40; availability = "partial"; samples = 1 },
  @{ a = 40; b = $null; value = 40; availability = "partial"; samples = 1 },
  @{ a = $null; b = $null; value = $null; availability = "unavailable"; samples = 0 },
  @{ a = 0; b = 0; value = 0; availability = "available"; samples = 2 },
  @{ a = $null; b = 0; value = 0; availability = "partial"; samples = 1 }
)) {
  $hostSample = HostCpuSample $case.a $case.b
  AssertEqual $hostSample.value $case.value "host value"
  AssertEqual $hostSample.availability $case.availability "host availability"
  AssertEqual $hostSample.samples $case.samples "host sample count"
}
Write-Output "CPU sampling regression: PASS"
