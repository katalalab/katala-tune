//! 道具の棚卸し・Do-gu（lib/inventory.js・lib/dogu.js・lib/db.js の saveInventory など）を仕様として、
//! tune-core が同じ入力に同じ出力を返すかを確かめる。入力は決まった種から作る架空の道具・機体・マスター（実在の機体名などは使わない）。
//! node（24 以上）が要る。無い環境では失敗する（KATALA_TUNE_PARITY=skip で飛ばせる）。
//! 並び（localeCompare）は OS の言語で変わるので、JS 側は en-US に固定して比べる（tests/js/eval_inventory.js）。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value, json};
use tune_core::collate::locale_compare;
use tune_core::db::Store;
use tune_core::dogu::{self, Matcher};
use tune_core::inventory::{matrix, norm_name, normalize_items};

// ---- 決まった種の乱数 ----
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn chance(&mut self, p: f64) -> bool {
        (self.next() % 10_000) as f64 / 10_000.0 < p
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
}

/// 道具の名前の見本（照合の別表記・アーキテクチャ・版・記号・かな・漢字・全角・アクセントを含む）
const NAMES: &[&str] = &[
    "Visual Studio Code",
    "Visual Studio Code.app",
    "code",
    "Microsoft Visual Studio Code (User)",
    "Microsoft Visual Studio Code (x64)",
    "node",
    "Node.js",
    "nodejs",
    "GitHub CLI",
    "gh",
    "Docker Desktop",
    "docker-desktop",
    "@scope/ripgrep",
    "ripgrep",
    "jq",
    "jq 1.7.1",
    "Example Tool 2.4.1",
    "Example Tool (x64)",
    "Example Tool (ARM64)",
    "example-tool",
    "Example Tool v3.0-beta",
    "tool 1.2 beta 3.4",
    "秀丸エディタ",
    "Google 日本語入力",
    "ｱﾌﾟﾘ",
    "Ａｐｐ",
    "Café",
    "cafe",
    "Zoom",
    "zoom.us",
    "1Password",
    "1Password 8",
    "Python 3.12.4 (64-bit)",
    "python3.12",
    "claude",
    "Claude",
    "Claude Code",
    "anthropic-claude-code",
    "codex",
    "OpenAI Codex",
    "Chocolatey",
    "chocolatey",
    "über-tool",
    "naïve",
    "Ærø",
    "Œuvre",
    "straße",
    "a",
    "B",
    "b",
    "_private",
    "-dash",
    "日本",
    "にほん",
    "ニホン",
    "かな",
    "がな",
    "カナ",
    "Ωmega",
    "émoji 😀",
    "x　y",
    "ab-2",
    "ab10",
    "ab2",
    "Ab2",
    "mac.app.app",
    "tool (system) 2.0",
    "",
    "  ",
];
const SOURCES: &[&str] = &["brew", "cask", "app", "bin", "platform", "winreg", "scoop", "npm", "mise", "uv", "cargo", "choco"];
const VERSIONS: &[&str] = &["1.0", "1.0.1", "2", "", "1.2,34", "1.2", "v1", "24.21.0", "2026.10.1"];
const NODES: &[&str] = &["n1", "n2", "mac-a", "win-b", "10", "2"];
const CATS: &[&str] = &["editor", "cli", "language", "agent-harness", "ai-chat", "Editor", "エディタ", "infra", "_misc"];

fn name(r: &mut Rng) -> String {
    if r.chance(0.8) {
        return r.pick(NAMES).to_string();
    }
    // 文字を混ぜた名前
    const ALPHA: &[&str] =
        &["a", "B", "z", "Ä", "é", "0", "9", " ", "-", "_", ".", "(", ")", "/", "@", "v", "1", "2", "あ", "ア", "が", "漢", "字", "😀", "　", "\t"];
    (0..r.below(10) + 1).map(|_| r.pick(ALPHA).to_string()).collect()
}

fn raw_item(r: &mut Rng) -> Value {
    let mut m = Map::new();
    if !r.chance(0.03) {
        m.insert("source".into(), json!(r.pick(SOURCES)));
    }
    if !r.chance(0.03) {
        m.insert("name".into(), json!(name(r)));
    }
    match r.below(8) {
        0 => {}
        1 => {
            m.insert("version".into(), Value::Null);
        }
        2 => {
            m.insert("version".into(), json!(r.below(30)));
        }
        3 => {
            m.insert("version".into(), json!(1.5));
        }
        _ => {
            m.insert("version".into(), json!(r.pick(VERSIONS)));
        }
    }
    match r.below(4) {
        0 => {}
        1 => {
            m.insert("explicit".into(), json!(false));
        }
        2 => {
            m.insert("explicit".into(), json!(true));
        }
        _ => {
            m.insert("explicit".into(), json!(0));
        }
    }
    if r.chance(0.3) {
        m.insert("id".into(), json!(format!("com.example.{}", r.below(50))));
    }
    if r.chance(0.1) {
        m.insert("arch".into(), json!("arm64"));
    }
    Value::Object(m)
}

fn raw_items(r: &mut Rng, n: u64) -> Vec<Value> {
    let mut v: Vec<Value> = (0..n).map(|_| raw_item(r)).collect();
    if r.chance(0.2) {
        v.push(Value::Null);
    }
    if r.chance(0.2) {
        v.push(json!("not an object"));
    }
    v
}

/// DB の行と同じ形（matrix の入力）
fn row(r: &mut Rng) -> Value {
    let version = if r.chance(0.15) { Value::Null } else { json!(r.pick(VERSIONS)) };
    let removed = match r.below(10) {
        0 => json!(5),
        1 => json!(0),
        _ => Value::Null,
    };
    json!({ "node_id": r.pick(NODES), "source": r.pick(SOURCES), "name": name(r).trim(), "version": version, "explicit": !r.chance(0.25), "removed_at": removed })
}

fn tools(r: &mut Rng) -> Vec<Value> {
    let base: &[(&str, &str)] = &[
        ("visual-studio-code", "Visual Studio Code"),
        ("vscode", "Visual Studio Code"),
        ("node-js", "Node.js"),
        ("gh", "GitHub CLI"),
        ("docker-desktop", "Docker Desktop"),
        ("claude-code", "Claude Code"),
        ("claude", "Claude"),
        ("codex", "Codex"),
        ("ripgrep", "ripgrep"),
        ("jq", "jq"),
        ("chocolatey", "Chocolatey"),
        ("zoom", "Zoom"),
        ("1password", "1Password"),
        ("hidemaru", "秀丸エディタ"),
        ("example-tool", "Example Tool"),
        ("cafe", "Café"),
    ];
    let keep: Vec<(&str, &str)> = base.iter().copied().filter(|_| !r.chance(0.2)).collect();
    keep.into_iter()
        .map(|(slug, n)| {
            let mut m = Map::new();
            m.insert("slug".into(), json!(slug));
            m.insert("name".into(), json!(n));
            m.insert("category".into(), json!(r.pick(CATS)));
            if r.chance(0.5) {
                m.insert("website_url".into(), json!(format!("https://{slug}.example")));
            }
            if r.chance(0.5) {
                m.insert("icon_url".into(), json!(format!("/icons/{slug}.png")));
            }
            Value::Object(m)
        })
        .collect()
}

// ---- JS を呼ぶ ----
fn run_js(cases: &Value) -> Option<Value> {
    if std::env::var("KATALA_TUNE_PARITY").as_deref() == Ok("skip") {
        eprintln!("KATALA_TUNE_PARITY=skip のため JS との比較を飛ばした");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("kt-parity-inv-{}-{}", std::process::id(), tune_core::db::now_ms()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("cases.json");
    std::fs::write(&f, cases.to_string()).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("js").join("eval_inventory.js");
    let out = Command::new("node").arg("--no-warnings").arg(&script).arg(&f).output().expect("node が要る（KATALA_TUNE_PARITY=skip で飛ばせる）");
    assert!(out.status.success(), "eval_inventory.js が失敗: {}", String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::remove_dir_all(&dir);
    Some(serde_json::from_slice(&out.stdout).expect("eval_inventory.js の出力が JSON ではない"))
}

// ---- 比べる（数は値で比べる。オブジェクトのキーの順は問わない） ----
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w))),
        _ => a == b,
    }
}

/// JS が例外になった入力は比べない（数を返す）。それ以外は全部一致すること
fn check_all(kind: &str, inputs: &[Value], js: &Value, rust: &[Value]) -> usize {
    let js = js.as_array().unwrap_or_else(|| panic!("{kind}: JS の結果が無い"));
    assert_eq!(js.len(), rust.len(), "{kind}: 件数が違う");
    let mut thrown = 0;
    let mut bad = Vec::new();
    for (i, (j, r)) in js.iter().zip(rust).enumerate() {
        if j.get("__throw").is_some() {
            thrown += 1;
            continue;
        }
        if !same(j, r) {
            bad.push(i);
        }
    }
    if let Some(&i) = bad.first() {
        panic!(
            "{kind}: {} / {} 件が JS と違う。最初の例 #{i}\n入力: {}\nJS:   {}\nRust: {}",
            bad.len(),
            js.len(),
            inputs.get(i).map(Value::to_string).unwrap_or_default(),
            js[i],
            rust[i]
        );
    }
    thrown
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().map(|x| x.as_str().unwrap_or_default().to_string()).collect()).unwrap_or_default()
}

#[test]
fn inventory_and_dogu_match_js() {
    let mut r = Rng(0x2545_F491_4F6C_DD1D);

    // normalizeItems
    let normalize_in: Vec<Value> = (0..400).map(|_| json!(raw_items(&mut r, 12))).collect();
    let normalize_rs: Vec<Value> =
        normalize_in.iter().map(|c| json!(normalize_items(c.as_array().unwrap()).iter().map(|i| i.to_json()).collect::<Vec<_>>())).collect();

    // normName
    let mut names_in: Vec<Value> = NAMES.iter().map(|s| json!(s)).collect();
    names_in.extend((0..1500).map(|_| json!(name(&mut r))));
    let names_rs: Vec<Value> = names_in.iter().map(|s| json!(norm_name(s.as_str().unwrap()))).collect();

    // localeCompare の並び
    let sort_in: Vec<Value> = (0..300).map(|_| json!((0..r.below(12) + 2).map(|_| name(&mut r)).collect::<Vec<_>>())).collect();
    let sort_rs: Vec<Value> = sort_in
        .iter()
        .map(|xs| {
            let mut v = strs(xs);
            v.sort_by(|a, b| locale_compare(a, b));
            json!(v)
        })
        .collect();

    // matrix（照合なし／あり、自分で入れたものだけ／すべて）と下書き
    let matrix_in: Vec<Value> = (0..400)
        .map(|i| {
            let rows: Vec<Value> = (0..r.below(40) + 1).map(|_| row(&mut r)).collect();
            let t = if i % 3 == 0 { Value::Null } else { json!(tools(&mut r)) };
            let exclude: Vec<&str> = ["jq", "gh", "zoom", "vscode"].iter().copied().filter(|_| r.chance(0.3)).collect();
            json!({ "rows": rows, "tools": t, "explicitOnly": i % 2 == 0, "exclude": exclude })
        })
        .collect();
    let matrix_rs: Vec<Value> = matrix_in
        .iter()
        .map(|c| {
            let rows = c["rows"].as_array().unwrap();
            let tl = c["tools"].as_array().cloned();
            let m = tl.as_ref().map(|t| Matcher::new(t));
            let f = |x: &Value| m.as_ref().and_then(|m| m.slug(x));
            let slug_fn: Option<&tune_core::inventory::SlugFn<'_>> = if m.is_some() { Some(&f) } else { None };
            let g = matrix(rows, slug_fn, c["explicitOnly"].as_bool().unwrap());
            let draft = tl.as_ref().map(|t| json!(dogu::deck_draft(&g, t, &strs(&c["exclude"])))).unwrap_or(Value::Null);
            json!({ "groups": g.iter().map(|x| x.to_json()).collect::<Vec<_>>(), "draft": draft })
        })
        .collect();

    // buildIndex と makeMatcher
    let index_in: Vec<Value> = (0..200).map(|_| json!(tools(&mut r))).collect();
    let index_rs: Vec<Value> = index_in
        .iter()
        .map(|t| {
            let mut v: Vec<(String, String)> = dogu::build_index(t.as_array().unwrap()).into_iter().collect();
            v.sort_by(|a, b| tune_core::collate::utf16_cmp(&a.0, &b.0));
            json!(v.into_iter().map(|(k, s)| json!([k, s])).collect::<Vec<_>>())
        })
        .collect();
    let match_in: Vec<Value> = (0..200)
        .map(|_| {
            let items: Vec<Value> = (0..30).map(|_| json!({ "source": r.pick(SOURCES), "name": name(&mut r) })).collect();
            json!({ "tools": tools(&mut r), "items": items })
        })
        .collect();
    let match_rs: Vec<Value> = match_in
        .iter()
        .map(|c| {
            let m = Matcher::new(c["tools"].as_array().unwrap());
            json!(c["items"].as_array().unwrap().iter().map(|it| m.slug(it)).collect::<Vec<_>>())
        })
        .collect();

    // planPublish・samePlan・deckPayload
    let plan_in: Vec<Value> = (0..300)
        .map(|_| {
            let pool = ["jq", "gh", "ripgrep", "zoom", "codex", "Zoom", "ゼロ", "10", "2"];
            let chosen: Vec<&str> = pool.iter().copied().filter(|_| r.chance(0.6)).collect();
            let draft: Vec<Value> = chosen.iter().map(|s| json!({ "slug": s, "name": s.to_uppercase(), "category": r.pick(CATS) })).collect();
            let draft2: Vec<Value> = draft.iter().filter(|_| !r.chance(0.15)).cloned().collect();
            let slugs = match r.below(6) {
                0 => json!("jq"),
                1 => Value::Null,
                2 => json!([1, null, "jq", "jq"]),
                _ => json!(pool.iter().filter(|_| r.chance(0.5)).collect::<Vec<_>>()),
            };
            json!({ "draft": draft, "draft2": draft2, "slugs": slugs })
        })
        .collect();
    let plan_rs: Vec<Value> = plan_in
        .iter()
        .map(|c| {
            let a = dogu::plan_publish(c["draft"].as_array().unwrap(), &c["slugs"]);
            let b = dogu::plan_publish(c["draft2"].as_array().unwrap(), &c["slugs"]);
            let payload = a.get("pick").map(|p| {
                let mut v = strs(p);
                v.extend(strs(p));
                dogu::deck_payload(&v)
            });
            json!({ "same": dogu::same_plan(&a, &b), "a": a, "b": b, "payload": payload })
        })
        .collect();

    // apiKey（架空のホームと環境変数）
    let base = std::env::temp_dir().join(format!("kt-parity-key-{}-{}", std::process::id(), tune_core::db::now_ms()));
    let mut key_in: Vec<Value> = Vec::new();
    for i in 0..60 {
        let home = base.join(format!("h{i}"));
        let local = base.join(format!("l{i}"));
        let roaming = base.join(format!("r{i}"));
        let dirs: [PathBuf; 4] = [home.join(".local").join("share"), home.join(".config"), local.clone(), roaming.clone()];
        for d in &dirs {
            for sub in ["do-gu", "do_gu"] {
                for f in ["api_key", "api_key.txt"] {
                    if r.chance(0.2) {
                        let p = d.join(sub).join(f);
                        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                        let content = match r.below(4) {
                            0 => "   \n".to_string(),
                            1 => format!("\u{FEFF}key-{i}-{sub}-{f}\n"),
                            _ => format!("key-{i}-{}-{sub}-{f}", d.file_name().unwrap().to_string_lossy()),
                        };
                        std::fs::write(p, content).unwrap();
                    }
                }
            }
        }
        let mut env = Map::new();
        match r.below(5) {
            0 => {
                env.insert("DO_GU_API_KEY".into(), json!(" env-key "));
            }
            1 => {
                env.insert("DO_GU_API_KEY".into(), json!("   "));
            }
            _ => {}
        }
        if r.chance(0.7) {
            env.insert("LOCALAPPDATA".into(), json!(local));
        }
        if r.chance(0.7) {
            env.insert("APPDATA".into(), json!(roaming));
        }
        key_in.push(json!({ "env": env, "home": home, "platform": if i % 2 == 0 { "darwin" } else { "win32" } }));
    }
    let key_rs: Vec<Value> = key_in
        .iter()
        .map(|c| {
            let env = c["env"].as_object().unwrap().clone();
            let get = move |k: &str| env.get(k).and_then(Value::as_str).map(str::to_string);
            json!(dogu::api_key(&get, Path::new(c["home"].as_str().unwrap()), c["platform"].as_str().unwrap()))
        })
        .collect();

    let Some(js) = run_js(&json!({
        "normalize": normalize_in, "normName": names_in, "sort": sort_in, "matrix": matrix_in, "index": index_in, "match": match_in, "plan": plan_in, "apiKey": key_in,
    })) else {
        return;
    };
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(check_all("normalizeItems", &normalize_in, &js["normalize"], &normalize_rs), 0);
    assert_eq!(check_all("normName", &names_in, &js["normName"], &names_rs), 0);
    assert_eq!(check_all("localeCompare", &sort_in, &js["sort"], &sort_rs), 0);
    let thrown = check_all("matrix・deckDraft", &matrix_in, &js["matrix"], &matrix_rs);
    assert_eq!(thrown, 0, "matrix で JS が例外になった");
    assert_eq!(check_all("buildIndex", &index_in, &js["index"], &index_rs), 0);
    assert_eq!(check_all("makeMatcher", &match_in, &js["match"], &match_rs), 0);
    assert_eq!(check_all("planPublish・samePlan・deckPayload", &plan_in, &js["plan"], &plan_rs), 0);
    assert_eq!(check_all("apiKey", &key_in, &js["apiKey"], &key_rs), 0);
    // 照合が本当に効いている（別表記・重複の選び方・種類ごとの別表記が出る）こと
    let matched: usize = matrix_rs.iter().map(|m| m["groups"].as_array().unwrap().iter().filter(|g| g["slug"].is_string()).count()).sum();
    assert!(matched > 200, "照合された道具が少なすぎる: {matched}");
}

// ---- DB の相互運用（saveInventory・inventory・inventoryEvents） ----
fn ops(r: &mut Rng) -> Vec<Value> {
    let mut now = 1_800_000_000_000i64;
    let mut pool: Vec<Value> = (0..40).map(|_| raw_item(r)).collect();
    (0..24)
        .map(|_| {
            now += 3_600_000 + r.below(1000) as i64;
            // 少しずつ変える（版の更新・削除・再追加・取り方の失敗）
            for it in pool.iter_mut() {
                if r.chance(0.1) {
                    it["version"] = json!(r.pick(VERSIONS));
                }
            }
            let items: Vec<Value> = pool.iter().filter(|_| !r.chance(0.15)).cloned().collect();
            let skip: Vec<&str> = SOURCES.iter().copied().filter(|_| r.chance(0.1)).collect();
            if r.chance(0.2) {
                pool.push(raw_item(r));
            }
            json!({ "node_id": r.pick(&["n1", "n2", "10"]), "items": items, "now": now, "skip": skip })
        })
        .collect()
}

fn rust_dump(db: &Store, nodes: &[&str]) -> Value {
    let mut o = Map::new();
    o.insert("all".into(), json!(db.inventory(None, false).unwrap()));
    o.insert("removed".into(), json!(db.inventory(None, true).unwrap()));
    o.insert("events".into(), json!(db.inventory_events(1000).unwrap()));
    o.insert("nodes".into(), json!(db.inventory_nodes().unwrap()));
    for id in nodes {
        o.insert(format!("node:{id}"), json!(db.inventory(Some(id), false).unwrap()));
    }
    Value::Object(o)
}

fn rust_write(db: &Store, ops: &[Value]) -> Vec<Value> {
    ops.iter()
        .map(|op| {
            let items = normalize_items(op["items"].as_array().unwrap());
            let c = db.save_inventory(op["node_id"].as_str().unwrap(), &items, op["now"].as_i64().unwrap(), &strs(&op["skip"])).unwrap();
            serde_json::to_value(c).unwrap()
        })
        .collect()
}

#[test]
fn inventory_db_is_interchangeable_with_electron() {
    let mut r = Rng(0x9E6C_63D0_676A_9A99);
    let ops = ops(&mut r);
    let nodes = ["n1", "n2", "10", "none"];
    let base = std::env::temp_dir().join(format!("kt-parity-invdb-{}-{}", std::process::id(), tune_core::db::now_ms()));
    let (dir_rs, dir_js) = (base.join("rust"), base.join("js"));
    let (rust_results, rust_written) = {
        let db = Store::open(&dir_rs).unwrap();
        let res = rust_write(&db, &ops);
        (res, rust_dump(&db, &nodes))
    };
    let spec = json!({ "nodes": nodes });
    let Some(js) = run_js(&json!({ "dbRead": { "dir": dir_rs, "spec": spec }, "dbWrite": { "dir": dir_js, "spec": spec, "ops": ops } })) else { return };
    let diff = |a: &Value, b: &Value| -> String {
        a.as_object()
            .unwrap()
            .iter()
            .filter(|(k, v)| !b.get(k.as_str()).is_some_and(|w| same(v, w)))
            .map(|(k, v)| format!("{k}:\n  左: {v}\n  右: {}", b.get(k.as_str()).cloned().unwrap_or(Value::Null)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    // 1) 同じ手順の戻り値（増減の数・初回か・飛ばした種類）が同じ
    assert!(
        same(&js["dbWrite"]["results"], &json!(rust_results)),
        "saveInventory の戻り値が違う\nJS: {}\nRust: {}",
        js["dbWrite"]["results"],
        json!(rust_results)
    );
    // 2) Rust が書いた DB を JS（Electron 版の lib/db.js）で読むと、Rust で読んだのと同じ
    assert!(same(&js["dbRead"], &rust_written), "Rust が書いた DB を JS（左）と Rust（右）で読んだ結果が違う\n{}", diff(&js["dbRead"], &rust_written));
    // 3) JS が書いた DB を Rust で読むと、JS で読んだのと同じ。同じ手順で書いた2つの DB の中身も同じ
    let js_written_by_rust = rust_dump(&Store::open(&dir_js).unwrap(), &nodes);
    assert!(
        same(&js["dbWrite"]["dump"], &js_written_by_rust),
        "JS が書いた DB を JS（左）と Rust（右）で読んだ結果が違う\n{}",
        diff(&js["dbWrite"]["dump"], &js_written_by_rust)
    );
    assert!(same(&rust_written, &js_written_by_rust), "同じ手順で書いた DB の中身が違う（左: Rust、右: JS）\n{}", diff(&rust_written, &js_written_by_rust));
    // 増減が実際に起きている（比べた意味がある）
    let kinds: Vec<&str> = rust_written["events"].as_array().unwrap().iter().filter_map(|e| e["kind"].as_str()).collect();
    for k in ["added", "removed", "updated"] {
        assert!(kinds.contains(&k), "{k} の記録が無い");
    }
    let _ = std::fs::remove_dir_all(&base);
}
