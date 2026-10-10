use serde_json::{Value, json};

fn watts(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64).filter(|value| value.is_finite() && *value >= 0.0)
}

fn setting_watts(settings: &Value, key: &str) -> Option<f64> {
    watts(settings.get(key))
}

fn display_currency(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).filter(|value| !value.is_empty() && value.chars().count() <= 8).map(str::to_owned)
}

pub fn summary(snapshot: &Value, settings: &Value) -> Value {
    let power = snapshot.get("power").and_then(Value::as_object);
    let gpu_w: Option<f64> = snapshot
        .get("gpus")
        .and_then(Value::as_array)
        .and_then(|rows| rows.iter().map(|gpu| watts(gpu.get("power_w"))).collect::<Option<Vec<_>>>().map(|values| values.into_iter().sum()));
    let soc_w = power.and_then(|power| watts(power.get("soc_w")));
    let cpu_w = if soc_w.is_none() { power.and_then(|power| watts(power.get("package_w"))) } else { None };
    let wall_w = power.and_then(|power| watts(power.get("wall_w")));
    let base_w = setting_watts(settings, "base_w");
    let efficiency = settings.get("psu_efficiency").and_then(Value::as_f64).filter(|value| value.is_finite() && *value > 0.0 && *value <= 1.0);
    let mut missing = Vec::new();
    let (total_w, kind, source) = if let Some(wall_w) = wall_w {
        (Some(wall_w), "measured", Some("wall"))
    } else if let (Some(soc_w), Some(base_w), Some(efficiency)) = (soc_w, base_w, efficiency) {
        (Some((soc_w + base_w) / efficiency), "estimated", Some("soc"))
    } else if let (Some(cpu_w), Some(gpu_w), Some(base_w), Some(efficiency)) = (cpu_w, gpu_w, base_w, efficiency) {
        (Some((cpu_w + gpu_w + base_w) / efficiency), "estimated", Some("components"))
    } else {
        if soc_w.is_none() && gpu_w.is_none() {
            missing.push("gpu_w");
        }
        if soc_w.is_none() && cpu_w.is_none() {
            missing.push("cpu_w");
        }
        if base_w.is_none() {
            missing.push("base_w");
        }
        if efficiency.is_none() {
            missing.push("psu_efficiency");
        }
        (None, "missing", None)
    };
    let hours = setting_watts(settings, "hours");
    let rate = setting_watts(settings, "rate_per_kwh");
    let projected_kwh = total_w.zip(hours).map(|(watts, hours)| watts * hours / 1000.0);
    let projected_cost = projected_kwh.zip(rate).map(|(kwh, rate)| kwh * rate);
    json!({
        "gpu_w": if soc_w.is_none() { gpu_w } else { None },
        "cpu_w": cpu_w,
        "soc_w": soc_w,
        "wall_w": wall_w,
        "total_w": total_w,
        "kind": kind,
        "source": source,
        "missing": missing,
        "projected_kwh": projected_kwh,
        "projected_cost": projected_cost,
        "currency": display_currency(settings.get("currency")),
    })
}

#[derive(Clone)]
struct Point {
    t: f64,
    seq: i64,
    epoch: String,
    source: String,
    watts: Option<f64>,
}

fn point(value: &Value) -> Option<Point> {
    let object = value.as_object()?;
    let t = object.get("t").and_then(Value::as_f64).filter(|value| value.is_finite())?;
    let seq = object.get("seq").and_then(Value::as_i64)?;
    let epoch = object.get("epoch").and_then(Value::as_str).filter(|value| !value.is_empty())?.to_owned();
    let source = object.get("source").and_then(Value::as_str).filter(|value| !value.is_empty())?.to_owned();
    Some(Point { t, seq, epoch, source, watts: watts(object.get("watts")) })
}

pub fn integrate(points: &Value, interval_seconds: f64) -> Value {
    let interval = if interval_seconds.is_finite() && interval_seconds > 0.0 { interval_seconds } else { 1.0 };
    let mut kwh = 0.0;
    let mut covered_seconds = 0.0;
    let mut duration_seconds = 0.0;
    let mut previous: Option<Point> = None;
    for raw in points.as_array().into_iter().flatten() {
        let Some(current) = point(raw) else {
            previous = None;
            continue;
        };
        if let Some(prior) = &previous
            && prior.epoch == current.epoch
            && prior.source == current.source
            && current.seq > prior.seq
            && current.t > prior.t
        {
            let seconds = (current.t - prior.t) / 1000.0;
            duration_seconds += seconds;
            if seconds <= interval * 3.0
                && let (Some(before), Some(after)) = (prior.watts, current.watts)
            {
                covered_seconds += seconds;
                kwh += (before + after) * seconds / 7_200_000.0;
            }
        }
        previous = Some(current);
    }
    json!({
        "kwh": kwh,
        "covered_seconds": covered_seconds,
        "duration_seconds": duration_seconds,
        "coverage": if duration_seconds == 0.0 { 0.0 } else { covered_seconds / duration_seconds },
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value};

    const FIXTURE: &str = include_str!("../../../test/fixtures/power.json");

    fn normalise_numbers(value: &Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(values.iter().map(normalise_numbers).collect()),
            Value::Object(values) => Value::Object(values.iter().map(|(key, value)| (key.clone(), normalise_numbers(value))).collect::<Map<_, _>>()),
            Value::Number(value) => serde_json::json!(value.as_f64().unwrap()),
            _ => value.clone(),
        }
    }

    #[test]
    fn summary_matches_shared_fixture() {
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        for row in fixture["summary"].as_array().unwrap() {
            assert_eq!(normalise_numbers(&super::summary(&row["snapshot"], &row["settings"])), normalise_numbers(&row["expected"]));
        }
    }

    #[test]
    fn integration_matches_shared_fixture() {
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        for row in fixture["integration"].as_array().unwrap() {
            assert_eq!(normalise_numbers(&super::integrate(&row["points"], row["interval_seconds"].as_f64().unwrap())), normalise_numbers(&row["expected"]));
        }
    }
}
