//! 実行できる最適化の許可リスト。ここに無い操作は実行しない（lib/actions.js と同じ）。
//! 呼び出し側（アプリ）が確認ダイアログで操作者の承認を取り、その直前と直後に台帳と保護リストを読み直してから execute する。
//!
//! プロセス終了は NeonMonitor 1.1.x のレビューで見つかった失敗の型を避ける（docs/safety.md）:
//!   - 古い判断のまま終了しない: 終了の直前に、同じ PID が同じ名前・同じ起動時刻のままかを確かめる（PID の再利用対策）
//!   - 回復していれば終了しない: 直前に負荷を測り直し、下がっていれば中止する
//!   - 保護リストを読めなければ終了しない: 呼び出し側が毎回読み直し、失敗したら plan まで進まない
//!   - 終了の成功を断定しない: 5秒以内に消えたことを確認できなければ「終了未確認」として返す

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde_json::{Value, json};

use crate::collect::{Runner, System, exec_on_with, exec_on_with_input};
use crate::js;
use crate::nodes::Node;
use crate::rules::no_kill;

static GUID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?i-u:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$").expect("GUID"));
static PROC_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_ .()+-]{1,80}$").expect("PROC_NAME"));
static MAC_LSTART: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z]{3} [A-Za-z]{3} [ 0-9][0-9] [0-9]{2}:[0-9]{2}:[0-9]{2} [0-9]{4}$").expect("MAC_LSTART"));
static WIN_START: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9]{17}$").expect("WIN_START"));
static NVIDIA_UUID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^GPU-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$").expect("NVIDIA_UUID"));
const GPU_ACTIVE_UTIL: i64 = 5;
fn gpu_smi_helper() -> String {
    r#"function Stop-KatalaGpuSmiProcess { param($Process); if ($script:KatalaGpuSmiCleanupAttempted) { return $false }; $script:KatalaGpuSmiCleanupAttempted = $true; try { if ($Process.HasExited) { return $true }; $left = [Math]::Max(0, 9000 - [int]$script:KatalaGpuGuardClock.ElapsedMilliseconds); if ($left -le 0) { $script:KatalaGpuSmiTransportFailed = $true; return $false }; $Process.Kill(); $killAck = $Process.WaitForExit([Math]::Min(1000, $left)); if (-not $killAck -or -not $Process.HasExited) { $script:KatalaGpuSmiTransportFailed = $true; return $false }; return $true } catch { $script:KatalaGpuSmiTransportFailed = $true; return $false } }; function Fail-KatalaGpuSmi { param([bool]$TimedOut, [string]$Reason, [bool]$Permanent); $script:KatalaGpuSmiTimedOut = $TimedOut; $script:KatalaGpuSmiFailed = $true; $script:KatalaGpuSmiFailureReason = $Reason; if ($Permanent) { $script:KatalaGpuSmiTransportFailed = $true }; if ($script:KatalaGpuMutationStarted) { Write-Output "GPU power outcome unverified"; exit 7 }; return $null }; function Invoke-KatalaGpuSmi { param([string]$Arguments); $script:KatalaGpuSmiTimedOut = $false; $script:KatalaGpuSmiExitCode = $null; $script:KatalaGpuSmiFailed = $false; $script:KatalaGpuSmiFailureReason = "GPU activity unavailable; operation refused"; if ($script:KatalaGpuSmiTransportFailed) { return (Fail-KatalaGpuSmi $false "GPU SMI process lifetime unverified; operation refused" $true) }; $script:KatalaGpuSmiCleanupAttempted = $false; Test-KatalaGpuDeadline; if ($script:KatalaGpuSmiFunction) { $argv = @($Arguments.Split(" ")); $rows = @(& nvidia-smi.exe @argv); $script:KatalaGpuSmiExitCode = $LASTEXITCODE; if ($script:KatalaGpuSmiExitCode -ne 0) { return (Fail-KatalaGpuSmi $false "GPU SMI exited unsuccessfully; operation refused" $false) }; return $rows }; $p = $null; try { $si = New-Object Diagnostics.ProcessStartInfo; $si.FileName = $script:KatalaGpuSmiSource; $si.Arguments = $Arguments; $si.UseShellExecute = $false; $si.CreateNoWindow = $true; $si.RedirectStandardOutput = $true; $si.RedirectStandardError = $true; $p = New-Object Diagnostics.Process; $p.StartInfo = $si; $smiDeadline = [Math]::Min(9000, [int]$script:KatalaGpuGuardClock.ElapsedMilliseconds + 4000); [void]$p.Start(); $stdout = New-Object IO.MemoryStream(65536); $stderrCount = 0; $outBuffer = New-Object byte[] 4096; $errBuffer = New-Object byte[] 4096; $outRead = $p.StandardOutput.BaseStream.ReadAsync($outBuffer, 0, $outBuffer.Length); $errRead = $p.StandardError.BaseStream.ReadAsync($errBuffer, 0, $errBuffer.Length); $outDone = $false; $errDone = $false; $failed = $false; $timedOut = $false; $reason = "GPU SMI output exceeded limit; operation refused"; while (-not ($outDone -and $errDone)) { $left = [Math]::Max(0, $smiDeadline - [int]$script:KatalaGpuGuardClock.ElapsedMilliseconds); if ($left -le 0) { $timedOut = $true; $failed = $true; $reason = "GPU SMI timed out or held a pipe; operation refused"; break }; $pending = New-Object Collections.Generic.List[Threading.Tasks.Task]; if (-not $outDone) { $pending.Add($outRead) }; if (-not $errDone) { $pending.Add($errRead) }; if ([Threading.Tasks.Task]::WaitAny([Threading.Tasks.Task[]]$pending.ToArray(), $left) -lt 0) { $timedOut = $true; $failed = $true; $reason = "GPU SMI timed out or held a pipe; operation refused"; break }; if (-not $outDone -and $outRead.IsCompleted) { $outCount = $outRead.Result; if ($outCount -le 0) { $outDone = $true } elseif ($stdout.Length + $outCount -gt 65536) { $failed = $true; $reason = "GPU SMI stdout exceeded limit; operation refused"; break } else { $stdout.Write($outBuffer, 0, $outCount); $outRead = $p.StandardOutput.BaseStream.ReadAsync($outBuffer, 0, $outBuffer.Length) } }; if (-not $errDone -and $errRead.IsCompleted) { $errCount = $errRead.Result; if ($errCount -le 0) { $errDone = $true } elseif ($stderrCount + $errCount -gt 65536) { $failed = $true; $reason = "GPU SMI stderr exceeded limit; operation refused"; break } else { $stderrCount += $errCount; $errRead = $p.StandardError.BaseStream.ReadAsync($errBuffer, 0, $errBuffer.Length) } } }; if ($failed) { $script:KatalaGpuSmiTransportFailed = $true; [void](Stop-KatalaGpuSmiProcess $p); return (Fail-KatalaGpuSmi $timedOut $reason $true) }; $left = [Math]::Max(0, $smiDeadline - [int]$script:KatalaGpuGuardClock.ElapsedMilliseconds); if ($left -le 0 -or -not $p.WaitForExit($left)) { $script:KatalaGpuSmiTransportFailed = $true; [void](Stop-KatalaGpuSmiProcess $p); return (Fail-KatalaGpuSmi $true "GPU SMI timed out or held a pipe; operation refused" $true) }; $script:KatalaGpuSmiExitCode = $p.ExitCode; if ($script:KatalaGpuSmiExitCode -ne 0) { return (Fail-KatalaGpuSmi $false "GPU SMI exited unsuccessfully; operation refused" $false) }; return @([Text.Encoding]::UTF8.GetString($stdout.ToArray()) -split "`r?`n") } catch { if ($p) { $script:KatalaGpuSmiTransportFailed = $true; [void](Stop-KatalaGpuSmiProcess $p); return (Fail-KatalaGpuSmi $false "GPU SMI transport failed; operation refused" $true) }; return (Fail-KatalaGpuSmi $false "GPU SMI transport failed; operation refused" $true) } finally { if ($p) { try { if (-not $p.HasExited -and -not $script:KatalaGpuSmiCleanupAttempted) { $cleanupAck = Stop-KatalaGpuSmiProcess $p; if (-not $cleanupAck) { $script:KatalaGpuSmiTransportFailed = $true } } } catch { $script:KatalaGpuSmiTransportFailed = $true }; $p.Dispose() } } }"#.to_string()
}
const ACTION_INPUT_MAX: usize = 65_536;
static TASK_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\\([A-Za-z0-9_ .-]+\\)*$").expect("TASK_PATH"));
static TASK_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_ .()+-]{1,120}$").expect("TASK_NAME"));
static LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.-]{1,150}$").expect("LABEL"));
/// ユーザーの LaunchAgents の plist（rules の「登録し直す」コマンドでも使う）
pub(crate) static LAUNCH_AGENTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/Users/[A-Za-z0-9_.-]+/Library/LaunchAgents/[A-Za-z0-9_.-]+\.plist$").expect("LAUNCH_AGENTS"));

// Only this fixed receiver goes in argv. The body and dynamic arguments stay in stdin.
static ACTION_RECEIVER: LazyLock<String> = LazyLock::new(|| {
    [
    "$s=[Console]::OpenStandardInput()", "$w=[Diagnostics.Stopwatch]::StartNew()", "$b=New-Object byte[] 1", "$h=New-Object Collections.Generic.List[byte]", "$headerDone=$false",
    "while($h.Count -lt 256){$remaining=5000-[int]$w.ElapsedMilliseconds;if($remaining -le 0){exit 9};$t=$s.ReadAsync($b,0,1);if(-not $t.Wait($remaining)){exit 9};if($t.Result -ne 1){exit 9};if($b[0]-eq 10){$headerDone=$true;break};if($b[0]-lt 32 -or $b[0]-gt 126){exit 9};$h.Add($b[0])}", "if(-not $headerDone){exit 9}",
    "try{$m=[Text.Encoding]::ASCII.GetString($h.ToArray()) -split \" \"", "if($m.Count-ne 4 -or $m[0]-ne \"KT1\" -or $m[1]-notmatch \"^[0-9]+$\" -or $m[2]-notmatch \"^[0-9a-f]{64}$\" -or $m[3]-notmatch \"^[0-9]+$\"){exit 9}", "$n=[int]$m[1];$e=[int64]$m[3]", "if($n -lt 1 -or $n -gt 65536 -or [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() -gt $e){exit 9}", "$d=New-Object byte[] $n;$o=0",
    "while($o-lt$n){$remaining=5000-[int]$w.ElapsedMilliseconds;if($remaining -le 0){exit 9};$t=$s.ReadAsync($d,$o,$n-$o);if(-not $t.Wait($remaining)){exit 9};if($t.Result-le 0){exit 9};$o+=$t.Result}", "$x=New-Object byte[] 1;$remaining=5000-[int]$w.ElapsedMilliseconds;if($remaining -le 0){exit 9};$t=$s.ReadAsync($x,0,1);if(-not $t.Wait($remaining)){exit 9};if($t.Result-ne 0){exit 9}",
    "$q=[BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash($d)).Replace(\"-\",\"\").ToLowerInvariant()", "if($q-ne$m[2] -or [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() -gt $e){exit 9}", "$u=New-Object Text.UTF8Encoding($false,$true);$script:KatalaActionExpiryMs=$e", "function Test-KatalaActionExpiry { if([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() -gt $script:KatalaActionExpiryMs){if($script:KatalaGpuMutationStarted){Write-Output \"GPU power outcome unverified\";exit 7};Write-Output \"action request expired; operation refused\";exit 9} }", "Test-KatalaActionExpiry;& ([ScriptBlock]::Create($u.GetString($d)))}catch{if($script:KatalaGpuMutationStarted){Write-Output \"GPU power outcome unverified\";exit 7};exit 9}",
].join(";")
});

fn action_receiver_command() -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = ACTION_RECEIVER.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut encoded = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        encoded.push(char::from(ALPHABET[((n >> 18) & 63) as usize]));
        encoded.push(char::from(ALPHABET[((n >> 12) & 63) as usize]));
        encoded.push(if chunk.len() > 1 { char::from(ALPHABET[((n >> 6) & 63) as usize]) } else { '=' });
        encoded.push(if chunk.len() > 2 { char::from(ALPHABET[(n & 63) as usize]) } else { '=' });
    }
    format!("powershell.exe -NoProfile -NonInteractive -EncodedCommand {encoded}")
}

fn action_input(script: &str) -> Result<Vec<u8>, String> {
    let body = script.as_bytes();
    if body.is_empty() || body.len() > ACTION_INPUT_MAX {
        return Err("action stdin 本文の長さが不正".into());
    }
    let expiry = crate::db::now_ms().checked_add(25_000).ok_or("action stdin 時刻が不正")?;
    let mut input = format!("KT1 {} {} {}\n", body.len(), crate::db::sha256_hex(body), expiry).into_bytes();
    input.extend_from_slice(body);
    Ok(input)
}

/// 終了スクリプトの結果コード
pub fn exit_text(code: i32) -> Option<&'static str> {
    Some(match code {
        0 => "終了を確認した",
        3 => "別のプロセスに入れ替わっていたので中止した",
        4 => "負荷が回復していたので中止した",
        5 => "終了を指示したが5秒以内に終了を確認できなかった（終了未確認）",
        6 => "終了の指示が失敗した",
        9 => "GPU の活動を確認できた、または取得できないため中止した",
        _ => return None,
    })
}

// Keep this byte-for-byte aligned with lib/actions.js. It only reads NVIDIA's
// bounded telemetry and refuses state that is active or cannot be observed.
fn gpu_active_guard_ps(pid: Option<i64>) -> String {
    let mut parts = vec![
        "if (-not $script:KatalaGpuGuardClock) { $script:KatalaGpuGuardClock = [Diagnostics.Stopwatch]::StartNew(); $script:KatalaGpuMutationStarted = $false }".to_string(),
        "function Test-KatalaGpuDeadline { if ($script:KatalaGpuGuardClock.ElapsedMilliseconds -gt 9000) { if ($script:KatalaGpuMutationStarted) { Write-Output \"GPU power outcome unverified\"; exit 7 }; Write-Output \"GPU guard timed out; operation refused\"; exit 9 } }".to_string(),
        gpu_smi_helper(),
        "$smi = Get-Command nvidia-smi.exe -ErrorAction SilentlyContinue; $gpuGuardApplicable = $true; if (-not $smi) { try { $adapters = @(Get-CimInstance Win32_VideoController -Property Name,PNPDeviceID -OperationTimeoutSec 4 -ErrorAction Stop); Test-KatalaGpuDeadline; $nvidia = @($adapters | Where-Object { $_.Name -match \"(?i)NVIDIA\" -or $_.PNPDeviceID -match \"(?i)VEN_10DE\" }); if ($adapters.Count -gt 0 -and $adapters.Count -le 32 -and $nvidia.Count -eq 0) { $gpuGuardApplicable = $false } else { Write-Output \"GPU activity unavailable; operation refused\"; exit 9 } } catch { Write-Output \"GPU activity unavailable; operation refused\"; exit 9 } } else { $script:KatalaGpuSmiSource = $smi.Source; $script:KatalaGpuSmiFunction = $smi.CommandType -eq \"Function\" }; if ($gpuGuardApplicable) {".to_string(),
        "$gpuRows = @(Invoke-KatalaGpuSmi \"--query-gpu=uuid,utilization.gpu --format=csv,noheader,nounits\"); if ($script:KatalaGpuSmiFailed) { Write-Output $script:KatalaGpuSmiFailureReason; exit 9 }; if ($null -eq $gpuRows[0] -or $gpuRows.Count -eq 0 -or $gpuRows.Count -gt 16) { Write-Output \"GPU activity unavailable; operation refused\"; exit 9 }".to_string(),
        "$gpuBusy = $false; foreach ($line in $gpuRows) { $part = $line -split \",\"; if ($part.Count -ne 2) { Write-Output \"GPU activity unreadable; operation refused\"; exit 9 }; try { $util = [double]::Parse($part[1].Trim(), [Globalization.CultureInfo]::InvariantCulture) } catch { Write-Output \"GPU activity unreadable; operation refused\"; exit 9 }; if ([double]::IsNaN($util) -or [double]::IsInfinity($util) -or $util -lt 0 -or $util -gt 100) { Write-Output \"GPU activity unreadable; operation refused\"; exit 9 }; if ($util -ge %%UTIL%%) { $gpuBusy = $true } }".replace("%%UTIL%%", &GPU_ACTIVE_UTIL.to_string()),
        "$computeResult = @(Invoke-KatalaGpuSmi \"--query-compute-apps=pid,used_gpu_memory --format=csv,noheader,nounits\"); if ($script:KatalaGpuSmiFailed) { Write-Output $script:KatalaGpuSmiFailureReason; exit 9 }; $computeRows = @($computeResult | Where-Object { $_.Trim() -and $_.Trim() -ne \"No running processes found\" }); if ($computeRows.Count -gt 16) { Write-Output \"GPU compute process list unavailable; operation refused\"; exit 9 }".to_string(),
        "$gpuPids = @(); foreach ($line in $computeRows) { $part = $line -split \",\"; [int]$gpuPid = 0; if ($part.Count -ne 2 -or -not [int]::TryParse($part[0].Trim(), [ref]$gpuPid) -or $gpuPid -le 0) { Write-Output \"GPU compute process list unreadable; operation refused\"; exit 9 }; $gpuPids += $gpuPid }".to_string(),
        "if ($gpuBusy) { Write-Output \"GPU activity is active; operation refused\"; exit 9 }".to_string(),
    ];
    if let Some(pid) = pid {
        parts.extend([
            "$tree = @(Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId,CreationDate -OperationTimeoutSec 4 -ErrorAction SilentlyContinue); Test-KatalaGpuDeadline".to_string(),
            "if ($tree.Count -eq 0 -or $tree.Count -gt 4096) { Write-Output \"GPU process tree unreadable; operation refused\"; exit 9 }".to_string(),
            "$parents = @{}; foreach ($r in $tree) { if ($null -ne $r.ProcessId -and $null -ne $r.ParentProcessId) { $parents[[int]$r.ProcessId] = @{ pid = [int]$r.ParentProcessId; born = $r.CreationDate } } }".to_string(),
            "$targetAncestors = @{}; $walk = %%PID%%; for ($i = 0; $i -lt 64 -and $walk -gt 0 -and -not $targetAncestors.ContainsKey($walk); $i++) { $targetAncestors[$walk] = $true; $child = $parents[$walk]; $parent = $(if ($child) { $parents[$child.pid] } else { $null }); if (-not $child -or -not $parent -or -not $child.born -or -not $parent.born -or $parent.born -gt $child.born) { break }; $walk = $child.pid }".replace("%%PID%%", &pid.to_string()),
            "$related = $false; foreach ($gpuPid in $gpuPids) { $walk = $gpuPid; for ($i = 0; $i -lt 64 -and $walk -gt 0; $i++) { if ($targetAncestors.ContainsKey($walk)) { $related = $true; break }; $child = $parents[$walk]; $parent = $(if ($child) { $parents[$child.pid] } else { $null }); if (-not $child -or -not $parent -or -not $child.born -or -not $parent.born -or $parent.born -gt $child.born) { break }; $walk = $child.pid }; if ($related) { break } }".to_string(),
            "if ($related) { Write-Output \"GPU compute process or its parent/child is active; operation refused\"; exit 9 }".to_string(),
        ]);
    }
    parts
        .push("if ($gpuPids.Count -gt 0) { Write-Output \"GPU compute process is active; operation refused\"; exit 9 }; Test-KatalaGpuDeadline; }".to_string());
    parts.join("; ")
}

/// 実行の計画（確認ダイアログに出す説明と、実際に送るコマンド）
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub describe: String,
    pub script: String,
    /// "sh" か "ps"
    pub shell: &'static str,
    /// 成功したときに記録する「元に戻す」操作
    pub undo: Option<Value>,
    /// 終了コードの意味がある操作（kill-process）
    pub has_exits: bool,
}

fn is_protected(name: &str, protect: &[String]) -> bool {
    no_kill(name)
        || protect.iter().any(|p| {
            let l = p.to_lowercase();
            l.strip_suffix(".exe").unwrap_or(&l) == name.to_lowercase()
        })
}

fn mac_kill_script(pid: i64, name: &str, start: Option<&str>, min_cpu: i64) -> String {
    [
        format!("n=$(basename \"$(ps -p {pid} -o comm= 2>/dev/null)\" 2>/dev/null)"),
        format!("[ \"$n\" = '{name}' ] || {{ echo \"PID {pid} は今 ${{n:-存在しない}}\"; exit 3; }}"),
        match start {
            Some(s) => {
                format!("[ \"$(ps -p {pid} -o lstart= | sed 's/^ *//;s/ *$//')\" = '{s}' ] || {{ echo \"PID {pid} は別の起動時刻のプロセス\"; exit 3; }}")
            }
            None => ":".into(),
        },
        format!("c=$(ps -p {pid} -o pcpu= | awk '{{print int($1)}}')"),
        format!("[ \"${{c:-0}}\" -ge {min_cpu} ] || {{ echo \"負荷が回復している（${{c}}%）\"; exit 4; }}"),
        format!("kill -TERM {pid} || exit 6"),
        format!(
            "i=0; while [ $i -lt 10 ]; do sleep 0.5; kill -0 {pid} 2>/dev/null || {{ echo \"終了を確認（$((i/2+1))秒以内）\"; exit 0; }}; i=$((i+1)); done"
        ),
        "echo \"5秒以内に終了を確認できない\"; exit 5".into(),
    ]
    .join("\n")
}

// PowerShell 本体。シングルクォートを含めない（ssh 越しに bash の '...' で包むため）
fn win_kill_ps(pid: i64, name: &str, start: Option<&str>, min_cpu: i64) -> String {
    let mut parts = vec![
        "if (-not (Get-Command Test-KatalaActionExpiry -ErrorAction SilentlyContinue)) { function Test-KatalaActionExpiry {} }".to_string(),
        format!("$p = Get-Process -Id {pid} -ErrorAction SilentlyContinue"),
        format!("if (-not $p -or $p.ProcessName -ne \"{name}\") {{ Write-Output (\"PID {pid} is now \" + $p.ProcessName); exit 3 }}"),
    ];
    if let Some(s) = start {
        parts.push(format!("if ($p.StartTime.ToUniversalTime().ToString(\"yyyyMMddHHmmssfff\") -ne \"{s}\") {{ Write-Output \"PID {pid} has a different start time\"; exit 3 }}"));
    }
    parts.extend([
        "$c1 = $p.CPU; Start-Sleep -Milliseconds 1000; $p.Refresh(); $core = ($p.CPU - $c1) * 100".to_string(),
        format!("if ($core -lt {min_cpu}) {{ Write-Output (\"recovered: \" + [math]::Round($core) + \"% of one core\"); exit 4 }}"),
        gpu_active_guard_ps(Some(pid)),
        win_kill_recheck_ps(pid, name, start, min_cpu),
        format!("Test-KatalaActionExpiry; try {{ Stop-Process -Id {pid} -Force -ErrorAction Stop }} catch {{ Write-Output $_.Exception.Message; exit 6 }}"),
        format!("try {{ Wait-Process -Id {pid} -Timeout 5 -ErrorAction Stop }} catch {{ if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ Write-Output \"not confirmed within 5s\"; exit 5 }} }}"),
        "Write-Output \"terminated\"; exit 0".into(),
    ]);
    parts.join("; ")
}

fn win_kill_recheck_ps(pid: i64, name: &str, start: Option<&str>, min_cpu: i64) -> String {
    let mut parts = vec![
        format!("$p = Get-Process -Id {pid} -ErrorAction SilentlyContinue"),
        format!("if (-not $p -or $p.ProcessName -ne \"{name}\") {{ Write-Output (\"PID {pid} is now \" + $p.ProcessName); exit 3 }}"),
    ];
    if let Some(s) = start {
        parts.push(format!("if ($p.StartTime.ToUniversalTime().ToString(\"yyyyMMddHHmmssfff\") -ne \"{s}\") {{ Write-Output \"PID {pid} has a different start time\"; exit 3 }}"));
    }
    parts.extend([
        "$c2 = $p.CPU; Start-Sleep -Milliseconds 1000; $p.Refresh(); $core2 = ($p.CPU - $c2) * 100".to_string(),
        format!("if ($p.ProcessName -ne \"{name}\") {{ Write-Output \"PID {pid} changed during CPU recheck\"; exit 3 }}"),
    ]);
    if let Some(s) = start {
        parts.push(format!("if ($p.StartTime.ToUniversalTime().ToString(\"yyyyMMddHHmmssfff\") -ne \"{s}\") {{ Write-Output \"PID {pid} has a different start time\"; exit 3 }}"));
    }
    parts.push(format!(
        "if ($core2 -lt {min_cpu}) {{ Write-Output (\"recovered: \" + [math]::Round($core2) + \"% of one core\"); exit 4 }}; Test-KatalaGpuDeadline"
    ));
    parts.join("; ")
}

/// 整数か（JS の Number.isInteger と同じく、1.0 も整数）
fn int_of(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15).map(|f| f as i64)),
        _ => None,
    }
}

fn str_of(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str)
}

fn watts(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|x| x.is_finite() && *x >= 0.0)
}

fn power_plan_script(guid: &str, prev_guid: &str) -> String {
    let body = [
        "function Read-Plan { $line = & powercfg.exe -getactivescheme; if ($LASTEXITCODE -ne 0) { return $null }; $m = [regex]::Match($line, \"[0-9A-Fa-f-]{36}\"); if (-not $m.Success) { return $null }; return $m.Value }".into(),
        "function Read-Plan-Retry { $v = Read-Plan; if (-not $v) { Start-Sleep -Milliseconds 200; $v = Read-Plan }; return $v }".into(),
        "$before = Read-Plan-Retry; if (-not $before) { Write-Output \"power plan unreadable\"; exit 6 }".into(),
        format!("if ($before -ne \"{prev_guid}\") {{ Write-Output \"power plan changed\"; exit 3 }}"),
        format!("& powercfg.exe -setactive {guid}; $apply = $LASTEXITCODE"),
        "$after = Read-Plan-Retry".into(),
        format!("if ($apply -eq 0 -and $after -eq \"{guid}\") {{ exit 0 }}"),
        format!("if ($after -eq \"{prev_guid}\") {{ Write-Output \"power plan was not changed\"; exit 6 }}"),
        format!("if ($after -eq \"{guid}\") {{ & powercfg.exe -setactive {prev_guid}; if ($LASTEXITCODE -eq 0) {{ $rolled = Read-Plan-Retry; if ($rolled -eq \"{prev_guid}\") {{ Write-Output \"power plan verification failed; restored\"; exit 6 }} }} }}"),
        "Write-Output \"power plan verification or rollback unverified\"; exit 7".into(),
    ].join("; ");
    format!(
        "$mutex = New-Object System.Threading.Mutex($false, \"Global\\KatalaTunePowerControl\"); $held = $false; try {{ try {{ $held = $mutex.WaitOne(0) }} catch [System.Threading.AbandonedMutexException] {{ $held = $true }}; if (-not $held) {{ Write-Output \"another power action is running\"; exit 8 }}; {body}; }} finally {{ if ($held) {{ $mutex.ReleaseMutex() }}; $mutex.Dispose() }}"
    )
}

fn gpu_power_script(uuid: &str, watts: f64, min: f64, max: f64, prev_w: f64) -> String {
    let body = [
        "if (-not (Get-Command Test-KatalaActionExpiry -ErrorAction SilentlyContinue)) { function Test-KatalaActionExpiry {} }".into(),
        format!("function Read-Power {{ $line = @(Invoke-KatalaGpuSmi \"--id={uuid} --query-gpu=power.min_limit,power.max_limit,power.limit --format=csv,noheader,nounits\"); if (-not $line) {{ return @() }}; try {{ $r = @(); foreach ($x in ($line -split \",\")) {{ $r += [double]::Parse($x.Trim(), [Globalization.CultureInfo]::InvariantCulture) }}; if ($r.Count -ne 3) {{ return @() }}; return $r }} catch {{ return @() }} }}"),
        "function Read-Power-Retry { $v = @(Read-Power); if ($v.Count -ne 3) { Start-Sleep -Milliseconds 200; $v = @(Read-Power) }; return $v }".into(),
        gpu_active_guard_ps(None),
        "$v = @(Read-Power-Retry); if ($v.Count -ne 3) { Write-Output \"GPU power unreadable\"; exit 6 }".into(),
        format!("if ($v[0] -ne {min} -or $v[1] -ne {max} -or $v[2] -ne {prev_w}) {{ Write-Output \"GPU power state changed\"; exit 3 }}"),
        gpu_active_guard_ps(None),
        "$beforeApply = @(Read-Power-Retry); if ($beforeApply.Count -ne 3) { Write-Output \"GPU power unreadable\"; exit 6 }".into(),
        format!("if ($beforeApply[0] -ne {min} -or $beforeApply[1] -ne {max} -or $beforeApply[2] -ne {prev_w}) {{ Write-Output \"GPU power state changed\"; exit 3 }}; Test-KatalaGpuDeadline"),
        format!("Test-KatalaActionExpiry; $script:KatalaGpuMutationStarted = $true; $applyRows = @(Invoke-KatalaGpuSmi \"--id={uuid} -pl {watts}\"); $apply = $script:KatalaGpuSmiExitCode; if ($script:KatalaGpuSmiTimedOut) {{ Write-Output \"GPU power apply outcome unverified\"; exit 7 }}"),
        "$after = @(Read-Power-Retry)".into(),
        format!("if ($apply -eq 0 -and $after.Count -eq 3 -and $after[2] -eq {watts}) {{ exit 0 }}"),
        format!("if ($after.Count -eq 3 -and $after[0] -eq {min} -and $after[1] -eq {max} -and $after[2] -eq {prev_w}) {{ Write-Output \"power limit was not changed\"; exit 6 }}"),
        format!("if ($after.Count -eq 3 -and $after[2] -eq {watts} -and $after[0] -le {prev_w} -and $after[1] -ge {prev_w}) {{ Test-KatalaActionExpiry; Test-KatalaGpuDeadline; $rollbackRows = @(Invoke-KatalaGpuSmi \"--id={uuid} -pl {prev_w}\"); if ($script:KatalaGpuSmiTimedOut) {{ Write-Output \"GPU power rollback outcome unverified\"; exit 7 }}; if ($script:KatalaGpuSmiExitCode -eq 0) {{ $rolled = @(Read-Power-Retry); if ($rolled.Count -eq 3 -and $rolled[2] -eq {prev_w}) {{ Write-Output \"power limit verification failed; restored\"; exit 6 }} }} }}"),
        "Write-Output \"power limit verification or rollback unverified\"; exit 7".into(),
    ].join("; ");
    format!(
        "$mutex = New-Object System.Threading.Mutex($false, \"Global\\KatalaTunePowerControl\"); $held = $false; try {{ try {{ $held = $mutex.WaitOne(0) }} catch [System.Threading.AbandonedMutexException] {{ $held = $true }}; if (-not $held) {{ Write-Output \"another power action is running\"; exit 8 }}; {body}; }} finally {{ if ($held) {{ $mutex.ReleaseMutex() }}; $mutex.Dispose() }}"
    )
}

fn low_power_mode_script(source: &str, enabled: bool, prev: bool) -> String {
    let (flag, heading) = if source == "ac" { ("-c", "AC Power") } else { ("-b", "Battery Power") };
    let desired = if enabled { 1 } else { 0 };
    let previous = if prev { 1 } else { 0 };
    let raw = |key: &str| format!("pmset -g custom | awk '/^{heading}:/{{f=1;next}} /^[^ ]/{{f=0}} f && /^[[:space:]]*{key} / {{print $2; exit}}'");
    let read = format!(
        "key=powermode; mode=$({}); [ -n \"$mode\" ] || {{ key=lowpowermode; mode=$({}); }}; if [ \"$mode\" = 0 ] || [ \"$mode\" = 1 ]; then echo \"$key:$mode\"; else exit 2; fi",
        raw("powermode"),
        raw("lowpowermode")
    );
    [
        r#"lock="/tmp/org.katala.tune-power-control.lock"; mkdir -m 700 "$lock" 2>/dev/null || { echo "another power action is running; remove only after verifying owner PID is gone"; exit 8; }; trap 'rm -f "$lock/pid"; rmdir "$lock"' EXIT; trap 'exit 7' HUP INT TERM; printf "%s\n" "$$" > "$lock/pid" || exit 6"#.into(),
        format!("before=$({read}) || {{ echo \"low power mode unsupported\"; exit 6; }}; before_key=${{before%%:*}}; before_mode=${{before#*:}}"),
        format!("[ \"$before_mode\" = {previous} ] || {{ echo \"low power mode changed\"; exit 3; }}"),
        format!("pmset {flag} \"$before_key\" {desired}; apply=$?"),
        format!("after=$({read}) || {{ echo \"low power mode verification unverified\"; exit 7; }}; after_key=${{after%%:*}}; after_mode=${{after#*:}}"),
        format!("[ \"$apply\" = 0 ] && [ \"$after_key\" = \"$before_key\" ] && [ \"$after_mode\" = {desired} ] && exit 0"),
        format!(r#"[ "$after_key" = "$before_key" ] && [ "$after_mode" = {previous} ] && exit 6"#),
        format!(r#"if [ "$after_key" = "$before_key" ] && [ "$after_mode" = {desired} ]; then pmset {flag} "$before_key" {previous}; rolled=$({read}); [ "$rolled" = "$before_key:{previous}" ] && {{ echo "low power mode apply failed; restored"; exit 6; }}; fi"#),
        "echo \"low power mode verification or rollback unverified\"; exit 7".into(),
    ].join("\n")
}

fn check_task(p: &Value) -> Result<(String, String), String> {
    let path = str_of(p.get("path")).filter(|s| TASK_PATH.is_match(s)).ok_or("タスクのパスが不正")?;
    let name = str_of(p.get("name")).filter(|s| TASK_NAME.is_match(s)).ok_or("タスク名が不正")?;
    if path.len() >= 11 && path[..11].eq_ignore_ascii_case("\\Microsoft\\") {
        return Err("Windows 標準のタスクは扱わない".into());
    }
    Ok((path.into(), name.into()))
}

fn check_label(p: &Value) -> Result<(String, Option<String>), String> {
    let label = str_of(p.get("label")).filter(|s| LABEL.is_match(s) && !s.starts_with("com.apple.")).ok_or("ラベルが不正")?;
    let plist = match p.get("plist") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if LAUNCH_AGENTS.is_match(s) => Some(s.clone()),
        Some(_) => return Err("plist はユーザーの LaunchAgents のものだけ".into()),
    };
    Ok((label.into(), plist))
}

/// 許可リストにある操作の種類
pub const TYPES: &[&str] = &[
    "kill-process",
    "set-power-plan",
    "set-gpu-power-limit",
    "set-low-power-mode",
    "task-disable",
    "task-enable",
    "task-run",
    "launchd-unload",
    "launchd-load",
    "launchd-kickstart",
];

/// 計画を立てる。protect は実行の直前に読み直した保護リスト（読めなかったら None を渡す＝実行しない）
pub fn plan(node: &Node, action: &Value, protect: Option<&[String]>) -> Result<Plan, String> {
    let ty = action.get("type").map(|v| js::string(Some(v))).unwrap_or_else(|| "undefined".into());
    if !TYPES.contains(&ty.as_str()) {
        return Err(format!("許可リストに無い操作: {ty}"));
    }
    if node.shared {
        return Err(format!("{} は共用機のため、このアプリからは変更しない", node.id));
    }
    if let Some(refused) = node.refusal() {
        return Err(refused);
    }
    let windows_only = matches!(ty.as_str(), "set-power-plan" | "set-gpu-power-limit" | "task-disable" | "task-enable" | "task-run");
    let mac_only = ty == "set-low-power-mode" || ty.starts_with("launchd-");
    if windows_only && !node.is_windows() {
        return Err("Windows だけの操作".into());
    }
    if mac_only && !node.is_mac() {
        return Err("macOS だけの操作".into());
    }
    let Some(protect) = protect else { return Err("保護リストを読めないので実行しない".into()) };
    if node.get("protect_gpu") == Some(&Value::Bool(true)) && matches!(ty.as_str(), "kill-process" | "set-gpu-power-limit") {
        return Err(format!("{} は GPU 学習保護のため、この操作を実行しない", node.id));
    }
    let empty = Value::Object(Default::default());
    let p = action.get("params").filter(|v| js::truthy(Some(v))).unwrap_or(&empty);
    let id = &node.id;
    match ty.as_str() {
        "kill-process" => {
            let pid = int_of(p.get("pid")).filter(|&x| x > 4).ok_or("PID が不正")?;
            let name = str_of(p.get("name")).filter(|s| PROC_NAME.is_match(s) && !s.contains('\'')).ok_or("プロセス名が不正")?;
            if is_protected(name, protect) {
                return Err(format!("{name} は保護対象（終了しない）"));
            }
            // min_cpu = 0 は負荷の再確認をしない（プロセス一覧から操作者が明示的に選んだとき）
            let min_cpu = match p.get("min_cpu") {
                None => 50,
                v => int_of(v).filter(|x| (0..=10000).contains(x)).ok_or("負荷のしきい値が不正")?,
            };
            let start = match p.get("start") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) if (if node.is_mac() { &MAC_LSTART } else { &WIN_START }).is_match(s) => Some(s.as_str()),
                Some(_) => return Err("起動時刻が不正".into()),
            };
            Ok(Plan {
                describe: format!(
                    "{id} の {name}（PID {pid}）を終了する。直前に同じプロセスか{}を確かめ、違えば中止する。保存していない作業は失われ、元には戻せない。",
                    if min_cpu != 0 { format!("・まだ重いか（1コアの {min_cpu}% 以上）") } else { String::new() }
                ),
                script: if node.is_mac() { mac_kill_script(pid, name, start, min_cpu) } else { win_kill_ps(pid, name, start, min_cpu) },
                shell: if node.is_mac() { "sh" } else { "ps" },
                undo: None,
                has_exits: true,
            })
        }
        "set-power-plan" => {
            let guid = str_of(p.get("guid")).filter(|s| GUID.is_match(s)).ok_or("GUID が不正")?;
            let prev = str_of(p.get("prev_guid")).filter(|s| GUID.is_match(s)).ok_or("GUID が不正")?;
            Ok(Plan {
                describe: format!("{id} の電源プランを切り替える。直前のプランを再読し、適用後に検証できなければ戻す。"),
                script: power_plan_script(guid, prev),
                shell: "ps",
                undo: Some(json!({ "type": "set-power-plan", "params": { "guid": prev, "prev_guid": guid } })),
                has_exits: false,
            })
        }
        "set-gpu-power-limit" => {
            let uuid = str_of(p.get("uuid")).filter(|s| NVIDIA_UUID.is_match(s)).ok_or("GPU UUID が不正")?;
            let watts_value = watts(p.get("watts")).ok_or("GPU 電力の値が不正")?;
            let min = watts(p.get("min")).ok_or("GPU 電力の値が不正")?;
            let max = watts(p.get("max")).ok_or("GPU 電力の値が不正")?;
            let prev_w = watts(p.get("prev_w")).ok_or("GPU 電力の値が不正")?;
            if min > max || watts_value < min || watts_value > max || prev_w < min || prev_w > max {
                return Err("GPU 電力の値が不正".into());
            }
            Ok(Plan {
                describe: format!("{id} の GPU 電力上限を {watts_value}W にする。直前に同じ GPU の対応範囲と現在値を確認し、検証できなければ戻す。"),
                script: gpu_power_script(uuid, watts_value, min, max, prev_w),
                shell: "ps",
                undo: Some(
                    json!({ "type": "set-gpu-power-limit", "params": { "uuid": uuid, "watts": prev_w, "min": min, "max": max, "prev_w": watts_value } }),
                ),
                has_exits: false,
            })
        }
        "set-low-power-mode" => {
            let source = str_of(p.get("source")).filter(|s| *s == "ac" || *s == "battery").ok_or("電源ドメインが不正")?;
            let enabled = p.get("enabled").and_then(Value::as_bool).ok_or("真偽値が不正")?;
            let prev = p.get("prev").and_then(Value::as_bool).ok_or("真偽値が不正")?;
            Ok(Plan {
                describe: format!(
                    "{id} の{}時の低電力モードを{}にする。直前の値が変わっていれば実行しない。",
                    if source == "ac" { "AC" } else { "バッテリー" },
                    if enabled { "有効" } else { "無効" }
                ),
                script: low_power_mode_script(source, enabled, prev),
                shell: "sh",
                undo: Some(json!({ "type": "set-low-power-mode", "params": { "source": source, "enabled": prev, "prev": enabled } })),
                has_exits: false,
            })
        }
        "task-disable" | "task-enable" | "task-run" => {
            let (path, name) = check_task(p)?;
            let state = format!("(Get-ScheduledTask -TaskPath \"{path}\" -TaskName \"{name}\").State");
            let (describe, script, undo) = match ty.as_str() {
                "task-disable" => (
                    format!("{id} のタスク「{path}{name}」を無効にする（予定どおりに動かなくなる）。元に戻せる。"),
                    format!("Disable-ScheduledTask -TaskPath \"{path}\" -TaskName \"{name}\" | Out-Null; {state}"),
                    Some(json!({ "type": "task-enable", "params": { "path": path, "name": name } })),
                ),
                "task-enable" => (
                    format!("{id} のタスク「{path}{name}」を有効にする。元に戻せる。"),
                    format!("Enable-ScheduledTask -TaskPath \"{path}\" -TaskName \"{name}\" | Out-Null; {state}"),
                    Some(json!({ "type": "task-disable", "params": { "path": path, "name": name } })),
                ),
                _ => (
                    format!("{id} のタスク「{path}{name}」を今すぐ1回実行する。中身（バックアップ・同期など）が実際に動く。元には戻せない。"),
                    format!("Start-ScheduledTask -TaskPath \"{path}\" -TaskName \"{name}\"; Start-Sleep -Seconds 2; {state}"),
                    None,
                ),
            };
            Ok(Plan { describe, script, shell: "ps", undo, has_exits: false })
        }
        _ => {
            let (label, plist) = check_label(p)?;
            match ty.as_str() {
                "launchd-unload" => {
                    let plist = plist.ok_or("元に戻すための plist のパスが要る")?;
                    Ok(Plan {
                        describe: format!("{id} の launchd ジョブ {label} を止めて読み込みを外す（次のログインまで、または元に戻すまで動かない）。"),
                        script: format!("launchctl bootout gui/$(id -u)/{label} && echo unloaded"),
                        shell: "sh",
                        undo: Some(json!({ "type": "launchd-load", "params": { "label": label, "plist": plist } })),
                        has_exits: false,
                    })
                }
                "launchd-load" => {
                    let plist = plist.ok_or("plist のパスが要る")?;
                    Ok(Plan {
                        describe: format!("{id} の launchd ジョブ {label} を読み込む。"),
                        script: format!("launchctl bootstrap gui/$(id -u) '{plist}' && echo loaded"),
                        shell: "sh",
                        undo: Some(json!({ "type": "launchd-unload", "params": { "label": label, "plist": plist } })),
                        has_exits: false,
                    })
                }
                _ => Ok(Plan {
                    describe: format!("{id} の launchd ジョブ {label} を今すぐ1回実行する（動いていれば再起動しない）。中身が実際に動く。"),
                    script: format!("launchctl kickstart gui/$(id -u)/{label} && echo started"),
                    shell: "sh",
                    undo: None,
                    has_exits: false,
                }),
            }
        }
    }
}

/// 実際に送るコマンド。Windows のリモートは Git Bash 経由なので PowerShell を '...' で包む
pub fn wrap(node: &Node, p: &Plan) -> Result<String, String> {
    if p.shell == "sh" || node.local {
        return Ok(p.script.clone());
    }
    if p.script.contains('\'') {
        return Err("PowerShell 本体にシングルクォートは使えない".into());
    }
    Ok(format!("powershell.exe -NoProfile -NonInteractive -Command '{}'", p.script))
}

/// 実行の結果（main.js の execute の戻り値と同じ形）
#[derive(Clone, Debug)]
pub struct Outcome {
    pub ok: bool,
    pub code: Option<i32>,
    pub outcome: String,
    pub output: String,
    pub undo: Option<Value>,
}

/// 計画を立て直してから実行する（ここでも許可リスト・共用機・保護リスト・引数を検証する）
pub async fn execute(node: &Node, action: &Value, protect: Option<&[String]>) -> Result<Outcome, String> {
    execute_with(&System, node, action, protect).await
}

/// `execute` の子プロセスの実行口を差し替えられる版（テストは偽の実行器を渡す。承認なしで呼ばれていないことを数えるため）
pub async fn execute_with(runner: &dyn Runner, node: &Node, action: &Value, protect: Option<&[String]>) -> Result<Outcome, String> {
    let p = plan(node, action, protect)?;
    let stdin_transport =
        !node.local && node.os == "windows" && matches!(action.get("type").and_then(Value::as_str), Some("kill-process" | "set-gpu-power-limit"));
    let res = if stdin_transport {
        let input = action_input(&p.script)?;
        exec_on_with_input(runner, node, &action_receiver_command(), &input, Duration::from_secs(30)).await
    } else {
        let cmd = wrap(node, &p)?;
        exec_on_with(runner, node, &cmd, Duration::from_secs(30)).await
    };
    let ok = res.code == Some(0);
    let outcome = res
        .code
        .filter(|_| p.has_exits || res.code == Some(9))
        .and_then(exit_text)
        .map(str::to_string)
        .unwrap_or_else(|| if ok { "完了".into() } else { format!("失敗（exit {}）", res.code_str()) });
    let output = js::slice16_tail(js::trim(&(res.out.clone() + &res.err)), 1500);
    Ok(Outcome {
        ok,
        code: res.code,
        outcome,
        output,
        undo: if ok || (res.code == Some(7) && action.get("type").and_then(Value::as_str) != Some("set-gpu-power-limit")) { p.undo.clone() } else { None },
    })
}
