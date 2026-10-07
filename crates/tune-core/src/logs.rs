//! 各機体のログを前回の続きから取り込む。読み取り専用。仕様は lib/logs.js。
//! 取り込みは (node, source, uid) で一意なので、同じ範囲を読み直しても重複しない（失敗したら次回やり直せばよい）。

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::collect::{self, MAC_LOGS, RunResult, WIN_LOGS, last_json_line, ssh_args};
use crate::db::{LogRow, Store, now_ms};
use crate::js::{self, Obj, get, present, string, truthy};
use crate::nodes::Node;

pub const MAX_MESSAGE: usize = 1000;

fn re(p: &str) -> Regex {
    Regex::new(&p.replace("{WS}", js::WS)).expect("regex")
}

// 秘密らしいものを伏せる。ログは機体の外へ出る前提で扱う。
// JS の \b・\d・\s・大文字小文字無視（i）は ASCII／JS の空白の意味に合わせて書き換えてある
static REDACT: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (re(r"(?-u:\b)(gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})"), "<github-token>"),
        (re(r"(?-u:\b)sk-[A-Za-z0-9_-]{16,}"), "<api-key>"),
        (re(r"(?-u:\b)(AKIA|ASIA)[0-9A-Z]{16}(?-u:\b)"), "<aws-key>"),
        (re(r"(?-u:\b)ops_[A-Za-z0-9_-]{20,}"), "<op-token>"),
        (re(r"(?-u:\b)eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}"), "<jwt>"),
        (re(r"((?i-u:Bearer|Basic))[{WS}]+[A-Za-z0-9._~+/=-]{12,}"), "${1} <redacted>"),
        (
            re(r#"(?-u:\b)((?i-u:password|passwd|pwd|token|secret|api[_-]?key|access[_-]?key))([{WS}]*[=:][{WS}]*)("[^"]*"|'[^']*'|[^{WS}]+)"#),
            "${1}${2}<redacted>",
        ),
    ]
});

/// 秘密らしい値を伏せ、1,000 文字（UTF-16 単位）で切る
pub fn redact(s: &str) -> String {
    let mut out = s.to_string();
    for (re, rep) in REDACT.iter() {
        out = re.replace_all(&out, *rep).into_owned();
    }
    if js::len16(&out) > MAX_MESSAGE { js::slice16(&out, MAX_MESSAGE) + "…" } else { out }
}

static FP: LazyLock<[Regex; 6]> = LazyLock::new(|| {
    [
        re(r"\{?[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\}?"),
        re(r"0x[0-9a-f]+"),
        re(r#"[a-z]:\\[^{WS}"']+|/(?:users|home|private|var|tmp|applications|library|system)/[^{WS}"']*"#),
        re(r#""[^"]*"|'[^']*'"#),
        re(r"[0-9]+(\.[0-9]+)?"),
        re(r"[{WS}]+"),
    ]
});

/// 同種のログをまとめる鍵。GUID・16進・数字・パス・引用符の中身を伏せてからハッシュする
pub fn fingerprint(source: &str, provider: Option<&Value>, event_id: Option<&Value>, message: &str) -> String {
    let lower = message.to_lowercase();
    let reps = ["<guid>", "<hex>", "<path>", "<q>", "<n>", " "];
    let mut norm = lower;
    for (re, rep) in FP.iter().zip(reps) {
        norm = re.replace_all(&norm, rep).into_owned();
    }
    let norm = js::slice16(js::trim(&norm), 400);
    let part = |v: Option<&Value>| if present(v) { string(v) } else { String::new() };
    let key = format!("{source}|{}|{}|{norm}", part(provider), part(event_id));
    let hex = sha1_smol::Sha1::from(key.as_bytes()).digest().to_string();
    hex[..16].to_string()
}

/// 取り込んだ行を正規化する（伏せ字・長さの上限・指紋）
pub fn normalize(source: &str, rows: &[Value]) -> Vec<LogRow> {
    rows.iter()
        .filter(|r| truthy(Some(r)) && present(r.get("uid")) && truthy(r.get("ts")))
        .map(|r| {
            let m = r.get("message");
            let message = redact(&if truthy(m) { string(m) } else { String::new() });
            let level = r.get("level").and_then(Value::as_str).filter(|l| ["critical", "error", "warn", "info"].contains(l)).unwrap_or("info");
            let provider = r.get("provider");
            let event_id = r.get("event_id");
            LogRow {
                uid: js::slice16(&string(r.get("uid")), 200),
                ts: js::num(r.get("ts")),
                level: level.into(),
                provider: truthy(provider).then(|| js::slice16(&string(provider), 120)),
                event_id: present(event_id).then(|| js::slice16(&string(event_id), 60)),
                fingerprint: fingerprint(source, provider, event_id, &message),
                message,
            }
        })
        .collect()
}

pub fn sources(os: &str) -> &'static [&'static str] {
    match os {
        "macos" => &["mac_diag", "mac_kernel"],
        "windows" => &["win_system", "win_application", "neonmonitor"],
        _ => &[],
    }
}

/// `String(Number.parseInt(cursor ?? '0', 10) || 0)`
fn cursor_arg(c: Option<&Value>) -> String {
    let s = match c {
        None | Some(Value::Null) => "0".to_string(),
        v => string(v),
    };
    let t = js::trim(&s);
    let (neg, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let digits: String = body.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return "0".into();
    }
    let x = digits.parse::<f64>().unwrap_or(0.0) * if neg { -1.0 } else { 1.0 };
    if x == 0.0 { "0".into() } else { js::num_str(x) }
}

async fn fetch_logs(node: &Node, cursors: &Map<String, Value>) -> RunResult {
    let c = |s: &str| cursor_arg(cursors.get(s));
    if node.is_mac() {
        let args = [c("mac_diag"), c("mac_kernel")];
        let t = Duration::from_secs(60);
        return if node.local {
            let mut a = vec!["python3".to_string(), "-".to_string()];
            a.extend(args.iter().cloned());
            collect::run("/usr/bin/env", &a, Some(MAC_LOGS.as_bytes()), t).await
        } else {
            let a = args.join(" ");
            let remote = format!("command -v python3 >/dev/null && exec python3 - {a} || exec /usr/bin/python3 - {a}");
            collect::run("ssh", &ssh_args(&node.alias, &remote), Some(MAC_LOGS.as_bytes()), t).await
        };
    }
    let params = format!("-SysCursor {} -AppCursor {} -NeonCursor {}", c("win_system"), c("win_application"), c("neonmonitor"));
    let t = Duration::from_secs(90);
    if node.local {
        return collect::local::powershell_file(WIN_LOGS, &params, t).await;
    }
    let remote = format!(
        "mkdir -p ~/.katala-tune && cat > ~/.katala-tune/logs.ps1 && powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"$(cygpath -w ~/.katala-tune/logs.ps1)\" {params}"
    );
    collect::run("ssh", &ssh_args(&node.alias, &remote), Some(WIN_LOGS), t).await
}

/// 1台分を取り込む。戻り値は `{ node_id, sources: { <source>: { inserted, fetched, dropped, note? } | { error } }, meta, error? }`
/// db はロックの道具（取り込み中もほかの読み出しを止めない）
pub async fn sync_node(db: &(impl DbAccess + ?Sized), node: &Node) -> Value {
    let srcs = sources(&node.os);
    let mut cursors = Map::new();
    for s in srcs {
        let c = db.with(|d| d.cursor(&node.id, s)).ok().flatten();
        cursors.insert((*s).into(), c.and_then(|c| c.get("cursor").cloned()).unwrap_or(Value::Null));
    }
    let res = fetch_logs(node, &cursors).await;
    let data = last_json_line(&res.out);
    let mut sources_out = Map::new();
    let mut meta = Map::new();
    let Some(data_sources) = data.as_ref().and_then(|d| d.get("sources")).filter(|v| truthy(Some(v))) else {
        let e = if !res.err.is_empty() {
            res.err.clone()
        } else if !res.out.is_empty() {
            res.out.clone()
        } else {
            format!("exit {}", res.code_str())
        };
        let err = js::slice16_tail(js::trim(&e), 400);
        for s in srcs {
            let _ = db.with(|d| d.cursor_error(&node.id, s, &err));
        }
        return json!({ "node_id": node.id, "sources": {}, "meta": {}, "error": err });
    };
    for s in srcs {
        let src = data_sources.get(*s);
        let src_err = get(src, "error");
        if !truthy(src) || truthy(src_err) {
            let e = if truthy(src_err) { src_err.cloned().unwrap_or(Value::Null) } else { Value::from("no data") };
            let _ = db.with(|d| d.cursor_error(&node.id, s, &string(Some(&e))));
            sources_out.insert((*s).into(), json!({ "error": e }));
            continue;
        }
        let raw_rows = match get(src, "rows") {
            Some(Value::Array(a)) => a.clone(),
            r if truthy(r) => vec![r.cloned().unwrap_or(Value::Null)],
            _ => Vec::new(),
        };
        let rows = normalize(s, &raw_rows);
        let dropped = js::or(get(src, "dropped"), Some(&Value::from(0))).cloned().unwrap_or(Value::from(0));
        let inserted = db.with(|d| {
            let n = d.insert_logs(&node.id, s, &rows, now_ms())?;
            d.cursor_ok(&node.id, s, js::nullish(get(src, "cursor"), None), rows.len() as i64, &dropped)?;
            Ok(n)
        });
        match inserted {
            Ok(n) => {
                sources_out.insert(
                    (*s).into(),
                    Obj::new().set("fetched", rows.len()).set("inserted", n).set("dropped", dropped).opt("note", get(src, "note").cloned()).build(),
                );
            }
            Err(e) => {
                // 書き込みの失敗は成功扱いにしない
                let _ = db.with(|d| d.cursor_error(&node.id, s, &e.to_string()));
                sources_out.insert((*s).into(), json!({ "error": e.to_string() }));
            }
        }
        if let Some(m) = get(src, "meta").filter(|v| truthy(Some(v))) {
            meta.insert((*s).into(), m.clone());
        }
    }
    json!({ "node_id": node.id, "sources": sources_out, "meta": meta })
}

/// DB を短く借りる道具（アプリでは Mutex 越し、テストでは直接）
pub trait DbAccess: Send + Sync {
    fn with<T>(&self, f: impl FnOnce(&Store) -> rusqlite::Result<T>) -> rusqlite::Result<T>;
}

impl DbAccess for std::sync::Mutex<Store> {
    fn with<T>(&self, f: impl FnOnce(&Store) -> rusqlite::Result<T>) -> rusqlite::Result<T> {
        let g = self.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&g)
    }
}

/// 全機体を並列に取り込む。on_result は機体ごとに終わった順で呼ぶ
pub async fn sync_all<D, F>(db: std::sync::Arc<D>, nodes: &[Node], on_result: F) -> Vec<Value>
where
    D: DbAccess + 'static,
    F: Fn(&Value) + Send + Sync + 'static,
{
    let on_result = std::sync::Arc::new(on_result);
    let mut set = tokio::task::JoinSet::new();
    for (i, n) in nodes.iter().cloned().enumerate() {
        let (db, cb) = (db.clone(), on_result.clone());
        set.spawn(async move {
            let r = sync_node(db.as_ref(), &n).await;
            cb(&r);
            (i, r)
        });
    }
    let mut out: Vec<Option<Value>> = vec![None; nodes.len()];
    while let Some(j) = set.join_next().await {
        if let Ok((i, r)) = j {
            out[i] = Some(r);
        }
    }
    out.into_iter().enumerate().map(|(i, r)| r.unwrap_or_else(|| json!({ "node_id": nodes[i].id, "error": "取り込みの途中で内部エラー" }))).collect()
}

/// ログから所見を作る（直近7日）。rules::analyze の結果に足して使う
pub fn log_findings(db: &Store, node_id: &str, now: i64) -> rusqlite::Result<Vec<Value>> {
    let rows = db.log_counts(node_id, now - 7 * 86_400_000)?;
    let sum = |pred: &dyn Fn(&crate::db::LogCount) -> bool| rows.iter().filter(|r| pred(r)).map(|r| r.n).sum::<i64>();
    let prov = |r: &crate::db::LogCount, p: &str| r.provider.as_deref() == Some(p);
    let ev = |r: &crate::db::LogCount, e: &str| r.event_id.as_deref() == Some(e);
    let mut out = Vec::new();
    let mut push = |id: &str, severity: &str, category: &str, title: String, detail: &str, advice: &str, log_query: Value| {
        out.push(json!({ "id": id, "severity": severity, "category": category, "title": title, "detail": detail, "advice": advice, "log_query": log_query }));
    };
    let whea = sum(&|r| prov(r, "Microsoft-Windows-WHEA-Logger"));
    if whea > 0 {
        push(
            "log-whea",
            if whea >= 5 { "critical" } else { "warn" },
            "stability",
            format!("ハードウェアエラー（WHEA）が7日で {whea} 件"),
            "System ログの Microsoft-Windows-WHEA-Logger",
            "CPU・メモリ・PCIe の訂正／訂正不能エラー。メモリの XMP を切る、枚数を減らす、MemTest86 で切り分ける。",
            json!({ "node_id": node_id, "q": "WHEA" }),
        );
    }
    let tdr = sum(&|r| prov(r, "Display") && ev(r, "4101")) + sum(&|r| prov(r, "nvlddmkm"));
    if tdr > 0 {
        push(
            "log-gpu-reset",
            "warn",
            "stability",
            format!("GPU ドライバのリセット・エラーが7日で {tdr} 件"),
            "Display 4101 / nvlddmkm",
            "GPU ドライバのタイムアウト。ドライバの入れ直し（DDU）、オーバークロックの解除、電源容量を確認する。",
            json!({ "node_id": node_id, "q": "nvlddmkm OR Display" }),
        );
    }
    let lowmem = sum(&|r| prov(r, "Microsoft-Windows-Resource-Exhaustion-Detector"));
    if lowmem > 0 {
        push(
            "log-lowmem",
            "warn",
            "memory",
            format!("メモリ枯渇の警告が7日で {lowmem} 件"),
            "Resource-Exhaustion-Detector 2004",
            "コミットが上限に達しかけた。どのプロセスが使っていたかはメッセージに出ている。ページファイルの拡大か大口の整理。",
            json!({ "node_id": node_id, "q": "Resource" }),
        );
    }
    let disk = sum(&|r| ["disk", "Ntfs", "stornvme", "storahci", "volmgr"].iter().any(|p| prov(r, p)) && r.level != "info");
    if disk > 0 {
        push(
            "log-disk",
            "warn",
            "disk",
            format!("ディスク・ファイルシステムのエラーが7日で {disk} 件"),
            "disk / Ntfs / stornvme",
            "SMART を確認し、バックアップを先に取る。ケーブルや M.2 の接触も疑う。",
            json!({ "node_id": node_id, "q": "disk OR Ntfs OR stornvme" }),
        );
    }
    // 1000: Application Error、1002: Application Hang、1026: .NET Runtime の未処理例外
    let crashes = sum(&|r| {
        (r.source == "win_application" && ["1000", "1002", "1026"].iter().any(|e| ev(r, e)))
            || (r.source == "mac_diag" && ["crash", "hang", "spin"].iter().any(|e| ev(r, e)))
    });
    if crashes >= 3 {
        push(
            "log-crashes",
            if crashes >= 15 { "warn" } else { "info" },
            "stability",
            format!("アプリのクラッシュ・ハングが7日で {crashes} 件"),
            "Application Error 1000 / Hang 1002、DiagnosticReports",
            "ログ画面の「同種ログ」で、どのアプリが繰り返しているかを見る。",
            json!({ "node_id": node_id, "level": "error" }),
        );
    }
    let panics = sum(&|r| ev(r, "kernel panic"));
    if panics > 0 {
        push(
            "log-panic",
            "critical",
            "stability",
            format!("カーネルパニックが7日で {panics} 件"),
            "DiagnosticReports",
            "直前に入れた拡張・周辺機器・OS 更新を疑う。パニックログの panicString を確認する。",
            json!({ "node_id": node_id, "q": "panic" }),
        );
    }
    let jetsam = sum(&|r| ev(r, "jetsam (memory)"));
    if jetsam > 0 {
        push(
            "log-jetsam",
            "warn",
            "memory",
            format!("メモリ不足でアプリが落とされた記録が7日で {jetsam} 件"),
            "jetsam",
            "メモリの大口を減らす。",
            json!({ "node_id": node_id, "q": "jetsam" }),
        );
    }
    let neon = sum(&|r| r.source == "neonmonitor" && r.level != "info");
    if neon > 0 {
        push(
            "log-neon",
            "warn",
            "memory",
            format!("NeonMonitor の自動保護が7日で {neon} 回動いた"),
            "guard.log の警告・強制終了・失敗",
            "逼迫が繰り返している。強制終了で落ちたアプリが無いか確認し、根本の大口を減らす。",
            json!({ "node_id": node_id, "source": "neonmonitor" }),
        );
    }
    // サービスの起動失敗・異常終了（7000/7009/7023/7031/7034）
    let svc = sum(&|r| prov(r, "Service Control Manager") && ["7000", "7009", "7023", "7031", "7034"].iter().any(|e| ev(r, e)));
    if svc >= 3 {
        push(
            "log-service",
            "warn",
            "stability",
            format!("サービスの起動失敗・異常終了が7日で {svc} 件"),
            "Service Control Manager 7000/7009/7023/7031/7034",
            "どのサービスかはログに出ている。セキュリティ製品（Defender など）なら、保護が止まっている可能性があるので先に直す。",
            json!({ "node_id": node_id, "q": "\"Service Control Manager\"" }),
        );
    }
    // 同じエラーの洪水（24時間で200件以上）。クラッシュと再起動の繰り返しや、ログの出し過ぎを見つける
    static WS_RUN: LazyLock<Regex> = LazyLock::new(|| re(r"[{WS}]+"));
    for t in db.top_signatures(node_id, now - 86_400_000, 2)?.into_iter().filter(|x| x.n >= 200) {
        let who = t.provider.clone().filter(|p| !p.is_empty()).unwrap_or_else(|| t.source.clone());
        let q = match t.provider.as_deref().filter(|p| !p.is_empty()) {
            Some(p) => format!("\"{}\"", p.replace('"', "")),
            None => String::new(),
        };
        push(
            &format!("log-flood-{}", t.fingerprint),
            "warn",
            "background",
            format!("同じエラーが24時間で {} 件繰り返している（{who}）", t.n),
            &js::slice16(&WS_RUN.replace_all(&t.sample, " "), 160),
            "起動と失敗を繰り返しているか、ログを出し過ぎている。どちらも CPU とディスクを使い続ける。出している側の設定を直すか止める。",
            json!({ "node_id": node_id, "q": q }),
        );
    }
    let dropped = db.dropped_total(node_id)?;
    if dropped >= 1000 {
        push(
            "log-dropped",
            "info",
            "background",
            format!("ログが多すぎて {dropped} 件を取り込めなかった"),
            "1回の取り込みは1か所あたり300件まで",
            "上の「同じエラーの繰り返し」を直すと収まる。",
            json!({ "node_id": node_id }),
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secrets() {
        let fake_aws = ["AKIA", "ABCDEFGHIJKLMNOP"].concat();
        let s = redact(&format!(
            "token=abc123 Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig12345678 ghp_{} {fake_aws} sk-{} password: \"p w\"",
            "a".repeat(36),
            "x".repeat(30)
        ));
        for bad in ["abc123", "ghp_a", "AKIAABCD", "sk-xxx", "p w"] {
            assert!(!s.contains(bad), "{s}");
        }
        assert!(s.contains("token=<redacted>"));
        assert!(s.contains("<github-token>"));
        assert!(s.contains("<aws-key>"));
        assert!(js::len16(&redact(&"a".repeat(5000))) <= 1001);
    }

    #[test]
    fn same_shape_same_fingerprint() {
        let p = Some(Value::from("disk"));
        let e = Some(Value::from("7"));
        let a = fingerprint("win_system", p.as_ref(), e.as_ref(), "The device \\Device\\Harddisk1\\DR1 has a bad block at 0x1F3 in C:\\Users\\a\\x.txt");
        let b = fingerprint("win_system", p.as_ref(), e.as_ref(), "The device \\Device\\Harddisk2\\DR2 has a bad block at 0xAB in C:\\Users\\b\\y.txt");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn cursor_arg_like_parse_int() {
        assert_eq!(cursor_arg(None), "0");
        assert_eq!(cursor_arg(Some(&Value::from("1791352417838"))), "1791352417838");
        assert_eq!(cursor_arg(Some(&Value::from(" 12abc"))), "12");
        assert_eq!(cursor_arg(Some(&Value::from("abc"))), "0");
        assert_eq!(cursor_arg(Some(&Value::from("-5"))), "-5");
    }
}
