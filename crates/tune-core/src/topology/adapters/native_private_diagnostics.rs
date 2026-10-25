//! Private compile-time failure observations. Never admission authority or IO.
use std::cell::Cell;

const RESERVED: [i32; 10] = [0, 1, 2, 3, 7, 9, 101, 124, 126, 127];

macro_rules! stages {
    ($($name:ident = $code:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        #[repr(u8)]
        pub enum Stage { $($name = $code),+ }
        impl Stage {
            fn from_code(code: i32) -> Option<Self> {
                match code { $($code => Some(Self::$name),)+ _ => None }
            }
            #[cfg(test)]
            const ALL: &'static [Self] = &[$(Self::$name),+];
        }
    };
}
stages! {
    MainArguments = 40, MainThread = 41, MainRequest = 42,
    MainAdmission = 43, MainWrite = 44, MainDispatch = 45,
    ReaderBody = 50, ReaderPin = 51, ReaderRuntime = 52,
    ReaderSerialize = 53, ReaderBounds = 54, ReaderIdentity = 55,
    ReaderProtocol = 56, ReaderDeadline = 57,
    VerifyBody = 70, VerifyBudget = 71, VerifyDerive = 72,
    VerifyPin = 73, VerifyEnvironment = 74, VerifyRuntime = 75,
    VerifyReceipt = 76, VerifyConsume = 77, VerifyProtocol = 78, VerifyDeadline = 79,
    SuperviseBody = 80, SuperviseBudget = 81, SuperviseNonce = 82,
    SuperviseDerive = 83, SupervisePin = 84, SuperviseEnvironment = 85,
    SuperviseRuntime = 86, SuperviseReceipt = 87, SuperviseConsume = 88,
    SuperviseProtocol = 89, SuperviseDeadline = 90,
    GitProgram = 91, GitConsume = 92,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    Unsupported,
    Policy,
    Limit,
    Timeout,
    Source,
    Utf8,
    Git,
    Identity,
}
const SOURCES: [Source; 8] = [Source::Unsupported, Source::Policy, Source::Limit, Source::Timeout, Source::Source, Source::Utf8, Source::Git, Source::Identity];
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    GitDir,
    CommonDir,
    Origin,
    Head,
}
const OPERATIONS: [Operation; 4] = [Operation::GitDir, Operation::CommonDir, Operation::Origin, Operation::Head];
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
const STOPS: [Stop; 8] = [
    Stop::SetupFailed,
    Stop::SpawnFailed,
    Stop::Completed,
    Stop::Timeout,
    Stop::OutputLimit,
    Stop::LingeringDescendants,
    Stop::IoFailure,
    Stop::CleanupUnconfirmed,
];
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Verify,
    Supervise,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IoSite {
    WriterWrite,
    WriterJoin,
    StdoutRead,
    StdoutJoin,
    StderrRead,
    StderrJoin,
    AccountingInitial,
    PidListQuery,
    PidListShape,
    RetentionCap,
    OpenCandidate,
    DisappearanceQuery,
    MembershipQuery,
    NonMember,
    IdentityTimes,
    IdentitySession,
    IdentityInvariant,
    RootWait,
    RootExitCode,
    AccountingAfterRoot,
    RetainedWait,
    QuiescenceStopped,
}
const IO_SITES: [IoSite; 22] = [
    IoSite::WriterWrite,
    IoSite::WriterJoin,
    IoSite::StdoutRead,
    IoSite::StdoutJoin,
    IoSite::StderrRead,
    IoSite::StderrJoin,
    IoSite::AccountingInitial,
    IoSite::PidListQuery,
    IoSite::PidListShape,
    IoSite::RetentionCap,
    IoSite::OpenCandidate,
    IoSite::DisappearanceQuery,
    IoSite::MembershipQuery,
    IoSite::NonMember,
    IoSite::IdentityTimes,
    IoSite::IdentitySession,
    IoSite::IdentityInvariant,
    IoSite::RootWait,
    IoSite::RootExitCode,
    IoSite::AccountingAfterRoot,
    IoSite::RetainedWait,
    IoSite::QuiescenceStopped,
];
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IoClass {
    InvalidParameter,
    PermissionDenied,
    InvalidHandle,
    MoreData,
    BrokenPipe,
    Aborted,
    InvalidInput,
    NotFound,
    JoinCancelled,
    JoinPanic,
    Other,
    Invariant,
}
const IO_CLASSES: [IoClass; 12] = [
    IoClass::InvalidParameter,
    IoClass::PermissionDenied,
    IoClass::InvalidHandle,
    IoClass::MoreData,
    IoClass::BrokenPipe,
    IoClass::Aborted,
    IoClass::InvalidInput,
    IoClass::NotFound,
    IoClass::JoinCancelled,
    IoClass::JoinPanic,
    IoClass::Other,
    IoClass::Invariant,
];
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IoFault {
    pub site: IoSite,
    pub class: IoClass,
}

pub fn io_class(error: &std::io::Error) -> IoClass {
    use std::io::ErrorKind;
    match error.raw_os_error() {
        Some(87) => IoClass::InvalidParameter,
        Some(5) => IoClass::PermissionDenied,
        Some(6) => IoClass::InvalidHandle,
        Some(234) => IoClass::MoreData,
        Some(109 | 232 | 233) => IoClass::BrokenPipe,
        Some(995) => IoClass::Aborted,
        Some(2 | 3) => IoClass::NotFound,
        _ => match error.kind() {
            ErrorKind::InvalidInput => IoClass::InvalidInput,
            ErrorKind::PermissionDenied => IoClass::PermissionDenied,
            ErrorKind::BrokenPipe => IoClass::BrokenPipe,
            ErrorKind::NotFound => IoClass::NotFound,
            _ => IoClass::Other,
        },
    }
}
thread_local! { static BACKEND: Cell<Option<IoFault>> = const { Cell::new(None) }; }
pub fn backend_begin() {
    BACKEND.with(|state| state.set(None));
}
pub fn backend_record(fault: IoFault) {
    BACKEND.with(|state| {
        if state.get().is_none() {
            state.set(Some(fault));
        }
    });
}
pub fn backend_take() -> Option<IoFault> {
    BACKEND.with(Cell::take)
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Stage(Stage),
    Source(Source),
    Git(Operation, Stop),
    Owned(Role, Stop),
    Io(Role, IoFault),
}
impl Failure {
    pub fn code(self) -> u32 {
        match self {
            Self::Stage(stage) => stage as u32,
            Self::Source(source) => 60 + source as u32,
            Self::Git(operation, stop) => 150 + (operation as u32) * 8 + stop as u32,
            Self::Owned(role, stop) => {
                (match role {
                    Role::Verify => 200,
                    Role::Supervise => 110,
                }) + stop as u32
            }
            Self::Io(role, fault) => 1000 + role as u32 * 512 + fault.site as u32 * 16 + fault.class as u32,
        }
    }
    pub fn from_code(code: i32) -> Option<Self> {
        if RESERVED.contains(&code) {
            return None;
        }
        if let Some(stage) = Stage::from_code(code) {
            return Some(Self::Stage(stage));
        }
        match code {
            1000..=2023 => {
                let offset = (code - 1000) as usize;
                let role = [Role::Verify, Role::Supervise][offset / 512];
                let slot = offset % 512;
                Some(Self::Io(role, IoFault { site: *IO_SITES.get(slot / 16)?, class: *IO_CLASSES.get(slot % 16)? }))
            }
            60..=67 => Some(Self::Source(SOURCES[(code - 60) as usize])),
            200..=207 => Some(Self::Owned(Role::Verify, STOPS[(code - 200) as usize])),
            110..=117 => Some(Self::Owned(Role::Supervise, STOPS[(code - 110) as usize])),
            150..=181 => Some(Self::Git(OPERATIONS[((code - 150) / 8) as usize], STOPS[((code - 150) % 8) as usize])),
            _ => None,
        }
    }
}

#[derive(Default)]
struct FirstFailure(Cell<Option<Failure>>);
impl FirstFailure {
    fn record(&self, failure: Failure) {
        if self.0.get().is_none() {
            self.0.set(Some(failure));
        }
    }
    fn code(&self) -> u32 {
        self.0.get().map_or(2, Failure::code)
    }
}
thread_local! { static FIRST: FirstFailure = FirstFailure::default(); }
pub fn record(failure: Failure) {
    FIRST.with(|first| first.record(failure));
}
pub fn code() -> u32 {
    FIRST.with(FirstFailure::code)
}

/// Only callers observing their pinned self-helper may call this. Git callers
/// always record Git(operation, stop), never decode subprocess exit codes.
pub fn child_failure(role: Role, stop: Stop, cleanup: bool, flags_clear: bool, root_code: Option<i32>) -> Failure {
    if stop == Stop::Completed
        && cleanup
        && flags_clear
        && let Some(failure) = root_code.and_then(Failure::from_code)
    {
        return failure;
    }
    Failure::Owned(role, stop)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failures() -> Vec<Failure> {
        let mut result: Vec<_> = Stage::ALL.iter().copied().map(Failure::Stage).collect();
        result.extend(SOURCES.into_iter().map(Failure::Source));
        for stop in STOPS {
            for role in [Role::Verify, Role::Supervise] {
                result.push(Failure::Owned(role, stop));
            }
            for operation in OPERATIONS {
                result.push(Failure::Git(operation, stop));
            }
        }
        for role in [Role::Verify, Role::Supervise] {
            for site in IO_SITES {
                for class in IO_CLASSES {
                    result.push(Failure::Io(role, IoFault { site, class }));
                }
            }
        }
        result
    }
    #[test]
    fn every_failure_has_a_unique_known_non_success_non_timeout_code() {
        let mut codes = std::collections::BTreeSet::new();
        for failure in failures() {
            let code = failure.code();
            assert!(!RESERVED.contains(&(code as i32)) && code < 2048);
            assert!(codes.insert(code));
            assert_eq!(Failure::from_code(code as i32), Some(failure));
        }
    }
    #[test]
    fn unknown_and_reserved_codes_stay_generic_child_rejections() {
        for code in -1..=2048 {
            if failures().iter().any(|failure| failure.code() as i32 == code) {
                continue;
            }
            assert_eq!(Failure::from_code(code), None);
            assert_eq!(child_failure(Role::Verify, Stop::Completed, true, true, Some(code)), Failure::Owned(Role::Verify, Stop::Completed));
        }
    }
    #[test]
    fn panic_and_common_cli_codes_are_never_decoded_as_diagnostics() {
        for code in RESERVED {
            assert_eq!(Failure::from_code(code), None, "reserved process code {code}");
            assert_eq!(
                child_failure(Role::Verify, Stop::Completed, true, true, Some(code)),
                Failure::Owned(Role::Verify, Stop::Completed),
                "a reserved process code must remain a generic child rejection"
            );
        }
    }
    #[test]
    fn only_a_completed_clean_helper_observation_can_propagate_a_known_failure() {
        let failure = Failure::Source(Source::Utf8);
        assert_eq!(child_failure(Role::Verify, Stop::Completed, true, true, Some(failure.code() as i32)), failure);
        for stop in STOPS.into_iter().filter(|stop| *stop != Stop::Completed) {
            assert_eq!(child_failure(Role::Verify, stop, true, true, Some(failure.code() as i32)), Failure::Owned(Role::Verify, stop));
        }
        for (cleanup, flags) in [(false, true), (true, false), (false, false)] {
            assert_eq!(
                child_failure(Role::Supervise, Stop::Completed, cleanup, flags, Some(failure.code() as i32)),
                Failure::Owned(Role::Supervise, Stop::Completed)
            );
        }
    }
    #[test]
    fn a_later_error_cannot_replace_the_first_failure() {
        let first = FirstFailure::default();
        assert_eq!(first.code(), 2);
        first.record(Failure::Stage(Stage::ReaderPin));
        first.record(Failure::Stage(Stage::MainDispatch));
        assert_eq!(first.code(), Failure::Stage(Stage::ReaderPin).code());
    }
    #[test]
    fn worker_code_must_travel_with_the_finished_event() {
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            record(Failure::Stage(Stage::ReaderPin));
            tx.send(code()).unwrap();
        });
        assert_eq!(rx.recv().unwrap(), Failure::Stage(Stage::ReaderPin).code());
        assert_eq!(code(), 2, "the receiver cannot read the worker's thread-local failure");
        worker.join().unwrap();
    }
    #[test]
    fn backend_cause_is_first_only_and_resets_between_invocations() {
        let first = IoFault { site: IoSite::OpenCandidate, class: IoClass::PermissionDenied };
        let later = IoFault { site: IoSite::RootWait, class: IoClass::InvalidHandle };
        backend_begin();
        backend_record(first);
        backend_record(later);
        assert_eq!(backend_take(), Some(first));
        assert_eq!(backend_take(), None);
        backend_record(later);
        backend_begin();
        assert_eq!(backend_take(), None);
    }
    #[test]
    fn os_errors_become_only_fixed_classes() {
        for (code, expected) in [
            (87, IoClass::InvalidParameter),
            (5, IoClass::PermissionDenied),
            (6, IoClass::InvalidHandle),
            (234, IoClass::MoreData),
            (109, IoClass::BrokenPipe),
            (995, IoClass::Aborted),
            (2, IoClass::NotFound),
            (3, IoClass::NotFound),
        ] {
            assert_eq!(io_class(&std::io::Error::from_raw_os_error(code)), expected);
        }
        assert_eq!(io_class(&std::io::Error::from(std::io::ErrorKind::InvalidInput)), IoClass::InvalidInput);
        assert_eq!(io_class(&std::io::Error::from(std::io::ErrorKind::Other)), IoClass::Other);
    }
    #[test]
    fn emit_private_table() {
        if std::env::var_os("KATALA_PRINT_PRIVATE_FAILURE_TABLE").is_some() {
            for failure in failures() {
                println!("{}\t{failure:?}", failure.code());
            }
        }
    }
}
