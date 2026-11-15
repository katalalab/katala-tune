//! 画面に出す形への変換。Engine が返す JSON（`last` / `status`）を、描画に必要な値だけにする。
//! GPUI に依存しないので、画面なしでテストできる。

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Critical,
    Warn,
    Info,
}

impl Severity {
    fn parse(s: &str) -> Self {
        match s {
            "critical" => Self::Critical,
            "warn" => Self::Warn,
            _ => Self::Info,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Critical => "要対応",
            Self::Warn => "注意",
            Self::Info => "情報",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub advice: String,
    /// 提案された変更操作（tune-core の許可リストの形）と、その表示名。共用機の所見には付かない
    pub action: Option<Value>,
    pub action_label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Health {
    /// まだ一度も分析していない
    Unobserved,
    /// 直近の分析が失敗した
    Failed(String),
    Scored(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Disk {
    pub mount: String,
    pub free_gb: f64,
    pub total_gb: f64,
    pub free_pct: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppUse {
    pub app: String,
    pub count: i64,
    pub cpu: f64,
    pub mem_mb: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NodeView {
    pub id: String,
    pub os: String,
    pub role: String,
    pub health: Health,
    pub at_ms: Option<i64>,
    pub cpu_busy: Option<f64>,
    pub mem_avail_pct: Option<f64>,
    pub uptime_h: Option<f64>,
    pub swap_gb: Option<f64>,
    pub cores: Option<i64>,
    pub cpu_name: String,
    pub disks: Vec<Disk>,
    /// 直近の分析時点のアプリ別の使用量（メモリの多い順に Engine が並べたもの）
    pub apps: Vec<AppUse>,
    pub agent_processes: Option<i64>,
    pub findings: Vec<Finding>,
    /// 直近の履歴（古い順）。画面の推移グラフ用
    pub score_trend: Vec<f64>,
    pub cpu_trend: Vec<f64>,
    pub mem_trend: Vec<f64>,
}

impl NodeView {
    pub fn count(&self, s: Severity) -> usize {
        self.findings.iter().filter(|f| f.severity == s).count()
    }

    /// 並べ替えの鍵。要対応が多い順、次に注意、次にスコアの低い順
    pub fn urgency(&self) -> (std::cmp::Reverse<usize>, std::cmp::Reverse<usize>, u32) {
        let score = match self.health {
            Health::Scored(s) => s,
            _ => 0,
        };
        (std::cmp::Reverse(self.count(Severity::Critical)), std::cmp::Reverse(self.count(Severity::Warn)), score)
    }
}

fn str_of(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default().to_string()
}

fn finding(v: &Value) -> Finding {
    Finding {
        severity: Severity::parse(v.get("severity").and_then(Value::as_str).unwrap_or("")),
        title: str_of(v.get("title")),
        detail: str_of(v.get("detail")),
        advice: str_of(v.get("advice")),
        action: v.get("action").filter(|a| a.get("type").is_some()).cloned(),
        action_label: str_of(v.get("action").and_then(|a| a.get("label"))),
    }
}

/// `Engine::config` の機体一覧（台帳）と `Engine::last` の分析結果を、機体ごとの表示値にまとめる。
/// 台帳の順を保ち、まだ分析していない機体も出す。
pub fn build(ledger_nodes: &[Value], last: &[Value]) -> Vec<NodeView> {
    ledger_nodes
        .iter()
        .map(|n| {
            let id = str_of(n.get("id"));
            let r = last.iter().find(|r| r.get("node_id").and_then(Value::as_str) == Some(id.as_str()));
            let data = r.and_then(|r| r.get("data"));
            let health = match r {
                None => Health::Unobserved,
                Some(r) if r.get("ok").and_then(Value::as_bool) != Some(true) => Health::Failed(str_of(r.get("error"))),
                Some(r) => Health::Scored(r.get("score").and_then(Value::as_u64).map_or(0, |s| s.min(100) as u32)),
            };
            let findings = r
                .and_then(|r| r.get("findings"))
                .and_then(Value::as_array)
                .map(|a| a.iter().map(finding).collect())
                .unwrap_or_default();
            NodeView {
                id,
                os: str_of(n.get("os")),
                role: str_of(n.get("role")),
                health,
                at_ms: r.and_then(|r| r.get("at")).and_then(Value::as_i64),
                cpu_busy: data.and_then(|d| d.get("cpu_busy")).and_then(Value::as_f64),
                mem_avail_pct: data.and_then(|d| d.get("memory")).and_then(|m| m.get("available_pct")).and_then(Value::as_f64),
                uptime_h: data.and_then(|d| d.get("host")).and_then(|h| h.get("uptime_h")).and_then(Value::as_f64),
                swap_gb: data.and_then(|d| d.get("memory")).and_then(|m| m.get("swap_used_gb")).and_then(Value::as_f64),
                cores: data.and_then(|d| d.get("host")).and_then(|h| h.get("cores")).and_then(Value::as_i64),
                cpu_name: str_of(data.and_then(|d| d.get("host")).and_then(|h| h.get("cpu"))),
                disks: data
                    .and_then(|d| d.get("disk"))
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|d| Disk {
                                mount: str_of(d.get("mount")),
                                free_gb: d.get("free_gb").and_then(Value::as_f64).unwrap_or(0.0),
                                total_gb: d.get("total_gb").and_then(Value::as_f64).unwrap_or(0.0),
                                free_pct: d.get("free_pct").and_then(Value::as_f64).unwrap_or(0.0),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                apps: data
                    .and_then(|d| d.get("processes"))
                    .and_then(|p| p.get("apps"))
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|x| AppUse {
                                app: one_line(&str_of(x.get("app")), 60),
                                count: x.get("count").and_then(Value::as_i64).unwrap_or(0),
                                cpu: x.get("cpu").and_then(Value::as_f64).unwrap_or(0.0),
                                mem_mb: x.get("mem_mb").and_then(Value::as_f64).unwrap_or(0.0),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                agent_processes: data.and_then(|d| d.get("processes")).and_then(|p| p.get("agent_processes")).and_then(Value::as_i64),
                findings,
                score_trend: Vec::new(),
                cpu_trend: Vec::new(),
                mem_trend: Vec::new(),
            }
        })
        .collect()
}

/// 履歴の行（`Store::history`）から 1 つの列を取り出す。値の無い行は飛ばす
pub fn series(rows: &[Value], key: &str) -> Vec<f64> {
    rows.iter().filter_map(|r| r.get(key).and_then(Value::as_f64)).collect()
}

/// 「3 分前」のような相対表示
pub fn ago(now_ms: i64, at_ms: i64) -> String {
    let s = ((now_ms - at_ms) / 1000).max(0);
    match s {
        0..=59 => "たった今".into(),
        60..=3599 => format!("{} 分前", s / 60),
        3600..=86_399 => format!("{} 時間前", s / 3600),
        _ => format!("{} 日前", s / 86_400),
    }
}

pub fn uptime(h: f64) -> String {
    if h >= 48.0 { format!("{:.0} 日", h / 24.0) } else { format!("{h:.0} 時間") }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LogRow {
    pub node: String,
    pub ts_ms: i64,
    pub level: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Signature {
    pub level: String,
    pub sample: String,
    pub count: i64,
    pub nodes: i64,
}

/// 1 行 1 画面行に収める（長いログは先頭の 1 行だけ、char 単位で切る）
fn one_line(s: &str, max: usize) -> String {
    let first = s.lines().next().unwrap_or("");
    if first.chars().count() > max { first.chars().take(max).collect::<String>() + "…" } else { first.to_string() }
}

pub fn log_rows(rows: &[Value]) -> Vec<LogRow> {
    rows.iter()
        .map(|r| LogRow {
            node: str_of(r.get("node_id")),
            ts_ms: r.get("ts").and_then(Value::as_f64).map_or(0, |t| t as i64),
            level: str_of(r.get("level")),
            message: one_line(&str_of(r.get("message")), 200),
        })
        .collect()
}

pub fn signatures(rows: &[Value]) -> Vec<Signature> {
    rows.iter()
        .map(|r| Signature {
            level: str_of(r.get("level")),
            sample: one_line(&str_of(r.get("sample")), 160),
            count: r.get("n").and_then(Value::as_i64).unwrap_or(0),
            nodes: r.get("nodes").and_then(Value::as_i64).unwrap_or(0),
        })
        .collect()
}

pub fn level_label(l: &str) -> &'static str {
    match l {
        "critical" => "重大",
        "error" => "エラー",
        "warn" => "警告",
        _ => "情報",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScopeRow {
    pub id: String,
    pub status: String,
    pub observed_ms: Option<i64>,
}

/// 構成図の内部表示（`Engine::topology_ui_view`）を、表示に必要な分だけにしたもの。
/// 表示は上限付きなので、保存されている件数（stored_*）と必ず並べて出す。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TopologyView {
    pub scopes: Vec<ScopeRow>,
    pub entities: Vec<(String, String)>,
    pub kind_counts: Vec<(String, usize)>,
    pub edges_shown: usize,
    pub stored_entities: i64,
    pub stored_edges: i64,
    pub history: usize,
}

pub fn scope_status_label(s: &str) -> &'static str {
    match s {
        "complete" => "完了",
        "partial" => "一部のみ",
        "failed" => "失敗",
        "excluded" => "対象外",
        _ => "未取得",
    }
}

pub fn topology(v: &Value) -> TopologyView {
    let arr = |k: &str| v.get(k).and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let entities: Vec<(String, String)> = arr("entities").iter().map(|e| (str_of(e.get("kind")), str_of(e.get("label")))).collect();
    let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
    for (k, _) in &entities {
        *kinds.entry(k.clone()).or_default() += 1;
    }
    let totals = v.get("window").and_then(|w| w.get("stored_totals"));
    let total = |k: &str| totals.and_then(|t| t.get(k)).and_then(Value::as_i64).unwrap_or(0);
    TopologyView {
        scopes: arr("scopes")
            .iter()
            .map(|s| ScopeRow {
                id: str_of(s.get("scope_id")),
                status: str_of(s.get("status")),
                observed_ms: s.get("observed_at").and_then(Value::as_i64),
            })
            .collect(),
        entities,
        kind_counts: kinds.into_iter().collect(),
        edges_shown: arr("edges").len(),
        stored_entities: total("accepted_entities"),
        stored_edges: total("accepted_edges"),
        history: arr("history").len(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckStatus {
    Fail,
    Warn,
    Unknown,
    Ok,
}

impl CheckStatus {
    fn parse(s: &str) -> Self {
        match s {
            "fail" => Self::Fail,
            "warn" => Self::Warn,
            "ok" => Self::Ok,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Fail => "異常",
            Self::Warn => "注意",
            Self::Unknown => "不明",
            Self::Ok => "正常",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub scope: String,
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
    pub checked_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StatusEvent {
    pub scope: String,
    pub name: String,
    pub from: CheckStatus,
    pub to: CheckStatus,
    pub detail: String,
    pub ts_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusView {
    /// 異常 → 注意 → 不明 → 正常の順
    pub checks: Vec<Check>,
    /// 新しい順
    pub events: Vec<StatusEvent>,
    pub last_probe_ms: Option<i64>,
    pub last_logs_ms: Option<i64>,
}

impl StatusView {
    pub fn count(&self, s: CheckStatus) -> usize {
        self.checks.iter().filter(|c| c.status == s).count()
    }
}

/// `Engine::status_snapshot`（保存済みの現在状態。問い合わせだけでは状態を計算し直さない）を表示用にする
pub fn status(v: &Value) -> StatusView {
    let arr = |k: &str| v.get(k).and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let mut checks: Vec<Check> = arr("checks")
        .iter()
        .map(|c| Check {
            scope: str_of(c.get("scope")),
            name: str_of(c.get("name")),
            status: CheckStatus::parse(&str_of(c.get("status"))),
            detail: one_line(&str_of(c.get("detail")), 160),
            checked_ms: c.get("checked_at").and_then(Value::as_i64),
        })
        .collect();
    checks.sort_by(|a, b| a.status.cmp(&b.status).then_with(|| a.scope.cmp(&b.scope)).then_with(|| a.name.cmp(&b.name)));
    let mut events: Vec<StatusEvent> = arr("events")
        .iter()
        .map(|e| StatusEvent {
            scope: str_of(e.get("scope")),
            name: str_of(e.get("name")),
            from: CheckStatus::parse(&str_of(e.get("from_status"))),
            to: CheckStatus::parse(&str_of(e.get("to_status"))),
            detail: one_line(&str_of(e.get("detail")), 140),
            ts_ms: e.get("ts").and_then(Value::as_i64).unwrap_or(0),
        })
        .collect();
    events.sort_by_key(|e| std::cmp::Reverse(e.ts_ms));
    StatusView {
        checks,
        events,
        last_probe_ms: v.get("lastProbeAt").and_then(Value::as_i64),
        last_logs_ms: v.get("lastLogsAt").and_then(Value::as_i64),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActionRow {
    pub id: String,
    /// 元に戻せる（元に戻す操作を持ち、まだ戻されていない）
    pub can_undo: bool,
    pub at_ms: i64,
    pub node: String,
    pub label: String,
    pub kind: String,
    pub ok: bool,
}

/// 実行記録（`Store::actions`）。新しい順
pub fn actions(rows: &[Value]) -> Vec<ActionRow> {
    let undone: std::collections::HashSet<String> = rows
        .iter()
        .filter(|r| r.get("ok").and_then(Value::as_bool) == Some(true))
        .filter_map(|r| r.get("undo_of").and_then(Value::as_str).map(str::to_string))
        .collect();
    let mut v: Vec<ActionRow> = rows
        .iter()
        .map(|r| ActionRow {
            id: str_of(r.get("id")),
            can_undo: r.get("undo").is_some_and(|u| !u.is_null()) && r.get("ok").and_then(Value::as_bool) == Some(true) && !undone.contains(&str_of(r.get("id"))),
            at_ms: r.get("at").and_then(Value::as_i64).unwrap_or(0),
            node: str_of(r.get("node_id")),
            label: one_line(&str_of(r.get("label")), 160),
            kind: str_of(r.get("type")),
            ok: r.get("ok").and_then(Value::as_bool) == Some(true),
        })
        .collect();
    v.sort_by_key(|a| std::cmp::Reverse(a.at_ms));
    v
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScheduleView {
    pub enabled: bool,
    pub probe_minutes: Option<i64>,
    pub logs_minutes: Option<i64>,
    pub inventory_hours: Option<i64>,
    pub ai_minutes: Option<i64>,
    pub last_probe_ms: Option<i64>,
    pub last_logs_ms: Option<i64>,
}

/// `status_snapshot` の schedule と前回の時刻
pub fn schedule(v: &Value) -> ScheduleView {
    let s = v.get("schedule");
    let num = |k: &str| s.and_then(|s| s.get(k)).and_then(Value::as_i64);
    ScheduleView {
        enabled: s.and_then(|s| s.get("enabled")).and_then(Value::as_bool).unwrap_or(false),
        probe_minutes: num("probe_minutes"),
        logs_minutes: num("logs_minutes"),
        inventory_hours: num("inventory_hours"),
        ai_minutes: num("ai_minutes"),
        last_probe_ms: v.get("lastProbeAt").and_then(Value::as_i64),
        last_logs_ms: v.get("lastLogsAt").and_then(Value::as_i64),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PowerRow {
    pub node: String,
    /// measured / estimated / missing など（Engine の summary.kind）
    pub kind: String,
    pub total_w: Option<f64>,
    pub wall_w: Option<f64>,
    pub cpu_w: Option<f64>,
    pub gpu_w: Option<f64>,
    pub missing: Vec<String>,
    pub projected_kwh: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PowerView {
    pub rows: Vec<PowerRow>,
    pub available: i64,
    pub expected: i64,
    pub fleet_total_w: Option<f64>,
}

/// `Engine::power_report`。値の無い機体は「未計測」として出し、0 とは扱わない
pub fn power(v: &Value) -> PowerView {
    let f = |s: &Value, k: &str| s.get(k).and_then(Value::as_f64);
    let rows = v
        .get("nodes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|n| {
                    let s = n.get("summary").cloned().unwrap_or(Value::Null);
                    PowerRow {
                        node: str_of(n.get("node_id")),
                        kind: str_of(s.get("kind")),
                        total_w: f(&s, "total_w"),
                        wall_w: f(&s, "wall_w"),
                        cpu_w: f(&s, "cpu_w"),
                        gpu_w: f(&s, "gpu_w"),
                        missing: s.get("missing").and_then(Value::as_array).map(|m| m.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
                        projected_kwh: f(&s, "projected_kwh"),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let fleet = v.get("fleet").cloned().unwrap_or(Value::Null);
    PowerView {
        rows,
        available: fleet.get("available").and_then(Value::as_i64).unwrap_or(0),
        expected: fleet.get("expected").and_then(Value::as_i64).unwrap_or(0),
        fleet_total_w: f(&fleet, "total_w"),
    }
}

pub fn power_kind_label(k: &str) -> &'static str {
    match k {
        "measured" => "実測",
        "estimated" => "推定",
        "missing" => "未計測",
        _ => "不明",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolGroup {
    pub name: String,
    pub node_count: i64,
    /// 機体どうしで版が違う
    pub drift: bool,
    pub versions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolEvent {
    pub node: String,
    pub name: String,
    pub kind: String,
    pub from: String,
    pub to: String,
    pub ts_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InventoryView {
    /// (機体, 件数, 最後の成功, 失敗理由)
    pub nodes: Vec<(String, i64, Option<i64>, Option<String>)>,
    /// 版差のあるものを先、次に台数の多い順
    pub groups: Vec<ToolGroup>,
    pub events: Vec<ToolEvent>,
    pub last_at: Option<i64>,
}

/// `Engine::inventory_view`（保存済み）。棚卸しの実行はしない
pub fn inventory(v: &Value) -> InventoryView {
    let arr = |k: &str| v.get(k).and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let mut groups: Vec<ToolGroup> = arr("groups")
        .iter()
        .map(|g| {
            let mut versions: Vec<String> = g
                .get("nodes")
                .and_then(Value::as_object)
                .map(|n| n.values().flat_map(|x| x.get("versions").and_then(Value::as_array).into_iter().flatten().filter_map(|s| s.as_str().map(str::to_string))).collect())
                .unwrap_or_default();
            versions.sort();
            versions.dedup();
            ToolGroup {
                name: str_of(g.get("name")),
                node_count: g.get("node_count").and_then(Value::as_i64).unwrap_or(0),
                drift: g.get("drift").and_then(Value::as_bool) == Some(true),
                versions,
            }
        })
        .collect();
    groups.sort_by(|a, b| b.drift.cmp(&a.drift).then(b.node_count.cmp(&a.node_count)).then_with(|| a.name.cmp(&b.name)));
    let mut events: Vec<ToolEvent> = arr("events")
        .iter()
        .map(|e| ToolEvent {
            node: str_of(e.get("node_id")),
            name: str_of(e.get("name")),
            kind: str_of(e.get("kind")),
            from: str_of(e.get("from_version")),
            to: str_of(e.get("to_version")),
            ts_ms: e.get("ts").and_then(Value::as_i64).unwrap_or(0),
        })
        .collect();
    events.sort_by_key(|e| std::cmp::Reverse(e.ts_ms));
    InventoryView {
        nodes: arr("nodes")
            .iter()
            .map(|n| (str_of(n.get("id")), n.get("count").and_then(Value::as_i64).unwrap_or(0), n.get("last_ok_at").and_then(Value::as_i64), n.get("error").and_then(Value::as_str).map(str::to_string)))
            .collect(),
        groups,
        events,
        last_at: v.get("lastInventoryAt").and_then(Value::as_i64),
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AiView {
    pub days: i64,
    pub sessions: i64,
    pub prompts: i64,
    /// 料金表から求めた概算（請求額ではない）
    pub cost_usd: f64,
    pub tok_in: i64,
    pub tok_out: i64,
    pub models: Vec<(String, f64)>,
    pub tools: Vec<(String, i64, i64)>,
    pub prs: usize,
    pub last_at: Option<i64>,
}

/// `Engine::ai_summary`（保存済み）。取り込みはしない
pub fn ai(v: &Value) -> AiView {
    let t = v.get("totals");
    let n = |k: &str| t.and_then(|t| t.get(k)).and_then(Value::as_i64).unwrap_or(0);
    let mut models: Vec<(String, f64)> = v
        .get("models")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|m| (str_of(m.get("model")), m.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0))).collect())
        .unwrap_or_default();
    models.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut tools: Vec<(String, i64, i64)> = v
        .get("tools")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|x| (str_of(x.get("name")), x.get("calls").and_then(Value::as_i64).unwrap_or(0), x.get("errors").and_then(Value::as_i64).unwrap_or(0))).collect())
        .unwrap_or_default();
    tools.sort_by_key(|x| std::cmp::Reverse(x.1));
    AiView {
        days: v.get("days").and_then(Value::as_i64).unwrap_or(0),
        sessions: n("sessions"),
        prompts: n("prompts"),
        cost_usd: t.and_then(|t| t.get("cost_usd")).and_then(Value::as_f64).unwrap_or(0.0),
        tok_in: n("tok_in"),
        tok_out: n("tok_out"),
        models,
        tools,
        prs: v.get("prs").and_then(Value::as_array).map_or(0, Vec::len),
        last_at: v.get("lastAiAt").and_then(Value::as_i64),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpecNode {
    pub node: String,
    pub eligible: bool,
    pub collector: String,
    pub at_ms: Option<i64>,
    /// 直近の取得が失敗したか（失敗しても前回の報告は残る）
    pub last_failed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpecSection {
    pub name: String,
    /// (機体, 部品名)。部品が無い機体は出さない
    pub rows: Vec<(String, Vec<String>)>,
    pub components: usize,
    /// 公式情報と照合できなかった部品の数
    pub unverified: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpecsView {
    pub nodes: Vec<SpecNode>,
    pub sections: Vec<SpecSection>,
}

/// `Engine::specs_view`（保存済みの報告）。収集は走らせない
pub fn specs(v: &Value) -> SpecsView {
    let nodes_json = v.get("nodes").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let mut sections: std::collections::BTreeMap<String, SpecSection> = Default::default();
    let mut nodes = Vec::new();
    for n in nodes_json {
        let id = str_of(n.get("node_id"));
        let report = n.get("report").filter(|r| r.is_object());
        nodes.push(SpecNode {
            node: id.clone(),
            eligible: n.get("eligible").and_then(Value::as_bool) == Some(true),
            collector: str_of(report.and_then(|r| r.get("collector_version"))),
            at_ms: report.and_then(|r| r.get("at")).and_then(Value::as_i64),
            last_failed: n.get("last_attempt").and_then(|a| a.get("ok")).and_then(Value::as_bool) == Some(false),
        });
        let Some(secs) = report.and_then(|r| r.get("sections")).and_then(Value::as_object) else { continue };
        for (name, sec) in secs {
            let comps = sec.get("components").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
            if comps.is_empty() {
                continue;
            }
            let e = sections.entry(name.clone()).or_insert_with(|| SpecSection { name: name.clone(), rows: Vec::new(), components: 0, unverified: 0 });
            e.components += comps.len();
            e.unverified += comps.iter().filter(|c| c.get("official").and_then(|o| o.get("status")).and_then(Value::as_str) != Some("verified")).count();
            e.rows.push((id.clone(), comps.iter().map(|c| one_line(&str_of(c.get("name")), 60)).collect()));
        }
    }
    SpecsView { nodes, sections: sections.into_values().collect() }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SecNode {
    pub node: String,
    pub enabled: bool,
    pub checks: usize,
    pub shared: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SecurityView {
    pub nodes: Vec<SecNode>,
    pub events: usize,
    pub peers: usize,
    pub logins: usize,
    pub keep_days: i64,
    pub learn_days: i64,
}

/// `Engine::netsec_view`（読み取りだけ。遮断や設定変更はしない）
pub fn security(v: &Value) -> SecurityView {
    let len = |k: &str| v.get(k).and_then(Value::as_array).map_or(0, Vec::len);
    SecurityView {
        nodes: v
            .get("nodes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|n| SecNode {
                        node: str_of(n.get("id")),
                        enabled: n.get("enabled").and_then(Value::as_bool) == Some(true),
                        checks: n.get("checks").and_then(Value::as_array).map_or(0, Vec::len),
                        shared: n.get("shared").and_then(Value::as_bool) == Some(true),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        events: len("events"),
        peers: len("peers"),
        logins: len("logins"),
        keep_days: v.get("keep_days").and_then(Value::as_i64).unwrap_or(0),
        learn_days: v.get("learn_days").and_then(Value::as_i64).unwrap_or(0),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NetRow {
    pub node: String,
    pub ok: bool,
    pub skipped: bool,
    pub error: String,
    pub at_ms: Option<i64>,
    /// (項目名, 値)。値が取れなかった項目は None（false とは区別する）
    pub flags: Vec<(&'static str, Option<bool>)>,
}

/// `Engine::network_check` の結果（受動確認。到達性の真偽だけで、宛先の IP は含まない）
pub fn network(v: &Value) -> Vec<NetRow> {
    const KEYS: [(&str, &str); 5] = [
        ("interface_up", "インターフェース"),
        ("link_active", "リンク"),
        ("ip_address_present", "IP"),
        ("default_route_present", "既定経路"),
        ("dns_configured", "DNS"),
    ];
    v.get("nodes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|n| NetRow {
                    node: str_of(n.get("node_id")),
                    ok: n.get("ok").and_then(Value::as_bool) == Some(true),
                    skipped: n.get("skipped").and_then(Value::as_bool) == Some(true),
                    error: one_line(&str_of(n.get("error")), 120),
                    at_ms: n.get("at").and_then(Value::as_i64),
                    flags: KEYS.iter().map(|(k, label)| (*label, n.get("data").and_then(|d| d.get(*k)).and_then(Value::as_bool))).collect(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `confirm_and_run` / `undo` の結果を一行にする。取り消し・拒否・失敗・成功を取り違えない
pub fn action_message(node: &str, r: Result<Value, String>) -> String {
    let who = if node.is_empty() { String::new() } else { format!("{node}: ") };
    match r {
        Err(e) => format!("{who}実行できませんでした: {e}"),
        Ok(v) if v.get("cancelled").and_then(Value::as_bool) == Some(true) => format!("{who}取り消しました（何も実行していません）"),
        Ok(v) if v.get("refused").is_some_and(|x| !x.is_null()) => format!("{who}実行しませんでした: {}", str_of(v.get("refused"))),
        Ok(v) if v.get("ok").and_then(Value::as_bool) == Some(true) => {
            let o = str_of(v.get("outcome"));
            format!("{who}{}", if o.is_empty() { "完了しました".to_string() } else { one_line(&o, 200) })
        }
        Ok(v) => format!("{who}失敗しました: {}", one_line(&str_of(v.get("output")), 200)),
    }
}

/// ライブ表示で貯める点（直近 3 分 = 1 秒ごとに 180 点）
pub const LIVE_KEEP: usize = 180;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveNode {
    /// connecting / live / stopped など（Engine の phase 名）
    pub state: String,
    pub detail: Option<String>,
    pub cpu: Vec<f64>,
    pub mem: Vec<f64>,
    last_t: i64,
}

/// `Live` が送るイベント `{ nodes: { id: { state, detail, points: [{t, cpu, mem}] } } }` を貯める。
/// 同じ時刻以前の点は捨て（開始時の全体と差分が重なる）、古い点は LIVE_KEEP を超えたら落とす。
#[derive(Clone, Debug, Default)]
pub struct LiveStore(pub std::collections::HashMap<String, LiveNode>);

impl LiveStore {
    pub fn merge(&mut self, ev: &Value) {
        let Some(nodes) = ev.get("nodes").and_then(Value::as_object) else { return };
        for (id, n) in nodes {
            let s = self.0.entry(id.clone()).or_default();
            if let Some(st) = n.get("state").and_then(Value::as_str) {
                s.state = st.to_string();
            }
            if n.get("detail").is_some() {
                s.detail = n.get("detail").and_then(Value::as_str).map(str::to_string);
            }
            for p in n.get("points").and_then(Value::as_array).into_iter().flatten() {
                let t = p.get("t").and_then(Value::as_i64).unwrap_or(0);
                if t <= s.last_t {
                    continue;
                }
                s.last_t = t;
                s.cpu.push(p.get("cpu").and_then(Value::as_f64).unwrap_or(0.0));
                s.mem.push(p.get("mem").and_then(Value::as_f64).unwrap_or(0.0));
            }
            for v in [&mut s.cpu, &mut s.mem] {
                if v.len() > LIVE_KEEP {
                    v.drain(..v.len() - LIVE_KEEP);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ledger() -> Vec<Value> {
        vec![json!({"id": "a", "os": "macOS"}), json!({"id": "b", "os": "Windows"}), json!({"id": "c"})]
    }

    #[test]
    fn combines_ledger_and_results_in_ledger_order() {
        let last = vec![
            json!({"node_id": "b", "ok": false, "error": "ssh timeout"}),
            json!({"node_id": "a", "ok": true, "score": 82, "at": 1000,
                   "data": {"cpu_busy": 12.5, "memory": {"available_pct": 40.0}, "host": {"uptime_h": 100.0}},
                   "findings": [{"severity": "warn", "title": "t", "detail": "d", "advice": "x"}, {"severity": "critical", "title": "c"}]}),
        ];
        let v = build(&ledger(), &last);
        assert_eq!(v.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(v[0].health, Health::Scored(82));
        assert_eq!((v[0].count(Severity::Warn), v[0].count(Severity::Critical)), (1, 1));
        assert_eq!(v[0].cpu_busy, Some(12.5));
        assert_eq!(v[0].mem_avail_pct, Some(40.0));
        assert_eq!(v[1].health, Health::Failed("ssh timeout".into()));
        assert_eq!(v[2].health, Health::Unobserved);
    }

    #[test]
    fn resources_come_from_snapshot_data() {
        let last = vec![json!({"node_id": "a", "ok": true, "score": 90, "data": {
            "host": {"cores": 8, "cpu": "X", "uptime_h": 1.0},
            "memory": {"swap_used_gb": 2.5},
            "disk": [{"mount": "/", "free_gb": 10, "total_gb": 100, "free_pct": 10.0}, {"mount": "/x"}]
        }})];
        let v = build(&[json!({"id": "a"})], &last);
        assert_eq!((v[0].cores, v[0].swap_gb, v[0].cpu_name.as_str()), (Some(8), Some(2.5), "X"));
        assert_eq!(v[0].disks.len(), 2);
        assert_eq!(v[0].disks[0].free_pct, 10.0);
        assert_eq!(v[0].disks[1].total_gb, 0.0);
    }

    #[test]
    fn score_is_clamped_and_missing_fields_are_empty() {
        let v = build(&[json!({"id": "a"})], &[json!({"node_id": "a", "ok": true, "score": 250})]);
        assert_eq!(v[0].health, Health::Scored(100));
        assert!(v[0].findings.is_empty() && v[0].cpu_busy.is_none());
    }

    #[test]
    fn urgency_puts_critical_first() {
        let mk = |crit: usize, score| NodeView {
            id: String::new(), os: String::new(), role: String::new(), health: Health::Scored(score), at_ms: None,
            cpu_busy: None, mem_avail_pct: None, uptime_h: None, score_trend: vec![], cpu_trend: vec![], mem_trend: vec![],
            swap_gb: None, cores: None, cpu_name: String::new(), disks: vec![], apps: vec![], agent_processes: None,
            findings: (0..crit).map(|_| Finding { severity: Severity::Critical, title: String::new(), detail: String::new(), advice: String::new(), action: None, action_label: String::new() }).collect(),
        };
        let mut v = vec![mk(0, 10), mk(2, 90), mk(1, 50)];
        v.sort_by_key(NodeView::urgency);
        assert_eq!(v.iter().map(|n| n.count(Severity::Critical)).collect::<Vec<_>>(), [2, 1, 0]);
    }

    #[test]
    fn series_skips_rows_without_the_value() {
        let rows = vec![json!({"score": 80}), json!({"score": null}), json!({"cpu": 3}), json!({"score": 60.5})];
        assert_eq!(series(&rows, "score"), [80.0, 60.5]);
    }

    #[test]
    fn live_store_dedups_overlap_and_caps() {
        let mut s = LiveStore::default();
        s.merge(&json!({"nodes": {"a": {"state": "live", "points": [{"t": 1, "cpu": 10, "mem": 50}, {"t": 2, "cpu": 20, "mem": 51}]}}}));
        // 開始時の全体（t=1,2 を含む）が後から届いても重ならない
        s.merge(&json!({"nodes": {"a": {"points": [{"t": 2, "cpu": 99}, {"t": 3, "cpu": 30, "mem": 52}]}}}));
        let a = &s.0["a"];
        assert_eq!(a.cpu, [10.0, 20.0, 30.0]);
        assert_eq!(a.state, "live");
        let pts: Vec<_> = (10..10 + 300).map(|t| json!({"t": t, "cpu": 1})).collect();
        s.merge(&json!({"nodes": {"a": {"points": pts}}}));
        assert_eq!(s.0["a"].cpu.len(), LIVE_KEEP);
        s.merge(&json!({"bad": 1}));
    }

    #[test]
    fn log_rows_and_signatures_are_one_line_and_tolerant() {
        let rows = log_rows(&[json!({"node_id": "a", "ts": 1700000000000.0, "level": "error", "message": "first\nsecond"}), json!({})]);
        assert_eq!(rows[0].message, "first");
        assert_eq!(rows[0].ts_ms, 1_700_000_000_000);
        assert_eq!(rows[1].level, "");
        let long = "あ".repeat(300);
        assert_eq!(log_rows(&[json!({"message": long})])[0].message.chars().count(), 201);
        let s = signatures(&[json!({"level": "warn", "sample": "x", "n": 5, "nodes": 2})]);
        assert_eq!((s[0].count, s[0].nodes), (5, 2));
        assert_eq!(level_label("warn"), "警告");
    }

    #[test]
    fn topology_view_counts_kinds_and_keeps_stored_totals() {
        let v = json!({
            "scopes": [{"scope_id": "s1", "status": "partial", "observed_at": 5}, {}],
            "entities": [{"kind": "host", "label": "a"}, {"kind": "package", "label": "b"}, {"kind": "package", "label": "c"}],
            "edges": [{}, {}],
            "history": [{}],
            "window": {"stored_totals": {"accepted_entities": 81, "accepted_edges": 99}}
        });
        let t = topology(&v);
        assert_eq!(t.kind_counts, [("host".to_string(), 1), ("package".to_string(), 2)]);
        assert_eq!((t.stored_entities, t.stored_edges, t.edges_shown, t.history), (81, 99, 2, 1));
        assert_eq!(t.scopes[0].observed_ms, Some(5));
        assert_eq!(scope_status_label(&t.scopes[0].status), "一部のみ");
        assert_eq!(scope_status_label(&t.scopes[1].status), "未取得");
        assert_eq!(topology(&json!({})), TopologyView::default());
    }

    #[test]
    fn status_sorts_worst_first_and_events_newest_first() {
        let v = json!({
            "checks": [
                {"scope": "b", "name": "n1", "status": "ok"},
                {"scope": "a", "name": "n2", "status": "fail", "detail": "x\ny"},
                {"scope": "a", "name": "n3", "status": "warn"},
                {"scope": "a", "name": "n4", "status": "???"}
            ],
            "events": [{"ts": 1, "from_status": "ok", "to_status": "fail"}, {"ts": 9, "from_status": "fail", "to_status": "ok"}],
            "lastProbeAt": 7
        });
        let s = status(&v);
        assert_eq!(s.checks.iter().map(|c| c.status).collect::<Vec<_>>(), [CheckStatus::Fail, CheckStatus::Warn, CheckStatus::Unknown, CheckStatus::Ok]);
        assert_eq!(s.checks[0].detail, "x");
        assert_eq!((s.count(CheckStatus::Fail), s.count(CheckStatus::Ok)), (1, 1));
        assert_eq!(s.events[0].ts_ms, 9);
        assert_eq!(s.last_probe_ms, Some(7));
        assert_eq!(status(&json!({})), StatusView::default());
    }

    #[test]
    fn findings_carry_actions_and_undo_state_is_derived() {
        let v = build(
            &[json!({"id": "a"})],
            &[json!({"node_id": "a", "ok": true, "findings": [
                {"severity": "warn", "title": "t", "action": {"type": "kill-process", "label": "止める", "params": {}}},
                {"severity": "warn", "title": "u"},
                {"severity": "warn", "title": "v", "action": {"label": "型なし"}}
            ]})],
        );
        let f = &v[0].findings;
        assert_eq!(f[0].action_label, "止める");
        assert!(f[0].action.is_some() && f[1].action.is_none() && f[2].action.is_none());
        let a = actions(&[
            json!({"id": "1", "at": 1, "ok": true, "undo": {"type": "x"}}),
            json!({"id": "2", "at": 2, "ok": true, "undo": {"type": "x"}}),
            json!({"id": "3", "at": 3, "ok": true, "undo_of": "1"}),
            json!({"id": "4", "at": 4, "ok": false, "undo": {"type": "x"}}),
        ]);
        let by = |id: &str| a.iter().find(|r| r.id == id).unwrap().can_undo;
        assert!(!by("1"), "戻し済み");
        assert!(by("2"));
        assert!(!by("3") && !by("4"), "戻しの記録・失敗した記録は戻せない");
    }

    #[test]
    fn action_messages_do_not_confuse_outcomes() {
        assert!(action_message("a", Ok(json!({"cancelled": true}))).contains("取り消しました"));
        assert!(action_message("a", Ok(json!({"refused": "共用機"}))).contains("実行しませんでした: 共用機"));
        assert!(action_message("a", Ok(json!({"ok": true, "outcome": "完了"}))).ends_with("完了"));
        assert!(action_message("a", Ok(json!({"ok": false, "output": "x"}))).contains("失敗しました"));
        assert!(action_message("", Err("e".into())).starts_with("実行できませんでした"));
        // cancelled が ok より優先される
        assert!(action_message("a", Ok(json!({"ok": true, "cancelled": true}))).contains("取り消しました"));
    }

    #[test]
    fn actions_newest_first_and_schedule_defaults() {
        let a = actions(&[json!({"at": 1, "ok": true, "label": "a"}), json!({"at": 5, "ok": false, "type": "t"}), json!({})]);
        assert_eq!(a.iter().map(|x| x.at_ms).collect::<Vec<_>>(), [5, 1, 0]);
        assert!(a[1].ok && !a[0].ok);
        let s = schedule(&json!({"schedule": {"enabled": true, "probe_minutes": 60, "inventory_hours": 24}, "lastProbeAt": 9}));
        assert!(s.enabled);
        assert_eq!((s.probe_minutes, s.logs_minutes, s.inventory_hours, s.last_probe_ms), (Some(60), None, Some(24), Some(9)));
        assert_eq!(schedule(&json!({})), ScheduleView::default());
    }

    #[test]
    fn power_keeps_missing_as_none_not_zero() {
        let v = json!({"fleet": {"available": 1, "expected": 8, "total_w": null}, "nodes": [
            {"node_id": "a", "summary": {"kind": "measured", "total_w": 120.5, "cpu_w": 30, "missing": []}},
            {"node_id": "b", "summary": {"kind": "missing", "missing": ["gpu_w", "cpu_w"], "total_w": null}},
            {"node_id": "c"}
        ]});
        let p = power(&v);
        assert_eq!((p.available, p.expected, p.fleet_total_w), (1, 8, None));
        assert_eq!(p.rows[0].total_w, Some(120.5));
        assert_eq!(p.rows[1].total_w, None);
        assert_eq!(p.rows[1].missing, ["gpu_w", "cpu_w"]);
        assert_eq!(power_kind_label(&p.rows[2].kind), "不明");
        assert_eq!(power(&json!({})), PowerView::default());
    }

    #[test]
    fn inventory_puts_drift_first_and_dedups_versions() {
        let v = json!({
            "groups": [
                {"name": "b", "node_count": 8, "drift": false, "nodes": {"x": {"versions": ["1"]}}},
                {"name": "a", "node_count": 2, "drift": true, "nodes": {"x": {"versions": ["2", "1"]}, "y": {"versions": ["1"]}}}
            ],
            "events": [{"ts": 1, "name": "p"}, {"ts": 5, "name": "q", "from_version": "1", "to_version": "2"}],
            "nodes": [{"id": "x", "count": 3, "last_ok_at": 9}],
            "lastInventoryAt": 7
        });
        let i = inventory(&v);
        assert_eq!(i.groups[0].name, "a");
        assert_eq!(i.groups[0].versions, ["1", "2"]);
        assert_eq!(i.events[0].name, "q");
        assert_eq!(i.nodes[0], ("x".to_string(), 3, Some(9), None));
        assert_eq!(i.last_at, Some(7));
        assert_eq!(inventory(&json!({})), InventoryView::default());
    }

    #[test]
    fn ai_summary_sorts_by_cost_and_calls() {
        let v = json!({"days": 30, "totals": {"sessions": 5, "prompts": 9, "cost_usd": 1.5, "tok_in": 10, "tok_out": 20},
            "models": [{"model": "small", "cost_usd": 1.0}, {"model": "big", "cost_usd": 9.0}],
            "tools": [{"name": "a", "calls": 1, "errors": 0}, {"name": "b", "calls": 7, "errors": 2}], "prs": [{}, {}]});
        let a = ai(&v);
        assert_eq!((a.sessions, a.prompts, a.prs), (5, 9, 2));
        assert_eq!(a.models[0].0, "big");
        assert_eq!(a.tools[0], ("b".to_string(), 7, 2));
        assert_eq!(ai(&json!({})), AiView::default());
    }

    #[test]
    fn specs_groups_by_section_and_counts_unverified() {
        let v = json!({"nodes": [
            {"node_id": "a", "eligible": true, "last_attempt": {"ok": false}, "report": {"collector_version": "1", "at": 5, "sections": {
                "gpu": {"components": [{"name": "G1", "official": {"status": "verified"}}, {"name": "G2", "official": {"status": "unverified"}}]},
                "audio": {"components": []}}}},
            {"node_id": "b", "eligible": false}
        ]});
        let s = specs(&v);
        assert_eq!(s.nodes.len(), 2);
        assert!(s.nodes[0].last_failed && !s.nodes[1].eligible);
        assert_eq!(s.sections.len(), 1);
        assert_eq!((s.sections[0].components, s.sections[0].unverified), (2, 1));
        assert_eq!(s.sections[0].rows[0].1, ["G1", "G2"]);
        assert_eq!(specs(&json!({})), SpecsView::default());
    }

    #[test]
    fn apps_and_security_parse_with_missing_fields() {
        let v = build(&[json!({"id": "a"})], &[json!({"node_id": "a", "ok": true, "data": {"processes": {"agent_processes": 3, "apps": [{"app": "x", "count": 2, "cpu": 1.5, "mem_mb": 100}, {}]}}})]);
        assert_eq!(v[0].agent_processes, Some(3));
        assert_eq!(v[0].apps.len(), 2);
        assert_eq!((v[0].apps[0].count, v[0].apps[0].mem_mb), (2, 100.0));
        let s = security(&json!({"nodes": [{"id": "a", "enabled": true, "checks": [{}, {}]}, {"id": "b"}], "events": [{}], "keep_days": 30}));
        assert_eq!((s.nodes[0].enabled, s.nodes[0].checks, s.nodes[1].enabled), (true, 2, false));
        assert_eq!((s.events, s.peers, s.keep_days), (1, 0, 30));
        assert_eq!(security(&json!({})), SecurityView::default());
    }

    #[test]
    fn network_keeps_unknown_flags_distinct_from_false() {
        let v = json!({"nodes": [
            {"node_id": "a", "ok": true, "at": 3, "data": {"dns_configured": true, "link_active": false}},
            {"node_id": "b", "ok": false, "skipped": true, "error": "x\ny"}
        ]});
        let r = network(&v);
        assert_eq!(r[0].flags[1], ("リンク", Some(false)));
        assert_eq!(r[0].flags[4], ("DNS", Some(true)));
        assert_eq!(r[0].flags[0], ("インターフェース", None));
        assert!(r[1].skipped && !r[1].ok && r[1].error == "x");
        assert!(network(&json!({})).is_empty());
    }

    #[test]
    fn relative_time() {
        assert_eq!(ago(100_000, 99_000), "たった今");
        assert_eq!(ago(1_000_000, 1_000_000 - 180_000), "3 分前");
        assert_eq!(ago(10_000_000, 10_000_000 - 7_200_000), "2 時間前");
        assert_eq!(uptime(100.0), "4 日");
        assert_eq!(uptime(5.0), "5 時間");
    }
}
