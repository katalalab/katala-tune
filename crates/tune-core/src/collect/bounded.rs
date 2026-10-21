/// Bounded binary transport used by topology collection. It never formats raw
/// output because manifests and command errors may contain private material.
#[derive(Default)]
pub struct BoundedResult {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    pub output_limited: bool,
    pub spawn_failed: bool,
    pub process_receipt: Option<ProcessReceipt>,
}

/// Complete Windows process creation time; decimal text preserves all FILETIME bits.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub created_filetime: String,
    pub os_session: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStop {
    #[default]
    SetupFailed,
    SpawnFailed,
    Completed,
    Timeout,
    OutputLimit,
    LingeringDescendants,
    IoFailure,
    CleanupUnconfirmed,
}

/// Evidence for one owned Job. Observed identities are not an exhaustive history.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessReceipt {
    pub owner_session: String,
    pub owner: Option<ProcessIdentity>,
    pub root: Option<ProcessIdentity>,
    pub observed: Vec<ProcessIdentity>,
    pub total_processes: u32,
    pub stop: ProcessStop,
    pub root_exited: bool,
    pub root_exit_code: Option<i32>,
    pub job_empty: bool,
    pub observed_exited: bool,
    pub stdout_eof: bool,
    pub stderr_eof: bool,
    pub writer_finished: bool,
}

impl BoundedResult {
    /// A receipt is mandatory on Windows; uncertain cleanup cannot expose parser input.
    #[cfg(any(windows, test))]
    pub(crate) fn seal_windows_payload(&mut self, deadline_elapsed: bool) {
        if let Some(receipt) = self.process_receipt.as_mut().filter(|r| deadline_elapsed && r.stop == ProcessStop::Completed) {
            receipt.stop = ProcessStop::Timeout;
            self.timed_out = true;
        }
        if !self.process_receipt.as_ref().is_some_and(|r| r.stop == ProcessStop::Completed && r.cleanup_confirmed()) {
            self.code = None;
            self.stdout.clear();
            self.stderr.clear();
        }
    }
}

fn valid_identity(identity: &ProcessIdentity) -> bool {
    identity.pid != 0 && identity.created_filetime.parse::<u64>().is_ok_and(|value| value != 0 && value.to_string() == identity.created_filetime)
}

impl ProcessReceipt {
    pub fn cleanup_confirmed(&self) -> bool {
        let unique: std::collections::BTreeSet<_> = self.observed.iter().map(|i| (i.pid, &i.created_filetime, i.os_session)).collect();
        self.observed.len() <= 4096
            && unique.len() == self.observed.len()
            && self.owner.as_ref().is_some_and(valid_identity)
            && self.root.as_ref().is_some_and(valid_identity)
            && self.root.as_ref().is_some_and(|root| self.observed.contains(root) && self.owner.as_ref().is_some_and(|owner| owner.pid != root.pid))
            && self.observed.iter().all(valid_identity)
            && self.owner_session.len() == 32
            && self.owner_session.bytes().all(|b| b.is_ascii_hexdigit())
            && self.total_processes >= self.observed.len() as u32
            && self.root_exited
            && self.root_exit_code.is_some()
            && self.job_empty
            && self.observed_exited
            && self.stdout_eof
            && self.stderr_eof
            && self.writer_finished
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete() -> ProcessReceipt {
        let root = ProcessIdentity { pid: 1, created_filetime: "134000000000000001".into(), os_session: 0 };
        ProcessReceipt {
            owner_session: "0123456789abcdef0123456789abcdef".into(),
            owner: Some(ProcessIdentity { pid: 2, created_filetime: "134000000000000000".into(), os_session: 0 }),
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
        }
    }

    #[test]
    fn ack_requires_every_exit_and_pipe_observation() {
        let good = complete();
        assert!(good.cleanup_confirmed());
        for missing in 0..11 {
            let mut receipt = good.clone();
            match missing {
                0 => receipt.root_exited = false,
                1 => receipt.job_empty = false,
                2 => receipt.observed_exited = false,
                3 => receipt.stdout_eof = false,
                4 => receipt.stderr_eof = false,
                5 => receipt.writer_finished = false,
                6 => receipt.root = None,
                7 => receipt.observed.clear(),
                8 => receipt.owner_session.clear(),
                9 => receipt.owner = None,
                _ => receipt.root_exit_code = None,
            }
            assert!(!receipt.cleanup_confirmed(), "missing observation {missing}");
        }
        assert!(!ProcessReceipt::default().cleanup_confirmed());
    }

    #[test]
    fn malformed_or_duplicate_identities_cannot_ack() {
        for value in ["", "0", "01", "-1", "1.0", "18446744073709551616"] {
            let mut receipt = complete();
            receipt.owner.as_mut().unwrap().created_filetime = value.into();
            assert!(!receipt.cleanup_confirmed());
        }
        let mut duplicate = complete();
        duplicate.observed.push(duplicate.observed[0].clone());
        duplicate.total_processes = 2;
        assert!(!duplicate.cleanup_confirmed());
        let mut zero = complete();
        zero.owner.as_mut().unwrap().pid = 0;
        assert!(!zero.cleanup_confirmed());
        let mut same = complete();
        same.owner = same.root.clone();
        assert!(!same.cleanup_confirmed());
    }

    #[test]
    fn receipt_roundtrip_preserves_complete_time_and_rejects_unknown_fields() {
        let receipt = complete();
        let text = serde_json::to_string(&receipt).unwrap();
        assert!(text.contains("\"created_filetime\":\"134000000000000001\""));
        let read: ProcessReceipt = serde_json::from_str(&text).unwrap();
        assert!(read.cleanup_confirmed());
        assert!(serde_json::from_str::<ProcessReceipt>(&text.replace("\"completed\"", "\"unknown\"")).is_err());
        let mut value = serde_json::to_value(&receipt).unwrap();
        value["extra"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ProcessReceipt>(value).is_err());
    }

    #[test]
    fn completion_first_admitted_after_deadline_cannot_release_payload() {
        let mut result = BoundedResult { code: Some(0), stdout: b"payload".to_vec(), process_receipt: Some(complete()), ..Default::default() };
        result.seal_windows_payload(true);
        assert_eq!(result.code, None);
        assert!(result.timed_out);
        assert!(result.stdout.is_empty());
        assert_eq!(result.process_receipt.unwrap().stop, ProcessStop::Timeout);
    }

    #[test]
    fn failures_and_unknown_cleanup_never_release_payload() {
        for stop in [
            ProcessStop::SetupFailed,
            ProcessStop::SpawnFailed,
            ProcessStop::Timeout,
            ProcessStop::OutputLimit,
            ProcessStop::LingeringDescendants,
            ProcessStop::IoFailure,
            ProcessStop::CleanupUnconfirmed,
        ] {
            let mut receipt = complete();
            receipt.stop = stop;
            let mut result = BoundedResult { code: Some(0), stdout: b"payload".to_vec(), process_receipt: Some(receipt), ..Default::default() };
            result.seal_windows_payload(false);
            assert_eq!(result.code, None);
            assert!(result.stdout.is_empty());
        }
        let mut good = BoundedResult { code: Some(0), stdout: b"payload".to_vec(), process_receipt: Some(complete()), ..Default::default() };
        good.seal_windows_payload(false);
        assert_eq!(good.code, Some(0));
        assert_eq!(good.stdout, b"payload");
    }
}
