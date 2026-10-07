//! Synthetic, local-only benchmark: cargo run --release -p tune-core --example performance --locked
use std::hint::black_box;
use std::time::Instant;

use serde_json::json;
use tune_core::logs;

fn main() {
    let rows: Vec<_> = (0..20_000)
        .map(|i| {
            json!({"uid": i.to_string(), "ts": 1_800_000_000_000i64 + i,
            "provider": "example", "event_id": "7", "level": "warn",
            "message": format!("device {i} retry failed after 42 ms at 0x1f3")})
        })
        .collect();
    // Initialize regexes before timing.
    black_box(logs::normalize("example", &rows[..1]));
    let mut timings = Vec::new();
    for _ in 0..7 {
        let start = Instant::now();
        let normalized = logs::normalize("example", black_box(&rows));
        assert_eq!(normalized.len(), rows.len());
        black_box(normalized);
        timings.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    timings.sort_by(f64::total_cmp);
    println!("{}", json!({"rows": rows.len(), "runs": timings.len(), "median_ms": timings[3], "min_ms": timings[0]}));
}
