use serde_json::Value;
use serde_saphyr::{DuplicateKeyPolicy, Error};

use crate::topology::{Failure, FailureCode, TopologyLimits, Workflow, WorkflowJob};

const MAX_ALIASES: usize = 64;
const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 20_000;
const MAX_FIELD_BYTES: usize = 8 * 1024;

fn bounded_string(value: &str) -> Result<String, Failure> {
    (value.len() <= MAX_FIELD_BYTES).then(|| value.to_owned()).ok_or_else(|| Failure::new(FailureCode::LimitReached, "workflow field budget exceeded"))
}

fn strings(value: Option<&Value>) -> Result<Vec<String>, Failure> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::String(value)) => Ok(vec![bounded_string(value)?]),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| value.as_str().ok_or_else(|| Failure::new(FailureCode::Unsupported, "workflow string list is invalid")).and_then(bounded_string))
            .collect(),
        Some(_) => Err(Failure::new(FailureCode::Unsupported, "workflow string list is invalid")),
    }
}

fn validate_field_bounds(value: &Value) -> Result<(), Failure> {
    match value {
        Value::String(value) => {
            bounded_string(value)?;
        }
        Value::Array(values) => {
            for value in values {
                validate_field_bounds(value)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                bounded_string(key)?;
                validate_field_bounds(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn workflow_failure(error: Error) -> Failure {
    let location = error.location();
    let code = workflow_failure_code(&error);
    let message = match code {
        FailureCode::LimitReached => "workflow parser budget exceeded",
        FailureCode::Unsupported => "workflow feature is unsupported",
        _ => "workflow parse failed",
    };
    Failure::at(code, message, location.map(|location| location.line()), location.map(|location| location.column()))
}

fn workflow_failure_code(error: &Error) -> FailureCode {
    match error {
        Error::WithSnippet { error, .. } | Error::AliasError { error, .. } => workflow_failure_code(error),
        Error::Budget { .. }
        | Error::AliasReplayCounterOverflow { .. }
        | Error::AliasReplayLimitExceeded { .. }
        | Error::AliasExpansionLimitExceeded { .. }
        | Error::AliasReplayStackDepthExceeded { .. } => FailureCode::LimitReached,
        Error::UnsupportedTag { .. } | Error::TaggedScalarCannotDeserializeIntoString { .. } => FailureCode::Unsupported,
        _ => FailureCode::ParseError,
    }
}

pub fn parse_workflow(text: &str, limits: &TopologyLimits) -> Result<Workflow, Failure> {
    if text.len() > limits.workflow_bytes {
        return Err(Failure::new(FailureCode::LimitReached, "workflow byte budget exceeded"));
    }
    let options = serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_documents: 1,
            max_aliases: MAX_ALIASES,
            max_anchors: MAX_ALIASES,
            max_depth: MAX_DEPTH,
            max_nodes: MAX_NODES,
        },
        alias_limits: serde_saphyr::alias_limits! {
            max_total_replayed_events: MAX_NODES,
            max_replay_stack_depth: MAX_DEPTH,
            max_alias_expansions_per_anchor: MAX_ALIASES,
        },
        duplicate_keys: DuplicateKeyPolicy::Error,
        strict_booleans: true,
        reject_unsupported_tags: true,
    };
    let value: Value = serde_saphyr::from_str_with_options(text, options).map_err(workflow_failure)?;
    validate_field_bounds(&value)?;
    let document = value.as_object().ok_or_else(|| Failure::new(FailureCode::Unsupported, "workflow document must be a mapping"))?;
    let triggers = match document.get("on") {
        Some(Value::String(value)) => vec![bounded_string(value)?],
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| value.as_str().ok_or_else(|| Failure::new(FailureCode::Unsupported, "workflow trigger is invalid")).and_then(bounded_string))
            .collect::<Result<Vec<_>, _>>()?,
        Some(Value::Object(values)) if !values.is_empty() => values.keys().map(|key| bounded_string(key)).collect::<Result<Vec<_>, _>>()?,
        _ => return Err(Failure::new(FailureCode::Unsupported, "workflow trigger is missing or invalid")),
    };
    let mut trigger_filters = std::collections::BTreeMap::new();
    if let Some(events) = document.get("on").and_then(Value::as_object) {
        for (event, declaration) in events {
            let mut filters = std::collections::BTreeMap::new();
            if let Some(declaration) = declaration.as_object() {
                for key in ["branches", "branches-ignore", "tags", "tags-ignore", "paths", "paths-ignore"] {
                    let values = strings(declaration.get(key))?;
                    if !values.is_empty() {
                        filters.insert(key.to_owned(), values);
                    }
                }
            }
            if !filters.is_empty() {
                trigger_filters.insert(event.clone(), filters);
            }
        }
    }
    let mut coverage = Vec::new();
    let mut jobs = Vec::new();
    let map = document
        .get("jobs")
        .and_then(Value::as_object)
        .filter(|jobs| !jobs.is_empty())
        .ok_or_else(|| Failure::new(FailureCode::Unsupported, "workflow jobs are missing or invalid"))?;
    for (id, raw) in map {
        let raw = raw.as_object().ok_or_else(|| Failure::new(FailureCode::Unsupported, "workflow job is invalid"))?;
        let job_uses = raw
            .get("uses")
            .map(|value| value.as_str().ok_or_else(|| Failure::new(FailureCode::Unsupported, "workflow job uses is invalid")).and_then(bounded_string))
            .transpose()?;
        let step_uses = match raw.get("steps") {
            None => Vec::new(),
            Some(Value::Array(steps)) => steps
                .iter()
                .map(|step| {
                    let step = step.as_object().ok_or_else(|| Failure::new(FailureCode::Unsupported, "workflow step is invalid"))?;
                    step.get("uses")
                        .map(|value| {
                            value.as_str().ok_or_else(|| Failure::new(FailureCode::Unsupported, "workflow step uses is invalid")).and_then(bounded_string)
                        })
                        .transpose()
                })
                .collect::<Result<Vec<_>, Failure>>()?
                .into_iter()
                .flatten()
                .collect(),
            Some(_) => return Err(Failure::new(FailureCode::Unsupported, "workflow steps are invalid")),
        };
        let mut uses = job_uses.iter().cloned().collect::<Vec<_>>();
        uses.extend(step_uses.iter().cloned());
        if raw.get("strategy").is_some() && !coverage.contains(&"matrix strategy is not evaluated".into()) {
            coverage.push("matrix strategy is not evaluated".into());
        }
        if raw.get("steps").and_then(Value::as_array).is_some_and(|steps| steps.iter().any(|step| step.get("if").is_some()))
            && !coverage.contains(&"step conditions are not evaluated".into())
        {
            coverage.push("step conditions are not evaluated".into());
        }
        jobs.push(WorkflowJob {
            id: bounded_string(id)?,
            needs: strings(raw.get("needs"))?,
            uses,
            job_uses,
            step_uses,
            if_condition: raw
                .get("if")
                .map(|value| value.as_str().ok_or_else(|| Failure::new(FailureCode::Unsupported, "workflow condition is invalid")).and_then(bounded_string))
                .transpose()?,
            runs_on: strings(raw.get("runs-on"))?,
            environment: raw
                .get("environment")
                .map(|value| {
                    value
                        .get("name")
                        .or(Some(value))
                        .and_then(Value::as_str)
                        .ok_or_else(|| Failure::new(FailureCode::Unsupported, "workflow environment is invalid"))
                        .and_then(bounded_string)
                })
                .transpose()?,
        });
    }
    Ok(Workflow { triggers, trigger_filters, coverage, jobs })
}
