//! 直近のライブ観測窓を積分する。欠測を埋めず、異なる取得源・セッションをつながない。
use serde_json::{Value, json};

pub fn observation(points: &[Value], settings: &Value) -> Value {
    let series = |key: &str, source_key: &str| -> Value {
        Value::Array(
            points
                .iter()
                .map(|p| json!({"t":p["t"],"seq":p["seq"],"epoch":p["epoch"],"source":p[source_key].as_str().unwrap_or(key),"watts":p[key]}))
                .collect(),
        )
    };
    let gpu = crate::power::integrate(&series("power_gpu_w", "power_gpu_source"), 1.0);
    let cpu = crate::power::integrate(&series("power_cpu_w", "power_cpu_source"), 1.0);
    let soc = crate::power::integrate(&series("power_soc_w", "power_soc_source"), 1.0);
    let whole: Vec<Value> = points
        .iter()
        .map(|p| {
            let s = crate::power::summary(
                &json!({"power":{"package_w":p["power_cpu_w"],"soc_w":p["power_soc_w"]},"gpus":[{"power_w":p["power_gpu_w"]}]}),
                settings,
            );
            let source = if p["power_soc_w"].is_number() {
                p["power_soc_source"].as_str().unwrap_or("soc").to_owned()
            } else {
                format!("{}+{}", p["power_cpu_source"].as_str().unwrap_or("cpu"), p["power_gpu_source"].as_str().unwrap_or("gpu"))
            };
            json!({"t":p["t"],"seq":p["seq"],"epoch":p["epoch"],"source":source,"watts":s["total_w"]})
        })
        .collect();
    let whole = crate::power::integrate(&json!(whole), 1.0);
    json!({"gpu":gpu,"cpu":cpu,"soc":soc,"whole":whole,"first_at":points.first().map(|p|&p["t"]),"last_at":points.last().map(|p|&p["t"]),"samples":points.len(),"settings":settings})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observed_gpu_energy_is_separate_from_whole_system_and_gap_coverage() {
        let points = vec![
            json!({"t":0,"seq":0,"epoch":"test","power_gpu_w":100}),
            json!({"t":1000,"seq":1,"epoch":"test","power_gpu_w":100}),
            json!({"t":10000,"seq":2,"epoch":"test","power_gpu_w":100}),
        ];
        let r = observation(&points, &json!({"base_w":0,"psu_efficiency":1}));
        assert_eq!(r["gpu"]["covered_seconds"], 1.0);
        assert_eq!(r["gpu"]["duration_seconds"], 10.0);
        assert_eq!(r["whole"]["covered_seconds"], 0.0);
        assert_eq!(r["cpu"]["covered_seconds"], 0.0);
        assert_eq!(r["samples"], 3);
    }
    #[test]
    fn soc_energy_does_not_add_overlapping_cpu_gpu_and_respects_source_changes() {
        let points = vec![
            json!({"t":0,"seq":0,"epoch":"a","power_soc_w":30,"power_cpu_w":20,"power_gpu_w":10,"power_soc_source":"IOReport"}),
            json!({"t":1000,"seq":1,"epoch":"a","power_soc_w":30,"power_cpu_w":20,"power_gpu_w":10,"power_soc_source":"IOReport"}),
            json!({"t":2000,"seq":2,"epoch":"a","power_soc_w":30,"power_cpu_w":20,"power_gpu_w":10,"power_soc_source":"new sensor"}),
        ];
        let r = observation(&points, &json!({"base_w":0,"psu_efficiency":1}));
        assert_eq!(r["soc"]["covered_seconds"], 1.0);
        assert_eq!(r["whole"]["covered_seconds"], 1.0);
        assert_eq!(r["whole"]["kwh"], json!(30.0 / 3_600_000.0));
    }
}
