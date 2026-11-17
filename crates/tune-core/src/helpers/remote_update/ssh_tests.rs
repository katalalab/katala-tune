use super::{
    protocol::{Image, Parser, SamplerHash},
    *,
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::process::Command;
static SERIAL: AtomicU64 = AtomicU64::new(0);
const GLUE: &[u8] = b"inert-owned-transport-fixture\n";
const CONTEXT: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const OLD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const MANIFEST: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const SCRIPT: &str = r#"
const fs=require('node:fs'),path=require('node:path'),cp=require('node:child_process');
const [root,specified,mode]=process.argv.slice(1);let response=specified;
fs.appendFileSync(path.join(root,'calls'),'1');
const release=()=>fs.existsSync(path.join(root,'release'));
const hold=()=>{const t=setInterval(()=>{if(release()){clearInterval(t);process.exit(0)}},10);setTimeout(()=>process.exit(9),15000)};
if(mode==='partial-input') {
  process.stdin.destroy();process.stdout.write(response,()=>process.exit(0));
} else {
  const chunks=[]; process.stdin.on('data',b=>chunks.push(b));
  process.stdin.on('end',()=>{
    const capture=Buffer.concat(chunks);
    fs.writeFileSync(path.join(root,'capture'),capture);
    if(specified==='derive') {
      const nl=capture.indexOf(10),len=Number(capture.subarray(0,nl).toString().split(' ')[1]);
      const h=Object.fromEntries(capture.subarray(nl+1,nl+1+len).toString().trimEnd().split('\n').map(s=>s.split('=')));
      const prefix='KTU1 '+h.token+' '+h.context+' ',old='b'.repeat(64);
      response=prefix+'Started '+h.sha256+' - 0 0\n'+prefix+'Staged '+h.sha256+' - 0 0\n'+prefix+'BackedUp '+h.sha256+' '+old+' 1 0\n'+prefix+'Placed '+h.sha256+' '+old+' 1 0\n';
    }
    if(mode==='stderr-flood')process.stderr.write(Buffer.alloc(128*1024,120));
    if(mode==='stdout-flood')process.stdout.write(Buffer.alloc(128*1024,120));
    const finish=()=>{
    fs.writeFileSync(path.join(root,'responded'),'yes');
    if(mode.startsWith('held'))hold();
    else if(mode==='descendant') {
      // libuv's Windows non-detached child job ends with its Node parent.
      // Only this fixture descendant must outlive that parent; no product work.
      const child=cp.spawn(process.execPath,['-e',`
const fs=require('node:fs'),path=require('node:path');const root=process.argv[1];
setInterval(()=>{if(fs.existsSync(path.join(root,'release')))process.exit(0)},10);
setTimeout(()=>process.exit(9),15000);
try {
  const out=process.stdout,err=process.stderr;
  const ready={stdout:out.fd===1&&out.writable&&!out.destroyed,stderr:err.fd===2&&err.writable&&!err.destroyed};
  if(!ready.stdout||!ready.stderr)throw new Error('stdio-not-ready');
  fs.writeFileSync(path.join(root,'descendant-ready'),JSON.stringify(ready));
} catch(_) {
  fs.writeFileSync(path.join(root,'descendant-error'),JSON.stringify({cause:'stdio-not-ready'}));process.exit(8);
}
`,root],{detached:process.platform==='win32',windowsHide:true,stdio:['ignore',1,2]});
      child.once('spawn',()=>fs.writeFileSync(path.join(root,'descendant-spawn'),JSON.stringify({spawned:true})));
      child.once('error',()=>{
        fs.writeFileSync(path.join(root,'descendant-error'),JSON.stringify({cause:'spawn'}));process.exit(8);
      });
      const ready=setInterval(()=>{
        if(fs.existsSync(path.join(root,'descendant-error'))){clearInterval(ready);process.exit(8)}
        if(fs.existsSync(path.join(root,'descendant-ready'))){clearInterval(ready);process.exit(0)}
      },10);
      setTimeout(()=>process.exit(9),15000);
    } else process.exitCode=mode==='nonzero'?7:0;
    };
    if(!mode.includes('lost'))process.stdout.write(response,finish);else finish();
  });
}
"#;
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("owned-update-{}-{}", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn command_at(root: &std::path::Path, response: &str, mode: &str) -> Command {
        let mut command = crate::collect::command("node");
        command.args(["-e", SCRIPT, root.to_str().unwrap(), response, mode]);
        command.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(false);
        command
    }
    fn command(&self, response: &str, mode: &str) -> Command {
        Self::command_at(&self.0, response, mode)
    }
    fn transport(&self, response: String, mode: &'static str) -> OwnedSshUpdateTransport {
        let root = self.0.clone();
        super::ssh::transport(move |_| Ok(Self::command_at(&root, &response, mode)), SamplerHash::parse(CONTEXT).unwrap())
    }
    fn release(&self) {
        std::fs::write(self.0.join("release"), b"release").unwrap();
    }
    fn cleanup(&self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
    fn calls(&self) -> usize {
        std::fs::read(self.0.join("calls")).unwrap().len()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // On assertion failure fixture-only children leave naturally; no signals.
        if self.0.exists() {
            let _ = std::fs::write(self.0.join("release"), b"release");
        }
    }
}
fn node() -> Node {
    Node::from_value(&serde_json::json!({"id":"synthetic-target","alias":"synthetic-host","os":"macos"}), "synthetic-host")
}
fn image<'a>(bytes: &'a [u8], hash: &'a str) -> Image<'a> {
    Image {
        token: AttemptToken::fresh(1),
        version: crate::helpers::VERSION,
        target: "synthetic-inert",
        manifest: MANIFEST,
        hash,
        context: SamplerHash::parse(CONTEXT).unwrap(),
        glue: GLUE,
        bytes,
    }
}
fn frames(image: &Image<'_>) -> String {
    let prefix = format!("KTU1 {} {}", image.token.as_str(), CONTEXT);
    format!(
        "{prefix} Started {} - 0 0\n{prefix} Staged {} - 0 0\n{prefix} BackedUp {} {OLD} 1 0\n{prefix} Placed {} {OLD} 1 0\n",
        image.hash, image.hash, image.hash, image.hash
    )
}
fn accepted(reply: &RemoteUpdateReply, hash: &str) -> bool {
    reply.local_settled && reply.response_complete && reply.code == Some(0) && reply.hash.as_deref() == Some(hash)
}

// Removing actual wait or EOF from exchange must fail both held ownership tests.
#[tokio::test]
async fn owned_exchange_marker_cannot_complete_while_real_root_is_held() {
    let fixture = Fixture::new();
    let hash = crate::db::sha256_hex(b"inert");
    let image = image(b"inert", &hash);
    let mut child = fixture.command(&frames(&image), "held").spawn().unwrap();
    let observed = std::sync::atomic::AtomicBool::new(false);
    let mut future = Box::pin(super::ssh::exchange_child(&mut child, &image, || {
        observed.store(true, Ordering::SeqCst);
    }));
    let ready = async {
        while !fixture.0.join("responded").is_file() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    let early = tokio::select! {
        reply = &mut future => Some(reply),
        _ = ready => tokio::time::timeout(Duration::from_millis(100), &mut future).await.ok(),
    };
    let premature = early.is_some();
    let waited_early = observed.load(Ordering::SeqCst);
    fixture.release();
    let reply = match early {
        Some(reply) => reply,
        None => (&mut future).await,
    };
    drop(future);
    assert!(child.wait().await.unwrap().success(), "retained actual Child wait is exit proof");
    assert!(!premature && !waited_early, "matching marker returned before actual root wait/pipe settlement");
    assert!(accepted(&reply, &hash));
    assert!(observed.load(Ordering::SeqCst));
    fixture.cleanup();
}
#[tokio::test]
async fn owned_exchange_actual_root_exit_still_waits_for_descendant_stdout_eof() {
    let fixture = Fixture::new();
    let hash = crate::db::sha256_hex(b"inert");
    let image = image(b"inert", &hash);
    let mut child = fixture.command(&frames(&image), "descendant").spawn().unwrap();
    let waited = tokio::sync::Notify::new();
    let mut future = Box::pin(super::ssh::exchange_child(&mut child, &image, || waited.notify_one()));
    // Collect precondition/early completion as values. No assertion may drop
    // this owned exchange before explicit release, actual wait and pipe EOF.
    let early = tokio::select! {
        _ = waited.notified() => tokio::time::timeout(Duration::from_millis(100), &mut future).await.ok(),
        reply = &mut future => Some(reply),
    };
    let premature = early.is_some();
    let read_fact = |name| std::fs::read(fixture.0.join(name)).ok().and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let spawned = read_fact("descendant-spawn");
    let ready = read_fact("descendant-ready");
    let error = read_fact("descendant-error");
    fixture.release();
    let reply = match early {
        Some(reply) => reply,
        None => (&mut future).await,
    };
    drop(future);
    let status = child.wait().await;
    assert!(status.is_ok_and(|status| status.success()), "retained actual root wait must succeed");
    assert_eq!(spawned, Some(serde_json::json!({"spawned":true})), "descendant must actually spawn");
    assert_eq!(ready, Some(serde_json::json!({"stdout":true,"stderr":true})), "descendant must hold both inherited writable stdio handles");
    assert_eq!(error, None, "descendant spawn/stdio failure is not a valid held-pipe fixture");
    assert!(!premature, "actual root exit is insufficient while stdout/stderr remain held");
    assert!(accepted(&reply, &hash));
    fixture.cleanup();
}
#[tokio::test]
async fn owned_exchange_malformed_response_cannot_be_success() {
    let fixture = Fixture::new();
    let hash = crate::db::sha256_hex(b"inert");
    let image = image(b"inert", &hash);
    let reply = super::ssh::execute(&fixture.transport("malformed\n".into(), "good"), &node(), &image).await;
    assert!(!reply.response_complete, "malformed stdout was accepted as placement success");
    assert!(reply.local_settled);
    assert_eq!(fixture.calls(), 1);
    fixture.cleanup();
}
#[tokio::test]
async fn owned_exchange_failures_drain_and_never_retry() {
    for mode in ["stderr-flood", "stdout-flood", "nonzero", "lost", "partial-input"] {
        let fixture = Fixture::new();
        let bytes = vec![42; 2 * 1024 * 1024];
        let hash = crate::db::sha256_hex(&bytes);
        let image = image(&bytes, &hash);
        let reply = super::ssh::execute(&fixture.transport(frames(&image), mode), &node(), &image).await;
        assert!(!accepted(&reply, &hash), "{mode}");
        assert!(reply.local_settled);
        assert_eq!(fixture.calls(), 1);
        fixture.cleanup();
    }
}
// A writer must tolerate partial AsyncWrite writes and propagate partial failure.
#[tokio::test]
async fn owned_exchange_sends_finite_image_once_and_normal_eof() {
    let fixture = Fixture::new();
    let bytes = vec![67; 150_003];
    let hash = crate::db::sha256_hex(&bytes);
    let image = image(&bytes, &hash);
    let reply = super::ssh::execute(&fixture.transport(frames(&image), "good"), &node(), &image).await;
    assert!(accepted(&reply, &hash));
    assert_eq!(fixture.calls(), 1);
    let capture = std::fs::read(fixture.0.join("capture")).unwrap();
    let nl = capture.iter().position(|b| *b == b'\n').unwrap();
    let sizes: Vec<_> = std::str::from_utf8(&capture[..nl]).unwrap().split(' ').collect();
    assert_eq!(sizes[0], "KTU1");
    assert_eq!(sizes.len(), 4);
    let header_len: usize = sizes[1].parse().unwrap();
    let glue_len: usize = sizes[2].parse().unwrap();
    let body_len: usize = sizes[3].parse().unwrap();
    assert_eq!(body_len, 150_003);
    assert_eq!(glue_len, GLUE.len());
    assert_eq!(&capture[nl + 1 + header_len..nl + 1 + header_len + glue_len], GLUE);
    assert_eq!(&capture[nl + 1 + header_len + glue_len..], bytes);
    let header = std::str::from_utf8(&capture[nl + 1..nl + 1 + header_len]).unwrap();
    assert!(header.contains(&format!("token={}\nversion={}\n", image.token.as_str(), crate::helpers::VERSION)));
    assert!(header.contains(&format!("manifest={MANIFEST}\nsize=150003\nsha256={hash}\ncontext={CONTEXT}\n")));
    let facts = reply.facts.unwrap();
    assert_eq!(facts.token, image.token);
    assert_eq!(facts.old_hash.unwrap().hex(), OLD);
    assert!(facts.retained_backup);
    fixture.cleanup();
}
#[test]
fn owned_protocol_rejects_wrong_token_hash_order_partial_duplicate_trailing() {
    let hash = crate::db::sha256_hex(b"inert");
    let image = image(b"inert", &hash);
    let valid = frames(&image);
    let lines: Vec<_> = valid.split_inclusive('\n').collect();
    let cases = [
        valid.replace(image.token.as_str(), "wrong"),
        valid.replace(CONTEXT, MANIFEST),
        valid.replace(&hash, OLD),
        format!("{}{}{}{}", lines[0], lines[2], lines[1], lines[3]),
        valid[..valid.len() - 1].into(),
        format!("{}{}{}{}{}", lines[0], lines[1], lines[1], lines[2], lines[3]),
        format!("{valid}x"),
        format!("{valid}\n"),
        format!("{}{}", lines[0], "x".repeat(4097)),
    ];
    for response in cases {
        let mut parser = Parser::new(&image);
        for part in response.as_bytes().chunks(3) {
            parser.feed(part);
        }
        assert!(!parser.confirmed());
    }
    let mut parser = Parser::new(&image);
    for b in valid.bytes() {
        parser.feed(&[b]);
    }
    assert!(parser.confirmed());
    let unchanged = format!("{}KTU1 {} {CONTEXT} Unchanged {hash} {hash} 0 0\n", lines[0], image.token.as_str());
    let mut parser = Parser::new(&image);
    parser.feed(unchanged.as_bytes());
    assert!(parser.confirmed());
    assert_eq!(parser.facts.unwrap().disposition, PlacementDisposition::Unchanged);
    for terminal in [
        format!("{}KTU1 {} {CONTEXT} Failed {hash} - 0 0\n", lines[0], image.token.as_str()),
        format!("{}{}{}KTU1 {} {CONTEXT} Failed {hash} {OLD} 0 1\n", lines[0], lines[1], lines[2], image.token.as_str()),
    ] {
        let mut parser = Parser::new(&image);
        parser.feed(terminal.as_bytes());
        assert!(!parser.confirmed());
        assert_eq!(parser.facts.unwrap().disposition, PlacementDisposition::Failed);
    }
}
#[tokio::test]
async fn owned_exchange_invalid_images_and_aliases_refuse_before_spawn() {
    let fixture = Fixture::new();
    let hash = crate::db::sha256_hex(b"inert");
    let mut image = image(b"inert", &hash);
    let transport = fixture.transport(frames(&image), "good");
    for alias in ["-oProxyCommand=x", "bad alias", "a\nother", "a@host", "a/host", ""] {
        let node = Node::from_value(&serde_json::json!({"id":"synthetic","alias":alias}), alias);
        let reply = super::ssh::execute(&transport, &node, &image).await;
        assert!(!accepted(&reply, &hash));
    }
    image.glue = &[0; 128 * 1024 + 1];
    assert!(!super::ssh::execute(&transport, &node(), &image).await.response_complete);
    image.glue = GLUE;
    image.version = "0.0.0";
    assert!(!super::ssh::execute(&transport, &node(), &image).await.response_complete);
    assert!(!fixture.0.join("calls").exists());
    fixture.cleanup();
}
#[test]
fn owned_ssh_arguments_keep_strict_options_and_node_shell_wrapping() {
    for shell in ["bash", "powershell", "cmd"] {
        let node = Node::from_value(&serde_json::json!({"id":"synthetic","alias":"synthetic-host","os":"windows","ssh_shell":shell}), "synthetic-host");
        let command = super::ssh::command(&node, "exec fixed-launcher 'literal'").unwrap();
        let args: Vec<_> = command.as_std().get_args().map(|a| a.to_str().unwrap().to_owned()).collect();
        assert_eq!(
            &args[..14],
            [
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectionAttempts=1",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ControlMaster=no",
                "-o",
                "ControlPath=none",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "ServerAliveInterval=0"
            ]
        );
        assert_eq!(args[14], "synthetic-host");
        assert_eq!(args[15], crate::collect::remote_command(&node, "exec fixed-launcher 'literal'"));
        assert_eq!(args.len(), 16);
    }
    let bad = Node::from_value(&serde_json::json!({"id":"synthetic","alias":"-bad"}), "synthetic-host");
    assert!(super::ssh::command(&bad, "fixed").is_err());
}

#[tokio::test]
async fn owned_writer_handles_short_writes_and_partial_failure() {
    use std::{
        io,
        pin::Pin,
        task::{Context, Poll},
    };
    struct ShortWriter {
        bytes: Vec<u8>,
        fail_at: usize,
        closed: bool,
        max_offer: usize,
    }
    impl tokio::io::AsyncWrite for ShortWriter {
        fn poll_write(mut self: Pin<&mut Self>, _: &mut Context<'_>, bytes: &[u8]) -> Poll<io::Result<usize>> {
            self.max_offer = self.max_offer.max(bytes.len());
            if self.bytes.len() >= self.fail_at {
                return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
            }
            let n = 7.min(bytes.len()).min(self.fail_at - self.bytes.len());
            self.bytes.extend_from_slice(&bytes[..n]);
            Poll::Ready(Ok(n))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.closed = true;
            Poll::Ready(Ok(()))
        }
    }
    let bytes = vec![42; 150_003];
    let hash = crate::db::sha256_hex(&bytes);
    let image = image(&bytes, &hash);
    let (prefix, header) = image.header().unwrap();
    for fail_at in [usize::MAX, 1000] {
        let mut writer = ShortWriter { bytes: Vec::new(), fail_at, closed: false, max_offer: 0 };
        let result = super::ssh::write_image(&mut writer, &image, &prefix, &header).await;
        assert!(writer.max_offer <= 64 * 1024);
        if fail_at == usize::MAX {
            assert!(result.is_ok() && writer.closed);
            assert_eq!(&writer.bytes[writer.bytes.len() - 150_003..], bytes);
            assert_eq!(writer.bytes.len(), prefix.len() + header.len() + GLUE.len() + 150_003);
        } else {
            assert!(result.is_err());
            assert_eq!(writer.bytes.len(), 1000);
            assert!(!writer.closed);
        }
    }
}

// Actual controlled direct-await path; all ledger/data/source files are owned
// synthetic fixtures. No product ProcessRoute, default namespace or actual SSH.
#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
#[tokio::test]
async fn owned_controlled_pause_quit_wait_real_child_and_prepared_image_is_immutable() {
    use crate::{
        live::{Ended, Opts, Owner, Route, Source},
        monitor::{RunPhase, RunStatus},
    };
    struct InertRoute;
    impl Route for InertRoute {
        fn open(&self, _: &Node, _: &Opts) -> Result<Source, String> {
            let (lines, read) = tokio::sync::mpsc::channel(1);
            let (stop, stopped) = tokio::sync::oneshot::channel();
            let (done, finished) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let _ = lines.send(r#"{"type":"hello","impl":"rust","agent":"0.0.1"}"#.into()).await;
                let _ = stopped.await;
                let _ = done.send(Ended::default());
            });
            Ok(Source::new(read, stop, finished))
        }
    }
    for (mode, fixture_mode) in [(RunMode::Paused, "held"), (RunMode::Exit, "held"), (RunMode::Exit, "held-lost")] {
        let fixture = Fixture::new();
        let os = if cfg!(windows) { "windows" } else { "macos" };
        let ledger = fixture.0.join("nodes.json");
        std::fs::write(&ledger, serde_json::json!({"nodes":[{"id":"a","alias":"synthetic-host","os":os}]}).to_string()).unwrap();
        let engine = Engine::open(ledger, fixture.0.join("data"), Arc::new(crate::engine::NoHost)).unwrap();
        engine.with_db(|d| d.set_meta("monitor", &serde_json::json!({"enabled":true}))).unwrap();
        let live = Live::new(Arc::new(InertRoute), Opts { max_retries: 0, ..Opts::default() }, Box::new(|_| {}));
        live.start_owned(Owner::Ui, &engine.config().nodes, &[]);
        while !super::super::agent_outdated(&engine.config().nodes[0], live.brief("a").as_ref()) {
            tokio::task::yield_now().await;
        }
        let target = RemoteTargetSnapshot::checked(&engine, "a", super::tests::architecture()).unwrap();
        let prepared = super::tests::prepared(&fixture.0, crate::helpers::VERSION);
        let expected = prepared.remote_bytes().to_vec();
        let request = RemoteUpdateRequest::new(1, 0, Instant::now() + Duration::from_secs(120), target, prepared).unwrap();
        let token = request.token;
        let transport = Arc::new(fixture.transport("derive".into(), fixture_mode));
        let updates = Arc::new(RemoteUpdateControl::new(transport));
        let (tx, _rx) = watch::channel(RunRequest { mode: RunMode::Run, pull_seq: 0, start_seq: 0 });
        let (status, _status_rx) = watch::channel(RunStatus { phase: RunPhase::Starting, policy: None, local_collecting: None, hub_configured: false });
        updates.admit(&engine, &live, &tx, request).unwrap();
        std::fs::write(fixture.0.join("synthetic-source").join(crate::helpers::exe("tune-agent")), b"replaced-source").unwrap();
        let mut worker = tokio::spawn(monitor::run_controlled(engine.clone(), live.clone(), |_| {}, tx.clone(), status, Some(updates.clone())));
        while !fixture.0.join("responded").is_file() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tx.send_modify(|request| request.mode = mode);
        let early = tokio::time::timeout(Duration::from_millis(3100), &mut worker).await.ok();
        let premature = early.is_some();
        let busy = updates.busy();
        let before = updates.local_completion();
        fixture.release();
        // On Pause the worker acknowledges after update and then exits explicitly.
        if mode == RunMode::Paused {
            tx.send_modify(|request| request.mode = RunMode::Exit);
        }
        let exit = match early {
            Some(exit) => exit.unwrap(),
            None => worker.await.unwrap(),
        };
        assert!(!premature && busy && before.is_none(), "accepted update must precede Pause/Quit's later stages");
        assert_eq!(exit.update_complete, Some(true));
        assert_eq!(fixture.calls(), 1);
        let capture = std::fs::read(fixture.0.join("capture")).unwrap();
        assert!(capture.ends_with(&expected));
        if fixture_mode == "held-lost" {
            assert!(matches!(exit.remote_update, Some(RemoteUpdateOutcome::Unknown { .. })));
            assert!(matches!(updates.available(), Err(AdmissionError::Closed)));
        } else {
            assert_eq!(exit.remote_update, Some(RemoteUpdateOutcome::ConfirmedPlacement { attempt_id: 1 }));
            assert_eq!(updates.placement_facts().unwrap().token, token);
            assert!(updates.placement_facts().unwrap().retained_backup);
        }
        // The real owned executor returns only after its actual wait and pipes;
        // journal ACK is checked separately, never inferred from placement.
        live.shutdown(Duration::from_secs(1)).await;
        assert!(engine.finish_monitor_journal(Duration::from_secs(1)).await);
        drop((updates, live, engine));
        fixture.cleanup();
    }
}

#[tokio::test]
async fn owned_exchange_terminal_failure_retains_bounded_rollback_fact() {
    let fixture = Fixture::new();
    let hash = crate::db::sha256_hex(b"inert");
    let image = image(b"inert", &hash);
    let valid = frames(&image);
    let prefix = valid.rsplit_once("KTU1").unwrap().0;
    let response = format!("{prefix}KTU1 {} {CONTEXT} Failed {hash} {OLD} 0 1\n", image.token.as_str());
    let reply = super::ssh::execute(&fixture.transport(response, "good"), &node(), &image).await;
    assert!(reply.response_complete, "valid terminal failure receipt was lost");
    assert!(!accepted(&reply, &hash));
    assert!(reply.local_settled);
    assert!(reply.facts.unwrap().rollback_observed);
    assert!(!reply.facts.unwrap().retained_backup);
    fixture.cleanup();
}
