#[test]
fn parsed_live_messages_keep_the_event_size_bounded() {
    assert!(std::mem::size_of::<super::Msg>() <= 64);
}
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn node(id: &str, os: &str) -> Node {
    Node::from_value(&json!({ "id": id, "alias": id, "os": os }), "this-host")
}

const SAMPLE: &str = r#"{"type":"s","t":1700000000000,"seq":3,"cpu":12.5,"cores":[10,20,null,150],"mem":{"used_pct":41.2,"swap_used_gb":1.5,"swap_total_gb":2,"pressure":"normal"},"disk":{"read_bps":1000,"write_bps":-5},"net":{"rx_bps":10,"tx_bps":20},"power":{"package_w":200.8,"package_source":"Energy Meter RAPL package average","available":true},"gpu":[{"uuid":"GPU-a","name":"G","util":30,"mem_used_mb":1024,"mem_total_mb":4096,"power_w":220,"power_limit_w":320,"power_min_w":100,"power_max_w":350,"temp_c":65,"clocks_graphics_mhz":1800,"clocks_sm_mhz":1815,"clocks_memory_mhz":9500,"pstate":"P0","source":"nvidia-smi dmon","available":true}],"procs":{"count":3,"top_cpu":[{"pid":7,"name":"a","cpu":150.5,"mem_mb":10}],"top_mem":[]},"self":{"cpu_s":0.5,"rss_mb":20}}"#;

#[test]
fn vram_keeps_adapter_fields_and_missing_memory_is_not_zero() {
    let Some(Msg::Sample(raw)) = parse_line(
        r#"{"type":"s","t":123,"gpu":[{"adapter_index":0,"name":"A","mem_used_mb":1024,"mem_total_mb":4096,"mem_free_mb":3000,"memory_status":"available","memory_kind":"dedicated","sampled_at":123},{"adapter_index":1,"name":"B","mem_total_mb":8192,"mem_used_mb":null,"mem_free_mb":null,"memory_status":"partial"}]}"#,
    ) else {
        panic!("sample")
    };
    let (s, _, _) = Sample::from_raw(*raw, 123);
    let point = s.point();
    assert_eq!(point["gpu_0_vram_used_mib"], json!(1024.0));
    assert_eq!(point["gpu_0_vram_free_mib"], json!(3000.0));
    assert_eq!(point["gpu_0_vram_pct"], json!(25.0));
    assert_eq!(point["gpu_1_vram_used_mib"], Value::Null);
    assert_eq!(point["gmem"], json!(25.0), "available adapter percent, never dilute with unknown adapter");
    assert_eq!(serde_json::to_value(&s.gpu).unwrap()[0]["memory_status"], "available");
}

#[test]
fn disk_null_partial_and_legacy_samples_preserve_unknown_and_idle() {
    for (disk, read, write) in [
        (None, None, None),
        (Some(Value::Null), None, None),
        (Some(json!({"read_bps":null,"write_bps":120})), None, Some(120.0)),
        (Some(json!({"read_bps":0,"write_bps":0})), Some(0.0), Some(0.0)),
    ] {
        let mut row = json!({"type":"s","t":1000,"cpu":10});
        if let Some(disk) = disk {
            row["disk"] = disk;
        }
        let Some(Msg::Sample(raw)) = parse_line(&row.to_string()) else { panic!("sample") };
        let (sample, _, _) = Sample::from_raw(*raw, 1000);
        let point = sample.point();
        assert_eq!(point.get("dr").and_then(Value::as_f64), read);
        assert_eq!(point.get("dw").and_then(Value::as_f64), write);
        assert_eq!(point["cpu"], json!(10.0));
    }
}

#[test]
fn parses_lines_and_ignores_noise() {
    let Some(Msg::Hello(h)) = parse_line("\u{FEFF}{\"type\":\"hello\",\"v\":1,\"os\":\"windows\",\"cores\":8,\"errors\":\"x\",\"has\":{\"gpu\":true}}\r")
    else {
        panic!("hello を読めない")
    };
    assert_eq!((h.os.as_deref(), h.cores, h.has.get("gpu")), (Some("windows"), Some(8), Some(&Value::Bool(true))));
    let Some(Msg::Sample(s)) = parse_line(SAMPLE) else { panic!("s を読めない") };
    let (p, procs, me) = Sample::from_raw(*s, 1_700_000_000_120);
    assert_eq!(p.cpu, Some(12.5));
    assert_eq!(p.cores, vec![Some(10.0), Some(20.0), None, Some(100.0)], "コアは 0〜100 に収める");
    assert_eq!(p.disk.as_ref().unwrap().write_bps, None, "負の速度は捨てる");
    assert_eq!((p.src_t, p.seq), (Some(1_700_000_000_000), Some(3)));
    assert_eq!(p.power.as_ref().and_then(|x| x.package_w), Some(200.8));
    let gpu = &p.gpu.as_ref().unwrap()[0];
    assert_eq!(
        (gpu.uuid.as_deref(), gpu.pstate.as_deref(), gpu.source.as_deref(), gpu.available),
        (Some("GPU-a"), Some("P0"), Some("nvidia-smi dmon"), Some(true))
    );
    assert_eq!((gpu.power_w, gpu.power_limit_w, gpu.temp_c), (Some(220.0), Some(320.0), Some(65.0)));
    assert_eq!((gpu.power_min_w, gpu.power_max_w), (Some(100.0), Some(350.0)));
    assert_eq!((gpu.clocks_graphics_mhz, gpu.clocks_sm_mhz, gpu.clocks_memory_mhz), (Some(1800.0), Some(1815.0), Some(9500.0)));
    assert_eq!(procs.unwrap().top_cpu[0].cpu, Some(150.5), "プロセスは 1 コア換算なので 100 を超えてよい");
    assert_eq!(me.unwrap().cpu_s, Some(0.5));
    let pt = p.point();
    assert_eq!(
        (pt["cpu"].as_f64(), pt["gpu"].as_f64(), pt["gmem"].as_f64(), pt["power_cpu_w"].as_f64(), pt["power_gpu_w"].as_f64(), pt["lag"].as_f64()),
        (Some(12.5), Some(30.0), Some(25.0), Some(200.8), Some(220.0), Some(120.0))
    );
    assert!(pt.get("dw").is_none(), "無い値は点に入れない");
    assert!(matches!(parse_line(r#"{"type":"end","reason":"max_age"}"#), Some(Msg::End(r)) if r == "max_age"));
    for bad in ["", "WARNING: something", "{broken", r#"{"type":"other"}"#, r#"{"cpu":1}"#, r#"{"type":"s","cores":"x"}"#] {
        assert!(parse_line(bad).is_none(), "{bad:?} は捨てる");
    }
    assert!(parse_line(&format!("{{\"type\":\"s\",\"pad\":\"{}\"}}", "x".repeat(MAX_LINE))).is_none(), "長すぎる行は捨てる");
}

#[test]
fn ring_keeps_the_latest_and_downsamples() {
    let mut r = Ring::new(3);
    for i in 0..5 {
        r.push(i);
    }
    assert_eq!(r.iter().copied().collect::<Vec<_>>(), vec![2, 3, 4]);
    assert_eq!(r.tail(2).copied().collect::<Vec<_>>(), vec![3, 4]);
    assert_eq!((r.len(), r.last()), (3, Some(&4)));
    let pts: Vec<Value> = (0..10).map(|i| json!({ "t": i, "cpu": i as f64 * 10.0 })).collect();
    let d = downsample(&pts, 4);
    assert_eq!(d.len(), 4);
    assert_eq!(d[0], json!({ "t": 2, "cpu": 10.0 }), "3 点の平均と、最後の時刻");
    assert_eq!(d[3], json!({ "t": 9, "cpu": 90.0 }));
    assert_eq!(downsample(&pts[..3], 4).len(), 3, "少なければそのまま");
}

struct LaunchTestDir {
    path: PathBuf,
    cleanup: bool,
}

impl LaunchTestDir {
    fn create(path: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir(&path)?;
        Ok(Self { path, cleanup: true })
    }

    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        #[cfg(target_os = "linux")]
        let parent = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
        #[cfg(not(target_os = "linux"))]
        let parent = std::env::temp_dir();
        Self::create(parent.join(format!("kt-live-launch-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)))).unwrap()
    }
}

impl Drop for LaunchTestDir {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

struct ExactChildOutput {
    status: Option<std::process::ExitStatus>,
    reason: Option<&'static str>,
    stdout_bytes: u64,
    stderr_bytes: u64,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

// Keep the exact Child handle; never rediscover or signal an arbitrary PID.
struct RetainedExactChild {
    child: std::process::Child,
    status: Option<std::process::ExitStatus>,
    stopped: bool,
}

impl RetainedExactChild {
    fn reap(&mut self) -> std::io::Result<bool> {
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
        }
        Ok(self.status.is_some())
    }

    fn stop_and_reap(&mut self) {
        if self.stopped || self.status.is_some() {
            return;
        }
        self.stopped = true;
        let _ = self.child.kill();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match self.reap() {
                Ok(true) | Err(_) => return,
                Ok(false) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}

impl Drop for RetainedExactChild {
    fn drop(&mut self) {
        self.stop_and_reap();
    }
}

fn capture_limit_reason(out: &std::fs::File, err: &std::fs::File, deadline: Instant, cap: usize) -> std::io::Result<Option<&'static str>> {
    // Prefer an observed excess even if this observation is also late.
    if out.metadata()?.len().saturating_add(err.metadata()?.len()) > cap as u64 {
        return Ok(Some("output limit"));
    }
    if Instant::now() >= deadline {
        return Ok(Some("deadline"));
    }
    Ok(None)
}

#[test]
fn launch_test_capture_prioritizes_observed_output_over_an_expired_deadline() {
    let dir = LaunchTestDir::new();
    let out = std::fs::OpenOptions::new().write(true).create_new(true).open(dir.path.join("stdout")).unwrap();
    let err = std::fs::OpenOptions::new().write(true).create_new(true).open(dir.path.join("stderr")).unwrap();
    let cap = 4096;
    let expired = Instant::now();
    assert_eq!(capture_limit_reason(&out, &err, expired, cap).unwrap(), Some("deadline"));
    out.set_len(cap as u64 + 1).unwrap();
    assert_eq!(capture_limit_reason(&out, &err, expired, cap).unwrap(), Some("output limit"));
    out.set_len(0).unwrap();
    err.set_len(cap as u64 + 1).unwrap();
    assert_eq!(capture_limit_reason(&out, &err, expired, cap).unwrap(), Some("output limit"));
    err.set_len(cap as u64).unwrap();
    assert_eq!(capture_limit_reason(&out, &err, expired, cap).unwrap(), Some("deadline"));
    out.set_len(1).unwrap();
    assert_eq!(capture_limit_reason(&out, &err, expired, cap).unwrap(), Some("output limit"));
}

fn capture_exact_child(command: &mut std::process::Command, dir: &mut LaunchTestDir, limit: Duration, cap: usize) -> ExactChildOutput {
    use std::io::Read as _;
    let deadline = Instant::now() + limit;
    let out_path = dir.path.join("stdout");
    let err_path = dir.path.join("stderr");
    let out_file = std::fs::OpenOptions::new().write(true).create_new(true).open(&out_path).unwrap();
    let err_file = std::fs::OpenOptions::new().write(true).create_new(true).open(&err_path).unwrap();
    command.stdout(out_file.try_clone().unwrap()).stderr(err_file.try_clone().unwrap());
    let mut child = RetainedExactChild { child: command.spawn().unwrap(), status: None, stopped: false };
    // If anything fails before confirmed reap, leave the owned directory for
    // diagnosis rather than removing files still used by a retained child.
    dir.cleanup = false;
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let reason = loop {
        // Regular files avoid pipe-reader joins. Poll size and stop excess
        // output; only cap bytes in total are ever loaded into diagnostics.
        if let Some(reason) = capture_limit_reason(&out_file, &err_file, deadline, cap).unwrap() {
            break Some(reason);
        }
        let reaped = child.reap().unwrap();
        if reaped {
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    child.stop_and_reap();
    dir.cleanup = child.status.is_some();
    let stdout_bytes = out_file.metadata().unwrap().len();
    let stderr_bytes = err_file.metadata().unwrap().len();
    let mut stdout = Vec::new();
    std::fs::File::open(out_path).unwrap().take(cap as u64).read_to_end(&mut stdout).unwrap();
    let mut stderr = Vec::new();
    std::fs::File::open(err_path).unwrap().take((cap - stdout.len()) as u64).read_to_end(&mut stderr).unwrap();
    ExactChildOutput { status: child.status, reason, stdout_bytes, stderr_bytes, stdout, stderr }
}

fn launch_test_command(dir: &std::path::Path, test: &str, case: &str) -> std::process::Command {
    let app = dir.join("owned app");
    let home = dir.join("synthetic-home");
    std::fs::create_dir(&app).unwrap();
    std::fs::create_dir(&home).unwrap();
    let executable = app.join(crate::helpers::exe("live-test"));
    // Linux links the already-running inode, avoiding a writable executable
    // copy while parallel tests spawn children. The owned root is on its FS.
    #[cfg(target_os = "linux")]
    std::fs::hard_link(std::env::current_exe().unwrap(), &executable).unwrap();
    #[cfg(not(target_os = "linux"))]
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let mut child = std::process::Command::new(&executable);
    child
        .args(["--exact", test, "--nocapture", "--test-threads=1", "--format=terse"])
        .env("KT_LIVE_LAUNCH_TEST_ROOT", dir)
        .env("KT_LIVE_LAUNCH_TEST_CASE", case)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("KATALA_TUNE_DATA_DIR", home.join(".katala-tune"))
        .stdin(Stdio::null());
    #[cfg(target_os = "macos")]
    child.env("PATH", "/usr/bin:/bin");
    child
}

#[test]
fn launch_test_directory_rejects_collisions_and_cleans_only_owned_paths() {
    let owner = LaunchTestDir::new();
    let existing = owner.path.join("existing");
    std::fs::create_dir(&existing).unwrap();
    let canary = existing.join("canary");
    std::fs::write(&canary, b"keep").unwrap();
    let collision = LaunchTestDir::create(existing.clone());
    assert!(matches!(collision, Err(ref e) if e.kind() == std::io::ErrorKind::AlreadyExists));
    drop(collision);
    assert_eq!(std::fs::read(&canary).unwrap(), b"keep");
    let path = owner.path.clone();
    drop(owner);
    assert!(!path.exists(), "successful exclusive owner cleans its own fixture");
}

#[cfg(target_os = "linux")]
#[test]
fn launch_test_links_the_running_inode_and_cleans_only_owned_names() {
    use std::os::unix::fs::MetadataExt as _;
    let source = std::env::current_exe().unwrap();
    let identity = |path: &std::path::Path| {
        let metadata = std::fs::metadata(path).unwrap();
        (metadata.dev(), metadata.ino())
    };
    let source_identity = identity(&source);
    let first = LaunchTestDir::new();
    let second = LaunchTestDir::new();
    assert_ne!(first.path, second.path);
    assert_eq!(first.path.parent(), source.parent());
    assert_eq!(second.path.parent(), source.parent());
    let first_command = launch_test_command(&first.path, "live::tests::launch_follows_the_probe_conventions", "absent");
    let second_command = launch_test_command(&second.path, "live::tests::launch_follows_the_probe_conventions", "absent");
    let first_executable = std::path::Path::new(first_command.get_program());
    let second_executable = std::path::Path::new(second_command.get_program());
    assert_ne!(first_executable, second_executable);
    assert_eq!(identity(first_executable), source_identity);
    assert_eq!(identity(second_executable), source_identity);
    drop(first);
    assert!(!first_executable.exists());
    assert_eq!(identity(&source), source_identity);
    assert_eq!(identity(second_executable), source_identity);
    drop(second);
    assert!(!second_executable.exists());
    assert_eq!(identity(&source), source_identity);
}

// Invoked only as an exact child with the test-only case environment set.
#[test]
#[ignore]
fn launch_test_watchdog_fixture() {
    println!("watchdog fixture ready");
    match std::env::var("KT_LIVE_LAUNCH_TEST_CASE").unwrap().as_str() {
        "hung" => loop {
            std::thread::park();
        },
        "noisy" => {
            use std::io::Write as _;
            std::io::stdout().write_all(&vec![b'x'; 262144]).unwrap();
            std::io::stderr().write_all(&vec![b'y'; 262144]).unwrap();
            loop {
                std::thread::park();
            }
        }
        "stderr" => {
            use std::io::Write as _;
            std::io::stderr().write_all(&vec![b'y'; 262144]).unwrap();
            loop {
                std::thread::park();
            }
        }
        "normal" => (),
        other => panic!("unknown fixture case: {other}"),
    }
}

fn watchdog_case(case: &str, expected: Option<&str>) {
    let mut dir = LaunchTestDir::new();
    let mut command = launch_test_command(&dir.path, "live::tests::launch_test_watchdog_fixture", case);
    command.arg("--ignored");
    // 子はこのテストバイナリ自身なので、負荷が高いと準備完了までに 2 秒を超える。期限は準備完了の後に来るようにする
    let limit = Duration::from_secs(5);
    let began = Instant::now();
    let output = capture_exact_child(&mut command, &mut dir, limit, 4096);
    let ready = String::from_utf8_lossy(&output.stdout).contains("watchdog fixture ready");
    eprintln!(
        "watchdog reaped={} exit_code={} stdout_bytes={} stderr_bytes={} ready={}",
        u8::from(output.status.is_some()),
        output.status.and_then(|status| status.code()).unwrap_or(-1),
        output.stdout_bytes,
        output.stderr_bytes,
        u8::from(ready)
    );
    assert_eq!(output.reason, expected);
    assert!(output.status.is_some(), "retained child must be reaped before cleanup");
    assert!(began.elapsed() < limit + Duration::from_secs(3));
    assert!(output.stdout.len() + output.stderr.len() <= 4096, "capture must be bounded");
    assert!(ready, "child must reach the controlled fixture before watchdog termination");
    if expected.is_none() {
        assert!(output.status.unwrap().success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
    let path = dir.path.clone();
    drop(dir);
    assert!(!path.exists(), "reaped child fixture can be removed");
}

#[test]
fn launch_test_watchdog_stops_hung_exact_child() {
    watchdog_case("hung", Some("deadline"));
}

#[test]
fn launch_test_watchdog_caps_noisy_exact_child() {
    watchdog_case("noisy", Some("output limit"));
    watchdog_case("stderr", Some("output limit"));
}

#[test]
fn launch_test_watchdog_accepts_normal_exact_child() {
    watchdog_case("normal", None);
}

// Only child processes receive this HOME/data namespace. Placing the test
// executable here also confines the beside-app search; no installed helper runs.
fn isolated_launch_child(test: &str, cases: &[&str]) -> Option<(PathBuf, String)> {
    const ROOT: &str = "KT_LIVE_LAUNCH_TEST_ROOT";
    const CASE: &str = "KT_LIVE_LAUNCH_TEST_CASE";
    if let Some(dir) = std::env::var_os(ROOT) {
        return Some((PathBuf::from(dir), std::env::var(CASE).unwrap()));
    }
    for case in cases {
        let mut dir = LaunchTestDir::new();
        let mut command = launch_test_command(&dir.path, test, case);
        let output = capture_exact_child(&mut command, &mut dir, Duration::from_secs(45), 65536);
        assert!(
            output.reason.is_none() && output.status.is_some_and(|status| status.success()),
            "{test} case={case}: {:?}, {:?}, fixture={}\nstdout: {}\nstderr: {}",
            output.status,
            output.reason,
            dir.path.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"), "exact child must run one test");
        eprintln!("{test} case={case}: passed");
    }
    None
}

fn install_launch_fixture(dir: &std::path::Path, case: &str, bytes: &[u8]) -> Option<PathBuf> {
    let name = crate::helpers::exe("tune-agent");
    let beside = dir.join("owned app").join(&name);
    let managed = dir.join("synthetic-home").join(".katala-tune").join("bin").join(&name);
    assert_eq!(std::env::current_exe().unwrap().parent().unwrap(), beside.parent().unwrap());
    assert_eq!(crate::helpers::bin_dir(), managed.parent().unwrap());
    #[cfg(not(windows))]
    assert_eq!(crate::nodes::home(), dir.join("synthetic-home"));
    assert!(!beside.exists() && !managed.exists());
    if case == "absent" {
        assert_eq!(local_native(), None);
        return None;
    }
    assert!(case == "managed" || case == "bundled");
    std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
    std::fs::write(&managed, bytes).unwrap();
    if case == "bundled" {
        std::fs::write(&beside, bytes).unwrap();
        assert_eq!(local_native(), Some(beside.clone()), "beside-app wins when both exist");
        Some(beside)
    } else {
        std::fs::create_dir(&beside).unwrap();
        assert_eq!(local_native(), Some(managed.clone()));
        Some(managed)
    }
}

#[test]
fn launch_follows_the_probe_conventions() {
    let Some((dir, case)) = isolated_launch_child("live::tests::launch_follows_the_probe_conventions", &["absent", "managed", "bundled"]) else { return };
    let native = install_launch_fixture(&dir, &case, b"owned fixture - never execute");
    let o = Opts::default();
    let mac = launch(&node("m", "macos"), &o).unwrap();
    assert_eq!(mac.program, "ssh");
    let cmd = mac.args.last().unwrap();
    assert!(
        cmd.starts_with("[ -x ~/.katala-tune/bin/tune-agent ] && ~/.katala-tune/bin/tune-agent sample interval=1 procs=5 max=900 watch=1 || { "),
        "ネイティブを先に使う: {cmd}"
    );
    assert!(cmd.contains(&format!("exec python3 -c '{}' interval=1 procs=5 max=900 watch=1", python_boot(MAC_LIVE.len()))), "{cmd}");
    assert_eq!(cmd.matches('\'').count(), 4, "python3 -c の中身にシングルクォートを入れない");
    for opt in ["BatchMode=yes", "ControlMaster=no", "ControlPath=none"] {
        assert!(mac.args.contains(&opt.to_string()), "{opt}");
    }
    assert!(mac.keep_stdin && mac.stdin == MAC_LIVE.as_bytes());
    let win = launch(&node("w", "windows"), &Opts { interval: Duration::from_millis(1500), ..o.clone() }).unwrap();
    let cmd = win.args.last().unwrap();
    assert!(
            cmd.starts_with("if [ -x ~/.katala-tune/bin/tune-agent.exe ]; then exec ~/.katala-tune/bin/tune-agent.exe sample interval=1.5 procs=5 max=900 watch=1; else exec powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand "),
            "{cmd}"
        );
    assert!(cmd.ends_with("; fi"), "{cmd}");
    assert!(!cmd.contains("live.ps1") && !cmd.contains("mkdir") && !cmd.contains("-File"));
    let boot = windows_boot(WIN_LIVE.len() - 3, "-Interval 1.5 -ProcEvery 5 -MaxSeconds 900 -WatchStdin");
    assert!(boot.contains("OpenStandardInput") && boot.contains(".Read($b,$p,"));
    assert!(boot.ends_with("-Interval 1.5 -ProcEvery 5 -MaxSeconds 900 -WatchStdin"));
    assert!(win.keep_stdin && win.args.contains(&"ControlPath=none".to_string()));
    assert_eq!(win.stdin, WIN_LIVE[3..]);
    assert!(win.cleanup.is_none());
    let mut local_node = node("local", "windows");
    local_node.local = true;
    let local = launch(&local_node, &o).unwrap();
    assert!(local.cleanup.is_none() && local.keep_stdin);
    if let Some(native) = native.as_ref() {
        assert_eq!(local.program, native.to_str().unwrap());
        #[cfg(windows)]
        assert!(local.program.ends_with("tune-agent.exe"));
        assert!(local.stdin.is_empty());
        assert_eq!(local.args, ["sample", "interval=1", "procs=5", "max=900", "watch=1"]);
    } else {
        assert_eq!(local.program, "powershell.exe");
        assert_eq!(local.stdin, WIN_LIVE[3..]);
        assert_eq!(local.args[..4], ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand"]);
        assert_eq!(local.args.len(), 5);
    }
    local_node.os = "macos".into();
    let local = launch(&local_node, &o).unwrap();
    assert!(local.keep_stdin && local.cleanup.is_none());
    assert_eq!(local.stdin, MAC_LIVE.as_bytes());
    if let Some(native) = native {
        assert_eq!(local.program, "/bin/sh");
        assert_eq!(
            local.args,
            [
                "-c",
                &format!(
                    "\"$0\" sample interval=1 procs=5 max=900 watch=1 || exec /usr/bin/env python3 -c '{}' interval=1 procs=5 max=900 watch=1",
                    python_boot(MAC_LIVE.len())
                ),
                native.to_str().unwrap()
            ]
        );
    } else {
        assert_eq!(local.program, "/usr/bin/env");
        assert_eq!(local.args, ["python3", "-c", &python_boot(MAC_LIVE.len()), "interval=1", "procs=5", "max=900", "watch=1"]);
    }
    assert!(WIN_LIVE[3..].iter().all(|b| b.is_ascii()), "probes/live_win.ps1 は ASCII のみ");
    assert!(launch(&node("x", "linux"), &o).is_err());
}

/// 本番の spawn 経路で stderr flood 後の初回出力・scope・stdin 保持と回収を検証する。
#[cfg(windows)]
#[tokio::test]
async fn windows_boot_preserves_script_scope_and_stdin_lifetime() {
    let script = b"[Console]::Error.Write(('E' * 131072));$value=42;function Scope {$script:value};[Console]::WriteLine((Scope));$io=[Console]::OpenStandardInput();[Console]::WriteLine($io.ReadByte());[Console]::WriteLine($io.ReadByte())";
    let boot = windows_boot(script.len(), "");
    let utf16: Vec<u8> = boot.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut stdin = script.to_vec();
    stdin.push(b'Q');
    let mut source = spawn(Launch {
        program: "powershell.exe".into(),
        args: vec!["-NoLogo".into(), "-NoProfile".into(), "-NonInteractive".into(), "-EncodedCommand".into(), crate::logs::base64(&utf16)],
        stdin,
        keep_stdin: true,
        cleanup: None,
    })
    .unwrap();
    let first_line_timeout = Duration::from_secs(15);
    let scope = match tokio::time::timeout(first_line_timeout, source.lines.recv()).await {
        Ok(Some(scope)) => scope,
        Ok(None) => panic!("PowerShell stdout closed before the first line"),
        Err(_) => {
            let ended = tokio::time::timeout(Duration::from_secs(5), source.close()).await;
            panic!("PowerShell produced no first stdout line within {first_line_timeout:?}; shutdown: {ended:?}");
        }
    };
    assert_eq!(scope.trim_end(), "42", "dot-source must preserve $script scope");
    let first_byte = tokio::time::timeout(Duration::from_secs(5), source.lines.recv())
        .await
        .expect("PowerShell should read the first byte")
        .expect("PowerShell stdout closed before the first byte");
    assert_eq!(first_byte.trim_end(), "81", "bootstrap must consume exactly script.len() bytes");
    assert!(tokio::time::timeout(Duration::from_millis(100), source.lines.recv()).await.is_err(), "must remain alive while stdin stays open");
    let ended = tokio::time::timeout(Duration::from_secs(5), source.close()).await.unwrap();
    assert_eq!(ended.code, Some(0), "closing the source should close stdin and let PowerShell exit: {ended:?}");
    assert!(ended.err.contains("EEEE"), "spawn must drain the PowerShell stderr flood: {ended:?}");
    assert!(ended.err.len() <= 4096, "spawn must retain only the bounded stderr tail: {} bytes", ended.err.len());
}

/// Bootstrap must retain its remaining-byte loop so short pipe reads cannot truncate the script.
#[test]
fn windows_boot_accumulates_partial_reads_until_script_length() {
    let script_len = 7;
    let boot = windows_boot(script_len, "");
    let read_loop = format!(";while($p -lt {script_len}){{$r=$i.Read($b,$p,({script_len}-$p));if($r -eq 0){{exit 2}};$p+=$r}};");
    assert!(boot.contains(&read_loop), "bootstrap must read remaining bytes after each partial read: {boot}");
}

// ---- 偽のサンプラー（一定間隔で行を出すローカルのスクリプト）で、開始・停止・自動停止・上限・つなぎ直しを確かめる ----

/// sh で動かす偽のサンプラー。$1 に機体の id、$KT_DIR に記録の置き場所（起動ごとに pid を書く）
struct Fake {
    script: String,
    dir: PathBuf,
    opened: AtomicUsize,
}

impl Route for Fake {
    fn open(&self, node: &Node, _o: &Opts) -> Result<Source, String> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        let script = format!("KT_DIR='{}'; {}", self.dir.display(), self.script);
        spawn(Launch { program: "/bin/sh".into(), args: vec!["-c".into(), script, "sh".into(), node.id.clone()], ..Launch::default() })
    }
}

const STEADY: &str = r#"echo $$ >> "$KT_DIR/pids-$1"; echo '{"type":"hello","v":1,"os":"fake"}'; while true; do echo '{"type":"s","t":1,"cpu":5,"cores":[5,5],"mem":{"used_pct":50},"procs":{"count":1,"top_cpu":[{"pid":1,"name":"x","cpu":1}]}}'; sleep 0.05; done"#;

fn fake(script: &str) -> (Arc<Fake>, PathBuf) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("kt-live-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&dir).unwrap();
    (Arc::new(Fake { script: script.into(), dir: dir.clone(), opened: AtomicUsize::new(0) }), dir)
}

fn quick() -> Opts {
    Opts {
        interval: Duration::from_millis(50),
        idle_stop: Duration::from_secs(60),
        first_line_timeout: Duration::from_secs(5),
        stall_timeout: Duration::from_secs(5),
        backoff: vec![Duration::from_millis(20)],
        flush: Duration::from_millis(50),
        ..Opts::default()
    }
}

fn events() -> (Sink, Arc<Mutex<Vec<Value>>>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = got.clone();
    (Box::new(move |v| g.lock().unwrap().push(v)), got)
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("待っても {what} にならない");
}

fn pids(dir: &std::path::Path, id: &str) -> Vec<u32> {
    std::fs::read_to_string(dir.join(format!("pids-{id}"))).unwrap_or_default().lines().filter_map(|l| l.trim().parse().ok()).collect()
}

fn alive(pid: u32) -> bool {
    std::process::Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn state(l: &Live, id: &str) -> (String, String) {
    let s = l.status();
    (s[id]["state"].as_str().unwrap_or("").into(), s[id]["reason"].as_str().unwrap_or("").into())
}

#[tokio::test]
async fn owned_release_preserves_the_other_owner_and_stops_the_last() {
    if cfg!(windows) {
        return;
    }
    for (first, last) in [(Owner::Ui, Owner::Monitor), (Owner::Monitor, Owner::Ui)] {
        let (route, dir) = fake(STEADY);
        let live = Live::new(route.clone(), quick(), Box::new(|_| {}));
        let nodes = [node("a", "macos")];
        assert_eq!(live.start_owned(first, &nodes, &[])["started"], json!(["a"]));
        assert_eq!(live.start_owned(last, &nodes, &[])["running"], json!(["a"]));
        assert_eq!(live.start_owned(first, &nodes, &[])["running"], json!(["a"]), "repeated claim is idempotent");
        until("initial samples", || live.samples("a").len() >= 2).await;
        let before = live.samples("a").len();
        let released = live.release_owned(first, &["a".into()], Stop::Hidden);
        let still_active = live.active();
        let renewed = live.heartbeat_owned(last, "a");
        assert!(!live.heartbeat_owned(first, "a"));
        assert_eq!(live.release_owned(first, &["a".into()], Stop::User)["stopped"], json!([]));
        if still_active == ["a"] {
            until("samples after owner release", || live.samples("a").len() > before).await;
        }
        let final_release = live.release_owned(last, &["a".into()], Stop::User);
        until("last owner stopped", || state(&live, "a").0 == "stopped").await;
        let opened = route.opened.load(Ordering::SeqCst);
        let reason = state(&live, "a").1;
        live.shutdown(Duration::from_secs(1)).await;
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!(released["stopped"], json!([]), "one owner cannot stop the other");
        assert_eq!(still_active, ["a"]);
        assert!(renewed);
        assert_eq!(final_release["stopped"], json!(["a"]));
        assert_eq!(reason, "user");
        assert_eq!(opened, 1, "owners share the same sampler");
    }
}

#[tokio::test]
async fn owned_heartbeat_and_unclaimed_release_cannot_acquire_or_stop_a_stream() {
    if cfg!(windows) {
        return;
    }
    let (route, dir) = fake(STEADY);
    let live = Live::new(route.clone(), quick(), Box::new(|_| {}));
    assert!(!live.heartbeat_owned(Owner::Ui, "missing"));
    live.start_owned(Owner::Monitor, &[node("a", "macos")], &[]);
    until("initial sample", || !live.samples("a").is_empty()).await;
    let renewed_without_claim = live.heartbeat_owned(Owner::Ui, "a");
    let released_without_claim = live.release_owned(Owner::Ui, &["a".into()], Stop::User);
    let active = live.active();
    live.release_owned(Owner::Monitor, &["a".into()], Stop::User);
    live.shutdown(Duration::from_secs(1)).await;
    let _ = std::fs::remove_dir_all(dir);
    assert!(!renewed_without_claim, "renewal must not claim another owner's stream");
    assert_eq!(released_without_claim["stopped"], json!([]));
    assert_eq!(active, ["a"]);
    assert_eq!(route.opened.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn owned_refused_start_never_claims_a_later_legacy_stream() {
    if cfg!(windows) {
        return;
    }
    let (route, dir) = fake(STEADY);
    let live = Live::new(route, Opts { max_streams: 1, ..quick() }, Box::new(|_| {}));
    live.start_owned(Owner::Monitor, &[node("a", "macos")], &[]);
    let refused = live.start_owned(Owner::Ui, &[node("b", "macos")], &["missing".into()]);
    assert_eq!(refused["started"], json!([]));
    assert_eq!(refused["running"], json!([]));
    assert_eq!(refused["refused"][0]["reason"], "unknown");
    assert_eq!(refused["refused"][1]["reason"], "limit");
    live.release_owned(Owner::Monitor, &["a".into()], Stop::User);
    assert_eq!(live.start(&[node("b", "macos")], &[])["started"], json!(["b"]));
    assert!(!live.heartbeat_owned(Owner::Ui, "b"));
    assert_eq!(live.release_owned(Owner::Ui, &["b".into()], Stop::User)["stopped"], json!([]));
    assert_eq!(live.active(), ["b"]);
    assert!(live.heartbeat("b"), "legacy heartbeat still renews a legacy stream");
    assert_eq!(live.stop(None, Stop::User)["stopped"], json!(["b"]));
    live.shutdown(Duration::from_secs(1)).await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn owned_administrative_stop_exclusion_restart_and_shutdown_override_both_owners() {
    if cfg!(windows) {
        return;
    }
    for reason in [Stop::User, Stop::Excluded, Stop::Restart, Stop::AppExit] {
        let (route, dir) = fake(STEADY);
        let live = Live::new(route, quick(), Box::new(|_| {}));
        let nodes = [node("a", "macos")];
        live.start_owned(Owner::Ui, &nodes, &[]);
        live.start_owned(Owner::Monitor, &nodes, &[]);
        assert_eq!(live.stop(None, reason)["stopped"], json!(["a"]));
        assert!(!live.heartbeat_owned(Owner::Ui, "a"));
        assert!(!live.heartbeat_owned(Owner::Monitor, "a"));
        live.shutdown(Duration::from_secs(1)).await;
        assert_eq!(live.start_owned(Owner::Monitor, &nodes, &[])["refused"][0]["reason"], "app_exit");
        let _ = std::fs::remove_dir_all(dir);
    }
    for excluded in [Vec::new(), vec![Node::from_value(&json!({"id":"a","status":"paused"}), "synthetic-host")]] {
        let (route, dir) = fake(STEADY);
        let live = Live::new(route, quick(), Box::new(|_| {}));
        live.start_owned(Owner::Ui, &[node("a", "macos")], &[]);
        live.start_owned(Owner::Monitor, &[node("a", "macos")], &[]);
        live.retain_active(&excluded);
        assert!(live.active().is_empty(), "the full ledger removes both claims");
        live.shutdown(Duration::from_secs(1)).await;
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[tokio::test]
async fn owned_heartbeat_renews_deadline_but_never_restarts_exhausted_retries() {
    if cfg!(windows) {
        return;
    }
    let (route, dir) = fake(STEADY);
    let live = Live::new(route, quick(), Box::new(|_| {}));
    live.start_owned(Owner::Monitor, &[node("a", "macos")], &[]);
    live.lock().streams.get_mut("a").unwrap().last_hb = Instant::now() - quick().idle_stop - Duration::from_secs(1);
    assert!(live.heartbeat_owned(Owner::Monitor, "a"));
    live.tick();
    assert_eq!(live.active(), ["a"]);
    live.shutdown(Duration::from_secs(1)).await;
    let _ = std::fs::remove_dir_all(dir);

    let (route, dir) = fake("exit 255");
    let live = Live::new(route.clone(), Opts { max_retries: 0, ..quick() }, Box::new(|_| {}));
    live.start_owned(Owner::Ui, &[node("a", "macos")], &[]);
    until("retry exhaustion", || state(&live, "a").0 == "stopped").await;
    assert!(!live.heartbeat_owned(Owner::Ui, "a"));
    assert!(!live.heartbeat_owned(Owner::Monitor, "a"));
    assert_eq!(route.opened.load(Ordering::SeqCst), 1);
    live.shutdown(Duration::from_secs(1)).await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn owned_stale_finish_cannot_erase_the_replacement_stream_or_its_owner() {
    if cfg!(windows) {
        return;
    }
    let (route, dir) = fake(STEADY);
    let live = Live::new(route, quick(), Box::new(|_| {}));
    live.start_owned(Owner::Monitor, &[node("a", "macos")], &[]);
    let old = live.lock().streams["a"].epoch;
    live.release_owned(Owner::Monitor, &["a".into()], Stop::User);
    live.start_owned(Owner::Ui, &[node("a", "macos")], &[]);
    live.finish("a", old, Stop::Disconnected("old transport".into()));
    assert!(live.heartbeat_owned(Owner::Ui, "a"));
    assert!(!live.heartbeat_owned(Owner::Monitor, "a"));
    assert_eq!(live.release_owned(Owner::Monitor, &["a".into()], Stop::User)["stopped"], json!([]));
    assert_eq!(live.active(), ["a"]);
    live.shutdown(Duration::from_secs(1)).await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn streams_samples_and_stop_kills_the_sampler() {
    if cfg!(windows) {
        return;
    }
    let (route, dir) = fake(STEADY);
    let (sink, got) = events();
    let live = Live::new(route.clone(), quick(), sink);
    let r = live.start(&[node("a", "macos")], &["ghost".into()]);
    assert_eq!(r["started"], json!(["a"]));
    assert_eq!(r["refused"][0]["reason"], "unknown");
    until("3 点たまる", || live.samples("a").len() >= 3).await;
    assert_eq!(state(&live, "a").0, "running");
    live.tick();
    let ev = got.lock().unwrap().last().cloned().unwrap();
    let a = &ev["nodes"]["a"];
    assert!(a["points"].as_array().unwrap().len() >= 3);
    assert_eq!((a["state"].as_str(), a["cores"].as_array().map(Vec::len)), (Some("running"), Some(2)));
    assert!(a["procs"]["top_cpu"].is_array() && a["info"]["os"] == "fake");
    // 次の tick には新しい点だけ（procs・info は変わったときだけ）
    let before = got.lock().unwrap().len();
    until("次の点", || live.samples("a").len() >= 6).await;
    live.tick();
    let next = got.lock().unwrap()[before]["nodes"]["a"].clone();
    assert!(next.get("info").is_none());
    // 2 回目の start は合図（つなぎ直さない）
    assert_eq!(live.start(&[node("a", "macos")], &[])["running"], json!(["a"]));
    assert_eq!(route.opened.load(Ordering::SeqCst), 1);

    let pid = pids(&dir, "a")[0];
    assert!(alive(pid));
    assert_eq!(live.stop(None, Stop::User)["stopped"], json!(["a"]));
    until("止まる", || state(&live, "a").0 == "stopped").await;
    assert_eq!(state(&live, "a").1, "user");
    until("サンプラーが終わる", || !alive(pid)).await;
    assert!(live.active().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn stops_by_itself_when_the_screen_goes_quiet() {
    if cfg!(windows) {
        return;
    }
    let (route, dir) = fake(STEADY);
    let (sink, got) = events();
    let live = Live::new(route, Opts { idle_stop: Duration::from_millis(600), ..quick() }, sink);
    tokio::spawn(live.clone().run_ticker());
    live.start(&[node("a", "macos"), node("b", "windows")], &[]);
    until("流れる", || live.samples("a").len() >= 2 && live.samples("b").len() >= 2).await;
    // a にだけ合図を送り続ける
    for _ in 0..8 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        live.start(&[node("a", "macos")], &[]);
    }
    assert_eq!(state(&live, "b"), ("stopped".into(), "idle".into()), "合図の無い b は自動で止まる");
    assert_eq!(state(&live, "a").0, "running", "合図のある a は続く");
    until("b のサンプラーが終わる", || pids(&dir, "b").iter().all(|p| !alive(*p))).await;
    assert!(got.lock().unwrap().iter().any(|e| e["nodes"]["b"]["reason"] == "idle"), "止まった理由を画面へ送る");
    until("a も止まる", || state(&live, "a").0 == "stopped").await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn refuses_beyond_the_limit() {
    if cfg!(windows) {
        return;
    }
    let (route, dir) = fake(STEADY);
    let (sink, _) = events();
    let live = Live::new(route, Opts { max_streams: 2, ..quick() }, sink);
    let r = live.start(&[node("a", "macos"), node("b", "macos"), node("c", "macos")], &[]);
    assert_eq!((r["started"].clone(), r["refused"][0]["id"].clone(), r["refused"][0]["reason"].clone()), (json!(["a", "b"]), json!("c"), json!("limit")));
    assert_eq!(state(&live, "c"), ("stopped".into(), "limit".into()));
    // 1 台止めれば流せる
    live.stop(Some(&["a".to_string()]), Stop::User);
    assert_eq!(live.start(&[node("c", "macos")], &[])["started"], json!(["c"]));
    live.shutdown(Duration::from_secs(5)).await;
    assert!(live.active().is_empty());
    assert_eq!(live.start(&[node("a", "macos")], &[])["refused"][0]["reason"], "app_exit", "終了した後は流さない");
    for id in ["a", "b", "c"] {
        assert!(pids(&dir, id).iter().all(|p| !alive(*p)), "終了で {id} のサンプラーも終わる");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn gives_up_after_the_retry_limit() {
    if cfg!(windows) {
        return;
    }
    // 1 行出してすぐ終わる（切れる）サンプラー
    let (route, dir) = fake(r#"echo '{"type":"s","cpu":1}'; echo 'Connection closed by remote host' >&2; exit 255"#);
    let (sink, _) = events();
    let live = Live::new(route.clone(), Opts { max_retries: 2, ..quick() }, sink);
    live.start(&[node("a", "macos")], &[]);
    until("諦める", || state(&live, "a").0 == "stopped").await;
    let s = live.status();
    assert_eq!(s["a"]["reason"], "disconnected");
    assert!(s["a"]["detail"].as_str().unwrap().contains("exit 255: Connection closed"), "{}", s["a"]["detail"]);
    assert_eq!(route.opened.load(Ordering::SeqCst), 3, "最初の 1 回 + つなぎ直し 2 回");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn heartbeat_renews_existing_streams_without_starting_missing_or_stopped_targets() {
    if cfg!(windows) {
        return;
    }
    let (route, dir) = fake(STEADY);
    let (sink, _) = events();
    let live = Live::new(route.clone(), quick(), sink);
    assert!(!live.heartbeat("missing"));
    assert_eq!(route.opened.load(Ordering::SeqCst), 0);
    live.start(&[node("fixture", "macos")], &[]);
    until("initial sample", || state(&live, "fixture").0 == "running").await;
    // Advance just the fixture's heartbeat age instead of sleeping through
    // the production idle budget. A no-op renewal must fail the next tick.
    live.lock().streams.get_mut("fixture").unwrap().last_hb = Instant::now() - quick().idle_stop - Duration::from_secs(1);
    let renewed = live.heartbeat("fixture");
    live.tick();
    let kept_running = live.active().contains(&"fixture".to_string());
    live.stop(None, Stop::User);
    let while_stopping = live.heartbeat("fixture");
    until("stopped", || state(&live, "fixture").0 == "stopped").await;
    let after_stopping = live.heartbeat("fixture");
    let opened = route.opened.load(Ordering::SeqCst);
    live.shutdown(Duration::from_secs(1)).await;
    let _ = std::fs::remove_dir_all(dir);
    assert!(renewed, "an existing stream accepts its heartbeat");
    assert!(kept_running, "renewal resets the idle deadline");
    assert!(!while_stopping && !after_stopping, "heartbeat must never restart a stopped stream");
    assert_eq!(opened, 1);
}

#[tokio::test]
async fn heartbeat_does_not_reset_exhausted_retries() {
    if cfg!(windows) {
        return;
    }
    let (route, dir) = fake("exit 255");
    let (sink, _) = events();
    let live = Live::new(route.clone(), Opts { max_retries: 0, ..quick() }, sink);
    live.start(&[node("fixture", "macos")], &[]);
    until("retry limit", || state(&live, "fixture").0 == "stopped").await;
    let renewed = live.heartbeat("fixture");
    let opened = route.opened.load(Ordering::SeqCst);
    live.shutdown(Duration::from_secs(1)).await;
    let _ = std::fs::remove_dir_all(dir);
    assert!(!renewed);
    assert_eq!(opened, 1, "heartbeat cannot reset the sampler retry budget");
}

#[tokio::test]
async fn reconnects_without_counting_when_the_sampler_rotates() {
    if cfg!(windows) {
        return;
    }
    // max_age で区切って終わるサンプラー。つなぎ直しの上限 0 でも止まらない
    let (route, dir) = fake(r#"echo '{"type":"s","cpu":1}'; echo '{"type":"end","reason":"max_age"}'"#);
    let (sink, _) = events();
    let live = Live::new(route.clone(), Opts { max_retries: 0, ..quick() }, sink);
    live.start(&[node("a", "macos")], &[]);
    until("何度か区切る", || route.opened.load(Ordering::SeqCst) >= 4).await;
    assert_ne!(state(&live, "a").0, "stopped");
    live.stop(None, Stop::User);
    until("止まる", || state(&live, "a").0 == "stopped").await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_silent_sampler_counts_as_disconnected() {
    if cfg!(windows) {
        return;
    }
    let (route, dir) = fake(r#"echo $$ >> "$KT_DIR/pids-$1"; echo '{"type":"s","cpu":1}'; exec sleep 30"#);
    let (sink, _) = events();
    let live = Live::new(route, Opts { max_retries: 1, stall_timeout: Duration::from_millis(200), ..quick() }, sink);
    live.start(&[node("a", "macos")], &[]);
    until("諦める", || state(&live, "a").0 == "stopped").await;
    assert!(live.status()["a"]["detail"].as_str().unwrap().contains("データが来ない"));
    until("黙ったサンプラーも終わる", || pids(&dir, "a").iter().all(|p| !alive(*p))).await;
    let _ = std::fs::remove_dir_all(dir);
}

/// macOS の起動コマンドを実際のシェルで動かす: 置いたネイティブが動けばそれだけ、起動できなければ Python 版
#[cfg(target_os = "macos")]
#[tokio::test]
async fn mac_launch_prefers_native_and_falls_back_to_python() {
    use std::os::unix::fs::PermissionsExt;
    let o = Opts { sampler_max_age: Duration::from_secs(1), ..Opts::default() };
    // 古い版（macOS のサンプラーが無い）は標準入力を読まずに失敗する。動く版は hello を出して正常に終わる
    for (native, want) in [
        (
            "#!/bin/sh
exit 1
",
            None,
        ),
        (
            "#!/bin/sh
echo '{\"type\":\"hello\",\"os\":\"macos\",\"impl\":\"rust\"}'
",
            Some("rust"),
        ),
    ] {
        let (_, dir) = fake("");
        let bin = dir.join(".katala-tune/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("tune-agent"), native).unwrap();
        std::fs::set_permissions(bin.join("tune-agent"), std::fs::Permissions::from_mode(0o700)).unwrap();
        let l = launch(&node("m", "macos"), &o).unwrap();
        let remote = l.args.last().unwrap().clone();
        let mut src = spawn(Launch {
            program: "/usr/bin/env".into(),
            args: vec![format!("HOME={}", dir.display()), "PATH=/usr/bin:/bin".into(), "/bin/sh".into(), "-c".into(), remote],
            stdin: l.stdin,
            keep_stdin: true,
            cleanup: None,
        })
        .unwrap();
        // Python 版は max=1 で自分から終わる
        let mut msgs = Vec::new();
        while let Ok(Some(line)) = tokio::time::timeout(Duration::from_secs(20), src.lines.recv()).await {
            msgs.push(parse_line(&line).unwrap_or_else(|| panic!("読めない行: {line}")));
        }
        let ended = src.close().await;
        assert_eq!(ended.code, Some(0), "{}", ended.err);
        let Some(Msg::Hello(h)) = msgs.first() else { panic!("1 行目は hello") };
        assert_eq!(h.implementation.as_deref(), want, "{native}");
        assert_eq!(msgs.iter().filter(|m| matches!(m, Msg::Hello(_))).count(), 1, "動くのはどちらか一方");
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// 必須のOS統計を保ち、任意センサーの有無を独立した環境で確かめる。
#[cfg(target_os = "macos")]
#[tokio::test]
async fn mac_sampler_runs_here() {
    use std::os::unix::fs::PermissionsExt;
    for with_sensor in [false, true] {
        // センサーの最初の値は別の python の起動を待つ。負荷が高いと数秒かかる（全テストの並列実行や他のビルドと重なると
        // 10 点 = 約 3 秒では最後の標本にも値が届かない）ので、センサーありは標本を多めに取る
        let count = if with_sensor { 30 } else { 3 };
        let (_, dir) = fake("");
        if with_sensor {
            let sensor = dir.join("macmon");
            std::fs::write(
                &sensor,
                r#"#!/usr/bin/python3
import json,os,time
from pathlib import Path
Path(__file__).with_suffix('.pid').write_text(str(os.getpid()))
for _ in range(400):
    print(json.dumps({'cpu_power':0,'gpu_power':3,'all_power':4,'sys_power':0,'gpu_freq_mhz':500}),flush=True)
    time.sleep(0.05)
"#,
            )
            .unwrap();
            std::fs::set_permissions(sensor, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut src = spawn(Launch {
            program: "/usr/bin/env".into(),
            args: [
                format!("PATH={}", dir.display()),
                format!("HOME={}", dir.display()),
                "/usr/bin/python3".into(),
                "-".into(),
                "interval=0.3".into(),
                "procs=0.3".into(),
                format!("count={count}"),
            ]
            .into(),
            stdin: MAC_LIVE.as_bytes().to_vec(),
            ..Launch::default()
        })
        .unwrap();
        let mut msgs = Vec::new();
        while let Ok(Some(l)) = tokio::time::timeout(Duration::from_secs(20), src.lines.recv()).await {
            msgs.push(parse_line(&l).unwrap_or_else(|| panic!("読めない行: {l}")));
        }
        let ended = src.close().await;
        assert_eq!(ended.code, Some(0), "{}", ended.err);
        let Msg::Hello(h) = &msgs[0] else { panic!("1 行目は hello") };
        assert_eq!(h.os.as_deref(), Some("macos"));
        assert_eq!(h.errors, if with_sensor { json!([]) } else { json!(["power: FileNotFoundError: macmon unavailable"]) }, "予期しない欠測: {:?}", h.errors);
        assert_eq!(h.has.get("gpu"), Some(&json!(with_sensor)));
        let samples: Vec<Sample> = msgs
            .iter()
            .filter_map(|m| match m {
                Msg::Sample(s) => Some(Sample::from_raw((**s).clone(), now_ms()).0),
                _ => None,
            })
            .collect();
        assert_eq!(samples.len(), count);
        let last = samples.last().unwrap();
        assert!(last.cpu.is_some() && last.cores.len() == h.cores.unwrap() as usize);
        assert!(last.mem.as_ref().and_then(|m| m.used_pct).is_some());
        assert!(last.disk.is_some() && last.net.is_some());
        assert!(matches!(msgs.last(), Some(Msg::End(r)) if r == "count"));
        if with_sensor {
            let power = last.power.as_ref().unwrap();
            assert_eq!((power.package_w, power.soc_w, power.platform_w, power.available), (Some(0.0), Some(4.0), None, Some(true)));
            assert_eq!(power.package_source.as_deref(), Some("macmon IOReport CPU"));
            let gpu = &last.gpu.as_ref().unwrap()[0];
            assert_eq!((gpu.power_w, gpu.clocks_graphics_mhz), (Some(3.0), Some(500.0)));
            let pid: u32 = std::fs::read_to_string(dir.join("macmon.pid")).unwrap().parse().unwrap();
            assert!(!alive(pid), "サンプラー終了時にセンサーの子も終わる");
        } else {
            for sample in &samples {
                assert_eq!(sample.power, Some(Power { available: Some(false), ..Power::default() }));
                let gpu = &sample.gpu.as_ref().unwrap()[0];
                assert_eq!(gpu.power_w, None, "欠測をGPUの0Wとして作らない");
                assert_eq!(gpu.mem_used_mb, None, "統合メモリのGPU割当は未取得");
                assert!(matches!(gpu.memory_status.as_deref(), Some("not-applicable") | Some("unsupported")));
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// アプリと同じ起動のしかた（標準入力を開いたまま）で動かし、止めると標準入力が閉じてサンプラーが自分で終わる
#[cfg(target_os = "macos")]
#[tokio::test]
async fn mac_sampler_ends_by_itself_when_stdin_closes() {
    let me = Node::from_value(&json!({ "id": "me", "alias": "me", "os": "macos", "local_hostname": "this-host" }), "this-host");
    let Some((dir, case)) = isolated_launch_child("live::tests::mac_sampler_ends_by_itself_when_stdin_closes", &["absent"]) else { return };
    assert_eq!(install_launch_fixture(&dir, &case, b""), None);
    let l = launch(&me, &Opts::default()).unwrap();
    assert!(l.keep_stdin && l.args[1] == "-c");
    assert_eq!(l.program, "/usr/bin/env");
    assert_eq!(l.stdin, MAC_LIVE.as_bytes());
    let mut src = spawn(with_isolated_mac_sensor_env(l, &dir)).unwrap();
    let mut got = 0;
    while got < 2 {
        let line = tokio::time::timeout(Duration::from_secs(20), src.lines.recv()).await.unwrap().expect("行が来る");
        if matches!(parse_line(&line), Some(Msg::Sample(_))) {
            got += 1;
        }
    }
    let t0 = Instant::now();
    let ended = src.close().await;
    // 強制終了ならシグナルで終わり code は None。0 は標準入力が閉じたのを見て自分で終わったということ
    assert_eq!(ended.code, Some(0), "{}", ended.err);
    assert!(t0.elapsed() < Duration::from_millis(2500));
}

// collect::command enriches PATH. Set these again in the spawned process so
// the real Python script cannot discover a normal HOME/Homebrew macmon.
#[cfg(target_os = "macos")]
fn with_isolated_mac_sensor_env(mut launch: Launch, dir: &std::path::Path) -> Launch {
    let mut args = vec![format!("HOME={}", dir.join("synthetic-home").display()), "PATH=/usr/bin:/bin".into(), launch.program];
    args.extend(launch.args);
    launch.program = "/usr/bin/env".into();
    launch.args = args;
    launch
}

/// Owned fake: this checks the native launcher's stdin/EOF contract only.
/// Actual native sampler EOF behavior stays covered by tune-agent tests.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn mac_native_launcher_preserves_stdin_until_close() {
    use std::os::unix::fs::PermissionsExt as _;
    let Some((dir, case)) = isolated_launch_child("live::tests::mac_native_launcher_preserves_stdin_until_close", &["managed", "bundled"]) else { return };
    let script = b"#!/bin/sh\nprintf '%s\\n' '{\"type\":\"hello\",\"impl\":\"rust\",\"os\":\"macos\"}' '{\"type\":\"s\",\"seq\":1}' '{\"type\":\"s\",\"seq\":2}'\nwhile IFS= read -r line; do :; done\n";
    let native = install_launch_fixture(&dir, &case, script).unwrap();
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut me = node("me", "macos");
    me.local = true;
    let l = launch(&me, &Opts::default()).unwrap();
    assert_eq!(l.program, "/bin/sh");
    assert_eq!(l.args[0], "-c");
    assert_eq!(l.args[2], native.to_str().unwrap());
    assert!(l.keep_stdin && l.cleanup.is_none());
    assert_eq!(l.stdin, MAC_LIVE.as_bytes());
    let mut src = spawn(with_isolated_mac_sensor_env(l, &dir)).unwrap();
    for expected in ["hello", "s", "s"] {
        let line = tokio::time::timeout(Duration::from_secs(20), src.lines.recv()).await.unwrap().expect("fixture frame");
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["type"], expected);
        if expected == "hello" {
            assert_eq!(value["impl"], "rust");
        }
    }
    assert!(tokio::time::timeout(Duration::from_millis(100), src.lines.recv()).await.is_err(), "owned fake stays alive while stdin is open");
    let t0 = Instant::now();
    let ended = src.close().await;
    assert_eq!(ended.code, Some(0), "{}", ended.err);
    assert!(t0.elapsed() < Duration::from_millis(2500));
}
#[tokio::test]
async fn journal_planned_recycle_eof_and_privacy() {
    if cfg!(windows) {
        return;
    }
    let (route, dir) = fake(
        r#"echo $$ >> "$KT_DIR/pids-$1"; echo '{"type":"hello","impl":"rust","agent":"0.6.4","errors":"dummy-private-canary"}'; echo '{"type":"s","seq":1}'; echo '{"type":"end","reason":"max_age"}'; echo dummy-private-canary >&2"#,
    );
    let db = Arc::new(Mutex::new(crate::db::Store::open_in_memory().unwrap()));
    let journal = Journal::start(db.clone());
    let live = Live::new(route.clone(), quick(), Box::new(|_| {}));
    live.bind_journal(journal.clone());
    live.start(&[node("fixture", "macos")], &[]);
    until("3 open", || route.opened.load(Ordering::SeqCst) >= 3).await;
    live.shutdown(Duration::from_secs(3)).await;
    assert!(journal.flush(Duration::from_secs(2)).await);
    let v = db.lock().unwrap().monitor_events(&crate::monitor_journal::Filter { limit: Some(500), ..Default::default() }).unwrap();
    assert!(!v.to_string().contains("dummy-private-canary"));
    let xs = v["events"].as_array().unwrap();
    assert!(xs.iter().any(|e| e["kind"] == "first_sample" && e["data"]["transport_pid"].is_number()));
    assert!(xs.iter().any(|e| e["kind"] == "retry" && e["data"]["policy"] == "planned_recycle"));
    assert!(xs.iter().any(|e| e["kind"] == "end" && e["data"]["stdout"] == "eof" && e["data"]["exit_code"] == 0));
    assert!(!xs.iter().any(|e| e["kind"] == "stopped" && e["data"]["reason"] == "disconnected"));
    assert!(journal.shutdown(Duration::from_secs(2)).await);
    assert!(pids(&dir, "fixture").iter().all(|p| !alive(*p)));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn journal_distinguishes_first_sample_and_stall_timeout() {
    // Classification must depend on whether a sample arrived, not whether
    // the OS happened to start a shell within the 150 ms silence window.
    // Native source termination remains covered by the adjacent tests.
    struct SilentRoute(bool);
    impl Route for SilentRoute {
        fn open(&self, _: &Node, _: &Opts) -> Result<Source, String> {
            let (lines, received) = mpsc::channel(2);
            lines.try_send(r#"{"type":"hello"}"#.into()).unwrap();
            if self.0 {
                lines.try_send(r#"{"type":"s","seq":1}"#.into()).unwrap();
            }
            let (stop, stopped) = oneshot::channel();
            let (done, ended) = oneshot::channel();
            tokio::spawn(async move {
                let _ = stopped.await;
                drop(lines);
                let _ = done.send(Ended { code: Some(0), wait_observed: Some(true), ..Ended::default() });
            });
            Ok(Source::new(received, stop, ended))
        }
    }
    for (sample, category) in [(false, "first_sample_timeout"), (true, "stall_timeout")] {
        let db = Arc::new(Mutex::new(crate::db::Store::open_in_memory().unwrap()));
        let journal = Journal::start(db.clone());
        let live = Live::new(
            Arc::new(SilentRoute(sample)),
            Opts { first_line_timeout: Duration::from_millis(150), stall_timeout: Duration::from_millis(150), max_retries: 0, ..quick() },
            Box::new(|_| {}),
        );
        live.bind_journal(journal.clone());
        live.start(&[node("fixture", "macos")], &[]);
        until("disconnected", || state(&live, "fixture").1 == "disconnected").await;
        assert!(journal.flush(Duration::from_secs(2)).await);
        let v = db.lock().unwrap().monitor_events(&crate::monitor_journal::Filter::default()).unwrap();
        let events = v["events"].as_array().unwrap();
        assert!(events.iter().any(|e| e["kind"] == "hello"));
        assert_eq!(events.iter().any(|e| e["kind"] == "first_sample"), sample);
        assert!(events.iter().any(|e| e["kind"] == "end" && e["data"]["category"] == category && e["data"]["samples"] == u64::from(sample)));
        live.shutdown(Duration::from_secs(3)).await;
        assert!(journal.shutdown(Duration::from_secs(2)).await);
    }
}

// No remote sampler: the owned local child exits zero only after Source closes stdin.
#[cfg(not(windows))]
#[tokio::test]
async fn journal_separates_timeout_cleanup_from_natural_zero_exit() {
    struct StdinRoute(bool);
    impl Route for StdinRoute {
        fn open(&self, _: &Node, _: &Opts) -> Result<Source, String> {
            let tail = if self.0 { "cat >/dev/null" } else { "exit 0" };
            spawn(Launch {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), format!("echo '{{\"type\":\"s\",\"seq\":1}}'; {tail}")],
                keep_stdin: true,
                ..Launch::default()
            })
        }
    }
    for silent in [true, false] {
        let db = Arc::new(Mutex::new(crate::db::Store::open_in_memory().unwrap()));
        let journal = Journal::start(db.clone());
        let live = Live::new(Arc::new(StdinRoute(silent)), Opts { stall_timeout: Duration::from_millis(150), max_retries: 0, ..quick() }, Box::new(|_| {}));
        live.bind_journal(journal.clone());
        live.start(&[node("fixture", "macos")], &[]);
        until("disconnected", || state(&live, "fixture").1 == "disconnected").await;
        assert!(journal.flush(Duration::from_secs(2)).await);
        let rows = db.lock().unwrap().monitor_events(&crate::monitor_journal::Filter::default()).unwrap();
        let end = rows["events"].as_array().unwrap().iter().find(|e| e["kind"] == "end").unwrap();
        let d = &end["data"];
        assert_eq!(d["exit_code"], 0);
        assert_eq!(d["wait_observed"], true);
        assert_eq!(d["category"], if silent { json!("stall_timeout") } else { Value::Null });
        assert_eq!(d["io"]["close_trigger"], if silent { "timeout" } else { "channel_end" });
        assert_eq!(d["io"]["kill_attempted"], false);
        assert_eq!(d["io"]["forwarded"], 1);
        if silent {
            assert_eq!(d["io"]["wait_origin"], "close");
            assert_eq!(d["io"]["wait_before_close"], false);
            assert!(d["io"]["sample_age_ms"].as_u64().unwrap() >= 150);
            assert_eq!(d["io"]["pre_stdout"], Value::Null);
        } else {
            assert_eq!(d["io"]["pre_stdout"], "eof");
            assert_eq!(d["io"]["sample_age_ms"], Value::Null);
        }
        assert_eq!(d["producer_pid"], Value::Null);
        assert_eq!(d["actual_source_sha256"], Value::Null);
        live.shutdown(Duration::from_secs(2)).await;
        assert!(journal.shutdown(Duration::from_secs(2)).await);
    }
}

#[tokio::test]
async fn reader_boundary_retains_partial_discard_and_error_without_payload() {
    let boundary = Arc::new(Mutex::new(ReadBoundary::new()));
    let (mut writer, reader) = tokio::io::duplex(4096);
    let (tx, mut rx) = mpsc::channel(2);
    let task = tokio::spawn(read_lines_from(reader, tx, boundary.clone()));
    writer.write_all(b"partial-secret-canary").await.unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(30), rx.recv()).await.is_err());
    let pre = boundary.lock().unwrap().snapshot();
    assert_eq!(pre.bytes, 21);
    assert_eq!(pre.forwarded, 0);
    assert!(pre.last_byte_ms.is_some() && pre.last_line_ms.is_none());
    assert!(pre.snapshot_ms >= pre.last_byte_ms);
    assert_eq!(pre.pre_stdout, None);
    writer.write_all(&vec![b'x'; MAX_LINE]).await.unwrap();
    writer.write_all(b"\n{\"type\":\"s\",\"seq\":1}\n").await.unwrap();
    assert!(rx.recv().await.unwrap().contains("\"seq\":1"));
    drop(writer);
    assert_eq!(task.await.unwrap(), PipeEnd::Eof);
    let post = boundary.lock().unwrap().snapshot();
    assert_eq!(post.discarded, 1);
    assert_eq!(post.forwarded, 1);
    assert_eq!(post.pre_stdout, Some(PipeEnd::Eof));
    assert!(!serde_json::to_string(&post).unwrap().contains("secret-canary"));

    struct Broken;
    impl tokio::io::AsyncRead for Broken {
        fn poll_read(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>, _: &mut tokio::io::ReadBuf<'_>) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Err(std::io::Error::other("secret-canary")))
        }
    }
    let boundary = Arc::new(Mutex::new(ReadBoundary::new()));
    let (tx, _) = mpsc::channel(1);
    assert_eq!(read_lines_from(Broken, tx, boundary.clone()).await, PipeEnd::ReadError);
    let d = boundary.lock().unwrap().snapshot();
    assert_eq!(d.pre_stdout, Some(PipeEnd::ReadError));
    assert_eq!(d.bytes, 0);
    assert!(!serde_json::to_string(&d).unwrap().contains("secret-canary"));

    // Keep the existing final unterminated-frame behavior, but do not call it a newline.
    let boundary = Arc::new(Mutex::new(ReadBoundary::new()));
    let (tx, mut rx) = mpsc::channel(1);
    assert_eq!(read_lines_from(&b"{\"type\":\"s\",\"seq\":1}"[..], tx, boundary.clone()).await, PipeEnd::Eof);
    assert!(matches!(parse_line(&rx.recv().await.unwrap()), Some(Msg::Sample(_))));
    let partial = boundary.lock().unwrap().snapshot();
    assert_eq!(partial.forwarded, 1);
    assert_eq!(partial.last_line_ms, None);
    assert_eq!(partial.pre_stdout, Some(PipeEnd::Eof));
}

#[tokio::test]
async fn missing_cleanup_report_retains_unknown_diagnostics() {
    let (tx, rx) = mpsc::channel(1);
    drop(tx);
    let (stop, _stopped) = oneshot::channel();
    let (done, report) = oneshot::channel();
    drop(done);
    let ended = Source::new(rx, stop, report).close().await;
    assert_eq!(ended.wait_observed, None);
    assert_eq!(ended.code, None);
    assert_eq!(ended.wait_origin, None);
    assert_eq!(ended.kill_attempted, None);
    assert_eq!(ended.io, None);
    assert_eq!(end_category(false, 1, &ended), Some(Category::WaitUnknown));
    assert_eq!(end_category(true, 1, &ended), Some(Category::StallTimeout));
    let ended =
        Ended { code: Some(0), wait_observed: Some(true), io: Some(StreamBoundary { overflow: true, ..StreamBoundary::default() }), ..Ended::default() };
    assert_eq!(end_category(false, 1, &ended), None);
    assert!(ended.io.as_ref().unwrap().overflow);
    // Read errors remain explicit; diagnostics do not rewrite the pre-existing exit category.
    let ended = Ended { code: Some(0), wait_observed: Some(true), stdout: PipeEnd::ReadError, ..Ended::default() };
    assert_eq!(end_category(false, 1, &ended), None);
    assert_eq!(ended.stdout, PipeEnd::ReadError);
    assert!(ended.io.is_none());
}

#[test]
fn read_boundary_saturation_is_explicit_unknown() {
    let mut b = ReadBoundary::new();
    b.add(u32::MAX as usize);
    b.add(1);
    assert_eq!(b.snapshot().bytes, u32::MAX);
    assert!(b.snapshot().overflow);
}
