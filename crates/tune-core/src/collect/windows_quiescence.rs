//! Private clock policy only. OS evidence and admission remain with the owned guard.
use std::time::{Duration, Instant};

pub(crate) const QUIESCENCE: Duration = Duration::from_secs(1);
pub(crate) const CLEANUP: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Decision {
    Observe,
    Quiescent,
    Timeout,
    Lingering,
    CleanupExpired,
    Stopped,
}

pub(crate) struct Observation {
    pub root_exit_at: Option<Instant>,
    pub process_quiet: bool,
    pub failed: bool,
    pub cleanup_deadline: Option<Instant>,
}

pub(crate) struct Quiescence {
    execution_deadline: Instant,
    root_exit_at: Option<Instant>,
    quiet_deadline: Option<Instant>,
    cleanup_deadline: Option<Instant>,
    quiet_observed_at: Option<Instant>,
    stopped: Option<Decision>,
}

impl Quiescence {
    pub(crate) fn new(execution_deadline: Instant) -> Self {
        Self { execution_deadline, root_exit_at: None, quiet_deadline: None, cleanup_deadline: None, quiet_observed_at: None, stopped: None }
    }

    pub(crate) fn cleanup_deadline(&self) -> Option<Instant> {
        self.cleanup_deadline
    }

    pub(crate) fn observe(&mut self, observation: Observation, now: Instant) -> Decision {
        if let Some(earlier) = observation.cleanup_deadline {
            self.cleanup_deadline = Some(self.cleanup_deadline.map_or(earlier, |current| current.min(earlier)));
        }
        if self.root_exit_at.is_none()
            && let Some(root) = observation.root_exit_at
        {
            self.root_exit_at = Some(root);
            self.quiet_deadline = Some((root + QUIESCENCE).min(self.execution_deadline));
            let end = root + CLEANUP;
            self.cleanup_deadline = Some(self.cleanup_deadline.map_or(end, |earlier| earlier.min(end)));
        }
        if let (Some(root), Some(end), Some(quiet)) = (self.root_exit_at, self.cleanup_deadline, self.quiet_deadline) {
            // An earlier cleanup end cannot silently consume the nominal two-second
            // termination reserve. This can shorten, never restart, observation.
            let reserve = end.checked_sub(CLEANUP - QUIESCENCE).unwrap_or(root);
            self.quiet_deadline = Some(quiet.min(reserve));
        }
        if let Some(stopped) = self.stopped {
            return stopped;
        }
        let decision = if observation.failed || self.root_exit_at.is_some_and(|root| now < root) {
            Decision::Stopped
        } else if now >= self.execution_deadline {
            Decision::Timeout
        } else if self.cleanup_deadline.is_some_and(|end| now >= end) {
            Decision::CleanupExpired
        } else if self.root_exit_at.is_none() {
            Decision::Observe
        } else if let Some(proven) = self.quiet_observed_at {
            if observation.process_quiet && self.quiet_deadline.is_some_and(|end| proven < end) { Decision::Quiescent } else { Decision::Lingering }
        } else if self.quiet_deadline.is_some_and(|end| now >= end) {
            // A first quiet sample at/after the cutoff cannot prove earlier exit.
            Decision::Lingering
        } else if observation.process_quiet {
            self.quiet_observed_at = Some(now);
            Decision::Quiescent
        } else {
            Decision::Observe
        };
        if !matches!(decision, Decision::Observe | Decision::Quiescent) {
            self.stopped = Some(decision);
        }
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(root: Instant, quiet: bool) -> Observation {
        Observation { root_exit_at: Some(root), process_quiet: quiet, failed: false, cleanup_deadline: None }
    }

    #[test]
    fn root_first_member_can_finish_naturally_inside_fixed_window() {
        let root = Instant::now();
        let mut state = Quiescence::new(root + Duration::from_secs(10));
        assert_eq!(state.observe(observation(root, false), root), Decision::Observe);
        assert_eq!(state.observe(observation(root, true), root + Duration::from_millis(300)), Decision::Quiescent);
        assert_eq!(state.cleanup_deadline(), Some(root + CLEANUP));
    }

    #[test]
    fn first_anchor_does_not_move_when_observation_or_advance_is_delayed() {
        let root = Instant::now();
        let mut state = Quiescence::new(root + Duration::from_secs(10));
        assert_eq!(state.observe(observation(root, false), root + Duration::from_millis(700)), Decision::Observe);
        assert_eq!(state.root_exit_at, Some(root));
        assert_eq!(state.quiet_deadline, Some(root + QUIESCENCE));
        assert_eq!(state.observe(observation(root + Duration::from_millis(900), false), root + QUIESCENCE), Decision::Lingering);
        assert_eq!(state.cleanup_deadline(), Some(root + CLEANUP));
    }

    #[test]
    fn first_quiet_sample_at_or_after_cutoff_is_rejected_and_stays_rejected() {
        let root = Instant::now();
        for delay in [QUIESCENCE, QUIESCENCE + Duration::from_millis(1)] {
            let mut state = Quiescence::new(root + Duration::from_secs(10));
            assert_eq!(state.observe(observation(root, false), root), Decision::Observe);
            assert_eq!(state.observe(observation(root, true), root + delay), Decision::Lingering);
            assert_eq!(state.observe(observation(root, true), root + delay + Duration::from_millis(10)), Decision::Lingering);
        }
    }

    #[test]
    fn execution_deadline_precedes_quiescence_even_if_everything_looks_quiet() {
        let root = Instant::now();
        let end = root + Duration::from_millis(100);
        let mut state = Quiescence::new(end);
        assert_eq!(state.observe(observation(root, false), root), Decision::Observe);
        assert_eq!(state.observe(observation(root, true), end), Decision::Timeout);
        assert_eq!(state.cleanup_deadline(), Some(root + CLEANUP));
    }

    #[test]
    fn proven_process_quiescence_can_wait_for_pipes_only_inside_both_original_ends() {
        let root = Instant::now();
        let mut state = Quiescence::new(root + Duration::from_secs(10));
        assert_eq!(state.observe(observation(root, true), root), Decision::Quiescent);
        // This decision proves no EOF/writer/owner facts: the caller still needs them.
        assert_eq!(state.observe(observation(root, true), root + Duration::from_secs(2)), Decision::Quiescent);
        assert_eq!(state.observe(observation(root, true), root + CLEANUP), Decision::CleanupExpired);
        let mut short = Quiescence::new(root + Duration::from_millis(500));
        assert_eq!(short.observe(observation(root, true), root), Decision::Quiescent);
        assert_eq!(short.observe(observation(root, true), root + Duration::from_millis(500)), Decision::Timeout);
    }

    #[test]
    fn earlier_cleanup_end_is_never_replaced_and_keeps_nominal_termination_reserve() {
        let root = Instant::now();
        let earlier = root + Duration::from_millis(2500);
        let mut state = Quiescence::new(root + Duration::from_secs(10));
        let mut sample = observation(root, false);
        sample.cleanup_deadline = Some(earlier);
        assert_eq!(state.observe(sample, root), Decision::Observe);
        assert_eq!(state.cleanup_deadline(), Some(earlier));
        assert_eq!(state.quiet_deadline, Some(root + Duration::from_millis(500)));
        assert_eq!(state.observe(observation(root, false), root + Duration::from_millis(500)), Decision::Lingering);
        assert_eq!(state.cleanup_deadline(), Some(earlier));
    }

    #[test]
    fn a_later_earlier_cap_cannot_retroactively_validate_a_quiet_sample() {
        let root = Instant::now();
        let mut state = Quiescence::new(root + Duration::from_secs(10));
        assert_eq!(state.observe(observation(root, false), root), Decision::Observe);
        assert_eq!(state.observe(observation(root, true), root + Duration::from_millis(800)), Decision::Quiescent);
        let mut capped = observation(root, true);
        capped.cleanup_deadline = Some(root + Duration::from_millis(2500));
        assert_eq!(state.observe(capped, root + Duration::from_millis(900)), Decision::Lingering);
        assert_eq!(state.cleanup_deadline(), Some(root + Duration::from_millis(2500)));
    }

    #[test]
    fn failure_before_root_exit_is_sticky_and_cannot_restart_cleanup() {
        let start = Instant::now();
        let early = start + CLEANUP;
        let mut state = Quiescence::new(start + Duration::from_secs(10));
        let sample = Observation { root_exit_at: None, process_quiet: false, failed: true, cleanup_deadline: Some(early) };
        assert_eq!(state.observe(sample, start), Decision::Stopped);
        let root = start + Duration::from_millis(300);
        assert_eq!(state.observe(observation(root, true), root), Decision::Stopped);
        assert_eq!(state.cleanup_deadline(), Some(early));
    }

    #[test]
    fn an_immediate_io_or_limit_failure_is_not_rescued_by_quiet_or_late_samples() {
        let root = Instant::now();
        let end = root + Duration::from_millis(500);
        let mut state = Quiescence::new(end);
        assert_eq!(state.observe(observation(root, false), root), Decision::Observe);
        let mut failure = observation(root, true);
        failure.failed = true;
        assert_eq!(state.observe(failure, root + Duration::from_millis(10)), Decision::Stopped);
        assert_eq!(state.observe(observation(root, true), end), Decision::Stopped);
    }

    #[test]
    fn new_active_evidence_after_quiescence_cannot_be_ignored() {
        let root = Instant::now();
        let mut state = Quiescence::new(root + Duration::from_secs(10));
        assert_eq!(state.observe(observation(root, true), root), Decision::Quiescent);
        assert_eq!(state.observe(observation(root, false), root + Duration::from_millis(10)), Decision::Lingering);
    }
}
