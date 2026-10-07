//! snapshot（probe の出力）から所見と提案を作る。副作用なし。仕様は lib/rules.js（同じ入力に同じ出力）。
//! finding = { id, severity: critical|warn|info, category, title, detail, advice, commands?, action? }

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use crate::js::{self, Jv, Obj, arr, get, num, or, present, string, to_fixed, truthy};

pub const HIGH_PERF_GUID: &str = "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c";
pub const BALANCED_GUID: &str = "381b4222-f694-41f0-9685-ff5bb260df2e";
pub const POWER_SAVER_GUID: &str = "a1841308-3541-4fab-bc81-f71556f20b4a";
pub const SHARED_BLOCKED: &str = "共用機のため、この画面からは実行しない（持ち主と相談）";

/// JS の `.`（行末記号以外の1文字）
const DOT: &str = r"[^\n\r\x{2028}\x{2029}]";

// 終了の提案を出さないプロセス。OS の中核、セキュリティ、作業中の AI エージェントや実行環境。
// JS は `new RegExp('^(...)$', 'i')`（ASCII だけの大文字小文字無視）なので、入力を ASCII 小文字にして小文字のパターンに当てる
const NO_KILL_PARTS: &[&str] = &[
    "kernel_task",
    "launchd",
    "WindowServer",
    "loginwindow",
    "Finder",
    "Dock",
    "SystemUIServer",
    "mds",
    "mds_stores",
    "mdworker.*",
    "fileproviderd",
    "cloudd",
    "bird",
    "coreaudiod",
    "JamfDaemon",
    "Jamf.*",
    r"com\.apple\..*",
    "Virtualization.*",
    "System",
    "Idle",
    "Registry",
    "smss",
    "csrss",
    "wininit",
    "services",
    "lsass",
    "svchost",
    "winlogon",
    "dwm",
    "explorer",
    "MsMpEng",
    "NisSrv",
    "SecurityHealth.*",
    "Memory Compression",
    "vmmem.*",
    "vmcompute",
    "WmiPrvSE",
    "fontdrvhost",
    "sihost",
    "ctfmon",
    "audiodg",
    "spoolsv",
    "conhost",
    "SearchIndexer",
    "TiWorker",
    "TrustedInstaller",
    "MsSense",
    "sshd",
    "ssh.*",
    "bash",
    "zsh",
    "powershell",
    "pwsh",
    r"python3?(\.[0-9]+)?",
    "node",
    "claude",
    "codex",
    "opencode",
    "cursor-agent",
    "agy",
    "op",
    "op-agent",
    "1Password.*",
    "tailscale.*",
    "Tailscale.*",
    "docker.*",
    r"com\.docker\..*",
    "colima",
    "limactl",
    "qemu.*",
    "ollama.*",
    "llama-server",
    "nvcontainer",
    "NVDisplay.*",
];

static NO_KILL: LazyLock<Regex> = LazyLock::new(|| {
    let alt = NO_KILL_PARTS.iter().map(|p| p.to_ascii_lowercase().replace(".*", &format!("{DOT}*"))).collect::<Vec<_>>().join("|");
    Regex::new(&format!("^({alt})$")).expect("NO_KILL")
});

/// 終了を提案しないプロセスか（`NO_KILL.test(name)`）
pub fn no_kill(name: &str) -> bool {
    NO_KILL.is_match(&name.to_ascii_lowercase())
}

// 名前ごとの対処の知見（app 名かプロセス名に当てる）
static ADVICE: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    let d = |p: &str| Regex::new(&p.replace(".*", &format!("{DOT}*"))).expect("ADVICE");
    vec![
        (
            d("^Google Drive$|^fileproviderd$"),
            "Google Drive for desktop の同期が CPU を使い続けている。Drive を終了して再起動し、それでも続くならミラーリング対象を減らすかストリーミングに切り替える。",
        ),
        (
            d("^biomesyncd$|^BiomeAgent$"),
            "macOS の利用状況（Screen Time 等）の同期。iCloud 同期の再試行で張り付くことがある。数時間続くなら再ログインか再起動で収まる。",
        ),
        (
            d("^(Google Chrome|chrome)$"),
            "タブとプロファイルの数に比例する。chrome://settings/performance の「メモリセーバー」を有効にし、使っていないプロファイルのウィンドウを閉じる。",
        ),
        (
            d("^vmmemWSL$|^vmmem$"),
            "WSL の VM。使い終わったら `wsl --shutdown` でメモリを返す。常時大きいなら %UserProfile%\\.wslconfig の memory で上限を付ける。",
        ),
        (
            d("^(Discord|Slack|Microsoft Teams|ChatGPT|Claude|Notion|Spotify)$"),
            "Electron 系アプリ。開きっぱなしのワークスペースやウィンドウが多いほど重い。使わない時間帯は終了する。",
        ),
        (
            d("^WindowServer$"),
            "画面描画。外部ディスプレイの高解像度スケーリングや、透明効果・大量のウィンドウで上がる。「視差効果を減らす」「透明度を下げる」で軽くなる。",
        ),
        (d("^(mds|mds_stores|mdworker.*|SearchIndexer)$"), "検索インデックス作成。node_modules やリポジトリ群を検索対象から外すと収まる。"),
        (
            d("^MsMpEng$"),
            "Defender のリアルタイム保護がビルドや git 操作のファイルを全部検査している。開発ディレクトリを除外リストに加える（管理者権限が要る）。",
        ),
        (
            d("^PresentMon"),
            "NVIDIA FrameView SDK（nvfvsdksvc_x64.exe）が起動する計測プロセス。NVIDIA アプリの性能オーバーレイ・統計を使っていなければ、NVIDIA アプリ > 設定 > 機能でパフォーマンス監視を切ると止まる。",
        ),
        (d("^rust-analyzer$"), "エディタの Rust 解析。開いている大きなワークスペースごとに1つ動く。使っていない VS Code ウィンドウを閉じる。"),
    ]
});

fn advice_for(names: &[Jv<'_>]) -> Option<&'static str> {
    for n in names {
        if !truthy(*n) {
            continue;
        }
        let s = string(*n);
        for (re, a) in ADVICE.iter() {
            if re.is_match(&s) {
                return Some(a);
            }
        }
    }
    None
}

fn gb(mb: Jv<'_>) -> String {
    to_fixed(num(mb) / 1024.0, 1)
}

fn sev_rank(f: &Value) -> u8 {
    match f.get("severity").and_then(Value::as_str) {
        Some("critical") => 0,
        Some("warn") => 1,
        _ => 2,
    }
}

/// 重大 → 注意 → 提案 の順に並べる（安定ソート。JS の Array.prototype.sort と同じ）
pub fn sort_findings(out: &mut [Value]) {
    out.sort_by_key(sev_rank);
}

struct F(Vec<Value>);

impl F {
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        id: String,
        severity: &str,
        category: &str,
        title: String,
        detail: String,
        advice: String,
        commands: Option<Vec<String>>,
        action: Option<Value>,
    ) {
        self.0.push(
            Obj::new()
                .set("id", id)
                .set("severity", severity)
                .set("category", category)
                .set("title", title)
                .set("detail", detail)
                .set("advice", advice)
                .opt("commands", commands.map(|c| json!(c)))
                .opt("action", action)
                .build(),
        );
    }
    fn simple(&mut self, id: &str, severity: &str, category: &str, title: String, detail: String, advice: &str) {
        self.add(id.into(), severity, category, title, detail, advice.into(), None, None);
    }
}

/// 所見を作る。node は台帳の1台（shared を見る）
pub fn analyze(snap: &Value, node: &Value) -> Vec<Value> {
    let s = Some(snap);
    let mut out = F(Vec::new());
    let is_win = js::is_str(get(s, "probe"), "windows");
    let one = Value::from(1);
    let cores = num(or(get(get(s, "host"), "cores"), Some(&one)));
    let total_mb = num(or(get(get(s, "memory"), "total_gb"), Some(&Value::from(0)))) * 1024.0;
    let total_mb_truthy = total_mb != 0.0 && !total_mb.is_nan();
    let procs = or(get(s, "processes"), None);
    // Windows の cpu は機体全体に対する%（1.5秒の瞬間値）、macOS は1コアに対する%（ps の減衰平均）
    let per_core = |p: &Value| if is_win { num(p.get("cpu")) * cores } else { num(p.get("cpu")) };

    // CPU
    let cpu_busy = get(s, "cpu_busy");
    if present(cpu_busy) {
        let top = arr(get(procs, "apps_cpu"))
            .iter()
            .take(3)
            .map(|a| {
                let c = if is_win { num(a.get("cpu")) * cores } else { num(a.get("cpu")) };
                format!("{} {}%", string(a.get("app")), to_fixed(c, 0))
            })
            .collect::<Vec<_>>()
            .join("、");
        let busy = num(cpu_busy);
        if busy >= 85.0 {
            out.simple(
                "cpu-saturated",
                "critical",
                "cpu",
                format!("CPU が飽和している（{}%）", string(cpu_busy)),
                format!("上位: {top}"),
                "下の「暴走の疑い」から順に止める。止められない処理なら、その間ほかの重い作業を別の機体へ回す。",
            );
        } else if busy >= 60.0 {
            out.simple(
                "cpu-busy",
                "warn",
                "cpu",
                format!("CPU 使用率が高い（{}%）", string(cpu_busy)),
                format!("上位: {top}"),
                "常駐アプリの見直しで下がる余地がある。",
            );
        }
    }

    // 暴走の疑い（1コアを使い切っているプロセス）
    // Windows は瞬間値に加えて、起動からの平均が 1コアの 30% 以上のものだけ（一時的なスパイクを除く）
    let hogs = arr(get(procs, "top_cpu"))
        .iter()
        .filter(|p| per_core(p) >= 80.0 && (!is_win || num(js::nullish(p.get("avg_core"), Some(&Value::from(0)))) >= 30.0))
        .take(4);
    for p in hogs {
        let name = p.get("name");
        let app = p.get("app");
        let killable = !no_kill(&string(name)) && !no_kill(&if truthy(app) { string(app) } else { String::new() });
        let prefix = if truthy(app) && !js::strict_eq(app, name) { format!("{} / ", string(app)) } else { String::new() };
        let etime = p.get("etime");
        let detail = format!(
            "PID {}{}{}",
            string(p.get("pid")),
            if truthy(etime) { format!("、起動から {}", string(etime)) } else { String::new() },
            if is_win { format!("（瞬間値。起動からの平均は {}%）", string(p.get("avg_core"))) } else { "（直近の平均）".into() }
        );
        let advice = advice_for(&[app, name]).map(str::to_string).unwrap_or_else(|| {
            if killable {
                "想定外ならアプリを終了して様子を見る。".into()
            } else {
                "OS・セキュリティ、または誰かの作業（AI エージェント・python・node など）の可能性があるため、終了は提案しない。何の処理かを確かめ、原因側（同期・インデックス・ビルド・学習）を止める。".into()
            }
        });
        // min_cpu: 終了の直前に測り直して、これを下回っていたら（回復していたら）中止する
        let action = killable.then(|| {
            let params = Obj::new()
                .opt("pid", p.get("pid").cloned())
                .opt("name", name.cloned())
                .set("start", js::nullish(p.get("start"), None).cloned().unwrap_or(Value::Null))
                .set("min_cpu", 50)
                .build();
            json!({ "type": "kill-process", "label": "このプロセスを終了", "params": params })
        });
        out.add(
            format!("runaway-{}-{}", string(name), string(p.get("pid"))),
            "warn",
            "cpu",
            format!("{prefix}{} が CPU {}%（1コア換算）", string(name), to_fixed(per_core(p), 0)),
            detail,
            advice,
            None,
            action,
        );
    }

    // メモリ
    let m = get(s, "memory");
    let mg = |k: &str| get(m, k);
    if !is_win {
        let pressure_detail = || format!("圧縮 {} GB、swap {}/{} GB", string(mg("compressed_gb")), string(mg("swap_used_gb")), string(mg("swap_total_gb")));
        if js::is_str(mg("pressure"), "critical") {
            out.simple(
                "mem-pressure",
                "critical",
                "memory",
                "メモリ圧迫が危険域（critical）".into(),
                pressure_detail(),
                "メモリを多く使うアプリから閉じる。下の「メモリの大口」を参照。",
            );
        } else if js::is_str(mg("pressure"), "warn") {
            out.simple(
                "mem-pressure",
                "warn",
                "memory",
                "メモリ圧迫が警告域（warn）".into(),
                pressure_detail(),
                "swap への書き出しで体感が落ちている。メモリの大口を減らすと戻る。",
            );
        }
        let total_gb = num(or(mg("total_gb"), Some(&Value::from(0))));
        if num(mg("swap_used_gb")) >= js::max(&[4.0, total_gb * 0.15]) {
            out.simple(
                "swap",
                "warn",
                "memory",
                format!("swap を {} GB 使っている", string(mg("swap_used_gb"))),
                format!("実メモリ {} GB に対して {}%", string(mg("total_gb")), to_fixed(num(mg("swap_used_gb")) / num(mg("total_gb")) * 100.0, 0)),
                "一度大きく溜まった swap はアプリを閉じても残りやすい。大口を閉じたあと、余裕のある時間に再起動すると解消する。",
            );
        }
    } else {
        let avail = mg("available_pct");
        let low_detail = || format!("{}/{} GB", string(mg("free_gb")), string(mg("total_gb")));
        if present(avail) && num(avail) < 10.0 {
            out.simple("mem-low", "critical", "memory", format!("空きメモリ {}%", string(avail)), low_detail(), "メモリの大口を閉じる。");
        } else if present(avail) && num(avail) < 20.0 {
            out.simple("mem-low", "warn", "memory", format!("空きメモリ {}%", string(avail)), low_detail(), "メモリの大口を閉じる。");
        }
        let commit = num(mg("commit_pct"));
        if commit >= 90.0 {
            out.simple(
                "commit",
                "critical",
                "memory",
                format!("コミット済みメモリ {}%", string(mg("commit_pct"))),
                format!("ページファイル {} MB", string(mg("pagefile_alloc_mb"))),
                "上限に当たるとアプリが落ちる。大口を閉じるか、ページファイルを増やす。",
            );
        } else if commit >= 80.0 {
            out.simple(
                "commit",
                "warn",
                "memory",
                format!("コミット済みメモリ {}%", string(mg("commit_pct"))),
                format!("ページファイル {} MB（ピーク {} MB）", string(mg("pagefile_alloc_mb")), string(mg("pagefile_peak_mb"))),
                "予約だけで実際には使われていない分も含む。WSL や大きなモデルを動かすとここで詰まる。ページファイルが 4GB 固定なら「システム管理サイズ」に戻すと余裕ができる。",
            );
        }
    }
    for a in arr(get(procs, "apps")).iter().filter(|a| total_mb_truthy && num(a.get("mem_mb")) >= total_mb * 0.2).take(3) {
        out.simple(
            &format!("mem-hog-{}", string(a.get("app"))),
            "warn",
            "memory",
            format!("{} が {} GB（実メモリの {}%）", string(a.get("app")), gb(a.get("mem_mb")), to_fixed(num(a.get("mem_mb")) / total_mb * 100.0, 0)),
            format!(
                "{} プロセスの合計{}",
                string(a.get("count")),
                if is_win { "（ワーキングセット）" } else { "（RSS。共有分を重ねて数えるため実際より大きめ）" }
            ),
            advice_for(&[a.get("app")]).unwrap_or("このアプリの使い方を見直す。"),
        );
    }

    // ディスク
    for d in arr(get(s, "disk")) {
        let mount = d.get("mount");
        let sys = js::is_str(mount, "/") || js::is_c_drive(mount);
        let free = num(d.get("free_pct"));
        let title = format!("{} の空きが {}%（{} GB）", string(mount), string(d.get("free_pct")), string(d.get("free_gb")));
        let detail = format!("容量 {} GB", string(d.get("total_gb")));
        if free < 5.0 && sys {
            out.add(
                format!("disk-{}", string(mount)),
                "critical",
                "disk",
                title,
                detail,
                "システムディスクが埋まると swap とアップデートが止まり、全体が重くなる。下のキャッシュ削除コマンドから始める。".into(),
                Some(cache_commands(snap)),
                None,
            );
        } else if free < 10.0 {
            let advice = if sys {
                "10% を切ると swap の確保とアップデートに影響する。"
            } else {
                "大きなファイルの置き場所を見直す。"
            };
            out.add(format!("disk-{}", string(mount)), "warn", "disk", title, detail, advice.into(), sys.then(|| cache_commands(snap)), None);
        }
    }
    let caches: Vec<&Value> = arr(get(s, "caches")).iter().filter(|c| num(c.get("gb")) >= 2.0).collect();
    let cache_total: f64 = caches.iter().map(|c| num(c.get("gb"))).sum();
    if cache_total >= 10.0 {
        out.add(
            "caches".into(),
            "info",
            "disk",
            format!("再生成できるキャッシュが {} GB", to_fixed(cache_total, 0)),
            caches.iter().map(|c| format!("{} {} GB", string(c.get("path")), string(c.get("gb")))).collect::<Vec<_>>().join("、"),
            "消しても次に使うとき作り直される。ディスクに余裕があるなら急がない。".into(),
            Some(cache_commands(snap)),
            None,
        );
    }

    // 熱・電源
    let pw = get(s, "power");
    if !is_win {
        let limit = get(pw, "cpu_speed_limit");
        if present(limit) && num(limit) < 100.0 {
            out.simple(
                "thermal",
                "warn",
                "thermal",
                format!("熱で CPU が {}% に制限されている", string(limit)),
                "pmset -g therm".into(),
                "通気を確保し、負荷の高い処理を減らす。",
            );
        }
        if truthy(get(pw, "low_power_mode")) {
            out.simple(
                "lowpower",
                "info",
                "power",
                "低電力モードが有効".into(),
                "pmset lowpowermode 1".into(),
                "性能を優先するならシステム設定 > バッテリー（または省エネルギー）で低電力モードを切る。",
            );
        }
    } else {
        let perf = get(s, "cpu_perf_pct");
        if present(perf) && num(perf) < 70.0 && num(cpu_busy) >= 50.0 {
            out.simple(
                "clock-down",
                "warn",
                "thermal",
                format!("負荷中なのにクロックが定格の {}%", string(perf)),
                format!("CPU 使用率 {}%", string(cpu_busy)),
                "熱か電源設定で絞られている。冷却と電源プランを確認する。",
            );
        }
        let plans = get(pw, "plans");
        let has_high_perf = !truthy(plans)
            || match plans {
                Some(Value::Array(a)) => a.iter().any(|x| x.as_str() == Some(HIGH_PERF_GUID)),
                Some(Value::String(s)) => s.contains(HIGH_PERF_GUID),
                _ => false,
            };
        let guid = get(pw, "plan_guid");
        let guid_lc = string(guid).to_lowercase();
        if has_high_perf && truthy(guid) && (guid_lc == BALANCED_GUID || guid_lc == POWER_SAVER_GUID) {
            out.add(
                "power-plan".into(),
                if guid_lc == POWER_SAVER_GUID { "warn" } else { "info" },
                "power",
                format!("電源プランが「{}」", string(get(pw, "plan_name"))),
                format!("GUID {}", string(guid)),
                "1スレッドの速さはほぼ変わらない（2026-10-03 に Windows 機2台で実測して -1.6%、誤差の範囲）。効くのはコアの休止からの復帰遅延くらいで、アイドル時の消費電力は増える。遅延に敏感な処理（音声・ゲーム配信・計測）をする機体だけ試し、効かなければ元に戻す。".into(),
                None,
                Some(json!({ "type": "set-power-plan", "label": "高パフォーマンスにする", "params": Obj::new().set("guid", HIGH_PERF_GUID).opt("prev_guid", guid.cloned()).build() })),
            );
        }
        for g in arr(get(s, "gpus")) {
            if num(g.get("temp_c")) >= 85.0 {
                out.simple(
                    &format!("gpu-temp-{}", string(g.get("name"))),
                    "warn",
                    "thermal",
                    format!("{} が {}°C", string(g.get("name")), string(g.get("temp_c"))),
                    format!("使用率 {}%、{}/{} W", string(g.get("util")), string(g.get("power_w")), string(g.get("power_limit_w"))),
                    "ファン曲線とケース内の排気を見直す。電力上限を少し下げると温度が大きく下がることが多い。",
                );
            }
        }
    }

    // 安定性
    let st = get(s, "stability_7d");
    if truthy(st) {
        let zero = Value::from(0);
        let crashes = js::max(&[num(or(get(st, "bugcheck_1001"), Some(&zero))), num(or(get(st, "kernel_power_41"), Some(&zero)))]);
        let title = format!("直近7日で予期しない停止が {} 回", js::num_str(crashes));
        if crashes >= 3.0 {
            out.simple(
                "stability",
                "critical",
                "stability",
                title,
                format!(
                    "BugCheck(1001) {}、Kernel-Power(41) {}、6008 {}",
                    string(get(st, "bugcheck_1001")),
                    string(get(st, "kernel_power_41")),
                    string(get(st, "unexpected_6008"))
                ),
                "性能より先に安定性の問題。メモリ（XMP を切る・枚数を減らす）と CPU の劣化を疑う。この機体に常駐処理を増やさない。",
            );
        } else if crashes > 0.0 {
            out.simple(
                "stability",
                "warn",
                "stability",
                title,
                format!("BugCheck(1001) {}、Kernel-Power(41) {}", string(get(st, "bugcheck_1001")), string(get(st, "kernel_power_41"))),
                "停電やスリープ復帰の失敗でも記録される。続くようならダンプを解析する。",
            );
        }
    }

    // WSL / コンテナ VM
    if is_win {
        let wsl_memory = get(get(s, "wsl"), "memory");
        let vm = arr(get(procs, "apps")).iter().find(|a| string(a.get("app")).to_ascii_lowercase().starts_with("vmmem"));
        let wslconfig = |total: f64| {
            format!(
                "# %UserProfile%\\.wslconfig\n[wsl2]\nmemory={}GB\n\n[experimental]\nautoMemoryReclaim=gradual",
                js::num_str(js::max(&[8.0, js::round(total / 2.0)]))
            )
        };
        if let Some(vm) = vm.filter(|vm| total_mb_truthy && num(vm.get("mem_mb")) >= total_mb * 0.25 && !truthy(wsl_memory)) {
            out.add(
                "wsl-limit".into(),
                "warn",
                "memory",
                format!("WSL が {} GB を使っていて上限が無い", gb(vm.get("mem_mb"))),
                ".wslconfig に memory の指定が無い".into(),
                "Windows 側が足りなくなる前に上限を付ける。反映には `wsl --shutdown` が要る。".into(),
                Some(vec![wslconfig(num(get(get(s, "memory"), "total_gb")))]),
                None,
            );
        } else if !truthy(wsl_memory) && truthy(get(s, "wsl")) {
            out.add(
                "wsl-limit".into(),
                "info",
                "memory",
                "WSL のメモリ上限が未設定".into(),
                "既定では実メモリの半分まで使う".into(),
                "常用するなら上限と autoMemoryReclaim を設定しておくと、使い終わったメモリが Windows に戻る。".into(),
                Some(vec![wslconfig(num(or(get(get(s, "memory"), "total_gb"), Some(&Value::from(16)))))]),
                None,
            );
        }
        let defender = get(s, "defender");
        if matches!(get(defender, "realtime"), Some(Value::Bool(false))) {
            out.simple(
                "defender-off",
                "info",
                "security",
                "Defender のリアルタイム保護が無効".into(),
                "速さは出るが無防備".into(),
                "性能目的なら全体を切るより、開発ディレクトリだけ除外する方が安全。",
            );
        }
        if let Some(ms) = arr(get(procs, "top_cpu")).iter().find(|p| js::is_str(p.get("name"), "MsMpEng"))
            && per_core(ms) >= 30.0
        {
            let excl = js::nullish(get(defender, "exclusions"), None).map(|v| string(Some(v))).unwrap_or_else(|| "不明（管理者権限が要る）".into());
            out.add(
                "defender-cpu".into(),
                "warn",
                "cpu",
                format!("Defender が CPU {}%（1コア換算）", to_fixed(per_core(ms), 0)),
                format!("除外 {excl} 件"),
                advice_for(&[Some(&Value::from("MsMpEng"))]).unwrap_or_default().into(),
                Some(vec!["# 管理者 PowerShell で（パスは自分の開発ディレクトリへ）\nAdd-MpPreference -ExclusionPath \"$env:USERPROFILE\\ghq\"".into()]),
                None,
            );
        }
        let items = arr(get(s, "startup_items"));
        if items.len() >= 20 {
            out.simple(
                "startup",
                "info",
                "background",
                format!("スタートアップ項目が {} 件", items.len()),
                js::join(&items[..12.min(items.len())], "、"),
                "タスクマネージャー > スタートアップ で使わないものを無効にする。",
            );
        }
    } else {
        let c = get(s, "containers");
        let total_gb = mg("total_gb");
        for v in arr(get(c, "colima")) {
            if js::is_str(v.get("status"), "Running") && truthy(total_gb) && num(v.get("memory_gb")) >= num(total_gb) * 0.35 {
                let mem = js::max(&[2.0, js::round(num(total_gb) / 8.0)]);
                let cpu = js::max(&[2.0, js::round(cores / 3.0)]);
                out.add(
                    format!("colima-{}", string(v.get("name"))),
                    "warn",
                    "memory",
                    format!("colima の VM に {} GB を割り当てている", string(v.get("memory_gb"))),
                    format!("実メモリ {} GB、CPU {}", string(total_gb), string(v.get("cpus"))),
                    "割り当てた分はコンテナが使っていなくても macOS から見えにくくなる。必要量まで減らす（VM の再起動が要る）。".into(),
                    Some(vec![format!(
                        "colima stop {} && colima start {} --memory {} --cpu {}",
                        string(v.get("name")),
                        string(v.get("name")),
                        js::num_str(mem),
                        js::num_str(cpu)
                    )]),
                    None,
                );
            }
        }
        let dd = get(c, "docker_desktop");
        if truthy(dd) && truthy(total_gb) && num(get(dd, "memory_gb")) >= num(total_gb) * 0.35 {
            out.simple(
                "docker-desktop",
                "warn",
                "memory",
                format!("Docker Desktop に {} GB を割り当てている", string(get(dd, "memory_gb"))),
                format!("実メモリ {} GB", string(total_gb)),
                "Docker Desktop > Settings > Resources で減らす。",
            );
        }
        if truthy(get(s, "time_machine_running")) {
            out.simple(
                "tm",
                "info",
                "background",
                "Time Machine のバックアップ中".into(),
                "tmutil status".into(),
                "終わるまで I/O が重い。急ぎの作業中なら一時停止してよい。",
            );
        }
    }

    // 共通
    let uptime = get(get(s, "host"), "uptime_h");
    if num(uptime) >= 24.0 * 14.0 {
        out.simple(
            "uptime",
            "info",
            "background",
            format!("{} 日間再起動していない", js::num_str((num(uptime) / 24.0).floor())),
            String::new(),
            "swap・圧縮メモリ・漏れたプロセスは再起動でまとめて片付く。区切りの良いところで。",
        );
    }
    let agents = get(procs, "agent_processes");
    if num(or(agents, Some(&Value::from(0)))) >= 50.0 {
        out.simple(
            "agents",
            "info",
            "background",
            format!("AI エージェントのプロセスが {} 個", string(agents)),
            "claude / codex / opencode などの合計".into(),
            "終わったセッションが残っていないか確認する。1つずつのメモリは小さくても積み上がる。",
        );
    }
    let b = get(s, "bench");
    let runs = get(b, "runs_ms");
    let runs_len = match runs {
        Some(Value::Array(a)) => a.len() as f64,
        Some(Value::String(x)) => js::len16(x) as f64,
        _ => f64::NAN,
    };
    if runs_len >= 3.0 {
        let xs: Vec<f64> = arr(runs).iter().map(|x| num(Some(x))).collect();
        let spread = (js::max(&xs) - js::min(&xs)) / num(get(b, "median_ms"));
        if spread >= 0.25 {
            out.simple(
                "bench-noise",
                "info",
                "cpu",
                format!("計測のばらつきが {}%", to_fixed(spread * 100.0, 0)),
                format!("{} ms", js::join(arr(runs), " / ")),
                "裏で断続的に重い処理が走っている。CPU 上位を見て原因を探す。",
            );
        }
    }

    // 共用機では実行を止めて提案だけにする
    let mut out = out.0;
    if truthy(get(Some(node), "shared")) {
        block_actions(&mut out);
    }
    sort_findings(&mut out);
    out
}

/// 所見の実行ボタンを止める（共用機）
pub fn block_actions(findings: &mut [Value]) {
    for f in findings {
        if let Some(Value::Object(a)) = f.get_mut("action") {
            a.insert("blocked".into(), Value::from(SHARED_BLOCKED));
        }
    }
}

fn cache_commands(snap: &Value) -> Vec<String> {
    let s = Some(snap);
    if js::is_str(get(s, "probe"), "windows") {
        return vec!["cleanmgr /sageset:1  # 対象を選ぶ\ncleanmgr /sagerun:1".into(), "npm cache clean --force".into()];
    }
    let have: Vec<Option<&Value>> = arr(get(s, "caches")).iter().filter(|c| num(c.get("gb")) >= 1.0).map(|c| c.get("path")).collect();
    let has = |p: &str| have.iter().any(|x| js::is_str(*x, p));
    let mut cmds: Vec<String> = Vec::new();
    if has("~/Library/Developer/CoreSimulator/Devices") {
        cmds.push("xcrun simctl delete unavailable".into());
    }
    if has("~/Library/Developer/Xcode/DerivedData") {
        cmds.push("rm -rf ~/Library/Developer/Xcode/DerivedData/*".into());
    }
    if has("~/.npm/_cacache") {
        cmds.push("npm cache clean --force".into());
    }
    if has("~/Library/Caches/Homebrew") {
        cmds.push("brew cleanup --prune=all".into());
    }
    let colima_len = match get(get(s, "containers"), "colima") {
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::String(x)) => !x.is_empty(),
        _ => false,
    };
    if has("~/.colima") || colima_len {
        cmds.push("docker system df   # 確認してから\ndocker image prune -a".into());
    }
    if truthy(get(get(s, "containers"), "docker_desktop")) {
        cmds.push("docker system df   # 確認してから\ndocker builder prune".into());
    }
    cmds.push("du -sh ~/Library/Caches/* 2>/dev/null | sort -h | tail -15   # 大きいものを確認".into());
    cmds
}

/// スコア（重大 25・注意 8・提案 1 を 100 から引く）
pub fn score(findings: &[Value]) -> i64 {
    let pen: i64 = findings
        .iter()
        .map(|f| match f.get("severity").and_then(Value::as_str) {
            Some("critical") => 25,
            Some("warn") => 8,
            Some("info") => 1,
            _ => 0,
        })
        .sum();
    (100 - pen).max(0)
}

/// 前回との比較。bench が 15% 以上遅くなった／速くなったかを返す
pub fn compare(prev: Option<&Value>, cur: Option<&Value>) -> Value {
    let pm = get(get(prev, "bench"), "median_ms");
    let cm = get(get(cur, "bench"), "median_ms");
    if !truthy(pm) || !truthy(cm) {
        return Value::Null;
    }
    let ratio = num(cm) / num(pm);
    let delta = |a: Jv<'_>, b: Jv<'_>| if present(a) && present(b) { js::jnum(js::fixed_num(num(a) - num(b), 1)) } else { Value::Null };
    let pmem = get(get(prev, "memory"), "available_pct");
    let cmem = get(get(cur, "memory"), "available_pct");
    json!({
        "bench_ratio": js::jnum(js::fixed_num(ratio, 3)),
        "cpu_delta": delta(get(cur, "cpu_busy"), get(prev, "cpu_busy")),
        "mem_delta": delta(cmem, pmem),
        "verdict": if ratio >= 1.15 { "slower" } else if ratio <= 0.87 { "faster" } else { "same" },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_kill_matches_like_js() {
        assert!(no_kill("WindowServer"));
        assert!(no_kill("windowserver"));
        assert!(no_kill("python3.12"));
        assert!(no_kill("com.apple.WebKit.Networking"));
        assert!(!no_kill("Google Drive"));
        assert!(!no_kill("python3.x"));
        assert!(!no_kill("mdworker\nx"));
    }

    #[test]
    fn healthy_mac_has_no_findings() {
        let mac = json!({
            "probe": "mac", "host": { "cores": 10, "uptime_h": 10 }, "cpu_busy": 20,
            "memory": { "total_gb": 16, "available_pct": 50, "pressure": "normal", "swap_used_gb": 0, "swap_total_gb": 0 },
            "processes": { "top_cpu": [], "apps": [], "apps_cpu": [], "agent_processes": 3 },
            "disk": [{ "mount": "/", "total_gb": 500, "free_gb": 200, "free_pct": 40 }],
            "power": {}, "containers": {}, "caches": [], "bench": { "runs_ms": [100, 101, 99], "median_ms": 100 }
        });
        assert_eq!(analyze(&mac, &json!({})), Vec::<Value>::new());
        assert_eq!(score(&[]), 100);
    }
}
