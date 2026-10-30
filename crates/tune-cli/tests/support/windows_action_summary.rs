//! Test-only summaries. Raw protocol and diagnostic bytes never enter messages.
use std::time::Duration;

#[derive(Clone, Copy)]
pub enum Fixture {
    WriteError,
    ExactCap,
}

pub fn safe_summary(fixture: Fixture, receiver: Option<i32>, response: &[u8], diagnostic: &[u8], expected: (&str, &[u8]), elapsed: Duration) -> String {
    let fixture = match fixture {
        Fixture::WriteError => "write_error",
        Fixture::ExactCap => "exact_cap",
    };
    let receiver = match receiver {
        Some(0) => "zero",
        Some(7) => "seven",
        Some(9) => "nine",
        Some(_) => "other",
        None => "no_code",
    };
    format!(
        "fixture={fixture} receiver={receiver} terminal={} response_len={} diagnostic={} diagnostic_len={} complete_elapsed={}",
        terminal_shape(response, expected),
        response.len(),
        diagnostic_shape(diagnostic),
        diagnostic.len(),
        elapsed_bucket(elapsed),
    )
}

fn elapsed_bucket(elapsed: Duration) -> &'static str {
    match elapsed.as_secs() {
        0..=4 => "0_5s",
        5..=7 => "5_8s",
        _ => "8s_plus",
    }
}

// The production KT2 parser and receiver limits are private. Keep this narrow
// shape check in tests instead of exposing or changing a runtime protocol API.
fn terminal_shape(response: &[u8], (expected_nonce, body): (&str, &[u8])) -> &'static str {
    if response.is_empty() {
        return "empty";
    }
    let parsed = (|| {
        let end = response.iter().take(256).position(|byte| *byte == b'\n')?;
        let header = std::str::from_utf8(&response[..end]).ok()?;
        let fields: Vec<_> = header.split(' ').collect();
        let ["KT2", "RESULT", nonce, hash, code, stdout, stderr] = fields.as_slice() else { return None };
        if *nonce != expected_nonce
            || nonce.len() != 64
            || !nonce.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || *hash != tune_core::db::sha256_hex(body)
            || !matches!(*code, "0" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        {
            return None;
        }
        let count = |value: &str| {
            let number = value.parse::<usize>().ok()?;
            (number <= 65_536 && value == number.to_string()).then_some(number)
        };
        let length = (end + 1).checked_add(count(stdout)?)?.checked_add(count(stderr)?)?;
        Some(if response.len() < length {
            "header_incomplete"
        } else if response.len() > length {
            "invalid"
        } else {
            match *code {
                "0" => "complete_zero",
                "7" => "complete_seven",
                _ => "complete_other_allowed",
            }
        })
    })();
    parsed.unwrap_or("invalid")
}

enum Diagnostic {
    Empty,
    Invalid,
    // All fields point to allowlisted literals, never slices of the input.
    Fixed([&'static str; 8]),
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("empty"),
            Self::Invalid => f.write_str("invalid"),
            Self::Fixed([stage, outcome, timed_out, limited, spawn, stop, cleanup, elapsed]) => write!(
                f,
                "valid_fixed stage={stage} outcome={outcome} timed_out={timed_out} output_limited={limited} spawn_failed={spawn} receipt_stop={stop} cleanup_confirmed={cleanup} elapsed={elapsed}"
            ),
        }
    }
}

fn diagnostic_shape(bytes: &[u8]) -> Diagnostic {
    if bytes.is_empty() {
        return Diagnostic::Empty;
    }
    let parsed = (|| {
        if bytes.len() > 512 {
            return None;
        }
        let line = std::str::from_utf8(bytes).ok()?.strip_suffix('\n')?;
        let fields: Vec<_> = line.split(' ').collect();
        let ["KT2", "DIAG", stage, outcome, timed_out, limited, spawn, stop, cleanup, elapsed] = fields.as_slice() else { return None };
        let pair = (stage.strip_prefix("stage=")?, outcome.strip_prefix("outcome=")?);
        let (stage, outcome) = [
            ("setup", "powershell_unavailable"),
            ("setup", "environment_empty"),
            ("setup", "runtime_failed"),
            ("backend", "deadline_before_start"),
            ("backend", "deadline_elapsed"),
            ("backend", "timed_out"),
            ("backend", "output_limited"),
            ("backend", "spawn_failed"),
            ("backend", "missing_exit_code"),
            ("backend", "unexpected_exit_code"),
            ("backend", "receipt_unconfirmed"),
            ("terminal", "stdout_header_write_failed"),
            ("terminal", "stdout_payload_write_failed"),
        ]
        .into_iter()
        .find(|known| *known == pair)?;
        let flag = |value: &str, prefix: &str| match value.strip_prefix(prefix)? {
            "0" => Some("0"),
            "1" => Some("1"),
            _ => None,
        };
        let stop = stop.strip_prefix("receipt_stop=")?;
        let stop =
            ["absent", "setup_failed", "spawn_failed", "completed", "timeout", "output_limit", "lingering_descendants", "io_failure", "cleanup_unconfirmed"]
                .into_iter()
                .find(|known| *known == stop)?;
        let elapsed = elapsed.strip_prefix("elapsed=")?;
        let elapsed = ["0_5s", "5_8s", "8s_plus"].into_iter().find(|known| *known == elapsed)?;
        Some([
            stage,
            outcome,
            flag(timed_out, "timed_out=")?,
            flag(limited, "output_limited=")?,
            flag(spawn, "spawn_failed=")?,
            stop,
            flag(cleanup, "cleanup_confirmed=")?,
            elapsed,
        ])
    })();
    parsed.map_or(Diagnostic::Invalid, Diagnostic::Fixed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BODY: &[u8] = b"synthetic-private-body";

    fn summary(response: &[u8], diagnostic: &[u8]) -> String {
        safe_summary(Fixture::WriteError, Some(7), response, diagnostic, (NONCE, BODY), Duration::from_secs(2))
    }

    fn frame(code: &str, stdout: &str, stderr: &str, payload: &[u8]) -> Vec<u8> {
        [format!("KT2 RESULT {NONCE} {} {code} {stdout} {stderr}\n", tune_core::db::sha256_hex(BODY)).as_bytes(), payload].concat()
    }

    #[test]
    fn fixed_backend_diagnostic_preserves_the_observed_class_without_echoing_bytes() {
        let diagnostic = b"KT2 DIAG stage=backend outcome=missing_exit_code timed_out=0 output_limited=0 spawn_failed=0 receipt_stop=io_failure cleanup_confirmed=1 elapsed=0_5s\n";
        let rendered = summary(b"", diagnostic);
        assert!(rendered.contains("receiver=seven terminal=empty response_len=0"));
        assert!(rendered.contains("diagnostic=valid_fixed stage=backend outcome=missing_exit_code"));
        assert!(rendered.contains("receipt_stop=io_failure cleanup_confirmed=1 elapsed=0_5s"));
        assert!(!rendered.contains("watchdog"));
    }

    #[test]
    fn every_existing_stage_outcome_and_stop_is_allowlisted() {
        for (stage, outcomes) in [
            ("setup", "powershell_unavailable environment_empty runtime_failed"),
            (
                "backend",
                "deadline_before_start deadline_elapsed timed_out output_limited spawn_failed missing_exit_code unexpected_exit_code receipt_unconfirmed",
            ),
            ("terminal", "stdout_header_write_failed stdout_payload_write_failed"),
        ] {
            for outcome in outcomes.split(' ') {
                for stop in [
                    "absent",
                    "setup_failed",
                    "spawn_failed",
                    "completed",
                    "timeout",
                    "output_limit",
                    "lingering_descendants",
                    "io_failure",
                    "cleanup_unconfirmed",
                ] {
                    let diagnostic = format!(
                        "KT2 DIAG stage={stage} outcome={outcome} timed_out=1 output_limited=1 spawn_failed=1 receipt_stop={stop} cleanup_confirmed=0 elapsed=8s_plus\n"
                    );
                    let rendered = summary(b"", diagnostic.as_bytes());
                    assert!(rendered.contains(&format!("diagnostic=valid_fixed stage={stage} outcome={outcome}")));
                    assert!(rendered.contains(&format!("receipt_stop={stop} cleanup_confirmed=0 elapsed=8s_plus")));
                }
            }
        }
    }

    #[test]
    fn absent_diagnostic_keeps_the_cause_unknown() {
        let rendered = summary(b"", b"");
        assert!(rendered.contains("diagnostic=empty diagnostic_len=0"));
        assert!(!rendered.contains("watchdog") && !rendered.contains("io_failure") && !rendered.contains("timed_out"));
    }

    #[test]
    fn malformed_or_unrecognized_diagnostic_cannot_echo_private_material() {
        let good =
            "KT2 DIAG stage=backend outcome=timed_out timed_out=1 output_limited=0 spawn_failed=0 receipt_stop=timeout cleanup_confirmed=1 elapsed=5_8s\n";
        let cases = [
            good.replace("stage=backend", "stage=synthetic-private-stage"),
            good.replace("outcome=timed_out", "outcome=synthetic-private-outcome"),
            good.replace("stage=backend", "stage=setup"),
            good.replace("timed_out=1", "timed_out=2"),
            good.replace("receipt_stop=timeout", "receipt_stop=synthetic-private-stop"),
            good.replace("cleanup_confirmed=1", "cleanup_confirmed=01"),
            good.replace("elapsed=5_8s", "elapsed=synthetic-private-time"),
            good.replace(" timed_out=1", " timed_out=1 timed_out=1"),
            good.replace(" timed_out=1", ""),
            good.replace("stage=backend outcome=timed_out", "outcome=timed_out stage=backend"),
            good.replace('\n', " extra=synthetic-private-extra\n"),
            format!("{good}synthetic-private-line\n"),
            good.replace('\n', "\r\n"),
            good.trim_end().into(),
            format!("{good}{}", "synthetic-private-large".repeat(30)),
        ];
        for bytes in cases.iter().map(String::as_bytes).chain([b"\xffsynthetic-private-utf8".as_slice()]) {
            let rendered = summary(b"", bytes);
            assert!(rendered.contains("diagnostic=invalid"));
            assert!(!rendered.contains("synthetic-private") && !rendered.contains("watchdog"));
            assert!(!rendered.contains("stage=") && !rendered.contains("receipt_stop="));
        }
    }

    #[test]
    fn terminal_shape_checks_binding_canonical_fields_caps_and_payload_length() {
        for (response, terminal) in [
            (frame("0", "0", "0", b""), "complete_zero"),
            (frame("7", "0", "0", b""), "complete_seven"),
            (frame("3", "0", "0", b""), "complete_other_allowed"),
            (frame("9", "0", "0", b""), "complete_other_allowed"),
            (frame("7", "2", "0", b"x"), "header_incomplete"),
            (frame("7", "1", "0", b"xy"), "invalid"),
            (frame("2", "0", "0", b""), "invalid"),
            (frame("07", "0", "0", b""), "invalid"),
            (frame("7", "00", "0", b""), "invalid"),
            (frame("7", "65537", "0", b""), "invalid"),
            (frame("7", "0", "18446744073709551616", b""), "invalid"),
            (frame("7", "-1", "0", b""), "invalid"),
        ] {
            assert!(summary(&response, b"").contains(&format!("terminal={terminal} ")));
        }
        let exact = frame("0", "65536", "65536", &vec![0; 131_072]);
        assert!(summary(&exact, b"").contains("terminal=complete_zero "));
        for response in [
            frame("7", "0", "0", b"").into_iter().chain(b"KT2 RESULT synthetic-private-extra".iter().copied()).collect(),
            b"KT2 RESULT synthetic-private-short\n".to_vec(),
            vec![b'x'; 257],
            b"\xffsynthetic-private-header\n".to_vec(),
        ] {
            assert!(summary(&response, b"").contains("terminal=invalid "));
        }
    }

    #[test]
    fn wrong_nonce_and_wrong_hash_are_rejected_independently() {
        let good = String::from_utf8(frame("7", "0", "0", b"")).unwrap();
        for response in [good.replacen(NONCE, &"b".repeat(64), 1), good.replacen(&tune_core::db::sha256_hex(BODY), &"0".repeat(64), 1)] {
            assert!(summary(response.as_bytes(), b"").contains("terminal=invalid "));
        }
    }

    #[test]
    fn valid_terminal_payload_nonce_hash_and_request_are_never_formatted() {
        let payload = b"synthetic-private-response";
        let response = frame("7", "26", "0", payload);
        // Hand-counted payload length is 26; valid framing must still hide it.
        assert_eq!(payload.len(), 26);
        let rendered = summary(&response, b"synthetic-private-diagnostic");
        assert!(rendered.contains("terminal=complete_seven "));
        for private in ["synthetic-private", NONCE, &tune_core::db::sha256_hex(BODY)] {
            assert!(!rendered.contains(private));
        }
    }

    #[test]
    fn receiver_codes_fixture_names_and_elapsed_are_closed_categories() {
        for (code, expected) in [(Some(0), "zero"), (Some(7), "seven"), (Some(9), "nine"), (Some(206), "other"), (None, "no_code")] {
            for (seconds, bucket) in [(0, "0_5s"), (4, "0_5s"), (5, "5_8s"), (7, "5_8s"), (8, "8s_plus")] {
                let rendered = safe_summary(Fixture::ExactCap, code, b"", b"", (NONCE, BODY), Duration::from_secs(seconds));
                assert!(rendered.starts_with(&format!("fixture=exact_cap receiver={expected} terminal=empty ")));
                assert!(rendered.ends_with(&format!("complete_elapsed={bucket}")));
            }
        }
    }
}
