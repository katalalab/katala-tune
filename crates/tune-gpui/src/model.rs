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
            }
        })
        .collect()
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
            cpu_busy: None, mem_avail_pct: None, uptime_h: None,
            findings: (0..crit).map(|_| Finding { severity: Severity::Critical, title: String::new(), detail: String::new(), advice: String::new() }).collect(),
        };
        let mut v = vec![mk(0, 10), mk(2, 90), mk(1, 50)];
        v.sort_by_key(NodeView::urgency);
        assert_eq!(v.iter().map(|n| n.count(Severity::Critical)).collect::<Vec<_>>(), [2, 1, 0]);
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
