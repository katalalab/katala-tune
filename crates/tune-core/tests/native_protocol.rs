use serde_json::{Value, json};
use tune_core::{
    collect::{BoundedResult, ProcessIdentity, ProcessReceipt, ProcessStop},
    topology::{
        TopologyLimits,
        adapters::{
            native_protocol as native,
            remote::{RemotePlatform, source_root_fingerprint},
            windows_reader::{self as reader, ReaderRequest},
        },
    },
};

const INNER_NONCE: &str = "22222222222222222222222222222222";
const ARTIFACT: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const GIT_ARTIFACT: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
fn request() -> ReaderRequest {
    serde_json::from_value(json!({"schema":native::REQUEST_SCHEMA,"nonce":"11111111111111111111111111111111","scope_id":"fixture:native","observed_at_ms":1000,
        "expected_identity_hash":"a".repeat(64),"expected_runtime":"native","expected_distro":null,"roots":["C:/fixture"],"configs":[],"cursor":null,
        "source_allowlist":[{"root_index":0,"relative_path":".","expected_head":"b".repeat(40),"manifests":[]}],"git_program":"C:/tools/git.exe",
        "limits":TopologyLimits::default(),"budget_ms":10000,"probe_sha256":native::source_backend_sha256(),"git_artifact_sha256":GIT_ARTIFACT}))
    .unwrap()
}
fn identity(pid: u32) -> ProcessIdentity {
    ProcessIdentity { pid, created_filetime: format!("1340000000000000{pid:02}"), os_session: 1 }
}
fn receipt() -> ProcessReceipt {
    ProcessReceipt {
        owner_session: "33333333333333333333333333333333".into(),
        owner: Some(identity(1)),
        root: Some(identity(2)),
        observed: vec![identity(2)],
        total_processes: 1,
        stop: ProcessStop::Completed,
        root_exited: true,
        root_exit_code: Some(0),
        job_empty: true,
        observed_exited: true,
        stdout_eof: true,
        stderr_eof: true,
        writer_finished: true,
    }
}
fn capture(request: &ReaderRequest) -> Vec<u8> {
    serde_json::to_vec(&json!({"schema":"katala_topology_capture.v1","scope_id":request.scope_id,"adapter":"remote","observed_at_ms":request.observed_at_ms,
        "target_identity":request.expected_identity_hash,"limits":request.limits,"runtime":{"kind":"native","platform":"windows","kernel_wsl":false,"distro":null},
        "repositories":[{"relative_path":".","root_fingerprint":source_root_fingerprint("C:/fixture",RemotePlatform::Windows),"git":{"origin":null,"head":"b".repeat(40),"git_dir":"c".repeat(64),"common_dir":"c".repeat(64),"is_worktree":false},"manifests":[],"configs":[],"config_file_read":false,"failures":[]}],
        "failures":[],"counters":{"entries":1,"files":0,"bytes":0,"repositories":1,"manifests":0,"children":5,"duration_ms":1,"config_files":0,"config_keys":0},"progress":null})).unwrap()
}
fn frame(magic: &[u8], header: &[u8], body: &[u8]) -> Vec<u8> {
    let mut bytes = magic.to_vec();
    bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
    bytes.extend_from_slice(header);
    bytes.extend_from_slice(body);
    bytes
}
fn mutate(bytes: &[u8], change: impl FnOnce(&mut Value)) -> Vec<u8> {
    let n = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let mut header: Value = serde_json::from_slice(&bytes[16..16 + n]).unwrap();
    change(&mut header);
    frame(&bytes[..8], &serde_json::to_vec(&header).unwrap(), &bytes[16 + n..])
}
fn body(request: &ReaderRequest) -> Vec<u8> {
    serde_json::to_vec(request).unwrap()
}
#[test]
fn native_schema_roundtrips_and_legacy_paths_do_not_accept_it() {
    let request = request();
    let raw = body(&request);
    let encoded = frame(b"KTRDR001", &raw, &[]);
    assert!(reader::read_request(&mut std::io::Cursor::new(encoded)).is_ok());
    let output = BoundedResult { code: Some(0), stdout: capture(&request), process_receipt: Some(receipt()), ..Default::default() };
    assert!(reader::encode_response(&request, &raw, output).is_err());
    let mut mixed = request.clone();
    mixed.probe_sha256 = reader::probe_sha256();
    assert!(mixed.validate().is_err());
    mixed.schema = reader::REQUEST_SCHEMA.into();
    assert!(mixed.validate().is_err());
    mixed.git_artifact_sha256 = None;
    assert!(mixed.validate().is_ok());
    mixed.probe_sha256 = native::source_backend_sha256();
    assert!(mixed.validate().is_err());
    assert!(reader::decode_response(b"not a legacy frame", &request, &raw).is_err());
}

#[test]
fn native_constructor_and_request_caps_preserve_strict_pre_io_admission() {
    use tune_core::topology::adapters::remote::{RemoteRequest, RemoteRuntimeKind};
    let original = request();
    let remote = RemoteRequest {
        scope_id: original.scope_id.clone(),
        alias: String::new(),
        expected_identity_hash: Some(original.expected_identity_hash.clone()),
        platform: RemotePlatform::Windows,
        expected_runtime: RemoteRuntimeKind::Native,
        expected_distro: None,
        cursor: None,
        roots: original.roots.clone(),
        configs: vec![],
        source_allowlist: Some(original.source_allowlist.clone()),
        python_program: String::new(),
        windows_reader_program: None,
        git_program: Some(original.git_program.clone()),
        observed_at_ms: original.observed_at_ms,
        limits: original.limits.clone(),
    };
    let built = ReaderRequest::from_remote_native(&remote, original.nonce.clone(), original.budget_ms, GIT_ARTIFACT.into()).unwrap();
    assert_eq!(body(&built), body(&original));
    assert!(ReaderRequest::from_remote_native(&remote, original.nonce.clone(), original.budget_ms, "E".repeat(64)).is_err());
    let legacy = ReaderRequest::from_remote(&remote, original.nonce.clone(), original.budget_ms).unwrap();
    assert!(legacy.git_artifact_sha256.is_none());
    assert!(serde_json::from_slice::<Value>(&body(&legacy)).unwrap().get("git_artifact_sha256").is_none());
    let mut many = original.clone();
    many.limits.repositories_per_environment = 1000;
    many.source_allowlist = (0..820)
        .map(|i| {
            let mut checkout = original.source_allowlist[0].clone();
            checkout.relative_path = format!("repo-{i}");
            checkout
        })
        .collect();
    assert!(many.validate().is_err());
    let mut excessive = original.clone();
    excessive.limits.stdout_bytes = usize::MAX;
    assert!(excessive.validate().is_err());
    let mut fields: Value = serde_json::from_slice(&body(&original)).unwrap();
    fields["unknown"] = json!(true);
    assert!(reader::read_request(&mut std::io::Cursor::new(frame(b"KTRDR001", &serde_json::to_vec(&fields).unwrap(), &[]))).is_err());
}
#[test]
fn derived_frame_is_canonical_and_changes_only_nonce_and_budget() {
    let original = request();
    let (derived, bytes) = native::derive_inner(&original, INNER_NONCE.into(), 5000).unwrap();
    let mut expected = original.clone();
    expected.nonce = INNER_NONCE.into();
    expected.budget_ms = 5000;
    assert_eq!(body(&derived), body(&expected));
    assert_eq!(derived.git_artifact_sha256.as_deref(), Some(GIT_ARTIFACT));
    assert_eq!(bytes, reader::encode_request(&expected).unwrap());
    assert_eq!(&bytes[..8], b"KTRDR001");
    assert_eq!(&bytes[12..16], &[0; 4]);
    assert!(native::derive_inner(&original, original.nonce.clone(), 5000).is_err());
    assert!(native::derive_inner(&original, INNER_NONCE.into(), 10001).is_err());
    assert!(native::derive_inner(&original, INNER_NONCE.into(), 0).is_err());
}

#[test]
fn derived_body_golden_locks_dto_field_order_and_limit_projection() {
    let original = request();
    let (_, bytes) = native::derive_inner(&original, INNER_NONCE.into(), 5000).unwrap();
    let limits = concat!(
        "{\"roots_per_environment\":8,\"depth\":8,\"entries_per_environment\":20000,\"repositories_per_environment\":512,",
        "\"manifests_per_environment\":4096,\"file_bytes\":1048576,\"workflow_bytes\":524288,\"total_bytes_per_environment\":67108864,",
        "\"stdout_bytes\":16777216,\"stderr_bytes\":131072,\"local_timeout_seconds\":60,\"remote_timeout_seconds\":90,\"github_requests_per_run\":128}"
    );
    let expected = format!(
        concat!(
            "{{\"schema\":\"katala_native_reader_request.v2\",\"nonce\":\"{}\",\"scope_id\":\"fixture:native\",\"observed_at_ms\":1000,",
            "\"expected_identity_hash\":\"{}\",\"expected_runtime\":\"native\",\"expected_distro\":null,\"roots\":[\"C:/fixture\"],\"configs\":[],\"cursor\":null,",
            "\"source_allowlist\":[{{\"root_index\":0,\"relative_path\":\".\",\"expected_head\":\"{}\",\"manifests\":[]}}],\"git_program\":\"C:/tools/git.exe\",",
            "\"limits\":{},\"budget_ms\":5000,\"probe_sha256\":\"{}\",\"git_artifact_sha256\":\"{}\"}}"
        ),
        INNER_NONCE,
        "a".repeat(64),
        "b".repeat(40),
        limits,
        native::source_backend_sha256(),
        GIT_ARTIFACT
    );
    assert_eq!(bytes, frame(b"KTRDR001", expected.as_bytes(), &[]));
}

#[test]
fn native_git_artifact_is_required_lowercase_and_legacy_some_is_rejected() {
    let original = request();
    for value in [
        None,
        Some(json!(null)),
        Some(json!("")),
        Some(json!("E".repeat(64))),
        Some(json!("g".repeat(64))),
        Some(json!("e".repeat(63))),
        Some(json!("e".repeat(65))),
    ] {
        let mut fields: Value = serde_json::from_slice(&body(&original)).unwrap();
        if let Some(value) = value {
            fields["git_artifact_sha256"] = value;
        } else {
            fields.as_object_mut().unwrap().remove("git_artifact_sha256");
        }
        assert!(reader::read_request(&mut std::io::Cursor::new(frame(b"KTRDR001", &serde_json::to_vec(&fields).unwrap(), &[]))).is_err());
    }
    let mut legacy = original.clone();
    legacy.schema = reader::REQUEST_SCHEMA.into();
    legacy.probe_sha256 = reader::probe_sha256();
    for sha in [GIT_ARTIFACT.to_owned(), String::new(), "E".repeat(64)] {
        legacy.git_artifact_sha256 = Some(sha);
        assert!(legacy.validate().is_err());
    }
    legacy.git_artifact_sha256 = None;
    let mut explicit_null = serde_json::to_value(&legacy).unwrap();
    explicit_null["git_artifact_sha256"] = Value::Null;
    assert!(reader::read_request(&mut std::io::Cursor::new(frame(b"KTRDR001", &serde_json::to_vec(&explicit_null).unwrap(), &[]))).is_err());
    let expected = format!(
        concat!(
            "{{\"schema\":\"katala_windows_reader_request.v1\",\"nonce\":\"11111111111111111111111111111111\",\"scope_id\":\"fixture:native\",\"observed_at_ms\":1000,",
            "\"expected_identity_hash\":\"{}\",\"expected_runtime\":\"native\",\"expected_distro\":null,\"roots\":[\"C:/fixture\"],\"configs\":[],\"cursor\":null,",
            "\"source_allowlist\":[{{\"root_index\":0,\"relative_path\":\".\",\"expected_head\":\"{}\",\"manifests\":[]}}],\"git_program\":\"C:/tools/git.exe\",",
            "\"limits\":{},\"budget_ms\":10000,\"probe_sha256\":\"{}\"}}"
        ),
        "a".repeat(64),
        "b".repeat(40),
        serde_json::to_string(&TopologyLimits::default()).unwrap(),
        reader::probe_sha256()
    );
    assert_eq!(reader::encode_request(&legacy).unwrap(), frame(b"KTRDR001", expected.as_bytes(), &[]));
}

#[test]
fn approved_git_sha_changes_invalidate_processing_original_and_derived_bindings() {
    let original = request();
    let raw = body(&original);
    let (inner, derived) = native::derive_inner(&original, INNER_NONCE.into(), 5000).unwrap();
    let payload = capture(&inner);
    let response = native::encode_processing_response(&inner, &derived[16..], &identity(2), &payload).unwrap();
    let outer = native::encode_outer_response(&original, &raw, &derived, &response, &receipt(), ARTIFACT).unwrap();
    let mut changed_original = original.clone();
    changed_original.git_artifact_sha256 = Some("f".repeat(64));
    assert!(native::decode_outer_response(&outer, &changed_original, &body(&changed_original), ARTIFACT, &identity(1)).is_err());
    let mut changed_inner = inner.clone();
    changed_inner.git_artifact_sha256 = Some("f".repeat(64));
    let changed_frame = reader::encode_request(&changed_inner).unwrap();
    assert!(native::decode_processing_response(&response, &changed_inner, &changed_frame[16..]).is_err());
    let changed_response = native::encode_processing_response(&changed_inner, &changed_frame[16..], &identity(2), &payload).unwrap();
    assert!(native::encode_outer_response(&original, &raw, &changed_frame, &changed_response, &receipt(), ARTIFACT).is_err());
}
#[test]
fn processing_completion_binds_exact_body_capture_and_identity_without_exit_claims() {
    let request = request();
    let raw = body(&request);
    let capture = capture(&request);
    let frame = native::encode_processing_response(&request, &raw, &identity(2), &capture).unwrap();
    assert_eq!(native::decode_processing_response(&frame, &request, &raw).unwrap(), capture);
    let n = u32::from_le_bytes(frame[8..12].try_into().unwrap()) as usize;
    let header: Value = serde_json::from_slice(&frame[16..16 + n]).unwrap();
    assert!(header.get("process_receipt").is_none());
    assert!(header.get("self_exited").is_none());
    for key in ["nonce", "request_sha256", "source_backend_sha256", "capture_sha256", "schema"] {
        let changed = mutate(&frame, |header| header[key] = json!("wrong"));
        assert!(native::decode_processing_response(&changed, &request, &raw).is_err(), "{key}");
    }
    for value in [json!(0), json!("0"), json!("01"), json!("18446744073709551616")] {
        let changed = mutate(&frame, |header| header["reader_identity"]["created_filetime"] = value);
        assert!(native::decode_processing_response(&changed, &request, &raw).is_err());
    }
    let alternate = format!(" {} ", String::from_utf8(raw.clone()).unwrap()).into_bytes();
    assert!(native::decode_processing_response(&frame, &request, &alternate).is_err());
    assert!(native::encode_processing_response(&request, b"{}", &identity(2), &capture).is_err());
}
#[test]
fn outer_binds_original_raw_derived_frame_artifact_and_both_os_identities() {
    let original = request();
    let raw = format!(" {}\n", String::from_utf8(body(&original)).unwrap()).into_bytes();
    let (inner, derived) = native::derive_inner(&original, INNER_NONCE.into(), 5000).unwrap();
    let capture = capture(&inner);
    let response = native::encode_processing_response(&inner, &derived[16..], &identity(2), &capture).unwrap();
    let outer = native::encode_outer_response(&original, &raw, &derived, &response, &receipt(), ARTIFACT).unwrap();
    assert_eq!(native::decode_outer_response(&outer, &original, &raw, ARTIFACT, &identity(1)).unwrap(), capture);
    assert!(native::decode_outer_response(&outer, &original, &body(&original), ARTIFACT, &identity(1)).is_err());
    assert!(native::decode_outer_response(&outer, &original, &raw, &"e".repeat(64), &identity(1)).is_err());
    assert!(native::decode_outer_response(&outer, &original, &raw, ARTIFACT, &identity(3)).is_err());
    for key in ["original_request_body_sha256", "derived_request_frame_sha256", "inner_response_frame_sha256", "artifact_sha256", "inner_nonce", "schema"] {
        assert!(
            native::decode_outer_response(&mutate(&outer, |header| header[key] = json!("wrong")), &original, &raw, ARTIFACT, &identity(1)).is_err(),
            "{key}"
        );
    }
    for key in ["root_exited", "job_empty", "observed_exited", "stdout_eof", "stderr_eof", "writer_finished"] {
        assert!(
            native::decode_outer_response(&mutate(&outer, |header| header["inner_receipt"][key] = json!(false)), &original, &raw, ARTIFACT, &identity(1))
                .is_err(),
            "{key}"
        );
    }
    assert!(
        native::decode_outer_response(&mutate(&outer, |header| header["inner_receipt"]["root"] = json!(identity(3))), &original, &raw, ARTIFACT, &identity(1))
            .is_err()
    );
}
#[test]
fn strict_bounded_frames_reject_partial_extra_unknown_duplicate_and_native_cursor() {
    let original = request();
    for value in [json!({"version":1}), json!({"version":2})] {
        let mut changed = original.clone();
        changed.cursor = Some(value);
        assert!(changed.validate().is_err());
    }
    let raw = body(&original);
    let payload = capture(&original);
    let response = native::encode_processing_response(&original, &raw, &identity(2), &payload).unwrap();
    assert!(native::decode_processing_response(&response[..response.len() - 1], &original, &raw).is_err());
    let mut extra = response.clone();
    extra.push(0);
    assert!(native::decode_processing_response(&extra, &original, &raw).is_err());
    assert!(native::decode_processing_response(&mutate(&response, |header| header["extra"] = json!(true)), &original, &raw).is_err());
    let n = u32::from_le_bytes(response[8..12].try_into().unwrap()) as usize;
    let duplicate = String::from_utf8(response[16..16 + n].to_vec()).unwrap().replacen('{', "{\"nonce\":\"duplicate\",", 1);
    assert!(native::decode_processing_response(&frame(&response[..8], duplicate.as_bytes(), &response[16 + n..]), &original, &raw).is_err());
    let mut long = response.clone();
    long[8..12].copy_from_slice(&((reader::MAX_RESPONSE_HEADER_BYTES + 1) as u32).to_le_bytes());
    assert!(native::decode_processing_response(&long, &original, &raw).is_err());
}
