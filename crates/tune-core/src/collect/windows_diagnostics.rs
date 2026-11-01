//! Closed, test-only diagnostics: no process identity or arbitrary text can be formatted.
#![cfg(test)]

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageKind {
    SelfFixture,
    Git,
    PowerShell,
    Conhost,
    WerFault,
    Other,
    Unknown,
}

pub fn image_kind(basename: Option<&str>, fixture_basename: Option<&str>) -> ImageKind {
    let Some(basename) = basename else { return ImageKind::Unknown };
    if fixture_basename.is_some_and(|fixture| basename.eq_ignore_ascii_case(fixture)) {
        return ImageKind::SelfFixture;
    }
    match basename.to_ascii_lowercase().as_str() {
        "git.exe" => ImageKind::Git,
        "powershell.exe" | "pwsh.exe" => ImageKind::PowerShell,
        "conhost.exe" => ImageKind::Conhost,
        "werfault.exe" => ImageKind::WerFault,
        _ => ImageKind::Other,
    }
}

pub const MEMBER_LIMIT: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    FirstStop,
    TerminateRequested,
    CleanupConfirmed,
    CleanupExpired,
}

#[derive(Clone, Copy, Debug)]
pub enum Stop {
    SetupFailed,
    SpawnFailed,
    Completed,
    Timeout,
    OutputLimit,
    LingeringDescendants,
    IoFailure,
    CleanupUnconfirmed,
}

pub struct ReceiptState {
    pub stop: Stop,
    pub root_exited: bool,
    pub job_empty: bool,
    pub observed_exited: bool,
    pub stdout_eof: bool,
    pub stderr_eof: bool,
    pub writer_finished: bool,
    pub termination_requested: bool,
}

pub struct Accounting {
    pub active: Option<u32>,
    pub total: Option<u32>,
    pub error: Option<i32>,
}

pub struct Member {
    pub ordinal: usize,
    pub primary_root: bool,
    pub in_job: Option<bool>,
    pub membership_error: Option<i32>,
    pub wait: u32,
    pub wait_error: Option<i32>,
    pub exit_code: Option<u32>,
    pub exit_error: Option<i32>,
    pub image: ImageKind,
    pub image_error: Option<i32>,
}

pub struct Snapshot<'a> {
    pub event: Event,
    pub invocation: u64,
    pub elapsed_ms: u128,
    pub receipt: ReceiptState,
    pub accounting: Accounting,
    pub retained_count: usize,
    pub members: &'a [Member],
}

impl Snapshot<'_> {
    pub fn line(&self) -> String {
        use std::fmt::Write;
        let r = &self.receipt;
        let a = &self.accounting;
        let count = self.members.len().min(MEMBER_LIMIT);
        let mut line = format!(
            "owned_job_diagnostic event={:?} invocation={} elapsed_ms={} stop={:?} receipt_root_exited={} receipt_job_empty={} receipt_observed_exited={} stdout_eof={} stderr_eof={} writer_finished={} termination_requested={} accounting_active={:?} accounting_total={:?} accounting_error={:?} retained_count={} sampled_count={} truncated={}",
            self.event,
            self.invocation,
            self.elapsed_ms,
            r.stop,
            r.root_exited,
            r.job_empty,
            r.observed_exited,
            r.stdout_eof,
            r.stderr_eof,
            r.writer_finished,
            r.termination_requested,
            a.active,
            a.total,
            a.error,
            self.retained_count,
            count,
            self.retained_count > count,
        );
        for m in &self.members[..count] {
            write!(
                line,
                " | ordinal={} primary_root={} in_job={:?} membership_error={:?} wait={} wait_error={:?} exit_code={:?} exit_error={:?} image={:?} image_error={:?}",
                m.ordinal, m.primary_root, m.in_job, m.membership_error, m.wait, m.wait_error, m.exit_code, m.exit_error, m.image, m.image_error,
            )
            .expect("writing diagnostics to a String cannot fail");
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_labels_discard_private_names_and_preserve_only_closed_matches() {
        for (basename, expected) in [
            (Some("fixture-test.exe"), ImageKind::SelfFixture),
            (Some("GIT.EXE"), ImageKind::Git),
            (Some("powershell.exe"), ImageKind::PowerShell),
            (Some("conhost.exe"), ImageKind::Conhost),
            (Some("WerFault.exe"), ImageKind::WerFault),
            (Some("private-project-secret.exe"), ImageKind::Other),
            (Some("C:\\private\\git.exe"), ImageKind::Other),
            (Some("git.exe\nprivate=value"), ImageKind::Other),
            (None, ImageKind::Unknown),
        ] {
            assert_eq!(image_kind(basename, Some("fixture-test.exe")), expected);
        }
    }

    #[test]
    fn closed_output_distinguishes_events_and_bounds_later_handle_samples() {
        let members: Vec<_> = (0..MEMBER_LIMIT + 3)
            .map(|ordinal| Member {
                ordinal,
                primary_root: ordinal == 0,
                in_job: None,
                membership_error: Some(5),
                wait: u32::MAX,
                wait_error: Some(6),
                exit_code: None,
                exit_error: Some(6),
                image: image_kind(Some("private-project-secret.exe"), Some("fixture-test.exe")),
                image_error: None,
            })
            .collect();
        for (event, label) in [
            (Event::FirstStop, "FirstStop"),
            (Event::TerminateRequested, "TerminateRequested"),
            (Event::CleanupConfirmed, "CleanupConfirmed"),
            (Event::CleanupExpired, "CleanupExpired"),
        ] {
            let line = Snapshot {
                event,
                invocation: 3,
                elapsed_ms: 17,
                receipt: ReceiptState {
                    stop: Stop::LingeringDescendants,
                    root_exited: true,
                    job_empty: false,
                    observed_exited: false,
                    stdout_eof: false,
                    stderr_eof: true,
                    writer_finished: true,
                    termination_requested: event != Event::FirstStop,
                },
                accounting: Accounting { active: None, total: None, error: Some(6) },
                retained_count: members.len(),
                members: &members,
            }
            .line();
            assert!(line.contains(&format!("event={label}")));
            assert!(line.contains("stop=LingeringDescendants"));
            assert!(line.contains("accounting_error=Some(6)"));
            assert!(line.contains("in_job=None membership_error=Some(5)"));
            assert!(line.contains("truncated=true"));
            assert_eq!(line.matches("ordinal=").count(), MEMBER_LIMIT);
            assert!(!line.contains("private") && !line.contains("fixture-test.exe") && !line.contains("pid=") && !line.contains("filetime"));
        }
    }
}
