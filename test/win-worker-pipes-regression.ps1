$ErrorActionPreference = 'Stop'
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
param([string]$Mode)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
switch ($Mode) {
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
[IO.File]::WriteAllText($fixture, $childSource, [Text.UTF8Encoding]::new($true))
$passed = 0
try {
 foreach ($mode in 'small','out-exact','out-over','err-exact','err-over','dual','exit','timeout') {
  $process = [Diagnostics.Process]::new()
  $process.StartInfo = [Diagnostics.ProcessStartInfo]::new()
  $process.StartInfo.FileName = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
  $process.StartInfo.Arguments = '-NoProfile -NonInteractive -File "' + $fixture + '" -Mode ' + $mode
  $process.StartInfo.UseShellExecute = $false
  $process.StartInfo.CreateNoWindow = $true
  $process.StartInfo.RedirectStandardInput = $true
  $process.StartInfo.RedirectStandardOutput = $true
  $process.StartInfo.RedirectStandardError = $true
  $clock = [Diagnostics.Stopwatch]::StartNew()
  $failed = $false
  try {
   [void]$process.Start()
   try { $result = Read-KtProcessOutput $process $clock 2500 '' ([Text.Encoding]::UTF8) 8192 4096 } catch { $failed = $true }
   if ($mode -in 'out-over','err-over','timeout') {
    if (-not $failed) { throw ('accepted incomplete/overflow output: ' + $mode) }
   } else {
    if ($failed) { throw ('unexpected read failure: ' + $mode) }
    if ($mode -eq 'small' -and $result.out -ne 'ok') { throw 'small output lost' }
    if ($mode -in 'out-exact','dual' -and $result.out.Length -ne 8192) { throw 'stdout boundary lost' }
    if ($mode -in 'err-exact','dual' -and $result.err.Length -ne 4096) { throw 'stderr boundary lost' }
    if ($mode -eq 'exit' -and ($result.code -ne 3 -or $result.out -ne 'partial')) { throw 'exit status lost' }
   }
   if ($clock.ElapsedMilliseconds -gt 4000) { throw 'reader exceeded finite deadline' }
   $passed++
  } finally {
   if (-not $process.HasExited) { $process.Kill(); if (-not $process.WaitForExit(2000)) { throw 'owned fixture exit unconfirmed' } }
   $process.Dispose()
  }
 }
} finally { Remove-Item -LiteralPath $dir -Recurse -Force }
Write-Output ('worker-pipes: ' + $passed + ' passed')
