use serde_json::{Value, json};
use std::io::Cursor;
use tune_core::{
    collect::{BoundedResult, ProcessIdentity, ProcessReceipt, ProcessStop},
    topology::{
        TopologyLimits,
        adapters::windows_reader::{self as reader, ReaderRequest},
    },
};

fn request_body() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema":"katala_windows_reader_request.v1", "nonce":"0123456789abcdef0123456789abcdef",
        "scope_id":"fixture:environment", "observed_at_ms":1000,
        "expected_identity_hash":"a".repeat(64), "expected_runtime":"native",
        "expected_distro":null, "roots":["C:/fixture"], "configs":[], "cursor":null,
        "source_allowlist":[{"root_index":0,"relative_path":".","expected_head":"b".repeat(40),"manifests":[]}],
        "git_program":"C:/tools/git.exe", "limits":TopologyLimits::default(), "budget_ms":10000,
        "probe_sha256":reader::probe_sha256()
    }))
    .unwrap()
}
fn frame(header: &[u8], capture: &[u8]) -> Vec<u8> {
    let mut result = b"KTRDR001".to_vec();
    result.extend_from_slice(&(header.len() as u32).to_le_bytes());
    result.extend_from_slice(&(capture.len() as u32).to_le_bytes());
    result.extend_from_slice(header);
    result.extend_from_slice(capture);
    result
}
fn request() -> (ReaderRequest, Vec<u8>) {
    let body = request_body();
    let (request, bytes) = reader::read_request(&mut Cursor::new(frame(&body, &[]))).unwrap();
    assert_eq!(bytes, body);
    (request, bytes)
}
fn complete() -> BoundedResult {
    let root = ProcessIdentity { pid: 2, created_filetime: "134000000000000001".into(), os_session: 1 };
    BoundedResult {
        code: Some(0),
        stdout: b"{\"capture\":true}".to_vec(),
        process_receipt: Some(ProcessReceipt {
            owner_session: "fedcba9876543210fedcba9876543210".into(),
            owner: Some(ProcessIdentity { pid: 1, created_filetime: "134000000000000000".into(), os_session: 1 }),
            root: Some(root.clone()),
            observed: vec![root],
            total_processes: 1,
            stop: ProcessStop::Completed,
            root_exited: true,
            root_exit_code: Some(0),
            job_empty: true,
            observed_exited: true,
            stdout_eof: true,
            stderr_eof: true,
            writer_finished: true,
        }),
        ..Default::default()
    }
}
#[test]
fn one_request_frame_does_not_wait_for_eof_or_consume_following_bytes() {
    let body = request_body();
    let mut bytes = frame(&body, &[]);
    let end = bytes.len();
    bytes.extend_from_slice(b"ignored later bytes");
    let mut input = Cursor::new(bytes);
    reader::read_request(&mut input).unwrap();
    assert_eq!(input.position(), end as u64);
}
#[test]
fn request_rejects_unknown_duplicate_trailing_json_missing_and_unbounded_fields() {
    let original = request_body();
    let text = String::from_utf8(original.clone()).unwrap();
    for bytes in [format!("{} {{}}", text).into_bytes(), text.replacen('{', "{\"budget_ms\":1,", 1).into_bytes()] {
        assert!(reader::read_request(&mut Cursor::new(frame(&bytes, &[]))).is_err());
    }
    for index in 0..16 {
        let mut value: Value = serde_json::from_slice(&original).unwrap();
        match index {
            0 => value["extra"] = json!(true),
            1 => {
                value.as_object_mut().unwrap().remove("nonce");
            }
            2 => value["limits"]["stdout_bytes"] = json!(u64::MAX),
            3 => value["limits"]["extra"] = json!(0),
            4 => value["budget_ms"] = json!(0),
            5 => value["budget_ms"] = json!(reader::MAX_BUDGET_MS + 1),
            6 => value["expected_identity_hash"] = json!(""),
            7 => value["expected_runtime"] = json!("wsl"),
            8 => value["probe_sha256"] = json!("f".repeat(64)),
            9 => value["configs"] = json!([{}]),
            10 => value["source_allowlist"] = json!([]),
            11 => value["git_program"] = json!("git.exe"),
            12 => value["cursor"] = json!({"version":1,"unknown":true}),
            13 => {
                value.as_object_mut().unwrap().remove("expected_distro");
            }
            14 => {
                value.as_object_mut().unwrap().remove("cursor");
            }
            _ => value["limits"]["remote_timeout_seconds"] = json!(u64::MAX),
        }
        assert!(reader::read_request(&mut Cursor::new(frame(&serde_json::to_vec(&value).unwrap(), &[]))).is_err(), "case {index}");
    }
    let mut too_long = frame(b"", &[]);
    too_long[8..12].copy_from_slice(&((reader::MAX_REQUEST_BYTES + 1) as u32).to_le_bytes());
    assert!(reader::read_request(&mut Cursor::new(too_long)).is_err());
    assert!(reader::read_request(&mut Cursor::new(frame(&original, b"unwanted"))).is_err());
}
#[test]
fn response_needs_exact_binding_complete_frame_and_every_real_exit_ack() {
    let (request, body) = request();
    let response = reader::encode_response(&request, &body, complete()).unwrap();
    assert_eq!(reader::decode_response(&response, &request, &body).unwrap(), b"{\"capture\":true}");
    for index in 0..19 {
        let header_len = u32::from_le_bytes(response[8..12].try_into().unwrap()) as usize;
        let mut header: Value = serde_json::from_slice(&response[16..16 + header_len]).unwrap();
        match index {
            0 => header["nonce"] = json!("0".repeat(32)),
            1 => header["request_sha256"] = json!("0".repeat(64)),
            2 => header["probe_sha256"] = json!("0".repeat(64)),
            3 => header["scope_id"] = json!("other"),
            4 => header["process_receipt"]["stdout_eof"] = json!(false),
            5 => header["process_receipt"]["stderr_eof"] = json!(false),
            6 => header["process_receipt"]["job_empty"] = json!(false),
            7 => header["process_receipt"]["root_exited"] = json!(false),
            8 => header["process_receipt"]["root_exit_code"] = json!(1),
            9 => header["process_receipt"]["writer_finished"] = json!(false),
            10 => header["process_receipt"]["observed_exited"] = json!(false),
            11 => header["process_receipt"]["stop"] = json!("timeout"),
            12 => header["helper_identity"]["created_filetime"] = json!("134000000000000002"),
            13 => header["extra"] = json!(true),
            14 => {
                header["process_receipt"].as_object_mut().unwrap().remove("root_exit_code");
            }
            15 => header["capture_sha256"] = json!("0".repeat(64)),
            16 => header["budget_ms"] = json!(1),
            17 => header["process_receipt"]["owner_session"] = header["nonce"].clone(),
            _ => header["process_receipt"]["owner"]["created_filetime"] = json!(134000000000000000u64),
        }
        let changed = frame(&serde_json::to_vec(&header).unwrap(), &response[16 + header_len..]);
        assert!(reader::decode_response(&changed, &request, &body).is_err(), "case {index}");
    }
    let mut trailing = response.clone();
    trailing.push(0);
    assert!(reader::decode_response(&trailing, &request, &body).is_err());
    assert!(reader::decode_response(&response[..response.len() - 1], &request, &body).is_err());
    assert!(reader::decode_response(&response, &request, b"different exact bytes").is_err());
    for stop in [ProcessStop::Timeout, ProcessStop::OutputLimit, ProcessStop::LingeringDescendants, ProcessStop::CleanupUnconfirmed] {
        let mut output = complete();
        output.process_receipt.as_mut().unwrap().stop = stop;
        assert!(reader::encode_response(&request, &body, output).is_err());
    }
}
#[test]
fn helper_path_is_explicit_shell_safe_drive_path() {
    for good in ["C:/tools/tune.exe", "D:/Program Files/Katala Tune/tune.EXE"] {
        assert!(reader::valid_program_path(good));
    }
    for bad in [
        "tune.exe",
        "/c/tune.exe",
        "C:\\tune.exe",
        "C:/../tune.exe",
        "C:/a//tune.exe",
        "C:/a/./tune.exe",
        "C:/tune.exe;bad",
        "C:/x'tune.exe",
        "C:/$(bad)/tune.exe",
        "//server/tune.exe",
        "C:/tune.cmd",
        "C:/a /tune.exe",
    ] {
        assert!(!reader::valid_program_path(bad), "{bad}");
    }
}

#[test]
fn maximum_observed_identity_receipt_is_finite_and_does_not_lose_time_precision() {
    let (request, body) = request();
    let mut output = complete();
    let receipt = output.process_receipt.as_mut().unwrap();
    receipt.observed = (2..4098).map(|pid| ProcessIdentity { pid, created_filetime: "18446744073709551615".into(), os_session: u32::MAX }).collect();
    receipt.root = Some(receipt.observed[0].clone());
    receipt.total_processes = 4096;
    let response = reader::encode_response(&request, &body, output).unwrap();
    let size = u32::from_le_bytes(response[8..12].try_into().unwrap()) as usize;
    assert!(size < reader::MAX_RESPONSE_HEADER_BYTES);
    assert!(reader::decode_response(&response, &request, &body).is_ok());
    let mut output = complete();
    let receipt = output.process_receipt.as_mut().unwrap();
    receipt.observed = (2..4099).map(|pid| ProcessIdentity { pid, created_filetime: "18446744073709551615".into(), os_session: 0 }).collect();
    receipt.root = Some(receipt.observed[0].clone());
    receipt.total_processes = 4097;
    assert!(reader::encode_response(&request, &body, output).is_err());
}

#[test]
fn duplicate_keys_inside_an_otherwise_valid_cursor_are_not_normalized_away() {
    let mut value: Value = serde_json::from_slice(&request_body()).unwrap();
    value["cursor"] = json!({"version":1,"fingerprint":"a".repeat(64),"next_root":0,"frames":[{"dir":".","after":""}]});
    let body = serde_json::to_vec(&value).unwrap();
    assert!(reader::read_request(&mut Cursor::new(frame(&body, &[]))).is_ok());
    let duplicate = String::from_utf8(body).unwrap().replace("\"cursor\":{", "\"cursor\":{\"version\":1,");
    assert!(reader::read_request(&mut Cursor::new(frame(duplicate.as_bytes(), &[]))).is_err());
}
