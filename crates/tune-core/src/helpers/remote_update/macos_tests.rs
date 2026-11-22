use super::*;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod native {
    use super::*;
    use std::{
        os::unix::fs::{MetadataExt, PermissionsExt},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        root: PathBuf,
        home: PathBuf,
        identity: (u64, u64),
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "native-update-fixture-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            let home = root.join("home");
            std::fs::create_dir_all(home.join(".katala-tune/bin")).unwrap();
            let meta = std::fs::symlink_metadata(&root).unwrap();
            Self { root, home, identity: (meta.dev(), meta.ino()) }
        }
        fn wrapper(&self, extra: &str) -> String {
            let root = serde_json::to_string(self.root.to_str().unwrap()).unwrap();
            let home = serde_json::to_string(self.home.to_str().unwrap()).unwrap();
            let (dev, ino) = self.identity;
            format!(
                "def run_fixture(stream, header, body):\n    root=os.fsencode({root})\n    s=os.stat(root,follow_symlinks=False)\n    assert (s.st_dev,s.st_ino)==({dev},{ino})\n    account={{'uid':os.getuid(),'home':os.fsencode({home})}}\n    class FixtureOps(Syscalls):\n        def point(self,name,chain):\n            if name=='opened':\n                held=os.fstat(chain.fds[len(home_components(root))])\n                assert (held.st_dev,held.st_ino)==({dev},{ino})\n    ops=FixtureOps()\n{extra}\n    ops.account=lambda:account\n    run_at(stream,header,body,account,ops)\n",
                dev = dev,
                ino = ino
            )
        }
        fn transport(&self, extra: &str) -> OwnedSshUpdateTransport {
            let bootstrap = macos::fixture_bootstrap(&self.wrapper(extra));
            ssh::native_factory(move |_| {
                let mut cmd = crate::collect::command("python3");
                cmd.args(["-I", "-c", &bootstrap]);
                Ok(cmd)
            })
        }
        fn cleanup(self) {
            let meta = std::fs::symlink_metadata(&self.root).unwrap();
            assert_eq!((meta.dev(), meta.ino()), self.identity, "cleanup requires the original exclusively owned root");
            std::fs::remove_dir_all(self.root).unwrap();
        }
    }
    fn image<'a>(body: &'a [u8], hash: &'a str) -> protocol::Image<'a> {
        protocol::Image {
            token: AttemptToken::fresh(1),
            version: crate::helpers::VERSION,
            target: "aarch64-apple-darwin",
            manifest: "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            hash,
            context: SamplerHash::parse(&"c".repeat(64)).unwrap(),
            glue: macos::GLUE,
            bytes: body,
            mode: protocol::ExchangeMode::Place,
            source_size: body.len(),
            source_hash: hash,
            ttl_ms: 120000,
            observation: "-",
            before: BeforeImageFact::Missing,
        }
    }
    fn node() -> Node {
        Node::from_value(&serde_json::json!({"id":"synthetic-target","alias":"synthetic-host","os":"macos"}), "synthetic-host")
    }
    async fn inspect(f: &Fixture) -> ssh::ExchangeResult {
        let hash = crate::db::sha256_hex(b"inert-source");
        let empty = crate::db::sha256_hex(b"");
        let mut image = image(b"inert-source", &hash);
        image.mode = protocol::ExchangeMode::Inspect;
        image.bytes = b"";
        image.hash = &empty;
        f.transport("").execute_image(&node(), &image).await
    }
    #[tokio::test]
    async fn same_function_inspect_missing_is_read_only_and_settled() {
        let f = Fixture::new();
        let result = inspect(&f).await;
        let entries: Vec<_> = std::fs::read_dir(f.home.join(".katala-tune/bin")).unwrap().collect();
        f.cleanup();
        assert!(result.reply.local_settled, "all child/stdin/stdout/stderr/wait must settle");
        assert!(entries.is_empty(), "inspect cannot create locks or stage files");
        assert_eq!(result.reply.code, Some(0));
        assert_eq!(result.observed.unwrap().before, BeforeImageFact::Missing);
    }
    async fn place(f: &Fixture, snapshot: &observation::MacSnapshot, body: &[u8], extra: &str) -> ssh::ExchangeResult {
        let hash = crate::db::sha256_hex(body);
        let mut image = image(body, &hash);
        image.observation = &snapshot.record;
        image.before = snapshot.before;
        f.transport(extra).execute_image(&node(), &image).await
    }
    #[tokio::test]
    async fn same_function_places_missing_present_and_unchanged_exact_bytes() {
        for mode in ["Missing", "Present", "Unchanged"] {
            let f = Fixture::new();
            let target = f.home.join(".katala-tune/bin/tune-agent");
            let body = super::super::super::binary::fixtures::macho();
            let mut old = body.clone();
            if mode == "Present" {
                old.extend_from_slice(b"old-before-image");
            }
            if mode != "Missing" {
                std::fs::write(&target, &old).unwrap();
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let before = std::fs::metadata(&target).ok().map(|s| (s.dev(), s.ino()));
            let snapshot = inspect(&f).await.observed.expect("native inspect must complete");
            let result = place(&f, &snapshot, &body, "").await;
            let actual = std::fs::read(&target).ok();
            let meta = std::fs::metadata(&target).ok();
            let entries: Vec<_> = std::fs::read_dir(f.home.join(".katala-tune/bin")).unwrap().map(|e| e.unwrap().path()).collect();
            let backups: Vec<_> = entries.iter().filter(|p| p.file_name().unwrap().to_str().unwrap().starts_with(".tune-agent-backup-")).collect();
            let backup = backups.first().map(|p| (std::fs::read(p).unwrap(), std::fs::metadata(p).map(|s| (s.dev(), s.ino())).unwrap()));
            f.cleanup();
            assert_eq!(result.reply.code, Some(0), "{mode}");
            assert!(result.reply.response_complete && result.reply.hash.is_some() && result.reply.local_settled, "{mode}");
            assert_eq!(actual.as_deref(), Some(body.as_slice()), "{mode}");
            assert_eq!(meta.unwrap().mode() & 0o7777, 0o755);
            if mode == "Present" {
                assert_eq!(backup, Some((old, before.unwrap())));
                assert_eq!(entries.len(), 2);
            } else {
                assert!(backup.is_none());
                assert_eq!(entries.len(), 1);
            }
        }
    }

    fn write_executable(path: &std::path::Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn names(f: &Fixture) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(f.home.join(".katala-tune/bin")).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        names
    }
    const FAULT_ADAPTER: &str = r#"    class Fault(FixtureOps):
        def __init__(self):
            super().__init__();self.writes=0;self.restored=False;self.fired=False;self.final_reads=None
        def write(self,fd,data):
            if fault=='partialwrite':
                self.writes+=1
                if self.writes>1: raise OSError(errno.EIO,'fixture')
                return os.write(fd,data[:7])
            return super().write(fd,data)
        def fsync(self,fd):
            if fault=='fsync': raise OSError(errno.EIO,'fixture')
            super().fsync(fd)
        def close(self,fd,label):
            os.close(fd)
            if label=='read' and self.final_reads is not None: self.final_reads+=1
            if fault=='backupreadclose' and self.final_reads==2:
                self.final_reads=None;raise OSError(errno.EINTR,'backup read close consumed')
            if not self.fired and (fault==label+'close' or (fault=='rollbackclose' and self.restored and label=='parent')):
                self.fired=True;raise OSError(errno.EINTR,'fixture close consumed')
        def point(self,name,chain):
            super().point(name,chain)
            if name=='after_rollback': self.restored=True
            if name=='final_verify': self.final_reads=0
            if name=='before_rollback' and fault=='rollbackcollision':
                fd=os.open(TARGET,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o755,dir_fd=chain.bin)
                os.write(fd,b'unowned-sentinel');os.close(fd)
            if name=='after_final_rename' and fault=='postfinalthirdparty':
                os.rename(TARGET,b'owned-displaced-new',src_dir_fd=chain.bin,dst_dir_fd=chain.bin)
                fd=os.open(TARGET,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o755,dir_fd=chain.bin)
                os.write(fd,b'unowned-sentinel');os.close(fd)
                raise Refused()
            if name=='after_backup' and fault=='thirdparty':
                fd=os.open(TARGET,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o755,dir_fd=chain.bin)
                os.write(fd,b'unowned-sentinel');os.close(fd)
                raise Refused()
            if (name=='after_backup' and fault in ('afterbackup','rollbackcollision','rollbackclose','cleanup')) or (name=='after_final_rename' and fault=='afterfinalrename') or (name=='final_verify' and fault=='finalverify') or (name=='cleanup' and fault=='cleanup'):
                raise Refused()
    ops=Fault()"#;
    #[tokio::test]
    async fn same_function_faults_preserve_exact_residuals_and_close_once() {
        for fault in [
            "partialwrite",
            "fsync",
            "stageclose",
            "afterbackup",
            "afterfinalrename",
            "finalverify",
            "thirdparty",
            "postfinalthirdparty",
            "rollbackcollision",
            "rollbackclose",
            "cleanup",
            "parentclose",
            "backupreadclose",
            "lockclose",
            "binclose",
        ] {
            let f = Fixture::new();
            let bin = f.home.join(".katala-tune/bin");
            let target = bin.join("tune-agent");
            let body = super::super::super::binary::fixtures::macho();
            let mut old = body.clone();
            old.extend_from_slice(b"exact-old");
            write_executable(&target, &old);
            let old_inode = std::fs::metadata(&target).unwrap().ino();
            let snapshot = inspect(&f).await.observed.unwrap();
            let adapter = format!("    fault={}\n{FAULT_ADAPTER}", serde_json::to_string(fault).unwrap());
            let result = place(&f, &snapshot, &body, &adapter).await;
            let entries = names(&f);
            let target_bytes = std::fs::read(&target).unwrap();
            let target_ino = std::fs::metadata(&target).unwrap().ino();
            let backups: Vec<_> = entries.iter().filter(|n| n.starts_with(".tune-agent-backup-")).collect();
            let stages: Vec<_> = entries.iter().filter(|n| n.starts_with(".tune-agent-stage-")).collect();
            for backup in &backups {
                assert_eq!(std::fs::read(bin.join(backup)).unwrap(), old, "{fault}");
                assert_eq!(std::fs::metadata(bin.join(backup)).unwrap().ino(), old_inode, "{fault}");
            }
            let lock = entries.iter().any(|n| n == ".tune-agent-update.lock");
            let facts = result.reply.facts;
            let displaced = std::fs::read(bin.join("owned-displaced-new")).ok();
            f.cleanup();
            assert!(result.reply.local_settled && result.reply.hash.is_none() && result.reply.response_complete, "{fault}");
            assert_eq!(facts.unwrap().disposition, PlacementDisposition::Failed, "{fault}");
            assert_ne!(result.reply.code, Some(0), "{fault}");
            match fault {
                "partialwrite" | "fsync" => {
                    assert_eq!(target_bytes, old, "{fault}");
                    assert_eq!(target_ino, old_inode);
                    assert!(!lock && backups.is_empty() && stages.is_empty(), "{fault}");
                }
                "stageclose" => {
                    assert_eq!(target_bytes, old);
                    assert_eq!(target_ino, old_inode);
                    assert!(lock && backups.is_empty() && stages.len() == 1);
                }
                "afterbackup" | "afterfinalrename" | "finalverify" => {
                    assert_eq!(target_bytes, old);
                    assert_eq!(target_ino, old_inode);
                    assert!(!lock && backups.is_empty() && stages.is_empty());
                    assert!(facts.unwrap().rollback_observed, "{fault}");
                }
                "thirdparty" | "postfinalthirdparty" | "rollbackcollision" => {
                    assert_eq!(target_bytes, b"unowned-sentinel");
                    assert!(lock && backups.len() == 1 && stages.len() == usize::from(fault != "postfinalthirdparty"));
                    if fault == "postfinalthirdparty" {
                        assert_eq!(displaced, Some(body));
                    }
                    assert!(facts.unwrap().retained_backup);
                }
                "rollbackclose" => {
                    assert_eq!(target_bytes, old);
                    assert_eq!(target_ino, old_inode);
                    assert!(lock && backups.is_empty() && stages.is_empty());
                    assert!(!facts.unwrap().rollback_observed);
                }
                "cleanup" => {
                    assert_eq!(target_bytes, old);
                    assert!(lock && backups.is_empty() && stages.len() == 1);
                    assert!(!facts.unwrap().rollback_observed);
                }
                "parentclose" | "backupreadclose" | "lockclose" | "binclose" => {
                    assert_eq!(target_bytes, body);
                    assert!(backups.len() == 1 && stages.is_empty());
                    assert_eq!(lock, fault != "binclose");
                    if fault == "backupreadclose" {
                        assert!(!facts.unwrap().retained_backup);
                    }
                    assert!(!facts.unwrap().rollback_observed);
                }
                _ => unreachable!(),
            }
        }
    }
    #[tokio::test]
    async fn same_function_missing_failure_restores_absence() {
        let f = Fixture::new();
        let body = super::super::super::binary::fixtures::macho();
        let snapshot = inspect(&f).await.observed.unwrap();
        let adapter = format!("    fault='afterfinalrename'\n{FAULT_ADAPTER}");
        let result = place(&f, &snapshot, &body, &adapter).await;
        let entries = names(&f);
        f.cleanup();
        assert!(entries.is_empty());
        assert!(result.reply.local_settled && result.reply.hash.is_none());
        assert!(result.reply.facts.unwrap().rollback_observed);
    }
    #[tokio::test]
    async fn same_function_unowned_lock_stage_backup_collisions_keep_sentinels() {
        for collision in ["lock", "stage", "backup"] {
            let f = Fixture::new();
            let bin = f.home.join(".katala-tune/bin");
            let target = bin.join("tune-agent");
            let old = b"safe-executable-old";
            write_executable(&target, old);
            let before = std::fs::metadata(&target).unwrap().ino();
            let snapshot = inspect(&f).await.observed.unwrap();
            let body = super::super::super::binary::fixtures::macho();
            let hash = crate::db::sha256_hex(&body);
            let mut image = image(&body, &hash);
            image.before = snapshot.before;
            image.observation = &snapshot.record;
            let name = match collision {
                "lock" => ".tune-agent-update.lock".into(),
                "stage" => format!(".tune-agent-stage-{}", image.token.as_str()),
                _ => format!(".tune-agent-backup-{}", image.token.as_str()),
            };
            let sentinel = bin.join(&name);
            if collision == "lock" {
                std::fs::create_dir(&sentinel).unwrap();
                std::fs::write(sentinel.join("sentinel"), b"unowned").unwrap();
            } else {
                std::fs::write(&sentinel, b"unowned").unwrap();
            }
            let identity = std::fs::symlink_metadata(&sentinel).unwrap().ino();
            let result = f.transport("").execute_image(&node(), &image).await;
            assert_eq!(std::fs::symlink_metadata(&sentinel).unwrap().ino(), identity, "{collision}");
            assert_eq!(std::fs::read(if collision == "lock" { sentinel.join("sentinel") } else { sentinel }).unwrap(), b"unowned");
            assert_eq!(std::fs::read(&target).unwrap(), old);
            assert_eq!(std::fs::metadata(target).unwrap().ino(), before);
            let entries = names(&f);
            f.cleanup();
            assert!(result.reply.local_settled && result.reply.hash.is_none());
            assert_eq!(entries.len(), 2, "{collision}");
        }
    }

    #[tokio::test]
    async fn same_function_rejects_leaf_parent_and_before_image_drift() {
        use std::os::unix::fs::symlink;
        for drift in ["leafsymlink", "hardlink", "samehashinode", "hash", "mode", "mtime", "ctime", "parentsymlink", "heldparent"] {
            let f = Fixture::new();
            let bin = f.home.join(".katala-tune/bin");
            let target = bin.join("tune-agent");
            let old = b"safe-before-image";
            write_executable(&target, old);
            let snapshot = inspect(&f).await.observed.unwrap();
            let mut extra = String::new();
            match drift {
                "leafsymlink" => {
                    std::fs::rename(&target, bin.join("saved-old")).unwrap();
                    symlink("saved-old", &target).unwrap();
                }
                "hardlink" => std::fs::hard_link(&target, bin.join("unowned-link")).unwrap(),
                "samehashinode" => {
                    write_executable(&bin.join("replacement"), old);
                    std::fs::rename(bin.join("replacement"), &target).unwrap();
                }
                "hash" => write_executable(&target, b"changed-sampler"),
                "mode" => std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap(),
                "mtime" => {
                    use std::fs::FileTimes;
                    std::fs::File::options()
                        .write(true)
                        .open(&target)
                        .unwrap()
                        .set_times(FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)))
                        .unwrap();
                }
                "ctime" => {
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
                }
                "parentsymlink" => {
                    std::fs::rename(&bin, f.home.join(".katala-tune/saved-bin")).unwrap();
                    symlink("saved-bin", &bin).unwrap();
                }
                "heldparent" => {
                    extra = r#"    class Replaced(FixtureOps):
        def point(self,name,chain):
            super().point(name,chain)
            if name=='opened':
                parent=chain.fds[-2]
                os.rename(b'bin',b'saved-bin',src_dir_fd=parent,dst_dir_fd=parent)
                os.mkdir(b'bin',0o755,dir_fd=parent)
    ops=Replaced()"#
                        .into()
                }
                _ => unreachable!(),
            }
            let body = super::super::super::binary::fixtures::macho();
            let result = place(&f, &snapshot, &body, &extra).await;
            let inspect_result = if matches!(drift, "leafsymlink" | "hardlink" | "parentsymlink") { Some(inspect(&f).await) } else { None };
            let old_target = if matches!(drift, "parentsymlink" | "heldparent") { f.home.join(".katala-tune/saved-bin/tune-agent") } else { target.clone() };
            assert!(std::fs::symlink_metadata(&old_target).is_ok(), "{drift}");
            if drift == "heldparent" {
                assert!(names(&f).is_empty());
                assert_eq!(std::fs::read(&old_target).unwrap(), old);
            }
            for parent in [bin.clone(), f.home.join(".katala-tune/saved-bin")] {
                if let Ok(entries) = std::fs::read_dir(parent) {
                    for e in entries {
                        assert!(!e.unwrap().file_name().to_string_lossy().starts_with(".tune-agent-"), "{drift}");
                    }
                }
            }
            f.cleanup();
            assert!(result.reply.local_settled && result.reply.hash.is_none(), "{drift}");
            if let Some(r) = inspect_result {
                assert!(r.observed.is_none() && r.reply.local_settled);
            }
        }
    }
    #[tokio::test]
    async fn native_preflight_binds_raw_node_source_token_and_original_deadline_without_factory() {
        let f = Fixture::new();
        let prepared = super::super::tests::prepared(&f.root, crate::helpers::VERSION);
        let mut request = RemoteUpdateRequest::new(
            9,
            4,
            Instant::now() + Duration::from_secs(120),
            RemoteTargetSnapshot { node: node(), architecture: RemoteArchitecture::Arm64 },
            prepared,
        )
        .unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counted = calls.clone();
        let transport = ssh::native_factory(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            Err(std::io::ErrorKind::Other.into())
        });
        assert_eq!(transport.preflight(&request.target.node, request.payload()), Err(Refusal::InvalidRequest));
        assert_eq!(transport.place(&request.target.node, request.payload()).await.before_start_refusal, Some(Refusal::InvalidRequest));
        let snapshot = inspect(&f).await.observed.unwrap();
        let observed = RemoteDestinationObservation::native(&request, observation::inspect_context(&request, 120000), snapshot);
        request = request.with_observation(observed).unwrap();
        assert!(transport.preflight(&request.target.node, request.payload()).is_ok());
        let original = request.target.node.clone();
        for field in ["id", "alias", "os", "local", "shared", "raw"] {
            request.target.node = original.clone();
            match field {
                "id" => request.target.node.id.push('x'),
                "alias" => request.target.node.alias.push('x'),
                "os" => request.target.node.os = "windows".into(),
                "local" => request.target.node.local = true,
                "shared" => request.target.node.shared = true,
                _ => request.target.node.raw["extra"] = serde_json::json!("changed"),
            }
            assert!(transport.preflight(&request.target.node, request.payload()).is_err(), "{field}");
            assert!(transport.place(&request.target.node, request.payload()).await.before_start_refusal.is_some(), "{field}");
        }
        request.target.node = original;
        let token = request.token;
        request.token = AttemptToken::fresh(9);
        assert_eq!(transport.preflight(&request.target.node, request.payload()), Err(Refusal::InvalidRequest));
        request.token = token;
        request.start_seq += 1;
        assert_eq!(transport.preflight(&request.target.node, request.payload()), Err(Refusal::InvalidRequest));
        request.start_seq -= 1;
        request.attempt_id += 1;
        assert_eq!(transport.preflight(&request.target.node, request.payload()), Err(Refusal::InvalidRequest));
        request.attempt_id -= 1;
        let manifest_path = f.root.join("synthetic-source/manifest.json");
        let manifest = std::fs::read(&manifest_path).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
        value["revision"] = serde_json::json!("e".repeat(40));
        std::fs::write(manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();
        request.prepared = super::super::super::native_install::prepare_native_sampler(&f.root.join("synthetic-source")).unwrap();
        assert_eq!(transport.preflight(&request.target.node, request.payload()), Err(Refusal::InvalidRequest));
        request.expires = Instant::now();
        assert_eq!(transport.preflight(&request.target.node, request.payload()), Err(Refusal::Expired));
        assert_eq!(transport.place(&request.target.node, request.payload()).await.before_start_refusal, Some(Refusal::Expired));
        assert_eq!(calls.load(Ordering::SeqCst), 0, "no refused request can reach command factory/spawn");
        f.cleanup();
    }
    #[tokio::test]
    async fn fixed_bootstrap_rejects_glue_length_and_hash_before_namespace_io() {
        let f = Fixture::new();
        let hash = crate::db::sha256_hex(b"source");
        let mut image = image(b"source", &hash);
        for glue in [b"wrong-length".as_slice(), Box::leak(vec![b'x'; macos::GLUE.len()].into_boxed_slice())] {
            image.glue = glue;
            let result = f.transport("").execute_image(&node(), &image).await;
            assert!(result.reply.local_settled && result.observed.is_none() && result.reply.hash.is_none());
            assert_eq!(result.reply.code, Some(2));
        }
        assert!(names(&f).is_empty());
        f.cleanup();
    }

    #[tokio::test]
    async fn same_function_present_inspect_and_pure_account_validation_do_not_mutate() {
        let f = Fixture::new();
        let target = f.home.join(".katala-tune/bin/tune-agent");
        write_executable(&target, b"safe-executable-old");
        let before = std::fs::metadata(&target).unwrap();
        let extra = r#"    good=validate_account(501,501,b'/fixture-home',b'/fixture-home','darwin','arm64')
    assert good=={'uid':501,'home':b'/fixture-home'}
    assert home_components(good['home'])+[b'.katala-tune',b'bin']==[b'fixture-home',b'.katala-tune',b'bin']
    cases=[(501,502,b'/fixture-home',b'/fixture-home','darwin','arm64'),(501,501,b'/fixture-home',b'/other','darwin','arm64'),(501,501,b'/fixture-home',None,'darwin','arm64'),(501,501,b'/fixture-home',b'/fixture-home','linux','arm64'),(501,501,b'/fixture-home',b'/fixture-home','darwin','x86_64'),(501,501,b'/',b'/','darwin','arm64')]
    for args in cases:
        try: validate_account(*args)
        except Refused: pass
        else: raise AssertionError('unsafe account accepted')"#;
        let hash = crate::db::sha256_hex(b"source");
        let empty = crate::db::sha256_hex(b"");
        let mut image = image(b"source", &hash);
        image.mode = protocol::ExchangeMode::Inspect;
        image.bytes = b"";
        image.hash = &empty;
        let result = f.transport(extra).execute_image(&node(), &image).await;
        let after = std::fs::metadata(&target).unwrap();
        assert_eq!(
            (before.dev(), before.ino(), before.mode(), before.len(), before.mtime(), before.mtime_nsec(), before.ctime(), before.ctime_nsec()),
            (after.dev(), after.ino(), after.mode(), after.len(), after.mtime(), after.mtime_nsec(), after.ctime(), after.ctime_nsec())
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"safe-executable-old");
        assert_eq!(names(&f), ["tune-agent"]);
        f.cleanup();
        assert!(result.reply.local_settled && result.reply.response_complete && result.reply.code == Some(0));
        assert!(matches!(result.observed.unwrap().before, BeforeImageFact::Present(_)));
    }

    #[tokio::test]
    async fn same_buffered_stream_rejects_truncated_excess_corrupt_body_and_bad_prefix_header() {
        for case in ["short", "excess", "hash", "prefix-overflow", "prefix-leadingzero", "header-order", "header-extra", "inspect-body"] {
            let f = Fixture::new();
            let snapshot = inspect(&f).await.observed.unwrap();
            let hash = crate::db::sha256_hex(b"abc");
            let mut image = image(b"abc", &hash);
            image.observation = &snapshot.record;
            let (mut prefix, mut header) = image.header().unwrap();
            match case {
                "short" | "excess" => {
                    let n = if case == "short" { 5 } else { 2 };
                    header = header.replace("size=3\n", &format!("size={n}\n"));
                    prefix = format!("KTU2 {} {} {n}\n", header.len(), macos::GLUE.len());
                }
                "hash" => header = header.replace(&hash, &crate::db::sha256_hex(b"xyz")),
                "prefix-overflow" => prefix = "KTU2 18446744073709551616 1 0\n".into(),
                "prefix-leadingzero" => prefix = prefix.replacen("KTU2 ", "KTU2 0", 1),
                "header-order" => header = header.replace("mode=place\nsource-size=3", "source-size=3\nmode=place"),
                "header-extra" => {
                    header.push_str("extra=1\n");
                    prefix = format!("KTU2 {} {} 3\n", header.len(), macos::GLUE.len());
                }
                "inspect-body" => {
                    header = header.replace("mode=place", "mode=inspect");
                    prefix = format!("KTU2 {} {} 3\n", header.len(), macos::GLUE.len());
                }
                _ => unreachable!(),
            }
            let result = ssh::wire_execute(&f.transport(""), &node(), &image, &prefix, &header).await;
            assert!(result.reply.local_settled && result.reply.hash.is_none(), "{case}");
            assert_ne!(result.reply.code, Some(0), "{case}");
            assert!(names(&f).is_empty(), "{case}");
            f.cleanup();
        }
    }
    #[tokio::test]
    async fn final_backup_replacement_is_unknown_and_never_removed_or_adopted() {
        let f = Fixture::new();
        let bin = f.home.join(".katala-tune/bin");
        let target = bin.join("tune-agent");
        write_executable(&target, b"exact-old");
        let snapshot = inspect(&f).await.observed.unwrap();
        let extra = r#"    class Changed(FixtureOps):
        def point(self,name,chain):
            super().point(name,chain)
            if name=='final_verify':
                backup=[n for n in os.listdir(chain.bin) if n.startswith('.tune-agent-backup-')][0]
                os.rename(backup,b'owned-saved-old',src_dir_fd=chain.bin,dst_dir_fd=chain.bin)
                fd=os.open(backup,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o755,dir_fd=chain.bin)
                os.write(fd,b'unowned-backup');os.close(fd)
    ops=Changed()"#;
        let body = super::super::super::binary::fixtures::macho();
        let result = place(&f, &snapshot, &body, extra).await;
        let entries = names(&f);
        let backup = entries.iter().find(|n| n.starts_with(".tune-agent-backup-")).unwrap();
        assert_eq!(std::fs::read(bin.join(backup)).unwrap(), b"unowned-backup");
        assert_eq!(std::fs::read(bin.join("owned-saved-old")).unwrap(), b"exact-old");
        assert_eq!(std::fs::read(target).unwrap(), body);
        assert!(bin.join(".tune-agent-update.lock").is_dir());
        f.cleanup();
        assert!(result.reply.local_settled && result.reply.response_complete && result.reply.hash.is_none());
        let facts = result.reply.facts.unwrap();
        assert!(!facts.retained_backup && !facts.rollback_observed);
    }

    #[tokio::test]
    async fn control_distinguishes_private_pre_spawn_refusal_from_factory_error_unknown() {
        for bound in [false, true] {
            let f = Fixture::new();
            let prepared = super::super::tests::prepared(&f.root, crate::helpers::VERSION);
            let mut request = RemoteUpdateRequest::new(
                1,
                0,
                Instant::now() + Duration::from_secs(120),
                RemoteTargetSnapshot { node: node(), architecture: RemoteArchitecture::Arm64 },
                prepared,
            )
            .unwrap();
            if bound {
                let snapshot = inspect(&f).await.observed.unwrap();
                let o = RemoteDestinationObservation::native(&request, observation::inspect_context(&request, 120000), snapshot);
                request = request.with_observation(o).unwrap();
            }
            let calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let observed = calls.clone();
            let transport = ssh::native_factory(move |_| {
                observed.fetch_add(1, Ordering::SeqCst);
                Err(std::io::ErrorKind::NotFound.into())
            });
            let control = RemoteUpdateControl::new(Arc::new(transport));
            control.state.lock().unwrap().slot = Slot::Claimed(1);
            control.execute(request).await;
            assert_eq!(control.local_completion(), Some(true));
            if bound {
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                assert!(matches!(control.outcome(), Some(RemoteUpdateOutcome::Unknown { .. })));
                assert_eq!(control.available(), Err(AdmissionError::Closed));
            } else {
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                assert_eq!(control.outcome(), Some(RemoteUpdateOutcome::Refused { attempt_id: 1, reason: Refusal::InvalidRequest }));
                assert_eq!(control.available(), Ok(()));
            }
            f.cleanup();
        }
    }
    #[tokio::test]
    async fn observation_ownership_rejects_rebinding_changed_token_and_remote_expiry() {
        for case in ["rebind", "token", "remote-expiry"] {
            let f = Fixture::new();
            let prepared = super::super::tests::prepared(&f.root, crate::helpers::VERSION);
            let mut request = RemoteUpdateRequest::new(
                1,
                0,
                Instant::now() + Duration::from_secs(120),
                RemoteTargetSnapshot { node: node(), architecture: RemoteArchitecture::Arm64 },
                prepared,
            )
            .unwrap();
            let snapshot = inspect(&f).await.observed.unwrap();
            let o = RemoteDestinationObservation::native(&request, observation::inspect_context(&request, 120000), snapshot);
            if case == "token" {
                request.token = AttemptToken::fresh(1);
                assert!(matches!(request.with_observation(o), Err(Refusal::InvalidRequest)));
            } else {
                request = request.with_observation(o).unwrap();
                if case == "rebind" {
                    let snapshot = inspect(&f).await.observed.unwrap();
                    let o = RemoteDestinationObservation::native(&request, observation::inspect_context(&request, 120000), snapshot);
                    assert!(matches!(request.with_observation(o), Err(Refusal::InvalidRequest)));
                } else {
                    let mut snapshot = request.observation.take().unwrap().snapshot;
                    let mut fields: Vec<_> = snapshot.record.split('|').collect();
                    fields[4] = "0";
                    snapshot.record = fields.join("|");
                    let result = place(&f, &snapshot, b"inert-body", "").await;
                    assert!(result.reply.local_settled && result.reply.hash.is_none());
                    assert!(names(&f).is_empty());
                }
            }
            f.cleanup();
        }
    }
    #[tokio::test]
    async fn public_inspect_binding_and_native_place_keep_original_prepared_bytes() {
        let f = Fixture::new();
        let ledger = f.root.join("nodes.json");
        std::fs::write(&ledger, r#"{"nodes":[{"id":"synthetic-target","alias":"synthetic-host","os":"macos"}]}"#).unwrap();
        let engine = Engine::open(ledger, f.root.join("data"), Arc::new(crate::engine::NoHost)).unwrap();
        let target = RemoteTargetSnapshot::checked(&engine, "synthetic-target", Some(RemoteArchitecture::Arm64)).unwrap();
        let prepared = super::super::tests::prepared(&f.root, crate::helpers::VERSION);
        let expected = prepared.remote_bytes().to_vec();
        let request = RemoteUpdateRequest::new(7, 3, Instant::now() + Duration::from_secs(120), target, prepared).unwrap();
        let transport = f.transport("");
        let observed = transport.inspect(&engine, &request).await.unwrap();
        let request = request.with_observation(observed).unwrap();
        assert_eq!(transport.inspect(&engine, &request).await.unwrap_err().reason, InspectFailure::Unsafe);
        std::fs::write(f.root.join("synthetic-source/tune-agent"), b"replaced-pathname-content").unwrap();
        let reply = transport.place(&request.target.node, request.payload()).await;
        let actual = std::fs::read(f.home.join(".katala-tune/bin/tune-agent")).unwrap();
        drop((request, engine));
        f.cleanup();
        assert_eq!(actual, expected);
        assert!(reply.local_settled && reply.response_complete && reply.hash.is_some() && reply.code == Some(0));
    }
    #[tokio::test]
    async fn inspect_checked_close_errors_never_create_observation_or_mutate() {
        for fault in ["readclose", "parentclose", "binclose"] {
            let f = Fixture::new();
            let target = f.home.join(".katala-tune/bin/tune-agent");
            write_executable(&target, b"safe-old");
            let before = std::fs::metadata(&target).unwrap().ino();
            let hash = crate::db::sha256_hex(b"source");
            let empty = crate::db::sha256_hex(b"");
            let mut image = image(b"source", &hash);
            image.mode = protocol::ExchangeMode::Inspect;
            image.bytes = b"";
            image.hash = &empty;
            let adapter = format!("    fault={}\n{FAULT_ADAPTER}", serde_json::to_string(fault).unwrap());
            let result = f.transport(&adapter).execute_image(&node(), &image).await;
            assert_eq!(std::fs::metadata(&target).unwrap().ino(), before);
            assert_eq!(std::fs::read(&target).unwrap(), b"safe-old");
            assert_eq!(names(&f), ["tune-agent"]);
            f.cleanup();
            assert!(result.reply.local_settled && result.reply.response_complete && result.observed.is_none());
            assert_eq!(result.failure, Some(InspectFailure::Io));
            assert_eq!(result.reply.code, Some(1));
        }
    }
    #[tokio::test]
    async fn unchanged_requires_full_hash_and_eof_without_staging_or_repairing() {
        let f = Fixture::new();
        let target = f.home.join(".katala-tune/bin/tune-agent");
        write_executable(&target, b"xyz");
        let before = std::fs::metadata(&target).unwrap().ino();
        let snapshot = inspect(&f).await.observed.unwrap();
        let hash = crate::db::sha256_hex(b"xyz");
        let mut image = image(b"abc", &hash);
        image.observation = &snapshot.record;
        image.before = snapshot.before;
        let result = f.transport("").execute_image(&node(), &image).await;
        assert_eq!(std::fs::read(&target).unwrap(), b"xyz");
        assert_eq!(std::fs::metadata(&target).unwrap().ino(), before);
        assert_eq!(names(&f), ["tune-agent"]);
        f.cleanup();
        assert!(result.reply.local_settled && result.reply.hash.is_none());
        assert_eq!(result.reply.facts.unwrap().disposition, PlacementDisposition::Failed);
    }
    #[tokio::test]
    async fn renamed_metadata_baseline_rejects_later_ctime_only_drift() {
        for which in ["backup", "target"] {
            let f = Fixture::new();
            let bin = f.home.join(".katala-tune/bin");
            let target = bin.join("tune-agent");
            write_executable(&target, b"old-before");
            let old_inode = std::fs::metadata(&target).unwrap().ino();
            let snapshot = inspect(&f).await.observed.unwrap();
            let extra = format!(
                r#"    which={which}
    class Drift(FixtureOps):
        def point(self,name,chain):
            super().point(name,chain)
            if (which=='backup' and name=='after_backup') or (which=='target' and name=='final_verify'):
                entry=TARGET if which=='target' else os.fsencode([n for n in os.listdir(chain.bin) if n.startswith('.tune-agent-backup-')][0])
                os.chmod(entry,0o700,dir_fd=chain.bin,follow_symlinks=False)
                os.chmod(entry,0o755,dir_fd=chain.bin,follow_symlinks=False)
    ops=Drift()"#,
                which = serde_json::to_string(which).unwrap()
            );
            let body = super::super::super::binary::fixtures::macho();
            let result = place(&f, &snapshot, &body, &extra).await;
            let entries = names(&f);
            let actual = std::fs::read(&target).unwrap();
            let inode = std::fs::metadata(&target).unwrap().ino();
            f.cleanup();
            assert!(result.reply.local_settled && result.reply.hash.is_none(), "later {which} ctime drift must not be accepted as rename's ctime change");
            if which == "backup" {
                assert_eq!(actual, body);
                assert!(entries.iter().any(|n| n == ".tune-agent-update.lock"));
                assert!(!result.reply.facts.unwrap().retained_backup);
            } else {
                assert_eq!(actual, b"old-before");
                assert_eq!(inode, old_inode);
                assert_eq!(entries, ["tune-agent"]);
                assert!(result.reply.facts.unwrap().rollback_observed);
            }
        }
    }
}

fn wire_image<'a>(bytes: &'a [u8], hash: &'a str) -> protocol::Image<'a> {
    protocol::Image {
        token: AttemptToken::fresh(1),
        version: crate::helpers::VERSION,
        target: "aarch64-apple-darwin",
        manifest: "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        hash,
        context: SamplerHash::parse(&"c".repeat(64)).unwrap(),
        glue: macos::GLUE,
        bytes,
        mode: protocol::ExchangeMode::Place,
        source_size: bytes.len(),
        source_hash: hash,
        ttl_ms: 119999,
        observation: "-",
        before: BeforeImageFact::Missing,
    }
}
fn record(leaf: &str) -> String {
    format!("501|2f66697874757265|darwin|arm64|18446744073709551615|1,1,0,16877;1,2,501,16877;1,3,501,16877;1,4,501,16877|{leaf}")
}
#[test]
fn observation_record_is_canonical_bounded_and_rejects_overflow() {
    let h = "a".repeat(64);
    let present = record(&format!("P,1,2,501,5,33261,-1,0,1,{h}"));
    assert!(observation::MacSnapshot::parse(&record("M")).is_some());
    assert_eq!(observation::MacSnapshot::parse(&present).unwrap().before, BeforeImageFact::Present(SamplerHash::parse(&h).unwrap()));
    for bad in [
        present.replace("501|", "0501|"),
        present.replace("18446744073709551615", "18446744073709551616"),
        present.replace(",-1,0,", ",-0,0,"),
        present.replace(",-1,0,", ",170141183460469231731687303715884105728,0,"),
        present.replace(",5,33261", ",0,33261"),
        present.replace(",0,1,", ",0,2,"),
        present.replace("2f66697874757265", "2f2e2e"),
        present.replace("2f66697874757265", "2F66697874757265"),
        format!("{present}|extra"),
        present.replace(";1,4,501,16877", ""),
    ] {
        assert!(observation::MacSnapshot::parse(&bad).is_none(), "accepted noncanonical record");
    }
}
#[tokio::test]
async fn inspect_wire_has_twelve_headers_zero_body_and_separate_source_tuple() {
    let hash = crate::db::sha256_hex(b"source");
    let empty = crate::db::sha256_hex(b"");
    let mut image = wire_image(b"source", &hash);
    image.mode = protocol::ExchangeMode::Inspect;
    image.bytes = b"";
    image.hash = &empty;
    let (prefix, header) = image.header().unwrap();
    let sizes: Vec<_> = prefix.trim_end().split(' ').collect();
    assert_eq!(sizes[0], "KTU2");
    assert_eq!(sizes[3], "0");
    assert_eq!(header.lines().count(), 12);
    assert!(header.contains(&format!("size=0\nsha256={empty}\n")));
    assert!(header.contains(&format!("mode=inspect\nsource-size=6\nsource-sha256={hash}\nttl-ms=119999\nobservation=-\n")));
    let mut capture = Vec::new();
    ssh::write_image(&mut capture, &image, &prefix, &header).await.unwrap();
    assert_eq!(capture, [prefix.as_bytes(), header.as_bytes(), macos::GLUE].concat());
    let bootstrap = macos::bootstrap();
    assert!(bootstrap.contains(&format!("g=={}", macos::GLUE.len())));
    assert!(bootstrap.contains(&crate::db::sha256_hex(macos::GLUE)));
}
#[test]
fn inspect_response_requires_one_complete_canonical_frame_and_irreversible_validity() {
    let hash = crate::db::sha256_hex(b"source");
    let empty = crate::db::sha256_hex(b"");
    let mut image = wire_image(b"source", &hash);
    image.mode = protocol::ExchangeMode::Inspect;
    image.bytes = b"";
    image.hash = &empty;
    let line = format!("KTU2 {} {} Observed {hash} {}\n", image.token.as_str(), image.context.hex(), record("M"));
    let mut parser = protocol::Parser::new(&image);
    for chunk in line.as_bytes().chunks(3) {
        parser.feed(chunk);
    }
    assert!(parser.complete() && parser.observed.is_some());
    assert!(!parser.confirmed());
    for invalid in [
        line[..line.len() - 1].into(),
        format!("{line}x"),
        format!("{line}{line}"),
        line.replace("KTU2", "KTU1"),
        line.replace(&hash, &"e".repeat(64)),
        line.replace("18446744073709551615", "18446744073709551616"),
        "x".repeat(20 * 1024),
    ] {
        let mut parser = protocol::Parser::new(&image);
        parser.feed(invalid.as_bytes());
        parser.feed(line.as_bytes());
        assert!(!parser.complete());
    }
    for reason in ["Unsupported", "Account", "Unsafe", "Io", "Expired"] {
        let mut parser = protocol::Parser::new(&image);
        parser.feed(format!("KTU2 {} {} InspectFailed {hash} {reason}\n", image.token.as_str(), image.context.hex()).as_bytes());
        assert!(parser.complete() && parser.observed.is_none() && parser.inspect_failure.is_some());
    }
}
#[test]
fn before_image_is_identical_in_every_frame_and_missing_failures_never_invent_backup() {
    for present in [false, true] {
        let hash = crate::db::sha256_hex(b"source");
        let mut image = wire_image(b"source", &hash);
        let old = "b".repeat(64);
        if present {
            image.before = BeforeImageFact::Present(SamplerHash::parse(&old).unwrap());
        }
        let old = if present { old.as_str() } else { "-" };
        let prefix = format!("KTU2 {} {}", image.token.as_str(), image.context.hex());
        let started = format!("{prefix} Started {hash} {old} 0 0\n");
        let staged = format!("{started}{prefix} Staged {hash} {old} 0 0\n");
        for flags in ["0 0", "0 1", "1 0"] {
            let valid = if present { flags == "0 0" } else { flags != "1 0" };
            let mut parser = protocol::Parser::new(&image);
            parser.feed(format!("{staged}{prefix} Failed {hash} {old} {flags}\n").as_bytes());
            assert_eq!(parser.complete(), valid);
        }
        let mut parser = protocol::Parser::new(&image);
        parser.feed(format!("{started}{prefix} Staged {hash} {} 0 0\n", if present { "-" } else { "a" }).as_bytes());
        assert!(!parser.complete());
        if present {
            for flags in ["0 0", "1 0", "0 1"] {
                let mut parser = protocol::Parser::new(&image);
                parser.feed(format!("{staged}{prefix} BackedUp {hash} {old} 1 0\n{prefix} Failed {hash} {old} {flags}\n").as_bytes());
                assert!(parser.complete() && !parser.confirmed());
            }
        }
    }
}
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[test]
fn unsupported_host_rejects_native_constructor_without_io() {
    assert!(matches!(OwnedSshUpdateTransport::macos(), Err(Refusal::Target)));
}
