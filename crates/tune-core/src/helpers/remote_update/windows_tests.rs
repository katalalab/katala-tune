use super::*;
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn record(leaf: &str) -> String {
    let profile: Vec<u8> = "C:\\Example\\fixture".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let parent = format!("1,{},16,{}", "01".repeat(16), "a".repeat(64));
    format!(
        "w1|010100000000000501000000|{}|{}|windows|x64|{}|1000|121000|{}|{leaf}",
        hex(&profile),
        hex(b"/c/Example/fixture"),
        "01".repeat(16),
        vec![parent; 5].join(";")
    )
}
#[test]
fn windows_record_accepts_only_canonical_missing_and_present_before_images() {
    assert!(observation::WindowsSnapshot::parse(&record("M")).is_some(), "closed Windows grammar must accept bounded Missing snapshot");
    let hash = "b".repeat(64);
    let present = record(&format!("P,1,{},7,32,-1,0,1,{},{}", "02".repeat(16), "a".repeat(64), hash));
    let parsed = observation::WindowsSnapshot::parse(&present).unwrap();
    assert_eq!(parsed.record, present);
    assert_eq!(parsed.before, BeforeImageFact::Present(SamplerHash::parse(&hash).unwrap()));
    assert!(observation::MacSnapshot::parse(&present).is_none());
}
#[test]
fn windows_record_rejects_encoding_bounds_metadata_overflow_and_cross_platform_tags() {
    let good = record("M");
    let profile = hex(&"C:\\Example\\fixture".encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
    for bad in [
        good.replace("w1|", "w2|"),
        good.replace("|windows|x64|", "|darwin|arm64|"),
        good.replace("|1000|", "|01000|"),
        good.replace("|121000|", "|121001|"),
        good.replace("|121000|", "|1000|"),
        good.replace("|1000|121000|", "|18446744073709551615|18446744073709551616|"),
        good.replace("010100000000000501000000", "010200000000000501000000"),
        good.replace("010100000000000501000000", "020100000000000501000000"),
        good.replace(&"01".repeat(16), &"00".repeat(16)),
        good.replace(&profile, "43003a005c0000d8"),
        good.replace(&profile, &hex("c:\\Example\\fixture".as_bytes())),
        good.replace(&profile, &hex(&"C:\\Example\\..".encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>())),
        good.replace(&hex(b"/c/Example/fixture"), "ff"),
        good.replace(&hex(b"/c/Example/fixture"), "00"),
        format!("{good}|extra"),
        good.replace(",16,", ",4294967296,"),
        good.replace("|M", "|P,1,01,0,32,0,0,1,a,b"),
        good.replace("|M", &format!("|P,1,{},1,32,9223372036854775808,0,1,{},{}", "01".repeat(16), "a".repeat(64), "b".repeat(64))),
    ] {
        assert!(observation::WindowsSnapshot::parse(&bad).is_none(), "accepted invalid Windows record");
    }
    for profile in [
        "C:\\Example\\trailing.",
        "C:\\Example\\trailing ",
        "C:\\Example\\a:b",
        "C:\\Example\\",
        "C:\\Example\\a/b",
        "C:\\Example\\.\\a",
        "\\\\server\\share",
        "C:\\",
    ] {
        let bytes = profile.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>();
        assert!(observation::profile_components(&bytes).is_none(), "{profile}");
    }
    assert!(observation::profile_components(&"C:\\Example\\\u{1f680}".encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>()).is_some());
    assert!(observation::unhex(&"00".repeat(257), 1, 256).is_none());
    assert!(observation::unhex("AA", 1, 1).is_none());
    assert!(observation::WindowsSnapshot::parse(&format!("{}M", "a".repeat(3072))).is_none());
}
const BINARY_STREAM_FIXTURE: &str = r#"public class F{public static int Run(Stream s,byte[] b,long n){var h=Ktu2.Header.Parse(b,(ulong)n);Ktu2.Body(new Ktu2.Chain{Ops=new Ktu2.Ops()},s,h,null);var w=new Ktu2.Wire(Console.OpenStandardOutput(),h);w.Placement("Started",false,false);w.Placement("Failed",false,false);return 2;}}"#;
fn image<'a>(body: &'a [u8], hash: &'a str) -> protocol::Image<'a> {
    protocol::Image {
        token: AttemptToken::fresh(1),
        version: crate::helpers::VERSION,
        target: "x86_64-pc-windows-msvc",
        manifest: "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        hash,
        context: SamplerHash::parse(&"c".repeat(64)).unwrap(),
        glue: windows::GLUE,
        bytes: body,
        mode: protocol::ExchangeMode::Place,
        source_size: body.len(),
        source_hash: hash,
        ttl_ms: 119999,
        observation: "-",
        before: BeforeImageFact::Missing,
    }
}
#[test]
fn windows_bootstrap_uses_independently_fixed_glue_and_all_existing_shell_wrappers_fit() {
    assert_eq!(windows::GLUE.len(), windows::GLUE_LENGTH, "bootstrap length must be an independently fixed product constant");
    assert_eq!(crate::db::sha256_hex(windows::GLUE), windows::GLUE_HASH);
    assert!(windows::GLUE.starts_with(b"\xef\xbb\xbf"));
    assert!(windows::GLUE[3..].is_ascii());
    assert!(!windows::GLUE[3..].windows(3).any(|b| b == b"\xef\xbb\xbf"));
    let bootstrap = windows::bootstrap();
    assert!(bootstrap.is_ascii() && !bootstrap.contains('\''));
    assert!(bootstrap.contains(windows::GLUE_HASH) && bootstrap.contains(&format!("$g-ne{}", windows::GLUE_LENGTH)));
    let launcher = windows::launcher();
    let encoded = launcher.rsplit(' ').next().unwrap();
    // The actual existing renderer, not a mirror of its quoting algorithm.
    for shell in ["bash", "powershell", "cmd"] {
        let n = Node::from_value(&serde_json::json!({"id":"fixture","alias":"synthetic-host","os":"windows","ssh_shell":shell}), "synthetic-host");
        let command = ssh::command(&n, &launcher).unwrap();
        let args: Vec<_> = command.as_std().get_args().collect();
        let remote = args.last().unwrap().to_str().unwrap();
        assert!(remote.is_ascii() && remote.len() <= 7900, "{shell}: {}", remote.len());
        let upper_bound = ssh::local_command_bound(&command).unwrap();
        assert!(upper_bound <= 32767, "local CreateProcess UTF16 bound");
        if shell == "bash" {
            assert!(remote.ends_with(encoded));
        } else {
            assert!(remote.contains("base64 -d"));
        }
        eprintln!("actual renderer {shell}: {} ASCII, local argument upper bound {upper_bound} UTF16", remote.len());
    }
    let n = Node::from_value(&serde_json::json!({"id":"fixture","alias":"synthetic-host","os":"windows"}), "synthetic-host");
    assert!(ssh::command(&n, &"x".repeat(7901)).is_err());
}
#[test]
fn native_binary_fixture_command_also_fits_all_actual_wrappers() {
    let bootstrap = windows::fixture_bootstrap(BINARY_STREAM_FIXTURE, "[F]::Run($s,$hb,$b)");
    let launcher = windows::fixture_launcher(&bootstrap);
    for shell in ["bash", "powershell", "cmd"] {
        let node = Node::from_value(&serde_json::json!({"id":"fixture","alias":"synthetic-host","os":"windows","ssh_shell":shell}), "synthetic-host");
        let wrapped = crate::collect::remote_command(&node, &launcher);
        eprintln!("native binary fixture actual renderer {shell}: {} ASCII", wrapped.len());
        assert!(wrapped.is_ascii() && wrapped.len() <= 7900, "{shell}: {}", wrapped.len());
    }
}

#[test]
fn local_create_process_bound_includes_actual_program_utf16_and_argument_quoting() {
    let mut command = crate::collect::command(&"x".repeat(17000));
    command.arg("fixed");
    assert!(ssh::local_command_bound(&command).unwrap() > 32767, "an oversized actual executable path must be refused before spawn");
    let mut command = crate::collect::command("program");
    command.arg("quote\"\\tail");
    let expected = 2 * ("program".encode_utf16().count() + "quote\"\\tail".encode_utf16().count()) + 6;
    assert_eq!(ssh::local_command_bound(&command), Some(expected));
}

#[tokio::test]
async fn windows_stream_preserves_binary_order_and_parser_closes_target_dispatch() {
    let body = [0, 10, 13, 255, 1, 0, 254];
    let hash = crate::db::sha256_hex(&body);
    let empty = crate::db::sha256_hex(b"");
    let mut img = image(&body, &hash);
    let (prefix, header) = img.header().unwrap();
    let mut stream = Vec::new();
    ssh::write_image(&mut stream, &img, &prefix, &header).await.unwrap();
    assert_eq!(stream, [prefix.as_bytes(), header.as_bytes(), windows::GLUE, &body].concat());
    img.mode = protocol::ExchangeMode::Inspect;
    img.bytes = b"";
    img.hash = &empty;
    let (prefix, header) = img.header().unwrap();
    let mut stream = Vec::new();
    ssh::write_image(&mut stream, &img, &prefix, &header).await.unwrap();
    assert_eq!(stream, [prefix.as_bytes(), header.as_bytes(), windows::GLUE].concat());
    let frame = format!("KTU2 {} {} Observed {hash} {}\n", img.token.as_str(), img.context.hex(), record("M"));
    let mut parser = protocol::Parser::new(&img);
    for chunk in frame.as_bytes().chunks(3) {
        parser.feed(chunk);
    }
    assert!(parser.complete());
    assert!(matches!(parser.observed, Some(observation::DestinationSnapshot::Windows(_))));
    for target in ["aarch64-apple-darwin", "unknown"] {
        img.target = target;
        let mut parser = protocol::Parser::new(&img);
        parser.feed(frame.as_bytes());
        assert!(!parser.complete());
    }
    img.target = "x86_64-pc-windows-msvc";
    for malformed in [frame[..frame.len() - 1].to_string(), format!("{frame}x"), format!("{frame}{frame}"), "x".repeat(protocol::STDOUT_LIMIT + 1)] {
        let mut p = protocol::Parser::new(&img);
        p.feed(malformed.as_bytes());
        p.feed(frame.as_bytes());
        assert!(!p.complete());
    }
}
#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
#[test]
fn unsupported_local_windows_constructor_refuses_without_io() {
    assert!(matches!(OwnedSshUpdateTransport::windows(), Err(Refusal::Target)));
}

#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(target_os = "windows", target_arch = "x86_64")))]
#[tokio::test]
async fn native_windows_preflight_refuses_unbound_unsupported_and_changed_bindings_before_factory() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let base = std::env::current_dir().unwrap();
    #[cfg(windows)]
    require_fixture_base_outside_profile(&base);
    let root = base.join(format!("ktu2-binding-fixture-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let os = if cfg!(windows) { "windows" } else { "macos" };
    let n = Node::from_value(&serde_json::json!({"id":"fixture","alias":"synthetic-host","os":os}), "synthetic-host");
    let mut request = RemoteUpdateRequest::new(
        1,
        0,
        Instant::now() + Duration::from_secs(120),
        RemoteTargetSnapshot { node: n.clone(), architecture: tests::architecture().unwrap() },
        tests::prepared(&root, crate::helpers::VERSION),
    )
    .unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let counter = count.clone();
    let transport = ssh::windows_factory(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        Err(std::io::ErrorKind::PermissionDenied.into())
    });
    let reply = transport.place(&n, request.payload()).await;
    assert!(reply.before_start_refusal.is_some());
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let snapshot = observation::DestinationSnapshot::parse("x86_64-pc-windows-msvc", &record("M")).unwrap();
    let observed = RemoteDestinationObservation::native(&request, observation::inspect_context(&request, 120000), snapshot);
    if cfg!(windows) {
        request = request.with_observation(observed).unwrap();
        let mut changed = n.clone();
        changed.raw["extra"] = serde_json::json!(true);
        assert!(transport.preflight(&changed, request.payload()).is_err());
        request.expires = Instant::now();
        assert_eq!(transport.preflight(&n, request.payload()), Err(Refusal::Expired));
        assert_eq!(count.load(Ordering::SeqCst), 0);
    } else {
        assert!(matches!(request.with_observation(observed), Err(Refusal::Target)));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
fn require_fixture_base_outside_profile(base: &std::path::Path) {
    let profile = std::env::var("USERPROFILE").expect("missing/non-Unicode actual profile boundary: native fixtures unsupported");
    assert!(
        observation::profile_components(&profile.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>()).is_some(),
        "invalid actual profile boundary: native fixtures unsupported"
    );
    let profile = profile.to_lowercase();
    let base = base.to_string_lossy().to_lowercase();
    assert!(base != profile && !base.starts_with(&format!("{profile}\\")), "fixture base must be outside actual profile");
}

// These fixtures are deliberately unavailable on this Mac. Windows CI must
// compile PS5.1/C# and execute them; no cfg(windows) success is inferred here.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
mod native {
    #![allow(unsafe_code)]
    use super::*;
    use std::{
        fs::OpenOptions,
        os::windows::{
            fs::OpenOptionsExt,
            io::{AsRawHandle, IntoRawHandle},
        },
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandleEx(handle: *mut std::ffi::c_void, class: i32, buffer: *mut u8, length: u32) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    fn identity(path: &Path) -> String {
        let file = OpenOptions::new().access_mode(0x80).share_mode(7).custom_flags(0x02000000 | 0x00200000).open(path).unwrap();
        let mut b = [0u8; 24];
        // SAFETY: live read-only no-reparse handle and exact FILE_ID_INFO buffer.
        assert_ne!(unsafe { GetFileInformationByHandleEx(file.as_raw_handle(), 18, b.as_mut_ptr(), 24) }, 0);
        // SAFETY: transfer the owned metadata handle exactly once.
        assert_ne!(unsafe { CloseHandle(file.into_raw_handle()) }, 0);
        format!("{},{}", u64::from_le_bytes(b[..8].try_into().unwrap()), hex(&b[8..]))
    }
    fn node() -> Node {
        Node::from_value(&serde_json::json!({"id":"fixture","alias":"synthetic-host","os":"windows"}), "synthetic-host")
    }
    struct Fixture {
        root: PathBuf,
        home: PathBuf,
        id: String,
        parents: Vec<String>,
        sid: String,
        boot: String,
        serial: u64,
    }
    impl Fixture {
        async fn new() -> Self {
            let base = std::env::current_dir().unwrap();
            let n = SERIAL.fetch_add(1, Ordering::Relaxed);
            let root = base.join(format!("ktu2-fixture-{}-{n}", std::process::id()));
            // Only pure environment/path comparisons, never actual profile IO.
            require_fixture_base_outside_profile(&base);
            assert!(observation::profile_components(&root.to_string_lossy().encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>()).is_some());
            std::fs::create_dir(&root).unwrap();
            let id = identity(&root);
            let mut f = Self { home: root.join("home"), root, id, parents: vec![], sid: String::new(), boot: String::new(), serial: 0 };
            let result = f.inspect("", "").await;
            assert!(result.reply.local_settled, "owned child/stdin/both pipes/actual wait must all finish");
            assert_eq!(result.reply.code, Some(0), "mandatory native compiler/Win32/boot/NTFS fixture unavailable or failed; never skip this gate");
            let snapshot = result.observed.expect("native first inspect");
            let record = snapshot.record();
            let fields: Vec<_> = record.split('|').collect();
            f.parents = fields[9].split(';').map(str::to_owned).collect();
            f.sid = fields[1].into();
            f.boot = fields[6].into();
            assert_eq!(fields[10], "M");
            f
        }
        fn bin(&self) -> PathBuf {
            self.home.join(".katala-tune/bin")
        }
        fn source(&mut self, ops: &str, opened: &str) -> String {
            self.serial += 1;
            let root = serde_json::to_string(self.root.to_str().unwrap()).unwrap();
            let rootid = serde_json::to_string(&self.id).unwrap();
            let sid = serde_json::to_string(&self.sid).unwrap();
            let report = serde_json::to_string(self.root.join(format!("facts-{}", self.serial)).to_str().unwrap()).unwrap();
            let ids =
                self.parents.iter().map(|p| serde_json::to_string(&p.split(',').take(2).collect::<Vec<_>>().join(",")).unwrap()).collect::<Vec<_>>().join(",");
            let sec = self.parents.iter().map(|p| serde_json::to_string(p.rsplit(',').next().unwrap()).unwrap()).collect::<Vec<_>>().join(",");
            format!(
                r#"
public static class Ktu2Fixture {{
    sealed class FixtureOps:Ktu2.Ops {{
        internal string Root={root}; internal int Count; internal bool Armed;
        [DllImport("kernel32.dll")] static extern IntPtr GetStdHandle(int kind);
        internal List<Ktu2.Owned> Extras=new List<Ktu2.Owned>();
        internal override void Hit(string label,Ktu2.Chain chain) {{
            if(label=="opened"){{{opened}}}
            {ops}
        }}
        internal override int WriteLimit(int count) {{ return Math.Min(count,7); }}
    }}
    public static int Run(Stream input,byte[] bytes,long body,long q,long freq) {{
        string root={root};Ktu2.Need(Directory.Exists(root));
        SecurityIdentifier sid;using(WindowsIdentity identity=WindowsIdentity.GetCurrent()){sid=identity.User;}byte[] raw=new byte[sid.BinaryLength];sid.GetBinaryForm(raw,0);
        Ktu2.Manifest manifest=new Ktu2.Manifest();manifest.Root=root;manifest.RootIdentity={rootid};manifest.RootIndex=Ktu2.Components(root).Length;manifest.Sid=raw;manifest.Identities=new string[]{{{ids}}};manifest.Security=new string[]{{{sec}}};
        if(manifest.Identities.Length==0){{
            DirectorySecurity security=new DirectorySecurity();security.SetOwner(sid);security.SetAccessRuleProtection(true,false);
            foreach(SecurityIdentifier trusted in new SecurityIdentifier[]{{sid,new SecurityIdentifier(WellKnownSidType.LocalSystemSid,null),new SecurityIdentifier(WellKnownSidType.BuiltinAdministratorsSid,null)}})
                security.AddAccessRule(new FileSystemAccessRule(trusted,FileSystemRights.FullControl,InheritanceFlags.ContainerInherit|InheritanceFlags.ObjectInherit,PropagationFlags.None,AccessControlType.Allow));
            Directory.SetAccessControl(root,security);Directory.CreateDirectory(root+"\\home\\.katala-tune\\bin");
        }} else Ktu2.Need(Ktu2.Hex(raw)=={sid});
        Ktu2.Account account=new Ktu2.Account();account.Sid=raw;account.Profile=root+"\\home";account.Bash=Encoding.UTF8.GetBytes("/synthetic-fixture");FixtureOps operations=new FixtureOps();
        Stream output=Console.OpenStandardOutput();int result=2;
        try{{result=Ktu2.RunAt(input,bytes,checked((ulong)body),account,operations,manifest,q,freq,output,delegate{{return account;}});}}
        finally{{foreach(Ktu2.Owned h in operations.Extras)if(h.Value!=IntPtr.Zero)try{{h.Close("extra");}}catch{{result=2;}}
            List<string> facts=new List<string>();Ktu2.Ops readOps=new Ktu2.Ops();Ktu2.Chain chain=null;
            try{{chain=Ktu2.OpenChain(account,readOps,manifest);foreach(string path in Directory.GetFileSystemEntries(account.Profile+"\\.katala-tune\\bin")){{string name=Path.GetFileName(path);try{{bool dir=Directory.Exists(path);Ktu2.State state=dir?Ktu2.Named(chain,name,true):Ktu2.ReadLeaf(chain,name);facts.Add(name+"|"+(dir?state.Parent:state.Leaf));}}catch{{facts.Add(name+"|UNVERIFIED");}}}}}}
            finally{{if(chain!=null)chain.CloseAll();if(readOps.Uncertain)result=2;}}
            using(FileStream proof=new FileStream({report},FileMode.CreateNew,FileAccess.Write,FileShare.None)){{byte[] data=Encoding.ASCII.GetBytes(string.Join("\n",facts.ToArray()));proof.Write(data,0,data.Length);proof.Flush();}}
            output.Flush();output.Dispose();}}
        return result;
    }}
}}
"#
            )
        }
        fn transport(&mut self, ops: &str, opened: &str) -> OwnedSshUpdateTransport {
            let code = self.source(ops, opened);
            // The appended fixture shares one Add-Type compilation with the exact
            // fixed bridge; production has no callable fixture/root parameter.
            let bootstrap = windows::fixture_bootstrap(&code, "[Ktu2Fixture]::Run($s,$hb,$b,$q,$f)");
            let encoded = crate::logs::base64(&bootstrap.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
            assert!(encoded.len() + 100 < 32767, "fixture CreateProcess bound");
            ssh::windows_factory(move |_| {
                let mut c = crate::collect::command("powershell.exe");
                c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded]);
                Ok(c)
            })
        }
        async fn inspect(&mut self, ops: &str, opened: &str) -> ssh::ExchangeResult {
            let hash = crate::db::sha256_hex(b"inert-source");
            let empty = crate::db::sha256_hex(b"");
            let mut img = image(b"inert-source", &hash);
            img.mode = protocol::ExchangeMode::Inspect;
            img.bytes = b"";
            img.hash = &empty;
            self.transport(ops, opened).execute_image(&node(), &img).await
        }
        async fn place(&mut self, snapshot: &observation::DestinationSnapshot, body: &[u8], ops: &str) -> ssh::ExchangeResult {
            let hash = crate::db::sha256_hex(body);
            let mut img = image(body, &hash);
            img.observation = snapshot.record();
            img.before = snapshot.before();
            self.transport(ops, "").execute_image(&node(), &img).await
        }
        fn facts(&self) -> String {
            std::fs::read_to_string(self.root.join(format!("facts-{}", self.serial))).unwrap()
        }
        fn cleanup(self) {
            assert_eq!(identity(&self.root), self.id, "only the original exclusively created root may be cleaned after awaited child/pipes");
            std::fs::remove_dir_all(self.root).unwrap();
        }
    }
    #[tokio::test]
    async fn same_functions_inspect_missing_present_and_place_exact_bytes_retained_backup_and_unchanged() {
        for mode in ["missing", "present", "unchanged"] {
            let mut f = Fixture::new().await;
            let mut body = super::super::super::binary::fixtures::pe();
            body.extend_from_slice(&[0, 10, 13, 255]);
            let target = f.bin().join("tune-agent.exe");
            let old = if mode == "unchanged" { body.clone() } else { b"old-before".to_vec() };
            if mode != "missing" {
                std::fs::write(&target, &old).unwrap();
            }
            let old_id = (mode != "missing").then(|| identity(&target));
            let before = std::fs::read(&target).ok();
            let snapshot = f.inspect("", "").await;
            assert_eq!(snapshot.reply.code, Some(0));
            assert_eq!(std::fs::read(&target).ok(), before, "inspect is read only");
            if let Some(id) = &old_id {
                assert_eq!(&identity(&target), id);
            }
            let snapshot = snapshot.observed.unwrap();
            let after = f.inspect("", "").await.observed.unwrap();
            let first: Vec<_> = snapshot.record().split('|').collect();
            let next: Vec<_> = after.record().split('|').collect();
            assert_eq!(&first[..7], &next[..7]);
            assert_eq!(&first[9..], &next[9..], "read-only state/security unchanged; lease is separately refreshed");
            let old_leaf = after.record().rsplit('|').next().unwrap().to_owned();
            let reply = f.place(&after, &body, "").await.reply;
            let actual = std::fs::read(&target).unwrap();
            let facts = f.facts();
            let names = std::fs::read_dir(f.bin()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect::<Vec<_>>();
            let backup = names.iter().find(|n| n.starts_with(".tune-agent-backup-")).map(|n| f.bin().join(n));
            assert!(reply.local_settled && reply.response_complete);
            assert_eq!(reply.code, Some(0));
            assert_eq!(actual, body);
            assert_eq!(reply.hash, Some(crate::db::sha256_hex(&body)));
            assert!(!names.iter().any(|n| n == ".tune-agent-update.lock" || n.starts_with(".tune-agent-stage-")));
            assert!(facts.contains("tune-agent.exe|P,"));
            if mode == "present" {
                let backup = backup.unwrap();
                assert_eq!(std::fs::read(&backup).unwrap(), old);
                assert_eq!(identity(&backup), old_id.as_deref().unwrap());
                assert!(reply.facts.unwrap().retained_backup);
                let backup_record = facts.lines().find(|line| line.starts_with(".tune-agent-backup-")).unwrap().split_once('|').unwrap().1;
                let original: Vec<_> = old_leaf.split(',').collect();
                let retained: Vec<_> = backup_record.split(',').collect();
                assert_eq!(&original[1..6], &retained[1..6], "old identity/size/attrs/LastWriteTime retained");
                assert_eq!(&original[7..], &retained[7..], "old links/security/hash retained");
            } else {
                assert!(backup.is_none());
            }
            if mode == "unchanged" {
                assert_eq!(identity(&target), old_id.as_deref().unwrap());
                assert_eq!(reply.facts.unwrap().disposition, PlacementDisposition::Unchanged);
            }
            f.cleanup();
        }
    }
    #[tokio::test]
    async fn same_functions_fault_matrix_keeps_unknown_occupants_and_exact_residuals() {
        for fault in [
            "write",
            "flush",
            "close:stage",
            "after_backup",
            "after_final_rename",
            "final_verify",
            "occupancy",
            "backup_identity",
            "cleanup",
            "close:parent",
            "close:lock",
            "close:bin",
            "rollback_close",
            "lock_disposition",
            "response_loss",
            "stage_read_close",
            "backup_read_close",
            "stage_delete_pending",
            "backup_time",
            "target_time",
            "rollback_collision",
        ] {
            let mut f = Fixture::new().await;
            let target = f.bin().join("tune-agent.exe");
            std::fs::write(&target, b"old-before").unwrap();
            let old_id = identity(&target);
            let sentinel = f.bin().join("unowned-sentinel");
            std::fs::write(&sentinel, b"sentinel").unwrap();
            let sentinel_id = identity(&sentinel);
            let snapshot = f.inspect("", "").await.observed.unwrap();
            let ops=match fault {
                "occupancy"=>r#"if(label=="after_backup"){File.WriteAllBytes(chain.Account.Profile+"\\.katala-tune\\bin\\tune-agent.exe",Encoding.ASCII.GetBytes("third-party"));throw new Ktu2.Failure("Io");}"#.to_string(),
                "backup_identity"=>r#"if(label=="after_backup"){string bin=chain.Account.Profile+"\\.katala-tune\\bin\\";string p=Directory.GetFiles(bin,".tune-agent-backup-*")[0];File.Move(p,bin+"saved-old");File.WriteAllBytes(p,Encoding.ASCII.GetBytes("third-party"));throw new Ktu2.Failure("Io");}"#.to_string(),
                "cleanup"=>r#"if(label=="flush"||label=="cleanup")throw new Ktu2.Failure("Io");"#.to_string(),
                "stage_read_close"=>r#"if(label=="after_lock")Armed=true;if(Armed&&label=="close:read"&&++Count==1)throw new Ktu2.Failure("Io");"#.to_string(),
                "backup_read_close"=>r#"if(label=="before_backup_verify")Armed=true;if(Armed&&label=="close:read"&&++Count==1)throw new Ktu2.Failure("Io");"#.to_string(),
                "stage_delete_pending"=>r#"if(label=="flush")throw new Ktu2.Failure("Io");if(label=="cleanup"){string name=Path.GetFileName(Directory.GetFiles(chain.Account.Profile+"\\.katala-tune\\bin",".tune-agent-stage-*")[0]);Extras.Add(Ktu2.Open(chain.Bin,name,Ktu2.Read|Ktu2.Control,5,1,false,false,null,this));}"#.to_string(),
                "backup_time"=>r#"if(label=="after_backup"){string path=Directory.GetFiles(chain.Account.Profile+"\\.katala-tune\\bin",".tune-agent-backup-*")[0];FileAttributes original=File.GetAttributes(path);File.SetAttributes(path,original|FileAttributes.ReadOnly);File.SetAttributes(path,original);}"#.to_string(),
                "target_time"=>r#"if(label=="final_verify"){string path=chain.Account.Profile+"\\.katala-tune\\bin\\tune-agent.exe";FileAttributes original=File.GetAttributes(path);File.SetAttributes(path,original|FileAttributes.ReadOnly);File.SetAttributes(path,original);}"#.to_string(),
                "rollback_collision"=>r#"if(label=="after_final_rename")throw new Ktu2.Failure("Io");if(label=="before_rollback")File.WriteAllBytes(chain.Account.Profile+"\\.katala-tune\\bin\\tune-agent.exe",Encoding.ASCII.GetBytes("third-party"));"#.to_string(),
                "response_loss"=>r#"if(label=="final_verify")Ktu2.Need(Ktu2.CloseHandle(GetStdHandle(-11)));"#.to_string(),
                "rollback_close"=>r#"if(label=="after_backup")throw new Ktu2.Failure("Io");if(label=="before_rollback")Armed=true;if(Armed&&label=="close:mutation"&&++Count==1)throw new Ktu2.Failure("Io");"#.to_string(),
                other=>format!("if(label=={} && ++Count==1)throw new Ktu2.Failure(\"Io\");",serde_json::to_string(other).unwrap()),
            };
            let body = super::super::super::binary::fixtures::pe();
            let result = f.place(&snapshot, &body, &ops).await;
            let facts = f.facts();
            let bytes = std::fs::read(&target).ok();
            let names = std::fs::read_dir(f.bin()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect::<Vec<_>>();
            assert!(result.reply.local_settled && result.reply.hash.is_none(), "{fault}");
            assert_eq!(std::fs::read(&sentinel).unwrap(), b"sentinel");
            assert_eq!(identity(&sentinel), sentinel_id, "{fault}");
            match fault {
                "write" | "flush" => {
                    assert_eq!(bytes.unwrap(), b"old-before");
                    assert_eq!(identity(&target), old_id);
                    assert!(!names.iter().any(|n| n.starts_with(".tune-agent-")), "{fault}: {facts}");
                }
                "after_backup" | "after_final_rename" | "final_verify" | "target_time" => {
                    assert_eq!(bytes.unwrap(), b"old-before");
                    assert_eq!(identity(&target), old_id);
                    assert!(result.reply.facts.unwrap().rollback_observed, "{fault}: {facts}");
                }
                "occupancy" | "rollback_collision" => {
                    assert_eq!(bytes.unwrap(), b"third-party");
                    assert!(names.iter().any(|n| n.starts_with(".tune-agent-backup-")));
                    assert!(names.iter().any(|n| n == ".tune-agent-update.lock"));
                }
                "backup_identity" => {
                    assert_eq!(std::fs::read(f.bin().join("saved-old")).unwrap(), b"old-before");
                    assert_eq!(identity(&f.bin().join("saved-old")), old_id);
                    assert!(!result.reply.facts.unwrap().retained_backup);
                    assert!(names.iter().any(|n| n == ".tune-agent-update.lock"));
                }
                "close:stage" | "cleanup" | "stage_read_close" => {
                    assert_eq!(bytes.unwrap(), b"old-before");
                    assert!(names.iter().any(|n| n.starts_with(".tune-agent-stage-")));
                    assert!(names.iter().any(|n| n == ".tune-agent-update.lock"));
                }
                "close:bin" | "close:lock" | "response_loss" => {
                    assert_eq!(bytes.unwrap(), body);
                    assert!(!names.iter().any(|n| n == ".tune-agent-update.lock"));
                    assert!(!result.reply.facts.unwrap().rollback_observed);
                }
                "backup_read_close" | "backup_time" => {
                    assert_eq!(bytes.unwrap(), body);
                    assert!(!result.reply.facts.unwrap().retained_backup);
                    assert!(names.iter().any(|n| n == ".tune-agent-update.lock"));
                }
                "stage_delete_pending" => {
                    assert_eq!(bytes.unwrap(), b"old-before");
                    assert!(!names.iter().any(|n| n.starts_with(".tune-agent-stage-")));
                    assert!(names.iter().any(|n| n == ".tune-agent-update.lock"));
                    assert!(!result.reply.facts.unwrap().rollback_observed);
                }
                "rollback_close" => {
                    assert_eq!(bytes.unwrap(), b"old-before");
                    assert!(!result.reply.facts.unwrap().rollback_observed);
                    assert!(names.iter().any(|n| n == ".tune-agent-update.lock"));
                }
                _ => {
                    assert!(!result.reply.facts.unwrap().rollback_observed);
                    assert!(names.iter().any(|n| n == ".tune-agent-update.lock"));
                }
            }
            // Native evidence includes identity/attrs/security and real residual
            // entries; stdout receipts alone are never the filesystem proof.
            assert!(facts.contains("unowned-sentinel|P,"));
            f.cleanup();
        }
    }
    #[tokio::test]
    async fn same_functions_missing_rollback_and_read_handle_denial_never_invent_old_backup() {
        let mut f = Fixture::new().await;
        let snapshot = f.inspect("", "").await.observed.unwrap();
        let body = super::super::super::binary::fixtures::pe();
        let result = f.place(&snapshot, &body, "if(label==\"final_verify\")throw new Ktu2.Failure(\"Io\");").await;
        assert!(result.reply.local_settled && result.reply.hash.is_none());
        assert!(result.reply.facts.unwrap().rollback_observed);
        assert!(!f.bin().join("tune-agent.exe").exists());
        assert_eq!(std::fs::read_dir(f.bin()).unwrap().count(), 0);
        f.cleanup();
        let mut f = Fixture::new().await;
        let target = f.bin().join("tune-agent.exe");
        std::fs::write(&target, b"old-before").unwrap();
        let id = identity(&target);
        let snapshot = f.inspect("", "").await.observed.unwrap();
        let held = OpenOptions::new().read(true).share_mode(1).open(&target).unwrap();
        let result = f.place(&snapshot, &body, "").await;
        // SAFETY: the extra fixture read handle is transferred and closed once after the owned child/pipes/wait.
        assert_ne!(unsafe { CloseHandle(held.into_raw_handle()) }, 0);
        assert!(result.reply.local_settled && result.reply.hash.is_none());
        assert_eq!(std::fs::read(&target).unwrap(), b"old-before");
        assert_eq!(identity(&target), id);
        f.cleanup();
    }
    #[tokio::test]
    async fn same_functions_reject_hardlinks_same_hash_new_id_attributes_and_unowned_collisions() {
        for case in ["hardlink", "new-id", "readonly", "lock", "stage", "backup"] {
            let mut f = Fixture::new().await;
            let target = f.bin().join("tune-agent.exe");
            std::fs::write(&target, b"old-before").unwrap();
            let snapshot = f.inspect("", "").await.observed.unwrap();
            let original_id = identity(&target);
            let body = super::super::super::binary::fixtures::pe();
            if case == "hardlink" {
                std::fs::hard_link(&target, f.bin().join("unowned-link")).unwrap();
            }
            if case == "new-id" {
                std::fs::rename(&target, f.bin().join("unowned-saved")).unwrap();
                std::fs::write(&target, b"old-before").unwrap();
                assert_ne!(identity(&target), original_id);
            }
            if case == "readonly" {
                let mut p = std::fs::metadata(&target).unwrap().permissions();
                p.set_readonly(true);
                std::fs::set_permissions(&target, p).unwrap();
            }
            let ops=match case {
                "lock"=>r#"if(label=="opened"){string p=chain.Account.Profile+"\\.katala-tune\\bin\\.tune-agent-update.lock";Directory.CreateDirectory(p);File.WriteAllText(p+"\\sentinel","unowned");}"#.to_string(),
                "stage"|"backup"=>format!(r#"if(label=="opened"){{File.WriteAllText(chain.Account.Profile+"\\.katala-tune\\bin\\.tune-agent-{}-"+TOKEN,"unowned");}}"#,case),
                _=>String::new(),
            };
            // Collision names are bound to this exact attempted image token.
            let hash = crate::db::sha256_hex(&body);
            let mut img = image(&body, &hash);
            img.observation = snapshot.record();
            img.before = snapshot.before();
            let ops = ops.replace("TOKEN", &serde_json::to_string(img.token.as_str()).unwrap());
            let result = f.transport(&ops, "").execute_image(&node(), &img).await;
            assert!(result.reply.local_settled && result.reply.hash.is_none(), "{case}");
            assert_eq!(std::fs::read(&target).unwrap(), b"old-before");
            if case == "readonly" {
                let mut p = std::fs::metadata(&target).unwrap().permissions();
                p.set_readonly(false);
                std::fs::set_permissions(&target, p).unwrap();
            }
            if case == "lock" {
                assert_eq!(std::fs::read(f.bin().join(".tune-agent-update.lock/sentinel")).unwrap(), b"unowned");
            }
            if case == "stage" || case == "backup" {
                assert_eq!(std::fs::read(f.bin().join(format!(".tune-agent-{case}-{}", img.token.as_str()))).unwrap(), b"unowned");
            }
            f.cleanup();
        }
    }
    #[tokio::test]
    async fn same_bridge_boot_abi_agrees_across_processes_and_account_acl_inputs_are_bounded() {
        let source = r#"
public class F {
    static void Reject(Action f) {
        bool rejected=false;
        try { f(); } catch(Ktu2.Failure) { rejected=true; }
        Ktu2.Need(rejected);
    }
    static byte[] Sd(string s) {
        RawSecurityDescriptor d=new RawSecurityDescriptor(s);
        byte[] b=new byte[d.BinaryLength];
        d.GetBinaryForm(b,0);
        return b;
    }
    public static int Run(Stream s,byte[] b,long n,long q,long frequency) {
        var h=Ktu2.Header.Parse(b,(ulong)n);
        Ktu2.Need(s.ReadByte()==-1);
        string boot=Ktu2.Boot();
        var lease=Ktu2.Lease.Start(h.Ttl,q,frequency);
        lease.Check();
        Ktu2.Need(lease.BootId==boot);
        byte[] sid=Ktu2.KnownSid(21,1,2,3,1001);
        string sidText=new SecurityIdentifier(sid,0).Value;
        string profile="C:\\Example\\fixture";
        string bash=Ktu2.Hex(Encoding.UTF8.GetBytes("/c/Example/fixture"));
        string native=Ktu2.Hex(Encoding.UTF8.GetBytes(profile));
        Ktu2.ValidateAccount(sid,profile,profile,profile,bash,native);
        Reject(delegate { Ktu2.ValidateAccount(sid,profile,profile.ToLowerInvariant(),profile,bash,native); });
        byte[] safe=Sd("O:"+sidText+"D:(A;;FA;;;"+sidText+")");
        Ktu2.SecurityBytes(safe,(uint)safe.Length,sid,false);
        byte[] ancestor=Sd("O:"+sidText+"D:(A;;0x2;;;WD)");
        Ktu2.SecurityBytes(ancestor,(uint)ancestor.Length,sid,true);
        Reject(delegate { Ktu2.SecurityBytes(ancestor,(uint)ancestor.Length,sid,false); });
        byte[] unsafeMask=Sd("O:"+sidText+"D:(A;;GW;;;WD)");
        Reject(delegate { Ktu2.SecurityBytes(unsafeMask,(uint)unsafeMask.Length,sid,true); });
        Reject(delegate { Ktu2.SecurityBytes(safe,16385,sid,false); });
        byte[] bad=(byte[])safe.Clone();
        int acl=(int)BitConverter.ToUInt32(bad,16);
        bad[acl+8]=5;
        Reject(delegate { Ktu2.SecurityBytes(bad,(uint)bad.Length,sid,false); });
        bad=(byte[])safe.Clone();
        bad[acl+9]=32;
        Reject(delegate { Ktu2.SecurityBytes(bad,(uint)bad.Length,sid,false); });
        bad=(byte[])safe.Clone();
        bad[acl+2]=255;
        bad[acl+3]=255;
        Reject(delegate { Ktu2.SecurityBytes(bad,(uint)bad.Length,sid,false); });
        Ktu2.Need(!Ktu2.SidBytes(new byte[]{1,16,0,0,0,0,0,5}));
        var output=Console.OpenStandardOutput();
        new Ktu2.Wire(output,h).Emit("Observed",RECORD.Replace("|01010101010101010101010101010101|","|"+boot+"|"));
        output.Flush();
        return 0;
    }
}
"#;
        let source = source.replace("RECORD", &serde_json::to_string(&record("M")).unwrap());
        let mut boots = vec![];
        for _ in 0..2 {
            let bootstrap = windows::fixture_bootstrap(&source, "[F]::Run($s,$hb,$b,$q,$f)");
            let encoded = crate::logs::base64(&bootstrap.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
            assert!(encoded.len() + 100 < 32767);
            let transport = ssh::windows_factory(move |_| {
                let mut c = crate::collect::command("powershell.exe");
                c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded]);
                Ok(c)
            });
            let hash = crate::db::sha256_hex(b"source");
            let empty = crate::db::sha256_hex(b"");
            let mut img = image(b"source", &hash);
            img.mode = protocol::ExchangeMode::Inspect;
            img.bytes = b"";
            img.hash = &empty;
            let r = transport.execute_image(&node(), &img).await;
            assert!(r.reply.local_settled && r.reply.response_complete);
            assert_eq!(r.reply.code, Some(0), "native ABI/account/ACL/compiler gate must actually run");
            boots.push(r.observed.unwrap().record().split('|').nth(6).unwrap().to_string());
        }
        assert_eq!(boots[0], boots[1]);
        assert_ne!(boots[0], "0".repeat(32));
    }
    #[tokio::test]
    async fn actual_three_shell_wrappers_preserve_binary_stream_exit_and_compiler_failure_drains() {
        let source = BINARY_STREAM_FIXTURE;
        let body: Vec<u8> = (0..65537).map(|i| [0, 10, 13, 255][i % 4]).collect();
        let hash = crate::db::sha256_hex(&body);
        let record = record("M");
        for shell in ["bash", "powershell", "cmd"] {
            let bootstrap = windows::fixture_bootstrap(source, "[F]::Run($s,$hb,$b)");
            let launcher = windows::fixture_launcher(&bootstrap);
            let n = Node::from_value(&serde_json::json!({"id":"fixture","alias":"synthetic-host","os":"windows","ssh_shell":shell}), "synthetic-host");
            let remote = crate::collect::remote_command(&n, &launcher);
            assert!(remote.is_ascii() && remote.len() <= 7900, "native fixture wrapper {shell}: {}", remote.len());
            let transport = ssh::windows_factory(move |_| {
                let mut c = match shell {
                    "bash" => {
                        let mut c = crate::collect::command(r"C:\Program Files\Git\bin\bash.exe");
                        c.args(["-c", &remote]);
                        c
                    }
                    "powershell" => {
                        let mut c = crate::collect::command("powershell.exe");
                        c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &remote]);
                        c
                    }
                    _ => {
                        let mut c = crate::collect::command("cmd.exe");
                        c.args(["/d", "/s", "/c", &remote]);
                        c
                    }
                };
                c.stdin(std::process::Stdio::piped());
                Ok(c)
            });
            let mut img = image(&body, &hash);
            img.observation = &record;
            let r = transport.execute_image(&n, &img).await;
            assert!(r.reply.local_settled && r.reply.response_complete, "binary EOF/hash under {shell}");
            assert_eq!(r.reply.code, Some(2));
            assert_eq!(r.reply.facts.unwrap().disposition, PlacementDisposition::Failed);
        }
        let bootstrap = windows::fixture_bootstrap("THIS IS DELIBERATELY INVALID CSHARP", "0");
        let encoded = crate::logs::base64(&bootstrap.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
        let transport = ssh::windows_factory(move |_| {
            let mut c = crate::collect::command("powershell.exe");
            c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded]);
            Ok(c)
        });
        let mut img = image(&body, &hash);
        img.observation = &record;
        let r = transport.execute_image(&node(), &img).await;
        assert!(r.reply.local_settled && !r.reply.response_complete);
        assert_ne!(r.reply.code, Some(0));
    }

    #[tokio::test]
    async fn same_functions_reject_hash_time_security_reparse_and_delete_pending_without_missing_fallback() {
        for case in ["hash", "time", "security", "leaf-reparse", "parent-junction", "delete-pending"] {
            let mut f = Fixture::new().await;
            let target = f.bin().join("tune-agent.exe");
            std::fs::write(&target, b"old-before").unwrap();
            let snapshot = f.inspect("", "").await.observed.unwrap();
            let sentinel = f.root.join("unowned");
            std::fs::write(&sentinel, b"sentinel").unwrap();
            let sentinel_id = identity(&sentinel);
            let mut ops = String::new();
            match case {
                "hash"=>std::fs::write(&target,b"changed-hash").unwrap(),
                "time"=>std::fs::OpenOptions::new().write(true).open(&target).unwrap().set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH+Duration::from_secs(10))).unwrap(),
                "security"=>ops=r#"if(label=="opened"){string path=chain.Account.Profile+"\\.katala-tune\\bin\\tune-agent.exe";FileSecurity acl=File.GetAccessControl(path);acl.AddAccessRule(new FileSystemAccessRule(new SecurityIdentifier(chain.Account.Sid,0),FileSystemRights.ReadData,AccessControlType.Deny));File.SetAccessControl(path,acl);}"#.into(),
                "leaf-reparse"=>{std::fs::rename(&target,f.bin().join("saved-owned")).unwrap();std::os::windows::fs::symlink_file(f.bin().join("saved-owned"),&target).expect("UNVERIFIED native symlink construction: this case cannot be counted as PASS");},
                "parent-junction"=>{let old=f.home.join(".katala-tune");let saved=f.home.join("saved-owned");std::fs::rename(&old,&saved).unwrap();let result=std::process::Command::new("cmd.exe").args(["/d","/c","mklink","/J",old.to_str().unwrap(),saved.to_str().unwrap()]).output().unwrap();assert!(result.status.success(),"UNVERIFIED owned junction construction; no skip-to-pass");},
                "delete-pending"=>ops=r#"if(label=="opened"){var h=Ktu2.Open(chain.Bin,Ktu2.Target,Ktu2.Read|Ktu2.Control|Ktu2.Delete,7,1,false,false,null,this);var d=new Ktu2.DISPOSITION();d.Delete=1;Ktu2.Need(Ktu2.SetFileInformationByHandle(h.Value,4,ref d,1));Extras.Add(h);}"#.into(),
                _=>unreachable!(),
            }
            let body = super::super::super::binary::fixtures::pe();
            let r = if case == "delete-pending" { f.inspect(&ops, "").await } else { f.place(&snapshot, &body, &ops).await };
            assert!(r.reply.local_settled && r.reply.hash.is_none(), "{case}");
            if case == "delete-pending" {
                assert!(r.observed.is_none(), "delete-pending is never Missing");
            }
            assert_eq!(std::fs::read(&sentinel).unwrap(), b"sentinel");
            assert_eq!(identity(&sentinel), sentinel_id);
            if case == "security" {
                let r = f.inspect("", "").await;
                assert!(r.observed.is_none(), "access denied is never Missing");
            }
            f.cleanup();
        }
    }
    #[tokio::test]
    async fn same_functions_held_parent_prevents_rename_and_short_no_replace_rename_uses_native_abi() {
        let mut f = Fixture::new().await;
        std::fs::write(f.bin().join("tune-agent.exe"), b"old-before").unwrap();
        let id = identity(&f.bin().join("tune-agent.exe"));
        let opened = r#"bool blocked=false;try{Directory.Move(chain.Account.Profile+"\\.katala-tune\\bin",chain.Account.Profile+"\\.katala-tune\\renamed");}catch(IOException){blocked=true;}Ktu2.Need(blocked);Ktu2.State before=Ktu2.ReadLeaf(chain,Ktu2.Target);Ktu2.Rename(chain,Ktu2.Target,"x",before);Ktu2.State moved=Ktu2.ReadLeaf(chain,"x");Ktu2.Need(Ktu2.Same(moved,before,true));Ktu2.Rename(chain,"x",Ktu2.Target,moved);"#;
        let r = f.inspect("", opened).await;
        assert!(r.reply.local_settled && r.reply.response_complete);
        assert_eq!(r.reply.code, Some(0));
        assert_eq!(std::fs::read(f.bin().join("tune-agent.exe")).unwrap(), b"old-before");
        assert_eq!(identity(&f.bin().join("tune-agent.exe")), id);
        f.cleanup();
        let mut f = Fixture::new().await;
        std::fs::write(f.bin().join("tune-agent.exe"), b"old-before").unwrap();
        std::fs::write(f.bin().join("x"), b"unowned").unwrap();
        let id = identity(&f.bin().join("x"));
        let r = f.inspect("", r#"Ktu2.Rename(chain,Ktu2.Target,"x",Ktu2.ReadLeaf(chain,Ktu2.Target));"#).await;
        assert!(r.reply.local_settled && r.observed.is_none());
        assert_eq!(std::fs::read(f.bin().join("x")).unwrap(), b"unowned");
        assert_eq!(identity(&f.bin().join("x")), id);
        assert_eq!(std::fs::read(f.bin().join("tune-agent.exe")).unwrap(), b"old-before");
        f.cleanup();
    }
    #[tokio::test]
    async fn same_stream_rejects_truncation_excess_glue_mismatch_and_remote_boot_clock_mismatch() {
        let mut f = Fixture::new().await;
        let snapshot = f.inspect("", "").await.observed.unwrap();
        let body = super::super::super::binary::fixtures::pe();
        let hash = crate::db::sha256_hex(&body);
        for case in ["short", "excess", "hash", "glue-hash", "glue-size", "boot", "clock"] {
            let mut img = image(&body, &hash);
            img.observation = snapshot.record();
            img.before = snapshot.before();
            let altered;
            if case == "boot" {
                let mut parts = snapshot.record().split('|').map(str::to_owned).collect::<Vec<_>>();
                parts[6] = "02".repeat(16);
                altered = parts.join("|");
                img.observation = &altered;
            } else if case == "clock" {
                let mut parts = snapshot.record().split('|').map(str::to_owned).collect::<Vec<_>>();
                parts[7] = "0".into();
                parts[8] = "1".into();
                altered = parts.join("|");
                img.observation = &altered;
            }
            if case == "short" {
                img.bytes = &body[..body.len() - 1];
            }
            if case == "hash" {
                img.bytes = b"incorrect";
            }
            if case == "glue-hash" {
                img.glue = Box::leak(vec![b'x'; windows::GLUE.len()].into_boxed_slice());
            }
            if case == "glue-size" {
                img.glue = b"wrong";
            }
            // Render the legitimate declared frame separately so malformed wire
            // data reaches the SAME bootstrap/stream rather than local refusal.
            let mut declared = image(&body, &hash);
            declared.observation = img.observation;
            let (mut prefix, header) = declared.header().unwrap();
            if case == "glue-size" {
                prefix = format!("KTU2 {} {} {}\n", header.len(), img.glue.len(), body.len());
            }
            if case == "excess" {
                let mut bytes = body.clone();
                bytes.push(0);
                img.bytes = Box::leak(bytes.into_boxed_slice());
            }
            // This helper bypasses only locally generated header validation.
            let transport = f.transport("", "");
            let r = ssh::wire_execute(&transport, &node(), &img, &prefix, &header).await;
            assert!(r.reply.local_settled && r.reply.hash.is_none(), "{case}");
            assert!(!f.bin().join("tune-agent.exe").exists());
            assert_eq!(std::fs::read_dir(f.bin()).unwrap().count(), 0, "{case}");
        }
        f.cleanup();
    }
    #[tokio::test]
    async fn fixed_product_compiler_refusal_is_unsupported_after_same_stream_drain() {
        let bootstrap = windows::fixture_bootstrap("", r#"(& {$script:Ktu2Bridge="DELIBERATE COMPILER FAILURE";Invoke-Ktu2 $s $hb $b "" "" "" "" $q $f})"#);
        let encoded = crate::logs::base64(&bootstrap.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
        let transport = ssh::windows_factory(move |_| {
            let mut c = crate::collect::command("powershell.exe");
            c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded]);
            Ok(c)
        });
        let hash = crate::db::sha256_hex(b"source");
        let empty = crate::db::sha256_hex(b"");
        let mut img = image(b"source", &hash);
        img.mode = protocol::ExchangeMode::Inspect;
        img.bytes = b"";
        img.hash = &empty;
        let result = transport.execute_image(&node(), &img).await;
        assert!(result.reply.local_settled && result.reply.response_complete);
        assert_eq!(result.reply.code, Some(2));
        assert_eq!(result.failure, Some(InspectFailure::Unsupported));
        assert!(result.observed.is_none());
    }
    #[tokio::test]
    async fn public_windows_inspect_binding_and_native_place_keep_original_prepared_bytes() {
        let mut f = Fixture::new().await;
        let ledger = f.root.join("nodes.json");
        std::fs::write(&ledger, serde_json::json!({"nodes":[{"id":"fixture","alias":"synthetic-host","os":"windows"}]}).to_string()).unwrap();
        let engine = Engine::open(ledger, f.root.join("data"), Arc::new(crate::engine::NoHost)).unwrap();
        let request = RemoteUpdateRequest::new(
            1,
            0,
            Instant::now() + Duration::from_secs(120),
            RemoteTargetSnapshot { node: node(), architecture: RemoteArchitecture::X64 },
            tests::prepared(&f.root, crate::helpers::VERSION),
        )
        .unwrap();
        let original = request.payload().bytes().to_vec();
        let observation = f.transport("", "").inspect(&engine, &request).await.expect("public same backend native inspect");
        let request = request.with_observation(observation).unwrap();
        let source = f.root.join("synthetic-source");
        std::fs::rename(&source, f.root.join("saved-source")).unwrap();
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("tune-agent.exe"), b"path-replaced-after-preparation").unwrap();
        let result = f.transport("", "").place(&node(), request.payload()).await;
        assert!(result.local_settled && result.response_complete);
        assert_eq!(result.code, Some(0));
        assert_eq!(result.hash, Some(crate::db::sha256_hex(&original)));
        assert_eq!(std::fs::read(f.bin().join("tune-agent.exe")).unwrap(), original);
        assert_eq!(std::fs::read(source.join("tune-agent.exe")).unwrap(), b"path-replaced-after-preparation");
        drop(engine);
        f.cleanup();
    }
}
