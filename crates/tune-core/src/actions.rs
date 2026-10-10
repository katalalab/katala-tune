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

use crate::collect::{Runner, System, exec_on_with};
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
static TASK_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\\([A-Za-z0-9_ .-]+\\)*$").expect("TASK_PATH"));
static TASK_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_ .()+-]{1,120}$").expect("TASK_NAME"));
static LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.-]{1,150}$").expect("LABEL"));
/// ユーザーの LaunchAgents の plist（rules の「登録し直す」コマンドでも使う）
pub(crate) static LAUNCH_AGENTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/Users/[A-Za-z0-9_.-]+/Library/LaunchAgents/[A-Za-z0-9_.-]+\.plist$").expect("LAUNCH_AGENTS"));

/// 終了スクリプトの結果コード
pub fn exit_text(code: i32) -> Option<&'static str> {
    Some(match code {
        0 => "終了を確認した",
        3 => "別のプロセスに入れ替わっていたので中止した",
        4 => "負荷が回復していたので中止した",
        5 => "終了を指示したが5秒以内に終了を確認できなかった（終了未確認）",
        6 => "終了の指示が失敗した",
        _ => return None,
    })
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
        format!("$p = Get-Process -Id {pid} -ErrorAction SilentlyContinue"),
        format!("if (-not $p -or $p.ProcessName -ne \"{name}\") {{ Write-Output (\"PID {pid} is now \" + $p.ProcessName); exit 3 }}"),
    ];
    if let Some(s) = start {
        parts.push(format!("if ($p.StartTime.ToUniversalTime().ToString(\"yyyyMMddHHmmssfff\") -ne \"{s}\") {{ Write-Output \"PID {pid} has a different start time\"; exit 3 }}"));
    }
    parts.extend([
        "$c1 = $p.CPU; Start-Sleep -Milliseconds 1000; $p.Refresh(); $core = ($p.CPU - $c1) * 100".to_string(),
        format!("if ($core -lt {min_cpu}) {{ Write-Output (\"recovered: \" + [math]::Round($core) + \"% of one core\"); exit 4 }}"),
        format!("try {{ Stop-Process -Id {pid} -Force -ErrorAction Stop }} catch {{ Write-Output $_.Exception.Message; exit 6 }}"),
        format!("try {{ Wait-Process -Id {pid} -Timeout 5 -ErrorAction Stop }} catch {{ if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ Write-Output \"not confirmed within 5s\"; exit 5 }} }}"),
        "Write-Output \"terminated\"; exit 0".into(),
    ]);
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
        format!("if ($after -eq \"{guid}\") {{ & powercfg.exe -setactive {prev_guid}; if ($LASTEXITCODE -eq 0) {{ $rolled = Read-Plan-Retry; if ($rolled -eq \"{prev_guid}\") {{ Write-Output \"power plan verification failed; restored\"; exit 6 }} }} }}"),
        "Write-Output \"power plan verification or rollback unverified\"; exit 7".into(),
    ].join("; ");
    format!(
        "$mutex = New-Object System.Threading.Mutex($false, \"Global\\KatalaTunePowerControl\"); $held = $false; try {{ try {{ $held = $mutex.WaitOne(0) }} catch [System.Threading.AbandonedMutexException] {{ $held = $true }}; if (-not $held) {{ Write-Output \"another power action is running\"; exit 8 }}; {body}; }} finally {{ if ($held) {{ $mutex.ReleaseMutex() }}; $mutex.Dispose() }}"
    )
}

fn gpu_power_script(uuid: &str, watts: f64, min: f64, max: f64, prev_w: f64) -> String {
    let body = [
        format!("function Read-Power {{ $line = & nvidia-smi.exe --id={uuid} --query-gpu=power.min_limit,power.max_limit,power.limit --format=csv,noheader,nounits; if ($LASTEXITCODE -ne 0 -or -not $line) {{ return @() }}; try {{ $r = @(); foreach ($x in ($line -split \",\")) {{ $r += [double]::Parse($x.Trim(), [Globalization.CultureInfo]::InvariantCulture) }}; if ($r.Count -ne 3) {{ return @() }}; return $r }} catch {{ return @() }} }}"),
        "function Read-Power-Retry { $v = @(Read-Power); if ($v.Count -ne 3) { Start-Sleep -Milliseconds 200; $v = @(Read-Power) }; return $v }".into(),
        "$v = @(Read-Power-Retry); if ($v.Count -ne 3) { Write-Output \"GPU power unreadable\"; exit 6 }".into(),
        format!("if ($v[0] -ne {min} -or $v[1] -ne {max} -or $v[2] -ne {prev_w}) {{ Write-Output \"GPU power state changed\"; exit 3 }}"),
        format!("& nvidia-smi.exe --id={uuid} -pl {watts}; $apply = $LASTEXITCODE"),
        "$after = @(Read-Power-Retry)".into(),
        format!("if ($apply -eq 0 -and $after.Count -eq 3 -and $after[2] -eq {watts}) {{ exit 0 }}"),
        format!("if ($after.Count -eq 3 -and $after[2] -eq {watts} -and $after[0] -le {prev_w} -and $after[1] -ge {prev_w}) {{ & nvidia-smi.exe --id={uuid} -pl {prev_w}; if ($LASTEXITCODE -eq 0) {{ $rolled = @(Read-Power-Retry); if ($rolled.Count -eq 3 -and $rolled[2] -eq {prev_w}) {{ Write-Output \"power limit verification failed; restored\"; exit 6 }} }} }}"),
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
        "key=powermode; mode=$({}); [ -n \"$mode\" ] || {{ key=lowpowermode; mode=$({}); }}; case \"$mode\" in 0|1) echo \"$key:$mode\";; *) return 2;; esac",
        raw("powermode"),
        raw("lowpowermode")
    );
    [
        "lock=\"$HOME/.katala-tune/power-action.lock\"; mkdir -p -m 700 \"$HOME/.katala-tune\" || exit 6; mkdir \"$lock\" 2>/dev/null || { echo \"another power action is running; remove only after verifying owner PID is gone\"; exit 8; }; printf \"%s\\n\" \"$$\" > \"$lock/pid\"; trap \"rm -f \\\"$lock/pid\\\"; rmdir \\\"$lock\\\"\" EXIT HUP INT TERM".into(),
        format!("before=$({read}) || {{ echo \"low power mode unsupported\"; exit 6; }}; before_key=${{before%%:*}}; before_mode=${{before#*:}}"),
        format!("[ \"$before_mode\" = {previous} ] || {{ echo \"low power mode changed\"; exit 3; }}"),
        format!("pmset {flag} \"$before_key\" {desired}; apply=$?"),
        format!("after=$({read}) || {{ echo \"low power mode verification unverified\"; exit 7; }}; after_key=${{after%%:*}}; after_mode=${{after#*:}}"),
        format!("[ \"$apply\" = 0 ] && [ \"$after_key\" = \"$before_key\" ] && [ \"$after_mode\" = {desired} ] && exit 0"),
        format!("[ \"$after_key\" = \"$before_key\" ] && [ \"$after_mode\" = {desired} ] && pmset {flag} \"$before_key\" {previous}"),
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
    let windows_only = matches!(ty.as_str(), "set-power-plan" | "set-gpu-power-limit" | "task-disable" | "task-enable" | "task-run");
    let mac_only = ty == "set-low-power-mode" || ty.starts_with("launchd-");
    if windows_only && !node.is_windows() {
        return Err("Windows だけの操作".into());
    }
    if mac_only && !node.is_mac() {
        return Err("macOS だけの操作".into());
    }
    let Some(protect) = protect else { return Err("保護リストを読めないので実行しない".into()) };
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
    let cmd = wrap(node, &p)?;
    let res = exec_on_with(runner, node, &cmd, Duration::from_secs(30)).await;
    let ok = res.code == Some(0);
    let outcome = res
        .code
        .filter(|_| p.has_exits)
        .and_then(exit_text)
        .map(str::to_string)
        .unwrap_or_else(|| if ok { "完了".into() } else { format!("失敗（exit {}）", res.code_str()) });
    let output = js::slice16_tail(js::trim(&(res.out.clone() + &res.err)), 1500);
    Ok(Outcome { ok, code: res.code, outcome, output, undo: if ok || res.code == Some(7) { p.undo.clone() } else { None } })
}
