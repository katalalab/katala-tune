//! 機体台帳を読み、この機体をローカル実行に切り替える（lib/nodes.js と同じ）。
//! 台帳は個人の情報なのでリポジトリに置かず `~/.config/katala-tune/nodes.json` を使う（見本は config/nodes.example.json）。
//! 保存先（DB）は Electron 版の userData と同じ `<appData>/Katala Tune/data`。どちらのアプリからも同じ台帳・同じ DB を読む。

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::js;

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

    pub fn is_mac(&self) -> bool {
        self.os == "macos"
    }

    pub fn is_windows(&self) -> bool {
        self.os == "windows"
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
        let expect = self.raw.get("expect").filter(|v| js::truthy(Some(v))).cloned().unwrap_or(Value::Null);
        m.insert("expect".into(), expect);
        Value::Object(m)
    }
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
        assert_eq!(c.nodes.len(), 3);
        assert!(c.nodes[0].local, "local_hostname は大文字小文字を問わず一致する");
        assert!(!c.nodes[1].local);
        assert!(c.nodes[2].shared);
        assert_eq!(c.protect, vec!["python.exe", "Code", "Xcode"]);
        assert!(c.fleet.is_none());
        let p = c.nodes[2].public();
        assert_eq!(p["shared"], Value::Bool(true));
        assert_eq!(p["expect"], Value::Null);
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
