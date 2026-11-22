use super::*;
use std::time::Instant;
fn name() -> String {
    Sha256::digest(format!("fixture-{}-{:?}", std::process::id(), Instant::now()).as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}
#[test]
fn protocol_refuses_unbounded_foreign_and_effect_requests() {
    let namespace = name();
    assert_eq!(decode(&encode(&namespace, Some("dashboard")).unwrap(), &namespace).unwrap(), Some("dashboard".into()));
    for bytes in [
        br#"{"version":1,"namespace":"foreign","open":"status"}"#.to_vec(),
        serde_json::to_vec(&serde_json::json!({"version":1,"namespace":namespace,"open":"status","collect":true})).unwrap(),
        serde_json::to_vec(&serde_json::json!({"version":1,"namespace":namespace,"open":7})).unwrap(),
        br#"{"version":1}"#.to_vec(),
    ] {
        assert!(decode(&bytes, &namespace).is_err());
    }
    assert!(encode(&namespace, Some(&"x".repeat(257))).is_err());
    assert!(encode(&namespace, Some("logs\nquit")).is_err());
    assert!(read_frame(&mut std::io::Cursor::new((MAX_MESSAGE as u32 + 1).to_le_bytes())).is_err());
}
#[test]
fn real_local_ipc_forwards_and_rejects_review_without_new_owner() {
    let namespace = name();
    let Claim::Primary(primary) = claim_named(namespace.clone(), None, false).unwrap() else { panic!() };
    let name = namespace.clone();
    let secondary = std::thread::spawn(move || claim_named(name, Some("status"), false).map(|c| matches!(c, Claim::Forwarded)));
    let request = primary.requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(request.open.as_deref(), Some("status"));
    request.reply.send(Ok(())).unwrap();
    assert!(secondary.join().unwrap().unwrap());
    assert!(claim_named(namespace.clone(), None, true).err().unwrap().contains("--review"));
    drop(primary);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match platform::claim(&namespace).unwrap() {
            platform::Claim::Primary(_) => break,
            platform::Claim::Secondary if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => panic!("owned endpoint was not cleaned up"),
        }
    }
}
#[test]
fn resolved_missing_data_stays_same_after_creation() {
    let dir = std::env::temp_dir().join(format!("preview-resolve-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let before = resolved(&dir.join("missing")).unwrap();
    std::fs::create_dir_all(dir.join("missing")).unwrap();
    assert_eq!(before, resolved(&dir.join("missing")).unwrap());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn actual_endpoint_rejects_foreign_malformed_oversized_and_accepts_fragmented_frames() {
    let namespace = name();
    let Claim::Primary(primary) = claim_named(namespace.clone(), None, false).unwrap() else { panic!() };
    for frame in [
        encode("foreign", Some("status")).unwrap(),
        br#"["#.to_vec(),
        serde_json::to_vec(&serde_json::json!({"namespace":namespace,"version":1,"open":"logs","collect":true})).unwrap(),
    ] {
        let mut stream = platform::connect(&namespace).unwrap();
        write_frame(&mut stream, &frame).unwrap();
        let answer: Result<(), String> = serde_json::from_slice(&read_frame(&mut stream).unwrap()).unwrap();
        stream.write_all(&[1]).unwrap();
        assert!(answer.is_err());
        assert!(primary.requests.try_recv().is_err());
    }
    let mut oversized = platform::connect(&namespace).unwrap();
    oversized.write_all(&2048u32.to_le_bytes()).unwrap();
    let answer: Result<(), String> = serde_json::from_slice(&read_frame(&mut oversized).unwrap()).unwrap();
    oversized.write_all(&[1]).unwrap();
    assert!(answer.unwrap_err().contains("limit"));
    drop(oversized);
    // Consume and ACK the rejection promptly. Deliberately writing/sleeping
    // past the frame deadline could also outlive the bounded reply/ACK window;
    // a closed peer no longer promises a readable structured reply.
    let mut slow = platform::connect(&namespace).unwrap();
    slow.write_all(&200u32.to_le_bytes()).unwrap();
    let answer: Result<(), String> = serde_json::from_slice(&read_frame(&mut slow).unwrap()).unwrap();
    slow.write_all(&[1]).unwrap();
    assert_eq!(answer.unwrap_err(), "incomplete activation frame");
    assert!(primary.requests.try_recv().is_err());
    drop(slow);
    let key = namespace.clone();
    let fragmented = std::thread::spawn(move || {
        let mut stream = platform::connect(&key).unwrap();
        let bytes = encode(&key, Some("logs")).unwrap();
        stream.write_all(&(bytes.len() as u32).to_le_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        stream.write_all(&bytes).unwrap();
        let answer: Result<(), String> = serde_json::from_slice(&read_frame(&mut stream).unwrap()).unwrap();
        stream.write_all(&[1]).unwrap();
        answer.unwrap();
    });
    let request = primary.requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(request.open.as_deref(), Some("logs"));
    request.reply.send(Ok(())).unwrap();
    fragmented.join().unwrap();
    drop(primary);
}

#[test]
fn trickling_frame_uses_one_server_read_deadline() {
    let namespace = name();
    let platform::Claim::Primary(listener) = platform::claim(&namespace).unwrap() else { panic!() };
    let (ready, start) = mpsc::channel();
    let (done, finish) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let mut stream = platform::connect(&namespace).unwrap();
        stream.write_all(&200u32.to_le_bytes()).unwrap();
        start.recv_timeout(Duration::from_secs(2)).unwrap();
        // Intentionally span the 500ms budget with individually timely bytes.
        // Retaining the accepted stream below makes every write observable,
        // independently of the production reply/ACK deadline.
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(150));
            stream.write_all(b" ").unwrap();
        }
        // Do not let EOF masquerade as the server's whole-frame deadline.
        finish.recv_timeout(Duration::from_secs(2)).unwrap();
    });
    let accept_deadline = Instant::now() + Duration::from_secs(2);
    let mut stream = loop {
        if let Some(stream) = listener.accept().unwrap() {
            break stream;
        }
        assert!(Instant::now() < accept_deadline, "owned client did not connect");
        std::thread::sleep(Duration::from_millis(25));
    };
    let began = Instant::now();
    // Start the read budget after accept/coordination, not during client setup.
    stream.read_budget(IO_TIMEOUT);
    ready.send(()).unwrap();
    let result = read_frame(&mut stream);
    let elapsed = began.elapsed();
    // The server owns this measurement; client scheduling after rejection
    // cannot turn a successful timeout into a late response-read error.
    done.send(()).unwrap();
    writer.join().unwrap();
    assert_eq!(result.unwrap_err(), "incomplete activation frame");
    assert!(elapsed >= IO_TIMEOUT.saturating_sub(Duration::from_millis(100)), "frame must wait for its missing bytes");
    assert!(elapsed < Duration::from_secs(1), "frame deadline must not restart for each byte: {elapsed:?}");
}

#[test]
#[ignore = "owned IPC child; only the bounded synthetic harness supplies its directory"]
fn owned_activation_child() {
    let Some(dir) = std::env::var_os("TUNE_IPC_FIXTURE") else { return };
    let dir = PathBuf::from(dir);
    let mode = std::env::var("TUNE_IPC_MODE").unwrap();
    let config = dir.join("nodes.json");
    let data = dir.join("data");
    match claim(&config, &data, Some("status"), false).unwrap() {
        Claim::Forwarded => {
            assert_eq!(mode, "secondary");
        }
        Claim::Primary(_guard) => {
            // This branch can never run for the normal secondary fixture.
            use std::fs::OpenOptions;
            let mut counter = OpenOptions::new().append(true).create(true).open(dir.join("engine-opens")).unwrap();
            writeln!(counter, "open").unwrap();
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let engine = {
                let _enter = runtime.enter();
                tune_core::engine::Engine::open(config, data, Arc::new(tune_core::engine::NoHost)).unwrap()
            };
            assert_eq!(mode, "crash", "secondary must not open an Engine");
            // Deliberate process exit leaves stale socket but the kernel releases
            // our lock/pipe. Only this owned synthetic child is terminated.
            drop(engine);
            std::process::exit(0);
        }
    }
}
fn child(dir: &Path, mode: &str) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "instance::tests::owned_activation_child", "--ignored", "--nocapture"])
        .env("TUNE_IPC_FIXTURE", dir)
        .env("TUNE_IPC_MODE", mode)
        .env("TUNE_IPC_FIXTURE_OWNER", std::process::id().to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap()
}
fn child_finished(mut child: std::process::Child) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let output = child.wait_with_output().unwrap();
            assert!(status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            return;
        }
        assert!(Instant::now() < deadline, "owned activation child exceeded its protocol deadline");
        std::thread::sleep(Duration::from_millis(25));
    }
}
#[test]
fn subprocess_secondary_opens_engine_once_and_stale_owner_recovers() {
    let root = std::env::temp_dir().join(format!("gpui-ipc-{}-{}", std::process::id(), name()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("nodes.json"), r#"{"nodes":[]}"#).unwrap();
    let config = root.join("nodes.json");
    let data = root.join("data");
    let Claim::Primary(primary) = claim(&config, &data, None, false).unwrap() else { panic!() };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    std::fs::write(root.join("engine-opens"), "open\n").unwrap();
    let engine = {
        let _enter = runtime.enter();
        tune_core::engine::Engine::open(config.clone(), data.clone(), Arc::new(tune_core::engine::NoHost)).unwrap()
    };
    let secondary = child(&root, "secondary");
    let request = primary.requests.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(request.open.as_deref(), Some("status"));
    request.reply.send(Ok(())).unwrap();
    child_finished(secondary);
    assert_eq!(std::fs::read_to_string(root.join("engine-opens")).unwrap(), "open\n");
    assert!(engine.with_db(|d| d.get_meta("lastProbeAt")).unwrap().is_none());
    assert_eq!(engine.monitor_journal_health()["running"], false);
    // A different resolved data namespace never forwards into this primary.
    let Claim::Primary(other) = claim(&config, &root.join("other-data"), None, false).unwrap() else { panic!() };
    assert!(primary.requests.try_recv().is_err());
    drop((other, primary, engine, runtime));
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match platform::claim(&namespace(&config, &data).unwrap()).unwrap() {
            platform::Claim::Primary(_) => break,
            _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => panic!("primary did not release its owned endpoint"),
        }
    }
    child_finished(child(&root, "crash"));
    let Claim::Primary(restarted) = claim(&config, &data, None, false).unwrap() else { panic!() };
    drop(restarted);
    // Endpoint lifetime is independent of DB lifetime. All DB/runtime owners
    // have ended before Windows directory cleanup.
    std::fs::remove_dir_all(root).unwrap();
}
