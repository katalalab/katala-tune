//! プロセス所見の根拠と手当てを、停止操作とは分けて表す。
//!
//! このモジュールは副作用を持たない。`rules` が高 CPU の所見に付け、
//! `actions` は従来どおり確認・保護台帳の再読・直前の再検証を担当する。

use serde::Serialize;
use serde_json::{Value, json};

const MAX_EVIDENCE_AGE_MS: i64 = 5 * 60 * 1000;
const STOP_MIN_CPU: f64 = 50.0;
const MAX_CPU_PERCENT_OF_CORE: f64 = 10_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    Observed,
    Partial,
    Stale,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonStatus {
    Confirmed,
    Hypothesis,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessClass {
    OsRequired,
    ProtectedWorkload,
    LegitimateParallel,
    DuplicateCandidate,
    Unclassified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemediationClass {
    AutoSafe,
    RequiresApproval,
    ExternalDependency,
    ManualOnly,
    NoAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopRisk {
    None,
    Low,
    Medium,
    High,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EvidenceRef {
    pub run: String,
    pub scope: String,
    pub source_sha256: String,
    pub observed_at: i64,
    pub rowref: String,
}

/// 所有者の値そのものは保持しない。取得できたかだけを扱う。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProcessIdentity {
    pub pid: i64,
    pub ppid: Option<i64>,
    pub started_at: Option<String>,
    pub owner_observed: bool,
    pub exe_name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Metric {
    pub value: f64,
    pub unit: String,
    pub definition: String,
    pub sample_window_ms: i64,
    pub at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TypedFinding {
    pub symptom: String,
    pub evidence_state: EvidenceState,
    pub process_class: ProcessClass,
    pub identity: ProcessIdentity,
    pub metrics: Vec<Metric>,
    pub reason: Value,
    pub evidence: Vec<EvidenceRef>,
    pub needed_observation: Vec<String>,
    pub remediation: Value,
    pub proposed_action: Option<Value>,
}

fn enum_value<T: Serialize>(v: T) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

fn parse_evidence_state(v: Option<&Value>) -> EvidenceState {
    match v.and_then(Value::as_str) {
        Some("observed") => EvidenceState::Observed,
        Some("partial") => EvidenceState::Partial,
        Some("stale") => EvidenceState::Stale,
        _ => EvidenceState::Unknown,
    }
}

fn parse_reason_status(v: Option<&Value>) -> ReasonStatus {
    match v.and_then(Value::as_str) {
        Some("confirmed") => ReasonStatus::Confirmed,
        Some("hypothesis") => ReasonStatus::Hypothesis,
        _ => ReasonStatus::Unknown,
    }
}

fn parse_process_class(v: Option<&Value>, protected: bool) -> ProcessClass {
    if protected {
        return ProcessClass::ProtectedWorkload;
    }
    match v.and_then(Value::as_str) {
        Some("os_required") => ProcessClass::OsRequired,
        Some("protected_workload") => ProcessClass::ProtectedWorkload,
        Some("legitimate_parallel") => ProcessClass::LegitimateParallel,
        Some("duplicate_candidate") => ProcessClass::DuplicateCandidate,
        _ => ProcessClass::Unclassified,
    }
}

fn safe_ref(v: &Value) -> Option<EvidenceRef> {
    let sha = v.get("source_sha256")?.as_str()?;
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let text = |key| {
        v.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 160 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-')))
            .map(str::to_owned)
    };
    Some(EvidenceRef {
        run: text("run")?,
        scope: text("scope")?,
        source_sha256: sha.to_ascii_lowercase(),
        observed_at: v.get("observed_at")?.as_i64()?,
        rowref: text("rowref")?,
    })
}

fn safe_exe_name(v: Option<&Value>) -> String {
    v.and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 80 && !s.contains('/') && !s.contains('\\') && !s.contains('='))
        .unwrap_or("unknown")
        .to_owned()
}

fn safe_start(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b':')))
        .map(str::to_owned)
}

fn metric(v: &Value) -> Option<Metric> {
    let value = v.get("value")?.as_f64().filter(|n| n.is_finite() && (0.0..=MAX_CPU_PERCENT_OF_CORE).contains(n))?;
    let unit = v.get("unit")?.as_str()?.to_owned();
    let definition = v.get("definition")?.as_str()?.to_owned();
    let sample_window_ms = v.get("sample_window_ms")?.as_i64().filter(|n| *n > 0)?;
    let at = v.get("at")?.as_i64()?;
    if unit != "percent_of_core" || definition != "cpu_time_delta" || sample_window_ms > MAX_EVIDENCE_AGE_MS {
        return None;
    }
    Some(Metric { value, unit, definition, sample_window_ms, at })
}

fn flag(meta: &Value, key: &str) -> bool {
    meta.get(key).and_then(Value::as_bool) == Some(true)
}

/// `AutoSafe` のときだけ、全前提が満たされた読み取り・表示操作を自動実行できる。
/// プロセス停止は常に RequiresApproval なので、ここは常に false になる。
pub fn can_auto_fix(class: RemediationClass, risk: StopRisk, blocked: bool, all_preconditions_observed: bool) -> bool {
    class == RemediationClass::AutoSafe && risk == StopRisk::None && !blocked && all_preconditions_observed
}

/// 高 CPU のプロセスを停止候補にしてよいかを判定する。
///
/// collector が `decision` に明示した照合結果だけを信用する。名前・CPU・同名数から
/// DuplicateCandidate を推測しないため、未収集データは必ず「停止なし」になる。
pub fn process_stop_finding_at(process: &Value, node: &Value, protected: bool, now_ms: i64) -> TypedFinding {
    let meta = process.get("decision").unwrap_or(&Value::Null);
    let identity = ProcessIdentity {
        pid: process.get("pid").and_then(Value::as_i64).unwrap_or_default(),
        ppid: process.get("ppid").and_then(Value::as_i64),
        started_at: safe_start(process.get("start")),
        owner_observed: flag(meta, "owner_observed"),
        exe_name: safe_exe_name(process.get("name")),
    };
    let evidence_state = parse_evidence_state(meta.get("evidence_state"));
    let reason_status = parse_reason_status(meta.get("reason_status"));
    let process_class = parse_process_class(meta.get("process_class"), protected);
    let stop_blocked_by_class = matches!(process_class, ProcessClass::OsRequired | ProcessClass::ProtectedWorkload);
    let evidence = meta.get("evidence").and_then(Value::as_array).into_iter().flatten().filter_map(safe_ref).collect::<Vec<_>>();
    let raw_cpu_samples = meta.get("cpu_samples").and_then(Value::as_array).cloned().unwrap_or_default();
    let metrics = raw_cpu_samples.iter().filter_map(metric).collect::<Vec<_>>();

    let matching_cpu_samples = raw_cpu_samples.len() >= 2
        && metrics.len() == raw_cpu_samples.len()
        && raw_cpu_samples
            .iter()
            .all(|sample| sample.get("pid").and_then(Value::as_i64) == Some(identity.pid) && safe_start(sample.get("start")) == identity.started_at)
        && metrics.iter().all(|sample| sample.value >= STOP_MIN_CPU)
        && metrics
            .windows(2)
            .all(|pair| pair[0].at < pair[1].at && pair[0].sample_window_ms == pair[1].sample_window_ms && pair[1].at - pair[0].at >= pair[1].sample_window_ms);
    let freshest_cpu_at = metrics.last().map(|sample| sample.at);
    let fresh_timestamps = now_ms > 0
        && evidence
            .iter()
            .all(|reference| reference.observed_at > 0 && reference.observed_at <= now_ms && now_ms - reference.observed_at <= MAX_EVIDENCE_AGE_MS)
        && metrics.iter().all(|sample| sample.at > 0 && sample.at <= now_ms && now_ms - sample.at <= MAX_EVIDENCE_AGE_MS)
        && evidence.iter().all(|reference| freshest_cpu_at.is_some_and(|at| reference.observed_at >= at));
    let service_scheduler_workload = flag(meta, "service_match") && flag(meta, "scheduler_match") && flag(meta, "workload_match");
    let protect_ledger_readable = flag(meta, "protect_ledger_readable");
    let node_hold_clear = node.get("hold").and_then(Value::as_bool) == Some(false);
    let node_shared_clear = node.get("shared").and_then(Value::as_bool) == Some(false);
    let collector_self_excluded = meta.get("collector_self").and_then(Value::as_bool) == Some(false);
    let identity_complete = identity.pid > 4
        && identity.ppid.is_some_and(|pid| pid > 0)
        && identity.started_at.is_some()
        && identity.owner_observed
        && identity.exe_name != "unknown";
    let fresh_confirmed_duplicate =
        evidence_state == EvidenceState::Observed && reason_status == ReasonStatus::Confirmed && process_class == ProcessClass::DuplicateCandidate;
    let all_preconditions_observed = fresh_confirmed_duplicate
        && identity_complete
        && !evidence.is_empty()
        && matching_cpu_samples
        && fresh_timestamps
        && service_scheduler_workload
        && protect_ledger_readable
        && node_hold_clear
        && node_shared_clear
        && collector_self_excluded
        && !stop_blocked_by_class;

    let mut needed_observation = Vec::new();
    if evidence_state != EvidenceState::Observed {
        needed_observation.push("fresh_evidence".into());
    }
    if reason_status != ReasonStatus::Confirmed {
        needed_observation.push("confirmed_cause".into());
    }
    if process_class != ProcessClass::DuplicateCandidate {
        needed_observation.push("service_scheduler_workload_attribution".into());
    }
    if !identity_complete {
        needed_observation.push("pid_name_start_owner_and_parent".into());
    }
    if evidence.is_empty() {
        needed_observation.push("evidence_reference".into());
    }
    if !matching_cpu_samples {
        needed_observation.push("two_cpu_time_samples".into());
    }
    if !fresh_timestamps {
        needed_observation.push("fresh_timestamp_and_clock_consistency".into());
    }
    if !service_scheduler_workload {
        needed_observation.push("service_scheduler_workload_match".into());
    }
    if !protect_ledger_readable {
        needed_observation.push("readable_protect_ledger".into());
    }
    if !node_hold_clear {
        needed_observation.push("explicit_node_hold_clear".into());
    }
    if !node_shared_clear {
        needed_observation.push("explicit_node_shared_clear".into());
    }
    if !collector_self_excluded {
        needed_observation.push("collector_self_excluded".into());
    }
    if stop_blocked_by_class {
        needed_observation.push("protected_workload_no_stop".into());
    }

    let (class, risk, proposed_action) = if all_preconditions_observed {
        (
            RemediationClass::RequiresApproval,
            StopRisk::High,
            Some(json!({
                "type": "kill-process",
                "label": "このプロセスを終了",
                "params": {
                    "pid": identity.pid,
                    "name": identity.exe_name,
                    "start": identity.started_at,
                    "min_cpu": STOP_MIN_CPU as i64
                }
            })),
        )
    } else if stop_blocked_by_class {
        (RemediationClass::NoAction, StopRisk::High, None)
    } else if !node_hold_clear || !node_shared_clear || !protect_ledger_readable {
        (RemediationClass::ManualOnly, StopRisk::High, None)
    } else {
        (RemediationClass::ExternalDependency, StopRisk::Unknown, None)
    };
    let blocked = !all_preconditions_observed;
    let can_auto_fix = can_auto_fix(class, risk, blocked, all_preconditions_observed);
    let reason_code = if all_preconditions_observed {
        "duplicate_confirmed"
    } else if stop_blocked_by_class {
        "protected_workload"
    } else {
        "insufficient_stop_evidence"
    };
    let reason_summary = if all_preconditions_observed {
        "二つの CPU 時間差分とサービス・スケジューラ・作業負荷の照合で重複候補を確認した。"
    } else if stop_blocked_by_class {
        "保護または OS 必須の作業負荷なので、この所見から停止候補は作らない。"
    } else {
        "CPU と名前だけでは不要なプロセスと断定できないため、停止候補は作らない。"
    };
    let alternatives = if all_preconditions_observed {
        vec!["approval_required_before_stop"]
    } else if stop_blocked_by_class {
        vec!["reobserve", "manual_investigation"]
    } else {
        vec!["reobserve", "build_on_another_machine", "manual_investigation", "approval_then_restart_candidate"]
    };
    TypedFinding {
        symptom: "repeated_high_cpu_samples".into(),
        evidence_state,
        process_class,
        identity,
        metrics,
        reason: json!({ "status": enum_value(reason_status), "code": reason_code, "summary": reason_summary }),
        evidence,
        needed_observation,
        remediation: json!({
            "class": enum_value(class),
            "stop_risk": enum_value(risk),
            "can_auto_fix": can_auto_fix,
            "alternatives": alternatives,
        }),
        proposed_action,
    }
}

/// 収集時刻を持たない既存の呼び出しでは、停止候補を fail-closed にする。
pub fn process_stop_finding(process: &Value, node: &Value, protected: bool) -> TypedFinding {
    process_stop_finding_at(process, node, protected, 0)
}
