use serde_json::Value;

use super::TopologyLimits;

#[derive(Clone, Debug, Default)]
pub struct TopologyConfig {
    pub enabled: bool,
    pub owners: Vec<String>,
    pub github_enabled: bool,
    pub github_cache_minutes: u64,
    pub limits: TopologyLimits,
}

impl TopologyConfig {
    pub fn from_value(value: Option<&Value>) -> Result<Self, String> {
        let value = value.unwrap_or(&Value::Null);
        if !value.is_null() && !value.is_object() {
            return Err("topology must be an object".into());
        }
        let empty_owners = Value::Array(vec![]);
        let owners_value = value.get("owners").unwrap_or(&empty_owners);
        let owners = owners_value
            .as_array()
            .ok_or("topology.owners must be an array")?
            .iter()
            .map(|owner| owner.as_str().map(str::trim).filter(|owner| !owner.is_empty()).map(str::to_owned).ok_or("topology.owners contains an invalid owner"))
            .collect::<Result<Vec<_>, _>>()?;
        let mut limits = TopologyLimits::default();
        let empty_limits = Value::Object(Default::default());
        let input = value.get("limits").unwrap_or(&empty_limits);
        let input = input.as_object().ok_or("topology.limits must be an object")?;
        macro_rules! limit {
            ($field:ident, $maximum:expr) => {
                if let Some(value) = input.get(stringify!($field)) {
                    limits.$field = value
                        .as_u64()
                        .and_then(|number| usize::try_from(number).ok())
                        .filter(|number| *number > 0 && *number <= $maximum)
                        .ok_or(concat!("invalid topology limit: ", stringify!($field)))?;
                }
            };
        }
        limit!(roots_per_environment, 128);
        limit!(depth, 64);
        limit!(entries_per_environment, 1_000_000);
        limit!(repositories_per_environment, 100_000);
        limit!(manifests_per_environment, 1_000_000);
        limit!(file_bytes, 16 * 1024 * 1024);
        limit!(workflow_bytes, 16 * 1024 * 1024);
        limit!(total_bytes_per_environment, 1024 * 1024 * 1024);
        limit!(stdout_bytes, 64 * 1024 * 1024);
        limit!(stderr_bytes, 4 * 1024 * 1024);
        limit!(github_requests_per_run, 4096);
        macro_rules! seconds {
            ($field:ident) => {
                if let Some(value) = input.get(stringify!($field)) {
                    limits.$field =
                        value.as_u64().filter(|number| *number > 0 && *number <= 3_600).ok_or(concat!("invalid topology limit: ", stringify!($field)))?;
                }
            };
        }
        seconds!(local_timeout_seconds);
        seconds!(remote_timeout_seconds);
        if limits.workflow_bytes > limits.file_bytes || limits.file_bytes > limits.total_bytes_per_environment {
            return Err("topology byte limits are inconsistent".into());
        }
        let github = value.get("github").unwrap_or(&Value::Null);
        if !github.is_null() && !github.is_object() {
            return Err("topology.github must be an object".into());
        }
        let github_cache_minutes = github
            .get("cache_minutes")
            .map(|v| v.as_u64().filter(|n| *n > 0 && *n <= 1_440).ok_or("invalid topology.github.cache_minutes"))
            .transpose()?
            .unwrap_or(15);
        Ok(Self {
            enabled: value.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            owners,
            github_enabled: github.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            github_cache_minutes,
            limits,
        })
    }
}
