// 実行できる最適化の許可リスト。ここに無い操作は実行しない。
// 呼び出し側（main.js）が確認ダイアログで操作者の承認を取り、その直前に台帳と保護リストを読み直してから execute する。
//
// プロセス終了は NeonMonitor 1.1.x のレビューで見つかった失敗の型を避ける（docs/safety.md）:
//   - 古い判断のまま終了しない: 終了の直前に、同じ PID が同じ名前・同じ起動時刻のままかを確かめる（PID の再利用対策）
//   - 回復していれば終了しない: 直前に負荷を測り直し、下がっていれば中止する
//   - 保護リストを読めなければ終了しない: 呼び出し側が毎回読み直し、失敗したら plan まで進まない
//   - 終了の成功を断定しない: 5秒以内に消えたことを確認できなければ「終了未確認」として返す
'use strict';
const { execOn, actionExchange: defaultActionExchange, actionHelper } = require('./collect');
const ledger = require('./ledger');
const { NO_KILL } = require('./rules');

const GUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const PROC_NAME = /^[\w .()+-]{1,80}$/;
const MAC_LSTART = /^[A-Za-z]{3} [A-Za-z]{3} [ \d]\d \d{2}:\d{2}:\d{2} \d{4}$/;
const WIN_START = /^\d{17}$/;
const NVIDIA_UUID = /^GPU-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const GPU_ACTIVE_UTIL = 5;
const ACTION_INPUT_MAX = 65536;
const LOWER_HEX_40 = /^[0-9a-f]{40}$/;
const LOWER_HEX_64 = /^[0-9a-f]{64}$/;
// .NET Framework 4.5+ の BaseStream.ReadAsync で両方の pipe を小さく並行 drain する。
// 読み取り失敗・上限超過・EOF/終了未確認は、変更前なら拒否、変更後なら結果不明にする。
const GPU_SMI_HELPER = 'function Stop-KatalaGpuSmiProcess { param($Process); if ($script:KatalaGpuSmiCleanupAttempted) { return $false }; $script:KatalaGpuSmiCleanupAttempted = $true; try { if ($Process.HasExited) { return $true }; $left = [Math]::Max(0, 9000 - [int]$script:KatalaGpuGuardClock.ElapsedMilliseconds); if ($left -le 0) { $script:KatalaGpuSmiTransportFailed = $true; return $false }; $Process.Kill(); $killAck = $Process.WaitForExit([Math]::Min(1000, $left)); if (-not $killAck -or -not $Process.HasExited) { $script:KatalaGpuSmiTransportFailed = $true; return $false }; return $true } catch { $script:KatalaGpuSmiTransportFailed = $true; return $false } }; function Fail-KatalaGpuSmi { param([bool]$TimedOut, [string]$Reason, [bool]$Permanent); $script:KatalaGpuSmiTimedOut = $TimedOut; $script:KatalaGpuSmiFailed = $true; $script:KatalaGpuSmiFailureReason = $Reason; if ($Permanent) { $script:KatalaGpuSmiTransportFailed = $true }; if ($script:KatalaGpuMutationStarted) { Write-Output "GPU power outcome unverified"; exit 7 }; return $null }; function Invoke-KatalaGpuSmi { param([string]$Arguments); $script:KatalaGpuSmiTimedOut = $false; $script:KatalaGpuSmiExitCode = $null; $script:KatalaGpuSmiFailed = $false; $script:KatalaGpuSmiFailureReason = "GPU activity unavailable; operation refused"; if ($script:KatalaGpuSmiTransportFailed) { return (Fail-KatalaGpuSmi $false "GPU SMI process lifetime unverified; operation refused" $true) }; $script:KatalaGpuSmiCleanupAttempted = $false; Test-KatalaGpuDeadline; if ($script:KatalaGpuSmiFunction) { $argv = @($Arguments.Split(" ")); $rows = @(& nvidia-smi.exe @argv); $script:KatalaGpuSmiExitCode = $LASTEXITCODE; if ($script:KatalaGpuSmiExitCode -ne 0) { return (Fail-KatalaGpuSmi $false "GPU SMI exited unsuccessfully; operation refused" $false) }; return $rows }; $p = $null; try { $si = New-Object Diagnostics.ProcessStartInfo; $si.FileName = $script:KatalaGpuSmiSource; $si.Arguments = $Arguments; $si.UseShellExecute = $false; $si.CreateNoWindow = $true; $si.RedirectStandardOutput = $true; $si.RedirectStandardError = $true; $p = New-Object Diagnostics.Process; $p.StartInfo = $si; $smiDeadline = [Math]::Min(9000, [int]$script:KatalaGpuGuardClock.ElapsedMilliseconds + 4000); [void]$p.Start(); $stdout = New-Object IO.MemoryStream(65536); $stderrCount = 0; $outBuffer = New-Object byte[] 4096; $errBuffer = New-Object byte[] 4096; $outRead = $p.StandardOutput.BaseStream.ReadAsync($outBuffer, 0, $outBuffer.Length); $errRead = $p.StandardError.BaseStream.ReadAsync($errBuffer, 0, $errBuffer.Length); $outDone = $false; $errDone = $false; $failed = $false; $timedOut = $false; $reason = "GPU SMI output exceeded limit; operation refused"; while (-not ($outDone -and $errDone)) { $left = [Math]::Max(0, $smiDeadline - [int]$script:KatalaGpuGuardClock.ElapsedMilliseconds); if ($left -le 0) { $timedOut = $true; $failed = $true; $reason = "GPU SMI timed out or held a pipe; operation refused"; break }; $pending = New-Object Collections.Generic.List[Threading.Tasks.Task]; if (-not $outDone) { $pending.Add($outRead) }; if (-not $errDone) { $pending.Add($errRead) }; if ([Threading.Tasks.Task]::WaitAny([Threading.Tasks.Task[]]$pending.ToArray(), $left) -lt 0) { $timedOut = $true; $failed = $true; $reason = "GPU SMI timed out or held a pipe; operation refused"; break }; if (-not $outDone -and $outRead.IsCompleted) { $outCount = $outRead.Result; if ($outCount -le 0) { $outDone = $true } elseif ($stdout.Length + $outCount -gt 65536) { $failed = $true; $reason = "GPU SMI stdout exceeded limit; operation refused"; break } else { $stdout.Write($outBuffer, 0, $outCount); $outRead = $p.StandardOutput.BaseStream.ReadAsync($outBuffer, 0, $outBuffer.Length) } }; if (-not $errDone -and $errRead.IsCompleted) { $errCount = $errRead.Result; if ($errCount -le 0) { $errDone = $true } elseif ($stderrCount + $errCount -gt 65536) { $failed = $true; $reason = "GPU SMI stderr exceeded limit; operation refused"; break } else { $stderrCount += $errCount; $errRead = $p.StandardError.BaseStream.ReadAsync($errBuffer, 0, $errBuffer.Length) } } }; if ($failed) { $script:KatalaGpuSmiTransportFailed = $true; [void](Stop-KatalaGpuSmiProcess $p); return (Fail-KatalaGpuSmi $timedOut $reason $true) }; $left = [Math]::Max(0, $smiDeadline - [int]$script:KatalaGpuGuardClock.ElapsedMilliseconds); if ($left -le 0 -or -not $p.WaitForExit($left)) { $script:KatalaGpuSmiTransportFailed = $true; [void](Stop-KatalaGpuSmiProcess $p); return (Fail-KatalaGpuSmi $true "GPU SMI timed out or held a pipe; operation refused" $true) }; $script:KatalaGpuSmiExitCode = $p.ExitCode; if ($script:KatalaGpuSmiExitCode -ne 0) { return (Fail-KatalaGpuSmi $false "GPU SMI exited unsuccessfully; operation refused" $false) }; return @([Text.Encoding]::UTF8.GetString($stdout.ToArray()) -split "`r?`n" | Where-Object { $_.Trim().Length -gt 0 }) } catch { if ($p) { $script:KatalaGpuSmiTransportFailed = $true; [void](Stop-KatalaGpuSmiProcess $p); return (Fail-KatalaGpuSmi $false "GPU SMI transport failed; operation refused" $true) }; return (Fail-KatalaGpuSmi $false "GPU SMI transport failed; operation refused" $true) } finally { if ($p) { try { if (-not $p.HasExited -and -not $script:KatalaGpuSmiCleanupAttempted) { $cleanupAck = Stop-KatalaGpuSmiProcess $p; if (-not $cleanupAck) { $script:KatalaGpuSmiTransportFailed = $true } } } catch { $script:KatalaGpuSmiTransportFailed = $true }; $p.Dispose() } } }';
// 固定の受信器だけを argv に載せ、本文・引数は stdin の bytes に閉じる。
const ACTION_RECEIVER = [
  '$s=[Console]::OpenStandardInput()',
  '$w=[Diagnostics.Stopwatch]::StartNew()',
  '$b=New-Object byte[] 1',
  '$h=New-Object Collections.Generic.List[byte]',
  '$headerDone=$false',
  'while($h.Count -lt 256){$remaining=5000-[int]$w.ElapsedMilliseconds;if($remaining -le 0){exit 9};$t=$s.ReadAsync($b,0,1);if(-not $t.Wait($remaining)){exit 9};if($t.Result -ne 1){exit 9};if($b[0]-eq 10){$headerDone=$true;break};if($b[0]-lt 32 -or $b[0]-gt 126){exit 9};$h.Add($b[0])}',
  'if(-not $headerDone){exit 9}',
  'try{$m=[Text.Encoding]::ASCII.GetString($h.ToArray()) -split " "',
  'if($m.Count-ne 4 -or $m[0]-ne "KT1" -or $m[1]-notmatch "^[0-9]+$" -or $m[2]-notmatch "^[0-9a-f]{64}$" -or $m[3]-notmatch "^[0-9]+$"){exit 9}',
  '$n=[int]$m[1];$e=[int64]$m[3]',
  'if($n -lt 1 -or $n -gt 65536 -or [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() -gt $e){exit 9}',
  '$d=New-Object byte[] $n;$o=0',
  'while($o-lt$n){$remaining=5000-[int]$w.ElapsedMilliseconds;if($remaining -le 0){exit 9};$t=$s.ReadAsync($d,$o,$n-$o);if(-not $t.Wait($remaining)){exit 9};if($t.Result-le 0){exit 9};$o+=$t.Result}',
  '$x=New-Object byte[] 1;$remaining=5000-[int]$w.ElapsedMilliseconds;if($remaining -le 0){exit 9};$t=$s.ReadAsync($x,0,1);if(-not $t.Wait($remaining)){exit 9};if($t.Result-ne 0){exit 9}',
  '$q=[BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash($d)).Replace("-","").ToLowerInvariant()',
  'if($q-ne$m[2] -or [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() -gt $e){exit 9}',
  '$u=New-Object Text.UTF8Encoding($false,$true);$script:KatalaActionExpiryMs=$e',
  'function Test-KatalaActionExpiry { if([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() -gt $script:KatalaActionExpiryMs){if($script:KatalaGpuMutationStarted){Write-Output "GPU power outcome unverified";exit 7};Write-Output "action request expired; operation refused";exit 9} }',
  'Test-KatalaActionExpiry;& ([ScriptBlock]::Create($u.GetString($d)))}catch{if($script:KatalaGpuMutationStarted){Write-Output "GPU power outcome unverified";exit 7};exit 9}',
].join(';');
const ACTION_RECEIVER_COMMAND = `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(ACTION_RECEIVER, 'utf16le').toString('base64')}`;

function actionInput(script, now = Date.now()) {
  if (!Number.isSafeInteger(now)) throw new Error('action stdin 時刻が不正');
  const body = Buffer.from(script, 'utf8');
  if (!body.length || body.length > ACTION_INPUT_MAX) throw new Error('action stdin 本文の長さが不正');
  const hash = require('node:crypto').createHash('sha256').update(body).digest('hex');
  return Buffer.concat([Buffer.from(`KT1 ${body.length} ${hash} ${now + 25000}\n`, 'ascii'), body]);
}

function actionHelperFingerprint(node) {
  const helper = node.action_helper;
  try {
    actionHelper(node);
    return [helper.schema, helper.source_sha, helper.tune_sha256, helper.manifest_sha256, helper.run_id, helper.attempt].join('\u0000');
  } catch {
    return null;
  }
}

// NVIDIA の実行直前ガード。compute-apps は PID と使用メモリだけを返すので、
// プロセス名・引数は読まない。取得不能も「未使用」とは扱わない。
// Windows PowerShell 5.1 と Git Bash 経由の ssh のため、シングルクォートを入れない。
function gpuActiveGuardPs(pid = null) {
  const related = pid == null ? [] : [
    '$tree = @(Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId,CreationDate -OperationTimeoutSec 4 -ErrorAction SilentlyContinue); Test-KatalaGpuDeadline',
    'if ($tree.Count -eq 0 -or $tree.Count -gt 4096) { Write-Output "GPU process tree unreadable; operation refused"; exit 9 }',
    '$parents = @{}; foreach ($r in $tree) { if ($null -ne $r.ProcessId -and $null -ne $r.ParentProcessId) { $parents[[int]$r.ProcessId] = @{ pid = [int]$r.ParentProcessId; born = $r.CreationDate } } }',
    `$targetAncestors = @{}; $walk = ${pid}; for ($i = 0; $i -lt 64 -and $walk -gt 0 -and -not $targetAncestors.ContainsKey($walk); $i++) { $targetAncestors[$walk] = $true; $child = $parents[$walk]; $parent = $(if ($child) { $parents[$child.pid] } else { $null }); if (-not $child -or -not $parent -or -not $child.born -or -not $parent.born -or $parent.born -gt $child.born) { break }; $walk = $child.pid }`,
    '$related = $false; foreach ($gpuPid in $gpuPids) { $walk = $gpuPid; for ($i = 0; $i -lt 64 -and $walk -gt 0; $i++) { if ($targetAncestors.ContainsKey($walk)) { $related = $true; break }; $child = $parents[$walk]; $parent = $(if ($child) { $parents[$child.pid] } else { $null }); if (-not $child -or -not $parent -or -not $child.born -or -not $parent.born -or $parent.born -gt $child.born) { break }; $walk = $child.pid }; if ($related) { break } }',
    'if ($related) { Write-Output "GPU compute process or its parent/child is active; operation refused"; exit 9 }',
  ];
  return [
    'if (-not $script:KatalaGpuGuardClock) { $script:KatalaGpuGuardClock = [Diagnostics.Stopwatch]::StartNew(); $script:KatalaGpuMutationStarted = $false }',
    'function Test-KatalaGpuDeadline { if ($script:KatalaGpuGuardClock.ElapsedMilliseconds -gt 9000) { if ($script:KatalaGpuMutationStarted) { Write-Output "GPU power outcome unverified"; exit 7 }; Write-Output "GPU guard timed out; operation refused"; exit 9 } }',
    GPU_SMI_HELPER,
    '$smi = Get-Command nvidia-smi.exe -ErrorAction SilentlyContinue; $gpuGuardApplicable = $true; if (-not $smi) { try { $adapters = @(Get-CimInstance Win32_VideoController -Property Name,PNPDeviceID -OperationTimeoutSec 4 -ErrorAction Stop); Test-KatalaGpuDeadline; $nvidia = @($adapters | Where-Object { $_.Name -match "(?i)NVIDIA" -or $_.PNPDeviceID -match "(?i)VEN_10DE" }); if ($adapters.Count -gt 0 -and $adapters.Count -le 32 -and $nvidia.Count -eq 0) { $gpuGuardApplicable = $false } else { Write-Output "GPU activity unavailable; operation refused"; exit 9 } } catch { Write-Output "GPU activity unavailable; operation refused"; exit 9 } } else { $script:KatalaGpuSmiSource = $smi.Source; $script:KatalaGpuSmiFunction = $smi.CommandType -eq "Function" }; if ($gpuGuardApplicable) {',
    '$gpuRows = @(Invoke-KatalaGpuSmi "--query-gpu=uuid,utilization.gpu --format=csv,noheader,nounits"); if ($script:KatalaGpuSmiFailed) { Write-Output $script:KatalaGpuSmiFailureReason; exit 9 }; if ($null -eq $gpuRows[0] -or $gpuRows.Count -eq 0 -or $gpuRows.Count -gt 16) { Write-Output "GPU activity unavailable; operation refused"; exit 9 }',
    '$gpuBusy = $false; foreach ($line in $gpuRows) { $part = $line -split ","; if ($part.Count -ne 2) { Write-Output "GPU activity unreadable; operation refused"; exit 9 }; try { $util = [double]::Parse($part[1].Trim(), [Globalization.CultureInfo]::InvariantCulture) } catch { Write-Output "GPU activity unreadable; operation refused"; exit 9 }; if ([double]::IsNaN($util) -or [double]::IsInfinity($util) -or $util -lt 0 -or $util -gt 100) { Write-Output "GPU activity unreadable; operation refused"; exit 9 }; if ($util -ge ' + GPU_ACTIVE_UTIL + ') { $gpuBusy = $true } }',
    '$computeResult = @(Invoke-KatalaGpuSmi "--query-compute-apps=pid,used_gpu_memory --format=csv,noheader,nounits"); if ($script:KatalaGpuSmiFailed) { Write-Output $script:KatalaGpuSmiFailureReason; exit 9 }; $computeRows = @($computeResult | Where-Object { $_.Trim() -and $_.Trim() -ne "No running processes found" }); if ($computeRows.Count -gt 16) { Write-Output "GPU compute process list unavailable; operation refused"; exit 9 }',
    '$gpuPids = @(); foreach ($line in $computeRows) { $part = $line -split ","; [int]$gpuPid = 0; if ($part.Count -ne 2 -or -not [int]::TryParse($part[0].Trim(), [ref]$gpuPid) -or $gpuPid -le 0) { Write-Output "GPU compute process list unreadable; operation refused"; exit 9 }; $gpuPids += $gpuPid }',
    'if ($gpuBusy) { Write-Output "GPU activity is active; operation refused"; exit 9 }',
    ...related,
    'if ($gpuPids.Count -gt 0) { Write-Output "GPU compute process is active; operation refused"; exit 9 }; Test-KatalaGpuDeadline; }',
  ].filter(Boolean).join('; ');
}

function finiteWatts(value) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0;
}

function winPowerScript(body) {
  return ['$mutex = New-Object System.Threading.Mutex($false, "Global\\KatalaTunePowerControl")', '$held = $false', 'try { try { $held = $mutex.WaitOne(0) } catch [System.Threading.AbandonedMutexException] { $held = $true }; if (-not $held) { Write-Output "another power action is running"; exit 8 }', body, '} finally { if ($held) { $mutex.ReleaseMutex() }; $mutex.Dispose() }'].join('; ');
}

function powerPlanScript({ guid, prev_guid }) {
  return winPowerScript([
    'function Read-Plan { $line = & powercfg.exe -getactivescheme; if ($LASTEXITCODE -ne 0) { return $null }; $m = [regex]::Match($line, "[0-9A-Fa-f-]{36}"); if (-not $m.Success) { return $null }; return $m.Value }',
    'function Read-Plan-Retry { $v = Read-Plan; if (-not $v) { Start-Sleep -Milliseconds 200; $v = Read-Plan }; return $v }',
    '$before = Read-Plan-Retry; if (-not $before) { Write-Output "power plan unreadable"; exit 6 }',
    `if ($before -ne "${prev_guid}") { Write-Output "power plan changed"; exit 3 }`,
    `& powercfg.exe -setactive ${guid}; $apply = $LASTEXITCODE`,
    '$after = Read-Plan-Retry',
    `if ($apply -eq 0 -and $after -eq "${guid}") { exit 0 }`,
    `if ($after -eq "${prev_guid}") { Write-Output "power plan was not changed"; exit 6 }`,
    `if ($after -eq "${guid}") { & powercfg.exe -setactive ${prev_guid}; if ($LASTEXITCODE -eq 0) { $rolled = Read-Plan-Retry; if ($rolled -eq "${prev_guid}") { Write-Output "power plan verification failed; restored"; exit 6 } } }`,
    'Write-Output "power plan verification or rollback unverified"; exit 7',
  ].join('; '));
}

function gpuPowerScript({ uuid, watts, min, max, prev_w }) {
  return winPowerScript([
    'if (-not (Get-Command Test-KatalaActionExpiry -ErrorAction SilentlyContinue)) { function Test-KatalaActionExpiry {} }',
    'function Read-Power { $line = @(Invoke-KatalaGpuSmi "--id=' + uuid + ' --query-gpu=power.min_limit,power.max_limit,power.limit --format=csv,noheader,nounits"); if (-not $line) { return @() }; try { $r = @(); foreach ($x in ($line -split ",")) { $r += [double]::Parse($x.Trim(), [Globalization.CultureInfo]::InvariantCulture) }; if ($r.Count -ne 3) { return @() }; return $r } catch { return @() } }',
    'function Read-Power-Retry { $v = @(Read-Power); if ($v.Count -ne 3) { Start-Sleep -Milliseconds 200; $v = @(Read-Power) }; return $v }',
    gpuActiveGuardPs(),
    '$v = @(Read-Power-Retry); if ($v.Count -ne 3) { Write-Output "GPU power unreadable"; exit 6 }',
    `if ($v[0] -ne ${min} -or $v[1] -ne ${max} -or $v[2] -ne ${prev_w}) { Write-Output "GPU power state changed"; exit 3 }`,
    gpuActiveGuardPs(),
    '$beforeApply = @(Read-Power-Retry); if ($beforeApply.Count -ne 3) { Write-Output "GPU power unreadable"; exit 6 }',
    `if ($beforeApply[0] -ne ${min} -or $beforeApply[1] -ne ${max} -or $beforeApply[2] -ne ${prev_w}) { Write-Output "GPU power state changed"; exit 3 }; Test-KatalaGpuDeadline`,
    `Test-KatalaActionExpiry; $script:KatalaGpuMutationStarted = $true; $applyRows = @(Invoke-KatalaGpuSmi "--id=${uuid} -pl ${watts}"); $apply = $script:KatalaGpuSmiExitCode; if ($script:KatalaGpuSmiTimedOut) { Write-Output "GPU power apply outcome unverified"; exit 7 }`,
    '$after = @(Read-Power-Retry)',
    `if ($apply -eq 0 -and $after.Count -eq 3 -and $after[2] -eq ${watts}) { exit 0 }`,
    `if ($after.Count -eq 3 -and $after[0] -eq ${min} -and $after[1] -eq ${max} -and $after[2] -eq ${prev_w}) { Write-Output "power limit was not changed"; exit 6 }`,
    `if ($after.Count -eq 3 -and $after[2] -eq ${watts} -and $after[0] -le ${prev_w} -and $after[1] -ge ${prev_w}) { Test-KatalaActionExpiry; Test-KatalaGpuDeadline; $rollbackRows = @(Invoke-KatalaGpuSmi "--id=${uuid} -pl ${prev_w}"); if ($script:KatalaGpuSmiTimedOut) { Write-Output "GPU power rollback outcome unverified"; exit 7 }; if ($script:KatalaGpuSmiExitCode -eq 0) { $rolled = @(Read-Power-Retry); if ($rolled.Count -eq 3 -and $rolled[2] -eq ${prev_w}) { Write-Output "power limit verification failed; restored"; exit 6 } } }`,
    'Write-Output "power limit verification or rollback unverified"; exit 7',
  ].join('; '));
}

function lowPowerModeScript({ source, enabled, prev }) {
  const flag = source === 'ac' ? '-c' : '-b';
  const heading = source === 'ac' ? 'AC Power' : 'Battery Power';
  const desired = enabled ? 1 : 0;
  const previous = prev ? 1 : 0;
  const raw = (key) => `pmset -g custom | awk '/^${heading}:/{f=1;next} /^[^ ]/{f=0} f && /^[[:space:]]*${key} / {print $2; exit}'`;
  const read = `key=powermode; mode=$(${raw('powermode')}); [ -n "$mode" ] || { key=lowpowermode; mode=$(${raw('lowpowermode')}); }; if [ "$mode" = 0 ] || [ "$mode" = 1 ]; then echo "$key:$mode"; else exit 2; fi`;
  return [
    `lock="/tmp/org.katala.tune-power-control.lock"; mkdir -m 700 "$lock" 2>/dev/null || { echo "another power action is running; remove only after verifying owner PID is gone"; exit 8; }; trap 'rm -f "$lock/pid"; rmdir "$lock"' EXIT; trap 'exit 7' HUP INT TERM; printf "%s\\n" "$$" > "$lock/pid" || exit 6`,
    `before=$(${read}) || { echo "low power mode unsupported"; exit 6; }; before_key=${'${before%%:*}'}; before_mode=${'${before#*:}'}`,
    `[ "$before_mode" = ${previous} ] || { echo "low power mode changed"; exit 3; }`,
    `pmset ${flag} "$before_key" ${desired}; apply=$?`,
    `after=$(${read}) || { echo "low power mode verification unverified"; exit 7; }; after_key=${'${after%%:*}'}; after_mode=${'${after#*:}'}`,
    `[ "$apply" = 0 ] && [ "$after_key" = "$before_key" ] && [ "$after_mode" = ${desired} ] && exit 0`,
    `[ "$after_key" = "$before_key" ] && [ "$after_mode" = ${previous} ] && exit 6`,
    `if [ "$after_key" = "$before_key" ] && [ "$after_mode" = ${desired} ]; then pmset ${flag} "$before_key" ${previous}; rolled=$(${read}); [ "$rolled" = "$before_key:${previous}" ] && { echo "low power mode apply failed; restored"; exit 6; }; fi`,
    'echo "low power mode verification or rollback unverified"; exit 7',
  ].join('\n');
}

// 終了スクリプトの結果コード
const EXIT = { 0: '終了を確認した', 3: '別のプロセスに入れ替わっていたので中止した', 4: '負荷が回復していたので中止した', 5: '終了を指示したが5秒以内に終了を確認できなかった（終了未確認）', 6: '終了の指示が失敗した', 9: 'GPU の活動を確認できた、または取得できないため中止した' };

function isProtected(name, protect = []) {
  return NO_KILL.test(name) || protect.some((p) => String(p).toLowerCase().replace(/\.exe$/, '') === name.toLowerCase());
}

function macKillScript({ pid, name, start, min_cpu }) {
  return [
    `n=$(basename "$(ps -p ${pid} -o comm= 2>/dev/null)" 2>/dev/null)`,
    `[ "$n" = '${name}' ] || { echo "PID ${pid} は今 \${n:-存在しない}"; exit 3; }`,
    start ? `[ "$(ps -p ${pid} -o lstart= | sed 's/^ *//;s/ *$//')" = '${start}' ] || { echo "PID ${pid} は別の起動時刻のプロセス"; exit 3; }` : ':',
    `c=$(ps -p ${pid} -o pcpu= | awk '{print int($1)}')`,
    `[ "\${c:-0}" -ge ${min_cpu} ] || { echo "負荷が回復している（\${c}%）"; exit 4; }`,
    `kill -TERM ${pid} || exit 6`,
    'i=0; while [ $i -lt 10 ]; do sleep 0.5; kill -0 ' + pid + ' 2>/dev/null || { echo "終了を確認（$((i/2+1))秒以内）"; exit 0; }; i=$((i+1)); done',
    'echo "5秒以内に終了を確認できない"; exit 5',
  ].join('\n');
}

// PowerShell 本体。シングルクォートを含めない（ssh 越しに bash の '...' で包むため）
function winKillPs({ pid, name, start, min_cpu }) {
  return [
    'if (-not (Get-Command Test-KatalaActionExpiry -ErrorAction SilentlyContinue)) { function Test-KatalaActionExpiry {} }',
    `$p = Get-Process -Id ${pid} -ErrorAction SilentlyContinue`,
    `if (-not $p -or $p.ProcessName -ne "${name}") { Write-Output ("PID ${pid} is now " + $p.ProcessName); exit 3 }`,
    start ? `if ($p.StartTime.ToUniversalTime().ToString("yyyyMMddHHmmssfff") -ne "${start}") { Write-Output "PID ${pid} has a different start time"; exit 3 }` : '',
    '$c1 = $p.CPU; Start-Sleep -Milliseconds 1000; $p.Refresh(); $core = ($p.CPU - $c1) * 100',
    `if ($core -lt ${min_cpu}) { Write-Output ("recovered: " + [math]::Round($core) + "% of one core"); exit 4 }`,
    `Test-KatalaActionExpiry; try { Stop-Process -Id ${pid} -Force -ErrorAction Stop } catch { Write-Output $_.Exception.Message; exit 6 }`,
    `try { Wait-Process -Id ${pid} -Timeout 5 -ErrorAction Stop } catch { if (Get-Process -Id ${pid} -ErrorAction SilentlyContinue) { Write-Output "not confirmed within 5s"; exit 5 } }`,
    'Write-Output "terminated"; exit 0',
  ].filter(Boolean).join('; ');
}

function winKillRecheckPs({ pid, name, start, min_cpu }) {
  return [
    `$p = Get-Process -Id ${pid} -ErrorAction SilentlyContinue`,
    `if (-not $p -or $p.ProcessName -ne "${name}") { Write-Output ("PID ${pid} is now " + $p.ProcessName); exit 3 }`,
    start ? `if ($p.StartTime.ToUniversalTime().ToString("yyyyMMddHHmmssfff") -ne "${start}") { Write-Output "PID ${pid} has a different start time"; exit 3 }` : '',
    '$c2 = $p.CPU; Start-Sleep -Milliseconds 1000; $p.Refresh(); $core2 = ($p.CPU - $c2) * 100',
    `if ($p.ProcessName -ne "${name}") { Write-Output "PID ${pid} changed during CPU recheck"; exit 3 }`,
    start ? `if ($p.StartTime.ToUniversalTime().ToString("yyyyMMddHHmmssfff") -ne "${start}") { Write-Output "PID ${pid} has a different start time"; exit 3 }` : '',
    `if ($core2 -lt ${min_cpu}) { Write-Output ("recovered: " + [math]::Round($core2) + "% of one core"); exit 4 }; Test-KatalaGpuDeadline`,
  ].filter(Boolean).join('; ');
}

// Windows の ssh 既定シェルは Git Bash で、/xxx 形式の引数をパスに書き換える。powercfg などは -xxx 形式で渡す
const SPECS = {
  'kill-process': {
    build(node, { pid, name, start = null, min_cpu = 50 }, { protect }) {
      if (!Number.isInteger(pid) || pid <= 4) throw new Error('PID が不正');
      if (typeof name !== 'string' || !PROC_NAME.test(name) || name.includes("'")) throw new Error('プロセス名が不正');
      if (isProtected(name, protect)) throw new Error(`${name} は保護対象（終了しない）`);
      // min_cpu = 0 は負荷の再確認をしない（プロセス一覧から操作者が明示的に選んだとき）
      if (!Number.isInteger(min_cpu) || min_cpu < 0 || min_cpu > 10000) throw new Error('負荷のしきい値が不正');
      if (start != null && !(node.os === 'macos' ? MAC_LSTART : WIN_START).test(start)) throw new Error('起動時刻が不正');
      const p = { pid, name, start, min_cpu };
      return {
        describe: `${node.id} の ${name}（PID ${pid}）を終了する。直前に同じプロセスか${min_cpu ? `・まだ重いか（1コアの ${min_cpu}% 以上）` : ''}を確かめ、違えば中止する。保存していない作業は失われ、元には戻せない。`,
        script: node.os === 'macos' ? macKillScript(p) : `${winKillPs(p).replace(`Test-KatalaActionExpiry; try { Stop-Process -Id ${pid}`, `${gpuActiveGuardPs(pid)}; ${winKillRecheckPs(p)}; Test-KatalaActionExpiry; try { Stop-Process -Id ${pid}`)}`,
        shell: node.os === 'macos' ? 'sh' : 'ps',
        exits: EXIT,
      };
    },
  },
  'set-power-plan': {
    windowsOnly: true,
    build(node, { guid, prev_guid }) {
      if (!GUID.test(guid) || !GUID.test(prev_guid)) throw new Error('GUID が不正');
      return {
        describe: `${node.id} の電源プランを切り替える。直前のプランを再読し、適用後に検証できなければ戻す。`,
        script: powerPlanScript({ guid, prev_guid }),
        shell: 'ps',
        undo: { type: 'set-power-plan', params: { guid: prev_guid, prev_guid: guid } },
      };
    },
  },
  'set-gpu-power-limit': {
    windowsOnly: true,
    build(node, { uuid, watts, min, max, prev_w }) {
      if (typeof uuid !== 'string' || !NVIDIA_UUID.test(uuid)) throw new Error('GPU UUID が不正');
      if (![watts, min, max, prev_w].every(finiteWatts) || min > max || watts < min || watts > max || prev_w < min || prev_w > max) throw new Error('GPU 電力の値が不正');
      const params = { uuid, watts, min, max, prev_w };
      return {
        describe: `${node.id} の GPU 電力上限を ${watts}W にする。直前に同じ GPU の対応範囲と現在値を確認し、検証できなければ戻す。`,
        script: gpuPowerScript(params), shell: 'ps',
        undo: { type: 'set-gpu-power-limit', params: { ...params, watts: prev_w, prev_w: watts } },
      };
    },
  },
  'set-low-power-mode': {
    macOnly: true,
    build(node, { source, enabled, prev }) {
      if (source !== 'ac' && source !== 'battery') throw new Error('電源ドメインが不正');
      if (typeof enabled !== 'boolean' || typeof prev !== 'boolean') throw new Error('真偽値が不正');
      const params = { source, enabled, prev };
      return {
        describe: `${node.id} の${source === 'ac' ? 'AC' : 'バッテリー'}時の低電力モードを${enabled ? '有効' : '無効'}にする。直前の値が変わっていれば実行しない。`,
        script: lowPowerModeScript(params), shell: 'sh',
        undo: { type: 'set-low-power-mode', params: { ...params, enabled: prev, prev: enabled } },
      };
    },
  },
};

// 定期処理の管理。Windows はタスクスケジューラ、macOS は自分のユーザーの LaunchAgents だけ（system の daemon は触らない）
const TASK_PATH = /^\\([\w .-]+\\)*$/;
const TASK_NAME = /^[\w .()+-]{1,120}$/;
const LABEL = /^[\w.-]{1,150}$/;
const LAUNCH_AGENTS = /^\/Users\/[\w.-]+\/Library\/LaunchAgents\/[\w.-]+\.plist$/;

function checkTask({ path, name }) {
  if (typeof path !== 'string' || !TASK_PATH.test(path)) throw new Error('タスクのパスが不正');
  if (typeof name !== 'string' || !TASK_NAME.test(name)) throw new Error('タスク名が不正');
  if (/^\\Microsoft\\/i.test(path)) throw new Error('Windows 標準のタスクは扱わない');
}
function checkLabel({ label, plist }) {
  if (typeof label !== 'string' || !LABEL.test(label) || label.startsWith('com.apple.')) throw new Error('ラベルが不正');
  if (plist != null && (typeof plist !== 'string' || !LAUNCH_AGENTS.test(plist))) throw new Error('plist はユーザーの LaunchAgents のものだけ');
}

// Read the actual enabled bit, not State (a running task can still be enabled).
function taskScript(p, enabled) {
  if (Object.hasOwn(p, 'expected_enabled') && typeof p.expected_enabled !== 'boolean') throw new Error('タスクの期待状態が不正');
  const read = `(Get-ScheduledTask -TaskPath "${p.path}" -TaskName "${p.name}" -ErrorAction Stop).Settings.Enabled`;
  const desired = enabled ? '$true' : '$false';
  return [
    'try {',
    `$before = ${read}`,
    'if ($before -isnot [bool]) { throw "task state unavailable" }',
    ...(Object.hasOwn(p, 'expected_enabled') ? [`if ($before -ne $${p.expected_enabled}) { throw "task state changed; operation refused" }`] : []),
    `if ($before -ne ${desired}) { ${enabled ? 'Enable' : 'Disable'}-ScheduledTask -TaskPath "${p.path}" -TaskName "${p.name}" -ErrorAction Stop | Out-Null }`,
    `$after = ${read}`,
    `if ($after -isnot [bool] -or $after -ne ${desired}) { throw "task readback mismatch" }`,
    'Write-Output ("KT_TASK_STATE " + [int]$before + " " + [int]$after)',
    'exit 0',
    '} catch { Write-Output "task outcome unverified"; exit 7 }',
  ].join('; ');
}

Object.assign(SPECS, {
  'task-disable': {
    windowsOnly: true,
    build(node, p) {
      checkTask(p);
      return { describe: `${node.id} のタスク「${p.path}${p.name}」を無効にする（予定どおりに動かなくなる）。元に戻せる。`, script: taskScript(p, false), shell: 'ps' };
    },
  },
  'task-enable': {
    windowsOnly: true,
    build(node, p) {
      checkTask(p);
      return { describe: `${node.id} のタスク「${p.path}${p.name}」を有効にする。元に戻せる。`, script: taskScript(p, true), shell: 'ps' };
    },
  },
  'task-run': {
    windowsOnly: true,
    build(node, p) {
      checkTask(p);
      return { describe: `${node.id} のタスク「${p.path}${p.name}」を今すぐ1回実行する。中身（バックアップ・同期など）が実際に動く。元には戻せない。`, script: `try { Start-ScheduledTask -TaskPath "${p.path}" -TaskName "${p.name}" -ErrorAction Stop; Write-Output "KT_TASK_RUN"; exit 0 } catch { Write-Output "task outcome unverified"; exit 7 }`, shell: 'ps' };
    },
  },
  'launchd-unload': {
    macOnly: true,
    build(node, p) {
      checkLabel(p);
      if (!p.plist) throw new Error('元に戻すための plist のパスが要る');
      return { describe: `${node.id} の launchd ジョブ ${p.label} を止めて読み込みを外す（次のログインまで、または元に戻すまで動かない）。`, script: `launchctl bootout gui/$(id -u)/${p.label} && echo unloaded`, shell: 'sh', undo: { type: 'launchd-load', params: { label: p.label, plist: p.plist } } };
    },
  },
  'launchd-load': {
    macOnly: true,
    build(node, p) {
      checkLabel(p);
      if (!p.plist) throw new Error('plist のパスが要る');
      return { describe: `${node.id} の launchd ジョブ ${p.label} を読み込む。`, script: `launchctl bootstrap gui/$(id -u) '${p.plist}' && echo loaded`, shell: 'sh', undo: { type: 'launchd-unload', params: { label: p.label, plist: p.plist } } };
    },
  },
  'launchd-kickstart': {
    macOnly: true,
    build(node, p) {
      checkLabel(p);
      return { describe: `${node.id} の launchd ジョブ ${p.label} を今すぐ1回実行する（動いていれば再起動しない）。中身が実際に動く。`, script: `launchctl kickstart gui/$(id -u)/${p.label} && echo started`, shell: 'sh' };
    },
  },
});

// ctx.protect: 実行の直前に読み直した保護リスト。読めなかったら呼び出し側で止める（ここには来ない）
function plan(node, action, ctx = {}) {
  const spec = SPECS[action?.type];
  if (!spec) throw new Error(`許可リストに無い操作: ${action?.type}`);
  if (node.shared) throw new Error(`${node.id} は共用機のため、このアプリからは変更しない`);
  const refused = ledger.refusal(node);
  if (refused) throw new Error(refused);
  if (spec.windowsOnly && node.os !== 'windows') throw new Error('Windows だけの操作');
  if (spec.macOnly && node.os !== 'macos') throw new Error('macOS だけの操作');
  if (!Array.isArray(ctx.protect)) throw new Error('保護リストを読めないので実行しない');
  if (node.protect_gpu === true && (action.type === 'kill-process' || action.type === 'set-gpu-power-limit')) throw new Error(`${node.id} は GPU 学習保護のため、この操作を実行しない`);
  return spec.build(node, action.params || {}, ctx);
}

// 実際に送るコマンド。Windows のリモートは Git Bash 経由なので PowerShell を '...' で包む
function wrap(node, p) {
  if (p.shell === 'sh' || node.local) return p.script;
  if (p.script.includes("'")) throw new Error('PowerShell 本体にシングルクォートは使えない');
  return `powershell.exe -NoProfile -NonInteractive -Command '${p.script}'`;
}

// exec: 機体でコマンドを実行する口（既定は ssh / ローカルシェル。テストが偽物に差し替える）
async function execute(node, action, ctx, exec = execOn, actionExchange = defaultActionExchange) {
  const p = plan(node, action, ctx);
  const stdinTransport = node.os === 'windows' && (action.type === 'kill-process' || action.type === 'set-gpu-power-limit');
  let res;
  if (stdinTransport) {
    try {
      const helper = actionHelper(node);
      const body = Buffer.from(p.script, 'utf8');
      if (!body.length || body.length > ACTION_INPUT_MAX) throw new Error('action stdin 本文の長さが不正');
      res = await actionExchange(node, helper, body);
    } catch (e) { res = { code: 9, out: '', err: String(e.message || e) }; }
  }
  if (!res) res = await exec(node, wrap(node, p), 30000, null);
  let undo = p.undo || null;
  if (action.type === 'task-disable' || action.type === 'task-enable') {
    const receipt = /^KT_TASK_STATE ([01]) ([01])$/.exec(res.out.trim());
    const desired = action.type === 'task-enable';
    if (res.code === 0 && (!receipt || res.err.trim() || (receipt[2] === '1') !== desired
      || (Object.hasOwn(action.params, 'expected_enabled') && (receipt[1] === '1') !== action.params.expected_enabled))) {
      res = { code: 7, out: 'task outcome unverified', err: '' };
    }
    if (res.code === 0 && receipt[1] !== receipt[2]) undo = { type: desired ? 'task-disable' : 'task-enable', params: { path: action.params.path, name: action.params.name, expected_enabled: desired } };
  } else if (action.type === 'task-run' && res.code === 0 && (res.out.trim() !== 'KT_TASK_RUN' || res.err.trim())) {
    res = { code: 7, out: 'task outcome unverified', err: '' };
  }
  return {
    ok: res.code === 0, code: res.code,
    outcome: p.exits?.[res.code] || (res.code === 9 ? EXIT[9] : (res.code === 0 ? '完了' : `失敗（exit ${res.code}）`)),
    output: (res.out + res.err).trim().slice(-1500),
    undo: (res.code === 0 || (action.type !== 'set-gpu-power-limit' && res.code === 7 && p.undo)) ? undo : null,
  };
}

module.exports = { plan, execute, wrap, SPECS, EXIT, actionHelper, actionHelperFingerprint };
