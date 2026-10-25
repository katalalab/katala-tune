//! Private cfg(test) experiment, not an admission or Unicode acceptance test.
use super::{
    open_probe::{Facts, Outcome, Schedule, classify},
    private_diagnostic::{IoClass, IoSite, io_class},
    *,
};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug)]
enum Step {
    SetupJob,
    SetupRoot,
    SetupTarget,
    State,
    OpenIdentity,
    Resume,
    Signal,
    NaturalZero,
    Cleanup,
}
#[derive(Clone, Copy, Default)]
struct Record {
    facts: Facts,
    failure: Option<(Step, IoClass)>,
}

fn require<T, E>(result: Result<T, E>, step: Step) -> T {
    result.unwrap_or_else(|_| panic!("controlled_open_probe setup={step:?}"))
}
fn update(record: &Mutex<Record>, change: impl FnOnce(&mut Record)) {
    let mut state = require(record.lock(), Step::State);
    change(&mut state);
}
fn capture<T>(record: &Mutex<Record>, step: Step, result: io::Result<T>) -> io::Result<T> {
    result.inspect_err(|error| {
        update(record, |state| {
            if state.failure.is_none() {
                state.failure = Some((step, io_class(error)));
            }
        })
    })
}
fn signaled(handle: &OwnedHandle, deadline: Instant) -> io::Result<()> {
    let remaining = deadline.checked_duration_since(Instant::now()).ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
    let millis = remaining.as_millis().min(u128::from(u32::MAX)) as u32;
    // SAFETY: live retained handle of this fixture; original D bounds every wait.
    match unsafe { WaitForSingleObject(handle.as_raw_handle(), millis) } {
        WAIT_OBJECT_0 if Instant::now() < deadline => Ok(()),
        WAIT_OBJECT_0 | WAIT_TIMEOUT => Err(io::Error::from(io::ErrorKind::TimedOut)),
        _ => Err(os_error()),
    }
}
fn natural_zero(handle: &OwnedHandle, deadline: Instant) -> io::Result<()> {
    signaled(handle, deadline)?;
    let mut code = 0;
    // SAFETY: retained signaled fixture process, never an exit-code liveness test.
    check(unsafe { GetExitCodeProcess(handle.as_raw_handle(), &mut code) })?;
    if code != 0 {
        return Err(invalid());
    }
    Ok(())
}
fn resume(thread: &OwnedHandle, deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        return Err(io::Error::from(io::ErrorKind::TimedOut));
    }
    // SAFETY: only the exact primary thread created suspended by this fixture.
    let previous = unsafe { ResumeThread(thread.as_raw_handle()) };
    if previous == u32::MAX {
        return Err(os_error());
    }
    if previous != 1 {
        return Err(invalid());
    }
    Ok(())
}
fn opened_identity_matches(handle: &OwnedHandle, expected: &ProcessIdentity) -> io::Result<bool> {
    let (mut created, mut exit, mut kernel, mut user) = (Default::default(), Default::default(), Default::default(), Default::default());
    // SAFETY: readonly query on the just-opened, retained fixture candidate.
    check(unsafe { GetProcessTimes(handle.as_raw_handle(), &mut created, &mut exit, &mut kernel, &mut user) })?;
    let created = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    // Session is deliberately not queried here before the experimental exit.
    Ok(created.to_string() == expected.created_filetime)
}
fn target_error_site(site: IoSite) -> bool {
    matches!(
        site,
        IoSite::RetentionCap
            | IoSite::OpenCandidate
            | IoSite::MembershipQuery
            | IoSite::NonMember
            | IoSite::IdentityTimes
            | IoSite::IdentitySession
            | IoSite::IdentityInvariant
    )
}

fn run(schedule: Schedule) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut job = require(Job::new(), Step::SetupJob);
    let (root, _root_thread) = require(super::tests::observation_member(&job, deadline), Step::SetupRoot);
    let (target, target_thread) = require(super::tests::observation_member(&job, deadline), Step::SetupTarget);
    let target = Arc::new(target);
    let target_thread = Arc::new(target_thread);
    let mut receipt =
        ProcessReceipt { root: Some(root.identity.clone()), observed: vec![root.identity.clone()], stop: ProcessStop::Completed, ..Default::default() };
    job.processes.push(root);
    let record = Arc::new(Mutex::new(Record::default()));
    let (hook_record, hook_target, hook_thread) = (Arc::clone(&record), Arc::clone(&target), Arc::clone(&target_thread));
    job.candidate_open_probe = Some(CandidateOpenProbe {
        expected: target.identity.clone(),
        current_is_target: false,
        action: Some(Box::new(move |handle, expected| {
            update(&hook_record, |state| state.facts.hook_reached = true);
            let alive = !capture(&hook_record, Step::OpenIdentity, exited(handle))? && !capture(&hook_record, Step::OpenIdentity, exited(&hook_target.handle))?;
            let matches = capture(&hook_record, Step::OpenIdentity, opened_identity_matches(handle, expected))?;
            update(&hook_record, |state| {
                state.facts.child_live_at_open = alive;
                state.facts.opened_match = matches;
            });
            if !alive || !matches {
                return Err(invalid());
            }
            if schedule == Schedule::ExitBeforeMetadata {
                capture(&hook_record, Step::Resume, resume(&hook_thread, deadline))?;
                capture(&hook_record, Step::Signal, signaled(&hook_target.handle, deadline))?;
                update(&hook_record, |state| state.facts.child_signaled_at_hook = true);
                capture(&hook_record, Step::NaturalZero, natural_zero(&hook_target.handle, deadline))?;
                update(&hook_record, |state| state.facts.child_zero_at_hook = true);
            }
            update(&hook_record, |state| state.facts.hook_completed = true);
            Ok(())
        })),
    });
    let observed = job.observe(&mut receipt);
    let error = observed.as_ref().err().map(|error| (job.observation_site.get(), io_class(error)));
    let mut snapshot = *require(record.lock(), Step::State);
    snapshot.facts.root_live = exited(&job.processes[0].handle).is_ok_and(|done| !done) && Instant::now() < deadline;
    snapshot.facts.child_live_after_observe = exited(&target.handle).is_ok_and(|done| !done);
    snapshot.facts.observe_ok = observed.is_ok();
    snapshot.facts.error_targets_child =
        job.candidate_open_probe.as_ref().is_some_and(|probe| probe.current_is_target) && error.is_some_and(|(site, _)| target_error_site(site));
    let recorded = receipt.observed.iter().find(|identity| identity.pid == target.identity.pid);
    snapshot.facts.identity_recorded = recorded.is_some();
    snapshot.facts.identity_matches = recorded == Some(&target.identity) && job.processes.iter().any(|process| process.identity == target.identity);
    let outcome = classify(schedule, snapshot.facts);
    eprintln!(
        "controlled_open_probe schedule={schedule:?} outcome={outcome:?} hook={} hook_complete={} opened_match={} live_at_open={} signaled_at_hook={} zero_at_hook={} root_live={} identity_recorded={} identity_matches={} observer_error={error:?} hook_error={:?} within_deadline={}",
        snapshot.facts.hook_reached,
        snapshot.facts.hook_completed,
        snapshot.facts.opened_match,
        snapshot.facts.child_live_at_open,
        snapshot.facts.child_signaled_at_hook,
        snapshot.facts.child_zero_at_hook,
        snapshot.facts.root_live,
        snapshot.facts.identity_recorded,
        snapshot.facts.identity_matches,
        snapshot.failure,
        Instant::now() < deadline,
    );
    let accepted = matches!(outcome, Outcome::ControlVerified | Outcome::ExitObserved | Outcome::ExitApiRejected);
    let no_witness = !receipt.cleanup_confirmed();
    if schedule == Schedule::AliveThroughIdentity && outcome == Outcome::ControlVerified {
        require(resume(&target_thread, deadline), Step::Resume);
        require(natural_zero(&target.handle, deadline), Step::NaturalZero);
    }
    // Only owned fixture processes are terminated after measurement; no PID mutation.
    require(job.terminate(), Step::Cleanup);
    require(signaled(&job.processes[0].handle, deadline), Step::Cleanup);
    require(signaled(&target.handle, deadline), Step::Cleanup);
    let mut job_empty = false;
    while Instant::now() < deadline {
        let mut cleanup = ProcessReceipt::default();
        require(job.accounting(&mut cleanup), Step::Cleanup);
        if cleanup.job_empty {
            job_empty = true;
            break;
        }
        std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
    assert!(job_empty && Instant::now() < deadline, "controlled_open_probe cleanup expired");
    assert!(no_witness && snapshot.facts.root_live, "controlled_open_probe cannot authorize completion");
    assert!(accepted, "controlled_open_probe preconditions failed: {outcome:?}");
}

#[test]
fn control_alive_through_identity() {
    run(Schedule::AliveThroughIdentity);
}
#[test]
fn characterize_exit_before_metadata() {
    run(Schedule::ExitBeforeMetadata);
}
