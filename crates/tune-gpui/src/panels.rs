//! Saved-data presentation only. Accessors retain collection and privacy ownership.
use serde_json::{Value, json};
use tune_core::{db::TopologyUiRequest, engine::Engine};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogLevel {
    #[default]
    All,
    Error,
    Warn,
    Info,
}
impl LogLevel {
    pub const ALL: [Self; 4] = [Self::All, Self::Error, Self::Warn, Self::Info];
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "すべて",
            Self::Error => "エラー",
            Self::Warn => "警告",
            Self::Info => "情報",
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Page {
    Overview,
    Status,
    Resources,
    Processes,
    Jobs,
    Logs,
    Tools,
    Ai,
    Security,
    Specs,
    Power,
    Topology,
    Actions,
    Network,
    Link,
    Review,
}
impl Page {
    pub const ALL: [Self; 16] = [
        Self::Overview,
        Self::Status,
        Self::Resources,
        Self::Processes,
        Self::Jobs,
        Self::Logs,
        Self::Tools,
        Self::Ai,
        Self::Security,
        Self::Specs,
        Self::Power,
        Self::Topology,
        Self::Actions,
        Self::Network,
        Self::Link,
        Self::Review,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "概要",
            Self::Status => "状態",
            Self::Resources => "リソース",
            Self::Processes => "プロセス",
            Self::Jobs => "定期処理",
            Self::Logs => "ログ",
            Self::Tools => "道具",
            Self::Ai => "AI",
            Self::Security => "セキュリティ",
            Self::Specs => "スペック",
            Self::Power => "電力・Clock",
            Self::Topology => "構成図",
            Self::Actions => "実行記録",
            Self::Network => "ネットワーク",
            Self::Link => "接続",
            Self::Review => "レビュー",
        }
    }
    pub fn search_hint(self) -> &'static str {
        match self {
            Self::Logs => "ログ検索（FTS5 式・Enter で実行）",
            Self::Ai => "セッションの ID・モデル・作業場所・版（Enter）",
            Self::Topology => "保存済み表示列を検索（Enter）",
            _ => "表示レコードを検索（Enter）",
        }
    }
}
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Muted,
    Good,
    Warning,
    Error,
}
#[derive(Debug, Clone)]
pub struct Row {
    pub title: String,
    pub detail: String,
    pub badge: String,
    pub tone: Tone,
    /// 実行記録の行で、元に戻せるときの記録の id（元に戻す操作を持ち、まだ戻されていない成功の記録）
    pub undo: Option<String>,
}
#[derive(Debug, Clone)]
pub struct Panel {
    pub facts: Vec<(String, String)>,
    pub rows: Vec<Row>,
    pub note: String,
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) if !s.is_empty() => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => if *b { "はい" } else { "いいえ" }.into(),
        _ => "未取得".into(),
    }
}
fn field(v: &Value, key: &str) -> String {
    scalar(&v[key])
}
fn array<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v[key].as_array().map_or(&[], Vec::as_slice)
}
fn list(v: &Value) -> String {
    v.as_array().map(|a| a.iter().map(scalar).collect::<Vec<_>>().join(" / ")).filter(|s| !s.is_empty()).unwrap_or_else(|| "なし・未報告".into())
}
fn fields(v: &Value, keys: &[&str]) -> String {
    keys.iter().map(|k| format!("{}: {}", label(k), field(v, k))).collect::<Vec<_>>().join(" · ")
}
fn label(key: &str) -> &str {
    match key {
        "ok" => "正常・成功",
        "warn" => "注意",
        "fail" => "異常",
        "unknown" => "不明",
        "enabled" => "有効",
        "probe_minutes" => "分析間隔（分）",
        "logs_minutes" => "ログ同期間隔（分）",
        "type" | "kind" => "種類",
        "state" | "status" => "状態",
        "name" => "名前",
        "source" => "取得元",
        "public" => "外部アドレス",
        "event_id" => "イベントID",
        "n_24h" => "24時間の件数",
        "n_7d" => "7日間の件数",
        "addr" => "アドレス",
        "port" => "ポート",
        "proto" => "プロトコル",
        "exposure" => "公開範囲",
        "seen" => "観測回数",
        "key" => "対象",
        "program" => "実行ファイル",
        "from_program" => "変更前の実行ファイル",
        "free_pct" => "ディスク空き（%）",
        "free_gb" => "ディスク空き（GB）",
        "total_gb" => "容量（GB）",
        "util" => "使用率（%）",
        "temp_c" => "温度（℃）",
        "power_w" => "電力（W）",
        "clocks_graphics_mhz" => "Graphics Clock（MHz）",
        "mem_used_mb" => "使用メモリ（MB）",
        "mem_total_mb" => "総メモリ（MB）",
        "schedule" => "予定",
        "scope" => "対象範囲",
        "sessions" => "セッション数",
        "prompts" => "プロンプト数",
        "tool_calls" => "ツール呼出数",
        "tool_errors" => "ツール失敗数",
        "turn_errors" => "応答失敗数",
        "api_errors" => "API失敗数",
        "hook_errors" => "フック失敗数",
        "tok_in" => "入力トークン",
        "tok_out" => "出力トークン",
        "tok_cache_read" => "キャッシュ読取トークン",
        "tok_cache_write" => "キャッシュ書込トークン",
        "tok_reasoning" => "推論トークン",
        "version" => "版",
        "prices_version" => "単価表の版",
        "limits_enabled" => "残り枠取得が有効",
        "truncated" => "続きあり・上限到達",
        "no_python" => "Pythonなし",
        "files_total" => "対象ファイル数",
        "files_changed" => "変更ファイル数",
        "bytes_pending" => "未読バイト数",
        "bytes_read" => "読取バイト数",
        "ingesting" => "取込中",
        "pending" => "続きあり",
        "firewall" => "ファイアウォール",
        "stealth" => "ステルスモード",
        "gatekeeper" => "Gatekeeper",
        "xprotect_version" => "XProtect版",
        "security_log" => "セキュリティログ",
        "detections_30d" => "30日間の検出数",
        "detections_open_30d" => "30日間の未解決検出数",
        "realtime" => "リアルタイム保護",
        "antivirus" => "ウイルス対策",
        "service" => "サービス",
        "mode" => "モード",
        "sig_age_days" => "定義の経過日数",
        "tamper" => "改変保護",
        "uptodate" => "更新済み",
        "total_w" => "合計電力（W）",
        "wall_w" => "入力側実測（W）",
        "soc_w" => "SoC電力（W）",
        "cpu_w" => "CPU電力（W）",
        "gpu_w" => "GPU電力（W）",
        "package_w" => "CPUパッケージ電力（W）",
        "package_source" => "CPU電力取得元",
        "soc_source" => "SoC電力取得元",
        "platform_w" => "システム推定電力（W）",
        "platform_source" => "システム電力取得元",
        "kwh" => "電力量（kWh）",
        "covered_seconds" => "有効観測時間（秒）",
        "duration_seconds" => "観測窓の長さ（秒）",
        "coverage" => "観測カバー率",
        "accepted_entities" => "採用済み要素数",
        "accepted_edges" => "採用済み関係数",
        "accepted_closed_edges" => "選択要素間の採用済み関係数",
        "candidate_entities" => "候補要素数",
        "candidate_edges" => "候補関係数",
        "selected_rows_checked" => "検証した選択行数",
        "corrupt_scopes_hidden" => "破損のため非表示の範囲数",
        "full_archive_checked" => "全アーカイブ検証済み",
        "unselected_rows_checked" => "未選択行の検証済み",
        "attempt_id" => "試行ID",
        "is_latest_attempt" => "最新試行",
        "validation_scope" => "検証範囲",
        "id" => "ID",
        "change_kind" => "変更の種類",
        "run_id" => "実行ID",
        "entries" => "項目数",
        "files" => "ファイル数",
        "bytes" => "バイト数",
        "health" => "健全性",
        "revision" => "リビジョン",
        "ports_total" => "保存ポート数",
        "ports_omitted" => "省略ポート数",
        "attributes_truncated" => "属性の省略あり",
        _ => key,
    }
}
fn state(v: &Value) -> Tone {
    match v.as_str() {
        Some("ok" | "complete" | "measured") => Tone::Good,
        Some("error" | "fail" | "failed" | "permission_denied") => Tone::Error,
        Some("warn" | "partial" | "incomplete" | "estimated" | "timeout") => Tone::Warning,
        Some("unknown" | "missing" | "excluded" | "not_collected") | None => Tone::Muted,
        _ => Tone::Normal,
    }
}
fn time(v: &Value) -> String {
    let Some(ms) = v.as_i64().filter(|n| *n >= 0) else { return "未取得".into() };
    let sec = ms / 1000;
    // Gregorian civil date, with epoch days shifted to the March-based era.
    let z = sec / 86400 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let y = y + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", sec / 3600 % 24, sec / 60 % 60, sec % 60)
}
fn timestamp_or_text(v: &Value) -> String {
    if v.is_number() { time(v) } else { scalar(v) }
}
fn row(p: &mut Panel, title: impl Into<String>, detail: impl Into<String>, badge: impl Into<String>, tone: Tone) {
    p.rows.push(Row { title: title.into(), detail: detail.into(), badge: badge.into(), tone, undo: None });
}
fn fact(p: &mut Panel, label: &str, value: impl ToString) {
    p.facts.push((label.into(), value.to_string()));
}
fn money(v: &Value) -> String {
    v.as_f64().map_or_else(|| "単価不明（算出不可）".into(), |n| format!("${n:.4}（既知単価分の推定）"))
}

pub fn load(engine: &Engine, page: Page, query: &str, log_level: LogLevel) -> Result<Panel, String> {
    // Validate before every accessor, including pages without backend search.
    if query.len() > 256 || query.chars().any(char::is_control) {
        return Err("検索は制御文字を含まない256バイト以内で指定してください".into());
    }
    let query = query.trim();
    if page != Page::Overview {
        engine.reload_config();
        if let Some(error) = engine.config_error() {
            return Err(error);
        }
    }
    let mut p = Panel {
        facts: vec![], rows: vec![], note: "保存済みデータの読み取り専用表示。取得・再計算・変更操作は行いません。".into()
    };
    match page {
        Page::Overview => return Err("概要は機体の概要画面を使います".into()),
        Page::Status => {
            let v = engine.status_snapshot()?;
            fact(&mut p, "現在チェック数", fields(&v["counts"], &["ok", "warn", "fail", "unknown"]));
            fact(&mut p, "最終分析", time(&v["lastProbeAt"]));
            fact(&mut p, "最終ログ同期", time(&v["lastLogsAt"]));
            fact(&mut p, "保存済み間隔", fields(&v["schedule"], &["enabled", "probe_minutes", "logs_minutes"]));
            for c in array(&v, "checks") {
                row(
                    &mut p,
                    format!("チェック · {} · {}", field(c, "scope"), field(c, "name")),
                    format!("{} · 状態開始 {} · 最終確認 {} · ID {}", field(c, "detail"), time(&c["since"]), time(&c["checked_at"]), field(c, "id")),
                    field(c, "status"),
                    state(&c["status"]),
                );
            }
            for e in array(&v, "events") {
                row(
                    &mut p,
                    format!("状態履歴 · {} · {}", field(e, "scope"), field(e, "name")),
                    format!(
                        "{} · {} → {} · {} · check {} / event {}",
                        time(&e["ts"]),
                        e["from_status"].as_str().unwrap_or("初回"),
                        field(e, "to_status"),
                        field(e, "detail"),
                        field(e, "check_id"),
                        field(e, "id")
                    ),
                    field(e, "to_status"),
                    state(&e["to_status"]),
                );
            }
            p.note.push_str(" 現在チェックは現在の台帳範囲。状態履歴は最大150件で、削除済み機体を含む場合があります。再計算はしていません。");
        }
        Page::Resources | Page::Processes | Page::Jobs => snapshots(engine, page, &mut p)?,
        Page::Logs => {
            let mut filter = json!({"limit":2000,"offset":0});
            if let Some(level) = match log_level {
                LogLevel::All => None,
                LogLevel::Error => Some("error"),
                LogLevel::Warn => Some("warn"),
                LogLevel::Info => Some("info"),
            } {
                filter["level"] = json!(level);
            }
            if !query.is_empty() {
                filter["q"] = json!(query);
            }
            let logs = engine.logs_query(&filter)?;
            fact(&mut p, "この取得窓で公開されたログ行数", logs.len());
            for l in logs {
                row(
                    &mut p,
                    format!("ログ · {} · {}", field(&l, "node_id"), field(&l, "source")),
                    format!(
                        "{} · {} · provider {} / event {} · 出現 {} 回",
                        time(&l["ts"]),
                        field(&l, "message"),
                        field(&l, "provider"),
                        field(&l, "event_id"),
                        field(&l, "occurrences")
                    ),
                    field(&l, "level"),
                    state(&l["level"]),
                );
            }
            if query.is_empty() {
                for s in engine.logs_signatures(&json!({"limit":50,"offset":0}))? {
                    row(
                        &mut p,
                        format!("署名 · {}", field(&s, "fingerprint")),
                        format!(
                            "{} · 窓内出現 {} / 機体 {} · 最終 {} · 累積 {}（期間非連動）",
                            field(&s, "sample"),
                            field(&s, "n"),
                            field(&s, "nodes"),
                            time(&s["last_ts"]),
                            field(&s, "total")
                        ),
                        "署名・別集計",
                        state(&s["level"]),
                    );
                }
            }
            for c in engine.logs_cursors()? {
                let error = c["last_error"].as_str();
                row(
                    &mut p,
                    format!("収集状態 · {} · {}", field(&c, "node_id"), field(&c, "source")),
                    format!(
                        "最終成功 {} · 最終試行 {} · {} · 前回取得行 {} · 破棄累積 {} / 連続失敗 {} · 保存注記 {}",
                        time(&c["last_ok_at"]),
                        time(&c["updated_at"]),
                        error.unwrap_or("直近エラーなし"),
                        field(&c, "last_count"),
                        field(&c, "dropped_total"),
                        field(&c, "fail_streak"),
                        field(&c, "note")
                    ),
                    if error.is_some() {
                        "収集失敗"
                    } else if c["last_ok_at"].is_null() {
                        "未実施"
                    } else {
                        "収集状態"
                    },
                    if error.is_some() { Tone::Error } else { Tone::Muted },
                );
            }
            p.note.push_str(" ログは検索・レベルを適用した最新の生行最大2000件を取得した後で現行のプライバシー設定を適用する有界窓です。表示件数は全履歴の総数・末尾を意味しません。検索はFTS5式。署名は検索なしのときだけログのレベルとは別集計で最大50件。収集状態は検索・レベルとは別の全許可対象です。");
        }
        Page::Tools => inventory(&engine.inventory_view(false)?, &mut p),
        Page::Ai => {
            ai_summary(&engine.ai_summary(&json!({"days":30,"tz":0}))?, &mut p);
            let sessions = engine.ai_sessions(&json!({"limit":100,"offset":0,"sort":"last","q":query}))?;
            fact(&mut p, "検索条件に一致する保存セッション総数", field(&sessions, "total"));
            for s in array(&sessions, "rows") {
                row(
                    &mut p,
                    format!("セッション · {} · {}", field(s, "node_id"), field(s, "tool")),
                    format!(
                        "ID {} · モデル {} · {} → {} · {} · 親 {}",
                        field(s, "session_id"),
                        field(s, "model"),
                        time(&s["first_ts"]),
                        time(&s["last_ts"]),
                        fields(s, &["prompts", "tool_calls", "tool_errors", "turn_errors", "api_errors", "hook_errors", "tok_in", "tok_out", "version"]),
                        field(s, "parent_id")
                    ),
                    "保存済みセッション",
                    Tone::Normal,
                );
            }
            p.note.push_str(" 集計はUTC基準30日・検索非連動。費用はAPI単価による推定で実請求ではなく、未知単価分を含みません。セッションは検索一致の最新100件（全期間）、重複除去集計と同じ量ではありません。残り枠は保存値で、失敗後に古い窓が残る場合があります。");
        }
        Page::Security => security(&engine.netsec_view(&json!({}))?, &mut p),
        Page::Specs => specs(&engine.specs_view()?, &mut p),
        Page::Power => power(engine, &mut p)?,
        Page::Topology => {
            let request = TopologyUiRequest { filter: query.into(), ..Default::default() };
            topology(&engine.topology_ui_view(&request)?, &mut p);
        }
        Page::Network => {
            let eligible = engine.config().nodes.iter().filter(|n| crate::collection::network_refusal(n).is_none()).count();
            fact(&mut p, "到達性を確認できる機体", eligible);
            p.note.push_str(
                "各機体のネットワークの到達性を、受動的に（外へ探りを入れずに）確認します。結果は真偽だけで、IP アドレスは含みません。保存はせず、上の「全対象のネットワーク確認」を押したときだけ各機体へ接続して、結果を上の欄に出します。共用機・保護対象・台帳で接続診断を止めた機体は対象外です。",
            );
        }
        Page::Link => link(engine, &mut p)?,
        // 読み込んだ完了台帳は画面側の状態。何も読み込んでいないときの表だけをここで返す
        Page::Review => p = crate::review::empty_panel(None),
        Page::Actions => {
            let actions = engine.with_db(|d| d.actions(100))?;
            fact(&mut p, "取得窓の操作記録数（最大100）", actions.len());
            // 戻し済みの記録（成功した undo_of）は、もう一度戻せない
            let undone: std::collections::HashSet<String> =
                actions.iter().filter(|a| a["ok"] == json!(true)).filter_map(|a| a["undo_of"].as_str().map(str::to_string)).collect();
            for a in actions {
                let status = a["state"].as_str().unwrap_or("incomplete");
                let undoable = a["ok"] == json!(true) && !a["undo"].is_null() && a["id"].as_str().is_some_and(|id| !undone.contains(id));
                row(
                    &mut p,
                    format!("操作 · {} · {}", field(&a, "node_id"), field(&a, "label")),
                    format!("{} · {} · 出力 {} · undo_of {}", time(&a["at"]), fields(&a, &["type", "state", "ok"]), field(&a, "output"), field(&a, "undo_of")),
                    match status {
                        "ok" => "成功",
                        "failed" => "失敗",
                        _ => "未完了",
                    },
                    state(&json!(status)),
                );
                if undoable && let Some(r) = p.rows.last_mut() {
                    r.undo = a["id"].as_str().map(str::to_string);
                }
            }
            p.note.push_str(
                " 保存済みの最新100件。未完了は失敗とは別です。AI handoffはターミナル起動要求でありCLI起動確認ではありません。元に戻せる記録は、選んだ行の「元に戻す…」から戻せます（確認欄を通したときだけ実行します）。",
            );
        }
    }
    if !matches!(page, Page::Logs | Page::Ai | Page::Topology) && !query.is_empty() {
        let q = query.to_lowercase();
        p.rows.retain(|r| format!("{} {} {}", r.title, r.detail, r.badge).to_lowercase().contains(&q));
        p.note.push_str(" 検索はこの取得範囲の表示レコードのみ。上部の集計値は検索前です。");
    }
    let shown = p.rows.len();
    fact(&mut p, "今回の表示レコード数", shown);
    Ok(p)
}

/// ペア済みの tune-agent（公開鍵の指紋・名前・つなぎ先・ペアリングした時刻だけ）。秘密鍵とコードは読まない。
/// ペアリングと解除は鍵とコードを扱うので、この画面ではしない。
fn link(engine: &Engine, p: &mut Panel) -> Result<(), String> {
    let dir = tune_link::console_dir(engine.data_dir());
    let peers = tune_link::peers::PeerStore::new(&dir).list().map_err(|e| e.to_string())?;
    let agents: Vec<_> = peers.iter().filter(|x| x.role == tune_link::peers::Role::Agent).map(tune_link::peers::Peer::summary).collect();
    fact(p, "ペア済みの tune-agent", agents.len());
    for a in &agents {
        row(
            p,
            format!("ペア済み · {}", field(a, "name")),
            format!("公開鍵の指紋 {} · つなぎ先 {} · ペアリング {}", field(a, "fingerprint"), field(a, "addr"), time(&a["paired_at"])),
            "ペア済み",
            Tone::Normal,
        );
    }
    p.note.push_str(
        "ペア済みの機体の一覧だけです（公開鍵の指紋・名前・つなぎ先・時刻。秘密鍵とコードは読みません）。ペアリングと解除は鍵とコードを扱うため、この画面ではできません。既存版か CLI（tune agent-pair / agent-unpair）で行います。調べる経路は今は SSH のままです。",
    );
    Ok(())
}

fn snapshots(engine: &Engine, page: Page, p: &mut Panel) -> Result<(), String> {
    let cfg = engine.config();
    let saved = engine.last()?;
    fact(p, "台帳機体数", cfg.nodes.len());
    for node in cfg.nodes {
        let op = tune_core::nodes::op_state(&node.raw);
        let r = saved.iter().find(|r| r["node_id"] == node.id);
        if !op.active {
            row(p, &node.id, format!("{} · {}", tune_core::nodes::state_label(&op.state), op.reason), "運用対象外", Tone::Muted);
            continue;
        }
        let Some(r) = r else {
            row(p, &node.id, "保存済み分析なし", "未分析", Tone::Muted);
            continue;
        };
        if r["ok"] != true {
            row(p, &node.id, format!("{} · {}", time(&r["at"]), field(r, "error")), "直近分析失敗", Tone::Error);
            continue;
        }
        let d = &r["data"];
        match page {
            Page::Resources => {
                let cpu_partial = d["cpu_busy_availability"] == "partial";
                let cpu_quality = if node.os == "windows" {
                    let quality = match d["cpu_busy_availability"].as_str() {
                        Some("partial") => "部分観測",
                        Some("available") => "観測済み",
                        Some("unavailable") => "未取得",
                        _ => "品質未報告",
                    };
                    format!(" · CPU観測品質 {quality} · 観測サンプル {} / 2", field(d, "cpu_busy_samples"))
                } else {
                    String::new()
                };
                row(
                    p,
                    format!("リソース · {}", node.id),
                    format!(
                        "{} · CPU {}% · メモリ空き {}% · swap {} GB / commit {}% · 稼働 {} h · 計測 {} ms · スコア {}{}",
                        time(&r["at"]),
                        field(d, "cpu_busy"),
                        field(&d["memory"], "available_pct"),
                        field(&d["memory"], "swap_used_gb"),
                        field(&d["memory"], "commit_pct"),
                        field(&d["host"], "uptime_h"),
                        field(&d["bench"], "median_ms"),
                        field(r, "score"),
                        cpu_quality
                    ),
                    if cpu_partial { "保存済み・CPU部分観測" } else { "保存済み" },
                    if cpu_partial { Tone::Warning } else { Tone::Normal },
                );
                for disk in array(d, "disk") {
                    row(
                        p,
                        format!("ディスク · {} · {}", node.id, field(disk, "mount")),
                        fields(disk, &["name", "free_pct", "free_gb", "total_gb"]),
                        "保存済み",
                        Tone::Normal,
                    );
                }
                for g in array(d, "gpus") {
                    row(
                        p,
                        format!("GPU · {} · {}", node.id, field(g, "name")),
                        fields(g, &["util", "temp_c", "power_w", "clocks_graphics_mhz", "mem_used_mb", "mem_total_mb"]),
                        "保存済み",
                        Tone::Normal,
                    );
                }
            }
            Page::Processes => {
                let mut seen = std::collections::HashSet::new();
                let mut count = 0;
                for proc in array(&d["processes"], "top_cpu").iter().chain(array(&d["processes"], "top_mem")) {
                    if !seen.insert(proc["pid"].to_string()) {
                        continue;
                    }
                    count += 1;
                    let cpu = proc["cpu"]
                        .as_f64()
                        .and_then(|n| if node.os == "windows" { d["host"]["cores"].as_f64().filter(|c| *c > 0.0).map(|c| n * c) } else { Some(n) });
                    row(
                        p,
                        format!("プロセス · {} · {}", node.id, field(proc, "name")),
                        format!(
                            "{} · PID {} · app {} · CPU {}%（1コア換算） · メモリ {} MiB · 平均コア {}",
                            time(&r["at"]),
                            field(proc, "pid"),
                            field(proc, "app"),
                            cpu.map_or_else(|| "未取得".into(), |n| format!("{n:.0}")),
                            field(proc, "mem_mb"),
                            field(proc, "avg_core")
                        ),
                        "保存済み上位",
                        Tone::Normal,
                    );
                }
                if count == 0 {
                    row(p, &node.id, format!("{} · 上位プロセス情報なし", time(&r["at"])), "欠測・空", Tone::Muted);
                }
            }
            Page::Jobs => {
                for j in array(d, "jobs") {
                    row(
                        p,
                        format!("定期処理 · {} · {}", node.id, field(j, "name")),
                        format!(
                            "{} · {} · 前回 {} / 次回 {} · program {} · 結果 {}",
                            time(&r["at"]),
                            fields(j, &["kind", "schedule", "scope", "state"]),
                            timestamp_or_text(&j["last_run"]),
                            timestamp_or_text(&j["next_run"]),
                            field(j, "program"),
                            field(j, "last_result")
                        ),
                        field(j, "state"),
                        if tune_core::health::failing_jobs(std::slice::from_ref(j)).is_empty() { Tone::Normal } else { Tone::Error },
                    );
                }
                if array(d, "jobs").is_empty() {
                    row(p, &node.id, format!("{} · 定期処理情報なし", time(&r["at"])), "欠測・空", Tone::Muted);
                }
            }
            _ => unreachable!(),
        }
    }
    p.note.push_str(" 各機体の直近保存分析です。失敗・未分析・運用対象外を区別し、過去成功値で失敗を埋めません。プロセスは保存されたCPU・メモリ上位だけで全稼働一覧ではありません。Live・終了・定期処理変更は未接続です。");
    Ok(())
}

fn inventory(v: &Value, p: &mut Panel) {
    fact(p, "明示インストールの保存済み道具グループ数", array(v, "groups").len());
    fact(p, "最終棚卸し", time(&v["lastInventoryAt"]));
    fact(
        p,
        "Do-guキャッシュ",
        if v["dogu"].is_null() { "未取得".into() } else { format!("{} · {}", time(&v["dogu"]["at"]), fields(&v["dogu"], &["tools", "matched"])) },
    );
    for n in array(v, "nodes") {
        row(
            p,
            format!("棚卸し状態 · {}", field(n, "id")),
            format!("明示インストールの保存件数 {} · 最終成功 {} · 前回エラー {}", field(n, "count"), time(&n["last_ok_at"]), field(n, "error")),
            if n["error"].is_string() {
                "収集失敗"
            } else if n["last_ok_at"].is_null() {
                "未棚卸し"
            } else {
                "保存済み"
            },
            if n["error"].is_string() { Tone::Error } else { Tone::Muted },
        );
    }
    for g in array(v, "groups") {
        let cells = array(v, "nodes")
            .iter()
            .map(|n| {
                let id = field(n, "id");
                let c = &g["nodes"][&id];
                let version = if c.is_null() {
                    if n["last_ok_at"].is_null() {
                        "導入状況不明（未棚卸し）".into()
                    } else if n["error"].is_string() {
                        "前回の保存一覧にない（直近収集失敗）".into()
                    } else {
                        "未導入（保存一覧）".into()
                    }
                } else if array(c, "versions").is_empty() {
                    "導入済み・版不明".into()
                } else {
                    list(&c["versions"])
                };
                format!("{id}: {version}")
            })
            .collect::<Vec<_>>()
            .join(" · ");
        row(
            p,
            format!("道具 · {}", field(g, "name")),
            format!(
                "{} · slug {} / 分類 {} · source {} · 機体数 {}",
                cells,
                field(g, "slug"),
                field(g, "category"),
                list(&g["sources"]),
                field(g, "node_count")
            ),
            if g["drift"] == true { "版の差異あり" } else { "明示インストール" },
            if g["drift"] == true { Tone::Warning } else { Tone::Normal },
        );
    }
    for e in array(v, "events") {
        row(
            p,
            format!("棚卸し履歴 · {} · {}", field(e, "node_id"), field(e, "name")),
            format!("{} · source {} · {} → {}", time(&e["ts"]), field(e, "source"), field(e, "from_version"), field(e, "to_version")),
            field(e, "kind"),
            Tone::Normal,
        );
    }
    p.note.push_str(" 明示インストールだけ。Do-guはキャッシュ照合のみ。履歴は最新100件。セルの版不明と未導入は別で、前回失敗時の保存一覧は残ります。収集エラーはプロセス内の前回値です。");
}

fn ai_summary(v: &Value, p: &mut Panel) {
    fact(p, "30日集計・セッション", field(&v["totals"], "sessions"));
    fact(p, "30日推定費用", money(&v["totals"]["cost_usd"]));
    fact(p, "30日集計開始", time(&v["since"]));
    fact(p, "最終取込", time(&v["lastAiAt"]));
    fact(p, "取込状態", fields(v, &["ingesting", "pending"]));
    row(
        p,
        "30日使用量",
        format!(
            "{} · 単価不明モデル {}",
            fields(&v["totals"], &["prompts", "tool_calls", "tok_in", "tok_out", "tok_cache_read", "tok_cache_write", "tok_reasoning", "prices_version"]),
            list(&v["totals"]["unpriced_models"])
        ),
        "推定・重複除去後",
        Tone::Normal,
    );
    for m in array(v, "models") {
        row(
            p,
            format!("30日モデル · {} · {}", field(m, "tool"), field(m, "model")),
            format!("{} · {}", fields(m, &["sessions", "tok_in", "tok_out", "tok_cache_read", "tok_cache_write"]), money(&m["cost_usd"])),
            if m["price"].is_null() { "単価不明" } else { "推定" },
            if m["price"].is_null() { Tone::Warning } else { Tone::Normal },
        );
    }
    for n in array(v, "nodes") {
        let partial = n["truncated"] == true || n["bytes_pending"].as_u64().is_some_and(|n| n > 0) || !array(n, "file_errors").is_empty();
        let fail = n["last_error"].is_string();
        row(
            p,
            format!("AI取込状態 · {}", field(n, "id")),
            format!(
                "{} · 最終成功 {} / 試行 {} · エラー {} · ファイルエラー {} 件 · {}",
                fields(n, &["enabled", "limits_enabled", "truncated", "no_python"]),
                time(&n["last_ok_at"]),
                time(&n["updated_at"]),
                field(n, "last_error"),
                array(n, "file_errors").len(),
                fields(n, &["sessions", "files_total", "files_changed", "bytes_pending", "bytes_read"])
            ),
            if fail {
                "直近失敗"
            } else if partial {
                "部分取込"
            } else if n["enabled"] == false {
                "対象外"
            } else if n["last_ok_at"].is_null() {
                "未取込"
            } else {
                "保存済み"
            },
            if fail {
                Tone::Error
            } else if partial {
                Tone::Warning
            } else {
                Tone::Muted
            },
        );
        let l = &n["limits"];
        if l.is_object() {
            row(
                p,
                format!("残り枠の確認状態 · {}", field(n, "id")),
                format!("最終確認 {} · エラー {} · 取得版 {}", time(&l["checked_at"]), field(l, "error"), field(l, "probe_version")),
                field(l, "state"),
                state(&l["state"]),
            );
            for w in array(l, "windows") {
                let reset = w["resets_at"].as_i64().and_then(|n| n.checked_mul(1000));
                row(
                    p,
                    format!("残り枠の保存窓 · {} · {}分", field(n, "id"), field(w, "window_mins")),
                    format!("使用 {}% · リセット {} · 観測 {} · 0分は窓長不明", field(w, "used_pct"), time(&json!(reset)), time(&w["observed_at"])),
                    "保存済み・最新確認状態を参照",
                    Tone::Normal,
                );
            }
            if array(l, "windows").is_empty() {
                row(p, format!("残り枠 · {}", field(n, "id")), "窓の報告なし。無制限を意味しません。", "未報告", Tone::Muted);
            }
        }
    }
}

fn security(v: &Value, p: &mut Panel) {
    fact(p, "保存済み公開範囲の窓", format!("events {} / peers {} / logins {}", array(v, "events").len(), array(v, "peers").len(), array(v, "logins").len()));
    for n in array(v, "nodes") {
        row(
            p,
            format!("セキュリティ · {}", field(n, "id")),
            format!(
                "{} · enabled {} · 保存情報 {} · shared {}",
                time(&n["at"]),
                field(n, "enabled"),
                if n["netsec"].is_null() { "未取得・非公開" } else { "あり" },
                field(n, "shared")
            ),
            if n["enabled"] == false { "無効" } else { "保存済み" },
            Tone::Muted,
        );
        for c in array(n, "checks") {
            row(p, format!("セキュリティチェック · {} · {}", field(n, "id"), field(c, "name")), field(c, "detail"), field(c, "status"), state(&c["status"]));
        }
        for a in array(&n["netsec"], "listen") {
            row(
                p,
                format!("待ち受け · {} · {}", field(n, "id"), field(a, "proc")),
                format!("{} · アドレス {}", fields(a, &["port", "proto", "exposure"]), list(&a["addrs"])),
                "保存済み",
                Tone::Normal,
            );
        }
        if n["netsec"]["defense"].is_object() {
            let defense = &n["netsec"]["defense"];
            row(
                p,
                format!("防御 · {}", field(n, "id")),
                fields(defense, &["firewall", "stealth", "gatekeeper", "xprotect_version", "security_log", "detections_30d", "detections_open_30d"]),
                "保存済み",
                Tone::Normal,
            );
            if defense["defender"].is_object() {
                row(
                    p,
                    format!("Defender · {}", field(n, "id")),
                    fields(&defense["defender"], &["realtime", "antivirus", "service", "mode", "sig_age_days", "tamper"]),
                    "保存済み",
                    Tone::Normal,
                );
            }
            for key in ["av", "firewall"] {
                for a in array(defense, key) {
                    row(p, format!("防御 · {} · {key}", field(n, "id")), fields(a, &["name", "enabled", "uptodate"]), "保存済み", Tone::Normal);
                }
            }
        }
    }
    for e in array(v, "events") {
        row(
            p,
            format!("常駐履歴 · {} · {}", field(e, "node_id"), field(e, "kind")),
            format!("{} · {}", time(&e["ts"]), fields(e, &["key", "program", "from_program"])),
            field(e, "change"),
            Tone::Normal,
        );
    }
    for e in array(v, "peers") {
        row(
            p,
            format!("保存先通信 · {} · {}", field(e, "node_id"), field(e, "proc")),
            format!("{} · 初回 {} / 最終 {}", fields(e, &["addr", "port", "seen", "public"]), time(&e["first_seen"]), time(&e["last_seen"])),
            "許可された保存範囲",
            Tone::Normal,
        );
    }
    for e in array(v, "logins") {
        row(
            p,
            format!("ログイン · {} · {}", field(e, "node_id"), field(e, "provider")),
            format!("{} · 最終 {}", fields(e, &["public", "event_id", "n_24h", "n_7d", "source"]), time(&e["last_ts"])),
            "許可された保存範囲",
            Tone::Normal,
        );
    }
    p.note.push_str(" 現在の許可設定を適用したnetsec_viewだけを表示。events最大300 / peers最大300 / logins最大500の取得窓で全履歴ではありません。診断・遮断・変更は行いません。");
}

fn specs(v: &Value, p: &mut Panel) {
    fact(p, "台帳機体数", array(v, "nodes").len());
    for n in array(v, "nodes") {
        let report = &n["report"];
        row(
            p,
            format!("スペック · {}", field(n, "node_id")),
            format!(
                "取得 {} · eligible {} · 最終試行 {} · 成功 {} · エラー {}",
                time(&report["at"]),
                field(n, "eligible"),
                time(&n["last_attempt"]["at"]),
                field(&n["last_attempt"], "ok"),
                field(&n["last_attempt"], "error")
            ),
            if report.is_null() {
                "未取得"
            } else if n["stale"] == true {
                "古い保存値"
            } else {
                "保存済み"
            },
            if n["stale"] == true { Tone::Warning } else { Tone::Normal },
        );
        for (name, s) in report["sections"].as_object().into_iter().flatten() {
            row(
                p,
                format!("仕様区分 · {} · {name}", field(n, "node_id")),
                format!("source {} · component {} 件", field(s, "source"), array(s, "components").len()),
                field(s, "status"),
                state(&s["status"]),
            );
            for c in array(s, "components") {
                let detail = c["fields"].as_object().into_iter().flatten().map(|(k, v)| format!("{k}: {}", scalar(v))).collect::<Vec<_>>().join(" · ");
                row(
                    p,
                    format!("仕様 · {} · {} · {}", field(n, "node_id"), name, field(c, "name")),
                    format!("source {} · {detail}", field(c, "source")),
                    field(s, "status"),
                    state(&s["status"]),
                );
            }
        }
    }
    p.note.push_str(" 検証済み保存レポート。古さは24時間経過または最終失敗を含みます。失敗後も前回成功が残り、未取得は非搭載・ゼロではありません。公式最大仕様は実効性能順位ではありません。");
}

fn power(engine: &Engine, p: &mut Panel) -> Result<(), String> {
    let v = engine.power_report()?;
    let latest = engine.last()?;
    let config = engine.config();
    // The core fleet aggregation covers every ledger entry, including excluded
    // nodes. Hide that aggregation instead of publishing their old watts or
    // reimplementing a second calculation for a different target set.
    if config.nodes.iter().any(|n| !tune_core::nodes::op_state(&n.raw).active) {
        fact(p, "全機体の総電力", "運用対象外を含む集計のため非表示。運用対象の個別保存値を参照してください。");
    } else {
        fact(p, "全機体の総電力", v["fleet"]["total_w"].as_f64().map_or_else(|| "未取得（全機体の新鮮な値が揃っていません）".into(), |w| format!("{w:.1} W")));
        fact(p, "新鮮な数値がある機体 / 期待数", format!("{} / {}", field(&v["fleet"], "available"), field(&v["fleet"], "expected")));
        fact(p, "新鮮な値の部分合計", v["fleet"]["available_w"].as_f64().map_or_else(|| "未取得".into(), |w| format!("{w:.1} W（全体総量ではありません）")));
    }
    for n in array(&v, "nodes") {
        let Some(node) = config.nodes.iter().find(|node| n["node_id"] == node.id) else { continue };
        let op = tune_core::nodes::op_state(&node.raw);
        if !op.active {
            row(p, &node.id, format!("{} · {}", tune_core::nodes::state_label(&op.state), op.reason), "運用対象外", Tone::Muted);
            continue;
        }
        let Some(last) = latest.iter().find(|last| last["node_id"] == node.id) else {
            row(p, &node.id, "保存済み分析なし", "未分析", Tone::Muted);
            continue;
        };
        if last["ok"] != true {
            row(p, &node.id, format!("{} · {}", time(&last["at"]), field(last, "error")), "直近分析失敗", Tone::Error);
            continue;
        }
        let s = &n["summary"];
        row(
            p,
            format!("電力 · {}", field(n, "node_id")),
            format!(
                "{} · fresh {} · source {} · {} · 欠測 {}",
                time(&n["at"]),
                field(n, "fresh"),
                field(s, "source"),
                fields(s, &["total_w", "wall_w", "soc_w", "cpu_w", "gpu_w"]),
                list(&s["missing"])
            ),
            if s["kind"] == "missing" { "missing（総電力未算出）".into() } else { field(s, "kind") },
            if n["fresh"] == false { Tone::Warning } else { state(&s["kind"]) },
        );
        let data = &n["data"];
        row(
            p,
            format!("CPU・SoC センサー · {}", field(n, "node_id")),
            fields(&data["power"], &["package_w", "package_source", "soc_w", "soc_source", "platform_w", "platform_source", "wall_w"]),
            "保存済み・取得元",
            Tone::Normal,
        );
        row(
            p,
            format!("CPU Clock · {}", field(n, "node_id")),
            format!(
                "実効 {} MHz / 基準 {} MHz · source {} · E {} MHz / P {} MHz",
                field(&data["cpu_clock"], "effective_mhz"),
                field(&data["cpu_clock"], "nominal_mhz"),
                field(&data["cpu_clock"], "source"),
                field(&data["cpu_clock"]["clusters_mhz"], "e"),
                field(&data["cpu_clock"]["clusters_mhz"], "p")
            ),
            "保存済み・読み取り専用",
            Tone::Normal,
        );
        for g in array(data, "gpus") {
            row(
                p,
                format!("GPU Clock · {} · {}", field(n, "node_id"), field(g, "name")),
                format!(
                    "Graphics {} / SM {} / Memory {} MHz · {} W · source {}",
                    field(g, "clocks_graphics_mhz"),
                    field(g, "clocks_sm_mhz"),
                    field(g, "clocks_memory_mhz"),
                    field(g, "power_w"),
                    field(g, "source")
                ),
                "保存済み・読み取り専用",
                Tone::Normal,
            );
        }
        if !n["observation"].is_null() {
            let o = &n["observation"];
            row(
                p,
                format!("保存観測 · {}", field(n, "node_id")),
                format!("{} → {} · {} 点", time(&o["first_at"]), time(&o["last_at"]), field(o, "samples")),
                "保存済み",
                Tone::Muted,
            );
            for key in ["gpu", "cpu", "soc", "whole"] {
                row(
                    p,
                    format!("保存観測 · {} · {key}", field(n, "node_id")),
                    fields(&o[key], &["kwh", "covered_seconds", "duration_seconds", "coverage"]),
                    "保存済み積分",
                    Tone::Muted,
                );
            }
        }
    }
    p.note.push_str(" measured / estimated / missingを保持。freshは保存から5分以内（未来5秒まで）。欠測を0にせず、入力側・SoC・CPU・GPUを区別します。Liveの新規観測は行っていません。");
    Ok(())
}

fn topology(v: &Value, p: &mut Panel) {
    let w = &v["window"];
    for (label, key) in [("保存総数（表示・検索一致数とは別）", "stored_totals"), ("検索一致の保存数（表示上限適用前）", "filtered_stored_totals")]
    {
        fact(p, label, fields(&w[key], &["accepted_entities", "accepted_edges", "accepted_closed_edges", "candidate_entities", "candidate_edges"]));
    }
    fact(p, "検証範囲", field(w, "validation_scope"));
    fact(p, "有界射影の状態", fields(w, &["selected_rows_checked", "corrupt_scopes_hidden", "truncated", "full_archive_checked", "unselected_rows_checked"]));
    for key in ["omission_reasons", "candidate_omission_reasons", "candidate_unavailable_reasons"] {
        fact(p, key, list(&w[key]));
    }
    topology_records(p, v, "採用済み", "");
    for c in array(v, "candidates") {
        let scope = field(c, "scope_id");
        row(
            p,
            format!("候補グループ · {scope}"),
            format!(
                "観測 {} · 試行終了 {} · {}",
                time(&c["facts_observed_at_ms"]),
                time(&c["attempt_finished_at_ms"]),
                fields(c, &["attempt_id", "is_latest_attempt", "validation_scope"])
            ),
            "部分候補・未採用",
            Tone::Warning,
        );
        topology_records(p, c, "候補・未採用", &scope);
    }
    for key in ["scopes", "history", "changes"] {
        for m in array(v, key) {
            row(
                p,
                format!("構成メタデータ · {key} · {}", field(m, "scope_id")),
                format!(
                    "{} · {} · 件数 {} · 失敗コード {}",
                    time(m.get("observed_at").unwrap_or(&m["at"])),
                    fields(m, &["id", "kind", "change_kind", "status", "run_id"]),
                    fields(&m["counters"], &["entries", "files", "bytes"]),
                    array(m, "failures").iter().filter_map(|f| f["code"].as_str()).collect::<Vec<_>>().join(" / ")
                ),
                field(m, "status"),
                state(&m["status"]),
            );
        }
    }
    p.note.push_str(" bounded UI projectionだけ。entities最大200 / edges最大200 / 応答1MiB、表示値・メタデータも省略されます。採用済みと部分候補は別です。検証は選択された射影行だけで全アーカイブ・未選択行の検証ではなく、完全な構成図・公開用exportではありません。");
}
fn topology_records(p: &mut Panel, v: &Value, badge: &str, scope: &str) {
    for e in array(v, "entities") {
        let scopes = if !array(e, "scope_ids").is_empty() {
            list(&e["scope_ids"])
        } else if scope.is_empty() {
            field(e, "scope_id")
        } else {
            scope.into()
        };
        row(
            p,
            format!("{badge} · {} · {}", field(e, "kind"), field(e, "label")),
            format!(
                "ID {} · 範囲 {} · 観測 {} · {}",
                field(e, "id"),
                scopes,
                time(&e["facts_observed_at_ms"]),
                fields(&e["attributes"], &["state", "health", "revision", "ports_total", "ports_omitted", "attributes_truncated"])
            ),
            badge,
            if badge == "採用済み" { Tone::Normal } else { Tone::Warning },
        );
    }
    for e in array(v, "edges") {
        let scopes = if !array(e, "scope_ids").is_empty() {
            list(&e["scope_ids"])
        } else if scope.is_empty() {
            field(e, "scope_id")
        } else {
            scope.into()
        };
        row(
            p,
            format!("{badge} 関係 · {}", field(e, "kind")),
            format!("{} → {} · 範囲 {} · 観測 {}", field(e, "from"), field(e, "to"), scopes, time(&e["facts_observed_at_ms"])),
            badge,
            Tone::Muted,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, sync::Arc};
    struct Fixture {
        engine: Arc<Engine>,
        dir: PathBuf,
    }
    impl Fixture {
        fn new(nodes: Value) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("tune-gpui-panels-{}-{}-{sequence}", std::process::id(), tune_core::db::now_ms()));
            std::fs::create_dir(&dir).unwrap();
            let config = dir.join("nodes.json");
            std::fs::write(&config, json!({"nodes":nodes}).to_string()).unwrap();
            let engine = Engine::open(config, dir.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap();
            Self { engine, dir }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    fn text(panel: &Panel) -> String {
        format!("{panel:?}")
    }

    #[test]
    fn log_level_filters_in_sql_before_the_raw_row_budget_and_combines_with_search() {
        let f = Fixture::new(json!([{"id":"example-node","os":"windows","alias":"unused"}]));
        let now = tune_core::db::now_ms();
        f.engine
            .with_db(|d| {
                let mut logs = (0..2000).map(|i| json!({"uid":format!("error-{i}"),"ts":now+i,"level":"error","message":"error needle"})).collect::<Vec<_>>();
                logs.push(json!({"uid":"older-warning","ts":now-1,"level":"warn","message":"WARN_SENTINEL needle"}));
                d.insert_logs("example-node", "win_system", &tune_core::logs::normalize("win_system", &logs), now)?;
                Ok(())
            })
            .unwrap();
        let panel = load(&f.engine, Page::Logs, "needle", LogLevel::Warn).unwrap();
        let records: Vec<_> = panel.rows.iter().filter(|r| r.title.starts_with("ログ ·")).collect();
        assert_eq!(records.len(), 1);
        assert!(records[0].detail.contains("WARN_SENTINEL"));
    }

    #[test]
    fn every_saved_page_loads_empty_without_recompute_or_collection() {
        let f = Fixture::new(json!([]));
        let before = f.engine.with_db(|d| Ok((d.checks()?, d.check_events(150)?, d.cursors()?))).unwrap();
        for page in Page::ALL.into_iter().filter(|p| *p != Page::Overview) {
            let p = load(&f.engine, page, "", LogLevel::All).unwrap_or_else(|e| panic!("{page:?}: {e}"));
            assert!(!p.note.is_empty(), "{page:?}");
        }
        let after = f.engine.with_db(|d| Ok((d.checks()?, d.check_events(150)?, d.cursors()?))).unwrap();
        assert_eq!(before, after);
    }
    #[test]
    fn nullable_power_and_ai_prices_never_become_zero() {
        let f = Fixture::new(json!([{"id":"example-node","os":"macos","alias":"unused"}]));
        let power = load(&f.engine, Page::Power, "", LogLevel::All).unwrap();
        assert!(text(&power).contains("未取得"));
        assert!(!text(&power).contains("0 W"));
        let ai = load(&f.engine, Page::Ai, "", LogLevel::All).unwrap();
        assert!(text(&ai).contains("単価不明"));
        assert!(!text(&ai).contains("$0"));
    }
    #[test]
    fn engine_privacy_filters_login_logs_signatures_cursors_and_checks() {
        let f = Fixture::new(json!([{"id":"example-node","os":"windows","alias":"unused","network":false}]));
        let now = tune_core::db::now_ms();
        f.engine
            .with_db(|d| {
                let logs = tune_core::logs::normalize(
                    "win_security",
                    &[json!({"uid":"private-log","ts":now,"level":"error","message":"PRIVATE_SENTINEL","provider":"PRIVATE_SENTINEL"})],
                );
                d.insert_logs("example-node", "win_security", &logs, now)?;
                d.cursor_ok("example-node", "win_security", None, 1, &json!(0), Some("PRIVATE_SENTINEL"))?;
                d.save_checks(
                    "example-node",
                    &[tune_core::db::Check {
                        id: "sec-login".into(),
                        name: "PRIVATE_SENTINEL".into(),
                        status: "fail".into(),
                        detail: Some("PRIVATE_SENTINEL".into()),
                    }],
                    now,
                )?;
                Ok(())
            })
            .unwrap();
        for page in [Page::Logs, Page::Status, Page::Security] {
            assert!(!text(&load(&f.engine, page, "", LogLevel::All).unwrap()).contains("PRIVATE_SENTINEL"));
        }
    }
    #[test]
    fn invalid_queries_are_rejected_before_reading_any_page() {
        let f = Fixture::new(json!([]));
        for page in Page::ALL {
            assert!(load(&f.engine, page, "bad\nquery", LogLevel::All).unwrap_err().contains("検索"));
            assert!(load(&f.engine, page, &"あ".repeat(86), LogLevel::All).unwrap_err().contains("検索"));
        }
    }
    #[test]
    fn incomplete_actions_are_distinct_and_log_fts_errors_are_visible() {
        let f = Fixture::new(json!([]));
        f.engine
            .with_db(|d| d.add_action(&json!({"id":"example-action","at":1,"node_id":"example-node","type":"example","label":"pending action","ok":null})))
            .unwrap();
        let actions = load(&f.engine, Page::Actions, "", LogLevel::All).unwrap();
        assert_eq!(actions.rows[0].badge, "未完了");
        assert_eq!(actions.rows[0].tone, Tone::Warning);
        assert!(load(&f.engine, Page::Logs, "\"", LogLevel::All).is_err());
    }
    #[test]
    fn failed_and_excluded_snapshots_do_not_show_previous_processes_or_jobs() {
        let f = Fixture::new(json!([
            {"id":"example-failed","alias":"unused","os":"macos"},
            {"id":"example-paused","alias":"unused","os":"macos","status":"paused"},
            {"id":"example-active","alias":"unused","os":"windows"}
        ]));
        let now = tune_core::db::now_ms();
        f.engine.with_db(|d| {
            for id in ["example-failed","example-paused"] {
                d.add_snapshot(id, now-1000,None,None,&json!([]),&json!({"processes":{"top_cpu":[{"pid":1,"name":"OLD_SENTINEL"}]},"jobs":[{"name":"OLD_SENTINEL"}]}),&json!({}))?;
            }
            d.save_probe_failure("example-failed",now,"synthetic connection unavailable")?;
            d.add_snapshot("example-active",now,None,None,&json!([]),&json!({"probe":"windows","host":{"cores":4},"processes":{"top_cpu":[{"pid":2,"name":"example-process","cpu":25,"mem_mb":null}],"top_mem":[{"pid":2,"name":"example-process"}]},"disk":[{"mount":"example-volume","free_gb":4,"total_gb":10}]}),&json!({}))?;
            Ok(())
        }).unwrap();
        for page in [Page::Resources, Page::Processes, Page::Jobs] {
            let panel = load(&f.engine, page, "", LogLevel::All).unwrap();
            assert!(!text(&panel).contains("OLD_SENTINEL"));
            assert!(panel.rows.iter().any(|r| r.badge == "直近分析失敗"));
            assert!(panel.rows.iter().any(|r| r.badge == "運用対象外"));
        }
        let procs = load(&f.engine, Page::Processes, "example-process", LogLevel::All).unwrap();
        assert_eq!(procs.rows.len(), 1);
        assert!(procs.rows[0].detail.contains("CPU 100%"));
        assert!(procs.rows[0].detail.contains("メモリ 未取得 MiB"));
        assert!(text(&load(&f.engine, Page::Resources, "", LogLevel::All).unwrap()).contains("example-volume"));
    }

    #[test]
    fn temporary_log_collection_failure_keeps_saved_health_and_last_success() {
        let f = Fixture::new(json!([{"id":"example-node","alias":"unused","os":"macos"}]));
        let now = tune_core::db::now_ms();
        f.engine
            .with_db(|d| {
                d.cursor_ok("example-node", "mac_kernel", Some(&json!("opaque")), 0, &json!(4), Some("not-found-for-account"))?;
                d.cursor_error("example-node", "mac_kernel", "synthetic temporary error")?;
                d.save_checks(
                    "example-node",
                    &[tune_core::db::Check {
                        id: "logs".into(),
                        name: "saved logs".into(),
                        status: "ok".into(),
                        detail: Some("temporary error retained in detail".into()),
                    }],
                    now,
                )?;
                Ok(())
            })
            .unwrap();
        let logs = load(&f.engine, Page::Logs, "", LogLevel::All).unwrap();
        let cursor = logs.rows.iter().find(|r| r.title.starts_with("収集状態")).unwrap();
        assert_eq!(cursor.tone, Tone::Error);
        assert!(cursor.detail.contains("not-found-for-account"));
        assert!(cursor.detail.contains("synthetic temporary error"));
        assert!(!cursor.detail.contains("最終成功 未取得"));
        let status = load(&f.engine, Page::Status, "", LogLevel::All).unwrap();
        assert!(status.rows.iter().any(|r| r.title.starts_with("チェック") && r.badge == "ok" && r.detail.contains("temporary error retained")));
    }

    #[test]
    fn ai_partial_ingest_and_failed_limit_refresh_preserve_times_and_unknown_prices() {
        use tune_core::db::{LimitWindow, LimitsReport};
        let f = Fixture::new(json!([{"id":"example-node","alias":"unused","os":"macos"}]));
        let now = 1_800_000_000_000_i64;
        f.engine.with_db(|d| {
            d.ai_ingest("example-node",&json!({"sessions":[],"cursors":{},"gone":[],"truncated":true,"bytes_pending":900,"errors":[{"file":"example","error":"synthetic partial file"}]}),now)?;
            d.ai_error("example-node","synthetic latest AI failure",false,now+1000)?;
            d.save_limits("example-node","codex",&LimitsReport {state:"ok".into(),windows:vec![LimitWindow {mins:Some(300),used_pct:125.0,resets_at:Some(1_800_003_600)}],..Default::default()},now)?;
            d.save_limits("example-node","codex",&LimitsReport {state:"error".into(),error:Some("synthetic latest limit failure".into()),..Default::default()},now+1000)
        }).unwrap();
        let panel = load(&f.engine, Page::Ai, "", LogLevel::All).unwrap();
        let cursor = panel.rows.iter().find(|r| r.title.starts_with("AI取込状態")).unwrap();
        assert_eq!(cursor.badge, "直近失敗");
        assert!(cursor.detail.contains("続きあり・上限到達: はい"));
        assert!(cursor.detail.contains("ファイルエラー 1 件"));
        assert!(cursor.detail.contains("synthetic latest AI failure"));
        let limit = panel.rows.iter().find(|r| r.title.starts_with("残り枠の保存窓")).unwrap();
        assert!(limit.detail.contains("125%"));
        assert!(limit.detail.contains(&time(&json!(1_800_003_600_000_i64))));
        assert!(limit.detail.contains(&time(&json!(now))));
        assert!(text(&panel).contains("synthetic latest limit failure"));
        assert!(text(&panel).contains("単価不明"));
    }

    #[test]
    fn inventory_unknown_version_is_distinct_from_absent_installation_and_drift() {
        let mut p = Panel { facts: vec![], rows: vec![], note: String::new() };
        inventory(
            &json!({"nodes":[{"id":"example-a","count":1,"last_ok_at":1800000000000_i64},{"id":"example-b","count":0,"last_ok_at":1800000000000_i64},{"id":"example-never","count":0,"last_ok_at":null}],"groups":[{"name":"Example Tool","node_count":1,"drift":true,"nodes":{"example-a":{"versions":[]}}}],"events":[],"dogu":null}),
            &mut p,
        );
        let tool = p.rows.iter().find(|r| r.title.starts_with("道具")).unwrap();
        assert!(tool.detail.contains("example-a: 導入済み・版不明"));
        assert!(tool.detail.contains("example-b: 未導入"));
        assert!(tool.detail.contains("example-never: 導入状況不明（未棚卸し）"));
        assert!(p.rows.iter().any(|r| r.title.contains("example-never") && r.badge == "未棚卸し"));
        assert_eq!(tool.tone, Tone::Warning);
    }

    #[test]
    fn topology_keeps_candidates_separate_and_does_not_claim_archive_validation() {
        let mut p = Panel { facts: vec![], rows: vec![], note: String::new() };
        topology(
            &json!({"entities":[{"id":"example-accepted","kind":"host","label":"Accepted Example","scope_ids":["example-accepted-scope"],"facts_observed_at_ms":1800000000000_i64}],"candidates":[{"scope_id":"example-scope","entities":[{"id":"example-candidate","kind":"host","label":"Candidate Example"}],"status":"partial"}],"window":{"validation_scope":"selected_projection_rows","full_archive_checked":false,"omission_reasons":["entity_budget"],"stored_totals":{"accepted_entities":400},"filtered_stored_totals":{"accepted_entities":300}}}),
            &mut p,
        );
        assert!(p.rows.iter().any(|r| r.badge == "採用済み" && r.title.contains("Accepted Example")));
        assert!(p.rows.iter().any(|r| r.badge == "候補・未採用" && r.title.contains("Candidate Example")));
        assert!(text(&p).contains("selected_projection_rows"));
        assert!(
            p.rows.iter().any(|r| r.badge == "採用済み" && r.detail.contains("example-accepted-scope") && r.detail.contains(&time(&json!(1800000000000_i64))))
        );
        assert!(text(&p).contains("entity_budget"));
        assert!(p.note.contains("全アーカイブ・未選択行の検証ではなく"));
    }

    #[test]
    fn privacy_after_raw_log_limit_is_not_reported_as_end_of_history() {
        let f = Fixture::new(json!([{"id":"example-node","alias":"unused","os":"windows","network":false}]));
        let now = tune_core::db::now_ms();
        f.engine
            .with_db(|d| {
                let hidden = (0..2000).map(|i| json!({"uid":format!("hidden-{i}"),"ts":now-i,"level":"error","message":"hidden login"})).collect::<Vec<_>>();
                d.insert_logs("example-node", "win_security", &tune_core::logs::normalize("win_security", &hidden), now)?;
                d.insert_logs(
                    "example-node",
                    "win_system",
                    &tune_core::logs::normalize("win_system", &[json!({"uid":"older-public","ts":now-3000,"message":"OLDER_VISIBLE_RECORD"})]),
                    now,
                )?;
                Ok(())
            })
            .unwrap();
        let panel = load(&f.engine, Page::Logs, "", LogLevel::All).unwrap();
        assert_eq!(panel.rows.iter().filter(|r| r.title.starts_with("ログ ·")).count(), 0);
        assert!(!panel.note.contains("すべて表示"));
        assert!(panel.note.contains("全履歴の総数・末尾を意味しません"));
        assert!(!f.engine.logs_query(&json!({"source":"win_system"})).unwrap().is_empty());
    }

    #[test]
    fn specs_keeps_last_valid_report_with_failed_attempt_and_section_components() {
        let f = Fixture::new(json!([{"id":"example-node","alias":"unused","os":"macos"}]));
        let now = tune_core::db::now_ms();
        let report = tune_core::specs::normalize(
            "macos",
            &json!({"SPHardwareDataType":[{"machine_model":"Example Device","chip_type":"Example Processor","number_processors":8}]}),
        );
        assert_eq!(report["ok"], true);
        f.engine
            .with_db(|d| {
                d.set_meta("specsReport:example-node", &report)?;
                d.set_meta("specsAttempt:example-node", &json!({"ok":false,"at":now,"error":"synthetic latest specs failure"}))
            })
            .unwrap();
        let panel = load(&f.engine, Page::Specs, "", LogLevel::All).unwrap();
        assert!(panel.rows.iter().any(|r| r.badge == "古い保存値" && r.detail.contains("synthetic latest specs failure")));
        assert!(panel.rows.iter().any(|r| r.title.starts_with("仕様区分")));
        assert!(text(&panel).contains("Example Processor"));
    }

    #[test]
    fn searches_use_fts_only_for_logs_and_local_matching_preserves_fact_scope() {
        let f = Fixture::new(json!([]));
        f.engine.with_db(|d| {
            for i in 0..101 {d.add_action(&json!({"id":format!("example-{i}"),"at":i,"node_id":"example-node","label":if i==100 {"検索対象"} else {"other"},"type":"example","ok":true}))?;}
            Ok(())
        }).unwrap();
        let panel = load(&f.engine, Page::Actions, "検索対象", LogLevel::All).unwrap();
        assert_eq!(panel.rows.len(), 1);
        assert!(panel.facts.iter().any(|(k, v)| k.contains("最大100") && v == "100"));
        assert!(panel.note.contains("上部の集計値は検索前"));
        let panel = load(&f.engine, Page::Actions, "other", LogLevel::All).unwrap();
        assert_eq!(panel.rows.len(), 99);
    }
    #[test]
    fn power_preserves_failure_exclusion_and_missing_sensor_states() {
        let f = Fixture::new(json!([
            {"id":"example-failed","alias":"unused","os":"macos"},
            {"id":"example-excluded","alias":"unused","os":"macos","status":"paused"},
            {"id":"example-never","alias":"unused","os":"macos"},
            {"id":"example-sensor-missing","alias":"unused","os":"macos"},
            {"id":"example-settings-missing","alias":"unused","os":"macos"}
        ]));
        let now = tune_core::db::now_ms();
        f.engine
            .with_db(|d| {
                for id in ["example-failed", "example-excluded"] {
                    d.add_snapshot(
                        id,
                        now - 1000,
                        None,
                        None,
                        &json!([]),
                        &json!({"power":{"wall_w":1111},"cpu_clock":{"source":"OLD_POWER_SENTINEL"}}),
                        &json!({}),
                    )?;
                }
                d.save_probe_failure("example-failed", now, "synthetic latest power analysis failure")?;
                d.add_snapshot("example-sensor-missing", now, None, None, &json!([]), &json!({}), &json!({}))?;
                d.add_snapshot("example-settings-missing", now, None, None, &json!([]), &json!({"power":{"soc_w":30}}), &json!({}))
            })
            .unwrap();
        let p = load(&f.engine, Page::Power, "", LogLevel::All).unwrap();
        assert!(p.rows.iter().any(|r| r.title.contains("example-failed")
            && r.badge == "直近分析失敗"
            && r.detail.contains("synthetic latest power analysis failure")
            && r.detail.contains(&time(&json!(now)))));
        assert!(p.rows.iter().any(|r| r.title.contains("example-excluded") && r.badge == "運用対象外"));
        assert!(p.rows.iter().any(|r| r.title.contains("example-never") && r.badge == "未分析"));
        assert!(p.rows.iter().any(|r| r.title.contains("example-sensor-missing") && r.badge == "missing（総電力未算出）"));
        let settings = p.rows.iter().find(|r| r.title == "電力 · example-settings-missing").unwrap();
        assert_eq!(settings.badge, "missing（総電力未算出）");
        assert!(settings.detail.contains("SoC電力（W）: 30"));
        assert!(settings.detail.contains("base_w"));
        assert!(settings.detail.contains("psu_efficiency"));
        assert!(!text(&p).contains("OLD_POWER_SENTINEL"));
        assert!(!text(&p).contains("1111"));
    }

    #[test]
    fn windows_cpu_partial_observations_show_quality_and_sample_count() {
        let f = Fixture::new(json!([{"id":"example-node","alias":"unused","os":"windows"}]));
        f.engine
            .with_db(|d| {
                d.add_snapshot(
                    "example-node",
                    tune_core::db::now_ms(),
                    None,
                    None,
                    &json!([]),
                    &json!({"cpu_busy":25,"cpu_busy_availability":"partial","cpu_busy_samples":1}),
                    &json!({}),
                )
            })
            .unwrap();
        let p = load(&f.engine, Page::Resources, "", LogLevel::All).unwrap();
        let row = p.rows.iter().find(|r| r.title.starts_with("リソース")).unwrap();
        assert!(row.detail.contains("部分観測"));
        assert!(row.detail.contains("1 / 2"));
        assert_eq!(row.tone, Tone::Warning);
    }
}
