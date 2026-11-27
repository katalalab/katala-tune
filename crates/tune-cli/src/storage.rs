//! Local operator-invoked wrapper. No GUI command execution or remote shells.
mod options;
mod receipt;
mod volume;

use options::Options;
use receipt::ReportClaim;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    io,
    path::Path,
    process::{Command, ExitCode},
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

// Reports are read by the desktop using JavaScript numbers. Never silently round a byte count.
const MAX_BYTES: u64 = (1u64 << 53) - 1;

fn label(volume: Option<u64>) -> String {
    let Some(id) = volume else { return "unknown".into() };
    let mut h = DefaultHasher::new();
    id.hash(&mut h);
    format!("volume-{:016x}", h.finish())
}

fn logical_bytes(root: &Path, device: u64) -> Option<u64> {
    let mut todo = vec![root.to_owned()];
    let mut total = 0u64;
    while let Some(p) = todo.pop() {
        let md = p.symlink_metadata().ok()?;
        if volume::link(&md) || volume::identity(&p).ok()? != device {
            return None;
        }
        if md.is_dir() {
            for entry in fs::read_dir(&p).ok()? {
                todo.push(entry.ok()?.path());
            }
        } else if md.is_file() {
            total = total.checked_add(md.len())?;
        }
    }
    Some(total)
}

fn same_volume(path: &Path, id: Option<u64>) -> bool {
    id.is_some() && volume::identity(path).ok() == id
}

fn read_free(path: &Path, id: Option<u64>) -> Option<u64> {
    if !same_volume(path, id) {
        return None;
    }
    let size = volume::available(path).ok().filter(|n| *n <= MAX_BYTES)?;
    same_volume(path, id).then_some(size)
}

fn difference(a: Option<u64>, b: Option<u64>) -> Option<i64> {
    i64::try_from(i128::from(a?) - i128::from(b?)).ok().filter(|n| n.unsigned_abs() <= MAX_BYTES)
}

#[derive(Default)]
struct Observations {
    samples: Vec<Value>,
    count: u64,
    missing: u64,
    baseline: Option<u64>,
    min: Option<u64>,
    last: Option<u64>,
    stride: u64,
}
impl Observations {
    fn add(&mut self, elapsed: u64, free: Option<u64>, end: bool) {
        if self.count == 0 {
            self.baseline = free;
            self.stride = 1;
        }
        self.last = free;
        if let Some(n) = free {
            self.min = Some(self.min.map_or(n, |m| m.min(n)));
        } else {
            self.missing += 1;
        }
        if self.samples.len() >= 8192 {
            let mut index = 0;
            self.samples.retain(|_| {
                let keep = index % 2 == 0;
                index += 1;
                keep
            });
            self.stride *= 2;
        }
        if self.count.is_multiple_of(self.stride) || end {
            self.samples.push(json!({"elapsed_ms":elapsed,"free_bytes":free}));
        }
        self.count += 1;
    }
    fn summary(&self, ran: bool) -> (Option<i64>, Option<i64>) {
        if !ran {
            return (None, None);
        }
        (difference(self.baseline, self.min), difference(self.baseline, self.last))
    }
}

fn run(o: Options<'_>) -> Result<ExitCode, String> {
    let device = volume::identity(&o.target).ok();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "invalid system clock")?.as_millis() as u64;
    let mut report = json!({
        "schema_version":1,"kind":"job-storage","job_id":o.job,"volume":label(device),"interval_ms":o.interval,
        "started_at_ms":now,"status":"failed","reason":"incomplete","samples":[],"sample_count":0,"missing_sample_count":0,
        "peak_bytes":null,"net_bytes":null,"unattributed_net_bytes":null,"residual_bytes":null,
        "categories":[],"preflight":{"os_reserve_bytes":o.os,"other_reserved_bytes":o.other,"expected_peak_bytes":o.expected,"free_bytes":null}
    });
    let claim = ReportClaim::acquire(&o.report, &report).map_err(|e| format!("cannot reserve report ({e})"))?;
    let category_sizes = || {
        o.categories
            .values()
            .map(|dir| {
                if dir.canonicalize().ok().as_deref() != Some(dir) || !same_volume(&o.target, device) {
                    return None;
                }
                logical_bytes(dir, device?)
            })
            .collect::<Vec<_>>()
    };
    let before = category_sizes();
    // Keep admission and actual start adjacent even for large directory scans.
    #[cfg(target_os = "macos")]
    let important = volume::admission(&o.target).filter(|n| *n <= MAX_BYTES);
    let initial = read_free(&o.target, device);
    #[cfg(target_os = "macos")]
    let (allowance, source) = if let Some(n) = important { (Some(n), "important_usage") } else { (initial, "available_to_caller") };
    #[cfg(not(target_os = "macos"))]
    let (allowance, source) = (initial, "available_to_caller");
    report["preflight"]["free_bytes"] = json!(allowance);
    report["preflight"]["basis"] = json!(source);
    let start = Instant::now();
    report["started_at_ms"] = json!(SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "invalid system clock")?.as_millis() as u64);
    let mut obs = Observations::default();
    obs.add(0, initial, false);
    let admission = volume::environment_hold().or_else(|| match (initial, allowance) {
        _ if o.categories.values().any(|d| !same_volume(d, device)) => Some("unavailable_space"),
        (None, _) | (_, None) => Some("unavailable_space"),
        (_, Some(n)) if n < o.required => Some("insufficient_space"),
        _ => None,
    });
    let mut ran = false;
    let (status, reason, code) = if let Some(reason) = admission {
        ("blocked", Some(reason), None)
    } else {
        match Command::new(&o.command[0]).args(&o.command[1..]).spawn() {
            Err(_) => ("failed", Some("spawn_error"), None),
            Ok(mut child) => {
                ran = true;
                let (tx, rx) = mpsc::sync_channel(1);
                thread::spawn(move || {
                    let _ = tx.send(child.wait());
                });
                let outcome = loop {
                    match rx.recv_timeout(Duration::from_millis(o.interval)) {
                        Ok(r) => break r,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break Err(io::Error::other("child wait interrupted")),
                        Err(mpsc::RecvTimeoutError::Timeout) => obs.add(start.elapsed().as_millis() as u64, read_free(&o.target, device), false),
                    }
                };
                obs.add(start.elapsed().as_millis() as u64, read_free(&o.target, device), true);
                match outcome {
                    Ok(st) if st.success() => ("completed", None, st.code()),
                    Ok(st) => ("failed", Some("command_failed"), st.code()),
                    Err(_) => ("failed", Some("wait_error"), None),
                }
            }
        }
    };
    let after = if ran { category_sizes() } else { vec![None; before.len()] };
    let (peak, net) = obs.summary(ran);
    report["status"] = json!(status);
    report["reason"] = json!(reason);
    report["exit_code"] = json!(code);
    report["samples"] = Value::Array(obs.samples);
    report["sample_count"] = json!(obs.count);
    report["missing_sample_count"] = json!(obs.missing);
    report["peak_bytes"] = json!(peak);
    report["net_bytes"] = json!(net);
    // Logical file lengths (including shared/COW blocks) cannot be subtracted from physical usage.
    report["unattributed_net_bytes"] = json!(net);
    report["categories"] = Value::Array(
        o.categories.keys().zip(after).zip(before).map(|((kind, end), start)| json!({"kind":kind,"logical_delta_bytes":difference(end,start)})).collect(),
    );
    claim.publish(&o.report, &report).map_err(|e| format!("cannot finalize report ({e})"))?;
    if let Some(reason) = reason {
        eprintln!("storage-run: {reason}");
    }
    Ok(if status == "completed" { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

pub(super) fn main(args: &[OsString]) -> ExitCode {
    match options::parse(args).and_then(run) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("storage-run: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_or_changed_volume_never_reports_free_bytes() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path();
        let dev = volume::identity(path).unwrap();
        assert!(read_free(path, Some(dev)).is_some());
        assert!(read_free(path, Some(dev.wrapping_add(1))).is_none());
        assert!(read_free(path, None).is_none());
    }
    #[test]
    fn observations_preserve_gaps_freed_space_and_a_transient_minimum() {
        let mut obs = Observations::default();
        obs.add(0, Some(80), false);
        for i in 1..20000 {
            obs.add(
                i,
                match i {
                    1 => None,
                    17 => Some(10),
                    _ => Some(80),
                },
                false,
            );
        }
        obs.add(20000, Some(81), true);
        assert_eq!(obs.samples[0]["elapsed_ms"], 0);
        assert_eq!(obs.samples.last().unwrap()["elapsed_ms"], 20000);
        assert!(obs.samples.len() < 8192);
        assert_eq!(obs.missing, 1);
        assert_eq!(obs.summary(true), (Some(70), Some(-1)));
        assert_eq!(obs.summary(false), (None, None));
        obs.add(20001, None, true);
        assert_eq!(obs.summary(true), (Some(70), None));
        assert_eq!(difference(None, Some(1)), None);
    }
}
