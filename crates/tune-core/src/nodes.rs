//! 機体台帳を読み、この機体をローカル実行に切り替える（lib/nodes.js と同じ）。
//! 台帳は個人の情報なのでリポジトリに置かず `~/.config/katala-tune/nodes.json` を使う（見本は config/nodes.example.json）。
//! 保存先（DB）は Electron 版の userData と同じ `<appData>/Katala Tune/data`。どちらのアプリからも同じ台帳・同じ DB を読む。

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::js;
use crate::topology::config::TopologyConfig;

pub const EXAMPLE: &str = include_str!("../../../config/nodes.example.json");
/// Electron の productName。userData（`app.getPath('userData')`）はこの名前のフォルダ
pub const PRODUCT_NAME: &str = "Katala Tune";

/// 台帳の場所。`KATALA_TUNE_CONFIG` で差し替えられる（検証用）
pub fn user_config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("KATALA_TUNE_CONFIG").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    home().join(".config").join("katala-tune").join("nodes.json")
}

/// DB の置き場所（Electron の `<userData>/data`）。`KATALA_TUNE_DATA_DIR` で差し替えられる（検証用）
/// macOS: ~/Library/Application Support/Katala Tune/data、Windows: %APPDATA%\Katala Tune\data、Linux: ~/.config/Katala Tune/data
pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("KATALA_TUNE_DATA_DIR").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    app_data().join(PRODUCT_NAME).join("data")
}

/// Electron の `app.getPath('appData')` と同じ場所
pub fn app_data() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| home().join(".config"))
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// `~` を展開する（`~/x` と `~` だけ）
pub fn expand(p: &str) -> String {
    if p == "~" {
        return home().to_string_lossy().into_owned();
    }
    for sep in ["~/", "~\\"] {
        if let Some(rest) = p.strip_prefix(sep) {
            return format!("{}{}{}", home().to_string_lossy(), &sep[1..], rest);
        }
    }
    p.to_string()
}

/// この機体の hostname（`.local` を除き小文字）
pub fn local_host() -> String {
    let h = gethostname::gethostname().to_string_lossy().into_owned();
    h.strip_suffix(".local").unwrap_or(&h).to_lowercase()
}

/// 台帳の1台
#[derive(Clone, Debug)]
pub struct Node {
    pub id: String,
    pub alias: String,
    pub os: String,
    pub shared: bool,
    pub local: bool,
    /// 台帳に書かれたそのまま（`local` を足したもの）
    pub raw: Value,
}

impl Node {
    pub fn from_value(v: &Value, host: &str) -> Node {
        let mut raw = match v {
            Value::Object(m) => m.clone(),
            _ => Map::new(),
        };
        let lh = raw.get("local_hostname");
        let local = js::truthy(lh) && js::string(lh).to_lowercase() == host;
        raw.insert("local".into(), Value::Bool(local));
        let raw = Value::Object(raw);
        let s = |k: &str| js::string(raw.get(k));
        Node { id: s("id"), alias: s("alias"), os: s("os"), shared: js::truthy(raw.get("shared")), local, raw }
    }

    pub fn get(&self, k: &str) -> Option<&Value> {
        self.raw.get(k)
    }

    /// Topology collector の私有 environment 設定。無ければ空で、collector は root を推測しない。
    pub fn topology(&self) -> &Value {
        self.raw.get("topology").unwrap_or(&Value::Null)
    }

    pub fn is_mac(&self) -> bool {
        self.os == "macos"
    }

    pub fn is_windows(&self) -> bool {
        self.os == "windows"
    }

    /// 運用状態（台帳の `status`。lib/ledger.js の opState と同じ）
    pub fn op_state(&self) -> OpState {
        op_state(&self.raw)
    }

    /// 運用対象（online）か。そうでない機体には SSH も probe も実行しない
    pub fn is_active(&self) -> bool {
        self.op_state().active
    }

    /// 運用対象でないとき、実行を断る理由（呼び出し側は接続も変更もしない）
    pub fn refusal(&self) -> Option<String> {
        let st = self.op_state();
        (!st.active).then(|| format!("{} は運用状態が {}（{}）のため、接続も変更もしない", self.id, st.state, state_label(&st.state)))
    }

    /// 画面に渡す形（main.js の publicNode）
    pub fn public(&self) -> Value {
        let mut m = Map::new();
        for k in ["id", "alias", "os", "role", "note"] {
            if let Some(v) = self.raw.get(k) {
                m.insert(k.into(), v.clone());
            }
        }
        m.insert("shared".into(), Value::Bool(self.shared));
        m.insert("local".into(), Value::Bool(self.local));
        let op = self.op_state();
        m.insert("status".into(), Value::String(op.state));
        m.insert("status_reason".into(), if op.reason.is_empty() { Value::Null } else { Value::String(op.reason) });
        m.insert("status_date".into(), if op.date.is_empty() { Value::Null } else { Value::String(op.date) });
        let expect = self.raw.get("expect").filter(|v| js::truthy(Some(v))).cloned().unwrap_or(Value::Null);
        m.insert("expect".into(), expect);
        Value::Object(m)
    }
}

/// 台帳に書ける運用状態。省略は online（この項目を持たない既存の台帳もそのまま動く）
pub const STATES: [&str; 5] = ["online", "retired", "no-ssh", "paused", "out-of-scope"];
/// 値が読めないときの状態。安全側に倒して対象外にする（退役機に誤って接続しない）
pub const INVALID: &str = "invalid";

pub fn state_label(state: &str) -> &'static str {
    match state {
        "online" => "運用中",
        "retired" => "退役",
        "no-ssh" => "SSH 対象外",
        "paused" => "一時停止",
        "out-of-scope" => "対象外",
        _ => "状態不明",
    }
}

/// 台帳の1台の運用状態（到達できるかとは別の情報。調査結果から自動で変えない）
#[derive(Clone, Debug, PartialEq)]
pub struct OpState {
    pub state: String,
    pub active: bool,
    pub valid: bool,
    pub reason: String,
    pub date: String,
}

fn trimmed(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    }
}

/// lib/ledger.js の opState と同じ
pub fn op_state(raw: &Value) -> OpState {
    let reason = trimmed(raw.get("status_reason"));
    let date = trimmed(raw.get("status_date"));
    let status = raw.get("status").filter(|v| !v.is_null());
    let Some(status) = status else { return OpState { state: "online".into(), active: true, valid: true, reason, date } };
    let s = trimmed(Some(status)).to_lowercase().replace('_', "-");
    if STATES.contains(&s.as_str()) {
        return OpState { active: s == "online", state: s, valid: true, reason, date };
    }
    let shown = match status {
        Value::String(x) => x.chars().take(40).collect::<String>(),
        Value::Bool(_) => "boolean".into(),
        Value::Number(_) => "number".into(),
        Value::Array(_) | Value::Object(_) => "object".into(),
        Value::Null => "object".into(),
    };
    OpState {
        state: INVALID.into(), active: false, valid: false, reason: format!("status の値が読めない（{shown}）ため対象外にしている。台帳を直す"), date
    }
}

/// 収集・自動スキャン・変更操作の対象（online だけ）と、除外（理由つき）に分ける
pub fn partition(nodes: &[Node]) -> (Vec<Node>, Vec<Value>) {
    let mut targets = Vec::new();
    let mut excluded = Vec::new();
    for n in nodes {
        let st = n.op_state();
        if st.active {
            targets.push(n.clone());
        } else {
            excluded.push(json!({ "node_id": n.id, "state": st.state, "reason": st.reason, "date": st.date }));
        }
    }
    (targets, excluded)
}

/// 機体ごとの最後の観測（対象台帳に使う）
pub struct Observation {
    pub last_ok_at: Option<i64>,
    pub last_error: Option<String>,
}

/// 対象台帳（lib/ledger.js の targetLedger と同じ）。
/// target: online で最後の分析が成功。unobserved: online なのに観測できていない（異常として扱う）。excluded: 運用対象外（履歴は残る）
pub fn target_ledger(nodes: &[Node], obs: impl Fn(&str) -> Observation) -> Value {
    let mut rows = Vec::new();
    for n in nodes {
        let st = n.op_state();
        let mut row = json!({ "node_id": n.id, "os": if n.raw.get("os").is_some_and(|v| !v.is_null()) { json!(n.os) } else { Value::Null }, "state": st.state, "state_label": state_label(&st.state) });
        let o = &mut row;
        if !st.active {
            o["status"] = json!("excluded");
            o["reason"] = json!(if st.reason.is_empty() { "理由の記載なし".to_string() } else { st.reason });
            o["date"] = if st.date.is_empty() { Value::Null } else { json!(st.date) };
        } else {
            let ob = obs(&n.id);
            o["last_ok_at"] = json!(ob.last_ok_at);
            match (ob.last_error.filter(|e| !e.is_empty()), ob.last_ok_at) {
                (Some(e), _) => {
                    o["status"] = json!("unobserved");
                    o["reason"] = json!(format!("最後の分析が失敗: {}", e.trim().rsplit('\n').next().unwrap_or("").chars().take(120).collect::<String>()));
                }
                (None, None) => {
                    o["status"] = json!("unobserved");
                    o["reason"] = json!("まだ分析していない");
                }
                (None, Some(_)) => {
                    o["status"] = json!("target");
                    o["reason"] = Value::Null;
                }
            }
        }
        rows.push(row);
    }
    let count = |s: &str| rows.iter().filter(|r| r["status"] == s).count();
    let counts = json!({ "target": count("target"), "unobserved": count("unobserved"), "excluded": count("excluded"), "total": rows.len() });
    json!({ "rows": rows, "counts": counts })
}

/// katala-fleet 連携（任意）
#[derive(Clone, Debug)]
pub struct Fleet {
    pub repo: String,
    pub env_file: String,
}

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub nodes: Vec<Node>,
    pub protect: Vec<String>,
    pub fleet: Option<Fleet>,
    pub schedule: Option<Value>,
    pub topology: TopologyConfig,
    pub file: Option<PathBuf>,
}

impl Config {
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// 台帳が見本のまま（main.js の isExample）
    pub fn is_example(&self) -> bool {
        self.nodes.iter().any(|n| n.id == "my-mac") && self.nodes.iter().any(|n| n.id == "family-pc")
    }
}

/// 読めなければ Err を返す（保護リストを読めないまま操作しないため、呼び出し側は失敗を握りつぶさない）
pub fn load_config(file: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    parse_config(&text, file, &local_host())
}

pub fn parse_config(text: &str, file: &Path, host: &str) -> Result<Config, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("{}: JSON を読めない（{e}）", file.display()))?;
    let nodes = match v.get("nodes") {
        Some(Value::Array(a)) => a,
        _ => return Err(format!("{}: nodes が配列ではない", file.display())),
    };
    let protect = match v.get("protect") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.iter().map(|x| js::string(Some(x))).collect(),
        Some(_) => return Err(format!("{}: protect が配列ではない", file.display())),
    };
    let fleet = v
        .get("fleet")
        .filter(|f| js::truthy(js::get(Some(f), "repo")))
        .map(|f| Fleet { repo: expand(&js::string(f.get("repo"))), env_file: js::string(f.get("env_file")) });
    Ok(Config {
        nodes: nodes.iter().map(|n| Node::from_value(n, host)).collect(),
        protect,
        fleet,
        schedule: v.get("schedule").cloned(),
        topology: TopologyConfig::from_value(v.get("topology")).map_err(|e| format!("{}: {e}", file.display()))?,
        file: Some(file.to_path_buf()),
    })
}

/// 初回起動: 台帳が無ければ見本を置く。置いたら true
pub fn ensure_config(file: &Path) -> std::io::Result<bool> {
    if file.exists() {
        return Ok(false);
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, EXAMPLE)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_ledger_parses_and_is_example() {
        let c = parse_config(EXAMPLE, Path::new("nodes.json"), "my-macbook-pro").unwrap();
        assert!(c.is_example());
        assert_eq!(c.nodes.len(), 4);
        assert!(c.nodes[0].local, "local_hostname は大文字小文字を問わず一致する");
        assert!(!c.nodes[1].local);
        assert!(c.nodes[2].shared);
        assert_eq!(c.protect, vec!["python.exe", "Code", "Xcode"]);
        assert!(c.fleet.is_none());
        let p = c.nodes[2].public();
        assert_eq!(p["shared"], Value::Bool(true));
        assert_eq!(p["expect"], Value::Null);
        assert_eq!(p["status"], json!("online"));
        assert!(c.nodes[2].is_active());
        // 見本の退役機は対象外（接続しない）だが、台帳には残る
        assert_eq!(c.nodes[3].op_state().state, "retired");
        assert!(!c.nodes[3].is_active());
        assert_eq!(c.nodes[3].public()["status_date"], json!("2026-01-15"));
    }

    #[test]
    fn broken_ledger_is_an_error() {
        assert!(parse_config("{", Path::new("x"), "h").is_err());
        assert!(parse_config(r#"{"nodes": {}}"#, Path::new("x"), "h").unwrap_err().contains("nodes が配列ではない"));
        assert!(parse_config(r#"{"nodes": [], "protect": "x"}"#, Path::new("x"), "h").unwrap_err().contains("protect が配列ではない"));
    }

    #[test]
    fn paths_match_electron() {
        // Electron: app.getPath('userData') = <appData>/<productName>
        let d = data_dir();
        if std::env::var_os("KATALA_TUNE_DATA_DIR").is_none() {
            assert!(d.ends_with(Path::new("Katala Tune").join("data")));
            #[cfg(target_os = "macos")]
            assert!(d.starts_with(home().join("Library").join("Application Support")));
        }
        if std::env::var_os("KATALA_TUNE_CONFIG").is_none() {
            assert!(user_config_path().ends_with(Path::new(".config").join("katala-tune").join("nodes.json")));
        }
    }
}
