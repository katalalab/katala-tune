$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'worker-fixture-cleanup.ps1')
$mode = 'setup'; $stage = 'source'; $killFailed = $false; $exitConfirmed = $false; $expectedReadFailure = $false
try {
$source = [IO.File]::ReadAllText((Join-Path $PSScriptRoot '../probes/win_probe.ps1'))
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseInput($source, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw 'probe syntax failure' }
$function = $ast.Find({param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq 'Read-KtProcessOutput'}, $true)
if (-not $function) { throw 'bounded worker reader is missing' }
. ([ScriptBlock]::Create($function.Extent.Text))
$dir = Join-Path ([IO.Path]::GetTempPath()) ('kt-worker-pipes-' + [Guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($dir)
$fixture = Join-Path $dir 'child.ps1'
$childSource = @'
param([string]$Mode, [long]$AdoptDeadline, [long]$ReaderDeadline, [long]$DescendantDeadline)
# RELEASE_DEFINITION
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
switch ($Mode) {
  'utf16' { [Console]::OutputEncoding = [Text.Encoding]::Unicode; [Console]::Out.Write([char]0x65e5); break }
  'held-eof' {
    $argsValue = '-NoProfile -NonInteractive -File "' + $PSCommandPath + '" -Mode hold -DescendantDeadline ' + $DescendantDeadline
    $desc = Start-Process -FilePath ([Diagnostics.Process]::GetCurrentProcess().MainModule.FileName) -ArgumentList $argsValue -NoNewWindow -PassThru
    try {
      $identity = @{ pid = $desc.Id; start_ticks = $desc.StartTime.ToUniversalTime().Ticks } | ConvertTo-Json -Compress
      [IO.File]::WriteAllText((Join-Path $PSScriptRoot 'held.identity'), $identity)
      $release = Join-Path $PSScriptRoot 'held.release'
      while (-not (Test-Path -LiteralPath $release) -and [Diagnostics.Stopwatch]::GetTimestamp() -lt $AdoptDeadline) { [Threading.Thread]::Sleep(10) }
      if (-not (Test-KtFixtureRelease (Test-Path -LiteralPath $release) $AdoptDeadline)) { exit 4 }
      # Let the reader close stdin before the root exits; do not accept a broken stdin as held EOF.
      $inputPipe = [Console]::OpenStandardInput()
      $inputByte = New-Object byte[] 1
      $inputDone = $inputPipe.ReadAsync($inputByte, 0, 1)
      $left = [Math]::Max(0, [Math]::Floor(($ReaderDeadline - [Diagnostics.Stopwatch]::GetTimestamp()) * 1000.0 / [Diagnostics.Stopwatch]::Frequency))
      if (-not $inputDone.Wait([int]$left) -or $inputDone.Result -ne 0) { exit 5 }
      [Console]::Out.Write('prefix')
    } finally { $desc.Dispose() }
    break
  }
  'hold' {
    # Absolute QPC deadline from before root Start: no ACK or wait can extend it.
    while ([Diagnostics.Stopwatch]::GetTimestamp() -lt $DescendantDeadline) {
      $left = [Math]::Ceiling(($DescendantDeadline - [Diagnostics.Stopwatch]::GetTimestamp()) * 1000.0 / [Diagnostics.Stopwatch]::Frequency)
      if ($left -gt 0) { [Threading.Thread]::Sleep([int][Math]::Min(20, $left)) }
    }
    break
  }
  'small' { [Console]::Out.Write('ok'); break }
  'out-exact' { [Console]::Out.Write(('x' * 8192)); break }
  'out-over' { [Console]::Out.Write(('x' * 8193)); break }
  'err-exact' { [Console]::Error.Write(('z' * 4096)); break }
  'err-over' { [Console]::Error.Write(('z' * 4097)); break }
  'dual' { [Console]::Out.Write(('x' * 8192)); [Console]::Error.Write(('z' * 4096)); break }
  'exit' { [Console]::Out.Write('partial'); exit 3 }
  'timeout' { [Console]::Out.Write('prefix'); [Threading.Thread]::Sleep(5000); break }
}
'@
$releaseDefinition = 'function Test-KtFixtureRelease {' + [Environment]::NewLine + (Get-Command Test-KtFixtureRelease).Definition + [Environment]::NewLine + '}'
$childSource = $childSource.Replace('# RELEASE_DEFINITION', $releaseDefinition)
[IO.File]::WriteAllText($fixture, $childSource, [Text.UTF8Encoding]::new($true))
$passed = 0
$overallDeadline = Get-KtFixtureDeadline ([Diagnostics.Stopwatch]::GetTimestamp()) 25000
try {
 foreach ($mode in 'small','out-exact','out-over','err-exact','err-over','dual','exit','timeout','utf16','held-eof') {
  $stage = 'budget'; $killFailed = $false; $exitConfirmed = $false; $expectedReadFailure = $false
  $needed = $(if ($mode -eq 'held-eof') { 8000 } else { 4500 })
  if ((Get-KtFixtureRemainingMs $overallDeadline) -lt $needed) { throw 'fixture failed' }
  $caseTicks = [Diagnostics.Stopwatch]::GetTimestamp()
  $clock = [Diagnostics.Stopwatch]::StartNew()
  $adoptDeadline = Get-KtFixtureDeadline $caseTicks 1500
  $readerDeadline = Get-KtFixtureDeadline $caseTicks 2500
  $descendantDeadline = Get-KtFixtureDeadline $caseTicks 6500
  $cleanupDeadline = [Math]::Min($overallDeadline, (Get-KtFixtureDeadline $caseTicks 8000))
  $process = [Diagnostics.Process]::new()
  $process.StartInfo = [Diagnostics.ProcessStartInfo]::new()
  $process.StartInfo.FileName = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
  $process.StartInfo.Arguments = '-NoProfile -NonInteractive -File "' + $fixture + '" -Mode ' + $mode + ' -AdoptDeadline ' + $adoptDeadline + ' -ReaderDeadline ' + $readerDeadline + ' -DescendantDeadline ' + $descendantDeadline
  $process.StartInfo.UseShellExecute = $false
  $process.StartInfo.CreateNoWindow = $true
  $process.StartInfo.RedirectStandardInput = $true
  $process.StartInfo.RedirectStandardOutput = $true
  $process.StartInfo.RedirectStandardError = $true
  $failed = $false; $started = $false; $descendant = $null
  try {
   $stage = 'start'
   if (-not $process.Start()) { throw 'fixture failed' }
   $started = $true
   if ($mode -eq 'held-eof') {
    $stage = 'adopt'
    $marker = Join-Path $dir 'held.identity'
    while ($null -eq $descendant -and (Get-KtFixtureRemainingMs $adoptDeadline) -gt 0) {
      $identity = $null
      if (Test-Path -LiteralPath $marker) { try { $identity = [IO.File]::ReadAllText($marker) | ConvertFrom-Json } catch { } }
      if ($null -ne $identity) {
        $descendant = Get-KtFixtureDescendant $identity
        if ($null -eq $descendant) { throw 'fixture failed' }
      } else { [Threading.Thread]::Sleep([Math]::Min(10, (Get-KtFixtureRemainingMs $adoptDeadline))) }
    }
    if ($null -eq $descendant -or [Diagnostics.Stopwatch]::GetTimestamp() -gt $adoptDeadline) { throw 'fixture failed' }
    [IO.File]::WriteAllText((Join-Path $dir 'held.release'), 'ready')
    if (-not (Test-KtFixtureRelease $true $adoptDeadline)) { throw 'fixture failed' }
   }
   $stage = 'reader'
   try { $result = Read-KtProcessOutput $process $clock 2500 '' $(if ($mode -eq 'utf16') { [Text.Encoding]::Unicode } else { [Text.Encoding]::UTF8 }) 8192 4096 } catch {
    $failed = $true
    $expected = $(if ($mode -eq 'out-over') { 'worker stdout exceeded limit' } elseif ($mode -eq 'err-over') { 'worker stderr exceeded limit' } else { 'worker output timeout or missing EOF' })
    $expectedReadFailure = $_.Exception.Message -ceq $expected
   }
   if ($mode -in 'out-over','err-over','timeout','held-eof') {
    if (-not $failed -or -not $expectedReadFailure) { throw 'fixture failed' }
   } else {
    if ($failed) { throw ('unexpected read failure: ' + $mode) }
    if ($mode -eq 'utf16' -and $result.out -ne [string][char]0x65e5) { throw 'UTF16 output lost' }
    if ($mode -eq 'small' -and $result.out -ne 'ok') { throw 'small output lost' }
    if ($mode -in 'out-exact','dual' -and $result.out.Length -ne 8192) { throw 'stdout boundary lost' }
    if ($mode -in 'err-exact','dual' -and $result.err.Length -ne 4096) { throw 'stderr boundary lost' }
    if ($mode -eq 'exit' -and ($result.code -ne 3 -or $result.out -ne 'partial')) { throw 'exit status lost' }
   }
   if ($clock.ElapsedMilliseconds -gt 4000) { throw 'reader exceeded finite deadline' }
   if ($mode -eq 'held-eof' -and (-not $process.HasExited -or $process.ExitCode -ne 0)) { throw 'held-EOF root did not exit successfully' }
   $passed++
  } finally {
   $rootOk = $false; $descendantOk = $true
   try {
    $cleanup = Stop-KtOwnedFixtureRoot $process $started $cleanupDeadline
    $killFailed = $cleanup.kill_failed; $exitConfirmed = $cleanup.exit_confirmed
    $rootOk = -not $started -or $exitConfirmed
    if ($killFailed) { Write-Output ('worker-pipes mode=' + $mode + ' stage=root-cleanup kill_failed=True exit_confirmed=' + $exitConfirmed) }
   } catch { $rootOk = $false }
   if ($null -ne $descendant) {
    try { $childCleanup = Wait-KtFixtureDescendant $descendant $cleanupDeadline; $descendantOk = $childCleanup.exit_confirmed } catch { $descendantOk = $false }
   }
   if (-not $rootOk) { $stage = 'root-cleanup'; $exitConfirmed = $false; throw 'fixture failed' }
   if (-not $descendantOk) { $stage = 'descendant-cleanup'; $exitConfirmed = $false; throw 'fixture failed' }
  }
 }
} finally {
 try { Remove-Item -LiteralPath $dir -Recurse -Force } catch { $stage = 'temp-cleanup'; throw 'fixture failed' } }
Write-Output ('worker-pipes: ' + $passed + ' passed')
} catch {
 [Console]::Error.WriteLine(('worker-pipes mode=' + $mode + ' stage=' + $stage + ' kill_failed=' + $killFailed + ' exit_confirmed=' + $exitConfirmed + ' expected_read_failure=' + $expectedReadFailure))
 exit 1
}
