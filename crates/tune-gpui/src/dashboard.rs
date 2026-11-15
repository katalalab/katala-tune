//! Saved Dashboard data stays separate from generic record panels and UI Live.
use crate::model::{self, NodeView};
use serde_json::Value;
use tune_core::engine::Engine;

#[derive(Clone, Debug)]
pub struct DashboardNode {
    pub saved: Value,
    pub observation: NodeView,
}
#[derive(Clone, Debug)]
pub struct DashboardView {
    pub saved: Value,
    pub nodes: Vec<DashboardNode>,
    pub total_nodes: usize,
}

pub fn load_saved(engine: &Engine, minutes: i64) -> Result<DashboardView, String> {
    let cfg = engine.reload_config();
    if let Some(error) = engine.config_error() {
        return Err(error);
    }
    let ledger: Vec<_> = cfg.nodes.iter().map(tune_core::nodes::Node::public).collect();
    let observations = model::build(&ledger, &engine.last()?);
    let mut saved = tune_core::monitor::dashboard(engine, None, minutes)?;
    let rows = saved["nodes"].take().as_array().cloned().unwrap_or_default();
    let total_nodes = rows.len();
    // Only the displayed node set is bounded; core reads and series are intact.
    let nodes = rows.into_iter().zip(observations).take(200).map(|(saved, observation)| DashboardNode { saved, observation }).collect();
    Ok(DashboardView { saved, nodes, total_nodes })
}

struct CurrentValues {
    values: Value,
    at: Option<i64>,
    source: &'static str,
}

fn current_values(node: &DashboardNode, brief: Option<&Value>, now: i64) -> CurrentValues {
    if let Some(point) = brief.and_then(|b| b.get("point"))
        && let Some(t) = point["t"].as_i64()
        && now.saturating_sub(t) <= 10_000
    {
        return CurrentValues { values: point.clone(), at: Some(t), source: "Live（10秒以内）" };
    }
    let latest = &node.saved["latest"];
    let at = latest["minute"].as_i64();
    let values = latest["v"]
        .as_object()
        .map(|v| {
            v.iter()
                .filter_map(|(k, v)| {
                    // Saved top lists are [name,value], not a mean/max metric pair.
                    if k.starts_with("top_") {
                        return None;
                    }
                    Some((k.clone(), v.get(usize::from(k == "gtemp")).cloned().unwrap_or(Value::Null)))
                })
                .collect::<serde_json::Map<_, _>>()
        })
        .unwrap_or_default();
    let source = match at {
        None => "未取得",
        Some(t) if now.saturating_sub(t) > 180_000 => "古い保存分平均（温度は最大）",
        _ => "保存分平均（温度は最大）",
    };
    CurrentValues { values: Value::Object(values), at, source }
}

// Rendering reads only the shared Live memory. It never starts or renews a stream.
use crate::native::palette;
use gpui::{FontWeight, Hsla, div, prelude::*, px, relative};
use tune_core::live::Live;

fn number(v: &Value, unit: &str) -> String {
    v.as_f64().filter(|n| n.is_finite()).map(|n| format!("{n:.1}{unit}")).unwrap_or_else(|| "未取得".into())
}
fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "はい" } else { "いいえ" }.into(),
        Value::Number(n) => n.to_string(),
        _ => "未取得".into(),
    }
}
fn field(label: &str, value: impl Into<String>) -> gpui::Div {
    div()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(palette::card())
        .child(div().text_xs().text_color(palette::muted()).child(label.to_owned()))
        .child(div().text_sm().child(value.into()))
}
fn chart_points(series: &Value, key: &str, since: i64, until: i64) -> Vec<(f32, Option<f64>)> {
    series["t"]
        .as_array()
        .map(|times| {
            times
                .iter()
                .enumerate()
                .filter_map(|(i, t)| {
                    let t = t.as_i64()?;
                    let x = (t.saturating_sub(since) as f64 / until.saturating_sub(since).max(1) as f64).clamp(0., 1.) as f32;
                    Some((x, series[key][i].as_f64()))
                })
                .collect()
        })
        .unwrap_or_default()
}
fn series_chart(node: &DashboardNode, key: &str, label: &str, color: Hsla, since: i64, until: i64) -> gpui::Div {
    let mut chart = div().h(px(32.)).relative().overflow_hidden();
    if node.saved["series"][key].is_array() {
        for (x, value) in chart_points(&node.saved["series"], key, since, until) {
            let bar = div().absolute().left(relative(x)).bottom_0().w(px(2.)).h(px(value.map_or(32., |v| (v.clamp(0., 100.) as f32 / 100. * 32.).max(1.))));
            chart = chart.child(if value.is_some() { bar.bg(color) } else { bar.border_l_1().border_color(palette::line()) });
        }
    } else {
        chart = chart.child(div().text_xs().text_color(palette::muted()).child("未取得"));
    }
    div().flex_1().min_w(px(100.)).child(div().text_xs().text_color(color).child(label.to_owned())).child(chart)
}

pub fn render_dashboard(view: &DashboardView, live: &Live, now: i64) -> gpui::Div {
    let saved = &view.saved;
    let mut body = div().flex().flex_col().gap_4().p_5()
        .child(div().text_sm().child(format!("{}台表示 / 全{}台 · 省略{}台（UI表示上限200台） · 過去{}分",view.nodes.len(),view.total_nodes,view.total_nodes.saturating_sub(view.nodes.len()),text(&saved["minutes"]))))
        .child(div().text_xs().text_color(palette::muted()).child("保存済み集計と、明示開始された同じ Live の現在値。閲覧・再読込では収集しません。monitor・詳細schedulerの稼働はこのpreviewでは未接続です。200台はUIの表示上限です。coreの読取台数・応答byte数の上限ではありません。"))
        .child(div().flex().flex_wrap().gap_2()
            .child(field("保存済みmonitor設定",format!("enabled {} · hub {}",text(&saved["monitor"]["enabled"]),text(&saved["monitor"]["hub"]))))
            .child(field("core採取方針（稼働証明ではありません）",text(&saved["collecting"])))
            .child(field("収集保守記録 health",format!("{} · running {} · {}",text(&saved["journal"]["status"]),text(&saved["journal"]["running"]),text(&saved["journal"]["summary"]))))
            .child(field("保存済み詳細scheduler",format!("enabled {} · 分析 {}分 · ログ {}分 · 道具 {}時間 · AI {}分",text(&saved["schedule"]["enabled"]),text(&saved["schedule"]["probe_minutes"]),text(&saved["schedule"]["logs_minutes"]),text(&saved["schedule"]["inventory_hours"]),text(&saved["schedule"]["ai_minutes"]))))
            .child(field("hub取込の保存結果",format!("成功 {} · 最終試行 {} · metric fresh {} · snapshot fresh {} · error {}",text(&saved["hub_pull"]["ok"]),crate::panels::time(&saved["hub_pull"]["at"]),text(&saved["hub_pull"]["fresh"]),text(&saved["hub_pull"]["snapshot_fresh"]),text(&saved["hub_pull"]["error"])))))
        .child(div().text_xs().text_color(palette::muted()).child(format!("保存集計の読込 {} · アプリ {} · 記録healthのunknownは過去の欠落ゼロを意味しません",crate::panels::time(&saved["at"]),text(&saved["app"]["version"]))));
    let until = saved["at"].as_i64().unwrap_or(now);
    let since = until - saved["minutes"].as_i64().unwrap_or(60) * 60_000;
    for node in &view.nodes {
        let brief = live.brief(&node.observation.id);
        let current = current_values(node, brief.as_ref(), now);
        let observation = match &node.observation.health {
            model::Health::Excluded(reason) => format!("運用対象外 · {reason}"),
            model::Health::Failed(reason) => format!("直近分析失敗 · {reason}"),
            model::Health::Unobserved => "詳細分析は未観測".into(),
            model::Health::Scored(score) => format!(
                "保存分析スコア {score} · {}",
                if node.observation.at_ms.is_some_and(|t| now.saturating_sub(t) > 7_200_000) { "古い分析（2時間超）" } else { "保存済み分析" }
            ),
        };
        let mut card = div()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(palette::line())
            .bg(palette::panel())
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(format!("{} · {}", node.observation.id, node.observation.os)))
            .child(div().text_sm().text_color(palette::muted()).child(observation))
            .child(div().text_xs().text_color(palette::muted()).child(format!(
                "{} · 値の時刻 {} · 保存元 {} · 保存minute {}",
                current.source,
                crate::panels::time(&current.at.into()),
                text(&node.saved["latest"]["src"]),
                crate::panels::time(&node.saved["latest"]["minute"])
            )));
        let mut metrics = div().flex().flex_wrap().gap_2();
        for (key, label, unit) in [
            ("cpu", "CPU", "%"),
            ("mem", "メモリ", "%"),
            ("gpu", "GPU", "%"),
            ("gtemp", "GPU温度", "℃"),
            ("gmem", "GPUメモリ", "%"),
            ("swap", "swap", " GB"),
            ("commit", "コミット", "%"),
            ("dr", "ディスク読込", " B/s"),
            ("dw", "ディスク書込", " B/s"),
            ("rx", "受信", " B/s"),
            ("tx", "送信", " B/s"),
            ("power_platform_w", "機体電力（推定）", " W"),
            ("power_cpu_w", "CPU電力", " W"),
            ("power_gpu_w", "GPU電力", " W"),
            ("power_soc_w", "SoC電力", " W"),
        ] {
            metrics = metrics.child(field(label, number(&current.values[key], unit)));
        }
        card = card.child(metrics);
        let mut vram = false;
        for slot in 0..16 {
            let key = |suffix: &str| format!("gpu_{slot}_vram_{suffix}");
            if ["used_mib", "total_mib", "free_mib", "pct"].iter().any(|s| current.values.get(key(s)).is_some()) {
                vram = true;
                card = card.child(div().text_xs().child(format!(
                    "GPU 保存slot {slot} · VRAM {} / {} · 空き {} · {}",
                    number(&current.values[key("used_mib")], " MiB"),
                    number(&current.values[key("total_mib")], " MiB"),
                    number(&current.values[key("free_mib")], " MiB"),
                    number(&current.values[key("pct")], "%")
                )));
            }
        }
        if !vram {
            card = card.child(div().text_xs().text_color(palette::muted()).child("VRAM 未取得（統合メモリ・専用VRAMの判定情報なし）"));
        }
        card = card.child(
            div()
                .flex()
                .gap_3()
                .child(series_chart(node, "cpu", "CPU 保存平均", palette::accent(), since, until))
                .child(series_chart(node, "mem", "メモリ 保存平均", palette::ok(), since, until))
                .child(series_chart(node, "gpu", "GPU 保存平均", palette::warn(), since, until)),
        );
        if let Some(b) = &brief {
            card = card.child(div().text_xs().child(format!(
                "Live {} · reason {} · {} · 実装 {} · sampler負荷 {} · RSS {} · {}",
                text(&b["state"]),
                text(&b["reason"]),
                text(&b["detail"]),
                text(&b["impl"]),
                number(&b["load"], "%（1コア比）"),
                number(&b["rss_mb"], " MB"),
                crate::panels::time(&b["point"]["t"])
            )));
        }
        for (key, label, unit) in [("top_cpu", "CPU上位", "%（1コア比）"), ("top_mem", "メモリ上位", " MB")] {
            let top = brief.as_ref().and_then(|b| b[key].as_array());
            if let Some(top) = top {
                for p in top.iter().take(3) {
                    card = card.child(div().text_xs().child(format!(
                        "{label} · {} {}",
                        text(&p["name"]),
                        number(&p[if key == "top_cpu" { "cpu" } else { "mem_mb" }], unit)
                    )));
                }
            } else if let Some(top) = node.saved["latest"]["v"][key].as_array() {
                card = card.child(div().text_xs().child(format!("保存{label} · {} {}", text(&top[0]), number(&top[1], unit))));
            }
        }
        let last = &node.saved["last"];
        card = card.child(div().text_xs().text_color(palette::muted()).child(format!(
            "分析時刻 {} · 保存稼働時間 {} · 最小空きディスク {} {} · 防御 {} · 電源 {} · 7日の停止 {}",
            crate::panels::time(&last["at"]),
            number(&last["uptime_h"], " 時間"),
            text(&last["disk_low"]["mount"]),
            number(&last["disk_low"]["free_pct"], "%"),
            text(&last["defender_realtime"]),
            text(&last["power_plan"]),
            text(&last["bugchecks_7d"])
        )));
        if let Some(alerts) = node.saved["alerts"].as_array() {
            for alert in alerts {
                card = card.child(div().text_sm().text_color(palette::warn()).child(format!("{} · {}", text(&alert["title"]), text(&alert["detail"]))));
            }
        }
        body = body.child(card);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Health;
    use serde_json::json;
    use std::{path::PathBuf, sync::Arc};
    use tune_core::{
        db::{Store, metrics::MinuteRow},
        engine::NoHost,
    };

    struct Fixture {
        engine: Arc<Engine>,
        dir: PathBuf,
    }
    impl Fixture {
        fn new(nodes: Value) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("tune-dashboard-{}-{}-{n}", std::process::id(), tune_core::db::now_ms()));
            std::fs::create_dir(&dir).unwrap();
            let config = dir.join("nodes.json");
            std::fs::write(&config, json!({"nodes":nodes}).to_string()).unwrap();
            Self { engine: Engine::open(config, dir.join("data"), Arc::new(NoHost)).unwrap(), dir }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    // Catches compressing sparse timestamps into equally-spaced bars or
    // dropping a missing metric in the middle of the saved series.
    #[test]
    fn saved_chart_keeps_timestamp_gaps_and_missing_values() {
        let points = chart_points(&json!({"t":[120_000,180_000,540_000],"cpu":[20,null,60]}), "cpu", 0, 600_000);
        assert_eq!(points, vec![(0.2, Some(20.)), (0.3, None), (0.9, Some(60.))]);
    }

    // Catches replacing core saved metrics with CPU/mem LiveStore, filling holes
    // with zero, or treating a failed latest probe / excluded node as healthy.
    #[test]
    fn saved_metrics_preserve_vram_missing_disk_alerts_and_observation_failures() {
        let f = Fixture::new(json!([
            {"id":"example-active","os":"windows","alias":"unused"},
            {"id":"example-failed","os":"macos","alias":"unused"},
            {"id":"example-paused","os":"macos","alias":"unused","status":"paused"},
            {"id":"example-new","os":"windows","alias":"unused"}
        ]));
        let now = tune_core::db::now_ms();
        f.engine
            .with_db(|d| {
                d.add_metrics(
                    &(1..=4)
                        .map(|i| MinuteRow {
                            node_id: "example-active".into(),
                            minute: now.div_euclid(60_000) * 60_000 - i * 60_000,
                            n: 60,
                            v: json!({"cpu":[95,99],"mem":[93,94],"gpu_0_vram_used_mib":[512,600],"gpu_0_vram_total_mib":[2048,2048],"gtemp":[82,88]}),
                            src: Some("example-hub".into()),
                        })
                        .collect::<Vec<_>>(),
                )?;
                d.set_meta("monitor", &json!({"enabled":true,"hub":"example-failed"}))?;
                d.set_meta("hubPull", &json!({"hub":"example-failed","at":now-3_600_000,"ok":false,"error":"synthetic unavailable"}))?;
                d.set_meta("probeFailure:example-failed", &json!({"at":now,"error":"synthetic failure"}))?;
                Ok(())
            })
            .unwrap();
        let view = load_saved(&f.engine, 60).unwrap();
        assert_eq!(view.total_nodes, 4);
        assert_eq!(view.saved["minutes"], 60);
        let n = &view.nodes[0];
        assert_eq!(n.saved["latest"]["v"]["gpu_0_vram_used_mib"], json!([512, 600]));
        assert_eq!(n.saved["latest"]["src"], "example-hub");
        assert!(n.saved["latest"]["v"]["dr"].is_null());
        assert!(n.saved["series"]["dr"].is_null());
        assert!(n.saved["alerts"].as_array().unwrap().iter().any(|a| a["id"] == "cpu"));
        assert!(matches!(view.nodes[1].observation.health, Health::Failed(_)));
        assert!(matches!(view.nodes[2].observation.health, Health::Excluded(_)));
        assert_eq!(view.nodes[3].observation.health, Health::Unobserved);
        assert_eq!(view.saved["hub_pull"]["ok"], false);
        assert_eq!(view.saved["journal"]["status"], "unknown");
        assert_eq!(view.saved["journal"]["running"], false);
        assert!(view.saved["schedule"].is_object());
    }

    // Catches a UI limit applied to the core series, or a range clamp differing
    // from core; the 200-node bound must report omitted and total counts.
    #[test]
    fn range_clamp_and_ui_node_bound_keep_the_core_bucket_series() {
        let f = Fixture::new(Value::Array((0..203).map(|i| json!({"id":format!("example-{i}"),"os":"macos","alias":"unused"})).collect()));
        let now = tune_core::db::now_ms();
        // A future-dated saved row is returned by the existing core accessor.
        // Preserve it too, rather than silently imposing a final 60-point cap.
        f.engine
            .with_db(|d| {
                d.add_metrics(
                    &(0..=60)
                        .map(|i| MinuteRow { node_id: "example-0".into(), minute: now - 3_540_000 + i * 60_000, n: 1, v: json!({"cpu":[i,i]}), src: None })
                        .collect::<Vec<_>>(),
                )
            })
            .unwrap();
        let view = load_saved(&f.engine, 60).unwrap();
        assert_eq!(view.total_nodes, 203);
        assert_eq!(view.nodes.len(), 200);
        assert!(view.nodes[0].saved["series"]["t"].as_array().unwrap().len() > 60, "60 buckets is a target, not a final array cap");
        assert_eq!(load_saved(&f.engine, 1).unwrap().saved["minutes"], 10);
        assert_eq!(load_saved(&f.engine, 9999).unwrap().saved["minutes"], 1440);
        let db = Store::open_read_only(&f.dir.join("data")).unwrap();
        assert_eq!(db.metrics(None, 0, i64::MAX).unwrap().len(), 61);
    }

    // Catches stale Live overriding saved metrics, using temperature average
    // instead of maximum, and a missing Live sensor filled from another source.
    #[test]
    fn current_values_use_one_fresh_brief_or_saved_averages_with_temperature_max() {
        let f = Fixture::new(json!([{"id":"example-node","os":"macos","alias":"unused"}]));
        let now = tune_core::db::now_ms();
        f.engine
            .with_db(|d| {
                d.add_metrics(&[MinuteRow {
                    node_id: "example-node".into(),
                    minute: now - 60_000,
                    n: 60,
                    v: json!({"cpu":[45,90],"gtemp":[70,88],"power_soc_w":[12,20],"gpu_2_vram_used_mib":[1024,1200]}),
                    src: None,
                }])
            })
            .unwrap();
        let view = load_saved(&f.engine, 60).unwrap();
        let n = &view.nodes[0];
        let saved = current_values(n, None, now);
        assert_eq!(saved.values["cpu"], 45);
        assert_eq!(saved.values["gtemp"], 88);
        assert_eq!(saved.values["gpu_2_vram_used_mib"], 1024);
        assert!(saved.values["dr"].is_null());
        assert_eq!(saved.source, "保存分平均（温度は最大）");
        assert_eq!(saved.at, Some(now - 60_000));
        let brief = json!({"point":{"t":now-10_000,"cpu":7,"power_platform_w":42,"gpu_2_vram_used_mib":512},"impl":"rust","load":0.2,"rss_mb":8,"top_cpu":[{"name":"example","cpu":5}]});
        let fresh = current_values(n, Some(&brief), now);
        assert_eq!(fresh.values["cpu"], 7);
        assert_eq!(fresh.values["power_platform_w"], 42);
        assert!(fresh.values["gtemp"].is_null(), "do not mix saved and Live sensor timestamps");
        assert_eq!(fresh.source, "Live（10秒以内）");
        assert_eq!(current_values(n, Some(&brief), now + 1).values["cpu"], 45);
        assert_eq!(current_values(n, None, now + 180_000).source, "古い保存分平均（温度は最大）");
    }
}
