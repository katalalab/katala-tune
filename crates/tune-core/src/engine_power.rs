//! 電力の計算条件は私有 DB の meta に保存する。センサーの値と計算条件を混ぜない。
use crate::engine::Engine;
use serde_json::{Value, json};
const KEY: &str = "power_settings_v1";

pub fn validate(patch: &Value) -> Result<(), String> {
    let m = patch.as_object().ok_or("計算条件はオブジェクトで指定してください")?;
    for (k, v) in m {
        if k == "currency" {
            if !v.is_null() && !v.as_str().is_some_and(|s| s.len() == 3 && s.bytes().all(|c| c.is_ascii_uppercase())) {
                return Err("通貨は3文字のコードで指定してください".into());
            }
            continue;
        }
        let (lo, hi) = match k.as_str() {
            "base_w" => (0.0, 10000.0),
            "psu_efficiency" => (f64::MIN_POSITIVE, 1.0),
            "hours" => (0.0, 8784.0),
            "rate_per_kwh" => (0.0, 1000000.0),
            _ => return Err(format!("計算条件が不正です: {k}")),
        };
        if !v.is_null() && !v.as_f64().is_some_and(|n| n.is_finite() && n >= lo && n <= hi) {
            return Err(format!("計算条件が不正です: {k}"));
        }
    }
    Ok(())
}

impl Engine {
    pub fn power_observation(&self, node_id: &str, points: &[Value], save: bool) -> Result<Value, String> {
        let cfg = self.reload_config();
        if cfg.node(node_id).is_none() {
            return Err("台帳にない機体です".into());
        }
        let settings = self.with_db(|d| d.get_meta(KEY))?.unwrap_or_else(|| json!({}));
        let s = settings.get(node_id).cloned().unwrap_or_else(|| json!({}));
        // Live の保持上限と同じ300点。画面から送られた値は使わない。
        let from = points.len().saturating_sub(300);
        let result = crate::power_observation::observation(&points[from..], &s);
        if save && result["samples"].as_u64().unwrap_or(0) > 0 {
            self.with_db(|d| {
                let mut all = d.get_meta("power_observations_v1")?.and_then(|v| v.as_object().cloned()).unwrap_or_default();
                all.retain(|id, _| cfg.node(id).is_some());
                all.insert(node_id.into(), result.clone());
                d.set_meta("power_observations_v1", &Value::Object(all))
            })?;
        }
        Ok(result)
    }
    pub fn power_settings(&self, node_id: &str, patch: &Value) -> Result<Value, String> {
        validate(patch)?;
        let cfg = self.reload_config();
        if let Some(err) = self.config_error() {
            return Err(err);
        }
        if cfg.node(node_id).is_none() {
            return Err("台帳にない機体です".into());
        }
        self.with_db(|d| {
            let mut all = d.get_meta(KEY)?.and_then(|v| v.as_object().cloned()).unwrap_or_default();
            let mut s = all.get(node_id).and_then(|v| v.as_object().cloned()).unwrap_or_default();
            s.extend(patch.as_object().unwrap().clone());
            let value = Value::Object(s);
            all.insert(node_id.into(), value.clone());
            d.set_meta(KEY, &Value::Object(all))?;
            Ok(value)
        })
    }
    pub fn power_report(&self) -> Result<Value, String> {
        let cfg = self.reload_config();
        let snapshots = self.last()?;
        let settings = self.with_db(|d| d.get_meta(KEY))?.unwrap_or_else(|| json!({}));
        let saved = self.with_db(|d| d.get_meta("power_observations_v1"))?.unwrap_or_else(|| json!({}));
        let now = crate::db::now_ms();
        let mut rows = Vec::new();
        let mut sum = 0.0;
        let mut count = 0;
        for n in &cfg.nodes {
            let r = snapshots.iter().find(|r| r["node_id"] == n.id && r["ok"] == true);
            let at = r.and_then(|v| v["at"].as_i64());
            let fresh = at.is_some_and(|at| (-5000..=300000).contains(&(now - at)));
            let s = settings.get(&n.id).cloned().unwrap_or_else(|| json!({}));
            let data = r.map(|v| v["data"].clone()).unwrap_or_else(|| json!({}));
            let summary = crate::power::summary(&data, &s);
            if fresh && let Some(w) = summary["total_w"].as_f64() {
                sum += w;
                count += 1;
            }
            rows.push(json!({ "node_id": n.id, "at": at, "fresh": fresh, "settings": s, "data": data, "summary": summary,"observation":saved.get(&n.id) }));
        }
        Ok(
            json!({ "nodes": rows, "fleet": { "expected": cfg.nodes.len(), "available": count, "available_w": if count > 0 { Some(sum) } else { None }, "total_w": if count > 0 && count == cfg.nodes.len() { Some(sum) } else { None } } }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calculation_conditions_reject_unsafe_and_unknown_inputs() {
        assert!(validate(&json!({"base_w":0,"psu_efficiency":1,"hours":0,"rate_per_kwh":0,"currency":"JPY"})).is_ok());
        for v in [
            json!({"psu_efficiency":0}),
            json!({"psu_efficiency":1.1}),
            json!({"hours":-1}),
            json!({"rate_per_kwh":"30"}),
            json!({"currency":"<b>"}),
            json!({"wall_w":100}),
            json!([]),
        ] {
            assert!(validate(&v).is_err(), "{v}");
        }
    }
    #[test]
    fn private_conditions_survive_reopen_and_unknown_nodes_are_refused() {
        use std::sync::Arc;
        let tmp = std::env::temp_dir().join(format!("katala-tune-power-settings-{}-{}", std::process::id(), crate::db::now_ms()));
        std::fs::create_dir_all(&tmp).unwrap();
        let config = tmp.join("nodes.json");
        std::fs::write(&config, r#"{"nodes":[{"id":"test-node","alias":"unused","os":"macos"}]}"#).unwrap();
        let e = Engine::open(config.clone(), tmp.join("data"), Arc::new(crate::engine::NoHost)).unwrap();
        assert!(e.power_settings("missing", &json!({"hours":1})).is_err());
        e.power_settings("test-node", &json!({"hours":1,"currency":"JPY"})).unwrap();
        e.power_settings("test-node", &json!({"base_w":0})).unwrap();
        drop(e);
        let e = Engine::open(config.clone(), tmp.join("data"), Arc::new(crate::engine::NoHost)).unwrap();
        let r = e.power_report().unwrap();
        assert_eq!(r["nodes"][0]["settings"], json!({"hours":1,"currency":"JPY","base_w":0}));
        assert_eq!(r["fleet"]["total_w"], Value::Null);
        std::fs::write(config, "invalid").unwrap();
        assert!(e.power_settings("test-node", &json!({"hours":2})).is_err());
        drop(e);
        std::fs::remove_dir_all(tmp).unwrap();
    }
}
