//! Test-only classification: completing this probe never means admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Schedule {
    AliveThroughIdentity,
    ExitBeforeMetadata,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Outcome {
    PreconditionFailed,
    TargetNotReached,
    OtherObserverFailure,
    ControlRejected,
    ControlVerified,
    ExitObserved,
    ExitApiRejected,
}
#[derive(Clone, Copy, Default)]
pub(crate) struct Facts {
    pub root_live: bool,
    pub hook_reached: bool,
    pub hook_completed: bool,
    pub opened_match: bool,
    pub child_live_at_open: bool,
    pub child_signaled_at_hook: bool,
    pub child_zero_at_hook: bool,
    pub child_live_after_observe: bool,
    pub observe_ok: bool,
    pub error_targets_child: bool,
    pub identity_recorded: bool,
    pub identity_matches: bool,
}
pub(crate) fn classify(schedule: Schedule, facts: Facts) -> Outcome {
    if !facts.root_live {
        return Outcome::PreconditionFailed;
    }
    if !facts.hook_reached {
        return if !facts.observe_ok && !facts.error_targets_child { Outcome::OtherObserverFailure } else { Outcome::TargetNotReached };
    }
    if !facts.hook_completed || !facts.opened_match || !facts.child_live_at_open {
        return Outcome::PreconditionFailed;
    }
    if !facts.observe_ok && !facts.error_targets_child {
        return Outcome::OtherObserverFailure;
    }
    match schedule {
        Schedule::AliveThroughIdentity => {
            if !facts.child_live_after_observe || facts.child_signaled_at_hook {
                return Outcome::PreconditionFailed;
            }
            if !facts.observe_ok {
                return Outcome::ControlRejected;
            }
            if facts.identity_recorded && facts.identity_matches { Outcome::ControlVerified } else { Outcome::PreconditionFailed }
        }
        Schedule::ExitBeforeMetadata => {
            if !facts.child_signaled_at_hook || !facts.child_zero_at_hook {
                return Outcome::PreconditionFailed;
            }
            if facts.observe_ok {
                if facts.identity_recorded && facts.identity_matches { Outcome::ExitObserved } else { Outcome::PreconditionFailed }
            } else if facts.identity_recorded {
                Outcome::PreconditionFailed
            } else {
                Outcome::ExitApiRejected
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn control() -> Facts {
        Facts {
            root_live: true,
            hook_reached: true,
            hook_completed: true,
            opened_match: true,
            child_live_at_open: true,
            child_live_after_observe: true,
            observe_ok: true,
            identity_recorded: true,
            identity_matches: true,
            ..Default::default()
        }
    }
    fn exited() -> Facts {
        Facts { child_signaled_at_hook: true, child_zero_at_hook: true, child_live_after_observe: false, ..control() }
    }
    #[test]
    fn live_control_requires_real_identity_and_an_observed_success() {
        assert_eq!(classify(Schedule::AliveThroughIdentity, control()), Outcome::ControlVerified);
        assert_eq!(classify(Schedule::AliveThroughIdentity, Facts { observe_ok: false, error_targets_child: true, ..control() }), Outcome::ControlRejected);
    }
    #[test]
    fn exit_api_success_and_rejection_are_both_characterizations() {
        assert_eq!(classify(Schedule::ExitBeforeMetadata, exited()), Outcome::ExitObserved);
        assert_eq!(
            classify(
                Schedule::ExitBeforeMetadata,
                Facts { observe_ok: false, error_targets_child: true, identity_recorded: false, identity_matches: false, ..exited() }
            ),
            Outcome::ExitApiRejected
        );
    }
    #[test]
    fn other_member_failure_never_becomes_the_target_exit_result() {
        assert_eq!(classify(Schedule::ExitBeforeMetadata, Facts { observe_ok: false, error_targets_child: false, ..exited() }), Outcome::OtherObserverFailure);
        assert_eq!(classify(Schedule::ExitBeforeMetadata, Facts { hook_reached: false, ..exited() }), Outcome::TargetNotReached);
    }
    #[test]
    fn incomplete_ordering_or_dead_root_cannot_characterize_an_exit() {
        for facts in [
            Facts { root_live: false, ..exited() },
            Facts { hook_completed: false, ..exited() },
            Facts { opened_match: false, ..exited() },
            Facts { child_live_at_open: false, ..exited() },
            Facts { child_signaled_at_hook: false, ..exited() },
            Facts { child_zero_at_hook: false, ..exited() },
        ] {
            assert_eq!(classify(Schedule::ExitBeforeMetadata, facts), Outcome::PreconditionFailed);
        }
    }
    #[test]
    fn unknown_or_unrecorded_identity_is_not_a_successful_observation() {
        assert_eq!(classify(Schedule::ExitBeforeMetadata, Facts { identity_recorded: false, ..exited() }), Outcome::PreconditionFailed);
        assert_eq!(classify(Schedule::AliveThroughIdentity, Facts { identity_matches: false, ..control() }), Outcome::PreconditionFailed);
    }
}
