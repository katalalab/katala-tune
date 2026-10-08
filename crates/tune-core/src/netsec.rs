//! ネットワークとセキュリティ（docs/observability.md の 6）。仕様は lib/netsec.js（同じ入力に同じ出力。tests/parity_netsec.rs）。
//!
//! - 純粋な部分（normalize・annotate・strip・persist_kinds・diff_persist・classify_peers・findings・checks）は JS 版と同じ
//! - [`ingest`] は DB（db/netsec.rs）を使い、常駐の増減と初めての接続先を決めて snapshot の netsec に書き込む（tune-core だけ）。
//!   宛先の IP は snapshot に残さず、`net_peers`（30 日）にだけ置く
//! - [`Engine::netsec_view`] は「セキュリティ」の画面（Tauri 版だけ）
//!
//! パケットの中身は扱わない。機体を変える処理は無い（調査は読み取り専用、ここは判定と保存だけ）。

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use serde_json::{Map, Value, json};

use crate::db::{Store, now_ms};
use crate::engine::Engine;
use crate::js;
use crate::nodes::Node;

pub const DAY: f64 = 86_400_000.0;
/// 外向きの接続は、機体ごとに最初の 7 日は覚えるだけ
pub const LEARN_DAYS: i64 = 7;
/// 宛先の保持（日）
pub const PEER_KEEP_DAYS: i64 = 30;
/// これ以下の宛先としか話さないプロセスだけ、新しい宛先を知らせる
pub const STABLE_MAX: usize = 10;
/// これ以上の番号は起動ごとに変わることが多いので、プロセスごとに1つとして比べる
pub const HIGH_PORT: f64 = 10000.0;
pub const XPROTECT_OLD_DAYS: f64 = 45.0;
pub const SIG_OLD_DAYS: f64 = 7.0;
/// 24 時間のログイン失敗のしきい値
pub const LOGIN_WARN: i64 = 10;
pub const LOGIN_CRIT: i64 = 100;
const LIM_LISTEN: usize = 500;
const LIM_OUTBOUND: usize = 400;
const LIM_PERSIST: usize = 3000;
const LIM_ADDRS: usize = 8;
const LIM_NAME: usize = 120;
const LIM_KEY: usize = 300;
const LIM_ERR: usize = 200;
const LIM_NEW: usize = 20;
/// errors のキー（調査の部分の名前）。この順に読む
pub const PARTS: [&str; 14] =
    ["listen", "firewall", "gatekeeper", "xprotect", "launchd", "defender", "av", "detections", "security_log", "tasks", "services", "run", "startup", "store"];
const DEFENSE_IDS: [&str; 6] = ["sec-gatekeeper-off", "sec-firewall-off", "sec-xprotect-old", "sec-av-off", "sec-av-old", "sec-detections"];

pub fn persist_kinds_of(os: &str) -> &'static [&'static str] {
    if os == "windows" { &["schtask", "service", "run", "startup"] } else { &["launchd"] }
}

fn kind_part(k: &str) -> &'static str {
    match k {
        "launchd" => "launchd",
        "schtask" => "tasks",
        "service" => "services",
        "run" => "run",
        "startup" => "startup",
        _ => "",
    }
}

pub fn kind_label(k: &str) -> Option<&'static str> {
    match k {
        "launchd" => Some("launchd"),
        "schtask" => Some("タスク"),
        "service" => Some("サービス"),
        "run" => Some("Run キー"),
        "startup" => Some("スタートアップ"),
        _ => None,
    }
}

fn why_rank(w: &str) -> f64 {
    match w {
        "proc" => 0.0,
        "port" => 1.0,
        "dest" => 2.0,
        _ => f64::NAN,
    }
}

pub fn why_label(w: &str) -> Option<&'static str> {
    match w {
        "proc" => Some("外と初めて通信したプロセス"),
        "port" => Some("初めてのポート"),
        "dest" => Some("決まった宛先としか話さないプロセスの新しい宛先"),
        _ => None,
    }
}

fn exposure_rank(e: &str) -> i32 {
    match e {
        "loopback" => 0,
        "lan" => 1,
        _ => 2,
    }
}

// ---- 型の決まった読み方（JS 版の S・N・A・O・B と同じ） ----
static EMPTY: LazyLock<Map<String, Value>> = LazyLock::new(Map::new);

fn st(v: Option<&Value>) -> &str {
    match v {
        Some(Value::String(s)) => s,
        _ => "",
    }
}

fn nm(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|x| x.is_finite()),
        _ => None,
    }
}

fn ar(v: Option<&Value>) -> &[Value] {
    js::arr(v)
}

fn ob(v: Option<&Value>) -> &Map<String, Value> {
    match v {
        Some(Value::Object(m)) => m,
        _ => &EMPTY,
    }
}

fn bo(v: Option<&Value>) -> Option<bool> {
    match v {
        Some(Value::Bool(b)) => Some(*b),
        _ => None,
    }
}

fn is_obj(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Object(_)))
}

fn rnd(v: Option<&Value>) -> Value {
    nm(v).map_or(Value::Null, |x| js::jnum(js::round(x)))
}

fn opt_bool(b: Option<bool>) -> Value {
    b.map_or(Value::Null, Value::Bool)
}

fn cut(s: &str, n: usize) -> String {
    if js::len16(s) > n { js::slice16(s, n) } else { s.to_string() }
}

/// UTF-16 の単位で比べる（JS の `<` と同じ）
fn cmp16(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn onoff(b: Option<bool>) -> &'static str {
    match b {
        Some(true) => "有効",
        Some(false) => "無効",
        None => "不明",
    }
}

fn ns_str(x: f64) -> String {
    js::num_str(x)
}

/// `${n}`（null なら "null"）
fn tmpl_num(x: Option<f64>) -> String {
    x.map_or_else(|| "null".into(), ns_str)
}

/// 台帳の機体で集めるか（"network": false で止める）
pub fn enabled(node: &Value) -> bool {
    !matches!(node.get("network"), Some(Value::Bool(false)))
}

/// ログインのアカウント名・送り元を残すか。共用機は明示 opt-in のときだけ集める。
pub fn logins_enabled(node: &Value) -> bool {
    if !enabled(node) || matches!(node.get("network_logins"), Some(Value::Bool(false))) {
        return false;
    }
    matches!(node.get("network_logins"), Some(Value::Bool(true))) || !matches!(node.get("shared"), Some(Value::Bool(true)))
}

/// 外向きの接続先（宛先）を残すか（lib/netsec.js の peersEnabled）。台帳の "network_peers" が true / false ならそれに従い、
/// 書いていなければ共用機（shared）は残さない（他の人の通信の宛先を集めないため）
pub fn peers_enabled(node: &Value) -> bool {
    if !enabled(node) {
        return false;
    }
    match node.get("network_peers") {
        Some(Value::Bool(b)) => *b,
        _ => !matches!(node.get("shared"), Some(Value::Bool(true))),
    }
}

/// 調査の結果から宛先の一覧を落とす（lib/netsec.js の dropPeers）
pub fn drop_peers(data: &mut Value) {
    if let Some(Value::Object(ns)) = data.get_mut("netsec") {
        ns.insert("outbound".into(), Value::Array(vec![]));
        ns.insert("outbound_skipped".into(), Value::String("shared".into()));
    }
}

/// アドレスと範囲（any・loopback・link・private・public）
#[derive(Clone, Debug, PartialEq)]
pub struct AddrInfo {
    pub addr: String,
    pub scope: &'static str,
}

pub fn addr_info(raw: Option<&Value>) -> Option<AddrInfo> {
    let mut a = js::trim(st(raw)).to_ascii_lowercase();
    if a.starts_with('[') && a.ends_with(']') {
        // JS の "[".slice(1, -1) は空文字
        a = if a.len() >= 2 { a[1..a.len() - 1].to_string() } else { String::new() };
    }
    if let Some(i) = a.find('%') {
        a.truncate(i);
    }
    if a.is_empty() || js::len16(&a) > 64 {
        return None;
    }
    if a == "*" || a == "0.0.0.0" || a == "::" || a == "0:0:0:0:0:0:0:0" {
        return Some(AddrInfo { addr: a, scope: "any" });
    }
    let v4 = if a.starts_with("::ffff:") && a.len() > 7 { &a[7..] } else { a.as_str() };
    let q: Vec<&str> = v4.split('.').collect();
    if q.len() == 4 && q.iter().all(|x| (1..=3).contains(&x.len()) && x.chars().all(|c| c.is_ascii_digit()) && x.parse::<u32>().is_ok_and(|n| n <= 255)) {
        let o: Vec<u32> = q.iter().map(|x| x.parse::<u32>().unwrap_or(0)).collect();
        let (o1, o2) = (o[0], o[1]);
        let scope = if o1 == 127 {
            "loopback"
        } else if o1 == 169 && o2 == 254 {
            "link"
        } else if o1 == 0
            || o1 == 10
            || (o1 == 172 && (16..=31).contains(&o2))
            || (o1 == 192 && o2 == 168)
            || (o1 == 100 && (64..=127).contains(&o2))
            || o1 >= 224
        {
            "private"
        } else {
            "public"
        };
        return Some(AddrInfo { addr: o.iter().map(u32::to_string).collect::<Vec<_>>().join("."), scope });
    }
    if a.contains(':') && a.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c) || c == ':' || c == '.') {
        let scope = if a == "::1" {
            "loopback"
        } else if ["fe8", "fe9", "fea", "feb"].iter().any(|p| a.starts_with(p)) {
            "link"
        } else if ["fc", "fd", "ff"].iter().any(|p| a.starts_with(p)) {
            "private"
        } else {
            "public"
        };
        return Some(AddrInfo { addr: a, scope });
    }
    if a == "localhost" {
        return Some(AddrInfo { addr: a, scope: "loopback" });
    }
    Some(AddrInfo { addr: a, scope: "private" })
}

pub fn is_public(raw: Option<&Value>) -> bool {
    addr_info(raw).is_some_and(|i| i.scope == "public")
}

fn exposure_of(scope: &str) -> &'static str {
    match scope {
        "any" => "any",
        "loopback" => "loopback",
        _ => "lan",
    }
}

fn is_ext(l: &Value) -> bool {
    let e = ob(Some(l)).get("exposure");
    js::is_str(e, "any") || js::is_str(e, "lan")
}

fn port_str(p: Option<&Value>) -> String {
    nm(p).map_or_else(|| "?".into(), ns_str)
}

/// 待ち受けを比べる鍵。番号の大きいもの（HIGH_PORT 以上）はプロセスごとに1つにまとめる
pub fn listen_key(l: &Value) -> String {
    let o = ob(Some(l));
    let p = match nm(o.get("port")) {
        None => String::new(),
        Some(x) if x < HIGH_PORT => ns_str(x),
        Some(_) => "high".into(),
    };
    format!("{}|{}|{p}", st(o.get("proto")), st(o.get("proc")))
}

fn listen_label(l: &Value) -> String {
    let o = ob(Some(l));
    let proc = st(o.get("proc"));
    format!("{} {} {}", if proc.is_empty() { "?" } else { proc }, if js::is_str(o.get("proto"), "udp") { "UDP" } else { "TCP" }, port_str(o.get("port")))
}

fn is_int_port(p: Option<f64>) -> Option<f64> {
    p.filter(|x| x.fract() == 0.0 && *x > 0.0 && *x <= 65535.0)
}

fn or_q(s: &str) -> &str {
    if s.is_empty() { "?" } else { s }
}

/// 調査の出力（netsec）を正規化する。probe = snapshot の probe（"mac" | "windows"）
pub fn normalize(raw: &Value, probe: &str) -> Value {
    let r = ob(Some(raw));
    let os = if probe == "windows" { "windows" } else { "mac" };

    // 待ち受け: (proto, port, proc) でまとめる
    struct G {
        proto: &'static str,
        port: f64,
        proc_: String,
        exposure: &'static str,
        addrs: Vec<String>,
    }
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, G> = HashMap::new();
    for x in ar(r.get("listen")) {
        let o = ob(Some(x));
        let proto = match o.get("proto") {
            Some(Value::String(s)) if s == "tcp" => "tcp",
            Some(Value::String(s)) if s == "udp" => "udp",
            _ => continue,
        };
        let Some(port) = is_int_port(nm(o.get("port"))) else { continue };
        let Some(ai) = addr_info(o.get("addr")) else { continue };
        let proc_ = cut(or_q(st(o.get("proc"))), LIM_NAME);
        let key = format!("{proto}|{}|{proc_}", ns_str(port));
        let ex = exposure_of(ai.scope);
        if !groups.contains_key(&key) {
            if groups.len() >= LIM_LISTEN {
                continue;
            }
            order.push(key.clone());
            groups.insert(key.clone(), G { proto, port, proc_, exposure: ex, addrs: Vec::new() });
        }
        let g = groups.get_mut(&key).expect("inserted");
        if exposure_rank(ex) > exposure_rank(g.exposure) {
            g.exposure = ex;
        }
        if !g.addrs.contains(&ai.addr) && g.addrs.len() < LIM_ADDRS {
            g.addrs.push(ai.addr);
        }
    }
    let mut listen: Vec<G> = order.into_iter().filter_map(|k| groups.remove(&k)).collect();
    for g in &mut listen {
        g.addrs.sort_by(|a, b| cmp16(a, b));
    }
    listen.sort_by(|a, b| cmp16(a.proto, b.proto).then(a.port.partial_cmp(&b.port).unwrap_or(Ordering::Equal)).then(cmp16(&a.proc_, &b.proc_)));
    let listen: Vec<Value> =
        listen.into_iter().map(|g| json!({ "proto": g.proto, "port": js::jnum(g.port), "proc": g.proc_, "exposure": g.exposure, "addrs": g.addrs })).collect();

    // 外向きの接続（標本）
    struct P {
        proc_: String,
        addr: String,
        port: f64,
        n: f64,
        public: bool,
    }
    let mut porder: Vec<String> = Vec::new();
    let mut peers: HashMap<String, P> = HashMap::new();
    for x in ar(r.get("outbound")) {
        let o = ob(Some(x));
        let Some(port) = is_int_port(nm(o.get("port"))) else { continue };
        let Some(ai) = addr_info(o.get("addr")) else { continue };
        if ai.scope == "any" || ai.scope == "loopback" {
            continue;
        }
        let proc_ = cut(or_q(st(o.get("proc"))), LIM_NAME);
        let n = nm(o.get("n")).map_or(1.0, |v| js::round(v).clamp(1.0, 100_000.0));
        let key = format!("{proc_}|{}|{}", ai.addr, ns_str(port));
        if let Some(g) = peers.get_mut(&key) {
            g.n = (g.n + n).min(1_000_000.0);
        } else if peers.len() < LIM_OUTBOUND {
            porder.push(key.clone());
            peers.insert(key, P { proc_, public: ai.scope == "public", addr: ai.addr, port, n });
        }
    }
    let mut outbound: Vec<P> = porder.into_iter().filter_map(|k| peers.remove(&k)).collect();
    outbound.sort_by(|a, b| cmp16(&a.proc_, &b.proc_).then(a.port.partial_cmp(&b.port).unwrap_or(Ordering::Equal)).then(cmp16(&a.addr, &b.addr)));
    let outbound: Vec<Value> =
        outbound.into_iter().map(|p| json!({ "proc": p.proc_, "addr": p.addr, "port": js::jnum(p.port), "n": js::jnum(p.n), "public": p.public })).collect();

    // 防御の状態
    let d = ob(r.get("defense"));
    let defense = if os == "mac" {
        let fw = nm(d.get("firewall")).filter(|x| [0.0, 1.0, 2.0].contains(x)).map_or(Value::Null, js::jnum);
        let xv = st(d.get("xprotect_version"));
        json!({
            "firewall": fw,
            "stealth": opt_bool(bo(d.get("stealth"))),
            "gatekeeper": opt_bool(bo(d.get("gatekeeper"))),
            "xprotect_version": if xv.is_empty() { Value::Null } else { Value::from(cut(xv, 40)) },
            "xprotect_at": rnd(d.get("xprotect_at")),
        })
    } else {
        let defender = if is_obj(d.get("defender")) {
            let df = ob(d.get("defender"));
            json!({
                "realtime": opt_bool(bo(df.get("realtime"))), "antivirus": opt_bool(bo(df.get("antivirus"))), "service": opt_bool(bo(df.get("service"))),
                "mode": cut(st(df.get("mode")), 40), "sig_age_days": rnd(df.get("sig_age_days")), "sig_at": rnd(df.get("sig_at")), "tamper": opt_bool(bo(df.get("tamper"))),
            })
        } else {
            Value::Null
        };
        let av: Vec<Value> = ar(d.get("av"))
            .iter()
            .take(10)
            .map(|x| {
                let o = ob(Some(x));
                json!({ "name": cut(st(o.get("name")), 80), "enabled": opt_bool(bo(o.get("enabled"))), "uptodate": opt_bool(bo(o.get("uptodate"))) })
            })
            .collect();
        let fw: Vec<Value> = ar(d.get("firewall"))
            .iter()
            .take(5)
            .map(|x| {
                let o = ob(Some(x));
                json!({ "name": cut(st(o.get("name")), 20), "enabled": opt_bool(bo(o.get("enabled"))) })
            })
            .collect();
        let active: Vec<Value> = ar(d.get("active")).iter().filter_map(Value::as_str).take(5).map(|s| Value::from(cut(s, 20))).collect();
        let sl = match d.get("security_log") {
            Some(Value::String(s)) if s == "ok" || s == "no-permission" => Value::from(s.as_str()),
            _ => Value::Null,
        };
        json!({
            "defender": defender, "av": av, "firewall": fw, "active": active, "detections_30d": rnd(d.get("detections_30d")),
            "detections_open_30d": rnd(d.get("detections_open_30d")), "security_log": sl,
        })
    };

    // 自動起動（常駐）の一覧。(種類, 鍵) で一意
    let kinds = persist_kinds_of(os);
    let mut seen: HashSet<String> = HashSet::new();
    let mut persist: Vec<(String, String, Value)> = Vec::new();
    for x in ar(r.get("persist")) {
        let o = ob(Some(x));
        let kind = st(o.get("kind"));
        let key = cut(st(o.get("key")), LIM_KEY);
        if !kinds.contains(&kind) || key.is_empty() {
            continue;
        }
        let k = format!("{kind}|{key}");
        if seen.contains(&k) || seen.len() >= LIM_PERSIST {
            continue;
        }
        seen.insert(k);
        let prog = cut(st(o.get("program")), LIM_NAME);
        persist.push((kind.to_string(), key, if prog.is_empty() { Value::Null } else { Value::from(prog) }));
    }
    persist.sort_by(|a, b| cmp16(&a.0, &b.0).then(cmp16(&a.1, &b.1)));
    let persist: Vec<Value> = persist.into_iter().map(|(kind, key, program)| json!({ "kind": kind, "key": key, "program": program })).collect();

    let e = ob(r.get("errors"));
    let pm = ob(r.get("parts_ms"));
    let mut errors = Map::new();
    let mut parts_ms = Map::new();
    for p in PARTS {
        if let Some(Value::String(s)) = e.get(p) {
            errors.insert(p.into(), Value::from(cut(s, LIM_ERR)));
        }
        if let Some(x) = nm(pm.get(p)) {
            parts_ms.insert(p.into(), js::jnum(js::round(x)));
        }
    }
    let mut out = json!({
        "v": 1, "os": os, "listen": listen, "outbound": outbound, "defense": defense, "persist": persist,
        "errors": errors, "parts_ms": parts_ms, "elapsed_ms": rnd(r.get("elapsed_ms")), "cpu_ms": rnd(r.get("cpu_ms")),
    });
    // 共用機で宛先を落としたしるし（drop_peers）。あれば初めての接続先を覚えない・点検に出さない
    if r.get("outbound_skipped").and_then(Value::as_str) == Some("shared") {
        out["outbound_skipped"] = json!("shared");
    }
    out
}

/// 前回の分析と比べる。at = この分析の時刻（ms）。prev = 前回の snapshot の netsec
pub fn annotate(ns: &Value, prev: Option<&Value>, at: Option<&Value>) -> Value {
    let cur = ob(Some(ns));
    let p = ob(prev);
    let usable =
        |x: &Map<String, Value>| matches!(x.get("listen"), Some(Value::Array(_))) && !matches!(ob(x.get("errors")).get("listen"), Some(Value::String(_)));
    let mut out = cur.clone();
    out.insert("at".into(), rnd(at));
    let mut new: Vec<Value> = Vec::new();
    let mut base = true;
    if usable(cur) && usable(p) {
        let before: HashSet<String> = ar(p.get("listen")).iter().filter(|l| is_ext(l)).map(listen_key).collect();
        new = ar(cur.get("listen")).iter().filter(|l| is_ext(l) && !before.contains(&listen_key(l))).take(LIM_NEW).cloned().collect();
        base = false;
    }
    out.insert("listen_new".into(), Value::Array(new));
    out.insert("listen_base".into(), Value::Bool(base));
    Value::Object(out)
}

/// snapshot に残す形にする。宛先（outbound）と自動起動の一覧（persist）は件数だけ残す
pub fn strip(ns: &Value) -> Value {
    let mut o = ob(Some(ns)).clone();
    let outbound = ar(o.get("outbound")).len();
    let persist = ar(o.get("persist")).len();
    o.remove("outbound");
    o.remove("persist");
    o.insert("outbound_count".into(), Value::from(outbound));
    o.insert("persist_count".into(), Value::from(persist));
    if let Some(peers) = o.get_mut("peers") {
        sanitize_peer_summary(peers);
    }
    Value::Object(o)
}

fn sanitize_peer_summary(peers: &mut Value) {
    let Some(o) = peers.as_object_mut() else { return };
    o.retain(|k, _| matches!(k.as_str(), "learning" | "since" | "until" | "sampled" | "known" | "new"));
    if let Some(rows) = o.get_mut("new").and_then(Value::as_array_mut) {
        for row in rows {
            if let Some(r) = row.as_object_mut() {
                r.retain(|k, _| matches!(k.as_str(), "proc" | "port" | "why" | "dests" | "public"));
            }
        }
    }
}

/// Engine の外へ返す snapshot。古い保存値にも同じ宛先の除外と現在の台帳の設定を適用する。
pub fn snapshot_for_node(mut data: Value, node: &Value) -> Value {
    if !enabled(node) {
        if let Some(o) = data.as_object_mut() {
            o.remove("netsec");
        }
        return data;
    }
    let Some(ns) = data.get_mut("netsec").and_then(Value::as_object_mut) else { return data };
    ns.remove("outbound");
    if let Some(peers) = ns.get_mut("peers") {
        sanitize_peer_summary(peers);
    }
    if !peers_enabled(node) {
        ns.remove("peers");
        ns.remove("outbound_count");
    }
    data
}

/// ログイン収集を止めた後も DB に残る旧所見を公開結果へ混ぜない。
pub fn filter_log_findings(mut findings: Vec<Value>, node: &Value) -> Vec<Value> {
    if !logins_enabled(node) {
        findings.retain(|f| !st(f.get("id")).starts_with("log-login-"));
    }
    findings
}

fn login_node_ids(nodes: &[Value]) -> HashSet<String> {
    nodes.iter().filter(|n| logins_enabled(n)).map(|n| st(n.get("id"))).filter(|id| !id.is_empty()).map(str::to_string).collect()
}

fn login_source(row: &Value) -> bool {
    matches!(st(row.get("source")), "mac_auth" | "win_security")
}

/// logs-query / cursors の旧ログイン情報に現在の台帳の privacy 設定を適用する。
pub fn filter_log_rows(rows: Vec<Value>, nodes: &[Value]) -> Vec<Value> {
    let allowed = login_node_ids(nodes);
    rows.into_iter().filter(|r| !login_source(r) || allowed.contains(st(r.get("node_id")))).collect()
}

/// logs-signatures は複数機体を集約するため、許可されない機体が一つでも含まれるログイン署名は返さない。
pub fn filter_log_signatures(rows: Vec<Value>, nodes: &[Value]) -> Vec<Value> {
    let allowed = login_node_ids(nodes);
    rows.into_iter()
        .filter(|r| {
            if !login_source(r) {
                return true;
            }
            let ids: Vec<&str> = st(r.get("node_ids")).split(',').map(str::trim).filter(|id| !id.is_empty()).collect();
            !ids.is_empty() && ids.iter().all(|id| allowed.contains(*id))
        })
        .collect()
}

/// status の現在値と履歴に、現在の台帳で非公開になった security 項目を残さない。
pub fn filter_check_rows(rows: Vec<Value>, nodes: &[Value]) -> Vec<Value> {
    let by_id: HashMap<String, &Value> = nodes.iter().map(|n| (st(n.get("id")).to_string(), n)).collect();
    rows.into_iter()
        .filter(|r| {
            let id = r.get("id").and_then(Value::as_str).unwrap_or_else(|| st(r.get("check_id")));
            if !id.starts_with("sec-") {
                return true;
            }
            let Some(node) = by_id.get(st(r.get("scope"))) else { return false };
            enabled(node) && (id != "sec-login" || logins_enabled(node)) && (id != "sec-peers" || peers_enabled(node))
        })
        .collect()
}

/// Electron 版の分析の後処理と同じ（normalize → annotate → strip）
pub fn prepare(raw: &Value, probe: &str, prev: Option<&Value>, at: Option<&Value>) -> Value {
    strip(&annotate(&normalize(raw, probe), prev, at))
}

/// 今回取った自動起動の種類と、取れなかった種類
pub fn persist_kinds(ns: &Value) -> (Vec<&'static str>, Vec<&'static str>) {
    let o = ob(Some(ns));
    let kinds = persist_kinds_of(if js::is_str(o.get("os"), "windows") { "windows" } else { "mac" });
    let e = ob(o.get("errors"));
    let failed = kinds.iter().copied().filter(|k| matches!(e.get(kind_part(k)), Some(Value::String(_)))).collect();
    (kinds.to_vec(), failed)
}

/// 自動起動の増減（lib/netsec.js の diffPersist）。戻り値は `{ changes, baseline }`
pub fn diff_persist(prev: &[Value], items: &[Value], kinds: &[String], failed: &[String], baselined: &[String]) -> Value {
    let mut changes: Vec<Value> = Vec::new();
    let mut baseline: Vec<Value> = Vec::new();
    for kind in kinds {
        if failed.contains(kind) {
            continue;
        }
        if !baselined.contains(kind) {
            baseline.push(Value::from(kind.as_str()));
            continue;
        }
        // Map と同じく、同じ鍵は後のもので上書きし、位置は最初のまま
        let mut order: Vec<String> = Vec::new();
        let mut before: HashMap<String, &Map<String, Value>> = HashMap::new();
        for x in prev {
            let o = ob(Some(x));
            if st(o.get("kind")) == kind && nm(o.get("removed_at")).is_none() {
                let key = st(o.get("key")).to_string();
                if !before.contains_key(&key) {
                    order.push(key.clone());
                }
                before.insert(key, o);
            }
        }
        for x in items {
            let it = ob(Some(x));
            if st(it.get("kind")) != kind {
                continue;
            }
            let key = st(it.get("key"));
            let prog = st(it.get("program"));
            let program = if prog.is_empty() { Value::Null } else { Value::from(prog) };
            match before.remove(key) {
                None => changes.push(json!({ "kind": kind, "key": key, "program": program, "change": "added" })),
                Some(p) => {
                    order.retain(|k| k != key);
                    let w = st(p.get("program"));
                    let was = if w.is_empty() { Value::Null } else { Value::from(w) };
                    if was != program {
                        changes.push(json!({ "kind": kind, "key": key, "program": program, "change": "changed", "from": was }));
                    }
                }
            }
        }
        for key in order {
            if let Some(p) = before.get(&key) {
                let w = st(p.get("program"));
                changes.push(json!({ "kind": kind, "key": key, "program": if w.is_empty() { Value::Null } else { Value::from(w) }, "change": "removed" }));
            }
        }
    }
    json!({ "changes": changes, "baseline": baseline })
}

/// 初めての接続先（lib/netsec.js の classifyPeers）
pub fn classify_peers(known: &[Value], sample: &[Value], learning: bool) -> Vec<Value> {
    let mut procs: HashSet<String> = HashSet::new();
    let mut ports: HashSet<String> = HashSet::new();
    let mut pairs: HashSet<String> = HashSet::new();
    let mut addrs: HashMap<String, HashSet<String>> = HashMap::new();
    for x in known {
        let k = ob(Some(x));
        let (proc_, addr, port) = (st(k.get("proc")), st(k.get("addr")), tmpl_num(nm(k.get("port"))));
        procs.insert(proc_.to_string());
        ports.insert(format!("{proc_}|{port}"));
        pairs.insert(format!("{proc_}|{addr}|{port}"));
        addrs.entry(proc_.to_string()).or_default().insert(addr.to_string());
    }
    struct G {
        proc_: String,
        port: Option<f64>,
        why: &'static str,
        dests: i64,
        public: bool,
    }
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, G> = HashMap::new();
    for x in sample {
        let s = ob(Some(x));
        let (proc_, addr, port) = (st(s.get("proc")), st(s.get("addr")), nm(s.get("port")));
        let ps = tmpl_num(port);
        if pairs.contains(&format!("{proc_}|{addr}|{ps}")) {
            continue;
        }
        let why = if !procs.contains(proc_) {
            "proc"
        } else if !ports.contains(&format!("{proc_}|{ps}")) {
            "port"
        } else if addrs.get(proc_).map_or(0, HashSet::len) <= STABLE_MAX {
            "dest"
        } else {
            continue;
        };
        let key = format!("{proc_}|{ps}");
        let g = groups.entry(key.clone()).or_insert_with(|| {
            order.push(key);
            G { proc_: proc_.to_string(), port, why, dests: 0, public: false }
        });
        g.dests += 1;
        if matches!(s.get("public"), Some(Value::Bool(true))) {
            g.public = true;
        }
        if why_rank(why) < why_rank(g.why) {
            g.why = why;
        }
    }
    if learning {
        return Vec::new();
    }
    let mut out: Vec<G> = order.into_iter().filter_map(|k| groups.remove(&k)).collect();
    out.sort_by(|a, b| {
        (why_rank(a.why) - why_rank(b.why))
            .partial_cmp(&0.0)
            .unwrap_or(Ordering::Equal)
            .then(cmp16(&a.proc_, &b.proc_))
            .then((a.port.unwrap_or(-1.0) - b.port.unwrap_or(-1.0)).partial_cmp(&0.0).unwrap_or(Ordering::Equal))
    });
    out.into_iter()
        .take(LIM_NEW)
        .map(|g| json!({ "proc": g.proc_, "port": g.port.map_or(Value::Null, js::jnum), "why": g.why, "dests": g.dests, "public": g.public }))
        .collect()
}

fn days_since(at: Option<f64>, t: Option<f64>) -> Option<f64> {
    match (at, t) {
        (Some(a), Some(t)) => Some(((a - t) / DAY).floor()),
        _ => None,
    }
}

/// 注意にする初めての接続先: 外と初めて通信したプロセスが、外部のアドレスへ 80・443 以外の番号で話している
/// （新しく入れたソフトや更新は 443 で話すことが多く、443 だけで知らせると注意が出続けたため）
fn odd_peer(p: &Map<String, Value>) -> bool {
    js::is_str(p.get("why"), "proc") && bo(p.get("public")) == Some(true) && nm(p.get("port")).is_some_and(|x| x != 443.0 && x != 80.0)
}

/// macOS のファイアウォールが「すべての受信を遮断」（State = 2）なら、待ち受けは外から届かない
fn blocks_all(ns: &Map<String, Value>) -> bool {
    !js::is_str(ns.get("os"), "windows") && nm(ob(ns.get("defense")).get("firewall")) == Some(2.0)
}

struct Out(Vec<Value>);

impl Out {
    fn add(&mut self, id: String, severity: &str, title: String, detail: String, advice: String) {
        self.0.push(json!({ "id": id, "severity": severity, "category": "security", "title": title, "detail": detail, "advice": advice }));
    }
}

fn defender_of(d: &Map<String, Value>) -> Option<&Map<String, Value>> {
    match d.get("defender") {
        Some(Value::Object(m)) => Some(m),
        _ => None,
    }
}

/// 所見（lib/netsec.js の findings）。node = 台帳の1台（shared を見る）
pub fn findings(ns: Option<&Value>, node: &Value) -> Vec<Value> {
    if !enabled(node) {
        return Vec::new();
    }
    let Some(Value::Object(ns)) = ns else { return Vec::new() };
    let mut out = Out(Vec::new());
    let shared = matches!(ob(Some(node)).get("shared"), Some(Value::Bool(true)));
    let d = ob(ns.get("defense"));
    let errors = ob(ns.get("errors"));
    let has_def = js::truthy(ns.get("defense"));
    let ext: Vec<&Value> = ar(ns.get("listen")).iter().filter(|l| is_ext(l)).collect();
    let ext_tcp: Vec<&Value> = ext.iter().copied().filter(|l| js::is_str(ob(Some(l)).get("proto"), "tcp")).collect();
    let windows = js::is_str(ns.get("os"), "windows");

    if windows && has_def {
        let df = defender_of(d);
        let av: Vec<&Map<String, Value>> = ar(d.get("av")).iter().map(|x| ob(Some(x))).collect();
        let df_on = df.is_some_and(|m| bo(m.get("realtime")) == Some(true));
        let mut states: Vec<Option<bool>> = df.map(|m| bo(m.get("realtime"))).into_iter().collect();
        states.extend(av.iter().map(|a| bo(a.get("enabled"))));
        let explicitly_off = !states.is_empty() && states.iter().all(|v| *v == Some(false));
        let age = df.and_then(|m| nm(m.get("sig_age_days")));
        let defender_read = df.is_some() && !matches!(errors.get("defender"), Some(Value::String(_)));
        let av_read = !matches!(errors.get("av"), Some(Value::String(_)));
        if defender_read && av_read && explicitly_off {
            let list = if av.is_empty() {
                String::new()
            } else {
                format!(
                    "、登録: {}",
                    av.iter().map(|a| format!("{}（{}）", or_q(st(a.get("name"))), onoff(bo(a.get("enabled"))))).collect::<Vec<_>>().join("、")
                )
            };
            out.add(
                "sec-av-off".into(),
                "critical",
                "リアルタイムのウイルス対策が動いていない".into(),
                format!("Defender のリアルタイム保護 {}{list}", df.map_or("不明", |m| onoff(bo(m.get("realtime"))))),
                "Windows セキュリティ > ウイルスと脅威の防止 で、リアルタイム保護をオンにする。別のウイルス対策を入れているなら、それが動いているかを確かめる。この画面からは変更しない。".into(),
            );
        } else if let (true, Some(age)) = (df_on, age.filter(|a| *a >= SIG_OLD_DAYS)) {
            let small = age < 10000.0;
            out.add(
                "sec-av-old".into(),
                "warn",
                if small {
                    format!("Defender の定義が {} 日前のまま", ns_str(age))
                } else {
                    "Defender の定義の更新日が分からない".into()
                },
                format!("定義の更新 {}", if small { format!("{} 日前", ns_str(age)) } else { "記録なし".into() }),
                "Windows Update か「ウイルスと脅威の防止の更新」で定義を更新する。更新が止まり続けるなら、ネットワークと Windows Update のエラーを確かめる。"
                    .into(),
            );
        } else if let Some(tp) = av.iter().find(|a| bo(a.get("enabled")) == Some(true) && bo(a.get("uptodate")) == Some(false))
            && !df_on
        {
            let name = st(tp.get("name"));
            out.add(
                "sec-av-old".into(),
                "warn",
                format!("{} の定義が古い", if name.is_empty() { "ウイルス対策" } else { name }),
                "Windows セキュリティに登録された状態".into(),
                "そのソフトの画面で定義を更新する。".into(),
            );
        }
        let fw: Vec<&Map<String, Value>> = ar(d.get("firewall")).iter().map(|x| ob(Some(x))).collect();
        let off: Vec<&&Map<String, Value>> = fw.iter().filter(|p| bo(p.get("enabled")) == Some(false)).collect();
        if !off.is_empty() {
            let active: Vec<&str> = ar(d.get("active")).iter().filter_map(Value::as_str).collect();
            let hot = off.iter().any(|p| active.contains(&st(p.get("name"))));
            out.add(
                "sec-firewall-off".into(),
                if hot { "critical" } else { "warn" },
                format!("ファイアウォールが無効（{}）", off.iter().map(|p| or_q(st(p.get("name")))).collect::<Vec<_>>().join("、")),
                if active.is_empty() { "いまつながっているネットワークの種類は不明".into() } else { format!("いまつながっているネットワークの種類: {}", active.join("、")) },
                "Windows セキュリティ > ファイアウォールとネットワーク保護 で有効にする。特定のアプリだけ通したいなら、全体を切らずに規則を足す。この画面からは変更しない。".into(),
            );
        }
        // 検出は、隔離・削除・駆除・遮断が済んでいれば提案、未解決（または状態が分からない）なら注意
        if let Some(det) = nm(d.get("detections_30d")).filter(|x| *x > 0.0) {
            let open = nm(d.get("detections_open_30d"));
            let (sev, title) = match open {
                None => ("warn", format!("Defender が直近30日に {} 件を検出した", ns_str(det))),
                Some(o) if o > 0.0 => ("warn", format!("Defender が直近30日に {} 件を検出し、{} 件が未解決", ns_str(det), ns_str(o))),
                Some(_) => ("info", format!("Defender が直近30日に {} 件を検出した（すべて隔離・削除済み）", ns_str(det))),
            };
            out.add(
                "sec-detections".into(),
                sev,
                title,
                "Defender の検出の記録（MSFT_MpThreatDetection）".into(),
                "Windows セキュリティ > ウイルスと脅威の防止 > 保護の履歴 で、何がどこで見つかり、どう処理されたかを確かめる。".into(),
            );
        }
    } else if !windows && has_def {
        if bo(d.get("gatekeeper")) == Some(false) {
            out.add(
                "sec-gatekeeper-off".into(),
                "critical",
                "Gatekeeper が無効".into(),
                "spctl --status: assessments disabled".into(),
                "署名や公証を確かめずにアプリが開く状態。`sudo spctl --global-enable` で戻せる（管理者権限が要る。この画面からは変更しない）。".into(),
            );
        }
        if nm(d.get("firewall")) == Some(0.0) {
            let detail = if ext_tcp.is_empty() {
                "外から届く TCP の待ち受けは無い".to_string()
            } else {
                format!(
                    "外から届く TCP の待ち受けが {} 件: {}{}",
                    ext_tcp.len(),
                    ext_tcp.iter().take(4).map(|l| listen_label(l)).collect::<Vec<_>>().join("、"),
                    if ext_tcp.len() > 4 { " ほか" } else { "" }
                )
            };
            out.add(
                "sec-firewall-off".into(),
                if ext_tcp.is_empty() { "info" } else { "warn" },
                "ファイアウォールが無効".into(),
                detail,
                "システム設定 > ネットワーク > ファイアウォール で有効にする。外から使うもの（画面共有・ファイル共有など）は、有効にした後に許可される。この画面からは変更しない。".into(),
            );
        }
        if let Some(xd) = days_since(nm(ns.get("at")), nm(d.get("xprotect_at"))).filter(|x| *x >= XPROTECT_OLD_DAYS) {
            let v = st(d.get("xprotect_version"));
            out.add(
                "sec-xprotect-old".into(),
                "warn",
                format!("XProtect の定義が {} 日更新されていない", ns_str(xd)),
                format!("版 {}", if v.is_empty() { "不明" } else { v }),
                "macOS の自動アップデート（「セキュリティ対応とシステムファイルをインストール」）がオンかを確かめる。".into(),
            );
        }
    }

    let blocked = blocks_all(ns);
    for x in ar(ns.get("listen_new")).iter().take(5) {
        let l = ob(Some(x));
        let tcp = !js::is_str(l.get("proto"), "udp");
        let proc_ = st(l.get("proc"));
        let addrs: Vec<&str> = ar(l.get("addrs")).iter().filter_map(Value::as_str).collect();
        out.add(
            format!("net-listen-{}-{}-{proc_}", if tcp { "tcp" } else { "udp" }, port_str(l.get("port"))),
            if tcp && !blocked { "warn" } else { "info" },
            format!("{} が {} {} で外から届く待ち受けを始めた", or_q(proc_), if tcp { "TCP" } else { "UDP" }, port_str(l.get("port"))),
            format!(
                "{}（前回の分析には無かった{}）",
                if addrs.is_empty() { "-".to_string() } else { addrs.join(", ") },
                if blocked { "。ファイアウォールがすべての受信を遮断しているので、いまは外から届かない" } else { "" }
            ),
            "心当たりが無ければ、そのアプリの共有・リモート操作・開発サーバ（--host など）の設定を確かめる。この機体の中だけで使うなら 127.0.0.1 で待つ設定にする。".into(),
        );
    }

    let added: Vec<&Map<String, Value>> = ar(ns.get("persist_changes")).iter().map(|x| ob(Some(x))).filter(|c| js::is_str(c.get("change"), "added")).collect();
    if !added.is_empty() {
        let list = added
            .iter()
            .take(5)
            .map(|c| {
                let k = st(c.get("kind"));
                let p = st(c.get("program"));
                format!("{} {}{}", kind_label(k).unwrap_or(k), st(c.get("key")), if p.is_empty() { String::new() } else { format!("（{p}）") })
            })
            .collect::<Vec<_>>()
            .join("、");
        out.add(
            "net-persist-added".into(),
            "warn",
            format!("自動起動が {} 件増えた", added.len()),
            format!("{list}{}", if added.len() > 5 { " ほか" } else { "" }),
            "入れた覚えのあるソフト（更新を含む）なら問題ない。覚えが無いものは、実行ファイルの場所と署名を確かめる。増減の記録は「セキュリティ」の画面にある。".into(),
        );
    }

    if let Some(Value::Object(pe)) = ns.get("peers")
        && bo(pe.get("learning")) != Some(true)
    {
        let nw: Vec<&Map<String, Value>> = ar(pe.get("new")).iter().map(|x| ob(Some(x))).collect();
        if !nw.is_empty() {
            let list = nw
                .iter()
                .take(5)
                .map(|p| {
                    let w = st(p.get("why"));
                    let dests = nm(p.get("dests")).filter(|x| *x > 1.0).map(|x| format!("・宛先 {}", ns_str(x))).unwrap_or_default();
                    format!("{} → {} 番（{}{dests}）", or_q(st(p.get("proc"))), port_str(p.get("port")), why_label(w).unwrap_or(w))
                })
                .collect::<Vec<_>>()
                .join("、");
            out.add(
                "net-peers-new".into(),
                if nw.iter().any(|p| odd_peer(p)) && !shared { "warn" } else { "info" },
                format!("初めての接続先が {} 件", nw.len()),
                format!("{list}{}", if nw.len() > 5 { " ほか" } else { "" }),
                format!(
                    "新しく入れたソフトや更新なら問題ない。心当たりの無いプロセスなら、宛先（「セキュリティ」の画面）と実行ファイルを確かめる。{}",
                    if shared { "共用機なので提案だけにしている。" } else { "" }
                ),
            );
        }
    }
    out.0
}

fn defense_summary(ns: &Map<String, Value>) -> String {
    let d = ob(ns.get("defense"));
    if js::is_str(ns.get("os"), "windows") {
        let df = defender_of(d);
        let others: Vec<&str> = ar(d.get("av"))
            .iter()
            .map(|x| ob(Some(x)))
            .filter(|a| bo(a.get("enabled")) == Some(true) && !st(a.get("name")).to_ascii_lowercase().contains("defender"))
            .map(|a| st(a.get("name")))
            .collect();
        let age = df.and_then(|m| nm(m.get("sig_age_days")));
        let fw: Vec<&Map<String, Value>> = ar(d.get("firewall")).iter().map(|x| ob(Some(x))).collect();
        let mut parts = vec![format!(
            "リアルタイム保護 {}{}",
            df.map_or("不明", |m| onoff(bo(m.get("realtime")))),
            if others.is_empty() { String::new() } else { format!("（{}）", others.join("、")) }
        )];
        if let Some(a) = age.filter(|a| *a < 10000.0) {
            parts.push(format!("定義 {} 日前", ns_str(a)));
        }
        if !fw.is_empty() {
            parts.push(format!(
                "ファイアウォール {}",
                fw.iter().map(|p| format!("{} {}", or_q(st(p.get("name"))), onoff(bo(p.get("enabled"))))).collect::<Vec<_>>().join("・")
            ));
        }
        if let Some(det) = nm(d.get("detections_30d")).filter(|x| *x > 0.0) {
            let open = nm(d.get("detections_open_30d")).map_or_else(|| "不明".into(), ns_str);
            parts.push(format!("検出 30 日で {} 件（未解決 {open}）", ns_str(det)));
        }
        return parts.join("、");
    }
    let fw = nm(d.get("firewall"));
    let xd = days_since(nm(ns.get("at")), nm(d.get("xprotect_at")));
    let v = st(d.get("xprotect_version"));
    [
        format!(
            "ファイアウォール {}",
            match fw {
                Some(2.0) => "すべて遮断",
                Some(1.0) => "有効",
                Some(0.0) => "無効",
                _ => "不明",
            }
        ),
        format!("Gatekeeper {}", onoff(bo(d.get("gatekeeper")))),
        format!("XProtect {}{}", if v.is_empty() { "不明" } else { v }, xd.map(|x| format!("（{} 日前）", ns_str(x))).unwrap_or_default()),
    ]
    .join("、")
}

fn worst(xs: &[&Map<String, Value>]) -> &'static str {
    if xs.iter().any(|f| js::is_str(f.get("severity"), "critical")) {
        "fail"
    } else if xs.iter().any(|f| js::is_str(f.get("severity"), "warn")) {
        "warn"
    } else {
        "ok"
    }
}

/// 状態（health の node_checks から呼ぶ）。findings = その機体の所見（ログ由来を含む）、ctx = node_checks の ctx（cursors）
pub fn checks(ns: Option<&Value>, findings: &[Value], ctx: &Value, node: &Value) -> Vec<crate::db::Check> {
    if !enabled(node) {
        return Vec::new();
    }
    let Some(Value::Object(ns)) = ns else { return Vec::new() };
    let mut out: Vec<crate::db::Check> = Vec::new();
    let mut add = |id: &str, name: &str, status: &str, detail: String| {
        out.push(crate::db::Check { id: id.into(), name: name.into(), status: status.into(), detail: Some(detail) });
    };
    let fs: Vec<&Map<String, Value>> = findings.iter().map(|f| ob(Some(f))).collect();
    let e = ob(ns.get("errors"));
    let err = |p: &str| -> Option<String> { e.get(p).and_then(Value::as_str).map(str::to_string) };
    let shared = matches!(ob(Some(node)).get("shared"), Some(Value::Bool(true)));
    let windows = js::is_str(ns.get("os"), "windows");

    // 防御
    let def_parts: &[&str] = if windows { &["defender", "av", "firewall", "detections"] } else { &["firewall", "gatekeeper", "xprotect"] };
    let def_errs: Vec<&str> = def_parts.iter().copied().filter(|p| err(p).is_some()).collect();
    let def_f: Vec<&Map<String, Value>> = fs
        .iter()
        .copied()
        .filter(|f| DEFENSE_IDS.contains(&st(f.get("id"))) && (js::is_str(f.get("severity"), "critical") || js::is_str(f.get("severity"), "warn")))
        .collect();
    let defense = ob(ns.get("defense"));
    let df = defender_of(defense);
    let mut protection: Vec<Option<bool>> = df.map(|m| bo(m.get("realtime"))).into_iter().collect();
    protection.extend(ar(defense.get("av")).iter().map(|a| bo(ob(Some(a)).get("enabled"))));
    let protection_known = protection.contains(&Some(true)) || (!protection.is_empty() && protection.iter().all(|v| *v == Some(false)));
    let protection_unknown = windows && !protection_known;
    if !js::truthy(ns.get("defense")) || def_errs.len() == def_parts.len() || (!def_errs.is_empty() && def_f.is_empty()) || protection_unknown {
        let mut list = def_errs.iter().map(|p| format!("{p} {}", err(p).unwrap_or_default())).collect::<Vec<_>>().join(" / ");
        if list.is_empty() && protection_unknown {
            list = "リアルタイム保護の状態が不明".into();
        }
        add("sec-defense", "防御", "unknown", format!("取れない: {}", if list.is_empty() { "調査の結果が無い".to_string() } else { list }));
    } else {
        let s = worst(&def_f);
        let tail = if def_errs.is_empty() { String::new() } else { format!("（取れない: {}）", def_errs.join(", ")) };
        let detail = if s == "ok" {
            format!("{}{tail}", defense_summary(ns))
        } else {
            format!("{}（{}）{tail}", def_f.iter().map(|f| st(f.get("title"))).collect::<Vec<_>>().join(" / "), defense_summary(ns))
        };
        add("sec-defense", "防御", s, detail);
    }

    // 待ち受け
    let listen = ar(ns.get("listen"));
    let ext: Vec<&Value> = listen.iter().filter(|l| is_ext(l)).collect();
    let neu: Vec<&Value> = ar(ns.get("listen_new")).iter().collect();
    if let Some(le) = err("listen") {
        add("sec-listen", "待ち受け", "unknown", format!("取れない: {le}"));
    } else {
        let tcp_new = neu.iter().filter(|l| !js::is_str(ob(Some(l)).get("proto"), "udp")).count();
        let ext_tcp = ext.iter().filter(|l| js::is_str(ob(Some(l)).get("proto"), "tcp")).count();
        let detail = if !neu.is_empty() {
            format!(
                "新しく外から届く: {}{}",
                neu.iter().take(4).map(|l| listen_label(l)).collect::<Vec<_>>().join("、"),
                if neu.len() > 4 { " ほか" } else { "" }
            )
        } else {
            format!(
                "外から届く {} 件（TCP {ext_tcp}・UDP {}）、この機体の中だけ {} 件{}",
                ext.len(),
                ext.len() - ext_tcp,
                listen.len() - ext.len(),
                if bo(ns.get("listen_base")) == Some(true) { "（比べる前回が無い）" } else { "" }
            )
        };
        add("sec-listen", "待ち受け", if tcp_new > 0 && !blocks_all(ns) { "warn" } else { "ok" }, detail);
    }

    // 常駐の増減（tune-core が DB と比べた結果があるときだけ）
    if matches!(ns.get("persist_changes"), Some(Value::Array(_))) || matches!(ns.get("persist_baseline"), Some(Value::Array(_))) {
        let (kinds, failed) = persist_kinds(&Value::Object(ns.clone()));
        let ch: Vec<&Map<String, Value>> = ar(ns.get("persist_changes")).iter().map(|x| ob(Some(x))).collect();
        let added: Vec<&&Map<String, Value>> = ch.iter().filter(|c| js::is_str(c.get("change"), "added")).collect();
        let removed = ch.iter().filter(|c| js::is_str(c.get("change"), "removed")).count();
        let base: Vec<&str> = ar(ns.get("persist_baseline")).iter().filter_map(Value::as_str).collect();
        let count = nm(ns.get("persist_count"));
        if failed.len() == kinds.len() {
            let list = failed.iter().map(|k| format!("{k} {}", err(kind_part(k)).unwrap_or_else(|| "null".into()))).collect::<Vec<_>>().join(" / ");
            add("sec-persist", "常駐の増減", "unknown", format!("取れない: {list}"));
        } else if !added.is_empty() {
            add(
                "sec-persist",
                "常駐の増減",
                "warn",
                format!(
                    "{} 件増えた: {}{}",
                    added.len(),
                    added.iter().take(4).map(|c| st(c.get("key"))).collect::<Vec<_>>().join("、"),
                    if added.len() > 4 { " ほか" } else { "" }
                ),
            );
        } else {
            let base_s = if base.is_empty() {
                "、前回から増えていない".to_string()
            } else {
                format!("（初回の記録: {}。増減は次回から）", base.iter().map(|k| kind_label(k).unwrap_or(k)).collect::<Vec<_>>().join("・"))
            };
            add(
                "sec-persist",
                "常駐の増減",
                "ok",
                format!(
                    "{} 件{}{base_s}{}",
                    count.map_or_else(|| "-".into(), ns_str),
                    if removed > 0 { format!("、{removed} 件減った") } else { String::new() },
                    if failed.is_empty() { String::new() } else { format!("（取れない: {}）", failed.join(", ")) }
                ),
            );
        }
    }

    // ログイン
    if logins_enabled(node) {
        let lg: Vec<&Map<String, Value>> = fs.iter().copied().filter(|f| st(f.get("id")).starts_with("log-login-")).collect();
        let lg_bad: Vec<&Map<String, Value>> =
            lg.iter().copied().filter(|f| js::is_str(f.get("severity"), "critical") || js::is_str(f.get("severity"), "warn")).collect();
        let src = if windows { "win_security" } else { "mac_auth" };
        let node_id = ob(Some(node)).get("id");
        let cur =
            ar(ob(Some(ctx)).get("cursors")).iter().map(|c| ob(Some(c))).find(|c| js::strict_eq(c.get("node_id"), node_id) && js::is_str(c.get("source"), src));
        if !lg_bad.is_empty() {
            add("sec-login", "ログイン", worst(&lg_bad), lg_bad.iter().map(|f| st(f.get("title"))).collect::<Vec<_>>().join(" / "));
        } else if windows && js::is_str(ob(ns.get("defense")).get("security_log"), "no-permission") {
            add("sec-login", "ログイン", "unknown", "セキュリティログを読む権限が無い（管理者か Event Log Readers のグループが要る）".into());
        } else if cur.is_none() {
            add("sec-login", "ログイン", "unknown", "まだ取り込んでいない".into());
        } else if let Some(le) = cur.map(|c| st(c.get("last_error"))).filter(|s| !s.is_empty()) {
            add("sec-login", "ログイン", "unknown", format!("取り込めない: {}", cut(le, 80)));
        } else {
            let detail = if lg.is_empty() {
                format!("24時間のログイン失敗は {LOGIN_WARN} 件未満、外部のアドレスからの成功なし")
            } else {
                lg.iter().map(|f| st(f.get("title"))).collect::<Vec<_>>().join(" / ")
            };
            add("sec-login", "ログイン", "ok", detail);
        }
    }

    // 初めての接続先（tune-core が覚えた結果があるときだけ）
    if peers_enabled(node)
        && let Some(Value::Object(pe)) = ns.get("peers")
    {
        let known = nm(pe.get("known"));
        let known_s = known.map_or_else(|| "-".into(), ns_str);
        if let Some(le) = err("listen") {
            add("sec-peers", "初めての接続先", "unknown", format!("取れない: {le}"));
        } else if bo(pe.get("learning")) == Some(true) {
            let left = match (nm(pe.get("until")), nm(ns.get("at"))) {
                (Some(u), Some(a)) => Some(((u - a) / DAY).ceil().max(0.0)),
                _ => None,
            };
            add(
                "sec-peers",
                "初めての接続先",
                "ok",
                format!("覚えている途中（{}{known_s} 件）", left.map(|l| format!("あと {} 日、", ns_str(l))).unwrap_or_default()),
            );
        } else {
            let nw: Vec<&Map<String, Value>> = ar(pe.get("new")).iter().map(|x| ob(Some(x))).collect();
            let proc_new: Vec<&&Map<String, Value>> = nw.iter().filter(|p| js::is_str(p.get("why"), "proc")).collect();
            let odd: Vec<&&Map<String, Value>> = nw.iter().filter(|p| odd_peer(p)).collect();
            let detail = if nw.is_empty() {
                format!("新しい接続先なし（覚えている {known_s} 件）")
            } else {
                format!(
                    "初めて {} 件{}{}{}",
                    nw.len(),
                    if proc_new.is_empty() {
                        String::new()
                    } else {
                        format!("（外と初めて通信したプロセス: {}）", proc_new.iter().take(4).map(|p| or_q(st(p.get("proc")))).collect::<Vec<_>>().join("、"))
                    },
                    if odd.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "（外部へ 80・443 以外の番号: {}）",
                            odd.iter().take(4).map(|p| format!("{} {}", or_q(st(p.get("proc"))), port_str(p.get("port")))).collect::<Vec<_>>().join("、")
                        )
                    },
                    if shared { "（共用機のため提案だけ）" } else { "" }
                )
            };
            add("sec-peers", "初めての接続先", if !odd.is_empty() && !shared { "warn" } else { "ok" }, detail);
        }
    }
    out
}

// ---- 取り込み（DB を使う。tune-core だけ） ----

/// 分析の結果（collect の `{ node_id, ok, data, at }`）の data.netsec を、正規化・前回との比較・常駐の増減・初めての接続先を足した形に置き換える。
/// 台帳で `"network": false` の機体からは netsec を受け取らない（残っていても捨てる）。DB に書けなかったときは errors.store に残す
pub fn ingest(db: &Store, node: Option<&Node>, r: &Value, prev_data: Option<&Value>) -> Value {
    let mut out = r.clone();
    if !js::truthy(r.get("ok")) {
        return out;
    }
    let Some(data) = out.get_mut("data").and_then(Value::as_object_mut) else { return out };
    let Some(raw) = data.remove("netsec") else { return out };
    if node.is_some_and(|n| !enabled(&n.raw)) {
        return out;
    }
    let node_id = js::string(r.get("node_id"));
    let probe = st(data.get("probe")).to_string();
    let at = nm(r.get("at")).map_or_else(now_ms, |x| x as i64);
    let mut ns = annotate(&normalize(&raw, &probe), prev_data.and_then(|d| d.get("netsec")), Some(&Value::from(at)));
    let mut store_err: Vec<String> = Vec::new();

    // 常駐の増減
    let (kinds, failed) = persist_kinds(&ns);
    let kinds: Vec<String> = kinds.iter().map(|s| s.to_string()).collect();
    let failed: Vec<String> = failed.iter().map(|s| s.to_string()).collect();
    let items = ar(ns.get("persist")).to_vec();
    match db.net_persist_step(&node_id, &items, &kinds, &failed, at) {
        Ok(d) => {
            let ch: Vec<Value> = ar(d.get("changes")).iter().take(50).cloned().collect();
            ns["persist_changes"] = Value::Array(ch);
            ns["persist_baseline"] = d.get("baseline").cloned().unwrap_or(json!([]));
        }
        Err(e) => store_err.push(format!("常駐の記録: {e}")),
    }

    // 初めての接続先（取れなかったとき・共用機で宛先を落としたときは覚えない）
    if !matches!(ob(ns.get("errors")).get("listen"), Some(Value::String(_))) && ns.get("outbound_skipped").is_none() {
        let sample = ar(ns.get("outbound")).to_vec();
        match db.net_peers_step(&node_id, &sample, at) {
            Ok(p) => ns["peers"] = p,
            Err(e) => store_err.push(format!("接続先の記録: {e}")),
        }
    }
    if !store_err.is_empty()
        && let Some(Value::Object(e)) = ns.get_mut("errors")
    {
        e.insert("store".into(), Value::from(cut(&store_err.join(" / "), LIM_ERR)));
    }
    data.insert("netsec".into(), strip(&ns));
    out
}

impl Store {
    /// 常駐の増減を決めて記録する（初回の種類は記録だけ）。戻り値は diff_persist の結果
    pub fn net_persist_step(&self, node_id: &str, items: &[Value], kinds: &[String], failed: &[String], at: i64) -> rusqlite::Result<Value> {
        let prev = self.net_items(node_id)?;
        let baselined = self.net_baselined(node_id, "persist:")?;
        let d = diff_persist(&prev, items, kinds, failed, &baselined);
        let ok_kinds: Vec<String> = kinds.iter().filter(|k| !failed.contains(k)).cloned().collect();
        self.net_apply_persist(node_id, items, &d, &ok_kinds, at)?;
        Ok(d)
    }

    /// 今回の標本を覚え、初めての接続先を返す。`{ learning, since, until, sampled, known, new }`
    pub fn net_peers_step(&self, node_id: &str, sample: &[Value], at: i64) -> rusqlite::Result<Value> {
        self.net_prune_peers(at)?;
        let since = match self.net_baseline_since(node_id, "peers")? {
            Some(s) => s,
            None => {
                self.net_set_baseline(node_id, "peers", at)?;
                at
            }
        };
        let until = since + LEARN_DAYS * 86_400_000;
        let learning = at < until;
        let known = self.net_peers_known(node_id)?;
        let new = classify_peers(&known, sample, learning);
        self.net_upsert_peers(node_id, sample, at)?;
        let known_n = self.net_peers_count(node_id)?;
        Ok(json!({ "learning": learning, "since": since, "until": until, "sampled": sample.len(), "known": known_n, "new": new }))
    }
}

impl Engine {
    /// 「セキュリティ」の画面（Tauri 版だけ）。機体ごとの最新の netsec と状態、常駐の増減、初めての接続先、ログインの送り元
    pub fn netsec_view(&self, _filter: &Value) -> Result<Value, String> {
        let cfg = self.config();
        let now = now_ms();
        let (snaps, checks, events, peers, logins) = self.with_db(|d| {
            let mut snaps = Vec::new();
            for n in &cfg.nodes {
                snaps.push(d.last_snapshots(&n.id, 1)?.into_iter().next());
            }
            Ok((snaps, d.checks()?, d.net_events(300)?, d.net_peers_recent(now - LEARN_DAYS * 86_400_000, 300)?, d.net_login_counts(now)?))
        })?;
        let enabled_nodes: HashSet<String> = cfg.nodes.iter().filter(|n| enabled(&n.raw)).map(|n| n.id.clone()).collect();
        let peer_nodes: HashSet<String> = cfg.nodes.iter().filter(|n| peers_enabled(&n.raw)).map(|n| n.id.clone()).collect();
        let login_nodes: HashSet<String> = cfg.nodes.iter().filter(|n| logins_enabled(&n.raw)).map(|n| n.id.clone()).collect();
        let events = rows_for_nodes(events, &enabled_nodes);
        let peers = rows_for_nodes(peers, &peer_nodes);
        let logins = rows_for_nodes(logins, &login_nodes);
        let nodes: Vec<Value> = cfg
            .nodes
            .iter()
            .zip(snaps)
            .map(|(n, s)| {
                let ns = s.as_ref().and_then(|s| s.data.get("netsec")).cloned().unwrap_or(Value::Null);
                let ch: Vec<Value> = checks.iter().filter(|c| js::is_str(c.get("scope"), &n.id) && st(c.get("id")).starts_with("sec-")).cloned().collect();
                let (ns, ch) = view_node_policy(ns, ch, enabled(&n.raw), peers_enabled(&n.raw), logins_enabled(&n.raw));
                json!({ "id": n.id, "os": n.os, "shared": n.shared, "enabled": enabled(&n.raw), "at": s.map(|s| s.at), "netsec": ns, "checks": ch })
            })
            .collect();
        let logins: Vec<Value> = logins
            .into_iter()
            .map(|mut l| {
                let p = is_public(l.get("provider"));
                l["public"] = Value::Bool(p);
                l
            })
            .collect();
        Ok(json!({
            "nodes": nodes, "events": events, "peers": peers, "logins": logins,
            "learn_days": LEARN_DAYS, "keep_days": PEER_KEEP_DAYS, "login_warn": LOGIN_WARN, "now": now,
        }))
    }
}

fn rows_for_nodes(rows: Vec<Value>, allowed: &HashSet<String>) -> Vec<Value> {
    rows.into_iter().filter(|r| r.get("node_id").and_then(Value::as_str).is_some_and(|id| allowed.contains(id))).collect()
}

fn view_node_policy(mut ns: Value, checks: Vec<Value>, enabled: bool, keep_peers: bool, keep_logins: bool) -> (Value, Vec<Value>) {
    if !enabled {
        return (Value::Null, Vec::new());
    }
    if let Some(o) = ns.as_object_mut() {
        o.remove("outbound");
    }
    if let Some(peers) = ns.get_mut("peers") {
        sanitize_peer_summary(peers);
    }
    if !keep_peers && let Some(o) = ns.as_object_mut() {
        o.remove("peers");
        o.remove("outbound_count");
    }
    let checks = checks
        .into_iter()
        .filter(|c| (keep_peers || !js::is_str(c.get("id"), "sec-peers")) && (keep_logins || !js::is_str(c.get("id"), "sec-login")))
        .collect();
    (ns, checks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_nodes_drop_peers_by_default() {
        assert!(peers_enabled(&json!({ "id": "a" })));
        assert!(!peers_enabled(&json!({ "id": "s", "shared": true })));
        assert!(peers_enabled(&json!({ "id": "s", "shared": true, "network_peers": true })));
        assert!(!peers_enabled(&json!({ "id": "a", "network_peers": false })));
        assert!(!peers_enabled(&json!({ "id": "a", "network": false, "network_peers": true })));
        let mut data = json!({ "netsec": { "outbound": [{ "proc": "x", "addr": "203.0.113.5", "port": 8443, "n": 1 }], "listen": [] } });
        drop_peers(&mut data);
        assert_eq!(data["netsec"]["outbound"], json!([]));
        assert_eq!(normalize(&data["netsec"], "mac")["outbound_skipped"], json!("shared"));
        assert!(normalize(&json!({ "outbound": [] }), "mac").get("outbound_skipped").is_none());
    }

    #[test]
    fn view_rows_follow_the_current_node_policy() {
        let rows = vec![json!({ "node_id": "keep", "addr": "192.0.2.1" }), json!({ "node_id": "drop", "addr": "192.0.2.2" })];
        let allowed = std::collections::HashSet::from(["keep".to_string()]);
        assert_eq!(rows_for_nodes(rows, &allowed), vec![json!({ "node_id": "keep", "addr": "192.0.2.1" })]);
    }

    #[test]
    fn view_removes_stored_peer_and_login_data_after_opt_out() {
        let ns = json!({
            "outbound": [{ "addr": "192.0.2.8", "proc": "app", "port": 443 }],
            "peers": { "new": [{ "addr": "192.0.2.9" }] },
            "listen": []
        });
        let checks =
            vec![json!({ "id": "sec-listen" }), json!({ "id": "sec-peers", "detail": "old peer" }), json!({ "id": "sec-login", "detail": "old login" })];
        let (ns, checks) = view_node_policy(ns, checks, true, false, false);
        assert!(ns.get("peers").is_none());
        assert!(ns.get("outbound").is_none());
        assert_eq!(checks, vec![json!({ "id": "sec-listen" })]);
        assert!(!serde_json::to_string(&ns).unwrap().contains("192.0.2.9"));

        let (ns, checks) = view_node_policy(json!({ "listen": [{ "port": 22 }] }), vec![json!({ "id": "sec-listen" })], false, true, true);
        assert!(ns.is_null());
        assert!(checks.is_empty());
    }

    #[test]
    fn snapshots_never_keep_or_return_peer_addresses() {
        let ns = json!({
            "outbound": [{ "addr": "192.0.2.8", "proc": "app", "port": 443 }],
            "peers": { "learning": false, "new": [{ "addr": "192.0.2.9", "proc": "app", "port": 443, "why": "dest", "dests": 1, "public": true }] }
        });
        let stored = strip(&ns);
        assert!(!serde_json::to_string(&stored).unwrap().contains("192.0.2."));
        assert_eq!(stored["peers"]["new"][0]["proc"], "app");

        let legacy = json!({ "netsec": ns });
        let allowed = snapshot_for_node(legacy.clone(), &json!({ "id": "a" }));
        assert!(!serde_json::to_string(&allowed).unwrap().contains("192.0.2."));
        let opted_out = snapshot_for_node(legacy, &json!({ "id": "a", "network_peers": false }));
        assert!(opted_out["netsec"].get("peers").is_none());

        let disabled = snapshot_for_node(
            json!({ "cpu_busy": 1, "netsec": { "listen": [], "peers": { "new": [] } } }),
            &json!({
                "id": "a", "network": false, "network_peers": true
            }),
        );
        assert!(disabled.get("netsec").is_none());
        assert_eq!(disabled["cpu_busy"], 1);
    }

    #[test]
    fn login_findings_follow_the_current_node_policy() {
        let findings = vec![json!({ "id": "log-login-fail" }), json!({ "id": "log-disk" })];
        assert_eq!(filter_log_findings(findings.clone(), &json!({ "id": "a", "network_logins": false })), vec![json!({ "id": "log-disk" })]);
        assert_eq!(filter_log_findings(findings.clone(), &json!({ "id": "s", "shared": true })), vec![json!({ "id": "log-disk" })]);
        assert_eq!(filter_log_findings(findings.clone(), &json!({ "id": "s", "shared": true, "network_logins": true })), findings);
    }

    #[test]
    fn log_api_rows_follow_the_current_node_policy() {
        let nodes = vec![json!({ "id": "private", "network_logins": false }), json!({ "id": "keep", "network_logins": true })];
        let rows = vec![
            json!({ "node_id": "private", "source": "win_security", "provider": "192.0.2.1" }),
            json!({ "node_id": "private", "source": "win_system", "provider": "disk" }),
            json!({ "node_id": "keep", "source": "win_security", "provider": "198.51.100.1" }),
        ];
        assert_eq!(filter_log_rows(rows.clone(), &nodes), rows[1..].to_vec());
        let signatures = vec![
            json!({ "source": "win_security", "node_ids": "private", "sample": "login 192.0.2.1" }),
            json!({ "source": "win_security", "node_ids": "keep", "sample": "login 198.51.100.1" }),
            json!({ "source": "win_system", "node_ids": "private", "sample": "disk" }),
        ];
        let got: Vec<Value> = filter_log_signatures(signatures, &nodes).into_iter().map(|v| v["sample"].clone()).collect();
        assert_eq!(got, vec![json!("login 198.51.100.1"), json!("disk")]);
    }

    #[test]
    fn removed_nodes_do_not_expose_security_history() {
        let rows = vec![
            json!({ "scope": "removed", "id": "sec-login", "detail": "192.0.2.1" }),
            json!({ "scope": "removed", "check_id": "sec-peers", "detail": "192.0.2.2" }),
            json!({ "scope": "_app", "id": "sec-login", "detail": "192.0.2.3" }),
            json!({ "scope": "removed", "id": "memory" }),
            json!({ "scope": "_app", "id": "database" }),
            json!({ "scope": "keep", "id": "sec-listen" }),
        ];
        assert_eq!(filter_check_rows(rows.clone(), &[json!({ "id": "keep" })]), rows[3..].to_vec());
    }

    #[test]
    fn addresses_are_classified() {
        let c = |s: &str| addr_info(Some(&Value::from(s))).map(|i| (i.addr, i.scope));
        assert_eq!(c("*"), Some(("*".into(), "any")));
        assert_eq!(c("127.0.0.1"), Some(("127.0.0.1".into(), "loopback")));
        assert_eq!(c("::ffff:198.51.100.7"), Some(("198.51.100.7".into(), "public")));
        // CGNAT（tailnet）の範囲。点検（oss-check）が本物のアドレスと見分けられないので、実行時に組み立てる
        let cg = format!("{}.64.0.1", 100);
        assert_eq!(c(&cg), Some((cg.clone(), "private")));
        assert_eq!(c("[fe80::1%en0]"), Some(("fe80::1".into(), "link")));
        assert_eq!(c("2001:DB8::5"), Some(("2001:db8::5".into(), "public")));
        assert_eq!(c("fd7a::1"), Some(("fd7a::1".into(), "private")));
        assert_eq!(c(""), None);
    }

    #[test]
    fn new_external_listener_is_found_against_the_previous_analysis() {
        let raw = json!({ "listen": [
            { "proto": "tcp", "addr": "*", "port": 3000, "proc": "node" },
            { "proto": "tcp", "addr": "127.0.0.1", "port": 5432, "proc": "postgres" },
            { "proto": "tcp", "addr": "0.0.0.0", "port": 51234, "proc": "app" }
        ] });
        let prev = annotate(&normalize(&json!({ "listen": [{ "proto": "tcp", "addr": "*", "port": 52000, "proc": "app" }] }), "mac"), None, None);
        let cur = annotate(&normalize(&raw, "mac"), Some(&prev), Some(&json!(1)));
        let new: Vec<String> = ar(cur.get("listen_new")).iter().map(listen_key).collect();
        // 大きい番号はプロセスごとに1つ（app の番号が変わっても新しいとは数えない）。localhost だけのものは数えない
        assert_eq!(new, vec!["tcp|node|3000"]);
        let f = findings(Some(&cur), &json!({}));
        assert_eq!(f[0]["severity"], "warn");
    }
}
