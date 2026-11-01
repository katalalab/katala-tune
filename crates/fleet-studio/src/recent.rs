//! Privacy-preserving projection of saved AI session metadata. No collection runs here.
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use serde_json::Value;
use tune_core::{
    db::Store,
    nodes::{self, Node},
};

use crate::{Observation, observation};

const MAX_NODES: usize = 32;
const PER_NODE: usize = 20;
const MAX_ENTRIES: usize = 50;
const MAX_PRS: usize = 3;

#[derive(Clone, Debug)]
pub struct RecentTicket {
    active: Arc<AtomicBool>,
}

impl RecentTicket {
    pub fn is_current(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Recheck the generation on both sides of the caller's identity check.
    pub fn revalidate(&self, check: impl FnOnce() -> bool) -> bool {
        self.is_current() && check() && self.is_current()
    }
}

pub struct RecentGate {
    current: Arc<AtomicBool>,
}

impl Default for RecentGate {
    fn default() -> Self {
        Self { current: Arc::new(AtomicBool::new(true)) }
    }
}

impl RecentGate {
    pub fn ticket(&self) -> RecentTicket {
        RecentTicket { active: Arc::clone(&self.current) }
    }

    pub fn invalidate(&mut self) {
        self.current.store(false, Ordering::SeqCst);
        self.current = Arc::new(AtomicBool::new(true));
    }
}

impl Drop for RecentGate {
    fn drop(&mut self) {
        self.current.store(false, Ordering::SeqCst);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionState {
    Unknown,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug)]
pub struct SyncStatus {
    pub node: String,
    pub attempt_at: Option<i64>,
    pub last_ok_at: Option<i64>,
    pub failure: Option<bool>,
    pub pending: Option<bool>,
    pub file_errors: Option<u64>,
    pub session_error: bool,
}

impl SyncStatus {
    pub fn collection_state(&self) -> CollectionState {
        match (self.failure, self.attempt_at, self.last_ok_at) {
            (Some(true), _, _) => CollectionState::Failed,
            (Some(false), Some(at), Some(ok)) if at == ok => CollectionState::Succeeded,
            _ => CollectionState::Unknown,
        }
    }

    pub fn freshness(&self, now: i64, max_age_ms: i64) -> Observation {
        observation(self.last_ok_at, false, true, now, max_age_ms)
    }
}

/// Immutable evidence for the row and path observed together.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentDirectory {
    node: String,
    local: bool,
    cwd: String,
    path: PathBuf,
    url: String,
}

impl RecentDirectory {
    pub fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn path_for(&self, node: &str, local: bool, cwd: Option<&str>) -> Option<&Path> {
        (local && self.local == local && self.node == node && cwd == Some(self.cwd.as_str())).then_some(self.path.as_path())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentEntry {
    pub node: String,
    pub local: bool,
    pub tool: String,
    pub cwd: Option<String>,
    pub activity_at: Option<i64>,
    pub directory_url: Option<RecentDirectory>,
    pub prs: Vec<String>,
    pub pr_state: PrState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrState {
    Unknown,
    Empty,
    Complete,
    Partial,
}

#[derive(Clone, Debug, Default)]
pub struct RecentWork {
    pub nodes: Vec<SyncStatus>,
    pub entries: Vec<RecentEntry>,
    pub error: Option<&'static str>,
    pub node_limited: bool,
    pub entries_limited: bool,
    pub selection_incomplete: bool,
    pub local_ambiguous: bool,
    pub max_age_ms: Option<i64>,
    pub validated_at: Option<i64>,
    pub selection_fingerprint: Option<String>,
}

pub fn empty_work_text(work: &RecentWork, now: i64) -> &'static str {
    let observed = !work.nodes.is_empty()
        && !work.selection_incomplete
        && !work.node_limited
        && !work.entries_limited
        && work.nodes.iter().all(|node| {
            node.collection_state() == CollectionState::Succeeded
                && node.pending == Some(false)
                && node.file_errors == Some(0)
                && !node.session_error
                && node.freshness(now, work.max_age_ms.unwrap_or(-1)) == Observation::Current
        });
    if observed {
        "保存済みの表示範囲に作業はありません。収集の網羅性は未確認です。"
    } else {
        "保存済み作業の有無は未確認です。対象と収集状態を確認してください。"
    }
}

/// A refresh invalidates private rows immediately and never overlaps database reads.
#[derive(Default)]
pub struct RecentSlot {
    pub data: Option<RecentWork>,
    pub timed_out: bool,
    pub in_flight: Option<u64>,
    generation: u64,
    pending: bool,
}

impl RecentSlot {
    pub fn request(&mut self) -> Option<u64> {
        self.generation = self.generation.wrapping_add(1);
        self.data = None;
        if self.in_flight.is_some() {
            self.pending = true;
            None
        } else {
            self.timed_out = false;
            self.in_flight = Some(self.generation);
            Some(self.generation)
        }
    }

    pub fn timeout(&mut self, generation: u64) {
        if self.in_flight == Some(generation) {
            self.timed_out = true;
        }
    }

    pub fn complete(&mut self, generation: u64, data: RecentWork) -> Option<u64> {
        if self.in_flight != Some(generation) {
            return None;
        }
        self.in_flight = None;
        self.timed_out = false;
        if generation == self.generation {
            self.data = Some(data);
        }
        if std::mem::take(&mut self.pending) {
            self.in_flight = Some(self.generation);
            Some(self.generation)
        } else {
            None
        }
    }
}

/// Only unambiguous active, non-shared, explicitly allowed nodes may be read.
pub fn targets(nodes: &[Node]) -> Vec<Node> {
    let local_ambiguous = local_ambiguous(nodes);
    let counts = id_counts(nodes);
    nodes
        .iter()
        .filter(|node| {
            let valid_id = node
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.trim().is_empty() && id.len() <= 256 && !id.chars().any(char::is_control) && counts.get(id) == Some(&1));
            let config_valid = matches!(node.get("ai_sessions"), None | Some(Value::Bool(_)));
            valid_id
                && config_valid
                && matches!(node.get("shared"), None | Some(Value::Bool(false)))
                && !(node.local && local_ambiguous)
                && tune_core::ai_sessions::enabled(node)
                && node.local
        })
        .cloned()
        .collect()
}

fn id_counts(nodes: &[Node]) -> std::collections::HashMap<&str, usize> {
    let mut counts = std::collections::HashMap::new();
    for node in nodes {
        *counts.entry(node.id.as_str()).or_insert(0) += 1;
    }
    counts
}

pub(crate) fn normalized_hostname(value: &str) -> Option<String> {
    let name = value.trim().trim_end_matches('.').to_lowercase();
    let name = name.strip_suffix(".local").unwrap_or(&name);
    (!name.is_empty()
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
        && name.bytes().any(|byte| byte.is_ascii_alphanumeric())
        && !name.starts_with('.')
        && !name.ends_with('.'))
    .then(|| name.to_owned())
}

pub(crate) fn resolved_nodes(nodes: &[Node], host: &str) -> Vec<Node> {
    let host = normalized_hostname(host);
    nodes
        .iter()
        .cloned()
        .map(|mut node| {
            node.local = host.is_some() && node.get("local_hostname").and_then(Value::as_str).and_then(normalized_hostname) == host;
            node
        })
        .collect()
}

pub fn targets_for_host(nodes: &[Node], host: &str) -> Vec<Node> {
    if normalized_hostname(host).is_none() { Vec::new() } else { targets(&resolved_nodes(nodes, host)) }
}

pub fn local_ambiguous_for_host(nodes: &[Node], host: &str) -> bool {
    local_ambiguous(&resolved_nodes(nodes, host))
}

pub fn selection_incomplete_for_host(nodes: &[Node], host: &str) -> bool {
    normalized_hostname(host).is_none() || selection_incomplete(&resolved_nodes(nodes, host))
}

/// Count every matching ledger row, including shared or inactive rows.
pub fn local_ambiguous(nodes: &[Node]) -> bool {
    nodes.iter().filter(|node| node.local).take(2).count() > 1
}

pub fn selection_incomplete(nodes: &[Node]) -> bool {
    let counts = id_counts(nodes);
    local_ambiguous(nodes)
        || nodes.iter().any(|node| {
            node.get("id").and_then(Value::as_str).is_none_or(|id| id.trim().is_empty() || id.len() > 256 || id.chars().any(char::is_control))
                || !matches!(node.get("ai_sessions"), None | Some(Value::Bool(_)))
                || !matches!(node.get("shared"), Some(Value::Bool(_)))
                || counts.get(node.id.as_str()).is_some_and(|count| *count > 1)
        })
}

/// A changed permission or local identity invalidates the entire in-flight read.
pub fn same_selection(before: &[Node], after: &[Node]) -> bool {
    let keys = |nodes: &[Node]| targets(nodes).into_iter().map(|node| (node.id, node.local)).collect::<Vec<_>>();
    before.len() == after.len()
        && before.iter().zip(after).all(|(a, b)| a.raw == b.raw && a.local == b.local)
        && keys(before) == keys(after)
        && selection_incomplete(before) == selection_incomplete(after)
        && local_ambiguous(before) == local_ambiguous(after)
}

/// Store only a digest of the full ledger snapshot for an activation-time check.
pub fn selection_fingerprint_for_host(nodes: &[Node], host: &str, node_id: &str) -> Option<String> {
    normalized_hostname(host)?;
    let resolved = resolved_nodes(nodes, host);
    let selected = targets(&resolved);
    if selected.len() != 1 || selected[0].id != node_id {
        return None;
    }
    let values: Vec<_> = resolved.iter().map(|node| (&node.raw, node.local)).collect();
    serde_json::to_vec(&values).ok().map(|bytes| tune_core::db::sha256_hex(&bytes))
}

/// Re-read the ledger immediately before handing a recent-work target to the OS.
pub fn revalidate_local(config_path: &Path, host: &str, node_id: &str, fingerprint: &str) -> bool {
    nodes::load_config(config_path)
        .ok()
        .and_then(|config| selection_fingerprint_for_host(&config.nodes, host, node_id))
        .is_some_and(|current| current == fingerprint)
}

fn flag(value: Option<&Value>) -> Option<bool> {
    match value.and_then(Value::as_i64) {
        Some(0) => Some(false),
        Some(1) => Some(true),
        _ => None,
    }
}

fn time(value: Option<&Value>, now: i64) -> Option<i64> {
    value.and_then(Value::as_i64).filter(|at| *at >= 0 && *at <= now)
}

pub fn project_cursor(node: &str, row: Option<&Value>, now: i64, _max_age_ms: i64) -> SyncStatus {
    let get = |key: &str| row.and_then(|row| row.get(key));
    let error = flag(get("has_error"));
    let python = flag(get("no_python"));
    let failure = match (error, python) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    };
    SyncStatus {
        node: node.to_owned(),
        attempt_at: time(get("updated_at"), now),
        last_ok_at: time(get("last_ok_at"), now),
        failure,
        pending: flag(get("truncated")),
        file_errors: get("file_error_count").and_then(Value::as_u64),
        session_error: false,
    }
}

/// Require the canonical, credential-free public GitHub PR URL shape.
pub fn github_pr_url(value: &str) -> Option<String> {
    let rest = value.strip_prefix("https://github.com/")?;
    if value.len() > 2048 || value.chars().any(char::is_control) {
        return None;
    }
    let parts: Vec<_> = rest.split('/').collect();
    if parts.len() != 4 || parts[2] != "pull" {
        return None;
    }
    let owner = parts[0];
    let repo = parts[1];
    let number = parts[3].parse::<u64>().ok()?;
    if owner.is_empty()
        || owner.len() > 39
        || owner.starts_with('-')
        || owner.ends_with('-')
        || !owner.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        || repo.is_empty()
        || repo.len() > 100
        || matches!(repo, "." | "..")
        || !repo.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        || number == 0
        || number.to_string() != parts[3]
    {
        return None;
    }
    Some(value.to_owned())
}

pub fn project_prs(text: Option<&str>) -> (Vec<String>, PrState) {
    let parsed = text.and_then(|value| serde_json::from_str::<Value>(value).ok());
    let Some(values) = parsed.as_ref().and_then(Value::as_array) else { return (Vec::new(), PrState::Unknown) };
    if values.is_empty() {
        return (Vec::new(), PrState::Empty);
    }
    let valid: Vec<_> = values.iter().filter_map(Value::as_str).filter_map(github_pr_url).collect();
    let incomplete = valid.len() != values.len() || valid.len() > MAX_PRS;
    (valid.into_iter().take(MAX_PRS).collect(), if incomplete { PrState::Partial } else { PrState::Complete })
}

/// Classify home shorthand without interpreting paths for a different OS.
fn home_shorthand_suffix(cwd: &str, windows: bool) -> Result<Option<&str>, ()> {
    if cwd == "~" {
        return Ok(Some(""));
    }
    let suffix = if let Some(rest) = cwd.strip_prefix("~/") {
        Some(rest)
    } else if let Some(rest) = cwd.strip_prefix("~\\") {
        if !windows {
            return Err(());
        }
        Some(rest)
    } else {
        None
    };
    if suffix.is_some_and(|rest| {
        let bytes = rest.as_bytes();
        rest.starts_with('/') || rest.starts_with('\\') || (windows && bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
    }) {
        return Err(());
    }
    Ok(suffix)
}

/// A conservative subset of normal Win32 names; extended namespaces are not supported.
#[cfg(any(windows, test))]
fn reserved_windows_component(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ').to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") {
        return true;
    }
    let Some(suffix) = stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT")) else {
        return false;
    };
    let mut chars = suffix.chars();
    matches!(chars.next(), Some('0'..='9' | '¹' | '²' | '³')) && chars.next().is_none()
}

/// Validate syntax only; the existing OS handoff checks actual directories.
pub(crate) fn safe_local_directory_path(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    if !path.is_absolute()
        || text.starts_with("\\\\")
        || text.starts_with("//")
        || text.chars().any(char::is_control)
        || path.components().any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return false;
    }
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        if !matches!(path.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_))) {
            return false;
        }
        for part in path.components() {
            if let Component::Normal(name) = part {
                let Some(name) = name.to_str() else {
                    return false;
                };
                if name.starts_with(' ')
                    || name.ends_with(' ')
                    || name.ends_with('.')
                    || reserved_windows_component(name)
                    || name.chars().any(|ch| matches!(ch, ':' | '<' | '>' | '"' | '|' | '?' | '*'))
                {
                    return false;
                }
            }
        }
    }
    true
}

/// A remote path or UNC path is never converted to a local file URL.
pub fn local_cwd_url(local: bool, cwd: &str) -> Option<String> {
    local_cwd_url_with_home(local, cwd, &nodes::home())
}

fn local_cwd_path_and_url(local: bool, cwd: &str, home: &Path) -> Option<(PathBuf, String)> {
    if !local || cwd.len() > 2048 || cwd.chars().any(char::is_control) {
        return None;
    }
    let path = match home_shorthand_suffix(cwd, cfg!(windows)).ok()? {
        Some(rest) => home.join(rest),
        None => PathBuf::from(cwd),
    };
    if !safe_local_directory_path(&path) {
        return None;
    }
    let path: PathBuf = path.components().collect();
    let url = url::Url::from_directory_path(&path).ok()?;
    let roundtrip = url.to_file_path().ok()?;
    if url.host_str().is_some() || !safe_local_directory_path(&roundtrip) || roundtrip != path {
        return None;
    }
    Some((path, url.into()))
}

pub fn local_cwd_url_with_home(local: bool, cwd: &str, home: &Path) -> Option<String> {
    local_cwd_path_and_url(local, cwd, home).map(|(_, url)| url)
}

/// Bind a locally validated path to the row for which it was observed.
pub fn local_cwd_candidate_with_home(node: &str, local: bool, cwd: &str, home: &Path) -> Option<RecentDirectory> {
    if node.is_empty() || node.chars().any(char::is_control) {
        return None;
    }
    let (path, url) = local_cwd_path_and_url(local, cwd, home)?;
    Some(RecentDirectory { node: node.into(), local, cwd: cwd.into(), path, url })
}

pub fn recent_provenance_notice(sample: bool) -> &'static str {
    if sample {
        "サンプルです。実データでは保存済みの場所が現在の作業場所と一致するか、保存元の実機・利用者・ホームが現在と同じかは未確認です。~ は読込時のホームで展開します。"
    } else {
        "保存済みの場所が現在の作業場所と一致するか、保存元の実機・利用者・ホームが現在と同じかは未確認です。~ は読込時のホームで展開します。"
    }
}

pub fn file_error_count_text(count: Option<u64>) -> String {
    match count {
        None => "不明".into(),
        Some(count) if count >= 5 => format!("{count}件以上"),
        Some(count) => format!("{count}件"),
    }
}

fn max_age(db: &Store, schedule: Option<&Value>) -> Option<i64> {
    let stored = db.get_meta_strict("schedule").ok()?;
    if stored.as_ref().is_some_and(|value| !value.is_object()) {
        return None;
    }
    let value = match stored.as_ref().and_then(|v| v.get("ai_minutes")) {
        Some(value) => Some(value),
        None => match schedule {
            None => None,
            Some(Value::Object(object)) => object.get("ai_minutes"),
            _ => return None,
        },
    };
    let minutes = match value {
        None => tune_core::ai_sessions::DEFAULT_AI_MINUTES,
        Some(value) => value.as_i64().filter(|minutes| (5..=1440).contains(minutes))?,
    };
    minutes.checked_mul(3 * 60 * 1_000)
}

fn entry(node: &Node, row: &Value, now: i64) -> RecentEntry {
    let text = |key: &str, max: usize| {
        row.get(key).and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)).map(str::to_owned)
    };
    let cwd = text("cwd", 2048);
    let directory_url = cwd.as_deref().and_then(|cwd| local_cwd_candidate_with_home(&node.id, node.local, cwd, &nodes::home()));
    let (prs, pr_state) = project_prs(row.get("prs").and_then(Value::as_str));
    RecentEntry {
        node: node.id.clone(),
        local: node.local,
        tool: text("tool", 64).unwrap_or_else(|| "不明".into()),
        cwd,
        activity_at: time(row.get("last_ts"), now),
        directory_url,
        prs,
        pr_state,
    }
}

/// Read saved summaries only; this never contacts a machine or opens a session file.
pub fn load(config_path: &Path, data_dir: &Path, now: i64, local_opt_in: bool) -> RecentWork {
    load_for_host(config_path, data_dir, now, &nodes::local_host(), local_opt_in)
}

pub fn load_for_host(config_path: &Path, data_dir: &Path, now: i64, host: &str, local_opt_in: bool) -> RecentWork {
    if !local_opt_in {
        return RecentWork {
            error: Some("最近の作業は既定で非表示です。対象を確認した後に明示して起動してください。"), ..Default::default()
        };
    }
    let config = match nodes::load_config(config_path) {
        Ok(config) => config,
        Err(_) => return RecentWork { error: Some("台帳を読み取れないため最近の作業は非表示"), ..Default::default() },
    };
    if normalized_hostname(host).is_none() {
        return RecentWork {
            error: Some("この機体の識別が不明なため最近の作業は非表示"), selection_incomplete: true, ..Default::default()
        };
    }
    let initial_nodes = resolved_nodes(&config.nodes, host);
    if !initial_nodes.iter().any(|node| node.local) {
        return RecentWork {
            error: Some("機体照合不明：この機体に一致する台帳行がありません"), selection_incomplete: true, ..Default::default()
        };
    }
    let selected = targets(&initial_nodes);
    let node_limited = selected.len() > MAX_NODES;
    let selection_incomplete = selection_incomplete(&initial_nodes);
    let local_ambiguous = local_ambiguous(&initial_nodes);
    let selected: Vec<_> = selected.into_iter().take(MAX_NODES).collect();
    let mut result = RecentWork { node_limited, selection_incomplete, local_ambiguous, validated_at: Some(now), ..Default::default() };
    if selected.is_empty() {
        return result;
    }
    let db = match Store::open_read_only(data_dir) {
        Ok(db) => db,
        Err(_) => {
            result.nodes = selected.iter().map(|node| project_cursor(&node.id, None, now, -1)).collect();
            result.error = Some("保存済み AI 情報を読み取れません");
            return result;
        }
    };
    if db.conn().execute_batch("BEGIN").is_err() {
        result.error = Some("保存済み AI 情報を一貫して読み取れません");
        return result;
    }
    result.max_age_ms = max_age(&db, config.schedule.as_ref());
    let ids: Vec<_> = selected.iter().map(|node| node.id.clone()).collect();
    let cursors = match db.ai_cursor_summaries(&ids) {
        Ok(cursors) => cursors,
        Err(_) => {
            result.error = Some("収集状態を読み取れません");
            Vec::new()
        }
    };
    for node in &selected {
        let cursor = cursors.iter().find(|row| row.get("node_id").and_then(Value::as_str) == Some(node.id.as_str()));
        let mut status = project_cursor(&node.id, cursor, now, result.max_age_ms.unwrap_or(-1));
        match db.ai_recent_work_list(&node.id, PER_NODE + 1) {
            Ok(rows) => {
                result.entries_limited |= rows.len() > PER_NODE;
                result.entries.extend(rows.iter().take(PER_NODE).map(|row| entry(node, row, now)));
            }
            Err(_) => status.session_error = true,
        }
        result.nodes.push(status);
    }
    result.entries.sort_by(|a, b| b.activity_at.cmp(&a.activity_at).then_with(|| a.node.cmp(&b.node)));
    if result.entries.len() > MAX_ENTRIES {
        result.entries_limited = true;
        result.entries.truncate(MAX_ENTRIES);
    }
    let current = nodes::load_config(config_path).ok();
    if !current.as_ref().is_some_and(|current| same_selection(&initial_nodes, &resolved_nodes(&current.nodes, host))) {
        return RecentWork { error: Some("台帳の対象が変わったため最近の作業は非表示"), ..Default::default() };
    }
    result.max_age_ms = max_age(&db, current.as_ref().and_then(|current| current.schedule.as_ref()));
    if db.conn().execute_batch("COMMIT").is_err() {
        return RecentWork { error: Some("保存済み AI 情報を一貫して読み取れません"), ..Default::default() };
    }
    result.selection_fingerprint =
        current.as_ref().and_then(|current| selected.first().and_then(|node| selection_fingerprint_for_host(&current.nodes, host, &node.id)));
    if result.selection_fingerprint.is_none() {
        return RecentWork { error: Some("台帳の対象を照合できないため最近の作業は非表示"), ..Default::default() };
    }
    result.validated_at = Some(tune_core::db::now_ms());
    result
}

#[cfg(test)]
mod path_syntax_tests {
    use super::{home_shorthand_suffix, recent_provenance_notice, reserved_windows_component};

    #[test]
    fn virtual_windows_and_posix_home_shorthand_reject_ambiguous_separators() {
        assert_eq!(home_shorthand_suffix("~", false), Ok(Some("")));
        assert_eq!(home_shorthand_suffix("~/project", false), Ok(Some("project")));
        assert_eq!(home_shorthand_suffix("~\\project", false), Err(()));
        assert_eq!(home_shorthand_suffix("~//project", false), Err(()));
        assert_eq!(home_shorthand_suffix("~/\\project", false), Err(()));

        assert_eq!(home_shorthand_suffix("~", true), Ok(Some("")));
        assert_eq!(home_shorthand_suffix("~/project", true), Ok(Some("project")));
        assert_eq!(home_shorthand_suffix("~\\project", true), Ok(Some("project")));
        assert_eq!(home_shorthand_suffix("~//project", true), Err(()));
        assert_eq!(home_shorthand_suffix("~\\\\project", true), Err(()));
        assert_eq!(home_shorthand_suffix("~\\/project", true), Err(()));
        assert_eq!(home_shorthand_suffix("~/C:/project", true), Err(()));
    }

    #[test]
    fn virtual_windows_device_names_are_rejected_including_extensions_and_superscripts() {
        for name in [
            "CON",
            "con.txt",
            "PRN",
            "aux.data",
            "NUL.tar.gz",
            "COM0",
            "COM1",
            "com9.txt",
            "COM¹",
            "COM².txt",
            "com³.tar.gz",
            "LPT0",
            "LPT1",
            "lpt9.log",
            "LPT¹",
            "lpt².txt",
            "Lpt³.x",
            "CONIN$",
            "conout$.txt",
        ] {
            assert!(reserved_windows_component(name));
        }
        for name in ["console", "COM10", "LPT10", "NULled", "xCON", "COM⁴", ".env", "project"] {
            assert!(!reserved_windows_component(name));
        }
    }

    #[test]
    fn provenance_notice_is_present_for_sample_and_regular_views() {
        for sample in [false, true] {
            let notice = recent_provenance_notice(sample);
            for expected in ["保存", "現在の作業場所", "保存元", "実機", "利用者", "未確認", "ホーム"] {
                assert!(notice.contains(expected));
            }
        }
        assert!(recent_provenance_notice(true).contains("サンプル"));
    }
}
