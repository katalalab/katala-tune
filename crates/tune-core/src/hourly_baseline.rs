//! Pure observations of hourly percentage means; no host, action or speed verdict.
use crate::js::jnum;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
pub(crate) const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;
pub(crate) const MAX_CLOCK: i64 = 8_640_000_000_000_000;
const SAFE: f64 = 9_007_199_254_740_991.0;
pub(crate) fn integer(v: &Value) -> Option<i64> {
    v.as_f64().filter(|x| x.is_finite() && x.fract() == 0.0 && x.abs() <= SAFE).map(|x| x as i64)
}
fn quantile(mut xs: Vec<f64>, q: f64) -> f64 {
    xs.sort_by(f64::total_cmp);
    let pos = (xs.len() - 1) as f64 * q;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    xs[lo] + (xs[hi] - xs[lo]) * (pos - lo as f64)
}
fn profile_key(at: i64) -> i64 {
    ((at / DAY + 4) % 7) * 6 + (at / HOUR % 24) / 4
}
struct Point {
    at: i64,
    mean: f64,
}

pub fn analyze_hours(points: &Value, metric: &str, now: i64) -> Result<Value, &'static str> {
    let xs = points.as_array().filter(|_| matches!(metric, "cpu" | "mem" | "commit")).ok_or("invalid hourly baseline input/metric")?;
    if !(0..=MAX_CLOCK).contains(&now) {
        return Err("invalid baseline clock");
    }
    let end = now - now % HOUR;
    let since = (end - 28 * DAY).max(0);
    let until = (end - DAY).max(0);
    let mut report = json!({
        "metric":metric,"timezone":"UTC","state":"insufficient","reasons":[],"observations":[],
        "coverage":{"input_points":xs.len(),"accepted_hours":null,"history_hours":null,"history_days":null,"recent_hours":null,"matched_hours":null,"first_hours":null,"last_hours":null,"ignored_points":null,"conflicting_hours":null,"duplicate_points":null},
        "reference":{"since":since,"until":until,"profiles":[]},
        "evidence":{"elevation_assessed":false,"elevated_hours":null,"trend":{"assessed":false,"slope_pp_per_hour":null,"change_pp":null}}
    });
    if xs.len() > 4096 {
        report["reasons"] = json!(["input_limit"]);
        return Ok(report);
    }
    let (mut ignored, mut coarse) = (0, 0);
    type Variants = BTreeMap<(u64, i64), f64>;
    let mut buckets: BTreeMap<i64, (usize, Variants)> = BTreeMap::new();
    for p in xs {
        if p["resolution"] != "hour" {
            ignored += 1;
            if p["resolution"] == "day" {
                coarse += 1;
            }
            continue;
        }
        let at = integer(&p["at"]).filter(|t| *t >= since && *t < end && *t % HOUR == 0);
        let mean = p["mean"].as_f64().filter(|x| x.is_finite() && (0.0..=100.0).contains(x));
        let minutes = integer(&p["minutes"]).filter(|n| (30..=60).contains(n));
        let (Some(at), Some(mean), Some(minutes)) = (at, mean, minutes) else {
            ignored += 1;
            continue;
        };
        let mean = if mean == 0.0 { 0.0 } else { mean };
        let b = buckets.entry(at).or_default();
        b.0 += 1;
        b.1.insert((mean.to_bits(), minutes), mean);
    }
    let (mut conflicting, mut duplicates) = (0, 0);
    let mut valid = Vec::new();
    for (at, (count, variants)) in buckets {
        duplicates += count - variants.len();
        if variants.len() > 1 {
            conflicting += 1;
        } else {
            valid.push(Point { at, mean: *variants.values().next().unwrap() });
        }
    }
    let history: Vec<_> = valid.iter().filter(|p| p.at < until).collect();
    let recent: Vec<_> = valid.iter().filter(|p| p.at >= until).collect();
    let mut groups: BTreeMap<i64, (Vec<f64>, BTreeSet<i64>)> = BTreeMap::new();
    for p in &history {
        let g = groups.entry(profile_key(p.at)).or_default();
        g.0.push(p.mean);
        g.1.insert(p.at / DAY);
    }
    let mut refs = BTreeMap::new();
    let mut profiles = Vec::new();
    for (key, (values, days)) in groups {
        let mature = values.len() >= 8 && days.len() >= 3;
        let median = if mature { Some(quantile(values.clone(), 0.5)) } else { None };
        let p90 = if mature { Some(quantile(values.clone(), 0.9)) } else { None };
        profiles.push(json!({"weekday":key/6,"slot":key%6,"hours":values.len(),"days":days.len(),"mature":mature,
            "p10":if mature {jnum(quantile(values,0.1))}else{Value::Null},"median":median.map(jnum),"p90":p90.map(jnum)}));
        if let (Some(median), Some(p90)) = (median, p90) {
            refs.insert(key, (median, p90));
        }
    }
    report["reference"]["profiles"] = json!(profiles);
    let matched: Vec<_> =
        recent.iter().filter_map(|p| refs.get(&profile_key(p.at)).map(|(median, p90)| (p.at, p.mean - median, p.mean > p90 + 15.0))).collect();
    let first: Vec<_> = matched.iter().filter(|p| p.0 < end - 18 * HOUR).collect();
    let last: Vec<_> = matched.iter().filter(|p| p.0 >= end - 6 * HOUR).collect();
    let days = history.iter().map(|p| p.at / DAY).collect::<BTreeSet<_>>().len();
    report["coverage"] = json!({"input_points":xs.len(),"accepted_hours":valid.len(),"history_hours":history.len(),"history_days":days,
        "recent_hours":recent.len(),"matched_hours":matched.len(),"first_hours":first.len(),"last_hours":last.len(),"ignored_points":ignored,"conflicting_hours":conflicting,"duplicate_points":duplicates});
    let mut reasons = Vec::new();
    if history.len() < 72 {
        reasons.push("history_hours");
    }
    if days < 7 {
        reasons.push("history_days");
    }
    if recent.len() < 18 {
        reasons.push("recent_coverage");
    }
    if matched.len() < 18 {
        reasons.push("profile_coverage");
    }
    if first.len() < 4 || last.len() < 4 {
        reasons.push("endpoint_coverage");
    }
    if valid.is_empty() && coarse > 0 {
        reasons.push("hourly_data_required");
    }
    if !reasons.is_empty() {
        report["reasons"] = json!(reasons);
        return Ok(report);
    }
    let elevated_hours = last.iter().filter(|p| p.2).count();
    let elevated = elevated_hours >= 4;
    report["evidence"]["elevation_assessed"] = json!(true);
    report["evidence"]["elevated_hours"] = json!(elevated_hours);
    let mut rising = false;
    if metric != "cpu" {
        let mut slopes = Vec::new();
        for i in 0..matched.len() {
            for j in i + 1..matched.len() {
                slopes.push((matched[j].1 - matched[i].1) / ((matched[j].0 - matched[i].0) as f64 / HOUR as f64));
            }
        }
        let slope = quantile(slopes, 0.5);
        let change = quantile(last.iter().map(|p| p.1).collect(), 0.5) - quantile(first.iter().map(|p| p.1).collect(), 0.5);
        rising = slope >= 0.25 && change >= 8.0;
        report["evidence"]["trend"] = json!({"assessed":true,"slope_pp_per_hour":jnum(slope),"change_pp":jnum(change)});
    }
    let mut observations = Vec::new();
    if elevated {
        observations.push(json!({"kind":"elevated","metric":metric}));
    }
    if rising {
        observations.push(json!({"kind":"rising","metric":metric}));
    }
    report["observations"] = json!(observations);
    report["state"] = json!(match (elevated, rising) {
        (true, true) => "elevated_and_rising",
        (true, false) => "elevated",
        (false, true) => "rising",
        _ => "within_baseline",
    });
    Ok(report)
}
