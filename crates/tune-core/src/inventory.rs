//! 道具の棚卸し（lib/inventory.js と同じ）: 各機体に入っているソフトウェア（パッケージ・アプリ・開発ツール）を集め、
//! 機体をまたいだ一覧（機体×道具）にする。読み取り専用。調査は probes/mac_inventory.py・probes/win_inventory.ps1
//! （インストール先を直接読むだけ。パッケージマネージャもネットワークも使わない）。

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::collate::locale_compare;
use crate::collect::{self, RunResult, last_json_line, ssh_args};
use crate::db::now_ms;
use crate::js::{self, WS};
use crate::nodes::Node;

pub const MAC_INVENTORY: &str = include_str!("../../../probes/mac_inventory.py");
/// PS 5.1 のため BOM 付き ASCII。バイトのまま渡す
pub const WIN_INVENTORY: &[u8] = include_bytes!("../../../probes/win_inventory.ps1");

/// 種類ごとの表示名。種類は「どこから入ったか」で、同じ道具が複数の種類に出ることがある（brew と app など）
pub const SOURCES: &[(&str, &str)] = &[
    ("brew", "Homebrew"),
    ("cask", "Homebrew Cask"),
    ("app", "アプリ"),
    ("mise", "mise"),
    ("uv", "uv tool"),
    ("cargo", "cargo"),
    ("npm", "npm -g"),
    ("winreg", "インストール済み"),
    ("scoop", "Scoop"),
    ("choco", "Chocolatey"),
    ("bin", "単体の CLI"),
    ("platform", "基盤"),
];

pub fn sources_json() -> Value {
    Value::Object(SOURCES.iter().map(|(k, v)| (k.to_string(), Value::from(*v))).collect())
}

/// 正規化した1件（normalizeItems の出力）
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub source: String,
    pub name: String,
    pub version: Option<String>,
    pub explicit: bool,
    pub extra: Option<Value>,
}

impl Item {
    pub fn to_json(&self) -> Value {
        json!({ "source": self.source, "name": self.name, "version": self.version, "explicit": self.explicit, "extra": self.extra })
    }
}

/// 種類と名前で一意にし、空の名前を捨てる。版は文字列に揃える（normalizeItems）
pub fn normalize_items(items: &[Value]) -> Vec<Item> {
    let mut seen: Vec<Item> = Vec::new();
    let mut keys: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for it in items {
        let field = |k: &str| match it.get(k) {
            None | Some(Value::Null) => String::new(),
            v => js::string(v),
        };
        let name = js::trim(&field("name")).to_string();
        let source = js::trim(&field("source")).to_string();
        if name.is_empty() || source.is_empty() {
            continue;
        }
        if !keys.insert((source.clone(), name.clone())) {
            continue;
        }
        let Some(obj) = it.as_object() else { continue };
        let version = match obj.get("version") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.is_empty() => None,
            v => Some(js::string(v)),
        };
        let explicit = obj.get("explicit") != Some(&Value::Bool(false));
        let extra: Map<String, Value> =
            obj.iter().filter(|(k, _)| !matches!(k.as_str(), "source" | "name" | "version" | "explicit")).map(|(k, v)| (k.clone(), v.clone())).collect();
        seen.push(Item { source, name, version, explicit, extra: if extra.is_empty() { None } else { Some(Value::Object(extra)) } });
    }
    seen
}

static RE_ARCH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"[{WS}]*\((?:x64|x86|arm64|64-bit|32-bit|user|machine|system)\)[{WS}]*")).expect("arch"));
static RE_VER: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"[{WS}]+v?[0-9]+(?:\.[0-9]+)+[^{WS}]*$")).expect("ver"));
static RE_SYM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}]+").expect("sym"));

/// 機体をまたいで同じ道具をまとめるための名前の正規化（normName）
pub fn norm_name(s: &str) -> String {
    let mut x = s.to_lowercase();
    if x.ends_with(".app") {
        x.truncate(x.len() - 4);
    }
    let x = RE_ARCH.replace_all(&x, " ");
    let x = RE_VER.replace(&x, "");
    RE_SYM.replace_all(&x, "").into_owned()
}

/// Object.keys の順（配列の添字になる名前は数の小さい順に先、残りは入れた順）
pub fn js_key_order(keys: &[String]) -> Vec<String> {
    let index = |k: &str| -> Option<u32> {
        let n: u32 = k.parse().ok()?;
        (n.to_string() == k && n < u32::MAX).then_some(n)
    };
    let mut ints: Vec<(u32, String)> = keys.iter().filter_map(|k| index(k).map(|n| (n, k.clone()))).collect();
    ints.sort_by_key(|(n, _)| *n);
    ints.into_iter().map(|(_, k)| k).chain(keys.iter().filter(|k| index(k).is_none()).cloned()).collect()
}

/// 道具の行から Do-gu の slug を探す関数（makeMatcher の戻り値）
pub type SlugFn<'a> = dyn Fn(&Value) -> Option<String> + 'a;

/// 機体×道具の1行（matrix の出力）
#[derive(Clone, Debug)]
pub struct Group {
    pub key: String,
    pub name: Value,
    pub slug: Option<String>,
    pub sources: Vec<String>,
    /// 機体（入れた順）→ (版, 種類)
    pub nodes: Vec<(String, Vec<Value>, Vec<String>)>,
    pub drift: bool,
}

impl Group {
    /// Object.keys(g.nodes) と同じ順の機体
    pub fn node_ids(&self) -> Vec<String> {
        js_key_order(&self.nodes.iter().map(|(n, _, _)| n.clone()).collect::<Vec<_>>())
    }

    pub fn to_json(&self) -> Value {
        let nodes: Map<String, Value> = self.nodes.iter().map(|(n, v, s)| (n.clone(), json!({ "versions": v, "sources": s }))).collect();
        json!({ "key": self.key, "name": self.name, "slug": self.slug, "sources": self.sources, "nodes": nodes, "node_count": self.nodes.len(), "drift": self.drift })
    }
}

/// 版の先頭の数字の並び（lib/inventory.js の core と同じ: `^\d+(?:\.\d+)*`、無ければそのまま）。
/// 2.51.0.windows.1 → 2.51.0、Cask の 1.2,34 → 1.2
pub fn version_core(v: &str) -> String {
    let b = v.as_bytes();
    let mut end = 0;
    while end < b.len() && b[end].is_ascii_digit() {
        end += 1;
    }
    if end == 0 {
        return v.to_string();
    }
    loop {
        let mut j = end;
        if j < b.len() && b[j] == b'.' {
            j += 1;
            let start = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > start {
                end = j;
                continue;
            }
        }
        break;
    }
    v[..end].to_string()
}

/// 機体×道具の表（matrix）。drift は「共通の版（先頭の数字の並び）を1つも持たない機体の組がある」とき（同じ機体の中の書き方の違い・OS ごとの接尾辞は数えない）。
/// match_slug: Do-gu の slug に寄せられればそれで、無ければ名前を正規化してまとめる
pub fn matrix(rows: &[Value], match_slug: Option<&SlugFn<'_>>, explicit_only: bool) -> Vec<Group> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Group> = HashMap::new();
    for r in rows {
        if js::truthy(r.get("removed_at")) || (explicit_only && !js::truthy(r.get("explicit"))) {
            continue;
        }
        let slug = match_slug.and_then(|f| f(r)).filter(|s| !s.is_empty());
        let key = match &slug {
            Some(s) => format!("dogu:{s}"),
            None => format!("name:{}", norm_name(&js::string(r.get("name")))),
        };
        let g = groups.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            Group {
                key: key.clone(),
                name: r.get("name").cloned().unwrap_or(Value::Null),
                slug: slug.clone(),
                sources: Vec::new(),
                nodes: Vec::new(),
                drift: false,
            }
        });
        let source = js::string(r.get("source"));
        if !g.sources.contains(&source) {
            g.sources.push(source.clone());
        }
        let node_id = js::string(r.get("node_id"));
        let pos = match g.nodes.iter().position(|(n, _, _)| *n == node_id) {
            Some(p) => p,
            None => {
                g.nodes.push((node_id, Vec::new(), Vec::new()));
                g.nodes.len() - 1
            }
        };
        let cell = &mut g.nodes[pos];
        if let Some(v) = r.get("version").filter(|v| js::truthy(Some(v)))
            && !cell.1.iter().any(|x| js::strict_eq(Some(x), Some(v)))
        {
            cell.1.push(v.clone());
        }
        if !cell.2.contains(&source) {
            cell.2.push(source);
        }
    }
    let mut out: Vec<Group> = order
        .into_iter()
        .filter_map(|k| groups.remove(&k))
        .map(|mut g| {
            let sets: Vec<Vec<String>> =
                g.nodes.iter().map(|(_, v, _)| v.iter().map(|x| version_core(&js::string(Some(x)))).collect::<Vec<_>>()).filter(|v| !v.is_empty()).collect();
            g.drift = sets.iter().enumerate().any(|(i, a)| sets[i + 1..].iter().any(|b| !a.iter().any(|v| b.contains(v))));
            g
        })
        .collect();
    out.sort_by(|a, b| b.nodes.len().cmp(&a.nodes.len()).then_with(|| locale_compare(&js::string(Some(&a.name)), &js::string(Some(&b.name)))));
    out
}

/// 1台の棚卸し（inventoryNode）。戻り値は `{ node_id, ok, items, errors, failedSources, wall_s, at }` か `{ node_id, ok: false, error, wall_s, at }`
pub async fn inventory_node(node: &Node) -> Value {
    let started = Instant::now();
    let res: RunResult = if node.is_mac() {
        let script = MAC_INVENTORY.as_bytes();
        if node.local {
            collect::run("/usr/bin/env", &["python3".into(), "-".into()], Some(script), Duration::from_secs(60)).await
        } else {
            collect::run(
                "ssh",
                &ssh_args(&node.alias, "command -v python3 >/dev/null && exec python3 - || exec /usr/bin/python3 -"),
                Some(script),
                Duration::from_secs(60),
            )
            .await
        }
    } else if node.local {
        collect::local::powershell_file(WIN_INVENTORY, "", Duration::from_secs(90)).await
    } else {
        let remote = "mkdir -p ~/.katala-tune && cat > ~/.katala-tune/inventory.ps1 && powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"$(cygpath -w ~/.katala-tune/inventory.ps1)\"";
        collect::run("ssh", &ssh_args(&node.alias, remote), Some(WIN_INVENTORY), Duration::from_secs(90)).await
    };
    parse_result(&node.id, &res, started.elapsed().as_secs_f64())
}

/// 調査の出力を結果の形にする（inventoryNode の後半）
pub fn parse_result(node_id: &str, res: &RunResult, wall_s: f64) -> Value {
    let data = last_json_line(&res.out).filter(|d| d.get("items").is_some_and(Value::is_array));
    let Some(data) = data else {
        let e = if !res.err.is_empty() {
            res.err.clone()
        } else if !res.out.is_empty() {
            res.out.clone()
        } else {
            format!("exit {}", res.code_str())
        };
        return json!({ "node_id": node_id, "ok": false, "error": js::slice16_tail(js::trim(&e), 800), "wall_s": wall_s, "at": now_ms() });
    };
    let items: Vec<Value> = normalize_items(js::arr(data.get("items"))).iter().map(Item::to_json).collect();
    let or_empty = |k: &str| data.get(k).filter(|v| js::truthy(Some(v))).cloned().unwrap_or_else(|| json!([]));
    json!({ "node_id": node_id, "ok": true, "items": items, "errors": or_empty("errors"), "failedSources": or_empty("failed_sources"), "wall_s": wall_s, "at": now_ms() })
}

/// 結果の items を保存用の形に戻す
pub fn items_of(result: &Value) -> Vec<Item> {
    js::arr(result.get("items"))
        .iter()
        .map(|v| Item {
            source: js::string(v.get("source")),
            name: js::string(v.get("name")),
            version: v.get("version").and_then(Value::as_str).map(str::to_string),
            explicit: v.get("explicit") != Some(&Value::Bool(false)),
            extra: v.get("extra").filter(|x| !x.is_null()).cloned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_core_takes_leading_numbers() {
        assert_eq!(version_core("2.51.0.windows.1"), "2.51.0");
        assert_eq!(version_core("1.2,34"), "1.2");
        assert_eq!(version_core("24.21.0"), "24.21.0");
        assert_eq!(version_core("3."), "3");
        assert_eq!(version_core("v1.2"), "v1.2");
        assert_eq!(version_core(""), "");
    }

    #[test]
    fn normalize_dedupes_and_drops_blank() {
        let items = normalize_items(&[
            json!({ "source": "brew", "name": "ripgrep", "version": "14.1.0", "explicit": true }),
            json!({ "source": "brew", "name": "ripgrep", "version": "14.1.0" }),
            json!({ "source": "app", "name": "Example App", "version": 3, "id": "com.example.app" }),
            json!({ "source": "brew", "name": "  " }),
            json!({ "source": "npm", "name": "tool", "version": "" }),
            Value::Null,
        ]);
        assert_eq!(items.len(), 3);
        assert_eq!(
            items[1].to_json(),
            json!({ "source": "app", "name": "Example App", "version": "3", "explicit": true, "extra": { "id": "com.example.app" } })
        );
        assert_eq!(items[2].version, None);
    }

    #[test]
    fn norm_name_like_js() {
        assert_eq!(norm_name("Visual Studio Code.app"), "visualstudiocode");
        assert_eq!(norm_name("Example Tool (x64)"), "exampletool");
        assert_eq!(norm_name("Example Tool 2.4.1"), "exampletool");
        assert_eq!(norm_name("秀丸エディタ"), "秀丸エディタ");
        assert_eq!(norm_name("tool 1.2 beta 3.4"), "tool12beta");
    }

    #[test]
    fn matrix_drift_between_nodes_only() {
        let row = |n: &str, s: &str, v: &str| json!({ "node_id": n, "source": s, "name": "Tool", "version": v, "explicit": true });
        assert!(!matrix(&[row("a", "cask", "1.2,34"), row("a", "app", "1.2")], None, true)[0].drift);
        assert!(!matrix(&[row("a", "cask", "1.2,34"), row("a", "app", "1.2"), row("b", "app", "1.2")], None, true)[0].drift);
        assert!(matrix(&[row("a", "app", "1.2"), row("b", "app", "1.3")], None, true)[0].drift);
        let rows = [
            json!({ "node_id": "a", "source": "brew", "name": "jq", "version": "1.7", "explicit": true }),
            json!({ "node_id": "a", "source": "brew", "name": "libfoo", "version": "1", "explicit": false }),
            json!({ "node_id": "b", "source": "cask", "name": "gone", "version": "1", "explicit": true, "removed_at": 5 }),
        ];
        assert_eq!(matrix(&rows, None, true).len(), 1);
        assert_eq!(matrix(&rows, None, false).len(), 2);
    }

    #[test]
    fn key_order_like_object_keys() {
        let k = |xs: &[&str]| js_key_order(&xs.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(k(&["b", "10", "a", "2", "01"]), ["2", "10", "b", "a", "01"]);
    }

    #[test]
    fn failed_probe_is_an_error() {
        let r = parse_result("n", &RunResult { code: Some(1), out: String::new(), err: "boom\n".into() }, 1.0);
        assert_eq!((r["ok"].clone(), r["error"].clone()), (json!(false), json!("boom")));
        let ok = parse_result(
            "n",
            &RunResult { code: Some(0), out: "{\"items\":[{\"source\":\"brew\",\"name\":\"a\"}],\"failed_sources\":[\"app\"]}".into(), err: String::new() },
            1.0,
        );
        assert_eq!(ok["failedSources"], json!(["app"]));
        assert_eq!(items_of(&ok)[0].name, "a");
    }
}
