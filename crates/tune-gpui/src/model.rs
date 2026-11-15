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
pub struct NodeView {
    pub id: String,
    pub os: String,
    pub role: String,
    pub health: Health,
    pub at_ms: Option<i64>,
    pub cpu_busy: Option<f64>,
    pub mem_avail_pct: Option<f64>,
    pub uptime_h: Option<f64>,
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
            findings: (0..crit).map(|_| Finding { severity: Severity::Critical, title: String::new(), detail: String::new(), advice: String::new() }).collect(),
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
    fn relative_time() {
        assert_eq!(ago(100_000, 99_000), "たった今");
        assert_eq!(ago(1_000_000, 1_000_000 - 180_000), "3 分前");
        assert_eq!(ago(10_000_000, 10_000_000 - 7_200_000), "2 時間前");
        assert_eq!(uptime(100.0), "4 日");
        assert_eq!(uptime(5.0), "5 時間");
    }
}
