use std::path::Path;

use tune_core::{db::Store, nodes};

use crate::{
    Observation, observation,
    scope::{Grants, Permit},
};
use tune_core::db::StoredSchedule;
pub use tune_core::db::{MachineReading, ProbeFailure};

pub const TOPOLOGY_SCOPE_UNAVAILABLE: &str = "構成情報は機体・権限の対象を確認できないため非表示です";

#[derive(Clone, Debug)]
pub struct NodeCard {
    pub id: String,
    pub fingerprint: Option<String>,
    pub permit: Option<Permit>,
    pub saved_state: SavedState,
    pub previous: bool,
    pub role: String,
    pub operating_state: String,
    pub local: Option<bool>,
    pub last_at: Option<i64>,
    pub observation: Observation,
    pub failure_reason: Option<&'static str>,
    pub failure_at: Option<i64>,
    pub max_age_ms: Option<i64>,
    pub score: Option<i64>,
    pub critical: Option<u64>,
    pub warnings: Option<u64>,
}

impl NodeCard {
    pub fn matches_search(&self, query: &str) -> bool {
        query.is_empty() || format!("{} {} {}", self.id, self.role, self.operating_state).to_lowercase().contains(&query.to_lowercase())
    }

    pub fn is_visible_target(&self, id: &str, fingerprint: &str, query: &str) -> bool {
        self.id == id && self.fingerprint.as_deref() == Some(fingerprint) && self.matches_search(query)
    }

    pub fn identity_text(&self) -> &'static str {
        match self.local {
            Some(true) => "この機体",
            Some(false) if self.role.contains("この機体") => "台帳の『この機体』表記は現在ホストと不一致",
            Some(false) => "別の機体",
            None => "同一性不明",
        }
    }

    pub fn current_observation(&self, now: i64) -> Observation {
        match self.observation {
            Observation::Current | Observation::Stale => observation(self.last_at, false, true, now, self.max_age_ms.unwrap_or(-1)),
            other => other,
        }
    }
    /// Never reveal saved status unless the exact grant is currently usable.
    pub fn visible_status(&self, now: i64, authorized: bool, rechecking: bool) -> &'static str {
        if rechecking {
            return "台帳を再照合中：保存要約は非表示";
        }
        if !authorized {
            return "保存情報: 未確認／表示対象外";
        }
        match self.saved_state {
            SavedState::Unapproved => "保存情報: 未確認／表示対象外",
            SavedState::NoData => "保存要約なし（機体の状態は不明）",
            SavedState::Unavailable => "保存要約を読み取れません",
            SavedState::Invalid => "記録が不正、または時刻不明",
            SavedState::Available => match self.current_observation(now) {
                Observation::Current => "保存要約の記録時刻が新しい",
                Observation::Stale => "保存要約の記録が古い",
                Observation::Failed => "保存された取得失敗の記録",
                Observation::Unknown | Observation::Excluded => "保存要約の鮮度不明",
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SavedState {
    Unapproved,
    NoData,
    Available,
    Unavailable,
    Invalid,
}

impl NodeCard {
    pub fn clear_saved(&mut self) {
        self.permit = None;
        self.saved_state = SavedState::Unapproved;
        self.previous = false;
        self.last_at = None;
        self.observation = Observation::Unknown;
        self.failure_reason = None;
        self.failure_at = None;
        self.max_age_ms = None;
        self.score = None;
        self.critical = None;
        self.warnings = None;
    }
}

/// Return a safe error category without exposing raw command output.
pub fn failure_reason(error: &str) -> &'static str {
    let error = error.to_lowercase();
    if error.contains("host key") || error.contains("ホスト鍵") {
        "ホスト鍵"
    } else if error.contains("permission denied") || error.contains("認証") {
        "SSH 認証・権限"
    } else if error.contains("timed out") || error.contains("timeout") || error.contains("タイムアウト") {
        "タイムアウト"
    } else {
        "詳細不明（Katala Tune で確認）"
    }
}

/// Keep a previous value only after a read failure under the same active grant.
pub fn preserve_previous(previous: &Dashboard, next: &mut Dashboard) {
    if next.config_error.is_some() {
        return;
    }
    for node in &mut next.nodes {
        if node.saved_state != SavedState::Unavailable {
            continue;
        }
        let Some(permit) = &node.permit else { continue };
        let Some(old) = previous
            .nodes
            .iter()
            .find(|old| old.id == node.id && old.fingerprint == node.fingerprint && old.permit.as_ref().is_some_and(|previous| permit.same_as(previous)))
        else {
            continue;
        };
        if old.last_at.is_some() {
            node.last_at = old.last_at;
            node.score = old.score;
            node.critical = old.critical;
            node.warnings = old.warnings;
            node.max_age_ms = old.max_age_ms;
            node.failure_at = old.failure_at;
            node.failure_reason = old.failure_reason;
            node.previous = true;
            node.observation = Observation::Unknown;
        }
    }
}

/// Drop results whose grant was revoked or replaced while the read ran.
pub fn discard_revoked(dashboard: &mut Dashboard, grants: &crate::scope::Grants) {
    for node in &mut dashboard.nodes {
        if node.permit.as_ref().is_none_or(|permit| !grants.contains(&node.id, permit)) {
            node.clear_saved();
        }
    }
}

#[derive(Clone, Debug)]
pub struct Dashboard {
    pub nodes: Vec<NodeCard>,
    pub config_error: Option<String>,
    pub database_error: Option<String>,
    pub repositories: Vec<Repository>,
    pub topology_coverage: Coverage,
    pub topology_error: Option<String>,
    pub topology_oldest_at: Option<i64>,
    pub topology_limited: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Repository {
    pub name: String,
    pub origin: Option<String>,
    pub observed_at: Option<i64>,
    pub previous: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Coverage {
    Complete,
    Selected,
    Partial,
    Stale,
    Unknown,
}

pub fn empty_repository_text(coverage: Coverage, has_error: bool) -> &'static str {
    if coverage == Coverage::Unknown || has_error {
        "保存済みリポジトリの有無は未確認です。"
    } else {
        "表示範囲に該当するリポジトリはありません。"
    }
}

impl Dashboard {
    pub fn current_coverage(&self, now: i64) -> Coverage {
        if !matches!(self.topology_coverage, Coverage::Complete | Coverage::Selected) {
            return self.topology_coverage;
        }
        match observation(self.topology_oldest_at, false, true, now, 24 * 60 * 60 * 1_000) {
            Observation::Current => self.topology_coverage,
            Observation::Stale => Coverage::Stale,
            _ => Coverage::Unknown,
        }
    }
}

impl Repository {
    pub fn freshness(&self, now: i64) -> Observation {
        observation(self.observed_at, false, true, now, 24 * 60 * 60 * 1_000)
    }
}

/// Project accepted topology without treating partial or absent coverage as complete.
pub fn repositories(view: &serde_json::Value, now: i64, max_age_ms: i64) -> Result<(Vec<Repository>, Coverage), &'static str> {
    let entities = view.get("entities").and_then(|v| v.as_array()).ok_or("entities unavailable")?;
    let scopes = view.get("scopes").and_then(|v| v.as_array()).ok_or("coverage unavailable")?;
    let repos: Vec<Repository> = entities
        .iter()
        .filter(|entity| entity.get("kind").and_then(|v| v.as_str()) == Some("repository"))
        .map(|entity| {
            let label = entity.get("label").and_then(|v| v.as_str()).unwrap_or("名称不明");
            let origin = entity
                .get("attributes")
                .and_then(|v| v.get("origin"))
                .and_then(|v| v.as_str())
                .or_else(|| (label.contains('/') && label.chars().count() < 256).then_some(label));
            Repository {
                name: tune_core::hygiene::strip_url(Some(label)).unwrap_or_else(|| "名称不明".into()),
                origin: tune_core::hygiene::strip_url(origin),
                observed_at: entity.get("facts_observed_at_ms").and_then(|v| v.as_i64()),
                previous: false,
            }
        })
        .collect();
    let mut coverage = if scopes.is_empty() { Coverage::Unknown } else { Coverage::Complete };
    for scope in scopes {
        let status = scope.get("status").and_then(|v| v.as_str());
        let observed_at = scope.get("observed_at").and_then(|v| v.as_i64());
        if matches!(status, Some("partial" | "failed")) {
            coverage = Coverage::Partial;
            break;
        }
        if status != Some("complete") || observed_at.is_none_or(|at| at < 0 || at > now) || max_age_ms < 0 {
            coverage = Coverage::Unknown;
        } else if observed_at.is_some_and(|at| now.saturating_sub(at) > max_age_ms) && coverage == Coverage::Complete {
            coverage = Coverage::Stale;
        }
    }
    for repo in &repos {
        if coverage == Coverage::Partial {
            break;
        }
        match observation(repo.observed_at, false, true, now, max_age_ms) {
            Observation::Unknown => coverage = Coverage::Unknown,
            Observation::Stale if coverage == Coverage::Complete => coverage = Coverage::Stale,
            _ => {}
        }
    }
    let limited = view.pointer("/window/filtered_stored_totals/accepted_entities").and_then(|v| v.as_u64()).is_some_and(|total| total > repos.len() as u64)
        || view.get("candidates").and_then(|v| v.as_array()).is_some_and(|rows| !rows.is_empty());
    if limited && coverage != Coverage::Unknown {
        coverage = Coverage::Partial;
    }
    // The bounded projection only includes receipts for selected scopes. It cannot
    // establish coverage for a failed scope which produced no accepted entities.
    if view.get("schema").and_then(|v| v.as_str()) == Some("katala_topology_ui.v1") {
        let missing_scope = entities.iter().filter(|entity| entity.get("kind").and_then(|v| v.as_str()) == Some("repository")).any(|entity| {
            entity.get("scope_ids_truncated").and_then(|v| v.as_bool()) == Some(true)
                || entity
                    .get("scope_ids")
                    .and_then(|v| v.as_array())
                    .is_none_or(|ids| ids.is_empty() || ids.iter().any(|id| !scopes.iter().any(|scope| scope.get("scope_id") == Some(id))))
        });
        if missing_scope && coverage != Coverage::Partial {
            coverage = Coverage::Unknown;
        }
        if coverage == Coverage::Complete {
            coverage = Coverage::Selected;
        }
    }
    Ok((repos, coverage))
}

/// Fixed category; raw DB errors are never exposed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SavedReadError;

/// Abstraction permits synthetic-only verification without opening any file.
pub trait SavedReader {
    fn read_machine(&self, node_id: &str) -> Result<MachineReading, SavedReadError>;
}

impl SavedReader for Store {
    fn read_machine(&self, node_id: &str) -> Result<MachineReading, SavedReadError> {
        self.saved_machine_reading(node_id).map_err(|_| SavedReadError)
    }
}

fn fingerprints_for_nodes(nodes: &[nodes::Node], host: &str) -> std::collections::HashMap<String, String> {
    let Some(host) = crate::recent::normalized_hostname(host) else { return Default::default() };
    let resolved = crate::recent::resolved_nodes(nodes, &host);
    let ambiguous = crate::recent::local_ambiguous(&resolved);
    let mut counts = std::collections::HashMap::new();
    for node in &resolved {
        *counts.entry(node.id.as_str()).or_insert(0_usize) += 1;
    }
    resolved
        .iter()
        .filter_map(|node| {
            let id = node.get("id")?.as_str()?;
            if id != node.id
                || id.trim() != id
                || id.is_empty()
                || id.len() > 256
                || id.chars().any(char::is_control)
                || counts.get(id) != Some(&1)
                || (node.local && ambiguous)
            {
                return None;
            }
            if node.get("local_hostname").is_some_and(|value| !value.is_null() && value.as_str().and_then(crate::recent::normalized_hostname).is_none()) {
                return None;
            }
            let bytes = serde_json::to_vec(&(&node.raw, &host, node.local)).ok()?;
            Some((id.to_owned(), tune_core::db::sha256_hex(&bytes)))
        })
        .collect()
}

/// Fingerprint only the uniquely identified current ledger row; the digest is
/// not evidence of the physical origin of an older saved record.
pub fn fingerprint_for_node(nodes: &[nodes::Node], host: &str, id: &str) -> Option<String> {
    fingerprints_for_nodes(nodes, host).remove(id)
}

fn summary_max_age(stored: StoredSchedule, configured: Option<&serde_json::Value>) -> Option<i64> {
    let value = match stored {
        StoredSchedule::Minutes(minutes) => Some(minutes),
        StoredSchedule::Invalid => return None,
        StoredSchedule::Missing => match configured {
            None => None,
            Some(serde_json::Value::Object(object)) => match object.get("probe_minutes") {
                None => None,
                Some(value) => Some(value.as_i64()?),
            },
            _ => return None,
        },
    };
    let minutes = value.unwrap_or(60);
    if !(5..=1440).contains(&minutes) {
        return None;
    }
    minutes.checked_mul(3 * 60 * 1_000)
}

fn apply_reading(card: &mut NodeCard, reading: MachineReading, schedule: Option<&serde_json::Value>, now: i64) {
    let invalid_time = reading.last_at.is_some_and(|at| at < 0 || at > now) || matches!(reading.failure, ProbeFailure::At(at) if at < 0 || at > now);
    if invalid_time || reading.failure == ProbeFailure::Invalid || reading.score.is_some_and(|score| !(0..=100).contains(&score)) {
        card.saved_state = SavedState::Invalid;
        card.observation = Observation::Unknown;
        return;
    }
    card.max_age_ms = summary_max_age(reading.schedule, schedule);
    card.last_at = reading.last_at;
    card.score = reading.score;
    card.critical = reading.critical;
    card.warnings = reading.warnings;
    let failed = match reading.failure {
        ProbeFailure::At(at) => reading.last_at.is_none_or(|last| at >= last),
        ProbeFailure::None => false,
        ProbeFailure::Invalid => unreachable!(),
    };
    if failed {
        card.saved_state = SavedState::Available;
        card.failure_at = match reading.failure {
            ProbeFailure::At(at) => Some(at),
            _ => None,
        };
        card.observation = Observation::Failed;
        card.failure_reason = Some("保存された失敗記録（詳細未確認）");
    } else if reading.last_at.is_none() {
        card.saved_state = SavedState::NoData;
        card.observation = Observation::Unknown;
    } else {
        card.saved_state = SavedState::Available;
        card.observation = observation(reading.last_at, false, true, now, card.max_age_ms.unwrap_or(-1));
    }
}

/// Project ledger cards first; open the observation source lazily and only for
/// matching, still-active grants. This function is also exercised with fakes.
pub fn project_nodes<R: SavedReader>(
    nodes: &[nodes::Node],
    schedule: Option<&serde_json::Value>,
    host: &str,
    now: i64,
    grants: &Grants,
    open: impl FnOnce() -> Result<R, SavedReadError>,
) -> Dashboard {
    let fingerprints = fingerprints_for_nodes(nodes, host);
    let host_known = crate::recent::normalized_hostname(host).is_some();
    let resolved = crate::recent::resolved_nodes(nodes, host);
    let local_ambiguous = crate::recent::local_ambiguous(&resolved);
    let mut database_error = None;
    let mut opener = Some(open);
    let mut reader: Option<Result<R, SavedReadError>> = None;
    let mut cards = Vec::with_capacity(resolved.len());

    for node in &resolved {
        let fingerprint = fingerprints.get(&node.id).cloned();
        let permit = fingerprint.as_deref().and_then(|fingerprint| grants.matching(&node.id, fingerprint));
        let mut card = NodeCard {
            id: node.id.clone(),
            fingerprint,
            permit: permit.clone(),
            saved_state: SavedState::Unapproved,
            previous: false,
            role: node.get("role").and_then(|v| v.as_str()).unwrap_or("不明").to_owned(),
            operating_state: node.op_state().state,
            local: if !host_known
                || node.get("local_hostname").and_then(serde_json::Value::as_str).and_then(crate::recent::normalized_hostname).is_none()
                || (node.local && local_ambiguous)
            {
                None
            } else {
                Some(node.local)
            },
            last_at: None,
            observation: Observation::Unknown,
            failure_reason: None,
            failure_at: None,
            max_age_ms: None,
            score: None,
            critical: None,
            warnings: None,
        };
        if let Some(permit) = &permit {
            let result = permit
                .read(|| {
                    if reader.is_none() {
                        let open = opener.take()?;
                        reader = Some(open());
                    }
                    if !permit.is_active() {
                        return None;
                    }
                    Some(match reader.as_ref() {
                        Some(Ok(reader)) => reader.read_machine(&node.id),
                        Some(Err(_)) => {
                            database_error = Some("保存済みデータベースを読み取れません".to_owned());
                            Err(SavedReadError)
                        }
                        None => Err(SavedReadError),
                    })
                })
                .flatten();
            match result {
                Some(Ok(reading)) => apply_reading(&mut card, reading, schedule, now),
                Some(Err(_)) => {
                    card.saved_state = SavedState::Unavailable;
                    card.observation = Observation::Unknown;
                }
                None => card.clear_saved(),
            }
        }
        cards.push(card);
    }
    Dashboard {
        nodes: cards,
        config_error: None,
        database_error,
        repositories: Vec::new(),
        topology_coverage: Coverage::Unknown,
        topology_error: Some(TOPOLOGY_SCOPE_UNAVAILABLE.into()),
        topology_oldest_at: None,
        topology_limited: false,
    }
}

/// Recheck the selected ledger rows after the read. A failed recheck or any
/// selected-row/schedule change revokes all results from this read.
pub fn revalidate_dashboard(
    dashboard: &mut Dashboard,
    current_nodes: Option<&[nodes::Node]>,
    previous_schedule: Option<&serde_json::Value>,
    current_schedule: Option<&serde_json::Value>,
    host: &str,
) {
    let invalid = match current_nodes {
        None => true,
        Some(nodes) => {
            let fingerprints = fingerprints_for_nodes(nodes, host);
            previous_schedule != current_schedule
                || dashboard.nodes.iter().filter(|node| node.permit.is_some()).any(|node| fingerprints.get(&node.id) != node.fingerprint.as_ref())
        }
    };
    if invalid {
        for node in &mut dashboard.nodes {
            if let Some(permit) = &node.permit {
                permit.cancel();
            }
            node.clear_saved();
            node.fingerprint = None;
        }
        dashboard.config_error = Some("台帳を再照合できないか変更があったため、保存情報は非表示です。更新してください。".into());
    }
}

/// Read the ledger but no saved machine observations without session grants.
pub fn load(config: &Path, data_dir: &Path, now: i64) -> Dashboard {
    load_for_host_scoped(config, data_dir, now, &nodes::local_host(), &Grants::default())
}

pub fn load_for_host(config: &Path, data_dir: &Path, now: i64, host: &str) -> Dashboard {
    load_for_host_scoped(config, data_dir, now, host, &Grants::default())
}

pub fn load_scoped(config: &Path, data_dir: &Path, now: i64, grants: &Grants) -> Dashboard {
    load_for_host_scoped(config, data_dir, now, &nodes::local_host(), grants)
}

pub fn load_for_host_scoped(config_path: &Path, data_dir: &Path, now: i64, host: &str, grants: &Grants) -> Dashboard {
    let config = match nodes::load_config(config_path) {
        Ok(config) => config,
        Err(_) => {
            return Dashboard {
                nodes: Vec::new(),
                config_error: Some("台帳を読み取れません".into()),
                database_error: None,
                repositories: Vec::new(),
                topology_coverage: Coverage::Unknown,
                topology_error: Some(TOPOLOGY_SCOPE_UNAVAILABLE.into()),
                topology_oldest_at: None,
                topology_limited: false,
            };
        }
    };
    let mut dashboard =
        project_nodes(&config.nodes, config.schedule.as_ref(), host, now, grants, || Store::open_read_only(data_dir).map_err(|_| SavedReadError));
    if dashboard.nodes.iter().any(|node| node.permit.is_some()) {
        let current = nodes::load_config(config_path).ok();
        revalidate_dashboard(
            &mut dashboard,
            current.as_ref().map(|config| config.nodes.as_slice()),
            config.schedule.as_ref(),
            current.as_ref().and_then(|config| config.schedule.as_ref()),
            host,
        );
    }
    dashboard
}

#[derive(Clone, Debug)]
pub struct TopologyResult {
    pub repositories: Vec<Repository>,
    pub coverage: Coverage,
    pub oldest_at: Option<i64>,
    pub limited: bool,
}

/// Refuse topology search until the requested machine and scope can be authorized.
pub fn search_topology(_data_dir: &Path, _filter: &str, _now: i64) -> Result<TopologyResult, &'static str> {
    Err(TOPOLOGY_SCOPE_UNAVAILABLE)
}
