//! Test-only first adopted IO failure. No identities, paths, payloads or text.
#![cfg(test)]
use serde::{Deserialize, Serialize};

pub const PREFIX: &str = "KATALA_WINDOWS_FIRST_IO=";
pub const MAX_RECORD_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    JobAccounting,
    JobPidQuery,
    JobPidValidation,
    ObservedLimit,
    OpenProcess,
    MembershipQuery,
    MembershipValidation,
    IdentityTimes,
    IdentitySession,
    IdentityValidation,
    RootWait,
    RootExitCode,
    MemberWait,
    FixtureHook,
    WriterIo,
    WriterJoin,
    StdoutRead,
    StdoutJoin,
    StderrRead,
    StderrJoin,
    ObserveUnknown,
    QuiescenceStop,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PidListReason {
    AssignedCountMismatch,
    CountOutOfBounds,
    IdentifierOutOfRange,
    ZeroIdentifier,
    DuplicateIdentifier,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PidListShape {
    pub assigned: u32,
    pub returned: u32,
    pub reason: PidListReason,
    pub query_context: QueryContext,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryContext {
    Initial,
    AfterInvalidParameter,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub stage: Stage,
    pub raw_os_error: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_list: Option<PidListShape>,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub invocation: u64,
    pub failure: Failure,
}
impl Record {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_RECORD_BYTES {
            return None;
        }
        serde_json::from_slice(bytes).ok()
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Attempt {
    Alias,
    Canonical,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct AttemptFailure {
    pub attempt: Attempt,
    pub failure: Failure,
}
// Markers are independent of the final diagnostic shape, including setup
// failure. At most two attempts and one adopted failure per attempt are valid.
pub fn attempt_failures(text: &str) -> Option<Vec<AttemptFailure>> {
    let mut phase = None;
    let mut seen_attempts = [false; 2];
    let mut seen_errors = [false; 2];
    let mut failures = Vec::new();
    for line in text.lines() {
        if let Some(label) = line.strip_prefix("KATALA_LEGACY_ATTEMPT=") {
            let index = match label {
                "alias" if phase.is_none() => 0,
                "canonical" if !seen_attempts[1] => 1,
                _ => return None,
            };
            if seen_attempts[index] {
                return None;
            }
            seen_attempts[index] = true;
            phase = Some(index);
        } else if let Some(json) = line.strip_prefix(PREFIX) {
            let record = Record::parse(json.as_bytes())?;
            let index = phase?;
            if seen_errors[index] {
                return None;
            }
            seen_errors[index] = true;
            failures.push(AttemptFailure { attempt: if index == 0 { Attempt::Alias } else { Attempt::Canonical }, failure: record.failure });
        }
    }
    seen_attempts[1].then_some(failures)
}

#[derive(Default)]
pub struct FirstFailure(pub Option<Failure>);
impl FirstFailure {
    pub fn adopt(&mut self, failure: Failure) {
        self.0.get_or_insert(failure);
    }
}
thread_local! {
    static CANDIDATE: std::cell::Cell<Option<Failure>> = const { std::cell::Cell::new(None) };
    static QUERY: std::cell::Cell<QueryContext> = const { std::cell::Cell::new(QueryContext::Initial) };
}
pub fn clear_candidate() {
    CANDIDATE.set(None);
    QUERY.set(QueryContext::Initial);
}
pub fn set_query_context(context: QueryContext) {
    QUERY.set(context);
}
pub fn candidate() -> Option<Failure> {
    CANDIDATE.get()
}
pub fn note(stage: Stage, error: &std::io::Error) {
    CANDIDATE.set(Some(Failure { stage, raw_os_error: error.raw_os_error(), pid_list: None }));
}

pub fn note_pid_list(reason: PidListReason, assigned: u32, returned: u32, error: &std::io::Error) {
    CANDIDATE.set(Some(Failure {
        stage: Stage::JobPidValidation,
        raw_os_error: error.raw_os_error(),
        pid_list: Some(PidListShape { assigned, returned, reason, query_context: QUERY.get() }),
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn later_cleanup_failure_does_not_replace_first_adopted_failure() {
        let first = Failure { stage: Stage::WriterIo, raw_os_error: Some(109), pid_list: None };
        let mut trace = FirstFailure::default();
        trace.adopt(first);
        trace.adopt(Failure { stage: Stage::JobPidValidation, raw_os_error: None, pid_list: None });
        assert_eq!(trace.0, Some(first));
    }
    #[test]
    fn recovered_candidate_is_not_fatal_and_none_is_not_an_os_code() {
        clear_candidate();
        note(Stage::OpenProcess, &std::io::Error::from_raw_os_error(87));
        clear_candidate(); // successful observation/recovered87 is not adopted
        assert_eq!(candidate(), None);
        let mut trace = FirstFailure::default();
        note(Stage::JobPidValidation, &std::io::Error::from(std::io::ErrorKind::InvalidInput));
        trace.adopt(candidate().unwrap());
        assert_eq!(trace.0.unwrap().raw_os_error, None);
        clear_candidate();
    }
    #[test]
    fn structural_records_reject_extensions_duplicates_text_and_oversized_input() {
        let valid = Record { invocation: 1, failure: Failure { stage: Stage::JobPidValidation, raw_os_error: None, pid_list: None } };
        let bytes = serde_json::to_vec(&valid).unwrap();
        assert_eq!(Record::parse(&bytes), Some(valid));
        for bad in [
            br#"{"invocation":1,"invocation":2,"failure":{"stage":"writer_io","raw_os_error":5}}"#.to_vec(),
            br#"{"invocation":1,"failure":{"stage":"writer_io","raw_os_error":null,"path":"private"}}"#.to_vec(),
            br#"{"invocation":1,"failure":{"stage":"private","raw_os_error":null}}"#.to_vec(),
            br#"{"invocation":1,"failure":{"stage":"writer_io","raw_os_error":"private"}}"#.to_vec(),
            [bytes.clone(), vec![b' '; MAX_RECORD_BYTES]].concat(),
        ] {
            assert_eq!(Record::parse(&bad), None);
        }
        assert!(!PREFIX.contains("private"));
    }
    fn io_line(stage: Stage) -> String {
        format!("{PREFIX}{}", serde_json::to_string(&Record { invocation: 0, failure: Failure { stage, raw_os_error: None, pid_list: None } }).unwrap())
    }
    #[test]
    fn attempts_survive_preparation_records_without_alias_flags() {
        let text = format!(
            "KATALA_LEGACY_ATTEMPT=alias\n{}\nKATALA_LEGACY_ATTEMPT=canonical\nKATALA_LEGACY_PROBE_DIAGNOSTIC={{\"stage\":\"launch_preparation\"}}",
            io_line(Stage::WriterIo)
        );
        assert_eq!(
            attempt_failures(&text),
            Some(vec![AttemptFailure { attempt: Attempt::Alias, failure: Failure { stage: Stage::WriterIo, raw_os_error: None, pid_list: None } }])
        );
        let text = format!("KATALA_LEGACY_ATTEMPT=canonical\n{}", io_line(Stage::JobPidValidation));
        assert_eq!(attempt_failures(&text).unwrap()[0].attempt, Attempt::Canonical);
        assert_eq!(attempt_failures("KATALA_LEGACY_ATTEMPT=canonical"), Some(vec![]));
    }
    #[test]
    fn attempts_reject_unbound_reordered_duplicate_and_invalid_records() {
        let io = io_line(Stage::WriterIo);
        for bad in [
            io.clone(),
            format!("KATALA_LEGACY_ATTEMPT=alias\n{io}"),
            "KATALA_LEGACY_ATTEMPT=canonical\nKATALA_LEGACY_ATTEMPT=alias".into(),
            "KATALA_LEGACY_ATTEMPT=canonical\nKATALA_LEGACY_ATTEMPT=canonical".into(),
            "KATALA_LEGACY_ATTEMPT=private".into(),
            format!("KATALA_LEGACY_ATTEMPT=canonical\n{io}\n{io}"),
            format!("KATALA_LEGACY_ATTEMPT=canonical\n{PREFIX}{{}}"),
        ] {
            assert_eq!(attempt_failures(&bad), None);
        }
    }
    #[test]
    fn pid_shape_survives_first_adoption_without_identifiers_or_text() {
        clear_candidate();
        note_pid_list(PidListReason::AssignedCountMismatch, 18, 17, &std::io::Error::from(std::io::ErrorKind::InvalidInput));
        let failure = candidate().unwrap();
        assert_eq!(
            failure.pid_list,
            Some(PidListShape { assigned: 18, returned: 17, reason: PidListReason::AssignedCountMismatch, query_context: QueryContext::Initial })
        );
        let mut first = FirstFailure::default();
        first.adopt(failure);
        first.adopt(Failure { stage: Stage::WriterIo, raw_os_error: Some(5), pid_list: None });
        let record = Record { invocation: 1, failure: first.0.unwrap() };
        let json = serde_json::to_vec(&record).unwrap();
        assert_eq!(Record::parse(&json), Some(record));
        let with_ids = br#"{"invocation":1,"failure":{"stage":"job_pid_validation","raw_os_error":null,"pid_list":{"assigned":18,"returned":17,"reason":"assigned_count_mismatch","query_context":"initial","pids":[123456]}}}"#;
        assert_eq!(Record::parse(with_ids), None);
        clear_candidate();
    }

    #[test]
    fn fresh_invalid_parameter_query_context_is_explicit() {
        clear_candidate();
        set_query_context(QueryContext::AfterInvalidParameter);
        note_pid_list(PidListReason::AssignedCountMismatch, 2, 1, &std::io::Error::from(std::io::ErrorKind::InvalidInput));
        assert_eq!(candidate().unwrap().pid_list.unwrap().query_context, QueryContext::AfterInvalidParameter);
        clear_candidate();
    }
}
