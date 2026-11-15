//! レビュー画面の中身。task-completion.v1（完了台帳）の申告を一時表示するだけで、完了・受入は判定しない。
//! renderer/review-model.js と同じ制限・同じ文言で検証する（外部入力はこの境界で検証し、描画側は検証済みの値だけを受ける）。
//! 外部参照の取得・保存・承認操作は行わない。GPUI に依存しないので、画面なしでテストできる。

use std::collections::HashSet;

use serde_json::{Map, Value};

pub const GATES: [&str; 5] = ["implementation", "verification", "integration", "deployment", "acceptance"];
pub const MAX_FILE: usize = 2 * 1024 * 1024;
const MAX_COVERAGE_ITEMS: usize = 10_000;
const MAX_STRING: usize = 8192;
const MAX_DEPTH: usize = 16;

pub fn gate_label(key: &str) -> &'static str {
    match key {
        "implementation" => "実装",
        "verification" => "検証",
        "integration" => "正式反映",
        "deployment" => "配布",
        _ => "受入",
    }
}

pub fn delivery_label(d: &str) -> &str {
    match d {
        "local" => "この環境",
        "repository" => "正式ブランチ",
        "environment" => "対象環境",
        other => other,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Evidence {
    pub reference: Option<String>,
    pub result: Option<String>,
    pub observed_at: Option<String>,
    pub revision: Option<String>,
    pub environment: Option<String>,
    pub link: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Gate {
    pub status_label: Option<String>,
    pub evidence: Vec<Evidence>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReviewBlock {
    pub accepted_label: Option<String>,
    pub reference: Option<String>,
    pub observed_at: Option<String>,
    pub revision: Option<String>,
    pub target: Option<String>,
    pub link: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub id: String,
    pub goal: Option<String>,
    pub target: Option<String>,
    pub revision: Option<String>,
    pub delivery: Option<String>,
    pub gates: Vec<(&'static str, Gate)>,
    pub review: ReviewBlock,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Coverage {
    pub declared: bool,
    pub expected_declared: bool,
    pub observed_declared: bool,
    pub unobserved_declared: bool,
    pub expected: Vec<String>,
    pub observed: Vec<String>,
    pub unobserved: Vec<String>,
    pub missing: Vec<String>,
    pub extra: Vec<String>,
    pub overlap: Vec<String>,
    pub unobserved_outside_expected: Vec<String>,
    pub duplicates: Vec<String>,
    pub insufficient: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    pub tasks: Vec<Task>,
    pub coverage: Coverage,
}

type R<T> = Result<T, String>;

fn text(v: Option<&Value>, name: &str) -> R<Option<String>> {
    match v {
        None => Ok(None),
        Some(Value::String(s)) => Ok((!s.is_empty()).then(|| s.clone())),
        Some(_) => Err(format!("{name} は文字列で指定してください")),
    }
}

fn status_label(v: Option<&Value>) -> R<Option<String>> {
    match v {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(match s.as_str() {
            "pending" => "未着手の申告".to_string(),
            "in_progress" => "進行中の申告".to_string(),
            "blocked" => "停止中の申告".to_string(),
            "unverified" => "未照合の申告".to_string(),
            "pass" => "成功の申告（未照合）".to_string(),
            "fail" => "失敗の申告（未照合）".to_string(),
            other => format!("未対応の申告値: {other}"),
        })),
        Some(_) => Err("gate status は文字列で指定してください".into()),
    }
}

fn check_strings_and_depth(v: &Value, depth: usize) -> R<()> {
    match v {
        Value::String(s) => {
            if s.chars().count() > MAX_STRING {
                return Err("文字列は8192文字までです".into());
            }
            Ok(())
        }
        Value::Array(a) => {
            if depth > MAX_DEPTH {
                return Err("ネストは16段までです".into());
            }
            a.iter().try_for_each(|x| check_strings_and_depth(x, depth + 1))
        }
        Value::Object(o) => {
            if depth > MAX_DEPTH {
                return Err("ネストは16段までです".into());
            }
            for (k, x) in o {
                if k.chars().count() > MAX_STRING {
                    return Err("文字列は8192文字までです".into());
                }
                check_strings_and_depth(x, depth + 1)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// 1970-01-01 からの日数（グレゴリオ暦）
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// `YYYY-MM-DDTHH:MM:SS(.f{1,9})?(Z|±HH:MM)`。形式か範囲が不正なら None。Unix 時刻のミリ秒
pub fn iso_time(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |a: usize, n: usize| -> Option<i64> {
        let part = b.get(a..a + n)?;
        part.iter().all(u8::is_ascii_digit).then(|| part.iter().fold(0i64, |acc, d| acc * 10 + i64::from(d - b'0')))
    };
    let (y, m, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') || b.get(10) != Some(&b'T') || b.get(13) != Some(&b':') || b.get(16) != Some(&b':') {
        return None;
    }
    let (hh, mm, ss) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    let mut i = 19;
    let mut millis = 0i64;
    if b.get(i) == Some(&b'.') {
        let start = i + 1;
        let mut end = start;
        while b.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        let n = end - start;
        if !(1..=9).contains(&n) {
            return None;
        }
        let frac = &s[start..end.min(start + 3)];
        millis = frac.parse::<i64>().ok()? * 10i64.pow(3 - frac.len() as u32);
        i = end;
    }
    let offset_min = match b.get(i..) {
        Some(b"Z") => 0,
        Some(z) if z.len() == 6 && (z[0] == b'+' || z[0] == b'-') && z[3] == b':' => {
            let zs = std::str::from_utf8(z).ok()?;
            let (zh, zm) = (zs[1..3].parse::<i64>().ok()?, zs[4..6].parse::<i64>().ok()?);
            if zh > 23 || zm > 59 || !zs[1..3].bytes().all(|c| c.is_ascii_digit()) || !zs[4..6].bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            (if z[0] == b'-' { -1 } else { 1 }) * (zh * 60 + zm)
        }
        _ => return None,
    };
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) || hh > 23 || mm > 59 || ss > 59 {
        return None;
    }
    Some(((days_from_civil(y, m, d) * 86_400 + hh * 3600 + mm * 60 + ss - offset_min * 60) * 1000) + millis)
}

/// `https://github.com/<owner>/<repo>/(pull|issues)/<n>` だけをリンクとして認める（表示のみで開かない）
pub fn github_url(s: &str) -> Option<String> {
    if s.chars().any(|c| c == '%' || c == '?' || c == '#' || c.is_control()) {
        return None;
    }
    let rest = s.strip_prefix("https://github.com/")?;
    let parts: Vec<&str> = rest.split('/').collect();
    let ok_name = |p: &str| !p.is_empty() && p.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'));
    let ok = parts.len() == 4
        && ok_name(parts[0])
        && ok_name(parts[1])
        && (parts[2] == "pull" || parts[2] == "issues")
        && !parts[3].starts_with('0')
        && !parts[3].is_empty()
        && parts[3].bytes().all(|c| c.is_ascii_digit());
    ok.then(|| s.to_string())
}

fn evidence(v: Option<&Value>, task: &Task, now_ms: i64, evidence_total: &mut usize, gate: &str) -> R<Vec<Evidence>> {
    let Some(v) = v else { return Ok(Vec::new()) };
    let Value::Array(items) = v else { return Err("evidence は配列で指定してください".into()) };
    if items.len() > 100 {
        return Err("gate ごとの evidence は100件までです".into());
    }
    *evidence_total += items.len();
    if *evidence_total > 4000 {
        return Err("evidence は文書全体で4000件までです".into());
    }
    items
        .iter()
        .map(|item| {
            let Value::Object(o) = item else { return Err("evidence の項目は object で指定してください".to_string()) };
            let reference = text(o.get("ref"), "evidence ref")?;
            let result = text(o.get("result"), "evidence result")?;
            let observed_at = text(o.get("observed_at"), "evidence observed_at")?;
            let revision = text(o.get("revision"), "evidence revision")?;
            let environment = text(o.get("environment"), "evidence environment")?;
            let mut warnings = Vec::new();
            if let Some(at) = &observed_at {
                match iso_time(at) {
                    None => warnings.push("日時が不正です".to_string()),
                    Some(t) if t > now_ms => warnings.push("未来の日時です".to_string()),
                    Some(_) => {}
                }
            }
            if let (Some(tr), Some(er)) = (&task.revision, &revision)
                && tr != er
            {
                warnings.push(format!("revision が対象と異なります: {er}"));
            }
            if (gate == "deployment" || gate == "acceptance")
                && let (Some(tt), Some(e)) = (&task.target, &environment)
                && tt != e
            {
                warnings.push(format!("対象環境が対象と異なります: {e}"));
            }
            let link = reference.as_deref().and_then(github_url);
            Ok(Evidence { reference, result, observed_at, revision, environment, link, warnings })
        })
        .collect()
}

fn review(v: Option<&Value>, task: &Task, now_ms: i64) -> R<ReviewBlock> {
    let empty = ReviewBlock { accepted_label: None, reference: None, observed_at: None, revision: None, target: None, link: None, warnings: Vec::new() };
    let Some(v) = v else { return Ok(empty) };
    let Value::Object(o) = v else { return Err("review status は文字列で指定してください".into()) };
    if o.get("status").is_some_and(|s| !s.is_string()) {
        return Err("review status は文字列で指定してください".into());
    }
    let status = o.get("status").and_then(Value::as_str).filter(|s| !s.is_empty());
    let accepted_label = status.map(|s| match s {
        "accepted" => "受入の申告（未照合）".to_string(),
        "pending" => "受入待ちの申告".to_string(),
        other => format!("未対応の申告値: {other}"),
    });
    let reference = text(o.get("ref"), "review ref")?;
    let observed_at = text(o.get("observed_at"), "review observed_at")?;
    let revision = text(o.get("revision"), "review revision")?;
    let target = text(o.get("target"), "review target")?;
    let mut warnings = Vec::new();
    if let Some(at) = &observed_at {
        match iso_time(at) {
            None => warnings.push("レビュー日時が不正です".to_string()),
            Some(t) if t > now_ms => warnings.push("レビュー日時が未来です".to_string()),
            Some(_) => {}
        }
    }
    if let (Some(tr), Some(r)) = (&task.revision, &revision)
        && tr != r
    {
        warnings.push(format!("review revision が対象と異なります: {r}"));
    }
    if let (Some(tt), Some(t)) = (&task.target, &target)
        && tt != t
    {
        warnings.push(format!("review target が対象と異なります: {t}"));
    }
    if status == Some("accepted") {
        for (label, f) in [("review ref", &reference), ("review observed_at", &observed_at), ("review revision", &revision), ("review target", &target)] {
            if f.is_none() {
                warnings.push(format!("受入の申告に {label} がありません"));
            }
        }
    }
    let link = reference.as_deref().and_then(github_url);
    Ok(ReviewBlock { accepted_label, reference, observed_at, revision, target, link, warnings })
}

fn string_list(v: Option<&Value>, name: &str) -> R<Vec<String>> {
    match v {
        None => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| match x {
                Value::String(s) if !s.is_empty() => Ok(s.clone()),
                _ => Err(format!("{name} は文字列の配列で指定してください")),
            })
            .collect(),
        Some(_) => Err(format!("{name} は文字列の配列で指定してください")),
    }
}

fn unique(list: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    list.iter().filter(|x| seen.insert((*x).clone())).cloned().collect()
}

fn coverage(v: Option<&Value>) -> R<Coverage> {
    let Some(v) = v else { return Ok(Coverage::default()) };
    let Value::Object(o) = v else { return Err("coverage は object で指定してください".into()) };
    let expected = string_list(o.get("expected"), "coverage expected")?;
    let observed = string_list(o.get("observed"), "coverage observed")?;
    let unobserved = string_list(o.get("unobserved"), "coverage unobserved")?;
    if expected.len() + observed.len() + unobserved.len() > MAX_COVERAGE_ITEMS {
        return Err("coverage は合計10000件までです".into());
    }
    let mut duplicates = Vec::new();
    for (name, list) in [("expected", &expected), ("observed", &observed), ("unobserved", &unobserved)] {
        let mut seen = HashSet::new();
        for item in list {
            if !seen.insert(item) {
                duplicates.push(format!("{name}: {item}"));
            }
        }
    }
    let mut insufficient = Vec::new();
    if expected.is_empty() {
        insufficient.push("想定範囲が未記載です".to_string());
    }
    if observed.is_empty() {
        insufficient.push("観測範囲が未記載です".to_string());
    }
    let (ex, ob, un): (HashSet<&String>, HashSet<&String>, HashSet<&String>) =
        (expected.iter().collect(), observed.iter().collect(), unobserved.iter().collect());
    let overlap = unique(&observed).into_iter().filter(|x| un.contains(x)).collect();
    let unobserved_outside_expected = unique(&unobserved).into_iter().filter(|x| !ex.contains(x)).collect();
    Ok(Coverage {
        declared: true,
        expected_declared: o.contains_key("expected"),
        observed_declared: o.contains_key("observed"),
        unobserved_declared: o.contains_key("unobserved"),
        missing: unique(&expected).into_iter().filter(|x| !ob.contains(x)).collect(),
        extra: unique(&observed).into_iter().filter(|x| !ex.contains(x)).collect(),
        overlap,
        unobserved_outside_expected,
        duplicates,
        insufficient,
        expected,
        observed,
        unobserved,
    })
}

fn is_object(v: &Value) -> Option<&Map<String, Value>> {
    v.as_object()
}

/// 入力を検証して表示用にする。形が合わなければ理由つきで断る（部分的には受け付けない）
pub fn parse(input: &Value, now_ms: i64) -> R<Document> {
    check_strings_and_depth(input, 0)?;
    let tasks_json = input
        .get("schema")
        .filter(|s| s.as_str() == Some("task-completion.v1"))
        .and_then(|_| input.get("tasks"))
        .and_then(Value::as_array)
        .ok_or("task-completion.v1 の tasks 配列が必要です")?;
    if tasks_json.is_empty() || tasks_json.len() > 1000 {
        return Err("tasks は1件から1000件までです".into());
    }
    let mut ids = HashSet::new();
    let mut evidence_total = 0usize;
    let mut tasks = Vec::new();
    for t in tasks_json {
        let o = is_object(t).ok_or("task は object で指定してください")?;
        let id = o
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter(|s| ids.insert((*s).to_string()))
            .ok_or("task id は空でない一意の文字列で指定してください")?
            .to_string();
        let mut view = Task {
            id,
            goal: text(o.get("goal"), "goal")?,
            target: text(o.get("target"), "target")?,
            revision: text(o.get("revision"), "revision")?,
            delivery: text(o.get("delivery"), "delivery")?,
            gates: Vec::new(),
            review: ReviewBlock { accepted_label: None, reference: None, observed_at: None, revision: None, target: None, link: None, warnings: Vec::new() },
        };
        let gates_json = match o.get("gates") {
            None => None,
            Some(Value::Object(g)) => Some(g),
            Some(_) => return Err("gates は object で指定してください".into()),
        };
        if let Some(d) = o.get("delivery")
            && !matches!(d.as_str(), Some("local" | "repository" | "environment"))
        {
            return Err("delivery は local、repository、environment のいずれかで指定してください".into());
        }
        for key in GATES {
            let value = match gates_json.and_then(|g| g.get(key)) {
                None => None,
                Some(Value::Object(g)) => Some(g),
                Some(_) => return Err(format!("{key} gate は object で指定してください")),
            };
            let items = evidence(value.and_then(|g| g.get("evidence")), &view, now_ms, &mut evidence_total, key)?;
            let mut warnings: Vec<String> = items.iter().flat_map(|i| i.warnings.clone()).collect();
            let status = value.and_then(|g| g.get("status"));
            if status.and_then(Value::as_str) == Some("pass") {
                if items.is_empty() {
                    warnings.push("成功の申告に証跡がありません".into());
                }
                for it in &items {
                    for (label, f) in [
                        ("ref", &it.reference),
                        ("result", &it.result),
                        ("observed_at", &it.observed_at),
                        ("revision", &it.revision),
                        ("environment", &it.environment),
                    ] {
                        if f.is_none() {
                            warnings.push(format!("成功の申告の証跡に {label} がありません"));
                        }
                    }
                    if let Some(r) = &it.result
                        && r != "pass"
                    {
                        warnings.push(format!("成功の申告の証跡の result が pass ではありません: {r}"));
                    }
                }
            }
            warnings = unique(&warnings);
            view.gates.push((key, Gate { status_label: status_label(status)?, evidence: items, warnings }));
        }
        let mut rv = review(o.get("review"), &view, now_ms)?;
        let accepted = o.get("review").and_then(|r| r.get("status")).and_then(Value::as_str) == Some("accepted");
        let acceptance = gates_json.and_then(|g| g.get("acceptance")).and_then(|a| a.get("status")).and_then(Value::as_str);
        if accepted && let Some(a @ ("fail" | "blocked")) = acceptance {
            rv.warnings.push(format!("受入の申告と acceptance gate の {a} が矛盾します"));
        }
        view.review = rv;
        tasks.push(view);
    }
    Ok(Document { tasks, coverage: coverage(input.get("coverage"))? })
}

/// 現在時刻（ミリ秒）
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub name: String,
    pub size: usize,
    pub sha256: String,
}

/// ファイルを読む（2 MiB まで・UTF-8 のみ）。内容は保存しない
pub fn load(path: &std::path::Path, now: i64) -> R<(Source, Document)> {
    use sha2::{Digest, Sha256};
    let meta = std::fs::metadata(path).map_err(|_| "ファイルサイズを確認できません。".to_string())?;
    if !meta.is_file() {
        return Err("ファイルを指定してください。".into());
    }
    if meta.len() as usize > MAX_FILE {
        return Err("ファイルは2 MiB以下にしてください。".into());
    }
    let bytes = std::fs::read(path).map_err(|_| "ファイルを安全に読み込めません。".to_string())?;
    if bytes.len() > MAX_FILE {
        return Err("ファイルを安全に読み込めません。".into());
    }
    let digest = Sha256::digest(&bytes);
    let sha256 = digest.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let source = String::from_utf8(bytes.clone()).map_err(|_| "入力を受け付けられません: UTF-8 として読めません".to_string())?;
    let value: Value = serde_json::from_str(&source).map_err(|_| "JSONの形式が不正です。".to_string())?;
    let doc = parse(&value, now).map_err(|e| format!("入力を受け付けられません: {e}"))?;
    Ok((Source { name: path.file_name().map_or("名前未取得".to_string(), |n| n.to_string_lossy().into_owned()), size: bytes.len(), sha256 }, doc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_700_000_000_000; // 2023-11-14T22:13:20Z

    fn doc(tasks: Value) -> Value {
        json!({ "schema": "task-completion.v1", "tasks": tasks })
    }

    #[test]
    fn iso_time_accepts_valid_and_rejects_invalid() {
        assert_eq!(iso_time("2023-11-14T22:13:20Z"), Some(NOW));
        assert_eq!(iso_time("2023-11-14T22:13:20.500Z"), Some(NOW + 500));
        assert_eq!(iso_time("2023-11-15T07:13:20+09:00"), Some(NOW));
        assert_eq!(iso_time("2023-11-14T12:43:20-09:30"), Some(NOW));
        for bad in [
            "2023-02-30T00:00:00Z",
            "2023-13-01T00:00:00Z",
            "2023-11-14T24:00:00Z",
            "2023-11-14T22:13:20",
            "2023-11-14 22:13:20Z",
            "2023-11-14T22:13:20+24:00",
            "2023-11-14T22:13:20.Z",
            "2023-11-14T22:13:20.1234567890Z",
            "x",
        ] {
            assert_eq!(iso_time(bad), None, "{bad}");
        }
        assert!(iso_time("2024-02-29T00:00:00Z").is_some());
        assert!(iso_time("2023-02-29T00:00:00Z").is_none());
    }

    #[test]
    fn github_urls_are_strictly_filtered() {
        assert!(github_url("https://github.com/o/r/pull/12").is_some());
        assert!(github_url("https://github.com/o/r/issues/1").is_some());
        for bad in [
            "http://github.com/o/r/pull/1",
            "https://github.com/o/r/pull/0",
            "https://github.com/o/r/pull/01",
            "https://github.com/o/r/pull/1/",
            "https://github.com/o/r/pull/1?x=1",
            "https://github.com/o/r/pull/1#a",
            "https://evil.com/o/r/pull/1",
            "https://github.com/o/r/blob/1",
            "https://github.com:443/o/r/pull/1",
            "https://github.com/o/r/pull/%31",
            "https://github.com/o/r/pull/1\n",
        ] {
            assert_eq!(github_url(bad), None, "{bad}");
        }
        // userinfo 付き。メールアドレスの形を文字列として書かないよう、実行時に組み立てる
        let with_userinfo = ["https://", "u:p", "@", "github.com/o/r/pull/1"].concat();
        assert_eq!(github_url(&with_userinfo), None);
    }

    #[test]
    fn rejects_malformed_documents_with_reasons() {
        assert!(parse(&json!({}), NOW).unwrap_err().contains("tasks 配列が必要"));
        assert!(parse(&json!({"schema": "x", "tasks": []}), NOW).is_err());
        assert!(parse(&doc(json!([])), NOW).unwrap_err().contains("1件から1000件"));
        assert!(parse(&doc(json!([{"id": ""}])), NOW).unwrap_err().contains("一意の文字列"));
        assert!(parse(&doc(json!([{"id": "a"}, {"id": " a "}])), NOW).unwrap_err().contains("一意の文字列"));
        assert!(parse(&doc(json!([{"id": "a", "delivery": "other"}])), NOW).unwrap_err().contains("local、repository、environment"));
        assert!(parse(&doc(json!([{"id": "a", "goal": 1}])), NOW).unwrap_err().contains("goal は文字列"));
        assert!(parse(&doc(json!([{"id": "a", "gates": []}])), NOW).unwrap_err().contains("gates は object"));
        assert!(parse(&doc(json!([{"id": "a", "gates": {"verification": 1}}])), NOW).unwrap_err().contains("verification gate は object"));
        assert!(parse(&doc(json!([{"id": "a", "gates": {"verification": {"evidence": {}}}}])), NOW).unwrap_err().contains("evidence は配列"));
        assert!(parse(&doc(json!([{"id": "a", "gates": {"verification": {"status": 1}}}])), NOW).unwrap_err().contains("gate status は文字列"));
        let long = "あ".repeat(MAX_STRING + 1);
        assert!(parse(&doc(json!([{"id": "a", "goal": long}])), NOW).unwrap_err().contains("8192"));
        let mut deep = json!("x");
        for _ in 0..20 {
            deep = json!([deep]);
        }
        assert!(parse(&doc(json!([{"id": "a", "x": deep}])), NOW).unwrap_err().contains("16段"));
        let many: Vec<Value> = (0..101).map(|_| json!({})).collect();
        assert!(parse(&doc(json!([{"id": "a", "gates": {"verification": {"evidence": many}}}])), NOW).unwrap_err().contains("100件"));
    }

    #[test]
    fn a_pass_claim_without_evidence_is_flagged_not_judged() {
        let d = parse(&doc(json!([{ "id": "t", "revision": "r1", "target": "prod", "delivery": "environment",
            "gates": { "verification": {"status": "pass"},
                       "deployment": {"status": "pass", "evidence": [{"ref": "https://github.com/o/r/pull/3", "result": "fail", "observed_at": "2099-01-01T00:00:00Z", "revision": "r2", "environment": "stg"}]} },
            "review": {"status": "accepted"} }])), NOW)
        .unwrap();
        let t = &d.tasks[0];
        let verification = &t.gates[1].1;
        assert_eq!(verification.status_label.as_deref(), Some("成功の申告（未照合）"));
        assert!(verification.warnings.contains(&"成功の申告に証跡がありません".to_string()));
        let deployment = &t.gates[3].1;
        for w in
            ["未来の日時です", "revision が対象と異なります: r2", "対象環境が対象と異なります: stg", "成功の申告の証跡の result が pass ではありません: fail"]
        {
            assert!(deployment.warnings.contains(&w.to_string()), "{w} / {:?}", deployment.warnings);
        }
        assert!(deployment.evidence[0].link.is_some());
        assert_eq!(t.review.accepted_label.as_deref(), Some("受入の申告（未照合）"));
        assert!(t.review.warnings.contains(&"受入の申告に review ref がありません".to_string()));
        // 未記載の gate は None のまま（判定しない）
        assert_eq!(t.gates[0].1.status_label, None);
    }

    #[test]
    fn accepted_review_contradicting_a_failed_acceptance_gate_is_flagged() {
        let d = parse(&doc(json!([{ "id": "t", "gates": {"acceptance": {"status": "fail"}}, "review": {"status": "accepted", "ref": "r", "observed_at": "2023-01-01T00:00:00Z", "revision": "x", "target": "y"} }])), NOW).unwrap();
        assert!(d.tasks[0].review.warnings.iter().any(|w| w.contains("矛盾します")));
        let d = parse(&doc(json!([{ "id": "t", "review": {"status": "weird"} }])), NOW).unwrap();
        assert_eq!(d.tasks[0].review.accepted_label.as_deref(), Some("未対応の申告値: weird"));
    }

    #[test]
    fn coverage_reports_gaps_overlaps_and_duplicates() {
        let mut v = doc(json!([{"id": "t"}]));
        v["coverage"] = json!({"expected": ["a", "b", "b"], "observed": ["a", "z"], "unobserved": ["a", "q"]});
        let c = parse(&v, NOW).unwrap().coverage;
        assert!(c.declared && c.expected_declared);
        assert_eq!(c.missing, ["b"]);
        assert_eq!(c.extra, ["z"]);
        assert_eq!(c.overlap, ["a"]);
        assert_eq!(c.unobserved_outside_expected, ["q"]);
        assert_eq!(c.duplicates, ["expected: b"]);
        assert!(c.insufficient.is_empty());
        let none = parse(&doc(json!([{"id": "t"}])), NOW).unwrap().coverage;
        assert!(!none.declared);
        let mut bad = doc(json!([{"id": "t"}]));
        bad["coverage"] = json!({"expected": [1]});
        assert!(parse(&bad, NOW).unwrap_err().contains("文字列の配列"));
        bad["coverage"] = json!([]);
        assert!(parse(&bad, NOW).unwrap_err().contains("coverage は object"));
    }

    #[test]
    fn loads_files_with_size_utf8_and_json_checks() {
        let dir = std::env::temp_dir().join(format!("kt-review-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("ok.json");
        std::fs::write(&good, doc(json!([{"id": "t", "goal": "g"}])).to_string()).unwrap();
        let (src, d) = load(&good, NOW).unwrap();
        assert_eq!((src.name.as_str(), src.sha256.len(), d.tasks.len()), ("ok.json", 64, 1));
        let big = dir.join("big.json");
        std::fs::write(&big, vec![b' '; MAX_FILE + 1]).unwrap();
        assert!(load(&big, NOW).unwrap_err().contains("2 MiB"));
        let bad_utf8 = dir.join("bad.json");
        std::fs::write(&bad_utf8, [0xff, 0xfe, 0x00]).unwrap();
        assert!(load(&bad_utf8, NOW).unwrap_err().contains("UTF-8"));
        let bad_json = dir.join("syntax.json");
        std::fs::write(&bad_json, "{").unwrap();
        assert!(load(&bad_json, NOW).unwrap_err().contains("JSONの形式が不正"));
        assert!(load(&dir.join("none.json"), NOW).is_err());
        assert!(load(&dir, NOW).is_err(), "ディレクトリは読まない");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
