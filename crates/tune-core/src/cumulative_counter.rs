//! Cumulative-only arithmetic; no OS mapping or host/boot identity proof.
use serde_json::{Map, Value, json};

const U64_MAX: &str = "18446744073709551615";
const FIELDS: [&str; 9] = ["host", "boot", "source", "definition", "unit", "scope", "counter_epoch", "clock", "mode"];

fn keys(value: &Value, fields: &[&str]) -> bool {
    value.as_object().is_some_and(|map| map.len() == fields.len() && map.keys().all(|key| fields.contains(&key.as_str())))
}

fn uint(value: &Value, positive: bool) -> Option<u64> {
    let s = value.as_str()?;
    // Validate bounded canonical grammar AND u64 range before parsing.
    if s.is_empty()
        || s.len() > 20
        || !s.bytes().all(|b| b.is_ascii_digit())
        || (s.len() > 1 && s.starts_with('0'))
        || (s.len() == 20 && s > U64_MAX)
        || (positive && s == "0")
    {
        return None;
    }
    s.parse().ok()
}

struct Sample<'a> {
    count: u64,
    tick: u64,
    frequency: u64,
    descriptor: &'a Map<String, Value>,
}

fn validate(value: &Value) -> Result<Sample<'_>, &'static str> {
    if !keys(value, &["count", "tick", "frequency", "descriptor"]) {
        return Err("invalid");
    }
    let count = uint(&value["count"], false).ok_or("invalid")?;
    let tick = uint(&value["tick"], false).ok_or("invalid")?;
    let frequency = uint(&value["frequency"], true).ok_or("invalid")?;
    let d = value["descriptor"].as_object().ok_or("invalid")?;
    for field in ["host", "boot"] {
        if d.get(field).is_none_or(Value::is_null) || d[field].as_str() == Some("") {
            return Err("unknown_provenance");
        }
    }
    if !keys(&value["descriptor"], &FIELDS) {
        return Err("invalid");
    }
    for field in FIELDS {
        let s = d[field].as_str().ok_or("invalid")?;
        if s.is_empty() || s.len() > 128 || !s.as_bytes()[0].is_ascii_alphanumeric() || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b)) {
            return Err("invalid");
        }
    }
    if d["mode"] != "cumulative" {
        return Err("unsupported_mode");
    }
    if d["scope"] != "host" {
        return Err("unsupported_scope");
    }
    Ok(Sample { count, tick, frequency, descriptor: d })
}

fn window_limits(value: &Value) -> Result<Option<(u64, u64)>, ()> {
    if value.is_null() {
        return Ok(None);
    }
    if !keys(value, &["min_elapsed_ticks", "max_elapsed_ticks"]) {
        return Err(());
    }
    let min = uint(&value["min_elapsed_ticks"], true).ok_or(())?;
    let max = uint(&value["max_elapsed_ticks"], true).ok_or(())?;
    if min > max { Err(()) } else { Ok(Some((min, max))) }
}

fn decimal(delta: u64, ticks: u64, frequency: u64) -> String {
    // Each operand is u64, so their product fits u128. Scale only the
    // remainder (< u64), never the full product, to avoid overflow.
    let numerator = u128::from(delta) * u128::from(frequency);
    let divisor = u128::from(ticks);
    let mut whole = numerator / divisor;
    let mut fraction = ((numerator % divisor) * 1_000_000 + divisor / 2) / divisor;
    if fraction == 1_000_000 {
        whole += 1;
        fraction = 0;
    }
    format!("{whole}.{fraction:06}")
}

fn result(reason: &str, baseline: &Value, rate: Value) -> Value {
    json!({"baseline":baseline,"reason":reason,"rate":rate})
}

/// Advance one counter. Missing/invalid/time/window boundaries clear history;
/// valid resets and descriptor boundaries establish a fresh baseline.
/// A matching descriptor cannot supply missing host/boot evidence for callers.
pub fn step(previous: &Value, sample: &Value, window: &Value) -> Value {
    let none = Value::Null;
    let Ok(limits) = window_limits(window) else {
        return result("invalid_window", &none, none.clone());
    };
    if sample.is_null() {
        return result("missing", &none, none.clone());
    }
    let current = match validate(sample) {
        Ok(current) => current,
        Err(reason) => return result(reason, &none, none.clone()),
    };
    if previous.is_null() {
        return result("first", sample, none);
    }
    let Ok(old) = validate(previous) else {
        return result("invalid_baseline", sample, none);
    };
    if old.descriptor["clock"] != current.descriptor["clock"] {
        return result("clock_changed", &none, none.clone());
    }
    if old.frequency != current.frequency {
        return result("frequency_changed", &none, none.clone());
    }
    if FIELDS.iter().any(|field| old.descriptor[*field] != current.descriptor[*field]) {
        return result("descriptor_changed", sample, none);
    }
    let Some(ticks) = current.tick.checked_sub(old.tick).filter(|t| *t > 0) else {
        return result("invalid_time", &none, none.clone());
    };
    if limits.is_some_and(|(min, max)| ticks < min || ticks > max) {
        return result("window", &none, none.clone());
    }
    let Some(delta) = current.count.checked_sub(old.count) else {
        return result("reset", sample, none);
    };
    result(
        "ok",
        sample,
        json!({
            "delta_count":delta.to_string(), "elapsed_ticks":ticks.to_string(),
            "frequency":current.frequency.to_string(),
            "decimal":decimal(delta, ticks, current.frequency),
        }),
    )
}
