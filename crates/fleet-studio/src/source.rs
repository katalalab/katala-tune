use std::path::Path;

use tune_core::{db::Store, nodes};

use crate::{Observation, observation};

#[derive(Clone, Debug)]
pub struct NodeCard {
    pub id: String,
    pub role: String,
    pub operating_state: String,
    pub local: bool,
    pub last_at: Option<i64>,
    pub observation: Observation,
    pub failure_reason: Option<&'static str>,
    pub max_age_ms: Option<i64>,
    pub score: Option<i64>,
    pub critical: Option<u64>,
    pub warnings: Option<u64>,
}

impl NodeCard {
    pub fn current_observation(&self, now: i64) -> Observation {
        match self.observation {
            Observation::Current | Observation::Stale => observation(self.last_at, false, true, now, self.max_age_ms.unwrap_or(-1)),
            other => other,
        }
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

/// Keep prior values visible when a new read fails, but never mark them current.
pub fn preserve_previous(previous: &Dashboard, next: &mut Dashboard) {
    if next.config_error.is_some() && next.nodes.is_empty() {
        next.nodes = previous.nodes.clone();
        for node in &mut next.nodes {
            if node.observation != Observation::Excluded {
                node.observation = Observation::Unknown;
                node.failure_reason = None;
            }
        }
    } else {
        for node in &mut next.nodes {
            if node.last_at.is_none()
                && matches!(node.observation, Observation::Unknown | Observation::Failed | Observation::Excluded)
                && let Some(old) = previous.nodes.iter().find(|old| old.id == node.id)
            {
                node.last_at = old.last_at;
                node.score = old.score;
                node.critical = old.critical;
                node.warnings = old.warnings;
            }
        }
    }
    if next.repositories.is_empty() && (next.topology_error.is_some() || next.config_error.is_some() || next.database_error.is_some()) {
        next.repositories = previous.repositories.clone();
        for repo in &mut next.repositories {
            repo.previous = true;
        }
        next.topology_coverage = Coverage::Unknown;
        next.topology_oldest_at = previous.topology_oldest_at;
        next.topology_limited = previous.topology_limited;
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

/// Read only existing Katala Tune data; never create or migrate it.
pub fn load(config: &Path, data_dir: &Path, now: i64) -> Dashboard {
    let config = match nodes::load_config(config) {
        Ok(config) => config,
        Err(_) => {
            return Dashboard {
                nodes: Vec::new(),
                config_error: Some("台帳を読み取れません".into()),
                database_error: None,
                repositories: Vec::new(),
                topology_coverage: Coverage::Unknown,
                topology_error: None,
                topology_oldest_at: None,
                topology_limited: false,
            };
        }
    };
    let database = Store::open_read_only(data_dir);
    let database_error = database.as_ref().err().map(|_| "保存済みデータベースを読み取れません".to_owned());
    let age = database.as_ref().ok().and_then(|db| max_age(db, config.schedule.as_ref()));
    let nodes = config
        .nodes
        .iter()
        .map(|node| {
            let op = node.op_state();
            let mut reason = None;
            let (last_at, status) = if !op.active {
                let last_at = database
                    .as_ref()
                    .ok()
                    .and_then(|db| db.last_snapshots(&node.id, 1).ok())
                    .and_then(|snapshots| snapshots.first().map(|snapshot| snapshot.at));
                (last_at, Observation::Excluded)
            } else if let Ok(db) = &database {
                match db.last_snapshots(&node.id, 1) {
                    Ok(snapshots) => {
                        let last_at = snapshots.first().map(|snapshot| snapshot.at);
                        match (db.probe_error(&node.id, last_at), age) {
                            (Ok(error), Some(max_age)) => {
                                reason = error.as_deref().map(failure_reason);
                                (last_at, observation(last_at, error.is_some(), true, now, max_age))
                            }
                            (Ok(Some(error)), None) => {
                                reason = Some(failure_reason(&error));
                                (last_at, Observation::Failed)
                            }
                            _ => (last_at, Observation::Unknown),
                        }
                    }
                    Err(_) => (None, Observation::Unknown),
                }
            } else {
                (None, Observation::Unknown)
            };
            let history = database
                .as_ref()
                .ok()
                .and_then(|db| db.history(&node.id, 1).ok())
                .and_then(|rows| rows.into_iter().find(|row| row.get("at").and_then(|v| v.as_i64()) == last_at));
            NodeCard {
                id: node.id.clone(),
                role: node.get("role").and_then(|v| v.as_str()).unwrap_or("不明").to_owned(),
                operating_state: op.state,
                local: node.local,
                last_at,
                observation: status,
                failure_reason: reason,
                max_age_ms: age,
                score: history.as_ref().and_then(|v| v.get("score")).and_then(|v| v.as_i64()),
                critical: history.as_ref().and_then(|v| v.get("critical")).and_then(|v| v.as_u64()),
                warnings: history.as_ref().and_then(|v| v.get("warn")).and_then(|v| v.as_u64()),
            }
        })
        .collect();
    let topology = database.as_ref().ok().and_then(|db| topology_from(db, "", now).ok());
    let topology_error = topology.is_none().then(|| "保存済みの構成情報を読み取れません".to_owned());
    let topology = topology.unwrap_or(TopologyResult { repositories: Vec::new(), coverage: Coverage::Unknown, oldest_at: None, limited: false });
    Dashboard {
        nodes,
        config_error: None,
        database_error,
        repositories: topology.repositories,
        topology_coverage: topology.coverage,
        topology_error,
        topology_oldest_at: topology.oldest_at,
        topology_limited: topology.limited,
    }
}

#[derive(Clone, Debug)]
pub struct TopologyResult {
    pub repositories: Vec<Repository>,
    pub coverage: Coverage,
    pub oldest_at: Option<i64>,
    pub limited: bool,
}

/// Search the bounded existing topology projection without collecting or changing data.
pub fn search_topology(data_dir: &Path, filter: &str, now: i64) -> Result<TopologyResult, &'static str> {
    if filter.len() > 256 || filter.chars().any(char::is_control) {
        return Err("検索語が長すぎるか不正です");
    }
    let database = Store::open_read_only(data_dir).map_err(|_| "保存済みデータベースを読み取れません")?;
    topology_from(&database, filter, now)
}

fn topology_from(db: &Store, filter: &str, now: i64) -> Result<TopologyResult, &'static str> {
    let view = db
        .topology_ui_view(&tune_core::db::TopologyUiRequest { kind: "repository".into(), filter: filter.into(), ..Default::default() })
        .map_err(|_| "保存済みの構成情報を読み取れません")?;
    let (repositories, coverage) = repositories(&view, now, 24 * 60 * 60 * 1_000)?;
    let limited = view.pointer("/window/filtered_stored_totals/accepted_entities").and_then(|v| v.as_u64()).is_some_and(|n| n > repositories.len() as u64)
        || view.get("candidates").and_then(|v| v.as_array()).is_some_and(|rows| !rows.is_empty());
    let oldest_at = view.get("scopes").and_then(|v| v.as_array()).and_then(|scopes| {
        scopes.iter().filter_map(|v| v.get("observed_at").and_then(|v| v.as_i64())).chain(repositories.iter().filter_map(|repo| repo.observed_at)).min()
    });
    Ok(TopologyResult { repositories, coverage, oldest_at, limited })
}

fn max_age(db: &Store, configured: Option<&serde_json::Value>) -> Option<i64> {
    let schedule = db.get_meta("schedule").ok()?;
    let value = schedule
        .as_ref()
        .and_then(|v| v.as_object())
        .and_then(|v| v.get("probe_minutes"))
        .or_else(|| configured.and_then(|v| v.as_object()).and_then(|v| v.get("probe_minutes")));
    let minutes = match value {
        None => 60,
        Some(value) => value.as_i64().filter(|minutes| (5..=1440).contains(minutes))?,
    };
    minutes.checked_mul(3 * 60 * 1_000)
}
