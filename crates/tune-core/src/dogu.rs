//! Do-gu（https://do-gu.niwa.dev、仕事道具のデッキを見せ合うサービス）との連携（lib/dogu.js と同じ）。
//! - 道具の一覧（共通マスター）は認証なしの GET /api/tools。画面のボタンを押したときだけ取りにいき、DB の meta に1日キャッシュする
//! - デッキへの登録は本人の API キーで POST /api/decks（差分マージ）。登録したデッキは公開ページになるので、
//!   必ず画面で全件を見せて承認を取ってから送る。新しい道具の作成（name・category・website_url は後から直せない）はしない。リトライしない
//!
//! HTTP は OS の curl を使う（依存を増やさない。macOS・Windows 10 以降に入っている）。API キーは引数に載せず、
//! curl の設定を標準入力で渡す（他のプロセスから見えない）。テストでは [`Http`] を偽物に差し替える。

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::collate::{locale_compare, utf16_cmp};
use crate::db::{Store, now_ms};
use crate::inventory::{Group, norm_name};
use crate::js;

pub const BASE: &str = "https://do-gu.niwa.dev";
pub const TOOLS_TTL_MS: i64 = 24 * 3_600_000;
pub const META_TOOLS: &str = "dogu_tools";
pub const META_EXCLUDE: &str = "dogu_exclude";

/// 名前の別表記。インストール先の名前と Do-gu の slug が素直に一致しないものだけ
const ALIASES: &[(&str, &str)] = &[
    ("code", "visual-studio-code"),
    ("microsoftvisualstudiocode", "visual-studio-code"),
    ("githubcli", "gh"),
    ("node", "node-js"),
    ("nodejs", "node-js"),
    ("dockerdesktop", "docker-desktop"),
    ("anthropicclaudecode", "claude-code"),
    ("openaicodex", "codex"),
];
/// 種類ごとの別表記（単体の CLI の claude は Claude Code。アプリの Claude はチャットのほう）
const SOURCE_ALIASES: &[(&str, &str, &str)] = &[("bin", "claude", "claude-code"), ("platform", "chocolatey", "chocolatey")];

fn slug_of(t: &Value) -> Option<String> {
    t.get("slug").filter(|s| js::truthy(Some(s))).map(|s| js::string(Some(s)))
}

/// マスターから「正規化した名前 → slug」の索引を作る（buildIndex）。同じ名前の道具が複数あるときは、公式サイトとアイコンがそろったほうを選ぶ
pub fn build_index(tools: &[Value]) -> HashMap<String, String> {
    let rank = |t: &Value| (if js::truthy(t.get("website_url")) { 2 } else { 0 }) + i32::from(js::truthy(t.get("icon_url")));
    let mut idx: HashMap<String, &Value> = HashMap::new();
    for t in tools {
        let Some(slug) = slug_of(t) else { continue };
        let name = t.get("name").filter(|v| js::truthy(Some(v))).map(|v| js::string(Some(v))).unwrap_or_default();
        for k in [slug.clone(), name] {
            let n = norm_name(&k);
            if n.is_empty() {
                continue;
            }
            let replace = match idx.get(&n) {
                None => true,
                Some(cur) => slug_of(cur).as_deref() != Some(n.as_str()) && rank(t) > rank(cur),
            };
            if replace {
                idx.insert(n, t);
            }
        }
    }
    idx.into_iter().filter_map(|(k, t)| slug_of(t).map(|s| (k, s))).collect()
}

/// 道具の名前から slug を探す（makeMatcher）
pub struct Matcher {
    idx: HashMap<String, String>,
    by_slug: HashSet<String>,
}

impl Matcher {
    pub fn new(tools: &[Value]) -> Matcher {
        Matcher { idx: build_index(tools), by_slug: tools.iter().filter_map(|t| t.get("slug").map(|s| js::string(Some(s)))).collect() }
    }

    /// item = { source, name }
    pub fn slug(&self, item: &Value) -> Option<String> {
        let name = js::string(item.get("name"));
        let source = js::string(item.get("source"));
        let n = norm_name(&name);
        let tail = name.contains('/').then(|| norm_name(name.rsplit('/').next().unwrap_or("")));
        for c in [Some(n), tail].into_iter().flatten().filter(|c| !c.is_empty()) {
            let alias =
                SOURCE_ALIASES.iter().find(|(s, k, _)| *s == source && *k == c).map(|x| x.2).or_else(|| ALIASES.iter().find(|(k, _)| *k == c).map(|x| x.1));
            if let Some(a) = alias
                && self.by_slug.contains(a)
            {
                return Some(a.to_string());
            }
            if let Some(s) = self.idx.get(&c) {
                return Some(s.clone());
            }
        }
        None
    }
}

/// デッキの下書き（deckDraft）: 自分で入れた道具のうち、Do-gu に既にあるもの。除外リストに入れたものは出さない
pub fn deck_draft(groups: &[Group], tools: &[Value], exclude: &[String]) -> Vec<Value> {
    let mut meta: HashMap<String, &Value> = HashMap::new();
    for t in tools {
        meta.insert(js::string(t.get("slug")), t);
    }
    let mut out: Vec<Value> = groups
        .iter()
        .filter_map(|g| {
            let slug = g.slug.as_ref().filter(|s| !s.is_empty())?;
            let t = meta.get(slug)?;
            if exclude.contains(slug) {
                return None;
            }
            // マスターに無い項目は入れない（JS の undefined と同じ）
            let mut m = Map::new();
            m.insert("slug".into(), Value::from(slug.as_str()));
            for k in ["name", "category"] {
                if let Some(v) = t.get(k) {
                    m.insert(k.into(), v.clone());
                }
            }
            m.insert("nodes".into(), json!(g.node_ids()));
            m.insert("sources".into(), json!(g.sources));
            Some(Value::Object(m))
        })
        .collect();
    out.sort_by(|a, b| {
        locale_compare(&js::string(a.get("category")), &js::string(b.get("category")))
            .then_with(|| locale_compare(&js::string(a.get("name")), &js::string(b.get("name"))))
    });
    out
}

/// 送る前の検証（planPublish）。下書き（除外を反映済み）にある slug だけを通す。
/// 確認ダイアログの前と後の2回呼び、結果が変わっていたら送らない
pub fn plan_publish(draft: &[Value], slugs: &Value) -> Value {
    let allowed: HashMap<String, &Value> = draft.iter().map(|d| (js::string(d.get("slug")), d)).collect();
    let mut pick: Vec<String> = Vec::new();
    for s in js::arr(Some(slugs)) {
        let s = js::string(Some(s));
        if !pick.contains(&s) && allowed.contains_key(&s) {
            pick.push(s);
        }
    }
    pick.sort_by(|a, b| utf16_cmp(a, b));
    if pick.is_empty() {
        return json!({ "refused": "送る道具がない" });
    }
    let items: Vec<Value> = pick.iter().map(|s| allowed[s].clone()).collect();
    json!({ "pick": pick, "items": items })
}

/// 2つの計画が同じ slug を送るか（samePlan）
pub fn same_plan(a: &Value, b: &Value) -> bool {
    match (a.get("pick").and_then(Value::as_array), b.get("pick").and_then(Value::as_array)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// POST /api/decks の本文（deckPayload）。slug だけを送る（既存の道具に紐づけるだけ。新規作成はしない）
pub fn deck_payload(slugs: &[String]) -> Value {
    let mut seen: Vec<&String> = Vec::new();
    for s in slugs {
        if !seen.contains(&s) {
            seen.push(s);
        }
    }
    json!({ "items": seen.iter().map(|slug| json!({ "tool": { "slug": slug } })).collect::<Vec<_>>() })
}

/// API キー（apiKey）: 環境変数 DO_GU_API_KEY → Do-gu の案内にある保存場所の順。見つけても移動・書き換えはしない。
/// macOS / Linux: ~/.local/share/do-gu/api_key → ~/.config/do-gu/api_key（do_gu・api_key.txt も見る）。Windows: %LOCALAPPDATA% → %APPDATA%
pub fn api_key(env: &dyn Fn(&str) -> Option<String>, home: &Path, platform: &str) -> Option<String> {
    if let Some(v) = env("DO_GU_API_KEY") {
        let t = js::trim(&v);
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    let dirs: Vec<PathBuf> = if platform == "win32" {
        ["LOCALAPPDATA", "APPDATA"].iter().filter_map(|k| env(k).filter(|v| !v.is_empty())).map(PathBuf::from).collect()
    } else {
        vec![home.join(".local").join("share"), home.join(".config")]
    };
    for d in dirs {
        for sub in ["do-gu", "do_gu"] {
            for f in ["api_key", "api_key.txt"] {
                if let Ok(b) = std::fs::read(d.join(sub).join(f)) {
                    let v = String::from_utf8_lossy(&b);
                    let t = js::trim(&v);
                    if !t.is_empty() {
                        return Some(t.to_string());
                    }
                }
            }
        }
    }
    None
}

/// この機体の API キー（実行時の環境変数とホーム）
pub fn api_key_here() -> Option<String> {
    let platform = if cfg!(windows) { "win32" } else { "darwin" };
    api_key(&|k| std::env::var(k).ok(), &crate::nodes::home(), platform)
}

// ---- HTTP ----

#[derive(Clone, Debug, PartialEq)]
pub struct HttpRequest {
    pub method: &'static str,
    pub url: String,
    /// Authorization: Bearer の値（ログ・引数には出さない）
    pub bearer: Option<String>,
    /// JSON の本文
    pub body: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub type HttpFuture<'a> = Pin<Box<dyn Future<Output = Result<HttpResponse, String>> + Send + 'a>>;

/// HTTP の口。本物は [`Curl`]、テストは偽物
pub trait Http: Send + Sync {
    fn send(&self, req: HttpRequest) -> HttpFuture<'_>;
}

/// OS の curl で送る。リトライしない・20 秒で打ち切る・https だけ
pub struct Curl;

/// curl の設定ファイルの文字列（"…" の中の \ と " と改行を逃がす）
fn curl_quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// 標準入力で渡す curl の設定（API キーを引数に載せないため）
pub fn curl_config(req: &HttpRequest) -> String {
    let mut lines = vec![
        format!("url = {}", curl_quote(&req.url)),
        format!("request = {}", curl_quote(req.method)),
        "silent".into(),
        "show-error".into(),
        "max-time = 20".into(),
        "proto = \"=https\"".into(),
        "header = \"Accept: application/json\"".into(),
        "write-out = \"\\n%{http_code}\"".into(),
    ];
    if let Some(k) = &req.bearer {
        lines.push(format!("header = {}", curl_quote(&format!("Authorization: Bearer {k}"))));
    }
    if let Some(b) = &req.body {
        lines.push("header = \"Content-Type: application/json\"".into());
        lines.push(format!("data-binary = {}", curl_quote(b)));
    }
    lines.join("\n") + "\n"
}

impl Http for Curl {
    fn send(&self, req: HttpRequest) -> HttpFuture<'_> {
        Box::pin(async move {
            let cfg = curl_config(&req);
            let r = crate::collect::run("curl", &["--config".into(), "-".into()], Some(cfg.as_bytes()), Duration::from_secs(30)).await;
            if r.code != Some(0) {
                let e = if r.err.trim().is_empty() { format!("curl exit {}", r.code_str()) } else { r.err.trim().to_string() };
                return Err(format!("Do-gu に接続できない: {e}"));
            }
            let (body, code) = r.out.rsplit_once('\n').unwrap_or(("", r.out.as_str()));
            let status = code.trim().parse::<u16>().map_err(|_| format!("Do-gu の応答を読めない: {}", js::slice16(code, 100)))?;
            Ok(HttpResponse { status, body: body.to_string() })
        })
    }
}

/// fetchJson と同じ: 2xx なら本文の JSON（読めなければ null）、それ以外は `Do-gu <status>: <本文>` のエラー
pub async fn fetch_json(http: &dyn Http, req: HttpRequest) -> Result<Value, String> {
    let res = http.send(req).await?;
    let body: Option<Value> = serde_json::from_str(&res.body).ok();
    if !(200..300).contains(&res.status) {
        let detail = match &body {
            Some(b) if js::truthy(Some(b)) => b.to_string(),
            _ => js::slice16(&res.body, 300),
        };
        return Err(format!("Do-gu {}: {detail}", res.status));
    }
    Ok(body.unwrap_or(Value::Null))
}

/// 共通マスター（tools）。DB の meta に1日キャッシュする。force でキャッシュを使わない
pub async fn tools(db: &std::sync::Mutex<Store>, http: &dyn Http, force: bool) -> Result<Value, String> {
    let lock = || db.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let cached = lock().get_meta(META_TOOLS).map_err(|e| e.to_string())?;
    if !force
        && let Some(c) = &cached
        && (now_ms() as f64 - js::num(c.get("at"))) < TOOLS_TTL_MS as f64
    {
        return Ok(c.clone());
    }
    let body = fetch_json(http, HttpRequest { method: "GET", url: format!("{BASE}/api/tools"), bearer: None, body: None }).await?;
    let list = if body.is_array() {
        js::arr(Some(&body)).to_vec()
    } else {
        body.get("tools").filter(|v| js::truthy(Some(v))).map(|v| js::arr(Some(v)).to_vec()).unwrap_or_default()
    };
    let tools: Vec<Value> = list.iter().map(pick_tool).collect();
    let v = json!({ "at": now_ms(), "tools": tools });
    lock().set_meta(META_TOOLS, &v).map_err(|e| e.to_string())?;
    Ok(v)
}

/// マスターの1件から使う項目だけを残す（slug・name・category・website_url・icon_url）
pub fn pick_tool(t: &Value) -> Value {
    let mut m = Map::new();
    for k in ["slug", "name", "category", "website_url"] {
        if let Some(v) = t.get(k) {
            m.insert(k.into(), v.clone());
        }
    }
    m.insert("icon_url".into(), t.get("icon_url").filter(|v| js::truthy(Some(v))).cloned().unwrap_or(Value::Null));
    Value::Object(m)
}

/// GET /api/me（ログイン名を確かめる。確認ダイアログに公開ページの URL を出すため）
pub async fn me(http: &dyn Http, key: &str) -> Result<Value, String> {
    fetch_json(http, HttpRequest { method: "GET", url: format!("{BASE}/api/me"), bearer: Some(key.to_string()), body: None }).await
}

/// POST /api/decks。送るのは承認済みの slug だけ。リトライしない（エラーは本文ごと画面に出す）
pub async fn publish(http: &dyn Http, key: &str, slugs: &[String]) -> Result<Value, String> {
    let body = deck_payload(slugs).to_string();
    fetch_json(http, HttpRequest { method: "POST", url: format!("{BASE}/api/decks"), bearer: Some(key.to_string()), body: Some(body) }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::matrix;

    #[test]
    fn matcher_and_draft() {
        let tools = vec![
            json!({ "slug": "visual-studio-code", "name": "Visual Studio Code", "category": "editor" }),
            json!({ "slug": "ripgrep", "name": "ripgrep", "category": "cli" }),
            json!({ "slug": "node-js", "name": "Node.js", "category": "language" }),
            json!({ "slug": "secret-tool", "name": "Secret Tool", "category": "other" }),
        ];
        let m = Matcher::new(&tools);
        assert_eq!(m.slug(&json!({ "name": "Visual Studio Code" })).as_deref(), Some("visual-studio-code"));
        assert_eq!(m.slug(&json!({ "name": "code" })).as_deref(), Some("visual-studio-code"));
        assert_eq!(m.slug(&json!({ "name": "node" })).as_deref(), Some("node-js"));
        assert_eq!(m.slug(&json!({ "name": "@scope/ripgrep" })).as_deref(), Some("ripgrep"));
        assert_eq!(m.slug(&json!({ "name": "unknown-thing" })), None);
        let rows = [
            json!({ "node_id": "a", "source": "app", "name": "Visual Studio Code", "version": "1", "explicit": true }),
            json!({ "node_id": "b", "source": "winreg", "name": "Microsoft Visual Studio Code (User)", "version": "1", "explicit": true }),
            json!({ "node_id": "a", "source": "brew", "name": "ripgrep", "version": "14", "explicit": true }),
            json!({ "node_id": "a", "source": "cask", "name": "secret-tool", "version": "1", "explicit": true }),
        ];
        let f = |r: &Value| m.slug(r);
        let g = matrix(&rows, Some(&f), true);
        assert_eq!(g.iter().find(|x| x.slug.as_deref() == Some("visual-studio-code")).map(|x| x.nodes.len()), Some(2));
        let d = deck_draft(&g, &tools, &["secret-tool".into()]);
        assert_eq!(d.iter().map(|x| x["slug"].as_str().unwrap()).collect::<Vec<_>>(), ["ripgrep", "visual-studio-code"]);
        assert_eq!(
            deck_payload(&["ripgrep".into(), "ripgrep".into(), "gh".into()]),
            json!({ "items": [{ "tool": { "slug": "ripgrep" } }, { "tool": { "slug": "gh" } }] })
        );
    }

    #[test]
    fn plan_only_draft_slugs_and_compare() {
        let draft = vec![json!({ "slug": "jq", "name": "jq", "category": "cli" }), json!({ "slug": "gh", "name": "gh", "category": "cli" })];
        let a = plan_publish(&draft, &json!(["jq", "gh", "not-in-draft", "jq"]));
        assert_eq!(a["pick"], json!(["gh", "jq"]));
        assert_eq!(plan_publish(&draft, &json!(["nope"]))["refused"], "送る道具がない");
        assert_eq!(plan_publish(&draft, &json!("jq"))["refused"], "送る道具がない");
        assert!(same_plan(&a, &plan_publish(&draft, &json!(["gh", "jq"]))));
        assert!(!same_plan(&a, &plan_publish(&draft[1..], &json!(["jq", "gh"]))));
    }

    #[test]
    fn api_key_order() {
        let home = std::env::temp_dir().join(format!("kt-dogu-key-{}-{}", std::process::id(), now_ms()));
        let none = |_: &str| None;
        assert_eq!(api_key(&none, &home, "darwin"), None);
        std::fs::create_dir_all(home.join(".config/do-gu")).unwrap();
        std::fs::write(home.join(".config/do-gu/api_key"), "k-config\n").unwrap();
        assert_eq!(api_key(&none, &home, "darwin").as_deref(), Some("k-config"));
        std::fs::create_dir_all(home.join(".local/share/do-gu")).unwrap();
        std::fs::write(home.join(".local/share/do-gu/api_key"), "k-share").unwrap();
        assert_eq!(api_key(&none, &home, "darwin").as_deref(), Some("k-share"));
        let env = |k: &str| (k == "DO_GU_API_KEY").then(|| " k-env ".to_string());
        assert_eq!(api_key(&env, &home, "darwin").as_deref(), Some("k-env"));
        // 見つけたファイルは動かさない・書き換えない
        assert_eq!(std::fs::read_to_string(home.join(".config/do-gu/api_key")).unwrap(), "k-config\n");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn curl_config_keeps_key_out_of_args() {
        let c = curl_config(&HttpRequest {
            method: "POST",
            url: format!("{BASE}/api/decks"),
            bearer: Some("k\"1".into()),
            body: Some("{\"a\":\"b\\\\c\"}".into()),
        });
        assert!(c.contains("header = \"Authorization: Bearer k\\\"1\""));
        assert!(c.contains("data-binary = \"{\\\"a\\\":\\\"b\\\\\\\\c\\\"}\""));
        assert!(c.contains("proto = \"=https\""));
    }
}
